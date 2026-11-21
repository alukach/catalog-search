//! A catalog entry (one line of `catalog.jsonl`) onto the rows the index stores.

use serde_json::{json, Map, Value};

use crate::{bbox, ts, BASE};

#[derive(Debug, Clone)]
pub struct Record {
    pub id: String,
    pub account_id: String,
    pub product_id: String,
    pub title: String,
    pub description: String,
    pub license: String,
    pub data_mode: Option<String>,
    pub featured: bool,
    pub total_bytes: Option<i64>,
    pub object_count: Option<i64>,
    pub created: Option<String>,
    pub updated: Option<String>,
    pub dt_start: Option<String>,
    pub dt_end: Option<String>,
    pub bboxes: Vec<bbox::BBox>,
    pub keywords: Vec<String>,
    /// STAC Collection JSON, `{{BASE}}` in place of the API origin.
    pub doc: String,
}

/// Why an entry isn't searchable.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Skip {
    /// `seed` and `unregistered` entries are only inventoried: counts, no STAC body.
    NoBody,
    NotPublic,
    Disabled,
    NoId,
}

impl std::fmt::Display for Skip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoBody => "seed or unregistered, no STAC body",
            Self::NotPublic => "not public",
            Self::Disabled => "disabled",
            Self::NoId => "no account_id or product_id",
        })
    }
}

/// D1 rejects a statement over 100 KB, and the document is written into one. Documents carry
/// a few outliers (a 790 KB `summaries`), so the heavy optional parts go before the record does.
const DOC_BUDGET: usize = 40_000;

pub fn from_entry(v: &Value) -> Result<Record, Skip> {
    let s = |k: &str| v.get(k).and_then(Value::as_str);
    if matches!(s("_state"), Some("seed" | "unregistered")) {
        return Err(Skip::NoBody);
    }
    if s("visibility") != Some("public") {
        return Err(Skip::NotPublic);
    }
    if v.get("disabled").and_then(Value::as_bool) == Some(true) {
        return Err(Skip::Disabled);
    }
    let account_id = s("account_id").ok_or(Skip::NoId)?.to_owned();
    let product_id = s("product_id").ok_or(Skip::NoId)?.to_owned();
    let id = s("id").map(str::to_owned).unwrap_or_else(|| format!("{account_id}/{product_id}"));
    // Trimmed: a title with a leading space sorts ahead of every other title.
    let title = s("title").map(str::trim).filter(|t| !t.is_empty()).unwrap_or(&id).to_owned();
    let description = s("description").unwrap_or_default().trim().to_owned();
    // STAC requires a license. "other" is its value for "not stated"; the catalog leaves
    // unresolved licenses out, and inventing one would be worse.
    let license = s("license").unwrap_or("other").to_owned();

    let time = |k: &str| s(k).and_then(|t| ts::norm(t, ts::Edge::Start));
    let (created, updated) = (time("created_at"), time("updated_at"));
    let int = |k: &str| v.get(k).and_then(Value::as_i64);

    let interval = v.pointer("/extent/temporal/interval/0").and_then(Value::as_array);
    let edge =
        |i: usize, e: ts::Edge| interval.and_then(|a| a.get(i)).and_then(Value::as_str).and_then(|t| ts::norm(t, e));

    let mut keywords: Vec<String> = Vec::new();
    for k in v.get("keywords").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        if !keywords.iter().any(|x| x == k) {
            keywords.push(k.to_owned());
        }
    }

    let doc = doc(v, &id, &title, &description, &license, created.as_deref(), updated.as_deref());
    Ok(Record {
        data_mode: s("data_mode").map(str::to_owned),
        featured: v.get("featured").is_some_and(|f| f.as_i64() == Some(1) || f.as_bool() == Some(true)),
        total_bytes: int("total_bytes"),
        object_count: int("object_count"),
        created,
        updated,
        dt_start: edge(0, ts::Edge::Start),
        dt_end: edge(1, ts::Edge::End),
        bboxes: bboxes(v),
        keywords,
        doc,
        id,
        account_id,
        product_id,
        title,
        description,
        license,
    })
}

/// Every known box in `extent.spatial.bbox`. `null` components mean "not established", so a
/// box with one is no extent at all.
fn bboxes(v: &Value) -> Vec<bbox::BBox> {
    let mut out = Vec::new();
    for b in v.pointer("/extent/spatial/bbox").and_then(Value::as_array).into_iter().flatten() {
        let Some(a) = b.as_array() else { continue };
        let n: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
        if n.len() != a.len() {
            continue;
        }
        let Some((w, s, e, nn)) = bbox::from_numbers(&n) else { continue };
        // Global datasets come with a hair of overshoot (-90.0000051, 180.05, even a 97.1), and
        // dropping the box would hide exactly the datasets a world-sized query is after.
        // `split` clamps longitude; latitude is clamped here. A query's own bbox stays strict.
        // A box wholly past a pole is garbage, not overshoot; clamping would invent a line there.
        if s > 90.0 || nn < -90.0 {
            continue;
        }
        let (s, nn) = (s.clamp(-90.0, 90.0), nn.clamp(-90.0, 90.0));
        if bbox::check(w, s, e, nn).is_ok() {
            out.extend(bbox::split(w, s, e, nn));
        }
    }
    out
}

fn doc(
    v: &Value,
    id: &str,
    title: &str,
    description: &str,
    license: &str,
    created: Option<&str>,
    updated: Option<&str>,
) -> String {
    let mut m = Map::new();
    m.insert("type".into(), json!("Collection"));
    m.insert("stac_version".into(), v.get("stac_version").cloned().unwrap_or(json!("1.1.0")));
    if let Some(x) = v.get("stac_extensions").filter(|x| x.is_array()) {
        m.insert("stac_extensions".into(), x.clone());
    }
    m.insert("id".into(), json!(id));
    m.insert("title".into(), json!(title));
    m.insert("description".into(), json!(description));
    m.insert("license".into(), json!(license));
    if let Some(x) = extent(v) {
        m.insert("extent".into(), x);
    }
    for k in ["keywords", "providers"] {
        if let Some(x) = v.get(k).filter(|x| !x.is_null()) {
            m.insert(k.into(), x.clone());
        }
    }
    if let Some(c) = created {
        m.insert("created".into(), json!(c));
    }
    if let Some(u) = updated {
        m.insert("updated".into(), json!(u));
    }
    for k in ["account_id", "product_id", "data_mode", "total_bytes", "object_count"] {
        if let Some(x) = v.get(k).filter(|x| !x.is_null()) {
            m.insert(k.into(), x.clone());
        }
    }
    m.insert("links".into(), links(v, id));

    // Heavy and optional last, so they can be dropped without touching the rest.
    // A summary must be an array, range or schema; a bare scalar (`item_count`) is invalid.
    // The count moves to the top level instead of being lost.
    if let Some(n) = v.pointer("/summaries/item_count").filter(|n| n.is_number()) {
        m.insert("item_count".into(), n.clone());
    }
    if let Some(sm) = v.get("summaries").and_then(Value::as_object) {
        let ok: Map<String, Value> =
            sm.iter().filter(|(_, x)| x.is_array() || x.is_object()).map(|(k, x)| (k.clone(), x.clone())).collect();
        if !ok.is_empty() {
            m.insert("summaries".into(), Value::Object(ok));
        }
    }
    if let Some(x) = v.get("assets").filter(|x| !x.is_null()) {
        m.insert("assets".into(), x.clone());
    }
    let mut out = Value::Object(m.clone()).to_string();
    for k in ["summaries", "assets"] {
        if out.len() <= DOC_BUDGET {
            break;
        }
        m.remove(k);
        out = Value::Object(m.clone()).to_string();
    }
    out
}

/// STAC wants numeric bboxes and UTC timestamps; the source has `null` boxes and zoneless times.
/// With no usable box the whole world stands in, as the schema requires one.
fn extent(v: &Value) -> Option<Value> {
    let e = v.get("extent").filter(|e| e.is_object())?;
    let mut boxes: Vec<Value> = e
        .pointer("/spatial/bbox")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|b| b.as_array().is_some_and(|a| !a.is_empty() && a.iter().all(Value::is_number)))
        .cloned()
        .collect();
    if boxes.is_empty() {
        boxes.push(json!([-180, -90, 180, 90]));
    }
    let intervals: Vec<Value> = e
        .pointer("/temporal/interval")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .map(|i| {
            let t = |n: usize, edge| i.get(n).and_then(Value::as_str).and_then(|t| ts::norm(t, edge));
            json!([t(0, ts::Edge::Start), t(1, ts::Edge::End)])
        })
        .collect();
    let intervals = if intervals.is_empty() { vec![json!([null, null])] } else { intervals };
    Some(json!({"spatial": {"bbox": boxes}, "temporal": {"interval": intervals}}))
}

fn links(v: &Value, id: &str) -> Value {
    let mut out = vec![
        json!({"rel": "self", "type": "application/json", "title": "Collection", "href": format!("{BASE}/collections/{id}")}),
        json!({"rel": "root", "type": "application/json", "title": "Catalog root", "href": format!("{BASE}/")}),
        json!({"rel": "parent", "type": "application/json", "title": "Collections", "href": format!("{BASE}/collections")}),
    ];
    for l in v.get("links").and_then(Value::as_array).into_iter().flatten() {
        match l.get("rel").and_then(Value::as_str) {
            // The catalog's own self link names where its copy lives, which is no longer us.
            Some("self") => {
                let mut l = l.clone();
                l["rel"] = json!("canonical");
                out.push(l);
            }
            Some("root" | "parent" | "collection" | "child" | "item" | "items") | None => {}
            Some(_) => out.push(l.clone()),
        }
    }
    Value::Array(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(extra: Value) -> Value {
        let mut e = json!({
            "account_id": "a", "product_id": "p", "visibility": "public", "_state": "drafted",
            "title": "T", "description": "D", "keywords": ["x", "y", "x"],
            "created_at": "2023-10-03T20:46:06.861356+00:00",
            "extent": {"spatial": {"bbox": [[170.0, -10.0, -170.0, 10.0], [null, null, null, null]]},
                       "temporal": {"interval": [["2020-01-01T00:00:00Z", null]]}},
            "links": [{"rel": "self", "href": "https://x/collection.json"}, {"rel": "via", "href": "https://x"}],
        });
        e.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        e
    }

    #[test]
    fn maps_an_entry() {
        let r = from_entry(&entry(json!({}))).unwrap();
        assert_eq!(r.id, "a/p");
        assert_eq!(r.license, "other");
        assert_eq!(r.keywords, ["x", "y"]);
        assert_eq!(r.created.as_deref(), Some("2023-10-03T20:46:06Z"));
        assert_eq!(r.dt_start.as_deref(), Some("2020-01-01T00:00:00Z"));
        assert_eq!(r.dt_end, None);
        // antimeridian box split in two; the all-null box dropped
        assert_eq!(r.bboxes.len(), 2);
        let doc: Value = serde_json::from_str(&r.doc).unwrap();
        let rels: Vec<_> = doc["links"].as_array().unwrap().iter().map(|l| l["rel"].as_str().unwrap()).collect();
        assert_eq!(rels, ["self", "root", "parent", "canonical", "via"]);
    }

    #[test]
    fn extent_is_schema_valid() {
        let r = from_entry(&entry(json!({"extent": {"spatial": {"bbox": [[null, null, null, null]]},
            "temporal": {"interval": [["2018-12-23T19:12:50", null]]}}})))
        .unwrap();
        let d: Value = serde_json::from_str(&r.doc).unwrap();
        assert_eq!(d["extent"]["spatial"]["bbox"], json!([[-180, -90, 180, 90]]));
        assert_eq!(d["extent"]["temporal"]["interval"], json!([["2018-12-23T19:12:50Z", null]]));
    }

    #[test]
    fn scalar_summaries_are_dropped() {
        let r = from_entry(&entry(json!({"summaries": {"item_count": 5, "x": ["a"]}}))).unwrap();
        let d: Value = serde_json::from_str(&r.doc).unwrap();
        assert_eq!((d["item_count"].as_i64(), &d["summaries"]), (Some(5), &json!({"x": ["a"]})));
    }

    #[test]
    fn titles_are_trimmed_and_fall_back_to_the_id() {
        let r = from_entry(&entry(json!({"title": "  Padded \n", "description": " d "}))).unwrap();
        assert_eq!((r.title.as_str(), r.description.as_str()), ("Padded", "d"));
        assert!(r.doc.contains(r#""title":"Padded""#));
        assert_eq!(from_entry(&entry(json!({"title": "   "}))).unwrap().title, "a/p");
    }

    #[test]
    fn global_extents_with_a_hair_of_overshoot_are_kept() {
        let r = from_entry(&entry(json!({"extent": {"spatial": {"bbox": [
            [-180.0000051, -90.0000051, 180.0000051, 89.9999975],
            [-173.5, -14.5, 173.0, 97.1],
            [10.0, 95.0, 20.0, 99.0]
        ]}}})))
        .unwrap();
        // overshoot is clamped and kept; a box wholly past the pole is dropped
        assert_eq!(r.bboxes, [[-180.0, -90.0, 180.0, 89.9999975], [-173.5, -14.5, 173.0, 90.0]]);
    }

    #[test]
    fn skips_what_isnt_searchable() {
        assert_eq!(from_entry(&entry(json!({"_state": "seed"}))).unwrap_err(), Skip::NoBody);
        assert_eq!(from_entry(&entry(json!({"visibility": "unlisted"}))).unwrap_err(), Skip::NotPublic);
        assert_eq!(from_entry(&entry(json!({"disabled": true}))).unwrap_err(), Skip::Disabled);
        assert_eq!(from_entry(&entry(json!({"account_id": null}))).unwrap_err(), Skip::NoId);
    }

    #[test]
    fn oversized_parts_are_dropped_not_the_record() {
        let big = "x".repeat(60_000);
        let r = from_entry(&entry(json!({"summaries": {"blob": big}}))).unwrap();
        assert!(r.doc.len() < DOC_BUDGET);
        assert!(!r.doc.contains("summaries"));
    }
}

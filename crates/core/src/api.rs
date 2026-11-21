//! The STAC API's own documents: landing page, conformance, queryables, and the paged
//! `/collections` response.

use serde_json::{json, Value};

use crate::BASE;

/// Only what is implemented and tested. Operators beyond these (S_INTERSECTS, the T_ and A_
/// functions) work but don't claim a class they don't fully cover; the README lists them.
pub const CONFORMS_TO: &[&str] = &[
    "https://api.stacspec.org/v1.0.0/core",
    "https://api.stacspec.org/v1.0.0/collections",
    "https://api.stacspec.org/v1.0.0/collection-search",
    "http://www.opengis.net/spec/ogcapi-common-2/1.0/conf/simple-query",
    "https://api.stacspec.org/v1.0.0-rc.1/collection-search#free-text",
    "https://api.stacspec.org/v1.0.0-rc.4/collection-search#filter",
    "https://api.stacspec.org/v1.1.0/collection-search#sort",
    "http://www.opengis.net/spec/ogcapi-features-3/1.0/conf/filter",
    "http://www.opengis.net/spec/cql2/1.0/conf/cql2-text",
    "http://www.opengis.net/spec/cql2/1.0/conf/cql2-json",
    "http://www.opengis.net/spec/cql2/1.0/conf/basic-cql2",
    "http://www.opengis.net/spec/cql2/1.0/conf/advanced-comparison-operators",
    "http://www.opengis.net/spec/cql2/1.0/conf/case-insensitive-comparison",
    "http://www.opengis.net/spec/cql2/1.0/conf/property-property",
];

const QUERYABLES_REL: &str = "http://www.opengis.net/def/rel/ogc/1.0/queryables";

fn link(rel: &str, href: String) -> Value {
    json!({"rel": rel, "type": "application/json", "href": href})
}

pub fn landing(base: &str) -> String {
    json!({
        "type": "Catalog",
        "stac_version": "1.0.0",
        "id": "source-coop-catalog-search",
        "title": "Source Cooperative catalog search",
        "description": "Search the Source Cooperative metadata catalog as STAC Collections.",
        "conformsTo": CONFORMS_TO,
        "links": [
            link("self", format!("{base}/")),
            link("root", format!("{base}/")),
            link("conformance", format!("{base}/conformance")),
            link("data", format!("{base}/collections")),
            link(QUERYABLES_REL, format!("{base}/collection-queryables")),
        ],
    })
    .to_string()
}

pub fn conformance() -> String {
    json!({"conformsTo": CONFORMS_TO}).to_string()
}

pub fn queryables(base: &str) -> String {
    let text = json!({"type": "string"});
    let int = json!({"type": "integer"});
    json!({
        "$schema": "https://json-schema.org/draft/2019-09/schema",
        "$id": format!("{base}/collection-queryables"),
        "type": "object",
        "title": "Queryables for Source Cooperative collections",
        "properties": {
            "id": text, "title": text, "description": text, "license": text,
            "account_id": text, "product_id": text, "data_mode": text,
            "featured": {"type": "boolean"},
            "total_bytes": int, "object_count": int,
            "created": {"type": "string", "format": "date-time"},
            "updated": {"type": "string", "format": "date-time"},
            "keywords": {"type": "array", "items": {"type": "string"}},
            "geometry": {"$ref": "https://geojson.org/schema/Geometry.json"},
            "datetime": {"$ref": "https://schema.stacspec.org/v1.0.0/item-spec/json-schema/datetime.json#/properties/datetime"},
        },
        "additionalProperties": false,
    })
    .to_string()
}

/// Percent-encodes everything but RFC 3986 unreserved characters.
fn enc(s: &str) -> String {
    s.bytes().fold(String::new(), |mut o, b| {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
        o
    })
}

/// `/collections` with the request's own parameters, and `token` set to `token` if given.
fn href(base: &str, query: &[(String, String)], token: Option<u32>) -> String {
    let mut pairs: Vec<String> =
        query.iter().filter(|(k, _)| k != "token").map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect();
    if let Some(t) = token {
        pairs.push(format!("token={t}"));
    }
    if pairs.is_empty() {
        format!("{base}/collections")
    } else {
        format!("{base}/collections?{}", pairs.join("&"))
    }
}

/// The page, with documents spliced in as stored. They are already JSON, so parsing them just
/// to print them again would be the most expensive part of the request.
pub fn collections_body(
    docs: &[String],
    base: &str,
    query: &[(String, String)],
    offset: u32,
    limit: u32,
    matched: u64,
) -> String {
    let mut links = vec![
        link("root", format!("{base}/")),
        link("self", href(base, query, (offset > 0).then_some(offset))),
        link(QUERYABLES_REL, format!("{base}/collection-queryables")),
        link("first", href(base, query, None)),
    ];
    if offset > 0 {
        links.push(link("prev", href(base, query, Some(offset.saturating_sub(limit)))));
    }
    if (offset as u64) + (limit as u64) < matched {
        links.push(link("next", href(base, query, Some(offset + limit))));
    }
    format!(
        r#"{{"collections":[{}],"links":{},"numberMatched":{matched},"numberReturned":{}}}"#,
        docs.join(",").replace(BASE, base),
        Value::Array(links),
        docs.len(),
    )
}

pub fn collection_body(doc: &str, base: &str) -> String {
    doc.replace(BASE, base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cql;

    #[test]
    fn queryables_match_what_the_translator_accepts() {
        let v: Value = serde_json::from_str(&queryables("http://x")).unwrap();
        let mut documented: Vec<_> = v["properties"].as_object().unwrap().keys().cloned().collect();
        let mut accepted: Vec<_> = cql::names().map(str::to_owned).collect();
        documented.sort();
        accepted.sort();
        assert_eq!(documented, accepted);
    }

    #[test]
    fn paging_links() {
        let q = vec![("q".to_owned(), "a b".to_owned()), ("token".to_owned(), "10".to_owned())];
        let body = collections_body(&[r#"{"id":"x","links":[{"href":"{{BASE}}/c"}]}"#.into()], "http://h", &q, 10, 10, 25);
        let v: Value = serde_json::from_str(&body).unwrap();
        let href = |rel: &str| v["links"].as_array().unwrap().iter().find(|l| l["rel"] == rel).map(|l| l["href"].as_str().unwrap().to_owned());
        assert_eq!(href("next").unwrap(), "http://h/collections?q=a%20b&token=20");
        assert_eq!(href("prev").unwrap(), "http://h/collections?q=a%20b&token=0");
        assert_eq!(href("first").unwrap(), "http://h/collections?q=a%20b");
        assert_eq!(v["collections"][0]["links"][0]["href"], "http://h/c");
        assert_eq!((v["numberMatched"].as_u64(), v["numberReturned"].as_u64()), (Some(25), Some(1)));
    }
}

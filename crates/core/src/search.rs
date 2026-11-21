//! Collection search parameters (`bbox`, `datetime`, `q`, `filter`, `sortby`, `limit`, `token`)
//! to one SQL query and its bindings.

use crate::{
    bbox,
    cql::{self, Bind, Rel, DATETIME},
    ts::{bound, Edge},
    Problem, Result,
};

pub const DEFAULT_LIMIT: u32 = 10;
pub const MAX_LIMIT: u32 = 100;
/// D1 allows 100 bound parameters per query; two go to LIMIT and OFFSET.
const MAX_BINDS: usize = 98;

#[derive(Default, Debug, Clone)]
pub struct Params {
    pub bbox: Option<String>,
    pub datetime: Option<String>,
    pub q: Option<String>,
    pub filter: Option<String>,
    pub filter_lang: Option<String>,
    pub sortby: Option<String>,
    pub limit: Option<String>,
    pub token: Option<String>,
}

impl Params {
    /// Unknown parameters are ignored, as STAC asks.
    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut p = Self::default();
        for (k, v) in pairs {
            let v = Some(v.to_owned());
            match k {
                "bbox" => p.bbox = v,
                "datetime" => p.datetime = v,
                "q" => p.q = v,
                "filter" => p.filter = v,
                "filter-lang" => p.filter_lang = v,
                "sortby" => p.sortby = v,
                "limit" => p.limit = v,
                "token" => p.token = v,
                _ => {}
            }
        }
        p
    }
}

#[derive(Debug)]
pub struct Query {
    pub rows_sql: String,
    pub rows_binds: Vec<Bind>,
    pub count_sql: String,
    pub count_binds: Vec<Bind>,
    pub limit: u32,
    pub offset: u32,
}

pub fn build(p: &Params) -> Result<Query> {
    let mut b = Vec::new();
    let mut wh: Vec<String> = Vec::new();
    let mut join = "";

    if let Some(m) = p.q.as_deref().and_then(fts_match) {
        join = " JOIN collections_fts ON collections_fts.rowid = c.rowid";
        wh.push("collections_fts MATCH ?".into());
        b.push(Bind::Text(m));
    }
    if let Some(raw) = &p.bbox {
        wh.push(cql::bbox_clause(&bbox::parse_param(raw).map_err(Problem::bad)?, &mut b));
    }
    if let Some(raw) = &p.datetime {
        let (lo, hi) = datetime(raw)?;
        wh.push(cql::temporal_clause(&DATETIME, Rel::Intersects, lo, hi, &mut b));
    }
    if let Some(f) = &p.filter {
        wh.push(cql::to_sql(&parse_filter(f, p.filter_lang.as_deref())?, &mut b)?);
    }
    if b.len() > MAX_BINDS {
        return Err(Problem::bad(format!("the query needs {} values; the limit is {MAX_BINDS}", b.len())));
    }

    let limit = match &p.limit {
        None => DEFAULT_LIMIT,
        Some(l) => match l.parse::<u32>() {
            Ok(n) if n >= 1 => n.min(MAX_LIMIT),
            _ => return Err(Problem::bad("limit must be a positive integer")),
        },
    };
    let offset = match &p.token {
        None => 0,
        Some(t) => t.parse::<u32>().map_err(|_| Problem::bad("token is not valid"))?,
    };

    let where_sql = if wh.is_empty() { "1".to_owned() } else { wh.join(" AND ") };
    let from = format!("FROM collections c{join} WHERE {where_sql}");
    let mut rows_binds = b.clone();
    rows_binds.extend([Bind::Int(limit as i64), Bind::Int(offset as i64)]);
    Ok(Query {
        rows_sql: format!("SELECT c.doc AS doc {from} ORDER BY {} LIMIT ? OFFSET ?", order(p.sortby.as_deref(), !join.is_empty())?),
        rows_binds,
        count_sql: format!("SELECT COUNT(*) AS n {from}"),
        count_binds: b,
        limit,
        offset,
    })
}

/// `q`: comma-separated terms, any of which may match (the STAC basic free-text search). A
/// term is matched as a phrase, so `q=EO,Earth Observation` is "EO" or "Earth Observation".
fn fts_match(q: &str) -> Option<String> {
    let terms: Vec<String> = q
        .split(',')
        .map(str::trim)
        .filter(|t| t.chars().any(char::is_alphanumeric))
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

fn datetime(raw: &str) -> Result<(Option<String>, Option<String>)> {
    let (lo, hi) = match raw.split_once('/') {
        Some((a, b)) => (bound(a, Edge::Start), bound(b, Edge::End)),
        // An instant; a bare date is the whole day.
        None => (bound(raw, Edge::Start), bound(raw, Edge::End)),
    };
    let (lo, hi) = (lo.map_err(Problem::bad)?, hi.map_err(Problem::bad)?);
    match (&lo, &hi) {
        (None, None) => Err(Problem::bad("datetime must be bounded at one end at least")),
        (Some(l), Some(h)) if l > h => Err(Problem::bad("datetime starts after it ends")),
        _ => Ok((lo, hi)),
    }
}

fn parse_filter(f: &str, lang: Option<&str>) -> Result<cql2::Expr> {
    match lang.unwrap_or("cql2-text") {
        "cql2-text" => cql2::parse_text(f).map_err(|e| Problem::bad(format!("invalid cql2-text filter: {e}"))),
        "cql2-json" => cql2::parse_json(f).map_err(|e| Problem::bad(format!("invalid cql2-json filter: {e}"))),
        other => Err(Problem::bad(format!("filter-lang `{other}` is not supported; use cql2-text or cql2-json"))),
    }
}

const SORTABLE: &[(&str, &str)] = &[
    ("id", "c.id"),
    ("title", "c.title COLLATE NOCASE"),
    ("license", "c.license"),
    ("account_id", "c.account_id"),
    ("product_id", "c.product_id"),
    ("created", "c.created"),
    ("updated", "c.updated"),
    ("total_bytes", "c.total_bytes"),
    ("object_count", "c.object_count"),
];

/// `sortby=-updated,title`. An unescaped `+` reaches us as a space, so a leading space is `+`.
/// Without one, relevance when searching text, else id; rowid last so pages never reshuffle.
fn order(sortby: Option<&str>, ranked: bool) -> Result<String> {
    let mut parts = Vec::new();
    for tok in sortby.into_iter().flat_map(|s| s.split(',')) {
        let tok = tok.trim();
        let (dir, field) = match tok.strip_prefix('-') {
            Some(f) => ("DESC", f),
            None => ("ASC", tok.strip_prefix('+').unwrap_or(tok)),
        };
        let col = SORTABLE.iter().find(|s| s.0 == field).ok_or_else(|| {
            Problem::bad(format!(
                "can't sort by `{field}`; sortable fields are {}",
                SORTABLE.iter().map(|s| s.0).collect::<Vec<_>>().join(", ")
            ))
        })?;
        parts.push(format!("{} {dir}", col.1));
    }
    if parts.is_empty() {
        parts.push(if ranked { "bm25(collections_fts, 10.0, 1.0, 5.0)".into() } else { "c.id".into() });
    }
    parts.push("c.rowid".into());
    Ok(parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_text_terms_are_phrases_or_ed() {
        assert_eq!(fts_match("EO,Earth Observation").unwrap(), r#""EO" OR "Earth Observation""#);
        assert_eq!(fts_match(r#"say "hi""#).unwrap(), r#""say ""hi""""#);
        assert_eq!(fts_match(" , --- ,"), None);
    }

    #[test]
    fn datetime_forms() {
        let d = |s| datetime(s).unwrap();
        assert_eq!(d("2020-01-01/.."), (Some("2020-01-01T00:00:00Z".into()), None));
        assert_eq!(d("../2020-01-01"), (None, Some("2020-01-01T23:59:59Z".into())));
        assert_eq!(d("2020-01-01").0, Some("2020-01-01T00:00:00Z".into()));
        assert!(datetime("../..").is_err());
        assert!(datetime("2021-01-01/2020-01-01").is_err());
    }

    #[test]
    fn limit_and_token_are_validated() {
        let q = |l: &str| build(&Params { limit: Some(l.into()), ..Default::default() });
        assert_eq!(q("500").unwrap().limit, MAX_LIMIT);
        assert!(q("0").is_err());
        assert!(q("x").is_err());
        assert!(build(&Params { token: Some("-1".into()), ..Default::default() }).is_err());
    }
}

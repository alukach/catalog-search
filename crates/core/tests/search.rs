//! The index and search against a real SQLite (FTS5 and R*Tree included), loaded from
//! `fixtures/sample.jsonl`: a slice of the actual catalog. Where there is an independent way
//! to know the answer — brute force over the records, or cql2's own evaluator — the SQL is
//! held to it.

use catalog_core::{
    cql::Bind,
    load::load_sql,
    record::{from_entry, Record},
    search::{build, Params},
    Problem,
};
use rusqlite::{params_from_iter, types::Value as V, Connection};
use serde_json::Value;

struct Fx {
    conn: Connection,
    records: Vec<Record>,
}

fn fx() -> Fx {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/sample.jsonl");
    let text = std::fs::read_to_string(path).unwrap();
    let mut records: Vec<Record> =
        text.lines().filter_map(|l| from_entry(&serde_json::from_str(l).unwrap()).ok()).collect();
    records.sort_by(|a, b| a.id.cmp(&b.id));
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(catalog_core::MIGRATION).unwrap();
    conn.execute_batch(&load_sql(&records).unwrap()).unwrap();
    Fx { conn, records }
}

fn sql_value(b: &Bind) -> V {
    match b {
        Bind::Text(s) => V::Text(s.clone()),
        Bind::Int(i) => V::Integer(*i),
        Bind::Real(r) => V::Real(*r),
    }
}

fn params(f: impl FnOnce(&mut Params)) -> Params {
    let mut p = Params { limit: Some("100".into()), ..Default::default() };
    f(&mut p);
    p
}

impl Fx {
    /// Ids on the page, in order, and the total matched.
    fn run(&self, p: &Params) -> Result<(Vec<String>, i64), Problem> {
        let q = build(p)?;
        let mut st = self.conn.prepare(&q.rows_sql).unwrap();
        let ids = st
            .query_map(params_from_iter(q.rows_binds.iter().map(sql_value)), |r| r.get::<_, String>(0))
            .unwrap()
            .map(|d| serde_json::from_str::<Value>(&d.unwrap()).unwrap()["id"].as_str().unwrap().to_owned())
            .collect();
        let n = self
            .conn
            .query_row(&q.count_sql, params_from_iter(q.count_binds.iter().map(sql_value)), |r| r.get(0))
            .unwrap();
        Ok((ids, n))
    }

    fn ids(&self, p: &Params) -> Vec<String> {
        let mut ids = self.run(p).unwrap().0;
        ids.sort();
        ids
    }

    fn filter(&self, text: &str) -> Vec<String> {
        self.ids(&params(|p| p.filter = Some(text.into())))
    }

    fn all(&self, f: impl Fn(&Record) -> bool) -> Vec<String> {
        let mut v: Vec<_> = self.records.iter().filter(|r| f(r)).map(|r| r.id.clone()).collect();
        v.sort();
        v
    }
}

fn err(r: Result<(Vec<String>, i64), Problem>) -> Problem {
    r.expect_err("expected the request to be rejected")
}

// ---- the index --------------------------------------------------------------------------------

#[test]
fn every_record_is_in_every_index() {
    let f = fx();
    let count = |t: &str| -> i64 { f.conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap() };
    assert_eq!(count("collections") as usize, f.records.len());
    assert_eq!(count("collections_fts") as usize, f.records.len());
    assert_eq!(count("collection_bbox") as usize, f.records.iter().map(|r| r.bboxes.len()).sum::<usize>());
    assert_eq!(count("collection_keywords") as usize, f.records.iter().map(|r| r.keywords.len()).sum::<usize>());
    // The fixture must exercise what the tests below claim to.
    assert!(f.records.iter().any(|r| r.bboxes.len() > 1), "fixture lacks a multi-box entry");
    assert!(f.records.iter().any(|r| r.dt_start.is_some()), "fixture lacks a dated entry");
}

#[test]
fn reloading_replaces_rather_than_appends() {
    let f = fx();
    f.conn.execute_batch(&load_sql(&f.records).unwrap()).unwrap();
    assert_eq!(f.run(&params(|_| {})).unwrap().1 as usize, f.records.len());
}

// ---- free text --------------------------------------------------------------------------------

#[test]
fn free_text_finds_by_title_and_ors_commas() {
    let f = fx();
    let word = |r: &Record| r.title.split(|c: char| !c.is_alphabetic()).find(|w| w.len() >= 6).map(str::to_lowercase);
    let mut seen = f.records.iter().filter_map(|r| word(r).map(|w| (r.id.clone(), w)));
    let (id_a, a) = seen.next().expect("a title with a long word");
    let (_, b) = seen.find(|(_, w)| *w != a).expect("a second word");

    let by = |q: &str| f.ids(&params(|p| p.q = Some(q.into())));
    assert!(by(&a).contains(&id_a));
    let mut union: Vec<_> = by(&a).into_iter().chain(by(&b)).collect();
    union.sort();
    union.dedup();
    assert_eq!(by(&format!("{a},{b}")), union);
    assert!(by("zzqxjvk").is_empty());
}

#[test]
fn free_text_ranks_before_it_falls_back_to_id() {
    let f = fx();
    let r = &f.records[0];
    let w = r.title.split(|c: char| !c.is_alphabetic()).find(|w| w.len() >= 6).unwrap().to_lowercase();
    let (ids, matched) = f.run(&params(|p| p.q = Some(w.clone()))).unwrap();
    assert_eq!(matched as usize, ids.len());
    // An explicit sortby wins over relevance.
    let (by_id, _) = f.run(&params(|p| (p.q, p.sortby) = (Some(w), Some("id".into())))).unwrap();
    let mut sorted = by_id.clone();
    sorted.sort();
    assert_eq!(by_id, sorted);
}

// ---- bbox, datetime ---------------------------------------------------------------------------

fn overlaps(r: &Record, q: [f64; 4]) -> bool {
    r.bboxes.iter().any(|b| b[0] <= q[2] && b[2] >= q[0] && b[1] <= q[3] && b[3] >= q[1])
}

#[test]
fn bbox_matches_brute_force_including_multi_box_and_antimeridian() {
    let f = fx();
    for q in ["-10,35,30,60", "100,-40,160,-10", "-180,-90,180,90", "170,-20,-170,20", "-100,30,-90,40", "5,5,5.5,5.5"]
    {
        let n: Vec<f64> = q.split(',').map(|x| x.parse().unwrap()).collect();
        let pieces: Vec<[f64; 4]> = if n[0] > n[2] {
            vec![[n[0], n[1], 180.0, n[3]], [-180.0, n[1], n[2], n[3]]]
        } else {
            vec![[n[0], n[1], n[2], n[3]]]
        };
        let expect = f.all(|r| pieces.iter().any(|p| overlaps(r, *p)));
        assert_eq!(f.ids(&params(|p| p.bbox = Some(q.into()))), expect, "bbox={q}");
    }
    // the world misses only entries with no known extent
    assert_eq!(f.ids(&params(|p| p.bbox = Some("-180,-90,180,90".into()))), f.all(|r| !r.bboxes.is_empty()));
}

fn in_range(r: &Record, lo: Option<&str>, hi: Option<&str>) -> bool {
    hi.is_none_or(|h| r.dt_start.as_deref().is_none_or(|s| s <= h))
        && lo.is_none_or(|l| r.dt_end.as_deref().is_none_or(|e| e >= l))
}

#[test]
fn datetime_matches_brute_force_with_open_ends() {
    let f = fx();
    let cases = [
        ("2020-01-01/..", Some("2020-01-01T00:00:00Z"), None),
        ("../2015-06-01", None, Some("2015-06-01T23:59:59Z")),
        ("2019-01-01/2021-12-31", Some("2019-01-01T00:00:00Z"), Some("2021-12-31T23:59:59Z")),
        ("2022-03-04T05:06:07Z", Some("2022-03-04T05:06:07Z"), Some("2022-03-04T05:06:07Z")),
    ];
    for (q, lo, hi) in cases {
        assert_eq!(f.ids(&params(|p| p.datetime = Some(q.into()))), f.all(|r| in_range(r, lo, hi)), "datetime={q}");
    }
}

#[test]
fn bbox_and_datetime_and_text_combine_with_and() {
    let f = fx();
    let both = f.ids(&params(|p| {
        p.bbox = Some("-10,35,30,60".into());
        p.datetime = Some("2020-01-01/..".into());
    }));
    assert_eq!(
        both,
        f.all(|r| overlaps(r, [-10.0, 35.0, 30.0, 60.0]) && in_range(r, Some("2020-01-01T00:00:00Z"), None))
    );
}

// ---- CQL2 -------------------------------------------------------------------------------------

/// cql2's own evaluator, run over the stored documents.
fn oracle(f: &Fx, text: &str) -> Option<Vec<String>> {
    let expr = cql2::parse_text(text).unwrap();
    let mut ids = Vec::new();
    for r in &f.records {
        let doc: Value = serde_json::from_str(&r.doc).unwrap();
        match expr.clone().matches(Some(&doc)) {
            Ok(true) => ids.push(r.id.clone()),
            Ok(false) => {}
            Err(_) => return None,
        }
    }
    ids.sort();
    Some(ids)
}

#[test]
fn scalar_filters_agree_with_the_cql2_evaluator() {
    let f = fx();
    let license = f.records.iter().find(|r| r.license != "other").unwrap().license.clone();
    let filters = [
        format!("license = '{license}'"),
        format!("license <> '{license}'"),
        format!("license IN ('{license}', 'other', 'MIT')"),
        "total_bytes > 10000000 AND total_bytes <= 100000000000".to_owned(),
        "total_bytes BETWEEN 1000 AND 100000000".to_owned(),
        format!("NOT (license = '{license}') OR object_count < 10"),
        "title LIKE '%data%'".to_owned(),
        "title LIKE 'A%'".to_owned(),
        "CASEI(title) LIKE '%data%'".to_owned(),
        "CASEI(license) = CASEI('mit')".to_owned(),
        "A_OVERLAPS(keywords, ('geoparquet', 'remote-sensing'))".to_owned(),
        "A_CONTAINS(keywords, ('geoparquet'))".to_owned(),
        "A_CONTAINEDBY(keywords, ('geoparquet', 'cog', 'remote-sensing'))".to_owned(),
        "account_id = 'nobody' OR total_bytes >= 0".to_owned(),
    ];
    let mut compared = 0;
    for text in &filters {
        let got = f.filter(text);
        match oracle(&f, text) {
            Some(want) => {
                assert_eq!(got, want, "filter: {text}");
                compared += 1;
            }
            None => eprintln!("cql2's evaluator can't run `{text}`; not compared"),
        }
        assert!(f.filter(text).len() <= f.records.len());
    }
    assert!(compared >= 10, "only {compared} filters were compared against the evaluator");
}

#[test]
fn keyword_functions_match_set_logic() {
    let f = fx();
    let kw = |r: &Record, k: &str| r.keywords.iter().any(|x| x == k);
    let (a, b, nope) = ("geoparquet", "remote-sensing", "no-such-keyword");
    let has = |ids: &str| f.filter(ids);
    assert_eq!(has(&format!("A_OVERLAPS(keywords, ('{a}', '{nope}'))")), f.all(|r| kw(r, a)));
    assert_eq!(has(&format!("A_CONTAINS(keywords, ('{a}', '{b}'))")), f.all(|r| kw(r, a) && kw(r, b)));
    assert!(has(&format!("A_CONTAINS(keywords, ('{a}', '{nope}'))")).is_empty());
    assert_eq!(has(&format!("A_CONTAINEDBY(keywords, ('{a}'))")), f.all(|r| r.keywords.iter().all(|k| k == a)));
    assert_eq!(has(&format!("A_EQUALS(keywords, ('{a}'))")), f.all(|r| r.keywords.len() == 1 && kw(r, a)));
    // an entry with no keywords is "contained by" anything, and overlaps nothing. cql2-text has
    // no spelling for an empty list, so these two go in as cql2-json.
    let json = |op: &str| {
        f.ids(&params(|p| {
            p.filter = Some(format!(r#"{{"op":"{op}","args":[{{"property":"keywords"}},[]]}}"#));
            p.filter_lang = Some("cql2-json".into());
        }))
    };
    assert_eq!(json("a_containedBy"), f.all(|r| r.keywords.is_empty()));
    assert!(json("a_overlaps").is_empty());
    assert_eq!(json("a_contains").len(), f.records.len());
    // swapped operands read the other way round
    assert_eq!(has(&format!("A_CONTAINEDBY(('{a}'), keywords)")), has(&format!("A_CONTAINS(keywords, ('{a}'))")));
}

#[test]
fn spatial_filters_equal_the_bbox_parameter() {
    let f = fx();
    let by_param = f.ids(&params(|p| p.bbox = Some("-10,35,30,60".into())));
    assert!(!by_param.is_empty());
    assert_eq!(f.filter("S_INTERSECTS(geometry, BBOX(-10,35,30,60))"), by_param);
    assert_eq!(f.filter("S_INTERSECTS(BBOX(-10,35,30,60), geometry)"), by_param);
    // a polygon is compared by its envelope, which for a rectangle is exact
    assert_eq!(f.filter("S_INTERSECTS(geometry, POLYGON((-10 35, 30 35, 30 60, -10 60, -10 35)))"), by_param);
    // disjoint is the rest, minus entries with no extent at all
    assert_eq!(
        f.filter("S_DISJOINT(geometry, BBOX(-10,35,30,60))"),
        f.all(|r| !r.bboxes.is_empty() && !overlaps(r, [-10.0, 35.0, 30.0, 60.0]))
    );
    // a point
    let pt = f.filter("S_INTERSECTS(geometry, POINT(2.35 48.85))");
    assert_eq!(pt, f.all(|r| overlaps(r, [2.35, 48.85, 2.35, 48.85])));
}

#[test]
fn cql2_json_means_what_cql2_text_means() {
    let f = fx();
    let text = f.filter("total_bytes > 1000000 AND S_INTERSECTS(geometry, BBOX(-10,35,30,60))");
    let json = r#"{"op":"and","args":[{"op":">","args":[{"property":"total_bytes"},1000000]},
        {"op":"s_intersects","args":[{"property":"geometry"},{"bbox":[-10,35,30,60]}]}]}"#;
    let by_json = f.ids(&params(|p| {
        p.filter = Some(json.into());
        p.filter_lang = Some("cql2-json".into());
    }));
    assert_eq!(by_json, text);
    // and a GeoJSON geometry literal
    let poly = r#"{"op":"s_intersects","args":[{"property":"geometry"},
        {"type":"Polygon","coordinates":[[[-10,35],[30,35],[30,60],[-10,60],[-10,35]]]}]}"#;
    let by_poly = f.ids(&params(|p| {
        p.filter = Some(poly.into());
        p.filter_lang = Some("cql2-json".into());
    }));
    assert_eq!(by_poly, f.ids(&params(|p| p.bbox = Some("-10,35,30,60".into()))));
}

#[test]
fn temporal_functions_match_interval_logic() {
    let f = fx();
    let ge = |s: &Option<String>, v: &str| s.as_deref().is_some_and(|s| s >= v);
    let lt = |s: &Option<String>, v: &str| s.as_deref().is_some_and(|s| s < v);

    assert_eq!(
        f.filter("T_INTERSECTS(datetime, INTERVAL('2020-01-01','..'))"),
        f.ids(&params(|p| p.datetime = Some("2020-01-01/..".into())))
    );
    assert_eq!(
        f.filter("T_BEFORE(datetime, TIMESTAMP('2015-01-01T00:00:00Z'))"),
        f.all(|r| lt(&r.dt_end, "2015-01-01T00:00:00Z"))
    );
    // a bare date is its whole day: "after" it means after its last second
    assert_eq!(
        f.filter("T_AFTER(datetime, DATE('2020-01-01'))"),
        f.all(|r| r.dt_start.as_deref().is_some_and(|s| s > "2020-01-01T23:59:59Z"))
    );
    assert_eq!(
        f.filter("T_DURING(datetime, INTERVAL('2015-01-01','2025-01-01'))"),
        f.all(|r| ge(&r.dt_start, "2015-01-01T00:00:00Z")
            && r.dt_end.as_deref().is_some_and(|e| e <= "2025-01-01T23:59:59Z"))
    );
    // with the value first the relation reads the other way round
    assert_eq!(
        f.filter("T_AFTER(TIMESTAMP('2015-01-01T00:00:00Z'), datetime)"),
        f.filter("T_BEFORE(datetime, TIMESTAMP('2015-01-01T00:00:00Z'))")
    );
    // instants: created/updated
    assert_eq!(
        f.filter("T_INTERSECTS(created, INTERVAL('2024-01-01','..'))"),
        f.all(|r| ge(&r.created, "2024-01-01T00:00:00Z"))
    );
    assert_eq!(
        f.filter("updated >= TIMESTAMP('2025-01-01T00:00:00Z')"),
        f.all(|r| ge(&r.updated, "2025-01-01T00:00:00Z"))
    );
    // disjoint is the complement among entries whose dates are known to the comparison
    let inter = f.filter("T_INTERSECTS(datetime, INTERVAL('2020-01-01','..'))");
    let disj = f.filter("T_DISJOINT(datetime, INTERVAL('2020-01-01','..'))");
    assert!(inter.iter().all(|i| !disj.contains(i)));
}

#[test]
fn hostile_values_are_data_not_sql() {
    let f = fx();
    for evil in ["x'; DROP TABLE collections; --", "' OR '1'='1", "%", "\\"] {
        let escaped = evil.replace('\'', "''");
        assert!(f.filter(&format!("title = '{escaped}'")).is_empty(), "{evil}");
    }
    assert!(f.ids(&params(|p| p.q = Some("\" OR 1=1 --".into()))).len() <= f.records.len());
    assert_eq!(f.run(&params(|_| {})).unwrap().1 as usize, f.records.len());
}

// ---- sort, paging -----------------------------------------------------------------------------

#[test]
fn sorting_and_paging_walk_the_whole_set_once() {
    let f = fx();
    let mut want: Vec<_> = f.records.iter().collect();
    want.sort_by(|a, b| b.total_bytes.cmp(&a.total_bytes).then(a.id.cmp(&b.id)));

    let mut got = Vec::new();
    let mut token = None;
    loop {
        let p = Params {
            sortby: Some("-total_bytes".into()),
            limit: Some("7".into()),
            token: token.clone(),
            ..Default::default()
        };
        let (ids, matched) = f.run(&p).unwrap();
        assert_eq!(matched as usize, f.records.len());
        got.extend(ids.clone());
        let next = build(&p).unwrap().offset + 7;
        if ids.len() < 7 || next as i64 >= matched {
            break;
        }
        token = Some(next.to_string());
    }
    assert_eq!(got, want.iter().map(|r| r.id.clone()).collect::<Vec<_>>());

    // " title" is what an unescaped "+title" turns into on its way through a URL
    let a = f.run(&params(|p| p.sortby = Some(" title".into()))).unwrap().0;
    let b = f.run(&params(|p| p.sortby = Some("+title".into()))).unwrap().0;
    assert_eq!(a, b);
    let title = |id: &String| f.records.iter().find(|r| &r.id == id).unwrap().title.to_ascii_lowercase();
    assert!(a.windows(2).all(|w| title(&w[0]) <= title(&w[1])));
}

// ---- rejections -------------------------------------------------------------------------------

#[test]
fn bad_requests_are_400s_that_say_why() {
    let f = fx();
    let bad = |f_: &Fx, p: Params| err(f_.run(&p));
    let filter = |s: &str| params(|p| p.filter = Some(s.into()));

    let e = bad(&f, filter("nonsense = 1"));
    assert_eq!((e.status, e.detail.contains("collection-queryables")), (400, true));
    assert_eq!(bad(&f, filter("title = ")).status, 400);
    assert!(bad(&f, filter("S_WITHIN(geometry, BBOX(0,0,1,1))")).detail.contains("s_within"));
    assert!(bad(&f, filter("keywords = 'x'")).detail.contains("keywords"));
    assert!(bad(&f, filter("T_INTERSECTS(title, INTERVAL('2020-01-01','..'))")).detail.contains("datetime"));
    assert!(bad(
        &f,
        params(|p| {
            p.filter = Some("title = 'x'".into());
            p.filter_lang = Some("cql".into())
        })
    )
    .detail
    .contains("filter-lang"));
    assert_eq!(bad(&f, params(|p| p.bbox = Some("1,2,3".into()))).status, 400);
    assert_eq!(bad(&f, params(|p| p.datetime = Some("yesterday".into()))).status, 400);
    assert!(bad(&f, params(|p| p.sortby = Some("nope".into()))).detail.contains("sortable fields"));
    assert_eq!(bad(&f, params(|p| p.limit = Some("0".into()))).status, 400);

    // D1 allows 100 bound values; asking for more is a 400 here, not a failure there
    let many = (0..120).map(|i| format!("'k{i}'")).collect::<Vec<_>>().join(",");
    assert!(bad(&f, filter(&format!("A_OVERLAPS(keywords, ({many}))"))).detail.contains("limit"));
}

#[test]
fn query_bindings_match_their_placeholders() {
    let f = fx();
    for text in [
        "title = 'a' AND S_INTERSECTS(geometry, BBOX(0,0,1,1))",
        "A_EQUALS(keywords, ('a','b'))",
        "T_DURING(datetime, INTERVAL('2015-01-01','2016-01-01')) OR license IN ('a','b','c')",
    ] {
        let q = build(&params(|p| p.filter = Some(text.into()))).unwrap();
        assert_eq!(q.rows_sql.matches('?').count(), q.rows_binds.len(), "{text}");
        assert_eq!(q.count_sql.matches('?').count(), q.count_binds.len(), "{text}");
    }
    let _ = f;
}

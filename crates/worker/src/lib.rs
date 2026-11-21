//! HTTP in, D1 out. Everything that decides what to ask or how to answer lives in
//! `catalog-core`; this only moves bytes between it and the runtime.

use catalog_core::{
    api,
    cql::Bind,
    search::{self, Params},
    Problem,
};
use serde::Deserialize;
use worker::*;

type Reply = std::result::Result<Response, Problem>;

#[derive(Deserialize)]
struct DocRow {
    doc: String,
}

#[derive(Deserialize)]
struct CountRow {
    n: f64,
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let url = req.url()?;
    let base = url.origin().ascii_serialization();
    let mut resp = match route(&req, &url, &env, &base).await {
        Ok(r) => r,
        Err(p) => problem(&p)?,
    };
    let h = resp.headers_mut();
    h.set("access-control-allow-origin", "*")?;
    h.set("access-control-allow-methods", "GET, HEAD, OPTIONS")?;
    h.set("access-control-allow-headers", "*")?;
    Ok(resp)
}

async fn route(req: &Request, url: &Url, env: &Env, base: &str) -> Reply {
    match req.method() {
        Method::Options => return Response::empty().map(|r| r.with_status(204)).map_err(internal),
        Method::Get | Method::Head => {}
        _ => return Err(Problem { status: 405, title: "Method Not Allowed", detail: "this API is read-only".into() }),
    }
    match url.path().trim_end_matches('/') {
        "" => json(api::landing(base), "application/json"),
        "/api" => json(api::openapi(base), "application/vnd.oai.openapi+json;version=3.0"),
        "/conformance" => json(api::conformance(), "application/json"),
        "/collection-queryables" => json(api::queryables(base), "application/schema+json"),
        "/collections" => collections(url, env, base).await,
        p => match p.strip_prefix("/collections/") {
            Some(id) => collection(&decode(id), env, base).await,
            None => Err(Problem::not_found(format!("no resource at {p}"))),
        },
    }
}

async fn collections(url: &Url, env: &Env, base: &str) -> Reply {
    let pairs: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let q = search::build(&Params::from_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str()))))?;

    let db = env.d1("DB").map_err(internal)?;
    let rows = prepare(&db, &q.rows_sql, &q.rows_binds).map_err(internal)?;
    let count = prepare(&db, &q.count_sql, &q.count_binds).map_err(internal)?;
    // One round trip for both.
    let mut results = db.batch(vec![rows, count]).await.map_err(internal)?.into_iter();
    let (Some(rows), Some(count)) = (results.next(), results.next()) else {
        return Err(internal("D1 returned fewer results than statements"));
    };
    let docs: Vec<String> = rows.results::<DocRow>().map_err(internal)?.into_iter().map(|r| r.doc).collect();
    let matched = count.results::<CountRow>().map_err(internal)?.first().map_or(0.0, |c| c.n) as u64;

    json(api::collections_body(&docs, base, &pairs, q.offset, q.limit, matched), "application/json")
}

async fn collection(id: &str, env: &Env, base: &str) -> Reply {
    let db = env.d1("DB").map_err(internal)?;
    let stmt = prepare(&db, "SELECT doc FROM collections WHERE id = ?", &[Bind::Text(id.to_owned())]).map_err(internal)?;
    match stmt.first::<DocRow>(None).await.map_err(internal)? {
        Some(r) => json(api::collection_body(&r.doc, base), "application/json"),
        None => Err(Problem::not_found(format!("no collection `{id}`"))),
    }
}

fn prepare(db: &D1Database, sql: &str, binds: &[Bind]) -> Result<D1PreparedStatement> {
    // JS numbers are doubles: integers beyond i32 go as Real, which SQLite compares exactly up
    // to 2^53, far past any size or count here.
    let vals: Vec<D1Type> = binds
        .iter()
        .map(|b| match b {
            Bind::Text(s) => D1Type::Text(s),
            Bind::Real(r) => D1Type::Real(*r),
            Bind::Int(i) => i32::try_from(*i).map_or(D1Type::Real(*i as f64), D1Type::Integer),
        })
        .collect();
    db.prepare(sql).bind_refs(&vals)
}

fn json(body: String, content_type: &str) -> Reply {
    let mut r = Response::ok(body).map_err(internal)?;
    let h = r.headers_mut();
    h.set("content-type", content_type).map_err(internal)?;
    // The index is rebuilt daily; a few minutes at the edge costs nothing in freshness.
    h.set("cache-control", "public, max-age=300").map_err(internal)?;
    Ok(r)
}

fn problem(p: &Problem) -> Result<Response> {
    let mut r = Response::ok(p.json())?.with_status(p.status);
    r.headers_mut().set("content-type", "application/problem+json")?;
    Ok(r)
}

/// The cause goes to the log; the caller gets nothing that could leak a query.
fn internal(e: impl std::fmt::Display) -> Problem {
    console_error!("{e}");
    Problem { status: 500, title: "Internal Server Error", detail: "the request could not be completed".into() }
}

/// Collection ids contain a slash (`account/product`), which clients may send as `%2F`.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        match (b[i], b.get(i + 1).and_then(|c| hex(*c)), b.get(i + 2).and_then(|c| hex(*c))) {
            (b'%', Some(h), Some(l)) => {
                out.push(h << 4 | l);
                i += 3;
            }
            (c, ..) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

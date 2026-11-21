//! HTTP in, D1 out. Everything that decides what to ask or how to answer lives in
//! `catalog-core`; this only moves bytes between it and the runtime.

use catalog_core::{
    api,
    cql::Bind,
    search::{self, Params},
    Problem,
};
use percent_encoding::percent_decode_str;
use serde::Deserialize;
use worker::*;

type Reply = std::result::Result<Response, Failure>;

/// A `Problem` to answer with. Anything the runtime raises becomes the opaque 500 through `?`,
/// its cause going to the log; the orphan rule keeps that `From` from living on `Problem`.
struct Failure(Problem);

impl From<Problem> for Failure {
    fn from(p: Problem) -> Self {
        Self(p)
    }
}

impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        console_error!("{e}");
        Self(Problem::internal())
    }
}

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
        Err(Failure(p)) => problem(&p)?,
    };
    let h = resp.headers_mut();
    h.set("access-control-allow-origin", "*")?;
    h.set("access-control-allow-methods", "GET, HEAD, OPTIONS")?;
    h.set("access-control-allow-headers", "*")?;
    Ok(resp)
}

async fn route(req: &Request, url: &Url, env: &Env, base: &str) -> Reply {
    match req.method() {
        Method::Options => return Ok(Response::empty()?.with_status(204)),
        Method::Get | Method::Head => {}
        _ => return Err(Problem::method_not_allowed().into()),
    }
    match url.path().trim_end_matches('/') {
        "" => json(api::landing(base), "application/json"),
        "/api" => json(api::openapi(base), "application/vnd.oai.openapi+json;version=3.0"),
        "/conformance" => json(api::conformance(), "application/json"),
        "/collection-queryables" => json(api::queryables(base), "application/schema+json"),
        "/collections" => collections(url, env, base).await,
        p => match p.strip_prefix("/collections/") {
            Some(id) => collection(&decode(id), env, base).await,
            None => Err(Problem::not_found(format!("no resource at {p}")).into()),
        },
    }
}

async fn collections(url: &Url, env: &Env, base: &str) -> Reply {
    let pairs: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let q = search::build(&Params::from_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str()))))?;

    let db = env.d1("DB")?;
    let rows = prepare(&db, &q.rows_sql, &q.rows_binds)?;
    let count = prepare(&db, &q.count_sql, &q.count_binds)?;
    // One round trip for both.
    let mut results = db.batch(vec![rows, count]).await?.into_iter();
    let (Some(rows), Some(count)) = (results.next(), results.next()) else {
        console_error!("D1 returned fewer results than statements");
        return Err(Problem::internal().into());
    };
    let docs: Vec<String> = rows.results::<DocRow>()?.into_iter().map(|r| r.doc).collect();
    let matched = count.results::<CountRow>()?.first().map_or(0.0, |c| c.n) as u64;

    json(api::collections_body(&docs, base, &pairs, q.offset, q.limit, matched), "application/json")
}

async fn collection(id: &str, env: &Env, base: &str) -> Reply {
    let db = env.d1("DB")?;
    let stmt = prepare(&db, "SELECT doc FROM collections WHERE id = ?", &[Bind::Text(id.to_owned())])?;
    match stmt.first::<DocRow>(None).await? {
        Some(r) => json(api::collection_body(&r.doc, base), "application/json"),
        None => Err(Problem::not_found(format!("no collection `{id}`")).into()),
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
    let mut r = Response::ok(body)?;
    let h = r.headers_mut();
    h.set("content-type", content_type)?;
    // The index is rebuilt daily; a few minutes at the edge costs nothing in freshness.
    h.set("cache-control", "public, max-age=300")?;
    Ok(r)
}

fn problem(p: &Problem) -> Result<Response> {
    let mut r = Response::ok(p.json())?.with_status(p.status);
    r.headers_mut().set("content-type", "application/problem+json")?;
    Ok(r)
}

/// Collection ids contain a slash (`account/product`), which clients may send as `%2F`.
fn decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

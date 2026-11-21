# catalog-search

[![smoke](https://github.com/alukach/catalog-search/actions/workflows/smoke.yml/badge.svg)](https://github.com/alukach/catalog-search/actions/workflows/smoke.yml)

A prototype that indexes the [Source Cooperative metadata catalog](https://github.com/source-cooperative/metadata-catalog) into Cloudflare D1 and serves it as a STAC API with Collection Search. Filters are CQL2, parsed by [`cql2`](https://crates.io/crates/cql2) (cql2-rs). Demo: https://catalog-search.alukach.workers.dev

```
catalog.jsonl ──► indexer ──► out/load.sql ──► wrangler d1 execute ──► D1
                                                                       │
                       GET /collections?bbox=…&q=…&filter=… ◄── worker ┘
```

| crate | runs as | does |
|---|---|---|
| `crates/core` | native + wasm | schema, catalog entry → rows, search parameters and CQL2 → SQL, STAC response shapes |
| `crates/indexer` | native CLI | `catalog.jsonl` → one SQL file that replaces the index |
| `crates/worker` | wasm (Workers) | HTTP in, D1 out; no decisions of its own |

The indexer and the worker share `core`, so the code that writes the index is the code that reads it.

## Run it

Needs Rust with the `wasm32-unknown-unknown` target, and Node 22+ (wrangler 4.114 and later). `.nvmrc` says `22`.

```sh
npm install --prefix crates/worker
curl -L -o catalog.jsonl https://github.com/source-cooperative/metadata-catalog/releases/latest/download/catalog.jsonl

npm run index        # catalog.jsonl -> out/load.sql
npm run db:migrate   # local D1
npm run db:load
npm run dev          # http://127.0.0.1:8787   (first run builds the wasm; installs worker-build 0.9 into .tools/)

npm test             # 32 tests against a real SQLite
npm run e2e          # 21 checks of the running API against jq over the raw catalog
```

## Deploy

Two things deploy separately, because they change at different rates and need different credentials: the **Worker** (on every push, by Cloudflare's GitHub app) and the **data** (daily, by a GitHub Action). Workers Builds only runs on a push, and its default token has Workers Scripts, KV and R2 access but not D1, so it can't load the index.

**1. Create the database, once.**

```sh
cd crates/worker && npx wrangler d1 create catalog-search
```

Put the `database_id` it prints into `crates/worker/wrangler.toml`, replacing the all-zeros placeholder, and commit it. The id isn't a secret.

**2. Connect the repo in Cloudflare** (Workers & Pages → Create → Import a repository → this repo) with:

| setting | value |
|---|---|
| Root directory | `crates/worker` |
| Build command | `npm run build` |
| Deploy command | `npx wrangler deploy` |

`npm run build` installs `worker-build` 0.9 into `.tools/` and builds the wasm. It is the same command `wrangler dev` runs locally. Workers Builds doesn't read `[build]` in `wrangler.toml`, which is why it is set in the dashboard too.

**3. Let GitHub load the data.** Add repository secrets `CLOUDFLARE_API_TOKEN` (an API token with *D1: Edit*) and `CLOUDFLARE_ACCOUNT_ID`, then run the **load index** workflow once (Actions → load index → Run workflow). After that it runs daily, applying migrations and replacing the index from the catalog's latest release. Until the secrets exist it skips itself with a notice instead of failing. The `ci` workflow runs `cargo fmt --check`, the tests, clippy and a wasm build on every push and pull request; `.githooks/pre-commit` runs the format and clippy checks locally (`npm install` enables it, or `git config core.hooksPath .githooks`).

Run by hand against a real account (alukach, 2026-11-21): migrate, load, `wrangler deploy` and the 21 checks, which all pass against the deployed Worker. Not yet run: the Workers Builds connection (it needs a dashboard step; the Cloudflare GitHub app has to be authorised) and the scheduled `load index` workflow with real secrets.

## API

| | |
|---|---|
| `GET /` | STAC landing page |
| `GET /conformance` | conformance classes (only what is implemented and tested) |
| `GET /collections` | Collection Search |
| `GET /collections/{account}/{product}` | one collection (`%2F` works too) |
| `GET /collection-queryables` | JSON Schema of what a filter can name |

`/collections` parameters: `bbox`, `datetime`, `q`, `filter` (+ `filter-lang`, default `cql2-text`), `sortby`, `limit` (default 10, max 100), `token`. They combine with AND.

```sh
# text, place, time and a CQL2 filter together
curl -G localhost:8787/collections \
  --data-urlencode 'q=forest,tree cover' \
  --data-urlencode 'bbox=-80,-20,-40,10' \
  --data-urlencode 'datetime=2015-01-01/..' \
  --data-urlencode "filter=total_bytes > 1000000 AND A_OVERLAPS(keywords, ('remote-sensing','land-cover'))"

# cql2-json works the same way
curl -G localhost:8787/collections --data-urlencode filter-lang=cql2-json \
  --data-urlencode 'filter={"op":"s_intersects","args":[{"property":"geometry"},{"bbox":[-10,35,30,60]}]}'
```

**Queryables:** `id title description license account_id product_id data_mode featured total_bytes object_count created updated keywords geometry datetime`.

**CQL2 supported:** `AND OR NOT`, `= <> < <= > >=` (property–property too), `BETWEEN`, `IN`, `LIKE`, `IS NULL`, `CASEI`, `S_INTERSECTS` / `S_DISJOINT`, `T_INTERSECTS` / `T_DISJOINT` / `T_BEFORE` / `T_AFTER` / `T_DURING`, `A_CONTAINS` / `A_CONTAINEDBY` / `A_EQUALS` / `A_OVERLAPS` (on `keywords`). Anything else is a 400 that names the operator. Errors are RFC 9457 `application/problem+json`.

`q` is the STAC basic free-text search: comma-separated terms, any may match, each matched as a phrase, over title, description and keywords, ranked by BM25 (title ×10, keywords ×5).

## Design

- **D1 holds three indexes beside the document.** FTS5 for text; an R\*Tree for bbox; a keyword table, because neither SQLite nor D1's metadata filters can match inside an array.
- **One R\*Tree row per bbox.** A collection can carry several, and `id` must be unique, so the owning collection rides in an auxiliary column (`+cid`). A box with west > east crosses the antimeridian and is stored as two.
- **The stored document is the response.** `collections_body` splices stored JSON together and swaps `{{BASE}}` for the request origin, rather than parsing and re-serialising every page.
- **Bindings, never splicing.** Every value in a filter is a bound parameter, in the order its `?` appears. D1 allows 100 per query, so a larger filter is refused with a 400 instead of failing in D1.
- **`LIKE` is `GLOB`.** CQL2's `LIKE` is case-sensitive and SQLite's isn't.
- **What gets indexed.** Public, enabled entries with a STAC body (484 → 481 here; 321 `seed`/`unregistered` inventory-only entries are skipped). Heavy optional parts (`summaries`, `assets`) are dropped from a document over 40 KB because D1 rejects a statement over 100 KB; one entry has a 790 KB `summaries`.

### Rust, not TypeScript

`cql2` is native Rust, so Rust gets it with no wrapper, and the indexer shares its code with the worker. Rust is not why it's fast: the request is dominated by the D1 round trip (one, since rows and count go in a batch), and a TypeScript worker would spend about the same. The wasm is 1.0 MB, 363 KB gzipped, well inside Workers' limits. Locally a query takes ~4–5 ms (miniflare). Deployed, p50 was about 110 ms from a laptop to the remote D1, which includes the network round trip and is not a server-side figure.

## What was checked

- 32 tests on a real SQLite (FTS5 and R\*Tree included), loaded from `fixtures/sample.jsonl`, a slice of the real catalog. bbox, datetime, keyword and temporal results are compared with brute force in Rust; scalar filters are compared with cql2's own evaluator, so the translator and cql2 can't drift apart unnoticed.
- `npm run e2e` runs the live Worker against `jq` over the raw `catalog.jsonl`. All 21 agree, including a world-sized bbox and one across the antimeridian.

Both found real bugs in the first draft: an R\*Tree `id` can't repeat, and boxes with a hair of latitude overshoot (`-90.0000051`) had been dropped, hiding five global datasets from every bbox query.

## Not verified, and limits

- **Workers Builds.** Not connected yet, so `npm run build` has not run on its image; the docs don't say whether that image has a Rust toolchain. If the build fails there on `rustup` or `cargo`, that is why. The `load index` workflow has only been run without secrets, where it skips itself.
- **Remote D1 and R\*Tree: verified.** The migration (R\*Tree with the `+cid` auxiliary column, and FTS5) applied to a real remote D1, and the 2.86 MB load file went in in about 2 seconds. Local D1 is the stricter one: workerd only enabled R\*Tree on 2026-07-17, so an older wrangler refuses `CREATE VIRTUAL TABLE … rtree` with `SQLITE_AUTH`. Cloudflare's D1 page still lists only FTS5, JSON and math as supported extensions, so treat R\*Tree as working but undocumented.
- **Spatial literals other than a bbox are compared by their envelope.** A collection's geometry is its bboxes, so a rectangle is exact; a polygon can over-match, never under-match. For that reason `S_INTERSECTS` works but the spatial conformance classes are not claimed. Likewise the `T_` and `A_` functions work but aren't claimed as full classes.
- **Dirty extents in the catalog.** Overshoot is clamped. A few global products have west slightly greater than east (`-179.9375` / `-179.979`); by STAC's rule that is a box crossing the antimeridian, i.e. nearly the whole globe, and it is indexed that way.
- Entries with no known bbox are invisible to `bbox` and `S_INTERSECTS`. Unbounded datetimes (`null`) match any `datetime`, as STAC defines it.
- Only the first temporal interval is indexed; every bbox is.
- No `/items`, no `POST`, no authentication. Public data only.
- Not done: synonym expansion from the catalog's `_synonyms.json`, tuned ranking weights, `fields`, scheduled re-indexing. The load file replaces everything and has not been shown to be atomic on remote D1.
- `.tools/` holds a project-local `worker-build` 0.9, because the globally installed 0.7.5 rejects `worker` 0.9.

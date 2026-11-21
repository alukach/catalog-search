-- One row per STAC Collection. `doc` is the collection JSON served verbatim, with
-- `{{BASE}}` standing in for the API's origin so links can be filled in per request.
-- The columns beside it exist only to be filtered and sorted on.
CREATE TABLE IF NOT EXISTS collections (
  id           TEXT NOT NULL UNIQUE,   -- "{account_id}/{product_id}"
  account_id   TEXT NOT NULL,
  product_id   TEXT NOT NULL,
  title        TEXT,
  description  TEXT,
  license      TEXT,
  data_mode    TEXT,
  featured     INTEGER NOT NULL DEFAULT 0,
  total_bytes  INTEGER,
  object_count INTEGER,
  created      TEXT,                   -- UTC, "YYYY-MM-DDTHH:MM:SSZ", so text order is time order
  updated      TEXT,
  dt_start     TEXT,                   -- first temporal interval; NULL = open, per STAC
  dt_end       TEXT,
  has_bbox     INTEGER NOT NULL DEFAULT 0,
  doc          TEXT NOT NULL
);

-- Keywords as rows, because D1's metadata filters (and SQLite) can't match inside an array.
CREATE TABLE IF NOT EXISTS collection_keywords (
  cid  INTEGER NOT NULL,               -- collections.rowid
  term TEXT NOT NULL,
  PRIMARY KEY (term, cid)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS collection_keywords_cid ON collection_keywords (cid);

-- One row per spatial extent: a collection can carry several bboxes, and a box that crosses
-- the antimeridian is stored as two. `id` must be unique per box, so the owning collection
-- rides along as an auxiliary column (`+cid`). R*Tree stores 32-bit floats, rounding min
-- down and max up, so it can only over-match a boundary, never miss one.
CREATE VIRTUAL TABLE IF NOT EXISTS collection_bbox USING rtree(id, minx, maxx, miny, maxy, +cid INTEGER);

-- Free text. rowid = collections.rowid. Weights are applied in the query (bm25).
CREATE VIRTUAL TABLE IF NOT EXISTS collections_fts USING fts5(
  title, description, keywords,
  tokenize = 'porter unicode61 remove_diacritics 2'
);

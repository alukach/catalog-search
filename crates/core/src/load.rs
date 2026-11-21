//! Records to a SQL file that replaces the whole index.

use std::fmt::Write;

use crate::record::Record;

/// D1 rejects a statement over 100 KB; stay under it.
pub const MAX_STATEMENT: usize = 95_000;

fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn opt(s: &Option<String>) -> String {
    s.as_deref().map_or("NULL".into(), q)
}

fn num(n: Option<i64>) -> String {
    n.map_or("NULL".into(), |n| n.to_string())
}

/// Rowids are assigned here, 1..=N, so the child tables can name their parent without a lookup.
/// Fails, naming the entries, if any statement would exceed D1's limit.
pub fn load_sql(records: &[Record]) -> Result<String, String> {
    let mut out = String::from(
        "DELETE FROM collection_keywords;\nDELETE FROM collection_bbox;\nDELETE FROM collections_fts;\nDELETE FROM collections;\n",
    );
    let mut too_big = Vec::new();
    let mut box_id = 0;
    for (i, r) in records.iter().enumerate() {
        let rowid = i + 1;
        let stmts = [
            format!(
                "INSERT INTO collections (rowid, id, account_id, product_id, title, description, license, data_mode, featured, total_bytes, object_count, created, updated, dt_start, dt_end, has_bbox, doc) VALUES ({rowid}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {});",
                q(&r.id),
                q(&r.account_id),
                q(&r.product_id),
                q(&r.title),
                q(&r.description),
                q(&r.license),
                r.data_mode.as_deref().map_or("NULL".into(), q),
                r.featured as u8,
                num(r.total_bytes),
                num(r.object_count),
                opt(&r.created),
                opt(&r.updated),
                opt(&r.dt_start),
                opt(&r.dt_end),
                !r.bboxes.is_empty() as u8,
                q(&r.doc),
            ),
            format!(
                "INSERT INTO collections_fts (rowid, title, description, keywords) VALUES ({rowid}, {}, {}, {});",
                q(&r.title),
                q(&r.description),
                q(&r.keywords.join(" ")),
            ),
        ];
        for s in &stmts {
            if s.len() > MAX_STATEMENT {
                too_big.push(format!("{} ({} bytes)", r.id, s.len()));
            }
            out.push_str(s);
            out.push('\n');
        }
        if !r.keywords.is_empty() {
            let rows: Vec<_> = r.keywords.iter().map(|k| format!("({rowid}, {})", q(k))).collect();
            writeln!(out, "INSERT INTO collection_keywords (cid, term) VALUES {};", rows.join(", ")).unwrap();
        }
        if !r.bboxes.is_empty() {
            let rows: Vec<_> = r
                .bboxes
                .iter()
                .map(|b| {
                    box_id += 1;
                    format!("({box_id}, {}, {}, {}, {}, {rowid})", b[0], b[2], b[1], b[3])
                })
                .collect();
            writeln!(out, "INSERT INTO collection_bbox (id, minx, maxx, miny, maxy, cid) VALUES {};", rows.join(", ")).unwrap();
        }
    }
    if too_big.is_empty() {
        Ok(out)
    } else {
        Err(format!("statements over D1's 100 KB limit: {}", too_big.join(", ")))
    }
}

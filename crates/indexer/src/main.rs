//! `indexer --input catalog.jsonl --out load.sql`
//!
//! Turns the catalog's JSON-Lines release into one SQL file that replaces the D1 index:
//! `wrangler d1 execute DB --file load.sql [--local | --remote]`.

use std::{collections::BTreeMap, env, fs, process};

use catalog_core::{
    load::load_sql,
    record::{from_entry, Record},
};
use serde_json::Value;

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("indexer: {msg}");
    process::exit(1)
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let flag = |name: &str| args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone());
    let (Some(input), Some(out)) = (flag("--input"), flag("--out")) else {
        eprintln!("usage: indexer --input catalog.jsonl --out load.sql");
        process::exit(2)
    };

    let text = fs::read_to_string(&input).unwrap_or_else(|e| die(format!("{input}: {e}")));
    let mut records: Vec<Record> = Vec::new();
    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
    for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let entry: Value = serde_json::from_str(line).unwrap_or_else(|e| die(format!("{input}:{}: {e}", n + 1)));
        match from_entry(&entry) {
            Ok(r) => records.push(r),
            Err(why) => *skipped.entry(why.to_string()).or_default() += 1,
        }
    }

    // Stable output, and a duplicate id is a catalog bug worth stopping on, not a row to drop.
    records.sort_by(|a, b| a.id.cmp(&b.id));
    if let Some(w) = records.windows(2).find(|w| w[0].id == w[1].id) {
        die(format!("duplicate id {}", w[0].id));
    }

    let sql = load_sql(&records).unwrap_or_else(|e| die(e));
    fs::write(&out, &sql).unwrap_or_else(|e| die(format!("{out}: {e}")));

    let with_bbox = records.iter().filter(|r| !r.bboxes.is_empty()).count();
    println!("indexed {} collections ({with_bbox} with a bbox), {} bytes of SQL -> {out}", records.len(), sql.len());
    for (why, n) in skipped {
        println!("skipped {n}: {why}");
    }
}

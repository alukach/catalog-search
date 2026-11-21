//! CQL2 to a SQLite `WHERE` clause. `cql2` (cql2-rs) parses both encodings into one `Expr`; this
//! walks it. Values are always bound, never spliced into the SQL, and the bindings are pushed
//! in the order their `?` appear in the text.
//!
//! What a collection is, for filtering: scalar columns, a keyword set, a geometry (its bboxes)
//! and a `datetime` interval. Anything outside that is a 400 naming the part, not a silent
//! mismatch.

use cql2::Expr;
use serde_json::Value;

use crate::{
    bbox::{self, BBox},
    ts::{bound, exact, Edge},
    Problem, Result,
};

#[derive(Debug, Clone, PartialEq)]
pub enum Bind {
    Text(String),
    Int(i64),
    Real(f64),
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Num,
    Bool,
    Time,
    Keywords,
    Geometry,
    Interval,
}

/// Queryable name, kind, SQL column.
const TABLE: &[(&str, Kind, &str)] = &[
    ("id", Kind::Text, "c.id"),
    ("title", Kind::Text, "c.title"),
    ("description", Kind::Text, "c.description"),
    ("license", Kind::Text, "c.license"),
    ("account_id", Kind::Text, "c.account_id"),
    ("product_id", Kind::Text, "c.product_id"),
    ("data_mode", Kind::Text, "c.data_mode"),
    ("featured", Kind::Bool, "c.featured"),
    ("total_bytes", Kind::Num, "c.total_bytes"),
    ("object_count", Kind::Num, "c.object_count"),
    ("created", Kind::Time, "c.created"),
    ("updated", Kind::Time, "c.updated"),
    ("keywords", Kind::Keywords, ""),
    ("geometry", Kind::Geometry, ""),
    ("datetime", Kind::Interval, ""),
];

pub fn names() -> impl Iterator<Item = &'static str> {
    TABLE.iter().map(|t| t.0)
}

fn queryable(name: &str) -> Option<(Kind, &'static str)> {
    TABLE.iter().find(|t| t.0 == name).map(|t| (t.1, t.2))
}

fn unsupported(what: impl std::fmt::Display) -> Problem {
    Problem::bad(format!("{what} is not supported by this API"))
}

fn arity(op: &str, args: &[Box<Expr>], n: usize) -> Result<()> {
    if args.len() == n {
        Ok(())
    } else {
        Err(Problem::bad(format!("`{op}` takes {n} arguments, got {}", args.len())))
    }
}

pub fn to_sql(e: &Expr, b: &mut Vec<Bind>) -> Result<String> {
    let (op, args) = match e {
        Expr::Operation { op, args } => (op.as_str(), args),
        Expr::Bool(v) => return Ok(if *v { "1".into() } else { "0".into() }),
        _ => return Err(Problem::bad("a filter must be a boolean expression")),
    };
    // cql2 spells some operators in camelCase (`isNull`, `a_containedBy`); fold them so the
    // table below doesn't have to know which.
    let lower = op.to_ascii_lowercase();
    let op = lower.as_str();
    match op {
        "and" | "or" => {
            if args.is_empty() {
                return Err(Problem::bad(format!("`{op}` needs at least one argument")));
            }
            let parts = args.iter().map(|a| to_sql(a, b)).collect::<Result<Vec<_>>>()?;
            Ok(format!("({})", parts.join(if op == "and" { " AND " } else { " OR " })))
        }
        "not" => {
            arity(op, args, 1)?;
            Ok(format!("(NOT {})", to_sql(&args[0], b)?))
        }
        "=" | "<>" | "<" | "<=" | ">" | ">=" => {
            arity(op, args, 2)?;
            let (lk, rk) = (peek(&args[0]), peek(&args[1]));
            let l = scalar(&args[0], b, rk)?;
            let r = scalar(&args[1], b, lk)?;
            Ok(format!("({l} {op} {r})"))
        }
        "like" => {
            arity(op, args, 2)?;
            let fold = matches!(&*args[0], Expr::Operation { op, .. } if op == "casei");
            let l = scalar(&args[0], b, None)?;
            let pat = match &*args[1] {
                Expr::Operation { op, args } if op == "casei" && args.len() == 1 => literal(&args[0])?,
                other => literal(other)?,
            };
            let pat = if fold { pat.to_lowercase() } else { pat.to_owned() };
            // CQL2 LIKE is case-sensitive; SQLite's LIKE is not, but GLOB is.
            b.push(Bind::Text(like_to_glob(&pat)));
            Ok(format!("({l} GLOB ?)"))
        }
        "between" => {
            arity(op, args, 3)?;
            let k = peek(&args[0]);
            let v = scalar(&args[0], b, None)?;
            let lo = scalar(&args[1], b, k)?;
            let hi = scalar(&args[2], b, k)?;
            Ok(format!("({v} BETWEEN {lo} AND {hi})"))
        }
        "in" => {
            arity(op, args, 2)?;
            let Expr::Array(items) = &*args[1] else {
                return Err(Problem::bad("`in` needs a list of values"));
            };
            if items.is_empty() {
                return Ok("0".into());
            }
            let k = peek(&args[0]);
            let v = scalar(&args[0], b, None)?;
            let ph = items.iter().map(|i| scalar(i, b, k)).collect::<Result<Vec<_>>>()?;
            Ok(format!("({v} IN ({}))", ph.join(", ")))
        }
        "isnull" => {
            arity(op, args, 1)?;
            Ok(format!("({} IS NULL)", scalar(&args[0], b, None)?))
        }
        "s_intersects" | "s_disjoint" => spatial(op, args, b),
        "t_intersects" | "t_disjoint" | "t_before" | "t_after" | "t_during" => temporal(op, args, b),
        "a_contains" | "a_containedby" | "a_equals" | "a_overlaps" => array(op, args, b),
        other => Err(unsupported(format!("operator `{other}`"))),
    }
}

fn peek(e: &Expr) -> Option<Kind> {
    match e {
        Expr::Property { property } => queryable(property).map(|q| q.0),
        _ => None,
    }
}

fn literal(e: &Expr) -> Result<&str> {
    match e {
        Expr::Literal(s) => Ok(s),
        _ => Err(Problem::bad("expected a string literal")),
    }
}

fn num_bind(f: f64) -> Bind {
    if f.fract() == 0.0 && f.abs() < 9.0e15 {
        Bind::Int(f as i64)
    } else {
        Bind::Real(f)
    }
}

/// A column or a bound value, as SQL.
fn scalar(e: &Expr, b: &mut Vec<Bind>, hint: Option<Kind>) -> Result<String> {
    let text = |b: &mut Vec<Bind>, s: String| {
        b.push(Bind::Text(s));
        Ok("?".to_owned())
    };
    match e {
        Expr::Property { property } => match queryable(property) {
            Some((Kind::Text | Kind::Num | Kind::Bool | Kind::Time, col)) => Ok(col.to_owned()),
            Some(_) => Err(Problem::bad(format!(
                "`{property}` can't be compared like this; it takes a spatial, temporal or array function"
            ))),
            None => Err(Problem::bad(format!(
                "`{property}` is not queryable; see /collection-queryables for the properties that are"
            ))),
        },
        // Compared with a timestamp column, a string is read as a time.
        Expr::Literal(s) if hint == Some(Kind::Time) => text(b, exact(s, Edge::Start)?),
        Expr::Literal(s) => text(b, s.clone()),
        Expr::Float(f) => {
            b.push(num_bind(*f));
            Ok("?".into())
        }
        Expr::Bool(v) => {
            b.push(Bind::Int(*v as i64));
            Ok("?".into())
        }
        Expr::Timestamp { timestamp: t } | Expr::Date { date: t } => text(b, exact(literal(t)?, Edge::Start)?),
        Expr::Operation { op, args } if op == "casei" => {
            arity(op, args, 1)?;
            Ok(format!("lower({})", scalar(&args[0], b, hint)?))
        }
        Expr::Operation { op, .. } if op == "accenti" => Err(unsupported("ACCENTI")),
        _ => Err(unsupported("this kind of operand")),
    }
}

/// CQL2 `LIKE` pattern (`%`, `_`, `\` to escape) to a GLOB pattern.
fn like_to_glob(p: &str) -> String {
    let mut out = String::new();
    let mut it = p.chars();
    while let Some(c) = it.next() {
        let lit = match c {
            '%' => {
                out.push('*');
                continue;
            }
            '_' => {
                out.push('?');
                continue;
            }
            '\\' => it.next().unwrap_or('\\'),
            c => c,
        };
        match lit {
            '*' | '?' | '[' => {
                out.push('[');
                out.push(lit);
                out.push(']');
            }
            c => out.push(c),
        }
    }
    out
}

// ---- spatial ----------------------------------------------------------------------------------

/// `c.rowid IN (R*Tree hits)`, one per box, OR'd. A box overlaps another when each interval
/// does; this is the shape R*Tree answers from its index.
pub(crate) fn bbox_clause(boxes: &[BBox], b: &mut Vec<Bind>) -> String {
    let parts: Vec<String> = boxes
        .iter()
        .map(|x| {
            b.extend([x[2], x[0], x[3], x[1]].map(Bind::Real));
            "c.rowid IN (SELECT cid FROM collection_bbox WHERE minx <= ? AND maxx >= ? AND miny <= ? AND maxy >= ?)"
                .to_owned()
        })
        .collect();
    format!("({})", parts.join(" OR "))
}

fn spatial(op: &str, args: &[Box<Expr>], b: &mut Vec<Bind>) -> Result<String> {
    arity(op, args, 2)?;
    let is_geom = |e: &Expr| matches!(e, Expr::Property { property } if property == "geometry");
    let lit = match (is_geom(&args[0]), is_geom(&args[1])) {
        (true, false) => &args[1],
        (false, true) => &args[0],
        _ => return Err(Problem::bad(format!("`{op}` needs the `geometry` property and a spatial literal"))),
    };
    let any = bbox_clause(&envelope(lit)?, b);
    Ok(if op == "s_disjoint" { format!("(c.has_bbox = 1 AND NOT {any})") } else { any })
}

/// A collection's geometry is its bboxes, so a literal is compared by its bounding box. For a
/// rectangle that is exact; for any other shape it can over-match, never under-match.
fn envelope(e: &Expr) -> Result<Vec<BBox>> {
    match e {
        Expr::BBox { bbox: v } => {
            let n: Vec<f64> = v
                .iter()
                .map(|x| match &**x {
                    Expr::Float(f) => Ok(*f),
                    _ => Err(Problem::bad("BBOX takes numbers")),
                })
                .collect::<Result<_>>()?;
            let (w, s, e, n) = bbox::from_numbers(&n)
                .ok_or_else(|| Problem::bad(format!("BBOX takes 4 or 6 numbers, got {}", n.len())))?;
            bbox::check(w, s, e, n)?;
            Ok(bbox::split(w, s, e, n))
        }
        Expr::Geometry(g) => {
            let v = serde_json::to_value(g).map_err(|e| Problem::bad(format!("bad geometry: {e}")))?;
            let mut pts = Vec::new();
            collect(&v, &mut pts);
            if pts.is_empty() {
                return Err(Problem::bad("geometry has no coordinates"));
            }
            let fold = |f: fn(f64, f64) -> f64, i: usize, init: f64| {
                pts.iter().map(|p| if i == 0 { p.0 } else { p.1 }).fold(init, f)
            };
            let (w, e) = (fold(f64::min, 0, f64::MAX), fold(f64::max, 0, f64::MIN));
            let (s, n) = (fold(f64::min, 1, f64::MAX), fold(f64::max, 1, f64::MIN));
            bbox::check(w, s, e, n)?;
            Ok(bbox::split(w, s, e, n))
        }
        _ => Err(Problem::bad("expected a BBOX or geometry literal")),
    }
}

fn collect(v: &Value, pts: &mut Vec<(f64, f64)>) {
    match v {
        Value::Object(o) => {
            for k in ["coordinates", "geometries"] {
                if let Some(c) = o.get(k) {
                    collect(c, pts);
                }
            }
        }
        Value::Array(a) => match (a.first().and_then(Value::as_f64), a.get(1).and_then(Value::as_f64)) {
            (Some(x), Some(y)) => pts.push((x, y)),
            _ => a.iter().for_each(|x| collect(x, pts)),
        },
        _ => {}
    }
}

// ---- temporal ---------------------------------------------------------------------------------

/// A column pair holding an interval. `open`: NULL means unbounded (STAC's `null` in an
/// extent), not unknown.
pub(crate) struct TCol {
    pub start: &'static str,
    pub end: &'static str,
    pub open: bool,
}

pub(crate) const DATETIME: TCol = TCol { start: "c.dt_start", end: "c.dt_end", open: true };

#[derive(Clone, Copy)]
pub(crate) enum Rel {
    Intersects,
    Disjoint,
    /// column ends before the value starts
    Before,
    /// column starts after the value ends
    After,
    /// column lies inside the value
    During,
    /// column spans the value
    Contains,
}

/// `(start <= ?)`, or `(start IS NULL OR start <= ?)` when NULL means unbounded.
fn start_le(c: &TCol) -> String {
    if c.open {
        format!("({0} IS NULL OR {0} <= ?)", c.start)
    } else {
        format!("{} <= ?", c.start)
    }
}

fn end_ge(c: &TCol) -> String {
    if c.open {
        format!("({0} IS NULL OR {0} >= ?)", c.end)
    } else {
        format!("{} >= ?", c.end)
    }
}

/// The column's interval against `[lo, hi]`, either end of which may be unbounded.
pub(crate) fn temporal_clause(c: &TCol, rel: Rel, lo: Option<String>, hi: Option<String>, b: &mut Vec<Bind>) -> String {
    let mut parts = Vec::new();
    let mut with = |sql: String, v: &Option<String>, b: &mut Vec<Bind>| {
        if let Some(v) = v {
            parts.push(sql);
            b.push(Bind::Text(v.clone()));
        }
    };
    match rel {
        Rel::Intersects => {
            with(start_le(c), &hi, b);
            with(end_ge(c), &lo, b);
        }
        Rel::Disjoint => return format!("(NOT {})", temporal_clause(c, Rel::Intersects, lo, hi, b)),
        // A bounded end is needed to be before anything; "before the beginning of time" is empty.
        Rel::Before if lo.is_none() => return "0".into(),
        Rel::After if hi.is_none() => return "0".into(),
        Rel::Before => with(format!("{} < ?", c.end), &lo, b),
        Rel::After => with(format!("{} > ?", c.start), &hi, b),
        Rel::During => {
            with(format!("{} >= ?", c.start), &lo, b);
            with(format!("{} <= ?", c.end), &hi, b);
        }
        Rel::Contains => {
            with(start_le(c), &lo, b);
            with(end_ge(c), &hi, b);
        }
    }
    if parts.is_empty() {
        "1".into()
    } else {
        format!("({})", parts.join(" AND "))
    }
}

fn temporal_col(e: &Expr) -> Option<TCol> {
    let Expr::Property { property } = e else { return None };
    match property.as_str() {
        "datetime" => Some(DATETIME),
        "created" => Some(TCol { start: "c.created", end: "c.created", open: false }),
        "updated" => Some(TCol { start: "c.updated", end: "c.updated", open: false }),
        _ => None,
    }
}

fn instant(e: &Expr, edge: Edge) -> Result<Option<String>> {
    match e {
        Expr::Literal(s) => bound(s, edge),
        Expr::Timestamp { timestamp: t } | Expr::Date { date: t } => bound(literal(t)?, edge),
        _ => Err(Problem::bad("an INTERVAL takes dates, timestamps or `..`")),
    }
}

fn temporal_lit(e: &Expr) -> Result<(Option<String>, Option<String>)> {
    match e {
        Expr::Interval { interval } if interval.len() == 2 => {
            Ok((instant(&interval[0], Edge::Start)?, instant(&interval[1], Edge::End)?))
        }
        // An instant is a range of one; a bare date is the day.
        Expr::Timestamp { timestamp: t } | Expr::Date { date: t } => {
            let s = literal(t)?;
            Ok((bound(s, Edge::Start)?, bound(s, Edge::End)?))
        }
        _ => Err(Problem::bad("expected an INTERVAL, TIMESTAMP or DATE")),
    }
}

fn temporal(op: &str, args: &[Box<Expr>], b: &mut Vec<Bind>) -> Result<String> {
    arity(op, args, 2)?;
    let (col_first, col, lit) = match (temporal_col(&args[0]), temporal_col(&args[1])) {
        (Some(c), None) => (true, c, &args[1]),
        (None, Some(c)) => (false, c, &args[0]),
        _ => {
            return Err(Problem::bad(format!(
                "`{op}` needs one of the properties datetime, created or updated, and one temporal value"
            )))
        }
    };
    let (lo, hi) = temporal_lit(lit)?;
    // With the property second, the relation reads the other way round.
    let rel = match (op, col_first) {
        ("t_intersects", _) => Rel::Intersects,
        ("t_disjoint", _) => Rel::Disjoint,
        ("t_before", true) | ("t_after", false) => Rel::Before,
        ("t_after", true) | ("t_before", false) => Rel::After,
        ("t_during", true) => Rel::During,
        _ => Rel::Contains,
    };
    Ok(temporal_clause(&col, rel, lo, hi, b))
}

// ---- arrays (keywords) ------------------------------------------------------------------------

fn array(op: &str, args: &[Box<Expr>], b: &mut Vec<Bind>) -> Result<String> {
    arity(op, args, 2)?;
    let is_kw = |e: &Expr| matches!(e, Expr::Property { property } if property == "keywords");
    let (prop_first, lit) = match (&*args[0], &*args[1]) {
        (p, Expr::Array(a)) if is_kw(p) => (true, a),
        (Expr::Array(a), p) if is_kw(p) => (false, a),
        _ => return Err(Problem::bad(format!("`{op}` needs the `keywords` property and a list of strings"))),
    };
    let mut terms: Vec<String> = Vec::new();
    for t in lit {
        let s = literal(t)?.to_owned();
        if !terms.contains(&s) {
            terms.push(s);
        }
    }
    let n = terms.len();
    let list = || vec!["?"; n].join(", ");
    let sub = "FROM collection_keywords k WHERE k.cid = c.rowid";
    let mut push = |b: &mut Vec<Bind>| b.extend(terms.iter().cloned().map(Bind::Text));

    // keywords ⊇ list
    let has_all = |b: &mut Vec<Bind>, push: &mut dyn FnMut(&mut Vec<Bind>)| {
        if n == 0 {
            return "1".to_owned();
        }
        push(b);
        format!("(SELECT COUNT(*) {sub} AND k.term IN ({})) = {n}", list())
    };
    // keywords ⊆ list
    let only_in = |b: &mut Vec<Bind>, push: &mut dyn FnMut(&mut Vec<Bind>)| {
        if n == 0 {
            return format!("NOT EXISTS (SELECT 1 {sub})");
        }
        push(b);
        format!("NOT EXISTS (SELECT 1 {sub} AND k.term NOT IN ({}))", list())
    };

    // With the list first, "list contains keywords" is "keywords containedby list".
    let op = match (op, prop_first) {
        ("a_contains", false) => "a_containedby",
        ("a_containedby", false) => "a_contains",
        (o, _) => o,
    };
    Ok(match op {
        "a_contains" => format!("({})", has_all(b, &mut push)),
        "a_containedby" => format!("({})", only_in(b, &mut push)),
        "a_overlaps" if n == 0 => "0".into(),
        "a_overlaps" => {
            push(b);
            format!("EXISTS (SELECT 1 {sub} AND k.term IN ({}))", list())
        }
        _ => {
            let a = has_all(b, &mut push);
            let o = only_in(b, &mut push);
            format!("({a} AND {o})")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_becomes_glob() {
        assert_eq!(like_to_glob("fo%"), "fo*");
        assert_eq!(like_to_glob("a_c"), "a?c");
        assert_eq!(like_to_glob(r"100\%"), "100%");
        assert_eq!(like_to_glob("a*b?[c"), "a[*]b[?][[]c");
    }

    #[test]
    fn every_queryable_has_a_kind() {
        assert!(names().all(|n| queryable(n).is_some()));
    }
}

//! Timestamps are stored and compared as UTC text, `YYYY-MM-DDTHH:MM:SSZ`, so that text order
//! is time order and SQLite needs no date functions.

use crate::{Problem, Result};
use jiff::{
    civil::{Date, DateTime},
    tz::TimeZone,
    Timestamp,
};

/// Which end of a day a bare date stands for: a date used as a range start is its first
/// second, as a range end its last.
#[derive(Clone, Copy)]
pub enum Edge {
    Start,
    End,
}

pub fn norm(s: &str, edge: Edge) -> Option<String> {
    let s = s.trim();
    if let Ok(t) = s.parse::<Timestamp>() {
        return Some(fmt(t));
    }
    let (h, m, sec) = match edge {
        Edge::Start => (0, 0, 0),
        Edge::End => (23, 59, 59),
    };
    // jiff's `Date` parser accepts a datetime and drops the time, so a bare date is told apart first.
    if s.contains('T') {
        return s.parse::<DateTime>().ok().and_then(utc);
    }
    if let Ok(d) = s.parse::<Date>() {
        return utc(d.at(h, m, sec, 0));
    }
    // No offset given: read it as UTC rather than guessing a local zone.
    s.parse::<DateTime>().ok().and_then(utc)
}

fn utc(dt: DateTime) -> Option<String> {
    dt.to_zoned(TimeZone::UTC).ok().map(|z| fmt(z.timestamp()))
}

fn fmt(t: Timestamp) -> String {
    t.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A bound of an open-ended range: `..` or empty means unbounded.
pub fn bound(s: &str, edge: Edge) -> Result<Option<String>> {
    let s = s.trim();
    if s.is_empty() || s == ".." {
        return Ok(None);
    }
    exact(s, edge).map(Some)
}

/// `norm`, as a 400 naming the value when it isn't a date or timestamp.
pub fn exact(s: &str, edge: Edge) -> Result<String> {
    norm(s, edge).ok_or_else(|| Problem::bad(format!("`{s}` is not an RFC 3339 date or timestamp")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_the_formats_the_catalog_uses() {
        let n = |s| norm(s, Edge::Start).unwrap();
        assert_eq!(n("2023-10-03T20:46:06.861356+00:00"), "2023-10-03T20:46:06Z");
        assert_eq!(n("2025-08-21T16:39:55.569Z"), "2025-08-21T16:39:55Z");
        assert_eq!(n("2020-01-01T02:00:00+02:00"), "2020-01-01T00:00:00Z");
        assert_eq!(n("2020-01-01"), "2020-01-01T00:00:00Z");
        assert_eq!(norm("2020-01-01", Edge::End).unwrap(), "2020-01-01T23:59:59Z");
        assert_eq!(norm("nope", Edge::Start), None);
    }

    #[test]
    fn open_bounds() {
        assert_eq!(bound("..", Edge::Start), Ok(None));
        assert_eq!(bound("", Edge::End), Ok(None));
        assert!(bound("x", Edge::End).is_err());
    }
}

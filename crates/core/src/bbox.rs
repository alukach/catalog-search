//! Boxes are `[west, south, east, north]` in degrees.

use crate::{Problem, Result};

pub type BBox = [f64; 4];

/// A box with west > east crosses the antimeridian. R*Tree has no such notion, so it becomes
/// two boxes that don't.
pub fn split(w: f64, s: f64, e: f64, n: f64) -> Vec<BBox> {
    let (w, e) = (w.clamp(-180.0, 180.0), e.clamp(-180.0, 180.0));
    if w > e {
        vec![[w, s, 180.0, n], [-180.0, s, e, n]]
    } else {
        vec![[w, s, e, n]]
    }
}

/// `bbox` query parameter: `w,s,e,n`, or six numbers with elevation, which are ignored.
pub fn parse_param(raw: &str) -> Result<Vec<BBox>> {
    let nums: Vec<f64> = raw
        .split(',')
        .map(|p| p.trim().parse::<f64>().map_err(|_| Problem::bad(format!("`{p}` in bbox is not a number"))))
        .collect::<Result<_>>()?;
    let (w, s, e, n) =
        from_numbers(&nums).ok_or_else(|| Problem::bad(format!("bbox needs 4 or 6 numbers, got {}", nums.len())))?;
    check(w, s, e, n)?;
    Ok(split(w, s, e, n))
}

/// `(w, s, e, n)` from four numbers, or from six with the elevations dropped.
pub fn from_numbers(n: &[f64]) -> Option<(f64, f64, f64, f64)> {
    match *n {
        [w, s, e, n] | [w, s, _, e, n, _] => Some((w, s, e, n)),
        _ => None,
    }
}

pub fn check(w: f64, s: f64, e: f64, n: f64) -> Result<()> {
    if ![w, s, e, n].iter().all(|v| v.is_finite()) {
        return Err(Problem::bad("bbox values must be finite numbers"));
    }
    if !(-90.0..=90.0).contains(&s) || !(-90.0..=90.0).contains(&n) {
        return Err(Problem::bad("bbox latitudes must be within -90..90"));
    }
    if s > n {
        return Err(Problem::bad("bbox south must not exceed north"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antimeridian_splits_in_two() {
        assert_eq!(
            parse_param("170,-10,-170,10").unwrap(),
            vec![[170.0, -10.0, 180.0, 10.0], [-180.0, -10.0, -170.0, 10.0]]
        );
        assert_eq!(parse_param("1,2,3,4").unwrap(), vec![[1.0, 2.0, 3.0, 4.0]]);
        assert_eq!(parse_param("1,2,0,3,4,9").unwrap(), vec![[1.0, 2.0, 3.0, 4.0]]);
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse_param("1,2,3").is_err());
        assert!(parse_param("a,2,3,4").is_err());
        assert!(parse_param("1,5,3,4").is_err());
        assert!(parse_param("1,-91,3,4").is_err());
    }
}

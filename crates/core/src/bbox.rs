//! Boxes are `[west, south, east, north]` in degrees.

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
pub fn parse_param(raw: &str) -> Result<Vec<BBox>, String> {
    let nums: Vec<f64> = raw
        .split(',')
        .map(|p| p.trim().parse::<f64>().map_err(|_| format!("`{p}` in bbox is not a number")))
        .collect::<Result<_, _>>()?;
    let (w, s, e, n) = match nums.len() {
        4 => (nums[0], nums[1], nums[2], nums[3]),
        6 => (nums[0], nums[1], nums[3], nums[4]),
        k => return Err(format!("bbox needs 4 or 6 numbers, got {k}")),
    };
    check(w, s, e, n)?;
    Ok(split(w, s, e, n))
}

pub fn check(w: f64, s: f64, e: f64, n: f64) -> Result<(), String> {
    if ![w, s, e, n].iter().all(|v| v.is_finite()) {
        return Err("bbox values must be finite numbers".into());
    }
    if !(-90.0..=90.0).contains(&s) || !(-90.0..=90.0).contains(&n) {
        return Err("bbox latitudes must be within -90..90".into());
    }
    if s > n {
        return Err("bbox south must not exceed north".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antimeridian_splits_in_two() {
        assert_eq!(parse_param("170,-10,-170,10").unwrap(), vec![[170.0, -10.0, 180.0, 10.0], [-180.0, -10.0, -170.0, 10.0]]);
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

//! Number arguments of `/pregen`.

/// A length in blocks: `500`, `5c` (chunks), `+100` / `-2c` relative to `current`. Never negative.
pub fn parse_len(s: &str, current: f64) -> Option<f64> {
    let (rel, body) = match s.as_bytes().first() {
        Some(b'+') => (1.0, &s[1..]),
        Some(b'-') => (-1.0, &s[1..]),
        _ => (0.0, s),
    };
    let (body, mul) = match body.strip_suffix(['c', 'C']) {
        Some(b) => (b, 16.0),
        None => (body, 1.0),
    };
    let v = body.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)? * mul;
    let out = if rel == 0.0 { v } else { current + rel * v };
    (out.is_finite() && (0.0..=MAX_BLOCKS).contains(&out)).then_some(out)
}

/// A coordinate: `120`, `~` (the base), `~-30`.
pub fn parse_coord(s: &str, base: f64) -> Option<f64> {
    let v = match s.strip_prefix('~') {
        Some("") => Some(base),
        Some(rest) => rest.parse::<f64>().ok().map(|v| base + v),
        None => s.parse::<f64>().ok(),
    }?;
    (v.is_finite() && v.abs() <= MAX_BLOCKS).then_some(v)
}

/// The vanilla world limit; larger numbers would only overflow chunk coordinates.
pub const MAX_BLOCKS: f64 = 30_000_000.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths() {
        assert_eq!(parse_len("500", 0.0), Some(500.0));
        assert_eq!(parse_len("5c", 0.0), Some(80.0));
        assert_eq!(parse_len("+100", 500.0), Some(600.0));
        assert_eq!(parse_len("-2c", 500.0), Some(468.0));
        assert_eq!(parse_len("-600", 500.0), None);
        assert_eq!(parse_len("--5", 500.0), None);
        assert_eq!(parse_len("1e99", 0.0), None);
        assert_eq!(parse_len("nan", 0.0), None);
        assert_eq!(parse_len("x", 0.0), None);
    }

    #[test]
    fn coords() {
        assert_eq!(parse_coord("~", 12.0), Some(12.0));
        assert_eq!(parse_coord("~-30", 12.0), Some(-18.0));
        assert_eq!(parse_coord("-250.5", 0.0), Some(-250.5));
        assert_eq!(parse_coord("inf", 0.0), None);
        assert_eq!(parse_coord("99999999999", 0.0), None);
    }
}

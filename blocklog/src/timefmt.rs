//! Time arguments (`30s`, `1d12h`, `1h-2h`) and the "3.20/h ago" column.

/// Longest span a time argument may name (ten years).
pub const MAX_SPAN: u64 = 10 * 365 * 86_400;

/// `30s`, `10m`, `2h`, `7d`, `1w`, joined like `1d12h`, decimals like `1.5h`. Seconds.
pub fn parse_duration(text: &str) -> Option<u64> {
    if text.is_empty() || text.len() > 32 {
        return None;
    }
    let mut total = 0f64;
    let mut number = String::new();
    let mut parts = 0;
    for ch in text.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            number.push(ch);
            continue;
        }
        let unit = match ch.to_ascii_lowercase() {
            's' => 1.0,
            'm' => 60.0,
            'h' => 3600.0,
            'd' => 86_400.0,
            'w' => 604_800.0,
            _ => return None,
        };
        let value: f64 = number.parse().ok()?;
        number.clear();
        total += value * unit;
        parts += 1;
    }
    // A number with no unit is not a time (CoreProtect asks for the unit too)
    if !number.is_empty() || parts == 0 || !total.is_finite() || total < 0.0 {
        return None;
    }
    let secs = total.round() as u64;
    (secs <= MAX_SPAN).then_some(secs)
}

/// A `t:` value: `(older, newer)` in seconds before now. `2h` = `(7200, 0)`,
/// `1h-2h` = between one and two hours ago = `(7200, 3600)`.
pub fn parse_time(text: &str) -> Option<(u64, u64)> {
    match text.split_once('-') {
        Some((a, b)) => {
            let (a, b) = (parse_duration(a)?, parse_duration(b)?);
            Some((a.max(b), a.min(b)))
        }
        None => Some((parse_duration(text)?, 0)),
    }
}

/// How long ago, the CoreProtect way: `0.50/m`, `3.20/h`, `2.04/d`.
pub fn ago(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then) as f64;
    if secs < 3600.0 {
        format!("{:.2}/m", secs / 60.0)
    } else if secs < 86_400.0 {
        format!("{:.2}/h", secs / 3600.0)
    } else {
        format!("{:.2}/d", secs / 86_400.0)
    }
}

/// A span in words for messages: `45s`, `10m`, `2h`, `7d`, `1d12h`.
pub fn span(secs: u64) -> String {
    let mut out = String::new();
    let mut left = secs;
    for (unit, size) in [("d", 86_400), ("h", 3600), ("m", 60), ("s", 1)] {
        if left >= size {
            out.push_str(&format!("{}{unit}", left / size));
            left %= size;
        }
    }
    if out.is_empty() { "0s".into() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30s"), Some(30));
        assert_eq!(parse_duration("10m"), Some(600));
        assert_eq!(parse_duration("2h"), Some(7200));
        assert_eq!(parse_duration("7d"), Some(604_800));
        assert_eq!(parse_duration("1w"), Some(604_800));
        assert_eq!(parse_duration("1d12h"), Some(129_600));
        assert_eq!(parse_duration("1w2d3h4m5s"), Some(604_800 + 172_800 + 10_800 + 240 + 5));
        assert_eq!(parse_duration("1.5h"), Some(5400));
        assert_eq!(parse_duration("2H"), Some(7200));
    }

    #[test]
    fn bad_durations() {
        for bad in ["", "10", "h", "10x", "1h30", "-5m", "1..5h", "99999w", "1h 2m", "１h"] {
            assert_eq!(parse_duration(bad), None, "{bad}");
        }
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_time("2h"), Some((7200, 0)));
        assert_eq!(parse_time("1h-2h"), Some((7200, 3600)));
        assert_eq!(parse_time("2h-1h"), Some((7200, 3600)));
        assert_eq!(parse_time("1h-"), None);
        assert_eq!(parse_time("-"), None);
    }

    #[test]
    fn ago_column() {
        assert_eq!(ago(1000, 970), "0.50/m");
        assert_eq!(ago(100_000, 100_000 - 11_520), "3.20/h");
        assert_eq!(ago(1_000_000, 1_000_000 - 176_256), "2.04/d");
        // A clock that went backwards never shows a negative age
        assert_eq!(ago(10, 20), "0.00/m");
    }

    #[test]
    fn spans() {
        assert_eq!(span(0), "0s");
        assert_eq!(span(45), "45s");
        assert_eq!(span(129_600), "1d12h");
        assert_eq!(span(2_592_000), "30d");
    }
}

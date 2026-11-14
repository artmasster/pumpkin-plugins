//! Finding `%id_params%` in a text and putting values in (no server needed).

/// Longest text that is looked at; a longer one comes back as it is.
pub const MAX_TEXT: usize = 8192;
/// Placeholders filled in one text; the rest stay as written.
pub const MAX_TOKENS: usize = 64;
/// Longest `id_params` between the percent signs.
pub const MAX_TOKEN: usize = 120;

/// What may stand between the percent signs. No spaces: `100% sure, 50% not` has no placeholder.
fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | ',' | '#' | '/')
}

/// `basics_balance_formatted` → (`basics`, `balance_formatted`); `online` → (`online`, ``).
/// The id is lowercased, the parameters are kept as written. `None` = not a placeholder.
pub fn split(token: &str) -> Option<(String, &str)> {
    if token.is_empty() || token.len() > MAX_TOKEN || !token.chars().all(token_char) {
        return None;
    }
    let (id, params) = token.split_once('_').unwrap_or((token, ""));
    let ok = !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && id.starts_with(|c: char| c.is_ascii_alphabetic());
    ok.then(|| (id.to_ascii_lowercase(), params))
}

/// Put values into `text`. `lookup(id, params)` gives the value of one placeholder or `None`
/// when nobody knows it; an unknown one stays as written (another plugin may fill it later).
/// One pass: a value that itself looks like a placeholder is not looked up again, so a player
/// who names an item `%server_tps%` cannot make a plugin work for them twice.
pub fn expand(text: &str, lookup: &mut dyn FnMut(&str, &str) -> Option<String>) -> String {
    if text.len() > MAX_TEXT || !text.contains('%') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + 32);
    let mut rest = text;
    let mut filled = 0;
    while let Some(open) = rest.find('%') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('%') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let token = &after[..close];
        let value = if filled < MAX_TOKENS { split(token).and_then(|(id, params)| lookup(&id, params)) } else { None };
        match value {
            Some(v) => {
                out.push_str(&v);
                filled += 1;
                rest = &after[close + 1..];
            }
            // Not ours: the first sign is plain text, and the second may open the next one
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `{player_name}` → `%player_name%` (PlaceholderAPI's bracket form), for texts that are already full of percent signs.
pub fn brackets(text: &str) -> String {
    if !text.contains('{') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if split(&after[..close]).is_some() && after[..close].contains('_') => {
                out.push('%');
                out.push_str(&after[..close]);
                out.push('%');
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `1234567.891` → `1,234,567.89`
pub fn money(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let neg = v < 0.0;
    let cents = (v.abs() * 100.0).round() as u128;
    let (whole, frac) = (cents / 100, cents % 100);
    let digits = whole.to_string();
    let mut s = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            s.push(',');
        }
        s.push(c);
    }
    if frac > 0 {
        s.push_str(&format!(".{frac:02}"));
    }
    if neg { format!("-{s}") } else { s }
}

/// `3725` → `1h 2m`; under a minute → `45s`.
pub fn span(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// Wall clock of a Unix time at a fixed offset from UTC: (`HH:MM`, `HH:MM:SS`, `YYYY-MM-DD`).
pub fn clock(unix: u64, offset_minutes: i32) -> (String, String, String) {
    let t = unix as i64 + i64::from(offset_minutes) * 60;
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    (format!("{h:02}:{m:02}"), format!("{h:02}:{m:02}:{s:02}"), format!("{year:04}-{month:02}-{day:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(text: &str) -> String {
        expand(text, &mut |id, params| match (id, params) {
            ("player", "name") => Some("Alice".into()),
            ("server", "online") => Some("3".into()),
            ("basics", p) => Some(format!("<{p}>")),
            ("evil", _) => Some("%server_online%".into()),
            _ => None,
        })
    }

    #[test]
    fn placeholders_are_filled() {
        assert_eq!(fill("Hi %player_name%!"), "Hi Alice!");
        assert_eq!(fill("%player_name%%player_name%"), "AliceAlice");
        assert_eq!(fill("%PLAYER_name% %server_online%/%server_max%"), "Alice 3/%server_max%");
        // Parameters keep their case and may have more underscores
        assert_eq!(fill("%basics_balance_Formatted%"), "<balance_Formatted>");
        assert_eq!(fill("no placeholders"), "no placeholders");
        assert_eq!(fill(""), "");
    }

    #[test]
    fn plain_percent_signs_are_left_alone() {
        assert_eq!(fill("100% sure, 50% not"), "100% sure, 50% not");
        assert_eq!(fill("50%"), "50%");
        assert_eq!(fill("%%"), "%%");
        assert_eq!(fill("%unknown_thing% stays"), "%unknown_thing% stays");
        // The second sign of something that is not a placeholder may open a real one
        assert_eq!(fill("20% %player_name%"), "20% Alice");
        assert_eq!(fill("%not one%player_name%"), "%not oneAlice");
        assert_eq!(fill("ไทย %player_name% ไทย"), "ไทย Alice ไทย");
        assert_eq!(fill("%ไทย_x%"), "%ไทย_x%");
    }

    #[test]
    fn a_value_is_never_looked_up_again() {
        assert_eq!(fill("%evil_x%"), "%server_online%");
    }

    #[test]
    fn work_is_bounded() {
        let many = "%player_name%".repeat(MAX_TOKENS + 5);
        let out = fill(&many);
        assert_eq!(out.matches("Alice").count(), MAX_TOKENS);
        assert_eq!(out.matches("%player_name%").count(), 5);
        let long = format!("%player_name%{}", "x".repeat(MAX_TEXT));
        assert_eq!(fill(&long), long);
        assert_eq!(split(&"a".repeat(MAX_TOKEN + 1)), None);
        let mut calls = 0;
        expand(&"%a_b% ".repeat(500), &mut |_, _| {
            calls += 1;
            None
        });
        assert_eq!(calls, 500, "an unknown one is asked about once, not once per later sign");
    }

    #[test]
    fn tokens_are_split() {
        assert_eq!(split("player_name"), Some(("player".into(), "name")));
        assert_eq!(split("Server_TPS"), Some(("server".into(), "TPS")));
        assert_eq!(split("online"), Some(("online".into(), "")));
        assert_eq!(split("custom_discord-link"), Some(("custom".into(), "discord-link")));
        for bad in ["", "_x", "1abc_x", "has space", "a\"b", "a%b", "a;b_c", &"x".repeat(33)] {
            assert_eq!(split(bad), None, "{bad}");
        }
    }

    #[test]
    fn bracket_form() {
        assert_eq!(brackets("{player_name} has {basics_balance}"), "%player_name% has %basics_balance%");
        assert_eq!(brackets("{0} and {name} and {} and {a b_c}"), "{0} and {name} and {} and {a b_c}");
        assert_eq!(brackets("{{player_name}}"), "{%player_name%}");
        assert_eq!(brackets("no braces"), "no braces");
        assert_eq!(brackets("open { only"), "open { only");
    }

    #[test]
    fn numbers_and_times() {
        assert_eq!(money(0.0), "0");
        assert_eq!(money(1234567.891), "1,234,567.89");
        assert_eq!(money(1000.0), "1,000");
        assert_eq!(money(-12.5), "-12.50");
        assert_eq!(money(f64::NAN), "0");
        assert_eq!(span(45), "45s");
        assert_eq!(span(125), "2m 5s");
        assert_eq!(span(3725), "1h 2m");
        assert_eq!(span(90_000), "1d 1h");
        assert_eq!(clock(0, 0), ("00:00".into(), "00:00:00".into(), "1970-01-01".into()));
        // 2026-11-14 04:41:48 UTC = 11:41:48 in Bangkok
        assert_eq!(clock(1_794_631_308, 420), ("11:41".into(), "11:41:48".into(), "2026-11-14".into()));
        assert_eq!(clock(1_794_631_308, 0).2, "2026-11-14");
        // Leap day, and a negative offset crossing midnight backwards
        assert_eq!(clock(1_709_164_800, 0).2, "2024-02-29");
        assert_eq!(clock(1_709_164_800, -60), ("23:00".into(), "23:00:00".into(), "2024-02-28".into()));
    }
}

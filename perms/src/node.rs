//! Nodes, contexts, durations and argument splitting (plain Rust, no server types).
//!
//! Everything a holder (user or group) owns is a node, like in LuckPerms: a permission
//! (`basics:command.fly`), a parent group (`group.vip`), a prefix (`prefix.100.&c[Admin] `),
//! a suffix, a meta pair (`meta.rank-color.gold`), a weight (`weight.50`) or a display name.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const MAX_KEY: usize = 160;
pub const MAX_NAME: usize = 32;
pub const MAX_NODES: usize = 2000;
pub const MAX_GROUPS: usize = 500;
pub const MAX_TRACKS: usize = 200;
/// Names remembered for offline lookups; players beyond this are still served while online.
pub const MAX_NAMES: usize = 200_000;
pub const MAX_EXPORTS: usize = 100;
/// Ten years: longer "temporary" nodes are a typo.
pub const MAX_DURATION: u64 = 10 * 365 * 86_400;

/// Where a player is right now. Nodes with a context only apply when it matches.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Ctx {
    /// Dimension without `minecraft:` (`overworld`, `the_nether`, `the_end`).
    pub world: String,
    pub gamemode: String,
}

impl Ctx {
    pub fn new(world: &str, gamemode: &str) -> Self {
        Ctx { world: norm_world(world), gamemode: gamemode.to_ascii_lowercase() }
    }

    /// Used for players who are offline.
    pub fn offline() -> Self {
        Ctx { world: "overworld".into(), gamemode: "survival".into() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Wire", into = "Wire")]
pub struct Node {
    pub key: String,
    pub value: bool,
    /// Unix seconds; 0 = permanent.
    pub expiry: u64,
    pub context: BTreeMap<String, String>,
}

/// File shape: `"key"`, `"-key"` (denied) or a table when the node has an expiry or a context.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Wire {
    Short(String),
    Full {
        key: String,
        #[serde(default = "yes")]
        value: bool,
        #[serde(default)]
        expiry: u64,
        #[serde(default)]
        context: BTreeMap<String, String>,
    },
}

fn yes() -> bool {
    true
}

/// The MCSV plugins were renamed in 0.6.0 (Nexus became Basics, Roster became TabList, …). A node
/// saved under an old id (`nexus:command.fly`) means the node of the same plugin today, so every
/// node read from a file goes through [`modern_key`]. Basics keeps the same table.
const OLD_IDS: [(&str, &str); 13] = [
    ("nexus", "basics"),
    ("charter", "perms"),
    ("passport", "login"),
    ("homestead", "claims"),
    ("chronicle", "blocklog"),
    ("quill", "scripts"),
    ("herald", "discordlink"),
    ("roster", "tablist"),
    ("sculpt", "worldtools"),
    ("pulse", "monitor"),
    ("sweep", "cleaner"),
    ("marquee", "motd"),
    ("atlas", "worldmap"),
];

/// `key` with an old plugin id in front replaced by today's id (also inside `r=` regex nodes);
/// anything else unchanged.
pub fn modern_key(key: &str) -> String {
    let (lead, body) = match key.get(..2) {
        Some(p) if p.eq_ignore_ascii_case("r=") => key.split_at(2),
        _ => ("", key),
    };
    if let Some((ns, rest)) = body.split_once(':') {
        if let Some((_, new)) = OLD_IDS.iter().find(|(old, _)| *old == ns) {
            return format!("{lead}{new}:{rest}");
        }
    }
    key.to_owned()
}

impl From<Wire> for Node {
    fn from(w: Wire) -> Self {
        match w {
            Wire::Short(s) => match s.strip_prefix('-') {
                Some(k) => Node { key: modern_key(k), value: false, expiry: 0, context: BTreeMap::new() },
                None => Node { key: modern_key(&s), value: true, expiry: 0, context: BTreeMap::new() },
            },
            Wire::Full { key, value, expiry, context } => Node { key: modern_key(&key), value, expiry, context },
        }
    }
}

impl From<Node> for Wire {
    fn from(n: Node) -> Self {
        if n.is_plain() {
            Wire::Short(if n.value { n.key } else { format!("-{}", n.key) })
        } else {
            Wire::Full { key: n.key, value: n.value, expiry: n.expiry, context: n.context }
        }
    }
}

/// What a node means.
#[derive(Debug, PartialEq, Eq)]
pub enum Kind<'a> {
    Perm,
    Group(&'a str),
    Prefix(i32, &'a str),
    Suffix(i32, &'a str),
    Meta(&'a str, &'a str),
    Weight(i32),
    DisplayName(&'a str),
}

pub fn kind(key: &str) -> Kind<'_> {
    if let Some(g) = key.strip_prefix("group.") {
        if valid_name(g) {
            return Kind::Group(g);
        }
    } else if let Some(rest) = key.strip_prefix("prefix.") {
        if let Some((p, text)) = prio_text(rest) {
            return Kind::Prefix(p, text);
        }
    } else if let Some(rest) = key.strip_prefix("suffix.") {
        if let Some((p, text)) = prio_text(rest) {
            return Kind::Suffix(p, text);
        }
    } else if let Some(rest) = key.strip_prefix("meta.") {
        if let Some((k, v)) = rest.split_once('.') {
            if !k.is_empty() {
                return Kind::Meta(k, v);
            }
        }
    } else if let Some(n) = key.strip_prefix("weight.") {
        if let Ok(n) = n.parse::<i32>() {
            return Kind::Weight(n);
        }
    } else if let Some(n) = key.strip_prefix("displayname.") {
        if !n.is_empty() {
            return Kind::DisplayName(n);
        }
    }
    Kind::Perm
}

fn prio_text(rest: &str) -> Option<(i32, &str)> {
    let (p, text) = rest.split_once('.')?;
    Some((p.parse().ok()?, text))
}

impl Node {
    pub fn new(key: &str, value: bool) -> Self {
        Node { key: key.to_owned(), value, expiry: 0, context: BTreeMap::new() }
    }

    pub fn with(key: &str, value: bool, expiry: u64, context: BTreeMap<String, String>) -> Self {
        Node { key: key.to_owned(), value, expiry, context }
    }

    pub fn kind(&self) -> Kind<'_> {
        kind(&self.key)
    }

    pub fn is_plain(&self) -> bool {
        self.expiry == 0 && self.context.is_empty()
    }

    pub fn is_temp(&self) -> bool {
        self.expiry != 0
    }

    pub fn active(&self, now: u64) -> bool {
        self.expiry == 0 || self.expiry > now
    }

    /// Unknown context keys are kept in the file and never match.
    pub fn applies(&self, ctx: &Ctx) -> bool {
        self.context.iter().all(|(k, v)| match k.as_str() {
            "world" => *v == ctx.world,
            "gamemode" => *v == ctx.gamemode,
            _ => false,
        })
    }

    /// Order inside one holder when several nodes share a key: the more specific context first,
    /// then temporary before permanent, then denied before granted.
    pub fn rank(&self) -> (usize, bool, bool) {
        (self.context.len(), self.is_temp(), !self.value)
    }

    /// A node read from a file, made safe. Err says why the entry cannot be used: a node whose
    /// context is broken is refused rather than applied everywhere.
    pub fn cleaned(self) -> Result<Node, &'static str> {
        let key = norm_key(&self.key).map_err(KeyError::reason)?;
        let mut context = BTreeMap::new();
        for (k, v) in self.context {
            let k = k.trim().to_ascii_lowercase();
            if k == "server" {
                continue;
            }
            if !valid_ctx_key(&k) || v.is_empty() || v.len() > 64 || v.chars().any(|c| c.is_control() || c.is_whitespace()) {
                return Err("bad context");
            }
            context.insert(k.clone(), norm_ctx_value(&k, &v));
        }
        Ok(Node { key, value: self.value, expiry: self.expiry, context })
    }

    /// `key`, `-key`, plus ` (world=the_nether)` and the time left.
    pub fn describe(&self, now: u64, th: bool) -> String {
        let mut s = format!("{}{}", if self.value { "&a" } else { "&c-" }, self.key);
        if !self.context.is_empty() {
            let c: Vec<String> = self.context.iter().map(|(k, v)| format!("{k}={v}")).collect();
            s.push_str(&format!(" &7({})", c.join(" ")));
        }
        if self.is_temp() {
            let left = fmt_duration(self.expiry.saturating_sub(now), th);
            s.push_str(&if th { format!(" &e[เหลือ {left}]") } else { format!(" &e[{left} left]") });
        }
        s
    }
}

/// Group and track names: `a-z 0-9 _ -`, at most 32.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_NAME && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn valid_ctx_key(s: &str) -> bool {
    valid_name(s)
}

#[derive(Debug, PartialEq, Eq)]
pub enum KeyError {
    Empty,
    TooLong,
    BadChar,
    BadRegex,
}

impl KeyError {
    pub fn reason(self) -> &'static str {
        match self {
            KeyError::Empty => "empty key",
            KeyError::TooLong => "key too long",
            KeyError::BadChar => "bad character in key",
            KeyError::BadRegex => "bad regex",
        }
    }
}

/// How `settemp`/`addtemp` treats a temporary node that is already there (LuckPerms' modifiers).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TempMode {
    /// No modifier given: the new time replaces the old one.
    Set,
    /// Add the new duration to the time that is left.
    Accumulate,
    /// Keep whichever ends later.
    Replace,
    /// Refuse when there already is one.
    Deny,
}

pub fn parse_temp_mode(s: &str) -> Option<TempMode> {
    match s.to_ascii_lowercase().as_str() {
        "accumulate" => Some(TempMode::Accumulate),
        "replace" => Some(TempMode::Replace),
        "deny" => Some(TempMode::Deny),
        _ => None,
    }
}

pub const MAX_REGEX: usize = 120;
/// Regex nodes one player can have in effect; more are ignored.
pub const MAX_REGEX_NODES: usize = 32;
/// Nodes one shorthand key may stand for.
pub const MAX_SHORTHAND: usize = 64;

/// `r=<pattern>`: a node that grants or denies every permission the pattern matches in full.
pub fn regex_pattern(key: &str) -> Option<&str> {
    key.strip_prefix("r=").filter(|p| !p.is_empty())
}

/// The pattern compiled to match a whole node. regex-lite runs in linear time and the compiled
/// size is capped, so a hostile pattern cannot stall a check.
pub fn compile_regex(pattern: &str) -> Option<regex_lite::Regex> {
    if pattern.is_empty() || pattern.len() > MAX_REGEX {
        return None;
    }
    regex_lite::RegexBuilder::new(&format!("^(?:{pattern})$")).size_limit(64 * 1024).build().ok()
}

/// `kit.(a|b).use` → `kit.a.use`, `kit.b.use`; `home.(1-3)` → `home.1`, `home.2`, `home.3`.
/// None when the key has no group, a group is not closed, or it stands for too many nodes.
pub fn expand_shorthand(key: &str) -> Option<Vec<String>> {
    if regex_pattern(key).is_some() {
        return None;
    }
    let open = key.find('(')?;
    let close = open + key[open..].find(')')?;
    let inner = &key[open + 1..close];
    if inner.is_empty() || inner.contains('(') {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for alt in inner.split('|') {
        match alt.split_once('-').and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?))) {
            Some((a, b)) if a > b || (b - a) as usize >= MAX_SHORTHAND => return None,
            Some((a, b)) => parts.extend((a..=b).map(|n| n.to_string())),
            None => parts.push(alt.to_owned()),
        }
        if parts.len() > MAX_SHORTHAND {
            return None;
        }
    }
    let (head, tail) = (&key[..open], &key[close + 1..]);
    let tails = if tail.contains('(') { expand_shorthand(tail)? } else { vec![tail.to_owned()] };
    if parts.len() * tails.len() > MAX_SHORTHAND {
        return None;
    }
    Some(parts.iter().flat_map(|p| tails.iter().map(move |t| format!("{head}{p}{t}"))).collect())
}

/// Check and normalise a node key typed by a person. Permission keys are lowercased and may not
/// hold spaces; the text of a prefix, suffix, meta value or display name is kept as typed.
pub fn norm_key(raw: &str) -> Result<String, KeyError> {
    let raw = raw.trim_start();
    if raw.is_empty() {
        return Err(KeyError::Empty);
    }
    if raw.len() > MAX_KEY {
        return Err(KeyError::TooLong);
    }
    if raw.starts_with('-') || raw.chars().any(char::is_control) {
        return Err(KeyError::BadChar);
    }
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("r=") {
        // The pattern keeps its case: `\D` and `\d` are different things
        let pattern = raw[2..].trim_end();
        if pattern.chars().any(char::is_whitespace) || pattern.contains('"') {
            return Err(KeyError::BadChar);
        }
        return match compile_regex(pattern) {
            Some(_) => Ok(format!("r={pattern}")),
            None => Err(KeyError::BadRegex),
        };
    }
    let texty = ["prefix.", "suffix.", "meta.", "displayname."].iter().find(|p| lower.starts_with(**p));
    match texty {
        Some(p) => {
            let out = format!("{p}{}", &raw[p.len()..]);
            // Free text must really be one of those kinds, else it is a permission with odd characters
            if kind(&out) == Kind::Perm {
                return Err(KeyError::BadChar);
            }
            Ok(out)
        }
        None => {
            let lower = lower.trim_end().to_owned();
            if lower.chars().any(char::is_whitespace) || lower.contains('"') {
                return Err(KeyError::BadChar);
            }
            Ok(lower)
        }
    }
}

/// `nether`, `world_nether`, `minecraft:the_nether` → `the_nether`.
pub fn norm_world(raw: &str) -> String {
    let w = raw.trim().to_ascii_lowercase();
    let w = w.strip_prefix("minecraft:").unwrap_or(&w);
    match w {
        "world" | "overworld" => "overworld".into(),
        "nether" | "world_nether" | "the_nether" => "the_nether".into(),
        "end" | "world_the_end" | "world_end" | "the_end" => "the_end".into(),
        other => other.to_owned(),
    }
}

fn norm_ctx_value(key: &str, value: &str) -> String {
    match key {
        "world" => norm_world(value),
        _ => value.trim().to_ascii_lowercase(),
    }
}

/// `world=nether gamemode=creative` → map. `server=…` is accepted and dropped (one server only).
/// Err holds the word that is not a context.
pub fn parse_context(words: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for w in words {
        let Some((k, v)) = w.split_once('=') else { return Err(w.clone()) };
        let k = k.to_ascii_lowercase();
        if !valid_ctx_key(&k) || v.is_empty() || v.len() > 64 || v.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(w.clone());
        }
        if k == "server" {
            continue;
        }
        out.insert(k.clone(), norm_ctx_value(&k, v));
    }
    Ok(out)
}

/// `30s`, `10m`, `2h`, `7d`, `2w`, `1mo`, `1y`, also joined (`1d12h`). None when it is not a duration.
pub fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    if s.is_empty() || s.len() > 24 {
        return None;
    }
    let mut total: u64 = 0;
    let mut num = String::new();
    let mut unit = String::new();
    let mut parts: Vec<(u64, String)> = Vec::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            if !unit.is_empty() {
                parts.push((num.parse().ok()?, std::mem::take(&mut unit)));
                num.clear();
            }
            num.push(ch);
        } else if ch.is_ascii_alphabetic() {
            if num.is_empty() {
                return None;
            }
            unit.push(ch);
        } else {
            return None;
        }
    }
    if num.is_empty() || unit.is_empty() {
        return None;
    }
    parts.push((num.parse().ok()?, unit));
    for (n, unit) in parts {
        let mul = match unit.as_str() {
            "s" | "sec" | "secs" => 1,
            "m" | "min" | "mins" => 60,
            "h" | "hr" | "hrs" => 3600,
            "d" | "day" | "days" => 86_400,
            "w" | "wk" => 7 * 86_400,
            "mo" | "month" | "months" => 30 * 86_400,
            "y" | "yr" => 365 * 86_400,
            _ => return None,
        };
        total = total.checked_add(n.checked_mul(mul)?)?;
    }
    (total > 0 && total <= MAX_DURATION).then_some(total)
}

/// `1d 2h`, `5m 3s`: the two biggest units.
pub fn fmt_duration(secs: u64, th: bool) -> String {
    let units: [(u64, &str, &str); 4] = [(86_400, "d", " วัน"), (3600, "h", " ชม."), (60, "m", " นาที"), (1, "s", " วินาที")];
    let mut left = secs;
    let mut out: Vec<String> = Vec::new();
    for (size, en, thai) in units {
        if left >= size && out.len() < 2 {
            out.push(format!("{}{}", left / size, if th { thai } else { en }));
            left %= size;
        }
    }
    if out.is_empty() {
        return if th { "0 วินาที".into() } else { "0s".into() };
    }
    out.join(" ")
}

/// Split a command line into words. `"two words"` stays one word (for prefixes with spaces);
/// `\"` inside quotes is a quote. Input and word counts are capped.
pub fn tokenize(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut have = false;
    let mut it = raw.chars().take(2000).peekable();
    while let Some(ch) = it.next() {
        match ch {
            '"' => {
                quoted = !quoted;
                have = true;
            }
            '\\' if quoted && it.peek() == Some(&'"') => {
                cur.push('"');
                it.next();
            }
            c if c.is_whitespace() && !quoted => {
                if have {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            c if c.is_control() => {}
            c => {
                cur.push(c);
                have = true;
            }
        }
        if out.len() >= 48 {
            return out;
        }
    }
    if have {
        out.push(cur);
    }
    out
}

/// Standard text form of a UUID from its two halves.
pub fn uuid_string(high: u64, low: u64) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        high >> 32,
        (high >> 16) & 0xffff,
        high & 0xffff,
        low >> 48,
        low & 0xffff_ffff_ffff
    )
}

/// A UUID typed by a person (with or without dashes) in the standard lowercase form.
pub fn parse_uuid(s: &str) -> Option<String> {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let high = u64::from_str_radix(&hex[..16], 16).ok()?;
    let low = u64::from_str_radix(&hex[16..], 16).ok()?;
    Some(uuid_string(high, low))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_plugin_ids_in_saved_nodes() {
        assert_eq!(modern_key("nexus:command.fly"), "basics:command.fly");
        assert_eq!(modern_key("roster:group."), "tablist:group.");
        assert_eq!(modern_key(r"r=nexus:command\.(fly|heal)"), r"r=basics:command\.(fly|heal)");
        assert_eq!(modern_key("basics:command.fly"), "basics:command.fly");
        assert_eq!(modern_key("group.vip"), "group.vip");
        assert_eq!(modern_key("xnexus:command.fly"), "xnexus:command.fly");
        let nodes: Vec<Node> = serde_json::from_str(r#"["nexus:command.fly", "-sculpt:command", {"key": "chronicle:rollback", "expiry": 5}]"#).unwrap();
        let keys: Vec<&str> = nodes.iter().map(|n| n.key.as_str()).collect();
        assert_eq!(keys, ["basics:command.fly", "worldtools:command", "blocklog:rollback"]);
        assert!(!nodes[1].value);
    }

    fn w(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn kinds() {
        assert_eq!(kind("group.vip"), Kind::Group("vip"));
        assert_eq!(kind("group.Not Valid"), Kind::Perm);
        assert_eq!(kind("prefix.100.&c[Admin] "), Kind::Prefix(100, "&c[Admin] "));
        assert_eq!(kind("prefix.x.y"), Kind::Perm);
        assert_eq!(kind("suffix.5. &7*"), Kind::Suffix(5, " &7*"));
        assert_eq!(kind("meta.color.gold.dark"), Kind::Meta("color", "gold.dark"));
        assert_eq!(kind("weight.50"), Kind::Weight(50));
        assert_eq!(kind("weight.abc"), Kind::Perm);
        assert_eq!(kind("displayname.Very Important"), Kind::DisplayName("Very Important"));
        assert_eq!(kind("basics:command.fly"), Kind::Perm);
        assert_eq!(kind("tablist:group.admin"), Kind::Perm);
    }

    #[test]
    fn keys_are_normalised() {
        assert_eq!(norm_key("Basics:Command.Fly").unwrap(), "basics:command.fly");
        assert_eq!(norm_key("prefix.10.&6[VIP] ").unwrap(), "prefix.10.&6[VIP] ");
        assert_eq!(norm_key("PREFIX.10.Hi").unwrap(), "prefix.10.Hi");
        assert_eq!(norm_key("meta.Color.Gold").unwrap(), "meta.Color.Gold");
        assert_eq!(norm_key(""), Err(KeyError::Empty));
        assert_eq!(norm_key("-x"), Err(KeyError::BadChar));
        assert_eq!(norm_key("a b"), Err(KeyError::BadChar));
        assert_eq!(norm_key("a\u{7}b"), Err(KeyError::BadChar));
        assert_eq!(norm_key("prefix.notanumber.x y"), Err(KeyError::BadChar));
        assert_eq!(norm_key(&"a".repeat(MAX_KEY + 1)), Err(KeyError::TooLong));
        assert_eq!(norm_key("*").unwrap(), "*");
    }

    #[test]
    fn worlds() {
        for (a, b) in [
            ("minecraft:the_nether", "the_nether"),
            ("nether", "the_nether"),
            ("world_nether", "the_nether"),
            ("World", "overworld"),
            ("minecraft:overworld", "overworld"),
            ("end", "the_end"),
            ("world_the_end", "the_end"),
            ("custom:arena", "custom:arena"),
        ] {
            assert_eq!(norm_world(a), b);
        }
    }

    #[test]
    fn contexts() {
        let c = parse_context(&w("world=minecraft:the_nether GameMode=Creative server=lobby")).unwrap();
        assert_eq!(c.get("world").unwrap(), "the_nether");
        assert_eq!(c.get("gamemode").unwrap(), "creative");
        assert!(!c.contains_key("server"));
        assert_eq!(parse_context(&w("world=nether true")), Err("true".to_owned()));
        assert!(parse_context(&w("=x")).is_err());
        assert!(parse_context(&w("world=")).is_err());

        let n = Node::with("x", true, 0, c);
        assert!(n.applies(&Ctx::new("minecraft:the_nether", "creative")));
        assert!(!n.applies(&Ctx::new("minecraft:the_nether", "survival")));
        assert!(!n.applies(&Ctx::new("minecraft:overworld", "creative")));
        // unknown keys never match
        let odd = Node::with("x", true, 0, parse_context(&w("region=spawn")).unwrap());
        assert!(!odd.applies(&Ctx::offline()));
        assert!(Node::new("x", true).applies(&Ctx::offline()));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30s"), Some(30));
        assert_eq!(parse_duration("10m"), Some(600));
        assert_eq!(parse_duration("2h"), Some(7200));
        assert_eq!(parse_duration("7d"), Some(604_800));
        assert_eq!(parse_duration("1mo"), Some(2_592_000));
        assert_eq!(parse_duration("1d12h"), Some(129_600));
        assert_eq!(parse_duration("1y"), Some(31_536_000));
        assert_eq!(parse_duration("2w"), Some(1_209_600));
        for bad in ["", "10", "s", "10x", "-5m", "1.5h", "0s", "99999y", "5m3", "true"] {
            assert_eq!(parse_duration(bad), None, "{bad}");
        }
        assert_eq!(fmt_duration(90_061, false), "1d 1h");
        assert_eq!(fmt_duration(59, false), "59s");
        assert_eq!(fmt_duration(0, false), "0s");
        assert_eq!(fmt_duration(3600, true), "1 ชม.");
    }

    #[test]
    fn expiry() {
        let n = Node::with("x", true, 100, BTreeMap::new());
        assert!(n.active(99));
        assert!(!n.active(100));
        assert!(Node::new("x", true).active(u64::MAX));
    }

    #[test]
    fn tokens() {
        assert_eq!(tokenize("user  Alice   info"), w("user Alice info"));
        assert_eq!(tokenize(r#"meta setprefix 10 "&c[Admin] " world=nether"#), vec!["meta", "setprefix", "10", "&c[Admin] ", "world=nether"]);
        assert_eq!(tokenize(r#"a "say \"hi\"" b"#), vec!["a", "say \"hi\"", "b"]);
        assert_eq!(tokenize(r#"a "" b"#), vec!["a", "", "b"]);
        assert_eq!(tokenize(r#"a "unclosed text"#), vec!["a", "unclosed text"]);
        assert!(tokenize(&"x ".repeat(500)).len() <= 48);
        assert!(tokenize("").is_empty());
    }

    #[test]
    fn wire_shapes() {
        let nodes = vec![
            Node::new("group.default", true),
            Node::new("basics:command.god", false),
            Node::with("x.y", true, 123, parse_context(&w("world=nether")).unwrap()),
        ];
        let json = serde_json::to_string(&nodes).unwrap();
        assert_eq!(json, r#"["group.default","-basics:command.god",{"key":"x.y","value":true,"expiry":123,"context":{"world":"the_nether"}}]"#);
        let back: Vec<Node> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, nodes);
        // a hand-written table may leave fields out
        let short: Vec<Node> = serde_json::from_str(r#"[{"key":"a.b"},{"key":"c","value":false,"context":{"world":"nether"}}]"#).unwrap();
        assert_eq!(short[0], Node::new("a.b", true));
        assert_eq!(short[1].clone().cleaned().unwrap().context.get("world").unwrap(), "the_nether");
        assert_eq!(Node::new("bad key", true).cleaned(), Err("bad character in key"));
        assert_eq!(Node::new("r=(unclosed", true).cleaned(), Err("bad regex"));
        let odd: Node = serde_json::from_str(r#"{"key":"a","context":{"Bad Key":"x"}}"#).unwrap();
        assert_eq!(odd.cleaned(), Err("bad context"), "a node with a broken context must not apply everywhere");
        let srv: Node = serde_json::from_str(r#"{"key":"a","context":{"server":"lobby"}}"#).unwrap();
        assert!(srv.cleaned().unwrap().context.is_empty());
    }

    #[test]
    fn regex_keys() {
        assert_eq!(norm_key("r=Basics:Command\\.(fly|heal)").unwrap(), "r=Basics:Command\\.(fly|heal)", "the pattern is kept as typed");
        assert_eq!(norm_key("R=a.*").unwrap(), "r=a.*");
        assert_eq!(norm_key("r=(a"), Err(KeyError::BadRegex));
        assert_eq!(norm_key("r="), Err(KeyError::BadRegex));
        assert_eq!(norm_key(&format!("r={}", "a".repeat(MAX_REGEX + 1))), Err(KeyError::BadRegex));
        assert_eq!(norm_key("r=a b"), Err(KeyError::BadChar));
        let re = compile_regex("basics:command\\.(fly|heal)").unwrap();
        assert!(re.is_match("basics:command.fly"));
        assert!(!re.is_match("basics:command.flying"), "the whole node must match");
        assert!(!re.is_match("xbasics:command.fly"));
        // a pattern that would explode a backtracking engine is just slow-free here
        let evil = compile_regex("(a+)+$").unwrap();
        assert!(!evil.is_match(&format!("{}b", "a".repeat(150))));
        assert_eq!(regex_pattern("r=x"), Some("x"));
        assert_eq!(regex_pattern("rx"), None);
    }

    #[test]
    fn shorthand() {
        assert_eq!(expand_shorthand("kit.(a|b).use").unwrap(), vec!["kit.a.use", "kit.b.use"]);
        assert_eq!(expand_shorthand("home.(1-3)").unwrap(), vec!["home.1", "home.2", "home.3"]);
        assert_eq!(expand_shorthand("x.(a|b).(1-2)").unwrap(), vec!["x.a.1", "x.a.2", "x.b.1", "x.b.2"]);
        assert_eq!(expand_shorthand("basics:command.(fly|heal|a-z)").unwrap(), vec!["basics:command.fly", "basics:command.heal", "basics:command.a-z"]);
        assert_eq!(expand_shorthand("plain.node"), None);
        assert_eq!(expand_shorthand("open.(a|b"), None);
        assert_eq!(expand_shorthand("empty.()"), None);
        assert_eq!(expand_shorthand("r=(a|b)"), None, "a regex is not shorthand");
        assert_eq!(expand_shorthand("big.(1-500)"), None);
        assert_eq!(expand_shorthand("big.(1-9).(1-9)"), None, "81 nodes is over the cap");
        assert_eq!(expand_shorthand("ok.(1-8).(1-8)").unwrap().len(), 64);
    }

    #[test]
    fn temp_modes() {
        assert_eq!(parse_temp_mode("Accumulate"), Some(TempMode::Accumulate));
        assert_eq!(parse_temp_mode("replace"), Some(TempMode::Replace));
        assert_eq!(parse_temp_mode("deny"), Some(TempMode::Deny));
        assert_eq!(parse_temp_mode("world=nether"), None);
    }

    #[test]
    fn uuids() {
        let s = uuid_string(0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210);
        assert_eq!(s, "01234567-89ab-cdef-fedc-ba9876543210");
        assert_eq!(parse_uuid("0123456789ABCDEFFEDCBA9876543210").unwrap(), s);
        assert_eq!(parse_uuid(&s).unwrap(), s);
        assert!(parse_uuid("Alice").is_none());
    }
}

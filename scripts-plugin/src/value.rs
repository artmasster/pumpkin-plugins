//! Values of the script language: text, numbers, players, locations, items, timespans, dates.
use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerRef {
    pub uuid: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Loc {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    /// Script-facing world name (`world`, `world_nether`, `world_the_end`).
    pub world: String,
    #[serde(default)]
    pub yaw: f32,
    #[serde(default)]
    pub pitch: f32,
}

impl Loc {
    pub fn block(&self) -> (i32, i32, i32) {
        (self.x.floor() as i32, self.y.floor() as i32, self.z.floor() as i32)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// Registry id without `minecraft:` (`diamond_sword`).
    pub id: String,
    pub count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lore: Vec<String>,
    /// Enchantment ids with their level.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ench: Vec<(String, u32)>,
}

/// A mob, an item on the ground, a projectile: anything in the world that is not a player.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntityRef {
    pub id: i32,
    /// Entity type id (`zombie`, `snow_golem`).
    pub kind: String,
    pub world: String,
}

impl Item {
    pub fn new(id: &str, count: u32) -> Self {
        Item { id: norm_id(id), count, name: None, lore: Vec::new(), ench: Vec::new() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "lowercase")]
pub enum Value {
    None,
    Bool(bool),
    Num(f64),
    Text(String),
    Player(PlayerRef),
    Console,
    Loc(Loc),
    Item(Item),
    /// A block in the world; its type is read when it is used.
    Block(Loc),
    World(String),
    Entity(EntityRef),
    /// Timespan in seconds.
    Span(f64),
    /// Unix time in seconds.
    Date(f64),
    List(Vec<Value>),
}

impl Value {
    pub fn text(s: impl Into<String>) -> Value {
        Value::Text(s.into())
    }

    pub fn is_none(&self) -> bool {
        match self {
            Value::None => true,
            Value::List(l) => l.is_empty(),
            _ => false,
        }
    }

    /// Every single value in this value (a list is flattened one level deep, `None` is nothing).
    pub fn flat(self) -> Vec<Value> {
        match self {
            Value::None => Vec::new(),
            Value::List(l) => l.into_iter().filter(|v| !matches!(v, Value::None)).collect(),
            v => vec![v],
        }
    }

    pub fn first(self) -> Value {
        match self {
            Value::List(l) => l.into_iter().next().unwrap_or(Value::None),
            v => v,
        }
    }

    pub fn from_list(mut l: Vec<Value>) -> Value {
        match l.len() {
            0 => Value::None,
            1 => l.remove(0),
            _ => Value::List(l),
        }
    }

    pub fn num(&self) -> Option<f64> {
        match self {
            Value::Num(n) | Value::Span(n) | Value::Date(n) => Some(*n),
            Value::Text(t) => parse_number(t),
            Value::Bool(b) => Some(f64::from(u8::from(*b))),
            Value::List(l) if l.len() == 1 => l[0].num(),
            _ => None,
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Text(t) => matches!(t.to_lowercase().as_str(), "true" | "yes" | "on"),
            Value::Num(n) => *n != 0.0,
            Value::None => false,
            Value::List(l) => !l.is_empty(),
            _ => true,
        }
    }
}

/// `minecraft:Diamond Sword` → `diamond_sword`.
pub fn norm_id(id: &str) -> String {
    let id = id.trim().to_lowercase();
    let id = id.strip_prefix("minecraft:").unwrap_or(&id);
    id.replace([' ', '-'], "_")
}

/// The script-facing name of a world from anything an owner may type.
pub fn world_name(name: &str) -> String {
    let n = name.trim().to_lowercase();
    match n.strip_prefix("minecraft:").unwrap_or(&n) {
        "world" | "overworld" => "world".into(),
        "world_nether" | "nether" | "the_nether" => "world_nether".into(),
        "world_the_end" | "world_end" | "the_end" | "end" => "world_the_end".into(),
        other => other.to_owned(),
    }
}

/// Pumpkin dimension key for a script-facing world name.
pub fn world_dimension(name: &str) -> String {
    match world_name(name).as_str() {
        "world" => "minecraft:overworld".into(),
        "world_nether" => "minecraft:the_nether".into(),
        "world_the_end" => "minecraft:the_end".into(),
        other => format!("minecraft:{other}"),
    }
}

pub fn parse_number(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() || s.len() > 40 || !s.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+')) {
        return None;
    }
    if !s.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// Numbers the way Skript prints them: whole numbers without decimals, others with up to two.
pub fn fmt_num(n: f64) -> String {
    if !n.is_finite() {
        return "0".into();
    }
    if (n - n.round()).abs() < 1e-9 && n.abs() < 1e15 {
        return format!("{}", n.round() as i64);
    }
    let s = format!("{n:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

const UNITS: [(&str, f64); 8] = [
    ("year", 31_536_000.0),
    ("month", 2_592_000.0),
    ("week", 604_800.0),
    ("day", 86_400.0),
    ("hour", 3600.0),
    ("minute", 60.0),
    ("second", 1.0),
    ("tick", 0.05),
];

pub fn unit_seconds(word: &str) -> Option<f64> {
    let w = word.to_lowercase();
    let w = w.strip_suffix('s').unwrap_or(&w);
    let w = match w {
        "sec" => "second",
        "min" => "minute",
        "hr" => "hour",
        o => o,
    };
    UNITS.iter().find(|(n, _)| *n == w).map(|(_, s)| *s)
}

/// `5 seconds`, `1 minute and 30 seconds`, `a tick`, `2.5 hours` → seconds.
pub fn parse_span(s: &str) -> Option<f64> {
    let lower = s.trim().to_lowercase();
    let mut words: Vec<&str> = lower.split([' ', ',']).filter(|w| !w.is_empty() && *w != "and").collect();
    if matches!(words.first(), Some(&"real" | &"minecraft" | &"mc")) {
        words.remove(0);
    }
    if words.is_empty() || words.len() % 2 != 0 {
        return None;
    }
    let mut total = 0.0;
    for pair in words.chunks(2) {
        let n = match pair[0] {
            "a" | "an" | "one" => 1.0,
            w => parse_number(w)?,
        };
        if n < 0.0 {
            return None;
        }
        total += n * unit_seconds(pair[1])?;
    }
    Some(total)
}

pub fn fmt_span(secs: f64) -> String {
    let mut left = secs.max(0.0);
    if left < 1.0 {
        let ticks = (left * 20.0).round();
        if ticks >= 1.0 && left > 0.0 && (left * 20.0 - ticks).abs() < 1e-6 && ticks < 20.0 {
            return format!("{} tick{}", ticks as i64, if ticks == 1.0 { "" } else { "s" });
        }
        return format!("{} seconds", fmt_num(left));
    }
    let mut parts = Vec::new();
    for (name, size) in &UNITS[3..7] {
        let whole = (left / size).floor();
        let n = if *name == "second" { left } else { whole };
        if *name == "second" {
            if n > 1e-9 {
                let shown = fmt_num(n);
                parts.push(format!("{shown} second{}", if shown == "1" { "" } else { "s" }));
            }
        } else if whole >= 1.0 {
            parts.push(format!("{} {name}{}", whole as i64, if whole == 1.0 { "" } else { "s" }));
            left -= whole * size;
        }
    }
    join_and(&parts)
}

pub fn join_and(parts: &[String]) -> String {
    match parts.len() {
        0 => String::new(),
        1 => parts[0].clone(),
        n => format!("{} and {}", parts[..n - 1].join(", "), parts[n - 1]),
    }
}

/// Civil date from unix seconds: (year, month, day, hour, minute, second).
pub fn civil(unix: f64) -> (i64, u32, u32, u32, u32, u32) {
    let secs = unix.floor() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, (rem / 3600) as u32, (rem / 60 % 60) as u32, (rem % 60) as u32)
}

/// Java-style date pattern (`yyyy-MM-dd HH:mm:ss`), already shifted to local time by the caller.
pub fn fmt_date(unix: f64, pattern: &str) -> String {
    let (y, mo, d, h, mi, s) = civil(unix);
    let mut out = String::new();
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let mut n = 1;
        while i + n < chars.len() && chars[i + n] == c {
            n += 1;
        }
        let pad = |v: i64| if n >= 2 { format!("{v:02}") } else { v.to_string() };
        match c {
            'y' => out.push_str(&if n == 2 { format!("{:02}", y % 100) } else { y.to_string() }),
            'M' => out.push_str(&pad(i64::from(mo))),
            'd' => out.push_str(&pad(i64::from(d))),
            'H' => out.push_str(&pad(i64::from(h))),
            'h' => out.push_str(&pad(i64::from(if h % 12 == 0 { 12 } else { h % 12 }))),
            'm' => out.push_str(&pad(i64::from(mi))),
            's' => out.push_str(&pad(i64::from(s))),
            'a' => out.push_str(if h < 12 { "AM" } else { "PM" }),
            '\'' => {
                // quoted literal text
                let mut j = i + 1;
                while j < chars.len() && chars[j] != '\'' {
                    out.push(chars[j]);
                    j += 1;
                }
                i = j + 1;
                continue;
            }
            other => (0..n).for_each(|_| out.push(other)),
        }
        i += n;
    }
    out
}

pub const DATE_FORMAT: &str = "yyyy-MM-dd HH:mm:ss";

/// How a value reads inside a text (`"%player% has %{coins}%"`).
pub fn to_text(v: &Value) -> String {
    match v {
        Value::None => "<none>".into(),
        Value::Bool(b) => b.to_string(),
        Value::Num(n) => fmt_num(*n),
        Value::Text(t) => t.clone(),
        Value::Player(p) => p.name.clone(),
        Value::Console => "console".into(),
        Value::Loc(l) | Value::Block(l) => format!("x: {}, y: {}, z: {} in {}", fmt_num(l.x), fmt_num(l.y), fmt_num(l.z), l.world),
        Value::Item(i) => {
            let name = i.id.replace('_', " ");
            if i.count == 1 { name } else { format!("{} {name}", i.count) }
        }
        Value::World(w) => w.clone(),
        Value::Entity(e) => e.kind.replace('_', " "),
        Value::Span(s) => fmt_span(*s),
        Value::Date(d) => fmt_date(*d, DATE_FORMAT),
        Value::List(l) => join_and(&l.iter().map(to_text).collect::<Vec<_>>()),
    }
}

/// Equality the way a script author expects it (`"Alice" is player`, `2 is "2"`, `STONE is stone`).
pub fn loose_eq(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (None, None) | (Console, Console) => true,
        (Bool(x), Bool(y)) => x == y,
        (Player(x), Player(y)) => x.uuid == y.uuid,
        (Player(p), Text(t)) | (Text(t), Player(p)) => p.name.eq_ignore_ascii_case(t) || p.uuid.eq_ignore_ascii_case(t),
        (Entity(x), Entity(y)) => x.id == y.id,
        // `attacker is a zombie`
        (Entity(e), Text(t)) | (Text(t), Entity(e)) => {
            let t = t.trim().to_lowercase();
            let t = t.strip_prefix("an ").or_else(|| t.strip_prefix("a ")).unwrap_or(&t);
            e.kind == norm_id(t) || e.kind == norm_id(t.trim_end_matches('s'))
        }
        (Entity(e), Item(i)) | (Item(i), Entity(e)) => e.kind == i.id,
        (Item(x), Item(y)) => x.id == y.id,
        (Item(i), Text(t)) | (Text(t), Item(i)) => i.id == norm_id(t),
        (World(x), World(y)) => world_name(x) == world_name(y),
        (World(w), Text(t)) | (Text(t), World(w)) => world_name(w) == world_name(t),
        (Loc(x), Loc(y)) | (Block(x), Block(y)) => x.world == y.world && (x.x - y.x).abs() < 1e-6 && (x.y - y.y).abs() < 1e-6 && (x.z - y.z).abs() < 1e-6,
        (Text(x), Text(y)) => x.to_lowercase() == y.to_lowercase(),
        (Bool(x), Text(t)) | (Text(t), Bool(x)) => t.eq_ignore_ascii_case(if *x { "true" } else { "false" }),
        (List(x), List(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| loose_eq(p, q)),
        (List(x), y) | (y, List(x)) => x.len() == 1 && loose_eq(&x[0], y),
        _ => match (a.num(), b.num()) {
            (Some(x), Some(y)) => (x - y).abs() < 1e-9,
            _ => false,
        },
    }
}

pub fn loose_cmp(a: &Value, b: &Value) -> Option<Ordering> {
    match (a.num(), b.num()) {
        (Some(x), Some(y)) => x.partial_cmp(&y),
        _ => match (a, b) {
            (Value::Text(x), Value::Text(y)) => Some(x.to_lowercase().cmp(&y.to_lowercase())),
            _ => None,
        },
    }
}

/// Skript colour tags and `§` to `&` codes; `&` codes stay as they are.
pub fn colorize(text: &str) -> String {
    const TAGS: [(&str, char); 30] = [
        ("black", '0'), ("dark blue", '1'), ("dark green", '2'), ("dark aqua", '3'), ("dark cyan", '3'), ("dark red", '4'),
        ("dark purple", '5'), ("gold", '6'), ("orange", '6'), ("grey", '7'), ("gray", '7'), ("light grey", '7'), ("light gray", '7'), ("dark grey", '8'),
        ("dark gray", '8'), ("blue", '9'), ("green", 'a'), ("lime", 'a'), ("aqua", 'b'), ("cyan", 'b'), ("light blue", 'b'), ("red", 'c'),
        ("light red", 'c'), ("pink", 'd'), ("magenta", 'd'), ("light purple", 'd'), ("yellow", 'e'), ("white", 'f'), ("bold", 'l'), ("reset", 'r'),
    ];
    if !text.contains(['<', '§']) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let code = tail.find('>').filter(|end| *end <= 14).and_then(|end| {
            let tag = tail[..end].to_lowercase();
            let code = match tag.as_str() {
                "italic" | "italics" | "o" => Some('o'),
                "underline" | "underlined" | "u" => Some('n'),
                "strikethrough" | "strike" | "s" => Some('m'),
                "magic" | "obfuscated" => Some('k'),
                "b" => Some('l'),
                "r" => Some('r'),
                t => TAGS.iter().find(|(n, _)| *n == t).map(|(_, c)| *c),
            };
            code.map(|c| (c, end))
        });
        match code {
            Some((c, end)) => {
                out.push('&');
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('<');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out.replace('§', "&")
}

/// Remove `&x` / `§x` colour codes.
pub fn uncolor(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if (c == '&' || c == '§') && it.peek().is_some_and(|n| "0123456789abcdefklmnorABCDEFKLMNOR".contains(*n)) {
            it.next();
        } else {
            out.push(c);
        }
    }
    out
}

/// Minecraft time of day in ticks from `18:00`, `6:30`, `day`, `night`, `noon`, `midnight`.
pub fn parse_time_of_day(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase();
    match s.as_str() {
        "day" | "morning" => return Some(1000),
        "noon" | "midday" => return Some(6000),
        "sunset" | "dusk" | "evening" => return Some(12_000),
        "night" => return Some(13_000),
        "midnight" => return Some(18_000),
        "sunrise" | "dawn" => return Some(23_000),
        _ => {}
    }
    let (clock, pm) = match s.strip_suffix("pm").map(str::trim) {
        Some(c) => (c, Some(true)),
        None => match s.strip_suffix("am").map(str::trim) {
            Some(c) => (c, Some(false)),
            None => (s.as_str(), None),
        },
    };
    let (h, m) = clock.split_once(':').unwrap_or((clock, "0"));
    if pm.is_none() && !clock.contains(':') {
        return None;
    }
    let mut h: u64 = h.trim().parse().ok()?;
    let m: u64 = m.trim().parse().ok()?;
    if let Some(pm) = pm {
        if h == 12 {
            h = 0;
        }
        if pm {
            h += 12;
        }
    }
    if h > 24 || m > 59 {
        return None;
    }
    Some((h * 1000 + m * 1000 / 60 + 18_000) % 24_000)
}

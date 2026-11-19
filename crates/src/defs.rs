//! What the server owner wrote: the crates, their keys and rewards, and the draw. No server
//! needed here.
//!
//! A crate is a block (set with `/crates set <crate>` while looking at it). A player opens it
//! by right clicking it with its key, which is used up; a reward is drawn by weight while a
//! window spins through the rewards. A left click shows the rewards and their chances.
use serde::{Deserialize, Serialize};

pub const NS: &str = "crates";
pub const MAX_CRATES: usize = 200;
pub const MAX_REWARDS: usize = 54;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// Ticks the window spins before it stops on the reward (0 = no spinning).
    pub spin_ticks: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), spin_ticks: 60 }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.spin_ticks = self.spin_ticks.min(200);
        self
    }

    pub fn thai(&self) -> bool {
        self.language == "th"
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub th: String,
    pub en: String,
}

impl Note {
    pub fn new(place: &str, th: impl AsRef<str>, en: impl AsRef<str>) -> Note {
        Note { th: format!("{place}: {}", th.as_ref()), en: format!("{place}: {}", en.as_ref()) }
    }

    pub fn text(&self, thai: bool) -> &str {
        if thai { &self.th } else { &self.en }
    }
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// `minecraft:<item>` of the game or an id of CustomItems.
pub fn item_ref(text: &str) -> Option<String> {
    let t = text.trim().to_ascii_lowercase();
    match t.strip_prefix("minecraft:") {
        Some(name) => valid_id(name).then_some(t),
        None => valid_id(&t).then_some(t),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reward {
    pub name: String,
    /// What is shown in the windows; also given when `give` is true.
    pub item: Option<String>,
    pub count: u32,
    pub give: bool,
    pub money: f64,
    /// Run as the console; `{player}` is the player's name.
    pub commands: Vec<String>,
    pub weight: f64,
    /// Everyone hears of it.
    pub broadcast: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Crate {
    pub id: String,
    pub name: String,
    /// The game item a key is made of.
    pub key_item: String,
    /// An item model (`namespace:path`) the key looks like.
    pub key_model: Option<String>,
    /// A model of the Models plugin drawn over the crate block.
    pub model: Option<String>,
    /// Its animation when the crate opens.
    pub open_animation: String,
    pub rewards: Vec<Reward>,
}

impl Crate {
    pub fn total_weight(&self) -> f64 {
        self.rewards.iter().map(|r| r.weight).sum()
    }

    /// The chance of a reward, 0 to 1.
    pub fn chance(&self, at: usize) -> f64 {
        let total = self.total_weight();
        if total <= 0.0 { 0.0 } else { self.rewards.get(at).map_or(0.0, |r| r.weight / total) }
    }

    /// The reward a number in [0, 1) draws.
    pub fn draw(&self, roll: f64) -> Option<usize> {
        let total = self.total_weight();
        if total <= 0.0 {
            return None;
        }
        let mut left = roll.clamp(0.0, 0.999_999) * total;
        for (i, r) in self.rewards.iter().enumerate() {
            if left < r.weight {
                return Some(i);
            }
            left -= r.weight;
        }
        self.rewards.len().checked_sub(1)
    }
}

fn number(v: Option<&toml::Value>) -> Option<f64> {
    match v? {
        toml::Value::Integer(i) => Some(*i as f64),
        toml::Value::Float(f) if f.is_finite() => Some(*f),
        _ => None,
    }
}

fn valid_item_model(text: &str) -> bool {
    let fine = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-./".contains(&b));
    text.len() <= 200 && text.split_once(':').is_some_and(|(ns, path)| fine(ns) && fine(path))
}

const KNOWN: [&str; 7] = ["name", "key-item", "key-model", "model", "open-animation", "rewards", "reward"];
const KNOWN_REWARD: [&str; 8] = ["name", "item", "count", "give", "money", "commands", "weight", "broadcast"];

/// The crates of one file.
pub fn parse_crates(file: &str, text: &str, out: &mut Vec<Note>) -> Vec<Crate> {
    let table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            let why = e.to_string().lines().next().unwrap_or_default().to_owned();
            out.push(Note::new(file, format!("อ่านไม่ได้: {why}"), format!("cannot be read: {why}")));
            return Vec::new();
        }
    };
    let mut list = Vec::new();
    for (id, v) in &table {
        let place = format!("{file} [{id}]");
        let Some(t) = v.as_table() else { continue };
        if !valid_id(id) {
            out.push(Note::new(&place, "ชื่อใช้ได้แค่ a-z, 0-9 และ _", "a name may only have a-z, 0-9 and _"));
            continue;
        }
        for key in t.keys().filter(|k| !KNOWN.contains(&k.as_str())) {
            out.push(Note::new(&place, format!("ไม่รู้จัก {key} จึงข้าม"), format!("{key} is not known and is skipped")));
        }
        let text_of = |key: &str| t.get(key).and_then(toml::Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        let key_item = match text_of("key-item").map(|k| k.to_ascii_lowercase()) {
            None => "minecraft:tripwire_hook".to_owned(),
            Some(k) if k.starts_with("minecraft:") && valid_id(&k["minecraft:".len()..]) => k,
            Some(_) => {
                out.push(Note::new(&place, "key-item ต้องเป็น minecraft:<ไอเท็ม>", "key-item must be minecraft:<item>"));
                "minecraft:tripwire_hook".to_owned()
            }
        };
        let key_model = text_of("key-model").map(|m| m.to_ascii_lowercase()).filter(|m| {
            let ok = valid_item_model(m);
            if !ok {
                out.push(Note::new(&place, "key-model ต้องเป็น namespace:path", "key-model must be namespace:path"));
            }
            ok
        });
        let raw = t.get("rewards").or_else(|| t.get("reward")).and_then(toml::Value::as_array).cloned().unwrap_or_default();
        let mut rewards = Vec::new();
        for (n, r) in raw.iter().enumerate().take(MAX_REWARDS) {
            let here = format!("{place} reward {}", n + 1);
            let Some(r) = r.as_table() else { continue };
            for key in r.keys().filter(|k| !KNOWN_REWARD.contains(&k.as_str())) {
                out.push(Note::new(&here, format!("ไม่รู้จัก {key} จึงข้าม"), format!("{key} is not known and is skipped")));
            }
            let item = match r.get("item").and_then(toml::Value::as_str) {
                Some(i) => match item_ref(i) {
                    Some(i) => Some(i),
                    None => {
                        out.push(Note::new(&here, "item ต้องเป็น id ของ CustomItems หรือ minecraft:<ไอเท็ม>", "item must be a CustomItems id or minecraft:<item>"));
                        continue;
                    }
                },
                None => None,
            };
            let commands: Vec<String> = match r.get("commands") {
                Some(toml::Value::String(one)) => vec![one.trim_start_matches('/').to_owned()],
                Some(toml::Value::Array(many)) => many.iter().filter_map(toml::Value::as_str).map(|c| c.trim_start_matches('/').to_owned()).collect(),
                _ => Vec::new(),
            };
            let money = number(r.get("money")).unwrap_or(0.0).max(0.0);
            if item.is_none() && commands.is_empty() && money <= 0.0 {
                out.push(Note::new(&here, "ต้องมี item, money หรือ commands", "needs an item, money or commands"));
                continue;
            }
            let weight = number(r.get("weight")).unwrap_or(1.0);
            if weight <= 0.0 {
                out.push(Note::new(&here, "weight ต้องมากกว่า 0", "weight must be above 0"));
                continue;
            }
            let count = number(r.get("count")).unwrap_or(1.0).clamp(1.0, 2304.0) as u32;
            let name = r.get("name").and_then(toml::Value::as_str).map(str::to_owned).unwrap_or_else(|| match &item {
                Some(i) => format!("{} x{count}", i.trim_start_matches("minecraft:")),
                None if money > 0.0 => format!("{money}"),
                None => "?".into(),
            });
            rewards.push(Reward { name, give: r.get("give").and_then(toml::Value::as_bool).unwrap_or(item.is_some()), item, count, money, commands, weight, broadcast: r.get("broadcast").and_then(toml::Value::as_bool).unwrap_or(false) });
        }
        if rewards.is_empty() {
            out.push(Note::new(&place, "ยังไม่มีรางวัล ([[<ชื่อ>.rewards]])", "no rewards yet ([[<id>.rewards]])"));
            continue;
        }
        list.push(Crate {
            id: id.clone(),
            name: text_of("name").unwrap_or_else(|| id.clone()),
            key_item,
            key_model,
            model: text_of("model").map(|m| m.to_ascii_lowercase()),
            open_animation: text_of("open-animation").unwrap_or_else(|| "open".into()),
            rewards,
        });
    }
    list
}

pub fn merge_crates(all: Vec<Crate>, out: &mut Vec<Note>) -> Vec<Crate> {
    let mut list: Vec<Crate> = Vec::new();
    for c in all {
        if list.iter().any(|o| o.id == c.id) {
            out.push(Note::new(&c.id, "ชื่อซ้ำกับไฟล์อื่น ใช้อันแรก", "the name is used in another file too; the first counts"));
        } else if list.len() >= MAX_CRATES {
            out.push(Note::new(&c.id, format!("มีได้ไม่เกิน {MAX_CRATES} กล่อง"), format!("at most {MAX_CRATES} crates")));
        } else {
            list.push(c);
        }
    }
    list
}

/// A crate block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub crate_id: String,
    pub world: String,
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// The ticks between the steps of a spin of `total` ticks: fast at first, slower at the end.
pub fn spin_steps(total: u32) -> Vec<u32> {
    let mut steps = Vec::new();
    let mut used = 0;
    let mut gap = 1;
    while used + gap <= total {
        steps.push(gap);
        used += gap;
        if used > total / 2 {
            gap += 1;
        }
    }
    steps
}

/// xorshift: a number in [0, 1).
pub fn next_random(state: &mut u64) -> f64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    (x >> 11) as f64 / (1u64 << 53) as f64
}

pub const EXAMPLE: &str = r#"# Crates: one [table] per crate, with [[<crate>.rewards]] for each reward.
# /crates set <crate> while looking at a block makes it that crate; /crates key <player> <crate> [n]
# gives keys. A right click with a key opens it; a left click shows the rewards.
#
# key-item  = the game item keys are made of (minecraft:tripwire_hook)
# key-model = an item model the key looks like (namespace:path)
# model     = a model of the Models plugin drawn on the crate; open-animation plays when it opens
#
# A reward: name, item (CustomItems id or minecraft:<item>) and count, money (Basics),
# commands (as the console, {player} = the player), weight (how likely), broadcast (tell everyone).
# give = false shows the item in the windows without giving it.

[basic]
name = "&aBasic Crate"

[[basic.rewards]]
item = "minecraft:diamond"
count = 2
weight = 10

[[basic.rewards]]
name = "&6100 coins"
item = "minecraft:gold_ingot"
give = false
money = 100
weight = 30

[[basic.rewards]]
name = "&dA golden apple"
item = "minecraft:enchanted_golden_apple"
weight = 1
broadcast = true
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_reads_cleanly() {
        let mut notes = Vec::new();
        let list = parse_crates("example.toml", EXAMPLE, &mut notes);
        assert!(notes.is_empty(), "{notes:?}");
        let c = &list[0];
        assert_eq!(c.rewards.len(), 3);
        assert!((c.chance(0) - 10.0 / 41.0).abs() < 1e-9);
        assert!(!c.rewards[1].give);
        assert!(c.rewards[0].give);
        assert_eq!(c.key_item, "minecraft:tripwire_hook");
    }

    #[test]
    fn bad_rewards_are_left_out_with_a_note() {
        let mut notes = Vec::new();
        let list = parse_crates("x.toml", "[a]\n[[a.rewards]]\nweight = 0\nitem = \"minecraft:dirt\"\n[[a.rewards]]\nname = \"nothing\"\n[[a.rewards]]\nitem = \"Bad Item\"\n[b]\n[[b.rewards]]\nmoney = 5\n", &mut notes);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "b");
        assert_eq!(notes.len(), 4, "{notes:?}");
    }

    #[test]
    fn draws_follow_the_weights() {
        let mut notes = Vec::new();
        let c = &parse_crates("e.toml", EXAMPLE, &mut notes)[0];
        assert_eq!(c.draw(0.0), Some(0));
        assert_eq!(c.draw(10.0 / 41.0 - 1e-6), Some(0));
        assert_eq!(c.draw(10.0 / 41.0 + 1e-6), Some(1));
        assert_eq!(c.draw(0.9999), Some(2));
        let mut seed = 7u64;
        let mut seen = [0u32; 3];
        for _ in 0..41_000 {
            seen[c.draw(next_random(&mut seed)).unwrap()] += 1;
        }
        assert!((9_000..11_000).contains(&seen[0]), "{seen:?}");
        assert!((500..1_500).contains(&seen[2]), "{seen:?}");
    }

    #[test]
    fn a_spin_slows_down_and_fits() {
        let steps = spin_steps(60);
        assert!(steps.iter().sum::<u32>() <= 60);
        assert_eq!(steps[0], 1);
        assert!(steps.last().unwrap() > &steps[0]);
        assert!(spin_steps(0).is_empty());
    }
}

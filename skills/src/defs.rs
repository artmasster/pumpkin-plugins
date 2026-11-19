//! What the server owner wrote: the skills, the experience each deed gives and the perk each
//! level brings. No server needed here.
//!
//! A skill grows with deeds (breaking ores for Mining, logs for Woodcutting, kills for Combat …).
//! Its perk grows with its level: a chance of a second drop from a block, or more damage.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[path = "../../common/src/names.rs"]
mod names;

pub const NS: &str = "skills";
pub const MAX_SKILLS: usize = 50;
pub const KINDS: [&str; 8] = ["break", "place", "kill", "fish", "craft", "smelt", "eat", "tame"];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    pub xp_base: f64,
    pub xp_growth: f64,
    /// Blocks players placed that are remembered, so breaking them again gives nothing.
    pub placed_memory: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), xp_base: 50.0, xp_growth: 1.08, placed_memory: 50_000 }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        let d = Config::default();
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.xp_base = if self.xp_base.is_finite() { self.xp_base.clamp(1.0, 1e9) } else { d.xp_base };
        self.xp_growth = if self.xp_growth.is_finite() { self.xp_growth.clamp(1.0, 3.0) } else { d.xp_growth };
        self.placed_memory = self.placed_memory.min(1_000_000);
        self
    }

    pub fn thai(&self) -> bool {
        self.language == "th"
    }

    pub fn need(&self, level: u32) -> f64 {
        self.xp_base * self.xp_growth.powi(level.min(1000) as i32)
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Perk {
    None,
    /// A chance (per level, up to the cap) of a second drop from a block that gave experience.
    DoubleDrop,
    /// More damage (per level, up to the cap) by a player's own hand.
    Damage,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub max_level: u32,
    /// (kind, pattern, xp)
    pub xp: Vec<(String, String, f64)>,
    pub perk: Perk,
    pub perk_per_level: f64,
    pub perk_max: f64,
    /// What a block drops, for the double drop (`diamond_ore` = `minecraft:diamond`); a block
    /// not named here drops itself.
    pub drops: BTreeMap<String, (String, u32)>,
}

impl Skill {
    /// Experience for a deed: an exact name first, then the longest pattern.
    pub fn xp_for(&self, kind: &str, target: &str) -> Option<f64> {
        let rank = |p: &str| if p.contains('*') { p.len() } else { usize::MAX };
        self.xp.iter().filter(|(k, p, _)| k == kind && names::matches(p, target)).max_by_key(|(_, p, _)| rank(p)).map(|(_, _, xp)| *xp)
    }

    /// The perk at a level: a chance (0 to 1) or a bonus (0.1 = +10 %).
    pub fn perk_at(&self, level: u32) -> f64 {
        (self.perk_per_level * f64::from(level)).clamp(0.0, self.perk_max)
    }

    /// What one more drop of this block is.
    pub fn drop_of(&self, block: &str) -> (String, u32) {
        self.drops.get(block).cloned().unwrap_or_else(|| (default_drop(block), 1))
    }
}

pub use names::default_drop;

fn number(v: &toml::Value) -> Option<f64> {
    match v {
        toml::Value::Integer(i) => Some(*i as f64),
        toml::Value::Float(f) if f.is_finite() => Some(*f),
        _ => None,
    }
}

const KNOWN: [&str; 8] = ["name", "icon", "max-level", "perk", "perk-per-level", "perk-max", "drops", "xp"];

/// The skills of one file.
pub fn parse_skills(file: &str, text: &str, out: &mut Vec<Note>) -> Vec<Skill> {
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
        let mut xp = Vec::new();
        for (key, value) in t {
            if KNOWN.contains(&key.as_str()) {
                continue;
            }
            if !KINDS.contains(&key.as_str()) {
                out.push(Note::new(&place, format!("ไม่รู้จัก {key} (ใช้ {})", KINDS.join(", ")), format!("{key} is not known (use {})", KINDS.join(", "))));
                continue;
            }
            let Some(entries) = value.as_table() else { continue };
            for (pattern, amount) in entries {
                let pattern = pattern.trim().to_ascii_lowercase();
                let pattern = pattern.strip_prefix("minecraft:").map(str::to_owned).unwrap_or(pattern);
                match number(amount) {
                    Some(n) if n >= 0.0 => xp.push((key.clone(), pattern, n)),
                    _ => out.push(Note::new(&place, format!("{key}.{pattern} ต้องเป็นตัวเลขไม่ติดลบ"), format!("{key}.{pattern} must be a number, not below 0"))),
                }
            }
        }
        if xp.is_empty() {
            out.push(Note::new(&place, "ยังไม่ให้ xp กับอะไรเลย", "gives experience for nothing yet"));
        }
        let perk = match t.get("perk").and_then(toml::Value::as_str).unwrap_or("none") {
            "double-drop" => Perk::DoubleDrop,
            "damage" => Perk::Damage,
            "none" => Perk::None,
            other => {
                out.push(Note::new(&place, format!("perk {other} ไม่รู้จัก (double-drop, damage, none)"), format!("perk {other} is not known (double-drop, damage, none)")));
                Perk::None
            }
        };
        let mut drops = BTreeMap::new();
        if let Some(d) = t.get("drops").and_then(toml::Value::as_table) {
            for (block, item) in d {
                let (name, count) = match item {
                    toml::Value::String(s) => (s.clone(), 1),
                    toml::Value::Array(a) if a.len() == 2 => (a[0].as_str().unwrap_or_default().to_owned(), number(&a[1]).unwrap_or(1.0).clamp(1.0, 64.0) as u32),
                    _ => continue,
                };
                let name = if name.contains(':') { name.to_ascii_lowercase() } else { format!("minecraft:{}", name.to_ascii_lowercase()) };
                drops.insert(block.to_ascii_lowercase(), (name, count));
            }
        }
        let text_of = |key: &str| t.get(key).and_then(toml::Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        list.push(Skill {
            id: id.clone(),
            name: text_of("name").unwrap_or_else(|| id.clone()),
            icon: text_of("icon").map_or_else(|| "minecraft:book".to_owned(), |i| if i.contains(':') { i.to_ascii_lowercase() } else { format!("minecraft:{}", i.to_ascii_lowercase()) }),
            max_level: t.get("max-level").and_then(number).unwrap_or(100.0).clamp(1.0, 1000.0) as u32,
            xp,
            perk,
            perk_per_level: t.get("perk-per-level").and_then(number).unwrap_or(0.0).max(0.0),
            perk_max: t.get("perk-max").and_then(number).unwrap_or(1.0).clamp(0.0, 10.0),
            drops,
        });
    }
    list
}

pub fn merge_skills(all: Vec<Skill>, out: &mut Vec<Note>) -> Vec<Skill> {
    let mut list: Vec<Skill> = Vec::new();
    for s in all {
        if list.iter().any(|o| o.id == s.id) {
            out.push(Note::new(&s.id, "ชื่อซ้ำกับไฟล์อื่น ใช้อันแรก", "the name is used in another file too; the first counts"));
        } else if list.len() < MAX_SKILLS {
            list.push(s);
        }
    }
    list
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Progress {
    pub level: u32,
    pub xp: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Learner {
    pub name: String,
    pub skills: BTreeMap<String, Progress>,
}

impl Learner {
    /// All levels added up.
    pub fn power(&self) -> u32 {
        self.skills.values().map(|p| p.level).sum()
    }
}

/// Add experience; returns how many levels were gained.
pub fn add_xp(cfg: &Config, p: &mut Progress, xp: f64, max_level: u32) -> u32 {
    if !xp.is_finite() || xp <= 0.0 || p.level >= max_level {
        return 0;
    }
    let mut gained = 0;
    p.xp += xp;
    while p.level < max_level && p.xp >= cfg.need(p.level) {
        p.xp -= cfg.need(p.level);
        p.level += 1;
        gained += 1;
    }
    if p.level >= max_level {
        p.xp = 0.0;
    }
    gained
}

pub const EXAMPLE: &str = r#"# Skills: one [table] per skill, then a table for each kind of deed that teaches it:
#   [<skill>.break] [<skill>.place] [<skill>.kill] [<skill>.fish] [<skill>.craft] [<skill>.smelt] [<skill>.eat] [<skill>.tame]
# Each line is  name = xp  (the game's name, or a pattern like "*_ore", "*_log", "*").
# perk: double-drop (a block that gave experience may drop twice) or damage (more damage by hand);
# perk-per-level and perk-max: 0.001 = 0.1 % a level, at most perk-max (0.5 = 50 %).
# drops: what a block drops for the double drop when it is not the block itself (ores are known).

[mining]
name = "&7Mining"
icon = "minecraft:iron_pickaxe"
perk = "double-drop"
perk-per-level = 0.005
perk-max = 0.5

[mining.break]
stone = 1
deepslate = 2
"*_ore" = 10
diamond_ore = 40
deepslate_diamond_ore = 40

[woodcutting]
name = "&6Woodcutting"
icon = "minecraft:iron_axe"
perk = "double-drop"
perk-per-level = 0.005
perk-max = 0.5

[woodcutting.break]
"*_log" = 5
"*_stem" = 5

[combat]
name = "&cCombat"
icon = "minecraft:iron_sword"
perk = "damage"
perk-per-level = 0.003
perk-max = 0.3

[combat.kill]
"*" = 5
zombie = 10
skeleton = 10
creeper = 15

[fishing]
name = "&bFishing"
icon = "minecraft:fishing_rod"

[fishing.fish]
"*" = 20
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_reads_cleanly() {
        let mut notes = Vec::new();
        let list = parse_skills("e.toml", EXAMPLE, &mut notes);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(list.len(), 4);
        let mining = list.iter().find(|s| s.id == "mining").unwrap();
        assert_eq!(mining.xp_for("break", "diamond_ore"), Some(40.0));
        assert_eq!(mining.xp_for("break", "iron_ore"), Some(10.0));
        assert_eq!(mining.xp_for("break", "dirt"), None);
        assert_eq!(mining.perk, Perk::DoubleDrop);
        assert!((mining.perk_at(50) - 0.25).abs() < 1e-9);
        assert_eq!(mining.perk_at(1000), 0.5);
    }

    #[test]
    fn blocks_drop_what_the_game_drops() {
        assert_eq!(default_drop("deepslate_diamond_ore"), "minecraft:diamond");
        assert_eq!(default_drop("oak_log"), "minecraft:oak_log");
        assert_eq!(default_drop("stone"), "minecraft:cobblestone");
        let mut notes = Vec::new();
        let s = &parse_skills("x.toml", "[a]\ndrops = { glowstone = [\"glowstone_dust\", 3] }\n[a.break]\nglowstone = 1\n", &mut notes)[0];
        assert_eq!(s.drop_of("glowstone"), ("minecraft:glowstone_dust".to_owned(), 3));
    }

    #[test]
    fn levels_and_power() {
        let cfg = Config::default();
        let mut p = Progress::default();
        assert_eq!(add_xp(&cfg, &mut p, cfg.need(0) + cfg.need(1) + 1e-6, 100), 2);
        let mut l = Learner::default();
        l.skills.insert("a".into(), p);
        l.skills.insert("b".into(), Progress { level: 5, xp: 0.0 });
        assert_eq!(l.power(), 7);
    }
}

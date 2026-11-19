//! What the server owner wrote: the jobs and what each pays, and how levels grow. No server
//! needed here.
//!
//! A job pays money (through Basics) and experience for things a player does: breaking or
//! placing blocks, killing creatures, fishing, crafting, smelting, eating, taming. Experience
//! raises the job's level, and each level pays a little more.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[path = "../../common/src/names.rs"]
mod names;

pub const NS: &str = "jobs";
pub const MAX_JOBS: usize = 100;
pub const KINDS: [&str; 8] = ["break", "place", "kill", "fish", "craft", "smelt", "eat", "tame"];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// Jobs a player may have at once.
    pub max_jobs: u32,
    /// Seconds between two payments (money is added up in between).
    pub pay_every: u32,
    /// Each level pays this much more (0.02 = 2 % a level).
    pub level_bonus: f64,
    /// Experience for the first level; each next level needs `xp-growth` times more.
    pub xp_base: f64,
    pub xp_growth: f64,
    /// Blocks players placed that are remembered, so breaking them again pays nothing.
    pub placed_memory: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), max_jobs: 3, pay_every: 5, level_bonus: 0.02, xp_base: 100.0, xp_growth: 1.1, placed_memory: 50_000 }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        let d = Config::default();
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.max_jobs = self.max_jobs.clamp(1, 20);
        self.pay_every = self.pay_every.clamp(1, 300);
        self.level_bonus = if self.level_bonus.is_finite() { self.level_bonus.clamp(0.0, 1.0) } else { d.level_bonus };
        self.xp_base = if self.xp_base.is_finite() { self.xp_base.clamp(1.0, 1e9) } else { d.xp_base };
        self.xp_growth = if self.xp_growth.is_finite() { self.xp_growth.clamp(1.0, 3.0) } else { d.xp_growth };
        self.placed_memory = self.placed_memory.min(1_000_000);
        self
    }

    pub fn thai(&self) -> bool {
        self.language == "th"
    }

    /// Experience from `level` to the next.
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

#[derive(Clone, Debug, PartialEq)]
pub struct Pay {
    pub kind: String,
    /// `stone`, `*_ore`, `oak_*` or `*`.
    pub pattern: String,
    pub money: f64,
    pub xp: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub description: Vec<String>,
    pub max_level: u32,
    pub pays: Vec<Pay>,
    pub permission: Option<String>,
}

impl Job {
    /// What this job pays for one deed: an exact name first, then the longest pattern.
    pub fn pay_for(&self, kind: &str, target: &str) -> Option<&Pay> {
        let mut best: Option<&Pay> = None;
        for p in self.pays.iter().filter(|p| p.kind == kind && names::matches(&p.pattern, target)) {
            let better = match best {
                None => true,
                Some(b) => rank(&p.pattern) > rank(&b.pattern),
            };
            if better {
                best = Some(p);
            }
        }
        best
    }
}

fn rank(pattern: &str) -> usize {
    if pattern.contains('*') { pattern.len() } else { usize::MAX }
}

fn number(v: &toml::Value) -> Option<f64> {
    match v {
        toml::Value::Integer(i) => Some(*i as f64),
        toml::Value::Float(f) if f.is_finite() => Some(*f),
        _ => None,
    }
}

const KNOWN: [&str; 5] = ["name", "icon", "description", "max-level", "permission"];

/// The jobs of one file.
pub fn parse_jobs(file: &str, text: &str, out: &mut Vec<Note>) -> Vec<Job> {
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
        let mut pays = Vec::new();
        for (key, value) in t {
            if KNOWN.contains(&key.as_str()) {
                continue;
            }
            if !KINDS.contains(&key.as_str()) {
                out.push(Note::new(&place, format!("ไม่รู้จัก {key} (ใช้ {})", KINDS.join(", ")), format!("{key} is not known (use {})", KINDS.join(", "))));
                continue;
            }
            let Some(entries) = value.as_table() else {
                out.push(Note::new(&place, format!("{key} ต้องเป็นตาราง ชื่อ = [เงิน, xp]"), format!("{key} must be a table of name = [money, xp]")));
                continue;
            };
            for (pattern, pair) in entries {
                let pattern = pattern.trim().to_ascii_lowercase();
                let pattern = pattern.strip_prefix("minecraft:").map(str::to_owned).unwrap_or(pattern);
                let pair: Option<(f64, f64)> = match pair {
                    toml::Value::Array(a) if a.len() == 2 => number(&a[0]).zip(number(&a[1])),
                    v => number(v).map(|m| (m, 0.0)),
                };
                match pair {
                    Some((money, xp)) if money >= 0.0 && xp >= 0.0 => pays.push(Pay { kind: key.clone(), pattern, money, xp }),
                    _ => out.push(Note::new(&place, format!("{key}.{pattern} ต้องเป็น [เงิน, xp] ที่ไม่ติดลบ"), format!("{key}.{pattern} must be [money, xp], not below 0"))),
                }
            }
        }
        if pays.is_empty() {
            out.push(Note::new(&place, "ยังไม่จ่ายอะไรเลย (เพิ่ม [<ชื่อ>.break] ฯลฯ)", "pays for nothing yet (add [<id>.break] and so on)"));
        }
        let text_of = |key: &str| t.get(key).and_then(toml::Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        let description = match t.get("description") {
            Some(toml::Value::String(s)) => vec![s.clone()],
            Some(toml::Value::Array(a)) => a.iter().filter_map(toml::Value::as_str).map(str::to_owned).collect(),
            _ => Vec::new(),
        };
        list.push(Job {
            id: id.clone(),
            name: text_of("name").unwrap_or_else(|| id.clone()),
            icon: text_of("icon").map_or_else(|| "minecraft:book".to_owned(), |i| if i.contains(':') { i.to_ascii_lowercase() } else { format!("minecraft:{}", i.to_ascii_lowercase()) }),
            description,
            max_level: t.get("max-level").and_then(number).unwrap_or(50.0).clamp(1.0, 1000.0) as u32,
            pays,
            permission: text_of("permission"),
        });
    }
    list
}

pub fn merge_jobs(all: Vec<Job>, out: &mut Vec<Note>) -> Vec<Job> {
    let mut list: Vec<Job> = Vec::new();
    for j in all {
        if list.iter().any(|o| o.id == j.id) {
            out.push(Note::new(&j.id, "ชื่อซ้ำกับไฟล์อื่น ใช้อันแรก", "the name is used in another file too; the first counts"));
        } else if list.len() >= MAX_JOBS {
            out.push(Note::new(&j.id, format!("มีได้ไม่เกิน {MAX_JOBS} งาน"), format!("at most {MAX_JOBS} jobs")));
        } else {
            list.push(j);
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
pub struct Worker {
    pub name: String,
    pub jobs: BTreeMap<String, Progress>,
    /// Money earned in all.
    pub earned: f64,
}

/// Add experience; returns how many levels were gained.
pub fn add_xp(cfg: &Config, p: &mut Progress, xp: f64, max_level: u32) -> u32 {
    if !xp.is_finite() || xp <= 0.0 {
        return 0;
    }
    let mut gained = 0;
    p.xp += xp;
    while p.level < max_level {
        let need = cfg.need(p.level);
        if p.xp < need {
            break;
        }
        p.xp -= need;
        p.level += 1;
        gained += 1;
    }
    if p.level >= max_level {
        p.xp = 0.0;
    }
    gained
}

/// Money for one deed at a level.
pub fn money_at(cfg: &Config, base: f64, level: u32) -> f64 {
    base * (1.0 + cfg.level_bonus * f64::from(level))
}

pub const EXAMPLE: &str = r#"# Jobs: one [table] per job, then a table for each kind of deed it pays for:
#   [<job>.break] [<job>.place] [<job>.kill] [<job>.fish] [<job>.craft] [<job>.smelt] [<job>.eat] [<job>.tame]
# Each line is  name = [money, xp]  where the name is the game's (stone, zombie, bread) or a
# pattern: "*_ore" (ends with), "oak_*" (starts with), "*" (anything). Money goes through Basics.
# Blocks a player placed pay nothing when broken again. Players in creative are not paid.

[miner]
name = "&7Miner"
icon = "minecraft:iron_pickaxe"
description = ["Break stone and ores"]
max-level = 50

[miner.break]
stone = [0.05, 1]
deepslate = [0.08, 1]
"*_ore" = [1.0, 5]
diamond_ore = [8.0, 30]
deepslate_diamond_ore = [8.0, 30]

[hunter]
name = "&cHunter"
icon = "minecraft:iron_sword"
description = ["Fight monsters"]

[hunter.kill]
zombie = [2.0, 10]
skeleton = [2.5, 12]
creeper = [3.0, 15]
"*" = [0.5, 2]

[farmer]
name = "&aFarmer"
icon = "minecraft:wheat"

[farmer.break]
wheat = [0.3, 2]
carrots = [0.3, 2]
potatoes = [0.3, 2]
melon = [0.5, 3]
pumpkin = [0.5, 3]

[fisher]
name = "&bFisher"
icon = "minecraft:fishing_rod"

[fisher.fish]
"*" = [3.0, 15]
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_reads_cleanly() {
        let mut notes = Vec::new();
        let list = parse_jobs("example.toml", EXAMPLE, &mut notes);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(list.len(), 4);
    }

    #[test]
    fn the_most_exact_pay_wins() {
        let mut notes = Vec::new();
        let list = parse_jobs("e.toml", EXAMPLE, &mut notes);
        let miner = list.iter().find(|j| j.id == "miner").unwrap();
        assert_eq!(miner.pay_for("break", "diamond_ore").unwrap().money, 8.0);
        assert_eq!(miner.pay_for("break", "iron_ore").unwrap().money, 1.0);
        assert!(miner.pay_for("break", "dirt").is_none());
        assert!(miner.pay_for("place", "stone").is_none());
        let hunter = list.iter().find(|j| j.id == "hunter").unwrap();
        assert_eq!(hunter.pay_for("kill", "zombie").unwrap().xp, 10.0);
        assert_eq!(hunter.pay_for("kill", "pig").unwrap().money, 0.5);
    }

    #[test]
    fn bad_lines_are_explained() {
        let mut notes = Vec::new();
        let list = parse_jobs("x.toml", "[a]\n[a.break]\nstone = [-1, 2]\ndirt = \"x\"\n[a.mine]\nx = 1\n[a.fish]\n\"*\" = 2\n", &mut notes);
        assert_eq!(list[0].pays.len(), 1);
        assert_eq!(list[0].pays[0], Pay { kind: "fish".into(), pattern: "*".into(), money: 2.0, xp: 0.0 });
        assert_eq!(notes.len(), 3, "{notes:?}");
    }

    #[test]
    fn levels_grow_and_stop_at_the_top() {
        let cfg = Config::default();
        let mut p = Progress::default();
        assert_eq!(add_xp(&cfg, &mut p, 99.0, 50), 0);
        assert_eq!(add_xp(&cfg, &mut p, 1.0 + cfg.need(1) + 1e-6, 50), 2);
        assert_eq!(p.level, 2);
        assert!(p.xp.abs() < 1e-5);
        assert_eq!(add_xp(&cfg, &mut p, 1e12, 5), 3);
        assert_eq!((p.level, p.xp), (5, 0.0));
        assert_eq!(add_xp(&cfg, &mut p, f64::NAN, 50), 0);
        assert!((money_at(&cfg, 1.0, 10) - 1.2).abs() < 1e-9);
    }
}

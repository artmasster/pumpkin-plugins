//! What the server owner wrote: the quests, their goals and rewards, and each player's
//! progress. No server needed here.
//!
//! A quest has goals (break, place, kill, fish, craft, smelt, eat, tame things; talk to an NPC
//! of the Npcs plugin; bring items) and rewards (items, money, console commands). It may be
//! given and handed in at an NPC, need other quests first, and come back after a cooldown.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[path = "../../common/src/names.rs"]
mod names;

pub const NS: &str = "quests";
pub const MAX_QUESTS: usize = 500;
pub const MAX_GOALS: usize = 16;
pub const KINDS: [&str; 10] = ["break", "place", "kill", "fish", "craft", "smelt", "eat", "tame", "talk", "bring"];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// Quests a player may have going at once.
    pub max_active: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), max_active: 5 }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.max_active = self.max_active.clamp(1, 50);
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

#[derive(Clone, Debug, PartialEq)]
pub struct Goal {
    pub kind: String,
    /// What it is done to: a name or pattern (`oak_log`, `*_ore`), an NPC for `talk`, an item
    /// (`minecraft:diamond` or a CustomItems id) for `bring`.
    pub target: String,
    pub count: u32,
    /// Shown instead of `kind target`.
    pub text: Option<String>,
}

impl Goal {
    /// Does this deed count for the goal?
    pub fn counts(&self, kind: &str, target: &str) -> bool {
        self.kind == kind && names::matches(&self.target, target)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reward {
    pub item: Option<String>,
    pub count: u32,
    pub money: f64,
    pub commands: Vec<String>,
    pub text: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Quest {
    pub id: String,
    pub name: String,
    pub description: Vec<String>,
    pub icon: String,
    /// Given and handed in at this NPC; none = from `/quests`, done by itself.
    pub npc: Option<String>,
    pub requires: Vec<String>,
    /// Seconds before it can be done again; none = once.
    pub cooldown: Option<u64>,
    pub permission: Option<String>,
    pub goals: Vec<Goal>,
    pub rewards: Vec<Reward>,
}

fn number(v: Option<&toml::Value>) -> Option<f64> {
    match v? {
        toml::Value::Integer(i) => Some(*i as f64),
        toml::Value::Float(f) if f.is_finite() => Some(*f),
        _ => None,
    }
}

fn item_ref(text: &str) -> Option<String> {
    let t = text.trim().to_ascii_lowercase();
    let ok = |s: &str| valid_id(s);
    match t.strip_prefix("minecraft:") {
        Some(name) => ok(name).then_some(t),
        None => ok(&t).then_some(t),
    }
}

const KNOWN: [&str; 10] = ["name", "description", "icon", "npc", "requires", "cooldown", "repeat", "permission", "goals", "rewards"];

/// The quests of one file.
pub fn parse_quests(file: &str, text: &str, out: &mut Vec<Note>) -> Vec<Quest> {
    let table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            let why = e.to_string().lines().next().unwrap_or_default().to_owned();
            out.push(Note::new(file, format!("อ่านไม่ได้: {why}"), format!("cannot be read: {why}")));
            return Vec::new();
        }
    };
    let mut list = Vec::new();
    'quests: for (id, v) in &table {
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
        let strings = |key: &str| match t.get(key) {
            Some(toml::Value::String(s)) => vec![s.clone()],
            Some(toml::Value::Array(a)) => a.iter().filter_map(toml::Value::as_str).map(str::to_owned).collect(),
            _ => Vec::new(),
        };
        let mut goals = Vec::new();
        for (n, g) in t.get("goals").and_then(toml::Value::as_array).cloned().unwrap_or_default().iter().enumerate().take(MAX_GOALS) {
            let here = format!("{place} goal {}", n + 1);
            let Some(g) = g.as_table() else { continue };
            let kind = g.get("kind").and_then(toml::Value::as_str).unwrap_or_default().to_ascii_lowercase();
            if !KINDS.contains(&kind.as_str()) {
                out.push(Note::new(&here, format!("kind ต้องเป็น {}", KINDS.join(", ")), format!("kind must be one of {}", KINDS.join(", "))));
                continue 'quests;
            }
            let raw = g.get("target").and_then(toml::Value::as_str).unwrap_or("*").trim().to_ascii_lowercase();
            let target = match kind.as_str() {
                "bring" => match item_ref(&raw) {
                    Some(i) => i,
                    None => {
                        out.push(Note::new(&here, "bring ต้องมี target เป็นไอเท็ม", "bring needs an item as target"));
                        continue 'quests;
                    }
                },
                _ => raw.strip_prefix("minecraft:").map(str::to_owned).unwrap_or(raw),
            };
            if kind == "talk" && !valid_id(&target) {
                out.push(Note::new(&here, "talk ต้องมี target เป็นชื่อ NPC", "talk needs an NPC id as target"));
                continue 'quests;
            }
            let count = number(g.get("count")).unwrap_or(1.0).clamp(1.0, 1_000_000.0) as u32;
            goals.push(Goal { kind, target, count, text: g.get("text").and_then(toml::Value::as_str).map(str::to_owned) });
        }
        if goals.is_empty() {
            out.push(Note::new(&place, "ต้องมีอย่างน้อยหนึ่ง goal ([[<ชื่อ>.goals]])", "needs at least one goal ([[<id>.goals]])"));
            continue;
        }
        let mut rewards = Vec::new();
        for (n, r) in t.get("rewards").and_then(toml::Value::as_array).cloned().unwrap_or_default().iter().enumerate() {
            let Some(r) = r.as_table() else { continue };
            let item = match r.get("item").and_then(toml::Value::as_str) {
                Some(i) => match item_ref(i) {
                    Some(i) => Some(i),
                    None => {
                        out.push(Note::new(&format!("{place} reward {}", n + 1), "item ต้องเป็น id ของ CustomItems หรือ minecraft:<ไอเท็ม>", "item must be a CustomItems id or minecraft:<item>"));
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
            rewards.push(Reward { item, count: number(r.get("count")).unwrap_or(1.0).clamp(1.0, 2304.0) as u32, money: number(r.get("money")).unwrap_or(0.0).max(0.0), commands, text: r.get("text").and_then(toml::Value::as_str).map(str::to_owned) });
        }
        let cooldown = match (t.get("cooldown"), text_of("repeat").as_deref()) {
            (Some(v), _) => number(Some(v)).map(|s| s.max(0.0) as u64),
            (None, Some("daily")) => Some(86_400),
            (None, Some("weekly")) => Some(7 * 86_400),
            (None, Some("always")) => Some(0),
            _ => None,
        };
        list.push(Quest {
            id: id.clone(),
            name: text_of("name").unwrap_or_else(|| id.clone()),
            description: strings("description"),
            icon: text_of("icon").map_or_else(|| "minecraft:writable_book".to_owned(), |i| if i.contains(':') { i.to_ascii_lowercase() } else { format!("minecraft:{}", i.to_ascii_lowercase()) }),
            npc: text_of("npc").map(|n| n.to_ascii_lowercase()),
            requires: strings("requires").into_iter().map(|r| r.to_ascii_lowercase()).collect(),
            cooldown,
            permission: text_of("permission"),
            goals,
            rewards,
        });
    }
    list
}

pub fn merge_quests(all: Vec<Quest>, out: &mut Vec<Note>) -> Vec<Quest> {
    let mut list: Vec<Quest> = Vec::new();
    for q in all {
        if list.iter().any(|o| o.id == q.id) {
            out.push(Note::new(&q.id, "ชื่อซ้ำกับไฟล์อื่น ใช้อันแรก", "the name is used in another file too; the first counts"));
        } else if list.len() >= MAX_QUESTS {
            out.push(Note::new(&q.id, format!("มีได้ไม่เกิน {MAX_QUESTS} เควสต์"), format!("at most {MAX_QUESTS} quests")));
        } else {
            list.push(q);
        }
    }
    for q in &list {
        for r in q.requires.iter().filter(|r| !list.iter().any(|o| o.id == **r)) {
            out.push(Note::new(&q.id, format!("requires {r} ไม่มีอยู่"), format!("requires {r}, which does not exist")));
        }
    }
    list
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Journal {
    pub name: String,
    /// Quests going on: counts per goal.
    pub active: BTreeMap<String, Vec<u32>>,
    /// Quests done: when (epoch seconds).
    pub done: BTreeMap<String, u64>,
}

/// Why a player cannot take a quest now.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    Active,
    Done,
    /// Seconds left of the cooldown.
    Later(u64),
    Needs(String),
    Full,
}

pub fn may_take(q: &Quest, j: &Journal, now: u64, max_active: u32) -> Result<(), Refusal> {
    if j.active.contains_key(&q.id) {
        return Err(Refusal::Active);
    }
    if let Some(at) = j.done.get(&q.id) {
        match q.cooldown {
            None => return Err(Refusal::Done),
            Some(wait) if now < at + wait => return Err(Refusal::Later(at + wait - now)),
            _ => {}
        }
    }
    if let Some(missing) = q.requires.iter().find(|r| !j.done.contains_key(*r)) {
        return Err(Refusal::Needs(missing.clone()));
    }
    if j.active.len() >= max_active as usize {
        return Err(Refusal::Full);
    }
    Ok(())
}

/// Count a deed for a quest going on. Returns true when any goal moved.
pub fn advance(q: &Quest, counts: &mut Vec<u32>, kind: &str, target: &str, n: u32) -> bool {
    counts.resize(q.goals.len(), 0);
    let mut moved = false;
    for (g, have) in q.goals.iter().zip(counts.iter_mut()) {
        if g.kind != "bring" && *have < g.count && g.counts(kind, target) {
            *have = (*have + n).min(g.count);
            moved = true;
        }
    }
    moved
}

/// All goals but `bring` are reached (those are looked at when the quest is handed in).
pub fn ready(q: &Quest, counts: &[u32]) -> bool {
    q.goals.iter().enumerate().all(|(i, g)| g.kind == "bring" || counts.get(i).copied().unwrap_or(0) >= g.count)
}

pub const EXAMPLE: &str = r#"# Quests: one [table] per quest, with [[<quest>.goals]] and [[<quest>.rewards]].
# Players see them in /quests. A quest with npc = "<id of an NPC of the Npcs plugin>" is given and
# handed in at that NPC (give the NPC the action: quest <quest id>); others start from /quests and
# finish by themselves.
#
# goal kind: break, place, kill, fish, craft, smelt, eat, tame (target = the game's name or a
#   pattern like "*_ore"), talk (target = NPC id), bring (target = item; taken at hand-in)
# requires = quests done before; cooldown = seconds until it can be done again, or
# repeat = "daily" / "weekly"; none of them = once only.
# reward: item + count, money (Basics), commands (as the console, {player} = the player), text.

[wood]
name = "&aLumberjack"
description = ["Chop some oak for the village"]
icon = "minecraft:oak_log"

[[wood.goals]]
kind = "break"
target = "oak_log"
count = 10

[[wood.rewards]]
item = "minecraft:iron_axe"

[[wood.rewards]]
money = 50

[hunt]
name = "&cNight Watch"
requires = ["wood"]
repeat = "daily"

[[hunt.goals]]
kind = "kill"
target = "zombie"
count = 5

[[hunt.rewards]]
money = 100
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> Vec<Quest> {
        let mut notes = Vec::new();
        let list = merge_quests(parse_quests("e.toml", EXAMPLE, &mut notes), &mut notes);
        assert!(notes.is_empty(), "{notes:?}");
        list
    }

    #[test]
    fn the_example_reads_cleanly() {
        let list = example();
        assert_eq!(list.len(), 2);
        let hunt = list.iter().find(|q| q.id == "hunt").unwrap();
        assert_eq!(hunt.cooldown, Some(86_400));
        assert_eq!(hunt.requires, vec!["wood".to_owned()]);
    }

    #[test]
    fn who_may_take_a_quest() {
        let list = example();
        let (wood, hunt) = (&list.iter().find(|q| q.id == "wood").unwrap(), &list.iter().find(|q| q.id == "hunt").unwrap());
        let mut j = Journal::default();
        assert_eq!(may_take(hunt, &j, 1000, 5), Err(Refusal::Needs("wood".into())));
        assert_eq!(may_take(wood, &j, 1000, 5), Ok(()));
        j.active.insert("wood".into(), vec![0]);
        assert_eq!(may_take(wood, &j, 1000, 5), Err(Refusal::Active));
        j.active.clear();
        j.done.insert("wood".into(), 900);
        assert_eq!(may_take(wood, &j, 1000, 5), Err(Refusal::Done));
        assert_eq!(may_take(hunt, &j, 1000, 5), Ok(()));
        j.done.insert("hunt".into(), 1000);
        assert_eq!(may_take(hunt, &j, 2000, 5), Err(Refusal::Later(85_400)));
        assert_eq!(may_take(hunt, &j, 1000 + 86_400, 5), Ok(()));
        assert_eq!(may_take(hunt, &Journal { active: [("x".to_owned(), vec![])].into(), done: j.done.clone(), ..Journal::default() }, 1000 + 86_400, 1), Err(Refusal::Full));
    }

    #[test]
    fn deeds_move_goals_up_to_their_count() {
        let list = example();
        let wood = list.iter().find(|q| q.id == "wood").unwrap();
        let mut c = Vec::new();
        assert!(advance(wood, &mut c, "break", "oak_log", 4));
        assert!(!advance(wood, &mut c, "break", "birch_log", 1));
        assert!(!advance(wood, &mut c, "place", "oak_log", 1));
        assert!(!ready(wood, &c));
        advance(wood, &mut c, "break", "oak_log", 100);
        assert_eq!(c, vec![10]);
        assert!(ready(wood, &c));
    }

    #[test]
    fn broken_quests_are_left_out() {
        let mut notes = Vec::new();
        let list = parse_quests("x.toml", "[a]\nname = \"A\"\n[b]\n[[b.goals]]\nkind = \"dance\"\n[c]\n[[c.goals]]\nkind = \"bring\"\ntarget = \"Not An Item\"\n[d]\nrequires = \"zz\"\n[[d.goals]]\nkind = \"talk\"\ntarget = \"guide\"\n", &mut notes);
        let list = merge_quests(list, &mut notes);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "d");
        assert_eq!(notes.len(), 4, "{notes:?}");
    }
}

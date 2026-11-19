//! Regions: boxes of the world with rules (flags), set by admins. No server needed here.
//!
//! A flag says `allow` or `deny` (or holds a text, for `greeting` and `farewell`). Where regions
//! overlap, the one with the highest priority that sets a flag decides it; a world's region
//! `__global__` covers all of that world below every other region.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[path = "../../common/src/names.rs"]
mod names;
pub use names::{block_use, is_bucket, item_changes_world};

pub const NS: &str = "regions";
pub const GLOBAL: &str = "__global__";

/// Flags with a yes or no.
pub const SWITCHES: [&str; 12] = ["build", "interact", "pvp", "mob-damage", "mob-spawning", "explosions", "fire-spread", "entry", "item-drop", "fall-damage", "invincible", "hunger"];
pub const TEXTS: [&str; 2] = ["greeting", "farewell"];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into() }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self
    }

    pub fn thai(&self) -> bool {
        self.language == "th"
    }
}

pub fn valid_id(id: &str) -> bool {
    id == GLOBAL || (!id.is_empty() && id.len() <= 48 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub id: String,
    pub world: String,
    pub min: [i32; 3],
    pub max: [i32; 3],
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub flags: BTreeMap<String, String>,
    /// Player key → name.
    #[serde(default)]
    pub members: BTreeMap<String, String>,
    #[serde(default)]
    pub owners: BTreeMap<String, String>,
}

impl Region {
    pub fn new(id: &str, world: &str, a: [i32; 3], b: [i32; 3]) -> Region {
        Region {
            id: id.to_owned(),
            world: world.to_owned(),
            min: [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
            max: [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
            priority: 0,
            flags: BTreeMap::new(),
            members: BTreeMap::new(),
            owners: BTreeMap::new(),
        }
    }

    pub fn global(&self) -> bool {
        self.id == GLOBAL
    }

    pub fn contains(&self, world: &str, x: i32, y: i32, z: i32) -> bool {
        self.world == world && (self.global() || (x >= self.min[0] && x <= self.max[0] && y >= self.min[1] && y <= self.max[1] && z >= self.min[2] && z <= self.max[2]))
    }

    /// Members and owners pass `build`, `interact` and `entry`.
    pub fn knows(&self, key: &str) -> bool {
        self.members.contains_key(key) || self.owners.contains_key(key)
    }

    pub fn volume(&self) -> i64 {
        (0..3).map(|i| i64::from(self.max[i] - self.min[i] + 1)).product()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Map {
    pub regions: Vec<Region>,
}

/// What a flag says somewhere: allowed, refused, or nothing set (the game's way).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Say {
    Allow,
    Deny,
    Unset,
}

impl Map {
    /// Regions at a block, highest priority first, the global one last.
    pub fn at(&self, world: &str, x: i32, y: i32, z: i32) -> Vec<&Region> {
        let mut found: Vec<&Region> = self.regions.iter().filter(|r| r.contains(world, x, y, z)).collect();
        found.sort_by(|a, b| a.global().cmp(&b.global()).then(b.priority.cmp(&a.priority)).then(a.volume().cmp(&b.volume())));
        found
    }

    /// A yes-or-no flag at a block for a player (`None` = not a player). Members of the region
    /// that decides pass the flags of their own region for `build`, `interact` and `entry`.
    pub fn say(&self, flag: &str, world: &str, x: i32, y: i32, z: i32, player: Option<&str>) -> Say {
        for r in self.at(world, x, y, z) {
            match r.flags.get(flag).map(String::as_str) {
                Some("allow") => return Say::Allow,
                Some("deny") => {
                    let passes = matches!(flag, "build" | "interact" | "entry") && player.is_some_and(|p| r.knows(p));
                    return if passes { Say::Allow } else { Say::Deny };
                }
                _ => {}
            }
        }
        Say::Unset
    }

    /// A text flag where the regions decide it.
    pub fn text(&self, flag: &str, world: &str, x: i32, y: i32, z: i32) -> Option<String> {
        self.at(world, x, y, z).iter().find_map(|r| r.flags.get(flag).cloned())
    }

    pub fn get_mut(&mut self, world: &str, id: &str) -> Option<&mut Region> {
        self.regions.iter_mut().find(|r| r.id == id && (r.world == world || !r.global()))
    }

    /// The non-global regions a block is in, as their ids (to see when a player walks in).
    pub fn ids_at(&self, world: &str, x: i32, y: i32, z: i32) -> Vec<String> {
        self.regions.iter().filter(|r| !r.global() && r.contains(world, x, y, z)).map(|r| r.id.clone()).collect()
    }
}

/// `allow`, `deny` or text for a flag; `none` clears it. Err = what is wrong.
pub fn flag_value(flag: &str, value: &str) -> Result<Option<String>, (String, String)> {
    if value.eq_ignore_ascii_case("none") || value.is_empty() {
        return Ok(None);
    }
    if SWITCHES.contains(&flag) {
        return match value.to_ascii_lowercase().as_str() {
            "allow" | "true" | "yes" => Ok(Some("allow".into())),
            "deny" | "false" | "no" => Ok(Some("deny".into())),
            _ => Err(("ค่าต้องเป็น allow, deny หรือ none".into(), "the value must be allow, deny or none".into())),
        };
    }
    if TEXTS.contains(&flag) {
        return Ok(Some(value.chars().filter(|c| !c.is_control()).take(200).collect()));
    }
    Err((format!("ไม่รู้จัก flag {flag}"), format!("unknown flag {flag}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> Map {
        let mut spawn = Region::new("spawn", "w", [10, 0, 10], [-10, 100, -10]);
        spawn.flags.insert("build".into(), "deny".into());
        spawn.flags.insert("pvp".into(), "deny".into());
        spawn.members.insert("m".into(), "M".into());
        let mut arena = Region::new("arena", "w", [0, 0, 0], [5, 100, 5]);
        arena.priority = 10;
        arena.flags.insert("pvp".into(), "allow".into());
        let mut global = Region::new(GLOBAL, "w", [0, 0, 0], [0, 0, 0]);
        global.flags.insert("explosions".into(), "deny".into());
        global.flags.insert("greeting".into(), "hi".into());
        Map { regions: vec![global, spawn, arena] }
    }

    #[test]
    fn boxes_are_ordered_corners() {
        let r = Region::new("a", "w", [5, 70, -3], [-2, 60, 4]);
        assert_eq!((r.min, r.max), ([-2, 60, -3], [5, 70, 4]));
        assert!(r.contains("w", 5, 60, 4));
        assert!(!r.contains("w", 6, 60, 4));
        assert!(!r.contains("x", 0, 65, 0));
        assert_eq!(r.volume(), 8 * 11 * 8);
    }

    #[test]
    fn the_highest_priority_decides() {
        let m = map();
        assert_eq!(m.say("pvp", "w", 2, 50, 2, None), Say::Allow);
        assert_eq!(m.say("pvp", "w", -5, 50, -5, None), Say::Deny);
        assert_eq!(m.say("pvp", "w", 50, 50, 50, None), Say::Unset);
        // The global region is below everything
        assert_eq!(m.say("explosions", "w", 2, 50, 2, None), Say::Deny);
        assert_eq!(m.say("explosions", "other", 2, 50, 2, None), Say::Unset);
        assert_eq!(m.text("greeting", "w", 0, 0, 0).as_deref(), Some("hi"));
    }

    #[test]
    fn members_build_where_others_cannot() {
        let m = map();
        assert_eq!(m.say("build", "w", -5, 50, -5, Some("x")), Say::Deny);
        assert_eq!(m.say("build", "w", -5, 50, -5, Some("m")), Say::Allow);
        // Members do not pass pvp
        assert_eq!(m.say("pvp", "w", -5, 50, -5, Some("m")), Say::Deny);
        assert_eq!(m.ids_at("w", 2, 50, 2), vec!["spawn".to_owned(), "arena".to_owned()]);
    }

    #[test]
    fn flag_values() {
        assert_eq!(flag_value("pvp", "DENY"), Ok(Some("deny".into())));
        assert_eq!(flag_value("pvp", "none"), Ok(None));
        assert!(flag_value("pvp", "maybe").is_err());
        assert!(flag_value("fly", "allow").is_err());
        assert_eq!(flag_value("greeting", "&aWelcome"), Ok(Some("&aWelcome".into())));
        assert!(valid_id("spawn_1") && valid_id(GLOBAL) && !valid_id("Spawn"));
    }
}

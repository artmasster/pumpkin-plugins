//! Graves: what a player carried when they died, kept where they died. No server needed here.
//!
//! Items are kept as the text of `mcsv_common::items` (every part of the item) with the slot
//! they were in, so they go back where they were.
use serde::{Deserialize, Serialize};

pub const NS: &str = "graves";
/// Longest item text a grave keeps; a longer item (a full shulker box) is left to the game.
pub const MAX_ITEM: usize = 30_000;
/// Slots: 0 to 35 the inventory, then these.
pub const HEAD: u8 = 100;
pub const CHEST: u8 = 101;
pub const LEGS: u8 = 102;
pub const FEET: u8 = 103;
pub const OFF_HAND: u8 = 104;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// Minutes a grave lasts (0 = until it is emptied).
    pub expire_minutes: u32,
    /// Minutes after which anyone may open it (0 = only its owner, ever).
    pub public_minutes: u32,
    /// Armor and the off hand go into the grave too, as in the game.
    pub take_armor: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), expire_minutes: 120, public_minutes: 30, take_armor: true }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.expire_minutes = self.expire_minutes.min(60 * 24 * 30);
        self
    }

    pub fn thai(&self) -> bool {
        self.language == "th"
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Kept {
    pub slot: u8,
    pub item: String,
    pub count: u8,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Grave {
    pub id: u64,
    pub owner: String,
    pub owner_name: String,
    pub world: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    /// Epoch seconds.
    pub made: u64,
    pub items: Vec<Kept>,
}

impl Grave {
    pub fn expired(&self, cfg: &Config, now: u64) -> bool {
        cfg.expire_minutes > 0 && now >= self.made + u64::from(cfg.expire_minutes) * 60
    }

    /// May this player open it?
    pub fn may_open(&self, cfg: &Config, player: &str, admin: bool, now: u64) -> bool {
        admin || self.owner == player || (cfg.public_minutes > 0 && now >= self.made + u64::from(cfg.public_minutes) * 60)
    }

    /// Seconds until it is gone; none when it never goes.
    pub fn left(&self, cfg: &Config, now: u64) -> Option<u64> {
        (cfg.expire_minutes > 0).then(|| (self.made + u64::from(cfg.expire_minutes) * 60).saturating_sub(now))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Yard {
    pub next_id: u64,
    pub graves: Vec<Grave>,
}

/// Where a grave goes for a death at `y`: never below the bottom of the world.
pub fn grave_y(y: f64, min_y: i32) -> f64 {
    if y.is_finite() { y.max(f64::from(min_y) + 1.0) } else { f64::from(min_y) + 1.0 }
}

/// The order items go back: each to its own slot when that is empty, the rest to empty slots.
/// `free(slot)` says whether a slot is empty; returns (item index, slot) pairs.
pub fn placement(items: &[Kept], free: &dyn Fn(u8) -> bool) -> Vec<(usize, u8)> {
    let mut taken: Vec<u8> = Vec::new();
    let mut out = Vec::new();
    let mut later = Vec::new();
    for (i, k) in items.iter().enumerate() {
        if free(k.slot) && !taken.contains(&k.slot) {
            taken.push(k.slot);
            out.push((i, k.slot));
        } else {
            later.push(i);
        }
    }
    let mut spare = (0u8..36).filter(|s| free(*s) && !taken.contains(s));
    for i in later {
        match spare.next() {
            Some(s) => out.push((i, s)),
            None => break,
        }
    }
    out
}

pub fn span(th: bool, secs: u64) -> String {
    let (h, m) = (secs / 3600, secs / 60 % 60);
    match (h, th) {
        (0, true) => format!("{} นาที", m.max(1)),
        (0, false) => format!("{}m", m.max(1)),
        (_, true) => format!("{h} ชม. {m} นาที"),
        (_, false) => format!("{h}h {m}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(slot: u8) -> Kept {
        Kept { slot, item: "dirt".into(), count: 1 }
    }

    #[test]
    fn items_go_back_where_they_were() {
        let items = vec![kept(0), kept(HEAD), kept(5), kept(6)];
        // Slot 5 is taken now: that item goes to the first empty one
        let free = |s: u8| s != 5 && s != 1;
        let out = placement(&items, &free);
        assert_eq!(out, vec![(0, 0), (1, HEAD), (3, 6), (2, 2)]);
        // No room at all: nothing moves
        assert!(placement(&items, &|_| false).is_empty());
    }

    #[test]
    fn who_may_open_and_when_it_goes() {
        let cfg = Config::default();
        let g = Grave { id: 1, owner: "a".into(), owner_name: "A".into(), world: "w".into(), x: 0.0, y: 0.0, z: 0.0, made: 1000, items: vec![] };
        assert!(g.may_open(&cfg, "a", false, 1000));
        assert!(!g.may_open(&cfg, "b", false, 1000 + 29 * 60));
        assert!(g.may_open(&cfg, "b", false, 1000 + 30 * 60));
        assert!(g.may_open(&cfg, "b", true, 1000));
        assert!(!g.expired(&cfg, 1000 + 119 * 60));
        assert!(g.expired(&cfg, 1000 + 120 * 60));
        assert_eq!(g.left(&cfg, 1000), Some(7200));
        let never = Config { expire_minutes: 0, public_minutes: 0, ..Config::default() };
        assert!(!g.expired(&never, u64::MAX / 2));
        assert!(!g.may_open(&never, "b", false, u64::MAX / 2));
    }

    #[test]
    fn graves_stay_in_the_world() {
        assert_eq!(grave_y(-80.0, -64), -63.0);
        assert_eq!(grave_y(70.5, -64), 70.5);
        assert_eq!(grave_y(f64::NAN, 0), 1.0);
    }
}

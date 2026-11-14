//! What to remove and when to warn. Plain Rust: the plugin hands in what it sees in the world.
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// Seconds between automatic clears (0 = only by command).
    pub interval: u64,
    /// Seconds before a clear at which players are told.
    pub warnings: Vec<u64>,
    /// Entity kinds a clear removes.
    pub remove: Vec<String>,
    /// Things younger than this many seconds are left alone (a player may have just dropped them).
    pub min_age: u64,
    /// Never remove something that carries a custom name.
    pub keep_named: bool,
    /// Tell players about clears and warnings.
    pub broadcast: bool,
    /// More than this many `limited` mobs in one chunk: the extra ones are removed (0 = off).
    pub chunk_limit: u32,
    /// Kinds counted by `chunk-limit`.
    pub limited: Vec<String>,
    /// Most wild creatures (monsters, fish, squid, bats) a world may hold; the youngest beyond
    /// that are removed every ten seconds (0 = off). Pumpkin gives plugins no say in natural
    /// spawning, so the cap works by removal.
    pub mob_cap: u32,
}

impl Default for Config {
    fn default() -> Self {
        let list = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect();
        Self {
            language: "th".into(),
            interval: 600,
            warnings: vec![60, 30, 10, 5],
            remove: list(&["item", "experience_orb", "arrow", "spectral_arrow"]),
            min_age: 15,
            keep_named: true,
            broadcast: true,
            chunk_limit: 0,
            limited: list(&["cow", "pig", "sheep", "chicken", "zombie", "skeleton", "creeper", "spider", "zombified_piglin"]),
            mob_cap: 0,
        }
    }
}

/// `minecraft:Experience_Orb` → `experience_orb`
pub fn kind_key(name: &str) -> String {
    name.trim().to_lowercase().trim_start_matches("minecraft:").to_owned()
}

impl Config {
    pub fn cleaned(mut self) -> Self {
        if self.interval > 0 {
            self.interval = self.interval.clamp(30, 86_400);
        }
        self.warnings.retain(|w| *w > 0 && *w < self.interval.max(1));
        self.warnings.sort_unstable_by(|a, b| b.cmp(a));
        self.warnings.dedup();
        self.warnings.truncate(8);
        // Players are never ours to remove, whatever the file says
        let tidy = |v: &mut Vec<String>| {
            *v = v.iter().map(|k| kind_key(k)).filter(|k| !k.is_empty() && k != "player").collect();
            v.sort();
            v.dedup();
        };
        tidy(&mut self.remove);
        tidy(&mut self.limited);
        self.min_age = self.min_age.min(3600);
        self
    }

    pub fn thai(&self) -> bool {
        self.language != "en"
    }
}

/// Mobs `/cleaner killmobs` removes.
pub const HOSTILE: &[&str] = &[
    "blaze", "bogged", "breeze", "cave_spider", "creaking", "creeper", "drowned", "elder_guardian", "endermite", "evoker",
    "ghast", "guardian", "hoglin", "husk", "magma_cube", "phantom", "piglin_brute", "pillager", "ravager", "shulker",
    "silverfish", "skeleton", "slime", "spider", "stray", "vex", "vindicator", "witch", "wither_skeleton", "zoglin",
    "zombie", "zombie_villager", "parched", "camel_husk",
];

/// Wild creatures that come back by themselves, besides the monsters: safe to thin out.
const AMBIENT: &[&str] = &["bat", "cod", "salmon", "squid", "glow_squid", "tropical_fish", "pufferfish"];

/// Counted and culled by `mob-cap`: never pets, farm animals or villagers.
pub fn is_wild(kind: &str) -> bool {
    HOSTILE.contains(&kind) || AMBIENT.contains(&kind)
}

/// One entity as the plugin saw it.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    pub kind: String,
    pub named: bool,
    pub age_secs: u64,
    pub world: usize,
    pub chunk: (i32, i32),
}

/// Indexes a timed or manual clear removes.
pub fn pick_clear(cfg: &Config, seen: &[Seen]) -> Vec<usize> {
    seen.iter()
        .enumerate()
        .filter(|(_, e)| cfg.remove.contains(&e.kind) && e.age_secs >= cfg.min_age && !(cfg.keep_named && e.named))
        .map(|(i, _)| i)
        .collect()
}

/// Indexes `/cleaner killmobs` removes: hostile mobs without a name.
pub fn pick_hostile(seen: &[Seen]) -> Vec<usize> {
    seen.iter().enumerate().filter(|(_, e)| HOSTILE.contains(&e.kind.as_str()) && !e.named).map(|(i, _)| i).collect()
}

/// Indexes over the per-chunk limit: in each chunk the oldest `chunk-limit` limited mobs stay.
pub fn pick_over_limit(cfg: &Config, seen: &[Seen]) -> Vec<usize> {
    if cfg.chunk_limit == 0 {
        return Vec::new();
    }
    let mut by_chunk: HashMap<(usize, i32, i32), Vec<usize>> = HashMap::new();
    for (i, e) in seen.iter().enumerate() {
        if cfg.limited.contains(&e.kind) && !(cfg.keep_named && e.named) {
            by_chunk.entry((e.world, e.chunk.0, e.chunk.1)).or_default().push(i);
        }
    }
    let mut out = Vec::new();
    for mut list in by_chunk.into_values() {
        if list.len() > cfg.chunk_limit as usize {
            list.sort_by(|a, b| seen[*b].age_secs.cmp(&seen[*a].age_secs).then(a.cmp(b)));
            out.extend(list.into_iter().skip(cfg.chunk_limit as usize));
        }
    }
    out.sort_unstable();
    out
}

/// Indexes over `mob-cap`: per world, the youngest unnamed wild creatures beyond the cap.
pub fn pick_over_cap(cfg: &Config, seen: &[Seen]) -> Vec<usize> {
    if cfg.mob_cap == 0 {
        return Vec::new();
    }
    let mut by_world: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, e) in seen.iter().enumerate() {
        if is_wild(&e.kind) && !e.named {
            by_world.entry(e.world).or_default().push(i);
        }
    }
    let mut out = Vec::new();
    for mut list in by_world.into_values() {
        if list.len() > cfg.mob_cap as usize {
            list.sort_by(|a, b| seen[*b].age_secs.cmp(&seen[*a].age_secs).then(a.cmp(b)));
            out.extend(list.into_iter().skip(cfg.mob_cap as usize));
        }
    }
    out.sort_unstable();
    out
}

/// The warning to send when the time left went from `before` to `now` seconds.
pub fn due_warning(cfg: &Config, before: u64, now: u64) -> Option<u64> {
    cfg.warnings.iter().copied().filter(|w| before > *w && now <= *w).min()
}

/// `EntityType::ExperienceOrb` → `experience_orb` (kinds arrive as Rust enum names).
pub fn snake(name: &str) -> String {
    let name = name.rsplit("::").next().unwrap_or(name);
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(kind: &str, named: bool, age: u64, chunk: (i32, i32)) -> Seen {
        Seen { kind: kind.into(), named, age_secs: age, world: 0, chunk }
    }

    #[test]
    fn clear_takes_old_unnamed_listed_things() {
        let cfg = Config::default().cleaned();
        let seen = [
            e("item", false, 100, (0, 0)),
            e("item", false, 3, (0, 0)),
            e("item", true, 100, (0, 0)),
            e("cow", false, 100, (0, 0)),
            e("experience_orb", false, 20, (1, 0)),
            e("arrow", false, 15, (1, 0)),
        ];
        assert_eq!(pick_clear(&cfg, &seen), vec![0, 4, 5]);
        let all = Config { keep_named: false, min_age: 0, ..cfg };
        assert_eq!(pick_clear(&all, &seen), vec![0, 1, 2, 4, 5]);
    }

    #[test]
    fn config_never_lists_players_and_keeps_warnings_sane() {
        let cfg: Config = toml::from_str("interval = 5\nwarnings = [600, 10, 10, 0, 3]\nremove = [\"minecraft:Item\", \"player\", \" \", \"item\"]").unwrap();
        let cfg = cfg.cleaned();
        assert_eq!(cfg.interval, 30);
        assert_eq!(cfg.warnings, vec![10, 3]);
        assert_eq!(cfg.remove, vec!["item"]);
        let off = Config { interval: 0, ..Config::default() }.cleaned();
        assert_eq!(off.interval, 0);
        assert!(off.warnings.is_empty());
    }

    #[test]
    fn chunk_limit_keeps_the_oldest() {
        let cfg = Config { chunk_limit: 2, ..Config::default() }.cleaned();
        let seen = [
            e("cow", false, 10, (0, 0)),
            e("cow", false, 50, (0, 0)),
            e("cow", false, 30, (0, 0)),
            e("cow", true, 1, (0, 0)),
            e("cow", false, 5, (1, 0)),
            e("villager", false, 5, (0, 0)),
            e("pig", false, 40, (0, 0)),
        ];
        // Chunk (0,0) holds cows aged 10, 50, 30 and a pig aged 40: the two youngest go
        assert_eq!(pick_over_limit(&cfg, &seen), vec![0, 2]);
        assert!(pick_over_limit(&Config::default(), &seen).is_empty());
    }

    #[test]
    fn mob_cap_culls_the_youngest_wild_ones() {
        let cfg = Config { mob_cap: 2, ..Config::default() }.cleaned();
        let mut seen = vec![
            e("zombie", false, 100, (0, 0)),
            e("cod", false, 5, (9, 9)),
            e("creeper", false, 50, (3, 3)),
            e("zombie", true, 1, (0, 0)),
            e("cow", false, 1, (0, 0)),
            e("villager", false, 1, (0, 0)),
            e("bat", false, 70, (0, 0)),
        ];
        // Wild and unnamed: zombie 100, cod 5, creeper 50, bat 70. The two oldest stay.
        assert_eq!(pick_over_cap(&cfg, &seen), vec![1, 2]);
        // Each world has its own cap
        seen.push(Seen { world: 1, ..e("zombie", false, 1, (0, 0)) });
        assert_eq!(pick_over_cap(&cfg, &seen), vec![1, 2]);
        assert!(pick_over_cap(&Config::default(), &seen).is_empty());
        assert!(is_wild("skeleton") && is_wild("squid") && !is_wild("wolf") && !is_wild("horse") && !is_wild("iron_golem"));
    }

    #[test]
    fn hostile_and_warnings() {
        let seen = [e("zombie", false, 1, (0, 0)), e("zombie", true, 1, (0, 0)), e("cow", false, 1, (0, 0)), e("creeper", false, 1, (0, 0))];
        assert_eq!(pick_hostile(&seen), vec![0, 3]);
        let cfg = Config::default().cleaned();
        assert_eq!(due_warning(&cfg, 61, 60), Some(60));
        assert_eq!(due_warning(&cfg, 60, 59), None);
        assert_eq!(due_warning(&cfg, 31, 29), Some(30));
        // A long stall that jumps several marks only sends the nearest one
        assert_eq!(due_warning(&cfg, 100, 4), Some(5));
        assert_eq!(due_warning(&cfg, 4, 3), None);
        assert_eq!(snake("EntityType::ExperienceOrb"), "experience_orb");
        assert_eq!(kind_key(" Minecraft:Item "), "item");
    }
}

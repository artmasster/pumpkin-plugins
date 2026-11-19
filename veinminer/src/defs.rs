//! VeinMiner's rules: which blocks go together, which tool fits, how far a vein reaches and what
//! the tool can take. No server needed here.
use std::collections::{HashSet, VecDeque};

use serde::{Deserialize, Serialize};

#[path = "../../common/src/names.rs"]
mod names;
pub use names::default_drop;

pub const NS: &str = "veinminer";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// Most blocks one break takes with it.
    pub max_blocks: u32,
    /// Only while sneaking.
    pub sneak: bool,
    pub ores: bool,
    pub logs: bool,
    /// Each extra block wears the tool; a tool about to break stops the vein.
    pub wear_tool: bool,
    /// More blocks the owner wants to take whole (`glowstone`, `"*_leaves"`).
    pub extra: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), max_blocks: 64, sneak: true, ores: true, logs: true, wear_tool: true, extra: Vec::new() }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.max_blocks = self.max_blocks.clamp(2, 256);
        self.extra = self.extra.iter().map(|e| names::bare(e)).filter(|e| !e.is_empty()).take(100).collect();
        self
    }

    pub fn thai(&self) -> bool {
        self.language == "th"
    }

    /// What kind of vein a block starts, if any.
    pub fn family(&self, block: &str) -> Option<Family> {
        let ore = block.strip_prefix("deepslate_").unwrap_or(block);
        if self.ores && (ore.ends_with("_ore") || block == "ancient_debris") {
            return Some(Family::Ore(ore.to_owned()));
        }
        if self.logs && (block.ends_with("_log") || block.ends_with("_stem")) {
            return Some(Family::Log(block.to_owned()));
        }
        self.extra.iter().any(|p| names::matches(p, block)).then(|| Family::Other(block.to_owned()))
    }
}

/// Blocks of one family go in one vein: an ore with its deepslate form, a log with logs of its
/// own wood.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Family {
    Ore(String),
    Log(String),
    Other(String),
}

impl Family {
    pub fn tool_fits(&self, tool: &str) -> bool {
        let tool = tool.strip_prefix("minecraft:").unwrap_or(tool);
        match self {
            Family::Ore(_) => tool.ends_with("_pickaxe"),
            Family::Log(_) => tool.ends_with("_axe"),
            Family::Other(_) => tool.ends_with("_pickaxe") || tool.ends_with("_axe") || tool.ends_with("_shovel") || tool.ends_with("_hoe"),
        }
    }
}

/// The most damage a tool takes before it breaks.
pub fn max_damage(tool: &str) -> Option<u32> {
    let tool = tool.strip_prefix("minecraft:").unwrap_or(tool);
    let material = tool.split('_').next()?;
    Some(match material {
        "wooden" => 59,
        "stone" => 131,
        "iron" => 250,
        "golden" => 32,
        "diamond" => 1561,
        "netherite" => 2031,
        "copper" => 190,
        _ => return None,
    })
}

/// The blocks of a vein from `start` (not counted itself), up to `max`, nearest first.
/// `same(pos)` says whether a block belongs to it.
pub fn vein(start: (i32, i32, i32), max: usize, same: &mut dyn FnMut((i32, i32, i32)) -> bool) -> Vec<(i32, i32, i32)> {
    let mut seen: HashSet<(i32, i32, i32)> = HashSet::from([start]);
    let mut queue = VecDeque::from([start]);
    let mut out = Vec::new();
    while let Some(at) = queue.pop_front() {
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let next = (at.0 + dx, at.1 + dy, at.2 + dz);
                    if out.len() >= max || !seen.insert(next) {
                        continue;
                    }
                    if same(next) {
                        out.push(next);
                        queue.push_back(next);
                    }
                }
            }
        }
        if out.len() >= max {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_and_tools() {
        let cfg = Config::default();
        assert_eq!(cfg.family("deepslate_iron_ore"), Some(Family::Ore("iron_ore".into())));
        assert_eq!(cfg.family("iron_ore"), cfg.family("deepslate_iron_ore"));
        assert_eq!(cfg.family("oak_log"), Some(Family::Log("oak_log".into())));
        assert_ne!(cfg.family("oak_log"), cfg.family("birch_log"));
        assert_eq!(cfg.family("stone"), None);
        let more = Config { extra: vec!["glowstone".into()], ..Config::default() }.cleaned();
        assert_eq!(more.family("glowstone"), Some(Family::Other("glowstone".into())));
        assert!(Family::Ore(String::new()).tool_fits("minecraft:iron_pickaxe"));
        assert!(!Family::Ore(String::new()).tool_fits("minecraft:iron_axe"));
        assert!(Family::Log(String::new()).tool_fits("minecraft:diamond_axe"));
        assert_eq!(max_damage("minecraft:diamond_pickaxe"), Some(1561));
        assert_eq!(max_damage("minecraft:stick"), None);
    }

    #[test]
    fn a_vein_stops_at_its_size() {
        // A line of 10 blocks along x
        let line = |p: (i32, i32, i32)| p.1 == 0 && p.2 == 0 && (0..10).contains(&p.0);
        assert_eq!(vein((0, 0, 0), 64, &mut |p| line(p)).len(), 9);
        assert_eq!(vein((0, 0, 0), 4, &mut |p| line(p)), vec![(1, 0, 0), (2, 0, 0), (3, 0, 0), (4, 0, 0)]);
        // Diagonal neighbours count
        let diag = |p: (i32, i32, i32)| p == (1, 1, 1) || p == (2, 2, 2);
        assert_eq!(vein((0, 0, 0), 64, &mut |p| diag(p)).len(), 2);
        assert!(vein((0, 0, 0), 64, &mut |_| false).is_empty());
    }
}

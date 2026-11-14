//! Claims, trust and claim blocks. Plain Rust: the plugin asks, this answers.
use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

/// Longest side of one claim.
pub const MAX_SIDE: i32 = 1024;

pub const ACCESS: u8 = 1;
pub const CONTAINER: u8 = 2;
pub const BUILD: u8 = 3;
pub const OWNER: u8 = 4;
pub const PUBLIC: &str = "public";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// Claim blocks every player starts with.
    pub initial_blocks: u32,
    /// Claim blocks earned per hour online.
    pub blocks_per_hour: u32,
    /// Most blocks a player can earn by playing (bonus blocks from admins come on top).
    pub max_accrued: u32,
    pub min_width: i32,
    pub min_area: i64,
    /// Most claims one player may hold (0 = no limit).
    pub max_claims: u32,
    /// Half the side of the square `/claim` makes without a number.
    pub claim_radius: i32,
    /// Item that marks claim corners with a right click.
    pub claim_tool: String,
    /// Item that shows whose land a block is on.
    pub inspect_tool: String,
    /// Dimensions where land can be claimed (empty = everywhere).
    pub worlds: Vec<String>,
    pub protect_explosions: bool,
    pub protect_fire: bool,
    pub protect_fluids: bool,
    pub protect_pistons: bool,
    /// Animals, villagers' pets, boats, item frames and armor stands on claimed land.
    pub protect_entities: bool,
    /// Players may hurt each other on claimed land.
    pub claim_pvp: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: "th".into(),
            initial_blocks: 400,
            blocks_per_hour: 100,
            max_accrued: 50_000,
            min_width: 5,
            min_area: 100,
            max_claims: 0,
            claim_radius: 5,
            claim_tool: "golden_shovel".into(),
            inspect_tool: "stick".into(),
            worlds: Vec::new(),
            protect_explosions: true,
            protect_fire: true,
            protect_fluids: true,
            protect_pistons: true,
            protect_entities: true,
            claim_pvp: false,
        }
    }
}

pub fn short(name: &str) -> &str {
    name.trim().strip_prefix("minecraft:").unwrap_or(name.trim())
}

impl Config {
    pub fn cleaned(mut self) -> Self {
        self.min_width = self.min_width.clamp(1, 64);
        self.min_area = self.min_area.clamp(1, 10_000);
        self.claim_radius = self.claim_radius.clamp(1, 100);
        self.claim_tool = short(&self.claim_tool).to_lowercase();
        self.inspect_tool = short(&self.inspect_tool).to_lowercase();
        self.worlds = self.worlds.iter().map(|w| short(w).to_lowercase()).filter(|w| !w.is_empty()).collect();
        self
    }

    pub fn thai(&self) -> bool {
        self.language != "en"
    }

    pub fn world_ok(&self, dim: &str) -> bool {
        self.worlds.is_empty() || self.worlds.iter().any(|w| w == short(dim))
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Claim {
    pub id: u64,
    /// Owner's player key (uuid as hex).
    pub owner: String,
    pub owner_name: String,
    pub dim: String,
    pub x1: i32,
    pub z1: i32,
    pub x2: i32,
    pub z2: i32,
    /// Lowercase player name (or `public`) to trust level.
    #[serde(default)]
    pub trust: BTreeMap<String, u8>,
    /// The owner allows explosions here.
    #[serde(default)]
    pub explosions: bool,
    #[serde(default)]
    pub created: u64,
}

impl Claim {
    pub fn contains(&self, dim: &str, x: i32, z: i32) -> bool {
        self.dim == dim && self.covers(x, z)
    }

    pub fn covers(&self, x: i32, z: i32) -> bool {
        (self.x1..=self.x2).contains(&x) && (self.z1..=self.z2).contains(&z)
    }

    pub fn width(&self) -> i32 {
        (self.x2 - self.x1 + 1).min(self.z2 - self.z1 + 1)
    }

    pub fn area(&self) -> i64 {
        i64::from(self.x2 - self.x1 + 1) * i64::from(self.z2 - self.z1 + 1)
    }

    /// What this player may do here.
    pub fn level_of(&self, key: &str, lower_name: &str) -> u8 {
        if self.owner == key {
            return OWNER;
        }
        let own = self.trust.get(lower_name).copied().unwrap_or(0);
        own.max(self.trust.get(PUBLIC).copied().unwrap_or(0))
    }

    /// Blocks between this claim and a point, 0 inside.
    pub fn distance(&self, x: i32, z: i32) -> i32 {
        let dx = (self.x1 - x).max(x - self.x2).max(0);
        let dz = (self.z1 - z).max(z - self.z2).max(0);
        dx.max(dz)
    }

    pub fn is_corner(&self, x: i32, z: i32) -> bool {
        (x == self.x1 || x == self.x2) && (z == self.z1 || z == self.z2)
    }
}

#[derive(Debug, PartialEq)]
pub enum Problem {
    World,
    TooNarrow(i32),
    TooSmall(i64),
    TooBig,
    Overlaps(String),
    NeedBlocks(i64),
    TooMany(u32),
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Ledger {
    pub name: String,
    /// Earned by playing.
    #[serde(default)]
    pub accrued: f64,
    /// Given or taken by admins.
    #[serde(default)]
    pub bonus: i64,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Book {
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub claims: Vec<Claim>,
    /// Chunk to the claims touching it. Rebuilt after every change.
    #[serde(skip)]
    index: HashMap<(String, i32, i32), Vec<usize>>,
}

fn order(a: i32, b: i32) -> (i32, i32) {
    if a <= b { (a, b) } else { (b, a) }
}

impl Book {
    pub fn reindex(&mut self) {
        self.index.clear();
        for (i, c) in self.claims.iter().enumerate() {
            for cx in (c.x1 >> 4)..=(c.x2 >> 4) {
                for cz in (c.z1 >> 4)..=(c.z2 >> 4) {
                    self.index.entry((c.dim.clone(), cx, cz)).or_default().push(i);
                }
            }
        }
    }

    pub fn at(&self, dim: &str, x: i32, z: i32) -> Option<&Claim> {
        // Avoid allocating a key for every block event: most chunks have no claim at all
        if self.claims.is_empty() {
            return None;
        }
        self.index.get(&(dim.to_owned(), x >> 4, z >> 4))?.iter().map(|i| &self.claims[*i]).find(|c| c.covers(x, z))
    }

    pub fn at_id(&self, dim: &str, x: i32, z: i32) -> Option<u64> {
        self.at(dim, x, z).map(|c| c.id)
    }

    /// Any claim, in any dimension, within `margin` blocks of this column.
    pub fn near_any(&self, x: i32, z: i32, margin: i32) -> Option<&Claim> {
        self.claims.iter().find(|c| c.distance(x, z) <= margin)
    }

    pub fn used(&self, owner: &str) -> i64 {
        self.claims.iter().filter(|c| c.owner == owner).map(Claim::area).sum()
    }

    pub fn count(&self, owner: &str) -> usize {
        self.claims.iter().filter(|c| c.owner == owner).count()
    }

    fn check(&self, cfg: &Config, dim: &str, rect: (i32, i32, i32, i32), skip: Option<u64>) -> Result<(), Problem> {
        let (x1, z1, x2, z2) = rect;
        if !cfg.world_ok(dim) {
            return Err(Problem::World);
        }
        let (w, d) = (x2 - x1 + 1, z2 - z1 + 1);
        if w > MAX_SIDE || d > MAX_SIDE {
            return Err(Problem::TooBig);
        }
        if w.min(d) < cfg.min_width {
            return Err(Problem::TooNarrow(cfg.min_width));
        }
        if i64::from(w) * i64::from(d) < cfg.min_area {
            return Err(Problem::TooSmall(cfg.min_area));
        }
        if let Some(other) = self.claims.iter().find(|c| Some(c.id) != skip && c.dim == dim && c.x1 <= x2 && c.x2 >= x1 && c.z1 <= z2 && c.z2 >= z1) {
            return Err(Problem::Overlaps(other.owner_name.clone()));
        }
        Ok(())
    }

    /// Make a claim from two corners. `free` = claim blocks the owner has left.
    #[allow(clippy::too_many_arguments)]
    pub fn create(&mut self, cfg: &Config, owner: &str, owner_name: &str, dim: &str, a: (i32, i32), b: (i32, i32), free: i64, now: u64) -> Result<u64, Problem> {
        let ((x1, x2), (z1, z2)) = (order(a.0, b.0), order(a.1, b.1));
        self.check(cfg, dim, (x1, z1, x2, z2), None)?;
        if cfg.max_claims > 0 && self.count(owner) >= cfg.max_claims as usize {
            return Err(Problem::TooMany(cfg.max_claims));
        }
        let area = i64::from(x2 - x1 + 1) * i64::from(z2 - z1 + 1);
        if area > free {
            return Err(Problem::NeedBlocks(area - free));
        }
        self.next_id += 1;
        let id = self.next_id;
        self.claims.push(Claim { id, owner: owner.into(), owner_name: owner_name.into(), dim: dim.into(), x1, z1, x2, z2, trust: BTreeMap::new(), explosions: false, created: now });
        self.reindex();
        Ok(id)
    }

    /// Move one corner of a claim: `fixed` is the opposite corner, `to` the new one.
    pub fn resize(&mut self, cfg: &Config, id: u64, fixed: (i32, i32), to: (i32, i32), free: i64) -> Result<(), Problem> {
        let Some(i) = self.claims.iter().position(|c| c.id == id) else { return Err(Problem::TooBig) };
        let ((x1, x2), (z1, z2)) = (order(fixed.0, to.0), order(fixed.1, to.1));
        let dim = self.claims[i].dim.clone();
        self.check(cfg, &dim, (x1, z1, x2, z2), Some(id))?;
        let grow = i64::from(x2 - x1 + 1) * i64::from(z2 - z1 + 1) - self.claims[i].area();
        if grow > free {
            return Err(Problem::NeedBlocks(grow - free));
        }
        let c = &mut self.claims[i];
        (c.x1, c.z1, c.x2, c.z2) = (x1, z1, x2, z2);
        self.reindex();
        Ok(())
    }

    pub fn remove(&mut self, id: u64) -> Option<Claim> {
        let i = self.claims.iter().position(|c| c.id == id)?;
        let gone = self.claims.remove(i);
        self.reindex();
        Some(gone)
    }

    pub fn remove_all(&mut self, owner: &str) -> usize {
        let before = self.claims.len();
        self.claims.retain(|c| c.owner != owner);
        self.reindex();
        before - self.claims.len()
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Claim> {
        self.claims.iter_mut().find(|c| c.id == id)
    }
}

/// Claim blocks a player has left.
pub fn free_blocks(cfg: &Config, ledger: Option<&Ledger>, used: i64) -> i64 {
    let (accrued, bonus) = ledger.map_or((0.0, 0), |l| (l.accrued, l.bonus));
    i64::from(cfg.initial_blocks) + accrued.floor() as i64 + bonus - used
}

/// One minute online.
pub fn accrue(cfg: &Config, ledger: &mut Ledger) {
    ledger.accrued = (ledger.accrued + f64::from(cfg.blocks_per_hour) / 60.0).min(f64::from(cfg.max_accrued));
}

fn has_word(name: &str, words: &[&str]) -> bool {
    words.iter().any(|w| name.contains(w))
}

/// Trust needed to right-click this block on someone's land (0 = anyone).
pub fn block_needs(block: &str) -> u8 {
    let b = short(block);
    if has_word(b, &["chest", "barrel", "shulker_box", "furnace", "smoker", "hopper", "dispenser", "dropper", "brewing_stand", "lectern", "bookshelf", "crafter", "decorated_pot", "jukebox", "campfire", "beehive", "bee_nest", "composter", "cauldron", "anvil", "beacon", "flower_pot", "vault"]) {
        return CONTAINER;
    }
    if has_word(b, &["door", "fence_gate", "button", "lever", "_bed", "bell", "note_block", "repeater", "comparator", "daylight_detector", "respawn_anchor", "cake", "candle"]) {
        return ACCESS;
    }
    0
}

/// Holding this, a right click can change the world without placing a block (strip, till, burn, spawn).
pub fn item_changes_world(item: &str) -> bool {
    let i = short(item);
    i.ends_with("_axe")
        || i.ends_with("_hoe")
        || i.ends_with("_shovel")
        || i.ends_with("_spawn_egg")
        || i.ends_with("minecart")
        || i.ends_with("_boat")
        || i.ends_with("_raft")
        || i.ends_with("_dye")
        || matches!(i, "bone_meal" | "flint_and_steel" | "fire_charge" | "shears" | "armor_stand" | "item_frame" | "glow_item_frame" | "painting" | "end_crystal" | "lead" | "honeycomb" | "brush" | "glass_bottle" | "ink_sac" | "glow_ink_sac")
}

/// Monsters: anyone may fight them anywhere.
const HOSTILE: &[&str] = &[
    "blaze", "bogged", "breeze", "cave_spider", "creaking", "creeper", "drowned", "elder_guardian", "ender_dragon", "enderman",
    "endermite", "evoker", "ghast", "giant", "guardian", "hoglin", "husk", "illusioner", "magma_cube", "phantom", "piglin",
    "piglin_brute", "pillager", "ravager", "shulker", "silverfish", "skeleton", "slime", "spider", "stray", "vex", "vindicator",
    "warden", "witch", "wither", "wither_skeleton", "zoglin", "zombie", "zombie_villager", "zombified_piglin", "parched", "camel_husk",
];

/// Trust needed to hit or use this kind of entity on someone's land (0 = anyone).
/// Decorations need build trust, animals and vehicles container trust; monsters and traders are free.
pub fn entity_needs(kind: &str) -> u8 {
    let k = short(kind);
    if HOSTILE.contains(&k) || matches!(k, "villager" | "wandering_trader" | "player") {
        return 0;
    }
    if matches!(k, "item_frame" | "glow_item_frame" | "armor_stand" | "painting" | "end_crystal" | "leash_knot") || k.ends_with("_display") {
        return BUILD;
    }
    // Things nobody owns: arrows on the ground, dropped items, orbs
    if matches!(k, "item" | "experience_orb" | "arrow" | "spectral_arrow" | "trident" | "snowball" | "egg" | "fishing_bobber" | "tnt" | "falling_block") {
        return 0;
    }
    CONTAINER
}

/// `EntityType::ArmorStand` → `armor_stand` (kinds arrive as Rust enum names).
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

/// A bucket pours next to the block it is used on, and Pumpkin 0.2.2 ignores a cancelled
/// `PlayerBucketEmptyEvent`: the right click itself has to be refused, one block around.
pub fn is_bucket(item: &str) -> bool {
    let i = short(item);
    i == "bucket" || i.ends_with("_bucket")
}

/// Positions to light up so a player sees a claim: the corners and a mark every ten blocks.
pub fn outline(c: &Claim) -> Vec<(i32, i32, bool)> {
    let mut out = vec![(c.x1, c.z1, true), (c.x1, c.z2, true), (c.x2, c.z1, true), (c.x2, c.z2, true)];
    let mut x = c.x1 + 10;
    while x < c.x2 {
        out.push((x, c.z1, false));
        out.push((x, c.z2, false));
        x += 10;
    }
    let mut z = c.z1 + 10;
    while z < c.z2 {
        out.push((c.x1, z, false));
        out.push((c.x2, z, false));
        z += 10;
    }
    out.truncate(120);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const OW: &str = "minecraft:overworld";

    fn book_with(cfg: &Config) -> Book {
        let mut b = Book::default();
        b.create(cfg, "amy", "Amy", OW, (0, 0), (19, 19), 1000, 1).unwrap();
        b
    }

    #[test]
    fn create_checks_everything() {
        let cfg = Config::default().cleaned();
        let mut b = book_with(&cfg);
        assert_eq!(b.used("amy"), 400);
        assert_eq!(b.create(&cfg, "bob", "Bob", OW, (15, 15), (40, 40), 10_000, 1), Err(Problem::Overlaps("Amy".into())));
        assert_eq!(b.create(&cfg, "bob", "Bob", OW, (30, 30), (32, 60), 10_000, 1), Err(Problem::TooNarrow(5)));
        assert_eq!(b.create(&cfg, "bob", "Bob", OW, (30, 30), (36, 36), 10_000, 1), Err(Problem::TooSmall(100)));
        assert_eq!(b.create(&cfg, "bob", "Bob", OW, (30, 30), (49, 49), 399, 1), Err(Problem::NeedBlocks(1)));
        assert_eq!(b.create(&cfg, "bob", "Bob", OW, (0, 0), (2000, 20), 1 << 40, 1), Err(Problem::TooBig));
        // Corners in any order, and the same columns in another dimension are free
        let id = b.create(&cfg, "bob", "Bob", "minecraft:the_nether", (19, 19), (0, 0), 400, 1).unwrap();
        assert_eq!(b.claims.iter().find(|c| c.id == id).map(|c| (c.x1, c.z1, c.x2, c.z2)), Some((0, 0, 19, 19)));
        let only = Config { worlds: vec!["minecraft:overworld".into()], max_claims: 1, ..Config::default() }.cleaned();
        assert_eq!(b.create(&only, "cat", "Cat", "minecraft:the_end", (0, 0), (20, 20), 9999, 1), Err(Problem::World));
        assert_eq!(b.create(&only, "amy", "Amy", OW, (100, 100), (120, 120), 9999, 1), Err(Problem::TooMany(1)));
    }

    #[test]
    fn lookup_and_trust() {
        let cfg = Config::default().cleaned();
        let mut b = book_with(&cfg);
        b.create(&cfg, "bob", "Bob", OW, (-40, -40), (-21, -21), 1000, 1).unwrap();
        assert_eq!(b.at(OW, 19, 0).map(|c| c.owner.as_str()), Some("amy"));
        assert_eq!(b.at(OW, -21, -40).map(|c| c.owner.as_str()), Some("bob"));
        assert!(b.at(OW, 20, 0).is_none() && b.at(OW, -1, -1).is_none() && b.at("minecraft:the_nether", 5, 5).is_none());
        let c = b.get_mut(1).unwrap();
        c.trust.insert("bob".into(), CONTAINER);
        assert_eq!(c.level_of("amy", "amy"), OWNER);
        assert_eq!(c.level_of("bobkey", "bob"), CONTAINER);
        assert_eq!(c.level_of("x", "eve"), 0);
        c.trust.insert(PUBLIC.into(), ACCESS);
        assert_eq!(c.level_of("x", "eve"), ACCESS);
        assert_eq!(c.level_of("bobkey", "bob"), CONTAINER);
        assert_eq!((c.distance(5, 5), c.distance(25, 5), c.distance(-3, 30)), (0, 6, 11));
        assert!(b.near_any(25, 5, 6).is_some() && b.near_any(40, 40, 6).is_none());
        // The index survives a save and load
        let json = serde_json::to_string(&b).unwrap();
        let mut again: Book = serde_json::from_str(&json).unwrap();
        assert!(again.at(OW, 5, 5).is_none());
        again.reindex();
        assert_eq!(again.at(OW, 5, 5).map(|c| c.id), Some(1));
    }

    #[test]
    fn resize_and_remove() {
        let cfg = Config::default().cleaned();
        let mut b = book_with(&cfg);
        b.create(&cfg, "bob", "Bob", OW, (30, 0), (49, 19), 1000, 1).unwrap();
        // Growing costs only the difference
        assert_eq!(b.resize(&cfg, 1, (0, 0), (24, 19), 99), Err(Problem::NeedBlocks(1)));
        assert_eq!(b.resize(&cfg, 1, (0, 0), (24, 19), 100), Ok(()));
        assert_eq!(b.used("amy"), 500);
        assert!(b.at(OW, 24, 10).is_some());
        assert_eq!(b.resize(&cfg, 1, (0, 0), (35, 19), 9999), Err(Problem::Overlaps("Bob".into())));
        assert_eq!(b.resize(&cfg, 1, (0, 0), (3, 19), 9999), Err(Problem::TooNarrow(5)));
        // Shrinking gives blocks back
        assert_eq!(b.resize(&cfg, 1, (0, 0), (9, 9), 0), Ok(()));
        assert_eq!(b.used("amy"), 100);
        assert!(b.at(OW, 15, 15).is_none());
        assert!(b.remove(1).is_some() && b.remove(1).is_none());
        assert!(b.at(OW, 5, 5).is_none());
        assert_eq!(b.remove_all("bob"), 1);
        assert!(b.claims.is_empty());
    }

    #[test]
    fn blocks_and_rules() {
        let cfg = Config::default().cleaned();
        let mut l = Ledger { name: "Amy".into(), accrued: 0.0, bonus: 50 };
        assert_eq!(free_blocks(&cfg, None, 0), 400);
        for _ in 0..90 {
            accrue(&cfg, &mut l);
        }
        assert_eq!(free_blocks(&cfg, Some(&l), 400), 200);
        let capped = Config { max_accrued: 10, ..cfg.clone() };
        accrue(&capped, &mut l);
        assert_eq!(l.accrued, 10.0);
        assert_eq!((block_needs("minecraft:chest"), block_needs("oak_door"), block_needs("stone"), block_needs("red_bed")), (CONTAINER, ACCESS, 0, ACCESS));
        assert_eq!((block_needs("blast_furnace"), block_needs("stone_button"), block_needs("bedrock")), (CONTAINER, ACCESS, 0));
        assert!(item_changes_world("minecraft:iron_axe") && item_changes_world("zombie_spawn_egg") && item_changes_world("bone_meal"));
        assert!(!item_changes_world("bread") && !item_changes_world("diamond_sword") && !item_changes_world("stone"));
        assert_eq!((entity_needs("cow"), entity_needs("zombie"), entity_needs("item_frame"), entity_needs("villager")), (CONTAINER, 0, BUILD, 0));
        assert_eq!((entity_needs("minecraft:oak_boat"), entity_needs("armor_stand"), entity_needs("item"), entity_needs("horse")), (CONTAINER, BUILD, 0, CONTAINER));
        assert_eq!(snake("EntityType::GlowItemFrame"), "glow_item_frame");
        assert!(is_bucket("minecraft:lava_bucket") && is_bucket("bucket") && is_bucket("water_bucket") && !is_bucket("bucket_hat"));
        let c = Claim { id: 1, owner: "a".into(), owner_name: "A".into(), dim: OW.into(), x1: 0, z1: 0, x2: 25, z2: 9, trust: BTreeMap::new(), explosions: false, created: 0 };
        let marks = outline(&c);
        assert_eq!(marks.iter().filter(|m| m.2).count(), 4);
        assert!(marks.contains(&(10, 0, false)) && marks.contains(&(20, 9, false)) && marks.len() == 8);
        assert!(c.is_corner(25, 0) && !c.is_corner(10, 0));
    }
}

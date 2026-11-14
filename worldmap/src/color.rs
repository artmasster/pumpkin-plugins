//! What colour a block is on the map.
//!
//! A pixel is one byte: `slot * 3 + shade`. Slots 1..=61 are the vanilla map colours (the server
//! tells us the id of every block), slots above are WorldMap's own (biome tints for grass, leaves and
//! water, two fixed leaf colours and a grey for anything unknown). Shade is 0 dark, 1 normal,
//! 2 bright, with the vanilla map multipliers 180, 220 and 255. Byte 0 is "nothing mapped here".
use crate::mapcolors::VANILLA;

pub const WATER: u8 = 12;
pub const GRASS: u8 = 1;
pub const GRASS_DRY: u8 = 62;
pub const GRASS_SWAMP: u8 = 63;
pub const GRASS_JUNGLE: u8 = 64;
pub const GRASS_COLD: u8 = 65;
pub const GRASS_DARK: u8 = 66;
pub const FOLIAGE: u8 = 67;
pub const FOLIAGE_DRY: u8 = 68;
pub const FOLIAGE_SWAMP: u8 = 69;
pub const FOLIAGE_JUNGLE: u8 = 70;
pub const FOLIAGE_COLD: u8 = 71;
pub const BIRCH: u8 = 72;
pub const SPRUCE: u8 = 73;
pub const WATER_SWAMP: u8 = 74;
pub const WATER_WARM: u8 = 75;
pub const WATER_COLD: u8 = 76;
/// A block whose colour id this table does not know (a newer Minecraft than this build).
pub const UNKNOWN: u8 = 77;
pub const SLOTS: usize = 78;

const EXTRA: [(u8, u8, u8); SLOTS - 62] = [
    (176, 172, 82),  // grass, dry (savanna, desert, badlands)
    (104, 116, 58),  // grass, swamp
    (84, 196, 58),   // grass, jungle
    (122, 174, 140), // grass, cold (taiga, snowy)
    (88, 138, 52),   // grass, dark forest
    (56, 130, 34),   // leaves
    (126, 142, 50),  // leaves, dry
    (80, 106, 40),   // leaves, swamp
    (42, 150, 26),   // leaves, jungle
    (70, 124, 80),   // leaves, cold
    (116, 152, 78),  // birch leaves
    (64, 102, 64),   // spruce leaves
    (84, 110, 96),   // water, swamp
    (62, 150, 232),  // water, warm
    (52, 58, 200),   // water, cold
    (128, 128, 128), // unknown block
];

pub const DARK: u8 = 0;
pub const NORMAL: u8 = 1;
pub const BRIGHT: u8 = 2;
const MULT: [u32; 3] = [180, 220, 255];

/// Base colour of a slot (before shading).
pub fn slot_rgb(slot: u8) -> (u8, u8, u8) {
    let s = slot as usize;
    if s < VANILLA.len() {
        VANILLA[s]
    } else if s < SLOTS {
        EXTRA[s - VANILLA.len()]
    } else {
        (0, 0, 0)
    }
}

/// Pixel byte for a slot and a shade.
pub fn pixel(slot: u8, shade: u8) -> u8 {
    if slot == 0 || slot as usize >= SLOTS { 0 } else { slot * 3 + shade.min(2) }
}

/// RGB of a pixel byte; `None` for "nothing here".
pub fn pixel_rgb(px: u8) -> Option<(u8, u8, u8)> {
    let (slot, shade) = (px / 3, px % 3);
    if slot == 0 || slot as usize >= SLOTS {
        return None;
    }
    let (r, g, b) = slot_rgb(slot);
    let m = MULT[shade as usize];
    let f = |c: u8| (u32::from(c) * m / 255) as u8;
    Some((f(r), f(g), f(b)))
}

/// The whole palette as 256 x `rrggbb`, for readers that do not want to know the rule above.
pub fn palette_hex() -> String {
    let mut out = String::with_capacity(256 * 6);
    for px in 0..=255u8 {
        let (r, g, b) = pixel_rgb(px).unwrap_or((0, 0, 0));
        out.push_str(&format!("{r:02x}{g:02x}{b:02x}"));
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// Not drawn: air, glass, torches... the map shows what is below.
    Clear = 0,
    Solid = 1,
    /// Grass colour of the biome.
    Grass = 2,
    /// Leaf colour of the biome.
    Foliage = 3,
    /// Shaded by depth, coloured by the biome.
    Water = 4,
}

/// What the renderer needs to know about one block state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    pub kind: Kind,
    pub slot: u8,
    /// Plants, carpets, snow layers: drawn, but the ground height is the block below.
    pub low: bool,
}

impl Info {
    pub const CLEAR: Info = Info { kind: Kind::Clear, slot: 0, low: false };

    pub fn pack(self) -> u16 {
        u16::from(self.slot) | (self.kind as u16) << 8 | u16::from(self.low) << 11
    }

    pub fn unpack(v: u16) -> Info {
        let kind = match (v >> 8) & 7 {
            1 => Kind::Solid,
            2 => Kind::Grass,
            3 => Kind::Foliage,
            4 => Kind::Water,
            _ => Kind::Clear,
        };
        Info { kind, slot: (v & 0xff) as u8, low: v & 0x800 != 0 }
    }
}

/// Block names come with and without `minecraft:`.
pub fn bare(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

/// Sort a block state into what the map draws. `map_color` is the vanilla map colour id the
/// server reports for the block; the flags are those of the state.
pub fn classify(name: &str, map_color: u8, is_air: bool, is_liquid: bool, is_solid: bool, waterlogged: bool) -> Info {
    let name = bare(name);
    if is_air {
        return Info::CLEAR;
    }
    let water = Info { kind: Kind::Water, slot: WATER, low: false };
    if matches!(name, "water" | "bubble_column" | "seagrass" | "tall_seagrass" | "kelp" | "kelp_plant") || (waterlogged && !is_solid) {
        return water;
    }
    if map_color == 0 {
        return Info::CLEAR;
    }
    match name {
        "grass_block" => return Info { kind: Kind::Grass, slot: GRASS, low: false },
        "short_grass" | "tall_grass" | "fern" | "large_fern" | "bush" => return Info { kind: Kind::Grass, slot: GRASS, low: true },
        "oak_leaves" | "jungle_leaves" | "acacia_leaves" | "dark_oak_leaves" | "mangrove_leaves" => {
            return Info { kind: Kind::Foliage, slot: FOLIAGE, low: false };
        }
        "vine" => return Info { kind: Kind::Foliage, slot: FOLIAGE, low: true },
        "birch_leaves" => return Info { kind: Kind::Solid, slot: BIRCH, low: false },
        "spruce_leaves" => return Info { kind: Kind::Solid, slot: SPRUCE, low: false },
        _ => {}
    }
    let slot = if (map_color as usize) < VANILLA.len() { map_color } else { UNKNOWN };
    Info { kind: Kind::Solid, slot, low: !is_solid && !is_liquid }
}

/// Biome groups that share a grass and leaf colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Climate {
    Temperate = 0,
    Dry = 1,
    Swamp = 2,
    Jungle = 3,
    Cold = 4,
    Dark = 5,
}

/// Biome groups that share a water colour: 0 normal, 1 swamp, 2 warm, 3 cold.
pub type WaterTint = u8;

fn squash(biome: &str) -> String {
    bare(biome).chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// Climate of a biome, from its name in any spelling (`bamboo_jungle`, `BambooJungle`, `bamboo-jungle`).
pub fn climate(biome: &str) -> Climate {
    let b = squash(biome);
    let has = |words: &[&str]| words.iter().any(|w| b.contains(w));
    if has(&["swamp"]) {
        Climate::Swamp
    } else if has(&["jungle"]) {
        Climate::Jungle
    } else if has(&["badlands", "savanna", "desert"]) {
        Climate::Dry
    } else if has(&["taiga", "snowy", "frozen", "grove", "icespikes", "jaggedpeaks", "cold"]) {
        Climate::Cold
    } else if has(&["darkforest", "palegarden"]) {
        Climate::Dark
    } else {
        Climate::Temperate
    }
}

pub fn water_tint(biome: &str) -> WaterTint {
    let b = squash(biome);
    if b.contains("swamp") {
        1
    } else if b.contains("warmocean") {
        // warm_ocean, lukewarm_ocean, deep_lukewarm_ocean
        2
    } else if b.contains("cold") || b.contains("frozen") {
        3
    } else {
        0
    }
}

/// Climate and water tint in one byte, for a cache per biome.
pub fn biome_class(biome: &str) -> u8 {
    climate(biome) as u8 | water_tint(biome) << 4
}

pub fn grass_slot(class: u8) -> u8 {
    match class & 15 {
        1 => GRASS_DRY,
        2 => GRASS_SWAMP,
        3 => GRASS_JUNGLE,
        4 => GRASS_COLD,
        5 => GRASS_DARK,
        _ => GRASS,
    }
}

pub fn foliage_slot(class: u8) -> u8 {
    match class & 15 {
        1 => FOLIAGE_DRY,
        2 => FOLIAGE_SWAMP,
        3 => FOLIAGE_JUNGLE,
        4 => FOLIAGE_COLD,
        _ => FOLIAGE,
    }
}

pub fn water_slot(class: u8) -> u8 {
    match class >> 4 {
        1 => WATER_SWAMP,
        2 => WATER_WARM,
        3 => WATER_COLD,
        _ => WATER,
    }
}

/// Shade of a land pixel like the vanilla map: brighter when the ground is higher than the
/// column to the north, darker when lower. No northern neighbour known = normal.
pub fn height_shade(height: i32, north: Option<i32>) -> u8 {
    match north {
        Some(n) if height > n => BRIGHT,
        Some(n) if height < n => DARK,
        _ => NORMAL,
    }
}

/// Shade of a water pixel like the vanilla map: by depth (blocks of water in the column), with a
/// checkerboard offset (`odd` = x + z is odd) so the steps between depths look dithered.
pub fn water_shade(depth: i32, odd: bool) -> u8 {
    let t = depth + if odd { 2 } else { 0 };
    if t < 5 {
        BRIGHT
    } else if t > 9 {
        DARK
    } else {
        NORMAL
    }
}

/// The two depths at which `water_shade` changes, so the renderer looks at two blocks per water
/// column instead of counting them all: `(normal_from, dark_from)`.
pub fn water_steps(odd: bool) -> (i32, i32) {
    if odd { (3, 8) } else { (5, 10) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanilla_table_matches_the_game() {
        assert_eq!(slot_rgb(1), (127, 178, 56)); // grass
        assert_eq!(slot_rgb(12), (64, 64, 255)); // water
        assert_eq!(slot_rgb(11), (112, 112, 112)); // stone
        assert_eq!(slot_rgb(30), (250, 238, 77)); // gold
        assert_eq!(slot_rgb(61), (127, 167, 150)); // glow lichen, the last one
        assert_eq!(slot_rgb(UNKNOWN), (128, 128, 128));
        assert_eq!(slot_rgb(200), (0, 0, 0));
    }

    #[test]
    fn pixels_round_trip_and_shade() {
        assert_eq!(pixel(0, 2), 0);
        assert_eq!(pixel_rgb(0), None);
        assert_eq!(pixel_rgb(1), None);
        assert_eq!(pixel_rgb(2), None);
        let px = pixel(11, BRIGHT);
        assert_eq!(px, 35);
        assert_eq!(pixel_rgb(px), Some((112, 112, 112)));
        assert_eq!(pixel_rgb(pixel(11, NORMAL)), Some((96, 96, 96)));
        assert_eq!(pixel_rgb(pixel(11, DARK)), Some((79, 79, 79)));
        // Every slot fits in a byte and comes back
        for slot in 1..SLOTS as u8 {
            for shade in 0..3 {
                let px = pixel(slot, shade);
                assert_eq!((px / 3, px % 3), (slot, shade));
                assert!(pixel_rgb(px).is_some());
            }
        }
        assert_eq!(pixel(SLOTS as u8, 0), 0);
        assert_eq!(pixel_rgb(255), None);
    }

    #[test]
    fn palette_lists_every_byte() {
        let p = palette_hex();
        assert_eq!(p.len(), 256 * 6);
        assert_eq!(&p[0..6], "000000");
        let at = pixel(GRASS, BRIGHT) as usize * 6;
        assert_eq!(&p[at..at + 6], "7fb238");
    }

    #[test]
    fn blocks_are_sorted() {
        let stone = classify("minecraft:stone", 11, false, false, true, false);
        assert_eq!(stone, Info { kind: Kind::Solid, slot: 11, low: false });
        assert_eq!(classify("air", 0, true, false, false, false), Info::CLEAR);
        assert_eq!(classify("glass", 0, false, false, true, false).kind, Kind::Clear);
        assert_eq!(classify("torch", 0, false, false, false, false).kind, Kind::Clear);
        assert_eq!(classify("water", 12, false, true, false, false).kind, Kind::Water);
        assert_eq!(classify("minecraft:kelp", 12, false, false, false, false).kind, Kind::Water);
        // A waterlogged plant is water; a waterlogged stair is a stair
        assert_eq!(classify("sea_pickle", 27, false, false, false, true).kind, Kind::Water);
        assert_eq!(classify("iron_bars", 0, false, false, false, true).kind, Kind::Water);
        assert_eq!(classify("oak_stairs", 13, false, false, true, true), Info { kind: Kind::Solid, slot: 13, low: false });
        assert_eq!(classify("lava", 4, false, true, false, false), Info { kind: Kind::Solid, slot: 4, low: false });
        assert_eq!(classify("grass_block", 1, false, false, true, false).kind, Kind::Grass);
        assert_eq!(classify("short_grass", 7, false, false, false, false), Info { kind: Kind::Grass, slot: GRASS, low: true });
        assert_eq!(classify("oak_leaves", 7, false, false, true, false).kind, Kind::Foliage);
        assert_eq!(classify("birch_leaves", 7, false, false, true, false).slot, BIRCH);
        assert_eq!(classify("poppy", 7, false, false, false, false), Info { kind: Kind::Solid, slot: 7, low: true });
        assert_eq!(classify("slime_block", 1, false, false, true, false), Info { kind: Kind::Solid, slot: 1, low: false });
        // A colour id from a newer game is grey, not a crash
        assert_eq!(classify("future_block", 63, false, false, true, false).slot, UNKNOWN);
        assert_eq!(classify("future_block", 250, false, false, true, false).slot, UNKNOWN);
    }

    #[test]
    fn info_packs_into_sixteen_bits() {
        for kind in [Kind::Clear, Kind::Solid, Kind::Grass, Kind::Foliage, Kind::Water] {
            for slot in [0u8, 1, 61, UNKNOWN] {
                for low in [false, true] {
                    let i = Info { kind, slot, low };
                    assert_eq!(Info::unpack(i.pack()), i);
                    assert_ne!(i.pack(), u16::MAX);
                }
            }
        }
    }

    #[test]
    fn biomes_get_a_tint() {
        assert_eq!(climate("plains"), Climate::Temperate);
        assert_eq!(climate("BambooJungle"), Climate::Jungle);
        assert_eq!(climate("minecraft:mangrove_swamp"), Climate::Swamp);
        assert_eq!(climate("wooded-badlands"), Climate::Dry);
        assert_eq!(climate("OldGrowthPineTaiga"), Climate::Cold);
        assert_eq!(climate("dark_forest"), Climate::Dark);
        assert_eq!(water_tint("DeepLukewarmOcean"), 2);
        assert_eq!(water_tint("frozen_river"), 3);
        assert_eq!(water_tint("river"), 0);
        let c = biome_class("Swamp");
        assert_eq!((grass_slot(c), foliage_slot(c), water_slot(c)), (GRASS_SWAMP, FOLIAGE_SWAMP, WATER_SWAMP));
        let c = biome_class("forest");
        assert_eq!((grass_slot(c), foliage_slot(c), water_slot(c)), (GRASS, FOLIAGE, WATER));
        let c = biome_class("ColdOcean");
        assert_eq!((grass_slot(c), water_slot(c)), (GRASS_COLD, WATER_COLD));
    }

    #[test]
    fn land_is_shaded_against_the_north() {
        assert_eq!(height_shade(70, Some(69)), BRIGHT);
        assert_eq!(height_shade(70, Some(70)), NORMAL);
        assert_eq!(height_shade(70, Some(75)), DARK);
        assert_eq!(height_shade(70, None), NORMAL);
        assert_eq!(height_shade(-64, Some(-63)), DARK);
    }

    #[test]
    fn water_is_shaded_by_depth() {
        assert_eq!(water_shade(1, false), BRIGHT);
        assert_eq!(water_shade(4, false), BRIGHT);
        assert_eq!(water_shade(5, false), NORMAL);
        assert_eq!(water_shade(9, false), NORMAL);
        assert_eq!(water_shade(10, false), DARK);
        assert_eq!(water_shade(2, true), BRIGHT);
        assert_eq!(water_shade(3, true), NORMAL);
        assert_eq!(water_shade(8, true), DARK);
        // The two-look shortcut agrees with counting, for every depth
        for odd in [false, true] {
            let (normal_from, dark_from) = water_steps(odd);
            for depth in 1..40 {
                let quick = if depth >= dark_from {
                    DARK
                } else if depth >= normal_from {
                    NORMAL
                } else {
                    BRIGHT
                };
                assert_eq!(quick, water_shade(depth, odd), "depth {depth} odd {odd}");
            }
        }
    }
}

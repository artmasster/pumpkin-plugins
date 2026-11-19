//! Names of blocks, items and creatures as owners write them in their files. Plain Rust: plugins
//! read this file into their own server-free part too (`#[path]`), so the rule lives once.
// Each plugin that reads this file uses some of it
#![allow(dead_code)]

/// `minecraft:stone` → `stone`.
pub fn bare(name: &str) -> String {
    name.strip_prefix("minecraft:").unwrap_or(name).to_ascii_lowercase()
}

/// Does `pattern` from an owner's file match `name`? `*` alone matches all; `*_ore` and
/// `oak_*` match by their end and start.
pub fn matches(pattern: &str, name: &str) -> bool {
    match (pattern.strip_prefix('*'), pattern.strip_suffix('*')) {
        _ if pattern == "*" => true,
        (Some(end), _) => name.ends_with(end),
        (_, Some(start)) => name.starts_with(start),
        _ => pattern == name,
    }
}

/// The game's usual drop of common blocks; others drop themselves.
pub fn default_drop(block: &str) -> String {
    let ore = block.strip_prefix("deepslate_").unwrap_or(block);
    let item = match ore {
        "coal_ore" => "coal",
        "iron_ore" => "raw_iron",
        "copper_ore" => "raw_copper",
        "gold_ore" => "raw_gold",
        "redstone_ore" => "redstone",
        "lapis_ore" => "lapis_lazuli",
        "diamond_ore" => "diamond",
        "emerald_ore" => "emerald",
        "nether_quartz_ore" => "quartz",
        "nether_gold_ore" => "gold_nugget",
        "stone" => "cobblestone",
        "deepslate" => "cobbled_deepslate",
        "grass_block" | "dirt_path" | "farmland" => "dirt",
        "wheat" => "wheat",
        "carrots" => "carrot",
        "potatoes" => "potato",
        "beetroots" => "beetroot",
        "melon" => "melon_slice",
        _ => block,
    };
    format!("minecraft:{item}")
}

fn short(name: &str) -> &str {
    name.trim().strip_prefix("minecraft:").unwrap_or(name.trim())
}

fn has_word(name: &str, words: &[&str]) -> bool {
    words.iter().any(|w| name.contains(w))
}

/// What a right click does to a block: 2 = opens it (a container), 1 = uses it (a door, a
/// button, a bed), 0 = neither.
pub fn block_use(block: &str) -> u8 {
    let b = short(block);
    if has_word(b, &["chest", "barrel", "shulker_box", "furnace", "smoker", "hopper", "dispenser", "dropper", "brewing_stand", "lectern", "bookshelf", "crafter", "decorated_pot", "jukebox", "campfire", "beehive", "bee_nest", "composter", "cauldron", "anvil", "beacon", "flower_pot", "vault"]) {
        return 2;
    }
    if has_word(b, &["door", "fence_gate", "button", "lever", "_bed", "bell", "note_block", "repeater", "comparator", "daylight_detector", "respawn_anchor", "cake", "candle"]) {
        return 1;
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

/// A bucket pours next to the block it is used on, and Pumpkin 0.2.2 ignores a cancelled
/// `PlayerBucketEmptyEvent`: the right click itself has to be refused, one block around.
pub fn is_bucket(item: &str) -> bool {
    let i = short(item);
    i == "bucket" || i.ends_with("_bucket")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_match_names() {
        assert!(matches("*", "stone"));
        assert!(matches("*_ore", "deepslate_iron_ore"));
        assert!(!matches("*_ore", "stone"));
        assert!(matches("oak_*", "oak_log"));
        assert!(matches("stone", "stone"));
        assert!(!matches("stone", "stone_bricks"));
        assert_eq!(bare("minecraft:Stone"), "stone");
        assert_eq!(default_drop("deepslate_iron_ore"), "minecraft:raw_iron");
        assert_eq!(default_drop("oak_log"), "minecraft:oak_log");
    }
}

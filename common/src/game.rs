//! Helpers many plugins need around players, items, money and the entities they make.
use pumpkin_plugin_api::common::{GameMode, Hand};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::text::TextComponent;
use pumpkin_plugin_api::uuid::Uuid;
use pumpkin_plugin_api::wit::pumpkin::plugin::common::{NbtTag, NbtTree};
use pumpkin_plugin_api::wit::pumpkin::plugin::world::{Entity, World};
use pumpkin_plugin_api::{ItemStack, Server};

/// A player's UUID as 32 hex digits: the key plugins keep players under.
pub fn key(p: &Player) -> String {
    key_of_uuid(&p.get_id())
}

pub fn key_of_uuid(id: &Uuid) -> String {
    format!("{:016x}{:016x}", id.high, id.low)
}

/// The UUID of a key written by [`key`]; dashes are allowed.
pub fn uuid_of_key(k: &str) -> Option<Uuid> {
    let hex: String = k.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(Uuid { high: u64::from_str_radix(&hex[..16], 16).ok()?, low: u64::from_str_radix(&hex[16..], 16).ok()? })
}

/// The online player with that key.
pub fn online(server: &Server, k: &str) -> Option<Player> {
    uuid_of_key(k).and_then(|u| server.get_player_by_uuid(u))
}

/// The world the server calls `minecraft:overworld` and so on.
pub fn world_named(server: &Server, name: &str) -> Option<World> {
    server.get_all_worlds().into_iter().find(|w| w.get_dimension() == name)
}

/// Squared distance.
pub fn dist2(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)
}

// ---- text for windows -------------------------------------------------------------

/// An item name or lore line with `&` colours, not in italics (the game slants custom names).
pub fn upright(text: &str) -> TextComponent {
    TextComponent::text("").italic(false).add_child(crate::color(text))
}

/// A window button: an item of the game with a name and lore lines.
pub fn button(material: &str, name: &str, lore: &[String]) -> ItemStack {
    let mut stack = ItemStack::new(material, 1);
    if is_empty_stack(&stack) {
        stack = ItemStack::new("minecraft:paper", 1);
    }
    stack.set_custom_name(Some(upright(name)));
    if !lore.is_empty() {
        stack.set_lore(lore.iter().map(|l| upright(l)).collect());
    }
    stack
}

// ---- items --------------------------------------------------------------------------

pub fn is_empty_stack(stack: &ItemStack) -> bool {
    stack.get_count() == 0 || stack.get_registry_key().ends_with("air")
}

pub fn is_empty(stack: &Option<ItemStack>) -> bool {
    stack.as_ref().is_none_or(is_empty_stack)
}

/// `minecraft:wheat_seeds` however the server spells an item's kind.
pub fn kind_of(stack: &ItemStack) -> String {
    let key = stack.get_registry_key();
    if key.contains(':') { key } else { format!("minecraft:{key}") }
}

/// Which item of CustomItems a stack is.
pub fn customitem_of(stack: &ItemStack) -> Option<String> {
    let tree = stack.get_custom_data("customitems", "id")?;
    match tree.tags.get(tree.root as usize) {
        Some(NbtTag::StringTag(id)) => Some(id.clone()),
        _ => None,
    }
}

/// A plain item of the game: no name, no enchantments, no other data.
pub fn is_plain(stack: &ItemStack) -> bool {
    stack.get_custom_name().is_none() && stack.get_enchantments().is_empty() && stack.get_components().is_empty()
}

/// Free slots of the 36 in a player's inventory.
pub fn empty_slots(p: &Player) -> usize {
    (0u8..36).filter(|s| is_empty(&p.get_inventory_item(*s))).count()
}

/// Put plain items of the game into a player's inventory: their stacks first, then empty
/// slots. Returns how many found no room.
pub fn give_game(p: &Player, name: &str, count: u32) -> u32 {
    let max = u32::from(ItemStack::new(name, 1).get_max_count().max(1));
    let mut left = count;
    let slots: Vec<Option<ItemStack>> = (0u8..36).map(|s| p.get_inventory_item(s)).collect();
    for (slot, have) in slots.iter().enumerate() {
        let Some(stack) = have.as_ref().filter(|s| kind_of(s) == name && is_plain(s)) else { continue };
        let room = max.saturating_sub(u32::from(stack.get_count())).min(left);
        if room > 0 {
            p.set_inventory_item(slot as u8, Some(ItemStack::new(name, (u32::from(stack.get_count()) + room) as u8)));
            left -= room;
        }
    }
    for (slot, have) in slots.iter().enumerate() {
        if left == 0 {
            break;
        }
        if is_empty(have) {
            let add = max.min(left);
            p.set_inventory_item(slot as u8, Some(ItemStack::new(name, add as u8)));
            left -= add;
        }
    }
    left
}

/// Put one whole stack (with all its data) into the first empty slot. False = no room.
pub fn give_stack(p: &Player, stack: ItemStack) -> bool {
    match (0u8..36).find(|s| is_empty(&p.get_inventory_item(*s))) {
        Some(slot) => {
            p.set_inventory_item(slot, Some(stack));
            true
        }
        None => false,
    }
}

/// An item of the game (`minecraft:…`) or of CustomItems (its id) into a player's inventory.
/// CustomItems is asked by message; `false` when it did not answer (not installed or busy).
pub fn give_item(p: &Player, item: &str, count: u32) -> bool {
    if count == 0 {
        return true;
    }
    if item.starts_with("minecraft:") {
        give_game(p, item, count);
        return true;
    }
    crate::ipc("customitems", &serde_json::json!({ "kind": "give", "player": key(p), "item": item, "count": count })).is_some()
}

/// Take one of what the player holds in their main hand (a creative player keeps it).
pub fn use_one_in_hand(p: &Player) {
    if matches!(p.get_gamemode(), GameMode::Creative) {
        return;
    }
    let inv = p.get_inventory();
    if let Some(stack) = inv.get_item_in_hand(Hand::Right) {
        let n = stack.get_count();
        if n > 1 {
            stack.set_count(n - 1);
            inv.set_item_in_hand(Hand::Right, Some(stack));
        } else {
            inv.set_item_in_hand(Hand::Right, None);
        }
    }
}

/// The item's custom data `namespace:key` when it is a string.
pub fn string_data(stack: &ItemStack, namespace: &str, data_key: &str) -> Option<String> {
    let tree = stack.get_custom_data(namespace, data_key)?;
    match tree.tags.get(tree.root as usize) {
        Some(NbtTag::StringTag(s)) => Some(s.clone()),
        _ => None,
    }
}

pub fn string_tree(text: &str) -> NbtTree {
    NbtTree { root: 0, tags: vec![NbtTag::StringTag(text.to_owned())] }
}

// ---- money (Basics) --------------------------------------------------------------------

/// Money through the Basics plugin, by player name. `None`: Basics is not there or busy.
pub mod money {
    fn ask(kind: &str, player: &str, amount: f64) -> Option<serde_json::Value> {
        crate::ipc("basics", &serde_json::json!({ "kind": kind, "player": player, "amount": amount }))
    }

    pub fn balance(player: &str) -> Option<f64> {
        ask("balance", player, 0.0)?.get("balance")?.as_f64()
    }

    /// The new balance.
    pub fn deposit(player: &str, amount: f64) -> Option<f64> {
        let v = ask("deposit", player, amount)?;
        v.get("ok")?.as_bool()?.then(|| v.get("balance")?.as_f64())?
    }

    /// `Some(Ok(new balance))`, `Some(Err(balance))` when there is not enough, `None` when
    /// Basics did not answer.
    pub fn withdraw(player: &str, amount: f64) -> Option<Result<f64, f64>> {
        let v = ask("withdraw", player, amount)?;
        let balance = v.get("balance")?.as_f64()?;
        Some(if v.get("ok")?.as_bool()? { Ok(balance) } else { Err(balance) })
    }
}

// ---- entities a plugin makes ----------------------------------------------------------

static RUN: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// What this run of the server writes on the entities plugins make: an entity that carries
/// another value is from an earlier run, and nothing owns it any more.
pub fn run_mark() -> &'static str {
    RUN.get_or_init(|| crate::now_secs().to_string())
}

/// Mark an entity as made by this plugin in this run (custom data `<ns>:part`).
pub fn mark(entity: &Entity, ns: &str) {
    entity.set_custom_data(ns, "part", &string_tree(run_mark()));
}

/// Made by this plugin in an earlier run.
pub fn leftover(entity: &Entity, ns: &str) -> bool {
    match entity.get_custom_data(ns, "part") {
        Some(tree) => !matches!(tree.tags.get(tree.root as usize), Some(NbtTag::StringTag(s)) if s == run_mark()),
        None => false,
    }
}

/// Remove what earlier runs left in every world. Returns how many.
pub fn sweep(server: &Server, ns: &str) -> usize {
    let mut removed = 0;
    for world in server.get_all_worlds() {
        for entity in world.get_entities() {
            if leftover(&entity, ns) {
                entity.remove();
                removed += 1;
            }
        }
    }
    removed
}

/// Remove what earlier runs left right around a place (its chunk is loaded now).
pub fn sweep_near(entity: &Entity, ns: &str, reach: f64) {
    for other in entity.get_nearby_entities(reach, reach, reach) {
        if leftover(&other, ns) {
            other.remove();
        }
    }
}

// ---- land of the Claims plugin ----------------------------------------------------------

/// May this player change the world at this column, by the Claims plugin? `need` is `build`,
/// `container` or `access`. `Err(owner)` when the land is someone else's; `Ok` when Claims is
/// not installed or did not answer (it is busy only while it runs its own handler).
pub fn may(p: &Player, x: f64, z: f64, need: &str) -> Result<(), String> {
    let answer = crate::ipc("claims", &serde_json::json!({ "kind": "may", "player": key(p), "name": p.get_name(), "world": p.get_world().get_dimension(), "x": x, "z": z, "need": need }));
    match answer {
        Some(v) if v.get("allowed").and_then(serde_json::Value::as_bool) == Some(false) => Err(v.get("owner").and_then(serde_json::Value::as_str).unwrap_or("?").to_owned()),
        _ => Ok(()),
    }
}

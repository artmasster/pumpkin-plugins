//! Pumpkin WASM glue for Cosmetics: the wardrobe window, what each player chose, and asking the
//! Models plugin to draw it.
//!
//! What players chose is kept in `players.json` by player UUID. Every second the plugin looks
//! at who is online and makes what Models draws match what they chose and may wear: a player
//! who joins gets their things back, one who lost a permission loses the thing, and a model
//! Models had to take off (a death, another world) is put on again.
//!
//! A hat that can be one item is worn in the helmet slot: the game draws it with the head
//! from the player's own pose, so it never lags behind a turn. A real helmet goes first: the
//! hat waits until the slot is empty again, and the hat item never leaves the slot. Any other
//! hat and a thing on the back ride the player next to whatever else Models has on them
//! (`extra`). A floating thing (a balloon, a pet) is a model that stands by itself and is
//! pulled after the player every tick, on a lead from their hand or without one.
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::{ClickType, GameMode, Hand};
use pumpkin_plugin_api::data_components::DataComponent;
use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, InventoryClickEvent, InventoryCloseEvent, PlayerLeaveEvent};
use pumpkin_plugin_api::gui::Gui;
use pumpkin_plugin_api::inventory::Inventory;
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::wit::pumpkin::plugin::common::{NbtTag, NbtTree};
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::text::TextComponent;
use pumpkin_plugin_api::{Context, ItemStack, Plugin, PluginMetadata, Screen, Server, register_plugin};

use crate::defs::{self, Config, Cosmetic, Look, NS, Note, Slot};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&dCosmetics&8]&r ";
const PLAYERS_FILE: &str = "players.json";
/// Cosmetics on one page of the wardrobe (the sixth row holds the buttons).
const PAGE: usize = 45;
const PREV_SLOT: i16 = 45;
const TAB_SLOTS: [i16; 3] = [46, 47, 48];
const INFO_SLOT: i16 = 49;
const OFF_SLOT: i16 = 50;
const NEXT_SLOT: i16 = 53;
/// Ticks between two looks at who wears what.
const LOOK_EVERY: u64 = 20;
/// Ticks between two looks through every inventory for a hat item that got out of its slot.
const SWEEP_EVERY: u64 = 100;
/// Where the custom data of a hat item keeps which cosmetic it is.
const ITEM_KEY: &str = "id";

/// What one player chose, by slot.
type Chosen = BTreeMap<Slot, String>;

/// A cosmetic that is on a player now.
struct On {
    cosmetic: String,
    /// The number Models gave it; None once Models said it is gone (put on again soon).
    model: Option<u64>,
    /// A hat worn in the helmet slot: its item model. Models draws nothing for it.
    item: Option<String>,
    /// A floating thing: where it is now.
    floats: Option<Floats>,
}

struct Floats {
    at: (f64, f64, f64),
    yaw: f32,
    world: String,
}

/// What is on one online player.
#[derive(Default)]
struct Worn {
    /// The player's entity: a new one (after a death) needs everything put on again.
    entity: i32,
    on: BTreeMap<Slot, On>,
}

struct Win {
    slot: Slot,
    page: usize,
    inv: Inventory,
}

struct State {
    cfg: Config,
    list: Vec<Cosmetic>,
    notes: Vec<Note>,
    /// By player UUID.
    chosen: BTreeMap<String, Chosen>,
    /// By player UUID, online players only.
    worn: HashMap<String, Worn>,
    /// Who has the wardrobe open, by lowercase name.
    wins: HashMap<String, Win>,
    /// The models the Models plugin has, with the item model of each worn on a head when it
    /// can be one (None: Models is not there, or not asked yet).
    models: Option<BTreeMap<String, Option<String>>>,
    /// Players whose helmet slot may hold a hat item of ours, by UUID.
    capped: std::collections::BTreeSet<String>,
    tick: u64,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Never blocks: WASM has one thread and Pumpkin calls handlers from inside host calls.
fn try_lock() -> Option<std::sync::MutexGuard<'static, Option<State>>> {
    match STATE.try_lock() {
        Ok(g) => Some(g),
        Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

fn msg(th: bool, thai: &str, english: &str) -> String {
    format!("{P}{}", if th { thai } else { english })
}

fn key(p: &Player) -> String {
    let id = p.get_id();
    format!("{:016x}{:016x}", id.high, id.low)
}

// ---- the owner's files ------------------------------------------------------------

fn load() -> (Config, Vec<Cosmetic>, Vec<Note>) {
    let cfg = c::load_config::<Config>("config.toml").cleaned();
    let mut notes = Vec::new();
    let first = !c::data_path("cosmetics").exists();
    let _ = std::fs::create_dir_all(c::data_path("cosmetics"));
    if first {
        let _ = std::fs::write(c::data_path("cosmetics/example.toml"), defs::EXAMPLE);
    }
    let mut names: Vec<String> = std::fs::read_dir(c::data_path("cosmetics")).map(|dir| dir.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.ends_with(".toml")).collect()).unwrap_or_default();
    names.sort();
    let mut all = Vec::new();
    for name in names {
        let file = format!("cosmetics/{name}");
        match std::fs::read_to_string(c::data_path(&file)) {
            Ok(text) => all.extend(defs::parse_cosmetics(&file, &text, cfg.permission_by_default, &mut notes)),
            Err(_) => notes.push(Note::new(&file, "อ่านไฟล์ไม่ได้ (ต้องเป็นข้อความ UTF-8)", "cannot be read (it must be UTF-8 text)")),
        }
    }
    let list = defs::merge_cosmetics(all, &mut notes);
    (cfg, list, notes)
}

/// Ask the Models plugin which models it has, and say which cosmetics name one it does not have.
fn ask_models(st: &mut State) {
    let answer = c::ipc("models", &serde_json::json!({ "kind": "models" }));
    st.models = answer.as_ref().and_then(|v| v.get("models")).and_then(serde_json::Value::as_array).map(|list| {
        list.iter().filter_map(|m| Some((m.get("name")?.as_str()?.to_owned(), m.get("worn").and_then(serde_json::Value::as_str).map(str::to_owned)))).collect()
    });
    st.notes.retain(|n| !n.en.contains("Models"));
    match &st.models {
        None => st.notes.push(Note::new("cosmetics", "ต้องลงปลั๊กอิน Models ด้วย ไม่งั้นไม่มีอะไรแสดง", "the Models plugin is needed: without it nothing is drawn")),
        Some(have) => {
            for cosmetic in &st.list {
                if let Look::Model(name) = &cosmetic.look
                    && !have.contains_key(name)
                {
                    st.notes.push(Note::new(&cosmetic.id, format!("ปลั๊กอิน Models ไม่มีโมเดล {name}"), format!("the Models plugin has no model {name}")));
                }
            }
        }
    }
}

fn find<'a>(st: &'a State, id: &str) -> Option<&'a Cosmetic> {
    st.list.iter().find(|c| c.id == id)
}

fn may_wear(p: &Player, cosmetic: &Cosmetic) -> bool {
    cosmetic.permission.as_ref().is_none_or(|node| p.has_permission(node))
}

/// Models can draw it now.
fn drawable(st: &State, cosmetic: &Cosmetic) -> bool {
    match (&st.models, &cosmetic.look) {
        (None, _) => false,
        (Some(have), Look::Model(name)) => have.contains_key(name),
        (Some(_), Look::Item(_)) => true,
    }
}

/// The item model a hat is worn as in the helmet slot, when it can be one: an item, or a model
/// Models can make one item of and that has no animation to run.
fn hat_item(st: &State, cosmetic: &Cosmetic) -> Option<String> {
    if cosmetic.slot != Slot::Hat {
        return None;
    }
    match &cosmetic.look {
        Look::Item(model) => Some(model.clone()),
        Look::Model(name) if cosmetic.animation.is_none() => st.models.as_ref()?.get(name)?.clone(),
        Look::Model(_) => None,
    }
}

/// Which cosmetic a hat item of ours is.
fn ours(stack: &ItemStack) -> Option<String> {
    let tree = stack.get_custom_data(NS, ITEM_KEY)?;
    match tree.tags.get(tree.root as usize) {
        Some(NbtTag::StringTag(id)) => Some(id.clone()),
        _ => None,
    }
}

fn is_ours(stack: &Option<ItemStack>) -> bool {
    stack.as_ref().is_some_and(|s| ours(s).is_some())
}

fn is_empty(stack: &Option<ItemStack>) -> bool {
    stack.as_ref().is_none_or(|s| s.get_count() == 0 || s.get_registry_key().ends_with("air"))
}

/// The hat as an item for the helmet slot: paper that looks like the hat and says what it is.
fn worn_item(st: &State, cosmetic: &Cosmetic, model: &str) -> ItemStack {
    let th = st.cfg.thai();
    let stack = modelled(model);
    stack.set_custom_name(Some(upright(&cosmetic.name)));
    let mut lore: Vec<TextComponent> = cosmetic.lore.iter().map(|l| upright(l)).collect();
    lore.push(upright(if th { "&7ของแต่งตัว: เปลี่ยนที่ /cosmetics" } else { "&7A cosmetic: change it with /cosmetics" }));
    stack.set_lore(lore);
    stack.set_custom_data(NS, ITEM_KEY, &NbtTree { root: 0, tags: vec![NbtTag::StringTag(cosmetic.id.clone())] });
    stack
}

/// Something a player puts on their head that our hat item should make way for.
fn helmet_like(stack: &ItemStack) -> bool {
    let key = stack.get_registry_key();
    key.ends_with("_helmet") || key.ends_with("_head") || key.ends_with("_skull") || key == "minecraft:carved_pumpkin"
}

/// Make the helmet slot of a player hold the hat item they wear, unless a real helmet is
/// there; take a hat item of ours out of their hand (a helmet put on by hand swaps with it).
/// Not `thorough`: only for a player who wears one or did.
fn keep_head(st: &mut State, p: &Player, thorough: bool) {
    let k = key(p);
    let want = st.worn.get(&k).and_then(|w| w.on.get(&Slot::Hat)).and_then(|on| Some((on.cosmetic.clone(), on.item.clone()?)));
    if !thorough && want.is_none() && !st.capped.contains(&k) {
        return;
    }
    let inv = p.get_inventory();
    if is_ours(&inv.get_item_in_hand(Hand::Right)) {
        inv.set_item_in_hand(Hand::Right, None);
    }
    let head = inv.get_helmet();
    let on_head = head.as_ref().and_then(ours);
    match (&on_head, &want) {
        (Some(id), Some((wanted, _))) if id == wanted => {}
        (Some(_), _) => {
            inv.set_helmet(None);
            if let Some((id, model)) = &want
                && let Some(cosmetic) = find(st, id)
            {
                inv.set_helmet(Some(worn_item(st, cosmetic, model)));
            }
        }
        (None, Some((id, model))) if is_empty(&head) => {
            if let Some(cosmetic) = find(st, id) {
                inv.set_helmet(Some(worn_item(st, cosmetic, model)));
            }
        }
        // A real helmet, or no hat
        _ => {}
    }
    if want.is_some() {
        st.capped.insert(k);
    } else {
        st.capped.remove(&k);
    }
}

/// A hat item of ours anywhere but in a helmet slot (a creative player moved it) is taken away.
fn sweep_items(p: &Player) {
    let inv = p.get_inventory();
    let main = inv.as_inventory();
    for slot in 0..main.get_size() {
        if is_ours(&main.get_item(slot)) {
            main.set_item(slot, None);
        }
    }
    if is_ours(&inv.get_off_hand()) {
        inv.set_off_hand(None);
    }
}

fn hidden(st: &State, p: &Player) -> bool {
    st.cfg.hide_when_invisible && (matches!(p.get_gamemode(), GameMode::Spectator) || p.as_entity().is_invisible())
}

// ---- putting things on and taking them off ------------------------------------------

/// What Models is told about how a cosmetic looks.
fn look_fields(ask: &mut serde_json::Value, cosmetic: &Cosmetic) {
    match &cosmetic.look {
        Look::Model(name) => {
            ask["model"] = name.clone().into();
            if let Some(size) = cosmetic.size {
                ask["scale"] = size.into();
            }
        }
        Look::Item(model) => {
            ask["item"] = model.clone().into();
            ask["offset"] = serde_json::json!(cosmetic.offset);
            ask["turn"] = cosmetic.turn.into();
            if let Some(size) = cosmetic.size {
                ask["size"] = size.into();
            }
            if let Some(display) = &cosmetic.display {
                ask["display"] = display.clone().into();
            }
        }
    }
}

fn put_on(st: &State, p: &Player, cosmetic: &Cosmetic) -> Option<On> {
    if let Some(item) = hat_item(st, cosmetic) {
        // In the helmet slot: see `keep_head`
        return Some(On { cosmetic: cosmetic.id.clone(), model: None, item: Some(item), floats: None });
    }
    let mut floats = None;
    let mut ask = match cosmetic.slot {
        Slot::Hat | Slot::Back => {
            let bone = if cosmetic.slot == Slot::Hat { "head" } else { "body" };
            serde_json::json!({ "kind": "attach", "entity": p.as_entity().get_id(), "extra": true, "hide": false, "bone": bone })
        }
        Slot::Float => {
            let (height, distance, side) = float_place(st, cosmetic);
            let at = defs::float_spot(p.get_position(), p.get_yaw(), height, distance, side);
            let world = p.get_world().get_dimension();
            floats = Some(Floats { at, yaw: p.get_yaw(), world: world.clone() });
            let mut ask = serde_json::json!({ "kind": "spawn", "x": at.0, "y": at.1, "z": at.2, "world": world, "yaw": p.get_yaw() });
            if cosmetic.lead {
                // From the player's hand to the model's feet
                ask["leash"] = key(p).into();
            }
            ask
        }
    };
    look_fields(&mut ask, cosmetic);
    if cosmetic.slot == Slot::Float {
        // The offset says where it floats, not where the item sits in it
        ask["offset"] = serde_json::json!([0.0, 0.0, 0.0]);
    }
    let model = c::ipc("models", &ask)?.get("id")?.as_u64()?;
    if let (Some(animation), Look::Model(_)) = (&cosmetic.animation, &cosmetic.look) {
        let _ = c::ipc("models", &serde_json::json!({ "kind": "play", "id": model, "animation": animation, "mode": "loop" }));
    }
    Some(On { cosmetic: cosmetic.id.clone(), model: Some(model), item: None, floats })
}

/// How high above the feet, how far behind and how far to the right a floating thing wants to be.
fn float_place(st: &State, cosmetic: &Cosmetic) -> (f32, f32, f32) {
    let [x, y, z] = cosmetic.offset;
    (st.cfg.float_height + y, (st.cfg.float_distance + z).max(0.0), 0.5 - x)
}

fn take_off(on: &On) {
    if let Some(model) = on.model {
        let _ = c::ipc("models", &serde_json::json!({ "kind": "remove", "id": model }));
    }
}

/// Make what is on one player match what they chose and may wear.
fn fit(st: &mut State, p: &Player) {
    let k = key(p);
    let entity = p.as_entity().get_id() as i32;
    let world = p.get_world().get_dimension();
    let hide = hidden(st, p);
    let chosen = st.chosen.get(&k).cloned().unwrap_or_default();
    let wanted: BTreeMap<Slot, Cosmetic> = if hide {
        BTreeMap::new()
    } else {
        chosen.iter().filter_map(|(slot, id)| find(st, id).filter(|c| c.slot == *slot && may_wear(p, c) && drawable(st, c)).map(|c| (*slot, c.clone()))).collect()
    };
    let mut worn = st.worn.remove(&k).unwrap_or_default();
    if worn.entity != entity {
        // Another body (after a death): what was on the old one is gone
        for on in worn.on.values() {
            take_off(on);
        }
        worn.on.clear();
        worn.entity = entity;
    }
    for slot in Slot::ALL {
        let want = wanted.get(&slot);
        if let Some(on) = worn.on.get(&slot) {
            // A hat that can be an item now (Models answered late), or no longer can
            let other_way = want.is_some_and(|c| hat_item(st, c) != on.item);
            let moved_world = on.floats.as_ref().is_some_and(|f| f.world != world);
            if want.is_none_or(|c| c.id != on.cosmetic) || (on.model.is_none() && on.item.is_none()) || other_way || moved_world {
                take_off(on);
                worn.on.remove(&slot);
            }
        }
        if let Some(cosmetic) = want
            && !worn.on.contains_key(&slot)
            && let Some(on) = put_on(st, p, cosmetic)
        {
            worn.on.insert(slot, on);
        }
    }
    st.worn.insert(k, worn);
    keep_head(st, p, true);
}

/// Floating things are pulled after their players, every tick, and face the way they go.
fn pull_floats(st: &mut State, players: &[Player]) {
    // A slow bob up and down
    let bob = ((st.tick as f64 * 0.08).sin() * 0.12) as f32;
    for p in players {
        let Some(on) = st.worn.get(&key(p)).and_then(|w| w.on.get(&Slot::Float)) else { continue };
        let Some((height, distance, side)) = st.list.iter().find(|c| c.id == on.cosmetic).map(|c| float_place(st, c)) else { continue };
        let Some(on) = st.worn.get_mut(&key(p)).and_then(|w| w.on.get_mut(&Slot::Float)) else { continue };
        let (Some(model), Some(floats)) = (on.model, on.floats.as_mut()) else { continue };
        let next = defs::float_follow(floats.at, p.get_position(), p.get_yaw(), height + bob, distance, side);
        if next == floats.at {
            continue;
        }
        let (dx, dz) = (next.0 - floats.at.0, next.2 - floats.at.2);
        if dx * dx + dz * dz > 0.0004 {
            // Turns a part of the way each tick, the short way round
            let want = defs::facing(floats.at, next);
            floats.yaw += ((want - floats.yaw + 540.0).rem_euclid(360.0) - 180.0) * 0.3;
        }
        floats.at = next;
        let _ = c::ipc("models", &serde_json::json!({ "kind": "move", "id": model, "x": next.0, "y": next.1, "z": next.2, "yaw": floats.yaw }));
    }
}

fn take_all_off(worn: Worn) {
    for on in worn.on.values() {
        take_off(on);
    }
}

fn save_chosen(st: &State) {
    c::save_json(PLAYERS_FILE, &st.chosen);
}

fn choose(st: &mut State, p: &Player, slot: Slot, id: Option<&str>) {
    let k = key(p);
    let mine = st.chosen.entry(k.clone()).or_default();
    match id {
        Some(id) => {
            mine.insert(slot, id.to_owned());
        }
        None => {
            mine.remove(&slot);
        }
    }
    if mine.is_empty() {
        st.chosen.remove(&k);
    }
    save_chosen(st);
    fit(st, p);
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.tick += 1;
    let tick = st.tick;
    if tick == 20 {
        ask_models(st);
    }
    if tick % LOOK_EVERY == 0 && tick >= 20 {
        let players = server.get_all_players();
        let online: Vec<String> = players.iter().map(key).collect();
        let gone: Vec<String> = st.worn.keys().filter(|k| !online.contains(k)).cloned().collect();
        for k in gone {
            if let Some(worn) = st.worn.remove(&k) {
                take_all_off(worn);
            }
        }
        for p in &players {
            if !c::awaiting_login(p) {
                fit(st, p);
            }
        }
    }
    // Every tick: a hat item swapped into the hand by a helmet, or an empty helmet slot
    else if !st.capped.is_empty() || st.worn.values().any(|w| w.on.get(&Slot::Hat).is_some_and(|on| on.item.is_some())) {
        for p in &server.get_all_players() {
            keep_head(st, p, false);
        }
    }
    if st.worn.values().any(|w| w.on.contains_key(&Slot::Float)) {
        let players = server.get_all_players();
        pull_floats(st, &players);
    }
    if tick % SWEEP_EVERY == 50 {
        for p in &server.get_all_players() {
            sweep_items(p);
        }
    }
}

// ---- the wardrobe ---------------------------------------------------------------------

fn upright(text: &str) -> TextComponent {
    TextComponent::text("").italic(false).add_child(c::color(text))
}

fn varint(mut n: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

fn game_item(name: &str) -> Option<ItemStack> {
    let stack = ItemStack::new(name, 1);
    (!stack.get_registry_key().ends_with("air")).then_some(stack)
}

/// Paper that looks like the item model of that name.
fn modelled(model: &str) -> ItemStack {
    let stack = ItemStack::new("minecraft:paper", 1);
    let mut bytes = varint(model.len() as u32);
    bytes.extend_from_slice(model.as_bytes());
    stack.set_component(DataComponent::ItemModel, &bytes);
    stack
}

fn slot_icon(slot: Slot) -> &'static str {
    match slot {
        Slot::Hat => "minecraft:leather_helmet",
        Slot::Back => "minecraft:leather_chestplate",
        Slot::Float => "minecraft:lead",
    }
}

fn icon(cosmetic: &Cosmetic) -> ItemStack {
    let named = |name: &str| if name.starts_with("minecraft:") { game_item(name) } else { Some(modelled(name)) };
    let stack = match (&cosmetic.icon, &cosmetic.look) {
        (Some(icon), _) => named(&defs::item_model(icon)),
        (None, Look::Item(model)) => named(model),
        (None, Look::Model(_)) => None,
    };
    stack.unwrap_or_else(|| ItemStack::new(slot_icon(cosmetic.slot), 1))
}

fn button(material: &str, name: &str) -> ItemStack {
    let stack = ItemStack::new(material, 1);
    stack.set_custom_name(Some(upright(name)));
    stack
}

/// The cosmetics of a slot this player may wear.
fn usable<'a>(st: &'a State, p: &Player, slot: Slot) -> Vec<&'a Cosmetic> {
    st.list.iter().filter(|c| c.slot == slot && may_wear(p, c)).collect()
}

fn open(st: &mut State, p: &Player, slot: Slot) {
    let th = st.cfg.thai();
    let gui = Gui::new(Screen::Generic9x6, c::color(if th { "&8ตู้เสื้อผ้า" } else { "&8Wardrobe" }));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    let mut win = Win { slot, page: 0, inv: gui.get_inventory() };
    win.page = draw(st, p, &win);
    st.wins.insert(p.get_name().to_ascii_lowercase(), win);
    p.open_gui(gui);
}

/// Fill the wardrobe with its page. Returns the page it is on.
fn draw(st: &State, p: &Player, win: &Win) -> usize {
    let th = st.cfg.thai();
    let list = usable(st, p, win.slot);
    let total = list.len().div_ceil(PAGE).max(1);
    let page = win.page.min(total - 1);
    let wearing = st.chosen.get(&key(p)).and_then(|m| m.get(&win.slot)).cloned();
    let inv = &win.inv;
    inv.clear();
    for (i, cosmetic) in list.iter().skip(page * PAGE).take(PAGE).enumerate() {
        let stack = icon(cosmetic);
        stack.set_custom_name(Some(upright(&cosmetic.name)));
        let mut lore: Vec<TextComponent> = cosmetic.lore.iter().map(|l| upright(l)).collect();
        let on = wearing.as_deref() == Some(cosmetic.id.as_str());
        if !drawable(st, cosmetic) {
            lore.push(upright(if th { "&cยังแสดงไม่ได้ (ดู /cosmetics info)" } else { "&cCannot be drawn yet (see /cosmetics info)" }));
        }
        lore.push(upright(match (on, th) {
            (true, true) => "&aใส่อยู่ &7| คลิกเพื่อถอด",
            (true, false) => "&aWearing &7| click to take it off",
            (false, true) => "&eคลิกเพื่อใส่",
            (false, false) => "&eClick to wear",
        }));
        stack.set_lore(lore);
        if on {
            stack.set_component(DataComponent::EnchantmentGlintOverride, &[1]);
        }
        inv.set_item(i as u32, Some(stack));
    }
    if list.is_empty() {
        inv.set_item(22, Some(button("minecraft:barrier", if th { "&7ยังไม่มีของในหมวดนี้ที่คุณใส่ได้" } else { "&7Nothing here you may wear yet" })));
    }
    for (slot, at) in Slot::ALL.into_iter().zip(TAB_SLOTS) {
        let tab = button(slot_icon(slot), &format!("{}{}", if slot == win.slot { "&a" } else { "&f" }, slot.label(th)));
        if slot == win.slot {
            tab.set_component(DataComponent::EnchantmentGlintOverride, &[1]);
        }
        inv.set_item(at as u32, Some(tab));
    }
    if page > 0 {
        inv.set_item(PREV_SLOT as u32, Some(button("minecraft:arrow", if th { "&eหน้าก่อน" } else { "&ePrevious page" })));
    }
    if page + 1 < total {
        inv.set_item(NEXT_SLOT as u32, Some(button("minecraft:arrow", if th { "&eหน้าถัดไป" } else { "&eNext page" })));
    }
    let book = button("minecraft:book", &format!("&7{} {}/{}", if th { "หน้า" } else { "Page" }, page + 1, total));
    book.add_lore(upright(if th { "&fคลิกของเพื่อใส่ คลิกอีกครั้งเพื่อถอด" } else { "&fClick a thing to wear it, again to take it off" }));
    inv.set_item(INFO_SLOT as u32, Some(book));
    if wearing.is_some() {
        inv.set_item(OFF_SLOT as u32, Some(button("minecraft:barrier", &format!("&c{} {}", if th { "ถอด" } else { "Take off:" }, win.slot.label(th)))));
    }
    page
}

/// A click while the wardrobe is open. True = it was ours (the click is cancelled).
fn on_click(st: &mut State, p: &Player, raw_slot: i16) -> bool {
    let name = p.get_name().to_ascii_lowercase();
    let Some(mut win) = st.wins.remove(&name) else { return false };
    let th = st.cfg.thai();
    let list: Vec<String> = usable(st, p, win.slot).iter().map(|c| c.id.clone()).collect();
    let total = list.len().div_ceil(PAGE).max(1);
    let mut again = true;
    match raw_slot {
        PREV_SLOT if win.page > 0 => win.page -= 1,
        NEXT_SLOT if win.page + 1 < total => win.page += 1,
        OFF_SLOT => choose(st, p, win.slot, None),
        slot if TAB_SLOTS.contains(&slot) => {
            let at = TAB_SLOTS.iter().position(|s| *s == slot).unwrap_or(0);
            (win.slot, win.page) = (Slot::ALL[at], 0);
        }
        slot if (0..PAGE as i16).contains(&slot) => match list.get(win.page * PAGE + slot as usize) {
            Some(id) => {
                let wearing = st.chosen.get(&key(p)).and_then(|m| m.get(&win.slot)) == Some(id);
                if wearing {
                    choose(st, p, win.slot, None);
                } else {
                    let id = id.clone();
                    choose(st, p, win.slot, Some(&id));
                    if hidden(st, p) {
                        c::tell(p, &msg(th, "&7จะเห็นเมื่อคุณไม่ได้ล่องหนหรือเป็นผู้ชม", "&7It shows once you are not invisible or a spectator"));
                    }
                }
            }
            None => again = false,
        },
        _ => again = false,
    }
    if again {
        win.page = draw(st, p, &win);
    }
    st.wins.insert(name, win);
    true
}

// ---- events ------------------------------------------------------------------------

type Data<E> = <E as FromIntoEvent>::Data;

/// The helmet slot in a player's own inventory window.
const HEAD_SLOT: i16 = 5;

/// A hat item of ours never leaves the helmet slot, but makes way for a helmet put there.
/// Pumpkin hands the items of a click only to the first plugin that hears of it, so this goes
/// by the slot: a click on the hat takes the hat off first, so whatever the click does then
/// happens to an empty slot (a helmet goes on, nothing comes out), and the hat comes back the
/// next tick if the slot is still empty. Needs no lock: a click may come while the plugin holds it.
fn guard_hat(e: &Data<InventoryClickEvent>) {
    if e.window_type.is_some() {
        return;
    }
    let inv = e.player.get_inventory();
    // A helmet shift-clicked from the inventory goes to the head
    let shifted = matches!(e.click_type, ClickType::ShiftLeft | ClickType::ShiftRight)
        && match e.raw_slot {
            9..=35 => inv.as_inventory().get_item(e.raw_slot as u32),
            36..=44 => inv.as_inventory().get_item((e.raw_slot - 36) as u32),
            45 => inv.get_off_hand(),
            _ => None,
        }
        .as_ref()
        .is_some_and(helmet_like);
    if (e.raw_slot == HEAD_SLOT || shifted) && is_ours(&inv.get_helmet()) {
        inv.set_helmet(None);
    }
}

struct OnClick;
impl EventHandler<InventoryClickEvent> for OnClick {
    fn handle(&self, _server: Server, mut e: Data<InventoryClickEvent>) -> Data<InventoryClickEvent> {
        if e.cancelled {
            return e;
        }
        guard_hat(&e);
        if !matches!(e.window_type, Some(Screen::Generic9x6)) {
            return e;
        }
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut())
            && !st.wins.is_empty()
            && on_click(st, &e.player, e.raw_slot)
        {
            e.cancelled = true;
        }
        e
    }
}

struct OnClose;
impl EventHandler<InventoryCloseEvent> for OnClose {
    fn handle(&self, _server: Server, e: Data<InventoryCloseEvent>) -> Data<InventoryCloseEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            st.wins.remove(&e.player.get_name().to_ascii_lowercase());
        }
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, e: Data<PlayerLeaveEvent>) -> Data<PlayerLeaveEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            st.wins.remove(&e.player.get_name().to_ascii_lowercase());
            if let Some(worn) = st.worn.remove(&key(&e.player)) {
                take_all_off(worn);
            }
            // Not kept with the player: it comes back when they join
            let inv = e.player.get_inventory();
            if is_ours(&inv.get_helmet()) {
                inv.set_helmet(None);
            }
            st.capped.remove(&key(&e.player));
        }
        e
    }
}

// ---- commands ----------------------------------------------------------------------

fn help(th: bool, admin: bool) -> Vec<String> {
    let mut out = vec![
        msg(th, "&f/cosmetics &7เปิดตู้เสื้อผ้า", "&f/cosmetics &7open the wardrobe"),
        msg(th, "&f/cosmetics wear <ชื่อ> &7| &fremove [hat|back|float|all]", "&f/cosmetics wear <id> &7| &fremove [hat|back|float|all]"),
    ];
    if admin {
        out.push(msg(th, "&f/cosmetics wear <ชื่อ> <ผู้เล่น> &7| &fremove <ช่อง|all> <ผู้เล่น> &7| &flist &7| &finfo &7| &freload &7| &fversion", "&f/cosmetics wear <id> <player> &7| &fremove <slot|all> <player> &7| &flist &7| &finfo &7| &freload &7| &fversion"));
    }
    out
}

fn player_named(server: &Server, name: &str) -> Option<Player> {
    server.get_all_players().into_iter().find(|p| p.get_name().eq_ignore_ascii_case(name))
}

fn run(st: &mut State, sender: &CommandSender, server: &Server, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    let admin = c::has_perm(sender, &format!("{NS}:admin"));
    let me = c::sender_player(sender);
    let no_admin = || vec![msg(th, "&cต้องมีสิทธิ์ cosmetics:admin", "&cThis needs cosmetics:admin")];
    // The player a command is for: the one named (staff only), or the one who typed it
    let target = |at: usize| -> Result<Player, String> {
        match (w.get(at), &me) {
            (Some(name), _) if !admin => Err(msg(th, &format!("&cเปลี่ยนของของ {name} ไม่ได้: ต้องมีสิทธิ์ cosmetics:admin"), &format!("&cYou may not change what {name} wears: it needs cosmetics:admin"))),
            (Some(name), _) => player_named(server, name).ok_or_else(|| msg(th, &format!("&cไม่พบผู้เล่น {name}"), &format!("&cNo player {name} online"))),
            (None, Some(_)) => c::sender_player(sender).ok_or_else(|| msg(th, "&cใส่ชื่อผู้เล่นด้วย", "&cName a player")),
            (None, None) => Err(msg(th, "&cใส่ชื่อผู้เล่นด้วย", "&cName a player")),
        }
    };
    match w.first().map(String::as_str) {
        None => match me {
            Some(p) => {
                open(st, &p, Slot::Hat);
                Vec::new()
            }
            None => help(th, admin),
        },
        Some("version") => vec![format!("Cosmetics {VERSION}")],
        Some("wear") => {
            let Some(id) = w.get(1) else { return help(th, admin) };
            let p = match target(2) {
                Ok(p) => p,
                Err(line) => return vec![line],
            };
            let Some(cosmetic) = find(st, &id.to_ascii_lowercase()).cloned() else { return vec![msg(th, &format!("&cไม่มีของชื่อ {id}"), &format!("&cThere is no cosmetic {id}"))] };
            // Staff choose for a player, but the player still needs the permission to wear it
            if !may_wear(&p, &cosmetic) {
                let node = cosmetic.permission.clone().unwrap_or_default();
                return vec![msg(th, &format!("&c{} ยังใส่ชิ้นนี้ไม่ได้: ต้องมีสิทธิ์ {node}", p.get_name()), &format!("&c{} may not wear this one: it needs {node}", p.get_name()))];
            }
            choose(st, &p, cosmetic.slot, Some(&cosmetic.id));
            vec![msg(th, &format!("&a{} ใส่ {} แล้ว", p.get_name(), cosmetic.name), &format!("&a{} now wears {}", p.get_name(), cosmetic.name))]
        }
        Some("remove") => {
            let which = w.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_else(|| "all".into());
            let slots: Vec<Slot> = match Slot::parse(&which) {
                Some(slot) => vec![slot],
                None if which == "all" => Slot::ALL.to_vec(),
                None => return help(th, admin),
            };
            let p = match target(2) {
                Ok(p) => p,
                Err(line) => return vec![line],
            };
            let what = slots.iter().map(|s| s.label(th)).collect::<Vec<_>>().join(", ");
            for slot in slots {
                choose(st, &p, slot, None);
            }
            vec![msg(th, &format!("&aถอด {what} ของ {} แล้ว", p.get_name()), &format!("&aTook {what} off {}", p.get_name()))]
        }
        Some("list") if admin => {
            if st.list.is_empty() {
                return vec![msg(th, "&7ยังไม่มีของ ดูตัวอย่างใน plugins/data/cosmetics/cosmetics/example.toml แล้วสั่ง /cosmetics reload", "&7No cosmetics yet: see plugins/data/cosmetics/cosmetics/example.toml, then /cosmetics reload")];
            }
            let mut out = vec![msg(th, &format!("&fของแต่งตัว {} ชิ้น", st.list.len()), &format!("&f{} cosmetic(s)", st.list.len()))];
            out.extend(st.list.iter().map(|c| {
                let look = match &c.look {
                    Look::Model(m) => format!("model {m}"),
                    Look::Item(i) => format!("item {i}"),
                };
                format!("&e{} &7{} {look} {}{}", c.id, c.slot.name(), c.name, c.permission.as_ref().map(|n| format!(" &8({n})")).unwrap_or_default())
            }));
            out
        }
        Some("info") if admin => {
            let models = match &st.models {
                Some(list) => format!("Models: {}", list.len()),
                None => if th { "ไม่มีปลั๊กอิน Models".to_owned() } else { "no Models plugin".to_owned() },
            };
            let on: usize = st.worn.values().map(|w| w.on.len()).sum();
            let mut out = vec![msg(th, &format!("&fรุ่น {VERSION} &7ของ {} ชิ้น ผู้เล่นที่เลือกไว้ {} คน ใส่อยู่ตอนนี้ {} ชิ้น, {models}", st.list.len(), st.chosen.len(), on), &format!("&fversion {VERSION} &7{} cosmetic(s), {} player(s) chose some, {on} worn now, {models}", st.list.len(), st.chosen.len()))];
            out.extend(st.notes.iter().take(30).map(|n| format!("&6! &7{}", n.text(th))));
            out
        }
        Some("reload") if admin => {
            let (cfg, list, notes) = load();
            (st.cfg, st.list, st.notes) = (cfg, list, notes);
            ask_models(st);
            // Everything is put on again from the new files at the next look
            for (_, worn) in st.worn.drain() {
                take_all_off(worn);
            }
            let th = st.cfg.thai();
            let mut out = vec![msg(th, &format!("&aอ่านใหม่แล้ว: ของ {} ชิ้น", st.list.len()), &format!("&aRead again: {} cosmetic(s)", st.list.len()))];
            if !st.notes.is_empty() {
                out.push(msg(th, &format!("&6มี {} เรื่องที่ควรดู: /cosmetics info", st.notes.len()), &format!("&6{} thing(s) to look at: /cosmetics info", st.notes.len())));
            }
            out
        }
        Some("list" | "info" | "reload") => no_admin(),
        Some(_) => help(th, admin),
    }
}

struct Cmd;
impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let lines = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            let Some(st) = guard.as_mut() else { return Ok(0) };
            run(st, &sender, &server, &w)
        };
        for line in lines {
            c::reply(&sender, &line);
        }
        Ok(1)
    }
}

// ---- the plugin ------------------------------------------------------------------

struct Cosmetics;

impl Plugin for Cosmetics {
    fn new() -> Self {
        Cosmetics
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Hats, backpacks and floating things players put on themselves".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:use"), description: "Open the wardrobe".into(), default: PermissionDefault::Allow, children: vec![] });
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Manage cosmetics and dress other players".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let (cfg, list, notes) = load();
        let chosen = c::load_json::<BTreeMap<String, Chosen>>(PLAYERS_FILE).unwrap_or_default();
        c::log_info(&format!("{} cosmetic(s), {} player(s) chose some{}", list.len(), chosen.len(), if notes.is_empty() { String::new() } else { format!(", {} thing(s) to look at (/cosmetics info)", notes.len()) }));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg, list, notes, chosen, worn: HashMap::new(), wins: HashMap::new(), models: None, capped: Default::default(), tick: 0 });
        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let cmd = Command::new(&[NS.to_owned(), "cosmetic".to_owned(), "wardrobe".to_owned()], "Cosmetics").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:use"));
        context.register_event_handler(OnClick, EventPriority::Normal, true)?;
        context.register_event_handler(OnClose, EventPriority::Normal, false)?;
        context.register_event_handler(OnLeave, EventPriority::Normal, false)?;
        context.schedule_repeating_task(1, 1, on_tick);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        if let Ok(mut guard) = STATE.lock()
            && let Some(st) = guard.as_mut()
        {
            for (_, worn) in st.worn.drain() {
                take_all_off(worn);
            }
        }
        Ok(())
    }

    /// Other plugins: `{"kind":"cosmetics"}` lists the cosmetics. Messages from Models about
    /// its models: a model that is gone is put on again at the next look.
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let answer = match v.get("kind").and_then(serde_json::Value::as_str).unwrap_or_default() {
            "model_gone" => {
                let id = v.get("id").and_then(serde_json::Value::as_u64);
                if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
                    for on in st.worn.values_mut().flat_map(|w| w.on.values_mut()) {
                        if on.model.is_some() && on.model == id {
                            on.model = None;
                        }
                    }
                }
                serde_json::json!({ "ok": true })
            }
            "model_click" => serde_json::json!({ "ok": true }),
            "cosmetics" => {
                let guard = try_lock().ok_or("busy")?;
                let st = guard.as_ref().ok_or("not loaded")?;
                serde_json::json!({ "ok": true, "cosmetics": st.list.iter().map(|c| serde_json::json!({ "id": c.id, "slot": c.slot.name(), "name": c.name })).collect::<Vec<_>>() })
            }
            other => return Err(format!("unknown kind {other}")),
        };
        serde_json::to_vec(&answer).map_err(|e| e.to_string())
    }
}

register_plugin!(Cosmetics);

//! Pumpkin WASM glue for Crops: planting, growing, picking and drawing the plants.
//!
//! Plants are kept in `plants.json`. A plant near a player (`show-distance`) is drawn: two item
//! displays crossed like the game's own crops (or a model of the Models plugin) and an
//! interaction box that takes the clicks. Farther plants have no entities at all.
//!
//! Every few seconds the plants near players grow by the time since they were last looked at,
//! when their soil is still there (and wet, for a crop that wants water): a plant nobody is near
//! grows by the whole time once someone comes back.
//!
//! Pumpkin cannot drop items into the world from a plugin: what a plant gives goes into the
//! player's inventory, and a player whose inventory has no room is told so and gets nothing yet.
//!
//! A hit on a plant fires no event: the interaction box only notes who hit it last. The swing
//! of the arm that comes with every hit does, and then the boxes near the player are asked.
//! Pumpkin drops hits altogether on a server with PvP off; breaking the soil still works there.
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::{BlockPos, GameMode, Hand};
use pumpkin_plugin_api::data_components::DataComponent;
use pumpkin_plugin_api::events::{BlockBreakEvent, BlockPlaceEvent, EventHandler, EventPriority, FromIntoEvent, InteractAction, MoistureChangeEvent, PlayerAnimationEvent, PlayerInteractEntityEvent, PlayerInteractEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::wit::pumpkin::plugin::common::{NbtTag, NbtTree};
use pumpkin_plugin_api::wit::pumpkin::plugin::display::{DisplayTransformation, ItemDisplayEntity, Quaternionf, Vector3f};
use pumpkin_plugin_api::wit::pumpkin::plugin::event::EntityInteractionAction;
use pumpkin_plugin_api::wit::pumpkin::plugin::world as host_world;
use pumpkin_plugin_api::wit::pumpkin::plugin::world::{BlockFlags, Entity, World};
use pumpkin_plugin_api::{Context, EntityDisplayExt, EntityType, ItemDisplayMode, ItemStack, Plugin, PluginMetadata, Server, register_plugin};

use crate::defs::{self, Config, Crop, ItemRef, Look, NS, Note, Plant};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&aCrops&8]&r ";
const PLANTS_FILE: &str = "plants.json";
/// Ticks between two looks at which plants are near a player.
const LOOK_EVERY: u64 = 20;
/// Ticks between two looks at the soil of the plants near players, when they grow.
const GROW_EVERY: u64 = 100;
const SAVE_EVERY: u64 = 1200;
/// Ticks between two looks through the worlds for entities an earlier run left.
const SWEEP_EVERY: u64 = 6000;
/// The mark on every entity this plugin makes.
const PART: &str = "part";
/// A click on a plant counts once in this many ticks (a click can come as two packets).
const CLICK_TICKS: u64 = 4;
/// How long an item for CustomItems is tried again when it was busy.
const GIFT_TRIES: u32 = 40;

/// A plant's block: world, x, y, z (the block above its soil).
type Key = (String, i32, i32, i32);

struct Shown {
    /// The stage it is drawn at.
    stage: usize,
    displays: Vec<ItemDisplayEntity>,
    entities: Vec<Entity>,
    /// The number Models gave its model.
    model: Option<u64>,
    /// The interaction box and its entity id.
    hit: (Entity, i32),
}

struct Live {
    plant: Plant,
    shown: Option<Shown>,
}

/// A right click on a plant, done at the next tick: Pumpkin looks the clicked entity up again
/// after the handler, and a box removed in it would bring the server down.
struct Act {
    player: String,
    key: Key,
    attack: bool,
}

/// An item of CustomItems that could not be given yet (it was busy): tried again next tick.
struct Gift {
    player: String,
    item: String,
    count: u32,
    tries: u32,
}

struct State {
    cfg: Config,
    list: Vec<Crop>,
    notes: Vec<Note>,
    plants: HashMap<Key, Live>,
    /// Which plant an interaction box belongs to, by its entity id.
    boxes: HashMap<i32, Key>,
    /// The last tick each player clicked a plant, by lowercase name.
    clicks: HashMap<String, u64>,
    gifts: Vec<Gift>,
    acts: Vec<Act>,
    /// Items of CustomItems (None: it is not there, or not asked yet).
    ours: Option<BTreeSet<String>>,
    /// Models of the Models plugin.
    models: Option<BTreeSet<String>>,
    tick: u64,
    dirty: bool,
    random: u64,
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

fn uuid_of(p: &Player) -> String {
    let id = p.get_id();
    format!("{:016x}{:016x}", id.high, id.low)
}

fn key_at(world: &str, pos: &BlockPos) -> Key {
    (world.to_owned(), pos.x, pos.y, pos.z)
}

fn key_of(plant: &Plant) -> Key {
    (plant.world.clone(), plant.x, plant.y, plant.z)
}

fn world_named(server: &Server, name: &str) -> Option<World> {
    server.get_all_worlds().into_iter().find(|w| w.get_dimension() == name)
}

/// `3 นาที 20 วิ`, `3m 20s`.
fn span(th: bool, secs: f64) -> String {
    let s = secs.ceil() as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    let parts: Vec<String> = [(h, if th { " ชม." } else { "h" }), (m, if th { " นาที" } else { "m" }), (s, if th { " วิ" } else { "s" })].iter().filter(|(n, _)| *n > 0).map(|(n, unit)| format!("{n}{unit}")).collect();
    if parts.is_empty() { if th { "0 วิ".into() } else { "0s".into() } } else { parts.join(" ") }
}

// ---- the owner's files ------------------------------------------------------------

fn load() -> (Config, Vec<Crop>, Vec<Note>) {
    let cfg = c::load_config::<Config>("config.toml").cleaned();
    let mut notes = Vec::new();
    let first = !c::data_path("crops").exists();
    let _ = std::fs::create_dir_all(c::data_path("crops"));
    if first {
        let _ = std::fs::write(c::data_path("crops/example.toml"), defs::EXAMPLE);
    }
    let mut names: Vec<String> = std::fs::read_dir(c::data_path("crops")).map(|dir| dir.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.ends_with(".toml")).collect()).unwrap_or_default();
    names.sort();
    let mut all = Vec::new();
    for name in names {
        let file = format!("crops/{name}");
        match std::fs::read_to_string(c::data_path(&file)) {
            Ok(text) => all.extend(defs::parse_crops(&file, &text, &mut notes)),
            Err(_) => notes.push(Note::new(&file, "อ่านไฟล์ไม่ได้ (ต้องเป็นข้อความ UTF-8)", "cannot be read (it must be UTF-8 text)")),
        }
    }
    let list = defs::merge_crops(all, &mut notes);
    (cfg, list, notes)
}

fn load_plants() -> HashMap<Key, Live> {
    let list: Vec<Plant> = c::load_json(PLANTS_FILE).unwrap_or_default();
    list.into_iter().map(|plant| (key_of(&plant), Live { plant, shown: None })).collect()
}

fn save_plants(st: &mut State) {
    let mut list: Vec<&Plant> = st.plants.values().map(|l| &l.plant).collect();
    list.sort_by(|a, b| (&a.world, a.x, a.y, a.z).cmp(&(&b.world, b.x, b.y, b.z)));
    if c::save_json(PLANTS_FILE, &list) {
        st.dirty = false;
    }
}

/// Ask CustomItems and Models what they have, and say what the crops name that is not there.
fn ask_others(st: &mut State) {
    let names = |plugin: &str, kind: &str, field: &str, key: &str| -> Option<BTreeSet<String>> {
        let answer = c::ipc(plugin, &serde_json::json!({ "kind": kind }))?;
        Some(answer.get(field)?.as_array()?.iter().filter_map(|i| i.get(key).and_then(serde_json::Value::as_str).map(str::to_owned)).collect())
    };
    st.ours = names("customitems", "items", "items", "id");
    st.models = names("models", "models", "models", "name");
    st.notes.retain(|n| !n.en.contains("CustomItems") && !n.en.contains("Models"));
    let mut wanted: Vec<(String, String)> = Vec::new();
    let mut models: Vec<(String, String)> = Vec::new();
    for crop in &st.list {
        let mut items: Vec<String> = Vec::new();
        if let ItemRef::Ours(id) = &crop.seed {
            items.push(id.clone());
        }
        items.extend(crop.harvest.iter().filter_map(|d| match &d.item {
            ItemRef::Ours(id) => Some(id.clone()),
            ItemRef::Game(_) => None,
        }));
        items.extend(crop.stages.iter().filter_map(|s| match &s.look {
            Look::Model(m) => m.strip_prefix("customitems:").map(str::to_owned),
            _ => None,
        }));
        items.sort();
        items.dedup();
        wanted.extend(items.into_iter().map(|i| (crop.id.clone(), i)));
        if let Some(model) = &crop.model {
            models.push((crop.id.clone(), model.clone()));
        }
    }
    match &st.ours {
        None if !wanted.is_empty() => st.notes.push(Note::new("crops", "ต้องลงปลั๊กอิน CustomItems ด้วย ผักที่ใช้ไอเท็มของมันจึงจะใช้ได้", "the CustomItems plugin is needed for the crops that use its items")),
        Some(have) => {
            for (crop, item) in wanted.iter().filter(|(_, i)| !have.contains(i)) {
                st.notes.push(Note::new(crop, format!("CustomItems ไม่มีไอเท็ม {item}"), format!("CustomItems has no item {item}")));
            }
        }
        None => {}
    }
    match &st.models {
        None if !models.is_empty() => st.notes.push(Note::new("crops", "ต้องลงปลั๊กอิน Models ด้วย ผักที่ใช้โมเดลจึงจะแสดง", "the Models plugin is needed to draw the crops that use a model")),
        Some(have) => {
            for (crop, model) in models.iter().filter(|(_, m)| !have.contains(m)) {
                st.notes.push(Note::new(crop, format!("ปลั๊กอิน Models ไม่มีโมเดล {model}"), format!("the Models plugin has no model {model}")));
            }
        }
        None => {}
    }
}

fn find<'a>(list: &'a [Crop], id: &str) -> Option<&'a Crop> {
    list.iter().find(|c| c.id == id)
}

// ---- items ----------------------------------------------------------------------------

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

fn is_empty(stack: &Option<ItemStack>) -> bool {
    stack.as_ref().is_none_or(|s| s.get_count() == 0 || s.get_registry_key().ends_with("air"))
}

/// `minecraft:wheat_seeds` however the server spells an item's kind.
fn kind_of(stack: &ItemStack) -> String {
    let key = stack.get_registry_key();
    if key.contains(':') { key } else { format!("minecraft:{key}") }
}

/// Which item of CustomItems a stack is.
fn ours_of(stack: &ItemStack) -> Option<String> {
    let tree = stack.get_custom_data("customitems", "id")?;
    match tree.tags.get(tree.root as usize) {
        Some(NbtTag::StringTag(id)) => Some(id.clone()),
        _ => None,
    }
}

fn is_item(stack: &ItemStack, item: &ItemRef) -> bool {
    match item {
        ItemRef::Ours(id) => ours_of(stack).as_deref() == Some(id.as_str()),
        ItemRef::Game(name) => kind_of(stack) == *name && ours_of(stack).is_none(),
    }
}

/// The item a stage shows.
fn look_stack(look: &Look) -> Option<ItemStack> {
    match look {
        Look::Game(name) => {
            let stack = ItemStack::new(name, 1);
            (!stack.get_registry_key().ends_with("air")).then_some(stack)
        }
        Look::Model(model) => {
            let stack = ItemStack::new("minecraft:paper", 1);
            let mut bytes = varint(model.len() as u32);
            bytes.extend_from_slice(model.as_bytes());
            stack.set_component(DataComponent::ItemModel, &bytes);
            Some(stack)
        }
        Look::Animation(_) => None,
    }
}

fn empty_slots(p: &Player) -> usize {
    (0u8..36).filter(|s| is_empty(&p.get_inventory_item(*s))).count()
}

/// Put plain items of the game into a player's inventory: their stacks first, then empty slots.
fn give_game(p: &Player, name: &str, count: u32) -> u32 {
    let max = u32::from(ItemStack::new(name, 1).get_max_count().max(1));
    let mut left = count;
    let plain = |s: &ItemStack| s.get_custom_name().is_none() && s.get_enchantments().is_empty() && s.get_components().is_empty();
    let slots: Vec<Option<ItemStack>> = (0u8..36).map(|s| p.get_inventory_item(s)).collect();
    for (slot, have) in slots.iter().enumerate() {
        let Some(stack) = have.as_ref().filter(|s| kind_of(s) == name && plain(s)) else { continue };
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

fn give(st: &mut State, p: &Player, item: &ItemRef, count: u32) {
    if count == 0 {
        return;
    }
    match item {
        ItemRef::Game(name) => {
            give_game(p, name, count);
        }
        ItemRef::Ours(id) => {
            if c::ipc("customitems", &serde_json::json!({ "kind": "give", "player": uuid_of(p), "item": id, "count": count })).is_none() {
                st.gifts.push(Gift { player: uuid_of(p), item: id.clone(), count, tries: 0 });
            }
        }
    }
}

/// Take one of what the player holds in their main hand (a creative player keeps it).
fn use_one(p: &Player) {
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

// ---- drawing ----------------------------------------------------------------------

static RUN: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// What this run of the server writes on its entities: an entity that carries another value
/// is from an earlier run, and nothing owns it any more.
fn run_mark() -> &'static str {
    RUN.get_or_init(|| c::now_secs().to_string())
}

fn mark(entity: &Entity) {
    entity.set_custom_data(NS, PART, &NbtTree { root: 0, tags: vec![NbtTag::StringTag(run_mark().to_owned())] });
}

fn leftover(entity: &Entity) -> bool {
    match entity.get_custom_data(NS, PART) {
        Some(tree) => !matches!(tree.tags.get(tree.root as usize), Some(NbtTag::StringTag(s)) if s == run_mark()),
        None => false,
    }
}

fn sweep(server: &Server) -> usize {
    let mut removed = 0;
    for world in server.get_all_worlds() {
        for entity in world.get_entities() {
            if leftover(&entity) {
                entity.remove();
                removed += 1;
            }
        }
    }
    removed
}

/// A sprite stood up in the middle of the block, turned `degrees` about the up axis.
fn crossed(degrees: f32) -> DisplayTransformation {
    let half = degrees.to_radians() / 2.0;
    DisplayTransformation {
        translation: Vector3f { x: 0.0, y: 0.5, z: 0.0 },
        scale: Vector3f { x: 1.0, y: 1.0, z: 1.0 },
        left_rotation: Quaternionf { x: 0.0, y: half.sin(), z: 0.0, w: half.cos() },
        right_rotation: Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
    }
}

fn play_stage(model: u64, crop: &Crop, stage: usize) {
    let _ = c::ipc("models", &serde_json::json!({ "kind": "stop", "id": model }));
    if let Some(Look::Animation(animation)) = crop.stages.get(stage).map(|s| &s.look) {
        let _ = c::ipc("models", &serde_json::json!({ "kind": "play", "id": model, "animation": animation, "mode": "hold", "blend": 0 }));
    }
}

fn show(crop: &Crop, live: &mut Live, world: &World, boxes: &mut HashMap<i32, Key>) {
    let plant = &live.plant;
    let at = (f64::from(plant.x) + 0.5, f64::from(plant.y), f64::from(plant.z) + 0.5);
    let mut entities = Vec::new();
    let mut displays = Vec::new();
    let mut model = None;
    match &crop.model {
        Some(name) => {
            model = c::ipc("models", &serde_json::json!({ "kind": "spawn", "model": name, "x": at.0, "y": at.1, "z": at.2, "world": plant.world, "yaw": 0 })).and_then(|a| a.get("id")?.as_u64());
            if let Some(id) = model {
                play_stage(id, crop, plant.stage);
            }
        }
        None => {
            let look = crop.stages.get(plant.stage).map(|s| &s.look);
            for degrees in [45.0, 135.0] {
                let entity = world.spawn_entity(EntityType::ItemDisplay, at);
                let Some(item) = entity.as_item_display() else {
                    entity.remove();
                    continue;
                };
                mark(&entity);
                item.set_item(look.and_then(look_stack));
                item.set_item_display_mode(ItemDisplayMode::Fixed);
                item.get_display().set_transformation(crossed(degrees));
                displays.push(item);
                entities.push(entity);
            }
        }
    }
    let entity = world.spawn_entity(EntityType::Interaction, at);
    // The chunk is loaded now: what an earlier run left at this very place goes
    for other in entity.get_nearby_entities(0.5, 1.0, 0.5) {
        if leftover(&other) {
            other.remove();
        }
    }
    let id = entity.get_id() as i32;
    if let Some(click) = entity.as_interaction() {
        click.set_width(0.9);
        click.set_height(1.0);
        click.set_response(true);
    }
    mark(&entity);
    boxes.insert(id, key_of(plant));
    live.shown = Some(Shown { stage: plant.stage, displays, entities, model, hit: (entity, id) });
}

fn hide(live: &mut Live, boxes: &mut HashMap<i32, Key>) {
    let Some(shown) = live.shown.take() else { return };
    for entity in &shown.entities {
        entity.remove();
    }
    if let Some(id) = shown.model {
        let _ = c::ipc("models", &serde_json::json!({ "kind": "remove", "id": id }));
    }
    boxes.remove(&shown.hit.1);
    shown.hit.0.remove();
}

/// Make what is drawn match the plant's stage.
fn redraw(crop: &Crop, live: &mut Live) {
    let stage = live.plant.stage;
    let Some(shown) = live.shown.as_mut() else { return };
    if shown.stage == stage {
        return;
    }
    shown.stage = stage;
    if let Some(id) = shown.model {
        play_stage(id, crop, stage);
    }
    let look = crop.stages.get(stage).map(|s| &s.look);
    for item in &shown.displays {
        item.set_item(look.and_then(look_stack));
    }
}

// ---- growing ------------------------------------------------------------------------

/// What is under a plant and in its block: gone (it cannot stay), dry, or fine.
enum Ground {
    Gone,
    Dry,
    Wet,
}

fn ground(world: &World, crop: &Crop, plant: &Plant) -> Ground {
    let below = BlockPos { x: plant.x, y: plant.y - 1, z: plant.z };
    let soil = world.get_block_state(BlockPos { x: below.x, y: below.y, z: below.z });
    let name = soil.block_name.strip_prefix("minecraft:").unwrap_or(&soil.block_name);
    if !crop.soil.iter().any(|s| s == name) || !world.get_block_state(BlockPos { x: plant.x, y: plant.y, z: plant.z }).is_air {
        return Ground::Gone;
    }
    let moisture = soil.properties.iter().find(|(k, _)| k == "moisture").map(|(_, v)| v.as_str());
    if name == "farmland" && moisture == Some("0") {
        keep_farmland(world, below);
    }
    if moisture == Some("7") { Ground::Wet } else { Ground::Dry }
}

/// Farmland that is quite dry turns to dirt on a random tick unless a crop of the game is on it,
/// and a plant of ours is no block. It is kept just short of dry: from 1 the game only dries it
/// further through an event, which keeps it at 1 (`OnMoisture`).
fn keep_farmland(world: &World, at: BlockPos) {
    if let Some(state) = host_world::resolve_block_state("minecraft:farmland", &[("moisture".to_owned(), "1".to_owned())]) {
        world.set_block_state(at, state, BlockFlags::NOTIFY_LISTENERS);
    }
}

/// The plants near players grow by the time since they were last looked at, when their soil
/// lets them; a plant whose soil is gone is gone too.
fn grow_near(st: &mut State, server: &Server) {
    let now = c::now_secs();
    let mut worlds: HashMap<String, Option<World>> = HashMap::new();
    let mut gone = Vec::new();
    for (key, live) in &mut st.plants {
        if live.shown.is_none() {
            continue;
        }
        let Some(crop) = find(&st.list, &live.plant.crop) else { continue };
        let Some(world) = worlds.entry(key.0.clone()).or_insert_with(|| world_named(server, &key.0)).as_ref() else { continue };
        let elapsed = now.saturating_sub(live.plant.seen) as f64;
        live.plant.seen = now;
        match ground(world, crop, &live.plant) {
            Ground::Gone => gone.push(key.clone()),
            Ground::Dry if crop.water => {}
            _ => {
                let (stage, into) = defs::grow(&crop.stages, live.plant.stage, live.plant.into, elapsed);
                (live.plant.stage, live.plant.into) = (stage, into);
                redraw(crop, live);
            }
        }
        st.dirty = true;
    }
    for key in gone {
        if let Some(mut live) = st.plants.remove(&key) {
            hide(&mut live, &mut st.boxes);
        }
    }
}

/// Draw the plants that are near a player now, and take away the entities of the others.
fn look_around(st: &mut State, server: &Server) {
    let players: Vec<(String, (f64, f64, f64))> = server.get_all_players().iter().map(|p| (p.get_world().get_dimension(), p.get_position())).collect();
    let reach = f64::from(st.cfg.show_distance);
    let mut worlds: HashMap<String, Option<World>> = HashMap::new();
    for (key, live) in &mut st.plants {
        let (x, y, z) = (f64::from(key.1) + 0.5, f64::from(key.2), f64::from(key.3) + 0.5);
        // A little further to leave than to come, so a player at the edge does not flicker it
        let keep = if live.shown.is_some() { reach + 8.0 } else { reach };
        let near = players.iter().any(|(w, p)| *w == key.0 && (p.0 - x).powi(2) + (p.1 - y).powi(2) + (p.2 - z).powi(2) <= keep * keep);
        match (near, live.shown.is_some()) {
            (true, false) => {
                let Some(crop) = find(&st.list, &live.plant.crop) else { continue };
                let Some(world) = worlds.entry(key.0.clone()).or_insert_with(|| world_named(server, &key.0)).as_ref() else { continue };
                show(crop, live, world, &mut st.boxes);
            }
            (false, true) => hide(live, &mut st.boxes),
            _ => {}
        }
    }
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.tick += 1;
    let tick = st.tick;
    if tick == 20 {
        ask_others(st);
    }
    if tick == 40 || tick % SWEEP_EVERY == 0 {
        // Before anything is drawn: what is there now is from an earlier run
        let n = sweep(&server);
        if n > 0 {
            c::log_info(&format!("removed {n} entit(ies) that an earlier run left in the world"));
        }
    }
    if tick >= 40 && tick % LOOK_EVERY == 0 {
        look_around(st, &server);
    }
    if tick >= 40 && tick % GROW_EVERY == 0 {
        grow_near(st, &server);
    }
    if st.dirty && tick % SAVE_EVERY == 0 {
        save_plants(st);
    }
    if !st.acts.is_empty() {
        let players = server.get_all_players();
        for act in std::mem::take(&mut st.acts) {
            let Some(p) = players.iter().find(|p| uuid_of(p) == act.player) else { continue };
            let ripe = st.plants.get(&act.key).and_then(|l| find(&st.list, &l.plant.crop).map(|c| c.ripe(l.plant.stage)));
            match ripe {
                None => {}
                Some(_) if act.attack => {
                    take(st, p, &act.key, true);
                }
                Some(true) => {
                    take(st, p, &act.key, false);
                }
                Some(false) => tend(st, p, &act.key),
            }
        }
    }
    if !st.gifts.is_empty() {
        let players = server.get_all_players();
        let th = st.cfg.thai();
        let gifts = std::mem::take(&mut st.gifts);
        for mut gift in gifts {
            if c::ipc("customitems", &serde_json::json!({ "kind": "give", "player": gift.player, "item": gift.item, "count": gift.count })).is_some() {
                continue;
            }
            gift.tries += 1;
            if gift.tries < GIFT_TRIES {
                st.gifts.push(gift);
            } else if let Some(p) = players.iter().find(|p| uuid_of(p) == gift.player) {
                c::tell(p, &msg(th, &format!("&cให้ {} x{} ไม่ได้: ปลั๊กอิน CustomItems ไม่ตอบ", gift.item, gift.count), &format!("&cCould not give {} x{}: the CustomItems plugin did not answer", gift.item, gift.count)));
            }
        }
    }
}

// ---- what players do ------------------------------------------------------------

/// Which crop a stack plants.
fn seed_of<'a>(list: &'a [Crop], stack: &ItemStack) -> Option<&'a Crop> {
    list.iter().find(|c| is_item(stack, &c.seed))
}

/// Right click on a block with a seed: plant it on top. True = planted (the click is used up).
fn plant_seed(st: &mut State, p: &Player, clicked: &BlockPos, block: &str) -> bool {
    let Some(stack) = p.get_inventory().get_item_in_hand(Hand::Right).filter(|s| s.get_count() > 0) else { return false };
    let Some(crop) = seed_of(&st.list, &stack) else { return false };
    let soil = block.strip_prefix("minecraft:").unwrap_or(block);
    if !crop.soil.iter().any(|s| s == soil) {
        return false;
    }
    let world = p.get_world();
    let at = BlockPos { x: clicked.x, y: clicked.y + 1, z: clicked.z };
    let key = key_at(&world.get_dimension(), &at);
    if st.plants.contains_key(&key) || !world.get_block_state(BlockPos { x: at.x, y: at.y, z: at.z }).is_air {
        // Taken: the seed does nothing here, as the game's own seeds would
        return true;
    }
    let plant = Plant { crop: crop.id.clone(), world: key.0.clone(), x: at.x, y: at.y, z: at.z, stage: 0, into: 0.0, seen: c::now_secs() };
    // Dry farmland would turn to dirt under it
    let _ = ground(&world, crop, &plant);
    let mut live = Live { plant, shown: None };
    show(crop, &mut live, &world, &mut st.boxes);
    st.plants.insert(key, live);
    st.dirty = true;
    use_one(p);
    true
}

/// What a plant gives when it is picked ripe.
fn harvest_of(st: &mut State, crop: &Crop) -> Vec<(ItemRef, u32)> {
    let mut out: Vec<(ItemRef, u32)> = Vec::new();
    for drop in &crop.harvest {
        let n = defs::roll(drop, defs::next_random(&mut st.random));
        if n == 0 {
            continue;
        }
        match out.iter_mut().find(|(i, _)| *i == drop.item) {
            Some((_, have)) => *have += n,
            None => out.push((drop.item.clone(), n)),
        }
    }
    out
}

/// A player takes a plant: picked (ripe) or broken. Returns false when their inventory has no
/// room for it (nothing happens then).
fn take(st: &mut State, p: &Player, key: &Key, broken: bool) -> bool {
    let th = st.cfg.thai();
    let Some(live) = st.plants.get(key) else { return false };
    let Some(crop) = find(&st.list, &live.plant.crop).cloned() else { return false };
    let ripe = crop.ripe(live.plant.stage);
    let creative = matches!(p.get_gamemode(), GameMode::Creative);
    let items: Vec<(ItemRef, u32)> = match (ripe, creative) {
        (_, true) => Vec::new(),
        (true, false) => harvest_of(st, &crop),
        (false, false) => vec![(crop.seed.clone(), 1)],
    };
    if items.len() > empty_slots(p) {
        c::action_bar(p, &msg(th, "&cกระเป๋าเต็ม ต้องมีช่องว่างก่อน", "&cYour inventory is full: make room first"));
        return false;
    }
    for (item, count) in &items {
        give(st, p, item, *count);
    }
    let replant = ripe && crop.replant && !broken;
    if replant {
        if let Some(live) = st.plants.get_mut(key) {
            (live.plant.stage, live.plant.into, live.plant.seen) = (0, 0.0, c::now_secs());
            redraw(&crop, live);
        }
    } else if let Some(mut live) = st.plants.remove(key) {
        hide(&mut live, &mut st.boxes);
    }
    st.dirty = true;
    true
}

/// A right click on a plant that is not ripe: bone meal, or how far it is.
fn tend(st: &mut State, p: &Player, key: &Key) {
    let th = st.cfg.thai();
    let Some(live) = st.plants.get_mut(key) else { return };
    let Some(crop) = find(&st.list, &live.plant.crop) else { return };
    let held = p.get_inventory().get_item_in_hand(Hand::Right);
    if crop.bone_meal && held.as_ref().is_some_and(|s| is_item(s, &ItemRef::Game("minecraft:bone_meal".into()))) {
        live.plant.stage = (live.plant.stage + 1).min(crop.stages.len() - 1);
        live.plant.into = 0.0;
        redraw(crop, live);
        if let Some(particle) = c::particles::particle("happy_villager") {
            p.get_world().spawn_particle(particle, (f64::from(key.1) + 0.5, f64::from(key.2) + 0.5, f64::from(key.3) + 0.5), (0.3, 0.3, 0.3), 0.0, 10);
        }
        use_one(p);
        st.dirty = true;
        return;
    }
    let (n, of) = (live.plant.stage + 1, crop.stages.len());
    let left = span(th, defs::until_ripe(&crop.stages, live.plant.stage, live.plant.into));
    let name = crop.name.clone();
    let dry = crop.water && matches!(ground(&p.get_world(), crop, &live.plant), Ground::Dry);
    let mut line = msg(th, &format!("&f{name} &7ขั้น {n}/{of} &8| &7สุกในอีก {left}"), &format!("&f{name} &7stage {n}/{of} &8| &7ripe in {left}"));
    if dry {
        line.push_str(if th { " &c(ดินแห้ง ต้องมีน้ำใกล้ ๆ)" } else { " &c(the soil is dry: it needs water near)" });
    }
    c::action_bar(p, &line);
}

type Data<E> = <E as FromIntoEvent>::Data;

struct OnUse;
impl EventHandler<PlayerInteractEvent> for OnUse {
    fn handle(&self, _server: Server, mut e: Data<PlayerInteractEvent>) -> Data<PlayerInteractEvent> {
        let (InteractAction::RightClickBlock, Some(at)) = (&e.action, &e.clicked_pos) else { return e };
        if e.cancelled {
            return e;
        }
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        if plant_seed(st, &e.player, at, &e.block) {
            e.cancelled = true;
        }
        e
    }
}

struct OnClick;
impl EventHandler<PlayerInteractEntityEvent> for OnClick {
    fn handle(&self, _server: Server, mut e: Data<PlayerInteractEntityEvent>) -> Data<PlayerInteractEntityEvent> {
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let Some(key) = st.boxes.get(&e.entity_id).cloned() else { return e };
        if e.cancelled {
            return e;
        }
        e.cancelled = true;
        let name = e.player.get_name().to_ascii_lowercase();
        let tick = st.tick;
        if st.clicks.get(&name).is_some_and(|t| tick < t + CLICK_TICKS) {
            return e;
        }
        st.clicks.insert(name, tick);
        st.acts.push(Act { player: uuid_of(&e.player), key, attack: matches!(e.action, EntityInteractionAction::Attack) });
        e
    }
}

/// The swing of a hit: the plant whose box this player hit last breaks.
struct OnSwing;
impl EventHandler<PlayerAnimationEvent> for OnSwing {
    fn handle(&self, _server: Server, e: Data<PlayerAnimationEvent>) -> Data<PlayerAnimationEvent> {
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        if st.boxes.is_empty() {
            return e;
        }
        let p = &e.player;
        let (world, at, me) = (p.get_world().get_dimension(), p.get_position(), p.get_id());
        let close = |k: &Key| k.0 == world && (f64::from(k.1) + 0.5 - at.0).powi(2) + (f64::from(k.2) - at.1).powi(2) + (f64::from(k.3) + 0.5 - at.2).powi(2) <= 64.0;
        let hit = st.plants.iter().filter(|(k, l)| l.shown.is_some() && close(k)).find_map(|(k, l)| {
            let by = l.shown.as_ref()?.hit.0.as_interaction()?.get_last_attacker()?;
            (by.high == me.high && by.low == me.low).then(|| k.clone())
        });
        let Some(key) = hit else { return e };
        let name = p.get_name().to_ascii_lowercase();
        st.clicks.insert(name, st.tick);
        if !take(st, p, &key, true) {
            // Still there (no room): a new box forgets the hit
            renew_box(st, &key);
        }
        e
    }
}

/// Give a plant a new interaction box (one that remembers no hit).
fn renew_box(st: &mut State, key: &Key) {
    let Some(live) = st.plants.get_mut(key) else { return };
    let Some(shown) = live.shown.as_mut() else { return };
    let world = shown.hit.0.get_world();
    st.boxes.remove(&shown.hit.1);
    shown.hit.0.remove();
    let at = (f64::from(key.1) + 0.5, f64::from(key.2), f64::from(key.3) + 0.5);
    let entity = world.spawn_entity(EntityType::Interaction, at);
    let id = entity.get_id() as i32;
    if let Some(click) = entity.as_interaction() {
        click.set_width(0.9);
        click.set_height(1.0);
        click.set_response(true);
    }
    mark(&entity);
    st.boxes.insert(id, key.clone());
    shown.hit = (entity, id);
}

struct OnBreak;
impl EventHandler<BlockBreakEvent> for OnBreak {
    fn handle(&self, server: Server, e: Data<BlockBreakEvent>) -> Data<BlockBreakEvent> {
        if e.cancelled {
            return e;
        }
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let above = BlockPos { x: e.block_pos.x, y: e.block_pos.y + 1, z: e.block_pos.z };
        let worlds: Vec<String> = match &e.player {
            Some(p) => vec![p.get_world().get_dimension()],
            None => server.get_all_worlds().iter().map(World::get_dimension).collect(),
        };
        for world in worlds {
            let key = key_at(&world, &above);
            if !st.plants.contains_key(&key) {
                continue;
            }
            // The soil is gone: so is the plant, and who broke it gets what breaking it gives
            let given = e.player.as_ref().is_some_and(|p| take(st, p, &key, true));
            if !given && let Some(mut live) = st.plants.remove(&key) {
                hide(&mut live, &mut st.boxes);
                st.dirty = true;
            }
        }
        e
    }
}

struct OnPlace;
impl EventHandler<BlockPlaceEvent> for OnPlace {
    fn handle(&self, _server: Server, mut e: Data<BlockPlaceEvent>) -> Data<BlockPlaceEvent> {
        if e.cancelled {
            return e;
        }
        let Some(guard) = try_lock() else { return e };
        let Some(st) = guard.as_ref() else { return e };
        if st.plants.contains_key(&key_at(&e.player.get_world().get_dimension(), &e.block_pos)) {
            // A plant's block is taken
            e.cancelled = true;
        }
        e
    }
}

struct OnMoisture;
impl EventHandler<MoistureChangeEvent> for OnMoisture {
    fn handle(&self, _server: Server, mut e: Data<MoistureChangeEvent>) -> Data<MoistureChangeEvent> {
        if e.cancelled || e.new_moisture > 0 {
            return e;
        }
        let Some(guard) = try_lock() else { return e };
        let Some(st) = guard.as_ref() else { return e };
        let above = BlockPos { x: e.block_pos.x, y: e.block_pos.y + 1, z: e.block_pos.z };
        if st.plants.contains_key(&key_at(&e.target_world.get_dimension(), &above)) {
            // Dry farmland under a crop stays farmland, as under the game's own crops
            e.new_moisture = 1;
        }
        e
    }
}

// ---- commands ----------------------------------------------------------------------

fn help(th: bool, admin: bool) -> Vec<String> {
    let mut out = vec![msg(th, "&7ปลูก: คลิกขวาดินด้วยเมล็ด &8| &7เก็บ: คลิกขวาต้นที่สุก &8| &7ทุบต้น = ได้เมล็ดคืน", "&7Plant: right click the soil with a seed &8| &7Pick: right click a ripe plant &8| &7Hit a plant: the seed comes back")];
    if admin {
        out.push(msg(th, "&f/crops list &7| &finfo &7| &freload &7| &fnear [รัศมี] &7| &fgrow [รัศมี] &7| &fversion", "&f/crops list &7| &finfo &7| &freload &7| &fnear [radius] &7| &fgrow [radius] &7| &fversion"));
    }
    out
}

/// The plants within `radius` blocks of a player, nearest first.
fn near(st: &State, p: &Player, radius: f64) -> Vec<Key> {
    let world = p.get_world().get_dimension();
    let at = p.get_position();
    let dist = |k: &Key| (f64::from(k.1) + 0.5 - at.0).powi(2) + (f64::from(k.2) - at.1).powi(2) + (f64::from(k.3) + 0.5 - at.2).powi(2);
    let mut keys: Vec<Key> = st.plants.keys().filter(|k| k.0 == world && dist(k) <= radius * radius).cloned().collect();
    keys.sort_by(|a, b| dist(a).total_cmp(&dist(b)));
    keys
}

fn run(st: &mut State, sender: &CommandSender, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    let admin = c::has_perm(sender, &format!("{NS}:admin"));
    let me = c::sender_player(sender);
    let radius = |at: usize| w.get(at).and_then(|r| r.parse::<f64>().ok()).unwrap_or(8.0).clamp(1.0, 64.0);
    match w.first().map(String::as_str) {
        None => help(th, admin),
        Some("version") => vec![format!("Crops {VERSION}")],
        Some(_) if !admin => vec![msg(th, "&cต้องมีสิทธิ์ crops:admin", "&cThis needs crops:admin")],
        Some("list") => {
            if st.list.is_empty() {
                return vec![msg(th, "&7ยังไม่มีผัก ดูตัวอย่างใน plugins/data/crops/crops/example.toml แล้วสั่ง /crops reload", "&7No crops yet: see plugins/data/crops/crops/example.toml, then /crops reload")];
            }
            let mut out = vec![msg(th, &format!("&fผัก {} อย่าง", st.list.len()), &format!("&f{} crop(s)", st.list.len()))];
            out.extend(st.list.iter().map(|c| {
                let planted = st.plants.values().filter(|l| l.plant.crop == c.id).count();
                let grow: f64 = c.stages.iter().take(c.stages.len() - 1).map(|s| s.seconds).sum();
                format!("&e{} &7{} &8| &7{} {} &8| &7{} {} &8| &7{} {}", c.id, c.name, if th { "เมล็ด" } else { "seed" }, c.seed.text(), if th { "โต" } else { "grows" }, span(th, grow), if th { "ปลูกอยู่" } else { "planted" }, planted)
            }));
            out
        }
        Some("info") => {
            let shown = st.plants.values().filter(|l| l.shown.is_some()).count();
            let others = format!(
                "CustomItems: {}, Models: {}",
                st.ours.as_ref().map_or_else(|| "-".to_owned(), |s| s.len().to_string()),
                st.models.as_ref().map_or_else(|| "-".to_owned(), |s| s.len().to_string())
            );
            let mut out = vec![msg(th, &format!("&fรุ่น {VERSION} &7ผัก {} อย่าง ต้นที่ปลูก {} ต้น แสดงอยู่ {} ต้น, {others}", st.list.len(), st.plants.len(), shown), &format!("&fversion {VERSION} &7{} crop(s), {} plant(s), {shown} drawn now, {others}", st.list.len(), st.plants.len()))];
            out.extend(st.notes.iter().take(30).map(|n| format!("&6! &7{}", n.text(th))));
            out
        }
        Some("reload") => {
            let (cfg, list, notes) = load();
            for live in st.plants.values_mut() {
                hide(live, &mut st.boxes);
            }
            (st.cfg, st.list, st.notes) = (cfg, list, notes);
            ask_others(st);
            let th = st.cfg.thai();
            let lost = st.plants.values().filter(|l| find(&st.list, &l.plant.crop).is_none()).count();
            let mut out = vec![msg(th, &format!("&aอ่านใหม่แล้ว: ผัก {} อย่าง", st.list.len()), &format!("&aRead again: {} crop(s)", st.list.len()))];
            if lost > 0 {
                out.push(msg(th, &format!("&6{lost} ต้นเป็นผักที่ไม่มีแล้ว จึงไม่แสดง (ยังเก็บไว้ในไฟล์)"), &format!("&6{lost} plant(s) are of crops that are gone: not drawn (kept in the file)")));
            }
            if !st.notes.is_empty() {
                out.push(msg(th, &format!("&6มี {} เรื่องที่ควรดู: /crops info", st.notes.len()), &format!("&6{} thing(s) to look at: /crops info", st.notes.len())));
            }
            out
        }
        Some("near") => {
            let Some(p) = me else { return vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")] };
            let keys = near(st, &p, radius(1));
            if keys.is_empty() {
                return vec![msg(th, "&7ไม่มีต้นไม้ใกล้ ๆ", "&7No plants near")];
            }
            keys.iter().take(20).filter_map(|k| {
                let live = st.plants.get(k)?;
                let crop = find(&st.list, &live.plant.crop);
                let state = match crop {
                    Some(c) if c.ripe(live.plant.stage) => if th { "&aสุกแล้ว".to_owned() } else { "&aripe".to_owned() },
                    Some(c) => format!("&7{}/{} {}", live.plant.stage + 1, c.stages.len(), span(th, defs::until_ripe(&c.stages, live.plant.stage, live.plant.into))),
                    None => if th { "&cไม่มีผักนี้แล้ว".to_owned() } else { "&cno such crop".to_owned() },
                };
                Some(format!("&e{} &7{} {} {} &8| {state}", live.plant.crop, k.1, k.2, k.3))
            }).collect()
        }
        Some("grow") => {
            let Some(p) = me else { return vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")] };
            let keys = near(st, &p, radius(1));
            let mut n = 0;
            for k in keys {
                let Some(live) = st.plants.get_mut(&k) else { continue };
                let Some(crop) = find(&st.list, &live.plant.crop) else { continue };
                (live.plant.stage, live.plant.into) = (crop.stages.len() - 1, 0.0);
                redraw(crop, live);
                n += 1;
            }
            st.dirty = n > 0 || st.dirty;
            vec![msg(th, &format!("&aโตจนสุก {n} ต้น"), &format!("&a{n} plant(s) are ripe now"))]
        }
        Some(_) => help(th, admin),
    }
}

struct Cmd;
impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, _server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let lines = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            let Some(st) = guard.as_mut() else { return Ok(0) };
            run(st, &sender, &w)
        };
        for line in lines {
            c::reply(&sender, &line);
        }
        Ok(1)
    }
}

// ---- the plugin ------------------------------------------------------------------

struct Crops;

impl Plugin for Crops {
    fn new() -> Self {
        Crops
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Crops of your own that players plant, grow and pick".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:use"), description: "The /crops help".into(), default: PermissionDefault::Allow, children: vec![] });
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Manage crops".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let (cfg, list, notes) = load();
        let plants = load_plants();
        c::log_info(&format!("{} crop(s), {} plant(s){}", list.len(), plants.len(), if notes.is_empty() { String::new() } else { format!(", {} thing(s) to look at (/crops info)", notes.len()) }));
        let random = c::now_secs().wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg, list, notes, plants, boxes: HashMap::new(), clicks: HashMap::new(), gifts: Vec::new(), acts: Vec::new(), ours: None, models: None, tick: 0, dirty: false, random });
        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let cmd = Command::new(&[NS.to_owned(), "crop".to_owned()], "Crops").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:use"));
        context.register_event_handler(OnUse, EventPriority::Normal, true)?;
        context.register_event_handler(OnClick, EventPriority::Normal, true)?;
        context.register_event_handler(OnSwing, EventPriority::Normal, false)?;
        context.register_event_handler(OnBreak, EventPriority::Normal, true)?;
        context.register_event_handler(OnPlace, EventPriority::Normal, true)?;
        context.register_event_handler(OnMoisture, EventPriority::Normal, true)?;
        context.schedule_repeating_task(1, 1, on_tick);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        if let Ok(mut guard) = STATE.lock()
            && let Some(st) = guard.as_mut()
        {
            let mut boxes = std::mem::take(&mut st.boxes);
            for live in st.plants.values_mut() {
                hide(live, &mut boxes);
            }
            save_plants(st);
        }
        Ok(())
    }

    /// Other plugins: `{"kind":"crops"}` lists the crops.
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let answer = match v.get("kind").and_then(serde_json::Value::as_str).unwrap_or_default() {
            "crops" => {
                let guard = try_lock().ok_or("busy")?;
                let st = guard.as_ref().ok_or("not loaded")?;
                serde_json::json!({ "ok": true, "crops": st.list.iter().map(|c| serde_json::json!({ "id": c.id, "name": c.name, "seed": c.seed.text(), "stages": c.stages.len() })).collect::<Vec<_>>() })
            }
            "model_click" | "model_gone" => serde_json::json!({ "ok": true }),
            other => return Err(format!("unknown kind {other}")),
        };
        serde_json::to_vec(&answer).map_err(|e| e.to_string())
    }
}

register_plugin!(Crops);

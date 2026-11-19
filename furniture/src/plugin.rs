//! Pumpkin WASM glue for Furniture: placing, drawing, sitting and taking pieces back.
//!
//! Pieces are kept in `placed.json`. A piece near a player (`show-distance`) is drawn: an item
//! display (or a model of the Models plugin) and an interaction box that takes the clicks;
//! farther pieces have no entities. A solid piece also has a barrier block, which stays.
//!
//! A hit on the box fires no event: the swing of the arm that comes with it does, and then the
//! boxes near the player are asked who hit them last (as in Crops). A solid piece is hit
//! through its barrier block (a left click on a block fires an event).
//!
//! Sitting: the player rides a display entity made for that, at the seat's height; sneaking
//! gets them off, and the seat is taken away once nobody is on it.
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::{BlockPos, GameMode, Hand};
use pumpkin_plugin_api::data_components::DataComponent;
use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, InteractAction, PlayerAnimationEvent, PlayerInteractEntityEvent, PlayerInteractEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::wit::pumpkin::plugin::display::{DisplayTransformation, Quaternionf, Vector3f};
use pumpkin_plugin_api::wit::pumpkin::plugin::event::EntityInteractionAction;
use pumpkin_plugin_api::wit::pumpkin::plugin::world::{BlockFlags, Entity, World};
use pumpkin_plugin_api::{Context, EntityDisplayExt, EntityType, ItemDisplayMode, ItemStack, Plugin, PluginMetadata, Server, register_plugin};

use crate::defs::{self, Config, Kind, NS, Note, Piece};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&6Furniture&8]&r ";
const FILE: &str = "placed.json";
const LOOK_EVERY: u64 = 20;
const SAVE_EVERY: u64 = 600;
const CLICK_TICKS: u64 = 4;

type Key = (String, i32, i32, i32);

struct Shown {
    entities: Vec<Entity>,
    model: Option<u64>,
    hit: (Entity, i32),
}

struct Live {
    piece: Piece,
    shown: Option<Shown>,
}

struct Seat {
    entity: Entity,
    key: Key,
    born: u64,
}

enum Act {
    Use { player: String, key: Key },
    Take { player: String, key: Key },
}

struct State {
    cfg: Config,
    list: Vec<Kind>,
    notes: Vec<Note>,
    placed: HashMap<Key, Live>,
    boxes: HashMap<i32, Key>,
    seats: Vec<Seat>,
    clicks: HashMap<String, u64>,
    acts: Vec<Act>,
    ours: Option<BTreeSet<String>>,
    models: Option<BTreeSet<String>>,
    tick: u64,
    dirty: bool,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

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

fn key_of(p: &Piece) -> Key {
    (p.world.clone(), p.x, p.y, p.z)
}

fn find<'a>(list: &'a [Kind], id: &str) -> Option<&'a Kind> {
    list.iter().find(|k| k.id == id)
}

// ---- files ----------------------------------------------------------------------------

fn load() -> (Config, Vec<Kind>, Vec<Note>) {
    let cfg = c::load_config::<Config>("config.toml").cleaned();
    let mut notes = Vec::new();
    let first = !c::data_path("furniture").exists();
    let _ = std::fs::create_dir_all(c::data_path("furniture"));
    if first {
        let _ = std::fs::write(c::data_path("furniture/example.toml"), defs::EXAMPLE);
    }
    let mut names: Vec<String> = std::fs::read_dir(c::data_path("furniture")).map(|d| d.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.ends_with(".toml")).collect()).unwrap_or_default();
    names.sort();
    let mut all = Vec::new();
    for name in names {
        let file = format!("furniture/{name}");
        match std::fs::read_to_string(c::data_path(&file)) {
            Ok(text) => all.extend(defs::parse_kinds(&file, &text, &mut notes)),
            Err(_) => notes.push(Note::new(&file, "อ่านไฟล์ไม่ได้", "cannot be read")),
        }
    }
    let list = defs::merge_kinds(all, &mut notes);
    (cfg, list, notes)
}

fn save(st: &mut State) {
    let mut list: Vec<&Piece> = st.placed.values().map(|l| &l.piece).collect();
    list.sort_by(|a, b| (&a.world, a.x, a.y, a.z).cmp(&(&b.world, b.x, b.y, b.z)));
    if c::save_json(FILE, &list) {
        st.dirty = false;
    }
}

fn ask_others(st: &mut State) {
    let names = |plugin: &str, kind: &str, field: &str, key: &str| -> Option<BTreeSet<String>> {
        let answer = c::ipc(plugin, &serde_json::json!({ "kind": kind }))?;
        Some(answer.get(field)?.as_array()?.iter().filter_map(|i| i.get(key).and_then(serde_json::Value::as_str).map(str::to_owned)).collect())
    };
    st.ours = names("customitems", "items", "items", "id");
    st.models = names("models", "models", "models", "name");
    st.notes.retain(|n| !n.en.contains("CustomItems") && !n.en.contains("Models"));
    for k in &st.list {
        if !k.item.starts_with("minecraft:") {
            match &st.ours {
                None => st.notes.push(Note::new(&k.id, "ต้องลงปลั๊กอิน CustomItems ด้วย", "the CustomItems plugin is needed")),
                Some(have) if !have.contains(&k.item) => st.notes.push(Note::new(&k.id, format!("CustomItems ไม่มีไอเท็ม {}", k.item), format!("CustomItems has no item {}", k.item))),
                _ => {}
            }
        }
        if let Some(m) = &k.model {
            match &st.models {
                None => st.notes.push(Note::new(&k.id, "ต้องลงปลั๊กอิน Models ด้วย", "the Models plugin is needed")),
                Some(have) if !have.contains(m) => st.notes.push(Note::new(&k.id, format!("ปลั๊กอิน Models ไม่มีโมเดล {m}"), format!("the Models plugin has no model {m}"))),
                _ => {}
            }
        }
    }
}

// ---- drawing --------------------------------------------------------------------------

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

fn look_stack(kind: &Kind) -> ItemStack {
    if kind.look.starts_with("minecraft:") {
        return ItemStack::new(&kind.look, 1);
    }
    let stack = ItemStack::new("minecraft:paper", 1);
    let mut bytes = varint(kind.look.len() as u32);
    bytes.extend_from_slice(kind.look.as_bytes());
    stack.set_component(DataComponent::ItemModel, &bytes);
    stack
}

fn show(st: &mut State, kind: &Kind, key: &Key, world: &World) {
    let Some(live) = st.placed.get_mut(key) else { return };
    let p = &live.piece;
    let at = (f64::from(p.x) + 0.5, f64::from(p.y), f64::from(p.z) + 0.5);
    let mut entities = Vec::new();
    let mut model = None;
    let usable_model = kind.model.as_ref().filter(|m| st.models.as_ref().is_some_and(|l| l.contains(*m)));
    match usable_model {
        Some(name) => {
            model = c::ipc("models", &serde_json::json!({ "kind": "spawn", "model": name, "x": at.0, "y": at.1 + f64::from(kind.lift), "z": at.2, "world": p.world, "yaw": p.yaw, "scale": kind.size })).and_then(|a| a.get("id")?.as_u64());
        }
        None => {
            let entity = world.spawn_entity(EntityType::ItemDisplay, at);
            c::game::sweep_near(&entity, NS, 0.6);
            c::game::mark(&entity, NS);
            entity.set_rotation(p.yaw, 0.0);
            if let Some(item) = entity.as_item_display() {
                item.set_item(Some(look_stack(kind)));
                item.set_item_display_mode(ItemDisplayMode::Fixed);
                item.get_display().set_transformation(DisplayTransformation {
                    translation: Vector3f { x: 0.0, y: 0.5 * kind.size + kind.lift, z: 0.0 },
                    scale: Vector3f { x: kind.size, y: kind.size, z: kind.size },
                    left_rotation: Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
                    right_rotation: Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
                });
            }
            entities.push(entity);
        }
    }
    let hit = world.spawn_entity(EntityType::Interaction, at);
    c::game::sweep_near(&hit, NS, 0.6);
    c::game::mark(&hit, NS);
    if let Some(box_) = hit.as_interaction() {
        box_.set_width(kind.width);
        box_.set_height(kind.height);
        box_.set_response(true);
    }
    let id = hit.get_id() as i32;
    st.boxes.insert(id, key.clone());
    live.shown = Some(Shown { entities, model, hit: (hit, id) });
}

fn hide(live: &mut Live, boxes: &mut HashMap<i32, Key>) {
    let Some(shown) = live.shown.take() else { return };
    for e in &shown.entities {
        e.remove();
    }
    if let Some(m) = shown.model {
        let _ = c::ipc("models", &serde_json::json!({ "kind": "remove", "id": m }));
    }
    boxes.remove(&shown.hit.1);
    shown.hit.0.remove();
}

fn look_around(st: &mut State, server: &Server) {
    let players: Vec<(String, (f64, f64, f64))> = server.get_all_players().iter().map(|p| (p.get_world().get_dimension(), p.get_position())).collect();
    let reach = f64::from(st.cfg.show_distance);
    let keys: Vec<Key> = st.placed.keys().cloned().collect();
    for key in keys {
        let shown = st.placed.get(&key).is_some_and(|l| l.shown.is_some());
        let at = (f64::from(key.1) + 0.5, f64::from(key.2), f64::from(key.3) + 0.5);
        let keep = if shown { reach + 8.0 } else { reach };
        let near = players.iter().any(|(w, p)| *w == key.0 && c::game::dist2(*p, at) <= keep * keep);
        match (near, shown) {
            (true, false) => {
                let Some(kind) = st.placed.get(&key).and_then(|l| find(&st.list, &l.piece.kind)).cloned() else { continue };
                if let Some(world) = c::game::world_named(server, &key.0) {
                    show(st, &kind, &key, &world);
                }
            }
            (false, true) => {
                if let Some(live) = st.placed.get_mut(&key) {
                    hide(live, &mut st.boxes);
                }
            }
            _ => {}
        }
    }
}

// ---- what players do ------------------------------------------------------------------

fn kind_in_hand<'a>(list: &'a [Kind], p: &Player) -> Option<&'a Kind> {
    let stack = p.get_inventory().get_item_in_hand(Hand::Right).filter(|s| !c::game::is_empty_stack(s))?;
    match c::game::customitem_of(&stack) {
        Some(id) => list.iter().find(|k| k.item == id),
        None => {
            let name = c::game::kind_of(&stack);
            list.iter().find(|k| k.item == name)
        }
    }
}

/// Right click on a block with a furniture item: the piece goes on top. True = it was ours.
fn place(st: &mut State, p: &Player, clicked: &BlockPos) -> bool {
    let th = st.cfg.thai();
    let Some(kind) = kind_in_hand(&st.list, p).cloned() else { return false };
    if let Some(node) = &kind.permission
        && !p.has_permission(node)
    {
        c::action_bar(p, &msg(th, "&cคุณวางชิ้นนี้ไม่ได้", "&cYou may not place this"));
        return true;
    }
    let world = p.get_world();
    let at = BlockPos { x: clicked.x, y: clicked.y + 1, z: clicked.z };
    let key = (world.get_dimension(), at.x, at.y, at.z);
    if st.placed.contains_key(&key) || !world.get_block_state(BlockPos { x: at.x, y: at.y, z: at.z }).is_air {
        return true;
    }
    if let Err(owner) = c::game::may(p, f64::from(at.x), f64::from(at.z), "build") {
        c::action_bar(p, &msg(th, &format!("&cที่นี่เป็นที่ดินของ {owner}"), &format!("&cThis land belongs to {owner}")));
        return true;
    }
    let me = c::game::key(p);
    if st.cfg.max_per_player > 0 && st.placed.values().filter(|l| l.piece.owner == me).count() >= st.cfg.max_per_player as usize {
        c::action_bar(p, &msg(th, &format!("&cวางได้ไม่เกิน {} ชิ้น", st.cfg.max_per_player), &format!("&cYou may place at most {}", st.cfg.max_per_player)));
        return true;
    }
    let piece = Piece { kind: kind.id.clone(), world: key.0.clone(), x: at.x, y: at.y, z: at.z, yaw: defs::facing(p.get_yaw(), kind.turns), owner: me };
    if kind.solid {
        world.set_block_by_name(BlockPos { x: at.x, y: at.y, z: at.z }, "minecraft:barrier".into(), BlockFlags::NOTIFY_NEIGHBORS | BlockFlags::NOTIFY_LISTENERS | BlockFlags::SKIP_DROPS);
    }
    st.placed.insert(key.clone(), Live { piece, shown: None });
    show(st, &kind, &key, &world);
    c::game::use_one_in_hand(p);
    st.dirty = true;
    true
}

/// A player takes a piece back. False when they may not (or have no room).
fn take(st: &mut State, server: &Server, p: &Player, key: &Key) -> bool {
    let th = st.cfg.thai();
    let Some(live) = st.placed.get(key) else { return false };
    let Some(kind) = find(&st.list, &live.piece.kind).cloned() else { return false };
    if let Err(owner) = c::game::may(p, f64::from(key.1), f64::from(key.3), "build") {
        c::action_bar(p, &msg(th, &format!("&cที่นี่เป็นที่ดินของ {owner}"), &format!("&cThis land belongs to {owner}")));
        return false;
    }
    let creative = matches!(p.get_gamemode(), GameMode::Creative);
    if !creative {
        if c::game::empty_slots(p) == 0 {
            c::action_bar(p, &msg(th, "&cกระเป๋าเต็ม ต้องมีช่องว่างก่อน", "&cYour inventory is full: make room first"));
            return false;
        }
        if !c::game::give_item(p, &kind.item, 1) {
            c::action_bar(p, &msg(th, "&cตอนนี้เก็บไม่ได้ (CustomItems ไม่ตอบ) ลองใหม่", "&cCannot take it now (CustomItems did not answer): try again"));
            return false;
        }
    }
    if let Some(mut live) = st.placed.remove(key) {
        hide(&mut live, &mut st.boxes);
    }
    // Who sits on it gets up
    st.seats.retain(|s| {
        if s.key == *key {
            s.entity.eject_passengers();
            s.entity.remove();
            false
        } else {
            true
        }
    });
    if kind.solid
        && let Some(world) = c::game::world_named(server, &key.0)
    {
        let at = BlockPos { x: key.1, y: key.2, z: key.3 };
        if world.get_block_state(BlockPos { x: at.x, y: at.y, z: at.z }).block_name.ends_with("barrier") {
            world.set_block_by_name(at, "minecraft:air".into(), BlockFlags::NOTIFY_NEIGHBORS | BlockFlags::NOTIFY_LISTENERS | BlockFlags::SKIP_DROPS);
        }
    }
    st.dirty = true;
    true
}

/// A right click on a piece: sit when it has a seat.
fn use_piece(st: &mut State, p: &Player, key: &Key) {
    let Some(kind) = st.placed.get(key).and_then(|l| find(&st.list, &l.piece.kind)) else { return };
    let Some(height) = kind.seat else { return };
    if st.seats.iter().any(|s| s.key == *key) {
        c::action_bar(p, &msg(st.cfg.thai(), "&7มีคนนั่งอยู่แล้ว", "&7Someone sits here already"));
        return;
    }
    let me = p.as_entity();
    if me.get_vehicle().is_some() {
        return;
    }
    let world = p.get_world();
    let at = (f64::from(key.1) + 0.5, f64::from(key.2) + f64::from(height), f64::from(key.3) + 0.5);
    let seat = world.spawn_entity(EntityType::ItemDisplay, at);
    c::game::mark(&seat, NS);
    if let Some(yaw) = st.placed.get(key).map(|l| l.piece.yaw) {
        seat.set_rotation(yaw, 0.0);
    }
    seat.add_passenger(me);
    st.seats.push(Seat { entity: seat, key: key.clone(), born: st.tick });
}

/// Seats nobody sits on any more are taken away.
fn tidy_seats(st: &mut State) {
    let tick = st.tick;
    st.seats.retain(|s| {
        if tick > s.born + 10 && s.entity.get_passengers().is_empty() {
            s.entity.remove();
            false
        } else {
            true
        }
    });
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.tick += 1;
    let tick = st.tick;
    if tick == 20 {
        ask_others(st);
    }
    if tick == 40 {
        let n = c::game::sweep(&server, NS);
        if n > 0 {
            c::log_info(&format!("removed {n} entit(ies) that an earlier run left in the world"));
        }
    }
    if tick >= 40 && tick % LOOK_EVERY == 0 {
        look_around(st, &server);
    }
    if tick % 5 == 0 && !st.seats.is_empty() {
        tidy_seats(st);
    }
    if st.dirty && tick % SAVE_EVERY == 0 {
        save(st);
    }
    for a in std::mem::take(&mut st.acts) {
        // Models names a player who clicked a model; everything else has their key
        let who = |player: &str| match player.strip_prefix("name:") {
            Some(name) => server.get_all_players().into_iter().find(|p| p.get_name().eq_ignore_ascii_case(name)),
            None => c::game::online(&server, player),
        };
        match a {
            Act::Use { player, key } => {
                if let Some(p) = who(&player) {
                    use_piece(st, &p, &key);
                }
            }
            Act::Take { player, key } => {
                if let Some(p) = who(&player) {
                    take(st, &server, &p, &key);
                }
            }
        }
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

fn debounce(st: &mut State, p: &Player) -> bool {
    let k = c::game::key(p);
    if st.clicks.get(&k).is_some_and(|t| st.tick < t + CLICK_TICKS) {
        return false;
    }
    st.clicks.insert(k, st.tick);
    true
}

struct OnUse;
impl EventHandler<PlayerInteractEvent> for OnUse {
    fn handle(&self, _server: Server, mut e: Data<PlayerInteractEvent>) -> Data<PlayerInteractEvent> {
        if e.cancelled {
            return e;
        }
        let Some(at) = e.clicked_pos.clone() else { return e };
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let key = (e.player.get_world().get_dimension(), at.x, at.y, at.z);
        let on_piece = st.placed.contains_key(&key);
        match e.action {
            // A solid piece is clicked through its barrier block
            InteractAction::RightClickBlock if on_piece => {
                e.cancelled = true;
                if debounce(st, &e.player) {
                    st.acts.push(Act::Use { player: c::game::key(&e.player), key });
                }
            }
            InteractAction::LeftClickBlock if on_piece => {
                e.cancelled = true;
                if debounce(st, &e.player) {
                    st.acts.push(Act::Take { player: c::game::key(&e.player), key });
                }
            }
            InteractAction::RightClickBlock => {
                if place(st, &e.player, &at) {
                    e.cancelled = true;
                }
            }
            _ => {}
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
        e.cancelled = true;
        if matches!(e.action, EntityInteractionAction::Attack) || !debounce(st, &e.player) {
            return e;
        }
        st.acts.push(Act::Use { player: c::game::key(&e.player), key });
        e
    }
}

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
        let hit = st.placed.iter().filter(|(k, l)| l.shown.is_some() && k.0 == world && c::game::dist2((f64::from(k.1) + 0.5, f64::from(k.2), f64::from(k.3) + 0.5), at) <= 64.0).find_map(|(k, l)| {
            let by = l.shown.as_ref()?.hit.0.as_interaction()?.get_last_attacker()?;
            (by.high == me.high && by.low == me.low).then(|| k.clone())
        });
        let Some(key) = hit else { return e };
        // The box forgets the hit: a new one is made when the piece stays
        if let Some(live) = st.placed.get_mut(&key) {
            hide(live, &mut st.boxes);
        }
        st.acts.push(Act::Take { player: c::game::key(p), key });
        e
    }
}

// ---- commands ----------------------------------------------------------------------

fn run(st: &mut State, server: &Server, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    match w.first().map(String::as_str) {
        Some("version") => vec![format!("Furniture {VERSION}")],
        Some("list") => {
            let mut out = vec![msg(th, &format!("&fเฟอร์นิเจอร์ {} อย่าง วางอยู่ {} ชิ้น", st.list.len(), st.placed.len()), &format!("&f{} kind(s), {} placed", st.list.len(), st.placed.len()))];
            out.extend(st.list.iter().map(|k| format!("&e{} &7{} &8| &7{} &8| &7{}{}", k.id, k.name, k.item, if k.seat.is_some() { "seat " } else { "" }, if k.solid { "solid" } else { "" })));
            out
        }
        Some("info") => {
            let mut out = vec![msg(th, &format!("&fรุ่น {VERSION} &7วางอยู่ {} ชิ้น มีคนนั่ง {}", st.placed.len(), st.seats.len()), &format!("&fversion {VERSION} &7{} placed, {} seated", st.placed.len(), st.seats.len()))];
            out.extend(st.notes.iter().take(30).map(|n| format!("&6! &7{}", n.text(th))));
            out
        }
        Some("reload") => {
            let (cfg, list, notes) = load();
            for live in st.placed.values_mut() {
                hide(live, &mut st.boxes);
            }
            (st.cfg, st.list, st.notes) = (cfg, list, notes);
            ask_others(st);
            vec![msg(st.cfg.thai(), &format!("&aอ่านใหม่แล้ว: {} อย่าง", st.list.len()), &format!("&aRead again: {} kind(s)", st.list.len()))]
        }
        Some("give") => {
            let (Some(id), Some(name)) = (w.get(1), w.get(2)) else { return vec![msg(th, "&f/furniture give <ชื่อ> <ผู้เล่น> [จำนวน]", "&f/furniture give <id> <player> [count]")] };
            let Some(kind) = find(&st.list, id) else { return vec![msg(th, "&cไม่มีเฟอร์นิเจอร์นี้", "&cNo such furniture")] };
            let count = w.get(3).and_then(|n| n.parse::<u32>().ok()).unwrap_or(1).clamp(1, 64);
            let Some(p) = server.get_all_players().into_iter().find(|p| p.get_name().eq_ignore_ascii_case(name)) else { return vec![msg(th, "&cผู้เล่นไม่ได้ออนไลน์", "&cThat player is not online")] };
            if c::game::give_item(&p, &kind.item, count) { vec![msg(th, "&aให้แล้ว", "&aGiven")] } else { vec![msg(th, "&cให้ไม่ได้ (CustomItems ไม่ตอบ)", "&cCould not give it (CustomItems did not answer)")] }
        }
        _ => vec![msg(th, "&f/furniture list &7| &finfo &7| &freload &7| &fgive <ชื่อ> <ผู้เล่น> [จำนวน] &7| &fversion", "&f/furniture list &7| &finfo &7| &freload &7| &fgive <id> <player> [count] &7| &fversion")],
    }
}

struct Cmd;
impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let lines = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            let Some(st) = guard.as_mut() else { return Ok(0) };
            run(st, &server, &w)
        };
        for line in lines {
            c::reply(&sender, &line);
        }
        Ok(1)
    }
}

// ---- the plugin ------------------------------------------------------------------

struct Furniture;

impl Plugin for Furniture {
    fn new() -> Self {
        Furniture
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Furniture of your own that players place, sit on and take back".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Manage furniture".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let (cfg, list, notes) = load();
        let pieces: Vec<Piece> = c::load_json(FILE).unwrap_or_default();
        c::log_info(&format!("{} kind(s), {} placed{}", list.len(), pieces.len(), if notes.is_empty() { String::new() } else { format!(", {} thing(s) to look at (/furniture info)", notes.len()) }));
        let placed = pieces.into_iter().map(|p| (key_of(&p), Live { piece: p, shown: None })).collect();
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg, list, notes, placed, boxes: HashMap::new(), seats: Vec::new(), clicks: HashMap::new(), acts: Vec::new(), ours: None, models: None, tick: 0, dirty: false });
        let cmd = Command::new(&[NS.to_owned()], "Furniture").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:admin"));
        context.register_event_handler(OnUse, EventPriority::Normal, true)?;
        context.register_event_handler(OnClick, EventPriority::Normal, true)?;
        context.register_event_handler(OnSwing, EventPriority::Normal, false)?;
        context.schedule_repeating_task(1, 1, on_tick);
        c::mark_running(VERSION);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::mark_stopped();
        if let Ok(mut guard) = STATE.lock()
            && let Some(st) = guard.as_mut()
        {
            let mut boxes = std::mem::take(&mut st.boxes);
            for live in st.placed.values_mut() {
                hide(live, &mut boxes);
            }
            for s in &st.seats {
                s.entity.eject_passengers();
                s.entity.remove();
            }
            save(st);
        }
        Ok(())
    }

    /// Other plugins: `{"kind":"furniture"}` lists the kinds.
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let answer = match v.get("kind").and_then(serde_json::Value::as_str).unwrap_or_default() {
            "furniture" => {
                let guard = try_lock().ok_or("busy")?;
                let st = guard.as_ref().ok_or("not loaded")?;
                serde_json::json!({ "ok": true, "furniture": st.list.iter().map(|k| serde_json::json!({ "id": k.id, "name": k.name, "item": k.item })).collect::<Vec<_>>() })
            }
            "model_click" => {
                let mut guard = try_lock().ok_or("busy")?;
                let st = guard.as_mut().ok_or("not loaded")?;
                let id = v.get("id").and_then(serde_json::Value::as_u64);
                let player = format!("name:{}", v.get("player").and_then(serde_json::Value::as_str).unwrap_or_default());
                let attack = v.get("attack").and_then(serde_json::Value::as_bool).unwrap_or(false);
                if let Some(key) = st.placed.iter().find(|(_, l)| l.shown.as_ref().is_some_and(|s| s.model.is_some() && s.model == id)).map(|(k, _)| k.clone()) {
                    st.acts.push(if attack { Act::Take { player, key } } else { Act::Use { player, key } });
                }
                serde_json::json!({ "ok": true })
            }
            "model_gone" => serde_json::json!({ "ok": true }),
            other => return Err(format!("unknown kind {other}")),
        };
        serde_json::to_vec(&answer).map_err(|e| e.to_string())
    }
}

register_plugin!(Furniture);

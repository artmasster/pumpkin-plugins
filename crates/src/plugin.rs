//! Pumpkin WASM glue for Crates: crate blocks, keys, the spinning window and the rewards.
//!
//! Crate blocks are kept in `places.json`. A name floats over each crate near a player. Keys
//! are items of the game with this plugin's mark (`crates:key`), so they cannot be faked by
//! renaming an item. A reward is given when the window stops, or at once when the player
//! closes it or leaves; commands of a reward run as the console after the plugin lets go of
//! its state.
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::Hand;
use pumpkin_plugin_api::data_components::DataComponent;
use pumpkin_plugin_api::events::{BlockBreakEvent, EventHandler, EventPriority, FromIntoEvent, InteractAction, InventoryClickEvent, InventoryCloseEvent, PlayerInteractEvent, PlayerLeaveEvent};
use pumpkin_plugin_api::gui::Gui;
use pumpkin_plugin_api::inventory::Inventory;
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::screens_wit::Screen;
use pumpkin_plugin_api::wit::pumpkin::plugin::server::CommandSender as ServerSender;
use pumpkin_plugin_api::wit::pumpkin::plugin::sounds::SoundCategory;
use pumpkin_plugin_api::wit::pumpkin::plugin::world::Entity;
use pumpkin_plugin_api::{Context, EntityDisplayExt, EntityType, ItemStack, Plugin, PluginMetadata, Server, register_plugin};

use crate::defs::{self, Config, Crate, NS, Note, Place, Reward};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&dCrates&8]&r ";
const FILE: &str = "places.json";
const SHOW_DISTANCE: f64 = 32.0;
const CLICK_TICKS: u64 = 6;
/// The slot the spin stops on, in the middle of the window.
const MIDDLE: usize = 4;

type Key = (String, i32, i32, i32);

struct Spin {
    crate_id: String,
    reward: usize,
    inv: Inventory,
    /// The rewards that pass through the middle row, in order.
    strip: Vec<usize>,
    steps: Vec<u32>,
    step: usize,
    next: u64,
    done: bool,
}

enum Act {
    Open { player: String, key: Key },
    Preview { player: String, crate_id: String },
}

enum After {
    Console(String),
}

struct State {
    cfg: Config,
    list: Vec<Crate>,
    notes: Vec<Note>,
    places: Vec<Place>,
    /// The name over each crate shown now, and its model.
    holos: HashMap<Key, Entity>,
    models: HashMap<Key, u64>,
    have_models: Option<BTreeSet<String>>,
    spins: HashMap<String, Spin>,
    /// Players looking at the rewards of a crate.
    previews: BTreeSet<String>,
    clicks: HashMap<String, u64>,
    acts: Vec<Act>,
    after: Vec<After>,
    random: u64,
    tick: u64,
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

fn key_of(p: &Place) -> Key {
    (p.world.clone(), p.x, p.y, p.z)
}

fn find<'a>(list: &'a [Crate], id: &str) -> Option<&'a Crate> {
    list.iter().find(|c| c.id == id)
}

fn load() -> (Config, Vec<Crate>, Vec<Note>) {
    let cfg = c::load_config::<Config>("config.toml").cleaned();
    let mut notes = Vec::new();
    let first = !c::data_path("crates").exists();
    let _ = std::fs::create_dir_all(c::data_path("crates"));
    if first {
        let _ = std::fs::write(c::data_path("crates/example.toml"), defs::EXAMPLE);
    }
    let mut names: Vec<String> = std::fs::read_dir(c::data_path("crates")).map(|d| d.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.ends_with(".toml")).collect()).unwrap_or_default();
    names.sort();
    let mut all = Vec::new();
    for name in names {
        let file = format!("crates/{name}");
        match std::fs::read_to_string(c::data_path(&file)) {
            Ok(text) => all.extend(defs::parse_crates(&file, &text, &mut notes)),
            Err(_) => notes.push(Note::new(&file, "อ่านไฟล์ไม่ได้", "cannot be read")),
        }
    }
    let list = defs::merge_crates(all, &mut notes);
    (cfg, list, notes)
}

// ---- items -------------------------------------------------------------------------

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

fn with_model(stack: &ItemStack, model: &str) {
    let mut bytes = varint(model.len() as u32);
    bytes.extend_from_slice(model.as_bytes());
    stack.set_component(DataComponent::ItemModel, &bytes);
}

/// A key to a crate.
fn key_stack(cr: &Crate, count: u8, th: bool) -> ItemStack {
    let stack = c::game::button(&cr.key_item, &format!("{} &e{}", cr.name, if th { "กุญแจ" } else { "Key" }), &[if th { "&7คลิกขวาที่กล่องเพื่อเปิด".into() } else { "&7Right click the crate to open it".into() }]);
    stack.set_count(count.max(1));
    stack.set_custom_data(NS, "key", &c::game::string_tree(&cr.id));
    stack.set_component(DataComponent::EnchantmentGlintOverride, &[1]);
    if let Some(m) = &cr.key_model {
        with_model(&stack, m);
    }
    stack
}

/// What a reward looks like in the windows.
fn icon(r: &Reward, chance: Option<f64>, th: bool) -> ItemStack {
    let material = match &r.item {
        Some(i) if i.starts_with("minecraft:") => i.clone(),
        Some(_) => "minecraft:paper".into(),
        None if r.money > 0.0 => "minecraft:gold_nugget".into(),
        None => "minecraft:command_block".into(),
    };
    let mut lore = Vec::new();
    if let Some(ch) = chance {
        lore.push(format!("&7{} {:.2}%", if th { "โอกาส" } else { "Chance" }, ch * 100.0));
    }
    let stack = c::game::button(&material, &r.name, &lore);
    if let Some(item) = r.item.as_ref().filter(|i| !i.starts_with("minecraft:")) {
        with_model(&stack, &format!("customitems:{item}"));
    }
    if r.count > 1 && r.count <= 64 {
        stack.set_count(r.count as u8);
    }
    stack
}

fn filler() -> ItemStack {
    c::game::button("minecraft:gray_stained_glass_pane", " ", &[])
}

// ---- the crate blocks -----------------------------------------------------------------

fn holo_text(cr: &Crate, th: bool) -> String {
    format!("{}\n&7{}", cr.name, if th { "คลิกขวาด้วยกุญแจเพื่อเปิด" } else { "Right click with a key" })
}

fn look_around(st: &mut State, server: &Server) {
    let players: Vec<(String, (f64, f64, f64))> = server.get_all_players().iter().map(|p| (p.get_world().get_dimension(), p.get_position())).collect();
    let th = st.cfg.thai();
    for place in st.places.clone() {
        let key = key_of(&place);
        let at = (f64::from(place.x) + 0.5, f64::from(place.y), f64::from(place.z) + 0.5);
        let shown = st.holos.contains_key(&key);
        let reach = if shown { SHOW_DISTANCE + 8.0 } else { SHOW_DISTANCE };
        let near = players.iter().any(|(w, p)| *w == place.world && c::game::dist2(*p, at) <= reach * reach);
        if near && !shown {
            let Some(cr) = find(&st.list, &place.crate_id) else { continue };
            let Some(world) = c::game::world_named(server, &place.world) else { continue };
            let holo = world.spawn_entity(EntityType::TextDisplay, (at.0, at.1 + 1.25, at.2));
            c::game::sweep_near(&holo, NS, 0.5);
            c::game::mark(&holo, NS);
            if let Some(text) = holo.as_text_display() {
                text.set_text(c::color(&holo_text(cr, th)));
                text.get_display().set_billboard(pumpkin_plugin_api::BillboardMode::Center);
            }
            st.holos.insert(key.clone(), holo);
            if let Some(model) = cr.model.as_ref().filter(|m| st.have_models.as_ref().is_some_and(|l| l.contains(*m)))
                && let Some(id) = c::ipc("models", &serde_json::json!({ "kind": "spawn", "model": model, "x": at.0, "y": at.1, "z": at.2, "world": place.world, "yaw": 0 })).and_then(|a| a.get("id")?.as_u64())
            {
                st.models.insert(key, id);
            }
        } else if !near && shown {
            hide(st, &key);
        }
    }
}

fn hide(st: &mut State, key: &Key) {
    if let Some(e) = st.holos.remove(key) {
        e.remove();
    }
    if let Some(id) = st.models.remove(key) {
        let _ = c::ipc("models", &serde_json::json!({ "kind": "remove", "id": id }));
    }
}

fn hide_all(st: &mut State) {
    let keys: Vec<Key> = st.holos.keys().cloned().collect();
    for k in keys {
        hide(st, &k);
    }
}

// ---- opening -----------------------------------------------------------------------

fn give_reward(st: &mut State, server: &Server, p: &Player, cr: &Crate, at: usize) {
    let th = st.cfg.thai();
    let Some(r) = cr.rewards.get(at).cloned() else { return };
    let name = p.get_name();
    if r.give
        && let Some(item) = &r.item
        && !c::game::give_item(p, item, r.count)
    {
        c::tell(p, &msg(th, &format!("&cให้ {} ไม่ได้ (CustomItems ไม่ตอบ) แจ้งแอดมิน", r.name), &format!("&cCould not give {} (CustomItems did not answer): tell an admin", r.name)));
        c::log_info(&format!("could not give {item} x{} to {name} from crate {}", r.count, cr.id));
    }
    if r.money > 0.0 && c::game::money::deposit(&name, r.money).is_none() {
        c::log_info(&format!("could not pay {} to {name} from crate {} (Basics did not answer)", r.money, cr.id));
    }
    for cmd in &r.commands {
        st.after.push(After::Console(cmd.replace("{player}", &name)));
    }
    c::tell(p, &msg(th, &format!("&fได้รับ {}", r.name), &format!("&fYou got {}", r.name)));
    p.play_custom_sound("minecraft:entity.player.levelup", SoundCategory::Players, 0.6, 1.2);
    if r.broadcast {
        let line = msg(th, &format!("&e{name} &fได้ {} &fจาก {}", r.name, cr.name), &format!("&e{name} &fgot {} &ffrom {}", r.name, cr.name));
        for other in server.get_all_players() {
            c::tell(&other, &line);
        }
    }
}

fn finish(st: &mut State, server: &Server, player: &str) {
    let Some(mut spin) = st.spins.remove(player) else { return };
    if spin.done {
        return;
    }
    spin.done = true;
    let Some(cr) = find(&st.list, &spin.crate_id).cloned() else { return };
    if let Some(p) = c::game::online(server, player) {
        give_reward(st, server, &p, &cr, spin.reward);
    }
}

fn draw_strip(st: &State, spin: &Spin, th: bool) {
    let Some(cr) = find(&st.list, &spin.crate_id) else { return };
    for i in 0..9 {
        let Some(r) = spin.strip.get(spin.step + i).and_then(|at| cr.rewards.get(*at)) else { continue };
        spin.inv.set_item(9 + i as u32, Some(icon(r, None, th)));
    }
}

fn open(st: &mut State, server: &Server, p: &Player, key: &Key) {
    let th = st.cfg.thai();
    let Some(place) = st.places.iter().find(|pl| key_of(pl) == *key).cloned() else { return };
    let Some(cr) = find(&st.list, &place.crate_id).cloned() else { return };
    let me = c::game::key(p);
    if st.spins.contains_key(&me) {
        return;
    }
    let held = p.get_inventory().get_item_in_hand(Hand::Right);
    let fits = held.as_ref().and_then(|s| c::game::string_data(s, NS, "key")).is_some_and(|id| id == cr.id);
    if !fits {
        c::action_bar(p, &msg(th, &format!("&cต้องถือกุญแจของ {} &cก่อน (คลิกซ้ายเพื่อดูรางวัล)", cr.name), &format!("&cHold a key to {} &cfirst (left click to see the rewards)", cr.name)));
        return;
    }
    if c::game::empty_slots(p) == 0 {
        c::action_bar(p, &msg(th, "&cกระเป๋าเต็ม ต้องมีช่องว่างก่อน", "&cYour inventory is full: make room first"));
        return;
    }
    let Some(reward) = cr.draw(defs::next_random(&mut st.random)) else { return };
    // The key goes first, so a key is never used twice
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
    if let Some(id) = st.models.get(key) {
        let _ = c::ipc("models", &serde_json::json!({ "kind": "play", "id": id, "animation": cr.open_animation, "mode": "once" }));
    }
    let steps = defs::spin_steps(st.cfg.spin_ticks);
    if steps.is_empty() {
        give_reward(st, server, p, &cr, reward);
        return;
    }
    // The strip ends with the reward in the middle slot at the last step
    let n = steps.len() + 9;
    let mut strip: Vec<usize> = (0..n).map(|_| cr.draw(defs::next_random(&mut st.random)).unwrap_or(0)).collect();
    strip[steps.len() + MIDDLE] = reward;
    let gui = Gui::new(Screen::Generic9x3, c::color(&cr.name));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    let window = gui.get_inventory();
    for slot in (0..9).chain(18..27) {
        window.set_item(slot, Some(filler()));
    }
    let pointer = c::game::button("minecraft:hopper", "&e▼", &[]);
    window.set_item(MIDDLE as u32, Some(pointer));
    let spin = Spin { crate_id: cr.id.clone(), reward, inv: window, strip, steps, step: 0, next: st.tick + 1, done: false };
    draw_strip(st, &spin, th);
    st.spins.insert(me, spin);
    p.open_gui(gui);
}

fn turn_spins(st: &mut State, server: &Server) {
    let th = st.cfg.thai();
    let tick = st.tick;
    let due: Vec<String> = st.spins.iter().filter(|(_, s)| !s.done && tick >= s.next).map(|(k, _)| k.clone()).collect();
    for player in due {
        let Some(mut spin) = st.spins.remove(&player) else { continue };
        if spin.step >= spin.steps.len() {
            st.spins.insert(player.clone(), spin);
            finish(st, server, &player);
            continue;
        }
        spin.next = tick + u64::from(spin.steps[spin.step]);
        spin.step += 1;
        draw_strip(st, &spin, th);
        if let Some(p) = c::game::online(server, &player) {
            p.play_custom_sound("minecraft:block.note_block.hat", SoundCategory::Players, 0.5, 1.4);
        }
        st.spins.insert(player, spin);
    }
}

fn preview(st: &mut State, p: &Player, crate_id: &str) {
    let th = st.cfg.thai();
    let Some(cr) = find(&st.list, crate_id) else { return };
    let gui = Gui::new(Screen::Generic9x6, c::color(&format!("{} &8{}", cr.name, if th { "รางวัล" } else { "rewards" })));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    let inv = gui.get_inventory();
    for (i, r) in cr.rewards.iter().enumerate().take(54) {
        inv.set_item(i as u32, Some(icon(r, Some(cr.chance(i)), th)));
    }
    st.previews.insert(c::game::key(p));
    p.open_gui(gui);
}

fn on_tick(server: Server) {
    let jobs = {
        let Some(mut guard) = try_lock() else { return };
        let Some(st) = guard.as_mut() else { return };
        st.tick += 1;
        let tick = st.tick;
        if tick == 20 {
            st.have_models = c::ipc("models", &serde_json::json!({ "kind": "models" })).and_then(|a| Some(a.get("models")?.as_array()?.iter().filter_map(|m| m.get("name")?.as_str().map(str::to_owned)).collect()));
        }
        if tick == 40 {
            c::game::sweep(&server, NS);
        }
        if tick >= 40 && tick % 20 == 0 {
            look_around(st, &server);
        }
        if !st.spins.is_empty() {
            turn_spins(st, &server);
        }
        for a in std::mem::take(&mut st.acts) {
            match a {
                Act::Open { player, key } => {
                    if let Some(p) = c::game::online(&server, &player) {
                        open(st, &server, &p, &key);
                    }
                }
                Act::Preview { player, crate_id } => {
                    if let Some(p) = c::game::online(&server, &player) {
                        preview(st, &p, &crate_id);
                    }
                }
            }
        }
        std::mem::take(&mut st.after)
    };
    for job in jobs {
        match job {
            After::Console(cmd) => server.execute_command(&cmd, ServerSender::Console),
        }
    }
}

// ---- events ------------------------------------------------------------------------

type Data<E> = <E as FromIntoEvent>::Data;

struct OnUse;
impl EventHandler<PlayerInteractEvent> for OnUse {
    fn handle(&self, _server: Server, mut e: Data<PlayerInteractEvent>) -> Data<PlayerInteractEvent> {
        let Some(at) = e.clicked_pos.clone() else { return e };
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let key = (e.player.get_world().get_dimension(), at.x, at.y, at.z);
        let Some(place) = st.places.iter().find(|p| key_of(p) == key).cloned() else { return e };
        e.cancelled = true;
        let me = c::game::key(&e.player);
        if st.clicks.get(&me).is_some_and(|t| st.tick < t + CLICK_TICKS) {
            return e;
        }
        st.clicks.insert(me.clone(), st.tick);
        match e.action {
            InteractAction::RightClickBlock => st.acts.push(Act::Open { player: me, key }),
            InteractAction::LeftClickBlock => st.acts.push(Act::Preview { player: me, crate_id: place.crate_id }),
            _ => {}
        }
        e
    }
}

/// A crate block is not broken by players (an admin removes it with `/crates unset`).
struct OnBreak;
impl EventHandler<BlockBreakEvent> for OnBreak {
    fn handle(&self, _server: Server, mut e: Data<BlockBreakEvent>) -> Data<BlockBreakEvent> {
        let Some(p) = &e.player else { return e };
        if let Some(st) = try_lock().as_ref().and_then(|g| g.as_ref()) {
            let key = (p.get_world().get_dimension(), e.block_pos.x, e.block_pos.y, e.block_pos.z);
            if st.places.iter().any(|pl| key_of(pl) == key) {
                e.cancelled = true;
            }
        }
        e
    }
}

struct OnClick;
impl EventHandler<InventoryClickEvent> for OnClick {
    fn handle(&self, _server: Server, mut e: Data<InventoryClickEvent>) -> Data<InventoryClickEvent> {
        if e.window_type.is_none() {
            return e;
        }
        if let Some(st) = try_lock().as_ref().and_then(|g| g.as_ref()) {
            let me = c::game::key(&e.player);
            if st.spins.contains_key(&me) || st.previews.contains(&me) {
                e.cancelled = true;
            }
        }
        e
    }
}

struct OnClose;
impl EventHandler<InventoryCloseEvent> for OnClose {
    fn handle(&self, server: Server, e: Data<InventoryCloseEvent>) -> Data<InventoryCloseEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let me = c::game::key(&e.player);
            st.previews.remove(&me);
            // Closed before it stopped: the reward comes now
            finish(st, &server, &me);
        }
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, server: Server, e: Data<PlayerLeaveEvent>) -> Data<PlayerLeaveEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let me = c::game::key(&e.player);
            st.previews.remove(&me);
            if let Some(spin) = st.spins.remove(&me)
                && !spin.done
                && let Some(cr) = find(&st.list, &spin.crate_id).cloned()
            {
                give_reward(st, &server, &e.player, &cr, spin.reward);
            }
        }
        e
    }
}

// ---- commands ----------------------------------------------------------------------

fn target_block(p: &Player) -> Option<Key> {
    let hit = p.as_entity().ray_trace_block(6.0, false)?;
    Some((p.get_world().get_dimension(), hit.pos.x, hit.pos.y, hit.pos.z))
}

fn run(st: &mut State, sender: &CommandSender, server: &Server, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    let me = c::sender_player(sender);
    let usage = || {
        vec![
            msg(th, "&f/crates list &7| &finfo &7| &freload &7| &fversion", "&f/crates list &7| &finfo &7| &freload &7| &fversion"),
            msg(th, "&f/crates set <กล่อง> &7(มองที่บล็อก) &7| &funset &7| &fkey <ผู้เล่น> <กล่อง> [จำนวน] &7| &fpreview <กล่อง>", "&f/crates set <crate> &7(look at a block) &7| &funset &7| &fkey <player> <crate> [n] &7| &fpreview <crate>"),
        ]
    };
    match w.first().map(String::as_str) {
        Some("version") => vec![format!("Crates {VERSION}")],
        Some("list") => {
            let mut out = vec![msg(th, &format!("&fกล่อง {} แบบ วางอยู่ {} จุด", st.list.len(), st.places.len()), &format!("&f{} crate(s), {} placed", st.list.len(), st.places.len()))];
            out.extend(st.list.iter().map(|cr| format!("&e{} &7{} &8| &7{} {}", cr.id, cr.name, cr.rewards.len(), if th { "รางวัล" } else { "reward(s)" })));
            out.extend(st.places.iter().map(|pl| format!("&7- {} {} {} {} {}", pl.crate_id, pl.x, pl.y, pl.z, pl.world)));
            out
        }
        Some("info") => {
            let mut out = vec![msg(th, &format!("&fรุ่น {VERSION} &7กำลังเปิด {} คน", st.spins.len()), &format!("&fversion {VERSION} &7{} opening now", st.spins.len()))];
            out.extend(st.notes.iter().take(30).map(|n| format!("&6! &7{}", n.text(th))));
            out
        }
        Some("reload") => {
            let (cfg, list, notes) = load();
            hide_all(st);
            (st.cfg, st.list, st.notes) = (cfg, list, notes);
            let mut out = vec![msg(st.cfg.thai(), &format!("&aอ่านใหม่แล้ว: {} กล่อง", st.list.len()), &format!("&aRead again: {} crate(s)", st.list.len()))];
            if !st.notes.is_empty() {
                out.push(msg(th, &format!("&6มี {} เรื่องที่ควรดู: /crates info", st.notes.len()), &format!("&6{} thing(s) to look at: /crates info", st.notes.len())));
            }
            out
        }
        Some("set") => {
            let Some(p) = &me else { return vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")] };
            let Some(id) = w.get(1).filter(|id| find(&st.list, id).is_some()).cloned() else { return vec![msg(th, "&cไม่มีกล่องนี้", "&cNo such crate")] };
            let Some(key) = target_block(p) else { return vec![msg(th, "&cมองที่บล็อกก่อน", "&cLook at a block first")] };
            st.places.retain(|pl| key_of(pl) != key);
            hide(st, &key);
            st.places.push(Place { crate_id: id.clone(), world: key.0.clone(), x: key.1, y: key.2, z: key.3 });
            c::save_json(FILE, &st.places);
            vec![msg(th, &format!("&aบล็อกนี้เป็นกล่อง {id} แล้ว"), &format!("&aThis block is crate {id} now"))]
        }
        Some("unset") => {
            let Some(p) = &me else { return vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")] };
            let Some(key) = target_block(p) else { return vec![msg(th, "&cมองที่บล็อกก่อน", "&cLook at a block first")] };
            let before = st.places.len();
            st.places.retain(|pl| key_of(pl) != key);
            hide(st, &key);
            c::save_json(FILE, &st.places);
            if st.places.len() < before { vec![msg(th, "&aเอาออกแล้ว", "&aRemoved")] } else { vec![msg(th, "&7บล็อกนี้ไม่ใช่กล่อง", "&7This block is no crate")] }
        }
        Some("key") => {
            let (Some(name), Some(id)) = (w.get(1), w.get(2)) else { return usage() };
            let Some(cr) = find(&st.list, id) else { return vec![msg(th, "&cไม่มีกล่องนี้", "&cNo such crate")] };
            let count = w.get(3).and_then(|n| n.parse::<u32>().ok()).unwrap_or(1).clamp(1, 640);
            let Some(p) = server.get_all_players().into_iter().find(|p| p.get_name().eq_ignore_ascii_case(name)) else { return vec![msg(th, "&cผู้เล่นไม่ได้ออนไลน์", "&cThat player is not online")] };
            let mut left = count;
            while left > 0 {
                let n = left.min(64);
                if !c::game::give_stack(&p, key_stack(cr, n as u8, th)) {
                    break;
                }
                left -= n;
            }
            c::tell(&p, &msg(th, &format!("&aได้กุญแจ {} x{}", cr.name, count - left), &format!("&aYou got {} key(s) x{}", cr.name, count - left)));
            if left > 0 {
                vec![msg(th, &format!("&6ให้ได้ {} จาก {} (กระเป๋าเต็ม)", count - left, count), &format!("&6Gave {} of {} (inventory full)", count - left, count))]
            } else {
                vec![msg(th, "&aให้แล้ว", "&aGiven")]
            }
        }
        Some("preview") => {
            let Some(p) = &me else { return vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")] };
            match w.get(1).filter(|id| find(&st.list, id).is_some()) {
                Some(id) => {
                    st.acts.push(Act::Preview { player: c::game::key(p), crate_id: id.clone() });
                    Vec::new()
                }
                None => vec![msg(th, "&cไม่มีกล่องนี้", "&cNo such crate")],
            }
        }
        _ => usage(),
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

struct Crates;

impl Plugin for Crates {
    fn new() -> Self {
        Crates
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Crates opened with keys, with a spinning window and rewards".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Manage crates and give keys".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let (cfg, list, notes) = load();
        let places: Vec<Place> = c::load_json(FILE).unwrap_or_default();
        c::log_info(&format!("{} crate(s), {} placed{}", list.len(), places.len(), if notes.is_empty() { String::new() } else { format!(", {} thing(s) to look at (/crates info)", notes.len()) }));
        let random = c::now_secs().wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State {
            cfg,
            list,
            notes,
            places,
            holos: HashMap::new(),
            models: HashMap::new(),
            have_models: None,
            spins: HashMap::new(),
            previews: BTreeSet::new(),
            clicks: HashMap::new(),
            acts: Vec::new(),
            after: Vec::new(),
            random,
            tick: 0,
        });
        let cmd = Command::new(&[NS.to_owned(), "crate".to_owned()], "Crates").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:admin"));
        context.register_event_handler(OnUse, EventPriority::Normal, true)?;
        context.register_event_handler(OnBreak, EventPriority::Normal, true)?;
        context.register_event_handler(OnClick, EventPriority::Normal, true)?;
        context.register_event_handler(OnClose, EventPriority::Normal, true)?;
        context.register_event_handler(OnLeave, EventPriority::Normal, false)?;
        context.schedule_repeating_task(1, 1, on_tick);
        c::mark_running(VERSION);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::mark_stopped();
        if let Ok(mut guard) = STATE.lock()
            && let Some(st) = guard.as_mut()
        {
            hide_all(st);
        }
        Ok(())
    }

    /// Other plugins: `{"kind":"crates"}` lists the crates. Keys go to players by the console
    /// command `crates key <player> <crate> [n]` (a reward of a quest, say).
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let guard = try_lock().ok_or("busy")?;
        let st = guard.as_ref().ok_or("not loaded")?;
        let answer = match v.get("kind").and_then(serde_json::Value::as_str).unwrap_or_default() {
            "crates" => serde_json::json!({ "ok": true, "crates": st.list.iter().map(|cr| serde_json::json!({ "id": cr.id, "name": cr.name })).collect::<Vec<_>>() }),
            "model_click" | "model_gone" => serde_json::json!({ "ok": true }),
            other => return Err(format!("unknown kind {other}")),
        };
        serde_json::to_vec(&answer).map_err(|e| e.to_string())
    }
}

register_plugin!(Crates);

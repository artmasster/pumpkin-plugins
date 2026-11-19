//! Pumpkin WASM glue for Pets: the window, the creatures that follow, and riding.
//!
//! A pet is a creature of the game with its own goals taken away: this plugin walks it to its
//! player with the game's navigation, jumps it over when it falls far behind, and keeps it from
//! being hurt. It exists only while its player is online; `players.json` remembers which pet is
//! out and the names players gave.
//!
//! Riding: the player becomes the pet's passenger; the keys they hold (Pumpkin tells plugins
//! about the input packet) push the pet the way they look.
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::events::{EntityDamageByEntityEvent, EntityDamageEvent, EventHandler, EventPriority, FromIntoEvent, InventoryClickEvent, InventoryCloseEvent, PlayerInputEvent, PlayerInteractEntityEvent, PlayerJoinEvent, PlayerLeaveEvent};
use pumpkin_plugin_api::gui::Gui;
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::screens_wit::Screen;
use pumpkin_plugin_api::wit::pumpkin::plugin::event::EntityInteractionAction;
use pumpkin_plugin_api::wit::pumpkin::plugin::world::{Entity, Mob};
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::defs::{self, Config, Keys, NS, Note, Owner, Pet};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&bPets&8]&r ";
const FILE: &str = "players.json";
const PAGE: usize = 45;
const SLOT_OFF: i16 = 49;

struct Live {
    pet: String,
    entity: Entity,
    id: i32,
    mob: Option<Mob>,
    model: Option<u64>,
}

enum Act {
    Call { player: String, pet: String },
    Dismiss { player: String },
    Ride { player: String },
}

struct State {
    cfg: Config,
    list: Vec<Pet>,
    notes: Vec<Note>,
    owners: HashMap<String, Owner>,
    /// The pet out for each online player.
    lives: HashMap<String, Live>,
    by_entity: HashMap<i32, String>,
    /// Keys each rider holds.
    keys: HashMap<String, Keys>,
    /// Players with the `/pets` window open.
    wins: BTreeSet<String>,
    acts: Vec<Act>,
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

fn load() -> (Config, Vec<Pet>, Vec<Note>) {
    let cfg = c::load_config::<Config>("config.toml").cleaned();
    let mut notes = Vec::new();
    let first = !c::data_path("pets").exists();
    let _ = std::fs::create_dir_all(c::data_path("pets"));
    if first {
        let _ = std::fs::write(c::data_path("pets/example.toml"), defs::EXAMPLE);
    }
    let mut names: Vec<String> = std::fs::read_dir(c::data_path("pets")).map(|d| d.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.ends_with(".toml")).collect()).unwrap_or_default();
    names.sort();
    let mut all = Vec::new();
    for name in names {
        let file = format!("pets/{name}");
        match std::fs::read_to_string(c::data_path(&file)) {
            Ok(text) => all.extend(defs::parse_pets(&file, &text, &mut notes)),
            Err(_) => notes.push(Note::new(&file, "อ่านไฟล์ไม่ได้", "cannot be read")),
        }
    }
    let mut list = defs::merge_pets(all, &mut notes);
    list.retain(|p| {
        let ok = c::tables::entity_type(&p.kind).is_some();
        if !ok {
            notes.push(Note::new(&p.id, format!("ไม่รู้จักสัตว์ {}", p.kind), format!("unknown creature {}", p.kind)));
        }
        ok
    });
    (cfg, list, notes)
}

fn find<'a>(list: &'a [Pet], id: &str) -> Option<&'a Pet> {
    list.iter().find(|p| p.id == id)
}

fn may_have(p: &Player, pet: &Pet) -> bool {
    pet.permission.as_ref().is_none_or(|node| p.has_permission(node))
}

// ---- the creatures ---------------------------------------------------------------

fn shown_name(st: &State, owner: &str, pet: &Pet, player_name: &str) -> String {
    let given = st.owners.get(owner).and_then(|o| o.names.get(&pet.id)).cloned();
    given.unwrap_or_else(|| format!("{} &7({player_name})", pet.name))
}

fn spawn(st: &mut State, p: &Player, pet: &Pet) {
    let owner = c::game::key(p);
    despawn(st, &owner);
    let Some(kind) = c::tables::entity_type(&pet.kind) else { return };
    let pos = p.get_position();
    let entity = p.get_world().spawn_entity(kind, pos);
    c::game::mark(&entity, NS);
    entity.set_remaining_air(entity.get_max_air());
    entity.set_invulnerable(true);
    entity.set_silent(true);
    entity.set_custom_name(c::color(&shown_name(st, &owner, pet, &p.get_name())));
    entity.set_custom_name_visible(true);
    if pet.baby
        && let Some(living) = entity.as_living()
    {
        living.set_age(-24000);
    }
    let mob = entity.as_mob();
    if let Some(mob) = &mob {
        mob.clear_ai_goals();
    }
    let id = entity.get_id() as i32;
    let model = pet.model.as_ref().filter(|m| st.models.as_ref().is_some_and(|l| l.contains(*m))).and_then(|model| {
        c::ipc("models", &serde_json::json!({ "kind": "attach", "model": model, "entity": id, "hide": true }))?.get("id")?.as_u64()
    });
    st.by_entity.insert(id, owner.clone());
    st.lives.insert(owner.clone(), Live { pet: pet.id.clone(), entity, id, mob, model });
    st.owners.entry(owner).or_default().out = Some(pet.id.clone());
    st.dirty = true;
}

fn despawn(st: &mut State, owner: &str) {
    let Some(live) = st.lives.remove(owner) else { return };
    st.by_entity.remove(&live.id);
    st.keys.remove(owner);
    if let Some(m) = live.model {
        let _ = c::ipc("models", &serde_json::json!({ "kind": "remove", "id": m }));
    }
    live.entity.eject_passengers();
    live.entity.remove();
}

/// Pets walk after their players, or carry them.
fn tend(st: &mut State, server: &Server) {
    let follow = f64::from(st.cfg.follow_distance);
    let far = f64::from(st.cfg.teleport_distance);
    let tick = st.tick;
    let owners: Vec<String> = st.lives.keys().cloned().collect();
    for owner in owners {
        let Some(p) = c::game::online(server, &owner) else {
            despawn(st, &owner);
            continue;
        };
        let Some(live) = st.lives.get(&owner) else { continue };
        let Some(pet) = find(&st.list, &live.pet).cloned() else {
            despawn(st, &owner);
            continue;
        };
        let ridden = live.entity.get_passengers().iter().any(|e| e.get_id() == p.as_entity().get_id());
        if ridden {
            let k = st.keys.get(&owner).copied().unwrap_or_default();
            let yaw = p.get_yaw();
            let (vx, vz) = defs::ride_velocity(k, yaw, pet.ride_speed);
            let now = live.entity.get_velocity();
            let vy = if k.jump && live.entity.is_on_ground() { 0.5 } else { now.1 };
            if let Some(mob) = &live.mob {
                mob.stop_navigation();
            }
            live.entity.set_velocity((vx, vy, vz));
            live.entity.set_rotation(yaw, 0.0);
            continue;
        }
        if tick % 10 != 0 {
            continue;
        }
        let here = live.entity.get_position();
        let there = p.get_position();
        let same_world = live.entity.get_world().get_dimension() == p.get_world().get_dimension();
        let d2 = c::game::dist2(here, there);
        if !same_world || d2 > far * far {
            live.entity.teleport(there, p.get_world());
        } else if d2 > follow * follow {
            if let Some(mob) = &live.mob {
                mob.navigate_to_pos(there, f64::from(pet.follow_speed));
            }
        } else if let Some(mob) = &live.mob
            && mob.is_navigating()
        {
            mob.stop_navigation();
        }
    }
}

// ---- the window --------------------------------------------------------------------

fn open(st: &mut State, p: &Player) {
    let th = st.cfg.thai();
    let me = c::game::key(p);
    let gui = Gui::new(Screen::Generic9x6, c::color(if th { "&8สัตว์เลี้ยง" } else { "&8Pets" }));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    let inv = gui.get_inventory();
    let out = st.lives.get(&me).map(|l| l.pet.clone());
    for (i, pet) in st.list.iter().filter(|pet| may_have(p, pet)).take(PAGE).enumerate() {
        let on = out.as_deref() == Some(pet.id.as_str());
        let mut lore = Vec::new();
        if pet.rideable {
            lore.push(if th { "&7ขี่ได้ (คลิกขวาที่ตัว)".to_owned() } else { "&7Rideable (right click it)".to_owned() });
        }
        lore.push(match (on, th) {
            (true, true) => "&aอยู่ข้างนอก &7| คลิกเพื่อเก็บ".to_owned(),
            (true, false) => "&aOut &7| click to put away".to_owned(),
            (false, true) => "&eคลิกเพื่อเรียก".to_owned(),
            (false, false) => "&eClick to call".to_owned(),
        });
        let stack = c::game::button(&pet.icon, &pet.name, &lore);
        if on {
            stack.set_component(pumpkin_plugin_api::data_components::DataComponent::EnchantmentGlintOverride, &[1]);
        }
        inv.set_item(i as u32, Some(stack));
    }
    if out.is_some() {
        inv.set_item(SLOT_OFF as u32, Some(c::game::button("minecraft:barrier", if th { "&cเก็บสัตว์เลี้ยง" } else { "&cPut your pet away" }, &[])));
    }
    st.wins.insert(me);
    p.open_gui(gui);
}

fn on_window_click(st: &mut State, p: &Player, slot: i16) -> bool {
    let me = c::game::key(p);
    if !st.wins.contains(&me) {
        return false;
    }
    let usable: Vec<String> = st.list.iter().filter(|pet| may_have(p, pet)).take(PAGE).map(|pet| pet.id.clone()).collect();
    if slot == SLOT_OFF {
        st.acts.push(Act::Dismiss { player: me });
    } else if let Some(id) = usize::try_from(slot).ok().and_then(|s| usable.get(s)) {
        let out = st.lives.get(&me).is_some_and(|l| l.pet == *id);
        st.acts.push(if out { Act::Dismiss { player: me } } else { Act::Call { player: me, pet: id.clone() } });
    }
    true
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.tick += 1;
    let tick = st.tick;
    if tick == 20 {
        st.models = c::ipc("models", &serde_json::json!({ "kind": "models" })).and_then(|a| Some(a.get("models")?.as_array()?.iter().filter_map(|m| m.get("name")?.as_str().map(str::to_owned)).collect()));
    }
    if tick == 40 {
        c::game::sweep(&server, NS);
        // Players online when the plugin starts get their pets back
        for p in server.get_all_players() {
            let me = c::game::key(&p);
            if let Some(id) = st.owners.get(&me).and_then(|o| o.out.clone())
                && let Some(pet) = find(&st.list, &id).cloned()
                && may_have(&p, &pet)
            {
                spawn(st, &p, &pet);
            }
        }
    }
    let th = st.cfg.thai();
    for a in std::mem::take(&mut st.acts) {
        match a {
            Act::Call { player, pet } => {
                let (Some(p), Some(pet)) = (c::game::online(&server, &player), find(&st.list, &pet).cloned()) else { continue };
                if !may_have(&p, &pet) {
                    continue;
                }
                spawn(st, &p, &pet);
                c::action_bar(&p, &msg(th, &format!("&aเรียก {} &aแล้ว", pet.name), &format!("&a{} &ais out", pet.name)));
                if st.wins.contains(&player) {
                    open(st, &p);
                }
            }
            Act::Dismiss { player } => {
                despawn(st, &player);
                if let Some(o) = st.owners.get_mut(&player) {
                    o.out = None;
                }
                st.dirty = true;
                if let Some(p) = c::game::online(&server, &player)
                    && st.wins.contains(&player)
                {
                    open(st, &p);
                }
            }
            Act::Ride { player } => {
                let Some(p) = c::game::online(&server, &player) else { continue };
                let Some(live) = st.lives.get(&player) else { continue };
                if find(&st.list, &live.pet).is_some_and(|pet| pet.rideable) && p.as_entity().get_vehicle().is_none() {
                    if let Some(mob) = &live.mob {
                        mob.stop_navigation();
                    }
                    live.entity.add_passenger(p.as_entity());
                }
            }
        }
    }
    if tick >= 40 {
        tend(st, &server);
    }
    if st.dirty && tick % 600 == 0 && c::save_json(FILE, &st.owners) {
        st.dirty = false;
    }
}

// ---- events ------------------------------------------------------------------------

type Data<E> = <E as FromIntoEvent>::Data;

struct OnInteract;
impl EventHandler<PlayerInteractEntityEvent> for OnInteract {
    fn handle(&self, _server: Server, mut e: Data<PlayerInteractEntityEvent>) -> Data<PlayerInteractEntityEvent> {
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let Some(owner) = st.by_entity.get(&e.entity_id).cloned() else { return e };
        e.cancelled = true;
        let me = c::game::key(&e.player);
        if owner == me && !matches!(e.action, EntityInteractionAction::Attack) {
            st.acts.push(Act::Ride { player: me });
        }
        e
    }
}

struct OnInput;
impl EventHandler<PlayerInputEvent> for OnInput {
    fn handle(&self, _server: Server, e: Data<PlayerInputEvent>) -> Data<PlayerInputEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let me = c::game::key(&e.player);
            if st.lives.contains_key(&me) {
                st.keys.insert(me, defs::keys(&e.input));
            }
        }
        e
    }
}

struct OnHurt;
impl EventHandler<EntityDamageEvent> for OnHurt {
    fn handle(&self, _server: Server, mut e: Data<EntityDamageEvent>) -> Data<EntityDamageEvent> {
        if let Some(st) = try_lock().as_ref().and_then(|g| g.as_ref())
            && st.by_entity.contains_key(&e.entity_id)
        {
            e.cancelled = true;
        }
        e
    }
}

struct OnHit;
impl EventHandler<EntityDamageByEntityEvent> for OnHit {
    fn handle(&self, _server: Server, mut e: Data<EntityDamageByEntityEvent>) -> Data<EntityDamageByEntityEvent> {
        if let Some(st) = try_lock().as_ref().and_then(|g| g.as_ref())
            && (st.by_entity.contains_key(&e.entity_id) || st.by_entity.contains_key(&e.damager_id))
        {
            // A pet neither takes nor deals hits
            e.cancelled = true;
        }
        e
    }
}

struct OnClick;
impl EventHandler<InventoryClickEvent> for OnClick {
    fn handle(&self, _server: Server, mut e: Data<InventoryClickEvent>) -> Data<InventoryClickEvent> {
        if !matches!(e.window_type, Some(Screen::Generic9x6)) {
            return e;
        }
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut())
            && on_window_click(st, &e.player, e.raw_slot)
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
            st.wins.remove(&c::game::key(&e.player));
        }
        e
    }
}

struct OnJoin;
impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _server: Server, e: Data<PlayerJoinEvent>) -> Data<PlayerJoinEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let me = c::game::key(&e.player);
            if let Some(id) = st.owners.get(&me).and_then(|o| o.out.clone()) {
                // Next tick: the player is in the world then
                st.acts.push(Act::Call { player: me, pet: id });
            }
        }
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, e: Data<PlayerLeaveEvent>) -> Data<PlayerLeaveEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let me = c::game::key(&e.player);
            st.wins.remove(&me);
            despawn(st, &me);
        }
        e
    }
}

// ---- commands ----------------------------------------------------------------------

fn run(st: &mut State, sender: &CommandSender, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    let me = c::sender_player(sender);
    let admin = c::has_perm(sender, &format!("{NS}:admin"));
    let in_game = || vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")];
    match w.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        None => {
            let Some(p) = me else { return in_game() };
            open(st, &p);
            Vec::new()
        }
        Some("call") => {
            let Some(p) = me else { return in_game() };
            match w.get(1).and_then(|id| find(&st.list, id)).filter(|pet| may_have(&p, pet)) {
                Some(pet) => {
                    st.acts.push(Act::Call { player: c::game::key(&p), pet: pet.id.clone() });
                    Vec::new()
                }
                None => vec![msg(th, "&cไม่มีสัตว์เลี้ยงนี้ หรือคุณยังไม่มีสิทธิ์", "&cNo such pet, or you may not have it")],
            }
        }
        Some("dismiss" | "away") => {
            let Some(p) = me else { return in_game() };
            st.acts.push(Act::Dismiss { player: c::game::key(&p) });
            vec![msg(th, "&7เก็บแล้ว", "&7Put away")]
        }
        Some("name") => {
            let Some(p) = me else { return in_game() };
            let k = c::game::key(&p);
            let Some(id) = st.lives.get(&k).map(|l| l.pet.clone()) else { return vec![msg(th, "&cเรียกสัตว์เลี้ยงออกมาก่อน", "&cCall a pet out first")] };
            let name: String = w[1..].join(" ").chars().take(48).collect();
            let owner = st.owners.entry(k.clone()).or_default();
            if name.is_empty() || name.eq_ignore_ascii_case("none") {
                owner.names.remove(&id);
            } else {
                owner.names.insert(id.clone(), name);
            }
            st.dirty = true;
            if let Some(pet) = find(&st.list, &id).cloned() {
                let shown = shown_name(st, &k, &pet, &p.get_name());
                if let Some(live) = st.lives.get(&k) {
                    live.entity.set_custom_name(c::color(&shown));
                }
            }
            vec![msg(th, "&aตั้งชื่อแล้ว", "&aName set")]
        }
        Some("list") if admin => {
            let mut out = vec![msg(th, &format!("&fสัตว์เลี้ยง {} แบบ ออกมาอยู่ {} ตัว", st.list.len(), st.lives.len()), &format!("&f{} pet(s), {} out now", st.list.len(), st.lives.len()))];
            out.extend(st.list.iter().map(|p| format!("&e{} &7{} &8| &7{}{}", p.id, p.name, p.kind, if p.rideable { " ride" } else { "" })));
            out.extend(st.notes.iter().take(20).map(|n| format!("&6! &7{}", n.text(th))));
            out
        }
        Some("reload") if admin => {
            let (cfg, list, notes) = load();
            let out: Vec<String> = st.lives.keys().cloned().collect();
            for o in &out {
                despawn(st, o);
            }
            (st.cfg, st.list, st.notes) = (cfg, list, notes);
            // They come back at the next look (tick 40 logic is only at start): call again
            for o in out {
                if let Some(id) = st.owners.get(&o).and_then(|x| x.out.clone()) {
                    st.acts.push(Act::Call { player: o, pet: id });
                }
            }
            vec![msg(st.cfg.thai(), &format!("&aอ่านใหม่แล้ว: {} แบบ", st.list.len()), &format!("&aRead again: {} pet(s)", st.list.len()))]
        }
        Some("version") => vec![format!("Pets {VERSION}")],
        _ => {
            let mut out = vec![msg(th, "&f/pets &7เปิดหน้าต่าง &7| &f/pets call <ชื่อ> &7| &fdismiss &7| &fname <ชื่อเล่น|none>", "&f/pets &7open the window &7| &f/pets call <id> &7| &fdismiss &7| &fname <name|none>")];
            if admin {
                out.push(msg(th, "&f/pets list &7| &freload &7| &fversion", "&f/pets list &7| &freload &7| &fversion"));
            }
            out
        }
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

struct Pets;

impl Plugin for Pets {
    fn new() -> Self {
        Pets
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Pets that follow their players and can be ridden".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:use"), description: "Have pets".into(), default: PermissionDefault::Allow, children: vec![] });
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Manage pets".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let (cfg, list, notes) = load();
        let owners: HashMap<String, Owner> = c::load_json(FILE).unwrap_or_default();
        c::log_info(&format!("{} pet(s){}", list.len(), if notes.is_empty() { String::new() } else { format!(", {} thing(s) to look at (/pets list)", notes.len()) }));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State {
            cfg,
            list,
            notes,
            owners,
            lives: HashMap::new(),
            by_entity: HashMap::new(),
            keys: HashMap::new(),
            wins: BTreeSet::new(),
            acts: Vec::new(),
            models: None,
            tick: 0,
            dirty: false,
        });
        let cmd = Command::new(&[NS.to_owned(), "pet".to_owned()], "Pets").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:use"));
        context.register_event_handler(OnInteract, EventPriority::Normal, true)?;
        context.register_event_handler(OnInput, EventPriority::Normal, false)?;
        context.register_event_handler(OnHurt, EventPriority::Normal, true)?;
        context.register_event_handler(OnHit, EventPriority::Normal, true)?;
        context.register_event_handler(OnClick, EventPriority::Normal, true)?;
        context.register_event_handler(OnClose, EventPriority::Normal, true)?;
        context.register_event_handler(OnJoin, EventPriority::Normal, false)?;
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
            let out: Vec<String> = st.lives.keys().cloned().collect();
            for o in out {
                despawn(st, &o);
            }
            c::save_json(FILE, &st.owners);
        }
        Ok(())
    }

    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let answer = match v.get("kind").and_then(serde_json::Value::as_str).unwrap_or_default() {
            "pets" => {
                let guard = try_lock().ok_or("busy")?;
                let st = guard.as_ref().ok_or("not loaded")?;
                serde_json::json!({ "ok": true, "pets": st.list.iter().map(|p| serde_json::json!({ "id": p.id, "name": p.name })).collect::<Vec<_>>() })
            }
            "model_click" | "model_gone" => serde_json::json!({ "ok": true }),
            other => return Err(format!("unknown kind {other}")),
        };
        serde_json::to_vec(&answer).map_err(|e| e.to_string())
    }
}

register_plugin!(Pets);

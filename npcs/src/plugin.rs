//! Pumpkin WASM glue for Npcs: the creatures, clicks, dialogs and commands.
//!
//! An NPC is a creature of the game with its AI off, that cannot be hurt and makes no sound,
//! made anew while a player is near (`show-distance`) and taken away when nobody is. A model of
//! the Models plugin can be drawn over it. Entities a run leaves behind carry its mark and are
//! removed by the next run.
//!
//! A right click is done at the next tick: Pumpkin looks the clicked entity up again after the
//! handler, and actions may run commands, which must not run while the plugin holds its state.
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::Hand;
use pumpkin_plugin_api::events::{EntityDamageByEntityEvent, EntityDamageEvent, EventHandler, EventPriority, FromIntoEvent, PlayerInteractEntityEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::text::TextComponent;
use pumpkin_plugin_api::wit::pumpkin::plugin::event::EntityInteractionAction;
use pumpkin_plugin_api::wit::pumpkin::plugin::server::CommandSender as ServerSender;
use pumpkin_plugin_api::wit::pumpkin::plugin::world::{Entity, EquipmentSlot, Mob, World};
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::defs::{self, Action, Config, Dialog as DialogDef, NS, Npc};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&eNpcs&8]&r ";
const FILE: &str = "npcs.json";
const LOOK_EVERY: u64 = 20;
const CLICK_TICKS: u64 = 4;
/// A dialog button counts this long after its dialog was shown.
const DIALOG_TICKS: u64 = 20 * 300;

struct Live {
    entity: Entity,
    id: i32,
    mob: Option<Mob>,
    model: Option<u64>,
    /// Where it is now (it may be walking its path).
    at: (f64, f64, f64),
    /// The path point it walks to.
    next: usize,
    /// Ticks it still waits at a point.
    waiting: u64,
}

/// What a player did, done at the next tick.
enum Act {
    Click { player: String, npc: String },
    Button { player: String, npc: String, button: usize },
}

/// What runs once the state is let go.
enum After {
    AsPlayer { player: String, command: String },
    AsConsole(String),
    Quest { player: String, quest: String },
    Clicked { player: String, npc: String },
}

struct State {
    cfg: Config,
    list: Vec<Npc>,
    notes: Vec<(String, String)>,
    lives: HashMap<String, Live>,
    /// Which NPC an entity is.
    by_entity: HashMap<i32, String>,
    by_model: HashMap<u64, String>,
    /// The tick a player last used an NPC: by `player key/npc`.
    used: HashMap<String, u64>,
    clicks: HashMap<String, u64>,
    /// The dialog a player was shown last, and when.
    shown: HashMap<String, (String, u64)>,
    acts: Vec<Act>,
    after: Vec<After>,
    /// The NPC each sender works on (`/npc select`).
    selected: HashMap<String, String>,
    models: Option<BTreeSet<String>>,
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

fn load() -> (Config, Vec<Npc>, Vec<(String, String)>) {
    let cfg = c::load_config::<Config>("config.toml").cleaned();
    let mut notes = Vec::new();
    let raw: Vec<Npc> = match std::fs::read_to_string(c::data_path(FILE)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            notes.push((format!("{FILE} อ่านไม่ได้: {e}"), format!("{FILE} cannot be read: {e}")));
            Vec::new()
        }),
        Err(_) => Vec::new(),
    };
    let list = defs::read_npcs(raw, &mut notes);
    (cfg, list, notes)
}

fn save(st: &State) -> bool {
    c::save_json(FILE, &st.list)
}

fn slot_of(name: &str) -> EquipmentSlot {
    match name {
        "offhand" => EquipmentSlot::OffHand,
        "head" => EquipmentSlot::Head,
        "chest" => EquipmentSlot::Chest,
        "legs" => EquipmentSlot::Legs,
        "feet" => EquipmentSlot::Feet,
        _ => EquipmentSlot::MainHand,
    }
}

// ---- the creatures ---------------------------------------------------------------

fn show(st: &mut State, npc: &Npc, world: &World) {
    let Some(kind) = c::tables::entity_type(&npc.kind) else { return };
    let at = npc.path.first().map_or((npc.x, npc.y, npc.z), |p| (p[0], p[1], p[2]));
    let entity = world.spawn_entity(kind, at);
    c::game::sweep_near(&entity, NS, 1.0);
    let Some(living) = entity.as_living() else {
        entity.remove();
        return;
    };
    c::game::mark(&entity, NS);
    entity.set_remaining_air(entity.get_max_air());
    entity.set_invulnerable(true);
    entity.set_silent(true);
    entity.set_glowing(npc.glowing);
    entity.set_rotation(npc.yaw, 0.0);
    let mob = entity.as_mob();
    if let Some(mob) = &mob {
        mob.clear_ai_goals();
        mob.set_ai_disabled(true);
    }
    if !npc.name.is_empty() {
        entity.set_custom_name(c::color(&npc.name));
        entity.set_custom_name_visible(true);
    }
    for (slot, text) in &npc.equipment {
        let item = c::items::decode(text);
        living.set_equipment(slot_of(slot), c::make_stack(&item, 1));
    }
    let id = entity.get_id() as i32;
    let model = npc.model.as_ref().filter(|m| st.models.as_ref().is_some_and(|l| l.contains(*m))).and_then(|model| {
        c::ipc("models", &serde_json::json!({ "kind": "attach", "model": model, "entity": id, "name": npc.name, "hide": true }))?.get("id")?.as_u64()
    });
    st.by_entity.insert(id, npc.id.clone());
    if let Some(m) = model {
        st.by_model.insert(m, npc.id.clone());
    }
    st.lives.insert(npc.id.clone(), Live { entity, id, mob, model, at, next: usize::from(npc.path.len() > 1), waiting: 0 });
}

fn hide(st: &mut State, id: &str) {
    let Some(live) = st.lives.remove(id) else { return };
    st.by_entity.remove(&live.id);
    if let Some(m) = live.model {
        st.by_model.remove(&m);
        let _ = c::ipc("models", &serde_json::json!({ "kind": "remove", "id": m }));
    }
    live.entity.remove();
}

fn hide_all(st: &mut State) {
    let ids: Vec<String> = st.lives.keys().cloned().collect();
    for id in ids {
        hide(st, &id);
    }
}

/// Make the NPCs near players and take away the others.
fn look_around(st: &mut State, server: &Server) {
    let players: Vec<(String, (f64, f64, f64))> = server.get_all_players().iter().map(|p| (p.get_world().get_dimension(), p.get_position())).collect();
    let reach = f64::from(st.cfg.show_distance);
    let list = st.list.clone();
    for npc in &list {
        let spot = st.lives.get(&npc.id).map_or((npc.x, npc.y, npc.z), |l| l.at);
        let keep = if st.lives.contains_key(&npc.id) { reach + 8.0 } else { reach };
        let near = players.iter().any(|(w, p)| *w == npc.world && c::game::dist2(*p, spot) <= keep * keep);
        match (near, st.lives.contains_key(&npc.id)) {
            (true, false) => {
                if let Some(world) = c::game::world_named(server, &npc.world) {
                    show(st, npc, &world);
                }
            }
            (false, true) => hide(st, &npc.id),
            _ => {}
        }
    }
}

/// Walk, turn to players, and stay put.
fn move_all(st: &mut State, server: &Server) {
    let players: Vec<(String, (f64, f64, f64))> = server.get_all_players().iter().map(|p| (p.get_world().get_dimension(), { let at = p.get_position(); (at.0, at.1 + 1.62, at.2) })).collect();
    let look = f64::from(st.cfg.look_distance);
    for npc in &st.list {
        let Some(live) = st.lives.get_mut(&npc.id) else { continue };
        let walking = npc.path.len() > 1 && live.waiting == 0;
        if walking {
            let p = npc.path[live.next % npc.path.len()];
            let (to, there) = defs::step_towards(live.at, (p[0], p[1], p[2]), npc.speed / 20.0);
            let yaw = defs::yaw_towards(live.at, to);
            live.at = to;
            live.entity.teleport(to, live.entity.get_world());
            live.entity.set_rotation(yaw, 0.0);
            if let Some(mob) = &live.mob {
                mob.look_at((to.0 + f64::from(-yaw.to_radians().sin()), to.1 + 1.5, to.2 + f64::from(yaw.to_radians().cos())));
            }
            if there {
                live.next = (live.next + 1) % npc.path.len();
                live.waiting = (npc.wait * 20.0) as u64;
            }
            continue;
        }
        live.waiting = live.waiting.saturating_sub(1);
        // Pushed away: back to its place
        let now = live.entity.get_position();
        if c::game::dist2(now, live.at) > 0.25 {
            live.entity.teleport(live.at, live.entity.get_world());
        }
        if npc.look && look > 0.0 && st.tick % 2 == 0 {
            let nearest = players
                .iter()
                .filter(|(w, _)| *w == npc.world)
                .map(|(_, p)| (*p, c::game::dist2(*p, live.at)))
                .filter(|(_, d)| *d <= look * look)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            match (nearest, &live.mob) {
                (Some((eye, _)), Some(mob)) => {
                    mob.look_at(eye);
                    live.entity.set_rotation(defs::yaw_towards(live.at, eye), 0.0);
                }
                (Some((eye, _)), None) => live.entity.set_rotation(defs::yaw_towards(live.at, eye), 0.0),
                (None, _) => live.entity.set_rotation(npc.yaw, 0.0),
            }
        }
    }
}

// ---- clicks ------------------------------------------------------------------------

/// A dialog in chat: its title, its lines and a row of buttons to click. Pumpkin 0.2.2 cannot
/// send a dialog window made by a plugin (it does not write the window's data), so chat it is.
fn show_dialog(p: &Player, npc: &Npc, d: &DialogDef, th: bool) {
    let title = if d.title.is_empty() { if npc.name.is_empty() { npc.id.as_str() } else { npc.name.as_str() } } else { d.title.as_str() };
    c::tell(p, &format!("&8&m          &r &f{title} &8&m          "));
    let name = p.get_name();
    for line in &d.lines {
        c::tell(p, &c::placeholders(&defs::fill(line, &name), Some(&name)));
    }
    if d.buttons.is_empty() {
        return;
    }
    let mut row = TextComponent::text("");
    for (i, b) in d.buttons.iter().enumerate() {
        let button = c::color(&format!("&8[&r{}&8]", b.label))
            .click_run_command(&format!("/npctalk {} {i}", npc.id))
            .hover_show_text(c::color(if th { "&7คลิกเพื่อเลือก" } else { "&7Click to choose" }));
        row = row.add_child(button).add_text("  ");
    }
    p.send_system_message(row, false);
}

/// Do a list of actions for a player.
fn run_actions(st: &mut State, server: &Server, p: &Player, npc: &Npc, actions: &[String]) {
    let name = p.get_name();
    for line in actions {
        let Ok(action) = defs::parse_action(line) else { continue };
        match action {
            Action::Message(text) => c::tell(p, &c::placeholders(&defs::fill(&text, &name), Some(&name))),
            Action::Command(cmd) => st.after.push(After::AsPlayer { player: c::game::key(p), command: defs::fill(&cmd, &name) }),
            Action::Console(cmd) => st.after.push(After::AsConsole(defs::fill(&cmd, &name))),
            Action::Dialog => {
                if let Some(d) = &npc.dialog {
                    show_dialog(p, npc, d, st.cfg.thai());
                    st.shown.insert(c::game::key(p), (npc.id.clone(), st.tick));
                }
            }
            Action::Quest(quest) => st.after.push(After::Quest { player: c::game::key(p), quest }),
            Action::Teleport { x, y, z, world } => {
                let world = world.and_then(|w| c::game::world_named(server, &w)).unwrap_or_else(|| p.get_world());
                p.teleport((x, y, z), None, None, world);
            }
        }
    }
}

fn act(st: &mut State, server: &Server, a: Act) {
    let th = st.cfg.thai();
    let (player, npc_id) = match &a {
        Act::Click { player, npc } | Act::Button { player, npc, .. } => (player.clone(), npc.clone()),
    };
    // Models names a player who clicked a model; everything else has their key
    let p = match player.strip_prefix("name:") {
        Some(name) => server.get_all_players().into_iter().find(|p| p.get_name().eq_ignore_ascii_case(name)),
        None => c::game::online(server, &player),
    };
    let Some(p) = p else { return };
    let player = c::game::key(&p);
    let Some(npc) = st.list.iter().find(|n| n.id == npc_id).cloned() else { return };
    let Some(live) = st.lives.get(&npc.id) else { return };
    let reach = f64::from(st.cfg.click_distance) + 2.0;
    if p.get_world().get_dimension() != npc.world || c::game::dist2(p.get_position(), live.at) > reach * reach {
        return;
    }
    if let Some(node) = &npc.permission
        && !p.has_permission(node)
    {
        c::action_bar(&p, &msg(th, "&cคุณยังใช้ NPC นี้ไม่ได้", "&cYou cannot use this NPC yet"));
        return;
    }
    match a {
        Act::Click { .. } => {
            let used = format!("{player}/{}", npc.id);
            let wait = (npc.cooldown * 20.0) as u64;
            if st.used.get(&used).is_some_and(|t| st.tick < t + wait) {
                return;
            }
            st.used.insert(used, st.tick);
            st.after.push(After::Clicked { player: player.clone(), npc: npc.id.clone() });
            let actions = npc.actions.clone();
            run_actions(st, server, &p, &npc, &actions);
        }
        Act::Button { button, .. } => {
            // Only the dialog this player was shown, and not long ago
            let fresh = st.shown.get(&player).is_some_and(|(n, t)| *n == npc.id && st.tick < t + DIALOG_TICKS);
            let Some(b) = npc.dialog.as_ref().and_then(|d| d.buttons.get(button)).cloned() else { return };
            if !fresh {
                return;
            }
            st.shown.remove(&player);
            run_actions(st, server, &p, &npc, &b.actions);
        }
    }
}

fn run_after(server: &Server, jobs: Vec<After>) {
    for job in jobs {
        match job {
            After::AsPlayer { player, command } => {
                if let Some(p) = c::game::online(server, &player) {
                    server.execute_command(&command, ServerSender::Player(p));
                }
            }
            After::AsConsole(command) => server.execute_command(&command, ServerSender::Console),
            After::Quest { player, quest } => {
                let _ = c::ipc("quests", &serde_json::json!({ "kind": "offer", "player": player, "quest": quest }));
            }
            After::Clicked { player, npc } => {
                let _ = c::ipc("quests", &serde_json::json!({ "kind": "npc_click", "player": player, "npc": npc }));
            }
        }
    }
}

fn on_tick(server: Server) {
    let jobs = {
        let Some(mut guard) = try_lock() else { return };
        let Some(st) = guard.as_mut() else { return };
        st.tick += 1;
        let tick = st.tick;
        if tick == 20 {
            st.models = c::ipc("models", &serde_json::json!({ "kind": "models" }))
                .and_then(|a| Some(a.get("models")?.as_array()?.iter().filter_map(|m| m.get("name")?.as_str().map(str::to_owned)).collect()));
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
        if tick >= 40 {
            move_all(st, &server);
        }
        for a in std::mem::take(&mut st.acts) {
            act(st, &server, a);
        }
        std::mem::take(&mut st.after)
    };
    run_after(&server, jobs);
}

type Data<E> = <E as FromIntoEvent>::Data;

struct OnClick;
impl EventHandler<PlayerInteractEntityEvent> for OnClick {
    fn handle(&self, _server: Server, mut e: Data<PlayerInteractEntityEvent>) -> Data<PlayerInteractEntityEvent> {
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let Some(npc) = st.by_entity.get(&e.entity_id).cloned() else { return e };
        e.cancelled = true;
        if matches!(e.action, EntityInteractionAction::Attack) {
            return e;
        }
        let player = c::game::key(&e.player);
        if st.clicks.get(&player).is_some_and(|t| st.tick < t + CLICK_TICKS) {
            return e;
        }
        st.clicks.insert(player.clone(), st.tick);
        st.acts.push(Act::Click { player, npc });
        e
    }
}

/// NPCs cannot be hurt, whatever hurts them.
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
            && st.by_entity.contains_key(&e.entity_id)
        {
            e.cancelled = true;
        }
        e
    }
}

// ---- commands ----------------------------------------------------------------------

fn help(th: bool) -> Vec<String> {
    let lines: [(&str, &str); 9] = [
        ("&f/npc create <ชื่อ> [ชนิด] &7| &fselect <ชื่อ> &7| &fremove &7| &flist &7| &finfo", "&f/npc create <id> [type] &7| &fselect <id> &7| &fremove &7| &flist &7| &finfo"),
        ("&f/npc tphere &7| &ftp &7| &fname <ข้อความ|none> &7| &ftype <ชนิด> &7| &fmodel <โมเดล|none>", "&f/npc tphere &7| &ftp &7| &fname <text|none> &7| &ftype <type> &7| &fmodel <model|none>"),
        ("&f/npc look on|off &7| &fglow on|off &7| &fcooldown <วินาที> &7| &fpermission <node|none>", "&f/npc look on|off &7| &fglow on|off &7| &fcooldown <seconds> &7| &fpermission <node|none>"),
        ("&f/npc action add <การกระทำ> &7| &faction list &7| &faction remove <ลำดับ> &7| &faction clear", "&f/npc action add <action> &7| &faction list &7| &faction remove <n> &7| &faction clear"),
        ("&7การกระทำ: message <ข้อความ>, command <คำสั่ง>, console <คำสั่ง>, dialog, quest <id>, teleport <x y z> ({player} = ชื่อผู้เล่น)", "&7Actions: message <text>, command <cmd>, console <cmd>, dialog, quest <id>, teleport <x y z> ({player} = the player)"),
        ("&f/npc dialog title <ข้อความ> &7| &fline <ข้อความ> &7| &fbutton <ชื่อปุ่ม> | <การกระทำ> && ... &7| &fclear", "&f/npc dialog title <text> &7| &fline <text> &7| &fbutton <label> | <action> && ... &7| &fclear"),
        ("&f/npc path add &7(จุดที่ยืน) &7| &fpath clear &7| &fspeed <บล็อก/วิ> &7| &fwait <วินาที>", "&f/npc path add &7(where you stand) &7| &fpath clear &7| &fspeed <blocks/s> &7| &fwait <seconds>"),
        ("&f/npc equip <hand|offhand|head|chest|legs|feet> &7(ของในมือ, มือว่าง = ถอด)", "&f/npc equip <hand|offhand|head|chest|legs|feet> &7(what you hold; empty hand = none)"),
        ("&f/npc reload &7| &fversion", "&f/npc reload &7| &fversion"),
    ];
    lines.iter().map(|(t, e)| msg(th, t, e)).collect()
}

fn describe(npc: &Npc, th: bool) -> Vec<String> {
    let mut out = vec![
        format!("&e{} &7{} &8| &7{} {} &8| &7{:.1} {:.1} {:.1} {}", npc.id, npc.name, if th { "ชนิด" } else { "type" }, npc.kind, npc.x, npc.y, npc.z, npc.world),
        format!("&7model: {} &8| &7look: {} &8| &7cooldown: {}s &8| &7path: {} &8| &7permission: {}", npc.model.as_deref().unwrap_or("-"), npc.look, npc.cooldown, npc.path.len(), npc.permission.as_deref().unwrap_or("-")),
    ];
    out.extend(npc.actions.iter().enumerate().map(|(i, a)| format!("&7{}. &f{a}", i + 1)));
    if let Some(d) = &npc.dialog {
        out.push(format!("&7dialog: &f{} &7({} {}, {} {})", d.title, d.lines.len(), if th { "บรรทัด" } else { "line(s)" }, d.buttons.len(), if th { "ปุ่ม" } else { "button(s)" }));
        out.extend(d.buttons.iter().map(|b| format!("&7  [{}&7] {}", b.label, b.actions.join(" && "))));
    }
    out
}

fn on_off(word: Option<&String>) -> Option<bool> {
    match word.map(|w| w.to_ascii_lowercase()).as_deref() {
        Some("on" | "true" | "yes") => Some(true),
        Some("off" | "false" | "no") => Some(false),
        _ => None,
    }
}

/// Text after the first `n` words, as typed.
fn rest(w: &[String], n: usize) -> String {
    w.iter().skip(n).cloned().collect::<Vec<_>>().join(" ")
}

fn run(st: &mut State, sender: &CommandSender, server: &Server, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    let me = c::sender_player(sender);
    let who = c::sender_key(sender);
    let word = w.first().map(|s| s.to_ascii_lowercase());
    match word.as_deref() {
        None | Some("help") => return help(th),
        Some("version") => return vec![format!("Npcs {VERSION}")],
        Some("list") => {
            if st.list.is_empty() {
                return vec![msg(th, "&7ยังไม่มี NPC: /npc create <ชื่อ>", "&7No NPCs yet: /npc create <id>")];
            }
            let mut out = vec![msg(th, &format!("&fNPC {} ตัว", st.list.len()), &format!("&f{} NPC(s)", st.list.len()))];
            out.extend(st.list.iter().map(|n| format!("&e{} &7{} &8| &7{} &8| &7{:.1} {:.1} {:.1}", n.id, n.name, n.kind, n.x, n.y, n.z)));
            return out;
        }
        Some("reload") => {
            let (cfg, list, notes) = load();
            hide_all(st);
            (st.cfg, st.list, st.notes) = (cfg, list, notes);
            let th = st.cfg.thai();
            let mut out = vec![msg(th, &format!("&aอ่านใหม่แล้ว: NPC {} ตัว", st.list.len()), &format!("&aRead again: {} NPC(s)", st.list.len()))];
            out.extend(st.notes.iter().take(20).map(|(t, e)| format!("&6! &7{}", if th { t } else { e })));
            return out;
        }
        Some("create") => {
            let Some(p) = &me else { return vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")] };
            let Some(id) = w.get(1).map(|s| s.to_ascii_lowercase()).filter(|s| defs::valid_id(s)) else {
                return vec![msg(th, "&cชื่อใช้ได้แค่ a-z, 0-9 และ _", "&cAn id may only have a-z, 0-9 and _")];
            };
            if st.list.iter().any(|n| n.id == id) {
                return vec![msg(th, "&cมีชื่อนี้แล้ว", "&cThat id is taken")];
            }
            if st.list.len() >= defs::MAX_NPCS {
                return vec![msg(th, "&cNPC เต็มแล้ว", "&cThere are too many NPCs")];
            }
            let kind = w.get(2).map_or_else(|| "villager".to_owned(), |k| k.to_ascii_lowercase());
            if c::tables::entity_type(&kind).is_none() {
                return vec![msg(th, &format!("&cไม่รู้จักชนิด {kind}"), &format!("&cUnknown type {kind}"))];
            }
            let at = p.get_position();
            st.list.push(Npc { id: id.clone(), name: id.clone(), kind, world: p.get_world().get_dimension(), x: at.0, y: at.1, z: at.2, yaw: p.get_yaw(), ..Npc::default() });
            st.selected.insert(who, id.clone());
            save(st);
            return vec![msg(th, &format!("&aสร้าง {id} แล้ว (เลือกไว้ให้แล้ว)"), &format!("&aMade {id} (it is selected)"))];
        }
        Some("select") => {
            let Some(id) = w.get(1).map(|s| s.to_ascii_lowercase()) else { return help(th) };
            let id = if id == "near" || id == "nearest" {
                let Some(p) = &me else { return vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")] };
                let at = p.get_position();
                let world = p.get_world().get_dimension();
                match st.list.iter().filter(|n| n.world == world).min_by(|a, b| c::game::dist2((a.x, a.y, a.z), at).total_cmp(&c::game::dist2((b.x, b.y, b.z), at))) {
                    Some(n) => n.id.clone(),
                    None => return vec![msg(th, "&cไม่มี NPC ในโลกนี้", "&cNo NPC in this world")],
                }
            } else {
                id
            };
            if !st.list.iter().any(|n| n.id == id) {
                return vec![msg(th, "&cไม่มี NPC ชื่อนี้", "&cNo such NPC")];
            }
            st.selected.insert(who, id.clone());
            return vec![msg(th, &format!("&aเลือก {id} แล้ว"), &format!("&aSelected {id}"))];
        }
        _ => {}
    }
    // Everything else works on the selected NPC
    let Some(id) = st.selected.get(&who).cloned().filter(|id| st.list.iter().any(|n| &n.id == id)) else {
        return vec![msg(th, "&cยังไม่ได้เลือก NPC: /npc select <ชื่อ> หรือ /npc select near", "&cNo NPC selected: /npc select <id> or /npc select near")];
    };
    let Some(at) = st.list.iter().position(|n| n.id == id) else { return Vec::new() };
    let mut redraw = true;
    let out: Vec<String> = match word.as_deref() {
        Some("info") => {
            redraw = false;
            let mut out = describe(&st.list[at], th);
            if let Some(live) = st.lives.get(&id) {
                out.push(format!("&7{} {:.1} {:.1} {:.1}", if th { "อยู่ที่" } else { "now at" }, live.at.0, live.at.1, live.at.2));
            }
            out
        }
        Some("remove") => {
            hide(st, &id);
            st.list.remove(at);
            st.selected.remove(&who);
            save(st);
            return vec![msg(th, &format!("&aลบ {id} แล้ว"), &format!("&aRemoved {id}"))];
        }
        Some("tphere") => {
            let Some(p) = &me else { return vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")] };
            let pos = p.get_position();
            let npc = &mut st.list[at];
            (npc.x, npc.y, npc.z, npc.yaw, npc.world) = (pos.0, pos.1, pos.2, p.get_yaw(), p.get_world().get_dimension());
            vec![msg(th, "&aย้ายมาที่คุณแล้ว", "&aMoved to you")]
        }
        Some("tp") => {
            redraw = false;
            let Some(p) = &me else { return vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")] };
            let npc = &st.list[at];
            if let Some(world) = c::game::world_named(server, &npc.world) {
                p.teleport((npc.x, npc.y, npc.z), Some(npc.yaw), None, world);
            }
            Vec::new()
        }
        Some("name") => {
            let text = rest(w, 1);
            st.list[at].name = if text.eq_ignore_ascii_case("none") { String::new() } else { text.chars().take(120).collect() };
            vec![msg(th, "&aตั้งชื่อแล้ว", "&aName set")]
        }
        Some("type") => match w.get(1).map(|k| k.to_ascii_lowercase()).filter(|k| c::tables::entity_type(k).is_some()) {
            Some(kind) => {
                st.list[at].kind = kind;
                vec![msg(th, "&aเปลี่ยนชนิดแล้ว", "&aType changed")]
            }
            None => vec![msg(th, "&cไม่รู้จักชนิดนี้", "&cUnknown type")],
        },
        Some("model") => match w.get(1).map(|m| m.to_ascii_lowercase()) {
            Some(m) if m == "none" => {
                st.list[at].model = None;
                vec![msg(th, "&aเอาโมเดลออกแล้ว", "&aModel removed")]
            }
            Some(m) if st.models.as_ref().is_some_and(|l| l.contains(&m)) => {
                st.list[at].model = Some(m);
                vec![msg(th, "&aใส่โมเดลแล้ว", "&aModel set")]
            }
            _ => vec![msg(th, "&cไม่มีโมเดลนี้ (ต้องลงปลั๊กอิน Models และมีไฟล์โมเดล)", "&cNo such model (the Models plugin and its model file are needed)")],
        },
        Some("look") | Some("glow") => match on_off(w.get(1)) {
            Some(v) => {
                if word.as_deref() == Some("look") {
                    st.list[at].look = v;
                } else {
                    st.list[at].glowing = v;
                }
                vec![msg(th, "&aตั้งค่าแล้ว", "&aDone")]
            }
            None => help(th),
        },
        Some("cooldown") | Some("speed") | Some("wait") => match w.get(1).and_then(|n| n.parse::<f64>().ok()).filter(|n| n.is_finite() && *n >= 0.0) {
            Some(n) => {
                let npc = &mut st.list[at];
                match word.as_deref() {
                    Some("cooldown") => npc.cooldown = n,
                    Some("speed") => npc.speed = n,
                    _ => npc.wait = n,
                }
                let _ = defs::clean(npc);
                vec![msg(th, "&aตั้งค่าแล้ว", "&aDone")]
            }
            None => help(th),
        },
        Some("permission") => {
            let node = w.get(1).cloned().unwrap_or_default();
            st.list[at].permission = (!node.eq_ignore_ascii_case("none") && !node.is_empty()).then_some(node);
            let _ = defs::clean(&mut st.list[at]);
            vec![msg(th, "&aตั้งค่าแล้ว", "&aDone")]
        }
        Some("action") => {
            redraw = false;
            match w.get(1).map(|s| s.to_ascii_lowercase()).as_deref() {
                Some("add") => {
                    let line = rest(w, 2);
                    match defs::parse_action(&line) {
                        Ok(_) if st.list[at].actions.len() < defs::MAX_ACTIONS => {
                            st.list[at].actions.push(line);
                            vec![msg(th, "&aเพิ่มแล้ว", "&aAdded")]
                        }
                        Ok(_) => vec![msg(th, "&cการกระทำเต็มแล้ว", "&cToo many actions")],
                        Err((t, e)) => vec![msg(th, &format!("&c{t}"), &format!("&c{e}"))],
                    }
                }
                Some("remove") => match w.get(2).and_then(|n| n.parse::<usize>().ok()).filter(|n| *n >= 1 && *n <= st.list[at].actions.len()) {
                    Some(n) => {
                        st.list[at].actions.remove(n - 1);
                        vec![msg(th, "&aลบแล้ว", "&aRemoved")]
                    }
                    None => vec![msg(th, "&cไม่มีลำดับนี้", "&cNo such number")],
                },
                Some("clear") => {
                    st.list[at].actions.clear();
                    vec![msg(th, "&aล้างแล้ว", "&aCleared")]
                }
                _ => describe(&st.list[at], th),
            }
        }
        Some("dialog") => {
            redraw = false;
            let d = st.list[at].dialog.get_or_insert_with(DialogDef::default);
            match w.get(1).map(|s| s.to_ascii_lowercase()).as_deref() {
                Some("title") => {
                    d.title = rest(w, 2).chars().take(120).collect();
                    vec![msg(th, "&aตั้งหัวข้อแล้ว", "&aTitle set")]
                }
                Some("line") if d.lines.len() < defs::MAX_LINES => {
                    d.lines.push(rest(w, 2).chars().take(300).collect());
                    vec![msg(th, "&aเพิ่มบรรทัดแล้ว", "&aLine added")]
                }
                Some("button") if d.buttons.len() < defs::MAX_BUTTONS => match defs::parse_button(&rest(w, 2)) {
                    Ok(b) => {
                        d.buttons.push(b);
                        vec![msg(th, "&aเพิ่มปุ่มแล้ว (อย่าลืม /npc action add dialog)", "&aButton added (remember /npc action add dialog)")]
                    }
                    Err((t, e)) => vec![msg(th, &format!("&c{t}"), &format!("&c{e}"))],
                },
                Some("line" | "button") => vec![msg(th, "&cเต็มแล้ว", "&cFull")],
                Some("clear") => {
                    st.list[at].dialog = None;
                    vec![msg(th, "&aล้าง dialog แล้ว", "&aDialog cleared")]
                }
                _ => describe(&st.list[at], th),
            }
        }
        Some("path") => match w.get(1).map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("add") => {
                let Some(p) = &me else { return vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")] };
                let npc = &mut st.list[at];
                if npc.path.len() >= defs::MAX_PATH {
                    vec![msg(th, "&cจุดเต็มแล้ว", "&cToo many points")]
                } else {
                    if npc.path.is_empty() {
                        npc.path.push([npc.x, npc.y, npc.z]);
                    }
                    let pos = p.get_position();
                    // Standing on the last point already adds nothing
                    if npc.path.last().is_none_or(|l| c::game::dist2((l[0], l[1], l[2]), pos) > 0.25) {
                        npc.path.push([pos.0, pos.1, pos.2]);
                    }
                    vec![msg(th, &format!("&aเพิ่มจุดที่ {} แล้ว", npc.path.len()), &format!("&aPoint {} added", npc.path.len()))]
                }
            }
            Some("clear") => {
                st.list[at].path.clear();
                vec![msg(th, "&aล้างทางเดินแล้ว", "&aPath cleared")]
            }
            _ => help(th),
        },
        Some("equip") => {
            let Some(p) = &me else { return vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")] };
            let Some(slot) = w.get(1).map(|s| s.to_ascii_lowercase()).filter(|s| defs::SLOTS.contains(&s.as_str())) else { return help(th) };
            match p.get_inventory().get_item_in_hand(Hand::Right).and_then(|s| c::item_text(&s, 4000)) {
                Some(text) => {
                    st.list[at].equipment.insert(slot, text);
                }
                None => {
                    st.list[at].equipment.remove(&slot);
                }
            }
            vec![msg(th, "&aตั้งของแล้ว", "&aEquipment set")]
        }
        _ => {
            redraw = false;
            help(th)
        }
    };
    save(st);
    if redraw {
        // Made again with what changed, next time players are looked for
        hide(st, &id);
    }
    out
}

/// `/npctalk <npc> <button>`: what a dialog button in chat runs for the player who clicks it.
/// Only counts for the dialog that player was shown last, and not long ago.
struct Talk;
impl CommandHandler for Talk {
    fn handle(&self, sender: CommandSender, _server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let Some(p) = c::sender_player(&sender) else { return Ok(0) };
        let (Some(npc), Some(Ok(button))) = (w.first(), w.get(1).map(|b| b.parse::<usize>())) else { return Ok(0) };
        let Some(mut guard) = try_lock() else { return Ok(0) };
        let Some(st) = guard.as_mut() else { return Ok(0) };
        st.acts.push(Act::Button { player: c::game::key(&p), npc: npc.clone(), button });
        Ok(1)
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

struct Npcs;

impl Plugin for Npcs {
    fn new() -> Self {
        Npcs
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "NPCs that stand, look, walk and do things when clicked".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Make and change NPCs".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let (cfg, list, notes) = load();
        c::log_info(&format!("{} NPC(s){}", list.len(), if notes.is_empty() { String::new() } else { format!(", {} thing(s) to look at (/npc reload)", notes.len()) }));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State {
            cfg,
            list,
            notes,
            lives: HashMap::new(),
            by_entity: HashMap::new(),
            by_model: HashMap::new(),
            used: HashMap::new(),
            clicks: HashMap::new(),
            shown: HashMap::new(),
            acts: Vec::new(),
            after: Vec::new(),
            selected: HashMap::new(),
            models: None,
            tick: 0,
        });
        let cmd = Command::new(&["npc".to_owned(), NS.to_owned()], "Npcs").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:admin"));
        let _ = context.register_permission(&Permission { node: format!("{NS}:use"), description: "Press the buttons of NPC dialogs".into(), default: PermissionDefault::Allow, children: vec![] });
        let talk = Command::new(&["npctalk".to_owned()], "Npcs dialog buttons").then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Talk));
        context.register_command(talk, &format!("{NS}:use"));
        context.register_event_handler(OnClick, EventPriority::Normal, true)?;
        context.register_event_handler(OnHurt, EventPriority::Normal, true)?;
        context.register_event_handler(OnHit, EventPriority::Normal, true)?;
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

    /// Other plugins: `{"kind":"npcs"}` lists the NPCs. Models tells about clicks on a model.
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let mut guard = try_lock().ok_or("busy")?;
        let st = guard.as_mut().ok_or("not loaded")?;
        let answer = match v.get("kind").and_then(serde_json::Value::as_str).unwrap_or_default() {
            "npcs" => serde_json::json!({ "ok": true, "npcs": st.list.iter().map(|n| serde_json::json!({ "id": n.id, "name": n.name, "world": n.world, "at": [n.x, n.y, n.z] })).collect::<Vec<_>>() }),
            "model_click" => {
                let model = v.get("id").and_then(serde_json::Value::as_u64);
                let attack = v.get("attack").and_then(serde_json::Value::as_bool).unwrap_or(false);
                let player = v.get("player").and_then(serde_json::Value::as_str).unwrap_or_default().to_ascii_lowercase();
                if let (Some(npc), false) = (model.and_then(|m| st.by_model.get(&m)).cloned(), attack) {
                    // Models names the player; the next tick finds them
                    st.acts.push(Act::Click { player: format!("name:{player}"), npc });
                }
                serde_json::json!({ "ok": true })
            }
            "model_gone" => serde_json::json!({ "ok": true }),
            other => return Err(format!("unknown kind {other}")),
        };
        serde_json::to_vec(&answer).map_err(|e| e.to_string())
    }
}

register_plugin!(Npcs);

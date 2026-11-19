//! Pumpkin WASM glue for Graves: taking the items at a death, drawing graves, giving items back.
//!
//! At a death (unless the world keeps inventories) the player's items are written to
//! `graves.json` first and taken out of the inventory only once that write worked, so a failed
//! write leaves everything to the game as before. An item too big to keep stays to the game.
//! Items of the Cosmetics plugin (worn hats) are left alone. Pumpkin 0.2.2 drops only the 36
//! inventory slots at a death and keeps armor on the player; with `take-armor` the grave takes
//! armor and the off hand too, as the game itself does.
use std::collections::HashMap;
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::events::{EntityDamageEvent, EventHandler, EventPriority, FromIntoEvent, PlayerDeathEvent, PlayerInteractEntityEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::wit::pumpkin::plugin::event::EntityInteractionAction;
use pumpkin_plugin_api::wit::pumpkin::plugin::world::Entity;
use pumpkin_plugin_api::{BillboardMode, Context, EntityDisplayExt, EntityType, GameRule, GameRuleValue, ItemDisplayMode, ItemStack, Plugin, PluginMetadata, Server, register_plugin};

use crate::defs::{self, CHEST, Config, FEET, Grave, HEAD, Kept, LEGS, MAX_ITEM, NS, OFF_HAND, Yard};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&7Graves&8]&r ";
const FILE: &str = "graves.json";
const SHOW: f64 = 48.0;

struct Shown {
    entities: Vec<Entity>,
    hit: i32,
}

struct State {
    cfg: Config,
    yard: Yard,
    shown: HashMap<u64, Shown>,
    boxes: HashMap<i32, u64>,
    opens: Vec<(String, u64)>,
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

fn save(st: &State) -> bool {
    c::save_json(FILE, &st.yard)
}

// ---- a death -------------------------------------------------------------------------

/// What a slot holds now, and how to empty it.
fn read_slot(p: &Player, slot: u8) -> Option<ItemStack> {
    let inv = p.get_inventory();
    let stack = match slot {
        HEAD => inv.get_helmet(),
        CHEST => inv.get_chestplate(),
        LEGS => inv.get_leggings(),
        FEET => inv.get_boots(),
        OFF_HAND => inv.get_off_hand(),
        s => p.get_inventory_item(s),
    };
    stack.filter(|s| !c::game::is_empty_stack(s))
}

fn write_slot(p: &Player, slot: u8, stack: Option<ItemStack>) {
    let inv = p.get_inventory();
    match slot {
        HEAD => inv.set_helmet(stack),
        CHEST => inv.set_chestplate(stack),
        LEGS => inv.set_leggings(stack),
        FEET => inv.set_boots(stack),
        OFF_HAND => inv.set_off_hand(stack),
        s => p.set_inventory_item(s, stack),
    }
}

fn died(st: &mut State, p: &Player) {
    let th = st.cfg.thai();
    let world = p.get_world();
    if matches!(world.get_game_rule(GameRule::KeepInventory), GameRuleValue::Bool(true)) {
        return;
    }
    let mut slots: Vec<u8> = (0u8..36).collect();
    if st.cfg.take_armor {
        slots.extend([HEAD, CHEST, LEGS, FEET, OFF_HAND]);
    }
    let mut items = Vec::new();
    for slot in slots {
        let Some(stack) = read_slot(p, slot) else { continue };
        // A worn cosmetic comes back by itself
        if stack.has_custom_data("cosmetics", "id") {
            continue;
        }
        let Some(text) = c::item_text(&stack, MAX_ITEM).filter(|t| !t.ends_with("|!")) else { continue };
        items.push(Kept { slot, item: text, count: stack.get_count() });
    }
    if items.is_empty() {
        return;
    }
    let pos = p.get_position();
    st.yard.next_id += 1;
    let grave = Grave { id: st.yard.next_id, owner: c::game::key(p), owner_name: p.get_name(), world: world.get_dimension(), x: pos.0, y: defs::grave_y(pos.1, world.get_min_y()), z: pos.2, made: c::now_secs(), items };
    st.yard.graves.push(grave.clone());
    if !save(st) {
        // Nothing is taken when it could not be written down: the game drops what it drops
        st.yard.graves.pop();
        c::log_info(&format!("could not write {FILE}: {}'s items were left to the game", p.get_name()));
        return;
    }
    for k in &grave.items {
        write_slot(p, k.slot, None);
    }
    c::tell(p, &msg(th, &format!("&fของของคุณอยู่ในหลุมศพที่ &e{:.0} {:.0} {:.0}", grave.x, grave.y, grave.z), &format!("&fYour items are in a grave at &e{:.0} {:.0} {:.0}", grave.x, grave.y, grave.z)));
    if let Some(left) = grave.left(&st.cfg, grave.made) {
        c::tell(p, &msg(th, &format!("&7หลุมศพอยู่ได้อีก {}", defs::span(th, left)), &format!("&7It lasts {}", defs::span(th, left))));
    }
}

// ---- drawing --------------------------------------------------------------------------

fn label(cfg: &Config, g: &Grave, now: u64) -> String {
    let th = cfg.thai();
    let mut text = format!("&c{} {}", if th { "หลุมศพของ" } else { "Grave of" }, g.owner_name);
    if let Some(left) = g.left(cfg, now) {
        text.push_str(&format!("\n&7{} {}", if th { "เหลือ" } else { "Gone in" }, defs::span(th, left)));
    }
    text
}

fn show(st: &mut State, server: &Server, g: &Grave) {
    let Some(world) = c::game::world_named(server, &g.world) else { return };
    let at = (g.x, g.y, g.z);
    let stone = world.spawn_entity(EntityType::ItemDisplay, at);
    c::game::sweep_near(&stone, NS, 0.5);
    c::game::mark(&stone, NS);
    if let Some(item) = stone.as_item_display() {
        item.set_item(Some(ItemStack::new("minecraft:skeleton_skull", 1)));
        item.set_item_display_mode(ItemDisplayMode::Ground);
    }
    let text = world.spawn_entity(EntityType::TextDisplay, (at.0, at.1 + 0.9, at.2));
    c::game::mark(&text, NS);
    if let Some(t) = text.as_text_display() {
        t.set_text(c::color(&label(&st.cfg, g, c::now_secs())));
        t.get_display().set_billboard(BillboardMode::Center);
    }
    let hit = world.spawn_entity(EntityType::Interaction, (at.0, at.1, at.2));
    c::game::mark(&hit, NS);
    if let Some(b) = hit.as_interaction() {
        b.set_width(0.8);
        b.set_height(0.8);
        b.set_response(true);
    }
    let id = hit.get_id() as i32;
    st.boxes.insert(id, g.id);
    st.shown.insert(g.id, Shown { entities: vec![stone, text, hit], hit: id });
}

fn hide(st: &mut State, id: u64) {
    if let Some(s) = st.shown.remove(&id) {
        st.boxes.remove(&s.hit);
        for e in s.entities {
            e.remove();
        }
    }
}

fn look_around(st: &mut State, server: &Server) {
    let players: Vec<(String, (f64, f64, f64))> = server.get_all_players().iter().map(|p| (p.get_world().get_dimension(), p.get_position())).collect();
    let now = c::now_secs();
    for g in st.yard.graves.clone() {
        let shown = st.shown.contains_key(&g.id);
        let reach = if shown { SHOW + 8.0 } else { SHOW };
        let near = players.iter().any(|(w, p)| *w == g.world && c::game::dist2(*p, (g.x, g.y, g.z)) <= reach * reach);
        match (near, shown) {
            (true, false) => show(st, server, &g),
            (false, true) => hide(st, g.id),
            (true, true) => {
                // The time left on the label
                if let Some(t) = st.shown.get(&g.id).and_then(|s| s.entities.get(1)).and_then(EntityDisplayExt::as_text_display) {
                    t.set_text(c::color(&label(&st.cfg, &g, now)));
                }
            }
            _ => {}
        }
    }
}

/// A player opens a grave: items go back where they were, what finds no room stays.
fn open(st: &mut State, p: &Player, id: u64) {
    let th = st.cfg.thai();
    let now = c::now_secs();
    let admin = p.has_permission(&format!("{NS}:admin"));
    let Some(at) = st.yard.graves.iter().position(|g| g.id == id) else { return };
    let g = st.yard.graves[at].clone();
    if !g.may_open(&st.cfg, &c::game::key(p), admin, now) {
        let wait = (g.made + u64::from(st.cfg.public_minutes) * 60).saturating_sub(now);
        c::action_bar(p, &msg(th, &format!("&cหลุมศพของ {}{}", g.owner_name, if st.cfg.public_minutes > 0 { format!(" &7(เปิดได้ทุกคนในอีก {})", defs::span(th, wait)) } else { String::new() }), &format!("&cThis is {}'s grave{}", g.owner_name, if st.cfg.public_minutes > 0 { format!(" &7(anyone may open it in {})", defs::span(th, wait)) } else { String::new() })));
        return;
    }
    let free = |slot: u8| read_slot(p, slot).is_none();
    let plan = defs::placement(&g.items, &free);
    let mut moved = Vec::new();
    for (i, slot) in plan {
        let k = &g.items[i];
        if let Some(stack) = c::make_stack(&c::items::decode(&k.item), k.count) {
            write_slot(p, slot, Some(stack));
            moved.push(i);
        }
    }
    let left: Vec<Kept> = g.items.iter().enumerate().filter(|(i, _)| !moved.contains(i)).map(|(_, k)| k.clone()).collect();
    if left.is_empty() {
        st.yard.graves.remove(at);
        hide(st, id);
        c::tell(p, &msg(th, &format!("&aได้ของคืนครบ {} ชิ้น", moved.len()), &format!("&aAll {} item(s) are back", moved.len())));
    } else {
        st.yard.graves[at].items = left.clone();
        c::tell(p, &msg(th, &format!("&6ได้คืน {} ชิ้น เหลือในหลุมศพ {} ชิ้น (กระเป๋าเต็ม)", moved.len(), left.len()), &format!("&6{} item(s) back, {} still in the grave (inventory full)", moved.len(), left.len())));
    }
    save(st);
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.tick += 1;
    if st.tick == 40 {
        c::game::sweep(&server, NS);
    }
    for (player, id) in std::mem::take(&mut st.opens) {
        if let Some(p) = c::game::online(&server, &player) {
            open(st, &p, id);
        }
    }
    if st.tick >= 40 && st.tick % 20 == 0 {
        let now = c::now_secs();
        let gone: Vec<u64> = st.yard.graves.iter().filter(|g| g.expired(&st.cfg, now)).map(|g| g.id).collect();
        if !gone.is_empty() {
            for id in &gone {
                hide(st, *id);
                if let Some(g) = st.yard.graves.iter().find(|g| g.id == *id) {
                    c::log_info(&format!("grave {} of {} ran out with {} item(s)", g.id, g.owner_name, g.items.len()));
                }
            }
            st.yard.graves.retain(|g| !gone.contains(&g.id));
            save(st);
        }
        look_around(st, &server);
    }
}

// ---- events --------------------------------------------------------------------------

type Data<E> = <E as FromIntoEvent>::Data;

struct OnDeath;
impl EventHandler<PlayerDeathEvent> for OnDeath {
    fn handle(&self, _server: Server, e: Data<PlayerDeathEvent>) -> Data<PlayerDeathEvent> {
        if e.cancelled {
            return e;
        }
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            died(st, &e.player);
        }
        e
    }
}

struct OnClick;
impl EventHandler<PlayerInteractEntityEvent> for OnClick {
    fn handle(&self, _server: Server, mut e: Data<PlayerInteractEntityEvent>) -> Data<PlayerInteractEntityEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut())
            && let Some(id) = st.boxes.get(&e.entity_id).copied()
        {
            e.cancelled = true;
            if !matches!(e.action, EntityInteractionAction::Attack) && !st.opens.iter().any(|(_, g)| *g == id) {
                st.opens.push((c::game::key(&e.player), id));
            }
        }
        e
    }
}

/// Grave entities are not hurt (an interaction box can be "hit").
struct OnHurt;
impl EventHandler<EntityDamageEvent> for OnHurt {
    fn handle(&self, _server: Server, mut e: Data<EntityDamageEvent>) -> Data<EntityDamageEvent> {
        if let Some(st) = try_lock().as_ref().and_then(|g| g.as_ref())
            && st.boxes.contains_key(&e.entity_id)
        {
            e.cancelled = true;
        }
        e
    }
}

// ---- commands ----------------------------------------------------------------------

fn run(st: &mut State, sender: &CommandSender, server: &Server, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    let me = c::sender_player(sender);
    let admin = c::has_perm(sender, &format!("{NS}:admin"));
    let now = c::now_secs();
    let line = |g: &Grave| format!("&e#{} &7{} &f{:.0} {:.0} {:.0} &7{} &8| &7{} {}{}", g.id, g.owner_name, g.x, g.y, g.z, g.world.trim_start_matches("minecraft:"), g.items.len(), if th { "ชิ้น" } else { "item(s)" }, g.left(&st.cfg, now).map(|l| format!(" &8| &7{}", defs::span(th, l))).unwrap_or_default());
    match w.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        None | Some("list") if !(admin && w.get(1).is_some_and(|a| a == "all")) => {
            let Some(p) = me else { return vec![msg(th, "&f/graves list all", "&f/graves list all")] };
            let k = c::game::key(&p);
            let mine: Vec<String> = st.yard.graves.iter().filter(|g| g.owner == k).map(line).collect();
            if mine.is_empty() { vec![msg(th, "&7คุณไม่มีหลุมศพ", "&7You have no graves")] } else { mine }
        }
        Some("list") => {
            let mut out = vec![msg(th, &format!("&fหลุมศพ {} หลุม", st.yard.graves.len()), &format!("&f{} grave(s)", st.yard.graves.len()))];
            out.extend(st.yard.graves.iter().take(40).map(line));
            out
        }
        Some("tp") if admin => {
            let Some(p) = me else { return vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")] };
            let Some(g) = w.get(1).and_then(|n| n.trim_start_matches('#').parse::<u64>().ok()).and_then(|id| st.yard.graves.iter().find(|g| g.id == id)) else { return vec![msg(th, "&cไม่มีหลุมศพนี้", "&cNo such grave")] };
            if let Some(world) = c::game::world_named(server, &g.world) {
                p.teleport((g.x, g.y + 0.5, g.z), None, None, world);
            }
            Vec::new()
        }
        Some("remove") if admin => {
            let Some(id) = w.get(1).and_then(|n| n.trim_start_matches('#').parse::<u64>().ok()) else { return vec![msg(th, "&f/graves remove <เลข>", "&f/graves remove <number>")] };
            hide(st, id);
            let before = st.yard.graves.len();
            st.yard.graves.retain(|g| g.id != id);
            save(st);
            vec![if st.yard.graves.len() < before { msg(th, "&aลบแล้ว (ของในหลุมหายไปด้วย)", "&aRemoved (with its items)") } else { msg(th, "&cไม่มีหลุมศพนี้", "&cNo such grave") }]
        }
        Some("reload") if admin => {
            st.cfg = c::load_config::<Config>("config.toml").cleaned();
            vec![msg(st.cfg.thai(), "&aอ่านค่าตั้งใหม่แล้ว", "&aSettings read again")]
        }
        Some("version") => vec![format!("Graves {VERSION}")],
        _ => {
            let mut out = vec![msg(th, "&f/graves &7หลุมศพของคุณ", "&f/graves &7your graves")];
            if admin {
                out.push(msg(th, "&f/graves list all &7| &ftp <เลข> &7| &fremove <เลข> &7| &freload &7| &fversion", "&f/graves list all &7| &ftp <n> &7| &fremove <n> &7| &freload &7| &fversion"));
            }
            out
        }
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

struct Graves;

impl Plugin for Graves {
    fn new() -> Self {
        Graves
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Graves that keep a player's items where they died".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:use"), description: "List your graves".into(), default: PermissionDefault::Allow, children: vec![] });
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Open and manage every grave".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let cfg = c::load_config::<Config>("config.toml").cleaned();
        let yard: Yard = c::load_json(FILE).unwrap_or_default();
        c::log_info(&format!("{} grave(s)", yard.graves.len()));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg, yard, shown: HashMap::new(), boxes: HashMap::new(), opens: Vec::new(), tick: 0 });
        let cmd = Command::new(&[NS.to_owned(), "grave".to_owned()], "Graves").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:use"));
        context.register_event_handler(OnDeath, EventPriority::Lowest, true)?;
        context.register_event_handler(OnClick, EventPriority::Normal, true)?;
        context.register_event_handler(OnHurt, EventPriority::Normal, true)?;
        context.schedule_repeating_task(1, 1, on_tick);
        c::mark_running(VERSION);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::mark_stopped();
        if let Ok(mut guard) = STATE.lock()
            && let Some(st) = guard.as_mut()
        {
            let ids: Vec<u64> = st.shown.keys().copied().collect();
            for id in ids {
                hide(st, id);
            }
            save(st);
        }
        Ok(())
    }
}

register_plugin!(Graves);

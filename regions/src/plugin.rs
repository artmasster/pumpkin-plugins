//! Pumpkin WASM glue for Regions: the events each flag guards, and the `/rg` commands.
//!
//! `regions.json` keeps the regions. Explosion and ignite events of Pumpkin 0.2.2 carry no
//! world, so `explosions` and `fire-spread` look at that place in every world (as Claims does).
//! Players with `regions:bypass` (ops) pass `build`, `interact`, `entry` and `item-drop`.
use std::collections::HashMap;
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::Hand;
use pumpkin_plugin_api::events::{BlockBreakEvent, BlockBurnEvent, BlockExplodeEvent, BlockIgniteEvent, BlockPlaceEvent, CreatureSpawnEvent, EntityDamageByEntityEvent, EntityDamageEvent, EntityExplodeEvent, EventHandler, EventPriority, FoodLevelChangeEvent, FromIntoEvent, InteractAction, PlayerDropItemEvent, PlayerInteractEvent, PlayerMoveEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::{Context, DamageType, Plugin, PluginMetadata, Server, register_plugin};

use crate::defs::{self, Config, GLOBAL, Map, NS, Region, Say};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&2Regions&8]&r ";
const FILE: &str = "regions.json";

struct State {
    cfg: Config,
    map: Map,
    /// Selection corners by sender.
    corners: HashMap<String, (Option<(String, [i32; 3])>, Option<(String, [i32; 3])>)>,
    /// The regions each player was in, for greetings.
    inside: HashMap<String, Vec<String>>,
    /// When a player was last told they may not: once in a while is enough.
    told: HashMap<String, std::time::Instant>,
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

fn save(st: &State) {
    if !c::save_json(FILE, &st.map) {
        c::log_info("could not write regions.json");
    }
}

fn block_of(pos: (f64, f64, f64)) -> (i32, i32, i32) {
    (pos.0.floor() as i32, pos.1.floor() as i32, pos.2.floor() as i32)
}

fn deny(st: &mut State, p: &Player, flag: &str) {
    let th = st.cfg.thai();
    let k = c::game::key(p);
    if st.told.get(&k).is_some_and(|t| t.elapsed().as_secs_f32() < 2.0) {
        return;
    }
    st.told.insert(k, std::time::Instant::now());
    let what = match (flag, th) {
        ("build", true) => "สร้างหรือทุบ",
        ("build", false) => "build",
        ("interact", true) => "ใช้สิ่งนี้",
        ("interact", false) => "use that",
        ("pvp", true) => "ตีผู้เล่น",
        ("pvp", false) => "fight players",
        ("entry", true) => "เข้าไป",
        ("entry", false) => "go in",
        ("item-drop", true) => "ทิ้งของ",
        ("item-drop", false) => "drop items",
        (_, true) => "ทำแบบนี้",
        (_, false) => "do that",
    };
    c::action_bar(p, &msg(th, &format!("&cที่นี่{what}ไม่ได้"), &format!("&cYou may not {what} here")));
}

/// Is a player refused `flag` at a block (and told so)?
fn refused(st: &mut State, p: &Player, flag: &str, at: (i32, i32, i32)) -> bool {
    if matches!(flag, "build" | "interact" | "entry" | "item-drop") && p.has_permission(&format!("{NS}:bypass")) {
        return false;
    }
    let world = p.get_world().get_dimension();
    let no = st.map.say(flag, &world, at.0, at.1, at.2, Some(&c::game::key(p))) == Say::Deny;
    if no {
        deny(st, p, flag);
    }
    no
}

/// A flag at a place in any world (for events without a world).
fn denied_anywhere(st: &State, flag: &str, at: (i32, i32, i32)) -> bool {
    let worlds: Vec<String> = st.map.regions.iter().map(|r| r.world.clone()).collect();
    worlds.iter().any(|w| st.map.say(flag, w, at.0, at.1, at.2, None) == Say::Deny)
}

type Data<E> = <E as FromIntoEvent>::Data;

macro_rules! guard {
    ($name:ident, $event:ty, |$st:ident, $e:ident| $body:block) => {
        struct $name;
        impl EventHandler<$event> for $name {
            fn handle(&self, _server: Server, mut $e: Data<$event>) -> Data<$event> {
                if $e.cancelled {
                    return $e;
                }
                if let Some($st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
                    if !$st.map.regions.is_empty() && $body {
                        $e.cancelled = true;
                    }
                }
                $e
            }
        }
    };
}

guard!(OnBreak, BlockBreakEvent, |st, e| {
    match &e.player {
        Some(p) => refused(st, p, "build", (e.block_pos.x, e.block_pos.y, e.block_pos.z)),
        None => false,
    }
});

guard!(OnPlace, BlockPlaceEvent, |st, e| { refused(st, &e.player, "build", (e.block_pos.x, e.block_pos.y, e.block_pos.z)) });

guard!(OnUse, PlayerInteractEvent, |st, e| {
    match (&e.action, &e.clicked_pos) {
        (InteractAction::RightClickBlock, Some(at)) => {
            let spot = (at.x, at.y, at.z);
            let held = e.player.get_inventory().get_item_in_hand(Hand::Right).map(|s| c::game::kind_of(&s)).unwrap_or_default();
            if defs::block_use(&e.block) > 0 {
                refused(st, &e.player, "interact", spot)
            } else if defs::is_bucket(&held) || defs::item_changes_world(&held) {
                refused(st, &e.player, "build", spot)
            } else {
                false
            }
        }
        _ => false,
    }
});

guard!(OnDrop, PlayerDropItemEvent, |st, e| { refused(st, &e.player, "item-drop", block_of(e.player.get_position())) });

guard!(OnExplode, EntityExplodeEvent, |st, e| { denied_anywhere(st, "explosions", block_of(e.position)) });

guard!(OnBlockExplode, BlockExplodeEvent, |st, e| { denied_anywhere(st, "explosions", (e.block_pos.x, e.block_pos.y, e.block_pos.z)) });

guard!(OnIgnite, BlockIgniteEvent, |st, e| { denied_anywhere(st, "fire-spread", (e.block_pos.x, e.block_pos.y, e.block_pos.z)) });

guard!(OnBurn, BlockBurnEvent, |st, e| { st.map.say("fire-spread", &e.target_world.get_dimension(), e.block_pos.x, e.block_pos.y, e.block_pos.z, None) == Say::Deny });

guard!(OnSpawn, CreatureSpawnEvent, |st, e| {
    // What admins and players put there on purpose still comes
    let reason = e.spawn_reason.to_ascii_lowercase();
    let on_purpose = ["command", "egg", "custom", "plugin", "spawner_egg", "dispense"].iter().any(|r| reason.contains(r));
    let at = block_of(e.position);
    !on_purpose && st.map.say("mob-spawning", &e.target_world.get_dimension(), at.0, at.1, at.2, None) == Say::Deny
});

struct OnHurt;
impl EventHandler<EntityDamageEvent> for OnHurt {
    fn handle(&self, server: Server, mut e: Data<EntityDamageEvent>) -> Data<EntityDamageEvent> {
        if e.cancelled {
            return e;
        }
        let Some(guard) = try_lock() else { return e };
        let Some(st) = guard.as_ref() else { return e };
        if st.map.regions.is_empty() {
            return e;
        }
        let Some(p) = server.get_all_players().into_iter().find(|p| p.as_entity().get_id() as i32 == e.entity_id) else { return e };
        let at = block_of(p.get_position());
        let world = p.get_world().get_dimension();
        let say = |flag: &str| st.map.say(flag, &world, at.0, at.1, at.2, None);
        if say("invincible") == Say::Allow || (matches!(e.damage_type, DamageType::Fall) && say("fall-damage") == Say::Deny) {
            e.cancelled = true;
        }
        e
    }
}

struct OnHit;
impl EventHandler<EntityDamageByEntityEvent> for OnHit {
    fn handle(&self, server: Server, mut e: Data<EntityDamageByEntityEvent>) -> Data<EntityDamageByEntityEvent> {
        if e.cancelled {
            return e;
        }
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        if st.map.regions.is_empty() {
            return e;
        }
        let players = server.get_all_players();
        let victim = players.iter().find(|p| p.as_entity().get_id() as i32 == e.entity_id);
        let attacker = players.iter().find(|p| p.as_entity().get_id() as i32 == e.damager_id);
        let Some(victim) = victim else { return e };
        let world = victim.get_world().get_dimension();
        let at = block_of(victim.get_position());
        match attacker {
            Some(a) => {
                let from = block_of(a.get_position());
                let no = st.map.say("pvp", &world, at.0, at.1, at.2, None) == Say::Deny || st.map.say("pvp", &world, from.0, from.1, from.2, None) == Say::Deny;
                if no {
                    e.cancelled = true;
                    deny(st, a, "pvp");
                }
            }
            None => {
                if st.map.say("mob-damage", &world, at.0, at.1, at.2, None) == Say::Deny {
                    e.cancelled = true;
                }
            }
        }
        e
    }
}

struct OnHunger;
impl EventHandler<FoodLevelChangeEvent> for OnHunger {
    fn handle(&self, server: Server, mut e: Data<FoodLevelChangeEvent>) -> Data<FoodLevelChangeEvent> {
        if e.cancelled {
            return e;
        }
        let Some(guard) = try_lock() else { return e };
        let Some(st) = guard.as_ref() else { return e };
        if st.map.regions.is_empty() {
            return e;
        }
        if let Some(p) = server.get_all_players().into_iter().find(|p| p.as_entity().get_id() as i32 == e.entity_id) {
            let at = block_of(p.get_position());
            if st.map.say("hunger", &p.get_world().get_dimension(), at.0, at.1, at.2, None) == Say::Deny {
                e.cancelled = true;
            }
        }
        e
    }
}

struct OnMove;
impl EventHandler<PlayerMoveEvent> for OnMove {
    fn handle(&self, _server: Server, mut e: Data<PlayerMoveEvent>) -> Data<PlayerMoveEvent> {
        if e.cancelled {
            return e;
        }
        let (from, to) = (block_of(e.from_position), block_of(e.to_position));
        if from == to {
            return e;
        }
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        if st.map.regions.is_empty() {
            return e;
        }
        let p = &e.player;
        let world = p.get_world().get_dimension();
        let me = c::game::key(p);
        let entering = st.map.say("entry", &world, to.0, to.1, to.2, Some(&me)) == Say::Deny && st.map.say("entry", &world, from.0, from.1, from.2, Some(&me)) != Say::Deny;
        if entering && !p.has_permission(&format!("{NS}:bypass")) {
            deny(st, p, "entry");
            e.cancelled = true;
            return e;
        }
        let now = st.map.ids_at(&world, to.0, to.1, to.2);
        let before = st.inside.insert(me, now.clone()).unwrap_or_default();
        if now != before {
            for id in now.iter().filter(|id| !before.contains(id)) {
                if let Some(text) = st.map.regions.iter().find(|r| r.id == *id).and_then(|r| r.flags.get("greeting")) {
                    c::action_bar(p, text);
                }
            }
            for id in before.iter().filter(|id| !now.contains(id)) {
                if let Some(text) = st.map.regions.iter().find(|r| r.id == *id).and_then(|r| r.flags.get("farewell")) {
                    c::action_bar(p, text);
                }
            }
        }
        e
    }
}

// ---- commands ----------------------------------------------------------------------

fn run(st: &mut State, sender: &CommandSender, server: &Server, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    let me = c::sender_player(sender);
    let who = c::sender_key(sender);
    let world = me.as_ref().map(|p| p.get_world().get_dimension()).unwrap_or_else(|| "minecraft:overworld".into());
    let usage = || {
        vec![
            msg(th, "&f/rg pos1 &7| &fpos2 &7| &fdefine <ชื่อ> &7| &fredefine <ชื่อ> &7| &fremove <ชื่อ> &7| &flist &7| &finfo [ชื่อ] &7| &fpriority <ชื่อ> <เลข>", "&f/rg pos1 &7| &fpos2 &7| &fdefine <id> &7| &fredefine <id> &7| &fremove <id> &7| &flist &7| &finfo [id] &7| &fpriority <id> <n>"),
            msg(th, "&f/rg flag <ชื่อ> <flag> <allow|deny|ข้อความ|none> &7| &faddmember|removemember|addowner|removeowner <ชื่อ> <ผู้เล่น>", "&f/rg flag <id> <flag> <allow|deny|text|none> &7| &faddmember|removemember|addowner|removeowner <id> <player>"),
            format!("&7flags: {} &8| &7{}", defs::SWITCHES.join(" "), defs::TEXTS.join(" ")),
        ]
    };
    let in_game = || vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")];
    let find = |st: &State, id: &str| st.map.regions.iter().position(|r| r.id == id && (!r.global() || r.world == world));
    match w.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        Some(which @ ("pos1" | "pos2")) => {
            let Some(p) = &me else { return in_game() };
            let at = block_of(p.get_position());
            let corner = Some((world.clone(), [at.0, at.1, at.2]));
            let entry = st.corners.entry(who).or_default();
            if which == "pos1" {
                entry.0 = corner;
            } else {
                entry.1 = corner;
            }
            vec![msg(th, &format!("&aตั้ง {which} ที่ {} {} {}", at.0, at.1, at.2), &format!("&a{which} set at {} {} {}", at.0, at.1, at.2))]
        }
        Some(verb @ ("define" | "redefine")) => {
            let Some(id) = w.get(1).map(|s| s.to_ascii_lowercase()).filter(|s| defs::valid_id(s) && s != GLOBAL) else { return vec![msg(th, "&cชื่อใช้ได้แค่ a-z, 0-9, _ และ -", "&cAn id may only have a-z, 0-9, _ and -")] };
            let Some((Some((w1, a)), Some((w2, b)))) = st.corners.get(&who).cloned() else { return vec![msg(th, "&cตั้ง /rg pos1 และ /rg pos2 ก่อน", "&cSet /rg pos1 and /rg pos2 first")] };
            if w1 != w2 {
                return vec![msg(th, "&cสองมุมต้องอยู่โลกเดียวกัน", "&cBoth corners must be in one world")];
            }
            let existing = st.map.regions.iter().position(|r| r.id == id);
            match (verb, existing) {
                ("define", Some(_)) => return vec![msg(th, "&cมีชื่อนี้แล้ว (ใช้ redefine)", "&cThat id exists (use redefine)")],
                ("redefine", None) => return vec![msg(th, "&cไม่มีพื้นที่นี้", "&cNo such region")],
                ("redefine", Some(at)) => {
                    let fresh = Region::new(&id, &w1, a, b);
                    let r = &mut st.map.regions[at];
                    (r.world, r.min, r.max) = (fresh.world, fresh.min, fresh.max);
                }
                _ => {
                    let mut r = Region::new(&id, &w1, a, b);
                    if let Some(p) = &me {
                        r.owners.insert(c::game::key(p), p.get_name());
                    }
                    st.map.regions.push(r);
                }
            }
            save(st);
            vec![msg(th, &format!("&aพื้นที่ {id} พร้อมแล้ว ตั้ง flag ด้วย /rg flag {id} <flag> <ค่า>"), &format!("&aRegion {id} is set: give it flags with /rg flag {id} <flag> <value>"))]
        }
        Some("remove") => match w.get(1).and_then(|id| find(st, id)) {
            Some(at) => {
                st.map.regions.remove(at);
                save(st);
                vec![msg(th, "&aลบแล้ว", "&aRemoved")]
            }
            None => vec![msg(th, "&cไม่มีพื้นที่นี้", "&cNo such region")],
        },
        Some("list") => {
            let mut out = vec![msg(th, &format!("&fพื้นที่ {} แห่ง", st.map.regions.len()), &format!("&f{} region(s)", st.map.regions.len()))];
            out.extend(st.map.regions.iter().map(|r| format!("&e{} &7{} &8| &7{} {:?} → {:?} &8| &7prio {}", r.id, r.world.trim_start_matches("minecraft:"), if r.global() { "global" } else { "" }, r.min, r.max, r.priority)));
            out
        }
        Some("info") => {
            let regions: Vec<Region> = match (w.get(1), &me) {
                (Some(id), _) => find(st, id).map(|at| vec![st.map.regions[at].clone()]).unwrap_or_default(),
                (None, Some(p)) => {
                    let at = block_of(p.get_position());
                    st.map.at(&world, at.0, at.1, at.2).into_iter().cloned().collect()
                }
                _ => Vec::new(),
            };
            if regions.is_empty() {
                return vec![msg(th, "&7ไม่มีพื้นที่ตรงนี้", "&7No region here")];
            }
            regions
                .iter()
                .flat_map(|r| {
                    vec![
                        format!("&e{} &7{:?} → {:?} prio {}", r.id, r.min, r.max, r.priority),
                        format!("&7flags: {}", r.flags.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", ")),
                        format!("&7owners: {} &8| &7members: {}", r.owners.values().cloned().collect::<Vec<_>>().join(", "), r.members.values().cloned().collect::<Vec<_>>().join(", ")),
                    ]
                })
                .collect()
        }
        Some("priority") => {
            let (Some(at), Some(n)) = (w.get(1).and_then(|id| find(st, id)), w.get(2).and_then(|n| n.parse::<i32>().ok())) else { return usage() };
            st.map.regions[at].priority = n;
            save(st);
            vec![msg(th, "&aตั้งลำดับแล้ว", "&aPriority set")]
        }
        Some("flag") => {
            let (Some(id), Some(flag)) = (w.get(1).map(|s| s.to_ascii_lowercase()), w.get(2).map(|s| s.to_ascii_lowercase())) else { return usage() };
            let value = w.iter().skip(3).cloned().collect::<Vec<_>>().join(" ");
            let value = match defs::flag_value(&flag, &value) {
                Ok(v) => v,
                Err((t, e)) => return vec![msg(th, &format!("&c{t}"), &format!("&c{e}"))],
            };
            if id == GLOBAL && find(st, GLOBAL).is_none() {
                st.map.regions.push(Region::new(GLOBAL, &world, [0, 0, 0], [0, 0, 0]));
            }
            let Some(at) = find(st, &id) else { return vec![msg(th, "&cไม่มีพื้นที่นี้", "&cNo such region")] };
            match value {
                Some(v) => {
                    st.map.regions[at].flags.insert(flag, v);
                }
                None => {
                    st.map.regions[at].flags.remove(&flag);
                }
            }
            save(st);
            vec![msg(th, "&aตั้ง flag แล้ว", "&aFlag set")]
        }
        Some(verb @ ("addmember" | "removemember" | "addowner" | "removeowner")) => {
            let (Some(at), Some(name)) = (w.get(1).and_then(|id| find(st, id)), w.get(2)) else { return usage() };
            let found = server.get_all_players().into_iter().find(|p| p.get_name().eq_ignore_ascii_case(name)).map(|p| (c::game::key(&p), p.get_name())).or_else(|| {
                st.map.regions.iter().flat_map(|r| r.members.iter().chain(r.owners.iter())).find(|(_, n)| n.eq_ignore_ascii_case(name)).map(|(k, n)| (k.clone(), n.clone()))
            });
            let Some((key, name)) = found else { return vec![msg(th, "&cไม่รู้จักผู้เล่นนี้ (ต้องออนไลน์)", "&cUnknown player (they must be online)")] };
            let r = &mut st.map.regions[at];
            let list = if verb.ends_with("member") { &mut r.members } else { &mut r.owners };
            if verb.starts_with("add") {
                list.insert(key, name);
            } else {
                list.remove(&key);
            }
            save(st);
            vec![msg(th, "&aแก้รายชื่อแล้ว", "&aDone")]
        }
        Some("reload") => {
            st.cfg = c::load_config::<Config>("config.toml").cleaned();
            st.map = c::load_json(FILE).unwrap_or_default();
            vec![msg(st.cfg.thai(), &format!("&aอ่านใหม่แล้ว: {} พื้นที่", st.map.regions.len()), &format!("&aRead again: {} region(s)", st.map.regions.len()))]
        }
        Some("version") => vec![format!("Regions {VERSION}")],
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

struct Regions;

impl Plugin for Regions {
    fn new() -> Self {
        Regions
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Regions of the world with rules: no building, no PvP, no mobs and more".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Make regions and set their flags".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let _ = context.register_permission(&Permission { node: format!("{NS}:bypass"), description: "Build, use and go anywhere".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let cfg = c::load_config::<Config>("config.toml").cleaned();
        let map: Map = c::load_json(FILE).unwrap_or_default();
        c::log_info(&format!("{} region(s)", map.regions.len()));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg, map, corners: HashMap::new(), inside: HashMap::new(), told: HashMap::new() });
        let cmd = Command::new(&["rg".to_owned(), "region".to_owned(), NS.to_owned()], "Regions").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:admin"));
        let b = true;
        context.register_event_handler(OnBreak, EventPriority::Normal, b)?;
        context.register_event_handler(OnPlace, EventPriority::Normal, b)?;
        context.register_event_handler(OnUse, EventPriority::Normal, b)?;
        context.register_event_handler(OnDrop, EventPriority::Normal, b)?;
        context.register_event_handler(OnExplode, EventPriority::Normal, b)?;
        context.register_event_handler(OnBlockExplode, EventPriority::Normal, b)?;
        context.register_event_handler(OnIgnite, EventPriority::Normal, b)?;
        context.register_event_handler(OnBurn, EventPriority::Normal, b)?;
        context.register_event_handler(OnSpawn, EventPriority::Normal, b)?;
        context.register_event_handler(OnHurt, EventPriority::Normal, b)?;
        context.register_event_handler(OnHit, EventPriority::Normal, b)?;
        context.register_event_handler(OnHunger, EventPriority::Normal, b)?;
        context.register_event_handler(OnMove, EventPriority::Normal, b)?;
        c::mark_running(VERSION);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::mark_stopped();
        Ok(())
    }

    /// Other plugins: `{"kind":"flag","flag","world","x","y","z","player"?}` answers
    /// `{"ok":true,"say":"allow"|"deny"|"unset"}`.
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let guard = try_lock().ok_or("busy")?;
        let st = guard.as_ref().ok_or("not loaded")?;
        if v.get("kind").and_then(serde_json::Value::as_str) != Some("flag") {
            return Err("unknown kind".into());
        }
        let num = |k: &str| v.get(k).and_then(serde_json::Value::as_f64).unwrap_or(0.0).floor() as i32;
        let text = |k: &str| v.get(k).and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
        let player = v.get("player").and_then(serde_json::Value::as_str).map(|p| p.replace('-', "").to_ascii_lowercase());
        let say = st.map.say(&text("flag"), &text("world"), num("x"), num("y"), num("z"), player.as_deref());
        let word = match say {
            Say::Allow => "allow",
            Say::Deny => "deny",
            Say::Unset => "unset",
        };
        serde_json::to_vec(&serde_json::json!({ "ok": true, "say": word })).map_err(|e| e.to_string())
    }
}

register_plugin!(Regions);

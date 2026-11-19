//! Pumpkin WASM glue for VeinMiner: breaking the rest of a vein after the first block.
//!
//! The first block breaks as the game breaks it. The rest of the vein is broken the tick after,
//! by this plugin: Pumpkin 0.2.2 cannot drop items from a plugin, so their drops go into the
//! inventory (the game's usual drop; the block itself with Silk Touch; Fortune is not counted),
//! and the vein stops where the inventory has no room or the tool would break. Land of the Claims
//! plugin that the player may not build on is left alone. These blocks fire no break event, so
//! other plugins (Jobs, Skills, BlockLog) do not hear of them.
use std::collections::HashSet;
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::{BlockPos, GameMode, Hand};
use pumpkin_plugin_api::data_components::DataComponent;
use pumpkin_plugin_api::events::{BlockBreakEvent, EventHandler, EventPriority, FromIntoEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::wit::pumpkin::plugin::enchantments::Enchantment;
use pumpkin_plugin_api::wit::pumpkin::plugin::world::BlockFlags;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::defs::{self, Config, Family, NS};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&6VeinMiner&8]&r ";
const FILE: &str = "off.json";

struct Job {
    player: String,
    start: (i32, i32, i32),
    family: Family,
}

struct State {
    cfg: Config,
    /// Players who switched it off.
    off: HashSet<String>,
    jobs: Vec<Job>,
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

fn read_varint(bytes: &[u8]) -> u32 {
    let mut n = 0u32;
    for (i, b) in bytes.iter().take(5).enumerate() {
        n |= u32::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            break;
        }
    }
    n
}

/// How many of its drop a block gives (the game's middle of the range).
fn drop_count(block: &str) -> u32 {
    match block.strip_prefix("deepslate_").unwrap_or(block) {
        "redstone_ore" => 4,
        "lapis_ore" => 6,
        "copper_ore" => 3,
        "nether_gold_ore" => 4,
        _ => 1,
    }
}

fn mine(st: &State, server: &Server, job: &Job) {
    let th = st.cfg.thai();
    let Some(p) = c::game::online(server, &job.player) else { return };
    let inv = p.get_inventory();
    let Some(tool) = inv.get_item_in_hand(Hand::Right) else { return };
    let tool_kind = c::game::kind_of(&tool);
    if !job.family.tool_fits(&tool_kind) {
        return;
    }
    let silk = tool.get_enchantments().iter().any(|e| matches!(e.enchantment, Enchantment::SilkTouch));
    let world = p.get_world();
    let name_at = |pos: (i32, i32, i32)| c::deeds::bare(&world.get_block_state(BlockPos { x: pos.0, y: pos.1, z: pos.2 }).block_name);
    let family = &job.family;
    let mut same = |pos: (i32, i32, i32)| st.cfg.family(&name_at(pos)).as_ref() == Some(family);
    let blocks = defs::vein(job.start, st.cfg.max_blocks as usize - 1, &mut same);
    if blocks.is_empty() {
        return;
    }
    // How much wear the tool can still take
    let damage = tool.get_components().iter().find(|comp| matches!(comp.component, DataComponent::Damage)).map_or(0, |comp| read_varint(&comp.value));
    let mut budget = match (st.cfg.wear_tool, defs::max_damage(&tool_kind)) {
        (true, Some(max)) => max.saturating_sub(damage + 1),
        _ => u32::MAX,
    };
    let creative = matches!(p.get_gamemode(), GameMode::Creative);
    let mut allowed_columns: Vec<((i32, i32), bool)> = Vec::new();
    let mut broken = 0u32;
    let mut stopped = None;
    for pos in blocks {
        if budget == 0 {
            stopped = Some(msg(th, "&6หยุดก่อนเครื่องมือพัง", "&6Stopped before your tool breaks"));
            break;
        }
        let column = (pos.0, pos.2);
        let allowed = match allowed_columns.iter().find(|(c, _)| *c == column) {
            Some((_, a)) => *a,
            None => {
                let a = c::game::may(&p, f64::from(pos.0), f64::from(pos.2), "build").is_ok();
                allowed_columns.push((column, a));
                a
            }
        };
        if !allowed {
            continue;
        }
        let block = name_at(pos);
        if !creative {
            let (item, count) = if silk { (format!("minecraft:{block}"), 1) } else { (defs::default_drop(&block), drop_count(&block)) };
            if c::game::give_game(&p, &item, count) > 0 {
                stopped = Some(msg(th, "&6กระเป๋าเต็ม หยุดขุดต่อ", "&6Inventory full: the vein stops here"));
                break;
            }
        }
        world.set_block_by_name(BlockPos { x: pos.0, y: pos.1, z: pos.2 }, "minecraft:air".into(), BlockFlags::NOTIFY_NEIGHBORS | BlockFlags::NOTIFY_LISTENERS | BlockFlags::SKIP_DROPS);
        broken += 1;
        if !creative && budget != u32::MAX {
            budget -= 1;
        }
    }
    if broken > 0 && !creative && st.cfg.wear_tool && defs::max_damage(&tool_kind).is_some() {
        tool.set_component(DataComponent::Damage, &varint(damage + broken));
        inv.set_item_in_hand(Hand::Right, Some(tool));
    }
    if let Some(line) = stopped {
        c::action_bar(&p, &line);
    } else if broken > 0 {
        c::action_bar(&p, &msg(th, &format!("&7ขุดเพิ่ม {broken} บล็อก"), &format!("&7{broken} more block(s)")));
    }
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    if st.jobs.is_empty() {
        return;
    }
    let jobs = std::mem::take(&mut st.jobs);
    for job in &jobs {
        mine(st, &server, job);
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

struct OnBreak;
impl EventHandler<BlockBreakEvent> for OnBreak {
    fn handle(&self, _server: Server, e: Data<BlockBreakEvent>) -> Data<BlockBreakEvent> {
        let Some(p) = &e.player else { return e };
        if e.cancelled || matches!(p.get_gamemode(), GameMode::Spectator) {
            return e;
        }
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let me = c::game::key(p);
        if st.off.contains(&me) || (st.cfg.sneak && !p.as_entity().is_sneaking()) || !p.has_permission(&format!("{NS}:use")) {
            return e;
        }
        let Some(family) = st.cfg.family(&c::deeds::bare(&e.block)) else { return e };
        let fits = p.get_inventory().get_item_in_hand(Hand::Right).is_some_and(|t| family.tool_fits(&c::game::kind_of(&t)));
        if fits && st.jobs.len() < 64 {
            st.jobs.push(Job { player: me, start: (e.block_pos.x, e.block_pos.y, e.block_pos.z), family });
        }
        e
    }
}

fn run(st: &mut State, sender: &CommandSender, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    match w.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("reload") if c::has_perm(sender, &format!("{NS}:admin")) => {
            st.cfg = c::load_config::<Config>("config.toml").cleaned();
            vec![msg(st.cfg.thai(), "&aอ่านค่าตั้งใหม่แล้ว", "&aSettings read again")]
        }
        Some("version") => vec![format!("VeinMiner {VERSION}")],
        _ => {
            let Some(p) = c::sender_player(sender) else { return vec!["veinminer reload | version".into()] };
            let me = c::game::key(&p);
            let on = if st.off.remove(&me) {
                true
            } else {
                st.off.insert(me);
                false
            };
            c::save_json(FILE, &st.off);
            let how = if st.cfg.sneak { if th { " (ย่อแล้วขุด)" } else { " (sneak while you break)" } } else { "" };
            vec![if on { msg(th, &format!("&aเปิดขุดทั้งสายแล้ว{how}"), &format!("&aVein mining on{how}")) } else { msg(th, "&7ปิดขุดทั้งสายแล้ว", "&7Vein mining off") }]
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

struct VeinMiner;

impl Plugin for VeinMiner {
    fn new() -> Self {
        VeinMiner
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Break a whole vein of ore or a whole tree at once".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:use"), description: "Break veins whole".into(), default: PermissionDefault::Allow, children: vec![] });
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Reload VeinMiner".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let cfg = c::load_config::<Config>("config.toml").cleaned();
        let off: HashSet<String> = c::load_json(FILE).unwrap_or_default();
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg, off, jobs: Vec::new() });
        let cmd = Command::new(&[NS.to_owned(), "vm".to_owned()], "VeinMiner").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:use"));
        context.register_event_handler(OnBreak, EventPriority::Lowest, false)?;
        context.schedule_repeating_task(1, 1, on_tick);
        c::mark_running(VERSION);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::mark_stopped();
        Ok(())
    }
}

register_plugin!(VeinMiner);

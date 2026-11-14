//! Pumpkin WASM glue for BlockLog.
//!
//! Two locks, never held together by an event handler:
//! - `LIVE` is what handlers touch: who is inspecting, changes being watched, records waiting to be
//!   stored. While it is held only host calls that fire no event are made (reads of the world).
//! - `STATE` is the store, the jobs and the results. Only the tick and the command take it.
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mcsv_common as c;
use pumpkin_plugin_api::block_entity::{BlockEntityType, ContainerBlockEntity};
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::data_components::DataComponent;
use pumpkin_plugin_api::events::{
    BlockBreakEvent, BlockBurnEvent, BlockExplodeEvent, BlockFromToEvent, BlockGrowEvent, BlockPistonExtendEvent, BlockPistonRetractEvent, BlockPlaceEvent,
    BlockSpreadEvent, EntityChangeBlockEvent, EntityDamageByEntityEvent, EntityDeathEvent, EventHandler, EventPriority, FromIntoEvent, InventoryCloseEvent,
    InventoryMoveItemEvent, InventoryOpenEvent, LeavesDecayEvent, PlayerBucketFillEvent, PlayerChatEvent, PlayerCommandSendEvent, PlayerDeathEvent, PlayerDropItemEvent,
    PlayerInteractEvent, PlayerJoinEvent, PlayerLeaveEvent, ProjectileLaunchEvent, SignChangeEvent, StructureGrowEvent,
};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::wit::pumpkin::plugin::common::BlockPos;
use pumpkin_plugin_api::wit::pumpkin::plugin::event::InteractAction;
use pumpkin_plugin_api::wit::pumpkin::plugin::world as host_world;
use pumpkin_plugin_api::wit::pumpkin::plugin::world::BlockFlags;
use pumpkin_plugin_api::world::{BlockDirection, World};
use pumpkin_plugin_api::{Context, ItemStack, Plugin, PluginMetadata, Server, register_plugin};
use serde::{Deserialize, Serialize};

use crate::engine::{Job, Mode, Outcome, Purge, Stored, WorldEdit};
use crate::ipc;
use crate::items;
use crate::params::{self, ActionSet, ParamError, Params, Radius, world_key};
use crate::query::{Area, Query, Row, block_name};
use crate::record::{Action, MAX_ITEM, MAX_TEXT, TextRec, clean};
use crate::store::{MAX_NBT, Store, day_name};
use crate::timefmt;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NS: &str = "blocklog";
const AIR: u16 = 0;
/// Ticks between an event and the look at what it really did to the world.
const SETTLE_TICKS: u64 = 2;
/// Explosions snapshotted in one tick; a bigger chain is counted, not logged.
const MAX_EXPLOSIONS: u32 = 8;
/// Changes made by the world itself (flow, pistons, blocks losing their support) watched in one tick.
const MAX_NATURAL: u32 = 256;
/// Trees whose growth is snapshotted in one tick (each reads some 6,000 blocks twice).
const MAX_TREES: u32 = 2;
/// Ticks a piston is given to finish moving its blocks.
const PISTON_TICKS: u64 = 4;
/// Changes watched at once; more than this is an event flood.
const MAX_WATCHES: usize = 20_000;
/// Jobs (lookups, rollbacks) running at once on the whole server.
const MAX_JOBS: usize = 8;
/// A command works this long before its job moves to the tick loop.
const INLINE: Duration = Duration::from_millis(10);
/// Youngest data a purge may delete: a player's, and the console's.
const PURGE_MIN_PLAYER: u64 = 30 * 86_400;
const PURGE_MIN_CONSOLE: u64 = 86_400;

type P = (i32, i32, i32);

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct LogCfg {
    block_break: bool,
    block_place: bool,
    bucket: bool,
    burn: bool,
    explosion: bool,
    sign: bool,
    container: bool,
    chat: bool,
    command: bool,
    session: bool,
    kill: bool,
    item: bool,
    /// Edits other plugins report (WorldTools's world edits).
    world_edit: bool,
    /// Fire a player lights (flint and steel, fire charge) and TNT a player primes.
    ignite: bool,
    /// Fire spreading to a new block (`#fire`).
    fire_spread: bool,
    /// Blocks the world itself breaks: washed away by water, losing their support (`#natural`).
    natural_break: bool,
    /// Blocks an enderman takes (`#enderman`).
    entity_change: bool,
    /// Right clicks on doors, levers, buttons and containers.
    click: bool,
    /// The noisy ones, off unless asked for. A change takes effect at the next server start.
    leaf_decay: bool,
    growth: bool,
    tree_growth: bool,
    flow: bool,
    piston: bool,
    /// Items a hopper pushes into a container (`#hopper`).
    hopper: bool,
}

impl Default for LogCfg {
    fn default() -> Self {
        LogCfg {
            block_break: true,
            block_place: true,
            bucket: true,
            burn: true,
            explosion: true,
            sign: true,
            container: true,
            chat: true,
            command: true,
            session: true,
            kill: true,
            item: true,
            world_edit: true,
            ignite: true,
            fire_spread: true,
            natural_break: true,
            entity_change: true,
            click: true,
            leaf_decay: false,
            growth: false,
            tree_growth: false,
            flow: false,
            piston: false,
            hopper: false,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct Config {
    /// th | en
    language: String,
    /// Days of history kept; older days are deleted by themselves. 0 = keep everything.
    retention_days: u32,
    /// Seconds between writes to disk (1..=60). A crash loses at most this much.
    flush_interval: u32,
    /// Most rows one lookup keeps (the newest ones).
    max_lookup_rows: usize,
    /// Rows per page.
    page_size: usize,
    /// Largest `r:<number>` a player may use.
    max_radius: u32,
    /// Milliseconds of each server tick that lookups and rollbacks may use (1..=40).
    tick_ms: u32,
    /// What to log.
    log: LogCfg,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), retention_days: 90, flush_interval: 5, max_lookup_rows: 2000, page_size: 7, max_radius: 500, tick_ms: 8, log: LogCfg::default() }
    }
}

fn load_config() -> Config {
    let mut cfg: Config = c::load_config("config.toml");
    cfg.flush_interval = cfg.flush_interval.clamp(1, 60);
    cfg.max_lookup_rows = cfg.max_lookup_rows.clamp(10, 100_000);
    cfg.page_size = cfg.page_size.clamp(1, 50);
    cfg.max_radius = cfg.max_radius.clamp(1, 100_000);
    cfg.tick_ms = cfg.tick_ms.clamp(1, 40);
    // `load_config` adds missing top-level options to an older file; the switches under [log] are
    // one level down, so an older file gets the new ones here
    let on_disk = std::fs::read_to_string(c::data_path("config.toml")).ok().and_then(|t| t.parse::<toml::Table>().ok());
    let known = toml::Table::try_from(cfg.log).unwrap_or_default();
    if on_disk.is_some_and(|t| t.get("log").and_then(|l| l.as_table()).is_none_or(|have| known.keys().any(|k| !have.contains_key(k)))) {
        c::save_config("config.toml", &cfg);
    }
    cfg
}

#[derive(Clone, Copy, PartialEq)]
enum WatchKind {
    Break,
    Place,
    BucketEmpty,
    BucketFill,
    Explosion,
    /// A player used flint and steel or a fire charge on a block.
    Ignite,
    /// The world broke a block (no player in the event).
    Natural,
    /// A fluid is about to flow into a block.
    Flow,
    /// A piston is about to move the blocks in front of it.
    Piston,
    /// A sapling is about to become a tree.
    Tree,
}

/// A change that an event announced. The event comes before the change and may still be cancelled,
/// so the blocks are read now and again a moment later: what differs is what happened.
struct Watch {
    kind: WatchKind,
    user: String,
    dim: String,
    born: u64,
    /// The tick at which the world is looked at again.
    due: u64,
    time: u32,
    /// The block of the event first, then its neighbours (or the blast area), with their state before.
    cells: Vec<(P, u16)>,
    /// What is inside the block entities among the cells (chest contents, sign text), taken
    /// before the change: a rollback of the break gives it back.
    nbt: Vec<(P, Vec<u8>)>,
}

struct OutBlock {
    world: String,
    time: u32,
    user: String,
    pos: P,
    action: Action,
    old: u16,
    new: u16,
    nbt: Option<Vec<u8>>,
}

/// Block changes another plugin reported in one message.
struct Edits {
    user: String,
    world: String,
    time: u32,
    changes: Vec<ipc::Change>,
}

struct InspectReq {
    player: String,
    world: String,
    pos: P,
    container: bool,
}

/// A container a player has (probably) opened: what was in it then.
struct Opened {
    dim: String,
    at: P,
    parts: Vec<P>,
    before: HashMap<String, u32>,
    opened: bool,
    born: Instant,
}

struct Live {
    tick: u64,
    log: LogCfg,
    /// Players with the inspector on.
    inspectors: HashSet<String>,
    clicks: HashMap<String, (P, bool, Instant)>,
    watches: Vec<Watch>,
    blocks: Vec<OutBlock>,
    texts: Vec<TextRec>,
    inspects: Vec<InspectReq>,
    containers: HashMap<String, Opened>,
    /// Entity id -> the player who hit it last, and when (tick).
    hits: HashMap<i32, (String, u64)>,
    /// Entity id of each online player.
    ids: HashMap<i32, String>,
    explosions: u32,
    /// Events not logged because too many came at once.
    dropped: u64,
    /// Arrow entity id -> the player who shot it, and when (tick).
    shots: HashMap<i32, (String, u64)>,
    /// Natural changes and trees watched this tick.
    natural: u32,
    trees: u32,
    /// Events without a world that two loaded worlds could both have fired: not logged.
    ambiguous: u64,
    /// Reported by other plugins, waiting for the tick.
    edits: Vec<Edits>,
    /// Chat lines a chat plugin sent itself (name, message, time).
    notices: Vec<(String, String, u32)>,
    /// The last block each player clicked, against the second event of the same click.
    clicked: HashMap<String, (P, Instant)>,
    /// The last chat lines logged from the event, against a notice for the same line.
    said: Vec<(String, String, Instant)>,
    /// Edit messages received, the changes in them, and the time spent receiving them.
    ipc_messages: u64,
    ipc_changes: u64,
    ipc_micros: u64,
}

static LIVE: Mutex<Option<Live>> = Mutex::new(None);

#[derive(Clone, PartialEq)]
enum Owner {
    Console,
    Player(String),
}

impl Owner {
    fn key(&self) -> &str {
        match self {
            Owner::Console => "console",
            Owner::Player(k) => k,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Lookup,
    Inspect,
    Count,
    Rollback,
    Restore,
    /// Undo of a rollback (a restore) or of a restore (a rollback).
    Undo,
}

struct Running {
    owner: Owner,
    kind: Kind,
    job: Job,
    title: String,
    started: Instant,
    busy: Duration,
}

struct Results {
    rows: Vec<Row>,
    truncated: bool,
    title: String,
    at: Instant,
}

struct State {
    cfg: Config,
    store: Store,
    /// State string -> state id on this server (`None` = unknown block).
    ids: HashMap<String, Option<u16>>,
    jobs: Vec<Running>,
    results: HashMap<String, Results>,
    /// The last rollback or restore of each sender, for `/co undo`.
    last_op: HashMap<String, (Mode, Query)>,
    purge: Option<(Purge, Owner, bool)>,
    last_flush: Instant,
    next_auto_purge: Instant,
    loaded: Instant,
    tick: u64,
    /// State id -> its line in the state table.
    state_ids: HashMap<u16, u32>,
    /// Time spent storing reported edits.
    ipc_store_micros: u64,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// True while BlockLog itself changes blocks (a rollback or restore): what breaks as a result
/// (the other half of a door) is not something the world did.
static APPLYING: AtomicBool = AtomicBool::new(false);
/// State id -> state string, asked from the server once each.
static NAMES: Mutex<Option<HashMap<u16, String>>> = Mutex::new(None);

type Guard<T> = std::sync::MutexGuard<'static, Option<T>>;

/// Never blocks: WASM has one thread and Pumpkin calls handlers from inside host calls.
fn try_lock<T>(m: &'static Mutex<Option<T>>) -> Option<Guard<T>> {
    match m.try_lock() {
        Ok(g) => Some(g),
        Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

fn key(p: &Player) -> String {
    let id = p.get_id();
    format!("{:016x}{:016x}", id.high, id.low)
}

fn player_of(server: &Server, key: &str) -> Option<Player> {
    let high = u64::from_str_radix(key.get(..16)?, 16).ok()?;
    let low = u64::from_str_radix(key.get(16..)?, 16).ok()?;
    server.get_player_by_uuid(pumpkin_plugin_api::uuid::Uuid { high, low })
}

fn pos(p: P) -> BlockPos {
    BlockPos { x: p.0, y: p.1, z: p.2 }
}

fn feet(p: &Player) -> P {
    let (x, y, z) = p.get_position();
    (x.floor() as i32, y.floor() as i32, z.floor() as i32)
}

fn now() -> u32 {
    c::now_secs().min(u64::from(u32::MAX)) as u32
}

/// The block above comes first: `settle` follows a column upwards from it.
const SIDES: [P; 6] = [(0, 1, 0), (0, -1, 0), (1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1)];
/// Blocks above the neighbour above that a break also remembers: what stood on the broken block
/// can be taller than one block (a door, a cactus, sugar cane).
const COLUMN: i32 = 5;

fn add(a: P, b: P) -> P {
    (a.0.saturating_add(b.0), a.1.saturating_add(b.1), a.2.saturating_add(b.2))
}

/// `oak_stairs[facing=north,half=bottom]` for a state id: a name that survives a Minecraft update,
/// which the number does not.
fn state_string(id: u16) -> String {
    let mut guard = try_lock(&NAMES);
    if let Some(name) = guard.as_ref().and_then(|g| g.as_ref()).and_then(|m| m.get(&id)) {
        return name.clone();
    }
    let name = match host_world::block_state_to_info(id) {
        Some(info) => {
            let block = info.name.strip_prefix("minecraft:").unwrap_or(&info.name).to_owned();
            if info.properties.is_empty() {
                block
            } else {
                format!("{block}[{}]", info.properties.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(","))
            }
        }
        None => "air".to_owned(),
    };
    if let Some(g) = guard.as_mut() {
        g.get_or_insert_with(HashMap::new).insert(id, name.clone());
    }
    name
}

/// The state id of a state string on this server. A block this version does not have = `None`;
/// a block it has with properties it does not = the block's default state.
fn find_state(state: &str) -> Option<u16> {
    let (name, props) = match state.split_once('[') {
        Some((name, rest)) => (name, rest.trim_end_matches(']')),
        None => (state, ""),
    };
    let block = host_world::get_block_by_name(name)?;
    if props.is_empty() {
        return Some(block.default_state_id);
    }
    let mut want: Vec<(&str, &str)> = props.split(',').filter_map(|kv| kv.split_once('=')).collect();
    want.sort_unstable();
    for id in &block.state_ids {
        let have = host_world::get_block_properties(*id);
        let mut have: Vec<(&str, &str)> = have.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        have.sort_unstable();
        if have == want {
            return Some(*id);
        }
    }
    Some(block.default_state_id)
}

/// Like WorldTools: no neighbour updates, so a rollback does not start a wave of block physics.
fn flags() -> BlockFlags {
    BlockFlags::NOTIFY_LISTENERS | BlockFlags::FORCE_STATE | BlockFlags::SKIP_DROPS
}

/// The server as a rollback sees it.
struct ServerWorld<'a> {
    server: &'a Server,
    worlds: Option<Vec<(String, World)>>,
    ids: &'a mut HashMap<String, Option<u16>>,
}

impl ServerWorld<'_> {
    fn world(&mut self, name: &str) -> Option<&World> {
        let server = self.server;
        let worlds = self.worlds.get_or_insert_with(|| server.get_all_worlds().into_iter().map(|w| (world_key(&w.get_dimension()), w)).collect());
        worlds.iter().find(|(k, _)| k == name).map(|(_, w)| w)
    }
}

impl WorldEdit for ServerWorld<'_> {
    fn chunk_loaded(&mut self, world: &str, cx: i32, cz: i32) -> bool {
        self.world(world).is_some_and(|w| w.get_chunk(cx, cz).is_some())
    }

    fn change_container(&mut self, world: &str, at: P, item: &str, count: i64) -> Stored {
        match self.world(world) {
            Some(w) => change_container(w, at, item, count),
            None => Stored::NoContainer,
        }
    }

    fn set_block(&mut self, world: &str, at: P, state: &str, nbt: Option<&[u8]>) -> bool {
        let id = match self.ids.get(state) {
            Some(id) => *id,
            None => {
                let id = find_state(state);
                self.ids.insert(state.to_owned(), id);
                id
            }
        };
        let (Some(id), Some(w)) = (id, self.world(world)) else { return false };
        if w.get_block_state_id(pos(at)) != id {
            match nbt {
                // What was inside when it was broken (chest contents, sign text) goes back in. The
                // server makes a block entity from stored data only where none exists yet, and
                // placing a block makes an empty one: so place it without that step.
                Some(data) => {
                    w.set_block_state(pos(at), id, flags() | BlockFlags::SKIP_BLOCK_ADDED_CALLBACK);
                    if let Err(e) = w.set_block_entity_nbt(pos(at), data) {
                        c::log_info(&format!("block entity at {} {} {} not restored: {e}", at.0, at.1, at.2));
                    }
                }
                None => w.set_block_state(pos(at), id, flags()),
            }
        }
        true
    }
}

fn msg(th: bool, thai: &str, english: &str) -> String {
    format!("&8[&bBlockLog&8]&r {}", if th { thai } else { english })
}

/// Player text shown to staff must not bring its own colours.
fn safe(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars().peekable();
    while let Some(ch) = it.next() {
        if ch == '&' && it.peek().is_some_and(|n| n.is_ascii_alphanumeric() || *n == '#') {
            out.push('＆');
        } else if ch != '§' {
            out.push(ch);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Watching changes
// ---------------------------------------------------------------------------------------------

fn fluid(state: u16) -> bool {
    let s = state_string(state);
    matches!(block_name(&s), "water" | "lava" | "powder_snow") || s.contains("waterlogged=true")
}

/// Compare a watch with the world. Returns whether the watch is finished.
/// `forced` = looked at before its time (another event hit the same block): if nothing changed
/// yet, the change may still be coming, so the watch stays.
fn settle(w: &Watch, world: &World, others: &HashSet<P>, out: &mut Vec<OutBlock>, forced: bool) -> bool {
    let key = world_key(&w.dim);
    let mut push = |at: P, action: Action, old: u16, new: u16, user: &str| {
        // A block that is gone takes what was inside it along: kept for the rollback
        let nbt = if new == AIR { w.nbt.iter().find(|(p, _)| *p == at).map(|(_, data)| data.clone()) } else { None };
        out.push(OutBlock { world: key.clone(), time: w.time, user: user.to_owned(), pos: at, action, old, new, nbt });
    };
    let mut chunk: Option<(i32, i32, bool)> = None;
    let mut loaded = |at: P| match chunk {
        Some((x, z, l)) if (x, z) == (at.0 >> 4, at.2 >> 4) => l,
        _ => {
            let l = world.get_chunk(at.0 >> 4, at.2 >> 4).is_some();
            chunk = Some((at.0 >> 4, at.2 >> 4, l));
            l
        }
    };
    let read = |at: P| world.get_block_state_id(pos(at));
    let named = |state: u16| block_name(&state_string(state)).to_owned();
    let Some(&(p0, old0)) = w.cells.first() else { return true };
    match w.kind {
        WatchKind::Explosion => {
            for &(at, old) in &w.cells {
                if loaded(at) && read(at) == AIR {
                    push(at, Action::Explode, old, AIR, "#explosion");
                }
            }
            true
        }
        WatchKind::BucketEmpty => {
            let mut any = false;
            for &(at, old) in &w.cells {
                if !loaded(at) {
                    continue;
                }
                let new = read(at);
                if new != old && fluid(new) {
                    push(at, Action::BucketEmpty, old, new, &w.user);
                    any = true;
                }
            }
            any || !forced
        }
        WatchKind::Ignite => {
            let mut any = false;
            for (i, &(at, old)) in w.cells.iter().enumerate() {
                if others.contains(&at) && i > 0 || !loaded(at) {
                    continue;
                }
                let new = read(at);
                if new == old {
                    continue;
                }
                if matches!(named(new).as_str(), "fire" | "soul_fire") {
                    push(at, Action::Place, old, new, &w.user);
                    any = true;
                } else if i == 0 && new == AIR && named(old) == "tnt" {
                    // Primed: the block became an entity
                    push(at, Action::Break, old, new, &w.user);
                    any = true;
                }
            }
            any || !forced
        }
        WatchKind::Flow => {
            if loaded(p0) {
                let new = read(p0);
                if new != old0 && fluid(new) {
                    push(p0, Action::Place, old0, new, if named(new) == "lava" { "#lava" } else { "#water" });
                }
            }
            true
        }
        WatchKind::Natural => {
            if loaded(p0) {
                let new = read(p0);
                if new != old0 {
                    push(p0, Action::Break, old0, new, "#natural");
                }
            }
            true
        }
        WatchKind::Piston => {
            let moving = |state: u16| matches!(named(state).as_str(), "moving_piston" | "piston_head");
            // Still pushing: look again a little later
            if !forced && w.due < w.born + 40 && w.cells.iter().any(|&(at, _)| loaded(at) && named(read(at)) == "moving_piston") {
                return false;
            }
            for &(at, old) in &w.cells {
                if !loaded(at) {
                    continue;
                }
                let new = read(at);
                // The piston's own head coming and going is not a block that was moved; a block the
                // head now stands in was pushed away
                if new != old && !moving(old) && named(new) != "moving_piston" {
                    push(at, if new == AIR || moving(new) { Action::Break } else { Action::Place }, old, new, "#piston");
                }
            }
            true
        }
        WatchKind::Tree => {
            for &(at, old) in &w.cells {
                if !loaded(at) {
                    continue;
                }
                let new = read(at);
                if new != old {
                    push(at, if new == AIR { Action::Break } else { Action::Place }, old, new, "#tree");
                }
            }
            true
        }
        WatchKind::Break | WatchKind::Place | WatchKind::BucketFill => {
            if !loaded(p0) {
                // The chunk went away in between: a break is believed, the rest cannot be known
                if w.kind == WatchKind::Break {
                    push(p0, Action::Break, old0, AIR, &w.user);
                }
                return true;
            }
            let new0 = read(p0);
            if new0 == old0 {
                // Cancelled by another plugin, or not done yet
                return !forced;
            }
            let action = match w.kind {
                WatchKind::Break => Action::Break,
                WatchKind::Place => Action::Place,
                _ => Action::BucketFill,
            };
            push(p0, action, old0, new0, &w.user);
            // Whether the column above the broken block is still falling apart
            let mut column = false;
            for (i, &(at, old)) in w.cells.iter().enumerate().skip(1) {
                let above = i > SIDES.len();
                if above && !column {
                    break;
                }
                let gone = !others.contains(&at) && loaded(at) && old != AIR && read(at) == AIR;
                if i == 1 || above {
                    column = gone;
                }
                if others.contains(&at) || !loaded(at) {
                    continue;
                }
                let new = read(at);
                if new == old {
                    continue;
                }
                match w.kind {
                    // What stood on or hung from the broken block went with it (torch, door, cactus)
                    WatchKind::Break if gone => push(at, Action::Break, old, new, &w.user),
                    // The other half of a door, bed or tall plant
                    WatchKind::Place if old == AIR && named(new) == named(new0) => push(at, Action::Place, old, new, &w.user),
                    _ => {}
                }
            }
            true
        }
    }
}

/// Settle the watches of a world that `pick` chooses.
fn settle_where(live: &mut Live, world: &World, dim: &str, forced: bool, pick: impl Fn(&Watch) -> bool) {
    // Blocks another watch is about are that watch's to report, not this one's neighbours. Taken
    // before any watch is settled: one settled a moment ago in this same pass still counts.
    let others: HashSet<P> = live.watches.iter().filter(|o| o.dim == dim && !matches!(o.kind, WatchKind::Explosion | WatchKind::Tree | WatchKind::Piston)).filter_map(|o| o.cells.first().map(|c| c.0)).collect();
    let tick = live.tick;
    // In the order the events came: a rollback undoes the records of one second last-first, and
    // "water broke the torch" must stay before "water flowed in"
    for mut w in std::mem::take(&mut live.watches) {
        if w.dim == dim && pick(&w) {
            if settle(&w, world, &others, &mut live.blocks, forced) {
                continue;
            }
            if !forced {
                w.due = tick + SETTLE_TICKS;
            }
        }
        live.watches.push(w);
    }
}

/// What is inside the block entity there, if there is one and it is not huge.
fn inside(world: &World, at: P) -> Option<Vec<u8>> {
    world.get_block_entity_nbt(pos(at)).filter(|data| !data.is_empty() && data.len() <= MAX_NBT)
}

/// Start watching the block of a player's event.
fn watch(live: &mut Live, kind: WatchKind, p: &Player, at: P) {
    if live.watches.len() >= MAX_WATCHES {
        live.dropped += 1;
        return;
    }
    let world = p.get_world();
    let dim = world.get_dimension();
    let user = p.get_name();
    let same = |w: &Watch| !matches!(w.kind, WatchKind::Explosion | WatchKind::Tree | WatchKind::Piston) && w.cells.first().is_some_and(|c| c.0 == at);
    // The same block again before the first change was looked at (break then place in one tick)
    settle_where(live, &world, &dim, true, same);
    // The same click twice (a client sends "use on block" and "use" for one right click)
    if live.watches.iter().any(|w| w.kind == kind && w.dim == dim && w.user == user && same(w)) {
        return;
    }
    let mut cells = vec![(at, world.get_block_state_id(pos(at)))];
    if kind != WatchKind::BucketFill {
        for side in SIDES {
            let n = add(at, side);
            cells.push((n, world.get_block_state_id(pos(n))));
        }
    }
    let mut nbt = Vec::new();
    if kind == WatchKind::Break {
        if cells[1].1 != AIR {
            for up in 2..=1 + COLUMN {
                let n = add(at, (0, up, 0));
                let state = world.get_block_state_id(pos(n));
                if state == AIR {
                    break;
                }
                cells.push((n, state));
            }
        }
        if let Some(data) = inside(&world, at) {
            nbt.push((at, data));
        }
    }
    live.watches.push(Watch { kind, user, dim, born: live.tick, due: live.tick + SETTLE_TICKS, time: now(), cells, nbt });
}

/// The world an event without a world happened in: the only one that has the place loaded.
/// With the same place loaded in two worlds nobody can tell, and the event is not logged.
fn only_world(live: &mut Live, server: &Server, at: P) -> Option<World> {
    let mut found = None;
    for world in server.get_all_worlds() {
        if world.get_chunk(at.0 >> 4, at.2 >> 4).is_some() {
            if found.is_some() {
                live.ambiguous += 1;
                return None;
            }
            found = Some(world);
        }
    }
    found
}

/// Start watching cells for a change the world itself is about to make.
fn watch_world(live: &mut Live, kind: WatchKind, world: &World, cells: Vec<(P, u16)>, wait: u64) {
    if cells.is_empty() {
        return;
    }
    if live.watches.len() >= MAX_WATCHES {
        live.dropped += 1;
        return;
    }
    live.watches.push(Watch { kind, user: String::new(), dim: world.get_dimension(), born: live.tick, due: live.tick + wait, time: now(), cells, nbt: Vec::new() });
}

// ---------------------------------------------------------------------------------------------
// Containers
// ---------------------------------------------------------------------------------------------

fn container_of(world: &World, at: P) -> Option<ContainerBlockEntity> {
    use BlockEntityType as B;
    Some(match world.get_block_entity(pos(at))? {
        B::ChestBlockEntity(b) => b.get_container(),
        B::TrappedChestBlockEntity(b) => b.get_container(),
        B::BarrelBlockEntity(b) => b.get_container(),
        B::ShulkerBoxBlockEntity(b) => b.get_container(),
        B::HopperBlockEntity(b) => b.get_container(),
        B::DispenserBlockEntity(b) => b.get_container(),
        B::DropperBlockEntity(b) => b.get_container(),
        B::FurnaceBlockEntity(b) => b.get_container(),
        B::BlastingFurnaceBlockEntity(b) => b.get_container(),
        B::SmokerBlockEntity(b) => b.get_container(),
        B::BrewingStandBlockEntity(b) => b.get_container(),
        _ => return None,
    })
}

/// The blocks of the container at `at`: both halves of a double chest, lowest position first.
fn container_parts(world: &World, at: P) -> Option<Vec<P>> {
    container_of(world, at)?;
    let mut parts = vec![at];
    let state = state_string(world.get_block_state_id(pos(at)));
    if state.contains("type=left") || state.contains("type=right") {
        let facing = state.split(',').find(|s| s.contains("facing=")).unwrap_or("").trim_start_matches(|c| c != 'f').to_owned();
        for side in [(1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1)] {
            let n = add(at, side);
            let other = state_string(world.get_block_state_id(pos(n)));
            if block_name(&other) == block_name(&state) && other.contains(&facing) && (other.contains("type=left") || other.contains("type=right")) && other != state && container_of(world, n).is_some() {
                parts.push(n);
                break;
            }
        }
    }
    parts.sort_unstable();
    Some(parts)
}

/// Item name -> how many, over all parts of a container. `None` when it is gone.
fn contents(world: &World, parts: &[P]) -> Option<HashMap<String, u32>> {
    let mut found = HashMap::new();
    for at in parts {
        let container = container_of(world, *at)?;
        for slot in 0..container.get_size().min(128) {
            if let Some(stack) = container.get_stack(slot) {
                let count = u32::from(stack.get_count());
                if let Some(text) = item_text(&stack).filter(|_| count > 0) {
                    *found.entry(text).or_insert(0) += count;
                }
            }
        }
    }
    Some(found)
}

/// The record text of a stack: its name and, when it has any, its data (see `items`).
/// Two stacks with the same text are the same item.
fn item_text(stack: &ItemStack) -> Option<String> {
    let name = stack.get_registry_key();
    let name = name.strip_prefix("minecraft:").unwrap_or(&name);
    if name.is_empty() || name == "air" {
        return None;
    }
    let parts: Vec<(u16, Vec<u8>)> = stack.get_components().into_iter().map(|c| (c.component as u16, c.value)).collect();
    Some(items::encode(name, &parts, MAX_ITEM))
}

/// The component with that number, for numbers this server's API has.
fn component(id: u16) -> Option<DataComponent> {
    // Numbers come from our own log; the list only ever grows at its end
    (id <= DataComponent::CushionColor as u16).then(|| unsafe { DataComponent::_lift(id as u8) })
}

/// A stack as a container record describes it. `None` when this server has no such item.
fn make_stack(item: &items::Item, count: u8) -> Option<ItemStack> {
    let stack = ItemStack::new(&format!("minecraft:{}", item.name), count);
    if stack.get_registry_key().trim_start_matches("minecraft:") != item.name || stack.get_count() == 0 {
        return None;
    }
    for (id, value) in &item.parts {
        if let Some(kind) = component(*id) {
            stack.set_component(kind, value);
        }
    }
    Some(stack)
}

/// Put items into (`count > 0`) or take them out of (`count < 0`) the container at `at`.
fn change_container(world: &World, at: P, text: &str, count: i64) -> Stored {
    let Some(parts) = container_parts(world, at) else { return Stored::NoContainer };
    let containers: Vec<ContainerBlockEntity> = parts.iter().filter_map(|p| container_of(world, *p)).collect();
    let item = items::decode(text);
    // An item whose data was too long to keep is found by its name alone
    let is_it = |stack: &ItemStack| if item.lossy { item_text(stack).is_some_and(|t| items::name(&t) == item.name) } else { item_text(stack).as_deref() == Some(text) };
    let mut left = count.unsigned_abs().min(100_000) as u32;
    if count < 0 {
        for container in &containers {
            for slot in 0..container.get_size().min(128) {
                if left == 0 {
                    return Stored::Done;
                }
                let Some(stack) = container.get_stack(slot).filter(|s| is_it(s)) else { continue };
                let have = u32::from(stack.get_count());
                if have <= left {
                    container.set_stack(slot, None);
                    left -= have;
                } else {
                    stack.set_count((have - left) as u8);
                    container.set_stack(slot, Some(stack));
                    left = 0;
                }
            }
        }
        // What is no longer there cannot be taken out; the record is done all the same
        return Stored::Done;
    }
    let Some(sample) = make_stack(&item, 1) else { return Stored::NoRoom };
    let max = u32::from(sample.get_max_count().max(1));
    // First see that it all fits: half a return is worse than none
    let mut room = 0u32;
    let mut slots = Vec::new();
    for (i, container) in containers.iter().enumerate() {
        for slot in 0..container.get_size().min(128) {
            match container.get_stack(slot) {
                None => {
                    room += max;
                    slots.push((i, slot, 0));
                }
                Some(stack) if is_it(&stack) && u32::from(stack.get_count()) < max => {
                    let have = u32::from(stack.get_count());
                    room += max - have;
                    // Stacks that are already started fill up first
                    slots.insert(0, (i, slot, have));
                }
                Some(_) => {}
            }
        }
    }
    if room < left {
        return Stored::NoRoom;
    }
    for (i, slot, have) in slots {
        if left == 0 {
            break;
        }
        let put = left.min(max - have);
        let Some(stack) = make_stack(&item, (have + put) as u8) else { return Stored::NoRoom };
        containers[i].set_stack(slot, Some(stack));
        left -= put;
    }
    Stored::Done
}

/// The player closed (or left) the container: log what is different from when it was opened.
fn container_end(live: &mut Live, p: &Player, k: &str) {
    let Some(open) = live.containers.remove(k) else { return };
    if !open.opened || open.born.elapsed() > Duration::from_secs(1800) {
        return;
    }
    let world = p.get_world();
    if world.get_dimension() != open.dim {
        return;
    }
    let Some(after) = contents(&world, &open.parts) else { return };
    let user = p.get_name();
    let mut names: Vec<&String> = open.before.keys().chain(after.keys()).collect();
    names.sort_unstable();
    names.dedup();
    for name in names.into_iter().take(64) {
        let (was, is) = (open.before.get(name).copied().unwrap_or(0), after.get(name).copied().unwrap_or(0));
        if was == is {
            continue;
        }
        live.texts.push(TextRec {
            time: now(),
            action: if is > was { Action::ContainerAdd } else { Action::ContainerRemove },
            user: user.clone(),
            world: world_key(&open.dim),
            x: open.at.0,
            y: open.at.1,
            z: open.at.2,
            amount: is.abs_diff(was),
            text: name.clone(),
        });
    }
}

// ---------------------------------------------------------------------------------------------
// Event handlers
// ---------------------------------------------------------------------------------------------

type Data<E> = <E as FromIntoEvent>::Data;

fn text_rec(p: &Player, action: Action, amount: u32, text: &str) -> TextRec {
    let at = feet(p);
    TextRec { time: now(), action, user: p.get_name(), world: world_key(&p.get_world().get_dimension()), x: at.0, y: at.1, z: at.2, amount, text: clean(text, MAX_TEXT) }
}

/// An inspector click: ask the tick loop for the history of the block. The same click can
/// arrive twice (both hands, or interact then break): answer once.
fn inspect_click(live: &mut Live, p: &Player, k: &str, at: P, right: bool) {
    if live.clicks.get(k).is_some_and(|(a, r, t)| *a == at && *r == right && t.elapsed().as_millis() < 300) {
        return;
    }
    live.clicks.insert(k.to_owned(), (at, right, Instant::now()));
    let world = p.get_world();
    let mut req = InspectReq { player: k.to_owned(), world: world_key(&world.get_dimension()), pos: at, container: false };
    if right {
        if let Some(parts) = container_parts(&world, at) {
            // A container: who put in and took out
            req.pos = parts[0];
            req.container = true;
        } else if let Some(hit) = p.get_target_block_exact(6.0, false) {
            // Anything else: the spot a block would be placed in, when the server sees the same click
            if (hit.pos.x, hit.pos.y, hit.pos.z) == at {
                req.pos = add(at, match hit.face {
                    BlockDirection::Down => (0, -1, 0),
                    BlockDirection::Up => (0, 1, 0),
                    BlockDirection::North => (0, 0, -1),
                    BlockDirection::South => (0, 0, 1),
                    BlockDirection::West => (-1, 0, 0),
                    BlockDirection::East => (1, 0, 0),
                });
            }
        }
    }
    live.inspects.retain(|r| r.player != k);
    live.inspects.push(req);
}

struct OnBreak;
impl EventHandler<BlockBreakEvent> for OnBreak {
    fn handle(&self, server: Server, mut e: Data<BlockBreakEvent>) -> Data<BlockBreakEvent> {
        if e.cancelled {
            return e;
        }
        let at = (e.block_pos.x, e.block_pos.y, e.block_pos.z);
        let Some(p) = &e.player else {
            // The world broke it (water washing a torch away, a plant losing its support). The
            // event names no world: see `only_world`.
            if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
                // Part of a break or a piston push that is being watched: reported from there
                let watched = live.watches.iter().any(|w| matches!(w.kind, WatchKind::Break | WatchKind::Piston) && w.cells.iter().any(|c| c.0 == at));
                // A fire that goes out is not logged: rolling an area back must not light it again
                let block = e.block.trim_start_matches("minecraft:");
                let wanted = live.log.natural_break && !watched && !matches!(block, "fire" | "soul_fire") && !APPLYING.load(Ordering::Relaxed);
                if wanted && live.natural < MAX_NATURAL {
                    if let Some(world) = only_world(live, &server, at) {
                        let old = world.get_block_state_id(pos(at));
                        if old != AIR && block_name(&state_string(old)) == block {
                            live.natural += 1;
                            watch_world(live, WatchKind::Natural, &world, vec![(at, old)], SETTLE_TICKS);
                        }
                    }
                }
            }
            return e;
        };
        if c::awaiting_login(p) {
            return e;
        }
        let Some(mut guard) = try_lock(&LIVE) else { return e };
        let Some(live) = guard.as_mut() else { return e };
        let k = key(p);
        if live.inspectors.contains(&k) {
            e.cancelled = true;
            // The click was answered when the digging began (the interact event); the break that
            // ends the digging is the same click
            if !live.clicks.get(&k).is_some_and(|(a, right, t)| *a == at && !*right && t.elapsed() < Duration::from_secs(20)) {
                inspect_click(live, p, &k, at, false);
            }
        } else if live.log.block_break {
            watch(live, WatchKind::Break, p, at);
        }
        e
    }
}

struct OnPlace;
impl EventHandler<BlockPlaceEvent> for OnPlace {
    fn handle(&self, _server: Server, e: Data<BlockPlaceEvent>) -> Data<BlockPlaceEvent> {
        if e.cancelled || c::awaiting_login(&e.player) {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            if live.log.block_place {
                watch(live, WatchKind::Place, &e.player, (e.block_pos.x, e.block_pos.y, e.block_pos.z));
            }
        }
        e
    }
}

struct OnInteract;
impl EventHandler<PlayerInteractEvent> for OnInteract {
    fn handle(&self, _server: Server, mut e: Data<PlayerInteractEvent>) -> Data<PlayerInteractEvent> {
        let right = match e.action {
            InteractAction::RightClickBlock => true,
            InteractAction::LeftClickBlock => false,
            _ => return e,
        };
        let Some(at) = e.clicked_pos.as_ref().map(|b| (b.x, b.y, b.z)) else { return e };
        if e.cancelled || c::awaiting_login(&e.player) {
            return e;
        }
        let Some(mut guard) = try_lock(&LIVE) else { return e };
        let Some(live) = guard.as_mut() else { return e };
        let k = key(&e.player);
        if live.inspectors.contains(&k) {
            e.cancelled = true;
            inspect_click(live, &e.player, &k, at, right);
        } else if right {
            // The events for fire and for an emptied bucket name no player or come too late (the
            // bucket's fires after the water is placed): the click with the item in hand is the start
            let held = e.player.get_inventory_item(e.player.get_selected_slot()).map(|s| s.get_registry_key()).unwrap_or_default();
            let held = held.trim_start_matches("minecraft:");
            if live.log.ignite && matches!(held, "flint_and_steel" | "fire_charge") {
                watch(live, WatchKind::Ignite, &e.player, at);
            } else if live.log.bucket && held.ends_with("_bucket") && held != "milk_bucket" {
                watch(live, WatchKind::BucketEmpty, &e.player, at);
            }
            if live.log.container {
                // Remember what is in the container; the close event tells what the player changed
                container_end(live, &e.player, &k);
                let world = e.player.get_world();
                if let Some(parts) = container_parts(&world, at) {
                    if let Some(before) = contents(&world, &parts) {
                        live.containers.insert(k, Opened { dim: world.get_dimension(), at: parts[0], parts, before, opened: false, born: Instant::now() });
                    }
                }
            }
        }
        e
    }
}

/// Whether a right click on that block does something worth a line in the log.
fn clickable(block: &str) -> bool {
    const ENDS: [&str; 6] = ["_door", "_trapdoor", "_fence_gate", "_button", "shulker_box", "chest"];
    const IS: [&str; 15] =
        ["lever", "repeater", "comparator", "note_block", "daylight_detector", "barrel", "hopper", "dispenser", "dropper", "furnace", "blast_furnace", "smoker", "brewing_stand", "lectern", "jukebox"];
    ENDS.iter().any(|end| block.ends_with(end)) || IS.contains(&block)
}

/// The same event again, after every plugin that may refuse it: a click that went through.
struct OnClick;
impl EventHandler<PlayerInteractEvent> for OnClick {
    fn handle(&self, _server: Server, e: Data<PlayerInteractEvent>) -> Data<PlayerInteractEvent> {
        if e.cancelled || !matches!(e.action, InteractAction::RightClickBlock) {
            return e;
        }
        let block = e.block.trim_start_matches("minecraft:");
        let Some(at) = e.clicked_pos.as_ref().map(|b| (b.x, b.y, b.z)).filter(|_| clickable(block)) else { return e };
        if c::awaiting_login(&e.player) {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            let k = key(&e.player);
            // A client sends two packets for one right click, and each fires this event
            let again = live.clicked.get(&k).is_some_and(|(a, t)| *a == at && t.elapsed().as_millis() < 300);
            if live.log.click && !again {
                live.clicked.insert(k, (at, Instant::now()));
                let mut rec = text_rec(&e.player, Action::Click, 0, block);
                (rec.x, rec.y, rec.z) = at;
                live.texts.push(rec);
            }
        }
        e
    }
}

struct OnHopper;
impl EventHandler<InventoryMoveItemEvent> for OnHopper {
    fn handle(&self, server: Server, e: Data<InventoryMoveItemEvent>) -> Data<InventoryMoveItemEvent> {
        if e.cancelled {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            let at = (e.target_pos.x, e.target_pos.y, e.target_pos.z);
            if live.natural >= MAX_NATURAL {
                live.dropped += 1;
            } else if let Some(world) = only_world(live, &server, at) {
                // The event comes when a hopper is about to push an item out; it names no world
                live.natural += 1;
                live.texts.push(TextRec {
                    time: now(),
                    action: Action::ContainerAdd,
                    user: "#hopper".into(),
                    world: world_key(&world.get_dimension()),
                    x: at.0,
                    y: at.1,
                    z: at.2,
                    amount: e.item_amount,
                    text: e.item_id.trim_start_matches("minecraft:").to_owned(),
                });
            }
        }
        e
    }
}

struct OnInvOpen;
impl EventHandler<InventoryOpenEvent> for OnInvOpen {
    fn handle(&self, _server: Server, e: Data<InventoryOpenEvent>) -> Data<InventoryOpenEvent> {
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            let k = key(&e.player);
            match live.containers.get_mut(&k) {
                // The window that opens right after the click on the container is the container
                Some(open) if !e.cancelled && open.born.elapsed() < Duration::from_secs(1) => open.opened = true,
                Some(_) => {
                    live.containers.remove(&k);
                }
                None => {}
            }
        }
        e
    }
}

struct OnInvClose;
impl EventHandler<InventoryCloseEvent> for OnInvClose {
    fn handle(&self, _server: Server, e: Data<InventoryCloseEvent>) -> Data<InventoryCloseEvent> {
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            let k = key(&e.player);
            // A container clicked a moment ago whose window is not open yet: this close is of the
            // window before it
            if live.containers.get(&k).is_none_or(|open| open.opened || open.born.elapsed() > Duration::from_secs(1)) {
                container_end(live, &e.player, &k);
            }
        }
        e
    }
}

struct OnBucketFill;
impl EventHandler<PlayerBucketFillEvent> for OnBucketFill {
    fn handle(&self, _server: Server, e: Data<PlayerBucketFillEvent>) -> Data<PlayerBucketFillEvent> {
        if e.cancelled || c::awaiting_login(&e.player) {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            if live.log.bucket {
                watch(live, WatchKind::BucketFill, &e.player, (e.block_pos.x, e.block_pos.y, e.block_pos.z));
            }
        }
        e
    }
}

struct OnBurn;
impl EventHandler<BlockBurnEvent> for OnBurn {
    fn handle(&self, _server: Server, e: Data<BlockBurnEvent>) -> Data<BlockBurnEvent> {
        if e.cancelled {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            if live.log.burn && live.blocks.len() < MAX_WATCHES {
                let at = (e.block_pos.x, e.block_pos.y, e.block_pos.z);
                let old = e.target_world.get_block_state_id(pos(at));
                if old != AIR {
                    live.blocks.push(OutBlock { world: world_key(&e.target_world.get_dimension()), time: now(), user: "#fire".into(), pos: at, action: Action::Burn, old, new: AIR, nbt: None });
                }
            }
        }
        e
    }
}

struct OnExplode;
impl EventHandler<BlockExplodeEvent> for OnExplode {
    fn handle(&self, server: Server, e: Data<BlockExplodeEvent>) -> Data<BlockExplodeEvent> {
        if e.cancelled {
            return e;
        }
        let Some(mut guard) = try_lock(&LIVE) else { return e };
        let Some(live) = guard.as_mut() else { return e };
        if !live.log.explosion {
            return e;
        }
        if live.explosions >= MAX_EXPLOSIONS || live.watches.len() >= MAX_WATCHES {
            live.dropped += 1;
            return e;
        }
        live.explosions += 1;
        // The event carries 1/power and the centre, not the blocks and not the world: remember
        // every block the blast can reach, in each world that has this place loaded
        let power = if e.yield_rate > 0.0 { 1.0 / e.yield_rate } else { 4.0 };
        let r = ((power * 1.75).ceil() as i32 + 1).clamp(2, 10);
        let centre = (e.block_pos.x, e.block_pos.y, e.block_pos.z);
        for world in server.get_all_worlds() {
            if world.get_chunk(centre.0 >> 4, centre.2 >> 4).is_none() {
                continue;
            }
            let dim = world.get_dimension();
            // An earlier blast of this tick has done its damage by now: settle it before its
            // blocks are counted for this one too
            settle_where(live, &world, &dim, false, |w| w.kind == WatchKind::Explosion);
            let lo = world.get_min_y();
            let hi = if lo < 0 { 319 } else { 255 };
            let mut cells = Vec::new();
            for dy in -r..=r {
                let y = centre.1 + dy;
                if y < lo || y > hi {
                    continue;
                }
                for dx in -r..=r {
                    for dz in -r..=r {
                        if dx * dx + dy * dy + dz * dz > r * r + r {
                            continue;
                        }
                        let at = (centre.0 + dx, y, centre.2 + dz);
                        let state = world.get_block_state_id(pos(at));
                        if state != AIR {
                            cells.push((at, state));
                        }
                    }
                }
            }
            if !cells.is_empty() {
                // Chests and signs in the blast: what is inside them, up to a megabyte in all
                let mut nbt = Vec::new();
                let mut kept = 0;
                for (at, _) in &cells {
                    if let Some(data) = inside(&world, *at).filter(|d| kept + d.len() <= MAX_NBT) {
                        kept += data.len();
                        nbt.push((*at, data));
                    }
                }
                live.watches.push(Watch { kind: WatchKind::Explosion, user: "#explosion".into(), dim, born: live.tick, due: live.tick + SETTLE_TICKS, time: now(), cells, nbt });
            }
        }
        e
    }
}

struct OnSign;
impl EventHandler<SignChangeEvent> for OnSign {
    fn handle(&self, _server: Server, e: Data<SignChangeEvent>) -> Data<SignChangeEvent> {
        if e.cancelled || c::awaiting_login(&e.player) {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            if live.log.sign {
                let text = e.lines.iter().take(8).map(|l| clean(l, 96)).filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join(" | ");
                let mut rec = text_rec(&e.player, Action::Sign, 0, &text);
                (rec.x, rec.y, rec.z) = (e.block_pos.x, e.block_pos.y, e.block_pos.z);
                live.texts.push(rec);
            }
        }
        e
    }
}

struct OnChat;
impl EventHandler<PlayerChatEvent> for OnChat {
    fn handle(&self, _server: Server, e: Data<PlayerChatEvent>) -> Data<PlayerChatEvent> {
        // A cancelled line was not said. A chat plugin that cancels every line to send it in its own
        // format (Basics) tells BlockLog what it sent with a notice (see `ipc`).
        if e.cancelled || c::awaiting_login(&e.player) {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            if live.log.chat {
                let rec = text_rec(&e.player, Action::Chat, 0, &e.message);
                live.said.push((rec.user.clone(), rec.text.clone(), Instant::now()));
                if live.said.len() > 32 {
                    live.said.remove(0);
                }
                live.texts.push(rec);
            }
        }
        e
    }
}

struct OnCommand;
impl EventHandler<PlayerCommandSendEvent> for OnCommand {
    fn handle(&self, _server: Server, e: Data<PlayerCommandSendEvent>) -> Data<PlayerCommandSendEvent> {
        if e.cancelled || c::awaiting_login(&e.player) {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            if live.log.command {
                // Passwords never reach the log
                let line = params::mask_command(&e.command);
                if !line.is_empty() {
                    live.texts.push(text_rec(&e.player, Action::Command, 0, &format!("/{line}")));
                }
            }
        }
        e
    }
}

struct OnJoin;
impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _server: Server, e: Data<PlayerJoinEvent>) -> Data<PlayerJoinEvent> {
        if e.cancelled {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            live.ids.insert(e.player.as_entity().get_id() as i32, e.player.get_name());
            if live.log.session {
                live.texts.push(text_rec(&e.player, Action::Join, 0, ""));
            }
        }
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, e: Data<PlayerLeaveEvent>) -> Data<PlayerLeaveEvent> {
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            let k = key(&e.player);
            container_end(live, &e.player, &k);
            live.inspectors.remove(&k);
            live.clicks.remove(&k);
            live.inspects.retain(|r| r.player != k);
            live.ids.remove(&(e.player.as_entity().get_id() as i32));
            if live.log.session {
                live.texts.push(text_rec(&e.player, Action::Leave, 0, ""));
            }
        }
        e
    }
}

struct OnDrop;
impl EventHandler<PlayerDropItemEvent> for OnDrop {
    fn handle(&self, _server: Server, e: Data<PlayerDropItemEvent>) -> Data<PlayerDropItemEvent> {
        if e.cancelled || c::awaiting_login(&e.player) {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            if live.log.item {
                let name = e.item_name.strip_prefix("minecraft:").unwrap_or(&e.item_name);
                live.texts.push(text_rec(&e.player, Action::Drop, u32::from(e.count), name));
            }
        }
        e
    }
}

struct OnHit;
impl EventHandler<EntityDamageByEntityEvent> for OnHit {
    fn handle(&self, _server: Server, e: Data<EntityDamageByEntityEvent>) -> Data<EntityDamageByEntityEvent> {
        if e.cancelled {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            if live.log.kill {
                // The player's own hit, or the hit of an arrow a player shot
                if let Some(name) = live.ids.get(&e.damager_id).or_else(|| live.shots.get(&e.damager_id).map(|(name, _)| name)).cloned() {
                    if live.hits.len() < 4096 {
                        live.hits.insert(e.entity_id, (name, live.tick));
                    }
                }
            }
        }
        e
    }
}

struct OnLaunch;
impl EventHandler<ProjectileLaunchEvent> for OnLaunch {
    fn handle(&self, _server: Server, e: Data<ProjectileLaunchEvent>) -> Data<ProjectileLaunchEvent> {
        if e.cancelled {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            // Pumpkin 0.2.2 fires this for arrows only, and names the shooter
            if let Some(name) = e.shooter_id.and_then(|id| live.ids.get(&id)).cloned() {
                if live.log.kill && live.shots.len() < 4096 {
                    live.shots.insert(e.entity_id, (name, live.tick));
                }
            }
        }
        e
    }
}

struct OnSpread;
impl EventHandler<BlockSpreadEvent> for OnSpread {
    fn handle(&self, _server: Server, e: Data<BlockSpreadEvent>) -> Data<BlockSpreadEvent> {
        if e.cancelled {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            if live.log.fire_spread && live.blocks.len() < MAX_WATCHES {
                let at = (e.target_pos.x, e.target_pos.y, e.target_pos.z);
                let old = e.target_world.get_block_state_id(pos(at));
                if old != e.new_state_id {
                    live.blocks.push(OutBlock { world: world_key(&e.target_world.get_dimension()), time: now(), user: "#fire".into(), pos: at, action: Action::Place, old, new: e.new_state_id, nbt: None });
                }
            }
        }
        e
    }
}

struct OnDecay;
impl EventHandler<LeavesDecayEvent> for OnDecay {
    fn handle(&self, _server: Server, e: Data<LeavesDecayEvent>) -> Data<LeavesDecayEvent> {
        if e.cancelled {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            if live.blocks.len() < MAX_WATCHES {
                let at = (e.block_pos.x, e.block_pos.y, e.block_pos.z);
                let old = e.target_world.get_block_state_id(pos(at));
                if old != AIR {
                    live.blocks.push(OutBlock { world: world_key(&e.target_world.get_dimension()), time: now(), user: "#decay".into(), pos: at, action: Action::Break, old, new: AIR, nbt: None });
                }
            }
        }
        e
    }
}

struct OnGrow;
impl EventHandler<BlockGrowEvent> for OnGrow {
    fn handle(&self, _server: Server, e: Data<BlockGrowEvent>) -> Data<BlockGrowEvent> {
        if e.cancelled || e.old_state_id == e.new_state_id {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            if live.blocks.len() < MAX_WATCHES {
                let at = (e.block_pos.x, e.block_pos.y, e.block_pos.z);
                live.blocks.push(OutBlock { world: world_key(&e.target_world.get_dimension()), time: now(), user: "#grow".into(), pos: at, action: Action::Place, old: e.old_state_id, new: e.new_state_id, nbt: None });
            }
        }
        e
    }
}

struct OnEnderman;
impl EventHandler<EntityChangeBlockEvent> for OnEnderman {
    fn handle(&self, server: Server, e: Data<EntityChangeBlockEvent>) -> Data<EntityChangeBlockEvent> {
        if e.cancelled {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            let at = (e.block_pos.x, e.block_pos.y, e.block_pos.z);
            if live.log.entity_change && live.natural < MAX_NATURAL {
                // The event names the entity by number only and no world
                if let Some(world) = only_world(live, &server, at) {
                    let old = world.get_block_state_id(pos(at));
                    if old != AIR {
                        live.natural += 1;
                        live.blocks.push(OutBlock { world: world_key(&world.get_dimension()), time: now(), user: "#enderman".into(), pos: at, action: Action::Break, old, new: AIR, nbt: None });
                    }
                }
            }
        }
        e
    }
}

struct OnFlow;
impl EventHandler<BlockFromToEvent> for OnFlow {
    fn handle(&self, server: Server, e: Data<BlockFromToEvent>) -> Data<BlockFromToEvent> {
        if e.cancelled {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            let at = (e.to_pos.x, e.to_pos.y, e.to_pos.z);
            if live.natural < MAX_NATURAL {
                if let Some(world) = only_world(live, &server, at) {
                    live.natural += 1;
                    let old = world.get_block_state_id(pos(at));
                    watch_world(live, WatchKind::Flow, &world, vec![(at, old)], SETTLE_TICKS);
                }
            } else {
                live.dropped += 1;
            }
        }
        e
    }
}

/// A piston is about to move: remember the blocks in its way. Twelve can be pushed, one more is
/// where the last one lands, and the head takes the first place.
fn piston(server: &Server, at: P, direction: &str, reach: i32) {
    let Some(step) = ipc::direction(direction) else { return };
    let Some(mut guard) = try_lock(&LIVE) else { return };
    let Some(live) = guard.as_mut() else { return };
    if live.natural >= MAX_NATURAL {
        live.dropped += 1;
        return;
    }
    let Some(world) = only_world(live, server, at) else { return };
    live.natural += 1;
    let cells = (1..=reach).map(|i| (at.0 + step.0 * i, at.1 + step.1 * i, at.2 + step.2 * i)).map(|p| (p, world.get_block_state_id(pos(p)))).collect();
    watch_world(live, WatchKind::Piston, &world, cells, PISTON_TICKS);
}

struct OnPistonOut;
impl EventHandler<BlockPistonExtendEvent> for OnPistonOut {
    fn handle(&self, server: Server, e: Data<BlockPistonExtendEvent>) -> Data<BlockPistonExtendEvent> {
        if !e.cancelled {
            piston(&server, (e.block_pos.x, e.block_pos.y, e.block_pos.z), &e.direction, 14);
        }
        e
    }
}

struct OnPistonIn;
impl EventHandler<BlockPistonRetractEvent> for OnPistonIn {
    fn handle(&self, server: Server, e: Data<BlockPistonRetractEvent>) -> Data<BlockPistonRetractEvent> {
        if !e.cancelled {
            // A sticky piston pulls the block behind its head back with it
            piston(&server, (e.block_pos.x, e.block_pos.y, e.block_pos.z), &e.direction, 3);
        }
        e
    }
}

struct OnTree;
impl EventHandler<StructureGrowEvent> for OnTree {
    fn handle(&self, server: Server, e: Data<StructureGrowEvent>) -> Data<StructureGrowEvent> {
        if e.cancelled {
            return e;
        }
        let Some(mut guard) = try_lock(&LIVE) else { return e };
        let Some(live) = guard.as_mut() else { return e };
        if live.trees >= MAX_TREES {
            live.dropped += 1;
            return e;
        }
        let at = (e.pos.x, e.pos.y, e.pos.z);
        // The event has the sapling's place only: remember the space a tree can take
        let Some(world) = only_world(live, &server, at) else { return e };
        live.trees += 1;
        let lo = world.get_min_y();
        let hi = if lo < 0 { 319 } else { 255 };
        let mut cells = Vec::with_capacity(13 * 13 * 35);
        for y in (at.1 - 1).max(lo)..=(at.1 + 33).min(hi) {
            for x in at.0 - 6..=at.0 + 6 {
                for z in at.2 - 6..=at.2 + 6 {
                    cells.push(((x, y, z), world.get_block_state_id(pos((x, y, z)))));
                }
            }
        }
        watch_world(live, WatchKind::Tree, &world, cells, SETTLE_TICKS);
        e
    }
}

struct OnEntityDeath;
impl EventHandler<EntityDeathEvent> for OnEntityDeath {
    fn handle(&self, server: Server, e: Data<EntityDeathEvent>) -> Data<EntityDeathEvent> {
        let Some(mut guard) = try_lock(&LIVE) else { return e };
        let Some(live) = guard.as_mut() else { return e };
        // A player's death is logged from its own event, which follows this one
        if live.ids.contains_key(&e.entity_id) {
            return e;
        }
        let Some((killer, tick)) = live.hits.remove(&e.entity_id) else { return e };
        if live.tick.saturating_sub(tick) > 200 {
            return e;
        }
        let Some(p) = server.get_player_by_name(&killer) else { return e };
        // The event has only the entity's number: find the entity near its killer
        let victim = p.as_entity().get_nearby_entities(24.0, 24.0, 24.0).into_iter().find(|v| v.get_id() as i32 == e.entity_id);
        let mut rec = text_rec(&p, Action::Kill, 0, "entity");
        if let Some(v) = victim {
            // `EntityType::ZombieVillager` -> `zombievillager`
            let kind = format!("{:?}", v.get_type()).to_lowercase();
            rec.text = kind.rsplit(':').next().unwrap_or(&kind).to_owned();
            let (x, y, z) = v.get_position();
            (rec.x, rec.y, rec.z) = (x.floor() as i32, y.floor() as i32, z.floor() as i32);
        }
        live.texts.push(rec);
        e
    }
}

struct OnPlayerDeath;
impl EventHandler<PlayerDeathEvent> for OnPlayerDeath {
    fn handle(&self, _server: Server, e: Data<PlayerDeathEvent>) -> Data<PlayerDeathEvent> {
        if e.cancelled {
            return e;
        }
        if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
            if live.log.kill {
                let id = e.player.as_entity().get_id() as i32;
                let killer = live.hits.remove(&id).filter(|(_, tick)| live.tick.saturating_sub(*tick) <= 200).map_or_else(|| "#environment".to_owned(), |(name, _)| name);
                let mut rec = text_rec(&e.player, Action::Kill, 0, &format!("player:{}", e.player.get_name()));
                rec.user = killer;
                live.texts.push(rec);
            }
        }
        e
    }
}

// ---------------------------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------------------------

fn row_line(th: bool, now: u64, r: &Row) -> String {
    use Action::*;
    let t = |thai: &'static str, english: &'static str| if th { thai } else { english };
    // An item with data (enchanted, renamed) shows its name and a mark
    let text = if r.action.is_container() { format!("{}{}", safe(items::name(&r.text)), if items::has_data(&r.text) { "*" } else { "" }) } else { safe(&r.text) };
    let what = match r.action {
        Break => format!("{} &3{text}", t("ทุบ", "broke")),
        Place => format!("{} &3{text}", t("วาง", "placed")),
        BucketEmpty => format!("{} &3{text}", t("เทถัง", "emptied a bucket:")),
        BucketFill => format!("{} &3{text}", t("ตักถัง", "filled a bucket:")),
        Burn => format!("{} &3{text}", t("เผา", "burned")),
        Explode => format!("{} &3{text}", t("ระเบิด", "blew up")),
        Chat => format!("{} &7{text}", t("พูด:", "said:")),
        Command => format!("{} &7{text}", t("ใช้คำสั่ง", "ran")),
        Join => t("เข้าเกม", "logged in").to_owned(),
        Leave => t("ออกเกม", "logged out").to_owned(),
        Sign => format!("{} &7{text}", t("เขียนป้าย:", "wrote on a sign:")),
        ContainerAdd => format!("{} &3x{} {text}", t("ใส่ของ", "added"), r.amount),
        ContainerRemove => format!("{} &3x{} {text}", t("หยิบของ", "removed"), r.amount),
        Kill => format!("{} &3{text}", t("ฆ่า", "killed")),
        Drop => format!("{} &3x{} {text}", t("ทิ้ง", "dropped"), r.amount),
        Click => format!("{} &3{text}", t("คลิก", "clicked")),
    };
    let place = if r.world.is_empty() { String::new() } else { format!(" &8(x{}/y{}/z{}/{})", r.x, r.y, r.z, r.world) };
    let rolled = if r.rolled { t(" &c(ย้อนแล้ว)", " &c(rolled back)") } else { "" };
    format!("&7{} {} &8- &3{} &f{what}{place}{rolled}", timefmt::ago(now, u64::from(r.time)), t("ก่อน", "ago"), safe(&r.user))
}

fn page_lines(th: bool, per_page: usize, res: &Results, page: usize) -> Vec<String> {
    if res.rows.is_empty() {
        return vec![msg(th, &format!("&eไม่พบรายการ &7({})", res.title), &format!("&eNo results &7({})", res.title))];
    }
    let pages = res.rows.len().div_ceil(per_page);
    let page = page.clamp(1, pages);
    let now = c::now_secs();
    let more = if res.truncated { "+" } else { "" };
    let mut lines = vec![if th {
        format!("&8----- &bBlockLog &8----- &7{} &8({}{more} รายการ)", res.title, res.rows.len())
    } else {
        format!("&8----- &bBlockLog &8----- &7{} &8({}{more} rows)", res.title, res.rows.len())
    }];
    lines.extend(res.rows.iter().skip((page - 1) * per_page).take(per_page).map(|r| row_line(th, now, r)));
    if pages > 1 {
        lines.push(if th { format!("&7หน้า {page}/{pages} &8· &7/co l <หน้า>") } else { format!("&7Page {page}/{pages} &8· &7/co l <page>") });
    }
    lines
}

fn emit(server: &Server, owner: &Owner, lines: &[String]) {
    match owner {
        Owner::Console => lines.iter().for_each(|l| c::log_info(&c::plain(l))),
        Owner::Player(k) => {
            if let Some(p) = player_of(server, k) {
                lines.iter().for_each(|l| c::tell(&p, l));
            }
        }
    }
}

fn skipped_note(th: bool, out: &Outcome) -> String {
    let mut note = String::new();
    if out.skipped > 0 {
        note.push_str(&if th {
            format!(" &e(ข้าม {} บล็อกใน {} chunk ที่ยังไม่โหลด เข้าไปใกล้แล้วสั่งซ้ำ)", out.skipped, out.skipped_chunks)
        } else {
            format!(" &e({} blocks in {} chunks that are not loaded were skipped; go closer and repeat)", out.skipped, out.skipped_chunks)
        });
    }
    if out.failed > 0 {
        note.push_str(&if th {
            format!(" &c({} รายการทำไม่ได้: บล็อกที่เซิร์ฟรุ่นนี้ไม่รู้จัก หรือกล่องหายไป/เต็ม)", out.failed)
        } else {
            format!(" &c({} could not be done: a block this server version does not know, or a container that is gone or full)", out.failed)
        });
    }
    note
}

/// A job ended: remember what must be remembered and say what happened.
fn finish(st: &mut State, run: Running) -> Vec<String> {
    let th = st.cfg.language != "en";
    let Running { owner, kind, mut job, title, started, busy } = run;
    let out = std::mem::take(&mut job.out);
    let secs = started.elapsed().as_secs_f32();
    match kind {
        Kind::Lookup | Kind::Inspect => {
            let res = Results { rows: out.rows, truncated: out.truncated, title, at: Instant::now() };
            let lines = page_lines(th, st.cfg.page_size, &res, 1);
            st.results.insert(owner.key().to_owned(), res);
            lines
        }
        Kind::Count => vec![msg(th, &format!("&bพบ {} รายการ &7({title})", out.matched), &format!("&b{} rows found &7({title})", out.matched))],
        Kind::Rollback | Kind::Restore | Kind::Undo => {
            let rollback = job.mode == Mode::Rollback;
            c::log_info(&format!(
                "{} by {}: {} blocks, {} container records, {} skipped (chunks not loaded), {} not done, {:.1}s, {} ms of server time ({title})",
                if rollback { "rollback" } else { "restore" },
                match &owner {
                    Owner::Console => "console",
                    Owner::Player(k) => k,
                },
                out.matched - out.items,
                out.items,
                out.skipped,
                out.failed,
                secs,
                busy.as_millis()
            ));
            if kind == Kind::Undo {
                st.last_op.remove(owner.key());
            } else {
                st.last_op.insert(owner.key().to_owned(), (job.mode, job.query().clone()));
            }
            let mut note = skipped_note(th, &out);
            if out.items > 0 {
                note = format!("{}{note}", if th { format!(" &aและของในกล่อง {} รายการ", out.items) } else { format!(" &aand {} container records", out.items) });
            }
            if out.truncated {
                note.push_str(if th { " &e(ยังเหลือ สั่งซ้ำเพื่อทำต่อ)" } else { " &e(more is left; repeat to continue)" });
            }
            let blocks = out.matched - out.items;
            vec![if out.matched == 0 && out.skipped == 0 && out.failed == 0 {
                msg(th, &format!("&eไม่มีอะไรให้{} &7({title})", if rollback { "ย้อน" } else { "คืน" }), &format!("&eNothing to {} &7({title})", if rollback { "roll back" } else { "restore" }))
            } else if rollback {
                msg(th, &format!("&aย้อนกลับ {blocks} บล็อก ใน {secs:.1} วินาที{note} &7· /co undo ยกเลิกได้"), &format!("&aRolled back {blocks} blocks in {secs:.1}s{note} &7· /co undo reverts it"))
            } else {
                msg(th, &format!("&aคืนสภาพ {blocks} บล็อก ใน {secs:.1} วินาที{note} &7· /co undo ยกเลิกได้"), &format!("&aRestored {blocks} blocks in {secs:.1}s{note} &7· /co undo reverts it"))
            }]
        }
    }
}

/// Run a job for at most `budget`. True when it finished.
fn drive(run: &mut Running, store: &Store, ids: &mut HashMap<String, Option<u16>>, server: &Server, budget: Duration) -> bool {
    let began = Instant::now();
    let mut world = ServerWorld { server, worlds: None, ids };
    APPLYING.store(true, Ordering::Relaxed);
    let done = loop {
        if run.job.step(store, &mut world, 512, 8) {
            break true;
        }
        if began.elapsed() >= budget {
            break false;
        }
    };
    APPLYING.store(false, Ordering::Relaxed);
    run.busy += began.elapsed();
    done
}

/// Start a job: work on it for a moment right away (most lookups end there), else queue it.
fn start(st: &mut State, server: &Server, owner: Owner, kind: Kind, mode: Mode, query: Query, title: String) -> Vec<String> {
    let th = st.cfg.language != "en";
    if kind == Kind::Inspect {
        // A new click replaces the answer to the last one
        st.jobs.retain(|j| !(j.owner == owner && j.kind == Kind::Inspect));
    }
    if st.jobs.iter().any(|j| j.owner == owner) {
        return vec![msg(th, "&cงานก่อนหน้ายังไม่เสร็จ รอสักครู่หรือ /co cancel", "&cYour previous job is still running; wait or /co cancel")];
    }
    if st.jobs.len() >= MAX_JOBS {
        return vec![msg(th, "&cมีงานค้นหาทำอยู่มากเกินไป ลองใหม่อีกครู่", "&cToo many jobs are running; try again in a moment")];
    }
    // Only what is on disk can be found
    flush(st);
    let mut run = Running { owner, kind, job: Job::new(&st.store, mode, query), title, started: Instant::now(), busy: Duration::ZERO };
    if drive(&mut run, &st.store, &mut st.ids, server, INLINE) {
        return finish(st, run);
    }
    st.jobs.push(run);
    if kind == Kind::Inspect { Vec::new() } else { vec![msg(th, "&7กำลังทำงาน… จะแจ้งเมื่อเสร็จ", "&7Working… you will be told when it is done")] }
}

fn flush(st: &mut State) {
    st.last_flush = Instant::now();
    if st.store.buffered == 0 {
        return;
    }
    if let Err(e) = st.store.flush() {
        c::log_info(&format!("cannot write the log: {e}"));
    }
}

/// The line of a state id in the state table; asks the server for its name once.
fn state_line(st: &mut State, id: u16) -> u32 {
    if let Some(line) = st.state_ids.get(&id) {
        return *line;
    }
    let line = st.store.states.intern(&state_string(id));
    st.state_ids.insert(id, line);
    line
}

/// Move what the handlers gathered into the store.
fn drain(st: &mut State, blocks: Vec<OutBlock>, texts: Vec<TextRec>) {
    for b in blocks {
        let (user, old, new) = (st.store.users.intern(&b.user), state_line(st, b.old), state_line(st, b.new));
        st.store.log_block_ids(&b.world, b.time, user, b.pos, b.action, old, new);
        if let Some(data) = &b.nbt {
            st.store.log_nbt(&b.world, b.time, b.pos, data);
        }
    }
    for t in texts {
        st.store.log_text(&t);
    }
}

/// Store the block changes another plugin reported: one record each, under the player's name.
fn drain_edits(st: &mut State, edits: Vec<Edits>) {
    if edits.is_empty() {
        return;
    }
    let began = Instant::now();
    for batch in edits {
        let user = st.store.users.intern(&batch.user);
        for change in &batch.changes {
            if change.old == change.new {
                continue;
            }
            let (old, new) = (state_line(st, change.old), state_line(st, change.new));
            let action = if change.new == AIR { Action::Break } else { Action::Place };
            st.store.log_block_ids(&batch.world, batch.time, user, (change.x, change.y, change.z), action, old, new);
        }
    }
    st.ipc_store_micros += began.elapsed().as_micros() as u64;
}

/// A message from another plugin (see `ipc`). No server handle here: the tick finishes the work.
fn on_message(message: &[u8]) -> Result<Vec<u8>, String> {
    let began = Instant::now();
    if message.starts_with(ipc::MAGIC) {
        let batch = ipc::parse_changes(message).map_err(str::to_owned)?;
        let Some(mut guard) = try_lock(&LIVE) else { return Err("busy".into()) };
        let Some(live) = guard.as_mut() else { return Err("not loaded".into()) };
        // One message a tick is what a world edit sends; hundreds waiting means the tick is stuck
        if live.edits.len() >= 256 {
            return Err("busy".into());
        }
        if live.log.world_edit && !batch.changes.is_empty() {
            live.ipc_messages += 1;
            live.ipc_changes += batch.changes.len() as u64;
            live.edits.push(Edits { user: batch.user, world: world_key(&batch.dimension), time: now(), changes: batch.changes });
            live.ipc_micros += began.elapsed().as_micros() as u64;
        }
        return Ok(b"ok".to_vec());
    }
    if message.first() == Some(&b'{') {
        let notice = ipc::parse_notice(message).map_err(str::to_owned)?;
        let Some(mut guard) = try_lock(&LIVE) else { return Err("busy".into()) };
        let Some(live) = guard.as_mut() else { return Err("not loaded".into()) };
        return Ok(match notice {
            Some(chat) => {
                if live.log.chat && live.notices.len() < 1024 {
                    live.notices.push((chat.name, clean(&chat.message, MAX_TEXT), now()));
                }
                br#"{"ok":true}"#.to_vec()
            }
            None => br#"{"ok":false,"error":"unknown kind"}"#.to_vec(),
        });
    }
    Err("unknown message".into())
}

/// Every server tick.
fn on_tick(server: Server) {
    let Some(mut guard) = try_lock(&STATE) else { return };
    let Some(st) = guard.as_mut() else { return };
    st.tick += 1;
    let th = st.cfg.language != "en";

    // 1. What the handlers gathered
    let (blocks, texts, inspects, edits) = {
        let Some(mut live_guard) = try_lock(&LIVE) else { return };
        let Some(live) = live_guard.as_mut() else { return };
        live.tick = st.tick;
        (live.explosions, live.natural, live.trees) = (0, 0, 0);
        let tick = live.tick;
        if live.watches.iter().any(|w| tick >= w.due) {
            let worlds: Vec<(String, World)> = server.get_all_worlds().into_iter().map(|w| (w.get_dimension(), w)).collect();
            for (dim, world) in &worlds {
                settle_where(live, world, dim, false, |w| tick >= w.due);
            }
            // A world that is gone takes its watches with it
            live.watches.retain(|w| tick < w.due || worlds.iter().any(|(d, _)| *d == w.dim));
        }
        // Chat lines a chat plugin sent itself: the notice has the name, the player has the place
        for (name, message, time) in std::mem::take(&mut live.notices) {
            // The same line already logged from the chat event (the plugin did not cancel it)
            if live.said.iter().any(|(n, m, t)| *n == name && *m == message && t.elapsed() < Duration::from_secs(2)) {
                continue;
            }
            let mut rec = match server.get_player_by_name(&name) {
                Some(p) if c::awaiting_login(&p) => continue,
                Some(p) => text_rec(&p, Action::Chat, 0, &message),
                None => TextRec { time, action: Action::Chat, user: clean(&name, 64), world: String::new(), x: 0, y: 0, z: 0, amount: 0, text: message },
            };
            rec.time = time;
            live.texts.push(rec);
        }
        if st.tick % 200 == 0 {
            live.hits.retain(|_, (_, t)| tick.saturating_sub(*t) <= 200);
            live.shots.retain(|_, (_, t)| tick.saturating_sub(*t) <= 1200);
            live.clicks.retain(|_, (_, _, t)| t.elapsed() < Duration::from_secs(30));
            live.said.retain(|(_, _, t)| t.elapsed() < Duration::from_secs(5));
            live.clicked.retain(|_, (_, t)| t.elapsed() < Duration::from_secs(5));
        }
        (std::mem::take(&mut live.blocks), std::mem::take(&mut live.texts), std::mem::take(&mut live.inspects), std::mem::take(&mut live.edits))
    };
    drain(st, blocks, texts);
    drain_edits(st, edits);

    // 2. Inspector clicks
    for req in inspects {
        let title = format!("x{}/y{}/z{}/{}", req.pos.0, req.pos.1, req.pos.2, req.world);
        let mut actions = ActionSet::of(&[Action::ContainerAdd, Action::ContainerRemove]);
        if !req.container {
            actions = ActionSet::blocks();
            actions.add(ActionSet::of(&[Action::Sign, Action::Click]));
        }
        let query = Query::everything(Area::Point { world: req.world, x: req.pos.0, y: req.pos.1, z: req.pos.2 }, actions);
        let owner = Owner::Player(req.player);
        let lines = start(st, &server, owner.clone(), Kind::Inspect, Mode::Lookup { cap: st.cfg.max_lookup_rows }, query, title);
        emit(&server, &owner, &lines);
    }

    // 3. Disk
    if st.last_flush.elapsed() >= Duration::from_secs(u64::from(st.cfg.flush_interval)) || st.store.buffered >= 8192 {
        flush(st);
    }

    // 4. Jobs, inside the time budget of this tick
    if !st.jobs.is_empty() {
        let share = Duration::from_micros(u64::from(st.cfg.tick_ms) * 1000 / st.jobs.len() as u64);
        let mut i = 0;
        while i < st.jobs.len() {
            if drive(&mut st.jobs[i], &st.store, &mut st.ids, &server, share) {
                let run = st.jobs.remove(i);
                let owner = run.owner.clone();
                let lines = finish(st, run);
                emit(&server, &owner, &lines);
            } else {
                i += 1;
            }
        }
    }

    // 5. Purge: a few files a tick
    if let Some((purge, owner, quiet)) = st.purge.as_mut() {
        if purge.step(16) {
            let line = msg(
                th,
                &format!("&aลบข้อมูลเก่า {} วัน: {} ไฟล์ {:.1} MB", purge.days_removed, purge.files_removed, purge.bytes_removed as f64 / 1_048_576.0),
                &format!("&aPurged {} old days: {} files, {:.1} MB", purge.days_removed, purge.files_removed, purge.bytes_removed as f64 / 1_048_576.0),
            );
            if !*quiet || purge.files_removed > 0 {
                c::log_info(&c::plain(&line));
                if *owner != Owner::Console {
                    emit(&server, owner, &[line]);
                }
            }
            st.purge = None;
        }
    } else if st.cfg.retention_days > 0 && Instant::now() >= st.next_auto_purge {
        st.next_auto_purge = Instant::now() + Duration::from_secs(3600);
        let first_kept = (now() / 86_400).saturating_sub(st.cfg.retention_days);
        let purge = Purge::new(&st.store, first_kept);
        if !purge.done {
            st.purge = Some((purge, Owner::Console, true));
        }
    }

    // 6. Results nobody pages through any more
    if st.tick % 1200 == 0 {
        st.results.retain(|_, r| r.at.elapsed() < Duration::from_secs(900));
    }
}

// ---------------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------------

fn param_error(th: bool, e: &ParamError) -> String {
    match e {
        ParamError::Time(v) => msg(th, &format!("&cเวลาไม่ถูกต้อง: {} &7(ตัวอย่าง t:30m t:2h t:7d t:1d12h)", safe(v)), &format!("&cInvalid time: {} &7(examples: t:30m t:2h t:7d t:1d12h)", safe(v))),
        ParamError::Radius(v) => msg(th, &format!("&cรัศมีไม่ถูกต้อง: {} &7(ตัวอย่าง r:10 r:#global r:#world r:#nether)", safe(v)), &format!("&cInvalid radius: {} &7(examples: r:10 r:#global r:#world r:#nether)", safe(v))),
        ParamError::Action(v) => msg(
            th,
            &format!("&cไม่รู้จัก action: {} &7(block +block -block bucket burn explosion container kill item click sign chat command session)", safe(v)),
            &format!("&cUnknown action: {} &7(block +block -block bucket burn explosion container kill item click sign chat command session)", safe(v)),
        ),
        ParamError::Name(v) => msg(th, &format!("&cชื่อไม่ถูกต้อง: {}", safe(v)), &format!("&cInvalid name: {}", safe(v))),
        ParamError::Unknown(v) => msg(th, &format!("&cไม่เข้าใจ: {} &7(ใช้ u: t: r: a: i: e:)", safe(v)), &format!("&cNot understood: {} &7(use u: t: r: a: i: e:)", safe(v))),
        ParamError::TooMany => msg(th, "&cใส่ค่ามากเกินไป", "&cToo many values"),
    }
}

/// Turn parsed arguments into a query for this sender. `Err` is the line to show.
fn build_query(st: &State, p: &Params, player: Option<&Player>, default_actions: ActionSet) -> Result<Query, String> {
    let th = st.cfg.language != "en";
    let need_player = || msg(th, "&cจาก console ใช้ r:#global หรือ r:#world / r:#nether / r:#end", "&cFrom the console use r:#global or r:#world / r:#nether / r:#end");
    let area = match &p.radius {
        None | Some(Radius::Global) => Area::Global,
        Some(Radius::World(Some(world))) => Area::World(world.clone()),
        Some(Radius::World(None)) => Area::World(world_key(&player.ok_or_else(need_player)?.get_world().get_dimension())),
        Some(Radius::Blocks(r)) => {
            let player = player.ok_or_else(need_player)?;
            if *r > st.cfg.max_radius {
                return Err(msg(th, &format!("&cรัศมีสูงสุดคือ {} (ทั้งโลกใช้ r:#world)", st.cfg.max_radius), &format!("&cThe largest radius is {} (use r:#world for a whole world)", st.cfg.max_radius)));
            }
            let at = feet(player);
            Area::Column { world: world_key(&player.get_world().get_dimension()), x: at.0, z: at.2, r: *r as i32 }
        }
    };
    let now = c::now_secs();
    let (since, until) = match p.time {
        Some((older, newer)) => (now.saturating_sub(older), now.saturating_sub(newer)),
        None => (0, u64::from(u32::MAX)),
    };
    Ok(Query {
        users: p.users.clone(),
        include: p.include.clone(),
        exclude: p.exclude.clone(),
        since: since.min(u64::from(u32::MAX)) as u32,
        until: until.min(u64::from(u32::MAX)) as u32,
        area,
        actions: p.actions.unwrap_or(default_actions),
    })
}

fn title_of(words: &[String]) -> String {
    safe(&clean(&words.join(" "), 80))
}

fn help(th: bool) -> Vec<String> {
    let lines: &[(&str, &str, &str)] = &[
        ("/co inspect &7(i)", "เปิด/ปิดโหมดตรวจ: คลิกซ้ายดูประวัติบล็อก คลิกขวาดูช่องที่จะวางหรือของในกล่อง", "toggle the inspector: left click a block for its history, right click for the spot next to it or a container"),
        ("/co lookup &7(l) u:<ผู้เล่น> t:<เวลา> r:<รัศมี> a:<action> i:<บล็อก> e:<ยกเว้น>", "ค้นประวัติ ใหม่สุดก่อน", "search the log, newest first"),
        ("/co l <หน้า> &7| /co page <หน้า>", "ดูหน้าถัดไปของผลล่าสุด", "another page of the last result"),
        ("/co near [รัศมี]", "ประวัติรอบตัว (5 บล็อก)", "what happened around you (5 blocks)"),
        ("/co rollback &7(rb) u:<ผู้เล่น> t:<เวลา> [r:<รัศมี>]", "ย้อนสิ่งที่ผู้เล่นทำ (บล็อกและของในกล่อง)", "undo what a player did (blocks and container items)"),
        ("/co restore &7(rs) u:<ผู้เล่น> t:<เวลา> [r:<รัศมี>]", "คืนสิ่งที่ย้อนไป", "put back what a rollback removed"),
        ("/co undo", "ยกเลิก rollback/restore ครั้งล่าสุดของตัวเอง", "revert your last rollback or restore"),
        ("/co purge t:<เวลา>", "ลบข้อมูลที่เก่ากว่าเวลานี้", "delete data older than this"),
        ("/co status &7| /co reload | /co version | /co cancel", "สถานะ โหลดตั้งค่า รุ่น ยกเลิกงาน", "status, reload the settings, version, cancel a job"),
    ];
    let mut out = vec!["&8----- &bBlockLog &8-----".to_owned()];
    out.extend(lines.iter().map(|(cmd, thai, english)| format!("&f{cmd} &8- &7{}", if th { thai } else { english })));
    out.push(if th { "&7เวลา: 30s 10m 2h 7d 1w 1d12h &8· &7รัศมี: 10, #global, #world, #nether, #end".into() } else { "&7time: 30s 10m 2h 7d 1w 1d12h &8· &7radius: 10, #global, #world, #nether, #end".into() });
    out
}

fn status(st: &State) -> Vec<String> {
    let th = st.cfg.language != "en";
    let (dropped, watching, inspecting, ambiguous, ipc) = try_lock(&LIVE)
        .as_ref()
        .and_then(|g| g.as_ref())
        .map_or((0, 0, 0, 0, (0, 0, 0)), |l| (l.dropped, l.watches.len(), l.inspectors.len(), l.ambiguous, (l.ipc_messages, l.ipc_changes, l.ipc_micros)));
    let l = &st.cfg.log;
    let on: Vec<&str> = [
        ("break", l.block_break),
        ("place", l.block_place),
        ("bucket", l.bucket),
        ("burn", l.burn),
        ("explosion", l.explosion),
        ("sign", l.sign),
        ("container", l.container),
        ("chat", l.chat),
        ("command", l.command),
        ("session", l.session),
        ("kill", l.kill),
        ("item", l.item),
        ("world-edit", l.world_edit),
        ("ignite", l.ignite),
        ("fire-spread", l.fire_spread),
        ("natural-break", l.natural_break),
        ("entity-change", l.entity_change),
        ("click", l.click),
        ("leaf-decay", l.leaf_decay),
        ("growth", l.growth),
        ("tree-growth", l.tree_growth),
        ("flow", l.flow),
        ("piston", l.piston),
        ("hopper", l.hopper),
    ]
    .iter()
    .filter(|(_, on)| *on)
    .map(|(name, _)| *name)
    .collect();
    let mut out = vec![format!("&8----- &bBlockLog {VERSION} &8-----")];
    out.push(if th { format!("&7บันทึก: &f{}", on.join(" ")) } else { format!("&7logging: &f{}", on.join(" ")) });
    out.push(if th {
        format!(
            "&7รอบนี้: &f{} บล็อก {} เหตุการณ์ &7ลงดิสก์แล้ว, รอเขียน &f{}&7, เสีย &f{}&7, ข้ามเพราะมากเกิน &f{}",
            st.store.written_blocks, st.store.written_text, st.store.buffered, st.store.lost, dropped
        )
    } else {
        format!(
            "&7this run: &f{} block and {} other records &7on disk, &f{} &7waiting, &f{} &7lost, &f{} &7dropped in floods",
            st.store.written_blocks, st.store.written_text, st.store.buffered, st.store.lost, dropped
        )
    });
    for world in st.store.worlds() {
        let days = st.store.block_days(&world);
        if let (Some(first), Some(last)) = (days.first(), days.last()) {
            out.push(if th {
                format!("&7{world}: &f{} วัน &7({} ถึง {})", days.len(), day_name(*first), day_name(*last))
            } else {
                format!("&7{world}: &f{} days &7({} to {})", days.len(), day_name(*first), day_name(*last))
            });
        }
    }
    out.push(if th {
        format!(
            "&7ผู้ใช้ &f{} &7ชนิดบล็อก &f{} &7· เก็บ &f{} &7· งาน &f{} &7· กำลังตรวจ &f{} &7· กำลังจับตา &f{} &7· เปิดมา &f{}",
            st.store.users.len(),
            st.store.states.len(),
            if st.cfg.retention_days == 0 { "ตลอดไป".to_owned() } else { format!("{} วัน", st.cfg.retention_days) },
            st.jobs.len() + usize::from(st.purge.is_some()),
            inspecting,
            watching,
            timefmt::span(st.loaded.elapsed().as_secs())
        )
    } else {
        format!(
            "&7users &f{} &7block states &f{} &7· keeping &f{} &7· jobs &f{} &7· inspecting &f{} &7· watching &f{} &7· up &f{}",
            st.store.users.len(),
            st.store.states.len(),
            if st.cfg.retention_days == 0 { "forever".to_owned() } else { format!("{} days", st.cfg.retention_days) },
            st.jobs.len() + usize::from(st.purge.is_some()),
            inspecting,
            watching,
            timefmt::span(st.loaded.elapsed().as_secs())
        )
    });
    if ipc.0 > 0 {
        // What a world edit costs: receiving a message, and storing its records in the tick
        let (messages, changes, receive) = ipc;
        out.push(if th {
            format!(
                "&7จากปลั๊กอินอื่น: &f{messages} &7ข้อความ &f{changes} &7บล็อก · รับ &f{:.0} µs &7+ เก็บ &f{:.0} µs &7ต่อข้อความ",
                receive as f64 / messages as f64,
                st.ipc_store_micros as f64 / messages as f64
            )
        } else {
            format!(
                "&7from other plugins: &f{messages} &7messages, &f{changes} &7blocks · &f{:.0} µs &7to receive + &f{:.0} µs &7to store per message",
                receive as f64 / messages as f64,
                st.ipc_store_micros as f64 / messages as f64
            )
        });
    }
    if ambiguous > 0 {
        out.push(if th {
            format!("&7เหตุการณ์ที่บอกไม่ได้ว่าอยู่โลกไหน (ไม่บันทึก): &f{ambiguous}")
        } else {
            format!("&7events whose world could not be told (not logged): &f{ambiguous}")
        });
    }
    if let Some(e) = &st.store.last_error {
        out.push(if th { format!("&cเขียนดิสก์ผิดพลาดล่าสุด: {}", safe(e)) } else { format!("&clast write error: {}", safe(e)) });
    }
    out
}

fn run(st: &mut State, player: Option<&Player>, server: &Server, w: &[String]) -> Vec<String> {
    let th = st.cfg.language != "en";
    let can = |node: &str| player.is_none_or(|p| p.has_permission(&format!("{NS}:{node}")));
    let denied = || vec![msg(th, "&cคุณไม่มีสิทธิ์ใช้คำสั่งนี้", "&cYou do not have permission for this")];
    let only_player = || vec![msg(th, "&cคำสั่งนี้ใช้ได้เฉพาะในเกม", "&cOnly players can use this")];
    let owner = match player {
        Some(p) => Owner::Player(key(p)),
        None => Owner::Console,
    };
    let sub = w.first().map_or_else(|| "help".to_owned(), |s| s.to_ascii_lowercase());
    let rest = w.get(1..).unwrap_or(&[]);
    // Someone with no BlockLog permission at all learns nothing, not even the help
    if !["inspect", "lookup", "rollback", "restore", "purge", "reload", "status"].iter().any(|n| can(n)) {
        return denied();
    }
    let per_page = st.cfg.page_size;
    let show_page = |st: &State, page: usize| match st.results.get(owner.key()) {
        Some(res) => page_lines(th, per_page, res, page),
        None => vec![msg(th, "&eยังไม่มีผลค้นหา ใช้ /co lookup ก่อน", "&eNo lookup yet; use /co lookup first")],
    };

    match sub.as_str() {
        "help" | "?" => help(th),
        "version" => vec![msg(th, &format!("&fBlockLog {VERSION}"), &format!("&fBlockLog {VERSION}"))],
        "inspect" | "i" => {
            if !can("inspect") {
                return denied();
            }
            let Some(p) = player else { return only_player() };
            let Some(mut guard) = try_lock(&LIVE) else { return Vec::new() };
            let Some(live) = guard.as_mut() else { return Vec::new() };
            let k = key(p);
            if live.inspectors.remove(&k) {
                vec![msg(th, "&bปิดโหมดตรวจแล้ว", "&bInspector off")]
            } else {
                live.inspectors.insert(k);
                vec![msg(th, "&bเปิดโหมดตรวจแล้ว &7คลิกซ้าย = ประวัติบล็อก, คลิกขวา = ช่องที่จะวางหรือของในกล่อง (/co i ปิด)", "&bInspector on &7left click = block history, right click = the spot next to it or a container (/co i turns it off)")]
            }
        }
        "page" => {
            if !can("lookup") {
                return denied();
            }
            match rest.first().and_then(|s| s.parse::<usize>().ok()) {
                Some(page) => show_page(st, page),
                None => vec![msg(th, "&cวิธีใช้: /co page <หน้า>", "&cUsage: /co page <page>")],
            }
        }
        "lookup" | "l" | "near" => {
            if !can("lookup") {
                return denied();
            }
            let mut p = match params::parse(rest) {
                Ok(p) => p,
                Err(e) => return vec![param_error(th, &e)],
            };
            let mut title = title_of(rest);
            if sub == "near" {
                if player.is_none() {
                    return only_player();
                }
                // `/co near 10`
                p.radius = Some(Radius::Blocks(p.page.take().unwrap_or(5)));
                title = format!("near {title}").trim().to_owned();
            } else if let (Some(page), true) = (p.page, Params { page: None, ..p.clone() } == Params::default()) {
                return show_page(st, page as usize);
            } else if rest.is_empty() {
                return vec![msg(th, "&cวิธีใช้: /co lookup u:<ผู้เล่น> t:<เวลา> r:<รัศมี> a:<action> i:<บล็อก> e:<ยกเว้น>", "&cUsage: /co lookup u:<player> t:<time> r:<radius> a:<action> i:<block> e:<exclude>")];
            }
            let query = match build_query(st, &p, player, ActionSet::default_lookup()) {
                Ok(q) => q,
                Err(line) => return vec![line],
            };
            let (kind, mode) = if p.count { (Kind::Count, Mode::Count) } else { (Kind::Lookup, Mode::Lookup { cap: st.cfg.max_lookup_rows }) };
            let mut lines = start(st, server, owner.clone(), kind, mode, query, title);
            // `/co l u:Alice 2` asks for a page right away
            if let (Some(page), true) = (p.page, st.results.contains_key(owner.key()) && !st.jobs.iter().any(|j| j.owner == owner)) {
                if page > 1 && kind == Kind::Lookup {
                    lines = show_page(st, page as usize);
                }
            }
            lines
        }
        "rollback" | "rb" | "restore" | "rs" => {
            let rollback = sub.starts_with("rollback") || sub == "rb";
            if !can(if rollback { "rollback" } else { "restore" }) {
                return denied();
            }
            let p = match params::parse(rest) {
                Ok(p) => p,
                Err(e) => return vec![param_error(th, &e)],
            };
            if (p.users.is_empty() && p.radius.is_none()) || p.time.is_none() {
                return vec![msg(
                    th,
                    &format!("&cต้องระบุ u:<ผู้เล่น> หรือ r:<รัศมี> และ t:<เวลา> &7เช่น /co {sub} u:Steve t:1h r:20"),
                    &format!("&cGive u:<player> or r:<radius>, and t:<time> &7e.g. /co {sub} u:Steve t:1h r:20"),
                )];
            }
            // Blocks and what was put into or taken out of containers can be put back; words cannot
            let mut can = ActionSet::blocks();
            can.add(ActionSet::of(&[Action::ContainerAdd, Action::ContainerRemove]));
            if p.actions.is_some_and(|a| a.0 & !can.0 != 0) {
                return vec![msg(
                    th,
                    "&cย้อนได้เฉพาะบล็อกและของในกล่อง (a:block +block -block bucket burn explosion container)",
                    "&cOnly block changes and container items can be rolled back (a:block +block -block bucket burn explosion container)",
                )];
            }
            let query = match build_query(st, &p, player, can) {
                Ok(q) => q,
                Err(line) => return vec![line],
            };
            let (kind, mode) = if rollback { (Kind::Rollback, Mode::Rollback) } else { (Kind::Restore, Mode::Restore) };
            start(st, server, owner, kind, mode, query, title_of(rest))
        }
        "undo" => {
            let Some((mode, query)) = st.last_op.get(owner.key()).cloned() else {
                return vec![msg(th, "&eไม่มี rollback หรือ restore ให้ยกเลิก", "&eNo rollback or restore to undo")];
            };
            let (mode, node) = if mode == Mode::Rollback { (Mode::Restore, "restore") } else { (Mode::Rollback, "rollback") };
            if !can(node) {
                return denied();
            }
            start(st, server, owner, Kind::Undo, mode, query, "undo".into())
        }
        "cancel" => {
            let before = st.jobs.len();
            // Dropping the job writes the rolled-back bits of what it already did
            st.jobs.retain(|j| j.owner != owner);
            if st.jobs.len() < before {
                vec![msg(th, "&bยกเลิกงานแล้ว (ส่วนที่ทำไปแล้วยังอยู่ สั่งซ้ำเพื่อทำต่อได้)", "&bJob cancelled (what was done stays; repeat the command to continue)")]
            } else {
                vec![msg(th, "&eไม่มีงานที่กำลังทำ", "&eNo job is running")]
            }
        }
        "purge" => {
            // Deleting history is for the console and full operators
            if !can("purge") || player.is_some_and(|p| (p.get_permission_level() as u8) < 4) {
                return denied();
            }
            let p = match params::parse(rest) {
                Ok(p) => p,
                Err(e) => return vec![param_error(th, &e)],
            };
            let Some((older, _)) = p.time else {
                return vec![msg(th, "&cวิธีใช้: /co purge t:<เวลา> &7เช่น t:30d = ลบข้อมูลที่เก่ากว่า 30 วัน", "&cUsage: /co purge t:<time> &7e.g. t:30d = delete data older than 30 days")];
            };
            let least = if player.is_some() { PURGE_MIN_PLAYER } else { PURGE_MIN_CONSOLE };
            if older < least {
                return vec![msg(
                    th,
                    &format!("&cลบได้เฉพาะข้อมูลที่เก่ากว่า {} (ในเกม 30d, console 24h)", timefmt::span(least)),
                    &format!("&cOnly data older than {} can be purged (30d in game, 24h from the console)", timefmt::span(least)),
                )];
            }
            if st.purge.is_some() {
                return vec![msg(th, "&cกำลังลบข้อมูลเก่าอยู่แล้ว", "&cA purge is already running")];
            }
            // Whole days only: a day goes when all of it is older than the limit
            let first_kept = (c::now_secs().saturating_sub(older) / 86_400) as u32;
            let purge = Purge::new(&st.store, first_kept);
            if purge.done {
                return vec![msg(th, &format!("&eไม่มีข้อมูลก่อนวันที่ {}", day_name(first_kept)), &format!("&eNo data from before {}", day_name(first_kept)))];
            }
            let days = purge.days_removed;
            c::log_info(&format!("purge by {}: {} days before {}", owner.key(), days, day_name(first_kept)));
            st.purge = Some((purge, owner, false));
            vec![msg(th, &format!("&7กำลังลบข้อมูล {days} วันก่อนวันที่ {}…", day_name(first_kept)), &format!("&7Deleting {days} days from before {}…", day_name(first_kept)))]
        }
        "status" => {
            if !can("status") {
                return denied();
            }
            status(st)
        }
        "reload" => {
            if !can("reload") {
                return denied();
            }
            st.cfg = load_config();
            st.next_auto_purge = Instant::now() + Duration::from_secs(5);
            if let Some(live) = try_lock(&LIVE).as_mut().and_then(|g| g.as_mut()) {
                live.log = st.cfg.log;
            }
            let th = st.cfg.language != "en";
            vec![msg(th, "&aโหลดไฟล์ตั้งค่าใหม่แล้ว", "&aConfiguration reloaded")]
        }
        _ => vec![msg(th, &format!("&cไม่รู้จักคำสั่ง {} &7ดู /co help", safe(&clean(&sub, 24))), &format!("&cUnknown command {} &7see /co help", safe(&clean(&sub, 24))))],
    }
}

struct Cmd;

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let player = c::sender_player(&sender);
        if player.as_ref().is_some_and(c::awaiting_login) {
            return Ok(0);
        }
        let Some(mut guard) = try_lock(&STATE) else { return Ok(0) };
        let Some(st) = guard.as_mut() else { return Ok(0) };
        for line in run(st, player.as_ref(), &server, &w) {
            c::reply(&sender, &line);
        }
        Ok(1)
    }
}

struct BlockLog;

impl Plugin for BlockLog {
    fn new() -> Self {
        BlockLog
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Block logging and rollback by MCSV".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let cfg = load_config();
        let log = cfg.log;
        let store = Store::open(&c::data_path(""));
        *LIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Live {
            tick: 0,
            log: cfg.log,
            inspectors: HashSet::new(),
            clicks: HashMap::new(),
            watches: Vec::new(),
            blocks: Vec::new(),
            texts: Vec::new(),
            inspects: Vec::new(),
            containers: HashMap::new(),
            hits: HashMap::new(),
            ids: HashMap::new(),
            explosions: 0,
            dropped: 0,
            shots: HashMap::new(),
            natural: 0,
            trees: 0,
            ambiguous: 0,
            edits: Vec::new(),
            clicked: HashMap::new(),
            notices: Vec::new(),
            said: Vec::new(),
            ipc_messages: 0,
            ipc_changes: 0,
            ipc_micros: 0,
        });
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State {
            cfg,
            store,
            ids: HashMap::new(),
            jobs: Vec::new(),
            results: HashMap::new(),
            last_op: HashMap::new(),
            purge: None,
            last_flush: Instant::now(),
            // Old days go a little after start-up, then once an hour
            next_auto_purge: Instant::now() + Duration::from_secs(30),
            loaded: Instant::now(),
            tick: 0,
            state_ids: HashMap::new(),
            ipc_store_micros: 0,
        });
        // The panel reads this to tell "loaded" from "installed, waiting for a restart"
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": c::now_secs() }));

        let op = |level| PermissionDefault::Op(level);
        for (node, default) in [
            // Everyone may type /co; each subcommand asks for its own node, so a rank needs only that one
            ("command", PermissionDefault::Allow),
            ("inspect", op(PermissionLevel::Two)),
            ("lookup", op(PermissionLevel::Two)),
            ("rollback", op(PermissionLevel::Two)),
            ("restore", op(PermissionLevel::Two)),
            ("status", op(PermissionLevel::Two)),
            ("reload", op(PermissionLevel::Two)),
            ("purge", op(PermissionLevel::Four)),
        ] {
            let _ = context.register_permission(&Permission { node: format!("{NS}:{node}"), description: "BlockLog".into(), default, children: vec![] });
        }

        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let cmd = Command::new(&["co".to_owned(), "coreprotect".to_owned(), NS.to_owned()], "BlockLog: block log, inspector, rollback")
            .execute(Cmd)
            .then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:command"));

        // Blocking = may cancel (the inspector's clicks). Not blocking = runs after every blocking
        // handler of every plugin and sees whether the event was cancelled: only what really
        // happens is logged. Pumpkin 0.2.2 ignores the priority.
        let p = EventPriority::Normal;
        context.register_event_handler(OnBreak, p, true)?;
        context.register_event_handler(OnInteract, p, true)?;
        context.register_event_handler(OnChat, p, false)?;
        context.register_event_handler(OnPlace, p, false)?;
        context.register_event_handler(OnBucketFill, p, false)?;
        context.register_event_handler(OnBurn, p, false)?;
        context.register_event_handler(OnExplode, p, false)?;
        context.register_event_handler(OnSign, p, false)?;
        context.register_event_handler(OnCommand, p, false)?;
        context.register_event_handler(OnJoin, p, false)?;
        context.register_event_handler(OnLeave, p, false)?;
        context.register_event_handler(OnInvOpen, p, false)?;
        context.register_event_handler(OnInvClose, p, false)?;
        context.register_event_handler(OnDrop, p, false)?;
        context.register_event_handler(OnHit, p, false)?;
        context.register_event_handler(OnEntityDeath, p, false)?;
        context.register_event_handler(OnPlayerDeath, p, false)?;
        context.register_event_handler(OnLaunch, p, false)?;
        context.register_event_handler(OnSpread, p, false)?;
        context.register_event_handler(OnEnderman, p, false)?;
        context.register_event_handler(OnClick, p, false)?;
        if log.hopper {
            context.register_event_handler(OnHopper, p, false)?;
        }
        // The noisy ones cost a call into the plugin for every leaf, crop, flow step and piston
        // stroke of the server, so they are only listened to when switched on (at load: Pumpkin
        // has no way to stop listening)
        if log.leaf_decay {
            context.register_event_handler(OnDecay, p, false)?;
        }
        if log.growth {
            context.register_event_handler(OnGrow, p, false)?;
        }
        if log.tree_growth {
            context.register_event_handler(OnTree, p, false)?;
        }
        if log.flow {
            context.register_event_handler(OnFlow, p, false)?;
        }
        if log.piston {
            context.register_event_handler(OnPistonOut, p, false)?;
            context.register_event_handler(OnPistonIn, p, false)?;
        }
        pumpkin_plugin_api::scheduler::schedule_repeating_task(1, 1, on_tick);
        Ok(())
    }

    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        on_message(&message)
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        // What the handlers gathered since the last tick, then everything to disk. Dropping the
        // jobs writes the rolled-back bits of a rollback that was still running.
        let (blocks, texts, edits) = match LIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut() {
            Some(live) => (std::mem::take(&mut live.blocks), std::mem::take(&mut live.texts), std::mem::take(&mut live.edits)),
            None => (Vec::new(), Vec::new(), Vec::new()),
        };
        if let Some(st) = STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut() {
            st.jobs.clear();
            drain(st, blocks, texts);
            drain_edits(st, edits);
            flush(st);
        }
        c::remove_file("status.json");
        Ok(())
    }
}

register_plugin!(BlockLog);

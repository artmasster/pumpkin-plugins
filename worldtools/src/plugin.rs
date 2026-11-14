//! Pumpkin WASM glue for WorldTools.
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::events::{BlockBreakEvent, EventHandler, EventPriority, FromIntoEvent, PlayerInteractEvent, PlayerLeaveEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::wit::pumpkin::plugin::common::BlockPos;
use pumpkin_plugin_api::wit::pumpkin::plugin::event::InteractAction;
use pumpkin_plugin_api::wit::pumpkin::plugin::world::BlockFlags;
use pumpkin_plugin_api::world::{self as host, World};
use pumpkin_plugin_api::{Context, ItemStack, Plugin, PluginMetadata, Server, register_plugin};
use serde::{Deserialize, Serialize};

use crate::geo::{self, Dir, P, Region};
use crate::schem::{self, Schematic};
use crate::spec::{self, BlockSpec, Facts, Mask, Palette, Pattern, Props, StateError};
use crate::xform::{Axis, Xf};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NS: &str = "worldtools";
const AIR: u16 = 0;
/// Most block changes in one message to BlockLog, and so in one slice of a tick.
const BATCH: usize = 4096;
/// Our own schematic files and the Sponge ones we can read.
const EXT: &str = "worldtools";
const SPONGE_EXT: &str = "schem";

/// Commands, typed `//set` like WorldEdit or `/worldtools set`.
const SUBS: &[&str] = &[
    "wand", "pos1", "pos2", "hpos1", "hpos2", "desel", "size", "count", "expand", "contract", "shift", "set", "replace", "walls", "faces",
    "cyl", "hcyl", "sphere", "hsphere", "copy", "cut", "paste", "undo", "redo", "cancel",
    // 0.6.0
    "gmask", "stack", "move", "overlay", "drain", "hollow", "outline", "center", "line", "pyramid", "hpyramid", "rotate", "flip", "schem",
    "brush", "sel", "fill", "fillr", "replacenear", "removenear", "naturalize", "distr", "chunk", "inset", "outset", "clearclipboard",
];

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct Config {
    /// th | en
    language: String,
    /// Largest edit, in blocks.
    max_blocks: u32,
    /// Milliseconds of each server tick that edits may use, shared by all running edits (1..=40).
    /// A tick is 50 ms; lower = smoother server, slower edits.
    tick_ms: u32,
    /// Edits kept per player for //undo, and the most blocks all of them may hold together.
    history: usize,
    history_blocks: usize,
    /// Item that selects corners: left click = first, right click = second.
    wand: String,
    /// Largest radius of a brush, the least time between two strokes of one player, and how far
    /// away the block a brush paints on may be.
    brush_max_radius: u32,
    brush_interval_ms: u32,
    brush_range: u32,
    /// Schematic files in plugins/data/worldtools/schematics: largest file and how many may exist.
    schem_max_kb: u32,
    schem_max_files: u32,
    /// Show the corners and edges of the selection as fake blocks (each player: //sel show|hide),
    /// for how long, and with which block.
    sel_show: bool,
    sel_show_secs: u32,
    sel_block: String,
    /// Tell the BlockLog plugin (when installed) about every block WorldTools changes.
    blocklog: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: "th".into(),
            max_blocks: 500_000,
            tick_ms: 10,
            history: 15,
            history_blocks: 2_000_000,
            wand: "minecraft:wooden_axe".into(),
            brush_max_radius: 6,
            brush_interval_ms: 250,
            brush_range: 120,
            schem_max_kb: 1024,
            schem_max_files: 50,
            sel_show: false,
            sel_show_secs: 10,
            sel_block: "minecraft:sea_lantern".into(),
            blocklog: true,
        }
    }
}

/// Blocks with the state they had (undo) or should get (redo, paste).
struct Edit {
    dim: String,
    blocks: Vec<(P, u16)>,
}

/// Copied blocks, relative to where the player stood (`origin`, kept for //paste -o).
struct Clip {
    blocks: Vec<(P, u16)>,
    origin: P,
    dim: String,
}

struct Brush {
    cylinder: bool,
    pattern: Palette,
    radius: i32,
    height: i32,
}

#[derive(Default)]
struct Session {
    dim: String,
    a: Option<P>,
    b: Option<P>,
    clip: Option<Clip>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    last_click: Option<(P, bool, Instant)>,
    /// What every edit of this player is limited to (//gmask), with the text it came from.
    gmask: Option<(String, Mask)>,
    /// Item in hand (without `minecraft:`) -> brush.
    brushes: HashMap<String, Brush>,
    last_stroke: Option<Instant>,
    last_file: Option<Instant>,
    /// `None` = the server default (`sel-show`).
    show: Option<bool>,
    /// Fake blocks the player currently sees, and when they are taken back.
    marks: Vec<P>,
    marks_until: Option<Instant>,
    /// The selection changed: draw it on the next tick (not inside the click that changed it,
    /// where the server is about to resend the clicked block).
    redraw: bool,
}

/// What happens to blocks that were read once all of them are in.
enum Then {
    /// Copies towards a direction, `stride` apart.
    Stack { n: i32, stride: P, skip_air: bool },
    /// The blocks go `delta` further; what they leave behind is filled.
    Move { from: Region, delta: P, fill: Palette, skip_air: bool },
    /// The inside of whatever is solid in the region is filled.
    Hollow { region: Region, thickness: usize, fill: Palette },
}

enum Flood {
    /// Water and lava connected to the start, and with `true` the water inside waterlogged blocks.
    Drain(bool),
    /// Air connected to the start: sideways on the top layer and straight down (`false`), or
    /// in every direction (`true`), between `bottom` and `top`.
    Fill { pattern: Palette, top: i32, bottom: i32, recursive: bool },
}

enum Work {
    /// Set every position from the pattern.
    Fill(Palette),
    /// Set positions whose block is in the mask.
    Replace(Mask, Palette),
    /// Set each position to the state stored next to it.
    States(Vec<u16>),
    /// Count positions whose block is in the mask.
    Count(Mask),
    /// Count every kind of block; `true` = every state on its own (//distr -d).
    Distr(bool),
    /// Remember the blocks relative to the origin; `true` also clears them (//cut).
    Copy(P, bool),
    /// Remember the blocks where they are, then carry on with `Then`.
    Read(Then),
    /// One position per column: put a block on the highest block between the two heights.
    Overlay(Palette, i32, i32),
    /// One position per column: grass on top, three of dirt, stone below, for the blocks in the mask.
    Naturalize(Mask, [u16; 3], i32, i32),
    /// Spread from the start positions within the radius (squared) of the origin; `todo` grows.
    Flood(Flood, P, i64),
}

#[derive(PartialEq)]
enum Keep {
    /// A new edit: goes to undo and clears redo.
    Undo,
    /// Redoing: goes to undo, redo stays.
    UndoKeepRedo,
    /// Undoing: goes to redo.
    Redo,
}

/// Everything about a running job except what it does.
struct Run {
    owner: String,
    /// The player's name, for BlockLog.
    name: String,
    label: String,
    dim: String,
    /// Lowest and highest block of the world.
    span: (i32, i32),
    todo: Vec<P>,
    at: usize,
    changed: u64,
    /// Blocks in chunks that are not loaded: the server cannot change those.
    skipped: u64,
    found: u64,
    /// A flood that stopped at `max-blocks`.
    cut_short: bool,
    before: Vec<(P, u16)>,
    clip: Vec<(P, u16)>,
    keep: Keep,
    /// A brush stroke: the result goes to the action bar, not the chat.
    quiet: bool,
    started: Instant,
    /// Time spent inside the server tick.
    busy: Duration,
    /// State id -> whether it is in the mask of the work / in the global mask.
    masked: HashMap<u16, bool>,
    gmask: Option<Mask>,
    gmasked: HashMap<u16, bool>,
    /// The chunk of the last block and whether it is loaded.
    chunk: Option<((i32, i32), bool)>,
    /// Positions a flood has already queued.
    seen: HashSet<P>,
    distr: HashMap<u16, u64>,
    /// Changes of this slice for BlockLog: position, old state, new state.
    batch: Vec<(P, u16, u16)>,
    /// BlockLog answered so far (or was not asked yet).
    report: bool,
    told: u64,
    told_messages: u32,
    told_time: Duration,
    /// Slices in a row in which BlockLog was busy, and changes it never got.
    refused: u32,
    untold: u64,
}

struct Job {
    work: Work,
    r: Run,
}

/// Every state of a block with its properties.
struct Table {
    default: u16,
    states: Vec<(u16, Props)>,
}

/// What the server told us about blocks, kept so each question is asked once.
#[derive(Default)]
struct Cache {
    facts: HashMap<u16, Facts>,
    tables: HashMap<String, Table>,
}

impl Cache {
    fn facts(&mut self, state: u16) -> &Facts {
        if self.facts.len() >= 8192 && !self.facts.contains_key(&state) {
            self.facts.clear();
        }
        self.facts.entry(state).or_insert_with(|| {
            let (name, props) = host::block_state_to_info(state).map_or_else(|| ("minecraft:air".to_owned(), Props::new()), |i| (i.name, i.properties));
            let (air, solid) = host::get_block_state_by_id(state).map_or((state == AIR, false), |s| (s.is_air, s.is_solid));
            // By name: the server's `is-liquid` is also true for a waterlogged slab or fence
            let liquid = matches!(name.trim_start_matches("minecraft:"), "water" | "lava" | "bubble_column");
            Facts { name, props, air, solid, liquid }
        })
    }

    fn is_air(&mut self, state: u16) -> bool {
        state == AIR || self.facts(state).air
    }

    /// `None` when the server has no block of that name.
    fn table(&mut self, name: &str) -> Option<&Table> {
        if !self.tables.contains_key(name) {
            let block = host::get_block_by_name(name)?;
            if self.tables.len() >= 128 {
                self.tables.clear();
            }
            // Never `resolve-block-state`: on a value the block does not have, Pumpkin 0.2.2 panics
            // inside the host and the panic hook stops the server. Asking for the properties of
            // each state of the block cannot fail.
            let states = block.state_ids.iter().map(|id| (*id, host::get_block_properties(*id))).collect();
            self.tables.insert(name.to_owned(), Table { default: block.default_state_id, states });
        }
        self.tables.get(name)
    }

    /// The state id of a block with properties; `Err` is the line for the player.
    fn state(&mut self, spec: &BlockSpec, th: bool) -> Result<u16, String> {
        let unknown = || msg(th, &format!("&cไม่รู้จักบล็อก {}", spec.name), &format!("&cUnknown block {}", spec.name));
        if spec.props.is_empty() {
            return host::get_block_by_name(&spec.name).map(|b| b.default_state_id).ok_or_else(unknown);
        }
        let table = self.table(&spec.name).ok_or_else(unknown)?;
        spec::pick_state(&table.states, table.default, &spec.props).map_err(|e| state_error(th, &spec.name, &e))
    }

    fn pattern(&mut self, raw: &str, th: bool) -> Result<Palette, String> {
        let bad = || msg(th, &format!("&cรูปแบบบล็อกไม่ถูกต้อง: {raw} (ตัวอย่าง stone, 70%stone,30%dirt, oak_stairs[facing=north])"), &format!("&cInvalid block pattern: {raw} (examples: stone, 70%stone,30%dirt, oak_stairs[facing=north])"));
        let pattern = Pattern::parse(raw).ok_or_else(bad)?;
        let states = pattern.specs().map(|s| self.state(s, th)).collect::<Result<Vec<u16>, String>>()?;
        pattern.palette(&states).ok_or_else(bad)
    }

    fn mask(&mut self, raw: &str, th: bool) -> Result<Mask, String> {
        let mask = Mask::parse(raw).ok_or_else(|| msg(th, &format!("&cmask ไม่ถูกต้อง: {raw} (ตัวอย่าง stone,dirt  !air  #existing  #solid  oak_stairs[half=top])"), &format!("&cInvalid mask: {raw} (examples: stone,dirt  !air  #existing  #solid  oak_stairs[half=top])")))?;
        for s in mask.specs() {
            let table = self.table(&s.name).ok_or_else(|| msg(th, &format!("&cไม่รู้จักบล็อก {}", s.name), &format!("&cUnknown block {}", s.name)))?;
            spec::check_props(&table.states, &s.props).map_err(|e| state_error(th, &s.name, &e))?;
        }
        Ok(mask)
    }
}

fn state_error(th: bool, block: &str, e: &StateError) -> String {
    match e {
        StateError::Property(k, have) if have.is_empty() => msg(th, &format!("&c{block} ไม่มี property ใดเลย (ไม่มี {k})"), &format!("&c{block} has no properties (no {k})")),
        StateError::Property(k, have) => msg(th, &format!("&c{block} ไม่มี property {k} (มี: {})", have.join(", ")), &format!("&c{block} has no property {k} (it has: {})", have.join(", "))),
        StateError::Value(k, v, can) => msg(th, &format!("&c{k} ของ {block} เป็น {v} ไม่ได้ (ใช้ได้: {})", can.join(", ")), &format!("&c{k} of {block} cannot be {v} (use: {})", can.join(", "))),
        StateError::Combination => msg(th, &format!("&c{block} ไม่มี state ที่มี property ชุดนี้"), &format!("&c{block} has no state with these properties")),
    }
}

struct State {
    cfg: Config,
    sessions: HashMap<String, Session>,
    jobs: Vec<Job>,
    seed: u64,
    cache: Cache,
    /// Some session wants its selection drawn or its marks taken back.
    marks: bool,
    ticks: u64,
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

fn key(p: &Player) -> String {
    let id = p.get_id();
    format!("{:016x}{:016x}", id.high, id.low)
}

fn pos(p: P) -> BlockPos {
    BlockPos { x: p.0, y: p.1, z: p.2 }
}

fn feet(p: &Player) -> P {
    let (x, y, z) = p.get_position();
    (x.floor() as i32, y.floor() as i32, z.floor() as i32)
}

fn world_of(server: &Server, dim: &str) -> Option<World> {
    server.get_all_worlds().into_iter().find(|w| w.get_dimension() == dim)
}

fn height(world: &World) -> (i32, i32) {
    let lo = world.get_min_y();
    (lo, if lo < 0 { 319 } else { 255 })
}

/// Never NOTIFY_NEIGHBORS: a big edit must not start a wave of block physics. Measured in the
/// lab (2026-11-11): what a write costs is the server's light update (15 to 150 microseconds a
/// block, by how much light changes), not the call into the server; skipping the "block added"
/// callback or changing the order of the blocks made no difference.
fn flags() -> BlockFlags {
    BlockFlags::NOTIFY_LISTENERS | BlockFlags::FORCE_STATE | BlockFlags::SKIP_DROPS
}

fn msg(th: bool, thai: &str, english: &str) -> String {
    format!("&8[&dWorldTools&8]&r {}", if th { thai } else { english })
}

/// The item in the player's hand without `minecraft:`; `None` for an empty hand.
fn held(p: &Player) -> Option<String> {
    let stack = p.get_inventory_item(p.get_selected_slot())?;
    let key = stack.get_registry_key();
    let key = key.trim_start_matches("minecraft:");
    (stack.get_count() > 0 && key != "air").then(|| key.to_owned())
}

fn allowed(p: &Player) -> bool {
    p.has_permission(&format!("{NS}:use"))
}

/// Remember a corner; returns the line for the player.
fn set_corner(st: &mut State, p: &Player, at: P, second: bool) -> String {
    let th = st.cfg.language != "en";
    let dim = p.get_world().get_dimension();
    let s = st.sessions.entry(key(p)).or_default();
    if s.dim != dim {
        (s.a, s.b) = (None, None);
        s.dim = dim;
    }
    if second {
        s.b = Some(at);
    } else {
        s.a = Some(at);
    }
    s.redraw = true;
    st.marks = true;
    let size = match (s.a, s.b) {
        (Some(a), Some(b)) => format!(" &7({})", Region::new(a, b).volume()),
        _ => String::new(),
    };
    let n = if second { 2 } else { 1 };
    msg(th, &format!("&dจุดที่ {n}: &f{} {} {}{size}", at.0, at.1, at.2), &format!("&dPosition {n}: &f{} {} {}{size}", at.0, at.1, at.2))
}

fn selection(st: &State, p: &Player) -> Option<Region> {
    let s = st.sessions.get(&key(p))?;
    (s.dim == p.get_world().get_dimension()).then_some(())?;
    Some(Region::new(s.a?, s.b?))
}

/// Chunk by chunk, bottom up: the server looks the chunk up for every block, and neighbours in
/// the list are then neighbours in memory.
/// One number that sorts like (chunk x, chunk z, y, x, z): 22 bits for each chunk coordinate
/// (the whole world), 12 for y, 4 + 4 for the place inside the chunk.
fn by_chunk(p: &P) -> u64 {
    let chunk = |v: i32| ((v >> 4) + (1 << 21)) as u64 & 0x3f_ffff;
    let (y, x, z) = (p.1.wrapping_add(2048) as u64 & 0xfff, (p.0 & 15) as u64, (p.2 & 15) as u64);
    chunk(p.0) << 42 | chunk(p.2) << 20 | y << 8 | x << 4 | z
}

fn too_large(th: bool, n: i64, max: u32) -> String {
    msg(th, &format!("&cใหญ่เกินไป: {n} บล็อก (สูงสุด {max})"), &format!("&cToo large: {n} blocks (the limit is {max})"))
}

/// Queue work for the tick loop.
fn start(st: &mut State, p: &Player, label: &str, mut todo: Vec<P>, mut work: Work, keep: Keep) -> String {
    let th = st.cfg.language != "en";
    let owner = key(p);
    let quiet = label == "brush";
    if st.jobs.iter().any(|j| j.r.owner == owner) {
        return msg(th, "&cงานก่อนหน้ายังไม่เสร็จ รอสักครู่หรือ //cancel", "&cYour previous edit is still running; wait or //cancel");
    }
    if todo.is_empty() {
        return msg(th, "&eไม่มีบล็อกให้แก้", "&eNothing to change");
    }
    if todo.len() > st.cfg.max_blocks as usize {
        return too_large(th, todo.len() as i64, st.cfg.max_blocks);
    }
    match &mut work {
        Work::States(states) => {
            let mut both: Vec<(P, u16)> = todo.into_iter().zip(std::mem::take(states)).collect();
            both.sort_by_key(|(at, _)| by_chunk(at));
            (todo, *states) = both.into_iter().unzip();
        }
        // A flood starts where the player stands and finds its own way
        Work::Flood(..) => {}
        _ => todo.sort_unstable_by_key(by_chunk),
    }
    // The global mask limits new edits; undo and redo put back exactly what was there
    let reads_only = matches!(work, Work::Count(_) | Work::Distr(_) | Work::Copy(_, false));
    let gmask = if keep == Keep::Undo && !reads_only { st.sessions.get(&owner).and_then(|s| s.gmask.as_ref()).map(|(_, m)| m.clone()) } else { None };
    let n = todo.len();
    let world = p.get_world();
    let seen = if matches!(work, Work::Flood(..)) { todo.iter().copied().collect() } else { HashSet::new() };
    st.jobs.push(Job {
        work,
        r: Run {
            owner,
            name: p.get_name(),
            label: label.to_owned(),
            dim: world.get_dimension(),
            span: height(&world),
            todo,
            at: 0,
            changed: 0,
            skipped: 0,
            found: 0,
            cut_short: false,
            before: Vec::new(),
            clip: Vec::new(),
            keep,
            quiet,
            started: Instant::now(),
            busy: Duration::ZERO,
            masked: HashMap::new(),
            gmask,
            gmasked: HashMap::new(),
            chunk: None,
            seen,
            distr: HashMap::new(),
            batch: Vec::new(),
            report: st.cfg.blocklog && !reads_only,
            told: 0,
            told_messages: 0,
            told_time: Duration::ZERO,
            refused: 0,
            untold: 0,
        },
    });
    if quiet {
        return String::new();
    }
    msg(th, &format!("&7{label}: {n} บล็อก กำลังทำ…"), &format!("&7{label}: working on {n} blocks…"))
}

/// Whether a state is in a mask; one question to the server per kind of block, not per block.
fn hit(memo: &mut HashMap<u16, bool>, cache: &mut Cache, mask: &Mask, state: u16) -> bool {
    *memo.entry(state).or_insert_with(|| mask.matches(cache.facts(state)))
}

impl Run {
    fn loaded(&mut self, world: &World, at: P) -> bool {
        let chunk = (at.0 >> 4, at.2 >> 4);
        match self.chunk {
            Some((c, loaded)) if c == chunk => loaded,
            _ => {
                let loaded = world.get_chunk(chunk.0, chunk.1).is_some();
                self.chunk = Some((chunk, loaded));
                loaded
            }
        }
    }

    /// Write one block, unless it already is that state or the global mask leaves it out.
    fn put(&mut self, world: &World, cache: &mut Cache, at: P, old: u16, want: u16, f: BlockFlags) {
        if want == old {
            return;
        }
        if let Some(mask) = &self.gmask {
            if !hit(&mut self.gmasked, cache, mask, old) {
                return;
            }
        }
        world.set_block_state(pos(at), want, f);
        self.changed += 1;
        self.before.push((at, old));
        if self.report {
            self.batch.push((at, old, want));
        }
    }

    /// Queue a position for a flood, once, while there is room.
    fn spread(&mut self, at: P, max: usize) {
        if at.1 < self.span.0 || at.1 > self.span.1 || self.seen.contains(&at) {
            return;
        }
        if self.todo.len() >= max {
            self.cut_short = true;
            return;
        }
        self.seen.insert(at);
        self.todo.push(at);
    }
}

/// One position of a job.
fn step(job: &mut Job, world: &World, cache: &mut Cache, seed: u64, f: BlockFlags, max: usize) {
    let Job { work, r } = job;
    let at = r.todo[r.at];
    if !r.loaded(world, at) {
        r.skipped += 1;
        return;
    }
    match work {
        Work::Fill(pattern) => {
            let old = world.get_block_state_id(pos(at));
            r.put(world, cache, at, old, pattern.pick(geo::roll(seed, at)), f);
        }
        Work::Replace(mask, pattern) => {
            let old = world.get_block_state_id(pos(at));
            if hit(&mut r.masked, cache, mask, old) {
                r.put(world, cache, at, old, pattern.pick(geo::roll(seed, at)), f);
            }
        }
        Work::States(states) => {
            let old = world.get_block_state_id(pos(at));
            r.put(world, cache, at, old, states[r.at], f);
        }
        Work::Count(mask) => {
            let old = world.get_block_state_id(pos(at));
            if hit(&mut r.masked, cache, mask, old) {
                r.found += 1;
            }
        }
        Work::Distr(_) => {
            *r.distr.entry(world.get_block_state_id(pos(at))).or_default() += 1;
        }
        Work::Copy(origin, cut) => {
            let old = world.get_block_state_id(pos(at));
            r.clip.push(((at.0 - origin.0, at.1 - origin.1, at.2 - origin.2), old));
            if *cut {
                r.put(world, cache, at, old, AIR, f);
            }
        }
        Work::Read(_) => {
            let old = world.get_block_state_id(pos(at));
            r.clip.push((at, old));
        }
        Work::Overlay(pattern, bottom, top) => {
            // The highest block of the column, then the air above it
            for y in (*bottom..=*top).rev() {
                if cache.is_air(world.get_block_state_id(pos((at.0, y, at.2)))) {
                    continue;
                }
                let above = (at.0, y + 1, at.2);
                if above.1 <= r.span.1 {
                    let old = world.get_block_state_id(pos(above));
                    if cache.is_air(old) {
                        r.put(world, cache, above, old, pattern.pick(geo::roll(seed, above)), f);
                    }
                }
                break;
            }
        }
        Work::Naturalize(mask, [grass, dirt, stone], bottom, top) => {
            let mut ground = None;
            for y in (*bottom..=*top).rev() {
                let here = (at.0, y, at.2);
                let old = world.get_block_state_id(pos(here));
                if !hit(&mut r.masked, cache, mask, old) {
                    continue;
                }
                let depth = *ground.get_or_insert(y) - y;
                let want = match depth {
                    0 => *grass,
                    1..=3 => *dirt,
                    _ => *stone,
                };
                r.put(world, cache, here, old, want, f);
            }
        }
        Work::Flood(kind, origin, radius2) => {
            let d = |a: i32, b: i32| i64::from(a) - i64::from(b);
            let (dx, dy, dz) = (d(at.0, origin.0), d(at.1, origin.1), d(at.2, origin.2));
            let old = world.get_block_state_id(pos(at));
            match kind {
                Flood::Drain(waterlogged) => {
                    if dx * dx + dy * dy + dz * dz > *radius2 {
                        return;
                    }
                    let facts = cache.facts(old);
                    let want = if facts.liquid {
                        Some(AIR)
                    } else if *waterlogged && facts.props.iter().any(|(k, v)| k == "waterlogged" && v == "true") {
                        let name = facts.name.clone();
                        let dry: Props = facts.props.iter().map(|(k, v)| (k.clone(), if k == "waterlogged" { "false".to_owned() } else { v.clone() })).collect();
                        cache.table(&name).and_then(|t| spec::find_state(&t.states, &dry))
                    } else {
                        None
                    };
                    let Some(want) = want else { return };
                    r.put(world, cache, at, old, want, f);
                    for step in [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)] {
                        r.spread(geo::add(at, step), max);
                    }
                }
                Flood::Fill { pattern, top, bottom, recursive } => {
                    if dx * dx + dz * dz > *radius2 || at.1 > *top || at.1 < *bottom || !cache.is_air(old) {
                        return;
                    }
                    r.put(world, cache, at, old, pattern.pick(geo::roll(seed, at)), f);
                    r.spread(geo::add(at, (0, -1, 0)), max);
                    if *recursive || at.1 == *top {
                        for step in [(1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1)] {
                            r.spread(geo::add(at, step), max);
                        }
                    }
                    if *recursive {
                        r.spread(geo::add(at, (0, 1, 0)), max);
                    }
                }
            }
        }
    }
}

/// The blocks have been read: turn the job into the writes that follow. `false` = nothing to write.
fn after_read(job: &mut Job, cache: &mut Cache, seed: u64) -> bool {
    let Work::Read(then) = std::mem::replace(&mut job.work, Work::States(Vec::new())) else { return false };
    let r = &mut job.r;
    let read = std::mem::take(&mut r.clip);
    let (lo, hi) = r.span;
    let mut out: Vec<(P, u16)> = Vec::new();
    match then {
        Then::Stack { n, stride, skip_air } => {
            for k in 1..=n {
                let d = geo::times(stride, k);
                out.extend(read.iter().filter(|(_, s)| !(skip_air && *s == AIR)).map(|(p, s)| (geo::add(*p, d), *s)));
            }
        }
        Then::Move { from, delta, fill, skip_air } => {
            let to = from.translated(delta);
            out.extend(read.iter().filter(|(p, _)| !to.contains(*p)).map(|(p, _)| (*p, fill.pick(geo::roll(seed, *p)))));
            out.extend(read.iter().filter(|(_, s)| !(skip_air && *s == AIR)).map(|(p, s)| (geo::add(*p, delta), *s)));
        }
        Then::Hollow { region, thickness, fill } => {
            let (w, h, l) = region.size();
            let (w, h, l) = (w as usize, h as usize, l as usize);
            let index = |p: &P| (p.0 - region.min.0) as usize + (p.2 - region.min.2) as usize * w + (p.1 - region.min.1) as usize * w * l;
            let mut solid = vec![false; w * h * l];
            let mut state = vec![AIR; w * h * l];
            for (p, s) in &read {
                let i = index(p);
                solid[i] = cache.facts(*s).solid;
                state[i] = *s;
            }
            for i in geo::hollow((w, h, l), &solid, thickness) {
                let p = (region.min.0 + (i % w) as i32, region.min.1 + (i / (w * l)) as i32, region.min.2 + (i / w % l) as i32);
                let want = fill.pick(geo::roll(seed, p));
                if state[i] != want {
                    out.push((p, want));
                }
            }
        }
    }
    out.retain(|(p, _)| p.1 >= lo && p.1 <= hi);
    out.sort_by_key(|(at, _)| by_chunk(at));
    let (todo, states): (Vec<P>, Vec<u16>) = out.into_iter().unzip();
    (r.todo, r.at, r.chunk) = (todo, 0, None);
    job.work = Work::States(states);
    !r.todo.is_empty()
}

/// Hand this slice's changes to BlockLog: `CHB1`, name, dimension, count, then per block
/// x, y, z (i32) and the old and new state id (u16), all little endian. One message a slice,
/// at most `BATCH` changes in it. No BlockLog = the call fails and this job stops collecting;
/// a BlockLog that is busy gets the same changes again on the next slice.
fn tell_blocklog(r: &mut Run) {
    if r.batch.is_empty() {
        return;
    }
    let began = Instant::now();
    let n = r.batch.len().min(BATCH);
    let mut m = Vec::with_capacity(12 + r.name.len() + r.dim.len() + n * 16);
    m.extend_from_slice(b"CHB1");
    for s in [&r.name, &r.dim] {
        m.extend_from_slice(&(s.len().min(usize::from(u16::MAX)) as u16).to_le_bytes());
        m.extend_from_slice(&s.as_bytes()[..s.len().min(usize::from(u16::MAX))]);
    }
    m.extend_from_slice(&(n as u32).to_le_bytes());
    for (p, old, new) in &r.batch[..n] {
        m.extend_from_slice(&p.0.to_le_bytes());
        m.extend_from_slice(&p.1.to_le_bytes());
        m.extend_from_slice(&p.2.to_le_bytes());
        m.extend_from_slice(&old.to_le_bytes());
        m.extend_from_slice(&new.to_le_bytes());
    }
    match pumpkin_plugin_api::ipc::send_ipc_message("blocklog", &m) {
        Ok(Ok(_)) => {
            r.told += n as u64;
            r.told_messages += 1;
            r.refused = 0;
            r.batch.drain(..n);
        }
        // Installed but busy: try again, for two seconds at most
        Ok(Err(_)) => {
            r.refused += 1;
            if r.refused >= 40 {
                r.untold += r.batch.len() as u64;
                r.batch.clear();
                r.report = false;
            }
        }
        // Not installed
        Err(()) => {
            r.batch.clear();
            r.report = false;
        }
    }
    r.told_time += began.elapsed();
}

fn player_of(server: &Server, owner: &str) -> Option<Player> {
    let high = u64::from_str_radix(owner.get(..16)?, 16).ok()?;
    let low = u64::from_str_radix(owner.get(16..)?, 16).ok()?;
    server.get_player_by_uuid(pumpkin_plugin_api::uuid::Uuid { high, low })
}

/// Keep at most `count` edits and `blocks` blocks, dropping the oldest.
fn trim(list: &mut Vec<Edit>, count: usize, blocks: usize) {
    while list.len() > count || (list.len() > 1 && list.iter().map(|e| e.blocks.len()).sum::<usize>() > blocks) {
        list.remove(0);
    }
}

/// The lines of //distr: the commonest blocks of what was counted.
fn distr_lines(th: bool, cache: &mut Cache, counts: &HashMap<u16, u64>, states: bool) -> Vec<String> {
    let mut by_block: HashMap<String, u64> = HashMap::new();
    for (state, n) in counts {
        let f = cache.facts(*state);
        let name = if states { spec::state_string(&f.name, &f.props) } else { f.name.clone() };
        *by_block.entry(name.trim_start_matches("minecraft:").to_owned()).or_default() += n;
    }
    let total: u64 = by_block.values().sum();
    let mut rows: Vec<(String, u64)> = by_block.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let kinds = rows.len();
    let mut out = vec![msg(th, &format!("&d{total} บล็อก {kinds} ชนิด:"), &format!("&d{total} blocks of {kinds} kinds:"))];
    for (name, n) in rows.iter().take(12) {
        out.push(format!("&7{:>6.2}%  &f{n}  &d{name}", *n as f64 * 100.0 / total.max(1) as f64));
    }
    if kinds > 12 {
        out.push(format!("&7… +{}", kinds - 12));
    }
    out
}

/// Take back the fake blocks a player sees.
fn unmark(s: &mut Session, p: &Player) {
    for at in s.marks.drain(..) {
        p.reset_block_change(pos(at));
    }
    s.marks_until = None;
}

/// Draw selections that changed and take back marks whose time is up.
fn tend_marks(st: &mut State, server: &Server) {
    let (default_on, secs) = (st.cfg.sel_show, st.cfg.sel_show_secs.clamp(1, 600));
    let block = st.cache.state(&BlockSpec { name: geo::block_name(&st.cfg.sel_block).unwrap_or_default(), props: Props::new() }, false).unwrap_or(1);
    let mut busy = false;
    for (owner, s) in &mut st.sessions {
        let expired = s.marks_until.is_some_and(|t| t <= Instant::now());
        if !s.redraw && !expired {
            busy |= s.marks_until.is_some();
            continue;
        }
        let Some(p) = player_of(server, owner) else {
            (s.marks, s.marks_until, s.redraw) = (Vec::new(), None, false);
            continue;
        };
        unmark(s, &p);
        if s.redraw && s.show.unwrap_or(default_on) && s.dim == p.get_world().get_dimension() {
            if let (Some(a), Some(b)) = (s.a, s.b) {
                s.marks = geo::outline_marks(&Region::new(a, b), 6);
                for at in &s.marks {
                    p.send_block_change(pos(*at), block);
                }
                s.marks_until = Some(Instant::now() + Duration::from_secs(u64::from(secs)));
                busy = true;
            }
        }
        s.redraw = false;
    }
    st.marks = busy;
}

/// Every server tick: move the running jobs forward within the time budget.
fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.ticks += 1;
    if st.marks && st.ticks % 2 == 0 {
        tend_marks(st, &server);
    }
    if st.jobs.is_empty() {
        return;
    }
    let th = st.cfg.language != "en";
    let share = Duration::from_micros(u64::from(st.cfg.tick_ms.clamp(1, 40)) * 1000 / st.jobs.len() as u64);
    let (seed, f, max) = (st.seed, flags(), st.cfg.max_blocks as usize);
    let mut done = Vec::new();
    for (i, job) in st.jobs.iter_mut().enumerate() {
        let Some(world) = world_of(&server, &job.r.dim) else {
            done.push((i, true));
            continue;
        };
        let began = Instant::now();
        // How many positions to work before looking at the clock again: one write costs the
        // server anything from 10 microseconds to a millisecond (a block set in open sky relights
        // the whole column under it), a read a fraction of a microsecond. Start with one and
        // let the time the last ones took decide, so a slice ends within about a block of its share.
        let (mut stride, mut since, mut last) = (1u32, 0u32, began);
        // Writes can turn slow from one block to the next: never more than 16 between two looks
        let most = if matches!(job.work, Work::Count(_) | Work::Distr(_) | Work::Copy(_, false) | Work::Read(_)) { 128 } else { 16 };
        while job.r.at < job.r.todo.len() {
            step(job, &world, &mut st.cache, seed, f, max);
            job.r.at += 1;
            since += 1;
            if job.r.batch.len() >= BATCH {
                break;
            }
            if since >= stride {
                let now = Instant::now();
                let spent = now - began;
                if spent >= share {
                    break;
                }
                let each = ((now - last).as_nanos() / u128::from(since)).max(1);
                stride = ((share - spent).as_nanos() / each / 2).clamp(1, most) as u32;
                (since, last) = (0, now);
            }
        }
        tell_blocklog(&mut job.r);
        let finished = job.r.at >= job.r.todo.len() && !(matches!(job.work, Work::Read(_)) && after_read(job, &mut st.cache, seed));
        job.r.busy += began.elapsed();
        if finished {
            done.push((i, false));
        }
    }
    for (i, failed) in done.into_iter().rev() {
        let Job { work, mut r } = st.jobs.remove(i);
        // What a busy BlockLog has not taken yet: once more, then it counts as not told
        tell_blocklog(&mut r);
        r.untold += r.batch.len() as u64;
        let secs = r.started.elapsed().as_secs_f32();
        let mut notes = String::new();
        if r.skipped > 0 {
            notes += &if th { format!(" &e(ข้าม {} บล็อกใน chunk ที่ยังไม่โหลด เข้าไปใกล้แล้วทำใหม่)", r.skipped) } else { format!(" &e({} blocks skipped in chunks that are not loaded; go closer and repeat)", r.skipped) };
        }
        if r.cut_short {
            notes += &if th { format!(" &e(หยุดที่เพดาน {max} บล็อก)") } else { format!(" &e(stopped at the limit of {max} blocks)") };
        }
        let mut lines = vec![match (failed, &work) {
            (true, _) => msg(th, &format!("&cหยุดงาน {}: ไม่พบโลกนั้นแล้ว (แก้ไปแล้ว {} บล็อก)", r.label, r.changed), &format!("&c{} stopped: that world is gone ({} blocks changed)", r.label, r.changed)),
            (false, Work::Count(_)) => msg(th, &format!("&dพบ {} บล็อก{notes}", r.found), &format!("&d{} blocks found{notes}", r.found)),
            (false, Work::Distr(_)) => String::new(),
            (false, Work::Copy(_, false)) => msg(th, &format!("&dคัดลอก {} บล็อก ยืนตรงจุดที่ต้องการแล้ว //paste{notes}", r.clip.len()), &format!("&d{} blocks copied; stand where you want them and //paste{notes}", r.clip.len())),
            (false, _) => msg(th, &format!("&a{}: แก้ {} บล็อก ใน {secs:.1} วินาที{notes}", r.label, r.changed), &format!("&a{}: {} blocks changed in {secs:.1}s{notes}", r.label, r.changed)),
        }];
        if let Work::Distr(states) = work {
            lines = distr_lines(th, &mut st.cache, &r.distr, states);
            if let Some(first) = lines.first_mut() {
                *first += &notes;
            }
        }
        if !r.quiet || r.told > 0 || r.untold > 0 {
            let mut blocklog = if r.told > 0 { format!(", blocklog: {} blocks in {} messages, {} us", r.told, r.told_messages, r.told_time.as_micros()) } else { String::new() };
            if r.untold > 0 {
                blocklog += &format!(", {} changes blocklog did not take", r.untold);
            }
            c::log_info(&format!("{}: {} of {} blocks changed, {} skipped, {:.1}s, {} ms of server time{blocklog}", r.label, r.changed, r.todo.len(), r.skipped, secs, r.busy.as_millis()));
        }
        // The history and clipboard belong to someone who is still here
        let Some(p) = player_of(&server, &r.owner) else { continue };
        let (count, blocks) = (st.cfg.history.clamp(1, 100), st.cfg.history_blocks.max(1));
        let s = st.sessions.entry(r.owner.clone()).or_default();
        if let Work::Copy(origin, _) = work {
            if !r.clip.is_empty() {
                s.clip = Some(Clip { blocks: r.clip, origin, dim: r.dim.clone() });
            }
        }
        if !r.before.is_empty() {
            let edit = Edit { dim: r.dim.clone(), blocks: r.before };
            match r.keep {
                Keep::Redo => s.redo.push(edit),
                Keep::Undo => {
                    s.redo.clear();
                    s.undo.push(edit);
                }
                Keep::UndoKeepRedo => s.undo.push(edit),
            }
            trim(&mut s.undo, count, blocks);
            trim(&mut s.redo, count, blocks);
        }
        if r.quiet {
            c::action_bar(&p, &if th { format!("&dbrush: {} บล็อก{notes}", r.changed) } else { format!("&dbrush: {} blocks{notes}", r.changed) });
            continue;
        }
        for line in lines.iter().filter(|l| !l.is_empty()) {
            c::tell(&p, line);
        }
    }
}

/// Turn every block of a clipboard and the properties that point somewhere. Returns how many
/// blocks kept their state because the server has no state with the turned properties.
fn transform(cache: &mut Cache, clip: &mut Clip, xf: Xf) -> usize {
    // New state per old state: found once per kind of block
    let mut new_state: HashMap<u16, Option<u16>> = HashMap::new();
    let mut kept = 0;
    let mut last: Option<(u16, Option<u16>)> = None;
    for (p, state) in &mut clip.blocks {
        *p = xf.point(*p);
        let to = match last {
            Some((s, to)) if s == *state => to,
            _ => {
                let to = *new_state.entry(*state).or_insert_with(|| {
                    let facts = cache.facts(*state);
                    if facts.props.is_empty() {
                        return Some(*state);
                    }
                    let (name, props) = (facts.name.clone(), facts.props.clone());
                    let turned = xf.props(&props);
                    if turned == props {
                        return Some(*state);
                    }
                    cache.table(&name).and_then(|t| spec::find_state(&t.states, &turned))
                });
                last = Some((*state, to));
                to
            }
        };
        match to {
            Some(s) => *state = s,
            None => kept += 1,
        }
    }
    kept
}

fn schem_dir() -> std::path::PathBuf {
    c::data_path("schematics")
}

/// Schematic files: (name, extension, bytes), by name.
fn schem_files() -> Vec<(String, String, u64)> {
    let mut out: Vec<(String, String, u64)> = std::fs::read_dir(schem_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let ext = path.extension()?.to_str()?.to_owned();
            let name = schem::valid_name(path.file_stem()?.to_str()?)?;
            (ext == EXT || ext == SPONGE_EXT).then(|| (name, ext, e.metadata().map_or(0, |m| m.len())))
        })
        .collect();
    out.sort();
    out
}

fn schem_list(th: bool) -> String {
    let files = schem_files();
    if files.is_empty() {
        return msg(th, "&eยังไม่มี schematic (//schem save <ชื่อ> หลัง //copy)", "&eNo schematics yet (//schem save <name> after //copy)");
    }
    let names: Vec<String> = files.iter().take(40).map(|(n, ext, size)| format!("&f{n}&7{} {}KB", if ext == SPONGE_EXT { ".schem" } else { "" }, size.div_ceil(1024))).collect();
    let more = if files.len() > 40 { format!(" &7… +{}", files.len() - 40) } else { String::new() };
    msg(th, &format!("&dschematic {} ไฟล์: {}{more}", files.len(), names.join("&7, ")), &format!("&d{} schematics: {}{more}", files.len(), names.join("&7, ")))
}

fn schem_delete(th: bool, raw: Option<&str>) -> String {
    let Some(name) = raw.and_then(schem::valid_name) else {
        return msg(th, "&cชื่อใช้ได้เฉพาะ a-z 0-9 _ - ไม่เกิน 32 ตัว", "&cNames are a-z 0-9 _ - and at most 32 characters");
    };
    let mut removed = false;
    for ext in [EXT, SPONGE_EXT] {
        removed |= std::fs::remove_file(schem_dir().join(format!("{name}.{ext}"))).is_ok();
    }
    if removed { msg(th, &format!("&dลบ schematic {name} แล้ว"), &format!("&dSchematic {name} deleted")) } else { msg(th, &format!("&cไม่มี schematic ชื่อ {name}"), &format!("&cNo schematic named {name}")) }
}

/// //schem save: the clipboard as a file of state strings.
fn schem_save(st: &mut State, p: &Player, name: &str) -> String {
    let th = st.cfg.language != "en";
    let (max_bytes, max_files, max_cells) = (u64::from(st.cfg.schem_max_kb) * 1024, st.cfg.schem_max_files as usize, u64::from(st.cfg.max_blocks));
    let began = Instant::now();
    let Some(clip) = st.sessions.get(&key(p)).and_then(|s| s.clip.as_ref()) else {
        return msg(th, "&cยังไม่ได้คัดลอกอะไร (//copy)", "&cNothing copied yet (//copy)");
    };
    let mut palette: Vec<String> = Vec::new();
    let mut index: HashMap<u16, u32> = HashMap::new();
    let mut blocks = Vec::with_capacity(clip.blocks.len());
    for (at, state) in &clip.blocks {
        let i = *index.entry(*state).or_insert_with(|| {
            let f = st.cache.facts(*state);
            palette.push(spec::state_string(&f.name, &f.props));
            palette.len() as u32 - 1
        });
        blocks.push((*at, i));
    }
    let kinds = palette.len();
    let Some(file) = Schematic::from_blocks(&blocks, palette, max_cells) else {
        return msg(th, "&cคลิปบอร์ดว่างหรือใหญ่เกินไป", "&cThe clipboard is empty or too large");
    };
    let bytes = file.encode();
    if bytes.len() as u64 > max_bytes {
        return msg(th, &format!("&cไฟล์ใหญ่เกินไป: {} KB (สูงสุด {} KB)", bytes.len().div_ceil(1024), max_bytes / 1024), &format!("&cThe file is too large: {} KB (the limit is {} KB)", bytes.len().div_ceil(1024), max_bytes / 1024));
    }
    // The cap is on what players save; `.schem` files are put there by the owner
    let files: Vec<_> = schem_files().into_iter().filter(|(_, ext, _)| ext == EXT).collect();
    if files.len() >= max_files && !files.iter().any(|(n, _, _)| n == name) {
        return msg(th, &format!("&cมี schematic ครบ {max_files} ไฟล์แล้ว ลบของเก่าก่อน (//schem delete <ชื่อ>)"), &format!("&cThere are already {max_files} schematics; delete one first (//schem delete <name>)"));
    }
    let dir = schem_dir();
    let (tmp, path) = (dir.join(format!("{name}.tmp")), dir.join(format!("{name}.{EXT}")));
    let written = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&tmp, &bytes)).and_then(|()| std::fs::rename(&tmp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return msg(th, "&cเขียนไฟล์ไม่ได้", "&cCould not write the file");
    }
    c::log_info(&format!("{} saved schematic {name}: {} blocks, {kinds} kinds, {} bytes, {} us", p.get_name(), blocks.len(), bytes.len(), began.elapsed().as_micros()));
    msg(th, &format!("&dบันทึก schematic {name}: {} บล็อก {kinds} ชนิด {} KB", blocks.len(), bytes.len().div_ceil(1024)), &format!("&dSchematic {name} saved: {} blocks, {kinds} kinds, {} KB", blocks.len(), bytes.len().div_ceil(1024)))
}

/// //schem load: a file onto the clipboard. Our own format first, then a Sponge `.schem`.
fn schem_load(st: &mut State, p: &Player, name: &str) -> String {
    let th = st.cfg.language != "en";
    let (max_bytes, max_cells) = (u64::from(st.cfg.schem_max_kb) * 1024, u64::from(st.cfg.max_blocks));
    let began = Instant::now();
    let dir = schem_dir();
    let Some((path, sponge)) = [(EXT, false), (SPONGE_EXT, true)].into_iter().map(|(ext, sponge)| (dir.join(format!("{name}.{ext}")), sponge)).find(|(path, _)| path.is_file()) else {
        return msg(th, &format!("&cไม่มี schematic ชื่อ {name} (//schem list)"), &format!("&cNo schematic named {name} (//schem list)"));
    };
    let size = std::fs::metadata(&path).map_or(u64::MAX, |m| m.len());
    if size > max_bytes {
        return msg(th, &format!("&cไฟล์ใหญ่เกินไป: {} KB (สูงสุด {} KB)", size.div_ceil(1024), max_bytes / 1024), &format!("&cThe file is too large: {} KB (the limit is {} KB)", size.div_ceil(1024), max_bytes / 1024));
    }
    let parsed = std::fs::read(&path).map_err(|_| "cannot read the file").and_then(|bytes| if sponge { schem::sponge(&bytes, max_cells) } else { Schematic::decode(&bytes, max_cells) });
    let file = match parsed {
        Ok(f) => f,
        Err(e) => return msg(th, &format!("&cอ่าน schematic {name} ไม่ได้: {e}"), &format!("&cCannot read schematic {name}: {e}")),
    };
    // State strings -> the ids of this server; a block it does not have is left out, a property
    // it does not have falls back to the block's default
    let (mut unknown, mut changed) = (0u64, 0u64);
    let states: Vec<Option<(u16, bool)>> = file
        .palette
        .iter()
        .map(|s| {
            let spec = BlockSpec::parse(s)?;
            if spec.props.is_empty() {
                return host::get_block_by_name(&spec.name).map(|b| (b.default_state_id, false));
            }
            let table = st.cache.table(&spec.name)?;
            Some(spec::pick_state_lenient(&table.states, table.default, &spec.props))
        })
        .collect();
    let mut blocks = Vec::with_capacity(file.cells.len());
    for (at, i) in file.blocks() {
        match states.get(i as usize).copied().flatten() {
            Some((state, lossy)) => {
                changed += u64::from(lossy);
                blocks.push((at, state));
            }
            None => unknown += 1,
        }
    }
    if blocks.is_empty() {
        return msg(th, &format!("&cschematic {name} ไม่มีบล็อกที่เซิร์ฟนี้รู้จัก"), &format!("&cSchematic {name} has no block this server knows"));
    }
    let n = blocks.len();
    let (w, h, l) = file.size;
    let dim = p.get_world().get_dimension();
    st.sessions.entry(key(p)).or_default().clip = Some(Clip { blocks, origin: feet(p), dim });
    c::log_info(&format!("{} loaded schematic {name}{}: {n} blocks, {} us", p.get_name(), if sponge { ".schem" } else { "" }, began.elapsed().as_micros()));
    let mut notes = String::new();
    if unknown > 0 {
        notes += &if th { format!(" &e(ข้าม {unknown} บล็อกที่เซิร์ฟนี้ไม่รู้จัก)") } else { format!(" &e({unknown} blocks this server does not know were left out)") };
    }
    if changed > 0 {
        notes += &if th { format!(" &e({changed} บล็อกใช้ค่า property ตั้งต้นแทนค่าที่ไม่มี)") } else { format!(" &e({changed} blocks use a default for a property that does not exist here)") };
    }
    if sponge {
        notes += if th { " &7(ไฟล์ .schem: ไม่อ่านของในกล่อง ป้าย มอนสเตอร์ และ biome)" } else { " &7(.schem file: chest contents, signs, entities and biomes are not read)" };
    }
    msg(th, &format!("&dโหลด schematic {name} ขึ้นคลิปบอร์ด: {n} บล็อก ({w} x {h} x {l}) ยืนตรงจุดที่ต้องการแล้ว //paste{notes}"), &format!("&dSchematic {name} is on the clipboard: {n} blocks ({w} x {h} x {l}); stand where you want it and //paste{notes}"))
}

const HELP: &str = "&f//wand //pos1 //pos2 //hpos1 //hpos2 //chunk //desel //size //expand //contract //shift //inset //outset //sel show|hide &7· \
&f//set //replace //walls //faces //outline //center //line //overlay //hollow //naturalize //stack //move //drain //fill //fillr //replacenear //removenear &7· \
&f//cyl //hcyl //sphere //hsphere //pyramid //hpyramid &7· &f//copy //cut //paste [-a] [-o] //rotate //flip //clearclipboard //schem save|load|list|delete &7· \
&f//brush sphere|cylinder|none &7· &f//gmask //count //distr //undo //redo //cancel";

fn run(sub: &str, st: &mut State, p: &Player, w: &[String]) -> String {
    let th = st.cfg.language != "en";
    // Flags (`-a`) can stand anywhere; the rest are the arguments in order
    let flag = |f: &str| w.iter().any(|a| a == f);
    let args: Vec<&str> = w.iter().map(String::as_str).filter(|a| !(a.len() == 2 && a.starts_with('-') && a.as_bytes()[1].is_ascii_alphabetic())).collect();
    let arg = |i: usize| args.get(i).copied();
    let world = p.get_world();
    let dim = world.get_dimension();
    let (lo, hi) = height(&world);
    let max = st.cfg.max_blocks;
    let need_sel = msg(th, "&cเลือกพื้นที่ก่อน: //wand แล้วคลิกซ้าย/ขวา หรือ //pos1 //pos2", "&cSelect a region first: //wand then left/right click, or //pos1 //pos2");
    let usage = |u: &str| msg(th, &format!("&cวิธีใช้: {u}"), &format!("&cUsage: {u}"));
    let number = |i: usize, max: i32| arg(i).and_then(|s| s.parse::<i32>().ok()).filter(|n| (1..=max).contains(n));
    let dir_at = |i: usize| match arg(i) {
        Some(d) => Dir::parse(d),
        None => Some(Dir::facing(p.get_yaw(), p.get_pitch())),
    };
    let in_world = |blocks: Vec<P>| -> Vec<P> { blocks.into_iter().filter(|b| b.1 >= lo && b.1 <= hi).collect() };
    macro_rules! ok {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(line) => return line,
            }
        };
    }
    // The selection inside the world, refused when it is larger than `max-blocks`
    macro_rules! region {
        () => {{
            let Some(r) = selection(st, p).and_then(|r| r.clamped_y(lo, hi)) else { return need_sel };
            if r.volume() > i64::from(max) {
                return too_large(th, r.volume(), max);
            }
            r
        }};
    }

    match sub {
        "wand" => {
            let item = geo::block_name(&st.cfg.wand).unwrap_or_else(|| "minecraft:wooden_axe".into());
            let Some(slot) = (0u8..36).find(|s| p.get_inventory_item(*s).is_none_or(|i| i.get_count() == 0 || i.get_registry_key().ends_with("air"))) else {
                return msg(th, "&cกระเป๋าเต็ม", "&cYour inventory is full");
            };
            p.set_inventory_item(slot, Some(ItemStack::new(&item, 1)));
            msg(th, "&dคลิกซ้าย = จุดที่ 1, คลิกขวา = จุดที่ 2", "&dLeft click = position 1, right click = position 2")
        }
        "pos1" | "pos2" => {
            // `//pos1` = where the player stands; `//pos1 x y z` or `//pos1 x,y,z` = that block
            let given: Vec<&str> = args.iter().flat_map(|a| a.split(',')).collect();
            let at = match given.as_slice() {
                [] => feet(p),
                [x, y, z] => match (x.parse::<i32>(), y.parse::<i32>(), z.parse::<i32>()) {
                    (Ok(x), Ok(y), Ok(z)) if x.abs() <= 30_000_000 && z.abs() <= 30_000_000 => (x, y, z),
                    _ => return usage(&format!("//{sub} [x y z]")),
                },
                _ => return usage(&format!("//{sub} [x y z]")),
            };
            set_corner(st, p, at, sub == "pos2")
        }
        "hpos1" | "hpos2" => match p.get_target_block(120) {
            Some((x, y, z)) => set_corner(st, p, (x.floor() as i32, y.floor() as i32, z.floor() as i32), sub == "hpos2"),
            None => msg(th, "&cไม่มีบล็อกในระยะที่มองอยู่", "&cNo block in sight"),
        },
        "chunk" => {
            let at = feet(p);
            let (x, z) = ((at.0 >> 4) << 4, (at.2 >> 4) << 4);
            set_corner(st, p, (x, lo, z), false);
            set_corner(st, p, (x + 15, hi, z + 15), true);
            msg(th, &format!("&dเลือก chunk {} {} ({} บล็อก)", x >> 4, z >> 4, 256 * (i64::from(hi) - i64::from(lo) + 1)), &format!("&dChunk {} {} selected ({} blocks)", x >> 4, z >> 4, 256 * (i64::from(hi) - i64::from(lo) + 1)))
        }
        "desel" => {
            if let Some(s) = st.sessions.get_mut(&key(p)) {
                (s.a, s.b) = (None, None);
                unmark(s, p);
            }
            msg(th, "&dล้างพื้นที่ที่เลือกแล้ว", "&dSelection cleared")
        }
        "size" => match selection(st, p) {
            Some(r) => {
                let (x, y, z) = r.size();
                msg(th, &format!("&d{x} x {y} x {z} = {} บล็อก &7({} {} {} ถึง {} {} {})", r.volume(), r.min.0, r.min.1, r.min.2, r.max.0, r.max.1, r.max.2), &format!("&d{x} x {y} x {z} = {} blocks &7({} {} {} to {} {} {})", r.volume(), r.min.0, r.min.1, r.min.2, r.max.0, r.max.1, r.max.2))
            }
            None => need_sel,
        },
        "expand" | "contract" | "shift" | "inset" | "outset" => {
            let Some(r) = selection(st, p) else { return need_sel };
            let r2 = if sub == "inset" || sub == "outset" {
                let Some(n) = number(0, 10_000) else { return usage(&format!("//{sub} <จำนวน> [-h|-v]")) };
                // -h = sideways only, -v = up and down only
                r.outset(if sub == "inset" { -n } else { n }, !flag("-v"), !flag("-h"))
            } else {
                let (Some(n), Some(d)) = (number(0, 10_000), dir_at(1)) else { return usage(&format!("//{sub} <จำนวน> [n|s|e|w|u|d]")) };
                match sub {
                    "expand" => r.grown(d, n),
                    "contract" => r.grown(d, -n),
                    _ => r.shifted(d, n),
                }
            };
            if let Some(s) = st.sessions.get_mut(&key(p)) {
                (s.a, s.b, s.redraw) = (Some(r2.min), Some(r2.max), true);
                st.marks = true;
            }
            let (x, y, z) = r2.size();
            msg(th, &format!("&dพื้นที่ใหม่ {x} x {y} x {z} = {} บล็อก", r2.volume()), &format!("&dRegion is now {x} x {y} x {z} = {} blocks", r2.volume()))
        }
        "sel" => {
            let s = st.sessions.entry(key(p)).or_default();
            match arg(0) {
                Some("show" | "on") => s.show = Some(true),
                Some("hide" | "off") => {
                    s.show = Some(false);
                    unmark(s, p);
                    return msg(th, "&dซ่อนขอบพื้นที่ที่เลือก", "&dThe selection outline is hidden");
                }
                // `//sel` alone: once more, with whatever setting applies
                None if s.show.unwrap_or(st.cfg.sel_show) => {}
                _ => return usage("//sel show|hide"),
            }
            s.redraw = true;
            st.marks = true;
            let secs = st.cfg.sel_show_secs.clamp(1, 600);
            msg(th, &format!("&dแสดงมุมและขอบของพื้นที่ที่เลือก {secs} วินาทีทุกครั้งที่เปลี่ยน (เห็นเฉพาะคุณ บล็อกจริงไม่เปลี่ยน)"), &format!("&dThe corners and edges of the selection show for {secs}s whenever it changes (only you see them; no real block changes)"))
        }
        "gmask" => {
            let k = key(p);
            match arg(0) {
                None => match st.sessions.get(&k).and_then(|s| s.gmask.as_ref()) {
                    Some((raw, _)) => msg(th, &format!("&dgmask ตอนนี้: &f{raw} &7(//gmask off เพื่อเลิก)"), &format!("&dThe global mask is &f{raw} &7(//gmask off clears it)")),
                    None => msg(th, "&eยังไม่ได้ตั้ง gmask · //gmask <mask|off>", "&eNo global mask · //gmask <mask|off>"),
                },
                Some("off" | "none" | "clear") => {
                    if let Some(s) = st.sessions.get_mut(&k) {
                        s.gmask = None;
                    }
                    msg(th, "&dเลิกใช้ gmask แล้ว", "&dGlobal mask cleared")
                }
                Some(raw) => {
                    let mask = ok!(st.cache.mask(raw, th));
                    st.sessions.entry(k).or_default().gmask = Some((raw.to_owned(), mask));
                    msg(th, &format!("&dgmask: &f{raw} &7(การแก้ทุกอย่างของคุณจะแตะเฉพาะบล็อกที่ตรง จนกว่าจะ //gmask off)"), &format!("&dGlobal mask: &f{raw} &7(every edit of yours only touches matching blocks until //gmask off)"))
                }
            }
        }
        "count" => {
            let r = region!();
            let Some(raw) = arg(0) else { return usage("//count <mask>") };
            let mask = ok!(st.cache.mask(raw, th));
            start(st, p, "count", r.chunk_order().collect(), Work::Count(mask), Keep::Undo)
        }
        "distr" => {
            let r = region!();
            // -d: every state on its own line (stairs by facing, logs by axis)
            start(st, p, "distr", r.chunk_order().collect(), Work::Distr(flag("-d")), Keep::Undo)
        }
        "set" | "walls" | "faces" | "outline" | "replace" | "center" => {
            let r = region!();
            let (from, raw) = match (sub, arg(0), arg(1)) {
                ("replace", Some(mask), Some(pattern)) => (Some(ok!(st.cache.mask(mask, th))), pattern),
                // `//replace <pattern>` = everything that is not air
                ("replace", Some(pattern), None) => (Some(ok!(st.cache.mask("#existing", th))), pattern),
                ("replace", ..) => return usage("//replace [mask] <บล็อก>"),
                (_, Some(pattern), _) => (None, pattern),
                _ => return usage(&format!("//{sub} <บล็อก>")),
            };
            let pattern = ok!(st.cache.pattern(raw, th));
            let todo: Vec<P> = match sub {
                "walls" => r.walls().collect(),
                "faces" | "outline" => r.faces().collect(),
                "center" => r.center().blocks().collect(),
                _ => r.chunk_order().collect(),
            };
            let work = match from {
                Some(f) => Work::Replace(f, pattern),
                None => Work::Fill(pattern),
            };
            start(st, p, sub, todo, work, Keep::Undo)
        }
        "replacenear" | "removenear" => {
            // A cube around the player
            let (radius, mask, pattern) = if sub == "removenear" {
                let Some(mask) = arg(0) else { return usage("//removenear <mask> [รัศมี]") };
                (number(1, 64).unwrap_or(5), mask, "air")
            } else {
                let (Some(radius), Some(mask), Some(pattern)) = (number(0, 64), arg(1), arg(2)) else { return usage("//replacenear <รัศมี> <mask> <บล็อก>") };
                (radius, mask, pattern)
            };
            let at = feet(p);
            let Some(r) = Region::new((at.0 - radius, at.1 - radius, at.2 - radius), (at.0 + radius, at.1 + radius, at.2 + radius)).clamped_y(lo, hi) else { return msg(th, "&eไม่มีบล็อกให้แก้", "&eNothing to change") };
            if r.volume() > i64::from(max) {
                return too_large(th, r.volume(), max);
            }
            let (mask, pattern) = (ok!(st.cache.mask(mask, th)), ok!(st.cache.pattern(pattern, th)));
            start(st, p, sub, r.chunk_order().collect(), Work::Replace(mask, pattern), Keep::Undo)
        }
        "line" => {
            let Some((a, b)) = st.sessions.get(&key(p)).filter(|s| s.dim == dim).and_then(|s| Some((s.a?, s.b?))) else { return need_sel };
            let Some(raw) = arg(0) else { return usage("//line <บล็อก>") };
            let pattern = ok!(st.cache.pattern(raw, th));
            if geo::line_len(a, b) > i64::from(max) {
                return too_large(th, geo::line_len(a, b), max);
            }
            start(st, p, sub, in_world(geo::line(a, b)), Work::Fill(pattern), Keep::Undo)
        }
        "overlay" | "naturalize" => {
            let r = region!();
            // One job position per column; a column reads up to the height of the region
            let columns: Vec<P> = (r.min.0..=r.max.0).flat_map(|x| (r.min.2..=r.max.2).map(move |z| (x, r.max.1, z))).collect();
            let work = if sub == "overlay" {
                let Some(raw) = arg(0) else { return usage("//overlay <บล็อก>") };
                // Like WorldEdit, the ground may lie one block under the selection
                Work::Overlay(ok!(st.cache.pattern(raw, th)), (r.min.1 - 1).max(lo), r.max.1)
            } else {
                let mut ids = [AIR; 3];
                for (id, name) in ids.iter_mut().zip(["grass_block", "dirt", "stone"]) {
                    *id = ok!(st.cache.state(&BlockSpec { name: format!("minecraft:{name}"), props: Props::new() }, th));
                }
                Work::Naturalize(ok!(st.cache.mask("grass_block,dirt,stone", th)), ids, r.min.1, r.max.1)
            };
            start(st, p, sub, columns, work, Keep::Undo)
        }
        "hollow" => {
            let r = region!();
            // `//hollow [thickness] [pattern]`
            let thickness = match arg(0) {
                Some(_) => match number(0, 64) {
                    Some(n) => n as usize,
                    None => return usage("//hollow [ความหนา] [บล็อก]"),
                },
                None => 1,
            };
            let fill = ok!(st.cache.pattern(arg(1).unwrap_or("air"), th));
            start(st, p, sub, r.chunk_order().collect(), Work::Read(Then::Hollow { region: r, thickness, fill }), Keep::Undo)
        }
        "stack" | "move" => {
            let r = region!();
            // `//stack [n] [direction]`, `//move [n] [direction] [fill pattern]`; -a leaves air out
            let n = match arg(0) {
                Some(_) => match number(0, 10_000) {
                    Some(n) => n,
                    None => return usage(&format!("//{sub} [จำนวน] [n|s|e|w|u|d]{}", if sub == "move" { " [บล็อกที่เติมแทน]" } else { "" })),
                },
                None => 1,
            };
            let Some(d) = dir_at(1) else { return usage(&format!("//{sub} [จำนวน] [n|s|e|w|u|d]")) };
            let then = if sub == "stack" {
                if r.volume().saturating_mul(i64::from(n)) > i64::from(max) {
                    return too_large(th, r.volume().saturating_mul(i64::from(n)), max);
                }
                Then::Stack { n, stride: r.stride(d), skip_air: flag("-a") }
            } else {
                let fill = ok!(st.cache.pattern(arg(2).unwrap_or("air"), th));
                let delta = geo::times(d.step(), n);
                // -s: the selection goes along
                if flag("-s") {
                    if let Some(s) = st.sessions.get_mut(&key(p)) {
                        let moved = Region::new(s.a.unwrap_or(r.min), s.b.unwrap_or(r.max)).translated(delta);
                        (s.a, s.b, s.redraw) = (Some(moved.min), Some(moved.max), true);
                        st.marks = true;
                    }
                }
                Then::Move { from: r, delta, fill, skip_air: flag("-a") }
            };
            start(st, p, sub, r.chunk_order().collect(), Work::Read(then), Keep::Undo)
        }
        "drain" | "fill" | "fillr" => {
            let at = feet(p);
            let (work, seeds) = if sub == "drain" {
                let Some(radius) = number(0, 256) else { return usage("//drain <รัศมี> [-w]") };
                // Like WorldEdit: the pool has to touch the block the player stands in or one next to it
                let seeds: Vec<P> = Region::new((at.0 - 1, at.1 - 1, at.2 - 1), (at.0 + 1, at.1 + 1, at.2 + 1)).blocks().collect();
                (Work::Flood(Flood::Drain(flag("-w")), at, i64::from(radius) * i64::from(radius)), seeds)
            } else {
                let u = format!("//{sub} <บล็อก> <รัศมี> [ลึก]");
                let (Some(raw), Some(radius)) = (arg(0), number(1, 256)) else { return usage(&u) };
                let depth = match (arg(2), sub) {
                    (Some(_), _) => match number(2, 4096) {
                        Some(d) => d,
                        None => return usage(&u),
                    },
                    (None, "fill") => 1,
                    (None, _) => 4096,
                };
                let pattern = ok!(st.cache.pattern(raw, th));
                (Work::Flood(Flood::Fill { pattern, top: at.1, bottom: (at.1 - depth + 1).max(lo), recursive: sub == "fillr" }, at, i64::from(radius) * i64::from(radius)), vec![at])
            };
            start(st, p, sub, in_world(seeds), work, Keep::Undo)
        }
        "cyl" | "hcyl" | "sphere" | "hsphere" | "pyramid" | "hpyramid" => {
            let (ball, pyramid) = (sub.ends_with("sphere"), sub.ends_with("pyramid"));
            let u = if ball { format!("//{sub} <บล็อก> <รัศมี>") } else if pyramid { format!("//{sub} <บล็อก> <ขนาด>") } else { format!("//{sub} <บล็อก> <รัศมี> [สูง]") };
            let Some(raw) = arg(0) else { return usage(&u) };
            let pattern = ok!(st.cache.pattern(raw, th));
            let Some(radius) = number(1, if ball || pyramid { 128 } else { 256 }) else { return usage(&u) };
            let hollow = sub.starts_with('h');
            let tall = number(2, 384).unwrap_or(1);
            // Refuse the clearly too large before building the list of positions
            let r = f64::from(radius);
            let about = if ball { 4.19 * r * r * r } else if pyramid { 1.33 * r * r * r } else { 3.14 * r * r * f64::from(tall) };
            if !hollow && about * 0.9 > f64::from(max) {
                return too_large(th, about as i64, max);
            }
            let at = feet(p);
            let todo = if ball { geo::sphere(at, radius, hollow) } else if pyramid { geo::pyramid(at, radius, hollow) } else { geo::cylinder(at, radius, tall, hollow) };
            start(st, p, sub, in_world(todo), Work::Fill(pattern), Keep::Undo)
        }
        "copy" | "cut" => {
            let r = region!();
            start(st, p, sub, r.chunk_order().collect(), Work::Copy(feet(p), sub == "cut"), Keep::Undo)
        }
        "paste" => {
            let (skip_air, original) = (flag("-a"), flag("-o"));
            let Some(clip) = st.sessions.get(&key(p)).and_then(|s| s.clip.as_ref()) else {
                return msg(th, "&cยังไม่ได้คัดลอกอะไร (//copy)", "&cNothing copied yet (//copy)");
            };
            if original && clip.dim != dim {
                return msg(th, "&cคัดลอกมาจากอีกโลกหนึ่ง ใช้ //paste แบบไม่มี -o", "&cThat was copied in another world; use //paste without -o");
            }
            let o = if original { clip.origin } else { feet(p) };
            let (mut todo, mut states) = (Vec::new(), Vec::new());
            for (rel, state) in &clip.blocks {
                let at = geo::add(o, *rel);
                if at.1 >= lo && at.1 <= hi && !(skip_air && *state == AIR) {
                    todo.push(at);
                    states.push(*state);
                }
            }
            start(st, p, "paste", todo, Work::States(states), Keep::Undo)
        }
        "rotate" | "flip" => {
            let xf = if sub == "rotate" {
                let Some(xf) = arg(0).and_then(|a| a.parse::<i32>().ok()).and_then(Xf::turn) else { return usage("//rotate <90|180|270>") };
                xf
            } else {
                let Some(d) = dir_at(0) else { return usage("//flip [n|s|e|w|u|d]") };
                Xf::Flip(match d {
                    Dir::East | Dir::West => Axis::X,
                    Dir::Up | Dir::Down => Axis::Y,
                    Dir::North | Dir::South => Axis::Z,
                })
            };
            let State { sessions, cache, .. } = st;
            let Some(clip) = sessions.get_mut(&key(p)).and_then(|s| s.clip.as_mut()) else {
                return msg(th, "&cยังไม่ได้คัดลอกอะไร (//copy)", "&cNothing copied yet (//copy)");
            };
            let kept = transform(cache, clip, xf);
            let n = clip.blocks.len();
            let note = if kept == 0 {
                String::new()
            } else if th {
                format!(" &e({kept} บล็อกหมุนทิศไม่ได้ คงทิศเดิม)")
            } else {
                format!(" &e({kept} blocks could not be turned and keep their direction)")
            };
            let what = match xf {
                Xf::Turn(q) => format!("{}°", u32::from(q) * 90),
                Xf::Flip(Axis::X) => "east-west".to_owned(),
                Xf::Flip(Axis::Y) => "up-down".to_owned(),
                Xf::Flip(Axis::Z) => "north-south".to_owned(),
            };
            if sub == "rotate" { msg(th, &format!("&dหมุนคลิปบอร์ด {what} ({n} บล็อก){note}"), &format!("&dClipboard rotated {what} ({n} blocks){note}")) } else { msg(th, &format!("&dพลิกคลิปบอร์ด {what} ({n} บล็อก){note}"), &format!("&dClipboard flipped {what} ({n} blocks){note}")) }
        }
        "clearclipboard" => {
            if let Some(s) = st.sessions.get_mut(&key(p)) {
                s.clip = None;
            }
            msg(th, "&dล้างคลิปบอร์ดแล้ว", "&dClipboard cleared")
        }
        "schem" => {
            let name_rule = msg(th, "&cชื่อใช้ได้เฉพาะ a-z 0-9 _ - ไม่เกิน 32 ตัว", "&cNames are a-z 0-9 _ - and at most 32 characters");
            match arg(0) {
                Some("list") => schem_list(th),
                Some("delete" | "del" | "remove") => schem_delete(th, arg(1)),
                Some(action @ ("save" | "load")) => {
                    let Some(name) = arg(1).and_then(schem::valid_name) else { return name_rule };
                    // Files are work for the disk: one a second per player
                    let s = st.sessions.entry(key(p)).or_default();
                    if s.last_file.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
                        return msg(th, "&eรอสักครู่แล้วลองใหม่", "&eWait a moment and try again");
                    }
                    s.last_file = Some(Instant::now());
                    if action == "save" { schem_save(st, p, &name) } else { schem_load(st, p, &name) }
                }
                _ => usage("//schem save|load|delete <ชื่อ> · //schem list"),
            }
        }
        "brush" => {
            let k = key(p);
            let u = "//brush sphere <บล็อก> [รัศมี] · //brush cylinder <บล็อก> [รัศมี] [สูง] · //brush none";
            let kind = match arg(0) {
                None | Some("list") => {
                    let have: Vec<String> = st.sessions.get(&k).map(|s| s.brushes.iter().map(|(item, b)| format!("{item} = {} {}", if b.cylinder { "cylinder" } else { "sphere" }, b.radius)).collect()).unwrap_or_default();
                    return if have.is_empty() { usage(u) } else { msg(th, &format!("&dbrush ของคุณ: &f{}", have.join("&7, &f")), &format!("&dYour brushes: &f{}", have.join("&7, &f"))) };
                }
                Some(kind) => kind,
            };
            let Some(item) = held(p) else { return msg(th, "&cถือไอเท็มที่จะใช้เป็น brush ไว้ในมือก่อน", "&cHold the item that should be the brush") };
            if kind == "none" || kind == "off" {
                let had = st.sessions.get_mut(&k).is_some_and(|s| s.brushes.remove(&item).is_some());
                return if had { msg(th, &format!("&dเลิกใช้ {item} เป็น brush แล้ว"), &format!("&d{item} is no longer a brush")) } else { msg(th, &format!("&e{item} ไม่ได้เป็น brush อยู่"), &format!("&e{item} is not a brush")) };
            }
            let cylinder = match kind {
                "sphere" | "s" => false,
                "cylinder" | "cyl" | "c" => true,
                _ => return usage(u),
            };
            if item == st.cfg.wand.trim_start_matches("minecraft:") {
                return msg(th, "&cไอเท็มนี้เป็น wand อยู่ ถือไอเท็มอื่น", "&cThis item is the wand; hold another one");
            }
            let Some(raw) = arg(1) else { return usage(u) };
            let pattern = ok!(st.cache.pattern(raw, th));
            let cap = st.cfg.brush_max_radius.clamp(1, 32) as i32;
            let radius = match arg(2) {
                Some(_) => match number(2, cap) {
                    Some(r) => r,
                    None => return msg(th, &format!("&cรัศมี brush ต้องเป็น 1 ถึง {cap}"), &format!("&cA brush radius is 1 to {cap}")),
                },
                None => 2.min(cap),
            };
            let height = number(3, 2 * cap + 1).unwrap_or(1);
            let s = st.sessions.entry(k).or_default();
            if s.brushes.len() >= 16 && !s.brushes.contains_key(&item) {
                return msg(th, "&cมี brush มากเกินไป เลิกบางอันก่อน (//brush none)", "&cToo many brushes; remove one first (//brush none)");
            }
            s.brushes.insert(item.clone(), Brush { cylinder, pattern, radius, height });
            msg(th, &format!("&d{item} เป็น brush {} รัศมี {radius} แล้ว: คลิกขวาเพื่อระบายที่บล็อกที่มอง (//brush none เพื่อเลิก)", if cylinder { "ทรงกระบอก" } else { "ทรงกลม" }), &format!("&d{item} is now a {} brush of radius {radius}: right click paints at the block you look at (//brush none removes it)", if cylinder { "cylinder" } else { "sphere" }))
        }
        "undo" | "redo" => {
            let undo = sub == "undo";
            let k = key(p);
            if st.jobs.iter().any(|j| j.r.owner == k) {
                return msg(th, "&cงานก่อนหน้ายังไม่เสร็จ รอสักครู่หรือ //cancel", "&cYour previous edit is still running; wait or //cancel");
            }
            let s = st.sessions.entry(k).or_default();
            let Some(edit) = (if undo { s.undo.pop() } else { s.redo.pop() }) else {
                return msg(th, if undo { "&eไม่มีอะไรให้ย้อน" } else { "&eไม่มีอะไรให้ทำซ้ำ" }, if undo { "&eNothing to undo" } else { "&eNothing to redo" });
            };
            if edit.dim != dim {
                let line = msg(th, "&cการแก้นั้นอยู่อีกโลกหนึ่ง ไปที่โลกนั้นก่อน", "&cThat edit is in another world; go there first");
                if undo { s.undo.push(edit) } else { s.redo.push(edit) }
                return line;
            }
            let (todo, states): (Vec<P>, Vec<u16>) = edit.blocks.into_iter().unzip();
            start(st, p, sub, todo, Work::States(states), if undo { Keep::Redo } else { Keep::UndoKeepRedo })
        }
        "cancel" => {
            let k = key(p);
            let before = st.jobs.len();
            // What was already changed stays undoable
            let mut kept = Vec::new();
            for mut job in std::mem::take(&mut st.jobs) {
                if job.r.owner == k {
                    tell_blocklog(&mut job.r);
                    if !job.r.before.is_empty() {
                        st.sessions.entry(k.clone()).or_default().undo.push(Edit { dim: job.r.dim, blocks: job.r.before });
                    }
                } else {
                    kept.push(job);
                }
            }
            st.jobs = kept;
            if st.jobs.len() < before { msg(th, "&dยกเลิกงานแล้ว (//undo ย้อนส่วนที่ทำไปได้)", "&dEdit cancelled (//undo reverts what was done)") } else { msg(th, "&eไม่มีงานที่กำลังทำ", "&eNo edit is running") }
        }
        _ => help(th),
    }
}

fn help(th: bool) -> String {
    msg(th, &format!("&dWorldTools: {HELP} &7· พิมพ์ /worldtools <คำสั่ง> ก็ได้"), &format!("&dWorldTools: {HELP} &7· /worldtools <command> works too"))
}

/// `Some(sub)` for a `//sub` command, `None` for the `/worldtools <sub>` umbrella.
struct Cmd(Option<&'static str>);

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, _server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let mut w = c::words(&args, "args");
        let sub = match self.0 {
            Some(s) => s.to_owned(),
            None if w.is_empty() => "help".to_owned(),
            None => w.remove(0).to_lowercase(),
        };
        let Some(mut guard) = try_lock() else { return Ok(0) };
        let Some(st) = guard.as_mut() else { return Ok(0) };
        let th = st.cfg.language != "en";
        let first = w.first().map(String::as_str);
        let line = match (sub.as_str(), c::sender_player(&sender)) {
            ("version", _) => msg(th, &format!("&fWorldTools {VERSION}"), &format!("&fWorldTools {VERSION}")),
            ("reload", _) => {
                st.cfg = c::load_config("config.toml");
                msg(th, "&aโหลดไฟล์ตั้งค่าใหม่แล้ว", "&aConfiguration reloaded")
            }
            // Files can be looked after from the console too
            ("schem", None) if first == Some("list") => schem_list(th),
            ("schem", None) if matches!(first, Some("delete" | "del" | "remove")) => schem_delete(th, w.get(1).map(String::as_str)),
            // The list of commands is worth reading from the console as well
            ("help", None) => help(th),
            (_, Some(p)) => run(&sub, st, &p, &w),
            (_, None) => msg(th, "&cคำสั่งนี้ใช้ได้เฉพาะในเกม", "&cOnly players can use this"),
        };
        if !line.is_empty() {
            c::reply(&sender, &line);
        }
        Ok(1)
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

/// The same click can arrive twice (both hands); answer once.
fn click(p: &Player, at: P, second: bool) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    let s = st.sessions.entry(key(p)).or_default();
    if s.last_click.is_some_and(|(a, b, t)| a == at && b == second && t.elapsed().as_millis() < 300) {
        return;
    }
    s.last_click = Some((at, second, Instant::now()));
    let line = set_corner(st, p, at, second);
    c::tell(p, &line);
}

#[derive(PartialEq)]
enum Tool {
    None,
    Wand,
    Brush,
}

/// What the item in this player's hand is to WorldTools. Runs on every click of every player, so
/// the cheap checks come first.
fn tool(p: &Player) -> Tool {
    let found = {
        let Some(guard) = try_lock() else { return Tool::None };
        let Some(st) = guard.as_ref() else { return Tool::None };
        let Some(item) = held(p) else { return Tool::None };
        if item == st.cfg.wand.trim_start_matches("minecraft:") {
            Tool::Wand
        } else if st.sessions.get(&key(p)).is_some_and(|s| s.brushes.contains_key(&item)) {
            Tool::Brush
        } else {
            Tool::None
        }
    };
    // Asked without the lock: a permission check can call into other plugins
    if found != Tool::None && allowed(p) { found } else { Tool::None }
}

/// A right click with a brush: paint at the clicked block, or at the block the player looks at.
fn stroke(p: &Player, clicked: Option<P>) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    let th = st.cfg.language != "en";
    let (gap, range) = (Duration::from_millis(u64::from(st.cfg.brush_interval_ms.clamp(50, 10_000))), st.cfg.brush_range.clamp(5, 256));
    let Some(item) = held(p) else { return };
    let k = key(p);
    let Some(s) = st.sessions.get_mut(&k) else { return };
    let Some(brush) = s.brushes.get(&item) else { return };
    // One click arrives as two events (use on block, then use in air), and a held button repeats
    if s.last_stroke.is_some_and(|t| t.elapsed() < gap) {
        return;
    }
    s.last_stroke = Some(Instant::now());
    let target = clicked.or_else(|| p.get_target_block(range).map(|(x, y, z)| (x.floor() as i32, y.floor() as i32, z.floor() as i32)));
    let Some(at) = target else {
        c::action_bar(p, if th { "&cไม่มีบล็อกในระยะที่มองอยู่" } else { "&cNo block in sight" });
        return;
    };
    let (lo, hi) = height(&p.get_world());
    let shape = if brush.cylinder { geo::cylinder(at, brush.radius, brush.height, false) } else { geo::sphere(at, brush.radius, false) };
    let todo: Vec<P> = shape.into_iter().filter(|b| b.1 >= lo && b.1 <= hi).collect();
    let work = Work::Fill(brush.pattern.clone());
    let line = start(st, p, "brush", todo, work, Keep::Undo);
    if !line.is_empty() {
        c::action_bar(p, &line);
    }
}

struct OnBreak;
impl EventHandler<BlockBreakEvent> for OnBreak {
    fn handle(&self, _server: Server, mut e: Data<BlockBreakEvent>) -> Data<BlockBreakEvent> {
        if let Some(p) = &e.player {
            if tool(p) == Tool::Wand {
                e.cancelled = true;
                click(p, (e.block_pos.x, e.block_pos.y, e.block_pos.z), false);
            }
        }
        e
    }
}

struct OnInteract;
impl EventHandler<PlayerInteractEvent> for OnInteract {
    fn handle(&self, _server: Server, mut e: Data<PlayerInteractEvent>) -> Data<PlayerInteractEvent> {
        if !matches!(e.action, InteractAction::RightClickBlock | InteractAction::RightClickAir) {
            return e;
        }
        let at = e.clicked_pos.as_ref().map(|at| (at.x, at.y, at.z));
        match (tool(&e.player), at) {
            (Tool::Wand, Some(at)) if matches!(e.action, InteractAction::RightClickBlock) => {
                e.cancelled = true;
                click(&e.player, at, true);
            }
            (Tool::Brush, at) => {
                e.cancelled = true;
                stroke(&e.player, at);
            }
            _ => {}
        }
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, e: Data<PlayerLeaveEvent>) -> Data<PlayerLeaveEvent> {
        // A running edit finishes; the clipboard, history and brushes go with the player
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            st.sessions.remove(&key(&e.player));
        }
        e
    }
}

struct WorldTools;

impl Plugin for WorldTools {
    fn new() -> Self {
        WorldTools
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "World editing by MCSV".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(State { cfg: c::load_config("config.toml"), sessions: HashMap::new(), jobs: Vec::new(), seed: now | 1, cache: Cache::default(), marks: false, ticks: 0 });
        // The panel reads this to tell "loaded" from "installed, waiting for a restart"
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": now }));
        let _ = context.register_permission(&Permission {
            node: format!("{NS}:use"),
            description: "WorldTools".into(),
            default: PermissionDefault::Op(PermissionLevel::Two),
            children: vec![],
        });
        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let greedy = |cmd: Cmd| CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(cmd);
        for sub in SUBS {
            // Named `/set`, so it is typed `//set`
            let cmd = Command::new(&[format!("/{sub}")], "WorldTools").execute(Cmd(Some(sub))).then(greedy(Cmd(Some(sub))));
            context.register_command(cmd, &format!("{NS}:use"));
        }
        let cmd = Command::new(&[NS.to_owned()], "WorldTools").execute(Cmd(None)).then(greedy(Cmd(None)));
        context.register_command(cmd, &format!("{NS}:use"));
        context.register_event_handler(OnBreak, EventPriority::Normal, true)?;
        context.register_event_handler(OnInteract, EventPriority::Normal, true)?;
        context.register_event_handler(OnLeave, EventPriority::Normal, false)?;
        pumpkin_plugin_api::scheduler::schedule_repeating_task(1, 1, on_tick);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::remove_file("status.json");
        Ok(())
    }
}

register_plugin!(WorldTools);

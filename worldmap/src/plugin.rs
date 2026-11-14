//! Pumpkin WASM glue for WorldMap: what to draw when, the drawing itself, saving, exporting, commands.
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::events::{BlockBreakEvent, BlockPlaceEvent, EventHandler, EventPriority, FromIntoEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::common::GameMode;
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA, NETWORK_TCP_BIND};
use pumpkin_plugin_api::wit::pumpkin::plugin::common::BlockPos;
use pumpkin_plugin_api::world::World;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::cfg::{self, Config, commas, dim_dir, spiral, square};
use crate::color::{self, Info, Kind};
use crate::export::{self, Marker, Meta};
use crate::store::{self, Key, Store, locate};
use crate::tile::{self, SIZE};
use crate::web::{self, Response, Route, Web};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NS: &str = "worldmap";
/// A chunk where a block changed is drawn again this many ticks later (the event comes before
/// the change, and one edit is rarely alone).
const DIRTY_DELAY: u64 = 40;
/// A `render … wait` job gives up after this long without a single new chunk.
const WAIT_IDLE_SECS: u64 = 1800;
const MAX_RADIUS: u32 = 512;
const MAX_QUEUE: usize = 50_000;
const MAX_DELAYED: usize = 2048;
/// Time of each tick the live page may take answering browsers, on top of what the drawing left
/// of `tick-ms`. One answer is always given, so a slow disk can go over it by one file read.
const WEB_BUDGET: Duration = Duration::from_millis(2);
/// The live page: no frames around it, and it may ask only the server it came from.
const PAGE_HEADERS: &str = "X-Frame-Options: DENY\r\n";

/// One mapped dimension of the running server.
struct Dim {
    /// `minecraft:overworld`
    name: String,
    /// Its number in the store.
    id: u8,
    min_y: i32,
    /// Highest block a column can have.
    top: i32,
    /// Look under the roof (nether).
    roof: bool,
}

/// What block states and biomes look like, asked from the server once each.
struct Cache {
    /// `Info::pack` per state id; `u16::MAX` = not asked yet.
    info: Vec<u16>,
    /// `color::biome_class` per biome; 255 = not asked yet.
    biomes: [u8; 256],
}

impl Cache {
    fn info(&mut self, state: u16) -> Info {
        let i = state as usize;
        if i >= self.info.len() {
            self.info.resize(i + 1, u16::MAX);
        }
        if self.info[i] == u16::MAX {
            let info = match pumpkin_plugin_api::world::get_block_state_by_id(state) {
                Some(s) => {
                    let wet = s.properties.iter().any(|(k, v)| k == "waterlogged" && v == "true");
                    color::classify(&s.block_name, s.map_color, s.is_air, s.is_liquid, s.is_solid, wet)
                }
                None => Info { kind: Kind::Solid, slot: color::UNKNOWN, low: false },
            };
            self.info[i] = info.pack();
        }
        Info::unpack(self.info[i])
    }

    fn biome(&mut self, world: &World, x: i32, y: i32, z: i32) -> u8 {
        let b = world.get_biome(BlockPos { x, y, z });
        let i = b as usize & 255;
        if self.biomes[i] == 255 {
            self.biomes[i] = color::biome_class(&format!("{b:?}"));
        }
        self.biomes[i]
    }
}

/// A chunk being drawn, one row of 16 columns at a time.
struct Job {
    key: Key,
    row: usize,
    px: [u8; 256],
    /// Ground height of the row north of `row`.
    prev: [i32; 16],
    north_known: bool,
    /// Biome class per 4 x 4 columns; 255 = not asked.
    biome: [u8; 16],
    spent: Duration,
}

#[derive(Clone)]
enum Who {
    Console,
    Player(String),
}

/// `/worldmap render`: a square of chunks around a point.
struct Area {
    dim: u8,
    cx: i32,
    cz: i32,
    r: u32,
    n: u32,
    cursor: u32,
    pass: u32,
    /// Keep going until every chunk of the square has been seen loaded once.
    wait: bool,
    /// Chunks still wanted (bit = spiral position).
    pending: Vec<u64>,
    left: u32,
    drawn: u32,
    absent: u32,
    who: Who,
    started: u64,
    progress: u64,
    /// Rounds in a row that found nothing new.
    empty_rounds: u32,
    /// Wanted chunks next to ones just found loaded: asked about first, because chunks are
    /// generated (and unloaded again) in strips.
    hot: Vec<u32>,
}

impl Area {
    /// Spiral position of a chunk, if it is inside the square.
    fn position(&self, key: Key) -> Option<u32> {
        if key.0 != self.dim {
            return None;
        }
        let (dx, dz) = (i64::from(key.1) - i64::from(self.cx), i64::from(key.2) - i64::from(self.cz));
        if dx.abs().max(dz.abs()) > i64::from(self.r) {
            return None;
        }
        Some(cfg::spiral_index(dx as i32, dz as i32))
    }

    fn wanted(&self, i: u32) -> bool {
        self.pending[(i / 64) as usize] >> (i % 64) & 1 == 1
    }

    /// Chunk at spiral position `i` was found loaded: remember its wanted neighbours.
    fn warm(&mut self, i: u32) {
        let (dx, dz) = spiral(i);
        let r = self.r as i32;
        for (ox, oz) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
            let (nx, nz) = (dx + ox, dz + oz);
            if nx.abs() <= r && nz.abs() <= r && self.hot.len() < 4096 {
                let n = cfg::spiral_index(nx, nz);
                if self.wanted(n) {
                    self.hot.push(n);
                }
            }
        }
    }

    fn set(&mut self, i: u32, on: bool) {
        let (w, b) = ((i / 64) as usize, i % 64);
        let was = self.pending[w] >> b & 1 == 1;
        if on && !was {
            self.pending[w] |= 1 << b;
            self.left += 1;
        } else if !on && was {
            self.pending[w] &= !(1 << b);
            self.left -= 1;
        }
    }
}

enum Phase {
    Flush,
    Head,
    Tiles,
    Tail,
}

/// `/worldmap export`: one page being written.
struct Export {
    dim: u8,
    who: Who,
    phase: Phase,
    scale: u32,
    tiles: Vec<(i32, i32)>,
    at: usize,
    bytes: u64,
    left_out: u32,
    cap: u64,
    tmp: PathBuf,
    out: PathBuf,
    file: String,
    meta: Meta,
    started: Instant,
    /// A tile being shrunk, one band per step: its position, the bands on disk, the bands done.
    part: Option<(i32, i32, Vec<String>, Vec<String>)>,
}

#[derive(Default)]
struct Stats {
    drawn: u64,
    unchanged: u64,
    not_loaded: u64,
    draw_time: Duration,
    slowest_chunk: Duration,
    slowest_tick: Duration,
    probes: u64,
    probe_time: Duration,
    /// Longest single piece of saving and of looking around players.
    slowest_save: Duration,
    slowest_scan: Duration,
    slowest_export: Duration,
    /// Ticks in which WorldMap did something, and those that took over a millisecond more than the budget.
    busy_ticks: u64,
    over_ticks: u64,
    /// Longest time one tick spent on the live page.
    slowest_web: Duration,
}

/// The live map page (`web-port`).
struct Site {
    /// The listener and its connections; `None` = off, or the port could not be opened.
    web: Option<Web>,
    /// Why the port could not be opened.
    error: Option<String>,
    /// The page and its entity tag (plugin version, language, with or without players).
    page: Arc<Vec<u8>>,
    page_tag: String,
    /// `index.json` as last read, by entity tag.
    index: Option<(String, Arc<Vec<u8>>)>,
    /// `players.json`, made again every second.
    players: Arc<Vec<u8>>,
}

struct State {
    cfg: Config,
    paused: bool,
    tick: u64,
    store: Store,
    dims: Vec<Dim>,
    cache: Cache,
    queue: VecDeque<Key>,
    queued: HashSet<Key>,
    /// Chunks to draw at a later tick (a block changed there).
    delayed: Vec<(u64, Key)>,
    /// Chunks players stand in, still to be looked around (`Key` = dimension, chunk x, chunk z).
    centres: VecDeque<Key>,
    /// Places where everything around is drawn and fresh, and until when (Unix seconds): a player
    /// standing still is not looked around again before a chunk there is due.
    settled: HashMap<Key, u64>,
    job: Option<Job>,
    area: Option<Area>,
    export: Option<Export>,
    stats: Stats,
    site: Site,
    /// When this plugin was loaded (Unix seconds), for `status.json`.
    loaded_at: u64,
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

fn load() -> Config {
    c::load_config::<Config>("config.toml").cleaned()
}

fn msg(th: bool, thai: &str, english: &str) -> String {
    format!("&8[&bWorldMap&8]&r {}", if th { thai } else { english })
}

fn who(sender: &CommandSender) -> Who {
    match c::sender_player(sender) {
        Some(p) => Who::Player(p.get_name()),
        None => Who::Console,
    }
}

/// Tell whoever started a job how it ended; the console always gets the line.
fn notify(server: &Server, who: &Who, text: &str) {
    if let Who::Player(name) = who {
        if let Some(p) = server.get_player_by_name(name) {
            c::tell(&p, text);
        }
    }
    c::log_info(&c::plain(text.trim_start_matches("&8[&bWorldMap&8]&r ")));
}

/// Short name of a dimension for messages: `overworld`.
fn short(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// The worlds of the server by dimension name.
fn worlds(server: &Server) -> Vec<(String, World)> {
    server.get_all_worlds().into_iter().map(|w| (w.get_dimension(), w)).collect()
}

fn world_of<'a>(worlds: &'a [(String, World)], name: &str) -> Option<&'a World> {
    worlds.iter().find(|(n, _)| n == name).map(|(_, w)| w)
}

/// Find the dimensions to map among the worlds the server has now.
fn refresh_dims(st: &mut State, server: &Server) {
    let mut dims = Vec::new();
    for (name, world) in worlds(server) {
        if !st.cfg.maps(&name) || dims.iter().any(|d: &Dim| d.name == name) {
            continue;
        }
        let Some(id) = st.store.dim_id(&name, &dim_dir(&name)) else { continue };
        let min_y = world.get_min_y();
        let spawn = world.get_spawn_location().pos;
        st.store.set_world(id, [spawn.x, spawn.y, spawn.z], min_y);
        // The API has the bottom of the world but not its height: the two vanilla shapes
        let top = if min_y < 0 { 319 } else { 255 };
        dims.push(Dim { roof: cfg::has_roof(&name), name, id, min_y, top });
    }
    st.dims = dims;
}

fn enqueue(st: &mut State, key: Key, front: bool) {
    if st.queue.len() >= MAX_QUEUE || !st.queued.insert(key) {
        return;
    }
    if front { st.queue.push_front(key) } else { st.queue.push_back(key) }
}

/// The topmost block the map draws in a column, and its height.
///
/// The server's height map is only a first guess. Pumpkin 0.2.2 never raises it when a block is
/// placed (`ChunkData::update_heightmap` passes x, z, y to a function that takes x, y, z), and it
/// saves it that way, so anything built after the terrain was generated is above the recorded
/// top. Such a column is darker than open sky right over the recorded top (sky light is kept up
/// to date), and then one ray from the build limit down finds the real top. Removed blocks leave
/// the recorded top too high; the walk down below covers that.
fn surface(cache: &mut Cache, world: &World, dim: &Dim, x: i32, z: i32) -> (i32, Info) {
    let mut y = world.get_top_block_y(x, z);
    if !dim.roof && y < dim.top {
        // An empty column answers min_y after the server looked through all of it: nothing to find above
        let empty = y <= dim.min_y && cache.info(world.get_block_state_id(BlockPos { x, y, z })).kind == Kind::Clear;
        if !empty && world.get_sky_light(BlockPos { x, y: y + 1, z }) < 15 {
            let (fx, fz) = (f64::from(x) + 0.5, f64::from(z) + 0.5);
            if let Some(hit) = world.ray_trace_block((fx, f64::from(dim.top) + 1.0, fz), (fx, f64::from(y) + 1.0, fz), true) {
                if hit.pos.x == x && hit.pos.z == z && hit.pos.y > y {
                    y = hit.pos.y;
                }
            }
        }
    }
    if dim.roof {
        // Under the roof: down through the solid part (four blocks at a time, which also steps
        // over small pockets in it), then down through the open part
        let top = y;
        let mut steps = 0;
        while y > dim.min_y && steps < 40 && cache.info(world.get_block_state_id(BlockPos { x, y, z })).kind != Kind::Clear {
            y -= 4;
            steps += 1;
        }
        if y <= dim.min_y || steps >= 40 {
            // Solid all the way: show the top
            return (top, cache.info(world.get_block_state_id(BlockPos { x, y: top, z })));
        }
    }
    let mut steps = 0;
    loop {
        if y < dim.min_y || steps > 160 {
            return (dim.min_y, Info::CLEAR);
        }
        let info = cache.info(world.get_block_state_id(BlockPos { x, y, z }));
        if info.kind != Kind::Clear {
            return (y, info);
        }
        y -= 1;
        steps += 1;
    }
}

fn ground(y: i32, info: Info) -> i32 {
    if info.low { y - 1 } else { y }
}

/// Begin drawing a chunk. `None` when it is not loaded.
fn start_job(cache: &mut Cache, world: &World, dim: &Dim, key: Key) -> Option<Job> {
    let (cx, cz) = (key.1, key.2);
    world.get_chunk(cx, cz)?;
    let mut job = Job { key, row: 0, px: [0; 256], prev: [0; 16], north_known: false, biome: [255; 16], spent: Duration::ZERO };
    let began = Instant::now();
    if world.get_chunk(cx, cz - 1).is_some() {
        for lx in 0..16 {
            let (y, info) = surface(cache, world, dim, cx * 16 + lx as i32, cz * 16 - 1);
            job.prev[lx] = ground(y, info);
        }
        job.north_known = true;
    }
    job.spent = began.elapsed();
    Some(job)
}

/// Draw the next row of a chunk.
fn draw_row(cache: &mut Cache, world: &World, dim: &Dim, job: &mut Job) {
    let row = job.row;
    let z = job.key.2 * 16 + row as i32;
    let known = row > 0 || job.north_known;
    let mut heights = [0i32; 16];
    for lx in 0..16usize {
        let x = job.key.1 * 16 + lx as i32;
        let (y, info) = surface(cache, world, dim, x, z);
        heights[lx] = ground(y, info);
        if info.kind == Kind::Clear {
            continue;
        }
        let class = if info.kind == Kind::Solid {
            0
        } else {
            // Biomes are stored per 4 x 4 x 4 blocks: one question per 4 x 4 columns is exact enough
            let cell = (row / 4) * 4 + lx / 4;
            if job.biome[cell] == 255 {
                job.biome[cell] = cache.biome(world, x, y, z);
            }
            job.biome[cell]
        };
        job.px[row * 16 + lx] = match info.kind {
            Kind::Water => {
                let (normal_from, dark_from) = color::water_steps((x + z) & 1 != 0);
                let mut wet = |depth: i32| cache.info(world.get_block_state_id(BlockPos { x, y: y - (depth - 1), z })).kind == Kind::Water;
                let shade = if wet(dark_from) {
                    color::DARK
                } else if wet(normal_from) {
                    color::NORMAL
                } else {
                    color::BRIGHT
                };
                color::pixel(color::water_slot(class), shade)
            }
            kind => {
                let slot = match kind {
                    Kind::Grass => color::grass_slot(class),
                    Kind::Foliage => color::foliage_slot(class),
                    _ => info.slot,
                };
                color::pixel(slot, color::height_shade(heights[lx], known.then_some(job.prev[lx])))
            }
        };
    }
    job.prev = heights;
    job.row += 1;
}

/// A chunk could not be drawn (it was unloaded): a waiting render job wants it back.
fn give_back(st: &mut State, key: Key) {
    st.stats.not_loaded += 1;
    if let Some(a) = st.area.as_mut() {
        if a.wait {
            if let Some(i) = a.position(key) {
                a.set(i, true);
            }
        }
    }
}

/// Store a finished chunk.
fn finish(st: &mut State, world: &World, job: Job, now: u64) {
    let key = job.key;
    if world.get_chunk(key.1, key.2).is_none() {
        // Unloaded half way: what was read after that is air
        give_back(st, key);
        return;
    }
    let ((tx, tz), (lx, lz)) = locate(key.1, key.2);
    let tile = st.store.tile((key.0, tx, tz), now);
    let changed = tile.set_chunk(lx, lz, &job.px, now);
    if job.north_known {
        tile.north[lz] &= !(1 << lx);
    } else {
        tile.north[lz] |= 1 << lx;
    }
    st.stats.drawn += 1;
    if !changed {
        st.stats.unchanged += 1;
    }
    st.stats.draw_time += job.spent;
    st.stats.slowest_chunk = st.stats.slowest_chunk.max(job.spent);
    // The chunk to the south may have been drawn before this one existed: its first row can be shaded now
    if st.store.chunk_time(key.0, key.1, key.2 + 1, now) != 0 {
        let ((sx, sz), (slx, slz)) = locate(key.1, key.2 + 1);
        if st.store.tile((key.0, sx, sz), now).north[slz] >> slx & 1 == 1 {
            enqueue(st, (key.0, key.1, key.2 + 1), false);
        }
    }
    if let Some(a) = st.area.as_mut() {
        if a.position(key).is_some() {
            a.drawn += 1;
        }
    }
}

/// Note where the players are; the looking around is done a couple of places per tick.
fn find_players(st: &mut State, server: &Server, now: u64) {
    if st.settled.len() > 4096 {
        st.settled.clear();
    }
    for p in server.get_all_players() {
        if c::awaiting_login(&p) {
            continue;
        }
        let name = p.get_world().get_dimension();
        let Some(id) = st.dims.iter().find(|d| d.name == name).map(|d| d.id) else { continue };
        let (x, _, z) = p.get_position();
        let centre = (id, (x.floor() as i32) >> 4, (z.floor() as i32) >> 4);
        if !st.centres.contains(&centre) && st.settled.get(&centre).is_none_or(|until| *until <= now) {
            st.centres.push_back(centre);
        }
    }
}

/// Queue the chunks around one place that were never drawn or are due again.
fn scan_around(st: &mut State, centre: Key, radius: u32, now: u64) {
    let refresh = u64::from(st.cfg.refresh_minutes) * 60;
    let (mut waiting, mut oldest) = (false, now);
    for i in 0..square(radius) {
        let (dx, dz) = spiral(i);
        let key = (centre.0, centre.1 + dx, centre.2 + dz);
        if st.queued.contains(&key) || st.job.as_ref().is_some_and(|j| j.key == key) {
            waiting = true;
            continue;
        }
        let at = u64::from(st.store.chunk_time(key.0, key.1, key.2, now));
        if at == 0 || now.saturating_sub(at) >= refresh {
            enqueue(st, key, false);
            waiting = true;
        } else {
            oldest = oldest.min(at);
        }
    }
    if waiting {
        st.settled.remove(&centre);
    } else {
        st.settled.insert(centre, oldest + refresh);
    }
}

/// Move a render job forward: look for loaded chunks of its square and queue them.
/// Returns whether anything was queued.
fn feed_area(st: &mut State, worlds: &[(String, World)], server: &Server, now: u64, began: Instant, budget: Duration) -> bool {
    let th = st.cfg.language != "en";
    let Some(mut a) = st.area.take() else { return false };
    // Nothing new for ten seconds: ask every fourth tick only, until something turns up
    if a.empty_rounds >= 200 && st.tick % 4 != 0 {
        st.area = Some(a);
        return false;
    }
    let Some(world) = st.dims.iter().find(|d| d.id == a.dim).and_then(|d| world_of(worlds, &d.name)) else {
        notify(server, &a.who, &msg(th, "&cยกเลิกงาน render: มิตินี้ไม่อยู่แล้ว", "&cRender cancelled: that dimension is gone"));
        return false;
    };
    let (mut found, mut probes, mut looked) = (false, 0u32, 0u32);
    let t0 = Instant::now();
    // Asking "is this chunk loaded" may use half a tick's share: a waiting job asks every tick
    let limit = budget / 2;
    let mut ended = false;
    // Not more than can be drawn in the next few ticks: a chunk that waits in the queue may be gone by its turn
    while st.queue.len() < 256 && probes < 2048 {
        let Some(i) = a.hot.pop() else { break };
        if !a.wanted(i) {
            continue;
        }
        let (dx, dz) = spiral(i);
        let key = (a.dim, a.cx + dx, a.cz + dz);
        probes += 1;
        if world.get_chunk(key.1, key.2).is_some() {
            a.set(i, false);
            a.progress = now;
            a.empty_rounds = 0;
            a.warm(i);
            enqueue(st, key, false);
            found = true;
        }
    }
    while st.queue.len() < 256 && probes < 4096 && looked < 200_000 {
        if a.cursor >= a.n {
            ended = true;
            break;
        }
        let i = a.cursor;
        looked += 1;
        if a.pass > 0 {
            let word = a.pending[(i / 64) as usize];
            if word == 0 {
                a.cursor = (i / 64 + 1) * 64;
                continue;
            }
            if word >> (i % 64) & 1 == 0 {
                a.cursor += 1;
                continue;
            }
        }
        a.cursor += 1;
        let (dx, dz) = spiral(i);
        let key = (a.dim, a.cx + dx, a.cz + dz);
        probes += 1;
        if world.get_chunk(key.1, key.2).is_some() {
            a.set(i, false);
            a.progress = now;
            a.empty_rounds = 0;
            a.warm(i);
            enqueue(st, key, false);
            found = true;
        } else if a.pass == 0 {
            a.absent += 1;
            if a.wait && st.store.chunk_time(a.dim, key.1, key.2, now) == 0 {
                a.set(i, true);
            }
        }
        if probes % 128 == 0 && (began.elapsed() >= budget || t0.elapsed() >= limit) {
            break;
        }
    }
    st.stats.probes += u64::from(probes);
    st.stats.probe_time += t0.elapsed();
    if ended {
        let idle = st.queue.is_empty() && st.job.is_none();
        let gave_up = a.wait && now.saturating_sub(a.progress) >= WAIT_IDLE_SECS;
        if a.wait && a.left > 0 && !gave_up {
            // Go round again for the chunks that were not loaded yet
            if a.pass == 0 {
                notify(
                    server,
                    &a.who,
                    &msg(
                        th,
                        &format!("&7รอบแรกเสร็จ: วาดได้ {} chunks เหลือ {} chunks ที่ยังไม่เคยโหลด กำลังรอ (เช่นให้ Pregen โหลดให้) · หยุดรอด้วย /worldmap render stop", commas(u64::from(a.n - a.left)), commas(u64::from(a.left))),
                        &format!("&7First pass done: {} chunks found, {} chunks were never loaded; waiting for them (run Pregen over the area, for example) · /worldmap render stop ends the wait", commas(u64::from(a.n - a.left)), commas(u64::from(a.left))),
                    ),
                );
            }
            a.pass += 1;
            a.cursor = 0;
            a.empty_rounds = a.empty_rounds.saturating_add(1);
        } else if idle {
            let secs = now.saturating_sub(a.started);
            let line = if a.wait {
                if a.left == 0 {
                    msg(th, &format!("&aทำแผนที่ครบ {} chunks ใน {} วินาที", commas(u64::from(a.drawn)), secs), &format!("&aAll {} chunks mapped in {} s", commas(u64::from(a.drawn)), secs))
                } else {
                    msg(
                        th,
                        &format!("&eเลิกรอ: ไม่มี chunk ใหม่มา {} นาทีแล้ว วาดได้ {} chunks เหลือ {} chunks", WAIT_IDLE_SECS / 60, commas(u64::from(a.drawn)), commas(u64::from(a.left))),
                        &format!("&eStopped waiting: no new chunk for {} minutes. {} chunks mapped, {} still missing", WAIT_IDLE_SECS / 60, commas(u64::from(a.drawn)), commas(u64::from(a.left))),
                    )
                }
            } else if a.absent == 0 {
                msg(th, &format!("&aวาดแผนที่ {} chunks เสร็จใน {} วินาที", commas(u64::from(a.drawn)), secs), &format!("&a{} chunks mapped in {} s", commas(u64::from(a.drawn)), secs))
            } else {
                msg(
                    th,
                    &format!("&aวาดแผนที่ {} chunks เสร็จใน {} วินาที &e(ข้าม {} chunks ที่ไม่ได้โหลดอยู่: ปลั๊กอินโหลด chunk เองไม่ได้ ให้ผู้เล่นเดินไปใกล้ หรือสั่ง /worldmap render {} wait แล้วเปิด Pregen)", commas(u64::from(a.drawn)), secs, commas(u64::from(a.absent)), a.r),
                    &format!("&a{} chunks mapped in {} s &e({} chunks skipped because they are not loaded: a plugin cannot load chunks. Walk there, or run /worldmap render {} wait and start Pregen)", commas(u64::from(a.drawn)), secs, commas(u64::from(a.absent)), a.r),
                )
            };
            notify(server, &a.who, &line);
            return found;
        }
        // else: wait for the last chunks in the queue to be drawn, then report
    }
    st.area = Some(a);
    found
}

/// Draw chunks until the time for this tick is used up.
fn render(st: &mut State, server: &Server, now: u64, began: Instant, budget: Duration) {
    if st.job.is_none() && st.queue.is_empty() && st.area.is_none() {
        return;
    }
    let worlds = worlds(server);
    while began.elapsed() < budget {
        let Some(mut job) = st.job.take() else {
            let Some(key) = st.queue.pop_front() else {
                if feed_area(st, &worlds, server, now, began, budget) {
                    continue;
                }
                break;
            };
            st.queued.remove(&key);
            let started = st.dims.iter().find(|d| d.id == key.0).and_then(|dim| Some(start_job(&mut st.cache, world_of(&worlds, &dim.name)?, dim, key)));
            match started {
                Some(Some(job)) => st.job = Some(job),
                Some(None) => give_back(st, key),
                None => {}
            }
            continue;
        };
        let Some((dim, world)) = st.dims.iter().find(|d| d.id == job.key.0).and_then(|d| Some((d, world_of(&worlds, &d.name)?))) else { continue };
        let t0 = Instant::now();
        draw_row(&mut st.cache, world, dim, &mut job);
        job.spent += t0.elapsed();
        if job.row == 16 {
            finish(st, world, job, now);
        } else {
            st.job = Some(job);
        }
    }
}

fn append(path: &PathBuf, text: &str, truncate: bool) -> bool {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true);
    if truncate { options.truncate(true) } else { options.append(true) };
    options.open(path).and_then(|mut f| f.write_all(text.as_bytes())).is_ok()
}

/// One piece of an export. Returns false when the export is over.
fn export_step(st: &mut State, server: &Server, now: u64) -> bool {
    let th = st.cfg.language != "en";
    let Some(mut e) = st.export.take() else { return false };
    let t0 = Instant::now();
    let fail = |e: &Export, server: &Server| {
        let _ = std::fs::remove_file(&e.tmp);
        notify(server, &e.who, &msg(th, "&cexport ไม่สำเร็จ: เขียนไฟล์ไม่ได้ (ดิสก์เต็มหรือไม่มีสิทธิ์)", "&cExport failed: the file could not be written (disk full or no permission)"));
    };
    match e.phase {
        Phase::Flush => {
            // What is in memory goes to disk first: the page is built from the tile files
            if st.store.dirty_in(e.dim) > 0 {
                st.store.flush_step(now, true);
            } else {
                let dim = &st.store.dims[e.dim as usize];
                e.tiles = dim.tiles.keys().copied().collect();
                e.meta.chunks = dim.chunks();
                e.scale = export::pick_scale(dim.bytes(), export::fixed_bytes(&e.meta), e.cap);
                e.phase = Phase::Head;
            }
        }
        Phase::Head => {
            e.meta.scale = e.scale;
            let head = export::head(&e.meta);
            if !append(&e.tmp, &head, true) {
                fail(&e, server);
                return false;
            }
            e.bytes = head.len() as u64;
            e.at = 0;
            e.left_out = 0;
            e.part = None;
            e.phase = Phase::Tiles;
        }
        Phase::Tiles => {
            let f = e.scale as usize;
            // `Some(Some(block))` = a tile is ready, `Some(None)` = a tile is left out, `None` = still working
            let ready = match e.part.take() {
                // Shrinking: one band of the tile per step
                Some((x, z, bands, mut done)) if done.len() < bands.len() => {
                    let rows = tile::unpack(&bands[done.len()], tile::BAND_ROWS * SIZE).unwrap_or_else(|| vec![0; tile::BAND_ROWS * SIZE]);
                    done.push(tile::pack(&tile::downsample(&rows, SIZE, f)));
                    e.part = Some((x, z, bands, done));
                    None
                }
                Some((x, z, _, done)) => Some(export::tile_block(x, z, &tile::export_json(x, z, SIZE / f, tile::BAND_ROWS / f, &done))),
                None => match e.tiles.get(e.at).copied() {
                    None => {
                        e.phase = Phase::Tail;
                        None
                    }
                    Some((x, z)) => {
                        e.at += 1;
                        let text = st.store.read_file(e.dim, x, z);
                        if f == 1 {
                            // Full resolution: the tile file goes in as it is
                            Some(text.and_then(|t| export::tile_block(x, z, &t)))
                        } else {
                            match text.and_then(|t| tile::file_bands(&t)) {
                                Some(bands) => {
                                    e.part = Some((x, z, bands, Vec::new()));
                                    None
                                }
                                None => Some(None),
                            }
                        }
                    }
                },
            };
            match ready {
                None => {}
                Some(None) => e.left_out += 1,
                Some(Some(block)) => {
                    if !append(&e.tmp, &block, false) {
                        fail(&e, server);
                        return false;
                    }
                    e.bytes += block.len() as u64;
                    // The guess was too kind: start over one step smaller
                    if e.bytes + export::tail().len() as u64 > e.cap && e.scale < export::MAX_SCALE {
                        e.scale *= 2;
                        e.phase = Phase::Head;
                    }
                }
            }
        }
        Phase::Tail => {
            let tail = export::tail();
            if !append(&e.tmp, &tail, false) || std::fs::rename(&e.tmp, &e.out).is_err() {
                fail(&e, server);
                return false;
            }
            e.bytes += tail.len() as u64;
            let secs = e.started.elapsed().as_secs_f32();
            let (tiles, chunks, size) = (e.tiles.len() as u32 - e.left_out, commas(e.meta.chunks), cfg::size(e.bytes));
            let detail = if e.scale == 1 {
                msg(th, "ความละเอียดเต็ม 1 จุด = 1 บล็อก", "full resolution, 1 pixel = 1 block")
            } else {
                msg(
                    th,
                    &format!("&eย่อความละเอียดเหลือ 1 จุด = {} บล็อก เพราะแผนที่เต็มใหญ่กว่า {} MB (ตั้ง export-max-mb)", e.scale, e.cap / 1024 / 1024),
                    &format!("&ereduced to 1 pixel = {} blocks because the full map is larger than {} MB (export-max-mb)", e.scale, e.cap / 1024 / 1024),
                )
            };
            let detail = detail.trim_start_matches("&8[&bWorldMap&8]&r ");
            let mut line = msg(
                th,
                &format!("&aexport เสร็จ: &fplugins/data/worldmap/{} &7({size}, {tiles} tiles, {chunks} chunks, {secs:.1} วินาที) {detail}&7 · ดาวน์โหลดไฟล์นี้จากตัวจัดการไฟล์แล้วเปิดในเบราว์เซอร์", e.file),
                &format!("&aExport done: &fplugins/data/worldmap/{} &7({size}, {tiles} tiles, {chunks} chunks, {secs:.1} s) {detail}&7 · download the file from the file manager and open it in a browser", e.file),
            );
            if e.left_out > 0 {
                line.push_str(&if th { format!(" &c(ข้าม {} tile ที่อ่านไม่ได้)", e.left_out) } else { format!(" &c({} unreadable tiles left out)", e.left_out) });
            }
            notify(server, &e.who, &line);
            return false;
        }
    }
    st.stats.slowest_export = st.stats.slowest_export.max(t0.elapsed());
    st.export = Some(e);
    true
}

/// The panel reads this to tell "loaded" from "installed, waiting for a restart", and whether the
/// live page is up.
fn write_status(st: &State) {
    let web = serde_json::json!({ "port": st.cfg.web_port, "listening": st.site.web.is_some(), "error": st.site.error });
    c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": st.loaded_at, "web": web }));
}

/// Open, move or close the live page so that it is what the settings say.
fn web_apply(st: &mut State) {
    let th = st.cfg.language != "en";
    st.site.page = Arc::new(export::live_page(th, VERSION, st.cfg.web_players).into_bytes());
    st.site.page_tag = format!("\"{VERSION}-{}-{}\"", st.cfg.language, u8::from(st.cfg.web_players));
    let port = st.cfg.web_port;
    if st.site.web.as_ref().is_none_or(|w| w.port != port) {
        // Dropping the old one closes its port and its connections
        st.site.web = None;
        st.site.error = None;
        if port != 0 {
            match Web::bind(port) {
                Ok(w) => {
                    st.site.web = Some(w);
                    c::log_info(&if th { format!("เว็บแผนที่เปิดแล้วที่ port {port} (http://<ที่อยู่เซิร์ฟ>:{port})") } else { format!("The live map page is open on port {port} (http://<server address>:{port})") });
                }
                Err(e) => {
                    c::log_info(&if th { format!("เปิดเว็บแผนที่ที่ port {port} ไม่ได้: {e}") } else { format!("The live map page could not open port {port}: {e}") });
                    st.site.error = Some(e.to_string());
                }
            }
        }
    }
    write_status(st);
}

/// `"<modified>-<size>"` of a file, to tell a browser that what it has is still right.
fn file_tag(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(format!("\"{:x}-{:x}\"", modified.as_millis(), meta.len()))
}

/// Who is where, for the live page. Spectators and invisible players are left out, as on squaremap.
fn players_json(st: &State, server: &Server, now: u64) -> Vec<u8> {
    let mut list = Vec::new();
    if st.cfg.web_players {
        for p in server.get_all_players() {
            if c::awaiting_login(&p) || matches!(p.get_gamemode(), GameMode::Spectator) || p.has_effect(pumpkin_plugin_api::wit::pumpkin::plugin::status_effect::StatusEffectType::Invisibility) {
                continue;
            }
            let (x, y, z) = p.get_position();
            let round = |v: f64| (v * 10.0).round() / 10.0;
            list.push(serde_json::json!({ "name": p.get_name(), "dim": dim_dir(&p.get_world().get_dimension()), "x": round(x), "y": round(y), "z": round(z) }));
        }
    }
    serde_json::to_vec(&serde_json::json!({ "t": now, "players": list })).unwrap_or_else(|_| b"{\"players\":[]}".to_vec())
}

/// Answer one request of the live page. Only the page, the index, the players and tile files of
/// known dimensions can be asked for (`web::route`).
fn respond(st: &mut State, req: &web::Request, now: u64) -> Response {
    match web::route(&req.path) {
        Route::Page => Response { status: 200, kind: "text/html; charset=utf-8", body: st.site.page.clone(), etag: Some(st.site.page_tag.clone()), cache: web::NO_CACHE, extra: PAGE_HEADERS },
        Route::Players => Response::json(st.site.players.clone(), None, web::NO_STORE),
        Route::Index => {
            let path = c::data_path("index.json");
            if !path.exists() {
                // Nothing has been saved yet: an index without tiles
                st.store.write_index(now);
            }
            let tag = file_tag(&path);
            if let (Some(tag), Some((have, body))) = (&tag, &st.site.index) {
                if tag == have {
                    return Response::json(body.clone(), Some(tag.clone()), web::NO_CACHE);
                }
            }
            let Ok(bytes) = std::fs::read(&path) else { return Response::not_found() };
            let body = Arc::new(bytes);
            st.site.index = tag.clone().map(|t| (t, body.clone()));
            Response::json(body, tag, web::NO_CACHE)
        }
        Route::Tile { dir, x, z } => {
            if !st.store.dims.iter().any(|d| d.dir == dir) {
                return Response::not_found();
            }
            let path = c::data_path(&store::tile_file(&dir, x, z));
            let tag = file_tag(&path);
            // A tile asked for by version (`?v=<updated>`) never changes under that address
            let cache = if req.query.starts_with("v=") { web::FOREVER } else { web::NO_CACHE };
            let known = tag.as_deref().is_some_and(|t| req.etag.as_deref().is_some_and(|sent| web::etag_matches(sent, t)));
            let bytes = if known { Vec::new() } else { std::fs::read(&path).unwrap_or_default() };
            if bytes.is_empty() && !known {
                return Response::not_found();
            }
            Response::json(Arc::new(bytes), tag, cache)
        }
        Route::Unknown => Response::not_found(),
    }
}

/// The live page's share of a tick: its own budget plus what (`spare`) the drawing did not use of its own.
fn serve(st: &mut State, server: &Server, now: u64, spare: Duration) {
    let Some(mut web) = st.site.web.take() else { return };
    let began = Instant::now();
    if st.tick % 20 == 1 {
        st.site.players = Arc::new(players_json(st, server, now));
    }
    web.poll(began, began + WEB_BUDGET + spare, &mut |req| respond(st, req, now));
    st.site.web = Some(web);
    st.stats.slowest_web = st.stats.slowest_web.max(began.elapsed());
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    let began = Instant::now();
    st.tick += 1;
    let now = c::now_secs();
    if st.tick == 1 || st.tick % 600 == 0 {
        refresh_dims(st, &server);
    }
    if !st.delayed.is_empty() && !st.paused {
        let tick = st.tick;
        let mut i = 0;
        while i < st.delayed.len() {
            if st.delayed[i].0 <= tick {
                let key = st.delayed.swap_remove(i).1;
                enqueue(st, key, true);
            } else {
                i += 1;
            }
        }
    }
    let budget = Duration::from_micros(u64::from(st.cfg.tick_ms) * 1000);
    if !st.paused && st.cfg.around_players > 0 {
        // Once a second when few are online; with many, as fast as two places per tick allows
        if st.tick % 20 == 0 && st.centres.is_empty() {
            find_players(st, &server, now);
        }
        if !st.centres.is_empty() {
            let radius = st.cfg.around_players.min(u32::from(server.get_view_distance()).max(2));
            let t0 = Instant::now();
            for _ in 0..2 {
                let Some(centre) = st.centres.pop_front() else { break };
                scan_around(st, centre, radius, now);
            }
            st.stats.slowest_scan = st.stats.slowest_scan.max(t0.elapsed());
        }
    } else {
        st.centres.clear();
        st.settled.clear();
    }
    // Saving goes on while paused: one band or one file per tick
    let t0 = Instant::now();
    if st.store.flush_step(now, false) {
        st.stats.slowest_save = st.stats.slowest_save.max(t0.elapsed());
    }
    if st.export.is_some() {
        let share = if st.paused { budget } else { budget / 2 };
        while export_step(st, &server, now) && began.elapsed() < share {}
    }
    if !st.paused {
        render(st, &server, now, began, budget);
    }
    let spent = began.elapsed();
    st.stats.slowest_tick = st.stats.slowest_tick.max(spent);
    if spent > Duration::from_micros(200) {
        st.stats.busy_ticks += 1;
        if spent > budget + Duration::from_millis(1) {
            st.stats.over_ticks += 1;
        }
    }
    // The live page works while mapping is paused, with its own small budget
    serve(st, &server, now, budget.saturating_sub(spent));
}

/// A block is about to change at (`x`, `z`): draw its chunk again soon.
fn mark(dim: Option<String>, x: i32, z: i32) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    let due = st.tick + DIRTY_DELAY;
    let ids: Vec<u8> = st.dims.iter().filter(|d| dim.as_ref().is_none_or(|n| *n == d.name)).map(|d| d.id).collect();
    for id in ids {
        let mut keys = vec![(id, x >> 4, z >> 4)];
        if z & 15 == 15 {
            // The row south of it is shaded against this one
            keys.push((id, x >> 4, (z >> 4) + 1));
        }
        for key in keys {
            if st.delayed.len() < MAX_DELAYED && !st.delayed.iter().any(|(_, k)| *k == key) {
                st.delayed.push((due, key));
            }
        }
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

struct OnPlace;
impl EventHandler<BlockPlaceEvent> for OnPlace {
    fn handle(&self, _server: Server, e: Data<BlockPlaceEvent>) -> Data<BlockPlaceEvent> {
        if !e.cancelled && !c::awaiting_login(&e.player) {
            mark(Some(e.player.get_world().get_dimension()), e.block_pos.x, e.block_pos.z);
        }
        e
    }
}

struct OnBreak;
impl EventHandler<BlockBreakEvent> for OnBreak {
    fn handle(&self, _server: Server, e: Data<BlockBreakEvent>) -> Data<BlockBreakEvent> {
        if !e.cancelled && !e.player.as_ref().is_some_and(c::awaiting_login) {
            // No player (an explosion, a plugin): the event does not say which world, so every mapped one
            mark(e.player.as_ref().map(|p| p.get_world().get_dimension()), e.block_pos.x, e.block_pos.z);
        }
        e
    }
}

#[cfg(feature = "probe")]
mod probe {
    //! Lab only: which chunk events does the server fire?
    use std::sync::atomic::{AtomicU64, Ordering};

    use pumpkin_plugin_api::Server;
    use pumpkin_plugin_api::events::{ChunkLoadEvent, ChunkPopulateEvent, ChunkSaveEvent, ChunkSendEvent, ChunkUnloadEvent, EventHandler, FromIntoEvent};

    pub static COUNT: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
    pub const NAMES: [&str; 5] = ["load", "save", "send", "populate", "unload"];

    pub struct Seen<const N: usize>;
    macro_rules! seen {
        ($n:literal, $event:ty) => {
            impl EventHandler<$event> for Seen<$n> {
                fn handle(&self, _server: Server, e: <$event as FromIntoEvent>::Data) -> <$event as FromIntoEvent>::Data {
                    COUNT[$n].fetch_add(1, Ordering::Relaxed);
                    e
                }
            }
        };
    }
    seen!(0, ChunkLoadEvent);
    seen!(1, ChunkSaveEvent);
    seen!(2, ChunkSendEvent);
    seen!(3, ChunkPopulateEvent);
    seen!(4, ChunkUnloadEvent);

    /// `/worldmap probe <x> <z> [dimension]`: what the server says about one column.
    pub fn column(st: &mut super::State, server: &Server, w: &[String]) -> Vec<String> {
        use pumpkin_plugin_api::wit::pumpkin::plugin::common::BlockPos;
        let (Some(x), Some(z)) = (w.get(1).and_then(|s| s.parse::<i32>().ok()), w.get(2).and_then(|s| s.parse::<i32>().ok())) else {
            return vec!["&cprobe <x> <z> [dimension]".into()];
        };
        let want = w.get(3).map_or("overworld".to_owned(), |d| crate::cfg::dim_dir(d));
        let Some((name, world)) = super::worlds(server).into_iter().find(|(n, _)| crate::cfg::dim_dir(n) == want) else { return vec!["&cno such world".into()] };
        let top = world.get_top_block_y(x, z);
        let mut out = vec![format!(
            "&d{name} {x},{z}: loaded={} top={top} motion={} min_y={} sea={}",
            world.get_chunk(x >> 4, z >> 4).is_some(),
            world.get_motion_blocking_height(x, z),
            world.get_min_y(),
            world.get_sea_level()
        )];
        for y in (top - 2..=top + 3).rev() {
            let id = world.get_block_state_id(BlockPos { x, y, z });
            let name = pumpkin_plugin_api::world::get_block_state_by_id(id).map_or("?".to_owned(), |s| format!("{} color={} air={} solid={} liquid={}", s.block_name, s.map_color, s.is_air, s.is_solid, s.is_liquid));
            out.push(format!("&d y={y} state={id} sky={} {name} -> {:?}", world.get_sky_light(BlockPos { x, y, z }), st.cache.info(id)));
        }
        let t0 = std::time::Instant::now();
        let mut hit = None;
        for _ in 0..100 {
            hit = world.ray_trace_block((f64::from(x) + 0.5, 320.0, f64::from(z) + 0.5), (f64::from(x) + 0.5, f64::from(top) + 1.0, f64::from(z) + 0.5), true);
        }
        let ray = t0.elapsed();
        let t0 = std::time::Instant::now();
        for _ in 0..1000 {
            let _ = world.get_sky_light(BlockPos { x, y: top + 1, z });
        }
        let sky = t0.elapsed();
        let t0 = std::time::Instant::now();
        for _ in 0..1000 {
            let _ = world.get_block_state_id(BlockPos { x, y: top + 1, z });
        }
        let state = t0.elapsed();
        let t0 = std::time::Instant::now();
        for _ in 0..1000 {
            let _ = world.get_top_block_y(x, z);
        }
        let topt = t0.elapsed();
        let t0 = std::time::Instant::now();
        for _ in 0..1000 {
            let _ = world.get_chunk(x >> 4, z >> 4);
        }
        let chunk = t0.elapsed();
        out.push(format!(
            "&d ray from 320 down: {:?} · per call: ray {:.2} us, sky {:.3} us, state {:.3} us, top {:.3} us, get_chunk {:.3} us",
            hit.map(|h| (h.pos.x, h.pos.y, h.pos.z)),
            ray.as_secs_f64() * 1e4,
            sky.as_secs_f64() * 1e3,
            state.as_secs_f64() * 1e3,
            topt.as_secs_f64() * 1e3,
            chunk.as_secs_f64() * 1e3
        ));
        if let Some(dim) = st.dims.iter().find(|d| d.name == name) {
            let (y, info) = super::surface(&mut st.cache, &world, dim, x, z);
            out.push(format!("&d surface y={y} {info:?} biome={:?}", world.get_biome(BlockPos { x, y, z })));
        }
        out
    }

    pub fn line() -> String {
        let parts: Vec<String> = NAMES.iter().zip(&COUNT).map(|(n, c)| format!("{n}={}", c.load(Ordering::Relaxed))).collect();
        format!("&dchunk events: {}", parts.join(" "))
    }
}

/// Bytes of the exported pages in the data folder.
fn pages_bytes() -> u64 {
    let Ok(list) = std::fs::read_dir(c::data_path("")) else { return 0 };
    list.flatten().filter(|f| f.file_name().to_string_lossy().ends_with(".html")).filter_map(|f| f.metadata().ok()).map(|m| m.len()).sum()
}

/// The live page in one line.
fn web_line(st: &State, th: bool) -> String {
    let port = st.cfg.web_port;
    match (&st.site.web, &st.site.error) {
        (Some(w), _) => {
            if th {
                format!("&7 เว็บแผนที่: &aเปิดที่ port {port}&7 (http://<ที่อยู่เซิร์ฟ>:{port}) · เชื่อมต่ออยู่ {} · ตอบแล้ว {} คำขอ ({})", w.open(), commas(w.served), cfg::size(w.sent))
            } else {
                format!("&7 live page: &aopen on port {port}&7 (http://<server address>:{port}) · {} connections · {} requests answered ({})", w.open(), commas(w.served), cfg::size(w.sent))
            }
        }
        (None, Some(e)) => {
            if th {
                format!("&7 เว็บแผนที่: &cเปิด port {port} ไม่ได้ ({e}) &7ใช้พอร์ตเสริมของเซิร์ฟที่ยังว่าง แล้วสั่ง /worldmap web <port>")
            } else {
                format!("&7 live page: &cport {port} could not be opened ({e}) &7use a free extra port of the server: /worldmap web <port>")
            }
        }
        (None, None) => {
            if th {
                "&7 เว็บแผนที่: &8ปิด &7(เปิดด้วย /worldmap web <port> โดยใช้พอร์ตเสริมของเซิร์ฟ)".into()
            } else {
                "&7 live page: &8off &7(/worldmap web <port> with one of the server's extra ports turns it on)".into()
            }
        }
    }
}

fn status(st: &State, th: bool, debug: bool) -> Vec<String> {
    let mut out = Vec::new();
    let state = match (st.paused, th) {
        (true, true) => "&eหยุดชั่วคราว",
        (true, false) => "&epaused",
        (false, true) => "&aทำงานอยู่",
        (false, false) => "&arunning",
    };
    out.push(msg(th, &format!("&fWorldMap {VERSION} {state}"), &format!("&fWorldMap {VERSION} {state}")));
    for d in &st.store.dims {
        let live = if st.dims.iter().any(|x| x.name == d.name) { "" } else if th { " &8(ไม่ได้ทำแผนที่ตอนนี้)" } else { " &8(not being mapped now)" };
        let id = st.store.dims.iter().position(|x| x.name == d.name).unwrap_or(0) as u8;
        let (chunks, tiles) = st.store.mapped(id);
        out.push(format!("&7 {}: &f{} &7chunks, {tiles} tiles, {}{live}", short(&d.name), commas(chunks), cfg::size(d.bytes())));
    }
    if st.store.dims.is_empty() {
        out.push(if th { "&7 ยังไม่มีแผนที่".into() } else { "&7 nothing mapped yet".into() });
    }
    let waiting = st.queue.len() + st.delayed.len() + usize::from(st.job.is_some());
    out.push(if th {
        format!("&7 คิว: &f{}&7 chunks · tile ในหน่วยความจำ {}/{} (รอบันทึก {})", commas(waiting as u64), st.store.in_memory(), st.store.max_tiles, st.store.dirty_tiles())
    } else {
        format!("&7 queue: &f{}&7 chunks · tiles in memory {}/{} ({} to save)", commas(waiting as u64), st.store.in_memory(), st.store.max_tiles, st.store.dirty_tiles())
    });
    if let Some(a) = &st.area {
        let kind = if a.wait { "wait" } else { "once" };
        out.push(if th {
            format!("&7 งาน render: รัศมี {} chunks ({kind}) วาดแล้ว {} เหลือรอ {}", a.r, commas(u64::from(a.drawn)), commas(u64::from(a.left)))
        } else {
            format!("&7 render job: radius {} chunks ({kind}), {} mapped, {} waiting", a.r, commas(u64::from(a.drawn)), commas(u64::from(a.left)))
        });
    }
    if let Some(e) = &st.export {
        out.push(if th { format!("&7 กำลัง export {}: tile {}/{}", e.file, e.at, e.tiles.len()) } else { format!("&7 exporting {}: tile {}/{}", e.file, e.at, e.tiles.len()) });
    }
    let (tiles, pages) = (st.store.disk_bytes(), pages_bytes());
    out.push(if th {
        format!("&7 ดิสก์: &f{}&7 (tiles {} + หน้า export {})", cfg::size(tiles + pages), cfg::size(tiles), cfg::size(pages))
    } else {
        format!("&7 disk: &f{}&7 (tiles {} + exported pages {})", cfg::size(tiles + pages), cfg::size(tiles), cfg::size(pages))
    });
    out.push(web_line(st, th));
    let s = &st.stats;
    let avg = if s.drawn == 0 { 0.0 } else { s.draw_time.as_secs_f64() * 1000.0 / s.drawn as f64 };
    out.push(if th {
        format!("&7 ตั้งแต่เปิด: วาด {} chunks (ภาพเดิม {}) เฉลี่ย {avg:.2} ms/chunk ช้าสุด {:.2} ms · tick ที่ใช้นานสุด {:.2} ms (งบ {} ms)", commas(s.drawn), commas(s.unchanged), s.slowest_chunk.as_secs_f64() * 1000.0, s.slowest_tick.as_secs_f64() * 1000.0, st.cfg.tick_ms)
    } else {
        format!("&7 since start: {} chunks drawn ({} unchanged), {avg:.2} ms/chunk on average, slowest {:.2} ms · longest tick {:.2} ms (budget {} ms)", commas(s.drawn), commas(s.unchanged), s.slowest_chunk.as_secs_f64() * 1000.0, s.slowest_tick.as_secs_f64() * 1000.0, st.cfg.tick_ms)
    });
    if !debug {
        return out;
    }
    let per = if s.probes == 0 { 0.0 } else { s.probe_time.as_secs_f64() * 1e6 / s.probes as f64 };
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    out.push(format!(
        "&8 busy ticks {} (over budget by 1 ms+: {}) · probes {} ({per:.2} us) · not loaded {} · tiles saved {} · save errors {} · slowest save step {:.2} ms, player scan {:.2} ms, export step {:.2} ms, live page {:.2} ms (refused {})",
        commas(s.busy_ticks),
        commas(s.over_ticks),
        commas(s.probes),
        commas(s.not_loaded),
        st.store.saved,
        st.store.failed,
        ms(s.slowest_save),
        ms(s.slowest_scan),
        ms(s.slowest_export),
        ms(s.slowest_web),
        st.site.web.as_ref().map_or(0, |w| w.refused)
    ));
    #[cfg(feature = "probe")]
    out.push(probe::line());
    out
}

fn help(th: bool) -> Vec<String> {
    let lines: &[(&str, &str)] = &[
        ("&bWorldMap &7แผนที่โลก 2 มิติมองจากด้านบน (1 จุด = 1 บล็อก) ของทุก chunk ที่ถูกโหลดระหว่างที่ปลั๊กอินทำงาน", "&bWorldMap &7a 2D top-down map (1 pixel = 1 block) of every chunk that was loaded while the plugin ran"),
        ("&7ไม่ใช่ 3 มิติ และโหลด chunk เองไม่ได้ · ดูแผนที่ได้สองทาง: เว็บแผนที่สด (/worldmap web) หรือไฟล์ HTML (/worldmap export)", "&7Not 3D, and it cannot load chunks itself · two ways to look at the map: the live page (/worldmap web) or one HTML file (/worldmap export)"),
        ("&f/worldmap web <port> &7เปิดเว็บแผนที่สดที่ http://<ที่อยู่เซิร์ฟ>:<port> (ใช้พอร์ตเสริมของเซิร์ฟ ใครมีลิงก์ก็เปิดดูได้) · &f/worldmap web off", "&f/worldmap web <port> &7serve the live map at http://<server address>:<port> (one of the server's extra ports; anyone with the link can look) · &f/worldmap web off"),
        ("&f/worldmap status &7จำนวน chunk ต่อมิติ คิว และพื้นที่ดิสก์", "&f/worldmap status &7chunks per dimension, queue, disk use"),
        ("&f/worldmap render [รัศมี] &7วาด chunk ที่โหลดอยู่รอบตัว (หน่วย chunk, สูงสุด 512)", "&f/worldmap render [radius] &7map the loaded chunks around you (in chunks, up to 512)"),
        ("&f/worldmap render <รัศมี> wait &7รอ chunk ที่ยังไม่โหลด ใช้คู่กับ Pregen · &f/worldmap render stop", "&f/worldmap render <radius> wait &7also wait for chunks that are not loaded yet (pair it with Pregen) · &f/worldmap render stop"),
        ("&f/worldmap export [overworld|nether|end] &7สร้าง plugins/data/worldmap/map.html", "&f/worldmap export [overworld|nether|end] &7write plugins/data/worldmap/map.html"),
        ("&f/worldmap pause &7| &fresume &7| &freload &7| &fversion", "&f/worldmap pause &7| &fresume &7| &freload &7| &fversion"),
    ];
    lines.iter().map(|(t, e)| (if th { *t } else { *e }).to_owned()).collect()
}

struct Cmd;

impl Cmd {
    fn render(st: &mut State, sender: &CommandSender, server: &Server, w: &[String], th: bool) -> String {
        if w.get(1).is_some_and(|s| s == "stop") {
            return match st.area.take() {
                Some(a) => msg(th, &format!("&eหยุดงาน render แล้ว (วาดไป {} chunks)", commas(u64::from(a.drawn))), &format!("&eRender job stopped ({} chunks mapped)", commas(u64::from(a.drawn)))),
                None => msg(th, "&7ไม่มีงาน render อยู่", "&7No render job is running"),
            };
        }
        if st.paused {
            return msg(th, "&eWorldMap หยุดชั่วคราวอยู่ สั่ง /worldmap resume ก่อน", "&eWorldMap is paused: /worldmap resume first");
        }
        if st.area.is_some() {
            return msg(th, "&eมีงาน render อยู่แล้ว ดูที่ /worldmap status หรือหยุดด้วย /worldmap render stop", "&eA render job is already running: see /worldmap status, or /worldmap render stop");
        }
        let default = st.cfg.around_players.max(8);
        let radius = match w.get(1) {
            None => default,
            Some(s) => match s.parse::<u32>() {
                Ok(r) if (1..=MAX_RADIUS).contains(&r) => r,
                _ => return msg(th, &format!("&cรัศมีต้องเป็นจำนวน chunk 1 ถึง {MAX_RADIUS}"), &format!("&cThe radius is a number of chunks from 1 to {MAX_RADIUS}")),
            },
        };
        let wait = match w.get(2).map(String::as_str) {
            None => false,
            Some("wait") => true,
            Some(_) => return msg(th, "&cใช้: /worldmap render [รัศมี] [wait]", "&cUsage: /worldmap render [radius] [wait]"),
        };
        if st.dims.is_empty() {
            refresh_dims(st, server);
        }
        // Around the player, or around the world spawn for the console
        let place = match c::sender_player(sender) {
            Some(p) => {
                let name = p.get_world().get_dimension();
                let (x, _, z) = p.get_position();
                st.dims.iter().find(|d| d.name == name).map(|d| (d.id, (x.floor() as i32) >> 4, (z.floor() as i32) >> 4))
            }
            None => st.dims.first().map(|d| {
                let spawn = st.store.dims[d.id as usize].spawn;
                (d.id, spawn[0] >> 4, spawn[2] >> 4)
            }),
        };
        let Some((dim, cx, cz)) = place else {
            return msg(th, "&cมิตินี้ไม่ได้อยู่ในรายการ dimensions ของ config", "&cThis dimension is not in the config's dimensions list");
        };
        let n = square(radius);
        let now = c::now_secs();
        st.area = Some(Area { dim, cx, cz, r: radius, n, cursor: 0, pass: 0, wait, pending: vec![0; n.div_ceil(64) as usize], left: 0, drawn: 0, absent: 0, who: who(sender), started: now, progress: now, empty_rounds: 0, hot: Vec::new() });
        let name = short(&st.store.dims[dim as usize].name).to_owned();
        if wait {
            msg(
                th,
                &format!("&7เริ่มทำแผนที่ {} chunks รอบ chunk {cx}, {cz} ({name}) และรอ chunk ที่ยังไม่โหลด", commas(u64::from(n))),
                &format!("&7Mapping {} chunks around chunk {cx}, {cz} ({name}), waiting for the ones that are not loaded", commas(u64::from(n))),
            )
        } else {
            msg(th, &format!("&7เริ่มทำแผนที่ {} chunks รอบ chunk {cx}, {cz} ({name})", commas(u64::from(n))), &format!("&7Mapping {} chunks around chunk {cx}, {cz} ({name})", commas(u64::from(n))))
        }
    }

    /// `/worldmap web [port|off]`: the setting is saved, and the page opens, moves or closes at once.
    fn web(st: &mut State, w: &[String], th: bool) -> String {
        let port = match w.get(1).map(|s| s.to_ascii_lowercase()).as_deref() {
            None => return web_line(st, th).replacen("&7 ", "&8[&bWorldMap&8]&7 ", 1),
            Some("off" | "0") => 0,
            Some(text) => match text.parse::<u16>() {
                Ok(p) if p >= 1024 => p,
                _ => return msg(th, "&cport ต้องเป็นเลข 1024 ถึง 65535 และต้องเป็นพอร์ตเสริมของเซิร์ฟนี้ (ดูที่หน้าภาพรวมของเซิร์ฟ) หรือ off เพื่อปิด", "&cThe port is a number from 1024 to 65535, one of this server's extra ports, or off"),
            },
        };
        st.cfg.web_port = port;
        let saved = c::save_config("config.toml", &st.cfg);
        web_apply(st);
        let mut line = match (port, &st.site.error) {
            (0, _) => msg(th, "&eปิดเว็บแผนที่แล้ว", "&eThe live map page is off"),
            (_, Some(e)) => msg(th, &format!("&cเปิด port {port} ไม่ได้: {e} &7(port นี้อาจมีอย่างอื่นใช้อยู่ หรือไม่ใช่พอร์ตเสริมของเซิร์ฟ)"), &format!("&cPort {port} could not be opened: {e} &7(something else may be using it, or it is not one of the server's extra ports)")),
            (_, None) => msg(th, &format!("&aเปิดเว็บแผนที่แล้ว: &fhttp://<ที่อยู่เซิร์ฟ>:{port} &7(ใครมีลิงก์ก็เปิดดูได้ ไม่ต้องล็อกอิน)"), &format!("&aThe live map page is open: &fhttp://<server address>:{port} &7(anyone with the link can look, no login)")),
        };
        if !saved {
            line.push_str(if th { " &c(บันทึก config.toml ไม่ได้ ค่านี้จะหายเมื่อรีสตาร์ต)" } else { " &c(config.toml could not be written: this is lost at the next restart)" });
        }
        line
    }

    fn export(st: &mut State, sender: &CommandSender, server: &Server, w: &[String], th: bool) -> String {
        if st.export.is_some() {
            return msg(th, "&eกำลัง export อยู่ รอให้เสร็จก่อน", "&eAn export is already running");
        }
        // The dimension asked for, else the one the player is in, else the first
        let wanted = match w.get(1) {
            Some(arg) => Some(dim_dir(arg)),
            None => c::sender_player(sender).map(|p| dim_dir(&p.get_world().get_dimension())).filter(|d| st.store.dims.iter().any(|x| x.dir == *d)),
        };
        let found = match &wanted {
            Some(dir) => st.store.dims.iter().position(|d| d.dir == *dir),
            None => (!st.store.dims.is_empty()).then_some(0),
        };
        let Some(dim) = found else {
            let known: Vec<&str> = st.store.dims.iter().map(|d| d.dir.as_str()).collect();
            return msg(th, &format!("&cยังไม่มีแผนที่ของมิตินั้น (ที่มี: {})", known.join(", ")), &format!("&cThere is no map of that dimension (mapped: {})", known.join(", ")));
        };
        let d = &st.store.dims[dim];
        if d.tiles.is_empty() && st.store.dirty_in(dim as u8) == 0 {
            return msg(th, "&cมิตินี้ยังไม่มี chunk ที่ทำแผนที่", "&cNothing of this dimension has been mapped yet");
        }
        let players = if st.cfg.export_players {
            server
                .get_all_players()
                .into_iter()
                .filter(|p| !c::awaiting_login(p) && p.get_world().get_dimension() == d.name)
                .map(|p| {
                    let (x, y, z) = p.get_position();
                    Marker { name: p.get_name(), x: (x * 10.0).round() / 10.0, y: (y * 10.0).round() / 10.0, z: (z * 10.0).round() / 10.0 }
                })
                .collect()
        } else {
            Vec::new()
        };
        let file = export::file_name(&d.dir);
        // The server gives every dimension the overworld's spawn point: only show it there
        let spawn = (d.dir == "overworld").then_some(d.spawn);
        let meta = Meta { th, dim: d.name.clone(), version: VERSION.into(), generated: c::now_secs(), scale: 1, spawn, players, chunks: d.chunks(), palette: color::palette_hex() };
        st.export = Some(Export {
            dim: dim as u8,
            who: who(sender),
            phase: Phase::Flush,
            scale: 1,
            tiles: Vec::new(),
            at: 0,
            bytes: 0,
            left_out: 0,
            cap: u64::from(st.cfg.export_max_mb) * 1024 * 1024,
            tmp: c::data_path(&format!("{file}.tmp")),
            out: c::data_path(&file),
            file: file.clone(),
            meta,
            started: Instant::now(),
            part: None,
        });
        msg(th, &format!("&7กำลังสร้าง plugins/data/worldmap/{file} …"), &format!("&7Writing plugins/data/worldmap/{file} …"))
    }
}

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let Some(mut guard) = try_lock() else { return Ok(0) };
        let Some(st) = guard.as_mut() else { return Ok(0) };
        let th = st.cfg.language != "en";
        let lines = match w.first().map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("status") => status(st, th, w.get(1).is_some_and(|s| s == "debug")),
            Some("render") => vec![Cmd::render(st, &sender, &server, &w, th)],
            Some("export") => vec![Cmd::export(st, &sender, &server, &w, th)],
            Some("pause") => {
                st.paused = true;
                vec![msg(th, "&eหยุดทำแผนที่ชั่วคราว (แผนที่ที่มีอยู่ยังอยู่ครบ) · ทำต่อด้วย /worldmap resume", "&eMapping paused (the map so far is kept) · /worldmap resume continues")]
            }
            Some("resume") => {
                st.paused = false;
                vec![msg(th, "&aทำแผนที่ต่อแล้ว", "&aMapping resumed")]
            }
            Some("web") => vec![Cmd::web(st, &w, th)],
            Some("reload") => {
                st.cfg = load();
                st.store.max_tiles = st.cfg.max_tiles_in_memory;
                st.settled.clear();
                refresh_dims(st, &server);
                web_apply(st);
                let th = st.cfg.language != "en";
                let dims: Vec<&str> = st.dims.iter().map(|d| short(&d.name)).collect();
                vec![msg(th, &format!("&aโหลดไฟล์ตั้งค่าใหม่แล้ว (ทำแผนที่: {})", dims.join(", ")), &format!("&aConfiguration reloaded (mapping: {})", dims.join(", ")))]
            }
            Some("version") => vec![msg(th, &format!("&fWorldMap {VERSION}"), &format!("&fWorldMap {VERSION}"))],
            #[cfg(feature = "probe")]
            Some("probe") => probe::column(st, &server, &w),
            _ => help(th),
        };
        for line in lines {
            c::reply(&sender, &line);
        }
        Ok(1)
    }
}

struct WorldMap;

impl Plugin for WorldMap {
    fn new() -> Self {
        WorldMap
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "World map by MCSV".into(),
            dependencies: vec![],
            // TCP bind: the live map page (`web-port`); nothing is listened on while it is 0
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into(), NETWORK_TCP_BIND.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let cfg = load();
        let store = Store::open(&c::data_path(""), VERSION, cfg.max_tiles_in_memory);
        let mut st = State {
            cfg,
            paused: false,
            tick: 0,
            store,
            dims: Vec::new(),
            cache: Cache { info: Vec::new(), biomes: [255; 256] },
            queue: VecDeque::new(),
            queued: HashSet::new(),
            delayed: Vec::new(),
            centres: VecDeque::new(),
            settled: HashMap::new(),
            job: None,
            area: None,
            export: None,
            stats: Stats::default(),
            site: Site { web: None, error: None, page: Arc::new(Vec::new()), page_tag: String::new(), index: None, players: Arc::new(b"{\"players\":[]}".to_vec()) },
            loaded_at: c::now_secs(),
        };
        // Opens the live page if a port is set, and writes status.json
        web_apply(&mut st);
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(st);
        let _ = context.register_permission(&Permission {
            node: format!("{NS}:command"),
            description: "WorldMap".into(),
            default: PermissionDefault::Op(PermissionLevel::Two),
            children: vec![],
        });
        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let cmd = Command::new(&[NS.to_owned()], "WorldMap")
            .execute(Cmd)
            .then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:command"));
        // Observers only: they run after the handlers that may cancel
        context.register_event_handler(OnPlace, EventPriority::Normal, false)?;
        context.register_event_handler(OnBreak, EventPriority::Normal, false)?;
        #[cfg(feature = "probe")]
        {
            context.register_event_handler(probe::Seen::<0>, EventPriority::Normal, false)?;
            context.register_event_handler(probe::Seen::<1>, EventPriority::Normal, false)?;
            context.register_event_handler(probe::Seen::<2>, EventPriority::Normal, false)?;
            context.register_event_handler(probe::Seen::<3>, EventPriority::Normal, false)?;
            context.register_event_handler(probe::Seen::<4>, EventPriority::Normal, false)?;
        }
        pumpkin_plugin_api::scheduler::schedule_repeating_task(1, 1, on_tick);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        // Everything drawn since the last save goes to disk now
        if let Some(st) = STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut() {
            st.store.flush_all(c::now_secs());
            st.site.web = None;
            if let Some(e) = st.export.take() {
                let _ = std::fs::remove_file(&e.tmp);
            }
        }
        c::remove_file("status.json");
        Ok(())
    }
}

register_plugin!(WorldMap);

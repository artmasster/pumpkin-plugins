//! Pumpkin WASM glue for ImageMap: making a poster in tick slices, giving the items, and
//! sending the pictures to players.
//!
//! How a picture reaches a client: a map item only carries a number (the `map_id` component);
//! what the map shows comes in a separate packet. Pumpkin sends that packet only for the maps
//! it draws itself (the ones made from an empty map). In Pumpkin 0.2.2 a plugin cannot send it
//! through `send-packet` either: the map packet is in the plugin API but the host does not
//! write it out. What does work is Pumpkin's own "map" block entity, whose `stream_frame`
//! broadcasts a picture under any map number to everyone online. So this plugin keeps one such
//! block entity at the bottom of the world under a player (it is never shown to clients, only
//! saved with the chunk) and pushes its maps through it: a new poster at once, every map again
//! a little after someone joins (up to `join-burst-maps`), and one map per second in turn, so
//! that a latecomer on a server with a great many maps has them all after a while.
//! The numbers start at `first-map-id`, far above the ones Pumpkin hands out.
//!
//! Making a poster never holds up a tick: the download is polled, a PNG is decoded a few rows
//! at a time, and scaling plus colour matching run row by row under `tick-ms`. (A JPEG is
//! decoded in one call, at reduced size when it is much larger than the poster.)
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use mcsv_common as c;
use mcsv_common::http::{self, Pending, Request};
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, PlayerJoinEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA, HTTP_OUTBOUND, NETWORK_DNS};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::wit::pumpkin::plugin::block_entity::BlockEntityType;
use pumpkin_plugin_api::wit::pumpkin::plugin::common::BlockPos;
use pumpkin_plugin_api::wit::pumpkin::plugin::data_components::DataComponent;
use pumpkin_plugin_api::{Context, ItemStack, Plugin, PluginMetadata, Server, register_plugin};

use crate::cfg::{self, Config};
use crate::palette::Quantizer;
use crate::picture::{self, Kind, MAP, Mode, PngDecode, Scaler};
use crate::store::{MAP_BYTES, Poster, Store};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NS: &str = "imagemap";
const P: &str = "&8[&bImageMap&8]&r ";
/// Pictures waiting behind the one being made.
const MAX_QUEUE: usize = 4;
/// Map pictures kept in memory for sending (16 KB each).
const CACHE_MAPS: usize = 128;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
enum Source {
    Url(String),
    /// A file in `plugins/data/imagemap/images/`.
    File(String),
}

/// What was asked for.
#[derive(Clone)]
struct Order {
    /// Who asked (`None` = the console) and who will own the poster (lowercase; empty = console).
    who: Option<String>,
    owner: String,
    admin: bool,
    source: Source,
    size: Option<(u32, u32)>,
    mode: Mode,
    name: Option<String>,
}

enum Stage {
    Download(Pending),
    Png(Box<PngDecode>),
    Paint { scaler: Box<Scaler>, quant: Box<Quantizer>, y: usize, cols: u32, rows: u32, maps: Vec<Vec<u8>> },
}

struct Job {
    order: Order,
    stage: Stage,
    started: Instant,
}

struct State {
    cfg: Config,
    store: Store,
    job: Option<Job>,
    queue: VecDeque<Order>,
    /// Map numbers waiting to be broadcast.
    outbox: VecDeque<i32>,
    /// Where our map block entity is: dimension and position.
    sender: Option<(String, BlockPos)>,
    tick: u64,
    /// Tick at which every map is queued again (someone joined a moment before).
    burst_at: Option<u64>,
    /// Where the one-a-second round is.
    round: usize,
    /// Map pictures read lately, newest last.
    cache: VecDeque<(i32, Vec<u8>)>,
    /// Lowercase name → Unix second until which a new picture is refused.
    cooldown: HashMap<String, u64>,
    made: u64,
    sent: u64,
    /// Longest time one tick spent in this plugin.
    slowest: Duration,
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
    format!("{P}{}", if th { thai } else { english })
}

fn say(server: &Server, who: &Option<String>, line: &str) {
    match who.as_deref().and_then(|name| server.get_player_by_name(name)) {
        Some(p) => c::tell(&p, line),
        None => c::log_info(&c::plain(line)),
    }
}

fn varint(mut v: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(5);
    loop {
        let b = (v as u32 & 0x7F) as u8;
        v = ((v as u32) >> 7) as i32;
        if v == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

fn map_bytes(st: &mut State, id: i32) -> Option<Vec<u8>> {
    if let Some(at) = st.cache.iter().position(|(i, _)| *i == id) {
        let entry = st.cache.remove(at)?;
        st.cache.push_back(entry);
        return st.cache.back().map(|(_, b)| b.clone());
    }
    let bytes = st.store.read_map(id)?;
    if st.cache.len() >= CACHE_MAPS {
        st.cache.pop_front();
    }
    st.cache.push_back((id, bytes.clone()));
    Some(bytes)
}

/// Network NBT of a "map" block entity: `{id: "minecraft:map", MapId: 0}`.
fn sender_nbt() -> Vec<u8> {
    let mut out = vec![0x0A];
    let mut text = |name: &str, value: &str| {
        out.push(8);
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(value.len() as u16).to_be_bytes());
        out.extend_from_slice(value.as_bytes());
    };
    text("id", "minecraft:map");
    out.push(3);
    out.extend_from_slice(&5u16.to_be_bytes());
    out.extend_from_slice(b"MapId");
    out.extend_from_slice(&0i32.to_be_bytes());
    out.push(0);
    out
}

/// Broadcast one map to everyone online through our map block entity. False when it is not
/// there yet (it is put under a player now and can be used from the next tick on), when nobody
/// is online, or when the map's file is gone.
fn broadcast_map(st: &mut State, server: &Server, id: i32) -> bool {
    if let Some((dim, pos)) = st.sender.clone() {
        let world = server.get_all_worlds().into_iter().find(|w| w.get_dimension() == dim);
        if let Some(BlockEntityType::MapBlockEntity(map)) = world.and_then(|w| w.get_block_entity(pos)) {
            let Some(data) = map_bytes(st, id) else { return false };
            map.set_map_id(id);
            map.stream_frame(&data);
            st.sent += 1;
            return true;
        }
    }
    // Not there (first use, or its chunk is no longer loaded): under the first player online
    let Some(p) = server.get_all_players().into_iter().next() else { return false };
    let world = p.get_world();
    let (x, _, z) = p.get_position();
    let pos = BlockPos { x: x.floor() as i32, y: world.get_min_y(), z: z.floor() as i32 };
    if world.set_block_entity_nbt(pos, &sender_nbt()).is_ok() {
        st.sender = Some((world.get_dimension(), pos));
    }
    false
}

fn free_slots(p: &Player) -> Vec<u8> {
    (0u8..36).filter(|s| p.get_inventory_item(*s).is_none_or(|i| i.get_count() == 0 || i.get_registry_key().ends_with("air"))).collect()
}

/// Put the map items of a poster into a player's inventory, top left first. Returns how many did not fit.
fn give(p: &Player, poster: &Poster) -> usize {
    let mut free = free_slots(p);
    free.reverse();
    let mut missed = 0;
    for (i, id) in poster.maps.iter().enumerate() {
        let Some(slot) = free.pop() else {
            missed += 1;
            continue;
        };
        let stack = ItemStack::new("minecraft:filled_map", 1);
        stack.set_component(DataComponent::MapId, &varint(*id));
        let label = if poster.maps.len() == 1 { format!("&f{}", poster.name) } else { format!("&f{} &7(แถว {} ช่อง {})", poster.name, i as u32 / poster.cols + 1, i as u32 % poster.cols + 1) };
        stack.set_custom_name(Some(c::color(&label)));
        p.set_inventory_item(slot, Some(stack));
    }
    missed
}

/// Begin an order: start the download, or read the file.
fn begin(st: &mut State, order: Order) -> Result<Job, String> {
    let max_bytes = st.cfg.max_image_mb as usize * 1024 * 1024;
    let stage = match &order.source {
        Source::Url(url) => {
            let mut req = Request::get(url).header("user-agent", &format!("PumpkinImageMap/{VERSION}")).header("accept", "image/png,image/jpeg");
            req.timeout = DOWNLOAD_TIMEOUT;
            req.max_body = max_bytes;
            Stage::Download(http::start(req).map_err(|e| e.chars().take(80).collect::<String>())?)
        }
        Source::File(name) => {
            let path = st.store.images_dir().join(name);
            let size = std::fs::metadata(&path).map_err(|_| format!("no file images/{name}"))?.len();
            if size > max_bytes as u64 {
                return Err(format!("the file is larger than {} MB", st.cfg.max_image_mb));
            }
            decode(&st.cfg, &order, std::fs::read(&path).map_err(|e| e.to_string())?)?
        }
    };
    Ok(Job { order, stage, started: Instant::now() })
}

/// The file is here: start decoding it (a JPEG is decoded right now).
fn decode(cfg: &Config, order: &Order, bytes: Vec<u8>) -> Result<Stage, String> {
    let max_pixels = u64::from(cfg.max_megapixels) * 1_000_000;
    match picture::sniff(&bytes) {
        Some(Kind::Png) => Ok(Stage::Png(Box::new(PngDecode::start(bytes, max_pixels)?))),
        Some(Kind::Jpeg) => {
            let (cols, rows) = order.size.unwrap_or((cfg.auto_max_side, cfg.auto_max_side));
            let pic = picture::decode_jpeg(&bytes, max_pixels, (cols as usize * MAP, rows as usize * MAP))?;
            paint(cfg, order, pic)
        }
        None => Err("not a PNG or JPEG picture".into()),
    }
}

/// The picture is decoded: decide the grid and get ready to scale and colour it.
fn paint(cfg: &Config, order: &Order, pic: picture::Picture) -> Result<Stage, String> {
    let (cols, rows) = order.size.unwrap_or_else(|| picture::auto_grid(pic.w, pic.h, cfg.auto_max_side));
    let scaler = Scaler::new(pic, cols, rows, order.mode);
    let quant = Quantizer::new(scaler.width, cfg.dither);
    let maps = vec![vec![0u8; MAP_BYTES]; (cols * rows) as usize];
    Ok(Stage::Paint { scaler: Box::new(scaler), quant: Box::new(quant), y: 0, cols, rows, maps })
}

/// Room for `maps` more maps of this owner? Err = the line to tell them.
fn room(st: &State, order: &Order, maps: usize, th: bool) -> Result<(), String> {
    if st.store.total_maps() + maps > st.cfg.max_maps as usize {
        return Err(msg(th, &format!("&cเซิร์ฟเก็บแผนที่รูปได้ไม่เกิน {} แผ่น (ตอนนี้ {}) ลบของเก่าก่อน", st.cfg.max_maps, st.store.total_maps()), &format!("&cThe server keeps at most {} picture maps ({} now); delete some first", st.cfg.max_maps, st.store.total_maps())));
    }
    if !order.admin && st.store.maps_of(&order.owner) + maps > st.cfg.player_max_maps as usize {
        return Err(msg(th, &format!("&cคุณมีแผนที่รูปได้ไม่เกิน {} แผ่น (ตอนนี้ {}) ลบของเก่าด้วย /imagemap delete <ชื่อ>", st.cfg.player_max_maps, st.store.maps_of(&order.owner)), &format!("&cYou may own at most {} picture maps ({} now); delete some with /imagemap delete <name>", st.cfg.player_max_maps, st.store.maps_of(&order.owner))));
    }
    Ok(())
}

/// One slice of the job. `Ok(true)` = finished (the poster is saved).
fn work(st: &mut State, server: &Server, job: &mut Job, deadline: Instant) -> Result<bool, String> {
    let th = st.cfg.language != "en";
    loop {
        match &mut job.stage {
            Stage::Download(pending) => {
                let Some(result) = pending.poll() else { return Ok(false) };
                let resp = result.map_err(|e| if e.contains("too large") { format!("the picture file is larger than {} MB", st.cfg.max_image_mb) } else { e.chars().filter(|c| !matches!(c, '{' | '}' | '"')).take(70).collect() })?;
                if resp.status != 200 {
                    return Err(format!("the link answered {}", resp.status));
                }
                job.stage = decode(&st.cfg, &job.order, resp.body)?;
            }
            Stage::Png(png) => {
                while Instant::now() < deadline {
                    if png.step(16)? {
                        let Stage::Png(done) = std::mem::replace(&mut job.stage, Stage::Paint { scaler: Box::new(Scaler::new(picture::Picture { w: 1, h: 1, rgba: vec![0; 4] }, 1, 1, Mode::Fit)), quant: Box::new(Quantizer::new(MAP, false)), y: 0, cols: 1, rows: 1, maps: Vec::new() }) else { return Ok(false) };
                        job.stage = paint(&st.cfg, &job.order, done.finish())?;
                        break;
                    }
                }
                if matches!(job.stage, Stage::Png(_)) {
                    return Ok(false);
                }
            }
            Stage::Paint { scaler, quant, y, cols, rows, maps } => {
                if *y == 0 {
                    room(st, &job.order, maps.len(), th).map_err(|line| format!("\u{0}{line}"))?;
                }
                let width = scaler.width;
                let mut rgba = vec![0u8; width * 4];
                let mut bytes = vec![0u8; width];
                while *y < scaler.height {
                    scaler.row(*y, &mut rgba);
                    quant.row(&rgba, &mut bytes);
                    let (map_row, line) = (*y / MAP, *y % MAP);
                    for col in 0..*cols as usize {
                        maps[map_row * *cols as usize + col][line * MAP..(line + 1) * MAP].copy_from_slice(&bytes[col * MAP..(col + 1) * MAP]);
                    }
                    *y += 1;
                    if *y % 8 == 0 && Instant::now() >= deadline {
                        return Ok(false);
                    }
                }
                // Done: numbers, files, the index, the items, and the picture to everyone
                let (cols, rows) = (*cols, *rows);
                let maps = std::mem::take(maps);
                let ids = st.store.take_ids(maps.len());
                for (id, bytes) in ids.iter().zip(&maps) {
                    if !st.store.write_map(*id, bytes) {
                        return Err("the map files could not be written (disk full?)".into());
                    }
                }
                let source = match &job.order.source {
                    Source::Url(u) => u.chars().take(300).collect(),
                    Source::File(f) => format!("images/{f}"),
                };
                let poster = Poster { name: st.store.free_name(job.order.name.as_deref()), owner: job.order.owner.clone(), cols, rows, maps: ids.clone(), created: c::now_secs(), source };
                if !st.store.add(poster.clone()) {
                    return Err("the index could not be written (disk full?)".into());
                }
                st.made += 1;
                st.outbox.extend(ids.iter().copied());
                let secs = job.started.elapsed().as_secs_f32();
                let mut line = msg(
                    th,
                    &format!("&aสร้าง &f{} &aแล้ว: {cols}x{rows} = {} แผ่น ({secs:.1} วินาที)", poster.name, ids.len()),
                    &format!("&aMade &f{}&a: {cols}x{rows} = {} map(s) ({secs:.1} s)", poster.name, ids.len()),
                );
                match job.order.who.as_deref().and_then(|n| server.get_player_by_name(n)) {
                    Some(p) => {
                        let missed = give(&p, &poster);
                        line.push_str(&if missed > 0 {
                            if th { format!(" &eกระเป๋าเต็ม ยังขาด {missed} แผ่น: ทำที่ว่างแล้วพิมพ์ /imagemap get {}", poster.name) } else { format!(" &e{missed} did not fit: make room and run /imagemap get {}", poster.name) }
                        } else if ids.len() > 1 {
                            (if th { " &7ใส่กรอบไอเทมเรียงจากซ้ายบนไปขวา ทีละแถว" } else { " &7hang them in item frames from the top left, row by row" }).to_owned()
                        } else {
                            String::new()
                        });
                    }
                    None => line.push_str(&if th { format!(" &7รับของด้วย /imagemap get {}", poster.name) } else { format!(" &7get the items with /imagemap get {}", poster.name) }),
                }
                say(server, &job.order.who, &line);
                return Ok(true);
            }
        }
    }
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    let th = st.cfg.language != "en";
    let began = Instant::now();
    let deadline = began + Duration::from_millis(u64::from(st.cfg.tick_ms));

    // Pictures being made, one at a time
    if st.job.is_none() {
        if let Some(order) = st.queue.pop_front() {
            match begin(st, order.clone()) {
                Ok(job) => st.job = Some(job),
                Err(why) => say(&server, &order.who, &msg(th, &format!("&cทำแผนที่จากรูปไม่ได้: &f{why}"), &format!("&cThe picture could not be made into maps: &f{why}"))),
            }
        }
    }
    if let Some(mut job) = st.job.take() {
        match work(st, &server, &mut job, deadline) {
            Ok(false) => st.job = Some(job),
            Ok(true) => {}
            // A line that is already whole (a limit) comes marked with a NUL in front
            Err(why) => match why.strip_prefix('\u{0}') {
                Some(line) => say(&server, &job.order.who, line),
                None => say(&server, &job.order.who, &msg(th, &format!("&cทำแผนที่จากรูปไม่ได้: &f{why}"), &format!("&cThe picture could not be made into maps: &f{why}"))),
            },
        }
    }

    // Maps to broadcast: the queue first, else one of the round every second
    st.tick += 1;
    if st.burst_at.is_some_and(|at| st.tick >= at) {
        st.burst_at = None;
        let ids = st.store.all_ids();
        if ids.len() <= st.cfg.join_burst_maps as usize {
            for id in ids {
                if !st.outbox.contains(&id) {
                    st.outbox.push_back(id);
                }
            }
        }
    }
    if !st.outbox.is_empty() {
        for _ in 0..st.cfg.maps_per_tick {
            let Some(id) = st.outbox.front().copied() else { break };
            if !broadcast_map(st, &server, id) && st.sender.is_some() && server.get_player_count() > 0 && map_bytes(st, id).is_some() {
                // The block entity was only just put down: the same map again next tick
                break;
            }
            st.outbox.pop_front();
        }
        if server.get_player_count() == 0 {
            st.outbox.clear();
        }
    } else if st.cfg.round_seconds > 0 && st.tick % (u64::from(st.cfg.round_seconds) * 20) == 0 && server.get_player_count() > 0 {
        let ids = st.store.all_ids();
        if !ids.is_empty() {
            st.round = (st.round + 1) % ids.len();
            let id = ids[st.round];
            broadcast_map(st, &server, id);
        }
    }
    st.slowest = st.slowest.max(began.elapsed());
}

type Data<E> = <E as FromIntoEvent>::Data;

struct OnJoin;
impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _server: Server, e: Data<PlayerJoinEvent>) -> Data<PlayerJoinEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            // Two seconds for the client to be in the world; players who join together share one burst
            if st.burst_at.is_none() && !st.store.posters.is_empty() {
                st.burst_at = Some(st.tick + 40);
            }
        }
        e
    }
}

fn help(th: bool, can_create: bool, admin: bool) -> Vec<String> {
    let mut lines: Vec<(&str, &str)> = vec![("&bImageMap &7เปลี่ยนรูปเป็นแผนที่ ใส่กรอบไอเทมเป็นรูปติดผนังได้", "&bImageMap &7turns a picture into maps to hang in item frames")];
    if can_create {
        lines.push(("&f/tomap <ลิงก์รูป> [กว้างxสูง] [fit|cover|stretch] [ชื่อ] &7เช่น /tomap https://…/logo.png 2x2 · ไม่ใส่ขนาด = ตามสัดส่วนรูป", "&f/tomap <picture link> [WxH] [fit|cover|stretch] [name] &7like /tomap https://…/logo.png 2x2 · no size = the picture's own shape"));
        lines.push(("&7fit = เห็นทั้งรูป (มีขอบว่าง) · cover = เต็มทุกแผ่น (ตัดส่วนเกิน) · stretch = ยืดให้พอดี · รับ PNG และ JPEG", "&7fit = the whole picture (empty bands) · cover = every map filled (edges cut) · stretch = pulled to fit · PNG and JPEG"));
    }
    lines.push(("&f/imagemap list &7รูปของคุณ · &f/imagemap get <ชื่อ> &7รับแผนที่อีกชุด · &f/imagemap delete <ชื่อ> &7· &f/imagemap rename <เดิม> <ใหม่>", "&f/imagemap list &7your pictures · &f/imagemap get <name> &7another set of the maps · &f/imagemap delete <name> &7· &f/imagemap rename <old> <new>"));
    if admin {
        lines.push(("&7ผู้ดูแล: &f/imagemap file <ไฟล์ใน images/> … &7(อัปโหลดรูปไว้ที่ plugins/data/imagemap/images/) · &flist all &7· &fgive <ผู้เล่น> <ชื่อ> &7· &finfo &7· &freload &7· &fversion", "&7Staff: &f/imagemap file <file in images/> … &7(upload pictures to plugins/data/imagemap/images/) · &flist all &7· &fgive <player> <name> &7· &finfo &7· &freload &7· &fversion"));
    }
    lines.iter().map(|(t, e)| format!("{P}{}", if th { *t } else { *e })).collect()
}

/// `/imagemap …`; `/tomap …` is `/imagemap new …`.
struct Cmd {
    tomap: bool,
}

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let mut w = c::words(&args, "args");
        if self.tomap {
            w.insert(0, "new".into());
        }
        let me = c::sender_player(&sender);
        if me.as_ref().is_some_and(c::awaiting_login) {
            return Ok(0);
        }
        let Some(mut guard) = try_lock() else { return Ok(0) };
        let Some(st) = guard.as_mut() else { return Ok(0) };
        let th = st.cfg.language != "en";
        let admin = me.as_ref().is_none_or(|p| p.has_permission(&format!("{NS}:admin")));
        let can_create = admin || (st.cfg.players_can_create && me.as_ref().is_some_and(|p| p.has_permission(&format!("{NS}:create"))));
        let owner = me.as_ref().map(|p| p.get_name().to_ascii_lowercase()).unwrap_or_default();
        let reply = |line: String| c::reply(&sender, &line);
        let sub = w.first().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
        // A poster the sender may touch: their own, or any for staff
        let mine = |st: &State, name: &str| st.store.find(&name.to_ascii_lowercase()).filter(|p| admin || p.owner == owner).cloned();
        let no_poster = |name: &str| msg(th, &format!("&cไม่มีรูปชื่อ &f{} &7(ดู /imagemap list)", name.chars().take(32).collect::<String>()), &format!("&cNo picture named &f{} &7(see /imagemap list)", name.chars().take(32).collect::<String>()));
        match sub.as_str() {
            "new" | "file" => {
                if !can_create || (sub == "file" && !admin) {
                    reply(msg(th, "&cเซิร์ฟนี้ให้เฉพาะผู้ดูแลทำแผนที่จากรูป", "&cOnly staff may make picture maps on this server"));
                    return Ok(0);
                }
                let Some(what) = w.get(1) else {
                    help(th, can_create, admin).into_iter().for_each(reply);
                    return Ok(0);
                };
                let source = if sub == "file" {
                    if !cfg::valid_file(what) {
                        reply(msg(th, "&cชื่อไฟล์ต้องเป็น .png หรือ .jpg ที่อยู่ใน plugins/data/imagemap/images/ (ไม่ใส่ path)", "&cThe file must be a .png or .jpg in plugins/data/imagemap/images/ (a name, not a path)"));
                        return Ok(0);
                    }
                    Source::File(what.clone())
                } else {
                    match cfg::link_problem(what, st.cfg.allow_private_hosts) {
                        None => Source::Url(what.clone()),
                        Some("private address") => {
                            reply(msg(th, "&cลิงก์นี้ชี้เข้าเครือข่ายภายใน ใช้ไม่ได้", "&cThat link points inside a private network and cannot be used"));
                            return Ok(0);
                        }
                        Some(_) => {
                            reply(msg(th, "&cลิงก์ไม่ถูกต้อง &7ต้องเป็นลิงก์ตรงไปที่ไฟล์รูป เช่น https://…/picture.png", "&cThat is not a usable link &7(it must point straight at a picture, like https://…/picture.png)"));
                            return Ok(0);
                        }
                    }
                };
                let (mut size, mut mode, mut name) = (None, Mode::Fit, None);
                for word in &w[2..] {
                    if let Some(s) = cfg::parse_size(word, st.cfg.max_poster_side) {
                        size = Some(s);
                    } else if let Some(m) = cfg::parse_mode(word) {
                        mode = m;
                    } else if cfg::valid_name(&word.to_ascii_lowercase()) && name.is_none() && !word.split_once(['x', 'X']).is_some_and(|(a, b)| !a.is_empty() && !b.is_empty() && a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit())) {
                        name = Some(word.to_ascii_lowercase());
                    } else {
                        reply(msg(th, &format!("&cไม่เข้าใจ &f{} &7ขนาดเขียนแบบ 2x3 (ไม่เกิน {s}x{s}), โหมด fit|cover|stretch, ชื่อใช้ a-z 0-9 _ -", word.chars().take(20).collect::<String>(), s = st.cfg.max_poster_side), &format!("&cNot understood: &f{} &7a size is 2x3 (at most {s}x{s}), a mode fit|cover|stretch, a name a-z 0-9 _ -", word.chars().take(20).collect::<String>(), s = st.cfg.max_poster_side)));
                        return Ok(0);
                    }
                }
                let order = Order { who: me.as_ref().map(Player::get_name), owner: owner.clone(), admin, source, size, mode, name };
                if let Some((cols, rows)) = size {
                    if let Err(line) = room(st, &order, (cols * rows) as usize, th) {
                        reply(line);
                        return Ok(0);
                    }
                }
                let now = c::now_secs();
                if !admin {
                    if let Some(until) = st.cooldown.get(&owner).filter(|u| **u > now) {
                        reply(msg(th, &format!("&eรออีก {} วินาทีก่อนทำรูปถัดไป", until - now), &format!("&eWait {} more seconds before the next picture", until - now)));
                        return Ok(0);
                    }
                    st.cooldown.retain(|_, u| *u > now);
                    st.cooldown.insert(owner.clone(), now + st.cfg.cooldown_seconds);
                }
                if st.queue.len() >= MAX_QUEUE {
                    reply(msg(th, "&eมีรูปรอทำอยู่หลายรูป ลองใหม่อีกสักครู่", "&eSeveral pictures are waiting already; try again in a moment"));
                    return Ok(0);
                }
                let waiting = st.queue.len() + usize::from(st.job.is_some());
                st.queue.push_back(order);
                reply(if waiting == 0 { msg(th, "&7กำลังโหลดและแปลงรูป… จะบอกเมื่อเสร็จ", "&7Fetching and converting the picture… you will be told when it is done") } else { msg(th, &format!("&7เข้าคิวแล้ว (รออีก {waiting} รูป)"), &format!("&7Queued ({waiting} ahead)")) });
            }
            "list" => {
                let filter = w.get(1).map(|s| s.to_ascii_lowercase());
                let rows: Vec<&Poster> = st
                    .store
                    .posters
                    .iter()
                    .filter(|p| match (&filter, admin) {
                        (Some(f), true) if f == "all" => true,
                        (Some(f), true) => p.owner == *f,
                        _ => p.owner == owner,
                    })
                    .collect();
                reply(msg(th, &format!("&fรูป {} รายการ ({} แผ่น)", rows.len(), rows.iter().map(|p| p.maps.len()).sum::<usize>()), &format!("&f{} picture(s) ({} maps)", rows.len(), rows.iter().map(|p| p.maps.len()).sum::<usize>())));
                for p in rows.iter().take(40) {
                    reply(format!("&b {} &7{}x{} {}", p.name, p.cols, p.rows, if p.owner.is_empty() { "console" } else { &p.owner }));
                }
                if rows.len() > 40 {
                    reply(format!("&7 … +{}", rows.len() - 40));
                }
            }
            "get" | "give" => {
                let (target, name) = if sub == "give" { (w.get(1).and_then(|n| server.get_player_by_name(n)), w.get(2)) } else { (c::sender_player(&sender), w.get(1)) };
                if sub == "give" && !admin {
                    reply(msg(th, "&cคำสั่งนี้สำหรับผู้ดูแล", "&cThis is for staff"));
                    return Ok(0);
                }
                let (Some(p), Some(name)) = (target, name) else {
                    reply(msg(th, "&eวิธีใช้: &f/imagemap get <ชื่อ> &7หรือ &f/imagemap give <ผู้เล่นที่ออนไลน์> <ชื่อ>", "&eUsage: &f/imagemap get <name> &7or &f/imagemap give <online player> <name>"));
                    return Ok(0);
                };
                let Some(poster) = mine(st, name) else {
                    reply(no_poster(name));
                    return Ok(0);
                };
                let missed = give(&p, &poster);
                // The pictures too, in case this client never got them
                st.outbox.extend(poster.maps.iter().copied());
                reply(if missed == 0 { msg(th, &format!("&aให้แผนที่ของ &f{} &aแล้ว {} แผ่น", poster.name, poster.maps.len()), &format!("&aGave the {} map(s) of &f{}", poster.maps.len(), poster.name)) } else { msg(th, &format!("&eกระเป๋าเต็ม ให้ได้ {} จาก {} แผ่น", poster.maps.len() - missed, poster.maps.len()), &format!("&eThe inventory is full: {} of {} given", poster.maps.len() - missed, poster.maps.len())) });
            }
            "delete" | "remove" => {
                let Some(name) = w.get(1) else {
                    reply(msg(th, "&eวิธีใช้: &f/imagemap delete <ชื่อ>", "&eUsage: &f/imagemap delete <name>"));
                    return Ok(0);
                };
                let Some(poster) = mine(st, name) else {
                    reply(no_poster(name));
                    return Ok(0);
                };
                st.store.remove(&poster.name);
                st.cache.retain(|(id, _)| !poster.maps.contains(id));
                reply(msg(th, &format!("&aลบ &f{} &aแล้ว ({} แผ่น) &7แผนที่ที่แจกไปแล้วจะว่างเปล่าเมื่อผู้เล่นเข้าเซิร์ฟใหม่", poster.name, poster.maps.len()), &format!("&aDeleted &f{} &a({} maps) &7maps already handed out go blank when players rejoin", poster.name, poster.maps.len())));
            }
            "rename" => {
                let (Some(old), Some(new)) = (w.get(1), w.get(2).map(|n| n.to_ascii_lowercase())) else {
                    reply(msg(th, "&eวิธีใช้: &f/imagemap rename <ชื่อเดิม> <ชื่อใหม่>", "&eUsage: &f/imagemap rename <old> <new>"));
                    return Ok(0);
                };
                let Some(poster) = mine(st, old) else {
                    reply(no_poster(old));
                    return Ok(0);
                };
                reply(if st.store.rename(&poster.name, &new) { msg(th, &format!("&aเปลี่ยนชื่อเป็น &f{new} &aแล้ว"), &format!("&aRenamed to &f{new}")) } else { msg(th, "&cชื่อใหม่ใช้ไม่ได้หรือมีอยู่แล้ว &7(a-z 0-9 _ - ไม่เกิน 32 ตัว)", "&cThe new name is taken or not allowed &7(a-z 0-9 _ -, at most 32)") });
            }
            "info" | "version" => {
                let job = match &st.job {
                    Some(j) => match &j.stage {
                        Stage::Download(_) => "downloading".to_owned(),
                        Stage::Png(p) => format!("decoding {}/{}", p.progress().0, p.progress().1),
                        Stage::Paint { y, scaler, .. } => format!("painting {y}/{}", scaler.height),
                    },
                    None => "-".to_owned(),
                };
                reply(msg(th, &format!("&fImageMap {VERSION} &7· รูป {} รายการ {} แผ่น (สูงสุด {}) · ทำไปแล้ว {} · ส่งให้ผู้เล่น {} ครั้ง · งาน: {job} · คิว {} · tick ที่ใช้นานสุด {:.1} ms", st.store.posters.len(), st.store.total_maps(), st.cfg.max_maps, st.made, st.sent, st.queue.len(), st.slowest.as_secs_f64() * 1000.0), &format!("&fImageMap {VERSION} &7· {} pictures, {} maps (at most {}) · {} made · {} sent to players · job: {job} · {} queued · longest tick {:.1} ms", st.store.posters.len(), st.store.total_maps(), st.cfg.max_maps, st.made, st.sent, st.queue.len(), st.slowest.as_secs_f64() * 1000.0)));
            }
            "reload" if admin => {
                st.cfg = load();
                reply(msg(st.cfg.language != "en", "&aโหลดไฟล์ตั้งค่าใหม่แล้ว", "&aConfiguration reloaded"));
            }
            _ => help(th, can_create, admin).into_iter().for_each(reply),
        }
        Ok(1)
    }
}

struct ImageMap;

impl Plugin for ImageMap {
    fn new() -> Self {
        ImageMap
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Pictures on maps by MCSV".into(),
            dependencies: vec![],
            // HTTP and DNS: the picture is downloaded from the link given to /tomap
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into(), HTTP_OUTBOUND.into(), NETWORK_DNS.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let cfg = load();
        let store = Store::open(&c::data_path(""), cfg.first_map_id);
        // Where the owner uploads pictures for `/imagemap file`
        let _ = std::fs::create_dir_all(store.images_dir());
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg, store, job: None, queue: VecDeque::new(), outbox: VecDeque::new(), sender: None, tick: 0, burst_at: None, round: 0, cache: VecDeque::new(), cooldown: HashMap::new(), made: 0, sent: 0, slowest: Duration::ZERO });
        // The panel reads this to tell "loaded" from "installed, waiting for a restart"
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": c::now_secs() }));
        for (node, default) in [("command", PermissionDefault::Allow), ("create", PermissionDefault::Allow), ("admin", PermissionDefault::Op(PermissionLevel::Two))] {
            let _ = context.register_permission(&Permission { node: format!("{NS}:{node}"), description: "ImageMap".into(), default, children: vec![] });
        }
        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let cmd = Command::new(&[NS.to_owned(), "maps".to_owned()], "ImageMap")
            .execute(Cmd { tomap: false })
            .then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd { tomap: false }));
        context.register_command(cmd, &format!("{NS}:command"));
        let tomap = Command::new(&["tomap".to_owned()], "ImageMap: a picture link to maps")
            .execute(Cmd { tomap: true })
            .then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd { tomap: true }));
        context.register_command(tomap, &format!("{NS}:command"));
        context.register_event_handler(OnJoin, EventPriority::Normal, false)?;
        pumpkin_plugin_api::scheduler::schedule_repeating_task(1, 1, on_tick);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::remove_file("status.json");
        Ok(())
    }
}

register_plugin!(ImageMap);

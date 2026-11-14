//! Pumpkin WASM glue for Claims.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::BlockPos;
use pumpkin_plugin_api::events::{
    BlockBreakEvent, BlockBurnEvent, BlockExplodeEvent, BlockFromToEvent, BlockPistonExtendEvent, BlockPistonRetractEvent, BlockPlaceEvent,
    BlockSpreadEvent, CreatureSpawnEvent, EntityDamageByEntityEvent, EntityExplodeEvent, EventHandler, EventPriority, FromIntoEvent,
    InteractAction, PlayerBucketEmptyEvent, PlayerBucketFillEvent, PlayerInteractEntityEvent, PlayerInteractEvent, PlayerLeaveEvent,
    ProjectileLaunchEvent, SignChangeEvent,
};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::land::{self, Book, Claim, Config, Ledger, Problem};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NS: &str = "claims";
const PREFIX: &str = "&8[&bClaims&8]&r ";
const PLAYER_VERBS: &[&str] = &[
    "claim", "abandonclaim", "abandonallclaims", "trust", "containertrust", "accesstrust", "untrust", "trustlist", "claimslist", "claimlist",
    "claiminfo", "claimexplosions",
];
const ADMIN_VERBS: &[&str] = &["deleteclaim", "ignoreclaims", "adjustclaimblocks", "transferclaim"];

/// What a player is doing with the claim tool, and the marker blocks they are being shown.
#[derive(Default)]
struct Session {
    first: Option<(String, i32, i32)>,
    /// Claim being resized and the corner that stays.
    resize: Option<(u64, (i32, i32))>,
    marks: Vec<(i32, i32, i32, u64)>,
    last_click: Option<((i32, i32, i32), u64)>,
}

struct State {
    cfg: Config,
    book: Book,
    ledgers: BTreeMap<String, Ledger>,
    sessions: HashMap<String, Session>,
    /// Admins who switched protection off for themselves.
    ignoring: HashSet<String>,
    /// Arrows in flight: arrow entity id to (shooter entity id, forget after this second).
    arrows: HashMap<i32, (i32, u64)>,
    dirty: bool,
    secs: u64,
    corner_block: u16,
    edge_block: u16,
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

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn load() -> Config {
    c::load_config::<Config>("config.toml").cleaned()
}

fn save(st: &mut State) {
    c::save_json("claims.json", &st.book);
    c::save_json("players.json", &st.ledgers);
    st.dirty = false;
}

fn tr<'a>(th: bool, thai: &'a str, english: &'a str) -> &'a str {
    if th { thai } else { english }
}

fn held(p: &Player) -> String {
    p.get_inventory_item(p.get_selected_slot())
        .filter(|s| s.get_count() > 0)
        .map(|s| land::short(&s.get_registry_key()).to_lowercase())
        .unwrap_or_default()
}

fn free(st: &State, k: &str) -> i64 {
    land::free_blocks(&st.cfg, st.ledgers.get(k), st.book.used(k))
}

fn problem(th: bool, p: &Problem) -> String {
    match p {
        Problem::World => tr(th, "&cโลกนี้จองที่ดินไม่ได้", "&cLand cannot be claimed in this world").to_owned(),
        Problem::TooNarrow(n) => format!("&c{} {n} {}", tr(th, "ที่ดินต้องกว้างอย่างน้อย", "A claim must be at least"), tr(th, "บล็อก", "blocks wide")),
        Problem::TooSmall(n) => format!("&c{} {n} {}", tr(th, "ที่ดินต้องมีพื้นที่อย่างน้อย", "A claim must cover at least"), tr(th, "บล็อก", "blocks")),
        Problem::TooBig => tr(th, "&cที่ดินใหญ่เกินไป", "&cThat claim is too large").to_owned(),
        Problem::Overlaps(who) => format!("&c{} {who}", tr(th, "ทับที่ดินของ", "That overlaps the land of")),
        Problem::NeedBlocks(n) => format!("&c{} {n} {}", tr(th, "บล็อกจองไม่พอ ขาดอีก", "Not enough claim blocks,"), tr(th, "บล็อก", "more needed")),
        Problem::TooMany(n) => format!("&c{} {n} {}", tr(th, "จองได้ไม่เกิน", "No more than"), tr(th, "ที่", "claims each")),
    }
}

fn describe(th: bool, cl: &Claim) -> String {
    format!(
        "&f{} &7{} {}x{} ({} {}) &7x {}..{} z {}..{}",
        cl.owner_name,
        tr(th, "· ขนาด", "· size"),
        cl.x2 - cl.x1 + 1,
        cl.z2 - cl.z1 + 1,
        cl.area(),
        tr(th, "บล็อก", "blocks"),
        cl.x1, cl.x2, cl.z1, cl.z2
    )
}

/// Light up the outline of a claim for one player; the marks are taken back after 20 seconds.
fn show(st: &mut State, p: &Player, cl: &Claim) {
    let world = p.get_world();
    if world.get_dimension() != cl.dim {
        return;
    }
    let floor = world.get_min_y();
    let until = c::now_secs() + 20;
    let (corner, edge) = (st.corner_block, st.edge_block);
    let session = st.sessions.entry(key(p)).or_default();
    for (x, z, is_corner) in land::outline(cl) {
        let y = world.get_top_block_y(x, z);
        // An unloaded column reports the bottom of the world
        if y <= floor {
            continue;
        }
        p.send_block_change(BlockPos { x, y, z }, if is_corner { corner } else { edge });
        session.marks.push((x, y, z, until));
    }
}

fn deny(p: &Player, th: bool, owner: &str) {
    c::action_bar(p, &format!("&c{} {owner}", tr(th, "ที่นี่เป็นที่ดินของ", "This land belongs to")));
}

/// May this player do something needing `need` at this column? Says why not.
fn may(p: &Player, x: i32, z: i32, need: u8) -> bool {
    let Some(guard) = try_lock() else { return true };
    let Some(st) = guard.as_ref() else { return true };
    let dim = p.get_world().get_dimension();
    let Some(cl) = st.book.at(&dim, x, z) else { return true };
    let k = key(p);
    if st.ignoring.contains(&k) || cl.level_of(&k, &p.get_name().to_lowercase()) >= need {
        return true;
    }
    deny(p, st.cfg.thai(), &cl.owner_name);
    false
}

/// A right click with the claim tool.
fn tool_click(st: &mut State, p: &Player, x: i32, z: i32) {
    let th = st.cfg.thai();
    let k = key(p);
    let name = p.get_name();
    let dim = p.get_world().get_dimension();
    let left = free(st, &k);
    let session = st.sessions.entry(k.clone()).or_default();
    let line = if let Some((id, fixed)) = session.resize.take() {
        match st.book.resize(&st.cfg, id, fixed, (x, z), left) {
            Ok(()) => {
                st.dirty = true;
                if let Some(cl) = st.book.claims.iter().find(|cl| cl.id == id).cloned() {
                    show(st, p, &cl);
                }
                format!("&a{} &7· {} &f{}", tr(th, "ปรับขนาดที่ดินแล้ว", "Claim resized"), tr(th, "บล็อกจองเหลือ", "claim blocks left"), free(st, &k))
            }
            Err(e) => problem(th, &e),
        }
    } else if let Some((first_dim, fx, fz)) = session.first.take().filter(|f| f.0 == dim) {
        debug_assert_eq!(first_dim, dim);
        match st.book.create(&st.cfg, &k, &name, &dim, (fx, fz), (x, z), left, c::now_secs()) {
            Ok(id) => {
                st.dirty = true;
                st.ledgers.entry(k.clone()).or_default().name = name;
                if let Some(cl) = st.book.claims.iter().find(|cl| cl.id == id).cloned() {
                    show(st, p, &cl);
                }
                format!(
                    "&a{} &7· {} &f{} &7· {}",
                    tr(th, "จองที่ดินแล้ว", "Land claimed"), tr(th, "บล็อกจองเหลือ", "claim blocks left"), free(st, &k),
                    tr(th, "ให้เพื่อนสร้างได้ด้วย /trust <ชื่อ>", "let a friend build with /trust <name>")
                )
            }
            Err(e) => problem(th, &e),
        }
    } else {
        let here = st.book.at(&dim, x, z).cloned();
        match here {
            Some(cl) if (cl.owner == k || st.ignoring.contains(&k)) && cl.is_corner(x, z) => {
                let fixed = (if x == cl.x1 { cl.x2 } else { cl.x1 }, if z == cl.z1 { cl.z2 } else { cl.z1 });
                st.sessions.entry(k).or_default().resize = Some((cl.id, fixed));
                tr(th, "&eคลิกตำแหน่งใหม่ของมุมนี้เพื่อปรับขนาด", "&eClick where this corner should go").to_owned()
            }
            Some(cl) => {
                show(st, p, &cl);
                format!("&e{} {}", tr(th, "ตรงนี้เป็นที่ดินของ", "This is the land of"), describe(th, &cl))
            }
            None => {
                st.sessions.entry(k).or_default().first = Some((dim, x, z));
                format!("&e{} {x}, {z} &7· {}", tr(th, "มุมแรก", "First corner"), tr(th, "คลิกอีกมุมหนึ่งเพื่อจอง", "click the opposite corner to claim"))
            }
        }
    };
    c::tell(p, &format!("{PREFIX}{line}"));
}

fn valid_target(name: &str) -> bool {
    name == land::PUBLIC || ((1..=32).contains(&name.len()) && name.chars().all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '.'))
}

fn is_admin(sender: &CommandSender) -> bool {
    c::has_perm(sender, &format!("{NS}:admin"))
}

fn run(st: &mut State, server: &Server, sender: &CommandSender, verb: &str, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    let arg = w.first().map_or("", String::as_str);
    let ingame = || vec![tr(th, "&cคำสั่งนี้ใช้ได้เฉพาะในเกม", "&cThis command only works in game").to_owned()];
    let no_claim = || vec![tr(th, "&cยืนในที่ดินของคุณก่อน", "&cStand on your own land first").to_owned()];
    let player = c::sender_player(sender);
    // Where the sender stands, and the claim there if it is theirs to manage
    let spot = player.as_ref().map(|p| {
        let (pos, dim) = (p.get_position(), p.get_world().get_dimension());
        (key(p), dim, pos.0.floor() as i32, pos.2.floor() as i32)
    });
    let mine = |st: &State| -> Option<u64> {
        let (k, dim, x, z) = spot.as_ref()?;
        st.book.at(dim, *x, *z).filter(|cl| &cl.owner == k || st.ignoring.contains(k)).map(|cl| cl.id)
    };
    match verb {
        "claim" => {
            let (Some(p), Some((k, dim, x, z))) = (&player, &spot) else { return ingame() };
            let r = arg.parse::<i32>().unwrap_or(st.cfg.claim_radius).clamp(1, land::MAX_SIDE / 2 - 1);
            let left = free(st, k);
            match st.book.create(&st.cfg, k, &p.get_name(), dim, (x - r, z - r), (x + r, z + r), left, c::now_secs()) {
                Ok(id) => {
                    st.dirty = true;
                    st.ledgers.entry(k.clone()).or_default().name = p.get_name();
                    if let Some(cl) = st.book.claims.iter().find(|cl| cl.id == id).cloned() {
                        show(st, p, &cl);
                    }
                    vec![format!("&a{} {}x{} &7· {} &f{}", tr(th, "จองที่ดินแล้ว", "Land claimed"), r * 2 + 1, r * 2 + 1, tr(th, "บล็อกจองเหลือ", "claim blocks left"), free(st, k))]
                }
                Err(e) => vec![problem(th, &e)],
            }
        }
        "abandonclaim" | "deleteclaim" => {
            if verb == "deleteclaim" && !is_admin(sender) {
                return Vec::new();
            }
            let Some((_, dim, x, z)) = &spot else { return ingame() };
            let id = if verb == "deleteclaim" { st.book.at_id(dim, *x, *z) } else { mine(st) };
            match id.and_then(|id| st.book.remove(id)) {
                Some(cl) => {
                    st.dirty = true;
                    vec![format!("&a{} {}", tr(th, "ยกเลิกที่ดินแล้ว:", "Claim removed:"), describe(th, &cl))]
                }
                None => no_claim(),
            }
        }
        "abandonallclaims" => {
            let Some((k, ..)) = &spot else { return ingame() };
            if arg != "confirm" {
                return vec![format!("&e{} &f/abandonallclaims confirm", tr(th, "ยกเลิกที่ดินทั้งหมดของคุณ พิมพ์", "This removes all your claims. Type"))];
            }
            let n = st.book.remove_all(k);
            st.dirty = true;
            vec![format!("&a{} {n} {}", tr(th, "ยกเลิกที่ดินแล้ว", "Removed"), tr(th, "ที่", "claims"))]
        }
        "trust" | "containertrust" | "accesstrust" | "untrust" => {
            if spot.is_none() {
                return ingame();
            }
            let Some(id) = mine(st) else { return no_claim() };
            let target = arg.to_lowercase();
            if target.is_empty() || !(valid_target(&target) || (verb == "untrust" && target == "all")) {
                return vec![format!("&e{}: &f/{verb} <{}|public>", tr(th, "วิธีใช้", "Usage"), tr(th, "ชื่อผู้เล่น", "player"))];
            }
            let Some(cl) = st.book.get_mut(id) else { return no_claim() };
            let line = match verb {
                "untrust" if target == "all" => {
                    cl.trust.clear();
                    tr(th, "&aถอนสิทธิ์ทุกคนในที่ดินนี้แล้ว", "&aNobody else is trusted here now").to_owned()
                }
                "untrust" => {
                    cl.trust.remove(&target);
                    format!("&a{} {target}", tr(th, "ถอนสิทธิ์แล้ว:", "No longer trusted:"))
                }
                _ => {
                    let (level, what) = match verb {
                        "trust" => (land::BUILD, tr(th, "สร้างและใช้ของได้", "may build and use everything")),
                        "containertrust" => (land::CONTAINER, tr(th, "เปิดหีบและใช้ของได้", "may open containers")),
                        _ => (land::ACCESS, tr(th, "ใช้ประตู ปุ่ม และเตียงได้", "may use doors, buttons and beds")),
                    };
                    cl.trust.insert(target.clone(), level);
                    format!("&a{target} {what}")
                }
            };
            st.dirty = true;
            vec![line]
        }
        "trustlist" | "claiminfo" => {
            let Some((_, dim, x, z)) = &spot else { return ingame() };
            let Some(cl) = st.book.at(dim, *x, *z).cloned() else {
                return vec![tr(th, "&7ตรงนี้ยังไม่มีใครจอง", "&7Nobody has claimed this spot").to_owned()];
            };
            let names = |level: u8| cl.trust.iter().filter(|(_, l)| **l == level).map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ");
            let mut out = vec![format!("&e{} {}", tr(th, "ที่ดินของ", "Land of"), describe(th, &cl))];
            for (level, label) in [(land::BUILD, tr(th, "สร้างได้", "build")), (land::CONTAINER, tr(th, "เปิดหีบได้", "containers")), (land::ACCESS, tr(th, "ใช้ประตูได้", "access"))] {
                let list = names(level);
                if !list.is_empty() {
                    out.push(format!("&7{label}: &f{list}"));
                }
            }
            if cl.explosions {
                out.push(format!("&7{}", tr(th, "เจ้าของเปิดให้ระเบิดทำลายได้", "the owner allows explosions")));
            }
            if let Some(p) = &player {
                show(st, p, &cl);
            }
            out
        }
        "claimslist" | "claimlist" => {
            let Some((k, ..)) = &spot else { return ingame() };
            let mut out = vec![format!(
                "&7{} &f{} &7· {} &f{}",
                tr(th, "บล็อกจองเหลือ", "claim blocks left"), free(st, k), tr(th, "ใช้ไป", "used"), st.book.used(k)
            )];
            out.extend(st.book.claims.iter().filter(|cl| &cl.owner == k).map(|cl| {
                format!("&7#{} {} &f{},{} &7{}x{}", cl.id, land::short(&cl.dim), (cl.x1 + cl.x2) / 2, (cl.z1 + cl.z2) / 2, cl.x2 - cl.x1 + 1, cl.z2 - cl.z1 + 1)
            }));
            out
        }
        "claimexplosions" => {
            if spot.is_none() {
                return ingame();
            }
            let Some(cl) = mine(st).and_then(|id| st.book.get_mut(id)) else { return no_claim() };
            cl.explosions = !cl.explosions;
            let on = cl.explosions;
            st.dirty = true;
            vec![format!("&a{} {}", tr(th, "ระเบิดในที่ดินนี้:", "Explosions on this land:"), if on { tr(th, "ทำลายได้", "allowed") } else { tr(th, "กันไว้", "blocked") })]
        }
        "ignoreclaims" => {
            if !is_admin(sender) {
                return Vec::new();
            }
            let Some((k, ..)) = &spot else { return ingame() };
            let on = if st.ignoring.remove(k) { false } else { st.ignoring.insert(k.clone()) };
            vec![format!("&e{} {}", tr(th, "ข้ามการป้องกันที่ดิน:", "Ignoring claims:"), if on { tr(th, "เปิด", "on") } else { tr(th, "ปิด", "off") })]
        }
        "adjustclaimblocks" => {
            if !is_admin(sender) {
                return Vec::new();
            }
            let (Some(target), Some(amount)) = (w.first(), w.get(1).and_then(|s| s.parse::<i64>().ok())) else {
                return vec![format!("&e{}: &f/adjustclaimblocks <{}> <±{}>", tr(th, "วิธีใช้", "Usage"), tr(th, "ผู้เล่น", "player"), tr(th, "จำนวน", "amount"))];
            };
            let k = match server.get_player_by_name(target) {
                Some(p) => {
                    st.ledgers.entry(key(&p)).or_default().name = p.get_name();
                    Some(key(&p))
                }
                None => st.ledgers.iter().find(|(_, l)| l.name.eq_ignore_ascii_case(target)).map(|(k, _)| k.clone()),
            };
            let Some(k) = k else { return vec![format!("&c{} {target}", tr(th, "ไม่พบผู้เล่น", "No player named"))] };
            let ledger = st.ledgers.entry(k.clone()).or_default();
            ledger.bonus = (ledger.bonus + amount.clamp(-10_000_000, 10_000_000)).clamp(-10_000_000, 10_000_000);
            st.dirty = true;
            vec![format!("&a{target}: {} &f{}", tr(th, "บล็อกจองเหลือ", "claim blocks left"), free(st, &k))]
        }
        "transferclaim" => {
            if !is_admin(sender) {
                return Vec::new();
            }
            let Some((_, dim, x, z)) = &spot else { return ingame() };
            let Some(to) = server.get_player_by_name(arg) else { return vec![format!("&c{} {arg}", tr(th, "ไม่พบผู้เล่นออนไลน์ชื่อ", "No online player named"))] };
            let Some(cl) = st.book.at_id(dim, *x, *z).and_then(|id| st.book.get_mut(id)) else { return no_claim() };
            cl.owner = key(&to);
            cl.owner_name = to.get_name();
            st.dirty = true;
            vec![format!("&a{} {}", tr(th, "โอนที่ดินให้", "Claim now belongs to"), to.get_name())]
        }
        "reload" => {
            if !is_admin(sender) {
                return Vec::new();
            }
            st.cfg = load();
            vec![format!("&a{}", tr(st.cfg.thai(), "โหลดไฟล์ตั้งค่าใหม่แล้ว", "Configuration reloaded"))]
        }
        "version" => vec![format!("&fClaims {VERSION}")],
        _ => vec![
            format!("&eClaims: {}", tr(th, "คลิกขวาสองมุมด้วยพลั่วทองเพื่อจองที่ดิน คลิกด้วยไม้ (stick) เพื่อดูเจ้าของ", "right-click two corners with a golden shovel to claim, a stick shows the owner")),
            "&f/claim [รัศมี] &7| &f/abandonclaim &7| &f/trust &7| &f/containertrust &7| &f/accesstrust &7| &f/untrust &7| &f/trustlist &7| &f/claimslist &7| &f/claimexplosions".to_owned(),
        ],
    }
}

/// One of the commands; `"claims"` takes the verb from the first word.
struct Cmd(&'static str);

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let mut w = c::words(&args, "args");
        let verb = if self.0 == NS {
            if w.is_empty() { String::new() } else { w.remove(0).to_lowercase() }
        } else {
            self.0.to_owned()
        };
        let lines = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            let Some(st) = guard.as_mut() else { return Ok(0) };
            run(st, &server, &sender, &verb, &w)
        };
        for line in lines {
            c::reply(&sender, &format!("{PREFIX}{line}"));
        }
        Ok(1)
    }
}

/// Once a second: take marker blocks back, earn claim blocks, save.
fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.secs += 1;
    let now = c::now_secs();
    let players = server.get_all_players();
    for p in &players {
        if let Some(session) = st.sessions.get_mut(&key(p)) {
            session.marks.retain(|(x, y, z, until)| {
                if *until > now {
                    return true;
                }
                p.reset_block_change(BlockPos { x: *x, y: *y, z: *z });
                false
            });
        }
    }
    if !st.arrows.is_empty() {
        st.arrows.retain(|_, (_, until)| *until > now);
    }
    if st.secs % 60 == 0 {
        for p in &players {
            let ledger = st.ledgers.entry(key(p)).or_default();
            ledger.name = p.get_name();
            land::accrue(&st.cfg, ledger);
        }
        st.dirty |= !players.is_empty();
    }
    if st.secs % 30 == 0 && st.dirty {
        save(st);
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

macro_rules! needs_build {
    ($handler:ident, $event:ty, $player:expr, $pos:expr) => {
        struct $handler;
        impl EventHandler<$event> for $handler {
            fn handle(&self, _server: Server, mut e: Data<$event>) -> Data<$event> {
                let player: fn(&Data<$event>) -> Option<&Player> = $player;
                let pos: fn(&Data<$event>) -> (i32, i32) = $pos;
                let (x, z) = pos(&e);
                if player(&e).is_some_and(|p| !may(p, x, z, land::BUILD)) {
                    e.cancelled = true;
                }
                e
            }
        }
    };
}

needs_build!(OnBreak, BlockBreakEvent, |e| e.player.as_ref(), |e| (e.block_pos.x, e.block_pos.z));
needs_build!(OnPlace, BlockPlaceEvent, |e| Some(&e.player), |e| (e.block_pos.x, e.block_pos.z));
needs_build!(OnBucketEmpty, PlayerBucketEmptyEvent, |e| Some(&e.player), |e| (e.block_pos.x, e.block_pos.z));
needs_build!(OnBucketFill, PlayerBucketFillEvent, |e| Some(&e.player), |e| (e.block_pos.x, e.block_pos.z));
needs_build!(OnSign, SignChangeEvent, |e| Some(&e.player), |e| (e.block_pos.x, e.block_pos.z));
needs_build!(OnSpawnEgg, CreatureSpawnEvent, |e| e.player.as_ref(), |e| (e.position.0.floor() as i32, e.position.2.floor() as i32));

struct OnInteract;
impl EventHandler<PlayerInteractEvent> for OnInteract {
    fn handle(&self, _server: Server, mut e: Data<PlayerInteractEvent>) -> Data<PlayerInteractEvent> {
        let (InteractAction::RightClickBlock, Some(at)) = (&e.action, &e.clicked_pos) else { return e };
        let (x, y, z) = (at.x, at.y, at.z);
        let p = &e.player;
        let item = held(p);
        let (tool, stick) = match try_lock().as_ref().and_then(|g| g.as_ref()) {
            Some(st) => (item == st.cfg.claim_tool, item == st.cfg.inspect_tool),
            None => return e,
        };
        if tool || stick {
            if tool && !p.has_permission(&format!("{NS}:claim")) {
                return e;
            }
            e.cancelled = true;
            let Some(mut guard) = try_lock() else { return e };
            let Some(st) = guard.as_mut() else { return e };
            // The client sends one click per hand
            let now = now_ms();
            let session = st.sessions.entry(key(p)).or_default();
            if session.last_click.is_some_and(|(pos, at)| pos == (x, y, z) && now.saturating_sub(at) < 300) {
                return e;
            }
            session.last_click = Some(((x, y, z), now));
            if tool {
                tool_click(st, p, x, z);
            } else {
                let th = st.cfg.thai();
                let here = st.book.at(&p.get_world().get_dimension(), x, z).cloned();
                match here {
                    Some(cl) => {
                        show(st, p, &cl);
                        c::tell(p, &format!("{PREFIX}&e{} {}", tr(th, "ที่ดินของ", "Land of"), describe(th, &cl)));
                    }
                    None => c::tell(p, &format!("{PREFIX}&7{}", tr(th, "ตรงนี้ยังไม่มีใครจอง", "Nobody has claimed this spot"))),
                }
            }
            return e;
        }
        if land::is_bucket(&item) {
            // The fluid lands beside the clicked block: nobody pours across a border
            if [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|(dx, dz)| !may(p, x + dx, z + dz, land::BUILD)) {
                e.cancelled = true;
            }
            return e;
        }
        let need = land::block_needs(&e.block).max(if land::item_changes_world(&item) { land::BUILD } else { 0 });
        if need > 0 && !may(p, x, z, need) {
            e.cancelled = true;
        }
        e
    }
}

/// Explosions carry no world, so a claim at these columns in any dimension counts.
fn blast_blocked(x: i32, z: i32) -> bool {
    try_lock().as_ref().and_then(|g| g.as_ref()).is_some_and(|st| st.cfg.protect_explosions && st.book.claims.iter().any(|cl| !cl.explosions && cl.distance(x, z) <= 6))
}

struct OnEntityExplode;
impl EventHandler<EntityExplodeEvent> for OnEntityExplode {
    fn handle(&self, _server: Server, mut e: Data<EntityExplodeEvent>) -> Data<EntityExplodeEvent> {
        if blast_blocked(e.position.0.floor() as i32, e.position.2.floor() as i32) {
            e.cancelled = true;
        }
        e
    }
}

struct OnBlockExplode;
impl EventHandler<BlockExplodeEvent> for OnBlockExplode {
    fn handle(&self, _server: Server, mut e: Data<BlockExplodeEvent>) -> Data<BlockExplodeEvent> {
        if blast_blocked(e.block_pos.x, e.block_pos.z) {
            e.cancelled = true;
        }
        e
    }
}

/// Water and lava may not run from outside a claim into it. The event has no world: the world is
/// the one where the source block really is a fluid.
struct OnFlow;
impl EventHandler<BlockFromToEvent> for OnFlow {
    fn handle(&self, server: Server, mut e: Data<BlockFromToEvent>) -> Data<BlockFromToEvent> {
        let Some(guard) = try_lock() else { return e };
        let Some(st) = guard.as_ref() else { return e };
        if !st.cfg.protect_fluids || st.book.claims.is_empty() {
            return e;
        }
        let (from, to) = ((e.from_pos.x, e.from_pos.z), (e.to_pos.x, e.to_pos.z));
        let mut dims: Vec<&str> = st.book.claims.iter().filter(|cl| cl.covers(to.0, to.1) && !cl.covers(from.0, from.1)).map(|cl| cl.dim.as_str()).collect();
        dims.dedup();
        if dims.is_empty() {
            return e;
        }
        for world in server.get_all_worlds() {
            if !dims.contains(&world.get_dimension().as_str()) {
                continue;
            }
            let source = world.get_block(BlockPos { x: e.from_pos.x, y: e.from_pos.y, z: e.from_pos.z }).name;
            if source.contains("water") || source.contains("lava") {
                e.cancelled = true;
                break;
            }
        }
        e
    }
}

/// A piston outside every claim but right beside one could push blocks into it.
fn piston_blocked(x: i32, z: i32) -> bool {
    try_lock().as_ref().and_then(|g| g.as_ref()).is_some_and(|st| {
        st.cfg.protect_pistons && !st.book.claims.iter().any(|cl| cl.covers(x, z)) && st.book.near_any(x, z, 13).is_some()
    })
}

struct OnPistonOut;
impl EventHandler<BlockPistonExtendEvent> for OnPistonOut {
    fn handle(&self, _server: Server, mut e: Data<BlockPistonExtendEvent>) -> Data<BlockPistonExtendEvent> {
        if piston_blocked(e.block_pos.x, e.block_pos.z) {
            e.cancelled = true;
        }
        e
    }
}

struct OnPistonIn;
impl EventHandler<BlockPistonRetractEvent> for OnPistonIn {
    fn handle(&self, _server: Server, mut e: Data<BlockPistonRetractEvent>) -> Data<BlockPistonRetractEvent> {
        if piston_blocked(e.block_pos.x, e.block_pos.z) {
            e.cancelled = true;
        }
        e
    }
}

struct OnBurn;
impl EventHandler<BlockBurnEvent> for OnBurn {
    fn handle(&self, _server: Server, mut e: Data<BlockBurnEvent>) -> Data<BlockBurnEvent> {
        let Some(guard) = try_lock() else { return e };
        let Some(st) = guard.as_ref() else { return e };
        if st.cfg.protect_fire && !st.book.claims.is_empty() && st.book.at(&e.target_world.get_dimension(), e.block_pos.x, e.block_pos.z).is_some() {
            e.cancelled = true;
        }
        e
    }
}

/// Fire may not spread from outside a claim into it.
struct OnSpread;
impl EventHandler<BlockSpreadEvent> for OnSpread {
    fn handle(&self, _server: Server, mut e: Data<BlockSpreadEvent>) -> Data<BlockSpreadEvent> {
        let Some(guard) = try_lock() else { return e };
        let Some(st) = guard.as_ref() else { return e };
        if !st.cfg.protect_fire || st.book.claims.is_empty() {
            return e;
        }
        let dim = e.target_world.get_dimension();
        let target = st.book.at_id(&dim, e.target_pos.x, e.target_pos.z);
        if target.is_some() && target != st.book.at_id(&dim, e.source_pos.x, e.source_pos.z) {
            let source = e.target_world.get_block(BlockPos { x: e.source_pos.x, y: e.source_pos.y, z: e.source_pos.z }).name;
            if source.contains("fire") {
                e.cancelled = true;
            }
        }
        e
    }
}

/// The entity with this id near a player: its kind and block column. Entities carry no world or
/// position in their events, so we look around the player who touched them.
fn nearby(p: &Player, id: i32, reach: f64) -> Option<(String, i32, i32)> {
    p.as_entity().get_nearby_entities(reach, reach, reach).into_iter().find(|e| e.get_id() as i32 == id).map(|e| {
        let pos = e.get_position();
        (land::snake(&format!("{:?}", e.get_type())), pos.0.floor() as i32, pos.2.floor() as i32)
    })
}

/// Hitting animals, decorations and other players on claimed land.
struct OnHit;
impl EventHandler<EntityDamageByEntityEvent> for OnHit {
    fn handle(&self, server: Server, mut e: Data<EntityDamageByEntityEvent>) -> Data<EntityDamageByEntityEvent> {
        let (entities, pvp, any) = match try_lock().as_ref().and_then(|g| g.as_ref()) {
            Some(st) => (st.cfg.protect_entities, st.cfg.claim_pvp, !st.book.claims.is_empty()),
            None => return e,
        };
        if !any || (!entities && pvp) {
            return e;
        }
        // A player's own hit, or an arrow whose shooter we saw; a mob's hit is not ours to judge
        let shot_by = try_lock().as_ref().and_then(|g| g.as_ref()).and_then(|st| st.arrows.get(&e.damager_id).map(|(shooter, _)| *shooter));
        let damager = shot_by.unwrap_or(e.damager_id);
        let players = server.get_all_players();
        let Some(attacker) = players.iter().find(|p| p.as_entity().get_id() as i32 == damager) else { return e };
        if let Some(victim) = players.iter().find(|p| p.as_entity().get_id() as i32 == e.entity_id) {
            if pvp {
                return e;
            }
            let (pos, dim) = (victim.get_position(), victim.get_world().get_dimension());
            let on_claim = try_lock().as_ref().and_then(|g| g.as_ref()).is_some_and(|st| st.book.at(&dim, pos.0.floor() as i32, pos.2.floor() as i32).is_some());
            if on_claim {
                e.cancelled = true;
                let th = try_lock().as_ref().and_then(|g| g.as_ref()).is_none_or(|st| st.cfg.thai());
                c::action_bar(attacker, tr(th, "&cในที่ดินที่มีเจ้าของ ผู้เล่นทำร้ายกันไม่ได้", "&cNo fighting on claimed land"));
            }
            return e;
        }
        if !entities {
            return e;
        }
        // An arrow reaches much further than a hand
        if let Some((kind, x, z)) = nearby(attacker, e.entity_id, if shot_by.is_some() { 96.0 } else { 8.0 }) {
            let need = land::entity_needs(&kind);
            if need > 0 && !may(attacker, x, z, need) {
                e.cancelled = true;
            }
        }
        e
    }
}

/// Remember who shot an arrow, so its hit can be judged like the shooter's own.
struct OnLaunch;
impl EventHandler<ProjectileLaunchEvent> for OnLaunch {
    fn handle(&self, _server: Server, e: Data<ProjectileLaunchEvent>) -> Data<ProjectileLaunchEvent> {
        if let (Some(shooter), Some(st)) = (e.shooter_id, try_lock().as_mut().and_then(|g| g.as_mut())) {
            if st.arrows.len() < 4096 {
                st.arrows.insert(e.entity_id, (shooter, c::now_secs() + 60));
            }
        }
        e
    }
}

/// Right-clicking animals, boats, item frames and armor stands on claimed land.
struct OnUseEntity;
impl EventHandler<PlayerInteractEntityEvent> for OnUseEntity {
    fn handle(&self, _server: Server, mut e: Data<PlayerInteractEntityEvent>) -> Data<PlayerInteractEntityEvent> {
        let on = try_lock().as_ref().and_then(|g| g.as_ref()).is_some_and(|st| st.cfg.protect_entities && !st.book.claims.is_empty());
        if !on {
            return e;
        }
        if let Some((kind, x, z)) = nearby(&e.player, e.entity_id, 8.0) {
            let need = land::entity_needs(&kind);
            if need > 0 && !may(&e.player, x, z, need) {
                e.cancelled = true;
            }
        }
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, e: Data<PlayerLeaveEvent>) -> Data<PlayerLeaveEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let k = key(&e.player);
            st.sessions.remove(&k);
            st.ignoring.remove(&k);
        }
        e
    }
}

struct Claims;

impl Plugin for Claims {
    fn new() -> Self {
        Claims
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Land claims by MCSV".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        for (node, default) in [("claim", PermissionDefault::Allow), ("admin", PermissionDefault::Op(PermissionLevel::Three))] {
            let _ = context.register_permission(&Permission { node: format!("{NS}:{node}"), description: "Claims".into(), default, children: vec![] });
        }
        let mut book: Book = c::load_json("claims.json").unwrap_or_default();
        book.reindex();
        let ledgers: BTreeMap<String, Ledger> = c::load_json("players.json").unwrap_or_default();
        // `send_block_change` takes a block STATE id, not a block id
        let id_of = |name: &str| pumpkin_plugin_api::world::get_block_by_name(name).map_or(0, |b| b.default_state_id);
        c::log_info(&format!("{} claims", book.claims.len()));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State {
            cfg: load(),
            book,
            ledgers,
            sessions: HashMap::new(),
            ignoring: HashSet::new(),
            arrows: HashMap::new(),
            dirty: false,
            secs: 0,
            corner_block: id_of("glowstone"),
            edge_block: id_of("gold_block"),
        });
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": c::now_secs() }));

        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let greedy = || CommandNode::argument("args", &ArgumentType::String(StringType::Greedy));
        let root = Command::new(&[NS.to_owned()], "Claims").execute(Cmd(NS)).then(greedy().execute(Cmd(NS)));
        context.register_command(root, &format!("{NS}:claim"));
        for (verbs, node) in [(PLAYER_VERBS, "claim"), (ADMIN_VERBS, "admin")] {
            for verb in verbs {
                let cmd = Command::new(&[(*verb).to_owned()], "Claims").execute(Cmd(verb)).then(greedy().execute(Cmd(verb)));
                context.register_command(cmd, &format!("{NS}:{node}"));
            }
        }
        // Blocking: these cancel
        context.register_event_handler(OnBreak, EventPriority::Normal, true)?;
        context.register_event_handler(OnPlace, EventPriority::Normal, true)?;
        context.register_event_handler(OnInteract, EventPriority::Normal, true)?;
        context.register_event_handler(OnBucketEmpty, EventPriority::Normal, true)?;
        context.register_event_handler(OnBucketFill, EventPriority::Normal, true)?;
        context.register_event_handler(OnSign, EventPriority::Normal, true)?;
        context.register_event_handler(OnSpawnEgg, EventPriority::Normal, true)?;
        context.register_event_handler(OnEntityExplode, EventPriority::Normal, true)?;
        context.register_event_handler(OnBlockExplode, EventPriority::Normal, true)?;
        context.register_event_handler(OnFlow, EventPriority::Normal, true)?;
        context.register_event_handler(OnPistonOut, EventPriority::Normal, true)?;
        context.register_event_handler(OnPistonIn, EventPriority::Normal, true)?;
        context.register_event_handler(OnBurn, EventPriority::Normal, true)?;
        context.register_event_handler(OnSpread, EventPriority::Normal, true)?;
        context.register_event_handler(OnHit, EventPriority::Normal, true)?;
        context.register_event_handler(OnUseEntity, EventPriority::Normal, true)?;
        context.register_event_handler(OnLaunch, EventPriority::Normal, false)?;
        context.register_event_handler(OnLeave, EventPriority::Normal, false)?;
        context.schedule_repeating_task(20, 20, on_tick);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            save(st);
        }
        c::remove_file("status.json");
        Ok(())
    }
}

register_plugin!(Claims);

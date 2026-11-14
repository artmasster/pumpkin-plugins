//! Pumpkin WASM glue for Basics.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mcsv_common as c;
use pumpkin_plugin_api::command::{
    ArgumentType, Command, CommandError, CommandNode, CommandSender, CommandSuggestions, ConsumedArgs,
    StringType, SuggestionRequest,
};
use pumpkin_plugin_api::commands::{CommandHandler, CommandSuggestionHandler};
use pumpkin_plugin_api::common::GameMode;
use pumpkin_plugin_api::events::{
    EventHandler, EventPriority, FromIntoEvent, InventoryClickEvent, InventoryCloseEvent, PlayerChatEvent, PlayerCommandSendEvent,
    PlayerDeathEvent, PlayerJoinEvent, PlayerLeaveEvent, PlayerRespawnEvent, PlayerTeleportEvent,
};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::uuid::Uuid;
use pumpkin_plugin_api::world::World;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::lang::fmt;
use crate::store::{self, Burst, Config, Kits, Loc, PlayerData, Ranks, Worth};
use crate::{extra, gui, holo, more, plus};

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const NS: &str = "basics";

/// (names, default) — the first name is the command; permission `basics:command.<name>`.
const COMMANDS: &[(&[&str], bool)] = &[
    (&["basics"], false),
    (&["home", "h"], true),
    (&["sethome"], true),
    (&["delhome", "rmhome"], true),
    (&["homes"], true),
    (&["warp", "warps"], true),
    (&["setwarp"], false),
    (&["delwarp"], false),
    (&["spawn"], true),
    (&["setspawn"], false),
    (&["back"], true),
    (&["tpa"], true),
    (&["tpahere"], true),
    (&["tpaccept", "tpyes"], true),
    (&["tpdeny", "tpno"], true),
    (&["tpacancel"], true),
    (&["tphere", "s"], false),
    (&["top"], false),
    (&["m", "pm", "whisper"], true),
    (&["r", "reply"], true),
    (&["broadcast", "bc"], false),
    (&["fly"], false),
    (&["god"], false),
    (&["heal"], false),
    (&["feed"], false),
    (&["gm"], false),
    (&["gmc"], false),
    (&["gms"], false),
    (&["gma"], false),
    (&["gmsp"], false),
    (&["speed"], false),
    (&["nick"], false),
    (&["balance", "bal", "money"], true),
    (&["pay"], true),
    (&["baltop"], true),
    (&["eco"], false),
    (&["rank", "ranks"], true),
    (&["setrank"], false),
    (&["perm"], false),
    (&["mute"], false),
    (&["unmute"], false),
    (&["ignore"], true),
    (&["socialspy"], false),
    (&["afk"], true),
    (&["seen"], true),
    (&["whois"], false),
    (&["ping"], true),
    (&["near"], false),
    (&["mail"], true),
    (&["kit", "kits"], true),
    (&["ec", "enderchest"], false),
    (&["tpall"], false),
    (&["tptoggle"], true),
    (&["tppos"], false),
    (&["suicide"], true),
    (&["rtp", "wild"], true),
    (&["tempban"], false),
    (&["unban"], false),
    (&["warn"], false),
    (&["warnings"], true),
    (&["clearwarnings"], false),
    (&["freeze"], false),
    (&["jail"], false),
    (&["unjail"], false),
    (&["setjail"], false),
    (&["vanish", "v"], false),
    (&["invsee"], false),
    (&["clearinventory", "ci"], false),
    (&["hat"], false),
    (&["more"], false),
    (&["item", "i"], false),
    (&["itemname"], false),
    (&["itemlore"], false),
    (&["staffchat", "sc"], false),
    (&["helpop"], true),
    (&["clearchat"], false),
    (&["mutechat"], false),
    (&["kickall"], false),
    (&["exp", "xp"], false),
    (&["ptime"], false),
    (&["pweather"], false),
    (&["day"], false),
    (&["night"], false),
    (&["sun"], false),
    (&["rain"], false),
    (&["storm"], false),
    (&["getpos", "coords"], true),
    (&["playtime"], true),
    (&["realname"], true),
    (&["rules"], true),
    (&["motd"], true),
    (&["lag", "gc"], false),
    (&["jump", "j"], false),
    (&["smite", "lightning"], false),
    (&["ext", "extinguish"], false),
    (&["worth"], true),
    (&["sell"], true),
    (&["paytoggle"], true),
    (&["trash", "disposal"], true),
    (&["shop"], true),
    (&["repair", "fix"], false),
    (&["sudo"], false),
    (&["holo", "hologram"], false),
];

/// Commands whose default is operator level 3: they act with another player's hands.
const ADMIN_COMMANDS: &[&str] = &["sudo"];

struct Tpa {
    from: String,
    to: String,
    here: bool,
    at: Instant,
}

pub(crate) enum Target {
    Loc(Loc),
    Player(String),
}

struct PendingTp {
    start: (f64, f64, f64),
    due: Instant,
    target: Target,
    /// Taken from the player's balance when the teleport happens (a warp with a price).
    cost: f64,
}

/// Where a player stood when they last did something (AFK detection).
pub(crate) struct Afk {
    pub pos: (f64, f64, f64),
    pub yaw: f32,
    pub since: Instant,
    pub afk: bool,
}

/// A random teleport waiting for its chunk to load.
pub(crate) struct Rtp {
    pub x: i32,
    pub z: i32,
    pub started: Instant,
    pub origin: Loc,
    /// Random points already given up because nothing near them was safe to stand on.
    pub tries: u32,
}

pub(crate) struct State {
    pub cfg: Config,
    pub ranks: Ranks,
    pub kits: Kits,
    pub players: HashMap<String, PlayerData>,
    warps: BTreeMap<String, Loc>,
    pub spawn: Option<Loc>,
    tpa: Vec<Tpa>,
    pending: HashMap<String, PendingTp>,
    pub last_tp: HashMap<String, Instant>,
    reply: HashMap<String, String>,
    /// Permission nodes we set on each online player, to take them back.
    pub applied: HashMap<String, Vec<String>>,
    pub afk: HashMap<String, Afk>,
    pub rtp: HashMap<String, Rtp>,
    pub last_rtp: HashMap<String, Instant>,
    /// Players made invulnerable by /rtp until this instant.
    pub protect: HashMap<String, Instant>,
    /// Players whose file has changes not written yet (flushed every few seconds).
    pub dirty: HashSet<String>,
    /// When each player was last teleported by anything. Pumpkin kicks a client ("Wrong teleport
    /// id") when a second teleport is sent before the first is confirmed, so ours keep a gap.
    pub tp_guard: HashMap<String, Instant>,
    pub ticks: u64,
    pub last_announce: Instant,
    pub announce_idx: usize,
    pub seed: u64,
    pub worth: Worth,
    pub jail: Option<Loc>,
    /// Lower-case player name -> key, so lookups by name do not walk every player.
    names: HashMap<String, String>,
    /// Recent Basics commands per player (flood limit).
    burst: HashMap<String, Burst>,
    tpa_last: HashMap<String, Instant>,
    /// /mutechat: only staff may talk.
    pub chat_locked: bool,
    pub started: Instant,
    /// Windows on screen (/invsee, /ec <player>, /trash, /shop), by viewer.
    pub wins: HashMap<String, gui::Win>,
    pub mirror_tick: Instant,
    pub shop: store::Shop,
    pub shop_burst: HashMap<String, Burst>,
    /// `[aliases]` of the config after the unusable ones were dropped.
    pub aliases: BTreeMap<String, String>,
    /// Players who joined and are not welcomed yet: the motd, mail notice, spawn teleport, join
    /// line and AFK clock wait until a login prompt (Login) is behind them.
    pub joining: HashMap<String, plus::Joining>,
    /// Players whose join line was said, so their quit line is said too.
    pub announced: HashSet<String>,
    pub bar: Option<plus::Bar>,
    pub holos: store::Holos,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Shortest time between two teleports of one player (see `State::tp_guard`).
pub(crate) const TP_GAP: std::time::Duration = std::time::Duration::from_millis(1500);

fn lock() -> std::sync::MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// For event handlers: Pumpkin fires events synchronously from inside host calls
/// (a teleport fires PlayerTeleportEvent while a command still holds the state).
/// WASM has no threads, so a second `lock()` would trap; skip instead.
pub(crate) fn try_lock() -> Option<std::sync::MutexGuard<'static, Option<State>>> {
    match STATE.try_lock() {
        Ok(g) => Some(g),
        Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// The configured language, readable while the state is held by someone else.
static ENGLISH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn lang_now() -> &'static str {
    if ENGLISH.load(std::sync::atomic::Ordering::Relaxed) { "en" } else { "th" }
}

pub(crate) fn key(id: &Uuid) -> String {
    format!("{:016x}{:016x}", id.high, id.low)
}

pub(crate) fn uuid_of(k: &str) -> Option<Uuid> {
    if k.len() != 32 {
        return None;
    }
    Some(Uuid { high: u64::from_str_radix(&k[..16], 16).ok()?, low: u64::from_str_radix(&k[16..], 16).ok()? })
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ---- persistence ------------------------------------------------------------

impl State {
    pub(crate) fn data(&mut self, p: &Player) -> &mut PlayerData {
        let k = key(&p.get_id());
        let start = self.cfg.starting_balance;
        self.players.entry(k.clone()).or_insert_with(|| {
            c::load_json(&format!("players/{k}.json")).unwrap_or(PlayerData { balance: start, ..Default::default() })
        })
    }

    pub(crate) fn save(&self, k: &str) {
        if let Some(d) = self.players.get(k) {
            c::save_json(&format!("players/{k}.json"), d);
        }
    }

    /// Write files marked dirty (positions for /back change often; money and homes save at once).
    pub(crate) fn flush(&mut self) {
        for k in std::mem::take(&mut self.dirty) {
            self.save(&k);
        }
    }

    /// Key of a known player by name: online first, then anyone who has joined before.
    pub(crate) fn key_by_name(&mut self, server: &Server, name: &str) -> Option<String> {
        if let Some(p) = find_player(server, name) {
            self.data(&p);
            return Some(key(&p.get_id()));
        }
        self.key_of_name(name)
    }

    /// Key of anyone who has joined before, by name.
    pub(crate) fn key_of_name(&self, name: &str) -> Option<String> {
        self.names.get(&name.to_lowercase()).filter(|k| self.players.contains_key(*k)).cloned()
    }

    pub(crate) fn index_name(&mut self, k: &str) {
        if let Some(n) = self.players.get(k).map(|d| d.name.to_lowercase()).filter(|n| !n.is_empty()) {
            self.names.insert(n, k.to_owned());
        }
    }

    fn save_world(&self) {
        c::save_json("warps.json", &self.warps);
        match &self.spawn {
            Some(s) => {
                c::save_json("spawn.json", s);
            }
            None => c::remove_file("spawn.json"),
        }
    }

    fn load_all(&mut self) {
        self.warps = c::load_json("warps.json").unwrap_or_default();
        self.spawn = c::load_json("spawn.json");
        self.jail = c::load_json::<Option<Loc>>("jail.json").flatten();
        if let Ok(dir) = std::fs::read_dir(c::data_path("players")) {
            for e in dir.filter_map(Result::ok) {
                let name = e.file_name().to_string_lossy().into_owned();
                if let Some(k) = name.strip_suffix(".json") {
                    if let Some(d) = c::load_json::<PlayerData>(&format!("players/{name}")) {
                        self.players.insert(k.to_owned(), d);
                        self.index_name(k);
                    }
                }
            }
        }
    }
}

// ---- helpers ----------------------------------------------------------------

pub(crate) fn loc_of(p: &Player) -> Loc {
    let (x, y, z) = p.get_position();
    Loc { dim: p.get_world().get_dimension(), x, y, z, yaw: p.get_yaw(), pitch: p.get_pitch() }
}

pub(crate) fn world_of(server: &Server, dim: &str) -> Option<World> {
    server.get_all_worlds().into_iter().find(|w| w.get_dimension() == dim)
}

pub(crate) fn find_player(server: &Server, name: &str) -> Option<Player> {
    if let Some(p) = server.get_player_by_name(name) {
        return Some(p);
    }
    let lower = name.to_lowercase();
    let mut matches: Vec<Player> = server.get_all_players().into_iter().filter(|p| p.get_name().to_lowercase().starts_with(&lower)).collect();
    (matches.len() == 1).then(|| matches.remove(0))
}

pub(crate) fn perm(sender: &CommandSender, node: &str) -> bool {
    c::has_perm(sender, &format!("{NS}:{node}"))
}

pub(crate) fn player_perm(p: &Player, node: &str) -> bool {
    p.has_permission(&format!("{NS}:{node}"))
}

/// Nodes made from names in config files (`warp.<name>`, `kit.<name>`) are not registered, so
/// Pumpkin answers false unless a rank or /perm grants them. Operators (level 2+) always pass.
pub(crate) fn dyn_perm(p: &Player, node: &str) -> bool {
    player_perm(p, node) || p.get_permission_level() as u8 >= PermissionLevel::Two as u8
}

/// Teleport now and remember where the player was for /back.
pub(crate) fn teleport_now(st: &mut State, server: &Server, p: &Player, to: &Loc) -> bool {
    let Some(world) = world_of(server, &to.dim) else {
        c::tell(p, &fmt(&st.cfg.language, "no_world", &[]));
        return false;
    };
    let here = loc_of(p);
    let k = key(&p.get_id());
    if let Some(due) = st.tp_guard.get(&k).filter(|t| t.elapsed() < TP_GAP).map(|t| *t + TP_GAP) {
        // Too soon after the last teleport: do it when the client has had time to confirm that one
        st.pending.insert(k, PendingTp { start: (f64::NAN, 0.0, 0.0), due, target: Target::Loc(to.clone()), cost: 0.0 });
        return true;
    }
    st.tp_guard.insert(k.clone(), Instant::now());
    st.data(p).back = Some(here);
    st.dirty.insert(k.clone());
    p.teleport_world(world, (to.x, to.y, to.z), Some(to.yaw), Some(to.pitch));
    st.last_tp.insert(k, Instant::now());
    true
}

/// Take a teleport price from the player. False (and a message) when they cannot pay.
fn charge(st: &mut State, p: &Player, cost: f64) -> bool {
    if cost <= 0.0 {
        return true;
    }
    let l = st.cfg.language.clone();
    let sym = st.cfg.currency_symbol.clone();
    let k = key(&p.get_id());
    let balance = st.data(p).balance;
    match store::shop_buy(cost, 1, balance) {
        Ok((paid, new)) => {
            st.data(p).balance = new;
            st.save(&k);
            c::tell(p, &fmt(&l, "warp_paid", &[("money", store::money(paid, &sym)), ("balance", store::money(new, &sym))]));
            true
        }
        Err(_) => {
            c::tell(p, &fmt(&l, "no_money", &[("money", store::money(balance, &sym))]));
            false
        }
    }
}

/// Teleport now, taking the price first; nothing is taken when the place cannot be reached.
fn pay_and_go(st: &mut State, server: &Server, p: &Player, to: &Loc, cost: f64) -> bool {
    if world_of(server, &to.dim).is_none() {
        c::tell(p, &fmt(&st.cfg.language, "no_world", &[]));
        return false;
    }
    charge(st, p, cost) && teleport_now(st, server, p, to)
}

/// Send a player to /spawn on the next tick (first join, `spawn-on-join`). Goes through
/// `teleport_now`, so it keeps the gap after the teleport that came before.
pub(crate) fn queue_spawn(st: &mut State, p: &Player) {
    if let Some(spawn) = st.spawn.clone() {
        st.pending.insert(key(&p.get_id()), PendingTp { start: (f64::NAN, 0.0, 0.0), due: Instant::now(), target: Target::Loc(spawn), cost: 0.0 });
    }
}

/// Teleport after the configured delay (players standing still), honoring cooldown.
pub(crate) fn teleport(st: &mut State, server: &Server, p: &Player, target: Target) {
    teleport_for(st, server, p, target, None, 0.0);
}

/// `teleport` with a delay of its own and a price paid when the teleport happens.
pub(crate) fn teleport_for(st: &mut State, server: &Server, p: &Player, target: Target, delay: Option<u64>, cost: f64) {
    let l = st.cfg.language.clone();
    let k = key(&p.get_id());
    let bypass = player_perm(p, "bypass.delay");
    if st.cfg.teleport_cooldown > 0 && !player_perm(p, "bypass.cooldown") {
        if let Some(t) = st.last_tp.get(&k) {
            let left = st.cfg.teleport_cooldown.saturating_sub(t.elapsed().as_secs());
            if left > 0 {
                c::tell(p, &fmt(&l, "tp_cooldown", &[("sec", left.to_string())]));
                return;
            }
        }
    }
    let delay = delay.unwrap_or(st.cfg.teleport_delay).min(600);
    if delay == 0 || bypass {
        if let Some(loc) = resolve(server, &target) {
            pay_and_go(st, server, p, &loc, cost);
        }
        return;
    }
    st.pending.insert(k, PendingTp { start: p.get_position(), due: Instant::now() + std::time::Duration::from_secs(delay), target, cost });
    c::tell(p, &fmt(&l, "tp_wait", &[("sec", delay.to_string())]));
}

fn resolve(server: &Server, t: &Target) -> Option<Loc> {
    match t {
        Target::Loc(l) => Some(l.clone()),
        Target::Player(k) => uuid_of(k).and_then(|u| server.get_player_by_uuid(u)).map(|p| loc_of(&p)),
    }
}

fn home_limit(st: &State, p: &Player) -> Option<u32> {
    if player_perm(p, "homes.unlimited") {
        return None;
    }
    let mut max = st.cfg.default_homes;
    for n in [1u32, 2, 3, 4, 5, 6, 8, 10, 15, 20, 25, 30, 50, 100] {
        if n > max && player_perm(p, &format!("homes.{n}")) {
            max = n;
        }
    }
    Some(max)
}

pub(crate) fn on_off(l: &str, v: bool) -> String {
    crate::lang::text(l, if v { "on" } else { "off" }).to_owned()
}

fn gamemode(s: &str) -> Option<GameMode> {
    Some(match s.to_lowercase().as_str() {
        "0" | "s" | "survival" => GameMode::Survival,
        "1" | "c" | "creative" => GameMode::Creative,
        "2" | "a" | "adventure" => GameMode::Adventure,
        "3" | "sp" | "spectator" => GameMode::Spectator,
        _ => return None,
    })
}

fn gm_name(m: GameMode) -> &'static str {
    match m {
        GameMode::Survival => "survival",
        GameMode::Creative => "creative",
        GameMode::Adventure => "adventure",
        GameMode::Spectator => "spectator",
    }
}

fn set_fly(p: &Player, on: bool) {
    let mut a = p.get_abilities();
    a.allow_flying = on;
    if !on {
        a.flying = false;
    }
    p.set_abilities(a);
}

// ---- commands -----------------------------------------------------------------

fn run(name: &str, sender: &CommandSender, server: &Server, w: &[String]) -> Vec<String> {
    // Busy = this command was started from inside another Basics call (a script, a forced command):
    // waiting would never end on one thread
    let Some(mut guard) = try_lock() else { return vec![fmt(lang_now(), "busy", &[])] };
    let Some(st) = guard.as_mut() else { return Vec::new() };
    let lang = st.cfg.language.clone();
    let l = lang.as_str();
    let say = |k: &str, v: &[(&str, String)]| vec![fmt(l, k, v)];
    let me = c::sender_player(sender);
    let arg = |i: usize| w.get(i).map(String::as_str);
    let rest = |i: usize| w.get(i..).map(|s| s.join(" ")).unwrap_or_default();
    if let Some(p) = &me {
        // Flood limit; operators are trusted with scripts and command blocks
        let max = st.cfg.command_burst;
        if max > 0 && (p.get_permission_level() as u8) < PermissionLevel::Two as u8 {
            let now = st.started.elapsed().as_secs_f64();
            if !st.burst.entry(key(&p.get_id())).or_default().allow(now, max, 5.0) {
                return say("slow_down", &[]);
            }
        }
        if let Some(why) = extra::blocked(st, p, name) {
            return vec![why];
        }
        if name != "afk" {
            more::active(st, server, p);
        }
    }

    // Target: args[i] when allowed (`basics:others.<command>`), else the sender
    let target = |i: usize, cmd: &str| -> Result<Player, Vec<String>> {
        match arg(i) {
            Some(n) if me.is_none() || perm(sender, &format!("others.{cmd}")) => {
                find_player(server, n).ok_or_else(|| vec![fmt(l, "no_player", &[("arg", n.to_owned())])])
            }
            Some(_) => Err(vec![fmt(l, "no_perm", &[])]),
            None => c::sender_player(sender).ok_or_else(|| vec![fmt(l, "player_only", &[])]),
        }
    };
    macro_rules! need_player {
        () => {
            match c::sender_player(sender) {
                Some(p) => p,
                None => return say("player_only", &[]),
            }
        };
    }

    match name {
        "basics" => match arg(0) {
            Some("reload") => {
                st.cfg = c::load_config("config.toml");
                st.ranks = c::load_config("ranks.toml");
                st.kits = c::load_config("kits.toml");
                st.worth = c::load_config("worth.toml");
                st.shop = gui::load_shop(&st.cfg.language);
                let refused = load_aliases(st);
                gui::redraw_shops(st, server);
                for p in server.get_all_players() {
                    more::apply_perms(st, &p);
                    more::refresh_name(st, &p);
                }
                let l = st.cfg.language.clone();
                let mut out = vec![fmt(&l, "reloaded", &[])];
                if !refused.is_empty() {
                    out.push(fmt(&l, "alias_bad", &[("list", refused.join(", "))]));
                }
                out
            }
            Some("version") => vec![format!("{}&fBasics {VERSION}", crate::lang::text(l, "prefix"))],
            _ => say("help", &[]),
        },

        // -- homes
        "sethome" => {
            let p = need_player!();
            let hname = arg(0).unwrap_or("home").to_lowercase();
            if !store::valid_name(&hname) {
                return say("bad_name", &[]);
            }
            let max = home_limit(st, &p);
            let loc = loc_of(&p);
            let d = st.data(&p);
            if !d.homes.contains_key(&hname) && max.is_some_and(|m| d.homes.len() as u32 >= m) {
                return say("home_limit", &[("max", max.unwrap_or(0).to_string())]);
            }
            d.homes.insert(hname.clone(), loc);
            st.save(&key(&p.get_id()));
            say("home_set", &[("name", hname)])
        }
        "home" => {
            let p = need_player!();
            let d = st.data(&p).clone();
            let hname = match arg(0) {
                Some(n) => n.to_lowercase(),
                None if d.homes.len() == 1 => d.homes.keys().next().cloned().unwrap_or_default(),
                None => "home".into(),
            };
            match d.homes.get(&hname) {
                Some(loc) => {
                    teleport(st, server, &p, Target::Loc(loc.clone()));
                    Vec::new()
                }
                None => say("home_none", &[("name", hname), ("list", d.homes.keys().cloned().collect::<Vec<_>>().join(", "))]),
            }
        }
        "delhome" => {
            let p = need_player!();
            let Some(hname) = arg(0).map(str::to_lowercase) else { return say("usage", &[("arg", "/delhome <name>".into())]) };
            let d = st.data(&p);
            if d.homes.remove(&hname).is_none() {
                let list = d.homes.keys().cloned().collect::<Vec<_>>().join(", ");
                return say("home_none", &[("name", hname), ("list", list)]);
            }
            st.save(&key(&p.get_id()));
            say("home_del", &[("name", hname)])
        }
        "homes" => {
            let p = need_player!();
            let max = home_limit(st, &p).map_or("∞".into(), |m| m.to_string());
            let d = st.data(&p);
            let list = d.homes.keys().cloned().collect::<Vec<_>>().join(", ");
            say("homes", &[("count", d.homes.len().to_string()), ("max", max), ("list", list)])
        }

        // -- warps
        "setwarp" => {
            let p = need_player!();
            let Some(wname) = arg(0).map(str::to_lowercase) else { return say("usage", &[("arg", "/setwarp <name>".into())]) };
            if !store::valid_name(&wname) {
                return say("bad_name", &[]);
            }
            st.warps.insert(wname.clone(), loc_of(&p));
            st.save_world();
            say("warp_set", &[("name", wname)])
        }
        "delwarp" => {
            let Some(wname) = arg(0).map(str::to_lowercase) else { return say("usage", &[("arg", "/delwarp <name>".into())]) };
            if st.warps.remove(&wname).is_none() {
                return say("warp_none", &[("name", wname)]);
            }
            st.save_world();
            say("warp_del", &[("name", wname)])
        }
        "warp" => {
            let Some(wname) = arg(0).map(str::to_lowercase) else {
                let open = !st.cfg.warp_permission;
                let list: Vec<String> = st
                    .warps
                    .keys()
                    .filter(|n| open || me.as_ref().is_none_or(|p| dyn_perm(p, &format!("warp.{n}"))))
                    .cloned()
                    .collect();
                if list.is_empty() {
                    return say("warps_none", &[]);
                }
                return say("warps", &[("list", list.join(", "))]);
            };
            let Some(loc) = st.warps.get(&wname).cloned() else { return say("warp_none", &[("name", wname)]) };
            if st.cfg.warp_permission && me.as_ref().is_some_and(|p| !dyn_perm(p, &format!("warp.{wname}"))) {
                return say("warp_no_perm", &[("name", wname)]);
            }
            let p = match target(1, "warp") {
                Ok(p) => p,
                Err(e) => return e,
            };
            // A price and a wait of its own apply to players warping themselves, not to staff sending someone
            let own = me.as_ref().is_some_and(|m| key(&m.get_id()) == key(&p.get_id()));
            let cost = st.cfg.warp_cost.get(&wname).copied().filter(|v| own && v.is_finite() && *v > 0.0).unwrap_or(0.0);
            let delay = st.cfg.warp_delay.get(&wname).copied().filter(|_| own);
            if cost > 0.0 && st.data(&p).balance + 1e-9 < cost {
                let sym = st.cfg.currency_symbol.clone();
                return say("warp_cost", &[("name", wname), ("money", store::money(cost, &sym))]);
            }
            teleport_for(st, server, &p, Target::Loc(loc), delay, cost);
            Vec::new()
        }

        // -- spawn / back / top
        "setspawn" => {
            let p = need_player!();
            st.spawn = Some(loc_of(&p));
            st.save_world();
            say("spawn_set", &[])
        }
        "spawn" => {
            let Some(loc) = st.spawn.clone() else { return say("spawn_none", &[]) };
            let p = match target(0, "spawn") {
                Ok(p) => p,
                Err(e) => return e,
            };
            teleport(st, server, &p, Target::Loc(loc));
            Vec::new()
        }
        "back" => {
            let p = need_player!();
            match st.data(&p).back.clone() {
                Some(loc) => {
                    teleport(st, server, &p, Target::Loc(loc));
                    Vec::new()
                }
                None => say("back_none", &[]),
            }
        }
        "top" => {
            let p = need_player!();
            let mut loc = loc_of(&p);
            let y = p.get_world().get_top_block_y(loc.x.floor() as i32, loc.z.floor() as i32);
            loc.y = f64::from(y) + 1.0;
            teleport_now(st, server, &p, &loc);
            say("top", &[])
        }
        "tphere" => {
            let p = need_player!();
            let Some(t) = arg(0).and_then(|n| find_player(server, n)) else {
                return say("no_player", &[("arg", arg(0).unwrap_or("").to_owned())]);
            };
            let loc = loc_of(&p);
            teleport_now(st, server, &t, &loc);
            say("tp_done", &[])
        }

        // -- tpa
        "tpa" | "tpahere" => {
            let p = need_player!();
            let Some(t) = arg(0).and_then(|n| find_player(server, n)) else {
                return say("no_player", &[("arg", arg(0).unwrap_or("").to_owned())]);
            };
            let (from, to) = (key(&p.get_id()), key(&t.get_id()));
            if from == to {
                return say("tpa_self", &[]);
            }
            if extra::hidden_from(st, Some(&p), &to) {
                return say("no_player", &[("arg", arg(0).unwrap_or("").to_owned())]);
            }
            if st.tpa_last.get(&from).is_some_and(|t| t.elapsed().as_secs() < 3) {
                return say("slow_down", &[]);
            }
            st.tpa_last.insert(from.clone(), Instant::now());
            let td = st.data(&t).clone();
            if td.tp_off || td.ignored.contains(&from) {
                return say("tp_off", &[("name", t.get_name())]);
            }
            let here = name == "tpahere";
            st.tpa.retain(|r| !(r.from == from && r.to == to));
            st.tpa.push(Tpa { from, to, here, at: Instant::now() });
            c::tell(&t, &fmt(l, if here { "tpahere_to_you" } else { "tpa_to_you" }, &[("name", p.get_name())]));
            say("tpa_sent", &[("name", t.get_name())])
        }
        "tpaccept" | "tpdeny" => {
            let p = need_player!();
            let me_k = key(&p.get_id());
            let timeout = st.cfg.tpa_timeout;
            st.tpa.retain(|r| r.at.elapsed().as_secs() < timeout);
            let from_filter = arg(0).and_then(|n| find_player(server, n)).map(|x| key(&x.get_id()));
            let Some(idx) = st.tpa.iter().rposition(|r| r.to == me_k && from_filter.as_ref().is_none_or(|f| *f == r.from)) else {
                return say("tpa_none", &[]);
            };
            let req = st.tpa.remove(idx);
            let Some(other) = uuid_of(&req.from).and_then(|u| server.get_player_by_uuid(u)) else {
                return say("tpa_none", &[]);
            };
            if name == "tpdeny" {
                c::tell(&other, &fmt(l, "tpa_denied", &[("name", p.get_name())]));
                return say("tpa_you_denied", &[]);
            }
            c::tell(&other, &fmt(l, "tpa_accepted", &[("name", p.get_name())]));
            if req.here {
                teleport(st, server, &p, Target::Player(req.from.clone()));
            } else {
                teleport(st, server, &other, Target::Player(me_k));
            }
            Vec::new()
        }
        "tpacancel" => {
            let p = need_player!();
            let k = key(&p.get_id());
            st.tpa.retain(|r| r.from != k);
            say("tpa_cancelled", &[])
        }

        // -- chat
        "m" | "r" => {
            let (to, msg) = if name == "m" {
                let Some(t) = arg(0).and_then(|n| find_player(server, n)) else {
                    return say("no_player", &[("arg", arg(0).unwrap_or("").to_owned())]);
                };
                (t, rest(1))
            } else {
                let from_k = me.as_ref().map_or_else(|| "console".into(), |p| key(&p.get_id()));
                let Some(t) = st.reply.get(&from_k).and_then(|k| uuid_of(k)).and_then(|u| server.get_player_by_uuid(u)) else {
                    return say("msg_none", &[]);
                };
                (t, rest(0))
            };
            if msg.trim().is_empty() {
                return say("usage", &[("arg", "/m <player> <message>".into())]);
            }
            let from_name = me.as_ref().map_or_else(|| "Console".into(), Player::get_name);
            let mut colored = true;
            if let Some(p) = &me {
                let d = st.data(p).clone();
                if d.muted(now_secs()) {
                    return vec![more::mute_notice(l, &d)];
                }
                if extra::hidden_from(st, Some(p), &key(&to.get_id())) {
                    return say("no_player", &[("arg", arg(0).unwrap_or("").to_owned())]);
                }
                if st.data(&to).ignored.contains(&key(&p.get_id())) {
                    return say("msg_ignored", &[("name", to.get_name())]);
                }
                colored = player_perm(p, "chat.color");
            }
            let vars = [("from", from_name.clone()), ("to", to.get_name())];
            let format = st.cfg.msg_format.clone();
            to.send_system_message(more::chat_line(&format, &vars, &msg, colored), false);
            sender.send_message(more::chat_line(&format, &vars, &msg, colored));
            if let Some(p) = &me {
                st.reply.insert(key(&to.get_id()), key(&p.get_id()));
                st.reply.insert(key(&p.get_id()), key(&to.get_id()));
            }
            more::social_spy(st, server, &from_name, &to, &msg);
            Vec::new()
        }
        "broadcast" => {
            let msg = rest(0);
            if msg.is_empty() {
                return say("usage", &[("arg", "/broadcast <message>".into())]);
            }
            let line = fmt(l, "broadcast", &[("message", msg)]).replacen(crate::lang::text(l, "prefix"), "", 1);
            for p in server.get_all_players() {
                c::tell(&p, &line);
            }
            c::log_info(&c::plain(&line));
            Vec::new()
        }

        // -- player tools
        "fly" | "god" => {
            let p = match target(0, name) {
                Ok(p) => p,
                Err(e) => return e,
            };
            let k = key(&p.get_id());
            let d = st.data(&p);
            let on = if name == "fly" {
                d.fly = !d.fly;
                d.fly
            } else {
                d.god = !d.god;
                d.god
            };
            if name == "fly" {
                set_fly(&p, on);
            } else {
                p.set_invulnerable(on);
            }
            st.save(&k);
            let msg = fmt(l, name, &[("name", p.get_name()), ("state", on_off(l, on))]);
            if me.as_ref().is_none_or(|m| key(&m.get_id()) != k) {
                c::tell(&p, &msg);
            }
            vec![msg]
        }
        "heal" | "feed" => {
            let p = match target(0, name) {
                Ok(p) => p,
                Err(e) => return e,
            };
            if name == "heal" {
                p.set_health(p.get_max_health());
            }
            p.set_food_level(20);
            p.set_saturation(if name == "heal" { 5.0 } else { 10.0 });
            say(if name == "heal" { "healed" } else { "fed" }, &[("name", p.get_name())])
        }
        "gm" | "gmc" | "gms" | "gma" | "gmsp" => {
            let (mode, ti) = match name {
                "gmc" => (Some(GameMode::Creative), 0),
                "gms" => (Some(GameMode::Survival), 0),
                "gma" => (Some(GameMode::Adventure), 0),
                "gmsp" => (Some(GameMode::Spectator), 0),
                _ => (arg(0).and_then(gamemode), 1),
            };
            let Some(mode) = mode else { return say("usage", &[("arg", "/gm <survival|creative|adventure|spectator> [player]".into())]) };
            let p = match target(ti, "gm") {
                Ok(p) => p,
                Err(e) => return e,
            };
            p.set_gamemode(mode);
            say("gamemode", &[("name", p.get_name()), ("mode", gm_name(mode).into())])
        }
        "speed" => {
            let Some(n) = arg(0).and_then(|s| s.parse::<f32>().ok()).filter(|v| (0.0..=10.0).contains(v)) else {
                return say("usage", &[("arg", "/speed <0-10> [player]".into())]);
            };
            let p = match target(1, "speed") {
                Ok(p) => p,
                Err(e) => return e,
            };
            let flying = p.get_abilities().flying;
            // 1 = vanilla speed, 10 = maximum
            let scale = |def: f32| if n < 1.0 { def * n } else { def + (n - 1.0) / 9.0 * (1.0 - def) };
            if flying {
                p.set_fly_speed(scale(0.05));
            } else {
                p.set_walk_speed(scale(0.1));
            }
            let kind = crate::lang::text(l, if flying { "flyk" } else { "walk" }).to_owned();
            say("speed", &[("kind", kind), ("name", p.get_name()), ("value", n.to_string())])
        }
        "nick" => {
            let Some(nick) = arg(0) else { return say("usage", &[("arg", "/nick <name|off> [player]".into())]) };
            let p = match target(1, "nick") {
                Ok(p) => p,
                Err(e) => return e,
            };
            let k = key(&p.get_id());
            let real = p.get_name();
            let shown = if nick.eq_ignore_ascii_case("off") { None } else { Some(nick.chars().filter(|ch| !ch.is_control()).take(32).collect::<String>()) };
            // A nickname that reads as another player's name would let one player speak as another
            if let Some(n) = &shown {
                let bare = c::plain(n).to_lowercase();
                if bare.trim().is_empty() || st.key_of_name(&bare).is_some_and(|other| other != k) {
                    return say("nick_taken", &[]);
                }
            }
            st.data(&p).nick = shown.clone();
            st.save(&k);
            more::refresh_name(st, &p);
            say("nick", &[("name", real.clone()), ("nick", shown.unwrap_or(real))])
        }

        // -- economy
        "balance" => {
            let sym = st.cfg.currency_symbol.clone();
            match arg(0) {
                Some(n) if me.is_none() || perm(sender, "others.balance") => {
                    let found = st.key_of_name(n).and_then(|k| st.players.get(&k)).cloned();
                    match found {
                        Some(d) => say("balance", &[("name", d.name.clone()), ("money", store::money(d.balance, &sym))]),
                        None => say("no_player", &[("arg", n.to_owned())]),
                    }
                }
                _ => {
                    let p = need_player!();
                    let b = st.data(&p).balance;
                    say("balance", &[("name", p.get_name()), ("money", store::money(b, &sym))])
                }
            }
        }
        "pay" => {
            let p = need_player!();
            let (Some(n), Some(a)) = (arg(0), arg(1)) else { return say("usage", &[("arg", "/pay <player> <amount>".into())]) };
            let Some(amount) = store::parse_amount(a) else { return say("bad_amount", &[("arg", a.to_owned())]) };
            let Some(t) = find_player(server, n).filter(|t| !extra::hidden_from(st, Some(&p), &key(&t.get_id()))) else {
                return say("no_player", &[("arg", n.to_owned())]);
            };
            let (from, to) = (key(&p.get_id()), key(&t.get_id()));
            if from == to {
                return say("tpa_self", &[]);
            }
            let sym = st.cfg.currency_symbol.clone();
            if st.data(&t).pay_off {
                return say("pay_off", &[("name", t.get_name())]);
            }
            if st.data(&t).balance + amount > store::MAX_BALANCE {
                return say("bad_amount", &[("arg", a.to_owned())]);
            }
            st.data(&p);
            let mut a_bal = st.players.get(&from).map_or(0.0, |d| d.balance);
            let mut b_bal = st.players.get(&to).map_or(0.0, |d| d.balance);
            if !store::transfer(&mut a_bal, &mut b_bal, amount) {
                return say("no_money", &[("money", store::money(a_bal, &sym))]);
            }
            if let Some(d) = st.players.get_mut(&from) {
                d.balance = a_bal;
            }
            if let Some(d) = st.players.get_mut(&to) {
                d.balance = b_bal;
            }
            st.save(&from);
            st.save(&to);
            c::tell(&t, &fmt(l, "got_paid", &[("money", store::money(amount, &sym)), ("name", p.get_name())]));
            say("paid", &[("money", store::money(amount, &sym)), ("name", t.get_name())])
        }
        "baltop" => {
            let sym = st.cfg.currency_symbol.clone();
            let mut all: Vec<&PlayerData> = st.players.values().filter(|d| !d.name.is_empty()).collect();
            all.sort_by(|a, b| b.balance.total_cmp(&a.balance));
            let mut out = say("baltop", &[]);
            for (i, d) in all.iter().take(10).enumerate() {
                out.push(fmt(l, "baltop_row", &[("rank", (i + 1).to_string()), ("name", d.name.clone()), ("money", store::money(d.balance, &sym))]));
            }
            out
        }
        "eco" => {
            let (Some(op), Some(n), Some(a)) = (arg(0), arg(1), arg(2)) else {
                return say("usage", &[("arg", "/eco <give|take|set> <player> <amount>".into())]);
            };
            let amount = if op == "set" && a == "0" { Some(0.0) } else { store::parse_amount(a) };
            let Some(amount) = amount else { return say("bad_amount", &[("arg", a.to_owned())]) };
            let sym = st.cfg.currency_symbol.clone();
            let k = match find_player(server, n) {
                Some(t) => {
                    st.data(&t);
                    key(&t.get_id())
                }
                None => match st.key_of_name(n) {
                    Some(k) => k,
                    None => return say("no_player", &[("arg", n.to_owned())]),
                },
            };
            let Some(d) = st.players.get_mut(&k) else { return say("no_player", &[("arg", n.to_owned())]) };
            d.balance = match op {
                "give" => store::credit(d.balance, amount),
                "take" => (d.balance - amount).max(0.0),
                "set" => amount.min(store::MAX_BALANCE),
                _ => return say("usage", &[("arg", "/eco <give|take|set> <player> <amount>".into())]),
            };
            d.balance = (d.balance * 100.0).round() / 100.0;
            let (nm, bal) = (d.name.clone(), d.balance);
            st.save(&k);
            say("eco", &[("name", nm), ("money", store::money(bal, &sym))])
        }
        "holo" => holo::run(st, sender, server, w),
        _ => match plus::run(name, st, sender, server, w) {
            Some(out) => out,
            None => more::run(name, st, sender, server, w),
        },
    }
}

/// Take `[aliases]` from the config; returns the names that cannot be used.
fn load_aliases(st: &mut State) -> Vec<String> {
    ENGLISH.store(st.cfg.language == "en", std::sync::atomic::Ordering::Relaxed);
    let (good, bad) = store::clean_aliases(&st.cfg.aliases);
    st.aliases = good;
    if !bad.is_empty() {
        c::log_info(&format!("config.toml [aliases]: not used (bad name, login command or loop): {}", bad.join(", ")));
    }
    bad
}

// ---- handlers ---------------------------------------------------------------

struct Cmd(&'static str);

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        if !perm(&sender, &format!("command.{}", self.0)) {
            c::reply(&sender, &fmt(lang_now(), "no_perm", &[]));
            return Ok(0);
        }
        let w = c::words(&args, "args");
        for line in run(self.0, &sender, &server, &w) {
            c::reply(&sender, &line);
        }
        // Commands that had to wait for the state to be free (/sudo)
        plus::run_after(&server);
        Ok(1)
    }
}

/// Off: Pumpkin 0.2.2 asks a WASM plugin for suggestions from the console's readline thread,
/// outside its async runtime, and panics there (`pumpkin-wasm-host-v0_1` `commands/executor.rs`,
/// "there is no reactor running"). On a server whose console is a TTY (every panel host) typing
/// `rank ` in the console stopped the server. Turn on again once Pumpkin fixes that.
const SUGGEST_ARGS: bool = false;

struct Sug(&'static str);

impl CommandSuggestionHandler for Sug {
    fn suggest(&self, sender: CommandSender, server: Server, request: SuggestionRequest) -> CommandSuggestions {
        let typed = c::typed_words(&request);
        let players = || server.get_all_players().iter().map(Player::get_name).collect::<Vec<_>>();
        let list: Vec<String> = match (self.0, typed.len()) {
            ("home" | "delhome", 0) => match (c::sender_player(&sender), try_lock().as_mut().and_then(|g| g.as_mut())) {
                (Some(p), Some(st)) => st.data(&p).homes.keys().cloned().collect(),
                _ => Vec::new(),
            },
            ("warp" | "delwarp", 0) => try_lock().as_ref().and_then(|g| g.as_ref()).map(|st| st.warps.keys().cloned().collect()).unwrap_or_default(),
            ("gm", 0) => ["survival", "creative", "adventure", "spectator"].map(String::from).to_vec(),
            ("eco", 0) => ["give", "take", "set"].map(String::from).to_vec(),
            ("basics", 0) => ["reload", "version", "help"].map(String::from).to_vec(),
            ("tpa" | "tpahere" | "tphere" | "m" | "pay" | "tpaccept" | "tpdeny" | "balance", 0) => players(),
            ("setrank" | "perm" | "mute" | "unmute" | "ignore" | "seen" | "whois" | "ping", 0) => players(),
            ("mail", 0) => ["read", "send", "clear"].map(String::from).to_vec(),
            ("mail" | "kit", 1) => players(),
            ("rank", 0) => vec!["list".to_owned()],
            ("setrank", 1) => try_lock().as_ref().and_then(|g| g.as_ref()).map(|st| st.ranks.groups.keys().cloned().collect()).unwrap_or_default(),
            ("kit", 0) => try_lock().as_ref().and_then(|g| g.as_ref()).map(|st| st.kits.kits.keys().cloned().collect()).unwrap_or_default(),
            ("perm", 2) => ["true", "false", "unset"].map(String::from).to_vec(),
            ("fly" | "god" | "heal" | "feed" | "spawn" | "gmc" | "gms" | "gma" | "gmsp", 0) => players(),
            ("gm" | "warp" | "eco" | "speed" | "nick", 1) => players(),
            _ => Vec::new(),
        };
        c::suggest_last(&request, &list)
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

struct OnJoin;
impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, server: Server, e: Data<PlayerJoinEvent>) -> Data<PlayerJoinEvent> {
        // A missed join (state busy) only delays rank, name and mail notice until the next join
        let p = &e.player;
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let k = key(&p.get_id());
            // `joined` is set when the player is welcomed (after a login prompt, if there is one):
            // someone who leaves at the prompt is still new next time
            let first = !st.data(p).joined;
            let d = st.data(p);
            d.name = p.get_name();
            d.last_seen = now_secs();
            if d.first_seen == 0 {
                d.first_seen = d.last_seen;
            }
            let (god, fly) = (d.god, d.fly);
            st.tp_guard.insert(k.clone(), Instant::now());
            st.index_name(&k);
            st.save(&k);
            if god {
                p.set_invulnerable(true);
            }
            if fly {
                set_fly(p, true);
            }
            more::on_join(st, p);
            extra::on_join(st, &server, p);
            // Motd, mail notice, spawn teleport, join line and AFK clock wait for `plus::tick_joining`
            plus::on_join(st, p, first);
        }
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, server: Server, e: Data<PlayerLeaveEvent>) -> Data<PlayerLeaveEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let k = key(&e.player.get_id());
            st.pending.remove(&k);
            st.tpa.retain(|r| r.from != k && r.to != k);
            gui::on_leave(st, &server, &e.player);
            plus::on_leave(st, &server, &e.player);
            more::on_leave(st, &e.player);
            st.tp_guard.remove(&k);
            st.reply.remove(&k);
            st.burst.remove(&k);
            st.tpa_last.remove(&k);
            if let Some(d) = st.players.get_mut(&k) {
                d.last_seen = now_secs();
            }
            st.dirty.remove(&k);
            st.save(&k);
        }
        e
    }
}

struct OnDeath;
impl EventHandler<PlayerDeathEvent> for OnDeath {
    fn handle(&self, _server: Server, e: Data<PlayerDeathEvent>) -> Data<PlayerDeathEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            if st.cfg.back_on_death {
                let k = key(&e.player.get_id());
                let loc = loc_of(&e.player);
                st.data(&e.player).back = Some(loc);
                st.dirty.insert(k);
            }
        }
        e
    }
}

struct OnRespawn;
impl EventHandler<PlayerRespawnEvent> for OnRespawn {
    fn handle(&self, server: Server, mut e: Data<PlayerRespawnEvent>) -> Data<PlayerRespawnEvent> {
        if let Some(st) = try_lock().as_ref().and_then(|g| g.as_ref()) {
            if let (true, Some(s)) = (st.cfg.respawn_at_spawn, &st.spawn) {
                if let Some(w) = world_of(&server, &s.dim) {
                    e.respawned_world = w;
                    e.position = (s.x, s.y, s.z);
                    e.yaw = s.yaw;
                    e.pitch = s.pitch;
                }
            }
        }
        e
    }
}

struct OnTeleport;
impl EventHandler<PlayerTeleportEvent> for OnTeleport {
    fn handle(&self, _server: Server, e: Data<PlayerTeleportEvent>) -> Data<PlayerTeleportEvent> {
        // Vanilla /tp and other plugins: remember the origin for /back
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let (x, y, z) = e.from_position;
            let moved = {
                let (a, b, cz) = e.to_position;
                (a - x).powi(2) + (b - y).powi(2) + (cz - z).powi(2) > 4.0
            };
            st.tp_guard.insert(key(&e.player.get_id()), Instant::now());
            if moved {
                let k = key(&e.player.get_id());
                let dim = e.player.get_world().get_dimension();
                st.data(&e.player).back = Some(Loc { dim, x, y, z, yaw: e.player.get_yaw(), pitch: e.player.get_pitch() });
                st.dirty.insert(k);
            }
        }
        e
    }
}

struct OnChat;
impl EventHandler<PlayerChatEvent> for OnChat {
    fn handle(&self, server: Server, mut e: Data<PlayerChatEvent>) -> Data<PlayerChatEvent> {
        // Cancelled by a plugin that ran before us (a login gate, a script, a filter): not ours to send
        if e.cancelled {
            return e;
        }
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            if more::on_chat(st, &server, &e.player, &e.message) {
                e.cancelled = true;
            }
        }
        e
    }
}

struct OnClick;
impl EventHandler<InventoryClickEvent> for OnClick {
    fn handle(&self, _server: Server, mut e: Data<InventoryClickEvent>) -> Data<InventoryClickEvent> {
        if e.cancelled || c::awaiting_login(&e.player) {
            return e;
        }
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            if !st.wins.is_empty() && gui::on_click(st, &e.player, e.window_type, e.raw_slot, &e.click_type) {
                e.cancelled = true;
            }
        }
        e
    }
}

struct OnClose;
impl EventHandler<InventoryCloseEvent> for OnClose {
    fn handle(&self, server: Server, e: Data<InventoryCloseEvent>) -> Data<InventoryCloseEvent> {
        // Busy = we are opening a window ourselves; that code has ended the old one already
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            if !st.wins.is_empty() {
                gui::on_close(st, &server, &e.player);
            }
        }
        e
    }
}

/// Own command names from `[aliases]`: the text of the command is replaced before the server looks
/// it up, so nothing has to be registered.
struct OnCommandSend;
impl EventHandler<PlayerCommandSendEvent> for OnCommandSend {
    fn handle(&self, _server: Server, mut e: Data<PlayerCommandSendEvent>) -> Data<PlayerCommandSendEvent> {
        // A player at the login prompt types a password: never touch that line
        if e.cancelled || c::awaiting_login(&e.player) {
            return e;
        }
        if let Some(st) = try_lock().as_ref().and_then(|g| g.as_ref()) {
            if !st.aliases.is_empty() {
                if let Some(real) = store::expand_alias(&st.aliases, &e.command) {
                    e.command = real;
                }
            }
        }
        e
    }
}

/// Every 2 server ticks. Skips a round when a command is running (never blocks: WASM has one thread).
fn on_tick(server: Server) {
    tick_state(&server);
    plus::run_after(&server);
}

fn tick_state(server: &Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.ticks += 1;
    if !st.joining.is_empty() {
        plus::tick_joining(st, server);
    }
    if !st.wins.is_empty() && gui::any_mirror(st) && gui::due(&mut st.mirror_tick) {
        gui::tick(st, server);
    }
    if !st.pending.is_empty() {
        tick_pending(st, server);
    }
    if !st.rtp.is_empty() {
        more::tick_rtp(st, server);
    }
    if st.ticks % 10 == 0 {
        let timeout = st.cfg.tpa_timeout;
        st.tpa.retain(|r| r.at.elapsed().as_secs() < timeout);
        more::tick_second(st, server);
        if st.ticks % 50 == 0 {
            st.flush();
        }
        if st.ticks % 100 == 0 {
            holo::tick(st, server);
        }
    }
}

fn tick_pending(st: &mut State, server: &Server) {
    let l = st.cfg.language.clone();
    let keys: Vec<String> = st.pending.keys().cloned().collect();
    for k in keys {
        let Some(p) = uuid_of(&k).and_then(|u| server.get_player_by_uuid(u)) else {
            st.pending.remove(&k);
            continue;
        };
        let Some(pt) = st.pending.get(&k) else { continue };
        let (x, y, z) = p.get_position();
        if !pt.start.0.is_nan() {
            let (sx, sy, sz) = pt.start;
            if (sx - x).powi(2) + (sy - y).powi(2) + (sz - z).powi(2) > 0.25 {
                st.pending.remove(&k);
                c::tell(&p, &fmt(&l, "tp_moved", &[]));
                continue;
            }
        }
        if Instant::now() >= pt.due {
            if let Some(pt) = st.pending.remove(&k) {
                if let Some(loc) = resolve(server, &pt.target) {
                    if pay_and_go(st, server, &p, &loc, pt.cost) && !pt.start.0.is_nan() {
                        c::tell(&p, &fmt(&l, "tp_done", &[]));
                    }
                }
            }
        }
    }
}

// ---- plugin -----------------------------------------------------------------

struct Basics;

impl Plugin for Basics {
    fn new() -> Self {
        Basics
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Server essentials by MCSV".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let mut st = State {
            cfg: c::load_config("config.toml"),
            ranks: c::load_config("ranks.toml"),
            kits: c::load_config("kits.toml"),
            applied: HashMap::new(),
            afk: HashMap::new(),
            rtp: HashMap::new(),
            last_rtp: HashMap::new(),
            protect: HashMap::new(),
            dirty: HashSet::new(),
            tp_guard: HashMap::new(),
            ticks: 0,
            last_announce: Instant::now(),
            announce_idx: 0,
            seed: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0x9E37_79B9, |d| d.as_nanos() as u64) | 1,
            worth: c::load_config("worth.toml"),
            jail: None,
            names: HashMap::new(),
            burst: HashMap::new(),
            tpa_last: HashMap::new(),
            chat_locked: false,
            started: Instant::now(),
            wins: HashMap::new(),
            mirror_tick: Instant::now(),
            shop: store::Shop::default(),
            shop_burst: HashMap::new(),
            aliases: BTreeMap::new(),
            joining: HashMap::new(),
            announced: HashSet::new(),
            bar: None,
            holos: holo::load(),
            players: HashMap::new(),
            warps: BTreeMap::new(),
            spawn: None,
            tpa: Vec::new(),
            pending: HashMap::new(),
            last_tp: HashMap::new(),
            reply: HashMap::new(),
        };
        st.load_all();
        st.shop = gui::load_shop(&st.cfg.language);
        load_aliases(&mut st);
        // The panel reads this to tell "loaded" from "installed, waiting for a restart"
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": now_secs() }));
        let kit_names: Vec<String> = st.kits.kits.keys().cloned().collect();
        *lock() = Some(st);

        let reg = |node: String, default: PermissionDefault| {
            let _ = context.register_permission(&Permission { node, description: "Basics".into(), default, children: vec![] });
        };
        let op = || PermissionDefault::Op(PermissionLevel::Two);
        for extra in ["bypass.delay", "bypass.cooldown", "bypass.afkkick", "homes.unlimited", "warp.*", "kit.*", "chat.color"] {
            reg(format!("{NS}:{extra}"), op());
        }
        for kit in kit_names {
            reg(format!("{NS}:kit.{kit}"), op());
        }
        // Taking items out of another player's inventory is for admins, looking is for staff.
        // Not below `command.invsee`: a granted node covers everything under it.
        reg(format!("{NS}:invsee.modify"), PermissionDefault::Op(PermissionLevel::Three));
        for n in [1u32, 2, 3, 4, 5, 6, 8, 10, 15, 20, 25, 30, 50, 100] {
            reg(format!("{NS}:homes.{n}"), PermissionDefault::Deny);
        }
        for (names, everyone) in COMMANDS {
            let name = names[0];
            let default = match (*everyone, ADMIN_COMMANDS.contains(&name)) {
                (true, _) => PermissionDefault::Allow,
                (false, true) => PermissionDefault::Op(PermissionLevel::Three),
                (false, false) => op(),
            };
            reg(format!("{NS}:command.{name}"), default);
            // Not `command.<name>.others`: Pumpkin lets a granted node cover everything below it
            reg(format!("{NS}:others.{name}"), op());
            let mut args = CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd(name));
            if SUGGEST_ARGS {
                args = args.suggest(Sug(name));
            }
            let cmd = Command::new(&names.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(), "Basics").execute(Cmd(name)).then(args);
            context.register_command(cmd, &format!("{NS}:command.{name}"));
        }
        context.register_event_handler(OnJoin, EventPriority::Normal, false)?;
        context.register_event_handler(OnLeave, EventPriority::Normal, false)?;
        context.register_event_handler(OnDeath, EventPriority::Lowest, false)?;
        context.register_event_handler(OnRespawn, EventPriority::Normal, true)?;
        context.register_event_handler(OnTeleport, EventPriority::Lowest, false)?;
        context.register_event_handler(OnChat, EventPriority::Normal, true)?;
        context.register_event_handler(OnClick, EventPriority::Normal, true)?;
        context.register_event_handler(OnClose, EventPriority::Normal, true)?;
        context.register_event_handler(OnCommandSend, EventPriority::Normal, true)?;
        pumpkin_plugin_api::scheduler::schedule_repeating_task(1, 2, on_tick);
        Ok(())
    }

    /// Money for other plugins (scripts, shops): JSON `{"kind": "balance" | "deposit" | "withdraw" | "set",
    /// "player": <name>, "amount": <number>}` answers `{"ok": true, "balance": <new balance>}` or
    /// `{"ok": false, "error": "...", "balance": <balance>}`. A withdrawal never goes below zero.
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let kind = v.get("kind").and_then(serde_json::Value::as_str).unwrap_or("");
        let name = v.get("player").and_then(serde_json::Value::as_str).unwrap_or("");
        let amount = v.get("amount").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        let mut guard = try_lock().ok_or("busy")?;
        let st = guard.as_mut().ok_or("not loaded")?;
        let k = st.key_of_name(name).ok_or("unknown player")?;
        // `%basics_…%` for the Placeholders plugin: {"kind":"placeholder","params":"balance","player":"Alice"}
        if kind == "placeholder" {
            let d = st.players.get(&k).ok_or("unknown player")?;
            let sym = &st.cfg.currency_symbol;
            let value = match v.get("params").and_then(serde_json::Value::as_str).unwrap_or("") {
                "balance" => Some(store::money(d.balance, "")),
                "balance_formatted" | "money" => Some(store::money(d.balance, sym)),
                "balance_raw" => Some(format!("{:.2}", d.balance)),
                "rank" | "group" => Some(d.group.clone().unwrap_or_else(|| st.ranks.default_group().to_owned())),
                "nick" | "nickname" => Some(d.nick.clone().unwrap_or_else(|| d.name.clone())),
                "homes" => Some(d.homes.len().to_string()),
                "mail" => Some(d.mail.len().to_string()),
                "warnings" => Some(d.warnings.len().to_string()),
                "muted" => Some(if d.muted_until > c::now_secs() { "yes" } else { "no" }.to_owned()),
                "jailed" => Some(if d.jailed_until > c::now_secs() { "yes" } else { "no" }.to_owned()),
                "vanished" => Some(if d.vanished { "yes" } else { "no" }.to_owned()),
                _ => None,
            };
            return Ok(serde_json::json!({ "value": value }).to_string().into_bytes());
        }
        let balance = st.players.get(&k).map_or(0.0, |d| d.balance);
        let refuse = |error: &str| Ok(serde_json::json!({ "ok": false, "error": error, "balance": balance }).to_string().into_bytes());
        let new = match kind {
            "balance" => balance,
            _ if !amount.is_finite() || amount < 0.0 => return refuse("bad amount"),
            "deposit" => store::credit(balance, amount),
            "withdraw" if balance < amount => return refuse("not enough money"),
            "withdraw" => store::credit(balance, -amount),
            "set" => store::credit(0.0, amount),
            _ => return Err(format!("unknown kind {kind}")),
        };
        if new != balance {
            if let Some(d) = st.players.get_mut(&k) {
                d.balance = new;
            }
            st.save(&k);
        }
        Ok(serde_json::json!({ "ok": true, "balance": new }).to_string().into_bytes())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        // Every change is written when it happens; only people online have unsaved play time and
        // positions. Writing every known player here made a big server slow to stop.
        if let Some(st) = lock().as_mut() {
            // Dropping the bar takes it off every screen; a window left open must not keep items
            st.bar = None;
            gui::clear_all(st);
            let online: Vec<String> = st.afk.keys().cloned().collect();
            st.dirty.extend(online);
            st.flush();
            st.save_world();
        }
        c::remove_file("status.json");
        Ok(())
    }
}

register_plugin!(Basics);

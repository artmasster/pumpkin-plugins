//! Pumpkin WASM glue for Perms.
//!
//! How permissions take effect: Pumpkin fires `PlayerPermissionCheckEvent` from
//! `Player::has_permission_at_level` on every check, after its own answer (explicit attachments
//! from other plugins, then the node's registered default against the op level). Our handler is
//! registered blocking, looks the node up in the player's precomputed `Effective` and replaces
//! the answer only when Perms has something to say. Nothing is written into Pumpkin's
//! attachments, so Basics ranks keep working beside it.
//!
//! Two locks: `STATE` (data, config) and `HOT` (what the check handler reads). `HOT` is never
//! held across a host call, so the handler always gets it, also when it is entered from inside
//! one of our own host calls (`has_permission`, resending the command list).
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mcsv_common as c;
use mcsv_common::http::{self, Pending, Request};
use pumpkin_plugin_api::command::{Arg, ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::GameMode;
use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, PlayerJoinEvent, PlayerLeaveEvent, PlayerPermissionCheckEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA, HTTP_OUTBOUND, NETWORK_DNS};
use pumpkin_plugin_api::persistent_data::PersistentDataHolder;
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::uuid::Uuid;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::cmd::{self, Actor, Env, Verbose, WebAsk};
use crate::lang::{P, msg, net_reason};
use crate::model::{Data, Effective};
use crate::node::{Ctx, tokenize, uuid_string};
use crate::store::{self, Config, LogEntry};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NS: &str = "perms";
/// Custom data keys other plugins read from the player (namespace `perms`).
const DATA_KEYS: [&str; 4] = ["prefix", "suffix", "group", "weight"];
/// Distinct nodes remembered from checks for `/lp tree`.
const SEEN_CAP: usize = 4000;
/// Time one tick call may spend on telling clients their new command list.
const RESEND_BUDGET: Duration = Duration::from_millis(4);

type Key = (u64, u64);

struct Cached {
    eff: Effective,
    ctx: Ctx,
    name: String,
    /// Answers already worked out for a player who has regex nodes: a regex runs once per node,
    /// after that the check is one lookup again. Empty and unused for everyone else.
    memo: HashMap<String, Option<bool>>,
}

/// Regex answers remembered per player before the memo starts over.
const MEMO_CAP: usize = 2000;

impl Cached {
    fn new(eff: Effective, ctx: Ctx, name: String) -> Self {
        Cached { eff, ctx, name, memo: HashMap::new() }
    }

    fn check(&mut self, node: &str, exempt: &[String]) -> Option<bool> {
        if self.eff.regex.is_empty() {
            return self.eff.check(node, exempt);
        }
        if let Some(known) = self.memo.get(node) {
            return *known;
        }
        let answer = self.eff.check(node, exempt);
        if self.memo.len() >= MEMO_CAP {
            self.memo.clear();
        }
        self.memo.insert(node.to_owned(), answer);
        answer
    }
}

struct Listener {
    /// None = the console.
    who: Option<Key>,
    filter: String,
    /// Unix seconds; 0 = until turned off.
    until: u64,
    second: u64,
    sent: u32,
}

#[derive(Default)]
struct Hot {
    players: HashMap<Key, Cached>,
    exempt: Vec<String>,
    listeners: Vec<Listener>,
    seen: BTreeSet<String>,
    verbose_max: u32,
    checks: u64,
    answered: u64,
    resends: u64,
    resend_micros: u64,
    /// Nodes Pumpkin asks about when it builds a client's command list: learned while we make
    /// it send one. Only a change to one of these is worth sending the list again.
    command_nodes: HashSet<String>,
    learning: bool,
}

/// What we last put on a player for other plugins and the tab list.
#[derive(PartialEq)]
struct Shown {
    prefix: String,
    suffix: String,
    group: String,
    weight: i32,
    tab: bool,
}

/// A request to the web editor that is on its way (one at a time).
struct WebJob {
    pending: Pending,
    ask: WebAsk,
    /// Who asked: a player, or the console.
    who: Option<Key>,
    actor: String,
    /// Users that did not fit in the session.
    left_out: usize,
    started: Instant,
}

/// Longest a request to the web editor may take before the sender is told it failed.
const WEB_TIMEOUT: Duration = Duration::from_secs(20);

struct State {
    cfg: Config,
    data: Data,
    tick: u64,
    shown: HashMap<Key, Shown>,
    /// Players whose client still has the command list from before a change.
    resend: VecDeque<Key>,
    web: Option<WebJob>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static HOT: Mutex<Option<Hot>> = Mutex::new(None);

/// Never blocks: WASM has one thread and Pumpkin calls handlers from inside host calls.
fn try_lock() -> Option<std::sync::MutexGuard<'static, Option<State>>> {
    match STATE.try_lock() {
        Ok(g) => Some(g),
        Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// Run `f` on the hot data. The lock is released before this returns: never call the host inside.
fn hot<T>(f: impl FnOnce(&mut Hot) -> T) -> Option<T> {
    let mut guard = match HOT.try_lock() {
        Ok(g) => g,
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return None,
    };
    guard.as_mut().map(f)
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn key_of(p: &Player) -> Key {
    let id = p.get_id();
    (id.high, id.low)
}

fn key_text(k: Key) -> String {
    uuid_string(k.0, k.1)
}

fn key_from_text(s: &str) -> Option<Key> {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    Some((u64::from_str_radix(&hex[..16], 16).ok()?, u64::from_str_radix(&hex[16..], 16).ok()?))
}

fn ctx_of(p: &Player) -> Ctx {
    let mode = match p.get_gamemode() {
        GameMode::Survival => "survival",
        GameMode::Creative => "creative",
        GameMode::Adventure => "adventure",
        GameMode::Spectator => "spectator",
    };
    Ctx::new(&p.get_world().get_dimension(), mode)
}

fn data_dir() -> std::path::PathBuf {
    c::data_path("")
}

fn load_cfg() -> Config {
    c::load_config::<Config>("config.toml").cleaned()
}

fn flush(st: &mut State, names: bool) {
    let failed = store::flush(&mut st.data, &data_dir(), names);
    if failed > 0 {
        c::log_info(&format!("could not write {failed} file(s) under plugins/data/perms"));
    }
}

fn sync_hot_cfg(cfg: &Config) {
    hot(|h| {
        h.exempt = cfg.wildcard_exempt.clone();
        h.verbose_max = cfg.verbose_max_per_second;
    });
}

/// Prefix, suffix, primary group and weight on the player for other plugins, and the tab name.
fn publish(st: &mut State, p: &Player, k: Key, eff: &Effective, name: &str) {
    let had_tab = st.shown.get(&k).is_some_and(|s| s.tab);
    let wants_tab = st.cfg.tab_list_prefix && !(eff.prefix.is_empty() && eff.suffix.is_empty());
    let new = Shown { prefix: eff.prefix.clone(), suffix: eff.suffix.clone(), group: eff.primary.clone(), weight: eff.weight, tab: wants_tab };
    if st.shown.get(&k) == Some(&new) {
        return;
    }
    if st.cfg.export_custom_data {
        p.set_string(NS, "prefix", &new.prefix);
        p.set_string(NS, "suffix", &new.suffix);
        p.set_string(NS, "group", &new.group);
        p.set_int(NS, "weight", new.weight);
    } else if p.has_custom_data(NS, "group") {
        for key in DATA_KEYS {
            p.remove_custom_data(NS, key);
        }
    }
    if wants_tab {
        p.set_tab_list_name(Some(c::color(&format!("{}{name}{}", new.prefix, new.suffix))));
    } else if had_tab {
        p.set_tab_list_name(None);
    }
    st.shown.insert(k, new);
}

/// Work out what applies to one online player and make it live: checks are answered from the
/// new set at once. When the answers changed, the client is also due its command list again;
/// that costs one check per command, so it is queued and paid for in `resend_some`.
fn apply_player(st: &mut State, p: &Player) {
    let k = key_of(p);
    let id = key_text(k);
    let ctx = ctx_of(p);
    let name = p.get_name();
    st.data.note_name(&id, &name);
    let eff = st.data.resolve(&id, &ctx, now_secs());
    let fresh = eff.clone();
    let cached = Cached::new(eff, ctx, name.clone());
    let changed = hot(|h| match h.players.insert(k, cached) {
        Some(old) if h.command_nodes.is_empty() => !old.eff.same_perms(&fresh),
        Some(old) => h.command_nodes.iter().any(|n| old.eff.check(n, &h.exempt) != fresh.check(n, &h.exempt)),
        // Nothing was answered for this player yet; only what Perms adds can be missing on the client
        None => !fresh.exact.is_empty() || !fresh.wild.is_empty(),
    })
    .unwrap_or(false);
    publish(st, p, k, &fresh, &name);
    if changed && !st.resend.contains(&k) {
        st.resend.push_back(k);
    }
}

/// Send queued players their command list again, a few per tick. `set_permission_level` with the
/// level the player already has makes Pumpkin do it (`Player::set_permission_lvl` →
/// `send_c_commands_packet`), which asks us about every command while we are still in the call.
fn resend_some(st: &mut State, server: &Server) {
    let started = Instant::now();
    let mut done = 0u64;
    hot(|h| h.learning = true);
    while let Some((high, low)) = st.resend.pop_front() {
        if let Some(p) = server.get_player_by_uuid(Uuid { high, low }) {
            p.set_permission_level(p.get_permission_level());
            done += 1;
        }
        if started.elapsed() >= RESEND_BUDGET {
            break;
        }
    }
    let micros = started.elapsed().as_micros() as u64;
    hot(|h| {
        h.learning = false;
        h.resends += done;
        h.resend_micros += if done > 0 { micros } else { 0 };
    });
}

fn apply_all(st: &mut State, server: &Server) {
    for p in server.get_all_players() {
        apply_player(st, &p);
    }
}

/// First check for a player we have nothing on (the command list is sent before the join event).
fn first_sight(p: &Player, k: Key, node: &str) -> Option<bool> {
    let mut guard = try_lock()?;
    let st = guard.as_mut()?;
    let ctx = ctx_of(p);
    let eff = st.data.resolve(&key_text(k), &ctx, now_secs());
    let mut cached = Cached::new(eff, ctx, p.get_name());
    let answer = cached.check(node, &st.cfg.wildcard_exempt);
    hot(|h| h.players.insert(k, cached));
    answer
}

fn tell_listeners(server: &Server, k: Key, node: &str, server_said: bool, perms: Option<bool>) {
    let now = now_secs();
    let out: Vec<(Option<Key>, String)> = hot(|h| {
        let name = h.players.get(&k).map_or_else(|| key_text(k), |c| c.name.clone());
        let lower = name.to_lowercase();
        let result = perms.unwrap_or(server_said);
        let source = match perms {
            Some(v) if v != server_said => format!("perms, server said {server_said}"),
            Some(_) => "perms".to_owned(),
            None => "server".to_owned(),
        };
        let line = format!("&8[&bv&8] &f{name} &7{node} {} &8({source})", if result { "&atrue" } else { "&cfalse" });
        let max = h.verbose_max.max(1);
        let mut out = Vec::new();
        for l in &mut h.listeners {
            if !l.filter.is_empty() && !node.contains(&l.filter) && !lower.contains(&l.filter) {
                continue;
            }
            if l.second != now {
                l.second = now;
                l.sent = 0;
            }
            if l.sent >= max {
                continue;
            }
            l.sent += 1;
            out.push((l.who, line.clone()));
        }
        out
    })
    .unwrap_or_default();
    for (who, line) in out {
        match who {
            None => c::log_info(&c::plain(&line)),
            Some((high, low)) => {
                if let Some(p) = server.get_player_by_uuid(Uuid { high, low }) {
                    c::tell(&p, &line);
                }
            }
        }
    }
}

/// `/lp log notify`: tell the console and every online holder of the notify node about an edit,
/// except the one who made it and those who switched the notices off.
fn notify_watchers(st: &State, server: &Server, actor: Option<Key>, entry: &LogEntry) {
    let line = msg(st.cfg.th(), "log_notice", &[&entry.actor, &entry.kind, &entry.target, &entry.action]);
    if actor.is_some() {
        c::log_info(&c::plain(&line));
    }
    for p in server.get_all_players() {
        let k = key_of(&p);
        if Some(k) == actor {
            continue;
        }
        let ignoring = st.data.users.get(&key_text(k)).is_some_and(|u| u.nodes.iter().any(|n| n.key == cmd::NOTIFY_IGNORE && n.value));
        if !ignoring && p.has_permission(cmd::NOTIFY_NODE) {
            c::tell(&p, &line);
        }
    }
}

/// `/lp editor` or `/lp applyedits`: hand the request to the host and return at once; the tick
/// task picks up the answer. Err = the line to tell the sender now.
fn start_web(st: &mut State, ask: WebAsk, who: Option<Key>, actor: String) -> Result<(), String> {
    let th = st.cfg.th();
    if st.web.is_some() {
        return Err(msg(th, "web_busy", &[]));
    }
    let base = &st.cfg.editor_url;
    if base.is_empty() {
        return Err(msg(th, "editor_off", &[]));
    }
    let (mut request, left_out) = match &ask {
        WebAsk::Open => {
            let known = hot(|h| h.seen.iter().cloned().collect::<Vec<_>>()).unwrap_or_default();
            let (body, left) = store::editor_session(&st.data, now_secs(), VERSION, &known);
            (Request::post_json(&format!("{base}/api/perms-editor"), body), left)
        }
        WebAsk::Apply(code) => (Request::get(&format!("{base}/api/perms-editor/{code}")), 0),
    };
    request.timeout = WEB_TIMEOUT;
    match http::start(request) {
        Ok(pending) => {
            st.web = Some(WebJob { pending, ask, who, actor, left_out, started: Instant::now() });
            Ok(())
        }
        Err(e) => Err(msg(th, "editor_fail", &[&net_reason(th, &e)])),
    }
}

/// The answer of the web editor, if it has come: a link for the sender, or the changes applied.
fn poll_web(st: &mut State, server: &Server) {
    let Some(job) = st.web.as_mut() else { return };
    let result = match job.pending.poll() {
        Some(r) => r,
        None if job.started.elapsed() > WEB_TIMEOUT + Duration::from_secs(5) => Err("timed out".to_owned()),
        None => return,
    };
    let Some(job) = st.web.take() else { return };
    let th = st.cfg.th();
    let mut link = None;
    let line = match (&job.ask, result) {
        (_, Err(e)) => msg(th, "editor_fail", &[&net_reason(th, &e)]),
        (WebAsk::Open, Ok(resp)) => {
            let code = serde_json::from_slice::<serde_json::Value>(&resp.body).ok().and_then(|v| v["code"].as_str().map(str::to_owned)).filter(|c| resp.status / 100 == 2 && store::valid_code(c));
            match code {
                Some(code) => {
                    let url = format!("{}/{code}", st.cfg.editor_url);
                    let left = if job.left_out > 0 { msg(th, "editor_left", &[&job.left_out.to_string()]) } else { String::new() };
                    let line = msg(th, "editor_link", &[&url, &left]);
                    link = Some(url);
                    line
                }
                None => msg(th, "editor_fail", &[&net_reason(th, &format!("HTTP {}", resp.status))]),
            }
        }
        (WebAsk::Apply(_), Ok(resp)) if resp.status == 404 => msg(th, "edits_gone", &[]),
        (WebAsk::Apply(_), Ok(resp)) if resp.status != 200 => msg(th, "editor_fail", &[&net_reason(th, &format!("HTTP {}", resp.status))]),
        (WebAsk::Apply(code), Ok(resp)) => match store::apply_edits(&mut st.data, &resp.text()) {
            Ok(done) => {
                flush(st, false);
                apply_all(st, server);
                let entry = LogEntry {
                    t: now_secs(),
                    actor: job.actor.clone(),
                    actor_id: job.who.map(key_text).unwrap_or_default(),
                    kind: "S".into(),
                    target: "web-editor".into(),
                    action: format!("applyedits {code}: {} groups, {} players, {} tracks set, {} deleted", done.groups, done.users, done.tracks, done.deleted),
                };
                if store::append_log(&data_dir(), &entry).is_err() {
                    c::log_info("could not write plugins/data/perms/log.jsonl");
                }
                notify_watchers(st, server, job.who, &entry);
                let refused = if done.refused > 0 { msg(th, "import_refused", &[&done.refused.to_string()]) } else { String::new() };
                msg(th, "edits_done", &[&done.groups.to_string(), &done.users.to_string(), &done.tracks.to_string(), &done.deleted.to_string(), &refused])
            }
            Err(e) => msg(th, "edits_fail", &[&e.chars().take(120).collect::<String>()]),
        },
    };
    let line = format!("{P}{line}");
    let player = job.who.and_then(|(high, low)| server.get_player_by_uuid(Uuid { high, low }));
    match player {
        Some(p) => {
            c::tell(&p, &line);
            if let Some(url) = link {
                // A link in a chat line cannot be clicked or copied: this one opens it
                let button = c::color(if th { "&b&n[กดที่นี่เพื่อเปิดหน้าเว็บแก้สิทธิ์]" } else { "&b&n[Click here to open the web editor]" });
                p.send_system_message(button.click_open_url(&url), false);
            }
        }
        // The console, or a player who left meanwhile
        None => c::log_info(&c::plain(&line)),
    }
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.tick += 1;
    let now = now_secs();
    if st.web.is_some() {
        poll_web(st, &server);
    }
    // Temporary nodes: gone the moment their time is up (this runs every other tick)
    if now >= st.data.next_expiry && st.data.purge(now) {
        flush(st, false);
        apply_all(st, &server);
    }
    if !st.resend.is_empty() {
        resend_some(st, &server);
    }
    if st.tick % 5 != 0 {
        return;
    }
    // World and game mode are contexts: look twice a second who moved between them, and pick
    // up anyone a join handler missed
    let players = server.get_all_players();
    let mut online: HashSet<Key> = HashSet::with_capacity(players.len());
    for p in &players {
        let k = key_of(p);
        online.insert(k);
        let ctx = ctx_of(p);
        let stale = hot(|h| h.players.get(&k).is_none_or(|c| c.ctx != ctx)).unwrap_or(false) || !st.shown.contains_key(&k);
        if stale {
            apply_player(st, p);
        }
    }
    if st.tick % 50 == 0 {
        // Checks for players who already left would otherwise stay cached for ever
        st.shown.retain(|k, _| online.contains(k));
        let th = st.cfg.th();
        let timed_out: Vec<Option<Key>> = hot(|h| {
            h.players.retain(|k, _| online.contains(k));
            let mut gone = Vec::new();
            h.listeners.retain(|l| {
                let keep = (l.until == 0 || now < l.until) && l.who.is_none_or(|k| online.contains(&k));
                if !keep {
                    gone.push(l.who);
                }
                keep
            });
            gone
        })
        .unwrap_or_default();
        for who in timed_out {
            let line = format!("{P}{}", msg(th, "verbose_off", &[]));
            match who {
                None => c::log_info(&c::plain(&line)),
                Some((high, low)) => {
                    if let Some(p) = server.get_player_by_uuid(Uuid { high, low }) {
                        c::tell(&p, &line);
                    }
                }
            }
        }
    }
    if st.tick % 300 == 0 {
        flush(st, true);
    }
}

struct Cmd;

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let raw = match args.get_value("args") {
            Arg::Simple(s) | Arg::Msg(s) => s,
            _ => String::new(),
        };
        let words = tokenize(&raw);
        let player = c::sender_player(&sender);
        // Login (login on offline-mode servers) may still be waiting for this player's password
        if player.as_ref().is_some_and(c::awaiting_login) {
            return Ok(0);
        }
        let Some(mut guard) = try_lock() else {
            c::reply(&sender, &format!("{P}&cระบบไม่ว่าง ลองใหม่อีกครั้ง (busy, try again)"));
            return Ok(0);
        };
        let Some(st) = guard.as_mut() else { return Ok(0) };
        let th = st.cfg.th();
        let who = player.as_ref().map(key_of);
        let dir = data_dir();
        let has = |node: &str| player.as_ref().is_none_or(|p| p.has_permission(node));
        let actor = Actor {
            console: player.is_none(),
            bypass: player.as_ref().is_none_or(|p| p.get_permission_level() as u8 >= PermissionLevel::Four as u8),
            uuid: who.map(key_text),
            name: player.as_ref().map_or_else(|| "console".to_owned(), Player::get_name),
            has: &has,
        };
        let online = |uuid: &str| -> Option<Ctx> {
            let k = key_from_text(uuid)?;
            hot(|h| h.players.get(&k).map(|c| c.ctx.clone())).flatten()
        };
        let final_check = |uuid: &str, node: &str| -> Option<bool> {
            let (high, low) = key_from_text(uuid)?;
            server.get_player_by_uuid(Uuid { high, low }).map(|p| p.has_permission(node))
        };
        let seen = || hot(|h| h.seen.iter().cloned().collect::<Vec<_>>()).unwrap_or_default();
        let stats = hot(|h| (h.players.len(), h.checks, h.answered, h.listeners.len(), h.resends, h.resend_micros)).unwrap_or_default();
        let env = Env { now: now_secs(), th, dir: &dir, version: VERSION, exempt: &st.cfg.wildcard_exempt, online: &online, final_check: &final_check, seen: &seen, stats };
        let out = cmd::run(&mut st.data, &actor, &env, &words);
        for line in &out.lines {
            c::reply(&sender, line);
        }
        if out.sync {
            flush(st, true);
            st.cfg = load_cfg();
            sync_hot_cfg(&st.cfg);
            let store::Loaded { data, bad_files, bad_entries } = store::load(&dir, &st.cfg.default_group);
            st.data = data;
            flush(st, false);
            for line in cmd::sync_report(st.cfg.th(), &st.data, &bad_files, &bad_entries) {
                c::reply(&sender, &line);
            }
            // The config may have turned the tab name or the custom data on or off
            st.shown.clear();
            apply_all(st, &server);
        } else if out.changed {
            apply_all(st, &server);
        }
        if let Some(entry) = &out.logged {
            notify_watchers(st, &server, who, entry);
        }
        if let Some(ask) = out.web {
            let name = player.as_ref().map_or_else(|| "console".to_owned(), Player::get_name);
            if let Err(line) = start_web(st, ask, who, name) {
                c::reply(&sender, &format!("{P}{line}"));
            }
        }
        match out.verbose {
            Some(Verbose::On(filter)) => {
                let until = if st.cfg.verbose_timeout == 0 { 0 } else { now_secs() + st.cfg.verbose_timeout };
                hot(|h| {
                    h.listeners.retain(|l| l.who != who);
                    if h.listeners.len() < 16 {
                        h.listeners.push(Listener { who, filter, until, second: 0, sent: 0 });
                    }
                });
            }
            Some(Verbose::Off) => {
                hot(|h| h.listeners.retain(|l| l.who != who));
            }
            None => {}
        }
        Ok(1)
    }
}

type EventData<E> = <E as FromIntoEvent>::Data;

struct OnCheck;
impl EventHandler<PlayerPermissionCheckEvent> for OnCheck {
    fn handle(&self, server: Server, mut e: EventData<PlayerPermissionCheckEvent>) -> EventData<PlayerPermissionCheckEvent> {
        let k = key_of(&e.player);
        // (known player, Perms's answer, anyone listening)
        let Some((known, mut answer, listening)) = hot(|h| {
            h.checks += 1;
            if h.seen.len() < SEEN_CAP && e.permission.len() <= 160 && !h.seen.contains(&e.permission) {
                h.seen.insert(e.permission.clone());
            }
            if h.learning && h.command_nodes.len() < SEEN_CAP && !h.command_nodes.contains(&e.permission) {
                h.command_nodes.insert(e.permission.clone());
            }
            let listening = !h.listeners.is_empty();
            match h.players.get_mut(&k) {
                Some(c) => (true, c.check(&e.permission, &h.exempt), listening),
                None => (false, None, listening),
            }
        }) else {
            return e;
        };
        if !known {
            answer = first_sight(&e.player, k, &e.permission);
        }
        // Nobody locks the owner out of the tool that repairs a lockout: a denial of Perms's
        // own command nodes (a `-*` on the default group, say) does not reach level 4 operators
        if answer == Some(false) && e.permission.starts_with("perms:command.") && e.player.get_permission_level() as u8 >= PermissionLevel::Four as u8 {
            answer = None;
        }
        let server_said = e.permission_result;
        if let Some(v) = answer {
            e.permission_result = v;
            hot(|h| h.answered += 1);
        }
        if listening {
            tell_listeners(&server, k, &e.permission, server_said, answer);
        }
        e
    }
}

struct OnJoin;
impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _server: Server, e: EventData<PlayerJoinEvent>) -> EventData<PlayerJoinEvent> {
        // When the state is busy the tick task picks the player up within half a second
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            apply_player(st, &e.player);
        }
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, e: EventData<PlayerLeaveEvent>) -> EventData<PlayerLeaveEvent> {
        let k = key_of(&e.player);
        hot(|h| {
            h.players.remove(&k);
            h.listeners.retain(|l| l.who != Some(k));
        });
        // The values would outlive Perms in the player's saved data: take them off again
        for key in DATA_KEYS {
            e.player.remove_custom_data(NS, key);
        }
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            st.shown.remove(&k);
        }
        e
    }
}

struct Perms;

impl Plugin for Perms {
    fn new() -> Self {
        Perms
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Permissions by MCSV".into(),
            dependencies: vec![],
            // HTTP and DNS: the web editor (`/lp editor`, `/lp applyedits`); nothing else goes out
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into(), HTTP_OUTBOUND.into(), NETWORK_DNS.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let cfg = load_cfg();
        let store::Loaded { data, bad_files, bad_entries } = store::load(&data_dir(), &cfg.default_group);
        for b in bad_files.iter().take(20) {
            c::log_info(&format!("skipped a file that cannot be read: {b}"));
        }
        for b in bad_entries.iter().take(20) {
            c::log_info(&format!("skipped an entry that is not valid: {b}"));
        }
        let hidden = bad_files.len().saturating_sub(20) + bad_entries.len().saturating_sub(20);
        if hidden > 0 {
            c::log_info(&format!("… and {hidden} more; /lp sync lists them again"));
        }
        c::log_info(&format!("{} groups, {} players with data, {} tracks", data.groups.len(), data.users.len(), data.tracks.len()));
        *HOT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Hot::default());
        sync_hot_cfg(&cfg);
        let mut st = State { cfg, data, tick: 0, shown: HashMap::new(), resend: VecDeque::new(), web: None };
        flush(&mut st, true);
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(st);

        // A node that is not registered answers false even for operators
        for (area, owner) in cmd::AREAS {
            let level = if *owner { PermissionLevel::Four } else { PermissionLevel::Three };
            let _ = context.register_permission(&Permission {
                node: format!("{NS}:command.{area}"),
                description: "Perms".into(),
                default: PermissionDefault::Op(level),
                children: vec![],
            });
        }
        // The panel reads this to tell "loaded" from "installed, waiting for a restart"
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": now_secs() }));

        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let names = ["lp".to_owned(), "luckperms".to_owned(), NS.to_owned()];
        let command = Command::new(&names, "Perms")
            .execute(Cmd)
            .then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(command, &format!("{NS}:command.base"));
        // Blocking: only blocking handlers may change the result
        context.register_event_handler(OnCheck, EventPriority::Normal, true)?;
        context.register_event_handler(OnJoin, EventPriority::Normal, false)?;
        context.register_event_handler(OnLeave, EventPriority::Normal, false)?;
        pumpkin_plugin_api::scheduler::schedule_repeating_task(20, 2, on_tick);
        Ok(())
    }

    fn on_unload(&self, context: Context) -> Result<(), String> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            flush(st, true);
        }
        for p in context.get_server().get_all_players() {
            for key in DATA_KEYS {
                p.remove_custom_data(NS, key);
            }
        }
        c::remove_file("status.json");
        Ok(())
    }
}

register_plugin!(Perms);

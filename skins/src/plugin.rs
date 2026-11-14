//! Pumpkin WASM glue for Skins.
//!
//! When a skin takes effect: Pumpkin fires `PlayerLoginEvent` before the player is put into the
//! world, and the player's profile (with its `textures` property) is sent to everyone, the
//! player included, right after. A skin that is already known (fetched before, or chosen with
//! `/skin`) is set in that event and is simply there from the first moment. A skin that has to
//! be fetched first arrives a moment later: it is set on the profile and, with
//! `instant-refresh`, the player is respawned where they stand (`teleport_world` to their own
//! world), which makes Pumpkin take the player off everyone else's list and put them back with
//! the new profile, entity and all. Pumpkin does not send the player their own profile again
//! (and a plugin cannot: the player-info packet is not one the host writes out), so the
//! player's own view of themselves changes when they next join. Nothing is sent by hand.
//!
//! Fetching never blocks: requests are handed to the host and polled from the tick task.
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use mcsv_common as c;
use mcsv_common::http::{self, Pending, Request};
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, PlayerJoinEvent, PlayerLeaveEvent, PlayerLoginEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA, HTTP_OUTBOUND, NETWORK_DNS};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::wit::pumpkin::plugin::player::PlayerSkin;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::cfg::{self, Config};
use crate::store::{self, Cached, Choice, Lookup, Store};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NS: &str = "skins";
const P: &str = "&8[&bSkins&8]&r ";
/// Requests to Mojang and MineSkin under way at once; more wait in the queue.
const MAX_JOBS: usize = 6;
const MAX_QUEUE: usize = 64;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// A skin being made by MineSkin is asked about this often, this many times.
const MINESKIN_POLL: Duration = Duration::from_secs(2);
const MINESKIN_POLLS: u8 = 15;
/// A player is not respawned for a new skin sooner than this many ticks after joining.
const SETTLE_TICKS: u64 = 40;

/// Whom to tell how a fetch went: the sender of a `/skin` command.
#[derive(Clone)]
struct Tell {
    /// `None` = the console.
    who: Option<String>,
    /// The player whose skin it is.
    target: String,
}

enum Stage {
    /// Name → account id (api.mojang.com).
    Id,
    /// Account id → signed textures (sessionserver.mojang.com).
    Profile,
    /// Image link → signed textures (api.mineskin.org).
    Make,
    /// MineSkin is still making it: ask again at the time given.
    Wait(String, Instant, u8),
}

struct Job {
    key: String,
    choice: Choice,
    stage: Stage,
    pending: Option<Pending>,
    /// Players (as they are named) who get the skin when it arrives.
    wearers: Vec<String>,
    tell: Vec<Tell>,
}

struct State {
    cfg: Config,
    store: Store,
    tick: u64,
    jobs: Vec<Job>,
    queue: VecDeque<(Choice, Option<String>, Option<Tell>)>,
    /// Lowercase name → Unix second until which `/skin` is refused.
    cooldown: HashMap<String, u64>,
    /// Lowercase name → tick of the join, for players who are in the world.
    joined: HashMap<String, u64>,
    /// Players to respawn for a new skin, and the tick from which.
    refresh: Vec<(u64, String)>,
    fetched: u64,
    failed: u64,
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
        // The console, or a player who left meanwhile
        None => c::log_info(&c::plain(line)),
    }
}

/// What a player should look like when nothing was chosen for them: their own account's skin.
fn own(cfg: &Config, name: &str) -> Option<Choice> {
    (cfg.restore_own_skin && cfg::valid_name(name)).then(|| Choice::Name { skin: name.to_ascii_lowercase() })
}

fn skin_of(c: &Cached) -> PlayerSkin {
    PlayerSkin { value: c.value.clone(), signature: Some(c.signature.clone()) }
}

/// The textures `name` should wear now, if they are known; starts a fetch when they are not
/// (or are old). `tell` goes with that fetch.
fn resolve(st: &mut State, name: &str, tell: Option<Tell>) -> Option<Cached> {
    let now = c::now_secs();
    let keep = st.cfg.cache_hours * 3600;
    let chosen = st.store.choice(name).cloned();
    let explicit = chosen.is_some();
    let Some(choice) = chosen.or_else(|| own(&st.cfg, name)).or_else(|| store::pick_default(&st.cfg.default_skins, name).map(|s| Choice::Name { skin: s.to_owned() })) else {
        return None;
    };
    let found = match st.store.lookup(&choice.key(), now, keep) {
        Lookup::Have(c) => Ok(Some(c.clone())),
        Lookup::Missing => Ok(None),
        Lookup::Fetch { stale } => Err(stale.cloned()),
    };
    match found {
        Ok(Some(c)) => Some(c),
        // Their own name is not an account: one of the server's default skins instead
        Ok(None) if !explicit => {
            let fallback = store::pick_default(&st.cfg.default_skins, name).map(|s| Choice::Name { skin: s.to_owned() })?;
            if fallback == choice {
                return None;
            }
            let found = match st.store.lookup(&fallback.key(), now, keep) {
                Lookup::Have(c) => Ok(Some(c.clone())),
                Lookup::Missing => Ok(None),
                Lookup::Fetch { stale } => Err(stale.cloned()),
            };
            match found {
                Ok(c) => c,
                Err(stale) => {
                    fetch(st, fallback, Some(name.to_owned()), tell);
                    stale
                }
            }
        }
        Ok(None) => None,
        Err(stale) => {
            fetch(st, choice, Some(name.to_owned()), tell);
            stale
        }
    }
}

fn request(cfg: &Config, choice: &Choice, stage: &Stage, id: &str) -> Request {
    let mut req = match (choice, stage) {
        (Choice::Name { skin }, Stage::Id) => Request::get(&format!("https://api.mojang.com/users/profiles/minecraft/{skin}")),
        (_, Stage::Profile) => Request::get(&format!("https://sessionserver.mojang.com/session/minecraft/profile/{id}?unsigned=false")),
        (_, Stage::Wait(job, _, _)) => Request::get(&format!("https://api.mineskin.org/v2/queue/{job}")),
        (Choice::Url { url, slim }, _) => Request::post_json(
            "https://api.mineskin.org/v2/generate",
            serde_json::json!({ "url": url, "variant": if *slim { "slim" } else { "classic" }, "visibility": "unlisted" }).to_string(),
        ),
        // A name is never "made"
        (Choice::Name { skin }, Stage::Make) => Request::get(&format!("https://api.mojang.com/users/profiles/minecraft/{skin}")),
    };
    req.timeout = REQUEST_TIMEOUT;
    req = req.header("accept", "application/json").header("user-agent", &format!("PumpkinSkins/{VERSION}"));
    if matches!(choice, Choice::Url { .. }) && !cfg.mineskin_api_key.is_empty() {
        req = req.header("authorization", &format!("Bearer {}", cfg.mineskin_api_key));
    }
    req
}

/// Start fetching `choice` (or join the fetch of it that is under way). `wearer` gets it when
/// it arrives.
fn fetch(st: &mut State, choice: Choice, wearer: Option<String>, tell: Option<Tell>) {
    let key = choice.key();
    if let Some(job) = st.jobs.iter_mut().find(|j| j.key == key) {
        if let Some(w) = wearer {
            if !job.wearers.iter().any(|x| x.eq_ignore_ascii_case(&w)) {
                job.wearers.push(w);
            }
        }
        job.tell.extend(tell);
        return;
    }
    if st.jobs.len() >= MAX_JOBS {
        if st.queue.len() < MAX_QUEUE {
            st.queue.push_back((choice, wearer, tell));
        }
        return;
    }
    let stage = if matches!(choice, Choice::Name { .. }) { Stage::Id } else { Stage::Make };
    let pending = http::start(request(&st.cfg, &choice, &stage, "")).ok();
    let mut job = Job { key, choice, stage, pending, wearers: wearer.into_iter().collect(), tell: tell.into_iter().collect() };
    if job.pending.is_none() {
        // The host refused the request outright (no permission, bad address)
        job.stage = Stage::Wait(String::new(), Instant::now(), MINESKIN_POLLS);
    }
    st.jobs.push(job);
}

/// How a fetch ended.
enum Outcome {
    Got(String, String),
    /// No such account.
    Missing,
    Failed(String),
}

/// A few words out of an error answer (`{"error":"…"}`, `{"errors":[{"message":"…"}]}`).
fn error_text(status: u16, body: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let text = v.get("error").and_then(|e| e.as_str()).or_else(|| v.get("errorMessage").and_then(|e| e.as_str())).or_else(|| v.get("errors").and_then(|e| e.get(0)).and_then(|e| e.get("message")).and_then(|e| e.as_str())).or_else(|| v.get("message").and_then(|e| e.as_str()));
    match text {
        Some(t) => format!("{status} {}", t.chars().filter(|c| !c.is_control()).take(80).collect::<String>()),
        None => format!("HTTP {status}"),
    }
}

/// Move one job on with what has arrived. `None` = still under way.
fn advance(cfg: &Config, job: &mut Job) -> Option<Outcome> {
    if let Stage::Wait(id, at, left) = &job.stage {
        if job.pending.is_none() {
            if id.is_empty() {
                return Some(Outcome::Failed("request refused".into()));
            }
            if Instant::now() < *at {
                return None;
            }
            if *left == 0 {
                return Some(Outcome::Failed("mineskin.org took too long".into()));
            }
            job.pending = http::start(request(cfg, &job.choice, &job.stage, "")).ok();
            if job.pending.is_none() {
                return Some(Outcome::Failed("request refused".into()));
            }
        }
    }
    let resp = match job.pending.as_mut()?.poll()? {
        Ok(r) => r,
        Err(e) => return Some(Outcome::Failed(e.chars().take(80).collect())),
    };
    job.pending = None;
    let body = resp.text();
    match &job.stage {
        Stage::Id => match resp.status {
            200 => match store::profile_id(&body) {
                Some(id) => {
                    job.stage = Stage::Profile;
                    job.pending = http::start(request(cfg, &job.choice, &job.stage, &id)).ok();
                    if job.pending.is_none() { Some(Outcome::Failed("request refused".into())) } else { None }
                }
                None => Some(Outcome::Failed("unreadable answer from Mojang".into())),
            },
            204 | 404 => Some(Outcome::Missing),
            429 => Some(Outcome::Failed("Mojang is busy (rate limit)".into())),
            s => Some(Outcome::Failed(error_text(s, &body))),
        },
        Stage::Profile => match resp.status {
            200 => match store::textures(&body) {
                Some((v, s)) => Some(Outcome::Got(v, s)),
                None => Some(Outcome::Failed("unreadable answer from Mojang".into())),
            },
            204 | 404 => Some(Outcome::Missing),
            429 => Some(Outcome::Failed("Mojang is busy (rate limit)".into())),
            s => Some(Outcome::Failed(error_text(s, &body))),
        },
        Stage::Make | Stage::Wait(..) => {
            let left = if let Stage::Wait(_, _, left) = &job.stage { *left } else { MINESKIN_POLLS };
            if let Some((v, s)) = store::textures(&body) {
                return Some(Outcome::Got(v, s));
            }
            match store::job_id(&body) {
                Some(id) if resp.status < 400 && left > 0 => {
                    job.stage = Stage::Wait(id, Instant::now() + MINESKIN_POLL, left - 1);
                    None
                }
                _ => Some(Outcome::Failed(error_text(resp.status, &body))),
            }
        }
    }
}

/// Put a skin on a player who is on the server; the respawn that shows it comes a little later.
fn wear(st: &mut State, p: &Player, skin: &Cached) {
    if p.get_skin().is_some_and(|s| s.value == skin.value) {
        return;
    }
    p.set_skin(&skin_of(skin));
    let name = p.get_name().to_ascii_lowercase();
    if st.cfg.instant_refresh {
        // Only a player who is in the world is respawned; one still logging in gets it with the first spawn
        if let Some(joined) = st.joined.get(&name) {
            let due = (st.tick + 2).max(joined + SETTLE_TICKS);
            st.refresh.retain(|(_, n)| *n != name);
            st.refresh.push((due, name));
        }
    }
}

/// Respawn a player where they stand, so that everyone (they too) gets their profile again.
fn respawn(p: &Player) {
    let (x, y, z) = p.get_position();
    p.teleport_world(p.get_world(), (x, y, z), Some(p.get_yaw()), Some(p.get_pitch()));
}

fn finish(st: &mut State, server: &Server, job: Job, outcome: Outcome) {
    let th = st.cfg.language != "en";
    let now = c::now_secs();
    let label: String = job.choice.label().chars().take(60).collect();
    match outcome {
        Outcome::Got(value, signature) => {
            st.fetched += 1;
            let skin = Cached { value, signature, missing: false, at: now };
            st.store.put(&job.key, skin.clone());
            for name in &job.wearers {
                // Still what they should wear? (a second /skin may have come meanwhile)
                let wanted = st.store.choice(name).cloned().or_else(|| own(&st.cfg, name));
                let still = wanted.as_ref().is_none_or(|w| w.key() == job.key) || store::pick_default(&st.cfg.default_skins, name).is_some_and(|d| format!("name:{d}") == job.key);
                if let (true, Some(p)) = (still, server.get_player_by_name(name)) {
                    wear(st, &p, &skin);
                }
            }
            for t in &job.tell {
                let how = if server.get_player_by_name(&t.target).is_none() {
                    if th { " &7(มีผลเมื่อผู้เล่นเข้าเซิร์ฟ)" } else { " &7(applies when the player joins)" }
                } else if st.cfg.instant_refresh {
                    if th { " &7(คนอื่นเห็นแล้ว เจ้าตัวจะเห็น skin ใหม่ของตัวเองเมื่อเข้าเซิร์ฟใหม่)" } else { " &7(others see it now; the player sees their own new skin after rejoining)" }
                } else if th {
                    " &7(เห็นผลเมื่อเข้าเซิร์ฟใหม่)"
                } else {
                    " &7(seen after rejoining)"
                };
                say(server, &t.who, &msg(th, &format!("&aเปลี่ยน skin ของ &f{} &aเป็น &f{label} &aแล้ว{how}", t.target), &format!("&aThe skin of &f{} &ais now &f{label}{how}", t.target)));
            }
        }
        Outcome::Missing => {
            st.store.put(&job.key, Cached { missing: true, at: now, ..Default::default() });
            for t in &job.tell {
                say(server, &t.who, &msg(th, &format!("&cไม่มีบัญชี Minecraft ชื่อ &f{label}"), &format!("&cThere is no Minecraft account named &f{label}")));
            }
            // Someone whose own name is not an account gets a default skin, if the server has any
            for name in job.wearers.clone() {
                if st.store.choice(&name).is_none() {
                    if let (Some(skin), Some(p)) = (resolve(st, &name, None), server.get_player_by_name(&name)) {
                        wear(st, &p, &skin);
                    }
                }
            }
        }
        Outcome::Failed(why) => {
            st.failed += 1;
            c::log_info(&format!("could not fetch the skin {label}: {why}"));
            for t in &job.tell {
                say(server, &t.who, &msg(th, &format!("&cหา skin &f{label} &cไม่สำเร็จ &7({why}) ลองใหม่อีกครั้งในอีกสักครู่"), &format!("&cThe skin &f{label} &ccould not be fetched &7({why}); try again in a moment")));
            }
        }
    }
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.tick += 2;
    let mut i = 0;
    while i < st.jobs.len() {
        match advance(&st.cfg, &mut st.jobs[i]) {
            Some(outcome) => {
                let job = st.jobs.swap_remove(i);
                finish(st, &server, job, outcome);
            }
            None => i += 1,
        }
    }
    while st.jobs.len() < MAX_JOBS {
        let Some((choice, wearer, tell)) = st.queue.pop_front() else { break };
        fetch(st, choice, wearer, tell);
    }
    if !st.refresh.is_empty() {
        let tick = st.tick;
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut st.refresh).into_iter().partition(|(at, _)| *at <= tick);
        st.refresh = later;
        for (_, name) in due {
            if let Some(p) = server.get_player_by_name(&name) {
                respawn(&p);
            }
        }
    }
    if st.tick % 200 == 0 {
        let now = c::now_secs();
        st.cooldown.retain(|_, until| *until > now);
        if !st.store.flush(&c::data_path("")) {
            c::log_info("could not write plugins/data/skins");
        }
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

/// Before the player is put into the world: a skin that is known is simply there from the start.
struct OnLogin;
impl EventHandler<PlayerLoginEvent> for OnLogin {
    fn handle(&self, _server: Server, e: Data<PlayerLoginEvent>) -> Data<PlayerLoginEvent> {
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let name = e.player.get_name();
        // An online-mode server already gave the player their real, signed skin: only a skin
        // that was chosen with /skin goes over it
        let signed = e.player.get_skin().is_some_and(|s| s.signature.is_some_and(|x| !x.is_empty()));
        if signed && st.store.choice(&name).is_none() {
            return e;
        }
        if let Some(skin) = resolve(st, &name, None) {
            e.player.set_skin(&skin_of(&skin));
        }
        e
    }
}

struct OnJoin;
impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _server: Server, e: Data<PlayerJoinEvent>) -> Data<PlayerJoinEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let tick = st.tick;
            st.joined.insert(e.player.get_name().to_ascii_lowercase(), tick);
        }
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, e: Data<PlayerLeaveEvent>) -> Data<PlayerLeaveEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let name = e.player.get_name().to_ascii_lowercase();
            st.joined.remove(&name);
            st.refresh.retain(|(_, n)| *n != name);
        }
        e
    }
}

fn help(th: bool) -> Vec<String> {
    let lines: &[(&str, &str)] = &[
        ("&bSkins &7ให้ผู้เล่นมี skin บนเซิร์ฟ offline mode และเปลี่ยน skin ได้ด้วยคำสั่ง", "&bSkins &7gives players their skin on an offline-mode server, and lets them change it"),
        ("&f/skin <ชื่อบัญชี> &7ใช้ skin ของบัญชี Minecraft นั้น เช่น /skin Notch", "&f/skin <account name> &7wear the skin of that Minecraft account, like /skin Notch"),
        ("&f/skin url <ลิงก์รูป .png> [slim] &7ใช้ skin จากลิงก์รูป (slim = แขนเล็ก)", "&f/skin url <link to a .png> [slim] &7wear a skin from an image link (slim = thin arms)"),
        ("&f/skin clear &7กลับไปใช้ skin ของชื่อตัวเอง · &f/skin update &7ดึง skin ใหม่ · &f/skin info", "&f/skin clear &7back to the skin of your own name · &f/skin update &7fetch it again · &f/skin info"),
        ("&7ผู้ดูแล: ต่อท้ายชื่อผู้เล่นเพื่อทำแทนคนอื่น เช่น &f/skin set Notch Alice &7· &f/skin reload &7| &fversion", "&7Staff: add a player name to do it for someone else, like &f/skin set Notch Alice &7· &f/skin reload &7| &fversion"),
    ];
    lines.iter().map(|(t, e)| format!("{P}{}", if th { *t } else { *e })).collect()
}

struct Cmd;

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let me = c::sender_player(&sender);
        if me.as_ref().is_some_and(c::awaiting_login) {
            return Ok(0);
        }
        let Some(mut guard) = try_lock() else { return Ok(0) };
        let Some(st) = guard.as_mut() else { return Ok(0) };
        let th = st.cfg.language != "en";
        let admin = me.as_ref().is_none_or(|p| p.has_permission(&format!("{NS}:admin")));
        let sub = w.first().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
        // `/skin Notch` is `/skin set Notch`
        let (sub, rest): (&str, &[String]) = match sub.as_str() {
            "" | "help" => ("help", &[]),
            "set" | "url" | "clear" | "update" | "info" | "reload" | "version" => (sub.as_str(), &w[1..]),
            _ => ("set", &w[..]),
        };
        let reply = |line: String| c::reply(&sender, &line);
        match sub {
            "help" => help(th).into_iter().for_each(reply),
            "version" => reply(msg(th, &format!("&fSkins {VERSION} &7· ดึงมาแล้ว {} skin ล้มเหลว {} · จำไว้ {} skin, ผู้เล่นที่เลือกเอง {}", st.fetched, st.failed, st.store.cache.len(), st.store.players.len()), &format!("&fSkins {VERSION} &7· {} skins fetched, {} failed · {} kept, {} players chose their own", st.fetched, st.failed, st.store.cache.len(), st.store.players.len()))),
            "reload" => {
                if !admin {
                    reply(msg(th, "&cคำสั่งนี้สำหรับผู้ดูแล", "&cThis is for staff"));
                    return Ok(0);
                }
                st.cfg = load();
                reply(msg(st.cfg.language != "en", "&aโหลดไฟล์ตั้งค่าใหม่แล้ว", "&aConfiguration reloaded"));
            }
            _ => {
                // The words before an optional player name at the end
                let own_args = match sub {
                    "set" => 1,
                    "url" => 1 + usize::from(rest.get(1).is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "slim" | "classic"))),
                    _ => 0,
                };
                let target = match rest.get(own_args) {
                    Some(name) if admin && cfg::valid_name(name) => server.get_player_by_name(name).map_or_else(|| name.clone(), |p| p.get_name()),
                    Some(_) if !admin => {
                        reply(msg(th, "&cเปลี่ยน skin ให้คนอื่นได้เฉพาะผู้ดูแล", "&cOnly staff may change someone else's skin"));
                        return Ok(0);
                    }
                    Some(name) => {
                        reply(msg(th, &format!("&cชื่อผู้เล่น &f{} &cไม่ถูกต้อง", name.chars().take(20).collect::<String>()), &format!("&cBad player name &f{}", name.chars().take(20).collect::<String>())));
                        return Ok(0);
                    }
                    None => match &me {
                        Some(p) => p.get_name(),
                        None => {
                            reply(msg(th, "&eจาก console ต้องบอกชื่อผู้เล่นด้วย เช่น &fskin set Notch Alice", "&eFrom the console name the player too, like &fskin set Notch Alice"));
                            return Ok(0);
                        }
                    },
                };
                let key = target.to_ascii_lowercase();
                let tell = Tell { who: me.as_ref().map(Player::get_name), target: target.clone() };
                if sub == "info" {
                    let line = match st.store.choice(&target) {
                        Some(Choice::Name { skin }) => msg(th, &format!("&f{target} &7เลือก skin ของบัญชี &f{skin}"), &format!("&f{target} &7chose the skin of the account &f{skin}")),
                        Some(Choice::Url { url, slim }) => msg(th, &format!("&f{target} &7ใช้ skin จากลิงก์ &f{url}{}", if *slim { " &7(slim)" } else { "" }), &format!("&f{target} &7wears a skin from &f{url}{}", if *slim { " &7(slim)" } else { "" })),
                        None if st.cfg.restore_own_skin => msg(th, &format!("&f{target} &7ใช้ skin ของชื่อตัวเอง (ถ้าชื่อนี้ไม่มีบัญชีจะได้ skin เริ่มต้นของเซิร์ฟ)"), &format!("&f{target} &7wears the skin of their own name (or a default skin of the server when there is no such account)")),
                        None => msg(th, &format!("&f{target} &7ไม่ได้เลือก skin"), &format!("&f{target} &7has not chosen a skin")),
                    };
                    reply(line);
                    return Ok(1);
                }
                if !admin {
                    if !st.cfg.players_can_change {
                        reply(msg(th, "&cเซิร์ฟนี้ไม่ให้ผู้เล่นเปลี่ยน skin เอง", "&cPlayers may not change their skin on this server"));
                        return Ok(0);
                    }
                    let now = c::now_secs();
                    if let Some(until) = st.cooldown.get(&key).filter(|u| **u > now) {
                        let left = until - now;
                        reply(msg(th, &format!("&eรออีก {left} วินาทีก่อนเปลี่ยน skin อีกครั้ง"), &format!("&eWait {left} more seconds before changing your skin again")));
                        return Ok(0);
                    }
                    if st.cfg.cooldown_seconds > 0 {
                        st.cooldown.insert(key.clone(), now + st.cfg.cooldown_seconds);
                    }
                }
                match sub {
                    "set" => {
                        let Some(skin) = rest.first().filter(|s| cfg::valid_name(s)) else {
                            reply(msg(th, "&cชื่อบัญชีใช้ได้เฉพาะ A-Z a-z 0-9 _ ไม่เกิน 16 ตัว &7(จะใช้ลิงก์รูปให้พิมพ์ /skin url <ลิงก์>)", "&cAn account name is A-Z a-z 0-9 _, at most 16 &7(for an image link use /skin url <link>)"));
                            return Ok(0);
                        };
                        if st.cfg.blocked(skin) && !admin {
                            reply(msg(th, &format!("&cเซิร์ฟนี้ไม่ให้ใช้ skin &f{skin}"), &format!("&cThe skin &f{skin} &cis not allowed here")));
                            return Ok(0);
                        }
                        st.store.choose(&target, Some(Choice::Name { skin: skin.to_ascii_lowercase() }));
                    }
                    "url" => {
                        if !st.cfg.allow_url && !admin {
                            reply(msg(th, "&cเซิร์ฟนี้ไม่ให้ใช้ skin จากลิงก์", "&cSkins from links are turned off on this server"));
                            return Ok(0);
                        }
                        let Some(url) = rest.first().filter(|u| cfg::valid_url(u)) else {
                            reply(msg(th, "&cลิงก์ไม่ถูกต้อง &7ต้องเป็นลิงก์ตรงไปที่ไฟล์รูป skin เช่น https://…/skin.png", "&cThat is not a usable link &7(it must point straight at a skin image, like https://…/skin.png)"));
                            return Ok(0);
                        };
                        let slim = rest.get(1).is_some_and(|v| v.eq_ignore_ascii_case("slim"));
                        st.store.choose(&target, Some(Choice::Url { url: url.clone(), slim }));
                    }
                    "clear" => st.store.choose(&target, None),
                    // update: forget what is kept for the skin they wear, so that it is fetched again
                    _ => {
                        if let Some(choice) = st.store.choice(&target).cloned().or_else(|| own(&st.cfg, &target)) {
                            if st.store.cache.remove(&choice.key()).is_some() {
                                st.store.cache_dirty = true;
                            }
                        }
                    }
                }
                st.store.flush(&c::data_path(""));
                let label: String = st.store.choice(&target).map(|c| c.label().chars().take(60).collect()).unwrap_or_default();
                let before = st.jobs.len() + st.queue.len();
                let skin = resolve(st, &target, Some(tell));
                let fetching = st.jobs.len() + st.queue.len() > before || st.jobs.iter().any(|j| j.wearers.iter().any(|x| x.eq_ignore_ascii_case(&target)));
                let online = server.get_player_by_name(&target);
                if let (Some(skin), Some(p)) = (&skin, &online) {
                    wear(st, p, skin);
                }
                let line = if fetching {
                    msg(th, "&7กำลังหา skin… จะบอกเมื่อเสร็จ", "&7Fetching the skin… you will be told when it is done")
                } else if sub == "clear" {
                    msg(th, &format!("&aล้าง skin ที่เลือกของ &f{target} &aแล้ว กลับไปใช้ของชื่อตัวเอง"), &format!("&aThe chosen skin of &f{target} &ais cleared; back to the skin of their own name"))
                } else if skin.is_some() {
                    let how = if online.is_none() {
                        if th { " &7(มีผลเมื่อผู้เล่นเข้าเซิร์ฟ)" } else { " &7(applies when the player joins)" }
                    } else if st.cfg.instant_refresh {
                        if th { " &7(คนอื่นเห็นแล้ว เจ้าตัวจะเห็น skin ใหม่ของตัวเองเมื่อเข้าเซิร์ฟใหม่)" } else { " &7(others see it now; the player sees their own new skin after rejoining)" }
                    } else if th {
                        " &7(เห็นผลเมื่อเข้าเซิร์ฟใหม่)"
                    } else {
                        " &7(seen after rejoining)"
                    };
                    msg(th, &format!("&aเปลี่ยน skin ของ &f{target} &aเป็น &f{label} &aแล้ว{how}"), &format!("&aThe skin of &f{target} &ais now &f{label}{how}"))
                } else {
                    msg(th, &format!("&cไม่มีบัญชี Minecraft ชื่อ &f{label}"), &format!("&cThere is no Minecraft account named &f{label}"))
                };
                reply(line);
            }
        }
        Ok(1)
    }
}

struct Skins;

impl Plugin for Skins {
    fn new() -> Self {
        Skins
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Skins by MCSV".into(),
            dependencies: vec![],
            // HTTP and DNS: skins are fetched from Mojang (api.mojang.com, sessionserver.mojang.com) and mineskin.org
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into(), HTTP_OUTBOUND.into(), NETWORK_DNS.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let cfg = load();
        let store = Store::open(&c::data_path(""));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg, store, tick: 0, jobs: Vec::new(), queue: VecDeque::new(), cooldown: HashMap::new(), joined: HashMap::new(), refresh: Vec::new(), fetched: 0, failed: 0 });
        // The panel reads this to tell "loaded" from "installed, waiting for a restart"
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": c::now_secs() }));
        let _ = context.register_permission(&Permission { node: format!("{NS}:command"), description: "Skins".into(), default: PermissionDefault::Allow, children: vec![] });
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Skins".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let cmd = Command::new(&["skin".to_owned(), NS.to_owned()], "Skins")
            .execute(Cmd)
            .then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:command"));
        // Blocking: it has to be done before the player is put into the world
        context.register_event_handler(OnLogin, EventPriority::Normal, true)?;
        context.register_event_handler(OnJoin, EventPriority::Normal, false)?;
        context.register_event_handler(OnLeave, EventPriority::Normal, false)?;
        pumpkin_plugin_api::scheduler::schedule_repeating_task(2, 2, on_tick);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        if let Some(st) = STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut() {
            st.store.flush(&c::data_path(""));
        }
        c::remove_file("status.json");
        Ok(())
    }
}

register_plugin!(Skins);

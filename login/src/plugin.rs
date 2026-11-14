//! Pumpkin WASM glue for Login.
//!
//! The password never reaches the command dispatcher: `PlayerCommandSendEvent` is where
//! `/login` and friends are handled and cancelled, so Pumpkin neither runs nor logs them
//! ("Player (x): executed command /login …" is written only for commands that were not cancelled).
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::GameMode;
use pumpkin_plugin_api::events::{
    BlockBreakEvent, BlockPlaceEvent, EventHandler, EventPriority, FromIntoEvent, InventoryClickEvent, PlayerChatEvent,
    PlayerCommandSendEvent, PlayerDropItemEvent, PlayerInteractEntityEvent, PlayerInteractEvent, PlayerJoinEvent,
    PlayerLeaveEvent, PlayerLoginEvent,
};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::persistent_data::PersistentDataHolder;
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::wit::pumpkin::plugin::server::CommandSender as ServerSender;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::auth::{self, Account, Accounts, Budget, Config, Guard, Session};
use crate::lang::{PREFIX, span, t};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NS: &str = "login";
const ACCOUNTS: &str = "accounts.json";
const PLAYER_VERBS: &[&str] = &["login", "l", "register", "reg", "changepassword", "changepw", "logout", "unregister"];

/// A player who joined and has not given the password yet.
struct Waiting {
    name: String,
    ip: String,
    since: u64,
    tries: u32,
    last_remind: u64,
}

struct State {
    cfg: Config,
    accounts: Accounts,
    /// Last-login details changed since the file was written.
    dirty: bool,
    waiting: HashMap<String, Waiting>,
    /// Online players who are through.
    passed: HashSet<String>,
    sessions: HashMap<String, Session>,
    guard: Guard,
    budget: Budget,
    /// Last password command per player (milliseconds), to slow guessing.
    last_try: HashMap<String, u64>,
    secs: u64,
}

/// What to do once the state lock is released: kicking fires our own leave handler.
enum After {
    Nothing,
    Kick(String, String),
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
    c::save_json(ACCOUNTS, &st.accounts);
    st.dirty = false;
}

fn say(p: &Player, thai: bool, key: &'static str) {
    c::tell(p, &format!("{PREFIX}{}", t(thai, key)));
}

fn kick(server: &Server, name: &str, reason: &str) {
    server.execute_command(&format!("kick {name} {reason}"), ServerSender::Console);
}

fn run(server: &Server, after: After) {
    if let After::Kick(name, reason) = after {
        kick(server, &name, &reason);
    }
}

fn prompt(st: &State, p: &Player, registered: bool) {
    let th = st.cfg.thai();
    let (line, title) = match (registered, st.cfg.confirm_password) {
        (true, _) => ("ask-login", "title-login"),
        (false, true) => ("ask-register", "title-register"),
        (false, false) => ("ask-register-1", "title-register"),
    };
    say(p, th, line);
    p.show_title(c::color(t(th, title)));
}

/// Hold a player until the password comes.
fn hold(st: &mut State, p: &Player) {
    let name = p.get_name();
    let now = c::now_secs();
    p.set_movement_locked(true);
    p.set_invulnerable(true);
    // Other plugins read this to stay out of the way (`mcsv_common::awaiting_login`)
    p.set_long(c::LOGIN_NS, c::LOGIN_KEY, (now + st.cfg.login_timeout + 5) as i64);
    let registered = st.accounts.contains_key(&auth::key_of(&name));
    let k = key(p);
    st.passed.remove(&k);
    st.waiting.insert(k, Waiting { ip: auth::ip_of(&p.get_ip()), name, since: now, tries: 0, last_remind: now });
    prompt(st, p, registered);
}

/// Give a held player back what `hold` took.
fn release(p: &Player) {
    p.set_movement_locked(false);
    p.set_invulnerable(matches!(p.get_gamemode(), GameMode::Creative | GameMode::Spectator));
    p.remove_custom_data(c::LOGIN_NS, c::LOGIN_KEY);
}

/// The player is through: free them and remember the visit.
fn pass(st: &mut State, p: &Player) {
    let k = key(p);
    let now = c::now_secs();
    if let Some(w) = st.waiting.remove(&k) {
        release(p);
        let lower = auth::key_of(&w.name);
        st.guard.clear(&auth::guard_key(&w.ip, &lower));
        if let Some(acc) = st.accounts.get_mut(&lower) {
            acc.last_ip = w.ip.clone();
            acc.last_login = now;
            st.dirty = true;
        }
        if st.cfg.session_minutes > 0 {
            st.sessions.insert(lower, Session { ip: w.ip, until: now + st.cfg.session_minutes * 60 });
        }
    }
    st.passed.insert(k);
}

fn is_waiting(p: &Player) -> bool {
    try_lock().as_ref().and_then(|g| g.as_ref()).is_some_and(|st| st.waiting.contains_key(&key(p)))
}

/// One password command per second per player, and a server-wide cap on hashing.
fn may_hash(st: &mut State, k: &str) -> Result<(), &'static str> {
    let now = now_ms();
    if st.last_try.get(k).is_some_and(|last| now.saturating_sub(*last) < 1000) {
        return Err("slow");
    }
    st.last_try.insert(k.to_owned(), now);
    if st.budget.take(now) { Ok(()) } else { Err("busy") }
}

fn new_hash(password: &str) -> Option<String> {
    auth::hash_password(password, &auth::new_salt()?)
}

fn account(name: &str, hash: String, ip: &str) -> Account {
    Account { name: name.to_owned(), hash, reg_ip: ip.to_owned(), reg_at: c::now_secs(), last_ip: String::new(), last_login: 0 }
}

/// A command typed by a player. Returns whether to swallow it (never run, never logged).
fn on_command(st: &mut State, server: &Server, p: &Player, raw: &str) -> (bool, After) {
    let words: Vec<&str> = raw.trim_start_matches('/').split_whitespace().collect();
    let Some(first) = words.first().map(|w| w.to_lowercase()) else { return (false, After::Nothing) };
    let th = st.cfg.thai();
    let k = key(p);
    let name = p.get_name();
    let lower = auth::key_of(&name);
    let waiting = st.waiting.contains_key(&k);
    let confirm = st.cfg.confirm_password;
    match first.as_str() {
        "login" | "l" => {
            if !waiting {
                say(p, th, "already-in");
            } else if words.len() < 2 {
                say(p, th, "usage-login");
            } else if !st.accounts.contains_key(&lower) {
                say(p, th, "not-registered");
            } else if let Err(why) = may_hash(st, &k) {
                say(p, th, why);
            } else if st.accounts.get(&lower).is_some_and(|a| auth::verify_password(words[1], &a.hash)) {
                // Hashes made with an older, cheaper setting are renewed while we hold the password
                if st.accounts.get(&lower).is_some_and(|a| auth::needs_rehash(&a.hash)) {
                    if let (Some(hash), Some(acc)) = (new_hash(words[1]), st.accounts.get_mut(&lower)) {
                        acc.hash = hash;
                    }
                }
                pass(st, p);
                say(p, th, "logged-in");
            } else {
                let limit = st.cfg.max_tries;
                let (ip, tries) = match st.waiting.get_mut(&k) {
                    Some(w) => {
                        w.tries += 1;
                        (w.ip.clone(), w.tries)
                    }
                    None => (String::new(), limit),
                };
                c::log_info(&format!("{name} gave a wrong password ({tries}/{limit})"));
                if st.guard.fail(&auth::guard_key(&ip, &lower), c::now_secs(), limit * 2, st.cfg.lock_minutes * 60) {
                    c::log_info(&format!("{name} is refused from that address for {} minutes after repeated wrong passwords", st.cfg.lock_minutes));
                    let reason = t(th, "kick-locked").replace("{mins}", &st.cfg.lock_minutes.to_string());
                    return (true, After::Kick(name, reason));
                }
                if tries >= limit {
                    return (true, After::Kick(name, t(th, "kick-tries").to_owned()));
                }
                c::tell(p, &format!("{PREFIX}{}", t(th, "wrong").replace("{left}", &(limit - tries).to_string())));
            }
            (true, After::Nothing)
        }
        "register" | "reg" => {
            let ip = auth::ip_of(&p.get_ip());
            if st.accounts.contains_key(&lower) {
                say(p, th, "already-registered");
            } else if words.len() < if confirm { 3 } else { 2 } {
                say(p, th, if confirm { "usage-register" } else { "usage-register-1" });
            } else if confirm && words[1] != words[2] {
                say(p, th, "mismatch");
            } else if let Some(why) = auth::password_problem(words[1], &name, st.cfg.min_password) {
                c::tell(p, &format!("{PREFIX}{}", t(th, why).replace("{min}", &st.cfg.min_password.to_string())));
            } else if st.cfg.max_accounts_per_ip > 0 && auth::accounts_from(&st.accounts, &ip) >= st.cfg.max_accounts_per_ip as usize {
                say(p, th, "ip-limit");
            } else if let Err(why) = may_hash(st, &k) {
                say(p, th, why);
            } else if let Some(hash) = new_hash(words[1]) {
                st.accounts.insert(lower, account(&name, hash, &ip));
                pass(st, p);
                save(st);
                c::log_info(&format!("{name} registered"));
                say(p, th, "registered");
            } else {
                say(p, th, "failed");
            }
            (true, After::Nothing)
        }
        "changepassword" | "changepw" => {
            if waiting {
                say(p, th, "login-first");
            } else if !st.accounts.contains_key(&lower) {
                say(p, th, "not-registered");
            } else if words.len() < 3 {
                say(p, th, "usage-change");
            } else if let Some(why) = auth::password_problem(words[2], &name, st.cfg.min_password) {
                c::tell(p, &format!("{PREFIX}{}", t(th, why).replace("{min}", &st.cfg.min_password.to_string())));
            } else if let Err(why) = may_hash(st, &k) {
                say(p, th, why);
            } else if !st.accounts.get(&lower).is_some_and(|a| auth::verify_password(words[1], &a.hash)) {
                st.guard.fail(&auth::guard_key(&auth::ip_of(&p.get_ip()), &lower), c::now_secs(), st.cfg.max_tries * 2, st.cfg.lock_minutes * 60);
                say(p, th, "wrong-old");
            } else if let (Some(hash), Some(acc)) = (new_hash(words[2]), st.accounts.get_mut(&lower)) {
                acc.hash = hash;
                save(st);
                c::log_info(&format!("{name} changed their password"));
                say(p, th, "changed");
            } else {
                say(p, th, "failed");
            }
            (true, After::Nothing)
        }
        "logout" => {
            if waiting {
                say(p, th, "login-first");
            } else if !st.accounts.contains_key(&lower) {
                say(p, th, "not-registered");
            } else {
                st.sessions.remove(&lower);
                say(p, th, "logged-out");
                hold(st, p);
            }
            (true, After::Nothing)
        }
        "unregister" => {
            if waiting {
                say(p, th, "login-first");
            } else if !st.accounts.contains_key(&lower) {
                say(p, th, "not-registered");
            } else if words.len() < 2 {
                say(p, th, "usage-unregister");
            } else if let Err(why) = may_hash(st, &k) {
                say(p, th, why);
            } else if !st.accounts.get(&lower).is_some_and(|a| auth::verify_password(words[1], &a.hash)) {
                st.guard.fail(&auth::guard_key(&auth::ip_of(&p.get_ip()), &lower), c::now_secs(), st.cfg.max_tries * 2, st.cfg.lock_minutes * 60);
                say(p, th, "wrong-old");
            } else {
                st.accounts.remove(&lower);
                st.sessions.remove(&lower);
                save(st);
                c::log_info(&format!("{name} removed their account"));
                say(p, th, "unregistered");
                if st.cfg.force_register {
                    hold(st, p);
                }
            }
            (true, After::Nothing)
        }
        // An admin setting someone's password in game: keep that out of the console log too
        "auth" | "authme" if !waiting && matches!(words.get(1).map(|w| w.to_lowercase()).as_deref(), Some("setpassword" | "register")) => {
            if !p.has_permission(&format!("{NS}:admin")) {
                return (false, After::Nothing);
            }
            let (line, after) = admin(st, server, &words[1..]);
            c::tell(p, &format!("{PREFIX}{line}"));
            (true, after)
        }
        other => {
            if !waiting || st.cfg.allowed_commands.iter().any(|a| a == other) {
                return (false, After::Nothing);
            }
            prompt(st, p, st.accounts.contains_key(&lower));
            (true, After::Nothing)
        }
    }
}

fn when(th: bool, at: u64) -> String {
    if at == 0 {
        return t(th, "never").to_owned();
    }
    t(th, "ago").replace("{t}", &span(th, c::now_secs().saturating_sub(at)))
}

/// `/auth <sub> …` (also `/authme`) from the console or an admin.
fn admin(st: &mut State, server: &Server, w: &[&str]) -> (String, After) {
    let th = st.cfg.thai();
    let name = w.get(1).copied().unwrap_or("");
    let lower = auth::key_of(name);
    let sub = w.first().map(|s| s.to_lowercase()).unwrap_or_default();
    let line = match sub.as_str() {
        "version" => format!("&fLogin {VERSION}"),
        "reload" => {
            st.cfg = load();
            t(st.cfg.thai(), "a-reloaded").to_owned()
        }
        "status" => t(th, "a-status")
            .replace("{accounts}", &st.accounts.len().to_string())
            .replace("{waiting}", &st.waiting.len().to_string())
            .replace("{in}", &st.passed.len().to_string())
            .replace("{locked}", &st.guard.locked_count().to_string())
            .replace("{sessions}", &st.sessions.len().to_string()),
        "info" => match st.accounts.get(&lower) {
            Some(a) => {
                let state = match server.get_player_by_name(&a.name) {
                    Some(p) if st.waiting.contains_key(&key(&p)) => "s-waiting",
                    Some(_) => "s-in",
                    None => "s-off",
                };
                t(th, "a-info")
                    .replace("{name}", &a.name)
                    .replace("{reg}", &when(th, a.reg_at))
                    .replace("{regip}", if a.reg_ip.is_empty() { "-" } else { &a.reg_ip })
                    .replace("{last}", &when(th, a.last_login))
                    .replace("{lastip}", if a.last_ip.is_empty() { "-" } else { &a.last_ip })
                    .replace("{state}", t(th, state))
            }
            None => t(th, "a-none").replace("{name}", name),
        },
        "register" | "setpassword" => {
            let Some(password) = w.get(2) else { return (t(th, "a-help").to_owned(), After::Nothing) };
            let exists = st.accounts.contains_key(&lower);
            if sub == "register" && exists {
                t(th, "a-exists").replace("{name}", name)
            } else if sub == "register" && !auth::valid_name(name) {
                t(th, "a-bad-name").to_owned()
            } else if sub == "setpassword" && !exists {
                t(th, "a-none").replace("{name}", name)
            } else if let Some(why) = auth::password_problem(password, name, st.cfg.min_password) {
                t(th, why).replace("{min}", &st.cfg.min_password.to_string())
            } else if let Some(hash) = new_hash(password) {
                let done = if let Some(acc) = st.accounts.get_mut(&lower) {
                    acc.hash = hash;
                    "a-set"
                } else {
                    st.accounts.insert(lower.clone(), account(name, hash, ""));
                    "a-created"
                };
                // Whoever holds the old password must not ride an old session
                st.sessions.remove(&lower);
                save(st);
                t(th, done).replace("{name}", name)
            } else {
                t(th, "failed").to_owned()
            }
        }
        "unregister" => match st.accounts.remove(&lower) {
            Some(a) => {
                st.sessions.remove(&lower);
                save(st);
                let line = t(th, "a-removed").replace("{name}", &a.name);
                // An online owner has to come back and register again
                if st.cfg.force_register && server.get_player_by_name(&a.name).is_some() {
                    return (line, After::Kick(a.name, t(th, "kick-removed").to_owned()));
                }
                line
            }
            None => t(th, "a-none").replace("{name}", name),
        },
        "forcelogin" => match server.get_player_by_name(name) {
            Some(p) if st.waiting.contains_key(&key(&p)) => {
                pass(st, &p);
                say(&p, th, "logged-in");
                t(th, "a-forced").replace("{name}", &p.get_name())
            }
            _ => t(th, "a-not-waiting").replace("{name}", name),
        },
        "import" => {
            // A file the owner put into plugins/data/login/: plain name only, never a path
            let ok = !name.is_empty() && name.len() <= 64 && name.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')) && !name.starts_with('.');
            if !ok {
                return (t(th, "a-import-usage").to_owned(), After::Nothing);
            }
            let path = c::data_path(name);
            match std::fs::metadata(&path).ok().filter(|m| m.is_file() && m.len() <= 32 * 1024 * 1024).and_then(|_| std::fs::read_to_string(&path).ok()) {
                Some(text) => {
                    let (found, skipped) = auth::parse_import(&text);
                    let mut added = 0;
                    for (account_name, hash) in found {
                        let k = auth::key_of(&account_name);
                        if !st.accounts.contains_key(&k) {
                            st.accounts.insert(k, account(&account_name, hash, ""));
                            added += 1;
                        }
                    }
                    if added > 0 {
                        save(st);
                    }
                    t(th, "a-imported").replace("{n}", &added.to_string()).replace("{skipped}", &skipped.to_string())
                }
                None => t(th, "a-import-missing").replace("{name}", name),
            }
        }
        "unlock" if !name.is_empty() => t(th, "a-unlocked").replace("{n}", &st.guard.unlock(&lower).to_string()),
        "purge" => match name.parse::<u64>() {
            Ok(days) if days >= 30 => {
                let cut = c::now_secs().saturating_sub(days * 86_400);
                let online: HashSet<String> = server.get_all_players().iter().map(|p| auth::key_of(&p.get_name())).collect();
                let before = st.accounts.len();
                st.accounts.retain(|k, a| online.contains(k) || a.last_login.max(a.reg_at) >= cut);
                let gone = before - st.accounts.len();
                if gone > 0 {
                    save(st);
                }
                t(th, "a-purged").replace("{days}", &days.to_string()).replace("{n}", &gone.to_string())
            }
            _ => t(th, "a-purge-usage").to_owned(),
        },
        _ => t(th, "a-help").to_owned(),
    };
    (line, After::Nothing)
}

/// Once a second: reminders, the countdown, timeouts, housekeeping.
fn on_tick(server: Server) {
    let mut kicks = Vec::new();
    {
        let Some(mut guard) = try_lock() else { return };
        let Some(st) = guard.as_mut() else { return };
        st.secs += 1;
        let now = c::now_secs();
        let th = st.cfg.thai();
        let players = server.get_all_players();
        let online: HashSet<String> = players.iter().map(key).collect();
        st.waiting.retain(|k, _| online.contains(k));
        st.passed.retain(|k| online.contains(k));
        st.last_try.retain(|k, _| online.contains(k));
        if !st.waiting.is_empty() {
            for p in &players {
                let k = key(p);
                let Some(w) = st.waiting.get_mut(&k) else { continue };
                let spent = now.saturating_sub(w.since);
                if spent >= st.cfg.login_timeout {
                    kicks.push((w.name.clone(), t(th, "kick-timeout").to_owned()));
                    continue;
                }
                c::action_bar(p, &t(th, "bar-left").replace("{secs}", &(st.cfg.login_timeout - spent).to_string()));
                if now.saturating_sub(w.last_remind) >= st.cfg.remind_interval {
                    w.last_remind = now;
                    let registered = st.accounts.contains_key(&auth::key_of(&w.name));
                    prompt(st, p, registered);
                }
            }
        }
        if st.secs % 30 == 0 {
            st.guard.prune(now);
            st.sessions.retain(|_, s| s.until > now);
            if st.dirty {
                save(st);
            }
        }
    }
    for (name, reason) in kicks {
        kick(&server, &name, &reason);
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

struct OnLogin;
impl EventHandler<PlayerLoginEvent> for OnLogin {
    fn handle(&self, server: Server, mut e: Data<PlayerLoginEvent>) -> Data<PlayerLoginEvent> {
        let Some(guard) = try_lock() else { return e };
        let Some(st) = guard.as_ref() else { return e };
        let th = st.cfg.thai();
        let name = e.player.get_name();
        let lower = auth::key_of(&name);
        let ip = auth::ip_of(&e.player.get_ip());
        let refuse = if let Some(left) = st.guard.locked(&auth::guard_key(&ip, &lower), c::now_secs()) {
            Some(t(th, "kick-locked").replace("{mins}", &left.div_ceil(60).to_string()))
        } else if st.cfg.strict_names && e.player.as_java().is_some() && !auth::valid_name(&name) {
            Some(t(th, "kick-name").to_owned())
        } else if let Some(acc) = st.accounts.get(&lower).filter(|a| a.name != name) {
            // "steve" must not walk in beside (or instead of) "Steve"
            Some(t(th, "kick-case").replace("{name}", &acc.name))
        } else if server.get_all_players().iter().any(|o| auth::key_of(&o.get_name()) == lower) {
            Some(t(th, "kick-online").to_owned())
        } else {
            None
        };
        if let Some(reason) = refuse {
            e.kick_message = c::color(&reason);
            e.cancelled = true;
        }
        e
    }
}

struct OnJoin;
impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _server: Server, e: Data<PlayerJoinEvent>) -> Data<PlayerJoinEvent> {
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let p = &e.player;
        let th = st.cfg.thai();
        let lower = auth::key_of(&p.get_name());
        let ip = auth::ip_of(&p.get_ip());
        let registered = st.accounts.contains_key(&lower);
        if st.cfg.bedrock_auto_login && p.as_bedrock().is_some() {
            st.passed.insert(key(p));
        } else if registered && auth::session_ok(st.sessions.get(&lower), &ip, c::now_secs()) {
            st.passed.insert(key(p));
            say(p, th, "session");
        } else if !registered && !st.cfg.force_register {
            st.passed.insert(key(p));
            say(p, th, "optional");
        } else {
            hold(st, p);
        }
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, e: Data<PlayerLeaveEvent>) -> Data<PlayerLeaveEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let k = key(&e.player);
            if st.waiting.remove(&k).is_some() {
                // Do not let "invulnerable" be saved with the player
                release(&e.player);
            }
            st.passed.remove(&k);
            st.last_try.remove(&k);
        }
        e
    }
}

struct OnChat;
impl EventHandler<PlayerChatEvent> for OnChat {
    fn handle(&self, _server: Server, mut e: Data<PlayerChatEvent>) -> Data<PlayerChatEvent> {
        if let Some(st) = try_lock().as_ref().and_then(|g| g.as_ref()) {
            if st.waiting.contains_key(&key(&e.player)) {
                e.cancelled = true;
                prompt(st, &e.player, st.accounts.contains_key(&auth::key_of(&e.player.get_name())));
            }
        }
        e
    }
}

struct OnCommand;
impl EventHandler<PlayerCommandSendEvent> for OnCommand {
    fn handle(&self, server: Server, mut e: Data<PlayerCommandSendEvent>) -> Data<PlayerCommandSendEvent> {
        let after = {
            let Some(mut guard) = try_lock() else { return e };
            let Some(st) = guard.as_mut() else { return e };
            let (swallow, after) = on_command(st, &server, &e.player, &e.command);
            if swallow {
                e.cancelled = true;
            }
            after
        };
        run(&server, after);
        e
    }
}

macro_rules! block_while_waiting {
    ($handler:ident, $event:ty, $player:expr) => {
        struct $handler;
        impl EventHandler<$event> for $handler {
            fn handle(&self, _server: Server, mut e: Data<$event>) -> Data<$event> {
                let get: fn(&Data<$event>) -> Option<&Player> = $player;
                if get(&e).is_some_and(is_waiting) {
                    e.cancelled = true;
                }
                e
            }
        }
    };
}

block_while_waiting!(OnBreak, BlockBreakEvent, |e| e.player.as_ref());
block_while_waiting!(OnPlace, BlockPlaceEvent, |e| Some(&e.player));
block_while_waiting!(OnInteract, PlayerInteractEvent, |e| Some(&e.player));
block_while_waiting!(OnDrop, PlayerDropItemEvent, |e| Some(&e.player));
block_while_waiting!(OnClick, InventoryClickEvent, |e| Some(&e.player));
block_while_waiting!(OnEntity, PlayerInteractEntityEvent, |e| Some(&e.player));

/// `/auth …`: the plugin id `login` is the players' own command, so the admin root is `auth`
struct Admin;
impl CommandHandler for Admin {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let words = c::words(&args, "args");
        let refs: Vec<&str> = words.iter().map(String::as_str).collect();
        let (line, after) = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            match guard.as_mut() {
                Some(st) => admin(st, &server, &refs),
                None if refs.first() == Some(&"version") => (format!("&fLogin {VERSION}"), After::Nothing),
                None => (t(true, "inactive").to_owned(), After::Nothing),
            }
        };
        c::reply(&sender, &format!("{PREFIX}{line}"));
        run(&server, after);
        Ok(1)
    }
}

/// `/login` and friends exist as commands so clients list them; players never get here
/// because the command event handles and swallows them first.
struct Verb;
impl CommandHandler for Verb {
    fn handle(&self, sender: CommandSender, _server: Server, _args: ConsumedArgs) -> Result<i32, CommandError> {
        let (th, active) = match try_lock().as_ref().map(|g| g.as_ref().map(|st| st.cfg.thai())) {
            Some(Some(th)) => (th, true),
            _ => (true, false),
        };
        let line = if !active {
            "inactive"
        } else if c::sender_player(&sender).is_some() {
            "retry"
        } else {
            "ingame-only"
        };
        c::reply(&sender, &format!("{PREFIX}{}", t(th, line)));
        Ok(1)
    }
}

struct Login;

impl Plugin for Login {
    fn new() -> Self {
        Login
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Login for offline-mode servers by MCSV".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission {
            node: format!("{NS}:admin"),
            description: "Login".into(),
            default: PermissionDefault::Op(PermissionLevel::Three),
            children: vec![],
        });
        let _ = context.register_permission(&Permission {
            node: format!("{NS}:player"),
            description: "Login".into(),
            default: PermissionDefault::Allow,
            children: vec![],
        });
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": c::now_secs() }));

        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let greedy = || CommandNode::argument("args", &ArgumentType::String(StringType::Greedy));
        let cmd = Command::new(&["auth".to_owned(), "authme".to_owned()], "Login").execute(Admin).then(greedy().execute(Admin));
        context.register_command(cmd, &format!("{NS}:admin"));
        for verb in PLAYER_VERBS {
            let cmd = Command::new(&[(*verb).to_owned()], "Login").execute(Verb).then(greedy().execute(Verb));
            context.register_command(cmd, &format!("{NS}:player"));
        }

        // With online mode Mojang has verified every Java player: nothing for us to do
        if context.get_server().is_online_mode() {
            c::log_info("online mode is on: Login stays idle");
            return Ok(());
        }
        let cfg = load();
        let accounts: Accounts = c::load_json(ACCOUNTS).unwrap_or_default();
        c::log_info(&format!("{} accounts", accounts.len()));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State {
            cfg,
            accounts,
            dirty: false,
            waiting: HashMap::new(),
            passed: HashSet::new(),
            sessions: HashMap::new(),
            guard: Guard::default(),
            // One password check costs tens of milliseconds: four a second with a burst of eight
            budget: Budget::new(4.0, 8.0),
            last_try: HashMap::new(),
            secs: 0,
        });
        // Blocking handlers: only those may cancel (Pumpkin ignores the priority)
        context.register_event_handler(OnLogin, EventPriority::Highest, true)?;
        context.register_event_handler(OnJoin, EventPriority::Highest, true)?;
        context.register_event_handler(OnLeave, EventPriority::Normal, false)?;
        context.register_event_handler(OnChat, EventPriority::Highest, true)?;
        context.register_event_handler(OnCommand, EventPriority::Highest, true)?;
        context.register_event_handler(OnBreak, EventPriority::Highest, true)?;
        context.register_event_handler(OnPlace, EventPriority::Highest, true)?;
        context.register_event_handler(OnInteract, EventPriority::Highest, true)?;
        context.register_event_handler(OnDrop, EventPriority::Highest, true)?;
        context.register_event_handler(OnClick, EventPriority::Highest, true)?;
        context.register_event_handler(OnEntity, EventPriority::Highest, true)?;
        context.schedule_repeating_task(20, 20, on_tick);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            if st.dirty {
                save(st);
            }
        }
        c::remove_file("status.json");
        Ok(())
    }
}

register_plugin!(Login);

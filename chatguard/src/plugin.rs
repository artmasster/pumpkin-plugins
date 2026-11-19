//! Pumpkin WASM glue for ChatGuard: the chat event, signs, and the check other plugins ask for.
//!
//! The Basics plugin sends chat itself (with its format) and cancels the game's chat event, and
//! Pumpkin runs plugins' handlers in the order they were loaded, so Basics asks this plugin
//! about every line first (`{"kind":"check"}`). When Basics is not there, the game's chat event
//! is checked here. A line already checked a moment ago (the event, then Basics) is not counted
//! twice. Words are in `words.txt`.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, PlayerChatEvent, SignChangeEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::wit::pumpkin::plugin::server::CommandSender as ServerSender;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::defs::{self, Config, Memory, NS, Verdict, Why};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&cChatGuard&8]&r ";
const WORDS_FILE: &str = "words.txt";

struct State {
    cfg: Config,
    words: Vec<String>,
    memories: HashMap<String, Memory>,
    /// The last line each player was let through, and when: the same line again at once is the
    /// second look at one line, not a new one.
    passed: HashMap<String, (Instant, String)>,
    start: Instant,
    after: Vec<String>,
    stopped: u64,
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

fn load() -> (Config, Vec<String>) {
    let cfg = c::load_config::<Config>("config.toml").cleaned();
    let path = c::data_path(WORDS_FILE);
    if !path.exists() {
        let _ = std::fs::write(&path, defs::WORDS);
    }
    let words = std::fs::read_to_string(&path).map(|t| defs::read_words(&t)).unwrap_or_default();
    (cfg, words)
}

fn why_text(th: bool, why: Why) -> String {
    let (t, e) = match why {
        Why::BadWords => ("&cข้อความมีคำไม่สุภาพ", "&cThat line has a word that is not allowed"),
        Why::Advert => ("&cห้ามโฆษณาเซิร์ฟหรือเว็บอื่น", "&cNo adverts for other servers or sites"),
        Why::TooFast => ("&cพิมพ์เร็วเกินไป", "&cYou are typing too fast"),
        Why::Repeat => ("&cอย่าพิมพ์ข้อความเดิมซ้ำ", "&cDo not say the same thing again"),
    };
    msg(th, t, e)
}

/// Check a chat line. `Err(notice)` = stopped, with what to tell the player.
fn check(st: &mut State, key: &str, name: &str, line: &str, staff: bool) -> Result<String, String> {
    if let Some((at, text)) = st.passed.get(key)
        && at.elapsed().as_secs_f64() < 1.0
        && text == line
    {
        return Ok(line.to_owned());
    }
    let now = st.start.elapsed().as_secs_f64();
    let mem = st.memories.entry(key.to_owned()).or_default();
    match defs::check(&st.cfg, &st.words, mem, line, now, staff) {
        Verdict::Send(text) => {
            st.passed.insert(key.to_owned(), (Instant::now(), text.clone()));
            Ok(text)
        }
        Verdict::Stop(why) => {
            st.stopped += 1;
            if matches!(why, Why::BadWords | Why::Advert) {
                c::log_info(&format!("stopped a line of {name} ({why:?}): {}", line.chars().take(120).collect::<String>()));
                if defs::strike(&st.cfg, mem, now) && !st.cfg.punish.trim().is_empty() {
                    st.after.push(st.cfg.punish.replace("{player}", name));
                }
            }
            Err(why_text(st.cfg.thai(), why))
        }
    }
}

fn staff(p: &Player) -> bool {
    p.has_permission(&format!("{NS}:bypass"))
}

type Data<E> = <E as FromIntoEvent>::Data;

struct OnChat;
impl EventHandler<PlayerChatEvent> for OnChat {
    fn handle(&self, _server: Server, mut e: Data<PlayerChatEvent>) -> Data<PlayerChatEvent> {
        if e.cancelled || c::awaiting_login(&e.player) {
            return e;
        }
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        match check(st, &c::game::key(&e.player), &e.player.get_name(), &e.message, staff(&e.player)) {
            Ok(text) => e.message = text,
            Err(notice) => {
                c::tell(&e.player, &notice);
                e.cancelled = true;
            }
        }
        e
    }
}

/// Signs: bad words are starred (or the sign is refused); no spam checks.
struct OnSign;
impl EventHandler<SignChangeEvent> for OnSign {
    fn handle(&self, _server: Server, mut e: Data<SignChangeEvent>) -> Data<SignChangeEvent> {
        if e.cancelled || staff(&e.player) {
            return e;
        }
        let Some(guard) = try_lock() else { return e };
        let Some(st) = guard.as_ref() else { return e };
        let cfg = Config { cooldown: 0.0, repeat_seconds: 0, caps_min_length: u32::MAX, ..st.cfg.clone() };
        let mut lines = Vec::new();
        for line in &e.lines {
            match defs::check(&cfg, &st.words, &mut Memory::default(), line, 0.0, false) {
                Verdict::Send(text) => lines.push(text),
                Verdict::Stop(why) => {
                    c::tell(&e.player, &why_text(st.cfg.thai(), why));
                    e.cancelled = true;
                    return e;
                }
            }
        }
        e.lines = lines;
        e
    }
}

fn on_tick(server: Server) {
    let jobs = {
        let Some(mut guard) = try_lock() else { return };
        let Some(st) = guard.as_mut() else { return };
        if st.passed.len() > 500 {
            st.passed.retain(|_, (at, _)| at.elapsed().as_secs() < 5);
        }
        std::mem::take(&mut st.after)
    };
    for cmd in jobs {
        server.execute_command(&cmd, ServerSender::Console);
    }
}

struct Cmd;
impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, _server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let lines = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            let Some(st) = guard.as_mut() else { return Ok(0) };
            let th = st.cfg.thai();
            match w.first().map(String::as_str) {
                Some("reload") => {
                    (st.cfg, st.words) = load();
                    vec![msg(st.cfg.thai(), &format!("&aอ่านใหม่แล้ว: คำต้องห้าม {} คำ", st.words.len()), &format!("&aRead again: {} word(s)", st.words.len()))]
                }
                Some("test") => {
                    let line = w[1..].join(" ");
                    let cfg = Config { cooldown: 0.0, repeat_seconds: 0, ..st.cfg.clone() };
                    vec![match defs::check(&cfg, &st.words, &mut Memory::default(), &line, 0.0, false) {
                        Verdict::Send(t) => msg(th, &format!("&aผ่าน: &f{t}"), &format!("&aPasses: &f{t}")),
                        Verdict::Stop(why) => why_text(th, why),
                    }]
                }
                Some("version") => vec![format!("ChatGuard {VERSION}")],
                _ => vec![msg(th, &format!("&fรุ่น {VERSION} &7คำต้องห้าม {} คำ หยุดไปแล้ว {} ข้อความ &8| &f/chatguard reload &7| &ftest <ข้อความ>", st.words.len(), st.stopped), &format!("&fversion {VERSION} &7{} word(s), {} line(s) stopped &8| &f/chatguard reload &7| &ftest <line>", st.words.len(), st.stopped))],
            }
        };
        for line in lines {
            c::reply(&sender, &line);
        }
        Ok(1)
    }
}

struct ChatGuard;

impl Plugin for ChatGuard {
    fn new() -> Self {
        ChatGuard
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "A chat filter for bad words, adverts and spam".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Manage the chat filter".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let _ = context.register_permission(&Permission { node: format!("{NS}:bypass"), description: "Not slowed or stopped for adverts (bad words still count)".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let (cfg, words) = load();
        c::log_info(&format!("{} word(s)", words.len()));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg, words, memories: HashMap::new(), passed: HashMap::new(), start: Instant::now(), after: Vec::new(), stopped: 0 });
        let cmd = Command::new(&[NS.to_owned()], "ChatGuard").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:admin"));
        context.register_event_handler(OnChat, EventPriority::High, true)?;
        context.register_event_handler(OnSign, EventPriority::Normal, true)?;
        context.schedule_repeating_task(1, 1, on_tick);
        c::mark_running(VERSION);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::mark_stopped();
        Ok(())
    }

    /// Basics (or any plugin that sends chat itself): `{"kind":"check","player":<uuid hex>,
    /// "name","message","staff":bool}` answers `{"ok":true,"send":true,"message":<the line to
    /// send>}` or `{"ok":true,"send":false,"notice":<what to tell the player>}`.
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        if v.get("kind").and_then(serde_json::Value::as_str) != Some("check") {
            return Err("unknown kind".into());
        }
        let mut guard = try_lock().ok_or("busy")?;
        let st = guard.as_mut().ok_or("not loaded")?;
        let text = |k: &str| v.get(k).and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
        let key = text("player").replace('-', "").to_ascii_lowercase();
        let staff = v.get("staff").and_then(serde_json::Value::as_bool).unwrap_or(false);
        let answer = match check(st, &key, &text("name"), &text("message"), staff) {
            Ok(line) => serde_json::json!({ "ok": true, "send": true, "message": line }),
            Err(notice) => serde_json::json!({ "ok": true, "send": false, "notice": notice }),
        };
        serde_json::to_vec(&answer).map_err(|e| e.to_string())
    }
}

register_plugin!(ChatGuard);

//! Pumpkin WASM glue for Motd.
//!
//! Only the text of the server list entry is changed. Player counts are left alone on purpose:
//! MCSV reads them to know how many people are really online.
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, PlayerLoginEvent, ServerListPingEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::wit::pumpkin::plugin::server::CommandSender as ServerSender;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::sign::{self, Config, Maintenance};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NS: &str = "motd";
const PREFIX: &str = "&8[&bMotd&8]&r ";
const STATE_FILE: &str = "state.json";

struct State {
    cfg: Config,
    maintenance: Maintenance,
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

fn tr<'a>(th: bool, thai: &'a str, english: &'a str) -> &'a str {
    if th { thai } else { english }
}

fn left(th: bool, state: &Maintenance) -> String {
    if state.until == 0 {
        return tr(th, "จนกว่าจะสั่งปิด", "until switched off").to_owned();
    }
    format!("{} {}", tr(th, "อีก", "for another"), c::hms(state.until.saturating_sub(c::now_secs())))
}

/// Returns the lines for the sender and the players to kick once the lock is released.
fn command(st: &mut State, server: &Server, w: &[String]) -> (Vec<String>, Vec<String>) {
    let th = st.cfg.thai();
    let arg = w.get(1).map_or("", String::as_str);
    let mut kicks = Vec::new();
    let lines = match w.first().map_or("", String::as_str) {
        "on" => {
            let secs = if arg.is_empty() { Some(0) } else { sign::parse_duration(arg) };
            let Some(secs) = secs else {
                return (vec![format!("&e{}: &f/maintenance on [30m|2h|1d]", tr(th, "วิธีใช้", "Usage"))], kicks);
            };
            st.maintenance.on = true;
            st.maintenance.until = if secs == 0 { 0 } else { c::now_secs() + secs };
            c::save_json(STATE_FILE, &st.maintenance);
            if st.cfg.kick_on_enable {
                kicks = server
                    .get_all_players()
                    .iter()
                    .filter(|p| !st.maintenance.lets_in(&p.get_name(), p.get_permission_level() as u8, st.cfg.maintenance_op_level))
                    .map(|p| p.get_name())
                    .collect();
            }
            vec![format!("&e{} &7({})", tr(th, "เปิดโหมดปิดปรับปรุงแล้ว", "Maintenance is on"), left(th, &st.maintenance))]
        }
        "off" => {
            st.maintenance.on = false;
            st.maintenance.until = 0;
            c::save_json(STATE_FILE, &st.maintenance);
            vec![format!("&a{}", tr(th, "ปิดโหมดปิดปรับปรุงแล้ว ทุกคนเข้าได้ตามปกติ", "Maintenance is off, everyone can join"))]
        }
        "add" | "remove" if (1..=32).contains(&arg.len()) => {
            let changed = if w[0] == "add" { st.maintenance.allow(arg) } else { st.maintenance.disallow(arg) };
            if changed {
                c::save_json(STATE_FILE, &st.maintenance);
            }
            let line = match (w[0].as_str(), changed) {
                ("add", true) => tr(th, "เข้าได้ระหว่างปิดปรับปรุง", "may join during maintenance"),
                ("add", false) => tr(th, "อยู่ในรายชื่ออยู่แล้ว", "is already on the list"),
                (_, true) => tr(th, "ถูกถอดจากรายชื่อแล้ว", "was taken off the list"),
                _ => tr(th, "ไม่ได้อยู่ในรายชื่อ", "is not on the list"),
            };
            vec![format!("&f{arg} &7{line}")]
        }
        "list" => {
            let names = if st.maintenance.allowed.is_empty() { "-".to_owned() } else { st.maintenance.allowed.join(", ") };
            vec![format!("&7{}: &f{names}", tr(th, "เข้าได้ระหว่างปิดปรับปรุง (นอกจาก op)", "may join during maintenance (besides ops)"))]
        }
        "status" => {
            let mode = if st.maintenance.on {
                format!("&e{} &7({})", tr(th, "ปิดปรับปรุงอยู่", "maintenance on"), left(th, &st.maintenance))
            } else {
                format!("&a{}", tr(th, "เปิดปกติ", "open"))
            };
            vec![format!("{mode} &7· MOTD {} &f{}", tr(th, "ที่ตั้งไว้", "configured:"), st.cfg.motd.len())]
        }
        "reload" => {
            st.cfg = load();
            vec![format!("&a{}", tr(st.cfg.thai(), "โหลดไฟล์ตั้งค่าใหม่แล้ว", "Configuration reloaded"))]
        }
        "version" => vec![format!("&fMotd {VERSION}")],
        _ => vec![format!(
            "&eMotd: &f/maintenance on [{}] &7| &foff &7| &fadd <{p}> &7| &fremove <{p}> &7| &flist &7| &fstatus &7| &freload &7| &fversion",
            tr(th, "เวลา", "time"),
            p = tr(th, "ผู้เล่น", "player")
        )],
    };
    (lines, kicks)
}

struct Cmd;
impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let (lines, kicks, reason) = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            let Some(st) = guard.as_mut() else { return Ok(0) };
            let (lines, kicks) = command(st, &server, &w);
            (lines, kicks, st.cfg.maintenance_kick.clone())
        };
        for line in lines {
            c::reply(&sender, &format!("{PREFIX}{line}"));
        }
        // After the lock is gone: a kick fires events back into plugins
        for name in kicks {
            server.execute_command(&format!("kick {name} {}", c::plain(&reason)), ServerSender::Console);
        }
        Ok(1)
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

struct OnPing;
impl EventHandler<ServerListPingEvent> for OnPing {
    fn handle(&self, _server: Server, mut e: Data<ServerListPingEvent>) -> Data<ServerListPingEvent> {
        if let Some(st) = try_lock().as_ref().and_then(|g| g.as_ref()) {
            if let Some(text) = sign::current_motd(&st.cfg, &st.maintenance, c::now_secs(), e.num_players, e.max_players) {
                e.motd = c::color(&text);
            }
        }
        e
    }
}

struct OnLogin;
impl EventHandler<PlayerLoginEvent> for OnLogin {
    fn handle(&self, _server: Server, mut e: Data<PlayerLoginEvent>) -> Data<PlayerLoginEvent> {
        if let Some(st) = try_lock().as_ref().and_then(|g| g.as_ref()) {
            if !st.maintenance.lets_in(&e.player.get_name(), e.player.get_permission_level() as u8, st.cfg.maintenance_op_level) {
                e.kick_message = c::color(&st.cfg.maintenance_kick);
                e.cancelled = true;
            }
        }
        e
    }
}

/// Once a second: a timed maintenance ends by itself.
fn on_tick(_server: Server) {
    if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
        if st.maintenance.expire(c::now_secs()) {
            c::save_json(STATE_FILE, &st.maintenance);
            c::log_info("maintenance time is over: the server is open again");
        }
    }
}

struct Motd;

impl Plugin for Motd {
    fn new() -> Self {
        Motd
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Server list message and maintenance mode by MCSV".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission {
            node: format!("{NS}:command"),
            description: "Motd".into(),
            default: PermissionDefault::Op(PermissionLevel::Three),
            children: vec![],
        });
        let maintenance: Maintenance = c::load_json(STATE_FILE).unwrap_or_default();
        if maintenance.on {
            c::log_info("maintenance mode is ON: only ops and listed players can join (/maintenance off)");
        }
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg: load(), maintenance });
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": c::now_secs() }));

        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        // No `/motd` root: Basics owns `/motd` (shows the message of the day to a player)
        let cmd = Command::new(&["maintenance".to_owned()], "Motd")
            .execute(Cmd)
            .then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:command"));
        context.register_event_handler(OnPing, EventPriority::Normal, true)?;
        context.register_event_handler(OnLogin, EventPriority::Normal, true)?;
        context.schedule_repeating_task(20, 20, on_tick);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::remove_file("status.json");
        Ok(())
    }
}

register_plugin!(Motd);

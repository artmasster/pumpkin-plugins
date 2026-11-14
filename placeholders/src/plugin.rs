//! Pumpkin WASM glue for Placeholders: the values, the command, and the door for other plugins.
//!
//! Other plugins talk to this one over IPC (`mcsv_common::placeholders` does it for them):
//!
//! ```text
//! → {"kind":"parse","text":"Hi %player_name%","player":"Alice"}      player: name or UUID, or absent
//! ← {"text":"Hi Alice"}
//! → {"kind":"parse_many","texts":["…","…"],"player":"Alice"}
//! ← {"texts":["…","…"]}
//! ```
//!
//! and this one asks the plugin called `id` for `%id_params%` it does not know itself:
//!
//! ```text
//! → {"kind":"placeholder","params":"balance","player":"Alice","uuid":"…"}
//! ← {"value":"1,250"}        ("value": null, or an error, = not mine: the text stays as written)
//! ```
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::GameMode;
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::persistent_data::PersistentDataHolder;
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::cfg::Config;
use crate::parse::{self, clock, expand, span};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NS: &str = "placeholders";
/// A plugin that is not installed is not asked again for this long.
const ABSENT_SECS: u64 = 60;
/// What this plugin answers itself, for `/papi list`.
const BUILT_IN: &[(&str, &str)] = &[
    ("player", "name, displayname, uuid, ping, world, x, y, z, health, max_health, food, level, gamemode, locale, direction, flying"),
    ("server", "online, max_players, tps, mspt, time, time_seconds, date, uptime, motd"),
    ("world", "name, time, players, weather"),
    ("perms", "prefix, suffix, group, weight"),
    ("custom", "<name> from config.toml"),
];

struct State {
    cfg: Arc<Config>,
    started: Instant,
    /// Plugin ids nobody answered for, and until when (Unix seconds) they are not asked again.
    absent: HashMap<String, u64>,
    /// Since the start: texts parsed and placeholders filled.
    parsed: u64,
    filled: u64,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// The server, for texts that arrive over IPC (that call brings no server with it). Taken at
/// load and again on every run of the tick task.
static SERVER: Mutex<Option<Server>> = Mutex::new(None);

fn keep_server(server: Server) {
    if let Ok(mut slot) = SERVER.try_lock() {
        *slot = Some(server);
    }
}

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

fn uuid_text(p: &Player) -> String {
    let id = p.get_id();
    let h = format!("{:016x}{:016x}", id.high, id.low);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn short_dim(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// `%player_…%`
fn player_value(p: &Player, what: &str) -> Option<String> {
    Some(match what {
        "name" => p.get_name(),
        "displayname" => {
            let shown = p.get_display_name().get_text();
            if shown.is_empty() { p.get_name() } else { shown }
        }
        "uuid" => uuid_text(p),
        "ping" => p.get_ping().to_string(),
        "world" => short_dim(&p.get_world().get_dimension()).to_owned(),
        "x" | "y" | "z" => {
            let (x, y, z) = p.get_position();
            (match what {
                "x" => x,
                "y" => y,
                _ => z,
            })
            .floor()
            .to_string()
        }
        "health" => format!("{:.0}", p.get_health().ceil()),
        "max_health" => format!("{:.0}", p.get_max_health()),
        "food" => p.get_food_level().to_string(),
        "level" => p.get_experience_level().to_string(),
        "gamemode" => match p.get_gamemode() {
            GameMode::Survival => "survival",
            GameMode::Creative => "creative",
            GameMode::Adventure => "adventure",
            GameMode::Spectator => "spectator",
        }
        .to_owned(),
        "locale" => p.get_locale(),
        "direction" => {
            // Yaw 0 = south, 90 = west, 180 = north, 270 = east
            let yaw = p.get_yaw().rem_euclid(360.0);
            ["S", "SW", "W", "NW", "N", "NE", "E", "SE"][(((yaw + 22.5) / 45.0) as usize) % 8].to_owned()
        }
        "flying" => yes_no(p.is_flying()),
        _ => return None,
    })
}

fn yes_no(v: bool) -> String {
    if v { "yes" } else { "no" }.to_owned()
}

/// `%perms_…%`: what the Perms plugin publishes on the player (nothing when Perms is not installed).
fn perms_value(p: &Player, what: &str) -> Option<String> {
    match what {
        "prefix" | "suffix" | "group" | "weight" => Some(p.get_string("perms", what).or_else(|| p.get_long("perms", what).map(|n| n.to_string())).unwrap_or_default()),
        _ => None,
    }
}

/// Minecraft's day starts at 06:00 with tick 0.
fn game_clock(ticks: u64) -> String {
    let t = (ticks + 6000) % 24_000;
    format!("{:02}:{:02}", t / 1000, t % 1000 * 60 / 1000)
}

/// Ask the plugin called `id`. `Err(())` = there is no such plugin.
fn ask_plugin(id: &str, params: &str, player: Option<&Player>) -> Result<Option<String>, ()> {
    let message = serde_json::json!({
        "kind": "placeholder", "params": params,
        "player": player.map(Player::get_name), "uuid": player.map(uuid_text),
    });
    let bytes = serde_json::to_vec(&message).map_err(|_| ())?;
    match pumpkin_plugin_api::ipc::send_ipc_message(id, &bytes) {
        Ok(Ok(answer)) => Ok(serde_json::from_slice::<serde_json::Value>(&answer).ok().and_then(|v| match v.get("value") {
            Some(serde_json::Value::String(s)) => Some(s.chars().filter(|c| !c.is_control()).take(256).collect()),
            Some(serde_json::Value::Number(n)) => Some(n.to_string()),
            Some(serde_json::Value::Bool(b)) => Some(yes_no(*b)),
            _ => None,
        })),
        // The plugin is there but this is not one of its placeholders (or it is busy)
        Ok(Err(_)) => Ok(None),
        Err(_) => Err(()),
    }
}

/// Everything one text needs, taken out of the state so that no lock is held while other
/// plugins are asked (one of them may ask us back).
struct Job<'a> {
    cfg: Arc<Config>,
    server: &'a Server,
    player: Option<&'a Player>,
    now: u64,
    uptime: u64,
    absent: Vec<String>,
    newly_absent: Vec<String>,
    filled: u64,
}

impl Job<'_> {
    fn value(&mut self, id: &str, params: &str, depth: u8) -> Option<String> {
        let v = match id {
            "player" => match self.player {
                Some(p) => player_value(p, params),
                // Known placeholder, nobody to ask about
                None => player_known(params).then(|| self.cfg.no_player.clone()),
            },
            "perms" => match self.player {
                Some(p) => perms_value(p, params),
                None => matches!(params, "prefix" | "suffix" | "group" | "weight").then(String::new),
            },
            "server" => self.server_value(params),
            "world" => self.world_value(params),
            "custom" => {
                let text = self.cfg.custom.get(&params.to_ascii_lowercase())?.clone();
                // A custom text may use placeholders, but not other custom texts (no loops)
                Some(if depth == 0 { expand(&text, &mut |i, p| if i == "custom" { None } else { self.value(i, p, 1) }) } else { text })
            }
            NS => (params == "version").then(|| VERSION.to_owned()),
            other => {
                if self.absent.iter().chain(&self.newly_absent).any(|a| a == other) {
                    return None;
                }
                match ask_plugin(other, params, self.player) {
                    Ok(v) => v,
                    Err(()) => {
                        self.newly_absent.push(other.to_owned());
                        None
                    }
                }
            }
        };
        if v.is_some() {
            self.filled += 1;
        }
        v
    }

    fn server_value(&self, what: &str) -> Option<String> {
        let s = self.server;
        let (hm, hms, date) = clock(self.now, self.cfg.utc_offset_minutes);
        Some(match what {
            "online" => s.get_player_count().to_string(),
            "max_players" | "max" => s.get_max_players().to_string(),
            "tps" => format!("{:.1}", s.get_tps().min(20.0)),
            "mspt" => format!("{:.1}", s.get_mspt()),
            "time" => hm,
            "time_seconds" => hms,
            "date" => date,
            "uptime" => span(self.uptime),
            "motd" => s.get_motd(),
            _ => return None,
        })
    }

    fn world_value(&self, what: &str) -> Option<String> {
        if !matches!(what, "name" | "time" | "players" | "weather") {
            return None;
        }
        let world = match self.player {
            Some(p) => p.get_world(),
            None => self.server.get_all_worlds().into_iter().next()?,
        };
        Some(match what {
            "name" => short_dim(&world.get_dimension()).to_owned(),
            "time" => game_clock(world.get_time_of_day()),
            "players" => self.server.get_player_count_in_world(world).to_string(),
            _ => if world.is_raining() { "rain" } else { "clear" }.to_owned(),
        })
    }
}

fn player_known(what: &str) -> bool {
    BUILT_IN[0].1.split(", ").any(|k| k == what)
}

/// Fill the placeholders of `texts` for `player`. `None` when the plugin is busy with a parse
/// that is still going on (a plugin we asked is asking us): the caller keeps its text.
fn fill(server: &Server, player: Option<&Player>, texts: &[String]) -> Option<Vec<String>> {
    let now = c::now_secs();
    let (cfg, uptime, absent) = {
        let mut guard = try_lock()?;
        let st = guard.as_mut()?;
        st.absent.retain(|_, until| *until > now);
        (st.cfg.clone(), st.started.elapsed().as_secs(), st.absent.keys().cloned().collect())
    };
    let mut job = Job { cfg, server, player, now, uptime, absent, newly_absent: Vec::new(), filled: 0 };
    let out: Vec<String> = texts.iter().map(|t| expand(&parse::brackets(t), &mut |id, params| job.value(id, params, 0))).collect();
    if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
        st.parsed += texts.len() as u64;
        st.filled += job.filled;
        for id in job.newly_absent {
            st.absent.insert(id, now + ABSENT_SECS);
        }
    }
    Some(out)
}

fn find_player(server: &Server, who: &str) -> Option<Player> {
    if who.is_empty() {
        return None;
    }
    server.get_player_by_name(who).or_else(|| server.get_all_players().into_iter().find(|p| uuid_text(p).eq_ignore_ascii_case(who)))
}

fn msg(th: bool, thai: &str, english: &str) -> String {
    format!("&8[&bPlaceholders&8]&r {}", if th { thai } else { english })
}

struct Cmd;

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let th = try_lock().and_then(|g| g.as_ref().map(|s| s.cfg.language != "en")).unwrap_or(true);
        let sub = w.first().map(|s| s.to_ascii_lowercase());
        let lines: Vec<String> = match sub.as_deref() {
            Some("parse" | "bcparse") => {
                let broadcast = sub.as_deref() == Some("bcparse");
                let (Some(who), true) = (w.get(1), w.len() > 2) else {
                    c::reply(&sender, &msg(th, "&eวิธีใช้: &f/papi parse <ผู้เล่น|me|--null> <ข้อความ>", "&eUsage: &f/papi parse <player|me|--null> <text>"));
                    return Ok(0);
                };
                let player = match who.as_str() {
                    "--null" | "-" => None,
                    "me" => c::sender_player(&sender),
                    name => match find_player(&server, name) {
                        Some(p) => Some(p),
                        None => {
                            c::reply(&sender, &msg(th, &format!("&cไม่พบผู้เล่น &f{name} &7(ต้องออนไลน์อยู่ หรือใช้ --null)"), &format!("&cNo player &f{name} &7(they must be online, or use --null)")));
                            return Ok(0);
                        }
                    },
                };
                let text: String = w[2..].join(" ").chars().take(parse::MAX_TEXT).collect();
                match fill(&server, player.as_ref(), &[text]) {
                    Some(out) if broadcast => {
                        for p in server.get_all_players() {
                            c::tell(&p, &out[0]);
                        }
                        vec![msg(th, "&aส่งให้ทุกคนแล้ว", "&aSent to everyone")]
                    }
                    Some(out) => out,
                    None => vec![msg(th, "&eระบบไม่ว่าง ลองใหม่อีกครั้ง", "&eBusy, try again")],
                }
            }
            Some("list" | "info") => {
                let mut out = vec![msg(th, "&fplaceholder ที่มีในตัว &7(เขียนเป็น %ชื่อ_ค่า% เช่น %player_name%)", "&fBuilt-in placeholders &7(written %id_value%, like %player_name%)")];
                out.extend(BUILT_IN.iter().map(|(id, keys)| format!("&b {id}&7: {keys}")));
                let (custom, absent) = try_lock().and_then(|g| g.as_ref().map(|s| (s.cfg.custom.keys().cloned().collect::<Vec<_>>(), s.absent.len()))).unwrap_or_default();
                if !custom.is_empty() {
                    out.push(format!("&b custom&7: {}", custom.join(", ")));
                }
                out.push(if th {
                    "&7 ของปลั๊กอินอื่น: %ชื่อปลั๊กอิน_ค่า% เช่น %basics_balance% (ปลั๊กอินนั้นต้องติดตั้งอยู่และรองรับ)".into()
                } else {
                    "&7 other plugins: %plugin_value%, like %basics_balance% (that plugin must be installed and support it)".into()
                });
                if absent > 0 {
                    out.push(format!("&8 {absent} plugin id(s) did not answer in the last minute"));
                }
                out
            }
            Some("reload") => {
                let Some(mut guard) = try_lock() else { return Ok(0) };
                let Some(st) = guard.as_mut() else { return Ok(0) };
                st.cfg = Arc::new(load());
                st.absent.clear();
                let th = st.cfg.language != "en";
                vec![msg(th, &format!("&aโหลดไฟล์ตั้งค่าใหม่แล้ว (custom {} รายการ)", st.cfg.custom.len()), &format!("&aConfiguration reloaded ({} custom texts)", st.cfg.custom.len()))]
            }
            Some("version") => {
                let (parsed, filled) = try_lock().and_then(|g| g.as_ref().map(|s| (s.parsed, s.filled))).unwrap_or_default();
                vec![msg(th, &format!("&fPlaceholders {VERSION} &7· แปลงแล้ว {parsed} ข้อความ {filled} ค่า"), &format!("&fPlaceholders {VERSION} &7· {parsed} texts parsed, {filled} values filled"))]
            }
            _ => {
                let lines: &[(&str, &str)] = &[
                    ("&bPlaceholders &7ตัวแปรในข้อความ เช่น %player_name% ที่ปลั๊กอินอื่นและคำสั่งนี้แปลงเป็นค่าจริง", "&bPlaceholders &7values inside texts, like %player_name%, filled in for other plugins and by this command"),
                    ("&f/papi parse <ผู้เล่น|me|--null> <ข้อความ> &7ลองแปลงข้อความ · &f/papi bcparse … &7แปลงแล้วส่งให้ทุกคน", "&f/papi parse <player|me|--null> <text> &7try a text · &f/papi bcparse … &7parse and send to everyone"),
                    ("&f/papi list &7ดู placeholder ที่มี · &f/papi reload &7| &fversion &7· ข้อความของตัวเองตั้งที่ [custom] ใน plugins/data/placeholders/config.toml", "&f/papi list &7what there is · &f/papi reload &7| &fversion &7· your own texts go under [custom] in plugins/data/placeholders/config.toml"),
                ];
                lines.iter().map(|(t, e)| (if th { *t } else { *e }).to_owned()).collect()
            }
        };
        for line in lines {
            c::reply(&sender, &line);
        }
        Ok(1)
    }
}

struct Placeholders;

impl Plugin for Placeholders {
    fn new() -> Self {
        Placeholders
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Placeholders by MCSV".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg: Arc::new(load()), started: Instant::now(), absent: HashMap::new(), parsed: 0, filled: 0 });
        // The panel reads this to tell "loaded" from "installed, waiting for a restart"
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": c::now_secs() }));
        let _ = context.register_permission(&Permission {
            node: format!("{NS}:command"),
            description: "Placeholders".into(),
            default: PermissionDefault::Op(PermissionLevel::Two),
            children: vec![],
        });
        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let cmd = Command::new(&["papi".to_owned(), NS.to_owned(), "placeholderapi".to_owned()], "Placeholders")
            .execute(Cmd)
            .then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:command"));
        keep_server(context.get_server());
        pumpkin_plugin_api::scheduler::schedule_repeating_task(20, 100, keep_server);
        Ok(())
    }

    /// Another plugin hands over a text (see the module comment).
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let guard = SERVER.try_lock().map_err(|_| "busy")?;
        let server = guard.as_ref().ok_or("not ready")?;
        let player = v.get("player").and_then(serde_json::Value::as_str).and_then(|who| find_player(server, who));
        let texts: Vec<String> = match v.get("kind").and_then(serde_json::Value::as_str) {
            Some("parse") => vec![v.get("text").and_then(serde_json::Value::as_str).unwrap_or("").to_owned()],
            Some("parse_many") => v.get("texts").and_then(serde_json::Value::as_array).map(|a| a.iter().take(64).map(|t| t.as_str().unwrap_or("").to_owned()).collect()).unwrap_or_default(),
            other => return Err(format!("unknown kind {}", other.unwrap_or(""))),
        };
        let out = fill(server, player.as_ref(), &texts).ok_or("busy")?;
        let answer = if v["kind"] == "parse" { serde_json::json!({ "text": out.first() }) } else { serde_json::json!({ "texts": out }) };
        Ok(answer.to_string().into_bytes())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::remove_file("status.json");
        Ok(())
    }
}

register_plugin!(Placeholders);

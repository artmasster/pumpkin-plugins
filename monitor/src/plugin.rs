//! Pumpkin WASM glue for Monitor.
//!
//! The server tells us what every tick cost (`ServerTickEndEvent.duration_nanos`); everything
//! here is built on that one number plus player and entity counts. There is no CPU profiler:
//! a WASM plugin cannot look at the server's threads.
use std::collections::HashMap;
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, ServerTickEndEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::uuid::Uuid;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::stats::{self, Config, Recording, Sample, Ticks};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NS: &str = "monitor";
const PREFIX: &str = "&8[&bMonitor&8]&r ";
/// Entities looked at by one `/spark entities` (each costs two calls into the server).
const ENTITY_CAP: usize = 30_000;
const KEEP_REPORTS: usize = 20;

/// Who asked for live output.
#[derive(Clone, Copy, PartialEq)]
enum Who {
    Console,
    Player(u64, u64),
}

struct Monitor {
    who: Who,
    threshold: f32,
    until_ms: u64,
    /// Second of the last line and how many lines went out in it.
    burst: (u64, u8),
}

struct State {
    cfg: Config,
    ticks: Ticks,
    started_ms: u64,
    monitors: HashMap<String, Monitor>,
    recording: Option<(Recording, Who, u64)>,
    last_spike_log_ms: u64,
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

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn load() -> Config {
    c::load_config::<Config>("config.toml").cleaned()
}

fn tr<'a>(th: bool, thai: &'a str, english: &'a str) -> &'a str {
    if th { thai } else { english }
}

fn who_of(sender: &CommandSender) -> (String, Who) {
    match c::sender_player(sender) {
        Some(p) => {
            let id = p.get_id();
            (format!("{:016x}{:016x}", id.high, id.low), Who::Player(id.high, id.low))
        }
        None => ("console".into(), Who::Console),
    }
}

fn send(server: &Server, who: Who, line: &str) {
    match who {
        Who::Console => c::log_info(&c::plain(line)),
        Who::Player(high, low) => {
            if let Some(p) = server.get_player_by_uuid(Uuid { high, low }) {
                c::tell(&p, &format!("{PREFIX}{line}"));
            }
        }
    }
}

fn tps_cell(v: Option<f32>) -> String {
    match v {
        Some(v) => format!("{}{:.1}", stats::tps_colour(v), v.min(20.0)),
        None => "&7-".into(),
    }
}

fn mspt_cell(m: Option<stats::Mspt>) -> String {
    match m {
        Some(m) => format!(
            "{}{:.1}&7/{}{:.1}&7/{}{:.1}&7/{}{:.1}",
            stats::ms_colour(m.min), m.min, stats::ms_colour(m.median), m.median, stats::ms_colour(m.p95), m.p95, stats::ms_colour(m.max), m.max
        ),
        None => "&7-".into(),
    }
}

fn tps_lines(st: &State) -> Vec<String> {
    let th = st.cfg.thai();
    let t = &st.ticks;
    vec![
        format!(
            "&7TPS {}: {}&7, {}&7, {}&7, {}&7, {}",
            tr(th, "ช่วง 5 วิ, 10 วิ, 1 นาที, 5 นาที, 15 นาที", "last 5s, 10s, 1m, 5m, 15m"),
            tps_cell(t.tps(5)), tps_cell(t.tps(10)), tps_cell(t.tps(60)), tps_cell(t.tps(300)), tps_cell(t.tps(900))
        ),
        format!(
            "&7{} {}: {} &7· {}: {}",
            tr(th, "เวลาต่อ tick เป็น ms (ต่ำสุด/กลาง/95%/สูงสุด)", "tick time in ms (min/med/95%ile/max)"),
            tr(th, "10 วิ", "10s"), mspt_cell(t.mspt(200)),
            tr(th, "1 นาที", "1m"), mspt_cell(t.mspt(1200))
        ),
    ]
}

/// Count entities by type and by chunk in the worlds whose dimension contains `filter`.
fn entity_counts(server: &Server, filter: &str, chunks: bool) -> (Vec<(String, u32)>, Vec<(String, u32)>, usize, bool) {
    let mut kinds: HashMap<String, u32> = HashMap::new();
    let mut spots: HashMap<(String, i32, i32), u32> = HashMap::new();
    let mut seen = 0usize;
    let mut cut = false;
    for w in server.get_all_worlds() {
        let dim = w.get_dimension().trim_start_matches("minecraft:").to_owned();
        if !filter.is_empty() && !dim.contains(filter) {
            continue;
        }
        for e in w.get_entities() {
            if seen >= ENTITY_CAP {
                cut = true;
                break;
            }
            seen += 1;
            *kinds.entry(stats::snake(&format!("{:?}", e.get_type()))).or_default() += 1;
            if chunks {
                let p = e.get_position();
                *spots.entry((dim.clone(), (p.0.floor() as i32) >> 4, (p.2.floor() as i32) >> 4)).or_default() += 1;
            }
        }
    }
    let mut kinds: Vec<(String, u32)> = kinds.into_iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut spots: Vec<(String, u32)> = spots.into_iter().map(|((dim, cx, cz), n)| (format!("{dim} {},{}", cx * 16 + 8, cz * 16 + 8), n)).collect();
    spots.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    (kinds, spots, seen, cut)
}

fn entity_total(server: &Server) -> u32 {
    server.get_all_worlds().iter().map(|w| w.get_entities().len() as u32).sum()
}

/// Write the report of a finished recording and tell whoever started it.
fn finish_recording(st: &mut State, server: &Server) {
    let Some((rec, who, _)) = st.recording.take() else { return };
    let th = st.cfg.thai();
    let (kinds, _, _, _) = entity_counts(server, "", false);
    let pumpkin = server.get_sys_info().pumpkin_version;
    let text = rec.report(now_ms(), VERSION, &pumpkin, &kinds);
    let dir = c::data_path("reports");
    let name = format!("{}.txt", c::now_secs());
    let ok = std::fs::create_dir_all(&dir).is_ok() && std::fs::write(dir.join(&name), text).is_ok();
    // Old reports go so the folder cannot grow without end
    if let Ok(list) = std::fs::read_dir(&dir) {
        let mut names: Vec<_> = list.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "txt")).collect();
        names.sort();
        for old in names.iter().rev().skip(KEEP_REPORTS) {
            let _ = std::fs::remove_file(old);
        }
    }
    let line = if ok {
        format!("&a{} &fplugins/data/monitor/reports/{name}", tr(th, "บันทึกเสร็จแล้ว เปิดไฟล์", "Recording saved to"))
    } else {
        format!("&c{}", tr(th, "เขียนไฟล์รายงานไม่สำเร็จ", "Could not write the report file"))
    };
    send(server, who, &line);
    if who != Who::Console {
        c::log_info(&c::plain(&line));
    }
}

fn ago(th: bool, ms: u64) -> String {
    let secs = ms / 1000;
    let (n, unit) = if secs >= 3600 {
        (secs / 3600, tr(th, "ชม.", "h"))
    } else if secs >= 60 {
        (secs / 60, tr(th, "นาที", "min"))
    } else {
        (secs, tr(th, "วินาที", "s"))
    };
    format!("{n} {unit}")
}

fn command(st: &mut State, server: &Server, sender: &CommandSender, w: &[String], root: &str) -> Vec<String> {
    let th = st.cfg.thai();
    let sub = if root == "tps" { "tps" } else { w.first().map_or("", String::as_str) };
    let arg = w.get(if root == "tps" { 0 } else { 1 }).map_or("", String::as_str);
    match sub {
        "tps" => tps_lines(st),
        "health" | "healthreport" => {
            let mut out = tps_lines(st);
            let players = server.get_all_players();
            let mut pings: Vec<u32> = players.iter().map(|p| p.get_ping()).collect();
            out.push(format!(
                "&7{} &f{}&7/&f{} &7· ping {} &f{} ms &7· {} &f{}",
                tr(th, "ผู้เล่น", "players"), players.len(), server.get_max_players(),
                tr(th, "กลาง", "median"), stats::median(&mut pings),
                tr(th, "ทำงานมาแล้ว", "uptime"), c::hms(now_ms().saturating_sub(st.started_ms) / 1000)
            ));
            for world in server.get_all_worlds() {
                let dim = world.get_dimension();
                let here = players.iter().filter(|p| p.get_world().get_dimension() == dim).count();
                out.push(format!(
                    "&7{}: {} &f{here} &7· entity &f{}",
                    dim.trim_start_matches("minecraft:"), tr(th, "ผู้เล่น", "players"), world.get_entities().len()
                ));
            }
            let slow = st.ticks.spikes().len();
            out.push(format!(
                "&7Pumpkin &f{} &7· {} &f{} &7({} {} ms)",
                server.get_sys_info().pumpkin_version, tr(th, "tick ช้าที่จำไว้", "slow ticks remembered"), slow,
                tr(th, "เกิน", "over"), st.cfg.spike_ms
            ));
            out.push(format!("&7{}", tr(th, "RAM และ CPU ของเซิร์ฟดูได้ที่หน้าแผงควบคุม", "RAM and CPU of the server are on the panel")));
            out
        }
        "ping" => {
            if !arg.is_empty() {
                return match server.get_player_by_name(arg) {
                    Some(p) => vec![format!("&f{} &7ping &f{} ms", p.get_name(), p.get_ping())],
                    None => vec![format!("&c{} {arg}", tr(th, "ไม่พบผู้เล่น", "No player named"))],
                };
            }
            let mut pings: Vec<u32> = server.get_all_players().iter().map(|p| p.get_ping()).collect();
            if pings.is_empty() {
                return vec![format!("&7{}", tr(th, "ไม่มีผู้เล่นออนไลน์", "No players online"))];
            }
            let mean = pings.iter().sum::<u32>() / pings.len() as u32;
            let (min, max) = (pings.iter().min().copied().unwrap_or(0), pings.iter().max().copied().unwrap_or(0));
            vec![format!(
                "&7ping {} &f{} &7{}: {} &f{mean} ms &7({} &f{min}&7, {} &f{}&7, {} &f{max}&7)",
                tr(th, "ของ", "of"), pings.len(), tr(th, "คน", "players"), tr(th, "เฉลี่ย", "average"),
                tr(th, "ต่ำสุด", "min"), tr(th, "กลาง", "median"), stats::median(&mut pings), tr(th, "สูงสุด", "max")
            )]
        }
        "tickmonitor" | "monitor" => {
            let (k, who) = who_of(sender);
            if arg == "off" || (arg.is_empty() && st.monitors.contains_key(&k)) {
                st.monitors.remove(&k);
                return vec![format!("&7{}", tr(th, "หยุดเฝ้าดู tick แล้ว", "Tick monitor off"))];
            }
            let threshold = arg.parse::<f32>().ok().filter(|v| v.is_finite()).unwrap_or(50.0).clamp(1.0, 60_000.0);
            st.monitors.insert(k, Monitor { who, threshold, until_ms: now_ms() + 600_000, burst: (0, 0) });
            vec![format!(
                "&a{} &f{threshold} ms &7({})",
                tr(th, "เฝ้าดู tick ที่นานเกิน", "Watching for ticks longer than"),
                tr(th, "หยุดเองใน 10 นาที หรือสั่ง /spark tickmonitor off", "stops by itself in 10 minutes, or /spark tickmonitor off")
            )]
        }
        "entities" | "entity" => {
            let (kinds, spots, seen, cut) = entity_counts(server, arg, true);
            let mut out = vec![format!(
                "&7entity {} &f{seen}{}",
                tr(th, "ทั้งหมด", "in total"),
                if cut { tr(th, " &7(นับถึงเพดานแล้วหยุด)", " &7(stopped at the limit)") } else { "" }
            )];
            let list = |items: &[(String, u32)], n: usize| items.iter().take(n).map(|(k, v)| format!("&f{k} &7{v}")).collect::<Vec<_>>().join("&8, ");
            if !kinds.is_empty() {
                out.push(format!("&7{}: {}", tr(th, "ชนิดที่มากสุด", "most common"), list(&kinds, 10)));
                out.push(format!("&7{}: {}", tr(th, "จุดที่แน่นสุด (โลก x,z)", "densest spots (world x,z)"), list(&spots, 5)));
            }
            out
        }
        "spikes" => {
            let spikes = st.ticks.spikes();
            if spikes.is_empty() {
                return vec![format!("&a{} {} ms", tr(th, "ยังไม่มี tick ที่นานเกิน", "No tick longer than"), st.cfg.spike_ms)];
            }
            let now = now_ms();
            let mut out = vec![format!("&7{} ({} {} ms):", tr(th, "tick ช้าล่าสุด", "latest slow ticks"), tr(th, "เกิน", "over"), st.cfg.spike_ms)];
            out.extend(spikes.iter().take(10).map(|s| {
                format!("&7{} {}: {}{:.0} ms &8(tick {})", ago(th, now.saturating_sub(s.at_ms)), tr(th, "ที่แล้ว", "ago"), stats::ms_colour(s.ms), s.ms, s.tick)
            }));
            out
        }
        "profiler" | "record" => {
            let (_, who) = who_of(sender);
            match arg {
                "start" => {
                    if st.recording.is_some() {
                        return vec![format!("&c{}", tr(th, "กำลังบันทึกอยู่แล้ว สั่ง /spark profiler stop ก่อน", "Already recording, /spark profiler stop first"))];
                    }
                    let secs = w.get(2).and_then(|s| s.parse::<u64>().ok()).unwrap_or(60).clamp(5, st.cfg.max_record_secs);
                    let now = now_ms();
                    st.recording = Some((Recording::new(now, secs), who, now + 5_000));
                    vec![
                        format!("&a{} &f{secs} &a{}", tr(th, "เริ่มบันทึก", "Recording for"), tr(th, "วินาที", "seconds")),
                        format!("&7{}", tr(th, "ได้ไฟล์เวลาของ tick, จำนวนผู้เล่นและ entity ไม่ใช่กราฟ CPU แบบ spark", "The result is tick timing with player and entity counts, not a CPU graph like spark")),
                    ]
                }
                "stop" => {
                    if st.recording.is_none() {
                        return vec![format!("&7{}", tr(th, "ไม่ได้บันทึกอยู่", "Not recording"))];
                    }
                    finish_recording(st, server);
                    Vec::new()
                }
                _ => match &st.recording {
                    Some((rec, _, _)) => vec![format!(
                        "&7{} &f{} &7{} · tick &f{}",
                        tr(th, "บันทึกมาแล้ว", "recording for"), now_ms().saturating_sub(rec.started_ms) / 1000, tr(th, "วินาที", "seconds"), rec.ticks
                    )],
                    None => vec![format!("&e{}: &f/spark profiler start [{}] &7| &fstop", tr(th, "วิธีใช้", "Usage"), tr(th, "วินาที", "seconds"))],
                },
            }
        }
        "reload" => {
            st.cfg = load();
            vec![format!("&a{}", tr(st.cfg.thai(), "โหลดไฟล์ตั้งค่าใหม่แล้ว", "Configuration reloaded"))]
        }
        "version" => vec![format!("&fMonitor {VERSION}")],
        _ => vec![format!(
            "&eMonitor: &f/spark tps &7| &fhealth &7| &fping [{}] &7| &ftickmonitor [ms|off] &7| &fentities [{}] &7| &fspikes &7| &fprofiler start [{}]|stop &7| &freload &7| &fversion",
            tr(th, "ผู้เล่น", "player"), tr(th, "โลก", "world"), tr(th, "วินาที", "seconds")
        )],
    }
}

struct Cmd(&'static str);

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let lines = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            let Some(st) = guard.as_mut() else { return Ok(0) };
            command(st, &server, &sender, &w, self.0)
        };
        for line in lines {
            c::reply(&sender, &format!("{PREFIX}{line}"));
        }
        Ok(1)
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

struct OnTickEnd;
impl EventHandler<ServerTickEndEvent> for OnTickEnd {
    fn handle(&self, server: Server, e: Data<ServerTickEndEvent>) -> Data<ServerTickEndEvent> {
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let now = now_ms();
        let ms = (e.duration_nanos.max(0) as f64 / 1_000_000.0) as f32;
        let th = st.cfg.thai();
        if st.ticks.push(now, e.tick, ms, st.cfg.spike_ms) && st.cfg.log_spikes && now.saturating_sub(st.last_spike_log_ms) >= 10_000 {
            st.last_spike_log_ms = now;
            c::log_info(&format!("slow tick: {ms:.0} ms (tick {}, {} players)", e.tick, server.get_player_count()));
        }
        if !st.monitors.is_empty() {
            st.monitors.retain(|_, m| m.until_ms > now);
            for m in st.monitors.values_mut().filter(|m| ms >= m.threshold) {
                // At most four lines a second: a lagging server must not also be flooded with chat
                let sec = now / 1000;
                if m.burst.0 != sec {
                    m.burst = (sec, 0);
                }
                if m.burst.1 < 4 {
                    m.burst.1 += 1;
                    send(&server, m.who, &format!("&7tick {} {} {}{ms:.1} ms", e.tick, tr(th, "ใช้เวลา", "took"), stats::ms_colour(ms)));
                }
            }
        }
        let mut done = false;
        if let Some((rec, _, next_sample)) = st.recording.as_mut() {
            rec.tick(now, e.tick, ms);
            if now >= *next_sample {
                *next_sample = now + 5_000;
                let last = st.ticks.last_seconds(5);
                let ticks: u32 = last.iter().map(|s| u32::from(s.ticks)).sum();
                let sum: f32 = last.iter().map(|s| s.sum_ms).sum();
                rec.samples.push(Sample {
                    at_secs: now.saturating_sub(rec.started_ms) / 1000,
                    tps: ticks as f32 / last.len().max(1) as f32,
                    mean_ms: sum / ticks.max(1) as f32,
                    max_ms: last.iter().map(|s| s.max_ms).fold(0.0, f32::max),
                    players: server.get_player_count(),
                    entities: entity_total(&server),
                });
            }
            done = now >= rec.until_ms;
        }
        if done {
            finish_recording(st, &server);
        }
        e
    }
}

struct MonitorPlugin;

impl Plugin for MonitorPlugin {
    fn new() -> Self {
        MonitorPlugin
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Performance monitoring by MCSV".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission {
            node: format!("{NS}:command"),
            description: "Monitor".into(),
            default: PermissionDefault::Op(PermissionLevel::Two),
            children: vec![],
        });
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State {
            cfg: load(),
            ticks: Ticks::default(),
            started_ms: now_ms(),
            monitors: HashMap::new(),
            recording: None,
            last_spike_log_ms: 0,
        });
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": c::now_secs() }));

        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let greedy = || CommandNode::argument("args", &ArgumentType::String(StringType::Greedy));
        let main = Command::new(&["spark".to_owned(), NS.to_owned()], "Monitor").execute(Cmd("spark")).then(greedy().execute(Cmd("spark")));
        context.register_command(main, &format!("{NS}:command"));
        let tps = Command::new(&["tps".to_owned()], "Monitor").execute(Cmd("tps")).then(greedy().execute(Cmd("tps")));
        context.register_command(tps, &format!("{NS}:command"));
        context.register_event_handler(OnTickEnd, EventPriority::Normal, false)?;
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::remove_file("status.json");
        Ok(())
    }
}

register_plugin!(MonitorPlugin);

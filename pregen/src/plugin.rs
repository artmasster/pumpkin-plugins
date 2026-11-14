//! Native Pumpkin glue: commands, permissions, driver thread, config and saved tasks.
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use pumpkin::command::argument_builder::{ArgumentBuilder, argument, command};
use pumpkin::command::argument_types::core::string::StringArgumentType;
use pumpkin::command::context::command_context::CommandContext;
use pumpkin::command::suggestion::provider::SuggestionProvider;
use pumpkin::command::suggestion::suggestions::{Suggestions, SuggestionsBuilder};
use pumpkin::command::{CommandExecutor, CommandExecutorResult};
use pumpkin::plugin::Context;
use pumpkin::server::Server;
use pumpkin::world::World;
use pumpkin_util::PermissionLvl;
use pumpkin_util::math::vector2::Vector2;
use pumpkin_util::permission::{Permission, PermissionDefault};
use pumpkin_util::text::TextComponent;
use serde::{Deserialize, Serialize};

use crate::args::{parse_coord, parse_len};
use crate::lang::{fmt, text};
use crate::pattern::{self, Pattern};
use crate::shape::Kind;
use crate::task::{Host, Limits, Saved, Selection, Task};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SUBS: [&str; 21] = [
    "help", "start", "pause", "continue", "cancel", "confirm", "world", "worldborder", "spawn", "center", "radius",
    "shape", "pattern", "corners", "selection", "progress", "quiet", "silent", "reload", "version", "trim",
];

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct Config {
    /// th | en
    language: String,
    /// Resume saved tasks automatically after a server restart.
    continue_on_restart: bool,
    /// Hide progress messages.
    silent: bool,
    /// Seconds between progress messages.
    update_interval: u64,
    /// Chunks loading at the same time (higher = faster, more RAM/CPU).
    max_in_flight: usize,
    /// The same limit while players are online, so their own chunks keep loading fast.
    max_in_flight_players: usize,
    /// New runs of chunks requested per tick.
    max_strips_per_tick: usize,
    /// Chunks per run.
    max_strip_length: usize,
    /// Average tick time (ms) above which Pregen slows down; half speed here, a quarter 25% higher. 0 = off.
    lag_mspt: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: "th".into(),
            continue_on_restart: false,
            silent: false,
            update_interval: 5,
            max_in_flight: 64,
            max_in_flight_players: 24,
            max_strips_per_tick: 8,
            max_strip_length: 16,
            lag_mspt: 40,
        }
    }
}

impl Config {
    fn limits(&self, players_online: bool, mspt: f64) -> Limits {
        let mut in_flight = if players_online { self.max_in_flight.min(self.max_in_flight_players) } else { self.max_in_flight };
        // A slow tick means the server is already behind: give it room instead of piling on
        let lag = f64::from(self.lag_mspt);
        if lag > 0.0 && mspt >= lag * 1.25 {
            in_flight /= 4;
        } else if lag > 0.0 && mspt >= lag {
            in_flight /= 2;
        }
        Limits {
            max_in_flight: in_flight.clamp(1, 4096),
            max_strips_per_tick: self.max_strips_per_tick.clamp(1, 256),
            max_strip: self.max_strip_length.clamp(1, 256),
            scan_budget: 8192,
            retry_secs: 120,
        }
    }
}

enum Pending {
    Start(Selection, Option<String>),
    Cancel(Option<String>),
}

struct State {
    dir: PathBuf,
    cfg: Config,
    tasks: BTreeMap<String, Task>,
    selections: HashMap<String, Selection>,
    pending: HashMap<String, Pending>,
    last_report: Instant,
    last_save: Instant,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static SERVER: OnceLock<Arc<Server>> = OnceLock::new();
static STOP: AtomicBool = AtomicBool::new(false);
static DRIVER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

fn lock() -> std::sync::MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ---- files ----------------------------------------------------------------

impl State {
    fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    fn load_config(&mut self) {
        let path = self.path("config.toml");
        self.cfg = match std::fs::read_to_string(&path) {
            Ok(t) => toml::from_str(&t).unwrap_or_else(|e| {
                tracing::warn!("Pregen config.toml: {e}; using defaults");
                Config::default()
            }),
            Err(_) => Config::default(),
        };
        self.save_config();
    }

    fn save_config(&self) {
        if let Ok(t) = toml::to_string_pretty(&self.cfg) {
            write_atomic(&self.path("config.toml"), t.as_bytes());
        }
    }

    fn save_task(&self, t: &Task) {
        if let Ok(b) = serde_json::to_vec_pretty(&t.saved()) {
            write_atomic(&self.path(&task_file(&t.sel.world)), &b);
        }
    }

    fn remove_task_file(&self, world: &str) {
        let _ = std::fs::remove_file(self.path(&task_file(world)));
    }

    fn saved_tasks(&self) -> Vec<Saved> {
        let Ok(dir) = std::fs::read_dir(self.path("tasks")) else { return Vec::new() };
        dir.filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|t| serde_json::from_str(&t).ok())
            .collect()
    }

    fn load_csv(&self, sel: &Selection) -> Vec<(i32, i32)> {
        match (&sel.pattern, &sel.csv) {
            // A plain file name only: the command must not reach files outside the plugin folder
            (Pattern::Csv, Some(name)) if !name.is_empty() && !name.starts_with('.') && !name.contains(['/', '\\']) => std::fs::read_to_string(self.path(name))
                .map(|t| pattern::parse_csv(&t))
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn task_file(world: &str) -> String {
    let safe: String = world.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    format!("tasks/{safe}.json")
}

// ---- worlds ---------------------------------------------------------------

/// Bukkit-style world name players know: world, world_nether, world_the_end.
fn world_name(w: &World) -> String {
    match w.dimension.minecraft_name.trim_start_matches("minecraft:") {
        "overworld" => "world".into(),
        "the_nether" => "world_nether".into(),
        "the_end" => "world_the_end".into(),
        other => other.to_owned(),
    }
}

fn normalize(name: &str) -> String {
    match name.to_lowercase().trim_start_matches("minecraft:") {
        "world" | "overworld" => "world".into(),
        "world_nether" | "nether" | "the_nether" => "world_nether".into(),
        "world_the_end" | "end" | "the_end" => "world_the_end".into(),
        other => other.to_owned(),
    }
}

fn worlds(server: &Server) -> Vec<Arc<World>> {
    server.worlds.load().iter().cloned().collect()
}

fn world_names(server: &Server) -> Vec<String> {
    worlds(server).iter().map(|w| world_name(w)).collect()
}

fn find_world(server: &Server, name: &str) -> Option<Arc<World>> {
    let want = normalize(name);
    worlds(server).into_iter().find(|w| world_name(w) == want)
}

struct Native<'a>(&'a World);

impl Host for Native<'_> {
    fn is_loaded(&self, x: i32, z: i32) -> bool {
        self.0.level.loaded_chunks.contains_key(&Vector2::new(x, z))
    }
    fn request(&self, chunks: &[(i32, i32)]) {
        let mut l = self.0.level.chunk_loading.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for &(x, z) in chunks {
            l.add_force_ticket(Vector2::new(x, z));
        }
        l.send_change();
    }
    fn release(&self, chunks: &[(i32, i32)]) {
        let mut l = self.0.level.chunk_loading.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for &(x, z) in chunks {
            l.remove_force_ticket(Vector2::new(x, z));
        }
        l.send_change();
    }
}

// ---- messages -------------------------------------------------------------

fn color(s: &str) -> TextComponent {
    TextComponent::from_legacy_string_with_code(s, '&')
}

fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '&' && it.peek().is_some_and(char::is_ascii_alphanumeric) {
            it.next();
        } else {
            out.push(c);
        }
    }
    out
}

fn log(s: &str) {
    tracing::info!("{}", plain(s));
}

fn notify(server: &Server, who: &Option<String>, s: &str, bar: bool) {
    if let Some(p) = who.as_deref().and_then(|n| server.get_player_by_name(n)) {
        p.send_system_message_raw(&color(s), bar);
    }
}

fn hms(secs: u64) -> String {
    format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

fn task_vars(t: &mut Task) -> Vec<(&'static str, String)> {
    let rate = t.rate();
    let left = t.total.saturating_sub(t.processed());
    let eta = if rate > 0.0 { hms((left as f64 / rate) as u64) } else { "-".into() };
    vec![
        ("world", t.sel.world.clone()),
        ("done", t.processed().to_string()),
        ("total", t.total.to_string()),
        ("percent", format!("{:.2}", t.percent())),
        ("eta", eta),
        ("rate", format!("{rate:.1}")),
        ("current", format!("{}, {}", t.last.0, t.last.1)),
    ]
}

fn sel_vars(st: &State, sel: &Selection) -> Vec<(&'static str, String)> {
    vec![
        ("world", sel.world.clone()),
        ("shape", sel.shape.name().into()),
        ("center", sel.center_text()),
        ("radius", sel.radius_text()),
        ("pattern", sel.pattern.name().into()),
        ("total", sel.total(&st.load_csv(sel)).to_string()),
    ]
}

// ---- task control ---------------------------------------------------------

fn start_task(st: &mut State, server: &Server, sel: Selection, starter: Option<String>) -> String {
    let l = st.cfg.language.clone();
    let Some(world) = find_world(server, &sel.world) else {
        return fmt(&l, "bad_world", &[("arg", sel.world.clone()), ("list", world_names(server).join(", "))]);
    };
    let csv = st.load_csv(&sel);
    if sel.pattern == Pattern::Csv && csv.is_empty() {
        return fmt(&l, "csv_missing", &[("arg", sel.csv.clone().unwrap_or_default())]);
    }
    if sel.total(&csv) == 0 {
        return fmt(&l, "empty", &[]);
    }
    if let Some(mut old) = st.tasks.remove(&sel.world) {
        old.abort(&Native(&world));
    }
    let vars = sel_vars(st, &sel);
    let mut task = Task::new(sel, &csv, None);
    task.starter = starter;
    st.save_task(&task);
    st.tasks.insert(task.sel.world.clone(), task);
    fmt(&l, "started", &vars)
}

fn pause_tasks(st: &mut State, server: &Server, world: Option<&str>) -> Vec<String> {
    let l = st.cfg.language.clone();
    let ids: Vec<String> = st.tasks.keys().filter(|id| world.is_none_or(|w| w == id.as_str())).cloned().collect();
    let mut out = Vec::new();
    for id in ids {
        if let Some(mut t) = st.tasks.remove(&id) {
            if let Some(w) = find_world(server, &id) {
                t.abort(&Native(&w));
            }
            st.save_task(&t);
            out.push(fmt(&l, "paused", &[("world", id), ("percent", format!("{:.2}", t.percent()))]));
        }
    }
    out
}

/// One line per saved job that is not running, so a paused job is visible after a restart
fn waiting_tasks(st: &State) -> Vec<String> {
    st.saved_tasks()
        .into_iter()
        .filter(|saved| !st.tasks.contains_key(&saved.selection.world))
        .map(|saved| {
            let csv = st.load_csv(&saved.selection);
            let task = Task::new(saved.selection.clone(), &csv, Some(&saved));
            fmt(&st.cfg.language, "waiting", &[("world", saved.selection.world.clone()), ("percent", format!("{:.2}", task.percent()))])
        })
        .collect()
}

fn continue_tasks(st: &mut State, server: &Server, world: Option<&str>) -> Vec<String> {
    let l = st.cfg.language.clone();
    let mut out = Vec::new();
    for saved in st.saved_tasks() {
        let id = saved.selection.world.clone();
        if world.is_some_and(|w| w != id) || st.tasks.contains_key(&id) || find_world(server, &id).is_none() {
            continue;
        }
        let csv = st.load_csv(&saved.selection);
        let task = Task::new(saved.selection.clone(), &csv, Some(&saved));
        out.push(fmt(&l, "continued", &[("world", id.clone()), ("percent", format!("{:.2}", task.percent()))]));
        st.tasks.insert(id, task);
    }
    out
}

fn cancel_tasks(st: &mut State, server: &Server, world: Option<&str>) -> Vec<String> {
    let l = st.cfg.language.clone();
    let mut ids: Vec<String> = st.tasks.keys().cloned().collect();
    ids.extend(st.saved_tasks().into_iter().map(|s| s.selection.world));
    ids.sort();
    ids.dedup();
    let mut out = Vec::new();
    for id in ids.into_iter().filter(|id| world.is_none_or(|w| w == id)) {
        if let Some(mut t) = st.tasks.remove(&id) {
            if let Some(w) = find_world(server, &id) {
                t.abort(&Native(&w));
            }
        }
        st.remove_task_file(&id);
        out.push(fmt(&l, "cancelled", &[("world", id)]));
    }
    out
}

/// Runs every 50 ms on Pregen's own thread (not on the server tick).
fn drive(server: &Server) {
    let mut guard = lock();
    let Some(st) = guard.as_mut() else { return };
    if st.tasks.is_empty() {
        return;
    }
    // Average of the last 100 ticks, kept by the server
    let mspt = server.aggregated_tick_times_nanos.load(Ordering::Relaxed) as f64 / 100.0 / 1e6;
    let limits = st.cfg.limits(server.get_player_count() > 0, mspt);
    let l = st.cfg.language.clone();
    let mut finished = Vec::new();
    for (id, task) in st.tasks.iter_mut() {
        let Some(world) = find_world(server, id) else { continue };
        if task.tick(&Native(&world), &limits) {
            finished.push(id.clone());
        }
    }
    for id in finished {
        if let Some(t) = st.tasks.remove(&id) {
            st.remove_task_file(&id);
            let msg = fmt(&l, "finished", &[("world", id), ("total", t.total.to_string()), ("time", hms(t.elapsed()))]);
            log(&msg);
            notify(server, &t.starter, &msg, false);
        }
    }
    if st.last_report.elapsed().as_secs() >= st.cfg.update_interval.max(1) {
        st.last_report = Instant::now();
        let silent = st.cfg.silent;
        for t in st.tasks.values_mut() {
            let msg = fmt(&l, "progress", &task_vars(t));
            if !silent {
                log(&msg);
                notify(server, &t.starter, &msg, true);
            }
        }
    }
    if st.last_save.elapsed().as_secs() >= 30 {
        st.last_save = Instant::now();
        for t in st.tasks.values() {
            st.save_task(t);
        }
    }
}

// ---- commands -------------------------------------------------------------

struct Who {
    key: String,
    player: Option<String>,
    pos: Option<(f64, f64)>,
    world: Option<String>,
}

fn who(ctx: &CommandContext) -> Who {
    match ctx.source.as_player() {
        Some(p) => {
            let pos = p.position();
            Who {
                key: p.gameprofile.name.clone(),
                player: Some(p.gameprofile.name.clone()),
                pos: Some((pos.x.floor(), pos.z.floor())),
                world: Some(world_name(&p.world())),
            }
        }
        None => Who { key: "console".into(), player: None, pos: None, world: None },
    }
}

fn run(ctx: &CommandContext, server: &Server, w: &[String]) -> Vec<String> {
    let mut guard = lock();
    let Some(st) = guard.as_mut() else { return Vec::new() };
    let lang = st.cfg.language.clone();
    let l = lang.as_str();
    let sub = w.first().map_or_else(|| "help".to_owned(), |s| s.to_lowercase());
    let sub = sub.as_str();
    if !SUBS.contains(&sub) {
        return vec![fmt(l, "unknown", &[("arg", sub.to_owned())])];
    }
    if !ctx.source.has_permission(&format!("pregen:command.{sub}")) {
        return vec![fmt(l, "no_perm", &[])];
    }
    let me = who(ctx);
    let default_world = me.world.clone().unwrap_or_else(|| "world".into());
    let mut sel = st.selections.get(&me.key).cloned().unwrap_or_else(|| Selection::new(default_world));
    let arg = |i: usize| w.get(i).map(String::as_str);
    let bad_num = |s: &str| vec![fmt(l, "bad_number", &[("arg", s.to_owned())])];
    let usage = |u: &str| vec![fmt(l, "usage", &[("arg", u.to_owned())])];
    let world_arg = |i: usize| -> Result<Option<String>, Vec<String>> {
        match arg(i) {
            None => Ok(None),
            Some(name) => match find_world(server, name) {
                Some(wd) => Ok(Some(world_name(&wd))),
                None => Err(vec![fmt(l, "bad_world", &[("arg", name.to_owned()), ("list", world_names(server).join(", "))])]),
            },
        }
    };

    let out = match sub {
        "help" => vec![fmt(l, "help", &[])],
        "version" => vec![format!("{}&fPregen {VERSION}", text(l, "prefix"))],
        "trim" => vec![fmt(l, "trim", &[])],
        "world" => match world_arg(1) {
            Ok(Some(id)) => {
                sel.world = id.clone();
                vec![fmt(l, "set_world", &[("arg", id)])]
            }
            Ok(None) => usage("/pregen world <world>"),
            Err(e) => e,
        },
        "center" => {
            let (bx, bz) = me.pos.unwrap_or((sel.cx, sel.cz));
            match (arg(1), arg(2)) {
                (None, _) => match me.pos {
                    Some((x, z)) => {
                        sel.cx = x;
                        sel.cz = z;
                        vec![fmt(l, "set_center", &[("arg", sel.center_text())])]
                    }
                    None => vec![fmt(l, "player_only", &[])],
                },
                (Some(a), Some(b)) => match (parse_coord(a, bx), parse_coord(b, bz)) {
                    (Some(x), Some(z)) => {
                        sel.cx = x;
                        sel.cz = z;
                        vec![fmt(l, "set_center", &[("arg", sel.center_text())])]
                    }
                    (None, _) => bad_num(a),
                    _ => bad_num(b),
                },
                _ => usage("/pregen center [x z]"),
            }
        }
        "spawn" => match find_world(server, &sel.world) {
            Some(world) => {
                let (pos, _, _) = world.get_spawn_location();
                sel.cx = f64::from(pos.0.x);
                sel.cz = f64::from(pos.0.z);
                vec![fmt(l, "set_center", &[("arg", sel.center_text())])]
            }
            None => vec![fmt(l, "bad_world", &[("arg", sel.world.clone()), ("list", world_names(server).join(", "))])],
        },
        "worldborder" => match find_world(server, &sel.world) {
            Some(world) => {
                let b = world.worldborder.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                sel.cx = b.center_x;
                sel.cz = b.center_z;
                sel.rx = b.new_diameter / 2.0;
                sel.rz = sel.rx;
                sel.shape = Kind::Square;
                drop(b);
                vec![fmt(l, "selection", &sel_vars(st, &sel))]
            }
            None => vec![fmt(l, "bad_world", &[("arg", sel.world.clone()), ("list", world_names(server).join(", "))])],
        },
        "radius" => match arg(1) {
            None => usage("/pregen radius <radius> [radiusZ]"),
            Some(a) => match parse_len(a, sel.rx) {
                None => bad_num(a),
                Some(rx) => match arg(2).map(|b| (b, parse_len(b, sel.rz))) {
                    Some((b, None)) => bad_num(b),
                    other => {
                        sel.rx = rx;
                        sel.rz = other.and_then(|(_, v)| v).unwrap_or(rx);
                        vec![fmt(l, "set_radius", &[("arg", sel.radius_text())])]
                    }
                },
            },
        },
        "shape" => match arg(1) {
            None => usage("/pregen shape <shape>"),
            Some(s) => match Kind::parse(s) {
                Some(k) => {
                    sel.shape = k;
                    vec![fmt(l, "set_shape", &[("arg", k.name().into())])]
                }
                None => vec![fmt(l, "bad_shape", &[("arg", s.into()), ("list", Kind::NAMES.join(", "))])],
            },
        },
        "pattern" => match arg(1) {
            None => usage("/pregen pattern <pattern>"),
            Some(s) => match Pattern::parse(s) {
                Some(Pattern::Csv) => match arg(2) {
                    None => usage("/pregen pattern csv <file.csv>"),
                    Some(file) if file.contains("..") || file.starts_with('/') => {
                        vec![fmt(l, "csv_missing", &[("arg", file.into())])]
                    }
                    Some(file) => {
                        sel.pattern = Pattern::Csv;
                        sel.csv = Some(file.to_owned());
                        if st.load_csv(&sel).is_empty() {
                            vec![fmt(l, "csv_missing", &[("arg", file.into())])]
                        } else {
                            vec![fmt(l, "set_pattern", &[("arg", format!("csv {file}"))])]
                        }
                    }
                },
                Some(p) => {
                    sel.pattern = p;
                    sel.csv = None;
                    vec![fmt(l, "set_pattern", &[("arg", p.name().into())])]
                }
                None => vec![fmt(l, "bad_pattern", &[("arg", s.into()), ("list", Pattern::NAMES.join(", "))])],
            },
        },
        "corners" => {
            let v: Vec<Option<f64>> = (1..=4).map(|i| arg(i).and_then(|s| parse_coord(s, 0.0))).collect();
            if v.iter().any(Option::is_none) {
                usage("/pregen corners <x1> <z1> <x2> <z2>")
            } else {
                let v: Vec<f64> = v.into_iter().flatten().collect();
                sel.shape = Kind::Rectangle;
                sel.cx = (v[0] + v[2]) / 2.0;
                sel.cz = (v[1] + v[3]) / 2.0;
                sel.rx = (v[2] - v[0]).abs() / 2.0;
                sel.rz = (v[3] - v[1]).abs() / 2.0;
                vec![fmt(l, "selection", &sel_vars(st, &sel))]
            }
        }
        "selection" => vec![fmt(l, "selection", &sel_vars(st, &sel))],
        "start" => 'start: {
            match world_arg(1) {
                Ok(Some(id)) => sel.world = id,
                Ok(None) => {}
                Err(e) => break 'start e,
            }
            if let Some(s) = arg(2) {
                match Kind::parse(s) {
                    Some(k) => sel.shape = k,
                    None => break 'start vec![fmt(l, "bad_shape", &[("arg", s.into()), ("list", Kind::NAMES.join(", "))])],
                }
            }
            if let (Some(a), Some(b)) = (arg(3), arg(4)) {
                let (bx, bz) = me.pos.unwrap_or((0.0, 0.0));
                match (parse_coord(a, bx), parse_coord(b, bz)) {
                    (Some(x), Some(z)) => {
                        sel.cx = x;
                        sel.cz = z;
                    }
                    (None, _) => break 'start bad_num(a),
                    _ => break 'start bad_num(b),
                }
            }
            if let Some(r) = arg(5) {
                match parse_len(r, sel.rx) {
                    Some(v) => {
                        sel.rx = v;
                        sel.rz = v;
                    }
                    None => break 'start bad_num(r),
                }
            }
            if let Some(r) = arg(6) {
                match parse_len(r, sel.rz) {
                    Some(v) => sel.rz = v,
                    None => break 'start bad_num(r),
                }
            }
            let exists = st.tasks.contains_key(&sel.world) || st.saved_tasks().iter().any(|s| s.selection.world == sel.world);
            if exists {
                st.pending.insert(me.key.clone(), Pending::Start(sel.clone(), me.player.clone()));
                vec![fmt(l, "running_exists", &[("world", sel.world.clone())])]
            } else {
                vec![start_task(st, server, sel.clone(), me.player.clone())]
            }
        }
        "confirm" => match st.pending.remove(&me.key) {
            Some(Pending::Start(s, starter)) => {
                st.remove_task_file(&s.world);
                vec![start_task(st, server, s, starter)]
            }
            Some(Pending::Cancel(world)) => cancel_tasks(st, server, world.as_deref()),
            None => vec![fmt(l, "nothing_confirm", &[])],
        },
        "pause" => match world_arg(1) {
            Ok(world) => {
                let out = pause_tasks(st, server, world.as_deref());
                if out.is_empty() { vec![fmt(l, "no_tasks", &[])] } else { out }
            }
            Err(e) => e,
        },
        "continue" => match world_arg(1) {
            Ok(world) => {
                let out = continue_tasks(st, server, world.as_deref());
                if out.is_empty() { vec![fmt(l, "no_saved", &[])] } else { out }
            }
            Err(e) => e,
        },
        "cancel" => match world_arg(1) {
            Ok(world) => {
                let label = world.clone().unwrap_or_else(|| if l == "en" { "all".into() } else { "ทั้งหมด".into() });
                st.pending.insert(me.key.clone(), Pending::Cancel(world));
                vec![fmt(l, "confirm_cancel", &[("world", label)])]
            }
            Err(e) => e,
        },
        "progress" => {
            let mut out: Vec<String> = st.tasks.values_mut().map(|t| fmt(l, "progress", &task_vars(t))).collect();
            out.extend(waiting_tasks(st));
            if out.is_empty() {
                out.push(fmt(l, "no_tasks", &[]));
            }
            out
        }
        "quiet" => match arg(1).and_then(|s| s.parse::<u64>().ok()) {
            Some(n) => {
                st.cfg.update_interval = n.max(1);
                st.save_config();
                vec![fmt(l, "quiet", &[("arg", st.cfg.update_interval.to_string())])]
            }
            None => usage("/pregen quiet <seconds>"),
        },
        "silent" => {
            st.cfg.silent = !st.cfg.silent;
            st.save_config();
            vec![fmt(l, if st.cfg.silent { "silent_on" } else { "silent_off" }, &[])]
        }
        "reload" => {
            st.load_config();
            vec![fmt(&st.cfg.language, "reloaded", &[])]
        }
        _ => vec![fmt(l, "unknown", &[("arg", sub.to_owned())])],
    };
    st.selections.insert(me.key, sel);
    out
}

struct Exec;

impl CommandExecutor for Exec {
    fn execute(&self, ctx: &CommandContext) -> CommandExecutorResult {
        let Some(server) = SERVER.get() else { return Ok(0) };
        let words: Vec<String> = ctx
            .get_argument::<String>("args")
            .map(|s| s.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default();
        for line in run(ctx, server, &words) {
            ctx.source.send_message(color(&line));
        }
        Ok(1)
    }
}

struct Suggest;

impl SuggestionProvider for Suggest {
    fn suggest(&self, _ctx: &CommandContext, builder: SuggestionsBuilder) -> Suggestions {
        let remaining = builder.remaining().to_owned();
        let cut = remaining.rfind(' ').map_or(0, |i| i + 1);
        let typed: Vec<&str> = remaining[..cut].split_whitespace().collect();
        let names = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let list = match (typed.len(), typed.first().copied()) {
            (0, _) => names(&SUBS),
            (1, Some("start" | "world" | "pause" | "continue" | "cancel")) => SERVER.get().map(|s| world_names(s)).unwrap_or_default(),
            (1, Some("shape")) | (2, Some("start")) => names(&Kind::NAMES),
            (1, Some("pattern")) => names(&Pattern::NAMES),
            _ => Vec::new(),
        };
        builder.create_offset(builder.start + cut).filter_and_suggest_lowercase(list).build()
    }
}

/// The server's offset from UTC. `time` refuses to work it out in a process with several
/// threads, so ask libc, which is what `time` does itself at server start.
fn local_offset() -> time::UtcOffset {
    let mut tm = std::mem::MaybeUninit::<libc::tm>::zeroed();
    // SAFETY: `time` only returns the clock; `localtime_r` writes into `tm`, which is ours, and
    // returns null on failure. Nothing in the server changes the time zone while it runs.
    let secs = unsafe {
        let now = libc::time(std::ptr::null_mut());
        if libc::localtime_r(&now, tm.as_mut_ptr()).is_null() { 0 } else { tm.assume_init().tm_gmtoff }
    };
    i32::try_from(secs).ok().and_then(|s| time::UtcOffset::from_whole_seconds(s).ok()).unwrap_or(time::UtcOffset::UTC)
}

/// Console lines of this plugin. `Context::init_log` of Pumpkin 0.2.2 stamps them in UTC with a
/// date (`2026-11-10 16:20:44`) next to the server's own local `23:20:44`; this is the same
/// setup with the server's timestamp format and offset.
fn init_log(context: &Context) {
    use tracing_subscriber::fmt::{self, time::OffsetTime};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let Some(Some((_, level, config))) = context.logger.get() else { return };
    let layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(config.color)
        .with_ansi_sanitization(false)
        .with_target(config.target)
        .with_thread_names(config.threads)
        .with_thread_ids(config.thread_ids);
    let registry = tracing_subscriber::registry().with(*level);
    if config.timestamp {
        // Leaked once per load, like the server does: the parsed format borrows the text
        let wanted: &'static str = Box::leak(context.server.advanced_config.logging.timestamp_format.clone().into_boxed_str());
        let format = time::format_description::parse_borrowed::<1>(wanted)
            .or_else(|_| time::format_description::parse_borrowed::<1>("[hour]:[minute]:[second]"))
            .unwrap_or_default();
        let _ = registry.with(layer.with_timer(OffsetTime::new(local_offset(), format))).try_init();
    } else {
        let _ = registry.with(layer.without_time()).try_init();
    }
}

pub(crate) fn load(context: &Arc<Context>) -> Result<(), String> {
    init_log(context);
    let _ = SERVER.set(context.server.clone());
    let mut st = State {
        dir: context.get_data_folder(),
        cfg: Config::default(),
        tasks: BTreeMap::new(),
        selections: HashMap::new(),
        pending: HashMap::new(),
        last_report: Instant::now(),
        last_save: Instant::now(),
    };
    st.load_config();
    let resume = st.cfg.continue_on_restart;
    // The panel reads this to tell "loaded" from "installed, waiting for a restart"
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    write_atomic(&st.path("status.json"), format!("{{\"version\":\"{VERSION}\",\"loaded_at\":{now}}}").as_bytes());
    *lock() = Some(st);

    for node in std::iter::once("pregen:command".to_owned()).chain(SUBS.iter().map(|s| format!("pregen:command.{s}"))) {
        let _ = context.register_permission(Permission::new(&node, "Pregen", PermissionDefault::Op(PermissionLvl::Two)));
    }
    let exec = Arc::new(Exec);
    // `chunky` is the name people coming from Chunky type first
    context.register_command_with_aliases(
        command("pregen", "Pre-generate chunks (Chunky-style)")
            .executes_arc(exec.clone())
            .then(argument("args", StringArgumentType::GreedyPhrase).suggests(Suggest).executes_arc(exec)),
        &["chunky".to_owned()],
        "pregen:command",
    );

    STOP.store(false, Ordering::SeqCst);
    let driver = std::thread::Builder::new()
        .name("pregen".into())
        .spawn(move || {
            // Once the worlds are up: continue saved jobs, or say that they are waiting
            let mut greeted = false;
            let started = Instant::now();
            while !STOP.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
                let Some(server) = SERVER.get() else { continue };
                if !greeted && started.elapsed().as_secs() >= 5 && !server.worlds.load().is_empty() {
                    greeted = true;
                    if let Some(st) = lock().as_mut() {
                        let lines = if resume { continue_tasks(st, server, None) } else { waiting_tasks(st) };
                        for m in lines {
                            log(&m);
                        }
                    }
                }
                drive(server);
            }
        })
        .map_err(|e| e.to_string())?;
    *DRIVER.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(driver);
    tracing::info!("Pregen {VERSION} loaded");
    Ok(())
}

pub(crate) fn unload() -> Result<(), String> {
    STOP.store(true, Ordering::SeqCst);
    // The driver must be gone before the server unmaps this library
    let driver = DRIVER.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    if let Some(driver) = driver {
        let _ = driver.join();
    }
    if let (Some(server), Some(st)) = (SERVER.get(), lock().as_mut()) {
        for m in pause_tasks(st, server, None) {
            log(&m);
        }
        let _ = std::fs::remove_file(st.path("status.json"));
    }
    Ok(())
}

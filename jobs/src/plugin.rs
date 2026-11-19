//! Pumpkin WASM glue for Jobs: the events that pay, the window, payments through Basics.
//!
//! Money is added up and paid every `pay-every` seconds in one Basics call per player (Basics
//! is asked by message; a payment it did not take waits for the next round). Experience counts
//! at once. Blocks placed by players are remembered (`placed-memory`, in memory only) so
//! breaking them again pays nothing; after a restart those blocks pay once more.
//! `players.json` keeps each player's jobs and levels.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use mcsv_common as c;
use c::deeds::{self, Deed, Kills};
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::GameMode;
use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, InventoryClickEvent, InventoryCloseEvent};
use pumpkin_plugin_api::gui::Gui;
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::screens_wit::Screen;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::defs::{self, Config, Job, NS, Note, Worker};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&6Jobs&8]&r ";
const FILE: &str = "players.json";

type Spot = (String, i32, i32, i32);

struct State {
    cfg: Config,
    list: Vec<Job>,
    notes: Vec<Note>,
    workers: HashMap<String, Worker>,
    /// Money not paid yet, and what it was for (for the action bar).
    owed: HashMap<String, (f64, f64)>,
    placed: HashSet<Spot>,
    placed_order: VecDeque<Spot>,
    kills: Kills,
    /// Players with the `/jobs` window open.
    wins: HashSet<String>,
    tick: u64,
    dirty: bool,
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

fn load() -> (Config, Vec<Job>, Vec<Note>) {
    let cfg = c::load_config::<Config>("config.toml").cleaned();
    let mut notes = Vec::new();
    let first = !c::data_path("jobs").exists();
    let _ = std::fs::create_dir_all(c::data_path("jobs"));
    if first {
        let _ = std::fs::write(c::data_path("jobs/example.toml"), defs::EXAMPLE);
    }
    let mut names: Vec<String> = std::fs::read_dir(c::data_path("jobs")).map(|d| d.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.ends_with(".toml")).collect()).unwrap_or_default();
    names.sort();
    let mut all = Vec::new();
    for name in names {
        let file = format!("jobs/{name}");
        match std::fs::read_to_string(c::data_path(&file)) {
            Ok(text) => all.extend(defs::parse_jobs(&file, &text, &mut notes)),
            Err(_) => notes.push(Note::new(&file, "อ่านไฟล์ไม่ได้", "cannot be read")),
        }
    }
    let list = defs::merge_jobs(all, &mut notes);
    (cfg, list, notes)
}

fn find<'a>(list: &'a [Job], id: &str) -> Option<&'a Job> {
    list.iter().find(|j| j.id == id)
}

fn money_text(n: f64) -> String {
    if n >= 100.0 { format!("{n:.0}") } else { format!("{n:.2}") }
}

// ---- paying ---------------------------------------------------------------------------

/// A player did something: pay their jobs for it.
fn credit(st: &mut State, p: &Player, deed: &Deed) {
    if matches!(p.get_gamemode(), GameMode::Creative | GameMode::Spectator) {
        return;
    }
    let me = c::game::key(p);
    let Some(worker) = st.workers.get_mut(&me) else { return };
    if worker.jobs.is_empty() {
        return;
    }
    let th = st.cfg.thai();
    let (mut money, mut xp) = (0.0, 0.0);
    let mut ups = Vec::new();
    for (id, progress) in &mut worker.jobs {
        let Some(job) = st.list.iter().find(|j| j.id == *id) else { continue };
        let Some(pay) = job.pay_for(deed.kind(), deed.target()) else { continue };
        let n = f64::from(deed.count());
        money += defs::money_at(&st.cfg, pay.money, progress.level) * n;
        xp += pay.xp * n;
        if defs::add_xp(&st.cfg, progress, pay.xp * n, job.max_level) > 0 {
            ups.push((job.name.clone(), progress.level));
        }
    }
    if money > 0.0 || xp > 0.0 {
        let owed = st.owed.entry(me).or_default();
        owed.0 += money;
        owed.1 += xp;
        st.dirty = true;
    }
    for (name, level) in ups {
        c::tell(p, &msg(th, &format!("&a{name} &aเลเวล {level} แล้ว!"), &format!("&a{name} &ais level {level} now!")));
        p.play_custom_sound("minecraft:entity.player.levelup", pumpkin_plugin_api::wit::pumpkin::plugin::sounds::SoundCategory::Players, 0.6, 1.0);
    }
}

/// Pay what is owed, one Basics call per player.
fn pay_out(st: &mut State, server: &Server) {
    let th = st.cfg.thai();
    let owed = std::mem::take(&mut st.owed);
    for (k, (money, xp)) in owed {
        let Some(p) = c::game::online(server, &k) else {
            // Paid when they are back online (by name Basics also knows offline players)
            let name = st.workers.get(&k).map(|w| w.name.clone()).unwrap_or_default();
            if money > 0.0 && (name.is_empty() || c::game::money::deposit(&name, money).is_none()) {
                st.owed.insert(k, (money, 0.0));
            } else if let Some(w) = st.workers.get_mut(&k) {
                w.earned += money;
            }
            continue;
        };
        if money > 0.0 && c::game::money::deposit(&p.get_name(), money).is_none() {
            st.owed.insert(k, (money, xp));
            continue;
        }
        if let Some(w) = st.workers.get_mut(&k) {
            w.earned += money;
        }
        let line = if money > 0.0 { format!("&a+{} &8| &b+{:.0} xp", money_text(money), xp) } else { format!("&b+{xp:.0} xp") };
        c::action_bar(&p, &format!("{}{line}", if th { "&6งาน " } else { "&6Jobs " }));
    }
}

fn remember_placed(st: &mut State, spot: Spot) {
    if st.cfg.placed_memory == 0 {
        return;
    }
    if st.placed.insert(spot.clone()) {
        st.placed_order.push_back(spot);
    }
    while st.placed_order.len() > st.cfg.placed_memory as usize {
        if let Some(old) = st.placed_order.pop_front() {
            st.placed.remove(&old);
        }
    }
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.tick += 1;
    let tick = st.tick;
    if tick % (u64::from(st.cfg.pay_every) * 20) == 0 && !st.owed.is_empty() {
        pay_out(st, &server);
    }
    if st.dirty && tick % 1200 == 0 && c::save_json(FILE, &st.workers) {
        st.dirty = false;
    }
}

// ---- events ------------------------------------------------------------------------

type Data<E> = <E as FromIntoEvent>::Data;

fn with_state(f: impl FnOnce(&mut State)) {
    if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
        f(st);
    }
}

/// Jobs hears of deeds through `mcsv_common::deeds`.
#[derive(Clone, Copy)]
struct Deeds;

impl deeds::Sink for Deeds {
    fn deed(&self, _server: &Server, p: &Player, deed: Deed, spot: Option<deeds::Spot>) {
        with_state(|st| {
            match (&deed, spot) {
                // A block a player placed pays nothing when it is broken again
                (Deed::Break(_), Some(spot)) if st.placed.remove(&spot) => return,
                (Deed::Place(_), Some(spot)) => remember_placed(st, spot),
                _ => {}
            }
            credit(st, p, &deed);
        });
    }

    fn with_kills(&self, f: &mut dyn FnMut(&mut Kills, u64)) {
        with_state(|st| f(&mut st.kills, st.tick));
    }
}

// ---- the window --------------------------------------------------------------------

fn job_lines(st: &State, job: &Job, th: bool, have: Option<&defs::Progress>) -> Vec<String> {
    let mut lore: Vec<String> = job.description.iter().map(|d| format!("&7{d}")).collect();
    if let Some(p) = have {
        lore.push(format!("&f{} {}/{} &8| &b{:.0}/{:.0} xp", if th { "เลเวล" } else { "Level" }, p.level, job.max_level, p.xp, st.cfg.need(p.level)));
    }
    for pay in job.pays.iter().take(6) {
        lore.push(format!("&8- &7{} {} &a{} &b{} xp", pay.kind, pay.pattern, money_text(pay.money), pay.xp));
    }
    if job.pays.len() > 6 {
        lore.push(format!("&8... /jobs info {}", job.id));
    }
    lore
}

fn open(st: &mut State, p: &Player) {
    let th = st.cfg.thai();
    let me = c::game::key(p);
    let gui = Gui::new(Screen::Generic9x6, c::color(if th { "&8งาน" } else { "&8Jobs" }));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    let inv = gui.get_inventory();
    let mine = st.workers.get(&me).map(|w| w.jobs.clone()).unwrap_or_default();
    for (i, job) in st.list.iter().take(54).enumerate() {
        let have = mine.get(&job.id);
        let mut lore = job_lines(st, job, th, have);
        lore.push(match (have.is_some(), th) {
            (true, true) => "&aทำอยู่ &7| คลิกเพื่อลาออก".into(),
            (true, false) => "&aYour job &7| click to leave".into(),
            (false, true) => "&eคลิกเพื่อสมัคร".into(),
            (false, false) => "&eClick to join".into(),
        });
        let stack = c::game::button(&job.icon, &job.name, &lore);
        if have.is_some() {
            stack.set_component(pumpkin_plugin_api::data_components::DataComponent::EnchantmentGlintOverride, &[1]);
        }
        inv.set_item(i as u32, Some(stack));
    }
    st.wins.insert(me);
    p.open_gui(gui);
}

fn join(st: &mut State, p: &Player, id: &str) -> String {
    let th = st.cfg.thai();
    let Some(job) = find(&st.list, id).cloned() else { return msg(th, "&cไม่มีงานนี้", "&cNo such job") };
    if let Some(node) = &job.permission
        && !p.has_permission(node)
    {
        return msg(th, "&cคุณยังสมัครงานนี้ไม่ได้", "&cYou may not join this job yet");
    }
    let max = st.cfg.max_jobs as usize;
    let w = st.workers.entry(c::game::key(p)).or_default();
    w.name = p.get_name();
    if w.jobs.contains_key(&job.id) {
        return msg(th, "&7คุณทำงานนี้อยู่แล้ว", "&7You have this job already");
    }
    if w.jobs.len() >= max {
        return msg(th, &format!("&cทำได้ไม่เกิน {max} งาน ลาออกจากงานหนึ่งก่อน"), &format!("&cAt most {max} jobs: leave one first"));
    }
    w.jobs.insert(job.id.clone(), defs::Progress::default());
    st.dirty = true;
    msg(th, &format!("&aสมัครงาน {} &aแล้ว", job.name), &format!("&aYou joined {}", job.name))
}

fn leave(st: &mut State, p: &Player, id: &str) -> String {
    let th = st.cfg.thai();
    let removed = st.workers.get_mut(&c::game::key(p)).and_then(|w| w.jobs.remove(id)).is_some();
    st.dirty |= removed;
    if removed { msg(th, "&7ลาออกแล้ว (เลเวลของงานนี้หายไป)", "&7You left (that job's level is gone)") } else { msg(th, "&cคุณไม่ได้ทำงานนี้", "&cYou do not have this job") }
}

struct OnClick;
impl EventHandler<InventoryClickEvent> for OnClick {
    fn handle(&self, _server: Server, mut e: Data<InventoryClickEvent>) -> Data<InventoryClickEvent> {
        if !matches!(e.window_type, Some(Screen::Generic9x6)) {
            return e;
        }
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let me = c::game::key(&e.player);
            if !st.wins.contains(&me) {
                return e;
            }
            e.cancelled = true;
            if let Some(job) = usize::try_from(e.raw_slot).ok().and_then(|s| st.list.get(s)).map(|j| j.id.clone()) {
                let has = st.workers.get(&me).is_some_and(|w| w.jobs.contains_key(&job));
                let line = if has { leave(st, &e.player, &job) } else { join(st, &e.player, &job) };
                c::tell(&e.player, &line);
                open(st, &e.player);
            }
        }
        e
    }
}

struct OnClose;
impl EventHandler<InventoryCloseEvent> for OnClose {
    fn handle(&self, _server: Server, e: Data<InventoryCloseEvent>) -> Data<InventoryCloseEvent> {
        with_state(|st| {
            st.wins.remove(&c::game::key(&e.player));
        });
        e
    }
}

// ---- commands ----------------------------------------------------------------------

fn run(st: &mut State, sender: &CommandSender, server: &Server, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    let me = c::sender_player(sender);
    let admin = c::has_perm(sender, &format!("{NS}:admin"));
    let in_game = || vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")];
    match w.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        None => {
            let Some(p) = me else { return in_game() };
            open(st, &p);
            Vec::new()
        }
        Some("join") => match (me, w.get(1)) {
            (Some(p), Some(id)) => vec![join(st, &p, id)],
            (None, _) => in_game(),
            _ => vec![msg(th, "&f/jobs join <งาน>", "&f/jobs join <job>")],
        },
        Some("leave") => match (me, w.get(1)) {
            (Some(p), Some(id)) => vec![leave(st, &p, id)],
            (None, _) => in_game(),
            _ => vec![msg(th, "&f/jobs leave <งาน>", "&f/jobs leave <job>")],
        },
        Some("info") => {
            let Some(job) = w.get(1).and_then(|id| find(&st.list, id)) else { return vec![msg(th, "&cไม่มีงานนี้", "&cNo such job")] };
            let mut out = vec![msg(th, &format!("&f{} &7({})", job.name, job.id), &format!("&f{} &7({})", job.name, job.id))];
            out.extend(job.pays.iter().take(40).map(|p| format!("&8- &7{} {} &a{} &b{} xp", p.kind, p.pattern, money_text(p.money), p.xp)));
            out
        }
        Some("stats") => {
            let target = match (w.get(1), &me) {
                (Some(name), _) => st.workers.iter().find(|(_, x)| x.name.eq_ignore_ascii_case(name)).map(|(k, _)| k.clone()),
                (None, Some(p)) => Some(c::game::key(p)),
                (None, None) => None,
            };
            let Some(worker) = target.and_then(|k| st.workers.get(&k)) else { return vec![msg(th, "&7ยังไม่มีงาน: /jobs", "&7No jobs yet: /jobs")] };
            let mut out = vec![msg(th, &format!("&f{} &7ได้เงินจากงานรวม {}", worker.name, money_text(worker.earned)), &format!("&f{} &7earned {} from jobs", worker.name, money_text(worker.earned)))];
            out.extend(worker.jobs.iter().map(|(id, p)| {
                let name = find(&st.list, id).map_or(id.as_str(), |j| j.name.as_str());
                format!("&8- {name} &7{} {} &8| &b{:.0}/{:.0} xp", if th { "เลเวล" } else { "level" }, p.level, p.xp, st.cfg.need(p.level))
            }));
            out
        }
        Some("top") => {
            let Some(job) = w.get(1).and_then(|id| find(&st.list, id)) else { return vec![msg(th, "&f/jobs top <งาน>", "&f/jobs top <job>")] };
            let mut rows: Vec<(&str, u32, f64)> = st.workers.values().filter_map(|x| x.jobs.get(&job.id).map(|p| (x.name.as_str(), p.level, p.xp))).collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.total_cmp(&a.2)));
            let mut out = vec![msg(th, &format!("&fอันดับ {}", job.name), &format!("&fTop of {}", job.name))];
            out.extend(rows.iter().take(10).enumerate().map(|(i, (n, l, _))| format!("&e{}. &f{n} &7{} {l}", i + 1, if th { "เลเวล" } else { "level" })));
            out
        }
        Some("reload") if admin => {
            let (cfg, list, notes) = load();
            (st.cfg, st.list, st.notes) = (cfg, list, notes);
            let mut out = vec![msg(st.cfg.thai(), &format!("&aอ่านใหม่แล้ว: {} งาน", st.list.len()), &format!("&aRead again: {} job(s)", st.list.len()))];
            out.extend(st.notes.iter().take(20).map(|n| format!("&6! &7{}", n.text(th))));
            out
        }
        Some("setlevel") if admin => {
            let (Some(name), Some(id), Some(level)) = (w.get(1), w.get(2), w.get(3).and_then(|l| l.parse::<u32>().ok())) else { return vec![msg(th, "&f/jobs setlevel <ผู้เล่น> <งาน> <เลเวล>", "&f/jobs setlevel <player> <job> <level>")] };
            let Some(max) = find(&st.list, id).map(|j| j.max_level) else { return vec![msg(th, "&cไม่มีงานนี้", "&cNo such job")] };
            let key = server.get_all_players().into_iter().find(|p| p.get_name().eq_ignore_ascii_case(name)).map(|p| c::game::key(&p)).or_else(|| st.workers.iter().find(|(_, x)| x.name.eq_ignore_ascii_case(name)).map(|(k, _)| k.clone()));
            let Some(key) = key else { return vec![msg(th, "&cไม่รู้จักผู้เล่นนี้", "&cUnknown player")] };
            let worker = st.workers.entry(key).or_default();
            if worker.name.is_empty() {
                worker.name = name.clone();
            }
            worker.jobs.insert(id.clone(), defs::Progress { level: level.min(max), xp: 0.0 });
            st.dirty = true;
            vec![msg(th, "&aตั้งเลเวลแล้ว", "&aLevel set")]
        }
        Some("version") => vec![format!("Jobs {VERSION}")],
        _ => {
            let mut out = vec![msg(th, "&f/jobs &7เปิดหน้าต่าง &7| &fjoin <งาน> &7| &fleave <งาน> &7| &finfo <งาน> &7| &fstats [ผู้เล่น] &7| &ftop <งาน>", "&f/jobs &7open the window &7| &fjoin <job> &7| &fleave <job> &7| &finfo <job> &7| &fstats [player] &7| &ftop <job>")];
            if admin {
                out.push(msg(th, "&f/jobs reload &7| &fsetlevel <ผู้เล่น> <งาน> <เลเวล> &7| &fversion", "&f/jobs reload &7| &fsetlevel <player> <job> <level> &7| &fversion"));
            }
            out
        }
    }
}

struct Cmd;
impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let lines = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            let Some(st) = guard.as_mut() else { return Ok(0) };
            run(st, &sender, &server, &w)
        };
        for line in lines {
            c::reply(&sender, &line);
        }
        Ok(1)
    }
}

// ---- the plugin ------------------------------------------------------------------

struct Jobs;

impl Plugin for Jobs {
    fn new() -> Self {
        Jobs
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Jobs that pay money and experience for what players do".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:use"), description: "Have jobs".into(), default: PermissionDefault::Allow, children: vec![] });
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Manage jobs".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let (cfg, list, notes) = load();
        let workers: HashMap<String, Worker> = c::load_json(FILE).unwrap_or_default();
        c::log_info(&format!("{} job(s), {} player(s){}", list.len(), workers.len(), if notes.is_empty() { String::new() } else { format!(", {} thing(s) to look at (/jobs reload)", notes.len()) }));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State {
            cfg,
            list,
            notes,
            workers,
            owed: HashMap::new(),
            placed: HashSet::new(),
            placed_order: VecDeque::new(),
            kills: Kills::default(),
            wins: HashSet::new(),
            tick: 0,
            dirty: false,
        });
        let cmd = Command::new(&[NS.to_owned(), "job".to_owned()], "Jobs").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:use"));
        deeds::listen(&context, Deeds)?;
        context.register_event_handler(OnClick, EventPriority::Normal, true)?;
        context.register_event_handler(OnClose, EventPriority::Normal, false)?;
        context.schedule_repeating_task(1, 1, on_tick);
        c::mark_running(VERSION);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::mark_stopped();
        if let Ok(mut guard) = STATE.lock()
            && let Some(st) = guard.as_mut()
        {
            c::save_json(FILE, &st.workers);
        }
        Ok(())
    }

    /// `%jobs_<job>_level%`, `%jobs_count%` for the Placeholders plugin; `{"kind":"jobs",
    /// "player":<uuid hex>}` lists a player's jobs and levels.
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let guard = try_lock().ok_or("busy")?;
        let st = guard.as_ref().ok_or("not loaded")?;
        let text = |k: &str| v.get(k).and_then(serde_json::Value::as_str).unwrap_or_default();
        let key = |k: &str| text(k).replace('-', "").to_ascii_lowercase();
        let answer = match text("kind") {
            "placeholder" => {
                let worker = st.workers.get(&key("uuid"));
                let params = text("params");
                let value = match params {
                    "count" => Some(worker.map_or(0, |w| w.jobs.len()).to_string()),
                    "earned" => Some(money_text(worker.map_or(0.0, |w| w.earned))),
                    _ => params.strip_suffix("_level").map(|job| worker.and_then(|w| w.jobs.get(job)).map_or(0, |p| p.level).to_string()),
                };
                serde_json::json!({ "value": value })
            }
            "jobs" => {
                let worker = st.workers.get(&key("player"));
                serde_json::json!({ "ok": true, "jobs": worker.map(|w| w.jobs.iter().map(|(id, p)| serde_json::json!({ "id": id, "level": p.level })).collect::<Vec<_>>()).unwrap_or_default() })
            }
            other => return Err(format!("unknown kind {other}")),
        };
        serde_json::to_vec(&answer).map_err(|e| e.to_string())
    }
}

register_plugin!(Jobs);

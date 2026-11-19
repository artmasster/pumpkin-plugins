//! Pumpkin WASM glue for Skills: experience from deeds, perks, the window and the top lists.
//!
//! Deeds come through `mcsv_common::deeds`. A double drop puts the second drop straight into the
//! inventory (Pumpkin 0.2.2 cannot drop items into the world from a plugin); with no room it is
//! not given. More damage changes the hit in a blocking handler of `EntityDamageByEntityEvent`.
//! Blocks placed by players are remembered in memory only; after a restart they teach once more.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use mcsv_common as c;
use c::deeds::{self, Deed, Kills};
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::GameMode;
use pumpkin_plugin_api::events::{EntityDamageByEntityEvent, EventHandler, EventPriority, FromIntoEvent, InventoryClickEvent, InventoryCloseEvent};
use pumpkin_plugin_api::gui::Gui;
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::screens_wit::Screen;
use pumpkin_plugin_api::wit::pumpkin::plugin::sounds::SoundCategory;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::defs::{self, Config, Learner, NS, Note, Perk, Skill};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&bSkills&8]&r ";
const FILE: &str = "players.json";

struct State {
    cfg: Config,
    list: Vec<Skill>,
    notes: Vec<Note>,
    learners: HashMap<String, Learner>,
    placed: HashSet<deeds::Spot>,
    placed_order: VecDeque<deeds::Spot>,
    kills: Kills,
    wins: HashSet<String>,
    random: u64,
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

fn with_state(f: impl FnOnce(&mut State)) {
    if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
        f(st);
    }
}

fn msg(th: bool, thai: &str, english: &str) -> String {
    format!("{P}{}", if th { thai } else { english })
}

fn random(st: &mut State) -> f64 {
    let mut x = st.random;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    st.random = x;
    (x >> 11) as f64 / (1u64 << 53) as f64
}

fn load() -> (Config, Vec<Skill>, Vec<Note>) {
    let cfg = c::load_config::<Config>("config.toml").cleaned();
    let mut notes = Vec::new();
    let first = !c::data_path("skills").exists();
    let _ = std::fs::create_dir_all(c::data_path("skills"));
    if first {
        let _ = std::fs::write(c::data_path("skills/example.toml"), defs::EXAMPLE);
    }
    let mut names: Vec<String> = std::fs::read_dir(c::data_path("skills")).map(|d| d.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.ends_with(".toml")).collect()).unwrap_or_default();
    names.sort();
    let mut all = Vec::new();
    for name in names {
        let file = format!("skills/{name}");
        match std::fs::read_to_string(c::data_path(&file)) {
            Ok(text) => all.extend(defs::parse_skills(&file, &text, &mut notes)),
            Err(_) => notes.push(Note::new(&file, "อ่านไฟล์ไม่ได้", "cannot be read")),
        }
    }
    let list = defs::merge_skills(all, &mut notes);
    (cfg, list, notes)
}

fn learn(st: &mut State, p: &Player, deed: &Deed, spot: Option<deeds::Spot>) {
    if matches!(p.get_gamemode(), GameMode::Creative | GameMode::Spectator) {
        return;
    }
    match (deed, spot) {
        (Deed::Break(_), Some(spot)) if st.placed.remove(&spot) => return,
        (Deed::Place(_), Some(spot)) => {
            if st.cfg.placed_memory > 0 && st.placed.insert(spot.clone()) {
                st.placed_order.push_back(spot);
                while st.placed_order.len() > st.cfg.placed_memory as usize {
                    if let Some(old) = st.placed_order.pop_front() {
                        st.placed.remove(&old);
                    }
                }
            }
        }
        _ => {}
    }
    let th = st.cfg.thai();
    let me = c::game::key(p);
    let n = f64::from(deed.count());
    let mut ups = Vec::new();
    let mut doubles = Vec::new();
    for skill in st.list.clone() {
        let Some(xp) = skill.xp_for(deed.kind(), deed.target()) else { continue };
        let learner = st.learners.entry(me.clone()).or_default();
        learner.name = p.get_name();
        let progress = learner.skills.entry(skill.id.clone()).or_default();
        if defs::add_xp(&st.cfg, progress, xp * n, skill.max_level) > 0 {
            ups.push((skill.name.clone(), progress.level));
        }
        let level = progress.level;
        st.dirty = true;
        if let (Perk::DoubleDrop, Deed::Break(block)) = (skill.perk, deed)
            && random(st) < skill.perk_at(level)
        {
            doubles.push(skill.drop_of(block));
        }
    }
    for (name, level) in ups {
        p.show_title(c::color(&format!("&b{name}")));
        c::tell(p, &msg(th, &format!("&b{name} &fเลเวล {level}"), &format!("&b{name} &fis level {level}")));
        p.play_custom_sound("minecraft:entity.player.levelup", SoundCategory::Players, 0.6, 1.4);
    }
    for (item, count) in doubles.into_iter().take(1) {
        if c::game::give_game(p, &item, count) < count {
            c::action_bar(p, &msg(th, "&aได้ของเพิ่มอีกชิ้น!", "&aDouble drop!"));
        }
    }
}

#[derive(Clone, Copy)]
struct Deeds;

impl deeds::Sink for Deeds {
    fn deed(&self, _server: &Server, p: &Player, deed: Deed, spot: Option<deeds::Spot>) {
        with_state(|st| learn(st, p, &deed, spot));
    }

    fn with_kills(&self, f: &mut dyn FnMut(&mut Kills, u64)) {
        with_state(|st| f(&mut st.kills, st.tick));
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

/// More damage for players whose damage skills are higher.
struct OnHit;
impl EventHandler<EntityDamageByEntityEvent> for OnHit {
    fn handle(&self, server: Server, mut e: Data<EntityDamageByEntityEvent>) -> Data<EntityDamageByEntityEvent> {
        if e.cancelled {
            return e;
        }
        let Some(guard) = try_lock() else { return e };
        let Some(st) = guard.as_ref() else { return e };
        if !st.list.iter().any(|s| s.perk == Perk::Damage) {
            return e;
        }
        let Some(p) = server.get_all_players().into_iter().find(|p| p.as_entity().get_id() as i32 == e.damager_id) else { return e };
        let Some(learner) = st.learners.get(&c::game::key(&p)) else { return e };
        let bonus: f64 = st.list.iter().filter(|s| s.perk == Perk::Damage).map(|s| s.perk_at(learner.skills.get(&s.id).map_or(0, |p| p.level))).sum();
        if bonus > 0.0 {
            e.damage *= (1.0 + bonus) as f32;
        }
        e
    }
}

// ---- the window --------------------------------------------------------------------

fn open(st: &mut State, p: &Player) {
    let th = st.cfg.thai();
    let me = c::game::key(p);
    let learner = st.learners.get(&me).cloned().unwrap_or_default();
    let gui = Gui::new(Screen::Generic9x3, c::color(&format!("{} &8| {} {}", if th { "&8ทักษะ" } else { "&8Skills" }, if th { "พลังรวม" } else { "power" }, learner.power())));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    let inv = gui.get_inventory();
    for (i, s) in st.list.iter().take(27).enumerate() {
        let p = learner.skills.get(&s.id).cloned().unwrap_or_default();
        let need = st.cfg.need(p.level);
        let filled = if p.level >= s.max_level { 20 } else { ((p.xp / need) * 20.0).clamp(0.0, 20.0) as usize };
        let bar = format!("&a{}&8{}", "|".repeat(filled), "|".repeat(20 - filled));
        let mut lore = vec![format!("&f{} {}/{}", if th { "เลเวล" } else { "Level" }, p.level, s.max_level), format!("{bar} &7{:.0}/{:.0}", p.xp, need)];
        match s.perk {
            Perk::DoubleDrop => lore.push(format!("&7{} {:.1}%", if th { "โอกาสดรอปสองเท่า" } else { "Double drop chance" }, s.perk_at(p.level) * 100.0)),
            Perk::Damage => lore.push(format!("&7{} +{:.1}%", if th { "ดาเมจ" } else { "Damage" }, s.perk_at(p.level) * 100.0)),
            Perk::None => {}
        }
        inv.set_item(i as u32, Some(c::game::button(&s.icon, &s.name, &lore)));
    }
    st.wins.insert(me);
    p.open_gui(gui);
}

struct OnClick;
impl EventHandler<InventoryClickEvent> for OnClick {
    fn handle(&self, _server: Server, mut e: Data<InventoryClickEvent>) -> Data<InventoryClickEvent> {
        if e.window_type.is_some()
            && let Some(st) = try_lock().as_ref().and_then(|g| g.as_ref())
            && st.wins.contains(&c::game::key(&e.player))
        {
            e.cancelled = true;
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

fn on_tick(_server: Server) {
    with_state(|st| {
        st.tick += 1;
        if st.dirty && st.tick % 1200 == 0 && c::save_json(FILE, &st.learners) {
            st.dirty = false;
        }
    });
}

// ---- commands ----------------------------------------------------------------------

fn run(st: &mut State, sender: &CommandSender, server: &Server, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    let me = c::sender_player(sender);
    let admin = c::has_perm(sender, &format!("{NS}:admin"));
    match w.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        None => {
            let Some(p) = me else { return vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")] };
            open(st, &p);
            Vec::new()
        }
        Some("stats") => {
            let key = match (w.get(1), &me) {
                (Some(name), _) => st.learners.iter().find(|(_, l)| l.name.eq_ignore_ascii_case(name)).map(|(k, _)| k.clone()),
                (None, Some(p)) => Some(c::game::key(p)),
                _ => None,
            };
            let Some(l) = key.and_then(|k| st.learners.get(&k)) else { return vec![msg(th, "&7ยังไม่มีทักษะ", "&7No skills yet")] };
            let mut out = vec![msg(th, &format!("&f{} &7พลังรวม {}", l.name, l.power()), &format!("&f{} &7power {}", l.name, l.power()))];
            out.extend(st.list.iter().map(|s| format!("&8- {} &7{}", s.name, l.skills.get(&s.id).map_or(0, |p| p.level))));
            out
        }
        Some("top") => {
            let skill = w.get(1).and_then(|id| st.list.iter().find(|s| s.id == *id));
            let mut rows: Vec<(String, u32)> = st.learners.values().map(|l| (l.name.clone(), skill.map_or_else(|| l.power(), |s| l.skills.get(&s.id).map_or(0, |p| p.level)))).collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1));
            let title = skill.map_or_else(|| if th { "พลังรวม".to_owned() } else { "power".to_owned() }, |s| s.name.clone());
            let mut out = vec![msg(th, &format!("&fอันดับ {title}"), &format!("&fTop {title}"))];
            out.extend(rows.iter().take(10).enumerate().map(|(i, (n, l))| format!("&e{}. &f{n} &7{l}", i + 1)));
            out
        }
        Some("reload") if admin => {
            let (cfg, list, notes) = load();
            (st.cfg, st.list, st.notes) = (cfg, list, notes);
            let mut out = vec![msg(st.cfg.thai(), &format!("&aอ่านใหม่แล้ว: {} ทักษะ", st.list.len()), &format!("&aRead again: {} skill(s)", st.list.len()))];
            out.extend(st.notes.iter().take(20).map(|n| format!("&6! &7{}", n.text(th))));
            out
        }
        Some("setlevel") if admin => {
            let (Some(name), Some(id), Some(level)) = (w.get(1), w.get(2), w.get(3).and_then(|l| l.parse::<u32>().ok())) else { return vec![msg(th, "&f/skills setlevel <ผู้เล่น> <ทักษะ> <เลเวล>", "&f/skills setlevel <player> <skill> <level>")] };
            let Some(max) = st.list.iter().find(|s| s.id == *id).map(|s| s.max_level) else { return vec![msg(th, "&cไม่มีทักษะนี้", "&cNo such skill")] };
            let key = server.get_all_players().into_iter().find(|p| p.get_name().eq_ignore_ascii_case(name)).map(|p| c::game::key(&p)).or_else(|| st.learners.iter().find(|(_, l)| l.name.eq_ignore_ascii_case(name)).map(|(k, _)| k.clone()));
            let Some(key) = key else { return vec![msg(th, "&cไม่รู้จักผู้เล่นนี้", "&cUnknown player")] };
            let l = st.learners.entry(key).or_default();
            if l.name.is_empty() {
                l.name = name.clone();
            }
            l.skills.insert(id.clone(), defs::Progress { level: level.min(max), xp: 0.0 });
            st.dirty = true;
            vec![msg(th, "&aตั้งเลเวลแล้ว", "&aLevel set")]
        }
        Some("version") => vec![format!("Skills {VERSION}")],
        _ => {
            let mut out = vec![msg(th, "&f/skills &7เปิดหน้าต่าง &7| &fstats [ผู้เล่น] &7| &ftop [ทักษะ]", "&f/skills &7open &7| &fstats [player] &7| &ftop [skill]")];
            if admin {
                out.push(msg(th, "&f/skills reload &7| &fsetlevel <ผู้เล่น> <ทักษะ> <เลเวล> &7| &fversion", "&f/skills reload &7| &fsetlevel <player> <skill> <level> &7| &fversion"));
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

struct Skills;

impl Plugin for Skills {
    fn new() -> Self {
        Skills
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Skills that grow with what players do, with perks".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:use"), description: "See skills".into(), default: PermissionDefault::Allow, children: vec![] });
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Manage skills".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let (cfg, list, notes) = load();
        let learners: HashMap<String, Learner> = c::load_json(FILE).unwrap_or_default();
        c::log_info(&format!("{} skill(s), {} player(s){}", list.len(), learners.len(), if notes.is_empty() { String::new() } else { format!(", {} thing(s) to look at (/skills reload)", notes.len()) }));
        let random = c::now_secs().wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg, list, notes, learners, placed: HashSet::new(), placed_order: VecDeque::new(), kills: Kills::default(), wins: HashSet::new(), random, tick: 0, dirty: false });
        let cmd = Command::new(&[NS.to_owned(), "skill".to_owned()], "Skills").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:use"));
        deeds::listen(&context, Deeds)?;
        context.register_event_handler(OnHit, EventPriority::Normal, true)?;
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
            c::save_json(FILE, &st.learners);
        }
        Ok(())
    }

    /// `%skills_<skill>_level%`, `%skills_power%` for the Placeholders plugin.
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let guard = try_lock().ok_or("busy")?;
        let st = guard.as_ref().ok_or("not loaded")?;
        if v.get("kind").and_then(serde_json::Value::as_str) != Some("placeholder") {
            return Err("unknown kind".into());
        }
        let key = v.get("uuid").and_then(serde_json::Value::as_str).unwrap_or_default().replace('-', "").to_ascii_lowercase();
        let l = st.learners.get(&key);
        let params = v.get("params").and_then(serde_json::Value::as_str).unwrap_or_default();
        let value = match params {
            "power" => Some(l.map_or(0, Learner::power).to_string()),
            _ => params.strip_suffix("_level").map(|s| l.and_then(|l| l.skills.get(s)).map_or(0, |p| p.level).to_string()),
        };
        serde_json::to_vec(&serde_json::json!({ "value": value })).map_err(|e| e.to_string())
    }
}

register_plugin!(Skills);

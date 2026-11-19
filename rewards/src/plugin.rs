//! Pumpkin WASM glue for Rewards: the daily window, claiming, and counting time played.
//!
//! Time played counts each minute for players online. Rewards that give items need room in the
//! inventory; a daily reward that finds none is not claimed, a playtime reward waits for the next
//! minute. `players.json` keeps rows and time played.
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::data_components::DataComponent;
use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, InventoryClickEvent, InventoryCloseEvent, PlayerJoinEvent};
use pumpkin_plugin_api::gui::Gui;
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::screens_wit::Screen;
use pumpkin_plugin_api::text::TextComponent;
use pumpkin_plugin_api::wit::pumpkin::plugin::server::CommandSender as ServerSender;
use pumpkin_plugin_api::wit::pumpkin::plugin::sounds::SoundCategory;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::defs::{self, Config, NS, Note, Player as Record, Reward, Rules, Today};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&6Rewards&8]&r ";
const FILE: &str = "players.json";

struct State {
    cfg: Config,
    rules: Rules,
    notes: Vec<Note>,
    players: HashMap<String, Record>,
    wins: HashSet<String>,
    claims: Vec<String>,
    joins: Vec<String>,
    after: Vec<String>,
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

fn load() -> (Config, Rules, Vec<Note>) {
    let cfg = c::load_config::<Config>("config.toml").cleaned();
    let mut notes = Vec::new();
    let path = c::data_path("rewards.toml");
    if !path.exists() {
        let _ = std::fs::write(&path, defs::EXAMPLE);
    }
    let rules = match std::fs::read_to_string(&path) {
        Ok(text) => defs::parse(&text, &mut notes),
        Err(_) => {
            notes.push(Note::new("rewards.toml", "อ่านไฟล์ไม่ได้", "cannot be read"));
            Rules::default()
        }
    };
    (cfg, rules, notes)
}

fn reward_name(r: &Reward, th: bool) -> String {
    if let Some(t) = &r.text {
        return t.clone();
    }
    let mut parts = Vec::new();
    if let Some(i) = &r.item {
        parts.push(format!("{} x{}", i.trim_start_matches("minecraft:"), r.count));
    }
    if r.money > 0.0 {
        parts.push(format!("{} {}", r.money, if th { "เหรียญ" } else { "coins" }));
    }
    if parts.is_empty() {
        parts.push(if th { "รางวัลพิเศษ".into() } else { "a surprise".into() });
    }
    parts.join(" + ")
}

/// Give a reward. False when it gives items and there is no room (nothing is given then).
fn give(st: &mut State, p: &Player, r: &Reward) -> bool {
    if r.item.is_some() && c::game::empty_slots(p) == 0 {
        return false;
    }
    let name = p.get_name();
    if let Some(item) = &r.item
        && !c::game::give_item(p, item, r.count)
    {
        c::log_info(&format!("could not give {item} x{} to {name} (CustomItems did not answer)", r.count));
    }
    if r.money > 0.0 && c::game::money::deposit(&name, r.money).is_none() {
        c::log_info(&format!("could not pay {} to {name} (Basics did not answer)", r.money));
    }
    st.after.extend(r.commands.iter().map(|cmd| cmd.replace("{player}", &name)));
    true
}

fn record<'a>(st: &'a mut State, p: &Player) -> &'a mut Record {
    let r = st.players.entry(c::game::key(p)).or_default();
    r.name = p.get_name();
    r
}

fn claim(st: &mut State, p: &Player) -> String {
    let th = st.cfg.thai();
    if st.rules.days.is_empty() {
        return msg(th, "&7ยังไม่มีรางวัลรายวัน", "&7No daily rewards set up");
    }
    let now = c::now_secs();
    let days = st.rules.days.len();
    let rec = record(st, p).clone();
    let Today::Open(n) = defs::today(&st.cfg, &rec, days, now) else {
        return msg(th, &format!("&7รับของวันนี้แล้ว รับได้อีกในอีก {} ชม.", st.cfg.until_tomorrow(now) / 3600 + 1), &format!("&7Claimed today; the next one in {}h", st.cfg.until_tomorrow(now) / 3600 + 1));
    };
    let r = st.rules.days[(n as usize - 1).min(days - 1)].clone();
    if !give(st, p, &r) {
        return msg(th, "&cกระเป๋าเต็ม ต้องมีช่องว่างก่อน", "&cYour inventory is full: make room first");
    }
    let mut rec = rec;
    defs::claim(&st.cfg, &mut rec, days, now);
    st.players.insert(c::game::key(p), rec);
    st.dirty = true;
    p.play_custom_sound("minecraft:entity.player.levelup", SoundCategory::Players, 0.6, 1.3);
    msg(th, &format!("&aรางวัลวันที่ {n}: &f{}", reward_name(&r, th)), &format!("&aDay {n}: &f{}", reward_name(&r, th)))
}

fn open(st: &mut State, p: &Player) {
    let th = st.cfg.thai();
    let now = c::now_secs();
    let days = st.rules.days.len();
    let rec = record(st, p).clone();
    let state = defs::today(&st.cfg, &rec, days, now);
    let rows = if days <= 9 { Screen::Generic9x1 } else if days <= 18 { Screen::Generic9x2 } else { Screen::Generic9x3 };
    let gui = Gui::new(rows, c::color(if th { "&8รางวัลรายวัน" } else { "&8Daily rewards" }));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    let inv = gui.get_inventory();
    // Days of the row already claimed: before today's
    let (today_n, claimed_today) = match state {
        Today::Open(n) => (n as usize, false),
        Today::Claimed => (((rec.streak as usize).saturating_sub(1)) % days.max(1) + 1, true),
    };
    for (i, r) in st.rules.days.iter().enumerate() {
        let n = i + 1;
        let (material, line) = if n < today_n || (n == today_n && claimed_today) {
            ("minecraft:lime_stained_glass_pane", if th { "&aรับแล้ว" } else { "&aClaimed" })
        } else if n == today_n {
            ("minecraft:chest", if th { "&eวันนี้ &7| คลิกเพื่อรับ" } else { "&eToday &7| click to claim" })
        } else {
            ("minecraft:gray_stained_glass_pane", if th { "&8ยังไม่ถึง" } else { "&8Not yet" })
        };
        let stack = c::game::button(material, &format!("&f{} {n}", if th { "วันที่" } else { "Day" }), &[format!("&7{}", reward_name(r, th)), line.to_owned()]);
        if n == today_n && !claimed_today {
            stack.set_component(DataComponent::EnchantmentGlintOverride, &[1]);
        }
        inv.set_item(i as u32, Some(stack));
    }
    st.wins.insert(c::game::key(p));
    p.open_gui(gui);
}

fn count_minute(st: &mut State, server: &Server) {
    let th = st.cfg.thai();
    for p in server.get_all_players() {
        let rec = record(st, &p);
        rec.played_secs += 60;
        let rec = rec.clone();
        let due = defs::due(&st.rules, &rec);
        st.dirty = true;
        for (i, times) in due {
            let t = st.rules.playtime[i].clone();
            let mut given = 0;
            for _ in 0..times.min(10) {
                if !give(st, &p, &t.reward) {
                    break;
                }
                given += 1;
            }
            if given > 0 {
                *record(st, &p).given.entry(t.id.clone()).or_default() += given;
                c::tell(&p, &msg(th, &format!("&aรางวัลเวลาเล่น: &f{}", reward_name(&t.reward, th)), &format!("&aPlaytime reward: &f{}", reward_name(&t.reward, th))));
            } else {
                c::action_bar(&p, &msg(th, "&cมีรางวัลเวลาเล่นรออยู่ แต่กระเป๋าเต็ม", "&cA playtime reward is waiting: your inventory is full"));
            }
        }
    }
}

fn on_tick(server: Server) {
    let jobs = {
        let Some(mut guard) = try_lock() else { return };
        let Some(st) = guard.as_mut() else { return };
        st.tick += 1;
        let th = st.cfg.thai();
        for player in std::mem::take(&mut st.claims) {
            if let Some(p) = c::game::online(&server, &player) {
                let line = claim(st, &p);
                c::tell(&p, &line);
                if st.wins.contains(&player) {
                    open(st, &p);
                }
            }
        }
        for player in std::mem::take(&mut st.joins) {
            let Some(p) = c::game::online(&server, &player) else { continue };
            let rec = record(st, &p).clone();
            if !st.rules.days.is_empty() && matches!(defs::today(&st.cfg, &rec, st.rules.days.len(), c::now_secs()), Today::Open(_)) {
                let button = c::color(if th { "&a[รับรางวัลวันนี้]" } else { "&a[Claim today's reward]" }).click_run_command("/daily claim");
                p.send_system_message(TextComponent::text("").add_child(c::color(&msg(th, "&fรางวัลรายวันรออยู่ ", "&fA daily reward is waiting "))).add_child(button), false);
            }
        }
        if st.tick % 1200 == 0 {
            count_minute(st, &server);
        }
        if st.dirty && st.tick % 1200 == 600 && c::save_json(FILE, &st.players) {
            st.dirty = false;
        }
        std::mem::take(&mut st.after)
    };
    for cmd in jobs {
        server.execute_command(&cmd, ServerSender::Console);
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

struct OnClick;
impl EventHandler<InventoryClickEvent> for OnClick {
    fn handle(&self, _server: Server, mut e: Data<InventoryClickEvent>) -> Data<InventoryClickEvent> {
        if e.window_type.is_none() {
            return e;
        }
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let me = c::game::key(&e.player);
            if st.wins.contains(&me) {
                e.cancelled = true;
                if (0..defs::MAX_DAYS as i16).contains(&e.raw_slot) && !st.claims.contains(&me) {
                    st.claims.push(me);
                }
            }
        }
        e
    }
}

struct OnClose;
impl EventHandler<InventoryCloseEvent> for OnClose {
    fn handle(&self, _server: Server, e: Data<InventoryCloseEvent>) -> Data<InventoryCloseEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            st.wins.remove(&c::game::key(&e.player));
        }
        e
    }
}

struct OnJoin;
impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _server: Server, e: Data<PlayerJoinEvent>) -> Data<PlayerJoinEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut())
            && st.cfg.remind_on_join
        {
            st.joins.push(c::game::key(&e.player));
        }
        e
    }
}

fn run(st: &mut State, sender: &CommandSender, w: &[String]) -> Vec<String> {
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
        Some("claim") => {
            let Some(p) = me else { return in_game() };
            vec![claim(st, &p)]
        }
        Some("playtime") => {
            let Some(p) = me else { return in_game() };
            let rec = record(st, &p).clone();
            let mut out = vec![msg(th, &format!("&fเวลาเล่น {}", c::hms(rec.played_secs)), &format!("&fTime played {}", c::hms(rec.played_secs)))];
            for t in &st.rules.playtime {
                let have = rec.given.get(&t.id).copied().unwrap_or(0);
                let next = if t.repeat { (have + 1) * t.minutes } else { t.minutes };
                let done = !t.repeat && have > 0;
                out.push(format!("&8- &7{} &8| &f{}", reward_name(&t.reward, th), if done { if th { "ได้แล้ว".to_owned() } else { "got it".to_owned() } } else { format!("{} {}", if th { "ที่" } else { "at" }, c::hms(next * 60)) }));
            }
            out
        }
        Some("reload") if admin => {
            let (cfg, rules, notes) = load();
            (st.cfg, st.rules, st.notes) = (cfg, rules, notes);
            let mut out = vec![msg(st.cfg.thai(), &format!("&aอ่านใหม่แล้ว: {} วัน, รางวัลเวลาเล่น {} อย่าง", st.rules.days.len(), st.rules.playtime.len()), &format!("&aRead again: {} day(s), {} playtime reward(s)", st.rules.days.len(), st.rules.playtime.len()))];
            out.extend(st.notes.iter().take(20).map(|n| format!("&6! &7{}", n.text(th))));
            out
        }
        Some("reset") if admin => {
            let Some(name) = w.get(1) else { return vec![msg(th, "&f/daily reset <ผู้เล่น>", "&f/daily reset <player>")] };
            let found = st.players.values_mut().find(|r| r.name.eq_ignore_ascii_case(name));
            match found {
                Some(r) => {
                    r.last_day = None;
                    r.streak = 0;
                    st.dirty = true;
                    vec![msg(th, "&aล้างรางวัลรายวันแล้ว", "&aDaily rewards reset")]
                }
                None => vec![msg(th, "&cไม่รู้จักผู้เล่นนี้", "&cUnknown player")],
            }
        }
        Some("version") => vec![format!("Rewards {VERSION}")],
        _ => {
            let mut out = vec![msg(th, "&f/daily &7เปิดหน้าต่าง &7| &fclaim &7| &fplaytime", "&f/daily &7open &7| &fclaim &7| &fplaytime")];
            if admin {
                out.push(msg(th, "&f/daily reload &7| &freset <ผู้เล่น> &7| &fversion", "&f/daily reload &7| &freset <player> &7| &fversion"));
            }
            out
        }
    }
}

struct Cmd;
impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, _server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let lines = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            let Some(st) = guard.as_mut() else { return Ok(0) };
            run(st, &sender, &w)
        };
        for line in lines {
            c::reply(&sender, &line);
        }
        Ok(1)
    }
}

struct Rewards;

impl Plugin for Rewards {
    fn new() -> Self {
        Rewards
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Daily rewards in a row and rewards for time played".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:use"), description: "Claim rewards".into(), default: PermissionDefault::Allow, children: vec![] });
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Manage rewards".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let (cfg, rules, notes) = load();
        let players: HashMap<String, Record> = c::load_json(FILE).unwrap_or_default();
        c::log_info(&format!("{} day(s), {} playtime reward(s){}", rules.days.len(), rules.playtime.len(), if notes.is_empty() { String::new() } else { format!(", {} thing(s) to look at (/daily reload)", notes.len()) }));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg, rules, notes, players, wins: HashSet::new(), claims: Vec::new(), joins: Vec::new(), after: Vec::new(), tick: 0, dirty: false });
        let cmd = Command::new(&["daily".to_owned(), NS.to_owned()], "Rewards").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:use"));
        context.register_event_handler(OnClick, EventPriority::Normal, true)?;
        context.register_event_handler(OnClose, EventPriority::Normal, false)?;
        context.register_event_handler(OnJoin, EventPriority::Normal, false)?;
        context.schedule_repeating_task(1, 1, on_tick);
        c::mark_running(VERSION);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::mark_stopped();
        if let Ok(mut guard) = STATE.lock()
            && let Some(st) = guard.as_mut()
        {
            c::save_json(FILE, &st.players);
        }
        Ok(())
    }

    /// `%rewards_streak%`, `%rewards_playtime%` (hours) for the Placeholders plugin.
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let guard = try_lock().ok_or("busy")?;
        let st = guard.as_ref().ok_or("not loaded")?;
        if v.get("kind").and_then(serde_json::Value::as_str) != Some("placeholder") {
            return Err("unknown kind".into());
        }
        let key = v.get("uuid").and_then(serde_json::Value::as_str).unwrap_or_default().replace('-', "").to_ascii_lowercase();
        let rec = st.players.get(&key);
        let value = match v.get("params").and_then(serde_json::Value::as_str).unwrap_or_default() {
            "streak" => Some(rec.map_or(0, |r| r.streak).to_string()),
            "playtime" => Some(format!("{:.1}", rec.map_or(0, |r| r.played_secs) as f64 / 3600.0)),
            _ => None,
        };
        serde_json::to_vec(&serde_json::json!({ "value": value })).map_err(|e| e.to_string())
    }
}

register_plugin!(Rewards);

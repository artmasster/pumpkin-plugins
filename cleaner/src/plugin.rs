//! Pumpkin WASM glue for Cleaner.
use std::collections::HashMap;
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::world::Entity;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::plan::{self, Config, Seen};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NS: &str = "cleaner";
const PREFIX: &str = "&8[&bCleaner&8]&r ";
/// Entities one pass looks at (each costs a few calls into the server).
const CAP: usize = 60_000;

struct State {
    cfg: Config,
    /// Epoch second of the next automatic clear (0 = none).
    next_at: u64,
    last_left: u64,
    secs: u64,
    removed_total: u64,
    /// Removed by `mob-cap`.
    capped: u64,
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

fn announce(server: &Server, st: &State, line: &str) {
    if st.cfg.broadcast {
        for p in server.get_all_players() {
            c::tell(&p, &format!("{PREFIX}{line}"));
        }
    }
    c::log_info(&c::plain(line));
}

/// Everything in every world that is not a player, with the handle to remove it.
fn look(server: &Server) -> (Vec<Seen>, Vec<Entity>, Vec<String>, bool) {
    let mut seen = Vec::new();
    let mut handles = Vec::new();
    let mut worlds = Vec::new();
    let mut cut = false;
    for (wi, w) in server.get_all_worlds().into_iter().enumerate() {
        worlds.push(w.get_dimension().trim_start_matches("minecraft:").to_owned());
        for e in w.get_entities() {
            if seen.len() >= CAP {
                cut = true;
                break;
            }
            let kind = plan::snake(&format!("{:?}", e.get_type()));
            if kind == "player" {
                continue;
            }
            let pos = e.get_position();
            seen.push(Seen {
                kind,
                named: e.get_custom_name().is_some(),
                age_secs: (e.get_ticks_lived().max(0) / 20) as u64,
                world: wi,
                chunk: ((pos.0.floor() as i32) >> 4, (pos.2.floor() as i32) >> 4),
            });
            handles.push(e);
        }
    }
    (seen, handles, worlds, cut)
}

fn remove(handles: &[Entity], picks: &[usize]) -> usize {
    for i in picks {
        if let Some(e) = handles.get(*i) {
            e.remove();
        }
    }
    picks.len()
}

fn clear(st: &mut State, server: &Server) -> usize {
    let (seen, handles, _, _) = look(server);
    let n = remove(&handles, &plan::pick_clear(&st.cfg, &seen));
    st.removed_total += n as u64;
    if st.cfg.interval > 0 {
        st.next_at = c::now_secs() + st.cfg.interval;
        st.last_left = st.cfg.interval;
    }
    n
}

fn cleared_line(th: bool, n: usize) -> String {
    format!("&a{} &f{n} &a{}", tr(th, "ล้างของบนพื้นแล้ว", "Cleared"), tr(th, "ชิ้น", "entities on the ground"))
}

/// Once a second.
fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.secs += 1;
    let th = st.cfg.thai();
    let now = c::now_secs();
    if st.cfg.interval > 0 && st.next_at > 0 {
        let left = st.next_at.saturating_sub(now);
        if left == 0 {
            let n = clear(st, &server);
            announce(&server, st, &cleared_line(th, n));
        } else {
            if let Some(w) = plan::due_warning(&st.cfg, st.last_left, left) {
                announce(&server, st, &format!("&e{} &f{w} &e{}", tr(th, "จะล้างของที่ตกอยู่บนพื้นในอีก", "Items on the ground are cleared in"), tr(th, "วินาที", "seconds")));
            }
            st.last_left = left;
        }
    }
    if st.cfg.mob_cap > 0 && st.secs % 10 == 0 {
        let (seen, handles, _, _) = look(&server);
        let n = remove(&handles, &plan::pick_over_cap(&st.cfg, &seen));
        st.removed_total += n as u64;
        st.capped += n as u64;
    }
    // A full look at every entity: once a minute is plenty
    if st.cfg.chunk_limit > 0 && st.secs % 60 == 0 {
        let (seen, handles, _, _) = look(&server);
        let n = remove(&handles, &plan::pick_over_limit(&st.cfg, &seen));
        if n > 0 {
            st.removed_total += n as u64;
            c::log_info(&format!("chunk limit: removed {n} mobs"));
        }
    }
}

fn command(st: &mut State, server: &Server, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    match w.first().map_or("", String::as_str) {
        "now" | "clear" => {
            let n = clear(st, server);
            announce(server, st, &cleared_line(th, n));
            Vec::new()
        }
        "killmobs" => {
            let (seen, handles, _, _) = look(server);
            let n = remove(&handles, &plan::pick_hostile(&seen));
            st.removed_total += n as u64;
            vec![format!("&a{} &f{n} &a{}", tr(th, "ลบมอนสเตอร์แล้ว", "Removed"), tr(th, "ตัว", "hostile mobs"))]
        }
        "check" => {
            let (seen, _, worlds, cut) = look(server);
            let mut out = Vec::new();
            for (wi, name) in worlds.iter().enumerate() {
                let here: Vec<&Seen> = seen.iter().filter(|e| e.world == wi).collect();
                let items = here.iter().filter(|e| e.kind == "item").count();
                let orbs = here.iter().filter(|e| e.kind == "experience_orb").count();
                let would = here.iter().filter(|e| st.cfg.remove.contains(&e.kind) && e.age_secs >= st.cfg.min_age && !(st.cfg.keep_named && e.named)).count();
                let mut chunks: HashMap<(i32, i32), u32> = HashMap::new();
                for e in &here {
                    *chunks.entry(e.chunk).or_default() += 1;
                }
                let top = chunks.iter().max_by_key(|(k, n)| (**n, std::cmp::Reverse(**k)));
                let mut line = format!(
                    "&7{name}: entity &f{} &7({} &f{items}&7, {} &f{orbs}&7) · {} &f{would}",
                    here.len(), tr(th, "ไอเท็มบนพื้น", "items"), tr(th, "ลูกแก้ว exp", "xp orbs"), tr(th, "ถ้าล้างตอนนี้จะลบ", "a clear would remove")
                );
                if let Some(((cx, cz), n)) = top {
                    line.push_str(&format!(" &7· {} &f{},{} &7({n})", tr(th, "แน่นสุดที่", "densest at"), cx * 16 + 8, cz * 16 + 8));
                }
                out.push(line);
            }
            if cut {
                out.push(format!("&7{}", tr(th, "นับถึงเพดานแล้วหยุด", "stopped counting at the limit")));
            }
            out
        }
        "next" | "" if w.first().is_some() || st.cfg.interval > 0 => {
            let mut out = vec![if st.cfg.interval == 0 {
                format!("&7{}", tr(th, "ปิดการล้างอัตโนมัติอยู่ (interval = 0)", "Automatic clearing is off (interval = 0)"))
            } else {
                format!(
                    "&7{} &f{} &7{} · {} &f{}",
                    tr(th, "ล้างครั้งถัดไปในอีก", "Next clear in"), st.next_at.saturating_sub(c::now_secs()), tr(th, "วินาที", "seconds"),
                    tr(th, "ลบไปแล้วตั้งแต่เปิดเซิร์ฟ", "removed since start"), st.removed_total
                )
            }];
            if st.cfg.mob_cap > 0 {
                out.push(format!(
                    "&7{} &f{} &7· {} &f{}",
                    tr(th, "เพดานสัตว์ป่าและมอนสเตอร์ต่อโลก", "wild creature cap per world"), st.cfg.mob_cap,
                    tr(th, "ลบส่วนเกินไปแล้ว", "removed over the cap"), st.capped
                ));
            }
            out
        }
        "reload" => {
            st.cfg = load();
            st.next_at = if st.cfg.interval > 0 { c::now_secs() + st.cfg.interval } else { 0 };
            st.last_left = st.cfg.interval;
            vec![format!("&a{}", tr(st.cfg.thai(), "โหลดไฟล์ตั้งค่าใหม่แล้ว", "Configuration reloaded"))]
        }
        "version" => vec![format!("&fCleaner {VERSION}")],
        _ => vec![format!("&eCleaner: &f/cleaner now &7| &fcheck &7| &fkillmobs &7| &fnext &7| &freload &7| &fversion")],
    }
}

struct Cmd;

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let lines = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            let Some(st) = guard.as_mut() else { return Ok(0) };
            command(st, &server, &w)
        };
        for line in lines {
            c::reply(&sender, &format!("{PREFIX}{line}"));
        }
        Ok(1)
    }
}

struct Cleaner;

impl Plugin for Cleaner {
    fn new() -> Self {
        Cleaner
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Entity and item cleanup by MCSV".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission {
            node: format!("{NS}:command"),
            description: "Cleaner".into(),
            default: PermissionDefault::Op(PermissionLevel::Two),
            children: vec![],
        });
        let cfg = load();
        let next_at = if cfg.interval > 0 { c::now_secs() + cfg.interval } else { 0 };
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(State { last_left: cfg.interval, cfg, next_at, secs: 0, removed_total: 0, capped: 0 });
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": c::now_secs() }));

        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let cmd = Command::new(&[NS.to_owned(), "lagg".to_owned()], "Cleaner")
            .execute(Cmd)
            .then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:command"));
        context.schedule_repeating_task(20, 20, on_tick);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::remove_file("status.json");
        Ok(())
    }
}

register_plugin!(Cleaner);

//! Pumpkin WASM glue for TabList.
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, PlayerLeaveEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::wit::pumpkin::plugin::common::NamedColor;
use pumpkin_plugin_api::wit::pumpkin::plugin::scoreboard::{CollisionRule, DisplaySlot, NametagVisibility, NumberFormat, RenderType, TeamSettings};
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::cfg::{self, Config};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NS: &str = "tablist";
const OBJECTIVE: &str = "tablist_side";

/// What a player was last sent, so an update only talks to the server when something changed.
#[derive(Default, PartialEq)]
struct Shown {
    name: String,
    order: i32,
    header: String,
    footer: String,
    team: String,
}

struct State {
    cfg: Config,
    tick: u64,
    shown: HashMap<String, Shown>,
    /// Teams we made, as `dimension|team`.
    teams: HashSet<String>,
    /// Dimensions that have our sidebar, with the rows on it.
    sidebars: HashMap<String, Vec<String>>,
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

fn team_name(index: usize, group: &str) -> String {
    // The number keeps teams in group order for clients that sort by team name
    format!("r{index:02}_{group}")
}

fn team_settings(prefix: &str, suffix: &str) -> TeamSettings {
    TeamSettings {
        display_name: c::color(""),
        friendly_fire: true,
        see_friendly_invisibles: false,
        nametag_visibility: NametagVisibility::Always,
        collision_rule: CollisionRule::Always,
        color: NamedColor::White,
        prefix: c::color(prefix),
        suffix: c::color(suffix),
    }
}

fn load() -> Config {
    c::load_config::<Config>("config.toml").cleaned()
}

/// Take our teams and sidebars off every world (reload and unload).
fn clear(st: &mut State, server: &Server) {
    for w in server.get_all_worlds() {
        let dim = w.get_dimension();
        let sb = w.get_scoreboard();
        for t in st.teams.iter().filter_map(|t| t.strip_prefix(&format!("{dim}|"))) {
            sb.remove_team(t);
        }
        if st.sidebars.contains_key(&dim) {
            sb.clear_display_slot(DisplaySlot::Sidebar);
            sb.remove_objective(OBJECTIVE);
        }
    }
    st.teams.clear();
    st.sidebars.clear();
    st.shown.clear();
}

fn update(st: &mut State, server: &Server) {
    let players = server.get_all_players();
    let shared = [
        ("online", players.len().to_string()),
        ("max", server.get_max_players().to_string()),
        ("tps", format!("{:.1}", server.get_tps().min(20.0))),
        ("mspt", format!("{:.1}", server.get_mspt())),
    ];
    let head = cfg::frame(&st.cfg.header, st.tick).replace("\\n", "\n");
    let foot = cfg::frame(&st.cfg.footer, st.tick).replace("\\n", "\n");
    let groups = st.cfg.groups.len() as i32;

    for p in &players {
        let k = key(p);
        let world = p.get_world();
        let dim = world.get_dimension();
        let level = p.get_permission_level() as u8;
        let gi = st.cfg.group_index(level, |g| p.has_permission(&format!("{NS}:group.{g}")));
        let Some(group) = st.cfg.groups.get(gi).cloned() else { continue };
        // A prefix from Perms (the permissions plugin) wins over the group's own in the tab name
        let (prefix, suffix) = c::perms_decor(p).unwrap_or_else(|| (group.prefix.clone(), group.suffix.clone()));
        let mut vars = shared.to_vec();
        vars.extend([
            ("name", p.get_name()),
            ("ping", p.get_ping().to_string()),
            ("group", group.name.clone()),
            ("world", dim.trim_start_matches("minecraft:").to_owned()),
            ("time", cfg::clock(world.get_time_of_day())),
            ("prefix", prefix),
            ("suffix", suffix),
        ]);
        // `%placeholders%` (the Placeholders plugin, when installed) after TabList's own `{…}`: one call for the three texts
        let texts = [if st.cfg.name_format.is_empty() { String::new() } else { cfg::fill(&st.cfg.name_format, &vars) }, cfg::fill(&head, &vars), cfg::fill(&foot, &vars)];
        let [name, header, footer]: [String; 3] = c::placeholders_many(&texts, Some(&p.get_name())).try_into().unwrap_or(texts);
        let now = Shown {
            name,
            order: if st.cfg.sort { groups - gi as i32 } else { 0 },
            header,
            footer,
            team: if st.cfg.nametags { format!("{dim}|{}", team_name(gi, &group.name)) } else { String::new() },
        };
        let mut old = st.shown.remove(&k).unwrap_or_default();
        // Another plugin may have set the name since (Basics does on join and rank change): say it again now and then
        if st.tick % (st.cfg.interval * 15) == 0 {
            old.name.clear();
        }
        if now.name != old.name && !now.name.is_empty() {
            p.set_tab_list_name(Some(c::color(&now.name)));
        }
        if now.order != old.order {
            p.set_tab_list_order(now.order);
        }
        if (now.header != old.header || now.footer != old.footer) && !(now.header.is_empty() && now.footer.is_empty()) {
            p.set_tab_list_header_footer(c::color(&now.header), c::color(&now.footer));
        }
        if now.team != old.team && !now.team.is_empty() {
            let sb = world.get_scoreboard();
            let team = team_name(gi, &group.name);
            if st.teams.insert(now.team.clone()) {
                sb.create_team(&team, team_settings(&group.prefix, &group.suffix));
            }
            sb.add_player_to_team(&team, &p.get_name());
        }
        st.shown.insert(k, now);
    }

    if st.cfg.sidebar.enabled {
        for w in server.get_all_worlds() {
            let dim = w.get_dimension();
            if !players.iter().any(|p| p.get_world().get_dimension() == dim) && !st.sidebars.contains_key(&dim) {
                continue;
            }
            let mut vars = shared.to_vec();
            vars.push(("time", cfg::clock(w.get_time_of_day())));
            let rows = c::placeholders_many(&cfg::sidebar_rows(&st.cfg.sidebar.lines, &vars), None);
            let sb = w.get_scoreboard();
            let old = match st.sidebars.get(&dim) {
                Some(old) => old.clone(),
                None => {
                    sb.add_objective(OBJECTIVE, c::color(&st.cfg.sidebar.title), RenderType::Integer, Some(NumberFormat::Blank));
                    sb.set_display_slot(DisplaySlot::Sidebar, OBJECTIVE);
                    Vec::new()
                }
            };
            if rows != old {
                for gone in old.iter().filter(|r| !rows.contains(r)) {
                    sb.remove_score(gone, OBJECTIVE);
                }
                // The top row has the highest score. Rows that did not change are left alone:
                // a clock on the board must not resend the whole board every update.
                for (i, row) in rows.iter().enumerate() {
                    if old.len() != rows.len() || old.get(i) != Some(row) {
                        sb.update_score(row, OBJECTIVE, (rows.len() - i) as i32, Some(NumberFormat::Blank));
                    }
                }
                st.sidebars.insert(dim, rows);
            }
        }
    }
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.tick += 1;
    if st.tick % st.cfg.interval == 0 {
        update(st, &server);
    }
}

struct Cmd;

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let th = try_lock().and_then(|g| g.as_ref().map(|s| s.cfg.language != "en")).unwrap_or(true);
        let line = match w.first().map(String::as_str) {
            Some("reload") => {
                let Some(mut guard) = try_lock() else { return Ok(0) };
                if let Some(st) = guard.as_mut() {
                    clear(st, &server);
                    st.cfg = load();
                    update(st, &server);
                }
                if th { "&aโหลดไฟล์ตั้งค่าใหม่แล้ว" } else { "&aConfiguration reloaded" }.to_owned()
            }
            Some("version") => format!("&fTabList {VERSION}"),
            _ => {
                if th { "&eTabList: &f/tablist reload &7| &f/tablist version &7· ตั้งค่าที่ plugins/data/tablist/config.toml" } else { "&eTabList: &f/tablist reload &7| &f/tablist version &7· settings in plugins/data/tablist/config.toml" }.to_owned()
            }
        };
        c::reply(&sender, &format!("&8[&bTabList&8]&r {line}"));
        Ok(1)
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, e: Data<PlayerLeaveEvent>) -> Data<PlayerLeaveEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            st.shown.remove(&key(&e.player));
        }
        e
    }
}

struct TabList;

impl Plugin for TabList {
    fn new() -> Self {
        TabList
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Tab list by MCSV".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let cfg = load();
        // Group nodes can only be registered now; a group added later needs a restart to be grantable
        for g in &cfg.groups {
            let _ = context.register_permission(&Permission {
                node: format!("{NS}:group.{}", g.name),
                description: "TabList".into(),
                default: PermissionDefault::Deny,
                children: vec![],
            });
        }
        let _ = context.register_permission(&Permission {
            node: format!("{NS}:command"),
            description: "TabList".into(),
            default: PermissionDefault::Op(PermissionLevel::Two),
            children: vec![],
        });
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(State { cfg, tick: 0, shown: HashMap::new(), teams: HashSet::new(), sidebars: HashMap::new() });
        // The panel reads this to tell "loaded" from "installed, waiting for a restart"
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": now }));

        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let cmd = Command::new(&[NS.to_owned()], "TabList")
            .execute(Cmd)
            .then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:command"));
        context.register_event_handler(OnLeave, EventPriority::Normal, false)?;
        pumpkin_plugin_api::scheduler::schedule_repeating_task(20, 20, on_tick);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::remove_file("status.json");
        Ok(())
    }
}

register_plugin!(TabList);

//! Pumpkin WASM glue for Clans: commands, invites, clan chat, the home, friendly fire.
//!
//! `clans.json` keeps the clans and is written after every change. Members' tags reach other
//! plugins as `%clans_tag%` through the Placeholders plugin (Basics chat format, TabList).
use std::collections::HashMap;
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::events::{EntityDamageByEntityEvent, EventHandler, EventPriority, FromIntoEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::defs::{self, Book, Config, Home, NS, Problem, Rank};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&bClans&8]&r ";
const FILE: &str = "clans.json";
const INVITE_TICKS: u64 = 20 * 300;

struct Trip {
    player: String,
    at: u64,
    from: (f64, f64, f64),
}

struct State {
    cfg: Config,
    book: Book,
    /// Invited player → clan tag, when.
    invites: HashMap<String, (String, u64)>,
    trips: Vec<Trip>,
    tick: u64,
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

fn save(st: &State) {
    if !c::save_json(FILE, &st.book) {
        c::log_info("could not write clans.json");
    }
}

fn problem(th: bool, p: &Problem) -> String {
    let (t, e) = match p {
        Problem::BadTag => ("&cแท็กต้องเป็นตัวอักษรอังกฤษหรือตัวเลข 2 ถึง 6 ตัว", "&cA tag is 2 to 6 letters or digits"),
        Problem::BadName => ("&cต้องมีชื่อแคลน", "&cThe clan needs a name"),
        Problem::TagTaken => ("&cมีแคลนที่ใช้แท็กนี้แล้ว", "&cThat tag is taken"),
        Problem::InAClan => ("&cอยู่ในแคลนอยู่แล้ว ออกก่อน", "&cAlready in a clan: leave it first"),
        Problem::NotInAClan => ("&cคุณไม่ได้อยู่ในแคลน", "&cYou are in no clan"),
        Problem::NotAllowed => ("&cยศของคุณทำแบบนี้ไม่ได้", "&cYour rank cannot do that"),
        Problem::Full => ("&cแคลนเต็มแล้ว", "&cThe clan is full"),
        Problem::NotMember => ("&cผู้เล่นนี้ไม่ได้อยู่ในแคลนของคุณ", "&cThat player is not in your clan"),
        Problem::Leader => ("&cหัวหน้าต้องมอบแคลนให้คนอื่นก่อน (/clan leader <ผู้เล่น>) หรือยุบแคลน", "&cThe leader hands the clan over first (/clan leader <player>) or disbands it"),
    };
    msg(th, t, e)
}

/// Tell every online member of a clan.
fn tell_clan(server: &Server, book: &Book, tag: &str, line: &str) {
    let Some(clan) = book.by_tag(tag) else { return };
    for m in &clan.members {
        if let Some(p) = c::game::online(server, &m.key) {
            c::tell(&p, line);
        }
    }
}

/// The key of a player by name: online first, then members of clans.
fn key_by_name(st: &State, server: &Server, name: &str) -> Option<(String, String)> {
    if let Some(p) = server.get_all_players().into_iter().find(|p| p.get_name().eq_ignore_ascii_case(name)) {
        return Some((c::game::key(&p), p.get_name()));
    }
    st.book.clans.iter().flat_map(|c| &c.members).find(|m| m.name.eq_ignore_ascii_case(name)).map(|m| (m.key.clone(), m.name.clone()))
}

fn run(st: &mut State, sender: &CommandSender, server: &Server, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    let admin = c::has_perm(sender, &format!("{NS}:admin"));
    let Some(p) = c::sender_player(sender) else {
        // The console: admin words only
        return match w.first().map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("version") => vec![format!("Clans {VERSION}")],
            Some("list") if st.book.clans.is_empty() => vec!["no clans yet".into()],
            Some("list") => st.book.clans.iter().map(|cl| format!("{} {} ({})", cl.tag, cl.name, cl.members.len())).collect(),
            Some("delete") => match w.get(1).and_then(|t| st.book.clans.iter().position(|cl| cl.tag.eq_ignore_ascii_case(t))) {
                Some(at) => {
                    st.book.clans.remove(at);
                    save(st);
                    vec!["deleted".into()]
                }
                None => vec!["no such clan".into()],
            },
            Some("reload") => {
                st.cfg = c::load_config::<Config>("config.toml").cleaned();
                vec!["settings read again".into()]
            }
            _ => vec!["clans version | list | delete <tag> | reload".into()],
        };
    };
    let me = c::game::key(&p);
    let name = p.get_name();
    let rest = |n: usize| w.iter().skip(n).cloned().collect::<Vec<_>>().join(" ");
    let mine = st.book.of(&me).map(|cl| cl.tag.clone());
    match w.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("create") => {
            let (Some(tag), name_text) = (w.get(1), rest(2)) else { return vec![msg(th, "&f/clan create <แท็ก> <ชื่อแคลน>", "&f/clan create <tag> <name>")] };
            let clan_name = if name_text.trim().is_empty() { tag.clone() } else { name_text };
            if st.cfg.create_cost > 0.0 {
                if st.book.of(&me).is_some() {
                    return vec![problem(th, &Problem::InAClan)];
                }
                match c::game::money::withdraw(&name, st.cfg.create_cost) {
                    Some(Ok(_)) => {}
                    Some(Err(_)) => return vec![msg(th, &format!("&cต้องมีเงิน {} เพื่อตั้งแคลน", st.cfg.create_cost), &format!("&cA clan costs {}", st.cfg.create_cost))],
                    None => return vec![msg(th, "&cระบบเงิน (Basics) ไม่ตอบ ลองใหม่", "&cThe money plugin (Basics) did not answer: try again")],
                }
            }
            match st.book.create(tag, &clan_name, &me, &name, c::now_secs()) {
                Ok(()) => {
                    save(st);
                    vec![msg(th, &format!("&aตั้งแคลน [{tag}] แล้ว เชิญเพื่อนด้วย /clan invite <ผู้เล่น>"), &format!("&aClan [{tag}] made: invite friends with /clan invite <player>"))]
                }
                Err(e) => {
                    if st.cfg.create_cost > 0.0 {
                        let _ = c::game::money::deposit(&name, st.cfg.create_cost);
                    }
                    vec![problem(th, &e)]
                }
            }
        }
        Some("invite") => {
            let Some(tag) = mine else { return vec![problem(th, &Problem::NotInAClan)] };
            if st.book.of(&me).and_then(|cl| cl.rank_of(&me)) < Some(Rank::Officer) {
                return vec![problem(th, &Problem::NotAllowed)];
            }
            let Some(target) = w.get(1).and_then(|n| server.get_all_players().into_iter().find(|p| p.get_name().eq_ignore_ascii_case(n))) else { return vec![msg(th, "&cผู้เล่นไม่ได้ออนไลน์", "&cThat player is not online")] };
            st.invites.insert(c::game::key(&target), (tag.clone(), st.tick));
            let button = c::color(if th { "&a[เข้าร่วม]" } else { "&a[Join]" }).click_run_command(&format!("/clan join {tag}"));
            target.send_system_message(pumpkin_plugin_api::text::TextComponent::text("").add_child(c::color(&msg(th, &format!("&f{name} เชิญคุณเข้าแคลน [{tag}] "), &format!("&f{name} invites you to clan [{tag}] ")))).add_child(button), false);
            vec![msg(th, &format!("&aเชิญ {} แล้ว", target.get_name()), &format!("&aInvited {}", target.get_name()))]
        }
        Some("join") => {
            let Some(tag) = w.get(1) else { return vec![msg(th, "&f/clan join <แท็ก>", "&f/clan join <tag>")] };
            let invited = st.invites.get(&me).is_some_and(|(t, at)| t.eq_ignore_ascii_case(tag) && st.tick < at + INVITE_TICKS);
            if !invited {
                return vec![msg(th, "&cยังไม่มีคำเชิญจากแคลนนี้", "&cYou have no invite from that clan")];
            }
            match st.book.join(tag, &me, &name, st.cfg.max_members) {
                Ok(()) => {
                    st.invites.remove(&me);
                    save(st);
                    let tag = st.book.of(&me).map(|cl| cl.tag.clone()).unwrap_or_default();
                    tell_clan(server, &st.book, &tag, &msg(th, &format!("&a{name} เข้าแคลนแล้ว"), &format!("&a{name} joined the clan")));
                    Vec::new()
                }
                Err(e) => vec![problem(th, &e)],
            }
        }
        Some("leave") => match st.book.leave(&me) {
            Ok(()) => {
                save(st);
                if let Some(tag) = &mine {
                    tell_clan(server, &st.book, tag, &msg(th, &format!("&7{name} ออกจากแคลน"), &format!("&7{name} left the clan")));
                }
                vec![msg(th, "&7ออกจากแคลนแล้ว", "&7You left the clan")]
            }
            Err(e) => vec![problem(th, &e)],
        },
        Some(verb @ ("kick" | "promote" | "demote" | "leader")) => {
            let Some((who, who_name)) = w.get(1).and_then(|n| key_by_name(st, server, n)) else { return vec![msg(th, "&cไม่รู้จักผู้เล่นนี้", "&cUnknown player")] };
            let done = match verb {
                "kick" => st.book.kick(&me, &who),
                "promote" => st.book.set_rank(&me, &who, Rank::Officer),
                "demote" => st.book.set_rank(&me, &who, Rank::Member),
                _ => st.book.set_rank(&me, &who, Rank::Leader),
            };
            match done {
                Ok(()) => {
                    save(st);
                    let line = match verb {
                        "kick" => msg(th, &format!("&7{who_name} ถูกนำออกจากแคลน"), &format!("&7{who_name} was taken out of the clan")),
                        "leader" => msg(th, &format!("&a{who_name} เป็นหัวหน้าแคลนแล้ว"), &format!("&a{who_name} leads the clan now")),
                        _ => msg(th, &format!("&a{who_name} เปลี่ยนยศแล้ว"), &format!("&a{who_name}'s rank changed")),
                    };
                    if let Some(tag) = &mine {
                        tell_clan(server, &st.book, tag, &line);
                    }
                    if verb == "kick"
                        && let Some(k) = c::game::online(server, &who)
                    {
                        c::tell(&k, &line);
                    }
                    Vec::new()
                }
                Err(e) => vec![problem(th, &e)],
            }
        }
        Some("disband") => {
            if w.get(1).map(String::as_str) != Some("confirm") {
                return vec![msg(th, "&cพิมพ์ /clan disband confirm เพื่อยุบแคลน", "&cType /clan disband confirm to end the clan")];
            }
            match st.book.disband(&me) {
                Ok(clan) => {
                    save(st);
                    for m in &clan.members {
                        if let Some(o) = c::game::online(server, &m.key) {
                            c::tell(&o, &msg(th, &format!("&7แคลน [{}] ถูกยุบแล้ว", clan.tag), &format!("&7Clan [{}] has ended", clan.tag)));
                        }
                    }
                    Vec::new()
                }
                Err(e) => vec![problem(th, &e)],
            }
        }
        Some("info") => {
            let clan = match w.get(1) {
                Some(tag) => st.book.by_tag(tag),
                None => st.book.of(&me),
            };
            let Some(clan) = clan else { return vec![msg(th, "&cไม่มีแคลนนี้", "&cNo such clan")] };
            let mut out = vec![msg(th, &format!("&b[{}] &f{} &7สมาชิก {}/{}", clan.tag, clan.name, clan.members.len(), st.cfg.max_members), &format!("&b[{}] &f{} &7{}/{} members", clan.tag, clan.name, clan.members.len(), st.cfg.max_members))];
            let mut members = clan.members.clone();
            members.sort_by(|a, b| b.rank.cmp(&a.rank).then(a.name.cmp(&b.name)));
            out.extend(members.iter().map(|m| format!("&8- &f{} &7{}{}", m.name, m.rank.label(th), if c::game::online(server, &m.key).is_some() { " &a●" } else { "" })));
            out
        }
        Some("list") => {
            let mut out = vec![msg(th, &format!("&fแคลน {} แคลน", st.book.clans.len()), &format!("&f{} clan(s)", st.book.clans.len()))];
            let mut list: Vec<&defs::Clan> = st.book.clans.iter().collect();
            list.sort_by(|a, b| b.members.len().cmp(&a.members.len()));
            out.extend(list.iter().take(30).map(|cl| format!("&b[{}] &f{} &7({})", cl.tag, cl.name, cl.members.len())));
            out
        }
        Some("chat" | "c") => {
            let Some(tag) = mine else { return vec![problem(th, &Problem::NotInAClan)] };
            let text = rest(1);
            if !text.trim().is_empty() {
                tell_clan(server, &st.book, &tag, &defs::chat_line(&st.cfg, &tag, &name, &text));
            }
            Vec::new()
        }
        Some("sethome") => {
            let Some(clan) = st.book.of_mut(&me) else { return vec![problem(th, &Problem::NotInAClan)] };
            if clan.rank_of(&me) < Some(Rank::Officer) {
                return vec![problem(th, &Problem::NotAllowed)];
            }
            let at = p.get_position();
            clan.home = Some(Home { world: p.get_world().get_dimension(), x: at.0, y: at.1, z: at.2, yaw: p.get_yaw() });
            save(st);
            vec![msg(th, "&aตั้งบ้านแคลนแล้ว", "&aClan home set")]
        }
        Some("home") => {
            let Some(clan) = st.book.of(&me) else { return vec![problem(th, &Problem::NotInAClan)] };
            if clan.home.is_none() {
                return vec![msg(th, "&cแคลนยังไม่มีบ้าน (/clan sethome)", "&cThe clan has no home yet (/clan sethome)")];
            }
            st.trips.retain(|t| t.player != me);
            st.trips.push(Trip { player: me, at: st.tick + u64::from(st.cfg.home_delay) * 20, from: p.get_position() });
            vec![msg(th, &format!("&7ยืนนิ่ง {} วินาที", st.cfg.home_delay), &format!("&7Stand still for {}s", st.cfg.home_delay))]
        }
        Some("ff") => {
            let Some(clan) = st.book.of_mut(&me) else { return vec![problem(th, &Problem::NotInAClan)] };
            if clan.rank_of(&me) != Some(Rank::Leader) {
                return vec![problem(th, &Problem::NotAllowed)];
            }
            clan.friendly_fire = matches!(w.get(1).map(String::as_str), Some("on"));
            let on = clan.friendly_fire;
            save(st);
            vec![msg(th, if on { "&7สมาชิกตีกันได้แล้ว" } else { "&7สมาชิกตีกันไม่ได้แล้ว" }, if on { "&7Members can hurt each other now" } else { "&7Members cannot hurt each other now" })]
        }
        Some("delete") if admin => match w.get(1).and_then(|t| st.book.clans.iter().position(|cl| cl.tag.eq_ignore_ascii_case(t))) {
            Some(at) => {
                st.book.clans.remove(at);
                save(st);
                vec![msg(th, "&aลบแคลนแล้ว", "&aClan deleted")]
            }
            None => vec![msg(th, "&cไม่มีแคลนนี้", "&cNo such clan")],
        },
        Some("reload") if admin => {
            st.cfg = c::load_config::<Config>("config.toml").cleaned();
            vec![msg(st.cfg.thai(), "&aอ่านค่าตั้งใหม่แล้ว", "&aSettings read again")]
        }
        Some("version") => vec![format!("Clans {VERSION}")],
        _ => {
            let mut out = vec![
                msg(th, "&f/clan create <แท็ก> <ชื่อ> &7| &finvite <ผู้เล่น> &7| &fjoin <แท็ก> &7| &fleave &7| &finfo [แท็ก] &7| &flist", "&f/clan create <tag> <name> &7| &finvite <player> &7| &fjoin <tag> &7| &fleave &7| &finfo [tag] &7| &flist"),
                msg(th, "&f/clan kick|promote|demote|leader <ผู้เล่น> &7| &fdisband confirm &7| &fsethome &7| &fhome &7| &fff on|off &7| &f/cc <ข้อความ>", "&f/clan kick|promote|demote|leader <player> &7| &fdisband confirm &7| &fsethome &7| &fhome &7| &fff on|off &7| &f/cc <message>"),
            ];
            if admin {
                out.push(msg(th, "&f/clan delete <แท็ก> &7| &freload &7| &fversion", "&f/clan delete <tag> &7| &freload &7| &fversion"));
            }
            out
        }
    }
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.tick += 1;
    if st.trips.is_empty() {
        return;
    }
    let th = st.cfg.thai();
    let tick = st.tick;
    let (due, wait): (Vec<Trip>, Vec<Trip>) = std::mem::take(&mut st.trips).into_iter().partition(|t| tick >= t.at);
    st.trips = wait;
    for t in due {
        let Some(p) = c::game::online(&server, &t.player) else { continue };
        if c::game::dist2(p.get_position(), t.from) > 0.25 {
            c::action_bar(&p, &msg(th, "&cขยับแล้ว ยกเลิกการวาร์ป", "&cYou moved: the trip is off"));
            continue;
        }
        let Some(home) = st.book.of(&t.player).and_then(|cl| cl.home.clone()) else { continue };
        if let Some(world) = c::game::world_named(&server, &home.world) {
            p.teleport((home.x, home.y, home.z), Some(home.yaw), None, world);
        }
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

/// Members of one clan do not hurt each other (unless the clan allows it).
struct OnHit;
impl EventHandler<EntityDamageByEntityEvent> for OnHit {
    fn handle(&self, server: Server, mut e: Data<EntityDamageByEntityEvent>) -> Data<EntityDamageByEntityEvent> {
        if e.cancelled {
            return e;
        }
        let Some(guard) = try_lock() else { return e };
        let Some(st) = guard.as_ref() else { return e };
        if st.book.clans.is_empty() {
            return e;
        }
        let players = server.get_all_players();
        let find = |id: i32| players.iter().find(|p| p.as_entity().get_id() as i32 == id).map(c::game::key);
        if let (Some(a), Some(b)) = (find(e.damager_id), find(e.entity_id))
            && st.book.friends(&a, &b)
        {
            e.cancelled = true;
        }
        e
    }
}

fn answer(sender: &CommandSender, server: &Server, w: &[String]) -> i32 {
    let lines = {
        let Some(mut guard) = try_lock() else { return 0 };
        let Some(st) = guard.as_mut() else { return 0 };
        run(st, sender, server, w)
    };
    for line in lines {
        c::reply(sender, &line);
    }
    1
}

struct Cmd;
impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        Ok(answer(&sender, &server, &c::words(&args, "args")))
    }
}

/// `/cc <message>`: clan chat.
struct ChatCmd;
impl CommandHandler for ChatCmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let mut w = vec!["chat".to_owned()];
        w.extend(c::words(&args, "args"));
        Ok(answer(&sender, &server, &w))
    }
}

struct Clans;

impl Plugin for Clans {
    fn new() -> Self {
        Clans
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Clans with tags, ranks, a home and their own chat".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:use"), description: "Make and join clans".into(), default: PermissionDefault::Allow, children: vec![] });
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Delete clans".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let cfg = c::load_config::<Config>("config.toml").cleaned();
        let book: Book = c::load_json(FILE).unwrap_or_default();
        c::log_info(&format!("{} clan(s)", book.clans.len()));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg, book, invites: HashMap::new(), trips: Vec::new(), tick: 0 });
        let cmd = Command::new(&["clan".to_owned(), NS.to_owned()], "Clans").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:use"));
        let chat = Command::new(&["cc".to_owned()], "Clan chat").then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(ChatCmd));
        context.register_command(chat, &format!("{NS}:use"));
        context.register_event_handler(OnHit, EventPriority::Normal, true)?;
        context.schedule_repeating_task(1, 1, on_tick);
        c::mark_running(VERSION);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::mark_stopped();
        if let Ok(guard) = STATE.lock()
            && let Some(st) = guard.as_ref()
        {
            save(st);
        }
        Ok(())
    }

    /// `%clans_tag%`, `%clans_name%`, `%clans_rank%` for the Placeholders plugin; `{"kind":
    /// "clan","player":<uuid hex>}` says a player's clan.
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let guard = try_lock().ok_or("busy")?;
        let st = guard.as_ref().ok_or("not loaded")?;
        let text = |k: &str| v.get(k).and_then(serde_json::Value::as_str).unwrap_or_default().replace('-', "").to_ascii_lowercase();
        let answer = match v.get("kind").and_then(serde_json::Value::as_str).unwrap_or_default() {
            "placeholder" => {
                let key = text("uuid");
                let clan = st.book.of(&key);
                let value = match v.get("params").and_then(serde_json::Value::as_str).unwrap_or_default() {
                    "tag" => Some(clan.map(|c| c.tag.clone()).unwrap_or_default()),
                    "name" => Some(clan.map(|c| c.name.clone()).unwrap_or_default()),
                    "rank" => Some(clan.and_then(|c| c.rank_of(&key)).map(|r| r.label(st.cfg.thai()).to_owned()).unwrap_or_default()),
                    "members" => Some(clan.map_or(0, |c| c.members.len()).to_string()),
                    _ => None,
                };
                serde_json::json!({ "value": value })
            }
            "clan" => {
                let clan = st.book.of(&text("player"));
                serde_json::json!({ "ok": true, "tag": clan.map(|c| c.tag.clone()), "name": clan.map(|c| c.name.clone()) })
            }
            other => return Err(format!("unknown kind {other}")),
        };
        serde_json::to_vec(&answer).map_err(|e| e.to_string())
    }
}

register_plugin!(Clans);

//! Pumpkin WASM glue for ResourcePack: who has which pack, sending, answers, hashes, commands.
//!
//! Twice a second the tick task looks at every Java player: a new one gets their packs after
//! `join-delay-ticks`, one who changed world gets the packs of that world added and the others
//! taken away. The client's answers come through `PlayerResourcePackStatusEvent`.
//!
//! A pack without `sha1` in the config is downloaded once by the plugin (never kept, only
//! hashed, a few megabytes per tick at most) so that clients can tell "I already have this
//! file" and do not download it again at every join.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use mcsv_common as c;
use mcsv_common::http::{self, Pending, Request};
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, PlayerLeaveEvent, PlayerResourcePackStatusEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA, HTTP_OUTBOUND, NETWORK_DNS};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::uuid::Uuid;
use pumpkin_plugin_api::wit::pumpkin::plugin::player::{JavaKickOptions, JavaResourcePack, SocketTeardownPolicy};
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};
use serde::{Deserialize, Serialize};

use crate::cfg::{self, Answer, Config, Pack};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NS: &str = "resourcepack";
const P: &str = "&8[&bResourcePack&8]&r ";
/// The tick task runs this often (ticks).
const STEP: u64 = 10;
/// Seconds a player waits between two `/rp`.
const RELOAD_COOLDOWN: u64 = 10;
const HASH_TIMEOUT: Duration = Duration::from_secs(600);

/// SHA-1 of a pack as the plugin worked it out.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Hash {
    sha1: String,
    size: u64,
    /// Unix seconds.
    at: u64,
}

struct HashJob {
    pack: String,
    url: String,
    pending: Pending,
    /// Whom to tell (a `/rp hash` from a player); the console always gets the line.
    who: Option<String>,
}

/// What we know about one player.
#[derive(Default)]
struct Seen {
    world: String,
    /// Tick from which the packs are sent; `None` once that has happened.
    due: Option<u64>,
    /// Pack name → the client's last answer and the tick the pack was sent.
    packs: HashMap<String, (Answer, u64)>,
    /// Unix second until which `/rp` is refused.
    cooldown: u64,
}

struct State {
    cfg: Config,
    problems: Vec<String>,
    tick: u64,
    /// Download link → hash, kept in `hashes.json`.
    hashes: HashMap<String, Hash>,
    jobs: Vec<HashJob>,
    /// Lowercase player name → what they have.
    players: HashMap<String, Seen>,
    sent: u64,
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

fn load() -> (Config, Vec<String>) {
    c::load_config::<Config>("config.toml").cleaned()
}

fn msg(th: bool, thai: &str, english: &str) -> String {
    format!("{P}{}", if th { thai } else { english })
}

fn uuid_of(name: &str) -> Uuid {
    let (high, low) = cfg::pack_id(name);
    Uuid { high, low }
}

/// The SHA-1 to send with a pack: the one in the config, else the one worked out, else none
/// (the pack still loads; the client just downloads it every time).
fn hash_of(st: &State, pack: &Pack) -> String {
    if !pack.sha1.is_empty() {
        return pack.sha1.clone();
    }
    st.hashes.get(&pack.url).map(|h| h.sha1.clone()).unwrap_or_default()
}

fn save_hashes(st: &State) {
    c::save_json("hashes.json", &st.hashes);
}

/// Start working out the SHA-1 of a pack. False when the host refused the request.
fn start_hash(st: &mut State, pack: &Pack, who: Option<String>) -> bool {
    if st.jobs.iter().any(|j| j.url == pack.url) {
        return true;
    }
    let mut req = Request::get(&pack.url);
    req.timeout = HASH_TIMEOUT;
    req.digest = true;
    req.max_body = (st.cfg.max_pack_mb * 1024 * 1024) as usize;
    match http::start(req) {
        Ok(pending) => {
            st.jobs.push(HashJob { pack: pack.name.clone(), url: pack.url.clone(), pending, who });
            true
        }
        Err(e) => {
            c::log_info(&format!("pack {}: the download could not be started: {e}", pack.name));
            false
        }
    }
}

/// Packs without a `sha1` whose worked-out hash is missing or older than `recheck-hours`.
fn start_due_hashes(st: &mut State) {
    let now = c::now_secs();
    let recheck = st.cfg.recheck_hours * 3600;
    let due: Vec<Pack> = st
        .cfg
        .packs
        .iter()
        .filter(|p| p.enabled && p.sha1.is_empty())
        .filter(|p| st.hashes.get(&p.url).is_none_or(|h| recheck > 0 && now.saturating_sub(h.at) >= recheck))
        .cloned()
        .collect();
    for pack in due {
        start_hash(st, &pack, None);
    }
}

fn send_pack(st: &mut State, p: &Player, pack: &Pack) -> bool {
    let Some(java) = p.as_java() else { return false };
    let prompt = (!st.cfg.prompt.is_empty()).then(|| c::color(&st.cfg.prompt));
    java.send_resource_pack(JavaResourcePack { id: uuid_of(&pack.name), url: pack.url.clone(), hash: hash_of(st, pack), forced: st.cfg.forced(pack), prompt_message: prompt });
    st.sent += 1;
    true
}

/// Give a player the packs of the world they are in and take away the ones that belong elsewhere.
/// `again` = send also the ones they already have (`/rp`, a pack that changed).
fn sync(st: &mut State, p: &Player, again: bool) {
    let name = p.get_name().to_ascii_lowercase();
    let world = cfg::world_name(&p.get_world().get_dimension());
    let wanted: Vec<Pack> = st.cfg.packs_for(&world).into_iter().cloned().collect();
    let have: Vec<String> = st.players.get(&name).map(|s| s.packs.keys().cloned().collect()).unwrap_or_default();
    if let Some(java) = p.as_java() {
        for gone in have.iter().filter(|h| !wanted.iter().any(|w| &w.name == *h)) {
            java.remove_resource_pack(uuid_of(gone));
        }
    }
    let tick = st.tick;
    let mut packs: HashMap<String, (Answer, u64)> = HashMap::new();
    for pack in &wanted {
        let old = st.players.get(&name).and_then(|s| s.packs.get(&pack.name)).copied();
        match old {
            Some(o) if !again => {
                packs.insert(pack.name.clone(), o);
            }
            _ => {
                if send_pack(st, p, pack) {
                    packs.insert(pack.name.clone(), (Answer::Other, tick));
                }
            }
        }
    }
    let seen = st.players.entry(name).or_default();
    seen.world = world;
    seen.due = None;
    seen.packs = packs;
}

fn kick(p: &Player, text: &str) {
    if let Some(java) = p.as_java() {
        java.kick(JavaKickOptions { reason: c::color(text), log_to_console: true, teardown_policy: SocketTeardownPolicy::Graceful });
    }
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.tick += STEP;
    let th = st.cfg.language != "en";

    // Hashes that have arrived
    let mut i = 0;
    while i < st.jobs.len() {
        let Some(result) = st.jobs[i].pending.poll() else {
            i += 1;
            continue;
        };
        let job = st.jobs.swap_remove(i);
        let line = match result {
            Ok(resp) if resp.status == 200 && resp.size > 0 => match resp.digest {
                Some(d) => {
                    let sha1 = c::sha1::hex(&d);
                    let changed = st.hashes.get(&job.url).is_none_or(|h| h.sha1 != sha1);
                    st.hashes.insert(job.url.clone(), Hash { sha1: sha1.clone(), size: resp.size, at: c::now_secs() });
                    save_hashes(st);
                    let mut again = 0;
                    if changed {
                        // The file is new: everyone who has the pack gets it again (the client swaps it in)
                        if let Some(pack) = st.cfg.packs.iter().find(|p| p.name == job.pack && p.url == job.url).cloned() {
                            for p in server.get_all_players() {
                                let name = p.get_name().to_ascii_lowercase();
                                if st.players.get(&name).is_some_and(|s| s.packs.contains_key(&pack.name)) && send_pack(st, &p, &pack) {
                                    let tick = st.tick;
                                    if let Some(s) = st.players.get_mut(&name) {
                                        s.packs.insert(pack.name.clone(), (Answer::Other, tick));
                                    }
                                    again += 1;
                                }
                            }
                        }
                    }
                    let size = resp.size as f64 / 1024.0 / 1024.0;
                    let sent = if again > 0 { if th { format!(" ส่งใหม่ให้ {again} คน") } else { format!(", sent again to {again} players") } } else { String::new() };
                    msg(th, &format!("&apack &f{} &a{size:.1} MB SHA-1 &f{sha1}{}{sent}", job.pack, if changed { "" } else { " &7(เหมือนเดิม)" }), &format!("&apack &f{} &a{size:.1} MB SHA-1 &f{sha1}{}{sent}", job.pack, if changed { "" } else { " &7(unchanged)" }))
                }
                None => msg(th, &format!("&cpack &f{}&c: ไม่ได้ค่า SHA-1", job.pack), &format!("&cpack &f{}&c: no SHA-1 came back", job.pack)),
            },
            Ok(resp) => msg(th, &format!("&cpack &f{}&c: ลิงก์ตอบรหัส {} &7(ต้องเป็นลิงก์ดาวน์โหลดตรงของไฟล์ .zip)", job.pack, resp.status), &format!("&cpack &f{}&c: the link answered {} &7(it must be a direct download link of the .zip)", job.pack, resp.status)),
            Err(e) if e.contains("too large") => msg(th, &format!("&cpack &f{}&c: ไฟล์ใหญ่กว่า {} MB (max-pack-mb)", job.pack, st.cfg.max_pack_mb), &format!("&cpack &f{}&c: the file is larger than {} MB (max-pack-mb)", job.pack, st.cfg.max_pack_mb)),
            Err(e) => {
                let why: String = e.chars().filter(|c| !matches!(c, '{' | '}' | '"')).take(70).collect();
                msg(th, &format!("&cpack &f{}&c: ดาวน์โหลดไม่ได้ &7({why})", job.pack), &format!("&cpack &f{}&c: could not be downloaded &7({why})", job.pack))
            }
        };
        c::log_info(&c::plain(&line));
        if let Some(p) = job.who.as_deref().and_then(|w| server.get_player_by_name(w)) {
            c::tell(&p, &line);
        }
    }
    if st.tick % (20 * 60 * 5) < STEP {
        start_due_hashes(st);
    }

    // Players: the new ones, the ones who changed world, the ones who never answered
    let players = server.get_all_players();
    let mut online: Vec<String> = Vec::with_capacity(players.len());
    for p in &players {
        if p.as_java().is_none() || c::awaiting_login(p) {
            continue;
        }
        let name = p.get_name().to_ascii_lowercase();
        online.push(name.clone());
        let world = cfg::world_name(&p.get_world().get_dimension());
        let tick = st.tick;
        let delay = st.cfg.join_delay_ticks;
        let on_join = st.cfg.send_on_join;
        let seen = st.players.entry(name.clone()).or_insert_with(|| Seen { world: world.clone(), due: on_join.then_some(tick + delay), ..Seen::default() });
        let first = seen.due.is_some_and(|due| tick >= due);
        let moved = seen.due.is_none() && seen.world != world && on_join;
        if first || moved {
            sync(st, p, false);
        }
        let timeout = st.cfg.answer_timeout_seconds * 20;
        if timeout > 0 && st.cfg.kick_on_refuse {
            let silent = st.players.get(&name).is_some_and(|s| s.packs.iter().any(|(pack, (answer, at))| matches!(answer, Answer::Other) && tick.saturating_sub(*at) >= timeout && st.cfg.packs.iter().any(|x| &x.name == pack && st.cfg.forced(x))));
            if silent {
                kick(p, &st.cfg.kick_message);
            }
        }
    }
    st.players.retain(|name, _| online.contains(name));
}

type Data<E> = <E as FromIntoEvent>::Data;

/// The client's answer to a pack: remembered, and acted on when it is a refusal.
struct OnStatus;
impl EventHandler<PlayerResourcePackStatusEvent> for OnStatus {
    fn handle(&self, _server: Server, e: Data<PlayerResourcePackStatusEvent>) -> Data<PlayerResourcePackStatusEvent> {
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let Some(pack) = st.cfg.packs.iter().find(|p| cfg::id_text(cfg::pack_id(&p.name)).eq_ignore_ascii_case(&e.pack_id)).cloned() else { return e };
        let answer = cfg::answer(&e.status);
        let name = e.player.get_name().to_ascii_lowercase();
        if let Some(entry) = st.players.get_mut(&name).and_then(|s| s.packs.get_mut(&pack.name)) {
            entry.0 = answer;
        }
        if answer.refused() {
            if st.cfg.forced(&pack) {
                if st.cfg.kick_on_refuse {
                    kick(&e.player, &st.cfg.kick_message);
                } else {
                    c::tell(&e.player, &format!("{P}{}", st.cfg.kick_message.replace('\n', " ")));
                }
            } else if !st.cfg.declined_message.is_empty() {
                c::tell(&e.player, &format!("{P}{}", st.cfg.declined_message));
            }
        }
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, e: Data<PlayerLeaveEvent>) -> Data<PlayerLeaveEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            st.players.remove(&e.player.get_name().to_ascii_lowercase());
        }
        e
    }
}

fn pack_line(st: &State, p: &Pack, th: bool) -> String {
    let hash = if !p.sha1.is_empty() {
        format!("&7sha1 {}… (config)", &p.sha1[..10])
    } else if let Some(h) = st.hashes.get(&p.url) {
        format!("&7sha1 {}… ({:.1} MB)", &h.sha1[..10], h.size as f64 / 1024.0 / 1024.0)
    } else if st.jobs.iter().any(|j| j.url == p.url) {
        (if th { "&eกำลังคำนวณ sha1" } else { "&ehash being worked out" }).to_owned()
    } else {
        (if th { "&cยังไม่มี sha1 (พิมพ์ /rp hash)" } else { "&cno hash yet (/rp hash)" }).to_owned()
    };
    let worlds = if p.worlds.is_empty() { if th { "ทุกโลก".to_owned() } else { "every world".to_owned() } } else { p.worlds.join(", ") };
    let flags = match (p.enabled, st.cfg.forced(p), th) {
        (false, _, true) => " &8(ปิดอยู่)",
        (false, _, false) => " &8(off)",
        (true, true, true) => " &c(บังคับ)",
        (true, true, false) => " &c(required)",
        _ => "",
    };
    format!("&b {}{flags} &7{worlds} · {hash} &8{}", p.name, p.url.chars().take(70).collect::<String>())
}

fn help(th: bool, admin: bool) -> Vec<String> {
    let mut lines: Vec<(&str, &str)> = vec![
        ("&bResourcePack &7ส่ง resource pack ของเซิร์ฟให้ผู้เล่นตอนเข้าเกม", "&bResourcePack &7sends the server's resource packs to players when they join"),
        ("&f/rp &7โหลด resource pack ของเซิร์ฟอีกครั้ง · &f/rp status &7ดูว่าโหลดอะไรอยู่", "&f/rp &7load the server's packs again · &f/rp status &7what you have"),
    ];
    if admin {
        lines.push(("&f/rp list &7pack ที่ตั้งไว้ · &f/rp status <ผู้เล่น> &7· &f/rp send <ผู้เล่น|all> [pack] &7· &f/rp clear <ผู้เล่น|all>", "&f/rp list &7the packs · &f/rp status <player> &7· &f/rp send <player|all> [pack] &7· &f/rp clear <player|all>"));
        lines.push(("&f/rp hash [pack] &7คำนวณ SHA-1 ใหม่ (หลังอัปโหลดไฟล์ใหม่ที่ลิงก์เดิม) · &f/rp reload &7| &fversion &7· ตั้ง pack ที่ plugins/data/resourcepack/config.toml", "&f/rp hash [pack] &7work out the SHA-1 again (after uploading a new file to the same link) · &f/rp reload &7| &fversion &7· packs are set in plugins/data/resourcepack/config.toml"));
    }
    lines.iter().map(|(t, e)| format!("{P}{}", if th { *t } else { *e })).collect()
}

struct Cmd;

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let me = c::sender_player(&sender);
        if me.as_ref().is_some_and(c::awaiting_login) {
            return Ok(0);
        }
        let Some(mut guard) = try_lock() else { return Ok(0) };
        let Some(st) = guard.as_mut() else { return Ok(0) };
        let th = st.cfg.language != "en";
        let admin = me.as_ref().is_none_or(|p| p.has_permission(&format!("{NS}:admin")));
        let reply = |line: String| c::reply(&sender, &line);
        let staff_only = || c::reply(&sender, &msg(th, "&cคำสั่งนี้สำหรับผู้ดูแล", "&cThis is for staff"));
        // Players named by a staff command: one name, or `all`
        let targets = |who: Option<&String>| -> Vec<Player> {
            match who.map(String::as_str) {
                Some("all" | "*") => server.get_all_players(),
                Some(name) => server.get_player_by_name(name).into_iter().collect(),
                None => Vec::new(),
            }
        };
        let sub = w.first().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
        match sub.as_str() {
            "" | "load" if me.is_some() => {
                let Some(p) = &me else { return Ok(0) };
                if !st.cfg.players_can_reload && !admin {
                    reply(msg(th, "&cเซิร์ฟนี้ไม่ให้โหลด resource pack เอง", "&cLoading the packs yourself is turned off on this server"));
                    return Ok(0);
                }
                let name = p.get_name().to_ascii_lowercase();
                let now = c::now_secs();
                let wait = st.players.get(&name).map_or(0, |s| s.cooldown.saturating_sub(now));
                if wait > 0 && !admin {
                    reply(msg(th, &format!("&eรออีก {wait} วินาที"), &format!("&eWait {wait} more seconds")));
                    return Ok(0);
                }
                sync(st, p, true);
                let count = st.players.get_mut(&name).map_or(0, |s| {
                    s.cooldown = now + RELOAD_COOLDOWN;
                    s.packs.len()
                });
                reply(if count == 0 { msg(th, "&7เซิร์ฟนี้ไม่มี resource pack สำหรับโลกนี้", "&7This server has no resource pack for this world") } else { msg(th, &format!("&aส่ง resource pack ให้แล้ว {count} ชุด"), &format!("&a{count} resource pack(s) sent")) });
            }
            "" | "help" => help(th, admin).into_iter().for_each(reply),
            "version" => reply(msg(th, &format!("&fResourcePack {VERSION} &7· pack {} ชุด ส่งไปแล้ว {} ครั้ง", st.cfg.packs.iter().filter(|p| p.enabled).count(), st.sent), &format!("&fResourcePack {VERSION} &7· {} packs, sent {} times", st.cfg.packs.iter().filter(|p| p.enabled).count(), st.sent))),
            "status" => {
                let who = match (w.get(1), &me) {
                    (Some(name), _) if admin => name.clone(),
                    (Some(_), _) => return Ok(staff_only()).map(|()| 0),
                    (None, Some(p)) => p.get_name(),
                    (None, None) => {
                        reply(msg(th, "&eจาก console ต้องบอกชื่อผู้เล่น: &frp status <ผู้เล่น>", "&eFrom the console name the player: &frp status <player>"));
                        return Ok(0);
                    }
                };
                match st.players.get(&who.to_ascii_lowercase()) {
                    Some(seen) if !seen.packs.is_empty() => {
                        reply(msg(th, &format!("&f{who} &7(โลก {})", seen.world), &format!("&f{who} &7(world {})", seen.world)));
                        let mut rows: Vec<_> = seen.packs.iter().collect();
                        rows.sort_by(|a, b| a.0.cmp(b.0));
                        for (pack, (answer, _)) in rows {
                            reply(format!("&b {pack}&7: {}", answer.label(th)));
                        }
                    }
                    Some(_) => reply(msg(th, &format!("&f{who} &7ยังไม่ได้รับ resource pack"), &format!("&f{who} &7has not been sent a pack"))),
                    None => reply(msg(th, &format!("&cไม่พบผู้เล่น &f{who} &7(ต้องออนไลน์และเป็น Java)"), &format!("&cNo player &f{who} &7(online Java players only)"))),
                }
            }
            _ if !admin => staff_only(),
            "list" => {
                reply(msg(th, &format!("&fpack ที่ตั้งไว้ {} ชุด", st.cfg.packs.len()), &format!("&f{} pack(s) configured", st.cfg.packs.len())));
                for p in &st.cfg.packs {
                    reply(pack_line(st, p, th));
                }
                for problem in &st.problems {
                    reply(format!("&c {problem}"));
                }
            }
            "reload" => {
                let (cfg, problems) = load();
                st.cfg = cfg;
                st.problems = problems;
                let th = st.cfg.language != "en";
                start_due_hashes(st);
                // Everyone gets what the new settings say (packs that are gone are taken away)
                for p in server.get_all_players() {
                    if st.players.contains_key(&p.get_name().to_ascii_lowercase()) {
                        sync(st, &p, false);
                    }
                }
                reply(msg(th, &format!("&aโหลดไฟล์ตั้งค่าใหม่แล้ว: pack {} ชุด{}", st.cfg.packs.iter().filter(|p| p.enabled).count(), if st.problems.is_empty() { String::new() } else { format!(" &c(มีปัญหา {} จุด ดู /rp list)", st.problems.len()) }), &format!("&aConfiguration reloaded: {} pack(s){}", st.cfg.packs.iter().filter(|p| p.enabled).count(), if st.problems.is_empty() { String::new() } else { format!(" &c({} problem(s), see /rp list)", st.problems.len()) })));
            }
            "hash" => {
                let picked: Vec<Pack> = st.cfg.packs.iter().filter(|p| p.enabled && w.get(1).is_none_or(|n| n.eq_ignore_ascii_case(&p.name))).cloned().collect();
                if picked.is_empty() {
                    reply(msg(th, "&cไม่มี pack ชื่อนั้น &7(ดู /rp list)", "&cNo such pack &7(see /rp list)"));
                    return Ok(0);
                }
                let who = me.as_ref().map(Player::get_name);
                let started = picked.iter().filter(|p| start_hash(st, p, who.clone())).count();
                reply(msg(th, &format!("&7กำลังดาวน์โหลด {started} pack เพื่อคำนวณ SHA-1 จะบอกเมื่อเสร็จ"), &format!("&7Downloading {started} pack(s) to work out the SHA-1; you will be told when done")));
            }
            "send" => {
                let list = targets(w.get(1));
                let pack = w.get(2).map(|n| n.to_ascii_lowercase());
                if list.is_empty() || pack.as_ref().is_some_and(|n| !st.cfg.packs.iter().any(|p| &p.name == n && p.enabled)) {
                    reply(msg(th, "&eวิธีใช้: &f/rp send <ผู้เล่น|all> [pack] &7(ผู้เล่นต้องออนไลน์ และ pack ต้องเปิดอยู่)", "&eUsage: &f/rp send <player|all> [pack] &7(the player must be online and the pack on)"));
                    return Ok(0);
                }
                let mut count = 0;
                for p in &list {
                    match &pack {
                        // One pack, wherever the player is
                        Some(name) => {
                            let Some(one) = st.cfg.packs.iter().find(|x| &x.name == name).cloned() else { continue };
                            if send_pack(st, p, &one) {
                                let tick = st.tick;
                                // (a player who joined a moment ago still gets their own packs when the join delay is over)
                                let world = cfg::world_name(&p.get_world().get_dimension());
                                let seen = st.players.entry(p.get_name().to_ascii_lowercase()).or_insert_with(|| Seen { world, ..Seen::default() });
                                seen.packs.insert(one.name.clone(), (Answer::Other, tick));
                                count += 1;
                            }
                        }
                        None => {
                            if p.as_java().is_some() {
                                sync(st, p, true);
                                count += 1;
                            }
                        }
                    }
                }
                reply(msg(th, &format!("&aส่งให้ {count} คนแล้ว"), &format!("&aSent to {count} player(s)")));
            }
            "clear" => {
                let list = targets(w.get(1));
                if list.is_empty() {
                    reply(msg(th, "&eวิธีใช้: &f/rp clear <ผู้เล่น|all>", "&eUsage: &f/rp clear <player|all>"));
                    return Ok(0);
                }
                let mut count = 0;
                for p in &list {
                    let name = p.get_name().to_ascii_lowercase();
                    let have: Vec<String> = st.players.get(&name).map(|s| s.packs.keys().cloned().collect()).unwrap_or_default();
                    if let Some(java) = p.as_java() {
                        // Only our own packs: another plugin's stay
                        for pack in &have {
                            java.remove_resource_pack(uuid_of(pack));
                        }
                        count += 1;
                    }
                    if let Some(s) = st.players.get_mut(&name) {
                        s.packs.clear();
                        s.due = None;
                    }
                }
                reply(msg(th, &format!("&aถอด resource pack ของเซิร์ฟออกจาก {count} คนแล้ว"), &format!("&aThe server's packs were taken off {count} player(s)")));
            }
            _ => help(th, admin).into_iter().for_each(reply),
        }
        Ok(1)
    }
}

struct ResourcePack;

impl Plugin for ResourcePack {
    fn new() -> Self {
        ResourcePack
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Resource packs by MCSV".into(),
            dependencies: vec![],
            // HTTP and DNS: a pack is downloaded once to work out its SHA-1 (it is not kept)
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into(), HTTP_OUTBOUND.into(), NETWORK_DNS.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let (cfg, problems) = load();
        for problem in &problems {
            c::log_info(problem);
        }
        let hashes: HashMap<String, Hash> = c::load_json("hashes.json").unwrap_or_default();
        let mut st = State { cfg, problems, tick: 0, hashes, jobs: Vec::new(), players: HashMap::new(), sent: 0 };
        start_due_hashes(&mut st);
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(st);
        // The panel reads this to tell "loaded" from "installed, waiting for a restart"
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": c::now_secs() }));
        let _ = context.register_permission(&Permission { node: format!("{NS}:command"), description: "ResourcePack".into(), default: PermissionDefault::Allow, children: vec![] });
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "ResourcePack".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let cmd = Command::new(&["rp".to_owned(), NS.to_owned()], "ResourcePack")
            .execute(Cmd)
            .then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:command"));
        context.register_event_handler(OnStatus, EventPriority::Normal, false)?;
        context.register_event_handler(OnLeave, EventPriority::Normal, false)?;
        pumpkin_plugin_api::scheduler::schedule_repeating_task(STEP, STEP, on_tick);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::remove_file("status.json");
        Ok(())
    }
}

register_plugin!(ResourcePack);

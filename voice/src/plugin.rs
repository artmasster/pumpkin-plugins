//! Pumpkin WASM glue for Voice: the 5 Hz frame, `/voice`, the bridge's answers and talk marks.
//!
//! `config.toml` holds the bridge address, the server id and its key (MCSV's panel writes them;
//! a server outside MCSV gets them at v.mcsv.me/server); the plugin stays idle until all are set.
//! Datagrams go through a UDP socket of WASI (permissions `network.udp` and `network.dns`); the
//! bridge's address is looked up at start and again every ten minutes. A talking player gets a
//! green note riding over their head (a text display; it follows them on the client).
use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA, NETWORK_DNS, NETWORK_UDP};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::wit::pumpkin::plugin::display::{DisplayTransformation, Quaternionf, Vector3f};
use pumpkin_plugin_api::wit::pumpkin::plugin::world::Entity;
use pumpkin_plugin_api::{BillboardMode, Context, EntityDisplayExt, EntityType, Plugin, PluginMetadata, Server, register_plugin};

use crate::defs::{self, Config, Encoder, NS, Row, VERSION};

const ANSWER_SECONDS: f32 = 5.0;
const RESOLVE_EVERY: u64 = 20 * 600;

struct Live {
    key: Vec<u8>,
    sock: UdpSocket,
    addr: Option<SocketAddr>,
    encoder: Encoder,
}

struct State {
    cfg: Config,
    live: Option<Live>,
    /// Players waiting for the bridge's answer to `/voice`.
    pending: HashMap<String, Instant>,
    /// Link tries per player in the last minute.
    tries: HashMap<String, (Instant, u32)>,
    /// The note over each talking player.
    marks: HashMap<String, Entity>,
    update: Option<(String, String)>,
    stamp: Option<SystemTime>,
    tick: u64,
    sent: u64,
    dropped: u64,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn try_lock() -> Option<std::sync::MutexGuard<'static, Option<State>>> {
    match STATE.try_lock() {
        Ok(g) => Some(g),
        Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn id_of(p: &Player) -> String {
    let u = p.get_id();
    defs::player_id(u.high, u.low)
}

fn config_stamp() -> Option<SystemTime> {
    std::fs::metadata(c::data_path("config.toml")).and_then(|m| m.modified()).ok()
}

fn resolve(cfg: &Config) -> Option<SocketAddr> {
    (cfg.bridge_host.as_str(), cfg.bridge_port).to_socket_addrs().ok()?.find(SocketAddr::is_ipv4)
}

fn start(cfg: &Config) -> Option<Live> {
    let key = cfg.key()?;
    let sock = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => {
            c::log_info(&format!("voice is off: no UDP socket ({e}); the server must give this plugin network.udp"));
            return None;
        }
    };
    if let Err(e) = sock.set_nonblocking(true) {
        c::log_info(&format!("voice is off: the UDP socket cannot be non-blocking ({e})"));
        return None;
    }
    let addr = resolve(cfg);
    if addr.is_none() {
        c::log_info(&format!("cannot find {} yet (network.dns); trying again in ten minutes", cfg.bridge_host));
    }
    Some(Live { key, sock, addr, encoder: Encoder::default() })
}

fn send(st: &mut State, gram: &[u8]) {
    let Some(live) = &st.live else { return };
    let Some(addr) = live.addr else {
        st.dropped += 1;
        return;
    };
    // The bridge unreachable or a full buffer: positions are sent again anyway
    if live.sock.send_to(gram, addr).is_ok() {
        st.sent += 1;
    } else {
        st.dropped += 1;
    }
}

/// The note over the players who talk; taken off those who stopped.
fn mark(st: &mut State, server: &Server, talking: &[String]) {
    let stopped: Vec<String> = st.marks.keys().filter(|id| !talking.contains(id)).cloned().collect();
    for id in stopped {
        if let Some(e) = st.marks.remove(&id) {
            e.remove();
        }
    }
    for p in server.get_all_players() {
        let id = id_of(&p);
        if !talking.contains(&id) || st.marks.contains_key(&id) {
            continue;
        }
        let note = p.get_world().spawn_entity(EntityType::TextDisplay, p.get_position());
        c::game::mark(&note, NS);
        if let Some(t) = note.as_text_display() {
            t.set_text(c::color("&a♪"));
            let d = t.get_display();
            d.set_billboard(BillboardMode::Center);
            d.set_transformation(DisplayTransformation {
                translation: Vector3f { x: 0.0, y: 0.75, z: 0.0 },
                scale: Vector3f { x: 1.5, y: 1.5, z: 1.5 },
                left_rotation: Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
                right_rotation: Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
            });
        }
        // Handing it to the rider gives the handle away: it is found again among the passengers
        let note_id = note.get_id();
        let me = p.as_entity();
        me.add_passenger(note);
        if let Some(note) = me.get_passengers().into_iter().find(|e| e.get_id() == note_id) {
            st.marks.insert(id, note);
        }
    }
}

fn answers(st: &mut State, server: &Server) {
    let Some(live) = &st.live else { return };
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    for _ in 0..32 {
        match live.sock.recv_from(&mut buf) {
            Ok((n, from)) if Some(from) == live.addr => got.push(buf[..n].to_vec()),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let (key, srv) = (live.key.clone(), st.cfg.server_id.clone());
    let now = now_ms();
    for data in got {
        if let Some(talk) = defs::open_talk(&key, &srv, &data, now) {
            if st.cfg.talk_indicator {
                mark(st, server, &talk);
            }
            continue;
        }
        if let Some(update) = defs::open_update(&key, &srv, &data, now) {
            if st.update.as_ref() != Some(&update) {
                c::log_info(&format!("a newer voice plugin is out: {} (this one is {VERSION}){}", update.0, if update.1 == "restart" { "; a restart of the server brings it" } else { "; get it at https://v.mcsv.me/server" }));
                st.update = Some(update);
            }
            continue;
        }
        let Some((id, ok, why)) = defs::open_answer(&key, &srv, &data, now) else { continue };
        if st.pending.remove(&id).is_none() {
            continue;
        }
        if let Some(p) = server.get_all_players().into_iter().find(|p| id_of(p) == id) {
            if ok {
                c::tell(&p, "&a[เสียง] เชื่อมแอปแล้ว คุยกับคนใกล้ ๆ ได้เลย");
            } else {
                c::tell(&p, &format!("&c{}", defs::refusal_text(why.as_deref())));
            }
        }
    }
    let late: Vec<String> = st.pending.iter().filter(|(_, at)| at.elapsed().as_secs_f32() > ANSWER_SECONDS).map(|(id, _)| id.clone()).collect();
    for id in late {
        st.pending.remove(&id);
        if let Some(p) = server.get_all_players().into_iter().find(|p| id_of(p) == id) {
            c::tell(&p, "&c[เสียง] ติดต่อระบบเสียงไม่ได้ตอนนี้ ลองใหม่อีกครั้งในอีกสักครู่");
        }
    }
}

fn frame(st: &mut State, server: &Server) {
    let rows: Vec<Row> = server
        .get_all_players()
        .iter()
        .map(|p| {
            let at = p.get_position();
            Row { id: id_of(p), dim: p.get_world().get_dimension(), x: at.0, y: at.1, z: at.2, yaw: f64::from(p.get_yaw()) }
        })
        .collect();
    let Some(live) = st.live.as_mut() else { return };
    let (changed, gone) = live.encoder.frame(rows);
    if changed.is_empty() && gone.is_empty() {
        return;
    }
    let grams = defs::pos_datagrams(&live.key, &st.cfg.server_id, now_ms(), &changed, &gone);
    for g in grams {
        send(st, &g);
    }
}

fn hello(st: &mut State) {
    // The owner's distances, read again when the file changed
    let stamp = config_stamp();
    if stamp.is_some() && stamp != st.stamp {
        st.stamp = stamp;
        let fresh = c::load_config::<Config>("config.toml");
        if fresh.distances() != st.cfg.distances() {
            let (r, f) = fresh.distances();
            c::log_info(&format!("voice range now {r} blocks, full volume up to {f}"));
        }
        st.cfg.voice_range = fresh.voice_range;
        st.cfg.full_volume_range = fresh.full_volume_range;
    }
    let Some(live) = &st.live else { return };
    let (r, f) = st.cfg.distances();
    let gram = defs::hello_datagram(&live.key, &st.cfg.server_id, now_ms(), r, f);
    send(st, &gram);
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.tick += 1;
    if st.live.is_none() {
        return;
    }
    if st.tick % RESOLVE_EVERY == 0 {
        let addr = resolve(&st.cfg);
        if let (Some(live), Some(addr)) = (st.live.as_mut(), addr) {
            live.addr = Some(addr);
        }
    }
    if st.tick % 4 != 0 {
        return;
    }
    answers(st, &server);
    frame(st, &server);
    if st.tick % 200 == 4 {
        hello(st);
    }
}

// ---- commands ----------------------------------------------------------------------

fn voice(st: &mut State, sender: &CommandSender, w: &[String]) -> String {
    let Some(p) = c::sender_player(sender) else { return "Players only".into() };
    let Some(live) = &st.live else { return "&c[เสียง] เซิร์ฟนี้ยังไม่ได้ตั้งค่าเสียงตามระยะ".into() };
    let Some(code) = w.first().and_then(|a| defs::parse_code(a)) else {
        return format!("&e[เสียง] เปิด {} บนมือถือหรือคอม กดเริ่มใช้เสียง แล้วพิมพ์ /voice ตามด้วยรหัส 6 หลัก เช่น /voice 123456", st.cfg.app_url);
    };
    let id = id_of(&p);
    let entry = st.tries.entry(id.clone()).or_insert((Instant::now(), 0));
    if entry.0.elapsed().as_secs() > 60 {
        *entry = (Instant::now(), 0);
    }
    entry.1 += 1;
    if entry.1 > 5 {
        return "&c[เสียง] ลองบ่อยเกินไป รอสักนาทีแล้วลองใหม่".into();
    }
    let gram = defs::link_datagram(&live.key, &st.cfg.server_id, now_ms(), &id, &p.get_name(), &code, &st.cfg.server_name);
    st.pending.insert(id, Instant::now());
    send(st, &gram);
    "&7[เสียง] กำลังเชื่อมแอป...".into()
}

fn moderate(st: &mut State, server: &Server, sender: &CommandSender, on: bool, w: &[String]) -> String {
    let Some(live) = &st.live else { return "&c[เสียง] เซิร์ฟนี้ยังไม่ได้ตั้งค่าเสียงตามระยะ".into() };
    let name = w.first().cloned().unwrap_or_default();
    let Some(target) = server.get_all_players().into_iter().find(|p| p.get_name().eq_ignore_ascii_case(&name)) else { return format!("&c[เสียง] ไม่เจอผู้เล่นชื่อ {name} ในเซิร์ฟตอนนี้") };
    let gram = defs::mute_datagram(&live.key, &st.cfg.server_id, now_ms(), &id_of(&target), on);
    send(st, &gram);
    c::log_info(&format!("voice {} {} by {}", if on { "mute" } else { "unmute" }, target.get_name(), c::sender_key(sender)));
    if on {
        c::tell(&target, "&c[เสียง] ผู้ดูแลเซิร์ฟปิดเสียงคุณ คนอื่นจะไม่ได้ยินคุณ");
        format!("&a[เสียง] ปิดเสียง {} แล้ว คนอื่นจะไม่ได้ยินเขา แต่เขายังได้ยินคนอื่น", target.get_name())
    } else {
        c::tell(&target, "&a[เสียง] ผู้ดูแลเซิร์ฟเปิดเสียงคุณแล้ว");
        format!("&a[เสียง] เปิดเสียง {} แล้ว", target.get_name())
    }
}

#[derive(Clone, Copy)]
enum Verb {
    Voice,
    Mute,
    Unmute,
}

struct Cmd(Verb);
impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let line = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            let Some(st) = guard.as_mut() else { return Ok(0) };
            match (self.0, w.first().map(String::as_str)) {
                (Verb::Voice, Some("status")) if c::has_perm(&sender, &format!("{NS}:mute")) => {
                    format!("Voice {VERSION} &7{} &8| &7sent {} dropped {} &8| &7range {:?}", if st.live.as_ref().and_then(|l| l.addr).is_some() { "connected" } else { "idle" }, st.sent, st.dropped, st.cfg.distances())
                }
                (Verb::Voice, _) => voice(st, &sender, &w),
                (Verb::Mute, _) => moderate(st, &server, &sender, true, &w),
                (Verb::Unmute, _) => moderate(st, &server, &sender, false, &w),
            }
        };
        c::reply(&sender, &line);
        Ok(1)
    }
}

struct Voice;

impl Plugin for Voice {
    fn new() -> Self {
        Voice
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "MCSV proximity voice: positions to the voice bridge, /voice linking".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into(), NETWORK_UDP.into(), NETWORK_DNS.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:use"), description: "Use /voice".into(), default: PermissionDefault::Allow, children: vec![] });
        let _ = context.register_permission(&Permission { node: format!("{NS}:mute"), description: "Mute or unmute a player in voice".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let cfg = c::load_config::<Config>("config.toml");
        let live = start(&cfg);
        match &live {
            Some(l) => {
                let (r, f) = cfg.distances();
                c::log_info(&format!("voice {VERSION} positions to {}:{} ({}) as server {}, range {r} blocks, full volume up to {f}", cfg.bridge_host, cfg.bridge_port, l.addr.map_or_else(|| "not found yet".to_owned(), |a| a.to_string()), cfg.server_id));
            }
            None if cfg.key().is_none() => c::log_info("bridge-host, bridge-port, server-id and server-key are not all set; voice is off"),
            None => {}
        }
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg, live, pending: HashMap::new(), tries: HashMap::new(), marks: HashMap::new(), update: None, stamp: config_stamp(), tick: 0, sent: 0, dropped: 0 });
        let reg = |names: &[&str], verb: Verb, node: &str| {
            let cmd = Command::new(&names.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(), "Voice").execute(Cmd(verb)).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd(verb)));
            context.register_command(cmd, node);
        };
        reg(&["voice"], Verb::Voice, &format!("{NS}:use"));
        reg(&["voicemute"], Verb::Mute, &format!("{NS}:mute"));
        reg(&["voiceunmute"], Verb::Unmute, &format!("{NS}:mute"));
        context.schedule_repeating_task(1, 1, on_tick);
        c::mark_running(VERSION);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::mark_stopped();
        if let Ok(mut guard) = STATE.lock()
            && let Some(st) = guard.as_mut()
        {
            for (_, e) in st.marks.drain() {
                e.remove();
            }
            // Everyone leaves the bridge's room at once
            if let Some(live) = &st.live {
                let gone = live.encoder.known();
                let grams = defs::pos_datagrams(&live.key, &st.cfg.server_id, now_ms(), &[], &gone);
                for g in grams {
                    send(st, &g);
                }
            }
        }
        Ok(())
    }
}

register_plugin!(Voice);

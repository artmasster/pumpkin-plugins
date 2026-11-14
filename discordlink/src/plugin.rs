//! Pumpkin WASM glue for DiscordLink.
//!
//! Everything that touches the network goes through `mcsv_common::http`, which never waits:
//! requests are started from the tick task and their answers picked up on later ticks. One send
//! and one read of the channel are under way at most, so messages stay in order.
use std::sync::Mutex;
use std::time::Duration;

use mcsv_common as c;
use mcsv_common::http::{self, Pending, Request, Response};
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, PlayerChatEvent, PlayerDeathEvent, PlayerJoinEvent, PlayerLeaveEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA, HTTP_OUTBOUND, NETWORK_DNS};
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::wit::pumpkin::plugin::server::CommandSender as ServerSender;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

use crate::bridge::{self, Config, Line, Outbox};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NS: &str = "discordlink";
const PREFIX: &str = "&8[&bDiscordLink&8]&r ";
const AGENT: &str = "DiscordBot (https://pumpkinmc.org, 1.0) DiscordLink";
/// Pause between two messages to Discord (a webhook allows five in two seconds).
const SEND_GAP_MS: u64 = 1100;

struct State {
    cfg: Config,
    outbox: Outbox,
    /// The message under way, how many queued lines it carries, and how often it has failed.
    send: Option<(Pending, usize)>,
    send_failures: u32,
    next_send_ms: u64,
    read: Option<Pending>,
    next_read_ms: u64,
    /// Newest message seen in the channel. `None` until the first look, which shows nothing.
    last_id: Option<u64>,
    sent: u64,
    received: u64,
    /// Messages that arrived without text: the bot lacks the Message Content intent.
    blank: u64,
    last_error: Option<(u64, String)>,
    last_ok_ms: u64,
    greeted: bool,
    /// The command channel: its own read and its own newest message.
    cmd_read: Option<Pending>,
    next_cmd_ms: u64,
    cmd_last_id: Option<u64>,
    commands_run: u64,
    topic: Option<Pending>,
    next_topic_ms: u64,
    last_topic: String,
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

fn fresh(cfg: Config) -> State {
    State {
        cfg,
        outbox: Outbox::default(),
        send: None,
        send_failures: 0,
        next_send_ms: 0,
        read: None,
        next_read_ms: 0,
        last_id: None,
        sent: 0,
        received: 0,
        blank: 0,
        last_error: None,
        last_ok_ms: 0,
        greeted: false,
        cmd_read: None,
        next_cmd_ms: 0,
        cmd_last_id: None,
        commands_run: 0,
        topic: None,
        next_topic_ms: 0,
        last_topic: String::new(),
    }
}

/// Remember a failure for `/discordlink status` and say it once a minute in the console. Never the token.
fn fail(st: &mut State, what: &str) {
    let now = now_ms();
    if st.last_error.as_ref().is_none_or(|(at, old)| now.saturating_sub(*at) >= 60_000 || old != what) {
        c::log_info(&format!("Discord: {what}"));
    }
    st.last_error = Some((now, what.to_owned()));
}

fn explain(r: &Response) -> String {
    let hint = match r.status {
        401 => " (bot token is wrong)",
        403 => " (the bot may not see or write in that channel)",
        404 => " (channel or webhook does not exist)",
        _ => "",
    };
    format!("HTTP {}{hint}", r.status)
}

fn build_send(st: &State) -> Option<(Request, usize)> {
    let webhook = !st.cfg.webhook_url.is_empty();
    let out = st.outbox.next(webhook, &st.cfg.format_to_discord)?;
    let request = if webhook {
        Request::post_json(&st.cfg.webhook_url, bridge::payload(&out, &st.cfg.avatar_url))
    } else {
        Request::post_json(&format!("{}/channels/{}/messages", st.cfg.api_base, st.cfg.channel_id), bridge::payload(&out, ""))
            .header("authorization", &format!("Bot {}", st.cfg.bot_token))
    };
    Some((request.header("user-agent", AGENT), out.lines))
}

/// A message from the channel reaches the game (or answers `!list`).
fn deliver(st: &mut State, server: &Server, m: bridge::Incoming) {
    let th = st.cfg.thai();
    if m.automated {
        return;
    }
    let word = m.text.trim().to_lowercase();
    if st.cfg.list_command && (word == "!list" || word == "!online" || word == "playerlist") {
        let names: Vec<String> = server.get_all_players().iter().map(|p| bridge::to_discord(&p.get_name())).collect();
        let text = if names.is_empty() {
            tr(th, "ตอนนี้ไม่มีผู้เล่นออนไลน์", "Nobody is online right now").to_owned()
        } else {
            format!("{} {} {}: {}", tr(th, "ออนไลน์", "Online:"), names.len(), tr(th, "คน", "players"), names.join(", "))
        };
        st.outbox.push(Line { name: String::new(), text });
        return;
    }
    if !st.cfg.chat_to_game {
        return;
    }
    let mut text = m.text;
    if m.attachments > 0 {
        text = format!("{text} [{} {}]", tr(th, "ไฟล์แนบ", "attachments"), m.attachments).trim().to_owned();
    }
    if text.is_empty() {
        st.blank += 1;
        return;
    }
    st.received += 1;
    let line = st.cfg.format_to_game.replace("{name}", &m.author).replace("{message}", &text);
    for p in server.get_all_players() {
        // A player still at the login prompt is not part of the conversation yet
        if !c::awaiting_login(&p) {
            c::tell(&p, &line);
        }
    }
    c::log_info(&c::plain(&line));
}

/// Every two ticks: pick up answers, start the next request.
fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    let now = now_ms();
    let th = st.cfg.thai();
    if !st.greeted {
        st.greeted = true;
        if st.cfg.start_stop && st.cfg.can_send() {
            st.outbox.push(Line { name: String::new(), text: format!("✅ {}", tr(th, "เซิร์ฟเวอร์เปิดแล้ว", "Server is up")) });
        }
    }

    // Sending
    if let Some((flight, lines)) = st.send.as_mut() {
        if let Some(result) = flight.poll() {
            let lines = *lines;
            st.send = None;
            st.next_send_ms = now + SEND_GAP_MS;
            match result {
                Ok(r) if r.status < 300 => {
                    st.outbox.sent(lines);
                    st.sent += lines as u64;
                    st.send_failures = 0;
                    st.last_ok_ms = now;
                }
                Ok(r) if r.status == 429 => st.next_send_ms = now + bridge::retry_after(&r.body) * 1000,
                Ok(r) => {
                    // Refused for good: these lines will never be accepted, and the next ones should wait
                    st.outbox.sent(lines);
                    st.next_send_ms = now + if matches!(r.status, 401 | 403 | 404) { 30_000 } else { SEND_GAP_MS };
                    fail(st, &format!("send: {}", explain(&r)));
                }
                Err(e) => {
                    st.send_failures += 1;
                    if st.send_failures >= 3 {
                        st.outbox.sent(lines);
                        st.send_failures = 0;
                    }
                    st.next_send_ms = now + 5_000;
                    fail(st, &format!("send: {e}"));
                }
            }
        }
    }
    if st.send.is_none() && now >= st.next_send_ms && st.cfg.can_send() {
        if let Some((request, lines)) = build_send(st) {
            match http::start(request) {
                Ok(flight) => st.send = Some((flight, lines)),
                Err(e) => {
                    st.outbox.sent(lines);
                    st.next_send_ms = now + 5_000;
                    fail(st, &format!("send: {e}"));
                }
            }
        }
    }

    // Reading the channel
    if let Some(flight) = st.read.as_mut() {
        if let Some(result) = flight.poll() {
            st.read = None;
            match result {
                Ok(r) if r.status == 200 => {
                    st.last_ok_ms = now;
                    let first_look = st.last_id.is_none();
                    let mut newest = st.last_id.unwrap_or(0);
                    for m in bridge::parse_messages(&r.body) {
                        if m.id <= newest {
                            continue;
                        }
                        newest = m.id;
                        if !first_look {
                            deliver(st, &server, m);
                        }
                    }
                    st.last_id = Some(newest);
                }
                Ok(r) if r.status == 429 => st.next_read_ms = now + bridge::retry_after(&r.body) * 1000,
                Ok(r) => {
                    st.next_read_ms = now + 60_000;
                    fail(st, &format!("read: {}", explain(&r)));
                }
                Err(e) => {
                    st.next_read_ms = now + 15_000;
                    fail(st, &format!("read: {e}"));
                }
            }
        }
    }
    // Console commands from the command channel
    let mut to_run = Vec::new();
    if let Some(flight) = st.cmd_read.as_mut() {
        if let Some(result) = flight.poll() {
            st.cmd_read = None;
            match result {
                Ok(r) if r.status == 200 => {
                    let first_look = st.cmd_last_id.is_none();
                    let mut newest = st.cmd_last_id.unwrap_or(0);
                    for m in bridge::parse_messages(&r.body) {
                        if m.id <= newest {
                            continue;
                        }
                        newest = m.id;
                        // Only people on the list, by Discord's own id; never bots or webhooks
                        if first_look || m.automated || !st.cfg.command_user_ids.contains(&m.author_id) {
                            continue;
                        }
                        if let Some(command) = bridge::console_command(&m.text) {
                            to_run.push((m.author, command));
                        }
                    }
                    st.cmd_last_id = Some(newest);
                }
                Ok(r) if r.status == 429 => st.next_cmd_ms = now + bridge::retry_after(&r.body) * 1000,
                Ok(r) => {
                    st.next_cmd_ms = now + 60_000;
                    fail(st, &format!("command channel: {}", explain(&r)));
                }
                Err(e) => {
                    st.next_cmd_ms = now + 15_000;
                    fail(st, &format!("command channel: {e}"));
                }
            }
        }
    }
    if st.cmd_read.is_none() && now >= st.next_cmd_ms && st.cfg.can_command() {
        st.next_cmd_ms = now + st.cfg.poll_seconds * 1000;
        let url = match st.cmd_last_id {
            Some(id) => format!("{}/channels/{}/messages?limit=20&after={id}", st.cfg.api_base, st.cfg.command_channel_id),
            None => format!("{}/channels/{}/messages?limit=1", st.cfg.api_base, st.cfg.command_channel_id),
        };
        let request = Request::get(&url).header("authorization", &format!("Bot {}", st.cfg.bot_token)).header("user-agent", AGENT);
        match http::start(request) {
            Ok(flight) => st.cmd_read = Some(flight),
            Err(e) => fail(st, &format!("command channel: {e}")),
        }
    }

    // Channel topic
    if let Some(flight) = st.topic.as_mut() {
        if let Some(result) = flight.poll() {
            st.topic = None;
            match result {
                Ok(r) if r.status < 300 => st.last_ok_ms = now,
                Ok(r) if r.status == 429 => st.next_topic_ms = now + bridge::retry_after(&r.body) * 1000,
                Ok(r) => fail(st, &format!("topic: {}", explain(&r))),
                Err(e) => fail(st, &format!("topic: {e}")),
            }
        }
    }
    if st.topic.is_none() && now >= st.next_topic_ms && !st.cfg.topic.trim().is_empty() && st.cfg.can_read() {
        st.next_topic_ms = now + st.cfg.topic_minutes * 60_000;
        let text: String = st
            .cfg
            .topic
            .replace("{online}", &server.get_player_count().to_string())
            .replace("{max}", &server.get_max_players().to_string())
            .replace("{tps}", &format!("{:.1}", server.get_tps().min(20.0)))
            .chars()
            .take(1000)
            .collect();
        if text != st.last_topic {
            let request = Request {
                method: "PATCH",
                url: format!("{}/channels/{}", st.cfg.api_base, st.cfg.channel_id),
                headers: vec![("content-type".into(), "application/json".into()), ("authorization".into(), format!("Bot {}", st.cfg.bot_token)), ("user-agent".into(), AGENT.into())],
                body: serde_json::json!({ "topic": text }).to_string().into_bytes(),
                timeout: Duration::from_secs(10),
                max_body: http::MAX_BODY,
                digest: false,
            };
            match http::start(request) {
                Ok(flight) => {
                    st.topic = Some(flight);
                    st.last_topic = text;
                }
                Err(e) => fail(st, &format!("topic: {e}")),
            }
        }
    }

    // Run what the command channel asked for, after our state is free again: a command can
    // fire events that come back into this plugin
    st.commands_run += to_run.len() as u64;
    drop(guard);
    for (who, command) in to_run {
        c::log_info(&format!("Discord command channel, {who}: /{command}"));
        server.execute_command(&command, ServerSender::Console);
    }
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    if st.read.is_none() && now >= st.next_read_ms && st.cfg.can_read() && (st.cfg.chat_to_game || st.cfg.list_command) {
        st.next_read_ms = now + st.cfg.poll_seconds * 1000;
        let url = match st.last_id {
            Some(id) => format!("{}/channels/{}/messages?limit=50&after={id}", st.cfg.api_base, st.cfg.channel_id),
            None => format!("{}/channels/{}/messages?limit=1", st.cfg.api_base, st.cfg.channel_id),
        };
        let request = Request::get(&url).header("authorization", &format!("Bot {}", st.cfg.bot_token)).header("user-agent", AGENT);
        match http::start(request) {
            Ok(flight) => st.read = Some(flight),
            Err(e) => fail(st, &format!("read: {e}")),
        }
    }
}

fn problem_text(th: bool, key: &str) -> &'static str {
    match key {
        "bad-channel" => tr(th, "channel-id ต้องเป็นตัวเลขล้วน", "channel-id must be digits only"),
        "bad-webhook" => tr(th, "webhook-url ต้องขึ้นต้นด้วย https://discord.com/api/webhooks/", "webhook-url must start with https://discord.com/api/webhooks/"),
        "need-both" => tr(th, "การอ่านห้องต้องมีทั้ง bot-token และ channel-id", "reading the channel needs both bot-token and channel-id"),
        _ => tr(th, "ยังไม่ได้ตั้งค่า ใส่ webhook-url หรือ bot-token กับ channel-id ใน plugins/data/discordlink/config.toml", "not set up: put webhook-url, or bot-token and channel-id, in plugins/data/discordlink/config.toml"),
    }
}

fn admin(st: &mut State, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    match w.first().map_or("", String::as_str) {
        "status" => {
            let now = now_ms();
            let ago = |at: u64| c::hms(now.saturating_sub(at) / 1000);
            let mut out = vec![format!(
                "&7{}: &f{} &7· {}: &f{}",
                tr(th, "ส่งเข้า Discord", "to Discord"),
                if !st.cfg.webhook_url.is_empty() { "webhook" } else if st.cfg.can_read() { "bot" } else { tr(th, "ปิด", "off") },
                tr(th, "อ่านจาก Discord", "from Discord"),
                if st.cfg.can_read() { tr(th, "เปิด", "on") } else { tr(th, "ปิด", "off") }
            )];
            out.push(format!(
                "&7{} &f{} &7· {} &f{} &7· {} &f{} &7· {} &f{}",
                tr(th, "ส่งแล้ว", "sent"), st.sent, tr(th, "รับแล้ว", "received"), st.received,
                tr(th, "รอส่ง", "waiting"), st.outbox.len(), tr(th, "ทิ้งเพราะคิวเต็ม", "dropped"), st.outbox.dropped
            ));
            if st.last_ok_ms > 0 {
                out.push(format!("&7{} &f{}", tr(th, "ติดต่อ Discord สำเร็จล่าสุดเมื่อ", "last answer from Discord"), ago(st.last_ok_ms)));
            }
            if let Some((at, what)) = &st.last_error {
                out.push(format!("&c{} ({}): &f{what}", tr(th, "ปัญหาล่าสุด", "last problem"), ago(*at)));
            }
            if st.cfg.can_command() {
                out.push(format!("&7{} &f{} &7· {} &f{}", tr(th, "ห้องคำสั่ง: ผู้ที่สั่งได้", "command channel: people allowed"), st.cfg.command_user_ids.len(), tr(th, "คำสั่งที่รันแล้ว", "commands run"), st.commands_run));
            }
            if st.blank > 0 {
                out.push(format!("&e{}", tr(th, "ข้อความจาก Discord มาแบบไม่มีเนื้อหา: เปิด Message Content Intent ของบอทในหน้า Developer Portal", "Messages arrive without text: turn on the bot's Message Content Intent in the Developer Portal")));
            }
            out.extend(st.cfg.problems().into_iter().map(|k| format!("&e{}", problem_text(th, k))));
            out
        }
        "reload" => {
            let old_queue = std::mem::take(&mut st.outbox);
            let (sent, received) = (st.sent, st.received);
            *st = fresh(load());
            st.outbox = old_queue;
            st.sent = sent;
            st.received = received;
            st.greeted = true;
            let th = st.cfg.thai();
            let mut out = vec![format!("&a{}", tr(th, "โหลดไฟล์ตั้งค่าใหม่แล้ว", "Configuration reloaded"))];
            out.extend(st.cfg.problems().into_iter().map(|k| format!("&e{}", problem_text(th, k))));
            out
        }
        "test" => {
            if !st.cfg.can_send() {
                return vec![format!("&e{}", problem_text(th, "nothing-set"))];
            }
            st.outbox.push(Line { name: String::new(), text: format!("🔧 {}", tr(th, "ข้อความทดสอบจาก DiscordLink", "Test message from DiscordLink")) });
            vec![format!("&a{}", tr(th, "เข้าคิวแล้ว ดูผลด้วย /discordlink status", "Queued. See /discordlink status for the outcome"))]
        }
        "version" => vec![format!("&fDiscordLink {VERSION}")],
        _ => vec!["&eDiscordLink: &f/discordlink status &7| &ftest &7| &freload &7| &fversion".to_owned()],
    }
}

struct Admin;
impl CommandHandler for Admin {
    fn handle(&self, sender: CommandSender, _server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let lines = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            let Some(st) = guard.as_mut() else { return Ok(0) };
            admin(st, &w)
        };
        for line in lines {
            c::reply(&sender, &format!("{PREFIX}{line}"));
        }
        Ok(1)
    }
}

/// `/discord`: the invite link.
struct Invite;
impl CommandHandler for Invite {
    fn handle(&self, sender: CommandSender, _server: Server, _args: ConsumedArgs) -> Result<i32, CommandError> {
        let (th, invite) = match try_lock().as_ref().and_then(|g| g.as_ref().map(|st| (st.cfg.thai(), st.cfg.invite.clone()))) {
            Some(v) => v,
            None => return Ok(0),
        };
        let line = if invite.trim().is_empty() {
            format!("&7{}", tr(th, "เซิร์ฟนี้ยังไม่ได้ใส่ลิงก์ Discord", "This server has not set a Discord link"))
        } else {
            format!("&9Discord: &f{}", bridge::strip_codes(invite.trim()))
        };
        c::reply(&sender, &line);
        Ok(1)
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

fn queue(line: Line, wanted: fn(&Config) -> bool) {
    if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
        if wanted(&st.cfg) && st.cfg.can_send() {
            st.outbox.push(line);
        }
    }
}

/// Not blocking: runs after every plugin that may cancel, so `cancelled` is final. A chat plugin
/// that sends the line itself (Basics) cancels the event and tells us over IPC instead.
struct OnChat;
impl EventHandler<PlayerChatEvent> for OnChat {
    fn handle(&self, _server: Server, e: Data<PlayerChatEvent>) -> Data<PlayerChatEvent> {
        if !e.cancelled && !c::awaiting_login(&e.player) {
            queue(Line { name: e.player.get_name(), text: bridge::to_discord(&e.message) }, |cfg| cfg.chat_to_discord);
        }
        e
    }
}

struct OnJoin;
impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _server: Server, e: Data<PlayerJoinEvent>) -> Data<PlayerJoinEvent> {
        if !e.cancelled {
            let th = try_lock().as_ref().and_then(|g| g.as_ref().map(|st| st.cfg.thai())).unwrap_or(true);
            let text = format!("📥 **{}** {}", bridge::to_discord(&e.player.get_name()), tr(th, "เข้าเกม", "joined"));
            queue(Line { name: String::new(), text }, |cfg| cfg.join_leave);
        }
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, e: Data<PlayerLeaveEvent>) -> Data<PlayerLeaveEvent> {
        let th = try_lock().as_ref().and_then(|g| g.as_ref().map(|st| st.cfg.thai())).unwrap_or(true);
        let text = format!("📤 **{}** {}", bridge::to_discord(&e.player.get_name()), tr(th, "ออกจากเกม", "left"));
        queue(Line { name: String::new(), text }, |cfg| cfg.join_leave);
        e
    }
}

struct OnDeath;
impl EventHandler<PlayerDeathEvent> for OnDeath {
    fn handle(&self, _server: Server, e: Data<PlayerDeathEvent>) -> Data<PlayerDeathEvent> {
        if !e.cancelled {
            // The console form of the death message is plain English text with colour escapes
            let shown = e.death_message.to_pretty_console();
            let mut plain = String::new();
            let mut it = shown.chars();
            while let Some(ch) = it.next() {
                if ch == '\u{1b}' {
                    for end in it.by_ref() {
                        if end.is_ascii_alphabetic() {
                            break;
                        }
                    }
                } else if !ch.is_control() {
                    plain.push(ch);
                }
            }
            let name = e.player.get_name();
            let text = if plain.trim().is_empty() || !plain.contains(&name) { format!("**{}** died", bridge::to_discord(&name)) } else { bridge::to_discord(plain.trim()) };
            queue(Line { name: String::new(), text: format!("💀 {text}") }, |cfg| cfg.deaths);
        }
        e
    }
}

struct DiscordLink;

impl Plugin for DiscordLink {
    fn new() -> Self {
        DiscordLink
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Discord bridge by MCSV".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into(), HTTP_OUTBOUND.into(), NETWORK_DNS.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission {
            node: format!("{NS}:admin"),
            description: "DiscordLink".into(),
            default: PermissionDefault::Op(PermissionLevel::Three),
            children: vec![],
        });
        let _ = context.register_permission(&Permission {
            node: format!("{NS}:discord"),
            description: "DiscordLink".into(),
            default: PermissionDefault::Allow,
            children: vec![],
        });
        let cfg = load();
        let th = cfg.thai();
        for k in cfg.problems() {
            c::log_info(problem_text(th, k));
        }
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fresh(cfg));
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": c::now_secs() }));

        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let cmd = Command::new(&[NS.to_owned()], "DiscordLink")
            .execute(Admin)
            .then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Admin));
        context.register_command(cmd, &format!("{NS}:admin"));
        context.register_command(Command::new(&["discord".to_owned()], "Discord").execute(Invite), &format!("{NS}:discord"));
        context.register_event_handler(OnChat, EventPriority::Lowest, false)?;
        context.register_event_handler(OnJoin, EventPriority::Lowest, false)?;
        context.register_event_handler(OnLeave, EventPriority::Lowest, false)?;
        context.register_event_handler(OnDeath, EventPriority::Lowest, false)?;
        context.schedule_repeating_task(40, 2, on_tick);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        // The one place we wait for the network: the server is stopping anyway
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            if st.cfg.start_stop && st.cfg.can_send() {
                st.send = None;
                let th = st.cfg.thai();
                st.outbox.push(Line { name: String::new(), text: format!("🛑 {}", tr(th, "เซิร์ฟเวอร์ปิดแล้ว", "Server stopped")) });
                // Whatever is still queued goes out in one message, the notice included
                let lines: Vec<String> = std::iter::from_fn(|| {
                    let out = st.outbox.next(false, &st.cfg.format_to_discord)?;
                    st.outbox.sent(out.lines);
                    Some(out.content)
                })
                .collect();
                let text: String = lines.join("\n").chars().rev().take(1900).collect::<Vec<_>>().into_iter().rev().collect();
                st.outbox.push(Line { name: String::new(), text });
                if let Some((request, _)) = build_send(st) {
                    if let Ok(flight) = http::start(request) {
                        let _ = flight.wait(Duration::from_secs(3));
                    }
                }
            }
        }
        c::remove_file("status.json");
        Ok(())
    }

    /// Other MCSV plugins hand us lines they sent themselves: `{"kind":"chat","name":…,"message":…}`
    /// for chat (Basics formats and sends chat itself, so the chat event we see is cancelled) and
    /// `{"kind":"notice","text":…}` for anything else worth showing.
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let field = |k: &str| v.get(k).and_then(serde_json::Value::as_str).unwrap_or("").chars().take(1000).collect::<String>();
        match field("kind").as_str() {
            "chat" => queue(Line { name: field("name"), text: bridge::to_discord(&field("message")) }, |cfg| cfg.chat_to_discord),
            "notice" => queue(Line { name: String::new(), text: bridge::to_discord(&field("text")) }, |_| true),
            other => return Err(format!("unknown kind {other}")),
        }
        Ok(b"{\"ok\":true}".to_vec())
    }
}

register_plugin!(DiscordLink);

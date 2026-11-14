//! Basics second set: ranks and permissions, chat (format, mute, ignore, spy), AFK, player info,
//! mail, kits and random teleport. `plugin.rs` owns the state, events and the first set.
use std::time::{Duration, Instant};

use mcsv_common as c;
use pumpkin_plugin_api::command::CommandSender;
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::text::TextComponent;
use pumpkin_plugin_api::{ItemStack, Server};

use crate::lang::{fmt, text};
use crate::plugin::{
    Afk, Rtp, State, TP_GAP, dyn_perm, find_player, key, loc_of, now_secs, on_off, perm, player_perm, teleport_now, uuid_of,
    world_of,
};
use crate::store::{self, Loc, Mail, PlayerData};

const OVERWORLD: &str = "minecraft:overworld";
/// Lowest block of the overworld: `get_top_block_y` answers this while the chunk is not loaded.
const OVERWORLD_MIN_Y: i32 = -64;

// ---- ranks ------------------------------------------------------------------

/// Give the player the nodes of their rank plus their own, taking back what we set before.
/// Pumpkin keeps these in memory only, so this runs on every join and after every change.
pub(crate) fn apply_perms(st: &mut State, p: &Player) {
    let k = key(&p.get_id());
    for node in st.applied.remove(&k).unwrap_or_default() {
        p.unset_permission(&node);
    }
    let d = st.data(p).clone();
    let group = st.ranks.group_of(d.group.as_deref()).to_owned();
    let mut nodes = st.ranks.resolve(&group);
    // A player's own nodes saved before the 0.6.0 rename carry the old plugin id
    nodes.extend(d.perms.into_iter().map(|(node, value)| (crate::store::modern_node(&node), value)));
    for (node, value) in &nodes {
        p.set_permission(node, *value);
    }
    st.applied.insert(k, nodes.into_keys().collect());
}

fn shown_name(st: &mut State, p: &Player) -> String {
    st.data(p).nick.clone().unwrap_or_else(|| p.get_name())
}

/// Display name and tab list name: rank prefix, nickname, AFK mark.
pub(crate) fn refresh_name(st: &mut State, p: &Player) {
    let k = key(&p.get_id());
    let d = st.data(p).clone();
    let group = st.ranks.group_of(d.group.as_deref()).to_owned();
    // A prefix from Perms (the full permissions plugin) wins over the rank's own
    let (prefix, suffix) = c::perms_decor(p).unwrap_or_else(|| {
        let (a, b) = st.ranks.decor(&group);
        (a.to_owned(), b.to_owned())
    });
    let shown = d.nick.clone().unwrap_or_else(|| p.get_name());
    let afk = st.afk.get(&k).is_some_and(|a| a.afk);
    let plain = prefix.is_empty() && suffix.is_empty() && d.nick.is_none();
    p.set_display_name(c::color(&format!("{prefix}{shown}{suffix}")));
    if plain && !afk {
        p.set_tab_list_name(None);
    } else {
        let mark = if afk { " &7[AFK]" } else { "" };
        p.set_tab_list_name(Some(c::color(&format!("{prefix}{shown}{suffix}{mark}"))));
    }
}

// ---- join / leave -----------------------------------------------------------

pub(crate) fn on_join(st: &mut State, p: &Player) {
    apply_perms(st, p);
    refresh_name(st, p);
}

/// What a player sees on joining, once a login prompt is behind them: the motd and the mail
/// notice. Their AFK clock starts here too.
pub(crate) fn welcome(st: &mut State, p: &Player) {
    let k = key(&p.get_id());
    st.afk.insert(k, Afk { pos: p.get_position(), yaw: p.get_yaw(), since: Instant::now(), afk: false });
    let l = st.cfg.language.clone();
    let vars = [("name", p.get_name())];
    for line in st.cfg.motd.clone() {
        c::tell(p, &store::fill(&line, &vars));
    }
    let mail = st.data(p).mail.len();
    if mail > 0 {
        c::tell(p, &fmt(&l, "mail_new", &[("count", mail.to_string())]));
    }
}

pub(crate) fn on_leave(st: &mut State, p: &Player) {
    let k = key(&p.get_id());
    for node in st.applied.remove(&k).unwrap_or_default() {
        p.unset_permission(&node);
    }
    st.afk.remove(&k);
    if st.rtp.remove(&k).is_some() | st.protect.remove(&k).is_some() {
        let god = st.data(p).god;
        p.set_invulnerable(god);
    }
}

// ---- chat -------------------------------------------------------------------

/// `format` with its placeholders filled and the message put where `{message}` is. The message is
/// literal text unless `colored`, so players without `basics:chat.color` cannot use `&` codes.
pub(crate) fn chat_line(format: &str, vars: &[(&str, String)], msg: &str, colored: bool) -> TextComponent {
    let (head, tail) = format.split_once("{message}").unwrap_or((format, ""));
    let line = c::color(&store::fill(head, vars)).add_child(if colored { c::color(msg) } else { TextComponent::text(msg) });
    if tail.is_empty() { line } else { line.add_child(c::color(&store::fill(tail, vars))) }
}

pub(crate) fn mute_notice(l: &str, d: &PlayerData) -> String {
    let th = l != "en";
    let left = if d.muted_until == u64::MAX {
        String::new()
    } else {
        store::fill(text(l, "mute_left"), &[("time", store::fmt_duration(d.muted_until.saturating_sub(now_secs()), th))])
    };
    let reason = if d.mute_reason.is_empty() {
        String::new()
    } else {
        store::fill(text(l, "mute_reason"), &[("reason", d.mute_reason.clone())])
    };
    fmt(l, "muted", &[("left", left), ("reason", reason)])
}

/// Show a private message to staff who switched /socialspy on.
pub(crate) fn social_spy(st: &State, server: &Server, from: &str, to: &Player, msg: &str) {
    let l = st.cfg.language.as_str();
    let to_name = to.get_name();
    let vars = [("from", from.to_owned()), ("to", to_name.clone())];
    for p in server.get_all_players() {
        let name = p.get_name();
        if name == from || name == to_name {
            continue;
        }
        if st.players.get(&key(&p.get_id())).is_some_and(|d| d.socialspy) && player_perm(&p, "command.socialspy") {
            p.send_system_message(chat_line(text(l, "spy"), &vars, msg, false), false);
        }
    }
}

/// Public chat. Returns true when Basics handled the line (the server must not send its own).
pub(crate) fn on_chat(st: &mut State, server: &Server, p: &Player, message: &str) -> bool {
    // Not logged in yet (Login): the line may be a password. Leave it to Login to cancel.
    if c::awaiting_login(p) {
        return false;
    }
    let l = st.cfg.language.clone();
    let k = key(&p.get_id());
    let d = st.data(p).clone();
    if d.muted(now_secs()) {
        c::tell(p, &mute_notice(&l, &d));
        return true;
    }
    if st.chat_locked && !player_perm(p, "command.mutechat") {
        c::tell(p, &fmt(&l, "chat_is_locked", &[]));
        return true;
    }
    active(st, server, p);
    let format = st.cfg.chat_format.clone();
    if format.trim().is_empty() {
        return false;
    }
    let group = st.ranks.group_of(d.group.as_deref()).to_owned();
    let (prefix, suffix) = c::perms_decor(p).unwrap_or_else(|| {
        let (a, b) = st.ranks.decor(&group);
        (a.to_owned(), b.to_owned())
    });
    let real = p.get_name();
    let vars = [
        ("prefix", prefix),
        ("suffix", suffix),
        ("name", d.nick.clone().unwrap_or_else(|| real.clone())),
        ("player", real.clone()),
    ];
    let colored = player_perm(p, "chat.color");
    for r in server.get_all_players() {
        let rk = key(&r.get_id());
        if rk != k && st.players.get(&rk).is_some_and(|rd| rd.ignored.contains(&k)) {
            continue;
        }
        // Someone still at the login prompt is not in the conversation yet
        if rk != k && c::awaiting_login(&r) {
            continue;
        }
        r.send_system_message(chat_line(&format, &vars, message, colored), false);
    }
    c::log_info(&format!("<chat> {real}: {message}"));
    // We cancel the game's chat event, so a Discord bridge never sees this line: hand it over.
    // Nothing happens when DiscordLink is not installed.
    let notice = serde_json::json!({ "kind": "chat", "name": real, "message": message });
    let _ = c::ipc("discordlink", &notice);
    // Same for the block and chat log: its own chat handler only sees lines nobody cancelled
    let _ = c::ipc("blocklog", &notice);
    true
}

// ---- AFK and timers ---------------------------------------------------------

fn announce_afk(st: &mut State, server: &Server, p: &Player, on: bool) {
    let l = st.cfg.language.clone();
    let name = shown_name(st, p);
    let line = store::fill(text(&l, if on { "afk_on" } else { "afk_off" }), &[("name", name)]);
    if st.cfg.afk_broadcast {
        for r in server.get_all_players() {
            c::tell(&r, &line);
        }
    } else {
        c::tell(p, &line);
    }
    refresh_name(st, p);
}

/// The player did something: restart their idle clock and end AFK.
pub(crate) fn active(st: &mut State, server: &Server, p: &Player) {
    let k = key(&p.get_id());
    let was = st.afk.get(&k).is_some_and(|a| a.afk);
    st.afk.insert(k, Afk { pos: p.get_position(), yaw: p.get_yaw(), since: Instant::now(), afk: false });
    if was {
        announce_afk(st, server, p, false);
    }
}

/// Once a second: AFK by position, the end of /rtp protection, announcements.
pub(crate) fn tick_second(st: &mut State, server: &Server) {
    let auto = st.cfg.afk_auto;
    let players = server.get_all_players();
    for p in &players {
        let k = key(&p.get_id());
        // Still at a login prompt: no AFK clock yet
        if st.joining.contains_key(&k) {
            continue;
        }
        let (pos, yaw) = (p.get_position(), p.get_yaw());
        let Some((old, old_yaw, idle, afk)) = st.afk.get(&k).map(|a| (a.pos, a.yaw, a.since.elapsed().as_secs(), a.afk)) else {
            st.afk.insert(k, Afk { pos, yaw, since: Instant::now(), afk: false });
            continue;
        };
        let moved = (old.0 - pos.0).powi(2) + (old.1 - pos.1).powi(2) + (old.2 - pos.2).powi(2) > 0.01 || (old_yaw - yaw).abs() > 1.0;
        if moved {
            st.afk.insert(k, Afk { pos, yaw, since: Instant::now(), afk: false });
            if afk {
                announce_afk(st, server, p, false);
            }
        } else if !afk && auto > 0 && idle >= auto {
            if let Some(a) = st.afk.get_mut(&k) {
                a.afk = true;
            }
            announce_afk(st, server, p, true);
        }
    }

    let now = Instant::now();
    let over: Vec<String> = st.protect.iter().filter(|(_, until)| now >= **until).map(|(k, _)| k.clone()).collect();
    for k in over {
        st.protect.remove(&k);
        if let Some(p) = uuid_of(&k).and_then(|u| server.get_player_by_uuid(u)) {
            let god = st.data(&p).god;
            p.set_invulnerable(god);
        }
    }

    crate::extra::tick_second(st, server, &players);
    crate::plus::tick_second(st, server, &players);
    // Cooldown clocks outlive a relog (leaving used to reset them); drop the stale ones here
    if st.ticks % 600 == 0 {
        st.last_rtp.retain(|_, t| t.elapsed().as_secs() < 86_400);
        st.last_tp.retain(|_, t| t.elapsed().as_secs() < 86_400);
    }

    let every = st.cfg.announce_interval;
    if every > 0 && !st.cfg.announcements.is_empty() && !players.is_empty() && st.last_announce.elapsed().as_secs() >= every {
        st.last_announce = now;
        let line = st.cfg.announcements[st.announce_idx % st.cfg.announcements.len()].clone();
        st.announce_idx = st.announce_idx.wrapping_add(1);
        if st.cfg.announce_bossbar {
            crate::plus::announce_bar(st, server, &players, &line);
        } else {
            for p in &players {
                c::tell(p, &line);
            }
        }
    }
}

// ---- random teleport --------------------------------------------------------

fn rand01(st: &mut State) -> f64 {
    // xorshift64
    let mut x = st.seed;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    st.seed = x;
    (x >> 11) as f64 / (1u64 << 53) as f64
}

fn start_rtp(st: &mut State, server: &Server, p: &Player) -> Vec<String> {
    let l = st.cfg.language.clone();
    let k = key(&p.get_id());
    if st.rtp.contains_key(&k) {
        return vec![fmt(&l, "rtp_wait", &[])];
    }
    let origin = loc_of(p);
    if origin.dim != OVERWORLD {
        return vec![fmt(&l, "rtp_world", &[])];
    }
    let cooldown = st.cfg.rtp_cooldown;
    if cooldown > 0 && !player_perm(p, "bypass.cooldown") {
        let left = st.last_rtp.get(&k).map_or(0, |t| cooldown.saturating_sub(t.elapsed().as_secs()));
        if left > 0 {
            return vec![fmt(&l, "tp_cooldown", &[("sec", left.to_string())])];
        }
    }
    let Some(world) = world_of(server, OVERWORLD) else { return vec![fmt(&l, "no_world", &[])] };
    let (x, z) = rtp_point(st);
    st.data(p).back = Some(origin.clone());
    st.dirty.insert(k.clone());
    // High above the target: the chunk loads around the player, then `tick_rtp` puts them on the ground
    p.set_invulnerable(true);
    p.teleport_world(world, (f64::from(x) + 0.5, 320.0, f64::from(z) + 0.5), None, None);
    st.tp_guard.insert(k.clone(), Instant::now());
    st.rtp.insert(k, Rtp { x, z, started: Instant::now(), origin, tries: 0 });
    vec![fmt(&l, "rtp_wait", &[])]
}

/// A random column in the ring `rtp-min-radius`..`rtp-max-radius` around the spawn.
fn rtp_point(st: &mut State) -> (i32, i32) {
    let (cx, cz) = st.spawn.as_ref().filter(|s| s.dim == OVERWORLD).map_or((0.0, 0.0), |s| (s.x, s.z));
    let (min, max) = (f64::from(st.cfg.rtp_min_radius), f64::from(st.cfg.rtp_max_radius.max(st.cfg.rtp_min_radius + 16)));
    let r = min + rand01(st) * (max - min);
    let a = rand01(st) * std::f64::consts::TAU;
    ((cx + r * a.cos()).floor() as i32, (cz + r * a.sin()).floor() as i32)
}

/// Random points one /rtp may try before it gives up.
const RTP_TRIES: u32 = 4;

/// The block a player would stand on in a column, and its height. `None` = chunk not loaded.
fn ground(world: &pumpkin_plugin_api::world::World, x: i32, z: i32) -> Option<(i32, String)> {
    use pumpkin_plugin_api::common::BlockPos;
    let top = world.get_top_block_y(x, z);
    if top <= OVERWORLD_MIN_Y {
        return None;
    }
    // The height map may name the highest block or the air above it
    let name = world.get_block(BlockPos { x, y: top, z }).name;
    if name.ends_with("air") {
        return Some((top - 1, world.get_block(BlockPos { x, y: top - 1, z }).name));
    }
    Some((top, name))
}

/// Finish random teleports whose chunk is loaded; give up after 15 seconds.
pub(crate) fn tick_rtp(st: &mut State, server: &Server) {
    let l = st.cfg.language.clone();
    let keys: Vec<String> = st.rtp.keys().cloned().collect();
    for k in keys {
        let Some(p) = uuid_of(&k).and_then(|u| server.get_player_by_uuid(u)) else {
            st.rtp.remove(&k);
            continue;
        };
        let Some((x, z, waited)) = st.rtp.get(&k).map(|r| (r.x, r.z, r.started.elapsed())) else { continue };
        // The second teleport waits for the client to confirm the first (see `State::tp_guard`)
        if waited < TP_GAP + Duration::from_millis(500) {
            continue;
        }
        let waited = waited.as_secs();
        let Some(world) = world_of(server, OVERWORLD) else { continue };
        let mut hopeless = false;
        if world.get_top_block_y(x, z) > OVERWORLD_MIN_Y {
            // The chunk under the player is there: look for ground that is not water, lava or
            // another bad landing, here and in the loaded columns around
            let spots: Vec<(i32, i32, Option<(i32, String)>)> = store::rtp_offsets().into_iter().map(|(dx, dz)| (x + dx, z + dz, ground(&world, x + dx, z + dz))).collect();
            let names: Vec<Option<&str>> = spots.iter().map(|(_, _, g)| g.as_ref().map(|(_, n)| n.as_str())).collect();
            let found = store::pick_ground(&names).and_then(|i| spots[i].2.as_ref().map(|(y, n)| (spots[i].0, *y, spots[i].1, n.clone())));
            if let Some((gx, gy, gz, on)) = found {
                st.rtp.remove(&k);
                c::log_info(&format!("rtp {} -> {gx} {} {gz} on {on}", p.get_name(), gy + 1));
                p.teleport_world(world, (f64::from(gx) + 0.5, f64::from(gy) + 1.0, f64::from(gz) + 0.5), None, None);
                st.tp_guard.insert(k.clone(), Instant::now());
                st.protect.insert(k.clone(), Instant::now() + Duration::from_secs(5));
                st.last_rtp.insert(k, Instant::now());
                c::tell(&p, &fmt(&l, "rtp_done", &[("x", gx.to_string()), ("z", gz.to_string())]));
                continue;
            }
            let tries = st.rtp.get(&k).map_or(RTP_TRIES, |r| r.tries + 1);
            if tries < RTP_TRIES {
                // Sea or lava all around: fly to another random point and look again
                let (nx, nz) = rtp_point(st);
                p.teleport_world(world, (f64::from(nx) + 0.5, 320.0, f64::from(nz) + 0.5), None, None);
                st.tp_guard.insert(k.clone(), Instant::now());
                if let Some(r) = st.rtp.get_mut(&k) {
                    (r.x, r.z, r.tries, r.started) = (nx, nz, tries, Instant::now());
                }
                continue;
            }
            hopeless = true;
        }
        if waited >= 15 || hopeless {
            if let Some(r) = st.rtp.remove(&k) {
                c::log_info(&format!("rtp {}: no safe ground after {} places", p.get_name(), r.tries + 1));
                let god = st.data(&p).god;
                p.set_invulnerable(god);
                p.teleport_world(world, (r.origin.x, r.origin.y, r.origin.z), Some(r.origin.yaw), Some(r.origin.pitch));
                st.tp_guard.insert(k.clone(), Instant::now());
                c::tell(&p, &fmt(&l, "rtp_fail", &[]));
            }
        }
    }
}

// ---- kits -------------------------------------------------------------------

fn empty_slot(p: &Player, slot: u8) -> bool {
    match p.get_inventory_item(slot) {
        None => true,
        Some(s) => s.get_count() == 0 || s.get_registry_key().ends_with("air"),
    }
}

/// Put the kit into free main-inventory slots. Returns how many stacks did not fit or were unknown.
pub(crate) fn give_kit(p: &Player, items: &[String]) -> usize {
    let mut free: Vec<u8> = (0u8..36).filter(|s| empty_slot(p, *s)).collect();
    free.reverse();
    let mut missed = 0;
    for line in items {
        let Some((id, count)) = store::parse_item(line) else {
            tracing::warn!("kit item not understood: {line}");
            missed += 1;
            continue;
        };
        let stack = ItemStack::new(&id, count);
        if stack.get_registry_key().ends_with("air") {
            tracing::warn!("kit item unknown to this Minecraft version: {id}");
            missed += 1;
            continue;
        }
        match free.pop() {
            Some(slot) => p.set_inventory_item(slot, Some(stack)),
            None => missed += 1,
        }
    }
    missed
}

// ---- commands ---------------------------------------------------------------

fn dim_of(name: &str) -> Option<&'static str> {
    Some(match name.to_lowercase().trim_start_matches("minecraft:") {
        "world" | "overworld" => "minecraft:overworld",
        "world_nether" | "nether" | "the_nether" => "minecraft:the_nether",
        "world_the_end" | "end" | "the_end" => "minecraft:the_end",
        _ => return None,
    })
}

pub(crate) fn run(name: &str, st: &mut State, sender: &CommandSender, server: &Server, w: &[String]) -> Vec<String> {
    let lang = st.cfg.language.clone();
    let l = lang.as_str();
    let th = l != "en";
    let say = |k: &str, v: &[(&str, String)]| vec![fmt(l, k, v)];
    let me = c::sender_player(sender);
    let arg = |i: usize| w.get(i).map(String::as_str);
    let rest = |i: usize| w.get(i..).map(|s| s.join(" ")).unwrap_or_default();
    let none = || text(l, "none").to_owned();
    let list_or_none = |v: Vec<String>| if v.is_empty() { none() } else { v.join(", ") };
    macro_rules! need_player {
        () => {
            match c::sender_player(sender) {
                Some(p) => p,
                None => return say("player_only", &[]),
            }
        };
    }
    // A known player by name (online or not), or the usage line
    macro_rules! known {
        ($i:expr, $usage:expr) => {{
            let Some(n) = arg($i) else { return say("usage", &[("arg", $usage.into())]) };
            match st.key_by_name(server, n) {
                Some(k) => k,
                None => return say("no_player", &[("arg", n.to_owned())]),
            }
        }};
    }
    let online = |k: &str| uuid_of(k).and_then(|u| server.get_player_by_uuid(u));
    let name_of = |st: &State, k: &str| st.players.get(k).map_or_else(String::new, |d| d.name.clone());

    match name {
        // -- ranks and permissions
        "rank" => {
            // The console has no rank of its own: a bare `rank` there lists the ranks
            if matches!(arg(0), Some("list")) || (arg(0).is_none() && me.is_none()) {
                return say("ranks", &[("list", st.ranks.groups.keys().cloned().collect::<Vec<_>>().join(", "))]);
            }
            let k = match arg(0) {
                Some(n) if me.is_none() || perm(sender, "others.rank") => match st.key_by_name(server, n) {
                    Some(k) => k,
                    None => return say("no_player", &[("arg", n.to_owned())]),
                },
                Some(_) => return say("no_perm", &[]),
                None => key(&need_player!().get_id()),
            };
            let group = st.ranks.group_of(st.players.get(&k).and_then(|d| d.group.as_deref())).to_owned();
            say("rank", &[("name", name_of(st, &k)), ("group", group)])
        }
        "setrank" => {
            let k = known!(0, "/setrank <player> <rank>");
            let Some(group) = arg(1).map(str::to_lowercase) else { return say("usage", &[("arg", "/setrank <player> <rank>".into())]) };
            if !st.ranks.groups.contains_key(&group) {
                return say("rank_none", &[("name", group), ("list", st.ranks.groups.keys().cloned().collect::<Vec<_>>().join(", "))]);
            }
            let is_default = group == st.ranks.default_group();
            if let Some(d) = st.players.get_mut(&k) {
                d.group = (!is_default).then(|| group.clone());
            }
            st.save(&k);
            if let Some(p) = online(&k) {
                apply_perms(st, &p);
                refresh_name(st, &p);
                if me.as_ref().is_none_or(|m| key(&m.get_id()) != k) {
                    c::tell(&p, &fmt(l, "rank_yours", &[("group", group.clone())]));
                }
            }
            say("rank_set", &[("name", name_of(st, &k)), ("group", group)])
        }
        "perm" => {
            let usage = "/perm <player> [node] [true|false|unset]";
            let k = known!(0, usage);
            let Some(node) = arg(1) else {
                let own = st.players.get(&k).map(|d| d.perms.iter().map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>()).unwrap_or_default();
                return say("perm_list", &[("name", name_of(st, &k)), ("list", list_or_none(own))]);
            };
            if !store::valid_node(node) {
                return say("bad_node", &[("arg", node.to_owned())]);
            }
            // A rank that only holds `basics:command.perm` must not hand out what it does not have
            if let Some(m) = &me {
                if (m.get_permission_level() as u8) < 3 && arg(2).is_none_or(|v| matches!(v.to_lowercase().as_str(), "true" | "on" | "yes")) && !m.has_permission(node) {
                    return say("perm_not_yours", &[("node", node.to_owned())]);
                }
            }
            let value = match arg(2).map(str::to_lowercase).as_deref() {
                None | Some("true" | "on" | "yes") => Some(true),
                Some("false" | "off" | "no") => Some(false),
                Some("unset" | "remove" | "clear") => None,
                Some(_) => return say("usage", &[("arg", usage.into())]),
            };
            if let Some(d) = st.players.get_mut(&k) {
                match value {
                    Some(v) => {
                        d.perms.insert(node.to_owned(), v);
                    }
                    None => {
                        d.perms.remove(node);
                    }
                }
            }
            st.save(&k);
            if let Some(p) = online(&k) {
                apply_perms(st, &p);
            }
            match value {
                Some(v) => say("perm_set", &[("node", node.to_owned()), ("name", name_of(st, &k)), ("value", v.to_string())]),
                None => say("perm_unset", &[("node", node.to_owned()), ("name", name_of(st, &k))]),
            }
        }

        // -- chat moderation
        "mute" => {
            let usage = "/mute <player> [30s|10m|2h|7d] [reason]";
            let k = known!(0, usage);
            let (until, reason, shown) = match arg(1).and_then(store::parse_duration) {
                Some(secs) => (now_secs() + secs, rest(2), store::fmt_duration(secs, th)),
                None => (u64::MAX, rest(1), text(l, "mute_forever").to_owned()),
            };
            if let Some(d) = st.players.get_mut(&k) {
                d.muted_until = until;
                d.mute_reason = reason.chars().take(120).collect();
            }
            st.save(&k);
            if let (Some(p), Some(d)) = (online(&k), st.players.get(&k)) {
                c::tell(&p, &mute_notice(l, d));
            }
            say("mute_set", &[("name", name_of(st, &k)), ("time", shown)])
        }
        "unmute" => {
            let k = known!(0, "/unmute <player>");
            let was = st.players.get(&k).is_some_and(|d| d.muted(now_secs()));
            if let Some(d) = st.players.get_mut(&k) {
                d.muted_until = 0;
                d.mute_reason.clear();
            }
            st.save(&k);
            say(if was { "unmuted" } else { "not_muted" }, &[("name", name_of(st, &k))])
        }
        "ignore" => {
            let p = need_player!();
            let mine = key(&p.get_id());
            let Some(n) = arg(0) else {
                let names: Vec<String> = st.data(&p).ignored.clone().iter().map(|k| name_of(st, k)).filter(|n| !n.is_empty()).collect();
                return say("ignore_list", &[("list", list_or_none(names))]);
            };
            let Some(k) = st.key_by_name(server, n) else { return say("no_player", &[("arg", n.to_owned())]) };
            if k == mine {
                return say("ignore_self", &[]);
            }
            let d = st.data(&p);
            let on = if let Some(i) = d.ignored.iter().position(|x| *x == k) {
                d.ignored.remove(i);
                false
            } else if d.ignored.len() < 200 {
                d.ignored.push(k.clone());
                true
            } else {
                false
            };
            st.save(&mine);
            say(if on { "ignore_on" } else { "ignore_off" }, &[("name", name_of(st, &k))])
        }
        "socialspy" => {
            let p = need_player!();
            let d = st.data(&p);
            d.socialspy = !d.socialspy;
            let on = d.socialspy;
            st.save(&key(&p.get_id()));
            say("socialspy", &[("state", on_off(l, on))])
        }

        // -- presence and info
        "afk" => {
            let p = need_player!();
            let k = key(&p.get_id());
            if st.afk.get(&k).is_some_and(|a| a.afk) {
                active(st, server, &p);
            } else {
                st.afk.insert(k, Afk { pos: p.get_position(), yaw: p.get_yaw(), since: Instant::now(), afk: true });
                announce_afk(st, server, &p, true);
            }
            Vec::new()
        }
        "seen" => {
            let k = known!(0, "/seen <player>");
            // A vanished player reads as offline to everyone but staff
            let (last, first, played) = st.players.get(&k).map_or((0, 0, 0), |d| (d.last_seen, d.first_seen, d.playtime));
            let now = now_secs();
            let mut out = if online(&k).is_some() && !crate::extra::hidden_from(st, me.as_ref(), &k) {
                say("seen_online", &[("name", name_of(st, &k)), ("time", store::fmt_duration(now.saturating_sub(last), th))])
            } else {
                say("seen_offline", &[("name", name_of(st, &k)), ("time", store::fmt_duration(now.saturating_sub(last), th))])
            };
            if first > 0 {
                out.push(fmt(l, "seen_more", &[("first", store::fmt_duration(now.saturating_sub(first), th)), ("time", store::fmt_duration(played, th))]));
            }
            out
        }
        "ping" => {
            let p = match arg(0) {
                Some(n) => match find_player(server, n).filter(|t| !crate::extra::hidden_from(st, me.as_ref(), &key(&t.get_id()))) {
                    Some(p) => p,
                    None => return say("no_player", &[("arg", n.to_owned())]),
                },
                None => need_player!(),
            };
            say("ping", &[("name", p.get_name()), ("ms", p.get_ping().to_string())])
        }
        "near" => {
            let p = need_player!();
            let radius = arg(0).and_then(|s| s.parse::<u32>().ok()).unwrap_or(st.cfg.near_radius).clamp(1, 10_000);
            let here = loc_of(&p);
            let mine = key(&p.get_id());
            let mut found: Vec<(f64, String, &str)> = server
                .get_all_players()
                .into_iter()
                .filter(|o| key(&o.get_id()) != mine && o.get_world().get_dimension() == here.dim)
                .filter(|o| !crate::extra::hidden_from(st, Some(&p), &key(&o.get_id())))
                .filter_map(|o| {
                    let (x, y, z) = o.get_position();
                    let d = here.dist2(x, y, z).sqrt();
                    (d <= f64::from(radius)).then(|| (d, o.get_name(), store::compass(x - here.x, z - here.z)))
                })
                .collect();
            if found.is_empty() {
                return say("near_none", &[("radius", radius.to_string())]);
            }
            found.sort_by(|a, b| a.0.total_cmp(&b.0));
            let list = found.iter().map(|(d, n, dir)| format!("{n} ({d:.0}m {dir})").replace(" )", ")")).collect::<Vec<_>>().join(", ");
            say("near", &[("radius", radius.to_string()), ("list", list)])
        }
        "whois" => {
            let k = known!(0, "/whois <player>");
            let Some(d) = st.players.get(&k).cloned() else { return say("no_player", &[("arg", arg(0).unwrap_or("").to_owned())]) };
            let uuid = uuid_of(&k).map_or_else(String::new, |u| format!("{:08x}-{:04x}-{:04x}-{:04x}-{:012x}", u.high >> 32, (u.high >> 16) & 0xffff, u.high & 0xffff, u.low >> 48, u.low & 0xffff_ffff_ffff));
            let sym = st.cfg.currency_symbol.clone();
            let mut rows: Vec<(&str, String)> = vec![
                ("rank", st.ranks.group_of(d.group.as_deref()).to_owned()),
                ("nick", d.nick.clone().unwrap_or_else(none)),
                ("money", store::money(d.balance, &sym)),
                ("homes", d.homes.len().to_string()),
                ("muted", if d.muted(now_secs()) { "yes".into() } else { "no".into() }),
            ];
            match online(&k) {
                Some(p) => {
                    rows.push(("where", loc_of(&p).short()));
                    rows.push(("ping", format!("{} ms", p.get_ping())));
                    let ip = p.get_ip();
                    rows.push(("ip", ip.rsplit_once(':').map_or(ip.clone(), |(host, _)| host.to_owned())));
                    rows.push(("afk", st.afk.get(&k).is_some_and(|a| a.afk).to_string()));
                }
                None => rows.push(("last seen", format!("{} ago", store::fmt_duration(now_secs().saturating_sub(d.last_seen), false)))),
            }
            let mut out = say("whois", &[("name", d.name.clone()), ("uuid", uuid)]);
            out.extend(rows.into_iter().map(|(key, value)| store::fill(text(l, "whois_row"), &[("key", key.to_owned()), ("value", value)])));
            out
        }

        // -- mail
        "mail" => {
            let p = need_player!();
            let mine = key(&p.get_id());
            match arg(0) {
                None | Some("read") => {
                    let mail = st.data(&p).mail.clone();
                    if mail.is_empty() {
                        return say("mail_none", &[]);
                    }
                    c::reply(sender, &fmt(l, "mail_head", &[("count", mail.len().to_string())]));
                    let now = now_secs();
                    for m in &mail {
                        let vars = [("time", store::fmt_duration(now.saturating_sub(m.at), th)), ("from", m.from.clone())];
                        sender.send_message(chat_line(text(l, "mail_row"), &vars, &m.text, false));
                    }
                    Vec::new()
                }
                Some("clear") => {
                    st.data(&p).mail.clear();
                    st.save(&mine);
                    say("mail_cleared", &[])
                }
                Some("send") => {
                    let usage = "/mail send <player> <message>";
                    let k = known!(1, usage);
                    let msg = rest(2);
                    if msg.trim().is_empty() {
                        return say("usage", &[("arg", usage.into())]);
                    }
                    let d = st.data(&p).clone();
                    if d.muted(now_secs()) {
                        return vec![mute_notice(l, &d)];
                    }
                    let max = st.cfg.mail_max.clamp(1, 500);
                    let Some(to) = st.players.get_mut(&k) else { return say("no_player", &[("arg", arg(1).unwrap_or("").to_owned())]) };
                    if to.ignored.contains(&mine) {
                        return say("msg_ignored", &[("name", to.name.clone())]);
                    }
                    // One sender may not fill a mailbox alone
                    let sender_name = p.get_name();
                    if to.mail.len() >= max || to.mail.iter().filter(|m| m.from == sender_name).count() >= 5 {
                        return say("mail_full", &[("name", to.name.clone())]);
                    }
                    to.mail.push(Mail { from: p.get_name(), text: msg.chars().take(256).collect(), at: now_secs() });
                    let (count, to_name) = (to.mail.len(), to.name.clone());
                    st.save(&k);
                    if let Some(t) = online(&k) {
                        c::tell(&t, &fmt(l, "mail_new", &[("count", count.to_string())]));
                    }
                    say("mail_sent", &[("name", to_name)])
                }
                Some(_) => say("usage", &[("arg", "/mail [read|clear|send <player> <message>]".into())]),
            }
        }

        // -- kits
        "kit" => {
            let allowed = |st: &State, kit: &str, p: &Player| st.kits.kits.get(kit).is_some_and(|k| k.everyone) || dyn_perm(p, &format!("kit.{kit}"));
            let Some(kname) = arg(0).map(str::to_lowercase) else {
                let names: Vec<String> = st.kits.kits.keys().filter(|n| me.as_ref().is_none_or(|p| allowed(st, n, p))).cloned().collect();
                return say("kits", &[("list", list_or_none(names))]);
            };
            let Some(kit) = st.kits.kits.get(&kname).cloned() else { return say("kit_none", &[("name", kname)]) };
            let to = match arg(1) {
                Some(n) if me.is_none() || perm(sender, "others.kit") => match find_player(server, n) {
                    Some(t) => t,
                    None => return say("no_player", &[("arg", n.to_owned())]),
                },
                Some(_) => return say("no_perm", &[]),
                None => need_player!(),
            };
            let own = me.as_ref().is_some_and(|m| key(&m.get_id()) == key(&to.get_id()));
            if own {
                if !allowed(st, &kname, &to) {
                    return say("kit_no_perm", &[("name", kname)]);
                }
                if kit.cooldown > 0 && !player_perm(&to, "bypass.cooldown") {
                    let last = st.data(&to).kits.get(&kname).copied().unwrap_or(0);
                    let left = (last + kit.cooldown).saturating_sub(now_secs());
                    if left > 0 {
                        return say("kit_wait", &[("name", kname), ("time", store::fmt_duration(left, th))]);
                    }
                }
            }
            let missed = give_kit(&to, &kit.items);
            if own {
                st.data(&to).kits.insert(kname.clone(), now_secs());
                st.save(&key(&to.get_id()));
            }
            let mut out = if own {
                say("kit_given", &[("name", kname)])
            } else {
                c::tell(&to, &fmt(l, "kit_given", &[("name", kname.clone())]));
                say("kit_given_to", &[("name", kname), ("player", to.get_name())])
            };
            if missed > 0 {
                out.push(fmt(l, "kit_full", &[("count", missed.to_string())]));
            }
            out
        }

        // -- teleport extras and tools
        "ec" => {
            let p = need_player!();
            let other = match arg(0) {
                Some(n) if perm(sender, "others.ec") => match find_player(server, n) {
                    Some(t) => Some(t).filter(|t| key(&t.get_id()) != key(&p.get_id())),
                    None => return say("no_player", &[("arg", n.to_owned())]),
                },
                Some(_) => return say("no_perm", &[]),
                None => None,
            };
            match other {
                Some(t) => crate::gui::open_mirror(st, server, &p, &t, true, player_perm(&p, "invsee.modify")),
                None => {
                    // The server closes any window we had open from inside this call
                    crate::gui::finish(st, server, &p);
                    p.open_ender_chest();
                }
            }
            Vec::new()
        }
        "tpall" => {
            let p = need_player!();
            let loc = loc_of(&p);
            let mine = key(&p.get_id());
            let others: Vec<Player> = server.get_all_players().into_iter().filter(|o| key(&o.get_id()) != mine).collect();
            for o in &others {
                teleport_now(st, server, o, &loc);
            }
            say("tpall", &[("count", others.len().to_string())])
        }
        "tptoggle" => {
            let p = need_player!();
            let d = st.data(&p);
            d.tp_off = !d.tp_off;
            let accept = !d.tp_off;
            st.save(&key(&p.get_id()));
            say("tptoggle", &[("state", on_off(l, accept))])
        }
        "tppos" => {
            let p = need_player!();
            let usage = "/tppos <x> <y> <z> [world]";
            let num = |i: usize| arg(i).and_then(|s| s.parse::<f64>().ok()).filter(|v| v.is_finite() && v.abs() <= 30_000_000.0);
            let (Some(x), Some(y), Some(z)) = (num(0), num(1), num(2)) else { return say("usage", &[("arg", usage.into())]) };
            let here = loc_of(&p);
            let dim = match arg(3) {
                Some(n) => match dim_of(n) {
                    Some(d) => d.to_owned(),
                    None => return say("no_world", &[]),
                },
                None => here.dim.clone(),
            };
            let to = Loc { dim, x, y: y.clamp(-64.0, 1024.0), z, yaw: here.yaw, pitch: here.pitch };
            if teleport_now(st, server, &p, &to) {
                say("tp_pos", &[("x", format!("{x:.0}")), ("y", format!("{:.0}", to.y)), ("z", format!("{z:.0}"))])
            } else {
                Vec::new()
            }
        }
        "suicide" => {
            need_player!().kill();
            Vec::new()
        }
        "rtp" => {
            let p = need_player!();
            start_rtp(st, server, &p)
        }
        _ => crate::extra::run(name, st, sender, server, w),
    }
}

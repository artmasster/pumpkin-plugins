//! Basics third set: moderation (tempban, warn, jail, freeze, vanish), staff chat, item and player
//! tools, world shortcuts, selling to the server and server info.
use mcsv_common as c;
use pumpkin_plugin_api::command::CommandSender;
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::wit::pumpkin::plugin::player::{BanPlayerOptions, PlayerWeather};
use pumpkin_plugin_api::wit::pumpkin::plugin::server::CommandSender as ServerSender;
use pumpkin_plugin_api::Server;

use crate::lang::{fmt, text};
use crate::more;
use crate::plugin::{State, find_player, key, loc_of, now_secs, on_off, perm, player_perm, teleport_now, uuid_of, world_of};
use crate::store::{self, Warning};

/// Commands a jailed player may not use.
const TELEPORTS: &[&str] = &["home", "warp", "spawn", "back", "top", "tpa", "tpahere", "tpaccept", "rtp", "tppos", "jump", "tphere", "tpall"];

/// The reason a player may not run this command right now, if any.
pub(crate) fn blocked(st: &mut State, p: &Player, name: &str) -> Option<String> {
    let l = st.cfg.language.clone();
    let d = st.data(p);
    if d.jailed(now_secs()) && TELEPORTS.contains(&name) {
        return Some(fmt(&l, "jailed_no", &[]));
    }
    // Frozen means held in place: a teleport command would be the way out
    if d.frozen && TELEPORTS.contains(&name) {
        return Some(fmt(&l, "frozen_you", &[]));
    }
    None
}

/// Staff see vanished players; everyone else must not learn that they are online.
pub(crate) fn hidden_from(st: &State, viewer: Option<&Player>, target_key: &str) -> bool {
    st.players.get(target_key).is_some_and(|d| d.vanished) && viewer.is_some_and(|v| !player_perm(v, "command.vanish"))
}

fn set_vanish(st: &mut State, server: &Server, p: &Player, on: bool) {
    let mine = key(&p.get_id());
    for o in server.get_all_players() {
        if key(&o.get_id()) == mine {
            continue;
        }
        // The API takes the player by value, so each call needs its own handle
        let Some(v) = server.get_player_by_uuid(p.get_id()) else { continue };
        if on && !player_perm(&o, "command.vanish") {
            o.hide_player(v);
        } else {
            o.show_player(v);
        }
    }
    p.set_tab_list_listed(!on);
    st.data(p).vanished = on;
    st.save(&mine);
}

pub(crate) fn on_join(st: &mut State, server: &Server, p: &Player) {
    let d = st.data(p).clone();
    if d.frozen {
        p.set_movement_locked(true);
    }
    if d.vanished {
        set_vanish(st, server, p, true);
    }
    // Players who vanished earlier stay hidden from the newcomer
    if !player_perm(p, "command.vanish") {
        let mine = key(&p.get_id());
        for o in server.get_all_players() {
            let k = key(&o.get_id());
            if k != mine && st.players.get(&k).is_some_and(|x| x.vanished) {
                p.hide_player(o);
            }
        }
    }
}

/// Reminders for a player who joined, once a login prompt is behind them.
pub(crate) fn welcome(st: &mut State, p: &Player) {
    let l = st.cfg.language.clone();
    let d = st.data(p).clone();
    if d.vanished {
        c::tell(p, &fmt(&l, "vanish", &[("state", on_off(&l, true))]));
    }
    if d.jailed(now_secs()) {
        c::tell(p, &fmt(&l, "jailed_no", &[]));
    }
}

fn tab_text(template: &str, vars: &[(&str, String)]) -> String {
    store::fill(&template.replace("\\n", "\n"), vars)
}

/// Once a second: play time, jail walls and release, AFK kick, tab list text.
pub(crate) fn tick_second(st: &mut State, server: &Server, players: &[Player]) {
    let l = st.cfg.language.clone();
    let now = now_secs();
    let minute = st.ticks % 600 == 0;
    let radius = f64::from(st.cfg.jail_radius.clamp(2, 256));
    let kick_after = st.cfg.afk_kick_minutes.saturating_mul(60);
    for p in players {
        let k = key(&p.get_id());
        let (jail_until, was_jailed, frozen) = {
            let d = st.data(p);
            d.playtime += 1;
            (d.jailed_until, d.jailed_until > 0, d.frozen)
        };
        // Another plugin may have unlocked the player (Login does after a login)
        if frozen && !p.is_movement_locked() {
            p.set_movement_locked(true);
        }
        if minute {
            st.dirty.insert(k.clone());
        }
        // Still at a login prompt: no jail teleport and no AFK kick yet
        if st.joining.contains_key(&k) {
            continue;
        }
        if was_jailed {
            if jail_until > now {
                if let Some(j) = st.jail.clone() {
                    let here = loc_of(p);
                    if here.dim != j.dim || here.dist2(j.x, j.y, j.z) > radius * radius {
                        teleport_now(st, server, p, &j);
                    }
                }
            } else {
                st.data(p).jailed_until = 0;
                st.save(&k);
                c::tell(p, &fmt(&l, "unjailed_you", &[]));
                if let Some(s) = st.spawn.clone() {
                    teleport_now(st, server, p, &s);
                }
            }
        }
        if kick_after > 0 && !player_perm(p, "bypass.afkkick") {
            let idle = st.afk.get(&k).filter(|a| a.afk).map_or(0, |a| a.since.elapsed().as_secs());
            if idle >= kick_after {
                st.afk.remove(&k);
                server.execute_command(&format!("kick {} AFK", p.get_name()), ServerSender::Console);
            }
        }
    }
    if st.ticks % 50 == 0 && (!st.cfg.tab_header.is_empty() || !st.cfg.tab_footer.is_empty()) {
        let shared = [
            ("online", players.iter().filter(|p| !st.players.get(&key(&p.get_id())).is_some_and(|d| d.vanished)).count().to_string()),
            ("max", server.get_max_players().to_string()),
            ("tps", format!("{:.1}", server.get_tps().min(20.0))),
        ];
        for p in players {
            let mut vars = shared.to_vec();
            vars.push(("name", p.get_name()));
            p.set_tab_list_header_footer(c::color(&tab_text(&st.cfg.tab_header, &vars)), c::color(&tab_text(&st.cfg.tab_footer, &vars)));
        }
    }
}

/// Send a line to everyone who may read staff chat, and to the console.
fn to_staff(server: &Server, line: &str) {
    for p in server.get_all_players() {
        if player_perm(&p, "command.staffchat") {
            c::tell(&p, line);
        }
    }
    c::log_info(&c::plain(line));
}

fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|ch| !ch.is_control()).take(max).collect()
}

fn item_label(key: &str, count: u8) -> String {
    format!("{} x{count}", key.strip_prefix("minecraft:").unwrap_or(key))
}

fn empty(key: &str, count: u8) -> bool {
    count == 0 || key.ends_with("air")
}

pub(crate) fn run(name: &str, st: &mut State, sender: &CommandSender, server: &Server, w: &[String]) -> Vec<String> {
    let lang = st.cfg.language.clone();
    let l = lang.as_str();
    let th = l != "en";
    let say = |k: &str, v: &[(&str, String)]| vec![fmt(l, k, v)];
    let me = c::sender_player(sender);
    let arg = |i: usize| w.get(i).map(String::as_str);
    let rest = |i: usize| w.get(i..).map(|s| s.join(" ")).unwrap_or_default();
    let who = me.as_ref().map_or_else(|| "Console".to_owned(), Player::get_name);
    macro_rules! need_player {
        () => {
            match c::sender_player(sender) {
                Some(p) => p,
                None => return say("player_only", &[]),
            }
        };
    }
    macro_rules! usage {
        ($u:expr) => {
            return say("usage", &[("arg", $u.into())])
        };
    }
    // A known player by name (online or not)
    macro_rules! known {
        ($i:expr, $usage:expr) => {{
            let Some(n) = arg($i) else { usage!($usage) };
            match st.key_by_name(server, n) {
                Some(k) => k,
                None => return say("no_player", &[("arg", n.to_owned())]),
            }
        }};
    }
    // args[i] when the sender may act on others (`basics:others.<cmd>`), else the sender
    macro_rules! target {
        ($i:expr, $cmd:expr) => {
            match arg($i) {
                Some(n) if me.is_none() || perm(sender, &format!("others.{}", $cmd)) => match find_player(server, n) {
                    Some(p) => p,
                    None => return say("no_player", &[("arg", n.to_owned())]),
                },
                Some(_) => return say("no_perm", &[]),
                None => need_player!(),
            }
        };
    }
    let online = |k: &str| uuid_of(k).and_then(|u| server.get_player_by_uuid(u));
    let name_of = |st: &State, k: &str| st.players.get(k).map_or_else(String::new, |d| d.name.clone());

    match name {
        // -- bans and warnings
        "tempban" => {
            let u = "/tempban <player> <30m|2h|7d> [reason]";
            let k = known!(0, u);
            let Some(secs) = arg(1).and_then(store::parse_duration) else { usage!(u) };
            let reason = clean(&rest(2), 120);
            let (Some(id), pname) = (uuid_of(&k), name_of(st, &k)) else { return say("no_player", &[("arg", arg(0).unwrap_or("").to_owned())]) };
            let shown = if reason.is_empty() { text(l, "ban_default").to_owned() } else { reason };
            server.get_ban_manager().ban_player(
                &pname,
                id,
                BanPlayerOptions {
                    reason: Some(c::color(&shown)),
                    source: Some(who.clone()),
                    expires_at_utc: None,
                    duration_seconds: Some(secs),
                    kick_if_online: true,
                    log_to_console: true,
                },
            );
            say("tempban", &[("name", pname), ("time", store::fmt_duration(secs, th))])
        }
        "unban" => {
            let k = known!(0, "/unban <player>");
            let was = uuid_of(&k).is_some_and(|id| server.get_ban_manager().unban_player(id));
            say(if was { "unbanned" } else { "not_banned" }, &[("name", name_of(st, &k))])
        }
        "warn" => {
            let u = "/warn <player> <reason>";
            let k = known!(0, u);
            let reason = clean(&rest(1), 120);
            if reason.is_empty() {
                usage!(u);
            }
            let count = {
                let Some(d) = st.players.get_mut(&k) else { return say("no_player", &[]) };
                if d.warnings.len() >= 50 {
                    d.warnings.remove(0);
                }
                d.warnings.push(Warning { by: who.clone(), reason: reason.clone(), at: now_secs() });
                d.warnings.len()
            };
            st.save(&k);
            let pname = name_of(st, &k);
            if let Some(p) = online(&k) {
                c::tell(&p, &fmt(l, "warned_you", &[("reason", reason.clone()), ("count", count.to_string())]));
            }
            let at = st.cfg.warn_ban_at;
            if at > 0 && count as u32 >= at {
                if let Some(id) = uuid_of(&k) {
                    let minutes = st.cfg.warn_ban_minutes;
                    server.get_ban_manager().ban_player(
                        &pname,
                        id,
                        BanPlayerOptions {
                            reason: Some(c::color(&reason)),
                            source: Some("Basics".into()),
                            expires_at_utc: None,
                            duration_seconds: (minutes > 0).then(|| minutes.saturating_mul(60)),
                            kick_if_online: true,
                            log_to_console: true,
                        },
                    );
                }
            }
            say("warned", &[("name", pname), ("count", count.to_string())])
        }
        "warnings" => {
            let k = match arg(0) {
                Some(n) if me.is_none() || perm(sender, "others.warnings") => match st.key_by_name(server, n) {
                    Some(k) => k,
                    None => return say("no_player", &[("arg", n.to_owned())]),
                },
                Some(_) => return say("no_perm", &[]),
                None => key(&need_player!().get_id()),
            };
            let list = st.players.get(&k).map(|d| d.warnings.clone()).unwrap_or_default();
            if list.is_empty() {
                return say("warnings_none", &[("name", name_of(st, &k))]);
            }
            let now = now_secs();
            let mut out = say("warnings", &[("name", name_of(st, &k)), ("count", list.len().to_string())]);
            for wn in list.iter().rev().take(10) {
                let vars = [("time", store::fmt_duration(now.saturating_sub(wn.at), th)), ("by", wn.by.clone()), ("reason", wn.reason.clone())];
                out.push(store::fill(text(l, "warning_row"), &vars));
            }
            out
        }
        "clearwarnings" => {
            let k = known!(0, "/clearwarnings <player>");
            if let Some(d) = st.players.get_mut(&k) {
                d.warnings.clear();
            }
            st.save(&k);
            say("warnings_cleared", &[("name", name_of(st, &k))])
        }

        // -- freeze, jail, vanish
        "freeze" => {
            let Some(p) = arg(0).and_then(|n| find_player(server, n)) else {
                return say("no_player", &[("arg", arg(0).unwrap_or("").to_owned())]);
            };
            let k = key(&p.get_id());
            let d = st.data(&p);
            d.frozen = !d.frozen;
            let on = d.frozen;
            p.set_movement_locked(on);
            st.save(&k);
            c::tell(&p, &fmt(l, if on { "frozen_you" } else { "unfrozen_you" }, &[]));
            say("freeze", &[("name", p.get_name()), ("state", on_off(l, on))])
        }
        "setjail" => {
            let p = need_player!();
            st.jail = Some(loc_of(&p));
            c::save_json("jail.json", &st.jail);
            say("jail_set", &[])
        }
        "jail" => {
            let u = "/jail <player> [10m|2h|7d] [reason]";
            let k = known!(0, u);
            let Some(j) = st.jail.clone() else { return say("jail_none", &[]) };
            let (until, reason_at, shown) = match arg(1).and_then(store::parse_duration) {
                Some(secs) => (now_secs() + secs, 2, store::fmt_duration(secs, th)),
                None => (u64::MAX, 1, text(l, "mute_forever").to_owned()),
            };
            let reason = clean(&rest(reason_at), 120);
            if let Some(d) = st.players.get_mut(&k) {
                d.jailed_until = until;
            }
            st.save(&k);
            if let Some(p) = online(&k) {
                teleport_now(st, server, &p, &j);
                c::tell(&p, &fmt(l, "jailed_you", &[("time", shown.clone()), ("reason", reason)]));
            }
            say("jailed", &[("name", name_of(st, &k)), ("time", shown)])
        }
        "unjail" => {
            let k = known!(0, "/unjail <player>");
            let was = st.players.get(&k).is_some_and(|d| d.jailed(now_secs()));
            if let Some(d) = st.players.get_mut(&k) {
                // The next tick releases an online player and sends them to spawn
                d.jailed_until = if was && online(&k).is_some() { 1 } else { 0 };
            }
            st.save(&k);
            say(if was { "unjailed" } else { "not_jailed" }, &[("name", name_of(st, &k))])
        }
        "vanish" => {
            let p = target!(0, "vanish");
            let on = !st.data(&p).vanished;
            set_vanish(st, server, &p, on);
            let msg = fmt(l, "vanish", &[("state", on_off(l, on))]);
            if me.as_ref().is_none_or(|m| key(&m.get_id()) != key(&p.get_id())) {
                c::tell(&p, &msg);
            }
            vec![msg]
        }

        // -- inventories
        "invsee" => {
            let Some(p) = arg(0).and_then(|n| find_player(server, n)) else {
                return say("no_player", &[("arg", arg(0).unwrap_or("").to_owned())]);
            };
            // In game: a window that follows the target's inventory. The console gets the list.
            if let Some(viewer) = &me {
                if key(&viewer.get_id()) == key(&p.get_id()) {
                    return say("invsee_self", &[]);
                }
                crate::gui::open_mirror(st, server, viewer, &p, false, player_perm(viewer, "invsee.modify"));
                return Vec::new();
            }
            let mut items = Vec::new();
            for slot in 0u8..36 {
                if let Some(s) = p.get_inventory_item(slot) {
                    let (id, n) = (s.get_registry_key(), s.get_count());
                    if !empty(&id, n) {
                        items.push(item_label(&id, n));
                    }
                }
            }
            let inv = p.get_inventory();
            let mut armor = Vec::new();
            for s in [inv.get_helmet(), inv.get_chestplate(), inv.get_leggings(), inv.get_boots(), inv.get_off_hand()].into_iter().flatten() {
                let (id, n) = (s.get_registry_key(), s.get_count());
                if !empty(&id, n) {
                    armor.push(item_label(&id, n));
                }
            }
            let none = text(l, "none").to_owned();
            let join = |v: Vec<String>| if v.is_empty() { none.clone() } else { v.join(", ") };
            vec![fmt(l, "invsee", &[("name", p.get_name()), ("list", join(items))]), fmt(l, "invsee_armor", &[("list", join(armor))])]
        }
        "clearinventory" => {
            let p = target!(0, "clearinventory");
            p.get_inventory().clear_all();
            say("inv_cleared", &[("name", p.get_name())])
        }
        "hat" => {
            let p = need_player!();
            let slot = p.get_selected_slot();
            let Some(stack) = p.get_inventory_item(slot).filter(|s| !empty(&s.get_registry_key(), s.get_count())) else {
                return say("hand_empty", &[]);
            };
            let inv = p.get_inventory();
            let old = inv.get_helmet().filter(|s| !empty(&s.get_registry_key(), s.get_count()));
            inv.set_helmet(Some(stack));
            p.set_inventory_item(slot, old);
            say("hat", &[])
        }
        "more" => {
            let p = need_player!();
            let slot = p.get_selected_slot();
            let Some(stack) = p.get_inventory_item(slot).filter(|s| !empty(&s.get_registry_key(), s.get_count())) else {
                return say("hand_empty", &[]);
            };
            stack.set_count(stack.get_max_count().max(1));
            p.set_inventory_item(slot, Some(stack));
            say("more", &[])
        }
        "item" => {
            let u = "/item <item> [count] [player]";
            let Some(id) = arg(0) else { usage!(u) };
            let count = arg(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(1).clamp(1, 2304);
            let p = target!(2, "item");
            // Tools and armor do not stack: never hand out a stack the game would not make
            let per = store::parse_item(id).map_or(1, |(key, _)| u32::from(pumpkin_plugin_api::ItemStack::new(&key, 1).get_max_count().clamp(1, 64)));
            let count = count.min(per * 36);
            let mut left = count;
            let mut lines = Vec::new();
            while left > 0 {
                let n = left.min(per);
                lines.push(format!("{id} {n}"));
                left -= n;
            }
            let missed = more::give_kit(&p, &lines);
            if missed == lines.len() {
                return say("item_bad", &[("arg", id.to_owned())]);
            }
            let mut out = say("item_given", &[("item", id.to_owned()), ("count", count.to_string()), ("name", p.get_name())]);
            if missed > 0 {
                out.push(fmt(l, "kit_full", &[("count", missed.to_string())]));
            }
            out
        }
        "itemname" | "itemlore" => {
            let p = need_player!();
            let textv = clean(&rest(0), 100);
            if textv.is_empty() {
                usage!(if name == "itemname" { "/itemname <name>" } else { "/itemlore <text>" });
            }
            let slot = p.get_selected_slot();
            let Some(stack) = p.get_inventory_item(slot).filter(|s| !empty(&s.get_registry_key(), s.get_count())) else {
                return say("hand_empty", &[]);
            };
            if name == "itemname" {
                stack.set_custom_name(Some(c::color(&textv)));
            } else {
                if stack.get_lore().len() >= 12 {
                    return say("lore_full", &[]);
                }
                stack.add_lore(c::color(&textv));
            }
            p.set_inventory_item(slot, Some(stack));
            say("item_edited", &[])
        }

        // -- staff chat
        "staffchat" | "helpop" => {
            let msg = clean(&rest(0), 256);
            if msg.is_empty() {
                usage!(if name == "staffchat" { "/sc <message>" } else { "/helpop <message>" });
            }
            if let Some(p) = &me {
                let d = st.data(p).clone();
                if name == "helpop" && d.muted(now_secs()) {
                    return vec![more::mute_notice(l, &d)];
                }
            }
            let line = store::fill(text(l, name), &[("name", who.clone())]) + &msg.replace('&', "&\u{200b}");
            to_staff(server, &line);
            if name == "helpop" && me.as_ref().is_some_and(|p| !player_perm(p, "command.staffchat")) {
                return say("helpop_sent", &[]);
            }
            Vec::new()
        }
        "clearchat" => {
            for p in server.get_all_players() {
                if !player_perm(&p, "command.clearchat") {
                    for _ in 0..100 {
                        c::tell(&p, " ");
                    }
                }
                c::tell(&p, &fmt(l, "chat_cleared", &[("name", who.clone())]));
            }
            Vec::new()
        }
        "mutechat" => {
            st.chat_locked = !st.chat_locked;
            let line = fmt(l, if st.chat_locked { "chat_locked" } else { "chat_unlocked" }, &[("name", who.clone())]);
            for p in server.get_all_players() {
                c::tell(&p, &line);
            }
            if me.is_none() { vec![line] } else { Vec::new() }
        }
        "kickall" => {
            let reason = clean(&rest(0), 100);
            let mine = me.as_ref().map(|m| key(&m.get_id()));
            let mut n = 0;
            for p in server.get_all_players() {
                if mine.as_deref() == Some(key(&p.get_id()).as_str()) || player_perm(&p, "command.kickall") {
                    continue;
                }
                server.execute_command(&format!("kick {} {reason}", p.get_name()), ServerSender::Console);
                n += 1;
            }
            say("kickall", &[("count", n.to_string())])
        }

        // -- player tools
        "exp" => {
            let (op, amount_at) = match arg(0) {
                Some("give" | "add") => ("give", 1),
                Some("set") => ("set", 1),
                _ => ("show", 9),
            };
            if op == "show" {
                let p = target!(0, "exp");
                return say("exp", &[("name", p.get_name()), ("level", p.get_experience_level().to_string())]);
            }
            let u = "/exp <give|set> <levels> [player]";
            let Some(n) = arg(amount_at).and_then(|s| s.parse::<i32>().ok()).filter(|n| (0..=100_000).contains(n)) else { usage!(u) };
            let p = target!(2, "exp");
            if op == "give" {
                p.add_experience_levels(n);
            } else {
                p.set_experience_level(n);
            }
            say("exp", &[("name", p.get_name()), ("level", p.get_experience_level().to_string())])
        }
        "ptime" => {
            let p = need_player!();
            let t = match arg(0).map(str::to_lowercase).as_deref() {
                Some("reset" | "off") | None => None,
                Some("day") => Some(1000),
                Some("noon") => Some(6000),
                Some("night") => Some(13_000),
                Some("midnight") => Some(18_000),
                Some(s) => match s.parse::<u64>() {
                    Ok(v) if v <= 24_000 => Some(v),
                    _ => usage!("/ptime <day|noon|night|midnight|0-24000|reset>"),
                },
            };
            match t {
                Some(v) => p.set_player_time(v, false),
                None => p.reset_player_time(),
            }
            say("ptime", &[("value", t.map_or_else(|| text(l, "default").to_owned(), |v| v.to_string()))])
        }
        "pweather" => {
            let p = need_player!();
            let (wv, shown) = match arg(0).map(str::to_lowercase).as_deref() {
                Some("clear" | "sun") => (Some(PlayerWeather::Clear), "clear"),
                Some("rain" | "storm" | "downfall") => (Some(PlayerWeather::Downfall), "rain"),
                Some("reset" | "off") | None => (None, ""),
                Some(_) => usage!("/pweather <clear|rain|reset>"),
            };
            match wv {
                Some(v) => p.set_player_weather(v),
                None => p.reset_player_weather(),
            }
            say("pweather", &[("value", if shown.is_empty() { text(l, "default").to_owned() } else { shown.to_owned() })])
        }
        "day" | "night" | "sun" | "rain" | "storm" => {
            let world = match &me {
                Some(p) => p.get_world(),
                None => match world_of(server, "minecraft:overworld") {
                    Some(wd) => wd,
                    None => return say("no_world", &[]),
                },
            };
            match name {
                "day" => world.set_time_of_day(1000),
                "night" => world.set_time_of_day(13_000),
                "sun" => {
                    world.set_raining(false);
                    world.set_thundering(false);
                }
                "rain" => {
                    world.set_raining(true);
                    world.set_thundering(false);
                }
                _ => {
                    world.set_raining(true);
                    world.set_thundering(true);
                }
            }
            say("world_set", &[("value", name.to_owned())])
        }
        "getpos" => {
            let p = target!(0, "getpos");
            say("getpos", &[("name", p.get_name()), ("where", loc_of(&p).short())])
        }
        "playtime" => {
            let k = match arg(0) {
                Some(n) => match st.key_by_name(server, n) {
                    Some(k) => k,
                    None => return say("no_player", &[("arg", n.to_owned())]),
                },
                None => key(&need_player!().get_id()),
            };
            let secs = st.players.get(&k).map_or(0, |d| d.playtime);
            say("playtime", &[("name", name_of(st, &k)), ("time", store::fmt_duration(secs, th))])
        }
        "realname" => {
            let Some(n) = arg(0) else { usage!("/realname <nickname>") };
            let wanted = c::plain(n).to_lowercase();
            let found: Vec<String> = st
                .players
                .values()
                .filter(|d| d.nick.as_ref().is_some_and(|x| c::plain(x).to_lowercase() == wanted))
                .map(|d| d.name.clone())
                .collect();
            if found.is_empty() {
                return say("no_player", &[("arg", n.to_owned())]);
            }
            say("realname", &[("nick", n.to_owned()), ("name", found.join(", "))])
        }
        "rules" | "motd" => {
            let lines = if name == "rules" { st.cfg.rules.clone() } else { st.cfg.motd.clone() };
            if lines.is_empty() {
                return say("text_none", &[]);
            }
            let vars = [("name", who.clone())];
            lines.iter().map(|x| store::fill(x, &vars)).collect()
        }
        "lag" => {
            let info = server.get_sys_info();
            // Unknown inside most containers; leave the figure out rather than print "?"
            let ram = match (info.used_memory, info.total_memory) {
                (Some(u), Some(t)) if t > 0 => format!(" &7| &eRAM &f{}/{} MB", u / 1_048_576, t / 1_048_576),
                _ => String::new(),
            };
            say(
                "lag",
                &[
                    ("tps", format!("{:.1}", server.get_tps().min(20.0))),
                    ("mspt", format!("{:.1}", server.get_mspt())),
                    ("players", server.get_player_count().to_string()),
                    ("uptime", store::fmt_duration(st.started.elapsed().as_secs(), th)),
                    ("ram", ram),
                    ("known", st.players.len().to_string()),
                ],
            )
        }
        "jump" => {
            let p = need_player!();
            let Some((x, y, z)) = p.get_target_block(120) else { return say("jump_none", &[]) };
            let mut to = loc_of(&p);
            (to.x, to.y, to.z) = (x.floor() + 0.5, y.floor() + 1.0, z.floor() + 0.5);
            teleport_now(st, server, &p, &to);
            Vec::new()
        }
        "smite" => {
            let p = target!(0, "smite");
            p.get_world().strike_lightning(p.get_position(), false);
            say("smite", &[("name", p.get_name())])
        }
        "ext" => {
            let p = target!(0, "ext");
            p.as_entity().set_fire_ticks(0);
            say("ext", &[("name", p.get_name())])
        }

        // -- selling to the server
        "worth" => {
            let id = match arg(0) {
                Some(n) => match store::parse_item(n) {
                    Some((id, _)) => id,
                    None => return say("item_bad", &[("arg", n.to_owned())]),
                },
                None => {
                    let p = need_player!();
                    match p.get_inventory_item(p.get_selected_slot()).map(|s| (s.get_registry_key(), s.get_count())) {
                        Some((id, n)) if !empty(&id, n) => id,
                        _ => return say("hand_empty", &[]),
                    }
                }
            };
            let sym = st.cfg.currency_symbol.clone();
            match st.worth.price(&id) {
                Some(price) => say("worth", &[("item", id), ("money", store::money(price, &sym))]),
                None => say("worth_none", &[("item", id)]),
            }
        }
        "sell" => {
            let p = need_player!();
            let slots: Vec<u8> = match arg(0).map(str::to_lowercase).as_deref() {
                Some("hand") | None => vec![p.get_selected_slot()],
                Some("all") => (0u8..36).collect(),
                Some(_) => usage!("/sell <hand|all>"),
            };
            let (mut total, mut count) = (0.0f64, 0u32);
            for slot in slots {
                let Some(s) = p.get_inventory_item(slot) else { continue };
                let (id, n) = (s.get_registry_key(), s.get_count());
                // Renamed or enchanted items are someone's gear, not goods
                if empty(&id, n) || s.get_custom_name().is_some() || !s.get_enchantments().is_empty() {
                    continue;
                }
                if let Some(price) = st.worth.price(&id) {
                    p.set_inventory_item(slot, None);
                    total += price * f64::from(n);
                    count += u32::from(n);
                }
            }
            if count == 0 {
                return say("sell_none", &[]);
            }
            let sym = st.cfg.currency_symbol.clone();
            let k = key(&p.get_id());
            let d = st.data(&p);
            d.balance = store::credit(d.balance, total);
            let bal = d.balance;
            st.save(&k);
            say("sold", &[("count", count.to_string()), ("money", store::money((total * 100.0).round() / 100.0, &sym)), ("balance", store::money(bal, &sym))])
        }
        "paytoggle" => {
            let p = need_player!();
            let d = st.data(&p);
            d.pay_off = !d.pay_off;
            let accept = !d.pay_off;
            st.save(&key(&p.get_id()));
            say("paytoggle", &[("state", on_off(l, accept))])
        }
        _ => say("help", &[]),
    }
}

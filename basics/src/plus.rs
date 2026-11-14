//! Basics fourth set: `/repair`, `/sudo`, join and quit lines, ranks earned by play time and
//! announcements on a boss bar. Windows are in `gui.rs`.
use std::sync::Mutex;
use std::time::Instant;

use mcsv_common as c;
use pumpkin_plugin_api::boss_bar::{BossBar, BossBarColor, BossBarDivision};
use pumpkin_plugin_api::command::CommandSender;
use pumpkin_plugin_api::data_components::DataComponent;
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::text::TextComponent;
use pumpkin_plugin_api::uuid::Uuid;
use pumpkin_plugin_api::wit::pumpkin::plugin::server::CommandSender as ServerSender;
use pumpkin_plugin_api::{ItemStack, Server};

use crate::lang::fmt;
use crate::more;
use crate::plugin::{State, find_player, key, perm};
use crate::store::{self, Sudo};

// ---- work for after the state is released -----------------------------------

/// `server.execute_command` runs the command before it returns, and a Basics command needs the
/// state: such calls wait here until the command that asked for them has let go of it.
pub(crate) enum After {
    RunAs { target: Uuid, command: String },
}

static AFTER: Mutex<Vec<After>> = Mutex::new(Vec::new());

fn after(job: After) {
    if let Ok(mut q) = AFTER.try_lock() {
        q.push(job);
    }
}

/// Run what was queued. Call only while the state is not held.
pub(crate) fn run_after(server: &Server) {
    let jobs = match AFTER.try_lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => return,
    };
    for job in jobs {
        match job {
            After::RunAs { target, command } => {
                if let Some(p) = server.get_player_by_uuid(target) {
                    server.execute_command(&command, ServerSender::Player(p));
                }
            }
        }
    }
}

// ---- repair -----------------------------------------------------------------

fn damage_of(s: &ItemStack) -> i32 {
    s.get_components().iter().find(|comp| matches!(comp.component, DataComponent::Damage)).and_then(|comp| store::read_varint(&comp.value)).unwrap_or(0)
}

/// The stack without its wear, when it had any.
fn mended(s: Option<ItemStack>) -> Option<ItemStack> {
    let s = s?;
    if s.get_count() == 0 || damage_of(&s) <= 0 {
        return None;
    }
    s.remove_component(DataComponent::Damage);
    Some(s)
}

fn repair(p: &Player, all: bool) -> Option<u32> {
    if !all {
        let slot = p.get_selected_slot();
        let held = p.get_inventory_item(slot).filter(|s| s.get_count() > 0 && !s.get_registry_key().ends_with("air"))?;
        return Some(match mended(Some(held)) {
            Some(s) => {
                p.set_inventory_item(slot, Some(s));
                1
            }
            None => 0,
        });
    }
    let mut n = 0;
    for slot in 0u8..36 {
        if let Some(s) = mended(p.get_inventory_item(slot)) {
            p.set_inventory_item(slot, Some(s));
            n += 1;
        }
    }
    let inv = p.get_inventory();
    if let Some(s) = mended(inv.get_helmet()) {
        inv.set_helmet(Some(s));
        n += 1;
    }
    if let Some(s) = mended(inv.get_chestplate()) {
        inv.set_chestplate(Some(s));
        n += 1;
    }
    if let Some(s) = mended(inv.get_leggings()) {
        inv.set_leggings(Some(s));
        n += 1;
    }
    if let Some(s) = mended(inv.get_boots()) {
        inv.set_boots(Some(s));
        n += 1;
    }
    if let Some(s) = mended(inv.get_off_hand()) {
        inv.set_off_hand(Some(s));
        n += 1;
    }
    Some(n)
}

// ---- commands ---------------------------------------------------------------

pub(crate) fn run(name: &str, st: &mut State, sender: &CommandSender, server: &Server, w: &[String]) -> Option<Vec<String>> {
    let lang = st.cfg.language.clone();
    let l = lang.as_str();
    let say = |k: &str, v: &[(&str, String)]| Some(vec![fmt(l, k, v)]);
    let me = c::sender_player(sender);
    let arg = |i: usize| w.get(i).map(String::as_str);
    let rest = |i: usize| w.get(i..).map(|s| s.join(" ")).unwrap_or_default();
    macro_rules! need_player {
        () => {
            match c::sender_player(sender) {
                Some(p) => p,
                None => return say("player_only", &[]),
            }
        };
    }
    match name {
        "trash" => {
            let p = need_player!();
            crate::gui::open_trash(st, server, &p);
            Some(Vec::new())
        }
        "shop" => {
            let p = need_player!();
            Some(crate::gui::open_shop(st, server, &p, arg(0)))
        }
        "repair" => {
            let u = "/repair [hand|all] [player]";
            let all = match arg(0).map(str::to_lowercase).as_deref() {
                None | Some("hand") => false,
                Some("all") => true,
                Some(_) => return say("usage", &[("arg", u.into())]),
            };
            let p = match arg(1) {
                Some(n) if me.is_none() || perm(sender, "others.repair") => match find_player(server, n) {
                    Some(p) => p,
                    None => return say("no_player", &[("arg", n.to_owned())]),
                },
                Some(_) => return say("no_perm", &[]),
                None => need_player!(),
            };
            match repair(&p, all) {
                None => say("hand_empty", &[]),
                Some(0) => say("repair_none", &[]),
                Some(n) => say("repaired", &[("count", n.to_string()), ("name", p.get_name())]),
            }
        }
        "sudo" => {
            let u = "/sudo <player> <command | c:message>";
            let Some(n) = arg(0) else { return say("usage", &[("arg", u.into())]) };
            let Some(t) = find_player(server, n) else { return say("no_player", &[("arg", n.to_owned())]) };
            let Some(action) = store::parse_sudo(&rest(1)) else { return say("usage", &[("arg", u.into())]) };
            // The console and command blocks stand above every player
            let mine = me.as_ref().map_or(4, |m| m.get_permission_level() as u8);
            if !store::sudo_allowed(mine, t.get_permission_level() as u8) {
                return say("sudo_higher", &[("name", t.get_name())]);
            }
            // Before the password is in, nothing may act for the player
            if c::awaiting_login(&t) {
                return say("sudo_login", &[("name", t.get_name())]);
            }
            let who = me.as_ref().map_or_else(|| "Console".to_owned(), Player::get_name);
            match action {
                Sudo::Chat(msg) => {
                    c::log_info(&format!("{who} made {} say: {msg}", t.get_name()));
                    if !more::on_chat(st, server, &t, &msg) {
                        // No chat format of ours: say it the way the game would
                        for r in server.get_all_players() {
                            r.send_system_message(TextComponent::text(&format!("<{}> {msg}", t.get_name())), false);
                        }
                    }
                    say("sudo_chat", &[("name", t.get_name())])
                }
                Sudo::Command(cmd) => {
                    let cmd = store::expand_alias(&st.aliases, &cmd).unwrap_or(cmd);
                    if !store::sudo_command_ok(&cmd) {
                        return say("sudo_refused", &[]);
                    }
                    c::log_info(&format!("{who} ran as {}: /{cmd}", t.get_name()));
                    after(After::RunAs { target: t.get_id(), command: cmd.clone() });
                    say("sudo_done", &[("name", t.get_name()), ("command", cmd)])
                }
            }
        }
        _ => None,
    }
}

// ---- join and quit lines ----------------------------------------------------

fn decor_vars(st: &mut State, server: &Server, p: &Player) -> Vec<(&'static str, String)> {
    let d = st.data(p).clone();
    let group = st.ranks.group_of(d.group.as_deref()).to_owned();
    let (prefix, suffix) = c::perms_decor(p).unwrap_or_else(|| {
        let (a, b) = st.ranks.decor(&group);
        (a.to_owned(), b.to_owned())
    });
    let mine = key(&p.get_id());
    let online = server
        .get_all_players()
        .iter()
        .filter(|o| {
            let k = key(&o.get_id());
            k != mine && !st.players.get(&k).is_some_and(|x| x.vanished)
        })
        .count();
    vec![
        ("name", d.nick.clone().unwrap_or_else(|| p.get_name())),
        ("player", p.get_name()),
        ("prefix", prefix),
        ("suffix", suffix),
        ("online", online.to_string()),
    ]
}

fn tell_logged_in(server: &Server, skip: Option<&str>, line: &str) {
    for r in server.get_all_players() {
        if skip == Some(key(&r.get_id()).as_str()) || c::awaiting_login(&r) {
            continue;
        }
        c::tell(&r, line);
    }
    c::log_info(&c::plain(line));
}

/// A player between joining and being welcomed.
pub(crate) struct Joining {
    /// Never welcomed before.
    pub first: bool,
    /// Was held at a login prompt.
    pub waited: bool,
}

pub(crate) fn on_join(st: &mut State, p: &Player, first: bool) {
    // Welcomed from the tick: a login plugin marks the player in its own join handler, which may
    // run after ours, and everything a player sees or gets on joining must wait for the password
    st.joining.insert(key(&p.get_id()), Joining { first, waited: false });
}

pub(crate) fn on_leave(st: &mut State, server: &Server, p: &Player) {
    let k = key(&p.get_id());
    st.joining.remove(&k);
    // Never announced (still at the login prompt, or vanished): leaving is silent too
    if !st.announced.remove(&k) {
        return;
    }
    let template = st.cfg.quit_message.clone();
    if template.trim().is_empty() || st.data(p).vanished {
        return;
    }
    let vars = decor_vars(st, server, p);
    tell_logged_in(server, Some(&k), &store::fill(&template, &vars));
}

/// Every tick while someone is waiting: welcome the players who are past the login prompt (at
/// once when there is none).
pub(crate) fn tick_joining(st: &mut State, server: &Server) {
    let waiting: Vec<String> = st.joining.keys().cloned().collect();
    for k in waiting {
        let Some(p) = crate::plugin::uuid_of(&k).and_then(|u| server.get_player_by_uuid(u)) else {
            st.joining.remove(&k);
            continue;
        };
        if c::awaiting_login(&p) {
            if let Some(j) = st.joining.get_mut(&k) {
                j.waited = true;
            }
            continue;
        }
        let Some(j) = st.joining.remove(&k) else { continue };
        if j.waited {
            // The login plugin held the player in place with teleports of its own: ours keep the gap
            st.tp_guard.insert(k.clone(), Instant::now());
        }
        st.data(&p).joined = true;
        st.dirty.insert(k.clone());
        more::welcome(st, &p);
        crate::extra::welcome(st, &p);
        if store::spawn_on_welcome(j.first, &st.cfg, st.spawn.is_some()) {
            crate::plugin::queue_spawn(st, &p);
        }
        if st.data(&p).vanished {
            continue;
        }
        st.announced.insert(k);
        let template = store::join_template(j.first, &st.cfg).to_owned();
        if template.trim().is_empty() {
            continue;
        }
        let mut vars = decor_vars(st, server, &p);
        // The newcomer counts
        if let Some(v) = vars.iter_mut().find(|(n, _)| *n == "online") {
            v.1 = (v.1.parse::<usize>().unwrap_or(0) + 1).to_string();
        }
        tell_logged_in(server, None, &store::fill(&template, &vars));
    }
}

// ---- ranks earned by play time ----------------------------------------------

fn auto_rank(st: &mut State, server: &Server, players: &[Player]) {
    if !st.ranks.groups.values().any(|g| g.after_hours > 0.0) {
        return;
    }
    let l = st.cfg.language.clone();
    for p in players {
        if c::awaiting_login(p) {
            continue;
        }
        let k = key(&p.get_id());
        let (rank, played, vanished) = {
            let d = st.data(p);
            (d.group.clone(), d.playtime, d.vanished)
        };
        let Some(new) = st.ranks.auto_rank(rank.as_deref(), played) else { continue };
        st.data(p).group = Some(new.clone());
        st.save(&k);
        more::apply_perms(st, p);
        more::refresh_name(st, p);
        c::log_info(&format!("{} reached rank {new} by play time", p.get_name()));
        if st.cfg.auto_rank_broadcast && !vanished {
            let line = fmt(&l, "auto_rank", &[("name", p.get_name()), ("group", new)]);
            tell_logged_in(server, None, &line);
        } else {
            c::tell(p, &fmt(&l, "rank_yours", &[("group", new)]));
        }
    }
}

// ---- boss bar announcements -------------------------------------------------

pub(crate) struct Bar {
    bar: BossBar,
    shown: Instant,
    secs: u64,
}

/// Show an announcement on a boss bar that empties over `announce-bossbar-seconds`.
pub(crate) fn announce_bar(st: &mut State, server: &Server, players: &[Player], line: &str) {
    let bar = BossBar::new(c::color(line), BossBarColor::Yellow, BossBarDivision::NoDivision);
    bar.set_health(1.0);
    for p in players {
        if c::awaiting_login(p) {
            continue;
        }
        // The API takes the player by value: a handle of its own for each call
        if let Some(own) = server.get_player_by_uuid(p.get_id()) {
            bar.add_player(own);
        }
    }
    c::log_info(&format!("<announce> {}", c::plain(line)));
    // Replacing the old bar drops it, which takes it off every screen
    st.bar = Some(Bar { bar, shown: Instant::now(), secs: st.cfg.announce_bossbar_seconds.clamp(1, 600) });
}

fn tick_bar(st: &mut State) {
    let Some(b) = &st.bar else { return };
    let gone = b.shown.elapsed().as_secs_f32();
    if gone >= b.secs as f32 {
        st.bar = None;
    } else {
        b.bar.set_health((1.0 - gone / b.secs as f32).clamp(0.0, 1.0));
    }
}

/// Once a second.
pub(crate) fn tick_second(st: &mut State, server: &Server, players: &[Player]) {
    tick_bar(st);
    if st.ticks % 100 == 0 {
        auto_rank(st, server, players);
    }
}

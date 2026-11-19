//! Pumpkin WASM glue for Quests: the window, counting deeds, NPCs, handing in and rewards.
//!
//! Deeds come through `mcsv_common::deeds`. The Npcs plugin tells this one when a player clicks
//! an NPC (`npc_click`) and when an NPC offers a quest (`offer`); the offer is a chat line with
//! an Accept button (Pumpkin 0.2.2 cannot send dialog windows from plugins). `players.json`
//! keeps every player's journal.
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use mcsv_common as c;
use c::deeds::{self, Deed, Kills};
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::GameMode;
use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, InventoryClickEvent, InventoryCloseEvent};
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

use crate::defs::{self, Config, Journal, NS, Note, Quest, Refusal};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&eQuests&8]&r ";
const FILE: &str = "players.json";
/// An offer at an NPC can be accepted for this long.
const OFFER_TICKS: u64 = 20 * 120;

enum Act {
    Offer { player: String, quest: String },
    Talk { player: String, npc: String },
}

struct State {
    cfg: Config,
    list: Vec<Quest>,
    notes: Vec<Note>,
    journals: HashMap<String, Journal>,
    /// Quests an NPC offered a player, and when.
    offers: HashMap<(String, String), u64>,
    kills: Kills,
    acts: Vec<Act>,
    /// Console commands of rewards, run once the state is let go.
    after: Vec<String>,
    wins: HashSet<String>,
    /// Quests handed in during this tick's clicks: the offer that comes with the same click
    /// says nothing about them.
    handed: HashSet<(String, String)>,
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

fn with_state(f: impl FnOnce(&mut State)) {
    if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
        f(st);
    }
}

fn msg(th: bool, thai: &str, english: &str) -> String {
    format!("{P}{}", if th { thai } else { english })
}

fn load() -> (Config, Vec<Quest>, Vec<Note>) {
    let cfg = c::load_config::<Config>("config.toml").cleaned();
    let mut notes = Vec::new();
    let first = !c::data_path("quests").exists();
    let _ = std::fs::create_dir_all(c::data_path("quests"));
    if first {
        let _ = std::fs::write(c::data_path("quests/example.toml"), defs::EXAMPLE);
    }
    let mut names: Vec<String> = std::fs::read_dir(c::data_path("quests")).map(|d| d.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.ends_with(".toml")).collect()).unwrap_or_default();
    names.sort();
    let mut all = Vec::new();
    for name in names {
        let file = format!("quests/{name}");
        match std::fs::read_to_string(c::data_path(&file)) {
            Ok(text) => all.extend(defs::parse_quests(&file, &text, &mut notes)),
            Err(_) => notes.push(Note::new(&file, "อ่านไฟล์ไม่ได้", "cannot be read")),
        }
    }
    let list = defs::merge_quests(all, &mut notes);
    (cfg, list, notes)
}

fn find<'a>(list: &'a [Quest], id: &str) -> Option<&'a Quest> {
    list.iter().find(|q| q.id == id)
}

fn span(th: bool, secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60);
    match (d, h, th) {
        (0, 0, true) => format!("{} นาที", m.max(1)),
        (0, 0, false) => format!("{}m", m.max(1)),
        (0, _, true) => format!("{h} ชม. {m} นาที"),
        (0, _, false) => format!("{h}h {m}m"),
        (_, _, true) => format!("{d} วัน {h} ชม."),
        (_, _, false) => format!("{d}d {h}h"),
    }
}

fn refusal_text(st: &State, r: &Refusal) -> String {
    let th = st.cfg.thai();
    match r {
        Refusal::Active => msg(th, "&7ทำเควสต์นี้อยู่แล้ว", "&7You have this quest already"),
        Refusal::Done => msg(th, "&7ทำเควสต์นี้ไปแล้ว", "&7You have done this quest"),
        Refusal::Later(s) => msg(th, &format!("&7ทำได้อีกครั้งในอีก {}", span(th, *s)), &format!("&7You can do it again in {}", span(th, *s))),
        Refusal::Needs(q) => {
            let name = find(&st.list, q).map_or(q.as_str(), |q| q.name.as_str());
            msg(th, &format!("&cต้องทำ {name} &cก่อน"), &format!("&cDo {name} &cfirst"))
        }
        Refusal::Full => msg(th, &format!("&cรับได้พร้อมกันไม่เกิน {} เควสต์", st.cfg.max_active), &format!("&cAt most {} quests at once", st.cfg.max_active)),
    }
}

fn goal_text(g: &defs::Goal, have: u32) -> String {
    let what = g.text.clone().unwrap_or_else(|| format!("{} {}", g.kind, g.target.trim_start_matches("minecraft:")));
    format!("{what} {}/{}", have.min(g.count), g.count)
}

// ---- taking, counting, handing in -------------------------------------------------------

fn take(st: &mut State, p: &Player, id: &str) -> String {
    let th = st.cfg.thai();
    let Some(q) = find(&st.list, id).cloned() else { return msg(th, "&cไม่มีเควสต์นี้", "&cNo such quest") };
    if let Some(node) = &q.permission
        && !p.has_permission(node)
    {
        return msg(th, "&cคุณยังรับเควสต์นี้ไม่ได้", "&cYou may not take this quest yet");
    }
    let me = c::game::key(p);
    if q.npc.is_some() {
        let offered = st.offers.get(&(me.clone(), q.id.clone())).is_some_and(|t| st.tick < t + OFFER_TICKS);
        if !offered {
            let npc = q.npc.clone().unwrap_or_default();
            return msg(th, &format!("&7รับเควสต์นี้ได้ที่ NPC {npc}"), &format!("&7Take this quest at NPC {npc}"));
        }
    }
    let journal = st.journals.entry(me.clone()).or_default();
    journal.name = p.get_name();
    if let Err(r) = defs::may_take(&q, journal, c::now_secs(), st.cfg.max_active) {
        let journal_text = refusal_text(st, &r);
        return journal_text;
    }
    journal.active.insert(q.id.clone(), vec![0; q.goals.len()]);
    st.offers.remove(&(me, q.id.clone()));
    st.dirty = true;
    let mut line = msg(th, &format!("&aรับเควสต์ {} &aแล้ว", q.name), &format!("&aQuest taken: {}", q.name));
    for g in &q.goals {
        line.push_str(&format!("\n&8- &7{}", goal_text(g, 0)));
    }
    line
}

/// How many of an item a player carries (plain items of the game, or CustomItems by id).
fn carried(p: &Player, item: &str) -> u32 {
    (0u8..36).filter_map(|s| p.get_inventory_item(s)).filter(|s| is_item(s, item)).map(|s| u32::from(s.get_count())).sum()
}

fn is_item(stack: &pumpkin_plugin_api::ItemStack, item: &str) -> bool {
    match item.strip_prefix("minecraft:") {
        Some(_) => c::game::kind_of(stack) == item && c::game::customitem_of(stack).is_none(),
        None => c::game::customitem_of(stack).as_deref() == Some(item),
    }
}

fn take_items(p: &Player, item: &str, mut n: u32) {
    for slot in 0u8..36 {
        if n == 0 {
            return;
        }
        let Some(stack) = p.get_inventory_item(slot).filter(|s| is_item(s, item)) else { continue };
        let have = u32::from(stack.get_count());
        if have <= n {
            p.set_inventory_item(slot, None);
            n -= have;
        } else {
            stack.set_count((have - n) as u8);
            p.set_inventory_item(slot, Some(stack));
            n = 0;
        }
    }
}

/// Hand a quest in. False (and the player is told why) when it is not done yet.
fn hand_in(st: &mut State, p: &Player, id: &str) -> bool {
    let th = st.cfg.thai();
    let me = c::game::key(p);
    let Some(q) = find(&st.list, id).cloned() else { return false };
    let Some(counts) = st.journals.get(&me).and_then(|j| j.active.get(id)).cloned() else { return false };
    if !defs::ready(&q, &counts) {
        return false;
    }
    for g in q.goals.iter().filter(|g| g.kind == "bring") {
        let have = carried(p, &g.target);
        if have < g.count {
            c::tell(p, &msg(th, &format!("&cยังขาด {} อีก {}", g.target.trim_start_matches("minecraft:"), g.count - have), &format!("&cYou still need {} more {}", g.count - have, g.target.trim_start_matches("minecraft:"))));
            return false;
        }
    }
    let items = q.rewards.iter().filter(|r| r.item.is_some()).count();
    if items > c::game::empty_slots(p) {
        c::tell(p, &msg(th, &format!("&cต้องมีช่องว่างในกระเป๋า {items} ช่องเพื่อรับรางวัล"), &format!("&cMake room for {items} reward item(s) first")));
        return false;
    }
    for g in q.goals.iter().filter(|g| g.kind == "bring") {
        take_items(p, &g.target, g.count);
    }
    let name = p.get_name();
    for r in &q.rewards {
        if let Some(item) = &r.item
            && !c::game::give_item(p, item, r.count)
        {
            c::log_info(&format!("could not give {item} x{} to {name} for quest {} (CustomItems did not answer)", r.count, q.id));
        }
        if r.money > 0.0 && c::game::money::deposit(&name, r.money).is_none() {
            c::log_info(&format!("could not pay {} to {name} for quest {} (Basics did not answer)", r.money, q.id));
        }
        st.after.extend(r.commands.iter().map(|cmd| cmd.replace("{player}", &name)));
    }
    if let Some(j) = st.journals.get_mut(&me) {
        j.active.remove(id);
        j.done.insert(id.to_owned(), c::now_secs());
    }
    st.handed.insert((me.clone(), id.to_owned()));
    st.dirty = true;
    c::tell(p, &msg(th, &format!("&aเควสต์ {} &aสำเร็จ!", q.name), &format!("&aQuest complete: {}", q.name)));
    for r in q.rewards.iter().filter_map(|r| r.text.as_ref()) {
        c::tell(p, &format!("&8- &f{r}"));
    }
    p.play_custom_sound("minecraft:ui.toast.challenge_complete", SoundCategory::Players, 0.6, 1.0);
    true
}

/// A deed of a player: count it for every quest going on.
fn count(st: &mut State, p: &Player, kind: &str, target: &str, n: u32) {
    if matches!(p.get_gamemode(), GameMode::Spectator) {
        return;
    }
    let th = st.cfg.thai();
    let me = c::game::key(p);
    let Some(journal) = st.journals.get_mut(&me) else { return };
    let mut finished = Vec::new();
    for (id, counts) in &mut journal.active {
        let Some(q) = st.list.iter().find(|q| q.id == *id) else { continue };
        if !defs::advance(q, counts, kind, target, n) {
            continue;
        }
        st.dirty = true;
        let line: Vec<String> = q.goals.iter().zip(counts.iter()).filter(|(g, _)| g.kind != "bring").map(|(g, have)| goal_text(g, *have)).collect();
        c::action_bar(p, &format!("&e{} &8| &f{}", q.name, line.join(" &8| &f")));
        if defs::ready(q, counts) {
            match &q.npc {
                Some(npc) => c::tell(p, &msg(th, &format!("&a{} &aครบแล้ว กลับไปหา NPC {npc}", q.name), &format!("&a{} &ais done: go back to NPC {npc}", q.name))),
                None => finished.push(q.id.clone()),
            }
        }
    }
    for id in finished {
        hand_in(st, p, &id);
    }
}

fn offer(st: &mut State, p: &Player, id: &str) {
    let th = st.cfg.thai();
    let Some(q) = find(&st.list, id).cloned() else { return };
    let me = c::game::key(p);
    let journal = st.journals.get(&me).cloned().unwrap_or_default();
    // A quest going on, or just handed in, was dealt with by the click itself (`talk`)
    if journal.active.contains_key(id) || st.handed.contains(&(me.clone(), q.id.clone())) {
        return;
    }
    if let Err(r) = defs::may_take(&q, &journal, c::now_secs(), st.cfg.max_active) {
        c::tell(p, &refusal_text(st, &r));
        return;
    }
    st.offers.insert((me, q.id.clone()), st.tick);
    c::tell(p, &format!("&8&m        &r &e{} &8&m        ", q.name));
    for d in &q.description {
        c::tell(p, &format!("&7{d}"));
    }
    for g in &q.goals {
        c::tell(p, &format!("&8- &f{}", goal_text(g, 0)));
    }
    let accept = c::color(if th { "&a[รับเควสต์]" } else { "&a[Accept]" }).click_run_command(&format!("/quests accept {}", q.id)).hover_show_text(c::color(&q.name));
    p.send_system_message(TextComponent::text("").add_child(accept), false);
}

/// An NPC was clicked: talk goals, and the quests handed in there (or how far they are).
fn talk(st: &mut State, p: &Player, npc: &str) {
    let th = st.cfg.thai();
    count(st, p, "talk", npc, 1);
    let me = c::game::key(p);
    let here: Vec<String> = st.journals.get(&me).map(|j| j.active.keys().filter(|id| find(&st.list, id).is_some_and(|q| q.npc.as_deref() == Some(npc))).cloned().collect()).unwrap_or_default();
    for id in here {
        let Some(q) = find(&st.list, &id).cloned() else { continue };
        let counts = st.journals.get(&me).and_then(|j| j.active.get(&id)).cloned().unwrap_or_default();
        if defs::ready(&q, &counts) {
            // Says itself what is still missing (items to bring, room for rewards)
            hand_in(st, p, &id);
        } else {
            let line: Vec<String> = q.goals.iter().enumerate().map(|(i, g)| goal_text(g, if g.kind == "bring" { carried(p, &g.target) } else { counts.get(i).copied().unwrap_or(0) })).collect();
            c::tell(p, &msg(th, &format!("&e{} &7ยังไม่ครบ: {}", q.name, line.join(", ")), &format!("&e{} &7is not done: {}", q.name, line.join(", "))));
        }
    }
}

fn on_tick(server: Server) {
    let jobs = {
        let Some(mut guard) = try_lock() else { return };
        let Some(st) = guard.as_mut() else { return };
        st.tick += 1;
        for a in std::mem::take(&mut st.acts) {
            match a {
                Act::Offer { player, quest } => {
                    if let Some(p) = c::game::online(&server, &player) {
                        offer(st, &p, &quest);
                    }
                }
                Act::Talk { player, npc } => {
                    if let Some(p) = c::game::online(&server, &player) {
                        talk(st, &p, &npc);
                    }
                }
            }
        }
        st.handed.clear();
        if st.dirty && st.tick % 1200 == 0 && c::save_json(FILE, &st.journals) {
            st.dirty = false;
        }
        std::mem::take(&mut st.after)
    };
    for cmd in jobs {
        server.execute_command(&cmd, ServerSender::Console);
    }
}

#[derive(Clone, Copy)]
struct Deeds;

impl deeds::Sink for Deeds {
    fn deed(&self, _server: &Server, p: &Player, deed: Deed, _spot: Option<deeds::Spot>) {
        with_state(|st| count(st, p, deed.kind(), deed.target(), deed.count()));
    }

    fn with_kills(&self, f: &mut dyn FnMut(&mut Kills, u64)) {
        with_state(|st| f(&mut st.kills, st.tick));
    }
}

// ---- the window --------------------------------------------------------------------

fn open(st: &mut State, p: &Player) {
    let th = st.cfg.thai();
    let me = c::game::key(p);
    let journal = st.journals.get(&me).cloned().unwrap_or_default();
    let gui = Gui::new(Screen::Generic9x6, c::color(if th { "&8เควสต์" } else { "&8Quests" }));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    let inv = gui.get_inventory();
    let now = c::now_secs();
    for (slot, q) in st.list.iter().filter(|q| q.permission.as_ref().is_none_or(|n| p.has_permission(n))).take(54).enumerate() {
        let mut lore: Vec<String> = q.description.iter().map(|d| format!("&7{d}")).collect();
        let (state, glint) = match (journal.active.get(&q.id), defs::may_take(q, &journal, now, st.cfg.max_active)) {
            (Some(counts), _) => {
                for (i, g) in q.goals.iter().enumerate() {
                    let have = if g.kind == "bring" { carried(p, &g.target) } else { counts.get(i).copied().unwrap_or(0) };
                    lore.push(format!("&8- &f{}", goal_text(g, have)));
                }
                (if th { "&eกำลังทำ &7| คลิกขวาเพื่อยกเลิก" } else { "&eGoing on &7| right click to give up" }.to_owned(), true)
            }
            (None, Ok(())) => {
                for g in &q.goals {
                    lore.push(format!("&8- &7{}", goal_text(g, 0)));
                }
                match &q.npc {
                    Some(npc) => (if th { format!("&7รับได้ที่ NPC {npc}") } else { format!("&7Take it at NPC {npc}") }, false),
                    None => (if th { "&aคลิกเพื่อรับ" } else { "&aClick to take it" }.to_owned(), false),
                }
            }
            (None, Err(r)) => (refusal_text(st, &r).replace(P, ""), false),
        };
        lore.push(state);
        let stack = c::game::button(&q.icon, &q.name, &lore);
        if glint {
            stack.set_component(pumpkin_plugin_api::data_components::DataComponent::EnchantmentGlintOverride, &[1]);
        }
        inv.set_item(slot as u32, Some(stack));
    }
    st.wins.insert(me);
    p.open_gui(gui);
}

struct OnClick;
impl EventHandler<InventoryClickEvent> for OnClick {
    fn handle(&self, _server: Server, mut e: Data<InventoryClickEvent>) -> Data<InventoryClickEvent> {
        if !matches!(e.window_type, Some(Screen::Generic9x6)) {
            return e;
        }
        with_state(|st| {
            let me = c::game::key(&e.player);
            if !st.wins.contains(&me) {
                return;
            }
            e.cancelled = true;
            let shown: Vec<String> = st.list.iter().filter(|q| q.permission.as_ref().is_none_or(|n| e.player.has_permission(n))).take(54).map(|q| q.id.clone()).collect();
            let Some(id) = usize::try_from(e.raw_slot).ok().and_then(|s| shown.get(s)).cloned() else { return };
            let active = st.journals.get(&me).is_some_and(|j| j.active.contains_key(&id));
            let right = matches!(e.click_type, pumpkin_plugin_api::common::ClickType::Right);
            let line = if active && right {
                if let Some(j) = st.journals.get_mut(&me) {
                    j.active.remove(&id);
                }
                st.dirty = true;
                msg(st.cfg.thai(), "&7ยกเลิกเควสต์แล้ว", "&7Quest given up")
            } else if active {
                return;
            } else {
                take(st, &e.player, &id)
            };
            c::tell(&e.player, &line);
            open(st, &e.player);
        });
        e
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

struct OnClose;
impl EventHandler<InventoryCloseEvent> for OnClose {
    fn handle(&self, _server: Server, e: Data<InventoryCloseEvent>) -> Data<InventoryCloseEvent> {
        with_state(|st| {
            st.wins.remove(&c::game::key(&e.player));
        });
        e
    }
}

// ---- commands ----------------------------------------------------------------------

fn run(st: &mut State, sender: &CommandSender, server: &Server, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    let me = c::sender_player(sender);
    let admin = c::has_perm(sender, &format!("{NS}:admin"));
    let in_game = || vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")];
    let target_key = |name: &str, st: &State| server.get_all_players().into_iter().find(|p| p.get_name().eq_ignore_ascii_case(name)).map(|p| c::game::key(&p)).or_else(|| st.journals.iter().find(|(_, j)| j.name.eq_ignore_ascii_case(name)).map(|(k, _)| k.clone()));
    match w.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        None => {
            let Some(p) = me else { return in_game() };
            open(st, &p);
            Vec::new()
        }
        Some("accept" | "take") => match (me, w.get(1)) {
            (Some(p), Some(id)) => vec![take(st, &p, id)],
            (None, _) => in_game(),
            _ => vec![msg(th, "&f/quests accept <เควสต์>", "&f/quests accept <quest>")],
        },
        Some("abandon") => {
            let (Some(p), Some(id)) = (me, w.get(1)) else { return vec![msg(th, "&f/quests abandon <เควสต์>", "&f/quests abandon <quest>")] };
            let removed = st.journals.get_mut(&c::game::key(&p)).and_then(|j| j.active.remove(id)).is_some();
            st.dirty |= removed;
            vec![if removed { msg(th, "&7ยกเลิกแล้ว", "&7Given up") } else { msg(th, "&cคุณไม่ได้ทำเควสต์นี้", "&cYou do not have this quest") }]
        }
        Some("list") => {
            let Some(p) = me else { return in_game() };
            let Some(j) = st.journals.get(&c::game::key(&p)).filter(|j| !j.active.is_empty()) else { return vec![msg(th, "&7ไม่มีเควสต์ที่ทำอยู่: /quests", "&7No quests going on: /quests")] };
            let mut out = Vec::new();
            for (id, counts) in &j.active {
                let Some(q) = find(&st.list, id) else { continue };
                let line: Vec<String> = q.goals.iter().enumerate().map(|(i, g)| goal_text(g, if g.kind == "bring" { carried(&p, &g.target) } else { counts.get(i).copied().unwrap_or(0) })).collect();
                out.push(format!("&e{} &8| &f{}", q.name, line.join(" &8| &f")));
            }
            out
        }
        Some("reload") if admin => {
            let (cfg, list, notes) = load();
            (st.cfg, st.list, st.notes) = (cfg, list, notes);
            let mut out = vec![msg(st.cfg.thai(), &format!("&aอ่านใหม่แล้ว: {} เควสต์", st.list.len()), &format!("&aRead again: {} quest(s)", st.list.len()))];
            out.extend(st.notes.iter().take(20).map(|n| format!("&6! &7{}", n.text(th))));
            out
        }
        Some("reset") if admin => {
            let Some(name) = w.get(1) else { return vec![msg(th, "&f/quests reset <ผู้เล่น> [เควสต์]", "&f/quests reset <player> [quest]")] };
            let Some(k) = target_key(name, st) else { return vec![msg(th, "&cไม่รู้จักผู้เล่นนี้", "&cUnknown player")] };
            if let Some(j) = st.journals.get_mut(&k) {
                match w.get(2) {
                    Some(id) => {
                        j.active.remove(id);
                        j.done.remove(id);
                    }
                    None => {
                        j.active.clear();
                        j.done.clear();
                    }
                }
            }
            st.dirty = true;
            vec![msg(th, "&aล้างแล้ว", "&aReset")]
        }
        Some("complete") if admin => {
            let (Some(name), Some(id)) = (w.get(1), w.get(2)) else { return vec![msg(th, "&f/quests complete <ผู้เล่น> <เควสต์>", "&f/quests complete <player> <quest>")] };
            let Some(p) = server.get_all_players().into_iter().find(|p| p.get_name().eq_ignore_ascii_case(name)) else { return vec![msg(th, "&cผู้เล่นไม่ได้ออนไลน์", "&cThat player is not online")] };
            let Some(q) = find(&st.list, id).cloned() else { return vec![msg(th, "&cไม่มีเควสต์นี้", "&cNo such quest")] };
            let j = st.journals.entry(c::game::key(&p)).or_default();
            j.name = p.get_name();
            j.active.insert(q.id.clone(), q.goals.iter().map(|g| g.count).collect());
            if hand_in(st, &p, id) { vec![msg(th, "&aให้สำเร็จแล้ว", "&aCompleted")] } else { vec![msg(th, "&6ยังส่งไม่ได้ (ดูข้อความที่ผู้เล่นได้)", "&6Not handed in yet (see what the player was told)")] }
        }
        Some("version") => vec![format!("Quests {VERSION}")],
        _ => {
            let mut out = vec![msg(th, "&f/quests &7เปิดหน้าต่าง &7| &faccept <เควสต์> &7| &fabandon <เควสต์> &7| &flist", "&f/quests &7open the window &7| &faccept <quest> &7| &fabandon <quest> &7| &flist")];
            if admin {
                out.push(msg(th, "&f/quests reload &7| &freset <ผู้เล่น> [เควสต์] &7| &fcomplete <ผู้เล่น> <เควสต์> &7| &fversion", "&f/quests reload &7| &freset <player> [quest] &7| &fcomplete <player> <quest> &7| &fversion"));
            }
            out
        }
    }
}

struct Cmd;
impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let lines = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            let Some(st) = guard.as_mut() else { return Ok(0) };
            run(st, &sender, &server, &w)
        };
        for line in lines {
            for part in line.split('\n') {
                c::reply(&sender, part);
            }
        }
        Ok(1)
    }
}

// ---- the plugin ------------------------------------------------------------------

struct Quests;

impl Plugin for Quests {
    fn new() -> Self {
        Quests
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Quests with goals and rewards, given by NPCs or from a window".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:use"), description: "Do quests".into(), default: PermissionDefault::Allow, children: vec![] });
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Manage quests".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let (cfg, list, notes) = load();
        let journals: HashMap<String, Journal> = c::load_json(FILE).unwrap_or_default();
        c::log_info(&format!("{} quest(s){}", list.len(), if notes.is_empty() { String::new() } else { format!(", {} thing(s) to look at (/quests reload)", notes.len()) }));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg, list, notes, journals, offers: HashMap::new(), kills: Kills::default(), acts: Vec::new(), after: Vec::new(), wins: HashSet::new(), handed: HashSet::new(), tick: 0, dirty: false });
        let cmd = Command::new(&[NS.to_owned(), "quest".to_owned()], "Quests").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:use"));
        deeds::listen(&context, Deeds)?;
        context.register_event_handler(OnClick, EventPriority::Normal, true)?;
        context.register_event_handler(OnClose, EventPriority::Normal, false)?;
        context.schedule_repeating_task(1, 1, on_tick);
        c::mark_running(VERSION);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::mark_stopped();
        if let Ok(mut guard) = STATE.lock()
            && let Some(st) = guard.as_mut()
        {
            c::save_json(FILE, &st.journals);
        }
        Ok(())
    }

    /// The Npcs plugin: `{"kind":"offer","player":<uuid hex>,"quest":<id>}` when an NPC with the
    /// action `quest <id>` is clicked, `{"kind":"npc_click","player","npc"}` for every click.
    /// Others: `{"kind":"quests"}` lists the quests; `placeholder` gives `%quests_done%`,
    /// `%quests_active%`.
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let mut guard = try_lock().ok_or("busy")?;
        let st = guard.as_mut().ok_or("not loaded")?;
        let text = |k: &str| v.get(k).and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
        let key = |k: &str| text(k).replace('-', "").to_ascii_lowercase();
        let answer = match text("kind").as_str() {
            "offer" => {
                st.acts.push(Act::Offer { player: key("player"), quest: text("quest") });
                serde_json::json!({ "ok": true })
            }
            "npc_click" => {
                st.acts.push(Act::Talk { player: key("player"), npc: text("npc") });
                serde_json::json!({ "ok": true })
            }
            "quests" => serde_json::json!({ "ok": true, "quests": st.list.iter().map(|q| serde_json::json!({ "id": q.id, "name": q.name, "npc": q.npc })).collect::<Vec<_>>() }),
            "placeholder" => {
                let j = st.journals.get(&key("uuid"));
                let value = match text("params").as_str() {
                    "done" => Some(j.map_or(0, |j| j.done.len()).to_string()),
                    "active" => Some(j.map_or(0, |j| j.active.len()).to_string()),
                    _ => None,
                };
                serde_json::json!({ "value": value })
            }
            other => return Err(format!("unknown kind {other}")),
        };
        serde_json::to_vec(&answer).map_err(|e| e.to_string())
    }
}

register_plugin!(Quests);

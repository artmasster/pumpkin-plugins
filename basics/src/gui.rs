//! Basics windows: `/invsee`, `/ec <player>`, `/trash` and the server shop (`/shop`).
//!
//! A window is a `Gui` the server shows; its `Inventory` handle stays valid after `open_gui` took
//! the `Gui`, and changing it changes what the viewer sees. Pumpkin 0.2.2 has no call that closes
//! a window from the server side: `close_for` can only tell the client to close it.
use std::time::Instant;

use mcsv_common as c;
use pumpkin_plugin_api::common::ClickType;
use pumpkin_plugin_api::gui::Gui;
use pumpkin_plugin_api::inventory::Inventory;
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::screens_wit::Screen;
use pumpkin_plugin_api::text::TextComponent;
use pumpkin_plugin_api::{ItemStack, Server};

use crate::lang::{fmt, text};
use crate::plugin::{State, key, uuid_of};
use crate::store::{self, SHOP_PAGE, ShopError};

/// Item id, count and a hash of its components: enough to tell that a slot changed.
type Sig = (String, u8, u64);

pub(crate) enum Kind {
    /// Whatever is inside when it closes is gone.
    Trash,
    /// A live copy of another player's inventory or ender chest.
    Mirror { target: String, ender: bool, modify: bool },
    /// `cat` = the open category (`None` = the category menu).
    Shop { cat: Option<String>, page: usize },
}

pub(crate) struct Win {
    kind: Kind,
    screen: Screen,
    size: u32,
    inv: Inventory,
    /// Mirror only: what each slot held when both sides last agreed.
    snap: Vec<Option<Sig>>,
}

const INVSEE_SIZE: u32 = 45;
const ENDER_SIZE: u32 = 27;
const SHOP_SIZE: u32 = 54;
const TRASH_SIZE: u32 = 36;
const SLOT_PREV: u32 = 45;
const SLOT_PAGE: u32 = 48;
const SLOT_BACK: u32 = 49;
const SLOT_MONEY: u32 = 50;
const SLOT_NEXT: u32 = 53;

/// Where a mirror slot lives on the target.
#[derive(Clone, Copy)]
enum Src {
    Main(u8),
    Helmet,
    Chest,
    Legs,
    Boots,
    Off,
    Ender(u8),
    Filler,
}

/// /invsee: three rows of storage, the hotbar, then armor and off hand.
fn src_of(ender: bool, slot: u32) -> Src {
    if ender {
        return if slot < ENDER_SIZE { Src::Ender(slot as u8) } else { Src::Filler };
    }
    match slot {
        0..=26 => Src::Main(slot as u8 + 9),
        27..=35 => Src::Main(slot as u8 - 27),
        36 => Src::Helmet,
        37 => Src::Chest,
        38 => Src::Legs,
        39 => Src::Boots,
        40 => Src::Off,
        _ => Src::Filler,
    }
}

fn short(id: &str) -> &str {
    id.strip_prefix("minecraft:").unwrap_or(id)
}

fn is_empty(s: &ItemStack) -> bool {
    s.get_count() == 0 || s.get_registry_key().ends_with("air")
}

fn some(s: Option<ItemStack>) -> Option<ItemStack> {
    s.filter(|s| !is_empty(s))
}

fn read(t: &Player, src: Src) -> Option<ItemStack> {
    some(match src {
        Src::Main(n) => t.get_inventory_item(n),
        Src::Helmet => t.get_inventory().get_helmet(),
        Src::Chest => t.get_inventory().get_chestplate(),
        Src::Legs => t.get_inventory().get_leggings(),
        Src::Boots => t.get_inventory().get_boots(),
        Src::Off => t.get_inventory().get_off_hand(),
        Src::Ender(n) => t.get_ender_chest_item(n),
        Src::Filler => None,
    })
}

fn write(t: &Player, src: Src, item: Option<ItemStack>) {
    match src {
        Src::Main(n) => t.set_inventory_item(n, item),
        Src::Helmet => t.get_inventory().set_helmet(item),
        Src::Chest => t.get_inventory().set_chestplate(item),
        Src::Legs => t.get_inventory().set_leggings(item),
        Src::Boots => t.get_inventory().set_boots(item),
        Src::Off => t.get_inventory().set_off_hand(item),
        Src::Ender(n) => t.set_ender_chest_item(n, item),
        Src::Filler => {}
    }
}

fn sig(s: Option<&ItemStack>) -> Option<Sig> {
    let s = s.filter(|s| !is_empty(s))?;
    // FNV-1a over the components the stack carries (damage, name, enchantments, contents…)
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for comp in s.get_components() {
        eat(comp.component as u8);
        eat(0xff);
        comp.value.iter().for_each(|b| eat(*b));
    }
    Some((s.get_registry_key(), s.get_count(), h))
}

/// A stack with nothing on it: the only kind the shop takes or tops up.
fn plain(s: &ItemStack) -> bool {
    s.get_custom_name().is_none() && s.get_enchantments().is_empty() && s.get_components().is_empty()
}

/// Text for an item name or lore line: the client slants those unless told otherwise.
fn upright(text: &str) -> TextComponent {
    TextComponent::text("").italic(false).add_child(c::color(text))
}

fn named(id: &str, name: &str, lore: &[String]) -> ItemStack {
    let mut s = ItemStack::new(id, 1);
    if is_empty(&s) {
        s = ItemStack::new("minecraft:paper", 1);
    }
    s.set_custom_name(Some(upright(name)));
    if !lore.is_empty() {
        s.set_lore(lore.iter().map(|l| upright(l)).collect::<Vec<_>>());
    }
    s
}

fn new_gui(screen: Screen, title: &str, locked: bool) -> (Gui, Inventory) {
    let gui = Gui::new(screen, c::color(title));
    gui.set_allow_grab_items(!locked);
    gui.set_allow_put_items(!locked);
    let inv = gui.get_inventory();
    (gui, inv)
}

fn online(server: &Server, k: &str) -> Option<Player> {
    uuid_of(k).and_then(|u| server.get_player_by_uuid(u))
}

/// End the window a player has, the way a close would: last edits of a mirror reach the target,
/// a trash can is emptied. Call before opening another window: the close event the server fires
/// from inside `open_gui` finds the state busy and is skipped.
pub(crate) fn finish(st: &mut State, server: &Server, p: &Player) {
    let k = key(&p.get_id());
    let Some(mut win) = st.wins.remove(&k) else { return };
    match &win.kind {
        Kind::Trash => {
            let n: u32 = win.inv.get_all_items().iter().flatten().filter(|s| !is_empty(s)).map(|s| u32::from(s.get_count())).sum();
            if n > 0 {
                c::tell(p, &fmt(&st.cfg.language, "trash_done", &[("count", n.to_string())]));
            }
        }
        Kind::Mirror { target, modify: true, .. } => {
            if let Some(t) = online(server, target) {
                sync(&mut win, &t, false);
            }
        }
        _ => {}
    }
    // Pumpkin keeps the window open on its side until the client closes something: leave nothing
    // in it that a later click could take
    win.inv.clear();
}

/// Close a window from our side: the client is told to close it and the session ends.
fn close_for(st: &mut State, server: &Server, p: &Player) {
    finish(st, server, p);
    if let Some(java) = p.as_java() {
        use pumpkin_plugin_api::java_packets::{CCloseContainer, ClientboundPacket};
        java.send_packet(&ClientboundPacket::CCloseContainer(CCloseContainer { sync_id: 0 }));
    }
}

// ---- trash ------------------------------------------------------------------

pub(crate) fn open_trash(st: &mut State, server: &Server, p: &Player) {
    finish(st, server, p);
    let (gui, inv) = new_gui(Screen::Generic9x4, text(&st.cfg.language, "trash_title"), false);
    st.wins.insert(key(&p.get_id()), Win { kind: Kind::Trash, screen: Screen::Generic9x4, size: TRASH_SIZE, inv, snap: Vec::new() });
    p.open_gui(gui);
}

// ---- mirrors (/invsee, /ec <player>) ----------------------------------------

/// Bring the window and the target's inventory together. A slot the viewer changed goes to the
/// target (only when the viewer may modify); with `pull`, a slot the target changed comes to the window.
fn sync(win: &mut Win, t: &Player, pull: bool) {
    let Kind::Mirror { ender, modify, .. } = &win.kind else { return };
    let (ender, modify) = (*ender, *modify);
    for i in 0..win.size {
        let src = src_of(ender, i);
        if matches!(src, Src::Filler) {
            continue;
        }
        let at = i as usize;
        let g = some(win.inv.get_item(i));
        let gs = sig(g.as_ref());
        if modify && gs != win.snap[at] {
            write(t, src, g);
            win.snap[at] = gs;
            continue;
        }
        if !pull {
            continue;
        }
        let tv = read(t, src);
        let ts = sig(tv.as_ref());
        if ts != win.snap[at] || gs != win.snap[at] {
            win.inv.set_item(i, tv);
            win.snap[at] = ts;
        }
    }
}

/// Open another player's inventory (`ender` = their ender chest) for `viewer`.
pub(crate) fn open_mirror(st: &mut State, server: &Server, viewer: &Player, target: &Player, ender: bool, modify: bool) {
    finish(st, server, viewer);
    let l = st.cfg.language.clone();
    let (screen, size) = if ender { (Screen::Generic9x3, ENDER_SIZE) } else { (Screen::Generic9x5, INVSEE_SIZE) };
    let title = store::fill(text(&l, if ender { "ec_title" } else { "invsee_title" }), &[("name", target.get_name())]);
    let (gui, inv) = new_gui(screen, &title, !modify);
    let mut snap = Vec::with_capacity(size as usize);
    for i in 0..size {
        let src = src_of(ender, i);
        if matches!(src, Src::Filler) {
            inv.set_item(i, Some(named("minecraft:gray_stained_glass_pane", " ", &[])));
            snap.push(None);
            continue;
        }
        let item = read(target, src);
        snap.push(sig(item.as_ref()));
        inv.set_item(i, item);
    }
    let kind = Kind::Mirror { target: key(&target.get_id()), ender, modify };
    st.wins.insert(key(&viewer.get_id()), Win { kind, screen, size, inv, snap });
    viewer.open_gui(gui);
}

/// Every few ticks: keep mirrors equal to their targets; close the ones whose target left.
pub(crate) fn tick(st: &mut State, server: &Server) {
    let mirrors: Vec<(String, String)> = st
        .wins
        .iter()
        .filter_map(|(k, w)| match &w.kind {
            Kind::Mirror { target, .. } => Some((k.clone(), target.clone())),
            _ => None,
        })
        .collect();
    for (vk, tk) in mirrors {
        let Some(viewer) = online(server, &vk) else {
            if let Some(w) = st.wins.remove(&vk) {
                w.inv.clear();
            }
            continue;
        };
        match online(server, &tk) {
            Some(t) => {
                if let Some(win) = st.wins.get_mut(&vk) {
                    sync(win, &t, true);
                }
            }
            None => {
                // Nothing to write back to: drop the copy before anything can be taken from it
                close_for(st, server, &viewer);
                c::tell(&viewer, &fmt(&st.cfg.language, "win_gone", &[]));
            }
        }
    }
}

// ---- shop -------------------------------------------------------------------

/// Read shop.toml (written with examples the first time) and drop what cannot be used.
pub(crate) fn load_shop(lang: &str) -> store::Shop {
    if !c::data_path("shop.toml").exists() {
        c::save_config("shop.toml", &store::Shop::sample(lang != "en"));
    }
    let mut shop: store::Shop = c::load_config("shop.toml");
    let mut notes = shop.clean();
    shop.items.retain(|i| {
        let known = !is_empty(&ItemStack::new(&i.item, 1));
        if !known {
            notes.push(format!("item {}: unknown to this Minecraft version", i.item));
        }
        known
    });
    for n in notes {
        c::log_info(&format!("shop.toml: {n}"));
    }
    shop
}

fn money(st: &State, v: f64) -> String {
    store::money(v, &st.cfg.currency_symbol)
}

fn draw_shop(st: &mut State, p: &Player) {
    let k = key(&p.get_id());
    let l = st.cfg.language.clone();
    let balance = st.data(p).balance;
    let Some(win) = st.wins.get(&k) else { return };
    let Kind::Shop { cat, page } = &win.kind else { return };
    let inv = &win.inv;
    inv.clear();
    let line = |key: &str, vars: &[(&str, String)]| store::fill(text(&l, key), vars);
    match cat {
        None => {
            for (slot, name) in st.shop.menu() {
                let Some(cdef) = st.shop.categories.get(&name) else { continue };
                let count = st.shop.items.iter().filter(|i| i.category == name).count();
                let title = if cdef.title.trim().is_empty() { format!("&e{name}") } else { cdef.title.clone() };
                let icon = if cdef.icon.trim().is_empty() { "minecraft:chest" } else { cdef.icon.as_str() };
                let icon = store::parse_item(icon).map_or_else(|| "minecraft:chest".to_owned(), |(id, _)| id);
                inv.set_item(slot as u32, Some(named(&icon, &title, &[line("shop_cat_lore", &[("count", count.to_string())])])));
            }
        }
        Some(cat) => {
            let layout = st.shop.layout(cat);
            let pages = layout.len() / SHOP_PAGE;
            let page = (*page).min(pages - 1);
            for i in 0..SHOP_PAGE {
                let Some(item) = layout[page * SHOP_PAGE + i].and_then(|n| st.shop.items.get(n)) else { continue };
                let s = ItemStack::new(&item.item, 1);
                let stack = s.get_max_count().clamp(1, 64);
                let mut lore = Vec::new();
                if item.buy > 0.0 {
                    lore.push(line("shop_buy_lore", &[("money", money(st, item.buy)), ("stack", stack.to_string())]));
                }
                if item.sell > 0.0 {
                    lore.push(line("shop_sell_lore", &[("money", money(st, item.sell))]));
                }
                s.set_lore(lore.iter().map(|x| upright(x)).collect::<Vec<_>>());
                inv.set_item(i as u32, Some(s));
            }
            if page > 0 {
                inv.set_item(SLOT_PREV, Some(named("minecraft:arrow", text(&l, "shop_prev"), &[])));
            }
            if page + 1 < pages {
                inv.set_item(SLOT_NEXT, Some(named("minecraft:arrow", text(&l, "shop_next"), &[])));
            }
            if pages > 1 {
                inv.set_item(SLOT_PAGE, Some(named("minecraft:paper", &line("shop_page", &[("page", (page + 1).to_string()), ("pages", pages.to_string())]), &[])));
            }
            if st.shop.categories.len() > 1 {
                inv.set_item(SLOT_BACK, Some(named("minecraft:oak_door", text(&l, "shop_back"), &[])));
            }
        }
    }
    inv.set_item(SLOT_MONEY, Some(named("minecraft:gold_ingot", &line("shop_balance", &[("money", money(st, balance))]), &[])));
}

/// Open the shop: the category menu, or one category when the shop has only one or `cat` names it.
pub(crate) fn open_shop(st: &mut State, server: &Server, p: &Player, cat: Option<&str>) -> Vec<String> {
    let l = st.cfg.language.clone();
    if st.shop.items.is_empty() || st.shop.categories.is_empty() {
        return vec![fmt(&l, "shop_empty", &[])];
    }
    let cat = match cat.map(str::to_lowercase) {
        Some(name) if st.shop.categories.contains_key(&name) => Some(name),
        Some(name) => return vec![fmt(&l, "shop_no_cat", &[("name", name), ("list", st.shop.categories.keys().cloned().collect::<Vec<_>>().join(", "))])],
        None if st.shop.categories.len() == 1 => st.shop.categories.keys().next().cloned(),
        None => None,
    };
    finish(st, server, p);
    let title = if st.shop.title.trim().is_empty() { text(&l, "shop_title").to_owned() } else { st.shop.title.clone() };
    let (gui, inv) = new_gui(Screen::Generic9x6, &title, true);
    st.wins.insert(key(&p.get_id()), Win { kind: Kind::Shop { cat, page: 0 }, screen: Screen::Generic9x6, size: SHOP_SIZE, inv, snap: Vec::new() });
    draw_shop(st, p);
    p.open_gui(gui);
    Vec::new()
}

/// Put `count` plain items into the main inventory: existing plain stacks first, then empty slots.
/// Nothing is given when not all of it fits.
fn give_plain(p: &Player, id: &str, count: u32, max: u32) -> bool {
    let want = short(id);
    let mut plan: Vec<(u8, u32, bool)> = Vec::new();
    let mut left = count;
    let mut empty_slots = Vec::new();
    for slot in 0u8..36 {
        if left == 0 {
            break;
        }
        match some(p.get_inventory_item(slot)) {
            None => empty_slots.push(slot),
            Some(s) => {
                let have = u32::from(s.get_count());
                if short(&s.get_registry_key()) == want && have < max && plain(&s) {
                    let add = left.min(max - have);
                    plan.push((slot, have + add, false));
                    left -= add;
                }
            }
        }
    }
    for slot in empty_slots {
        if left == 0 {
            break;
        }
        let add = left.min(max);
        plan.push((slot, add, true));
        left -= add;
    }
    if left > 0 {
        return false;
    }
    for (slot, n, _) in plan {
        p.set_inventory_item(slot, Some(ItemStack::new(id, n as u8)));
    }
    true
}

/// Plain stacks of an item in the main inventory: (slot, count).
fn plain_stacks(p: &Player, id: &str) -> Vec<(u8, u32)> {
    let want = short(id);
    (0u8..36)
        .filter_map(|slot| {
            let s = some(p.get_inventory_item(slot))?;
            (short(&s.get_registry_key()) == want && plain(&s)).then(|| (slot, u32::from(s.get_count())))
        })
        .collect()
}

fn trade(st: &mut State, p: &Player, item: &store::ShopItem, click: &ClickType) {
    let l = st.cfg.language.clone();
    let k = key(&p.get_id());
    let balance = st.data(p).balance;
    let max = u32::from(ItemStack::new(&item.item, 1).get_max_count().clamp(1, 64));
    let label = short(&item.item).to_owned();
    let say = |key: &str, vars: &[(&str, String)]| c::tell(p, &fmt(&l, key, vars));
    match click {
        ClickType::Left | ClickType::ShiftLeft => {
            let count = if matches!(click, ClickType::ShiftLeft) { max } else { 1 };
            match store::shop_buy(item.buy, count, balance) {
                Err(ShopError::NoMoney) => say("no_money", &[("money", money(st, balance))]),
                Err(_) => say("shop_no_buy", &[("item", label)]),
                Ok((total, new)) => {
                    if !give_plain(p, &item.item, count, max) {
                        return say("shop_full", &[]);
                    }
                    st.data(p).balance = new;
                    st.save(&k);
                    say("shop_bought", &[("item", label), ("count", count.to_string()), ("money", money(st, total)), ("balance", money(st, new))]);
                }
            }
        }
        ClickType::Right | ClickType::ShiftRight => {
            if item.sell <= 0.0 {
                return say("shop_no_sell", &[("item", label)]);
            }
            let stacks = plain_stacks(p, &item.item);
            let have: u32 = stacks.iter().map(|(_, n)| n).sum();
            if have == 0 {
                return say("shop_none", &[("item", label)]);
            }
            let count = if matches!(click, ClickType::ShiftRight) { have } else { 1 };
            match store::shop_sell(item.sell, count, balance) {
                Err(ShopError::TooRich) => say("shop_rich", &[]),
                Err(_) => say("shop_no_sell", &[("item", label)]),
                Ok((total, new)) => {
                    let mut left = count;
                    for (slot, n) in stacks {
                        if left == 0 {
                            break;
                        }
                        let take = left.min(n);
                        left -= take;
                        p.set_inventory_item(slot, (n > take).then(|| ItemStack::new(&item.item, (n - take) as u8)));
                    }
                    st.data(p).balance = new;
                    st.save(&k);
                    say("sold", &[("count", count.to_string()), ("money", money(st, total)), ("balance", money(st, new))]);
                }
            }
        }
        _ => {}
    }
}

fn shop_click(st: &mut State, p: &Player, slot: u32, click: &ClickType) {
    let k = key(&p.get_id());
    // Every trade writes the player's file: a click macro must not turn into a disk load
    let now = st.started.elapsed().as_secs_f64();
    if !st.shop_burst.entry(k.clone()).or_default().allow(now, 30, 5.0) {
        return;
    }
    let Some(Kind::Shop { cat, page }) = st.wins.get(&k).map(|w| &w.kind) else { return };
    let (cat, page) = (cat.clone(), *page);
    let goto = |st: &mut State, cat: Option<String>, page: usize| {
        if let Some(w) = st.wins.get_mut(&k) {
            w.kind = Kind::Shop { cat, page };
        }
    };
    match cat {
        None => {
            if let Some((_, name)) = st.shop.menu().into_iter().find(|(s, _)| *s as u32 == slot) {
                goto(st, Some(name), 0);
            } else {
                return;
            }
        }
        Some(cat) => {
            let layout = st.shop.layout(&cat);
            let pages = layout.len() / SHOP_PAGE;
            let page = page.min(pages - 1);
            match slot {
                SLOT_PREV if page > 0 => goto(st, Some(cat), page - 1),
                SLOT_NEXT if page + 1 < pages => goto(st, Some(cat), page + 1),
                SLOT_BACK if st.shop.categories.len() > 1 => goto(st, None, 0),
                s if (s as usize) < SHOP_PAGE => {
                    let Some(item) = layout[page * SHOP_PAGE + s as usize].and_then(|n| st.shop.items.get(n)).cloned() else { return };
                    trade(st, p, &item, click);
                }
                _ => return,
            }
        }
    }
    draw_shop(st, p);
}

// ---- events -----------------------------------------------------------------

/// A click in a window. Returns true when the server must undo it.
pub(crate) fn on_click(st: &mut State, p: &Player, window: Option<Screen>, raw_slot: i16, click: &ClickType) -> bool {
    let k = key(&p.get_id());
    let Some(win) = st.wins.get(&k) else { return false };
    if window != Some(win.screen) {
        // Not our window any more (the client closed it without telling the server yet)
        return false;
    }
    let inside = raw_slot >= 0 && (raw_slot as u32) < win.size;
    match &win.kind {
        Kind::Trash => false,
        Kind::Mirror { modify: false, .. } => true,
        Kind::Mirror { ender, .. } => inside && matches!(src_of(*ender, raw_slot as u32), Src::Filler),
        Kind::Shop { .. } => {
            if inside {
                shop_click(st, p, raw_slot as u32, click);
            }
            true
        }
    }
}

/// The player closed a window (or left).
pub(crate) fn on_close(st: &mut State, server: &Server, p: &Player) {
    finish(st, server, p);
}

/// A player left: their own window ends; windows looking at them close on the next tick.
pub(crate) fn on_leave(st: &mut State, server: &Server, p: &Player) {
    finish(st, server, p);
    st.shop_burst.remove(&key(&p.get_id()));
}

/// After `/basics reload`: shops on screen show the new file.
pub(crate) fn redraw_shops(st: &mut State, server: &Server) {
    let open: Vec<String> = st.wins.iter().filter(|(_, w)| matches!(w.kind, Kind::Shop { .. })).map(|(k, _)| k.clone()).collect();
    for k in open {
        let Some(p) = online(server, &k) else { continue };
        if let Some(w) = st.wins.get_mut(&k) {
            w.kind = Kind::Shop { cat: None, page: 0 };
        }
        draw_shop(st, &p);
    }
}

/// The plugin is unloading: nothing may stay in a window that nobody watches any more.
pub(crate) fn clear_all(st: &mut State) {
    for (_, w) in st.wins.drain() {
        w.inv.clear();
    }
}

/// Windows that need the tick (mirrors).
pub(crate) fn any_mirror(st: &State) -> bool {
    st.wins.values().any(|w| matches!(w.kind, Kind::Mirror { .. }))
}

/// Used by the tick to space the mirror work out.
pub(crate) fn due(last: &mut Instant) -> bool {
    if last.elapsed().as_millis() >= 150 {
        *last = Instant::now();
        true
    } else {
        false
    }
}

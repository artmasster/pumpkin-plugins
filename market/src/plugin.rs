//! Pumpkin WASM glue for Market: selling from the hand, the windows, buying and paying.
//!
//! Everything is kept in `market.json`, written after every change. Money goes through the
//! Basics plugin: the buyer pays first, then gets the item; the seller is paid at once (Basics
//! knows offline players by name), or later when Basics did not answer. Windows are locked:
//! clicks only choose; items move by this plugin alone.
use std::collections::HashMap;
use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::Hand;
use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, InventoryClickEvent, InventoryCloseEvent, PlayerLeaveEvent};
use pumpkin_plugin_api::gui::Gui;
use pumpkin_plugin_api::inventory::Inventory;
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::screens_wit::Screen;
use pumpkin_plugin_api::{Context, ItemStack, Plugin, PluginMetadata, Server, register_plugin};

use crate::defs::{self, Book, Boxed, Config, MAX_ITEM, NS, Refusal};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&aMarket&8]&r ";
const FILE: &str = "market.json";
const PER_PAGE: usize = 45;
const SLOT_PREV: u32 = 45;
const SLOT_MINE: u32 = 48;
const SLOT_BOX: u32 = 49;
const SLOT_NEXT: u32 = 53;
const SLOT_YES: u32 = 11;
const SLOT_ITEM: u32 = 13;
const SLOT_NO: u32 = 15;

#[derive(Clone)]
enum View {
    Browse { page: usize, ids: Vec<u64> },
    Confirm { id: u64, back: usize },
    Mine { ids: Vec<u64> },
    Box,
}

struct Win {
    view: View,
    inv: Inventory,
}

enum Act {
    Click { player: String, slot: i16 },
}

struct State {
    cfg: Config,
    book: Book,
    wins: HashMap<String, Win>,
    acts: Vec<Act>,
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
        c::log_info("could not write market.json");
    }
}

fn stack_of(item: &str, count: u8) -> Option<ItemStack> {
    c::make_stack(&c::items::decode(item), count)
}

fn left(th: bool, secs: u64) -> String {
    let (h, m) = (secs / 3600, secs / 60 % 60);
    if th { format!("{h} ชม. {m} นาที") } else { format!("{h}h {m}m") }
}

// ---- windows ------------------------------------------------------------------------

fn open_window(title: &str, rows6: bool) -> (Gui, Inventory) {
    let gui = Gui::new(if rows6 { Screen::Generic9x6 } else { Screen::Generic9x3 }, c::color(title));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    let inv = gui.get_inventory();
    (gui, inv)
}

fn browse(st: &mut State, p: &Player, page: usize) {
    let th = st.cfg.thai();
    let now = c::now_secs();
    let total = st.book.listings.len().div_ceil(PER_PAGE).max(1);
    let page = page.min(total - 1);
    let (gui, inv) = open_window(&format!("{} &8{}/{}", if th { "&8ตลาด" } else { "&8Market" }, page + 1, total), true);
    let mut ids = Vec::new();
    for (slot, l) in st.book.page(page, PER_PAGE).into_iter().enumerate() {
        let Some(stack) = stack_of(&l.item, l.count) else { continue };
        stack.add_lore(c::game::upright("&8----------"));
        stack.add_lore(c::game::upright(&format!("&f{} &a{}", if th { "ราคา" } else { "Price" }, st.cfg.money(l.price))));
        stack.add_lore(c::game::upright(&format!("&7{} {}", if th { "ผู้ขาย" } else { "Seller" }, l.seller_name)));
        stack.add_lore(c::game::upright(&format!("&7{} {}", if th { "เหลือเวลา" } else { "Ends in" }, left(th, l.ends.saturating_sub(now)))));
        stack.add_lore(c::game::upright(if th { "&eคลิกเพื่อซื้อ" } else { "&eClick to buy" }));
        inv.set_item(slot as u32, Some(stack));
        ids.push(l.id);
    }
    if page > 0 {
        inv.set_item(SLOT_PREV, Some(c::game::button("minecraft:arrow", if th { "&eหน้าก่อน" } else { "&ePrevious page" }, &[])));
    }
    if page + 1 < total {
        inv.set_item(SLOT_NEXT, Some(c::game::button("minecraft:arrow", if th { "&eหน้าถัดไป" } else { "&eNext page" }, &[])));
    }
    let me = c::game::key(p);
    let mine = st.book.listings.iter().filter(|l| l.seller == me).count();
    inv.set_item(SLOT_MINE, Some(c::game::button("minecraft:writable_book", &format!("{} &7({mine})", if th { "&fของที่ฉันลงขาย" } else { "&fMy listings" }), &[if th { "&7ลงขาย: ถือของแล้วพิมพ์ /market sell <ราคา>".into() } else { "&7To sell: hold it and type /market sell <price>".into() }])));
    let boxed = st.book.boxes.get(&me).map_or(0, Vec::len);
    inv.set_item(SLOT_BOX, Some(c::game::button("minecraft:chest", &format!("{} &7({boxed})", if th { "&fกล่องรับของคืน" } else { "&fReturned items" }), &[])));
    st.wins.insert(me, Win { view: View::Browse { page, ids }, inv });
    p.open_gui(gui);
}

fn confirm(st: &mut State, p: &Player, id: u64, back: usize) {
    let th = st.cfg.thai();
    let Some(l) = st.book.listings.iter().find(|l| l.id == id).cloned() else { return browse(st, p, back) };
    let (gui, inv) = open_window(if th { "&8ยืนยันการซื้อ" } else { "&8Confirm" }, false);
    if let Some(stack) = stack_of(&l.item, l.count) {
        inv.set_item(SLOT_ITEM, Some(stack));
    }
    inv.set_item(SLOT_YES, Some(c::game::button("minecraft:lime_concrete", &format!("{} &a{}", if th { "&aซื้อในราคา" } else { "&aBuy for" }, st.cfg.money(l.price)), &[])));
    inv.set_item(SLOT_NO, Some(c::game::button("minecraft:red_concrete", if th { "&cยกเลิก" } else { "&cCancel" }, &[])));
    st.wins.insert(c::game::key(p), Win { view: View::Confirm { id, back }, inv });
    p.open_gui(gui);
}

fn mine(st: &mut State, p: &Player) {
    let th = st.cfg.thai();
    let me = c::game::key(p);
    let now = c::now_secs();
    let (gui, inv) = open_window(if th { "&8ของที่ฉันลงขาย" } else { "&8My listings" }, true);
    let mut ids = Vec::new();
    for (slot, l) in st.book.listings.iter().filter(|l| l.seller == me).take(PER_PAGE).enumerate() {
        let Some(stack) = stack_of(&l.item, l.count) else { continue };
        stack.add_lore(c::game::upright(&format!("&f{} &a{}", if th { "ราคา" } else { "Price" }, st.cfg.money(l.price))));
        stack.add_lore(c::game::upright(&format!("&7{} {}", if th { "เหลือเวลา" } else { "Ends in" }, left(th, l.ends.saturating_sub(now)))));
        stack.add_lore(c::game::upright(if th { "&cคลิกเพื่อเอาคืน" } else { "&cClick to take it back" }));
        inv.set_item(slot as u32, Some(stack));
        ids.push(l.id);
    }
    inv.set_item(SLOT_PREV, Some(c::game::button("minecraft:arrow", if th { "&eกลับ" } else { "&eBack" }, &[])));
    st.wins.insert(me, Win { view: View::Mine { ids }, inv });
    p.open_gui(gui);
}

fn boxed(st: &mut State, p: &Player) {
    let th = st.cfg.thai();
    let me = c::game::key(p);
    let (gui, inv) = open_window(if th { "&8กล่องรับของคืน" } else { "&8Returned items" }, true);
    for (slot, b) in st.book.boxes.get(&me).map(Vec::as_slice).unwrap_or_default().iter().take(PER_PAGE).enumerate() {
        if let Some(stack) = stack_of(&b.item, b.count) {
            stack.add_lore(c::game::upright(if th { "&eคลิกเพื่อรับ" } else { "&eClick to take it" }));
            inv.set_item(slot as u32, Some(stack));
        }
    }
    inv.set_item(SLOT_PREV, Some(c::game::button("minecraft:arrow", if th { "&eกลับ" } else { "&eBack" }, &[])));
    st.wins.insert(me, Win { view: View::Box, inv });
    p.open_gui(gui);
}

// ---- deals ---------------------------------------------------------------------------

fn buy(st: &mut State, server: &Server, p: &Player, id: u64) {
    let th = st.cfg.thai();
    let me = c::game::key(p);
    let Some(l) = st.book.listings.iter().find(|l| l.id == id).cloned() else {
        c::tell(p, &msg(th, "&cมีคนซื้อไปแล้ว", "&cSomeone bought it already"));
        return;
    };
    if l.seller == me {
        c::tell(p, &msg(th, "&cซื้อของตัวเองไม่ได้ (เอาคืนได้ที่ ของที่ฉันลงขาย)", "&cYou cannot buy your own (take it back in My listings)"));
        return;
    }
    if c::game::empty_slots(p) == 0 {
        c::tell(p, &msg(th, "&cกระเป๋าเต็ม ต้องมีช่องว่างก่อน", "&cYour inventory is full: make room first"));
        return;
    }
    let Some(stack) = stack_of(&l.item, l.count) else {
        c::tell(p, &msg(th, "&cเซิร์ฟนี้ไม่มีไอเท็มนี้แล้ว", "&cThis server has no such item any more"));
        return;
    };
    match c::game::money::withdraw(&p.get_name(), l.price) {
        Some(Ok(_)) => {}
        Some(Err(have)) => {
            c::tell(p, &msg(th, &format!("&cเงินไม่พอ (มี {})", st.cfg.money(have)), &format!("&cNot enough money (you have {})", st.cfg.money(have))));
            return;
        }
        None => {
            c::tell(p, &msg(th, "&cระบบเงิน (Basics) ไม่ตอบ ลองใหม่", "&cThe money plugin (Basics) did not answer: try again"));
            return;
        }
    }
    st.book.take(id);
    if !c::game::give_stack(p, stack) {
        // Checked above; never lose what was paid for
        st.book.boxes.entry(me.clone()).or_default().push(Boxed { item: l.item.clone(), count: l.count });
    }
    let payout = st.cfg.payout(l.price);
    if c::game::money::deposit(&l.seller_name, payout).is_none() {
        st.book.owe(&l.seller, &l.seller_name, payout);
    }
    st.book.sales += 1;
    save(st);
    c::tell(p, &msg(th, &format!("&aซื้อแล้วในราคา {}", st.cfg.money(l.price)), &format!("&aBought for {}", st.cfg.money(l.price))));
    if let Some(seller) = c::game::online(server, &l.seller) {
        c::tell(&seller, &msg(th, &format!("&a{} ซื้อของของคุณ ได้ {}", p.get_name(), st.cfg.money(payout)), &format!("&a{} bought your item: you got {}", p.get_name(), st.cfg.money(payout))));
    }
}

/// Put a listing or a boxed item back into the player's inventory.
fn take_back(st: &mut State, p: &Player, item: &str, count: u8) -> bool {
    let th = st.cfg.thai();
    let Some(stack) = stack_of(item, count) else { return false };
    if !c::game::give_stack(p, stack) {
        c::tell(p, &msg(th, "&cกระเป๋าเต็ม ต้องมีช่องว่างก่อน", "&cYour inventory is full: make room first"));
        return false;
    }
    true
}

fn click(st: &mut State, server: &Server, p: &Player, slot: i16) {
    let me = c::game::key(p);
    let Some(view) = st.wins.get(&me).map(|w| w.view.clone()) else { return };
    let slot = u32::try_from(slot).unwrap_or(u32::MAX);
    match view {
        View::Browse { page, ids } => match slot {
            SLOT_PREV if page > 0 => browse(st, p, page - 1),
            SLOT_NEXT => browse(st, p, page + 1),
            SLOT_MINE => mine(st, p),
            SLOT_BOX => boxed(st, p),
            s if (s as usize) < ids.len() => confirm(st, p, ids[s as usize], page),
            _ => {}
        },
        View::Confirm { id, back } => match slot {
            SLOT_YES => {
                buy(st, server, p, id);
                browse(st, p, back);
            }
            SLOT_NO => browse(st, p, back),
            _ => {}
        },
        View::Mine { ids } => match slot {
            SLOT_PREV => browse(st, p, 0),
            s if (s as usize) < ids.len() => {
                if let Some(l) = st.book.listings.iter().find(|l| l.id == ids[s as usize] && l.seller == me).cloned()
                    && take_back(st, p, &l.item, l.count)
                {
                    st.book.take(l.id);
                    save(st);
                }
                mine(st, p);
            }
            _ => {}
        },
        View::Box => match slot {
            SLOT_PREV => browse(st, p, 0),
            s if (s as usize) < PER_PAGE => {
                if let Some(b) = st.book.boxes.get(&me).and_then(|v| v.get(s as usize)).cloned()
                    && take_back(st, p, &b.item, b.count)
                {
                    if let Some(v) = st.book.boxes.get_mut(&me) {
                        v.remove(s as usize);
                        if v.is_empty() {
                            st.book.boxes.remove(&me);
                        }
                    }
                    save(st);
                }
                boxed(st, p);
            }
            _ => {}
        },
    }
}

fn sell(st: &mut State, p: &Player, price: f64, count: Option<u8>) -> String {
    let th = st.cfg.thai();
    let me = c::game::key(p);
    let inv = p.get_inventory();
    let held = inv.get_item_in_hand(Hand::Right).filter(|s| !c::game::is_empty_stack(s));
    let text = held.as_ref().and_then(|s| c::item_text(s, MAX_ITEM));
    match defs::may_list(&st.cfg, &st.book, &me, text.as_deref(), price) {
        Ok(()) => {}
        Err(Refusal::Price) => return msg(th, &format!("&cราคาต้องอยู่ระหว่าง {} ถึง {}", st.cfg.money(st.cfg.min_price), st.cfg.money(st.cfg.max_price)), &format!("&cThe price must be from {} to {}", st.cfg.money(st.cfg.min_price), st.cfg.money(st.cfg.max_price))),
        Err(Refusal::NoItem) => return msg(th, "&cถือของที่จะขายไว้ในมือก่อน", "&cHold what you sell in your hand"),
        Err(Refusal::TooBig) => return msg(th, "&cของชิ้นนี้มีข้อมูลมากเกินไป ขายในตลาดไม่ได้", "&cThis item carries too much data to sell here"),
        Err(Refusal::TooMany) => return msg(th, &format!("&cลงขายได้พร้อมกันไม่เกิน {} ชิ้น", st.cfg.max_listings), &format!("&cAt most {} listings at once", st.cfg.max_listings)),
    }
    let (Some(stack), Some(text)) = (held, text) else { return String::new() };
    let have = stack.get_count();
    let n = count.unwrap_or(have).clamp(1, have);
    let fee = st.cfg.fee(price);
    if fee > 0.0 {
        match c::game::money::withdraw(&p.get_name(), fee) {
            Some(Ok(_)) => {}
            Some(Err(_)) => return msg(th, &format!("&cเงินไม่พอจ่ายค่าลงขาย {}", st.cfg.money(fee)), &format!("&cNot enough money for the listing fee of {}", st.cfg.money(fee))),
            None => return msg(th, "&cระบบเงิน (Basics) ไม่ตอบ ลองใหม่", "&cThe money plugin (Basics) did not answer: try again"),
        }
    }
    if n >= have {
        inv.set_item_in_hand(Hand::Right, None);
    } else {
        stack.set_count(have - n);
        inv.set_item_in_hand(Hand::Right, Some(stack));
    }
    let id = st.book.add(&st.cfg, &me, &p.get_name(), text, n, price, c::now_secs());
    save(st);
    msg(th, &format!("&aลงขายแล้ว (#{id}) ราคา {}{}", st.cfg.money(price), if fee > 0.0 { format!(" ค่าลงขาย {}", st.cfg.money(fee)) } else { String::new() }), &format!("&aListed (#{id}) for {}{}", st.cfg.money(price), if fee > 0.0 { format!(", fee {}", st.cfg.money(fee)) } else { String::new() }))
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.tick += 1;
    for a in std::mem::take(&mut st.acts) {
        let Act::Click { player, slot } = a;
        if let Some(p) = c::game::online(&server, &player) {
            click(st, &server, &p, slot);
        }
    }
    if st.tick % 1200 == 0 {
        let th = st.cfg.thai();
        let before: Vec<(String, usize)> = st.book.boxes.iter().map(|(k, v)| (k.clone(), v.len())).collect();
        if st.book.expire(c::now_secs()) > 0 {
            for (k, v) in &st.book.boxes {
                let was = before.iter().find(|(b, _)| b == k).map_or(0, |(_, n)| *n);
                if v.len() > was
                    && let Some(p) = c::game::online(&server, k)
                {
                    c::tell(&p, &msg(th, "&7ของที่ลงขายหมดเวลาแล้ว รับคืนได้ที่ /market box", "&7A listing ended unsold: take it back with /market box"));
                }
            }
            save(st);
        }
        let owed: Vec<(String, String, f64)> = st.book.owed.iter().map(|(k, (n, a))| (k.clone(), n.clone(), *a)).collect();
        for (k, name, amount) in owed {
            if c::game::money::deposit(&name, amount).is_some() {
                st.book.owed.remove(&k);
                save(st);
            }
        }
    }
}

// ---- events ------------------------------------------------------------------------

type Data<E> = <E as FromIntoEvent>::Data;

struct OnClick;
impl EventHandler<InventoryClickEvent> for OnClick {
    fn handle(&self, _server: Server, mut e: Data<InventoryClickEvent>) -> Data<InventoryClickEvent> {
        if e.window_type.is_none() {
            return e;
        }
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let me = c::game::key(&e.player);
            if st.wins.contains_key(&me) {
                e.cancelled = true;
                // Next tick: opening the next window from inside the click would close it again
                st.acts.push(Act::Click { player: me, slot: e.raw_slot });
            }
        }
        e
    }
}

struct OnClose;
impl EventHandler<InventoryCloseEvent> for OnClose {
    fn handle(&self, _server: Server, e: Data<InventoryCloseEvent>) -> Data<InventoryCloseEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let me = c::game::key(&e.player);
            // A window we just opened in its place is not closed by the close of the old one
            if !st.acts.iter().any(|Act::Click { player, .. }| *player == me) {
                if let Some(w) = st.wins.remove(&me) {
                    w.inv.clear();
                }
            }
        }
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, e: Data<PlayerLeaveEvent>) -> Data<PlayerLeaveEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            st.wins.remove(&c::game::key(&e.player));
        }
        e
    }
}

// ---- commands ----------------------------------------------------------------------

fn run(st: &mut State, sender: &CommandSender, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    let me = c::sender_player(sender);
    let admin = c::has_perm(sender, &format!("{NS}:admin"));
    let in_game = || vec![msg(th, "&cใช้ได้ในเกมเท่านั้น", "&cIn game only")];
    match w.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        None => {
            let Some(p) = me else { return in_game() };
            browse(st, &p, 0);
            Vec::new()
        }
        Some("sell") => {
            let Some(p) = me else { return in_game() };
            let Some(price) = w.get(1).and_then(|x| x.replace(',', "").parse::<f64>().ok()) else { return vec![msg(th, "&f/market sell <ราคา> [จำนวน]", "&f/market sell <price> [count]")] };
            let count = w.get(2).and_then(|n| n.parse::<u8>().ok());
            vec![sell(st, &p, price, count)]
        }
        Some("mine") => {
            let Some(p) = me else { return in_game() };
            mine(st, &p);
            Vec::new()
        }
        Some("box" | "collect") => {
            let Some(p) = me else { return in_game() };
            boxed(st, &p);
            Vec::new()
        }
        Some("info") => vec![msg(th, &format!("&fรุ่น {VERSION} &7ลงขายอยู่ {} ชิ้น ขายไปแล้ว {} ครั้ง ค่าลงขาย {}% ภาษีขาย {}%", st.book.listings.len(), st.book.sales, st.cfg.listing_fee * 100.0, st.cfg.sales_tax * 100.0), &format!("&fversion {VERSION} &7{} listed, {} sold, fee {}%, tax {}%", st.book.listings.len(), st.book.sales, st.cfg.listing_fee * 100.0, st.cfg.sales_tax * 100.0))],
        Some("reload") if admin => {
            st.cfg = c::load_config::<Config>("config.toml").cleaned();
            vec![msg(st.cfg.thai(), "&aอ่านค่าตั้งใหม่แล้ว", "&aSettings read again")]
        }
        Some("remove") if admin => {
            let Some(id) = w.get(1).and_then(|n| n.trim_start_matches('#').parse::<u64>().ok()) else { return vec![msg(th, "&f/market remove <เลข>", "&f/market remove <number>")] };
            match st.book.take(id) {
                Some(l) => {
                    st.book.boxes.entry(l.seller.clone()).or_default().push(Boxed { item: l.item, count: l.count });
                    save(st);
                    vec![msg(th, "&aเอาลงแล้ว ของกลับไปที่กล่องของผู้ขาย", "&aTaken down: the item went to the seller's box")]
                }
                None => vec![msg(th, "&cไม่มีรายการนี้", "&cNo such listing")],
            }
        }
        Some("version") => vec![format!("Market {VERSION}")],
        _ => {
            let mut out = vec![msg(th, "&f/market &7เปิดตลาด &7| &fsell <ราคา> [จำนวน] &7| &fmine &7| &fbox &7| &finfo", "&f/market &7open &7| &fsell <price> [count] &7| &fmine &7| &fbox &7| &finfo")];
            if admin {
                out.push(msg(th, "&f/market remove <เลข> &7| &freload &7| &fversion", "&f/market remove <number> &7| &freload &7| &fversion"));
            }
            out
        }
    }
}

struct Cmd;
impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, _server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let lines = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            let Some(st) = guard.as_mut() else { return Ok(0) };
            run(st, &sender, &w)
        };
        for line in lines {
            c::reply(&sender, &line);
        }
        Ok(1)
    }
}

// ---- the plugin ------------------------------------------------------------------

struct Market;

impl Plugin for Market {
    fn new() -> Self {
        Market
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "A market where players sell items to each other".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:use"), description: "Buy and sell on the market".into(), default: PermissionDefault::Allow, children: vec![] });
        let _ = context.register_permission(&Permission { node: format!("{NS}:admin"), description: "Take listings down".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let cfg = c::load_config::<Config>("config.toml").cleaned();
        let book: Book = c::load_json(FILE).unwrap_or_default();
        c::log_info(&format!("{} listing(s), {} sale(s) so far", book.listings.len(), book.sales));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State { cfg, book, wins: HashMap::new(), acts: Vec::new(), tick: 0 });
        let cmd = Command::new(&[NS.to_owned(), "ah".to_owned()], "Market").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:use"));
        context.register_event_handler(OnClick, EventPriority::Normal, true)?;
        context.register_event_handler(OnClose, EventPriority::Normal, false)?;
        context.register_event_handler(OnLeave, EventPriority::Normal, false)?;
        context.schedule_repeating_task(1, 1, on_tick);
        c::mark_running(VERSION);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::mark_stopped();
        if let Ok(mut guard) = STATE.lock()
            && let Some(st) = guard.as_mut()
        {
            for (_, w) in st.wins.drain() {
                w.inv.clear();
            }
            save(st);
        }
        Ok(())
    }
}

register_plugin!(Market);

//! Pumpkin WASM glue for CustomItems: reading the owner's files, making the items, the chest
//! window, recipes, and getting the resource pack to players.
//!
//! An item is an item of the game (paper unless the owner says otherwise) that carries three
//! things: the `item_model` component, which makes it look like the model of that name in the
//! resource pack; a name; and our id in its custom data, which is how the plugin knows it again.
//!
//! The pack is built when the plugin loads and on `/customitems reload`, in one go: text files
//! are deflated, pictures and sounds are stored as they are, so even a large pack takes a
//! moment only. It is kept in memory for the built-in download and written to `pack.zip`.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mcsv_common as c;
use mcsv_web::{Request, Response, Web};
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::{ClickType, Hand};
use pumpkin_plugin_api::data_components::DataComponent;
use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, InventoryClickEvent, InventoryCloseEvent, PlayerLeaveEvent, PlayerResourcePackStatusEvent};
use pumpkin_plugin_api::gui::Gui;
use pumpkin_plugin_api::inventory::Inventory;
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA, NETWORK_TCP_BIND};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::text::TextComponent;
use pumpkin_plugin_api::uuid::Uuid;
use pumpkin_plugin_api::wit::pumpkin::plugin::common::{NbtTag, NbtTree};
use pumpkin_plugin_api::wit::pumpkin::plugin::player::JavaResourcePack;
use pumpkin_plugin_api::{Context, ItemStack, Plugin, PluginMetadata, Screen, Server, register_plugin};

use crate::defs::{self, Config, Item, Look, NS, Part, Problem, Recipe};
use crate::pack::{self, Entry};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&bCustomItems&8]&r ";
const STATE_FILE: &str = "state.json";
/// Items on one page of the chest window (the sixth row holds the page buttons).
const PAGE: usize = 45;
const PREV_SLOT: i16 = 45;
const NEXT_SLOT: i16 = 53;
/// What a tick may spend on pack downloads.
const WEB_BUDGET: Duration = Duration::from_millis(2);
/// Files of `pack/` are read this deep, no deeper.
const MAX_DEPTH: usize = 12;

struct Built {
    bytes: Arc<Vec<u8>>,
    sha1: String,
    files: usize,
    took_ms: u128,
}

#[derive(Default)]
struct Seen {
    /// The SHA-1 of the pack this player was sent.
    sent: String,
    /// The tick from which the pack may be sent.
    due: u64,
    /// What the client said about it last.
    answer: String,
}

struct State {
    cfg: Config,
    items: Vec<Item>,
    /// Items that lost their look (the picture is missing): they stay plain.
    plain: BTreeSet<String>,
    /// Glyphs in the pack and their characters.
    glyphs: BTreeMap<String, char>,
    problems: Vec<Problem>,
    pack: Option<Built>,
    web: Option<Web>,
    /// Why the download port could not be opened.
    web_error: Option<String>,
    /// By lowercase name.
    players: HashMap<String, Seen>,
    /// Every recipe as (item, which of its recipes): the entries of the crafting window.
    crafts: Vec<(usize, usize)>,
    /// Who has one of our windows open.
    wins: HashMap<String, Win>,
    /// The files were read again: open windows show what was.
    redraw: bool,
    tick: u64,
    sent: u64,
}

struct Win {
    kind: Kind,
    page: usize,
    /// The window's slots.
    inv: Inventory,
}

/// Our two chest windows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Every item, free to take: for staff.
    Give,
    /// The items that have a recipe: for everyone.
    Craft,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// The server, for messages from other plugins (they do not bring it).
static SERVER: Mutex<Option<Server>> = Mutex::new(None);

/// Never blocks: WASM has one thread and Pumpkin calls handlers from inside host calls.
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

// ---- reading the owner's files ------------------------------------------------

fn read(rel: &str) -> Option<Vec<u8>> {
    std::fs::read(c::data_path(rel)).ok()
}

/// File names of a folder in the data folder, sorted.
fn names_in(rel: &str) -> Vec<(String, bool)> {
    let mut names: Vec<(String, bool)> = std::fs::read_dir(c::data_path(rel))
        .map(|dir| dir.flatten().filter_map(|e| Some((e.file_name().into_string().ok()?, e.file_type().ok()?.is_dir()))).collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// Every file under `pack/`, with its path inside the pack.
fn own_pack_files(out: &mut Vec<Problem>) -> Vec<Entry> {
    let mut found = Vec::new();
    let mut todo: Vec<(String, usize)> = vec![(String::new(), 0)];
    let mut bytes = 0usize;
    while let Some((dir, depth)) = todo.pop() {
        for (name, is_dir) in names_in(&format!("pack/{dir}")) {
            let path = if dir.is_empty() { name } else { format!("{dir}/{name}") };
            if is_dir {
                if depth < MAX_DEPTH {
                    todo.push((path, depth + 1));
                }
            } else if found.len() <= pack::MAX_FILES && bytes <= pack::MAX_BYTES {
                // More than the pack may hold is not read at all: `assemble` then says so
                if let Some(data) = read(&format!("pack/{path}")) {
                    bytes += data.len();
                    found.push(Entry { path, data });
                }
            }
        }
    }
    if found.len() > pack::MAX_FILES || bytes > pack::MAX_BYTES {
        out.push(Problem::new("pack/", "มีไฟล์มากเกินกว่าที่ pack รับได้", "holds more than a pack can take"));
    }
    found
}

struct Loaded {
    cfg: Config,
    items: Vec<Item>,
    plain: BTreeSet<String>,
    glyphs: BTreeMap<String, char>,
    problems: Vec<Problem>,
    pack: Option<Built>,
}

fn load() -> Loaded {
    let cfg = c::load_config::<Config>("config.toml").cleaned();
    let mut problems = Vec::new();

    // The folders the owner works in, with an example on the very first start
    let first = !c::data_path("items").exists();
    for dir in ["items", "textures", "pack"] {
        let _ = std::fs::create_dir_all(c::data_path(dir));
    }
    if first {
        let _ = std::fs::write(c::data_path("items/example.toml"), defs::EXAMPLE_ITEMS);
        if !c::data_path("glyphs.toml").exists() {
            let _ = std::fs::write(c::data_path("glyphs.toml"), defs::EXAMPLE_GLYPHS);
        }
    }

    let mut all = Vec::new();
    for (name, is_dir) in names_in("items") {
        if is_dir || !name.ends_with(".toml") {
            continue;
        }
        let file = format!("items/{name}");
        match read(&file).map(String::from_utf8) {
            Some(Ok(text)) => all.extend(defs::parse_items(&file, &text, &mut problems)),
            _ => problems.push(Problem::new(&file, "อ่านไฟล์ไม่ได้ (ต้องเป็นข้อความ UTF-8)", "cannot be read (it must be UTF-8 text)")),
        }
    }
    let mut items = defs::merge_items(all, &mut problems);
    // The material must be an item the game has: asking for an unknown one gives air
    items.retain(|item| {
        let real = item.material == "minecraft:air" || !ItemStack::new(&item.material, 1).get_registry_key().ends_with("air");
        if !real {
            problems.push(Problem::new(&item.id, format!("ไม่มีไอเท็ม {} ในเกม", item.material), format!("the game has no item {}", item.material)));
        }
        real
    });
    // So must every ingredient; one of ours may have gone with its material just above
    let ours: BTreeSet<String> = items.iter().map(|i| i.id.clone()).collect();
    for item in &mut items {
        let id = item.id.clone();
        item.recipes.retain(|recipe| {
            let missing = recipe.ingredients.iter().find_map(|i| match &i.part {
                Part::Game(name) if ItemStack::new(name, 1).get_registry_key().ends_with("air") => Some(name.clone()),
                Part::Ours(name) if !ours.contains(name) => Some(name.clone()),
                _ => None,
            });
            if let Some(name) = &missing {
                problems.push(Problem::new(&id, format!("สูตรถูกข้าม: ไม่มีไอเท็ม {name}"), format!("a recipe was skipped: there is no item {name}")));
            }
            missing.is_none()
        });
    }

    let glyph_defs = read("glyphs.toml").and_then(|b| String::from_utf8(b).ok()).map(|text| defs::parse_glyphs(&text, &mut problems)).unwrap_or_default();
    let mut known: BTreeMap<String, u32> = c::load_json::<serde_json::Value>(STATE_FILE).and_then(|v| serde_json::from_value(v.get("glyphs")?.clone()).ok()).unwrap_or_default();
    let chars = defs::assign_glyphs(&glyph_defs, &known, &mut problems);
    // A glyph that is taken out keeps its character reserved: old texts never show another picture
    let before = known.len();
    known.extend(chars.iter().map(|(k, v)| (k.clone(), *v)));
    if known.len() != before {
        c::save_json(STATE_FILE, &serde_json::json!({ "glyphs": known }));
    }

    let started = Instant::now();
    let made = pack::generate(&cfg.pack, &items, &glyph_defs, &chars, &mut |file| read(&format!("textures/{file}")), &mut problems);
    let own = own_pack_files(&mut problems);
    let glyphs = chars.iter().filter(|(name, _)| made.glyphs.contains(*name)).filter_map(|(name, code)| Some((name.clone(), char::from_u32(*code)?))).collect();
    let built = match pack::assemble(made.entries, own, &mut problems) {
        Ok(files) => {
            let bytes = pack::zip(&files);
            let mut sha = c::sha1::Sha1::new();
            sha.update(&bytes);
            let sha1 = c::sha1::hex(&sha.finish());
            if std::fs::write(c::data_path("pack.zip"), &bytes).is_err() {
                problems.push(Problem::new("pack.zip", "เขียนไฟล์ไม่ได้", "cannot be written"));
            }
            Some(Built { bytes: Arc::new(bytes), sha1, files: files.len(), took_ms: started.elapsed().as_millis() })
        }
        Err(problem) => {
            problems.push(problem);
            None
        }
    };
    Loaded { cfg, items, plain: made.plain, glyphs, problems, pack: built }
}

/// Open or close the download port to match the settings.
fn listen(st: &mut State) {
    let want = st.cfg.pack.port;
    if st.web.as_ref().map_or(0, |w| w.port) == want {
        return;
    }
    // The old listener goes first: its port may be the one wanted
    st.web = None;
    st.web_error = None;
    if want != 0 {
        match Web::bind(want) {
            Ok(web) => st.web = Some(web),
            Err(e) => st.web_error = Some(e.to_string()),
        }
    }
}

fn apply(st: &mut State, loaded: Loaded) {
    st.cfg = loaded.cfg;
    st.items = loaded.items;
    st.plain = loaded.plain;
    st.glyphs = loaded.glyphs;
    st.problems = loaded.problems;
    st.pack = loaded.pack;
    st.crafts = st.items.iter().enumerate().flat_map(|(i, item)| (0..item.recipes.len()).map(move |r| (i, r))).collect();
    st.redraw = true;
    listen(st);
}

// ---- items ----------------------------------------------------------------------

/// A string the way the network writes it: length, then bytes.
fn net_string(s: &str) -> Vec<u8> {
    let mut out = varint(s.len() as u32);
    out.extend_from_slice(s.as_bytes());
    out
}

fn varint(mut n: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(5);
    loop {
        let b = (n & 0x7F) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

/// Names and lore are not slanted the way renamed items are.
fn upright(text: &str) -> TextComponent {
    TextComponent::text("").italic(false).add_child(c::color(text))
}

fn stack_of(st: &State, item: &Item, count: u8) -> ItemStack {
    let stack = ItemStack::new(&item.material, count.max(1));
    if !st.plain.contains(&item.id) {
        if let Some(model) = item.model_id() {
            stack.set_component(DataComponent::ItemModel, &net_string(&model));
        }
    }
    if !item.name.is_empty() {
        stack.set_custom_name(Some(upright(&item.name)));
    }
    if !item.lore.is_empty() {
        stack.set_lore(item.lore.iter().map(|l| upright(l)).collect::<Vec<_>>());
    }
    if item.glint {
        stack.set_component(DataComponent::EnchantmentGlintOverride, &[1]);
    }
    if item.max_stack != 0 {
        stack.set_component(DataComponent::MaxStackSize, &varint(u32::from(item.max_stack)));
    }
    stack.set_custom_data(NS, "id", &NbtTree { root: 0, tags: vec![NbtTag::StringTag(item.id.clone())] });
    stack
}

/// Which of our items a stack is.
fn id_of(stack: &ItemStack) -> Option<String> {
    let tree = stack.get_custom_data(NS, "id")?;
    match tree.tags.get(tree.root as usize) {
        Some(NbtTag::StringTag(s)) => Some(s.clone()),
        _ => None,
    }
}

fn empty(slot: &Option<ItemStack>) -> bool {
    slot.as_ref().is_none_or(|s| s.get_count() == 0 || s.get_registry_key().ends_with("air"))
}

/// Put `count` of an item into a player's inventory: stacks of it are filled up first, then
/// free slots are used. Returns how many did not fit.
fn give(st: &State, p: &Player, item: &Item, count: u32) -> u32 {
    let per_stack = u32::from(stack_of(st, item, 1).get_max_count().max(1));
    let mut left = count;
    let slots: Vec<Option<ItemStack>> = (0u8..36).map(|s| p.get_inventory_item(s)).collect();
    for (slot, have) in slots.iter().enumerate() {
        if left == 0 {
            break;
        }
        let Some(stack) = have.as_ref().filter(|s| id_of(s).as_deref() == Some(&item.id)) else { continue };
        let room = per_stack.saturating_sub(u32::from(stack.get_count()));
        if room > 0 {
            let add = room.min(left);
            p.set_inventory_item(slot as u8, Some(stack_of(st, item, (u32::from(stack.get_count()) + add) as u8)));
            left -= add;
        }
    }
    for (slot, have) in slots.iter().enumerate() {
        if left == 0 {
            break;
        }
        if empty(have) {
            let add = per_stack.min(left);
            p.set_inventory_item(slot as u8, Some(stack_of(st, item, add as u8)));
            left -= add;
        }
    }
    left
}

// ---- recipes --------------------------------------------------------------------

/// `minecraft:paper` however the server spells an item's kind.
fn kind_of(stack: &ItemStack) -> String {
    let key = stack.get_registry_key();
    if key.contains(':') { key } else { format!("minecraft:{key}") }
}

/// An item of the game only counts as it comes: one of ours made of redstone is not redstone.
fn is_part(part: &Part, stack: &ItemStack) -> bool {
    match part {
        Part::Ours(id) => id_of(stack).as_deref() == Some(id),
        Part::Game(name) => kind_of(stack) == *name && id_of(stack).is_none(),
    }
}

fn slots_of(p: &Player) -> Vec<Option<ItemStack>> {
    (0u8..36).map(|s| p.get_inventory_item(s)).collect()
}

fn have_of(slots: &[Option<ItemStack>], part: &Part) -> u32 {
    slots.iter().flatten().filter(|s| is_part(part, s)).map(|s| u32::from(s.get_count())).sum()
}

/// `Gold Ingot` for `minecraft:gold_ingot`; our own items by their name.
fn part_name(st: &State, part: &Part) -> String {
    match part {
        Part::Ours(id) => st.items.iter().find(|i| i.id == *id).map(|i| c::plain(&i.name)).filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone()),
        Part::Game(name) => {
            let words = name.rsplit(':').next().unwrap_or(name).split('_').map(|w| {
                let mut chars = w.chars();
                chars.next().map_or_else(String::new, |first| first.to_ascii_uppercase().to_string() + chars.as_str())
            });
            words.collect::<Vec<_>>().join(" ")
        }
    }
}

enum Crafted {
    /// How many times the recipe was made.
    Made(u32),
    Missing,
    Full,
}

/// Make a recipe up to `times` times: fewer when the ingredients or the room for the result
/// run out. Everything happens in this one call, so nothing can be taken twice.
fn craft(st: &State, p: &Player, item: &Item, recipe: &Recipe, times: u32) -> Crafted {
    let mut slots = slots_of(p);
    let can = recipe.ingredients.iter().map(|i| have_of(&slots, &i.part) / i.count.max(1)).min().unwrap_or(0);
    if can == 0 {
        return Crafted::Missing;
    }
    // Room for the result as the inventory is now (slots the ingredients free are a bonus)
    let per_stack = u32::from(stack_of(st, item, 1).get_max_count().max(1));
    let room: u32 = slots.iter().map(|s| match s {
        Some(have) if id_of(have).as_deref() == Some(&item.id) => per_stack.saturating_sub(u32::from(have.get_count())),
        other if empty(other) => per_stack,
        _ => 0,
    }).sum();
    let times = times.min(can).min(room / u32::from(recipe.count.max(1)));
    if times == 0 {
        return Crafted::Full;
    }
    for ingredient in &recipe.ingredients {
        let mut need = ingredient.count * times;
        for (slot, held) in slots.iter_mut().enumerate() {
            if need == 0 {
                break;
            }
            if !held.as_ref().is_some_and(|s| is_part(&ingredient.part, s)) {
                continue;
            }
            let Some(stack) = held.take() else { continue };
            let have = u32::from(stack.get_count());
            let take = have.min(need);
            need -= take;
            if have > take {
                // The same stack with fewer in it: whatever else it carries stays
                stack.set_count((have - take) as u8);
                p.set_inventory_item(slot as u8, Some(stack));
            } else {
                p.set_inventory_item(slot as u8, None);
            }
        }
    }
    give(st, p, item, u32::from(recipe.count) * times);
    Crafted::Made(times)
}

// ---- the chest windows ------------------------------------------------------------

fn pages(st: &State, kind: Kind) -> usize {
    let entries = if kind == Kind::Give { st.items.len() } else { st.crafts.len() };
    entries.div_ceil(PAGE).max(1)
}

fn button(material: &str, name: &str) -> ItemStack {
    let stack = ItemStack::new(material, 1);
    stack.set_custom_name(Some(upright(name)));
    stack
}

fn open_window(st: &mut State, p: &Player, kind: Kind) {
    let title = match (kind, st.cfg.thai()) {
        (Kind::Give, _) => "&8CustomItems",
        (Kind::Craft, true) => "&8คราฟไอเท็มพิเศษ",
        (Kind::Craft, false) => "&8Craft custom items",
    };
    let gui = Gui::new(Screen::Generic9x6, c::color(title));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    // The handle stays good after `open_gui` took the window: pages and counts are drawn into
    // the open window, which never has to be closed and opened again
    let win = Win { kind, page: 0, inv: gui.get_inventory() };
    draw(st, p, &win);
    st.wins.insert(p.get_name().to_ascii_lowercase(), win);
    p.open_gui(gui);
}

/// Fill a window with its page. Returns the page it is on (the last one if items went away).
fn draw(st: &State, p: &Player, win: &Win) -> usize {
    let th = st.cfg.thai();
    let kind = win.kind;
    let total = pages(st, kind);
    let page = win.page.min(total - 1);
    let inv = &win.inv;
    inv.clear();
    match kind {
        Kind::Give => {
            for (slot, item) in st.items.iter().skip(page * PAGE).take(PAGE).enumerate() {
                let stack = stack_of(st, item, 1);
                stack.add_lore(upright(&format!("&8{}", item.id)));
                inv.set_item(slot as u32, Some(stack));
            }
        }
        Kind::Craft => {
            // What the player has, next to what each recipe takes
            let slots = slots_of(p);
            for (slot, (i, r)) in st.crafts.iter().skip(page * PAGE).take(PAGE).enumerate() {
                let (Some(item), Some(recipe)) = (st.items.get(*i), st.items.get(*i).and_then(|item| item.recipes.get(*r))) else { continue };
                let stack = stack_of(st, item, recipe.count);
                stack.add_lore(upright(if th { "&7ใช้:" } else { "&7Takes:" }));
                for ingredient in &recipe.ingredients {
                    let have = have_of(&slots, &ingredient.part);
                    let mark = if have >= ingredient.count { "&a✔" } else { "&c✘" };
                    stack.add_lore(upright(&format!("{mark} &f{} × {} &8({have})", ingredient.count, part_name(st, &ingredient.part))));
                }
                inv.set_item(slot as u32, Some(stack));
            }
        }
    }
    if page > 0 {
        inv.set_item(PREV_SLOT as u32, Some(button("minecraft:arrow", if th { "&eหน้าก่อน" } else { "&ePrevious page" })));
    }
    if page + 1 < total {
        inv.set_item(NEXT_SLOT as u32, Some(button("minecraft:arrow", if th { "&eหน้าถัดไป" } else { "&eNext page" })));
    }
    let hint = match (kind, th) {
        (Kind::Give, true) => "&fคลิก = 1 ชิ้น &7| &fShift+คลิก = เต็มกอง",
        (Kind::Give, false) => "&fClick = 1 item &7| &fShift+click = a full stack",
        (Kind::Craft, true) => "&fคลิก = คราฟ 1 ครั้ง &7| &fShift+คลิก = เท่าที่ทำได้",
        (Kind::Craft, false) => "&fClick = craft once &7| &fShift+click = as many as you can",
    };
    let book = button("minecraft:book", &format!("&7{} {}/{}", if th { "หน้า" } else { "Page" }, page + 1, total));
    book.add_lore(upright(hint));
    inv.set_item(49, Some(book));
    page
}

/// A click while one of our windows is open. True = it was ours (the click is cancelled).
fn on_click(st: &mut State, p: &Player, raw_slot: i16, click: &ClickType) -> bool {
    let name = p.get_name().to_ascii_lowercase();
    // Out of the list while it is worked on: drawing needs the rest of the state
    let Some(mut win) = st.wins.remove(&name) else { return false };
    let (kind, page) = (win.kind, win.page);
    let th = st.cfg.thai();
    let shift = matches!(click, ClickType::ShiftLeft | ClickType::ShiftRight);
    let mut again = false;
    match raw_slot {
        PREV_SLOT if page > 0 => (win.page, again) = (page - 1, true),
        NEXT_SLOT if page + 1 < pages(st, kind) => (win.page, again) = (page + 1, true),
        slot if (0..PAGE as i16).contains(&slot) && kind == Kind::Give => {
            if let Some(item) = st.items.get(page * PAGE + slot as usize).cloned() {
                let full = u32::from(stack_of(st, &item, 1).get_max_count().max(1));
                if give(st, p, &item, if shift { full } else { 1 }) > 0 {
                    c::tell(p, &msg(th, "&eกระเป๋าเต็ม", "&eYour inventory is full"));
                }
            }
        }
        slot if (0..PAGE as i16).contains(&slot) => {
            let entry = st.crafts.get(page * PAGE + slot as usize).and_then(|(i, r)| Some((st.items.get(*i)?.clone(), st.items.get(*i)?.recipes.get(*r)?.clone())));
            if let Some((item, recipe)) = entry {
                let label = if item.name.is_empty() { item.id.clone() } else { item.name.clone() };
                match craft(st, p, &item, &recipe, if shift { 64 } else { 1 }) {
                    Crafted::Made(times) => {
                        c::tell(p, &msg(th, &format!("&aคราฟ &f{label} &a× {}", u32::from(recipe.count) * times), &format!("&aCrafted &f{label} &a× {}", u32::from(recipe.count) * times)));
                        // The window shows what the player has
                        again = true;
                    }
                    Crafted::Missing => c::tell(p, &msg(th, "&cวัตถุดิบไม่พอ", "&cYou do not have the ingredients")),
                    Crafted::Full => c::tell(p, &msg(th, "&eกระเป๋าเต็ม", "&eYour inventory is full")),
                }
            }
        }
        _ => {}
    }
    if again {
        win.page = draw(st, p, &win);
    }
    st.wins.insert(name, win);
    true
}

// ---- the resource pack on its way to players ----------------------------------------

fn pack_uuid() -> Uuid {
    let (high, low) = pack::pack_id();
    Uuid { high, low }
}

/// Where players get the pack now, if it exists and the owner said where it is.
fn pack_link(st: &State) -> Option<(String, String)> {
    let built = st.pack.as_ref()?;
    Some((st.cfg.pack_url(&built.sha1)?, built.sha1.clone()))
}

fn send_pack(st: &mut State, p: &Player) -> bool {
    let (Some((url, sha1)), Some(java)) = (pack_link(st), p.as_java()) else { return false };
    let prompt = (!st.cfg.pack.prompt.is_empty()).then(|| c::color(&st.cfg.pack.prompt));
    java.send_resource_pack(JavaResourcePack { id: pack_uuid(), url, hash: sha1.clone(), forced: st.cfg.pack.required, prompt_message: prompt });
    st.sent += 1;
    let seen = st.players.entry(p.get_name().to_ascii_lowercase()).or_default();
    seen.sent = sha1;
    seen.answer.clear();
    true
}

/// A download: the pack under its hash (kept by caches for good) or as `pack.zip`.
fn respond(pack: Option<&Built>, req: &Request) -> Response {
    let Some(built) = pack else { return Response::not_found() };
    let hashed = req.path.strip_prefix('/').and_then(|p| p.strip_suffix(".zip")).is_some_and(|name| name == built.sha1);
    if !hashed && req.path != "/pack.zip" {
        return Response::not_found();
    }
    Response { status: 200, kind: "application/zip", body: built.bytes.clone(), etag: Some(format!("\"{}\"", built.sha1)), cache: if hashed { mcsv_web::FOREVER } else { mcsv_web::NO_CACHE }, extra: "" }
}

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.tick += 1;

    if let Some(web) = st.web.as_mut() {
        let now = Instant::now();
        let pack = st.pack.as_ref();
        web.poll(now, now + WEB_BUDGET, &mut |req| respond(pack, req));
    }
    if st.redraw {
        st.redraw = false;
        for (name, mut win) in std::mem::take(&mut st.wins) {
            if let Some(p) = server.get_player_by_name(&name) {
                win.page = draw(st, &p, &win);
                st.wins.insert(name, win);
            }
        }
    }
    if st.tick % 10 != 0 {
        return;
    }

    // Players: whoever does not have the pack as it is now gets it
    let players = server.get_all_players();
    let online: Vec<String> = players.iter().map(|p| p.get_name().to_ascii_lowercase()).collect();
    st.players.retain(|name, _| online.contains(name));
    st.wins.retain(|name, _| online.contains(name));
    let Some((_, sha1)) = pack_link(st).filter(|_| st.cfg.pack.send) else { return };
    for p in &players {
        if p.as_java().is_none() || c::awaiting_login(p) {
            continue;
        }
        let (tick, delay) = (st.tick, st.cfg.pack.join_delay_ticks);
        let seen = st.players.entry(p.get_name().to_ascii_lowercase()).or_insert_with(|| Seen { due: tick + delay, ..Seen::default() });
        if seen.sent != sha1 && tick >= seen.due {
            send_pack(st, p);
        }
    }
}

type Data<E> = <E as FromIntoEvent>::Data;

struct OnClick;
impl EventHandler<InventoryClickEvent> for OnClick {
    fn handle(&self, _server: Server, mut e: Data<InventoryClickEvent>) -> Data<InventoryClickEvent> {
        if e.cancelled || !matches!(e.window_type, Some(Screen::Generic9x6)) {
            return e;
        }
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            if !st.wins.is_empty() && on_click(st, &e.player, e.raw_slot, &e.click_type) {
                e.cancelled = true;
            }
        }
        e
    }
}

struct OnClose;
impl EventHandler<InventoryCloseEvent> for OnClose {
    fn handle(&self, _server: Server, e: Data<InventoryCloseEvent>) -> Data<InventoryCloseEvent> {
        // Busy = we are opening a window ourselves: that one stays on the list
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            st.wins.remove(&e.player.get_name().to_ascii_lowercase());
        }
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, e: Data<PlayerLeaveEvent>) -> Data<PlayerLeaveEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let name = e.player.get_name().to_ascii_lowercase();
            st.players.remove(&name);
            st.wins.remove(&name);
        }
        e
    }
}

/// What the client says about our pack, for `/customitems pack`.
struct OnStatus;
impl EventHandler<PlayerResourcePackStatusEvent> for OnStatus {
    fn handle(&self, _server: Server, e: Data<PlayerResourcePackStatusEvent>) -> Data<PlayerResourcePackStatusEvent> {
        if e.pack_id.eq_ignore_ascii_case(&pack::id_text(pack::pack_id())) {
            if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
                if let Some(seen) = st.players.get_mut(&e.player.get_name().to_ascii_lowercase()) {
                    seen.answer = e.status.chars().filter(char::is_ascii_alphanumeric).take(24).collect();
                }
            }
        }
        e
    }
}

// ---- commands ---------------------------------------------------------------------

fn look_text(st: &State, item: &Item) -> String {
    match &item.look {
        _ if st.plain.contains(&item.id) => if st.cfg.thai() { " &c(ไม่พบภาพ)" } else { " &c(picture missing)" }.to_owned(),
        Look::Plain => String::new(),
        Look::Texture { file, .. } => format!(" &8{file}"),
        Look::Model(model) => format!(" &8{model}"),
    }
}

fn problem_lines(st: &State, th: bool) -> Vec<String> {
    let mut lines: Vec<String> = st.problems.iter().take(8).map(|p| format!("&e! &7{}", p.text(th))).collect();
    if st.problems.len() > 8 {
        lines.push(if th { format!("&7และอีก {} ข้อ (ดูใน console ตอนโหลด)", st.problems.len() - 8) } else { format!("&7and {} more (see the console at load)", st.problems.len() - 8) });
    }
    lines
}

fn pack_lines(st: &State, th: bool) -> Vec<String> {
    let mut lines = Vec::new();
    match &st.pack {
        Some(b) => lines.push(if th { format!("&fpack.zip &7{} ไฟล์ {} KB สร้างใน {} ms &8{}", b.files, b.bytes.len().div_ceil(1024), b.took_ms, b.sha1) } else { format!("&fpack.zip &7{} files, {} KB, built in {} ms &8{}", b.files, b.bytes.len().div_ceil(1024), b.took_ms, b.sha1) }),
        None => lines.push(if th { "&cยังไม่มี pack (ดูปัญหาด้วย /customitems reload)".to_owned() } else { "&cThere is no pack (see the problems with /customitems reload)".to_owned() }),
    }
    match (pack_link(st), st.cfg.pack.send) {
        (Some((url, _)), true) => lines.push(if th { format!("&aส่งให้ผู้เล่นตอนเข้าเกม &7จาก &f{url}") } else { format!("&aSent to players when they join, &7from &f{url}") }),
        (Some((url, _)), false) => lines.push(if th { format!("&eไม่ส่งอัตโนมัติ (pack.send = false) &7ลิงก์ &f{url}") } else { format!("&eNot sent by itself (pack.send = false); &7link &f{url}") }),
        (None, _) if st.pack.is_some() => lines.push(if th {
            "&eยังไม่ส่งให้ใคร: ตั้ง pack.url (ลิงก์ที่ฝาก pack.zip) หรือ pack.port + pack.host ใน config.toml".to_owned()
        } else {
            "&eNot sent to anyone yet: set pack.url (a link to the uploaded pack.zip) or pack.port + pack.host in config.toml".to_owned()
        }),
        _ => {}
    }
    match (&st.web, &st.web_error) {
        (Some(web), _) => lines.push(if th { format!("&7พอร์ตดาวน์โหลด &f{} &7ตอบแล้ว {} ครั้ง", web.port, web.served) } else { format!("&7Download port &f{}&7, {} answers so far", web.port, web.served) }),
        (None, Some(why)) => lines.push(if th { format!("&cเปิดพอร์ต {} ไม่ได้: {why}", st.cfg.pack.port) } else { format!("&cPort {} cannot be opened: {why}", st.cfg.pack.port) }),
        _ => {}
    }
    let current = st.pack.as_ref().map_or("", |b| b.sha1.as_str());
    let have = st.players.values().filter(|s| s.sent == current && !current.is_empty()).count();
    let said: Vec<String> = st.players.iter().filter(|(_, s)| !s.answer.is_empty()).map(|(n, s)| format!("{n}={}", s.answer)).take(20).collect();
    lines.push(if th { format!("&7ส่งไปแล้ว {} ครั้ง ผู้เล่นออนไลน์ที่ได้รุ่นนี้ {have} คน {}", st.sent, said.join(" ")) } else { format!("&7Sent {} time(s); {have} online player(s) have this version {}", st.sent, said.join(" ")) });
    lines
}

fn help(th: bool) -> Vec<String> {
    let line = |thai: &str, english: &str| if th { thai.to_owned() } else { english.to_owned() };
    vec![
        line("&bCustomItems &7ไอเท็มที่มีหน้าตาของตัวเอง นิยามใน plugins/data/customitems/items/*.toml", "&bCustomItems &7items with their own look, described in plugins/data/customitems/items/*.toml"),
        line("&f/customitems &7เปิดหน้าต่างเลือกของ · &fgive <ผู้เล่น> <ไอเท็ม> [จำนวน] &7· &flist [หน้า] &7· &finfo", "&f/customitems &7opens the item window · &fgive <player> <item> [amount] &7· &flist [page] &7· &finfo"),
        line("&freload &7อ่านไฟล์และสร้าง pack ใหม่ · &fpack &7สถานะ pack · &fpack send [ผู้เล่น] &7· &fglyphs &7· &fversion", "&freload &7read the files and build the pack again · &fpack &7its state · &fpack send [player] &7· &fglyphs &7· &fversion"),
        line("&f/craft &7(ทุกคน) หน้าต่างคราฟไอเท็มที่มีสูตร", "&f/craft &7(everyone) the window for crafting the items that have a recipe"),
    ]
}

fn run(st: &mut State, sender: &CommandSender, server: &Server, w: &[String]) -> Vec<String> {
    let th = st.cfg.thai();
    let me = c::sender_player(sender);
    let arg = |i: usize| w.get(i).map(String::as_str);
    match arg(0).unwrap_or("") {
        "" | "menu" => match me {
            Some(p) if st.items.is_empty() => {
                drop(p);
                vec![msg(th, "&eยังไม่มีไอเท็ม: เพิ่มใน items/*.toml แล้ว /customitems reload", "&eNo items yet: add some to items/*.toml, then /customitems reload")]
            }
            Some(p) => {
                open_window(st, &p, Kind::Give);
                Vec::new()
            }
            None => help(th),
        },
        "give" => {
            let (Some(who), Some(id)) = (arg(1), arg(2)) else { return vec![msg(th, "&eวิธีใช้: &f/customitems give <ผู้เล่น> <ไอเท็ม> [จำนวน]", "&eUsage: &f/customitems give <player> <item> [amount]")] };
            let Some(item) = st.items.iter().find(|i| i.id == id.to_ascii_lowercase()).cloned() else { return vec![msg(th, &format!("&cไม่มีไอเท็ม &f{id} &7(ดู /customitems list)"), &format!("&cThere is no item &f{id} &7(see /customitems list)"))] };
            let Some(p) = server.get_player_by_name(who) else { return vec![msg(th, &format!("&cไม่พบผู้เล่น &f{who}"), &format!("&cPlayer &f{who} &cis not online"))] };
            let count = arg(3).and_then(|n| n.parse::<u32>().ok()).unwrap_or(1).clamp(1, 2304);
            let left = give(st, &p, &item, count);
            let got = count - left;
            let mut lines = vec![msg(th, &format!("&aให้ &f{} &a× {got} แก่ &f{}", item.id, p.get_name()), &format!("&aGave &f{} &a× {got} to &f{}", item.id, p.get_name()))];
            if left > 0 {
                lines.push(msg(th, &format!("&eกระเป๋าเต็ม ให้ไม่ได้อีก {left} ชิ้น"), &format!("&eThe inventory is full: {left} could not be given")));
            }
            lines
        }
        "list" => {
            if st.items.is_empty() {
                return vec![msg(th, "&7ยังไม่มีไอเท็ม: เพิ่มใน items/*.toml แล้ว /customitems reload", "&7No items yet: add some to items/*.toml, then /customitems reload")];
            }
            const LINES: usize = 10;
            let total = st.items.len().div_ceil(LINES);
            let page = arg(1).and_then(|n| n.parse::<usize>().ok()).unwrap_or(1).clamp(1, total);
            let mut lines = vec![msg(th, &format!("&fไอเท็ม {} ชิ้น &7(หน้า {page}/{total})", st.items.len()), &format!("&f{} item(s) &7(page {page}/{total})", st.items.len()))];
            lines.extend(st.items.iter().skip((page - 1) * LINES).take(LINES).map(|i| format!("&7- &f{} &7{}{}{}", i.id, i.material.trim_start_matches("minecraft:"), look_text(st, i), if i.recipes.is_empty() { String::new() } else { format!(" &8+{}", i.recipes.len()) })));
            lines
        }
        "info" => {
            let Some(p) = me else { return vec![msg(th, "&cใช้ได้เฉพาะผู้เล่น", "&cOnly a player can use this")] };
            match p.get_inventory().get_item_in_hand(Hand::Right).filter(|s| s.get_count() > 0 && !s.get_registry_key().ends_with("air")) {
                Some(held) => match id_of(&held) {
                    Some(id) => vec![msg(th, &format!("&fของในมือคือ &b{id}"), &format!("&fThe item in your hand is &b{id}"))],
                    None => vec![msg(th, &format!("&7ของในมือ ({}) ไม่ใช่ไอเท็มของ CustomItems", held.get_registry_key()), &format!("&7The item in your hand ({}) is not one of CustomItems", held.get_registry_key()))],
                },
                None => vec![msg(th, "&7มือว่าง", "&7Your hand is empty")],
            }
        }
        "reload" => {
            apply(st, load());
            let th = st.cfg.thai();
            let mut lines = vec![msg(
                th,
                &format!("&aโหลดใหม่แล้ว: ไอเท็ม {} ชิ้น สูตร {} สูตร glyph {} ตัว", st.items.len(), st.crafts.len(), st.glyphs.len()),
                &format!("&aReloaded: {} item(s), {} recipe(s), {} glyph(s)", st.items.len(), st.crafts.len(), st.glyphs.len()),
            )];
            lines.extend(pack_lines(st, th));
            lines.extend(problem_lines(st, th));
            lines
        }
        "pack" => match arg(1) {
            Some("send") => {
                if pack_link(st).is_none() {
                    return pack_lines(st, th);
                }
                let targets: Vec<Player> = match arg(2) {
                    Some(who) => server.get_player_by_name(who).into_iter().collect(),
                    None => server.get_all_players(),
                };
                let count = targets.iter().filter(|p| send_pack(st, p)).count();
                vec![msg(th, &format!("&aส่ง pack ให้ {count} คนแล้ว"), &format!("&aThe pack was sent to {count} player(s)"))]
            }
            _ => pack_lines(st, th),
        },
        "glyphs" => {
            if st.glyphs.is_empty() {
                return vec![msg(th, "&7ยังไม่มี glyph: เพิ่มใน glyphs.toml แล้ว /customitems reload", "&7No glyphs yet: add some to glyphs.toml, then /customitems reload")];
            }
            let list: Vec<String> = st.glyphs.iter().map(|(name, ch)| format!("&f{name} &7{ch} &8(\\u{:04X})", *ch as u32)).collect();
            vec![msg(th, &format!("&fglyph {} ตัว &7ใช้ในข้อความด้วย %customitems_<ชื่อ>%", list.len()), &format!("&f{} glyph(s); &7in texts: %customitems_<name>%", list.len())), list.join("&7, ")]
        }
        "version" => vec![msg(th, &format!("&fCustomItems {VERSION} &7· ไอเท็ม {} ชิ้น สูตร {} สูตร glyph {} ตัว", st.items.len(), st.crafts.len(), st.glyphs.len()), &format!("&fCustomItems {VERSION} &7· {} item(s), {} recipe(s), {} glyph(s)", st.items.len(), st.crafts.len(), st.glyphs.len()))],
        _ => help(th),
    }
}

/// `/craft`: the window where players make the items that have a recipe.
struct CraftCmd;
impl CommandHandler for CraftCmd {
    fn handle(&self, sender: CommandSender, _server: Server, _args: ConsumedArgs) -> Result<i32, CommandError> {
        let Some(mut guard) = try_lock() else { return Ok(0) };
        let Some(st) = guard.as_mut() else { return Ok(0) };
        let th = st.cfg.thai();
        match c::sender_player(&sender) {
            Some(p) if c::awaiting_login(&p) => {}
            Some(p) if !st.crafts.is_empty() => open_window(st, &p, Kind::Craft),
            Some(_) => c::reply(&sender, &msg(th, "&7เซิร์ฟเวอร์นี้ยังไม่มีไอเท็มที่คราฟได้", "&7This server has no items to craft yet")),
            None => c::reply(&sender, &msg(th, &format!("&7สูตร {} สูตร ผู้เล่นเปิดหน้าต่างคราฟด้วย /craft", st.crafts.len()), &format!("&7{} recipe(s); players open the crafting window with /craft", st.crafts.len()))),
        }
        Ok(1)
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
            c::reply(&sender, &line);
        }
        Ok(1)
    }
}

struct CustomItems;

impl Plugin for CustomItems {
    fn new() -> Self {
        CustomItems
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Custom items with their own look by MCSV".into(),
            dependencies: vec![],
            // The port is only opened when the owner sets `pack.port`
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into(), NETWORK_TCP_BIND.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:command"), description: "CustomItems".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });

        let loaded = load();
        let th = loaded.cfg.thai();
        let mut st = State {
            cfg: loaded.cfg.clone(),
            items: Vec::new(),
            plain: BTreeSet::new(),
            glyphs: BTreeMap::new(),
            problems: Vec::new(),
            pack: None,
            web: None,
            web_error: None,
            players: HashMap::new(),
            crafts: Vec::new(),
            wins: HashMap::new(),
            redraw: false,
            tick: 0,
            sent: 0,
        };
        apply(&mut st, loaded);
        for p in &st.problems {
            c::log_info(p.text(th));
        }
        if let Some(why) = &st.web_error {
            c::log_info(&format!("port {} cannot be opened: {why}", st.cfg.pack.port));
        }
        c::log_info(&format!("{} item(s), {} glyph(s), pack {}", st.items.len(), st.glyphs.len(), st.pack.as_ref().map_or_else(|| "not built".to_owned(), |b| format!("{} KB in {} ms", b.bytes.len().div_ceil(1024), b.took_ms))));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(st);
        *SERVER.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(context.get_server());
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": c::now_secs() }));

        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let cmd = Command::new(&[NS.to_owned(), "citems".to_owned()], "CustomItems").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:command"));
        let _ = context.register_permission(&Permission { node: format!("{NS}:craft"), description: "CustomItems".into(), default: PermissionDefault::Allow, children: vec![] });
        context.register_command(Command::new(&["craft".to_owned(), "recipes".to_owned()], "CustomItems").execute(CraftCmd), &format!("{NS}:craft"));
        context.register_event_handler(OnClick, EventPriority::Normal, true)?;
        context.register_event_handler(OnClose, EventPriority::Normal, false)?;
        context.register_event_handler(OnLeave, EventPriority::Normal, false)?;
        context.register_event_handler(OnStatus, EventPriority::Normal, false)?;
        pumpkin_plugin_api::scheduler::schedule_repeating_task(1, 1, on_tick);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::remove_file("status.json");
        Ok(())
    }

    /// Messages from other plugins, as JSON:
    ///
    /// | `kind` | fields | answer |
    /// |---|---|---|
    /// | `placeholder` | `params` (a glyph, for `%customitems_<glyph>%` of Placeholders) | `value` |
    /// | `items` | | `items`: `id`, `name`, `material`, `model` (the item model, or none) of each |
    /// | `give` | `player` (name, or UUID with or without dashes), `item` (an id), `count` | `left`: how many did not fit in the inventory |
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let text = |key: &str| v.get(key).and_then(serde_json::Value::as_str).unwrap_or("");
        let guard = try_lock().ok_or("busy")?;
        let st = guard.as_ref().ok_or("not loaded")?;
        let answer = match text("kind") {
            "placeholder" => {
                let name = text("params");
                let ch = st.glyphs.get(name.strip_prefix("glyph_").unwrap_or(name)).ok_or("unknown glyph")?;
                serde_json::json!({ "value": ch.to_string() })
            }
            "items" => serde_json::json!({ "ok": true, "items": st.items.iter().map(|i| serde_json::json!({ "id": i.id, "name": i.name, "material": i.material, "model": i.model_id().filter(|_| !st.plain.contains(&i.id)) })).collect::<Vec<_>>() }),
            "give" => {
                let item = st.items.iter().find(|i| i.id == text("item").to_ascii_lowercase()).ok_or("no such item")?;
                let count = v.get("count").and_then(serde_json::Value::as_u64).unwrap_or(1).clamp(1, 64 * 36) as u32;
                let server = SERVER.try_lock().map_err(|_| "busy")?;
                let who = text("player");
                let hex: String = who.chars().filter(|c| *c != '-').collect::<String>().to_ascii_lowercase();
                let p = server
                    .as_ref()
                    .ok_or("not loaded")?
                    .get_all_players()
                    .into_iter()
                    .find(|p| {
                        let id = p.get_id();
                        p.get_name().eq_ignore_ascii_case(who) || format!("{:016x}{:016x}", id.high, id.low) == hex
                    })
                    .ok_or("no such player online")?;
                serde_json::json!({ "ok": true, "left": give(st, &p, item, count) })
            }
            _ => return Err("unknown kind".into()),
        };
        serde_json::to_vec(&answer).map_err(|e| e.to_string())
    }
}

register_plugin!(CustomItems);

//! Pumpkin WASM glue for Scripts: the server behind the engine's `Host`, the event handlers,
//! script commands and the `/sk` admin command.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::GameMode;
use pumpkin_plugin_api::events::{
    BlockBreakEvent, BlockPlaceEvent, EntityDamageEvent, EntityPickupItemEvent, EventHandler, EventPriority, FromIntoEvent, PlayerBedEnterEvent, PlayerBedLeaveEvent,
    PlayerChangeWorldEvent, PlayerChatEvent, PlayerCommandSendEvent, PlayerDeathEvent, PlayerDropItemEvent, PlayerGamemodeChangeEvent, PlayerInteractEvent,
    PlayerItemConsumeEvent, PlayerJoinEvent, PlayerLeaveEvent, PlayerMoveEvent, PlayerRespawnEvent, PlayerTeleportEvent, PlayerToggleFlightEvent, PlayerToggleSneakEvent,
    EntityDamageByEntityEvent, EntityDeathEvent, InventoryClickEvent, InventoryCloseEvent, PlayerItemHeldEvent, PlayerToggleSprintEvent, ServerCommandEvent, WeatherChangeEvent,
};
use pumpkin_plugin_api::gui::Gui;
use pumpkin_plugin_api::text::TextComponent;
use pumpkin_plugin_api::wit::pumpkin::plugin::common::{ClickType, RgbColor};
use pumpkin_plugin_api::wit::pumpkin::plugin::player::ProjectileType;
use pumpkin_plugin_api::{Entity, Inventory, Screen};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::uuid::Uuid;
use pumpkin_plugin_api::wit::pumpkin::plugin::common::BlockPos;
use pumpkin_plugin_api::wit::pumpkin::plugin::event::InteractAction;
use pumpkin_plugin_api::wit::pumpkin::plugin::player::{BanPlayerOptions, JavaKickOptions, SocketTeardownPolicy};
use pumpkin_plugin_api::wit::pumpkin::plugin::server::CommandSender as ServerSender;
use pumpkin_plugin_api::wit::pumpkin::plugin::sounds::SoundCategory;
use pumpkin_plugin_api::wit::pumpkin::plugin::status_effect::StatusEffectInstance;
use pumpkin_plugin_api::wit::pumpkin::plugin::world::{BlockFlags, ExplosionInteraction};
use pumpkin_plugin_api::world::World;
use pumpkin_plugin_api::{Context, DamageType, ItemStack, Plugin, PluginMetadata, Server, register_plugin};

use crate::ast::{Check, EventKind, Fx, Prop, SendKind};
use crate::cfg::{self, Config, PREFIX, fill, tr};
use crate::engine::{Ctx, Engine, Host, VarStore};
use crate::parse::{self, Names};
use crate::tables;
use crate::value::{self, EntityRef, Item, Loc, PlayerRef, Value, to_text};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NS: &str = "scripts";
const SOUNDS: &str = include_str!("sounds.txt");
/// Commands whose text is a password: scripts never see them.
const SECRET: [&str; 7] = ["login", "l", "register", "reg", "changepassword", "changepw", "unregister"];

/// Work that arrived while the engine was running (Pumpkin calls handlers from inside host calls).
enum Pending {
    Event(EventKind, Ctx),
    Command(String, Value, String),
}

/// A chest menu a script opened for a player.
struct Menu {
    title: String,
    size: u32,
    /// The slots the server shows; changing them changes the open window.
    slots: Inventory,
    /// `format slot`: close on click, command to run on click.
    actions: HashMap<u32, (bool, Option<String>)>,
}

/// A hit whose attacker is told by the event that follows it.
struct Hit {
    tick: u64,
    victim: Value,
    kind: String,
    damage: f32,
    cause: String,
}

struct State {
    cfg: Config,
    eng: Engine,
    /// uuid → name of everyone who joined since Scripts was installed (first join, offline players).
    seen: HashMap<String, String>,
    seen_dirty: bool,
    /// Labels Pumpkin knows as commands of ours.
    registered: HashSet<String>,
    /// Labels of script commands (also readable while the engine runs).
    labels: HashSet<String>,
    rng: u64,
    ticks: u64,
    loaded_fired: bool,
    move_hooked: bool,
    last_block: HashMap<String, (i32, i32, i32)>,
    levels: HashMap<String, i32>,
    /// Tick of the last teleport per player, and teleports waiting for the client to confirm that one.
    tp_guard: HashMap<String, u64>,
    tp_queue: Vec<(u64, PlayerRef, Loc)>,
    log_bytes: u64,
    money_noticed: bool,
    menus: HashMap<String, Menu>,
    /// Players whose menu the script closed: the server tells us later, once.
    closed: HashSet<String>,
    /// Players who joined but have not logged in yet (Login), with "first join".
    pending_join: Vec<(PlayerRef, bool)>,
    hits: HashMap<i32, Hit>,
    /// Who hit an entity last, and when.
    last_hit: HashMap<i32, (Value, u64)>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static CONTEXT: Mutex<Option<Context>> = Mutex::new(None);
static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());
/// One bit per `EventKind` that has triggers, so idle events cost nothing.
static ACTIVE: AtomicU64 = AtomicU64::new(0);
/// Bit 0: a damage trigger is about entities; bit 1: a death trigger is.
static ENTITIES: AtomicU64 = AtomicU64::new(0);

/// Never blocks: WASM has one thread and Pumpkin calls handlers from inside host calls.
fn try_lock() -> Option<std::sync::MutexGuard<'static, Option<State>>> {
    match STATE.try_lock() {
        Ok(g) => Some(g),
        Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

fn pend(p: Pending) {
    let mut q = PENDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if q.len() < 500 {
        q.push(p);
    }
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

fn uuid_text(id: &Uuid) -> String {
    id.to_string()
}

fn uuid_of(text: &str) -> Option<Uuid> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.is_ascii() {
        return None;
    }
    Some(Uuid { high: u64::from_str_radix(&hex[..16], 16).ok()?, low: u64::from_str_radix(&hex[16..], 16).ok()? })
}

fn pref(p: &Player) -> PlayerRef {
    PlayerRef { uuid: uuid_text(&p.get_id()), name: p.get_name() }
}

fn pval(p: &Player) -> Value {
    Value::Player(pref(p))
}

fn world_value(w: &World) -> Value {
    Value::World(value::world_name(&w.get_dimension()))
}

fn loc_of(p: &Player) -> Loc {
    let (x, y, z) = p.get_position();
    Loc { x, y, z, world: value::world_name(&p.get_world().get_dimension()), yaw: p.get_yaw(), pitch: p.get_pitch() }
}

fn block_loc(pos: &BlockPos, world: &str) -> Loc {
    Loc { x: f64::from(pos.x), y: f64::from(pos.y), z: f64::from(pos.z), world: world.to_owned(), yaw: 0.0, pitch: 0.0 }
}

fn world_of(server: &Server, name: &str) -> Option<World> {
    let dim = value::world_dimension(name);
    server.get_all_worlds().into_iter().find(|w| w.get_dimension() == dim)
}

fn empty(stack: &ItemStack) -> bool {
    stack.get_count() == 0 || stack.get_registry_key().ends_with("air")
}

fn item_of(stack: &ItemStack) -> Item {
    let mut item = Item::new(&stack.get_registry_key(), u32::from(stack.get_count()));
    item.name = stack.get_custom_name().map(|n| n.get_text());
    item.ench = stack.get_enchantments().iter().map(|e| (snake(&format!("{:?}", e.enchantment)), e.level)).collect();
    item
}

fn held(p: &Player) -> Option<Item> {
    p.get_inventory_item(p.get_selected_slot()).filter(|s| !empty(s)).map(|s| item_of(&s))
}

/// Text with `&` codes plus hex colours (`&#rrggbb`, `<#rrggbb>`).
fn rich(text: &str) -> TextComponent {
    let text = value::colorize(text);
    let text = text.as_str();
    // (start of the code, length of the code, colour)
    let mut cuts: Vec<(usize, usize, RgbColor)> = Vec::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let (skip, close) = match b[i] {
            b'&' if b.get(i + 1) == Some(&b'#') => (2, 0),
            b'<' if b.get(i + 1) == Some(&b'#') && b.get(i + 8) == Some(&b'>') => (2, 1),
            _ => {
                i += 1;
                continue;
            }
        };
        let hex = text.get(i + skip..i + skip + 6).filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit()));
        match hex.and_then(|h| u32::from_str_radix(h, 16).ok()) {
            Some(n) => {
                cuts.push((i, skip + 6 + close, RgbColor { r: (n >> 16) as u8, g: (n >> 8) as u8, b: n as u8 }));
                i += skip + 6 + close;
            }
            None => i += 1,
        }
    }
    if cuts.is_empty() {
        return c::color(text);
    }
    let mut root = TextComponent::text("");
    if cuts[0].0 > 0 {
        root = root.add_child(c::color(&text[..cuts[0].0]));
    }
    for (n, (at, len, colour)) in cuts.iter().enumerate() {
        let end = cuts.get(n + 1).map_or(text.len(), |next| next.0);
        root = root.add_child(c::color(&text[at + len..end]).color_rgb(*colour));
    }
    root
}

/// `EntityType::SnowGolem` → `snow_golem`
fn snake(debug: &str) -> String {
    let mut out = String::new();
    for ch in debug.rsplit(':').next().unwrap_or_default().chars() {
        if ch.is_uppercase() && !out.is_empty() {
            out.push('_');
        }
        out.extend(ch.to_lowercase());
    }
    out
}

fn eref(e: &Entity) -> EntityRef {
    EntityRef { id: e.get_id() as i32, kind: snake(&format!("{:?}", e.get_type())), world: value::world_name(&e.get_world().get_dimension()) }
}

fn find_entity(server: &Server, id: i32, world: Option<&str>) -> Option<Entity> {
    let wanted = world.map(value::world_dimension);
    for w in server.get_all_worlds() {
        if wanted.as_ref().is_some_and(|d| *d != w.get_dimension()) {
            continue;
        }
        if let Some(e) = w.get_entities().into_iter().find(|e| e.get_id() as i32 == id) {
            return Some(e);
        }
    }
    None
}

/// The player or entity with this entity id, as a script value.
fn entity_value(server: &Server, id: i32) -> Value {
    match player_by_entity(server, id) {
        Some(p) => pval(&p),
        None => find_entity(server, id, None).map_or(Value::None, |e| Value::Entity(eref(&e))),
    }
}

fn stack_of(item: &Item, count: u8) -> ItemStack {
    let stack = ItemStack::new(&item.id, count);
    for (id, level) in &item.ench {
        if let Some(e) = tables::enchantment(id) {
            stack.add_enchantment(e, (*level).clamp(1, 255));
        }
    }
    if let Some(name) = &item.name {
        stack.set_custom_name(Some(rich(&(name))));
    }
    if !item.lore.is_empty() {
        stack.set_lore(item.lore.iter().map(|l| rich(&(l))).collect::<Vec<_>>());
    }
    stack
}

fn gamemode_name(mode: &GameMode) -> &'static str {
    match mode {
        GameMode::Survival => "survival",
        GameMode::Creative => "creative",
        GameMode::Adventure => "adventure",
        GameMode::Spectator => "spectator",
    }
}

/// The Minecraft id of a sound from `entity.player.levelup`, `ENTITY_PLAYER_LEVELUP` or `minecraft:…`.
fn sound_id(name: &str) -> Option<String> {
    let lower = name.trim().to_lowercase();
    let lower = lower.strip_prefix("minecraft:").unwrap_or(&lower).to_owned();
    if lower.contains(':') {
        // a resource pack sound: the client decides whether it exists
        return lower.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '_' | '-' | '/')).then_some(lower);
    }
    let flat = |s: &str| s.chars().filter(char::is_ascii_alphanumeric).collect::<String>();
    let want = flat(&lower);
    SOUNDS.lines().find(|s| *s == lower).or_else(|| SOUNDS.lines().find(|s| flat(s) == want)).map(|s| format!("minecraft:{s}"))
}

// ---- money (Basics keeps it) -------------------------------------------------------------------

enum NoMoney {
    /// Basics is not installed.
    Absent,
    /// Basics answered no: unknown player, busy, not enough money.
    Refused(String),
}

/// One request to Basics: `balance`, `deposit`, `withdraw` or `set`. Returns the balance afterwards.
fn basics_money(kind: &str, player: &str, amount: f64) -> Result<f64, NoMoney> {
    let request = serde_json::json!({ "kind": kind, "player": player, "amount": amount });
    let bytes = serde_json::to_vec(&request).map_err(|e| NoMoney::Refused(e.to_string()))?;
    let answer = match pumpkin_plugin_api::ipc::send_ipc_message("basics", &bytes) {
        Ok(Ok(answer)) => answer,
        Ok(Err(why)) => return Err(NoMoney::Refused(why)),
        Err(()) => return Err(NoMoney::Absent),
    };
    let v: serde_json::Value = serde_json::from_slice(&answer).map_err(|e| NoMoney::Refused(e.to_string()))?;
    if v.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        v.get("balance").and_then(serde_json::Value::as_f64).ok_or_else(|| NoMoney::Refused("no balance in the answer".into()))
    } else {
        Err(NoMoney::Refused(v.get("error").and_then(serde_json::Value::as_str).unwrap_or("refused").to_owned()))
    }
}

// ---- names the parser asks about -------------------------------------------------------------

#[derive(Default)]
struct Registry {
    cache: RefCell<HashMap<String, bool>>,
}

impl Names for Registry {
    fn item(&self, id: &str) -> bool {
        if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return false;
        }
        if let Some(known) = self.cache.borrow().get(id) {
            return *known;
        }
        let known = id == "air" || !ItemStack::new(id, 1).get_registry_key().ends_with("air") || pumpkin_plugin_api::world::get_block_by_name(&format!("minecraft:{id}")).is_some();
        self.cache.borrow_mut().insert(id.to_owned(), known);
        known
    }

    fn entity(&self, id: &str) -> bool {
        tables::entity_type(id).is_some()
    }
}

// ---- the server behind the engine ------------------------------------------------------------

struct W<'a> {
    server: &'a Server,
    cfg: &'a Config,
    rng: &'a mut u64,
    labels: &'a HashSet<String>,
    seen: &'a HashMap<String, String>,
    tick: u64,
    tp_guard: &'a mut HashMap<String, u64>,
    tp_queue: &'a mut Vec<(u64, PlayerRef, Loc)>,
    log_bytes: &'a mut u64,
    money_noticed: &'a mut bool,
    menus: &'a mut HashMap<String, Menu>,
    closed: &'a mut HashSet<String>,
}

impl W<'_> {
    fn money_error(&self, e: NoMoney, name: &str) -> String {
        match e {
            NoMoney::Absent => self.t("money_absent").to_owned(),
            NoMoney::Refused(why) if why == "not enough money" => fill(self.t("money_short"), &[("name", name.to_owned())]),
            NoMoney::Refused(why) => fill(self.t("money_refused"), &[("name", name.to_owned()), ("error", why.chars().take(80).collect())]),
        }
    }

    fn t(&self, key: &str) -> &'static str {
        tr(self.cfg.th(), key)
    }

    fn player(&self, v: &Value) -> Option<Player> {
        match v {
            Value::Player(p) => self.server.get_player_by_uuid(uuid_of(&p.uuid)?),
            _ => None,
        }
    }

    /// Mobs and other entities in a value (players are handled as players).
    fn entities_of(&self, v: &Value) -> Vec<Entity> {
        v.clone().flat().iter().filter_map(|e| match e {
            Value::Entity(r) => find_entity(self.server, r.id, Some(&r.world)),
            _ => None,
        }).collect()
    }

    /// Closes the window on the client. Pumpkin 0.2.2 has no call that closes it on the server too:
    /// the server closes its side when the client next closes any inventory.
    fn close_menu(&mut self, p: &Player) {
        let key = uuid_text(&p.get_id());
        let Some(menu) = self.menus.remove(&key) else { return };
        self.closed.insert(key);
        if let Some(java) = p.as_java() {
            use pumpkin_plugin_api::java_packets::{CCloseContainer, ClientboundPacket};
            java.send_packet(&ClientboundPacket::CCloseContainer(CCloseContainer { sync_id: 0 }));
        }
        pend(Pending::Event(EventKind::InvClose, player_ctx(p).with("inventory", Value::Text(menu.title))));
    }

    fn players_of(&self, v: &Value) -> Vec<Player> {
        v.clone().flat().iter().filter_map(|p| self.player(p)).collect()
    }

    fn place(&self, v: &Value) -> Option<Loc> {
        match v {
            Value::Loc(l) => Some(l.clone()),
            Value::Block(l) => Some(Loc { x: l.x.floor() + 0.5, y: l.y.floor(), z: l.z.floor() + 0.5, ..l.clone() }),
            Value::Player(_) => self.player(v).map(|p| loc_of(&p)),
            Value::Entity(r) => find_entity(self.server, r.id, Some(&r.world)).map(|e| {
                let (x, y, z) = e.get_position();
                Loc { x, y, z, world: r.world.clone(), yaw: e.get_yaw(), pitch: e.get_pitch() }
            }),
            Value::List(l) if l.len() == 1 => self.place(&l[0]),
            _ => None,
        }
    }

    fn block_pos(l: &Loc) -> BlockPos {
        let (x, y, z) = l.block();
        BlockPos { x, y, z }
    }

    fn teleport(&mut self, p: &Player, to: &Loc) -> Result<(), String> {
        let who = pref(p);
        // The client confirms one teleport at a time: a second one right after the first kicks the player
        if self.tp_guard.get(&who.uuid).is_some_and(|t| self.tick.saturating_sub(*t) < 30) {
            if self.tp_queue.len() < 200 {
                let due = self.tp_guard.get(&who.uuid).copied().unwrap_or(self.tick) + 30;
                self.tp_guard.insert(who.uuid.clone(), due);
                self.tp_queue.push((due, who, to.clone()));
            }
            return Ok(());
        }
        let Some(world) = world_of(self.server, &to.world) else { return Err(fill(self.t("no_world"), &[("name", to.world.clone())])) };
        self.tp_guard.insert(who.uuid, self.tick);
        p.teleport_world(world, (to.x, to.y, to.z), Some(to.yaw), Some(to.pitch));
        Ok(())
    }

    fn give(&self, p: &Player, item: &Item) -> u32 {
        if item.id == "air" || item.count == 0 {
            return 0;
        }
        let per = u32::from(ItemStack::new(&item.id, 1).get_max_count().clamp(1, 64));
        let mut left = item.count.min(per * 36);
        let plain = item.name.is_none() && item.lore.is_empty();
        for slot in 0u8..36 {
            if left == 0 {
                break;
            }
            match p.get_inventory_item(slot).filter(|s| !empty(s)) {
                Some(have) => {
                    let room = per.saturating_sub(u32::from(have.get_count()));
                    if plain && room > 0 && value::norm_id(&have.get_registry_key()) == item.id && have.get_custom_name().is_none() {
                        let add = room.min(left);
                        have.set_count((u32::from(have.get_count()) + add) as u8);
                        p.set_inventory_item(slot, Some(have));
                        left -= add;
                    }
                }
                None => {
                    let n = left.min(per);
                    p.set_inventory_item(slot, Some(stack_of(item, n as u8)));
                    left -= n;
                }
            }
        }
        left
    }

    fn take(&self, p: &Player, item: &Item, all: bool) {
        let mut left = if all { u32::MAX } else { item.count };
        for slot in 0u8..41 {
            if left == 0 {
                break;
            }
            let Some(have) = p.get_inventory_item(slot).filter(|s| !empty(s)) else { continue };
            if value::norm_id(&have.get_registry_key()) != item.id {
                continue;
            }
            let count = u32::from(have.get_count());
            if count <= left {
                p.set_inventory_item(slot, None);
                left -= count;
            } else {
                have.set_count((count - left) as u8);
                p.set_inventory_item(slot, Some(have));
                left = 0;
            }
        }
    }

    fn console_command(&mut self, sender: &Value, raw: &str) -> Result<(), String> {
        let command: String = raw.trim().trim_start_matches('/').chars().filter(|c| !c.is_control()).take(1000).collect();
        if command.is_empty() {
            return Ok(());
        }
        let (label, rest) = command.split_once(' ').unwrap_or((&command, ""));
        let label = label.to_lowercase();
        match self.player(sender) {
            Some(p) => {
                if self.labels.contains(&label) {
                    pend(Pending::Command(label, pval(&p), rest.to_owned()));
                } else {
                    self.server.execute_command(&command, ServerSender::Player(p));
                }
            }
            None => {
                if !self.cfg.allow_console_commands {
                    return Err(self.t("console_off").to_owned());
                }
                if self.labels.contains(&label) {
                    pend(Pending::Command(label, Value::Console, rest.to_owned()));
                } else {
                    self.server.execute_command(&command, ServerSender::Console);
                }
            }
        }
        Ok(())
    }
}

impl Host for W<'_> {
    fn now(&mut self) -> f64 {
        now()
    }

    fn random(&mut self) -> f64 {
        // xorshift64*
        let mut x = *self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        *self.rng = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn players(&mut self) -> Vec<PlayerRef> {
        self.server.get_all_players().iter().map(pref).collect()
    }

    fn find_player(&mut self, name: &str) -> Option<PlayerRef> {
        if name.is_empty() || name.len() > 40 {
            return None;
        }
        if let Some(p) = self.server.get_player_by_name(name) {
            return Some(pref(&p));
        }
        let lower = name.to_lowercase();
        let all = self.server.get_all_players();
        if let Some(p) = all.iter().find(|p| p.get_name().to_lowercase() == lower) {
            return Some(pref(p));
        }
        let mut close = all.iter().filter(|p| p.get_name().to_lowercase().starts_with(&lower));
        match (close.next(), close.next()) {
            (Some(p), None) => Some(pref(p)),
            _ => None,
        }
    }

    fn offline_player(&mut self, name: &str) -> PlayerRef {
        let name: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.').take(32).collect();
        if let Some(p) = self.server.get_player_by_name(&name) {
            return pref(&p);
        }
        match self.seen.iter().find(|(_, n)| n.eq_ignore_ascii_case(&name)) {
            Some((uuid, known)) => PlayerRef { uuid: uuid.clone(), name: known.clone() },
            None => PlayerRef { uuid: format!("name-{}", name.to_lowercase()), name },
        }
    }

    fn item(&mut self, text: &str) -> Option<Item> {
        let id = value::norm_id(text);
        let ok = !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        (ok && !ItemStack::new(&id, 1).get_registry_key().ends_with("air")).then(|| Item::new(&id, 1))
    }

    fn get(&mut self, target: &Value, prop: Prop) -> Value {
        match target {
            Value::Block(l) => {
                let Some(world) = world_of(self.server, &l.world) else { return Value::None };
                match prop {
                    Prop::Type | Prop::Name => Value::Item(Item::new(&world.get_block(Self::block_pos(l)).name, 1)),
                    _ => Value::None,
                }
            }
            Value::World(name) => {
                let Some(world) = world_of(self.server, name) else { return Value::None };
                match prop {
                    Prop::Time => Value::Num((world.get_time_of_day() % 24_000) as f64),
                    Prop::Weather => Value::text(if world.is_thundering() { "thunder" } else if world.is_raining() { "rain" } else { "clear" }),
                    _ => Value::None,
                }
            }
            Value::Entity(r) => match prop {
                Prop::Location => self.place(target).map_or(Value::None, Value::Loc),
                Prop::World => Value::World(r.world.clone()),
                Prop::Health | Prop::MaxHealth => match find_entity(self.server, r.id, Some(&r.world)).and_then(|e| e.as_living()) {
                    Some(l) => Value::Num(f64::from(if prop == Prop::Health { l.get_health() } else { l.get_max_health() })),
                    None => Value::None,
                },
                _ => Value::None,
            },
            Value::Player(r) if prop == Prop::Balance => match basics_money("balance", &r.name, 0.0) {
                Ok(balance) => Value::Num(balance),
                Err(NoMoney::Absent) => {
                    // said once: scripts load before we can know whether Basics is there
                    if !*self.money_noticed {
                        *self.money_noticed = true;
                        c::log_info(self.t("money_absent"));
                    }
                    Value::None
                }
                Err(NoMoney::Refused(_)) => Value::None,
            },
            Value::Player(r) => {
                let Some(p) = self.player(target) else {
                    return match prop {
                        Prop::DisplayName | Prop::TabName => Value::Text(r.name.clone()),
                        _ => Value::None,
                    };
                };
                let slot = |s: Option<ItemStack>| s.filter(|s| !empty(s)).map_or(Value::None, |s| Value::Item(item_of(&s)));
                match prop {
                    Prop::DisplayName => Value::Text(p.get_display_name().get_text()),
                    Prop::TabName => Value::Text(p.get_tab_list_name().map_or_else(|| r.name.clone(), |t| t.get_text())),
                    Prop::Location => Value::Loc(loc_of(&p)),
                    Prop::World => world_value(&p.get_world()),
                    Prop::Health => Value::Num(f64::from(p.get_health())),
                    Prop::MaxHealth => Value::Num(f64::from(p.get_max_health())),
                    Prop::Food => Value::Num(f64::from(p.get_food_level())),
                    Prop::Saturation => Value::Num(f64::from(p.get_saturation())),
                    Prop::Level => Value::Num(f64::from(p.get_experience_level())),
                    Prop::Gamemode => Value::text(gamemode_name(&p.get_gamemode())),
                    Prop::Tool => held(&p).map_or(Value::None, Value::Item),
                    Prop::Ip => Value::Text(p.get_ip()),
                    Prop::Ping => Value::Num(f64::from(p.get_ping())),
                    Prop::Flight => Value::Bool(p.get_abilities().allow_flying),
                    Prop::WalkSpeed => Value::Num(f64::from(p.get_abilities().walk_speed)),
                    Prop::FlySpeed => Value::Num(f64::from(p.get_abilities().fly_speed)),
                    Prop::Helmet => slot(p.get_inventory().get_helmet()),
                    Prop::Chestplate => slot(p.get_inventory().get_chestplate()),
                    Prop::Leggings => slot(p.get_inventory().get_leggings()),
                    Prop::Boots => slot(p.get_inventory().get_boots()),
                    Prop::FireTicks => Value::Num(f64::from(p.as_entity().get_fire_ticks())),
                    Prop::Inventory => Value::from_list((0u8..36).filter_map(|s| p.get_inventory_item(s)).filter(|s| !empty(s)).map(|s| Value::Item(item_of(&s))).collect()),
                    Prop::Time => Value::Num((p.get_world().get_time_of_day() % 24_000) as f64),
                    _ => Value::None,
                }
            }
            _ => Value::None,
        }
    }

    fn set(&mut self, target: &Value, prop: Prop, v: &Value) -> Result<(), String> {
        let cannot = || fill(self.t("not_settable"), &[("name", format!("{prop:?}").to_lowercase())]);
        match target {
            Value::Block(l) => {
                let Some(world) = world_of(self.server, &l.world) else { return Err(fill(self.t("no_world"), &[("name", l.world.clone())])) };
                let pos = Self::block_pos(l);
                if world.get_chunk(pos.x >> 4, pos.z >> 4).is_none() {
                    return Err(self.t("chunk_unloaded").to_owned());
                }
                let id = match v {
                    Value::Item(i) => i.id.clone(),
                    Value::None => "air".into(),
                    other => value::norm_id(&to_text(other)),
                };
                let flags = BlockFlags::NOTIFY_NEIGHBORS | BlockFlags::NOTIFY_LISTENERS | BlockFlags::SKIP_DROPS;
                if world.set_block_by_name(pos, &format!("minecraft:{id}"), flags) { Ok(()) } else { Err(fill(self.t("bad_block"), &[("name", id)])) }
            }
            Value::World(name) => {
                let Some(world) = world_of(self.server, name) else { return Err(fill(self.t("no_world"), &[("name", name.clone())])) };
                match prop {
                    Prop::Time => {
                        let t = match v {
                            Value::Text(t) => value::parse_time_of_day(t).map(|t| t as f64),
                            other => other.num(),
                        };
                        world.set_time_of_day(t.ok_or_else(cannot)?.rem_euclid(24_000.0) as u64);
                    }
                    Prop::Weather => {
                        let w = to_text(v).to_lowercase();
                        let thunder = matches!(w.as_str(), "thunder" | "thunderstorm" | "storm" | "thundering");
                        let rain = thunder || matches!(w.as_str(), "rain" | "rainy" | "raining");
                        world.set_raining(rain);
                        world.set_thundering(thunder);
                    }
                    _ => return Err(cannot()),
                }
                Ok(())
            }
            Value::Player(r) if prop == Prop::Balance => {
                let amount = v.num().filter(|n| n.is_finite() && *n >= 0.0).ok_or_else(|| self.t("money_amount").to_owned())?;
                basics_money("set", &r.name, amount).map(|_| ()).map_err(|e| self.money_error(e, &r.name))
            }
            Value::Player(_) => {
                let Some(p) = self.player(target) else { return Err(self.t("no_target").to_owned()) };
                let n = v.num();
                let stack = |v: &Value| match v {
                    Value::Item(i) if i.id != "air" && i.count > 0 => Some(stack_of(i, i.count.min(99) as u8)),
                    _ => None,
                };
                match prop {
                    Prop::Health => {
                        let max = p.get_max_health();
                        let h = n.map_or(max, |n| (n as f32).clamp(0.0, max));
                        if h <= 0.0 {
                            p.kill();
                        } else {
                            p.set_health(h);
                        }
                    }
                    Prop::MaxHealth => p.set_max_health(n.map_or(20.0, |n| (n as f32).clamp(1.0, 1024.0))),
                    Prop::Food => p.set_food_level(n.map_or(20.0, |n| n.clamp(0.0, 20.0)) as u8),
                    Prop::Saturation => p.set_saturation(n.map_or(5.0, |n| n.clamp(0.0, 20.0)) as f32),
                    Prop::Level => p.set_experience_level(n.map_or(0.0, |n| n.clamp(0.0, 1_000_000.0)) as i32),
                    Prop::Gamemode => {
                        let mode = match to_text(v).to_lowercase().as_str() {
                            "survival" | "0" | "<none>" => GameMode::Survival,
                            "creative" | "1" => GameMode::Creative,
                            "adventure" | "2" => GameMode::Adventure,
                            "spectator" | "3" => GameMode::Spectator,
                            _ => return Err(cannot()),
                        };
                        p.set_gamemode(mode);
                    }
                    Prop::DisplayName => p.set_display_name(c::color(&if v.is_none() { p.get_name() } else { value::colorize(&to_text(v)) })),
                    Prop::TabName => p.set_tab_list_name(if v.is_none() { None } else { Some(rich(&(&to_text(v)))) }),
                    Prop::Flight => {
                        let on = v.truthy();
                        p.set_allow_flight(on);
                        if !on {
                            p.set_flying(false);
                        }
                    }
                    Prop::WalkSpeed => p.set_walk_speed(n.map_or(0.1, |n| n.clamp(0.0, 1.0)) as f32),
                    Prop::FlySpeed => p.set_fly_speed(n.map_or(0.05, |n| n.clamp(0.0, 1.0)) as f32),
                    Prop::Tool => p.set_inventory_item(p.get_selected_slot(), stack(v)),
                    Prop::Helmet => p.get_inventory().set_helmet(stack(v)),
                    Prop::Chestplate => p.get_inventory().set_chestplate(stack(v)),
                    Prop::Leggings => p.get_inventory().set_leggings(stack(v)),
                    Prop::Boots => p.get_inventory().set_boots(stack(v)),
                    Prop::FireTicks => p.as_entity().set_fire_ticks(n.unwrap_or(0.0).clamp(0.0, 72_000.0) as i32),
                    Prop::Location => {
                        let to = self.place(v).ok_or_else(|| self.t("no_target").to_owned())?;
                        self.teleport(&p, &to)?;
                    }
                    _ => return Err(cannot()),
                }
                Ok(())
            }
            _ => Err(self.t("no_target").to_owned()),
        }
    }

    fn check(&mut self, check: Check, args: &[Value]) -> bool {
        let Some(who) = args.first() else { return false };
        if check == Check::Banned {
            return matches!(who, Value::Player(r) if uuid_of(&r.uuid).is_some_and(|id| self.server.get_ban_manager().is_player_banned(id)));
        }
        let Some(p) = self.player(who) else { return false };
        match check {
            Check::Online => true,
            Check::HasPerm => {
                let node = args.get(1).map(to_text).unwrap_or_default();
                // Nodes of scripts are not registered with Pumpkin: a rank or /perm grants them, operators have them all
                !node.is_empty() && node.len() <= 100 && (p.has_permission(&node) || p.get_permission_level() as u8 >= PermissionLevel::Two as u8)
            }
            Check::IsOp => p.get_permission_level() as u8 >= PermissionLevel::One as u8,
            Check::Sneaking => p.as_entity().is_sneaking(),
            Check::Sprinting => p.as_entity().is_sprinting(),
            Check::Swimming => p.as_entity().is_swimming(),
            Check::Burning => p.as_entity().is_on_fire(),
            Check::OnGround => p.as_entity().is_on_ground(),
            Check::Flying => p.is_flying(),
            Check::Alive => p.get_health() > 0.0,
            Check::HasItem => match args.get(1) {
                Some(Value::Item(want)) => {
                    let have: u32 = (0u8..41)
                        .filter_map(|s| p.get_inventory_item(s))
                        .filter(|s| !empty(s) && value::norm_id(&s.get_registry_key()) == want.id)
                        .map(|s| u32::from(s.get_count()))
                        .sum();
                    have >= want.count.max(1)
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn fx(&mut self, fx: Fx, a: &[Value], flags: u32) -> Result<(), String> {
        let none = Value::None;
        let arg = |i: usize| a.get(i).unwrap_or(&none);
        let no_target = || self.t("no_target").to_owned();
        match fx {
            Fx::Money => {
                let amount = arg(1).num().filter(|n| n.is_finite() && *n >= 0.0).ok_or_else(|| self.t("money_amount").to_owned())?;
                for who in arg(0).clone().flat() {
                    let Value::Player(r) = who else { continue };
                    basics_money(if flags == 1 { "withdraw" } else { "deposit" }, &r.name, amount).map_err(|e| self.money_error(e, &r.name))?;
                }
            }
            Fx::Teleport => {
                let to = self.place(arg(1)).ok_or_else(no_target)?;
                for p in self.players_of(arg(0)) {
                    self.teleport(&p, &to)?;
                }
                for e in self.entities_of(arg(0)) {
                    if let Some(world) = world_of(self.server, &to.world) {
                        e.teleport((to.x, to.y, to.z), world);
                    }
                }
            }
            Fx::OpenMenu => {
                let rows = arg(0).num().unwrap_or(3.0).clamp(1.0, 6.0) as u32;
                let screen = [Screen::Generic9x1, Screen::Generic9x2, Screen::Generic9x3, Screen::Generic9x4, Screen::Generic9x5, Screen::Generic9x6][rows as usize - 1];
                let title: String = if arg(1).is_none() { String::new() } else { to_text(arg(1)).chars().take(64).collect() };
                for p in self.players_of(arg(2)) {
                    let gui = Gui::new(screen, rich(&title));
                    // a menu is for clicking: nothing can be taken out or put in
                    gui.set_allow_grab_items(false);
                    gui.set_allow_put_items(false);
                    let slots = gui.get_inventory();
                    let key = uuid_text(&p.get_id());
                    self.closed.remove(&key);
                    self.menus.insert(key, Menu { title: title.clone(), size: rows * 9, slots, actions: HashMap::new() });
                    p.open_gui(gui);
                }
            }
            Fx::SetSlot => {
                let slot = arg(0).num().filter(|n| (0.0..=53.0).contains(n)).ok_or_else(no_target)? as u32;
                let stack = |v: &Value| match v {
                    Value::Item(i) if i.id != "air" && i.count > 0 => Some(stack_of(i, i.count.min(99) as u8)),
                    _ => None,
                };
                for p in self.players_of(arg(1)) {
                    if flags & 1 == 0 {
                        if slot < 41 {
                            p.set_inventory_item(slot as u8, stack(arg(2)));
                        }
                        continue;
                    }
                    let Some(menu) = self.menus.get_mut(&uuid_text(&p.get_id())).filter(|m| slot < m.size) else { return Err(self.t("no_menu").to_owned()) };
                    menu.slots.set_item(slot, stack(arg(2)));
                    menu.actions.remove(&slot);
                    if flags & 48 != 0 {
                        let command = (flags & 32 != 0 && !arg(3).is_none()).then(|| to_text(arg(3)).chars().take(256).collect());
                        menu.actions.insert(slot, (flags & 16 != 0, command));
                    }
                }
            }
            Fx::CloseMenu => {
                for p in self.players_of(arg(0)) {
                    self.close_menu(&p);
                }
            }
            Fx::Drop => {
                // the one way Pumpkin 0.2.2 makes an item entity with a stack: the summon command with NBT
                let at = self.place(arg(1)).ok_or_else(no_target)?;
                let dim = value::world_dimension(&at.world);
                for item in arg(0).clone().flat() {
                    let Value::Item(item) = item else { continue };
                    if item.id == "air" || !item.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
                        continue;
                    }
                    let mut left = item.count.min(640);
                    while left > 0 {
                        let n = left.min(64);
                        left -= n;
                        let line = format!("execute in {dim} run summon minecraft:item {:.2} {:.2} {:.2} {{Item:{{id:\"minecraft:{}\",count:{n}}}}}", at.x, at.y, at.z, item.id);
                        self.server.execute_command(&line, ServerSender::Console);
                    }
                }
            }
            Fx::Equip => {
                for p in self.players_of(arg(0)) {
                    for item in arg(1).clone().flat() {
                        let Value::Item(item) = item else { continue };
                        let stack = Some(stack_of(&item, 1));
                        let inv = p.get_inventory();
                        match item.id.rsplit('_').next().unwrap_or_default() {
                            "helmet" | "cap" | "head" | "skull" | "pumpkin" => inv.set_helmet(stack),
                            "chestplate" | "tunic" | "elytra" => inv.set_chestplate(stack),
                            "leggings" | "pants" => inv.set_leggings(stack),
                            "boots" => inv.set_boots(stack),
                            _ => p.set_inventory_item(p.get_selected_slot(), stack),
                        }
                    }
                }
            }
            Fx::Shoot => {
                let kind = match to_text(arg(1)).as_str() {
                    "arrow" => ProjectileType::Arrow,
                    "snowball" => ProjectileType::Snowball,
                    "egg" => ProjectileType::Egg,
                    "ender_pearl" => ProjectileType::EnderPearl,
                    "splash_potion" => ProjectileType::SplashPotion,
                    "fireball" => ProjectileType::Fireball,
                    "small_fireball" => ProjectileType::SmallFireball,
                    "trident" => ProjectileType::Trident,
                    _ => ProjectileType::WindCharge,
                };
                for p in self.players_of(arg(0)) {
                    p.launch_projectile(kind);
                }
            }
            Fx::Give => {
                let mut left = 0;
                for p in self.players_of(arg(1)) {
                    for item in arg(0).clone().flat() {
                        if let Value::Item(item) = item {
                            left += self.give(&p, &item);
                        }
                    }
                }
                if left > 0 {
                    return Err(format!("inventory full: {left} items were not given"));
                }
            }
            Fx::Take => {
                for p in self.players_of(arg(1)) {
                    for item in arg(0).clone().flat() {
                        if let Value::Item(item) = item {
                            self.take(&p, &item, flags == 1);
                        }
                    }
                }
            }
            Fx::ClearInventory => self.players_of(arg(0)).iter().for_each(|p| p.get_inventory().clear_all()),
            Fx::Kill => {
                self.players_of(arg(0)).iter().for_each(Player::kill);
                for e in self.entities_of(arg(0)) {
                    match e.as_living() {
                        Some(l) => l.damage(f32::MAX, DamageType::GenericKill),
                        None => e.remove(),
                    }
                }
            }
            Fx::Heal => {
                let amount = arg(1).num();
                for p in self.players_of(arg(0)) {
                    match (amount, flags) {
                        (Some(n), 1) => p.damage(n.clamp(0.0, 10_000.0) as f32, DamageType::Generic),
                        (Some(n), _) => p.set_health((p.get_health() + n.max(0.0) as f32).min(p.get_max_health())),
                        (None, _) => p.set_health(p.get_max_health()),
                    }
                }
                for l in self.entities_of(arg(0)).iter().filter_map(Entity::as_living) {
                    match (amount, flags) {
                        (Some(n), 1) => l.damage(n.clamp(0.0, 10_000.0) as f32, DamageType::Generic),
                        (Some(n), _) => l.set_health((l.get_health() + n.max(0.0) as f32).min(l.get_max_health())),
                        (None, _) => l.set_health(l.get_max_health()),
                    }
                }
            }
            Fx::Feed => {
                let amount = arg(1).num();
                for p in self.players_of(arg(0)) {
                    match amount {
                        Some(n) => p.set_food_level((f64::from(p.get_food_level()) + n.max(0.0)).min(20.0) as u8),
                        None => {
                            p.set_food_level(20);
                            p.set_saturation(5.0);
                        }
                    }
                }
            }
            Fx::Kick => {
                let reason = if arg(1).is_none() { "Kicked".to_owned() } else { value::colorize(&to_text(arg(1))) };
                for p in self.players_of(arg(0)) {
                    match p.as_java() {
                        Some(java) => java.kick(JavaKickOptions { reason: c::color(&reason), log_to_console: true, teardown_policy: SocketTeardownPolicy::Graceful }),
                        None => self.server.execute_command(&format!("kick {} {}", p.get_name(), c::plain(&reason)), ServerSender::Console),
                    }
                }
            }
            Fx::Ban | Fx::Unban => {
                for who in arg(0).clone().flat() {
                    let Value::Player(r) = who else { continue };
                    let Some(id) = uuid_of(&r.uuid) else { return Err(fill(self.t("no_player"), &[("name", r.name.clone())])) };
                    if fx == Fx::Unban {
                        self.server.get_ban_manager().unban_player(id);
                        continue;
                    }
                    let reason = (!arg(1).is_none()).then(|| rich(&(&to_text(arg(1)))));
                    let options = BanPlayerOptions { reason, source: Some("Scripts".into()), expires_at_utc: None, duration_seconds: None, kick_if_online: true, log_to_console: true };
                    self.server.get_ban_manager().ban_player(&r.name, id, options);
                }
            }
            Fx::Op | Fx::Deop => {
                if !self.cfg.allow_console_commands {
                    return Err(self.t("console_off").to_owned());
                }
                for who in arg(0).clone().flat() {
                    let Value::Player(r) = who else { continue };
                    if r.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') && !r.name.is_empty() {
                        self.server.execute_command(&format!("{} {}", if fx == Fx::Op { "op" } else { "deop" }, r.name), ServerSender::Console);
                    }
                }
            }
            Fx::ConsoleCmd => {
                for line in arg(0).clone().flat() {
                    self.console_command(&Value::Console, &to_text(&line))?;
                }
            }
            Fx::PlayerCmd => {
                for sender in arg(1).clone().flat() {
                    for line in arg(0).clone().flat() {
                        self.console_command(&sender, &to_text(&line))?;
                    }
                }
            }
            Fx::Sound => {
                let name = to_text(arg(0));
                let Some(id) = sound_id(&name) else { return Err(format!("unknown sound: {}", name.chars().take(60).collect::<String>())) };
                let volume = arg(1).num().unwrap_or(1.0).clamp(0.0, 10.0) as f32;
                let pitch = arg(2).num().unwrap_or(1.0).clamp(0.0, 2.0) as f32;
                if flags & 2 != 0 {
                    let at = self.place(arg(4)).ok_or_else(no_target)?;
                    let world = world_of(self.server, &at.world).ok_or_else(no_target)?;
                    world.play_custom_sound(&id, SoundCategory::Master, (at.x, at.y, at.z), volume, pitch);
                } else {
                    for p in self.players_of(arg(3)) {
                        p.play_custom_sound(&id, SoundCategory::Master, volume, pitch);
                    }
                }
            }
            Fx::Lightning => {
                let at = self.place(arg(0)).ok_or_else(no_target)?;
                world_of(self.server, &at.world).ok_or_else(no_target)?.strike_lightning((at.x, at.y, at.z), flags & 1 != 0);
            }
            Fx::Explosion => {
                let at = self.place(arg(1)).ok_or_else(no_target)?;
                let power = arg(0).num().unwrap_or(0.0).clamp(0.0, 16.0) as f32;
                let how = if flags & 1 != 0 { ExplosionInteraction::None } else { ExplosionInteraction::Tnt };
                world_of(self.server, &at.world).ok_or_else(no_target)?.create_explosion((at.x, at.y, at.z), power, flags & 2 != 0, how);
            }
            Fx::Potion | Fx::Unpotion => {
                let name = to_text(arg(0));
                let id = parse::potion_id(&name).map(str::to_owned).unwrap_or_else(|| value::norm_id(&name));
                let Some(effect) = tables::status_effect(&id) else { return Err(fill(self.t("bad_potion"), &[("name", name)])) };
                if fx == Fx::Unpotion {
                    self.players_of(arg(1)).iter().for_each(|p| p.remove_effect(effect));
                } else {
                    let tier = arg(1).num().unwrap_or(1.0).clamp(1.0, 255.0) as u8;
                    let secs = arg(3).num().unwrap_or(15.0).clamp(0.05, 1_000_000.0);
                    let instance = StatusEffectInstance { effect_type: effect, duration: (secs * 20.0) as u32, amplifier: tier - 1, ambient: false, show_particles: true, show_icon: true };
                    self.players_of(arg(2)).iter().for_each(|p| p.add_effect(instance));
                }
            }
            Fx::Spawn => {
                let name = to_text(arg(0));
                let Some(kind) = tables::entity_type(&name) else { return Err(fill(self.t("bad_entity"), &[("name", name)])) };
                let at = self.place(arg(1)).ok_or_else(no_target)?;
                let world = world_of(self.server, &at.world).ok_or_else(no_target)?;
                for _ in 0..arg(2).num().unwrap_or(1.0).clamp(1.0, 50.0) as u32 {
                    world.spawn_entity(kind, (at.x, at.y, at.z));
                }
            }
            Fx::Push => {
                let speed = arg(1).num().unwrap_or(1.0).clamp(-10.0, 10.0);
                for p in self.players_of(arg(0)) {
                    let yaw = f64::from(p.get_yaw()).to_radians();
                    let v = match flags {
                        1 => (0.0, speed, 0.0),
                        2 => (0.0, -speed, 0.0),
                        3 => (0.0, 0.0, -speed),
                        4 => (0.0, 0.0, speed),
                        5 => (speed, 0.0, 0.0),
                        6 => (-speed, 0.0, 0.0),
                        7 => (-yaw.sin() * speed, 0.0, yaw.cos() * speed),
                        _ => (yaw.sin() * speed, 0.0, -yaw.cos() * speed),
                    };
                    p.set_velocity(v);
                }
            }
            Fx::Ignite => {
                let ticks = (arg(1).num().unwrap_or(8.0).clamp(0.0, 3600.0) * 20.0) as i32;
                self.players_of(arg(0)).iter().for_each(|p| p.as_entity().set_fire_ticks(ticks));
                self.entities_of(arg(0)).iter().for_each(|e| e.set_fire_ticks(ticks));
            }
            Fx::Extinguish => self.players_of(arg(0)).iter().for_each(|p| p.as_entity().set_fire_ticks(0)),
            Fx::Log => {
                let line: String = c::plain(&to_text(arg(0))).chars().filter(|c| !c.is_control()).take(1000).collect();
                if arg(1).is_none() {
                    c::log_info(&line);
                    return Ok(());
                }
                let name = to_text(arg(1));
                let Some(rel) = cfg::log_file(&name) else { return Err(fill(self.t("bad_file"), &[("name", name.chars().take(60).collect())])) };
                // a script in a loop must not fill the disk
                if *self.log_bytes > 64 * 1024 * 1024 {
                    return Err("log files written this session exceed 64 MB; logging to files is paused until a restart".into());
                }
                let path = c::data_path(&rel);
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let stamp = value::fmt_date(now() + self.date_offset(), value::DATE_FORMAT);
                let text = format!("[{stamp}] {line}\n");
                *self.log_bytes += text.len() as u64;
                use std::io::Write;
                let done = std::fs::OpenOptions::new().create(true).append(true).open(&path).and_then(|mut f| f.write_all(text.as_bytes()));
                done.map_err(|e| format!("cannot write {rel}: {e}"))?;
            }
            Fx::Title => {
                let ticks = |i: usize, default: i32| arg(i).num().map_or(default, |s| (s * 20.0).clamp(0.0, 72_000.0) as i32);
                for p in self.players_of(arg(2)) {
                    p.send_title_animation(ticks(4, 10), ticks(3, 70), ticks(5, 20));
                    if !arg(1).is_none() {
                        p.show_subtitle(rich(&(&to_text(arg(1)))));
                    }
                    p.show_title(rich(&(&to_text(arg(0)))));
                }
            }
        }
        Ok(())
    }

    fn send(&mut self, to: &Value, text: &str, kind: SendKind) {
        let text = value::colorize(text);
        match to {
            Value::Console => c::log_info(&c::plain(&text)),
            _ => {
                if let Some(p) = self.player(to) {
                    match kind {
                        SendKind::Chat => p.send_system_message(rich(&text), false),
                        SendKind::ActionBar => p.send_system_message(rich(&text), true),
                    }
                }
            }
        }
    }

    fn broadcast(&mut self, text: &str) {
        let text = value::colorize(text);
        for p in self.server.get_all_players() {
            p.send_system_message(rich(&text), false);
        }
        c::log_info(&c::plain(&text));
    }

    fn log(&mut self, line: &str) {
        c::log_info(line);
    }

    fn tps(&mut self) -> f64 {
        self.server.get_tps().min(20.0)
    }

    fn target_block(&mut self, player: &Value) -> Value {
        let Some(p) = self.player(player) else { return Value::None };
        match p.get_target_block(6) {
            Some((x, y, z)) => Value::Block(Loc { x, y, z, world: value::world_name(&p.get_world().get_dimension()), yaw: 0.0, pitch: 0.0 }),
            None => Value::None,
        }
    }

    fn date_offset(&self) -> f64 {
        self.cfg.timezone_offset_hours * 3600.0
    }

    fn entities(&mut self, around: Option<(&Loc, f64)>) -> Vec<Value> {
        let players = self.server.get_all_players();
        let ids: HashMap<i32, PlayerRef> = players.iter().map(|p| (p.as_entity().get_id() as i32, pref(p))).collect();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let worlds = match around {
            Some((at, _)) => world_of(self.server, &at.world).into_iter().collect(),
            None => self.server.get_all_worlds(),
        };
        let near = |(x, y, z): (f64, f64, f64)| around.is_none_or(|(at, r)| (x - at.x).powi(2) + (y - at.y).powi(2) + (z - at.z).powi(2) <= r * r);
        for w in &worlds {
            let dim = w.get_dimension();
            for e in w.get_entities() {
                // a script may not walk an unbounded number of entities in one go
                if out.len() >= 500 {
                    return out;
                }
                let id = e.get_id() as i32;
                if near(e.get_position()) && seen.insert(id) {
                    out.push(match ids.get(&id) {
                        Some(p) => Value::Player(p.clone()),
                        None => Value::Entity(eref(&e)),
                    });
                }
            }
            for p in players.iter().filter(|p| p.get_world().get_dimension() == dim) {
                if near(p.get_position()) && seen.insert(p.as_entity().get_id() as i32) && out.len() < 500 {
                    out.push(pval(p));
                }
            }
        }
        out
    }
}

// ---- running the engine ----------------------------------------------------------------------

fn refresh(st: &mut State) {
    use EventKind::*;
    let mut bits = 0u64;
    for kind in [
        Join, FirstJoin, Quit, Chat, Command, Death, Respawn, Break, Place, Click, Damage, Drop, Pickup, Consume, Sneak, Sprint, Flight, WorldChange, GamemodeChange, LevelChange, BedEnter,
        BedLeave, Teleport, Move, WeatherChange, InvClick, InvClose, ToolChange,
    ] {
        if st.eng.has(kind) {
            bits |= 1 << kind as u64;
        }
    }
    ACTIVE.store(bits, Ordering::Relaxed);
    ENTITIES.store(u64::from(st.eng.wants_entities(Damage)) | u64::from(st.eng.wants_entities(Death)) << 1, Ordering::Relaxed);
    st.labels = st.eng.command_labels().into_iter().collect();
}

fn active(kind: EventKind) -> bool {
    ACTIVE.load(Ordering::Relaxed) & (1 << kind as u64) != 0
}

/// Runs `f` with the engine and the server behind it.
fn with<R>(st: &mut State, server: &Server, f: impl FnOnce(&mut Engine, &mut W) -> R) -> R {
    let State { cfg, eng, seen, labels, rng, ticks, tp_guard, tp_queue, log_bytes, money_noticed, menus, closed, .. } = st;
    let mut w = W { server, cfg, rng, labels, seen, tick: *ticks, tp_guard, tp_queue, log_bytes, money_noticed, menus, closed };
    f(eng, &mut w)
}

/// Work that could not run while the engine was busy runs now, after it (it can no longer cancel anything).
fn drain(st: &mut State, server: &Server) {
    for _ in 0..64 {
        let next = {
            let mut q = PENDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if q.is_empty() { None } else { Some(q.remove(0)) }
        };
        let Some(next) = next else { break };
        match next {
            Pending::Event(kind, ctx) => {
                let ctx = with(st, server, |eng, w| eng.fire(w, kind, ctx));
                after_event(server, kind, &ctx);
            }
            Pending::Command(label, sender, args) => {
                with(st, server, |eng, w| eng.run_command(w, &label, sender, &args));
            }
        }
    }
}

/// Things an event's triggers asked for that are done outside the event.
fn after_event(server: &Server, kind: EventKind, ctx: &Ctx) {
    let key = match kind {
        EventKind::Join | EventKind::FirstJoin => "join message",
        EventKind::Quit => "quit message",
        _ => return,
    };
    // The game's own line stays (MCSV counts players from it); the script's text is an extra line
    if let Some(Value::Text(text)) = ctx.vals.get(key) {
        let text = value::colorize(text);
        for p in server.get_all_players() {
            p.send_system_message(rich(&text), false);
        }
    }
}

/// Fires an event now, or queues it when the engine is busy. `None` = it could not run now.
fn fire(server: &Server, kind: EventKind, ctx: Ctx) -> Option<Ctx> {
    let Some(mut guard) = try_lock() else {
        pend(Pending::Event(kind, ctx));
        return None;
    };
    let st = guard.as_mut()?;
    let ctx = with(st, server, |eng, w| eng.fire(w, kind, ctx));
    after_event(server, kind, &ctx);
    drain(st, server);
    Some(ctx)
}

fn player_ctx(p: &Player) -> Ctx {
    Ctx::default().with("player", pval(p)).with("world", world_value(&p.get_world()))
}

/// Players who have not logged in yet (Login) do not exist for scripts.
fn hidden(p: &Player) -> bool {
    c::awaiting_login(p)
}

fn block_ctx(p: &Player, name: &str, pos: &BlockPos) -> Ctx {
    let world = value::world_name(&p.get_world().get_dimension());
    let loc = block_loc(pos, &world);
    player_ctx(p).with("block", Value::Block(loc.clone())).with("location", Value::Loc(loc)).with("block type", Value::Item(Item::new(name, 1)))
}

fn player_by_entity(server: &Server, entity_id: i32) -> Option<Player> {
    server.get_all_players().into_iter().find(|p| p.as_entity().get_id() as i32 == entity_id)
}

// ---- scripts on disk -------------------------------------------------------------------------

/// (path under `scripts/`, script key, enabled) of every script file, sorted. Folders may be three deep;
/// a `-` in front of the file or of one of its folders switches the script off.
fn script_files() -> Vec<(String, String, bool)> {
    fn walk(rel: &str, depth: u32, out: &mut Vec<(String, String, bool)>) {
        let Ok(dir) = std::fs::read_dir(c::data_path(&format!("scripts/{rel}"))) else { return };
        for entry in dir.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if depth < 3 && out.len() < 2000 {
                    walk(&path, depth + 1, out);
                }
            } else if let Some(stem) = path.strip_suffix(".sk") {
                if let Some(key) = cfg::script_path(stem) {
                    let enabled = !path.split('/').any(|part| part.starts_with('-'));
                    out.push((path.clone(), key, enabled));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk("", 0, &mut out);
    out.sort();
    out
}

fn register_command(ctx: &Context, name: &str, aliases: &[String]) {
    let mut names = vec![name.to_owned()];
    names.extend(aliases.iter().cloned());
    let handler = || ScriptCmd { name: name.to_owned() };
    let cmd = Command::new(&names, "Scripts command").execute(handler()).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(handler()));
    ctx.register_command(cmd, &format!("{NS}:script"));
}

/// Tells Pumpkin about script commands it does not know yet. Returns the new labels.
fn register_new(st: &mut State) -> Vec<String> {
    let guard = CONTEXT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(ctx) = guard.as_ref() else { return Vec::new() };
    let mut fresh = Vec::new();
    let mut names: Vec<String> = st.eng.command_labels();
    names.sort();
    for label in names {
        let Some(def) = st.eng.command(&label).cloned() else { continue };
        if def.name != label || st.registered.contains(&label) {
            continue;
        }
        let aliases: Vec<String> = def.aliases.iter().filter(|a| !st.registered.contains(*a)).cloned().collect();
        register_command(ctx, &def.name, &aliases);
        st.registered.insert(def.name.clone());
        st.registered.extend(aliases);
        fresh.push(def.name.clone());
    }
    fresh
}

/// Loads one file. Returns the lines to show (errors and warnings) and the error count.
fn load_file(st: &mut State, file: &str, name: &str, registry: &Registry) -> (Vec<String>, usize) {
    let th = st.cfg.th();
    let path = c::data_path(&format!("scripts/{file}"));
    let size = std::fs::metadata(&path).map_or(0, |m| m.len());
    if size > st.cfg.max_script_bytes {
        return (vec![fill(tr(th, "too_big"), &[("name", file.to_owned()), ("max", st.cfg.max_script_bytes.to_string())])], 1);
    }
    let Ok(src) = std::fs::read_to_string(&path) else {
        return (vec![fill(tr(th, "unreadable"), &[("name", file.to_owned())])], 1);
    };
    let loaded = st.eng.load(name, &src, registry);
    let mut lines = Vec::new();
    for e in &loaded.errors {
        lines.push(fill(tr(th, "error"), &[("file", file.to_owned()), ("text", cfg::format_error(th, e))]));
    }
    for e in &loaded.warnings {
        lines.push(fill(tr(th, "warning"), &[("file", file.to_owned()), ("text", cfg::format_error(th, e))]));
    }
    let errors = loaded.errors.len();
    let counts = [("name", file.to_owned()), ("commands", loaded.commands.len().to_string()), ("triggers", loaded.triggers.len().to_string()), ("functions", loaded.functions.len().to_string()), ("errors", errors.to_string())];
    lines.push(fill(tr(th, if errors == 0 { "reloaded" } else { "reloaded_errors" }), &counts));
    (lines, errors)
}

/// (Re)loads every enabled script. Scripts whose file is gone or disabled are unloaded first.
fn load_all(st: &mut State, server: Option<&Server>) -> (Vec<String>, usize, usize) {
    let files = script_files();
    let registry = Registry::default();
    let keep: HashSet<&String> = files.iter().filter(|f| f.2).map(|f| &f.1).collect();
    let gone: Vec<String> = st.eng.scripts.keys().filter(|k| !keep.contains(k)).cloned().collect();
    for name in gone {
        unload(st, server, &name);
    }
    // functions of every file are known before the first one is parsed
    st.eng.extra_functions.clear();
    for (file, _, enabled) in &files {
        if *enabled {
            if let Ok(src) = std::fs::read_to_string(c::data_path(&format!("scripts/{file}"))) {
                st.eng.extra_functions.extend(parse::function_names(&src));
            }
        }
    }
    let (mut lines, mut errors, mut count) = (Vec::new(), 0, 0);
    for (file, name, enabled) in &files {
        if !*enabled {
            continue;
        }
        if let Some(server) = server {
            with(st, server, |eng, w| eng.fire_script(w, name, EventKind::Unload));
        }
        let (mut l, e) = load_file(st, file, name, &registry);
        // one summary line per clean file is noise on a reload of everything
        if e == 0 {
            l.pop();
        }
        lines.append(&mut l);
        errors += e;
        count += 1;
        if let Some(server) = server {
            with(st, server, |eng, w| eng.fire_script(w, name, EventKind::Load));
        }
    }
    st.eng.extra_functions.clear();
    refresh(st);
    (lines, errors, count)
}

fn unload(st: &mut State, server: Option<&Server>, name: &str) {
    match server {
        Some(server) => {
            with(st, server, |eng, w| eng.unload(w, name));
        }
        None => {
            st.eng.scripts.remove(name);
        }
    }
    refresh(st);
}

fn save(st: &mut State, force: bool) {
    if st.eng.vars.dirty || force {
        if c::save_json("variables.json", &st.eng.vars.to_json()) {
            st.eng.vars.dirty = false;
        }
    }
    if st.seen_dirty && c::save_json("seen.json", &st.seen) {
        st.seen_dirty = false;
    }
}

// ---- tick ------------------------------------------------------------------------------------

fn on_tick(server: Server) {
    let Some(mut guard) = try_lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    st.ticks += 1;
    if !st.loaded_fired {
        // `on load` runs here: the worlds do not exist yet while the plugin is being loaded
        st.loaded_fired = true;
        let names: Vec<String> = st.eng.scripts.keys().cloned().collect();
        for name in names {
            with(st, &server, |eng, w| eng.fire_script(w, &name, EventKind::Load));
        }
    }
    with(st, &server, |eng, w| eng.tick(w));
    // teleports that waited for the client to confirm an earlier one
    if st.tp_queue.iter().any(|(due, ..)| *due <= st.ticks) {
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut st.tp_queue).into_iter().partition(|(due, ..)| *due <= st.ticks);
        st.tp_queue = later;
        for (_, who, to) in due {
            st.tp_guard.remove(&who.uuid);
            with(st, &server, |_, w| {
                if let Some(p) = w.player(&Value::Player(who.clone())) {
                    let _ = w.teleport(&p, &to);
                }
            });
        }
    }
    if st.ticks % 5 == 0 && !st.pending_join.is_empty() {
        for (who, first) in std::mem::take(&mut st.pending_join) {
            let Some(p) = uuid_of(&who.uuid).and_then(|id| server.get_player_by_uuid(id)) else { continue };
            if hidden(&p) {
                st.pending_join.push((who, first));
                continue;
            }
            for kind in [EventKind::FirstJoin, EventKind::Join] {
                if active(kind) && (first || kind == EventKind::Join) {
                    let ctx = with(st, &server, |eng, w| eng.fire(w, kind, player_ctx(&p)));
                    after_event(&server, kind, &ctx);
                }
            }
        }
    }
    // hits whose attacker never came: the triggers run now, too late to cancel
    if st.hits.values().any(|h| h.tick < st.ticks) {
        let late: Vec<i32> = st.hits.iter().filter(|(_, h)| h.tick < st.ticks).map(|(id, _)| *id).collect();
        for id in late {
            if let Some(hit) = st.hits.remove(&id) {
                let ctx = damage_ctx(&server, &hit, Value::None);
                with(st, &server, |eng, w| eng.fire(w, EventKind::Damage, ctx));
            }
        }
    }
    if st.ticks % 20 == 0 {
        st.last_hit.retain(|_, (_, t)| st.ticks.saturating_sub(*t) < 200);
        if active(EventKind::LevelChange) {
            for p in server.get_all_players() {
                let (key, level) = (uuid_text(&p.get_id()), p.get_experience_level());
                if st.levels.insert(key, level).is_some_and(|old| old != level) && !hidden(&p) {
                    let ctx = player_ctx(&p).with("level", Value::Num(f64::from(level)));
                    with(st, &server, |eng, w| eng.fire(w, EventKind::LevelChange, ctx));
                }
            }
        }
        if st.ticks % (st.cfg.save_interval_seconds * 20) == 0 {
            save(st, false);
        }
        if st.ticks % 6000 == 0 {
            st.tp_guard.retain(|_, t| st.ticks.saturating_sub(*t) < 100);
        }
    }
    drain(st, &server);
}

// ---- events ----------------------------------------------------------------------------------

type Data<E> = <E as FromIntoEvent>::Data;

struct OnJoin;
impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, server: Server, e: Data<PlayerJoinEvent>) -> Data<PlayerJoinEvent> {
        let who = pref(&e.player);
        let first = match try_lock().as_mut().and_then(|g| g.as_mut()) {
            Some(st) => {
                let first = !st.seen.contains_key(&who.uuid);
                if st.seen.get(&who.uuid) != Some(&who.name) && st.seen.len() < 500_000 {
                    st.seen.insert(who.uuid.clone(), who.name.clone());
                    st.seen_dirty = true;
                }
                st.levels.insert(who.uuid.clone(), e.player.get_experience_level());
                // the game teleports a joining player; ours must wait for the client to confirm that one
                st.tp_guard.insert(who.uuid.clone(), st.ticks);
                first
            }
            None => false,
        };
        // With Login the welcome waits until the password is given (see `on_tick`)
        if hidden(&e.player) {
            if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
                if st.pending_join.len() < 500 {
                    st.pending_join.push((who, first));
                }
            }
            return e;
        }
        if first && active(EventKind::FirstJoin) {
            fire(&server, EventKind::FirstJoin, player_ctx(&e.player));
        }
        if active(EventKind::Join) {
            fire(&server, EventKind::Join, player_ctx(&e.player));
        }
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, server: Server, e: Data<PlayerLeaveEvent>) -> Data<PlayerLeaveEvent> {
        if active(EventKind::Quit) {
            fire(&server, EventKind::Quit, player_ctx(&e.player));
        }
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            let key = uuid_text(&e.player.get_id());
            st.last_block.remove(&key);
            st.menus.remove(&key);
            st.closed.remove(&key);
            st.levels.remove(&key);
            st.tp_guard.remove(&key);
        }
        e
    }
}

struct OnChat;
impl EventHandler<PlayerChatEvent> for OnChat {
    fn handle(&self, server: Server, mut e: Data<PlayerChatEvent>) -> Data<PlayerChatEvent> {
        if e.cancelled || !active(EventKind::Chat) || hidden(&e.player) {
            return e;
        }
        let ctx = player_ctx(&e.player).with("message", Value::Text(e.message.clone()));
        if let Some(ctx) = fire(&server, EventKind::Chat, ctx) {
            e.cancelled |= ctx.cancelled;
            if let Some(Value::Text(new)) = ctx.vals.get("message") {
                if *new != e.message {
                    e.message = new.chars().filter(|c| !c.is_control()).take(256).collect();
                }
            }
        }
        e
    }
}

/// `label` and the rest of a command line, or `None` for commands scripts must not see.
fn split_command(line: &str) -> Option<(String, String)> {
    let line = line.trim().trim_start_matches('/');
    let (label, rest) = line.split_once(' ').unwrap_or((line, ""));
    let label = label.to_lowercase();
    let short = label.rsplit(':').next().unwrap_or(&label).to_owned();
    if SECRET.contains(&short.as_str()) {
        return None;
    }
    if matches!(short.as_str(), "auth" | "authme") && matches!(rest.split_whitespace().next().map(str::to_lowercase).as_deref(), Some("setpassword" | "register")) {
        return None;
    }
    (!label.is_empty()).then(|| (label, rest.trim().to_owned()))
}

fn command_ctx(base: Ctx, label: &str, rest: &str) -> Ctx {
    base.with("command", Value::text(label))
        .with("filter", Value::text(label))
        .with("arguments", Value::text(rest))
        .with("full command", Value::text(format!("{label} {rest}").trim().to_owned()))
}

struct OnCommand;
impl EventHandler<PlayerCommandSendEvent> for OnCommand {
    fn handle(&self, server: Server, mut e: Data<PlayerCommandSendEvent>) -> Data<PlayerCommandSendEvent> {
        if e.cancelled || hidden(&e.player) {
            return e;
        }
        let Some((label, rest)) = split_command(&e.command) else { return e };
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let sender = pval(&e.player);
        if active(EventKind::Command) {
            let ctx = command_ctx(player_ctx(&e.player).with("sender", sender.clone()), &label, &rest);
            if with(st, &server, |eng, w| eng.fire(w, EventKind::Command, ctx)).cancelled {
                e.cancelled = true;
            }
        }
        // A script command wins over a command of the same name from the game or another plugin
        if !e.cancelled && st.labels.contains(&label) {
            e.cancelled = true;
            with(st, &server, |eng, w| eng.run_command(w, &label, sender, &rest));
        }
        drain(st, &server);
        e
    }
}

struct OnConsoleCommand;
impl EventHandler<ServerCommandEvent> for OnConsoleCommand {
    fn handle(&self, server: Server, mut e: Data<ServerCommandEvent>) -> Data<ServerCommandEvent> {
        if e.cancelled {
            return e;
        }
        let Some((label, rest)) = split_command(&e.command) else { return e };
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        if active(EventKind::Command) {
            let ctx = command_ctx(Ctx::default().with("sender", Value::Console), &label, &rest);
            if with(st, &server, |eng, w| eng.fire(w, EventKind::Command, ctx)).cancelled {
                e.cancelled = true;
            }
        }
        // Commands Pumpkin could not be told about still work from the console
        if !e.cancelled && st.labels.contains(&label) && !st.registered.contains(&label) {
            e.cancelled = true;
            with(st, &server, |eng, w| eng.run_command(w, &label, Value::Console, &rest));
        }
        drain(st, &server);
        e
    }
}

struct OnDeath;
impl EventHandler<PlayerDeathEvent> for OnDeath {
    fn handle(&self, server: Server, mut e: Data<PlayerDeathEvent>) -> Data<PlayerDeathEvent> {
        if e.cancelled || !active(EventKind::Death) {
            return e;
        }
        let before = e.death_message.get_text();
        let attacker = match try_lock().as_mut().and_then(|g| g.as_mut()) {
            Some(st) => st.last_hit.remove(&(e.player.as_entity().get_id() as i32)).map_or(Value::None, |(a, _)| a),
            None => Value::None,
        };
        let ctx = player_ctx(&e.player)
            .with("victim", pval(&e.player))
            .with("victim type", Value::text("player"))
            .with("filter", Value::text("player"))
            .with("attacker", attacker)
            .with("death message", Value::Text(before.clone()));
        if let Some(ctx) = fire(&server, EventKind::Death, ctx) {
            e.cancelled |= ctx.cancelled;
            if let Some(Value::Text(new)) = ctx.vals.get("death message") {
                if *new != before {
                    e.death_message = rich(&(new));
                }
            }
        }
        e
    }
}

struct OnRespawn;
impl EventHandler<PlayerRespawnEvent> for OnRespawn {
    fn handle(&self, server: Server, e: Data<PlayerRespawnEvent>) -> Data<PlayerRespawnEvent> {
        if active(EventKind::Respawn) {
            let (x, y, z) = e.position;
            let world = value::world_name(&e.respawned_world.get_dimension());
            let ctx = Ctx::default().with("player", pval(&e.player)).with("world", Value::World(world.clone())).with("location", Value::Loc(Loc { x, y, z, world, yaw: e.yaw, pitch: e.pitch }));
            fire(&server, EventKind::Respawn, ctx);
        }
        e
    }
}

struct OnBreak;
impl EventHandler<BlockBreakEvent> for OnBreak {
    fn handle(&self, server: Server, mut e: Data<BlockBreakEvent>) -> Data<BlockBreakEvent> {
        if e.cancelled || !active(EventKind::Break) {
            return e;
        }
        let Some(p) = &e.player else { return e };
        if hidden(p) {
            return e;
        }
        let ctx = block_ctx(p, &e.block, &e.block_pos).with("tool", held(p).map_or(Value::None, Value::Item));
        if let Some(ctx) = fire(&server, EventKind::Break, ctx) {
            e.cancelled |= ctx.cancelled;
        }
        e
    }
}

struct OnPlace;
impl EventHandler<BlockPlaceEvent> for OnPlace {
    fn handle(&self, server: Server, mut e: Data<BlockPlaceEvent>) -> Data<BlockPlaceEvent> {
        if e.cancelled || !active(EventKind::Place) || hidden(&e.player) {
            return e;
        }
        let ctx = block_ctx(&e.player, &e.block_placed, &e.block_pos).with("tool", held(&e.player).map_or(Value::None, Value::Item));
        if let Some(ctx) = fire(&server, EventKind::Place, ctx) {
            e.cancelled |= ctx.cancelled;
        }
        e
    }
}

struct OnInteract;
impl EventHandler<PlayerInteractEvent> for OnInteract {
    fn handle(&self, server: Server, mut e: Data<PlayerInteractEvent>) -> Data<PlayerInteractEvent> {
        if e.cancelled || !active(EventKind::Click) || hidden(&e.player) {
            return e;
        }
        let side = match e.action {
            InteractAction::LeftClickBlock | InteractAction::LeftClickAir => "left",
            InteractAction::RightClickBlock | InteractAction::RightClickAir => "right",
        };
        let tool = held(&e.player).map_or(Value::None, Value::Item);
        let base = match &e.clicked_pos {
            Some(pos) => block_ctx(&e.player, &e.block, pos),
            None => player_ctx(&e.player).with("block type", Value::Item(Item::new("air", 1))),
        };
        let ctx = base.with("filter", Value::text(side)).with("tool", tool.clone()).with("item", tool);
        if let Some(ctx) = fire(&server, EventKind::Click, ctx) {
            e.cancelled |= ctx.cancelled;
        }
        e
    }
}

/// Damage types that come with an attacker: their trigger waits for `EntityDamageByEntityEvent`.
const HAS_ATTACKER: [&str; 19] = [
    "player_attack", "mob_attack", "mob_attack_no_aggro", "arrow", "trident", "mob_projectile", "thrown", "fireball", "wither_skull", "sting", "spit", "wind_charge",
    "mace_smash", "spear", "thorns", "player_explosion", "sonic_boom", "indirect_magic", "fireworks",
];

fn damage_ctx(server: &Server, hit: &Hit, attacker: Value) -> Ctx {
    let mut ctx = Ctx::default()
        .with("victim", hit.victim.clone())
        .with("victim type", Value::text(hit.kind.clone()))
        .with("filter", Value::text(if hit.kind == "player" { "player" } else { "entity" }))
        .with("damage", Value::Num(f64::from(hit.damage)))
        .with("cause", Value::Text(hit.cause.replace('_', " ")))
        .with("attacker", attacker);
    match &hit.victim {
        Value::Player(r) => {
            ctx = ctx.with("player", hit.victim.clone());
            if let Some(p) = uuid_of(&r.uuid).and_then(|id| server.get_player_by_uuid(id)) {
                ctx = ctx.with("world", world_value(&p.get_world()));
            }
        }
        Value::Entity(r) => ctx = ctx.with("world", Value::World(r.world.clone())),
        _ => {}
    }
    ctx
}

/// The player or (when a trigger asks for entities) the entity that an event is about.
fn victim_of(server: &Server, entity_id: i32, entities: bool) -> Option<(Value, String)> {
    if let Some(p) = player_by_entity(server, entity_id) {
        return (!hidden(&p)).then(|| (pval(&p), "player".to_owned()));
    }
    if !entities {
        return None;
    }
    let e = find_entity(server, entity_id, None)?;
    let r = eref(&e);
    let kind = r.kind.clone();
    Some((Value::Entity(r), kind))
}

struct OnDamage;
impl EventHandler<EntityDamageEvent> for OnDamage {
    fn handle(&self, server: Server, mut e: Data<EntityDamageEvent>) -> Data<EntityDamageEvent> {
        if e.cancelled || !active(EventKind::Damage) {
            return e;
        }
        let Some((victim, kind)) = victim_of(&server, e.entity_id, ENTITIES.load(Ordering::Relaxed) & 1 != 0) else { return e };
        let hit = Hit { tick: 0, victim, kind, damage: e.damage, cause: snake(&format!("{:?}", e.damage_type)) };
        if HAS_ATTACKER.contains(&hit.cause.as_str()) {
            // the next event of this hit says who did it; the trigger runs there
            if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
                if st.hits.len() < 2000 {
                    st.hits.insert(e.entity_id, Hit { tick: st.ticks, ..hit });
                }
            }
            return e;
        }
        if let Some(ctx) = fire(&server, EventKind::Damage, damage_ctx(&server, &hit, Value::None)) {
            e.cancelled |= ctx.cancelled;
            if let Some(n) = ctx.vals.get("damage").and_then(Value::num) {
                e.damage = (n as f32).max(0.0);
            }
        }
        e
    }
}

struct OnDamageBy;
impl EventHandler<EntityDamageByEntityEvent> for OnDamageBy {
    fn handle(&self, server: Server, mut e: Data<EntityDamageByEntityEvent>) -> Data<EntityDamageByEntityEvent> {
        if e.cancelled || !(active(EventKind::Damage) || active(EventKind::Death)) {
            return e;
        }
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let pending = st.hits.remove(&e.entity_id);
        if pending.is_none() && !active(EventKind::Death) {
            return e;
        }
        let attacker = entity_value(&server, e.damager_id);
        if st.last_hit.len() < 5000 && !attacker.is_none() {
            st.last_hit.insert(e.entity_id, (attacker.clone(), st.ticks));
        }
        // without a pending hit the trigger already ran from the first event of this hit
        if let Some(mut hit) = pending {
            hit.damage = e.damage;
            let ctx = with(st, &server, |eng, w| eng.fire(w, EventKind::Damage, damage_ctx(&server, &hit, attacker)));
            e.cancelled |= ctx.cancelled;
            if let Some(n) = ctx.vals.get("damage").and_then(Value::num) {
                e.damage = (n as f32).max(0.0);
            }
            drain(st, &server);
        }
        e
    }
}

struct OnEntityDeath;
impl EventHandler<EntityDeathEvent> for OnEntityDeath {
    fn handle(&self, server: Server, e: Data<EntityDeathEvent>) -> Data<EntityDeathEvent> {
        // players die through `PlayerDeathEvent`
        if !active(EventKind::Death) || ENTITIES.load(Ordering::Relaxed) & 2 == 0 || player_by_entity(&server, e.entity_id).is_some() {
            return e;
        }
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let Some((victim, kind)) = victim_of(&server, e.entity_id, true) else { return e };
        let attacker = st.last_hit.remove(&e.entity_id).map_or(Value::None, |(a, _)| a);
        let hit = Hit { tick: 0, victim, kind, damage: 0.0, cause: String::new() };
        let ctx = damage_ctx(&server, &hit, attacker);
        with(st, &server, |eng, w| eng.fire(w, EventKind::Death, ctx));
        drain(st, &server);
        e
    }
}

/// What the player clicked with, in Skript's words.
fn click_name(kind: &ClickType) -> &'static str {
    match kind {
        ClickType::Left => "left",
        ClickType::Right => "right",
        ClickType::ShiftLeft => "shift left",
        ClickType::ShiftRight => "shift right",
        ClickType::Middle => "middle",
        ClickType::Drop => "drop",
        ClickType::ControlDrop => "control drop",
        ClickType::DoubleClick => "double click",
        ClickType::NumberKey => "number key",
        ClickType::Unknown => "unknown",
    }
}

struct OnInvClick;
impl EventHandler<InventoryClickEvent> for OnInvClick {
    fn handle(&self, server: Server, mut e: Data<InventoryClickEvent>) -> Data<InventoryClickEvent> {
        if hidden(&e.player) {
            return e;
        }
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let key = uuid_text(&e.player.get_id());
        let (title, action) = match st.menus.get(&key) {
            Some(menu) => {
                let inside = e.raw_slot >= 0 && (e.raw_slot as u32) < menu.size;
                // a script menu never gives its items away, whatever the triggers say
                e.cancelled |= inside;
                (menu.title.clone(), menu.actions.get(&(e.raw_slot.max(0) as u32)).filter(|_| inside).cloned())
            }
            None => (String::new(), None),
        };
        if active(EventKind::InvClick) {
            let item = e.clicked_item.as_ref().filter(|s| !empty(s)).map_or(Value::None, |s| Value::Item(item_of(s)));
            let ctx = player_ctx(&e.player)
                .with("slot", Value::Num(f64::from(e.slot)))
                .with("raw slot", Value::Num(f64::from(e.raw_slot)))
                .with("item", item)
                .with("inventory", Value::Text(title))
                .with("click type", Value::text(click_name(&e.click_type)));
            e.cancelled |= with(st, &server, |eng, w| eng.fire(w, EventKind::InvClick, ctx)).cancelled;
        }
        if let Some((close, command)) = action {
            let who = pval(&e.player);
            with(st, &server, |_, w| {
                if close {
                    w.close_menu(&e.player);
                }
                if let Some(command) = command {
                    let _ = w.console_command(&who, &command);
                }
            });
        }
        drain(st, &server);
        e
    }
}

struct OnInvClose;
impl EventHandler<InventoryCloseEvent> for OnInvClose {
    fn handle(&self, server: Server, e: Data<InventoryCloseEvent>) -> Data<InventoryCloseEvent> {
        // busy = one of our own effects is replacing the window: that effect looks after the menu
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let key = uuid_text(&e.player.get_id());
        // the script closed this one already and its trigger ran then
        if st.closed.remove(&key) && !st.menus.contains_key(&key) {
            return e;
        }
        let title = st.menus.remove(&key).map(|m| m.title).unwrap_or_default();
        if active(EventKind::InvClose) && !hidden(&e.player) {
            let ctx = player_ctx(&e.player).with("inventory", Value::Text(title));
            with(st, &server, |eng, w| eng.fire(w, EventKind::InvClose, ctx));
            drain(st, &server);
        }
        e
    }
}

struct OnHeld;
impl EventHandler<PlayerItemHeldEvent> for OnHeld {
    fn handle(&self, server: Server, mut e: Data<PlayerItemHeldEvent>) -> Data<PlayerItemHeldEvent> {
        if e.cancelled || !active(EventKind::ToolChange) || hidden(&e.player) {
            return e;
        }
        let item = e.player.get_inventory_item(e.new_slot).filter(|s| !empty(s)).map_or(Value::None, |s| Value::Item(item_of(&s)));
        let ctx = player_ctx(&e.player).with("slot", Value::Num(f64::from(e.new_slot))).with("item", item);
        if let Some(ctx) = fire(&server, EventKind::ToolChange, ctx) {
            e.cancelled |= ctx.cancelled;
        }
        e
    }
}

struct OnDrop;
impl EventHandler<PlayerDropItemEvent> for OnDrop {
    fn handle(&self, server: Server, mut e: Data<PlayerDropItemEvent>) -> Data<PlayerDropItemEvent> {
        if e.cancelled || !active(EventKind::Drop) || hidden(&e.player) {
            return e;
        }
        let ctx = player_ctx(&e.player).with("item", Value::Item(Item::new(&e.item_name, u32::from(e.count))));
        if let Some(ctx) = fire(&server, EventKind::Drop, ctx) {
            e.cancelled |= ctx.cancelled;
        }
        e
    }
}

struct OnPickup;
impl EventHandler<EntityPickupItemEvent> for OnPickup {
    fn handle(&self, server: Server, mut e: Data<EntityPickupItemEvent>) -> Data<EntityPickupItemEvent> {
        if e.cancelled || !active(EventKind::Pickup) {
            return e;
        }
        let Some(p) = player_by_entity(&server, e.entity_id) else { return e };
        if hidden(&p) {
            return e;
        }
        // Pumpkin 0.2.2 passes the entity type here, not the item: scripts get the amount only
        let ctx = player_ctx(&p).with("amount", Value::Num(f64::from(e.count)));
        if let Some(ctx) = fire(&server, EventKind::Pickup, ctx) {
            e.cancelled |= ctx.cancelled;
        }
        e
    }
}

struct OnConsume;
impl EventHandler<PlayerItemConsumeEvent> for OnConsume {
    fn handle(&self, server: Server, mut e: Data<PlayerItemConsumeEvent>) -> Data<PlayerItemConsumeEvent> {
        if e.cancelled || !active(EventKind::Consume) || hidden(&e.player) {
            return e;
        }
        let ctx = player_ctx(&e.player).with("item", Value::Item(Item::new(&e.item_name, 1)));
        if let Some(ctx) = fire(&server, EventKind::Consume, ctx) {
            e.cancelled |= ctx.cancelled;
        }
        e
    }
}

/// Events that carry a player, a cancel flag and nothing a script reads.
macro_rules! simple_event {
    ($handler:ident, $event:ty, $kind:expr) => {
        struct $handler;
        impl EventHandler<$event> for $handler {
            fn handle(&self, server: Server, mut e: Data<$event>) -> Data<$event> {
                if e.cancelled || !active($kind) || hidden(&e.player) {
                    return e;
                }
                if let Some(ctx) = fire(&server, $kind, player_ctx(&e.player)) {
                    e.cancelled |= ctx.cancelled;
                }
                e
            }
        }
    };
}

simple_event!(OnSneak, PlayerToggleSneakEvent, EventKind::Sneak);
simple_event!(OnSprint, PlayerToggleSprintEvent, EventKind::Sprint);
simple_event!(OnFlight, PlayerToggleFlightEvent, EventKind::Flight);

struct OnWorldChange;
impl EventHandler<PlayerChangeWorldEvent> for OnWorldChange {
    fn handle(&self, server: Server, mut e: Data<PlayerChangeWorldEvent>) -> Data<PlayerChangeWorldEvent> {
        if e.cancelled || !active(EventKind::WorldChange) || hidden(&e.player) {
            return e;
        }
        let ctx = Ctx::default().with("player", pval(&e.player)).with("world", world_value(&e.new_world)).with("past world", world_value(&e.previous_world));
        if let Some(ctx) = fire(&server, EventKind::WorldChange, ctx) {
            e.cancelled |= ctx.cancelled;
        }
        e
    }
}

struct OnGamemode;
impl EventHandler<PlayerGamemodeChangeEvent> for OnGamemode {
    fn handle(&self, server: Server, mut e: Data<PlayerGamemodeChangeEvent>) -> Data<PlayerGamemodeChangeEvent> {
        if e.cancelled || !active(EventKind::GamemodeChange) || hidden(&e.player) {
            return e;
        }
        let mode = gamemode_name(&e.new_gamemode);
        let ctx = player_ctx(&e.player).with("gamemode", Value::text(mode)).with("filter", Value::text(mode));
        if let Some(ctx) = fire(&server, EventKind::GamemodeChange, ctx) {
            e.cancelled |= ctx.cancelled;
        }
        e
    }
}

struct OnBedEnter;
impl EventHandler<PlayerBedEnterEvent> for OnBedEnter {
    fn handle(&self, server: Server, mut e: Data<PlayerBedEnterEvent>) -> Data<PlayerBedEnterEvent> {
        if e.cancelled || !active(EventKind::BedEnter) || hidden(&e.player) {
            return e;
        }
        let world = value::world_name(&e.player.get_world().get_dimension());
        let ctx = player_ctx(&e.player).with("block", Value::Block(block_loc(&e.bed_pos, &world)));
        if let Some(ctx) = fire(&server, EventKind::BedEnter, ctx) {
            e.cancelled |= ctx.cancelled;
        }
        e
    }
}

struct OnBedLeave;
impl EventHandler<PlayerBedLeaveEvent> for OnBedLeave {
    fn handle(&self, server: Server, e: Data<PlayerBedLeaveEvent>) -> Data<PlayerBedLeaveEvent> {
        if active(EventKind::BedLeave) && !hidden(&e.player) {
            let world = value::world_name(&e.player.get_world().get_dimension());
            fire(&server, EventKind::BedLeave, player_ctx(&e.player).with("block", Value::Block(block_loc(&e.bed_pos, &world))));
        }
        e
    }
}

struct OnTeleport;
impl EventHandler<PlayerTeleportEvent> for OnTeleport {
    fn handle(&self, server: Server, mut e: Data<PlayerTeleportEvent>) -> Data<PlayerTeleportEvent> {
        if let Some(st) = try_lock().as_mut().and_then(|g| g.as_mut()) {
            st.tp_guard.insert(uuid_text(&e.player.get_id()), st.ticks);
        }
        if e.cancelled || !active(EventKind::Teleport) || hidden(&e.player) {
            return e;
        }
        let world = value::world_name(&e.player.get_world().get_dimension());
        let at = |(x, y, z): (f64, f64, f64)| Value::Loc(Loc { x, y, z, world: world.clone(), yaw: 0.0, pitch: 0.0 });
        let ctx = player_ctx(&e.player).with("location", at(e.to_position)).with("past location", at(e.from_position));
        if let Some(ctx) = fire(&server, EventKind::Teleport, ctx) {
            e.cancelled |= ctx.cancelled;
        }
        e
    }
}

struct OnMove;
impl EventHandler<PlayerMoveEvent> for OnMove {
    fn handle(&self, server: Server, mut e: Data<PlayerMoveEvent>) -> Data<PlayerMoveEvent> {
        if e.cancelled || !active(EventKind::Move) {
            return e;
        }
        let (x, y, z) = e.to_position;
        let at = (x.floor() as i32, y.floor() as i32, z.floor() as i32);
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        // only a step onto another block counts: this event comes twenty times a second per player
        let key = uuid_text(&e.player.get_id());
        if st.last_block.insert(key, at) == Some(at) || hidden(&e.player) {
            return e;
        }
        let world = e.player.get_world();
        let name = value::world_name(&world.get_dimension());
        let mut ctx = player_ctx(&e.player).with("location", Value::Loc(Loc { x, y, z, world: name.clone(), yaw: 0.0, pitch: 0.0 }));
        if st.eng.has_filtered(EventKind::Move) {
            let below = BlockPos { x: at.0, y: (y - 0.05).floor() as i32, z: at.2 };
            ctx = ctx.with("block type", Value::Item(Item::new(&world.get_block(below).name, 1))).with("block", Value::Block(block_loc(&below, &name)));
        }
        if with(st, &server, |eng, w| eng.fire(w, EventKind::Move, ctx)).cancelled {
            e.cancelled = true;
        }
        drain(st, &server);
        e
    }
}

struct OnWeather;
impl EventHandler<WeatherChangeEvent> for OnWeather {
    fn handle(&self, server: Server, mut e: Data<WeatherChangeEvent>) -> Data<WeatherChangeEvent> {
        if e.cancelled || !active(EventKind::WeatherChange) {
            return e;
        }
        let weather = if e.to_weather_state { "rain" } else { "clear" };
        let ctx = Ctx::default().with("world", world_value(&e.target_world)).with("weather", Value::text(weather)).with("filter", Value::text(weather));
        if let Some(ctx) = fire(&server, EventKind::WeatherChange, ctx) {
            e.cancelled |= ctx.cancelled;
        }
        e
    }
}

/// The move event is only hooked once a script needs it.
fn hook_move(st: &mut State) {
    if st.move_hooked || !st.eng.has(EventKind::Move) {
        return;
    }
    if let Some(ctx) = CONTEXT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() {
        st.move_hooked = ctx.register_event_handler(OnMove, EventPriority::Normal, true).is_ok();
    }
}

// ---- commands --------------------------------------------------------------------------------

fn sender_value(sender: &CommandSender) -> Value {
    match c::sender_player(sender) {
        Some(p) => pval(&p),
        None => Value::Console,
    }
}

fn greedy(args: &ConsumedArgs) -> String {
    c::words(args, "args").join(" ")
}

/// A command defined by a script (typed in the console, or run by another plugin).
struct ScriptCmd {
    name: String,
}

impl CommandHandler for ScriptCmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let who = sender_value(&sender);
        let rest = greedy(&args);
        if c::sender_player(&sender).is_some_and(|p| hidden(&p)) {
            return Ok(0);
        }
        let Some(mut guard) = try_lock() else {
            pend(Pending::Command(self.name.clone(), who, rest));
            return Ok(1);
        };
        let Some(st) = guard.as_mut() else { return Ok(0) };
        if !with(st, &server, |eng, w| eng.run_command(w, &self.name, who, &rest)) {
            c::reply(&sender, &format!("{PREFIX}{}", tr(st.cfg.th(), "gone_command")));
        }
        drain(st, &server);
        Ok(1)
    }
}

struct Admin;

impl CommandHandler for Admin {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let words = c::words(&args, "args");
        let Some(mut guard) = try_lock() else {
            c::reply(&sender, &format!("{PREFIX}{}", tr(true, "busy")));
            return Ok(0);
        };
        let Some(st) = guard.as_mut() else { return Ok(0) };
        let th = st.cfg.th();
        let console = c::sender_player(&sender).is_none();
        // players see the lines; the console always gets them too
        let say = |text: &str| {
            c::reply(&sender, &format!("{PREFIX}{text}"));
            if !console {
                c::log_info(&c::plain(text));
            }
        };
        let name_arg = words.get(1..).map(|w| w.join(" ")).unwrap_or_default();
        match words.first().map(|w| w.to_lowercase()).as_deref() {
            Some("reload") => {
                if matches!(name_arg.to_lowercase().as_str(), "" | "all" | "scripts" | "config") {
                    st.cfg = c::load_config::<Config>("config.toml").cleaned();
                    st.eng.limits = st.cfg.limits();
                    st.eng.msgs = st.cfg.messages();
                    let (lines, errors, files) = load_all(st, Some(&server));
                    lines.iter().for_each(|l| say(l));
                    say(&fill(tr(th, "reload_all"), &[("files", files.to_string()), ("errors", errors.to_string())]));
                } else {
                    let Some(want) = cfg::script_path(&name_arg) else {
                        say(&fill(tr(th, "not_found"), &[("name", name_arg.chars().take(40).collect())]));
                        return Ok(0);
                    };
                    match script_files().into_iter().find(|f| f.1 == want) {
                        None => say(&fill(tr(th, "not_found"), &[("name", want)])),
                        Some((_, name, false)) => say(&fill(tr(th, "is_disabled"), &[("name", name)])),
                        Some((file, name, true)) => {
                            with(st, &server, |eng, w| eng.fire_script(w, &name, EventKind::Unload));
                            let (lines, _) = load_file(st, &file, &name, &Registry::default());
                            refresh(st);
                            with(st, &server, |eng, w| eng.fire_script(w, &name, EventKind::Load));
                            lines.iter().for_each(|l| say(l));
                        }
                    }
                }
                for name in register_new(st) {
                    say(&fill(tr(th, "new_command"), &[("name", name)]));
                }
                hook_move(st);
                save(st, false);
            }
            Some(action @ ("enable" | "disable")) => {
                let enable = action == "enable";
                let found = cfg::script_path(&name_arg).and_then(|want| script_files().into_iter().find(|f| f.1 == want));
                match found {
                    None => say(&fill(tr(th, "not_found"), &[("name", name_arg.chars().take(40).collect())])),
                    Some((_, name, enabled)) if enabled == enable => say(&fill(tr(th, if enable { "already_enabled" } else { "already_disabled" }), &[("name", name)])),
                    Some((file, name, _)) => {
                        // only the file's own name changes; a folder that is switched off stays so
                        let (folder, base) = file.rsplit_once('/').map_or(("", file.as_str()), |(f, b)| (f, b));
                        let base = if enable { base.trim_start_matches('-').to_owned() } else { format!("-{base}") };
                        let to = if folder.is_empty() { base } else { format!("{folder}/{base}") };
                        if to == file {
                            say(&fill(tr(th, "rename_failed"), &[("name", file)]));
                            return Ok(0);
                        }
                        let (from_path, to_path) = (c::data_path(&format!("scripts/{file}")), c::data_path(&format!("scripts/{to}")));
                        if to_path.exists() || std::fs::rename(&from_path, &to_path).is_err() {
                            say(&fill(tr(th, "rename_failed"), &[("name", file)]));
                            return Ok(0);
                        }
                        if enable {
                            let (lines, _) = load_file(st, &to, &name, &Registry::default());
                            refresh(st);
                            with(st, &server, |eng, w| eng.fire_script(w, &name, EventKind::Load));
                            lines.iter().for_each(|l| say(l));
                            for name in register_new(st) {
                                say(&fill(tr(th, "new_command"), &[("name", name)]));
                            }
                            hook_move(st);
                        } else {
                            unload(st, Some(&server), &name);
                        }
                        say(&fill(tr(th, if enable { "enabled" } else { "disabled" }), &[("name", name)]));
                    }
                }
            }
            Some("list") => {
                let files = script_files();
                if files.is_empty() {
                    say(tr(th, "list_none"));
                }
                say(&fill(tr(th, "list_head"), &[("count", files.len().to_string())]));
                for (file, name, enabled) in files {
                    match st.eng.scripts.get(&name).filter(|_| enabled) {
                        Some(l) => {
                            let errors = if l.errors.is_empty() { String::new() } else { fill(tr(th, "list_errors"), &[("errors", l.errors.len().to_string())]) };
                            let vars = [("name", file), ("commands", l.commands.len().to_string()), ("triggers", l.triggers.len().to_string()), ("functions", l.functions.len().to_string()), ("errors", errors)];
                            say(&fill(tr(th, "list_on"), &vars));
                        }
                        None => say(&fill(tr(th, "list_off"), &[("name", file)])),
                    }
                }
            }
            Some("info") => {
                let (mut commands, mut triggers) = (0, 0);
                for l in st.eng.scripts.values() {
                    commands += l.commands.len();
                    triggers += l.triggers.len();
                }
                let vars = [
                    ("scripts", st.eng.scripts.len().to_string()),
                    ("commands", commands.to_string()),
                    ("triggers", triggers.to_string()),
                    ("variables", st.eng.vars.len().to_string()),
                    ("waits", st.eng.waiting().to_string()),
                ];
                say(&fill(tr(th, "info"), &vars));
                if !st.eng.runtime_errors.is_empty() {
                    say(tr(th, "info_errors"));
                    for e in st.eng.runtime_errors.iter().rev().take(5) {
                        say(&format!("&7{e}"));
                    }
                }
            }
            Some("version") => say(&fill(tr(th, "version"), &[("version", VERSION.to_owned())])),
            _ => say(tr(th, "help")),
        }
        drain(st, &server);
        Ok(1)
    }
}

// ---- plugin ----------------------------------------------------------------------------------

struct Scripts;

impl Plugin for Scripts {
    fn new() -> Self {
        Scripts
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Scripting by MCSV (Skript-style language)".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let cfg = c::load_config::<Config>("config.toml").cleaned();
        // first run: the scripts folder with examples that are switched off
        let scripts = c::data_path("scripts");
        if !scripts.exists() && std::fs::create_dir_all(&scripts).is_ok() {
            for (name, text) in cfg::EXAMPLES {
                let _ = std::fs::write(scripts.join(name), text);
            }
        }
        let mut eng = Engine::new(cfg.limits(), cfg.messages());
        if let Some(vars) = c::load_json::<serde_json::Value>("variables.json").and_then(VarStore::from_json) {
            eng.vars = vars;
        } else if c::data_path("variables.json").exists() {
            // never overwrite a file we could not read
            let _ = std::fs::rename(c::data_path("variables.json"), c::data_path(&format!("variables.broken-{}.json", now() as u64)));
            c::log_info("variables.json could not be read; it was kept under another name and variables start empty");
        }
        let seen = c::load_json::<HashMap<String, String>>("seen.json").unwrap_or_default();
        let mut st = State {
            cfg,
            eng,
            seen,
            seen_dirty: false,
            registered: HashSet::new(),
            labels: HashSet::new(),
            rng: (now() * 1e6) as u64 | 1,
            ticks: 0,
            loaded_fired: false,
            move_hooked: false,
            last_block: HashMap::new(),
            levels: HashMap::new(),
            tp_guard: HashMap::new(),
            tp_queue: Vec::new(),
            log_bytes: 0,
            money_noticed: false,
            menus: HashMap::new(),
            closed: HashSet::new(),
            pending_join: Vec::new(),
            hits: HashMap::new(),
            last_hit: HashMap::new(),
        };

        for (node, default) in [("admin", PermissionDefault::Op(PermissionLevel::Three)), ("script", PermissionDefault::Allow)] {
            let _ = context.register_permission(&Permission { node: format!("{NS}:{node}"), description: "Scripts".into(), default, children: vec![] });
        }
        // No `.suggest()`: Pumpkin 0.2.2 panics when a console that is a TTY asks a WASM plugin for suggestions
        let admin = Command::new(&["sk".to_owned(), "skript".to_owned(), NS.to_owned()], "Scripts").execute(Admin).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Admin));
        context.register_command(admin, &format!("{NS}:admin"));

        // Blocking handlers run first and may cancel or change the event; `EventPriority` is ignored by Pumpkin 0.2.2
        let p = EventPriority::Normal;
        context.register_event_handler(OnJoin, p, true)?;
        context.register_event_handler(OnLeave, p, true)?;
        context.register_event_handler(OnChat, p, true)?;
        context.register_event_handler(OnCommand, p, true)?;
        context.register_event_handler(OnConsoleCommand, p, true)?;
        context.register_event_handler(OnDeath, p, true)?;
        context.register_event_handler(OnRespawn, p, true)?;
        context.register_event_handler(OnBreak, p, true)?;
        context.register_event_handler(OnPlace, p, true)?;
        context.register_event_handler(OnInteract, p, true)?;
        context.register_event_handler(OnDamage, p, true)?;
        context.register_event_handler(OnDamageBy, p, true)?;
        context.register_event_handler(OnEntityDeath, p, true)?;
        context.register_event_handler(OnInvClick, p, true)?;
        context.register_event_handler(OnInvClose, p, true)?;
        context.register_event_handler(OnHeld, p, true)?;
        context.register_event_handler(OnDrop, p, true)?;
        context.register_event_handler(OnPickup, p, true)?;
        context.register_event_handler(OnConsume, p, true)?;
        context.register_event_handler(OnSneak, p, true)?;
        context.register_event_handler(OnSprint, p, true)?;
        context.register_event_handler(OnFlight, p, true)?;
        context.register_event_handler(OnWorldChange, p, true)?;
        context.register_event_handler(OnGamemode, p, true)?;
        context.register_event_handler(OnBedEnter, p, true)?;
        context.register_event_handler(OnBedLeave, p, true)?;
        context.register_event_handler(OnTeleport, p, true)?;
        context.register_event_handler(OnWeather, p, true)?;

        let (lines, errors, files) = load_all(&mut st, None);
        for line in &lines {
            c::log_info(&c::plain(line));
        }
        c::log_info(&c::plain(&fill(tr(st.cfg.th(), "reload_all"), &[("files", files.to_string()), ("errors", errors.to_string())])));

        // The context is kept: commands and the move hook are added later, when scripts are reloaded
        *CONTEXT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(context);
        register_new(&mut st);
        hook_move(&mut st);
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(st);

        // The panel reads this to tell "loaded" from "installed, waiting for a restart"
        c::save_json("status.json", &serde_json::json!({ "version": VERSION, "loaded_at": now() as u64 }));
        pumpkin_plugin_api::scheduler::schedule_repeating_task(1, 1, on_tick);
        Ok(())
    }

    fn on_unload(&self, context: Context) -> Result<(), String> {
        if let Some(mut guard) = try_lock() {
            if let Some(st) = guard.as_mut() {
                let server = context.get_server();
                let names: Vec<String> = st.eng.scripts.keys().cloned().collect();
                for name in names {
                    // also drops the triggers that were waiting
                    with(st, &server, |eng, w| eng.unload(w, &name));
                }
                save(st, true);
            }
        }
        CONTEXT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        c::remove_file("status.json");
        Ok(())
    }
}

register_plugin!(Scripts);

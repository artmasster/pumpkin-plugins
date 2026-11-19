//! Pumpkin WASM glue for Models: reading the owner's files, putting models into the world,
//! moving their bones, and getting the resource pack to players.
//!
//! A model in the world is one item display per bone that draws something. The displays are
//! real entities of the server: it tells the players near them, and only what changed. A model
//! either stands at a place, or sits on a creature (its bones ride it, so the game moves them
//! along for nothing).
//!
//! Moving a bone is one call that sets where its item model goes; the game glides there over
//! the ticks until the next pose. Poses are worked out from tables made when the model loaded,
//! and a bone that did not move is not sent again.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mcsv_common as c;
use mcsv_pack::Entry;
use mcsv_web::{Request, Response, Web};
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::data_components::DataComponent;
use pumpkin_plugin_api::events::{EntityDamageEvent, EntityDeathEvent, EventHandler, EventPriority, FromIntoEvent, PlayerChangeWorldEvent, PlayerInteractEntityEvent, PlayerLeaveEvent, PlayerResourcePackStatusEvent, PlayerRespawnEvent};
use pumpkin_plugin_api::java_packets::{CRemoveEntities, CSetEntityLink, CSetEntityMetadata, CSetPassengers, CSpawnEntity, ClientboundPacket};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA, NETWORK_TCP_BIND};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::uuid::Uuid;
use pumpkin_plugin_api::wit::pumpkin::plugin::common::{NbtTag, NbtTree};
use pumpkin_plugin_api::wit::pumpkin::plugin::display::{BillboardMode, DisplayEntity, DisplayTransformation, ItemDisplayEntity, Quaternionf, Vector3f};
use pumpkin_plugin_api::wit::pumpkin::plugin::player::JavaResourcePack;
use pumpkin_plugin_api::wit::pumpkin::plugin::server::CommandSender as ServerSender;
use pumpkin_plugin_api::wit::pumpkin::plugin::world::{Entity, World};
use pumpkin_plugin_api::{Context, EntityDisplayExt, EntityType, ItemDisplayMode, ItemStack, JavaMinecraftVersion, JavaPlayer, Plugin, PluginMetadata, Server, register_plugin};

use crate::bbmodel::{self, Loop};
use crate::defs::{self, Config, NS, Note, Placed, Settings};
use crate::math::Mat;
use crate::pack;
use crate::rig::{self, Kind, Layer, Model, Shown};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&dModels&8]&r ";
const PLACED_FILE: &str = "placed.json";
/// What a tick may spend on pack downloads.
const WEB_BUDGET: Duration = Duration::from_millis(2);
/// Ticks between two looks for bones a crash left in the world.
const SWEEP_EVERY: u64 = 6000;
/// A model file larger than this is not read.
const MAX_FILE: u64 = 32 * 1024 * 1024;
/// The tag on every entity this plugin makes.
const PART: &str = "part";
/// Models in the world at once, however they got there.
const MAX_MODELS: usize = 2000;
/// A player standing is this high: what rides one sits on top of it.
const PLAYER_HEIGHT: f32 = 1.8;
/// The number a lead's creature has (plus the model's number) until its marker is made (see
/// `Leash`): far above any the server gives its own entities. No game is told of it.
const LEASH_BASE: i32 = 0x5A00_0000;
/// A silverfish, in the game this is built for: the smallest creature, so the lead ends where
/// the model is. Only players on that game are told (see `Leash`).
const LEASH_CREATURE: i32 = 117;
const LEASH_GAME: JavaMinecraftVersion = JavaMinecraftVersion::V263;

struct Built {
    bytes: Arc<Vec<u8>>,
    sha1: String,
    files: usize,
    took_ms: u128,
}

#[derive(Default)]
struct Seen {
    sent: String,
    due: u64,
    answer: String,
}

/// A model with everything worked out that every one of its copies in the world shares.
struct Data {
    model: Model,
    set: Settings,
    /// The item model of each bone, for the bones that draw something.
    items: Vec<Option<String>>,
    idle: Option<usize>,
    walk: Option<usize>,
    hurt: Option<usize>,
    death: Option<usize>,
    spawn: Option<usize>,
    head: bool,
    tag: Option<usize>,
    /// How the item displays draw their item: `Fixed` for a model's bones.
    mode: ItemDisplayMode,
    /// Made for one item another plugin named (`attach` with `item`), not read from a file.
    from_item: bool,
    /// The item model of the whole model worn in a helmet slot, when it can be one.
    worn: Option<String>,
}

/// A model of one item for `attach` or `spawn` with `item`: made when asked, never from a
/// file. On a creature, measured on a player: the neck is 24 units up, the middle of the head
/// 28, the middle of the back 18. Standing by itself: the item's middle is where it stands.
/// `offset` is in blocks (+Z is behind).
fn item_data(item: &str, on_creature: bool, head: bool, offset: (f32, f32, f32), size: f32, turn: f32, mode: ItemDisplayMode) -> Data {
    use crate::math::Vec3;
    let middle = match (on_creature, head) {
        (true, true) => Vec3::new(0.0, 28.0, 0.0),
        (true, false) => Vec3::new(0.0, 18.0, 0.0),
        (false, _) => Vec3::ZERO,
    };
    let at = middle.add(Vec3::new(offset.0, offset.1, offset.2).scale(rig::UNITS));
    let model = rig::single(&format!("item:{item}"), head, Vec3::new(0.0, 24.0, 0.0), at, size, turn);
    let set = Settings { hide_base: false, ..Settings::default() }.cleaned();
    Data { model, set, items: vec![Some(item.to_owned())], idle: None, walk: None, hurt: None, death: None, spawn: None, head, tag: None, mode, from_item: true, worn: None }
}

fn display_mode(name: &str) -> Option<ItemDisplayMode> {
    Some(match name {
        "head" => ItemDisplayMode::Head,
        "fixed" => ItemDisplayMode::Fixed,
        "none" => ItemDisplayMode::None,
        "ground" => ItemDisplayMode::Ground,
        "gui" => ItemDisplayMode::Gui,
        "hand" => ItemDisplayMode::ThirdpersonRighthand,
        _ => return None,
    })
}

struct Part {
    bone: usize,
    entity: Entity,
    display: DisplayEntity,
    item: ItemDisplayEntity,
    last: Option<Shown>,
}

/// An animation that is running on one model.
struct Run {
    animation: usize,
    time: f32,
    speed: f32,
    mode: Loop,
    weight: f32,
    /// Where the weight is going: 1 while it plays, 0 while it leaves.
    target: f32,
    /// How much the weight changes per tick.
    fade: f32,
    /// Started by the plugin (idle, walk), not asked for.
    auto: bool,
    /// Which asked-for start on this model it is: a watcher sees a start over by it.
    nth: u64,
}

enum Anchor {
    Placed { world: String, pos: (f64, f64, f64) },
    /// `up`: how far above the creature's feet the model's own feet are.
    Riding { base: Entity, base_id: i32, player: bool, lift: f32, up: f32, hid: bool, lived: i32, stale: u8 },
}

/// A lead from a player's hand to a model that stands by itself (a balloon). The game draws a
/// lead only to a creature, so every player who sees the model is told of one that is in their
/// game alone: invisible, riding the model's first bone, held by the player. The server never
/// has it, so nothing in the world can hit, push or untie it.
///
/// The game drops a creature's lead whenever the creature starts to ride: whatever tells it
/// what the creature rides tells it who holds the lead right after.
///
/// A player who hits the creature sends its number to the server, and Pumpkin kicks a player
/// who hits a number it does not know. So the creature has the number of a marker the server
/// does have: a marker is never sent to any game, so nothing else in the games has that number.
struct Leash {
    /// The player who holds it: UUID as 32 hex digits.
    holder: String,
    /// The creature's number in the games told of it: its marker's, once that is made.
    fake: i32,
    marker: Option<Entity>,
    /// Who was told, by lowercase name.
    told: HashMap<String, Told>,
}

struct Told {
    java: JavaPlayer,
    /// The bone entity the creature rides there, and the holder's entity.
    ride: i32,
    holder: i32,
    /// The player's game lost it (a respawn, another world): told all again from this tick.
    again: Option<u64>,
}

impl Leash {
    fn packets(&self, at: (f64, f64, f64), ride: i32, holder: i32) -> [ClientboundPacket; 4] {
        let fake = self.fake;
        [
            ClientboundPacket::CSpawnEntity(CSpawnEntity { entity_id: fake, entity_uuid: Uuid { high: 0x4d43_5356_4c45_4153, low: fake as u64 }, r_type: LEASH_CREATURE, position: at, velocity: "[0.0,0.0,0.0]".into(), pitch: 0, yaw: 0, head_yaw: 0, data: 0 }),
            // Entity flags (index 0, a byte): invisible
            ClientboundPacket::CSetEntityMetadata(CSetEntityMetadata { entity_id: fake, metadata: vec![0, 0, 0x20, 0xff] }),
            ClientboundPacket::CSetPassengers(CSetPassengers { entity_id: ride, passengers: vec![fake] }),
            ClientboundPacket::CSetEntityLink(CSetEntityLink { attached_entity_id: fake, holding_entity_id: holder, leash: true }),
        ]
    }

    /// Take the creature out of the game of everyone who was told.
    fn drop_all(&mut self) {
        let gone = ClientboundPacket::CRemoveEntities(CRemoveEntities { entity_ids: vec![self.fake] });
        for (_, told) in self.told.drain() {
            told.java.send_packet(&gone);
        }
    }
}

/// A player's body as the game turns it (see `rig::body_turn`), worked out once a tick.
struct Body {
    yaw: f32,
    at: (f64, f64, f64),
    tick: u64,
}

struct Instance {
    id: u32,
    data: Arc<Data>,
    anchor: Anchor,
    yaw: f32,
    scale: f32,
    /// The plugin that asked for it (empty: a command did).
    owner: String,
    parts: Vec<Part>,
    hit: Option<(Entity, i32)>,
    tag: Option<Entity>,
    tag_text: String,
    runs: Vec<Run>,
    interval: u32,
    due: u64,
    last_tick: u64,
    look: (f32, f32),
    last_pos: (f64, f64, f64),
    moving: bool,
    tint: Option<i32>,
    tint_until: u64,
    /// Shown instead of the model's idle; and what a click runs.
    animation: Option<String>,
    commands: Vec<String>,
    /// From `placed.json`: it comes back after a restart.
    kept: bool,
    /// Rides next to the creature's own model (see `add_riding`).
    extra: bool,
    leash: Option<Leash>,
    die_at: Option<u64>,
    /// A player is close enough to see it.
    near: bool,
    far: bool,
    /// What the last pose was made from, as one number.
    seen: Option<u64>,
    /// What a watcher was last told about it, as one number, and when that last changed.
    told: u64,
    rev: u64,
    /// How many animations were asked for so far.
    plays: u64,
}

/// A model another plugin asked to put on a creature, until the next tick finds the creature.
struct Pending {
    id: u32,
    data: Arc<Data>,
    entity: u32,
    owner: String,
    scale: Option<f32>,
    name: String,
    /// What was asked of this model in the meantime.
    then: Vec<serde_json::Value>,
    /// Goes next to the creature's other models instead of taking the place of its model.
    extra: bool,
    /// Hide the creature, whatever the model's settings say.
    hide: Option<bool>,
}

#[derive(Default)]
struct Stat {
    ticks: u64,
    us: u64,
    moved: u64,
    skipped: u64,
    late: u64,
}

struct State {
    cfg: Config,
    models: Vec<Arc<Data>>,
    notes: Vec<Note>,
    pack: Option<Built>,
    web: Option<Web>,
    web_error: Option<String>,
    players: HashMap<String, Seen>,
    instances: Vec<Instance>,
    pending: Vec<Pending>,
    /// Entries of `placed.json` whose model is not there now.
    waiting: Vec<Placed>,
    next_id: u32,
    /// Which model a clickable box belongs to (entity id of the box).
    boxes: HashMap<i32, u32>,
    /// Which model sits on a creature (entity id of the creature).
    bases: HashMap<i32, u32>,
    /// The bodies of players something rides, by entity id.
    bodies: HashMap<i32, Body>,
    tick: u64,
    sent: u64,
    stat: Stat,
    scratch: Vec<Mat>,
    turn: usize,
    swept: u64,
    watch: Watch,
}

/// What another plugin that mirrors the models somewhere else (the Bedrock plugin does, for
/// players the bones cannot be shown to) is told: it asks every few ticks for what changed.
#[derive(Default)]
struct Watch {
    /// Somebody asked not long ago: the tick until which changes are noted.
    until: u64,
    /// Counts up with every change.
    rev: u64,
    /// Models that left the world, with the count at that moment.
    gone: Vec<(u64, u32)>,
    /// Older leavings than this were forgotten: who asks from before it is told everything.
    floor: u64,
    /// Counts the times the files were read: the models may be other ones after that.
    loaded: u64,
}

/// How many leavings are remembered for a watcher that asks late.
const WATCH_GONE: usize = 4096;

static STATE: Mutex<Option<State>> = Mutex::new(None);

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

fn names_in(rel: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(c::data_path(rel)).map(|dir| dir.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file())).filter_map(|e| e.file_name().into_string().ok()).collect()).unwrap_or_default();
    names.sort();
    names
}

struct Loaded {
    cfg: Config,
    models: Vec<Arc<Data>>,
    notes: Vec<Note>,
    pack: Option<Built>,
}

fn load() -> Loaded {
    let cfg = c::load_config::<Config>("config.toml").cleaned();
    let mut notes = Vec::new();
    let first = !c::data_path("models").exists();
    let _ = std::fs::create_dir_all(c::data_path("models"));
    if first {
        // Something to look at before the owner's own models are there: /models place example
        let _ = std::fs::write(c::data_path("models/example.bbmodel"), defs::EXAMPLE_MODEL);
        let _ = std::fs::write(c::data_path("models/example.toml"), defs::EXAMPLE_SETTINGS);
    }

    let mut models: Vec<Arc<Data>> = Vec::new();
    for file in names_in("models") {
        let Some(stem) = file.strip_suffix(".bbmodel") else { continue };
        let place = format!("models/{file}");
        let path = c::data_path(&place);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_FILE) {
            notes.push(Note::new(&place, "ไฟล์ใหญ่เกินไป", "the file is too large"));
            continue;
        }
        let Some(text) = std::fs::read(&path).ok().and_then(|b| String::from_utf8(b).ok()) else {
            notes.push(Note::new(&place, "อ่านไฟล์ไม่ได้", "cannot be read"));
            continue;
        };
        let model = match bbmodel::parse(&text).and_then(|raw| rig::build(stem, &raw)) {
            Ok(model) => model,
            Err(why) => {
                notes.push(Note::plain(&place, &why));
                continue;
            }
        };
        if models.iter().any(|m| m.model.name == model.name) {
            notes.push(Note::new(&place, format!("มีโมเดลชื่อ {} อยู่แล้ว ไฟล์นี้ถูกข้าม", model.name), format!("there already is a model named {}; this file is skipped", model.name)));
            continue;
        }
        for note in &model.notes {
            notes.push(Note::plain(&place, note));
        }
        let set = match std::fs::read_to_string(c::data_path(&format!("models/{stem}.toml"))) {
            Ok(text) => toml::from_str::<Settings>(&text).unwrap_or_else(|e| {
                notes.push(Note::plain(&format!("models/{stem}.toml"), &e.to_string().lines().next().unwrap_or_default().to_owned()));
                Settings::default()
            }),
            Err(_) => Settings::default(),
        }
        .cleaned();
        let items = model.bones.iter().map(|b| (!b.cubes.is_empty()).then(|| pack::item_id(&model.name, &b.name))).collect();
        let named = |name: &str| model.animation(&rig::plain_name(name));
        let worn = pack::worn_model(&model).map(|_| pack::worn_id(&model.name));
        models.push(Arc::new(Data {
            idle: named(&set.idle),
            walk: named(&set.walk),
            hurt: named(&set.hurt),
            death: named(&set.death),
            spawn: named(&set.spawn),
            head: model.has(Kind::Head),
            tag: model.bones.iter().position(|b| b.kind == Kind::Tag),
            items,
            set,
            model,
            mode: ItemDisplayMode::Fixed,
            from_item: false,
            worn,
        }));
    }

    let started = Instant::now();
    let list: Vec<&Model> = models.iter().map(|d| &d.model).collect();
    let made: Vec<Entry> = pack::generate(&list, &mut notes);
    let built = match pack::assemble(&cfg.pack, made) {
        Ok(files) => {
            let bytes = mcsv_pack::zip(&files);
            let mut sha = c::sha1::Sha1::new();
            sha.update(&bytes);
            let sha1 = c::sha1::hex(&sha.finish());
            if std::fs::write(c::data_path("pack.zip"), &bytes).is_err() {
                notes.push(Note::new("pack.zip", "เขียนไฟล์ไม่ได้", "cannot be written"));
            }
            Some(Built { bytes: Arc::new(bytes), sha1, files: files.len(), took_ms: started.elapsed().as_millis() })
        }
        Err(note) => {
            notes.push(note);
            None
        }
    };
    Loaded { cfg, models, notes, pack: built }
}

/// Open or close the download port to match the settings.
fn listen(st: &mut State) {
    let want = st.cfg.pack.port;
    if st.web.as_ref().map_or(0, |w| w.port) == want {
        return;
    }
    st.web = None;
    st.web_error = None;
    if want != 0 {
        match Web::bind(want) {
            Ok(web) => st.web = Some(web),
            Err(e) => st.web_error = Some(e.to_string()),
        }
    }
}

fn find_model(st: &State, name: &str) -> Option<Arc<Data>> {
    let plain = rig::plain_name(name);
    st.models.iter().find(|d| d.model.name == plain).cloned()
}

fn placed_list(st: &State) -> Vec<Placed> {
    st.instances
        .iter()
        .filter(|i| i.kept)
        .filter_map(|i| match &i.anchor {
            Anchor::Placed { world, pos } => Some(Placed { id: i.id, model: i.data.model.name.clone(), world: world.clone(), x: pos.0, y: pos.1, z: pos.2, yaw: i.yaw, scale: i.scale, animation: i.animation.clone(), commands: i.commands.clone(), name: i.tag_text.clone() }),
            Anchor::Riding { .. } => None,
        })
        // The ones whose model file is gone for now stay on the list: the file may come back
        .chain(st.waiting.iter().cloned())
        .collect()
}

fn save_placed(st: &State) {
    c::save_json(PLACED_FILE, &placed_list(st));
}

// ---- entities -------------------------------------------------------------------

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

/// The item a bone's display shows: any item that looks like the bone's model.
fn bone_item(model: &str, tint: Option<i32>) -> ItemStack {
    let stack = ItemStack::new("minecraft:paper", 1);
    let mut name = varint(model.len() as u32);
    name.extend_from_slice(model.as_bytes());
    stack.set_component(DataComponent::ItemModel, &name);
    if let Some(rgb) = tint {
        stack.set_component(DataComponent::DyedColor, &rgb.to_be_bytes());
    }
    stack
}

/// What this run of the server writes on its entities: the time it started. An entity that
/// carries another value is from an earlier run, and nothing owns it any more.
static RUN: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn run_mark() -> &'static str {
    RUN.get_or_init(|| c::now_secs().to_string())
}

fn mark(entity: &Entity) {
    entity.set_custom_data(NS, PART, &NbtTree { root: 0, tags: vec![NbtTag::StringTag(run_mark().to_owned())] });
}

/// One of our entities that an earlier run left in the world. A display's item is not saved,
/// so such an entity shows nothing; it is only in the way.
fn leftover(entity: &Entity) -> bool {
    match entity.get_custom_data(NS, PART) {
        Some(tree) => !matches!(tree.tags.get(tree.root as usize), Some(NbtTag::StringTag(s)) if s == run_mark()),
        None => false,
    }
}

fn transformation(s: &Shown) -> DisplayTransformation {
    DisplayTransformation {
        translation: Vector3f { x: s.translation.x, y: s.translation.y, z: s.translation.z },
        scale: Vector3f { x: s.scale.x, y: s.scale.y, z: s.scale.z },
        left_rotation: Quaternionf { x: s.rotation.x, y: s.rotation.y, z: s.rotation.z, w: s.rotation.w },
        right_rotation: Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
    }
}

fn world_named(server: &Server, name: &str) -> Option<World> {
    server.get_all_worlds().into_iter().find(|w| w.get_dimension() == name)
}

impl Instance {
    fn new(id: u32, data: Arc<Data>, anchor: Anchor, yaw: f32, owner: &str) -> Instance {
        let scale = data.set.scale;
        Instance {
            id,
            data,
            anchor,
            yaw,
            scale,
            owner: owner.to_owned(),
            parts: Vec::new(),
            hit: None,
            tag: None,
            tag_text: String::new(),
            runs: Vec::new(),
            interval: 0,
            due: 0,
            last_tick: 0,
            look: (0.0, 0.0),
            last_pos: (0.0, 0.0, 0.0),
            moving: false,
            tint: None,
            tint_until: 0,
            animation: None,
            commands: Vec::new(),
            kept: false,
            extra: false,
            leash: None,
            die_at: None,
            near: false,
            far: false,
            seen: None,
            told: 0,
            rev: 0,
            plays: 0,
        }
    }

    /// Everything a watcher is told about this model, as one number: when it changes the
    /// watcher hears of the model again.
    fn telling(&self) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let mut add = |n: u64| h = (h ^ n).wrapping_mul(0x0000_0100_0000_01b3);
        add(u64::from(self.scale.to_bits()));
        if let Anchor::Placed { pos, .. } = &self.anchor {
            add(pos.0.to_bits());
            add(pos.1.to_bits());
            add(pos.2.to_bits());
            add(u64::from(self.yaw.to_bits()));
        }
        for byte in self.tag_text.bytes() {
            add(u64::from(byte));
        }
        add(u64::from(self.tint.is_some()) + 2 * u64::from(self.die_at.is_some()));
        for run in self.runs.iter().filter(|r| r.target > 0.0) {
            add(run.animation as u64 * 4 + u64::from(run.auto) + 1);
            add(run.nth);
        }
        h | 1
    }

    fn shown(&self) -> bool {
        !self.parts.is_empty()
    }

    fn lift(&self) -> f32 {
        match &self.anchor {
            Anchor::Placed { .. } => self.data.set.lift,
            Anchor::Riding { lift, .. } => *lift,
        }
    }

    fn on_player(&self) -> bool {
        matches!(self.anchor, Anchor::Riding { player: true, .. })
    }

    fn root(&self) -> Mat {
        // A hat or a backpack is drawn around a model of a player: as small as the game draws one
        let grow = if self.extra && self.on_player() { rig::PLAYER_SIZE } else { 1.0 };
        rig::root(self.yaw, self.scale * grow, self.lift())
    }

    /// Follows a player: told every tick, so it keeps up with every turn.
    fn quick(&self) -> bool {
        self.leash.is_some() || self.on_player()
    }

    /// Where it is, and in which world.
    fn place(&self) -> ((f64, f64, f64), Option<World>, Option<String>) {
        match &self.anchor {
            Anchor::Placed { world, pos } => (*pos, None, Some(world.clone())),
            Anchor::Riding { base, .. } => (base.get_position(), Some(base.get_world()), None),
        }
    }

    /// Take every entity of this model out of the world.
    fn hide(&mut self, boxes: &mut HashMap<i32, u32>) {
        self.seen = None;
        if let Some(leash) = self.leash.as_mut() {
            leash.drop_all();
        }
        for part in self.parts.drain(..) {
            part.entity.remove();
        }
        if let Some((entity, id)) = self.hit.take() {
            boxes.remove(&id);
            entity.remove();
        }
        if let Some(entity) = self.tag.take() {
            entity.remove();
        }
    }

    /// Start an animation. One that is already running starts over.
    fn play(&mut self, animation: usize, mode: Option<Loop>, speed: f32, blend: u32, auto: bool) {
        let Some(a) = self.data.model.animations.get(animation) else { return };
        let fade = if blend == 0 { 1.0 } else { 1.0 / blend as f32 };
        if !auto {
            self.plays += 1;
        }
        let nth = if auto { 0 } else { self.plays };
        if let Some(run) = self.runs.iter_mut().find(|r| r.animation == animation) {
            run.time = if auto { run.time } else { 0.0 };
            run.target = 1.0;
            run.fade = fade;
            run.speed = speed;
            run.auto = auto;
            run.mode = mode.unwrap_or(a.looping);
            run.nth = nth;
            return;
        }
        self.runs.push(Run { animation, time: 0.0, speed, mode: mode.unwrap_or(a.looping), weight: if blend == 0 { 1.0 } else { 0.0 }, target: 1.0, fade, auto, nth });
    }

    fn stop(&mut self, animation: Option<usize>, blend: u32) {
        let fade = if blend == 0 { 1.0 } else { 1.0 / blend as f32 };
        for run in self.runs.iter_mut().filter(|r| animation.is_none_or(|a| a == r.animation)) {
            run.target = 0.0;
            run.fade = fade;
        }
    }

    /// The animation that plays when nothing else was asked for.
    fn resting(&self) -> Option<usize> {
        if self.moving && self.data.walk.is_some() {
            return self.data.walk;
        }
        self.animation.as_deref().and_then(|name| self.data.model.animation(&rig::plain_name(name))).or(self.data.idle)
    }

    /// Move every running animation on by `dt` ticks; start and end the ones the plugin runs
    /// by itself.
    fn advance(&mut self, dt: f32) {
        let blend = self.data.set.blend;
        let fade = if blend == 0 { 1.0 } else { 1.0 / blend as f32 };
        let want = if self.die_at.is_some() { None } else { self.resting() };
        for run in self.runs.iter_mut().filter(|r| r.auto) {
            if Some(run.animation) != want {
                run.target = 0.0;
                run.fade = fade;
            }
        }
        if let Some(want) = want
            && !self.runs.iter().any(|r| r.animation == want && r.target > 0.0)
        {
            self.play(want, Some(Loop::Loop), 1.0, blend, true);
        }
        let model = &self.data.model;
        for run in &mut self.runs {
            let ticks = model.animations[run.animation].ticks as f32;
            run.time += dt * run.speed;
            if run.time >= ticks {
                match run.mode {
                    Loop::Loop => run.time %= ticks,
                    Loop::Hold => run.time = ticks,
                    Loop::Once => {
                        run.time = ticks;
                        run.target = 0.0;
                    }
                }
            }
            let step = run.fade * dt;
            run.weight = if run.target > run.weight { (run.weight + step).min(run.target) } else { (run.weight - step).max(run.target) };
        }
        self.runs.retain(|r| r.target > 0.0 || r.weight > 0.0);
    }
}

/// Put the model's entities into the world.
fn show(inst: &mut Instance, server: &Server, interval: u32, boxes: &mut HashMap<i32, u32>, scratch: &mut Vec<Mat>) -> bool {
    let (pos, world, name) = inst.place();
    let Some(world) = world.or_else(|| name.and_then(|n| world_named(server, &n))) else { return false };
    let data = inst.data.clone();
    let layers: Vec<Layer> = inst.runs.iter().map(|r| Layer { animation: &data.model.animations[r.animation], time: r.time, weight: r.weight }).collect();
    data.model.pose(&layers, data.head.then_some(inst.look), scratch);
    let lead = rig::lead(&inst.root());
    for bone in data.model.drawn() {
        let entity = world.spawn_entity(EntityType::ItemDisplay, pos);
        let Some(item) = entity.as_item_display() else {
            entity.remove();
            continue;
        };
        if inst.parts.is_empty() {
            // The chunk is loaded now: what an earlier run left at this very place goes
            for other in entity.get_nearby_entities(4.0, 8.0, 4.0) {
                if leftover(&other) {
                    other.remove();
                }
            }
        }
        mark(&entity);
        item.set_item(Some(bone_item(data.items[bone].as_deref().unwrap_or_default(), inst.tint)));
        item.set_item_display_mode(data.mode);
        let display = item.get_display();
        let shown = rig::shown(&data.model.tail[bone], &scratch[bone], &lead);
        display.set_transformation(transformation(&shown));
        display.set_interpolation_duration(interval as i32);
        display.set_teleport_duration(interval.min(59) as i32);
        if data.set.view_range != 1.0 {
            display.set_view_range(data.set.view_range);
        }
        if data.set.shadow > 0.0 {
            display.set_shadow_radius(data.set.shadow * inst.scale);
            display.set_shadow_strength(1.0);
        }
        if let Anchor::Riding { base, .. } = &inst.anchor {
            base.add_passenger(item.get_entity());
        }
        inst.parts.push(Part { bone, entity, display, item, last: Some(shown) });
    }
    inst.interval = interval;

    // What can be clicked: only a model that stands by itself needs a box of its own
    if matches!(inst.anchor, Anchor::Placed { .. })
        && let Some((w, h)) = data.model.hitbox.or(data.set.hitbox)
    {
        let entity = world.spawn_entity(EntityType::Interaction, pos);
        if let Some(click) = entity.as_interaction() {
            click.set_width(w * inst.scale);
            click.set_height(h * inst.scale);
            click.set_response(true);
            mark(&entity);
            let id = entity.get_id() as i32;
            boxes.insert(id, inst.id);
            inst.hit = Some((entity, id));
        } else {
            entity.remove();
        }
    }
    show_tag(inst, &world, pos, scratch);
    true
}

/// The name above a model, at its `tag_` bone.
fn show_tag(inst: &mut Instance, world: &World, pos: (f64, f64, f64), scratch: &[Mat]) {
    if let Some(old) = inst.tag.take() {
        old.remove();
    }
    let Some(bone) = inst.data.tag.filter(|_| !inst.tag_text.is_empty()) else { return };
    let entity = world.spawn_entity(EntityType::TextDisplay, pos);
    let Some(text) = entity.as_text_display() else {
        entity.remove();
        return;
    };
    mark(&entity);
    text.set_text(c::color(&inst.tag_text));
    let display = text.get_display();
    display.set_billboard(BillboardMode::Center);
    let at = rig::place(inst.data.model.bones[bone].pivot, &scratch[bone], &inst.root());
    display.set_transformation(DisplayTransformation {
        translation: Vector3f { x: at.x, y: at.y, z: at.z },
        scale: Vector3f { x: 1.0, y: 1.0, z: 1.0 },
        left_rotation: Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
        right_rotation: Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
    });
    if let Anchor::Riding { base, .. } = &inst.anchor {
        base.add_passenger(text.get_entity());
    }
    inst.tag = Some(entity);
}

/// Work out the pose and tell the bones that moved. Returns how many were told.
fn pose(inst: &mut Instance, scratch: &mut Vec<Mat>, stat: &mut Stat) -> u32 {
    let data = inst.data.clone();
    let layers: Vec<Layer> = inst.runs.iter().map(|r| Layer { animation: &data.model.animations[r.animation], time: r.time, weight: r.weight }).collect();
    // Nothing that goes into a pose changed since the last one: every bone is where it was
    let mut seen = [inst.yaw.to_bits(), inst.scale.to_bits(), inst.look.0.to_bits(), inst.look.1.to_bits(), layers.len() as u32].iter().fold(0xcbf2_9ce4_8422_2325u64, |h, v| (h ^ u64::from(*v)).wrapping_mul(0x0000_0100_0000_01b3));
    for layer in &layers {
        seen = [layer.animation.ticks, layer.time.to_bits(), layer.weight.to_bits()].iter().fold(seen ^ layer.animation.name.len() as u64, |h, v| (h ^ u64::from(*v)).wrapping_mul(0x0000_0100_0000_01b3));
    }
    if inst.seen == Some(seen) {
        stat.skipped += inst.parts.len() as u64;
        return 0;
    }
    inst.seen = Some(seen);
    data.model.pose(&layers, data.head.then_some(inst.look), scratch);
    let lead = rig::lead(&inst.root());
    let mut moved = 0;
    for part in &mut inst.parts {
        let mut shown = rig::shown(&data.model.tail[part.bone], &scratch[part.bone], &lead);
        if let Some(last) = &part.last {
            shown.rotation = shown.rotation.close_to(last.rotation);
            if shown.same(last) {
                stat.skipped += 1;
                continue;
            }
        }
        part.display.set_transformation(transformation(&shown));
        // The game starts gliding when it is told the start again. The same value is not
        // sent twice, a different one in between is.
        part.display.set_interpolation_start(1);
        part.display.set_interpolation_start(0);
        part.last = Some(shown);
        moved += 1;
    }
    stat.moved += u64::from(moved);
    moved
}

fn set_tint(inst: &mut Instance, tint: Option<i32>) {
    if inst.tint == tint {
        return;
    }
    inst.tint = tint;
    for part in &inst.parts {
        part.item.set_item(Some(bone_item(inst.data.items[part.bone].as_deref().unwrap_or_default(), tint)));
    }
}

/// Remove every entity with our tag that no model of ours owns: what a crash left behind.
fn sweep(server: &Server) -> usize {
    let mut removed = 0;
    for world in server.get_all_worlds() {
        for entity in world.get_entities() {
            if leftover(&entity) {
                entity.remove();
                removed += 1;
            }
        }
    }
    removed
}

// ---- making and removing models -------------------------------------------------

fn add_placed(st: &mut State, data: Arc<Data>, world: String, pos: (f64, f64, f64), yaw: f32, owner: &str) -> u32 {
    let id = st.next_id;
    st.next_id += 1;
    let mut inst = Instance::new(id, data, Anchor::Placed { world, pos }, yaw, owner);
    if let Some(spawn) = inst.data.spawn {
        inst.play(spawn, Some(Loop::Once), 1.0, 0, false);
    }
    st.instances.push(inst);
    id
}

/// Put the models other plugins asked for onto their creatures: one look through the worlds
/// for all of them.
fn settle_pending(st: &mut State, server: &Server, after: &mut Vec<After>) {
    let pending = std::mem::take(&mut st.pending);
    let mut wanted: Vec<u32> = pending.iter().map(|p| p.entity).collect();
    let mut found: HashMap<u32, Entity> = HashMap::new();
    'worlds: for world in server.get_all_worlds() {
        for entity in world.get_entities() {
            let id = entity.get_id();
            if let Some(at) = wanted.iter().position(|w| *w == id) {
                wanted.swap_remove(at);
                found.insert(id, entity);
                if wanted.is_empty() {
                    break 'worlds;
                }
            }
        }
    }
    for p in pending {
        let Some(base) = found.remove(&p.entity) else {
            after.push(After::Tell { plugin: p.owner, message: serde_json::json!({ "kind": "model_gone", "id": p.id, "why": "no such entity" }) });
            continue;
        };
        if !p.extra
            && let Some(old) = st.bases.get(&(p.entity as i32)).copied()
        {
            remove_instance(st, old);
        }
        add_riding(st, p.data, base, &p.owner, Some(p.id), p.extra, p.hide);
        if let Some(inst) = st.instances.iter_mut().find(|i| i.id == p.id) {
            if let Some(scale) = p.scale {
                inst.scale = scale;
            }
            inst.tag_text = p.name;
        }
        for request in p.then {
            let _ = ipc(st, &p.owner, &request);
        }
    }
}

/// `extra`: the model goes next to the creature's own model (a hat on a player) instead of
/// being its model; hurt, death and `detach` are about the creature's own model.
fn add_riding(st: &mut State, data: Arc<Data>, base: Entity, owner: &str, id: Option<u32>, extra: bool, hide: Option<bool>) -> u32 {
    let id = id.unwrap_or_else(|| {
        st.next_id += 1;
        st.next_id - 1
    });
    let base_id = base.get_id() as i32;
    // Riders sit at the top of a creature: the model's feet go back down to its feet. A player
    // may be sneaking just now; what rides goes down and up with them by itself.
    let player = base.get_type() == EntityType::Player;
    let height = if player { PLAYER_HEIGHT } else { base.get_height() };
    let lift = data.set.ride_lift.unwrap_or(-height) + data.set.lift;
    let up = lift + height;
    let hid = hide.unwrap_or(data.set.hide_base && !extra) && !base.is_invisible();
    if hid {
        base.set_invisible(true);
    }
    let yaw = base.get_yaw();
    let pos = base.get_position();
    let lived = base.get_ticks_lived();
    let mut inst = Instance::new(id, data, Anchor::Riding { base, base_id, player, lift, up, hid, lived, stale: 0 }, yaw, owner);
    inst.last_pos = pos;
    if let Some(spawn) = inst.data.spawn {
        inst.play(spawn, Some(Loop::Once), 1.0, 0, false);
    }
    inst.extra = extra;
    if !extra {
        st.bases.insert(base_id, id);
    }
    st.instances.push(inst);
    id
}

fn remove_instance(st: &mut State, id: u32) -> bool {
    let Some(at) = st.instances.iter().position(|i| i.id == id) else { return false };
    let mut inst = st.instances.swap_remove(at);
    if st.tick < st.watch.until {
        st.watch.rev += 1;
        st.watch.gone.push((st.watch.rev, id));
        if st.watch.gone.len() > WATCH_GONE {
            let cut = st.watch.gone.len() - WATCH_GONE / 2;
            st.watch.floor = st.watch.gone[cut - 1].0;
            st.watch.gone.drain(..cut);
        }
    }
    inst.hide(&mut st.boxes);
    if let Some(marker) = inst.leash.as_ref().and_then(|l| l.marker.as_ref()) {
        marker.remove();
    }
    if let Anchor::Riding { base, base_id, hid, .. } = &inst.anchor {
        if st.bases.get(base_id) == Some(&id) {
            st.bases.remove(base_id);
        }
        if *hid {
            base.set_invisible(false);
        }
    }
    true
}

/// A player's UUID as 32 hex digits, the way a lead names its holder.
fn uuid_hex(id: &Uuid) -> String {
    format!("{:016x}{:016x}", id.high, id.low)
}

/// Every few ticks: who sees a model on a lead is told of its creature, again when the bone
/// it rides or the holder are other entities now, and it leaves the games of who went away.
/// `resend`: tell everyone again what the creature rides (a game that had the bone taken
/// away and given back keeps the creature but not the ride).
fn tend_leashes(st: &mut State, server: &Server, resend: bool) {
    if !st.instances.iter().any(|i| i.leash.is_some()) {
        return;
    }
    let players = server.get_all_players();
    // name, world, where, UUID, entity
    let seen: Vec<(String, String, (f64, f64, f64), String, i32)> = players.iter().map(|p| (p.get_name().to_ascii_lowercase(), p.get_world().get_dimension(), p.get_position(), uuid_hex(&p.get_id()), p.as_entity().get_id() as i32)).collect();
    let (view, keep) = (f64::from(st.cfg.view_distance), f64::from(st.cfg.view_distance) + 8.0);
    let far = |a: (f64, f64, f64), b: (f64, f64, f64)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt();
    let tick = st.tick;
    for inst in &mut st.instances {
        let Some(leash) = inst.leash.as_mut() else { continue };
        let Anchor::Placed { world, pos } = &inst.anchor else { continue };
        if leash.marker.is_none() {
            let Some(w) = world_named(server, world) else { continue };
            let marker = w.spawn_entity(EntityType::Marker, *pos);
            mark(&marker);
            leash.fake = marker.get_id() as i32;
            leash.marker = Some(marker);
        }
        let ride = inst.parts.first().map(|p| p.entity.get_id() as i32);
        let holder = seen.iter().find(|s| s.3 == leash.holder && s.1 == *world).map(|s| s.4);
        let (Some(ride), Some(holder)) = (ride, holder) else {
            leash.drop_all();
            continue;
        };
        let gone = ClientboundPacket::CRemoveEntities(CRemoveEntities { entity_ids: vec![leash.fake] });
        leash.told.retain(|name, told| match seen.iter().find(|s| s.0 == *name) {
            Some(s) if s.1 == *world && far(s.2, *pos) <= keep => true,
            Some(_) => {
                told.java.send_packet(&gone);
                false
            }
            None => false,
        });
        for (i, s) in seen.iter().enumerate() {
            if s.1 != *world || far(s.2, *pos) > view {
                continue;
            }
            let [spawn, look, rides, held] = leash.packets(*pos, ride, holder);
            match leash.told.get_mut(&s.0) {
                Some(told) if told.again.is_some_and(|t| tick >= t) => {
                    for packet in [&spawn, &look, &rides, &held] {
                        told.java.send_packet(packet);
                    }
                    (told.ride, told.holder, told.again) = (ride, holder, None);
                }
                Some(told) if told.again.is_none() => {
                    // Riding drops the lead in the game: the holder is told again after it
                    if told.ride != ride || told.holder != holder || resend {
                        told.java.send_packet(&rides);
                        told.java.send_packet(&held);
                        (told.ride, told.holder) = (ride, holder);
                    }
                }
                Some(_) => {}
                None => {
                    let Some(java) = players[i].as_java().filter(|j| j.get_version() == LEASH_GAME) else { continue };
                    for packet in [&spawn, &look, &rides, &held] {
                        java.send_packet(packet);
                    }
                    leash.told.insert(s.0.clone(), Told { java, ride, holder, again: None });
                }
            }
        }
    }
}

/// A player's game lost every entity (a respawn, another world): what they were told of goes
/// to them again, and a lead they hold goes to everyone again (their entity is a new one there).
fn lost_entities(st: &mut State, p: &Player) {
    let (name, uuid) = (p.get_name().to_ascii_lowercase(), uuid_hex(&p.get_id()));
    // The game hears of it a little later: the creature waits for that
    let again = Some(st.tick + 5);
    for leash in st.instances.iter_mut().filter_map(|i| i.leash.as_mut()) {
        let all = leash.holder == uuid;
        for (who, told) in &mut leash.told {
            if all || *who == name {
                told.again = again;
            }
        }
    }
}

/// Bring the list of models that stand in the world back from `placed.json`.
fn restore_placed(st: &mut State) {
    let list: Vec<Placed> = c::load_json(PLACED_FILE).unwrap_or_default();
    st.waiting.clear();
    for p in list {
        st.next_id = st.next_id.max(p.id + 1);
        let Some(data) = find_model(st, &p.model) else {
            st.notes.push(Note::new(PLACED_FILE, format!("ไม่มีโมเดล {} แล้ว (ตัวที่ {}) จึงไม่ถูกแสดง", p.model, p.id), format!("there is no model {} any more (number {}), so it is not shown", p.model, p.id)));
            st.waiting.push(p);
            continue;
        };
        let mut inst = Instance::new(p.id, data, Anchor::Placed { world: p.world, pos: (p.x, p.y, p.z) }, p.yaw, "");
        inst.scale = p.scale;
        inst.animation = p.animation;
        inst.commands = p.commands;
        inst.tag_text = p.name;
        inst.kept = true;
        st.instances.push(inst);
    }
}

fn apply(st: &mut State, loaded: Loaded) {
    // Everything leaves the world: the bones of a model may have changed
    let old = std::mem::take(&mut st.instances);
    let mut riding = Vec::new();
    for mut inst in old {
        inst.hide(&mut st.boxes);
        // The ones from `placed.json` are read from it again below
        if !inst.kept {
            riding.push(inst);
        }
    }
    st.boxes.clear();
    st.watch.loaded += 1;
    st.cfg = loaded.cfg;
    st.models = loaded.models;
    st.notes = loaded.notes;
    st.pack = loaded.pack;
    restore_placed(st);
    // Models on creatures, and ones another plugin put somewhere, stay if their model is still there
    for mut inst in riding {
        // A model made of an item has no file to read again
        let data = if inst.data.from_item { Some(inst.data.clone()) } else { find_model(st, &inst.data.model.name) };
        match data {
            Some(data) => {
                inst.data = data;
                inst.runs.clear();
                st.instances.push(inst);
            }
            None => {
                if let Anchor::Riding { base, base_id, hid, .. } = &inst.anchor {
                    if st.bases.get(base_id) == Some(&inst.id) {
                        st.bases.remove(base_id);
                    }
                    if *hid {
                        base.set_invisible(false);
                    }
                }
            }
        }
    }
    listen(st);
}

// ---- the tick --------------------------------------------------------------------

fn pack_link(st: &State) -> Option<(String, String)> {
    let built = st.pack.as_ref()?;
    // A pack that holds no model is not worth a download
    (!st.models.is_empty()).then_some(())?;
    Some((st.cfg.pack_url(&built.sha1)?, built.sha1.clone()))
}

fn pack_uuid() -> Uuid {
    let (high, low) = pack::pack_id();
    Uuid { high, low }
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

fn respond(pack: Option<&Built>, req: &Request) -> Response {
    let Some(built) = pack else { return Response::not_found() };
    let hashed = req.path.strip_prefix('/').and_then(|p| p.strip_suffix(".zip")).is_some_and(|name| name == built.sha1);
    if !hashed && req.path != "/pack.zip" {
        return Response::not_found();
    }
    Response { status: 200, kind: "application/zip", body: built.bytes.clone(), etag: Some(format!("\"{}\"", built.sha1)), cache: if hashed { mcsv_web::FOREVER } else { mcsv_web::NO_CACHE }, extra: "" }
}

/// Something to do once the state is let go of: commands and messages to other plugins run
/// code that may come back here.
enum After {
    Command { player: Uuid, command: String },
    Tell { plugin: String, message: serde_json::Value },
}

fn run_after(server: &Server, jobs: Vec<After>) {
    for job in jobs {
        match job {
            After::Command { player, command } => {
                if let Some(p) = server.get_player_by_uuid(player) {
                    server.execute_command(&command, ServerSender::Player(p));
                }
            }
            After::Tell { plugin, message } => {
                let _ = c::ipc(&plugin, &message);
            }
        }
    }
}

fn wrap_degrees(a: f32) -> f32 {
    let r = a.rem_euclid(360.0);
    if r > 180.0 { r - 360.0 } else { r }
}

/// Once a second: who is near what, which models are in the world, which creatures are gone.
fn look_around(st: &mut State, server: &Server, after: &mut Vec<After>) {
    let players: Vec<(String, (f64, f64, f64))> = server.get_all_players().iter().map(|p| (p.get_world().get_dimension(), p.get_position())).collect();
    let (view, far) = (f64::from(st.cfg.view_distance), f64::from(st.cfg.far_distance));
    let (base_interval, far_interval) = (st.cfg.update_interval, st.cfg.far_interval);
    let mut gone = Vec::new();
    let mut boxes = std::mem::take(&mut st.boxes);
    let mut scratch = std::mem::take(&mut st.scratch);
    for inst in &mut st.instances {
        let (pos, world) = match &mut inst.anchor {
            Anchor::Placed { world, pos } => (*pos, world.clone()),
            Anchor::Riding { base, lived, stale, .. } => {
                // A creature that was taken out of the world (or whose chunk went to sleep)
                // no longer grows older, and its riders were set down
                let now = base.get_ticks_lived();
                *stale = if now == *lived { *stale + 1 } else { 0 };
                *lived = now;
                let left = inst.parts.first().is_some_and(|p| p.entity.get_vehicle().is_none());
                if (*stale >= 3 || left) && inst.die_at.is_none() {
                    gone.push(inst.id);
                    continue;
                }
                (base.get_position(), base.get_world().get_dimension())
            }
        };
        let nearest = players.iter().filter(|(w, _)| *w == world).map(|(_, p)| ((p.0 - pos.0).powi(2) + (p.1 - pos.1).powi(2) + (p.2 - pos.2).powi(2)).sqrt()).fold(f64::INFINITY, f64::min);
        // A little further to leave than to come, so a player at the edge does not flicker it
        inst.near = nearest <= if inst.near { view + 8.0 } else { view };
        inst.far = nearest > far;
        let interval = if inst.quick() {
            1
        } else if inst.far {
            far_interval
        } else {
            base_interval
        };
        if inst.near && !inst.shown() {
            show(inst, server, interval, &mut boxes, &mut scratch);
            inst.due = 0;
        } else if !inst.near && inst.shown() && matches!(inst.anchor, Anchor::Placed { .. }) {
            inst.hide(&mut boxes);
        } else if inst.shown() && inst.interval != interval {
            inst.interval = interval;
            for part in &inst.parts {
                part.display.set_interpolation_duration(interval as i32);
                part.display.set_teleport_duration(interval.min(59) as i32);
            }
        }
    }
    st.boxes = boxes;
    st.scratch = scratch;
    let tick = st.tick;
    st.bodies.retain(|_, b| tick < b.tick + 100);
    for id in gone {
        let owner = st.instances.iter().find(|i| i.id == id).map(|i| i.owner.clone()).unwrap_or_default();
        remove_instance(st, id);
        if !owner.is_empty() {
            after.push(After::Tell { plugin: owner, message: serde_json::json!({ "kind": "model_gone", "id": id, "why": "entity gone" }) });
        }
    }
}

fn on_tick(server: Server) {
    let mut after = Vec::new();
    {
        let Some(mut guard) = try_lock() else { return };
        let Some(st) = guard.as_mut() else { return };
        st.tick += 1;
        let tick = st.tick;

        if let Some(web) = st.web.as_mut() {
            let now = Instant::now();
            let pack = st.pack.as_ref();
            web.poll(now, now + WEB_BUDGET, &mut |req| respond(pack, req));
        }

        let started = Instant::now();
        if !st.pending.is_empty() {
            settle_pending(st, &server, &mut after);
        }
        if tick % 20 == 1 {
            look_around(st, &server, &mut after);
        }
        if tick % 5 == 2 {
            tend_leashes(st, &server, tick % 40 == 2);
        }
        if tick >= st.swept + SWEEP_EVERY || st.swept == 0 && tick == 40 {
            st.swept = tick;
            let n = sweep(&server);
            if n > 0 {
                c::log_info(&format!("removed {n} bone(s) that an earlier run left in the world"));
            }
        }

        // Models that are due, starting after the one the last tick ended with
        let budget = st.cfg.max_updates_per_tick;
        let count = st.instances.len();
        let mut used = 0u32;
        let mut done = Vec::new();
        let mut scratch = std::mem::take(&mut st.scratch);
        for step in 0..count {
            let at = (st.turn + step) % count;
            let inst = &mut st.instances[at];
            if let Some(end) = inst.die_at
                && tick >= end
            {
                done.push(inst.id);
                continue;
            }
            if inst.tint.is_some() && tick >= inst.tint_until {
                set_tint(inst, None);
            }
            // Nobody near: a model on a creature keeps riding, but nothing moves its bones
            if !inst.shown() || !inst.near || tick < inst.due {
                continue;
            }
            if used >= budget {
                st.stat.late += 1;
                st.turn = at;
                break;
            }
            let dt = if inst.last_tick == 0 { inst.interval.max(1) as f32 } else { (tick - inst.last_tick) as f32 };
            if let Anchor::Riding { base, base_id, player, .. } = &inst.anchor {
                let pos = base.get_position();
                let moved = ((pos.0 - inst.last_pos.0).powi(2) + (pos.2 - inst.last_pos.2).powi(2)).sqrt();
                inst.moving = moved > 0.015 * f64::from(dt);
                inst.last_pos = pos;
                let look = base.get_yaw();
                // A player's yaw is where they look; the body turns after it the game's way
                let yaw = if *player {
                    let body = st.bodies.entry(*base_id).or_insert(Body { yaw: rig::wrap(look), at: pos, tick });
                    if body.tick != tick {
                        body.yaw = rig::body_turn(body.yaw, look, (pos.0 - body.at.0, pos.2 - body.at.2));
                        (body.at, body.tick) = (pos, tick);
                    }
                    body.yaw
                } else {
                    look
                };
                // A step of the game's own byte for turns: finer makes no difference to the eye
                inst.yaw = (yaw * 256.0 / 360.0).round() * 360.0 / 256.0;
                if inst.data.head {
                    // Something worn on a head goes as far as a player's head does
                    let up_down = if inst.extra { 90.0 } else { 60.0 };
                    inst.look = (wrap_degrees(base.get_head_yaw() - yaw).clamp(-75.0, 75.0), base.get_pitch().clamp(-up_down, up_down));
                }
            }
            inst.advance(dt);
            used += pose(inst, &mut scratch, &mut st.stat).max(1);
            inst.last_tick = tick;
            inst.due = tick + u64::from(inst.interval.max(1));
        }
        st.scratch = scratch;
        if tick < st.watch.until {
            let mut rev = st.watch.rev;
            for inst in &mut st.instances {
                let now = inst.telling();
                if now != inst.told {
                    inst.told = now;
                    rev += 1;
                    inst.rev = rev;
                }
            }
            st.watch.rev = rev;
        }
        for id in done {
            let owner = st.instances.iter().find(|i| i.id == id).map(|i| i.owner.clone()).unwrap_or_default();
            remove_instance(st, id);
            if !owner.is_empty() {
                after.push(After::Tell { plugin: owner, message: serde_json::json!({ "kind": "model_gone", "id": id, "why": "death" }) });
            }
        }
        st.stat.ticks += 1;
        st.stat.us += started.elapsed().as_micros() as u64;

        // Players: whoever does not have the pack as it is now gets it
        if tick % 10 == 0 {
            let players = server.get_all_players();
            let online: Vec<String> = players.iter().map(|p| p.get_name().to_ascii_lowercase()).collect();
            st.players.retain(|name, _| online.contains(name));
            if let Some((_, sha1)) = pack_link(st).filter(|_| st.cfg.pack.send) {
                for p in &players {
                    if p.as_java().is_none() || c::awaiting_login(p) {
                        continue;
                    }
                    let delay = st.cfg.pack.join_delay_ticks;
                    let seen = st.players.entry(p.get_name().to_ascii_lowercase()).or_insert_with(|| Seen { due: tick + delay, ..Seen::default() });
                    if seen.sent != sha1 && tick >= seen.due {
                        send_pack(st, p);
                    }
                }
            }
        }
    }
    run_after(&server, after);
}

// ---- events ----------------------------------------------------------------------

type Data_<E> = <E as FromIntoEvent>::Data;

struct OnClick;
impl EventHandler<PlayerInteractEntityEvent> for OnClick {
    fn handle(&self, server: Server, mut e: Data_<PlayerInteractEntityEvent>) -> Data_<PlayerInteractEntityEvent> {
        let mut after = Vec::new();
        {
            let Some(mut guard) = try_lock() else { return e };
            let Some(st) = guard.as_mut() else { return e };
            let Some(id) = st.boxes.get(&e.entity_id).copied() else { return e };
            let Some(inst) = st.instances.iter().find(|i| i.id == id) else { return e };
            e.cancelled = true;
            let name = e.player.get_name();
            let player = e.player.get_id();
            for command in &inst.commands {
                after.push(After::Command { player, command: command.replace("%player%", &name).trim_start_matches('/').to_owned() });
            }
            if !inst.owner.is_empty() {
                after.push(After::Tell { plugin: inst.owner.clone(), message: serde_json::json!({ "kind": "model_click", "id": id, "player": name, "attack": format!("{:?}", e.action).to_ascii_lowercase().contains("attack") }) });
            }
        }
        run_after(&server, after);
        e
    }
}

struct OnHurt;
impl EventHandler<EntityDamageEvent> for OnHurt {
    fn handle(&self, _server: Server, e: Data_<EntityDamageEvent>) -> Data_<EntityDamageEvent> {
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let Some(id) = st.bases.get(&e.entity_id).copied() else { return e };
        let tick = st.tick;
        if let Some(inst) = st.instances.iter_mut().find(|i| i.id == id)
            && !e.cancelled
            && inst.die_at.is_none()
        {
            if let Some(rgb) = inst.data.set.tint() {
                set_tint(inst, Some(rgb));
                inst.tint_until = tick + 6;
            }
            if let Some(hurt) = inst.data.hurt {
                let blend = inst.data.set.blend.min(2);
                inst.play(hurt, Some(Loop::Once), 1.0, blend, false);
            }
        }
        e
    }
}

struct OnDeath;
impl EventHandler<EntityDeathEvent> for OnDeath {
    fn handle(&self, _server: Server, e: Data_<EntityDeathEvent>) -> Data_<EntityDeathEvent> {
        let Some(mut guard) = try_lock() else { return e };
        let Some(st) = guard.as_mut() else { return e };
        let Some(id) = st.bases.get(&e.entity_id).copied() else { return e };
        let tick = st.tick;
        if let Some(inst) = st.instances.iter_mut().find(|i| i.id == id) {
            // The death animation plays where the creature fell, then the model goes
            match inst.data.death {
                Some(death) => {
                    inst.runs.clear();
                    inst.play(death, Some(Loop::Hold), 1.0, 0, false);
                    inst.die_at = Some(tick + u64::from(inst.data.model.animations[death].ticks) + 10);
                }
                None => inst.die_at = Some(tick + 20),
            }
            inst.due = 0;
        }
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, e: Data_<PlayerLeaveEvent>) -> Data_<PlayerLeaveEvent> {
        if let Some(mut guard) = try_lock()
            && let Some(st) = guard.as_mut()
        {
            let name = e.player.get_name().to_ascii_lowercase();
            st.players.remove(&name);
            // The models a player wears leave with them
            let id = e.player.as_entity().get_id() as i32;
            let worn: Vec<u32> = st.instances.iter().filter(|i| matches!(i.anchor, Anchor::Riding { base_id, .. } if base_id == id)).map(|i| i.id).collect();
            for model in worn {
                remove_instance(st, model);
            }
            st.bodies.remove(&id);
            let uuid = uuid_hex(&e.player.get_id());
            for leash in st.instances.iter_mut().filter_map(|i| i.leash.as_mut()) {
                leash.told.remove(&name);
                if leash.holder == uuid {
                    leash.drop_all();
                }
            }
        }
        e
    }
}

struct OnRespawn;
impl EventHandler<PlayerRespawnEvent> for OnRespawn {
    fn handle(&self, _server: Server, e: Data_<PlayerRespawnEvent>) -> Data_<PlayerRespawnEvent> {
        if let Some(mut guard) = try_lock()
            && let Some(st) = guard.as_mut()
        {
            lost_entities(st, &e.player);
        }
        e
    }
}

struct OnWorld;
impl EventHandler<PlayerChangeWorldEvent> for OnWorld {
    fn handle(&self, _server: Server, e: Data_<PlayerChangeWorldEvent>) -> Data_<PlayerChangeWorldEvent> {
        if !e.cancelled
            && let Some(mut guard) = try_lock()
            && let Some(st) = guard.as_mut()
        {
            lost_entities(st, &e.player);
        }
        e
    }
}

struct OnStatus;
impl EventHandler<PlayerResourcePackStatusEvent> for OnStatus {
    fn handle(&self, _server: Server, e: Data_<PlayerResourcePackStatusEvent>) -> Data_<PlayerResourcePackStatusEvent> {
        if let Some(mut guard) = try_lock()
            && let Some(st) = guard.as_mut()
            && e.pack_id == mcsv_pack::id_text(pack::pack_id())
            && let Some(seen) = st.players.get_mut(&e.player.get_name().to_ascii_lowercase())
        {
            seen.answer = format!("{:?}", e.status).to_ascii_lowercase();
        }
        e
    }
}

// ---- commands --------------------------------------------------------------------

fn nearest(st: &State, p: &Player, reach: f64) -> Option<u32> {
    let world = p.get_world().get_dimension();
    let at = p.get_position();
    st.instances
        .iter()
        .filter_map(|i| match &i.anchor {
            Anchor::Placed { world: w, pos } if *w == world => Some((i.id, ((pos.0 - at.0).powi(2) + (pos.1 - at.1).powi(2) + (pos.2 - at.2).powi(2)).sqrt())),
            _ => None,
        })
        .filter(|(_, d)| *d <= reach)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id)
}

/// The creature closest to a player, within 6 blocks: not a player, not one of our own entities.
fn creature_near(p: &Player) -> Option<Entity> {
    let me = p.as_entity();
    let (own, at) = (me.get_id(), p.get_position());
    let far = |e: &Entity| {
        let q = e.get_position();
        (q.0 - at.0).powi(2) + (q.1 - at.1).powi(2) + (q.2 - at.2).powi(2)
    };
    me.get_nearby_entities(6.0, 6.0, 6.0).into_iter().filter(|e| e.get_id() != own && e.is_living() && !matches!(e.get_type(), EntityType::Player) && !e.has_custom_data(NS, PART)).min_by(|a, b| far(a).total_cmp(&far(b)))
}

/// `<number>` or `.` (the placed model closest to the player).
fn target(st: &State, sender: &CommandSender, word: Option<&String>) -> Option<u32> {
    match word.map(String::as_str) {
        Some(".") | None => nearest(st, &c::sender_player(sender)?, 16.0),
        Some(n) => n.trim_start_matches('#').parse().ok().filter(|id| st.instances.iter().any(|i| i.id == *id)),
    }
}

fn mode_of(word: Option<&String>) -> Option<Loop> {
    match word.map(String::as_str) {
        Some("once") => Some(Loop::Once),
        Some("loop") => Some(Loop::Loop),
        Some("hold") => Some(Loop::Hold),
        _ => None,
    }
}

fn describe(i: &Instance, th: bool) -> String {
    let what = match &i.anchor {
        Anchor::Placed { world, pos } => format!("{:.0} {:.0} {:.0} {}", pos.0, pos.1, pos.2, world.trim_start_matches("minecraft:")),
        Anchor::Riding { base_id, .. } => {
            if th {
                format!("อยู่บน entity {base_id}")
            } else {
                format!("on entity {base_id}")
            }
        }
    };
    let playing: Vec<&str> = i.runs.iter().filter(|r| r.target > 0.0).map(|r| i.data.model.animations[r.animation].name.as_str()).collect();
    format!("&e#{} &f{} &7{what} &8| &7{}{}", i.id, i.data.model.name, if playing.is_empty() { "-".to_owned() } else { playing.join(", ") }, if i.shown() { "" } else if th { " &8(ไม่มีผู้เล่นใกล้)" } else { " &8(no player near)" })
}

fn help(th: bool) -> Vec<String> {
    let lines: &[(&str, &str, &str)] = &[
        ("/models list", "โมเดลที่โหลดไว้", "the models that are loaded"),
        ("/models info <model>", "กระดูก แอนิเมชัน และปัญหาของโมเดล", "bones, animations and problems of a model"),
        ("/models place <model> [animation]", "วางโมเดลตรงที่ยืน", "put a model where you stand"),
        ("/models remove [id]", "เอาโมเดลออก (ไม่ใส่ id = ตัวที่ใกล้สุด)", "take a model away (no id = the closest)"),
        ("/models near", "โมเดลที่วางไว้ทั้งหมด", "every model in the world"),
        ("/models play <id|.> <animation> [once|loop|hold]", "เล่นแอนิเมชัน", "play an animation"),
        ("/models stop <id|.> [animation]", "หยุดแอนิเมชัน", "stop an animation"),
        ("/models scale|turn|here <id|.> ...", "ขนาด หมุน หรือย้ายมาที่ยืน", "size, turn, or move it to you"),
        ("/models click <id|.> <command|clear>", "คำสั่งที่รันเมื่อผู้เล่นคลิกโมเดล", "a command a click on the model runs"),
        ("/models name <id|.> <text|clear>", "ชื่อเหนือโมเดล (ต้องมีกระดูก tag_)", "the name above a model (needs a tag_ bone)"),
        ("/models attach <model>", "ใส่โมเดลให้ entity ที่มองอยู่", "put a model on the creature you look at"),
        ("/models detach", "ถอดโมเดลจาก entity ที่มองอยู่", "take the model off the creature you look at"),
        ("/models wear <model> | unwear", "ใส่โมเดลให้ตัวเอง", "wear a model yourself"),
        ("/models pack [send [player]]", "สถานะของ resource pack", "the state of the resource pack"),
        ("/models reload | stat | sweep | version", "", ""),
    ];
    let mut out = vec![format!("{P}&f{VERSION}")];
    out.extend(lines.iter().map(|(cmd, thai, english)| format!("&e{cmd} &7{}", if th { thai } else { english })));
    out
}

fn run(st: &mut State, sender: &CommandSender, server: &Server, w: &[String], after: &mut Vec<After>) -> Vec<String> {
    let th = st.cfg.thai();
    let player = c::sender_player(sender);
    let no_model = |name: &str| msg(th, &format!("&cไม่มีโมเดล {name} &7(ดู /models list)"), &format!("&cThere is no model {name} &7(see /models list)"));
    let no_target = || msg(th, "&cไม่พบโมเดลนั้น &7(ใส่เลขจาก /models near หรือยืนใกล้ ๆ แล้วใช้ .)", "&cNo such model &7(give a number from /models near, or stand close and use .)");
    let players_only = || msg(th, "&cคำสั่งนี้ใช้ในเกมเท่านั้น", "&cThis command is for players in the game");
    let _ = after;
    match w.first().map(String::as_str) {
        Some("version") => vec![format!("Models {VERSION}")],
        Some("list") => {
            if st.models.is_empty() {
                return vec![msg(th, "&7ยังไม่มีโมเดล วางไฟล์ .bbmodel ใน plugins/data/models/models/ แล้วสั่ง /models reload", "&7No models yet. Put .bbmodel files into plugins/data/models/models/ and run /models reload")];
            }
            let mut out = vec![msg(th, &format!("&fโมเดล {} ตัว", st.models.len()), &format!("&f{} model(s)", st.models.len()))];
            for d in &st.models {
                let anims: Vec<&str> = d.model.animations.iter().map(|a| a.name.as_str()).collect();
                out.push(format!("&e{} &7{} {} &8| &7{}", d.model.name, d.model.drawn().count(), if th { "ชิ้น" } else { "part(s)" }, if anims.is_empty() { "-".to_owned() } else { anims.join(", ") }));
            }
            if !st.notes.is_empty() {
                out.push(msg(th, &format!("&6มี {} เรื่องที่ควรดู: /models info", st.notes.len()), &format!("&6{} thing(s) to look at: /models info", st.notes.len())));
            }
            out
        }
        Some("info") => {
            let Some(name) = w.get(1) else {
                let mut out = vec![msg(
                    th,
                    &format!("&fรุ่น {VERSION} &7โมเดล {} ตัว ในโลก {} ตัว อัปเดตทุก {} tick", st.models.len(), st.instances.len(), st.cfg.update_interval),
                    &format!("&fversion {VERSION} &7{} model(s), {} in the world, a pose every {} tick(s)", st.models.len(), st.instances.len(), st.cfg.update_interval),
                )];
                out.extend(st.notes.iter().take(30).map(|n| format!("&6! &7{}", n.text(th))));
                return out;
            };
            let Some(d) = find_model(st, name) else { return vec![no_model(name)] };
            let m = &d.model;
            let mut out = vec![msg(th, &format!("&f{} &7{} ชิ้น จากกระดูก {} ท่อน ภาพ {} ภาพ", m.name, m.drawn().count(), m.bones.len(), m.textures.len()), &format!("&f{} &7{} part(s) from {} bone(s), {} picture(s)", m.name, m.drawn().count(), m.bones.len(), m.textures.len()))];
            if let Some((width, height)) = m.hitbox.or(d.set.hitbox) {
                out.push(format!("&7hitbox {width:.2} x {height:.2}"));
            }
            let special: Vec<String> = m.bones.iter().filter(|b| b.kind != Kind::Plain).map(|b| format!("{} ({:?})", b.name, b.kind).to_ascii_lowercase()).collect();
            if !special.is_empty() {
                out.push(format!("&7{}", special.join(", ")));
            }
            for a in &m.animations {
                let role = [(d.idle, "idle"), (d.walk, "walk"), (d.hurt, "hurt"), (d.death, "death"), (d.spawn, "spawn")].iter().find(|(i, _)| m.animation(&a.name) == *i).map_or(String::new(), |(_, r)| format!(" &8({r})"));
                out.push(format!("&e{} &7{:.2}s {:?} {} {}{role}", a.name, a.ticks as f32 / 20.0, a.looping, a.tracks.iter().flatten().count(), if th { "กระดูก" } else { "bone(s)" }).replace("Loop", "loop").replace("Once", "once").replace("Hold", "hold"));
            }
            let place = format!("models/{}", m.name);
            out.extend(st.notes.iter().filter(|n| n.en.starts_with(&place)).map(|n| format!("&6! &7{}", n.text(th))));
            out
        }
        Some("reload") => {
            let loaded = load();
            apply(st, loaded);
            let mut out = vec![msg(th, &format!("&aโหลดใหม่แล้ว &7โมเดล {} ตัว", st.models.len()), &format!("&aReloaded &7{} model(s)", st.models.len()))];
            if let Some(b) = &st.pack {
                out.push(format!("&7pack.zip {} KB, {} {} ({} ms)", b.bytes.len() / 1024, b.files, if th { "ไฟล์" } else { "files" }, b.took_ms));
            }
            if !st.notes.is_empty() {
                out.push(msg(th, &format!("&6มี {} เรื่องที่ควรดู: /models info", st.notes.len()), &format!("&6{} thing(s) to look at: /models info", st.notes.len())));
            }
            out
        }
        Some("pack") => {
            if w.get(1).is_some_and(|a| a == "send") {
                if pack_link(st).is_none() {
                    return vec![msg(th, "&cยังส่งไม่ได้: ตั้ง pack.url หรือ pack.port กับ pack.host ใน config.toml ก่อน", "&cNothing to send yet: set pack.url, or pack.port and pack.host, in config.toml")];
                }
                let targets: Vec<Player> = match w.get(2) {
                    Some(name) => server.get_player_by_name(name).into_iter().collect(),
                    None => server.get_all_players(),
                };
                let n = targets.iter().filter(|p| send_pack(st, p)).count();
                return vec![msg(th, &format!("&aส่ง pack ให้ {n} คน"), &format!("&aSent the pack to {n} player(s)"))];
            }
            let mut out = Vec::new();
            match &st.pack {
                Some(b) => out.push(msg(th, &format!("&fpack.zip &7{} KB {} ไฟล์ SHA-1 {}", b.bytes.len() / 1024, b.files, b.sha1), &format!("&fpack.zip &7{} KB, {} files, SHA-1 {}", b.bytes.len() / 1024, b.files, b.sha1))),
                None => out.push(msg(th, "&cสร้าง pack ไม่ได้ &7(ดู /models info)", "&cThe pack could not be built &7(see /models info)")),
            }
            match pack_link(st) {
                Some((url, _)) => out.push(format!("&7{url}")),
                None => out.push(msg(th, "&6ผู้เล่นยังไม่ได้ pack: ตั้ง pack.url (ลิงก์ที่ฝากไฟล์เอง) หรือ pack.port กับ pack.host ใน config.toml", "&6Players do not get the pack yet: set pack.url (a link you host it at), or pack.port and pack.host, in config.toml")),
            }
            if let Some(e) = &st.web_error {
                out.push(msg(th, &format!("&cเปิดพอร์ต {} ไม่ได้: {e}", st.cfg.pack.port), &format!("&cPort {} could not be opened: {e}", st.cfg.pack.port)));
            }
            if let Some(web) = &st.web {
                out.push(msg(th, &format!("&7เสิร์ฟที่พอร์ต {} ส่งไฟล์แล้ว {} ครั้ง ส่งให้ผู้เล่น {} ครั้ง", web.port, web.served, st.sent), &format!("&7Serving on port {}: {} download(s), sent to players {} time(s)", web.port, web.served, st.sent)));
            }
            let answers: Vec<String> = st.players.iter().filter(|(_, s)| !s.answer.is_empty()).map(|(name, s)| format!("{name}: {}", s.answer)).collect();
            if !answers.is_empty() {
                out.push(format!("&7{}", answers.join(", ")));
            }
            out
        }
        Some("place" | "attach" | "wear") if st.instances.len() >= MAX_MODELS => vec![msg(th, &format!("&cมีโมเดลในโลกครบ {MAX_MODELS} ตัวแล้ว"), &format!("&cThere are {MAX_MODELS} models in the world already"))],
        Some("place") => {
            let Some(p) = player else { return vec![players_only()] };
            let Some(name) = w.get(1) else { return help(th) };
            let Some(data) = find_model(st, name) else { return vec![no_model(name)] };
            let animation = match w.get(2) {
                Some(a) if data.model.animation(&rig::plain_name(a)).is_none() => return vec![msg(th, &format!("&cโมเดลนี้ไม่มีแอนิเมชัน {a}"), &format!("&cThis model has no animation {a}"))],
                other => other.cloned(),
            };
            let id = add_placed(st, data, p.get_world().get_dimension(), p.get_position(), p.get_yaw(), "");
            if let Some(inst) = st.instances.iter_mut().find(|i| i.id == id) {
                inst.animation = animation;
                inst.kept = true;
                inst.near = true;
                let (mut boxes, mut scratch) = (std::mem::take(&mut st.boxes), std::mem::take(&mut st.scratch));
                show(inst, server, st.cfg.update_interval, &mut boxes, &mut scratch);
                (st.boxes, st.scratch) = (boxes, scratch);
            }
            save_placed(st);
            vec![msg(th, &format!("&aวาง {name} แล้ว &7(เลข {id})"), &format!("&aPlaced {name} &7(number {id})"))]
        }
        Some("remove") => {
            // One that is only on the list, because its model file is not there
            if let Some(n) = w.get(1).and_then(|n| n.trim_start_matches('#').parse::<u32>().ok())
                && let Some(at) = st.waiting.iter().position(|p| p.id == n)
            {
                st.waiting.remove(at);
                save_placed(st);
                return vec![msg(th, &format!("&aเอาโมเดลเลข {n} ออกจากรายการแล้ว"), &format!("&aTook model number {n} off the list"))];
            }
            let Some(id) = target(st, sender, w.get(1)) else { return vec![no_target()] };
            remove_instance(st, id);
            save_placed(st);
            vec![msg(th, &format!("&aเอาโมเดลเลข {id} ออกแล้ว"), &format!("&aRemoved model number {id}"))]
        }
        Some("near") => {
            if st.instances.is_empty() && st.waiting.is_empty() {
                return vec![msg(th, "&7ยังไม่มีโมเดลในโลก", "&7No model is in the world")];
            }
            let mut list: Vec<&Instance> = st.instances.iter().collect();
            list.sort_by_key(|i| i.id);
            let mut out = vec![msg(th, &format!("&fโมเดลในโลก {} ตัว", list.len()), &format!("&f{} model(s) in the world", list.len()))];
            out.extend(list.iter().take(40).map(|i| describe(i, th)));
            out.extend(st.waiting.iter().take(20).map(|p| format!("&8#{} {} &7{}", p.id, p.model, if th { "(ไม่มีไฟล์โมเดลนี้แล้ว: /models remove <เลข> เพื่อเอาออก)" } else { "(its model file is gone: /models remove <number> takes it off)" })));
            out
        }
        Some("play") => {
            let Some(id) = target(st, sender, w.get(1)) else { return vec![no_target()] };
            let Some(inst) = st.instances.iter_mut().find(|i| i.id == id) else { return vec![no_target()] };
            let Some(name) = w.get(2) else { return help(th) };
            let Some(animation) = inst.data.model.animation(&rig::plain_name(name)) else { return vec![msg(th, &format!("&cโมเดลนี้ไม่มีแอนิเมชัน {name}"), &format!("&cThis model has no animation {name}"))] };
            let blend = inst.data.set.blend;
            inst.play(animation, mode_of(w.get(3)), 1.0, blend, false);
            inst.due = 0;
            vec![msg(th, &format!("&aเล่น {name} ที่โมเดลเลข {id}"), &format!("&aPlaying {name} on model number {id}"))]
        }
        Some("stop") => {
            let Some(id) = target(st, sender, w.get(1)) else { return vec![no_target()] };
            let Some(inst) = st.instances.iter_mut().find(|i| i.id == id) else { return vec![no_target()] };
            let animation = w.get(2).and_then(|n| inst.data.model.animation(&rig::plain_name(n)));
            let blend = inst.data.set.blend;
            inst.stop(animation, blend);
            vec![msg(th, &format!("&aหยุดแอนิเมชันที่โมเดลเลข {id}"), &format!("&aStopped on model number {id}"))]
        }
        Some(what @ ("scale" | "turn" | "here")) => {
            let Some(id) = target(st, sender, w.get(1)) else { return vec![no_target()] };
            let Some(inst) = st.instances.iter_mut().find(|i| i.id == id) else { return vec![no_target()] };
            match what {
                "scale" => {
                    let Some(n) = w.get(2).and_then(|n| n.parse::<f32>().ok()).filter(|n| n.is_finite() && (0.01..=100.0).contains(n)) else { return vec![msg(th, "&cขนาดต้องเป็นเลข 0.01 ถึง 100", "&cThe size must be a number from 0.01 to 100")] };
                    inst.scale = n;
                }
                "turn" => {
                    inst.yaw = match (w.get(2).and_then(|n| n.parse::<f32>().ok()), &player) {
                        (Some(yaw), _) if yaw.is_finite() => wrap_degrees(yaw),
                        (_, Some(p)) => p.get_yaw(),
                        _ => return vec![players_only()],
                    };
                }
                _ => {
                    let Some(p) = &player else { return vec![players_only()] };
                    let Anchor::Placed { world, pos } = &mut inst.anchor else { return vec![no_target()] };
                    *world = p.get_world().get_dimension();
                    *pos = p.get_position();
                }
            }
            // Its entities are made again where and as it now is
            let (mut boxes, mut scratch) = (std::mem::take(&mut st.boxes), std::mem::take(&mut st.scratch));
            let was_shown = inst.shown();
            inst.hide(&mut boxes);
            if was_shown {
                show(inst, server, st.cfg.update_interval, &mut boxes, &mut scratch);
            }
            (st.boxes, st.scratch) = (boxes, scratch);
            save_placed(st);
            vec![msg(th, &format!("&aแก้โมเดลเลข {id} แล้ว"), &format!("&aChanged model number {id}"))]
        }
        Some("click") => {
            let Some(id) = target(st, sender, w.get(1)) else { return vec![no_target()] };
            let Some(inst) = st.instances.iter_mut().find(|i| i.id == id) else { return vec![no_target()] };
            if w.len() < 3 {
                return if inst.commands.is_empty() { vec![msg(th, "&7ยังไม่มีคำสั่งเมื่อคลิก", "&7A click runs nothing yet")] } else { inst.commands.iter().map(|cmd| format!("&7/{cmd}")).collect() };
            }
            if w[2] == "clear" {
                inst.commands.clear();
            } else if inst.commands.len() < 10 {
                inst.commands.push(w[2..].join(" ").trim_start_matches('/').to_owned());
            }
            let n = inst.commands.len();
            let needs_box = inst.data.model.hitbox.or(inst.data.set.hitbox).is_none();
            save_placed(st);
            let mut out = vec![msg(th, &format!("&aโมเดลเลข {id} มีคำสั่งเมื่อคลิก {n} คำสั่ง"), &format!("&aModel number {id} runs {n} command(s) on a click"))];
            if needs_box {
                out.push(msg(th, "&6โมเดลนี้ยังไม่มีส่วนที่คลิกได้: ทำ cube ชื่อ hitbox ใน Blockbench หรือใส่ hitbox ในไฟล์ .toml ของโมเดล", "&6This model has nothing to click yet: make a cube named hitbox in Blockbench, or set hitbox in the model's .toml"));
            }
            out
        }
        Some("name") => {
            let Some(id) = target(st, sender, w.get(1)) else { return vec![no_target()] };
            let Some(inst) = st.instances.iter_mut().find(|i| i.id == id) else { return vec![no_target()] };
            if inst.data.tag.is_none() {
                return vec![msg(th, "&cโมเดลนี้ไม่มีกระดูกสำหรับชื่อ &7(ทำกลุ่มชื่อขึ้นต้นด้วย tag_ ใน Blockbench)", "&cThis model has no bone for a name &7(make a group whose name starts with tag_ in Blockbench)")];
            }
            inst.tag_text = if w.get(2).is_none_or(|a| a == "clear") { String::new() } else { w[2..].join(" ").chars().take(120).collect() };
            let (mut boxes, mut scratch) = (std::mem::take(&mut st.boxes), std::mem::take(&mut st.scratch));
            let was_shown = inst.shown();
            inst.hide(&mut boxes);
            if was_shown {
                show(inst, server, st.cfg.update_interval, &mut boxes, &mut scratch);
            }
            (st.boxes, st.scratch) = (boxes, scratch);
            save_placed(st);
            vec![msg(th, &format!("&aตั้งชื่อโมเดลเลข {id} แล้ว"), &format!("&aNamed model number {id}"))]
        }
        Some(what @ ("attach" | "wear")) => {
            let Some(p) = player else { return vec![players_only()] };
            let Some(name) = w.get(1) else { return help(th) };
            let Some(data) = find_model(st, name) else { return vec![no_model(name)] };
            let base = if what == "wear" { Some(p.as_entity()) } else { p.get_target_entity(12.0).filter(Entity::is_living).or_else(|| creature_near(&p)) };
            let Some(base) = base else { return vec![msg(th, "&cไม่พบ entity ที่จะใส่โมเดลให้: มองไปที่มัน หรือยืนห่างไม่เกิน 6 บล็อก", "&cNo creature to put the model on: look at it, or stand within 6 blocks of it")] };
            let base_id = base.get_id() as i32;
            if let Some(old) = st.bases.get(&base_id).copied() {
                remove_instance(st, old);
            }
            let id = add_riding(st, data, base, "", None, false, None);
            vec![msg(th, &format!("&aใส่ {name} แล้ว &7(เลข {id})"), &format!("&aPut {name} on &7(number {id})"))]
        }
        Some(what @ ("detach" | "unwear")) => {
            let Some(p) = player else { return vec![players_only()] };
            let found = if what == "unwear" {
                st.bases.get(&(p.as_entity().get_id() as i32)).copied()
            } else {
                // The creature in sight, or the closest one that has a model on it
                p.get_target_entity(12.0).and_then(|b| st.bases.get(&(b.get_id() as i32)).copied()).or_else(|| {
                    let own = p.as_entity().get_id();
                    let at = p.get_position();
                    let far = |e: &Entity| {
                        let q = e.get_position();
                        (q.0 - at.0).powi(2) + (q.1 - at.1).powi(2) + (q.2 - at.2).powi(2)
                    };
                    p.as_entity().get_nearby_entities(6.0, 6.0, 6.0).into_iter().filter(|e| e.get_id() != own && st.bases.contains_key(&(e.get_id() as i32))).min_by(|a, b| far(a).total_cmp(&far(b))).and_then(|e| st.bases.get(&(e.get_id() as i32)).copied())
                })
            };
            let Some(id) = found else { return vec![msg(th, "&cไม่มีโมเดลที่ถอดได้ตรงนั้น", "&cNo model to take off there")] };
            remove_instance(st, id);
            vec![msg(th, &format!("&aถอดโมเดลเลข {id} แล้ว"), &format!("&aTook model number {id} off"))]
        }
        Some("stat") => {
            let s = &st.stat;
            let t = s.ticks.max(1);
            let parts: usize = st.instances.iter().map(|i| i.parts.len()).sum();
            let shown = st.instances.iter().filter(|i| i.shown()).count();
            let line = format!("STAT models={} shown={shown} parts={parts} us_per_tick={} moved_per_tick={:.1} skipped_per_tick={:.1} late={} ticks={}", st.instances.len(), s.us / t, s.moved as f64 / t as f64, s.skipped as f64 / t as f64, s.late, s.ticks);
            st.stat = Stat::default();
            vec![line]
        }
        Some("sweep") => {
            let n = sweep(server);
            vec![msg(th, &format!("&aลบชิ้นส่วนที่ค้างในโลก {n} ชิ้น"), &format!("&aRemoved {n} leftover part(s)"))]
        }
        _ => help(th),
    }
}

struct Cmd;
impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let mut after = Vec::new();
        let lines = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            let Some(st) = guard.as_mut() else { return Ok(0) };
            run(st, &sender, &server, &w, &mut after)
        };
        for line in lines {
            c::reply(&sender, &line);
        }
        run_after(&server, after);
        Ok(1)
    }
}

// ---- other plugins ---------------------------------------------------------------

/// What another plugin asks for, as a JSON message to the plugin `models`. Every answer is
/// JSON with `"ok": true`; an error is a sentence.
///
/// | `kind` | fields | answer |
/// |---|---|---|
/// | `models` | | `models`: name, animations, hitbox, `worn` of each (the item model of the whole model worn in a helmet slot, when it can be one: the game draws it with the head, so it never lags behind) |
/// | `spawn` | `model` or `item` (as for `attach`, always `body`), `x`, `y`, `z`, `world`? (dimension), `yaw`?, `scale`?, `animation`?, `name`?, `leash`? (a player's UUID: a lead from their hand to the model's feet, seen by players on Java 26.3) | `id`; not kept over a restart |
/// | `attach` | `model` or `item`, `entity` (its id), `scale`?, `name`?, `extra`?, `hide`? | `id`; the model is on the creature from the next tick |
/// | | `item`: an item model (`namespace:path`) shown as a model of one bone; with `bone`? (`head`, the default, follows the look; `body`), `offset`? (`[x, y, z]` blocks, +Z behind), `size`?, `turn`? (degrees), `display`? (`head` on the head, else `fixed`) | |
/// | | `extra`: next to the creature's own model instead of in its place, and the creature stays seen; `hide`: hide the creature or not, whatever the model says | |
/// | `play` | `id`, `animation`, `mode`? (`once`, `loop`, `hold`), `speed`?, `blend`? (ticks) | `ticks`: its length |
/// | `stop` | `id`, `animation`? (none = all) | |
/// | `tint` | `id`, `colour` (`#rrggbb`, none = clear), `ticks`? | |
/// | `name` | `id`, `name` | `shown`: the model has a `tag_` bone |
/// | `move` | `id`, `x`?, `y`?, `z`?, `yaw`? (a model that stands by itself) | |
/// | `info` | `id` | `model`, `shown`, `playing` |
/// | `remove` | `id` | |
///
/// The plugin that asked is told about its models the same way: `{"kind":"model_click","id",
/// "player","attack"}` when a player clicks one that stands by itself, and `{"kind":
/// "model_gone","id","why"}` when the creature under one died (after its death animation), is
/// gone, or was never found.
/// The model a message names: `model` (a file of ours) or `item` (an item model the client
/// has from some pack, as `namespace:path`, made into a model of one bone). `on_creature`:
/// the item follows the look unless `bone` says `body`.
fn asked_model(st: &State, v: &serde_json::Value, on_creature: bool) -> Result<Arc<Data>, String> {
    let text = |key: &str| v.get(key).and_then(serde_json::Value::as_str);
    let number = |key: &str| v.get(key).and_then(serde_json::Value::as_f64);
    match (text("model"), text("item")) {
        (Some(model), _) => find_model(st, model).ok_or_else(|| "no such model".into()),
        (None, Some(item)) => {
            let fine = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-./".contains(&b));
            if item.len() > 200 || !item.split_once(':').is_some_and(|(ns, path)| fine(ns) && fine(path)) {
                return Err("item must be an item model as namespace:path".into());
            }
            let head = on_creature && text("bone").unwrap_or("head") != "body";
            let offset: Vec<f32> = v.get("offset").and_then(serde_json::Value::as_array).map(|a| a.iter().map(|n| n.as_f64().unwrap_or(0.0).clamp(-8.0, 8.0) as f32).collect()).unwrap_or_default();
            let at = |i: usize| offset.get(i).copied().unwrap_or(0.0);
            let size = number("size").map_or(1.0, |s| s.clamp(0.01, 16.0) as f32);
            let turn = number("turn").map_or(0.0, |t| t as f32);
            let mode = match text("display") {
                Some(name) => display_mode(name).ok_or("display is one of head, fixed, none, ground, gui, hand")?,
                None if head => ItemDisplayMode::Head,
                None => ItemDisplayMode::Fixed,
            };
            Ok(Arc::new(item_data(item, on_creature, head, (at(0), at(1), at(2)), size, turn, mode)))
        }
        (None, None) => Err("model or item is missing".into()),
    }
}

fn ipc(st: &mut State, sender: &str, v: &serde_json::Value) -> Result<serde_json::Value, String> {
    let text = |key: &str| v.get(key).and_then(serde_json::Value::as_str);
    let number = |key: &str| v.get(key).and_then(serde_json::Value::as_f64);
    if matches!(text("kind"), Some("spawn" | "attach")) && st.instances.len() + st.pending.len() >= MAX_MODELS {
        return Err(format!("there are {MAX_MODELS} models in the world already"));
    }
    // Asked of a model that is still on its way onto a creature: it is done once it is there
    if let Some(waiting) = number("id").map(|n| n as u32).and_then(|id| st.pending.iter_mut().find(|p| p.id == id)) {
        if waiting.then.len() < 32 {
            waiting.then.push(v.clone());
        }
        return Ok(serde_json::json!({ "ok": true, "queued": true }));
    }
    let id = || number("id").map(|n| n as u32).filter(|id| st.instances.iter().any(|i| i.id == *id)).ok_or("no such model in the world");
    let ok = |extra: serde_json::Value| {
        let mut out = serde_json::json!({ "ok": true });
        if let (Some(a), Some(b)) = (out.as_object_mut(), extra.as_object()) {
            a.extend(b.clone());
        }
        Ok(out)
    };
    match text("kind").unwrap_or_default() {
        "models" => ok(serde_json::json!({ "models": st.models.iter().map(|d| serde_json::json!({ "name": d.model.name, "animations": d.model.animations.iter().map(|a| a.name.clone()).collect::<Vec<_>>(), "hitbox": d.model.hitbox.or(d.set.hitbox), "worn": d.worn })).collect::<Vec<_>>() })),
        "spawn" => {
            let data = asked_model(st, v, false)?;
            let (Some(x), Some(y), Some(z)) = (number("x"), number("y"), number("z")) else { return Err("x, y and z are missing".into()) };
            let world = text("world").unwrap_or("minecraft:overworld").to_owned();
            let leash = match text("leash") {
                Some(holder) => {
                    let hex: String = holder.chars().filter(|c| *c != '-').collect::<String>().to_ascii_lowercase();
                    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err("leash must be a player's UUID".into());
                    }
                    Some(hex)
                }
                None => None,
            };
            let new = add_placed(st, data, world, (x, y, z), number("yaw").unwrap_or(0.0) as f32, sender);
            if let Some(inst) = st.instances.iter_mut().find(|i| i.id == new) {
                if let Some(scale) = number("scale").filter(|s| (0.01..=100.0).contains(s)) {
                    inst.scale = scale as f32;
                }
                inst.animation = text("animation").map(str::to_owned);
                inst.tag_text = text("name").unwrap_or_default().chars().take(120).collect();
                inst.leash = leash.map(|holder| Leash { holder, fake: LEASH_BASE.wrapping_add(new as i32), marker: None, told: HashMap::new() });
            }
            ok(serde_json::json!({ "id": new }))
        }
        "attach" => {
            let data = asked_model(st, v, true)?;
            let entity = number("entity").ok_or("entity is missing")? as u32;
            // Finding the creature needs the server, which a message does not bring: the next
            // tick does it. The number is given out now, and what is asked of it waits.
            let new = st.next_id;
            st.next_id += 1;
            let flag = |key: &str| v.get(key).and_then(serde_json::Value::as_bool);
            st.pending.push(Pending {
                id: new,
                data,
                entity,
                owner: sender.to_owned(),
                scale: number("scale").filter(|s| (0.01..=100.0).contains(s)).map(|s| s as f32),
                name: text("name").unwrap_or_default().chars().take(120).collect(),
                then: Vec::new(),
                extra: flag("extra").unwrap_or(false),
                hide: flag("hide"),
            });
            ok(serde_json::json!({ "id": new }))
        }
        "play" => {
            let id = id()?;
            let inst = st.instances.iter_mut().find(|i| i.id == id).ok_or("gone")?;
            let animation = inst.data.model.animation(&rig::plain_name(text("animation").ok_or("animation is missing")?)).ok_or("no such animation")?;
            let mode = mode_of(text("mode").map(str::to_owned).as_ref());
            let blend = number("blend").map_or(inst.data.set.blend, |b| b.clamp(0.0, 100.0) as u32);
            inst.play(animation, mode, number("speed").map_or(1.0, |s| s.clamp(0.05, 20.0) as f32), blend, false);
            inst.due = 0;
            ok(serde_json::json!({ "ticks": inst.data.model.animations[animation].ticks }))
        }
        "stop" => {
            let id = id()?;
            let inst = st.instances.iter_mut().find(|i| i.id == id).ok_or("gone")?;
            let animation = text("animation").and_then(|n| inst.data.model.animation(&rig::plain_name(n)));
            let blend = inst.data.set.blend;
            inst.stop(animation, blend);
            ok(serde_json::json!({}))
        }
        "remove" => {
            let id = id()?;
            remove_instance(st, id);
            ok(serde_json::json!({}))
        }
        "tint" => {
            let id = id()?;
            let tick = st.tick;
            let inst = st.instances.iter_mut().find(|i| i.id == id).ok_or("gone")?;
            let rgb = text("colour").or_else(|| text("color")).and_then(defs::colour);
            set_tint(inst, rgb);
            inst.tint_until = tick + number("ticks").map_or(u64::MAX / 2, |t| t.max(1.0) as u64);
            ok(serde_json::json!({}))
        }
        "name" => {
            let id = id()?;
            let inst = st.instances.iter_mut().find(|i| i.id == id).ok_or("gone")?;
            inst.tag_text = text("name").unwrap_or_default().chars().take(120).collect();
            // A name that is there already only gets other words (a mob's health changes
            // with every hit); a first name or none makes the model's entities anew
            match inst.tag.as_ref().and_then(EntityDisplayExt::as_text_display).filter(|_| !inst.tag_text.is_empty()) {
                Some(tag) => tag.set_text(c::color(&inst.tag_text)),
                None if inst.shown() && inst.data.tag.is_some() => {
                    let mut boxes = std::mem::take(&mut st.boxes);
                    inst.hide(&mut boxes);
                    st.boxes = boxes;
                    inst.near = false;
                }
                None => {}
            }
            ok(serde_json::json!({ "shown": inst.data.tag.is_some() }))
        }
        "move" => {
            let id = id()?;
            let inst = st.instances.iter_mut().find(|i| i.id == id).ok_or("gone")?;
            let Anchor::Placed { pos, .. } = &mut inst.anchor else { return Err("it sits on a creature: move the creature".into()) };
            let to = (number("x").unwrap_or(pos.0), number("y").unwrap_or(pos.1), number("z").unwrap_or(pos.2));
            *pos = to;
            if let Some(yaw) = number("yaw") {
                inst.yaw = wrap_degrees(yaw as f32);
            }
            // The entities glide to the new place; the pose follows the new yaw
            for part in &inst.parts {
                part.entity.teleport(to, part.entity.get_world());
            }
            if let Some((entity, _)) = &inst.hit {
                entity.teleport(to, entity.get_world());
            }
            if let Some(entity) = &inst.tag {
                entity.teleport(to, entity.get_world());
            }
            inst.due = 0;
            ok(serde_json::json!({}))
        }
        "info" => {
            let id = id()?;
            let inst = st.instances.iter().find(|i| i.id == id).ok_or("gone")?;
            ok(serde_json::json!({ "model": inst.data.model.name, "shown": inst.shown(), "playing": inst.runs.iter().filter(|r| r.target > 0.0).map(|r| inst.data.model.animations[r.animation].name.clone()).collect::<Vec<_>>() }))
        }
        "watch" => Ok(watch(st, number("since").map_or(0, |n| n as u64), number("loaded").map_or(0, |n| n as u64))),
        other => Err(format!("unknown kind {other}")),
    }
}

/// What changed since the asker's last count (`since`), or everything when it asks for the
/// first time, from too long ago, or from before the files were last read.
fn watch(st: &mut State, since: u64, loaded: u64) -> serde_json::Value {
    let fresh = st.tick >= st.watch.until;
    st.watch.until = st.tick + 200;
    if fresh {
        // Nothing was noted while nobody asked: everything counts as changed now
        st.watch.gone.clear();
        st.watch.rev += 1;
        st.watch.floor = st.watch.rev;
        for inst in &mut st.instances {
            inst.told = inst.telling();
            inst.rev = st.watch.rev;
        }
    }
    let full = fresh || since == 0 || since < st.watch.floor || since > st.watch.rev || loaded != st.watch.loaded;
    let models: Vec<serde_json::Value> = st
        .instances
        .iter()
        .filter(|i| full || i.rev > since)
        .map(|i| {
            let (base, world, at, up) = match &i.anchor {
                Anchor::Placed { world, pos } => (None, Some(world.as_str()), Some(*pos), i.data.set.lift),
                Anchor::Riding { base_id, up, .. } => (Some(*base_id), None, None, *up),
            };
            serde_json::json!({
                "id": i.id,
                "model": i.data.model.name,
                "base": base,
                "world": world,
                "at": at.map(|p| [p.0, p.1, p.2]),
                "yaw": i.yaw,
                "up": up,
                "scale": i.scale,
                "name": i.tag_text,
                // Blocks above the model's feet at its own size
                "tag": i.data.tag.map(|b| i.data.model.bones[b].pivot.y / rig::UNITS),
                "tint": i.tint.is_some(),
                "playing": i.runs.iter().filter(|r| r.target > 0.0).map(|r| serde_json::json!([i.data.model.animations[r.animation].name, r.auto, r.nth])).collect::<Vec<_>>(),
            })
        })
        .collect();
    let gone: Vec<u32> = if full { Vec::new() } else { st.watch.gone.iter().filter(|(rev, _)| *rev > since).map(|(_, id)| *id).collect() };
    serde_json::json!({ "ok": true, "rev": st.watch.rev, "loaded": st.watch.loaded, "full": full, "models": models, "gone": gone })
}

// ---- the plugin ------------------------------------------------------------------

struct Models;

impl Plugin for Models {
    fn new() -> Self {
        Models
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Animated models from Blockbench files".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into(), NETWORK_TCP_BIND.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:command"), description: "Manage models".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let loaded = load();
        let mut st = State {
            cfg: Config::default(),
            models: Vec::new(),
            notes: Vec::new(),
            pack: None,
            web: None,
            web_error: None,
            players: HashMap::new(),
            instances: Vec::new(),
            pending: Vec::new(),
            waiting: Vec::new(),
            next_id: 1,
            boxes: HashMap::new(),
            bases: HashMap::new(),
            bodies: HashMap::new(),
            tick: 0,
            sent: 0,
            stat: Stat::default(),
            scratch: Vec::new(),
            turn: 0,
            swept: 0,
            watch: Watch::default(),
        };
        apply(&mut st, loaded);
        c::log_info(&format!(
            "{} model(s), {} placed, pack {}{}",
            st.models.len(),
            st.instances.len(),
            st.pack.as_ref().map_or("not built".to_owned(), |b| format!("{} KB", b.bytes.len() / 1024)),
            if st.notes.is_empty() { String::new() } else { format!(", {} thing(s) to look at (/models info)", st.notes.len()) }
        ));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(st);

        let cmd = Command::new(&["models".to_owned(), "model".to_owned()], "Animated models").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:command"));
        context.register_event_handler(OnClick, EventPriority::Normal, true)?;
        context.register_event_handler(OnHurt, EventPriority::Lowest, false)?;
        context.register_event_handler(OnDeath, EventPriority::Normal, false)?;
        context.register_event_handler(OnLeave, EventPriority::Normal, false)?;
        context.register_event_handler(OnStatus, EventPriority::Normal, false)?;
        context.register_event_handler(OnRespawn, EventPriority::Lowest, false)?;
        context.register_event_handler(OnWorld, EventPriority::Lowest, false)?;
        context.schedule_repeating_task(1, 1, on_tick);
        c::mark_running(VERSION);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        c::mark_stopped();
        // Nothing of ours stays in the world: a display's item is not saved, so what stayed
        // would come back as an empty entity
        if let Ok(mut guard) = STATE.lock()
            && let Some(st) = guard.as_mut()
        {
            save_placed(st);
            let ids: Vec<u32> = st.instances.iter().map(|i| i.id).collect();
            for id in ids {
                remove_instance(st, id);
            }
        }
        Ok(())
    }

    fn handle_ipc_message(&self, sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let mut guard = try_lock().ok_or("busy")?;
        let st = guard.as_mut().ok_or("not loaded")?;
        let answer = ipc(st, &sender, &v)?;
        serde_json::to_vec(&answer).map_err(|e| e.to_string())
    }
}

register_plugin!(Models);

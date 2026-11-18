//! Native Pumpkin glue: the pack on disk and in `pumpkin.toml`, what the Models plugin says
//! about its models, and the packets that show them to Bedrock players.
//!
//! Nothing here exists for the server or for Java players: a model is an entity only in the
//! eyes of each Bedrock client it was sent to. The creature a model sits on stays the real
//! entity (hidden by Models), so hits and everything else about it work as on Java.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use models::bbmodel;
use models::rig;
use pumpkin::command::argument_builder::{ArgumentBuilder, argument, command};
use pumpkin::command::argument_types::core::string::StringArgumentType;
use pumpkin::command::context::command_context::CommandContext;
use pumpkin::command::{CommandExecutor, CommandExecutorResult};
use pumpkin::entity::EntityBase;
use pumpkin::entity::player::Player;
use pumpkin::net::ClientPlatform;
use pumpkin::net::bedrock::BedrockClient;
use pumpkin::plugin::player::player_join::PlayerJoinEvent;
use pumpkin::plugin::server::server_tick_end::ServerTickEndEvent;
use pumpkin::plugin::{BoxFuture, Context, EventHandler, EventPriority};
use pumpkin::server::Server;
use pumpkin_nbt::Nbt;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::BClientPacket;
use pumpkin_protocol::bedrock::client::add_actor::CAddActor;
use pumpkin_protocol::bedrock::client::item_registry::ItemData;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::bedrock::client::move_actor_absolute::CMoveActorAbsolute;
use pumpkin_protocol::bedrock::client::remove_actor::CRemoveActor;
use pumpkin_protocol::bedrock::client::set_actor_data::{CSetActorData, MetadataValue, PropertySyncData, SyncedActorDataList, entity_data_flag, entity_data_key};
use pumpkin_protocol::codec::var_long::VarLong;
use pumpkin_protocol::codec::var_ulong::VarULong;
use pumpkin_protocol::serial::PacketWrite;
use pumpkin_util::PermissionLvl;
use pumpkin_util::math::vector2::Vector2;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::permission::{Permission, PermissionDefault};
use pumpkin_util::text::TextComponent;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids;
use crate::pack::{self, Info};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "§6[Bedrock]§r ";
/// The plugin whose models are shown, and where it keeps them.
const MODELS: &str = "models";
/// Entity numbers far above anything the server hands out itself: a model's is this plus
/// twice its number at Models, the name above it one more.
const FIRST_ID: i64 = 2_000_000_000;

static SERVER: OnceLock<Arc<Server>> = OnceLock::new();
static STATE: Mutex<Option<State>> = Mutex::new(None);

fn lock() -> std::sync::MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ---- settings ----------------------------------------------------------------------

const CONFIG: &str = r#"# Bedrock: shows the models of the Models plugin to Bedrock players.

# th or en: the language of the plugin's own messages.
language = "en"

# A Bedrock player sees a model that is this close (blocks).
view-distance = 48

# Keep the plugin's own pack in pumpkin.toml ([resource_pack.bedrock]) up to date. The server
# reads that file when it starts: after the models change, Bedrock players get the new pack
# once the server has been restarted. false: the plugin only says what the entry should be.
update-pumpkin-toml = true

# The id of the pack. Made once; leave it alone (a client keeps a pack it has by this id).
pack-id = ""
"#;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
struct Config {
    language: String,
    view_distance: u32,
    update_pumpkin_toml: bool,
    pack_id: String,
    /// Trial with a Pumpkin that reads `items.json`: what it switches on for clients.
    items_experiments: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "en".into(), view_distance: 48, update_pumpkin_toml: true, pack_id: String::new(), items_experiments: vec!["data_driven_items".into()] }
    }
}

// ---- packets Pumpkin does not have ---------------------------------------------------

/// `AvailableActorIdentifiers`: the entity identifiers the client accepts.
struct CActorIds(Vec<u8>);

impl pumpkin_protocol::Packet for CActorIds {
    const PACKET_ID: i32 = 119;
}

impl PacketWrite for CActorIds {
    fn write<W: std::io::Write>(&self, writer: &mut W) -> Result<(), std::io::Error> {
        writer.write_all(&self.0)
    }
}

/// `ActorEvent`: something happened to an entity (2 = it was hurt: the red flash).
struct CActorEvent {
    actor: u64,
    event: u8,
}

impl pumpkin_protocol::Packet for CActorEvent {
    const PACKET_ID: i32 = 27;
}

impl PacketWrite for CActorEvent {
    fn write<W: std::io::Write>(&self, writer: &mut W) -> Result<(), std::io::Error> {
        VarULong(self.actor).write(writer)?;
        self.event.write(writer)?;
        // The event's own number: none
        writer.write_all(&[0])
    }
}

// ---- state -------------------------------------------------------------------------

/// The pack as it is on disk, and whether the server sends it.
struct Built {
    uuid: String,
    version: String,
    size: u64,
    /// The server started with exactly this pack in its settings.
    sent: bool,
    notes: Vec<(String, String)>,
}

/// A model in the world, as Models last described it.
struct Inst {
    model: String,
    /// The creature it sits on (entity number), or where it stands.
    base: Option<i32>,
    entity: Option<Arc<dyn EntityBase>>,
    /// The tick the creature was last looked for.
    sought: u64,
    world: String,
    at: (f64, f64, f64),
    yaw: f32,
    /// Blocks from the anchor's feet up to the model's feet.
    up: f32,
    scale: f32,
    name: String,
    /// Blocks from the model's feet to its name, at the model's own size.
    tag: Option<f32>,
    tint: bool,
    /// Name, started by Models itself, and which start of it this is.
    playing: Vec<(String, bool, u64)>,
}

/// Where a model is this tick.
#[derive(Clone, Copy, PartialEq)]
struct Now {
    world: &'static str,
    pos: (f32, f32, f32),
    /// Pitch, yaw and head yaw as the bytes the game sends.
    turn: (u8, u8, u8),
}

/// What one Bedrock player was last told about one model.
struct Seen {
    now: Now,
    scale: f32,
    name: String,
    tint: bool,
    acts: Acts,
}

/// The animations of a model as the two numbers its entity carries (see `pack::entity`): the
/// one that runs all the time, and the one that was asked for with which start of it this is.
/// Both count the model's animations from 1; 0 is none.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Acts {
    auto: i32,
    asked: i32,
    nth: u64,
}

fn acts(info: &Info, inst: &Inst) -> Acts {
    let number = |name: &str| info.animations.iter().position(|(n, _)| n == name).map_or(0, |i| i as i32 + 1);
    let auto = inst.playing.iter().find(|p| p.1).map_or(0, |p| number(&p.0));
    // Of several that were asked for, the one asked for last
    let (asked, nth) = inst.playing.iter().filter(|p| !p.1).max_by_key(|p| p.2).map_or((0, 0), |p| (number(&p.0), p.2));
    Acts { auto, asked, nth }
}

/// An item of the CustomItems plugin as a Bedrock client is told about it.
struct OurItem {
    id: String,
    /// Its number in the client's list of items: above every item of the game.
    number: i16,
    name: String,
    handheld: bool,
    max_stack: u8,
    glint: bool,
}

/// The first number of our items; the game's own end far below.
const FIRST_ITEM: i16 = 20_000;

/// The items of CustomItems that have a picture of their own, read the way it reads them.
fn read_items(dir: &Path) -> (Vec<OurItem>, Vec<pack::ItemPicture>) {
    use customitems::defs::{self, Look};
    let folder = dir.parent().map_or_else(|| PathBuf::from(pack::ITEMS), |p| p.join(pack::ITEMS));
    let mut files: Vec<String> = std::fs::read_dir(folder.join("items")).map(|d| d.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.ends_with(".toml")).collect()).unwrap_or_default();
    files.sort();
    let mut problems = Vec::new();
    let mut all = Vec::new();
    for file in files {
        if let Ok(text) = std::fs::read_to_string(folder.join("items").join(&file)) {
            all.extend(defs::parse_items(&file, &text, &mut problems));
        }
    }
    let mut items = defs::merge_items(all, &mut problems);
    items.sort_by(|a, b| a.id.cmp(&b.id));
    let (mut ours, mut pictures) = (Vec::new(), Vec::new());
    for item in items {
        let Look::Texture { file, handheld } = &item.look else { continue };
        let Ok(png) = std::fs::read(folder.join("textures").join(file)) else { continue };
        if !mcsv_pack::is_png(&png) || ours.len() >= 10_000 {
            continue;
        }
        pictures.push(pack::ItemPicture { id: item.id.clone(), png });
        ours.push(OurItem { number: FIRST_ITEM + ours.len() as i16, name: coloured(&item.name), handheld: *handheld, max_stack: item.max_stack, glint: item.glint, id: item.id });
    }
    (ours, pictures)
}

/// What the client's list of items says about one of ours.
fn item_entry(item: &OurItem) -> ItemData {
    let mut textures = NbtCompound::new();
    textures.put_string("default", pack::item_icon(&item.id));
    let mut icon = NbtCompound::new();
    icon.put_compound("textures", textures);
    let mut properties = NbtCompound::new();
    properties.put_compound("minecraft:icon", icon);
    properties.put_bool("allow_off_hand", true);
    properties.put_bool("hand_equipped", item.handheld);
    properties.put_int("max_stack_size", if item.max_stack > 0 { i32::from(item.max_stack) } else if item.handheld { 1 } else { 64 });
    properties.put_bool("foil", item.glint);
    properties.put_int("creative_category", 4);
    let mut name = NbtCompound::new();
    name.put_string("value", item.name.clone());
    let mut components = NbtCompound::new();
    components.put_compound("item_properties", properties);
    components.put_compound("minecraft:display_name", name);
    let mut root = NbtCompound::new();
    root.put_compound("components", components);
    root.put_int("id", i32::from(item.number));
    root.put_string("name", pack::item_identifier(&item.id));
    ItemData { item_name: pack::item_identifier(&item.id), item_id: item.number, is_component_based: true, item_version: VarInt(1), component_data: Nbt { name: String::new(), root_tag: root }.write_bedrock().to_vec() }
}

fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = u32::from(chunk[0]) << 16 | u32::from(*chunk.get(1).unwrap_or(&0)) << 8 | u32::from(*chunk.get(2).unwrap_or(&0));
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            out.push(if i <= chunk.len() { ABC[(n >> shift & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// Trial: the list of our items for a Pumpkin that was changed to read it (the server's own
/// list of items and every item it sends are out of a plugin's reach). `items.json` in the
/// data folder; a Pumpkin that does not know the file is not bothered by it.
fn write_items_file(dir: &Path, items: &[OurItem], experiments: &[String]) {
    let list: Vec<Value> = items
        .iter()
        .map(|item| {
            let entry = item_entry(item);
            serde_json::json!({ "name": entry.item_name, "id": entry.item_id, "model": pack::item_identifier(&item.id), "components": base64(&entry.component_data) })
        })
        .collect();
    let text = serde_json::json!({ "experiments": experiments, "items": list }).to_string();
    let file = dir.join("items.json");
    if std::fs::read_to_string(&file).ok().as_deref() != Some(text.as_str()) {
        write_atomic(&file, text.as_bytes());
    }
}

#[derive(Default)]
struct Viewer {
    /// The client knows our identifiers: it joined while the plugin was there.
    ready: bool,
    world: &'static str,
    seen: HashMap<u32, Seen>,
}

struct State {
    dir: PathBuf,
    cfg: Config,
    models: HashMap<String, Info>,
    items: Vec<OurItem>,
    /// The body of the identifier list every Bedrock player gets when joining.
    ids: Option<Vec<u8>>,
    built: Option<Built>,
    /// Bedrock is on in pumpkin.toml.
    on: bool,
    insts: HashMap<u32, Inst>,
    viewers: HashMap<String, Viewer>,
    tick: u64,
    /// Models' counters as last heard.
    rev: u64,
    loaded: Option<u64>,
    /// Models answered the last time it was asked.
    heard: bool,
    sent: u64,
}

impl State {
    fn thai(&self) -> bool {
        self.cfg.language == "th"
    }
}

// ---- files -------------------------------------------------------------------------

fn write_atomic(path: &Path, data: &[u8]) -> bool {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data).is_ok() && std::fs::rename(&tmp, path).is_ok()
}

fn load_config(dir: &Path) -> Config {
    let path = dir.join("config.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        let _ = std::fs::write(&path, CONFIG);
        CONFIG.to_owned()
    });
    let mut cfg: Config = toml::from_str(&text).unwrap_or_else(|e| {
        tracing::warn!("Bedrock config.toml: {e}; using defaults");
        Config::default()
    });
    cfg.view_distance = cfg.view_distance.clamp(8, 160);
    if !pack::usable_id(&cfg.pack_id) {
        // Made once from the clock: all it has to be is different from server to server
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) ^ u64::from(std::process::id()).rotate_left(40);
        cfg.pack_id = pack::pack_id(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let line = format!("pack-id = \"{}\"", cfg.pack_id);
        let with = if text.lines().any(|l| l.trim_start().starts_with("pack-id")) { text.lines().map(|l| if l.trim_start().starts_with("pack-id") { line.as_str() } else { l }).collect::<Vec<_>>().join("\n") + "\n" } else { format!("{}\n{line}\n", text.trim_end()) };
        write_atomic(&path, with.as_bytes());
    }
    cfg
}

/// Every model Models would load, read the way it reads them.
fn read_models(dir: &Path) -> Vec<rig::Model> {
    let folder = dir.parent().map_or_else(|| PathBuf::from(MODELS), |p| p.join(MODELS)).join("models");
    let mut names: Vec<String> = std::fs::read_dir(&folder).map(|d| d.flatten().filter_map(|e| e.file_name().into_string().ok()).collect()).unwrap_or_default();
    names.sort();
    let mut out: Vec<rig::Model> = Vec::new();
    for file in names {
        let Some(stem) = file.strip_suffix(".bbmodel") else { continue };
        let Ok(text) = std::fs::read_to_string(folder.join(&file)) else { continue };
        // What is wrong with a file Models says itself: here it is only left out
        if let Ok(model) = bbmodel::parse(&text).and_then(|raw| rig::build(stem, &raw))
            && !out.iter().any(|m| m.name == model.name)
        {
            out.push(model);
        }
    }
    out
}

/// The path of the pack as pumpkin.toml names it: from the server's folder.
fn pack_path(dir: &Path) -> String {
    dir.join(pack::FILE).to_string_lossy().replace('\\', "/")
}

/// Make `[resource_pack.bedrock]` in pumpkin.toml name the pack as it is now. `Ok(true)` when
/// the file was changed.
fn update_toml(dir: &Path, built: &Built) -> Result<bool, String> {
    let file = Path::new("pumpkin.toml");
    let text = std::fs::read_to_string(file).map_err(|e| format!("pumpkin.toml: {e}"))?;
    let mut root: toml::Table = text.parse().map_err(|e| format!("pumpkin.toml: {e}"))?;
    let path = pack_path(dir);
    let table = |value: &mut toml::Value| -> Result<(), String> {
        if !value.is_table() {
            return Err("pumpkin.toml: resource_pack is not a table".into());
        }
        Ok(())
    };
    let packs = root.entry("resource_pack").or_insert_with(|| toml::Value::Table(toml::Table::new()));
    table(packs)?;
    let bedrock = packs.as_table_mut().ok_or("pumpkin.toml")?.entry("bedrock").or_insert_with(|| toml::Value::Table(toml::Table::new()));
    table(bedrock)?;
    let bedrock = bedrock.as_table_mut().ok_or("pumpkin.toml")?;
    let before = bedrock.clone();
    bedrock.insert("enabled".into(), toml::Value::Boolean(true));
    let list = bedrock.entry("packs").or_insert_with(|| toml::Value::Array(Vec::new()));
    let list = list.as_array_mut().ok_or("pumpkin.toml: packs is not a list")?;
    let mut entry = toml::Table::new();
    entry.insert("uuid".into(), toml::Value::String(built.uuid.clone()));
    entry.insert("version".into(), toml::Value::String(built.version.clone()));
    entry.insert("size".into(), toml::Value::Integer(built.size as i64));
    entry.insert("download_url".into(), toml::Value::String(String::new()));
    entry.insert("path".into(), toml::Value::String(path.clone()));
    let ours = |v: &toml::Value| v.get("path").and_then(toml::Value::as_str) == Some(path.as_str()) || v.get("uuid").and_then(toml::Value::as_str) == Some(built.uuid.as_str());
    let first = match list.iter_mut().find(|v| ours(v)) {
        Some(old) => {
            *old = toml::Value::Table(entry);
            false
        }
        None => {
            list.push(toml::Value::Table(entry));
            true
        }
    };
    if first {
        // Without the pack a model is nothing at all to a Bedrock player: the pack is asked
        // of everybody when it is first put in. The owner may turn that off again.
        bedrock.insert("force".into(), toml::Value::Boolean(true));
    }
    if *bedrock == before {
        return Ok(false);
    }
    if !write_atomic(file, root.to_string().as_bytes()) {
        return Err("pumpkin.toml could not be written".into());
    }
    Ok(true)
}

/// Read the models, write the pack, and see whether the server sends it. Says what the owner
/// has to know.
fn rebuild(st: &mut State, server: &Server) -> Vec<String> {
    let th = st.thai();
    let mut out = Vec::new();
    let models = read_models(&st.dir);
    let (items, pictures) = read_items(&st.dir);
    write_items_file(&st.dir, &items, &st.cfg.items_experiments);
    st.items = items;
    let made = pack::build(&models, &pictures, &st.cfg.pack_id);
    st.ids = ids::list(&made.models.iter().map(|m| pack::identifier(&m.name)).collect::<Vec<_>>());
    st.models = made.models.iter().map(|m| (m.name.clone(), m.clone())).collect();
    let file = st.dir.join(pack::FILE);
    if std::fs::read(&file).ok().as_deref() != Some(made.bytes.as_slice()) && !write_atomic(&file, &made.bytes) {
        out.push(if th { format!("เขียน {} ไม่ได้", file.display()) } else { format!("{} could not be written", file.display()) });
    }
    let settings = &server.advanced_config.resource_pack.bedrock;
    let sent = settings.enabled && settings.packs.iter().any(|p| p.uuid.to_string() == made.uuid && p.version == made.version && p.size == made.bytes.len() as u64);
    let built = Built { uuid: made.uuid, version: made.version, size: made.bytes.len() as u64, sent, notes: made.notes };
    if !st.on {
        out.push(if th { "Bedrock ปิดอยู่ใน pumpkin.toml ([networking.bedrock] enabled) ปลั๊กอินนี้จึงยังไม่มีอะไรให้ทำ".into() } else { "Bedrock is off in pumpkin.toml ([networking.bedrock] enabled): nothing for this plugin to do yet".into() });
    } else if models.is_empty() && st.items.is_empty() {
        out.push(if th { "ยังไม่มีโมเดลของปลั๊กอิน Models".into() } else { "the Models plugin has no models yet".into() });
    } else if !sent {
        let entry = format!("{{ uuid = \"{}\", version = \"{}\", size = {}, download_url = \"\", path = \"{}\" }}", built.uuid, built.version, built.size, pack_path(&st.dir));
        match if st.cfg.update_pumpkin_toml { update_toml(&st.dir, &built) } else { Ok(false) } {
            Ok(_) if st.cfg.update_pumpkin_toml => out.push(if th { "โมเดลเปลี่ยน: ใส่ pack ใหม่ลง pumpkin.toml แล้ว รีสตาร์ตเซิร์ฟหนึ่งครั้งเพื่อให้ผู้เล่น Bedrock ได้รับ".into() } else { "the models changed: pumpkin.toml names the new pack now; restart the server once for Bedrock players to get it".into() }),
            Ok(_) => out.push(if th { format!("ผู้เล่น Bedrock ยังไม่ได้รับ pack นี้ ใส่ใน [resource_pack.bedrock] ของ pumpkin.toml แล้วรีสตาร์ต: enabled = true และ packs = [{entry}]") } else { format!("Bedrock players do not get this pack yet. Put it into [resource_pack.bedrock] of pumpkin.toml and restart: enabled = true and packs = [{entry}]") }),
            Err(e) => out.push(e),
        }
    }
    for (model, note) in &built.notes {
        out.push(format!("{model}: {note}"));
    }
    st.built = Some(built);
    out
}

// ---- what Models says --------------------------------------------------------------

/// Take in an answer of Models to `watch`. `true` when its files were read again since the
/// last answer: the pack may be another one now.
fn take(st: &mut State, v: &Value) -> bool {
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        return false;
    }
    let number = |v: &Value, key: &str| v.get(key).and_then(Value::as_f64);
    if v.get("full").and_then(Value::as_bool) == Some(true) {
        st.insts.clear();
    }
    for id in v.get("gone").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_u64) {
        st.insts.remove(&(id as u32));
    }
    for m in v.get("models").and_then(Value::as_array).into_iter().flatten() {
        let (Some(id), Some(model)) = (number(m, "id"), m.get("model").and_then(Value::as_str)) else { continue };
        let base = number(m, "base").map(|b| b as i32);
        let at = m.get("at").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_f64).collect::<Vec<_>>()).filter(|a| a.len() == 3).map_or((0.0, 0.0, 0.0), |a| (a[0], a[1], a[2]));
        let playing = m.get("playing").and_then(Value::as_array).into_iter().flatten().filter_map(|p| Some((p.get(0)?.as_str()?.to_owned(), p.get(1)?.as_bool()?, p.get(2).and_then(Value::as_u64).unwrap_or(0)))).collect();
        // The creature found for it stays found while it is the same one
        let (entity, sought) = st.insts.get(&(id as u32)).filter(|old| old.base == base).map_or((None, 0), |old| (old.entity.clone(), old.sought));
        st.insts.insert(
            id as u32,
            Inst {
                model: model.to_owned(),
                base,
                entity,
                sought,
                world: m.get("world").and_then(Value::as_str).unwrap_or_default().to_owned(),
                at,
                yaw: number(m, "yaw").unwrap_or(0.0) as f32,
                up: number(m, "up").unwrap_or(0.0) as f32,
                scale: number(m, "scale").unwrap_or(1.0) as f32,
                name: m.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                tag: number(m, "tag").map(|t| t as f32),
                tint: m.get("tint").and_then(Value::as_bool).unwrap_or(false),
                playing,
            },
        );
    }
    st.rev = number(v, "rev").map_or(st.rev, |n| n as u64);
    let loaded = number(v, "loaded").map(|n| n as u64);
    // The first answer counts too: Models may have made its example after this plugin looked
    let again = loaded != st.loaded;
    st.loaded = loaded;
    again
}

// ---- showing -----------------------------------------------------------------------

fn bedrock(player: &Player) -> Option<&Arc<BedrockClient>> {
    match player.client.as_ref() {
        ClientPlatform::Bedrock(client) => Some(client),
        ClientPlatform::Java(_) => None,
    }
}

fn send<T: BClientPacket>(client: &BedrockClient, packet: &T) -> bool {
    match client.serialize_packet(packet) {
        Ok(data) => {
            client.try_enqueue_packet(data);
            true
        }
        Err(_) => false,
    }
}

fn byte(degrees: f32) -> u8 {
    (degrees.rem_euclid(360.0) * 256.0 / 360.0).round() as i64 as u8
}

fn degrees(byte: u8) -> f32 {
    f32::from(byte) * 360.0 / 256.0
}

fn wrap(a: f32) -> f32 {
    let r = a.rem_euclid(360.0);
    if r > 180.0 { r - 360.0 } else { r }
}

/// `&a` and the like are how the owner writes colours; the game wants `§a`.
fn coloured(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' && chars.peek().is_some_and(|n| n.is_ascii_hexdigit() || "klmnor".contains(n.to_ascii_lowercase())) {
            out.push('§');
        } else {
            out.push(c);
        }
    }
    out
}

fn model_actor(id: u32) -> i64 {
    FIRST_ID + i64::from(id) * 2
}

fn name_actor(id: u32) -> i64 {
    model_actor(id) + 1
}

fn add(kind: String, actor: i64, now: Now, data: SyncedActorDataList) -> CAddActor {
    CAddActor {
        target_actor_id: VarLong(actor),
        target_runtime_id: VarULong(actor as u64),
        actor_type: kind,
        position: Vector3::new(now.pos.0, now.pos.1, now.pos.2),
        velocity: Vector3::new(0.0, 0.0, 0.0),
        rotation: Vector2::new(degrees(now.turn.0), degrees(now.turn.1)),
        y_head_rotation: degrees(now.turn.2),
        y_body_rotation: degrees(now.turn.1),
        attributes_list: Vec::new(),
        actor_data: data,
        synced_properties: PropertySyncData { int_entries_list: HashMap::new(), float_entries_list: HashMap::new() },
        actor_links: Vec::new(),
        passenger_of_block: None,
    }
}

/// What a model's entity is: its size, and a box so small that a hit goes through it to the
/// creature below.
fn model_data(scale: f32, acts: Acts) -> SyncedActorDataList {
    let mut data = SyncedActorDataList::new();
    data.set(entity_data_key::FLAGS, MetadataValue::Int64(0));
    data.set(entity_data_key::FLAGS_TWO, MetadataValue::Int64(0));
    data.set(entity_data_key::VARIANT, MetadataValue::Int(acts.auto));
    data.set(entity_data_key::MARK_VARIANT, MetadataValue::Int(acts.asked));
    data.set(entity_data_key::SCALE, MetadataValue::Float(scale));
    data.set(entity_data_key::WIDTH, MetadataValue::Float(0.02));
    data.set(entity_data_key::HEIGHT, MetadataValue::Float(0.02));
    data
}

/// A name in the air: an armour stand too small to be seen, whose name always shows.
fn name_data(name: &str) -> SyncedActorDataList {
    let mut data = SyncedActorDataList::new();
    data.set(entity_data_key::FLAGS, MetadataValue::Int64((1 << entity_data_flag::SHOW_NAME) | (1 << entity_data_flag::ALWAYS_SHOW_NAME)));
    data.set(entity_data_key::FLAGS_TWO, MetadataValue::Int64(0));
    data.set(entity_data_key::NAME, MetadataValue::String(coloured(name)));
    data.set(entity_data_key::ALWAYS_SHOW_NAME_TAG, MetadataValue::Byte(1));
    data.set(entity_data_key::SCALE, MetadataValue::Float(0.0));
    data.set(entity_data_key::WIDTH, MetadataValue::Float(0.0));
    data.set(entity_data_key::HEIGHT, MetadataValue::Float(0.0));
    data
}

fn set_data(actor: i64, data: SyncedActorDataList) -> CSetActorData {
    CSetActorData { target_runtime_id: VarULong(actor as u64), actor_data: data, synced_properties: PropertySyncData { int_entries_list: HashMap::new(), float_entries_list: HashMap::new() }, tick: VarULong(0) }
}

fn moved(actor: i64, now: Now, far: bool) -> CMoveActorAbsolute {
    CMoveActorAbsolute {
        actor_runtime_id: VarULong(actor as u64),
        // A long way at once is a jump, not a glide
        header: if far { CMoveActorAbsolute::FLAG_TELEPORT } else { 0 },
        position: Vector3::new(now.pos.0, now.pos.1, now.pos.2),
        rotation_x: now.turn.0,
        rotation_y: now.turn.1,
        rotation_y_head: now.turn.2,
    }
}

/// Where the name of a model is when the model is at `now`.
fn name_place(inst: &Inst, now: Now) -> Option<Now> {
    let tag = inst.tag.filter(|_| !inst.name.is_empty())?;
    Some(Now { world: now.world, pos: (now.pos.0, now.pos.1 + tag * inst.scale, now.pos.2), turn: (0, 0, 0) })
}

/// Where every model is now. A model on a creature that cannot be found is nowhere.
fn places(st: &mut State, server: &Server) -> HashMap<u32, Now> {
    let tick = st.tick;
    let mut out = HashMap::with_capacity(st.insts.len());
    for (id, inst) in &mut st.insts {
        let head = st.models.get(&inst.model).is_some_and(|m| m.head);
        let now = match inst.base {
            None => Now { world: server.worlds.load().iter().find(|w| w.dimension.minecraft_name == inst.world).map_or("", |w| w.dimension.minecraft_name), pos: (inst.at.0 as f32, inst.at.1 as f32 + inst.up, inst.at.2 as f32), turn: (0, byte(inst.yaw), byte(inst.yaw)) },
            Some(base) => {
                if inst.entity.is_none() && tick >= inst.sought {
                    // Not every tick: looking through the worlds for something that is gone costs
                    inst.sought = tick + 20;
                    inst.entity = server.worlds.load().iter().find_map(|w| w.get_entity_by_id(base).or_else(|| w.get_player_by_id(base).map(|p| p as Arc<dyn EntityBase>)));
                }
                let Some(entity) = inst.entity.as_ref().map(|e| e.get_entity()) else { continue };
                let (pos, yaw) = (entity.pos.load(), entity.yaw.load());
                let (pitch, look) = if head { (entity.pitch.load().clamp(-60.0, 60.0), yaw + wrap(entity.head_yaw.load() - yaw).clamp(-75.0, 75.0)) } else { (0.0, yaw) };
                Now { world: entity.world.load().dimension.minecraft_name, pos: (pos.x as f32, pos.y as f32 + inst.up, pos.z as f32), turn: (byte(pitch), byte(yaw), byte(look)) }
            }
        };
        out.insert(*id, now);
    }
    out
}

/// Bring what every Bedrock player sees in line with where the models are and what they do.
fn show(st: &mut State, server: &Server) {
    let players: Vec<Arc<Player>> = server.get_all_players().into_iter().filter(|p| bedrock(p).is_some()).collect();
    st.viewers.retain(|name, _| players.iter().any(|p| p.gameprofile.name == *name));
    if players.is_empty() {
        return;
    }
    let places = places(st, server);
    let view = st.cfg.view_distance as f32;
    let mut sent = 0u64;
    for player in &players {
        let (Some(client), Some(viewer)) = (bedrock(player), st.viewers.get_mut(&player.gameprofile.name)) else { continue };
        if !viewer.ready {
            continue;
        }
        let world = player.world().dimension.minecraft_name;
        if viewer.world != world {
            // Another world: the client dropped every entity it had
            viewer.world = world;
            viewer.seen.clear();
        }
        let eye = player.position();
        let eye = (eye.x as f32, eye.y as f32, eye.z as f32);
        for (id, inst) in &st.insts {
            let Some(info) = st.models.get(&inst.model) else { continue };
            let was = viewer.seen.contains_key(id);
            let near = places.get(id).filter(|now| now.world == world).filter(|now| {
                let d = ((now.pos.0 - eye.0).powi(2) + (now.pos.1 - eye.1).powi(2) + (now.pos.2 - eye.2).powi(2)).sqrt();
                // A little further to leave than to come, so a player at the edge does not flicker it
                d <= if was { view + 8.0 } else { view }
            });
            let acts = acts(info, inst);
            match (near, viewer.seen.get_mut(id)) {
                (Some(now), None) => {
                    send(client, &add(pack::identifier(&info.name), model_actor(*id), *now, model_data(inst.scale, acts)));
                    if let Some(place) = name_place(inst, *now) {
                        send(client, &add("minecraft:armor_stand".to_owned(), name_actor(*id), place, name_data(&inst.name)));
                    }
                    sent += 1;
                    viewer.seen.insert(*id, Seen { now: *now, scale: inst.scale, name: inst.name.clone(), tint: inst.tint, acts });
                }
                (Some(now), Some(seen)) => {
                    if seen.now != *now {
                        let far = (now.pos.0 - seen.now.pos.0).abs() + (now.pos.1 - seen.now.pos.1).abs() + (now.pos.2 - seen.now.pos.2).abs() > 8.0;
                        send(client, &moved(model_actor(*id), *now, far));
                        if let Some(place) = name_place(inst, *now).filter(|_| !seen.name.is_empty()) {
                            send(client, &moved(name_actor(*id), place, far));
                        }
                        seen.now = *now;
                        sent += 1;
                    }
                    if seen.scale != inst.scale {
                        let mut data = SyncedActorDataList::new();
                        data.set(entity_data_key::SCALE, MetadataValue::Float(inst.scale));
                        send(client, &set_data(model_actor(*id), data));
                        seen.scale = inst.scale;
                        sent += 1;
                    }
                    if seen.name != inst.name {
                        match (seen.name.is_empty(), name_place(inst, *now)) {
                            (true, Some(place)) => {
                                send(client, &add("minecraft:armor_stand".to_owned(), name_actor(*id), place, name_data(&inst.name)));
                            }
                            (false, Some(_)) => {
                                let mut data = SyncedActorDataList::new();
                                data.set(entity_data_key::NAME, MetadataValue::String(coloured(&inst.name)));
                                send(client, &set_data(name_actor(*id), data));
                            }
                            (false, None) => {
                                send(client, &CRemoveActor::new(VarLong(name_actor(*id))));
                            }
                            (true, None) => {}
                        }
                        seen.name.clone_from(&inst.name);
                        sent += 1;
                    }
                    if inst.tint && !seen.tint {
                        send(client, &CActorEvent { actor: model_actor(*id) as u64, event: 2 });
                        sent += 1;
                    }
                    seen.tint = inst.tint;
                    if seen.acts != acts {
                        // The same animation asked for again: its number goes away for a tick,
                        // so the client starts it from the beginning when it is back
                        let again = seen.acts.asked == acts.asked && acts.asked != 0 && seen.acts.nth != acts.nth;
                        let next = if again { Acts { asked: 0, ..acts } } else { acts };
                        let mut data = SyncedActorDataList::new();
                        if seen.acts.auto != next.auto {
                            data.set(entity_data_key::VARIANT, MetadataValue::Int(next.auto));
                        }
                        if seen.acts.asked != next.asked {
                            data.set(entity_data_key::MARK_VARIANT, MetadataValue::Int(next.asked));
                        }
                        if !data.0.is_empty() {
                            send(client, &set_data(model_actor(*id), data));
                            sent += 1;
                        }
                        seen.acts = next;
                    }
                }
                (None, Some(seen)) => {
                    send(client, &CRemoveActor::new(VarLong(model_actor(*id))));
                    if !seen.name.is_empty() && inst.tag.is_some() {
                        send(client, &CRemoveActor::new(VarLong(name_actor(*id))));
                    }
                    viewer.seen.remove(id);
                    sent += 1;
                }
                (None, None) => {}
            }
        }
        // Models that are no more
        viewer.seen.retain(|id, seen| {
            if st.insts.contains_key(id) {
                return true;
            }
            send(client, &CRemoveActor::new(VarLong(model_actor(*id))));
            if !seen.name.is_empty() {
                send(client, &CRemoveActor::new(VarLong(name_actor(*id))));
            }
            sent += 1;
            false
        });
    }
    st.sent += sent;
}

// ---- events ------------------------------------------------------------------------

/// A Bedrock player has just been let into the world (the server has sent `StartGame` by now;
/// what is sent before that the client drops): from here on it knows our identifiers.
struct Joined;

impl EventHandler<PlayerJoinEvent> for Joined {
    fn handle<'a>(&'a self, _server: &'a Arc<Server>, event: &'a PlayerJoinEvent) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let Some(client) = bedrock(&event.player) else { return };
            let mut guard = lock();
            let Some(st) = guard.as_mut() else { return };
            let ready = st.ids.as_ref().is_some_and(|list| send(client, &CActorIds(list.clone())));
            st.viewers.insert(event.player.gameprofile.name.clone(), Viewer { ready, world: "", seen: HashMap::new() });
        })
    }
}

struct Tick;

impl EventHandler<ServerTickEndEvent> for Tick {
    fn handle<'a>(&'a self, server: &'a Arc<Server>, _event: &'a ServerTickEndEvent) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            // Every other tick Models is asked what changed; nobody on Bedrock, nobody asks
            let ask = {
                let mut guard = lock();
                let Some(st) = guard.as_mut() else { return };
                st.tick += 1;
                (st.on && st.tick % 2 == 0 && (!st.viewers.is_empty() || st.tick % 100 == 0)).then(|| serde_json::json!({ "kind": "watch", "since": st.rev, "loaded": st.loaded.unwrap_or(0) }).to_string())
            };
            let answer = match ask {
                Some(message) => Some(match server.plugin_manager.send_message("bedrock", MODELS, message.as_bytes()).await {
                    Ok(Ok(bytes)) => serde_json::from_slice::<Value>(&bytes).ok(),
                    _ => None,
                }),
                None => None,
            };
            let mut guard = lock();
            let Some(st) = guard.as_mut() else { return };
            if let Some(answer) = answer {
                st.heard = answer.is_some();
                match answer {
                    Some(v) => {
                        if take(st, &v) {
                            // Said only when the pack came out different: the first answer
                            // after a start usually finds the pack it was started with
                            let before = st.built.as_ref().map(|b| b.version.clone());
                            let lines = rebuild(st, server);
                            if st.built.as_ref().map(|b| b.version.clone()) != before {
                                for line in lines {
                                    tracing::info!("{line}");
                                }
                            }
                        }
                    }
                    // Models is not there (any more): neither are its models
                    None => st.insts.clear(),
                }
            }
            if st.on {
                show(st, server);
            }
        })
    }
}

// ---- command -----------------------------------------------------------------------

fn run(st: &mut State, server: &Server, words: &[String]) -> Vec<String> {
    let th = st.thai();
    match words.first().map(String::as_str) {
        None | Some("status") => {
            let players = server.get_all_players();
            let on_bedrock = players.iter().filter(|p| bedrock(p).is_some()).count();
            let shown: usize = st.viewers.values().map(|v| v.seen.len()).sum();
            let mut out = vec![if th { format!("{P}Bedrock {VERSION}: ผู้เล่น Bedrock {on_bedrock} คน, โมเดลในโลก {} ตัว, กำลังแสดง {shown}, ส่งไปแล้ว {} ครั้ง", st.insts.len(), st.sent) } else { format!("{P}Bedrock {VERSION}: {on_bedrock} Bedrock player(s), {} model(s) in the world, {shown} shown, {} update(s) sent", st.insts.len(), st.sent) }];
            if !st.on {
                out.push(if th { format!("{P}§eBedrock ปิดอยู่ใน pumpkin.toml") } else { format!("{P}§eBedrock is off in pumpkin.toml") });
            }
            if !st.heard {
                out.push(if th { format!("{P}§eยังไม่ได้ยินจากปลั๊กอิน Models (ต้องมี Models รุ่นที่รองรับ)") } else { format!("{P}§ethe Models plugin has not answered (it has to be a version that knows this plugin)") });
            }
            if let Some(built) = &st.built {
                let state = match (built.sent, th) {
                    (true, true) => "§aเซิร์ฟส่งให้ผู้เล่นอยู่",
                    (true, false) => "§athe server sends it",
                    (false, true) => "§eยังไม่ถูกส่ง: ต้องรีสตาร์ตเซิร์ฟ",
                    (false, false) => "§enot sent yet: the server has to be restarted",
                };
                out.push(format!("{P}pack {} ({} KB, {}): {state}", built.version, built.size.div_ceil(1024), st.models.len()));
            }
            out
        }
        Some("reload") => {
            st.cfg = load_config(&st.dir);
            let mut out = rebuild(st, server).into_iter().map(|l| format!("{P}{l}")).collect::<Vec<_>>();
            // Everybody is told everything again
            st.rev = 0;
            out.push(if th { format!("{P}อ่านโมเดลใหม่แล้ว: {} แบบ", st.models.len()) } else { format!("{P}read the models again: {}", st.models.len()) });
            out
        }
        // What the items file (for a Pumpkin that reads it) says
        Some("items") => {
            let mut out = vec![format!("{P}items: {} of ours in items.json", st.items.len())];
            out.extend(st.items.iter().map(|i| format!("{P}  {} = {} ({})", i.number, pack::item_identifier(&i.id), i.name)));
            out
        }
        Some("version") => vec![format!("{P}Bedrock {VERSION}")],
        _ => vec![format!("{P}/bedrock status | reload | items | version")],
    }
}

struct Exec;

impl CommandExecutor for Exec {
    fn execute(&self, ctx: &CommandContext) -> CommandExecutorResult {
        let Some(server) = SERVER.get() else { return Ok(0) };
        let words: Vec<String> = ctx.get_argument::<String>("args").map(|s| s.split_whitespace().map(str::to_owned).collect()).unwrap_or_default();
        let lines = match lock().as_mut() {
            Some(st) => run(st, server, &words),
            None => Vec::new(),
        };
        for line in lines {
            ctx.source.send_message(TextComponent::text(line));
        }
        Ok(1)
    }
}

// ---- trial: which chest screen a Bedrock client opens ----------------------------------
//
// A screen of a plugin has no block behind it and a Bedrock client only opens a chest screen
// on a chest it sees. `/bchest <1-5>` shows the player who asks a chest nobody else sees and
// opens an empty screen on it, each number another way, to find the way a client takes. The
// server knows nothing of the screen. Goes away when the way is known.

struct ChestTrial;

impl CommandExecutor for ChestTrial {
    fn execute(&self, ctx: &CommandContext) -> CommandExecutorResult {
        use pumpkin_data::block_properties::{ChestLikeProperties, ChestType, HorizontalFacing};
        use pumpkin_data::{Block, BlockState};
        use pumpkin_protocol::bedrock::client::CUpdateBlock;
        use pumpkin_protocol::bedrock::client::block_actor_data::CBlockActorData;
        use pumpkin_protocol::bedrock::client::container_open::CContainerOpen;
        use pumpkin_util::math::position::BlockPos;

        let way: u8 = ctx.get_argument::<String>("args").ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        let Some(player) = ctx.source.as_player() else {
            ctx.source.send_message(TextComponent::text(format!("{P}/bchest is for a player")));
            return Ok(0);
        };
        if bedrock(&player).is_none() || !(1..=5).contains(&way) {
            ctx.source.send_message(TextComponent::text(format!("{P}/bchest <1-5>, from a Bedrock client")));
            return Ok(0);
        }
        // 1 one chest above the head, opened at once · 2 the same, opened a little later
        // 3 two chests side by side, a little later · 4 one chest under the feet, a little later
        // 5 two chests side by side, a second later
        let (double, below, wait) = match way {
            1 => (false, false, 0),
            2 => (false, false, 300),
            3 => (true, false, 300),
            4 => (false, true, 300),
            _ => (true, false, 1000),
        };
        let feet = player.position();
        let eye = player.eye_position();
        let base = if below {
            BlockPos::new(feet.x.floor() as i32, feet.y.floor() as i32 - 1, feet.z.floor() as i32)
        } else {
            BlockPos::new(eye.x.floor() as i32, eye.y.floor() as i32 + 1, eye.z.floor() as i32)
        };
        let chests = if double { vec![(base, ChestType::Left), (base.east(), ChestType::Right)] } else { vec![(base, ChestType::Single)] };
        let id = 90 + way;
        let name = player.gameprofile.name.clone();
        tracing::info!("chest trial {way} for {name}: feet {:.1} {:.1} {:.1}, chest at {} {} {}, {} chest(s), opened after {wait} ms as screen {id}", feet.x, feet.y, feet.z, base.0.x, base.0.y, base.0.z, chests.len());
        let Some(client) = bedrock(&player) else { return Ok(0) };
        for (at, kind) in &chests {
            let mut properties = ChestLikeProperties::default(&Block::CHEST);
            properties.facing = HorizontalFacing::North;
            properties.r#type = *kind;
            properties.waterlogged = false;
            let state = properties.to_state_id(&Block::CHEST);
            let block = send(client, &CUpdateBlock::new(*at, BlockState::to_be_network_id(state)));
            let actor = pumpkin::block::entities::chest_like_block_entity::build_chest_actor(*at, state, Some(&format!("Trial {way}"))).is_some_and(|data| send(client, &CBlockActorData::new(*at, data)));
            tracing::info!("chest trial {way}: block sent {block}, its data sent {actor}");
        }
        ctx.source.send_message(TextComponent::text(format!("{P}trial {way}: chest at {} {} {}", base.0.x, base.0.y, base.0.z)));
        let places: Vec<BlockPos> = chests.iter().map(|(at, _)| *at).collect();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(wait));
            if let Some(client) = bedrock(&player) {
                let ok = send(client, &CContainerOpen { container_id: id, container_type: 0, position: base, target_entity_id: VarLong(-1) });
                tracing::info!("chest trial {way}: screen {id} opened: {ok}");
            }
            // The chests go away again: what the world really has there is shown
            std::thread::sleep(std::time::Duration::from_secs(20));
            if let Some(client) = bedrock(&player) {
                let world = player.world();
                for at in places {
                    let state = world.get_block_state_id_if_loaded(&at).unwrap_or(Block::AIR.default_state.id);
                    send(client, &CUpdateBlock::new(at, BlockState::to_be_network_id(state)));
                }
            }
        });
        Ok(1)
    }
}

/// Trial: says when a Bedrock client closes a screen (it also does so to refuse one).
struct Closes;

impl EventHandler<pumpkin::plugin::server::packet::PacketReceivedEvent> for Closes {
    fn handle<'a>(&'a self, _server: &'a Arc<Server>, event: &'a pumpkin::plugin::server::packet::PacketReceivedEvent) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if event.packet_id == 47 && bedrock(&event.player).is_some() {
                tracing::info!("chest trial: {} closed a screen: {:?}", event.player.gameprofile.name, event.payload.as_ref());
            }
        })
    }
}

// ---- load --------------------------------------------------------------------------

/// The server's offset from UTC. `time` refuses to work it out in a process with several
/// threads, so ask libc, which is what `time` does itself at server start.
fn local_offset() -> time::UtcOffset {
    let mut tm = std::mem::MaybeUninit::<libc::tm>::zeroed();
    // SAFETY: `time` only returns the clock; `localtime_r` writes into `tm`, which is ours, and
    // returns null on failure. Nothing in the server changes the time zone while it runs.
    let secs = unsafe {
        let now = libc::time(std::ptr::null_mut());
        if libc::localtime_r(&now, tm.as_mut_ptr()).is_null() { 0 } else { tm.assume_init().tm_gmtoff }
    };
    i32::try_from(secs).ok().and_then(|s| time::UtcOffset::from_whole_seconds(s).ok()).unwrap_or(time::UtcOffset::UTC)
}

/// Console lines of this plugin, with the server's own timestamp format and offset (a native
/// plugin has its own copy of the logging machinery: without this nothing it logs is seen).
fn init_log(context: &Context) {
    use tracing_subscriber::fmt::{self, time::OffsetTime};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let Some(Some((_, level, config))) = context.logger.get() else { return };
    let layer = fmt::layer().with_writer(std::io::stderr).with_ansi(config.color).with_ansi_sanitization(false).with_target(config.target).with_thread_names(config.threads).with_thread_ids(config.thread_ids);
    let registry = tracing_subscriber::registry().with(*level);
    if config.timestamp {
        // Leaked once per load, like the server does: the parsed format borrows the text
        let wanted: &'static str = Box::leak(context.server.advanced_config.logging.timestamp_format.clone().into_boxed_str());
        let format = time::format_description::parse_borrowed::<1>(wanted).or_else(|_| time::format_description::parse_borrowed::<1>("[hour]:[minute]:[second]")).unwrap_or_default();
        let _ = registry.with(layer.with_timer(OffsetTime::new(local_offset(), format))).try_init();
    } else {
        let _ = registry.with(layer.without_time()).try_init();
    }
}

pub(crate) fn load(context: &Arc<Context>) -> Result<(), String> {
    init_log(context);
    let server = context.server.clone();
    let _ = SERVER.set(server.clone());
    let dir = context.get_data_folder();
    let cfg = load_config(&dir);
    let mut st = State { dir, cfg, models: HashMap::new(), items: Vec::new(), ids: None, built: None, on: server.advanced_config.networking.bedrock.enabled, insts: HashMap::new(), viewers: HashMap::new(), tick: 0, rev: 0, loaded: None, heard: false, sent: 0 };
    for line in rebuild(&mut st, &server) {
        tracing::info!("{line}");
    }
    let models = st.models.len();
    *lock() = Some(st);

    let _ = context.register_permission(Permission::new("bedrock:command", "Bedrock", PermissionDefault::Op(PermissionLvl::Two)));
    let exec = Arc::new(Exec);
    context.register_command(command("bedrock", "Models for Bedrock players").executes_arc(exec.clone()).then(argument("args", StringArgumentType::GreedyPhrase).executes_arc(exec)), "bedrock:command");
    // Trial (see `ChestTrial`): for everybody, it only touches the player who asks
    let _ = context.register_permission(Permission::new("bedrock:trial", "Bedrock chest trial", PermissionDefault::Allow));
    context.register_command(command("bchest", "Trial: a chest screen for a Bedrock player").then(argument("args", StringArgumentType::GreedyPhrase).executes_arc(Arc::new(ChestTrial))), "bedrock:trial");
    context.register_event(Arc::new(Closes), EventPriority::Normal, false);
    context.register_event(Arc::new(Joined), EventPriority::Normal, false);
    context.register_event(Arc::new(Tick), EventPriority::Normal, false);
    tracing::info!("Bedrock {VERSION} loaded: {models} model(s) for Bedrock players");
    Ok(())
}

pub(crate) fn unload() -> Result<(), String> {
    *lock() = None;
    Ok(())
}

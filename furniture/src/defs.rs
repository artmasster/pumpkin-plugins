//! What the server owner wrote: the settings and the furniture, and where placed pieces are.
//! No server needed here.
//!
//! A piece of furniture is placed with its item (of CustomItems or of the game) on top of a
//! block, turned to face the player who places it, and taken back by hitting it. It is drawn as
//! an item model or as a model of the Models plugin; it may have a seat, and may be solid.
use serde::{Deserialize, Serialize};

pub const NS: &str = "furniture";
pub const MAX_KINDS: usize = 500;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// Furniture is drawn for players this many blocks away; farther pieces have no entities.
    pub show_distance: f32,
    /// Pieces one player may place in all (0 = no limit).
    pub max_per_player: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), show_distance: 48.0, max_per_player: 0 }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.show_distance = if self.show_distance.is_finite() { self.show_distance.clamp(8.0, 128.0) } else { 48.0 };
        self
    }

    pub fn thai(&self) -> bool {
        self.language == "th"
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub th: String,
    pub en: String,
}

impl Note {
    pub fn new(place: &str, th: impl AsRef<str>, en: impl AsRef<str>) -> Note {
        Note { th: format!("{place}: {}", th.as_ref()), en: format!("{place}: {}", en.as_ref()) }
    }

    pub fn text(&self, thai: bool) -> &str {
        if thai { &self.th } else { &self.en }
    }
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn valid_item_model(text: &str) -> bool {
    let fine = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-./".contains(&b));
    text.len() <= 200 && text.split_once(':').is_some_and(|(ns, path)| fine(ns) && fine(path))
}

/// The item that places a piece: `minecraft:<item>` of the game or an id of CustomItems.
pub fn item_ref(text: &str) -> Option<String> {
    let t = text.trim().to_ascii_lowercase();
    match t.strip_prefix("minecraft:") {
        Some(name) => valid_id(name).then_some(t),
        None => valid_id(&t).then_some(t),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Kind {
    pub id: String,
    pub name: String,
    /// The item that places it and that it gives back.
    pub item: String,
    /// A model of the Models plugin; none = the item model `look` on an item display.
    pub model: Option<String>,
    /// The item model drawn (`namespace:path`).
    pub look: String,
    pub size: f32,
    /// Raises the drawing (blocks).
    pub lift: f32,
    /// Where a player sits, in blocks above the floor; none = no seat.
    pub seat: Option<f32>,
    /// A barrier block in its place, so it cannot be walked through.
    pub solid: bool,
    /// Width and height of what takes clicks.
    pub width: f32,
    pub height: f32,
    /// How many directions it can face: 4, 8 or 16.
    pub turns: u32,
    /// Needed to place it; none = everyone.
    pub permission: Option<String>,
}

fn number(v: Option<&toml::Value>) -> Option<f64> {
    match v? {
        toml::Value::Integer(i) => Some(*i as f64),
        toml::Value::Float(f) if f.is_finite() => Some(*f),
        _ => None,
    }
}

const KNOWN: [&str; 12] = ["name", "item", "model", "look", "size", "lift", "seat", "solid", "width", "height", "turns", "permission"];

/// The furniture of one file.
pub fn parse_kinds(file: &str, text: &str, out: &mut Vec<Note>) -> Vec<Kind> {
    let table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            let why = e.to_string().lines().next().unwrap_or_default().to_owned();
            out.push(Note::new(file, format!("อ่านไม่ได้: {why}"), format!("cannot be read: {why}")));
            return Vec::new();
        }
    };
    let mut list = Vec::new();
    for (id, v) in &table {
        let place = format!("{file} [{id}]");
        let Some(t) = v.as_table() else { continue };
        if !valid_id(id) {
            out.push(Note::new(&place, "ชื่อใช้ได้แค่ a-z, 0-9 และ _", "a name may only have a-z, 0-9 and _"));
            continue;
        }
        for key in t.keys().filter(|k| !KNOWN.contains(&k.as_str())) {
            out.push(Note::new(&place, format!("ไม่รู้จัก {key} จึงข้าม"), format!("{key} is not known and is skipped")));
        }
        let text_of = |key: &str| t.get(key).and_then(toml::Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        let Some(item) = text_of("item").as_deref().and_then(item_ref) else {
            out.push(Note::new(&place, "ต้องมี item เป็น id ของ CustomItems หรือ minecraft:<ไอเท็ม>", "item must be a CustomItems id or minecraft:<item>"));
            continue;
        };
        let look = match text_of("look") {
            Some(l) if valid_item_model(&l.to_ascii_lowercase()) => l.to_ascii_lowercase(),
            Some(l) if valid_id(&l.to_ascii_lowercase()) => format!("customitems:{}", l.to_ascii_lowercase()),
            Some(_) => {
                out.push(Note::new(&place, "look ต้องเป็น namespace:path", "look must be namespace:path"));
                continue;
            }
            None if item.starts_with("minecraft:") => item.clone(),
            None => format!("customitems:{item}"),
        };
        let turns = match number(t.get("turns")).map(|n| n as u32) {
            None => 8,
            Some(n @ (4 | 8 | 16)) => n,
            Some(_) => {
                out.push(Note::new(&place, "turns ต้องเป็น 4, 8 หรือ 16 ใช้ 8", "turns must be 4, 8 or 16: 8 is used"));
                8
            }
        };
        let size = number(t.get("size")).unwrap_or(1.0).clamp(0.05, 8.0) as f32;
        list.push(Kind {
            id: id.clone(),
            name: text_of("name").unwrap_or_else(|| id.clone()),
            item,
            model: text_of("model").map(|m| m.to_ascii_lowercase()),
            look,
            size,
            lift: number(t.get("lift")).unwrap_or(0.0).clamp(-2.0, 4.0) as f32,
            seat: number(t.get("seat")).map(|s| s.clamp(-1.0, 3.0) as f32),
            solid: t.get("solid").and_then(toml::Value::as_bool).unwrap_or(false),
            width: number(t.get("width")).unwrap_or(1.0).clamp(0.2, 4.0) as f32,
            height: number(t.get("height")).unwrap_or(1.0).clamp(0.2, 4.0) as f32,
            turns,
            permission: text_of("permission"),
        });
    }
    list
}

/// One list from all files: an id or an item counts once.
pub fn merge_kinds(all: Vec<Kind>, out: &mut Vec<Note>) -> Vec<Kind> {
    let mut list: Vec<Kind> = Vec::new();
    for k in all {
        if list.iter().any(|o| o.id == k.id) {
            out.push(Note::new(&k.id, "ชื่อซ้ำกับไฟล์อื่น ใช้อันแรก", "the name is used in another file too; the first counts"));
        } else if list.iter().any(|o| o.item == k.item) {
            out.push(Note::new(&k.id, format!("ไอเท็ม {} วางเฟอร์นิเจอร์อื่นอยู่แล้ว", k.item), format!("item {} places other furniture already", k.item)));
        } else if list.len() >= MAX_KINDS {
            out.push(Note::new(&k.id, format!("มีได้ไม่เกิน {MAX_KINDS} อย่าง"), format!("at most {MAX_KINDS} kinds")));
        } else {
            list.push(k);
        }
    }
    list
}

/// A placed piece.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Piece {
    pub kind: String,
    pub world: String,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub yaw: f32,
    /// Who placed it (UUID hex).
    #[serde(default)]
    pub owner: String,
}

/// The yaw a piece faces when a player looking along `player_yaw` places it: towards the
/// player, in one of `turns` directions.
pub fn facing(player_yaw: f32, turns: u32) -> f32 {
    let step = 360.0 / turns.max(1) as f32;
    let toward = player_yaw + 180.0;
    let snapped = (toward / step).round() * step;
    snapped.rem_euclid(360.0)
}

pub const EXAMPLE: &str = r#"# Furniture: one [table] per piece. Hand out the item; a player right clicks the top of a
# block with it to place the piece (turned to face them) and hits the piece to take it back.
#
# item       = the item that places it: an id of CustomItems, or minecraft:<item>
# look       = the item model drawn (namespace:path); default: the item's own model
# model      = a model of the Models plugin, drawn instead of look
# size, lift = how big it is drawn and how far up (blocks)
# seat       = players sit on it this far above the floor (right click it)
# solid      = a barrier block in its place, so nobody walks through it
# width, height = what takes the clicks
# turns      = 4, 8 or 16 directions it can face
# permission = needed to place it

# An item of the game here would stop players from using that item normally: give furniture an
# item of its own in CustomItems (this one needs an item "stool" there).
[stool]
name = "&6Stool"
item = "stool"
look = "minecraft:oak_slab"
size = 0.6
lift = 0.3
seat = 0.5
solid = false
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_are_read_with_their_defaults() {
        let mut notes = Vec::new();
        let list = parse_kinds("f.toml", "[chair]\nitem = \"chair_item\"\nseat = 0.4\nsolid = true\nturns = 4\nwobble = 1\n[lamp]\nitem = \"minecraft:lantern\"\nturns = 5\n[bad]\nlook = \"x\"\n", &mut notes);
        assert_eq!(list.len(), 2);
        let chair = &list[0];
        assert_eq!((chair.item.as_str(), chair.look.as_str(), chair.seat, chair.solid, chair.turns), ("chair_item", "customitems:chair_item", Some(0.4), true, 4));
        let lamp = &list[1];
        assert_eq!((lamp.look.as_str(), lamp.turns, lamp.seat), ("minecraft:lantern", 8, None));
        assert_eq!(notes.len(), 3, "{notes:?}");
    }

    #[test]
    fn one_item_places_one_kind() {
        let mut notes = Vec::new();
        let mut all = parse_kinds("a.toml", "[a]\nitem = \"x\"\n[b]\nitem = \"x\"\n", &mut notes);
        all.extend(parse_kinds("b.toml", "[a]\nitem = \"y\"\n", &mut notes));
        let list = merge_kinds(all, &mut notes);
        assert_eq!(list.len(), 1);
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn pieces_face_the_player() {
        // A player looking south (0) gets a piece facing north (180)
        assert_eq!(facing(0.0, 4), 180.0);
        assert_eq!(facing(100.0, 4), 270.0);
        assert_eq!(facing(-30.0, 8), 135.0);
        assert_eq!(facing(10.0, 16), 180.0 + 0.0f32.max(22.5 * 0.0));
    }

    #[test]
    fn the_example_reads_cleanly() {
        let mut notes = Vec::new();
        assert_eq!(parse_kinds("example.toml", EXAMPLE, &mut notes).len(), 1);
        assert!(notes.is_empty(), "{notes:?}");
    }
}

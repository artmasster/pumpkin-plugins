//! What the server owner wrote: the settings and the cosmetics. No server needed here.
//!
//! A cosmetic goes into one slot of a player (`hat`, `back`, `float`) and looks like a
//! model of the Models plugin or like an item of a resource pack.
use serde::{Deserialize, Serialize};

/// The namespace of this plugin's permissions.
pub const NS: &str = "cosmetics";
pub const MAX_COSMETICS: usize = 1000;
/// How far an offset may move a cosmetic from where its slot puts it, in blocks.
pub const MAX_OFFSET: f32 = 8.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// `th` or `en`: the language of the plugin's own messages.
    pub language: String,
    /// Every cosmetic that does not say `permission` needs `cosmetics:<id>`. Off: those are
    /// for everyone.
    pub permission_by_default: bool,
    /// Take cosmetics off a player who is invisible (a potion, a disguise) or a spectator, and
    /// put them back when that is over.
    pub hide_when_invisible: bool,
    /// Where a floating thing (a balloon, a pet) wants to be: this high above the player's
    /// feet, this far behind. Pulled after them when they walk away.
    pub float_height: f32,
    pub float_distance: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), permission_by_default: false, hide_when_invisible: true, float_height: 2.4, float_distance: 1.2 }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        let sane = |v: f32, low: f32, high: f32, or: f32| if v.is_finite() { v.clamp(low, high) } else { or };
        self.float_height = sane(self.float_height, -2.0, 8.0, 2.4);
        self.float_distance = sane(self.float_distance, 0.0, 6.0, 1.2);
        self
    }

    pub fn thai(&self) -> bool {
        self.language == "th"
    }
}

/// Something the owner should know, in both languages.
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

/// Where on a player a cosmetic goes. A player wears one cosmetic per slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Slot {
    /// On the head: follows where the player looks. Worn in the helmet slot when it can be
    /// one item; a real helmet goes first.
    Hat,
    /// On the body: turns with the player.
    Back,
    /// Floats behind the player and is pulled after them: a balloon on a lead from their hand,
    /// or a pet without one. Players chose a `balloon` before this was its name.
    #[serde(alias = "balloon")]
    Float,
}

impl Slot {
    pub const ALL: [Slot; 3] = [Slot::Hat, Slot::Back, Slot::Float];

    pub fn name(self) -> &'static str {
        match self {
            Slot::Hat => "hat",
            Slot::Back => "back",
            Slot::Float => "float",
        }
    }

    pub fn parse(text: &str) -> Option<Slot> {
        Slot::ALL.into_iter().find(|s| s.name().eq_ignore_ascii_case(text.trim()))
    }

    pub fn label(self, thai: bool) -> &'static str {
        match (self, thai) {
            (Slot::Hat, true) => "หมวก",
            (Slot::Back, true) => "ของบนหลัง",
            (Slot::Float, true) => "ของลอย",
            (Slot::Hat, false) => "Hats",
            (Slot::Back, false) => "Back",
            (Slot::Float, false) => "Floating",
        }
    }
}

/// What a cosmetic looks like.
#[derive(Clone, Debug, PartialEq)]
pub enum Look {
    /// A model of the Models plugin, by its name.
    Model(String),
    /// An item model of a resource pack, as `namespace:path`.
    Item(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cosmetic {
    pub id: String,
    pub slot: Slot,
    pub look: Look,
    pub name: String,
    pub lore: Vec<String>,
    /// What stands for it in the wardrobe: an item of the game (`minecraft:…`) or an item
    /// model. None: the cosmetic's own item, or one that fits its slot.
    pub icon: Option<String>,
    /// The permission it needs; None: everyone may wear it.
    pub permission: Option<String>,
    /// Blocks away from where the slot puts it: right is -X, up +Y, behind +Z.
    pub offset: [f32; 3],
    /// Times its size (an item), or the model's scale.
    pub size: Option<f32>,
    /// Degrees about the up axis (an item that faces the wrong way).
    pub turn: f32,
    /// How an item is drawn: `head`, `fixed`, `none`, `ground`, `gui`, `hand`.
    pub display: Option<String>,
    /// An animation of the model that runs all the time.
    pub animation: Option<String>,
    /// A floating thing hangs on a lead from the player's hand.
    pub lead: bool,
}

/// An item of CustomItems by its id, or an item model as it is.
pub fn item_model(text: &str) -> String {
    let t = text.trim().to_ascii_lowercase();
    if t.contains(':') { t } else { format!("customitems:{t}") }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn valid_item_model(text: &str) -> bool {
    let fine = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-./".contains(&b));
    text.len() <= 200 && text.split_once(':').is_some_and(|(ns, path)| fine(ns) && fine(path))
}

fn number(v: &toml::Value) -> Option<f32> {
    match v {
        toml::Value::Integer(i) => Some(*i as f32),
        toml::Value::Float(f) if f.is_finite() => Some(*f as f32),
        _ => None,
    }
}

const KNOWN: [&str; 13] = ["slot", "model", "item", "name", "lore", "icon", "permission", "offset", "size", "turn", "display", "animation", "lead"];
const DISPLAYS: [&str; 6] = ["head", "fixed", "none", "ground", "gui", "hand"];

/// The cosmetics of one file. `by_default`: a cosmetic that does not say `permission`
/// needs `cosmetics:<id>`.
pub fn parse_cosmetics(file: &str, text: &str, by_default: bool, out: &mut Vec<Note>) -> Vec<Cosmetic> {
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
        let Some(slot) = text_of("slot").as_deref().and_then(Slot::parse) else {
            out.push(Note::new(&place, "ต้องมี slot เป็น hat, back หรือ float", "slot must be hat, back or float"));
            continue;
        };
        let look = match (text_of("model"), text_of("item")) {
            (Some(model), item) => {
                if item.is_some() {
                    out.push(Note::new(&place, "มีทั้ง model และ item ใช้ model", "has both model and item; model is used"));
                }
                Look::Model(model.to_ascii_lowercase())
            }
            (None, Some(item)) => {
                let model = item_model(&item);
                if !valid_item_model(&model) {
                    out.push(Note::new(&place, format!("item {item} ใช้ไม่ได้ (ใส่ id ของ CustomItems หรือ namespace:path)"), format!("item {item} cannot be used (give a CustomItems id or namespace:path)")));
                    continue;
                }
                Look::Item(model)
            }
            (None, None) => {
                out.push(Note::new(&place, "ต้องมี model (ของ Models) หรือ item (ของ CustomItems)", "needs model (of Models) or item (of CustomItems)"));
                continue;
            }
        };
        let permission = match t.get("permission") {
            Some(toml::Value::String(node)) if !node.trim().is_empty() => Some(node.trim().to_owned()),
            Some(toml::Value::Boolean(true)) => Some(format!("{NS}:{id}")),
            Some(toml::Value::Boolean(false) | toml::Value::String(_)) => None,
            _ if by_default => Some(format!("{NS}:{id}")),
            _ => None,
        };
        let mut offset = [0.0f32; 3];
        if let Some(list) = t.get("offset").and_then(toml::Value::as_array) {
            if list.len() == 3 && list.iter().all(|n| number(n).is_some()) {
                for (i, n) in list.iter().enumerate() {
                    offset[i] = number(n).unwrap_or(0.0).clamp(-MAX_OFFSET, MAX_OFFSET);
                }
            } else {
                out.push(Note::new(&place, "offset ต้องเป็นตัวเลขสามตัว เช่น [0, 0.1, 0]", "offset must be three numbers, like [0, 0.1, 0]"));
            }
        }
        let display = text_of("display").map(|d| d.to_ascii_lowercase()).filter(|d| {
            let ok = DISPLAYS.contains(&d.as_str());
            if !ok {
                out.push(Note::new(&place, format!("display {d} ใช้ไม่ได้ (head, fixed, none, ground, gui, hand)"), format!("display {d} cannot be used (head, fixed, none, ground, gui, hand)")));
            }
            ok
        });
        let lore = match t.get("lore") {
            Some(toml::Value::String(line)) => vec![line.clone()],
            Some(toml::Value::Array(lines)) => lines.iter().filter_map(toml::Value::as_str).map(str::to_owned).take(16).collect(),
            _ => Vec::new(),
        };
        list.push(Cosmetic {
            id: id.clone(),
            slot,
            look,
            name: text_of("name").unwrap_or_else(|| id.clone()).chars().filter(|c| !c.is_control()).take(80).collect(),
            lore: lore.into_iter().map(|l| l.chars().filter(|c| !c.is_control()).take(120).collect()).collect(),
            icon: text_of("icon").map(|i| i.to_ascii_lowercase()),
            permission,
            offset,
            size: t.get("size").and_then(number).map(|s| s.clamp(0.01, 16.0)),
            turn: t.get("turn").and_then(number).unwrap_or(0.0),
            display,
            animation: text_of("animation"),
            lead: t.get("lead").and_then(toml::Value::as_bool).unwrap_or(true),
        });
    }
    list
}

/// All files together: sorted, and the first of two cosmetics with one name wins.
pub fn merge_cosmetics(mut all: Vec<Cosmetic>, out: &mut Vec<Note>) -> Vec<Cosmetic> {
    all.sort_by(|a, b| a.id.cmp(&b.id));
    all.dedup_by(|b, a| {
        let same = a.id == b.id;
        if same {
            out.push(Note::new(&a.id, "มีของชื่อนี้สองชิ้น ใช้ชิ้นแรก", "two cosmetics have this name; the first one is used"));
        }
        same
    });
    if all.len() > MAX_COSMETICS {
        out.push(Note::new("cosmetics", format!("มีเกิน {MAX_COSMETICS} ชิ้น ส่วนที่เกินถูกข้าม"), format!("more than {MAX_COSMETICS}; the rest are skipped")));
        all.truncate(MAX_COSMETICS);
    }
    all
}

/// Where a floating thing wants to be for a player at `feet` looking `yaw` (the game's
/// degrees): `height` up, `distance` behind and `side` to the right.
pub fn float_spot(feet: (f64, f64, f64), yaw: f32, height: f32, distance: f32, side: f32) -> (f64, f64, f64) {
    let r = f64::from(yaw).to_radians();
    // Yaw 0 looks south (+Z); the right hand is then to the west (-X)
    let (forward, right) = ((-r.sin(), r.cos()), (-r.cos(), -r.sin()));
    let (back, side) = (f64::from(distance), f64::from(side));
    (feet.0 - forward.0 * back + right.0 * side, feet.1 + f64::from(height), feet.2 - forward.1 * back + right.1 * side)
}

/// The next place of a floating thing that follows a player at `feet`: it stays where it is
/// while it is near enough, is pulled along once the player is more than `distance` away (so it
/// trails behind where they go), never comes nearer than half a block and floats up to
/// `height` above the feet. Far away (a teleport), it goes to its place behind the player at once.
pub fn float_follow(now: (f64, f64, f64), feet: (f64, f64, f64), yaw: f32, height: f32, distance: f32, side: f32) -> (f64, f64, f64) {
    let up = feet.1 + f64::from(height);
    let (dx, dz) = (now.0 - feet.0, now.2 - feet.2);
    let flat = (dx * dx + dz * dz).sqrt();
    if flat > 8.0 + f64::from(distance) || (now.1 - up).abs() > 8.0 || flat < 1e-6 {
        return float_spot(feet, yaw, height, distance, side);
    }
    let keep = flat.clamp(0.5, f64::from(distance).max(0.5));
    (feet.0 + dx / flat * keep, now.1 + (up - now.1) * 0.3, feet.2 + dz / flat * keep)
}

/// Degrees (the game's) a thing faces when it moved from `from` to `to`.
pub fn facing(from: (f64, f64, f64), to: (f64, f64, f64)) -> f32 {
    (-(to.0 - from.0)).atan2(to.2 - from.2).to_degrees() as f32
}

pub const EXAMPLE: &str = r##"# Cosmetics: one table per cosmetic. The table's name is its id (a-z, 0-9, _).
# Players open the wardrobe with /cosmetics and put on what they may wear.
# The Models plugin draws everything: install it next to this one.
#
# slot       hat, back (turns with the body) or float (floats behind the player and is
#            pulled after them: a balloon on a lead, a pet that follows; on a lead, draw it
#            from its knot up)
#            A hat is worn in the helmet slot, so it moves with the head at once. A real
#            helmet goes first: the hat comes back when the helmet is taken off. A model hat
#            with a bone turned at rest, or with an animation, rides the head instead.
# model      a model of the Models plugin (plugins/data/models/models/<name>.bbmodel):
#            draw it standing where the player stands, the way it sits on a player
# item       an item of CustomItems (its id) or any item model as namespace:path
#            (worn as a hat, the item model's own head display places it)
# name, lore what the wardrobe shows (& colours)
# icon       what stands for it in the wardrobe: minecraft:<item> or an item model
# permission true: needs cosmetics:<id>; or a permission of your own; false: everyone
#            (config.toml permission-by-default sets what happens when it is not given)
# offset     [x, y, z] blocks away from where the slot puts it: right is -x, up y, behind z
#            (a float starts from config.toml float-height up and float-distance behind)
# size       times its size (an item), or the model's scale
# turn       degrees about the up axis, for an item that faces the wrong way
# display    how an item is drawn: fixed, none, ground, gui, hand, head
#            (offset, size, turn and display are for a back item and a float)
# animation  an animation of the model that runs all the time
# lead       a float hangs on a lead from the player's hand: true (the default) or false
#
# [ruby_crown]
# slot = "hat"
# item = "ruby_crown"
# name = "&cRuby Crown"
# lore = ["&7Shines a little"]
#
# [wings]
# slot = "back"
# model = "wings"
# animation = "flap"
# name = "&fWings"
# permission = true
#
# [red_balloon]
# slot = "float"
# item = "minecraft:red_wool"
# size = 0.6
# name = "&cRed balloon"
#
# [fox]
# slot = "float"
# model = "fox"
# animation = "walk"
# lead = false
# offset = [0, -2.4, 0.5]
# name = "&6Fox"
"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> (Vec<Cosmetic>, Vec<Note>) {
        let mut notes = Vec::new();
        let list = parse_cosmetics("a.toml", text, false, &mut notes);
        (list, notes)
    }

    #[test]
    fn the_example_reads_without_a_word_once_it_is_switched_on() {
        assert_eq!(read(EXAMPLE), (Vec::new(), Vec::new()));
        let text: String = EXAMPLE.lines().map(|l| l.strip_prefix("# ").or_else(|| l.strip_prefix('#')).unwrap_or(l)).filter(|l| l.starts_with('[') || l.contains(" = ")).collect::<Vec<_>>().join("\n");
        let (list, notes) = read(&text);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(list.len(), 4);
        let crown = list.iter().find(|c| c.id == "ruby_crown").unwrap();
        assert_eq!(crown.slot, Slot::Hat);
        assert_eq!(crown.look, Look::Item("customitems:ruby_crown".into()));
        assert_eq!(crown.permission, None);
        let wings = list.iter().find(|c| c.id == "wings").unwrap();
        assert_eq!(wings.look, Look::Model("wings".into()));
        assert_eq!(wings.permission.as_deref(), Some("cosmetics:wings"));
        assert_eq!(wings.animation.as_deref(), Some("flap"));
        let balloon = list.iter().find(|c| c.id == "red_balloon").unwrap();
        assert_eq!(balloon.look, Look::Item("minecraft:red_wool".into()));
        assert_eq!(balloon.size, Some(0.6));
        assert_eq!((balloon.slot, balloon.lead), (Slot::Float, true));
        let fox = list.iter().find(|c| c.id == "fox").unwrap();
        assert_eq!((fox.lead, fox.offset), (false, [0.0, -2.4, 0.5]));
    }

    #[test]
    fn what_cannot_be_used_is_said_and_skipped() {
        let (list, notes) = read("[Bad]\nslot = \"hat\"\nitem = \"x\"\n[noslot]\nitem = \"x\"\n[nolook]\nslot = \"back\"\n[odd]\nslot = \"hat\"\nitem = \"Bad Item!\"\n[ok]\nslot = \"HAT\"\nitem = \"x\"\noffset = [0, 1]\ndisplay = \"sideways\"\ncolour = 1\n");
        assert_eq!(list.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["ok"]);
        assert_eq!(notes.len(), 7, "{notes:?}");
        assert_eq!(list[0].offset, [0.0; 3]);
        assert_eq!(list[0].display, None);
    }

    #[test]
    fn permissions_follow_the_file_then_the_setting() {
        let text = "[a]\nslot = \"hat\"\nitem = \"x\"\n[b]\nslot = \"hat\"\nitem = \"x\"\npermission = false\n[c]\nslot = \"hat\"\nitem = \"x\"\npermission = \"vip.hats\"\n";
        let mut notes = Vec::new();
        let list = parse_cosmetics("f", text, true, &mut notes);
        let node = |id: &str| list.iter().find(|c| c.id == id).unwrap().permission.clone();
        assert_eq!(node("a").as_deref(), Some("cosmetics:a"));
        assert_eq!(node("b"), None);
        assert_eq!(node("c").as_deref(), Some("vip.hats"));
    }

    #[test]
    fn two_cosmetics_with_one_name_keep_the_first() {
        let mut notes = Vec::new();
        let mut all = parse_cosmetics("a", "[x]\nslot = \"hat\"\nitem = \"one\"\n", false, &mut notes);
        all.extend(parse_cosmetics("b", "[x]\nslot = \"hat\"\nitem = \"two\"\n", false, &mut notes));
        let list = merge_cosmetics(all, &mut notes);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].look, Look::Item("customitems:one".into()));
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn a_float_trails_behind_and_is_pulled_along() {
        // The settings are f32
        let close = |a: (f64, f64, f64), b: (f64, f64, f64)| (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6 && (a.2 - b.2).abs() < 1e-6;
        // Looking south (yaw 0): behind is north (-Z), the right hand west (-X)
        let at = float_spot((0.0, 64.0, 0.0), 0.0, 2.0, 1.0, 0.5);
        assert!(close(at, (-0.5, 66.0, -1.0)), "{at:?}");
        // Looking west (yaw 90): behind is east (+X)
        let at = float_spot((0.0, 64.0, 0.0), 90.0, 2.0, 1.0, 0.5);
        assert!((at.0 - 1.0).abs() < 1e-9 && (at.2 + 0.5).abs() < 1e-9, "{at:?}");
        let feet = (0.0, 64.0, 0.0);
        // Near enough: it stays, whichever way the player looks
        assert!(close(float_follow((0.8, 66.0, 0.0), feet, 123.0, 2.0, 1.2, 0.5), (0.8, 66.0, 0.0)));
        // The player walked away: pulled after them to its distance
        assert!(close(float_follow((-2.0, 66.0, 0.0), feet, 0.0, 2.0, 1.2, 0.5), (-1.2, 66.0, 0.0)));
        // Never into the player; it rises a part of the way up each tick
        let at = float_follow((0.0, 65.0, 0.1), feet, 0.0, 2.0, 1.2, 0.5);
        assert!(close(at, (0.0, 65.3, 0.5)), "{at:?}");
        // A teleport: straight to its place
        assert!(close(float_follow((20.0, 66.0, 0.0), feet, 0.0, 2.0, 1.0, 0.5), (-0.5, 66.0, -1.0)));
        // Pulled east it faces east (yaw -90), pulled south it faces south (0)
        assert!((facing((0.0, 0.0, 0.0), (1.0, 0.0, 0.0)) + 90.0).abs() < 1e-4);
        assert!(facing((0.0, 0.0, 0.0), (0.0, 0.0, 1.0)).abs() < 1e-4);
    }
}

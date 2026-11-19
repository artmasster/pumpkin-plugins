//! What the server owner wrote: the pets, and what each player has. No server needed here.
//!
//! A pet is a creature of the game (with a model of the Models plugin drawn over it, if the
//! owner likes) that follows its player, cannot be hurt, and can be ridden when it says so.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const NS: &str = "pets";
pub const MAX_PETS: usize = 300;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// A pet farther than this from its player jumps to them.
    pub teleport_distance: f32,
    /// A pet walks to its player when it is farther than this.
    pub follow_distance: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), teleport_distance: 20.0, follow_distance: 3.0 }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.teleport_distance = if self.teleport_distance.is_finite() { self.teleport_distance.clamp(8.0, 64.0) } else { 20.0 };
        self.follow_distance = if self.follow_distance.is_finite() { self.follow_distance.clamp(1.5, 8.0) } else { 3.0 };
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

#[derive(Clone, Debug, PartialEq)]
pub struct Pet {
    pub id: String,
    pub name: String,
    /// The creature it is (`wolf`, `cat`, …).
    pub kind: String,
    pub model: Option<String>,
    /// The item shown for it in the `/pets` window.
    pub icon: String,
    pub rideable: bool,
    /// Blocks a second when ridden.
    pub ride_speed: f32,
    /// Walking speed when it follows (the game's navigation speed).
    pub follow_speed: f32,
    pub baby: bool,
    /// Needed to have it; none = everyone.
    pub permission: Option<String>,
}

fn number(v: Option<&toml::Value>) -> Option<f64> {
    match v? {
        toml::Value::Integer(i) => Some(*i as f64),
        toml::Value::Float(f) if f.is_finite() => Some(*f),
        _ => None,
    }
}

const KNOWN: [&str; 10] = ["name", "type", "model", "icon", "rideable", "ride-speed", "follow-speed", "baby", "permission", "speed"];

/// The pets of one file.
pub fn parse_pets(file: &str, text: &str, out: &mut Vec<Note>) -> Vec<Pet> {
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
        let kind = text_of("type").map_or_else(|| "wolf".to_owned(), |k| k.to_ascii_lowercase().trim_start_matches("minecraft:").to_owned());
        if !valid_id(&kind) {
            out.push(Note::new(&place, "type ต้องเป็นชื่อสัตว์ของเกม", "type must be a creature of the game"));
            continue;
        }
        let icon = text_of("icon").map_or_else(|| "minecraft:bone".to_owned(), |i| if i.contains(':') { i.to_ascii_lowercase() } else { format!("minecraft:{}", i.to_ascii_lowercase()) });
        list.push(Pet {
            id: id.clone(),
            name: text_of("name").unwrap_or_else(|| id.clone()),
            kind,
            model: text_of("model").map(|m| m.to_ascii_lowercase()),
            icon,
            rideable: t.get("rideable").and_then(toml::Value::as_bool).unwrap_or(false),
            ride_speed: number(t.get("ride-speed").or_else(|| t.get("speed"))).unwrap_or(6.0).clamp(1.0, 20.0) as f32,
            follow_speed: number(t.get("follow-speed")).unwrap_or(1.2).clamp(0.3, 3.0) as f32,
            baby: t.get("baby").and_then(toml::Value::as_bool).unwrap_or(false),
            permission: text_of("permission"),
        });
    }
    list
}

pub fn merge_pets(all: Vec<Pet>, out: &mut Vec<Note>) -> Vec<Pet> {
    let mut list: Vec<Pet> = Vec::new();
    for p in all {
        if list.iter().any(|o| o.id == p.id) {
            out.push(Note::new(&p.id, "ชื่อซ้ำกับไฟล์อื่น ใช้อันแรก", "the name is used in another file too; the first counts"));
        } else if list.len() >= MAX_PETS {
            out.push(Note::new(&p.id, format!("มีได้ไม่เกิน {MAX_PETS} ตัว"), format!("at most {MAX_PETS} pets")));
        } else {
            list.push(p);
        }
    }
    list
}

/// What one player has: the pet that is out, and the names they gave.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Owner {
    pub out: Option<String>,
    pub names: BTreeMap<String, String>,
}

/// The keys a player holds, from the bits the game sends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keys {
    pub forward: bool,
    pub back: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    pub sprint: bool,
}

/// Pumpkin hands the input over as the bits written in binary (`"10001"`).
pub fn keys(bits: &str) -> Keys {
    let n = u8::from_str_radix(bits.trim(), 2).unwrap_or(0);
    Keys { forward: n & 1 != 0, back: n & 2 != 0, left: n & 4 != 0, right: n & 8 != 0, jump: n & 16 != 0, sprint: n & 64 != 0 }
}

/// The ground speed (x, z) in blocks a tick of a ridden pet, for a rider looking along `yaw`.
pub fn ride_velocity(k: Keys, yaw: f32, speed: f32) -> (f64, f64) {
    let ahead = f32::from(i8::from(k.forward) - i8::from(k.back));
    let side = f32::from(i8::from(k.left) - i8::from(k.right));
    if ahead == 0.0 && side == 0.0 {
        return (0.0, 0.0);
    }
    let len = (ahead * ahead + side * side).sqrt();
    let per_tick = speed / 20.0 * if k.sprint { 1.5 } else { 1.0 } * if ahead < 0.0 { 0.5 } else { 1.0 };
    let r = yaw.to_radians();
    // The game's forward for yaw 0 is south (+z); left is east (+x)
    let (fx, fz) = (-r.sin(), r.cos());
    let (lx, lz) = (r.cos(), r.sin());
    let x = (fx * ahead + lx * side) / len * per_tick;
    let z = (fz * ahead + lz * side) / len * per_tick;
    (f64::from(x), f64::from(z))
}

pub const EXAMPLE: &str = r#"# Pets: one [table] per pet. Players open /pets to call one out; it follows them and cannot
# be hurt. Right click your pet to ride it when it is rideable (W A S D, space jumps, sneak gets off).
#
# type         = the creature of the game (wolf, cat, fox, pig, horse, …)
# model        = a model of the Models plugin drawn over it (the creature is then hidden)
# icon         = the item shown for it in the window
# rideable     = players can ride it; ride-speed = blocks a second
# follow-speed = how fast it walks after its player
# baby         = a young one
# permission   = needed to have it; none = everyone

[wolf]
name = "&fWolf"
type = "wolf"
icon = "minecraft:bone"

[pig]
name = "&dPiggy"
type = "pig"
icon = "minecraft:carrot_on_a_stick"
rideable = true
ride-speed = 6
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_reads_cleanly() {
        let mut notes = Vec::new();
        let list = parse_pets("example.toml", EXAMPLE, &mut notes);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(list.len(), 2);
        let pig = list.iter().find(|p| p.id == "pig").unwrap();
        let wolf = list.iter().find(|p| p.id == "wolf").unwrap();
        assert!(pig.rideable && !wolf.rideable);
        assert_eq!(wolf.kind, "wolf");
    }

    #[test]
    fn keys_come_from_bits() {
        assert_eq!(keys("1"), Keys { forward: true, ..Keys::default() });
        assert_eq!(keys("1010001"), Keys { forward: true, jump: true, sprint: true, ..Keys::default() });
        assert_eq!(keys("junk"), Keys::default());
    }

    #[test]
    fn riding_goes_where_the_rider_looks() {
        let fwd = Keys { forward: true, ..Keys::default() };
        let (x, z) = ride_velocity(fwd, 0.0, 20.0);
        assert!(x.abs() < 1e-6 && (z - 1.0).abs() < 1e-6, "{x} {z}");
        // Looking west (90): forward is -x
        let (x, z) = ride_velocity(fwd, 90.0, 20.0);
        assert!((x + 1.0).abs() < 1e-6 && z.abs() < 1e-6, "{x} {z}");
        assert_eq!(ride_velocity(Keys::default(), 0.0, 20.0), (0.0, 0.0));
        // Diagonal is not faster
        let (x, z) = ride_velocity(Keys { forward: true, left: true, ..Keys::default() }, 0.0, 20.0);
        assert!(((x * x + z * z).sqrt() - 1.0).abs() < 1e-6);
    }
}

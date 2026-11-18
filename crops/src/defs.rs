//! What the server owner wrote: the settings and the crops, and how a plant grows. No server
//! needed here.
//!
//! A crop is planted with its seed on its soil (farmland unless it says otherwise) and goes
//! through its stages; the last one is ripe. Each stage looks like an item (of the game, of
//! CustomItems or any item model) or like an animation of a model of the Models plugin.
use serde::{Deserialize, Serialize};

/// The namespace of this plugin's permissions and of the marks on its entities.
pub const NS: &str = "crops";
pub const MAX_CROPS: usize = 500;
pub const MAX_STAGES: usize = 16;
pub const MAX_DROPS: usize = 16;
/// The longest a stage may take: a week.
pub const MAX_STAGE_SECONDS: f64 = 7.0 * 24.0 * 3600.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// `th` or `en`: the language of the plugin's own messages.
    pub language: String,
    /// Plants are drawn for players this many blocks away; farther ones have no entities.
    pub show_distance: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), show_distance: 48.0 }
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

/// An item a player holds or gets: of the game (`minecraft:wheat_seeds`) or of CustomItems
/// (its id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemRef {
    Game(String),
    Ours(String),
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl ItemRef {
    pub fn parse(text: &str) -> Option<ItemRef> {
        let t = text.trim().to_ascii_lowercase();
        match t.strip_prefix("minecraft:") {
            Some(name) if valid_id(name) => Some(ItemRef::Game(t)),
            Some(_) => None,
            None if valid_id(&t) => Some(ItemRef::Ours(t)),
            None => None,
        }
    }

    pub fn text(&self) -> &str {
        match self {
            ItemRef::Game(name) | ItemRef::Ours(name) => name,
        }
    }
}

/// What a stage looks like.
#[derive(Clone, Debug, PartialEq)]
pub enum Look {
    /// An item of the game as it is.
    Game(String),
    /// An item model (`namespace:path`): a CustomItems id becomes `customitems:<id>`.
    Model(String),
    /// An animation of the crop's model (Models), held at its end.
    Animation(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stage {
    pub look: Look,
    /// Seconds of growing to the next stage. The last stage is ripe and does not grow.
    pub seconds: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Drop {
    pub item: ItemRef,
    pub min: u32,
    pub max: u32,
    /// 0 to 1: how likely it drops at all.
    pub chance: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Crop {
    pub id: String,
    pub name: String,
    pub seed: ItemRef,
    /// Blocks it may be planted on, as the game names them without `minecraft:` (`farmland`).
    pub soil: Vec<String>,
    /// A model of the Models plugin: the stages are its animations.
    pub model: Option<String>,
    pub stages: Vec<Stage>,
    /// Grows only while the farmland under it is wet.
    pub water: bool,
    pub harvest: Vec<Drop>,
    /// After a harvest it starts again from its first stage; otherwise it is gone.
    pub replant: bool,
    /// Bone meal grows it one stage.
    pub bone_meal: bool,
}

impl Crop {
    pub fn ripe(&self, stage: usize) -> bool {
        stage + 1 >= self.stages.len()
    }
}

fn number(v: &toml::Value) -> Option<f64> {
    match v {
        toml::Value::Integer(i) => Some(*i as f64),
        toml::Value::Float(f) if f.is_finite() => Some(*f),
        _ => None,
    }
}

fn valid_item_model(text: &str) -> bool {
    let fine = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-./".contains(&b));
    text.len() <= 200 && text.split_once(':').is_some_and(|(ns, path)| fine(ns) && fine(path))
}

/// `minecraft:wheat` is the game's item, `tomato_1` an item of CustomItems, `ns:path` any item model.
fn item_look(text: &str) -> Option<Look> {
    let t = text.trim().to_ascii_lowercase();
    if t.starts_with("minecraft:") {
        return valid_item_model(&t).then_some(Look::Game(t));
    }
    let model = if t.contains(':') { t } else { format!("customitems:{t}") };
    valid_item_model(&model).then_some(Look::Model(model))
}

const KNOWN: [&str; 9] = ["name", "seed", "soil", "model", "stages", "water", "harvest", "replant", "bone-meal"];

/// The crops of one file.
pub fn parse_crops(file: &str, text: &str, out: &mut Vec<Note>) -> Vec<Crop> {
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
        let Some(seed) = text_of("seed").as_deref().and_then(ItemRef::parse) else {
            out.push(Note::new(&place, "ต้องมี seed เป็น id ของ CustomItems หรือ minecraft:<ไอเท็ม>", "seed must be a CustomItems id or minecraft:<item>"));
            continue;
        };
        let soil: Vec<String> = match t.get("soil") {
            Some(toml::Value::String(one)) => vec![one.clone()],
            Some(toml::Value::Array(many)) => many.iter().filter_map(toml::Value::as_str).map(str::to_owned).collect(),
            _ => vec!["farmland".to_owned()],
        };
        let soil: Vec<String> = soil.iter().map(|s| s.trim().to_ascii_lowercase()).map(|s| s.strip_prefix("minecraft:").map(str::to_owned).unwrap_or(s)).filter(|s| valid_id(s)).collect();
        if soil.is_empty() {
            out.push(Note::new(&place, "soil ต้องเป็นบล็อก เช่น minecraft:farmland", "soil must be blocks, like minecraft:farmland"));
            continue;
        }
        let model = text_of("model").map(|m| m.to_ascii_lowercase());
        let mut stages = Vec::new();
        let raw = t.get("stages").and_then(toml::Value::as_array).map(Vec::as_slice).unwrap_or_default();
        for (n, s) in raw.iter().take(MAX_STAGES).enumerate() {
            let Some(s) = s.as_table() else { continue };
            let at = format!("{place} stages[{n}]");
            let look = match (s.get("item").and_then(toml::Value::as_str), s.get("animation").and_then(toml::Value::as_str), &model) {
                (Some(item), _, _) => match item_look(item) {
                    Some(look) => look,
                    None => {
                        out.push(Note::new(&at, format!("item {item} ใช้ไม่ได้"), format!("item {item} cannot be used")));
                        continue;
                    }
                },
                (None, Some(animation), Some(_)) => Look::Animation(animation.trim().to_owned()),
                (None, Some(_), None) => {
                    out.push(Note::new(&at, "animation ต้องมี model ของผักด้วย", "animation needs the crop's model"));
                    continue;
                }
                (None, None, _) => {
                    out.push(Note::new(&at, "ต้องมี item หรือ animation", "needs item or animation"));
                    continue;
                }
            };
            let minutes = s.get("minutes").and_then(number).unwrap_or(0.0);
            let seconds = s.get("seconds").and_then(number).unwrap_or(0.0) + minutes * 60.0;
            stages.push(Stage { look, seconds: seconds.clamp(0.0, MAX_STAGE_SECONDS) });
        }
        if stages.len() < 2 {
            out.push(Note::new(&place, "ต้องมีอย่างน้อยสองขั้นใน stages (ขั้นสุดท้ายคือสุกแล้ว)", "stages needs at least two stages (the last one is ripe)"));
            continue;
        }
        if raw.len() > MAX_STAGES {
            out.push(Note::new(&place, format!("มีเกิน {MAX_STAGES} ขั้น ส่วนที่เกินถูกข้าม"), format!("more than {MAX_STAGES} stages; the rest are skipped")));
        }
        let mut harvest = Vec::new();
        for (n, d) in t.get("harvest").and_then(toml::Value::as_array).map(Vec::as_slice).unwrap_or_default().iter().take(MAX_DROPS).enumerate() {
            let Some(d) = d.as_table() else { continue };
            let Some(item) = d.get("item").and_then(toml::Value::as_str).and_then(ItemRef::parse) else {
                out.push(Note::new(&format!("{place} harvest[{n}]"), "item ต้องเป็น id ของ CustomItems หรือ minecraft:<ไอเท็ม>", "item must be a CustomItems id or minecraft:<item>"));
                continue;
            };
            let count = d.get("count").and_then(number);
            let min = d.get("min").and_then(number).or(count).unwrap_or(1.0).clamp(0.0, 640.0) as u32;
            let max = d.get("max").and_then(number).or(count).unwrap_or(f64::from(min)).clamp(0.0, 640.0) as u32;
            harvest.push(Drop { item, min: min.min(max), max: max.max(min), chance: d.get("chance").and_then(number).unwrap_or(1.0).clamp(0.0, 1.0) });
        }
        if harvest.is_empty() {
            out.push(Note::new(&place, "ไม่มี harvest: เก็บเกี่ยวแล้วไม่ได้อะไร", "no harvest: picking it gives nothing"));
        }
        let flag = |key: &str, or: bool| t.get(key).and_then(toml::Value::as_bool).unwrap_or(or);
        list.push(Crop {
            id: id.clone(),
            name: text_of("name").unwrap_or_else(|| id.clone()).chars().filter(|c| !c.is_control()).take(80).collect(),
            seed,
            soil,
            model,
            stages,
            water: flag("water", false),
            harvest,
            replant: flag("replant", false),
            bone_meal: flag("bone-meal", true),
        });
    }
    list
}

/// All files together: sorted, the first of two crops with one name wins, and one seed plants
/// one crop.
pub fn merge_crops(mut all: Vec<Crop>, out: &mut Vec<Note>) -> Vec<Crop> {
    all.sort_by(|a, b| a.id.cmp(&b.id));
    all.dedup_by(|b, a| {
        let same = a.id == b.id;
        if same {
            out.push(Note::new(&a.id, "มีผักชื่อนี้สองอย่าง ใช้อย่างแรก", "two crops have this name; the first one is used"));
        }
        same
    });
    let mut seen: Vec<ItemRef> = Vec::new();
    all.retain(|c| {
        if seen.contains(&c.seed) {
            out.push(Note::new(&c.id, format!("เมล็ด {} เป็นของผักอื่นแล้ว จึงข้าม", c.seed.text()), format!("seed {} already plants another crop; skipped", c.seed.text())));
            return false;
        }
        seen.push(c.seed.clone());
        true
    });
    if all.len() > MAX_CROPS {
        out.push(Note::new("crops", format!("มีเกิน {MAX_CROPS} อย่าง ส่วนที่เกินถูกข้าม"), format!("more than {MAX_CROPS}; the rest are skipped")));
        all.truncate(MAX_CROPS);
    }
    all
}

/// A plant in the world, as `plants.json` keeps it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plant {
    pub crop: String,
    pub world: String,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub stage: usize,
    /// Seconds it has grown in this stage.
    pub into: f64,
    /// When it was last looked at (Unix seconds): the time since then is grown when it is
    /// looked at again, so a plant grows while nobody is near.
    pub seen: u64,
}

/// A plant at `stage`, `into` seconds into it, grows `seconds` more: its stage and time then.
pub fn grow(stages: &[Stage], mut stage: usize, mut into: f64, seconds: f64) -> (usize, f64) {
    into += seconds.max(0.0);
    while stage + 1 < stages.len() && into >= stages[stage].seconds {
        into -= stages[stage].seconds;
        stage += 1;
    }
    if stage + 1 >= stages.len() {
        into = 0.0;
    }
    (stage, into)
}

/// Seconds until it is ripe.
pub fn until_ripe(stages: &[Stage], stage: usize, into: f64) -> f64 {
    let rest: f64 = stages.iter().skip(stage).take(stages.len().saturating_sub(stage + 1)).map(|s| s.seconds).sum();
    (rest - into).max(0.0)
}

/// How many of a drop come out, for a random number `r`.
pub fn roll(drop: &Drop, r: u64) -> u32 {
    let unit = (r >> 11) as f64 / (1u64 << 53) as f64;
    if unit >= drop.chance {
        return 0;
    }
    let span = u64::from(drop.max - drop.min) + 1;
    drop.min + ((r.rotate_left(17) % span) as u32)
}

/// The next random number (xorshift64*).
pub fn next_random(state: &mut u64) -> u64 {
    let mut x = *state | 1;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

pub const EXAMPLE: &str = r##"# Crops: one table per crop. The table's name is its id (a-z, 0-9, _).
# Players plant one with its seed (right click the soil) and pick it when it is ripe
# (right click). Hitting a plant breaks it: the seed comes back, or the harvest when ripe.
#
# name       what players are told (& colours)
# seed       an item of CustomItems (its id) or of the game (minecraft:<item>); one crop each
# soil       blocks it is planted on (default minecraft:farmland)
# stages     what it looks like as it grows, at least two; the last one is ripe:
#            item = a CustomItems id, minecraft:<item> or any item model (namespace:path),
#            or animation = an animation of `model`; minutes (and/or seconds) to the next stage
# model      a model of the Models plugin (plugins/data/models/models/<name>.bbmodel) whose
#            animations are the stages; drawn standing on the soil
# water      grows only while the farmland under it is wet (default false)
# harvest    what picking it gives: item, count (or min and max), chance (0 to 1)
# replant    after picking it grows again from the first stage (default false: it is gone)
# bone-meal  bone meal grows it one stage (default true)
#
# [tomato]
# name = "&cTomato"
# seed = "tomato_seeds"
# water = true
# replant = true
# stages = [
#   { item = "tomato_small", minutes = 3 },
#   { item = "tomato_green", minutes = 3 },
#   { item = "tomato_ripe" },
# ]
# harvest = [
#   { item = "tomato", min = 1, max = 3 },
#   { item = "tomato_seeds", count = 1, chance = 0.3 },
# ]
#
# [golden_wheat]
# seed = "minecraft:pumpkin_seeds"
# stages = [{ item = "minecraft:short_grass", minutes = 1 }, { item = "minecraft:wheat" }]
# harvest = [{ item = "minecraft:gold_nugget", min = 2, max = 5 }]
"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> (Vec<Crop>, Vec<Note>) {
        let mut notes = Vec::new();
        let list = parse_crops("a.toml", text, &mut notes);
        (list, notes)
    }

    #[test]
    fn the_example_reads_without_a_word_once_it_is_switched_on() {
        assert_eq!(read(EXAMPLE), (Vec::new(), Vec::new()));
        // The examples start at the first table
        let text: String = EXAMPLE.lines().skip_while(|l| !l.starts_with("# [")).map(|l| l.strip_prefix("# ").or_else(|| l.strip_prefix('#')).unwrap_or(l)).collect::<Vec<_>>().join("\n");
        let (list, notes) = read(&text);
        assert!(notes.is_empty(), "{notes:?}\n{text}");
        assert_eq!(list.len(), 2);
        let tomato = list.iter().find(|c| c.id == "tomato").unwrap();
        assert_eq!(tomato.seed, ItemRef::Ours("tomato_seeds".into()));
        assert_eq!(tomato.soil, ["farmland"]);
        assert_eq!(tomato.stages.len(), 3);
        assert_eq!(tomato.stages[0], Stage { look: Look::Model("customitems:tomato_small".into()), seconds: 180.0 });
        assert_eq!(tomato.harvest[0], Drop { item: ItemRef::Ours("tomato".into()), min: 1, max: 3, chance: 1.0 });
        assert_eq!(tomato.harvest[1].chance, 0.3);
        assert!(tomato.water && tomato.replant && tomato.bone_meal);
        let wheat = list.iter().find(|c| c.id == "golden_wheat").unwrap();
        assert_eq!(wheat.seed, ItemRef::Game("minecraft:pumpkin_seeds".into()));
        assert_eq!(wheat.stages[1].look, Look::Game("minecraft:wheat".into()));
        assert!(!wheat.replant && !wheat.water);
    }

    #[test]
    fn what_cannot_be_used_is_said_and_skipped() {
        let (list, notes) = read(concat!(
            "[Bad]\nseed = \"x\"\n",
            "[noseed]\nstages = [{ item = \"a\" }, { item = \"b\" }]\n",
            "[short]\nseed = \"s\"\nstages = [{ item = \"a\" }]\n",
            "[anim]\nseed = \"t\"\nstages = [{ animation = \"grow\" }, { item = \"b\" }]\n",
            "[ok]\nseed = \"u\"\ncolour = 1\nstages = [{ item = \"a\", minutes = 1 }, { item = \"Bad Item!\" }, { item = \"b\" }]\nharvest = [{ item = \"x:y\" }, { item = \"c\", count = 2 }]\n",
        ));
        assert_eq!(list.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["ok"]);
        // Bad, noseed, short, anim (its one stage left), colour, Bad Item!, x:y
        assert_eq!(notes.len(), 8, "{notes:?}");
        assert_eq!(list[0].stages.len(), 2);
        assert_eq!(list[0].harvest, [Drop { item: ItemRef::Ours("c".into()), min: 2, max: 2, chance: 1.0 }]);
    }

    #[test]
    fn one_seed_plants_one_crop() {
        let mut notes = Vec::new();
        let mut all = parse_crops("a", "[b]\nseed = \"s\"\nstages = [{ item = \"x\" }, { item = \"y\" }]\nharvest = [{ item = \"y\" }]\n", &mut notes);
        all.extend(parse_crops("b", "[a]\nseed = \"s\"\nstages = [{ item = \"x\" }, { item = \"y\" }]\nharvest = [{ item = \"y\" }]\n[b]\nseed = \"t\"\nstages = [{ item = \"x\" }, { item = \"y\" }]\nharvest = [{ item = \"y\" }]\n", &mut notes));
        let list = merge_crops(all, &mut notes);
        // [a] takes seed s first by name; the two [b] are one name: the first of them is kept
        assert_eq!(list.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["a"]);
        assert_eq!(notes.len(), 2, "{notes:?}");
    }

    #[test]
    fn a_plant_grows_stage_by_stage_and_stops_when_ripe() {
        let stage = |s: f64| Stage { look: Look::Game("minecraft:wheat".into()), seconds: s };
        let stages = [stage(60.0), stage(120.0), stage(0.0)];
        assert_eq!(grow(&stages, 0, 0.0, 30.0), (0, 30.0));
        assert_eq!(grow(&stages, 0, 30.0, 40.0), (1, 10.0));
        // A long time away: straight to ripe, which does not count on
        assert_eq!(grow(&stages, 0, 0.0, 10_000.0), (2, 0.0));
        assert_eq!(until_ripe(&stages, 0, 30.0), 150.0);
        assert_eq!(until_ripe(&stages, 1, 100.0), 20.0);
        assert_eq!(until_ripe(&stages, 2, 0.0), 0.0);
    }

    #[test]
    fn drops_come_in_their_range_and_as_often_as_their_chance() {
        let drop = Drop { item: ItemRef::Game("minecraft:wheat".into()), min: 1, max: 3, chance: 0.5 };
        let mut state = 7;
        let rolls: Vec<u32> = (0..2000).map(|_| roll(&drop, next_random(&mut state))).collect();
        assert!(rolls.iter().all(|n| *n <= 3));
        let none = rolls.iter().filter(|n| **n == 0).count();
        assert!((800..1200).contains(&none), "{none}");
        for n in 1..=3 {
            assert!(rolls.iter().filter(|r| **r == n).count() > 200, "{n}");
        }
    }
}

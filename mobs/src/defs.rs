//! What the server owner wrote: the settings, the mobs, their skills and drops, and the
//! spawners. No server needed here.
//!
//! A mob is a creature of the game (`type`) that gets other numbers, another behaviour, a
//! name, perhaps a model of the Models plugin, and skills: things it does when something
//! happens to it.
use serde::{Deserialize, Serialize};

/// The namespace of everything this plugin puts onto entities.
pub const NS: &str = "mobs";
pub const MAX_MOBS: usize = 500;
pub const MAX_SKILLS: usize = 32;
pub const MAX_ACTIONS: usize = 16;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// `th` or `en`: the language of the plugin's own messages.
    pub language: String,
    /// A mob with no player this close (blocks) is taken out of the world; a spawner makes a
    /// new one when players are back. 0 = never.
    pub despawn_range: u32,
    /// A spawner works while a player is this close.
    pub spawner_range: u32,
    /// Players this close to a mob with a boss bar see the bar.
    pub bar_range: u32,
    /// The most mobs of this plugin in the world at once.
    pub max_mobs: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), despawn_range: 64, spawner_range: 32, bar_range: 32, max_mobs: 200 }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.despawn_range = if self.despawn_range == 0 { 0 } else { self.despawn_range.clamp(16, 512) };
        self.spawner_range = self.spawner_range.clamp(8, 256);
        self.bar_range = self.bar_range.clamp(4, 256);
        self.max_mobs = self.max_mobs.clamp(1, 5000);
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

/// One thing a mob's own behaviour is made of.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Goal {
    Swim,
    Wander(f32),
    Melee(f32),
    LookAtPlayer(f32),
    LookAround,
    Escape(f32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Drop {
    /// `minecraft:diamond`, or `customitems:ruby` for an item of the CustomItems plugin.
    pub item: String,
    pub min: u32,
    pub max: u32,
    /// 0 to 1.
    pub chance: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Trigger {
    Spawn,
    Hurt,
    Death,
    /// Every this many ticks.
    Timer(u32),
    /// Once, when its health falls below this share (0 to 1).
    Health(f32),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Play an animation of its model once.
    Animation(String),
    /// To the players around it. `%mob%` is its name, `%target%` the player it is after.
    Message(String),
    Sound { id: String, volume: f32, pitch: f32 },
    Particle { name: String, count: u32 },
    /// Hurt every player within `radius` blocks.
    Damage { amount: f32, radius: f32 },
    Heal(f32),
    /// Other mobs of this plugin, next to it.
    Summon { mob: String, count: u32 },
    /// A console command. `%mob%`, `%target%`, `%x%`, `%y%`, `%z%`, `%world%` are filled in.
    Command(String),
    /// A status effect on every player within `radius`.
    Effect { effect: String, seconds: u32, level: u32, radius: f32 },
    /// Jump at the player it is after.
    Leap(f32),
    /// Set the players within `radius` on fire.
    Ignite { seconds: u32, radius: f32 },
    /// Appear at the player it is after.
    Teleport,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub on: Trigger,
    /// 0 to 1.
    pub chance: f32,
    /// Ticks before it may happen again.
    pub cooldown: u32,
    /// Only when a player is this close.
    pub range: Option<f32>,
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MobDef {
    pub id: String,
    /// The creature of the game it is: `zombie`, `skeleton`, `pig`...
    pub base: String,
    pub name: String,
    pub model: Option<String>,
    pub model_scale: Option<f32>,
    pub health: Option<f32>,
    pub damage: Option<f32>,
    pub speed: Option<f32>,
    pub armor: Option<f32>,
    pub follow_range: Option<f32>,
    pub knockback_resistance: Option<f32>,
    pub scale: Option<f32>,
    /// Go for players within its follow range (`None`: as the creature does by itself).
    pub hostile: Option<bool>,
    /// Replace the creature's own behaviour.
    pub ai: Option<Vec<Goal>>,
    pub show_health: bool,
    /// The colour of its boss bar.
    pub bossbar: Option<String>,
    pub glowing: bool,
    pub silent: bool,
    /// (slot, item): `hand`, `offhand`, `head`, `chest`, `legs`, `feet`.
    pub equipment: Vec<(String, String)>,
    pub drops: Vec<Drop>,
    pub exp: Option<u32>,
    pub skills: Vec<Skill>,
}

/// A place that keeps a number of one mob alive while players are near.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spawner {
    pub name: String,
    pub mob: String,
    pub world: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    /// How many of its mobs may be alive at once.
    pub max: u32,
    /// Seconds between two spawns.
    pub seconds: u32,
    /// Blocks around the place where they appear.
    pub radius: f32,
}

/// Lower case letters, digits and `_`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 48 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn number(v: &toml::Value) -> Option<f32> {
    match v {
        toml::Value::Integer(i) => Some(*i as f32),
        toml::Value::Float(f) => Some(*f as f32),
        _ => None,
    }
    .filter(|f| f.is_finite())
}

/// A number from a word, with an optional `%`.
fn word_number(word: &str) -> Option<f32> {
    word.trim_end_matches('%').parse::<f32>().ok().filter(|f| f.is_finite())
}

/// `swim`, `melee 1.2`, `wander 0.8`, `look-at-player 8`, `look-around`, `escape 1.4`.
pub fn parse_goal(text: &str) -> Option<Goal> {
    let mut words = text.split_whitespace();
    let name = words.next()?.to_ascii_lowercase().replace('_', "-");
    let n = words.next().and_then(word_number);
    Some(match name.as_str() {
        "swim" => Goal::Swim,
        "wander" | "wander-around" => Goal::Wander(n.unwrap_or(1.0).clamp(0.1, 4.0)),
        "melee" | "melee-attack" | "attack" => Goal::Melee(n.unwrap_or(1.0).clamp(0.1, 4.0)),
        "look-at-player" | "look" => Goal::LookAtPlayer(n.unwrap_or(8.0).clamp(1.0, 64.0)),
        "look-around" => Goal::LookAround,
        "escape" | "escape-danger" | "panic" => Goal::Escape(n.unwrap_or(1.25).clamp(0.1, 4.0)),
        _ => return None,
    })
}

/// `minecraft:diamond`, `minecraft:diamond 2`, `diamond 1-3 25%`, `customitems:ruby 1 10%`.
pub fn parse_drop(text: &str) -> Option<Drop> {
    let mut words = text.split_whitespace();
    let name = words.next()?.to_ascii_lowercase();
    let item = if name.contains(':') { name } else { format!("minecraft:{name}") };
    let (ns, id) = item.split_once(':')?;
    if !valid_id(ns) || id.is_empty() || !id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'/' | b'.')) {
        return None;
    }
    let (mut min, mut max, mut chance) = (1, 1, 1.0);
    for word in words {
        if let Some(percent) = word.strip_suffix('%') {
            chance = percent.parse::<f32>().ok()? / 100.0;
        } else if let Some((a, b)) = word.split_once('-') {
            (min, max) = (a.parse().ok()?, b.parse().ok()?);
        } else {
            min = word.parse().ok()?;
            max = min;
        }
    }
    (min <= max && max <= 6400 && (0.0..=1.0).contains(&chance)).then_some(Drop { item, min, max, chance })
}

/// `spawn`, `hurt`, `death`, `timer 5` (seconds), `health 50` (below 50%).
pub fn parse_trigger(text: &str) -> Option<Trigger> {
    let mut words = text.split_whitespace();
    let name = words.next()?.to_ascii_lowercase();
    let n = words.next().and_then(word_number);
    Some(match name.as_str() {
        "spawn" => Trigger::Spawn,
        "hurt" | "damaged" => Trigger::Hurt,
        "death" | "die" => Trigger::Death,
        "timer" | "every" => Trigger::Timer((n?.clamp(0.25, 3600.0) * 20.0).round() as u32),
        "health" | "below" => Trigger::Health((n? / 100.0).clamp(0.01, 1.0)),
        _ => return None,
    })
}

/// One action from a line like `damage 4 3` or `message &cRun!`.
pub fn parse_action(text: &str) -> Option<Action> {
    let text = text.trim();
    let (verb, rest) = text.split_once(char::is_whitespace).map_or((text, ""), |(v, r)| (v, r.trim()));
    let words: Vec<&str> = rest.split_whitespace().collect();
    let n = |i: usize| words.get(i).copied().and_then(word_number);
    let plain = |s: &str| !s.is_empty() && s.len() <= 100 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'.' | b'-' | b'/'));
    Some(match verb.to_ascii_lowercase().as_str() {
        "animation" | "play" => Action::Animation(words.first().filter(|w| plain(w))?.to_ascii_lowercase()),
        "message" | "say" if !rest.is_empty() => Action::Message(rest.chars().take(200).collect()),
        "sound" => {
            let name = words.first().filter(|w| plain(w))?.to_ascii_lowercase();
            Action::Sound { id: if name.contains(':') { name } else { format!("minecraft:{name}") }, volume: n(1).unwrap_or(1.0).clamp(0.0, 10.0), pitch: n(2).unwrap_or(1.0).clamp(0.5, 2.0) }
        }
        "particle" => Action::Particle { name: words.first().filter(|w| plain(w))?.to_ascii_lowercase().trim_start_matches("minecraft:").to_owned(), count: n(1).unwrap_or(10.0).clamp(1.0, 500.0) as u32 },
        "damage" => Action::Damage { amount: n(0)?.clamp(0.0, 10_000.0), radius: n(1).unwrap_or(3.0).clamp(0.5, 64.0) },
        "heal" => Action::Heal(n(0)?.clamp(0.0, 100_000.0)),
        "summon" => Action::Summon { mob: words.first().filter(|w| valid_id(w))?.to_string(), count: n(1).unwrap_or(1.0).clamp(1.0, 10.0) as u32 },
        "command" | "run" if !rest.is_empty() => Action::Command(rest.trim_start_matches('/').chars().take(250).collect()),
        "effect" => Action::Effect {
            effect: words.first().filter(|w| plain(w))?.to_ascii_lowercase().trim_start_matches("minecraft:").to_owned(),
            seconds: n(1).unwrap_or(5.0).clamp(1.0, 3600.0) as u32,
            level: n(2).unwrap_or(1.0).clamp(1.0, 255.0) as u32,
            radius: n(3).unwrap_or(6.0).clamp(0.5, 64.0),
        },
        "leap" | "jump" => Action::Leap(n(0).unwrap_or(1.0).clamp(0.1, 4.0)),
        "ignite" | "burn" => Action::Ignite { seconds: n(0).unwrap_or(3.0).clamp(1.0, 600.0) as u32, radius: n(1).unwrap_or(3.0).clamp(0.5, 64.0) },
        "teleport" | "tp" => Action::Teleport,
        _ => return None,
    })
}

fn parse_skill(place: &str, v: &toml::Value, out: &mut Vec<Note>) -> Option<Skill> {
    let t = v.as_table()?;
    let on_text = t.get("on").and_then(toml::Value::as_str).unwrap_or_default();
    let Some(on) = parse_trigger(on_text) else {
        out.push(Note::new(place, format!("ไม่รู้จัก on = \"{on_text}\" (ใช้ spawn, hurt, death, timer <วินาที>, health <เปอร์เซ็นต์>)"), format!("on = \"{on_text}\" is not known (use spawn, hurt, death, timer <seconds>, health <percent>)")));
        return None;
    };
    let mut actions = Vec::new();
    for line in t.get("do").and_then(toml::Value::as_array).into_iter().flatten().filter_map(toml::Value::as_str).take(MAX_ACTIONS) {
        match parse_action(line) {
            Some(a) => actions.push(a),
            None => out.push(Note::new(place, format!("ไม่เข้าใจ \"{line}\" จึงข้าม"), format!("\"{line}\" is not understood and is skipped"))),
        }
    }
    if actions.is_empty() {
        out.push(Note::new(place, "skill ไม่มีอะไรให้ทำ (do)", "a skill has nothing to do (do)"));
        return None;
    }
    Some(Skill {
        on,
        chance: t.get("chance").and_then(number).map_or(1.0, |c| (c / 100.0).clamp(0.0, 1.0)),
        cooldown: t.get("cooldown").and_then(number).map_or(0, |s| (s.clamp(0.0, 86_400.0) * 20.0).round() as u32),
        range: t.get("range").and_then(number).map(|r| r.clamp(0.5, 128.0)),
        actions,
    })
}

const KNOWN: [&str; 23] = [
    "type", "name", "model", "model-scale", "health", "damage", "speed", "armor", "follow-range", "knockback-resistance", "scale", "hostile", "ai", "show-health", "bossbar", "glowing", "silent", "equipment", "drops", "exp", "skills",
    "comment", "note",
];
pub const BAR_COLOURS: [&str; 7] = ["pink", "blue", "red", "green", "yellow", "purple", "white"];
pub const SLOTS: [&str; 6] = ["hand", "offhand", "head", "chest", "legs", "feet"];

/// The mobs of one file. `file` is only used to say where a problem is.
pub fn parse_mobs(file: &str, text: &str, out: &mut Vec<Note>) -> Vec<MobDef> {
    let table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            let why = e.to_string().lines().next().unwrap_or_default().to_owned();
            out.push(Note::new(file, format!("อ่านไม่ได้: {why}"), format!("cannot be read: {why}")));
            return Vec::new();
        }
    };
    let mut mobs = Vec::new();
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
        let text_of = |key: &str| t.get(key).and_then(toml::Value::as_str).map(str::to_owned);
        let num = |key: &str, low: f32, high: f32| t.get(key).and_then(number).map(|n| n.clamp(low, high));
        let flag = |key: &str| t.get(key).and_then(toml::Value::as_bool);
        let base = text_of("type").unwrap_or_else(|| "zombie".into()).to_ascii_lowercase().trim_start_matches("minecraft:").to_owned();
        if !valid_id(&base) {
            out.push(Note::new(&place, "type ต้องเป็นชื่อ mob ของเกม เช่น zombie", "type must be a creature of the game, like zombie"));
            continue;
        }
        let ai = t.get("ai").and_then(toml::Value::as_array).map(|list| {
            list.iter()
                .filter_map(toml::Value::as_str)
                .filter_map(|line| {
                    let goal = parse_goal(line);
                    if goal.is_none() {
                        out.push(Note::new(&place, format!("ไม่รู้จักพฤติกรรม \"{line}\" (ใช้ swim, melee, wander, look-at-player, look-around, escape)"), format!("the behaviour \"{line}\" is not known (use swim, melee, wander, look-at-player, look-around, escape)")));
                    }
                    goal
                })
                .take(12)
                .collect::<Vec<Goal>>()
        });
        let bossbar = match t.get("bossbar") {
            Some(toml::Value::Boolean(true)) => Some("red".to_owned()),
            Some(toml::Value::String(colour)) if BAR_COLOURS.contains(&colour.to_ascii_lowercase().as_str()) => Some(colour.to_ascii_lowercase()),
            Some(toml::Value::String(colour)) => {
                out.push(Note::new(&place, format!("ไม่มีสี bossbar {colour} ใช้ red แทน"), format!("there is no boss bar colour {colour}; red is used")));
                Some("red".to_owned())
            }
            _ => None,
        };
        let mut equipment = Vec::new();
        for (slot, item) in t.get("equipment").and_then(toml::Value::as_table).into_iter().flatten() {
            match (SLOTS.contains(&slot.as_str()), item.as_str()) {
                (true, Some(item)) => equipment.push((slot.clone(), if item.contains(':') { item.to_ascii_lowercase() } else { format!("minecraft:{}", item.to_ascii_lowercase()) })),
                _ => out.push(Note::new(&place, format!("equipment.{slot} ใช้ไม่ได้ (ช่อง: hand, offhand, head, chest, legs, feet)"), format!("equipment.{slot} cannot be used (slots: hand, offhand, head, chest, legs, feet)"))),
            }
        }
        let mut drops = Vec::new();
        for line in t.get("drops").and_then(toml::Value::as_array).into_iter().flatten().filter_map(toml::Value::as_str).take(32) {
            match parse_drop(line) {
                Some(d) => drops.push(d),
                None => out.push(Note::new(&place, format!("ไม่เข้าใจของดรอป \"{line}\" (เช่น \"minecraft:diamond 1-2 25%\")"), format!("the drop \"{line}\" is not understood (like \"minecraft:diamond 1-2 25%\")"))),
            }
        }
        let skills = t.get("skills").and_then(toml::Value::as_array).into_iter().flatten().take(MAX_SKILLS).filter_map(|s| parse_skill(&place, s, out)).collect();
        mobs.push(MobDef {
            id: id.clone(),
            base,
            name: text_of("name").unwrap_or_else(|| id.clone()).chars().filter(|c| !c.is_control()).take(80).collect(),
            model: text_of("model").filter(|m| !m.is_empty()),
            model_scale: num("model-scale", 0.01, 100.0),
            health: num("health", 1.0, 1_000_000.0),
            damage: num("damage", 0.0, 100_000.0),
            speed: num("speed", 0.0, 4.0),
            armor: num("armor", 0.0, 30.0),
            follow_range: num("follow-range", 1.0, 128.0),
            knockback_resistance: num("knockback-resistance", 0.0, 1.0),
            scale: num("scale", 0.1, 16.0),
            hostile: flag("hostile"),
            ai,
            show_health: flag("show-health").unwrap_or(false),
            bossbar,
            glowing: flag("glowing").unwrap_or(false),
            silent: flag("silent").unwrap_or(false),
            equipment,
            drops,
            exp: num("exp", 0.0, 100_000.0).map(|e| e as u32),
            skills,
        });
    }
    mobs
}

/// All files together: sorted, and the first of two mobs with one name wins.
pub fn merge_mobs(mut all: Vec<MobDef>, out: &mut Vec<Note>) -> Vec<MobDef> {
    all.sort_by(|a, b| a.id.cmp(&b.id));
    let before = all.len();
    all.dedup_by(|b, a| {
        let same = a.id == b.id;
        if same {
            out.push(Note::new(&a.id, "มี mob ชื่อนี้สองตัว ใช้ตัวแรก", "two mobs have this name; the first one is used"));
        }
        same
    });
    if before > MAX_MOBS {
        all.truncate(MAX_MOBS);
    }
    // A summon of a mob that does not exist would do nothing, silently
    let ids: Vec<String> = all.iter().map(|m| m.id.clone()).collect();
    for mob in &all {
        for skill in &mob.skills {
            for action in &skill.actions {
                if let Action::Summon { mob: other, .. } = action
                    && !ids.contains(other)
                {
                    out.push(Note::new(&mob.id, format!("summon {other}: ไม่มี mob นี้"), format!("summon {other}: there is no such mob")));
                }
            }
        }
    }
    all
}

/// A small random source: the plugin needs chances, not secrets.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0.max(1);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// 0 to 1, never 1.
    pub fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    pub fn chance(&mut self, p: f32) -> bool {
        p >= 1.0 || self.unit() < p
    }
    pub fn between(&mut self, min: u32, max: u32) -> u32 {
        if max <= min { min } else { min + (self.next() % u64::from(max - min + 1)) as u32 }
    }
}

/// What a mob leaves behind this time: (item, how many).
pub fn roll_drops(drops: &[Drop], rng: &mut Rng) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    for d in drops {
        if rng.chance(d.chance) {
            let n = rng.between(d.min, d.max);
            if n > 0 {
                out.push((d.item.clone(), n));
            }
        }
    }
    out
}

/// The name above a mob, with its health when asked for.
pub fn shown_name(def: &MobDef, health: f32, max: f32) -> String {
    if def.show_health { format!("{} &c{}&7/&c{}", def.name, health.max(0.0).ceil() as u32, max.ceil() as u32) } else { def.name.clone() }
}

/// Fill `%mob%`, `%target%`, `%x%`... into a message or a command.
pub fn fill(text: &str, mob: &str, target: &str, at: (f64, f64, f64), world: &str) -> String {
    // A name with colour codes in a command would be read as part of it
    let plain: String = {
        let mut out = String::new();
        let mut chars = mob.chars();
        while let Some(c) = chars.next() {
            if c == '&' || c == '§' {
                chars.next();
            } else {
                out.push(c);
            }
        }
        out
    };
    text.replace("%mob%", &plain).replace("%target%", target).replace("%x%", &format!("{:.2}", at.0)).replace("%y%", &format!("{:.2}", at.1)).replace("%z%", &format!("{:.2}", at.2)).replace("%world%", world)
}

pub const EXAMPLE_MOBS: &str = r##"# Mobs: one table per mob. The table's name is the mob's id (a-z, 0-9, _).
# Remove the "#" in front of an example to try it, then run /mobs reload and /mobs spawn <id>.
#
# [ruby_guard]
# type = "zombie"                  # the creature of the game it really is
# name = "&cRuby Guard"
# health = 40                      # 20 = ten hearts
# damage = 6
# speed = 0.28                     # a zombie has 0.23
# armor = 4
# follow-range = 24
# knockback-resistance = 0.5       # 0 to 1
# scale = 1.2
# show-health = true               # its health behind its name
# drops = ["minecraft:redstone 2-4", "minecraft:diamond 1 10%"]
# exp = 15
#
# [ruby_guard.equipment]
# hand = "minecraft:iron_sword"
# head = "minecraft:iron_helmet"
#
# [[ruby_guard.skills]]            # every 6 seconds, when a player is within 5 blocks
# on = "timer 6"
# range = 5
# do = ["message &c%mob% slams the ground!", "damage 4 5", "sound minecraft:entity.generic.explode 1 0.8", "particle explosion 3"]
#
# [[ruby_guard.skills]]            # once, when it is down to 30% of its health
# on = "health 30"
# do = ["heal 10", "effect slowness 5 2 8", "summon ruby_minion 2"]
#
# [[ruby_guard.skills]]
# on = "death"
# do = ["command say %mob% was defeated by %target%"]
#
# [ruby_minion]
# type = "zombie"
# name = "&7Minion"
# health = 8
# scale = 0.7
# speed = 0.3
#
# [king_boar]                      # a pig that fights: its own behaviour is replaced
# type = "pig"
# name = "&6King Boar"
# model = "example"                # a model of the Models plugin, when it is installed
# health = 60
# damage = 5
# hostile = true                   # it goes for players within follow-range
# ai = ["swim", "melee 1.3", "wander 0.9", "look-at-player 10", "look-around"]
# bossbar = "yellow"               # a bar at the top of the screen for players near it
# drops = ["minecraft:porkchop 3-6", "customitems:ruby 1 50%"]   # an item of the CustomItems plugin
#
# [[king_boar.skills]]
# on = "timer 8"
# range = 12
# chance = 60
# do = ["animation attack", "leap 1.2"]
"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn mobs(text: &str) -> (Vec<MobDef>, Vec<Note>) {
        let mut notes = Vec::new();
        let list = parse_mobs("mobs/test.toml", text, &mut notes);
        let list = merge_mobs(list, &mut notes);
        (list, notes)
    }

    #[test]
    fn the_example_reads_without_complaints() {
        // The example file as it is has only comments
        assert_eq!(mobs(EXAMPLE_MOBS), (Vec::new(), Vec::new()));
        let text: String = EXAMPLE_MOBS.lines().map(|l| l.strip_prefix("# ").or_else(|| l.strip_prefix('#')).unwrap_or(l)).filter(|l| l.starts_with('[') || l.contains(" = ")).collect::<Vec<_>>().join("\n");
        let (list, notes) = mobs(&text);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(list.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["king_boar", "ruby_guard", "ruby_minion"]);
        let guard = &list[1];
        assert_eq!((guard.base.as_str(), guard.health, guard.damage, guard.scale, guard.show_health, guard.exp), ("zombie", Some(40.0), Some(6.0), Some(1.2), true, Some(15)));
        assert_eq!(guard.equipment, [("hand".to_owned(), "minecraft:iron_sword".to_owned()), ("head".to_owned(), "minecraft:iron_helmet".to_owned())]);
        assert_eq!(guard.drops, [Drop { item: "minecraft:redstone".into(), min: 2, max: 4, chance: 1.0 }, Drop { item: "minecraft:diamond".into(), min: 1, max: 1, chance: 0.1 }]);
        assert_eq!(guard.skills.len(), 3);
        assert_eq!((guard.skills[0].on, guard.skills[0].range, guard.skills[0].actions.len()), (Trigger::Timer(120), Some(5.0), 4));
        assert_eq!(guard.skills[0].actions[1], Action::Damage { amount: 4.0, radius: 5.0 });
        assert_eq!(guard.skills[1].on, Trigger::Health(0.3));
        assert_eq!(guard.skills[1].actions, [Action::Heal(10.0), Action::Effect { effect: "slowness".into(), seconds: 5, level: 2, radius: 8.0 }, Action::Summon { mob: "ruby_minion".into(), count: 2 }]);
        assert_eq!(guard.skills[2].actions, [Action::Command("say %mob% was defeated by %target%".into())]);
        let boar = &list[0];
        assert_eq!((boar.hostile, boar.model.as_deref(), boar.bossbar.as_deref()), (Some(true), Some("example"), Some("yellow")));
        assert_eq!(boar.ai.as_deref(), Some(&[Goal::Swim, Goal::Melee(1.3), Goal::Wander(0.9), Goal::LookAtPlayer(10.0), Goal::LookAround][..]));
        assert_eq!((boar.skills[0].chance, boar.skills[0].actions.clone()), (0.6, vec![Action::Animation("attack".into()), Action::Leap(1.2)]));
        assert_eq!(boar.drops[1].item, "customitems:ruby");
    }

    #[test]
    fn what_is_wrong_is_said_and_skipped() {
        let (list, notes) = mobs(
            r#"
            [Bad-Name]
            type = "zombie"
            [odd]
            type = "zombie"
            colour = "red"
            ai = ["swim", "fly 2"]
            bossbar = "orange"
            drops = ["minecraft:diamond lots", "stick 2"]
            [odd.equipment]
            tail = "minecraft:stick"
            hand = "stick"
            [[odd.skills]]
            on = "sunrise"
            do = ["heal 1"]
            [[odd.skills]]
            on = "hurt"
            do = ["dance", "summon nobody"]
            [[odd.skills]]
            on = "hurt"
            do = ["dance"]
            "#,
        );
        assert_eq!(list.len(), 1);
        let odd = &list[0];
        assert_eq!((odd.ai.as_deref(), odd.bossbar.as_deref()), (Some(&[Goal::Swim][..]), Some("red")));
        assert_eq!(odd.drops, [Drop { item: "minecraft:stick".into(), min: 2, max: 2, chance: 1.0 }]);
        assert_eq!(odd.equipment, [("hand".to_owned(), "minecraft:stick".to_owned())]);
        assert_eq!(odd.skills.len(), 1);
        for part in ["Bad-Name", "colour", "fly 2", "orange", "lots", "tail", "sunrise", "dance", "nothing to do", "summon nobody"] {
            assert!(notes.iter().any(|n| n.en.contains(part)), "{part}: {notes:?}");
        }
        let mut out = Vec::new();
        assert!(parse_mobs("x.toml", "not [toml", &mut out).is_empty());
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn two_mobs_with_one_name() {
        let mut notes = Vec::new();
        let mut all = parse_mobs("a.toml", "[x]\nname = \"first\"", &mut notes);
        all.extend(parse_mobs("b.toml", "[x]\nname = \"second\"", &mut notes));
        let list = merge_mobs(all, &mut notes);
        assert_eq!((list.len(), list[0].name.as_str(), notes.len()), (1, "first", 1));
    }

    #[test]
    fn actions_from_lines() {
        assert_eq!(parse_action("sound entity.zombie.hurt"), Some(Action::Sound { id: "minecraft:entity.zombie.hurt".into(), volume: 1.0, pitch: 1.0 }));
        assert_eq!(parse_action("sound my_pack:roar 2 0.5"), Some(Action::Sound { id: "my_pack:roar".into(), volume: 2.0, pitch: 0.5 }));
        assert_eq!(parse_action("particle minecraft:flame"), Some(Action::Particle { name: "flame".into(), count: 10 }));
        assert_eq!(parse_action("message  &cHello there "), Some(Action::Message("&cHello there".into())));
        assert_eq!(parse_action("command /say hi"), Some(Action::Command("say hi".into())));
        assert_eq!(parse_action("ignite"), Some(Action::Ignite { seconds: 3, radius: 3.0 }));
        assert_eq!(parse_action("teleport"), Some(Action::Teleport));
        assert_eq!(parse_action("effect poison"), Some(Action::Effect { effect: "poison".into(), seconds: 5, level: 1, radius: 6.0 }));
        for bad in ["", "message", "damage", "damage lots", "summon Bad-Name", "sound", "sound a;b", "fly"] {
            assert_eq!(parse_action(bad), None, "{bad}");
        }
        assert_eq!(parse_trigger("timer 0.5"), Some(Trigger::Timer(10)));
        assert_eq!(parse_trigger("health 50%"), Some(Trigger::Health(0.5)));
        assert_eq!(parse_trigger("timer"), None);
        assert_eq!(parse_drop("diamond 3-1"), None);
        assert_eq!(parse_drop("customitems:ruby 2 150%"), None);
    }

    #[test]
    fn chances_names_and_placeholders() {
        let mut rng = Rng(42);
        let drops = [Drop { item: "a:always".into(), min: 2, max: 5, chance: 1.0 }, Drop { item: "a:never".into(), min: 1, max: 1, chance: 0.0 }, Drop { item: "a:half".into(), min: 1, max: 1, chance: 0.5 }];
        let (mut half, mut total) = (0, 0);
        for _ in 0..2000 {
            let got = roll_drops(&drops, &mut rng);
            assert!(got.iter().any(|(item, n)| item == "a:always" && (2..=5).contains(n)));
            assert!(!got.iter().any(|(item, _)| item == "a:never"));
            half += got.iter().filter(|(item, _)| item == "a:half").count();
            total += 1;
        }
        assert!((800..1200).contains(&half), "{half} of {total}");
        assert!((0..1000).all(|_| rng.unit() < 1.0));
        assert_eq!(rng.between(3, 3), 3);

        let def = MobDef { name: "&cGuard".into(), show_health: true, ..MobDef::default() };
        assert_eq!(shown_name(&def, 12.3, 40.0), "&cGuard &c13&7/&c40");
        assert_eq!(shown_name(&MobDef { name: "Pig".into(), ..MobDef::default() }, 1.0, 2.0), "Pig");
        assert_eq!(fill("say %mob% hit %target% at %x% %y% %z% in %world%", "&cRuby &lGuard", "Art", (1.0, 64.5, -2.25), "minecraft:overworld"), "say Ruby Guard hit Art at 1.00 64.50 -2.25 in minecraft:overworld");
        let cfg = Config { despawn_range: 5, max_mobs: 0, language: "EN".into(), ..Config::default() }.cleaned();
        assert_eq!((cfg.despawn_range, cfg.max_mobs, cfg.thai()), (16, 1, false));
        assert_eq!(Config { despawn_range: 0, ..Config::default() }.cleaned().despawn_range, 0);
    }
}

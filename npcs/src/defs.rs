//! What an NPC is and what clicking it does. No server needed here.
//!
//! The NPCs live in `npcs.json`, written by the `/npc` commands; an owner may edit it by hand
//! and `/npc reload`. Actions are short lines:
//!
//! | Action | Does |
//! |---|---|
//! | `message <text>` | tells the player (`&` colours, `{player}`, `%placeholders%`) |
//! | `command <command>` | runs a command as the player |
//! | `console <command>` | runs a command as the console (`{player}` is the player's name) |
//! | `dialog` | opens the NPC's dialog window |
//! | `quest <id>` | offers a quest of the Quests plugin |
//! | `teleport <x> <y> <z> [world]` | moves the player |
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const NS: &str = "npcs";
pub const MAX_NPCS: usize = 500;
pub const MAX_ACTIONS: usize = 32;
pub const MAX_PATH: usize = 64;
pub const MAX_BUTTONS: usize = 8;
pub const MAX_LINES: usize = 16;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// NPCs exist as entities only while a player is this many blocks away.
    pub show_distance: f32,
    /// An NPC turns to the nearest player this close.
    pub look_distance: f32,
    /// A player can use an NPC from this far.
    pub click_distance: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), show_distance: 48.0, look_distance: 8.0, click_distance: 6.0 }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        let fix = |v: f32, lo: f32, hi: f32, def: f32| if v.is_finite() { v.clamp(lo, hi) } else { def };
        self.show_distance = fix(self.show_distance, 8.0, 128.0, 48.0);
        self.look_distance = fix(self.look_distance, 0.0, 32.0, 8.0);
        self.click_distance = fix(self.click_distance, 2.0, 16.0, 6.0);
        self
    }

    pub fn thai(&self) -> bool {
        self.language == "th"
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Button {
    pub label: String,
    pub actions: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dialog {
    pub title: String,
    pub lines: Vec<String>,
    pub buttons: Vec<Button>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Npc {
    pub id: String,
    /// Shown over its head; empty = no name.
    pub name: String,
    /// The creature it is (`villager`, `zombie`, …).
    #[serde(rename = "type")]
    pub kind: String,
    /// A model of the Models plugin drawn over the creature.
    pub model: Option<String>,
    pub world: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    /// Turns to the nearest player.
    pub look: bool,
    pub glowing: bool,
    /// Done when a player right clicks it.
    pub actions: Vec<String>,
    /// Seconds before the same player can use it again.
    pub cooldown: f64,
    /// Points it walks between, in a loop; empty = it stands still.
    pub path: Vec<[f64; 3]>,
    /// Blocks a second.
    pub speed: f64,
    /// Seconds it stays at each point.
    pub wait: f64,
    /// Slot (`hand`, `offhand`, `head`, `chest`, `legs`, `feet`) to an item text.
    pub equipment: BTreeMap<String, String>,
    pub dialog: Option<Dialog>,
    /// Needed to use it; none = everyone.
    pub permission: Option<String>,
}

impl Default for Npc {
    fn default() -> Self {
        Npc {
            id: String::new(),
            name: String::new(),
            kind: "villager".into(),
            model: None,
            world: "minecraft:overworld".into(),
            x: 0.0,
            y: 0.0,
            z: 0.0,
            yaw: 0.0,
            look: true,
            glowing: false,
            actions: Vec::new(),
            cooldown: 1.0,
            path: Vec::new(),
            speed: 1.5,
            wait: 2.0,
            equipment: BTreeMap::new(),
            dialog: None,
            permission: None,
        }
    }
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

pub const SLOTS: [&str; 6] = ["hand", "offhand", "head", "chest", "legs", "feet"];

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Message(String),
    Command(String),
    Console(String),
    Dialog,
    Quest(String),
    Teleport { x: f64, y: f64, z: f64, world: Option<String> },
}

/// One action line. `Err` says what is wrong, in both languages.
pub fn parse_action(line: &str) -> Result<Action, (String, String)> {
    let line = line.trim();
    let (word, rest) = line.split_once(' ').map_or((line, ""), |(w, r)| (w, r.trim()));
    let need = |what: &str| (format!("{word} ต้องมี{what}"), format!("{word} needs {what}"));
    match word.to_ascii_lowercase().as_str() {
        "message" | "msg" | "say" if !rest.is_empty() => Ok(Action::Message(rest.to_owned())),
        "command" | "cmd" if !rest.is_empty() => Ok(Action::Command(rest.trim_start_matches('/').to_owned())),
        "console" if !rest.is_empty() => Ok(Action::Console(rest.trim_start_matches('/').to_owned())),
        "dialog" => Ok(Action::Dialog),
        "quest" if valid_id(rest) => Ok(Action::Quest(rest.to_owned())),
        "teleport" | "tp" => {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            let num = |i: usize| parts.get(i).and_then(|t| t.parse::<f64>().ok()).filter(|n| n.is_finite() && n.abs() < 3.0e7);
            match (num(0), num(1), num(2)) {
                (Some(x), Some(y), Some(z)) if parts.len() <= 4 => Ok(Action::Teleport { x, y, z, world: parts.get(3).map(|w| if w.contains(':') { (*w).to_owned() } else { format!("minecraft:{w}") }) }),
                _ => Err(need("x y z [โลก]")),
            }
        }
        "message" | "msg" | "say" => Err(need("ข้อความ")),
        "command" | "cmd" | "console" => Err(need("คำสั่ง")),
        "quest" => Err(need("ชื่อเควสต์")),
        _ => Err((format!("ไม่รู้จัก {word} (ใช้ message, command, console, dialog, quest, teleport)"), format!("{word} is not known (use message, command, console, dialog, quest, teleport)"))),
    }
}

/// `{player}` in an action becomes the player's name.
pub fn fill(text: &str, player: &str) -> String {
    text.replace("{player}", player)
}

/// What is wrong with an NPC read from the file, in both languages. Bad actions and points
/// are dropped from it.
pub fn clean(npc: &mut Npc) -> Vec<(String, String)> {
    let mut notes = Vec::new();
    let mut keep = Vec::new();
    for line in npc.actions.iter().take(MAX_ACTIONS) {
        match parse_action(line) {
            Ok(_) => keep.push(line.clone()),
            Err((th, en)) => notes.push((format!("{}: {th}", npc.id), format!("{}: {en}", npc.id))),
        }
    }
    npc.actions = keep;
    if let Some(dialog) = &mut npc.dialog {
        dialog.lines.truncate(MAX_LINES);
        dialog.buttons.truncate(MAX_BUTTONS);
        for button in &mut dialog.buttons {
            button.actions.retain(|a| match parse_action(a) {
                Ok(Action::Dialog) => false,
                Ok(_) => true,
                Err((th, en)) => {
                    notes.push((format!("{}: {th}", npc.id), format!("{}: {en}", npc.id)));
                    false
                }
            });
        }
    }
    npc.path.retain(|p| p.iter().all(|n| n.is_finite() && n.abs() < 3.0e7));
    npc.path.truncate(MAX_PATH);
    npc.equipment.retain(|slot, _| SLOTS.contains(&slot.as_str()));
    if !npc.cooldown.is_finite() || npc.cooldown < 0.0 {
        npc.cooldown = 1.0;
    }
    npc.cooldown = npc.cooldown.min(86_400.0);
    npc.speed = if npc.speed.is_finite() { npc.speed.clamp(0.2, 10.0) } else { 1.5 };
    npc.wait = if npc.wait.is_finite() { npc.wait.clamp(0.0, 600.0) } else { 2.0 };
    npc.name = npc.name.chars().take(120).collect();
    npc
        .permission
        .take_if(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b)));
    notes
}

/// The NPCs of the file, cleaned; bad ones are left out with a note.
pub fn read_npcs(list: Vec<Npc>, notes: &mut Vec<(String, String)>) -> Vec<Npc> {
    let mut out: Vec<Npc> = Vec::new();
    for mut npc in list {
        if !valid_id(&npc.id) {
            notes.push((format!("{}: ชื่อใช้ได้แค่ a-z, 0-9 และ _", npc.id), format!("{}: a name may only have a-z, 0-9 and _", npc.id)));
            continue;
        }
        if out.iter().any(|o| o.id == npc.id) {
            notes.push((format!("{}: ชื่อซ้ำ ใช้ตัวแรก", npc.id), format!("{}: the name is used twice; the first one counts", npc.id)));
            continue;
        }
        if out.len() >= MAX_NPCS {
            notes.push((format!("มีได้ไม่เกิน {MAX_NPCS} ตัว"), format!("at most {MAX_NPCS} NPCs")));
            break;
        }
        notes.extend(clean(&mut npc));
        out.push(npc);
    }
    out
}

/// Where an NPC that walks its path is after moving `step` blocks towards `to`, and whether
/// it got there.
pub fn step_towards(from: (f64, f64, f64), to: (f64, f64, f64), step: f64) -> ((f64, f64, f64), bool) {
    let d = (to.0 - from.0, to.1 - from.1, to.2 - from.2);
    let len = (d.0 * d.0 + d.1 * d.1 + d.2 * d.2).sqrt();
    if len <= step || len < 1e-6 {
        return (to, true);
    }
    let k = step / len;
    ((from.0 + d.0 * k, from.1 + d.1 * k, from.2 + d.2 * k), false)
}

/// The yaw (game degrees) of looking from `from` towards `to`.
pub fn yaw_towards(from: (f64, f64, f64), to: (f64, f64, f64)) -> f32 {
    let (dx, dz) = (to.0 - from.0, to.2 - from.2);
    (-dx.atan2(dz).to_degrees()) as f32
}

/// `label | action && action`: a dialog button from a command.
pub fn parse_button(text: &str) -> Result<Button, (String, String)> {
    let (label, actions) = text.split_once('|').ok_or_else(|| ("ใช้ <ชื่อปุ่ม> | <การกระทำ> && <การกระทำ>".to_owned(), "use <label> | <action> && <action>".to_owned()))?;
    let label = label.trim();
    if label.is_empty() {
        return Err(("ปุ่มต้องมีชื่อ".into(), "a button needs a label".into()));
    }
    let mut list = Vec::new();
    for a in actions.split("&&").map(str::trim).filter(|a| !a.is_empty()) {
        match parse_action(a)? {
            Action::Dialog => return Err(("ปุ่มเปิด dialog ซ้ำไม่ได้".into(), "a button cannot open the dialog again".into())),
            _ => list.push(a.to_owned()),
        }
    }
    Ok(Button { label: label.chars().take(60).collect(), actions: list })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_are_read_or_explained() {
        assert_eq!(parse_action("message &aHi {player}"), Ok(Action::Message("&aHi {player}".into())));
        assert_eq!(parse_action("command /spawn"), Ok(Action::Command("spawn".into())));
        assert_eq!(parse_action("console give {player} diamond 1"), Ok(Action::Console("give {player} diamond 1".into())));
        assert_eq!(parse_action("dialog"), Ok(Action::Dialog));
        assert_eq!(parse_action("quest wood_1"), Ok(Action::Quest("wood_1".into())));
        assert_eq!(parse_action("tp 1 64.5 -3 the_nether"), Ok(Action::Teleport { x: 1.0, y: 64.5, z: -3.0, world: Some("minecraft:the_nether".into()) }));
        assert!(parse_action("tp 1 2").is_err());
        assert!(parse_action("message").is_err());
        assert!(parse_action("fly away").is_err());
        assert!(parse_action("quest Bad-Name").is_err());
    }

    #[test]
    fn a_file_npc_is_cleaned() {
        let mut notes = Vec::new();
        let list = vec![
            Npc { id: "guide".into(), actions: vec!["message hi".into(), "jump".into()], speed: f64::NAN, path: vec![[0.0, 1.0, f64::INFINITY], [1.0, 2.0, 3.0]], ..Npc::default() },
            Npc { id: "guide".into(), ..Npc::default() },
            Npc { id: "Bad".into(), ..Npc::default() },
        ];
        let out = read_npcs(list, &mut notes);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].actions, vec!["message hi".to_owned()]);
        assert_eq!(out[0].path, vec![[1.0, 2.0, 3.0]]);
        assert_eq!(out[0].speed, 1.5);
        assert_eq!(notes.len(), 3);
    }

    #[test]
    fn buttons_split_label_and_actions() {
        let b = parse_button("&aShop | command shop && message &7Welcome").unwrap();
        assert_eq!(b.label, "&aShop");
        assert_eq!(b.actions, vec!["command shop".to_owned(), "message &7Welcome".to_owned()]);
        assert!(parse_button("no bar").is_err());
        assert!(parse_button("x | dialog").is_err());
        assert!(parse_button(" | message hi").is_err());
    }

    #[test]
    fn walking_reaches_its_point() {
        let (at, there) = step_towards((0.0, 0.0, 0.0), (3.0, 0.0, 4.0), 1.0);
        assert!(!there);
        assert!((at.0 - 0.6).abs() < 1e-9 && (at.2 - 0.8).abs() < 1e-9);
        assert_eq!(step_towards((0.0, 0.0, 0.0), (0.5, 0.0, 0.0), 1.0), ((0.5, 0.0, 0.0), true));
        // South is yaw 0, west is 90
        assert!((yaw_towards((0.0, 0.0, 0.0), (0.0, 0.0, 1.0))).abs() < 1e-4);
        assert!((yaw_towards((0.0, 0.0, 0.0), (-1.0, 0.0, 0.0)) - 90.0).abs() < 1e-4);
    }
}

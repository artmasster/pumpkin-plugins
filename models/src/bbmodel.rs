//! Reading a Blockbench project (`.bbmodel`): the cubes, the groups they are in, the pictures
//! and the animations, as the file has them. Nothing is interpreted here; `rig` does that.
//!
//! The file is JSON. Blockbench has changed its shape over the years, so everything optional
//! is read with a default and unknown things are passed over.
use std::collections::HashMap;

use serde_json::Value;

use crate::molang::{self, Expr};

/// The six sides of a cube, in the order used everywhere in this crate.
pub const SIDES: [&str; 6] = ["north", "east", "south", "west", "up", "down"];

#[derive(Clone, Debug, PartialEq)]
pub struct RawFace {
    /// Left, top, right, bottom in pixels of the picture.
    pub uv: [f32; 4],
    pub texture: usize,
    /// 0, 90, 180 or 270.
    pub rotation: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawCube {
    pub name: String,
    pub from: [f32; 3],
    pub to: [f32; 3],
    pub origin: [f32; 3],
    pub rotation: [f32; 3],
    pub inflate: f32,
    pub faces: [Option<RawFace>; 6],
    pub visible: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawGroup {
    pub name: String,
    pub uuid: String,
    pub origin: [f32; 3],
    pub rotation: [f32; 3],
    pub children: Vec<Node>,
    pub visible: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Cube(String),
    Group(RawGroup),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawTexture {
    pub name: String,
    pub png: Vec<u8>,
    /// The size its UV numbers are measured in (0 = the project's).
    pub uv_width: f32,
    pub uv_height: f32,
    /// Ticks per frame when the picture is a strip of frames.
    pub frame_time: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Rotation,
    Position,
    Scale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interp {
    Linear,
    Step,
    Smooth,
    Bezier,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bezier {
    pub left_time: [f32; 3],
    pub left_value: [f32; 3],
    pub right_time: [f32; 3],
    pub right_value: [f32; 3],
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawKey {
    pub channel: Channel,
    /// Seconds.
    pub time: f32,
    pub interp: Interp,
    /// The value when arriving at this keyframe, and when leaving it (usually the same).
    pub pre: [Expr; 3],
    pub post: [Expr; 3],
    pub bezier: Option<Bezier>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawAnimator {
    pub uuid: String,
    pub name: String,
    pub keys: Vec<RawKey>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loop {
    Once,
    Hold,
    Loop,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawAnimation {
    pub name: String,
    pub looping: Loop,
    /// Seconds.
    pub length: f32,
    pub animators: Vec<RawAnimator>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Raw {
    pub resolution: (f32, f32),
    pub cubes: HashMap<String, RawCube>,
    pub roots: Vec<Node>,
    pub textures: Vec<RawTexture>,
    pub animations: Vec<RawAnimation>,
    /// What the file has that a model in the game cannot show, in plain words.
    pub notes: Vec<String>,
}

fn num(v: Option<&Value>) -> Option<f32> {
    match v? {
        Value::Number(n) => n.as_f64().map(|f| f as f32),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|f: &f32| f.is_finite())
}

fn triple(v: Option<&Value>, default: f32) -> [f32; 3] {
    let list = v.and_then(Value::as_array);
    [0, 1, 2].map(|i| list.and_then(|l| num(l.get(i))).unwrap_or(default))
}

fn text(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// Standard base64 (padding and line breaks allowed).
pub fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in text.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn cube(v: &Value, textures: usize) -> RawCube {
    let faces = SIDES.map(|side| {
        let f = v.get("faces")?.get(side)?;
        // A face with no picture is not drawn
        let texture = f.get("texture").and_then(Value::as_u64).map(|t| t as usize).filter(|t| *t < textures)?;
        let list = f.get("uv")?.as_array()?;
        let uv = [0, 1, 2, 3].map(|i| num(list.get(i)).unwrap_or(0.0));
        let rotation = num(f.get("rotation")).map_or(0, |r| (r as i32).rem_euclid(360) as u16 / 90 * 90);
        Some(RawFace { uv, texture, rotation })
    });
    RawCube {
        name: text(v.get("name")),
        from: triple(v.get("from"), 0.0),
        to: triple(v.get("to"), 0.0),
        origin: triple(v.get("origin"), 0.0),
        rotation: triple(v.get("rotation"), 0.0),
        inflate: num(v.get("inflate")).unwrap_or(0.0),
        faces,
        visible: v.get("visibility").and_then(Value::as_bool).unwrap_or(true),
    }
}

fn node(v: &Value, groups: &HashMap<String, &Value>, depth: usize) -> Option<Node> {
    if let Some(uuid) = v.as_str() {
        return Some(Node::Cube(uuid.to_owned()));
    }
    if depth > 64 {
        return None;
    }
    let uuid = text(v.get("uuid"));
    // Since Blockbench 5 the outliner only names a group; what it is stands in `groups`
    let own = if v.get("name").is_some() { v } else { groups.get(&uuid).copied().unwrap_or(v) };
    let children = v.get("children").or_else(|| own.get("children")).and_then(Value::as_array).map(|list| list.iter().filter_map(|c| node(c, groups, depth + 1)).collect()).unwrap_or_default();
    Some(Node::Group(RawGroup {
        name: text(own.get("name")),
        uuid,
        origin: triple(own.get("origin"), 0.0),
        rotation: triple(own.get("rotation"), 0.0),
        children,
        visible: own.get("visibility").and_then(Value::as_bool).unwrap_or(true),
    }))
}

fn expr(v: Option<&Value>, default: f32, notes: &mut Vec<String>, animation: &str) -> Expr {
    match v {
        Some(Value::Number(n)) => Expr::Num(n.as_f64().unwrap_or(0.0) as f32),
        Some(Value::String(s)) => molang::parse(s).unwrap_or_else(|| {
            let note = format!("animation {animation}: \"{s}\" is not understood and counts as 0");
            if !notes.contains(&note) {
                notes.push(note);
            }
            Expr::Num(default)
        }),
        _ => Expr::Num(default),
    }
}

fn animation(v: &Value, notes: &mut Vec<String>) -> RawAnimation {
    let name = text(v.get("name"));
    let mut animators = Vec::new();
    if let Some(map) = v.get("animators").and_then(Value::as_object) {
        for (uuid, a) in map {
            let mut keys = Vec::new();
            for k in a.get("keyframes").and_then(Value::as_array).into_iter().flatten() {
                let channel = match k.get("channel").and_then(Value::as_str) {
                    Some("rotation") => Channel::Rotation,
                    Some("position") => Channel::Position,
                    Some("scale") => Channel::Scale,
                    // Sounds, particles and timeline instructions are for other programs
                    _ => continue,
                };
                let default = if channel == Channel::Scale { 1.0 } else { 0.0 };
                let points = k.get("data_points").and_then(Value::as_array);
                let mut point = |i: usize| {
                    let p = points.and_then(|l| l.get(i));
                    let x = expr(p.and_then(|p| p.get("x")), default, notes, &name);
                    // One number for all three axes (uniform scale)
                    let or_x = |key: &str, notes: &mut Vec<String>| match p.and_then(|p| p.get(key)) {
                        Some(v) => expr(Some(v), default, notes, &name),
                        None => x.clone(),
                    };
                    let (y, z) = (or_x("y", notes), or_x("z", notes));
                    [x, y, z]
                };
                let pre = point(0);
                let post = if points.is_some_and(|l| l.len() > 1) { point(1) } else { pre.clone() };
                let interp = match k.get("interpolation").and_then(Value::as_str) {
                    Some("step") => Interp::Step,
                    Some("catmullrom") => Interp::Smooth,
                    Some("bezier") => Interp::Bezier,
                    _ => Interp::Linear,
                };
                let bezier = (interp == Interp::Bezier).then(|| Bezier {
                    left_time: triple(k.get("bezier_left_time"), -0.1),
                    left_value: triple(k.get("bezier_left_value"), 0.0),
                    right_time: triple(k.get("bezier_right_time"), 0.1),
                    right_value: triple(k.get("bezier_right_value"), 0.0),
                });
                keys.push(RawKey { channel, time: num(k.get("time")).unwrap_or(0.0).max(0.0), interp, pre, post, bezier });
            }
            if !keys.is_empty() {
                animators.push(RawAnimator { uuid: uuid.clone(), name: text(a.get("name")), keys });
            }
        }
    }
    // The order of a JSON object is not the order of the file everywhere: keep ours fixed
    animators.sort_by(|a, b| a.uuid.cmp(&b.uuid));
    let looping = match v.get("loop").and_then(Value::as_str) {
        Some("loop") => Loop::Loop,
        Some("hold") => Loop::Hold,
        _ => Loop::Once,
    };
    let last_key = animators.iter().flat_map(|a| a.keys.iter().map(|k| k.time)).fold(0.0f32, f32::max);
    RawAnimation { name, looping, length: num(v.get("length")).unwrap_or(0.0).max(last_key), animators }
}

/// Read a `.bbmodel`. The error says what is wrong with the file as a whole.
pub fn parse(json: &str) -> Result<Raw, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("not a Blockbench file: {e}"))?;
    if v.get("elements").is_none() && v.get("outliner").is_none() {
        return Err("not a Blockbench file: it has no elements".into());
    }
    let mut raw = Raw { resolution: (num(v.get("resolution").and_then(|r| r.get("width"))).unwrap_or(16.0).max(1.0), num(v.get("resolution").and_then(|r| r.get("height"))).unwrap_or(16.0).max(1.0)), ..Raw::default() };

    for t in v.get("textures").and_then(Value::as_array).into_iter().flatten() {
        let name = text(t.get("name"));
        let png = t.get("source").and_then(Value::as_str).and_then(|s| s.strip_prefix("data:image/png;base64,")).and_then(base64).filter(|b| mcsv_pack::is_png(b));
        if png.is_none() {
            raw.notes.push(format!("picture {name} is not saved inside the file: turn on \"embed textures\" when saving, or save as a .bbmodel"));
        }
        raw.textures.push(RawTexture {
            name,
            png: png.unwrap_or_default(),
            uv_width: num(t.get("uv_width")).unwrap_or(0.0),
            uv_height: num(t.get("uv_height")).unwrap_or(0.0),
            frame_time: num(t.get("frame_time")).map_or(1, |f| (f as u32).max(1)),
        });
    }

    let mut other = 0;
    for e in v.get("elements").and_then(Value::as_array).into_iter().flatten() {
        let kind = e.get("type").and_then(Value::as_str).unwrap_or("cube");
        if kind != "cube" {
            // Locators and the like are helpers; a mesh is something the game's models cannot hold
            other += usize::from(kind == "mesh");
            continue;
        }
        raw.cubes.insert(text(e.get("uuid")), cube(e, raw.textures.len()));
    }
    if other > 0 {
        raw.notes.push(format!("{other} mesh(es) left out: the game's models are made of cubes only"));
    }

    let groups: HashMap<String, &Value> = v.get("groups").and_then(Value::as_array).into_iter().flatten().map(|g| (text(g.get("uuid")), g)).collect();
    raw.roots = v.get("outliner").and_then(Value::as_array).map(|list| list.iter().filter_map(|n| node(n, &groups, 0)).collect()).unwrap_or_default();

    for a in v.get("animations").and_then(Value::as_array).into_iter().flatten() {
        let a = animation(a, &mut raw.notes);
        raw.animations.push(a);
    }
    Ok(raw)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A one pixel PNG is enough for everything but drawing it.
    pub const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

    /// A small creature: a body, a head on it, a leg turned in a way a model file cannot hold
    /// (so it becomes a part of its own), a hitbox, and two animations.
    pub fn sample() -> String {
        serde_json::json!({
            "meta": { "format_version": "4.10", "model_format": "free" },
            "resolution": { "width": 32, "height": 32 },
            "elements": [
                { "name": "body", "uuid": "c-body", "from": [-4, 8, -6], "to": [4, 16, 6], "origin": [0, 8, 0],
                  "faces": { "north": { "uv": [0, 0, 8, 8], "texture": 0 }, "up": { "uv": [8, 0, 16, 12], "texture": 0, "rotation": 90 }, "south": { "uv": [0, 0, 8, 8], "texture": null } } },
                { "name": "skull", "uuid": "c-head", "from": [-3, 16, -9], "to": [3, 22, -3], "origin": [0, 16, -6], "rotation": [0, 22.5, 0], "inflate": 0.5,
                  "faces": { "north": { "uv": [16, 0, 22, 6], "texture": 0 } } },
                { "name": "leg", "uuid": "c-leg", "from": [-4, 0, -1], "to": [-2, 8, 1], "origin": [-3, 8, 0], "rotation": [10, 0, 30],
                  "faces": { "west": { "uv": [0, 16, 2, 24], "texture": 0 } } },
                { "name": "hitbox", "uuid": "c-hit", "from": [-5, 0, -5], "to": [5, 24, 5], "origin": [0, 0, 0], "faces": {} },
                { "name": "hidden", "uuid": "c-off", "from": [0, 0, 0], "to": [1, 1, 1], "visibility": false, "faces": { "north": { "uv": [0, 0, 1, 1], "texture": 0 } } },
                { "name": "blob", "uuid": "m-1", "type": "mesh" },
                { "name": "spot", "uuid": "l-1", "type": "locator" }
            ],
            "outliner": [
                { "name": "root", "uuid": "g-root", "origin": [0, 0, 0], "children": [
                    { "name": "body", "uuid": "g-body", "origin": [0, 8, 0], "rotation": [0, 0, 0], "children": [
                        "c-body",
                        { "name": "h_head", "uuid": "g-head", "origin": [0, 16, -3], "children": ["c-head"] },
                        { "name": "leg", "uuid": "g-leg", "origin": [-3, 8, 0], "children": ["c-leg"] },
                        { "name": "tag_name", "uuid": "g-tag", "origin": [0, 26, 0], "children": [] }
                    ] },
                    { "name": "hitbox", "uuid": "g-hit", "origin": [0, 0, 0], "children": ["c-hit"] },
                    "c-off"
                ] }
            ],
            "textures": [ { "name": "skin.png", "source": format!("data:image/png;base64,{PNG}"), "uv_width": 32, "uv_height": 32 } ],
            "animations": [
                { "name": "idle", "loop": "loop", "length": 2.0, "animators": {
                    "g-head": { "name": "h_head", "type": "bone", "keyframes": [
                        { "channel": "rotation", "time": 0, "interpolation": "linear", "data_points": [{ "x": "0", "y": 0, "z": 0 }] },
                        { "channel": "rotation", "time": 1, "interpolation": "linear", "data_points": [{ "x": 20, "y": "-10", "z": 0 }] },
                        { "channel": "rotation", "time": 2, "interpolation": "linear", "data_points": [{ "x": 0, "y": 0, "z": 0 }] }
                    ] },
                    "g-body": { "name": "body", "type": "bone", "keyframes": [
                        { "channel": "position", "time": 0, "data_points": [{ "x": 0, "y": "math.sin(query.anim_time * 180) * 2", "z": 0 }] },
                        { "channel": "scale", "time": 0, "data_points": [{ "x": 1 }] },
                        { "channel": "scale", "time": 2, "data_points": [{ "x": 2 }] }
                    ] },
                    "effects": { "name": "Effects", "type": "effect", "keyframes": [ { "channel": "sound", "time": 0.5, "data_points": [{ "effect": "x" }] } ] }
                } },
                { "name": "attack", "loop": "once", "length": 0.5, "animators": {
                    "g-leg": { "name": "leg", "keyframes": [
                        { "channel": "rotation", "time": 0, "interpolation": "step", "data_points": [{ "x": 0, "y": 0, "z": 0 }] },
                        { "channel": "rotation", "time": 0.25, "interpolation": "linear", "data_points": [{ "x": 45, "y": 0, "z": 0 }, { "x": 90, "y": 0, "z": 0 }] },
                        { "channel": "rotation", "time": 0.5, "interpolation": "linear", "data_points": [{ "x": "variable.oops", "y": 0, "z": 0 }] }
                    ] }
                } }
            ]
        })
        .to_string()
    }

    #[test]
    fn base64_reads_what_blockbench_writes() {
        assert_eq!(base64("aGVsbG8=").as_deref(), Some(&b"hello"[..]));
        assert_eq!(base64("aGVs\nbG8h").as_deref(), Some(&b"hello!"[..]));
        assert_eq!(base64("a*b"), None);
        assert!(mcsv_pack::is_png(&base64(PNG).unwrap()));
    }

    #[test]
    fn the_sample_is_read_whole() {
        let raw = parse(&sample()).unwrap();
        assert_eq!(raw.resolution, (32.0, 32.0));
        assert_eq!(raw.cubes.len(), 5);
        let body = &raw.cubes["c-body"];
        assert_eq!((body.from, body.to), ([-4.0, 8.0, -6.0], [4.0, 16.0, 6.0]));
        // north and up have a picture; south says null and is not drawn
        assert_eq!(body.faces.iter().filter(|f| f.is_some()).count(), 2);
        assert_eq!(body.faces[4].as_ref().unwrap().rotation, 90);
        assert_eq!(raw.cubes["c-head"].inflate, 0.5);
        assert!(!raw.cubes["c-off"].visible);
        assert_eq!(raw.textures.len(), 1);
        assert!(mcsv_pack::is_png(&raw.textures[0].png));
        assert!(raw.notes.iter().any(|n| n.contains("mesh")));
        assert!(raw.notes.iter().any(|n| n.contains("variable.oops")), "{:?}", raw.notes);

        let Node::Group(root) = &raw.roots[0] else { panic!() };
        assert_eq!((root.name.as_str(), root.children.len()), ("root", 3));
        let Node::Group(body) = &root.children[0] else { panic!() };
        assert_eq!(body.children.len(), 4);
        assert_eq!(body.children[0], Node::Cube("c-body".into()));

        assert_eq!(raw.animations.len(), 2);
        let idle = &raw.animations[0];
        assert_eq!((idle.name.as_str(), idle.looping, idle.length), ("idle", Loop::Loop, 2.0));
        // The effects animator has nothing for a model
        assert_eq!(idle.animators.len(), 2);
        let scale = idle.animators.iter().find(|a| a.name == "body").unwrap().keys.iter().find(|k| k.channel == Channel::Scale && k.time == 2.0).unwrap();
        assert_eq!(scale.pre.clone().map(|e| e.eval(0.0)), [2.0, 2.0, 2.0]);
        let attack = &raw.animations[1];
        assert_eq!(attack.looping, Loop::Once);
        let mid = &attack.animators[0].keys[1];
        assert_eq!((mid.pre[0].eval(0.0), mid.post[0].eval(0.0)), (45.0, 90.0));
        assert_eq!(attack.animators[0].keys[0].interp, Interp::Step);
    }

    #[test]
    fn newer_files_keep_groups_apart_from_the_outliner() {
        let json = serde_json::json!({
            "elements": [ { "uuid": "c1", "from": [0, 0, 0], "to": [1, 1, 1], "faces": {} } ],
            "groups": [ { "name": "arm", "uuid": "g1", "origin": [1, 2, 3], "rotation": [0, 0, 45] } ],
            "outliner": [ { "uuid": "g1", "isOpen": true, "children": ["c1"] } ]
        })
        .to_string();
        let raw = parse(&json).unwrap();
        let Node::Group(g) = &raw.roots[0] else { panic!() };
        assert_eq!((g.name.as_str(), g.origin, g.rotation, g.children.len()), ("arm", [1.0, 2.0, 3.0], [0.0, 0.0, 45.0], 1));
    }

    #[test]
    fn other_files_are_refused() {
        assert!(parse("{}").is_err());
        assert!(parse("nope").is_err());
        assert!(parse(r#"{"elements": []}"#).is_ok());
    }
}

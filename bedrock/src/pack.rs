//! The models of the Models plugin as a Bedrock resource pack: every model is an entity of
//! its own (`models:<name>`) with its geometry, its picture and its animations.
//!
//! The model comes from `models::rig`, the same reading of the Blockbench file the Java side
//! uses, so the bones and the animation names are the same on both. The animations are the
//! tables Java plays too (one value a tick), thinned to the keyframes a straight line between
//! them cannot replace.
//!
//! A Bedrock model is the mirror image of Blockbench's own space along X: places flip X, turns
//! flip X and Y (what Blockbench's own Bedrock export does).
use mcsv_pack::Entry;
use models::bbmodel::{Loop, SIDES};
use models::math::Vec3;
use models::rig::{Kind, Model, UNITS};
use serde_json::{Map, Value, json};

/// The namespace of the entities, as in `models:dragon`.
pub const NS: &str = "models";
/// The file the server sends, in the plugin's data folder.
pub const FILE: &str = "models.mcpack";
/// The namespace of the items of the CustomItems plugin, as in `customitems:ruby`.
pub const ITEMS: &str = "customitems";

/// The item a Bedrock client is told about for an item of CustomItems.
pub fn item_identifier(id: &str) -> String {
    format!("{ITEMS}:{id}")
}

/// The name the item's picture has in the pack's list of item pictures.
pub fn item_icon(id: &str) -> String {
    format!("{ITEMS}.{id}")
}

/// The picture of one item of CustomItems.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemPicture {
    pub id: String,
    pub png: Vec<u8>,
}

/// The entity a Bedrock client is told to draw for a model.
pub fn identifier(model: &str) -> String {
    format!("{NS}:{model}")
}

pub fn animation_id(model: &str, animation: &str) -> String {
    format!("animation.{NS}.{model}.{animation}")
}

/// What the plugin has to know about a model while the server runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub name: String,
    /// It has bones that follow where the creature looks.
    pub head: bool,
    pub animations: Vec<(String, Loop)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pack {
    pub bytes: Vec<u8>,
    pub uuid: String,
    pub version: String,
    pub models: Vec<Info>,
    /// What a model has that Bedrock is not shown, in plain words (model, what).
    pub notes: Vec<(String, String)>,
}

/// Four decimals: enough for a model, and the files stay small.
fn n(v: f32) -> Value {
    let r = (f64::from(v) * 10_000.0).round() / 10_000.0;
    json!(if r == 0.0 { 0.0 } else { r })
}

fn place(v: Vec3) -> Value {
    json!([n(-v.x), n(v.y), n(v.z)])
}

fn turn(v: Vec3) -> Value {
    json!([n(-v.x), n(-v.y), n(v.z)])
}

/// The geometry file of one model. Every picture of the model is one cell of 16 by 16 in the
/// file's own measure, the cells below each other.
fn geometry(m: &Model, cells: usize) -> Value {
    let mut rotated = false;
    let (mut wide, mut high) = (0.0f32, 0.0f32);
    let bones: Vec<Value> = m
        .bones
        .iter()
        .map(|b| {
            let mut bone = json!({ "name": b.name, "pivot": place(b.pivot) });
            if let Some(parent) = b.parent {
                bone["parent"] = json!(m.bones[parent].name);
            }
            if b.rotation != Vec3::ZERO {
                bone["rotation"] = turn(b.rotation);
            }
            let cubes: Vec<Value> = b
                .cubes
                .iter()
                .map(|c| {
                    for corner in [c.from, c.to] {
                        wide = wide.max(corner.x.abs()).max(corner.z.abs());
                        high = high.max(corner.y);
                    }
                    let size = c.to.sub(c.from);
                    let mut cube = json!({ "origin": [n(-c.to.x), n(c.from.y), n(c.from.z)], "size": [n(size.x), n(size.y), n(size.z)] });
                    if let Some((axis, degrees, around)) = c.turn {
                        let mut r = [0.0f32; 3];
                        r[axis.min(2)] = degrees;
                        cube["pivot"] = place(around);
                        cube["rotation"] = turn(Vec3::from_array(r));
                    }
                    let mut uv = Map::new();
                    for (side, face) in SIDES.iter().zip(&c.faces) {
                        let Some(f) = face else { continue };
                        let down = (f.texture.min(cells.saturating_sub(1)) * 16) as f32;
                        let (u1, v1, u2, v2) = (f.uv[0], f.uv[1] + down, f.uv[2], f.uv[3] + down);
                        // The top and the bottom are stored the other way round on Bedrock
                        let mut entry = if matches!(*side, "up" | "down") { json!({ "uv": [n(u2), n(v2)], "uv_size": [n(u1 - u2), n(v1 - v2)] }) } else { json!({ "uv": [n(u1), n(v1)], "uv_size": [n(u2 - u1), n(v2 - v1)] }) };
                        if f.rotation != 0 {
                            entry["uv_rotation"] = json!(f.rotation);
                            rotated = true;
                        }
                        uv.insert((*side).to_owned(), entry);
                    }
                    cube["uv"] = Value::Object(uv);
                    cube
                })
                .collect();
            if !cubes.is_empty() {
                bone["cubes"] = Value::Array(cubes);
            }
            bone
        })
        .collect();
    // What the client may leave out when it is off screen: generous, the bones move
    let (width, height) = ((wide / UNITS * 2.0 + 2.0).ceil(), (high / UNITS + 2.0).ceil());
    json!({
        // A face turned on its picture needs the newer file
        "format_version": if rotated { "1.21.0" } else { "1.12.0" },
        "minecraft:geometry": [{
            "description": {
                "identifier": format!("geometry.{NS}.{}", m.name),
                "texture_width": 16,
                "texture_height": 16 * cells.max(1),
                "visible_bounds_width": width,
                "visible_bounds_height": height,
                "visible_bounds_offset": [0, n(height / 2.0 - 0.5), 0],
            },
            "bones": bones,
        }],
    })
}

/// Which entries of a table of one value a tick have to stay so that straight lines between
/// them are the table again (within `slack`).
fn thin(list: &[Vec3], slack: f32) -> Vec<usize> {
    let close = |a: Vec3, b: Vec3| (a.x - b.x).abs() <= slack && (a.y - b.y).abs() <= slack && (a.z - b.z).abs() <= slack;
    let fits = |from: usize, to: usize| (from + 1..to).all(|i| close(list[from].lerp(list[to], (i - from) as f32 / (to - from) as f32), list[i]));
    let mut keep = vec![0];
    let mut at = 0;
    while at + 1 < list.len() {
        let mut to = at + 1;
        while to + 1 < list.len() && to + 1 - at <= 80 && fits(at, to + 1) {
            to += 1;
        }
        keep.push(to);
        at = to;
    }
    keep
}

fn seconds(tick: usize) -> String {
    format!("{}", tick as f64 / 20.0)
}

/// One channel of one bone: a value that never changes, or keyframes by time.
fn channel(list: &[Vec3], slack: f32, write: &dyn Fn(Vec3) -> Value) -> Value {
    if list.len() == 1 {
        return write(list[0]);
    }
    Value::Object(thin(list, slack).into_iter().map(|i| (seconds(i), write(list[i]))).collect())
}

/// The animation file of one model, and whether it has bones that follow the creature's look.
fn animations(m: &Model) -> (Value, bool) {
    let mut all = Map::new();
    for a in &m.animations {
        let mut bones = Map::new();
        for (bone, track) in m.bones.iter().zip(&a.tracks) {
            let Some(track) = track else { continue };
            let mut channels = Map::new();
            // The tables are in Blockbench's own space: back to Bedrock's
            if let Some(list) = &track.rot {
                channels.insert("rotation".into(), channel(list, 0.02, &turn));
            }
            if let Some(list) = &track.pos {
                channels.insert("position".into(), channel(list, 0.004, &place));
            }
            if let Some(list) = &track.scale {
                channels.insert("scale".into(), channel(list, 0.002, &|v| json!([n(v.x), n(v.y), n(v.z)])));
            }
            bones.insert(bone.name.clone(), Value::Object(channels));
        }
        let mut body = json!({ "animation_length": a.ticks as f64 / 20.0, "bones": bones });
        match a.looping {
            Loop::Loop => body["loop"] = json!(true),
            Loop::Hold => body["loop"] = json!("hold_on_last_frame"),
            Loop::Once => {}
        }
        all.insert(animation_id(&m.name, &a.name), body);
    }
    // Only the first bone of a chain turns: what hangs below it turns with it
    let heads: Vec<&str> = m.bones.iter().filter(|b| b.kind == Kind::Head && b.parent.is_none_or(|p| m.bones[p].kind != Kind::Head)).map(|b| b.name.as_str()).collect();
    if !heads.is_empty() {
        let bones: Map<String, Value> = heads.iter().map(|name| ((*name).to_owned(), json!({ "rotation": ["query.target_x_rotation", "query.target_y_rotation", 0] }))).collect();
        all.insert(animation_id(&m.name, "look"), json!({ "loop": true, "bones": bones }));
    }
    (json!({ "format_version": "1.8.0", "animations": all }), !heads.is_empty())
}

/// The name an animation has inside the entity file: never one of the names used here.
fn short(animation: &str) -> String {
    format!("a_{animation}")
}

/// The client picks the animations by two numbers the server sets on the entity (see
/// `plugin`): `variant` is the animation that runs all the time (idle, walk), `mark_variant`
/// the one that was asked for. Both count the model's animations from 1; 0 is none.
fn entity(m: &Model, head: bool) -> Value {
    let mut names: Map<String, Value> = m.animations.iter().map(|a| (short(&a.name), json!(animation_id(&m.name, &a.name)))).collect();
    names.insert("asked".into(), json!(format!("controller.animation.{NS}.{}.asked", m.name)));
    let mut animate: Vec<Value> = m.animations.iter().enumerate().map(|(i, a)| json!({ short(&a.name): format!("query.variant == {}", i + 1) })).collect();
    animate.push(json!("asked"));
    if head {
        names.insert("look".into(), json!(animation_id(&m.name, "look")));
        animate.push(json!("look"));
    }
    json!({
        "format_version": "1.10.0",
        "minecraft:client_entity": {
            "description": {
                "identifier": identifier(&m.name),
                "materials": { "default": "entity_alphatest" },
                "textures": { "default": format!("textures/{NS}/{}", m.name) },
                "geometry": { "default": format!("geometry.{NS}.{}", m.name) },
                "render_controllers": [format!("controller.render.{NS}")],
                "animations": names,
                "scripts": { "animate": animate },
            }
        }
    })
}

/// An animation that was asked for starts from its beginning when its number comes, and goes
/// when the number does.
fn controllers(m: &Model) -> Value {
    let mut states = Map::new();
    states.insert("none".into(), json!({ "transitions": m.animations.iter().enumerate().map(|(i, _)| json!({ format!("s{}", i + 1): format!("query.mark_variant == {}", i + 1) })).collect::<Vec<_>>(), "blend_transition": 0.15 }));
    for (i, a) in m.animations.iter().enumerate() {
        states.insert(format!("s{}", i + 1), json!({ "animations": [short(&a.name)], "transitions": [{ "none": format!("query.mark_variant != {}", i + 1) }], "blend_transition": 0.15 }));
    }
    json!({ "format_version": "1.10.0", "animation_controllers": { format!("controller.animation.{NS}.{}.asked", m.name): { "initial_state": "none", "states": states } } })
}

/// A picture as red, green, blue, alpha.
fn rgba(data: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let mut decoder = png::Decoder::new(data);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).ok()?;
    let raw = &buffer[..info.buffer_size()];
    let pixels: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => raw.to_vec(),
        png::ColorType::Rgb => raw.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::Grayscale => raw.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => raw.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Indexed => return None,
    };
    Some((info.width as usize, info.height as usize, pixels))
}

fn png_of(width: usize, height: usize, pixels: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    if let Ok(mut writer) = encoder.write_header() {
        let _ = writer.write_image_data(pixels);
    }
    out
}

/// The one picture a Bedrock entity has: the model's pictures below each other, each made as
/// large as the largest. `None` when the model's only picture can go in as it is.
fn picture(m: &Model, notes: &mut Vec<String>) -> Vec<u8> {
    if let [only] = m.textures.as_slice()
        && only.frame_time == 0
        && mcsv_pack::is_png(&only.png)
    {
        return only.png.clone();
    }
    let cells: Vec<Option<(usize, usize, Vec<u8>)>> = m
        .textures
        .iter()
        .map(|t| {
            let (w, h, pixels) = rgba(&t.png)?;
            // A moving picture is its frames below each other: Bedrock shows the first
            let frame = if t.frame_time > 0 && h > w { w } else { h };
            if frame != h {
                notes.push(format!("picture {} moves: Bedrock players see its first frame", t.name));
            }
            Some((w, frame, pixels[..w * frame * 4].to_vec()))
        })
        .collect();
    let (width, height) = cells.iter().flatten().fold((1, 1), |(w, h), c| (w.max(c.0), h.max(c.1)));
    let mut sheet = vec![0u8; width * height * cells.len().max(1) * 4];
    for (cell, picture) in cells.iter().enumerate() {
        let Some((w, h, pixels)) = picture else { continue };
        for y in 0..height {
            for x in 0..width {
                let from = ((y * h / height) * w + x * w / width) * 4;
                let to = ((cell * height + y) * width + x) * 4;
                sheet[to..to + 4].copy_from_slice(&pixels[from..from + 4]);
            }
        }
    }
    png_of(width, height * cells.len().max(1), &sheet)
}

/// A pack id the server can use: Pumpkin 0.2.2 writes the id into the pack list with each
/// half of its bytes in the opposite order to what the client reads, then looks the pack up by
/// what the client asks for, so only an id whose halves read the same both ways works.
pub fn pack_id(seed: u64) -> String {
    let b = seed.to_be_bytes();
    let hex = |part: &[u8]| part.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let (a, c) = (&b[0..4], &b[4..8]);
    let (a_back, c_back): (Vec<u8>, Vec<u8>) = (a.iter().rev().copied().collect(), c.iter().rev().copied().collect());
    format!("{}-{}-{}-{}-{}{}", hex(a), hex(&a_back[0..2]), hex(&a_back[2..4]), hex(&c[0..2]), hex(&c[2..4]), hex(&c_back))
}

/// Whether an id is one `pack_id` could have made.
pub fn usable_id(id: &str) -> bool {
    let hex: Vec<u8> = id.bytes().filter(|b| *b != b'-').collect();
    let dashes: Vec<usize> = id.bytes().enumerate().filter(|(_, b)| *b == b'-').map(|(i, _)| i).collect();
    hex.len() == 32 && dashes == [8, 13, 18, 23] && hex.iter().all(u8::is_ascii_hexdigit) && [0, 16].iter().all(|start| (0..8).all(|i| hex[start + 2 * i..start + 2 * i + 2] == hex[start + 14 - 2 * i..start + 16 - 2 * i]))
}

/// The pack for these models. Its version follows what is in it, so a client that has an
/// older one downloads the new one.
pub fn build(models: &[Model], items: &[ItemPicture], id: &str) -> Pack {
    let mut entries = Vec::new();
    let mut infos = Vec::new();
    let mut notes = Vec::new();
    if !items.is_empty() {
        let list: Map<String, Value> = items.iter().map(|i| (item_icon(&i.id), json!({ "textures": format!("textures/items/{ITEMS}/{}", i.id) }))).collect();
        entries.push(Entry::json("textures/item_texture.json".into(), &json!({ "resource_pack_name": NS, "texture_name": "atlas.items", "texture_data": list })));
        entries.extend(items.iter().map(|i| Entry { path: format!("textures/items/{ITEMS}/{}.png", i.id), data: i.png.clone() }));
    }
    for m in models {
        let mut said = Vec::new();
        let (file, head) = animations(m);
        entries.push(Entry::json(format!("animations/{}.animation.json", m.name), &file));
        entries.push(Entry::json(format!("animation_controllers/{}.animation_controllers.json", m.name), &controllers(m)));
        entries.push(Entry::json(format!("entity/{}.entity.json", m.name), &entity(m, head)));
        entries.push(Entry::json(format!("models/entity/{}.geo.json", m.name), &geometry(m, m.textures.len())));
        entries.push(Entry { path: format!("textures/{NS}/{}.png", m.name), data: picture(m, &mut said) });
        notes.extend(said.into_iter().map(|note| (m.name.clone(), note)));
        infos.push(Info { name: m.name.clone(), head, animations: m.animations.iter().map(|a| (a.name.clone(), a.looping)).collect() });
    }
    entries.push(Entry::json(
        format!("render_controllers/{NS}.render_controllers.json"),
        &json!({ "format_version": "1.8.0", "render_controllers": { format!("controller.render.{NS}"): { "geometry": "Geometry.default", "materials": [{ "*": "Material.default" }], "textures": ["Texture.default"] } } }),
    ));
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let sum = entries.iter().fold(0u32, |sum, e| mcsv_pack::crc32(&[&sum.to_le_bytes()[..], e.path.as_bytes(), &e.data].concat()));
    let version = [1, sum >> 16, sum & 0xffff];
    let module = mcsv_pack::id_text(mcsv_pack::pack_id(&format!("{id}-resources")));
    entries.push(Entry::json(
        "manifest.json".into(),
        &json!({
            "format_version": 2,
            "header": { "name": "Models", "description": "The models of this server", "uuid": id, "version": version, "min_engine_version": [1, 21, 0] },
            "modules": [{ "type": "resources", "uuid": module, "version": version }],
        }),
    ));
    Pack { bytes: mcsv_pack::zip(&entries), uuid: id.to_owned(), version: format!("{}.{}.{}", version[0], version[1], version[2]), models: infos, notes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::{bbmodel, defs, rig};

    fn example() -> Model {
        rig::build("example", &bbmodel::parse(defs::EXAMPLE_MODEL).unwrap()).unwrap()
    }

    #[test]
    fn pack_ids_read_the_same_both_ways() {
        let id = pack_id(0x0123_4567_89ab_cdef);
        assert_eq!(id, "01234567-6745-2301-89ab-cdefefcdab89");
        assert!(usable_id(&id));
        assert!(!usable_id("11112222-3333-4444-5555-666677778888"));
        assert!(!usable_id("01234567"));
        // What Pumpkin sends and what the client reads back are the same id
        let bytes: Vec<u8> = id.bytes().filter(|b| *b != b'-').collect();
        let halves: Vec<String> = bytes.chunks(16).map(|h| h.chunks(2).rev().flatten().map(|b| *b as char).collect()).collect();
        assert_eq!(halves.concat().as_bytes(), bytes.as_slice());
    }

    #[test]
    fn thinning_keeps_what_a_line_cannot_replace() {
        let line: Vec<Vec3> = (0..=10).map(|i| Vec3::new(i as f32, 0.0, 0.0)).collect();
        assert_eq!(thin(&line, 0.01), [0, 10]);
        let mut bent = line.clone();
        bent[5].y = 3.0;
        assert_eq!(thin(&bent, 0.01), [0, 4, 5, 6, 10]);
        // Every tick of the table is within the slack of the thinned one
        let wave: Vec<Vec3> = (0..=200).map(|i| Vec3::new((i as f32 * 0.02).sin() * 30.0, 0.0, 0.0)).collect();
        let kept = thin(&wave, 0.02);
        assert!(kept.len() < wave.len());
        for pair in kept.windows(2) {
            for i in pair[0]..=pair[1] {
                let on_line = wave[pair[0]].lerp(wave[pair[1]], (i - pair[0]) as f32 / (pair[1] - pair[0]).max(1) as f32);
                assert!((on_line.x - wave[i].x).abs() <= 0.021, "tick {i}");
            }
        }
    }

    #[test]
    fn the_example_becomes_a_pack() {
        let model = example();
        let pack = build(std::slice::from_ref(&model), &[], &pack_id(7));
        assert_eq!(pack.models.len(), 1);
        assert_eq!(pack.models[0].name, "example");
        assert!(pack.models[0].animations.iter().any(|(name, looping)| name == "idle" && *looping == Loop::Loop));
        assert!(pack.version.starts_with("1."));
        assert_eq!(&pack.bytes[..2], b"PK");
        // The same models give the same pack; other models give another version
        assert_eq!(build(std::slice::from_ref(&model), &[], &pack_id(7)).version, pack.version);
        let mut other = model.clone();
        other.bones[0].pivot.y += 1.0;
        assert_ne!(build(&[other], &[], &pack_id(7)).version, pack.version);
        // An item's picture is in the pack and in its list of item pictures
        let with = build(std::slice::from_ref(&model), &[ItemPicture { id: "ruby".into(), png: png_of(1, 1, &[255, 0, 0, 255]) }], &pack_id(7));
        assert_ne!(with.version, pack.version);
        let find = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
        assert!(find(&with.bytes, b"textures/items/customitems/ruby.png"));
        assert!(find(&with.bytes, b"textures/item_texture.json"));
    }

    #[test]
    fn geometry_is_mirrored_along_x() {
        let model = example();
        let geo = geometry(&model, 1);
        let bones = geo["minecraft:geometry"][0]["bones"].as_array().unwrap();
        assert_eq!(bones.len(), model.bones.len());
        for (bone, json) in model.bones.iter().zip(bones) {
            assert_eq!(json["name"], bone.name.as_str());
            assert_eq!(json["pivot"][0].as_f64().unwrap() as f32, -bone.pivot.x);
            for (cube, json) in bone.cubes.iter().zip(json["cubes"].as_array().into_iter().flatten()) {
                assert_eq!(json["origin"][0].as_f64().unwrap() as f32, -cube.to.x);
                assert_eq!(json["size"][0].as_f64().unwrap() as f32, cube.to.x - cube.from.x);
            }
        }
    }

    #[test]
    fn animations_go_back_to_bedrock_turns() {
        let model = example();
        let (file, _) = animations(&model);
        let idle = &file["animations"][animation_id("example", "idle")];
        assert_eq!(idle["loop"], true);
        let animation = model.animations.iter().find(|a| a.name == "idle").unwrap();
        let (at, track) = animation.tracks.iter().enumerate().find_map(|(i, t)| t.as_ref().filter(|t| t.rot.is_some()).map(|t| (i, t))).unwrap();
        let first = track.rot.as_ref().unwrap()[0];
        let written = &idle["bones"][model.bones[at].name.as_str()]["rotation"];
        let start = if written.is_array() { written } else { &written["0"] };
        assert_eq!(start[0].as_f64().unwrap() as f32, n(-first.x).as_f64().unwrap() as f32);
        assert_eq!(start[2].as_f64().unwrap() as f32, n(first.z).as_f64().unwrap() as f32);
    }

    #[test]
    fn several_pictures_become_one() {
        let mut model = example();
        let red = png_of(2, 2, &[255, 0, 0, 255].repeat(4));
        let blue = png_of(4, 4, &[0, 0, 255, 255].repeat(16));
        model.textures = vec![rig::Texture { name: "red".into(), png: red, frame_time: 0 }, rig::Texture { name: "blue".into(), png: blue, frame_time: 0 }];
        let mut notes = Vec::new();
        let (w, h, pixels) = rgba(&picture(&model, &mut notes)).unwrap();
        assert_eq!((w, h), (4, 8));
        assert_eq!(&pixels[0..4], [255, 0, 0, 255]);
        assert_eq!(&pixels[(4 * 4) * 4..(4 * 4) * 4 + 4], [0, 0, 255, 255]);
        assert_eq!(geometry(&model, 2)["minecraft:geometry"][0]["description"]["texture_height"], 32);
    }
}

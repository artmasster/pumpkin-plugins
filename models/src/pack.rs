//! The resource pack: one item model for every bone that draws something, the pictures, and
//! the zip that holds them. No server needed here.
//!
//! A bone's item model has the bone's pivot in its middle, because that is the point an item
//! display turns its item around.
use std::collections::BTreeMap;

use mcsv_pack::Entry;
use serde_json::{Value, json};

use crate::bbmodel::SIDES;
use crate::defs::{NS, Note, PackConfig};
use crate::math::Vec3;
use crate::rig::{Bone, Model};

/// The middle of a model file's space.
const MIDDLE: f32 = 8.0;

/// The item model an item display shows for this bone (`item_model` component).
pub fn item_id(model: &str, bone: &str) -> String {
    format!("{NS}:{model}/{bone}")
}

/// Four decimals: more than a model needs, and the same text every time.
fn tidy(v: f32) -> Value {
    let r = (f64::from(v) * 10_000.0).round() / 10_000.0;
    json!(if r == 0.0 { 0.0 } else { r })
}

fn point(p: Vec3, bone: &Bone) -> Value {
    let q = p.sub(bone.pivot).scale(bone.fit);
    json!([tidy(q.x + MIDDLE), tidy(q.y + MIDDLE), tidy(q.z + MIDDLE)])
}

fn bone_model(model: &Model, bone: &Bone) -> Value {
    let mut textures = BTreeMap::new();
    let mut elements = Vec::new();
    for cube in &bone.cubes {
        let mut faces = serde_json::Map::new();
        for (side, face) in SIDES.iter().zip(&cube.faces) {
            let Some(face) = face else { continue };
            textures.entry(face.texture.to_string()).or_insert_with(|| format!("{NS}:item/{}/{}", model.name, model.textures[face.texture].name));
            let mut f = json!({ "uv": face.uv.map(tidy), "texture": format!("#{}", face.texture), "tintindex": 0 });
            if face.rotation != 0 {
                f["rotation"] = face.rotation.into();
            }
            faces.insert((*side).to_owned(), f);
        }
        let mut element = json!({ "from": point(cube.from, bone), "to": point(cube.to, bone), "faces": faces });
        if let Some((axis, angle, origin)) = cube.turn {
            let name = ["x", "y", "z"][axis];
            element["rotation"] = json!({ "angle": angle, "axis": name, "origin": point(origin, bone) });
        }
        elements.push(element);
    }
    // What breaking particles and the like would use
    if let Some(first) = textures.values().next().cloned() {
        textures.insert("particle".to_owned(), first);
    }
    json!({ "textures": textures, "elements": elements })
}

/// The files made from the models.
pub fn generate(models: &[&Model], out: &mut Vec<Note>) -> Vec<Entry> {
    let mut entries = Vec::new();
    for model in models {
        for texture in &model.textures {
            if texture.png.is_empty() {
                continue;
            }
            let path = format!("assets/{NS}/textures/item/{}/{}.png", model.name, texture.name);
            // A strip of square frames is an animated picture
            if let Some((w, h)) = mcsv_pack::png_size(&texture.png)
                && w > 0
                && h > w
                && h % w == 0
            {
                entries.push(Entry::json(format!("{path}.mcmeta"), &json!({ "animation": { "frametime": texture.frame_time } })));
            }
            entries.push(Entry { path, data: texture.png.clone() });
        }
        for bone in model.bones.iter().filter(|b| !b.cubes.is_empty()) {
            entries.push(Entry::json(format!("assets/{NS}/models/item/{}/{}.json", model.name, bone.name), &bone_model(model, bone)));
            entries.push(Entry::json(
                format!("assets/{NS}/items/{}/{}.json", model.name, bone.name),
                &json!({ "model": { "type": "minecraft:model", "model": format!("{NS}:item/{}/{}", model.name, bone.name), "tints": [ { "type": "minecraft:dye", "default": 0xFF_FFFF } ] } }),
            ));
        }
    }
    for e in &entries {
        if !mcsv_pack::valid_pack_path(&e.path) {
            out.push(Note::plain(&e.path, "is not a path a resource pack can hold"));
        }
    }
    entries.retain(|e| mcsv_pack::valid_pack_path(&e.path));
    entries
}

/// The whole pack, in a fixed order. `Err` when it is more than a pack may be.
pub fn assemble(cfg: &PackConfig, generated: Vec<Entry>) -> Result<Vec<Entry>, Note> {
    let mut files: BTreeMap<String, Vec<u8>> = generated.into_iter().map(|e| (e.path, e.data)).collect();
    let meta = Entry::json("pack.mcmeta".into(), &mcsv_pack::mcmeta(&cfg.description, cfg.format_min, cfg.format_max));
    files.insert(meta.path, meta.data);
    let bytes: usize = files.values().map(Vec::len).sum();
    if files.len() > mcsv_pack::MAX_FILES || bytes > mcsv_pack::MAX_BYTES {
        return Err(Note::new("pack.zip", format!("pack ใหญ่เกินไป ({} ไฟล์, {} MB)", files.len(), bytes / 1_048_576), format!("the pack is too large ({} files, {} MB)", files.len(), bytes / 1_048_576)));
    }
    Ok(files.into_iter().map(|(path, data)| Entry { path, data }).collect())
}

pub fn pack_id() -> (u64, u64) {
    mcsv_pack::pack_id(NS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rig::tests::model;

    fn file<'a>(entries: &'a [Entry], path: &str) -> &'a Entry {
        entries.iter().find(|e| e.path == path).unwrap_or_else(|| panic!("{path} not in {:?}", entries.iter().map(|e| &e.path).collect::<Vec<_>>()))
    }

    #[test]
    fn every_drawn_bone_gets_a_model_and_an_item() {
        let m = model();
        let mut notes = Vec::new();
        let entries = generate(&[&m], &mut notes);
        assert!(notes.is_empty(), "{notes:?}");
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "assets/models/textures/item/test_creature/skin.png",
                "assets/models/models/item/test_creature/body.json",
                "assets/models/items/test_creature/body.json",
                "assets/models/models/item/test_creature/h_head.json",
                "assets/models/items/test_creature/h_head.json",
                "assets/models/models/item/test_creature/leg_leg.json",
                "assets/models/items/test_creature/leg_leg.json",
            ]
        );
        assert_eq!(item_id(&m.name, "body"), "models:test_creature/body");
        let item: Value = serde_json::from_slice(&file(&entries, "assets/models/items/test_creature/body.json").data).unwrap();
        assert_eq!(item["model"]["model"], "models:item/test_creature/body");
        assert_eq!(item["model"]["tints"][0]["type"], "minecraft:dye");
    }

    #[test]
    fn cubes_sit_around_the_pivot_in_the_middle() {
        let m = model();
        let entries = generate(&[&m], &mut Vec::new());
        let body: Value = serde_json::from_slice(&file(&entries, "assets/models/models/item/test_creature/body.json").data).unwrap();
        // Pivot (0, 8, 0); the cube runs from (-4, 8, -6) to (4, 16, 6)
        assert_eq!(body["elements"][0]["from"], json!([4.0, 8.0, 2.0]));
        assert_eq!(body["elements"][0]["to"], json!([12.0, 16.0, 14.0]));
        assert_eq!(body["textures"]["0"], "models:item/test_creature/skin");
        assert_eq!(body["textures"]["particle"], "models:item/test_creature/skin");
        let faces = body["elements"][0]["faces"].as_object().unwrap();
        assert_eq!(faces.keys().collect::<Vec<_>>(), ["north", "up"]);
        assert_eq!(faces["up"]["rotation"], 90);
        assert_eq!(faces["north"]["uv"], json!([0.0, 0.0, 4.0, 4.0]));
        assert_eq!(faces["north"]["tintindex"], 0);
        assert!(body["elements"][0].get("rotation").is_none());

        let head: Value = serde_json::from_slice(&file(&entries, "assets/models/models/item/test_creature/h_head.json").data).unwrap();
        // The head's pivot is (0, 16, -3); its cube turns 22.5 around Y at (0, 16, -6)
        assert_eq!(head["elements"][0]["rotation"], json!({ "angle": 22.5, "axis": "y", "origin": [8.0, 8.0, 5.0] }));
        assert_eq!(head["elements"][0]["from"], json!([4.5, 7.5, 1.5]));
        // The leg's cube carries no turn of its own: its bone does
        let leg: Value = serde_json::from_slice(&file(&entries, "assets/models/models/item/test_creature/leg_leg.json").data).unwrap();
        assert!(leg["elements"][0].get("rotation").is_none());
    }

    #[test]
    fn the_pack_is_whole_and_stays_the_same() {
        let m = model();
        let cfg = PackConfig::default();
        let files = assemble(&cfg, generate(&[&m], &mut Vec::new())).unwrap();
        assert!(files.iter().any(|e| e.path == "pack.mcmeta"));
        assert!(files.windows(2).all(|w| w[0].path < w[1].path));
        let again = assemble(&cfg, generate(&[&m], &mut Vec::new())).unwrap();
        assert_eq!(mcsv_pack::zip(&files), mcsv_pack::zip(&again));
        assert_eq!(mcsv_pack::id_text(pack_id()).len(), 36);
        assert_ne!(pack_id(), mcsv_pack::pack_id("customitems"));
    }

    #[test]
    fn the_example_model_loads_and_packs() {
        let raw = crate::bbmodel::parse(crate::defs::EXAMPLE_MODEL).unwrap();
        let m = crate::rig::build("example", &raw).unwrap();
        assert!(m.notes.is_empty(), "{:?}", m.notes);
        // Body, head (with its ear), the tail's turned cube and four legs
        assert_eq!(m.drawn().count(), 7);
        assert_eq!(m.animations.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["idle", "walk", "attack", "hurt", "death"]);
        assert!(m.hitbox.is_some() && m.has(crate::rig::Kind::Head) && m.has(crate::rig::Kind::Tag));
        let mut notes = Vec::new();
        let files = assemble(&PackConfig::default(), generate(&[&m], &mut notes)).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(files.len(), 1 + 1 + 7 * 2);
        assert!(mcsv_pack::png_size(&m.textures[0].png) == Some((32, 32)));
    }

    #[test]
    fn a_strip_of_frames_is_an_animated_picture() {
        let mut m = model();
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&16u32.to_be_bytes());
        png.extend_from_slice(&64u32.to_be_bytes());
        m.textures[0].png = png;
        m.textures[0].frame_time = 3;
        let entries = generate(&[&m], &mut Vec::new());
        let meta: Value = serde_json::from_slice(&file(&entries, "assets/models/textures/item/test_creature/skin.png.mcmeta").data).unwrap();
        assert_eq!(meta["animation"]["frametime"], 3);
    }
}

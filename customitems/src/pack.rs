//! The resource pack: the files made from the owner's items and glyphs, the files of `pack/`
//! as they are, and the zip that holds them. No server needed here.
//!
//! The zip is the same byte for byte as long as its content is: files in name order, one fixed
//! date. So the pack's SHA-1 only changes when the pack does, and players download it once.
use std::collections::{BTreeMap, BTreeSet};

use crate::defs::{Glyph, Item, Look, NS, PackConfig, Problem};

pub const MAX_FILES: usize = 20_000;
/// The game refuses server packs above 250 MB.
pub const MAX_BYTES: usize = 200 * 1024 * 1024;
pub const FONT: &str = "assets/minecraft/font/default.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub data: Vec<u8>,
}

impl Entry {
    fn json(path: String, value: &serde_json::Value) -> Entry {
        Entry { path, data: serde_json::to_vec_pretty(value).unwrap_or_default() }
    }
}

/// A path the game accepts inside a pack, and one of the places a pack has files in.
pub fn valid_pack_path(path: &str) -> bool {
    let plain = |seg: &str| !seg.is_empty() && seg != "." && seg != ".." && seg.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.'));
    path.len() <= 200 && path.split('/').all(plain) && (path == "pack.png" || path == "pack.mcmeta" || (path.starts_with("assets/") && path.matches('/').count() >= 2))
}

pub fn is_png(data: &[u8]) -> bool {
    data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
}

fn mcmeta(cfg: &PackConfig) -> serde_json::Value {
    let mut pack = serde_json::json!({ "description": cfg.description, "min_format": cfg.format_min, "max_format": cfg.format_max });
    // Games older than format 65 read the old fields; newer ones want them gone
    if cfg.format_min < 65 {
        pack["pack_format"] = cfg.format_min.into();
        pack["supported_formats"] = serde_json::json!([cfg.format_min, cfg.format_max]);
    }
    serde_json::json!({ "pack": pack })
}

pub struct Generated {
    pub entries: Vec<Entry>,
    /// Items whose picture could not be used: they keep the look of their material.
    pub plain: BTreeSet<String>,
    /// Glyphs that made it into the font.
    pub glyphs: BTreeSet<String>,
}

/// The files made from the definitions. `picture` reads a file of `textures/`.
pub fn generate(cfg: &PackConfig, items: &[Item], glyphs: &[Glyph], chars: &BTreeMap<String, u32>, picture: &mut dyn FnMut(&str) -> Option<Vec<u8>>, out: &mut Vec<Problem>) -> Generated {
    let mut made = Generated { entries: vec![Entry::json("pack.mcmeta".into(), &mcmeta(cfg))], plain: BTreeSet::new(), glyphs: BTreeSet::new() };
    let png = |picture: &mut dyn FnMut(&str) -> Option<Vec<u8>>, place: &str, file: &str, out: &mut Vec<Problem>| -> Option<Vec<u8>> {
        match picture(file) {
            Some(data) if is_png(&data) => Some(data),
            Some(_) => {
                out.push(Problem::new(place, format!("textures/{file} ไม่ใช่ไฟล์ PNG"), format!("textures/{file} is not a PNG file")));
                None
            }
            None => {
                out.push(Problem::new(place, format!("ไม่พบ textures/{file}"), format!("textures/{file} was not found")));
                None
            }
        }
    };
    for item in items {
        let model = match &item.look {
            Look::Plain => continue,
            Look::Model(model) => model.clone(),
            Look::Texture { file, handheld } => {
                let Some(data) = png(picture, &item.id, file, out) else {
                    made.plain.insert(item.id.clone());
                    continue;
                };
                made.entries.push(Entry { path: format!("assets/{NS}/textures/item/{}.png", item.id), data });
                // An animated picture brings its own description
                if let Some(meta) = picture(&format!("{file}.mcmeta")) {
                    made.entries.push(Entry { path: format!("assets/{NS}/textures/item/{}.png.mcmeta", item.id), data: meta });
                }
                let parent = if *handheld { "minecraft:item/handheld" } else { "minecraft:item/generated" };
                made.entries.push(Entry::json(format!("assets/{NS}/models/item/{}.json", item.id), &serde_json::json!({ "parent": parent, "textures": { "layer0": format!("{NS}:item/{}", item.id) } })));
                format!("{NS}:item/{}", item.id)
            }
        };
        made.entries.push(Entry::json(format!("assets/{NS}/items/{}.json", item.id), &serde_json::json!({ "model": { "type": "minecraft:model", "model": model } })));
    }
    let mut providers = Vec::new();
    for glyph in glyphs {
        let Some(code) = chars.get(&glyph.name).and_then(|c| char::from_u32(*c)) else { continue };
        let place = format!("glyphs.toml [{}]", glyph.name);
        let Some(data) = png(picture, &place, &glyph.file, out) else { continue };
        made.entries.push(Entry { path: format!("assets/{NS}/textures/font/{}.png", glyph.name), data });
        providers.push(serde_json::json!({ "type": "bitmap", "file": format!("{NS}:font/{}.png", glyph.name), "ascent": glyph.ascent, "height": glyph.height, "chars": [code.to_string()] }));
        made.glyphs.insert(glyph.name.clone());
    }
    if !providers.is_empty() {
        // The default font: the characters then work in every text, with no font to name
        made.entries.push(Entry::json(FONT.into(), &serde_json::json!({ "providers": providers })));
    }
    made
}

/// Put the owner's own pack files (`pack/`) over the generated ones. A file of the owner wins,
/// except the default font: there both lists of providers are kept.
pub fn assemble(generated: Vec<Entry>, own: Vec<Entry>, out: &mut Vec<Problem>) -> Result<Vec<Entry>, Problem> {
    let mut files: BTreeMap<String, Vec<u8>> = generated.into_iter().map(|e| (e.path, e.data)).collect();
    for entry in own {
        if !valid_pack_path(&entry.path) {
            out.push(Problem::new(&format!("pack/{}", entry.path), "ข้ามไฟล์นี้: ชื่อใน pack ใช้ได้แค่ a-z 0-9 _ - . และต้องอยู่ใต้ assets/", "skipped: names in a pack may only use a-z, 0-9, _ - . and belong under assets/"));
            continue;
        }
        if entry.path == FONT {
            if let Some(ours) = files.get(FONT) {
                let merged = serde_json::from_slice::<serde_json::Value>(&entry.data).ok().and_then(|mut theirs| {
                    let mine = serde_json::from_slice::<serde_json::Value>(ours).ok()?;
                    theirs.get_mut("providers")?.as_array_mut()?.extend(mine.get("providers")?.as_array()?.iter().cloned());
                    serde_json::to_vec_pretty(&theirs).ok()
                });
                match merged {
                    Some(data) => {
                        files.insert(entry.path, data);
                    }
                    None => out.push(Problem::new(&format!("pack/{FONT}"), "อ่านไม่ได้ จึงใช้ฟอนต์ของ glyph อย่างเดียว", "cannot be read, so only the glyph font is used")),
                }
                continue;
            }
        }
        files.insert(entry.path, entry.data);
    }
    let bytes: usize = files.values().map(Vec::len).sum();
    if files.len() > MAX_FILES || bytes > MAX_BYTES {
        return Err(Problem::new("pack", format!("pack ใหญ่เกินไป ({} ไฟล์ {} MB; สูงสุด {MAX_FILES} ไฟล์ {} MB)", files.len(), bytes >> 20, MAX_BYTES >> 20), format!("the pack is too large ({} files, {} MB; at most {MAX_FILES} files, {} MB)", files.len(), bytes >> 20, MAX_BYTES >> 20)));
    }
    Ok(files.into_iter().map(|(path, data)| Entry { path, data }).collect())
}

/// The id the pack is sent under: always the same, so a new pack replaces the old one. It is
/// made from the plugin's name and looks like any other UUID (version 4, variant 1).
pub fn pack_id() -> (u64, u64) {
    let fnv = |seed: u64| NS.bytes().fold(0xcbf2_9ce4_8422_2325 ^ seed, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3));
    ((fnv(0) & !0xF000) | 0x4000, (fnv(0x9E37_79B9_7F4A_7C15) & !(0xC000 << 48)) | (0x8000 << 48))
}

/// `8-4-4-4-12` hex, the way the client reports a pack id back.
pub fn id_text(id: (u64, u64)) -> String {
    let h = format!("{:016x}{:016x}", id.0, id.1);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *slot = c;
    }
    !data.iter().fold(!0u32, |crc, b| table[((crc ^ u32::from(*b)) & 0xFF) as usize] ^ (crc >> 8))
}

/// Text compresses well; pictures and sounds are compressed already and go in as they are.
fn worth_deflating(path: &str) -> bool {
    matches!(path.rsplit('.').next(), Some("json" | "mcmeta" | "txt" | "lang" | "fsh" | "vsh" | "glsl" | "properties"))
}

/// A zip of the entries, in the order given. Always the same bytes for the same entries.
pub fn zip(entries: &[Entry]) -> Vec<u8> {
    const DATE: u16 = (1 << 5) | 1; // 1 January 1980, the first day a zip can name
    let mut out = Vec::with_capacity(entries.iter().map(|e| e.data.len() + e.path.len() * 2 + 80).sum::<usize>() + 22);
    let mut central = Vec::new();
    for entry in entries {
        let packed = worth_deflating(&entry.path).then(|| miniz_oxide::deflate::compress_to_vec(&entry.data, 8)).filter(|p| p.len() < entry.data.len());
        let (method, body): (u16, &[u8]) = match &packed {
            Some(p) => (8, p),
            None => (0, &entry.data),
        };
        let offset = out.len() as u32;
        let mut head = Vec::with_capacity(26);
        head.extend_from_slice(&20u16.to_le_bytes()); // version needed
        head.extend_from_slice(&0u16.to_le_bytes()); // flags
        head.extend_from_slice(&method.to_le_bytes());
        head.extend_from_slice(&0u16.to_le_bytes()); // time
        head.extend_from_slice(&DATE.to_le_bytes());
        head.extend_from_slice(&crc32(&entry.data).to_le_bytes());
        head.extend_from_slice(&(body.len() as u32).to_le_bytes());
        head.extend_from_slice(&(entry.data.len() as u32).to_le_bytes());
        head.extend_from_slice(&(entry.path.len() as u16).to_le_bytes());
        head.extend_from_slice(&0u16.to_le_bytes()); // extra
        out.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
        out.extend_from_slice(&head);
        out.extend_from_slice(entry.path.as_bytes());
        out.extend_from_slice(body);
        central.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&head);
        central.extend_from_slice(&[0u8; 10]); // comment length, disk, attributes
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(entry.path.as_bytes());
    }
    let start = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
    out.extend_from_slice(&[0u8; 4]); // disk numbers
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defs::{self, Config};

    const PNG: [u8; 12] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4];

    fn u16_at(b: &[u8], at: usize) -> usize {
        usize::from(u16::from_le_bytes([b[at], b[at + 1]]))
    }

    fn u32_at(b: &[u8], at: usize) -> usize {
        u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize
    }

    /// Read a zip back through its central directory, the way an unzip tool does.
    fn unzip(z: &[u8]) -> Vec<Entry> {
        let end = z.len() - 22;
        assert_eq!(u32_at(z, end), 0x0605_4B50);
        let (count, mut at) = (u16_at(z, end + 10), u32_at(z, end + 16));
        let mut out = Vec::new();
        for _ in 0..count {
            assert_eq!(u32_at(z, at), 0x0201_4B50);
            let (method, crc, packed, size, name_len, local) = (u16_at(z, at + 10), u32_at(z, at + 16), u32_at(z, at + 20), u32_at(z, at + 24), u16_at(z, at + 28), u32_at(z, at + 42));
            let path = String::from_utf8(z[at + 46..at + 46 + name_len].to_vec()).unwrap();
            assert_eq!(u32_at(z, local), 0x0403_4B50);
            let body = local + 30 + u16_at(z, local + 26) + u16_at(z, local + 28);
            let raw = &z[body..body + packed];
            let data = if method == 8 { miniz_oxide::inflate::decompress_to_vec(raw).unwrap() } else { raw.to_vec() };
            assert_eq!((data.len(), crc32(&data) as usize), (size, crc), "{path}");
            out.push(Entry { path, data });
            at += 46 + name_len;
        }
        out
    }

    fn sample() -> (Vec<Item>, Vec<Glyph>, BTreeMap<String, u32>) {
        let mut out = Vec::new();
        let items = defs::parse_items(
            "items/a.toml",
            "[ruby]\ntexture = \"ruby.png\"\n[sword]\ntexture = \"sword.png\"\nhandheld = true\n[coin]\nmodel = \"minecraft:item/gold_nugget\"\n[plain]\nname = \"x\"\n[lost]\ntexture = \"nope.png\"\n",
            &mut out,
        );
        let glyphs = defs::parse_glyphs("[heart]\ntexture = \"heart.png\"\nheight = 7\nascent = 7\n[gone]\ntexture = \"nope.png\"\n", &mut out);
        assert!(out.is_empty(), "{out:?}");
        let chars = defs::assign_glyphs(&glyphs, &BTreeMap::new(), &mut out);
        (defs::merge_items(items, &mut out), glyphs, chars)
    }

    fn picture(file: &str) -> Option<Vec<u8>> {
        match file {
            "ruby.png" | "sword.png" | "heart.png" => Some(PNG.to_vec()),
            "ruby.png.mcmeta" => Some(b"{\"animation\":{}}".to_vec()),
            _ => None,
        }
    }

    #[test]
    fn the_pack_id_looks_like_a_uuid() {
        let text = id_text(pack_id());
        assert_eq!(text.len(), 36);
        assert_eq!(&text[14..15], "4");
        assert!(matches!(&text[19..20], "8" | "9" | "a" | "b"), "{text}");
        assert_eq!(id_text(pack_id()), text);
    }

    #[test]
    fn crc_is_the_one_zip_tools_expect() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn the_pack_has_what_each_look_needs() {
        let (items, glyphs, chars) = sample();
        let mut out = Vec::new();
        let made = generate(&Config::default().pack, &items, &glyphs, &chars, &mut picture, &mut out);
        let paths: Vec<&str> = made.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "pack.mcmeta",
                "assets/customitems/items/coin.json",
                "assets/customitems/textures/item/ruby.png",
                "assets/customitems/textures/item/ruby.png.mcmeta",
                "assets/customitems/models/item/ruby.json",
                "assets/customitems/items/ruby.json",
                "assets/customitems/textures/item/sword.png",
                "assets/customitems/models/item/sword.json",
                "assets/customitems/items/sword.json",
                "assets/customitems/textures/font/heart.png",
                "assets/minecraft/font/default.json",
            ]
        );
        assert!(paths.iter().all(|p| valid_pack_path(p)));
        let json = |path: &str| serde_json::from_slice::<serde_json::Value>(&made.entries.iter().find(|e| e.path == path).unwrap().data).unwrap();
        assert_eq!(json("assets/customitems/items/coin.json")["model"]["model"], "minecraft:item/gold_nugget");
        assert_eq!(json("assets/customitems/items/ruby.json")["model"], serde_json::json!({ "type": "minecraft:model", "model": "customitems:item/ruby" }));
        assert_eq!(json("assets/customitems/models/item/ruby.json"), serde_json::json!({ "parent": "minecraft:item/generated", "textures": { "layer0": "customitems:item/ruby" } }));
        assert_eq!(json("assets/customitems/models/item/sword.json")["parent"], "minecraft:item/handheld");
        assert_eq!(json(FONT)["providers"], serde_json::json!([{ "type": "bitmap", "file": "customitems:font/heart.png", "ascent": 7, "height": 7, "chars": ["\u{E001}"] }]));
        assert_eq!(json("pack.mcmeta")["pack"], serde_json::json!({ "description": "Custom items of this server", "min_format": 65, "max_format": 999 }));
        // The item and the glyph whose picture is missing are named, and left out
        assert_eq!(made.plain, BTreeSet::from(["lost".to_owned()]));
        assert_eq!(made.glyphs, BTreeSet::from(["heart".to_owned()]));
        let said: Vec<&str> = out.iter().map(|p| p.text(false)).collect();
        assert_eq!(said, ["lost: textures/nope.png was not found", "glyphs.toml [gone]: textures/nope.png was not found"]);
    }

    #[test]
    fn older_formats_get_the_older_fields() {
        let mut cfg = Config::default().pack;
        (cfg.format_min, cfg.format_max) = (46, 64);
        assert_eq!(mcmeta(&cfg)["pack"], serde_json::json!({ "description": cfg.description, "min_format": 46, "max_format": 64, "pack_format": 46, "supported_formats": [46, 64] }));
    }

    #[test]
    fn the_owners_files_win_and_fonts_are_joined() {
        let ours = vec![
            Entry { path: "pack.mcmeta".into(), data: b"{}".to_vec() },
            Entry { path: "assets/customitems/items/ruby.json".into(), data: b"ours".to_vec() },
            Entry::json(FONT.into(), &serde_json::json!({ "providers": [{ "type": "bitmap", "chars": ["\u{E000}"] }] })),
        ];
        let own = vec![
            Entry { path: "assets/customitems/items/ruby.json".into(), data: b"theirs".to_vec() },
            Entry { path: "assets/my_pack/sounds/ding.ogg".into(), data: vec![1, 2, 3] },
            Entry::json(FONT.into(), &serde_json::json!({ "providers": [{ "type": "space", "advances": { " ": 4 } }] })),
            Entry { path: "assets/My Pack/x.png".into(), data: vec![0] },
            Entry { path: "notes.txt".into(), data: vec![0] },
            Entry { path: "assets/../pack.mcmeta".into(), data: vec![0] },
        ];
        let mut out = Vec::new();
        let files = assemble(ours, own, &mut out).unwrap();
        let paths: Vec<&str> = files.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["assets/customitems/items/ruby.json", FONT, "assets/my_pack/sounds/ding.ogg", "pack.mcmeta"]);
        assert_eq!(files[0].data, b"theirs");
        let font: serde_json::Value = serde_json::from_slice(&files[1].data).unwrap();
        assert_eq!(font["providers"].as_array().unwrap().len(), 2);
        assert_eq!(font["providers"][0]["type"], "space");
        assert_eq!(out.len(), 3, "{out:?}");
    }

    #[test]
    fn a_pack_that_is_too_large_is_refused() {
        let many: Vec<Entry> = (0..=MAX_FILES).map(|i| Entry { path: format!("assets/a/{i}.json"), data: Vec::new() }).collect();
        assert!(assemble(Vec::new(), many, &mut Vec::new()).is_err());
    }

    #[test]
    fn the_zip_reads_back_and_never_changes() {
        let (items, glyphs, chars) = sample();
        let made = generate(&Config::default().pack, &items, &glyphs, &chars, &mut picture, &mut Vec::new());
        let big = Entry { path: "assets/customitems/lang/en_us.json".into(), data: br#"{"a": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#.to_vec() };
        let files = assemble(made.entries, vec![big.clone()], &mut Vec::new()).unwrap();
        let z = zip(&files);
        assert_eq!(unzip(&z), files);
        assert_eq!(zip(&files), z);
        // Text went in smaller, the picture as it is
        let local = |path: &str| z.windows(path.len()).position(|w| w == path.as_bytes()).unwrap() - 30;
        assert_eq!(u16_at(&z, local(&big.path) + 8), 8);
        assert!(u32_at(&z, local(&big.path) + 18) < big.data.len());
        assert_eq!(u16_at(&z, local("assets/customitems/textures/item/ruby.png") + 8), 0);
        assert_eq!(unzip(&zip(&[])), []);
    }
}

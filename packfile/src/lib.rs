//! A resource pack as a list of files, and the zip that holds them. Shared by the plugins that
//! make their own pack (CustomItems, Models). No server needed here.
//!
//! The zip is the same byte for byte as long as its content is: files in the order given, one
//! fixed date. So a pack's SHA-1 only changes when the pack does, and players download it once.

pub const MAX_FILES: usize = 20_000;
/// The game refuses server packs above 250 MB.
pub const MAX_BYTES: usize = 200 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub data: Vec<u8>,
}

impl Entry {
    pub fn json(path: String, value: &serde_json::Value) -> Entry {
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

/// Width and height of a PNG, from its header.
pub fn png_size(data: &[u8]) -> Option<(u32, u32)> {
    if !is_png(data) || data.get(12..16) != Some(b"IHDR") {
        return None;
    }
    let n = |at: usize| data.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    Some((n(16)?, n(20)?))
}

/// `pack.mcmeta` for a range of pack formats.
pub fn mcmeta(description: &str, format_min: u32, format_max: u32) -> serde_json::Value {
    let mut pack = serde_json::json!({ "description": description, "min_format": format_min, "max_format": format_max });
    // Games older than format 65 read the old fields; newer ones want them gone
    if format_min < 65 {
        pack["pack_format"] = format_min.into();
        pack["supported_formats"] = serde_json::json!([format_min, format_max]);
    }
    serde_json::json!({ "pack": pack })
}

/// The id a pack is sent under: always the same for one name, so a new pack replaces the old
/// one. It looks like any other UUID (version 4, variant 1).
pub fn pack_id(name: &str) -> (u64, u64) {
    let fnv = |seed: u64| name.bytes().fold(0xcbf2_9ce4_8422_2325 ^ seed, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3));
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

    #[test]
    fn paths_stay_inside_the_pack() {
        assert!(valid_pack_path("assets/models/items/dragon/body.json"));
        assert!(valid_pack_path("pack.mcmeta"));
        for bad in ["assets/x", "assets/../x/y.json", "Assets/a/b.json", "data/a/b.json", "assets/a/B.png", ""] {
            assert!(!valid_pack_path(bad), "{bad}");
        }
    }

    #[test]
    fn a_png_tells_its_size() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&64u32.to_be_bytes());
        png.extend_from_slice(&32u32.to_be_bytes());
        assert_eq!(png_size(&png), Some((64, 32)));
        assert_eq!(png_size(b"not a png"), None);
    }

    #[test]
    fn ids_differ_by_name_and_look_like_uuids() {
        let (a, b) = (id_text(pack_id("customitems")), id_text(pack_id("models")));
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
        assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"));
    }

    #[test]
    fn the_zip_never_changes_and_older_formats_get_older_fields() {
        let entries = [Entry::json("pack.mcmeta".into(), &mcmeta("x", 65, 999)), Entry { path: "assets/a/b.png".into(), data: vec![1, 2, 3] }];
        assert_eq!(zip(&entries), zip(&entries));
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert!(mcmeta("x", 46, 64)["pack"].get("pack_format").is_some());
        assert!(mcmeta("x", 65, 999)["pack"].get("pack_format").is_none());
    }
}

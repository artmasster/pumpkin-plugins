//! One map tile: 32 x 32 chunks = 512 x 512 pixels, one byte per pixel (see `color`).
//!
//! On disk a tile is one JSON text file, so anything that can only read text (the panel's file
//! API) can use it:
//!
//! ```json
//! {"v":1,"dim":"minecraft:overworld","x":0,"z":-1,"size":512,"band":32,"updated":1790000000,
//!  "chunks":812,"times":"<base64>","bands":["<base64>","", ...]}
//! ```
//!
//! * `x`, `z`: tile coordinates; the tile covers blocks `x*512 .. x*512+511` (same for z).
//! * `bands`: `size / band` strings, north to south. Each is base64 of a raw deflate stream
//!   (no zlib header) of `band * size` pixel bytes, row by row, west to east. An empty string is
//!   a band with nothing mapped.
//! * `times`: base64 of raw deflate of 1024 little-endian `u32`, one per chunk (row by row):
//!   Unix seconds of the last render, 0 = never.
//! * `chunks`: how many chunks of the tile are mapped.
use miniz_oxide::deflate::compress_to_vec;
use miniz_oxide::inflate::decompress_to_vec_with_limit;
use serde::Deserialize;

/// Chunks along one side of a tile.
pub const CHUNKS: usize = 32;
/// Pixels (blocks) along one side of a tile.
pub const SIZE: usize = CHUNKS * 16;
/// Pixel rows per band; a band is compressed on its own, so a small change recompresses little
/// and saving a tile is sixteen short steps instead of one long one.
pub const BAND_ROWS: usize = 32;
pub const BANDS: usize = SIZE / BAND_ROWS;
/// Deflate level. Measured on real terrain: 4 is within 3 % of 6 in size at half the time.
const LEVEL: u8 = 4;

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = u32::from(chunk[0]) << 16 | u32::from(*chunk.get(1).unwrap_or(&0)) << 8 | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

pub fn b64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (i, quad) in bytes.chunks(4).enumerate() {
        let last = i + 1 == bytes.len() / 4;
        let pad = if last { quad.iter().rev().take_while(|&&c| c == b'=').count() } else { 0 };
        if pad > 2 {
            return None;
        }
        let mut n = 0u32;
        for &c in &quad[..4 - pad] {
            n = n << 6 | val(c)?;
        }
        n <<= 6 * pad as u32;
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// Pixels to the text of one band: "" when nothing is mapped in it.
pub fn pack(px: &[u8]) -> String {
    if px.iter().all(|&p| p == 0) { String::new() } else { b64_encode(&compress_to_vec(px, LEVEL)) }
}

/// The text of one band back to `len` pixels.
pub fn unpack(text: &str, len: usize) -> Option<Vec<u8>> {
    if text.is_empty() {
        return Some(vec![0; len]);
    }
    let raw = decompress_to_vec_with_limit(&b64_decode(text)?, len).ok()?;
    (raw.len() == len).then_some(raw)
}

/// A square picture as bands of text (`rows` pixel rows each).
pub fn pack_bands(px: &[u8], size: usize, rows: usize) -> Vec<String> {
    px.chunks(size * rows).map(pack).collect()
}

/// Shrink a picture `size` pixels wide (a whole tile or one band of it) by `f`, a power of two
/// that divides both sides: each output pixel is the middle pixel of its `f` x `f` block, or the
/// first mapped pixel of the block when the middle is empty, so thin mapped strips do not vanish
/// from a far-out view.
pub fn downsample(px: &[u8], size: usize, f: usize) -> Vec<u8> {
    if f <= 1 {
        return px.to_vec();
    }
    let (n, rows) = (size / f, px.len() / size / f);
    let mut out = vec![0u8; n * rows];
    for oz in 0..rows {
        for ox in 0..n {
            let mut v = px[(oz * f + f / 2) * size + ox * f + f / 2];
            if v == 0 {
                'find: for dz in 0..f {
                    let row = (oz * f + dz) * size + ox * f;
                    for &p in &px[row..row + f] {
                        if p != 0 {
                            v = p;
                            break 'find;
                        }
                    }
                }
            }
            out[oz * n + ox] = v;
        }
    }
    out
}

/// The band texts of a tile file, without decoding them. `None` when it is not a tile file.
pub fn file_bands(text: &str) -> Option<Vec<String>> {
    let disk: Disk = serde_json::from_str(text).ok()?;
    (disk.size == SIZE && disk.band == BAND_ROWS && disk.bands.len() == BANDS).then_some(disk.bands)
}

/// The JSON of a tile inside an exported page: the same shape as a tile file, without the
/// per-chunk times, at any size.
pub fn export_json(x: i32, z: i32, size: usize, rows: usize, bands: &[String]) -> String {
    let list = bands.iter().map(|b| format!("\"{b}\"")).collect::<Vec<_>>().join(",");
    format!("{{\"v\":1,\"x\":{x},\"z\":{z},\"size\":{size},\"band\":{rows},\"bands\":[{list}]}}")
}

#[derive(Deserialize)]
struct Disk {
    #[serde(default)]
    size: usize,
    #[serde(default)]
    band: usize,
    #[serde(default)]
    times: String,
    #[serde(default)]
    bands: Vec<String>,
}

pub struct Tile {
    /// `SIZE * SIZE` pixel bytes, row by row from the north-west corner.
    pub px: Vec<u8>,
    /// Unix seconds of the last render of each chunk, 0 = never.
    pub times: Vec<u32>,
    /// The text of each band as it is (or will be) on disk; `None` = changed since.
    enc: Vec<Option<String>>,
    /// Chunks whose northern row was drawn without knowing the ground north of it
    /// (one word per chunk row, one bit per chunk). Not saved: a later refresh fixes them anyway.
    pub north: [u32; CHUNKS],
    /// Has changes that are not on disk.
    pub dirty: bool,
    /// Unix seconds of the first change since the last save.
    pub dirty_since: u64,
    /// Larger = used more recently.
    pub used: u64,
}

impl Default for Tile {
    fn default() -> Self {
        Tile::new()
    }
}

impl Tile {
    pub fn new() -> Self {
        Tile {
            px: vec![0; SIZE * SIZE],
            times: vec![0; CHUNKS * CHUNKS],
            enc: vec![Some(String::new()); BANDS],
            north: [0; CHUNKS],
            dirty: false,
            dirty_since: 0,
            used: 0,
        }
    }

    /// Chunks of this tile that are mapped.
    pub fn mapped(&self) -> u32 {
        self.times.iter().filter(|&&t| t != 0).count() as u32
    }

    pub fn chunk_time(&self, lx: usize, lz: usize) -> u32 {
        self.times[lz * CHUNKS + lx]
    }

    pub fn pixel(&self, x: usize, z: usize) -> u8 {
        self.px[z * SIZE + x]
    }

    pub fn chunk_pixels(&self, lx: usize, lz: usize) -> [u8; 256] {
        let mut out = [0u8; 256];
        for row in 0..16 {
            let at = (lz * 16 + row) * SIZE + lx * 16;
            out[row * 16..row * 16 + 16].copy_from_slice(&self.px[at..at + 16]);
        }
        out
    }

    /// Store a rendered chunk (`lx`, `lz` = chunk position inside the tile). Returns whether the
    /// picture changed; an identical re-render only moves the time and is not worth a disk write.
    pub fn set_chunk(&mut self, lx: usize, lz: usize, px: &[u8; 256], now: u64) -> bool {
        let first = self.times[lz * CHUNKS + lx] == 0;
        self.times[lz * CHUNKS + lx] = (now as u32).max(1);
        let mut changed = first;
        for row in 0..16 {
            let at = (lz * 16 + row) * SIZE + lx * 16;
            let new = &px[row * 16..row * 16 + 16];
            if self.px[at..at + 16] != *new {
                self.px[at..at + 16].copy_from_slice(new);
                changed = true;
            }
        }
        if changed {
            self.enc[lz * 16 / BAND_ROWS] = None;
            if !self.dirty {
                self.dirty = true;
                self.dirty_since = now;
            }
        }
        changed
    }

    /// Bands that still have to be compressed before the tile can be written.
    pub fn stale_bands(&self) -> usize {
        self.enc.iter().filter(|e| e.is_none()).count()
    }

    /// Compress one changed band. Returns false when there was none left.
    /// (One band at a time, so saving a tile never takes a whole tick.)
    pub fn encode_band(&mut self) -> bool {
        let Some(b) = self.enc.iter().position(Option::is_none) else { return false };
        let rows = &self.px[b * BAND_ROWS * SIZE..(b + 1) * BAND_ROWS * SIZE];
        self.enc[b] = Some(pack(rows));
        true
    }

    /// The tile file. Compresses whatever bands are still stale.
    pub fn to_json(&mut self, dim: &str, x: i32, z: i32, updated: u64) -> String {
        while self.encode_band() {}
        let bands = self.enc.iter().map(|b| format!("\"{}\"", b.as_deref().unwrap_or(""))).collect::<Vec<_>>().join(",");
        let mut raw = Vec::with_capacity(self.times.len() * 4);
        for t in &self.times {
            raw.extend_from_slice(&t.to_le_bytes());
        }
        let times = b64_encode(&compress_to_vec(&raw, LEVEL));
        let dim = serde_json::to_string(dim).unwrap_or_else(|_| "\"\"".into());
        format!(
            "{{\"v\":1,\"dim\":{dim},\"x\":{x},\"z\":{z},\"size\":{SIZE},\"band\":{BAND_ROWS},\"updated\":{updated},\"chunks\":{},\"times\":\"{times}\",\"bands\":[{bands}]}}",
            self.mapped()
        )
    }

    /// Read a tile file. `None` when it is not one of ours or is damaged.
    pub fn from_json(text: &str) -> Option<Tile> {
        let disk: Disk = serde_json::from_str(text).ok()?;
        if disk.size != SIZE || disk.band != BAND_ROWS || disk.bands.len() != BANDS {
            return None;
        }
        let mut tile = Tile::new();
        for (b, text) in disk.bands.iter().enumerate() {
            let rows = unpack(text, BAND_ROWS * SIZE)?;
            tile.px[b * BAND_ROWS * SIZE..(b + 1) * BAND_ROWS * SIZE].copy_from_slice(&rows);
        }
        let raw = unpack(&disk.times, CHUNKS * CHUNKS * 4)?;
        for (i, t) in raw.chunks_exact(4).enumerate() {
            tile.times[i] = u32::from_le_bytes([t[0], t[1], t[2], t[3]]);
        }
        tile.enc = disk.bands.into_iter().map(Some).collect();
        Some(tile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small deterministic generator, so the tests need no extra crate.
    fn noise(seed: u64) -> impl FnMut() -> u8 {
        let mut s = seed;
        move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        }
    }

    #[test]
    fn base64_round_trips() {
        assert_eq!(b64_encode(b""), "");
        assert_eq!(b64_encode(b"f"), "Zg==");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert_eq!(b64_encode(b"foo"), "Zm9v");
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
        let mut next = noise(7);
        for len in 0..70 {
            let data: Vec<u8> = (0..len).map(|_| next()).collect();
            assert_eq!(b64_decode(&b64_encode(&data)).as_deref(), Some(&data[..]));
        }
        assert_eq!(b64_decode("Zg="), None);
        assert_eq!(b64_decode("Z!=="), None);
        assert_eq!(b64_decode("===="), None);
    }

    #[test]
    fn an_empty_tile_is_tiny_and_comes_back_empty() {
        let mut t = Tile::new();
        let json = t.to_json("minecraft:overworld", 0, 0, 5);
        assert!(json.len() < 300, "{}", json.len());
        let back = Tile::from_json(&json).unwrap();
        assert!(back.px.iter().all(|&p| p == 0));
        assert_eq!(back.mapped(), 0);
    }

    #[test]
    fn a_tile_of_one_colour_round_trips() {
        let mut t = Tile::new();
        let px = [35u8; 256];
        for lz in 0..CHUNKS {
            for lx in 0..CHUNKS {
                assert!(t.set_chunk(lx, lz, &px, 1000));
            }
        }
        assert_eq!(t.stale_bands(), BANDS);
        let json = t.to_json("minecraft:overworld", -3, 4, 1000);
        // 262,144 identical pixels are a few hundred characters
        assert!(json.len() < 4000, "{}", json.len());
        let back = Tile::from_json(&json).unwrap();
        assert_eq!(back.px, t.px);
        assert_eq!(back.times, t.times);
        assert_eq!(back.mapped(), 1024);
        assert_eq!(back.stale_bands(), 0);
    }

    #[test]
    fn a_noisy_tile_round_trips() {
        let mut t = Tile::new();
        let mut next = noise(99);
        for lz in 0..CHUNKS {
            for lx in 0..CHUNKS {
                if (lx + lz) % 5 == 0 {
                    continue; // holes: chunks never seen
                }
                let mut px = [0u8; 256];
                px.iter_mut().for_each(|p| *p = 3 + next() % 228);
                t.set_chunk(lx, lz, &px, 2000 + (lx * lz) as u64);
            }
        }
        let json = t.to_json("minecraft:the_nether", 0, -1, 2000);
        assert!(!json.contains('<') && !json.contains('\n'));
        let back = Tile::from_json(&json).unwrap();
        assert_eq!(back.px, t.px);
        assert_eq!(back.times, t.times);
        assert_eq!(back.chunk_time(1, 0), 2000);
        assert_eq!(back.chunk_time(0, 0), 0);
        assert_eq!(back.chunk_pixels(3, 4), t.chunk_pixels(3, 4));
        // Writing it again without changes gives the same text: bands are not recompressed
        let mut back = back;
        assert_eq!(back.to_json("minecraft:the_nether", 0, -1, 2000), json);
    }

    #[test]
    fn only_a_changed_band_is_recompressed() {
        let mut t = Tile::new();
        t.set_chunk(0, 0, &[9; 256], 10);
        t.to_json("d", 0, 0, 10);
        t.dirty = false;
        assert_eq!(t.stale_bands(), 0);
        // The same picture again: time moves, nothing to write
        assert!(!t.set_chunk(0, 0, &[9; 256], 20));
        assert!(!t.dirty);
        assert_eq!(t.chunk_time(0, 0), 20);
        // One pixel differs: one band, and the tile wants saving
        let mut px = [9u8; 256];
        px[17] = 12;
        assert!(t.set_chunk(0, 0, &px, 30));
        assert!(t.dirty);
        assert_eq!(t.dirty_since, 30);
        assert_eq!(t.stale_bands(), 1);
        assert_eq!(t.pixel(1, 1), 12);
        // A chunk in the last rows belongs to the last band
        t.set_chunk(31, 31, &px, 31);
        assert_eq!(t.stale_bands(), 2);
        assert!(t.encode_band());
        assert!(t.encode_band());
        assert!(!t.encode_band());
        assert_eq!(t.dirty_since, 30);
    }

    #[test]
    fn damaged_files_are_refused() {
        assert!(Tile::from_json("").is_none());
        assert!(Tile::from_json("{}").is_none());
        assert!(Tile::from_json("{\"size\":512,\"band\":32,\"bands\":[\"\"]}").is_none());
        let mut t = Tile::new();
        t.set_chunk(0, 0, &[9; 256], 10);
        let json = t.to_json("d", 0, 0, 10);
        assert!(Tile::from_json(&json).is_some());
        assert!(Tile::from_json(&json.replacen("\"bands\":[\"", "\"bands\":[\"AAAA", 1)).is_none());
        assert!(Tile::from_json(&json[..json.len() - 9]).is_none());
    }

    #[test]
    fn downsampling_keeps_the_picture() {
        // 8 x 8, left half 5, right half 7
        let px: Vec<u8> = (0..64).map(|i| if i % 8 < 4 { 5 } else { 7 }).collect();
        assert_eq!(downsample(&px, 8, 1), px);
        assert_eq!(downsample(&px, 8, 2), [5, 5, 7, 7, 5, 5, 7, 7, 5, 5, 7, 7, 5, 5, 7, 7]);
        assert_eq!(downsample(&px, 8, 4), [5, 7, 5, 7]);
        assert_eq!(downsample(&px, 8, 8), [7]);
        // A single mapped pixel away from the middle of its block survives
        let mut lone = vec![0u8; 64];
        lone[0] = 9;
        assert_eq!(downsample(&lone, 8, 4), [9, 0, 0, 0]);
        // A whole tile: the right size, and a far corner pixel is where it should be
        let mut big = vec![0u8; SIZE * SIZE];
        big[(SIZE - 1) * SIZE + SIZE - 1] = 4;
        let small = downsample(&big, SIZE, 16);
        assert_eq!(small.len(), 32 * 32);
        assert_eq!(small[32 * 32 - 1], 4);
        assert_eq!(small.iter().filter(|&&p| p != 0).count(), 1);
        // Band by band gives the same picture as all at once (the export works band by band)
        let mut next = noise(31);
        let pic: Vec<u8> = (0..SIZE * SIZE).map(|_| if next() < 90 { 0 } else { 3 + next() % 200 }).collect();
        for f in [2usize, 4, 8, 16] {
            let whole = downsample(&pic, SIZE, f);
            let parts: Vec<u8> = pic.chunks(BAND_ROWS * SIZE).flat_map(|band| downsample(band, SIZE, f)).collect();
            assert_eq!(whole, parts, "factor {f}");
            assert_eq!(whole.len(), (SIZE / f) * (SIZE / f));
        }
    }

    #[test]
    fn band_texts_can_be_read_without_decoding() {
        let mut t = Tile::new();
        t.set_chunk(5, 9, &[40; 256], 10);
        let json = t.to_json("d", 0, 0, 10);
        let bands = file_bands(&json).unwrap();
        assert_eq!(bands.len(), BANDS);
        assert_eq!(bands.iter().filter(|b| !b.is_empty()).count(), 1);
        assert_eq!(unpack(&bands[9 * 16 / BAND_ROWS], BAND_ROWS * SIZE).unwrap()[..], t.px[(9 * 16 / BAND_ROWS) * BAND_ROWS * SIZE..][..BAND_ROWS * SIZE]);
        assert!(file_bands("{}").is_none());
        assert!(file_bands("no").is_none());
    }

    #[test]
    fn export_tiles_decode_like_tile_files() {
        let mut next = noise(5);
        let px: Vec<u8> = (0..SIZE * SIZE).map(|_| 3 + next() % 60).collect();
        let small = downsample(&px, SIZE, 4);
        let bands = pack_bands(&small, SIZE / 4, SIZE / 4);
        assert_eq!(bands.len(), 1);
        assert_eq!(unpack(&bands[0], small.len()).unwrap(), small);
        let json = export_json(-2, 7, SIZE / 4, SIZE / 4, &bands);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["x"], -2);
        assert_eq!(v["size"], 128);
        assert_eq!(v["bands"][0].as_str().unwrap(), bands[0]);
        // Full size in bands, as on disk
        assert_eq!(pack_bands(&px, SIZE, BAND_ROWS).len(), BANDS);
    }
}

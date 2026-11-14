//! Schematic files. WorldTools's own format keeps a table of block state strings
//! (`minecraft:oak_stairs[facing=north,half=top]`) and run-length encoded indexes into it, so a
//! file survives a Minecraft version change (state ids do not). Sponge/WorldEdit `.schem`
//! files (gzip + NBT, versions 1 to 3) can be read; their block entities, entities and biomes
//! are ignored. Plain Rust.

use crate::geo::P;

const MAGIC: &[u8; 5] = b"SCLP\x01";
/// Most palette entries a file may have, and the longest state string.
const MAX_PALETTE: usize = 65_535;
const MAX_STATE_LEN: usize = 512;

/// A box of blocks. `cells[x + z * width + y * width * length]` is 0 for "nothing here" or
/// 1 + an index into `palette`.
#[derive(Clone, Debug, PartialEq)]
pub struct Schematic {
    pub palette: Vec<String>,
    /// The lowest corner, relative to where the player stood when copying.
    pub offset: P,
    /// Width (x), height (y), length (z).
    pub size: (u32, u32, u32),
    pub cells: Vec<u32>,
}

/// A name a player may give a schematic: letters, digits, `_` and `-`, at most 32, lower case.
pub fn valid_name(s: &str) -> Option<String> {
    (!s.is_empty() && s.len() <= 32 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')).then(|| s.to_ascii_lowercase())
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn zigzag(v: i32) -> u64 {
    u64::from(((v << 1) ^ (v >> 31)) as u32)
}

fn unzigzag(v: u64) -> i32 {
    let v = v as u32;
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

/// Reads from a byte slice; every read checks the length.
struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        let end = self.at.checked_add(n).filter(|e| *e <= self.data.len()).ok_or("file ends too early")?;
        let s = &self.data[self.at..end];
        self.at = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }

    fn varint(&mut self) -> Result<u64, &'static str> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("bad number")
    }

    fn be<const N: usize>(&mut self) -> Result<[u8; N], &'static str> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn left(&self) -> usize {
        self.data.len() - self.at
    }
}

impl Schematic {
    /// From blocks at positions relative to the copy origin, each with a 0-based palette index.
    /// `None` when there is nothing, or when the box around them holds more than `max_cells`.
    pub fn from_blocks(blocks: &[(P, u32)], palette: Vec<String>, max_cells: u64) -> Option<Schematic> {
        let first = blocks.first()?.0;
        let (mut lo, mut hi) = (first, first);
        for (p, _) in blocks {
            lo = (lo.0.min(p.0), lo.1.min(p.1), lo.2.min(p.2));
            hi = (hi.0.max(p.0), hi.1.max(p.1), hi.2.max(p.2));
        }
        let span = |a: i32, b: i32| u32::try_from(i64::from(b) - i64::from(a) + 1).ok();
        let size = (span(lo.0, hi.0)?, span(lo.1, hi.1)?, span(lo.2, hi.2)?);
        let volume = u64::from(size.0).checked_mul(u64::from(size.1))?.checked_mul(u64::from(size.2))?;
        if volume > max_cells || palette.len() > MAX_PALETTE {
            return None;
        }
        let mut cells = vec![0u32; volume as usize];
        for (p, idx) in blocks {
            if *idx as usize >= palette.len() {
                return None;
            }
            let (x, y, z) = ((p.0 - lo.0) as u64, (p.1 - lo.1) as u64, (p.2 - lo.2) as u64);
            cells[(x + z * u64::from(size.0) + y * u64::from(size.0) * u64::from(size.2)) as usize] = idx + 1;
        }
        Some(Schematic { palette, offset: lo, size, cells })
    }

    /// Every block: position relative to the origin and 0-based palette index.
    pub fn blocks(&self) -> impl Iterator<Item = (P, u32)> + '_ {
        let (w, l) = (self.size.0 as usize, self.size.2 as usize);
        let o = self.offset;
        self.cells.iter().enumerate().filter(|(_, c)| **c != 0).map(move |(i, c)| {
            let (x, z, y) = (i % w, i / w % l, i / (w * l));
            ((o.0.saturating_add(x as i32), o.1.saturating_add(y as i32), o.2.saturating_add(z as i32)), c - 1)
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        put_varint(&mut out, self.palette.len() as u64);
        for s in &self.palette {
            put_varint(&mut out, s.len() as u64);
            out.extend_from_slice(s.as_bytes());
        }
        for v in [self.offset.0, self.offset.1, self.offset.2] {
            put_varint(&mut out, zigzag(v));
        }
        for v in [self.size.0, self.size.1, self.size.2] {
            put_varint(&mut out, u64::from(v));
        }
        let mut i = 0;
        while i < self.cells.len() {
            let cell = self.cells[i];
            let run = self.cells[i..].iter().take_while(|c| **c == cell).count();
            put_varint(&mut out, u64::from(cell));
            put_varint(&mut out, run as u64);
            i += run;
        }
        out
    }

    /// Read WorldTools's own format. Nothing in the file is trusted.
    pub fn decode(bytes: &[u8], max_cells: u64) -> Result<Schematic, &'static str> {
        let mut c = Cursor { data: bytes, at: 0 };
        if c.take(MAGIC.len()).ok() != Some(MAGIC.as_slice()) {
            return Err("not a WorldTools schematic");
        }
        let n = c.varint()? as usize;
        if n > MAX_PALETTE || n > c.left() {
            return Err("bad palette");
        }
        let mut palette = Vec::with_capacity(n);
        for _ in 0..n {
            let len = c.varint()? as usize;
            if len == 0 || len > MAX_STATE_LEN {
                return Err("bad palette");
            }
            palette.push(std::str::from_utf8(c.take(len)?).map_err(|_| "bad palette")?.to_owned());
        }
        let offset = (unzigzag(c.varint()?), unzigzag(c.varint()?), unzigzag(c.varint()?));
        let dim = |v: u64| u32::try_from(v).ok().filter(|v| *v >= 1).ok_or("bad size");
        let size = (dim(c.varint()?)?, dim(c.varint()?)?, dim(c.varint()?)?);
        let volume = u64::from(size.0).checked_mul(u64::from(size.1)).and_then(|v| v.checked_mul(u64::from(size.2))).ok_or("bad size")?;
        if volume > max_cells {
            return Err("too large");
        }
        let mut cells = Vec::with_capacity(volume as usize);
        while (cells.len() as u64) < volume {
            let (cell, run) = (c.varint()?, c.varint()?);
            if cell > n as u64 || run == 0 || run > volume - cells.len() as u64 {
                return Err("bad block data");
            }
            cells.resize(cells.len() + run as usize, cell as u32);
        }
        Ok(Schematic { palette, offset, size, cells })
    }
}

/// The parts of an NBT file a Sponge schematic needs; lists and long arrays are read past.
#[derive(Debug, PartialEq)]
enum Nbt {
    Int(i64),
    Bytes(Vec<u8>),
    Ints(Vec<i32>),
    Compound(Vec<(String, Nbt)>),
    Other,
}

impl Nbt {
    fn get(&self, key: &str) -> Option<&Nbt> {
        match self {
            Nbt::Compound(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn int(&self, key: &str) -> Option<i64> {
        match self.get(key)? {
            Nbt::Int(v) => Some(*v),
            _ => None,
        }
    }
}

fn nbt_string(c: &mut Cursor) -> Result<String, &'static str> {
    let len = u16::from_be_bytes(c.be()?) as usize;
    Ok(String::from_utf8_lossy(c.take(len)?).into_owned())
}

fn nbt_payload(c: &mut Cursor, tag: u8, depth: u32) -> Result<Nbt, &'static str> {
    if depth > 64 {
        return Err("NBT nested too deep");
    }
    let count = |c: &mut Cursor, each: usize| -> Result<usize, &'static str> {
        let n = i32::from_be_bytes(c.be()?);
        usize::try_from(n).ok().filter(|n| n.checked_mul(each.max(1)).is_some_and(|b| b <= c.left())).ok_or("bad NBT length")
    };
    Ok(match tag {
        1 => Nbt::Int(i64::from(c.u8()? as i8)),
        2 => Nbt::Int(i64::from(i16::from_be_bytes(c.be()?))),
        3 => Nbt::Int(i64::from(i32::from_be_bytes(c.be()?))),
        4 => Nbt::Int(i64::from_be_bytes(c.be()?)),
        5 => {
            c.take(4)?;
            Nbt::Other
        }
        6 => {
            c.take(8)?;
            Nbt::Other
        }
        7 => {
            let n = count(c, 1)?;
            Nbt::Bytes(c.take(n)?.to_vec())
        }
        8 => {
            nbt_string(c)?;
            Nbt::Other
        }
        9 => {
            let inner = c.u8()?;
            let n = i32::from_be_bytes(c.be()?).max(0) as usize;
            if n > 0 && (inner == 0 || n > c.left()) {
                return Err("bad NBT list");
            }
            for _ in 0..n {
                nbt_payload(c, inner, depth + 1)?;
            }
            Nbt::Other
        }
        10 => {
            let mut items = Vec::new();
            loop {
                let t = c.u8()?;
                if t == 0 {
                    break;
                }
                let name = nbt_string(c)?;
                let value = nbt_payload(c, t, depth + 1)?;
                if value != Nbt::Other {
                    items.push((name, value));
                }
            }
            Nbt::Compound(items)
        }
        11 => {
            let n = count(c, 4)?;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(i32::from_be_bytes(c.be()?));
            }
            Nbt::Ints(v)
        }
        12 => {
            let n = count(c, 8)?;
            c.take(n * 8)?;
            Nbt::Other
        }
        _ => return Err("bad NBT tag"),
    })
}

/// Strip the gzip wrapper and inflate, giving up beyond `limit` bytes.
fn gunzip(bytes: &[u8], limit: usize) -> Result<Vec<u8>, &'static str> {
    if bytes.len() < 18 || bytes[0] != 0x1f || bytes[1] != 0x8b || bytes[2] != 8 {
        return Err("not a gzip file");
    }
    let flags = bytes[3];
    let mut at = 10usize;
    if flags & 4 != 0 {
        let len = usize::from(u16::from_le_bytes([*bytes.get(at).ok_or("bad gzip header")?, *bytes.get(at + 1).ok_or("bad gzip header")?]));
        at += 2 + len;
    }
    for bit in [8u8, 16] {
        if flags & bit != 0 {
            at += bytes.get(at..).and_then(|rest| rest.iter().position(|b| *b == 0)).ok_or("bad gzip header")? + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    let body = bytes.get(at..bytes.len() - 8).ok_or("bad gzip header")?;
    miniz_oxide::inflate::decompress_to_vec_with_limit(body, limit).map_err(|_| "cannot unpack (damaged, or larger than allowed)")
}

/// Read a Sponge schematic (`.schem` of WorldEdit / FAWE, format versions 1 to 3): gzip, NBT,
/// a palette of state strings and varint block data. Block entities (chest contents, sign
/// text), entities and biomes are not read.
pub fn sponge(bytes: &[u8], max_cells: u64) -> Result<Schematic, &'static str> {
    // Block data is at most 3 bytes a block; leave room for the palette and what is skipped
    let limit = usize::try_from(max_cells.saturating_mul(3).saturating_add(8 << 20)).unwrap_or(usize::MAX);
    let raw = if bytes.first() == Some(&10) { bytes.to_vec() } else { gunzip(bytes, limit)? };
    let mut c = Cursor { data: &raw, at: 0 };
    if c.u8()? != 10 {
        return Err("not an NBT file");
    }
    nbt_string(&mut c)?;
    let root = nbt_payload(&mut c, 10, 0)?;
    // Version 3 wraps everything in a "Schematic" compound and the blocks in "Blocks"
    let top = root.get("Schematic").unwrap_or(&root);
    let (blocks, data_key) = match top.get("Blocks") {
        Some(b) => (b, "Data"),
        None => (top, "BlockData"),
    };
    let dim = |key: &str| top.int(key).map(|v| (v as u16) as u32).filter(|v| *v >= 1).ok_or("not a Sponge schematic (no size)");
    let size = (dim("Width")?, dim("Height")?, dim("Length")?);
    let volume = u64::from(size.0) * u64::from(size.1) * u64::from(size.2);
    if volume > max_cells {
        return Err("too large");
    }
    let Some(Nbt::Compound(entries)) = blocks.get("Palette") else { return Err("not a Sponge schematic (no palette)") };
    if entries.is_empty() || entries.len() > MAX_PALETTE {
        return Err("bad palette");
    }
    let mut palette = vec![String::new(); entries.len()];
    for (name, id) in entries {
        let Nbt::Int(id) = id else { return Err("bad palette") };
        let slot = usize::try_from(*id).ok().and_then(|i| palette.get_mut(i)).ok_or("bad palette")?;
        if name.is_empty() || name.len() > MAX_STATE_LEN || !slot.is_empty() {
            return Err("bad palette");
        }
        *slot = name.clone();
    }
    let Some(Nbt::Bytes(data)) = blocks.get(data_key) else { return Err("not a Sponge schematic (no block data)") };
    let mut d = Cursor { data, at: 0 };
    let mut cells = Vec::with_capacity(volume as usize);
    while (cells.len() as u64) < volume {
        let v = d.varint()?;
        if v >= palette.len() as u64 {
            return Err("bad block data");
        }
        cells.push(v as u32 + 1);
    }
    // Where the lowest corner sits relative to the player who copied it: version 3 says so in
    // "Offset"; WorldEdit's version 2 keeps it in its own metadata ("Offset" there is a world position)
    let ints = |n: Option<&Nbt>| match n {
        Some(Nbt::Ints(v)) if v.len() == 3 => Some((v[0], v[1], v[2])),
        _ => None,
    };
    let meta = top.get("Metadata");
    let we = |key: &str| meta.and_then(|m| m.int(key)).map(|v| v as i32);
    let offset = match (we("WEOffsetX"), we("WEOffsetY"), we("WEOffsetZ")) {
        (Some(x), Some(y), Some(z)) => (x, y, z),
        _ if top.get("Blocks").is_some() => ints(top.get("Offset")).unwrap_or((0, 0, 0)),
        _ => (0, 0, 0),
    };
    Ok(Schematic { palette, offset, size, cells })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Schematic {
        // 3 x 2 x 2 with a hole and three kinds of block
        let palette = vec!["minecraft:stone".to_owned(), "minecraft:oak_stairs[facing=north,half=top]".to_owned(), "minecraft:air".to_owned()];
        let mut blocks = Vec::new();
        for y in 0..2 {
            for z in 0..2 {
                for x in 0..3 {
                    if (x, y, z) != (1, 1, 1) {
                        blocks.push(((x - 1, y + 4, z - 7), ((x + y + z) % 3) as u32));
                    }
                }
            }
        }
        Schematic::from_blocks(&blocks, palette, 1000).unwrap()
    }

    #[test]
    fn names() {
        assert_eq!(valid_name("My-House_2"), Some("my-house_2".into()));
        for bad in ["", "a b", "../x", "a/b", "a.b", "บ้าน", "x\0", &"a".repeat(33), "a\\b", "con:"] {
            assert_eq!(valid_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn numbers() {
        for v in [0, 1, -1, 63, -64, 64, 1_000_000, -1_000_000, i32::MAX, i32::MIN] {
            assert_eq!(unzigzag(zigzag(v)), v);
        }
        let mut out = Vec::new();
        for v in [0u64, 127, 128, 300, u64::from(u32::MAX), u64::MAX] {
            put_varint(&mut out, v);
        }
        let mut c = Cursor { data: &out, at: 0 };
        for v in [0u64, 127, 128, 300, u64::from(u32::MAX), u64::MAX] {
            assert_eq!(c.varint(), Ok(v));
        }
        assert!(c.varint().is_err());
        assert!(Cursor { data: &[0xff; 20], at: 0 }.varint().is_err());
    }

    #[test]
    fn round_trip() {
        let s = sample();
        assert_eq!((s.size, s.offset, s.cells.len()), ((3, 2, 2), (-1, 4, -7), 12));
        assert_eq!(s.blocks().count(), 11);
        assert!(s.blocks().any(|(p, i)| p == (-1, 4, -7) && i == 0));
        assert!(s.blocks().any(|(p, i)| p == (1, 5, -7) && i == 0));
        assert!(!s.blocks().any(|(p, _)| p == (0, 5, -6)));
        let bytes = s.encode();
        assert_eq!(Schematic::decode(&bytes, 1000), Ok(s.clone()));
        // The blocks come back where they were
        let back = Schematic::decode(&bytes, 1000).unwrap();
        let mut a: Vec<_> = s.blocks().collect();
        let mut b: Vec<_> = back.blocks().collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
        // Long runs stay small: 100,000 blocks of one kind in a few bytes
        let big: Vec<(P, u32)> = (0..100).flat_map(|x| (0..10).flat_map(move |y| (0..100).map(move |z| ((x, y, z), 0)))).collect();
        let big = Schematic::from_blocks(&big, vec!["minecraft:stone".into()], 200_000).unwrap();
        let bytes = big.encode();
        assert!(bytes.len() < 64, "{}", bytes.len());
        assert_eq!(Schematic::decode(&bytes, 200_000).unwrap().blocks().count(), 100_000);
        assert_eq!(Schematic::decode(&bytes, 99_999), Err("too large"));
    }

    #[test]
    fn limits_and_garbage() {
        assert!(Schematic::from_blocks(&[], vec![], 10).is_none());
        assert!(Schematic::from_blocks(&[((0, 0, 0), 0), ((9, 9, 9), 0)], vec!["a".into()], 999).is_none());
        assert!(Schematic::from_blocks(&[((0, 0, 0), 1)], vec!["a".into()], 999).is_none());
        assert!(Schematic::from_blocks(&[((i32::MIN, 0, 0), 0), ((i32::MAX, 0, 0), 0)], vec!["a".into()], u64::MAX).is_none());
        let good = sample().encode();
        // Every cut-off copy and every single damaged byte is refused or decoded, never a panic
        for cut in 0..good.len() {
            assert!(Schematic::decode(&good[..cut], 1000).is_err(), "cut at {cut}");
        }
        for i in 0..good.len() {
            let mut bad = good.clone();
            bad[i] ^= 0xff;
            let _ = Schematic::decode(&bad, 1000);
        }
        assert_eq!(Schematic::decode(b"hello", 1000), Err("not a WorldTools schematic"));
        // A file that claims a huge box
        let mut huge = MAGIC.to_vec();
        put_varint(&mut huge, 1);
        put_varint(&mut huge, 1);
        huge.push(b'a');
        for _ in 0..3 {
            put_varint(&mut huge, 0);
        }
        let head = huge.len();
        for _ in 0..3 {
            put_varint(&mut huge, 100_000);
        }
        assert_eq!(Schematic::decode(&huge, 1_000_000), Err("too large"));
        huge.truncate(head);
        for _ in 0..3 {
            put_varint(&mut huge, u64::from(u32::MAX));
        }
        assert_eq!(Schematic::decode(&huge, u64::MAX), Err("bad size"));
    }

    // A tiny NBT writer for the Sponge tests
    fn tag(out: &mut Vec<u8>, t: u8, name: &str) {
        out.push(t);
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
    }
    fn short(out: &mut Vec<u8>, name: &str, v: i16) {
        tag(out, 2, name);
        out.extend_from_slice(&v.to_be_bytes());
    }
    fn int(out: &mut Vec<u8>, name: &str, v: i32) {
        tag(out, 3, name);
        out.extend_from_slice(&v.to_be_bytes());
    }
    fn bytes(out: &mut Vec<u8>, name: &str, v: &[u8]) {
        tag(out, 7, name);
        out.extend_from_slice(&(v.len() as i32).to_be_bytes());
        out.extend_from_slice(v);
    }
    fn palette(out: &mut Vec<u8>) {
        tag(out, 10, "Palette");
        int(out, "minecraft:air", 0);
        int(out, "minecraft:stone", 1);
        int(out, "minecraft:oak_stairs[facing=east,half=bottom,shape=straight,waterlogged=false]", 2);
        out.push(0);
    }
    fn gzip(raw: &[u8]) -> Vec<u8> {
        let mut out = vec![0x1f, 0x8b, 8, 8, 0, 0, 0, 0, 0, 3];
        out.extend_from_slice(b"house.schem\0");
        out.extend_from_slice(&miniz_oxide::deflate::compress_to_vec(raw, 6));
        out.extend_from_slice(&[0; 8]);
        out
    }
    /// 2 x 1 x 3: stone, stairs, air, stone, stone, air (x runs first, then z)
    const DATA: [u8; 6] = [1, 2, 0, 1, 1, 0];

    fn sponge_v2() -> Vec<u8> {
        let mut n = Vec::new();
        tag(&mut n, 10, "Schematic");
        int(&mut n, "Version", 2);
        int(&mut n, "DataVersion", 3700);
        short(&mut n, "Width", 2);
        short(&mut n, "Height", 1);
        short(&mut n, "Length", 3);
        tag(&mut n, 11, "Offset");
        n.extend_from_slice(&3i32.to_be_bytes());
        for v in [100i32, 64, -200] {
            n.extend_from_slice(&v.to_be_bytes());
        }
        tag(&mut n, 10, "Metadata");
        int(&mut n, "WEOffsetX", -1);
        int(&mut n, "WEOffsetY", 0);
        int(&mut n, "WEOffsetZ", -2);
        n.push(0);
        palette(&mut n);
        bytes(&mut n, "BlockData", &DATA);
        // A list of compounds (block entities) that must be read past
        tag(&mut n, 9, "BlockEntities");
        n.push(10);
        n.extend_from_slice(&1i32.to_be_bytes());
        tag(&mut n, 8, "Id");
        n.extend_from_slice(&[0, 5]);
        n.extend_from_slice(b"chest");
        tag(&mut n, 12, "Longs");
        n.extend_from_slice(&1i32.to_be_bytes());
        n.extend_from_slice(&[0; 8]);
        n.push(0);
        n.push(0);
        n
    }

    fn sponge_v3() -> Vec<u8> {
        let mut n = Vec::new();
        tag(&mut n, 10, "");
        tag(&mut n, 10, "Schematic");
        int(&mut n, "Version", 3);
        short(&mut n, "Width", 2);
        short(&mut n, "Height", 1);
        short(&mut n, "Length", 3);
        tag(&mut n, 11, "Offset");
        n.extend_from_slice(&3i32.to_be_bytes());
        for v in [4i32, -1, 0] {
            n.extend_from_slice(&v.to_be_bytes());
        }
        tag(&mut n, 10, "Blocks");
        palette(&mut n);
        bytes(&mut n, "Data", &DATA);
        n.push(0);
        n.push(0);
        n.push(0);
        n
    }

    #[test]
    fn sponge_files() {
        for (file, offset) in [(sponge_v2(), (-1, 0, -2)), (sponge_v3(), (4, -1, 0))] {
            for packed in [gzip(&file), file.clone()] {
                let s = sponge(&packed, 1000).unwrap();
                assert_eq!((s.size, s.offset), ((2, 1, 3), offset));
                assert_eq!(s.palette[1], "minecraft:stone");
                assert!(s.palette[2].starts_with("minecraft:oak_stairs[facing=east"));
                let got: Vec<(P, u32)> = s.blocks().map(|(p, i)| ((p.0 - offset.0, p.1 - offset.1, p.2 - offset.2), i)).collect();
                assert_eq!(got, vec![((0, 0, 0), 1), ((1, 0, 0), 2), ((0, 0, 1), 0), ((1, 0, 1), 1), ((0, 0, 2), 1), ((1, 0, 2), 0)]);
            }
            assert_eq!(sponge(&gzip(&file), 5), Err("too large"));
            // Damaged files are refused, never a panic
            let packed = gzip(&file);
            for cut in 0..packed.len() {
                assert!(sponge(&packed[..cut], 1000).is_err(), "cut at {cut}");
            }
            for cut in 0..file.len() {
                let _ = sponge(&file[..cut], 1000);
            }
            for i in 0..file.len() {
                let mut bad = file.clone();
                bad[i] ^= 0xff;
                let _ = sponge(&bad, 1000);
            }
        }
        assert!(sponge(b"not a schematic at all", 1000).is_err());
        // A small file that unpacks to far more than any schematic of the allowed size
        let bomb = gzip(&vec![0u8; 20 << 20]);
        assert!(bomb.len() < 100_000);
        assert!(sponge(&bomb, 1000).is_err());
    }
}

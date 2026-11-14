//! Tiles on disk and in memory.
//!
//! ```text
//! plugins/data/worldmap/
//!   index.json                     what exists (below)
//!   tiles/<dim>/<x>_<z>.json       one tile (see `tile`)
//! ```
//!
//! `index.json`:
//!
//! ```json
//! {"v":1,"plugin":"0.4.1","updated":1790000000,"tile_chunks":32,"tile_px":512,"band_rows":32,
//!  "palette":"<256 x rrggbb: the colour of every pixel byte; byte 0 = nothing mapped>",
//!  "dimensions":[{"name":"minecraft:overworld","dir":"overworld","spawn":[0,64,0],"min_y":-64,
//!    "chunks":812,"bytes":51234,
//!    "tiles":[{"x":0,"z":-1,"file":"tiles/overworld/0_-1.json","updated":1790000000,"chunks":812,"bytes":51234}]}]}
//! ```
//!
//! Only `max_tiles` tiles are held in memory; the least recently used one is written (if it has
//! changes) and dropped when another is needed.
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::color;
use crate::tile::{BAND_ROWS, CHUNKS, SIZE, Tile};

/// A tile with changes is written this long after its first unsaved change.
pub const SAVE_AFTER_SECS: u64 = 15;
/// ... or this long when all the room for tiles in memory is in use.
pub const SAVE_SOON_SECS: u64 = 2;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TileMeta {
    pub x: i32,
    pub z: i32,
    pub file: String,
    pub updated: u64,
    pub chunks: u32,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct DimDisk {
    name: String,
    dir: String,
    #[serde(default)]
    spawn: [i32; 3],
    #[serde(default)]
    min_y: i32,
    #[serde(default)]
    chunks: u64,
    #[serde(default)]
    bytes: u64,
    #[serde(default)]
    tiles: Vec<TileMeta>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct IndexDisk {
    v: u32,
    plugin: String,
    updated: u64,
    tile_chunks: u32,
    tile_px: u32,
    band_rows: u32,
    palette: String,
    dimensions: Vec<DimDisk>,
}

/// One dimension of the index.
#[derive(Clone, Debug, Default)]
pub struct Dim {
    /// `minecraft:overworld`
    pub name: String,
    /// Folder under `tiles/`: `overworld`
    pub dir: String,
    pub spawn: [i32; 3],
    pub min_y: i32,
    pub tiles: BTreeMap<(i32, i32), TileMeta>,
}

impl Dim {
    pub fn chunks(&self) -> u64 {
        self.tiles.values().map(|t| u64::from(t.chunks)).sum()
    }

    pub fn bytes(&self) -> u64 {
        self.tiles.values().map(|t| t.bytes).sum()
    }
}

/// Tile key: dimension number (position in `Store::dims`), tile x, tile z.
pub type Key = (u8, i32, i32);

/// Tile and position inside it of a chunk.
pub fn locate(cx: i32, cz: i32) -> ((i32, i32), (usize, usize)) {
    let n = CHUNKS as i32;
    ((cx.div_euclid(n), cz.div_euclid(n)), (cx.rem_euclid(n) as usize, cz.rem_euclid(n) as usize))
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

pub struct Store {
    root: PathBuf,
    plugin: String,
    pub max_tiles: usize,
    pub dims: Vec<Dim>,
    tiles: HashMap<Key, Tile>,
    clock: u64,
    index_dirty: bool,
    /// Tiles written and tiles that could not be written, since the start.
    pub saved: u64,
    pub failed: u64,
}

impl Store {
    /// Open the map under `root` (the plugin data folder), reading `index.json` if there is one.
    pub fn open(root: &Path, plugin: &str, max_tiles: usize) -> Store {
        let mut store = Store {
            root: root.to_path_buf(),
            plugin: plugin.to_owned(),
            max_tiles: max_tiles.max(1),
            dims: Vec::new(),
            tiles: HashMap::new(),
            clock: 0,
            index_dirty: false,
            saved: 0,
            failed: 0,
        };
        let disk = std::fs::read_to_string(root.join("index.json")).ok().and_then(|t| serde_json::from_str::<IndexDisk>(&t).ok());
        if let Some(disk) = disk {
            for d in disk.dimensions {
                if !valid_dir(&d.dir) || store.dims.iter().any(|x| x.name == d.name || x.dir == d.dir) || store.dims.len() >= 250 {
                    continue;
                }
                let dir = d.dir.clone();
                let tiles = d.tiles.into_iter().map(|t| ((t.x, t.z), TileMeta { file: tile_file(&dir, t.x, t.z), ..t })).collect();
                store.dims.push(Dim { name: d.name, dir: d.dir, spawn: d.spawn, min_y: d.min_y, tiles });
            }
        }
        store
    }

    /// Number of the dimension `name`, added to the index when new. `None` when the folder name
    /// is unusable or taken by another dimension.
    pub fn dim_id(&mut self, name: &str, dir: &str) -> Option<u8> {
        if let Some(i) = self.dims.iter().position(|d| d.name == name) {
            return Some(i as u8);
        }
        if !valid_dir(dir) || self.dims.iter().any(|d| d.dir == dir) || self.dims.len() >= 250 {
            return None;
        }
        // A folder of tiles without an index entry (index lost): take the tiles back in
        let mut dim = Dim { name: name.to_owned(), dir: dir.to_owned(), ..Dim::default() };
        if let Ok(list) = std::fs::read_dir(self.root.join("tiles").join(dir)) {
            for entry in list.flatten() {
                let file = entry.file_name().to_string_lossy().into_owned();
                let Some((x, z)) = file.strip_suffix(".json").and_then(|s| s.split_once('_')).and_then(|(x, z)| Some((x.parse().ok()?, z.parse().ok()?))) else {
                    continue;
                };
                let Some(tile) = std::fs::read_to_string(entry.path()).ok().and_then(|t| Tile::from_json(&t)) else { continue };
                let bytes = entry.metadata().map_or(0, |m| m.len());
                let updated = tile.times.iter().copied().max().unwrap_or(0);
                dim.tiles.insert((x, z), TileMeta { x, z, file: tile_file(dir, x, z), updated: u64::from(updated), chunks: tile.mapped(), bytes });
            }
        }
        self.dims.push(dim);
        self.index_dirty = true;
        Some((self.dims.len() - 1) as u8)
    }

    /// Remember where the world spawn is (for the index and the exported page).
    pub fn set_world(&mut self, dim: u8, spawn: [i32; 3], min_y: i32) {
        if let Some(d) = self.dims.get_mut(dim as usize) {
            if d.spawn != spawn || d.min_y != min_y {
                d.spawn = spawn;
                d.min_y = min_y;
                self.index_dirty = true;
            }
        }
    }

    fn path(&self, key: Key) -> Option<PathBuf> {
        let d = self.dims.get(key.0 as usize)?;
        Some(self.root.join(tile_file(&d.dir, key.1, key.2)))
    }

    pub fn in_memory(&self) -> usize {
        self.tiles.len()
    }

    pub fn dirty_tiles(&self) -> usize {
        self.tiles.values().filter(|t| t.dirty).count()
    }

    pub fn dirty_in(&self, dim: u8) -> usize {
        self.tiles.iter().filter(|(k, t)| k.0 == dim && t.dirty).count()
    }

    /// The tile `key`, read from disk or new, counted as just used. Drops the least recently used
    /// tile when there are more than `max_tiles`.
    pub fn tile(&mut self, key: Key, now: u64) -> &mut Tile {
        self.clock += 1;
        if !self.tiles.contains_key(&key) {
            while self.tiles.len() >= self.max_tiles {
                // Strictly the least recently used one. (Preferring tiles without unsaved changes was
                // tried and was worse: under pressure tiles are saved within seconds, so the "clean"
                // one was often the tile being drawn into, and it had to be read back at once.)
                let Some(old) = self.tiles.iter().min_by_key(|(_, t)| t.used).map(|(k, _)| *k) else { break };
                if self.tiles.get(&old).is_some_and(|t| t.dirty) {
                    self.save(old, now);
                }
                self.tiles.remove(&old);
            }
            let known = self.dims.get(key.0 as usize).is_some_and(|d| d.tiles.contains_key(&(key.1, key.2)));
            let tile = if known { self.path(key).and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| Tile::from_json(&t)) } else { None };
            self.tiles.insert(key, tile.unwrap_or_default());
        }
        let clock = self.clock;
        let tile = self.tiles.get_mut(&key).expect("just inserted");
        tile.used = clock;
        tile
    }

    /// When chunk (`cx`, `cz`) of dimension `dim` was last rendered (Unix seconds, 0 = never).
    /// Does not touch the disk for a tile that does not exist.
    pub fn chunk_time(&mut self, dim: u8, cx: i32, cz: i32, now: u64) -> u32 {
        let ((tx, tz), (lx, lz)) = locate(cx, cz);
        let key = (dim, tx, tz);
        // Asked hundreds of times a second (around every player): one lookup when the tile is in memory
        if let Some(tile) = self.tiles.get_mut(&key) {
            self.clock += 1;
            tile.used = self.clock;
            return tile.chunk_time(lx, lz);
        }
        if !self.dims.get(dim as usize).is_some_and(|d| d.tiles.contains_key(&(tx, tz))) {
            return 0;
        }
        self.tile(key, now).chunk_time(lx, lz)
    }

    /// Chunks and tiles of a dimension as they are now, counting what is not saved yet.
    pub fn mapped(&self, dim: u8) -> (u64, usize) {
        let Some(d) = self.dims.get(dim as usize) else { return (0, 0) };
        let (mut chunks, mut tiles) = (0u64, 0usize);
        for (pos, meta) in &d.tiles {
            if !self.tiles.contains_key(&(dim, pos.0, pos.1)) {
                chunks += u64::from(meta.chunks);
                tiles += 1;
            }
        }
        for (key, tile) in &self.tiles {
            let n = tile.mapped();
            if key.0 == dim && n > 0 {
                chunks += u64::from(n);
                tiles += 1;
            }
        }
        (chunks, tiles)
    }

    /// Write one tile now and note it in the index.
    fn save(&mut self, key: Key, now: u64) -> bool {
        let (Some(path), Some(name)) = (self.path(key), self.dims.get(key.0 as usize).map(|d| d.name.clone())) else { return false };
        let Some(tile) = self.tiles.get_mut(&key) else { return false };
        let json = tile.to_json(&name, key.1, key.2, now);
        let chunks = tile.mapped();
        if write_atomic(&path, json.as_bytes()).is_err() {
            // Keep the changes and try again later, but not on every tick
            tile.dirty_since = now;
            self.failed += 1;
            return false;
        }
        tile.dirty = false;
        self.saved += 1;
        let dim = &mut self.dims[key.0 as usize];
        let file = tile_file(&dim.dir, key.1, key.2);
        dim.tiles.insert((key.1, key.2), TileMeta { x: key.1, z: key.2, file, updated: now, chunks, bytes: json.len() as u64 });
        self.index_dirty = true;
        true
    }

    /// One small piece of saving: compress one band of a tile that is due, or write a tile whose
    /// bands are ready, or write the index. `everything` = do not wait for `SAVE_AFTER_SECS`.
    /// Returns false when there was nothing to do.
    pub fn flush_step(&mut self, now: u64, everything: bool) -> bool {
        // With the memory full, save sooner: the next tile to be dropped should not have to be written first
        let wait = if self.tiles.len() >= self.max_tiles { SAVE_SOON_SECS } else { SAVE_AFTER_SECS };
        let due = self.tiles.iter().filter(|(_, t)| t.dirty && (everything || now.saturating_sub(t.dirty_since) >= wait)).min_by_key(|(_, t)| t.dirty_since).map(|(k, _)| *k);
        if let Some(key) = due {
            let tile = self.tiles.get_mut(&key).expect("key from the map");
            if tile.stale_bands() > 0 {
                tile.encode_band();
            } else {
                self.save(key, now);
            }
            return true;
        }
        if self.index_dirty {
            self.write_index(now);
            return true;
        }
        false
    }

    /// Write everything that has changes (shutdown).
    pub fn flush_all(&mut self, now: u64) {
        let keys: Vec<Key> = self.tiles.iter().filter(|(_, t)| t.dirty).map(|(k, _)| *k).collect();
        for key in keys {
            self.save(key, now);
        }
        if self.index_dirty {
            self.write_index(now);
        }
    }

    pub fn write_index(&mut self, now: u64) -> bool {
        let disk = IndexDisk {
            v: 1,
            plugin: self.plugin.clone(),
            updated: now,
            tile_chunks: CHUNKS as u32,
            tile_px: SIZE as u32,
            band_rows: BAND_ROWS as u32,
            palette: color::palette_hex(),
            dimensions: self
                .dims
                .iter()
                .map(|d| DimDisk { name: d.name.clone(), dir: d.dir.clone(), spawn: d.spawn, min_y: d.min_y, chunks: d.chunks(), bytes: d.bytes(), tiles: d.tiles.values().cloned().collect() })
                .collect(),
        };
        let ok = serde_json::to_vec(&disk).is_ok_and(|bytes| write_atomic(&self.root.join("index.json"), &bytes).is_ok());
        // A failed write is not retried every tick: the next saved tile asks again
        self.index_dirty = false;
        ok
    }

    /// Text of a tile file as it is on disk (for the export).
    pub fn read_file(&self, dim: u8, x: i32, z: i32) -> Option<String> {
        std::fs::read_to_string(self.path((dim, x, z))?).ok()
    }

    /// Bytes of all tile files.
    pub fn disk_bytes(&self) -> u64 {
        self.dims.iter().map(Dim::bytes).sum()
    }
}

/// Folder names are made by us from dimension names, but the index file can be edited by hand.
pub fn valid_dir(dir: &str) -> bool {
    !dir.is_empty() && dir.len() <= 40 && dir.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

pub fn tile_file(dir: &str, x: i32, z: i32) -> String {
    format!("tiles/{dir}/{x}_{z}.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("worldmap-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn chunks_land_in_the_right_tile() {
        assert_eq!(locate(0, 0), ((0, 0), (0, 0)));
        assert_eq!(locate(31, 32), ((0, 1), (31, 0)));
        assert_eq!(locate(-1, -32), ((-1, -1), (31, 0)));
        assert_eq!(locate(-33, 63), ((-2, 1), (31, 31)));
        assert_eq!(tile_file("overworld", -2, 1), "tiles/overworld/-2_1.json");
        assert!(valid_dir("the_nether") && !valid_dir("../x") && !valid_dir("") && !valid_dir("A"));
    }

    #[test]
    fn the_map_survives_a_restart() {
        let root = temp("restart");
        let mut s = Store::open(&root, "1.0.0", 8);
        let d = s.dim_id("minecraft:overworld", "overworld").unwrap();
        assert_eq!(s.dim_id("minecraft:overworld", "overworld"), Some(d));
        assert_eq!(s.dim_id("minecraft:other", "overworld"), None);
        assert_eq!(s.dim_id("minecraft:evil", "../evil"), None);
        s.set_world(d, [10, 70, -20], -64);
        assert_eq!(s.chunk_time(d, 5, 5, 100), 0);
        assert_eq!(s.in_memory(), 0, "asking about an unknown tile must not create it");
        // The new dimension and its spawn go to the index first
        assert!(s.flush_step(100, false));
        assert!(!s.flush_step(100, false));
        let ((tx, tz), (lx, lz)) = locate(-40, 3);
        assert!(s.tile((d, tx, tz), 100).set_chunk(lx, lz, &[35; 256], 100));
        assert_eq!(s.chunk_time(d, -40, 3, 100), 100);
        assert_eq!(s.dirty_tiles(), 1);
        assert_eq!(s.mapped(d), (1, 1), "a chunk counts before it is saved");
        // Not due yet
        assert!(!s.flush_step(100 + SAVE_AFTER_SECS - 1, false));
        // Due: one band, the file, the index, then nothing
        let later = 100 + SAVE_AFTER_SECS;
        assert!(s.flush_step(later, false));
        assert!(s.flush_step(later, false));
        assert_eq!(s.dirty_tiles(), 0);
        assert!(s.flush_step(later, false));
        assert!(!s.flush_step(later, false));
        assert!(root.join("tiles/overworld/-2_0.json").is_file());
        assert!(!root.join("tiles/overworld/-2_0.tmp").exists());
        assert_eq!(s.dims[0].chunks(), 1);
        assert!(s.disk_bytes() > 100);

        let mut s = Store::open(&root, "1.0.0", 8);
        assert_eq!(s.dims.len(), 1);
        assert_eq!(s.dims[0].spawn, [10, 70, -20]);
        assert_eq!(s.dims[0].chunks(), 1);
        let d = s.dim_id("minecraft:overworld", "overworld").unwrap();
        assert_eq!(s.chunk_time(d, -40, 3, 500), 100);
        assert_eq!(s.tile((d, tx, tz), 500).chunk_pixels(lx, lz), [35; 256]);
        let index: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("index.json")).unwrap()).unwrap();
        assert_eq!(index["tile_px"], 512);
        assert_eq!(index["palette"].as_str().unwrap().len(), 1536);
        assert_eq!(index["dimensions"][0]["tiles"][0]["file"], "tiles/overworld/-2_0.json");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn memory_is_bounded_and_evicted_tiles_are_saved() {
        let root = temp("lru");
        let mut s = Store::open(&root, "1.0.0", 3);
        let d = s.dim_id("minecraft:overworld", "overworld").unwrap();
        for tx in 0..10 {
            s.tile((d, tx, 0), 50).set_chunk(0, 0, &[(3 + tx) as u8; 256], 50);
            assert!(s.in_memory() <= 3);
        }
        assert_eq!(s.in_memory(), 3);
        // Tiles 0..=6 were pushed out, and written on the way
        for tx in 0..7 {
            assert!(root.join(format!("tiles/overworld/{tx}_0.json")).is_file(), "tile {tx}");
        }
        assert!(!root.join("tiles/overworld/9_0.json").exists());
        // Using a tile keeps it: touch 7, then load two more; 8 and 9 go, 7 stays
        s.tile((d, 7, 0), 60);
        s.tile((d, 20, 0), 60);
        s.tile((d, 21, 0), 60);
        assert!(root.join("tiles/overworld/8_0.json").is_file());
        assert!(root.join("tiles/overworld/9_0.json").is_file());
        assert!(!root.join("tiles/overworld/7_0.json").exists());
        assert_eq!(s.in_memory(), 3);
        // An evicted tile comes back from disk with its picture
        assert_eq!(s.tile((d, 2, 0), 70).pixel(0, 0), 5);
        assert_eq!(s.chunk_time(d, 2 * 32, 0, 70), 50);
        s.flush_all(80);
        assert!(root.join("tiles/overworld/7_0.json").is_file());
        assert_eq!(s.dirty_tiles(), 0);
        assert_eq!(s.dims[0].chunks(), 10);
        assert_eq!(s.mapped(d), (10, 10), "tiles on disk and in memory are counted once each");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_full_memory_saves_sooner() {
        let root = temp("soon");
        let mut s = Store::open(&root, "1.0.0", 2);
        let d = s.dim_id("minecraft:overworld", "overworld").unwrap();
        while s.flush_step(100, false) {}
        s.tile((d, 0, 0), 100).set_chunk(0, 0, &[9; 256], 100);
        assert!(!s.flush_step(100 + SAVE_SOON_SECS, false), "room left: no hurry");
        s.tile((d, 1, 0), 100).set_chunk(0, 0, &[9; 256], 100);
        assert!(s.flush_step(100 + SAVE_SOON_SECS, false), "full: save now");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_lost_index_is_rebuilt_from_the_tiles() {
        let root = temp("rebuild");
        let mut s = Store::open(&root, "1.0.0", 4);
        let d = s.dim_id("minecraft:the_nether", "the_nether").unwrap();
        s.tile((d, 1, -1), 10).set_chunk(3, 3, &[40; 256], 10);
        s.tile((d, 1, -1), 10).set_chunk(4, 3, &[41; 256], 12);
        s.flush_all(20);
        std::fs::remove_file(root.join("index.json")).unwrap();
        std::fs::write(root.join("tiles/the_nether/junk.json"), "{}").unwrap();
        std::fs::write(root.join("tiles/the_nether/5_5.json"), "not a tile").unwrap();
        let mut s = Store::open(&root, "1.0.0", 4);
        assert!(s.dims.is_empty());
        let d = s.dim_id("minecraft:the_nether", "the_nether").unwrap();
        assert_eq!(s.dims[0].tiles.len(), 1);
        assert_eq!(s.dims[0].chunks(), 2);
        assert_eq!(s.chunk_time(d, 32 + 4, -32 + 3, 30), 12);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_damaged_tile_file_starts_over_instead_of_failing() {
        let root = temp("damaged");
        let mut s = Store::open(&root, "1.0.0", 4);
        let d = s.dim_id("minecraft:overworld", "overworld").unwrap();
        s.tile((d, 0, 0), 10).set_chunk(0, 0, &[40; 256], 10);
        s.flush_all(20);
        std::fs::write(root.join("tiles/overworld/0_0.json"), "{\"v\":1}").unwrap();
        let mut s = Store::open(&root, "1.0.0", 4);
        let d = s.dim_id("minecraft:overworld", "overworld").unwrap();
        assert_eq!(s.chunk_time(d, 0, 0, 30), 0);
        assert!(s.read_file(d, 0, 0).is_some());
        assert!(s.read_file(d, 9, 9).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}

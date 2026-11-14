//! Posters on disk (no server needed).
//!
//! ```text
//! plugins/data/imagemap/
//!   index.json        {"next_id":100004,"posters":[{"name":"logo","owner":"alice","cols":2,"rows":2,
//!                      "maps":[100000,100001,100002,100003],"created":1790000000,"source":"https://…"}]}
//!   maps/<id>.bin     the 128 x 128 colour bytes of one map, row by row
//!   images/           picture files the owner uploaded, for `/imagemap file <name>`
//! ```
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cfg::valid_name;
use crate::picture::MAP;

pub const MAP_BYTES: usize = MAP * MAP;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Poster {
    pub name: String,
    /// Lowercase name of the player who made it; empty = the console.
    #[serde(default)]
    pub owner: String,
    pub cols: u32,
    pub rows: u32,
    /// Map numbers, row by row from the top left.
    pub maps: Vec<i32>,
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub source: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Index {
    next_id: i32,
    #[serde(default)]
    posters: Vec<Poster>,
}

pub struct Store {
    dir: PathBuf,
    next_id: i32,
    pub posters: Vec<Poster>,
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

impl Store {
    /// Posters whose shape or map list makes no sense are left out (a hand-edited index).
    pub fn open(dir: &Path, first_id: i32) -> Store {
        let index: Index = std::fs::read_to_string(dir.join("index.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let mut posters: Vec<Poster> = Vec::new();
        for p in index.posters {
            let ok = valid_name(&p.name) && p.cols >= 1 && p.rows >= 1 && p.cols <= 16 && p.rows <= 16 && p.maps.len() == (p.cols * p.rows) as usize && p.maps.iter().all(|id| *id >= 0) && !posters.iter().any(|x| x.name == p.name);
            if ok {
                posters.push(p);
            }
        }
        // Never hand out a number that is in use, whatever the index says
        let used = posters.iter().flat_map(|p| p.maps.iter().copied()).max().map_or(first_id, |m| m + 1);
        Store { dir: dir.to_path_buf(), next_id: index.next_id.max(first_id).max(used), posters }
    }

    pub fn save(&self) -> bool {
        let index = Index { next_id: self.next_id, posters: self.posters.clone() };
        serde_json::to_vec(&index).is_ok_and(|b| write_atomic(&self.dir.join("index.json"), &b).is_ok())
    }

    fn map_path(&self, id: i32) -> PathBuf {
        self.dir.join("maps").join(format!("{id}.bin"))
    }

    pub fn images_dir(&self) -> PathBuf {
        self.dir.join("images")
    }

    /// Numbers for the maps of a new poster.
    pub fn take_ids(&mut self, count: usize) -> Vec<i32> {
        let ids: Vec<i32> = (0..count as i32).map(|i| self.next_id + i).collect();
        self.next_id += count as i32;
        ids
    }

    pub fn write_map(&self, id: i32, bytes: &[u8]) -> bool {
        bytes.len() == MAP_BYTES && write_atomic(&self.map_path(id), bytes).is_ok()
    }

    pub fn read_map(&self, id: i32) -> Option<Vec<u8>> {
        std::fs::read(self.map_path(id)).ok().filter(|b| b.len() == MAP_BYTES)
    }

    pub fn find(&self, name: &str) -> Option<&Poster> {
        self.posters.iter().find(|p| p.name == name)
    }

    pub fn total_maps(&self) -> usize {
        self.posters.iter().map(|p| p.maps.len()).sum()
    }

    pub fn maps_of(&self, owner: &str) -> usize {
        self.posters.iter().filter(|p| p.owner == owner).map(|p| p.maps.len()).sum()
    }

    pub fn all_ids(&self) -> Vec<i32> {
        self.posters.iter().flat_map(|p| p.maps.iter().copied()).collect()
    }

    /// `wanted` if it is free, else `wanted-2`, `wanted-3`, …; with nothing wanted: `img1`, `img2`, …
    pub fn free_name(&self, wanted: Option<&str>) -> String {
        let wanted = wanted.filter(|w| valid_name(w));
        let base: String = wanted.map(|w| w.chars().take(28).collect()).unwrap_or_else(|| "img".into());
        if wanted.is_some() && self.find(&base).is_none() {
            return base;
        }
        (if wanted.is_some() { 2 } else { 1 }..).map(|n| if wanted.is_some() { format!("{base}-{n}") } else { format!("{base}{n}") }).find(|n| self.find(n).is_none()).unwrap_or(base)
    }

    pub fn add(&mut self, poster: Poster) -> bool {
        self.posters.push(poster);
        self.save()
    }

    /// Take a poster away, its map files too.
    pub fn remove(&mut self, name: &str) -> Option<Poster> {
        let at = self.posters.iter().position(|p| p.name == name)?;
        let poster = self.posters.remove(at);
        for id in &poster.maps {
            let _ = std::fs::remove_file(self.map_path(*id));
        }
        self.save();
        Some(poster)
    }

    pub fn rename(&mut self, old: &str, new: &str) -> bool {
        if !valid_name(new) || self.find(new).is_some() {
            return false;
        }
        match self.posters.iter_mut().find(|p| p.name == old) {
            Some(p) => {
                p.name = new.to_owned();
                self.save()
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("imagemap-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn posters_survive_a_restart() {
        let dir = temp("restart");
        let mut s = Store::open(&dir, 100_000);
        assert_eq!(s.free_name(None), "img1");
        let ids = s.take_ids(4);
        assert_eq!(ids, [100_000, 100_001, 100_002, 100_003]);
        for (i, id) in ids.iter().enumerate() {
            assert!(s.write_map(*id, &vec![(i as u8 + 1) * 4; MAP_BYTES]));
        }
        assert!(!s.write_map(5, &[1, 2, 3]), "a map is exactly 128 x 128 bytes");
        assert!(s.add(Poster { name: "logo".into(), owner: "alice".into(), cols: 2, rows: 2, maps: ids.clone(), created: 7, source: "https://x/y.png".into() }));
        let one = s.take_ids(1);
        s.write_map(one[0], &vec![40; MAP_BYTES]);
        s.add(Poster { name: "img1".into(), owner: String::new(), cols: 1, rows: 1, maps: one.clone(), created: 8, source: "file".into() });

        let mut back = Store::open(&dir, 100_000);
        assert_eq!(back.posters.len(), 2);
        assert_eq!((back.total_maps(), back.maps_of("alice"), back.maps_of("bob")), (5, 4, 0));
        assert_eq!(back.read_map(100_002).unwrap()[0], 12);
        assert_eq!(back.read_map(999), None);
        assert_eq!(back.all_ids(), [100_000, 100_001, 100_002, 100_003, 100_004]);
        // Numbers go on after the ones in use; names do not repeat
        assert_eq!(back.take_ids(2), [100_005, 100_006]);
        assert_eq!((back.free_name(None), back.free_name(Some("logo")), back.free_name(Some("new")), back.free_name(Some("Bad Name"))), ("img2".into(), "logo-2".into(), "new".into(), "img2".into()));
        assert!(back.rename("logo", "banner") && !back.rename("img1", "banner") && !back.rename("nope", "x") && !back.rename("img1", "Bad Name"));
        // Deleting takes the files along
        let gone = back.remove("banner").unwrap();
        assert_eq!(gone.maps.len(), 4);
        assert!(back.read_map(100_000).is_none() && back.read_map(100_004).is_some());
        assert!(back.remove("banner").is_none());
        assert_eq!(Store::open(&dir, 100_000).posters.len(), 1);
    }

    #[test]
    fn a_damaged_index_cannot_reuse_numbers() {
        let dir = temp("damaged");
        std::fs::write(
            dir.join("index.json"),
            r#"{"next_id":5,"posters":[
              {"name":"ok","cols":1,"rows":1,"maps":[200000]},
              {"name":"ok","cols":1,"rows":1,"maps":[200001]},
              {"name":"Bad Name","cols":1,"rows":1,"maps":[200002]},
              {"name":"shape","cols":2,"rows":2,"maps":[1,2,3]},
              {"name":"huge","cols":99,"rows":1,"maps":[]}]}"#,
        )
        .unwrap();
        let mut s = Store::open(&dir, 100_000);
        assert_eq!(s.posters.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["ok"]);
        assert_eq!(s.take_ids(1), [200_001]);
        std::fs::write(dir.join("index.json"), "{ nope").unwrap();
        assert_eq!(Store::open(&dir, 100_000).take_ids(1), [100_000]);
    }
}

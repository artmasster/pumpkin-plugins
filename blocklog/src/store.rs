//! The log on disk: append-only, written in batches, readable after a crash.
//!
//! ```text
//! users.tbl                       one user per line; the line number is the id in block records
//! states.tbl                      one block state per line (`oak_stairs[facing=north,half=bottom]`)
//! blocks/<world>/<date>/r.<rx>.<rz>.c1   28-byte block records of one region (512 x 512 blocks) and day
//! blocks/<world>/<date>/r.<rx>.<rz>.nbt  what was inside block entities broken there (chest contents, sign text)
//! events/<date>.log               chat, commands, sessions, signs, containers, kills, drops: one line each
//! ```
//!
//! Nothing is rewritten except the rolled-back bit of a block record. A lookup by radius opens only
//! the region files it covers and a lookup by time skips the days outside it; the directory names
//! are the index. Memory holds the two name tables and the unwritten tail, never the log.
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::record::{Action, BlockRec, REC, TextRec};

/// File extension of block files; names the record format.
pub const EXT: &str = "c1";
/// File extension of the block entity data that goes with a block file.
pub const NBT_EXT: &str = "nbt";
/// Largest block entity kept (a chest full of filled shulker boxes is far below this).
pub const MAX_NBT: usize = 1 << 20;
/// A region is 2^9 = 512 blocks wide (32 chunks), like the world's own region files.
pub const REGION_SHIFT: u32 = 9;
/// With more than this many records waiting after a failed write, the tail is dropped.
const MAX_BUFFERED: usize = 200_000;

/// Days since 1970-01-01 (UTC) of a unix time.
pub fn day_of(time: u32) -> u32 {
    time / 86_400
}

/// `2026-11-12` for a day number.
pub fn day_name(day: u32) -> String {
    // Civil date from a day count (Howard Hinnant's algorithm)
    let z = i64::from(day) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The day number of a `2026-11-12` name.
pub fn parse_day(name: &str) -> Option<u32> {
    let mut it = name.split('-');
    let (y, m, d): (i64, i64, i64) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    if it.next().is_some() || !(1970..=9999).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let day = u32::try_from(era * 146_097 + doe - 719_468).ok()?;
    // 02-31 parses to a day in March: only names that print back the same are ours
    (day_name(day) == name).then_some(day)
}

/// Names by number: users in one file, block states in another. Append-only.
pub struct Table {
    path: PathBuf,
    names: Vec<String>,
    index: HashMap<String, u32>,
    /// How many names are on disk.
    flushed: usize,
}

impl Table {
    /// Load the table. A last line without its newline is a torn write: it is cut off, and since
    /// tables are written before the records that use them, nothing on disk can refer to it.
    pub fn open(path: PathBuf) -> Table {
        let mut names = Vec::new();
        if let Ok(bytes) = fs::read(&path) {
            let whole = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
            if whole != bytes.len() {
                if let Ok(f) = OpenOptions::new().write(true).open(&path) {
                    let _ = f.set_len(whole as u64);
                }
            }
            names = String::from_utf8_lossy(&bytes[..whole]).lines().map(str::to_owned).collect();
        }
        let mut index = HashMap::with_capacity(names.len());
        for (i, name) in names.iter().enumerate() {
            // The first line of a name wins, like the lookup that wrote it
            index.entry(name.clone()).or_insert(i as u32);
        }
        let flushed = names.len();
        Table { path, names, index, flushed }
    }

    pub fn intern(&mut self, name: &str) -> u32 {
        if let Some(id) = self.index.get(name) {
            return *id;
        }
        let clean: String = name.chars().filter(|c| !c.is_control()).take(200).collect();
        if let Some(id) = self.index.get(&clean) {
            return *id;
        }
        let id = self.names.len() as u32;
        self.index.insert(clean.clone(), id);
        self.names.push(clean);
        id
    }

    /// The name, or `?` for a number this table never wrote.
    pub fn get(&self, id: u32) -> &str {
        self.names.get(id as usize).map_or("?", String::as_str)
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Ids of the names in `wanted` (lower case), compared without case.
    pub fn ids_of(&self, wanted: &[String]) -> Vec<u32> {
        self.names.iter().enumerate().filter(|(_, n)| wanted.contains(&n.to_ascii_lowercase())).map(|(i, _)| i as u32).collect()
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.flushed == self.names.len() {
            return Ok(());
        }
        let mut text = String::new();
        for name in &self.names[self.flushed..] {
            text.push_str(name);
            text.push('\n');
        }
        let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        f.write_all(text.as_bytes())?;
        // Block records written next refer to these lines
        let _ = f.sync_data();
        self.flushed = self.names.len();
        Ok(())
    }
}

/// One block file: a world, a day and a region.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FileKey {
    pub world: String,
    pub day: u32,
    pub rx: i32,
    pub rz: i32,
}

/// Block entity data by where and when its block was broken.
pub type NbtIndex = HashMap<(i32, i32, i32, u32), Vec<u8>>;

pub struct Store {
    root: PathBuf,
    pub users: Table,
    pub states: Table,
    blocks: HashMap<FileKey, Vec<u8>>,
    /// The file the last record went to: runs of records for one file skip building its key.
    last: Option<FileKey>,
    nbt: HashMap<FileKey, Vec<u8>>,
    text: BTreeMap<u32, String>,
    /// Records waiting for the next flush.
    pub buffered: usize,
    pub written_blocks: u64,
    pub written_text: u64,
    /// Records lost because the disk refused them.
    pub lost: u64,
    pub last_error: Option<String>,
}

impl Store {
    pub fn open(root: &Path) -> Store {
        let _ = fs::create_dir_all(root.join("blocks"));
        let _ = fs::create_dir_all(root.join("events"));
        Store {
            root: root.to_owned(),
            users: Table::open(root.join("users.tbl")),
            states: Table::open(root.join("states.tbl")),
            blocks: HashMap::new(),
            last: None,
            nbt: HashMap::new(),
            text: BTreeMap::new(),
            buffered: 0,
            written_blocks: 0,
            written_text: 0,
            lost: 0,
            last_error: None,
        }
    }

    /// Remember a block change. `world` is a world key; `old` and `new` are state strings.
    #[allow(clippy::too_many_arguments)]
    pub fn log_block(&mut self, world: &str, time: u32, user: &str, pos: (i32, i32, i32), action: Action, old: &str, new: &str) {
        let (user, old, new) = (self.users.intern(user), self.states.intern(old), self.states.intern(new));
        self.log_block_ids(world, time, user, pos, action, old, new);
    }

    /// The same with the table ids already known: the cheap way for a caller that logs many
    /// changes of one user (a world edit of 100,000 blocks).
    #[allow(clippy::too_many_arguments)]
    pub fn log_block_ids(&mut self, world: &str, time: u32, user: u32, pos: (i32, i32, i32), action: Action, old: u32, new: u32) {
        let rec = BlockRec { time, user, x: pos.0, y: pos.1.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16, z: pos.2, action, rolled: false, old, new };
        let (day, rx, rz) = (day_of(time), pos.0 >> REGION_SHIFT, pos.2 >> REGION_SHIFT);
        if !self.last.as_ref().is_some_and(|k| k.day == day && k.rx == rx && k.rz == rz && k.world == world) {
            self.last = Some(FileKey { world: world.to_owned(), day, rx, rz });
        }
        let Some(key) = &self.last else { return };
        match self.blocks.get_mut(key) {
            Some(buf) => buf.extend_from_slice(&rec.encode()),
            None => {
                self.blocks.insert(key.clone(), rec.encode().to_vec());
            }
        }
        self.buffered += 1;
    }

    /// Remember what was inside the block entity of a block that was just broken (the block record
    /// of the same place and second is its owner). A rollback of that record puts it back.
    pub fn log_nbt(&mut self, world: &str, time: u32, pos: (i32, i32, i32), nbt: &[u8]) {
        if nbt.is_empty() || nbt.len() > MAX_NBT {
            return;
        }
        let key = FileKey { world: world.to_owned(), day: day_of(time), rx: pos.0 >> REGION_SHIFT, rz: pos.2 >> REGION_SHIFT };
        self.nbt.entry(key).or_default().extend_from_slice(&nbt_entry(pos, time, nbt));
    }

    pub fn nbt_path(&self, key: &FileKey) -> PathBuf {
        self.block_dir(&key.world, key.day).join(format!("r.{}.{}.{NBT_EXT}", key.rx, key.rz))
    }

    pub fn log_text(&mut self, rec: &TextRec) {
        self.text.entry(day_of(rec.time)).or_default().push_str(&rec.encode());
        self.buffered += 1;
    }

    pub fn block_dir(&self, world: &str, day: u32) -> PathBuf {
        self.root.join("blocks").join(world).join(day_name(day))
    }

    pub fn block_path(&self, key: &FileKey) -> PathBuf {
        self.block_dir(&key.world, key.day).join(format!("r.{}.{}.{EXT}", key.rx, key.rz))
    }

    pub fn text_path(&self, day: u32) -> PathBuf {
        self.root.join("events").join(format!("{}.log", day_name(day)))
    }

    /// Write everything that waits. Tables go first: a record must never reach the disk before the
    /// names it points at. A file that cannot be written keeps its tail for the next try.
    pub fn flush(&mut self) -> io::Result<()> {
        let mut first_error = None;
        let mut note = |e: io::Error| {
            if first_error.is_none() {
                first_error = Some(e);
            }
        };
        let tables = self.users.flush().and_then(|()| self.states.flush());
        match tables {
            Err(e) => note(e),
            Ok(()) => {
                for (key, bytes) in std::mem::take(&mut self.blocks) {
                    match append_records(&self.block_path(&key), &bytes) {
                        Ok(()) => self.written_blocks += (bytes.len() / REC) as u64,
                        Err(e) => {
                            note(e);
                            self.blocks.insert(key, bytes);
                        }
                    }
                }
                for (key, bytes) in std::mem::take(&mut self.nbt) {
                    if let Err(e) = append_nbt(&self.nbt_path(&key), &bytes) {
                        note(e);
                        self.nbt.insert(key, bytes);
                    }
                }
                for (day, lines) in std::mem::take(&mut self.text) {
                    match append_text(&self.text_path(day), &lines) {
                        Ok(()) => self.written_text += lines.matches('\n').count() as u64,
                        Err(e) => {
                            note(e);
                            self.text.insert(day, lines);
                        }
                    }
                }
            }
        }
        self.buffered = self.blocks.values().map(|b| b.len() / REC).sum::<usize>() + self.text.values().map(|t| t.matches('\n').count()).sum::<usize>();
        match first_error {
            None => Ok(()),
            Some(e) => {
                self.last_error = Some(e.to_string());
                if self.buffered > MAX_BUFFERED {
                    // A full or broken disk must not eat the server's memory
                    self.lost += self.buffered as u64;
                    self.blocks.clear();
                    self.nbt.clear();
                    self.text.clear();
                    self.buffered = 0;
                }
                Err(e)
            }
        }
    }

    /// World keys that have block files.
    pub fn worlds(&self) -> Vec<String> {
        let mut out = dir_names(&self.root.join("blocks"), true);
        out.sort();
        out
    }

    /// Days of a world that have block files, oldest first.
    pub fn block_days(&self, world: &str) -> Vec<u32> {
        let mut out: Vec<u32> = dir_names(&self.root.join("blocks").join(world), true).iter().filter_map(|n| parse_day(n)).collect();
        out.sort_unstable();
        out
    }

    /// Days that have a text log, oldest first.
    pub fn text_days(&self) -> Vec<u32> {
        let mut out: Vec<u32> = dir_names(&self.root.join("events"), false).iter().filter_map(|n| parse_day(n.strip_suffix(".log")?)).collect();
        out.sort_unstable();
        out
    }

    /// Regions of a world that have a block file for that day.
    pub fn regions(&self, world: &str, day: u32) -> Vec<(i32, i32)> {
        let mut out: Vec<(i32, i32)> = dir_names(&self.block_dir(world, day), false).iter().filter_map(|n| parse_region(n)).collect();
        out.sort_unstable();
        out
    }
}

/// `r.<rx>.<rz>.c1`
fn parse_region(name: &str) -> Option<(i32, i32)> {
    let mut it = name.split('.');
    let ok = it.next()? == "r";
    let (rx, rz) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    (ok && it.next()? == EXT && it.next().is_none()).then_some((rx, rz))
}

fn dir_names(dir: &Path, dirs: bool) -> Vec<String> {
    let Ok(read) = fs::read_dir(dir) else { return Vec::new() };
    read.filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() == dirs))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect()
}

/// Add whole records to a block file. A torn record at the end (the server died while writing)
/// is cut off first, so the file stays a row of whole records.
fn append_records(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    let len = f.metadata()?.len();
    let torn = (len % REC as u64) as usize;
    if torn != 0 && f.set_len(len - torn as u64).is_err() {
        // Cannot cut: fill the torn record up instead. Readers skip it (its check byte is wrong).
        f.write_all(&vec![0u8; REC - torn])?;
    }
    f.write_all(bytes)
}

/// One entry of an `.nbt` file: length, place, time, the data, a check of all that.
fn nbt_entry(pos: (i32, i32, i32), time: u32, nbt: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(24 + nbt.len());
    out.extend_from_slice(&(nbt.len() as u32).to_le_bytes());
    for v in [pos.0, pos.1, pos.2] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&time.to_le_bytes());
    out.extend_from_slice(nbt);
    let sum = fnv(&out);
    out.extend_from_slice(&sum.to_le_bytes());
    out
}

fn fnv(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5u32, |h, b| (h ^ u32::from(*b)).wrapping_mul(0x0100_0193))
}

/// The whole entries at the start of an `.nbt` file and how many bytes they take. Reading stops at
/// the first entry that is cut short or fails its check: a torn write only ever damages the end.
fn parse_nbt(bytes: &[u8]) -> (NbtIndex, usize) {
    let mut index = NbtIndex::new();
    let mut at = 0;
    while let Some(head) = bytes.get(at..at + 20) {
        let word = |i: usize| u32::from_le_bytes([head[i], head[i + 1], head[i + 2], head[i + 3]]);
        let len = word(0) as usize;
        let end = at + 20 + len;
        let (Some(body), Some(sum)) = (bytes.get(at..end), bytes.get(end..end + 4)) else { break };
        if len > MAX_NBT || fnv(body) != u32::from_le_bytes([sum[0], sum[1], sum[2], sum[3]]) {
            break;
        }
        index.insert((word(4) as i32, word(8) as i32, word(12) as i32, word(16)), body[20..].to_vec());
        at = end + 4;
    }
    (index, at)
}

/// The block entity data kept next to a block file; empty when there is none.
pub fn read_nbt(path: &Path) -> NbtIndex {
    match fs::metadata(path) {
        // A file this large is not ours to load into memory in one go
        Ok(meta) if meta.len() <= 64 * 1024 * 1024 => fs::read(path).map(|bytes| parse_nbt(&bytes).0).unwrap_or_default(),
        _ => NbtIndex::new(),
    }
}

/// Add entries to an `.nbt` file, after cutting off a torn entry at its end.
fn append_nbt(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    if let Ok(old) = fs::read(path) {
        let whole = parse_nbt(&old).1;
        if whole != old.len() {
            OpenOptions::new().write(true).open(path)?.set_len(whole as u64)?;
        }
    }
    OpenOptions::new().create(true).append(true).open(path)?.write_all(bytes)
}

/// Add whole lines to a text log, starting on a new line when the file ends in a torn one.
fn append_text(path: &Path, lines: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut torn = false;
    if let Ok(mut f) = File::open(path) {
        if f.metadata()?.len() > 0 {
            let mut last = [0u8; 1];
            f.seek(SeekFrom::End(-1))?;
            f.read_exact(&mut last)?;
            torn = last[0] != b'\n';
        }
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    if torn {
        f.write_all(b"\n")?;
    }
    f.write_all(lines.as_bytes())
}

/// A block file open for reading (and for flipping rolled-back bits).
pub struct BlockFile {
    file: File,
    /// Whole records in the file when it was opened.
    pub records: u64,
}

impl BlockFile {
    pub fn open(path: &Path, write: bool) -> Option<BlockFile> {
        let file = OpenOptions::new().read(true).write(write).open(path).ok()?;
        let records = file.metadata().ok()?.len() / REC as u64;
        Some(BlockFile { file, records })
    }

    /// Read `count` records starting at record `first` into `buf`.
    pub fn read(&mut self, first: u64, count: usize, buf: &mut Vec<u8>) -> io::Result<()> {
        buf.resize(count * REC, 0);
        self.file.seek(SeekFrom::Start(first * REC as u64))?;
        self.file.read_exact(buf)
    }

    /// Write records back where they came from. Only rolled-back bits differ from what is there,
    /// so a write that stops half way leaves every record either as it was or as it should be.
    pub fn write(&mut self, first: u64, bytes: &[u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(first * REC as u64))?;
        self.file.write_all(bytes)
    }
}

#[cfg(test)]
pub(crate) mod testdir {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT: AtomicU32 = AtomicU32::new(0);

    /// A fresh folder under the system temp dir, removed when dropped.
    pub struct TestDir(pub PathBuf);

    impl TestDir {
        pub fn new(name: &str) -> TestDir {
            let dir = std::env::temp_dir().join(format!("blocklog-test-{}-{name}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TestDir(dir)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testdir::TestDir;
    use super::*;

    const T0: u32 = 1_794_000_000;

    #[test]
    fn day_names() {
        assert_eq!(day_name(0), "1970-01-01");
        assert_eq!(day_name(day_of(1_794_441_600)), "2026-11-12");
        assert_eq!(day_name(19_782), "2024-02-29");
        for day in [0, 1, 58, 59, 60, 365, 11_016, 19_782, 20_769, 47_482] {
            assert_eq!(parse_day(&day_name(day)), Some(day));
        }
        for bad in ["", "2026-11", "2026-13-01", "2026-02-31", "x-1-1", "2026-11-12-1", "../..", "20769"] {
            assert_eq!(parse_day(bad), None, "{bad}");
        }
    }

    #[test]
    fn region_names() {
        assert_eq!(parse_region("r.0.-3.c1"), Some((0, -3)));
        assert_eq!(parse_region("r.0.-3.c2"), None);
        assert_eq!(parse_region("r.0.c1"), None);
        assert_eq!(parse_region("x.0.0.c1"), None);
        assert_eq!(parse_region("r.0.0.c1.tmp"), None);
    }

    #[test]
    fn tables_keep_their_numbers() {
        let dir = TestDir::new("table");
        let path = dir.0.join("users.tbl");
        let mut t = Table::open(path.clone());
        assert_eq!((t.intern("Alice"), t.intern("#fire"), t.intern("Alice"), t.intern("Bob")), (0, 1, 0, 2));
        t.flush().unwrap();
        let mut t = Table::open(path.clone());
        assert_eq!((t.get(0), t.get(1), t.get(2), t.get(9)), ("Alice", "#fire", "Bob", "?"));
        assert_eq!(t.intern("Bob"), 2);
        assert_eq!(t.intern("Carol"), 3);
        assert_eq!(t.ids_of(&["alice".into(), "carol".into()]), [0, 3]);
        // A name with a line break cannot shift the numbers of the names after it
        assert_eq!(t.intern("Da\nve"), 4);
        t.flush().unwrap();
        let t = Table::open(path);
        assert_eq!((t.len(), t.get(4)), (5, "Dave"));
    }

    #[test]
    fn torn_table_line_is_dropped() {
        let dir = TestDir::new("torn-table");
        let path = dir.0.join("states.tbl");
        fs::write(&path, b"air\nstone\noak_st").unwrap();
        let mut t = Table::open(path.clone());
        assert_eq!(t.len(), 2);
        assert_eq!(fs::read(&path).unwrap(), b"air\nstone\n");
        assert_eq!(t.intern("dirt"), 2);
        t.flush().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"air\nstone\ndirt\n");
    }

    #[test]
    fn records_land_in_region_and_day_files() {
        let dir = TestDir::new("layout");
        let mut s = Store::open(&dir.0);
        s.log_block("overworld", T0, "Alice", (5, 64, 5), Action::Place, "air", "stone");
        s.log_block("overworld", T0 + 1, "Alice", (-1, 64, 600), Action::Break, "stone", "air");
        s.log_block("the_nether", T0 + 86_400, "Bob", (5, 64, 5), Action::Place, "air", "netherrack");
        s.log_text(&TextRec { time: T0, action: Action::Chat, user: "Alice".into(), world: "overworld".into(), x: 0, y: 0, z: 0, amount: 0, text: "hi".into() });
        assert_eq!(s.buffered, 4);
        s.flush().unwrap();
        assert_eq!((s.buffered, s.written_blocks, s.written_text), (0, 3, 1));
        assert_eq!(s.worlds(), ["overworld", "the_nether"]);
        let day = day_of(T0);
        assert_eq!(s.block_days("overworld"), [day]);
        assert_eq!(s.block_days("the_nether"), [day + 1]);
        assert_eq!(s.regions("overworld", day), [(-1, 1), (0, 0)]);
        assert_eq!(s.text_days(), [day]);
        let path = s.block_path(&FileKey { world: "overworld".into(), day, rx: 0, rz: 0 });
        assert!(path.ends_with(format!("blocks/overworld/{}/r.0.0.c1", day_name(day))));
        assert_eq!(fs::metadata(path).unwrap().len(), REC as u64);
    }

    #[test]
    fn torn_block_file_is_repaired_on_the_next_write() {
        let dir = TestDir::new("torn-blocks");
        let mut s = Store::open(&dir.0);
        for i in 0..3 {
            s.log_block("overworld", T0 + i, "Alice", (i as i32, 64, 0), Action::Place, "air", "stone");
        }
        s.flush().unwrap();
        let path = s.block_path(&FileKey { world: "overworld".into(), day: day_of(T0), rx: 0, rz: 0 });
        // The server died 13 bytes into the fourth record
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[0x5A; 13]).unwrap();
        drop(f);

        // What was whole before is still read
        let mut file = BlockFile::open(&path, false).unwrap();
        assert_eq!(file.records, 3);
        let mut buf = Vec::new();
        file.read(0, 3, &mut buf).unwrap();
        for i in 0..3 {
            assert_eq!(BlockRec::decode(&buf[i * REC..(i + 1) * REC]).unwrap().x, i as i32);
        }

        // The next write starts on a record boundary
        let mut s = Store::open(&dir.0);
        s.log_block("overworld", T0 + 9, "Bob", (9, 64, 0), Action::Break, "stone", "air");
        s.flush().unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), 4 * REC as u64);
        let mut file = BlockFile::open(&path, false).unwrap();
        file.read(0, 4, &mut buf).unwrap();
        let last = BlockRec::decode(&buf[3 * REC..]).unwrap();
        assert_eq!((last.x, s.users.get(last.user), last.action), (9, "Bob", Action::Break));
    }

    #[test]
    fn torn_text_line_does_not_swallow_the_next_one() {
        let dir = TestDir::new("torn-text");
        let mut s = Store::open(&dir.0);
        let rec = |text: &str| TextRec { time: T0, action: Action::Chat, user: "Alice".into(), world: "overworld".into(), x: 0, y: 0, z: 0, amount: 0, text: text.into() };
        s.log_text(&rec("one"));
        s.flush().unwrap();
        let path = s.text_path(day_of(T0));
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"1794000000\tchat\tAli").unwrap();
        drop(f);
        s.log_text(&rec("two"));
        s.flush().unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let good: Vec<TextRec> = text.lines().filter_map(TextRec::decode).collect();
        assert_eq!(good.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["one", "two"]);
        assert_eq!(text.lines().count(), 3);
    }

    #[test]
    fn block_entity_data_is_kept_and_survives_a_torn_write() {
        let dir = TestDir::new("nbt");
        let mut s = Store::open(&dir.0);
        s.log_nbt("overworld", T0, (5, 64, 5), b"chest with diamonds");
        s.log_nbt("overworld", T0 + 1, (5, 64, 5), b"the same place, a second later");
        s.log_nbt("overworld", T0, (600, 64, 5), b"another region");
        s.log_nbt("overworld", T0, (6, 64, 5), b"");
        s.flush().unwrap();
        let key = FileKey { world: "overworld".into(), day: day_of(T0), rx: 0, rz: 0 };
        let path = s.nbt_path(&key);
        let index = read_nbt(&path);
        assert_eq!(index.len(), 2);
        assert_eq!(index[&(5, 64, 5, T0)], b"chest with diamonds");
        assert_eq!(index[&(5, 64, 5, T0 + 1)], b"the same place, a second later");
        assert_eq!(read_nbt(&s.nbt_path(&FileKey { rx: 1, ..key.clone() })).len(), 1);
        // Block files and their listing do not see the new files
        assert!(s.regions("overworld", day_of(T0)).is_empty());

        // The server dies in the middle of the third entry
        let whole = fs::metadata(&path).unwrap().len();
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&nbt_entry((7, 64, 5), T0, b"torn")[..15]).unwrap();
        drop(f);
        assert_eq!(read_nbt(&path).len(), 2);
        // The next write cuts the torn entry off, so what follows it can be read
        let mut s = Store::open(&dir.0);
        s.log_nbt("overworld", T0 + 2, (8, 64, 5), b"after the crash");
        s.flush().unwrap();
        let index = read_nbt(&path);
        assert_eq!(index.len(), 3);
        assert_eq!(index[&(8, 64, 5, T0 + 2)], b"after the crash");
        assert_eq!(fs::metadata(&path).unwrap().len(), whole + nbt_entry((8, 64, 5), T0 + 2, b"after the crash").len() as u64);
        // A flipped bit in the data is not handed out as a chest's contents
        let mut bytes = fs::read(&path).unwrap();
        bytes[25] ^= 1;
        fs::write(&path, &bytes).unwrap();
        assert_eq!(read_nbt(&path).len(), 0);
        assert_eq!(read_nbt(&dir.0.join("missing.nbt")).len(), 0);
    }

    #[test]
    fn ids_and_names_log_the_same_record() {
        let dir = TestDir::new("ids");
        let mut s = Store::open(&dir.0);
        s.log_block("overworld", T0, "Alice", (5, 64, 5), Action::Place, "air", "stone");
        let (user, air, stone) = (s.users.intern("Alice"), s.states.intern("air"), s.states.intern("stone"));
        // A run in one file, a jump to another region and world, and back
        s.log_block_ids("overworld", T0, user, (5, 64, 5), Action::Place, air, stone);
        s.log_block_ids("overworld", T0, user, (600, 64, 5), Action::Place, air, stone);
        s.log_block_ids("the_nether", T0, user, (5, 64, 5), Action::Place, air, stone);
        s.log_block_ids("overworld", T0, user, (6, 64, 5), Action::Break, stone, air);
        assert_eq!(s.buffered, 5);
        s.flush().unwrap();
        let path = s.block_path(&FileKey { world: "overworld".into(), day: day_of(T0), rx: 0, rz: 0 });
        let mut file = BlockFile::open(&path, false).unwrap();
        assert_eq!(file.records, 3);
        let mut buf = Vec::new();
        file.read(0, 3, &mut buf).unwrap();
        assert_eq!(buf[..REC], buf[REC..2 * REC]);
        assert_eq!(BlockRec::decode(&buf[2 * REC..]).unwrap().action, Action::Break);
        assert_eq!(s.regions("overworld", day_of(T0)), [(0, 0), (1, 0)]);
        assert_eq!(s.regions("the_nether", day_of(T0)), [(0, 0)]);
    }

    #[test]
    fn a_failed_write_keeps_the_tail() {
        let dir = TestDir::new("fail");
        let mut s = Store::open(&dir.0);
        // A file where the world folder should be: the write cannot succeed
        fs::write(dir.0.join("blocks").join("overworld"), b"x").unwrap();
        s.log_block("overworld", T0, "Alice", (0, 64, 0), Action::Place, "air", "stone");
        assert!(s.flush().is_err());
        assert_eq!((s.buffered, s.written_blocks), (1, 0));
        assert!(s.last_error.is_some());
        fs::remove_file(dir.0.join("blocks").join("overworld")).unwrap();
        s.flush().unwrap();
        assert_eq!((s.buffered, s.written_blocks), (0, 1));
    }
}

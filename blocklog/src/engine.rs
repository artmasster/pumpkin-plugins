//! Lookups, rollbacks, restores and purges as jobs that run a little at a time.
//!
//! A job walks the days it needs (newest first, oldest first for a restore), and inside a day the
//! region files and the text log. It never holds more than one chunk of one file plus the rows it
//! was asked for, and it can stop after any record: the caller decides how long a slice lasts.
use std::collections::{BTreeSet, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::query::{Matcher, Query, Row};
use crate::record::{Action, BlockRec, REC, TextRec, set_rolled};
use crate::store::{BlockFile, EXT, FileKey, NBT_EXT, NbtIndex, Store, read_nbt};

/// Records of a block file held in memory at a time (56 KiB).
const CHUNK: usize = 2048;
/// Longest line read from a text log; ours are far shorter, a longer one is damage.
const MAX_LINE: u64 = 8192;
/// Distinct chunks remembered for the "not loaded" report.
const MAX_SKIPPED_CHUNKS: usize = 4096;
/// More region files than this for one day: list the folder instead of probing each name.
const MAX_PROBES: i64 = 1024;
/// Container records of one day a rollback holds at a time; more are left for a repeat.
const MAX_FOUND: usize = 50_000;

/// What became of a change to a container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stored {
    Done,
    /// No container stands there (any more).
    NoContainer,
    /// The items do not fit.
    NoRoom,
}

/// The world as a rollback needs it. The plugin gives the real one, the tests a map.
pub trait WorldEdit {
    fn chunk_loaded(&mut self, world: &str, cx: i32, cz: i32) -> bool;
    /// Put the block state (`oak_stairs[facing=north]`) there; false = this server does not know it.
    /// `nbt` is what was inside the block when it was broken (chest contents, sign text).
    fn set_block(&mut self, world: &str, pos: (i32, i32, i32), state: &str, nbt: Option<&[u8]>) -> bool;
    /// Add (`count > 0`) or take out (`count < 0`) items of the container there. `item` is the
    /// text of a container record. Taking out takes what is there, up to the count.
    fn change_container(&mut self, world: &str, pos: (i32, i32, i32), item: &str, count: i64) -> Stored;
}

/// For jobs that only read.
pub struct ReadOnly;

impl WorldEdit for ReadOnly {
    fn chunk_loaded(&mut self, _: &str, _: i32, _: i32) -> bool {
        false
    }

    fn set_block(&mut self, _: &str, _: (i32, i32, i32), _: &str, _: Option<&[u8]>) -> bool {
        false
    }

    fn change_container(&mut self, _: &str, _: (i32, i32, i32), _: &str, _: i64) -> Stored {
        Stored::NoContainer
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Collect the newest `cap` rows.
    Lookup { cap: usize },
    /// Only count.
    Count,
    /// Put back what was there before every matching change that is not rolled back yet.
    Rollback,
    /// Put back what the change made, for every matching change that is rolled back.
    Restore,
}

impl Mode {
    fn writes(self) -> bool {
        matches!(self, Mode::Rollback | Mode::Restore)
    }
}

#[derive(Debug, Default)]
pub struct Outcome {
    /// Newest first.
    pub rows: Vec<Row>,
    /// There were more rows than the cap.
    pub truncated: bool,
    /// Records that matched (lookup, count) or were applied (rollback, restore).
    pub matched: u64,
    /// Records skipped because their chunk is not loaded, and how many chunks that is.
    pub skipped: u64,
    pub skipped_chunks: usize,
    /// Records whose block state this server does not know, or whose container is gone or full.
    pub failed: u64,
    /// Container records among `matched` (rollback, restore).
    pub items: u64,
    /// Records read.
    pub scanned: u64,
}

enum Unit {
    Text(PathBuf),
    Blocks(String, PathBuf),
}

struct BlockCur {
    world: String,
    file: BlockFile,
    asc: bool,
    /// Records not read yet: `lo..hi`.
    lo: u64,
    hi: u64,
    buf: Vec<u8>,
    buf_first: u64,
    /// Records of `buf` not handed out yet.
    left: usize,
    /// First and last record of `buf` whose rolled-back bit changed.
    dirty: Option<(usize, usize)>,
    /// What was inside the block entities broken in this file (loaded for a rollback only).
    nbt: NbtIndex,
}

impl BlockCur {
    fn open(world: String, path: &Path, mode: Mode) -> Option<BlockCur> {
        let file = BlockFile::open(path, mode.writes())?;
        let hi = file.records;
        let nbt = if mode == Mode::Rollback { read_nbt(&path.with_extension(NBT_EXT)) } else { NbtIndex::new() };
        Some(BlockCur { world, file, asc: mode == Mode::Restore, lo: 0, hi, buf: Vec::new(), buf_first: 0, left: 0, dirty: None, nbt })
    }

    fn save(&mut self) {
        if let Some((a, b)) = self.dirty.take() {
            let _ = self.file.write(self.buf_first + a as u64, &self.buf[a * REC..(b + 1) * REC]);
        }
    }

    /// Index in `buf` of the next record, newest first unless `asc`.
    fn next(&mut self) -> Option<usize> {
        if self.left == 0 {
            self.save();
            let n = (self.hi - self.lo).min(CHUNK as u64) as usize;
            if n == 0 {
                return None;
            }
            self.buf_first = if self.asc { self.lo } else { self.hi - n as u64 };
            if self.file.read(self.buf_first, n, &mut self.buf).is_err() {
                self.lo = self.hi;
                return None;
            }
            if self.asc {
                self.lo += n as u64;
            } else {
                self.hi -= n as u64;
            }
            self.left = n;
        }
        self.left -= 1;
        let n = self.buf.len() / REC;
        Some(if self.asc { n - 1 - self.left } else { self.left })
    }

    fn mark(&mut self, i: usize, rolled: bool) {
        set_rolled(&mut self.buf[i * REC..(i + 1) * REC], rolled);
        self.dirty = Some(match self.dirty {
            Some((a, b)) => (a.min(i), b.max(i)),
            None => (i, i),
        });
    }
}

impl Drop for BlockCur {
    fn drop(&mut self) {
        self.save();
    }
}

struct TextCur {
    reader: BufReader<File>,
    path: PathBuf,
    line: u64,
    /// Bytes read so far: where the next line starts.
    offset: u64,
    /// Rollback and restore: the container records to change, with the place of their mark, in
    /// the order they are to be taken from the end.
    found: Vec<(u64, TextRec)>,
    collected: bool,
    marker: Option<File>,
}

impl TextCur {
    /// Flip the rolled-back mark of a record in the file.
    fn mark(&mut self, at: u64, rolled: bool) -> bool {
        if self.marker.is_none() {
            self.marker = OpenOptions::new().write(true).open(&self.path).ok();
        }
        let Some(f) = self.marker.as_mut() else { return false };
        f.seek(SeekFrom::Start(at)).is_ok() && f.write_all(if rolled { b"C" } else { b"c" }).is_ok()
    }
}

enum Cur {
    Blocks(BlockCur),
    Text(TextCur),
}

pub struct Job {
    pub mode: Mode,
    matcher: Matcher,
    days: Vec<u32>,
    day_at: usize,
    units: Vec<Unit>,
    cur: Option<Cur>,
    day_rows: Vec<Row>,
    skipped_chunks: HashSet<(i32, i32)>,
    /// The chunk of the last record and whether it is loaded.
    chunk: Option<(i32, i32, bool)>,
    pub out: Outcome,
    pub done: bool,
}

impl Job {
    /// Plan the job. Flush the store first: only what is on disk is seen.
    pub fn new(store: &Store, mode: Mode, query: Query) -> Job {
        let worlds = Self::worlds(store, &query);
        let mut days = BTreeSet::new();
        if query.actions.any_block() {
            for world in &worlds {
                days.extend(store.block_days(world));
            }
        }
        if Self::reads_text(mode, &query) {
            days.extend(store.text_days());
        }
        let (first, last) = (query.since / 86_400, query.until / 86_400);
        let mut days: Vec<u32> = days.into_iter().filter(|d| *d >= first && *d <= last).collect();
        if mode != Mode::Restore {
            days.reverse();
        }
        let mut job = Job {
            mode,
            matcher: Matcher::new(query, store),
            days,
            day_at: 0,
            units: Vec::new(),
            cur: None,
            day_rows: Vec::new(),
            skipped_chunks: HashSet::new(),
            chunk: None,
            out: Outcome::default(),
            done: false,
        };
        job.plan_day(store);
        job
    }

    /// Lookups read every kind of text record; a rollback only what it can put back: container records.
    fn reads_text(mode: Mode, query: &Query) -> bool {
        if mode.writes() { query.actions.has(Action::ContainerAdd) || query.actions.has(Action::ContainerRemove) } else { query.actions.any_text() }
    }

    fn worlds(store: &Store, query: &Query) -> Vec<String> {
        match query.area.world() {
            Some(w) => vec![w.to_owned()],
            None => store.worlds(),
        }
    }

    /// Queue the files of the current day, or finish when no day is left.
    fn plan_day(&mut self, store: &Store) {
        let Some(day) = self.days.get(self.day_at).copied() else {
            self.done = true;
            return;
        };
        let q = &self.matcher.query;
        // Units are taken from the end. A rollback puts the blocks back first (a chest must stand
        // before its items return); a restore, which replays, changes the containers first.
        let text = Self::reads_text(self.mode, q);
        if text && self.mode != Mode::Restore {
            self.units.push(Unit::Text(store.text_path(day)));
        }
        if q.actions.any_block() {
            for world in Self::worlds(store, q) {
                let regions = match q.area.regions() {
                    Some(((x0, x1), (z0, z1))) if (i64::from(x1) - i64::from(x0) + 1) * (i64::from(z1) - i64::from(z0) + 1) <= MAX_PROBES => {
                        (x0..=x1).flat_map(|rx| (z0..=z1).map(move |rz| (rx, rz))).collect()
                    }
                    Some(((x0, x1), (z0, z1))) => store.regions(&world, day).into_iter().filter(|(rx, rz)| (x0..=x1).contains(rx) && (z0..=z1).contains(rz)).collect(),
                    None => store.regions(&world, day),
                };
                for (rx, rz) in regions {
                    let path = store.block_path(&FileKey { world: world.clone(), day, rx, rz });
                    self.units.push(Unit::Blocks(world.clone(), path));
                }
            }
        }
        if text && self.mode == Mode::Restore {
            self.units.push(Unit::Text(store.text_path(day)));
        }
    }

    /// The day is read: its rows join the result, newest first.
    fn end_day(&mut self, store: &Store) {
        if let Mode::Lookup { cap } = self.mode {
            self.day_rows.sort_by(|a, b| (b.time, b.ord).cmp(&(a.time, a.ord)));
            self.out.rows.append(&mut self.day_rows);
            if self.out.rows.len() >= cap {
                if self.out.rows.len() > cap || self.day_at + 1 < self.days.len() {
                    self.out.truncated = true;
                }
                self.out.rows.truncate(cap);
                self.done = true;
                return;
            }
        }
        self.day_at += 1;
        self.plan_day(store);
    }

    fn keep(&mut self, row: Row) {
        self.out.matched += 1;
        let Mode::Lookup { cap } = self.mode else { return };
        self.day_rows.push(row);
        // Only the newest rows can make it: do not hold a whole busy day in memory
        let need = cap.saturating_sub(self.out.rows.len()).max(1);
        if self.day_rows.len() > need * 2 + 64 {
            self.day_rows.sort_by(|a, b| (b.time, b.ord).cmp(&(a.time, a.ord)));
            self.day_rows.truncate(need);
            self.out.truncated = true;
        }
    }

    fn open_unit(&mut self, unit: Unit) {
        self.cur = match unit {
            Unit::Blocks(world, path) => BlockCur::open(world, &path, self.mode).map(Cur::Blocks),
            Unit::Text(path) => File::open(&path).ok().map(|f| {
                Cur::Text(TextCur { reader: BufReader::with_capacity(64 * 1024, f), path, line: 0, offset: 0, found: Vec::new(), collected: false, marker: None })
            }),
        };
    }

    /// One whole line without its newline, `None` at the end; an overlong line comes back empty.
    fn read_line(cur: &mut TextCur) -> Option<String> {
        let mut bytes = Vec::new();
        let mut overlong = false;
        loop {
            let n = (&mut cur.reader).take(MAX_LINE).read_until(b'\n', &mut bytes).ok()?;
            if n == 0 {
                return None;
            }
            cur.offset += n as u64;
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
                break;
            }
            if (n as u64) < MAX_LINE {
                // The file ends without a newline: a torn last line
                return None;
            }
            overlong = true;
            bytes.clear();
        }
        cur.line += 1;
        Some(if overlong { String::new() } else { String::from_utf8_lossy(&bytes).into_owned() })
    }

    fn loaded<W: WorldEdit>(&mut self, world: &mut W, name: &str, cx: i32, cz: i32) -> bool {
        match self.chunk {
            Some((x, z, loaded)) if (x, z) == (cx, cz) => loaded,
            _ => {
                let loaded = world.chunk_loaded(name, cx, cz);
                self.chunk = Some((cx, cz, loaded));
                loaded
            }
        }
    }

    /// Work until `max_scan` records were read or `max_apply` blocks were set. True when finished.
    pub fn step<W: WorldEdit>(&mut self, store: &Store, world: &mut W, max_scan: usize, max_apply: usize) -> bool {
        let (mut scanned, mut applied) = (0, 0);
        while !self.done && scanned < max_scan && applied < max_apply {
            let Some(cur) = self.cur.as_mut() else {
                match self.units.pop() {
                    Some(unit) => {
                        // Opening a file is work too: a day of a thousand regions must not be one slice
                        scanned += 8;
                        self.chunk = None;
                        self.open_unit(unit);
                    }
                    None => self.end_day(store),
                }
                continue;
            };
            scanned += 1;
            match cur {
                Cur::Text(text) if self.mode.writes() => {
                    let rollback = self.mode == Mode::Rollback;
                    if !text.collected {
                        // First the whole day: which container records are to change
                        let start = text.offset;
                        match Self::read_line(text) {
                            Some(line) => {
                                self.out.scanned += 1;
                                let Some((rec, rolled)) = TextRec::decode_marked(&line) else { continue };
                                if rec.action.is_container() && rolled != rollback && self.matcher.text(&rec) {
                                    match TextRec::mark_offset(&line) {
                                        Some(at) if text.found.len() < MAX_FOUND => text.found.push((start + at as u64, rec)),
                                        _ => self.out.truncated = true,
                                    }
                                }
                            }
                            None => {
                                text.collected = true;
                                // A rollback undoes the newest first, a restore replays the oldest first
                                if !rollback {
                                    text.found.reverse();
                                }
                            }
                        }
                        continue;
                    }
                    let Some((at, rec)) = text.found.pop() else {
                        self.cur = None;
                        continue;
                    };
                    let (cx, cz) = (rec.x >> 4, rec.z >> 4);
                    if !self.loaded(world, &rec.world, cx, cz) {
                        self.out.skipped += 1;
                        if self.skipped_chunks.len() < MAX_SKIPPED_CHUNKS {
                            self.skipped_chunks.insert((cx, cz));
                        }
                        self.out.skipped_chunks = self.skipped_chunks.len();
                        continue;
                    }
                    applied += 1;
                    // Undoing "put in" takes out, undoing "took out" puts back; a restore the other way
                    let put_in = (rec.action == Action::ContainerRemove) == rollback;
                    let count = i64::from(rec.amount) * if put_in { 1 } else { -1 };
                    let done = world.change_container(&rec.world, (rec.x, rec.y, rec.z), &rec.text, count) == Stored::Done;
                    let marked = done
                        && match self.cur.as_mut() {
                            Some(Cur::Text(text)) => text.mark(at, rollback),
                            _ => false,
                        };
                    if marked {
                        self.out.matched += 1;
                        self.out.items += 1;
                    } else {
                        self.out.failed += 1;
                    }
                }
                Cur::Text(text) => {
                    let Some(line) = Self::read_line(text) else {
                        self.cur = None;
                        continue;
                    };
                    self.out.scanned += 1;
                    let ord = text.line;
                    if let Some((rec, rolled)) = TextRec::decode_marked(&line).filter(|(r, _)| self.matcher.text(r)) {
                        self.keep(Matcher::text_row(rec, rolled, ord));
                    }
                }
                Cur::Blocks(blocks) => {
                    let Some(i) = blocks.next() else {
                        self.cur = None;
                        continue;
                    };
                    self.out.scanned += 1;
                    let Some(rec) = BlockRec::decode(&blocks.buf[i * REC..(i + 1) * REC]) else { continue };
                    if !self.matcher.block(store, &blocks.world, &rec) {
                        continue;
                    }
                    let target = match self.mode {
                        Mode::Lookup { .. } | Mode::Count => {
                            let row = Matcher::row(store, &blocks.world, &rec, blocks.buf_first + i as u64);
                            self.keep(row);
                            continue;
                        }
                        Mode::Rollback if !rec.rolled => rec.old,
                        Mode::Restore if rec.rolled => rec.new,
                        // Already in the state this job would bring it to
                        Mode::Rollback | Mode::Restore => continue,
                    };
                    let (cx, cz) = (rec.x >> 4, rec.z >> 4);
                    let name = blocks.world.clone();
                    if !self.loaded(world, &name, cx, cz) {
                        self.out.skipped += 1;
                        if self.skipped_chunks.len() < MAX_SKIPPED_CHUNKS {
                            self.skipped_chunks.insert((cx, cz));
                        }
                        self.out.skipped_chunks = self.skipped_chunks.len();
                        continue;
                    }
                    applied += 1;
                    // What was in a broken chest or on a broken sign returns with the block
                    let nbt = if self.mode == Mode::Rollback && rec.action.removes() {
                        match self.cur.as_ref() {
                            Some(Cur::Blocks(blocks)) => blocks.nbt.get(&(rec.x, i32::from(rec.y), rec.z, rec.time)).cloned(),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    if world.set_block(&name, (rec.x, i32::from(rec.y), rec.z), store.states.get(target), nbt.as_deref()) {
                        if let Some(Cur::Blocks(blocks)) = self.cur.as_mut() {
                            blocks.mark(i, self.mode == Mode::Rollback);
                        }
                        self.out.matched += 1;
                    } else {
                        self.out.failed += 1;
                    }
                }
            }
        }
        if self.done {
            // Closes the file and writes the last rolled-back bits
            self.cur = None;
        }
        self.done
    }

    /// The query this job runs.
    pub fn query(&self) -> &Query {
        &self.matcher.query
    }
}

/// Delete whole days older than a cut-off, a few files at a time.
pub struct Purge {
    /// Day folders and text logs still to delete.
    targets: Vec<PathBuf>,
    /// Files of the folder being emptied.
    files: Vec<PathBuf>,
    dir: Option<PathBuf>,
    pub files_removed: u64,
    pub bytes_removed: u64,
    pub days_removed: u32,
    pub done: bool,
}

impl Purge {
    /// Everything of the days before `first_kept` goes.
    pub fn new(store: &Store, first_kept: u32) -> Purge {
        let mut targets = Vec::new();
        let mut days = BTreeSet::new();
        for world in store.worlds() {
            for day in store.block_days(&world).into_iter().filter(|d| *d < first_kept) {
                targets.push(store.block_dir(&world, day));
                days.insert(day);
            }
        }
        for day in store.text_days().into_iter().filter(|d| *d < first_kept) {
            targets.push(store.text_path(day));
            days.insert(day);
        }
        let done = targets.is_empty();
        Purge { targets, files: Vec::new(), dir: None, files_removed: 0, bytes_removed: 0, days_removed: days.len() as u32, done }
    }

    fn remove(&mut self, path: &Path) {
        let size = fs::metadata(path).map_or(0, |m| m.len());
        if fs::remove_file(path).is_ok() {
            self.files_removed += 1;
            self.bytes_removed += size;
        }
    }

    /// Delete up to `max` files. True when finished.
    pub fn step(&mut self, max: usize) -> bool {
        let mut left = max;
        while !self.done && left > 0 {
            if let Some(file) = self.files.pop() {
                self.remove(&file);
                left -= 1;
                continue;
            }
            if let Some(dir) = self.dir.take() {
                let _ = fs::remove_dir(dir);
                continue;
            }
            let Some(target) = self.targets.pop() else {
                self.done = true;
                break;
            };
            if target.is_dir() {
                // Only our own files: anything else an owner put there stays (and keeps the folder)
                self.files = fs::read_dir(&target)
                    .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == EXT || e == NBT_EXT)).collect())
                    .unwrap_or_default();
                self.dir = Some(target);
            } else {
                self.remove(&target);
                left -= 1;
            }
        }
        self.done
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::Write;

    use super::*;
    use crate::params::ActionSet;
    use crate::query::Area;
    use crate::record::Action;
    use crate::store::day_of;
    use crate::store::testdir::TestDir;

    const T0: u32 = 1_794_000_000;
    const W: &str = "overworld";

    /// A world that is a map; chunks in `unloaded` refuse changes.
    #[derive(Default)]
    struct FakeWorld {
        blocks: HashMap<(String, (i32, i32, i32)), String>,
        unloaded: HashSet<(i32, i32)>,
        unknown: HashSet<String>,
        sets: usize,
        /// What is inside block entities: the data a rollback handed back.
        inside: HashMap<(i32, i32, i32), Vec<u8>>,
        /// Containers: item text -> how many. A container holds at most `room` items.
        chests: HashMap<(i32, i32, i32), HashMap<String, i64>>,
        room: i64,
    }

    impl FakeWorld {
        fn at(&self, pos: (i32, i32, i32)) -> &str {
            self.blocks.get(&(W.to_owned(), pos)).map_or("air", String::as_str)
        }
    }

    impl WorldEdit for FakeWorld {
        fn chunk_loaded(&mut self, _: &str, cx: i32, cz: i32) -> bool {
            !self.unloaded.contains(&(cx, cz))
        }

        fn set_block(&mut self, world: &str, pos: (i32, i32, i32), state: &str, nbt: Option<&[u8]>) -> bool {
            if self.unknown.contains(state) {
                return false;
            }
            self.sets += 1;
            self.blocks.insert((world.to_owned(), pos), state.to_owned());
            match nbt {
                Some(data) => self.inside.insert(pos, data.to_vec()),
                None => self.inside.remove(&pos),
            };
            true
        }

        fn change_container(&mut self, _: &str, pos: (i32, i32, i32), item: &str, count: i64) -> Stored {
            let room = self.room;
            let Some(chest) = self.chests.get_mut(&pos) else { return Stored::NoContainer };
            if count > 0 && chest.values().sum::<i64>() + count > room {
                return Stored::NoRoom;
            }
            let have = chest.entry(item.to_owned()).or_insert(0);
            *have = (*have + count).max(0);
            Stored::Done
        }
    }

    /// Log a change and make the fake world agree with it.
    fn act(s: &mut Store, w: &mut FakeWorld, time: u32, user: &str, pos: (i32, i32, i32), action: Action, old: &str, new: &str) {
        s.log_block(W, time, user, pos, action, old, new);
        w.blocks.insert((W.to_owned(), pos), new.to_owned());
    }

    fn run(s: &mut Store, w: &mut FakeWorld, mode: Mode, q: Query) -> Outcome {
        s.flush().unwrap();
        let mut job = Job::new(s, mode, q);
        let mut slices = 0;
        // Small slices: a job must be able to stop anywhere
        while !job.step(s, w, 7, 3) {
            slices += 1;
            assert!(slices < 1_000_000, "job never ends");
        }
        std::mem::take(&mut job.out)
    }

    fn by(user: &str) -> Query {
        Query { users: vec![user.to_ascii_lowercase()], ..Query::everything(Area::Global, ActionSet::blocks()) }
    }

    fn lookup(s: &mut Store, q: Query) -> Vec<Row> {
        run(s, &mut FakeWorld::default(), Mode::Lookup { cap: 1000 }, q).rows
    }

    const STAIRS: &str = "oak_stairs[facing=north,half=bottom]";
    const A: (i32, i32, i32) = (1, 64, 1);
    const B: (i32, i32, i32) = (40, 64, 1);

    /// Alice builds, Bob griefs: breaks her stone and stairs, puts TNT where the stone was.
    fn griefed(dir: &TestDir) -> (Store, FakeWorld) {
        let mut s = Store::open(&dir.0);
        let mut w = FakeWorld::default();
        act(&mut s, &mut w, T0, "Alice", A, Action::Place, "air", "stone");
        act(&mut s, &mut w, T0 + 1, "Alice", B, Action::Place, "air", STAIRS);
        act(&mut s, &mut w, T0 + 10, "Bob", A, Action::Break, "stone", "air");
        act(&mut s, &mut w, T0 + 11, "Bob", A, Action::Place, "air", "tnt");
        act(&mut s, &mut w, T0 + 12, "Bob", B, Action::Break, STAIRS, "air");
        (s, w)
    }

    #[test]
    fn rollback_restore_state_machine() {
        let dir = TestDir::new("machine");
        let (mut s, mut w) = griefed(&dir);
        assert_eq!((w.at(A), w.at(B)), ("tnt", "air"));

        // Rollback: newest first, so the block ends as it was before Bob's first change
        let out = run(&mut s, &mut w, Mode::Rollback, by("Bob"));
        assert_eq!((out.matched, out.skipped, out.failed), (3, 0, 0));
        assert_eq!((w.at(A), w.at(B)), ("stone", STAIRS));
        let rows = lookup(&mut s, by("Bob"));
        assert!(rows.len() == 3 && rows.iter().all(|r| r.rolled));
        assert!(lookup(&mut s, by("Alice")).iter().all(|r| !r.rolled));

        // A second rollback finds nothing left to do and touches nothing
        w.sets = 0;
        let out = run(&mut s, &mut w, Mode::Rollback, by("Bob"));
        assert_eq!((out.matched, w.sets), (0, 0));

        // Restore: oldest first, so the block ends as Bob left it
        let out = run(&mut s, &mut w, Mode::Restore, by("Bob"));
        assert_eq!(out.matched, 3);
        assert_eq!((w.at(A), w.at(B)), ("tnt", "air"));
        assert!(lookup(&mut s, by("Bob")).iter().all(|r| !r.rolled));

        // A second restore does nothing, and a restore never touches what was not rolled back
        w.sets = 0;
        let out = run(&mut s, &mut w, Mode::Restore, Query::everything(Area::Global, ActionSet::blocks()));
        assert_eq!((out.matched, w.sets), (0, 0));

        // Rolling back everyone returns the world to before anyone touched it
        let out = run(&mut s, &mut w, Mode::Rollback, Query::everything(Area::Global, ActionSet::blocks()));
        assert_eq!(out.matched, 5);
        assert_eq!((w.at(A), w.at(B)), ("air", "air"));
    }

    #[test]
    fn rollback_survives_a_restart() {
        let dir = TestDir::new("restart");
        let (mut s, mut w) = griefed(&dir);
        run(&mut s, &mut w, Mode::Rollback, by("Bob"));
        drop(s);
        // The rolled-back bits are on disk, not in memory
        let mut s = Store::open(&dir.0);
        assert_eq!(lookup(&mut s, by("Bob")).iter().filter(|r| r.rolled).count(), 3);
        let out = run(&mut s, &mut w, Mode::Rollback, by("Bob"));
        assert_eq!(out.matched, 0);
        let out = run(&mut s, &mut w, Mode::Restore, by("Bob"));
        assert_eq!(out.matched, 3);
        assert_eq!((w.at(A), w.at(B)), ("tnt", "air"));
    }

    #[test]
    fn unloaded_chunks_are_skipped_and_can_be_repeated() {
        let dir = TestDir::new("unloaded");
        let (mut s, mut w) = griefed(&dir);
        w.unloaded.insert((2, 0));
        let out = run(&mut s, &mut w, Mode::Rollback, by("Bob"));
        assert_eq!((out.matched, out.skipped, out.skipped_chunks), (2, 1, 1));
        assert_eq!((w.at(A), w.at(B)), ("stone", "air"));
        // The skipped record is not marked: going closer and repeating finishes the job
        w.unloaded.clear();
        let out = run(&mut s, &mut w, Mode::Rollback, by("Bob"));
        assert_eq!((out.matched, out.skipped), (1, 0));
        assert_eq!(w.at(B), STAIRS);
    }

    #[test]
    fn unknown_states_are_reported_not_marked() {
        let dir = TestDir::new("unknown");
        let (mut s, mut w) = griefed(&dir);
        w.unknown.insert(STAIRS.to_owned());
        let out = run(&mut s, &mut w, Mode::Rollback, by("Bob"));
        assert_eq!((out.matched, out.failed), (2, 1));
        assert_eq!(lookup(&mut s, by("Bob")).iter().filter(|r| r.rolled).count(), 2);
    }

    #[test]
    fn rollback_filters() {
        let dir = TestDir::new("filters");
        let (mut s, mut w) = griefed(&dir);
        // Only what Bob broke, only near A, only the last few seconds
        let q = Query {
            actions: ActionSet::of(&[Action::Break]),
            area: Area::Column { world: W.into(), x: 0, z: 0, r: 5 },
            since: T0 + 5,
            ..by("bob")
        };
        let out = run(&mut s, &mut w, Mode::Rollback, q);
        assert_eq!(out.matched, 1);
        // The TNT placed later is not undone, but the break under it is: the stone is back
        assert_eq!((w.at(A), w.at(B)), ("stone", "air"));
        // Another world has nothing
        let q = Query { area: Area::World("the_nether".into()), ..by("bob") };
        assert_eq!(run(&mut s, &mut w, Mode::Rollback, q).matched, 0);
        let q = Query { include: vec!["tnt".into()], ..by("bob") };
        assert_eq!(run(&mut s, &mut w, Mode::Rollback, q).matched, 1);
        assert_eq!(w.at(A), "air");
    }

    #[test]
    fn days_are_walked_in_order() {
        let dir = TestDir::new("days");
        let mut s = Store::open(&dir.0);
        let mut w = FakeWorld::default();
        let day = 86_400;
        act(&mut s, &mut w, T0, "Bob", A, Action::Place, "air", "dirt");
        act(&mut s, &mut w, T0 + day, "Bob", A, Action::Break, "dirt", "air");
        act(&mut s, &mut w, T0 + 2 * day, "Bob", A, Action::Place, "air", "tnt");
        assert_eq!(s.block_days(W).len(), 0);
        s.flush().unwrap();
        assert_eq!(s.block_days(W).len(), 3);
        let rows = lookup(&mut s, by("Bob"));
        assert_eq!(rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["tnt", "dirt", "dirt"]);
        // Only the last day and a half
        let q = Query { since: T0 + day, ..by("Bob") };
        let out = run(&mut s, &mut w, Mode::Rollback, q.clone());
        assert_eq!((out.matched, w.at(A)), (2, "dirt"));
        let out = run(&mut s, &mut w, Mode::Restore, q);
        assert_eq!((out.matched, w.at(A)), (2, "tnt"));
        let out = run(&mut s, &mut w, Mode::Rollback, by("Bob"));
        assert_eq!((out.matched, w.at(A)), (3, "air"));
    }

    #[test]
    fn big_files_cross_chunks() {
        let dir = TestDir::new("big");
        let mut s = Store::open(&dir.0);
        let mut w = FakeWorld::default();
        let n = CHUNK as i32 * 2 + 500;
        for i in 0..n {
            let pos = (i % 500, 64 + i / 500, 3);
            act(&mut s, &mut w, T0 + (i as u32) / 100, if i % 2 == 0 { "Bob" } else { "Alice" }, pos, Action::Place, "air", "stone");
        }
        let out = run(&mut s, &mut w, Mode::Count, by("Bob"));
        assert_eq!((out.matched, out.scanned), ((n as u64).div_ceil(2), n as u64));
        let out = run(&mut s, &mut w, Mode::Rollback, by("Bob"));
        assert_eq!(out.matched, (n as u64).div_ceil(2));
        assert_eq!((w.at((0, 64, 3)), w.at((1, 64, 3))), ("air", "stone"));
        // Every bit reached the disk, across chunk borders
        let mut s = Store::open(&dir.0);
        let rows = run(&mut s, &mut w, Mode::Lookup { cap: 100_000 }, Query::everything(Area::Global, ActionSet::blocks())).rows;
        assert_eq!(rows.len(), n as usize);
        assert!(rows.iter().all(|r| r.rolled == (r.user == "Bob")));
        // Newest first, and inside one second the later record first
        assert!(rows.windows(2).all(|p| (p[0].time, p[0].ord) > (p[1].time, p[1].ord)));
    }

    #[test]
    fn lookup_is_capped_to_the_newest_rows() {
        let dir = TestDir::new("cap");
        let mut s = Store::open(&dir.0);
        let mut w = FakeWorld::default();
        for i in 0..1000u32 {
            // Three days, two regions
            let pos = (if i % 2 == 0 { 5 } else { 600 }, 64, i as i32 % 16);
            act(&mut s, &mut w, T0 + i * 200, "Bob", pos, Action::Place, "air", "stone");
        }
        let out = run(&mut s, &mut w, Mode::Lookup { cap: 50 }, by("Bob"));
        assert!(out.truncated);
        assert_eq!(out.rows.len(), 50);
        assert_eq!(out.rows[0].time, T0 + 999 * 200);
        assert_eq!(out.rows[49].time, T0 + 950 * 200);
        let out = run(&mut s, &mut w, Mode::Lookup { cap: 5000 }, by("Bob"));
        assert!(!out.truncated);
        assert_eq!(out.rows.len(), 1000);
    }

    #[test]
    fn radius_reads_only_its_regions() {
        let dir = TestDir::new("regions");
        let mut s = Store::open(&dir.0);
        let mut w = FakeWorld::default();
        for i in 0..40 {
            act(&mut s, &mut w, T0 + i, "Bob", (i as i32 * 512 + 5, 64, 5), Action::Place, "air", "stone");
        }
        let q = Query { area: Area::Column { world: W.into(), x: 512 * 3, z: 0, r: 20 }, ..by("Bob") };
        let out = run(&mut s, &mut w, Mode::Lookup { cap: 100 }, q);
        // Regions 2 and 3 are read (one record each), the other 38 files are never opened
        assert_eq!((out.rows.len(), out.scanned), (1, 2));
        assert_eq!(out.rows[0].x, 512 * 3 + 5);
        // A radius too wide to probe file by file lists the folder instead
        let q = Query { area: Area::Column { world: W.into(), x: 0, z: 0, r: 20_000 }, ..by("Bob") };
        let out = run(&mut s, &mut w, Mode::Lookup { cap: 100 }, q);
        assert_eq!((out.rows.len(), out.scanned), (40, 40));
    }

    #[test]
    fn text_and_blocks_merge_by_time() {
        let dir = TestDir::new("merge");
        let mut s = Store::open(&dir.0);
        let mut w = FakeWorld::default();
        let text = |time, action, text: &str| TextRec { time, action, user: "Bob".into(), world: W.into(), x: 1, y: 64, z: 1, amount: 2, text: text.into() };
        act(&mut s, &mut w, T0 + 1, "Bob", A, Action::Place, "air", "stone");
        s.log_text(&text(T0 + 2, Action::Chat, "hello"));
        s.log_text(&text(T0 + 3, Action::ContainerRemove, "diamond"));
        act(&mut s, &mut w, T0 + 4, "Bob", A, Action::Break, "stone", "air");
        s.log_text(&text(T0 + 86_400, Action::Kill, "cow"));
        let all = ActionSet(u32::MAX);
        let rows = lookup(&mut s, Query::everything(Area::Global, all));
        assert_eq!(rows.iter().map(|r| r.action).collect::<Vec<_>>(), [Action::Kill, Action::Break, Action::ContainerRemove, Action::Chat, Action::Place]);
        // The default lookup leaves chat out
        let rows = lookup(&mut s, Query::everything(Area::Global, ActionSet::default_lookup()));
        assert_eq!(rows.len(), 4);
        let rows = lookup(&mut s, Query::everything(Area::Global, ActionSet::of(&[Action::Chat])));
        assert_eq!((rows.len(), rows[0].text.as_str()), (1, "hello"));
        // A rollback never reads the text log, whatever the actions say
        let out = run(&mut s, &mut w, Mode::Rollback, Query::everything(Area::Global, all));
        assert_eq!(out.matched, 2);
    }

    #[test]
    fn torn_files_lose_only_the_torn_record() {
        let dir = TestDir::new("torn");
        let (mut s, mut w) = griefed(&dir);
        s.log_text(&TextRec { time: T0, action: Action::Chat, user: "Alice".into(), world: W.into(), x: 0, y: 0, z: 0, amount: 0, text: "before".into() });
        s.flush().unwrap();
        let day = day_of(T0);
        let block_file = s.block_path(&FileKey { world: W.into(), day, rx: 0, rz: 0 });
        drop(s);

        // The server dies in the middle of a write to every kind of file
        for (path, junk) in [(block_file.clone(), &[0xEEu8; 17][..]), (dir.0.join("events").join(format!("{}.log", crate::store::day_name(day))), &b"1794000001\tchat\tBo"[..]), (dir.0.join("users.tbl"), &b"Car"[..]), (dir.0.join("states.tbl"), &b"glass["[..])] {
            let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
            f.write_all(junk).unwrap();
        }

        let mut s = Store::open(&dir.0);
        let all = Query::everything(Area::Global, ActionSet(u32::MAX));
        let rows = lookup(&mut s, all.clone());
        assert_eq!(rows.len(), 6);
        assert_eq!(rows.iter().filter(|r| r.user == "Bob").count(), 3);
        assert_eq!(rows.iter().find(|r| r.action == Action::Chat).unwrap().text, "before");
        assert_eq!((s.users.len(), s.states.len()), (2, 4));

        // Life goes on: new records, new names, and the old ones still read the same
        act(&mut s, &mut w, T0 + 20, "Carol", A, Action::Break, "tnt", "air");
        act(&mut s, &mut w, T0 + 21, "Carol", A, Action::Place, "air", "glass");
        s.log_text(&TextRec { time: T0 + 22, action: Action::Chat, user: "Carol".into(), world: W.into(), x: 0, y: 0, z: 0, amount: 0, text: "after".into() });
        let rows = lookup(&mut s, all);
        assert_eq!(rows.len(), 9);
        assert_eq!(fs::metadata(&block_file).unwrap().len() % REC as u64, 0);
        assert_eq!(rows.iter().map(|r| format!("{} {}", r.user, r.text)).take(3).collect::<Vec<_>>(), ["Carol after", "Carol glass", "Carol tnt"]);
        let out = run(&mut s, &mut w, Mode::Rollback, by("Carol"));
        assert_eq!((out.matched, w.at(A)), (2, "tnt"));
    }

    #[test]
    fn damaged_records_inside_a_file_are_skipped() {
        let dir = TestDir::new("damage");
        let (mut s, mut w) = griefed(&dir);
        s.flush().unwrap();
        let path = s.block_path(&FileKey { world: W.into(), day: day_of(T0), rx: 0, rz: 0 });
        let mut bytes = fs::read(&path).unwrap();
        // Zero the third record (Bob's break of the stone), as a file system may after a crash
        bytes[2 * REC..3 * REC].fill(0);
        fs::write(&path, &bytes).unwrap();
        let out = run(&mut s, &mut w, Mode::Rollback, by("Bob"));
        assert_eq!(out.matched, 2);
        assert_eq!((w.at(A), w.at(B)), ("air", STAIRS));
    }

    #[test]
    fn purge_removes_whole_old_days_only() {
        let dir = TestDir::new("purge");
        let mut s = Store::open(&dir.0);
        let mut w = FakeWorld::default();
        let day = 86_400;
        for d in 0..5u32 {
            for r in 0..3 {
                act(&mut s, &mut w, T0 + d * day, "Bob", (r * 512, 64, 0), Action::Place, "air", "stone");
            }
            s.log_text(&TextRec { time: T0 + d * day, action: Action::Chat, user: "Bob".into(), world: W.into(), x: 0, y: 0, z: 0, amount: 0, text: format!("day {d}") });
        }
        s.log_block("the_nether", T0, "Bob", (0, 64, 0), Action::Place, "air", "stone");
        s.flush().unwrap();
        // An owner's own file in a day folder is not ours to delete
        let note = s.block_dir(W, day_of(T0)).join("note.txt");
        fs::write(&note, b"keep").unwrap();

        let first_kept = day_of(T0) + 3;
        let mut purge = Purge::new(&s, first_kept);
        assert_eq!(purge.days_removed, 3);
        let mut slices = 0;
        while !purge.step(2) {
            slices += 1;
            assert!(slices < 100);
        }
        // 3 days x 3 regions + 1 nether file + 3 text logs
        assert_eq!(purge.files_removed, 13);
        assert_eq!(purge.bytes_removed, 10 * REC as u64 + 3 * fs::metadata(s.text_path(first_kept)).unwrap().len());
        assert_eq!(s.block_days(W), [day_of(T0), first_kept, first_kept + 1]);
        assert!(note.exists());
        assert!(s.block_days("the_nether").is_empty());
        assert_eq!(s.text_days(), [first_kept, first_kept + 1]);
        let rows = lookup(&mut s, Query::everything(Area::Global, ActionSet(u32::MAX)));
        assert_eq!(rows.len(), 8);
        assert!(rows.iter().all(|r| r.time >= T0 + 3 * day));
        // Nothing old left: a second purge has nothing to do
        fs::remove_file(note).unwrap();
        let mut purge = Purge::new(&s, first_kept);
        while !purge.step(10) {}
        assert_eq!(purge.files_removed, 0);
        assert!(Purge::new(&s, first_kept).done);
    }

    const CHEST: (i32, i32, i32) = (3, 64, 3);

    fn item(time: u32, user: &str, action: Action, amount: u32, text: &str) -> TextRec {
        TextRec { time, action, user: user.into(), world: W.into(), x: CHEST.0, y: CHEST.1, z: CHEST.2, amount, text: text.into() }
    }

    fn in_chest(w: &FakeWorld, text: &str) -> i64 {
        w.chests[&CHEST].get(text).copied().unwrap_or(0)
    }

    fn with_containers() -> Query {
        let mut actions = ActionSet::blocks();
        actions.add(ActionSet::of(&[Action::ContainerAdd, Action::ContainerRemove]));
        Query { users: vec!["bob".into()], ..Query::everything(Area::Global, actions) }
    }

    /// Alice fills a chest; Bob takes her diamonds and sword and leaves dirt.
    fn robbed(dir: &TestDir) -> (Store, FakeWorld) {
        let mut s = Store::open(&dir.0);
        let mut w = FakeWorld { room: 100, ..FakeWorld::default() };
        const SWORD: &str = "diamond_sword|000a00000002beef";
        s.log_text(&item(T0, "Alice", Action::ContainerAdd, 10, "diamond"));
        s.log_text(&item(T0, "Alice", Action::ContainerAdd, 1, SWORD));
        s.log_text(&item(T0 + 10, "Bob", Action::ContainerAdd, 5, "dirt"));
        s.log_text(&item(T0 + 11, "Bob", Action::ContainerRemove, 10, "diamond"));
        s.log_text(&item(T0 + 12, "Bob", Action::ContainerRemove, 1, SWORD));
        s.log_text(&item(T0 + 13, "Bob", Action::Chat, 0, "thanks"));
        w.chests.insert(CHEST, HashMap::from([("dirt".to_owned(), 5)]));
        (s, w)
    }

    #[test]
    fn container_rollback_and_restore() {
        let dir = TestDir::new("containers");
        let (mut s, mut w) = robbed(&dir);
        const SWORD: &str = "diamond_sword|000a00000002beef";

        // Rollback: what Bob took is back, what he put in is gone; the item keeps its data
        let out = run(&mut s, &mut w, Mode::Rollback, with_containers());
        assert_eq!((out.matched, out.items, out.failed), (3, 3, 0));
        assert_eq!((in_chest(&w, "diamond"), in_chest(&w, SWORD), in_chest(&w, "dirt")), (10, 1, 0));
        let rows = lookup(&mut s, Query::everything(Area::Global, ActionSet(u32::MAX)));
        assert_eq!(rows.iter().filter(|r| r.rolled).count(), 3);
        assert!(rows.iter().filter(|r| r.user == "Alice" || r.action == Action::Chat).all(|r| !r.rolled));

        // Again: nothing left to do
        let out = run(&mut s, &mut w, Mode::Rollback, with_containers());
        assert_eq!((out.matched, in_chest(&w, "diamond")), (0, 10));

        // The marks are on disk
        drop(s);
        let mut s = Store::open(&dir.0);
        let out = run(&mut s, &mut w, Mode::Restore, with_containers());
        assert_eq!((out.matched, out.items), (3, 3));
        assert_eq!((in_chest(&w, "diamond"), in_chest(&w, SWORD), in_chest(&w, "dirt")), (0, 0, 5));
        assert!(lookup(&mut s, Query::everything(Area::Global, ActionSet(u32::MAX))).iter().all(|r| !r.rolled));
        let out = run(&mut s, &mut w, Mode::Restore, with_containers());
        assert_eq!(out.matched, 0);
    }

    #[test]
    fn container_rollback_undoes_the_newest_first() {
        let dir = TestDir::new("container-order");
        let mut s = Store::open(&dir.0);
        let mut w = FakeWorld { room: 100, ..FakeWorld::default() };
        // Bob puts five diamonds in and takes them out again: the chest is empty before and after
        s.log_text(&item(T0, "Bob", Action::ContainerAdd, 5, "diamond"));
        s.log_text(&item(T0 + 1, "Bob", Action::ContainerRemove, 5, "diamond"));
        w.chests.insert(CHEST, HashMap::new());
        // Oldest first would take out nothing, then add five: the rollback would create diamonds
        let out = run(&mut s, &mut w, Mode::Rollback, with_containers());
        assert_eq!((out.matched, in_chest(&w, "diamond")), (2, 0));
        let out = run(&mut s, &mut w, Mode::Restore, with_containers());
        assert_eq!((out.matched, in_chest(&w, "diamond")), (2, 0));
    }

    #[test]
    fn container_records_wait_for_their_container() {
        let dir = TestDir::new("container-missing");
        let (mut s, mut w) = robbed(&dir);
        // Bob also broke the chest: the block comes back first, then its items
        let chest = w.chests.remove(&CHEST).unwrap();
        act(&mut s, &mut w, T0 + 20, "Bob", CHEST, Action::Break, "chest", "air");
        let out = run(&mut s, &mut w, Mode::Rollback, with_containers());
        // No container in this fake world yet: the block is back, the item records are not marked
        assert_eq!((out.matched, out.items, out.failed), (1, 0, 3));
        assert_eq!(w.at(CHEST), "chest");
        w.chests.insert(CHEST, chest);
        let out = run(&mut s, &mut w, Mode::Rollback, with_containers());
        assert_eq!((out.matched, out.items, out.failed), (3, 3, 0));
        assert_eq!(in_chest(&w, "diamond"), 10);
        // A full chest refuses, and the record stays for a repeat
        let out = run(&mut s, &mut w, Mode::Restore, with_containers());
        assert_eq!(out.items, 3);
        w.room = 7;
        let out = run(&mut s, &mut w, Mode::Rollback, with_containers());
        assert_eq!((out.items, out.failed, in_chest(&w, "diamond")), (2, 1, 0));
        // An unloaded chunk skips them like blocks
        w.room = 100;
        w.unloaded.insert((0, 0));
        let out = run(&mut s, &mut w, Mode::Rollback, with_containers());
        // Only the diamonds that did not fit are still to do
        assert_eq!((out.matched, out.skipped), (0, 1));
    }

    #[test]
    fn rollback_only_touches_the_asked_actions() {
        let dir = TestDir::new("container-actions");
        let (mut s, mut w) = robbed(&dir);
        // Blocks only: container records are not read at all
        let out = run(&mut s, &mut w, Mode::Rollback, by("Bob"));
        assert_eq!((out.matched, out.scanned, in_chest(&w, "diamond")), (0, 0, 0));
        // Only what he took
        let q = Query { actions: ActionSet::of(&[Action::ContainerRemove]), include: vec!["diamond".into()], ..with_containers() };
        let out = run(&mut s, &mut w, Mode::Rollback, q);
        assert_eq!((out.matched, in_chest(&w, "diamond"), in_chest(&w, "dirt")), (1, 10, 5));
    }

    #[test]
    fn broken_block_entities_come_back_with_their_data() {
        let dir = TestDir::new("nbt-rollback");
        let mut s = Store::open(&dir.0);
        let mut w = FakeWorld::default();
        act(&mut s, &mut w, T0, "Alice", A, Action::Place, "air", "chest");
        act(&mut s, &mut w, T0 + 5, "Bob", A, Action::Break, "chest", "air");
        s.log_nbt(W, T0 + 5, A, b"27 slots of loot");
        act(&mut s, &mut w, T0 + 6, "Bob", A, Action::Place, "air", "oak_sign");
        act(&mut s, &mut w, T0 + 7, "Bob", A, Action::Break, "oak_sign", "air");
        s.log_nbt(W, T0 + 7, A, b"Bob was here");
        act(&mut s, &mut w, T0 + 8, "Bob", B, Action::Break, "stone", "air");

        // Newest first: the sign returns with its text, then is replaced by the chest with its loot
        let out = run(&mut s, &mut w, Mode::Rollback, by("Bob"));
        assert_eq!(out.matched, 4);
        assert_eq!((w.at(A), w.inside.get(&A).map(Vec::as_slice)), ("chest", Some(&b"27 slots of loot"[..])));
        assert_eq!(w.inside.get(&B), None);
        // A restore breaks it again and hands nothing back
        let out = run(&mut s, &mut w, Mode::Restore, by("Bob"));
        assert_eq!((out.matched, w.at(A), w.inside.get(&A)), (4, "air", None));
        // Only the sign's break: its own data, not the chest's from two seconds before
        let q = Query { include: vec!["oak_sign".into()], actions: ActionSet::of(&[Action::Break]), ..by("Bob") };
        run(&mut s, &mut w, Mode::Rollback, q);
        assert_eq!((w.at(A), w.inside.get(&A).map(Vec::as_slice)), ("oak_sign", Some(&b"Bob was here"[..])));
    }

    #[test]
    fn purge_takes_the_block_entity_data_too() {
        let dir = TestDir::new("purge-nbt");
        let mut s = Store::open(&dir.0);
        let mut w = FakeWorld::default();
        act(&mut s, &mut w, T0, "Bob", A, Action::Break, "chest", "air");
        s.log_nbt(W, T0, A, b"loot");
        s.flush().unwrap();
        let mut purge = Purge::new(&s, day_of(T0) + 1);
        while !purge.step(10) {}
        assert_eq!(purge.files_removed, 2);
        assert!(!s.block_dir(W, day_of(T0)).exists());
    }

    /// Sizes and speeds on a big log (native, not WASM): `cargo test -p blocklog --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn million_records() {
        use std::time::Instant;
        let dir = TestDir::new("million");
        let mut s = Store::open(&dir.0);
        let t = Instant::now();
        // 30 days, 100 regions, 200 players
        for i in 0..1_000_000u32 {
            let region = i % 100;
            let pos = ((region % 10) as i32 * 512 + (i / 100 % 500) as i32, 64 + (i / 50_000) as i32, (region / 10) as i32 * 512 + (i % 7) as i32);
            let action = if i % 2 == 0 { Action::Place } else { Action::Break };
            s.log_block(W, T0 + (i / 100) * 259, &format!("player{}", i % 200), pos, action, "air", "stone");
            if i % 8192 == 0 {
                s.flush().unwrap();
            }
        }
        s.flush().unwrap();
        println!("write 1,000,000 records: {:?}", t.elapsed());
        let files: u64 = s.block_days(W).iter().map(|d| s.regions(W, *d).len() as u64).sum();
        println!("{} days, {files} files, {} MB", s.block_days(W).len(), 1_000_000 * REC / 1_048_576);
        let mut w = FakeWorld::default();
        let mut time = |name: &str, mode: Mode, q: Query| {
            let t = Instant::now();
            let out = run(&mut s, &mut w, mode, q);
            println!("{name}: {:?}, {} read, {} matched, {} rows", t.elapsed(), out.scanned, out.matched, out.rows.len());
        };
        time("lookup r:20, all time", Mode::Lookup { cap: 2000 }, Query::everything(Area::Column { world: W.into(), x: 100, z: 3, r: 20 }, ActionSet::blocks()));
        time("inspect one block", Mode::Lookup { cap: 2000 }, Query::everything(Area::Point { world: W.into(), x: 100, y: 64, z: 3 }, ActionSet::blocks()));
        time("lookup u:player7 global, newest 2000", Mode::Lookup { cap: 2000 }, by("player7"));
        time("count u:player7 global", Mode::Count, by("player7"));
        time("count t:1d global", Mode::Count, Query { since: T0 + 29 * 86_400, ..Query::everything(Area::Global, ActionSet::blocks()) });
        time("rollback u:player7 global", Mode::Rollback, by("player7"));
    }
}

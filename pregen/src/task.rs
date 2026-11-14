//! One generation task per world.
//!
//! The task requests a full-chunk ticket for a run of up to `max_strip`
//! chunks, waits until every chunk of the run is loaded (generated or read from
//! disk), then releases the tickets and the server saves and unloads them.
//! Chunks that are already loaded (a player is near, or another ticket holds
//! them) are counted and never touched, so releasing our tickets cannot unload
//! anything someone else keeps loaded.
use std::collections::VecDeque;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::pattern::{self, Pattern, Positions};
use crate::shape::{Kind, Shape};

/// What the task needs from the server; the plugin implements it, tests fake it.
pub trait Host {
    fn is_loaded(&self, x: i32, z: i32) -> bool;
    /// Add one load ticket per chunk.
    fn request(&self, chunks: &[(i32, i32)]);
    /// Remove the tickets added by `request`.
    fn release(&self, chunks: &[(i32, i32)]);
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Selection {
    pub world: String,
    pub shape: Kind,
    pub cx: f64,
    pub cz: f64,
    pub rx: f64,
    pub rz: f64,
    pub pattern: Pattern,
    #[serde(default)]
    pub csv: Option<String>,
}

impl Selection {
    pub fn new(world: String) -> Selection {
        Selection { world, shape: Kind::Square, cx: 0.0, cz: 0.0, rx: 500.0, rz: 500.0, pattern: Pattern::Region, csv: None }
    }

    pub fn geometry(&self) -> Shape {
        Shape::new(self.shape, self.cx, self.cz, self.rx, self.rz)
    }

    pub fn center_chunk(&self) -> (i32, i32) {
        ((self.cx / 16.0).floor() as i32, (self.cz / 16.0).floor() as i32)
    }

    pub fn total(&self, csv: &[(i32, i32)]) -> u64 {
        if self.pattern == Pattern::Csv { csv.len() as u64 } else { self.geometry().chunk_count() }
    }

    pub fn radius_text(&self) -> String {
        if self.shape.two_radii() { format!("{} x {}", num(self.rx), num(self.rz)) } else { num(self.rx) }
    }

    pub fn center_text(&self) -> String {
        format!("{}, {}", num(self.cx), num(self.cz))
    }
}

fn num(v: f64) -> String {
    if v.fract() == 0.0 { format!("{v:.0}") } else { format!("{v:.1}") }
}

/// Saved to `tasks/<world>.json` so a paused or interrupted task continues.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Saved {
    pub selection: Selection,
    pub index: u64,
    pub processed: u64,
    pub elapsed: u64,
    #[serde(default)]
    pub starter: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_in_flight: usize,
    pub max_strips_per_tick: usize,
    pub max_strip: usize,
    /// Pattern positions examined per tick at most (bounds tick time on huge empty areas).
    pub scan_budget: usize,
    pub retry_secs: u64,
}

enum Next {
    Pos(u64, (i32, i32)),
    Budget,
    End,
}

struct Strip {
    end_index: u64,
    count: u64,
    chunks: Vec<(i32, i32)>,
    cursor: usize,
    forced: bool,
    done: bool,
    issued: Instant,
}

pub struct Task {
    pub sel: Selection,
    shape: Shape,
    csv: bool,
    positions: Positions,
    next_index: u64,
    peek: Option<(u64, (i32, i32))>,
    pub total: u64,
    committed_index: u64,
    committed_processed: u64,
    /// Chunks already done when this run of the task began (a resumed task starts above zero).
    start_processed: u64,
    extra_done: u64,
    inflight: VecDeque<Strip>,
    inflight_chunks: usize,
    exhausted: bool,
    elapsed_before: u64,
    started: Instant,
    samples: VecDeque<(Instant, u64)>,
    pub last: (i32, i32),
    pub starter: Option<String>,
}

impl Task {
    pub fn new(sel: Selection, csv: &[(i32, i32)], saved: Option<&Saved>) -> Task {
        let shape = sel.geometry();
        let mut positions = pattern::positions(sel.pattern, shape.chunk_bounds(), sel.center_chunk(), csv);
        let index = saved.map_or(0, |s| s.index);
        if index > 0 {
            let _ = positions.nth(index as usize - 1);
        }
        let total = sel.total(csv);
        Task {
            csv: sel.pattern == Pattern::Csv,
            shape,
            positions,
            next_index: index,
            peek: None,
            total,
            committed_index: index,
            committed_processed: saved.map_or(0, |s| s.processed.min(total)),
            start_processed: saved.map_or(0, |s| s.processed.min(total)),
            extra_done: 0,
            inflight: VecDeque::new(),
            inflight_chunks: 0,
            exhausted: false,
            elapsed_before: saved.map_or(0, |s| s.elapsed),
            started: Instant::now(),
            samples: VecDeque::new(),
            last: sel.center_chunk(),
            starter: saved.and_then(|s| s.starter.clone()),
            sel,
        }
    }

    fn next_pos(&mut self, budget: &mut usize) -> Next {
        if let Some((i, p)) = self.peek.take() {
            return Next::Pos(i, p);
        }
        if *budget == 0 {
            return Next::Budget;
        }
        *budget -= 1;
        match self.positions.next() {
            Some(p) => {
                let i = self.next_index;
                self.next_index += 1;
                Next::Pos(i, p)
            }
            None => Next::End,
        }
    }

    fn inside(&self, x: i32, z: i32) -> bool {
        self.csv || self.shape.contains_chunk(x, z)
    }

    /// Next run of adjacent, not yet loaded chunks in pattern order.
    /// `Ok(None)` = pattern finished, `Err(())` = scan budget used up this tick.
    fn build_strip(&mut self, host: &dyn Host, limits: &Limits, budget: &mut usize) -> Result<Option<Strip>, ()> {
        let (i, (x, z)) = loop {
            match self.next_pos(budget) {
                Next::Pos(i, (x, z)) if self.inside(x, z) => break (i, (x, z)),
                Next::Pos(..) => continue,
                Next::Budget => return Err(()),
                Next::End => return Ok(None),
            }
        };
        let now = Instant::now();
        if host.is_loaded(x, z) {
            return Ok(Some(Strip { end_index: i + 1, count: 1, chunks: Vec::new(), cursor: 0, forced: false, done: true, issued: now }));
        }
        let mut chunks = vec![(x, z)];
        let mut end = i + 1;
        let mut dir: Option<(i32, i32)> = None;
        let mut free = usize::MAX;
        while chunks.len() < limits.max_strip {
            let Next::Pos(j, (nx, nz)) = self.next_pos(&mut free) else { break };
            let (lx, lz) = *chunks.last().unwrap_or(&(x, z));
            let step = (nx - lx, nz - lz);
            let unit = matches!(step, (1, 0) | (-1, 0) | (0, 1) | (0, -1));
            if unit && dir.is_none_or(|d| d == step) && self.inside(nx, nz) && !host.is_loaded(nx, nz) {
                dir = Some(step);
                chunks.push((nx, nz));
                end = j + 1;
            } else {
                self.peek = Some((j, (nx, nz)));
                break;
            }
        }
        let count = chunks.len() as u64;
        Ok(Some(Strip { end_index: end, count, chunks, cursor: 0, forced: true, done: false, issued: now }))
    }

    /// Advance one server tick. Returns true when every chunk is done.
    pub fn tick(&mut self, host: &dyn Host, limits: &Limits) -> bool {
        // 1. Release runs whose chunks are all loaded
        let mut released = Vec::new();
        let mut retry = Vec::new();
        for (n, s) in self.inflight.iter_mut().enumerate() {
            if s.done || !s.forced {
                continue;
            }
            while let Some(&(x, z)) = s.chunks.get(s.cursor) {
                if host.is_loaded(x, z) { s.cursor += 1 } else { break }
            }
            if s.cursor == s.chunks.len() {
                released.push(n);
            } else if s.issued.elapsed().as_secs() >= limits.retry_secs {
                // A lost ticket must not stall the task forever
                s.issued = Instant::now();
                retry.push(n);
            }
        }
        for n in retry {
            // Tickets are counted: drop ours before asking again
            host.release(&self.inflight[n].chunks);
            host.request(&self.inflight[n].chunks);
        }
        for n in released {
            host.release(&self.inflight[n].chunks);
            let s = &mut self.inflight[n];
            s.done = true;
            self.inflight_chunks -= s.chunks.len();
            self.extra_done += s.count;
            self.last = s.chunks[s.chunks.len() - 1];
        }

        // 2. Commit the finished prefix (what a restart may skip)
        while self.inflight.front().is_some_and(|s| s.done) {
            if let Some(s) = self.inflight.pop_front() {
                self.committed_index = s.end_index;
                self.committed_processed += s.count;
                self.extra_done -= s.count;
            }
        }

        // 3. Issue new runs
        let mut issued = 0;
        let mut budget = limits.scan_budget;
        while !self.exhausted && self.inflight_chunks < limits.max_in_flight && issued < limits.max_strips_per_tick {
            match self.build_strip(host, limits, &mut budget) {
                Ok(Some(s)) => {
                    if s.forced {
                        host.request(&s.chunks);
                        self.inflight_chunks += s.chunks.len();
                        issued += 1;
                    } else {
                        self.extra_done += s.count;
                    }
                    self.inflight.push_back(s);
                }
                Ok(None) => self.exhausted = true,
                Err(()) => break,
            }
        }
        if self.exhausted && self.inflight.iter().all(|s| s.done) {
            // Flush the commit for the final runs
            while let Some(s) = self.inflight.pop_front() {
                self.committed_index = s.end_index;
                self.committed_processed += s.count;
                self.extra_done -= s.count;
            }
            return true;
        }
        false
    }

    /// Release every ticket we hold (pause, cancel, unload).
    pub fn abort(&mut self, host: &dyn Host) {
        for s in self.inflight.iter().filter(|s| s.forced && !s.done) {
            host.release(&s.chunks);
        }
        self.inflight.clear();
        self.inflight_chunks = 0;
        self.extra_done = 0;
    }

    pub fn processed(&self) -> u64 {
        (self.committed_processed + self.extra_done).min(self.total)
    }

    pub fn percent(&self) -> f64 {
        if self.total == 0 { 100.0 } else { self.processed() as f64 * 100.0 / self.total as f64 }
    }

    pub fn elapsed(&self) -> u64 {
        self.elapsed_before + self.started.elapsed().as_secs()
    }

    /// Chunks per second over the last 30 seconds (since the start until there are two samples).
    pub fn rate(&mut self) -> f64 {
        let now = Instant::now();
        let done = self.processed();
        self.samples.push_back((now, done));
        while self.samples.len() > 2 && self.samples.front().is_some_and(|(t, _)| now.duration_since(*t).as_secs() > 30) {
            self.samples.pop_front();
        }
        match self.samples.front() {
            Some((t, d)) if now.duration_since(*t).as_secs_f64() >= 0.5 => {
                (done - d) as f64 / now.duration_since(*t).as_secs_f64()
            }
            _ => {
                let secs = self.started.elapsed().as_secs_f64();
                if secs >= 0.5 { done.saturating_sub(self.start_processed) as f64 / secs } else { 0.0 }
            }
        }
    }

    pub fn saved(&self) -> Saved {
        Saved {
            selection: self.sel.clone(),
            index: self.committed_index,
            processed: self.committed_processed,
            elapsed: self.elapsed(),
            starter: self.starter.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashSet;

    /// Fake server: a forced chunk loads `delay` ticks after its ticket.
    struct Fake {
        loaded: RefCell<HashSet<(i32, i32)>>,
        pending: RefCell<Vec<((i32, i32), u32)>>,
        forced: RefCell<HashSet<(i32, i32)>>,
        log: RefCell<Vec<(i32, i32)>>,
        delay: u32,
    }

    impl Fake {
        fn new(delay: u32) -> Fake {
            Fake { loaded: Default::default(), pending: Default::default(), forced: Default::default(), log: Default::default(), delay }
        }
        fn step(&self) {
            let mut p = self.pending.borrow_mut();
            for (c, t) in p.iter_mut() {
                if *t == 0 { self.loaded.borrow_mut().insert(*c); } else { *t -= 1; }
            }
            p.retain(|(c, _)| !self.loaded.borrow().contains(c));
        }
    }

    impl Host for Fake {
        fn is_loaded(&self, x: i32, z: i32) -> bool {
            self.loaded.borrow().contains(&(x, z))
        }
        fn request(&self, chunks: &[(i32, i32)]) {
            for &c in chunks {
                self.log.borrow_mut().push(c);
                assert!(self.forced.borrow_mut().insert(c), "double ticket {c:?}");
                self.pending.borrow_mut().push((c, self.delay));
            }
        }
        fn release(&self, chunks: &[(i32, i32)]) {
            for c in chunks {
                assert!(self.forced.borrow_mut().remove(c), "release without ticket {c:?}");
                self.loaded.borrow_mut().remove(c);
                self.pending.borrow_mut().retain(|(p, _)| p != c);
            }
        }
    }

    const L: Limits = Limits { max_in_flight: 64, max_strips_per_tick: 8, max_strip: 32, scan_budget: 4096, retry_secs: 120 };

    fn run(sel: Selection, host: &Fake) -> (u64, usize) {
        let mut t = Task::new(sel, &[], None);
        for n in 0..100_000 {
            host.step();
            if t.tick(host, &L) {
                return (t.processed(), n);
            }
        }
        panic!("task did not finish");
    }

    #[test]
    fn generates_every_chunk_once_and_releases_tickets() {
        for pattern in [Pattern::Region, Pattern::Concentric, Pattern::Loop, Pattern::Spiral] {
            for shape in [Kind::Square, Kind::Circle, Kind::Star] {
                let host = Fake::new(3);
                let mut sel = Selection::new("world".into());
                sel.shape = shape;
                sel.pattern = pattern;
                sel.rx = 300.0;
                let total = sel.total(&[]);
                let (done, _) = run(sel, &host);
                assert_eq!(done, total, "{pattern:?} {shape:?}");
                assert!(host.forced.borrow().is_empty(), "{pattern:?} {shape:?}: tickets left");
            }
        }
    }

    #[test]
    fn preloaded_chunks_are_counted_and_never_forced() {
        let host = Fake::new(1);
        host.loaded.borrow_mut().insert((0, 0));
        host.loaded.borrow_mut().insert((1, 0));
        let mut sel = Selection::new("world".into());
        sel.rx = 100.0;
        let total = sel.total(&[]);
        let (done, _) = run(sel, &host);
        assert_eq!(done, total);
        // Admin-forced chunk stays loaded and its state untouched
        assert!(host.loaded.borrow().contains(&(0, 0)));
        assert!(!host.log.borrow().iter().any(|c| *c == (0, 0) || *c == (1, 0)));
    }

    #[test]
    fn resume_from_saved_index_finishes_the_rest() {
        let host = Fake::new(2);
        let mut sel = Selection::new("world".into());
        sel.rx = 400.0;
        let total = sel.total(&[]);
        let mut t = Task::new(sel.clone(), &[], None);
        for _ in 0..40 {
            host.step();
            t.tick(&host, &L);
        }
        t.abort(&host);
        let saved = t.saved();
        assert!(saved.processed > 0 && saved.processed < total);
        let mut t2 = Task::new(sel, &[], Some(&saved));
        let mut finished = false;
        for _ in 0..100_000 {
            host.step();
            if t2.tick(&host, &L) {
                finished = true;
                break;
            }
        }
        assert!(finished);
        assert_eq!(t2.processed(), total);
        assert!(host.forced.borrow().is_empty());
    }

    #[test]
    fn in_flight_never_exceeds_limit() {
        let host = Fake::new(50);
        let mut sel = Selection::new("world".into());
        sel.rx = 2000.0;
        let mut t = Task::new(sel, &[], None);
        for _ in 0..200 {
            host.step();
            t.tick(&host, &L);
            assert!(host.forced.borrow().len() <= L.max_in_flight + L.max_strip);
        }
    }
}

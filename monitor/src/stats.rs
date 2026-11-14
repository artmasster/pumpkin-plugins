//! Tick timing: what a tick cost, how many fit in a second, and which ones were slow.
//! Plain Rust, fed one finished tick at a time.
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// Raw ticks kept for percentiles (one minute at 20 TPS).
const RECENT: usize = 1200;
/// Seconds of history kept for the long windows (15 minutes).
const SECONDS: usize = 900;
/// Slow ticks remembered for `/spark spikes`.
const SPIKES: usize = 50;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// A tick this long (milliseconds) is remembered as a spike. 50 = one full tick.
    pub spike_ms: f32,
    /// Write spikes to the console, at most one line every ten seconds.
    pub log_spikes: bool,
    /// Longest `/spark profiler` recording in seconds.
    pub max_record_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self { language: "th".into(), spike_ms: 100.0, log_spikes: true, max_record_secs: 600 }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Self {
        self.spike_ms = if self.spike_ms.is_finite() { self.spike_ms.clamp(20.0, 10_000.0) } else { 100.0 };
        self.max_record_secs = self.max_record_secs.clamp(10, 3600);
        self
    }

    pub fn thai(&self) -> bool {
        self.language != "en"
    }
}

/// One wall-clock second of ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Second {
    pub ticks: u16,
    pub sum_ms: f32,
    pub max_ms: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mspt {
    pub min: f32,
    pub median: f32,
    pub p95: f32,
    pub max: f32,
    pub mean: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spike {
    pub at_ms: u64,
    pub tick: i32,
    pub ms: f32,
}

#[derive(Default)]
pub struct Ticks {
    recent: VecDeque<f32>,
    seconds: VecDeque<Second>,
    current: Second,
    current_sec: u64,
    spikes: VecDeque<Spike>,
    /// The second we started in is only part of a second: it must not count as a slow one.
    partial: bool,
    pub total: u64,
}

impl Ticks {
    /// A tick that ended at `now_ms` and took `ms`. Returns true when it counts as a spike.
    pub fn push(&mut self, now_ms: u64, tick: i32, ms: f32, spike_ms: f32) -> bool {
        let sec = now_ms / 1000;
        if self.total == 0 {
            self.current_sec = sec;
            self.partial = true;
        }
        // Close the seconds that have passed; a frozen server leaves seconds with no tick at all
        while self.current_sec < sec {
            if self.partial {
                self.partial = false;
            } else {
                self.seconds.push_back(self.current);
            }
            if self.seconds.len() > SECONDS {
                self.seconds.pop_front();
            }
            self.current = Second::default();
            self.current_sec += 1;
            // After a very long stall there is nothing to learn from more empty seconds
            if sec - self.current_sec > SECONDS as u64 {
                self.current_sec = sec;
            }
        }
        self.current.ticks = self.current.ticks.saturating_add(1);
        self.current.sum_ms += ms;
        self.current.max_ms = self.current.max_ms.max(ms);
        self.recent.push_back(ms);
        if self.recent.len() > RECENT {
            self.recent.pop_front();
        }
        self.total += 1;
        let spike = ms >= spike_ms;
        if spike {
            self.spikes.push_back(Spike { at_ms: now_ms, tick, ms });
            if self.spikes.len() > SPIKES {
                self.spikes.pop_front();
            }
        }
        spike
    }

    /// Ticks per second over the last `window` whole seconds (fewer when the history is shorter).
    pub fn tps(&self, window: usize) -> Option<f32> {
        let n = window.min(self.seconds.len());
        if n == 0 {
            return None;
        }
        let ticks: u32 = self.seconds.iter().rev().take(n).map(|s| u32::from(s.ticks)).sum();
        Some(ticks as f32 / n as f32)
    }

    /// Tick cost over the last `ticks` ticks (at most one minute's worth).
    pub fn mspt(&self, ticks: usize) -> Option<Mspt> {
        let mut v: Vec<f32> = self.recent.iter().rev().take(ticks).copied().collect();
        if v.is_empty() {
            return None;
        }
        v.sort_by(f32::total_cmp);
        let at = |q: f32| v[(((v.len() - 1) as f32) * q).round() as usize];
        Some(Mspt { min: v[0], median: at(0.5), p95: at(0.95), max: v[v.len() - 1], mean: v.iter().sum::<f32>() / v.len() as f32 })
    }

    /// The last `n` whole seconds, oldest first.
    pub fn last_seconds(&self, n: usize) -> Vec<Second> {
        let skip = self.seconds.len().saturating_sub(n);
        self.seconds.iter().skip(skip).copied().collect()
    }

    /// Remembered slow ticks, newest first.
    pub fn spikes(&self) -> Vec<Spike> {
        self.spikes.iter().rev().copied().collect()
    }
}

/// A `/spark profiler` recording: one line every five seconds plus the slowest ticks.
pub struct Recording {
    pub started_ms: u64,
    pub until_ms: u64,
    pub samples: Vec<Sample>,
    pub slowest: Vec<Spike>,
    pub ticks: u64,
    pub sum_ms: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub at_secs: u64,
    pub tps: f32,
    pub mean_ms: f32,
    pub max_ms: f32,
    pub players: u32,
    pub entities: u32,
}

impl Recording {
    pub fn new(now_ms: u64, secs: u64) -> Self {
        Self { started_ms: now_ms, until_ms: now_ms + secs * 1000, samples: Vec::new(), slowest: Vec::new(), ticks: 0, sum_ms: 0.0 }
    }

    pub fn tick(&mut self, now_ms: u64, tick: i32, ms: f32) {
        self.ticks += 1;
        self.sum_ms += f64::from(ms);
        // Keep the 20 slowest
        if self.slowest.len() < 20 || self.slowest.last().is_some_and(|s| ms > s.ms) {
            self.slowest.push(Spike { at_ms: now_ms, tick, ms });
            self.slowest.sort_by(|a, b| b.ms.total_cmp(&a.ms));
            self.slowest.truncate(20);
        }
    }

    /// The report file. `counts` = entity types and how many, biggest first.
    pub fn report(&self, now_ms: u64, version: &str, pumpkin: &str, counts: &[(String, u32)]) -> String {
        let secs = now_ms.saturating_sub(self.started_ms) as f64 / 1000.0;
        let mut out = String::new();
        out.push_str(&format!("Monitor {version} tick recording (Pumpkin {pumpkin})\n"));
        out.push_str(&format!("length: {secs:.0} s, ticks: {}, average TPS: {:.2}, average tick: {:.2} ms\n", self.ticks, self.ticks as f64 / secs.max(0.001), self.sum_ms / (self.ticks.max(1) as f64)));
        out.push_str("This is a timing record, not a CPU call graph: a WASM plugin cannot sample the server's threads.\n");
        out.push_str("\n# timeline (every 5 s)\n  time   tps   mean ms    max ms  players  entities\n");
        for s in &self.samples {
            out.push_str(&format!("{:>5}s {:>5.1} {:>9.2} {:>9.2} {:>8} {:>9}\n", s.at_secs, s.tps, s.mean_ms, s.max_ms, s.players, s.entities));
        }
        out.push_str("\n# slowest ticks\n  at     tick        ms\n");
        for s in &self.slowest {
            out.push_str(&format!("{:>5.1}s {:>8} {:>9.2}\n", s.at_ms.saturating_sub(self.started_ms) as f64 / 1000.0, s.tick, s.ms));
        }
        out.push_str("\n# entities at the end\n");
        for (name, n) in counts {
            out.push_str(&format!("{n:>7}  {name}\n"));
        }
        out
    }
}

/// `EntityType::AcaciaChestBoat` → `acacia_chest_boat` (entity type names come as Rust enum names).
pub fn snake(name: &str) -> String {
    let name = name.rsplit("::").next().unwrap_or(name);
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Colour code for a TPS value, the way spark colours it.
pub fn tps_colour(tps: f32) -> &'static str {
    if tps >= 18.0 {
        "&a"
    } else if tps >= 16.0 {
        "&e"
    } else {
        "&c"
    }
}

/// Colour code for a tick cost: a tick has 50 ms.
pub fn ms_colour(ms: f32) -> &'static str {
    if ms <= 40.0 {
        "&a"
    } else if ms <= 50.0 {
        "&e"
    } else {
        "&c"
    }
}

/// Median of a list (0 for an empty one).
pub fn median(values: &mut [u32]) -> u32 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    values[values.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tps_counts_whole_seconds_and_stalls() {
        let mut t = Ticks::default();
        // 10 seconds at 20 TPS, 5 ms a tick
        for i in 0..200u64 {
            t.push(1_000_000 + i * 50, i as i32, 5.0, 100.0);
        }
        // Neither the second we started in nor the running one is counted: eight whole seconds
        assert_eq!(t.last_seconds(100).len(), 8);
        assert_eq!(t.tps(5), Some(20.0));
        assert_eq!(t.tps(900), Some(20.0));
        // A three second freeze, then one tick
        t.push(1_000_000 + 200 * 50 + 3_000, 200, 3000.0, 100.0);
        let tps = t.tps(4).unwrap();
        assert!(tps < 11.0, "{tps}");
        assert_eq!(t.spikes().first().map(|s| s.tick), Some(200));
        assert_eq!(t.total, 201);
    }

    #[test]
    fn mspt_percentiles() {
        let mut t = Ticks::default();
        assert!(t.mspt(100).is_none() && t.tps(5).is_none());
        for i in 1..=100u64 {
            t.push(i * 50, i as i32, i as f32, 1000.0);
        }
        let m = t.mspt(100).unwrap();
        assert_eq!((m.min, m.max), (1.0, 100.0));
        assert!((m.median - 50.0).abs() <= 1.0 && (m.p95 - 95.0).abs() <= 1.0, "{m:?}");
        assert!((m.mean - 50.5).abs() < 0.01);
        // Only the newest ticks when asked for fewer
        assert_eq!(t.mspt(10).unwrap().min, 91.0);
    }

    #[test]
    fn history_is_bounded() {
        let mut t = Ticks::default();
        for i in 0..40_000u64 {
            t.push(i * 50, i as i32, 200.0, 100.0);
        }
        assert_eq!(t.recent.len(), RECENT);
        assert_eq!(t.seconds.len(), SECONDS);
        assert_eq!(t.spikes().len(), SPIKES);
        // A stall of days does not loop for days
        t.push(40_000 * 50 + 86_400_000 * 30, 1, 1.0, 100.0);
        assert!(t.seconds.len() <= SECONDS);
    }

    #[test]
    fn recording_keeps_the_slowest_and_writes_a_report() {
        let mut r = Recording::new(10_000, 60);
        for i in 0..100u64 {
            r.tick(10_000 + i * 50, i as i32, if i == 42 { 500.0 } else { i as f32 / 10.0 });
        }
        assert_eq!(r.slowest.len(), 20);
        assert_eq!(r.slowest[0].tick, 42);
        r.samples.push(Sample { at_secs: 5, tps: 19.9, mean_ms: 4.2, max_ms: 500.0, players: 3, entities: 120 });
        let text = r.report(15_000, "1.0.0", "0.2.2", &[("zombie".into(), 7)]);
        assert!(text.contains("ticks: 100") && text.contains("zombie") && text.contains("500.00"));
    }

    #[test]
    fn small_helpers() {
        assert_eq!(snake("AcaciaChestBoat"), "acacia_chest_boat");
        assert_eq!(snake("Tnt"), "tnt");
        assert_eq!(snake("EntityType::GlowSquid"), "glow_squid");
        assert_eq!((tps_colour(19.9), tps_colour(17.0), tps_colour(9.0)), ("&a", "&e", "&c"));
        assert_eq!((ms_colour(10.0), ms_colour(45.0), ms_colour(80.0)), ("&a", "&e", "&c"));
        assert_eq!(median(&mut [30, 10, 20]), 20);
        assert_eq!(median(&mut []), 0);
        let cfg: Config = toml::from_str("spike-ms = 1.0\nmax-record-secs = 999999").unwrap();
        let cfg = cfg.cleaned();
        assert_eq!((cfg.spike_ms, cfg.max_record_secs), (20.0, 3600));
    }
}

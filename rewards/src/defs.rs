//! Rewards: a reward for each day a player comes back in a row, and rewards for time played.
//! No server needed here.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const NS: &str = "rewards";
pub const MAX_DAYS: usize = 27;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// Hours from UTC of the server's day (7 = Thailand): a new day starts at its midnight.
    pub utc_offset: f64,
    /// Missing a day starts the row again from day 1.
    pub reset_missed: bool,
    /// Tell players who join that today's reward is waiting.
    pub remind_on_join: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), utc_offset: 7.0, reset_missed: true, remind_on_join: true }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.utc_offset = if self.utc_offset.is_finite() { self.utc_offset.clamp(-12.0, 14.0) } else { 7.0 };
        self
    }

    pub fn thai(&self) -> bool {
        self.language == "th"
    }

    /// The day number (days since 1970 in the server's day) of an epoch second.
    pub fn day(&self, now: u64) -> i64 {
        (now as i64 + (self.utc_offset * 3600.0) as i64).div_euclid(86_400)
    }

    /// Seconds until the next day starts.
    pub fn until_tomorrow(&self, now: u64) -> u64 {
        let shifted = now as i64 + (self.utc_offset * 3600.0) as i64;
        (86_400 - shifted.rem_euclid(86_400)) as u64
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Reward {
    pub item: Option<String>,
    pub count: u32,
    pub money: f64,
    pub commands: Vec<String>,
    pub text: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Playtime {
    pub id: String,
    pub minutes: u64,
    /// Given again every `minutes`.
    pub repeat: bool,
    pub reward: Reward,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Rules {
    pub days: Vec<Reward>,
    pub playtime: Vec<Playtime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub th: String,
    pub en: String,
}

impl Note {
    pub fn new(place: &str, th: impl AsRef<str>, en: impl AsRef<str>) -> Note {
        Note { th: format!("{place}: {}", th.as_ref()), en: format!("{place}: {}", en.as_ref()) }
    }

    pub fn text(&self, thai: bool) -> &str {
        if thai { &self.th } else { &self.en }
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn number(v: Option<&toml::Value>) -> Option<f64> {
    match v? {
        toml::Value::Integer(i) => Some(*i as f64),
        toml::Value::Float(f) if f.is_finite() => Some(*f),
        _ => None,
    }
}

fn reward(place: &str, t: &toml::Table, out: &mut Vec<Note>) -> Option<Reward> {
    let item = match t.get("item").and_then(toml::Value::as_str) {
        Some(i) => {
            let i = i.trim().to_ascii_lowercase();
            let ok = match i.strip_prefix("minecraft:") {
                Some(n) => valid_id(n),
                None => valid_id(&i),
            };
            if !ok {
                out.push(Note::new(place, "item ต้องเป็น id ของ CustomItems หรือ minecraft:<ไอเท็ม>", "item must be a CustomItems id or minecraft:<item>"));
                return None;
            }
            Some(i)
        }
        None => None,
    };
    let commands: Vec<String> = match t.get("commands") {
        Some(toml::Value::String(one)) => vec![one.trim_start_matches('/').to_owned()],
        Some(toml::Value::Array(many)) => many.iter().filter_map(toml::Value::as_str).map(|c| c.trim_start_matches('/').to_owned()).collect(),
        _ => Vec::new(),
    };
    let money = number(t.get("money")).unwrap_or(0.0).max(0.0);
    if item.is_none() && commands.is_empty() && money <= 0.0 {
        out.push(Note::new(place, "ต้องมี item, money หรือ commands", "needs an item, money or commands"));
        return None;
    }
    Some(Reward { item, count: number(t.get("count")).unwrap_or(1.0).clamp(1.0, 2304.0) as u32, money, commands, text: t.get("text").and_then(toml::Value::as_str).map(str::to_owned) })
}

/// The rewards file.
pub fn parse(text: &str, out: &mut Vec<Note>) -> Rules {
    let file = "rewards.toml";
    let table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            let why = e.to_string().lines().next().unwrap_or_default().to_owned();
            out.push(Note::new(file, format!("อ่านไม่ได้: {why}"), format!("cannot be read: {why}")));
            return Rules::default();
        }
    };
    let mut rules = Rules::default();
    for (n, d) in table.get("days").and_then(toml::Value::as_array).cloned().unwrap_or_default().iter().enumerate().take(MAX_DAYS) {
        let place = format!("{file} day {}", n + 1);
        match d.as_table().and_then(|t| reward(&place, t, out)) {
            Some(r) => rules.days.push(r),
            // A broken day would shift every day after it
            None => rules.days.push(Reward { text: Some("?".into()), ..Reward::default() }),
        }
    }
    for p in table.get("playtime").and_then(toml::Value::as_array).cloned().unwrap_or_default() {
        let Some(t) = p.as_table() else { continue };
        let id = t.get("id").and_then(toml::Value::as_str).unwrap_or_default().to_ascii_lowercase();
        let place = format!("{file} playtime {id}");
        if !valid_id(&id) || rules.playtime.iter().any(|x| x.id == id) {
            out.push(Note::new(&place, "ต้องมี id ที่ไม่ซ้ำ (a-z, 0-9, _)", "needs an id of its own (a-z, 0-9, _)"));
            continue;
        }
        let Some(minutes) = number(t.get("minutes")).filter(|m| *m >= 1.0) else {
            out.push(Note::new(&place, "minutes ต้องตั้งแต่ 1", "minutes must be 1 or more"));
            continue;
        };
        if let Some(r) = reward(&place, t, out) {
            rules.playtime.push(Playtime { id, minutes: minutes as u64, repeat: t.get("repeat").and_then(toml::Value::as_bool).unwrap_or(false), reward: r });
        }
    }
    rules
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Player {
    pub name: String,
    /// The day of the last claim.
    pub last_day: Option<i64>,
    /// Days claimed in a row.
    pub streak: u32,
    pub played_secs: u64,
    /// Playtime rewards given, and how many times.
    pub given: BTreeMap<String, u64>,
}

/// Where a player stands today: may they claim, and which day of the row is it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Today {
    /// Claimable: the day of the row (1 = first).
    Open(u32),
    Claimed,
}

pub fn today(cfg: &Config, p: &Player, days: usize, now: u64) -> Today {
    let day = cfg.day(now);
    match p.last_day {
        Some(last) if last == day => Today::Claimed,
        Some(last) if last == day - 1 || !cfg.reset_missed => Today::Open(next_in_row(p.streak, days)),
        _ => Today::Open(1),
    }
}

/// Day `streak + 1` of a row of `days`, starting over after the last.
fn next_in_row(streak: u32, days: usize) -> u32 {
    if days == 0 { 1 } else { (streak % days as u32) + 1 }
}

/// Mark today's claim; returns the day of the row that was claimed.
pub fn claim(cfg: &Config, p: &mut Player, days: usize, now: u64) -> Option<u32> {
    let Today::Open(n) = today(cfg, p, days, now) else { return None };
    p.streak = if n == 1 { 1 } else { p.streak + 1 };
    p.last_day = Some(cfg.day(now));
    Some(n)
}

/// Playtime rewards due now: (index, times to give).
pub fn due(rules: &Rules, p: &Player) -> Vec<(usize, u64)> {
    let minutes = p.played_secs / 60;
    rules
        .playtime
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let have = p.given.get(&t.id).copied().unwrap_or(0);
            let earned = if t.repeat { minutes / t.minutes } else { u64::from(minutes >= t.minutes) };
            (earned > have).then_some((i, earned - have))
        })
        .collect()
}

pub const EXAMPLE: &str = r#"# Daily rewards: days = one reward for each day in a row (up to 27); after the last day the
# row starts again. Missing a day starts over at day 1 (reset-missed in config.toml).
# A reward: item (CustomItems id or minecraft:<item>) + count, money (Basics), commands (as the
# console, {player} = the player), text (shown in the window).
days = [
  { money = 50, text = "50 coins" },
  { money = 75, text = "75 coins" },
  { item = "minecraft:iron_ingot", count = 8 },
  { money = 100, text = "100 coins" },
  { item = "minecraft:golden_apple", count = 2 },
  { money = 150, text = "150 coins" },
  { item = "minecraft:diamond", count = 3, text = "3 diamonds" },
]

# Playtime: given once at `minutes` played, or every `minutes` with repeat = true.
[[playtime]]
id = "first_hour"
minutes = 60
money = 200
text = "200 coins for your first hour"

[[playtime]]
id = "every_30m"
minutes = 30
repeat = true
money = 20
"#;

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    #[test]
    fn the_example_reads_cleanly() {
        let mut notes = Vec::new();
        let r = parse(EXAMPLE, &mut notes);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(r.days.len(), 7);
        assert_eq!(r.playtime.len(), 2);
    }

    #[test]
    fn days_follow_the_server_clock() {
        let cfg = Config::default();
        // 16:59:59 UTC is 23:59:59 in Thailand; a second later it is the next day
        let t = 10 * DAY + 17 * 3600 - 1;
        assert_eq!(cfg.day(t) + 1, cfg.day(t + 1));
        assert_eq!(cfg.until_tomorrow(t), 1);
    }

    #[test]
    fn a_row_of_days() {
        let cfg = Config::default();
        let mut p = Player::default();
        let t0 = 100 * DAY;
        assert_eq!(today(&cfg, &p, 7, t0), Today::Open(1));
        assert_eq!(claim(&cfg, &mut p, 7, t0), Some(1));
        assert_eq!(claim(&cfg, &mut p, 7, t0 + 60), None);
        assert_eq!(today(&cfg, &p, 7, t0 + DAY), Today::Open(2));
        assert_eq!(claim(&cfg, &mut p, 7, t0 + DAY), Some(2));
        // A day missed: back to 1
        assert_eq!(today(&cfg, &p, 7, t0 + 3 * DAY), Today::Open(1));
        let lenient = Config { reset_missed: false, ..Config::default() };
        assert_eq!(today(&lenient, &p, 7, t0 + 3 * DAY), Today::Open(3));
        // After the last day the row starts again
        let mut q = Player { last_day: Some(cfg.day(t0)), streak: 7, ..Player::default() };
        assert_eq!(claim(&cfg, &mut q, 7, t0 + DAY), Some(1));
        assert_eq!(q.streak, 1);
    }

    #[test]
    fn playtime_rewards_come_due() {
        let mut notes = Vec::new();
        let r = parse(EXAMPLE, &mut notes);
        let mut p = Player { played_secs: 59 * 60, ..Player::default() };
        assert_eq!(due(&r, &p), vec![(1, 1)]);
        p.given.insert("every_30m".into(), 1);
        p.played_secs = 95 * 60;
        assert_eq!(due(&r, &p), vec![(0, 1), (1, 2)]);
        p.given.insert("first_hour".into(), 1);
        p.given.insert("every_30m".into(), 3);
        assert!(due(&r, &p).is_empty());
    }
}

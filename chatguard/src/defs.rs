//! ChatGuard's checks of one chat line: bad words (Thai and English, with the tricks people use
//! to slip them through), adverts for other servers, spam, shouting. No server needed here.
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

pub const NS: &str = "chatguard";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// `censor` stars bad words out; `block` stops the line.
    pub bad_words: String,
    /// Stop lines with server addresses or links that are not allowed.
    pub block_adverts: bool,
    /// Addresses players may write (your own server, your website).
    pub allowed_domains: Vec<String>,
    /// Seconds a player waits between two lines.
    pub cooldown: f64,
    /// The same line again within this many seconds is stopped.
    pub repeat_seconds: u32,
    /// Lines mostly in capitals (at least this long) go to small letters.
    pub caps_min_length: u32,
    /// A letter repeated more than this many times is cut down.
    pub max_repeat_letters: u32,
    /// Stopped lines within 10 minutes before `punish` runs (0 = never).
    pub strikes: u32,
    /// A console command for a player with too many strikes (`{player}`).
    pub punish: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: "th".into(),
            bad_words: "censor".into(),
            block_adverts: true,
            allowed_domains: vec!["mcsv.me".into()],
            cooldown: 1.0,
            repeat_seconds: 20,
            caps_min_length: 8,
            max_repeat_letters: 4,
            strikes: 0,
            punish: "mute {player} 5m".into(),
        }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        if self.bad_words != "block" && self.bad_words != "off" {
            self.bad_words = "censor".into();
        }
        self.cooldown = if self.cooldown.is_finite() { self.cooldown.clamp(0.0, 30.0) } else { 1.0 };
        self.allowed_domains = self.allowed_domains.iter().map(|d| d.trim().to_ascii_lowercase()).filter(|d| !d.is_empty()).collect();
        self.max_repeat_letters = self.max_repeat_letters.max(2);
        self
    }

    pub fn thai(&self) -> bool {
        self.language == "th"
    }
}

/// The words file: one word per line, `#` starts a note.
pub fn read_words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text.lines().map(|l| l.split('#').next().unwrap_or("").trim()).filter(|l| !l.is_empty()).map(squash).filter(|w| !w.is_empty()).collect();
    out.sort();
    out.dedup();
    out
}

/// What a word looks like once the tricks are taken off: small letters, look-alike digits as
/// letters, no spaces, dots or marks between letters, letters said once.
pub fn squash(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = None;
    for ch in text.chars().flat_map(char::to_lowercase) {
        let ch = match ch {
            '0' => 'o',
            '1' | '!' | '|' => 'i',
            '3' => 'e',
            '4' | '@' => 'a',
            '5' | '$' => 's',
            '7' => 't',
            other => other,
        };
        // Thai tone marks (and the like) are dropped, as are spaces and punctuation; vowels stay,
        // or different words would look the same
        let thai_mark = ('\u{0E47}'..='\u{0E4E}').contains(&ch);
        if !(ch.is_alphanumeric()) || thai_mark {
            continue;
        }
        if Some(ch) != last {
            out.push(ch);
        }
        last = Some(ch);
    }
    out
}

/// What a check decided.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// Send this line (maybe changed).
    Send(String),
    /// Stop it; why.
    Stop(Why),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    BadWords,
    Advert,
    TooFast,
    Repeat,
}

/// A player's recent lines, for spam.
#[derive(Clone, Debug, Default)]
pub struct Memory {
    /// (seconds, squashed line)
    pub recent: VecDeque<(f64, String)>,
    pub last: Option<f64>,
    /// Times of stopped lines.
    pub strikes: VecDeque<f64>,
}

/// Is this a server address or link that is not allowed?
pub fn advert(cfg: &Config, line: &str) -> bool {
    let lower = line.to_lowercase();
    // Dots written as words or spaces
    let joined = lower.replace(" dot ", ".").replace("(dot)", ".").replace("[dot]", ".").replace(" . ", ".").replace(", ", ".");
    for word in joined.split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')') {
        let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if word.is_empty() || !word.contains('.') {
            continue;
        }
        let host = word.split(['/', ':']).next().unwrap_or(word);
        let host = host.strip_prefix("www.").unwrap_or(host);
        // An IP address
        let parts: Vec<&str> = host.split('.').collect();
        if parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit())) {
            return true;
        }
        let Some(tld) = parts.last() else { continue };
        let named = parts.len() >= 2 && parts.iter().all(|p| !p.is_empty()) && TLDS.contains(tld);
        if named && !cfg.allowed_domains.iter().any(|d| host == d || host.ends_with(&format!(".{d}"))) {
            return true;
        }
    }
    false
}

const TLDS: [&str; 26] = ["com", "net", "org", "me", "gg", "io", "th", "co", "xyz", "club", "online", "fun", "top", "pro", "site", "store", "live", "cc", "us", "uk", "asia", "info", "biz", "dev", "app", "games"];

/// Star out bad words where they are written plainly; `None` when one is hidden (spaced out or
/// with look-alike letters) and cannot be starred in place.
fn censor(words: &[String], line: &str) -> Option<String> {
    let mut out = line.to_owned();
    let lower: Vec<char> = line.chars().flat_map(char::to_lowercase).collect();
    if lower.len() != line.chars().count() {
        return None;
    }
    for w in words {
        let w: Vec<char> = w.chars().collect();
        let mut i = 0;
        while i + w.len() <= lower.len() {
            if lower[i..i + w.len()] == w[..] {
                let mut chars: Vec<char> = out.chars().collect();
                for c in chars.iter_mut().skip(i).take(w.len()) {
                    *c = '*';
                }
                out = chars.into_iter().collect();
                i += w.len();
            } else {
                i += 1;
            }
        }
    }
    let still = squash(&out);
    if words.iter().any(|w| still.contains(w.as_str())) { None } else { Some(out) }
}

/// Check one line of a player at `now` (seconds).
pub fn check(cfg: &Config, words: &[String], mem: &mut Memory, line: &str, now: f64, staff: bool) -> Verdict {
    let squashed = squash(line);
    if !staff {
        if mem.last.is_some_and(|t| now - t < cfg.cooldown) {
            return Verdict::Stop(Why::TooFast);
        }
        mem.recent.retain(|(t, _)| now - t < f64::from(cfg.repeat_seconds));
        if cfg.repeat_seconds > 0 && !squashed.is_empty() && mem.recent.iter().any(|(_, s)| *s == squashed) {
            return Verdict::Stop(Why::Repeat);
        }
        if cfg.block_adverts && advert(cfg, line) {
            return Verdict::Stop(Why::Advert);
        }
    }
    let mut text = line.to_owned();
    if cfg.bad_words != "off" && words.iter().any(|w| squashed.contains(w.as_str())) {
        match (cfg.bad_words.as_str(), censor(words, &text)) {
            ("censor", Some(starred)) => text = starred,
            _ => return Verdict::Stop(Why::BadWords),
        }
    }
    if !staff {
        // Shouting: mostly capitals
        let letters: Vec<char> = text.chars().filter(|c| c.is_ascii_alphabetic()).collect();
        let caps = letters.iter().filter(|c| c.is_ascii_uppercase()).count();
        if letters.len() >= cfg.caps_min_length as usize && caps * 10 > letters.len() * 7 {
            text = text.to_lowercase();
        }
        text = cut_repeats(&text, cfg.max_repeat_letters as usize);
        mem.last = Some(now);
        mem.recent.push_back((now, squashed));
        while mem.recent.len() > 20 {
            mem.recent.pop_front();
        }
    }
    Verdict::Send(text)
}

/// `heyyyyyyy` → `heyyyy` (at most `max` of one letter in a row).
pub fn cut_repeats(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = None;
    let mut run = 0;
    for ch in text.chars() {
        if Some(ch) == last {
            run += 1;
        } else {
            run = 1;
            last = Some(ch);
        }
        if run <= max {
            out.push(ch);
        }
    }
    out
}

/// Note a stopped line; true when the player has `cfg.strikes` within 10 minutes.
pub fn strike(cfg: &Config, mem: &mut Memory, now: f64) -> bool {
    mem.strikes.retain(|t| now - t < 600.0);
    mem.strikes.push_back(now);
    if cfg.strikes > 0 && mem.strikes.len() >= cfg.strikes as usize {
        mem.strikes.clear();
        return true;
    }
    false
}

/// The words a new server starts with (owners change `words.txt`).
pub const WORDS: &str = "# One word per line. Spaces, dots, repeated letters, Thai tone marks and look-alike digits
# (4 = a, 3 = e, 0 = o …) are seen through, so write each word plainly once.
fuck
shit
bitch
cunt
dick
pussy
asshole
motherfucker
nigger
nigga
ควย
เหี้ย
สัส
เย็ด
แตด
ส้นตีน
อีดอก
อีเวร
ไอ้สัตว์
พ่อมึงตาย
แม่มึงตาย
ระยำ
";

#[cfg(test)]
mod tests {
    use super::*;

    fn words() -> Vec<String> {
        read_words(WORDS)
    }

    #[test]
    fn tricks_are_seen_through() {
        assert_eq!(squash("F u c k"), "fuck");
        assert_eq!(squash("fuuuuck"), "fuck");
        assert_eq!(squash("sh1t"), "shit");
        assert_eq!(squash("f.u.c.k"), "fuck");
        // Thai with spaces and tone marks
        assert_eq!(squash("ค ว ย"), "ควย");
        assert_eq!(squash("เหี้้ยยย"), squash("เหี้ย"));
    }

    #[test]
    fn bad_words_are_starred_or_stopped() {
        let cfg = Config::default();
        let mut m = Memory::default();
        assert_eq!(check(&cfg, &words(), &mut m, "what the fuck man", 0.0, false), Verdict::Send("what the **** man".into()));
        assert_eq!(check(&cfg, &words(), &mut m, "f u c k you", 10.0, false), Verdict::Stop(Why::BadWords));
        assert_eq!(check(&cfg, &words(), &mut m, "ไอ้ควย", 20.0, false), Verdict::Send("ไอ้***".into()));
        let block = Config { bad_words: "block".into(), ..Config::default() };
        assert_eq!(check(&block, &words(), &mut Memory::default(), "fuck", 0.0, false), Verdict::Stop(Why::BadWords));
        // Ordinary words that hold a short word inside are a known risk of a word list; these pass
        for fine in ["hello there", "เปิดหีบ", "สัดส่วน", "ควายตัวใหญ่", "เหยื่อตกปลา"] {
            assert_eq!(check(&cfg, &words(), &mut Memory::default(), fine, 0.0, false), Verdict::Send(fine.into()), "{fine}");
        }
    }

    #[test]
    fn adverts_are_stopped_but_our_own_address_is_fine() {
        let cfg = Config::default().cleaned();
        assert!(advert(&cfg, "join play.otherserver.net now"));
        assert!(advert(&cfg, "ip 203.0.113.5"));
        assert!(advert(&cfg, "otherserver dot com"));
        assert!(!advert(&cfg, "see mcsv.me"));
        assert!(!advert(&cfg, "panel.mcsv.me/servers"));
        assert!(!advert(&cfg, "version 1.21.4 is out"));
        assert!(!advert(&cfg, "wait... what"));
    }

    #[test]
    fn spam_and_shouting() {
        let cfg = Config::default();
        let w = words();
        let mut m = Memory::default();
        assert!(matches!(check(&cfg, &w, &mut m, "hi", 0.0, false), Verdict::Send(_)));
        assert_eq!(check(&cfg, &w, &mut m, "hello", 0.5, false), Verdict::Stop(Why::TooFast));
        assert_eq!(check(&cfg, &w, &mut m, "h i", 5.0, false), Verdict::Stop(Why::Repeat));
        assert_eq!(check(&cfg, &w, &mut m, "HELLO EVERYBODY", 30.0, false), Verdict::Send("hello everybody".into()));
        assert_eq!(check(&cfg, &w, &mut m, "nooooooooo", 40.0, false), Verdict::Send("noooo".into()));
        // Staff are not slowed or shortened
        assert!(matches!(check(&cfg, &w, &mut m, "x", 40.1, true), Verdict::Send(_)));
    }

    #[test]
    fn strikes_add_up() {
        let cfg = Config { strikes: 3, ..Config::default() };
        let mut m = Memory::default();
        assert!(!strike(&cfg, &mut m, 0.0));
        assert!(!strike(&cfg, &mut m, 1.0));
        assert!(strike(&cfg, &mut m, 2.0));
        assert!(!strike(&cfg, &mut m, 700.0));
    }
}

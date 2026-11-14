//! Which message the server list shows and who gets in during maintenance.
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// Messages for the server list, `&` colours, `\n` between the two lines.
    /// `{online}` and `{max}` are filled in. Empty list = leave the server's own message.
    pub motd: Vec<String>,
    /// Seconds each message stays before the next one takes over.
    pub rotate_seconds: u64,
    /// Shown in the server list while maintenance is on.
    pub maintenance_motd: String,
    /// Shown to a player who is refused during maintenance.
    pub maintenance_kick: String,
    /// Ops of this level and above may join during maintenance.
    pub maintenance_op_level: u8,
    /// Players already online who may not stay are kicked when maintenance starts.
    pub kick_on_enable: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: "th".into(),
            motd: Vec::new(),
            rotate_seconds: 10,
            maintenance_motd: "&cปิดปรับปรุงชั่วคราว\n&7เดี๋ยวกลับมาเปิดอีกครั้ง".into(),
            maintenance_kick: "เซิร์ฟเวอร์ปิดปรับปรุงอยู่ ลองเข้าใหม่ภายหลัง".into(),
            maintenance_op_level: 2,
            kick_on_enable: true,
        }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Self {
        self.rotate_seconds = self.rotate_seconds.clamp(1, 3600);
        self.maintenance_op_level = self.maintenance_op_level.clamp(1, 4);
        self.motd = self.motd.into_iter().filter(|m| !m.trim().is_empty()).take(50).map(|m| two_lines(&m)).collect();
        self.maintenance_motd = two_lines(&self.maintenance_motd);
        self
    }

    pub fn thai(&self) -> bool {
        self.language != "en"
    }
}

/// The server list has two lines; `\n` typed in a config file is a line break.
pub fn two_lines(text: &str) -> String {
    let text = text.replace("\\n", "\n");
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    match lines.next() {
        Some(second) => format!("{first}\n{second}"),
        None => first.to_owned(),
    }
}

/// Maintenance: on or off, until when, and who is let in besides ops.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Maintenance {
    #[serde(default)]
    pub on: bool,
    /// Epoch second at which it turns itself off (0 = stays until switched off).
    #[serde(default)]
    pub until: u64,
    /// Lowercase names.
    #[serde(default)]
    pub allowed: Vec<String>,
}

impl Maintenance {
    /// On right now? Also switches itself off once its time is up (returns true when it just did).
    pub fn expire(&mut self, now: u64) -> bool {
        if self.on && self.until > 0 && now >= self.until {
            self.on = false;
            self.until = 0;
            return true;
        }
        false
    }

    pub fn lets_in(&self, name: &str, op_level: u8, needed_level: u8) -> bool {
        !self.on || op_level >= needed_level || self.allowed.iter().any(|a| a == &name.to_lowercase())
    }

    pub fn allow(&mut self, name: &str) -> bool {
        let n = name.to_lowercase();
        if self.allowed.contains(&n) {
            return false;
        }
        self.allowed.push(n);
        self.allowed.sort();
        true
    }

    pub fn disallow(&mut self, name: &str) -> bool {
        let n = name.to_lowercase();
        let before = self.allowed.len();
        self.allowed.retain(|a| a != &n);
        before != self.allowed.len()
    }
}

/// The message to show now, or `None` to leave the server's own.
pub fn current_motd(cfg: &Config, state: &Maintenance, now: u64, online: u32, max: u32) -> Option<String> {
    let text = if state.on {
        cfg.maintenance_motd.clone()
    } else if cfg.motd.is_empty() {
        return None;
    } else {
        cfg.motd[((now / cfg.rotate_seconds) % cfg.motd.len() as u64) as usize].clone()
    };
    Some(text.replace("{online}", &online.to_string()).replace("{max}", &max.to_string()))
}

/// `30s`, `10m`, `2h`, `1d`, and sums like `1h30m`.
pub fn parse_duration(text: &str) -> Option<u64> {
    let mut total = 0u64;
    let mut digits = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        let n: u64 = digits.parse().ok()?;
        digits.clear();
        total = total.checked_add(n.checked_mul(match ch {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return None,
        })?)?;
    }
    (digits.is_empty() && total > 0 && total <= 30 * 86_400).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motd_rotates_and_maintenance_wins() {
        let cfg = Config { motd: vec!["&aone {online}/{max}".into(), "two\\nline2\\nline3".into(), "  ".into()], rotate_seconds: 10, ..Config::default() }.cleaned();
        assert_eq!(cfg.motd.len(), 2);
        let off = Maintenance::default();
        assert_eq!(current_motd(&cfg, &off, 5, 3, 20).as_deref(), Some("&aone 3/20"));
        assert_eq!(current_motd(&cfg, &off, 15, 3, 20).as_deref(), Some("two\nline2"));
        assert_eq!(current_motd(&cfg, &off, 25, 3, 20).as_deref(), Some("&aone 3/20"));
        let on = Maintenance { on: true, ..Maintenance::default() };
        assert!(current_motd(&cfg, &on, 5, 3, 20).unwrap().contains("ปิดปรับปรุง"));
        // No messages configured: the server keeps its own, but maintenance still shows
        let plain = Config::default().cleaned();
        assert_eq!(current_motd(&plain, &off, 5, 0, 0), None);
        assert!(current_motd(&plain, &on, 5, 0, 0).is_some());
    }

    #[test]
    fn maintenance_door() {
        let mut m = Maintenance::default();
        assert!(m.lets_in("Steve", 0, 2));
        m.on = true;
        assert!(!m.lets_in("Steve", 0, 2) && !m.lets_in("Steve", 1, 2));
        assert!(m.lets_in("Admin", 2, 2) && m.lets_in("Owner", 4, 2));
        assert!(m.allow("Steve") && !m.allow("STEVE"));
        assert!(m.lets_in("sTeVe", 0, 2));
        assert!(m.disallow("steve") && !m.disallow("steve"));
        assert!(!m.lets_in("Steve", 0, 2));
        // A timed one ends by itself, once
        m.until = 100;
        assert!(!m.expire(99) && m.on);
        assert!(m.expire(100) && !m.on && m.until == 0);
        assert!(!m.expire(200));
        assert!(m.lets_in("Steve", 0, 2));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30s"), Some(30));
        assert_eq!(parse_duration("1h30m"), Some(5400));
        assert_eq!(parse_duration("2d"), Some(172_800));
        for bad in ["", "10", "m", "5x", "0s", "999d", "99999999999999999999h"] {
            assert_eq!(parse_duration(bad), None, "{bad}");
        }
        let cfg: Config = toml::from_str("rotate-seconds = 0\nmaintenance-op-level = 9").unwrap();
        let cfg = cfg.cleaned();
        assert_eq!((cfg.rotate_seconds, cfg.maintenance_op_level), (1, 4));
    }
}

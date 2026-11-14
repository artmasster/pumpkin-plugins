//! Accounts, password rules, hashing and attempt limits. No server types here.
use std::collections::{BTreeMap, HashMap};

use argon2::{Algorithm, Argon2, Params, Version};
use serde::{Deserialize, Serialize};

/// Argon2id cost. OWASP's second recommended setting (19 MiB, 2 passes, 1 lane).
/// Every stored hash names its own cost, so raising these only affects new hashes;
/// old ones are replaced the next time their owner logs in.
pub const MEM_KIB: u32 = 19 * 1024;
pub const PASSES: u32 = 2;
const HASH_LEN: usize = 32;
pub const MAX_PASSWORD: usize = 64;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// Players without an account must register before they can play.
    pub force_register: bool,
    pub min_password: usize,
    /// `/register <password> <password>`.
    pub confirm_password: bool,
    /// Seconds a player may stay without logging in before being kicked.
    pub login_timeout: u64,
    /// Seconds between reminders in chat.
    pub remind_interval: u64,
    /// Wrong passwords in one visit before the player is kicked.
    pub max_tries: u32,
    /// Twice `max-tries` wrong passwords for one name from one address in ten minutes: that name
    /// is refused from that address this long.
    pub lock_minutes: u64,
    /// Stay logged in from the same address this long after leaving (0 = always ask).
    pub session_minutes: u64,
    /// Accounts one address may register (0 = no limit; use 0 behind a proxy).
    pub max_accounts_per_ip: u32,
    /// Java names must be 3-16 of A-Z a-z 0-9 _.
    pub strict_names: bool,
    /// Bedrock players skip the password (only safe while Bedrock online mode is on).
    pub bedrock_auto_login: bool,
    /// Commands a player may use before logging in, besides login and register.
    pub allowed_commands: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: "th".into(),
            force_register: true,
            min_password: 6,
            confirm_password: true,
            login_timeout: 60,
            remind_interval: 8,
            max_tries: 5,
            lock_minutes: 10,
            session_minutes: 0,
            max_accounts_per_ip: 3,
            strict_names: true,
            bedrock_auto_login: false,
            allowed_commands: Vec::new(),
        }
    }
}

impl Config {
    /// Keep every number in a range the plugin can live with.
    pub fn cleaned(mut self) -> Self {
        self.min_password = self.min_password.clamp(4, 32);
        self.login_timeout = self.login_timeout.clamp(15, 600);
        self.remind_interval = self.remind_interval.clamp(3, 60);
        self.max_tries = self.max_tries.clamp(1, 20);
        self.lock_minutes = self.lock_minutes.min(24 * 60);
        self.session_minutes = self.session_minutes.min(30 * 24 * 60);
        self.allowed_commands = self
            .allowed_commands
            .iter()
            .map(|c| c.trim().trim_start_matches('/').to_lowercase())
            .filter(|c| !c.is_empty())
            .collect();
        self
    }

    pub fn thai(&self) -> bool {
        self.language != "en"
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Account {
    /// The name as first registered. Other spellings of it are refused at the door.
    pub name: String,
    pub hash: String,
    #[serde(default)]
    pub reg_ip: String,
    #[serde(default)]
    pub reg_at: u64,
    #[serde(default)]
    pub last_ip: String,
    #[serde(default)]
    pub last_login: u64,
}

/// Accounts by lowercase name (sorted, so the file on disk is stable).
pub type Accounts = BTreeMap<String, Account>;

pub fn key_of(name: &str) -> String {
    name.to_lowercase()
}

/// Java account names: what vanilla allows.
pub fn valid_name(name: &str) -> bool {
    (3..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `1.2.3.4:5678` and `[::1]:5678` without the port.
pub fn ip_of(addr: &str) -> String {
    if let Some(rest) = addr.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest).to_owned();
    }
    match addr.rsplit_once(':') {
        // A bare IPv6 address has more than one colon and no port to cut
        Some((host, port)) if !host.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) => host.to_owned(),
        _ => addr.to_owned(),
    }
}

/// Passwords people pick first. Refused whatever the length setting says.
const WEAK: &[&str] = &[
    "123456", "1234567", "12345678", "123456789", "1234567890", "password", "password1", "qwerty", "qwerty123",
    "111111", "000000", "123123", "abc123", "654321", "666666", "112233", "121212", "asdfgh", "asdfghjkl",
    "zxcvbnm", "iloveyou", "minecraft", "letmein", "admin123", "aaaaaa", "1q2w3e4r", "qazwsx", "passw0rd",
];

/// Why a new password is not acceptable, as a message key.
pub fn password_problem(password: &str, name: &str, min: usize) -> Option<&'static str> {
    let chars = password.chars().count();
    if chars < min {
        return Some("pw-short");
    }
    if password.len() > MAX_PASSWORD {
        return Some("pw-long");
    }
    if password.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Some("pw-chars");
    }
    let lower = password.to_lowercase();
    if lower == name.to_lowercase() {
        return Some("pw-name");
    }
    if WEAK.contains(&lower.as_str()) || password.chars().all(|c| Some(c) == password.chars().next()) {
        return Some("pw-weak");
    }
    None
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

fn derive(password: &str, salt: &[u8], mem_kib: u32, passes: u32) -> Option<[u8; HASH_LEN]> {
    let params = Params::new(mem_kib, passes, 1, Some(HASH_LEN)).ok()?;
    let mut out = [0u8; HASH_LEN];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params).hash_password_into(password.as_bytes(), salt, &mut out).ok()?;
    Some(out)
}

/// `argon2id$<memory KiB>$<passes>$<salt hex>$<hash hex>`
pub fn hash_password(password: &str, salt: &[u8; 16]) -> Option<String> {
    hash_with(password, salt, MEM_KIB, PASSES)
}

pub fn hash_with(password: &str, salt: &[u8; 16], mem_kib: u32, passes: u32) -> Option<String> {
    let hash = derive(password, salt, mem_kib, passes)?;
    Some(format!("argon2id${mem_kib}${passes}${}${}", hex(salt), hex(&hash)))
}

/// AuthMe's default format: `$SHA$<salt>$<hex sha256(hex sha256(password) + salt)>`.
fn verify_authme_sha(password: &str, stored: &str) -> bool {
    use sha2::{Digest, Sha256};
    let mut parts = stored.split('$');
    let (Some(""), Some("SHA"), Some(salt), Some(want), None) = (parts.next(), parts.next(), parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let inner = hex(&Sha256::digest(password.as_bytes()));
    let got = hex(&Sha256::digest(format!("{inner}{salt}").as_bytes()));
    got.len() == want.len() && got.bytes().zip(want.to_lowercase().bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// A hash this plugin can check: its own, or one imported from AuthMe.
pub fn known_hash(stored: &str) -> bool {
    stored.starts_with("argon2id$") || stored.starts_with("$SHA$")
}

/// Accounts from an AuthMe export: one account per line as `name:hash[:more]` (the old flat file)
/// or `name,hash`. Returns the accounts and how many lines were skipped (blank, bad name, or a
/// hash kind we cannot check).
pub fn parse_import(text: &str) -> (Vec<(String, String)>, usize) {
    let mut out = Vec::new();
    let mut skipped = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let pair = line.split_once(':').or_else(|| line.split_once(','));
        let Some((name, rest)) = pair else {
            skipped += 1;
            continue;
        };
        let hash = rest.split([':', ',']).next().unwrap_or("").trim().trim_matches('"');
        let name = name.trim().trim_matches('"');
        if valid_name(name) && hash.starts_with("$SHA$") && hash.len() <= 200 {
            out.push((name.to_owned(), hash.to_owned()));
        } else {
            skipped += 1;
        }
    }
    (out, skipped)
}

/// Constant-time check of a password against a stored string.
pub fn verify_password(password: &str, stored: &str) -> bool {
    if stored.starts_with("$SHA$") {
        return verify_authme_sha(password, stored);
    }
    let mut parts = stored.split('$');
    let (Some("argon2id"), Some(mem), Some(passes), Some(salt), Some(hash), None) =
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Ok(mem), Ok(passes), Some(salt), Some(want)) = (mem.parse::<u32>(), passes.parse::<u32>(), unhex(salt), unhex(hash))
    else {
        return false;
    };
    // A corrupted or hostile file must not make the server burn memory or time
    if !(8..=262_144).contains(&mem) || !(1..=10).contains(&passes) || want.len() != HASH_LEN {
        return false;
    }
    let Some(got) = derive(password, &salt, mem, passes) else { return false };
    got.iter().zip(&want).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// True when the stored hash was made with weaker settings than today's (or is an imported one).
pub fn needs_rehash(stored: &str) -> bool {
    if !stored.starts_with("argon2id$") {
        return true;
    }
    let mut parts = stored.split('$').skip(1);
    let mem = parts.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
    let passes = parts.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
    mem < MEM_KIB || passes < PASSES
}

/// Fresh random salt from the system.
pub fn new_salt() -> Option<[u8; 16]> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).ok()?;
    Some(salt)
}

/// How many accounts were registered from an address.
pub fn accounts_from(accounts: &Accounts, ip: &str) -> usize {
    accounts.values().filter(|a| a.reg_ip == ip).count()
}

const FAIL_WINDOW: u64 = 600;

/// Key of a lock: one name from one address. Per address alone would let one guesser shut out
/// everybody behind the same carrier NAT; per name alone would let anyone lock the owner out.
pub fn guard_key(ip: &str, lower_name: &str) -> String {
    format!("{ip}|{lower_name}")
}

/// Wrong passwords per `guard_key`, and the keys refused because of them.
#[derive(Default)]
pub struct Guard {
    fails: HashMap<String, (u32, u64)>,
    locked: HashMap<String, u64>,
}

impl Guard {
    /// Record a wrong password. True when this one locks the address.
    pub fn fail(&mut self, ip: &str, now: u64, limit: u32, lock_secs: u64) -> bool {
        let entry = self.fails.entry(ip.to_owned()).or_insert((0, now));
        if now.saturating_sub(entry.1) > FAIL_WINDOW {
            *entry = (0, now);
        }
        entry.0 += 1;
        if entry.0 >= limit && lock_secs > 0 {
            self.fails.remove(ip);
            self.locked.insert(ip.to_owned(), now + lock_secs);
            return true;
        }
        false
    }

    /// Seconds this address is still refused.
    pub fn locked(&self, ip: &str, now: u64) -> Option<u64> {
        self.locked.get(ip).filter(|until| **until > now).map(|until| until - now)
    }

    pub fn clear(&mut self, ip: &str) {
        self.fails.remove(ip);
    }

    /// Lift locks by hand for a (lowercase) name from every address; `all` lifts every lock.
    /// Returns how many were lifted.
    pub fn unlock(&mut self, lower_name: &str) -> usize {
        let before = self.locked.len();
        if lower_name == "all" {
            self.locked.clear();
            self.fails.clear();
        } else {
            let mine = |k: &String| k.rsplit_once('|').is_some_and(|(_, n)| n == lower_name);
            self.locked.retain(|k, _| !mine(k));
            self.fails.retain(|k, _| !mine(k));
        }
        before - self.locked.len()
    }

    pub fn prune(&mut self, now: u64) {
        self.locked.retain(|_, until| *until > now);
        self.fails.retain(|_, (_, first)| now.saturating_sub(*first) <= FAIL_WINDOW);
    }

    pub fn locked_count(&self) -> usize {
        self.locked.len()
    }
}

/// Hashing is the one expensive thing a stranger can make the server do: at most
/// `per_sec` password checks a second for the whole server, with a small burst.
pub struct Budget {
    tokens: f64,
    last_ms: u64,
    per_sec: f64,
    burst: f64,
}

impl Budget {
    pub fn new(per_sec: f64, burst: f64) -> Self {
        Self { tokens: burst, last_ms: 0, per_sec, burst }
    }

    pub fn take(&mut self, now_ms: u64) -> bool {
        let gap = now_ms.saturating_sub(self.last_ms) as f64 / 1000.0;
        self.last_ms = now_ms;
        self.tokens = (self.tokens + gap * self.per_sec).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// A remembered login: same name from the same address until it runs out.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub ip: String,
    pub until: u64,
}

pub fn session_ok(session: Option<&Session>, ip: &str, now: u64) -> bool {
    session.is_some_and(|s| s.ip == ip && s.until > now)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Small cost so the tests stay fast; the format and the checks are the same
    fn quick(password: &str) -> String {
        hash_with(password, &[7u8; 16], 64, 1).unwrap()
    }

    #[test]
    fn hash_round_trip_and_tamper() {
        let stored = quick("correct horse");
        assert!(stored.starts_with("argon2id$64$1$"));
        assert!(verify_password("correct horse", &stored));
        assert!(!verify_password("correct horsf", &stored));
        assert!(!verify_password("", &stored));
        // Broken or hostile stored strings never verify and never run a huge hash
        for bad in ["", "plain", "argon2id$64$1$zz$00", "argon2id$99999999$1$00$00", "sha256$1$2$3$4", &format!("{stored}$x")] {
            assert!(!verify_password("correct horse", bad), "{bad}");
        }
        let mut cut = stored.clone();
        cut.truncate(stored.len() - 2);
        assert!(!verify_password("correct horse", &cut));
    }

    #[test]
    fn real_cost_hash_verifies() {
        let stored = hash_password("pässwörd-ไทย", &[1u8; 16]).unwrap();
        assert!(verify_password("pässwörd-ไทย", &stored));
        assert!(!needs_rehash(&stored));
        assert!(needs_rehash(&quick("x")));
    }

    #[test]
    fn authme_accounts_come_over() {
        // sha256(sha256("hunter22x") + "a1b2c3d4e5f60718"), the way AuthMe stores it
        use sha2::{Digest, Sha256};
        let inner = hex(&Sha256::digest(b"hunter22x"));
        let stored = format!("$SHA$a1b2c3d4e5f60718${}", hex(&Sha256::digest(format!("{inner}a1b2c3d4e5f60718").as_bytes())));
        assert!(verify_password("hunter22x", &stored));
        assert!(!verify_password("hunter22y", &stored));
        assert!(!verify_password("hunter22x", "$SHA$salt$deadbeef"));
        assert!(!verify_password("hunter22x", "$SHA$onlytwo"));
        assert!(needs_rehash(&stored) && known_hash(&stored) && !known_hash("$2a$10$abc"));
        let text = format!("# export\nSteve:{stored}:127.0.0.1:1700000000:0:0:0:world:a@b.c\n\"Alex\",\"{stored}\"\nbad name:{stored}\nBob:$2a$10$zzz\nnocolon\n\n");
        let (accounts, skipped) = parse_import(&text);
        assert_eq!(accounts, vec![("Steve".to_owned(), stored.clone()), ("Alex".to_owned(), stored)]);
        assert_eq!(skipped, 3);
    }

    #[test]
    fn password_rules() {
        assert_eq!(password_problem("abc", "Steve", 6), Some("pw-short"));
        assert_eq!(password_problem(&"x".repeat(65), "Steve", 6), Some("pw-long"));
        assert_eq!(password_problem("has space1", "Steve", 6), Some("pw-chars"));
        assert_eq!(password_problem("STEVE1", "steve1", 6), Some("pw-name"));
        assert_eq!(password_problem("Password", "Steve", 6), Some("pw-weak"));
        assert_eq!(password_problem("zzzzzzzz", "Steve", 6), Some("pw-weak"));
        assert_eq!(password_problem("รหัสลับของฉัน", "Steve", 6), None);
        assert_eq!(password_problem("tr0ub4dor", "Steve", 6), None);
    }

    #[test]
    fn names_and_addresses() {
        assert!(valid_name("Steve_01"));
        for bad in ["ab", "seventeen_letters", "sp ace", "ชื่อไทย", "a.b.c", ""] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert_eq!(ip_of("203.0.113.9:51234"), "203.0.113.9");
        assert_eq!(ip_of("[2001:db8::1]:25565"), "2001:db8::1");
        assert_eq!(ip_of("2001:db8::1"), "2001:db8::1");
        assert_eq!(ip_of("203.0.113.9"), "203.0.113.9");
        assert_eq!(key_of("StEvE"), "steve");
    }

    #[test]
    fn guard_locks_and_forgets() {
        let mut g = Guard::default();
        for _ in 0..9 {
            assert!(!g.fail("1.1.1.1", 100, 10, 600));
        }
        assert!(g.locked("1.1.1.1", 100).is_none());
        assert!(g.fail("1.1.1.1", 100, 10, 600));
        assert_eq!(g.locked("1.1.1.1", 400), Some(300));
        assert!(g.locked("1.1.1.1", 700).is_none());
        assert!(g.locked("2.2.2.2", 100).is_none());
        // Failures older than the window do not add up
        let mut g = Guard::default();
        for i in 0..30 {
            assert!(!g.fail("3.3.3.3", i * 700, 2, 600));
        }
        // A success wipes the count
        let mut g = Guard::default();
        g.fail("4.4.4.4", 1, 2, 600);
        g.clear("4.4.4.4");
        assert!(!g.fail("4.4.4.4", 2, 2, 600));
        assert!(g.fail("4.4.4.4", 3, 2, 600));
        assert_eq!(g.unlock("4.4.4.4"), 0);
        assert!(g.locked("4.4.4.4", 4).is_some());
        // Locks are per name and address: unlocking a name frees it everywhere, others stay
        let (a, b, other) = (guard_key("4.4.4.4", "steve"), guard_key("6.6.6.6", "steve"), guard_key("4.4.4.4", "alex"));
        for k in [&a, &b, &other] {
            g.fail(k, 5, 1, 600);
        }
        assert_eq!(g.unlock("steve"), 2);
        assert!(g.locked(&a, 6).is_none() && g.locked(&b, 6).is_none() && g.locked(&other, 6).is_some());
        assert_eq!(g.unlock("all"), 2);
        g.fail("5.5.5.5", 1, 1, 600);
        g.prune(10_000);
        assert_eq!(g.locked_count(), 0);
    }

    #[test]
    fn budget_limits_hashing() {
        let mut b = Budget::new(2.0, 3.0);
        assert!(b.take(1_000) && b.take(1_000) && b.take(1_000));
        assert!(!b.take(1_000));
        assert!(!b.take(1_400));
        assert!(b.take(1_900));
        assert!(!b.take(1_900));
        // A long quiet time never stores more than the burst
        let mut taken = 0;
        while b.take(100_000) {
            taken += 1;
        }
        assert_eq!(taken, 3);
    }

    #[test]
    fn sessions_and_limits() {
        let s = Session { ip: "1.1.1.1".into(), until: 500 };
        assert!(session_ok(Some(&s), "1.1.1.1", 499));
        assert!(!session_ok(Some(&s), "1.1.1.1", 500));
        assert!(!session_ok(Some(&s), "9.9.9.9", 10));
        assert!(!session_ok(None, "1.1.1.1", 10));
        let mut accounts = Accounts::new();
        for (i, ip) in ["1.1.1.1", "1.1.1.1", "2.2.2.2"].iter().enumerate() {
            accounts.insert(format!("p{i}"), Account { name: format!("P{i}"), hash: String::new(), reg_ip: (*ip).into(), reg_at: 0, last_ip: String::new(), last_login: 0 });
        }
        assert_eq!(accounts_from(&accounts, "1.1.1.1"), 2);
        assert_eq!(accounts_from(&accounts, "8.8.8.8"), 0);
    }

    #[test]
    fn config_is_cleaned_and_old_files_load() {
        let cfg: Config = toml::from_str("language = \"en\"\nmin-password = 1\nlogin-timeout = 5\nallowed-commands = [\"/Rules\", \" \", \"help\"]").unwrap();
        let cfg = cfg.cleaned();
        assert!(!cfg.thai());
        assert_eq!(cfg.min_password, 4);
        assert_eq!(cfg.login_timeout, 15);
        assert_eq!(cfg.allowed_commands, vec!["rules", "help"]);
        assert!(cfg.force_register && cfg.max_tries == 5);
    }
}

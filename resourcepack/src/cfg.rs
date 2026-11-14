//! ResourcePack settings and the small rules that need no server.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Pack {
    /// What it is called in commands: `a-z 0-9 _ -`.
    pub name: String,
    /// Direct download link of the .zip (`https://…`).
    pub url: String,
    /// SHA-1 of the file (40 hex characters). Empty = the plugin downloads the pack once and
    /// works it out, and checks again every `recheck-hours`.
    pub sha1: String,
    /// The player must accept it (overrides `force`).
    pub force: Option<bool>,
    /// Worlds it belongs to (`overworld`, `the_nether`, `the_end`); empty = everywhere.
    pub worlds: Vec<String>,
    /// Let it be sent (false = kept in the file but off).
    pub enabled: bool,
}

impl Default for Pack {
    fn default() -> Self {
        Pack { name: String::new(), url: String::new(), sha1: String::new(), force: None, worlds: Vec::new(), enabled: true }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// th | en
    pub language: String,
    /// Send the packs when a player joins.
    pub send_on_join: bool,
    /// Ticks to wait after the join before sending (0..=600; 20 = one second).
    pub join_delay_ticks: u64,
    /// Packs must be accepted, unless a pack says otherwise.
    pub force: bool,
    /// What the player reads on the "accept this pack?" screen (`&` colours).
    pub prompt: String,
    /// Kick a player who declines a required pack or cannot load it. Off = they are only told.
    pub kick_on_refuse: bool,
    pub kick_message: String,
    /// What a player who declined a pack that is not required is told (empty = nothing).
    pub declined_message: String,
    /// Kick a player who has not answered a required pack after this many seconds (0 = never).
    pub answer_timeout_seconds: u64,
    /// Hours after which the SHA-1 of a pack without `sha1` is worked out again (0 = only at
    /// start and on /rp hash). A pack that changed is sent again to everyone online.
    pub recheck_hours: u64,
    /// Largest pack the plugin will download to work out a SHA-1 (MB, 1..=500).
    pub max_pack_mb: u64,
    /// Let players use /rp to load the packs again themselves.
    pub players_can_reload: bool,
    pub packs: Vec<Pack>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: "th".into(),
            send_on_join: true,
            join_delay_ticks: 20,
            force: false,
            prompt: "&aเซิร์ฟเวอร์นี้มี resource pack &7กด ใช่ เพื่อโหลด".into(),
            kick_on_refuse: true,
            kick_message: "&cต้องยอมรับ resource pack ของเซิร์ฟเวอร์ก่อนจึงจะเข้าเล่นได้\n&7(ในรายชื่อเซิร์ฟเวอร์: แก้ไข > Resource Packs: เปิดใช้)".into(),
            declined_message: "&eคุณไม่ได้โหลด resource pack ของเซิร์ฟ &7พิมพ์ /rp เพื่อโหลดอีกครั้ง".into(),
            answer_timeout_seconds: 0,
            recheck_hours: 24,
            max_pack_mb: 250,
            players_can_reload: true,
            packs: vec![Pack { name: "example".into(), url: "https://example.com/pack.zip".into(), enabled: false, ..Pack::default() }],
        }
    }
}

/// Pack and world names: `a-z 0-9 _ -`, at most 32.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A download link the game client will take: `http(s)://host/…`, printable, at most 500 long.
pub fn valid_url(url: &str) -> bool {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"));
    let Some(rest) = rest else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    url.len() <= 500 && !host.is_empty() && !host.contains('@') && host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']')) && url.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'\\' | b'<' | b'>' | b'`'))
}

pub fn valid_sha1(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `minecraft:the_nether`, `nether` → `the_nether`.
pub fn world_name(raw: &str) -> String {
    let w = raw.trim().to_ascii_lowercase();
    match w.rsplit(':').next().unwrap_or("") {
        "world" | "overworld" => "overworld".into(),
        "nether" | "world_nether" | "the_nether" => "the_nether".into(),
        "end" | "world_the_end" | "the_end" => "the_end".into(),
        other => other.to_owned(),
    }
}

impl Config {
    /// Settings a server can run with, and one line per pack that was left out and why.
    pub fn cleaned(mut self) -> (Config, Vec<String>) {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.join_delay_ticks = self.join_delay_ticks.min(600);
        self.answer_timeout_seconds = self.answer_timeout_seconds.min(3600);
        self.recheck_hours = self.recheck_hours.min(24 * 30);
        self.max_pack_mb = self.max_pack_mb.clamp(1, 500);
        for text in [&mut self.prompt, &mut self.kick_message, &mut self.declined_message] {
            *text = text.chars().filter(|c| !c.is_control() || *c == '\n').take(400).collect();
        }
        let mut problems = Vec::new();
        let mut packs: Vec<Pack> = Vec::new();
        for mut p in std::mem::take(&mut self.packs) {
            p.name = p.name.trim().to_ascii_lowercase();
            p.url = p.url.trim().to_owned();
            p.sha1 = p.sha1.trim().to_ascii_lowercase();
            let shown: String = p.name.chars().filter(|c| !c.is_control()).take(32).collect();
            if !valid_name(&p.name) {
                problems.push(format!("pack \"{shown}\": the name must be a-z 0-9 _ - (at most 32)"));
            } else if packs.iter().any(|x| x.name == p.name) {
                problems.push(format!("pack \"{shown}\": listed twice, the second one is ignored"));
            } else if !valid_url(&p.url) {
                problems.push(format!("pack \"{shown}\": the url is not a usable http(s) link"));
            } else if !p.sha1.is_empty() && !valid_sha1(&p.sha1) {
                problems.push(format!("pack \"{shown}\": sha1 must be 40 hex characters (or empty to let the plugin work it out)"));
            } else if packs.len() >= 32 {
                problems.push(format!("pack \"{shown}\": more than 32 packs, ignored"));
            } else {
                let mut worlds: Vec<String> = Vec::new();
                for w in &p.worlds {
                    let w = world_name(w);
                    if valid_name(&w) && !worlds.contains(&w) {
                        worlds.push(w);
                    }
                }
                p.worlds = worlds;
                packs.push(p);
            }
        }
        self.packs = packs;
        (self, problems)
    }

    /// The packs a player in `world` should have, in config order.
    pub fn packs_for(&self, world: &str) -> Vec<&Pack> {
        let world = world_name(world);
        self.packs.iter().filter(|p| p.enabled && (p.worlds.is_empty() || p.worlds.contains(&world))).collect()
    }

    pub fn forced(&self, pack: &Pack) -> bool {
        pack.force.unwrap_or(self.force)
    }
}

/// The id a pack has on the client: made from its name, so it is the same after every restart
/// (the client then replaces the pack instead of stacking a second copy). Two 64-bit halves.
pub fn pack_id(name: &str) -> (u64, u64) {
    let fnv = |seed: u64| {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ seed;
        for b in name.bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    };
    // Version 4, variant 1: it looks like any other UUID
    ((fnv(0) & !0xF000) | 0x4000, (fnv(0x9E37_79B9_7F4A_7C15) & !(0xC000 << 48)) | (0x8000 << 48))
}

/// `8-4-4-4-12` hex, the way the client reports a pack id back.
pub fn id_text(id: (u64, u64)) -> String {
    let h = format!("{:016x}{:016x}", id.0, id.1);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// What the client says about a pack, from Pumpkin's status text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Accepted,
    Downloaded,
    Loaded,
    Declined,
    /// Could not download, bad link, or could not apply.
    Failed,
    Discarded,
    Other,
}

pub fn answer(status: &str) -> Answer {
    match status.to_ascii_lowercase().as_str() {
        "accepted" => Answer::Accepted,
        "downloaded" => Answer::Downloaded,
        "downloadsuccess" | "successfullyloaded" | "successfully_loaded" => Answer::Loaded,
        "declined" => Answer::Declined,
        "downloadfail" | "faileddownload" | "failed_download" | "invalidurl" | "invalid_url" | "reloadfailed" | "failedreload" | "failed_reload" => Answer::Failed,
        "discarded" => Answer::Discarded,
        _ => Answer::Other,
    }
}

impl Answer {
    /// The player still has to answer or the pack is still coming.
    pub fn waiting(self) -> bool {
        matches!(self, Answer::Accepted | Answer::Downloaded)
    }

    pub fn refused(self) -> bool {
        matches!(self, Answer::Declined | Answer::Failed)
    }

    pub fn label(self, th: bool) -> &'static str {
        match (self, th) {
            (Answer::Accepted, true) => "กำลังโหลด",
            (Answer::Accepted, false) => "accepted, downloading",
            (Answer::Downloaded, true) => "โหลดไฟล์แล้ว กำลังใช้",
            (Answer::Downloaded, false) => "downloaded, applying",
            (Answer::Loaded, true) => "ใช้อยู่",
            (Answer::Loaded, false) => "loaded",
            (Answer::Declined, true) => "ปฏิเสธ",
            (Answer::Declined, false) => "declined",
            (Answer::Failed, true) => "โหลดไม่สำเร็จ",
            (Answer::Failed, false) => "failed",
            (Answer::Discarded, true) => "ถูกถอดออก",
            (Answer::Discarded, false) => "discarded",
            (Answer::Other, true) => "ยังไม่ตอบ",
            (Answer::Other, false) => "no answer yet",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(text.contains("send-on-join = true") && text.contains("[[packs]]") && text.contains("recheck-hours = 24"), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        let (clean, problems) = back.cleaned();
        assert_eq!(clean, Config::default());
        assert!(problems.is_empty());
        // The example pack is off: a fresh install sends nothing
        assert!(clean.packs_for("overworld").is_empty());
    }

    #[test]
    fn packs_are_checked() {
        let text = r#"
language = "EN"
join-delay-ticks = 99999
max-pack-mb = 0
force = true
[[packs]]
name = "Main"
url = " https://cdn.example.com/main.zip "
[[packs]]
name = "nether"
url = "https://cdn.example.com/nether.zip"
sha1 = "A9993E364706816ABA3E25717850C26C9CD0D89D"
force = false
worlds = ["Nether", "minecraft:the_nether", "bad world"]
[[packs]]
name = "main"
url = "https://x.com/dup.zip"
[[packs]]
name = "bad name"
url = "https://x.com/a.zip"
[[packs]]
name = "ftp"
url = "ftp://x.com/a.zip"
[[packs]]
name = "hash"
url = "https://x.com/a.zip"
sha1 = "nope"
[[packs]]
name = "off"
url = "https://x.com/off.zip"
enabled = false
"#;
        let (c, problems) = toml::from_str::<Config>(text).unwrap().cleaned();
        assert_eq!((c.language.as_str(), c.join_delay_ticks, c.max_pack_mb), ("en", 600, 1));
        assert_eq!(c.packs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["main", "nether", "off"]);
        assert_eq!(c.packs[0].url, "https://cdn.example.com/main.zip");
        assert_eq!(c.packs[1].sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(c.packs[1].worlds, ["the_nether"]);
        assert_eq!(problems.len(), 4, "{problems:?}");
        // Who gets what
        assert_eq!(c.packs_for("minecraft:overworld").iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["main"]);
        assert_eq!(c.packs_for("minecraft:the_nether").iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["main", "nether"]);
        assert!(c.forced(&c.packs[0]) && !c.forced(&c.packs[1]));
    }

    #[test]
    fn links_and_names() {
        for ok in ["https://example.com/pack.zip", "http://1.2.3.4:8080/p.zip?x=1", "https://download.mc-packs.net/pack/abc.zip"] {
            assert!(valid_url(ok), "{ok}");
        }
        for bad in ["", "example.com/pack.zip", "file:///etc/passwd", "https://", "https://a b/p.zip", "https://u:p@x.com/p.zip", "https://x.com/\"p.zip", &format!("https://x.com/{}", "a".repeat(500))] {
            assert!(!valid_url(bad), "{bad}");
        }
        assert!(valid_name("main_pack-2") && !valid_name("Main") && !valid_name("") && !valid_name(&"a".repeat(33)));
        assert!(valid_sha1("da39a3ee5e6b4b0d3255bfef95601890afd80709") && !valid_sha1("da39") && !valid_sha1(&"g".repeat(40)));
    }

    #[test]
    fn ids_are_stable_and_distinct() {
        let a = pack_id("main");
        assert_eq!(a, pack_id("main"));
        assert_ne!(a, pack_id("main2"));
        let text = id_text(a);
        assert_eq!(text.len(), 36);
        assert_eq!(&text[14..15], "4");
        assert!(matches!(&text[19..20], "8" | "9" | "a" | "b"), "{text}");
        let ids: std::collections::HashSet<String> = (0..500).map(|i| id_text(pack_id(&format!("pack{i}")))).collect();
        assert_eq!(ids.len(), 500);
    }

    #[test]
    fn answers_are_understood() {
        // What Pumpkin 0.2.2 says (the Debug name of its enum)
        assert_eq!(answer("Accepted"), Answer::Accepted);
        assert_eq!(answer("Downloaded"), Answer::Downloaded);
        assert_eq!(answer("DownloadSuccess"), Answer::Loaded);
        assert_eq!(answer("Declined"), Answer::Declined);
        for failed in ["DownloadFail", "InvalidUrl", "ReloadFailed"] {
            assert_eq!(answer(failed), Answer::Failed, "{failed}");
        }
        assert_eq!(answer("Discarded"), Answer::Discarded);
        assert_eq!(answer("Unknown(9)"), Answer::Other);
        assert!(Answer::Accepted.waiting() && !Answer::Loaded.waiting());
        assert!(Answer::Declined.refused() && Answer::Failed.refused() && !Answer::Discarded.refused());
    }
}

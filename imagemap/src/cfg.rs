//! ImageMap settings and the small rules that need no server.
use serde::{Deserialize, Serialize};

use crate::picture::Mode;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// th | en
    pub language: String,
    /// Let every player make maps. Off = operators only (the server downloads whatever link it is given).
    pub players_can_create: bool,
    /// Maps one player may own (operators have no limit).
    pub player_max_maps: u32,
    /// Maps the server keeps in all (16 KB each).
    pub max_maps: u32,
    /// When someone joins, every map is sent again to everyone online, as long as there are
    /// at most this many (Pumpkin 0.2.2 cannot send a map to one player only).
    pub join_burst_maps: u32,
    /// Seconds between two maps of the slow round that re-sends every map in turn (0 = off).
    pub round_seconds: u32,
    /// Largest poster: maps per side (1..=16).
    pub max_poster_side: u32,
    /// With no size given, a picture becomes at most this many maps on its longer side.
    pub auto_max_side: u32,
    /// Largest picture file (MB, 1..=32) and largest picture (megapixels, 1..=40).
    pub max_image_mb: u32,
    pub max_megapixels: u32,
    /// Mix neighbouring colours to show shades a map does not have (better for photos; off suits pixel art).
    pub dither: bool,
    /// Seconds a player waits between two pictures (operators do not wait).
    pub cooldown_seconds: u64,
    /// Map numbers start here. Keep it far above the maps players craft, which count up from 0.
    pub first_map_id: i32,
    /// Maps broadcast per tick while a batch is being sent (1..=16).
    pub maps_per_tick: u32,
    /// Milliseconds of each tick a picture being made may use (1..=20).
    pub tick_ms: u32,
    /// Allow links to private addresses (192.168.x.x, localhost). Leave off on a public server.
    pub allow_private_hosts: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: "th".into(),
            players_can_create: false,
            player_max_maps: 50,
            max_maps: 400,
            join_burst_maps: 150,
            round_seconds: 1,
            max_poster_side: 8,
            auto_max_side: 3,
            max_image_mb: 8,
            max_megapixels: 16,
            dither: true,
            cooldown_seconds: 20,
            first_map_id: 100_000,
            maps_per_tick: 2,
            tick_ms: 4,
            allow_private_hosts: false,
        }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.player_max_maps = self.player_max_maps.clamp(1, 10_000);
        self.max_maps = self.max_maps.clamp(1, 20_000);
        self.join_burst_maps = self.join_burst_maps.min(5000);
        self.round_seconds = self.round_seconds.min(3600);
        self.max_poster_side = self.max_poster_side.clamp(1, 16);
        self.auto_max_side = self.auto_max_side.clamp(1, self.max_poster_side);
        self.max_image_mb = self.max_image_mb.clamp(1, 32);
        self.max_megapixels = self.max_megapixels.clamp(1, 40);
        self.cooldown_seconds = self.cooldown_seconds.min(3600);
        self.first_map_id = self.first_map_id.clamp(10_000, 1_000_000_000);
        self.maps_per_tick = self.maps_per_tick.clamp(1, 16);
        self.tick_ms = self.tick_ms.clamp(1, 20);
        self
    }
}

/// Poster and file names: `a-z 0-9 _ -`, at most 32.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A picture file in `images/`: a plain name with `.png`, `.jpg` or `.jpeg`, never a path.
pub fn valid_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".png").or_else(|| lower.strip_suffix(".jpg")).or_else(|| lower.strip_suffix(".jpeg"));
    name.len() <= 80 && stem.is_some_and(|s| !s.is_empty() && !s.starts_with('.') && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b' ')) && !s.contains(".."))
}

/// Why a link is refused, or `None` when the server may fetch it.
///
/// The server itself downloads the link, so a player must not be able to point it at the
/// machine it runs on or the network behind it: addresses written as numbers are checked
/// against the private ranges, and names that only mean something inside a network are
/// refused. (A public name that resolves to a private address is not caught here; that is why
/// making maps is for operators unless the owner opens it up.)
pub fn link_problem(url: &str, allow_private: bool) -> Option<&'static str> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"));
    let Some(rest) = rest else { return Some("not http(s)") };
    if url.len() > 500 || !url.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'\\' | b'<' | b'>' | b'`')) {
        return Some("odd characters or too long");
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return Some("no host");
    }
    if allow_private {
        return None;
    }
    if authority.starts_with('[') {
        return Some("private address");
    }
    let host = authority.rsplit_once(':').map_or(authority, |(h, port)| if port.bytes().all(|b| b.is_ascii_digit()) { h } else { authority }).to_ascii_lowercase();
    let parts: Vec<&str> = host.split('.').collect();
    // A host of numbers only: a dotted address, or one of the odd spellings of one (`2130706433`, `0x7f.1`)
    let numeric = parts.iter().all(|p| !p.is_empty() && (p.bytes().all(|b| b.is_ascii_digit()) || p.starts_with("0x")));
    if numeric {
        let octets: Vec<u8> = parts.iter().filter_map(|p| p.parse::<u8>().ok()).collect();
        if parts.len() != 4 || octets.len() != 4 || parts.iter().any(|p| p.len() > 1 && p.starts_with('0')) {
            return Some("private address");
        }
        let (a, b) = (octets[0], octets[1]);
        let private = a == 0 || a == 10 || a == 127 || (a == 100 && (64..128).contains(&b)) || (a == 169 && b == 254) || (a == 172 && (16..32).contains(&b)) || (a == 192 && b == 168) || (a == 192 && b == 0) || (a == 198 && (18..20).contains(&b)) || a >= 224;
        return private.then_some("private address");
    }
    let inside = parts.len() < 2 || host == "localhost" || [".localhost", ".local", ".internal", ".lan", ".home", ".intranet", ".corp"].iter().any(|s| host.ends_with(s));
    inside.then_some("private address")
}

/// `2x3` → (2, 3): columns by rows of maps.
pub fn parse_size(text: &str, max_side: u32) -> Option<(u32, u32)> {
    let (w, h) = text.to_ascii_lowercase().split_once('x').and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))?;
    ((1..=max_side).contains(&w) && (1..=max_side).contains(&h)).then_some((w, h))
}

pub fn parse_mode(text: &str) -> Option<Mode> {
    match text.to_ascii_lowercase().as_str() {
        "fit" | "contain" => Some(Mode::Fit),
        "cover" | "crop" | "fill" => Some(Mode::Cover),
        "stretch" => Some(Mode::Stretch),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(text.contains("players-can-create = false") && text.contains("first-map-id = 100000"), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.clone().cleaned(), Config::default());
        let odd: Config = toml::from_str("max-poster-side = 99\nauto-max-side = 50\nfirst-map-id = 3\nmaps-per-tick = 0\ntick-ms = 500\nmax-image-mb = 0").unwrap();
        let c = odd.cleaned();
        assert_eq!((c.max_poster_side, c.auto_max_side, c.first_map_id, c.maps_per_tick, c.tick_ms, c.max_image_mb), (16, 16, 10_000, 1, 20, 1));
    }

    #[test]
    fn links_inside_the_network_are_refused() {
        for ok in ["https://i.imgur.com/abc.png", "http://example.com:8080/a.jpg?x=1", "https://8.8.8.8/a.png", "https://cdn.discordapp.com/attachments/1/2/a.png"] {
            assert_eq!(link_problem(ok, false), None, "{ok}");
        }
        for bad in [
            "http://127.0.0.1/a.png", "http://127.0.0.1:8080/a.png", "http://localhost/a.png", "http://LOCALHOST:80/a.png", "http://10.0.0.5/a.png", "http://192.168.1.1/a.png",
            "http://172.16.0.1/a.png", "http://169.254.169.254/latest/meta-data", "http://100.64.0.1/a.png", "http://0.0.0.0/a.png", "http://[::1]/a.png", "http://[fd00::1]/a.png",
            "http://2130706433/a.png", "http://0x7f.0.0.1/a.png", "http://017700000001/a.png", "http://127.1/a.png", "http://0177.0.0.1/a.png", "http://intranet/a.png",
            "http://db.internal/a.png", "http://printer.local/a.png", "http://x.localhost/a.png", "http://224.0.0.1/a.png",
        ] {
            assert_eq!(link_problem(bad, false), Some("private address"), "{bad}");
        }
        assert_eq!(link_problem("ftp://example.com/a.png", false), Some("not http(s)"));
        assert_eq!(link_problem("https://user:pw@example.com/a.png", false), Some("no host"));
        assert_eq!(link_problem("https://example.com/a b.png", false), Some("odd characters or too long"));
        assert_eq!(link_problem("https:///a.png", false), Some("no host"));
        // The owner may open it up (a picture server on the same machine)
        assert_eq!(link_problem("http://127.0.0.1:8080/a.png", true), None);
        assert_eq!(link_problem("file:///etc/passwd", true), Some("not http(s)"));
    }

    #[test]
    fn names_sizes_and_modes() {
        assert!(valid_name("my-poster_2") && !valid_name("My Poster") && !valid_name("") && !valid_name("../x"));
        for ok in ["logo.png", "Photo 1.JPG", "a.b.jpeg"] {
            assert!(valid_file(ok), "{ok}");
        }
        for bad in ["logo.gif", ".png", "../logo.png", "a/b.png", "a\\b.png", "..png", "x..y.png", &format!("{}.png", "a".repeat(80))] {
            assert!(!valid_file(bad), "{bad}");
        }
        assert_eq!(parse_size("2x3", 8), Some((2, 3)));
        assert_eq!(parse_size("8X8", 8), Some((8, 8)));
        for bad in ["9x1", "0x2", "2", "axb", "2x", "-1x2", "2x3x4"] {
            assert_eq!(parse_size(bad, 8), None, "{bad}");
        }
        assert_eq!((parse_mode("FIT"), parse_mode("crop"), parse_mode("stretch"), parse_mode("x")), (Some(Mode::Fit), Some(Mode::Cover), Some(Mode::Stretch), None));
    }
}

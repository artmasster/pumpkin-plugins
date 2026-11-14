//! WorldMap settings and the small rules that need no server.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// th | en
    pub language: String,
    /// Milliseconds of each server tick WorldMap may use (1..=20). A tick is 50 ms.
    pub tick_ms: u32,
    /// A chunk near a player is drawn again at most this often (1..=1440).
    pub refresh_minutes: u32,
    /// Map the chunks this far around every online player (chunks, 0 = off, at most 32).
    pub around_players: u32,
    /// Dimensions to map: overworld, the_nether, the_end.
    pub dimensions: Vec<String>,
    /// Largest exported page in MB (1..=500); a bigger map is exported at a lower resolution.
    pub export_max_mb: u32,
    /// Put the players who are online at export time on the exported page.
    pub export_players: bool,
    /// Map tiles kept in memory (4..=256), about 0.35 MB each. A tile is 512 x 512 blocks; count
    /// four for every place where players are at the same time.
    pub max_tiles_in_memory: usize,
    /// Port of the live map page (`http://<server address>:<port>`), 0 = off. It has to be one of
    /// the extra ports of the server; anyone who knows the address can open the page.
    pub web_port: u16,
    /// Show where the online players are on the live map page.
    pub web_players: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: "th".into(),
            tick_ms: 4,
            refresh_minutes: 10,
            around_players: 10,
            dimensions: vec!["overworld".into(), "the_nether".into(), "the_end".into()],
            export_max_mb: 30,
            export_players: true,
            max_tiles_in_memory: 32,
            web_port: 0,
            web_players: true,
        }
    }
}

impl Config {
    /// Settings a server can run with.
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.tick_ms = self.tick_ms.clamp(1, 20);
        self.refresh_minutes = self.refresh_minutes.clamp(1, 1440);
        self.around_players = self.around_players.min(32);
        self.export_max_mb = self.export_max_mb.clamp(1, 500);
        self.max_tiles_in_memory = self.max_tiles_in_memory.clamp(4, 256);
        // Ports under 1024 are not for a game server; a wrong number turns the page off, not the plugin
        if self.web_port < 1024 {
            self.web_port = 0;
        }
        let mut dims: Vec<String> = Vec::new();
        for d in &self.dimensions {
            let d = dim_dir(d);
            if !d.is_empty() && !dims.contains(&d) {
                dims.push(d);
            }
        }
        self.dimensions = dims;
        self
    }

    /// Whether the dimension `name` (`minecraft:the_nether`) is to be mapped.
    pub fn maps(&self, name: &str) -> bool {
        self.dimensions.contains(&dim_dir(name))
    }
}

/// Folder name of a dimension, also how dimensions are written in the config and in commands:
/// `minecraft:the_nether`, `nether`, `world_nether` and `The_Nether` are all `the_nether`.
/// Anything else is cut down to `a-z 0-9 _ -` (never a path).
pub fn dim_dir(name: &str) -> String {
    let lower = name.trim().to_ascii_lowercase();
    let bare = lower.rsplit(':').next().unwrap_or("");
    let clean: String = bare.chars().filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-').take(40).collect();
    match clean.as_str() {
        "world" | "overworld" => "overworld".into(),
        "nether" | "world_nether" | "the_nether" => "the_nether".into(),
        "end" | "world_the_end" | "the_end" => "the_end".into(),
        _ => clean,
    }
}

/// The nether has a roof: the map looks under it instead of drawing bedrock.
pub fn has_roof(name: &str) -> bool {
    dim_dir(name) == "the_nether"
}

/// Position `i` of a square spiral around (0, 0): 0 is the centre, 1..=8 the ring around it,
/// and so on, so the first `(2r+1)^2` positions are exactly the square of radius `r`.
pub fn spiral(i: u32) -> (i32, i32) {
    if i == 0 {
        return (0, 0);
    }
    let i = i64::from(i);
    let mut r = (((i as f64).sqrt() + 1.0) / 2.0) as i64;
    while (2 * r + 1) * (2 * r + 1) <= i {
        r += 1;
    }
    while (2 * r - 1) * (2 * r - 1) > i {
        r -= 1;
    }
    let k = i - (2 * r - 1) * (2 * r - 1);
    let (side, off) = (k / (2 * r), k % (2 * r));
    let (x, z) = match side {
        0 => (r, -r + 1 + off),
        1 => (r - 1 - off, r),
        2 => (-r, r - 1 - off),
        _ => (-r + 1 + off, -r),
    };
    (x as i32, z as i32)
}

/// Where (`dx`, `dz`) is on the spiral: the inverse of `spiral`.
pub fn spiral_index(dx: i32, dz: i32) -> u32 {
    let (dx, dz) = (i64::from(dx), i64::from(dz));
    let r = dx.abs().max(dz.abs());
    if r == 0 {
        return 0;
    }
    let k = if dx == r && dz > -r {
        dz + r - 1
    } else if dz == r {
        2 * r + (r - 1 - dx)
    } else if dx == -r {
        4 * r + (r - 1 - dz)
    } else {
        6 * r + (dx + r - 1)
    };
    ((2 * r - 1) * (2 * r - 1) + k) as u32
}

/// Chunks in the square of radius `r`.
pub fn square(r: u32) -> u32 {
    (2 * r + 1) * (2 * r + 1)
}

/// `1234567` -> `1,234,567`
pub fn commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `1536` -> `1.5 KB`
pub fn size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn defaults_survive_a_round_trip() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(text.contains("tick-ms = 4") && text.contains("max-tiles-in-memory = 32") && text.contains("web-port = 0") && text.contains("web-players = true"), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.clone().cleaned(), Config::default());
        assert_eq!(back, Config::default());
    }

    #[test]
    fn bad_values_are_bounded() {
        let c: Config = toml::from_str("language = \"EN\"\ntick-ms = 900\nrefresh-minutes = 0\naround-players = 99\nexport-max-mb = 0\nmax-tiles-in-memory = 1\ndimensions = [\"World\", \"minecraft:the_nether\", \"nether\", \"../etc\", \"\"]").unwrap();
        let c = c.cleaned();
        assert_eq!(c.language, "en");
        assert_eq!((c.tick_ms, c.refresh_minutes, c.around_players, c.export_max_mb, c.max_tiles_in_memory), (20, 1, 32, 1, 4));
        assert_eq!(c.dimensions, ["overworld", "the_nether", "etc"]);
        assert!(c.maps("minecraft:overworld") && c.maps("minecraft:the_nether") && !c.maps("minecraft:the_end"));
        // A missing key takes its default
        let c: Config = toml::from_str("language = \"th\"").unwrap();
        assert_eq!(c.refresh_minutes, 10);
        assert_eq!((c.web_port, c.web_players), (0, true));
        let web = |text: &str| toml::from_str::<Config>(text).map(|c| c.cleaned().web_port).ok();
        assert_eq!(web("web-port = 21187"), Some(21187));
        assert_eq!(web("web-port = 80"), Some(0));
        assert_eq!(web("web-port = 70000"), None);
    }

    #[test]
    fn dimension_names_become_folders() {
        assert_eq!(dim_dir("minecraft:overworld"), "overworld");
        assert_eq!(dim_dir("Nether"), "the_nether");
        assert_eq!(dim_dir("world_the_end"), "the_end");
        assert_eq!(dim_dir("mymod:Sky Lands/../x"), "skylandsx");
        assert_eq!(dim_dir("../../etc/passwd"), "etcpasswd");
        assert!(has_roof("minecraft:the_nether") && !has_roof("minecraft:overworld"));
    }

    #[test]
    fn the_spiral_fills_squares() {
        assert_eq!(spiral(0), (0, 0));
        for r in [1u32, 2, 3, 10, 40] {
            let cells: HashSet<(i32, i32)> = (0..square(r)).map(spiral).collect();
            assert_eq!(cells.len() as u32, square(r), "radius {r}");
            let r = r as i32;
            assert!(cells.iter().all(|&(x, z)| x.abs() <= r && z.abs() <= r));
        }
        // Rings go outwards
        let mut last = 0;
        for i in 0..square(12) {
            let (x, z) = spiral(i);
            let ring = x.abs().max(z.abs());
            assert!(ring >= last);
            last = ring;
        }
        // And back
        for i in (0..square(30)).chain(square(511) - 5..square(512)) {
            let (x, z) = spiral(i);
            assert_eq!(spiral_index(x, z), i);
        }
        // Far out, where floats get rough
        let big: HashSet<(i32, i32)> = (square(511)..square(512)).map(spiral).collect();
        assert_eq!(big.len() as u32, square(512) - square(511));
        assert!(big.iter().all(|&(x, z)| x.abs().max(z.abs()) == 512));
    }

    #[test]
    fn numbers_read_well() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1000), "1,000");
        assert_eq!(commas(1234567), "1,234,567");
        assert_eq!(size(512), "512 B");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(3 * 1024 * 1024), "3.0 MB");
    }
}

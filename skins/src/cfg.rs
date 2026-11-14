//! Skins settings (no server needed).
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// th | en
    pub language: String,
    /// Give every player the skin of the Mojang account that has their name.
    pub restore_own_skin: bool,
    /// Skins (account names) for players whose name is not a Mojang account. Empty = they keep
    /// the plain Steve/Alex look. A player always gets the same one of the list.
    pub default_skins: Vec<String>,
    /// Let players choose a skin with /skin (operators always can).
    pub players_can_change: bool,
    /// Let players use an image link (`/skin url`), made into a skin by mineskin.org.
    pub allow_url: bool,
    /// Optional key of your own mineskin.org account (more image-link skins per minute).
    pub mineskin_api_key: String,
    /// Seconds a player waits between two /skin changes (0..=3600).
    pub cooldown_seconds: u64,
    /// Hours a fetched skin is kept before it is asked for again (1..=720).
    pub cache_hours: u64,
    /// Show a new skin to the other players at once by respawning the player where they stand
    /// (a short flicker for them; they see their own new skin after rejoining). Off = everyone
    /// sees a changed skin after the player rejoins.
    pub instant_refresh: bool,
    /// Account names nobody may wear.
    pub blocked_skins: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: "th".into(),
            restore_own_skin: true,
            default_skins: Vec::new(),
            players_can_change: true,
            allow_url: true,
            mineskin_api_key: String::new(),
            cooldown_seconds: 30,
            cache_hours: 72,
            instant_refresh: true,
            blocked_skins: Vec::new(),
        }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.cooldown_seconds = self.cooldown_seconds.min(3600);
        self.cache_hours = self.cache_hours.clamp(1, 720);
        let names = |list: &mut Vec<String>| {
            let mut out: Vec<String> = Vec::new();
            for n in list.drain(..) {
                let n = n.trim().to_ascii_lowercase();
                if valid_name(&n) && !out.contains(&n) && out.len() < 200 {
                    out.push(n);
                }
            }
            *list = out;
        };
        names(&mut self.default_skins);
        names(&mut self.blocked_skins);
        self.mineskin_api_key = self.mineskin_api_key.trim().chars().filter(|c| c.is_ascii_graphic()).take(200).collect();
        self
    }

    pub fn blocked(&self, skin: &str) -> bool {
        self.blocked_skins.iter().any(|b| b.eq_ignore_ascii_case(skin))
    }
}

/// A Minecraft account name: 1 to 16 of `A-Z a-z 0-9 _` (old accounts can be shorter than 3).
/// It goes into a Mojang URL as it is, so nothing else is let through.
pub fn valid_name(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// An image link a skin can be made from: plain `https://` (or `http://`) to a host, no
/// credentials, no spaces or quotes, at most 300 characters.
pub fn valid_url(url: &str) -> bool {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"));
    let Some(rest) = rest else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    url.len() <= 300
        && !host.is_empty()
        && host.contains('.')
        && !host.contains('@')
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'))
        && url.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'\\' | b'<' | b'>' | b'`'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(text.contains("restore-own-skin = true") && text.contains("cache-hours = 72") && text.contains("instant-refresh = true"), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.clone().cleaned(), Config::default());
    }

    #[test]
    fn odd_values_are_bounded() {
        let c: Config = toml::from_str("language = \"EN\"\ncooldown-seconds = 999999\ncache-hours = 0\ndefault-skins = [\"Notch\", \"notch\", \"bad name\", \"../x\", \"\"]\nblocked-skins = [\"Herobrine\"]\nmineskin-api-key = \" ab c\\n\"").unwrap();
        let c = c.cleaned();
        assert_eq!((c.language.as_str(), c.cooldown_seconds, c.cache_hours), ("en", 3600, 1));
        assert_eq!(c.default_skins, ["notch"]);
        assert!(c.blocked("HEROBRINE") && !c.blocked("notch"));
        assert_eq!(c.mineskin_api_key, "abc");
    }

    #[test]
    fn names_and_links() {
        for ok in ["Notch", "jeb_", "a", "A_16_chars_name_"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "seventeen_chars__", "has space", "a/b", "ไทย", "a.b", "a?b"] {
            assert!(!valid_name(bad), "{bad}");
        }
        for ok in ["https://i.imgur.com/abc.png", "http://example.com/s.png?x=1", "https://textures.minecraft.net/texture/abc"] {
            assert!(valid_url(ok), "{ok}");
        }
        for bad in ["", "ftp://x.com/a.png", "https://", "https://localhost/a.png", "https://user:pw@x.com/a.png", "https://x.com/a b.png", "https://x.com/\"a", "javascript:alert(1)", &format!("https://x.com/{}", "a".repeat(300))] {
            assert!(!valid_url(bad), "{bad}");
        }
    }
}

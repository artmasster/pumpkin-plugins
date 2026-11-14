//! Placeholders settings (no server needed).
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// th | en
    pub language: String,
    /// Minutes east of UTC for `%server_time%` and `%server_date%` (Thailand = 420).
    pub utc_offset_minutes: i32,
    /// What `%player_…%` placeholders say when there is no player (a console broadcast).
    pub no_player: String,
    /// Your own fixed texts: `discord = "discord.gg/abc"` makes `%custom_discord%`.
    /// A value may use the other placeholders.
    pub custom: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        let mut custom = BTreeMap::new();
        custom.insert("discord".into(), "discord.gg/your-invite".into());
        custom.insert("welcome".into(), "&aยินดีต้อนรับ &f%player_name% &7(ออนไลน์ %server_online% คน)".into());
        Config { language: "th".into(), utc_offset_minutes: 420, no_player: String::new(), custom }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.utc_offset_minutes = self.utc_offset_minutes.clamp(-12 * 60, 14 * 60);
        self.no_player = self.no_player.chars().filter(|c| !c.is_control()).take(64).collect();
        // Names are what stands after `custom_`: lowercase words
        let custom = std::mem::take(&mut self.custom);
        for (k, v) in custom {
            let k = k.trim().to_ascii_lowercase();
            let ok = !k.is_empty() && k.len() <= 48 && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            if ok && self.custom.len() < 200 {
                self.custom.insert(k, v.chars().filter(|c| !c.is_control()).take(512).collect());
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(text.contains("utc-offset-minutes = 420") && text.contains("[custom]"), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.clone().cleaned(), Config::default());
    }

    #[test]
    fn odd_values_are_bounded() {
        let c: Config = toml::from_str("language = \"EN\"\nutc-offset-minutes = 99999\nno-player = \"a\\nb\"\n[custom]\n\"Bad Key\" = \"x\"\nDiscord = \"d\"\n\"ok_1\" = \"v\"").unwrap();
        let c = c.cleaned();
        assert_eq!((c.language.as_str(), c.utc_offset_minutes, c.no_player.as_str()), ("en", 840, "ab"));
        assert_eq!(c.custom.keys().map(String::as_str).collect::<Vec<_>>(), ["discord", "ok_1"]);
    }
}

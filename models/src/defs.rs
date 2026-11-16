//! What the server owner wrote: the settings, the settings of each model, and the models that
//! stand in the world. No server needed here.
use serde::{Deserialize, Serialize};

/// The namespace of everything this plugin puts into the resource pack and onto entities.
pub const NS: &str = "models";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// `th` or `en`: the language of the plugin's own messages.
    pub language: String,
    /// Ticks between two poses of a model. The game glides from one to the next, so 2 looks
    /// as smooth as 1 and costs half.
    pub update_interval: u32,
    /// A model that stands in the world is only there while a player is this close (blocks).
    pub view_distance: u32,
    /// Further away than this, a model moves at `far-interval` instead.
    pub far_distance: u32,
    pub far_interval: u32,
    /// The most bones that may be moved in one tick, over all models. What does not fit waits
    /// for the next tick: many models get slower, the server does not.
    pub max_updates_per_tick: u32,
    pub pack: PackConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PackConfig {
    /// Send the pack to players when they join.
    pub send: bool,
    /// Where players download the pack from, when it is hosted somewhere else: upload
    /// `pack.zip` from this plugin's folder there after every `/models reload`.
    pub url: String,
    /// Or let the plugin serve it: a free TCP port of this server (0 = off) ...
    pub port: u16,
    /// ... and the address players use to reach the server (`play.example.com` or an IP).
    pub host: String,
    /// Players who say no cannot stay (the game asks them to accept or leave).
    pub required: bool,
    /// The question the game shows with the pack. `&` colours work.
    pub prompt: String,
    /// The text of the pack in the game's pack list.
    pub description: String,
    /// Ticks after joining before the pack is sent (20 = one second).
    pub join_delay_ticks: u64,
    /// The pack format range written into `pack.mcmeta`.
    pub format_min: u32,
    pub format_max: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), update_interval: 2, view_distance: 48, far_distance: 24, far_interval: 4, max_updates_per_tick: 1200, pack: PackConfig::default() }
    }
}

impl Default for PackConfig {
    fn default() -> Self {
        PackConfig {
            send: true,
            url: String::new(),
            port: 0,
            host: String::new(),
            required: false,
            prompt: "&aเซิร์ฟเวอร์นี้มีโมเดลพิเศษ &7กด ใช่ เพื่อโหลดภาพของโมเดล".into(),
            description: "Models of this server".into(),
            join_delay_ticks: 20,
            format_min: 65,
            format_max: 999,
        }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.update_interval = self.update_interval.clamp(1, 20);
        self.far_interval = self.far_interval.clamp(self.update_interval, 40);
        self.view_distance = self.view_distance.clamp(8, 256);
        self.far_distance = self.far_distance.min(self.view_distance);
        self.max_updates_per_tick = self.max_updates_per_tick.clamp(50, 100_000);
        let p = &mut self.pack;
        p.url = p.url.trim().to_owned();
        if !(p.url.is_empty() || p.url.starts_with("http://") || p.url.starts_with("https://")) || p.url.len() > 500 || p.url.chars().any(char::is_whitespace) {
            p.url.clear();
        }
        // A host as it goes into a URL: a name, an IPv4 address or `[v6]`, nothing else
        p.host = p.host.trim().trim_start_matches("http://").trim_end_matches('/').to_owned();
        if p.host.len() > 253 || !p.host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '[' | ']' | ':')) {
            p.host.clear();
        }
        p.join_delay_ticks = p.join_delay_ticks.min(600);
        p.format_min = p.format_min.clamp(1, 9999);
        p.format_max = p.format_max.clamp(p.format_min, 9999);
        p.description = p.description.chars().filter(|c| !c.is_control()).take(120).collect();
        p.prompt = p.prompt.chars().filter(|c| !c.is_control()).take(200).collect();
        self
    }

    pub fn thai(&self) -> bool {
        self.language == "th"
    }

    /// The link players get for a pack with this SHA-1, if the owner said where it is.
    pub fn pack_url(&self, sha1: &str) -> Option<String> {
        let p = &self.pack;
        if !p.url.is_empty() {
            return Some(p.url.clone());
        }
        // The hash in the path: a changed pack is a new link, so nothing serves a stale copy
        (p.port != 0 && !p.host.is_empty()).then(|| format!("http://{}:{}/{sha1}.zip", p.host, p.port))
    }
}

/// `models/<name>.toml`, next to `<name>.bbmodel`: how this model is shown. Every line is
/// optional.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Settings {
    /// Times the size it has in Blockbench.
    pub scale: f32,
    /// Blocks up (or down) from where it stands.
    pub lift: f32,
    /// Blocks up (or down) when it sits on a creature. Without it the plugin puts the model's
    /// feet at the creature's feet, taking the creature's height as where riders sit.
    pub ride_lift: Option<f32>,
    /// Hide the creature the model sits on.
    pub hide_base: bool,
    /// The animations the plugin starts by itself, by name.
    pub idle: String,
    pub walk: String,
    pub hurt: String,
    pub death: String,
    pub spawn: String,
    /// Ticks an animation takes to take over from another.
    pub blend: u32,
    /// Radius of the shadow on the ground, in blocks (0 = none).
    pub shadow: f32,
    /// How far it can be seen: 1.0 is the game's own distance for entities.
    pub view_range: f32,
    /// Width and height of what can be clicked, in blocks, when the model has no `hitbox`.
    pub hitbox: Option<(f32, f32)>,
    /// The colour a model turns for a moment when its creature is hurt (`#rrggbb`, empty = no).
    pub hurt_tint: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            scale: 1.0,
            lift: 0.0,
            ride_lift: None,
            hide_base: true,
            idle: "idle".into(),
            walk: "walk".into(),
            hurt: "hurt".into(),
            death: "death".into(),
            spawn: "spawn".into(),
            blend: 4,
            shadow: 0.0,
            view_range: 1.0,
            hitbox: None,
            hurt_tint: "#ff5555".into(),
        }
    }
}

impl Settings {
    pub fn cleaned(mut self) -> Settings {
        let sane = |v: f32, low: f32, high: f32, or: f32| if v.is_finite() { v.clamp(low, high) } else { or };
        self.scale = sane(self.scale, 0.01, 100.0, 1.0);
        self.lift = sane(self.lift, -64.0, 64.0, 0.0);
        self.ride_lift = self.ride_lift.map(|v| sane(v, -64.0, 64.0, 0.0));
        self.blend = self.blend.min(100);
        self.shadow = sane(self.shadow, 0.0, 64.0, 0.0);
        self.view_range = sane(self.view_range, 0.05, 64.0, 1.0);
        self.hitbox = self.hitbox.filter(|(w, h)| w.is_finite() && h.is_finite()).map(|(w, h)| (w.clamp(0.1, 64.0), h.clamp(0.1, 64.0)));
        self
    }

    pub fn tint(&self) -> Option<i32> {
        colour(&self.hurt_tint)
    }
}

/// `#rrggbb` or `rrggbb` as the number the game wants.
pub fn colour(text: &str) -> Option<i32> {
    let hex = text.trim().trim_start_matches('#');
    (hex.len() == 6).then(|| i32::from_str_radix(hex, 16).ok()).flatten()
}

/// A model that stands in the world: `placed.json` is a list of these.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Placed {
    pub id: u32,
    pub model: String,
    /// The world, as the server names its dimension.
    pub world: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    #[serde(default = "one")]
    pub scale: f32,
    /// The animation it shows instead of the model's idle.
    #[serde(default)]
    pub animation: Option<String>,
    /// Commands a click runs, as the player who clicked. `%player%` is their name.
    #[serde(default)]
    pub commands: Vec<String>,
    /// The name above it (needs a `tag_` bone). `&` colours work.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

fn one() -> f32 {
    1.0
}

/// Something the owner should know, in both languages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub th: String,
    pub en: String,
}

impl Note {
    pub fn new(place: &str, th: impl AsRef<str>, en: impl AsRef<str>) -> Note {
        Note { th: format!("{place}: {}", th.as_ref()), en: format!("{place}: {}", en.as_ref()) }
    }

    /// A note that only exists in English (what a model file says about itself).
    pub fn plain(place: &str, text: &str) -> Note {
        Note::new(place, text, text)
    }

    pub fn text(&self, thai: bool) -> &str {
        if thai { &self.th } else { &self.en }
    }
}

/// The model written on the very first start (`scripts/example-model.py` makes the file).
pub const EXAMPLE_MODEL: &str = include_str!("example.bbmodel");

pub const EXAMPLE_SETTINGS: &str = r##"# Models: settings of one model. Name this file like the model: dragon.bbmodel -> dragon.toml.
# Every line is optional; remove the "#" in front of one to use it, then run /models reload.
#
# scale = 1.0          # times the size it has in Blockbench
# lift = 0.0           # blocks up (or down) from where it stands
# ride-lift = -0.9     # blocks up (or down) when it sits on a creature
# hide-base = true     # hide the creature the model sits on
# idle = "idle"        # the animations the plugin starts by itself, by their name in Blockbench
# walk = "walk"
# hurt = "hurt"
# death = "death"
# spawn = "spawn"
# blend = 4            # ticks one animation takes to take over from another
# shadow = 0.0         # radius of the shadow on the ground, in blocks
# view-range = 1.0     # how far it can be seen (1.0 = like other entities)
# hitbox = [1.0, 2.0]  # width and height of what can be clicked (or make a cube named hitbox)
# hurt-tint = "#ff5555" # the colour it turns for a moment when hurt ("" = none)
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_kept_within_reason() {
        let cfg = Config { update_interval: 0, far_interval: 1, view_distance: 9999, far_distance: 5000, max_updates_per_tick: 1, ..Config::default() }.cleaned();
        assert_eq!((cfg.update_interval, cfg.far_interval, cfg.view_distance, cfg.far_distance, cfg.max_updates_per_tick), (1, 1, 256, 256, 50));
        let cfg = Config { update_interval: 4, far_interval: 2, language: "EN".into(), ..Config::default() }.cleaned();
        assert_eq!((cfg.far_interval, cfg.thai()), (4, false));
        let s = Settings { scale: f32::NAN, lift: 1e9, blend: 5000, hitbox: Some((0.0, 900.0)), ..Settings::default() }.cleaned();
        assert_eq!((s.scale, s.lift, s.blend, s.hitbox), (1.0, 64.0, 100, Some((0.1, 64.0))));
    }

    #[test]
    fn the_pack_link_needs_a_place() {
        let mut cfg = Config::default().cleaned();
        assert_eq!(cfg.pack_url("abc"), None);
        cfg.pack.port = 25570;
        assert_eq!(cfg.pack_url("abc"), None);
        cfg.pack.host = "play.example.com".into();
        assert_eq!(cfg.pack_url("abc").as_deref(), Some("http://play.example.com:25570/abc.zip"));
        cfg.pack.url = "https://cdn.example.com/models.zip".into();
        assert_eq!(cfg.clone().cleaned().pack_url("abc").as_deref(), Some("https://cdn.example.com/models.zip"));
        cfg.pack.url = "ftp://x".into();
        assert_eq!(cfg.cleaned().pack_url("abc").as_deref(), Some("http://play.example.com:25570/abc.zip"));
    }

    #[test]
    fn files_read_as_written() {
        let s: Settings = toml::from_str("scale = 2.5\nride-lift = -1.25\nwalk = \"run\"\nhitbox = [1.0, 2.0]\nhurt-tint = \"\"").unwrap();
        assert_eq!((s.scale, s.ride_lift, s.walk.as_str(), s.idle.as_str(), s.hitbox, s.tint()), (2.5, Some(-1.25), "run", "idle", Some((1.0, 2.0)), None));
        assert_eq!(Settings::default().tint(), Some(0xff5555));
        assert_eq!(colour("#00ff00"), Some(0x00ff00));
        assert_eq!(colour("red"), None);
        // The example is all comments: it reads as the defaults
        assert_eq!(toml::from_str::<Settings>(EXAMPLE_SETTINGS).unwrap(), Settings::default());
        let uncommented: String = EXAMPLE_SETTINGS.lines().filter_map(|l| l.strip_prefix("# ")).filter(|l| l.contains(" = ")).collect::<Vec<_>>().join("\n");
        let s: Settings = toml::from_str(&uncommented).unwrap();
        assert_eq!((s.ride_lift, s.hitbox), (Some(-0.9), Some((1.0, 2.0))));

        let p: Placed = serde_json::from_str(r#"{"id":3,"model":"dragon","world":"minecraft:overworld","x":1.5,"y":64.0,"z":-2.5,"yaw":90.0}"#).unwrap();
        assert_eq!((p.scale, p.animation.as_deref(), p.commands.len(), p.name.as_str()), (1.0, None, 0, ""));
        // A name is only written when there is one
        assert!(!serde_json::to_string(&p).unwrap().contains("name"));
        let named = Placed { name: "&aBob".into(), ..p };
        assert_eq!(serde_json::from_str::<Placed>(&serde_json::to_string(&named).unwrap()).unwrap(), named);
    }
}

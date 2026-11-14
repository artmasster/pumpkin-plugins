//! Data model and rules that do not need the server (unit-tested natively).
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A saved location. `dim` is the dimension id (`minecraft:overworld`), the only
/// stable world key Pumpkin gives plugins.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Loc {
    pub dim: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    #[serde(default)]
    pub yaw: f32,
    #[serde(default)]
    pub pitch: f32,
}

impl Loc {
    pub fn short(&self) -> String {
        format!("{} {:.0} {:.0} {:.0}", dim_name(&self.dim), self.x, self.y, self.z)
    }

    pub fn dist2(&self, x: f64, y: f64, z: f64) -> f64 {
        (self.x - x).powi(2) + (self.y - y).powi(2) + (self.z - z).powi(2)
    }
}

/// Bukkit-style names players know.
pub fn dim_name(dim: &str) -> &str {
    match dim {
        "minecraft:overworld" | "overworld" => "world",
        "minecraft:the_nether" | "the_nether" => "world_nether",
        "minecraft:the_end" | "the_end" => "world_the_end",
        other => other,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerData {
    pub name: String,
    pub homes: BTreeMap<String, Loc>,
    pub back: Option<Loc>,
    pub balance: f64,
    pub joined: bool,
    pub nick: Option<String>,
    pub god: bool,
    pub fly: bool,
    pub last_seen: u64,
    pub first_seen: u64,
    /// Rank from ranks.toml; unknown or missing = the default group.
    pub group: Option<String>,
    /// Extra permission nodes for this player only.
    pub perms: BTreeMap<String, bool>,
    /// Unix seconds; 0 = not muted, u64::MAX = until unmuted.
    pub muted_until: u64,
    pub mute_reason: String,
    /// Player keys whose chat, private messages and teleport requests are hidden.
    pub ignored: Vec<String>,
    pub mail: Vec<Mail>,
    /// Kit name -> unix seconds of the last use.
    pub kits: BTreeMap<String, u64>,
    /// Refuse /tpa and /tpahere requests.
    pub tp_off: bool,
    pub socialspy: bool,
    pub warnings: Vec<Warning>,
    /// Unix seconds; 0 = free, u64::MAX = until /unjail.
    pub jailed_until: u64,
    pub frozen: bool,
    pub vanished: bool,
    /// Refuse /pay from other players.
    pub pay_off: bool,
    /// Seconds played, counted while online.
    pub playtime: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Warning {
    pub by: String,
    pub reason: String,
    pub at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Mail {
    pub from: String,
    pub text: String,
    pub at: u64,
}

impl PlayerData {
    pub fn muted(&self, now: u64) -> bool {
        self.muted_until > now
    }

    pub fn jailed(&self, now: u64) -> bool {
        self.jailed_until > now
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// th | en
    pub language: String,
    /// Seconds a player must stand still before a teleport (0 = instant).
    pub teleport_delay: u64,
    /// Seconds between teleports per player (0 = none).
    pub teleport_cooldown: u64,
    /// Seconds a /tpa request stays valid.
    pub tpa_timeout: u64,
    /// Homes per player without a `basics:homes.<n>` permission.
    pub default_homes: u32,
    /// Balance of a new player.
    pub starting_balance: f64,
    pub currency_symbol: String,
    /// Send new players to /spawn on their first join.
    pub first_join_spawn: bool,
    /// Send every player to /spawn when they join.
    pub spawn_on_join: bool,
    /// Respawn at /spawn after death.
    pub respawn_at_spawn: bool,
    /// Remember the death location for /back.
    pub back_on_death: bool,
    /// Private message format: {from} {to} {message}
    pub msg_format: String,
    /// Public chat format: {prefix} {name} {suffix} {message}. Empty = leave chat to the server
    /// (no rank prefix, /ignore does not hide public chat).
    pub chat_format: String,
    /// Lines sent to a player on join: {name} {online}.
    pub motd: Vec<String>,
    /// Lines broadcast in turn every `announce-interval` seconds (0 or empty = off).
    pub announcements: Vec<String>,
    pub announce_interval: u64,
    /// Seconds without moving before a player is marked AFK (0 = only /afk).
    pub afk_auto: u64,
    /// Tell everyone when a player goes AFK or comes back (off = only that player).
    pub afk_broadcast: bool,
    /// Require `basics:warp.<name>` (or op) for each warp instead of letting everyone use every warp.
    pub warp_permission: bool,
    /// /rtp: distance from the world spawn, in blocks.
    pub rtp_min_radius: u32,
    pub rtp_max_radius: u32,
    /// Seconds between /rtp uses per player.
    pub rtp_cooldown: u64,
    /// Default radius of /near.
    pub near_radius: u32,
    /// Messages kept per mailbox.
    pub mail_max: usize,
    /// Lines of /rules.
    pub rules: Vec<String>,
    /// Tab list header and footer: {online} {max} {tps} {name}; `\n` or a new line splits lines. Empty = untouched.
    pub tab_header: String,
    pub tab_footer: String,
    /// Kick players who stay AFK this many minutes (0 = never). `basics:bypass.afkkick` is exempt.
    pub afk_kick_minutes: u64,
    /// Basics commands one player may run within 5 seconds before being told to slow down.
    pub command_burst: u32,
    /// Warnings that ban a player automatically (0 = never) and for how many minutes (0 = for good).
    pub warn_ban_at: u32,
    pub warn_ban_minutes: u64,
    /// How far a jailed player may walk from the jail point, in blocks.
    pub jail_radius: u32,
    /// Extra chat lines when a player joins or leaves: {name} {player} {prefix} {suffix} {online}.
    /// Empty = none. The game's own "joined the game" line is left alone.
    pub join_message: String,
    pub quit_message: String,
    /// Used instead of `join-message` the first time a player joins (empty = `join-message`).
    pub first_join_message: String,
    /// Show `announcements` as a boss bar for this many seconds instead of a chat line.
    pub announce_bossbar: bool,
    pub announce_bossbar_seconds: u64,
    /// Tell everyone when play time promotes a player (`after-hours` in ranks.toml).
    pub auto_rank_broadcast: bool,
    /// Own command names: `hub = "spawn"`, `day7 = "ptime 7000"`. `{args}` and `{1}`..`{9}` take
    /// what the player typed after the name; without them it is added at the end.
    pub aliases: BTreeMap<String, String>,
    /// Price of using a warp, by warp name.
    pub warp_cost: BTreeMap<String, f64>,
    /// Seconds to stand still before a warp, by warp name (instead of `teleport-delay`).
    pub warp_delay: BTreeMap<String, u64>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: "th".into(),
            teleport_delay: 3,
            teleport_cooldown: 0,
            tpa_timeout: 60,
            default_homes: 3,
            starting_balance: 0.0,
            currency_symbol: "$".into(),
            first_join_spawn: true,
            spawn_on_join: false,
            respawn_at_spawn: false,
            back_on_death: true,
            msg_format: "&7[&f{from} &7-> &f{to}&7] &f{message}".into(),
            chat_format: "{prefix}{name}{suffix}&7: &f{message}".into(),
            motd: Vec::new(),
            announcements: Vec::new(),
            announce_interval: 600,
            afk_auto: 300,
            afk_broadcast: true,
            warp_permission: false,
            rtp_min_radius: 200,
            rtp_max_radius: 3000,
            rtp_cooldown: 60,
            near_radius: 200,
            mail_max: 50,
            rules: Vec::new(),
            tab_header: String::new(),
            tab_footer: String::new(),
            afk_kick_minutes: 0,
            command_burst: 12,
            warn_ban_at: 0,
            warn_ban_minutes: 1440,
            jail_radius: 8,
            join_message: String::new(),
            quit_message: String::new(),
            first_join_message: String::new(),
            announce_bossbar: false,
            announce_bossbar_seconds: 10,
            auto_rank_broadcast: true,
            aliases: BTreeMap::new(),
            warp_cost: BTreeMap::new(),
            warp_delay: BTreeMap::new(),
        }
    }
}

/// Whether a player who has just been welcomed is sent to /spawn.
pub fn spawn_on_welcome(first: bool, cfg: &Config, has_spawn: bool) -> bool {
    has_spawn && ((first && cfg.first_join_spawn) || cfg.spawn_on_join)
}

/// The join line for a player: `first-join-message` on the first join when it is set, else
/// `join-message` (empty = no line).
pub fn join_template(first: bool, cfg: &Config) -> &str {
    if first && !cfg.first_join_message.trim().is_empty() { &cfg.first_join_message } else { &cfg.join_message }
}

// ---- ranks ------------------------------------------------------------------

/// One rank of ranks.toml. A permission written `-node` is denied.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Group {
    /// The group of players without a rank (exactly one group should set this).
    pub default: bool,
    pub prefix: String,
    pub suffix: String,
    pub inherits: Vec<String>,
    pub permissions: Vec<String>,
    /// Hours of play time after which a player of the default group (or of a rank with a smaller
    /// `after-hours`) is promoted to this rank. 0 = never.
    pub after_hours: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Ranks {
    pub groups: BTreeMap<String, Group>,
}

impl Default for Ranks {
    fn default() -> Self {
        let mut groups = BTreeMap::new();
        groups.insert("default".to_owned(), Group { default: true, ..Default::default() });
        groups.insert(
            "vip".to_owned(),
            Group {
                prefix: "&6[VIP] &r".into(),
                inherits: vec!["default".into()],
                permissions: vec!["basics:homes.10".into(), "basics:command.fly".into(), "basics:kit.vip".into()],
                ..Default::default()
            },
        );
        Ranks { groups }
    }
}

impl Ranks {
    /// Name of the group players without a rank belong to.
    pub fn default_group(&self) -> &str {
        self.groups.iter().find(|(_, g)| g.default).map_or("default", |(n, _)| n.as_str())
    }

    /// The group a player is in: their rank when it still exists, else the default group.
    pub fn group_of<'a>(&'a self, rank: Option<&'a str>) -> &'a str {
        match rank {
            Some(r) if self.groups.contains_key(r) => r,
            _ => self.default_group(),
        }
    }

    /// Permission nodes of a group, parents first so the group's own lines win. Cycles are cut.
    pub fn resolve(&self, group: &str) -> BTreeMap<String, bool> {
        fn walk(r: &Ranks, name: &str, seen: &mut Vec<String>, out: &mut BTreeMap<String, bool>) {
            if seen.iter().any(|s| s == name) {
                return;
            }
            seen.push(name.to_owned());
            let Some(g) = r.groups.get(name) else { return };
            for parent in &g.inherits {
                walk(r, parent, seen, out);
            }
            for node in &g.permissions {
                let node = node.trim();
                match node.strip_prefix('-') {
                    Some(n) if !n.is_empty() => out.insert(modern_node(n), false),
                    _ if !node.is_empty() => out.insert(modern_node(node), true),
                    _ => None,
                };
            }
        }
        let mut out = BTreeMap::new();
        walk(self, group, &mut Vec::new(), &mut out);
        out
    }

    pub fn decor(&self, group: &str) -> (&str, &str) {
        self.groups.get(group).map_or(("", ""), |g| (g.prefix.as_str(), g.suffix.as_str()))
    }

    fn hours(&self, group: &str) -> f64 {
        self.groups.get(group).map_or(0.0, |g| g.after_hours).max(0.0)
    }

    /// The rank play time has earned: the group with the largest `after-hours` the player has
    /// passed. Only players of the default group or of a smaller play-time rank move; a rank an
    /// admin gave by hand (no `after-hours`) is never touched.
    pub fn auto_rank(&self, rank: Option<&str>, playtime_secs: u64) -> Option<String> {
        let current = self.group_of(rank);
        let have = self.hours(current);
        if current != self.default_group() && have <= 0.0 {
            return None;
        }
        let played = playtime_secs as f64 / 3600.0;
        self.groups
            .iter()
            .filter(|(n, g)| g.after_hours.is_finite() && g.after_hours > have && played >= g.after_hours && n.as_str() != current && !g.default)
            .max_by(|a, b| a.1.after_hours.total_cmp(&b.1.after_hours))
            .map(|(n, _)| n.clone())
    }
}

/// Permission nodes: `namespace:key.parts`, `*` allowed as a whole part.
pub fn valid_node(s: &str) -> bool {
    let Some((ns, key)) = s.split_once(':') else { return false };
    let part = |p: &str| !p.is_empty() && (p == "*" || p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    part(ns) && ns != "*" && s.len() <= 96 && key.split('.').all(part)
}

// ---- kits -------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Kit {
    /// `minecraft:bread 16` (the count is optional, 1..=99).
    pub items: Vec<String>,
    /// Seconds between uses per player (0 = no limit).
    pub cooldown: u64,
    /// Everyone may use it; otherwise `basics:kit.<name>` (or op) is needed.
    pub everyone: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Kits {
    pub kits: BTreeMap<String, Kit>,
}

impl Default for Kits {
    fn default() -> Self {
        let mut kits = BTreeMap::new();
        kits.insert(
            "starter".to_owned(),
            Kit {
                items: ["minecraft:stone_sword", "minecraft:stone_pickaxe", "minecraft:stone_axe", "minecraft:bread 16"].map(String::from).to_vec(),
                cooldown: 86_400,
                everyone: true,
            },
        );
        Kits { kits }
    }
}

/// `bread 16` -> (`minecraft:bread`, 16).
pub fn parse_item(s: &str) -> Option<(String, u8)> {
    let mut it = s.split_whitespace();
    let id = it.next()?.to_lowercase();
    let count = match it.next() {
        Some(n) => n.parse::<u8>().ok().filter(|n| (1..=99).contains(n))?,
        None => 1,
    };
    let ok = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    let key = match id.split_once(':') {
        Some((ns, k)) if ok(ns) && ok(k) => id.clone(),
        None if ok(&id) => format!("minecraft:{id}"),
        _ => return None,
    };
    it.next().is_none().then_some((key, count))
}

// ---- worth ------------------------------------------------------------------

/// worth.toml: what the server pays for one item with /sell.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Worth {
    pub items: BTreeMap<String, f64>,
}

impl Default for Worth {
    fn default() -> Self {
        let items = [
            ("minecraft:cobblestone", 0.1),
            ("minecraft:wheat", 1.0),
            ("minecraft:coal", 2.0),
            ("minecraft:iron_ingot", 10.0),
            ("minecraft:gold_ingot", 15.0),
            ("minecraft:emerald", 25.0),
            ("minecraft:diamond", 100.0),
        ];
        Worth { items: items.into_iter().map(|(k, v)| (k.to_owned(), v)).collect() }
    }
}

impl Worth {
    /// Price of one item; `None` when the server does not buy it.
    pub fn price(&self, key: &str) -> Option<f64> {
        // The server names items without the namespace (`diamond`); the file may use either form
        let short = key.strip_prefix("minecraft:").unwrap_or(key);
        self.items
            .get(key)
            .or_else(|| self.items.get(short))
            .or_else(|| self.items.get(&format!("minecraft:{short}")))
            .copied()
            .filter(|p| p.is_finite() && *p > 0.0 && *p <= 1e9)
    }
}

/// The most one balance may hold; keeps sums exact in an f64.
pub const MAX_BALANCE: f64 = 1e13;

/// Add to a balance without passing `MAX_BALANCE`; rounds to 2 decimals.
pub fn credit(balance: f64, amount: f64) -> f64 {
    (((balance + amount).clamp(0.0, MAX_BALANCE)) * 100.0).round() / 100.0
}

/// Sliding window for "at most `max` actions per `window` seconds". Times are seconds as f64.
#[derive(Default)]
pub struct Burst {
    hits: Vec<f64>,
}

impl Burst {
    /// Count one action at `now`; false when it is over the limit (the action is not counted).
    pub fn allow(&mut self, now: f64, max: u32, window: f64) -> bool {
        self.hits.retain(|t| now - *t < window);
        if max == 0 || (self.hits.len() as u32) < max {
            self.hits.push(now);
            true
        } else {
            false
        }
    }

    pub fn idle(&self, now: f64, window: f64) -> bool {
        self.hits.iter().all(|t| now - *t >= window)
    }
}

// ---- time -------------------------------------------------------------------

/// `30s`, `10m`, `2h`, `7d`, `1w`; a bare number is minutes. `None` for anything else.
pub fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase();
    let (num, mul) = match s.chars().last()? {
        's' => (&s[..s.len() - 1], 1),
        'm' => (&s[..s.len() - 1], 60),
        'h' => (&s[..s.len() - 1], 3600),
        'd' => (&s[..s.len() - 1], 86_400),
        'w' => (&s[..s.len() - 1], 604_800),
        _ => (&s[..], 60),
    };
    let n = num.parse::<u64>().ok()?;
    (n > 0).then(|| n.saturating_mul(mul)).filter(|v| *v <= 10 * 365 * 86_400)
}

/// The two largest units: `2d 3h`, `5m 10s`.
pub fn fmt_duration(secs: u64, th: bool) -> String {
    let units: [(u64, &str, &str); 4] = [(86_400, "วัน", "d"), (3600, "ชม.", "h"), (60, "นาที", "m"), (1, "วินาที", "s")];
    let mut left = secs;
    let mut parts = Vec::new();
    for (size, t, e) in units {
        if left >= size && parts.len() < 2 {
            parts.push(if th { format!("{} {t}", left / size) } else { format!("{}{e}", left / size) });
            left %= size;
        }
    }
    if parts.is_empty() { if th { "0 วินาที".into() } else { "0s".into() } } else { parts.join(" ") }
}

/// Fill `{key}` placeholders in one pass, so a value that contains `{other}` is never expanded.
pub fn fill(template: &str, vars: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let hit = after.find('}').and_then(|j| vars.iter().find(|(k, _)| *k == &after[..j]).map(|(_, v)| (j, v)));
        match hit {
            Some((j, v)) => {
                out.push_str(v);
                rest = &after[j + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Home/warp names: letters (Thai included), digits, `_` and `-`, 1..=32 characters.
pub fn valid_name(s: &str) -> bool {
    let n = s.chars().count();
    n > 0 && n <= 32 && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || ('\u{0E00}'..='\u{0E7F}').contains(&c))
}

/// Money: positive, finite, at most 2 decimals kept. Accepts `1k`, `2.5m`.
pub fn parse_amount(s: &str) -> Option<f64> {
    let s = s.trim().to_lowercase();
    let (num, mul) = match s.chars().last()? {
        'k' => (&s[..s.len() - 1], 1e3),
        'm' => (&s[..s.len() - 1], 1e6),
        'b' => (&s[..s.len() - 1], 1e9),
        _ => (&s[..], 1.0),
    };
    let v = num.parse::<f64>().ok()? * mul;
    (v.is_finite() && v > 0.0 && v < 1e15).then(|| (v * 100.0).round() / 100.0)
}

pub fn money(v: f64, symbol: &str) -> String {
    let neg = v < 0.0;
    let cents = (v.abs() * 100.0).round() as u64;
    let whole = (cents / 100).to_string();
    let mut grouped = String::new();
    for (i, ch) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let frac = cents % 100;
    let body = if frac == 0 { grouped } else { format!("{grouped}.{frac:02}") };
    format!("{}{symbol}{body}", if neg { "-" } else { "" })
}

/// Move money; returns false (and changes nothing) when `from` cannot pay.
pub fn transfer(from: &mut f64, to: &mut f64, amount: f64) -> bool {
    if amount <= 0.0 || *from + 1e-9 < amount {
        return false;
    }
    *from = ((*from - amount) * 100.0).round() / 100.0;
    *to = ((*to + amount) * 100.0).round() / 100.0;
    true
}

// ---- shop -------------------------------------------------------------------

/// Item slots of one shop page (five rows; the sixth holds the arrows).
pub const SHOP_PAGE: usize = 45;
/// Category icons the shop menu can show.
pub const SHOP_MENU: usize = 45;
/// The dearest single item a shop may sell or buy.
pub const SHOP_MAX_PRICE: f64 = 1e9;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ShopCategory {
    /// Name shown on the icon (`&` colors).
    pub title: String,
    /// Item shown in the menu.
    pub icon: String,
    /// Slot in the menu (0..=44); missing = the next free one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ShopItem {
    pub item: String,
    pub category: String,
    /// What a player pays for one; 0 = not for sale.
    pub buy: f64,
    /// What a player gets for one; 0 = the shop does not buy it.
    pub sell: f64,
    /// Place inside the category (0 = first slot of page 1, 45 = first slot of page 2);
    /// missing = the next free one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<u32>,
}

/// shop.toml: the server shop of /shop.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Shop {
    pub title: String,
    pub categories: BTreeMap<String, ShopCategory>,
    pub items: Vec<ShopItem>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShopError {
    /// The shop does not trade this item in that direction.
    NotTraded,
    NoMoney,
    /// The sale would push the balance past `MAX_BALANCE`.
    TooRich,
}

fn shop_price(p: f64) -> Option<f64> {
    (p.is_finite() && p > 0.0 && p <= SHOP_MAX_PRICE).then(|| (p * 100.0).round() / 100.0).filter(|p| *p > 0.0)
}

impl Shop {
    /// The file written on first use.
    pub fn sample(th: bool) -> Shop {
        let cat = |title: &str, icon: &str| ShopCategory { title: title.into(), icon: icon.into(), slot: None };
        let mut categories = BTreeMap::new();
        categories.insert("blocks".to_owned(), cat(if th { "&aบล็อก" } else { "&aBlocks" }, "minecraft:stone"));
        categories.insert("food".to_owned(), cat(if th { "&6อาหาร" } else { "&6Food" }, "minecraft:bread"));
        categories.insert("ores".to_owned(), cat(if th { "&bแร่" } else { "&bOres" }, "minecraft:diamond"));
        let it = |item: &str, category: &str, buy: f64, sell: f64| ShopItem { item: format!("minecraft:{item}"), category: category.into(), buy, sell, slot: None };
        let items = vec![
            it("stone", "blocks", 2.0, 0.5),
            it("cobblestone", "blocks", 1.0, 0.1),
            it("oak_log", "blocks", 4.0, 1.0),
            it("glass", "blocks", 3.0, 0.0),
            it("bread", "food", 5.0, 1.0),
            it("cooked_beef", "food", 8.0, 2.0),
            it("wheat", "food", 0.0, 1.0),
            it("coal", "ores", 10.0, 2.0),
            it("iron_ingot", "ores", 40.0, 10.0),
            it("gold_ingot", "ores", 60.0, 15.0),
            it("diamond", "ores", 400.0, 100.0),
        ];
        Shop { title: if th { "ร้านค้า".into() } else { "Shop".into() }, categories, items }
    }

    /// Drop lines the shop cannot use and return why, one text per dropped or changed line.
    /// A selling price above the buying price would print money: it is lowered to the buying price.
    pub fn clean(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        let cats: Vec<String> = self.categories.keys().cloned().collect();
        for name in &cats {
            if !valid_name(name) {
                self.categories.remove(name);
                notes.push(format!("category {name}: bad name"));
            }
        }
        if self.categories.len() > SHOP_MENU {
            let extra: Vec<String> = self.categories.keys().skip(SHOP_MENU).cloned().collect();
            for name in extra {
                self.categories.remove(&name);
                notes.push(format!("category {name}: more than {SHOP_MENU} categories"));
            }
        }
        let categories = &self.categories;
        self.items.retain_mut(|i| {
            let Some((id, _)) = parse_item(&i.item) else {
                notes.push(format!("item {}: bad item name", i.item));
                return false;
            };
            i.item = id;
            if !categories.contains_key(&i.category) {
                notes.push(format!("item {}: unknown category {}", i.item, i.category));
                return false;
            }
            i.buy = shop_price(i.buy).unwrap_or(0.0);
            i.sell = shop_price(i.sell).unwrap_or(0.0);
            if i.buy > 0.0 && i.sell > i.buy {
                notes.push(format!("item {}: sell {} is above buy {}, lowered", i.item, i.sell, i.buy));
                i.sell = i.buy;
            }
            if i.buy <= 0.0 && i.sell <= 0.0 {
                notes.push(format!("item {}: no usable price", i.item));
                return false;
            }
            true
        });
        notes
    }

    /// Menu slot of every category: its own `slot` when free and inside the menu, else the next free one.
    pub fn menu(&self) -> Vec<(usize, String)> {
        let mut taken = vec![false; SHOP_MENU];
        let mut out: Vec<(usize, String)> = Vec::new();
        let mut later = Vec::new();
        for (name, c) in &self.categories {
            match c.slot.map(|s| s as usize).filter(|s| *s < SHOP_MENU && !taken[*s]) {
                Some(s) => {
                    taken[s] = true;
                    out.push((s, name.clone()));
                }
                None => later.push(name.clone()),
            }
        }
        for name in later {
            if let Some(s) = taken.iter().position(|t| !t) {
                taken[s] = true;
                out.push((s, name));
            }
        }
        out.sort();
        out
    }

    /// Items of a category by place: index in `items` for every slot of every page.
    /// The length is a whole number of pages (at least one).
    pub fn layout(&self, category: &str) -> Vec<Option<usize>> {
        let mine: Vec<usize> = self.items.iter().enumerate().filter(|(_, i)| i.category == category).map(|(n, _)| n).collect();
        let mut out: Vec<Option<usize>> = Vec::new();
        let mut later = Vec::new();
        for n in mine {
            // 100 pages is far more than a menu anyone would page through
            match self.items[n].slot.map(|s| s as usize).filter(|s| *s < SHOP_PAGE * 100) {
                Some(s) if out.get(s).is_none_or(Option::is_none) => {
                    if out.len() <= s {
                        out.resize(s + 1, None);
                    }
                    out[s] = Some(n);
                }
                _ => later.push(n),
            }
        }
        for n in later {
            match out.iter().position(Option::is_none) {
                Some(s) => out[s] = Some(n),
                None => out.push(Some(n)),
            }
        }
        while out.last().is_some_and(Option::is_none) {
            out.pop();
        }
        let pages = out.len().div_ceil(SHOP_PAGE).max(1);
        out.resize(pages * SHOP_PAGE, None);
        out
    }
}

/// Buying `count` at `price` each from a balance: (total, new balance).
pub fn shop_buy(price: f64, count: u32, balance: f64) -> Result<(f64, f64), ShopError> {
    let price = shop_price(price).ok_or(ShopError::NotTraded)?;
    if count == 0 {
        return Err(ShopError::NotTraded);
    }
    let total = (price * f64::from(count) * 100.0).round() / 100.0;
    if !balance.is_finite() || balance + 1e-9 < total {
        return Err(ShopError::NoMoney);
    }
    Ok((total, ((balance - total) * 100.0).round().max(0.0) / 100.0))
}

/// Selling `count` at `price` each into a balance: (total, new balance).
pub fn shop_sell(price: f64, count: u32, balance: f64) -> Result<(f64, f64), ShopError> {
    let price = shop_price(price).ok_or(ShopError::NotTraded)?;
    if count == 0 {
        return Err(ShopError::NotTraded);
    }
    let total = (price * f64::from(count) * 100.0).round() / 100.0;
    if !balance.is_finite() || balance + total > MAX_BALANCE {
        return Err(ShopError::TooRich);
    }
    Ok((total, credit(balance, total)))
}

/// Empty inventory slots `count` items need when a stack holds `max_stack`.
pub fn stacks_needed(count: u32, max_stack: u32) -> u32 {
    count.div_ceil(max_stack.max(1))
}

// ---- plugin ids before 0.6.0 --------------------------------------------------

/// The MCSV plugins were renamed in 0.6.0 (Nexus became Basics, Roster became TabList, …). A node
/// saved under an old id (`nexus:command.fly` in ranks.toml or in a player's file) means the node
/// of the same plugin today. Perms keeps the same table (`perms::node::modern_key`).
const OLD_IDS: [(&str, &str); 13] = [
    ("nexus", "basics"),
    ("charter", "perms"),
    ("passport", "login"),
    ("homestead", "claims"),
    ("chronicle", "blocklog"),
    ("quill", "scripts"),
    ("herald", "discordlink"),
    ("roster", "tablist"),
    ("sculpt", "worldtools"),
    ("pulse", "monitor"),
    ("sweep", "cleaner"),
    ("marquee", "motd"),
    ("atlas", "worldmap"),
];

/// `node` with an old plugin id in front replaced by today's id; anything else unchanged.
pub fn modern_node(node: &str) -> String {
    if let Some((ns, rest)) = node.split_once(':') {
        if let Some((_, new)) = OLD_IDS.iter().find(|(old, _)| *old == ns) {
            return format!("{new}:{rest}");
        }
    }
    node.to_owned()
}

// ---- command aliases --------------------------------------------------------

/// Names an alias may not take: the Login plugin reads these from what the player typed, and a
/// rewritten line would send a password to the command dispatcher and the console log.
pub const RESERVED_ALIASES: &[&str] = &["login", "l", "register", "reg", "changepassword", "changepw", "logout", "unregister", "auth", "authme", "basics"];

/// Longest command an alias may produce.
pub const ALIAS_MAX: usize = 256;

fn alias_word(s: &str) -> bool {
    !s.is_empty() && s.chars().count() <= 32 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn first_word(command: &str) -> String {
    command.split_whitespace().next().unwrap_or("").to_lowercase()
}

/// The aliases that can be used, and the names that were refused (bad name, reserved name, empty or
/// reserved target, or a chain that comes back to itself).
pub fn clean_aliases(raw: &BTreeMap<String, String>) -> (BTreeMap<String, String>, Vec<String>) {
    let mut good = BTreeMap::new();
    let mut bad = Vec::new();
    for (name, target) in raw {
        let n = name.trim().trim_start_matches('/').to_lowercase();
        let t = target.trim().trim_start_matches('/').to_owned();
        let head = first_word(&t);
        let fine = alias_word(&n)
            && !RESERVED_ALIASES.contains(&n.as_str())
            && !head.is_empty()
            && !RESERVED_ALIASES.contains(&head.as_str())
            && t.chars().count() <= ALIAS_MAX
            && !t.chars().any(char::is_control);
        if fine {
            good.insert(n, t);
        } else {
            bad.push(name.clone());
        }
    }
    // A chain that returns to a name it already used never ends: drop every alias on such a chain
    let looping: Vec<String> = good
        .keys()
        .filter(|start| {
            let mut seen = vec![(*start).clone()];
            let mut at = first_word(&good[*start]);
            loop {
                if seen.contains(&at) {
                    return true;
                }
                match good.get(&at) {
                    Some(next) => {
                        seen.push(at);
                        at = first_word(next);
                    }
                    None => return false,
                }
            }
        })
        .cloned()
        .collect();
    for name in looping {
        good.remove(&name);
        bad.push(name);
    }
    bad.sort();
    (good, bad)
}

/// The command an alias stands for, or `None` when the first word is not an alias (or the result
/// would be too long). `command` is what the player typed without the leading slash.
pub fn expand_alias(aliases: &BTreeMap<String, String>, command: &str) -> Option<String> {
    let mut current = command.trim().to_owned();
    let mut used: Vec<String> = Vec::new();
    // `clean_aliases` leaves no loops; the bound is for a table that was not cleaned
    for _ in 0..8 {
        let head = first_word(&current);
        if RESERVED_ALIASES.contains(&head.as_str()) || used.contains(&head) {
            break;
        }
        let Some(target) = aliases.get(&head) else { break };
        let args: Vec<&str> = current.split_whitespace().skip(1).collect();
        let mut vars: Vec<(String, String)> = vec![("args".to_owned(), args.join(" "))];
        for n in 1..=9 {
            vars.push((n.to_string(), args.get(n - 1).map(|s| (*s).to_owned()).unwrap_or_default()));
        }
        let placeholders = vars.iter().any(|(k, _)| target.contains(&format!("{{{k}}}")));
        let refs: Vec<(&str, String)> = vars.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        let mut next = fill(target, &refs).trim().to_owned();
        if !placeholders && !args.is_empty() {
            next.push(' ');
            next.push_str(&args.join(" "));
        }
        if next.chars().count() > ALIAS_MAX || RESERVED_ALIASES.contains(&first_word(&next).as_str()) {
            return None;
        }
        used.push(head);
        current = next;
    }
    (!used.is_empty()).then_some(current)
}

// ---- sudo -------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Sudo {
    /// Say this in chat as the player.
    Chat(String),
    /// Run this command (no leading slash) as the player.
    Command(String),
}

/// What `/sudo <player> <text>` asks for: `c:hello` is chat, anything else a command.
pub fn parse_sudo(text: &str) -> Option<Sudo> {
    let clean: String = text.trim().chars().filter(|c| !c.is_control()).take(ALIAS_MAX).collect();
    let lower = clean.to_lowercase();
    if lower.starts_with("c:") {
        let msg = clean[2..].trim();
        return (!msg.is_empty()).then(|| Sudo::Chat(msg.to_owned()));
    }
    let cmd = clean.trim_start_matches('/').trim();
    (!cmd.is_empty()).then(|| Sudo::Command(cmd.to_owned()))
}

/// Commands nobody may be made to run: Login's (they carry passwords) and /sudo itself.
pub fn sudo_command_ok(command: &str) -> bool {
    let head = first_word(command.trim_start_matches('/'));
    !head.is_empty() && head != "sudo" && head != "basics:sudo" && !RESERVED_ALIASES[..10].contains(&head.as_str())
}

/// Compass point of an offset in blocks (north is -z); empty when on the same spot.
pub fn compass(dx: f64, dz: f64) -> &'static str {
    if dx.abs() < 0.5 && dz.abs() < 0.5 {
        return "";
    }
    // 0 = north, clockwise
    let deg = (dx.atan2(-dz).to_degrees() + 360.0) % 360.0;
    ["N", "NE", "E", "SE", "S", "SW", "W", "NW"][((deg + 22.5) / 45.0) as usize % 8]
}

/// A sender may act for a player whose operator level is not above their own.
pub fn sudo_allowed(sender_level: u8, target_level: u8) -> bool {
    target_level <= sender_level
}

// ---- random teleport ground --------------------------------------------------

/// Whether a player can be put down on this block (the top block of a column).
pub fn safe_ground(block: &str) -> bool {
    let name = block.strip_prefix("minecraft:").unwrap_or(block);
    const BAD: &[&str] = &[
        "air", "cave_air", "void_air", "water", "lava", "bubble_column", "magma_block", "cactus", "fire", "soul_fire",
        "campfire", "soul_campfire", "sweet_berry_bush", "powder_snow", "pointed_dripstone", "wither_rose", "kelp",
        "kelp_plant", "seagrass", "tall_seagrass", "nether_portal", "end_portal", "cobweb",
    ];
    !name.is_empty() && !BAD.contains(&name)
}

/// Columns /rtp looks at around its random point, nearest first: the point itself, then rings
/// 8 blocks apart out to 32 blocks.
pub fn rtp_offsets() -> Vec<(i32, i32)> {
    let mut out = vec![(0, 0)];
    for ring in 1..=4i32 {
        let r = ring * 8;
        for dx in (-r..=r).step_by(8) {
            for dz in (-r..=r).step_by(8) {
                if dx.abs() == r || dz.abs() == r {
                    out.push((dx, dz));
                }
            }
        }
    }
    out
}

/// The first column whose top block is safe. `None` in a slot = that chunk is not loaded.
pub fn pick_ground(tops: &[Option<&str>]) -> Option<usize> {
    tops.iter().position(|b| b.is_some_and(safe_ground))
}

// ---- holograms --------------------------------------------------------------

/// One floating text: the text display entity that shows it and where it was put.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Holo {
    /// Entity UUID as 32 hex digits.
    pub uuid: String,
    pub dim: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub text: String,
}

/// holograms.json
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Holos {
    pub holos: BTreeMap<String, Holo>,
    /// (dimension, entity UUID) of deleted holograms whose entity could not be reached yet.
    pub remove: Vec<(String, String)>,
}

/// Hologram text as typed (`line one|line two` or `\n`) to the text shown.
pub fn holo_text(typed: &str) -> String {
    typed.replace("\\n", "\n").replace('|', "\n")
}

// ---- item damage ------------------------------------------------------------

/// A network VarInt (how Pumpkin hands plugins the `damage` component of an item).
pub fn read_varint(bytes: &[u8]) -> Option<i32> {
    let mut value: u32 = 0;
    for (i, b) in bytes.iter().take(5).enumerate() {
        value |= u32::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Some(value as i32);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts() {
        assert_eq!(parse_amount("10"), Some(10.0));
        assert_eq!(parse_amount("1.239"), Some(1.24));
        assert_eq!(parse_amount("2k"), Some(2000.0));
        assert_eq!(parse_amount("1.5M"), Some(1_500_000.0));
        assert_eq!(parse_amount("-5"), None);
        assert_eq!(parse_amount("0"), None);
        assert_eq!(parse_amount("nan"), None);
        assert_eq!(parse_amount("abc"), None);
    }

    #[test]
    fn burst_limits_a_window() {
        let mut b = Burst::default();
        assert!((0..3).all(|i| b.allow(f64::from(i) * 0.1, 3, 5.0)));
        assert!(!b.allow(0.5, 3, 5.0));
        assert!(!b.allow(4.9, 3, 5.0));
        assert!(b.allow(5.05, 3, 5.0));
        assert!(Burst::default().allow(0.0, 0, 5.0));
    }

    #[test]
    fn balances_stay_bounded() {
        assert_eq!(credit(10.0, 0.005), 10.01);
        assert_eq!(credit(MAX_BALANCE, 1e9), MAX_BALANCE);
        assert_eq!(credit(5.0, -9.0), 0.0);
        let w = Worth::default();
        assert_eq!(w.price("minecraft:diamond"), Some(100.0));
        assert_eq!(w.price("minecraft:dirt"), None);
        assert_eq!(w.price("diamond"), Some(100.0));
        let mut odd = Worth::default();
        odd.items.insert("stone".into(), f64::INFINITY);
        odd.items.insert("sand".into(), 2.0);
        assert_eq!(odd.price("minecraft:stone"), None);
        assert_eq!(odd.price("minecraft:sand"), Some(2.0));
    }

    #[test]
    fn money_format() {
        assert_eq!(money(1234567.5, "$"), "$1,234,567.50");
        assert_eq!(money(12.0, "฿"), "฿12");
        assert_eq!(money(-3.25, "$"), "-$3.25");
    }

    #[test]
    fn transfers() {
        let (mut a, mut b) = (10.0, 0.0);
        assert!(!transfer(&mut a, &mut b, 10.01));
        assert!(transfer(&mut a, &mut b, 10.0));
        assert_eq!((a, b), (0.0, 10.0));
    }

    #[test]
    fn ranks_resolve_with_inheritance_and_denies() {
        let mut r = Ranks::default();
        r.groups.insert("mod".into(), Group { inherits: vec!["vip".into(), "mod".into()], permissions: vec!["-basics:command.fly".into(), "basics:command.mute".into()], ..Default::default() });
        let vip = r.resolve("vip");
        assert_eq!(vip.get("basics:homes.10"), Some(&true));
        let m = r.resolve("mod");
        assert_eq!(m.get("basics:command.fly"), Some(&false));
        assert_eq!(m.get("basics:homes.10"), Some(&true));
        assert_eq!(m.get("basics:command.mute"), Some(&true));
        assert!(r.resolve("nobody").is_empty());
        assert_eq!(r.group_of(Some("vip")), "vip");
        assert_eq!(r.group_of(Some("gone")), "default");
        assert_eq!(r.group_of(None), "default");
        assert_eq!(r.decor("vip").0, "&6[VIP] &r");
    }

    #[test]
    fn nodes_items_durations() {
        assert!(valid_node("basics:homes.10"));
        assert!(valid_node("minecraft:command.*"));
        assert!(!valid_node("basics"));
        assert!(!valid_node("basics:a..b"));
        assert!(!valid_node("*:x"));
        assert_eq!(parse_item("bread 16"), Some(("minecraft:bread".into(), 16)));
        assert_eq!(parse_item("minecraft:stone_sword"), Some(("minecraft:stone_sword".into(), 1)));
        assert_eq!(parse_item("bread 0"), None);
        assert_eq!(parse_item("a b c"), None);
        assert_eq!(parse_item("../x"), None);
        assert_eq!(parse_duration("30s"), Some(30));
        assert_eq!(parse_duration("10"), Some(600));
        assert_eq!(parse_duration("2h"), Some(7200));
        assert_eq!(parse_duration("1w"), Some(604_800));
        assert_eq!(parse_duration("0m"), None);
        assert_eq!(parse_duration("x"), None);
        assert_eq!(fmt_duration(93_784, false), "1d 2h");
        assert_eq!(fmt_duration(65, true), "1 นาที 5 วินาที");
        assert_eq!(fmt_duration(0, false), "0s");
    }

    #[test]
    fn fill_is_single_pass() {
        let out = fill("{from} -> {to}: {message} {x}", &[("from", "{to}".into()), ("to", "Bob".into()), ("message", "{from}".into())]);
        assert_eq!(out, "{to} -> Bob: {from} {x}");
    }

    #[test]
    fn old_plugin_ids_in_nodes() {
        assert_eq!(modern_node("nexus:command.fly"), "basics:command.fly");
        assert_eq!(modern_node("roster:group.vip"), "tablist:group.vip");
        assert_eq!(modern_node("basics:command.fly"), "basics:command.fly");
        assert_eq!(modern_node("minecraft:command.gamemode"), "minecraft:command.gamemode");
        assert_eq!(modern_node("nexus"), "nexus");
        let mut r = Ranks::default();
        r.groups.insert("old".into(), Group { permissions: vec!["nexus:homes.10".into(), "-nexus:command.fly".into()], ..Default::default() });
        let nodes = r.resolve("old");
        assert_eq!(nodes.get("basics:homes.10"), Some(&true));
        assert_eq!(nodes.get("basics:command.fly"), Some(&false));
        assert!(!nodes.keys().any(|k| k.starts_with("nexus:")));
    }

    #[test]
    fn names() {
        assert!(valid_name("home_2"));
        assert!(valid_name("บ้าน"));
        assert!(!valid_name("a b"));
        assert!(!valid_name("../x"));
        assert!(!valid_name(""));
    }

    #[test]
    fn shop_prices_and_limits() {
        assert_eq!(shop_buy(2.5, 4, 10.0), Ok((10.0, 0.0)));
        assert_eq!(shop_buy(2.5, 4, 9.99), Err(ShopError::NoMoney));
        assert_eq!(shop_buy(0.0, 1, 100.0), Err(ShopError::NotTraded));
        assert_eq!(shop_buy(-5.0, 1, 100.0), Err(ShopError::NotTraded));
        assert_eq!(shop_buy(f64::NAN, 1, 100.0), Err(ShopError::NotTraded));
        assert_eq!(shop_buy(f64::INFINITY, 1, 1e12), Err(ShopError::NotTraded));
        assert_eq!(shop_buy(2e9, 1, 1e12), Err(ShopError::NotTraded));
        assert_eq!(shop_buy(1.0, 0, 100.0), Err(ShopError::NotTraded));
        assert_eq!(shop_buy(0.333, 3, 1.0), Ok((0.99, 0.01)));
        assert_eq!(shop_buy(1.0, 1, f64::NAN), Err(ShopError::NoMoney));
        assert_eq!(shop_sell(100.0, 3, 5.5), Ok((300.0, 305.5)));
        assert_eq!(shop_sell(0.0, 3, 5.5), Err(ShopError::NotTraded));
        assert_eq!(shop_sell(1e9, 64, MAX_BALANCE - 10.0), Err(ShopError::TooRich));
        assert_eq!(shop_sell(1.0, 10, MAX_BALANCE - 10.0), Ok((10.0, MAX_BALANCE)));
        // A price below one cent is no price
        assert_eq!(shop_sell(0.001, 5, 0.0), Err(ShopError::NotTraded));
        assert_eq!(stacks_needed(64, 64), 1);
        assert_eq!(stacks_needed(65, 64), 2);
        assert_eq!(stacks_needed(3, 1), 3);
        assert_eq!(stacks_needed(1, 0), 1);
    }

    #[test]
    fn shop_file_is_cleaned() {
        let mut s = Shop::sample(true);
        assert!(s.clean().is_empty());
        let n = s.items.len();
        s.items.push(ShopItem { item: "diamond".into(), category: "ores".into(), buy: 10.0, sell: 50.0, slot: None });
        s.items.push(ShopItem { item: "../x".into(), category: "ores".into(), buy: 1.0, sell: 0.0, slot: None });
        s.items.push(ShopItem { item: "dirt".into(), category: "nowhere".into(), buy: 1.0, sell: 0.0, slot: None });
        s.items.push(ShopItem { item: "sand".into(), category: "blocks".into(), buy: f64::NAN, sell: -1.0, slot: None });
        s.categories.insert("a b".into(), ShopCategory::default());
        let notes = s.clean();
        assert_eq!(notes.len(), 5, "{notes:?}");
        assert_eq!(s.items.len(), n + 1);
        let d = s.items.last().unwrap();
        assert_eq!((d.item.as_str(), d.buy, d.sell), ("minecraft:diamond", 10.0, 10.0));
        assert!(!s.categories.contains_key("a b"));
        // The sample survives a round trip through TOML
        let text = toml::to_string_pretty(&Shop::sample(false)).unwrap();
        let back: Shop = toml::from_str(&text).unwrap();
        assert_eq!(back.items, Shop::sample(false).items);
        // A hand-written file with only some keys loads
        let hand: Shop = toml::from_str("[categories.x]\nicon = \"stone\"\n[[items]]\nitem = \"stone\"\ncategory = \"x\"\nbuy = 3\n").unwrap();
        assert_eq!((hand.items[0].buy, hand.items[0].sell, hand.items[0].slot), (3.0, 0.0, None));
    }

    #[test]
    fn shop_layout_and_pages() {
        let mut s = Shop::default();
        s.categories.insert("a".into(), ShopCategory { slot: Some(13), ..Default::default() });
        s.categories.insert("b".into(), ShopCategory::default());
        s.categories.insert("c".into(), ShopCategory { slot: Some(13), ..Default::default() });
        s.categories.insert("d".into(), ShopCategory { slot: Some(99), ..Default::default() });
        assert_eq!(s.menu(), vec![(0, "b".to_owned()), (1, "c".to_owned()), (2, "d".to_owned()), (13, "a".to_owned())]);
        for n in 0..50 {
            s.items.push(ShopItem { item: format!("minecraft:i{n}"), category: "a".into(), buy: 1.0, sell: 0.0, slot: None });
        }
        s.items.push(ShopItem { item: "minecraft:far".into(), category: "a".into(), buy: 1.0, sell: 0.0, slot: Some(2) });
        s.items.push(ShopItem { item: "minecraft:other".into(), category: "b".into(), buy: 1.0, sell: 0.0, slot: Some(46) });
        let a = s.layout("a");
        assert_eq!(a.len(), 2 * SHOP_PAGE);
        assert_eq!(a[2], Some(50));
        assert_eq!((a[0], a[1], a[3]), (Some(0), Some(1), Some(2)));
        assert_eq!(a.iter().flatten().count(), 51);
        assert_eq!(a[51], None);
        let b = s.layout("b");
        assert_eq!((b.len(), b[46]), (2 * SHOP_PAGE, Some(51)));
        assert_eq!(s.layout("nothing"), vec![None; SHOP_PAGE]);
    }

    #[test]
    fn aliases_expand_once_and_never_loop() {
        let raw: BTreeMap<String, String> = [
            ("hub", "spawn"),
            ("/Day7", "/ptime 7000"),
            ("gift", "pay {1} 100"),
            ("say2", "broadcast [{1}] {args}"),
            ("lobby", "hub"),
            ("a", "b x"),
            ("b", "c"),
            ("c", "a"),
            ("self", "self now"),
            ("login", "spawn"),
            ("pw", "login hunter2"),
            ("bad name", "spawn"),
            ("empty", "  "),
            ("into", "a 1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let (good, bad) = clean_aliases(&raw);
        assert_eq!(good.keys().cloned().collect::<Vec<_>>(), ["day7", "gift", "hub", "lobby", "say2"]);
        assert_eq!(bad, ["a", "b", "bad name", "c", "empty", "into", "login", "pw", "self"]);
        assert_eq!(expand_alias(&good, "hub"), Some("spawn".into()));
        assert_eq!(expand_alias(&good, "HUB Bob"), Some("spawn Bob".into()));
        assert_eq!(expand_alias(&good, "day7"), Some("ptime 7000".into()));
        assert_eq!(expand_alias(&good, "gift Bob extra"), Some("pay Bob 100".into()));
        assert_eq!(expand_alias(&good, "say2 a b"), Some("broadcast [a] a b".into()));
        assert_eq!(expand_alias(&good, "lobby"), Some("spawn".into()));
        assert_eq!(expand_alias(&good, "spawn"), None);
        assert_eq!(expand_alias(&good, ""), None);
        // What the player typed is text, not a template
        assert_eq!(expand_alias(&good, "say2 {args} {1}"), Some("broadcast [{args}] {args} {1}".into()));
        // A table that skipped cleaning still ends, and never turns into a login command
        assert_eq!(expand_alias(&raw, "a 1"), Some("a x 1".into()));
        assert_eq!(expand_alias(&raw, "self"), Some("self now".into()));
        assert_eq!(expand_alias(&raw, "pw"), None);
        assert_eq!(expand_alias(&raw, "login abc"), None);
        let long: BTreeMap<String, String> = [("x".to_owned(), "say {args} {args} {args}".to_owned())].into();
        assert_eq!(expand_alias(&long, &format!("x {}", "y".repeat(120))), None);
    }

    #[test]
    fn welcome_rules() {
        let mut cfg = Config::default();
        assert!(spawn_on_welcome(true, &cfg, true));
        assert!(!spawn_on_welcome(false, &cfg, true));
        assert!(!spawn_on_welcome(true, &cfg, false));
        cfg.first_join_spawn = false;
        assert!(!spawn_on_welcome(true, &cfg, true));
        cfg.spawn_on_join = true;
        assert!(spawn_on_welcome(false, &cfg, true));
        assert!(!spawn_on_welcome(false, &cfg, false));
        assert_eq!(join_template(true, &cfg), "");
        cfg.join_message = "+ {name}".into();
        assert_eq!(join_template(true, &cfg), "+ {name}");
        cfg.first_join_message = "new {name}".into();
        assert_eq!(join_template(true, &cfg), "new {name}");
        assert_eq!(join_template(false, &cfg), "+ {name}");
        cfg.first_join_message = "  ".into();
        assert_eq!(join_template(true, &cfg), "+ {name}");
    }

    #[test]
    fn sudo_rules() {
        assert_eq!(parse_sudo("c:hello there"), Some(Sudo::Chat("hello there".into())));
        assert_eq!(parse_sudo("C: hi"), Some(Sudo::Chat("hi".into())));
        assert_eq!(parse_sudo("/spawn"), Some(Sudo::Command("spawn".into())));
        assert_eq!(parse_sudo("home  a"), Some(Sudo::Command("home  a".into())));
        assert_eq!(parse_sudo("c:"), None);
        assert_eq!(parse_sudo(" / "), None);
        assert!(sudo_command_ok("spawn"));
        assert!(sudo_command_ok("/pay Bob 5"));
        for no in ["login pw", "/register a a", "L pw", "sudo Bob spawn", "auth x", "authme info a", ""] {
            assert!(!sudo_command_ok(no), "{no}");
        }
        assert!(sudo_command_ok("basics reload"));
        assert_eq!(compass(0.0, -10.0), "N");
        assert_eq!(compass(10.0, 0.0), "E");
        assert_eq!(compass(0.0, 10.0), "S");
        assert_eq!(compass(-10.0, 0.0), "W");
        assert_eq!(compass(7.0, 7.0), "SE");
        assert_eq!(compass(-7.0, -7.0), "NW");
        assert_eq!(compass(0.0, 0.0), "");
        assert!(sudo_allowed(2, 2));
        assert!(sudo_allowed(4, 0));
        assert!(!sudo_allowed(2, 3));
        assert!(!sudo_allowed(0, 1));
    }

    #[test]
    fn auto_rank_thresholds() {
        let mut r = Ranks::default();
        let g = |h: f64| Group { after_hours: h, ..Default::default() };
        r.groups.insert("member".into(), g(1.0));
        r.groups.insert("veteran".into(), g(10.0));
        r.groups.insert("admin".into(), Group::default());
        assert_eq!(r.auto_rank(None, 3599), None);
        assert_eq!(r.auto_rank(None, 3600), Some("member".into()));
        assert_eq!(r.auto_rank(None, 36_000), Some("veteran".into()));
        assert_eq!(r.auto_rank(Some("member"), 7200), None);
        assert_eq!(r.auto_rank(Some("member"), 36_000), Some("veteran".into()));
        assert_eq!(r.auto_rank(Some("veteran"), 999_999), None);
        // Ranks given by hand stay: no after-hours of their own
        assert_eq!(r.auto_rank(Some("admin"), 999_999), None);
        assert_eq!(r.auto_rank(Some("vip"), 999_999), None);
        // A rank that no longer exists counts as the default group
        assert_eq!(r.auto_rank(Some("gone"), 3600), Some("member".into()));
        // Bad numbers never promote
        r.groups.insert("odd".into(), g(f64::NAN));
        r.groups.insert("neg".into(), g(-3.0));
        assert_eq!(r.auto_rank(None, 0), None);
        // Old ranks.toml without the key still loads
        let old: Ranks = toml::from_str("[groups.default]\ndefault = true\n[groups.vip]\nprefix = \"x\"\ninherits = [\"default\"]\n").unwrap();
        assert_eq!(old.groups["vip"].after_hours, 0.0);
        assert_eq!(old.auto_rank(None, 999_999), None);
    }

    #[test]
    fn rtp_ground() {
        assert!(safe_ground("minecraft:grass_block"));
        assert!(safe_ground("stone"));
        assert!(safe_ground("minecraft:oak_leaves"));
        for bad in ["minecraft:water", "lava", "minecraft:magma_block", "cactus", "minecraft:air", "powder_snow", ""] {
            assert!(!safe_ground(bad), "{bad}");
        }
        let o = rtp_offsets();
        assert_eq!(o[0], (0, 0));
        assert_eq!(o.len(), 81);
        assert!(o.iter().all(|(x, z)| x.abs() <= 32 && z.abs() <= 32));
        let mut uniq = o.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), o.len());
        assert_eq!(pick_ground(&[Some("minecraft:water"), None, Some("lava"), Some("minecraft:sand"), Some("stone")]), Some(3));
        assert_eq!(pick_ground(&[Some("water"), None]), None);
        assert_eq!(pick_ground(&[]), None);
    }

    #[test]
    fn varints_and_new_config_keys() {
        assert_eq!(holo_text("a|b\\nc"), "a\nb\nc");
        let h: Holos = serde_json::from_str(r#"{"holos":{"x":{"uuid":"ab","dim":"minecraft:overworld","x":1.0,"y":2.0,"z":3.0,"text":"hi"}}}"#).unwrap();
        assert_eq!((h.holos["x"].text.as_str(), h.remove.len()), ("hi", 0));
        assert_eq!(read_varint(&[0]), Some(0));
        assert_eq!(read_varint(&[100]), Some(100));
        assert_eq!(read_varint(&[0xac, 0x02]), Some(300));
        assert_eq!(read_varint(&[0x80]), None);
        assert_eq!(read_varint(&[]), None);
        // A 0.5.0 config has none of the new keys: they take defaults that change nothing
        let old: Config = toml::from_str("language = \"en\"\nteleport-delay = 0\nrules = [\"be kind\"]\n").unwrap();
        assert_eq!((old.language.as_str(), old.teleport_delay), ("en", 0));
        assert!(old.join_message.is_empty() && old.quit_message.is_empty() && old.first_join_message.is_empty());
        assert!(old.aliases.is_empty() && old.warp_cost.is_empty() && old.warp_delay.is_empty() && !old.announce_bossbar);
        // The whole config writes back as TOML (tables last) and reads again
        let mut cfg = Config::default();
        cfg.aliases.insert("hub".into(), "spawn".into());
        cfg.warp_cost.insert("mine".into(), 25.0);
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.aliases.get("hub").map(String::as_str), Some("spawn"));
        assert_eq!(back.warp_cost.get("mine"), Some(&25.0));
        // A 0.5.0 player file loads
        let p: PlayerData = serde_json::from_str(r#"{"name":"Alice","homes":{"home":{"dim":"minecraft:overworld","x":1.0,"y":64.0,"z":2.0}},"balance":12.5,"joined":true,"playtime":77}"#).unwrap();
        assert_eq!((p.balance, p.playtime, p.homes.len()), (12.5, 77, 1));
    }
}

//! TabList settings and the text rules that need no server.
use serde::{Deserialize, Serialize};

/// One group of `config.toml`. A player is in the first group (top to bottom) they qualify for.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Group {
    pub name: String,
    pub prefix: String,
    pub suffix: String,
    /// Operators of this permission level or higher are in the group (0 = operators do not count).
    pub op_level: u8,
    /// Everyone is in this group; put it last.
    pub default: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Sidebar {
    pub enabled: bool,
    pub title: String,
    /// At most 15 lines: {online} {max} {tps} {mspt} {time}
    pub lines: Vec<String>,
}

impl Default for Sidebar {
    fn default() -> Self {
        Sidebar {
            enabled: false,
            title: "&b&lMY SERVER".into(),
            lines: vec!["&7".into(), "&fออนไลน์ &a{online}&7/&a{max}".into(), "&fTPS &a{tps}".into(), "&8".into(), "&eplay.example.com".into()],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// th | en
    pub language: String,
    /// Seconds between updates (1..=60).
    pub interval: u64,
    /// Frames shown in turn, one per update. A frame may span lines (`\n`).
    /// {online} {max} {tps} {mspt} {time} {name} {ping} {group} {world}
    pub header: Vec<String>,
    pub footer: Vec<String>,
    /// Name in the tab list: {prefix} {name} {suffix} {ping} {group}. Empty = leave names alone.
    pub name_format: String,
    /// Sort the tab list by group, in the order written below.
    pub sort: bool,
    /// Show the group prefix and suffix on the name above the head (scoreboard teams).
    pub nametags: bool,
    pub groups: Vec<Group>,
    pub sidebar: Sidebar,
}

impl Default for Config {
    fn default() -> Self {
        let g = |name: &str, prefix: &str, op_level: u8, default: bool| Group { name: name.into(), prefix: prefix.into(), suffix: String::new(), op_level, default };
        Config {
            language: "th".into(),
            interval: 2,
            header: vec!["&b&lMY SERVER\n&7ยินดีต้อนรับ &f{name}".into()],
            footer: vec!["&7ออนไลน์ &a{online}&7/&a{max} &8| &7TPS &a{tps} &8| &7ping &a{ping} ms".into()],
            name_format: "{prefix}{name}{suffix}".into(),
            sort: true,
            nametags: true,
            groups: vec![g("admin", "&c[Admin] &r", 3, false), g("vip", "&6[VIP] &r", 0, false), g("default", "&7", 0, true)],
            sidebar: Sidebar::default(),
        }
    }
}

/// Group names become permission nodes and team names: a-z, 0-9, `_`, 1..=24 characters.
pub fn valid_group(name: &str) -> bool {
    !name.is_empty() && name.len() <= 24 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl Config {
    /// Settings a server can run with: bounded numbers, usable groups, one default group.
    pub fn cleaned(mut self) -> Self {
        self.interval = self.interval.clamp(1, 60);
        self.groups.retain(|g| valid_group(&g.name));
        self.groups.dedup_by(|a, b| a.name == b.name);
        self.groups.truncate(64);
        if !self.groups.iter().any(|g| g.default) {
            self.groups.push(Group { name: "default".into(), default: true, ..Default::default() });
            self.groups.dedup_by(|a, b| a.name == b.name);
            if let Some(last) = self.groups.last_mut() {
                last.default = true;
            }
        }
        self.sidebar.lines.truncate(15);
        self.header.truncate(64);
        self.footer.truncate(64);
        self
    }

    /// Index of the player's group: the first one they have the node for, are operator enough
    /// for, or that takes everyone.
    pub fn group_index(&self, op_level: u8, has_node: impl Fn(&str) -> bool) -> usize {
        self.groups
            .iter()
            .position(|g| g.default || (g.op_level > 0 && op_level >= g.op_level) || has_node(&g.name))
            .unwrap_or(self.groups.len().saturating_sub(1))
    }
}

/// Frame `tick` of an animation; empty text when there are no frames.
pub fn frame(frames: &[String], tick: u64) -> &str {
    if frames.is_empty() { "" } else { &frames[(tick % frames.len() as u64) as usize] }
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

/// Minecraft day time (0 = 06:00) as `HH:MM`.
pub fn clock(time_of_day: u64) -> String {
    let t = (time_of_day % 24_000 + 6000) % 24_000;
    format!("{:02}:{:02}", t / 1000, t % 1000 * 60 / 1000)
}

/// Sidebar rows must differ as text: a repeated row gets invisible reset codes appended.
/// `&` codes become section signs, which is what a score holder name is drawn with.
pub fn sidebar_rows(lines: &[String], vars: &[(&str, String)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in lines.iter().take(15) {
        let mut row = legacy(&fill(line, vars));
        while out.contains(&row) {
            row.push_str("\u{a7}r");
        }
        out.push(row);
    }
    out
}

fn legacy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '&' && it.peek().is_some_and(|n| "0123456789abcdefklmnorABCDEFKLMNOR".contains(*n)) {
            out.push('\u{a7}');
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_resolve_in_order() {
        let c = Config::default();
        assert_eq!(c.group_index(4, |_| false), 0);
        assert_eq!(c.group_index(0, |g| g == "vip"), 1);
        assert_eq!(c.group_index(2, |_| false), 2);
        assert_eq!(c.group_index(0, |g| g == "admin"), 0);
    }

    #[test]
    fn cleaned_drops_bad_groups_and_keeps_a_default() {
        let mut c = Config::default();
        c.groups = vec![Group { name: "Bad Name".into(), ..Default::default() }, Group { name: "mod".into(), ..Default::default() }];
        c.interval = 0;
        let c = c.cleaned();
        assert_eq!(c.interval, 1);
        assert_eq!(c.groups.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["mod", "default"]);
        assert!(c.groups[1].default);
        assert_eq!(c.group_index(0, |_| false), 1);
    }

    #[test]
    fn text_rules() {
        assert_eq!(fill("{a}{b}{c}", &[("a", "{b}".into()), ("b", "x".into())]), "{b}x{c}");
        assert_eq!(frame(&["a".into(), "b".into()], 3), "b");
        assert_eq!(frame(&[], 3), "");
        assert_eq!(clock(0), "06:00");
        assert_eq!(clock(18_000), "00:00");
        assert_eq!(clock(6500), "12:30");
        let rows = sidebar_rows(&["&7".into(), "&7".into(), "x {online}".into()], &[("online", "3".into())]);
        assert_eq!(rows, ["\u{a7}7", "\u{a7}7\u{a7}r", "x 3"]);
    }
}

//! The CoreProtect-style arguments of `/co lookup|rollback|restore|purge`.
use crate::record::Action;
use crate::timefmt;

/// A set of [`Action`]s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ActionSet(pub u32);

impl ActionSet {
    pub fn of(actions: &[Action]) -> ActionSet {
        ActionSet(actions.iter().fold(0, |m, a| m | a.bit()))
    }

    pub fn has(self, action: Action) -> bool {
        self.0 & action.bit() != 0
    }

    pub fn add(&mut self, other: ActionSet) {
        self.0 |= other.0;
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Everything kept in the block log.
    pub fn blocks() -> ActionSet {
        ActionSet::of(&[Action::Break, Action::Place, Action::BucketEmpty, Action::BucketFill, Action::Burn, Action::Explode])
    }

    /// What a lookup shows when no `a:` is given: the world changes, not chat, commands and sessions.
    pub fn default_lookup() -> ActionSet {
        let mut set = ActionSet::blocks();
        set.add(ActionSet::of(&[Action::ContainerAdd, Action::ContainerRemove, Action::Kill]));
        set
    }

    pub fn any_block(self) -> bool {
        self.0 & ActionSet::blocks().0 != 0
    }

    pub fn any_text(self) -> bool {
        self.0 & !ActionSet::blocks().0 != 0
    }
}

/// One `a:` value.
pub fn parse_action(text: &str) -> Option<ActionSet> {
    use Action::*;
    Some(ActionSet::of(match text.to_ascii_lowercase().as_str() {
        "block" | "blocks" => &[Break, Place, Burn, Explode],
        "+block" => &[Place],
        "-block" => &[Break, Burn, Explode],
        "bucket" | "buckets" => &[BucketEmpty, BucketFill],
        "+bucket" => &[BucketEmpty],
        "-bucket" => &[BucketFill],
        "burn" | "fire" => &[Burn],
        "explosion" | "explode" | "tnt" => &[Explode],
        "chat" => &[Chat],
        "command" | "commands" => &[Command],
        "session" | "sessions" => &[Join, Leave],
        "+session" | "login" => &[Join],
        "-session" | "logout" => &[Leave],
        "sign" | "signs" => &[Sign],
        "container" | "containers" => &[ContainerAdd, ContainerRemove],
        "+container" => &[ContainerAdd],
        "-container" => &[ContainerRemove],
        "kill" | "kills" => &[Kill],
        "item" | "items" | "-item" | "drop" => &[Drop],
        "click" | "clicks" | "interaction" => &[Click],
        _ => return None,
    }))
}

/// The `r:` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Radius {
    /// Blocks around the sender (x and z; every height, like CoreProtect).
    Blocks(u32),
    /// Every world.
    Global,
    /// One whole world by key; `None` = the world the sender is in.
    World(Option<String>),
}

/// World key used in file names and `r:#<world>`: `minecraft:the_nether` -> `the_nether`.
pub fn world_key(dimension: &str) -> String {
    let name = dimension.strip_prefix("minecraft:").unwrap_or(dimension);
    let key: String = name.chars().take(48).map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c.to_ascii_lowercase() } else { '_' }).collect();
    if key.is_empty() { "world".into() } else { key }
}

fn parse_radius(text: &str) -> Option<Radius> {
    if let Some(name) = text.strip_prefix('#') {
        // Bukkit world names work too, for people coming from CoreProtect
        return Some(match name.to_ascii_lowercase().as_str() {
            "global" | "all" => Radius::Global,
            "here" | "current" => Radius::World(None),
            "world" | "overworld" => Radius::World(Some("overworld".into())),
            "world_nether" | "nether" | "the_nether" => Radius::World(Some("the_nether".into())),
            "world_the_end" | "end" | "the_end" => Radius::World(Some("the_end".into())),
            "" => return None,
            other => Radius::World(Some(world_key(other))),
        });
    }
    text.parse::<u32>().ok().filter(|r| *r >= 1).map(Radius::Blocks)
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Params {
    /// Lower-case user names (`alice`, `#fire`).
    pub users: Vec<String>,
    /// `(older, newer)` seconds before now.
    pub time: Option<(u64, u64)>,
    pub radius: Option<Radius>,
    pub actions: Option<ActionSet>,
    /// Lower-case block, item or entity names without `minecraft:`.
    pub include: Vec<String>,
    /// Names from `e:`: blocks and users alike.
    pub exclude: Vec<String>,
    /// A bare number: the page.
    pub page: Option<u32>,
    /// `#count`: only say how many rows match.
    pub count: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParamError {
    Time(String),
    Radius(String),
    Action(String),
    Name(String),
    Unknown(String),
    TooMany,
}

const MAX_NAMES: usize = 32;

fn names(value: &str) -> Result<Vec<String>, ParamError> {
    let mut out = Vec::new();
    for raw in value.split(',').filter(|s| !s.is_empty()) {
        let name = raw.to_ascii_lowercase();
        let name = name.strip_prefix("minecraft:").unwrap_or(&name).to_owned();
        let ok = name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '#' | '-' | '.' | ':'));
        if !ok || name.is_empty() {
            return Err(ParamError::Name(crate::record::clean(raw, 32)));
        }
        if !out.contains(&name) {
            out.push(name);
        }
    }
    if out.is_empty() {
        return Err(ParamError::Name(String::new()));
    }
    Ok(out)
}

fn extend(list: &mut Vec<String>, more: Vec<String>) -> Result<(), ParamError> {
    for name in more {
        if !list.contains(&name) {
            list.push(name);
        }
    }
    if list.len() > MAX_NAMES { Err(ParamError::TooMany) } else { Ok(()) }
}

/// Parse the words after the subcommand. `u:Alice`, `u: Alice` and `user:Alice` all work.
pub fn parse(words: &[String]) -> Result<Params, ParamError> {
    let mut p = Params::default();
    if words.len() > 24 {
        return Err(ParamError::TooMany);
    }
    let mut i = 0;
    while i < words.len() {
        let word = words[i].as_str();
        i += 1;
        if word.eq_ignore_ascii_case("#count") {
            p.count = true;
            continue;
        }
        let Some((key, mut value)) = word.split_once(':') else {
            match word.parse::<u32>() {
                Ok(page) if p.page.is_none() => p.page = Some(page),
                _ => return Err(ParamError::Unknown(crate::record::clean(word, 32))),
            }
            continue;
        };
        if value.is_empty() {
            // `u: Alice`
            match words.get(i) {
                Some(next) if !next.contains(':') => {
                    value = next.as_str();
                    i += 1;
                }
                _ => return Err(ParamError::Unknown(crate::record::clean(word, 32))),
            }
        }
        match key.to_ascii_lowercase().as_str() {
            "u" | "user" | "users" => extend(&mut p.users, names(value)?)?,
            "t" | "time" => p.time = Some(timefmt::parse_time(value).ok_or_else(|| ParamError::Time(crate::record::clean(value, 32)))?),
            "r" | "radius" => p.radius = Some(parse_radius(value).ok_or_else(|| ParamError::Radius(crate::record::clean(value, 32)))?),
            "a" | "action" | "actions" => {
                let mut set = p.actions.unwrap_or_default();
                for one in value.split(',').filter(|s| !s.is_empty()) {
                    set.add(parse_action(one).ok_or_else(|| ParamError::Action(crate::record::clean(one, 32)))?);
                }
                if set.is_empty() {
                    return Err(ParamError::Action(String::new()));
                }
                p.actions = Some(set);
            }
            "i" | "b" | "include" | "block" | "blocks" => extend(&mut p.include, names(value)?)?,
            "e" | "exclude" => extend(&mut p.exclude, names(value)?)?,
            _ => return Err(ParamError::Unknown(crate::record::clean(word, 32))),
        }
    }
    Ok(p)
}

/// Commands whose arguments are passwords: only the command name is kept.
const SECRET: &[&str] = &["login", "l", "register", "reg", "changepassword", "changepw", "unregister"];
/// `/auth <sub> …` (Login's admin command) subcommands that carry a password.
const SECRET_ADMIN: &[&str] = &["setpassword", "register", "login", "changepassword", "changepw", "unregister"];

/// A command line as it may be written to the log. Pumpkin hands over `home x` for `/home x`
/// and `/set stone` for `//set stone`; that leading slash is kept.
pub fn mask_command(command: &str) -> String {
    let line = command.trim();
    let bare = line.trim_start_matches('/');
    let slashes = &line[..line.len() - bare.len()];
    let mut words = bare.split_whitespace();
    let Some(first) = words.next() else { return String::new() };
    // `login:login` names the same command
    let name = first.rsplit(':').next().unwrap_or(first).to_ascii_lowercase();
    let rest = words.next();
    if SECRET.contains(&name.as_str()) {
        return if rest.is_some() { format!("{slashes}{first} ***") } else { format!("{slashes}{first}") };
    }
    if matches!(name.as_str(), "auth" | "authme") {
        if let Some(sub) = rest {
            if SECRET_ADMIN.contains(&sub.to_ascii_lowercase().as_str()) {
                return if words.next().is_some() { format!("{slashes}{first} {sub} ***") } else { format!("{slashes}{first} {sub}") };
            }
        }
    }
    line.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn full_line() {
        let p = parse(&w("u:Alice,Bob,#fire t:1d12h r:20 a:-block i:stone,minecraft:oak_log e:dirt 3")).unwrap();
        assert_eq!(p.users, ["alice", "bob", "#fire"]);
        assert_eq!(p.time, Some((129_600, 0)));
        assert_eq!(p.radius, Some(Radius::Blocks(20)));
        assert_eq!(p.actions, Some(ActionSet::of(&[Action::Break, Action::Burn, Action::Explode])));
        assert_eq!(p.include, ["stone", "oak_log"]);
        assert_eq!(p.exclude, ["dirt"]);
        assert_eq!(p.page, Some(3));
        assert!(!p.count);
    }

    #[test]
    fn long_names_and_split_values() {
        let p = parse(&w("user: Alice time:10m radius:#global action:chat,command blocks:tnt #count")).unwrap();
        assert_eq!(p.users, ["alice"]);
        assert_eq!(p.time, Some((600, 0)));
        assert_eq!(p.radius, Some(Radius::Global));
        assert_eq!(p.actions, Some(ActionSet::of(&[Action::Chat, Action::Command])));
        assert_eq!(p.include, ["tnt"]);
        assert!(p.count);
    }

    #[test]
    fn radius_forms() {
        let r = |s: &str| parse(&w(s)).map(|p| p.radius.unwrap());
        assert_eq!(r("r:5"), Ok(Radius::Blocks(5)));
        assert_eq!(r("r:#world"), Ok(Radius::World(Some("overworld".into()))));
        assert_eq!(r("r:#world_nether"), Ok(Radius::World(Some("the_nether".into()))));
        assert_eq!(r("r:#the_end"), Ok(Radius::World(Some("the_end".into()))));
        assert_eq!(r("r:#here"), Ok(Radius::World(None)));
        assert_eq!(r("r:#My:World"), Ok(Radius::World(Some("my_world".into()))));
        assert_eq!(r("r:0"), Err(ParamError::Radius("0".into())));
        assert_eq!(r("r:-3"), Err(ParamError::Radius("-3".into())));
        assert_eq!(r("r:#"), Err(ParamError::Radius("#".into())));
    }

    #[test]
    fn actions() {
        assert_eq!(parse_action("block"), Some(ActionSet::of(&[Action::Break, Action::Place, Action::Burn, Action::Explode])));
        assert_eq!(parse_action("+block"), Some(ActionSet::of(&[Action::Place])));
        assert_eq!(parse_action("session"), Some(ActionSet::of(&[Action::Join, Action::Leave])));
        assert_eq!(parse_action("+container"), Some(ActionSet::of(&[Action::ContainerAdd])));
        assert_eq!(parse_action("click"), Some(ActionSet::of(&[Action::Click])));
        assert_eq!(parse_action("fly"), None);
        assert!(!ActionSet::default_lookup().has(Action::Click));
        assert!(ActionSet::default_lookup().has(Action::Kill) && !ActionSet::default_lookup().has(Action::Chat));
        assert!(ActionSet::of(&[Action::Chat]).any_text() && !ActionSet::of(&[Action::Chat]).any_block());
    }

    #[test]
    fn errors() {
        assert_eq!(parse(&w("t:10")), Err(ParamError::Time("10".into())));
        assert_eq!(parse(&w("a:fly")), Err(ParamError::Action("fly".into())));
        assert_eq!(parse(&w("x:1")), Err(ParamError::Unknown("x:1".into())));
        assert_eq!(parse(&w("hello")), Err(ParamError::Unknown("hello".into())));
        assert_eq!(parse(&w("u:")), Err(ParamError::Unknown("u:".into())));
        assert_eq!(parse(&w("u:a/../b")), Err(ParamError::Name("a/../b".into())));
        assert_eq!(parse(&w("u:{}")), Err(ParamError::Name("{}".into())));
        let many: Vec<String> = (0..40).map(|i| format!("u:p{i}")).collect();
        assert_eq!(parse(&many), Err(ParamError::TooMany));
        let many = format!("u:{}", (0..40).map(|i| format!("p{i}")).collect::<Vec<_>>().join(","));
        assert_eq!(parse(&[many]), Err(ParamError::TooMany));
    }

    #[test]
    fn world_keys() {
        assert_eq!(world_key("minecraft:overworld"), "overworld");
        assert_eq!(world_key("minecraft:the_nether"), "the_nether");
        assert_eq!(world_key("my:../World"), "my____world");
        assert_eq!(world_key(""), "world");
    }

    #[test]
    fn passwords_are_masked() {
        assert_eq!(mask_command("login hunter2"), "login ***");
        assert_eq!(mask_command("/l hunter2"), "/l ***");
        assert_eq!(mask_command("LOGIN hunter2 extra"), "LOGIN ***");
        assert_eq!(mask_command("register pw pw"), "register ***");
        assert_eq!(mask_command("reg pw pw"), "reg ***");
        assert_eq!(mask_command("changepassword old new"), "changepassword ***");
        assert_eq!(mask_command("changepw old new"), "changepw ***");
        assert_eq!(mask_command("unregister pw"), "unregister ***");
        assert_eq!(mask_command("login:login pw"), "login:login ***");
        assert_eq!(mask_command("auth setpassword Bob secret"), "auth setpassword ***");
        assert_eq!(mask_command("authme register Bob secret"), "authme register ***");
        assert_eq!(mask_command("auth reload"), "auth reload");
        assert_eq!(mask_command("auth"), "auth");
        assert_eq!(mask_command("co l u:Alice"), "co l u:Alice");
        assert_eq!(mask_command("/set stone"), "/set stone");
        assert_eq!(mask_command("   "), "");
    }
}

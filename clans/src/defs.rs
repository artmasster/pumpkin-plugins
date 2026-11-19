//! Clans: groups of players with a tag, ranks, a home and their own chat. No server needed here.
use serde::{Deserialize, Serialize};

pub const NS: &str = "clans";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    pub max_members: u32,
    /// Money a new clan costs (Basics); 0 = free.
    pub create_cost: f64,
    /// Seconds a player stands still before `/clan home` takes them.
    pub home_delay: u32,
    /// How clan chat looks: `{tag}`, `{player}`, `{message}`.
    pub chat_format: String,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), max_members: 20, create_cost: 0.0, home_delay: 3, chat_format: "&8[&b{tag}&8] &f{player}&7: &b{message}".into() }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.max_members = self.max_members.clamp(2, 500);
        self.create_cost = if self.create_cost.is_finite() { self.create_cost.max(0.0) } else { 0.0 };
        self.home_delay = self.home_delay.min(60);
        if !self.chat_format.contains("{message}") {
            self.chat_format = Config::default().chat_format;
        }
        self
    }

    pub fn thai(&self) -> bool {
        self.language == "th"
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rank {
    Member,
    Officer,
    Leader,
}

impl Rank {
    pub fn label(self, th: bool) -> &'static str {
        match (self, th) {
            (Rank::Leader, true) => "หัวหน้า",
            (Rank::Leader, false) => "leader",
            (Rank::Officer, true) => "รองหัวหน้า",
            (Rank::Officer, false) => "officer",
            (Rank::Member, true) => "สมาชิก",
            (Rank::Member, false) => "member",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub key: String,
    pub name: String,
    pub rank: Rank,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Home {
    pub world: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clan {
    pub tag: String,
    pub name: String,
    pub members: Vec<Member>,
    pub home: Option<Home>,
    /// Members may hurt each other.
    pub friendly_fire: bool,
    pub made: u64,
}

impl Clan {
    pub fn member(&self, key: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.key == key)
    }

    pub fn rank_of(&self, key: &str) -> Option<Rank> {
        self.member(key).map(|m| m.rank)
    }
}

/// A tag: 2 to 6 letters or digits.
pub fn valid_tag(tag: &str) -> bool {
    (2..=6).contains(&tag.chars().count()) && tag.chars().all(|c| c.is_ascii_alphanumeric())
}

/// A clan's name: 1 to 32 characters, no control characters and no `&` colour codes.
pub fn clean_name(name: &str) -> Option<String> {
    let name: String = name.trim().chars().filter(|c| !c.is_control() && *c != '&' && *c != '§').take(32).collect();
    (!name.is_empty()).then_some(name)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Book {
    pub clans: Vec<Clan>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    BadTag,
    BadName,
    TagTaken,
    InAClan,
    NotInAClan,
    NotAllowed,
    Full,
    NotMember,
    Leader,
}

impl Book {
    pub fn of(&self, key: &str) -> Option<&Clan> {
        self.clans.iter().find(|c| c.member(key).is_some())
    }

    pub fn of_mut(&mut self, key: &str) -> Option<&mut Clan> {
        self.clans.iter_mut().find(|c| c.member(key).is_some())
    }

    pub fn by_tag(&self, tag: &str) -> Option<&Clan> {
        self.clans.iter().find(|c| c.tag.eq_ignore_ascii_case(tag))
    }

    pub fn create(&mut self, tag: &str, name: &str, leader: &str, leader_name: &str, now: u64) -> Result<(), Problem> {
        if !valid_tag(tag) {
            return Err(Problem::BadTag);
        }
        let Some(name) = clean_name(name) else { return Err(Problem::BadName) };
        if self.by_tag(tag).is_some() {
            return Err(Problem::TagTaken);
        }
        if self.of(leader).is_some() {
            return Err(Problem::InAClan);
        }
        self.clans.push(Clan { tag: tag.to_owned(), name, members: vec![Member { key: leader.to_owned(), name: leader_name.to_owned(), rank: Rank::Leader }], home: None, friendly_fire: false, made: now });
        Ok(())
    }

    pub fn join(&mut self, tag: &str, key: &str, name: &str, max: u32) -> Result<(), Problem> {
        if self.of(key).is_some() {
            return Err(Problem::InAClan);
        }
        let clan = self.clans.iter_mut().find(|c| c.tag.eq_ignore_ascii_case(tag)).ok_or(Problem::NotInAClan)?;
        if clan.members.len() >= max as usize {
            return Err(Problem::Full);
        }
        clan.members.push(Member { key: key.to_owned(), name: name.to_owned(), rank: Rank::Member });
        Ok(())
    }

    /// A member leaves. A leader hands over first (or disbands).
    pub fn leave(&mut self, key: &str) -> Result<(), Problem> {
        let clan = self.of_mut(key).ok_or(Problem::NotInAClan)?;
        if clan.rank_of(key) == Some(Rank::Leader) {
            return Err(Problem::Leader);
        }
        clan.members.retain(|m| m.key != key);
        Ok(())
    }

    /// `by` takes `who` out: officers take members out, the leader anyone.
    pub fn kick(&mut self, by: &str, who: &str) -> Result<(), Problem> {
        let clan = self.of_mut(by).ok_or(Problem::NotInAClan)?;
        let mine = clan.rank_of(by).ok_or(Problem::NotInAClan)?;
        let theirs = clan.rank_of(who).ok_or(Problem::NotMember)?;
        if mine < Rank::Officer || theirs >= mine {
            return Err(Problem::NotAllowed);
        }
        clan.members.retain(|m| m.key != who);
        Ok(())
    }

    /// The leader sets a member's rank; `Rank::Leader` hands the clan over.
    pub fn set_rank(&mut self, by: &str, who: &str, rank: Rank) -> Result<(), Problem> {
        let clan = self.of_mut(by).ok_or(Problem::NotInAClan)?;
        if clan.rank_of(by) != Some(Rank::Leader) || by == who {
            return Err(Problem::NotAllowed);
        }
        if clan.member(who).is_none() {
            return Err(Problem::NotMember);
        }
        for m in &mut clan.members {
            if m.key == who {
                m.rank = rank;
            } else if m.key == by && rank == Rank::Leader {
                m.rank = Rank::Officer;
            }
        }
        Ok(())
    }

    pub fn disband(&mut self, by: &str) -> Result<Clan, Problem> {
        let at = self.clans.iter().position(|c| c.member(by).is_some()).ok_or(Problem::NotInAClan)?;
        if self.clans[at].rank_of(by) != Some(Rank::Leader) {
            return Err(Problem::NotAllowed);
        }
        Ok(self.clans.remove(at))
    }

    /// Two players in the same clan that does not allow friendly fire.
    pub fn friends(&self, a: &str, b: &str) -> bool {
        self.of(a).is_some_and(|c| !c.friendly_fire && c.member(b).is_some())
    }
}

/// One line of clan chat.
pub fn chat_line(cfg: &Config, tag: &str, player: &str, message: &str) -> String {
    // What a player writes keeps no colour codes
    let said: String = message.chars().filter(|c| !c.is_control()).take(256).collect::<String>().replace('&', "＆");
    cfg.chat_format.replace("{tag}", tag).replace("{player}", player).replace("{message}", &said)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_and_names() {
        assert!(valid_tag("AB1"));
        assert!(!valid_tag("A"));
        assert!(!valid_tag("TOOLONG"));
        assert!(!valid_tag("a-b"));
        assert_eq!(clean_name(" &cRed  Team "), Some("cRed  Team".into()));
        assert_eq!(clean_name("  "), None);
    }

    #[test]
    fn a_clan_from_start_to_end() {
        let mut b = Book::default();
        assert_eq!(b.create("RED", "Red", "a", "A", 0), Ok(()));
        assert_eq!(b.create("red", "Again", "b", "B", 0), Err(Problem::TagTaken));
        assert_eq!(b.create("BLU", "Blue", "a", "A", 0), Err(Problem::InAClan));
        assert_eq!(b.join("red", "b", "B", 3), Ok(()));
        assert_eq!(b.join("red", "c", "C", 3), Ok(()));
        assert_eq!(b.join("red", "d", "D", 3), Err(Problem::Full));
        assert!(b.friends("a", "b"));
        // A member cannot kick; the leader makes b an officer, who can kick c but not a
        assert_eq!(b.kick("c", "b"), Err(Problem::NotAllowed));
        assert_eq!(b.set_rank("a", "b", Rank::Officer), Ok(()));
        assert_eq!(b.kick("b", "a"), Err(Problem::NotAllowed));
        assert_eq!(b.kick("b", "c"), Ok(()));
        assert_eq!(b.leave("a"), Err(Problem::Leader));
        // Handing over: a becomes an officer
        assert_eq!(b.set_rank("a", "b", Rank::Leader), Ok(()));
        assert_eq!(b.of("a").unwrap().rank_of("a"), Some(Rank::Officer));
        assert_eq!(b.leave("a"), Ok(()));
        assert_eq!(b.disband("b").map(|c| c.tag), Ok("RED".into()));
        assert!(b.clans.is_empty());
    }

    #[test]
    fn chat_keeps_what_players_write_plain() {
        let cfg = Config::default();
        assert_eq!(chat_line(&cfg, "RED", "Al", "hi &4there"), "&8[&bRED&8] &fAl&7: &bhi ＆4there");
    }
}

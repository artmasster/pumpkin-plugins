//! What a lookup, rollback or restore is looking for, and whether a record is it.
use std::collections::{HashMap, HashSet};

use crate::params::ActionSet;
use crate::record::{Action, BlockRec, TextRec};
use crate::store::{REGION_SHIFT, Store};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Area {
    /// Every world.
    Global,
    /// One world by key.
    World(String),
    /// Within `r` blocks of a point on x and z, at any height (CoreProtect's radius).
    Column { world: String, x: i32, z: i32, r: i32 },
    /// One block (the inspector).
    Point { world: String, x: i32, y: i32, z: i32 },
}

impl Area {
    pub fn world(&self) -> Option<&str> {
        match self {
            Area::Global => None,
            Area::World(w) | Area::Column { world: w, .. } | Area::Point { world: w, .. } => Some(w),
        }
    }

    pub fn contains(&self, world: &str, x: i32, y: i32, z: i32) -> bool {
        match self {
            Area::Global => true,
            Area::World(w) => w == world,
            Area::Column { world: w, x: cx, z: cz, r } => w == world && (i64::from(x) - i64::from(*cx)).abs() <= i64::from(*r) && (i64::from(z) - i64::from(*cz)).abs() <= i64::from(*r),
            Area::Point { world: w, x: px, y: py, z: pz } => w == world && (x, y, z) == (*px, *py, *pz),
        }
    }

    /// The region files the area can touch: `(rx range, rz range)`, or `None` for a whole world.
    pub fn regions(&self) -> Option<((i32, i32), (i32, i32))> {
        let (x0, x1, z0, z1) = match self {
            Area::Global | Area::World(_) => return None,
            Area::Column { x, z, r, .. } => (x.saturating_sub(*r), x.saturating_add(*r), z.saturating_sub(*r), z.saturating_add(*r)),
            Area::Point { x, z, .. } => (*x, *x, *z, *z),
        };
        Some(((x0 >> REGION_SHIFT, x1 >> REGION_SHIFT), (z0 >> REGION_SHIFT, z1 >> REGION_SHIFT)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    /// Lower-case user names; empty = anyone.
    pub users: Vec<String>,
    /// Lower-case block, item or entity names; empty = anything.
    pub include: Vec<String>,
    /// Lower-case names to leave out, users and things alike.
    pub exclude: Vec<String>,
    /// Unix seconds, both ends included.
    pub since: u32,
    pub until: u32,
    pub area: Area,
    pub actions: ActionSet,
}

impl Query {
    pub fn everything(area: Area, actions: ActionSet) -> Query {
        Query { users: Vec::new(), include: Vec::new(), exclude: Vec::new(), since: 0, until: u32::MAX, area, actions }
    }
}

/// One line of a lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub time: u32,
    pub user: String,
    pub action: Action,
    pub world: String,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    /// Block, item or entity name, or the chat/command/sign text.
    pub text: String,
    pub amount: u32,
    pub rolled: bool,
    /// Place in its file, to keep records of the same second in the order they happened.
    pub(crate) ord: u64,
}

/// `oak_stairs` of `oak_stairs[facing=north]`.
pub fn block_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

/// A query prepared against the name tables.
pub struct Matcher {
    pub query: Query,
    /// Table ids of the wanted users; `None` = anyone.
    users: Option<HashSet<u32>>,
    excluded_users: HashSet<u32>,
    /// State id -> whether its block passes `include` / `exclude`.
    things: HashMap<u32, bool>,
}

impl Matcher {
    pub fn new(query: Query, store: &Store) -> Matcher {
        let users = (!query.users.is_empty()).then(|| store.users.ids_of(&query.users).into_iter().collect());
        let excluded_users = store.users.ids_of(&query.exclude).into_iter().collect();
        Matcher { query, users, excluded_users, things: HashMap::new() }
    }

    fn thing_ok(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        let name = name.strip_prefix("minecraft:").unwrap_or(&name);
        (self.query.include.is_empty() || self.query.include.iter().any(|n| n == name)) && !self.query.exclude.iter().any(|n| n == name)
    }

    pub fn block(&mut self, store: &Store, world: &str, rec: &BlockRec) -> bool {
        let q = &self.query;
        if !q.actions.has(rec.action) || rec.time < q.since || rec.time > q.until || !q.area.contains(world, rec.x, i32::from(rec.y), rec.z) {
            return false;
        }
        if self.users.as_ref().is_some_and(|u| !u.contains(&rec.user)) || self.excluded_users.contains(&rec.user) {
            return false;
        }
        if q.include.is_empty() && q.exclude.is_empty() {
            return true;
        }
        let state = if rec.action.removes() { rec.old } else { rec.new };
        match self.things.get(&state) {
            Some(ok) => *ok,
            None => {
                let ok = self.thing_ok(block_name(store.states.get(state)));
                self.things.insert(state, ok);
                ok
            }
        }
    }

    pub fn text(&self, rec: &TextRec) -> bool {
        let q = &self.query;
        if !q.actions.has(rec.action) || rec.time < q.since || rec.time > q.until || !q.area.contains(&rec.world, rec.x, rec.y, rec.z) {
            return false;
        }
        let user = rec.user.to_ascii_lowercase();
        if (!q.users.is_empty() && !q.users.contains(&user)) || q.exclude.contains(&user) {
            return false;
        }
        // Chat, commands, signs and sessions have no "thing" to filter on
        match rec.action {
            // An item may carry its data after its name (see `items`)
            Action::ContainerAdd | Action::ContainerRemove => self.thing_ok(crate::items::name(&rec.text)),
            Action::Kill | Action::Drop | Action::Click => self.thing_ok(&rec.text),
            _ => q.include.is_empty(),
        }
    }

    pub fn row(store: &Store, world: &str, rec: &BlockRec, ord: u64) -> Row {
        let state = store.states.get(if rec.action.removes() { rec.old } else { rec.new });
        Row {
            time: rec.time,
            user: store.users.get(rec.user).to_owned(),
            action: rec.action,
            world: world.to_owned(),
            x: rec.x,
            y: i32::from(rec.y),
            z: rec.z,
            text: block_name(state).to_owned(),
            amount: 0,
            rolled: rec.rolled,
            ord,
        }
    }

    pub fn text_row(rec: TextRec, rolled: bool, ord: u64) -> Row {
        Row { time: rec.time, user: rec.user, action: rec.action, world: rec.world, x: rec.x, y: rec.y, z: rec.z, text: rec.text, amount: rec.amount, rolled, ord }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testdir::TestDir;

    fn store(dir: &TestDir) -> Store {
        let mut s = Store::open(&dir.0);
        for name in ["Alice", "Bob", "#fire"] {
            s.users.intern(name);
        }
        for state in ["air", "stone", "oak_stairs[facing=north,half=bottom]", "tnt"] {
            s.states.intern(state);
        }
        s
    }

    fn rec(user: u32, action: Action, old: u32, new: u32) -> BlockRec {
        BlockRec { time: 1000, user, x: 10, y: 64, z: -10, action, rolled: false, old, new }
    }

    fn q(area: Area) -> Query {
        Query::everything(area, ActionSet::blocks())
    }

    #[test]
    fn areas() {
        let col = Area::Column { world: "overworld".into(), x: 0, z: 0, r: 10 };
        assert!(col.contains("overworld", 10, -64, -10));
        assert!(col.contains("overworld", 0, 319, 0));
        assert!(!col.contains("overworld", 11, 64, 0));
        assert!(!col.contains("the_nether", 0, 64, 0));
        assert_eq!(col.regions(), Some(((-1, 0), (-1, 0))));
        let far = Area::Column { world: "overworld".into(), x: 1000, z: -1000, r: 20 };
        assert_eq!(far.regions(), Some(((1, 1), (-2, -2))));
        let point = Area::Point { world: "overworld".into(), x: 513, y: 5, z: -1 };
        assert!(point.contains("overworld", 513, 5, -1) && !point.contains("overworld", 513, 6, -1));
        assert_eq!(point.regions(), Some(((1, 1), (-1, -1))));
        assert_eq!(Area::Global.regions(), None);
        assert!(Area::World("the_end".into()).contains("the_end", 1, 2, 3));
        // The edge of the world does not overflow
        let edge = Area::Column { world: "w".into(), x: i32::MAX, z: i32::MIN, r: 100 };
        assert!(edge.contains("w", i32::MAX, 0, i32::MIN));
        assert!(edge.regions().is_some());
    }

    #[test]
    fn users_and_time() {
        let dir = TestDir::new("match-users");
        let s = store(&dir);
        let mut m = Matcher::new(Query { users: vec!["bob".into(), "#fire".into()], ..q(Area::Global) }, &s);
        assert!(!m.block(&s, "overworld", &rec(0, Action::Place, 0, 1)));
        assert!(m.block(&s, "overworld", &rec(1, Action::Place, 0, 1)));
        assert!(m.block(&s, "overworld", &rec(2, Action::Burn, 1, 0)));
        // A user nobody has heard of matches nothing, not everything
        let mut m = Matcher::new(Query { users: vec!["nobody".into()], ..q(Area::Global) }, &s);
        assert!(!m.block(&s, "overworld", &rec(0, Action::Place, 0, 1)));
        let mut m = Matcher::new(Query { since: 1001, ..q(Area::Global) }, &s);
        assert!(!m.block(&s, "overworld", &rec(0, Action::Place, 0, 1)));
        let mut m = Matcher::new(Query { since: 900, until: 1000, ..q(Area::Global) }, &s);
        assert!(m.block(&s, "overworld", &rec(0, Action::Place, 0, 1)));
        let mut m = Matcher::new(Query { exclude: vec!["alice".into()], ..q(Area::Global) }, &s);
        assert!(!m.block(&s, "overworld", &rec(0, Action::Place, 0, 1)));
        assert!(m.block(&s, "overworld", &rec(1, Action::Place, 0, 1)));
    }

    #[test]
    fn blocks_and_actions() {
        let dir = TestDir::new("match-blocks");
        let s = store(&dir);
        let mut m = Matcher::new(Query { include: vec!["oak_stairs".into()], ..q(Area::Global) }, &s);
        // A break is about the block that was there, a place about the one that came
        assert!(m.block(&s, "overworld", &rec(0, Action::Break, 2, 0)));
        assert!(m.block(&s, "overworld", &rec(0, Action::Place, 0, 2)));
        assert!(!m.block(&s, "overworld", &rec(0, Action::Place, 2, 1)));
        let mut m = Matcher::new(Query { exclude: vec!["tnt".into()], ..q(Area::Global) }, &s);
        assert!(!m.block(&s, "overworld", &rec(0, Action::Place, 0, 3)));
        assert!(m.block(&s, "overworld", &rec(0, Action::Place, 0, 1)));
        let mut m = Matcher::new(Query { actions: ActionSet::of(&[Action::Break]), ..q(Area::Global) }, &s);
        assert!(!m.block(&s, "overworld", &rec(0, Action::Place, 0, 1)));
        assert!(m.block(&s, "overworld", &rec(0, Action::Break, 1, 0)));
        let row = Matcher::row(&s, "overworld", &rec(1, Action::Break, 2, 0), 0);
        assert_eq!((row.user.as_str(), row.text.as_str(), row.y), ("Bob", "oak_stairs", 64));
    }

    #[test]
    fn text_records() {
        let dir = TestDir::new("match-text");
        let s = store(&dir);
        let rec = |action, user: &str, text: &str| TextRec { time: 1000, action, user: user.into(), world: "overworld".into(), x: 0, y: 64, z: 0, amount: 1, text: text.into() };
        let all = ActionSet(u32::MAX);
        let m = Matcher::new(Query { users: vec!["alice".into()], ..Query::everything(Area::Global, all) }, &s);
        assert!(m.text(&rec(Action::Chat, "Alice", "hello")));
        assert!(!m.text(&rec(Action::Chat, "Bob", "hello")));
        let m = Matcher::new(Query { include: vec!["diamond".into()], ..Query::everything(Area::Global, all) }, &s);
        assert!(m.text(&rec(Action::ContainerRemove, "Bob", "diamond")));
        assert!(m.text(&rec(Action::ContainerRemove, "Bob", "minecraft:diamond")));
        assert!(!m.text(&rec(Action::ContainerRemove, "Bob", "dirt")));
        // An enchanted diamond is still a diamond to `i:diamond`
        assert!(m.text(&rec(Action::ContainerRemove, "Bob", "diamond|000100000001ff")));
        assert!(!m.text(&rec(Action::Chat, "Bob", "diamond")));
        let m = Matcher::new(Query { include: vec!["oak_door".into()], ..Query::everything(Area::Global, all) }, &s);
        assert!(m.text(&rec(Action::Click, "Bob", "oak_door")) && !m.text(&rec(Action::Click, "Bob", "lever")));
        let m = Matcher::new(Query::everything(Area::Column { world: "overworld".into(), x: 100, z: 0, r: 5 }, ActionSet::of(&[Action::Chat])), &s);
        assert!(!m.text(&rec(Action::Chat, "Bob", "far away")));
        assert!(!m.text(&rec(Action::Command, "Bob", "home")));
    }
}

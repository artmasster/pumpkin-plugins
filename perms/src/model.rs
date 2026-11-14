//! Groups, users, tracks and how a player's effective permissions come out of them (plain Rust).
//!
//! Resolution follows LuckPerms: the user's own nodes first, then parent groups depth-first with
//! the heaviest parent first; the first holder that has a key decides it. An exact key beats any
//! wildcard, and of the wildcards the longest wins.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::node::{Ctx, Kind, MAX_NAMES, MAX_NODES, MAX_REGEX_NODES, Node, compile_regex, expand_shorthand, kind, regex_pattern, valid_name};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub nodes: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameEntry {
    pub uuid: String,
    pub name: String,
}

/// Who a command edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    User(String),
    Group(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum SetResult {
    Added,
    Replaced,
    Unchanged,
    Full,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TrackMove {
    /// Was on no group of the track: put on the first one.
    Joined(String),
    Moved(String, String),
    /// Demoted off the first group.
    Left(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum TrackError {
    NoTrack,
    Empty,
    /// The user is in several groups of the track.
    Ambiguous,
    AtEnd,
    NotOnTrack,
    MissingGroup(String),
}

#[derive(Clone, Debug)]
pub struct RegexNode {
    pub key: String,
    pub re: regex_lite::Regex,
    pub value: bool,
    pub src: u16,
}

/// What applies to one player in one context, ready for O(1) checks.
#[derive(Clone, Debug, Default)]
pub struct Effective {
    /// Key → (value, index into `holders`).
    pub exact: HashMap<String, (bool, u16)>,
    /// Wildcards by the text before `*` (`basics:command.`, `basics:`, `` for a bare `*`).
    pub wild: HashMap<String, (bool, u16)>,
    /// Regex nodes (`r=…`) in holder order; asked after the exact keys and before the wildcards.
    pub regex: Vec<RegexNode>,
    /// `user`, then the inherited groups in order.
    pub holders: Vec<String>,
    pub prefix: String,
    pub suffix: String,
    pub primary: String,
    pub weight: i32,
    pub meta: BTreeMap<String, String>,
}

impl Effective {
    /// Perms's answer for a node, or None when nothing here speaks about it (Pumpkin's own
    /// answer then stands: op level, the node's registered default, other plugins).
    ///
    /// `exempt` holds node prefixes that a granting wildcard must not reach unless the wildcard
    /// is at least as long as the prefix (`tablist:group.*` reaches `tablist:group.admin`, `*` and
    /// `tablist:*` do not). Denying wildcards and exact nodes always apply.
    pub fn check(&self, node: &str, exempt: &[String]) -> Option<bool> {
        if let Some((v, _)) = self.exact.get(node) {
            return Some(*v);
        }
        if !self.regex.is_empty() {
            if let Some(r) = self.regex_match(node, exempt) {
                return Some(r.value);
            }
        }
        if self.wild.is_empty() {
            return None;
        }
        self.wild_match(node, exempt).map(|(v, _, _)| v)
    }

    /// The first regex node that matches. A granting regex never reaches an exempt prefix: a
    /// pattern cannot show that it "means" the prefix the way a long enough wildcard does.
    fn regex_match(&self, node: &str, exempt: &[String]) -> Option<&RegexNode> {
        let guarded = exempt.iter().any(|e| !e.is_empty() && node.starts_with(e.as_str()));
        self.regex.iter().find(|r| !(r.value && guarded) && r.re.is_match(node))
    }

    fn wild_match(&self, node: &str, exempt: &[String]) -> Option<(bool, u16, usize)> {
        let guard = exempt.iter().filter(|e| !e.is_empty() && node.starts_with(e.as_str())).map(String::len).max().unwrap_or(0);
        let bytes = node.as_bytes();
        for i in (0..bytes.len().saturating_sub(1)).rev() {
            if bytes[i] != b'.' && bytes[i] != b':' {
                continue;
            }
            if let Some((v, src)) = self.wild.get(&node[..=i]) {
                if *v && i + 1 < guard {
                    continue;
                }
                return Some((*v, *src, i + 1));
            }
        }
        match self.wild.get("") {
            Some((v, src)) if !(*v && guard > 0) => Some((*v, *src, 0)),
            _ => None,
        }
    }

    /// Like `check`, with the holder that decided and the key that matched.
    pub fn explain(&self, node: &str, exempt: &[String]) -> Option<(bool, String, String)> {
        let holder = |i: u16| self.holders.get(i as usize).cloned().unwrap_or_default();
        if let Some((v, src)) = self.exact.get(node) {
            return Some((*v, holder(*src), node.to_owned()));
        }
        if let Some(r) = self.regex_match(node, exempt) {
            return Some((r.value, holder(r.src), r.key.clone()));
        }
        self.wild_match(node, exempt).map(|(v, src, len)| (v, holder(src), format!("{}*", &node[..len])))
    }

    /// Same answers to every check (who the answer came from does not matter).
    pub fn same_perms(&self, other: &Effective) -> bool {
        fn eq(a: &HashMap<String, (bool, u16)>, b: &HashMap<String, (bool, u16)>) -> bool {
            a.len() == b.len() && a.iter().all(|(k, (v, _))| b.get(k).is_some_and(|(w, _)| v == w))
        }
        let same_regex = self.regex.len() == other.regex.len() && self.regex.iter().zip(&other.regex).all(|(a, b)| a.key == b.key && a.value == b.value);
        eq(&self.exact, &other.exact) && eq(&self.wild, &other.wild) && same_regex
    }

    pub fn same_meta(&self, other: &Effective) -> bool {
        self.prefix == other.prefix && self.suffix == other.suffix && self.primary == other.primary && self.weight == other.weight
    }
}

#[derive(Clone, Debug, Default)]
pub struct Data {
    pub groups: BTreeMap<String, Vec<Node>>,
    /// By UUID text. Only users with something of their own are kept.
    pub users: BTreeMap<String, User>,
    pub tracks: BTreeMap<String, Vec<String>>,
    /// Lowercase name → UUID, for players who joined at least once.
    pub names: BTreeMap<String, NameEntry>,
    pub default_group: String,
    /// The earliest expiry of any node, `u64::MAX` when there is none.
    pub next_expiry: u64,
    pub dirty_groups: BTreeSet<String>,
    pub dirty_users: BTreeSet<String>,
    pub dirty_tracks: bool,
    pub dirty_names: bool,
    /// A bulk update that was counted and waits for `/lp bulkupdate confirm <id>`.
    pub pending_bulk: Option<PendingBulk>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    Users,
    Groups,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Ne,
    Like,
    NotLike,
}

/// `permission <cmp> <value>`: which nodes a bulk update touches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Constraint {
    pub cmp: Cmp,
    pub value: String,
}

impl Constraint {
    pub fn matches(&self, key: &str) -> bool {
        match self.cmp {
            Cmp::Eq => key == self.value,
            Cmp::Ne => key != self.value,
            Cmp::Like => like(&self.value, key),
            Cmp::NotLike => !like(&self.value, key),
        }
    }
}

/// SQL LIKE: `%` is any run of characters, `_` is any one character.
pub fn like(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut back: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '_' || p[pi] == t[ti]) && p[pi] != '%' {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '%' {
            back = Some((pi, ti));
            pi += 1;
        } else if let Some((bp, bt)) = back {
            pi = bp + 1;
            ti = bt + 1;
            back = Some((bp, bt + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '%')
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BulkAction {
    Delete,
    /// Give every matching node this key.
    Rename(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingBulk {
    pub id: String,
    /// UUID of the player who asked, or `console`.
    pub actor: String,
    pub expires: u64,
    pub scope: Scope,
    pub constraint: Constraint,
    pub action: BulkAction,
    /// The request as it was typed, for the action log.
    pub text: String,
}

/// The winning node per key inside one holder, in file order.
fn winners<'a>(nodes: &'a [Node], ctx: &Ctx, now: u64) -> Vec<&'a Node> {
    let mut best: HashMap<&str, usize> = HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        if !n.active(now) || !n.applies(ctx) {
            continue;
        }
        match best.get(n.key.as_str()) {
            Some(&j) if nodes[j].rank() >= n.rank() => {}
            _ => {
                best.insert(&n.key, i);
            }
        }
    }
    nodes.iter().enumerate().filter(|(i, n)| best.get(n.key.as_str()) == Some(i)).map(|(_, n)| n).collect()
}

impl Data {
    pub fn new(default_group: &str) -> Self {
        let mut d = Data { default_group: default_group.to_owned(), next_expiry: u64::MAX, ..Default::default() };
        d.ensure_default();
        d
    }

    /// The default group always exists.
    pub fn ensure_default(&mut self) {
        if !valid_name(&self.default_group) {
            self.default_group = "default".into();
        }
        if !self.groups.contains_key(&self.default_group) {
            self.groups.insert(self.default_group.clone(), Vec::new());
            self.dirty_groups.insert(self.default_group.clone());
        }
    }

    pub fn nodes(&self, t: &Target) -> &[Node] {
        match t {
            Target::User(u) => self.users.get(u).map_or(&[], |u| u.nodes.as_slice()),
            Target::Group(g) => self.groups.get(g).map_or(&[], Vec::as_slice),
        }
    }

    fn nodes_mut(&mut self, t: &Target) -> &mut Vec<Node> {
        match t {
            Target::User(u) => {
                if !self.users.contains_key(u) {
                    let name = self.user_name(u);
                    self.users.insert(u.clone(), User { name, nodes: Vec::new() });
                }
                self.dirty_users.insert(u.clone());
                &mut self.users.get_mut(u).expect("just inserted").nodes
            }
            Target::Group(g) => {
                self.dirty_groups.insert(g.clone());
                self.groups.entry(g.clone()).or_default()
            }
        }
    }

    /// Users left with no nodes go away (their file too).
    fn tidy(&mut self, t: &Target) {
        if let Target::User(u) = t {
            if self.users.get(u).is_some_and(|x| x.nodes.is_empty()) {
                self.users.remove(u);
                self.dirty_users.insert(u.clone());
            }
        }
    }

    /// Add or replace the node with the same key, context and kind of lifetime.
    pub fn set_node(&mut self, t: &Target, node: Node) -> SetResult {
        let list = self.nodes_mut(t);
        let at = list.iter().position(|n| n.key == node.key && n.context == node.context && n.is_temp() == node.is_temp());
        let res = match at {
            Some(i) if list[i] == node => SetResult::Unchanged,
            Some(i) => {
                list[i] = node;
                SetResult::Replaced
            }
            None if list.len() >= MAX_NODES => SetResult::Full,
            None => {
                list.push(node);
                SetResult::Added
            }
        };
        self.tidy(t);
        self.refresh_expiry();
        res
    }

    /// Remove every node the test picks; returns them.
    pub fn remove_where(&mut self, t: &Target, pick: impl Fn(&Node) -> bool) -> Vec<Node> {
        if !self.nodes(t).iter().any(&pick) {
            return Vec::new();
        }
        let list = self.nodes_mut(t);
        let mut gone = Vec::new();
        list.retain(|n| {
            if pick(n) {
                gone.push(n.clone());
                false
            } else {
                true
            }
        });
        self.tidy(t);
        self.refresh_expiry();
        gone
    }

    pub fn refresh_expiry(&mut self) {
        let all = self.groups.values().flatten().chain(self.users.values().flat_map(|u| u.nodes.iter()));
        self.next_expiry = all.filter(|n| n.is_temp()).map(|n| n.expiry).min().unwrap_or(u64::MAX);
    }

    /// Drop nodes whose time is up. True when something went.
    pub fn purge(&mut self, now: u64) -> bool {
        let mut changed = false;
        for (name, nodes) in &mut self.groups {
            let before = nodes.len();
            nodes.retain(|n| n.active(now));
            if nodes.len() != before {
                self.dirty_groups.insert(name.clone());
                changed = true;
            }
        }
        for (id, user) in &mut self.users {
            let before = user.nodes.len();
            user.nodes.retain(|n| n.active(now));
            if user.nodes.len() != before {
                self.dirty_users.insert(id.clone());
                changed = true;
            }
        }
        self.users.retain(|_, u| !u.nodes.is_empty());
        self.refresh_expiry();
        changed
    }

    /// The heaviest `weight.<n>` a group holds without a context; 0 when it has none.
    pub fn weight(&self, group: &str) -> i32 {
        self.groups
            .get(group)
            .into_iter()
            .flatten()
            .filter(|n| n.value && n.context.is_empty())
            .filter_map(|n| match n.kind() {
                Kind::Weight(w) => Some(w),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    }

    pub fn display_name(&self, group: &str) -> Option<&str> {
        self.groups.get(group)?.iter().find_map(|n| match n.kind() {
            Kind::DisplayName(d) if n.value => Some(d),
            _ => None,
        })
    }

    /// Parent groups a holder's nodes name right now, heaviest first. Groups that do not exist are skipped.
    pub fn parents<'a>(&'a self, nodes: &'a [Node], ctx: &Ctx, now: u64) -> Vec<&'a str> {
        let mut out: Vec<(&str, i32)> = winners(nodes, ctx, now)
            .into_iter()
            .filter(|n| n.value)
            .filter_map(|n| match n.kind() {
                Kind::Group(g) => self.groups.get_key_value(g).map(|(k, _)| (k.as_str(), self.weight(g))),
                _ => None,
            })
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        out.dedup();
        out.into_iter().map(|(g, _)| g).collect()
    }

    fn walk<'a>(&'a self, nodes: &'a [Node], ctx: &Ctx, now: u64, seen: &mut HashSet<&'a str>, order: &mut Vec<&'a str>, depth: u32) {
        if depth > 64 {
            return;
        }
        for g in self.parents(nodes, ctx, now) {
            // `seen` cuts cycles: a group is entered once however it is reached
            if seen.insert(g) {
                order.push(g);
                if let Some(next) = self.groups.get(g) {
                    self.walk(next, ctx, now, seen, order, depth + 1);
                }
            }
        }
    }

    /// Groups a user inherits, in the order they are asked. The default group is always in it.
    pub fn inherited(&self, uuid: &str, ctx: &Ctx, now: u64) -> Vec<&str> {
        let own = self.users.get(uuid).map_or(&[][..], |u| u.nodes.as_slice());
        self.inherited_from(own, ctx, now)
    }

    fn inherited_from<'a>(&'a self, own: &'a [Node], ctx: &Ctx, now: u64) -> Vec<&'a str> {
        let mut seen = HashSet::new();
        let mut order = Vec::new();
        self.walk(own, ctx, now, &mut seen, &mut order, 0);
        if let Some((name, nodes)) = self.groups.get_key_value(&self.default_group) {
            if seen.insert(name.as_str()) {
                order.push(name.as_str());
                self.walk(nodes, ctx, now, &mut seen, &mut order, 0);
            }
        }
        order
    }

    /// The user's main group: the heaviest group they are directly in, else the default group.
    pub fn primary(&self, uuid: &str, ctx: &Ctx, now: u64) -> &str {
        let own = self.users.get(uuid).map_or(&[][..], |u| u.nodes.as_slice());
        self.parents(own, ctx, now).first().copied().unwrap_or(self.default_group.as_str())
    }

    pub fn resolve(&self, uuid: &str, ctx: &Ctx, now: u64) -> Effective {
        let own = self.users.get(uuid).map_or(&[][..], |u| u.nodes.as_slice());
        let mut eff = self.resolve_nodes(own, "user", ctx, now);
        eff.primary = self.primary(uuid, ctx, now).to_owned();
        eff.weight = self.weight(&eff.primary);
        eff
    }

    /// What a group gives by itself and through its parents (for `/lp group … permission check`).
    pub fn resolve_group(&self, group: &str, ctx: &Ctx, now: u64) -> Effective {
        let own = self.groups.get(group).map_or(&[][..], Vec::as_slice);
        let mut eff = self.resolve_nodes(own, group, ctx, now);
        eff.primary = group.to_owned();
        eff.weight = self.weight(group);
        eff
    }

    fn resolve_nodes(&self, own: &[Node], label: &str, ctx: &Ctx, now: u64) -> Effective {
        let order = self.inherited_from(own, ctx, now);
        let mut eff = Effective { holders: vec![label.to_owned()], ..Default::default() };
        eff.holders.extend(order.iter().map(|g| (*g).to_owned()));
        // (priority, text, holder, temporary): on one holder a temporary one beats a permanent
        // one of the same priority, like a temporary node beats a permanent one of the same key
        let mut prefix: Option<(i32, &str, usize, bool)> = None;
        let mut suffix: Option<(i32, &str, usize, bool)> = None;
        let mut meta_from: HashMap<&str, (usize, bool)> = HashMap::new();
        let better = |old: Option<(i32, &str, usize, bool)>, p: i32, i: usize, temp: bool| match old {
            None => true,
            Some((bp, _, bi, btemp)) => p > bp || (p == bp && bi == i && temp && !btemp),
        };
        // A group reached again through a cycle is simply read twice: the first holder of a key keeps it
        let lists = std::iter::once(own).chain(order.iter().map(|g| self.groups.get(*g).map_or(&[][..], Vec::as_slice)));
        for (i, nodes) in lists.enumerate() {
            let src = i.min(u16::MAX as usize) as u16;
            for n in winners(nodes, ctx, now) {
                match n.kind() {
                    Kind::Perm | Kind::Group(_) => {
                        if eff.exact.contains_key(&n.key) {
                            continue;
                        }
                        eff.exact.insert(n.key.clone(), (n.value, src));
                        if let Some(pattern) = regex_pattern(&n.key) {
                            if eff.regex.len() < MAX_REGEX_NODES {
                                if let Some(re) = compile_regex(pattern) {
                                    eff.regex.push(RegexNode { key: n.key.clone(), re, value: n.value, src });
                                }
                            }
                        } else if let Some(keys) = expand_shorthand(&n.key) {
                            // `kit.(a|b)` stands for both nodes: they go in as ordinary keys, so
                            // a check stays one lookup
                            for key in keys {
                                if let Some(stem) = wild_stem(&key) {
                                    eff.wild.entry(stem.to_owned()).or_insert((n.value, src));
                                }
                                eff.exact.entry(key).or_insert((n.value, src));
                            }
                        } else if let Some(stem) = wild_stem(&n.key) {
                            eff.wild.entry(stem.to_owned()).or_insert((n.value, src));
                        }
                    }
                    Kind::Prefix(p, text) if n.value => {
                        if better(prefix, p, i, n.is_temp()) {
                            prefix = Some((p, text, i, n.is_temp()));
                        }
                    }
                    Kind::Suffix(p, text) if n.value => {
                        if better(suffix, p, i, n.is_temp()) {
                            suffix = Some((p, text, i, n.is_temp()));
                        }
                    }
                    Kind::Meta(k, v) if n.value => {
                        let take = match meta_from.get(k) {
                            None => true,
                            Some((holder, temp)) => *holder == i && n.is_temp() && !temp,
                        };
                        if take {
                            meta_from.insert(k, (i, n.is_temp()));
                            eff.meta.insert(k.to_owned(), v.to_owned());
                        }
                    }
                    _ => {}
                }
            }
        }
        eff.prefix = prefix.map(|(_, t, _, _)| t.to_owned()).unwrap_or_default();
        eff.suffix = suffix.map(|(_, t, _, _)| t.to_owned()).unwrap_or_default();
        eff
    }

    // ---- names ----

    pub fn lookup_user(&self, name_or_uuid: &str) -> Option<String> {
        if let Some(e) = self.names.get(&name_or_uuid.to_lowercase()) {
            return Some(e.uuid.clone());
        }
        let id = crate::node::parse_uuid(name_or_uuid)?;
        (self.users.contains_key(&id) || self.names.values().any(|e| e.uuid == id)).then_some(id)
    }

    pub fn user_name(&self, uuid: &str) -> String {
        if let Some(u) = self.users.get(uuid) {
            if !u.name.is_empty() {
                return u.name.clone();
            }
        }
        self.names.values().find(|e| e.uuid == uuid).map_or_else(|| uuid.to_owned(), |e| e.name.clone())
    }

    /// Remember a player who is online. True when the index changed.
    pub fn note_name(&mut self, uuid: &str, name: &str) -> bool {
        let lower = name.to_lowercase();
        if self.names.get(&lower).is_some_and(|e| e.uuid == uuid && e.name == name) {
            return false;
        }
        if self.names.len() >= MAX_NAMES && !self.names.contains_key(&lower) {
            return false;
        }
        // A renamed account keeps its data: the old name stops pointing at it
        self.names.retain(|_, e| e.uuid != uuid);
        self.names.insert(lower, NameEntry { uuid: uuid.to_owned(), name: name.to_owned() });
        if let Some(u) = self.users.get_mut(uuid) {
            if u.name != name {
                u.name = name.to_owned();
                self.dirty_users.insert(uuid.to_owned());
            }
        }
        self.dirty_names = true;
        true
    }

    // ---- groups ----

    pub fn create_group(&mut self, name: &str) -> bool {
        if self.groups.contains_key(name) {
            return false;
        }
        self.groups.insert(name.to_owned(), Vec::new());
        self.dirty_groups.insert(name.to_owned());
        true
    }

    /// Every holder that has a `group.<name>` node: users (uuid, name) and groups.
    pub fn members(&self, group: &str) -> (Vec<(String, String)>, Vec<String>) {
        let is = |n: &Node| n.value && matches!(n.kind(), Kind::Group(g) if g == group);
        let users = self.users.iter().filter(|(_, u)| u.nodes.iter().any(is)).map(|(id, _)| (id.clone(), self.user_name(id))).collect();
        let groups = self.groups.iter().filter(|(_, nodes)| nodes.iter().any(is)).map(|(g, _)| g.clone()).collect();
        (users, groups)
    }

    /// Rewrite or drop every `group.<old>` node and track entry. `new` = None deletes.
    fn repoint(&mut self, old: &str, new: Option<&str>) {
        let old_key = format!("group.{old}");
        let fix = |nodes: &mut Vec<Node>| -> bool {
            let before = nodes.clone();
            match new {
                Some(n) => {
                    for node in nodes.iter_mut().filter(|x| x.key == old_key) {
                        node.key = format!("group.{n}");
                    }
                }
                None => nodes.retain(|x| x.key != old_key),
            }
            *nodes != before
        };
        for (name, nodes) in &mut self.groups {
            if fix(nodes) {
                self.dirty_groups.insert(name.clone());
            }
        }
        for (id, user) in &mut self.users {
            if fix(&mut user.nodes) {
                self.dirty_users.insert(id.clone());
            }
        }
        self.users.retain(|_, u| !u.nodes.is_empty());
        for groups in self.tracks.values_mut() {
            let before = groups.clone();
            match new {
                Some(n) => {
                    for g in groups.iter_mut().filter(|g| *g == old) {
                        *g = n.to_owned();
                    }
                }
                None => groups.retain(|g| g != old),
            }
            if *groups != before {
                self.dirty_tracks = true;
            }
        }
    }

    pub fn delete_group(&mut self, name: &str) -> bool {
        if name == self.default_group || self.groups.remove(name).is_none() {
            return false;
        }
        self.dirty_groups.insert(name.to_owned());
        self.repoint(name, None);
        self.refresh_expiry();
        true
    }

    pub fn rename_group(&mut self, old: &str, new: &str) -> bool {
        if old == self.default_group || self.groups.contains_key(new) {
            return false;
        }
        let Some(nodes) = self.groups.remove(old) else { return false };
        self.groups.insert(new.to_owned(), nodes);
        self.dirty_groups.insert(old.to_owned());
        self.dirty_groups.insert(new.to_owned());
        self.repoint(old, Some(new));
        true
    }

    pub fn clone_group(&mut self, from: &str, to: &str) -> bool {
        if self.groups.contains_key(to) {
            return false;
        }
        let Some(nodes) = self.groups.get(from).cloned() else { return false };
        self.groups.insert(to.to_owned(), nodes);
        self.dirty_groups.insert(to.to_owned());
        true
    }

    /// Replace everything `to` holds with a copy of what `from` holds.
    pub fn copy_nodes(&mut self, from: &Target, to: &Target) {
        let nodes = self.nodes(from).to_vec();
        *self.nodes_mut(to) = nodes;
        self.tidy(to);
        self.refresh_expiry();
    }

    // ---- tracks ----

    /// Groups of the user that sit on the track (any context, still valid).
    fn on_track(&self, uuid: &str, track: &[String], now: u64) -> Vec<usize> {
        let own = self.users.get(uuid).map_or(&[][..], |u| u.nodes.as_slice());
        let mut at: Vec<usize> = own
            .iter()
            .filter(|n| n.value && n.active(now))
            .filter_map(|n| match n.kind() {
                Kind::Group(g) => track.iter().position(|t| t == g),
                _ => None,
            })
            .collect();
        at.sort_unstable();
        at.dedup();
        at
    }

    /// Where a promotion or demotion would take the user, without changing anything.
    pub fn track_step(&self, uuid: &str, track: &str, up: bool, now: u64) -> Result<TrackMove, TrackError> {
        let groups = self.tracks.get(track).ok_or(TrackError::NoTrack)?;
        if groups.is_empty() {
            return Err(TrackError::Empty);
        }
        if let Some(missing) = groups.iter().find(|g| !self.groups.contains_key(*g)) {
            return Err(TrackError::MissingGroup(missing.clone()));
        }
        let at = self.on_track(uuid, groups, now);
        match (at.as_slice(), up) {
            ([], true) => Ok(TrackMove::Joined(groups[0].clone())),
            ([], false) => Err(TrackError::NotOnTrack),
            ([i], true) if i + 1 < groups.len() => Ok(TrackMove::Moved(groups[*i].clone(), groups[i + 1].clone())),
            ([_], true) => Err(TrackError::AtEnd),
            ([0], false) => Ok(TrackMove::Left(groups[0].clone())),
            ([i], false) => Ok(TrackMove::Moved(groups[*i].clone(), groups[i - 1].clone())),
            _ => Err(TrackError::Ambiguous),
        }
    }

    pub fn apply_track_move(&mut self, uuid: &str, mv: &TrackMove) {
        let t = Target::User(uuid.to_owned());
        let drop = |d: &mut Data, g: &str| {
            let key = format!("group.{g}");
            d.remove_where(&t, |n| n.key == key);
        };
        match mv {
            TrackMove::Joined(g) => {
                self.set_node(&t, Node::new(&format!("group.{g}"), true));
            }
            TrackMove::Moved(from, to) => {
                drop(self, from);
                self.set_node(&t, Node::new(&format!("group.{to}"), true));
            }
            TrackMove::Left(g) => drop(self, g),
        }
    }

    /// Count (and with `apply` change) the nodes a bulk update touches: (nodes, holders).
    pub fn bulk(&mut self, scope: Scope, constraint: &Constraint, action: &BulkAction, apply: bool) -> (usize, usize) {
        let mut nodes = 0;
        let mut holders = 0;
        let mut touch = |list: &mut Vec<Node>| -> bool {
            let hits = list.iter().filter(|n| constraint.matches(&n.key)).count();
            if hits == 0 {
                return false;
            }
            nodes += hits;
            holders += 1;
            if apply {
                match action {
                    BulkAction::Delete => list.retain(|n| !constraint.matches(&n.key)),
                    BulkAction::Rename(new) => {
                        let mut out: Vec<Node> = Vec::with_capacity(list.len());
                        for mut n in list.drain(..) {
                            if constraint.matches(&n.key) {
                                n.key = new.clone();
                            }
                            // two nodes that became the same node are one node
                            if !out.iter().any(|o| o.key == n.key && o.context == n.context && o.is_temp() == n.is_temp()) {
                                out.push(n);
                            }
                        }
                        *list = out;
                    }
                }
            }
            apply
        };
        if scope != Scope::Users {
            for (name, list) in &mut self.groups {
                if touch(list) {
                    self.dirty_groups.insert(name.clone());
                }
            }
        }
        if scope != Scope::Groups {
            for (id, user) in &mut self.users {
                if touch(&mut user.nodes) {
                    self.dirty_users.insert(id.clone());
                }
            }
        }
        if apply {
            self.users.retain(|_, u| !u.nodes.is_empty());
            self.refresh_expiry();
        }
        (nodes, holders)
    }

    /// Every node key in the data that is a permission (for `/lp tree`).
    pub fn permission_keys(&self) -> BTreeSet<String> {
        let all = self.groups.values().flatten().chain(self.users.values().flat_map(|u| u.nodes.iter()));
        all.filter(|n| kind(&n.key) == Kind::Perm).map(|n| n.key.clone()).collect()
    }
}

/// `basics:command.*` → `basics:command.`; `*` → ``; anything else is not a wildcard.
pub fn wild_stem(key: &str) -> Option<&str> {
    let stem = key.strip_suffix('*')?;
    (stem.is_empty() || stem.ends_with('.') || stem.ends_with(':')).then_some(stem)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::parse_context;

    const U: &str = "00000000-0000-0000-0000-000000000001";

    fn g(d: &mut Data, name: &str, nodes: &[&str]) {
        d.create_group(name);
        for n in nodes {
            let node = match n.strip_prefix('-') {
                Some(k) => Node::new(k, false),
                None => Node::new(n, true),
            };
            d.set_node(&Target::Group(name.to_owned()), node);
        }
    }

    fn u(d: &mut Data, nodes: &[&str]) {
        for n in nodes {
            let node = match n.strip_prefix('-') {
                Some(k) => Node::new(k, false),
                None => Node::new(n, true),
            };
            d.set_node(&Target::User(U.to_owned()), node);
        }
    }

    fn ctx() -> Ctx {
        Ctx::offline()
    }

    fn check(d: &Data, node: &str) -> Option<bool> {
        d.resolve(U, &ctx(), 1000).check(node, &[])
    }

    #[test]
    fn default_group_always_applies() {
        let mut d = Data::new("default");
        g(&mut d, "default", &["a.b"]);
        assert_eq!(check(&d, "a.b"), Some(true));
        assert_eq!(check(&d, "a.c"), None);
        assert!(d.users.is_empty(), "nobody needs a file for the default group");
        // also for a user who was put in another group that does not name default
        g(&mut d, "vip", &["v.x"]);
        u(&mut d, &["group.vip"]);
        assert_eq!(check(&d, "a.b"), Some(true));
        assert_eq!(check(&d, "v.x"), Some(true));
        assert_eq!(d.resolve(U, &ctx(), 0).holders, vec!["user", "vip", "default"]);
    }

    #[test]
    fn user_beats_group_and_deny_beats_grant_at_one_level() {
        let mut d = Data::new("default");
        g(&mut d, "default", &["a.b", "c.d"]);
        u(&mut d, &["-a.b"]);
        assert_eq!(check(&d, "a.b"), Some(false));
        assert_eq!(check(&d, "c.d"), Some(true));
        // same holder, same key, different contexts that both apply: the specific one wins;
        // equally specific: the temporary one, then the denied one
        let t = Target::Group("default".into());
        d.set_node(&t, Node::with("c.d", false, 0, parse_context(&["world=overworld".to_owned()]).unwrap()));
        assert_eq!(check(&d, "c.d"), Some(false));
        d.set_node(&t, Node::with("e.f", true, 0, parse_context(&["world=overworld".to_owned()]).unwrap()));
        d.set_node(&t, Node::with("e.f", false, 0, parse_context(&["gamemode=survival".to_owned()]).unwrap()));
        assert_eq!(check(&d, "e.f"), Some(false));
        d.set_node(&t, Node::with("g.h", false, 0, Default::default()));
        d.set_node(&t, Node::with("g.h", true, 5000, Default::default()));
        assert_eq!(check(&d, "g.h"), Some(true), "temporary beats permanent");
        assert_eq!(d.resolve(U, &ctx(), 6000).check("g.h", &[]), Some(false), "and stops applying when its time is up");
    }

    #[test]
    fn heavier_parent_wins_and_inheritance_is_transitive() {
        let mut d = Data::new("default");
        g(&mut d, "light", &["weight.10", "x.y", "only.light"]);
        g(&mut d, "heavy", &["weight.50", "-x.y", "group.base"]);
        g(&mut d, "base", &["from.base", "x.y"]);
        u(&mut d, &["group.light", "group.heavy"]);
        assert_eq!(check(&d, "x.y"), Some(false));
        assert_eq!(check(&d, "only.light"), Some(true));
        assert_eq!(check(&d, "from.base"), Some(true));
        let e = d.resolve(U, &ctx(), 0);
        // depth first: heavy, its parent base, then light, then default
        assert_eq!(e.holders, vec!["user", "heavy", "base", "light", "default"]);
        assert_eq!(e.primary, "heavy");
        assert_eq!(e.weight, 50);
        assert_eq!(e.check("group.heavy", &[]), Some(true));
        assert_eq!(e.check("group.base", &[]), Some(true), "inherited membership shows as a node");
    }

    #[test]
    fn cycles_end() {
        let mut d = Data::new("default");
        g(&mut d, "a", &["group.b", "in.a"]);
        g(&mut d, "b", &["group.c", "in.b"]);
        g(&mut d, "c", &["group.a", "in.c"]);
        g(&mut d, "default", &["group.default", "group.a"]);
        for n in ["in.a", "in.b", "in.c"] {
            assert_eq!(check(&d, n), Some(true));
        }
        assert_eq!(d.resolve_group("a", &ctx(), 0).check("in.c", &[]), Some(true));
        // a long chain does not blow the stack
        for i in 0..300 {
            let next = format!("group.n{}", i + 1);
            g(&mut d, &format!("n{i}"), &[next.as_str()]);
        }
        u(&mut d, &["group.n0"]);
        let _ = d.resolve(U, &ctx(), 0);
    }

    #[test]
    fn wildcards() {
        let mut d = Data::new("default");
        g(&mut d, "default", &["basics:command.*", "-basics:command.god", "other.thing.*"]);
        assert_eq!(check(&d, "basics:command.fly"), Some(true));
        assert_eq!(check(&d, "basics:command.fly.sub"), Some(true));
        assert_eq!(check(&d, "basics:command.god"), Some(false), "an exact node beats the wildcard");
        assert_eq!(check(&d, "basics:command"), None, "the wildcard does not cover its own stem");
        assert_eq!(check(&d, "basics:others.fly"), None);
        assert_eq!(check(&d, "other.thing.x"), Some(true));
        assert_eq!(check(&d, "basics:command.*"), Some(true), "holding the wildcard itself can be asked");
        // longest wildcard wins, whatever holder it came from
        u(&mut d, &["-basics:*"]);
        assert_eq!(check(&d, "basics:command.fly"), Some(true));
        assert_eq!(check(&d, "basics:others.fly"), Some(false));
        u(&mut d, &["*"]);
        assert_eq!(check(&d, "minecraft:command.gamemode"), Some(true));
        assert_eq!(check(&d, "basics:others.fly"), Some(false));
        let (v, holder, key) = d.resolve(U, &ctx(), 0).explain("basics:command.heal", &[]).unwrap();
        assert_eq!((v, holder.as_str(), key.as_str()), (true, "default", "basics:command.*"));
        assert_eq!(d.resolve(U, &ctx(), 0).explain("x", &[]).unwrap().2, "*");
    }

    #[test]
    fn granting_wildcards_stop_at_exempt_prefixes() {
        let exempt = vec!["tablist:group.".to_owned()];
        let mut d = Data::new("default");
        u(&mut d, &["*"]);
        let e = d.resolve(U, &ctx(), 0);
        assert_eq!(e.check("tablist:command", &exempt), Some(true));
        assert_eq!(e.check("tablist:group.admin", &exempt), None, "`*` must not put everyone in every TabList group");
        assert_eq!(e.check("tablist:group.admin", &[]), Some(true), "without the rule it would");
        u(&mut d, &["tablist:*"]);
        assert_eq!(d.resolve(U, &ctx(), 0).check("tablist:group.admin", &exempt), None);
        u(&mut d, &["tablist:group.*"]);
        assert_eq!(d.resolve(U, &ctx(), 0).check("tablist:group.admin", &exempt), Some(true), "a wildcard that names the prefix means it");
        // an exact grant and a denying wildcard are never held back
        let mut d = Data::new("default");
        u(&mut d, &["tablist:group.vip", "-*"]);
        let e = d.resolve(U, &ctx(), 0);
        assert_eq!(e.check("tablist:group.vip", &exempt), Some(true));
        assert_eq!(e.check("tablist:group.admin", &exempt), Some(false));
    }

    #[test]
    fn regex_nodes() {
        let exempt = vec!["tablist:group.".to_owned()];
        let mut d = Data::new("default");
        g(&mut d, "default", &["r=basics:command\\.(fly|heal)", "-r=basics:command\\.g.*", "basics:command.*", "-basics:command.heal"]);
        let e = d.resolve(U, &ctx(), 0);
        assert_eq!(e.regex.len(), 2);
        assert_eq!(e.check("basics:command.fly", &[]), Some(true));
        assert_eq!(e.check("basics:command.heal", &[]), Some(false), "an exact key beats a regex");
        assert_eq!(e.check("basics:command.god", &[]), Some(false), "a regex beats a wildcard");
        assert_eq!(e.check("basics:command.feed", &[]), Some(true), "no regex matches: the wildcard answers");
        assert_eq!(e.check("other:node", &[]), None);
        let (v, holder, key) = e.explain("basics:command.gm", &[]).unwrap();
        assert_eq!((v, holder.as_str(), key.as_str()), (false, "default", "r=basics:command\\.g.*"));
        // the first holder's regex wins, like every other key
        u(&mut d, &["r=basics:command\\.g.d"]);
        assert_eq!(d.resolve(U, &ctx(), 0).check("basics:command.god", &[]), Some(true));
        assert!(!d.resolve(U, &ctx(), 0).same_perms(&e));
        // a granting regex stays away from exempt prefixes, a denying one does not
        let mut d = Data::new("default");
        u(&mut d, &["r=.*"]);
        let e = d.resolve(U, &ctx(), 0);
        assert_eq!(e.check("anything:at.all", &exempt), Some(true));
        assert_eq!(e.check("tablist:group.admin", &exempt), None);
        let mut d = Data::new("default");
        u(&mut d, &["-r=tablist:.*"]);
        assert_eq!(d.resolve(U, &ctx(), 0).check("tablist:group.admin", &exempt), Some(false));
        // only so many regex nodes are kept
        let mut d = Data::new("default");
        for i in 0..MAX_REGEX_NODES + 5 {
            d.set_node(&Target::User(U.into()), Node::new(&format!("r=x{i}"), true));
        }
        assert_eq!(d.resolve(U, &ctx(), 0).regex.len(), MAX_REGEX_NODES);
    }

    #[test]
    fn shorthand_nodes() {
        let mut d = Data::new("default");
        g(&mut d, "default", &["basics:command.(fly|heal)", "-kit.(1-3)", "area.(a|b).*"]);
        let e = d.resolve(U, &ctx(), 0);
        assert_eq!(e.check("basics:command.fly", &[]), Some(true));
        assert_eq!(e.check("basics:command.heal", &[]), Some(true));
        assert_eq!(e.check("basics:command.feed", &[]), None);
        assert_eq!(e.check("kit.2", &[]), Some(false));
        assert_eq!(e.check("kit.4", &[]), None);
        assert_eq!(e.check("area.b.build", &[]), Some(true), "a wildcard inside shorthand works");
        assert_eq!(e.check("area.c.build", &[]), None);
        assert!(e.regex.is_empty(), "shorthand costs nothing per check");
        // an exact node of a nearer holder still wins over an expanded one
        u(&mut d, &["-basics:command.fly"]);
        assert_eq!(check(&d, "basics:command.fly"), Some(false));
    }

    #[test]
    fn like_and_bulk() {
        for (p, t, want) in [
            ("basics:%", "basics:command.fly", true),
            ("%.fly", "basics:command.fly", true),
            ("%command%", "basics:command.fly", true),
            ("basics:command.f_y", "basics:command.fly", true),
            ("basics:command.f_", "basics:command.fly", false),
            ("", "", true),
            ("%", "", true),
            ("a%b%c", "a-b-b-c", true),
            ("a%b%c", "a-b-b", false),
            ("abc", "abcd", false),
        ] {
            assert_eq!(like(p, t), want, "{p} ~ {t}");
        }
        let mut d = Data::new("default");
        g(&mut d, "default", &["old.node", "keep.me"]);
        g(&mut d, "vip", &["old.node", "new.node", "-old.other"]);
        u(&mut d, &["old.node"]);
        let eq = Constraint { cmp: Cmp::Eq, value: "old.node".into() };
        assert_eq!(d.bulk(Scope::All, &eq, &BulkAction::Delete, false), (3, 3));
        assert_eq!(d.groups["default"].len(), 2, "a count changes nothing");
        assert_eq!(d.bulk(Scope::Users, &eq, &BulkAction::Delete, false), (1, 1));
        assert_eq!(d.bulk(Scope::Groups, &eq, &BulkAction::Rename("new.node".into()), true), (2, 2));
        assert_eq!(d.groups["vip"].iter().filter(|n| n.key == "new.node").count(), 1, "no duplicate after a rename");
        assert_eq!(check(&d, "new.node"), Some(true));
        assert!(d.dirty_groups.contains("vip") && d.dirty_groups.contains("default"));
        let like_old = Constraint { cmp: Cmp::Like, value: "old.%".into() };
        assert_eq!(d.bulk(Scope::All, &like_old, &BulkAction::Delete, true), (2, 2));
        assert!(d.users.is_empty(), "a user left with nothing is forgotten");
        assert_eq!(d.bulk(Scope::All, &Constraint { cmp: Cmp::NotLike, value: "%.node".into() }, &BulkAction::Delete, false), (1, 1));
        assert_eq!(d.bulk(Scope::All, &Constraint { cmp: Cmp::Ne, value: "keep.me".into() }, &BulkAction::Delete, true), (2, 2));
        assert_eq!(d.groups["default"], vec![Node::new("keep.me", true)]);
    }

    #[test]
    fn context_nodes() {
        let mut d = Data::new("default");
        let t = Target::Group("default".into());
        d.set_node(&t, Node::with("fly", true, 0, parse_context(&["world=nether".to_owned()]).unwrap()));
        d.set_node(&t, Node::with("group.netherfolk", true, 0, parse_context(&["world=nether".to_owned()]).unwrap()));
        g(&mut d, "netherfolk", &["lava.swim"]);
        let over = Ctx::new("minecraft:overworld", "survival");
        let nether = Ctx::new("minecraft:the_nether", "survival");
        assert_eq!(d.resolve(U, &over, 0).check("fly", &[]), None);
        assert_eq!(d.resolve(U, &nether, 0).check("fly", &[]), Some(true));
        assert_eq!(d.resolve(U, &over, 0).check("lava.swim", &[]), None);
        assert_eq!(d.resolve(U, &nether, 0).check("lava.swim", &[]), Some(true), "a parent can depend on the world");
        assert!(!d.resolve(U, &over, 0).same_perms(&d.resolve(U, &nether, 0)));
    }

    #[test]
    fn prefix_suffix_meta() {
        let mut d = Data::new("default");
        g(&mut d, "default", &["prefix.1.&7", "suffix.1. &7*", "meta.color.gray", "meta.tier.0"]);
        g(&mut d, "vip", &["weight.10", "prefix.10.&6[VIP] ", "meta.color.gold"]);
        let e = d.resolve(U, &ctx(), 0);
        assert_eq!((e.prefix.as_str(), e.suffix.as_str(), e.primary.as_str(), e.weight), ("&7", " &7*", "default", 0));
        u(&mut d, &["group.vip"]);
        let e = d.resolve(U, &ctx(), 0);
        assert_eq!(e.prefix, "&6[VIP] ");
        assert_eq!(e.suffix, " &7*");
        assert_eq!(e.meta.get("color").unwrap(), "gold", "the nearer holder's meta wins");
        assert_eq!(e.meta.get("tier").unwrap(), "0");
        assert_eq!(e.primary, "vip");
        // a lower priority on the user does not beat a higher one from a group
        u(&mut d, &["prefix.5.&bMe "]);
        assert_eq!(d.resolve(U, &ctx(), 0).prefix, "&6[VIP] ");
        u(&mut d, &["prefix.99.&bMe "]);
        assert_eq!(d.resolve(U, &ctx(), 0).prefix, "&bMe ");
        assert!(d.resolve(U, &ctx(), 0).check("prefix.99.&bMe ", &[]).is_none(), "meta nodes are not permissions");
        // on one holder: a temporary prefix of the same priority, or a temporary meta value, is the one shown
        let t = Target::User(U.to_owned());
        d.set_node(&t, Node::with("prefix.99.&dEvent ", true, 500, Default::default()));
        d.set_node(&t, Node::new("meta.color.red", true));
        d.set_node(&t, Node::with("meta.color.pink", true, 500, Default::default()));
        let e = d.resolve(U, &ctx(), 100);
        assert_eq!((e.prefix.as_str(), e.meta["color"].as_str()), ("&dEvent ", "pink"));
        let e = d.resolve(U, &ctx(), 500);
        assert_eq!((e.prefix.as_str(), e.meta["color"].as_str()), ("&bMe ", "red"), "and the permanent ones return when it ends");
    }

    #[test]
    fn set_unset_and_user_files() {
        let mut d = Data::new("default");
        let t = Target::User(U.to_owned());
        assert_eq!(d.set_node(&t, Node::new("a", true)), SetResult::Added);
        assert_eq!(d.set_node(&t, Node::new("a", true)), SetResult::Unchanged);
        assert_eq!(d.set_node(&t, Node::new("a", false)), SetResult::Replaced);
        // a temporary node lives beside the permanent one
        assert_eq!(d.set_node(&t, Node::with("a", true, 500, Default::default())), SetResult::Added);
        assert_eq!(d.next_expiry, 500);
        assert_eq!(d.nodes(&t).len(), 2);
        assert_eq!(d.remove_where(&t, |n| n.key == "a" && !n.is_temp()).len(), 1);
        assert!(d.purge(499) == false && d.nodes(&t).len() == 1);
        assert!(d.purge(500));
        assert!(d.users.is_empty(), "a user with nothing left is forgotten");
        assert!(d.dirty_users.contains(U));
        assert_eq!(d.next_expiry, u64::MAX);
        for i in 0..MAX_NODES {
            d.set_node(&t, Node::new(&format!("n.{i}"), true));
        }
        assert_eq!(d.set_node(&t, Node::new("one.more", true)), SetResult::Full);
    }

    #[test]
    fn tracks() {
        let mut d = Data::new("default");
        for n in ["helper", "mod", "admin"] {
            g(&mut d, n, &[]);
        }
        d.tracks.insert("staff".into(), vec!["helper".into(), "mod".into(), "admin".into()]);
        assert_eq!(d.track_step(U, "none", true, 0), Err(TrackError::NoTrack));
        assert_eq!(d.track_step(U, "staff", false, 0), Err(TrackError::NotOnTrack));
        let mv = d.track_step(U, "staff", true, 0).unwrap();
        assert_eq!(mv, TrackMove::Joined("helper".into()));
        d.apply_track_move(U, &mv);
        let mv = d.track_step(U, "staff", true, 0).unwrap();
        assert_eq!(mv, TrackMove::Moved("helper".into(), "mod".into()));
        d.apply_track_move(U, &mv);
        assert_eq!(d.primary(U, &ctx(), 0), "mod");
        assert!(!d.nodes(&Target::User(U.into())).iter().any(|n| n.key == "group.helper"));
        let mv = d.track_step(U, "staff", true, 0).unwrap();
        d.apply_track_move(U, &mv);
        assert_eq!(d.track_step(U, "staff", true, 0), Err(TrackError::AtEnd));
        let mv = d.track_step(U, "staff", false, 0).unwrap();
        assert_eq!(mv, TrackMove::Moved("admin".into(), "mod".into()));
        d.apply_track_move(U, &mv);
        u(&mut d, &["group.helper"]);
        assert_eq!(d.track_step(U, "staff", true, 0), Err(TrackError::Ambiguous));
        d.remove_where(&Target::User(U.into()), |n| n.key == "group.mod");
        let mv = d.track_step(U, "staff", false, 0).unwrap();
        assert_eq!(mv, TrackMove::Left("helper".into()));
        d.apply_track_move(U, &mv);
        assert!(d.users.is_empty());
        d.tracks.insert("broken".into(), vec!["helper".into(), "ghost".into()]);
        assert_eq!(d.track_step(U, "broken", true, 0), Err(TrackError::MissingGroup("ghost".into())));
        d.tracks.insert("empty".into(), vec![]);
        assert_eq!(d.track_step(U, "empty", true, 0), Err(TrackError::Empty));
    }

    #[test]
    fn rename_delete_clone_group() {
        let mut d = Data::new("default");
        g(&mut d, "vip", &["v.x", "weight.5"]);
        g(&mut d, "mvp", &["group.vip"]);
        u(&mut d, &["group.vip"]);
        d.tracks.insert("donor".into(), vec!["vip".into(), "mvp".into()]);
        assert!(!d.rename_group("default", "x"));
        assert!(!d.rename_group("vip", "mvp"));
        assert!(d.rename_group("vip", "gold"));
        assert_eq!(check(&d, "v.x"), Some(true));
        assert_eq!(d.resolve_group("mvp", &ctx(), 0).check("v.x", &[]), Some(true));
        assert_eq!(d.tracks["donor"], vec!["gold", "mvp"]);
        let (users, groups) = d.members("gold");
        assert_eq!((users.len(), groups), (1, vec!["mvp".to_owned()]));
        assert!(d.clone_group("gold", "gold2") && !d.clone_group("gold", "gold2"));
        assert_eq!(d.weight("gold2"), 5);
        assert!(!d.delete_group("default"));
        assert!(d.delete_group("gold"));
        assert_eq!(check(&d, "v.x"), None);
        assert!(d.users.is_empty(), "the user only had that group");
        assert_eq!(d.tracks["donor"], vec!["mvp"]);
        assert!(d.groups["mvp"].is_empty());
        assert!(d.dirty_groups.contains("gold") && d.dirty_tracks);
    }

    #[test]
    fn names_index() {
        let mut d = Data::new("default");
        assert!(d.note_name(U, "Alice"));
        assert!(!d.note_name(U, "Alice"));
        assert_eq!(d.lookup_user("alice").as_deref(), Some(U));
        assert_eq!(d.lookup_user(U).as_deref(), Some(U));
        assert_eq!(d.lookup_user(&U.replace('-', "")).as_deref(), Some(U));
        assert_eq!(d.lookup_user("bob"), None);
        assert_eq!(d.lookup_user("00000000-0000-0000-0000-00000000ffff"), None, "an unknown uuid is not a user");
        u(&mut d, &["a"]);
        assert_eq!(d.users[U].name, "Alice");
        assert!(d.note_name(U, "Alicia"));
        assert_eq!(d.lookup_user("alice"), None);
        assert_eq!(d.user_name(U), "Alicia");
        assert_eq!(d.users[U].name, "Alicia");
    }

    #[test]
    fn copy_between_holders() {
        let mut d = Data::new("default");
        g(&mut d, "vip", &["v.x"]);
        u(&mut d, &["group.vip", "own.node"]);
        let other = Target::User("00000000-0000-0000-0000-000000000002".into());
        d.copy_nodes(&Target::User(U.into()), &other);
        assert_eq!(d.nodes(&other).len(), 2);
        d.copy_nodes(&Target::Group("default".into()), &other);
        assert!(d.nodes(&other).is_empty() && d.users.len() == 1);
    }
}

//! The `/lp` command tree (plain Rust: it works on `Data` and on callbacks the glue provides).
//!
//! Syntax follows LuckPerms. Every area has its own node `perms:command.<area>`; a sender who
//! is neither the console nor a level 4 operator is also held to the delegation rules in
//! `guard_*`: no granting of what they do not hold, no touching of heavier groups or players.
use std::collections::BTreeMap;
use std::path::Path;

use crate::lang::{HELP, P, msg};
use crate::model::{BulkAction, Cmp, Constraint, Data, PendingBulk, Scope, SetResult, Target, TrackError, TrackMove};
use crate::node::{Ctx, Kind, MAX_DURATION, MAX_EXPORTS, MAX_GROUPS, MAX_NODES, MAX_TRACKS, Node, TempMode, expand_shorthand, fmt_duration, kind, norm_key, parse_context, parse_duration, parse_temp_mode, regex_pattern, valid_name};
use crate::store::{self, LogEntry};

/// Who runs the command.
pub struct Actor<'a> {
    pub console: bool,
    /// Level 4 operator: no delegation limits.
    pub bypass: bool,
    pub uuid: Option<String>,
    pub name: String,
    /// The sender's real permission check (through the server).
    pub has: &'a dyn Fn(&str) -> bool,
}

/// What the command may ask about the running server.
pub struct Env<'a> {
    pub now: u64,
    pub th: bool,
    pub dir: &'a Path,
    pub version: &'a str,
    pub exempt: &'a [String],
    /// Context of an online player by UUID.
    pub online: &'a dyn Fn(&str) -> Option<Ctx>,
    /// The server's final answer for an online player.
    pub final_check: &'a dyn Fn(&str, &str) -> Option<bool>,
    /// Nodes seen in checks since the server started.
    pub seen: &'a dyn Fn() -> Vec<String>,
    /// Players with a computed answer set, checks seen, checks Perms answered, verbose
    /// listeners, command lists sent again, microseconds that took.
    pub stats: (usize, u64, u64, usize, u64, u64),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verbose {
    On(String),
    Off,
}

/// `/lp editor` and `/lp applyedits`: the glue talks to the web editor (the network is not this
/// module's business) and tells the sender how it went.
#[derive(Debug, PartialEq, Eq)]
pub enum WebAsk {
    /// Send the data and answer with a link.
    Open,
    /// Fetch the changes saved under this code and apply them.
    Apply(String),
}

#[derive(Default)]
pub struct Outcome {
    pub web: Option<WebAsk>,
    pub lines: Vec<String>,
    /// Data changed: the glue applies it to everyone online.
    pub changed: bool,
    /// `/lp sync`: the glue reloads config and data.
    pub sync: bool,
    pub verbose: Option<Verbose>,
    /// The change as it went into the action log: the glue tells the people who watch it.
    pub logged: Option<LogEntry>,
}

/// A player who has this node on themselves is not told about other people's edits.
pub const NOTIFY_IGNORE: &str = "perms:log.notify.ignoring";
/// Holders of this node are told when someone edits (see `/lp log notify`).
pub const NOTIFY_NODE: &str = "perms:command.log.notify";
const LOG_PAGE: usize = 10;

/// Areas and whether an operator needs level 4 (else level 3) by default.
pub const AREAS: &[(&str, bool)] = &[
    ("base", false),
    ("info", false),
    ("check", false),
    ("tree", false),
    ("search", false),
    ("listgroups", false),
    ("listtracks", false),
    ("verbose", false),
    ("editor", false),
    ("user.info", false),
    ("group.info", false),
    ("track.info", false),
    ("log", false),
    ("log.notify", false),
    ("user.permission", true),
    ("user.parent", true),
    ("user.meta", true),
    ("user.track", true),
    ("user.clear", true),
    ("user.clone", true),
    ("group.permission", true),
    ("group.parent", true),
    ("group.meta", true),
    ("group.clear", true),
    ("group.edit", true),
    ("group.clone", true),
    ("creategroup", true),
    ("deletegroup", true),
    ("createtrack", true),
    ("deletetrack", true),
    ("track.edit", true),
    ("sync", true),
    ("export", true),
    ("import", true),
    ("bulkupdate", true),
    ("applyedits", true),
];

const LIST_CAP: usize = 60;
const MAX_TEXT: usize = 64;

type R = Result<(), String>;

struct Run<'a> {
    d: &'a mut Data,
    a: &'a Actor<'a>,
    e: &'a Env<'a>,
    out: Outcome,
    /// What the action log should say when the typed words do not (a confirmed bulk update).
    note: Option<String>,
}

fn yes_no(v: bool) -> &'static str {
    if v { "&atrue" } else { "&cfalse" }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "true" | "t" | "yes" | "allow" => Some(true),
        "false" | "f" | "no" | "deny" => Some(false),
        _ => None,
    }
}

impl Run<'_> {
    fn m(&self, key: &str, args: &[&str]) -> String {
        format!("{P}{}", msg(self.e.th, key, args))
    }

    fn say(&mut self, key: &str, args: &[&str]) {
        let line = self.m(key, args);
        self.out.lines.push(line);
    }

    /// A list line: no plugin prefix.
    fn row(&mut self, key: &str, args: &[&str]) {
        self.out.lines.push(msg(self.e.th, key, args));
    }

    fn raw(&mut self, line: String) {
        self.out.lines.push(line);
    }

    fn fail<T>(&self, key: &str, args: &[&str]) -> Result<T, String> {
        Err(self.m(key, args))
    }

    fn usage<T>(&self, text: &str) -> Result<T, String> {
        self.fail("usage", &[text])
    }

    /// Rows of a list, cut at `LIST_CAP` with a count of the rest.
    fn rows(&mut self, rows: Vec<String>) {
        let extra = rows.len().saturating_sub(LIST_CAP);
        self.out.lines.extend(rows.into_iter().take(LIST_CAP));
        if extra > 0 {
            self.row("more", &[&extra.to_string()]);
        }
    }

    fn need(&self, area: &str) -> R {
        let node = format!("perms:command.{area}");
        if self.a.console || (self.a.has)(&node) { Ok(()) } else { self.fail("no_perm", &[&node]) }
    }

    fn free(&self) -> bool {
        self.a.console || self.a.bypass
    }

    fn ctx_of(&self, uuid: &str) -> Ctx {
        (self.e.online)(uuid).unwrap_or_else(Ctx::offline)
    }

    fn own_weight(&self) -> i32 {
        match (&self.a.uuid, self.free()) {
            (_, true) => i32::MAX,
            (Some(u), _) => self.d.weight(self.d.primary(u, &self.ctx_of(u), self.e.now)),
            (None, _) => i32::MIN,
        }
    }

    fn label(&self, t: &Target) -> String {
        match t {
            Target::User(u) => self.d.user_name(u),
            Target::Group(g) => g.clone(),
        }
    }

    // ---- delegation rules ----

    fn guard_target(&self, t: &Target) -> R {
        if self.free() {
            return Ok(());
        }
        match t {
            Target::Group(g) if self.d.weight(g) > self.own_weight() => self.fail("guard_group", &[g]),
            Target::User(u) if self.a.uuid.as_deref() != Some(u.as_str()) && self.d.weight(self.d.primary(u, &self.ctx_of(u), self.e.now)) > self.own_weight() => {
                self.fail("guard_user", &[&self.d.user_name(u)])
            }
            _ => Ok(()),
        }
    }

    /// Putting someone in a group gives them all it gives: the sender must hold all of that.
    fn guard_group_grant(&self, g: &str) -> R {
        if self.free() {
            return Ok(());
        }
        if self.d.weight(g) > self.own_weight() {
            return self.fail("guard_group", &[g]);
        }
        let eff = self.d.resolve_group(g, &Ctx::offline(), self.e.now);
        // What a regex gives cannot be listed, so it cannot be shown to be held
        if let Some(r) = eff.regex.iter().find(|r| r.value) {
            return self.fail("guard_group_node", &[g, &r.key]);
        }
        let mut keys: Vec<&String> = eff.exact.iter().filter(|(k, (v, _))| *v && kind(k) == Kind::Perm && expand_shorthand(k).is_none()).map(|(k, _)| k).collect();
        keys.sort();
        match keys.into_iter().find(|k| !(self.a.has)(k)) {
            Some(missing) => self.fail("guard_group_node", &[g, missing]),
            None => Ok(()),
        }
    }

    fn guard_add(&self, n: &Node) -> R {
        if self.free() {
            return Ok(());
        }
        match n.kind() {
            Kind::Perm if regex_pattern(&n.key).is_some() => self.fail("guard_regex", &[]),
            Kind::Perm if n.value => {
                // A shorthand node gives each node it stands for
                let keys = expand_shorthand(&n.key).unwrap_or_else(|| vec![n.key.clone()]);
                match keys.iter().find(|k| !(self.a.has)(k)) {
                    Some(missing) => self.fail("guard_node", &[missing]),
                    None => Ok(()),
                }
            }
            Kind::Group(g) if n.value => self.guard_group_grant(g),
            Kind::Weight(w) if w > self.own_weight() => self.fail("guard_weight", &[&self.own_weight().to_string()]),
            _ => Ok(()),
        }
    }

    /// Taking a denial away is a grant too.
    fn guard_remove(&self, n: &Node) -> R {
        if self.free() || n.value {
            return Ok(());
        }
        match n.kind() {
            Kind::Perm if regex_pattern(&n.key).is_some() => self.fail("guard_regex", &[]),
            Kind::Perm => {
                let keys = expand_shorthand(&n.key).unwrap_or_else(|| vec![n.key.clone()]);
                match keys.iter().find(|k| !(self.a.has)(k)) {
                    Some(missing) => self.fail("guard_undeny", &[missing]),
                    None => Ok(()),
                }
            }
            Kind::Group(g) => self.guard_group_grant(g),
            _ => Ok(()),
        }
    }

    // ---- parsing ----

    fn key(&self, raw: &str) -> Result<String, String> {
        norm_key(raw).or_else(|_| self.fail("bad_node", &[&raw.chars().take(40).collect::<String>()]))
    }

    fn ctx(&mut self, words: &[String]) -> Result<BTreeMap<String, String>, String> {
        let ctx = parse_context(words).or_else(|bad| self.fail("bad_context", &[&bad]))?;
        if let Some(k) = ctx.keys().find(|k| !matches!(k.as_str(), "world" | "gamemode")) {
            let k = k.clone();
            self.say("unknown_ctx", &[&k]);
        }
        Ok(ctx)
    }

    fn group(&self, raw: &str) -> Result<String, String> {
        let g = raw.to_ascii_lowercase();
        if self.d.groups.contains_key(&g) { Ok(g) } else { self.fail("no_group", &[&raw.chars().take(40).collect::<String>()]) }
    }

    fn new_name(&self, raw: &str) -> Result<String, String> {
        let n = raw.to_ascii_lowercase();
        if valid_name(&n) { Ok(n) } else { self.fail("bad_name", &[&raw.chars().take(40).collect::<String>()]) }
    }

    fn duration(&self, raw: &str) -> Result<u64, String> {
        parse_duration(raw).map(|d| self.e.now + d).ok_or(()).or_else(|()| self.fail("bad_duration", &[raw]))
    }

    /// `<duration> [accumulate|replace|deny]` at `rest[at]`: (expiry, modifier, index after them).
    fn temp_tail(&self, rest: &[String], at: usize, usage: &str) -> Result<(u64, TempMode, usize), String> {
        let Some(raw) = rest.get(at) else { return self.usage(usage) };
        let expiry = self.duration(raw)?;
        match rest.get(at + 1).and_then(|m| parse_temp_mode(m)) {
            Some(mode) => Ok((expiry, mode, at + 2)),
            None => Ok((expiry, TempMode::Set, at + 1)),
        }
    }

    /// Add a temporary node, treating one that is already there the way the modifier says.
    fn add_temp(&mut self, t: &Target, mut node: Node, mode: TempMode) -> Result<SetResult, String> {
        let now = self.e.now;
        let old = self.d.nodes(t).iter().find(|n| n.key == node.key && n.context == node.context && n.is_temp() && n.active(now)).map(|n| n.expiry);
        if let Some(old) = old {
            match mode {
                TempMode::Deny => return self.fail("temp_exists", &[&self.label(t), &node.key]),
                TempMode::Accumulate => node.expiry = (old + node.expiry.saturating_sub(now)).min(now + MAX_DURATION),
                TempMode::Replace => node.expiry = node.expiry.max(old),
                TempMode::Set => {}
            }
        }
        self.add(t, node)
    }

    // ---- changes ----

    fn add(&mut self, t: &Target, n: Node) -> Result<SetResult, String> {
        if let (Target::Group(g), Kind::Group(p)) = (t, n.kind()) {
            if g == p {
                return self.fail("self_parent", &[]);
            }
        }
        self.guard_add(&n)?;
        match self.d.set_node(t, n) {
            SetResult::Full => self.fail("full", &[&MAX_NODES.to_string()]),
            other => Ok(other),
        }
    }

    fn remove(&mut self, t: &Target, pick: impl Fn(&Node) -> bool) -> Result<Vec<Node>, String> {
        for n in self.d.nodes(t).iter().filter(|n| pick(n)) {
            self.guard_remove(n)?;
        }
        Ok(self.d.remove_where(t, pick))
    }

    // ---- holder commands ----

    fn holder(&mut self, t: Target, w: &[String]) -> R {
        let user = matches!(t, Target::User(_));
        let who = if user { "user" } else { "group" };
        let sub = w.first().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
        let rest = w.get(1..).unwrap_or_default();
        match sub.as_str() {
            "" | "info" => {
                self.need(&format!("{who}.info"))?;
                self.info(&t)
            }
            "permission" | "perm" | "permissions" => self.permission(&t, who, rest),
            "parent" | "parents" => self.parent(&t, who, rest),
            "meta" => self.meta(&t, who, rest),
            "clear" => {
                self.need(&format!("{who}.clear"))?;
                self.guard_target(&t)?;
                let gone = self.remove(&t, |_| true)?;
                self.say("cleared", &[&self.label(&t), &gone.len().to_string()]);
                Ok(())
            }
            "showtracks" => {
                self.need(&format!("{who}.info"))?;
                let names: Vec<String> = match &t {
                    Target::Group(g) => self.d.tracks.iter().filter(|(_, gs)| gs.contains(g)).map(|(n, _)| n.clone()).collect(),
                    Target::User(u) => {
                        let own: Vec<&str> = self.d.nodes(&Target::User(u.clone())).iter().filter(|n| n.value).filter_map(|n| match n.kind() {
                            Kind::Group(g) => Some(g),
                            _ => None,
                        }).collect();
                        self.d.tracks.iter().filter(|(_, gs)| gs.iter().any(|g| own.contains(&g.as_str()))).map(|(n, _)| n.clone()).collect()
                    }
                };
                let list = if names.is_empty() { msg(self.e.th, "none", &[]) } else { names.join(", ") };
                self.say("showtracks", &[&self.label(&t), &list]);
                Ok(())
            }
            "promote" | "demote" if user => self.step(&t, sub == "promote", rest),
            "clone" if user => {
                self.need("user.clone")?;
                let Some(other) = rest.first() else { return self.usage("user <name> clone <other>") };
                let Some(dest) = self.d.lookup_user(other) else { return self.fail("no_user", &[other]) };
                let dest = Target::User(dest);
                self.guard_target(&t)?;
                self.guard_target(&dest)?;
                for n in self.d.nodes(&t) {
                    self.guard_add(n)?;
                }
                for n in self.d.nodes(&dest) {
                    self.guard_remove(n)?;
                }
                self.d.copy_nodes(&t, &dest);
                self.say("cloned", &[&self.label(&t), &self.label(&dest), &self.d.nodes(&dest).len().to_string()]);
                Ok(())
            }
            "setweight" if !user => {
                self.need("group.edit")?;
                self.guard_target(&t)?;
                let Some(raw) = rest.first() else { return self.usage("group <group> setweight <n>") };
                let weight = self.weight_arg(raw)?;
                self.remove(&t, |n| matches!(n.kind(), Kind::Weight(_)))?;
                self.add(&t, Node::new(&format!("weight.{weight}"), true))?;
                self.say("weight_set", &[&self.label(&t), &weight.to_string()]);
                Ok(())
            }
            "setdisplayname" if !user => {
                self.need("group.edit")?;
                self.guard_target(&t)?;
                let name = rest.join(" ");
                if name.is_empty() {
                    return self.usage("group <group> setdisplayname <name>");
                }
                if name.chars().count() > MAX_TEXT {
                    return self.fail("too_long", &[&MAX_TEXT.to_string()]);
                }
                let key = self.key(&format!("displayname.{name}"))?;
                self.remove(&t, |n| matches!(n.kind(), Kind::DisplayName(_)))?;
                self.add(&t, Node::new(&key, true))?;
                self.say("display_set", &[&self.label(&t), &name]);
                Ok(())
            }
            "rename" if !user => {
                self.need("group.edit")?;
                self.guard_target(&t)?;
                let Some(raw) = rest.first() else { return self.usage("group <group> rename <new>") };
                let new = self.new_name(raw)?;
                let old = self.label(&t);
                if old == self.d.default_group {
                    return self.fail("group_default", &[&old]);
                }
                if !self.d.rename_group(&old, &new) {
                    return self.fail("group_exists", &[&new]);
                }
                self.say("group_renamed", &[&old, &new]);
                Ok(())
            }
            "clone" if !user => {
                self.need("group.clone")?;
                self.guard_target(&t)?;
                let Some(raw) = rest.first() else { return self.usage("group <group> clone <new>") };
                let new = self.new_name(raw)?;
                let old = self.label(&t);
                if self.d.groups.len() >= MAX_GROUPS {
                    return self.fail("too_many", &[&MAX_GROUPS.to_string()]);
                }
                if !self.d.clone_group(&old, &new) {
                    return self.fail("group_exists", &[&new]);
                }
                self.say("group_cloned", &[&old, &new]);
                Ok(())
            }
            "listmembers" if !user => {
                self.need("group.info")?;
                let g = self.label(&t);
                let (users, groups) = self.d.members(&g);
                self.say("members_title", &[&g]);
                let cut = |names: Vec<String>| -> String {
                    let extra = names.len().saturating_sub(LIST_CAP);
                    let mut s = names.into_iter().take(LIST_CAP).collect::<Vec<_>>().join(", ");
                    if extra > 0 {
                        s.push_str(&format!(" (+{extra})"));
                    }
                    if s.is_empty() { "-".into() } else { s }
                };
                let n_users = users.len().to_string();
                let n_groups = groups.len().to_string();
                self.row("members_users", &[&n_users, &cut(users.into_iter().map(|(_, n)| n).collect())]);
                self.row("members_groups", &[&n_groups, &cut(groups)]);
                Ok(())
            }
            other => self.fail("unknown_sub", &[&other.chars().take(40).collect::<String>()]),
        }
    }

    fn weight_arg(&self, raw: &str) -> Result<i32, String> {
        match raw.parse::<i32>() {
            Ok(w) if (-1_000_000..=1_000_000).contains(&w) => {
                if !self.free() && w > self.own_weight() {
                    return self.fail("guard_weight", &[&self.own_weight().to_string()]);
                }
                Ok(w)
            }
            _ => self.fail("bad_number", &[raw]),
        }
    }

    fn info(&mut self, t: &Target) -> R {
        let now = self.e.now;
        let th = self.e.th;
        let nodes = self.d.nodes(t).to_vec();
        let count = |f: fn(&Kind) -> bool| nodes.iter().filter(|n| f(&n.kind())).count().to_string();
        let perms = count(|k| matches!(k, Kind::Perm));
        let parents = count(|k| matches!(k, Kind::Group(_)));
        let metas = count(|k| matches!(k, Kind::Prefix(..) | Kind::Suffix(..) | Kind::Meta(..)));
        let show = |s: &str| if s.is_empty() { msg(th, "none", &[]) } else { format!("\"{s}&r\"") };
        match t {
            Target::User(u) => {
                let online = (self.e.online)(u);
                let ctx = online.clone().unwrap_or_else(Ctx::offline);
                let eff = self.d.resolve(u, &ctx, now);
                let state = msg(th, if online.is_some() { "online" } else { "offline" }, &[]);
                self.say("user_title", &[&self.d.user_name(u), u, &state]);
                self.row("line_primary", &[&eff.primary, &eff.weight.to_string()]);
                self.row("line_inherits", &[&eff.holders[1..].join(", ")]);
                self.row("line_prefix", &[&show(&eff.prefix), &show(&eff.suffix)]);
                if online.is_some() {
                    self.row("line_context", &[&format!("world={} gamemode={}", ctx.world, ctx.gamemode)]);
                }
            }
            Target::Group(g) => {
                let eff = self.d.resolve_group(g, &Ctx::offline(), now);
                let display = self.d.display_name(g).map(|d| format!(" &7({d}&7)")).unwrap_or_default();
                self.say("group_title", &[g, &display, &eff.weight.to_string()]);
                let inherits = eff.holders[1..].iter().filter(|h| *h != g).cloned().collect::<Vec<_>>().join(", ");
                self.row("line_inherits", &[if inherits.is_empty() { "-" } else { &inherits }]);
                self.row("line_prefix", &[&show(&eff.prefix), &show(&eff.suffix)]);
                let (users, groups) = self.d.members(g);
                self.row("line_members", &[&users.len().to_string(), &groups.len().to_string()]);
            }
        }
        self.row("line_counts", &[&nodes.len().to_string(), &perms, &parents, &metas]);
        Ok(())
    }

    fn list(&mut self, t: &Target, title: &str, pick: fn(&Kind) -> bool) {
        let now = self.e.now;
        let th = self.e.th;
        let mut nodes: Vec<&Node> = self.d.nodes(t).iter().filter(|n| pick(&n.kind())).collect();
        nodes.sort_by(|a, b| a.key.cmp(&b.key));
        let rows: Vec<String> = nodes.iter().map(|n| format!(" &7- {}", n.describe(now, th))).collect();
        self.say(title, &[&self.label(t), &rows.len().to_string()]);
        if rows.is_empty() {
            self.raw(format!(" {}", msg(th, "none", &[])));
        }
        self.rows(rows);
    }

    fn check(&mut self, t: &Target, node: &str) -> R {
        let now = self.e.now;
        let th = self.e.th;
        self.say("check_title", &[&self.label(t), node]);
        let own: Vec<String> = self.d.nodes(t).iter().filter(|n| n.key == node).map(|n| n.describe(now, th)).collect();
        if !own.is_empty() {
            self.row("check_own", &[&own.join("&7, ")]);
        }
        let eff = match t {
            Target::User(u) => self.d.resolve(u, &self.ctx_of(u), now),
            Target::Group(g) => self.d.resolve_group(g, &Ctx::offline(), now),
        };
        match eff.explain(node, self.e.exempt) {
            Some((v, holder, key)) => {
                let from = if holder == "user" { self.label(t) } else { holder };
                self.row("check_eff", &[yes_no(v), &from, &key]);
            }
            None => self.row("check_none", &[]),
        }
        if let Target::User(u) = t {
            if let Some(v) = (self.e.final_check)(u, node) {
                self.row("check_final", &[yes_no(v)]);
            }
        }
        Ok(())
    }

    fn permission(&mut self, t: &Target, who: &str, w: &[String]) -> R {
        let sub = w.first().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
        let rest = w.get(1..).unwrap_or_default();
        let edit = format!("{who}.permission");
        match sub.as_str() {
            "" | "info" | "list" => {
                self.need(&format!("{who}.info"))?;
                self.list(t, "perm_title", |k| matches!(k, Kind::Perm));
                Ok(())
            }
            "check" | "checkinherits" => {
                self.need(&format!("{who}.info"))?;
                let Some(raw) = rest.first() else { return self.usage("… permission check|checkinherits <node>") };
                let key = self.key(raw)?;
                self.check(t, &key)
            }
            "set" | "settemp" => {
                self.need(&edit)?;
                self.guard_target(t)?;
                let temp = sub == "settemp";
                let Some(raw) = rest.first() else {
                    return self.usage(if temp { "… permission settemp <node> <true|false> <duration> [accumulate|replace|deny] [context…]" } else { "… permission set <node> [true|false] [context…]" });
                };
                let key = self.key(raw)?;
                let mut at = 1;
                let mut value = true;
                match rest.get(1).map(|s| parse_bool(s)) {
                    Some(Some(v)) => {
                        value = v;
                        at = 2;
                    }
                    Some(None) if temp => return self.fail("bad_bool", &[&rest[1]]),
                    None if temp => return self.usage("… permission settemp <node> <true|false> <duration> [accumulate|replace|deny] [context…]"),
                    _ => {}
                }
                let mut expiry = 0;
                let mut mode = TempMode::Set;
                if temp {
                    (expiry, mode, at) = self.temp_tail(rest, at, "… permission settemp <node> <true|false> <duration> [accumulate|replace|deny] [context…]")?;
                }
                let ctx = self.ctx(rest.get(at..).unwrap_or_default())?;
                let node = Node::with(&key, value, expiry, ctx.clone());
                let label = self.label(t);
                let res = if temp { self.add_temp(t, node, mode)? } else { self.add(t, node)? };
                // Shown as it is stored: a modifier may have moved the end time
                let shown = self.d.nodes(t).iter().find(|n| n.key == key && n.context == ctx && n.is_temp() == temp).map(|n| n.describe(self.e.now, self.e.th)).unwrap_or_default();
                match res {
                    SetResult::Unchanged => self.say("set_same", &[&label, &shown]),
                    _ => self.say("set", &[&label, &shown]),
                }
                Ok(())
            }
            "unset" | "unsettemp" => {
                self.need(&edit)?;
                self.guard_target(t)?;
                let temp = sub == "unsettemp";
                let Some(raw) = rest.first() else { return self.usage("… permission unset <node> [context…]") };
                let key = self.key(raw)?;
                let ctx = self.ctx(&rest[1..])?;
                let gone = self.remove(t, |n| n.key == key && n.context == ctx && n.is_temp() == temp)?;
                let label = self.label(t);
                match gone.first() {
                    Some(n) => self.say("unset", &[&label, &n.describe(self.e.now, self.e.th)]),
                    None => self.say("unset_none", &[&label, &key]),
                }
                Ok(())
            }
            "clear" => {
                self.need(&edit)?;
                self.guard_target(t)?;
                let ctx = self.ctx(rest)?;
                let gone = self.remove(t, |n| n.kind() == Kind::Perm && (ctx.is_empty() || n.context == ctx))?;
                self.say("cleared", &[&self.label(t), &gone.len().to_string()]);
                Ok(())
            }
            other => self.fail("unknown_sub", &[&other.chars().take(40).collect::<String>()]),
        }
    }

    fn parent(&mut self, t: &Target, who: &str, w: &[String]) -> R {
        let sub = w.first().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
        let rest = w.get(1..).unwrap_or_default();
        let edit = format!("{who}.parent");
        let label = self.label(t);
        match sub.as_str() {
            "" | "info" | "list" => {
                self.need(&format!("{who}.info"))?;
                let now = self.e.now;
                let th = self.e.th;
                let mut rows: Vec<String> = self
                    .d
                    .nodes(t)
                    .iter()
                    .filter_map(|n| match n.kind() {
                        Kind::Group(g) => Some(format!(" &7- {} &7(weight {})", n.describe(now, th), self.d.weight(g))),
                        _ => None,
                    })
                    .collect();
                if let Target::User(u) = t {
                    let default = self.d.default_group.clone();
                    if !self.d.nodes(t).iter().any(|n| n.value && n.kind() == Kind::Group(&default)) {
                        rows.push(format!(" &7- &a{default} {}", msg(th, "implicit", &[])));
                    }
                    self.say("parent_title", &[&label]);
                    self.rows(rows);
                    let ctx = self.ctx_of(u);
                    let primary = self.d.primary(u, &ctx, now).to_owned();
                    self.row("line_primary", &[&primary, &self.d.weight(&primary).to_string()]);
                } else {
                    self.say("parent_title", &[&label]);
                    if rows.is_empty() {
                        self.raw(format!(" {}", msg(th, "none", &[])));
                    }
                    self.rows(rows);
                }
                Ok(())
            }
            "set" | "add" | "addtemp" => {
                self.need(&edit)?;
                self.guard_target(t)?;
                let temp = sub == "addtemp";
                let Some(raw) = rest.first() else {
                    return self.usage(if temp { "… parent addtemp <group> <duration> [context…]" } else { "… parent add|set <group> [context…]" });
                };
                let g = self.group(raw)?;
                let mut at = 1;
                let mut expiry = 0;
                let mut mode = TempMode::Set;
                if temp {
                    (expiry, mode, at) = self.temp_tail(rest, 1, "… parent addtemp <group> <duration> [accumulate|replace|deny] [context…]")?;
                }
                let ctx = self.ctx(rest.get(at..).unwrap_or_default())?;
                let node = Node::with(&format!("group.{g}"), true, expiry, ctx.clone());
                // Check before anything is taken away, so a refused `set` changes nothing
                if let Target::Group(own) = t {
                    if *own == g {
                        return self.fail("self_parent", &[]);
                    }
                }
                self.guard_add(&node)?;
                if sub == "set" {
                    self.remove(t, |n| matches!(n.kind(), Kind::Group(_)) && n.context == ctx)?;
                    self.add(t, node)?;
                    self.say("parent_set", &[&label, &g]);
                } else {
                    let key = node.key.clone();
                    let res = if temp { self.add_temp(t, node, mode)? } else { self.add(t, node)? };
                    let end = self.d.nodes(t).iter().find(|n| n.key == key && n.context == ctx && n.is_temp() == temp).map_or(expiry, |n| n.expiry);
                    let time = if temp { msg(self.e.th, "for_time", &[&fmt_duration(end.saturating_sub(self.e.now), self.e.th)]) } else { String::new() };
                    match res {
                        SetResult::Unchanged => self.say("set_same", &[&label, &format!("&fgroup.{g}")]),
                        _ => self.say("parent_add", &[&label, &g, &time]),
                    }
                }
                Ok(())
            }
            "remove" | "removetemp" => {
                self.need(&edit)?;
                self.guard_target(t)?;
                let temp = sub == "removetemp";
                let Some(raw) = rest.first() else { return self.usage("… parent remove <group> [context…]") };
                let g = raw.to_ascii_lowercase();
                let ctx = self.ctx(&rest[1..])?;
                let key = format!("group.{g}");
                let gone = self.remove(t, |n| n.key == key && n.context == ctx && n.is_temp() == temp)?;
                if gone.is_empty() {
                    self.say("parent_not", &[&label, &g.chars().take(40).collect::<String>()]);
                } else {
                    self.say("parent_remove", &[&label, &g]);
                }
                Ok(())
            }
            "settrack" | "cleartrack" => {
                self.need(&edit)?;
                self.guard_target(t)?;
                let set = sub == "settrack";
                let Some(raw) = rest.first() else { return self.usage(if set { "… parent settrack <track> <group> [context…]" } else { "… parent cleartrack <track> [context…]" }) };
                let track = raw.to_ascii_lowercase();
                let Some(on_track) = self.d.tracks.get(&track).cloned() else { return self.fail("no_track", &[&track.chars().take(40).collect::<String>()]) };
                let mut at = 1;
                let mut new = None;
                if set {
                    let Some(raw) = rest.get(1) else { return self.usage("… parent settrack <track> <group> [context…]") };
                    let g = self.group(raw)?;
                    if !on_track.contains(&g) {
                        return self.fail("track_hasnt", &[&track, &g]);
                    }
                    new = Some(g);
                    at = 2;
                }
                let ctx = self.ctx(rest.get(at..).unwrap_or_default())?;
                if let Some(g) = &new {
                    if let Target::Group(own) = t {
                        if own == g {
                            return self.fail("self_parent", &[]);
                        }
                    }
                    self.guard_add(&Node::with(&format!("group.{g}"), true, 0, ctx.clone()))?;
                }
                let gone = self.remove(t, |n| n.context == ctx && matches!(n.kind(), Kind::Group(g) if on_track.iter().any(|x| x == g)))?;
                match new {
                    Some(g) => {
                        self.add(t, Node::with(&format!("group.{g}"), true, 0, ctx))?;
                        self.say("parent_set", &[&label, &g]);
                    }
                    None => self.say("cleared", &[&label, &gone.len().to_string()]),
                }
                Ok(())
            }
            "switchprimarygroup" => {
                self.need(&format!("{who}.info"))?;
                self.say("primary_auto", &[]);
                Ok(())
            }
            "clear" => {
                self.need(&edit)?;
                self.guard_target(t)?;
                let ctx = self.ctx(rest)?;
                let gone = self.remove(t, |n| matches!(n.kind(), Kind::Group(_)) && (ctx.is_empty() || n.context == ctx))?;
                self.say("cleared", &[&label, &gone.len().to_string()]);
                Ok(())
            }
            other => self.fail("unknown_sub", &[&other.chars().take(40).collect::<String>()]),
        }
    }

    fn meta(&mut self, t: &Target, who: &str, w: &[String]) -> R {
        let sub = w.first().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
        let rest = w.get(1..).unwrap_or_default();
        let edit = format!("{who}.meta");
        let label = self.label(t);
        // The LuckPerms names spell out three choices: set/add/remove, temporary or not, prefix or suffix
        let fix = |name: &str| -> Option<(&'static str, bool, bool)> {
            let (verb, tail) = ["set", "add", "remove"].iter().find_map(|v| name.strip_prefix(v).map(|tail| (*v, tail)))?;
            let (temp, tail) = match tail.strip_prefix("temp") {
                Some(tail) => (true, tail),
                None => (false, tail),
            };
            match tail {
                "prefix" => Some((verb, temp, true)),
                "suffix" => Some((verb, temp, false)),
                _ => None,
            }
        };
        match sub.as_str() {
            "" | "info" | "list" => {
                self.need(&format!("{who}.info"))?;
                self.list(t, "meta_title", |k| matches!(k, Kind::Prefix(..) | Kind::Suffix(..) | Kind::Meta(..)));
                let now = self.e.now;
                let eff = match t {
                    Target::User(u) => self.d.resolve(u, &self.ctx_of(u), now),
                    Target::Group(g) => self.d.resolve_group(g, &Ctx::offline(), now),
                };
                let th = self.e.th;
                let show = |s: &str| if s.is_empty() { msg(th, "none", &[]) } else { format!("\"{s}&r\"") };
                self.row("line_prefix", &[&show(&eff.prefix), &show(&eff.suffix)]);
                Ok(())
            }
            "set" | "settemp" => {
                self.need(&edit)?;
                self.guard_target(t)?;
                let temp = sub == "settemp";
                let usage = if temp { "… meta settemp <key> <value> <duration> [accumulate|replace|deny] [context…]" } else { "… meta set <key> <value> [context…]" };
                let (Some(k), Some(v)) = (rest.first(), rest.get(1)) else { return self.usage(usage) };
                let k = k.to_ascii_lowercase();
                if k.is_empty() || k.len() > MAX_TEXT || k.chars().any(|c| c == '.' || c.is_whitespace() || c.is_control()) {
                    return self.fail("bad_meta_key", &[&k.chars().take(40).collect::<String>()]);
                }
                if v.chars().count() > MAX_TEXT {
                    return self.fail("too_long", &[&MAX_TEXT.to_string()]);
                }
                let (expiry, mode, at) = if temp { self.temp_tail(rest, 2, usage)? } else { (0, TempMode::Set, 2) };
                let ctx = self.ctx(rest.get(at..).unwrap_or_default())?;
                let key = self.key(&format!("meta.{k}.{v}"))?;
                // One value per key (and lifetime): the others go, the same value stays for the modifier to see
                self.remove(t, |n| matches!(n.kind(), Kind::Meta(mk, _) if mk == k) && n.context == ctx && n.is_temp() == temp && n.key != key)?;
                let node = Node::with(&key, true, expiry, ctx.clone());
                if temp {
                    self.add_temp(t, node, mode)?;
                    let end = self.d.nodes(t).iter().find(|n| n.key == key && n.context == ctx && n.is_temp()).map_or(expiry, |n| n.expiry);
                    let time = msg(self.e.th, "for_time", &[&fmt_duration(end.saturating_sub(self.e.now), self.e.th)]);
                    self.say("meta_set_temp", &[&label, &k, v, &time]);
                } else {
                    self.add(t, node)?;
                    self.say("meta_set", &[&label, &k, v]);
                }
                Ok(())
            }
            "unset" | "unsettemp" => {
                self.need(&edit)?;
                self.guard_target(t)?;
                let temp = sub == "unsettemp";
                let Some(k) = rest.first() else { return self.usage("… meta unset|unsettemp <key> [context…]") };
                let k = k.to_ascii_lowercase();
                let ctx = self.ctx(&rest[1..])?;
                let gone = self.remove(t, |n| matches!(n.kind(), Kind::Meta(mk, _) if mk == k) && n.context == ctx && n.is_temp() == temp)?;
                self.say("meta_unset", &[&label, &k.chars().take(40).collect::<String>(), &gone.len().to_string()]);
                Ok(())
            }
            "clear" => {
                self.need(&edit)?;
                self.guard_target(t)?;
                let gone = self.remove(t, |n| matches!(n.kind(), Kind::Prefix(..) | Kind::Suffix(..) | Kind::Meta(..)))?;
                self.say("cleared", &[&label, &gone.len().to_string()]);
                Ok(())
            }
            name => {
                let Some((verb, temp, prefix)) = fix(name) else { return self.fail("unknown_sub", &[&name.chars().take(40).collect::<String>()]) };
                self.need(&edit)?;
                self.guard_target(t)?;
                let word = if prefix { "prefix" } else { "suffix" };
                let word_shown = msg(self.e.th, &format!("word_{word}"), &[]);
                let is_kind = move |n: &Node| if prefix { matches!(n.kind(), Kind::Prefix(..)) } else { matches!(n.kind(), Kind::Suffix(..)) };
                let Some(p) = rest.first() else {
                    return self.usage(&match (verb, temp) {
                        ("remove", _) => format!("… meta {name} <priority> [context…]"),
                        (_, true) => format!("… meta {name} <priority> <text> <duration> [accumulate|replace|deny] [context…]"),
                        (_, false) => format!("… meta {name} <priority> <text> [context…]  (\"text with spaces\")"),
                    });
                };
                let Ok(prio) = p.parse::<i32>() else { return self.fail("bad_number", &[p]) };
                if verb == "remove" {
                    let ctx = self.ctx(&rest[1..])?;
                    let gone = self.remove(t, |n| {
                        n.context == ctx
                            && n.is_temp() == temp
                            && match n.kind() {
                                Kind::Prefix(q, _) => prefix && q == prio,
                                Kind::Suffix(q, _) => !prefix && q == prio,
                                _ => false,
                            }
                    })?;
                    self.say("prefix_removed", &[&label, &word_shown, &gone.len().to_string()]);
                    return Ok(());
                }
                let usage = if temp { format!("… meta {name} <priority> <text> <duration> [accumulate|replace|deny] [context…]") } else { format!("… meta {name} <priority> <text> [context…]  (\"text with spaces\")") };
                let Some(text) = rest.get(1) else { return self.usage(&usage) };
                if text.chars().count() > MAX_TEXT {
                    return self.fail("too_long", &[&MAX_TEXT.to_string()]);
                }
                let (expiry, mode, at) = if temp { self.temp_tail(rest, 2, &usage)? } else { (0, TempMode::Set, 2) };
                let ctx = self.ctx(rest.get(at..).unwrap_or_default())?;
                let key = self.key(&format!("{word}.{prio}.{text}"))?;
                if verb == "set" {
                    // `set…` leaves one prefix of that lifetime in the context; `add…` keeps the others
                    self.remove(t, |n| n.context == ctx && n.is_temp() == temp && is_kind(n) && n.key != key)?;
                }
                let node = Node::with(&key, true, expiry, ctx.clone());
                let shown = format!("\"{text}&r\"");
                if temp {
                    self.add_temp(t, node, mode)?;
                    let end = self.d.nodes(t).iter().find(|n| n.key == key && n.context == ctx && n.is_temp()).map_or(expiry, |n| n.expiry);
                    let time = msg(self.e.th, "for_time", &[&fmt_duration(end.saturating_sub(self.e.now), self.e.th)]);
                    self.say("prefix_set_temp", &[&label, &word_shown, &prio.to_string(), &shown, &time]);
                } else {
                    self.add(t, node)?;
                    self.say("prefix_set", &[&label, &word_shown, &prio.to_string(), &shown]);
                }
                Ok(())
            }
        }
    }

    fn step(&mut self, t: &Target, up: bool, rest: &[String]) -> R {
        self.need("user.track")?;
        self.guard_target(t)?;
        let Target::User(u) = t else { return Ok(()) };
        let Some(raw) = rest.first() else { return self.usage(if up { "user <name> promote <track>" } else { "user <name> demote <track>" }) };
        let track = raw.to_ascii_lowercase();
        let label = self.label(t);
        let shown: String = track.chars().take(40).collect();
        let mv = match self.d.track_step(u, &track, up, self.e.now) {
            Ok(mv) => mv,
            Err(TrackError::NoTrack) => return self.fail("no_track", &[&shown]),
            Err(TrackError::Empty) => return self.fail("track_empty", &[&shown]),
            Err(TrackError::Ambiguous) => return self.fail("track_ambiguous", &[&label, &shown]),
            Err(TrackError::AtEnd) => return self.fail("track_end", &[&label, &shown]),
            Err(TrackError::NotOnTrack) => return self.fail("track_not_on", &[&label, &shown]),
            Err(TrackError::MissingGroup(g)) => return self.fail("track_missing", &[&shown, &g]),
        };
        match &mv {
            TrackMove::Joined(g) | TrackMove::Moved(_, g) => self.guard_group_grant(g)?,
            TrackMove::Left(_) => {}
        }
        self.d.apply_track_move(u, &mv);
        match &mv {
            TrackMove::Joined(g) => self.say("track_joined", &[&label, &track, g]),
            TrackMove::Moved(a, b) => self.say(if up { "promoted" } else { "demoted" }, &[&label, &track, a, b]),
            TrackMove::Left(g) => self.say("track_left", &[&label, &track, g]),
        }
        Ok(())
    }

    // ---- tracks ----

    fn track(&mut self, w: &[String]) -> R {
        let Some(raw) = w.first() else { return self.usage("track <track> info|append <group>|insert <group> <pos>|remove <group>|clear|rename <new>|clone <new>") };
        let name = raw.to_ascii_lowercase();
        if !self.d.tracks.contains_key(&name) {
            return self.fail("no_track", &[&name.chars().take(40).collect::<String>()]);
        }
        let sub = w.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
        let rest = w.get(2..).unwrap_or_default();
        let show = |d: &Data| {
            let groups = &d.tracks[&name];
            if groups.is_empty() { "-".to_owned() } else { groups.join(" &7→&f ") }
        };
        match sub.as_str() {
            "" | "info" => {
                self.need("track.info")?;
                self.say("track_info", &[&name, &show(self.d)]);
                return Ok(());
            }
            "append" | "insert" => {
                self.need("track.edit")?;
                let Some(raw) = rest.first() else { return self.usage("track <track> append <group> | insert <group> <pos>") };
                let g = self.group(raw)?;
                if !self.free() && self.d.weight(&g) > self.own_weight() {
                    return self.fail("guard_group", &[&g]);
                }
                let groups = &self.d.tracks[&name];
                if groups.contains(&g) {
                    return self.fail("track_has", &[&name, &g]);
                }
                if groups.len() >= 64 {
                    return self.fail("track_long", &[]);
                }
                let at = if sub == "insert" {
                    let Some(p) = rest.get(1) else { return self.usage("track <track> insert <group> <pos>") };
                    match p.parse::<usize>() {
                        Ok(n) if n >= 1 && n <= groups.len() + 1 => n - 1,
                        _ => return self.fail("bad_number", &[p]),
                    }
                } else {
                    groups.len()
                };
                self.d.tracks.get_mut(&name).expect("checked").insert(at, g);
            }
            "remove" => {
                self.need("track.edit")?;
                let Some(raw) = rest.first() else { return self.usage("track <track> remove <group>") };
                let g = raw.to_ascii_lowercase();
                let groups = self.d.tracks.get_mut(&name).expect("checked");
                let before = groups.len();
                groups.retain(|x| *x != g);
                if groups.len() == before {
                    return self.fail("track_hasnt", &[&name, &g.chars().take(40).collect::<String>()]);
                }
            }
            "clear" => {
                self.need("track.edit")?;
                self.d.tracks.get_mut(&name).expect("checked").clear();
            }
            "rename" | "clone" => {
                self.need("track.edit")?;
                let Some(raw) = rest.first() else { return self.usage("track <track> rename|clone <new>") };
                let new = self.new_name(raw)?;
                if self.d.tracks.contains_key(&new) {
                    return self.fail("track_exists", &[&new]);
                }
                if sub == "clone" && self.d.tracks.len() >= MAX_TRACKS {
                    return self.fail("too_many", &[&MAX_TRACKS.to_string()]);
                }
                let groups = if sub == "rename" { self.d.tracks.remove(&name).expect("checked") } else { self.d.tracks[&name].clone() };
                self.d.tracks.insert(new.clone(), groups);
                self.d.dirty_tracks = true;
                self.say(if sub == "rename" { "track_renamed" } else { "track_cloned" }, &[&name, &new]);
                return Ok(());
            }
            other => return self.fail("unknown_sub", &[&other.chars().take(40).collect::<String>()]),
        }
        self.d.dirty_tracks = true;
        self.say("track_changed", &[&name, &show(self.d)]);
        Ok(())
    }

    // ---- action log ----

    fn log(&mut self, w: &[String]) -> R {
        let sub = w.first().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
        let rest = w.get(1..).unwrap_or_default();
        let usage = "log recent [user] [page] | search <text> [page] | userhistory <user> [page] | grouphistory <group> [page] | trackhistory <track> [page] | notify [on|off]";
        // The last word is the page when it is a number
        let page_of = |words: &[String]| words.last().and_then(|p| p.parse::<usize>().ok()).filter(|p| *p >= 1);
        let (pick, page): (Box<dyn Fn(&LogEntry) -> bool>, usize) = match sub.as_str() {
            "recent" => {
                let page = page_of(rest);
                match rest.first().filter(|_| !(rest.len() == 1 && page.is_some())) {
                    Some(who) => {
                        let who = who.to_lowercase();
                        (Box::new(move |e: &LogEntry| e.actor.to_lowercase() == who), page.filter(|_| rest.len() > 1).unwrap_or(1))
                    }
                    None => (Box::new(|_: &LogEntry| true), page.unwrap_or(1)),
                }
            }
            "search" => {
                let Some(q) = rest.first() else { return self.usage(usage) };
                let q = q.to_lowercase();
                (Box::new(move |e: &LogEntry| [&e.actor, &e.target, &e.action].iter().any(|f| f.to_lowercase().contains(&q))), page_of(&rest[1..]).unwrap_or(1))
            }
            "userhistory" | "grouphistory" | "trackhistory" => {
                let Some(name) = rest.first() else { return self.usage(usage) };
                let name = name.to_lowercase();
                let kind = match sub.as_str() {
                    "userhistory" => "U",
                    "grouphistory" => "G",
                    _ => "T",
                };
                (Box::new(move |e: &LogEntry| e.kind == kind && e.target.to_lowercase() == name), page_of(&rest[1..]).unwrap_or(1))
            }
            "notify" => {
                self.need("log.notify")?;
                // The console sees every edit in its own log already
                let Some(uuid) = self.a.uuid.clone() else { return self.fail("log_console", &[]) };
                let me = Target::User(uuid);
                let ignoring = self.d.nodes(&me).iter().any(|n| n.key == NOTIFY_IGNORE && n.value);
                let on = match rest.first().map(|s| s.to_ascii_lowercase()).as_deref() {
                    Some("on") => true,
                    Some("off") => false,
                    None => ignoring,
                    _ => return self.usage("log notify [on|off]"),
                };
                if on {
                    self.d.remove_where(&me, |n| n.key == NOTIFY_IGNORE);
                    self.say("log_notify_on", &[]);
                } else {
                    self.d.set_node(&me, Node::new(NOTIFY_IGNORE, true));
                    self.say("log_notify_off", &[]);
                }
                return Ok(());
            }
            _ => return self.usage(usage),
        };
        self.need("log")?;
        let entries = store::read_log(self.e.dir, pick);
        if entries.is_empty() {
            self.say("log_empty", &[]);
            return Ok(());
        }
        let pages = entries.len().div_ceil(LOG_PAGE);
        let page = page.min(pages);
        self.say("log_title", &[&page.to_string(), &pages.to_string(), &entries.len().to_string()]);
        let start = (page - 1) * LOG_PAGE;
        for (i, e) in entries.iter().enumerate().skip(start).take(LOG_PAGE) {
            self.raw(log_line(self.e.th, self.e.now, i + 1, e));
        }
        Ok(())
    }

    // ---- bulk update ----

    fn bulkupdate(&mut self, w: &[String]) -> R {
        self.need("bulkupdate")?;
        // It rewrites every holder at once: for the owner only, like an import
        if !self.free() {
            return self.fail("guard_owner", &[]);
        }
        let usage = "bulkupdate <all|users|groups> delete \"permission == <node>\" | update permission <new node> \"permission == <node>\" | confirm <id>   (== != ~~ !~, % and _ in ~~)";
        let actor = self.a.uuid.clone().unwrap_or_else(|| "console".to_owned());
        let now = self.e.now;
        let first = w.first().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
        if first == "confirm" {
            let id = w.get(1).cloned().unwrap_or_default();
            return match self.d.pending_bulk.take() {
                Some(p) if p.id == id && p.actor == actor && p.expires > now => {
                    let (nodes, holders) = self.d.bulk(p.scope, &p.constraint, &p.action, true);
                    self.note = Some(format!("bulkupdate {} ({nodes} nodes, {holders} holders)", p.text));
                    self.say("bulk_done", &[&nodes.to_string(), &holders.to_string()]);
                    Ok(())
                }
                other => {
                    // Someone else's pending update stays theirs
                    self.d.pending_bulk = other.filter(|p| p.actor != actor && p.expires > now);
                    self.fail("bulk_no_pending", &[])
                }
            };
        }
        let scope = match first.as_str() {
            "all" => Scope::All,
            "users" | "user" => Scope::Users,
            "groups" | "group" => Scope::Groups,
            _ => return self.usage(usage),
        };
        let (action, at) = match w.get(1).map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("delete") => (BulkAction::Delete, 2),
            Some("update") => {
                if w.get(2).map(|s| s.to_ascii_lowercase()).as_deref() != Some("permission") {
                    return self.usage(usage);
                }
                let Some(raw) = w.get(3) else { return self.usage(usage) };
                (BulkAction::Rename(self.key(raw)?), 4)
            }
            _ => return self.usage(usage),
        };
        // The constraint comes as one quoted word or as three words
        let joined = w.get(at..).unwrap_or_default().join(" ");
        let parts: Vec<&str> = joined.split_whitespace().collect();
        let [field, op, value] = parts.as_slice() else { return self.usage(usage) };
        if !field.eq_ignore_ascii_case("permission") {
            return self.usage(usage);
        }
        let cmp = match *op {
            "==" | "=" => Cmp::Eq,
            "!=" => Cmp::Ne,
            "~~" => Cmp::Like,
            "!~" => Cmp::NotLike,
            _ => return self.usage(usage),
        };
        let constraint = Constraint { cmp, value: (*value).to_owned() };
        let (nodes, holders) = self.d.bulk(scope, &constraint, &action, false);
        if nodes == 0 {
            self.d.pending_bulk = None;
            self.say("bulk_none", &[]);
            return Ok(());
        }
        let id = format!("{:04}", (now.wrapping_mul(2_654_435_761) >> 7).wrapping_add(nodes as u64) % 10_000);
        self.d.pending_bulk = Some(PendingBulk { id: id.clone(), actor, expires: now + 60, scope, constraint, action, text: w.join(" ") });
        self.say("bulk_preview", &[&nodes.to_string(), &holders.to_string(), &id]);
        Ok(())
    }

    // ---- top level ----

    fn top(&mut self, w: &[String]) -> R {
        let sub = w.first().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
        let rest = w.get(1..).unwrap_or_default();
        match sub.as_str() {
            "" | "help" | "?" => {
                self.say("help_title", &[self.e.version]);
                for line in HELP {
                    self.raw(format!(" &f/lp {line}"));
                }
                Ok(())
            }
            "user" | "u" => {
                let Some(name) = rest.first() else { return self.usage("user <name> info|permission|parent|meta|promote|demote|clear|clone") };
                let Some(id) = self.d.lookup_user(name) else {
                    // Asking about a name must not need more than the right to look
                    self.need("user.info")?;
                    return self.fail("no_user", &[&name.chars().take(40).collect::<String>()]);
                };
                self.holder(Target::User(id), &rest[1..])
            }
            "group" | "g" => {
                let Some(name) = rest.first() else { return self.usage("group <group> info|permission|parent|meta|setweight|setdisplayname|rename|clone|listmembers") };
                self.need("base")?;
                let g = self.group(name)?;
                self.holder(Target::Group(g), &rest[1..])
            }
            "creategroup" => {
                self.need("creategroup")?;
                let Some(raw) = rest.first() else { return self.usage("creategroup <name> [weight]") };
                let name = self.new_name(raw)?;
                if self.d.groups.contains_key(&name) {
                    return self.fail("group_exists", &[&name]);
                }
                if self.d.groups.len() >= MAX_GROUPS {
                    return self.fail("too_many", &[&MAX_GROUPS.to_string()]);
                }
                let weight = match rest.get(1) {
                    Some(raw) => Some(self.weight_arg(raw)?),
                    None => None,
                };
                self.d.create_group(&name);
                if let Some(w) = weight {
                    self.d.set_node(&Target::Group(name.clone()), Node::new(&format!("weight.{w}"), true));
                }
                self.say("group_created", &[&name]);
                Ok(())
            }
            "deletegroup" => {
                self.need("deletegroup")?;
                let Some(raw) = rest.first() else { return self.usage("deletegroup <name>") };
                let g = self.group(raw)?;
                if g == self.d.default_group {
                    return self.fail("group_default", &[&g]);
                }
                let t = Target::Group(g.clone());
                self.guard_target(&t)?;
                for n in self.d.nodes(&t) {
                    self.guard_remove(n)?;
                }
                self.d.delete_group(&g);
                self.say("group_deleted", &[&g]);
                Ok(())
            }
            "listgroups" => {
                self.need("listgroups")?;
                let mut groups: Vec<(i32, String, usize)> = self.d.groups.iter().map(|(g, n)| (self.d.weight(g), g.clone(), n.len())).collect();
                groups.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
                self.say("groups_title", &[&groups.len().to_string()]);
                let rows = groups
                    .into_iter()
                    .map(|(w, g, n)| {
                        let display = self.d.display_name(&g).map(|d| format!(" &7({d}&7)")).unwrap_or_default();
                        format!(" &7- &f{g}{display} &7weight &f{w} &7nodes &f{n}")
                    })
                    .collect();
                self.rows(rows);
                Ok(())
            }
            "createtrack" => {
                self.need("createtrack")?;
                let Some(raw) = rest.first() else { return self.usage("createtrack <name>") };
                let name = self.new_name(raw)?;
                if self.d.tracks.contains_key(&name) {
                    return self.fail("track_exists", &[&name]);
                }
                if self.d.tracks.len() >= MAX_TRACKS {
                    return self.fail("too_many", &[&MAX_TRACKS.to_string()]);
                }
                self.d.tracks.insert(name.clone(), Vec::new());
                self.d.dirty_tracks = true;
                self.say("track_created", &[&name]);
                Ok(())
            }
            "deletetrack" => {
                self.need("deletetrack")?;
                let Some(raw) = rest.first() else { return self.usage("deletetrack <name>") };
                let name = raw.to_ascii_lowercase();
                if self.d.tracks.remove(&name).is_none() {
                    return self.fail("no_track", &[&name.chars().take(40).collect::<String>()]);
                }
                self.d.dirty_tracks = true;
                self.say("track_deleted", &[&name]);
                Ok(())
            }
            "listtracks" => {
                self.need("listtracks")?;
                self.say("tracks_title", &[&self.d.tracks.len().to_string()]);
                let rows = self.d.tracks.iter().map(|(n, g)| format!(" &7- &f{n}&7: &f{}", if g.is_empty() { "-".to_owned() } else { g.join(" &7→&f ") })).collect();
                self.rows(rows);
                Ok(())
            }
            "track" | "t" => self.track(rest),
            "info" => {
                self.need("info")?;
                self.say("info_title", &[self.e.version]);
                let d = &*self.d;
                let counts = [d.groups.len().to_string(), d.users.len().to_string(), d.tracks.len().to_string(), d.names.len().to_string()];
                self.row("info_counts", &[&counts[0], &counts[1], &counts[2], &counts[3]]);
                let (cached, checks, answered, listeners, resends, micros) = self.e.stats;
                self.row("info_runtime", &[&cached.to_string(), &checks.to_string(), &answered.to_string(), &listeners.to_string()]);
                let each = if resends == 0 { 0.0 } else { micros as f64 / 1000.0 / resends as f64 };
                self.row("info_resend", &[&resends.to_string(), &format!("{each:.1}")]);
                self.row("info_files", &[]);
                Ok(())
            }
            "sync" | "reload" | "reloadconfig" | "networksync" => {
                self.need("sync")?;
                self.out.sync = true;
                Ok(())
            }
            "check" => {
                self.need("check")?;
                let (Some(name), Some(raw)) = (rest.first(), rest.get(1)) else { return self.usage("check <user> <node>") };
                let Some(id) = self.d.lookup_user(name) else { return self.fail("no_user", &[&name.chars().take(40).collect::<String>()]) };
                let key = self.key(raw)?;
                self.check(&Target::User(id), &key)
            }
            "tree" => {
                self.need("tree")?;
                let scope = rest.first().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
                let mut nodes = self.d.permission_keys();
                nodes.extend((self.e.seen)());
                let rows: Vec<String> = nodes.iter().filter(|n| n.starts_with(&scope)).map(|n| format!(" &7- &f{n}")).collect();
                let scoped = if scope.is_empty() { String::new() } else { msg(self.e.th, "tree_scope", &[&scope.chars().take(40).collect::<String>()]) };
                self.say("tree_title", &[&rows.len().to_string(), &scoped]);
                self.rows(rows);
                self.row("tree_hint", &[]);
                Ok(())
            }
            "search" => {
                self.need("search")?;
                let Some(raw) = rest.first() else { return self.usage("search <node>") };
                let key = self.key(raw)?;
                let now = self.e.now;
                let th = self.e.th;
                let mut rows = Vec::new();
                let word_g = msg(th, "word_group", &[]);
                let word_u = msg(th, "word_user", &[]);
                for (g, nodes) in &self.d.groups {
                    for n in nodes.iter().filter(|n| n.key == key) {
                        rows.push(msg(th, "search_line", &[&word_g, g, &n.describe(now, th)]));
                    }
                }
                for (id, u) in &self.d.users {
                    for n in u.nodes.iter().filter(|n| n.key == key) {
                        rows.push(msg(th, "search_line", &[&word_u, &self.d.user_name(id), &n.describe(now, th)]));
                    }
                }
                self.say("search_title", &[&key, &rows.len().to_string()]);
                self.rows(rows);
                Ok(())
            }
            "export" => {
                self.need("export")?;
                let Some(raw) = rest.first() else { return self.usage("export <file>") };
                let Some(file) = store::safe_file(raw) else { return self.fail("bad_file", &[&raw.chars().take(40).collect::<String>()]) };
                let folder = self.e.dir.join("exports");
                let have = std::fs::read_dir(&folder).map_or(0, |rd| rd.flatten().count());
                if have >= MAX_EXPORTS && !folder.join(&file).exists() {
                    return self.fail("too_many", &[&MAX_EXPORTS.to_string()]);
                }
                let text = store::export(self.d, self.e.now);
                match store::write_atomic(&folder.join(&file), text.as_bytes()) {
                    Ok(()) => {
                        let d = &*self.d;
                        let counts = [d.groups.len().to_string(), d.users.len().to_string(), d.tracks.len().to_string()];
                        self.say("exported", &[&file, &counts[0], &counts[1], &counts[2]]);
                        Ok(())
                    }
                    Err(e) => self.fail("export_fail", &[&e.to_string()]),
                }
            }
            "import" => {
                self.need("import")?;
                // An import can grant anything to anyone: not something to delegate
                if !self.free() {
                    return self.fail("guard_owner", &[]);
                }
                let Some(raw) = rest.first() else { return self.usage("import <file> [--replace]") };
                let Some(file) = store::safe_file(raw) else { return self.fail("bad_file", &[&raw.chars().take(40).collect::<String>()]) };
                let replace = rest.get(1).is_some_and(|f| f == "--replace" || f == "replace");
                let path = self.e.dir.join("exports").join(&file);
                let Ok(meta) = std::fs::metadata(&path) else { return self.fail("import_missing", &[&file]) };
                if meta.len() > store::MAX_IMPORT_BYTES {
                    return self.fail("import_big", &[]);
                }
                let text = std::fs::read_to_string(&path).or_else(|e| self.fail("import_bad", &[&e.to_string()]))?;
                match store::import(self.d, &text, replace) {
                    Ok(done) => {
                        let mut note = if replace { msg(self.e.th, "import_replaced", &[]) } else { String::new() };
                        if done.refused > 0 {
                            note.push_str(&msg(self.e.th, "import_refused", &[&done.refused.to_string()]));
                        }
                        self.say("imported", &[&done.groups.to_string(), &done.users.to_string(), &done.tracks.to_string(), &note]);
                        Ok(())
                    }
                    Err(e) => self.fail("import_bad", &[&e.chars().take(120).collect::<String>()]),
                }
            }
            "log" => self.log(rest),
            "bulkupdate" => self.bulkupdate(rest),
            "verbose" => {
                self.need("verbose")?;
                match rest.first().map(|s| s.to_ascii_lowercase()).as_deref() {
                    Some("on") | Some("record") => {
                        let filter: String = rest.get(1).map(|f| f.to_ascii_lowercase().chars().take(MAX_TEXT).collect()).unwrap_or_default();
                        let note = if filter.is_empty() { String::new() } else { msg(self.e.th, "verbose_filter", &[&filter]) };
                        self.say("verbose_on", &[&note]);
                        self.out.verbose = Some(Verbose::On(filter));
                        Ok(())
                    }
                    Some("off") | Some("stop") => {
                        self.say("verbose_off", &[]);
                        self.out.verbose = Some(Verbose::Off);
                        Ok(())
                    }
                    _ => self.usage("verbose on|off [filter]"),
                }
            }
            "editor" | "web" => {
                self.need("editor")?;
                // The page shows every group and player and can change any of them, and the
                // limits on delegated staff cannot be kept there: not something to delegate
                if !self.free() {
                    return self.fail("guard_owner", &[]);
                }
                self.say("editor_start", &[]);
                self.out.web = Some(WebAsk::Open);
                Ok(())
            }
            "applyedits" => {
                self.need("applyedits")?;
                if !self.free() {
                    return self.fail("guard_owner", &[]);
                }
                let Some(raw) = rest.first() else { return self.usage("applyedits <code>") };
                // People paste the whole link
                let code = raw.trim_end_matches('/').rsplit(['/', '#']).next().unwrap_or("");
                if !store::valid_code(code) {
                    return self.fail("edits_code", &[]);
                }
                self.say("edits_fetch", &[]);
                self.out.web = Some(WebAsk::Apply(code.to_owned()));
                Ok(())
            }
            other => self.fail("unknown_sub", &[&other.chars().take(40).collect::<String>()]),
        }
    }
}

/// One row of `/lp log`, also what watchers are told when it happens.
pub fn log_line(th: bool, now: u64, index: usize, e: &LogEntry) -> String {
    let ago = msg(th, "ago", &[&fmt_duration(now.saturating_sub(e.t), th)]);
    msg(th, "log_line", &[&index.to_string(), &ago, &e.actor, &e.kind, &e.target, &e.action])
}

/// What goes into the action log for a command line that changed something.
fn log_entry(d: &Data, a: &Actor, now: u64, words: &[String]) -> Option<LogEntry> {
    let first = words.first()?.to_ascii_lowercase();
    let join = |ws: &[String]| ws.iter().map(|w| if w.is_empty() || w.contains(char::is_whitespace) { format!("\"{w}\"") } else { w.clone() }).collect::<Vec<_>>().join(" ");
    let name = words.get(1).cloned().unwrap_or_default();
    let tail = join(words.get(2..).unwrap_or_default());
    let (kind, target, action) = match first.as_str() {
        // Turning one's own notices on or off is not an edit
        "log" => return None,
        "user" | "u" => ("U", d.lookup_user(&name).map_or(name, |id| d.user_name(&id)), tail),
        "group" | "g" => ("G", name.to_ascii_lowercase(), tail),
        "track" | "t" => ("T", name.to_ascii_lowercase(), tail),
        "creategroup" | "deletegroup" => ("G", name.to_ascii_lowercase(), format!("{first} {tail}").trim_end().to_owned()),
        "createtrack" | "deletetrack" => ("T", name.to_ascii_lowercase(), format!("{first} {tail}").trim_end().to_owned()),
        _ => ("S", "-".to_owned(), join(words)),
    };
    let clip = |s: String, n: usize| s.chars().filter(|c| !c.is_control()).take(n).collect::<String>();
    Some(LogEntry { t: now, actor: clip(a.name.clone(), 40), actor_id: a.uuid.clone().unwrap_or_default(), kind: kind.to_owned(), target: clip(target, 40), action: clip(action, 200) })
}

/// Run one `/lp …` line. Changed files are written before this returns.
pub fn run(d: &mut Data, a: &Actor, e: &Env, words: &[String]) -> Outcome {
    let mut run = Run { d, a, e, out: Outcome::default(), note: None };
    if let Err(line) = run.top(words) {
        run.out.lines.push(line);
    }
    let Run { d, mut out, note, .. } = run;
    out.changed = !d.dirty_groups.is_empty() || !d.dirty_users.is_empty() || d.dirty_tracks;
    if out.changed {
        let failed = store::flush(d, e.dir, false);
        if failed > 0 {
            out.lines.push(format!("{P}{}", msg(e.th, "save_fail", &[&failed.to_string()])));
        }
        if let Some(mut entry) = log_entry(d, a, e.now, words) {
            if let Some(note) = note {
                entry.action = note.chars().filter(|c| !c.is_control()).take(200).collect();
            }
            let _ = store::append_log(e.dir, &entry);
            out.logged = Some(entry);
        }
    }
    out
}

/// What `/lp sync` tells the sender after the glue reloaded: counts, then every file that was
/// skipped and every entry that was refused (nothing is dropped without a word).
pub fn sync_report(th: bool, d: &Data, bad_files: &[String], bad_entries: &[String]) -> Vec<String> {
    const SHOWN: usize = 12;
    let mut lines = vec![format!("{P}{}", msg(th, "synced", &[&d.groups.len().to_string(), &d.users.len().to_string(), &d.tracks.len().to_string()]))];
    lines.extend(bad_files.iter().take(SHOWN).map(|b| format!("{P}{}", msg(th, "sync_bad", &[b]))));
    lines.extend(bad_entries.iter().take(SHOWN).map(|b| format!("{P}{}", msg(th, "sync_entry", &[b]))));
    let hidden = bad_files.len().saturating_sub(SHOWN) + bad_entries.len().saturating_sub(SHOWN);
    if hidden > 0 {
        lines.push(msg(th, "more", &[&hidden.to_string()]));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    const ALICE: &str = "00000000-0000-0000-0000-00000000000a";
    const BOB: &str = "00000000-0000-0000-0000-00000000000b";
    static N: AtomicU32 = AtomicU32::new(0);

    struct World {
        d: Data,
        dir: PathBuf,
        now: u64,
        th: bool,
    }

    impl World {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("perms-cmd-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let mut d = Data::new("default");
            d.note_name(ALICE, "Alice");
            d.note_name(BOB, "Bob");
            World { d, dir, now: 1_000_000, th: false }
        }

        fn env_run(&mut self, a: &Actor, line: &str) -> Outcome {
            let env = Env {
                now: self.now,
                th: self.th,
                dir: &self.dir,
                version: "9.9.9",
                exempt: &[],
                online: &|_| None,
                final_check: &|_, _| None,
                seen: &|| vec!["seen:node.one".to_owned()],
                stats: (0, 0, 0, 0, 0, 0),
            };
            let words = crate::node::tokenize(line);
            let out = run(&mut self.d, a, &env, &words);
            for l in &out.lines {
                assert!(!l.contains("??"), "missing text for: {line} → {l}");
            }
            out
        }

        /// As the console.
        fn c(&mut self, line: &str) -> String {
            let a = Actor { console: true, bypass: true, uuid: None, name: "console".into(), has: &|_| true };
            self.env_run(&a, line).lines.join("\n")
        }

        /// As Bob, a player who is not an operator: his answers come from Perms's data alone.
        fn bob(&mut self, line: &str) -> String {
            let eff = self.d.resolve(BOB, &Ctx::offline(), self.now);
            let asked = RefCell::new(HashSet::new());
            let has = |n: &str| {
                asked.borrow_mut().insert(n.to_owned());
                eff.check(n, &[]).unwrap_or(false)
            };
            let a = Actor { console: false, bypass: false, uuid: Some(BOB.into()), name: "Bob".into(), has: &has };
            self.env_run(&a, line).lines.join("\n")
        }

        fn check(&self, uuid: &str, node: &str) -> Option<bool> {
            self.d.resolve(uuid, &Ctx::offline(), self.now).check(node, &[])
        }
    }

    impl Drop for World {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn groups_permissions_and_inheritance() {
        let mut w = World::new();
        assert!(w.c("creategroup VIP 10").contains("Created the group"));
        assert!(w.c("creategroup vip").contains("already exists"));
        assert!(w.c("creategroup Bad!Name").contains("not allowed"));
        assert!(w.c("group vip permission set Basics:Command.Fly").contains("basics:command.fly"));
        assert!(w.c("group vip permission set basics:command.fly true").contains("already has"));
        w.c("group vip permission set basics:command.god false");
        w.c("creategroup mvp 20");
        w.c("group mvp parent add vip");
        assert!(w.c("group mvp parent add mvp").contains("cannot inherit itself"));
        w.c("user alice parent add mvp");
        assert_eq!(w.check(ALICE, "basics:command.fly"), Some(true));
        assert_eq!(w.check(ALICE, "basics:command.god"), Some(false));
        assert_eq!(w.check(BOB, "basics:command.fly"), None);
        let info = w.c("user Alice info");
        assert!(info.contains("Primary group: &fmvp") && info.contains("mvp, vip, default"), "{info}");
        assert!(w.c("group vip info").contains("weight &f10"));
        assert!(w.c("group vip listmembers").contains("mvp"));
        assert!(w.c("group mvp listmembers").contains("Alice"));
        let list = w.c("listgroups");
        assert!(list.find("mvp").unwrap() < list.find("vip").unwrap(), "heaviest first: {list}");
        // files
        assert!(w.dir.join("groups/vip.toml").exists());
        assert!(w.dir.join(format!("users/{ALICE}.json")).exists());
        assert!(!w.dir.join(format!("users/{BOB}.json")).exists(), "a default-only player has no file");
        // unset and negation
        assert!(w.c("group vip permission unset basics:command.fly").contains("Removed"));
        assert!(w.c("group vip permission unset basics:command.fly").contains("has no"));
        assert_eq!(w.check(ALICE, "basics:command.fly"), None);
        w.c("user alice permission set a.b");
        w.c("user alice permission set a.b false");
        assert_eq!(w.check(ALICE, "a.b"), Some(false));
        assert!(w.c("user alice permission info").contains("-a.b"));
        assert!(w.c("user alice permission check a.b").contains("&cfalse"));
        assert!(w.c("check alice basics:command.god").contains("from &fvip"));
        assert!(w.c("check alice not.set").contains("server default"));
        assert!(w.c("user nobody info").contains("No player"));
        assert!(w.c("group nothing info").contains("No group"));
        // clear takes the file away again
        assert!(w.c("user alice clear").contains("Cleared 2"));
        assert!(!w.dir.join(format!("users/{ALICE}.json")).exists());
    }

    #[test]
    fn temporary_nodes_and_contexts() {
        let mut w = World::new();
        assert!(w.c("user alice permission settemp x.y true 10m").contains("left"));
        assert!(w.c("user alice permission settemp x.y maybe 10m").contains("true or false"));
        assert!(w.c("user alice permission settemp x.y true soon").contains("Bad duration"));
        assert!(w.c("user alice permission settemp x.y").contains("Usage"));
        assert_eq!(w.check(ALICE, "x.y"), Some(true));
        assert_eq!(w.d.next_expiry, w.now + 600);
        assert!(w.c("user alice permission unset x.y").contains("has no"), "unset leaves temporary nodes alone");
        assert!(w.c("user alice permission unsettemp x.y").contains("Removed"));
        w.c("user alice permission settemp x.y true 30s");
        w.now += 31;
        assert_eq!(w.check(ALICE, "x.y"), None);
        // contexts
        assert!(w.c("user alice permission set fly true world=nether").contains("world=the_nether"));
        assert!(w.c("user alice permission set fly world=minecraft:the_nether").contains("already has"), "true is the default value");
        assert!(w.c("user alice permission set fly true nonsense").contains("Bad context"));
        assert!(w.c("user alice permission set swim true region=spawn").contains("never applies"));
        assert_eq!(w.check(ALICE, "fly"), None, "the offline context is the overworld");
        assert_eq!(w.d.resolve(ALICE, &Ctx::new("minecraft:the_nether", "survival"), w.now).check("fly", &[]), Some(true));
        assert!(w.c("user alice permission unset fly").contains("has no"));
        assert!(w.c("user alice permission unset fly world=nether").contains("Removed"));
        assert!(w.c("user alice permission set fly true server=lobby").contains("Set"), "server= is accepted and ignored");
        assert_eq!(w.check(ALICE, "fly"), Some(true));
        // temporary parents
        w.c("creategroup vip");
        w.c("group vip permission set v.x");
        assert!(w.c("user alice parent addtemp vip 1h").contains("for &f1h"));
        assert_eq!(w.check(ALICE, "v.x"), Some(true));
        assert!(w.c("user alice parent remove vip").contains("is not in"));
        assert!(w.c("user alice parent removetemp vip").contains("Removed"));
        assert_eq!(w.check(ALICE, "v.x"), None);
    }

    #[test]
    fn parents_meta_and_weights() {
        let mut w = World::new();
        w.c("creategroup a 1");
        w.c("creategroup b 2");
        w.c("user alice parent add a");
        w.c("user alice parent add b");
        assert!(w.c("user alice parent set a").contains("Set the group"));
        assert_eq!(w.d.users[ALICE].nodes, vec![Node::new("group.a", true)]);
        assert!(w.c("user alice parent info").contains("default &7(implicit)"));
        assert!(w.c("user alice parent clear").contains("Cleared 1"));
        assert!(w.c("user alice parent add ghost").contains("No group"));
        // prefixes
        assert!(w.c(r#"group a meta setprefix 10 "&6[A] ""#).contains("&6[A] "));
        w.c(r#"group a meta addprefix 20 "&c[AA] ""#);
        w.c("group a meta setsuffix 5 &7*");
        w.c("user alice parent add a");
        let e = w.d.resolve(ALICE, &Ctx::offline(), w.now);
        assert_eq!((e.prefix.as_str(), e.suffix.as_str()), ("&c[AA] ", "&7*"));
        assert!(w.c("group a meta removeprefix 20").contains("(1)"));
        assert_eq!(w.d.resolve(ALICE, &Ctx::offline(), w.now).prefix, "&6[A] ");
        w.c("group a meta setprefix 30 new");
        assert_eq!(w.d.groups["a"].iter().filter(|n| matches!(n.kind(), Kind::Prefix(..))).count(), 1, "setprefix replaces");
        assert!(w.c("group a meta setprefix x y").contains("Bad number"));
        assert!(w.c(&format!("group a meta setprefix 1 {}", "x".repeat(65))).contains("longer"));
        // meta pairs
        w.c("user alice meta set Color gold");
        w.c("user alice meta set color red");
        assert_eq!(w.d.resolve(ALICE, &Ctx::offline(), w.now).meta["color"], "red");
        assert!(w.c("user alice meta set a.b c").contains("not allowed"));
        assert!(w.c("user alice meta info").contains("meta.color.red"));
        assert!(w.c("user alice meta unset color").contains("(1)"));
        assert!(w.c("user alice meta clear").contains("Cleared 0"));
        // weight and display name
        assert!(w.c("group a setweight 50").contains("50"));
        assert_eq!(w.d.weight("a"), 50);
        assert!(w.c("group a setweight heavy").contains("Bad number"));
        assert!(w.c("group a setdisplayname Very Important").contains("Very Important"));
        assert_eq!(w.d.display_name("a"), Some("Very Important"));
        // rename, clone, delete
        assert!(w.c("group a rename gold").contains("Renamed"));
        assert_eq!(w.d.primary(ALICE, &Ctx::offline(), w.now), "gold");
        assert!(w.c("group default rename x").contains("default group"));
        assert!(w.c("group gold clone gold2").contains("Cloned"));
        assert!(w.c("deletegroup default").contains("default group"));
        assert!(w.c("deletegroup gold").contains("Deleted"));
        assert!(!w.dir.join("groups/gold.toml").exists() && !w.dir.join("groups/a.toml").exists());
        assert!(w.d.users.is_empty());
        // user clone
        w.c("user alice permission set one.two");
        assert!(w.c("user alice clone bob").contains("Copied"));
        assert_eq!(w.check(BOB, "one.two"), Some(true));
    }

    #[test]
    fn tracks_and_promotion() {
        let mut w = World::new();
        for g in ["helper", "mod", "admin"] {
            w.c(&format!("creategroup {g}"));
        }
        assert!(w.c("createtrack Staff").contains("Created"));
        assert!(w.c("createtrack staff").contains("already exists"));
        w.c("track staff append helper");
        w.c("track staff append admin");
        assert!(w.c("track staff append admin").contains("already has"));
        assert!(w.c("track staff insert mod 2").contains("helper &7→&f mod &7→&f admin"));
        assert!(w.c("track staff insert mod 9").contains("already has"));
        assert!(w.c("track staff append ghost").contains("No group"));
        assert!(w.c("track staff info").contains("helper"));
        assert!(w.c("listtracks").contains("staff"));
        assert!(w.c("user alice demote staff").contains("is in no group"));
        assert!(w.c("user alice promote staff").contains("first group"));
        assert!(w.c("user alice promote staff").contains("helper &7→ &fmod"));
        assert!(w.c("user alice showtracks").contains("staff"));
        assert!(w.c("group mod showtracks").contains("staff"));
        w.c("user alice promote staff");
        assert!(w.c("user alice promote staff").contains("already at the end"));
        assert!(w.c("user alice demote staff").contains("admin &7→ &fmod"));
        assert!(w.c("user alice promote nothing").contains("No track"));
        assert!(w.c("track staff remove mod").contains("helper &7→&f admin"));
        assert!(w.c("track staff remove mod").contains("does not have"));
        assert!(w.c("track staff rename team").contains("Renamed"));
        assert!(w.c("track team clone team2").contains("Cloned"));
        assert!(w.c("track team clear").contains("-"));
        assert!(w.c("user alice promote team").contains("has no groups"));
        assert!(w.c("deletetrack team").contains("Deleted"));
        assert!(w.c("deletetrack team").contains("No track"));
        let text = std::fs::read_to_string(w.dir.join("tracks.toml")).unwrap();
        assert!(text.contains("team2") && !text.contains("staff"), "{text}");
    }

    #[test]
    fn delegated_admin_limits() {
        let mut w = World::new();
        w.c("creategroup helper 10");
        w.c("creategroup mod 20");
        w.c("creategroup admin 50");
        w.c("group mod permission set perms:command.*");
        w.c("group mod permission set basics:command.fly");
        w.c("group admin permission set basics:command.ban");
        w.c("group helper permission set basics:command.fly");
        w.c("user bob parent add mod");
        w.c("user alice parent add admin");

        // without any Perms node nothing works
        w.c("creategroup nobody");
        w.c("user alice parent set nobody");
        w.c("user alice parent add admin");
        // area nodes
        let mut plain = World::new();
        assert!(plain.bob("listgroups").contains("do not have permission"));
        assert!(plain.bob("user alice permission set x").contains("do not have permission"));

        // may give what he holds, to people who are not above him
        assert!(w.bob("user bob permission set basics:command.fly").contains("Set"));
        assert!(w.bob("group helper permission set basics:command.fly false").contains("Set"));
        // not what he lacks
        assert!(w.bob("user bob permission set basics:command.ban").contains("do not hold it"));
        assert!(w.bob("user bob permission set *").contains("do not hold it"));
        assert!(w.bob("user bob permission set basics:command.*").contains("do not hold it"));
        assert!(w.bob("user bob permission settemp basics:command.ban true 1h").contains("do not hold it"));
        assert_eq!(w.check(BOB, "basics:command.ban"), None);
        // a denial is always allowed, but lifting one he does not hold is a grant
        w.c("group helper permission set basics:command.ban false");
        assert!(w.bob("group helper permission unset basics:command.ban").contains("cannot lift"));
        assert!(w.bob("group helper clear").contains("cannot lift"));
        assert!(w.bob("deletegroup helper").contains("cannot lift"));
        // heavier groups and their members are out of reach
        assert!(w.bob("group admin permission set basics:command.fly").contains("heavier"));
        assert!(w.bob("group admin setweight 1").contains("heavier"));
        assert!(w.bob("deletegroup admin").contains("heavier"));
        assert!(w.bob("user alice permission set basics:command.fly false").contains("heavier group"));
        assert!(w.bob("user alice clear").contains("heavier group"));
        // no way up through groups
        assert!(w.bob("user bob parent add admin").contains("heavier"));
        assert!(w.bob("user bob parent set admin").contains("heavier"));
        assert_eq!(w.d.primary(BOB, &Ctx::offline(), w.now), "mod", "a refused set leaves the old group");
        assert!(w.bob("group mod parent add admin").contains("heavier"));
        assert!(w.bob("group mod setweight 99").contains("above your own"));
        assert!(w.bob("creategroup boss 99").contains("above your own"));
        assert!(w.bob("group helper permission set weight.99").contains("above your own"));
        // a lighter group that holds something he lacks cannot be handed out either
        w.c("creategroup builder 5");
        w.c("group builder permission set worldtools:use");
        assert!(w.bob("user bob parent add builder").contains("which you do not"));
        assert!(w.bob("user bob parent add helper").contains("Added"), "helper holds only what he holds: {}", w.bob("user bob parent add helper"));
        // tracks obey the same rule
        w.c("createtrack staff");
        w.c("track staff append helper");
        w.c("track staff append mod");
        w.c("track staff append admin");
        w.c("user bob parent remove helper");
        assert!(w.bob("user bob promote staff").contains("heavier"));
        assert!(w.bob("track staff append admin").contains("heavier"));
        // cloning cannot smuggle nodes
        assert!(w.bob("user alice clone bob").contains("heavier group"));
        // import is for owners
        assert!(w.bob("export b1").contains("Exported"));
        assert!(w.bob("import b1").contains("Only the console"));
        // an operator of level 4 has no such limits
        let has = |_: &str| true;
        let op = Actor { console: false, bypass: true, uuid: Some(BOB.into()), name: "Bob".into(), has: &has };
        assert!(w.env_run(&op, "user bob parent add admin").lines[0].contains("Added"));
    }

    #[test]
    fn search_tree_export_import_and_misc() {
        let mut w = World::new();
        w.c("creategroup vip 10");
        w.c("group vip permission set basics:command.fly");
        w.c("user alice permission set basics:command.fly false");
        w.c("createtrack donor");
        w.c("track donor append vip");
        let s = w.c("search basics:command.fly");
        assert!(s.contains("group &fvip") && s.contains("player &fAlice") && s.contains(": 2"), "{s}");
        let t = w.c("tree");
        assert!(t.contains("basics:command.fly") && t.contains("seen:node.one"), "{t}");
        assert!(!w.c("tree seen:").contains("basics:command.fly"));
        assert!(w.c("export ../../etc/passwd").contains("not allowed"));
        assert!(w.c("export backup").contains("exports/backup.json"));
        assert!(w.dir.join("exports/backup.json").exists());
        assert!(w.c("import nothing").contains("No file"));

        let mut clean = World::new();
        std::fs::create_dir_all(clean.dir.join("exports")).unwrap();
        std::fs::copy(w.dir.join("exports/backup.json"), clean.dir.join("exports/backup.json")).unwrap();
        assert!(clean.c("import backup").contains("Imported: &f2"));
        assert_eq!(clean.check(ALICE, "basics:command.fly"), Some(false));
        assert_eq!(clean.d.tracks["donor"], vec!["vip"]);
        assert!(clean.dir.join("groups/vip.toml").exists(), "an import is written to the files");
        std::fs::write(clean.dir.join("exports/junk.json"), "not json").unwrap();
        assert!(clean.c("import junk").contains("Could not read"));

        assert!(w.c("info").contains("9.9.9"));
        assert!(w.c("help").contains("creategroup"));
        assert!(w.c("").contains("creategroup"));
        // The web editor: the command only asks; the glue does the talking
        let a = Actor { console: true, bypass: true, uuid: None, name: "console".into(), has: &|_| true };
        let out = w.env_run(&a, "editor");
        assert_eq!(out.web, Some(WebAsk::Open));
        assert!(out.lines[0].contains("Preparing"));
        assert_eq!(w.env_run(&a, "applyedits Ab3dEf7hJk").web, Some(WebAsk::Apply("Ab3dEf7hJk".into())));
        assert_eq!(w.env_run(&a, "applyedits https://perms.mcsv.me/Ab3dEf7hJk").web, Some(WebAsk::Apply("Ab3dEf7hJk".into())));
        for bad in ["applyedits", "applyedits ../x", "applyedits a b", "applyedits abc", "applyedits https://evil/?x=1"] {
            let out = w.env_run(&a, bad);
            assert_eq!(out.web, None, "{bad}");
            assert!(!out.lines.is_empty(), "{bad}");
        }
        // Staff with delegated rights cannot open or apply it
        assert!(!w.bob("editor").contains("Preparing"));
        assert!(!w.bob("applyedits Ab3dEf7hJk").contains("Fetching"));
        assert!(w.c("frobnicate").contains("Unknown subcommand"));
        assert!(w.c("user alice frobnicate").contains("Unknown subcommand"));
        assert!(w.c("user").contains("Usage"));
        let a = Actor { console: true, bypass: true, uuid: None, name: "console".into(), has: &|_| true };
        assert_eq!(w.env_run(&a, "verbose on Basics").verbose, Some(Verbose::On("basics".into())));
        assert_eq!(w.env_run(&a, "verbose off").verbose, Some(Verbose::Off));
        assert!(w.c("verbose").contains("Usage"));
        assert!(w.env_run(&a, "sync").sync);
        assert!(!w.env_run(&a, "info").changed);
        assert!(w.env_run(&a, "group vip permission set z.z").changed);
        // hostile input stays text
        assert!(w.c("user alice permission set {0}{1}").contains("{0}{1}"));
        // nobody fills the disk with groups, tracks or exports
        for i in 0..MAX_EXPORTS {
            std::fs::write(w.dir.join(format!("exports/f{i}.json")), "{}").unwrap();
        }
        assert!(w.c("export another").contains("at most"));
        assert!(w.c("export backup").contains("Exported"), "an existing name may be written again");
        for i in 0..MAX_GROUPS {
            w.d.create_group(&format!("g{i}"));
        }
        assert!(w.c("creategroup onemore").contains("at most"));
        assert!(w.c("group vip clone onemore").contains("at most"));
        for i in 0..MAX_TRACKS {
            w.d.tracks.insert(format!("t{i}"), Vec::new());
        }
        assert!(w.c("createtrack onemore").contains("at most"));
        assert!(w.c(&format!("user alice permission set {}", "a".repeat(500))).contains("not allowed"));
    }

    #[test]
    fn temporary_meta_and_modifiers() {
        let mut w = World::new();
        w.c("creategroup vip 10");
        w.c("user alice parent add vip");
        w.c(r#"group vip meta setprefix 10 "&6[VIP] ""#);
        // a temporary prefix with a higher priority shows until its time is up
        assert!(w.c(r#"user alice meta settempprefix 50 "&c[Event] " 10m"#).contains("for &f10m"));
        assert_eq!(w.d.resolve(ALICE, &Ctx::offline(), w.now).prefix, "&c[Event] ");
        assert_eq!(w.d.next_expiry, w.now + 600);
        // settempprefix leaves one temporary prefix; addtempprefix keeps the others; permanent ones are not touched
        w.c(r#"user alice meta setprefix 5 "&7own ""#);
        w.c("user alice meta addtempprefix 60 second 1h");
        w.c("user alice meta settempprefix 70 third 1h");
        let temp: Vec<&str> = w.d.users[ALICE].nodes.iter().filter(|n| n.is_temp()).map(|n| n.key.as_str()).collect();
        assert_eq!(temp, vec!["prefix.70.third"]);
        assert!(w.d.users[ALICE].nodes.iter().any(|n| n.key == "prefix.5.&7own "));
        assert!(w.c("user alice meta removeprefix 70").contains("(0)"), "removeprefix leaves temporary ones alone");
        assert!(w.c("user alice meta removetempprefix 70").contains("(1)"));
        assert!(w.c("user alice meta settempsuffix 5 * 30s").contains("suffix"));
        assert!(w.c("user alice meta addtempsuffix 6 ** 30s").contains("suffix"));
        assert_eq!(w.d.resolve(ALICE, &Ctx::offline(), w.now).suffix, "**");
        w.now += 31;
        assert_eq!(w.d.resolve(ALICE, &Ctx::offline(), w.now).suffix, "", "gone when the time is up, before any purge");
        assert!(w.d.purge(w.now));
        assert!(w.c("user alice meta removetempsuffix 6").contains("(0)"));
        assert!(w.c("user alice meta settempprefix 1 x").contains("Usage"));
        assert!(w.c("user alice meta settempprefix 1 x soon").contains("Bad duration"));
        assert!(w.c("user alice meta frobprefix 1 x").contains("Unknown subcommand"));
        // temporary meta pairs
        w.c("user alice meta set color red");
        assert!(w.c("user alice meta settemp color gold 5m").contains("for &f5m"));
        assert_eq!(w.d.resolve(ALICE, &Ctx::offline(), w.now).meta["color"], "gold", "temporary beats permanent on one holder");
        assert!(w.c("user alice meta unset color").contains("(1)"));
        assert_eq!(w.d.resolve(ALICE, &Ctx::offline(), w.now).meta["color"], "gold");
        assert!(w.c("user alice meta unsettemp color").contains("(1)"));
        assert!(!w.d.resolve(ALICE, &Ctx::offline(), w.now).meta.contains_key("color"));

        // modifiers: no word = the new time; accumulate adds; replace keeps the later end; deny refuses
        let end = |w: &World, key: &str| w.d.users[ALICE].nodes.iter().find(|n| n.key == key && n.is_temp()).map(|n| n.expiry - w.now);
        w.c("user alice permission settemp x.y true 10m");
        w.c("user alice permission settemp x.y true 5m");
        assert_eq!(end(&w, "x.y"), Some(300));
        assert!(w.c("user alice permission settemp x.y true 10m accumulate").contains("15m"));
        assert_eq!(end(&w, "x.y"), Some(900));
        w.c("user alice permission settemp x.y true 1m replace");
        assert_eq!(end(&w, "x.y"), Some(900), "replace keeps the later end");
        w.c("user alice permission settemp x.y true 1h replace");
        assert_eq!(end(&w, "x.y"), Some(3600));
        assert!(w.c("user alice permission settemp x.y true 1h deny").contains("already has a temporary"));
        assert!(w.c("user alice permission settemp new.one true 1h deny world=nether").contains("Set"));
        assert_eq!(w.d.users[ALICE].nodes.iter().find(|n| n.key == "new.one").unwrap().context["world"], "the_nether", "a context may follow the modifier");
        w.c("user alice parent addtemp vip 1h");
        assert!(w.c("user alice parent addtemp vip 1h accumulate").contains("for &f2h"));
        assert!(w.c("user alice parent addtemp vip 1h deny").contains("already has a temporary"));
        w.c("user alice meta settempprefix 9 p 10m");
        assert!(w.c("user alice meta settempprefix 9 p 10m accumulate").contains("for &f20m"));
        w.c("user alice meta settemp k v 10m");
        assert!(w.c("user alice meta settemp k v 10m accumulate").contains("for &f20m"));
        // an expired leftover is not "already there"
        w.c("user alice permission settemp gone.soon true 10s");
        w.now += 11;
        assert!(w.c("user alice permission settemp gone.soon true 10s deny").contains("Set"));
    }

    #[test]
    fn action_log_commands() {
        let mut w = World::new();
        assert!(w.c("log recent").contains("No log entries"));
        w.c("creategroup vip 10");
        w.c("group vip permission set basics:command.fly");
        w.c(r#"group vip meta setprefix 10 "&6[VIP] ""#);
        w.c("user alice parent add vip");
        w.c("createtrack staff");
        w.c("track staff append vip");
        w.c("listgroups");
        w.c("user alice info");
        w.c("user ghost parent add vip");
        w.c("group vip permission set bad node");
        let log = w.c("log recent");
        assert!(log.contains("(6 entries)"), "reads and refused commands are not logged: {log}");
        let first = log.lines().nth(1).unwrap();
        assert!(first.contains("#1") && first.contains("console") && first.contains("[T]") && first.contains("staff") && first.contains("append vip"), "{first}");
        assert!(log.contains("[U] &fAlice&7: parent add vip"));
        assert!(log.contains("[G] &fvip&7: creategroup 10"));
        assert!(log.contains(r#"meta setprefix 10 "&6[VIP] ""#), "words with spaces are quoted again: {log}");
        // as a player, and filters
        w.c("group vip permission set perms:command.*");
        w.c("user bob parent add vip");
        assert!(w.bob("user bob permission set basics:command.fly").contains("Set"));
        w.now += 120;
        let by_bob = w.c("log recent Bob");
        assert!(by_bob.contains("(1 entries)") && by_bob.contains("2m ago") && by_bob.contains("&bBob"), "{by_bob}");
        assert!(w.c("log recent console").contains("(8 entries)"));
        assert!(w.c("log search SETPREFIX").contains("(1 entries)"));
        assert!(w.c("log userhistory bob").contains("(2 entries)"));
        assert!(w.c("log grouphistory vip").contains("(4 entries)"));
        assert!(w.c("log trackhistory staff").contains("(2 entries)"));
        assert!(w.c("log userhistory nobody").contains("No log entries"));
        assert!(w.c("log").contains("Usage") && w.c("log search").contains("Usage"));
        // pages
        for i in 0..25 {
            w.c(&format!("group vip permission set p.{i}"));
        }
        let p1 = w.c("log recent");
        assert!(p1.contains("&f1&7/&f4") && p1.lines().count() == 11 && p1.contains("p.24") && !p1.contains("p.14"), "{p1}");
        let p2 = w.c("log recent 2");
        assert!(p2.contains("&f2&7/&f4") && p2.contains("p.14") && p2.contains("#11"), "{p2}");
        assert!(w.c("log recent 99").contains("&f4&7/&f4"), "a page past the end shows the last one");
        assert!(w.c("log recent console 2").contains("&f2&7/&f4"));
        assert!(w.dir.join("log.jsonl").exists());
        // the outcome carries the entry for the watchers
        let a = Actor { console: true, bypass: true, uuid: None, name: "console".into(), has: &|_| true };
        let out = w.env_run(&a, "group vip setweight 11");
        let e = out.logged.unwrap();
        assert_eq!((e.kind.as_str(), e.target.as_str(), e.action.as_str(), e.actor.as_str()), ("G", "vip", "setweight 11", "console"));
        assert!(w.env_run(&a, "listgroups").logged.is_none());
        // notify: a player's own switch, stored on the player, not an edit
        assert!(w.c("log notify off").contains("console already sees"));
        assert!(w.bob("log notify off").contains("no longer"));
        assert!(w.d.users[BOB].nodes.iter().any(|n| n.key == NOTIFY_IGNORE));
        assert!(w.bob("log notify").contains("will be told"), "no word toggles");
        assert!(!w.d.users[BOB].nodes.iter().any(|n| n.key == NOTIFY_IGNORE));
        assert!(w.bob("log notify off").contains("no longer") && w.bob("log notify on").contains("will be told"));
        assert!(w.bob("log notify maybe").contains("Usage"));
        assert!(!w.c("log search notify").contains("log notify"), "the switch is not logged");
        let mut plain = World::new();
        assert!(plain.bob("log recent").contains("do not have permission"));
    }

    #[test]
    fn checkinherits_tracks_on_parents_and_bulk() {
        let mut w = World::new();
        for g in ["helper", "mod", "vip"] {
            w.c(&format!("creategroup {g}"));
        }
        w.c("group vip permission set basics:command.*");
        w.c("user alice parent add vip");
        let out = w.c("user alice permission checkinherits basics:command.fly");
        assert!(out.contains("from &fvip") && out.contains("basics:command.*"), "{out}");
        assert!(w.c("group vip permission checkinherits basics:command.fly").contains("&atrue"));
        assert!(w.c("user alice parent switchprimarygroup vip").contains("heaviest"));
        // parent settrack / cleartrack, the same for a user and a group
        w.c("createtrack staff");
        w.c("track staff append helper");
        w.c("track staff append mod");
        for holder in ["user alice", "group vip"] {
            assert!(w.c(&format!("{holder} parent settrack staff helper")).contains("helper"));
            assert!(w.c(&format!("{holder} parent settrack staff mod")).contains("mod"));
            let info = w.c(&format!("{holder} parent info"));
            assert!(info.contains("group.mod") && !info.contains("group.helper"), "{info}");
            assert!(w.c(&format!("{holder} parent settrack staff vip")).contains("does not have"));
            assert!(w.c(&format!("{holder} parent settrack nothing mod")).contains("No track"));
            assert!(w.c(&format!("{holder} parent cleartrack staff")).contains("Cleared 1"));
            assert!(w.c(&format!("{holder} parent addtemp helper 1h")).contains("for &f1h"));
            assert!(w.c(&format!("{holder} parent removetemp helper")).contains("Removed"));
            assert!(w.c(&format!("{holder} parent add helper world=nether")).contains("Added"));
            assert!(w.c(&format!("{holder} parent remove helper world=nether")).contains("Removed"));
            assert!(w.c(&format!("{holder} parent set mod")).contains("Set the group"));
            assert!(w.c(&format!("{holder} parent clear")).contains("Cleared 1"));
        }
        assert!(w.c("group mod parent settrack staff mod").contains("cannot inherit itself"));
        w.c("user alice parent add vip");

        // bulk update: count first, change only on confirm
        w.c("group helper permission set old.node");
        w.c("group mod permission set old.node false");
        w.c("user alice permission set old.node");
        w.c("user alice permission set old.other");
        assert!(w.c("bulkupdate all delete permission == nothing.here").contains("No node matches"));
        let preview = w.c(r#"bulkupdate all update permission new.node "permission == old.node""#);
        assert!(preview.contains("&f3 &enodes on &f3"), "{preview}");
        assert_eq!(w.check(ALICE, "old.node"), Some(true), "a preview changes nothing");
        let id = preview.rsplit("confirm ").next().unwrap().trim().to_owned();
        assert!(w.c("bulkupdate confirm 0000x").contains("No pending"));
        assert!(w.c(&format!("bulkupdate confirm {id}")).contains("No pending"), "a wrong id cancels it");
        let preview = w.c("bulkupdate all update permission new.node permission == old.node");
        let id = preview.rsplit("confirm ").next().unwrap().trim().to_owned();
        assert!(w.c(&format!("bulkupdate confirm {id}")).contains("Changed &f3"));
        assert_eq!(w.check(ALICE, "new.node"), Some(true));
        assert_eq!(w.check(ALICE, "old.node"), None);
        assert_eq!(w.d.groups["mod"].iter().find(|n| n.key == "new.node").map(|n| n.value), Some(false), "values are kept");
        let logged = w.c("log search bulkupdate");
        assert!(logged.contains("[S] &f-&7: bulkupdate all update permission new.node permission == old.node (3 nodes, 3 holders)"), "the log says what was done, not the id: {logged}");
        // scopes, LIKE, and the time limit
        let preview = w.c(r#"bulkupdate users delete "permission ~~ old.%""#);
        assert!(preview.contains("&f1 &enodes on &f1"), "{preview}");
        let id = preview.rsplit("confirm ").next().unwrap().trim().to_owned();
        w.now += 61;
        assert!(w.c(&format!("bulkupdate confirm {id}")).contains("No pending"), "after a minute it is void");
        let preview = w.c(r#"bulkupdate groups delete "permission ~~ new.%""#);
        assert!(preview.contains("&f2 &enodes on &f2"), "{preview}");
        let id = preview.rsplit("confirm ").next().unwrap().trim().to_owned();
        assert!(w.c(&format!("bulkupdate confirm {id}")).contains("Changed &f2"));
        assert_eq!(w.check(ALICE, "new.node"), Some(true), "the user's own node was out of scope");
        for bad in ["bulkupdate", "bulkupdate all", "bulkupdate all delete", "bulkupdate all delete server == x", "bulkupdate all delete permission <> x", "bulkupdate all update permission", "bulkupdate all update weight 5 permission == x", "bulkupdate everyone delete permission == x"] {
            assert!(w.c(bad).contains("Usage"), "{bad}");
        }
        assert!(w.c("bulkupdate all update permission {bad key} permission == x").contains("Usage") || w.c(r#"bulkupdate all update permission "bad key" permission == x"#).contains("not allowed"));
        // not for delegates, whatever nodes they hold
        w.c("group vip permission set perms:command.*");
        w.c("user bob parent add vip");
        assert!(w.bob("bulkupdate all delete permission == new.node").contains("Only the console"));
    }

    #[test]
    fn regex_and_shorthand_nodes() {
        let mut w = World::new();
        assert!(w.c(r"user alice permission set r=basics:command\.(fly|heal)").contains("Set"));
        assert_eq!(w.check(ALICE, "basics:command.fly"), Some(true));
        assert_eq!(w.check(ALICE, "basics:command.feed"), None);
        assert!(w.c("user alice permission set r=(broken").contains("not allowed"));
        let out = w.c("user alice permission checkinherits basics:command.heal");
        assert!(out.contains(r"matched &fr=basics:command\.(fly|heal)"), "{out}");
        assert!(w.c(r"user alice permission unset r=basics:command\.(fly|heal)").contains("Removed"));
        assert!(w.c("user alice permission set kit.(starter|vip|1-3)").contains("Set"));
        assert_eq!(w.check(ALICE, "kit.vip"), Some(true));
        assert_eq!(w.check(ALICE, "kit.2"), Some(true));
        assert_eq!(w.check(ALICE, "kit.4"), None);
        // a delegate: no regex at all; a shorthand only when every node it stands for is held
        w.c("creategroup mod 20");
        w.c("group mod permission set perms:command.*");
        w.c("group mod permission set basics:command.fly");
        w.c("group mod permission set basics:command.heal");
        w.c("user bob parent add mod");
        assert!(w.bob("user bob permission set r=basics:.*").contains("Regex nodes"));
        assert!(w.bob("user bob permission set r=basics:.* false").contains("Regex nodes"));
        assert!(w.bob("user bob permission set basics:command.(fly|heal)").contains("Set"));
        assert!(w.bob("user bob permission set basics:command.(fly|ban)").contains("basics:command.ban"));
        w.c("group mod permission set kit.(a|b) false");
        assert!(w.bob("group mod permission unset kit.(a|b)").contains("cannot lift"));
        // a group that gives by regex cannot be handed out by a delegate
        w.c("creategroup wide 5");
        w.c("group wide permission set r=basics:command\\..*");
        assert!(w.bob("user bob parent add wide").contains("which you do not"));
    }

    #[test]
    fn sync_report_names_what_was_skipped() {
        let d = Data::new("default");
        let files = vec!["groups/broken.toml: expected `]`".to_owned()];
        let entries: Vec<String> = (0..15).map(|i| format!("groups/vip.toml: entry {i} \"bad key\": bad character in key")).collect();
        let lines = sync_report(false, &d, &files, &entries);
        assert!(lines[0].contains("Reloaded from disk"));
        assert!(lines[1].contains("Could not read this file") && lines[1].contains("broken.toml"));
        assert!(lines[2].contains("not valid, skipped") && lines[2].contains("entry 0"));
        assert_eq!(lines.len(), 1 + 1 + 12 + 1);
        assert!(lines.last().unwrap().contains("3 more"));
        assert_eq!(sync_report(true, &d, &[], &[]).len(), 1);
    }

    #[test]
    fn thai_messages_have_no_holes() {
        let mut w = World::new();
        w.th = true;
        for line in [
            "creategroup vip 5", "group vip permission set a.b", "user alice parent add vip", "user alice info", "group vip info", "user alice permission info",
            "user alice parent info", "user alice meta info", "check alice a.b", "listgroups", "listtracks", "info", "tree", "search a.b", "editor", "help",
            "user alice permission settemp c.d true 1h world=nether", "user ghost info", "group vip listmembers", "createtrack t", "track t append vip", "user alice promote t",
        ] {
            let out = w.c(line);
            for n in 0..10 {
                assert!(!out.contains(&format!("{{{n}}}")), "{line}: {out}");
            }
            assert!(!out.is_empty(), "{line}");
        }
        assert!(w.c("user alice info").contains("กลุ่มหลัก"));
    }
}

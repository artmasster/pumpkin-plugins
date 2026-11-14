//! Files under `plugins/data/perms/` (plain Rust: `std::fs` only, so tests run on the host).
//!
//! `config.toml`, `groups/<name>.toml`, `users/<uuid>.json`, `tracks.toml`, `names.json`,
//! `exports/<file>.json`. Every write goes to a temp file first and is renamed into place.
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::{Data, NameEntry, User};
use crate::node::{MAX_NODES, Node, parse_uuid, valid_name};

/// An export bigger than this is not read back in one call.
pub const MAX_IMPORT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// `th` or `en`.
    pub language: String,
    /// The group every player inherits.
    pub default_group: String,
    /// Put prefix and suffix around the name in the tab list (only for players who have one).
    pub tab_list_prefix: bool,
    /// Publish prefix, suffix, primary group and weight on the player for other plugins.
    pub export_custom_data: bool,
    /// `/lp verbose`: lines per second to one listener, and seconds until it turns itself off (0 = never).
    pub verbose_max_per_second: u32,
    pub verbose_timeout: u64,
    /// Node prefixes a granting wildcard does not reach unless it is at least that specific.
    pub wildcard_exempt: Vec<String>,
    /// The web editor that `/lp editor` and `/lp applyedits` talk to. Empty = off.
    pub editor_url: String,
}

/// The web editor a build talks to when `editor-url` is not set in `config.toml`. Empty = the
/// editor is off until the owner sets `editor-url`. A plain build has none; a host that runs an
/// editor service bakes its address in with `PERMS_EDITOR_URL` at build time (MCSV's builds do,
/// in `scripts/publish.sh`).
pub const EDITOR_URL: &str = match option_env!("PERMS_EDITOR_URL") {
    Some(url) => url,
    None => "",
};

impl Default for Config {
    fn default() -> Self {
        Config {
            language: "th".into(),
            default_group: "default".into(),
            tab_list_prefix: true,
            export_custom_data: true,
            verbose_max_per_second: 20,
            verbose_timeout: 300,
            wildcard_exempt: vec!["tablist:group.".into()],
            editor_url: EDITOR_URL.into(),
        }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Self {
        if !valid_name(&self.default_group) {
            self.default_group = "default".into();
        }
        self.verbose_max_per_second = self.verbose_max_per_second.clamp(1, 200);
        self.wildcard_exempt.retain(|e| !e.trim().is_empty() && e.len() <= 160);
        // a config written before the 0.6.0 rename names TabList's groups `roster:group.`
        for e in &mut self.wildcard_exempt {
            *e = crate::node::modern_key(e);
        }
        self.wildcard_exempt.truncate(64);
        self.editor_url = self.editor_url.trim().trim_end_matches('/').to_owned();
        let plain = self.editor_url.len() <= 200 && self.editor_url.bytes().all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\\');
        if !(plain && (self.editor_url.starts_with("https://") || self.editor_url.starts_with("http://")) && self.editor_url.len() > 8) {
            self.editor_url = EDITOR_URL.into();
        }
        self
    }

    pub fn th(&self) -> bool {
        self.language != "en"
    }
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

fn quote(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

/// One group as a TOML file a person can edit.
pub fn group_toml(name: &str, nodes: &[Node]) -> String {
    let mut out = format!(
        "# Perms group \"{name}\". Edit, then run /lp sync.\n\
         # A node is \"key\", \"-key\" (denied) or\n\
         # {{ key = \"…\", value = true, expiry = <unix seconds, 0 = never>, context = {{ world = \"the_nether\" }} }}\n\
         # Special keys: group.<parent>, weight.<n>, prefix.<priority>.<text>, suffix.<priority>.<text>,\n\
         # meta.<key>.<value>, displayname.<name>\n\
         nodes = [\n"
    );
    for n in nodes {
        if n.is_plain() {
            let key = if n.value { n.key.clone() } else { format!("-{}", n.key) };
            out.push_str(&format!("    {},\n", quote(&key)));
        } else {
            let ctx: Vec<String> = n.context.iter().map(|(k, v)| format!("{k} = {}", quote(v))).collect();
            let ctx = if ctx.is_empty() { "{}".to_owned() } else { format!("{{ {} }}", ctx.join(", ")) };
            out.push_str(&format!("    {{ key = {}, value = {}, expiry = {}, context = {ctx} }},\n", quote(&n.key), n.value, n.expiry));
        }
    }
    out.push_str("]\n");
    out
}

#[derive(Deserialize)]
struct GroupFile {
    #[serde(default)]
    nodes: Vec<Node>,
}

#[derive(Serialize, Deserialize, Default)]
struct TracksFile {
    #[serde(default)]
    tracks: BTreeMap<String, Vec<String>>,
}

/// The nodes that can be used, and one line per entry that cannot (position, key, reason).
fn clean_nodes(nodes: Vec<Node>) -> (Vec<Node>, Vec<String>) {
    let mut out: Vec<Node> = Vec::new();
    let mut bad: Vec<String> = Vec::new();
    let mut over = false;
    for (i, n) in nodes.into_iter().enumerate() {
        let shown: String = n.key.chars().filter(|c| !c.is_control()).take(60).collect();
        match n.cleaned() {
            Ok(_) if out.len() >= MAX_NODES => {
                if !over {
                    bad.push(format!("entry {}: more than {MAX_NODES} nodes, the rest is ignored", i + 1));
                    over = true;
                }
            }
            Ok(n) => {
                if !out.contains(&n) {
                    out.push(n);
                }
            }
            Err(why) => bad.push(format!("entry {} \"{shown}\": {why}", i + 1)),
        }
    }
    (out, bad)
}

/// A group file: its usable nodes and the entries that were refused. Err = the file is not TOML.
pub fn parse_group(text: &str) -> Result<(Vec<Node>, Vec<String>), String> {
    toml::from_str::<GroupFile>(text).map(|f| clean_nodes(f.nodes)).map_err(|e| e.to_string().lines().next().unwrap_or("").to_owned())
}

fn clean_tracks(raw: BTreeMap<String, Vec<String>>) -> (BTreeMap<String, Vec<String>>, Vec<String>) {
    let mut bad = Vec::new();
    let mut tracks = BTreeMap::new();
    for (name, groups) in raw {
        let shown: String = name.chars().filter(|c| !c.is_control()).take(40).collect();
        if !valid_name(&name) {
            bad.push(format!("track \"{shown}\": bad name"));
            continue;
        }
        let mut out: Vec<String> = Vec::new();
        for g in groups {
            if !valid_name(&g) {
                bad.push(format!("track \"{shown}\": group \"{}\": bad name", g.chars().filter(|c| !c.is_control()).take(40).collect::<String>()));
            } else if !out.contains(&g) && out.len() < 64 {
                out.push(g);
            }
        }
        tracks.insert(name, out);
    }
    (tracks, bad)
}

fn stems(dir: &Path, ext: &str) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<String> = rd
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension().and_then(|x| x.to_str()) == Some(ext)).then(|| p.file_stem().and_then(|s| s.to_str()).map(str::to_owned)).flatten()
        })
        .collect();
    out.sort();
    out
}

/// What `load` found on disk.
pub struct Loaded {
    pub data: Data,
    /// Files that could not be read at all (skipped whole).
    pub bad_files: Vec<String>,
    /// Entries inside readable files that were refused: `groups/vip.toml: entry 3 "…": reason`.
    pub bad_entries: Vec<String>,
}

/// Read everything from disk. Nothing is dropped silently: see `Loaded`.
pub fn load(dir: &Path, default_group: &str) -> Loaded {
    let mut d = Data { default_group: default_group.to_owned(), next_expiry: u64::MAX, ..Default::default() };
    let mut bad = Vec::new();
    let mut entries: Vec<String> = Vec::new();
    for name in stems(&dir.join("groups"), "toml") {
        if !valid_name(&name) {
            bad.push(format!("groups/{name}.toml: bad group name"));
            continue;
        }
        match std::fs::read_to_string(dir.join("groups").join(format!("{name}.toml"))).map_err(|e| e.to_string()).and_then(|t| parse_group(&t)) {
            Ok((nodes, refused)) => {
                entries.extend(refused.into_iter().map(|r| format!("groups/{name}.toml: {r}")));
                d.groups.insert(name, nodes);
            }
            Err(e) => bad.push(format!("groups/{name}.toml: {e}")),
        }
    }
    for stem in stems(&dir.join("users"), "json") {
        let Some(id) = parse_uuid(&stem) else {
            bad.push(format!("users/{stem}.json: the file name is not a UUID"));
            continue;
        };
        let text = std::fs::read_to_string(dir.join("users").join(format!("{stem}.json"))).unwrap_or_default();
        match serde_json::from_str::<User>(&text) {
            Ok(u) => {
                let (nodes, refused) = clean_nodes(u.nodes);
                entries.extend(refused.into_iter().map(|r| format!("users/{stem}.json: {r}")));
                if !nodes.is_empty() {
                    d.users.insert(id, User { name: u.name.chars().take(32).collect(), nodes });
                }
            }
            Err(e) => bad.push(format!("users/{stem}.json: {e}")),
        }
    }
    if let Ok(text) = std::fs::read_to_string(dir.join("tracks.toml")) {
        match toml::from_str::<TracksFile>(&text) {
            Ok(f) => {
                let (tracks, refused) = clean_tracks(f.tracks);
                entries.extend(refused.into_iter().map(|r| format!("tracks.toml: {r}")));
                d.tracks = tracks;
            }
            Err(e) => bad.push(format!("tracks.toml: {}", e.to_string().lines().next().unwrap_or(""))),
        }
    }
    if let Ok(text) = std::fs::read_to_string(dir.join("names.json")) {
        match serde_json::from_str::<BTreeMap<String, NameEntry>>(&text) {
            Ok(names) => d.names = names.into_iter().filter(|(k, e)| *k == e.name.to_lowercase() && parse_uuid(&e.uuid).is_some()).collect(),
            Err(e) => bad.push(format!("names.json: {e}")),
        }
    }
    // Users edited while offline are found by the name in their own file too
    let missing: Vec<(String, String)> =
        d.users.iter().filter(|(_, u)| !u.name.is_empty() && !d.names.contains_key(&u.name.to_lowercase())).map(|(id, u)| (id.clone(), u.name.clone())).collect();
    for (id, name) in missing {
        d.names.insert(name.to_lowercase(), NameEntry { uuid: id, name });
    }
    d.ensure_default();
    d.refresh_expiry();
    Loaded { data: d, bad_files: bad, bad_entries: entries }
}

/// Write what changed. `names` also writes the name index (it grows with every new player, so
/// the caller does that on a timer rather than on every join). Returns the number of failures.
pub fn flush(d: &mut Data, dir: &Path, names: bool) -> usize {
    let mut failed = 0;
    let mut check = |r: io::Result<()>| {
        if r.is_err() {
            failed += 1;
        }
    };
    for name in std::mem::take(&mut d.dirty_groups) {
        let path = dir.join("groups").join(format!("{name}.toml"));
        match d.groups.get(&name) {
            Some(nodes) => check(write_atomic(&path, group_toml(&name, nodes).as_bytes())),
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    for id in std::mem::take(&mut d.dirty_users) {
        let path = dir.join("users").join(format!("{id}.json"));
        match d.users.get(&id) {
            Some(u) => check(serde_json::to_vec_pretty(u).map_err(io::Error::other).and_then(|b| write_atomic(&path, &b))),
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    if std::mem::take(&mut d.dirty_tracks) {
        let text = toml::to_string_pretty(&TracksFile { tracks: d.tracks.clone() }).unwrap_or_default();
        check(write_atomic(&dir.join("tracks.toml"), format!("# Perms tracks: name = [groups from lowest to highest]. Edit, then run /lp sync.\n{text}").as_bytes()));
    }
    if names && std::mem::take(&mut d.dirty_names) {
        check(serde_json::to_vec(&d.names).map_err(io::Error::other).and_then(|b| write_atomic(&dir.join("names.json"), &b)));
    }
    failed
}

// ---- action log ----

/// One file of the action log may grow to this; then it becomes `log.1.jsonl` (the one before
/// that is dropped) and a new file starts. Two files = at most twice this on disk.
pub const LOG_MAX_BYTES: u64 = 256 * 1024;

/// Who changed what, when. One JSON object per line in `log.jsonl`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub t: u64,
    pub actor: String,
    /// UUID of the acting player; empty for the console.
    #[serde(default)]
    pub actor_id: String,
    /// `U` user, `G` group, `T` track, `S` everything else.
    pub kind: String,
    pub target: String,
    pub action: String,
}

pub fn append_log(dir: &Path, entry: &LogEntry) -> io::Result<()> {
    use std::io::Write;
    let path = dir.join("log.jsonl");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() >= LOG_MAX_BYTES) {
        std::fs::rename(&path, dir.join("log.1.jsonl"))?;
    }
    let mut line = serde_json::to_string(entry).map_err(io::Error::other)?;
    line.push('\n');
    std::fs::create_dir_all(dir)?;
    std::fs::OpenOptions::new().create(true).append(true).open(&path)?.write_all(line.as_bytes())
}

/// The entries the test picks, newest first. Reads both files (at most `2 * LOG_MAX_BYTES`);
/// a line that is not an entry (a torn write, a hand edit) is skipped.
pub fn read_log(dir: &Path, pick: impl Fn(&LogEntry) -> bool) -> Vec<LogEntry> {
    let mut out = Vec::new();
    for file in ["log.1.jsonl", "log.jsonl"] {
        let Ok(text) = std::fs::read_to_string(dir.join(file)) else { continue };
        out.extend(text.lines().filter_map(|l| serde_json::from_str::<LogEntry>(l).ok()).filter(|e| pick(e)));
    }
    out.reverse();
    out
}

// ---- export / import ----

#[derive(Serialize, Deserialize)]
struct Dump {
    format: String,
    version: u32,
    #[serde(default)]
    exported_at: u64,
    #[serde(default)]
    groups: BTreeMap<String, Vec<Node>>,
    #[serde(default)]
    tracks: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    users: BTreeMap<String, User>,
}

/// `name` → `name.json` when it is a plain word; anything that could leave the folder is refused.
pub fn safe_file(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".json").unwrap_or(name);
    let ok = !stem.is_empty() && stem.len() <= 40 && stem.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    ok.then(|| format!("{stem}.json"))
}

pub fn export(d: &Data, now: u64) -> String {
    let dump = Dump { format: "perms".into(), version: 1, exported_at: now, groups: d.groups.clone(), tracks: d.tracks.clone(), users: d.users.clone() };
    serde_json::to_string_pretty(&dump).unwrap_or_default()
}

#[derive(Debug, PartialEq, Eq)]
pub struct Imported {
    pub groups: usize,
    pub users: usize,
    pub tracks: usize,
    /// Entries in the file that were refused.
    pub refused: usize,
}

/// Put an export into the data. Holders in the file replace the ones here; with `replace`
/// everything that is not in the file goes away first.
pub fn import(d: &mut Data, text: &str, replace: bool) -> Result<Imported, String> {
    let dump: Dump = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if dump.format != "perms" {
        return Err("not a Perms export".into());
    }
    if replace {
        for g in d.groups.keys() {
            d.dirty_groups.insert(g.clone());
        }
        for u in d.users.keys() {
            d.dirty_users.insert(u.clone());
        }
        d.groups.clear();
        d.users.clear();
        d.tracks.clear();
        d.dirty_tracks = true;
    }
    let mut done = Imported { groups: 0, users: 0, tracks: 0, refused: 0 };
    for (name, nodes) in dump.groups {
        if valid_name(&name) {
            let (nodes, refused) = clean_nodes(nodes);
            done.refused += refused.len();
            d.groups.insert(name.clone(), nodes);
            d.dirty_groups.insert(name);
            done.groups += 1;
        } else {
            done.refused += 1;
        }
    }
    for (id, user) in dump.users {
        let Some(id) = parse_uuid(&id) else {
            done.refused += 1;
            continue;
        };
        let (nodes, refused) = clean_nodes(user.nodes);
        done.refused += refused.len();
        let name: String = user.name.chars().filter(|c| !c.is_control()).take(32).collect();
        d.dirty_users.insert(id.clone());
        if nodes.is_empty() {
            d.users.remove(&id);
            continue;
        }
        if !name.is_empty() && !d.names.contains_key(&name.to_lowercase()) {
            d.names.insert(name.to_lowercase(), NameEntry { uuid: id.clone(), name: name.clone() });
            d.dirty_names = true;
        }
        d.users.insert(id, User { name, nodes });
        done.users += 1;
    }
    let (tracks, refused) = clean_tracks(dump.tracks);
    done.refused += refused.len();
    for (name, groups) in tracks {
        d.tracks.insert(name, groups);
        d.dirty_tracks = true;
        done.tracks += 1;
    }
    d.ensure_default();
    d.refresh_expiry();
    Ok(done)
}

/// Largest editor session sent to the web editor. Groups and tracks always go; users and the
/// names of players who joined go as far as this allows (the editor says how many were left out).
pub const MAX_SESSION_BYTES: usize = 400 * 1024;
/// Known permission nodes sent along for the editor's suggestions.
const MAX_SESSION_KEYS: usize = 3000;

/// What `/lp editor` sends to the web editor: an export (`format: "perms"`) plus what helps
/// editing. `known` = permission nodes seen on this server (suggestions only).
///
/// ```json
/// {"format":"perms","version":1,"exported_at":…,"plugin":"0.6.2","default_group":"default",
///  "groups":{"vip":[…nodes]},"tracks":{"staff":["helper","mod"]},
///  "users":{"<uuid>":{"name":"Alice","nodes":[…]}},
///  "names":[{"uuid":"<uuid>","name":"Bob"}],
///  "permissions":["basics:command.fly",…],"left_out":{"users":0,"names":0}}
/// ```
/// Returns the text and how many users did not fit.
pub fn editor_session(d: &Data, now: u64, plugin: &str, known: &[String]) -> (String, usize) {
    use serde_json::{Map, Value, json};
    let size = |v: &Value| serde_json::to_string(v).map_or(0, |t| t.len());
    let groups = serde_json::to_value(&d.groups).unwrap_or_default();
    let tracks = serde_json::to_value(&d.tracks).unwrap_or_default();
    let mut keys: BTreeSet<&str> = d.groups.values().flatten().chain(d.users.values().flat_map(|u| u.nodes.iter())).map(|n| n.key.as_str()).filter(|k| crate::node::kind(k) == crate::node::Kind::Perm).collect();
    keys.extend(known.iter().map(String::as_str).filter(|k| k.len() <= crate::node::MAX_KEY));
    let permissions: Vec<&str> = keys.into_iter().take(MAX_SESSION_KEYS).collect();
    let mut used = size(&groups) + size(&tracks) + permissions.iter().map(|k| k.len() + 3).sum::<usize>() + 256;
    // Users by name, so that what is left out of a very large server is the same every time
    let mut by_name: Vec<(&String, &User)> = d.users.iter().collect();
    by_name.sort_by(|a, b| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase()).then(a.0.cmp(b.0)));
    let mut users = Map::new();
    for (id, user) in &by_name {
        let v = serde_json::to_value(user).unwrap_or_default();
        let cost = size(&v) + id.len() + 4;
        if used + cost > MAX_SESSION_BYTES {
            break;
        }
        used += cost;
        users.insert((*id).clone(), v);
    }
    // Players without anything of their own, so that the editor can give them something
    let mut names: Vec<Value> = Vec::new();
    let mut left_names = 0usize;
    for entry in d.names.values() {
        if users.contains_key(&entry.uuid) {
            continue;
        }
        let cost = entry.uuid.len() + entry.name.len() + 24;
        if used + cost > MAX_SESSION_BYTES {
            left_names += 1;
            continue;
        }
        used += cost;
        names.push(json!({ "uuid": entry.uuid, "name": entry.name }));
    }
    let session = json!({
        "format": "perms", "version": 1, "exported_at": now, "plugin": plugin, "default_group": d.default_group,
        "groups": groups, "tracks": tracks, "users": users, "names": names, "permissions": permissions,
        "left_out": { "users": by_name.len() - users.len(), "names": left_names },
    });
    (session.to_string(), by_name.len() - users.len())
}

/// The changes made in the web editor (`format: "perms-edits"`): holders as they are to be
/// from now on, and the names of what was deleted.
#[derive(Deserialize)]
struct Edits {
    format: String,
    #[serde(default)]
    groups: BTreeMap<String, Vec<Node>>,
    #[serde(default)]
    users: BTreeMap<String, User>,
    #[serde(default)]
    tracks: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    deleted_groups: Vec<String>,
    #[serde(default)]
    deleted_users: Vec<String>,
    #[serde(default)]
    deleted_tracks: Vec<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub groups: usize,
    pub users: usize,
    pub tracks: usize,
    pub deleted: usize,
    /// Entries in the file that were refused.
    pub refused: usize,
}

/// Put what was changed in the web editor into the data. Only the holders named in the file
/// are touched: each becomes what the file says (a user left with no nodes goes away), the
/// deleted ones go, and everything else stays as it is now (a rank given in game while the
/// editor was open is not lost). The default group cannot be deleted.
pub fn apply_edits(d: &mut Data, text: &str) -> Result<Applied, String> {
    let edits: Edits = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if edits.format != "perms-edits" {
        return Err("not a change set from the Perms web editor".into());
    }
    if d.groups.len() + edits.groups.len() > crate::node::MAX_GROUPS * 2 || edits.tracks.len() > crate::node::MAX_TRACKS {
        return Err("too many groups or tracks".into());
    }
    let mut done = Applied::default();
    let listed: Vec<String> = edits.groups.keys().cloned().collect();
    for id in &edits.deleted_users {
        match parse_uuid(id) {
            Some(id) if !edits.users.contains_key(&id) => {
                if d.users.remove(&id).is_some() {
                    d.dirty_users.insert(id);
                    done.deleted += 1;
                }
            }
            Some(_) => {}
            None => done.refused += 1,
        }
    }
    for name in &edits.deleted_tracks {
        if !edits.tracks.contains_key(name) && d.tracks.remove(name).is_some() {
            d.dirty_tracks = true;
            done.deleted += 1;
        }
    }
    for (name, nodes) in edits.groups {
        if !valid_name(&name) || (!d.groups.contains_key(&name) && d.groups.len() >= crate::node::MAX_GROUPS) {
            done.refused += 1;
            continue;
        }
        let (nodes, refused) = clean_nodes(nodes);
        done.refused += refused.len();
        d.groups.insert(name.clone(), nodes);
        d.dirty_groups.insert(name);
        done.groups += 1;
    }
    for (id, user) in edits.users {
        let Some(id) = parse_uuid(&id) else {
            done.refused += 1;
            continue;
        };
        let (nodes, refused) = clean_nodes(user.nodes);
        done.refused += refused.len();
        d.dirty_users.insert(id.clone());
        done.users += 1;
        if nodes.is_empty() {
            d.users.remove(&id);
            continue;
        }
        // The name the server knows wins over the one in the file
        let known = d.names.values().find(|n| n.uuid == id).map(|n| n.name.clone());
        let name = known.unwrap_or_else(|| user.name.chars().filter(|c| !c.is_control()).take(32).collect());
        d.users.insert(id, User { name, nodes });
    }
    let (tracks, refused) = clean_tracks(edits.tracks);
    done.refused += refused.len();
    for (name, groups) in tracks {
        d.tracks.insert(name, groups);
        d.dirty_tracks = true;
        done.tracks += 1;
    }
    // Groups go last: deleting one also takes it out of every holder and track, including the
    // ones this file has just set (a parent that points at a deleted group never stays behind)
    for name in &edits.deleted_groups {
        // A group that is deleted and also listed is being replaced, not deleted
        if listed.contains(name) {
            continue;
        }
        if d.delete_group(name) {
            done.deleted += 1;
        } else if d.groups.contains_key(name) {
            done.refused += 1;
        }
    }
    d.ensure_default();
    d.refresh_expiry();
    Ok(done)
}

/// A code of the web editor (`/lp applyedits <code>`): letters and digits only, so it can
/// never be anything else in a URL.
pub fn valid_code(code: &str) -> bool {
    (6..=32).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Target;
    use crate::node::{Ctx, parse_context};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    const U: &str = "00000000-0000-0000-0000-000000000001";
    static N: AtomicU32 = AtomicU32::new(0);

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("perms-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn sample() -> Data {
        let mut d = Data::new("default");
        d.create_group("vip");
        let vip = Target::Group("vip".into());
        d.set_node(&vip, Node::new("group.default", true));
        d.set_node(&vip, Node::new("weight.10", true));
        d.set_node(&vip, Node::new("prefix.10.&6[VIP] \"q\" ", true));
        d.set_node(&vip, Node::new("basics:command.god", false));
        d.set_node(&vip, Node::with("basics:command.fly", true, 4_000_000_000, parse_context(&["world=nether".to_owned()]).unwrap()));
        d.note_name(U, "Alice");
        d.set_node(&Target::User(U.into()), Node::new("group.vip", true));
        d.tracks.insert("donor".into(), vec!["default".into(), "vip".into()]);
        d.dirty_tracks = true;
        d
    }

    #[test]
    fn config_defaults_and_cleaning() {
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c, Config::default());
        assert!(c.th() && c.tab_list_prefix && c.export_custom_data);
        assert_eq!(c.wildcard_exempt, vec!["tablist:group."]);
        let c: Config = toml::from_str("language = \"en\"\ndefault-group = \"Bad Name\"\nverbose-max-per-second = 0\nwildcard-exempt = [\"\", \"x:\"]").unwrap();
        let c = c.cleaned();
        assert!(!c.th());
        assert_eq!(c.default_group, "default");
        assert_eq!(c.verbose_max_per_second, 1);
        assert_eq!(c.wildcard_exempt, vec!["x:"]);
        assert!(toml::to_string_pretty(&Config::default()).unwrap().contains("tab-list-prefix = true"));
        // the web editor: the owner's address wins, anything else falls back to the one this build was given
        assert_eq!(Config::default().editor_url, EDITOR_URL);
        let own: Config = toml::from_str("editor-url = \" https://editor.example.org/ \"").unwrap();
        assert_eq!(own.cleaned().editor_url, "https://editor.example.org");
        for bad in ["", "ftp://x.example", "https://", "https://a b.example"] {
            let c = Config { editor_url: bad.into(), ..Config::default() }.cleaned();
            assert_eq!(c.editor_url, EDITOR_URL, "{bad}");
        }
        assert!(EDITOR_URL.is_empty() || EDITOR_URL.starts_with("https://"));
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tmp();
        let mut d = sample();
        assert_eq!(flush(&mut d, &dir, true), 0);
        assert!(d.dirty_groups.is_empty() && d.dirty_users.is_empty() && !d.dirty_tracks && !d.dirty_names);
        let text = std::fs::read_to_string(dir.join("groups/vip.toml")).unwrap();
        assert!(text.contains("\"-basics:command.god\","), "{text}");
        assert!(text.contains("context = { world = \"the_nether\" }"), "{text}");
        assert!(!dir.join("groups/vip.tmp").exists());
        let Loaded { data: back, bad_files, bad_entries } = load(&dir, "default");
        assert!(bad_files.is_empty() && bad_entries.is_empty(), "{bad_files:?} {bad_entries:?}");
        assert_eq!(back.groups, d.groups);
        assert_eq!(back.users, d.users);
        assert_eq!(back.tracks, d.tracks);
        assert_eq!(back.names, d.names);
        assert_eq!(back.next_expiry, 4_000_000_000);
        assert_eq!(back.resolve(U, &Ctx::offline(), 0).prefix, "&6[VIP] \"q\" ");

        // a user left with nothing loses the file; a deleted group too
        d.remove_where(&Target::User(U.into()), |_| true);
        d.delete_group("vip");
        flush(&mut d, &dir, false);
        assert!(!dir.join(format!("users/{U}.json")).exists());
        assert!(!dir.join("groups/vip.toml").exists());
        assert!(dir.join("groups/default.toml").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hand_edited_files() {
        let (nodes, refused) = parse_group(
            r#"
            nodes = [
              "Group.Default",
              "-Basics:Command.God",
              "has space",
              { key = "a.b", context = { world = "minecraft:the_nether", server = "x" } },
              { key = "c.d", value = false, expiry = 99 },
              "a.b",
              "a.b",
              "r=(broken",
              { key = "e.f", context = { "Bad Key" = "x" } },
              "kit.(a|b)",
            ]"#,
        )
        .unwrap();
        assert_eq!(nodes.len(), 6);
        assert_eq!(nodes[0], Node::new("group.default", true));
        assert_eq!(nodes[1], Node::new("basics:command.god", false));
        assert_eq!(nodes[2].context.len(), 1);
        assert_eq!(nodes[2].context["world"], "the_nether");
        assert_eq!((nodes[3].value, nodes[3].expiry), (false, 99));
        assert_eq!(nodes[5], Node::new("kit.(a|b)", true));
        // every refused entry is named with its position and the reason
        assert_eq!(refused, vec![
            "entry 3 \"has space\": bad character in key",
            "entry 8 \"r=(broken\": bad regex",
            "entry 9 \"e.f\": bad context",
        ]);
        assert!(parse_group("nodes = [").is_err());
        assert!(parse_group("").unwrap().0.is_empty());

        let dir = tmp();
        std::fs::create_dir_all(dir.join("groups")).unwrap();
        std::fs::create_dir_all(dir.join("users")).unwrap();
        std::fs::write(dir.join("groups/mod.toml"), "nodes = [\"x.y\", \"not valid\"]").unwrap();
        std::fs::write(dir.join("groups/Bad Name.toml"), "nodes = []").unwrap();
        std::fs::write(dir.join("groups/broken.toml"), "nodes = [").unwrap();
        std::fs::write(dir.join(format!("users/{U}.json")), r#"{"name":"Bob","nodes":["group.mod","-also bad"]}"#).unwrap();
        std::fs::write(dir.join("users/not-a-uuid.json"), "{}").unwrap();
        std::fs::write(dir.join("tracks.toml"), "[tracks]\nstaff = [\"mod\", \"mod\", \"Bad Name\"]\n\"Bad Track\" = []").unwrap();
        let Loaded { data: d, bad_files, bad_entries } = load(&dir, "default");
        assert_eq!(bad_files.len(), 3, "{bad_files:?}");
        assert_eq!(bad_entries, vec![
            "groups/mod.toml: entry 2 \"not valid\": bad character in key".to_owned(),
            format!("users/{U}.json: entry 2 \"also bad\": bad character in key"),
            "tracks.toml: track \"Bad Track\": bad name".to_owned(),
            "tracks.toml: track \"staff\": group \"Bad Name\": bad name".to_owned(),
        ]);
        assert!(d.groups.contains_key("mod") && d.groups.contains_key("default"));
        assert_eq!(d.tracks["staff"], vec!["mod"]);
        assert_eq!(d.tracks.len(), 1);
        assert_eq!(d.lookup_user("bob").as_deref(), Some(U), "the name in a user file works before the player joins again");
        assert_eq!(d.resolve(U, &Ctx::offline(), 0).check("x.y", &[]), Some(true));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn action_log() {
        let dir = tmp();
        let entry = |t: u64, actor: &str, target: &str| LogEntry { t, actor: actor.into(), actor_id: String::new(), kind: "U".into(), target: target.into(), action: "permission set \"a b\" true".into() };
        assert!(read_log(&dir, |_| true).is_empty());
        for t in 0..5 {
            append_log(&dir, &entry(t, if t % 2 == 0 { "Alice" } else { "console" }, "Bob")).unwrap();
        }
        let all = read_log(&dir, |_| true);
        assert_eq!(all.iter().map(|e| e.t).collect::<Vec<_>>(), vec![4, 3, 2, 1, 0], "newest first");
        assert_eq!(read_log(&dir, |e| e.actor == "Alice").len(), 3);
        assert_eq!(all[0], entry(4, "Alice", "Bob"));
        // a broken line does not hide the rest
        std::fs::write(dir.join("log.jsonl"), format!("{}\nnot json\n", serde_json::to_string(&entry(9, "A", "B")).unwrap())).unwrap();
        assert_eq!(read_log(&dir, |_| true).len(), 1);
        // rotation: the file never grows past the cap by more than one line, and one old file is kept
        let _ = std::fs::remove_file(dir.join("log.jsonl"));
        let line = serde_json::to_string(&entry(0, "Alice", "Bob")).unwrap().len() as u64 + 1;
        let per_file = LOG_MAX_BYTES / line + 1;
        for t in 0..per_file * 2 + 10 {
            append_log(&dir, &entry(t, "Alice", "Bob")).unwrap();
        }
        assert!(std::fs::metadata(dir.join("log.jsonl")).unwrap().len() <= LOG_MAX_BYTES + line);
        assert!(std::fs::metadata(dir.join("log.1.jsonl")).unwrap().len() <= LOG_MAX_BYTES + 2 * line);
        let all = read_log(&dir, |_| true);
        assert_eq!(all[0].t, per_file * 2 + 9);
        assert!(all.len() as u64 > per_file && (all.len() as u64) < per_file * 2 + 10, "the oldest file is gone: {}", all.len());
        assert!(all.windows(2).all(|w| w[0].t > w[1].t), "still newest first across both files");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn file_names() {
        assert_eq!(safe_file("backup").as_deref(), Some("backup.json"));
        assert_eq!(safe_file("backup-2026_11.json").as_deref(), Some("backup-2026_11.json"));
        for bad in ["", "../x", "a/b", "a\\b", ".hidden", "a.b.json", "x y", &"a".repeat(41), "names.toml"] {
            assert_eq!(safe_file(bad), None, "{bad}");
        }
    }

    #[test]
    fn export_then_import() {
        let d = sample();
        let text = export(&d, 123);
        let mut clean = Data::new("default");
        clean.create_group("old");
        let done = import(&mut clean, &text, false).unwrap();
        assert_eq!(done, Imported { groups: 2, users: 1, tracks: 1, refused: 0 });
        assert_eq!(clean.groups["vip"], d.groups["vip"]);
        assert!(clean.groups.contains_key("old"), "a merge keeps what the file does not name");
        assert_eq!(clean.lookup_user("alice").as_deref(), Some(U));
        assert_eq!(clean.resolve(U, &Ctx::offline(), 0).primary, "vip");
        assert_eq!(clean.next_expiry, 4_000_000_000);

        let done = import(&mut clean, &text, true).unwrap();
        assert_eq!(done.groups, 2);
        assert!(!clean.groups.contains_key("old"));
        assert!(clean.dirty_groups.contains("old"), "so its file gets removed");

        assert!(import(&mut clean, "{}", false).is_err());
        assert!(import(&mut clean, r#"{"format":"other","version":1}"#, false).is_err());
        // hostile content is cleaned, not trusted
        let odd = r#"{"format":"perms","version":1,"groups":{"../evil":[],"ok":["bad key","fine"]},"users":{"zz":{"name":"x","nodes":["a"]}},"tracks":{"t":["ok","../x"]}}"#;
        let mut d2 = Data::new("default");
        let done = import(&mut d2, odd, true).unwrap();
        assert_eq!(done, Imported { groups: 1, users: 0, tracks: 1, refused: 4 });
        assert_eq!(d2.groups["ok"], vec![Node::new("fine", true)]);
        assert_eq!(d2.tracks["t"], vec!["ok"]);
        assert!(d2.groups.contains_key("default"), "the default group survives a replace");
    }

    #[test]
    fn the_editor_gets_everything_it_needs() {
        let mut d = Data::new("default");
        d.create_group("vip");
        d.set_node(&Target::Group("vip".into()), Node::new("basics:command.fly", true));
        d.set_node(&Target::Group("vip".into()), Node::new("weight.10", true));
        d.tracks.insert("staff".into(), vec!["default".into(), "vip".into()]);
        let alice = "11111111-1111-1111-1111-111111111111";
        let bob = "22222222-2222-2222-2222-222222222222";
        d.note_name(alice, "Alice");
        d.note_name(bob, "Bob");
        d.set_node(&Target::User(alice.into()), Node::new("group.vip", true));
        let (text, left) = editor_session(&d, 1_790_000_000, "9.9.9", &["claims:command.trust".into(), "x".repeat(500)]);
        assert_eq!(left, 0);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!((v["format"].as_str(), v["plugin"].as_str(), v["default_group"].as_str()), (Some("perms"), Some("9.9.9"), Some("default")));
        assert_eq!(v["groups"]["vip"].as_array().unwrap().len(), 2);
        assert_eq!(v["tracks"]["staff"][1], "vip");
        assert_eq!(v["users"][alice]["name"], "Alice");
        // Bob has nothing of his own: only his name goes, so that the editor can give him something
        assert!(v["users"].get(bob).is_none());
        assert_eq!(v["names"], serde_json::json!([{ "uuid": bob, "name": "Bob" }]));
        // Suggestions: what is used here and what the server has seen; never a group or meta node
        assert_eq!(v["permissions"], serde_json::json!(["basics:command.fly", "claims:command.trust"]));
        assert_eq!(v["left_out"], serde_json::json!({ "users": 0, "names": 0 }));
        // It is also an export: a fresh server can import it
        let mut fresh = Data::new("default");
        assert_eq!(import(&mut fresh, &text, false).unwrap(), Imported { groups: 2, users: 1, tracks: 1, refused: 0 });
        // A very large server: groups always fit, users go as far as the budget allows
        for i in 0..4000u32 {
            let id = format!("33333333-3333-3333-3333-{i:012}");
            d.note_name(&id, &format!("Player{i}"));
            for n in 0..6 {
                d.set_node(&Target::User(id.clone()), Node::new(&format!("some.long.permission.node.number{n}"), true));
            }
        }
        let (big, left) = editor_session(&d, 0, "9.9.9", &[]);
        assert!(big.len() <= MAX_SESSION_BYTES + 4096, "{}", big.len());
        let v: serde_json::Value = serde_json::from_str(&big).unwrap();
        let sent = v["users"].as_object().unwrap().len() as u64;
        assert!(sent > 500 && sent < 4001, "{sent}");
        assert_eq!(v["left_out"]["users"].as_u64().unwrap() + sent, 4001);
        assert_eq!(left as u64 + sent, 4001);
        assert_eq!(v["groups"]["vip"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn edits_touch_only_what_they_name() {
        let alice = "11111111-1111-1111-1111-111111111111";
        let bob = "22222222-2222-2222-2222-222222222222";
        let carol = "33333333-3333-3333-3333-333333333333";
        let mut d = Data::new("default");
        for g in ["vip", "old", "mod"] {
            d.create_group(g);
        }
        d.set_node(&Target::Group("vip".into()), Node::new("a.b", true));
        d.set_node(&Target::Group("mod".into()), Node::new("m.x", true));
        d.set_node(&Target::Group("mod".into()), Node::new("group.old", true));
        d.note_name(alice, "Alice");
        d.set_node(&Target::User(alice.into()), Node::new("group.old", true));
        d.set_node(&Target::User(bob.into()), Node::new("b.b", true));
        d.set_node(&Target::User(carol.into()), Node::new("c.c", true));
        d.tracks.insert("staff".into(), vec!["vip".into(), "mod".into()]);
        d.tracks.insert("gone".into(), vec!["vip".into()]);
        d.dirty_groups.clear();
        d.dirty_users.clear();
        d.dirty_tracks = false;
        let edits = serde_json::json!({
            "format": "perms-edits", "version": 1,
            "groups": {
                "vip": ["a.b", "-a.c", { "key": "group.default", "value": true, "expiry": 0, "context": {} }, { "key": "t.x", "expiry": 4_000_000_000u64, "context": { "world": "nether" } }, "group.old"],
                "new_one": ["n.n"],
                "Bad Name": ["x"],
            },
            "users": {
                alice: { "name": "Mallory", "nodes": ["group.vip", "has space", "group.old"] },
                bob: { "name": "Bob", "nodes": [] },
                "not-a-uuid": { "name": "x", "nodes": ["a"] },
            },
            "tracks": { "staff": ["default", "old", "vip", "mod"] },
            "deleted_groups": ["old", "default", "never_was"],
            "deleted_users": [carol, "junk"],
            "deleted_tracks": ["gone"],
        });
        let done = apply_edits(&mut d, &edits.to_string()).unwrap();
        // old, carol and the track "gone" are deleted; the default group and junk are refused
        assert_eq!(done, Applied { groups: 2, users: 2, tracks: 1, deleted: 3, refused: 5 });
        let keys = |nodes: &[Node]| nodes.iter().map(|n| format!("{}{}", if n.value { "" } else { "-" }, n.key)).collect::<Vec<_>>();
        // vip came with the deleted group as a parent: that does not stay behind
        assert_eq!(keys(&d.groups["vip"]), ["a.b", "-a.c", "group.default", "t.x"]);
        assert_eq!((d.groups["vip"][3].expiry, d.groups["vip"][3].context.get("world").map(String::as_str)), (4_000_000_000, Some("the_nether")));
        assert_eq!(keys(&d.groups["new_one"]), ["n.n"]);
        assert!(!d.groups.contains_key("old") && d.groups.contains_key("default") && !d.groups.contains_key("Bad Name"));
        // mod was not in the file: it stays, minus the parent that no longer exists
        assert_eq!(keys(&d.groups["mod"]), ["m.x"]);
        // The name the server knows wins; a user left with nothing goes away
        assert_eq!((d.users[alice].name.as_str(), keys(&d.users[alice].nodes)), ("Alice", vec!["group.vip".to_owned()]));
        assert!(!d.users.contains_key(bob) && !d.users.contains_key(carol));
        assert_eq!(d.tracks.get("staff").unwrap(), &["default", "vip", "mod"]);
        assert!(!d.tracks.contains_key("gone"));
        assert!(d.dirty_tracks && d.dirty_groups.contains("vip") && d.dirty_groups.contains("old") && d.dirty_users.contains(bob) && d.dirty_users.contains(carol));
        assert_eq!(d.next_expiry, 4_000_000_000);
        // An empty change set changes nothing; other files are refused
        let before = (d.groups.clone(), d.users.clone(), d.tracks.clone());
        assert_eq!(apply_edits(&mut d, r#"{"format":"perms-edits"}"#).unwrap(), Applied::default());
        assert_eq!(before, (d.groups.clone(), d.users.clone(), d.tracks.clone()));
        let an_export = export(&d, 0);
        assert!(apply_edits(&mut d, &an_export).is_err());
        assert!(apply_edits(&mut d, "<html>").is_err());
        assert!(apply_edits(&mut d, r#"{"format":"perms-edits","groups":{"vip":[{"value":true}]}}"#).is_err());
        assert_eq!(before, (d.groups.clone(), d.users.clone(), d.tracks.clone()));
        // Codes
        assert!(valid_code("Ab3dEf7hJk") && valid_code("abcdef"));
        for bad in ["", "abc", "ab/cd/ef", "..%2f..%2f", "abc def", &"a".repeat(33), "กขคงจฉช"] {
            assert!(!valid_code(bad), "{bad}");
        }
    }
}

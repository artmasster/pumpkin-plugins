//! What players chose and the skins already fetched (no server needed).
//!
//! ```text
//! plugins/data/skins/
//!   players.json   {"alice": {"kind":"name","skin":"notch"}, "bob": {"kind":"url","url":"https://…","slim":true}}
//!   cache.json     {"name:notch": {"value":"…","signature":"…","at":1790000000}, "name:nobody": {"missing":true,"at":…}}
//! ```
use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Skins kept in the cache; the oldest go first.
pub const MAX_CACHE: usize = 4000;
/// A name that is not an account is asked about again after this long.
pub const MISSING_SECS: u64 = 6 * 3600;

/// What a player wants to look like.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Choice {
    /// The skin of the Mojang account with this (lowercase) name.
    Name { skin: String },
    /// A skin made from an image link.
    Url {
        url: String,
        #[serde(default)]
        slim: bool,
    },
}

impl Choice {
    /// Where its textures are in the cache.
    pub fn key(&self) -> String {
        match self {
            Choice::Name { skin } => format!("name:{}", skin.to_ascii_lowercase()),
            Choice::Url { url, slim } => format!("url:{}|{}", if *slim { "slim" } else { "classic" }, url),
        }
    }

    /// For messages: the account name, or the link.
    pub fn label(&self) -> &str {
        match self {
            Choice::Name { skin } => skin,
            Choice::Url { url, .. } => url,
        }
    }
}

/// Signed textures of a skin, or the note that there is no such account.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cached {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub signature: String,
    /// The name is not a Mojang account.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub missing: bool,
    /// Unix seconds when it was fetched.
    pub at: u64,
}

#[derive(Default)]
pub struct Store {
    /// Lowercase player name → choice.
    pub players: BTreeMap<String, Choice>,
    pub cache: BTreeMap<String, Cached>,
    pub players_dirty: bool,
    pub cache_dirty: bool,
}

/// What to do about a skin right now.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup<'a> {
    /// Use these textures.
    Have(&'a Cached),
    /// Known not to exist (asked recently).
    Missing,
    /// Not known, or too old: fetch it. `stale` = old textures to wear meanwhile.
    Fetch { stale: Option<&'a Cached> },
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

impl Store {
    /// A file that cannot be read starts empty: skins are fetched again, choices are lost
    /// (and the broken file is kept beside the new one).
    pub fn open(dir: &Path) -> Store {
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
        let mut store = Store::default();
        if let Some(text) = read("players.json") {
            match serde_json::from_str::<BTreeMap<String, Choice>>(&text) {
                Ok(map) => store.players = map.into_iter().filter(|(k, _)| crate::cfg::valid_name(k)).map(|(k, v)| (k.to_ascii_lowercase(), v)).collect(),
                Err(_) => {
                    let _ = std::fs::rename(dir.join("players.json"), dir.join("players.broken.json"));
                }
            }
        }
        if let Some(text) = read("cache.json") {
            store.cache = serde_json::from_str(&text).unwrap_or_default();
        }
        store
    }

    pub fn flush(&mut self, dir: &Path) -> bool {
        let mut ok = true;
        if self.players_dirty {
            self.players_dirty = false;
            ok &= serde_json::to_vec_pretty(&self.players).is_ok_and(|b| write_atomic(&dir.join("players.json"), &b).is_ok());
        }
        if self.cache_dirty {
            self.cache_dirty = false;
            ok &= serde_json::to_vec(&self.cache).is_ok_and(|b| write_atomic(&dir.join("cache.json"), &b).is_ok());
        }
        ok
    }

    pub fn choice(&self, player: &str) -> Option<&Choice> {
        self.players.get(&player.to_ascii_lowercase())
    }

    pub fn choose(&mut self, player: &str, choice: Option<Choice>) {
        let key = player.to_ascii_lowercase();
        let changed = match choice {
            Some(c) => self.players.insert(key, c.clone()) != Some(c),
            None => self.players.remove(&key).is_some(),
        };
        self.players_dirty |= changed;
    }

    pub fn lookup(&self, key: &str, now: u64, keep_secs: u64) -> Lookup<'_> {
        match self.cache.get(key) {
            Some(c) if c.missing && now.saturating_sub(c.at) < MISSING_SECS => Lookup::Missing,
            Some(c) if !c.missing && !c.value.is_empty() && now.saturating_sub(c.at) < keep_secs => Lookup::Have(c),
            Some(c) if !c.missing && !c.value.is_empty() => Lookup::Fetch { stale: Some(c) },
            _ => Lookup::Fetch { stale: None },
        }
    }

    pub fn put(&mut self, key: &str, entry: Cached) {
        if self.cache.len() >= MAX_CACHE && !self.cache.contains_key(key) {
            // Room for the new one: the tenth that was fetched longest ago goes
            let mut by_age: Vec<(u64, String)> = self.cache.iter().map(|(k, c)| (c.at, k.clone())).collect();
            by_age.sort();
            for (_, k) in by_age.into_iter().take(MAX_CACHE / 10) {
                self.cache.remove(&k);
            }
        }
        self.cache.insert(key.to_owned(), entry);
        self.cache_dirty = true;
    }
}

/// The skin of the list a player without an account gets: always the same one for a name.
pub fn pick_default<'a>(list: &'a [String], player: &str) -> Option<&'a str> {
    if list.is_empty() {
        return None;
    }
    // FNV-1a over the lowercase name
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in player.to_ascii_lowercase().bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    Some(&list[(h % list.len() as u64) as usize])
}

/// The account id in Mojang's answer to a name lookup (`{"id":"069a79f4…","name":"Notch"}`).
pub fn profile_id(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let id = v.get("id")?.as_str()?;
    (id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())).then(|| id.to_ascii_lowercase())
}

/// Signed textures anywhere in an answer: the first object that has a `value` and a `signature`
/// text (Mojang: `properties[0]`; MineSkin: `skin.texture.data` or `data.texture`).
pub fn textures(body: &str) -> Option<(String, String)> {
    fn find(v: &serde_json::Value, depth: u8) -> Option<(String, String)> {
        if depth > 8 {
            return None;
        }
        match v {
            serde_json::Value::Object(map) => {
                if let (Some(value), Some(signature)) = (map.get("value").and_then(|x| x.as_str()), map.get("signature").and_then(|x| x.as_str())) {
                    if looks_base64(value, 60_000) && looks_base64(signature, 2000) {
                        return Some((value.to_owned(), signature.to_owned()));
                    }
                }
                map.values().find_map(|x| find(x, depth + 1))
            }
            serde_json::Value::Array(list) => list.iter().find_map(|x| find(x, depth + 1)),
            _ => None,
        }
    }
    find(&serde_json::from_str(body).ok()?, 0)
}

fn looks_base64(s: &str, max: usize) -> bool {
    s.len() >= 40 && s.len() <= max && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

/// The id of a MineSkin job that is still being made (`{"job":{"id":"…","status":"waiting"}}`).
pub fn job_id(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let id = v.get("job")?.get("id")?.as_str()?;
    (!id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')).then(|| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("skins-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const VALUE: &str = "ewogICJ0aW1lc3RhbXAiIDogMTc5MDAwMDAwMDAwMCwKICAicHJvZmlsZUlkIiA6ICIwNjlhNzlmNDQ0ZTk0NzI2YTViZWZjYTkwZTM4YWFmNSIKfQ==";
    const SIG: &str = "bXkgc2lnbmF0dXJlIGlzIGxvbmcgZW5vdWdoIHRvIGxvb2sgbGlrZSBvbmUgYW5kIHRoZW4gc29tZQ==";

    #[test]
    fn choices_and_cache_survive_a_restart() {
        let dir = temp("restart");
        let mut s = Store::open(&dir);
        s.choose("Alice", Some(Choice::Name { skin: "notch".into() }));
        s.choose("Bob", Some(Choice::Url { url: "https://x.com/a.png".into(), slim: true }));
        s.choose("Carol", Some(Choice::Name { skin: "jeb_".into() }));
        s.choose("carol", None);
        s.put("name:notch", Cached { value: VALUE.into(), signature: SIG.into(), missing: false, at: 100 });
        s.put("name:nobody", Cached { missing: true, at: 100, ..Default::default() });
        assert!(s.flush(&dir));
        assert!(!s.players_dirty && !s.cache_dirty);
        let back = Store::open(&dir);
        assert_eq!(back.choice("ALICE"), Some(&Choice::Name { skin: "notch".into() }));
        assert_eq!(back.choice("bob").unwrap().key(), "url:slim|https://x.com/a.png");
        assert_eq!(back.choice("carol"), None);
        assert_eq!(back.lookup("name:notch", 200, 1000), Lookup::Have(&Cached { value: VALUE.into(), signature: SIG.into(), missing: false, at: 100 }));
        // Too old: fetched again, the old one is worn meanwhile
        assert!(matches!(back.lookup("name:notch", 2000, 1000), Lookup::Fetch { stale: Some(_) }));
        assert_eq!(back.lookup("name:nobody", 200, 1000), Lookup::Missing);
        assert_eq!(back.lookup("name:nobody", 100 + MISSING_SECS, 1000), Lookup::Fetch { stale: None });
        assert_eq!(back.lookup("name:never", 200, 1000), Lookup::Fetch { stale: None });
        // Choosing the same again writes nothing
        let mut again = Store::open(&dir);
        again.choose("alice", Some(Choice::Name { skin: "notch".into() }));
        assert!(!again.players_dirty);
        // A broken file starts over and is kept
        std::fs::write(dir.join("players.json"), "{ nope").unwrap();
        std::fs::write(dir.join("cache.json"), "[]").unwrap();
        let broken = Store::open(&dir);
        assert!(broken.players.is_empty() && broken.cache.is_empty() && dir.join("players.broken.json").exists());
    }

    #[test]
    fn the_cache_is_bounded() {
        let mut s = Store::default();
        for i in 0..MAX_CACHE + 50 {
            s.put(&format!("name:p{i}"), Cached { value: VALUE.into(), signature: SIG.into(), missing: false, at: i as u64 });
        }
        assert!(s.cache.len() <= MAX_CACHE);
        assert!(!s.cache.contains_key("name:p0") && s.cache.contains_key(&format!("name:p{}", MAX_CACHE + 49)));
    }

    #[test]
    fn default_skins_are_stable() {
        let list = vec!["notch".to_owned(), "jeb_".to_owned(), "dinnerbone".to_owned()];
        assert_eq!(pick_default(&[], "Alice"), None);
        let a = pick_default(&list, "Alice").unwrap();
        assert_eq!(pick_default(&list, "ALICE"), Some(a));
        let spread: std::collections::HashSet<&str> = (0..40).map(|i| pick_default(&list, &format!("player{i}")).unwrap()).collect();
        assert_eq!(spread.len(), 3);
    }

    #[test]
    fn answers_are_read() {
        assert_eq!(profile_id(r#"{"id":"069A79F444E94726A5BEFCA90E38AAF5","name":"Notch"}"#).as_deref(), Some("069a79f444e94726a5befca90e38aaf5"));
        for bad in ["", "{}", r#"{"id":"short"}"#, r#"{"id":"069a79f444e94726a5befca90e38aaf5/../x"}"#, "<html>"] {
            assert_eq!(profile_id(bad), None, "{bad}");
        }
        let mojang = format!(r#"{{"id":"x","name":"Notch","properties":[{{"name":"textures","value":"{VALUE}","signature":"{SIG}"}}]}}"#);
        assert_eq!(textures(&mojang), Some((VALUE.into(), SIG.into())));
        let mineskin_v2 = format!(r#"{{"success":true,"skin":{{"uuid":"u","texture":{{"data":{{"value":"{VALUE}","signature":"{SIG}"}},"hash":{{"skin":"h"}}}}}}}}"#);
        assert_eq!(textures(&mineskin_v2), Some((VALUE.into(), SIG.into())));
        let mineskin_v1 = format!(r#"{{"id":1,"data":{{"texture":{{"value":"{VALUE}","signature":"{SIG}","url":"u"}}}}}}"#);
        assert_eq!(textures(&mineskin_v1), Some((VALUE.into(), SIG.into())));
        // Unsigned, or not textures at all
        assert_eq!(textures(&format!(r#"{{"properties":[{{"name":"textures","value":"{VALUE}"}}]}}"#)), None);
        assert_eq!(textures(r#"{"value":"<script>","signature":"x"}"#), None);
        assert_eq!(textures("not json"), None);
        assert_eq!(job_id(r#"{"success":true,"job":{"id":"abc-123","status":"waiting"}}"#).as_deref(), Some("abc-123"));
        assert_eq!(job_id(r#"{"job":{"id":"../x"}}"#), None);
        assert_eq!(job_id("{}"), None);
    }
}

//! The voice bridge's wire format, as the Paper and Endstone plugins speak it: position frames,
//! signed datagrams and the bridge's answers. No server needed here.
//!
//! One UDP datagram is a JSON body, a newline, and the hex HMAC-SHA256 of the body with the
//! server's own key. Every body has `v` (1), `srv` (server id), `ts` (unix ms) and `t`:
//! `pos` (rows to set, `g` players that left), `link` (a code typed with `/voice`), `hello`
//! (every 10 s), `mute`. From the bridge: `linked`, `talk`, `update`.
use std::collections::HashMap;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

pub const NS: &str = "voice";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const KEYFRAME_EVERY: u64 = 25;
pub const MOVE_EPSILON: f64 = 0.25;
pub const YAW_EPSILON: f64 = 8.0;
pub const MAX_ROWS: usize = 40;
pub const MAX_SKEW_MS: i64 = 30_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub bridge_host: String,
    pub bridge_port: u16,
    pub server_id: String,
    pub server_key: String,
    /// Shown in the app once a player links.
    pub server_name: String,
    /// Where players get the app (shown by `/voice`).
    pub app_url: String,
    /// A note over whoever is talking.
    pub talk_indicator: bool,
    /// Silent at and beyond this distance (8 to 128).
    pub voice_range: i64,
    /// Full volume up to this distance, fading after it (0 to 64, at most the range).
    pub full_volume_range: i64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            bridge_host: String::new(),
            bridge_port: 0,
            server_id: String::new(),
            server_key: String::new(),
            server_name: String::new(),
            app_url: "v.mcsv.me".into(),
            talk_indicator: true,
            voice_range: 32,
            full_volume_range: 4,
        }
    }
}

impl Config {
    /// The server's key, when everything needed to talk to the bridge is set.
    pub fn key(&self) -> Option<Vec<u8>> {
        if self.bridge_host.is_empty() || self.bridge_port == 0 || self.server_id.is_empty() {
            return None;
        }
        let hex = self.server_key.trim();
        if hex.is_empty() || hex.len() % 2 != 0 {
            return None;
        }
        (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok()).collect()
    }

    /// (voice range, full volume range) in blocks, within the limits the bridge also keeps.
    pub fn distances(&self) -> (i64, i64) {
        let range = self.voice_range.clamp(8, 128);
        (range, self.full_volume_range.clamp(0, 64).min(range))
    }
}

/// A player's id as the bridge knows them: Bedrock players (a UUID with no high half) by their
/// XUID, Java players by their UUID.
pub fn player_id(high: u64, low: u64) -> String {
    if high == 0 {
        return format!("x:{low}");
    }
    let h = format!("{high:016x}{low:016x}");
    format!("j:{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn round1(n: f64) -> f64 {
    (n * 10.0).round() / 10.0
}

fn yaw_diff(a: f64, b: f64) -> f64 {
    ((a - b + 540.0).rem_euclid(360.0) - 180.0).abs()
}

/// A position row: id, dimension, x, y, z, yaw.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: String,
    pub dim: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f64,
}

impl Row {
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!([self.id, self.dim, self.x, self.y, self.z, self.yaw as i64])
    }
}

/// Sends a row when a player moved a quarter block, turned 8 degrees or changed dimension, and
/// every row again every 25 frames, so the bridge never lets a still player expire.
#[derive(Default)]
pub struct Encoder {
    last: HashMap<String, Row>,
    seq: u64,
}

impl Encoder {
    /// Every online player's row now. Returns (changed rows, ids that left).
    pub fn frame(&mut self, rows: Vec<Row>) -> (Vec<Row>, Vec<String>) {
        self.seq += 1;
        let key = self.seq % KEYFRAME_EVERY == 1;
        let mut changed = Vec::new();
        let mut seen = Vec::new();
        for r in rows {
            let r = Row { x: round1(r.x), y: round1(r.y), z: round1(r.z), yaw: r.yaw.round(), ..r };
            seen.push(r.id.clone());
            let moved = match self.last.get(&r.id) {
                None => true,
                Some(o) => key || o.dim != r.dim || (o.x - r.x).abs() >= MOVE_EPSILON || (o.y - r.y).abs() >= MOVE_EPSILON || (o.z - r.z).abs() >= MOVE_EPSILON || yaw_diff(o.yaw, r.yaw) >= YAW_EPSILON,
            };
            if moved {
                self.last.insert(r.id.clone(), r.clone());
                changed.push(r);
            }
        }
        let gone: Vec<String> = self.last.keys().filter(|id| !seen.contains(id)).cloned().collect();
        for id in &gone {
            self.last.remove(id);
        }
        (changed, gone)
    }

    /// Everyone sent so far (to say they left when the plugin stops).
    pub fn known(&self) -> Vec<String> {
        self.last.keys().cloned().collect()
    }
}

pub fn sign(key: &[u8], body: &serde_json::Value) -> Vec<u8> {
    let raw = serde_json::to_vec(body).unwrap_or_default();
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("any key length");
    mac.update(&raw);
    let tag: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let mut out = raw;
    out.push(b'\n');
    out.extend_from_slice(tag.as_bytes());
    out
}

fn head(srv: &str, now_ms: i64, kind: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("v".into(), 1.into());
    m.insert("srv".into(), srv.into());
    m.insert("ts".into(), now_ms.into());
    m.insert("t".into(), kind.into());
    m
}

pub fn pos_datagrams(key: &[u8], srv: &str, now_ms: i64, rows: &[Row], gone: &[String]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let chunks: Vec<&[Row]> = if rows.is_empty() { vec![&[]] } else { rows.chunks(MAX_ROWS).collect() };
    for (i, part) in chunks.into_iter().enumerate() {
        if part.is_empty() && gone.is_empty() {
            break;
        }
        let mut m = head(srv, now_ms, "pos");
        m.insert("p".into(), part.iter().map(Row::json).collect::<Vec<_>>().into());
        m.insert("g".into(), if i == 0 { gone.to_vec().into() } else { Vec::<String>::new().into() });
        out.push(sign(key, &serde_json::Value::Object(m)));
    }
    out
}

pub fn link_datagram(key: &[u8], srv: &str, now_ms: i64, id: &str, name: &str, code: &str, server_name: &str) -> Vec<u8> {
    let mut m = head(srv, now_ms, "link");
    m.insert("id".into(), id.into());
    m.insert("n".into(), name.into());
    m.insert("c".into(), code.into());
    if !server_name.is_empty() {
        m.insert("s".into(), server_name.into());
    }
    sign(key, &serde_json::Value::Object(m))
}

pub fn hello_datagram(key: &[u8], srv: &str, now_ms: i64, range: i64, full: i64) -> Vec<u8> {
    let mut m = head(srv, now_ms, "hello");
    m.insert("p".into(), "pumpkin".into());
    m.insert("ver".into(), VERSION.into());
    m.insert("range".into(), range.into());
    m.insert("full".into(), full.into());
    sign(key, &serde_json::Value::Object(m))
}

pub fn mute_datagram(key: &[u8], srv: &str, now_ms: i64, id: &str, on: bool) -> Vec<u8> {
    let mut m = head(srv, now_ms, "mute");
    m.insert("id".into(), id.into());
    m.insert("on".into(), on.into());
    sign(key, &serde_json::Value::Object(m))
}

/// A fresh datagram of this kind from the bridge, signed with our key and for our server.
pub fn open(key: &[u8], srv: &str, data: &[u8], kind: &str, now_ms: i64) -> Option<serde_json::Value> {
    let at = data.iter().rposition(|b| *b == b'\n')?;
    let (raw, mac_hex) = (&data[..at], &data[at + 1..]);
    if mac_hex.len() != 64 {
        return None;
    }
    let tag: Vec<u8> = (0..32).map(|i| std::str::from_utf8(&mac_hex[i * 2..i * 2 + 2]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())).collect::<Option<_>>()?;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).ok()?;
    mac.update(raw);
    mac.verify_slice(&tag).ok()?;
    let body: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let ok = body.get("v").and_then(serde_json::Value::as_i64) == Some(1)
        && body.get("srv").and_then(serde_json::Value::as_str) == Some(srv)
        && body.get("t").and_then(serde_json::Value::as_str) == Some(kind)
        && body.get("ts").and_then(serde_json::Value::as_i64).is_some_and(|ts| (now_ms - ts).abs() <= MAX_SKEW_MS);
    ok.then_some(body)
}

/// The bridge's answer to a link: (player id, worked, why not).
pub fn open_answer(key: &[u8], srv: &str, data: &[u8], now_ms: i64) -> Option<(String, bool, Option<String>)> {
    let b = open(key, srv, data, "linked", now_ms)?;
    Some((b.get("id")?.as_str()?.to_owned(), b.get("ok")?.as_bool()?, b.get("why").and_then(serde_json::Value::as_str).map(str::to_owned)))
}

/// Who is talking now.
pub fn open_talk(key: &[u8], srv: &str, data: &[u8], now_ms: i64) -> Option<Vec<String>> {
    let b = open(key, srv, data, "talk", now_ms)?;
    let ids = b.get("ids")?.as_array()?;
    let ids: Option<Vec<String>> = ids.iter().take(100).map(|i| i.as_str().map(str::to_owned)).collect();
    ids
}

/// A newer plugin is out: (version, how).
pub fn open_update(key: &[u8], srv: &str, data: &[u8], now_ms: i64) -> Option<(String, String)> {
    let b = open(key, srv, data, "update", now_ms)?;
    let ver = b.get("ver")?.as_str()?;
    let how = b.get("how")?.as_str()?;
    (ver.len() <= 17 && matches!(how, "restart" | "download")).then(|| (ver.to_owned(), how.to_owned()))
}

pub fn refusal_text(why: Option<&str>) -> &'static str {
    match why {
        Some("wait") => "[เสียง] ใส่รหัสผิดหลายครั้ง รอไม่เกิน 10 นาทีแล้วลองใหม่",
        Some("busy") => "[เสียง] ตอนนี้เชื่อมไม่ได้ ลองใหม่อีกครั้งภายหลัง",
        Some("used") => "[เสียง] รหัสนี้ใช้ไปแล้ว ดูรหัสล่าสุดในหน้าคุยหรือแอป แล้วพิมพ์ /voice <รหัส> อีกครั้ง",
        Some("expired") => "[เสียง] รหัสหมดอายุแล้ว ดูรหัสล่าสุดในหน้าคุยหรือแอป แล้วพิมพ์ /voice <รหัส> อีกครั้ง",
        _ => "[เสียง] รหัสไม่ถูกหรือหมดอายุ ดูรหัสล่าสุดในหน้าคุยหรือแอป แล้วพิมพ์ /voice <รหัส> อีกครั้ง",
    }
}

/// `/voice 12345` → `012345`; none if it is not a code of up to 6 digits.
pub fn parse_code(arg: &str) -> Option<String> {
    let s = arg.trim();
    (!s.is_empty() && s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit())).then(|| format!("{s:0>6}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_platform_qualified() {
        assert_eq!(player_id(0, 2535400000000101), "x:2535400000000101");
        assert_eq!(player_id(0x0123456789abcdef, 0xfedcba9876543210), "j:01234567-89ab-cdef-fedc-ba9876543210");
    }

    #[test]
    fn signed_datagrams_open_and_forged_ones_do_not() {
        let key = b"secret";
        let mut m = head("srv1", 1000, "linked");
        m.insert("id".into(), "j:x".into());
        m.insert("ok".into(), true.into());
        let gram = sign(key, &serde_json::Value::Object(m));
        assert_eq!(open_answer(key, "srv1", &gram, 1000), Some(("j:x".into(), true, None)));
        assert!(open_answer(b"other", "srv1", &gram, 1000).is_none());
        assert!(open_answer(key, "srv2", &gram, 1000).is_none());
        assert!(open_answer(key, "srv1", &gram, 1000 + 31_000).is_none());
        let mut bad = gram.clone();
        bad[5] ^= 1;
        assert!(open_answer(key, "srv1", &bad, 1000).is_none());
        assert!(open_talk(key, "srv1", &gram, 1000).is_none());
    }

    #[test]
    fn the_hmac_matches_the_bridge() {
        // The bridge (Python): hmac.new(b"k", b'{"a":1}', hashlib.sha256).hexdigest()
        let gram = sign(b"k", &serde_json::json!({ "a": 1 }));
        assert_eq!(String::from_utf8(gram).unwrap(), "{\"a\":1}\nc3a92ff9e274cdcce27a58c15a78ec6dcbbdbd0038a87e7a11baef2028fd8bff");
    }

    #[test]
    fn rows_are_sent_when_they_change() {
        let row = |x: f64, yaw: f64| Row { id: "j:a".into(), dim: "minecraft:overworld".into(), x, y: 64.0, z: 0.0, yaw };
        let mut e = Encoder::default();
        assert_eq!(e.frame(vec![row(0.0, 0.0)]).0.len(), 1);
        assert_eq!(e.frame(vec![row(0.1, 3.0)]).0.len(), 0);
        assert_eq!(e.frame(vec![row(0.3, 3.0)]).0.len(), 1);
        assert_eq!(e.frame(vec![row(0.3, 12.0)]).0.len(), 1);
        let (_, gone) = e.frame(vec![]);
        assert_eq!(gone, vec!["j:a".to_owned()]);
    }

    #[test]
    fn datagrams_are_chunked() {
        let rows: Vec<Row> = (0..90).map(|i| Row { id: format!("j:{i}"), dim: "d".into(), x: 0.0, y: 0.0, z: 0.0, yaw: 0.0 }).collect();
        assert_eq!(pos_datagrams(b"k", "s", 0, &rows, &["j:x".into()]).len(), 3);
        assert_eq!(pos_datagrams(b"k", "s", 0, &[], &["j:x".into()]).len(), 1);
        assert!(pos_datagrams(b"k", "s", 0, &[], &[]).is_empty());
    }

    #[test]
    fn codes_and_config() {
        assert_eq!(parse_code("12345"), Some("012345".into()));
        assert_eq!(parse_code("1234567"), None);
        assert_eq!(parse_code("12a"), None);
        let cfg = Config { bridge_host: "voice.example.net".into(), bridge_port: 4000, server_id: "s".into(), server_key: "00ff".into(), voice_range: 500, full_volume_range: 900, ..Config::default() };
        assert_eq!(cfg.key(), Some(vec![0, 255]));
        assert_eq!(cfg.distances(), (128, 64));
        assert_eq!(Config { server_key: "xyz".into(), ..cfg }.key(), None);
    }
}

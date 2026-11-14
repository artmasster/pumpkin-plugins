//! Messages other plugins send to BlockLog.
//!
//! - `CHB1` (binary): block changes a plugin made through the API, which fire no block event.
//!   WorldTools sends its edits this way. `b"CHB1"`, `u16` name length + player name, `u16` dimension
//!   length + dimension as `world.get_dimension()` gives it, `u32` count (at most 4096), then count
//!   times `i32 x, i32 y, i32 z, u16 old state id, u16 new state id`. All little endian.
//! - JSON starting with `{`: `{"kind":"chat","name":"<player>","message":"<text>"}` from a chat
//!   plugin that cancels the chat event and sends the line itself.

/// Most changes one message may carry.
pub const MAX_CHANGES: usize = 4096;
pub const MAGIC: &[u8; 4] = b"CHB1";
/// Bytes of one change.
const CHANGE: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub old: u16,
    pub new: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    pub user: String,
    pub dimension: String,
    pub changes: Vec<Change>,
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        if self.0.len() < n {
            return Err("message is cut short");
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn u16(&mut self) -> Result<u16, &'static str> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, &'static str> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn text(&mut self, max: usize) -> Result<String, &'static str> {
        let len = usize::from(self.u16()?);
        if len == 0 || len > max {
            return Err("bad name length");
        }
        let text = std::str::from_utf8(self.take(len)?).map_err(|_| "name is not UTF-8")?;
        if text.chars().any(char::is_control) {
            return Err("name has control characters");
        }
        Ok(text.to_owned())
    }
}

/// Read a `CHB1` message. Anything not exactly as specified is refused whole.
pub fn parse_changes(message: &[u8]) -> Result<Batch, &'static str> {
    let mut r = Reader(message);
    if r.take(4)? != MAGIC {
        return Err("not a CHB1 message");
    }
    let user = r.text(64)?;
    let dimension = r.text(128)?;
    let count = r.u32()? as usize;
    if count > MAX_CHANGES {
        return Err("more than 4096 changes");
    }
    if r.0.len() != count * CHANGE {
        return Err("length does not match the count");
    }
    let changes = r
        .0
        .chunks_exact(CHANGE)
        .map(|c| Change {
            x: i32::from_le_bytes([c[0], c[1], c[2], c[3]]),
            y: i32::from_le_bytes([c[4], c[5], c[6], c[7]]),
            z: i32::from_le_bytes([c[8], c[9], c[10], c[11]]),
            old: u16::from_le_bytes([c[12], c[13]]),
            new: u16::from_le_bytes([c[14], c[15]]),
        })
        .collect();
    Ok(Batch { user, dimension, changes })
}

/// Build a `CHB1` message (the sending side; used by the tests and the lab sender).
pub fn build_changes(user: &str, dimension: &str, changes: &[Change]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + user.len() + dimension.len() + changes.len() * CHANGE);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(user.len() as u16).to_le_bytes());
    out.extend_from_slice(user.as_bytes());
    out.extend_from_slice(&(dimension.len() as u16).to_le_bytes());
    out.extend_from_slice(dimension.as_bytes());
    out.extend_from_slice(&(changes.len() as u32).to_le_bytes());
    for c in changes {
        out.extend_from_slice(&c.x.to_le_bytes());
        out.extend_from_slice(&c.y.to_le_bytes());
        out.extend_from_slice(&c.z.to_le_bytes());
        out.extend_from_slice(&c.old.to_le_bytes());
        out.extend_from_slice(&c.new.to_le_bytes());
    }
    out
}

/// A chat line a chat plugin sent itself after cancelling the chat event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatNotice {
    pub name: String,
    pub message: String,
}

/// Read a JSON notice. `Ok(None)` = valid JSON of a kind BlockLog does not use.
pub fn parse_notice(message: &[u8]) -> Result<Option<ChatNotice>, &'static str> {
    if message.len() > 16 * 1024 {
        return Err("message too long");
    }
    let value: serde_json::Value = serde_json::from_slice(message).map_err(|_| "not JSON")?;
    if value.get("kind").and_then(|k| k.as_str()) != Some("chat") {
        return Ok(None);
    }
    let field = |key: &str| value.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    match (field("name"), field("message")) {
        (Some(name), Some(text)) if name.len() <= 64 => Ok(Some(ChatNotice { name: name.to_owned(), message: text.to_owned() })),
        _ => Err("chat notice needs name and message"),
    }
}

/// The step of a piston direction as the server names it (`North`, `Up`, …).
pub fn direction(name: &str) -> Option<(i32, i32, i32)> {
    Some(match name.to_ascii_lowercase().as_str() {
        "down" => (0, -1, 0),
        "up" => (0, 1, 0),
        "north" => (0, 0, -1),
        "south" => (0, 0, 1),
        "west" => (-1, 0, 0),
        "east" => (1, 0, 0),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(n: usize) -> Vec<Change> {
        (0..n).map(|i| Change { x: i as i32 - 5, y: 64, z: -(i as i32), old: (i % 3) as u16, new: 1 + (i % 7) as u16 }).collect()
    }

    #[test]
    fn changes_round_trip() {
        let changes = sample(100);
        let msg = build_changes("Alice", "minecraft:overworld", &changes);
        assert_eq!(msg.len(), 4 + 2 + 5 + 2 + 19 + 4 + 100 * 16);
        assert_eq!(parse_changes(&msg), Ok(Batch { user: "Alice".into(), dimension: "minecraft:overworld".into(), changes }));
        // Extremes survive
        let edge = [Change { x: i32::MIN, y: -64, z: i32::MAX, old: 0, new: u16::MAX }];
        assert_eq!(parse_changes(&build_changes("ผู้เล่น", "my:dim", &edge)).unwrap().changes, edge);
        assert_eq!(parse_changes(&build_changes("A", "d", &[])).unwrap().changes.len(), 0);
        assert_eq!(parse_changes(&build_changes("A", "d", &sample(MAX_CHANGES))).unwrap().changes.len(), MAX_CHANGES);
    }

    #[test]
    fn bad_messages_are_refused_whole() {
        let good = build_changes("Alice", "minecraft:overworld", &sample(3));
        assert!(parse_changes(&good).is_ok());
        // Every shorter message, and one byte too many
        for cut in 0..good.len() {
            assert!(parse_changes(&good[..cut]).is_err(), "cut at {cut}");
        }
        let mut long = good.clone();
        long.push(0);
        assert_eq!(parse_changes(&long), Err("length does not match the count"));
        let mut magic = good.clone();
        magic[3] = b'2';
        assert_eq!(parse_changes(&magic), Err("not a CHB1 message"));
        assert_eq!(parse_changes(&build_changes("", "d", &[])), Err("bad name length"));
        assert_eq!(parse_changes(&build_changes(&"a".repeat(65), "d", &[])), Err("bad name length"));
        assert_eq!(parse_changes(&build_changes("A\nB", "d", &[])), Err("name has control characters"));
        assert_eq!(parse_changes(&build_changes("A", "d", &sample(MAX_CHANGES + 1))), Err("more than 4096 changes"));
        let mut not_utf8 = build_changes("AB", "d", &[]);
        not_utf8[6] = 0xFF;
        assert_eq!(parse_changes(&not_utf8), Err("name is not UTF-8"));
        // A count that promises more than the message holds
        let mut lying = build_changes("A", "d", &sample(2));
        let at = 4 + 2 + 1 + 2 + 1;
        lying[at..at + 4].copy_from_slice(&4000u32.to_le_bytes());
        assert_eq!(parse_changes(&lying), Err("length does not match the count"));
    }

    #[test]
    fn notices() {
        assert_eq!(
            parse_notice(br#"{"kind":"chat","name":"Alice","message":"hello {} world"}"#),
            Ok(Some(ChatNotice { name: "Alice".into(), message: "hello {} world".into() }))
        );
        assert_eq!(parse_notice(br#"{"kind":"balance","player":"Alice"}"#), Ok(None));
        assert_eq!(parse_notice(br#"{"name":"Alice","message":"x"}"#), Ok(None));
        assert_eq!(parse_notice(br#"{"kind":"chat","name":"Alice"}"#), Err("chat notice needs name and message"));
        assert_eq!(parse_notice(br#"{"kind":"chat","name":"","message":"x"}"#), Err("chat notice needs name and message"));
        assert_eq!(parse_notice(br#"{"kind":"chat","name":7,"message":"x"}"#), Err("chat notice needs name and message"));
        assert_eq!(parse_notice(b"{not json"), Err("not JSON"));
        assert_eq!(parse_notice(&vec![b' '; 20_000]), Err("message too long"));
    }

    #[test]
    fn directions() {
        assert_eq!(direction("North"), Some((0, 0, -1)));
        assert_eq!(direction("UP"), Some((0, 1, 0)));
        assert_eq!(direction("east"), Some((1, 0, 0)));
        assert_eq!(direction("sideways"), None);
    }
}

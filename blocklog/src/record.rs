//! What is written to disk: fixed 28-byte block records and one-line text records.

/// Everything BlockLog can log. The number is what goes to disk: never renumber.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Action {
    Break = 0,
    Place = 1,
    /// A bucket emptied: a fluid appeared.
    BucketEmpty = 2,
    /// A bucket filled: a fluid was taken.
    BucketFill = 3,
    Burn = 4,
    Explode = 5,
    Chat = 16,
    Command = 17,
    Join = 18,
    Leave = 19,
    Sign = 20,
    ContainerAdd = 21,
    ContainerRemove = 22,
    Kill = 23,
    Drop = 24,
    /// A right click on a block that does something (door, lever, button, container).
    Click = 25,
}

impl Action {
    pub const ALL: [Action; 16] = [
        Action::Break,
        Action::Place,
        Action::BucketEmpty,
        Action::BucketFill,
        Action::Burn,
        Action::Explode,
        Action::Chat,
        Action::Command,
        Action::Join,
        Action::Leave,
        Action::Sign,
        Action::ContainerAdd,
        Action::ContainerRemove,
        Action::Kill,
        Action::Drop,
        Action::Click,
    ];

    pub fn from_code(code: u8) -> Option<Action> {
        Action::ALL.into_iter().find(|a| *a as u8 == code)
    }

    /// Kept in the block log (fixed records, can be rolled back).
    pub fn is_block(self) -> bool {
        (self as u8) < 16
    }

    /// The block was there before the action (the record is about the old state).
    pub fn removes(self) -> bool {
        matches!(self, Action::Break | Action::BucketFill | Action::Burn | Action::Explode)
    }

    /// Items put into or taken out of a container.
    pub fn is_container(self) -> bool {
        matches!(self, Action::ContainerAdd | Action::ContainerRemove)
    }

    pub fn bit(self) -> u32 {
        1 << (self as u8)
    }

    /// Short name in the text log.
    pub fn tag(self) -> &'static str {
        match self {
            Action::Break => "break",
            Action::Place => "place",
            Action::BucketEmpty => "b+",
            Action::BucketFill => "b-",
            Action::Burn => "burn",
            Action::Explode => "boom",
            Action::Chat => "chat",
            Action::Command => "cmd",
            Action::Join => "join",
            Action::Leave => "quit",
            Action::Sign => "sign",
            Action::ContainerAdd => "c+",
            Action::ContainerRemove => "c-",
            Action::Kill => "kill",
            Action::Drop => "drop",
            Action::Click => "click",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.tag() == tag)
    }
}

/// Size of one block record on disk.
pub const REC: usize = 28;
/// Where the action byte sits; its top bit is the only thing ever changed in place.
pub const FLAG_AT: usize = 18;
const ROLLED: u8 = 0x80;

/// One block change. `user`, `old` and `new` are line numbers in the name tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockRec {
    pub time: u32,
    pub user: u32,
    pub x: i32,
    pub y: i16,
    pub z: i32,
    pub action: Action,
    pub rolled: bool,
    pub old: u32,
    pub new: u32,
}

/// CRC-8 (poly 0x07), one table look-up per byte.
const CRC: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u8;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// The check byte of a record: a start value that makes a run of zero bytes invalid.
fn check(bytes: &[u8; REC]) -> u8 {
    let mut crc = 0xA5u8;
    for (i, b) in bytes.iter().enumerate() {
        let b = match i {
            19 => continue,
            // The rolled-back bit changes later without the check byte being rewritten
            FLAG_AT => b & !ROLLED,
            _ => *b,
        };
        crc = CRC[(crc ^ b) as usize];
    }
    crc
}

impl BlockRec {
    pub fn encode(&self) -> [u8; REC] {
        let mut b = [0u8; REC];
        b[0..4].copy_from_slice(&self.time.to_le_bytes());
        b[4..8].copy_from_slice(&self.user.to_le_bytes());
        b[8..12].copy_from_slice(&self.x.to_le_bytes());
        b[12..16].copy_from_slice(&self.z.to_le_bytes());
        b[16..18].copy_from_slice(&self.y.to_le_bytes());
        b[FLAG_AT] = self.action as u8 | if self.rolled { ROLLED } else { 0 };
        b[20..24].copy_from_slice(&self.old.to_le_bytes());
        b[24..28].copy_from_slice(&self.new.to_le_bytes());
        b[19] = check(&b);
        b
    }

    /// `None` for anything that is not a whole, intact record (torn write, zero fill).
    pub fn decode(bytes: &[u8]) -> Option<BlockRec> {
        let b: &[u8; REC] = bytes.try_into().ok()?;
        if check(b) != b[19] {
            return None;
        }
        let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        Some(BlockRec {
            time: u32_at(0),
            user: u32_at(4),
            x: u32_at(8) as i32,
            z: u32_at(12) as i32,
            y: i16::from_le_bytes([b[16], b[17]]),
            action: Action::from_code(b[FLAG_AT] & !ROLLED).filter(|a| a.is_block())?,
            rolled: b[FLAG_AT] & ROLLED != 0,
            old: u32_at(20),
            new: u32_at(24),
        })
    }
}

/// Flip the rolled-back bit of an encoded record in place.
pub fn set_rolled(bytes: &mut [u8], rolled: bool) {
    if rolled {
        bytes[FLAG_AT] |= ROLLED;
    } else {
        bytes[FLAG_AT] &= !ROLLED;
    }
}

/// Longest text kept for one record (chat line, command, sign).
pub const MAX_TEXT: usize = 256;
/// Longest item text of a container record: the item's name and, for an item that is more than
/// its name (enchanted, renamed, damaged), its data (see `items`).
pub const MAX_ITEM: usize = 3800;

/// Everything that is not a block change: one line of text per record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextRec {
    pub time: u32,
    pub action: Action,
    pub user: String,
    /// World key (`overworld`), empty when the record has no place.
    pub world: String,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    /// Item count for container and drop records, else 0.
    pub amount: u32,
    /// Message, command, sign text, item or entity name.
    pub text: String,
}

/// Player text is hostile: no control characters, bounded length.
pub fn clean(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

fn field(text: &str) -> String {
    clean(text, 64).replace('\t', " ")
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

impl TextRec {
    /// One line, newline included.
    pub fn encode(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            self.time,
            self.action.tag(),
            field(&self.user),
            field(&self.world),
            self.x,
            self.y,
            self.z,
            self.amount,
            escape(&self.text.chars().take(if self.action.is_container() { MAX_ITEM } else { MAX_TEXT }).collect::<String>())
        )
    }

    /// `None` for a line that is not a whole record (torn write).
    pub fn decode(line: &str) -> Option<TextRec> {
        TextRec::decode_marked(line).map(|(rec, _)| rec)
    }

    /// The record and whether it is marked rolled back. The mark is the tag's first letter in
    /// upper case (`C+`): the one byte of a text log that is ever changed in place.
    pub fn decode_marked(line: &str) -> Option<(TextRec, bool)> {
        let mut f = line.split('\t');
        let time = f.next()?.parse().ok()?;
        let tag = f.next()?;
        let rolled = tag.starts_with('C');
        let rec = TextRec {
            time,
            action: (if rolled { Action::from_tag(&tag.to_ascii_lowercase()).filter(|a| a.is_container()) } else { Action::from_tag(tag) }).filter(|a| !a.is_block())?,
            user: f.next()?.to_owned(),
            world: f.next()?.to_owned(),
            x: f.next()?.parse().ok()?,
            y: f.next()?.parse().ok()?,
            z: f.next()?.parse().ok()?,
            amount: f.next()?.parse().ok()?,
            text: unescape(f.next()?),
        };
        f.next().is_none().then_some((rec, rolled))
    }

    /// Where in a line the byte that carries the rolled-back mark sits.
    pub fn mark_offset(line: &str) -> Option<usize> {
        line.find('\t').map(|i| i + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec() -> BlockRec {
        BlockRec { time: 1_800_000_000, user: 7, x: -30_000_000, y: -64, z: 29_999_999, action: Action::Place, rolled: false, old: 0, new: 123_456 }
    }

    #[test]
    fn block_round_trip() {
        let r = rec();
        assert_eq!(BlockRec::decode(&r.encode()), Some(r));
        for action in Action::ALL.into_iter().filter(|a| a.is_block()) {
            let r = BlockRec { action, rolled: true, y: 319, ..rec() };
            assert_eq!(BlockRec::decode(&r.encode()), Some(r));
        }
    }

    #[test]
    fn rolled_bit_keeps_the_check_valid() {
        let mut bytes = rec().encode();
        set_rolled(&mut bytes, true);
        assert_eq!(BlockRec::decode(&bytes), Some(BlockRec { rolled: true, ..rec() }));
        set_rolled(&mut bytes, false);
        assert_eq!(BlockRec::decode(&bytes), Some(rec()));
    }

    #[test]
    fn damaged_records_are_refused() {
        assert_eq!(BlockRec::decode(&[0u8; REC]), None);
        assert_eq!(BlockRec::decode(&[0xFFu8; REC]), None);
        assert_eq!(BlockRec::decode(&rec().encode()[..REC - 1]), None);
        let good = rec().encode();
        for i in 0..REC {
            let mut bad = good;
            bad[i] ^= 0x01;
            assert_eq!(BlockRec::decode(&bad), None, "bit flip in byte {i}");
        }
        // A text action in a block file is damage too
        let mut bad = good;
        bad[FLAG_AT] = Action::Chat as u8;
        assert_eq!(BlockRec::decode(&bad), None);
    }

    #[test]
    fn text_round_trip() {
        let r = TextRec {
            time: 1_800_000_000,
            action: Action::Chat,
            user: "Alice".into(),
            world: "overworld".into(),
            x: -5,
            y: 64,
            z: 12,
            amount: 0,
            text: "tab\there \\ back\\slash and สวัสดี".into(),
        };
        let line = r.encode();
        assert!(line.ends_with('\n') && line.matches('\n').count() == 1);
        assert_eq!(TextRec::decode(line.trim_end_matches('\n')), Some(r));
    }

    #[test]
    fn text_is_cleaned() {
        let r = TextRec {
            time: 1,
            action: Action::Command,
            user: "Bad\tName\n".into(),
            world: String::new(),
            x: 0,
            y: 0,
            z: 0,
            amount: 0,
            text: format!("line1\nline2\u{7}{}", "x".repeat(1000)),
        };
        let line = r.encode();
        assert_eq!(line.matches('\n').count(), 1);
        let back = TextRec::decode(line.trim_end_matches('\n')).unwrap();
        assert_eq!(back.user, "BadName");
        assert!(back.text.starts_with("line1\nline2x"));
        assert!(back.text.chars().count() <= MAX_TEXT);
    }

    #[test]
    fn torn_text_lines_are_refused() {
        let r = TextRec { time: 5, action: Action::Join, user: "A".into(), world: "overworld".into(), x: 1, y: 2, z: 3, amount: 0, text: String::new() };
        let line = r.encode();
        let line = line.trim_end_matches('\n');
        assert!(TextRec::decode(line).is_some());
        for cut in 0..line.len() - 1 {
            // Cutting inside the last (empty) field cannot happen here: every shorter line lacks a field
            assert_eq!(TextRec::decode(&line[..cut]), None, "cut at {cut}");
        }
        assert_eq!(TextRec::decode("1\tplace\tA\tw\t0\t0\t0\t0\t"), None);
        // Only container records can carry the rolled-back mark
        assert_eq!(TextRec::decode("1\tChat\tA\tw\t0\t0\t0\t0\thi"), None);
    }

    #[test]
    fn rolled_back_mark_of_a_text_record() {
        let r = TextRec { time: 5, action: Action::ContainerRemove, user: "A".into(), world: "overworld".into(), x: 1, y: 2, z: 3, amount: 4, text: "diamond".into() };
        let line = r.encode();
        let line = line.trim_end_matches('\n');
        assert_eq!(TextRec::decode_marked(line), Some((r.clone(), false)));
        // Flip the one byte in place, as the rollback does in the file
        let at = TextRec::mark_offset(line).unwrap();
        let mut bytes = line.as_bytes().to_vec();
        assert_eq!(bytes[at], b'c');
        bytes[at] = b'C';
        let marked = String::from_utf8(bytes).unwrap();
        assert_eq!(TextRec::decode_marked(&marked), Some((r, true)));
        assert_eq!(marked.len(), line.len());
    }

    #[test]
    fn long_item_text_is_kept_for_containers_only() {
        let long = format!("diamond_sword|{}", "ab".repeat(1500));
        let item = TextRec { time: 5, action: Action::ContainerAdd, user: "A".into(), world: "w".into(), x: 0, y: 0, z: 0, amount: 1, text: long.clone() };
        assert_eq!(TextRec::decode(item.encode().trim_end_matches('\n')).unwrap().text, long);
        let chat = TextRec { action: Action::Chat, ..item };
        assert_eq!(TextRec::decode(chat.encode().trim_end_matches('\n')).unwrap().text.len(), MAX_TEXT);
        assert_eq!(TextRec::decode("1\tchat\tA\tw\t0\t0\t0\t0\thi\textra"), None);
    }
}

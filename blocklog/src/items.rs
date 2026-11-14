//! An item as a container record names it: `diamond`, or for an item that is more than its name
//! (enchanted, renamed, damaged, a filled shulker box) `diamond_sword|<data>`.
//!
//! The data is what the server hands out as the item's components, each as a number and its
//! serialized bytes, written in hex: `id` (4 digits), `length` (8 digits), bytes. Two items with the
//! same text are the same item to the game, which is what lets a rollback take exactly those out
//! of a chest again and put exactly those back.

/// Written instead of the data when it is too long to keep: the item comes back plain.
pub const LOSSY: &str = "!";

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Item {
    /// Without `minecraft:`.
    pub name: String,
    /// Component number and serialized value, ordered by number.
    pub parts: Vec<(u16, Vec<u8>)>,
    /// The item had data that was not kept.
    pub lossy: bool,
}

/// `diamond_sword` of `diamond_sword|0010…`.
pub fn name(text: &str) -> &str {
    text.split('|').next().unwrap_or(text)
}

/// Whether the text carries data (or says it lost it).
pub fn has_data(text: &str) -> bool {
    text.contains('|')
}

/// The record text of an item. Longer than `max` characters = the data is dropped and marked.
pub fn encode(item_name: &str, parts: &[(u16, Vec<u8>)], max: usize) -> String {
    let bare = item_name.strip_prefix("minecraft:").unwrap_or(item_name);
    let bare: String = bare.chars().filter(|c| !c.is_control() && *c != '|').take(96).collect();
    if parts.is_empty() {
        return bare;
    }
    let mut sorted: Vec<&(u16, Vec<u8>)> = parts.iter().collect();
    sorted.sort();
    let size: usize = sorted.iter().map(|(_, v)| 12 + v.len() * 2).sum();
    if bare.len() + 1 + size > max {
        return format!("{bare}|{LOSSY}");
    }
    let mut out = String::with_capacity(bare.len() + 1 + size);
    out.push_str(&bare);
    out.push('|');
    for (id, value) in sorted {
        out.push_str(&format!("{id:04x}{:08x}", value.len()));
        for b in value {
            out.push_str(&format!("{b:02x}"));
        }
    }
    out
}

fn hex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 || !text.is_ascii() {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok()).collect()
}

/// Read a record text back. Data that does not parse is treated as lost, never guessed.
pub fn decode(text: &str) -> Item {
    let Some((item_name, data)) = text.split_once('|') else {
        return Item { name: text.to_owned(), parts: Vec::new(), lossy: false };
    };
    let mut item = Item { name: item_name.to_owned(), parts: Vec::new(), lossy: false };
    let mut rest = data;
    while !rest.is_empty() {
        let part = (|| {
            let id = u16::from_str_radix(rest.get(..4)?, 16).ok()?;
            let len = usize::try_from(u32::from_str_radix(rest.get(4..12)?, 16).ok()?).ok()?;
            let value = hex(rest.get(12..12 + len.checked_mul(2)?)?)?;
            Some((id, value, 12 + len * 2))
        })();
        match part {
            Some((id, value, used)) => {
                item.parts.push((id, value));
                rest = &rest[used..];
            }
            None => {
                item.parts.clear();
                item.lossy = true;
                break;
            }
        }
    }
    item
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_items_are_their_name() {
        assert_eq!(encode("minecraft:diamond", &[], 100), "diamond");
        assert_eq!(encode("diamond", &[], 100), "diamond");
        assert_eq!(decode("diamond"), Item { name: "diamond".into(), parts: vec![], lossy: false });
        assert_eq!(name("diamond"), "diamond");
        assert!(!has_data("diamond"));
    }

    #[test]
    fn data_round_trips_in_a_fixed_order() {
        let parts = vec![(300u16, vec![0xde, 0xad, 0xbe, 0xef]), (7u16, vec![]), (16u16, vec![1, 2, 3])];
        let text = encode("minecraft:diamond_sword", &parts, 4000);
        assert_eq!(text, "diamond_sword|000700000000001000000003010203012c00000004deadbeef");
        assert_eq!(name(&text), "diamond_sword");
        assert!(has_data(&text));
        let back = decode(&text);
        assert_eq!(back.name, "diamond_sword");
        assert_eq!(back.parts, vec![(7, vec![]), (16, vec![1, 2, 3]), (300, vec![0xde, 0xad, 0xbe, 0xef])]);
        assert!(!back.lossy);
        // The same item gives the same text whatever order the server lists its data in
        let mut shuffled = parts.clone();
        shuffled.reverse();
        assert_eq!(encode("diamond_sword", &shuffled, 4000), text);
    }

    #[test]
    fn too_much_data_is_marked_not_cut() {
        let parts = vec![(1u16, vec![0u8; 5000])];
        let text = encode("shulker_box", &parts, 3800);
        assert_eq!(text, "shulker_box|!");
        let back = decode(&text);
        assert_eq!((back.name.as_str(), back.parts.len(), back.lossy), ("shulker_box", 0, true));
    }

    #[test]
    fn damaged_data_is_lost_not_guessed() {
        for bad in ["sword|0", "sword|zzzz00000001ff", "sword|000100000002ff", "sword|0001000000", "sword|000100000001f", "sword|00010000000éé"] {
            let back = decode(bad);
            assert_eq!((back.name.as_str(), back.parts.len(), back.lossy), ("sword", 0, true), "{bad}");
        }
        // A huge length must not overflow or allocate
        assert!(decode("sword|0001ffffffff00").lossy);
    }

    #[test]
    fn names_cannot_carry_a_separator() {
        assert_eq!(encode("a|b\tc", &[], 100), "ab\u{0}c".replace('\u{0}', ""));
    }
}

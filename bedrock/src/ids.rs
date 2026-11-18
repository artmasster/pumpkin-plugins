//! The list of entity identifiers a Bedrock client accepts (`AvailableActorIdentifiers`).
//!
//! Pumpkin 0.2.2 does not send it, so the client knows the game's own entities only and draws
//! nothing for one of ours. The list sent here is the game's own (`entity_identifiers.nbt`
//! from pmmp/BedrockData, CC0) with ours added. It is network NBT: one compound holding
//! `idlist`, a list of compounds.

/// The game's own identifiers.
const KNOWN: &[u8] = include_bytes!("entity_identifiers.nbt");
/// How the file starts: a compound without a name, a list `idlist` of compounds.
const HEAD: &[u8] = b"\x0a\x00\x09\x06idlist\x0a";
/// The numbers of the game's own entities stay far below this.
const FIRST_NUMBER: u32 = 10_000;

fn varint(mut v: u32, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn name(tag: u8, text: &str, out: &mut Vec<u8>) {
    out.push(tag);
    varint(text.len() as u32, out);
    out.extend_from_slice(text.as_bytes());
}

/// The game's list with `ours` added, as the body of the packet. `None` when the file does not
/// look as expected.
pub fn list(ours: &[String]) -> Option<Vec<u8>> {
    if KNOWN.get(..HEAD.len())? != HEAD {
        return None;
    }
    let (mut count, mut shift, mut at) = (0u32, 0, HEAD.len());
    loop {
        let byte = *KNOWN.get(at)?;
        at += 1;
        count |= u32::from(byte & 0x7f) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            break;
        }
        if shift > 28 {
            return None;
        }
    }
    let mut out = HEAD.to_vec();
    // A count n is written as 2n (zigzag)
    varint(((count >> 1) + ours.len() as u32) << 1, &mut out);
    for (i, id) in ours.iter().enumerate() {
        name(8, "bid", &mut out);
        varint(0, &mut out);
        name(1, "hasspawnegg", &mut out);
        out.push(0);
        name(8, "id", &mut out);
        varint(id.len() as u32, &mut out);
        out.extend_from_slice(id.as_bytes());
        name(3, "rid", &mut out);
        varint((FIRST_NUMBER + i as u32) << 1, &mut out);
        name(1, "summonable", &mut out);
        out.push(1);
        out.push(0);
    }
    out.extend_from_slice(&KNOWN[at..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ours_are_added_to_the_games_own() {
        let plain = list(&[]).unwrap();
        assert_eq!(plain, KNOWN);
        let with = list(&["models:dragon".to_owned(), "models:example".to_owned()]).unwrap();
        assert!(with.len() > KNOWN.len());
        let find = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
        assert!(find(&with, b"models:dragon"));
        assert!(find(&with, b"models:example"));
        assert!(find(&with, b"minecraft:pig"));
        // Two more entries than the game's own
        let count = |data: &[u8]| (u32::from(data[HEAD.len()] & 0x7f) | u32::from(data[HEAD.len() + 1]) << 7) >> 1;
        assert_eq!(count(&with), count(KNOWN) + 2);
        assert_eq!(with.last(), Some(&0));
    }
}

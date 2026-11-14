//! What a player types for blocks: `oak_stairs[facing=north,half=top]`, patterns with weights,
//! masks (`stone,dirt`, `!air`, `#existing`, `#solid`). No server types here: the glue hands in
//! what the server knows about a block (its states and their properties) and this module decides.

use crate::geo::block_name;

/// Properties of a block state, in the order the server lists them.
pub type Props = Vec<(String, String)>;

/// Longest text accepted for one pattern or mask argument.
pub const MAX_ARG: usize = 512;

fn word(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Split on `sep`, but not inside `[...]` (the properties of a block use commas too).
pub fn split_top(s: &str, sep: char) -> Vec<&str> {
    let (mut out, mut depth, mut from) = (Vec::new(), 0i32, 0usize);
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            c if c == sep && depth <= 0 => {
                out.push(&s[from..i]);
                from = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[from..]);
    out
}

/// A block and the properties the player asked for (none = the block's default state).
#[derive(Clone, Debug, PartialEq)]
pub struct BlockSpec {
    /// With its namespace: `minecraft:oak_stairs`.
    pub name: String,
    pub props: Props,
}

impl BlockSpec {
    /// `stone`, `minecraft:oak_log[axis=x]`, `oak_stairs[facing=north,half=top]`.
    pub fn parse(s: &str) -> Option<BlockSpec> {
        let s = s.trim().to_lowercase();
        let (name, rest) = match s.split_once('[') {
            Some((n, r)) => (n, Some(r.strip_suffix(']')?)),
            None => (s.as_str(), None),
        };
        let name = block_name(name)?;
        let mut props = Props::new();
        if let Some(rest) = rest {
            for pair in rest.split(',') {
                let (k, v) = pair.split_once('=')?;
                let (k, v) = (k.trim(), v.trim());
                if !word(k) || !word(v) || props.iter().any(|(have, _)| have == k) || props.len() >= 16 {
                    return None;
                }
                props.push((k.to_owned(), v.to_owned()));
            }
            if props.is_empty() {
                return None;
            }
        }
        Some(BlockSpec { name, props })
    }
}

/// `minecraft:oak_stairs[facing=north,half=top]`, or the bare name when there are no properties.
pub fn state_string(name: &str, props: &[(String, String)]) -> String {
    if props.is_empty() {
        return name.to_owned();
    }
    let inner: Vec<String> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("{name}[{}]", inner.join(","))
}

/// Why a block with properties could not be turned into a state.
#[derive(Debug, PartialEq)]
pub enum StateError {
    /// The block has no such property; the ones it has.
    Property(String, Vec<String>),
    /// The property cannot have that value; the values it can have.
    Value(String, String, Vec<String>),
    /// Every property is fine on its own but the block has no state with all of them.
    Combination,
}

/// The values a property takes over all states of a block, in first-seen order.
fn values_of<'a>(table: &'a [(u16, Props)], key: &str) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    for (_, props) in table {
        if let Some((_, v)) = props.iter().find(|(k, _)| k == key) {
            if !out.contains(&v.as_str()) {
                out.push(v);
            }
        }
    }
    out
}

/// Are these properties ones the block has, with values they can take? (`table` = every state of
/// the block with its properties.)
pub fn check_props(table: &[(u16, Props)], wanted: &[(String, String)]) -> Result<(), StateError> {
    for (k, v) in wanted {
        let values = values_of(table, k);
        if values.is_empty() {
            let mut keys: Vec<String> = table.first().map(|(_, p)| p.iter().map(|(k, _)| k.clone()).collect()).unwrap_or_default();
            keys.sort();
            return Err(StateError::Property(k.clone(), keys));
        }
        if !values.contains(&v.as_str()) {
            return Err(StateError::Value(k.clone(), v.clone(), values.into_iter().map(str::to_owned).collect()));
        }
    }
    Ok(())
}

/// The state of a block with the wanted properties; the ones not named keep the value of the
/// block's default state.
pub fn pick_state(table: &[(u16, Props)], default: u16, wanted: &[(String, String)]) -> Result<u16, StateError> {
    if wanted.is_empty() {
        return Ok(default);
    }
    check_props(table, wanted)?;
    let base = table.iter().find(|(id, _)| *id == default).or(table.first()).ok_or(StateError::Combination)?;
    let target: Props = base.1.iter().map(|(k, v)| (k.clone(), wanted.iter().find(|(wk, _)| wk == k).map_or_else(|| v.clone(), |(_, wv)| wv.clone()))).collect();
    find_state(table, &target).ok_or(StateError::Combination)
}

/// The state whose properties are exactly these (in any order).
pub fn find_state(table: &[(u16, Props)], props: &[(String, String)]) -> Option<u16> {
    table.iter().find(|(_, have)| have.len() == props.len() && have.iter().all(|(k, v)| props.iter().any(|(pk, pv)| pk == k && pv == v))).map(|(id, _)| *id)
}

/// Like `pick_state` for a state saved by another version of the game: properties the block no
/// longer has, or values it no longer takes, fall back to the default. `true` when that happened.
pub fn pick_state_lenient(table: &[(u16, Props)], default: u16, wanted: &[(String, String)]) -> (u16, bool) {
    let mut lost = false;
    let usable: Props = wanted
        .iter()
        .filter(|(k, v)| {
            let ok = values_of(table, k).contains(&v.as_str());
            lost |= !ok;
            ok
        })
        .cloned()
        .collect();
    match pick_state(table, default, &usable) {
        Ok(id) => (id, lost),
        Err(_) => (default, true),
    }
}

/// `stone`, `stone,dirt`, `70%stone,30%oak_log[axis=x]`: blocks with weights.
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    /// (block, running total of the weights up to and including it)
    items: Vec<(BlockSpec, u32)>,
    total: u32,
}

impl Pattern {
    pub fn parse(s: &str) -> Option<Pattern> {
        if s.len() > MAX_ARG {
            return None;
        }
        let mut items = Vec::new();
        let mut total = 0u32;
        for part in split_top(s, ',') {
            let (weight, name) = match part.split_once('%') {
                Some((w, n)) => (w.trim().parse::<u32>().ok().filter(|w| (1..=100_000).contains(w))?, n),
                None => (1, part),
            };
            total = total.checked_add(weight)?;
            items.push((BlockSpec::parse(name)?, total));
        }
        (!items.is_empty() && items.len() <= 32).then_some(Pattern { items, total })
    }

    pub fn specs(&self) -> impl Iterator<Item = &BlockSpec> {
        self.items.iter().map(|(s, _)| s)
    }

    /// The same pattern with every block turned into its state id (`states` in the order of `specs`).
    pub fn palette(&self, states: &[u16]) -> Option<Palette> {
        (states.len() == self.items.len()).then(|| Palette { items: self.items.iter().zip(states).map(|((_, upto), s)| (*s, *upto)).collect(), total: self.total })
    }
}

/// A pattern after the server has named the state of every block in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    items: Vec<(u16, u32)>,
    total: u32,
}

impl Palette {
    pub fn single(state: u16) -> Palette {
        Palette { items: vec![(state, 1)], total: 1 }
    }

    /// The state for one position; `roll` is any random number.
    pub fn pick(&self, roll: u64) -> u16 {
        if self.items.len() == 1 {
            return self.items[0].0;
        }
        let at = (roll % u64::from(self.total)) as u32;
        self.items.iter().find(|(_, upto)| at < *upto).map_or(self.items[0].0, |(s, _)| *s)
    }
}

/// What the server knows about one block state, as far as masks care.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    /// With its namespace.
    pub name: String,
    pub props: Props,
    pub air: bool,
    pub solid: bool,
    pub liquid: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MaskKind {
    /// This block; with properties, only the states that have all of them.
    Block(BlockSpec),
    /// Anything but air.
    Existing,
    Solid,
    Liquid,
    Air,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MaskTerm {
    pub not: bool,
    pub kind: MaskKind,
}

/// `stone,dirt` (any of them), `!air` (anything else), `#existing,!water` (both must hold):
/// entries without `!` are alternatives, entries with `!` are all excluded.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    terms: Vec<MaskTerm>,
}

impl Mask {
    pub fn parse(s: &str) -> Option<Mask> {
        if s.len() > MAX_ARG {
            return None;
        }
        let mut terms = Vec::new();
        for part in split_top(s, ',') {
            let part = part.trim();
            let (not, body) = match part.strip_prefix('!') {
                Some(b) => (true, b),
                None => (false, part),
            };
            let kind = match body.to_lowercase().as_str() {
                "#existing" => MaskKind::Existing,
                "#solid" => MaskKind::Solid,
                "#liquid" => MaskKind::Liquid,
                "#air" => MaskKind::Air,
                _ => MaskKind::Block(BlockSpec::parse(body)?),
            };
            terms.push(MaskTerm { not, kind });
        }
        (!terms.is_empty() && terms.len() <= 32).then_some(Mask { terms })
    }

    /// One block, any state (what the old `stone,dirt` lists meant).
    pub fn block(name: &str) -> Mask {
        Mask { terms: vec![MaskTerm { not: false, kind: MaskKind::Block(BlockSpec { name: name.to_owned(), props: Props::new() }) }] }
    }

    pub fn terms(&self) -> &[MaskTerm] {
        &self.terms
    }

    pub fn specs(&self) -> impl Iterator<Item = &BlockSpec> {
        self.terms.iter().filter_map(|t| match &t.kind {
            MaskKind::Block(s) => Some(s),
            _ => None,
        })
    }

    pub fn matches(&self, f: &Facts) -> bool {
        let hit = |kind: &MaskKind| match kind {
            MaskKind::Existing => !f.air,
            MaskKind::Solid => f.solid,
            MaskKind::Liquid => f.liquid,
            MaskKind::Air => f.air,
            MaskKind::Block(spec) => same_block(&spec.name, &f.name) && spec.props.iter().all(|(k, v)| f.props.iter().any(|(fk, fv)| fk == k && fv == v)),
        };
        let mut any_positive = false;
        let mut positive_hit = false;
        for t in &self.terms {
            if t.not {
                if hit(&t.kind) {
                    return false;
                }
            } else {
                any_positive = true;
                positive_hit |= hit(&t.kind);
            }
        }
        !any_positive || positive_hit
    }
}

/// The server names vanilla blocks with or without `minecraft:` depending on the call.
pub fn same_block(a: &str, b: &str) -> bool {
    a.strip_prefix("minecraft:").unwrap_or(a) == b.strip_prefix("minecraft:").unwrap_or(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::roll;

    fn p(pairs: &[(&str, &str)]) -> Props {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    /// A made-up stairs block: 4 facings x 2 halves, ids from 100, default = north/bottom.
    fn stairs() -> Vec<(u16, Props)> {
        let mut out = Vec::new();
        let mut id = 100;
        for facing in ["north", "south", "west", "east"] {
            for half in ["top", "bottom"] {
                out.push((id, p(&[("facing", facing), ("half", half)])));
                id += 1;
            }
        }
        out
    }

    #[test]
    fn block_specs() {
        assert_eq!(BlockSpec::parse("Stone"), Some(BlockSpec { name: "minecraft:stone".into(), props: vec![] }));
        let s = BlockSpec::parse("oak_stairs[facing=north, half=top]").unwrap();
        assert_eq!((s.name.as_str(), s.props), ("minecraft:oak_stairs", p(&[("facing", "north"), ("half", "top")])));
        assert_eq!(BlockSpec::parse("mod:thing[lit=true]").unwrap().name, "mod:thing");
        for bad in ["", "stone[", "stone[]", "stone[a]", "stone[a=]", "stone[=b]", "stone[a=b", "stone[a=b]x", "stone[a=b,a=c]", "st one", "stone[a=b c]", "stone[a={}]"] {
            assert_eq!(BlockSpec::parse(bad), None, "{bad}");
        }
        assert_eq!(state_string("minecraft:stone", &[]), "minecraft:stone");
        assert_eq!(state_string("minecraft:oak_log", &p(&[("axis", "x")])), "minecraft:oak_log[axis=x]");
    }

    #[test]
    fn splitting() {
        assert_eq!(split_top("a,b[c=d,e=f],g", ','), vec!["a", "b[c=d,e=f]", "g"]);
        assert_eq!(split_top("", ','), vec![""]);
        assert_eq!(split_top("a]],b", ','), vec!["a]]", "b"]);
    }

    #[test]
    fn states_from_properties() {
        let t = stairs();
        assert_eq!(pick_state(&t, 101, &[]), Ok(101));
        assert_eq!(pick_state(&t, 101, &p(&[("facing", "east")])), Ok(107));
        assert_eq!(pick_state(&t, 101, &p(&[("half", "top"), ("facing", "south")])), Ok(102));
        assert_eq!(pick_state(&t, 101, &p(&[("axis", "x")])), Err(StateError::Property("axis".into(), vec!["facing".into(), "half".into()])));
        assert_eq!(pick_state(&t, 101, &p(&[("facing", "up")])), Err(StateError::Value("facing".into(), "up".into(), vec!["north".into(), "south".into(), "west".into(), "east".into()])));
        assert_eq!(pick_state(&[(5, vec![])], 5, &p(&[("axis", "x")])), Err(StateError::Property("axis".into(), vec![])));
        assert_eq!(find_state(&t, &p(&[("half", "bottom"), ("facing", "west")])), Some(105));
        assert_eq!(find_state(&t, &p(&[("facing", "west")])), None);
        // A state from another version: what still exists is kept
        assert_eq!(pick_state_lenient(&t, 101, &p(&[("facing", "east"), ("half", "top")])), (106, false));
        assert_eq!(pick_state_lenient(&t, 101, &p(&[("facing", "east"), ("waterlogged", "true")])), (107, true));
        assert_eq!(pick_state_lenient(&t, 101, &p(&[("facing", "up")])), (101, true));
    }

    #[test]
    fn patterns() {
        let pat = Pattern::parse("70%stone,30%oak_log[axis=x]").unwrap();
        assert_eq!(pat.specs().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["minecraft:stone", "minecraft:oak_log"]);
        assert_eq!(pat.specs().nth(1).unwrap().props, p(&[("axis", "x")]));
        let pal = pat.palette(&[1, 77]).unwrap();
        let stone = (0..1000).filter(|i| pal.pick(roll(1, (*i, 0, 0))) == 1).count();
        assert!((600..800).contains(&stone), "{stone}");
        assert!(pat.palette(&[1]).is_none());
        assert_eq!(Palette::single(9).pick(12345), 9);
        assert_eq!(Pattern::parse("stone,dirt").unwrap().specs().count(), 2);
        assert_eq!(Pattern::parse("oak_stairs[facing=north,half=top],stone").unwrap().specs().count(), 2);
        for bad in ["0%stone", "", "stone,,dirt", "stone[facing=north", "200000%stone", "x%stone"] {
            assert!(Pattern::parse(bad).is_none(), "{bad}");
        }
        assert!(Pattern::parse(&"stone,".repeat(40)).is_none());
        assert!(Pattern::parse(&format!("{}stone", "a".repeat(MAX_ARG))).is_none());
    }

    #[test]
    fn masks() {
        let f = |name: &str, props: &[(&str, &str)], air: bool, solid: bool, liquid: bool| Facts { name: name.into(), props: p(props), air, solid, liquid };
        let stone = f("minecraft:stone", &[], false, true, false);
        let dirt = f("minecraft:dirt", &[], false, true, false);
        let air = f("minecraft:air", &[], true, false, false);
        let water = f("minecraft:water", &[("level", "0")], false, false, true);
        let stair_n = f("minecraft:oak_stairs", &[("facing", "north"), ("half", "top")], false, true, false);
        let stair_s = f("minecraft:oak_stairs", &[("facing", "south"), ("half", "top")], false, true, false);

        let m = Mask::parse("stone, dirt").unwrap();
        assert!(m.matches(&stone) && m.matches(&dirt) && !m.matches(&air) && !m.matches(&water));
        let m = Mask::parse("!stone").unwrap();
        assert!(!m.matches(&stone) && m.matches(&dirt) && m.matches(&air));
        let m = Mask::parse("#existing").unwrap();
        assert!(m.matches(&stone) && m.matches(&water) && !m.matches(&air));
        let m = Mask::parse("#solid").unwrap();
        assert!(m.matches(&stone) && !m.matches(&water) && !m.matches(&air));
        let m = Mask::parse("#existing,!water,!dirt").unwrap();
        assert!(m.matches(&stone) && !m.matches(&water) && !m.matches(&dirt) && !m.matches(&air));
        let m = Mask::parse("#liquid,stone").unwrap();
        assert!(m.matches(&stone) && m.matches(&water) && !m.matches(&dirt));
        let m = Mask::parse("#air").unwrap();
        assert!(m.matches(&air) && !m.matches(&stone));
        let m = Mask::parse("oak_stairs[facing=north]").unwrap();
        assert!(m.matches(&stair_n) && !m.matches(&stair_s) && !m.matches(&stone));
        let m = Mask::parse("oak_stairs,!oak_stairs[facing=north]").unwrap();
        assert!(!m.matches(&stair_n) && m.matches(&stair_s));
        // The server sometimes drops the namespace
        assert!(Mask::parse("stone").unwrap().matches(&f("stone", &[], false, true, false)));
        assert!(Mask::block("minecraft:dirt").matches(&dirt) && !Mask::block("minecraft:dirt").matches(&stone));
        assert_eq!(Mask::parse("stone,oak_log[axis=x]").unwrap().specs().count(), 2);
        for bad in ["", "st one", "#nothing", "!", "stone,,dirt", "!#"] {
            assert!(Mask::parse(bad).is_none(), "{bad}");
        }
    }
}

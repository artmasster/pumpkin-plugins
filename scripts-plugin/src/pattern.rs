//! Skript-style syntax patterns: `send %text% [to %players%]`, `(a|b)`, `[optional]`, `2:tagged`.
//! A pattern is matched against one line (or one expression); every way it can match is offered
//! to the caller, who accepts the first one whose `%slots%` parse.

#[derive(Debug, Clone)]
enum El {
    /// Text that must follow right here.
    Lit(String),
    /// A blank in the pattern: optional blanks in the input, but never the middle of a word.
    Space,
    Slot(usize),
    /// Alternatives with their tag; an optional part is an alternative with an empty arm.
    Alt(Vec<(Vec<El>, u32)>),
}

#[derive(Debug, Clone)]
pub struct Pattern {
    els: Vec<El>,
    slots: usize,
}

pub struct Caps<'a> {
    pub slots: Vec<Option<&'a str>>,
    pub mark: u32,
}

impl<'a> Caps<'a> {
    pub fn get(&self, i: usize) -> Option<&'a str> {
        self.slots.get(i).copied().flatten()
    }
}

/// One input with its nesting map: `top[i]` is true when byte `i` is outside quotes, braces and parentheses.
pub struct Input<'a> {
    pub s: &'a str,
    top: Vec<bool>,
}

impl<'a> Input<'a> {
    pub fn new(s: &'a str) -> Self {
        let mut top = Vec::with_capacity(s.len() + 1);
        let (mut quote, mut depth) = (false, 0i32);
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            top.push(!quote && depth == 0);
            match bytes[i] {
                // `""` inside a text is a quote character, not its end
                b'"' if quote && bytes.get(i + 1) == Some(&b'"') => {
                    top.push(false);
                    i += 1;
                }
                b'"' => quote = !quote,
                b'{' | b'(' if !quote => depth += 1,
                b'}' | b')' if !quote => depth = (depth - 1).max(0),
                _ => {}
            }
            i += 1;
        }
        top.push(!quote && depth == 0);
        Input { s, top }
    }

    pub fn is_top(&self, i: usize) -> bool {
        self.top.get(i).copied().unwrap_or(false)
    }

    /// Byte offsets of `needle` at the top level (case-insensitive ASCII).
    pub fn find_top(&self, needle: &str) -> Vec<usize> {
        let (hay, n) = (self.s.as_bytes(), needle.as_bytes());
        let mut out = Vec::new();
        if n.is_empty() || hay.len() < n.len() {
            return out;
        }
        for i in 0..=hay.len() - n.len() {
            if self.top[i] && hay[i..i + n.len()].eq_ignore_ascii_case(n) {
                out.push(i);
            }
        }
        out
    }
}

impl Pattern {
    pub fn new(src: &str) -> Pattern {
        let chars: Vec<char> = src.chars().collect();
        let (mut pos, mut slots) = (0, 0);
        let els = parse_seq(&chars, &mut pos, &mut slots, &[]);
        Pattern { els, slots }
    }

    /// Calls `accept` for every way the whole input matches; stops at the first accepted one.
    pub fn matches<'a>(&self, input: &Input<'a>, accept: &mut dyn FnMut(&Caps<'a>) -> bool) -> bool {
        let mut caps = Caps { slots: vec![None; self.slots], mark: 0 };
        let cont = Cont { els: &self.els, next: None };
        let mut steps = 0u32;
        walk(&cont, input, 0, &mut caps, accept, &mut steps)
    }
}

fn parse_seq(c: &[char], pos: &mut usize, slots: &mut usize, stop: &[char]) -> Vec<El> {
    let mut out = Vec::new();
    let mut lit = String::new();
    let flush = |lit: &mut String, out: &mut Vec<El>| {
        if !lit.is_empty() {
            out.push(El::Lit(lit.to_lowercase()));
        }
        lit.clear();
    };
    while *pos < c.len() && !stop.contains(&c[*pos]) {
        match c[*pos] {
            '%' => {
                flush(&mut lit, &mut out);
                *pos += 1;
                while *pos < c.len() && c[*pos] != '%' {
                    *pos += 1;
                }
                *pos += 1;
                out.push(El::Slot(*slots));
                *slots += 1;
            }
            open @ ('[' | '(') => {
                flush(&mut lit, &mut out);
                *pos += 1;
                let close = if open == '[' { ']' } else { ')' };
                let mut arms = Vec::new();
                loop {
                    let mut tag = 0u32;
                    // `3:` in front of an arm tags it
                    let mut p = *pos;
                    let mut digits = String::new();
                    while p < c.len() && c[p].is_ascii_digit() {
                        digits.push(c[p]);
                        p += 1;
                    }
                    if !digits.is_empty() && p < c.len() && c[p] == ':' {
                        tag = digits.parse().unwrap_or(0);
                        *pos = p + 1;
                    }
                    arms.push((parse_seq(c, pos, slots, &['|', close]), tag));
                    if *pos < c.len() && c[*pos] == '|' {
                        *pos += 1;
                    } else {
                        break;
                    }
                }
                *pos += 1;
                if open == '[' {
                    arms.push((Vec::new(), 0));
                }
                out.push(El::Alt(arms));
            }
            '\\' => {
                *pos += 1;
                if *pos < c.len() {
                    lit.push(c[*pos]);
                    *pos += 1;
                }
            }
            ch if ch.is_whitespace() => {
                flush(&mut lit, &mut out);
                if !matches!(out.last(), Some(El::Space)) {
                    out.push(El::Space);
                }
                *pos += 1;
            }
            ch => {
                lit.push(ch);
                *pos += 1;
            }
        }
    }
    flush(&mut lit, &mut out);
    out
}

struct Cont<'p, 'c> {
    els: &'p [El],
    next: Option<&'c Cont<'p, 'c>>,
}

fn word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn skip_ws(s: &[u8], mut pos: usize) -> usize {
    while pos < s.len() && s[pos] == b' ' {
        pos += 1;
    }
    pos
}

fn walk<'a>(cont: &Cont<'_, '_>, input: &Input<'a>, pos: usize, caps: &mut Caps<'a>, accept: &mut dyn FnMut(&Caps<'a>) -> bool, steps: &mut u32) -> bool {
    *steps += 1;
    if *steps > 200_000 {
        return false;
    }
    let s = input.s.as_bytes();
    let Some((el, rest)) = cont.els.split_first() else {
        return match cont.next {
            Some(next) => walk(next, input, pos, caps, accept, steps),
            None => skip_ws(s, pos) == s.len() && accept(caps),
        };
    };
    let rest = Cont { els: rest, next: cont.next };
    match el {
        El::Lit(lit) => {
            let l = lit.as_bytes();
            if !input.is_top(pos) || pos + l.len() > s.len() || !s[pos..pos + l.len()].eq_ignore_ascii_case(l) {
                return false;
            }
            walk(&rest, input, pos + l.len(), caps, accept, steps)
        }
        El::Space => {
            let at = skip_ws(s, pos);
            // whole words only: `to` must not match the start of `tool`
            if at == pos && pos > 0 && pos < s.len() && word(s[pos - 1]) && word(s[pos]) {
                return false;
            }
            walk(&rest, input, at, caps, accept, steps)
        }
        El::Slot(index) => {
            let start = skip_ws(s, pos);
            if start >= s.len() || !input.is_top(start) {
                return false;
            }
            // Nothing else to match: the slot is the rest of the input
            let last = rest.els.is_empty() && rest.next.is_none();
            let ends: Box<dyn Iterator<Item = usize>> = if last {
                let mut end = s.len();
                while end > start && s[end - 1] == b' ' {
                    end -= 1;
                }
                Box::new(std::iter::once(end))
            } else {
                Box::new(start + 1..=s.len())
            };
            for end in ends {
                if end <= start || !input.is_top(end) || !input.s.is_char_boundary(end) || s[end - 1] == b' ' {
                    continue;
                }
                let boundary = end == s.len() || s[end] == b' ' || !word(s[end]) || !word(s[end - 1]);
                if !boundary {
                    continue;
                }
                let old = caps.slots[*index];
                caps.slots[*index] = Some(&input.s[start..end]);
                if walk(&rest, input, end, caps, accept, steps) {
                    return true;
                }
                caps.slots[*index] = old;
            }
            false
        }
        El::Alt(arms) => {
            for (arm, tag) in arms {
                let saved = caps.mark;
                caps.mark |= tag;
                let inner = Cont { els: arm, next: Some(&rest) };
                if walk(&inner, input, pos, caps, accept, steps) {
                    return true;
                }
                caps.mark = saved;
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(pat: &str, input: &str) -> Option<(Vec<Option<String>>, u32)> {
        let p = Pattern::new(pat);
        let mut out = None;
        p.matches(&Input::new(input), &mut |c| {
            out = Some((c.slots.iter().map(|s| s.map(str::to_owned)).collect(), c.mark));
            true
        });
        out
    }

    #[test]
    fn literals_and_slots() {
        let (s, _) = caps("send %a% [to %b%]", "send \"hi to all\" to player").unwrap();
        assert_eq!(s, vec![Some("\"hi to all\"".into()), Some("player".into())]);
        let (s, _) = caps("send %a% [to %b%]", "send \"hi\"").unwrap();
        assert_eq!(s, vec![Some("\"hi\"".into()), None]);
        assert!(caps("send %a% [to %b%]", "sending \"hi\"").is_none());
    }

    #[test]
    fn word_boundaries_and_tags() {
        // `to` inside `tool` is not the keyword
        let (s, _) = caps("set %a% to %b%", "set player's tool to stone").unwrap();
        assert_eq!(s[0].as_deref(), Some("player's tool"));
        let (_, mark) = caps("strike lightning [1:effect] at %a%", "strike lightning effect at player").unwrap();
        assert_eq!(mark, 1);
        let (s, mark) = caps("%a% (is|are) [2:not] set", "{x::%player%} is not set").unwrap();
        assert_eq!((s[0].as_deref(), mark), (Some("{x::%player%}"), 2));
        let (s, _) = caps("%a%'s (name|1:uuid)", "player's name").unwrap();
        assert_eq!(s[0].as_deref(), Some("player"));
    }

    #[test]
    fn backtracks_to_a_later_split() {
        let p = Pattern::new("teleport %a% to %b%");
        let mut seen = Vec::new();
        p.matches(&Input::new("teleport player to location of player to north"), &mut |c| {
            seen.push((c.get(0).unwrap().to_owned(), c.get(1).unwrap().to_owned()));
            false
        });
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].0, "player");
    }

    #[test]
    fn nesting_is_respected() {
        let (s, _) = caps("%a% + %b%", "(1 + 2) + {x::a + b}").unwrap();
        assert_eq!(s, vec![Some("(1 + 2)".into()), Some("{x::a + b}".into())]);
    }
}

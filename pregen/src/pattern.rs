//! Generation order. Every pattern walks the selection's chunk bounding box;
//! the task drops positions outside the shape. Positions are deterministic so a
//! saved index resumes at the same place.

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pattern {
    Region,
    Concentric,
    Loop,
    Spiral,
    Csv,
}

impl Pattern {
    pub const NAMES: [&str; 5] = ["region", "concentric", "loop", "spiral", "csv"];

    pub fn parse(s: &str) -> Option<Pattern> {
        Some(match s.to_lowercase().as_str() {
            "region" => Pattern::Region,
            "concentric" => Pattern::Concentric,
            "loop" => Pattern::Loop,
            "spiral" => Pattern::Spiral,
            "csv" => Pattern::Csv,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        Pattern::NAMES[self as usize]
    }
}

pub type Positions = Box<dyn Iterator<Item = (i32, i32)> + Send>;

/// All positions of `pattern` over the inclusive chunk box, `center` in chunks.
pub fn positions(pattern: Pattern, bounds: (i32, i32, i32, i32), center: (i32, i32), csv: &[(i32, i32)]) -> Positions {
    let (x0, z0, x1, z1) = bounds;
    match pattern {
        Pattern::Loop => Box::new((x0..=x1).flat_map(move |x| (z0..=z1).map(move |z| (x, z)))),
        Pattern::Region => Box::new((x0 >> 5..=x1 >> 5).flat_map(move |rx| {
            (z0 >> 5..=z1 >> 5).flat_map(move |rz| {
                let (ax, bx) = ((rx << 5).max(x0), ((rx << 5) + 31).min(x1));
                let (az, bz) = ((rz << 5).max(z0), ((rz << 5) + 31).min(z1));
                (az..=bz).flat_map(move |z| (ax..=bx).map(move |x| (x, z)))
            })
        })),
        Pattern::Concentric => {
            let (cx, cz) = center;
            let rings = (cx - x0).max(x1 - cx).max(cz - z0).max(z1 - cz).max(0);
            Box::new((0..=rings).flat_map(move |r| ring(cx, cz, r)).filter(move |&(x, z)| {
                x >= x0 && x <= x1 && z >= z0 && z <= z1
            }))
        }
        Pattern::Spiral => {
            let (cx, cz) = center;
            let rings = (cx - x0).max(x1 - cx).max(cz - z0).max(z1 - cz).max(0);
            let side = 2 * i64::from(rings) + 1;
            Box::new(Spiral::new(cx, cz).take((side * side) as usize).filter(move |&(x, z)| {
                x >= x0 && x <= x1 && z >= z0 && z <= z1
            }))
        }
        Pattern::Csv => Box::new(csv.to_vec().into_iter()),
    }
}

/// Square ring at distance `r`, walked clockwise as straight runs.
fn ring(cx: i32, cz: i32, r: i32) -> Box<dyn Iterator<Item = (i32, i32)> + Send> {
    if r == 0 {
        return Box::new(std::iter::once((cx, cz)));
    }
    let (l, t, rr, b) = (cx - r, cz - r, cx + r, cz + r);
    let top = (l..rr).map(move |x| (x, t));
    let right = (t..b).map(move |z| (rr, z));
    let bottom = (l + 1..=rr).rev().map(move |x| (x, b));
    let left = (t + 1..=b).rev().map(move |z| (l, z));
    Box::new(top.chain(right).chain(bottom).chain(left))
}

/// Outward square spiral: 1 east, 1 south, 2 west, 2 north, 3 east ...
struct Spiral {
    pos: (i32, i32),
    dir: usize,
    leg: i32,
    step: i32,
    turns: u8,
    started: bool,
}

impl Spiral {
    fn new(x: i32, z: i32) -> Spiral {
        Spiral { pos: (x, z), dir: 0, leg: 1, step: 0, turns: 0, started: false }
    }
}

impl Iterator for Spiral {
    type Item = (i32, i32);
    fn next(&mut self) -> Option<(i32, i32)> {
        if !self.started {
            self.started = true;
            return Some(self.pos);
        }
        const DIRS: [(i32, i32); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];
        let (dx, dz) = DIRS[self.dir];
        self.pos = (self.pos.0 + dx, self.pos.1 + dz);
        self.step += 1;
        if self.step == self.leg {
            self.step = 0;
            self.dir = (self.dir + 1) % 4;
            self.turns += 1;
            if self.turns == 2 {
                self.turns = 0;
                self.leg += 1;
            }
        }
        Some(self.pos)
    }
}

/// Parse `x,z` chunk coordinates (one per line, `#` comments).
pub fn parse_csv(text: &str) -> Vec<(i32, i32)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            let mut it = l.split([',', ';', ' ']).filter(|s| !s.is_empty());
            Some((it.next()?.trim().parse().ok()?, it.next()?.trim().parse().ok()?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_pattern_covers_the_box_once() {
        let b = (-7, -3, 40, 9);
        let want: HashSet<_> = (b.0..=b.2).flat_map(|x| (b.1..=b.3).map(move |z| (x, z))).collect();
        for p in [Pattern::Loop, Pattern::Region, Pattern::Concentric, Pattern::Spiral] {
            let got: Vec<_> = positions(p, b, (5, 2), &[]).collect();
            assert_eq!(got.len(), want.len(), "{p:?}");
            assert_eq!(got.into_iter().collect::<HashSet<_>>(), want, "{p:?}");
        }
    }

    #[test]
    fn csv_lines() {
        assert_eq!(parse_csv("# x,z\n1,2\n-3; 4\nbad\n"), vec![(1, 2), (-3, 4)]);
    }
}

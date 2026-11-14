//! Regions, shapes and block patterns. No server types here, so all of it is unit-tested.

pub type P = (i32, i32, i32);

/// A cuboid between two corners, both included.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub min: P,
    pub max: P,
}

impl Region {
    pub fn new(a: P, b: P) -> Self {
        Region { min: (a.0.min(b.0), a.1.min(b.1), a.2.min(b.2)), max: (a.0.max(b.0), a.1.max(b.1), a.2.max(b.2)) }
    }

    pub fn size(&self) -> (i64, i64, i64) {
        (
            i64::from(self.max.0) - i64::from(self.min.0) + 1,
            i64::from(self.max.1) - i64::from(self.min.1) + 1,
            i64::from(self.max.2) - i64::from(self.min.2) + 1,
        )
    }

    pub fn volume(&self) -> i64 {
        let (x, y, z) = self.size();
        x.saturating_mul(y).saturating_mul(z)
    }

    /// Every block, bottom layer first.
    pub fn blocks(&self) -> impl Iterator<Item = P> + use<> {
        let r = *self;
        (r.min.1..=r.max.1).flat_map(move |y| (r.min.0..=r.max.0).flat_map(move |x| (r.min.2..=r.max.2).map(move |z| (x, y, z))))
    }

    /// Every block, one 16 x 16 chunk column after the other and bottom up inside each: the order
    /// edits are worked in. A list made this way needs no sorting (sorting 200,000 positions
    /// inside the command cost a 75 ms tick in the lab).
    pub fn chunk_order(&self) -> impl Iterator<Item = P> + use<> {
        let r = *self;
        (r.min.0 >> 4..=r.max.0 >> 4).flat_map(move |cx| {
            (r.min.2 >> 4..=r.max.2 >> 4).flat_map(move |cz| {
                let (x0, x1) = (r.min.0.max(cx << 4), r.max.0.min((cx << 4) + 15));
                let (z0, z1) = (r.min.2.max(cz << 4), r.max.2.min((cz << 4) + 15));
                (r.min.1..=r.max.1).flat_map(move |y| (x0..=x1).flat_map(move |x| (z0..=z1).map(move |z| (x, y, z))))
            })
        })
    }

    fn on_side(&self, p: P) -> bool {
        p.0 == self.min.0 || p.0 == self.max.0 || p.2 == self.min.2 || p.2 == self.max.2
    }

    /// The four vertical sides.
    pub fn walls(&self) -> impl Iterator<Item = P> + use<> {
        let r = *self;
        self.chunk_order().filter(move |p| r.on_side(*p))
    }

    /// All six sides.
    pub fn faces(&self) -> impl Iterator<Item = P> + use<> {
        let r = *self;
        self.chunk_order().filter(move |p| r.on_side(*p) || p.1 == r.min.1 || p.1 == r.max.1)
    }

    /// Grow (`n` > 0) or shrink (`n` < 0) towards `dir`. Shrinking stops at one block.
    pub fn grown(&self, dir: Dir, n: i32) -> Region {
        let (mut min, mut max) = (self.min, self.max);
        let (lo, hi): (&mut i32, &mut i32) = match dir {
            Dir::West | Dir::East => (&mut min.0, &mut max.0),
            Dir::Down | Dir::Up => (&mut min.1, &mut max.1),
            Dir::North | Dir::South => (&mut min.2, &mut max.2),
        };
        if dir.positive() {
            *hi = hi.saturating_add(n).max(*lo);
        } else {
            *lo = lo.saturating_sub(n).min(*hi);
        }
        Region { min, max }
    }

    pub fn shifted(&self, dir: Dir, n: i32) -> Region {
        let (dx, dy, dz) = dir.step();
        let mv = |p: P| (p.0.saturating_add(dx * n), p.1.saturating_add(dy * n), p.2.saturating_add(dz * n));
        Region { min: mv(self.min), max: mv(self.max) }
    }

    /// Moved by `d` blocks on each axis.
    pub fn translated(&self, d: P) -> Region {
        Region { min: add(self.min, d), max: add(self.max, d) }
    }

    pub fn contains(&self, p: P) -> bool {
        (self.min.0..=self.max.0).contains(&p.0) && (self.min.1..=self.max.1).contains(&p.1) && (self.min.2..=self.max.2).contains(&p.2)
    }

    /// The middle block; two blocks along an axis whose length is even.
    pub fn center(&self) -> Region {
        let mid = |lo: i32, hi: i32| {
            let span = i64::from(hi) - i64::from(lo);
            ((i64::from(lo) + span / 2) as i32, (i64::from(lo) + (span + 1) / 2) as i32)
        };
        let (x, y, z) = (mid(self.min.0, self.max.0), mid(self.min.1, self.max.1), mid(self.min.2, self.max.2));
        Region { min: (x.0, y.0, z.0), max: (x.1, y.1, z.1) }
    }

    /// Grow (`n` > 0) or shrink (`n` < 0) on every side: sideways, up and down, or both.
    pub fn outset(&self, n: i32, sideways: bool, vertical: bool) -> Region {
        let mut r = *self;
        for dir in [Dir::North, Dir::South, Dir::East, Dir::West, Dir::Up, Dir::Down] {
            if if matches!(dir, Dir::Up | Dir::Down) { vertical } else { sideways } {
                r = r.grown(dir, n);
            }
        }
        r
    }

    /// How far a copy has to move towards `dir` to sit right next to the original (//stack).
    pub fn stride(&self, dir: Dir) -> P {
        let (x, y, z) = self.size();
        let (dx, dy, dz) = dir.step();
        let clamp = |v: i64| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        (clamp(x * i64::from(dx)), clamp(y * i64::from(dy)), clamp(z * i64::from(dz)))
    }

    /// Inside the world's build height, or `None` when nothing is left.
    pub fn clamped_y(&self, lo: i32, hi: i32) -> Option<Region> {
        let (a, b) = (self.min.1.max(lo), self.max.1.min(hi));
        (a <= b).then(|| Region { min: (self.min.0, a, self.min.2), max: (self.max.0, b, self.max.2) })
    }
}

pub fn add(p: P, d: P) -> P {
    (p.0.saturating_add(d.0), p.1.saturating_add(d.1), p.2.saturating_add(d.2))
}

/// `d` taken `k` times.
pub fn times(d: P, k: i32) -> P {
    (d.0.saturating_mul(k), d.1.saturating_mul(k), d.2.saturating_mul(k))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dir {
    North,
    South,
    East,
    West,
    Up,
    Down,
}

impl Dir {
    pub fn parse(s: &str) -> Option<Dir> {
        Some(match s.to_lowercase().as_str() {
            "n" | "north" => Dir::North,
            "s" | "south" => Dir::South,
            "e" | "east" => Dir::East,
            "w" | "west" => Dir::West,
            "u" | "up" => Dir::Up,
            "d" | "down" => Dir::Down,
            _ => return None,
        })
    }

    /// Where a player with this yaw and pitch is looking (Minecraft: yaw 0 = south, pitch -90 = up).
    pub fn facing(yaw: f32, pitch: f32) -> Dir {
        if pitch <= -60.0 {
            return Dir::Up;
        }
        if pitch >= 60.0 {
            return Dir::Down;
        }
        match (yaw.rem_euclid(360.0) + 45.0) as i32 / 90 % 4 {
            0 => Dir::South,
            1 => Dir::West,
            2 => Dir::North,
            _ => Dir::East,
        }
    }

    pub fn step(self) -> P {
        match self {
            Dir::North => (0, 0, -1),
            Dir::South => (0, 0, 1),
            Dir::East => (1, 0, 0),
            Dir::West => (-1, 0, 0),
            Dir::Up => (0, 1, 0),
            Dir::Down => (0, -1, 0),
        }
    }

    fn positive(self) -> bool {
        matches!(self, Dir::South | Dir::East | Dir::Up)
    }
}

/// A cylinder standing on `base`: radius `r`, `height` layers. `hollow` keeps the shell only.
pub fn cylinder(base: P, r: i32, height: i32, hollow: bool) -> Vec<P> {
    let (r, height) = (r.clamp(0, 512), height.clamp(1, 4096));
    let inside = |dx: i32, dz: i32, rad: i32| i64::from(dx) * i64::from(dx) + i64::from(dz) * i64::from(dz) <= i64::from(rad) * i64::from(rad) + i64::from(rad);
    let mut out = Vec::new();
    for dy in 0..height {
        for dx in -r..=r {
            for dz in -r..=r {
                if !inside(dx, dz, r) {
                    continue;
                }
                // Shell: a neighbour in the layer is outside
                let edge = !inside(dx + 1, dz, r) || !inside(dx - 1, dz, r) || !inside(dx, dz + 1, r) || !inside(dx, dz - 1, r);
                if !hollow || edge {
                    out.push((base.0 + dx, base.1.saturating_add(dy), base.2 + dz));
                }
            }
        }
    }
    out
}

/// A ball around `center` with radius `r`. `hollow` keeps the shell only.
pub fn sphere(center: P, r: i32, hollow: bool) -> Vec<P> {
    let r = r.clamp(0, 256);
    let inside = |dx: i32, dy: i32, dz: i32| {
        let d = i64::from(dx) * i64::from(dx) + i64::from(dy) * i64::from(dy) + i64::from(dz) * i64::from(dz);
        d <= i64::from(r) * i64::from(r) + i64::from(r)
    };
    let mut out = Vec::new();
    for dy in -r..=r {
        for dx in -r..=r {
            for dz in -r..=r {
                if !inside(dx, dy, dz) {
                    continue;
                }
                let edge = !inside(dx + 1, dy, dz) || !inside(dx - 1, dy, dz) || !inside(dx, dy + 1, dz) || !inside(dx, dy - 1, dz) || !inside(dx, dy, dz + 1) || !inside(dx, dy, dz - 1);
                if !hollow || edge {
                    out.push((center.0 + dx, center.1.saturating_add(dy), center.2 + dz));
                }
            }
        }
    }
    out
}

/// `stone` -> `minecraft:stone`; `None` for anything that is not a block name.
pub fn block_name(s: &str) -> Option<String> {
    let s = s.trim().to_lowercase();
    let ok = |p: &str| !p.is_empty() && p.len() <= 64 && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    match s.split_once(':') {
        Some((ns, k)) if ok(ns) && ok(k) => Some(s),
        None if ok(&s) => Some(format!("minecraft:{s}")),
        _ => None,
    }
}

/// How many blocks a straight line between two corners has.
pub fn line_len(a: P, b: P) -> i64 {
    let d = |x: i32, y: i32| (i64::from(y) - i64::from(x)).abs();
    d(a.0, b.0).max(d(a.1, b.1)).max(d(a.2, b.2)) + 1
}

/// The blocks of a straight line from `a` to `b`, both included, one block per step along the
/// longest axis. Check `line_len` first: this allocates that many positions.
pub fn line(a: P, b: P) -> Vec<P> {
    let n = line_len(a, b) - 1;
    if n == 0 {
        return vec![a];
    }
    // Round half up: a + d * i / n
    let at = |from: i32, to: i32, i: i64| (i64::from(from) + (2 * (i64::from(to) - i64::from(from)) * i + n).div_euclid(2 * n)) as i32;
    (0..=n).map(|i| (at(a.0, b.0, i), at(a.1, b.1, i), at(a.2, b.2, i))).collect()
}

/// A square pyramid standing on `base` (the middle of its bottom layer), `size` layers high.
/// `hollow` keeps the outer ring of every layer.
pub fn pyramid(base: P, size: i32, hollow: bool) -> Vec<P> {
    let size = size.clamp(1, 256);
    let mut out = Vec::new();
    for dy in 0..size {
        let w = size - 1 - dy;
        for dx in -w..=w {
            for dz in -w..=w {
                if !hollow || dx.abs() == w || dz.abs() == w {
                    out.push((base.0 + dx, base.1.saturating_add(dy), base.2 + dz));
                }
            }
        }
    }
    out
}

/// Blocks that show a region to a player: the eight corners and, along every edge, a point
/// every few blocks (at most `per_edge` between two corners).
pub fn outline_marks(r: &Region, per_edge: i32) -> Vec<P> {
    let ticks = |lo: i32, hi: i32| -> Vec<i32> {
        let span = i64::from(hi) - i64::from(lo);
        let step = (span / i64::from(per_edge.max(0) + 1) + 1).max(1);
        let mut v: Vec<i32> = (1..).map(|i| i64::from(lo) + i * step).take_while(|x| *x < i64::from(hi)).map(|x| x as i32).collect();
        v.push(lo);
        v.push(hi);
        v
    };
    let mut out = Vec::new();
    for x in ticks(r.min.0, r.max.0) {
        for y in [r.min.1, r.max.1] {
            for z in [r.min.2, r.max.2] {
                out.push((x, y, z));
            }
        }
    }
    for y in ticks(r.min.1, r.max.1) {
        for x in [r.min.0, r.max.0] {
            for z in [r.min.2, r.max.2] {
                out.push((x, y, z));
            }
        }
    }
    for z in ticks(r.min.2, r.max.2) {
        for x in [r.min.0, r.max.0] {
            for y in [r.min.1, r.max.1] {
                out.push((x, y, z));
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The blocks inside a shape that //hollow empties. The box is `size` = (x, y, z) and
/// `solid[x + z * size.0 + y * size.0 * size.2]` says which blocks count as the object.
/// A block stays when it is within `thickness` steps of the outside: of anything not solid that
/// connects to the edge of the box, or of the edge itself. The rest is returned.
pub fn hollow(size: (usize, usize, usize), solid: &[bool], thickness: usize) -> Vec<usize> {
    let (w, h, l) = size;
    let n = w * h * l;
    if n == 0 || solid.len() != n {
        return Vec::new();
    }
    let thickness = thickness.max(1) as u32;
    let xyz = |i: usize| (i % w, i / (w * l), i / w % l);
    let on_edge = |i: usize| {
        let (x, y, z) = xyz(i);
        x == 0 || y == 0 || z == 0 || x == w - 1 || y == h - 1 || z == l - 1
    };
    let neighbours = |i: usize| {
        let (x, y, z) = xyz(i);
        [
            (x > 0).then(|| i - 1),
            (x + 1 < w).then(|| i + 1),
            (z > 0).then(|| i - w),
            (z + 1 < l).then(|| i + w),
            (y > 0).then(|| i - w * l),
            (y + 1 < h).then(|| i + w * l),
        ]
    };
    const UNKNOWN: u32 = u32::MAX;
    let mut dist = vec![UNKNOWN; n];
    let mut queue: Vec<usize> = Vec::new();
    // Outside: everything not solid that reaches the edge of the box
    for i in (0..n).filter(|i| on_edge(*i)) {
        if !solid[i] && dist[i] == UNKNOWN {
            dist[i] = 0;
            queue.push(i);
        }
    }
    let mut head = 0;
    while head < queue.len() {
        let i = queue[head];
        head += 1;
        for j in neighbours(i).into_iter().flatten() {
            if !solid[j] && dist[j] == UNKNOWN {
                dist[j] = 0;
                queue.push(j);
            }
        }
    }
    // Then count steps inwards from the outside and from the edge of the box
    for i in (0..n).filter(|i| on_edge(*i)) {
        if dist[i] == UNKNOWN {
            dist[i] = 1;
            queue.push(i);
        }
    }
    let mut head = 0;
    while head < queue.len() {
        let i = queue[head];
        head += 1;
        for j in neighbours(i).into_iter().flatten() {
            if dist[j] == UNKNOWN {
                dist[j] = dist[i] + 1;
                queue.push(j);
            }
        }
    }
    (0..n).filter(|i| dist[*i] > thickness).collect()
}

/// A cheap random number per position, the same on every run with the same seed.
pub fn roll(seed: u64, p: P) -> u64 {
    let mut x = seed ^ (p.0 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (p.1 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ (p.2 as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_basics() {
        let r = Region::new((3, 10, -2), (1, 8, 0));
        assert_eq!((r.min, r.max), ((1, 8, -2), (3, 10, 0)));
        assert_eq!(r.volume(), 27);
        assert_eq!(r.blocks().count(), 27);
        assert_eq!(r.blocks().next(), Some((1, 8, -2)));
        assert_eq!(r.walls().count(), 24);
        assert_eq!(r.faces().count(), 26);
        assert_eq!(Region::new((0, 0, 0), (0, 0, 0)).faces().count(), 1);
        assert_eq!(Region::new((i32::MIN, 0, 0), (i32::MAX, 0, 0)).volume(), 1i64 << 32);
    }

    #[test]
    fn chunk_order_is_the_work_order() {
        // The order the tick loop wants: chunk x, chunk z, y, x, z
        let key = |p: &P| (p.0 >> 4, p.2 >> 4, p.1, p.0, p.2);
        for r in [Region::new((-20, 60, -5), (17, 63, 33)), Region::new((0, 0, 0), (15, 0, 15)), Region::new((-1, -64, -1), (0, -60, 0)), Region::new((31, 5, -33), (32, 5, -32))] {
            let got: Vec<P> = r.chunk_order().collect();
            let mut want: Vec<P> = r.blocks().collect();
            want.sort_by_key(key);
            assert_eq!(got, want);
            assert_eq!(got.len() as i64, r.volume());
            let walls: Vec<P> = r.walls().collect();
            assert!(walls.windows(2).all(|w| key(&w[0]) < key(&w[1])));
            let faces: Vec<P> = r.faces().collect();
            assert!(faces.windows(2).all(|w| key(&w[0]) < key(&w[1])));
        }
    }

    #[test]
    fn grow_shift_clamp() {
        let r = Region::new((0, 0, 0), (2, 2, 2));
        assert_eq!(r.grown(Dir::Up, 3).max.1, 5);
        assert_eq!(r.grown(Dir::West, 2).min.0, -2);
        assert_eq!(r.grown(Dir::Up, -10), Region::new((0, 0, 0), (2, 0, 2)));
        assert_eq!(r.grown(Dir::Down, -10), Region::new((0, 2, 0), (2, 2, 2)));
        assert_eq!(r.shifted(Dir::North, 4), Region::new((0, 0, -4), (2, 2, -2)));
        assert_eq!(Region::new((0, -100, 0), (0, 500, 0)).clamped_y(-64, 319).map(|r| (r.min.1, r.max.1)), Some((-64, 319)));
        assert_eq!(Region::new((0, 400, 0), (0, 500, 0)).clamped_y(-64, 319), None);
    }

    #[test]
    fn directions() {
        assert_eq!(Dir::facing(0.0, 0.0), Dir::South);
        assert_eq!(Dir::facing(90.0, 0.0), Dir::West);
        assert_eq!(Dir::facing(-180.0, 0.0), Dir::North);
        assert_eq!(Dir::facing(-90.0, 10.0), Dir::East);
        assert_eq!(Dir::facing(10.0, -80.0), Dir::Up);
        assert_eq!(Dir::facing(10.0, 80.0), Dir::Down);
        assert_eq!(Dir::parse("U"), Some(Dir::Up));
        assert_eq!(Dir::parse("x"), None);
    }

    #[test]
    fn shapes() {
        assert_eq!(cylinder((0, 0, 0), 0, 3, false).len(), 3);
        let full = cylinder((0, 64, 0), 3, 1, false);
        let shell = cylinder((0, 64, 0), 3, 1, true);
        assert!(full.contains(&(0, 64, 0)) && !shell.contains(&(0, 64, 0)));
        assert!(shell.len() < full.len() && shell.iter().all(|p| full.contains(p)));
        assert!(full.iter().all(|p| p.0.abs() <= 3 && p.2.abs() <= 3));
        let ball = sphere((0, 0, 0), 4, false);
        let skin = sphere((0, 0, 0), 4, true);
        assert!(ball.contains(&(0, 0, 0)) && !skin.contains(&(0, 0, 0)) && skin.contains(&(4, 0, 0)));
        assert!(ball.len() > 200 && ball.len() < 400);
        assert_eq!(sphere((5, 5, 5), 0, true), vec![(5, 5, 5)]);
    }

    #[test]
    fn names() {
        assert_eq!(block_name("Stone"), Some("minecraft:stone".into()));
        assert_eq!(block_name("mod:thing_2"), Some("mod:thing_2".into()));
        assert_eq!(block_name("stone[axis=y]"), None);
        assert_eq!(block_name("a b"), None);
    }

    #[test]
    fn centre_outset_contains() {
        let r = Region::new((0, 0, 0), (4, 3, 0));
        assert_eq!(r.center(), Region::new((2, 1, 0), (2, 2, 0)));
        assert_eq!(r.center().volume(), 2);
        assert_eq!(Region::new((-3, 10, 7), (-3, 10, 7)).center(), Region::new((-3, 10, 7), (-3, 10, 7)));
        assert_eq!(Region::new((0, 0, 0), (1, 1, 1)).center().volume(), 8);
        assert_eq!(Region::new((-5, 0, 0), (-2, 0, 0)).center(), Region::new((-4, 0, 0), (-3, 0, 0)));
        assert_eq!(Region::new((i32::MIN, 0, 0), (i32::MAX, 0, 0)).center(), Region::new((-1, 0, 0), (0, 0, 0)));
        assert!(r.contains((4, 3, 0)) && r.contains((0, 0, 0)) && !r.contains((5, 0, 0)) && !r.contains((0, -1, 0)) && !r.contains((0, 0, 1)));
        let r = Region::new((0, 0, 0), (9, 9, 9));
        assert_eq!(r.outset(2, true, true), Region::new((-2, -2, -2), (11, 11, 11)));
        assert_eq!(r.outset(2, true, false), Region::new((-2, 0, -2), (11, 9, 11)));
        assert_eq!(r.outset(2, false, true), Region::new((0, -2, 0), (9, 11, 9)));
        assert_eq!(r.outset(-3, true, true), Region::new((3, 3, 3), (6, 6, 6)));
        // Shrinking past the middle stops at one block
        assert_eq!(r.outset(-50, true, true).volume(), 1);
    }

    #[test]
    fn stack_and_move() {
        let r = Region::new((10, 64, 20), (12, 67, 20));
        assert_eq!(r.stride(Dir::East), (3, 0, 0));
        assert_eq!(r.stride(Dir::West), (-3, 0, 0));
        assert_eq!(r.stride(Dir::Up), (0, 4, 0));
        assert_eq!(r.stride(Dir::North), (0, 0, -1));
        // The second copy towards the west sits two strides away and does not touch the first
        let first = r.translated(times(r.stride(Dir::West), 1));
        let second = r.translated(times(r.stride(Dir::West), 2));
        assert_eq!(first, Region::new((7, 64, 20), (9, 67, 20)));
        assert_eq!(second, Region::new((4, 64, 20), (6, 67, 20)));
        assert!(r.blocks().all(|p| !first.contains(p)) && first.blocks().all(|p| !second.contains(p)));
        assert_eq!(add((1, 2, 3), times((0, -1, 2), 3)), (1, -1, 9));
        assert_eq!(add((i32::MAX, 0, 0), (5, 0, 0)).0, i32::MAX);
        assert_eq!(times((i32::MAX, 0, 0), 3).0, i32::MAX);
        // Moving by less than the size: what is left behind is the part the copy does not cover
        let moved = r.translated(times(Dir::East.step(), 2));
        assert_eq!(moved, Region::new((12, 64, 20), (14, 67, 20)));
        assert_eq!(r.blocks().filter(|p| !moved.contains(*p)).count(), 8);
        let far = r.translated(times(Dir::Up.step(), 10));
        assert_eq!(r.blocks().filter(|p| !far.contains(*p)).count(), 12);
    }

    #[test]
    fn lines() {
        assert_eq!(line((0, 0, 0), (0, 0, 0)), vec![(0, 0, 0)]);
        assert_eq!(line((0, 0, 0), (3, 0, 0)), vec![(0, 0, 0), (1, 0, 0), (2, 0, 0), (3, 0, 0)]);
        assert_eq!(line((3, 0, 0), (0, 0, 0)), vec![(3, 0, 0), (2, 0, 0), (1, 0, 0), (0, 0, 0)]);
        assert_eq!(line((0, 0, 0), (2, 2, -2)), vec![(0, 0, 0), (1, 1, -1), (2, 2, -2)]);
        let l = line((0, 64, 0), (10, 67, -4));
        assert_eq!((l.len(), l[0], l[10]), (11, (0, 64, 0), (10, 67, -4)));
        // No gaps: every step moves at most one block on each axis
        assert!(l.windows(2).all(|w| (w[1].0 - w[0].0).abs() <= 1 && (w[1].1 - w[0].1).abs() <= 1 && (w[1].2 - w[0].2).abs() <= 1));
        assert_eq!(line_len((0, 0, 0), (10, 67, -4)), 68);
        assert_eq!(line_len((i32::MIN, 0, 0), (i32::MAX, 0, 0)), 1i64 << 32);
    }

    #[test]
    fn pyramids() {
        assert_eq!(pyramid((0, 0, 0), 1, false), vec![(0, 0, 0)]);
        let full = pyramid((0, 64, 0), 3, false);
        assert_eq!(full.len(), 25 + 9 + 1);
        assert!(full.contains(&(2, 64, -2)) && full.contains(&(0, 66, 0)) && !full.contains(&(2, 65, 0)));
        let shell = pyramid((0, 64, 0), 3, true);
        assert_eq!(shell.len(), 16 + 8 + 1);
        assert!(!shell.contains(&(0, 64, 0)) && shell.contains(&(0, 66, 0)) && shell.iter().all(|p| full.contains(p)));
    }

    #[test]
    fn outline() {
        assert_eq!(outline_marks(&Region::new((5, 5, 5), (5, 5, 5)), 6), vec![(5, 5, 5)]);
        let small = outline_marks(&Region::new((0, 0, 0), (2, 2, 2)), 6);
        // Every block on an edge of a 3x3x3 box: 8 corners + 12 edge middles
        assert_eq!(small.len(), 20);
        assert!(small.contains(&(0, 0, 0)) && small.contains(&(2, 2, 2)) && small.contains(&(1, 0, 0)) && !small.contains(&(1, 1, 0)) && !small.contains(&(1, 1, 1)));
        let r = Region::new((0, 64, 0), (99, 70, 199));
        let big = outline_marks(&r, 6);
        assert!(big.len() <= 8 + 12 * 6, "{}", big.len());
        for corner in [(0, 64, 0), (99, 64, 0), (0, 70, 199), (99, 70, 199)] {
            assert!(big.contains(&corner));
        }
        // All of them lie on an edge: at least two coordinates are at an end of the box
        assert!(big.iter().all(|p| u8::from(p.0 == 0 || p.0 == 99) + u8::from(p.1 == 64 || p.1 == 70) + u8::from(p.2 == 0 || p.2 == 199) >= 2));
        assert_eq!(outline_marks(&r, 0).len(), 8);
        assert!(outline_marks(&Region::new((i32::MIN, 0, 0), (i32::MAX, 0, 0)), 6).len() <= 8);
    }

    #[test]
    fn hollowing() {
        let idx = |size: (usize, usize, usize), x: usize, y: usize, z: usize| x + z * size.0 + y * size.0 * size.2;
        // A solid 5x5x5 cube that fills the whole box: one block of wall stays
        let size = (5, 5, 5);
        let gone = hollow(size, &[true; 125], 1);
        assert_eq!(gone.len(), 27);
        assert!(gone.contains(&idx(size, 2, 2, 2)) && gone.contains(&idx(size, 1, 1, 1)) && !gone.contains(&idx(size, 0, 2, 2)));
        assert_eq!(hollow(size, &[true; 125], 2), vec![idx(size, 2, 2, 2)]);
        assert!(hollow(size, &[true; 125], 3).is_empty());
        // The same cube inside a 7x7x7 box of air: the same 27 blocks go
        let size = (7, 7, 7);
        let mut solid = vec![false; 343];
        for x in 1..6 {
            for y in 1..6 {
                for z in 1..6 {
                    solid[idx(size, x, y, z)] = true;
                }
            }
        }
        let gone = hollow(size, &solid, 1);
        assert_eq!(gone.len(), 27);
        assert!(gone.iter().all(|i| solid[*i]));
        assert!(gone.contains(&idx(size, 3, 3, 3)) && !gone.contains(&idx(size, 1, 3, 3)));
        // A tunnel of air from the edge to the middle keeps the blocks around it
        let mut holed = solid.clone();
        for x in 0..4 {
            holed[idx(size, x, 3, 3)] = false;
        }
        let gone = hollow(size, &holed, 1);
        assert!(!gone.contains(&idx(size, 3, 3, 3)) && !gone.contains(&idx(size, 2, 2, 3)) && !gone.contains(&idx(size, 4, 3, 3)));
        assert!(gone.contains(&idx(size, 4, 2, 2)));
        // Nothing solid, or a box of the wrong size: nothing to do
        assert!(hollow((3, 3, 3), &[false; 27], 1).is_empty());
        assert!(hollow((3, 3, 3), &[true; 5], 1).is_empty());
        assert!(hollow((0, 3, 3), &[], 1).is_empty());
        assert_eq!(hollow((3, 3, 3), &[true; 27], 0), vec![13]);
    }
}

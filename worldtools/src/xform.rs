//! Turning and mirroring a clipboard: where each block goes and what happens to the properties
//! that point somewhere (`facing`, `axis`, `rotation`, the `north/east/south/west` connections of
//! fences, walls and panes, stairs and rail `shape`, door `hinge`, `half`). Plain Rust.

use crate::geo::P;
use crate::spec::Props;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Axis {
    X,
    Y,
    Z,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Xf {
    /// Quarter turns clockwise seen from above (north -> east -> south -> west), 1 to 3.
    Turn(u8),
    /// Mirror along this axis: `X` swaps east and west, `Y` up and down, `Z` north and south.
    Flip(Axis),
}

impl Xf {
    /// `90`, `180`, `270` (and negative or larger multiples of 90); `None` for anything else,
    /// including a turn that changes nothing.
    pub fn turn(degrees: i32) -> Option<Xf> {
        (degrees % 90 == 0).then_some(())?;
        match degrees.rem_euclid(360) / 90 {
            0 => None,
            q => Some(Xf::Turn(q as u8)),
        }
    }

    /// Where a block at `p` (relative to the clipboard origin) ends up.
    pub fn point(self, p: P) -> P {
        match self {
            Xf::Turn(q) => (0..q % 4).fold(p, |(x, y, z), _| (z.saturating_neg(), y, x)),
            Xf::Flip(Axis::X) => (p.0.saturating_neg(), p.1, p.2),
            Xf::Flip(Axis::Y) => (p.0, p.1.saturating_neg(), p.2),
            Xf::Flip(Axis::Z) => (p.0, p.1, p.2.saturating_neg()),
        }
    }

    /// A direction word after the transform; other words come back unchanged.
    pub fn dir<'a>(self, d: &'a str) -> &'a str {
        match self {
            Xf::Turn(q) => (0..q % 4).fold(d, |d, _| match d {
                "north" => "east",
                "east" => "south",
                "south" => "west",
                "west" => "north",
                other => other,
            }),
            Xf::Flip(Axis::X) => match d {
                "east" => "west",
                "west" => "east",
                other => other,
            },
            Xf::Flip(Axis::Y) => match d {
                "up" => "down",
                "down" => "up",
                other => other,
            },
            Xf::Flip(Axis::Z) => match d {
                "north" => "south",
                "south" => "north",
                other => other,
            },
        }
    }

    /// A horizontal mirror turns left into right.
    fn swaps_hands(self) -> bool {
        matches!(self, Xf::Flip(Axis::X | Axis::Z))
    }

    fn upside_down(self) -> bool {
        self == Xf::Flip(Axis::Y)
    }

    /// The `rotation` of signs, banners and heads: 16 steps, 0 = south, 4 = west.
    fn sixteenth(self, r: i32) -> i32 {
        match self {
            Xf::Turn(q) => (r + 4 * i32::from(q)).rem_euclid(16),
            Xf::Flip(Axis::X) => (16 - r).rem_euclid(16),
            Xf::Flip(Axis::Z) => (8 - r).rem_euclid(16),
            Xf::Flip(Axis::Y) => r,
        }
    }

    /// The properties of a block after the transform. The result can be a combination the block
    /// does not have (the caller looks it up and keeps the old state when it is not found).
    pub fn props(self, props: &Props) -> Props {
        const SIDES: [&str; 6] = ["north", "east", "south", "west", "up", "down"];
        let has = |k: &str| props.iter().any(|(pk, _)| pk == k);
        let swap = |v: &str, a: &str, b: &str| if v == a { b.to_owned() } else if v == b { a.to_owned() } else { v.to_owned() };
        props
            .iter()
            .map(|(k, v)| {
                let value = match k.as_str() {
                    // Connections: the value that was on the north side moves to wherever north went.
                    // A side whose partner the block lacks (a wall has `up` but no `down`) stays.
                    side if SIDES.contains(&side) => {
                        let from = SIDES.iter().find(|s| self.dir(s) == side).copied().unwrap_or(side);
                        match props.iter().find(|(pk, _)| pk == from) {
                            Some((_, moved)) if has(self.dir(side)) => moved.clone(),
                            _ => v.clone(),
                        }
                    }
                    "facing" | "vertical_direction" => self.dir(v).to_owned(),
                    "axis" => match (self, v.as_str()) {
                        (Xf::Turn(q), "x") if q % 2 == 1 => "z".to_owned(),
                        (Xf::Turn(q), "z") if q % 2 == 1 => "x".to_owned(),
                        _ => v.clone(),
                    },
                    "rotation" => v.parse::<i32>().ok().filter(|r| (0..16).contains(r)).map_or_else(|| v.clone(), |r| self.sixteenth(r).to_string()),
                    "shape" => self.shape(v),
                    // jigsaw, crafter: `north_up`, `up_east`
                    "orientation" => v.split('_').map(|w| self.dir(w)).collect::<Vec<_>>().join("_"),
                    "hinge" if self.swaps_hands() => swap(v, "left", "right"),
                    // chests: `type=left|right|single`; slabs: `type=top|bottom|double`
                    "type" if self.swaps_hands() => swap(v, "left", "right"),
                    "type" if self.upside_down() => swap(v, "top", "bottom"),
                    "half" if self.upside_down() => swap(&swap(v, "top", "bottom"), "upper", "lower"),
                    "face" | "attachment" if self.upside_down() => swap(v, "floor", "ceiling"),
                    "hanging" if self.upside_down() => swap(v, "true", "false"),
                    _ => v.clone(),
                };
                (k.clone(), value)
            })
            .collect()
    }

    /// `shape` of stairs (`inner_left`, `outer_right`, `straight`: relative to `facing`, so only a
    /// mirror changes it) and of rails (`north_south`, `ascending_east`, `south_west`).
    fn shape(self, v: &str) -> String {
        if let Some((side, hand)) = v.split_once('_').filter(|(_, h)| *h == "left" || *h == "right") {
            let hand = if self.swaps_hands() { if hand == "left" { "right" } else { "left" } } else { hand };
            return format!("{side}_{hand}");
        }
        if let Some(d) = v.strip_prefix("ascending_") {
            return format!("ascending_{}", self.dir(d));
        }
        match v.split_once('_') {
            Some((a, b)) if [a, b].iter().all(|w| ["north", "south", "east", "west"].contains(w)) => {
                let (a, b) = (self.dir(a), self.dir(b));
                // Rails name the north/south part first
                if (matches!(a, "east" | "west") && matches!(b, "north" | "south")) || (a, b) == ("south", "north") || (a, b) == ("west", "east") {
                    format!("{b}_{a}")
                } else {
                    format!("{a}_{b}")
                }
            }
            _ => v.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pairs: &[(&str, &str)]) -> Props {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn turns_parse() {
        assert_eq!(Xf::turn(90), Some(Xf::Turn(1)));
        assert_eq!(Xf::turn(180), Some(Xf::Turn(2)));
        assert_eq!(Xf::turn(270), Some(Xf::Turn(3)));
        assert_eq!(Xf::turn(-90), Some(Xf::Turn(3)));
        assert_eq!(Xf::turn(450), Some(Xf::Turn(1)));
        assert_eq!(Xf::turn(0), None);
        assert_eq!(Xf::turn(360), None);
        assert_eq!(Xf::turn(45), None);
    }

    #[test]
    fn points() {
        // One block north of the origin (z = -1) goes east (x = +1), then south, then west
        assert_eq!(Xf::Turn(1).point((0, 5, -1)), (1, 5, 0));
        assert_eq!(Xf::Turn(2).point((0, 5, -1)), (0, 5, 1));
        assert_eq!(Xf::Turn(3).point((0, 5, -1)), (-1, 5, 0));
        assert_eq!(Xf::Turn(1).point((2, 0, 3)), (-3, 0, 2));
        // Four quarter turns, or the same flip twice, change nothing
        let q = (7, -3, 11);
        assert_eq!((0..4).fold(q, |q, _| Xf::Turn(1).point(q)), q);
        assert_eq!(Xf::Turn(3).point(Xf::Turn(1).point(q)), q);
        assert_eq!(Xf::Turn(2).point(q), Xf::Turn(1).point(Xf::Turn(1).point(q)));
        for axis in [Axis::X, Axis::Y, Axis::Z] {
            assert_eq!(Xf::Flip(axis).point(Xf::Flip(axis).point(q)), q);
        }
        assert_eq!(Xf::Flip(Axis::X).point(q), (-7, -3, 11));
        assert_eq!(Xf::Flip(Axis::Y).point(q), (7, 3, 11));
        assert_eq!(Xf::Flip(Axis::Z).point(q), (7, -3, -11));
        assert_eq!(Xf::Flip(Axis::X).point((i32::MIN, 0, 0)).0, i32::MAX);
    }

    #[test]
    fn facing_axis_rotation() {
        let r = Xf::Turn(1);
        assert_eq!(r.props(&p(&[("facing", "north"), ("half", "top")])), p(&[("facing", "east"), ("half", "top")]));
        assert_eq!(Xf::Turn(2).props(&p(&[("facing", "north")])), p(&[("facing", "south")]));
        assert_eq!(Xf::Turn(3).props(&p(&[("facing", "north")])), p(&[("facing", "west")]));
        assert_eq!(r.props(&p(&[("facing", "up")])), p(&[("facing", "up")]));
        assert_eq!(r.props(&p(&[("axis", "x")])), p(&[("axis", "z")]));
        assert_eq!(r.props(&p(&[("axis", "y")])), p(&[("axis", "y")]));
        assert_eq!(Xf::Turn(2).props(&p(&[("axis", "x")])), p(&[("axis", "x")]));
        assert_eq!(Xf::Flip(Axis::X).props(&p(&[("axis", "x")])), p(&[("axis", "x")]));
        // Signs: 0 = south, 4 = west, 8 = north, 12 = east
        assert_eq!(r.props(&p(&[("rotation", "0")])), p(&[("rotation", "4")]));
        assert_eq!(r.props(&p(&[("rotation", "14")])), p(&[("rotation", "2")]));
        assert_eq!(Xf::Flip(Axis::X).props(&p(&[("rotation", "4")])), p(&[("rotation", "12")]));
        assert_eq!(Xf::Flip(Axis::X).props(&p(&[("rotation", "0")])), p(&[("rotation", "0")]));
        assert_eq!(Xf::Flip(Axis::Z).props(&p(&[("rotation", "0")])), p(&[("rotation", "8")]));
        assert_eq!(Xf::Flip(Axis::Z).props(&p(&[("rotation", "4")])), p(&[("rotation", "4")]));
        assert_eq!(Xf::Flip(Axis::Z).props(&p(&[("rotation", "13")])), p(&[("rotation", "11")]));
        assert_eq!(r.props(&p(&[("rotation", "junk")])), p(&[("rotation", "junk")]));
        assert_eq!(Xf::Flip(Axis::X).props(&p(&[("facing", "east")])), p(&[("facing", "west")]));
        assert_eq!(Xf::Flip(Axis::X).props(&p(&[("facing", "north")])), p(&[("facing", "north")]));
        assert_eq!(Xf::Flip(Axis::Y).props(&p(&[("facing", "up")])), p(&[("facing", "down")]));
        assert_eq!(Xf::Flip(Axis::Y).props(&p(&[("vertical_direction", "down")])), p(&[("vertical_direction", "up")]));
    }

    #[test]
    fn connections() {
        // A fence joined to the north and east
        let fence = p(&[("east", "true"), ("north", "true"), ("south", "false"), ("waterlogged", "false"), ("west", "false")]);
        let turned = Xf::Turn(1).props(&fence);
        assert_eq!(turned, p(&[("east", "true"), ("north", "false"), ("south", "true"), ("waterlogged", "false"), ("west", "false")]));
        let flipped = Xf::Flip(Axis::X).props(&fence);
        assert_eq!(flipped, p(&[("east", "false"), ("north", "true"), ("south", "false"), ("waterlogged", "false"), ("west", "true")]));
        // Walls: `up` has no `down` to swap with, and the side values are words
        let wall = p(&[("east", "low"), ("north", "tall"), ("south", "none"), ("up", "true"), ("west", "none")]);
        assert_eq!(Xf::Turn(2).props(&wall), p(&[("east", "none"), ("north", "none"), ("south", "tall"), ("up", "true"), ("west", "low")]));
        assert_eq!(Xf::Flip(Axis::Y).props(&wall), wall);
        // Vines have both
        let vine = p(&[("down", "false"), ("north", "true"), ("up", "true")]);
        assert_eq!(Xf::Flip(Axis::Y).props(&vine), p(&[("down", "true"), ("north", "true"), ("up", "false")]));
        // Four quarter turns bring everything back
        assert_eq!((0..4).fold(fence.clone(), |f, _| Xf::Turn(1).props(&f)), fence);
        assert_eq!(Xf::Turn(3).props(&Xf::Turn(1).props(&wall)), wall);
    }

    #[test]
    fn shapes_and_hands() {
        let stair = p(&[("facing", "north"), ("half", "bottom"), ("shape", "inner_left")]);
        assert_eq!(Xf::Turn(1).props(&stair), p(&[("facing", "east"), ("half", "bottom"), ("shape", "inner_left")]));
        assert_eq!(Xf::Flip(Axis::X).props(&stair), p(&[("facing", "north"), ("half", "bottom"), ("shape", "inner_right")]));
        assert_eq!(Xf::Flip(Axis::Z).props(&stair), p(&[("facing", "south"), ("half", "bottom"), ("shape", "inner_right")]));
        assert_eq!(Xf::Flip(Axis::Y).props(&stair), p(&[("facing", "north"), ("half", "top"), ("shape", "inner_left")]));
        assert_eq!(Xf::Flip(Axis::X).props(&p(&[("shape", "straight")])), p(&[("shape", "straight")]));
        // Rails
        let rail = |s: &str| p(&[("shape", s)]);
        assert_eq!(Xf::Turn(1).props(&rail("north_south")), rail("east_west"));
        assert_eq!(Xf::Turn(2).props(&rail("north_south")), rail("north_south"));
        assert_eq!(Xf::Turn(1).props(&rail("east_west")), rail("north_south"));
        assert_eq!(Xf::Turn(1).props(&rail("ascending_north")), rail("ascending_east"));
        assert_eq!(Xf::Turn(1).props(&rail("south_east")), rail("south_west"));
        assert_eq!(Xf::Turn(1).props(&rail("south_west")), rail("north_west"));
        assert_eq!(Xf::Turn(1).props(&rail("north_west")), rail("north_east"));
        assert_eq!(Xf::Turn(1).props(&rail("north_east")), rail("south_east"));
        assert_eq!(Xf::Flip(Axis::X).props(&rail("south_east")), rail("south_west"));
        assert_eq!(Xf::Flip(Axis::Z).props(&rail("south_east")), rail("north_east"));
        assert_eq!(Xf::Flip(Axis::X).props(&rail("ascending_east")), rail("ascending_west"));
        // Doors, chests, slabs, levers, lanterns
        let door = p(&[("facing", "east"), ("half", "lower"), ("hinge", "left")]);
        assert_eq!(Xf::Flip(Axis::Z).props(&door), p(&[("facing", "east"), ("half", "lower"), ("hinge", "right")]));
        assert_eq!(Xf::Flip(Axis::Y).props(&door), p(&[("facing", "east"), ("half", "upper"), ("hinge", "left")]));
        assert_eq!(Xf::Turn(1).props(&door), p(&[("facing", "south"), ("half", "lower"), ("hinge", "left")]));
        assert_eq!(Xf::Flip(Axis::X).props(&p(&[("type", "left")])), p(&[("type", "right")]));
        assert_eq!(Xf::Flip(Axis::X).props(&p(&[("type", "top")])), p(&[("type", "top")]));
        assert_eq!(Xf::Flip(Axis::Y).props(&p(&[("type", "top")])), p(&[("type", "bottom")]));
        assert_eq!(Xf::Flip(Axis::Y).props(&p(&[("type", "double")])), p(&[("type", "double")]));
        assert_eq!(Xf::Flip(Axis::Y).props(&p(&[("face", "floor")])), p(&[("face", "ceiling")]));
        assert_eq!(Xf::Flip(Axis::Y).props(&p(&[("hanging", "true")])), p(&[("hanging", "false")]));
        assert_eq!(Xf::Turn(1).props(&p(&[("orientation", "north_up")])), p(&[("orientation", "east_up")]));
        assert_eq!(Xf::Flip(Axis::Y).props(&p(&[("orientation", "up_north")])), p(&[("orientation", "down_north")]));
        // Things that point nowhere are left alone
        let plain = p(&[("lit", "true"), ("power", "7"), ("waterlogged", "false")]);
        for xf in [Xf::Turn(1), Xf::Turn(2), Xf::Flip(Axis::X), Xf::Flip(Axis::Y), Xf::Flip(Axis::Z)] {
            assert_eq!(xf.props(&plain), plain);
        }
    }
}

//! Animations, worked out once when a model loads: for every bone that moves, its turn, shift
//! and size at every tick. While the server runs, an animation is a table to look things up in.
//!
//! Blockbench's keyframes are written the way Bedrock sees a model: turns around X and Y and
//! shifts along X go the other way than in the model itself. That is undone here, so the
//! numbers in a track can be added to a bone as they are.
use crate::bbmodel::{Channel, Interp, Loop, RawAnimation, RawAnimator, RawKey};
use crate::math::Vec3;

/// Animations run at the game's 20 ticks a second.
pub const TICKS: f32 = 20.0;
/// Longer animations are cut: ten minutes is far beyond anything a model does.
pub const MAX_TICKS: u32 = 12_000;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    /// Degrees, added to the bone's own turn. One entry when it never changes.
    pub rot: Option<Vec<Vec3>>,
    /// Model units (1/16 block), added to the bone's place.
    pub pos: Option<Vec<Vec3>>,
    /// Multiplied with the bone's size.
    pub scale: Option<Vec<Vec3>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Animation {
    pub name: String,
    pub looping: Loop,
    /// Length in ticks, at least 1.
    pub ticks: u32,
    /// By bone; `None` for a bone this animation leaves alone.
    pub tracks: Vec<Option<Track>>,
}

fn values(exprs: &[crate::molang::Expr; 3], time: f32) -> Vec3 {
    Vec3::new(exprs[0].eval(time), exprs[1].eval(time), exprs[2].eval(time))
}

/// One axis of a Bezier segment: the value where the curve is at `time`.
fn bezier_axis(t0: f32, v0: f32, t1: f32, v1: f32, t2: f32, v2: f32, t3: f32, v3: f32, time: f32) -> f32 {
    let at = |a: f32, b: f32, c: f32, d: f32, s: f32| {
        let r = 1.0 - s;
        r * r * r * a + 3.0 * r * r * s * b + 3.0 * r * s * s * c + s * s * s * d
    };
    // The handles may not run backwards in time
    let (t1, t2) = (t1.clamp(t0, t3), t2.clamp(t0, t3));
    let (mut low, mut high) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = (low + high) * 0.5;
        if at(t0, t1, t2, t3, mid) < time {
            low = mid;
        } else {
            high = mid;
        }
    }
    at(v0, v1, v2, v3, (low + high) * 0.5)
}

/// The value of one channel at `time` seconds. `keys` are that channel's, in time order.
fn at(keys: &[&RawKey], time: f32) -> Vec3 {
    let (first, last) = (keys[0], keys[keys.len() - 1]);
    if time <= first.time {
        return values(&first.pre, time);
    }
    if time >= last.time {
        return values(&last.post, time);
    }
    let i = keys.iter().rposition(|k| k.time <= time).unwrap_or(0);
    let (a, b) = (keys[i], keys[i + 1]);
    let span = b.time - a.time;
    if span <= 0.0 || a.interp == Interp::Step {
        return values(&a.post, time);
    }
    let u = (time - a.time) / span;
    let (from, to) = (values(&a.post, time), values(&b.pre, time));
    if a.interp == Interp::Bezier || b.interp == Interp::Bezier {
        let third = span / 3.0;
        let (rt, rv) = a.bezier.as_ref().map_or(([third; 3], [0.0; 3]), |z| (z.right_time, z.right_value));
        let (lt, lv) = b.bezier.as_ref().map_or(([-third; 3], [0.0; 3]), |z| (z.left_time, z.left_value));
        let axis = |n: usize| bezier_axis(a.time, from.get(n), a.time + rt[n], from.get(n) + rv[n], b.time + lt[n], to.get(n) + lv[n], b.time, to.get(n), time);
        return Vec3::new(axis(0), axis(1), axis(2));
    }
    if a.interp == Interp::Smooth || b.interp == Interp::Smooth {
        let before = if i > 0 { values(&keys[i - 1].post, time) } else { from };
        let after = keys.get(i + 2).map_or(to, |k| values(&k.pre, time));
        let spline = |p0: f32, p1: f32, p2: f32, p3: f32| 0.5 * ((2.0 * p1) + (p2 - p0) * u + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * u * u + (3.0 * p1 - p0 - 3.0 * p2 + p3) * u * u * u);
        return Vec3::new(spline(before.x, from.x, to.x, after.x), spline(before.y, from.y, to.y, after.y), spline(before.z, from.z, to.z, after.z));
    }
    from.lerp(to, u)
}

fn channel(animator: &RawAnimator, which: Channel, ticks: u32) -> Option<Vec<Vec3>> {
    let mut keys: Vec<&RawKey> = animator.keys.iter().filter(|k| k.channel == which).collect();
    if keys.is_empty() {
        return None;
    }
    keys.sort_by(|a, b| a.time.total_cmp(&b.time));
    let mut out: Vec<Vec3> = (0..=ticks)
        .map(|tick| {
            let v = at(&keys, tick as f32 / TICKS);
            match which {
                Channel::Rotation => Vec3::new(-v.x, -v.y, v.z),
                Channel::Position => Vec3::new(-v.x, v.y, v.z),
                Channel::Scale => v,
            }
        })
        .collect();
    if out.iter().all(|v| *v == out[0]) {
        out.truncate(1);
    }
    let rest = if which == Channel::Scale { Vec3::ONE } else { Vec3::ZERO };
    // A channel that never leaves the bone as it is costs nothing by not being there
    (out.len() > 1 || out[0] != rest).then_some(out)
}

/// Work an animation out. `bone_of` says which bone an animator moves.
pub fn bake(raw: &RawAnimation, bones: usize, bone_of: &dyn Fn(&RawAnimator) -> Option<usize>) -> Animation {
    let ticks = ((raw.length * TICKS).round() as u32).clamp(1, MAX_TICKS);
    let mut tracks: Vec<Option<Track>> = vec![None; bones];
    for animator in &raw.animators {
        let Some(bone) = bone_of(animator).filter(|b| *b < bones) else { continue };
        let track = Track { rot: channel(animator, Channel::Rotation, ticks), pos: channel(animator, Channel::Position, ticks), scale: channel(animator, Channel::Scale, ticks) };
        if track != Track::default() {
            tracks[bone] = Some(track);
        }
    }
    Animation { name: raw.name.clone(), looping: raw.looping, ticks, tracks }
}

fn sample(list: &[Vec3], time: f32) -> Vec3 {
    if list.len() == 1 {
        return list[0];
    }
    let last = list.len() - 1;
    let t = time.clamp(0.0, last as f32);
    let i = (t as usize).min(last);
    if i == last {
        return list[last];
    }
    list[i].lerp(list[i + 1], t - i as f32)
}

impl Track {
    /// Turn, shift and size at `time` ticks.
    pub fn at(&self, time: f32) -> (Vec3, Vec3, Vec3) {
        (self.rot.as_ref().map_or(Vec3::ZERO, |l| sample(l, time)), self.pos.as_ref().map_or(Vec3::ZERO, |l| sample(l, time)), self.scale.as_ref().map_or(Vec3::ONE, |l| sample(l, time)))
    }
}

impl Animation {
    /// How many numbers this animation keeps in memory (for `/models info`).
    pub fn samples(&self) -> usize {
        self.tracks.iter().flatten().map(|t| [&t.rot, &t.pos, &t.scale].into_iter().flatten().map(Vec::len).sum::<usize>()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bbmodel::{self, tests::sample as sample_file};

    fn baked(name: &str) -> Animation {
        let raw = bbmodel::parse(&sample_file()).unwrap();
        let a = raw.animations.iter().find(|a| a.name == name).unwrap();
        // Bones in the test: 0 = body, 1 = head, 2 = leg
        bake(a, 3, &|animator| ["body", "h_head", "leg"].iter().position(|n| *n == animator.name))
    }

    #[test]
    fn linear_keys_and_the_bedrock_signs() {
        let idle = baked("idle");
        assert_eq!(idle.ticks, 40);
        let head = idle.tracks[1].as_ref().unwrap();
        // Half a second in: half way to (20, -10, 0), with X and Y the other way round
        let (rot, pos, scale) = head.at(10.0);
        assert!((rot.x + 10.0).abs() < 1e-4 && (rot.y - 5.0).abs() < 1e-4 && rot.z == 0.0, "{rot:?}");
        assert_eq!((pos, scale), (Vec3::ZERO, Vec3::ONE));
        // Between ticks
        assert!((head.at(10.5).0.x + 10.5).abs() < 1e-4);
        // Past the end stays at the end
        assert_eq!(head.at(400.0).0, Vec3::ZERO);
    }

    #[test]
    fn a_formula_is_worked_out_for_every_tick() {
        let idle = baked("idle");
        let body = idle.tracks[0].as_ref().unwrap();
        // y = sin(time * 180) * 2: 2 at half a second, 0 at one second
        assert!((body.at(10.0).1.y - 2.0).abs() < 1e-4);
        assert!(body.at(20.0).1.y.abs() < 1e-4);
        // One number for the size means all three axes
        let s = body.at(20.0).2;
        assert!((s.x - 1.5).abs() < 1e-4 && s.x == s.y && s.y == s.z);
        assert!(idle.samples() > 80);
    }

    #[test]
    fn steps_jumps_and_what_is_not_understood() {
        let attack = baked("attack");
        assert_eq!(attack.ticks, 10);
        let leg = attack.tracks[2].as_ref().unwrap();
        // A step holds its value until the next key
        assert_eq!(leg.at(4.0).0.x, 0.0);
        // The key at 0.25 s arrives at 45 and leaves at 90: from there on it runs to 0 (the
        // last key's formula is not understood and counts as 0)
        assert_eq!(leg.at(5.0).0.x, -90.0);
        assert!((leg.at(7.5).0.x + 45.0).abs() < 1e-3);
        assert_eq!(leg.at(10.0).0.x, 0.0);
        assert!(attack.tracks[0].is_none() && attack.tracks[1].is_none());
    }

    #[test]
    fn smooth_and_bezier_pass_through_their_keys() {
        let json = serde_json::json!({ "elements": [], "outliner": [], "animations": [ { "name": "a", "loop": "hold", "length": 1.0, "animators": { "g": { "name": "b", "keyframes": [
            { "channel": "position", "time": 0.0, "interpolation": "catmullrom", "data_points": [{ "x": 0, "y": 0, "z": 0 }] },
            { "channel": "position", "time": 0.5, "interpolation": "catmullrom", "data_points": [{ "x": 0, "y": 8, "z": 0 }] },
            { "channel": "position", "time": 1.0, "interpolation": "bezier", "data_points": [{ "x": 0, "y": 0, "z": 0 }],
              "bezier_left_time": [-0.1, -0.1, -0.1], "bezier_left_value": [0, 4, 0], "bezier_right_time": [0.1, 0.1, 0.1], "bezier_right_value": [0, 0, 0] }
        ] } } } ] })
        .to_string();
        let raw = bbmodel::parse(&json).unwrap();
        let a = bake(&raw.animations[0], 1, &|_| Some(0));
        let t = a.tracks[0].as_ref().unwrap();
        assert!(t.at(0.0).1.y.abs() < 1e-4);
        assert!((t.at(10.0).1.y - 8.0).abs() < 1e-3);
        assert!(t.at(20.0).1.y.abs() < 1e-3);
        // Between keys it is a curve, not a line: above the straight line on the way up
        assert!(t.at(5.0).1.y > 4.0, "{}", t.at(5.0).1.y);
        for tick in 0..=20 {
            assert!(t.at(tick as f32).1.is_finite());
        }
    }

    #[test]
    fn a_still_channel_is_one_entry_and_an_empty_one_is_gone() {
        let json = serde_json::json!({ "elements": [], "outliner": [], "animations": [ { "name": "a", "length": 5.0, "animators": { "g": { "name": "b", "keyframes": [
            { "channel": "rotation", "time": 0.0, "data_points": [{ "x": 0, "y": 30, "z": 0 }] },
            { "channel": "position", "time": 0.0, "data_points": [{ "x": 0, "y": 0, "z": 0 }] },
            { "channel": "scale", "time": 0.0, "data_points": [{ "x": 1, "y": 1, "z": 1 }] }
        ] } } } ] })
        .to_string();
        let raw = bbmodel::parse(&json).unwrap();
        let a = bake(&raw.animations[0], 1, &|_| Some(0));
        let t = a.tracks[0].as_ref().unwrap();
        assert_eq!(t.rot.as_ref().map(Vec::len), Some(1));
        assert!(t.pos.is_none() && t.scale.is_none());
        assert_eq!(a.samples(), 1);
    }
}

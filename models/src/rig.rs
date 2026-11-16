//! A model as the plugin shows it: bones, the cubes each bone draws, and where every bone is
//! for a set of running animations.
//!
//! One bone with cubes is one item display in the world, drawing one item model from the
//! resource pack. A model file of the game can only turn a cube around one axis, by 22.5 or 45
//! degrees; a cube turned any other way becomes a bone of its own, so it still looks right.
//!
//! Space: Blockbench's, which is also the game's model space. X to the east, Y up, Z to the
//! south, 16 units to a block, and a model looks to the north (-Z).
use std::collections::HashMap;

use crate::anim::{self, Animation};
use crate::bbmodel::{Node, Raw, RawCube, RawGroup};
use crate::math::{Mat, Quat, Vec3};

/// Model units to a block.
pub const UNITS: f32 = 16.0;
/// A cube in a model file may reach this far from the middle of the model, in units.
const REACH: f32 = 24.0;
/// More bones than this is more than a model should have: every one is an entity.
pub const MAX_BONES: usize = 400;
/// Entries (one bone's turn, shift or size at one tick) all animations of a model may keep:
/// about 24 MB.
pub const MAX_SAMPLES: usize = 2_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Plain,
    /// Follows where the creature looks (`h_...` or `head`).
    Head,
    /// Where the name stands (`tag_...` or `nametag`). Draws nothing.
    Tag,
    /// Where a rider sits (`p_...` or `seat`). Draws nothing.
    Seat,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    /// Left, top, right, bottom, 0 to 16 across the picture.
    pub uv: [f32; 4],
    pub texture: usize,
    pub rotation: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cube {
    pub from: Vec3,
    pub to: Vec3,
    /// A turn the model file can hold: axis (0 = X), degrees, and the point it turns around.
    pub turn: Option<(usize, f32, Vec3)>,
    pub faces: [Option<Face>; 6],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bone {
    /// Lower case letters, digits and `_`; no two bones of a model share one.
    pub name: String,
    pub parent: Option<usize>,
    pub pivot: Vec3,
    /// Degrees, at rest.
    pub rotation: Vec3,
    pub kind: Kind,
    pub cubes: Vec<Cube>,
    /// The cubes are drawn this much smaller in the model file so they fit its limits; the
    /// item display makes up for it.
    pub fit: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Texture {
    pub name: String,
    pub png: Vec<u8>,
    pub frame_time: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub name: String,
    /// Parents come before their children.
    pub bones: Vec<Bone>,
    pub textures: Vec<Texture>,
    pub animations: Vec<Animation>,
    /// Width and height in blocks, from a group or cube named `hitbox`.
    pub hitbox: Option<(f32, f32)>,
    pub notes: Vec<String>,
    /// By bone, worked out once: the bone at rest relative to its parent, and the part of its
    /// item display's transformation that never changes.
    pub rest: Vec<Mat>,
    pub tail: Vec<Mat>,
}

/// Lower case letters, digits and `_` only, never empty.
pub fn plain_name(name: &str) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(stem, ext)| if ["png", "bbmodel", "json", "jpg", "jpeg", "tga"].contains(&ext.to_ascii_lowercase().as_str()) { stem } else { name });
    let mut out: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
    out.truncate(48);
    if out.trim_matches('_').is_empty() { "part".to_owned() } else { out }
}

fn unique(name: String, taken: &mut Vec<String>) -> String {
    let mut out = name.clone();
    let mut n = 2;
    while taken.contains(&out) {
        out = format!("{name}_{n}");
        n += 1;
    }
    taken.push(out.clone());
    out
}

fn kind_of(name: &str) -> Kind {
    let n = name.to_ascii_lowercase();
    if n == "head" || n.starts_with("h_") {
        Kind::Head
    } else if n == "nametag" || n.starts_with("tag_") {
        Kind::Tag
    } else if n == "seat" || n.starts_with("p_") {
        Kind::Seat
    } else {
        Kind::Plain
    }
}

struct Build<'a> {
    raw: &'a Raw,
    bones: Vec<Bone>,
    names: Vec<String>,
    by_uuid: HashMap<String, usize>,
    hit: Option<(Vec3, Vec3)>,
}

impl Build<'_> {
    fn uv_size(&self, texture: usize) -> (f32, f32) {
        let t = &self.raw.textures[texture];
        (if t.uv_width > 0.0 { t.uv_width } else { self.raw.resolution.0 }, if t.uv_height > 0.0 { t.uv_height } else { self.raw.resolution.1 })
    }

    fn widen_hit(&mut self, cube: &RawCube) {
        let (from, to) = (Vec3::from_array(cube.from), Vec3::from_array(cube.to));
        let (low, high) = self.hit.unwrap_or((from, to));
        let min = |a: Vec3, b: Vec3| Vec3::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z));
        let max = |a: Vec3, b: Vec3| Vec3::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z));
        self.hit = Some((min(min(low, from), to), max(max(high, from), to)));
    }

    fn cube(&mut self, bone: usize, raw: &RawCube) {
        if !raw.visible {
            return;
        }
        if raw.name.eq_ignore_ascii_case("hitbox") {
            self.widen_hit(raw);
            return;
        }
        let faces: [Option<Face>; 6] = std::array::from_fn(|i| {
            let f = raw.faces[i].as_ref()?;
            let (w, h) = self.uv_size(f.texture);
            Some(Face { uv: [f.uv[0] * 16.0 / w, f.uv[1] * 16.0 / h, f.uv[2] * 16.0 / w, f.uv[3] * 16.0 / h], texture: f.texture, rotation: f.rotation })
        });
        if faces.iter().all(Option::is_none) {
            return;
        }
        let grow = Vec3::new(raw.inflate, raw.inflate, raw.inflate);
        let (from, to) = (Vec3::from_array(raw.from).sub(grow), Vec3::from_array(raw.to).add(grow));
        let (origin, rotation) = (Vec3::from_array(raw.origin), Vec3::from_array(raw.rotation));
        let turned: Vec<usize> = (0..3).filter(|a| rotation.get(*a).abs() > 1e-3).collect();
        let fits_a_model_file = |angle: f32| [22.5f32, 45.0].iter().any(|ok| (angle.abs() - ok).abs() < 1e-3);
        match turned.as_slice() {
            [] => self.bones[bone].cubes.push(Cube { from, to, turn: None, faces }),
            [axis] if fits_a_model_file(rotation.get(*axis)) => self.bones[bone].cubes.push(Cube { from, to, turn: Some((*axis, rotation.get(*axis), origin)), faces }),
            _ => {
                // Any other turn: a bone of its own that carries the turn
                let name = unique(format!("{}_{}", self.bones[bone].name, plain_name(&raw.name)), &mut self.names);
                self.bones.push(Bone { name, parent: Some(bone), pivot: origin, rotation, kind: Kind::Plain, cubes: vec![Cube { from, to, turn: None, faces }], fit: 1.0 });
            }
        }
    }

    fn group(&mut self, parent: Option<usize>, group: &RawGroup) {
        if !group.visible {
            return;
        }
        if group.name.eq_ignore_ascii_case("hitbox") {
            for child in &group.children {
                if let Node::Cube(uuid) = child
                    && let Some(cube) = self.raw.cubes.get(uuid)
                {
                    self.widen_hit(cube);
                }
            }
            return;
        }
        let name = unique(plain_name(&group.name), &mut self.names);
        let index = self.bones.len();
        self.bones.push(Bone { name, parent, pivot: Vec3::from_array(group.origin), rotation: Vec3::from_array(group.rotation), kind: kind_of(&group.name), cubes: Vec::new(), fit: 1.0 });
        self.by_uuid.insert(group.uuid.clone(), index);
        self.children(index, &group.children);
    }

    fn children(&mut self, bone: usize, children: &[Node]) {
        for child in children {
            match child {
                Node::Cube(uuid) => {
                    if let Some(cube) = self.raw.cubes.get(uuid) {
                        self.cube(bone, cube);
                    }
                }
                Node::Group(g) => self.group(Some(bone), g),
            }
        }
    }
}

/// Turn what a file holds into a model. `name` is the file's name without `.bbmodel`.
pub fn build(name: &str, raw: &Raw) -> Result<Model, String> {
    let mut b = Build { raw, bones: Vec::new(), names: Vec::new(), by_uuid: HashMap::new(), hit: None };
    let loose: Vec<Node> = raw.roots.iter().filter(|n| matches!(n, Node::Cube(_))).cloned().collect();
    for node in &raw.roots {
        if let Node::Group(g) = node {
            b.group(None, g);
        }
    }
    if !loose.is_empty() {
        // Cubes outside every group: one bone that never moves holds them
        let name = unique("loose".to_owned(), &mut b.names);
        let index = b.bones.len();
        b.bones.push(Bone { name, parent: None, pivot: Vec3::ZERO, rotation: Vec3::ZERO, kind: Kind::Plain, cubes: Vec::new(), fit: 1.0 });
        b.children(index, &loose);
    }
    // Tags and seats mark a place; what is drawn under them would never be seen in the right spot
    for bone in &mut b.bones {
        if matches!(bone.kind, Kind::Tag | Kind::Seat) {
            bone.cubes.clear();
        }
        let reach = bone.cubes.iter().flat_map(|c| [c.from.sub(bone.pivot), c.to.sub(bone.pivot)]).flat_map(Vec3::to_array).fold(0.0f32, |m, v| m.max(v.abs()));
        bone.fit = if reach > REACH { REACH / reach } else { 1.0 };
    }
    let drawn = b.bones.iter().filter(|bone| !bone.cubes.is_empty()).count();
    if drawn == 0 {
        return Err("it has no cubes with a picture on them".into());
    }
    if drawn > MAX_BONES {
        return Err(format!("it needs {drawn} parts; a model may have {MAX_BONES} (each part is an entity in the world)"));
    }

    let mut notes = raw.notes.clone();
    let mut taken = Vec::new();
    let textures: Vec<Texture> = raw.textures.iter().map(|t| Texture { name: unique(plain_name(&t.name), &mut taken), png: t.png.clone(), frame_time: t.frame_time }).collect();
    for (i, t) in textures.iter().enumerate() {
        let used = b.bones.iter().flat_map(|bone| &bone.cubes).flat_map(|c| c.faces.iter().flatten()).any(|f| f.texture == i);
        if used && t.png.is_empty() {
            notes.push(format!("picture {} is missing: what uses it shows as a missing picture", t.name));
        }
    }

    let by_name: HashMap<&str, usize> = b.bones.iter().enumerate().map(|(i, bone)| (bone.name.as_str(), i)).collect();
    let mut seen = Vec::new();
    let mut animations = Vec::new();
    let mut kept = 0usize;
    for raw_animation in &raw.animations {
        let mut baked = anim::bake(raw_animation, b.bones.len(), &|animator| b.by_uuid.get(&animator.uuid).copied().or_else(|| by_name.get(plain_name(&animator.name).as_str()).copied()));
        // An animation is a table in memory: very long ones on very many bones are refused
        kept += baked.samples();
        if kept > MAX_SAMPLES {
            notes.push(format!("animation {} left out: the animations of this model are too long for its number of bones", raw_animation.name));
            kept -= baked.samples();
            continue;
        }
        // `animation.dragon.walk` is how Bedrock names them; the last word is what people type
        baked.name = unique(plain_name(raw_animation.name.rsplit('.').next().unwrap_or(&raw_animation.name)), &mut seen);
        animations.push(baked);
    }

    let hitbox = b.hit.map(|(low, high)| (((high.x - low.x).max(high.z - low.z) / UNITS).max(0.1), ((high.y - low.y.min(0.0)) / UNITS).max(0.1)));
    let (rest, tail) = (b.bones.iter().map(rest).collect(), b.bones.iter().map(tail).collect());
    Ok(Model { name: plain_name(name), bones: b.bones, textures, animations, hitbox, notes, rest, tail })
}

/// A running animation as the pose sees it.
#[derive(Clone, Copy)]
pub struct Layer<'a> {
    pub animation: &'a Animation,
    /// Ticks into the animation.
    pub time: f32,
    /// 0 to 1: how much of it shows.
    pub weight: f32,
}

impl Model {
    pub fn animation(&self, name: &str) -> Option<usize> {
        self.animations.iter().position(|a| a.name == name)
    }

    /// Bones that are drawn, as indexes into `bones`.
    pub fn drawn(&self) -> impl Iterator<Item = usize> + '_ {
        self.bones.iter().enumerate().filter(|(_, b)| !b.cubes.is_empty()).map(|(i, _)| i)
    }

    pub fn has(&self, kind: Kind) -> bool {
        self.bones.iter().any(|b| b.kind == kind)
    }

    /// Where every bone is, in model space, for these animations. `look` is where the head
    /// points relative to the body: yaw and pitch in the game's degrees.
    pub fn pose(&self, layers: &[Layer], look: Option<(f32, f32)>, out: &mut Vec<Mat>) {
        out.clear();
        for (i, bone) in self.bones.iter().enumerate() {
            let (mut rot, mut pos, mut scale) = (bone.rotation, bone.pivot, Vec3::ONE);
            let mut still = true;
            for layer in layers {
                if let Some(track) = layer.animation.tracks.get(i).and_then(Option::as_ref) {
                    let (r, p, s) = track.at(layer.time);
                    rot = rot.add(r.scale(layer.weight));
                    pos = pos.add(p.scale(layer.weight));
                    scale = scale.mul(Vec3::ONE.lerp(s, layer.weight));
                    still = false;
                }
            }
            // The topmost head bone takes the look; head bones under it follow as children do
            let look = look.filter(|_| bone.kind == Kind::Head && bone.parent.is_none_or(|p| self.bones[p].kind != Kind::Head));
            let local = if still && look.is_none() {
                // Most bones of most poses: nothing moves them, and what they are at rest is known
                self.rest[i]
            } else {
                let mut turn = Mat::euler(rot);
                if let Some((yaw, pitch)) = look {
                    turn = Mat::rot_y(-yaw.to_radians()).mul(&Mat::rot_x(-pitch.to_radians())).mul(&turn);
                }
                let mut local = Mat::translation(pos).mul(&turn);
                if scale != Vec3::ONE {
                    local = local.mul(&Mat::scaling(scale));
                }
                local.mul(&Mat::translation(bone.pivot.scale(-1.0)))
            };
            out.push(match bone.parent {
                Some(p) => out[p].mul(&local),
                None => local,
            });
        }
    }
}

/// What an item display is told: where its item model goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shown {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Shown {
    /// Close enough that sending it again would change nothing a player can see.
    pub fn same(&self, o: &Shown) -> bool {
        let near = |a: Vec3, b: Vec3, eps: f32| (a.x - b.x).abs() < eps && (a.y - b.y).abs() < eps && (a.z - b.z).abs() < eps;
        let dot = self.rotation.x * o.rotation.x + self.rotation.y * o.rotation.y + self.rotation.z * o.rotation.z + self.rotation.w * o.rotation.w;
        near(self.translation, o.translation, 2e-4) && near(self.scale, o.scale, 2e-4) && dot.abs() > 0.999_999
    }
}

/// The model as a whole in the world: turned to `yaw` (the game's degrees), `scale` times its
/// size, and lifted by `lift` blocks. This is the same for every bone of one model.
pub fn root(yaw: f32, scale: f32, lift: f32) -> Mat {
    // The game's yaw runs clockwise seen from above; 0 looks to the south
    Mat::translation(Vec3::new(0.0, lift, 0.0)).mul(&Mat::rot_y(-yaw.to_radians())).mul(&Mat::scaling(Vec3::new(scale, scale, scale)))
}

/// The item display's transformation for one bone.
///
/// An item display draws its item model turned half way round (models look north, entities
/// look south at yaw 0), around the model's middle, which is where the bone's pivot was put.
/// `world` takes a point of the model at rest to where the pose has it.
///
/// The whole of it is `root * FLIP * (1/16) * world * move to the pivot * grow back * FLIP`.
/// The three last steps never change for a bone ([`tail`], kept in `Model::tail`), the three
/// first are the same for every bone of one model ([`lead`]).
pub fn shown(tail: &Mat, world: &Mat, lead: &Mat) -> Shown {
    let (translation, rotation, scale) = lead.mul(world).mul(tail).decompose();
    Shown { translation, rotation, scale }
}

const FLIP: Mat = Mat { c: [Vec3::new(-1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, -1.0)], t: Vec3::ZERO };

/// The part of a bone's transformation that depends on the model as a whole.
pub fn lead(root: &Mat) -> Mat {
    root.mul(&FLIP).mul(&Mat::scaling(Vec3::new(1.0 / UNITS, 1.0 / UNITS, 1.0 / UNITS)))
}

/// The part of a bone's transformation that depends on the bone alone.
fn tail(bone: &Bone) -> Mat {
    let grow = UNITS / bone.fit;
    Mat::translation(bone.pivot).mul(&Mat::scaling(Vec3::new(grow, grow, grow))).mul(&FLIP)
}

/// A bone at rest, relative to its parent.
fn rest(bone: &Bone) -> Mat {
    Mat::translation(bone.pivot).mul(&Mat::euler(bone.rotation)).mul(&Mat::translation(bone.pivot.scale(-1.0)))
}

/// A point of the model (in model units, at rest) in blocks relative to the model's place.
pub fn place(point: Vec3, world: &Mat, root: &Mat) -> Vec3 {
    root.mul(&FLIP).point(world.point(point).scale(1.0 / UNITS))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::bbmodel::{self, tests::sample};

    pub fn model() -> Model {
        build("Test Creature", &bbmodel::parse(&sample()).unwrap()).unwrap()
    }

    fn near(a: Vec3, b: Vec3) -> bool {
        a.sub(b).length() < 1e-3
    }

    #[test]
    fn bones_cubes_and_special_names() {
        let m = model();
        assert_eq!(m.name, "test_creature");
        let names: Vec<&str> = m.bones.iter().map(|b| b.name.as_str()).collect();
        // The leg's cube is turned around two axes: it becomes a bone under the leg
        assert_eq!(names, ["root", "body", "h_head", "leg", "leg_leg", "tag_name"]);
        assert_eq!(m.bones[1].parent, Some(0));
        assert_eq!(m.bones[4].parent, Some(3));
        assert_eq!(m.bones[4].rotation, Vec3::new(10.0, 0.0, 30.0));
        assert_eq!((m.bones[2].kind, m.bones[5].kind), (Kind::Head, Kind::Tag));
        // The head's cube is turned 22.5 around Y: a model file can hold that
        assert_eq!(m.bones[2].cubes[0].turn, Some((1, 22.5, Vec3::new(0.0, 16.0, -6.0))));
        // Inflate 0.5 makes it larger on every side
        assert_eq!(m.bones[2].cubes[0].from, Vec3::new(-3.5, 15.5, -9.5));
        // UV in pixels of a 32 pixel picture becomes 0 to 16
        assert_eq!(m.bones[1].cubes[0].faces[0].as_ref().unwrap().uv, [0.0, 0.0, 4.0, 4.0]);
        assert_eq!(m.drawn().collect::<Vec<_>>(), [1, 2, 4]);
        // 10 wide, 24 high, in blocks
        assert_eq!(m.hitbox, Some((0.625, 1.5)));
        assert_eq!(m.textures[0].name, "skin");
        assert_eq!(m.animations.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["idle", "attack"]);
        assert!(m.animations[0].tracks[2].is_some() && m.animations[0].tracks[1].is_some());
        assert!(m.animations[1].tracks[3].is_some());
    }

    #[test]
    fn names_are_made_plain_and_unique() {
        assert_eq!(plain_name("Left Arm.png"), "left_arm");
        assert_eq!(plain_name("animation.dragon.walk"), "animation_dragon_walk");
        assert_eq!(plain_name("***"), "part");
        let mut taken = Vec::new();
        assert_eq!(unique("a".into(), &mut taken), "a");
        assert_eq!(unique("a".into(), &mut taken), "a_2");
        assert_eq!(unique("a".into(), &mut taken), "a_3");
    }

    #[test]
    fn at_rest_nothing_moves() {
        let m = model();
        let mut world = Vec::new();
        m.pose(&[], None, &mut world);
        assert_eq!(world.len(), m.bones.len());
        // Bones without a turn of their own leave points where they are
        for i in [0, 1, 2, 3] {
            assert!(near(world[i].point(Vec3::new(3.0, 5.0, -7.0)), Vec3::new(3.0, 5.0, -7.0)), "{i}");
        }
        // The bone made for the turned cube turns around the cube's pivot
        let pivot = m.bones[4].pivot;
        assert!(near(world[4].point(pivot), pivot));
        assert!(!near(world[4].point(Vec3::new(-3.0, 0.0, 0.0)), Vec3::new(-3.0, 0.0, 0.0)));
    }

    #[test]
    fn an_item_display_at_rest_draws_the_bone_where_the_file_has_it() {
        let m = model();
        let mut world = Vec::new();
        m.pose(&[], None, &mut world);
        let root = root(0.0, 1.0, 0.0);
        let body = &m.bones[1];
        let shown = shown(&m.tail[1], &world[1], &lead(&root));
        // The body's pivot is 8 units up: half a block. No turn, no change of size.
        assert!(near(shown.translation, Vec3::new(0.0, 0.5, 0.0)), "{shown:?}");
        assert!(near(shown.scale, Vec3::ONE));
        assert!(shown.rotation.w.abs() > 0.9999);
        // What the display draws: its model, half way round, then this transformation. The
        // front of the body (north, -Z in the file) ends up to the south: where yaw 0 looks.
        let drawn = |p: Vec3| shown.rotation.rotate(Vec3::new(-p.x, p.y, -p.z).scale(body.fit / UNITS).mul(shown.scale)).add(shown.translation);
        let front = drawn(Vec3::new(0.0, 12.0, -6.0).sub(body.pivot));
        assert!(near(front, Vec3::new(0.0, 0.75, 0.375)), "{front:?}");
        assert!(near(place(Vec3::new(0.0, 12.0, -6.0), &world[1], &root), front));
    }

    #[test]
    fn yaw_scale_and_lift_move_the_whole_model() {
        let m = model();
        let mut world = Vec::new();
        m.pose(&[], None, &mut world);
        // Yaw 90 looks to the west (-X) in the game
        let turned = root(90.0, 2.0, 1.0);
        let front = place(Vec3::new(0.0, 0.0, -16.0), &world[0], &turned);
        assert!(near(front, Vec3::new(-2.0, 1.0, 0.0)), "{front:?}");
        let s = shown(&m.tail[1], &world[1], &lead(&turned));
        assert!(near(s.scale, Vec3::new(2.0, 2.0, 2.0)));
    }

    #[test]
    fn animations_add_up_by_weight() {
        let m = model();
        let idle = &m.animations[0];
        let mut world = Vec::new();
        // Half a second into idle the body is lifted 2 units and 1.25 times its size
        m.pose(&[Layer { animation: idle, time: 10.0, weight: 1.0 }], None, &mut world);
        let pivot = m.bones[1].pivot;
        assert!(near(world[1].point(pivot), pivot.add(Vec3::new(0.0, 2.0, 0.0))));
        let top = world[1].point(pivot.add(Vec3::new(0.0, 8.0, 0.0)));
        assert!(near(top, pivot.add(Vec3::new(0.0, 2.0 + 10.0, 0.0))), "{top:?}");
        // At half weight it is half of that
        m.pose(&[Layer { animation: idle, time: 10.0, weight: 0.5 }], None, &mut world);
        assert!(near(world[1].point(pivot), pivot.add(Vec3::new(0.0, 1.0, 0.0))));
        // The head is a child of the body: it rides along
        // (8 up and 3 in front of the body's pivot, at 1.125 times the size, and 1 unit up)
        assert!(near(world[2].point(m.bones[2].pivot).sub(m.bones[2].pivot), Vec3::new(0.0, 1.0 + 8.0 * 0.125, -3.0 * 0.125)), "{:?}", world[2].point(m.bones[2].pivot));
    }

    #[test]
    fn the_head_looks_where_it_is_told() {
        let m = model();
        let mut world = Vec::new();
        let root = root(0.0, 1.0, 0.0);
        let nose = Vec3::new(0.0, 16.0, -19.0);
        // Looking 90 to the right of the body (the game's yaw grows clockwise): the nose, in
        // front of the head's pivot, swings to the west
        m.pose(&[], Some((90.0, 0.0)), &mut world);
        let p = place(nose, &world[2], &root);
        assert!(near(p, Vec3::new(-1.0, 1.0, -3.0 / 16.0 * -1.0)), "{p:?}");
        // Looking down (positive pitch in the game): the nose drops
        m.pose(&[], Some((0.0, 90.0)), &mut world);
        let p = place(nose, &world[2], &root);
        assert!(near(p, Vec3::new(0.0, 0.0, 3.0 / 16.0)), "{p:?}");
    }

    #[test]
    fn large_bones_are_drawn_smaller_and_scaled_back() {
        let json = serde_json::json!({
            "resolution": { "width": 16, "height": 16 },
            "elements": [ { "uuid": "c", "from": [-48, 0, -8], "to": [48, 16, 8], "faces": { "up": { "uv": [0, 0, 16, 16], "texture": 0 } } } ],
            "outliner": [ { "name": "wing", "uuid": "g", "origin": [0, 0, 0], "children": ["c"] } ],
            "textures": [ { "name": "t", "source": format!("data:image/png;base64,{}", bbmodel::tests::PNG) } ]
        })
        .to_string();
        let m = build("big", &bbmodel::parse(&json).unwrap()).unwrap();
        assert_eq!(m.bones[0].fit, 0.5);
        let mut world = Vec::new();
        m.pose(&[], None, &mut world);
        assert!(near(shown(&m.tail[0], &world[0], &lead(&root(0.0, 1.0, 0.0))).scale, Vec3::new(2.0, 2.0, 2.0)));
    }

    #[test]
    fn a_file_with_nothing_to_draw_is_refused() {
        let empty = bbmodel::parse(r#"{"elements": [], "outliner": []}"#).unwrap();
        assert!(build("x", &empty).is_err());
    }
}

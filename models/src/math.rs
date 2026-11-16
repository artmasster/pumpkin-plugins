//! The little 3D math a pose needs: vectors, quaternions and affine matrices. Right-handed,
//! Y up, the way Blockbench and the game's models see space.

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 0.0 };
    pub const ONE: Vec3 = Vec3 { x: 1.0, y: 1.0, z: 1.0 };

    pub const fn new(x: f32, y: f32, z: f32) -> Vec3 {
        Vec3 { x, y, z }
    }
    pub fn add(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
    pub fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
    pub fn scale(self, k: f32) -> Vec3 {
        Vec3::new(self.x * k, self.y * k, self.z * k)
    }
    /// Component by component.
    pub fn mul(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x * o.x, self.y * o.y, self.z * o.z)
    }
    pub fn lerp(self, o: Vec3, t: f32) -> Vec3 {
        Vec3::new(self.x + (o.x - self.x) * t, self.y + (o.y - self.y) * t, self.z + (o.z - self.z) * t)
    }
    pub fn dot(self, o: Vec3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    pub fn cross(self, o: Vec3) -> Vec3 {
        Vec3::new(self.y * o.z - self.z * o.y, self.z * o.x - self.x * o.z, self.x * o.y - self.y * o.x)
    }
    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }
    pub fn get(self, axis: usize) -> f32 {
        [self.x, self.y, self.z][axis]
    }
    pub fn from_array(a: [f32; 3]) -> Vec3 {
        Vec3::new(a[0], a[1], a[2])
    }
    pub fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quat {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Quat {
    pub const IDENTITY: Quat = Quat { x: 0.0, y: 0.0, z: 0.0, w: 1.0 };

    /// From three unit vectors at right angles (the columns of a rotation matrix).
    pub fn from_basis(a: Vec3, b: Vec3, c: Vec3) -> Quat {
        let trace = a.x + b.y + c.z;
        let q = if trace > 0.0 {
            let s = (trace + 1.0).sqrt() * 2.0;
            Quat { w: 0.25 * s, x: (b.z - c.y) / s, y: (c.x - a.z) / s, z: (a.y - b.x) / s }
        } else if a.x > b.y && a.x > c.z {
            let s = (1.0 + a.x - b.y - c.z).sqrt() * 2.0;
            Quat { w: (b.z - c.y) / s, x: 0.25 * s, y: (b.x + a.y) / s, z: (c.x + a.z) / s }
        } else if b.y > c.z {
            let s = (1.0 + b.y - a.x - c.z).sqrt() * 2.0;
            Quat { w: (c.x - a.z) / s, x: (b.x + a.y) / s, y: 0.25 * s, z: (c.y + b.z) / s }
        } else {
            let s = (1.0 + c.z - a.x - b.y).sqrt() * 2.0;
            Quat { w: (a.y - b.x) / s, x: (c.x + a.z) / s, y: (c.y + b.z) / s, z: 0.25 * s }
        };
        q.normalized()
    }

    pub fn normalized(self) -> Quat {
        let n = (self.x * self.x + self.y * self.y + self.z * self.z + self.w * self.w).sqrt();
        if n < 1e-6 || !n.is_finite() {
            return Quat::IDENTITY;
        }
        Quat { x: self.x / n, y: self.y / n, z: self.z / n, w: self.w / n }
    }

    /// The same turn can be written two ways (q and -q). The game turns the short way between
    /// two values, so the one closer to `near` is the one to send.
    pub fn close_to(self, near: Quat) -> Quat {
        if self.x * near.x + self.y * near.y + self.z * near.z + self.w * near.w < 0.0 { Quat { x: -self.x, y: -self.y, z: -self.z, w: -self.w } } else { self }
    }

    pub fn rotate(self, v: Vec3) -> Vec3 {
        let u = Vec3::new(self.x, self.y, self.z);
        let t = u.cross(v).scale(2.0);
        v.add(t.scale(self.w)).add(u.cross(t))
    }
}

/// An affine transform: three basis columns and a translation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat {
    pub c: [Vec3; 3],
    pub t: Vec3,
}

impl Mat {
    pub const IDENTITY: Mat = Mat { c: [Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 1.0)], t: Vec3::ZERO };

    pub fn translation(t: Vec3) -> Mat {
        Mat { t, ..Mat::IDENTITY }
    }
    pub fn scaling(s: Vec3) -> Mat {
        Mat { c: [Vec3::new(s.x, 0.0, 0.0), Vec3::new(0.0, s.y, 0.0), Vec3::new(0.0, 0.0, s.z)], t: Vec3::ZERO }
    }
    pub fn rot_x(rad: f32) -> Mat {
        let (s, c) = rad.sin_cos();
        Mat { c: [Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, c, s), Vec3::new(0.0, -s, c)], t: Vec3::ZERO }
    }
    pub fn rot_y(rad: f32) -> Mat {
        let (s, c) = rad.sin_cos();
        Mat { c: [Vec3::new(c, 0.0, -s), Vec3::new(0.0, 1.0, 0.0), Vec3::new(s, 0.0, c)], t: Vec3::ZERO }
    }
    pub fn rot_z(rad: f32) -> Mat {
        let (s, c) = rad.sin_cos();
        Mat { c: [Vec3::new(c, s, 0.0), Vec3::new(-s, c, 0.0), Vec3::new(0.0, 0.0, 1.0)], t: Vec3::ZERO }
    }

    /// Blockbench's rotation of a bone or a cube, in degrees: around X first, then Y, then Z.
    pub fn euler(deg: Vec3) -> Mat {
        if deg == Vec3::ZERO {
            return Mat::IDENTITY;
        }
        Mat::rot_z(deg.z.to_radians()).mul(&Mat::rot_y(deg.y.to_radians())).mul(&Mat::rot_x(deg.x.to_radians()))
    }

    /// `self` after `o`: what `o` does happens first.
    pub fn mul(&self, o: &Mat) -> Mat {
        Mat { c: [self.vector(o.c[0]), self.vector(o.c[1]), self.vector(o.c[2])], t: self.point(o.t) }
    }
    pub fn vector(&self, v: Vec3) -> Vec3 {
        self.c[0].scale(v.x).add(self.c[1].scale(v.y)).add(self.c[2].scale(v.z))
    }
    pub fn point(&self, p: Vec3) -> Vec3 {
        self.vector(p).add(self.t)
    }

    /// Translation, rotation and scale that give this matrix back as translate * rotate *
    /// scale. A shear (a parent scaled unevenly above a turned child) is lost.
    pub fn decompose(&self) -> (Vec3, Quat, Vec3) {
        let mut scale = Vec3::new(self.c[0].length(), self.c[1].length(), self.c[2].length());
        // A mirror: one axis points the other way
        if self.c[0].cross(self.c[1]).dot(self.c[2]) < 0.0 {
            scale.x = -scale.x;
        }
        let unit = |v: Vec3, s: f32, fallback: Vec3| if s.abs() < 1e-6 { fallback } else { v.scale(1.0 / s) };
        let a = unit(self.c[0], scale.x, Vec3::new(1.0, 0.0, 0.0));
        let b = unit(self.c[1], scale.y, Vec3::new(0.0, 1.0, 0.0));
        let c = unit(self.c[2], scale.z, Vec3::new(0.0, 0.0, 1.0));
        (self.t, Quat::from_basis(a, b, c), scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: Vec3, b: Vec3) -> bool {
        a.sub(b).length() < 1e-4
    }

    #[test]
    fn turns_go_the_right_handed_way() {
        // A quarter turn around Y takes +Z to +X, around X takes +Y to +Z, around Z takes +X to +Y
        assert!(near(Mat::rot_y(90f32.to_radians()).vector(Vec3::new(0.0, 0.0, 1.0)), Vec3::new(1.0, 0.0, 0.0)));
        assert!(near(Mat::rot_x(90f32.to_radians()).vector(Vec3::new(0.0, 1.0, 0.0)), Vec3::new(0.0, 0.0, 1.0)));
        assert!(near(Mat::rot_z(90f32.to_radians()).vector(Vec3::new(1.0, 0.0, 0.0)), Vec3::new(0.0, 1.0, 0.0)));
    }

    #[test]
    fn euler_turns_around_x_first_then_y_then_z() {
        let m = Mat::euler(Vec3::new(90.0, 90.0, 0.0));
        // +Y goes to +Z by the X turn, then to +X by the Y turn
        assert!(near(m.vector(Vec3::new(0.0, 1.0, 0.0)), Vec3::new(1.0, 0.0, 0.0)));
    }

    #[test]
    fn a_matrix_comes_back_from_its_parts() {
        let m = Mat::translation(Vec3::new(1.0, 2.0, 3.0)).mul(&Mat::euler(Vec3::new(20.0, -75.0, 130.0))).mul(&Mat::scaling(Vec3::new(2.0, 0.5, 3.0)));
        let (t, q, s) = m.decompose();
        assert!(near(t, Vec3::new(1.0, 2.0, 3.0)));
        assert!(near(s, Vec3::new(2.0, 0.5, 3.0)));
        for v in [Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.3, -2.0, 0.7)] {
            assert!(near(q.rotate(v.mul(s)).add(t), m.point(v)), "{v:?}");
        }
    }

    #[test]
    fn the_short_way_round_is_chosen() {
        let q = Quat { x: 0.0, y: 1.0, z: 0.0, w: 0.0 };
        let other = Quat { x: 0.0, y: -0.9, z: 0.0, w: 0.1 }.normalized();
        assert!(q.close_to(other).y < 0.0);
        assert_eq!(q.close_to(q), q);
    }
}

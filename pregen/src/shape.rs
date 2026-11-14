//! Selection shapes. A chunk belongs to a shape when its center block
//! (`chunk * 16 + 8`) is inside, the same rule Chunky uses.
use std::f64::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Square,
    Rectangle,
    Circle,
    Ellipse,
    Diamond,
    Triangle,
    Pentagon,
    Hexagon,
    Star,
}

impl Kind {
    pub const NAMES: [&str; 9] = [
        "square", "rectangle", "circle", "ellipse", "diamond", "triangle", "pentagon", "hexagon", "star",
    ];

    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s.to_lowercase().as_str() {
            "square" => Kind::Square,
            "rectangle" | "rect" => Kind::Rectangle,
            "circle" => Kind::Circle,
            "ellipse" | "oval" => Kind::Ellipse,
            "diamond" => Kind::Diamond,
            "triangle" => Kind::Triangle,
            "pentagon" => Kind::Pentagon,
            "hexagon" => Kind::Hexagon,
            "star" => Kind::Star,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        Kind::NAMES[self as usize]
    }

    /// Shapes that use a separate Z radius.
    pub fn two_radii(self) -> bool {
        matches!(self, Kind::Rectangle | Kind::Ellipse)
    }
}

#[derive(Clone, Debug)]
pub struct Shape {
    kind: Kind,
    cx: f64,
    cz: f64,
    rx: f64,
    rz: f64,
    poly: Vec<(f64, f64)>,
}

impl Shape {
    pub fn new(kind: Kind, cx: f64, cz: f64, rx: f64, rz: f64) -> Shape {
        let rz = if kind.two_radii() { rz } else { rx };
        let poly = match kind {
            Kind::Triangle => regular(cx, cz, rx, 3),
            Kind::Pentagon => regular(cx, cz, rx, 5),
            Kind::Hexagon => regular(cx, cz, rx, 6),
            Kind::Star => star(cx, cz, rx),
            _ => Vec::new(),
        };
        Shape { kind, cx, cz, rx, rz, poly }
    }

    /// Bounding box in chunk coordinates (inclusive).
    pub fn chunk_bounds(&self) -> (i32, i32, i32, i32) {
        let f = |v: f64| (v / 16.0).floor() as i32;
        (f(self.cx - self.rx), f(self.cz - self.rz), f(self.cx + self.rx), f(self.cz + self.rz))
    }

    pub fn contains_chunk(&self, x: i32, z: i32) -> bool {
        let px = f64::from(x) * 16.0 + 8.0;
        let pz = f64::from(z) * 16.0 + 8.0;
        let dx = px - self.cx;
        let dz = pz - self.cz;
        match self.kind {
            Kind::Square | Kind::Rectangle => dx.abs() <= self.rx && dz.abs() <= self.rz,
            Kind::Circle => dx * dx + dz * dz <= self.rx * self.rx,
            Kind::Ellipse => {
                let (a, b) = (dx / self.rx.max(1e-9), dz / self.rz.max(1e-9));
                a * a + b * b <= 1.0
            }
            Kind::Diamond => dx.abs() + dz.abs() <= self.rx,
            _ => point_in_polygon(&self.poly, px, pz),
        }
    }

    /// X intervals (block coordinates) covered on the line `z`, used for counting.
    fn row_intervals(&self, pz: f64) -> Vec<(f64, f64)> {
        let dz = pz - self.cz;
        let half = match self.kind {
            Kind::Square | Kind::Rectangle => (dz.abs() <= self.rz).then_some(self.rx),
            Kind::Circle => {
                let r2 = self.rx * self.rx - dz * dz;
                (r2 >= 0.0).then(|| r2.sqrt())
            }
            Kind::Ellipse => {
                let t = 1.0 - (dz / self.rz.max(1e-9)).powi(2);
                (t >= 0.0).then(|| self.rx * t.sqrt())
            }
            Kind::Diamond => (dz.abs() <= self.rx).then(|| self.rx - dz.abs()),
            _ => return polygon_row(&self.poly, pz),
        };
        half.map(|h| vec![(self.cx - h, self.cx + h)]).unwrap_or_default()
    }

    /// Number of chunks inside the shape, in O(rows).
    pub fn chunk_count(&self) -> u64 {
        let (_, min_z, _, max_z) = self.chunk_bounds();
        let mut total = 0u64;
        for z in min_z..=max_z {
            let pz = f64::from(z) * 16.0 + 8.0;
            for (a, b) in self.row_intervals(pz) {
                let lo = ((a - 8.0) / 16.0).ceil() as i64;
                let hi = ((b - 8.0) / 16.0).floor() as i64;
                if hi >= lo {
                    total += (hi - lo + 1) as u64;
                }
            }
        }
        total
    }
}

fn regular(cx: f64, cz: f64, r: f64, n: usize) -> Vec<(f64, f64)> {
    (0..n)
        .map(|i| {
            let a = -PI / 2.0 + 2.0 * PI * i as f64 / n as f64;
            (cx + r * a.cos(), cz + r * a.sin())
        })
        .collect()
}

fn star(cx: f64, cz: f64, r: f64) -> Vec<(f64, f64)> {
    // Regular pentagram: inner radius = r * sin(18°) / sin(54°)
    let inner = r * (PI / 10.0).sin() / (3.0 * PI / 10.0).sin();
    (0..10)
        .map(|i| {
            let a = -PI / 2.0 + PI * i as f64 / 5.0;
            let rr = if i % 2 == 0 { r } else { inner };
            (cx + rr * a.cos(), cz + rr * a.sin())
        })
        .collect()
}

fn point_in_polygon(poly: &[(f64, f64)], x: f64, z: f64) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (xi, zi) = poly[i];
        let (xj, zj) = poly[j];
        if (zi > z) != (zj > z) && x < (xj - xi) * (z - zi) / (zj - zi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn polygon_row(poly: &[(f64, f64)], z: f64) -> Vec<(f64, f64)> {
    let mut xs = Vec::new();
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (xi, zi) = poly[i];
        let (xj, zj) = poly[j];
        if (zi > z) != (zj > z) {
            xs.push((xj - xi) * (z - zi) / (zj - zi) + xi);
        }
        j = i;
    }
    xs.sort_by(f64::total_cmp);
    xs.chunks_exact(2).map(|p| (p[0], p[1])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brute(s: &Shape) -> u64 {
        let (x0, z0, x1, z1) = s.chunk_bounds();
        let mut n = 0;
        for x in x0..=x1 {
            for z in z0..=z1 {
                n += u64::from(s.contains_chunk(x, z));
            }
        }
        n
    }

    #[test]
    fn counts_match_brute_force() {
        for kind in Kind::NAMES.iter().map(|n| Kind::parse(n).unwrap()) {
            for (cx, cz, rx, rz) in [(0.0, 0.0, 500.0, 300.0), (123.0, -77.0, 250.0, 900.0), (8.0, 8.0, 16.0, 16.0)] {
                let s = Shape::new(kind, cx, cz, rx, rz);
                assert_eq!(s.chunk_count(), brute(&s), "{kind:?} {cx} {cz} {rx} {rz}");
            }
        }
    }
}

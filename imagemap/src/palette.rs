//! The colours a Minecraft map can show, and matching a picture to them.
//!
//! A map pixel is one byte: `base * 4 + shade`. `base` is one of the vanilla map colours
//! (`mapcolors::VANILLA`, 0 = nothing), `shade` picks one of four brightnesses of it. Bytes
//! 0..=3 are transparent.
use crate::mapcolors::VANILLA;

/// Brightness of the four shades of a base colour, out of 255.
const SHADES: [u32; 4] = [180, 220, 255, 135];

/// The colour of a map byte; `None` for the transparent ones.
pub fn rgb(byte: u8) -> Option<[u8; 3]> {
    let (base, shade) = (usize::from(byte / 4), usize::from(byte % 4));
    if base == 0 || base >= VANILLA.len() {
        return None;
    }
    let (r, g, b) = VANILLA[base];
    let m = SHADES[shade];
    Some([(u32::from(r) * m / 255) as u8, (u32::from(g) * m / 255) as u8, (u32::from(b) * m / 255) as u8])
}

/// How far apart two colours look ("redmean": cheap and much closer to the eye than plain RGB distance).
fn distance(a: [i32; 3], b: [i32; 3]) -> i64 {
    let rm = (a[0] + b[0]) / 2;
    let (dr, dg, db) = (i64::from(a[0] - b[0]), i64::from(a[1] - b[1]), i64::from(a[2] - b[2]));
    let rm = i64::from(rm);
    ((512 + rm) * dr * dr) / 256 + 4 * dg * dg + ((767 - rm) * db * db) / 256
}

/// Finds the map byte nearest to a colour. Answers are remembered per 6-bit-per-channel
/// colour cube cell, so a whole poster costs a few thousand searches, not one per pixel.
pub struct Matcher {
    colours: Vec<(u8, [i32; 3])>,
    /// 0 = not worked out yet (0 is never an answer).
    cells: Vec<u8>,
}

impl Default for Matcher {
    fn default() -> Self {
        let colours = (4..=255u8).filter_map(|b| rgb(b).map(|c| (b, [i32::from(c[0]), i32::from(c[1]), i32::from(c[2])]))).collect();
        Matcher { colours, cells: vec![0; 64 * 64 * 64] }
    }
}

impl Matcher {
    pub fn new() -> Self {
        Matcher::default()
    }

    pub fn colours(&self) -> usize {
        self.colours.len()
    }

    /// The map byte for an opaque colour.
    pub fn nearest(&mut self, r: u8, g: u8, b: u8) -> u8 {
        let cell = (usize::from(r >> 2) << 12) | (usize::from(g >> 2) << 6) | usize::from(b >> 2);
        if self.cells[cell] == 0 {
            // The middle of the cell stands for all of it
            let want = [i32::from(r & 0xFC) + 2, i32::from(g & 0xFC) + 2, i32::from(b & 0xFC) + 2];
            self.cells[cell] = self.colours.iter().min_by_key(|(_, c)| distance(want, *c)).map_or(4, |(byte, _)| *byte);
        }
        self.cells[cell]
    }
}

/// Turns rows of RGBA into rows of map bytes, top to bottom, spreading what each pixel could
/// not show onto its neighbours (Floyd–Steinberg) when `dither` is on.
pub struct Quantizer {
    matcher: Matcher,
    dither: bool,
    width: usize,
    /// Error carried into this row and the next: three channels per pixel, in 1/16 units.
    now: Vec<i32>,
    next: Vec<i32>,
}

impl Quantizer {
    pub fn new(width: usize, dither: bool) -> Self {
        Quantizer { matcher: Matcher::new(), dither, width, now: vec![0; (width + 2) * 3], next: vec![0; (width + 2) * 3] }
    }

    /// One row of `width` RGBA pixels → `width` map bytes. Pixels under half opaque are transparent.
    pub fn row(&mut self, rgba: &[u8], out: &mut [u8]) {
        debug_assert!(rgba.len() >= self.width * 4 && out.len() >= self.width);
        for x in 0..self.width {
            let p = &rgba[x * 4..x * 4 + 4];
            if p[3] < 128 {
                out[x] = 0;
                continue;
            }
            if !self.dither {
                out[x] = self.matcher.nearest(p[0], p[1], p[2]);
                continue;
            }
            let e = (x + 1) * 3;
            let want = [
                (i32::from(p[0]) + self.now[e] / 16).clamp(0, 255),
                (i32::from(p[1]) + self.now[e + 1] / 16).clamp(0, 255),
                (i32::from(p[2]) + self.now[e + 2] / 16).clamp(0, 255),
            ];
            let byte = self.matcher.nearest(want[0] as u8, want[1] as u8, want[2] as u8);
            out[x] = byte;
            let got = rgb(byte).unwrap_or([0, 0, 0]);
            for ch in 0..3 {
                let err = want[ch] - i32::from(got[ch]);
                self.now[e + 3 + ch] += err * 7;
                self.next[e - 3 + ch] += err * 3;
                self.next[e + ch] += err * 5;
                self.next[e + 3 + ch] += err;
            }
        }
        std::mem::swap(&mut self.now, &mut self.next);
        self.next.iter_mut().for_each(|v| *v = 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_palette_is_the_games() {
        // Bytes 0..=3 are transparent; grass (base 1) at full brightness is the table colour
        assert_eq!(rgb(0), None);
        assert_eq!(rgb(3), None);
        assert_eq!(rgb(4 + 2), Some([127, 178, 56]));
        assert_eq!(rgb(4), Some([89, 125, 39]));
        assert_eq!(rgb(8 * 4 + 2), Some([255, 255, 255]));
        // 61 base colours with four shades each
        let m = Matcher::new();
        assert_eq!(m.colours(), 61 * 4);
        assert_eq!(rgb(61 * 4 + 3).is_some(), true);
        assert_eq!(rgb(62 * 4), None);
    }

    #[test]
    fn colours_find_their_neighbour() {
        let mut m = Matcher::new();
        assert_eq!(rgb(m.nearest(255, 255, 255)), Some([255, 255, 255]));
        let black = rgb(m.nearest(0, 0, 0)).unwrap();
        assert!(black.iter().all(|c| *c < 30), "{black:?}");
        let red = rgb(m.nearest(250, 5, 5)).unwrap();
        assert!(red[0] > 200 && red[1] < 40 && red[2] < 40, "{red:?}");
        let blue = rgb(m.nearest(60, 60, 250)).unwrap();
        assert!(blue[2] > 200 && blue[0] < 110, "{blue:?}");
        // Every palette colour comes back as itself or its closest neighbour (a cell is 4 wide per channel)
        for byte in 4..=247u8 {
            let c = rgb(byte).unwrap();
            let back = rgb(m.nearest(c[0], c[1], c[2])).unwrap();
            let off: i32 = (0..3).map(|i| (i32::from(c[i]) - i32::from(back[i])).abs()).sum();
            assert!(off <= 12, "byte {byte}: {c:?} came back {back:?}");
        }
    }

    #[test]
    fn rows_become_map_bytes() {
        let mut q = Quantizer::new(4, false);
        let mut out = [9u8; 4];
        q.row(&[255, 255, 255, 255, 0, 0, 0, 255, 10, 20, 30, 0, 255, 0, 0, 127], &mut out);
        assert_eq!(rgb(out[0]), Some([255, 255, 255]));
        assert!(out[1] >= 4);
        // Transparent pixels (alpha under half) are byte 0
        assert_eq!((out[2], out[3]), (0, 0));
    }

    #[test]
    fn dithering_keeps_the_average() {
        // A flat mid grey-blue that no map colour matches: with dithering the average of the
        // result is much closer to it than the single nearest colour is
        let (w, h) = (64usize, 64usize);
        let colour = [90u8, 140, 200];
        let row: Vec<u8> = (0..w).flat_map(|_| [colour[0], colour[1], colour[2], 255]).collect();
        let average = |dither: bool| {
            let mut q = Quantizer::new(w, dither);
            let mut sum = [0u64; 3];
            let mut out = vec![0u8; w];
            for _ in 0..h {
                q.row(&row, &mut out);
                for b in &out {
                    let c = rgb(*b).unwrap();
                    for i in 0..3 {
                        sum[i] += u64::from(c[i]);
                    }
                }
            }
            [sum[0] / (w * h) as u64, sum[1] / (w * h) as u64, sum[2] / (w * h) as u64]
        };
        let off = |a: [u64; 3]| (0..3).map(|i| (a[i] as i64 - i64::from(colour[i])).abs()).sum::<i64>();
        let (plain, mixed) = (off(average(false)), off(average(true)));
        assert!(mixed <= 9, "dithered average is {mixed} off");
        assert!(mixed < plain, "dithering ({mixed}) should beat the nearest colour ({plain})");
    }
}

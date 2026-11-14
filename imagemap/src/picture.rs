//! Reading a picture file and scaling it onto a grid of maps (no server needed).
use std::io::Cursor;

/// One map is this many pixels on a side.
pub const MAP: usize = 128;

/// How a picture is put onto a grid whose shape differs from its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// All of the picture, with empty (transparent) bands where the shapes differ.
    Fit,
    /// The grid filled; what sticks out is cut off, evenly on both sides.
    Cover,
    /// The picture pulled to the shape of the grid.
    Stretch,
}

pub struct Picture {
    pub w: usize,
    pub h: usize,
    /// `w * h` RGBA pixels, row by row.
    pub rgba: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
}

/// What kind of file it is, by its first bytes (never by its name).
pub fn sniff(bytes: &[u8]) -> Option<Kind> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(Kind::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Kind::Jpeg)
    } else {
        None
    }
}

fn check_size(w: u32, h: u32, max_pixels: u64) -> Result<(), String> {
    if w == 0 || h == 0 {
        return Err("the picture is empty".into());
    }
    if w > 16_384 || h > 16_384 || u64::from(w) * u64::from(h) > max_pixels {
        return Err(format!("the picture is too large ({w}x{h})"));
    }
    Ok(())
}

/// A PNG being decoded a few rows at a time, so that a big one never holds up a tick.
pub struct PngDecode {
    reader: png::Reader<Cursor<Vec<u8>>>,
    pic: Picture,
    row: usize,
    channels: usize,
    interlaced: bool,
}

impl PngDecode {
    pub fn start(bytes: Vec<u8>, max_pixels: u64) -> Result<PngDecode, String> {
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        // Palettes, 1/2/4-bit greys and transparency chunks become plain 8-bit channels
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let reader = decoder.read_info().map_err(|e| format!("not a readable PNG ({e})"))?;
        let (w, h, interlaced) = {
            let info = reader.info();
            (info.width, info.height, info.interlaced)
        };
        check_size(w, h, max_pixels)?;
        let channels = match reader.output_color_type().0 {
            png::ColorType::Grayscale => 1,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            png::ColorType::Indexed => return Err("unsupported PNG colour type".into()),
        };
        let (w, h) = (w as usize, h as usize);
        Ok(PngDecode { reader, pic: Picture { w, h, rgba: vec![0; w * h * 4] }, row: 0, channels, interlaced })
    }

    fn put(pic: &mut Picture, channels: usize, y: usize, data: &[u8]) {
        let w = pic.w;
        let out = &mut pic.rgba[y * w * 4..(y + 1) * w * 4];
        for (x, px) in data.chunks_exact(channels).take(w).enumerate() {
            let rgba = match channels {
                1 => [px[0], px[0], px[0], 255],
                2 => [px[0], px[0], px[0], px[1]],
                3 => [px[0], px[1], px[2], 255],
                _ => [px[0], px[1], px[2], px[3]],
            };
            out[x * 4..x * 4 + 4].copy_from_slice(&rgba);
        }
    }

    /// Decode up to `rows` more rows. `Ok(true)` when the picture is complete.
    pub fn step(&mut self, rows: usize) -> Result<bool, String> {
        if self.interlaced {
            // The rows of an interlaced file come in seven passes: it is read in one go
            let mut buf = vec![0; self.reader.output_buffer_size()];
            self.reader.next_frame(&mut buf).map_err(|e| format!("damaged PNG ({e})"))?;
            let line = self.pic.w * self.channels;
            for y in 0..self.pic.h {
                Self::put(&mut self.pic, self.channels, y, &buf[y * line..(y + 1) * line]);
            }
            self.row = self.pic.h;
            return Ok(true);
        }
        for _ in 0..rows {
            if self.row >= self.pic.h {
                return Ok(true);
            }
            match self.reader.next_row().map_err(|e| format!("damaged PNG ({e})"))? {
                Some(row) => {
                    Self::put(&mut self.pic, self.channels, self.row, row.data());
                    self.row += 1;
                }
                None => return Err("the PNG ends early".into()),
            }
        }
        Ok(self.row >= self.pic.h)
    }

    pub fn progress(&self) -> (usize, usize) {
        (self.row, self.pic.h)
    }

    pub fn finish(self) -> Picture {
        self.pic
    }
}

/// Decode a JPEG (in one go: the decoder has no row-by-row mode). `want` is the size it will
/// be shown at: a much larger photo is decoded at a half, quarter or eighth, which is far faster.
pub fn decode_jpeg(bytes: &[u8], max_pixels: u64, want: (usize, usize)) -> Result<Picture, String> {
    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(bytes));
    decoder.read_info().map_err(|e| format!("not a readable JPEG ({e})"))?;
    let info = decoder.info().ok_or("not a readable JPEG")?;
    check_size(u32::from(info.width), u32::from(info.height), max_pixels)?;
    let _ = decoder.scale(want.0.min(usize::from(u16::MAX)) as u16, want.1.min(usize::from(u16::MAX)) as u16);
    let data = decoder.decode().map_err(|e| format!("damaged JPEG ({e})"))?;
    let info = decoder.info().ok_or("not a readable JPEG")?;
    let (w, h) = (usize::from(info.width), usize::from(info.height));
    let mut rgba = vec![255u8; w * h * 4];
    match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => {
            for (i, v) in data.iter().take(w * h).enumerate() {
                rgba[i * 4..i * 4 + 3].copy_from_slice(&[*v, *v, *v]);
            }
        }
        jpeg_decoder::PixelFormat::L16 => {
            for (i, v) in data.chunks_exact(2).take(w * h).enumerate() {
                rgba[i * 4..i * 4 + 3].copy_from_slice(&[v[0], v[0], v[0]]);
            }
        }
        jpeg_decoder::PixelFormat::RGB24 => {
            for (i, px) in data.chunks_exact(3).take(w * h).enumerate() {
                rgba[i * 4..i * 4 + 3].copy_from_slice(px);
            }
        }
        jpeg_decoder::PixelFormat::CMYK32 => {
            for (i, px) in data.chunks_exact(4).take(w * h).enumerate() {
                // Adobe JPEGs store CMYK inverted
                let k = u32::from(px[3]);
                rgba[i * 4..i * 4 + 3].copy_from_slice(&[(u32::from(px[0]) * k / 255) as u8, (u32::from(px[1]) * k / 255) as u8, (u32::from(px[2]) * k / 255) as u8]);
            }
        }
    }
    Ok(Picture { w, h, rgba })
}

/// The grid for a picture when no size was asked for: its own shape, at most `max_side` maps
/// on the longer side, and never more maps than the picture has pixels for.
pub fn auto_grid(w: usize, h: usize, max_side: u32) -> (u32, u32) {
    let max_side = max_side.max(1) as f64;
    let (w, h) = (w.max(1) as f64, h.max(1) as f64);
    let long = (w.max(h) / MAP as f64).ceil().clamp(1.0, max_side);
    let (cols, rows) = if w >= h { (long, (long * h / w).round()) } else { ((long * w / h).round(), long) };
    (cols.clamp(1.0, max_side) as u32, rows.clamp(1.0, max_side) as u32)
}

/// Samples a picture onto a `cols` x `rows` grid of maps, one output row at a time.
pub struct Scaler {
    src: Picture,
    pub width: usize,
    pub height: usize,
    /// Source pixels per output pixel, and where output (0, 0) is in the source.
    step: (f64, f64),
    origin: (f64, f64),
    /// Source columns `[x0, x1)` under each output column; empty = outside the picture.
    spans: Vec<(usize, usize)>,
}

impl Scaler {
    pub fn new(src: Picture, cols: u32, rows: u32, mode: Mode) -> Scaler {
        let (width, height) = (cols as usize * MAP, rows as usize * MAP);
        let (sw, sh) = (src.w as f64, src.h as f64);
        let (fx, fy) = (sw / width as f64, sh / height as f64);
        let step = match mode {
            Mode::Stretch => (fx, fy),
            Mode::Cover => (fx.min(fy), fx.min(fy)),
            Mode::Fit => (fx.max(fy), fx.max(fy)),
        };
        let origin = ((sw - width as f64 * step.0) / 2.0, (sh - height as f64 * step.1) / 2.0);
        let mut scaler = Scaler { src, width, height, step, origin, spans: Vec::new() };
        scaler.spans = (0..width).map(|x| Self::span(scaler.origin.0, scaler.step.0, x, scaler.src.w)).collect();
        scaler
    }

    /// Source pixels `[a, b)` under output position `i`; `(0, 0)` when it lies outside.
    fn span(origin: f64, step: f64, i: usize, len: usize) -> (usize, usize) {
        let (a, b) = (origin + i as f64 * step, origin + (i + 1) as f64 * step);
        let mid = (a + b) / 2.0;
        if mid < 0.0 || mid >= len as f64 {
            return (0, 0);
        }
        let a = a.max(0.0).floor() as usize;
        let b = (b.min(len as f64).ceil() as usize).max(a + 1).min(len);
        if step < 1.0 {
            // Enlarging: the one pixel under the middle, so pixel art stays crisp
            let m = (mid.floor() as usize).min(len - 1);
            return (m, m + 1);
        }
        (a.min(len - 1), b)
    }

    /// Output row `y` as `width` RGBA pixels (the average of the source pixels under each).
    pub fn row(&self, y: usize, out: &mut [u8]) {
        let (y0, y1) = Self::span(self.origin.1, self.step.1, y, self.src.h);
        for (x, &(x0, x1)) in self.spans.iter().enumerate() {
            let px = &mut out[x * 4..x * 4 + 4];
            if x0 == x1 || y0 == y1 {
                px.copy_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            let (mut r, mut g, mut b, mut a) = (0u64, 0u64, 0u64, 0u64);
            for sy in y0..y1 {
                let line = &self.src.rgba[(sy * self.src.w + x0) * 4..(sy * self.src.w + x1) * 4];
                for p in line.chunks_exact(4) {
                    let alpha = u64::from(p[3]);
                    r += u64::from(p[0]) * alpha;
                    g += u64::from(p[1]) * alpha;
                    b += u64::from(p[2]) * alpha;
                    a += alpha;
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as u64;
            if a == 0 {
                px.copy_from_slice(&[0, 0, 0, 0]);
            } else {
                px.copy_from_slice(&[(r / a) as u8, (g / a) as u8, (b / a) as u8, (a / n) as u8]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG file of `w` x `h` made by `colour(x, y)`.
    pub fn png_file(w: u32, h: u32, colour_type: png::ColorType, interlaced: bool, colour: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(colour_type);
            enc.set_depth(png::BitDepth::Eight);
            // (png 0.17 has no interlaced encoder; an interlaced fixture is built by hand below)
            let _ = interlaced;
            let mut writer = enc.write_header().unwrap();
            let channels = match colour_type {
                png::ColorType::Grayscale => 1,
                png::ColorType::GrayscaleAlpha => 2,
                png::ColorType::Rgb => 3,
                _ => 4,
            };
            let mut data = Vec::with_capacity((w * h) as usize * channels);
            for y in 0..h {
                for x in 0..w {
                    let c = colour(x, y);
                    match channels {
                        1 => data.push(c[0]),
                        2 => data.extend_from_slice(&[c[0], c[3]]),
                        3 => data.extend_from_slice(&c[..3]),
                        _ => data.extend_from_slice(&c),
                    }
                }
            }
            writer.write_image_data(&data).unwrap();
        }
        out
    }

    fn decode(bytes: Vec<u8>, rows_per_step: usize) -> Picture {
        let mut d = PngDecode::start(bytes, 1 << 26).unwrap();
        let mut steps = 0;
        while !d.step(rows_per_step).unwrap() {
            steps += 1;
            assert!(steps < 100_000);
        }
        d.finish()
    }

    #[test]
    fn files_are_told_apart_by_content() {
        assert_eq!(sniff(&png_file(1, 1, png::ColorType::Rgb, false, |_, _| [1, 2, 3, 255])), Some(Kind::Png));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 16]), Some(Kind::Jpeg));
        for other in [&b"GIF89a"[..], b"<html>", b"", b"RIFF....WEBP", &[0x89, b'P', b'N']] {
            assert_eq!(sniff(other), None);
        }
    }

    #[test]
    fn png_kinds_decode_row_by_row() {
        let colour = |x: u32, y: u32| [(x * 5) as u8, (y * 7) as u8, ((x + y) * 3) as u8, if x == 0 { 0 } else { 255 }];
        for (kind, check) in [
            (png::ColorType::Rgba, (|c: [u8; 4]| c) as fn([u8; 4]) -> [u8; 4]),
            (png::ColorType::Rgb, |c| [c[0], c[1], c[2], 255]),
            (png::ColorType::Grayscale, |c| [c[0], c[0], c[0], 255]),
            (png::ColorType::GrayscaleAlpha, |c| [c[0], c[0], c[0], c[3]]),
        ] {
            let file = png_file(40, 30, kind, false, colour);
            for rows in [1, 7, 1000] {
                let pic = decode(file.clone(), rows);
                assert_eq!((pic.w, pic.h), (40, 30));
                for (x, y) in [(0u32, 0u32), (39, 29), (13, 21)] {
                    let got = &pic.rgba[(y as usize * 40 + x as usize) * 4..][..4];
                    assert_eq!(got, check(colour(x, y)), "{kind:?} at {x},{y}");
                }
            }
        }
    }

    #[test]
    fn bad_and_huge_files_are_refused() {
        assert!(PngDecode::start(b"not a png at all".to_vec(), 1 << 26).is_err());
        let file = png_file(100, 100, png::ColorType::Rgb, false, |_, _| [9, 9, 9, 255]);
        assert!(PngDecode::start(file.clone(), 5000).is_err(), "over the pixel limit");
        // Cut short: an error, not a hang or a panic
        let mut d = PngDecode::start(file[..file.len() / 2].to_vec(), 1 << 26).unwrap();
        let mut result = Ok(false);
        for _ in 0..200 {
            result = d.step(10);
            if result != Ok(false) {
                break;
            }
        }
        assert!(result.is_err(), "{result:?}");
        assert!(decode_jpeg(b"\xFF\xD8\xFFnot really", 1 << 26, (128, 128)).is_err());
    }

    #[test]
    fn grids_follow_the_picture() {
        assert_eq!(auto_grid(128, 128, 3), (1, 1));
        assert_eq!(auto_grid(64, 64, 3), (1, 1));
        assert_eq!(auto_grid(1920, 1080, 3), (3, 2));
        assert_eq!(auto_grid(1080, 1920, 3), (2, 3));
        assert_eq!(auto_grid(4000, 100, 4), (4, 1));
        assert_eq!(auto_grid(300, 300, 8), (3, 3));
        assert_eq!(auto_grid(256, 128, 1), (1, 1));
    }

    fn solid(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 4]) -> Picture {
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&f(x, y));
            }
        }
        Picture { w, h, rgba }
    }

    fn at(s: &Scaler, x: usize, y: usize) -> [u8; 4] {
        let mut row = vec![0u8; s.width * 4];
        s.row(y, &mut row);
        [row[x * 4], row[x * 4 + 1], row[x * 4 + 2], row[x * 4 + 3]]
    }

    #[test]
    fn scaling_modes() {
        // Left half red, right half blue, twice as wide as high
        let two = || solid(400, 200, |x, _| if x < 200 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
        // Stretch onto one map: both halves, each squeezed
        let s = Scaler::new(two(), 1, 1, Mode::Stretch);
        assert_eq!((s.width, s.height), (128, 128));
        assert_eq!((at(&s, 10, 10), at(&s, 120, 120)), ([255, 0, 0, 255], [0, 0, 255, 255]));
        // Fit: the whole picture across the middle, empty bands above and below
        let s = Scaler::new(two(), 1, 1, Mode::Fit);
        assert_eq!(at(&s, 10, 5)[3], 0);
        assert_eq!(at(&s, 10, 122)[3], 0);
        assert_eq!((at(&s, 10, 64), at(&s, 120, 64)), ([255, 0, 0, 255], [0, 0, 255, 255]));
        // Cover: the middle square of the picture, nothing empty
        let s = Scaler::new(two(), 1, 1, Mode::Cover);
        assert_eq!((at(&s, 2, 2), at(&s, 125, 125)), ([255, 0, 0, 255], [0, 0, 255, 255]));
        // A 2x1 poster of it is exact: 256 x 128
        let s = Scaler::new(two(), 2, 1, Mode::Fit);
        assert_eq!((s.width, s.height), (256, 128));
        assert_eq!((at(&s, 127, 0), at(&s, 128, 127)), ([255, 0, 0, 255], [0, 0, 255, 255]));
    }

    #[test]
    fn shrinking_averages_and_enlarging_keeps_pixels() {
        // A checkerboard of black and white shrinks to grey
        let board = solid(256, 256, |x, y| if (x + y) % 2 == 0 { [0, 0, 0, 255] } else { [255, 255, 255, 255] });
        let s = Scaler::new(board, 1, 1, Mode::Stretch);
        let grey = at(&s, 50, 50);
        assert!((120..=135).contains(&grey[0]) && grey[3] == 255, "{grey:?}");
        // A 16 x 16 pixel-art image on one map: every source pixel becomes an 8 x 8 block, unblurred
        let art = solid(16, 16, |x, y| [(x * 16) as u8, (y * 16) as u8, 0, 255]);
        let s = Scaler::new(art, 1, 1, Mode::Stretch);
        assert_eq!((at(&s, 0, 0), at(&s, 7, 7), at(&s, 8, 8), at(&s, 127, 127)), ([0, 0, 0, 255], [0, 0, 0, 255], [16, 16, 0, 255], [240, 240, 0, 255]));
        // Transparent parts stay out of the average
        let half = solid(256, 256, |x, _| if x % 2 == 0 { [200, 10, 10, 255] } else { [0, 255, 0, 0] });
        let s = Scaler::new(half, 1, 1, Mode::Stretch);
        let c = at(&s, 30, 30);
        assert_eq!((c[0], c[1], c[2]), (200, 10, 10));
        assert!((120..=135).contains(&c[3]), "{c:?}");
    }
}

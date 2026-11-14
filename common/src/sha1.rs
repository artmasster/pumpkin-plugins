//! SHA-1 of data that arrives in pieces (resource packs: Minecraft names a pack by this hash).
//! Not for security: SHA-1 is what the game protocol asks for.

pub struct Sha1 {
    state: [u32; 5],
    block: [u8; 64],
    fill: usize,
    len: u64,
}

impl Default for Sha1 {
    fn default() -> Self {
        Sha1 { state: [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0], block: [0; 64], fill: 0, len: 0 }
    }
}

impl Sha1 {
    pub fn new() -> Self {
        Sha1::default()
    }

    /// Bytes taken so far.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.len += data.len() as u64;
        if self.fill > 0 {
            let take = (64 - self.fill).min(data.len());
            self.block[self.fill..self.fill + take].copy_from_slice(&data[..take]);
            self.fill += take;
            data = &data[take..];
            if self.fill < 64 {
                return;
            }
            let block = self.block;
            self.compress(&block);
            self.fill = 0;
        }
        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            self.compress(block.try_into().unwrap_or(&[0; 64]));
            data = rest;
        }
        self.block[..data.len()].copy_from_slice(data);
        self.fill = data.len();
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 80];
        for (i, chunk) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = self.state;
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (s, v) in self.state.iter_mut().zip([a, b, c, d, e]) {
            *s = s.wrapping_add(v);
        }
    }

    pub fn finish(mut self) -> [u8; 20] {
        let bits = self.len.wrapping_mul(8);
        self.update(&[0x80]);
        while self.fill != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        let mut out = [0u8; 20];
        for (i, s) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&s.to_be_bytes());
        }
        out
    }
}

/// 40 lowercase hex characters.
pub fn hex(digest: &[u8; 20]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(data: &[u8]) -> String {
        let mut h = Sha1::new();
        h.update(data);
        hex(&h.finish())
    }

    #[test]
    fn known_answers() {
        assert_eq!(of(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(of(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(of(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"), "84983e441c3bd26ebaae4aa1f95129e5e54670f1");
        assert_eq!(of(&[b'a'; 1_000_000]), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
    }

    #[test]
    fn pieces_give_the_same_answer() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i * 31 % 251) as u8).collect();
        let whole = of(&data);
        for size in [1, 3, 63, 64, 65, 127, 1000] {
            let mut h = Sha1::new();
            for piece in data.chunks(size) {
                h.update(piece);
            }
            assert_eq!(h.len(), data.len() as u64);
            assert_eq!(hex(&h.finish()), whole, "pieces of {size}");
        }
    }
}

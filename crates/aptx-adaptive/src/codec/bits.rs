//! MSB-first bit writing (spec `codec-core.md` §0).

use super::tables::Code;

/// Somewhere bits go: a [`BitWriter`], or a [`BitCounter`] for sizing.
pub trait Sink {
    fn put(&mut self, value: u32, n: u32);

    /// # Panics
    /// On a zero-length (invalid) code, which means the caller fed a symbol the table can't
    /// code.
    fn code(&mut self, c: Code) {
        assert!(c.len > 0, "symbol has no Huffman code");
        self.put(c.code, u32::from(c.len));
    }
}

#[derive(Debug, Clone, Default)]
pub struct BitWriter {
    bytes: Vec<u8>,
    /// Bits used in the last byte, 0 when byte aligned.
    used: u32,
}

impl Sink for BitWriter {
    fn put(&mut self, value: u32, n: u32) {
        for i in (0..n).rev() {
            if self.used == 0 {
                self.bytes.push(0);
            }
            let bit = ((value >> i) & 1) as u8;
            *self.bytes.last_mut().expect("pushed above") |= bit << (7 - self.used);
            self.used = (self.used + 1) % 8;
        }
    }
}

impl BitWriter {
    /// Empties the writer, keeping its buffer.
    pub fn clear(&mut self) {
        self.bytes.clear();
        self.used = 0;
    }

    /// Exp-Golomb order 0.
    pub fn exp_golomb(&mut self, n: u32) {
        let v = n + 1;
        let len = 32 - v.leading_zeros();
        self.put(0, len - 1);
        self.put(v, len);
    }

    pub fn bytes(&mut self, b: &[u8]) {
        if self.used == 0 {
            self.bytes.extend_from_slice(b);
        } else {
            for &x in b {
                self.put(u32::from(x), 8);
            }
        }
    }

    pub fn bit_len(&self) -> usize {
        self.bytes.len() * 8
            - if self.used == 0 {
                0
            } else {
                8 - self.used as usize
            }
    }

    /// The bytes so far, the last one zero-padded.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BitCounter(pub usize);

impl Sink for BitCounter {
    fn put(&mut self, _: u32, n: u32) {
        self.0 += n as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msb_first() {
        let mut w = BitWriter::default();
        w.put(1, 1);
        w.put(0b011, 3);
        w.exp_golomb(0);
        w.exp_golomb(2);
        assert_eq!(w.bit_len(), 8);
        w.bytes(&[0xAB]);
        assert_eq!(w.as_bytes(), [0b1011_1011, 0xAB]);
        w.clear();
        w.put(1, 1);
        assert_eq!((w.as_bytes(), w.bit_len()), (&[0x80][..], 1));
    }
}

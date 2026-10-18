//! Entropy coding: bit writer/reader, Golomb-Rice, and the per-band value
//! coder.
//!
//! Evidence:
//!
//! * The decoder's unpacker `FUN_00408110` (decompiled) decodes each band
//!   value in two parts: a variable-length **prefix symbol** (a nibble-
//!   indexed decision tree followed by the stored `(len, v0, v1)` leaf)
//!   and, when the band's bit width exceeds 4, `nbits - 4` **raw extension
//!   bits** appended to it:
//!   `value = prefix | (raw << k)` (see `FUN_00408330`).
//! * Golomb-Rice appears in the encoder symbols `AxEntropyComposerGRice_C10`,
//!   `AxGRiceSetup`, `AxHist_GolombRiceEncode`, `AxWriteExGRK`/`AxWriteK`
//!   with the limit table `AXGR_LIMITQUOTIENTWITHWORDSIZE`, and the Huffman
//!   side in `AxEntropyComposerHuff_TID0` + the `g_aPacker_cfg0_Huff*`
//!   tables.
//!
//! This module implements a self-consistent coding of the quantiser codes:
//! per band, per frame, each sample's code is written as fixed-width raw
//! bits (the same "raw bit" primitive the vendor uses for extensions), with
//! Golomb-Rice provided for the sparse (zero-run) case.  The vendor's exact
//! Huffman trees are extracted (tables/huffman.h) but their stream syntax
//! mixing is still an open item; the encoder and decoder here agree on the
//! form below, and the form mirrors the recovered prefix|raw structure.
//!
//! SPDX-License-Identifier: MIT

/// MSB-first bit writer (matches the decoder's bit-reservoir convention:
/// bits are consumed from the top of the buffer).
#[derive(Default, Clone)]
pub struct BitWriter {
    buf: Vec<u8>,
    bitpos: u32, // bits used in the last byte (0..8)
}

impl BitWriter {
    /// New empty writer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bits written so far.
    pub fn len_bits(&self) -> usize {
        if self.bitpos == 0 {
            self.buf.len() * 8
        } else {
            (self.buf.len() - 1) * 8 + self.bitpos as usize
        }
    }

    /// Write the low `n` bits of `v` (n <= 32), MSB first.
    pub fn write(&mut self, v: u32, n: u32) {
        debug_assert!(n <= 32);
        for i in (0..n).rev() {
            let bit = (v >> i) & 1;
            if self.bitpos == 0 {
                self.buf.push(0);
                self.bitpos = 0;
            }
            let last = self.buf.len() - 1;
            self.buf[last] |= (bit as u8) << (7 - self.bitpos);
            self.bitpos = (self.bitpos + 1) % 8;
        }
    }

    /// Pad with zeros to a byte boundary.
    pub fn align(&mut self) {
        while self.bitpos != 0 {
            self.write(0, 1);
        }
    }

    /// Pad with zeros to `nbytes`, panic-free (returns false if already
    /// larger).
    pub fn pad_to(&mut self, nbytes: usize) -> bool {
        self.align();
        if self.buf.len() > nbytes {
            return false;
        }
        self.buf.resize(nbytes, 0);
        true
    }

    /// The written bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Take the written bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

/// MSB-first bit reader.
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize, // bit position
}

impl<'a> BitReader<'a> {
    /// Wrap a byte slice.
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    /// Bits consumed so far.
    pub fn pos_bits(&self) -> usize {
        self.pos
    }

    /// Bits still available.
    pub fn remaining_bits(&self) -> usize {
        self.data.len() * 8 - self.pos
    }

    /// Read the next `n` bits as the low bits of the result (MSB first).
    pub fn read(&mut self, n: u32) -> Option<u32> {
        if (n as usize) > self.remaining_bits() {
            return None;
        }
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.data[self.pos / 8];
            let bit = (byte >> (7 - (self.pos % 8))) & 1;
            v = (v << 1) | bit as u32;
            self.pos += 1;
        }
        Some(v)
    }

    /// Align to the next byte boundary.
    pub fn align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }

    /// Reposition to an absolute bit offset (the vendor's reader peeks
    /// nibbles and then consumes the leaf's exact code length).
    pub fn set_pos(&mut self, pos: usize) -> Option<()> {
        if pos > self.data.len() * 8 {
            return None;
        }
        self.pos = pos;
        Some(())
    }
}

/// The reference decoder's Huffman coding, over the extracted trees
/// (`crate::huffman`, selector table at `0x42a890` in `test-decoder.exe`).
///
/// [`Table::decode`] is a transcription of `FUN_00408110`: nibble-indexed
/// descent through i16 nodes (negatives link, non-negatives are leaves with
/// a `{len, v0, v1}` record), consuming exactly `len` bits.  The two values
/// are the channel pair prefixes; extension bits follow at the call site
/// (`FUN_00408330` appends `nbits - 4` raw bits per channel).
pub mod huff {
    use super::{BitReader, BitWriter};
    use crate::huffman::{
        HUFF_CODES_0, HUFF_CODES_1, HUFF_CODES_2, HUFF_CODES_3, HUFF_RAW_0, HUFF_RAW_1, HUFF_RAW_2,
        HUFF_RAW_3, HUFF_SELECTOR,
    };

    /// One extracted table: raw bytes + enumerated codes
    /// `(v0, v1, path_nibbles, path_bits, len)`.
    pub struct Table {
        /// The tree bytes as they appear in the binary.
        pub raw: &'static [u8],
        /// Enumerated codes for the encoder side.
        pub codes: &'static [(i16, i16, u8, u32, u8)],
    }

    /// The four distinct extracted tables (selectors 1, 2, 3, 4..15).
    pub static TABLES: [Table; 4] = [
        Table {
            raw: &HUFF_RAW_0,
            codes: &HUFF_CODES_0,
        },
        Table {
            raw: &HUFF_RAW_1,
            codes: &HUFF_CODES_1,
        },
        Table {
            raw: &HUFF_RAW_2,
            codes: &HUFF_CODES_2,
        },
        Table {
            raw: &HUFF_RAW_3,
            codes: &HUFF_CODES_3,
        },
    ];

    /// Resolve a stream selector byte (the per-band code index) to a table.
    pub fn table_for_selector(sel: usize) -> Option<&'static Table> {
        let idx = (*HUFF_SELECTOR.get(sel)?)?;
        TABLES.get(idx)
    }

    impl Table {
        /// Decode one `(v0, v1)` symbol (the vendor's `FUN_00408110`).
        pub fn decode(&self, r: &mut BitReader) -> Option<(i16, i16)> {
            let save = r.pos_bits();
            let mut pos = 0usize;
            let mut prev: i16 = 0;
            loop {
                let nib = r.read(4)? as i16;
                let idx = nib as isize - prev as isize;
                let npos = pos as isize + idx * 2;
                if npos < 0 || (npos as usize) + 2 > self.raw.len() {
                    return None;
                }
                pos = npos as usize;
                let node = i16::from_le_bytes([self.raw[pos], self.raw[pos + 1]]);
                if node < 0 {
                    prev = node;
                    continue;
                }
                let rec = pos + (node as usize) * 2;
                if rec + 6 > self.raw.len() {
                    return None;
                }
                let len = i16::from_le_bytes([self.raw[rec], self.raw[rec + 1]]) as usize;
                let v0 = i16::from_le_bytes([self.raw[rec + 2], self.raw[rec + 3]]);
                let v1 = i16::from_le_bytes([self.raw[rec + 4], self.raw[rec + 5]]);
                r.set_pos(save + len)?;
                return Some((v0, v1));
            }
        }

        /// Encode one `(v0, v1)` symbol using the shortest enumerated code.
        pub fn encode(&self, v0: i16, v1: i16, w: &mut BitWriter) -> bool {
            for &(a, b, nibbles, path, _len) in self.codes {
                if a == v0 && b == v1 {
                    let bits = nibbles as u32 * 4;
                    w.write(path, bits);
                    return true;
                }
            }
            false
        }
    }
}

/// `AxWriteUExGR32` from the R3 encoder: order-0 exp-Golomb of `v` (the DSP
/// codes `v+1`), with the vendor's >16-bit escape form — a single zero bit
/// followed by `v+1` in its natural bit length.  Used by the R3 cost path.
pub fn write_uex_gr32(w: &mut BitWriter, v: u32) {
    if v == 0 {
        w.write(1, 1);
        return;
    }
    let x = v.wrapping_add(1);
    let bl = 32 - x.leading_zeros();
    if bl > 16 {
        w.write(0, 1);
        w.write(x, bl);
    } else {
        w.write(x, 2 * bl - 1);
    }
}

/// Golomb-Rice encode `v` with parameter `k` (q unary bits, then the k LSBs).
pub fn gr_encode(w: &mut BitWriter, v: u32, k: u32) {
    let q = v >> k;
    for _ in 0..q {
        w.write(1, 1);
    }
    w.write(0, 1);
    if k > 0 {
        w.write(v & ((1 << k) - 1), k);
    }
}

/// Golomb-Rice decode with parameter `k`.
pub fn gr_decode(r: &mut BitReader, k: u32) -> Option<u32> {
    let mut q = 0u32;
    loop {
        match r.read(1)? {
            1 => {
                q += 1;
                if q > (1 << 20) {
                    return None; // runaway
                }
            }
            _ => break,
        }
    }
    let rem = if k > 0 { r.read(k)? } else { 0 };
    Some((q << k) | rem)
}

/// A dead-simple band coder: fixed-width codes, MSB first.  The frame
/// allocation stage decides the width per band; this coder just packs.
pub fn encode_band_fixed(w: &mut BitWriter, codes: &[u32], bits: u32) {
    for &c in codes {
        w.write(c, bits);
    }
}

/// Inverse of [`encode_band_fixed`].
pub fn decode_band_fixed(r: &mut BitReader, n: usize, bits: u32) -> Option<Vec<u32>> {
    (0..n).map(|_| r.read(bits)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitwriter_roundtrip() {
        let mut w = BitWriter::new();
        w.write(0b1011, 4);
        w.write(0b0000_0111, 8);
        w.write(1, 1);
        let bytes = w.clone().into_bytes();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read(4), Some(0b1011));
        assert_eq!(r.read(8), Some(0b0000_0111));
        assert_eq!(r.read(1), Some(1));
    }

    #[test]
    fn golomb_rice_roundtrip() {
        let mut w = BitWriter::new();
        let vals: Vec<u32> = vec![0, 1, 2, 3, 7, 8, 15, 16, 100, 4095];
        for &v in &vals {
            gr_encode(&mut w, v, 4);
        }
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        for &v in &vals {
            assert_eq!(gr_decode(&mut r, 4), Some(v));
        }
    }

    #[test]
    fn extracted_huffman_walker_matches_enumerated_codes() {
        // every enumerated code must decode back to its recorded (v0, v1)
        for table in huff::TABLES.iter() {
            for &(v0, v1, nibbles, path, _len) in table.codes {
                let mut w = BitWriter::new();
                w.write(path, nibbles as u32 * 4);
                w.write(0, 32); // padding for the peek-read of the next nibbles
                let bytes = w.into_bytes();
                let mut r = BitReader::new(&bytes);
                let got = table.decode(&mut r).expect("decode");
                assert_eq!(got, (v0, v1), "table mismatch for code {path:#x}");
            }
        }
    }

    #[test]
    fn extracted_huffman_encoder_roundtrips() {
        for table in huff::TABLES.iter() {
            for &(v0, v1, _, _, _) in table.codes.iter().take(64) {
                let mut w = BitWriter::new();
                assert!(table.encode(v0, v1, &mut w), "encode {v0},{v1}");
                w.write(0, 32);
                let bytes = w.into_bytes();
                let mut r = BitReader::new(&bytes);
                assert_eq!(table.decode(&mut r), Some((v0, v1)));
            }
        }
    }

    #[test]
    fn selector_resolves_tables() {
        assert!(huff::table_for_selector(0).is_none());
        for sel in 1..16 {
            assert!(huff::table_for_selector(sel).is_some(), "selector {sel}");
        }
    }

    #[test]
    fn fixed_band_roundtrip() {
        let codes: Vec<u32> = (0..64).map(|i| (i * 7) & 0x3f).collect();
        let mut w = BitWriter::new();
        encode_band_fixed(&mut w, &codes, 6);
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_band_fixed(&mut r, 64, 6), Some(codes));
    }
}

#[cfg(test)]
mod r3_tests {
    use super::*;

    fn bits(v: u32) -> (usize, Vec<u8>) {
        let mut w = BitWriter::new();
        write_uex_gr32(&mut w, v);
        (w.len_bits(), w.into_bytes())
    }

    #[test]
    fn uex_gr32_matches_vendor_coding() {
        assert_eq!(bits(0).0, 1);
        assert_eq!(bits(1).0, 3);
        assert_eq!(bits(1).1[0], 0b0100_0000);
        assert_eq!(bits(3).0, 5);
        assert_eq!(bits(3).1[0], 0b0010_0000);
        assert_eq!(bits(0x7fff).0, 31);
        assert_eq!(bits(0x8000).0, 31);
        assert_eq!(bits(0xffff).0, 18);
        assert_eq!(bits(0x10000).0, 18);
    }
}

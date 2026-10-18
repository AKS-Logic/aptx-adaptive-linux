//! The vendor frame writer (aptX Adaptive R2), structured after the
//! decompiled writer chain and verified bit-exact against the rig capture
//! (frame 1: head + alloc + 86 steps + dif).
//!
//! Two payload shapes share the writer:
//!
//! * **R2 HQ** ([`R2_FRAME`], 656 bytes, config 3, frame type 3): the
//!   25 ms shape the plugin has been sending.  Raw layout
//!   `[00][03|pad][a1][bitstream ...]`.
//! * **R2 LL** ([`LL_FRAME`], 348 bytes, config 4, frame type 4): the
//!   10 ms / 60-block LL row.  Raw layout `[00][04|pad][54][bitstream ...]`
//!   (0x54 = 348/4 - 3).  Config 4 codes the allocation without the
//!   config-3 escape extensions and without the zero-allocation escape.
//!
//! OTA layout (what the plugin emits): pairwise byte-swapped raw.
//!
//! Bitstream: `[overlap "1"][nibble 0xa][alloc16][steps][dif updates][flush]`
//! Steps: per step, fields 0..15; field f is written only when
//! `desc[f] != 0`, as a composer pair symbol (`sym = (v0<<4)|v1` plus 12+12
//! raw bits for descriptor 16).
//!
//! SPDX-License-Identifier: MIT

use crate::alloc_dif_tables::{HUFF_DIF_VAL, RANGE_DIF_VAL};
use crate::bitalloc_tables::{TABLE_A, TABLE_B};
use crate::composer::pair_bits;
use crate::entropy::BitWriter;

/// One step's values: `[field][(v0, v1)]`, 16-bit two's complement.
pub type StepVals = [[i32; 2]; 16];

/// Geometry of one OTA payload presentation.
///
/// Both shapes start with the 3-byte module header `[00][type|pad][len]`,
/// carry the same exp-Golomb carried-byte count, the 4-bit rate/stereo
/// allocation header and the TID0 composer pairs, and are byte-swapped in
/// 16-bit units for the wire.  They differ in the payload length, the frame
/// type byte, and whether the allocation uses the config-3 escapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameShape {
    /// Total codec payload bytes (656 for R2 HQ, 348 for R2 LL).
    pub bytes: usize,
    /// Header byte 1: codec frame type (3 = config 3 HQ, 4 = config 4 LL).
    pub frame_type: u8,
    /// Config-3 allocation escapes (3-bit width extensions and the
    /// zero-allocation escape).  Config 4 codes widths up to 9 directly.
    pub escapes: bool,
}

impl FrameShape {
    /// Header byte 2: the codec length in 4-byte units minus the 3-byte
    /// header (`bytes / 4 - 3`): 0xa1 for 656, 0x54 for 348.
    pub const fn length_byte(&self) -> u8 {
        (self.bytes / 4 - 3) as u8
    }

    /// Composer stream bytes: the payload minus the 3-byte header.
    pub const fn stream_bytes(&self) -> usize {
        self.bytes - 3
    }

    /// Composer budget in bits (the region the frame writer fills).
    pub const fn stream_bits(&self) -> usize {
        (self.bytes - 3) * 8
    }
}

/// R2 HQ: 656-byte payload, frame type 3, config-3 allocation escapes.
///
/// The 8-byte SPF container maps this shape to payload-table type 1.
pub const R2_FRAME: FrameShape = FrameShape {
    bytes: 656,
    frame_type: 3,
    escapes: true,
};

/// R2 LL: 348-byte payload, frame type 4, config-4 allocation coding.
///
/// The LL row is 30 composer steps (60 blocks = 480 samples = 10 ms at
/// 48 kHz); the 8-byte SPF container maps it to payload-table type 0.
/// Config 4 has no allocation escapes, so every width must be <= 9.
pub const LL_FRAME: FrameShape = FrameShape {
    bytes: 348,
    frame_type: 4,
    escapes: false,
};

/// 16-field allocation writer (`AxPackerCfg04_WriterBitAllocation` with the
/// 8-band pair tables `[A,A,B,B,A,A,B,B]`): per pair, the Huffman code of the
/// two coded nibbles (escape nibble 8 + 3 extension bits when coded > 7);
/// an 8-bit trailer only when all fields are zero.
///
/// This is the config-3 form: escapes on.  Use [`write_alloc16_cfg`] with
/// `escapes = false` for config 4 (the LL frame), which codes nibble 8 as
/// width 9 and writes neither the extension groups nor the zero trailer.
pub fn write_alloc16(widths: &[u8; 16], trailer: u8, w: &mut BitWriter) {
    write_alloc16_cfg(widths, trailer, true, w);
}

/// [`write_alloc16`] with the config-3 escapes selectable.
///
/// With `escapes = false` (config 4) the pair symbol is the terminal code:
/// nibble 8 means width 9, no 3-bit extension follows, and an all-zero
/// allocation is *not* followed by the 8-bit zero-allocation escape (config
/// 4 has no such escape).  Every width must be <= 9; wider values cannot be
/// represented and would silently change meaning.
pub fn write_alloc16_cfg(widths: &[u8; 16], trailer: u8, escapes: bool, w: &mut BitWriter) {
    let mut sum: u32 = 0;
    for p in 0..8 {
        let coded = |f: usize| -> u32 {
            let x = widths[f];
            if x >= 2 {
                (x - 1) as u32
            } else {
                0
            }
        };
        let (ca, cb) = (coded(2 * p), coded(2 * p + 1));
        sum += ca + cb;
        let (na, nb) = (ca.min(8), cb.min(8));
        let table = if p % 4 < 2 { &TABLE_A } else { &TABLE_B };
        let (code, len) = table[((na << 4) | nb) as usize];
        w.write(code as u32, len as u32);
        if !escapes {
            continue;
        }
        let mut ext = 0u32;
        let mut nbits = 0u32;
        if ca > 7 {
            ext = (ca - 8) & 7;
            nbits = 3;
        }
        if cb > 7 {
            ext = ((cb - 8) & 7) | (ext << 3);
            nbits += 3;
        }
        if nbits > 0 {
            w.write(ext, nbits);
        }
    }
    if escapes && sum == 0 {
        w.write(trailer as u32, 8);
    }
}

/// Differential allocation update (`AxPackerCfg04_WriterBitAllocation_Dif`):
/// per field, the Huffman-coded 4-bit difference of the coded width against
/// `prev_coded` (clamped into `RANGE_DIF_VAL[prev]`), with the 3-bit
/// below-min / 3-bit above-max extension groups.
///
/// This is the config-3 form: escapes on.  Config 4 uses
/// [`write_alloc_dif_cfg`] with `escapes = false`.
pub fn write_alloc_dif(prev_coded: &mut [u8; 16], widths: &[u8; 16], w: &mut BitWriter) {
    write_alloc_dif_cfg(prev_coded, widths, true, w);
}

/// [`write_alloc_dif`] with the config-3 escapes selectable.
///
/// Without escapes (config 4) the 4-bit difference is written directly: no
/// range clamping, no 3-bit groups and no zero-allocation escape.
pub fn write_alloc_dif_cfg(
    prev_coded: &mut [u8; 16],
    widths: &[u8; 16],
    escapes: bool,
    w: &mut BitWriter,
) {
    for f in 0..16 {
        let coded = if widths[f] >= 2 { widths[f] - 1 } else { 0 };
        let prev = prev_coded[f].min(15) as usize;
        let newc = coded as i32;
        if escapes {
            let (mn, mx) = RANGE_DIF_VAL[prev];
            // the writer emits an extension group whenever the new coded
            // width reaches or crosses a range bound (equality included)
            let below = newc <= mn;
            let above = newc >= mx;
            let mut clamped = newc;
            if clamped < mn {
                clamped = mn;
            }
            if clamped > mx {
                clamped = mx;
            }
            let idx = ((clamped - prev as i32) & 0xf) as usize;
            let (code, len) = HUFF_DIF_VAL[prev][idx];
            w.write(code as u32, len as u32);
            if below {
                let ext = ((mn - newc) & 7) as u32;
                w.write(ext, 3);
            } else if above {
                let ext = ((newc - mx) & 7) as u32;
                w.write(ext, 3);
            }
        } else {
            let idx = ((newc - prev as i32) & 0xf) as usize;
            let (code, len) = HUFF_DIF_VAL[prev][idx];
            w.write(code as u32, len as u32);
        }
        prev_coded[f] = coded;
    }
}

/// Per-stream frame writer state.
pub struct FrameWriter {
    /// previous coded widths for the differential updates
    pub coded: [u8; 16],
    pub frames: u64,
    /// stream bit position of the head's end and of each dif written
    /// (diagnostics for the structural tests)
    pub last_head_end: usize,
    pub last_dif_pos: Vec<usize>,
    /// composer steps actually written in the most recent frame (the frame
    /// budget may cut the tail; callers that must code every block read this)
    pub last_steps_written: usize,
}

impl Default for FrameWriter {
    fn default() -> Self {
        Self {
            coded: [0; 16],
            frames: 0,
            last_head_end: 0,
            last_dif_pos: Vec::new(),
            last_steps_written: 0,
        }
    }
}

impl FrameWriter {
    /// Build one 656-byte R2 HQ OTA payload (config 3).
    ///
    /// * `widths` — 16 allocation fields (the +4 array)
    /// * `desc`   — 16 composer descriptors (0 = field inactive)
    /// * `steps`  — the per-step composer values
    /// * `dif_after` — step indices after which the differential update goes
    ///   (the composer's 32-step chunk boundaries)
    /// * `pad` — force the 0x80 container flag (a short stream is always
    ///   padded and flagged, whatever this says)
    pub fn write(
        &mut self,
        widths: &[u8; 16],
        desc: &[u8; 16],
        steps: &[StepVals],
        dif_after: &[usize],
        pad: bool,
    ) -> Vec<u8> {
        self.write_shape(&R2_FRAME, widths, desc, steps, dif_after, pad)
    }

    /// Build one 348-byte R2 LL OTA payload (config 4).
    ///
    /// The LL row is a single chunk: 30 steps (60 blocks = 480 samples =
    /// 10 ms at 48 kHz) fit under the composer's 64-block chunk cap, so no
    /// differential allocation update is written.  Widths must be 0 or
    /// 2..=9 — config 4 cannot code a band above 9 bits (the 3-bit escape
    /// that would express it belongs to config 3).
    ///
    /// * `widths` — 16 allocation fields
    /// * `desc`   — 16 composer descriptors (0 = field inactive)
    /// * `steps`  — the per-step composer values (normally 30)
    /// * `pad` — force the 0x80 container flag (a short stream is always
    ///   padded and flagged, whatever this says)
    pub fn write_ll(
        &mut self,
        widths: &[u8; 16],
        desc: &[u8; 16],
        steps: &[StepVals],
        pad: bool,
    ) -> Vec<u8> {
        debug_assert!(
            widths.iter().all(|&w| w <= 9),
            "config 4 widths must be <= 9: {widths:?}"
        );
        self.write_shape(&LL_FRAME, widths, desc, steps, &[], pad)
    }

    /// Build one payload in the given shape.
    fn write_shape(
        &mut self,
        shape: &FrameShape,
        widths: &[u8; 16],
        desc: &[u8; 16],
        steps: &[StepVals],
        dif_after: &[usize],
        pad: bool,
    ) -> Vec<u8> {
        debug_assert_eq!(shape.bytes % 2, 0, "16-bit unit swap needs even sizes");
        let stream_bytes = shape.stream_bytes();
        let limit_bits = shape.stream_bits();
        let mut w = BitWriter::new();
        w.write(1, 1); // overlap prefix: N = 0
        w.write(0xa, 4); // nibble: 48 kHz stereo (mode 3 path)
        write_alloc16_cfg(widths, 0, shape.escapes, &mut w);
        self.last_head_end = w.len_bits();
        self.last_dif_pos.clear();
        // the head allocation stores the coded widths: the differential
        // update compares against this same array (`+0xAC`)
        for f in 0..16 {
            self.coded[f] = if widths[f] >= 2 { widths[f] - 1 } else { 0 };
        }
        // Serialise the steps, then keep the longest prefix that fits whole.
        // A partially written pair desynchronises every decoder (the
        // validated parser rejects such a packet with "data ends mid-pair"),
        // and a dangling differential update would parse as an empty frame.
        let mut pending_steps: Vec<(usize, Vec<Vec<bool>>)> = Vec::with_capacity(steps.len());
        let mut used = w.len_bits();
        let mut keep = 0usize;
        for (i, s) in steps.iter().enumerate() {
            let mut pending: Vec<Vec<bool>> = Vec::with_capacity(16);
            let mut step_bits = 0usize;
            for f in 0..16 {
                if desc[f] == 0 {
                    continue;
                }
                let bits = pair_bits(
                    desc[f] as usize,
                    (s[f][0] as u32) & 0xffff,
                    (s[f][1] as u32) & 0xffff,
                );
                step_bits += bits.len();
                pending.push(bits);
            }
            let dif = dif_after.contains(&i) && i + 1 < steps.len();
            let extra = step_bits + if dif { 16 } else { 0 };
            if i > 0 && used + extra > limit_bits {
                break;
            }
            used += extra;
            pending_steps.push((i, pending));
            keep = i + 1;
        }
        self.last_steps_written = keep;
        for (i, pending) in pending_steps {
            for bits in pending {
                for bit in bits {
                    w.write(bit as u32, 1);
                }
            }
            if dif_after.contains(&i) && i + 1 < keep {
                self.last_dif_pos.push(w.len_bits());
                if std::env::var("FW_DEBUG").is_ok() {
                    eprintln!("fw: dif after step {i} at stream bit {}", w.len_bits());
                }
                write_alloc_dif_cfg(&mut self.coded, widths, shape.escapes, &mut w);
            }
            if std::env::var("FW_DEBUG").is_ok() {
                eprintln!("fw: step {i} end {}", w.len_bits());
            }
        }
        let data = w.into_bytes();
        if std::env::var("FW_DEBUG").is_ok() {
            let hex: String = data.iter().take(96).map(|b| format!("{:02x}", b)).collect();
            eprintln!("fw: data0 {hex}");
        }

        // raw buffer: [00][type|pad][length][bitstream]
        let mut raw = vec![0u8; shape.bytes];
        raw[0] = 0x00;
        raw[1] = shape.frame_type | if pad { 0x80 } else { 0 };
        raw[2] = shape.length_byte();
        let n = data.len().min(stream_bytes);
        raw[3..3 + n].copy_from_slice(&data[..n]);

        // Padding: the container flag marks a padded packet and the tail
        // carries its length — a 0x80 marker, then zero fill, then the count
        // (mod 255) followed by that many 0xFF bytes.  A decoder that sees
        // the flag stops at the marker instead of parsing the zero fill as
        // more composer pairs.  The same scheme serves the LL payload.
        let pad_len = stream_bytes - n;
        if pad_len > 0 {
            raw[1] |= 0x80;
            let pad = &mut raw[3 + n..];
            pad.fill(0);
            pad[0] = 0x80;
            let t = pad.len() - 1;
            let ff = t / 255;
            let at = pad.len() - 1 - ff;
            pad[at + 1..].fill(0xFF);
            pad[at] = (t % 255) as u8;
        }

        // OTA presentation: pairwise byte swap
        let mut ota = vec![0u8; shape.bytes];
        let mut i = 0usize;
        while i + 1 < shape.bytes {
            ota[i] = raw[i + 1];
            ota[i + 1] = raw[i];
            i += 2;
        }
        self.frames += 1;
        ota
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitalloc_seq::read_alloc_seq16;
    use crate::composer::decode_pair;
    use crate::entropy::BitReader;

    #[test]
    fn dif_is_sixteen_ones_after_chunk() {
        let widths: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0, 16];
        let mut desc = [0u8; 16];
        desc[7] = 16;
        desc[15] = 16;
        let mut s = [[0i32; 2]; 16];
        s[7] = [0x1234, 0x2345];
        s[15] = [0x1234, 0x2345];
        let steps = vec![s, s, s, s];
        let mut fw = FrameWriter::default();
        let ota = fw.write(&widths, &desc, &steps, &[1], false);
        let mut raw = vec![0u8; 656];
        let mut i = 0;
        while i + 1 < 656 {
            raw[i] = ota[i + 1];
            raw[i + 1] = ota[i];
            i += 2;
        }
        let mut rd = BitReader::new(&raw);
        for _ in 0..24 {
            rd.read(1).unwrap();
        }
        assert_eq!(rd.read(1).unwrap(), 1, "overlap");
        assert_eq!(rd.read(4).unwrap(), 0xa, "nibble");
        let w16 = read_alloc_seq16(&mut rd).expect("alloc");
        assert_eq!(w16[7], 16);
        assert_eq!(w16[15], 16);
        for _ in 0..2 {
            decode_pair(16, &mut rd).expect("pair a");
            decode_pair(16, &mut rd).expect("pair b");
        }
        let pos = rd.pos_bits();
        let mut ones = true;
        for _ in 0..16 {
            if rd.read(1).unwrap() != 1 {
                ones = false;
            }
        }
        assert!(ones, "dif after step 1 not ones at bit {pos}");
    }

    #[test]
    fn pair_round_trip() {
        let vals: Vec<(u32, u32)> = vec![
            (0x1234, 0x2345),
            (0xffff, 0xffff),
            (0x0000, 0x0000),
            (0x7fff, 0x8000),
            (0x0a00, 0x0b11),
            (0x1fff, 0x0fff),
            (0x8000, 0x0001),
            (0xfff0, 0x000f),
        ];
        let mut bw = BitWriter::new();
        let mut lens = Vec::new();
        for &(v0, v1) in &vals {
            let bits = pair_bits(16, v0, v1);
            for b in &bits {
                bw.write(*b as u32, 1);
            }
            lens.push(bits.len());
        }
        let bytes = bw.into_bytes();
        let mut rd = BitReader::new(&bytes);
        for (k, &(v0, v1)) in vals.iter().enumerate() {
            let start = rd.pos_bits();
            let (d0, d1) = decode_pair(16, &mut rd).expect("decode");
            assert_eq!(
                rd.pos_bits() - start,
                lens[k],
                "pair {k} ({v0:#06x},{v1:#06x}): wrote {} consumed {}",
                lens[k],
                rd.pos_bits() - start
            );
            assert_eq!((d0, d1), (v0, v1), "pair {k} value round trip");
        }
    }

    #[test]
    fn pair_at_bit_offset() {
        // prefill to the exact stream position of the failing step (635)
        let mut bw = BitWriter::new();
        let mut lcg: u32 = 12345;
        for _ in 0..635 {
            lcg = lcg.wrapping_mul(1664525).wrapping_add(1013904223);
            bw.write((lcg >> 16) & 1, 1);
        }
        let b1 = pair_bits(16, 0x8000, 0x8000);
        assert_eq!(b1.len(), 43, "pair length for the saturated value");
        for b in &b1 {
            bw.write(*b as u32, 1);
        }
        for b in &b1 {
            bw.write(*b as u32, 1);
        }
        assert_eq!(bw.len_bits(), 635 + 86);
        let bytes = bw.into_bytes();
        eprintln!(
            "bytes[78..91] = {}",
            bytes[78..91]
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>()
        );
        // the pair code for (0x8000,0x8000) is T256[0x88] = (16379,19):
        // bits 635..654 must be 0000011111111111011
        let mut expect = String::new();
        for i in 0..86 {
            let b = bytes[(635 + i) / 8];
            expect.push(char::from(b'0' + ((b >> (7 - ((635 + i) % 8))) & 1)));
        }
        eprintln!("bits[635..721] = {}", expect);
        let mut rd = BitReader::new(&bytes);
        for _ in 0..635 {
            rd.read(1).unwrap();
        }
        let p0 = rd.pos_bits();
        let (v0, v1) = decode_pair(16, &mut rd).expect("pair 0");
        assert_eq!((v0, v1), (0x8000, 0x8000), "pair 0 value");
        assert_eq!(rd.pos_bits() - p0, 43, "pair 0 length");
        let p1 = rd.pos_bits();
        let (v0, v1) = decode_pair(16, &mut rd).expect("pair 1");
        assert_eq!((v0, v1), (0x8000, 0x8000), "pair 1 value");
        assert_eq!(rd.pos_bits() - p1, 43, "pair 1 length");
    }

    #[test]
    fn bitwriter_matches_reference() {
        // byte boundary sanity
        let mut bw = BitWriter::new();
        bw.write(0b1011_0011, 8);
        assert_eq!(bw.bytes(), &[0xb3]);
        bw.write(0b1010, 4);
        assert_eq!(bw.bytes(), &[0xb3, 0xa0]);

        // 635-bit prefill + the saturated pair, against the reference bytes
        // (independently rendered from the same bit sequence)
        let mut bw = BitWriter::new();
        let mut lcg: u32 = 12345;
        for _ in 0..635 {
            lcg = lcg.wrapping_mul(1664525).wrapping_add(1013904223);
            bw.write((lcg >> 16) & 1, 1);
        }
        let b1 = pair_bits(16, 0x8000, 0x8000);
        for _ in 0..2 {
            for b in &b1 {
                bw.write(*b as u32, 1);
            }
        }
        assert_eq!(bw.len_bits(), 721);
        let got: Vec<u8> = bw.bytes().to_vec();
        let expect: Vec<u8> = (0..13)
            .map(|i| {
                u8::from_str_radix(&"0d60ffec0000001ffd80000000"[i * 2..i * 2 + 2], 16).unwrap()
            })
            .collect();
        assert_eq!(&got[78..91], &expect[..], "635-bit prefill bytes");
    }

    fn bits_of(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 8);
        for b in bytes {
            s.push_str(&format!("{:08b}", b));
        }
        s
    }

    /// The vendor F1 head: overlap '1' + nibble '1010' at raw bits 24..29,
    /// allocation at bits 29..77.  For the F1 widths the writer must
    /// reproduce those exact 48 bits.
    #[test]
    fn f1_head_bits() {
        let widths: [u8; 16] = [6, 4, 3, 2, 2, 0, 0, 0, 6, 4, 3, 2, 2, 0, 0, 0];
        let mut w = BitWriter::new();
        w.write(1, 1);
        w.write(0xa, 4);
        write_alloc16(&widths, 0, &mut w);
        assert_eq!(w.len_bits(), 53, "5 head bits + 48 alloc bits");
        let bytes = w.into_bytes();
        let got = bits_of(&bytes);
        match std::fs::read("/tmp/opencode/rigpkt.bin.raw") {
            Ok(raw) => {
                let f1 = &raw[0..656];
                let fbits = bits_of(f1);
                assert_eq!(&got[..5], &fbits[24..29], "overlap + nibble");
                assert_eq!(&got[5..53], &fbits[29..77], "alloc bits");
            }
            Err(_) => {
                // no vendor frame available: just the known-good head bytes
                assert_eq!(&bytes[..3], &[0xd4, 0x87, 0xf6][..]);
            }
        }
    }
}

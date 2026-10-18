//! The vendor's dither generators, ported from the decompiled R2 module
//! (`QCalculateDitherNUSeed` @0x234a0, `QGenerateDitherNU` @0x23524,
//! `QGetDitherNUValue` @0x234e8 — see `analysis/QUANT-SSLMS-DECOMPILED.md`).
//!
//! The dither is a **golden-ratio hash (`×0x9e3776`) over a rolling 4-bit
//! accumulator**: `acc = (value & 0xf) << 8 + acc*16`.  The accumulator is
//! never reset, so the sequence is continuous across blocks; the decoder
//! regenerates the identical sequence (`IQNUDecode_GenerateDither` is
//! instruction-equal to `QGenerateDitherNU`), which is why the decision
//! dither needs no side information in the bitstream.
//!
//! Two banks exist in the vendor code and both are modelled here:
//! * the per-(band, slot) **seed bank** (`bank`, stride 0x41 words) written
//!   by `calculate_seed` and read by [`Dither::value`];
//! * the per-band **scaled buffer** ([`Dither::generate`]) whose nslots
//!   words hold the same hash word shifted to each refinement scale.
//!
//! SPDX-License-Identifier: MIT

/// Words per band in the seed bank (`0x41` in the vendor object).
pub const BANK_STRIDE: usize = 0x41;

/// Golden-ratio hash constants, verbatim from the binary.
const GOLDEN: i64 = 0x9e_3776;

pub struct Dither {
    nb: usize,
    nslots: usize,
    /// per-band rolling accumulator (the vendor keeps this at ctx+0x14 per band)
    acc: Vec<i32>,
    /// per-(band, slot) seed bank; `bank[band*BANK_STRIDE + slot]`
    bank: Vec<i32>,
    /// per-band scaled buffer; `out[band*nslots .. band*nslots + nslots]`
    out: Vec<i32>,
}

impl Dither {
    /// `nb` bands, `nslots` ∈ {4, 8} (voice/music slot classes).
    pub fn new(nb: usize, nslots: usize) -> Self {
        Dither {
            nb,
            nslots,
            acc: vec![0; nb],
            bank: vec![0; nb * BANK_STRIDE],
            out: vec![0; nb * nslots],
        }
    }

    /// One hash step: returns `(w, v3, seed)` where `w` is the shifted
    /// 22-bit payload and `v3` the two low bits carried alongside it.
    fn step(&mut self, value: u32, band: usize) -> (u32, u32, i32) {
        let acc = (value & 0xf) as i32 * 0x100 + self.acc[band] * 0x10;
        self.acc[band] = acc;
        let h = ((acc >> 8) as i64).wrapping_mul(GOLDEN);
        let u = h as u32;
        let w = (((h as u64 >> 24) as u32).wrapping_add(u) & 0x3f_ffff) << 2;
        let v3 = (u >> 22) & 3;
        (w, v3, (w | v3) as i32)
    }

    /// `QCalculateDitherNUSeed`: advance the accumulator and store the seed
    /// into the bank for `(band, slot)`.
    pub fn calculate_seed(&mut self, value: u32, band: usize, slot: usize) {
        assert!(band < self.nb && slot < BANK_STRIDE);
        let (_w, _v3, seed) = self.step(value, band);
        self.bank[band * BANK_STRIDE + slot] = seed;
    }

    /// `QGenerateDitherNU`: advance the accumulator and fill the band's
    /// scaled buffer with the `nslots` derived copies of one hash word.
    pub fn generate(&mut self, value: u32, band: usize) -> &[i32] {
        assert!(band < self.nb);
        let (w, v3, seed) = self.step(value, band);
        let dst = &mut self.out[band * self.nslots..(band + 1) * self.nslots];
        match self.nslots {
            8 => {
                dst[0] = seed;
                dst[1] = ((v3 | (w & 0x3f_ffff)) << 2) as i32;
                dst[2] = ((v3 | (w & 0xf_ffff)) << 4) as i32;
                dst[3] = ((v3 | (w & 0x3_ffff)) << 6) as i32;
                dst[4] = seed.wrapping_shl(16) >> 8;
                dst[5] = seed.wrapping_shl(18) >> 8;
                dst[6] = seed.wrapping_shl(20) >> 8;
                dst[7] = seed.wrapping_shl(22) >> 8;
            }
            4 => {
                dst[0] = seed.wrapping_shl(8) >> 8;
                dst[1] = seed.wrapping_shl(0xd) >> 8;
                dst[2] = seed.wrapping_shl(0x12) >> 8;
                dst[3] = seed.wrapping_shl(0x17) >> 8;
            }
            _ => panic!("nslots must be 4 or 8"),
        }
        dst
    }

    /// `QGetDitherNUValue`: scale the banked seed by the level index.
    /// `res` is the quantiser resolution class (16 / 8 / 4).
    pub fn value(&self, res: u32, idx: u32, band: usize, slot: usize) -> i32 {
        let seed = self.bank[band * BANK_STRIDE + slot];
        if res != 8 {
            let sh = (idx * 2 + 8) as u32;
            return (seed.wrapping_shl(sh)) >> 8;
        }
        if res != 4 {
            let sh = (idx * 5 + 8) as u32;
            return seed.wrapping_shl(sh) >> 8;
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden vector computed from the decompiled op sequence
    /// (python emulation of `QCalculateDitherNUSeed`, 2026-10-16).
    #[test]
    fn golden_seeds_and_accumulator() {
        let mut d = Dither::new(1, 8);
        let cases = [
            (1u32, 256i32, 7921114i32),
            (2, 4608, 8362332),
            (3, 74496, 6565719),
            (0, 1191936, 4388214),
            (0xf, 19074816, 4478769),
            (0xa, 305199616, 16653724),
        ];
        for (v, want_acc, want_seed) in cases {
            let (_w, _v3, seed) = d.step(v, 0);
            assert_eq!(d.acc[0], want_acc, "acc after value {v:#x}");
            assert_eq!(seed, want_seed, "seed after value {v:#x}");
        }
    }

    #[test]
    fn scaled_buffer_golden() {
        let mut d = Dither::new(1, 8);
        // feed 1,2,3 → third seed 6565719, then check the scaled copies
        let mut last = 0i32;
        for v in [1u32, 2, 3] {
            last = d.generate(v, 0)[0];
        }
        assert_eq!(last, 6565719);
        let expect = [
            6565719, 9485660, 4388208, 775616, 3102464, -4367360, -692224, -2768896,
        ];
        assert_eq!(d.generate(0, 0).len(), 8);
        // regenerate from a fresh instance to compare against the vector above
        let mut d2 = Dither::new(1, 8);
        d2.generate(1, 0);
        d2.generate(2, 0);
        let buf = d2.generate(3, 0).to_vec();
        assert_eq!(buf, expect);
    }

    #[test]
    fn bank_written_by_calculate_seed_and_read_by_value() {
        let mut d = Dither::new(2, 8);
        d.calculate_seed(3, 1, 5);
        // first step from a zero accumulator: value 3 -> acc 768, seed 6986127
        assert_eq!(d.bank[1 * BANK_STRIDE + 5], 6986127);
        assert_eq!(d.value(16, 0, 1, 5), 6986127);
        assert_eq!(d.value(8, 0, 1, 5), 6986127);
        // branch structure: only res==8 selects the idx*5+8 scaling;
        // res 16 and 4 both take the idx*2+8 path (the res==4 fall-through
        // in the vendor code is unreachable)
        assert_eq!(d.value(4, 3, 1, 5), d.value(16, 3, 1, 5));
        assert_eq!(d.value(8, 2, 1, 5), 6986127i32.wrapping_shl(2 * 5 + 8) >> 8);
    }

    #[test]
    fn accumulator_continuity_across_blocks() {
        // feeding the same value twice must differ in seed the second time
        let mut d = Dither::new(1, 8);
        let a = d.generate(5, 0)[0];
        let b = d.generate(5, 0)[0];
        assert_ne!(a, b);
    }
}

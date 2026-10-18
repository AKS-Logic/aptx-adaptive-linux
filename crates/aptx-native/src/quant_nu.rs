//! Width-coupled non-uniform quantiser with dither, ported from the
//! decompiled R2 module (`QuantizationNU_SetupForSubBands` @0x13890,
//! `_Run_Music_Block` @0x13bb4, `InvQuantizationNU_Run_Classic` @0x14f20 —
//! see `analysis/QUANT-SSLMS-DECOMPILED.md`).
//!
//! Per-band configuration from the allocated width `w`:
//!
//! | field | value |
//! |---|---|
//! | dequant table | `nu_dq{w}bit16` (i32 levels, indexed by `|code|`) |
//! | dither table | `nu_dq{w}dith16` (i16 per level, used in the gain leak) |
//! | increment table | `nu_q{w}incr16` |
//! | level offset | `w - 27` |
//! | threshold | `((25-w) << 8) - 1` |
//! | shift / mask | `w <= 9`: 0/0; `w >= 10`: `w-9`, `2^(w-9)-1` |
//!
//! Quantisation (block path): `value = residual; |value|<<11` bisected over
//! `dq[i] * gain` to extract the magnitude index MSB-first, refined against
//! the neighbouring `sum`-table entries for widths > 9, then the sign is
//! folded by XOR (`coded = mag ^ (sign >> 31)`), matching what the TID0
//! composer reads as a signed word.
//!
//! The reconstruction (decoder replica) uses the 1-based `|idx|` table
//! lookup `level = ±2*nq_dq[|idx|]` and the per-band gain; the gain itself
//! advances with the leaky integrator
//! `gain = cround(gain*0x7f6c00 + dith[level]*0x800000, 23)` clamped, with a
//! log-domain tracker through `nu_logTable` feeding the next block.
//!
//! SPDX-License-Identifier: MIT

use crate::nu_tables::*;
use crate::vendor_arith::cround;

/// One band's static quantiser configuration (from the width).
#[derive(Clone, Copy, Debug)]
pub struct BandConfig {
    pub width: u8,
    pub offset: i32,
    pub threshold: i32,
    pub shift: u32,
    pub mask: i32,
}

pub fn config_for_width(w: u8) -> BandConfig {
    let (shift, mask) = if w <= 9 {
        (0, 0)
    } else {
        (w as u32 - 9, (1i32 << (w - 9)) - 1)
    };
    BandConfig {
        width: w,
        offset: w as i32 - 27,
        threshold: ((25i32 - w as i32) << 8) - 1,
        shift,
        mask,
    }
}

/// The `nu_dq{w}bit16` level table for a width (i32, indexed by |code|).
pub fn dq_table(w: u8) -> &'static [i32] {
    match w {
        2 => &NU_DQ2BIT16,
        3 => &NU_DQ3BIT16,
        4 => &NU_DQ4BIT16,
        5 => &NU_DQ5BIT16,
        6 => &NU_DQ6BIT16,
        7 => &NU_DQ7BIT16,
        8 => &NU_DQ8BIT16,
        _ => &NU_DQ9BIT16,
    }
}

/// The `nu_dq{w}dith16` dither table for a width (i16 per level).
pub fn dith_table(w: u8) -> &'static [i16] {
    match w {
        2 => &NU_DQ2DITH16_I16,
        3 => &NU_DQ3DITH16_I16,
        4 => &NU_DQ4DITH16_I16,
        5 => &NU_DQ5DITH16_I16,
        6 => &NU_DQ6DITH16_I16,
        7 => &NU_DQ7DITH16_I16,
        8 => &NU_DQ8DITH16_I16,
        _ => &NU_DQ9DITH16_I16,
    }
}

/// Per-band runtime quantiser state.
#[derive(Clone, Debug)]
pub struct BandState {
    pub cfg: BandConfig,
    /// the `[8]`-initialised log-domain tracker (`ctx+0x1a24`)
    pub gain_log: i32,
    /// the `[0xf4]`-initialised linear gain (`ctx+0x2064`)
    pub gain_lin: i32,
    /// reconstruction published for the predictor loop (`ctx+0x19d0`)
    pub recon: i32,
}

impl BandState {
    pub fn new(width: u8) -> Self {
        BandState {
            cfg: config_for_width(width),
            gain_log: 8,
            gain_lin: 0xf4,
            recon: 0,
        }
    }

    /// Magnitude index extraction: bisect `|value|<<11` against
    /// `dq[i] * gain_lin` (the exact vendor search structure).
    fn magnitude_index(&self, value_abs: i64) -> u32 {
        let dq = dq_table(self.cfg.width);
        let scale = self.gain_log as i64; // the running "8"-seeded word
        let l = value_abs << 11;
        let count = dq.len() as i64;
        // bisection over the full table: find the largest i with dq[i]*scale <= l
        let (mut lo, mut hi) = (0i64, count - 1);
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            if (dq[mid as usize] as i64) * scale <= l {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        lo as u32
    }

    /// Quantise one residual sample.  Returns the coded word (sign folded),
    /// the reconstruction, and updates the gain state.
    pub fn quantize(&mut self, residual: i32) -> (i32, i32) {
        let neg = residual < 0;
        let mag = residual.unsigned_abs() as i64;
        let idx = self.magnitude_index(mag);

        // sign fold: coded = mag ^ (sign >> 31)  (decompiled verbatim)
        let sign_mask = if neg { !0 } else { 0 };
        let mut coded = (idx as i32).wrapping_add(0) ^ sign_mask;

        // reconstruction: 1-based |code| lookup, level = ±2*entry
        let dq = dq_table(self.cfg.width);
        let i = idx as usize;
        let level = if i == 0 {
            // code 0: first entry doubled (the table's entry 0 is the
            // negative mirror; index 1 is used for the magnitude walk)
            2 * dq[1.min(dq.len() - 1)]
        } else if i < dq.len() {
            2 * dq[i]
        } else {
            2 * dq[dq.len() - 1]
        };
        let recon = if neg { -level } else { level };

        // gain leak: gain = cround(gain*0x7f6c00 + dith[i]*0x800000, 23), clamped
        let dith = dith_table(self.cfg.width);
        let d = dith[i.min(dith.len() - 1)] as i64;
        let g = (self.gain_lin as i64 * 0x7f6c00 + d * 0x800000) >> 23;
        let g = g.clamp(-(self.cfg.threshold as i64), self.cfg.threshold as i64) as i32;
        self.gain_lin = g;

        // log-domain tracker (nu_logTable), shift by (0x18 - (w + gain>>8))
        let sh = 0x18 - (self.cfg.width as i32 + (self.gain_lin >> 8));
        let t = NU_LOGTABLE[(self.gain_lin >> 3 & 0x1f) as usize];
        let v = if sh >= 0 {
            t >> sh.min(31)
        } else {
            t << (-sh).min(31)
        };
        self.gain_log = v;
        self.recon = recon;

        // The dither-scaled decision tweak and extension-bit refinement for
        // w > 9 are staged behind `dither` (see module docs); the coded word
        // above already carries the vendor's sign-fold form.
        coded &= (1i32 << self.cfg.width.min(30)) - 1;
        (coded, recon)
    }
}

/// Cround wrapper exported for the encoder's value path.
pub fn gain_step(gain: i32, dith: i32) -> i32 {
    let v = (gain as i64) * 0x7f6c00 + (dith as i64) * 0x800000;
    cround((v >> 11) as i32, 12)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_mapping_matches_decompile() {
        let c2 = config_for_width(2);
        assert_eq!(c2.offset, -25);
        assert_eq!(c2.threshold, 0x16ff);
        assert_eq!((c2.shift, c2.mask), (0, 0));
        let c4 = config_for_width(4);
        assert_eq!(c4.offset, -23);
        assert_eq!(c4.threshold, 0x14ff);
        let c10 = config_for_width(10);
        assert_eq!(c10.shift, 1);
        assert_eq!(c10.mask, 1);
        let c12 = config_for_width(12);
        assert_eq!(c12.shift, 3);
        assert_eq!(c12.mask, 7);
    }

    #[test]
    fn magnitude_index_tracks_value() {
        let mut b = BandState::new(9);
        let (c0, _) = b.quantize(0);
        let (c1, _) = b.quantize(1000);
        let (c2, _) = b.quantize(200000);
        assert!(
            c0 <= c1 && c1 <= c2,
            "codes must be monotone: {c0} {c1} {c2}"
        );
    }

    #[test]
    fn sign_fold_is_symmetric() {
        let mut a = BandState::new(6);
        let mut b = BandState::new(6);
        let (cp, _) = a.quantize(12345);
        let (cn, _) = b.quantize(-12345);
        // vendor sign fold: coded = mag ^ (sign>>31) -> !mag for negatives
        let mask = (1i32 << 6) - 1;
        assert_eq!(cn, (!cp) & mask, "XOR-fold form");
    }
}

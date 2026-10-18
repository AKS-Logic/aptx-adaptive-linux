//! Non-uniform quantiser (delta / log-delta coding).
//!
//! Evidence:
//!
//! * symbols `AxR3Quantizer_{Setup,DeltaUpdate,DeltaWarmup}`, `AxR3CalDelta`,
//!   `AxR3CalDeltaLog`, `getQTableVal`, sources
//!   `quantizationNU/axQuantTablesNU.c` + `axR3QuantizerCom.c`, and the
//!   extracted tables `nu_dq{2..9}bit16`, `nu_q{2..9}incr16`, `nu_logTable`
//!   (see [`crate::tables`]).
//!
//! Table structure [inferred from the extracted shapes + the decoder's
//! `FUN_00408110` unpacker, which returns a Huffman prefix plus raw
//! extension bits]:
//!
//! * `nu_dq[b]` has 2^(b-1)+1 entries — the magnitude reconstruction levels
//!   of a sign-magnitude code (b bits = 1 sign + b-1 magnitude).
//! * `nu_q[b]_incr` has the same count — the decision increments between
//!   consecutive levels (the encoder's search thresholds).
//! * `nu_logTable` maps log-domain steps to value increments.
//!
//! This module implements the quantiser over the exact extracted level sets:
//! decision thresholds are the midpoints between consecutive levels, and
//! magnitudes are coded sign-first.  The exact vendor search path
//! (`AxR3Quantizer_DeltaUpdate`) is an open item — see docs/CMFB.md's open
//! items note for the project-wide policy on such gaps.
//!
//! SPDX-License-Identifier: MIT

use crate::nu_tables::NU_LOGTABLE;
use crate::tables::{
    APTX_NU_DQ2, APTX_NU_DQ3, APTX_NU_DQ4, APTX_NU_DQ5, APTX_NU_DQ6, APTX_NU_DQ7, APTX_NU_DQ8,
    APTX_NU_DQ9,
};

/// The quantiser's adaptive-delta state — the object the shipped encoder
/// passes as `state` to `AxR3CalDelta` / `AxR3CalDeltaLog` /
/// `AxR3Quantizer_DeltaUpdate` (`libaptXAdaptiveEnc3.so` at 0x6d00 / 0x6c60
/// / 0x6fa0; full transcription in `analysis/QUANT-RE-NOTES.md`).
///
/// ```text
///   acc      state[0x0]  delta accumulator (non-negative)
///   tracked  state[0x8]  tracked value / shift scale (-1 = uninitialised)
///   ceil     state[0xc]  clamp ceiling
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeltaState {
    /// `state[0x0]` — the accumulated delta (clamped at zero).
    pub acc: i32,
    /// `state[0x8]` — the tracked scale/value (`-1` = uninitialised).
    pub tracked: i32,
    /// `state[0xc]` — clamp ceiling.
    pub ceil: i32,
}

impl DeltaState {
    /// `AxR3Quantizer_DeltaUpdate(self_tracked, other)`: fold the difference
    /// between `self_tracked` and `other.tracked` into `other.acc` and sync
    /// the tracked value.
    pub fn update(self_tracked: i32, other: &mut DeltaState) -> i32 {
        if other.tracked == -1 || other.tracked == self_tracked {
            return other.acc;
        }
        let diff = self_tracked - other.tracked;
        other.tracked = self_tracked;
        other.acc -= diff << 8;
        if other.acc < 0 {
            other.acc = 0;
        }
        other.acc
    }

    /// `AxR3CalDelta(self, v)`: class-scaled delta over the 32-entry
    /// `nu_logTable`, exactly as the DSP module computes it.
    ///
    /// ```text
    ///   v     = clamp(v, 0, ceil)
    ///   delta = nu_logTable[(v >> 3) & 31] shifted by (-3 - tracked - (v >> 8))
    /// ```
    pub fn cal_delta(&self, v: i32) -> i32 {
        let v = v.clamp(0, self.ceil.max(0)) as u32;
        let shift = (-3 - self.tracked) - ((v >> 8) as i32);
        let base = NU_LOGTABLE[((v >> 3) & 0x1f) as usize];
        if shift >= 0 {
            base >> shift
        } else {
            base.wrapping_shl((-shift) as u32)
        }
    }

    /// `AxR3Quantizer_DeltaWarmup(self, shift, gain)`: seed a band's delta
    /// from its block gain.  Returns `(out0, out1, out2)` — the initial
    /// delta, the scaled gain, and the delta copied to the second slot.
    pub fn delta_warmup(&self, shift: i32, gain: i32) -> (u32, u32, u32) {
        let a = gain.unsigned_abs();
        let sh = shift + 2;
        let v = if sh >= 0 { a >> sh } else { a << (-sh) };

        let mut k = 23i32;
        let mut t = v;
        if v > 0 {
            loop {
                t >>= 1;
                k -= 1;
                if t == 0 {
                    break;
                }
            }
        }

        let m = if k >= 0 {
            (v as i32).wrapping_shl(k as u32)
        } else {
            (v as i32) >> (-k)
        };
        let mut bucket = 0usize;
        while bucket < 31 && m > NU_LOGTABLE[bucket] {
            bucket += 1;
        }

        let sh2 = (-3 - self.tracked) - k;
        let out0 = if sh2 >= 0 {
            let combined = (sh2 << 8) | ((bucket as i32) << 3);
            if combined >= 0 {
                if combined > self.ceil {
                    self.ceil as u32
                } else {
                    combined as u32
                }
            } else {
                0
            }
        } else {
            0
        };
        (out0, v, out0)
    }

    /// `AxR3CalDeltaLog(self, v)`: log-domain delta — bucket `v` in a
    /// threshold ladder, then pack the shift and the bucket index.
    ///
    /// ```text
    ///   k = 22 - floor(log2(v))          (23 for v <= 0; from the halving loop)
    ///   m = v << k  (or v >> -k)
    ///   bucket = first ladder index with m <= ladder[i]
    ///   code   = ((tracked_scale_shift) << 8) | (bucket << 3), clamped to ceil
    /// ```
    pub fn cal_delta_log(&self, v: i32, ladder: &[i32]) -> i32 {
        let mut k = 23i32;
        let mut r3 = v;
        if v > 0 {
            while r3 != 0 {
                r3 >>= 1;
                k -= 1;
            }
        }
        let r4 = -k;
        let m = if k >= 0 { v << k } else { v >> r4 };
        let mut idx: i32 = -1;
        for &e in ladder.iter().take(33) {
            if m <= e {
                break;
            }
            idx += 1;
        }
        let bucket = idx + 1;
        let sh = -3 - self.tracked - k;
        if sh >= 0 {
            let combined = (sh << 8) | (bucket << 3);
            if combined > -1 {
                return combined.min(self.ceil.max(0));
            }
        }
        0
    }
}

/// `getQTableVal(v, state, out_idx)` from the R3 quantizer: look up the
/// reconstruction level at magnitude index `state[6] & (v ^ (v>>31))`,
/// interpolating towards the next level with the `state[5] & mag + 1`
/// fraction when the width shift `state[4]` is non-zero.  Note the DSP uses
/// one's complement for the magnitude of negatives (`~v == |v|-1`), not
/// `abs(v)`.  Writes the 1-based magnitude index to `out_idx` and returns the
/// signed level.
pub fn get_q_table_val(
    v: i32,
    dq: &[i32],
    shift: i32,
    frac_mask: u32,
    idx_mask: u32,
) -> (i32, u32) {
    let sign = v >> 31;
    let mag = (v ^ sign) as u32;
    let idx = (idx_mask & mag) as usize;
    let mut level = dq[idx + 1];
    if shift != 0 {
        let frac = (frac_mask & mag).wrapping_add(1);
        let base = dq[idx];
        let diff = level.wrapping_sub(base);
        let prod = (frac as i64) * (diff as i64);
        let inc = if shift > 0 {
            (prod >> shift) as i32
        } else {
            (prod << (-shift)) as i32
        };
        level = base.wrapping_add(inc);
    }
    let out_idx = idx as u32 + 1;
    (if v < 0 { level.wrapping_neg() } else { level }, out_idx)
}

/// The dequantiser reconstruction levels for a `bits`-bit code (2..=9).
///
/// The extracted `nu_dq{b}` tables hold `2^(b-1)+1` **pairs**: the signed
/// level followed by its log-domain increment (the log column is monotone —
/// see docs/CMFB.md for the table dump).  This returns the signed levels,
/// sorted ascending, with a 0 level included.
pub fn magnitude_levels(bits: u8) -> Vec<f64> {
    let t: &[i16] = match bits {
        2 => &APTX_NU_DQ2,
        3 => &APTX_NU_DQ3,
        4 => &APTX_NU_DQ4,
        5 => &APTX_NU_DQ5,
        6 => &APTX_NU_DQ6,
        7 => &APTX_NU_DQ7,
        8 => &APTX_NU_DQ8,
        9 => &APTX_NU_DQ9,
        _ => return vec![0.0],
    };
    let mut v: Vec<f64> = t.iter().step_by(2).map(|&x| x as f64).collect();
    v.push(0.0);
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v.dedup();
    v
}

/// A quantiser for one code width.
#[derive(Clone, Debug)]
pub struct Quantizer {
    levels: Vec<f64>,
    bits: u32,
}

impl Quantizer {
    /// Build the quantiser for a `bits`-bit code.
    pub fn new(bits: u8) -> Self {
        Quantizer {
            levels: magnitude_levels(bits),
            bits: bits as u32,
        }
    }

    /// Number of reconstruction levels available.
    pub fn classes(&self) -> u32 {
        self.levels.len() as u32
    }

    /// Reconstruction value for a code (clamped to the level set).
    pub fn dequantize(&self, code: u32) -> f64 {
        let i = (code as usize).min(self.levels.len() - 1);
        self.levels[i]
    }

    /// Quantise a value to `(code, reconstruction)`: `code` is the index of
    /// the nearest level, clamped to `2^bits - 1`.
    pub fn quantize(&self, x: f64) -> (u32, f64) {
        let mut best = 0usize;
        let mut bestd = f64::INFINITY;
        for (i, &l) in self.levels.iter().enumerate() {
            let d = (x - l).abs();
            if d < bestd {
                bestd = d;
                best = i;
            }
        }
        let max_code = (1u32 << self.bits) - 1;
        if best as u32 > max_code {
            best = max_code as usize;
        }
        (best as u32, self.levels[best])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_update_matches_the_transcription() {
        let mut st = DeltaState {
            acc: 1000,
            tracked: 10,
            ceil: 255,
        };
        // no change -> no update
        assert_eq!(DeltaState::update(10, &mut st), 1000);
        // a decrease of 2 adds 2<<8 to the accumulator
        assert_eq!(DeltaState::update(8, &mut st), 1000 + 512);
        assert_eq!(st.tracked, 8);
        // a large increase drops it to zero (clamped)
        assert_eq!(DeltaState::update(100, &mut st), 0);
        // uninitialised other is left alone
        let mut st2 = DeltaState {
            acc: 7,
            tracked: -1,
            ceil: 0,
        };
        assert_eq!(DeltaState::update(5, &mut st2), 7);
        assert_eq!(st2.tracked, -1);
    }

    #[test]
    fn cal_delta_matches_ax_r3_cal_delta() {
        let st = DeltaState {
            acc: 0,
            tracked: 0,
            ceil: 4095,
        };
        // v = 0: shift = -3, base = nu_logTable[0]
        assert_eq!(st.cal_delta(0), NU_LOGTABLE[0] << 3);
        // v = 2047: table index 31, shift = -3 - 7 = -10
        assert_eq!(st.cal_delta(2047), NU_LOGTABLE[31] << 10);
        // values above the ceiling are clamped
        assert_eq!(st.cal_delta(9000), st.cal_delta(4095));
        // a larger tracked value shifts right, reducing the delta
        let hi = DeltaState { tracked: 12, ..st };
        assert!(hi.cal_delta(100) <= st.cal_delta(100));
    }

    #[test]
    fn delta_warmup_seeds_from_gain() {
        let st = DeltaState {
            acc: 0,
            tracked: -20,
            ceil: 4095,
        };
        let (d, v, d2) = st.delta_warmup(0, 1000);
        assert_eq!(v, 250); // 1000 >> 2
        assert_eq!(d, d2);
        assert!(d > 0);
        // a large tracked value drives the shift negative -> zero
        let st2 = DeltaState {
            acc: 0,
            tracked: 40,
            ceil: 4095,
        };
        assert_eq!(st2.delta_warmup(0, 1000).0, 0);
    }

    #[test]
    fn cal_delta_log_buckets() {
        let st = DeltaState {
            acc: 0,
            tracked: 0,
            ceil: 255,
        };
        let c1 = st.cal_delta_log(1, &NU_LOGTABLE);
        let c2 = st.cal_delta_log(1 << 20, &NU_LOGTABLE);
        // larger inputs fall in later buckets (or clamp), never negative
        assert!(c1 >= 0 && c2 >= 0);
    }

    #[test]
    fn q_table_val_matches_decompiled_interpolation() {
        let dq = [0, 10, 20, 30, 40];
        // no shift: level = dq[idx+1], out = idx+1.
        // Negatives use the DSP's one's-complement magnitude (~v), so -2 has
        // magnitude 1.
        assert_eq!(get_q_table_val(2, &dq, 0, 0, 3), (30, 3));
        assert_eq!(get_q_table_val(-2, &dq, 0, 0, 3), (-20, 2));
        // shift 1, frac = (1 & |v|) + 1, halfway between dq[idx] and dq[idx+1]
        assert_eq!(get_q_table_val(3, &dq, 1, 1, 3), (40, 4));
        assert_eq!(get_q_table_val(2, &dq, 1, 1, 3), (25, 3));
    }

    #[test]
    fn levels_are_grounded_in_tables() {
        let q = Quantizer::new(4);
        // 4 bits: 9 signed levels from the dq4 pair column + 0
        assert!(q.classes() >= 5 && q.classes() <= 16);
        // the level set contains zero and spans roughly 16-bit range
        let lo = q.dequantize(0);
        let hi = q.dequantize(q.classes() - 1);
        assert!(lo < 0.0 && hi > 0.0);
        let has_zero = (0..q.classes()).any(|c| q.dequantize(c) == 0.0);
        assert!(has_zero, "level set must include 0");
        // codes fit their width
        assert!(q.classes() <= (1 << 4));
    }

    #[test]
    fn round_trip_within_the_max_gap() {
        for bits in 2..=9 {
            let q = Quantizer::new(bits);
            let mut max_gap = 0.0f64;
            let mut prev = q.dequantize(0);
            for i in 1..q.classes() {
                let l = q.dequantize(i);
                max_gap = max_gap.max(l - prev);
                prev = l;
            }
            // inside the codebook the nearest-level error is bounded by the
            // largest gap; beyond the ends the quantiser saturates
            let lo = q.dequantize(0);
            let hi = q.dequantize(q.classes() - 1);
            let mut x = lo;
            while x <= hi {
                let (code, _) = q.quantize(x);
                let back = q.dequantize(code);
                assert!(
                    (back - x).abs() <= max_gap,
                    "bits {bits} x {x} -> {back} (max gap {max_gap})"
                );
                x += (hi - lo) / 500.0;
            }
        }
    }
}

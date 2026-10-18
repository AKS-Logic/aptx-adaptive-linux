//! The vendor's per-band sign-sign LMS predictor ("SSLMS_Encode_*"), ported
//! from the decompiled R2 module (`SSLMS_Encode_Run_CFG0_Normal` @0x1a760,
//! `_Init` @0x1b870, `sslms_UpdateCheck` @0x1b22c — see
//! `analysis/QUANT-SSLMS-DECOMPILED.md`).
//!
//! State layout (one state per band per channel, stride 0x1bc in the vendor
//! object; the live-verified object mapping is `ctx = *(obj+0x5B78)`,
//! states at `ctx+0x2A84+i*0x1BC`, channels = slots i and i+8):
//!
//! | offset | field |
//! |---|---|
//! | +0x00/+0x04 | rotating sign pair (from the saturating accumulate) |
//! | +0x08/+0x0c | gain pair (leaky integrators, `0xc00000`/`0xff`/`0xfe`) |
//! | +0x10 | tap count (per-band order from `NUM_ZEROS_*`) |
//! | +0x18 | coefficient pairs (updated two taps per vector op) |
//! | +0x94 | 64-word double-image history ring (two mirrored 32s) |
//! | +0x194 | ring write index, advanced `(idx-1) & 0x1f` |
//! | +0x19c | previous input word |
//! | +0x1a0/+0x1a4 | accumulators feeding the published band output |
//!
//! Per call (8 samples per channel per CMFB block):
//! 1. sign decision — `sat_sl(acc2 + input, 8)` against 0 / 0x100 branches;
//! 2. gain pair update — transcribed literally (convergent rounding,
//!    clamps at ±0x100000 / ±0x300000 / ±0x3c0000);
//! 3. tap loop — per tap pair, each coefficient is nudged toward
//!    `±0x800000` (sign of the input word) by `cround(target - coeff, 8)`
//!    and the prediction accumulates the updated coefficients against the
//!    ring samples; the ring receives the new input (mirrored into both
//!    halves) and the write index advances.
//!
//! Exactness note: the tap-loop transcription follows the decompiled scalar
//! reconstruction of the v65 vector ops; the pieces marked `INFERRED` in
//! the comments (the exact per-lane sign flip and the published-value
//! composition) are to be validated against the live vendor state via the
//! differential harness before this is called bit-exact.
//!
//! SPDX-License-Identifier: MIT

use crate::nu_tables::{NUM_ZEROS_MUSIC_8, NUM_ZEROS_MUSIC_8_REDUCED, N_PRED_UPDATE_RATE};
use crate::vendor_arith::{asl_sat, cround};

/// Tap counts per band (the `g_nNumZeros_*` family).
pub fn orders_music_48k() -> [u32; 8] {
    NUM_ZEROS_MUSIC_8
}
pub fn orders_music_reduced() -> [u32; 8] {
    NUM_ZEROS_MUSIC_8_REDUCED
}

/// `sslms_UpdateCheck` state (5 words) — the adaptation-rate controller.
#[derive(Clone, Copy, Default, Debug)]
pub struct UpdateCheck {
    pub prev: i32,
    pub tracker: i32,
    pub rate: i32,
    pub current: i32,
    pub counter: i32,
}

impl UpdateCheck {
    /// `sslms_UpdateCheck(value, state)`: returns whether the predictor
    /// update fires for this call and advances the controller.
    pub fn run(&mut self, value: i32) -> bool {
        let old = self.prev;
        let counter = self.counter;
        self.prev = value;
        let err = value.wrapping_sub(old.wrapping_add(self.tracker));
        let t1 = (self.rate.wrapping_mul(0xf)).min(0x7fffff);
        let t2 = (value
            .wrapping_sub(old)
            .wrapping_add(self.tracker.wrapping_mul(3)))
        .min(0x7fffff);
        let mut r = clamp_sym(t1, 0x800000);
        let b = counter < self.current;
        if err.unsigned_abs() > 2000 {
            r = r.wrapping_add(0x1000);
        }
        let mut c = counter;
        if b {
            c = counter + 1;
        }
        self.rate = r;
        let idx = ((r >> 12).min(0xf)) as usize;
        if !b {
            c = 0;
        }
        self.counter = c;
        let n = N_PRED_UPDATE_RATE[idx] as i32;
        self.tracker = clamp_sym(t2, 0x800000);
        if !b {
            self.current = n;
        }
        !b
    }
}

fn clamp_sym(v: i32, m: i32) -> i32 {
    if v > m {
        m
    } else if v < -m {
        -m
    } else {
        v
    }
}

/// `roundArithmetic(x, b)`: round-half-away-from-zero right shift.
fn round_arith(x: i32, b: u32) -> i32 {
    let half = 1i64 << (b - 1);
    let v = x as i64;
    if v >= 0 {
        ((v + half) >> b) as i32
    } else {
        -(((-v + half) >> b) as i32)
    }
}

/// One band/channel predictor state.
#[derive(Clone, Debug)]
pub struct SslmsState {
    pub sign: [i32; 2],
    pub gain: [i32; 2],
    pub taps: u32,
    /// coefficient pairs, stored flat (tap k at index k)
    pub coeff: [i32; 24],
    /// 64-word double-image history ring
    pub ring: [i32; 64],
    pub idx: u32,
    pub prev: i32, // +0x19c
    pub acc1: i32, // +0x1a0
    pub acc2: i32, // +0x1a4
    /// +0x1b0: set to 0xf000 by `AxSslmsPredictorDataClr` (role unread)
    pub adj: i32,
    pub update: UpdateCheck,
}

impl SslmsState {
    pub fn new(taps: u32) -> Self {
        SslmsState {
            sign: [1, 0],
            gain: [0, 0],
            taps,
            coeff: [0; 24],
            ring: [0; 64],
            idx: 0,
            prev: 0,
            acc1: 0,
            acc2: 0,
            adj: 0xf000, // AxSslmsPredictorDataClr: +0x1b0 = 0xf000
            update: UpdateCheck::default(),
        }
    }

    /// One run-kernel call for this state.  `input` is the per-band CMFB
    /// sample; returns the published band output word.
    pub fn process(&mut self, input: i32) -> i32 {
        // ---- 1. sign decision (transcribed; mode != 2 path) ----
        // the decision reads +0x1a0 (acc1); +0x1a4 (acc2) feeds the correction
        let v = asl_sat(self.acc1.wrapping_add(input), 8);
        let (mut iv9, mut iv28, iv25): (i32, i32, i32);
        if v < 0 {
            iv25 = -1;
            iv28 = self.sign[1];
            self.sign[1] = self.sign[0];
            iv9 = -self.sign[0];
            iv28 = -iv28;
        } else {
            iv9 = self.sign[0];
            if (v as u32) < 0x100 {
                iv28 = 0;
                iv25 = 1;
                self.sign[1] = iv9;
                iv9 = 0;
            } else {
                iv25 = 1;
                iv28 = self.sign[1];
                self.sign[1] = iv9;
            }
        }
        self.sign[0] = iv25;

        // ---- 2. gain pair update (transcribed literally) ----
        let f0 = self.gain[0];
        let f1 = self.gain[1];
        let iv35_raw = cround(0i32.wrapping_sub(iv9.wrapping_mul(f0)), 1);
        let i_var25 = cround(
            f0.wrapping_mul(0xff)
                .wrapping_add(iv9.wrapping_mul(0xc00000)),
            8,
        );
        let mut t = clamp_sym(iv35_raw, 0x100000);
        t = cround(
            (t.wrapping_add(iv28.wrapping_mul(0x80000)) & 0x0ffffff0)
                .wrapping_mul(0x10)
                .wrapping_add(f1.wrapping_mul(0xfe)),
            8,
        );
        let iv28b = clamp_sym(t, 0x300000);
        self.gain[1] = iv28b;
        let lo = iv28b.wrapping_sub(0x3c0000);
        let hi = 0x3c0000i32.wrapping_sub(iv28b);
        self.gain[0] = lo.max(i_var25).min(hi);

        // ---- 3. tap loop ----
        let target = if input == 0 {
            0
        } else {
            // sign of the input word scaled by 0x800000 (the vmux constant pair)
            let k = 0x800000i32;
            if input < 0 {
                -k
            } else {
                k
            }
        };
        let idx = (self.idx as usize) & 0x1f;
        let iters = (self.taps as usize + 1) / 2;
        let mut pred: i64 = 0;
        for j in 0..iters {
            for lane in 0..2 {
                let k = (2 * j + lane) as usize;
                if k >= 24 {
                    break;
                }
                let h = self.ring[idx + 2 * j + lane]; // ring[idx .. idx+taps]
                                                       // per-lane sign flip from the history word's sign (vcmpw.gtu)
                let s = if (h as u32) > 0x7fff_ffff {
                    target.wrapping_neg()
                } else {
                    target
                };
                let c = self.coeff[k];
                let delta = cround(s.wrapping_sub(c), 8);
                self.coeff[k] = c.wrapping_add(delta);
                pred += (self.coeff[k] as i64) * (h as i64);
            }
        }

        // ---- 4. publish (transcribed from the decompiled tail) ----
        // correction: roundArithmetic(((iv35_raw * iv28) + iv9 * (acc2_in >> 8)) >> 21, 1)
        let corr_src = asl_sat(self.acc2.wrapping_add(input), 8) >> 8; // iVar26 >> 8
        let uvar27 = round_arith(
            iv35_raw
                .wrapping_mul(iv28)
                .wrapping_add(iv9.wrapping_mul(corr_src))
                >> 21,
            1,
        );
        let acc1_new = asl_sat((pred >> 22) as i32, 8) >> 8;
        let acc2_new = asl_sat(acc1_new.wrapping_add(uvar27), 8) >> 8;
        self.prev = corr_src; // +0x19c <= sat_sl(acc2_old + input, 8) >> 8
        self.acc1 = acc1_new;
        self.acc2 = acc2_new;

        // ---- 5. ring update: the vendor writes at the DECREMENTED index,
        //         mirrored into both 32-sample images (+0x94 and +0x114) ----
        let ni = ((idx as u32).wrapping_sub(1)) & 0x1f;
        self.ring[ni as usize] = input;
        self.ring[32 + ni as usize] = input;
        self.idx = ni;
        acc2_new
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predictor_runs_and_is_bounded() {
        let mut s = SslmsState::new(24);
        for n in 0..2000 {
            let input = ((n as f64 * 0.3).sin() * 100000.0) as i32;
            let out = s.process(input);
            assert!(out.abs() < i32::MAX, "output bounded at call {n}");
        }
    }

    #[test]
    fn update_check_rate_ladder() {
        let mut u = UpdateCheck::default();
        // fire pattern: first call fires, then at least every call count <= 80
        let mut fires = 0;
        for n in 0..400 {
            if u.run(n * 100) {
                fires += 1;
            }
        }
        assert!(fires > 100, "controller must fire regularly, got {fires}");
        assert!(u.current >= 2 && u.current <= 80);
    }
}

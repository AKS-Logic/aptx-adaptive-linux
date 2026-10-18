//! CMFB analysis/synthesis filter bank — aptX Adaptive (8 subbands).
//!
//! Reconstructed from two independent sources of evidence:
//!
//! 1. The shipped Hexagon encoder modules (unstripped), which name the
//!    subsystem exactly:
//!    `CmfbPRPDAnalysis_Create` / `CmfbPRPDAnalysisFilter8Bands`,
//!    `CmfbPRPD_RunDCT2_N8`, `CMFB_PRPD_Proto_Sym_{256,128}Tap_50PcOL`,
//!    `CMFB_PRPD_CosineModulationMatrix_Sym_128_192_256Tap`, source files
//!    `aptx.adaptive.3.0/src/cmfb/axCmfbPRPD{,Analysis}.c`.
//!
//! 2. Qualcomm's own reference decoder (`test-decoder.exe`, aptX Adaptive
//!    2.1, decompiled with Ghidra):
//!
//!    * `FUN_0040cf70` — bank configuration (prototype lengths 256/192/144/128/96)
//!    * `FUN_0040d5c0` — polyphase driver: delay line + windowed fold
//!    * `FUN_0040d220` — band demodulation (8 subband → 16 modulated values)
//!    * `FUN_00411050` — the DCT-8 butterfly inside the demodulator
//!
//! PRPD = "Pseudo-Random Polyphase Decomposition": a 50 %-overlap cosine
//! modulated filter bank.  Per synthesis step the decoder:
//!
//! 1. demodulates 8 subband samples into 16 modulated values `v = M·x`,
//! 2. stores them at the head of a `2·ntaps`-deep delay ring,
//! 3. produces 8 PCM samples by the windowed polyphase fold
//!
//! ```text
//! y[j] = Σ_u p[u·16 + 15 - j] · V(2u+1)[j]
//!      + Σ_u p[u·16 +  7 - j] · V(2u)[8 + j]      u = 0..ntaps/16-1
//! ```
//!
//! where `V(b)` is the modulated block `b` steps behind the head.  The
//! fold was transcribed from the reference decoder's x86 assembly (not the
//! decompiler's C — the addressing is unambiguous there).
//!
//! The encoder's analysis is the PR dual of this synthesis (the bank is
//! biorthogonal — see [`crate::cmfb_dual`]):
//!
//! ```text
//! A = C ∘ Sᵀ      (Sᵀ = exact transpose, C = fitted equaliser)
//! ```
//!
//! Fixed point: PCM in/out is Q0 with 24-bit significant range (the
//! decoder saturates to s24); prototype Q30; modulation matrix Q31;
//! accumulators are i64 with round-half-up shifts.
//!
//! SPDX-License-Identifier: MIT

use crate::cmfb_dual::{
    DUAL_128, DUAL_128_DELAY, DUAL_128_LAG0, DUAL_128_NLAG, DUAL_128_PEAK, DUAL_256,
    DUAL_256_DELAY, DUAL_256_LAG0, DUAL_256_NLAG, DUAL_256_PEAK,
};
use crate::tables::{
    APTX_CMFB_MODMATRIX_128_192_256, APTX_CMFB_PROTO_128TAP, APTX_CMFB_PROTO_256TAP,
};
use crate::{BANDS, MODCH};

/// Ring size for the dual equaliser (support span is `C_SUPPORT_MAX - C_SUPPORT_MIN + 1 = 23`).
const DUAL_RING: usize = 32;
/// Emission delay of the analysis, in blocks, for the 256-tap window —
/// see the per-window `*_DELAY` constants in [`crate::cmfb_dual`].
pub const ANALYZE_DELAY_BLOCKS: usize = DUAL_256_DELAY;

/// Saturating narrow to signed 24-bit — the decoder's `FUN_0040e650`.
#[inline]
fn sat24(x: i64) -> i32 {
    x.clamp(-0x80_0000, 0x7f_ffff) as i32
}

/// Round-half-up right shift of a 64-bit accumulator.
#[inline]
fn round_shift(x: i64, sh: u32) -> i32 {
    ((x + (1i64 << (sh - 1))) >> sh) as i32
}

/// Round-half-up right shift of a 128-bit accumulator, saturated to i64.
#[inline]
fn round_shift128(x: i128, sh: u32) -> i64 {
    let v = (x + (1i128 << (sh - 1))) >> sh;
    v.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// The CMFB filter bank state (one instance per channel).
pub struct Cmfb {
    proto: &'static [i32],
    ntaps: usize,
    nseg: usize,
    /// synthesis delay ring of demodulated values, head = newest block
    vring: Vec<i32>,
    vpos: usize,
    /// analysis ring of PCM samples, newest block at head
    sring: Vec<i32>,
    spos: usize,
    /// ring of transposed-analysis outputs feeding the dual equaliser
    cring: [[i32; BANDS]; DUAL_RING],
    blocks: usize,
}

impl Cmfb {
    /// Create a bank with the long (256-tap) or short (128-tap) prototype.
    pub fn new(long_window: bool) -> Self {
        let (proto, ntaps): (&'static [i32], usize) = if long_window {
            (&APTX_CMFB_PROTO_256TAP, 256)
        } else {
            (&APTX_CMFB_PROTO_128TAP, 128)
        };
        Cmfb {
            proto,
            ntaps,
            nseg: ntaps / MODCH,
            vring: vec![0; 2 * ntaps],
            vpos: 0,
            sring: vec![0; ntaps],
            spos: 0,
            cring: [[0; BANDS]; DUAL_RING],
            blocks: 0,
        }
    }

    /// Drop all history (stream discontinuity).  Mirrors `axCompressorCMFBReset`.
    pub fn reset(&mut self) {
        self.vring.fill(0);
        self.sring.fill(0);
        self.cring = [[0; BANDS]; DUAL_RING];
        self.vpos = 0;
        self.spos = 0;
        self.blocks = 0;
    }

    /// Number of taps of the active prototype.
    pub fn ntaps(&self) -> usize {
        self.ntaps
    }

    /// Band demodulation: `v = M · x` (Q31 matrix, band-major 8×16).
    pub fn demod(&self, x: &[i32; BANDS]) -> [i32; MODCH] {
        let mm = APTX_CMFB_MODMATRIX_128_192_256;
        let mut v = [0i32; MODCH];
        for (i, vi) in v.iter_mut().enumerate() {
            let mut acc = 0i64;
            for (k, xk) in x.iter().enumerate() {
                acc += mm[k * MODCH + i] as i64 * *xk as i64;
            }
            *vi = round_shift(acc, 31);
        }
        v
    }

    /// Modulation transpose: `x = Mᵀ · w`.
    fn modtranspose(&self, w: &[i32; MODCH]) -> [i32; BANDS] {
        let mm = APTX_CMFB_MODMATRIX_128_192_256;
        let mut x = [0i32; BANDS];
        for (k, xk) in x.iter_mut().enumerate() {
            let mut acc = 0i64;
            for (i, wi) in w.iter().enumerate() {
                acc += mm[k * MODCH + i] as i64 * *wi as i64;
            }
            *xk = round_shift(acc, 31);
        }
        x
    }

    /// DECODER MIRROR: synthesise one step — 8 subband samples in, 8 PCM samples out.
    pub fn synth(&mut self, x: &[i32; BANDS]) -> [i32; BANDS] {
        let ntaps = self.ntaps;
        let nseg = self.nseg;
        let ring = 2 * ntaps;
        let p = self.proto;

        // 1. demodulate; the decoder moves the cursor back 16 then writes there
        let v = self.demod(x);
        let head = (self.vpos + ring - MODCH) % ring;
        for (i, vi) in v.iter().enumerate() {
            self.vring[(head + i) % ring] = *vi;
        }
        self.vpos = head;

        // 2. windowed polyphase fold
        let mut out = [0i32; BANDS];
        for (j, o) in out.iter_mut().enumerate() {
            // i128: with hot (near full-scale) tonal inputs the 32 windowed
            // terms add coherently and can exceed i64
            let mut acc = 0i128;
            for u in 0..nseg {
                let i0 = (head + 2 * u * MODCH + 8 + j) % ring;
                let i1 = (head + (2 * u + 1) * MODCH + j) % ring;
                acc += p[u * MODCH + 7 - j] as i128 * self.vring[i0] as i128;
                acc += p[u * MODCH + 15 - j] as i128 * self.vring[i1] as i128;
            }
            // FUN_0040e650 path: sat24(round((acc >> 12) >> 24))
            *o = sat24(round_shift128(acc, 36));
        }
        out
    }

    /// Exact transpose analysis `Sᵀ` (no dual): one block of raw subband estimates.
    ///
    /// ```text
    /// w[i] = Σ_u p[u·16 + 15 - i] · s[8m + 16u + ((i + 8) & 15)]
    /// x̂    = Mᵀ · w
    /// ```
    pub fn analyze_transpose(&mut self, xin: &[i32; BANDS]) -> [i32; BANDS] {
        let ntaps = self.ntaps;
        let nseg = self.nseg;
        let p = self.proto;

        let base = (self.spos + ntaps - BANDS) % ntaps;
        for (i, xi) in xin.iter().enumerate() {
            self.sring[(base + i) % ntaps] = *xi;
        }
        self.spos = base;

        let mut w = [0i32; MODCH];
        for (i, wi) in w.iter_mut().enumerate() {
            let mut acc = 0i128;
            for u in 0..nseg {
                let t = (base + u * MODCH + ((i + 8) & 15)) % ntaps;
                acc += p[u * MODCH + 15 - i] as i128 * self.sring[t] as i128;
            }
            *wi = round_shift128(acc, 30) as i32;
        }
        self.modtranspose(&w)
    }

    /// ENCODER: analyse one step — 8 PCM samples in, 8 subband samples out.
    ///
    /// Returns the subband block for `ANALYZE_DELAY_BLOCKS` blocks ago (the
    /// first calls return silence); overall latency is 256 samples (long
    /// window) plus the dual's group delay.
    pub fn analyze(&mut self, xin: &[i32; BANDS]) -> [i32; BANDS] {
        let t = self.analyze_transpose(xin);
        let b = self.blocks;
        self.cring[b % DUAL_RING] = t;

        let mut out = [0i32; BANDS];
        let (c_taps, lag0, nlag, peak, delay) = if self.ntaps == 256 {
            (
                &DUAL_256,
                DUAL_256_LAG0,
                DUAL_256_NLAG,
                DUAL_256_PEAK,
                DUAL_256_DELAY,
            )
        } else {
            (
                &DUAL_128,
                DUAL_128_LAG0,
                DUAL_128_NLAG,
                DUAL_128_PEAK,
                DUAL_128_DELAY,
            )
        };
        if b >= delay {
            // dual equaliser: out = C · t_out over block lags lag0..lag0+nlag-1
            let ob = b - delay;
            let mut acc = [0f64; BANDS];
            for l in 0..nlag {
                let j = ob as isize + peak as isize - (lag0 + l) as isize;
                if j < 0 {
                    continue;
                }
                let tap = &self.cring[(j as usize) % DUAL_RING];
                for k in 0..BANDS {
                    let tk = tap[k] as f64;
                    if tk == 0.0 {
                        continue;
                    }
                    let ck = &c_taps[k];
                    for kk in 0..BANDS {
                        acc[kk] += ck[kk][l] * tk;
                    }
                }
            }
            for (kk, o) in out.iter_mut().enumerate() {
                // the decoder's synthesis divides by 2^36 (proto Q30 + 6),
                // so the analysis emits subbands at 2^6 of the dual's natural
                // scale to make S·A unity through the real fold
                *o = (acc[kk] * 64.0).round() as i32;
            }
        }
        self.blocks += 1;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_block(b: usize, freq: f64) -> [i32; BANDS] {
        let mut x = [0i32; BANDS];
        for (i, xi) in x.iter_mut().enumerate() {
            let ph = 2.0 * std::f64::consts::PI * freq * ((b * BANDS + i) as f64) / 48000.0;
            *xi = (0.25 * 8388607.0 * ph.sin()) as i32;
        }
        x
    }

    #[test]
    fn band_localisation() {
        // the exact transpose S^T channelises naturally: a tone lands in its
        // nominal band (440 Hz -> 0, 7 kHz -> 2, 19 kHz -> 6, 22.5 kHz -> 7)
        for long_window in [true, false] {
            for (freq, expect) in [(440.0, 0usize), (7000.0, 2), (19000.0, 6), (22500.0, 7)] {
                let mut st = Cmfb::new(long_window);
                let mut e = [0f64; BANDS];
                for b in 0..400 {
                    let t = st.analyze_transpose(&tone_block(b, freq));
                    for (k, tk) in t.iter().enumerate() {
                        e[k] += (*tk as f64) * (*tk as f64);
                    }
                }
                let peak = e
                    .iter()
                    .enumerate()
                    .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                    .unwrap()
                    .0;
                assert_eq!(peak, expect, "freq {freq} landed in band {peak}");
                assert!(
                    e[peak] > 0.5 * e.iter().sum::<f64>(),
                    "frequency energy not concentrated"
                );
            }
        }
    }

    #[test]
    fn dual_energy_concentration() {
        // the PR dual reshapes the subband representation (it is the exact
        // inverse frame operator of the decoder's synthesis, so its band
        // pattern is what the shipped decoder needs); a tone's energy must
        // still stay concentrated in a couple of bands
        let mut st = Cmfb::new(true);
        let mut e = [0f64; BANDS];
        for b in 0..400 {
            let o = st.analyze(&tone_block(b, 7000.0));
            for (k, ok) in o.iter().enumerate() {
                e[k] += (*ok as f64) * (*ok as f64);
            }
        }
        let mut sorted = e;
        sorted.sort_by(|a, b| b.partial_cmp(a).unwrap());
        let tot: f64 = sorted.iter().sum();
        assert!(
            (sorted[0] + sorted[1]) / tot > 0.8,
            "dual energy spread too wide: {e:?}"
        );
    }

    #[test]
    fn perfect_reconstruction() {
        let mut enc = Cmfb::new(true);
        let mut dec = Cmfb::new(true);
        let nblk = 600usize;
        // deterministic noise, 24-bit amplitude
        let mut seed = 0x1234_5678u32;
        let mut x = Vec::with_capacity(nblk * BANDS);
        for _ in 0..nblk * BANDS {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            x.push(((seed % 16_777_216) as i32 - 8_388_608) as i32);
        }
        let mut sub = Vec::with_capacity(nblk * BANDS);
        for b in 0..nblk {
            let mut blk = [0i32; BANDS];
            blk.copy_from_slice(&x[b * BANDS..b * BANDS + BANDS]);
            let o = enc.analyze(&blk);
            sub.extend_from_slice(&o);
        }
        let mut y = Vec::with_capacity(nblk * BANDS);
        for b in 0..nblk {
            let mut blk = [0i32; BANDS];
            blk.copy_from_slice(&sub[b * BANDS..b * BANDS + BANDS]);
            let o = dec.synth(&blk);
            y.extend_from_slice(&o);
        }

        // find the reconstruction lag by maximum correlation
        let mut best = (0f64, 0usize, 0f64);
        for lag in 200..400usize {
            let mut num = 0f64;
            let mut den = 0f64;
            for i in 0..x.len() - lag {
                num += x[i] as f64 * y[i + lag] as f64;
                den += (x[i] as f64) * (x[i] as f64);
            }
            let g = num / den;
            let mut err = 0f64;
            for i in 0..x.len() - lag {
                let d = y[i + lag] as f64 - g * x[i] as f64;
                err += d * d;
            }
            let snr = 10.0 * (den / err).log10();
            if snr > best.0 {
                best = (snr, lag, g);
            }
        }
        let (snr, lag, gain) = best;
        assert!(snr > 60.0, "PR SNR only {snr:.1} dB at lag {lag}");
        assert!((gain - 1.0).abs() < 0.05, "gain {gain}");
    }
}

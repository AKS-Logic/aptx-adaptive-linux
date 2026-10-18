//! LPC / prediction stage.
//!
//! The aptX Adaptive encoder codes each subband block with one of several
//! prediction modes.  Evidence recovered from the shipped module:
//!
//! * symbols `AxLpcComputeAutocorrelation_32bFixPt`,
//!   `AxLpcComputeLpCoefficients_32bFixPt`, `axComputeLpCoefficientsFwd/Back`,
//!   `axBlock_R3_LPC`, `axBlock_R3_FIX`, `axBlock_TRUK`
//! * fixed predictor tables `g_FixedO1..g_FixedO4` (Q11), extracted:
//!   ```text
//!   o1: [1.0]
//!   o2: [2.0, -1.0]              (the classic (2, -1) fixed predictor)
//!   o3: (see tables/frametype.h)
//!   o4: …
//!   ```
//!
//! The autocorrelation + Levinson-Durbin routine is the same computation
//! FFmpeg's `lpc.c` implements (`AxLpcComputeAutocorrelation_32bFixPt` is
//! literally that shape); this Rust port follows the same algorithm so the
//! C reference can keep using the vendored FFmpeg primitive while both stay
//! numerically equivalent.
//!
//! SPDX-License-Identifier: MIT

use crate::tables::{APTX_FIXED_O1, APTX_FIXED_O2, APTX_FIXED_O3, APTX_FIXED_O4};

/// Maximum predictor order supported by the fixed tables.
pub const MAX_FIXED_ORDER: usize = 4;

/// The extracted fixed predictor for `order` 1..=4, in Q11 units
/// (2048 == 1.0), normalised to f64 here.
pub fn fixed_predictor(order: usize) -> &'static [i16] {
    match order {
        1 => &APTX_FIXED_O1,
        2 => &APTX_FIXED_O2,
        3 => &APTX_FIXED_O3,
        4 => &APTX_FIXED_O4,
        _ => &[],
    }
}

/// Apply a fixed predictor (Q11 coefficients) to `history`, most recent
/// sample first: `pred = Σ coef[i]·history[i] / 2048`.
pub fn predict_fixed(order: usize, history: &[i32]) -> i32 {
    let c = fixed_predictor(order);
    let n = c.len().min(history.len());
    if n == 0 {
        return 0;
    }
    let mut acc = 0i64;
    for i in 0..n {
        acc += c[i] as i64 * history[i] as i64;
    }
    ((acc + 1024) >> 11) as i32
}

/// Autocorrelation of `x` up to `order` lags (f64 accumulation — the
/// fixed-point variant `AxLpcComputeAutocorrelation_32bFixPt` is the same
/// computation in Q-format).
pub fn autocorrelation(x: &[i32], order: usize) -> Vec<f64> {
    let mut r = vec![0.0; order + 1];
    for (lag, rl) in r.iter_mut().enumerate() {
        let mut acc = 0.0;
        for i in lag..x.len() {
            acc += x[i] as f64 * x[i - lag] as f64;
        }
        *rl = acc;
    }
    r
}

/// Levinson-Durbin recursion.  Returns the predictor coefficients
/// `p[0..order]` in the convention `x[n] ≈ Σ p[i]·x[n-1-i]`.
///
/// Mirrors the structure of FFmpeg's `ff_lpc_calc_coefs`/`compute_lpc_coefs`,
/// which the design reuses on the C side.
pub fn levinson_durbin(r: &[f64], order: usize) -> Vec<f64> {
    let mut a = vec![0.0; order + 1];
    let mut err = r[0];
    if err <= 0.0 {
        return vec![0.0; order];
    }
    for m in 1..=order {
        let mut acc = r[m];
        for j in 1..m {
            acc += a[j] * r[m - j];
        }
        let k = -acc / err;
        a[m] = k;
        for j in 1..=m / 2 {
            let tmp = a[j] + k * a[m - j];
            a[m - j] += k * a[j];
            a[j] = tmp;
        }
        err *= 1.0 - k * k;
        if err <= 1e-12 {
            break;
        }
    }
    // a[] solves x[n] + Σ a[i]·x[n-i] = e[n]; the predictor negates it.
    a[1..].iter().map(|v| -v).collect()
}

/// LPC analysis of one block: autocorrelation + Levinson-Durbin.
/// Returns the `order` predictor coefficients as `f64`.
pub fn lpc_analyze(x: &[i32], order: usize) -> Vec<f64> {
    let r = autocorrelation(x, order);
    levinson_durbin(&r, order)
}

/// Apply an LPC predictor to the history (most recent first).
pub fn predict_lpc(coefs: &[f64], history: &[i32]) -> i32 {
    let n = coefs.len().min(history.len());
    let mut acc = 0.0;
    for i in 0..n {
        acc += coefs[i] * history[i] as f64;
    }
    acc.round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_predictors_from_tables() {
        // order 2 is the classic (2, -1) predictor in Q11
        let c2 = fixed_predictor(2);
        assert_eq!(c2.len(), 4, "o2 table is 4 i16 (two Q11 coefficient pairs)");
        assert_eq!(c2[1], 4096); // 2.0
        assert_eq!(c2[3], -2048); // -1.0
                                  // a constant history predicts itself
        let hist = [1000i32, 1000, 1000, 1000];
        assert_eq!(predict_fixed(2, &hist), 1000);
    }

    #[test]
    fn lpc_recovers_a_ramp() {
        // x[n] = 100 + 2n: the exact recurrence (2, -1) is the LS optimum
        // only when the autocorrelation is two-sided; the FFmpeg-style
        // one-sided sums bias it slightly, so allow a small error
        let x: Vec<i32> = (0..64).map(|n| 100 + 2 * n).collect();
        let c = lpc_analyze(&x, 2);
        let hist = [x[63], x[62]];
        let p = predict_lpc(&c, &hist);
        assert!(
            (p - 228).abs() <= 8,
            "predicted {p}, expected ~228 (coefs {c:?})"
        );
    }
}

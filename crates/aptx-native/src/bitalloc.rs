//! Per-band bit allocation.
//!
//! Evidence:
//!
//! * The decoder (`FUN_00408170`) carries one allocation value per band
//!   (`state[0x8c + band]`) and derives the frame payload size from it with
//!   the exact mapping:
//!
//!   ```text
//!   value 0  ->  0 payload bits
//!   value 2  ->  6 payload bits
//!   value 3  ->  14 payload bits
//!   value v  ->  2v + 11 payload bits
//!   ```
//!
//!   with the special marker values `0xff` / `0xff00` meaning "repeat the
//!   previous frame's allocation".
//! * Encoder symbols: `AxBitAlloc_Run_MonoMusic{,_96k}`,
//!   `AxBitAlloc_OnPktSizeChg`, `axBitAllocSoftmaxMono` (the NN variant),
//!   tables `BitAllocOffsetTable*`, `nMaxBits*`, `nMinBits*` (extracted).
//!
//! The vendor allocator combines a loudness model with a softmax network;
//! this module implements an energy-proportional allocator that honours the
//! same per-band value space and payload cost mapping, so streams remain
//! structurally identical.  [inferred/simplified — documented gap]
//!
//! SPDX-License-Identifier: MIT

use crate::BANDS;

/// Payload bits consumed by one band with allocation value `v`
/// (the exact mapping recovered from `FUN_00408170`).
pub fn band_cost_bits(v: u8) -> usize {
    match v {
        0 => 0,
        2 => 6,
        3 => 14,
        v => 2 * v as usize + 11,
    }
}

/// Allowed allocation values, ascending (the decoder treats 1 as "3" for
/// pre-config-2 streams; we simply avoid emitting it).
pub const ALLOC_STEPS: [u8; 9] = [0, 2, 3, 4, 5, 6, 7, 8, 9];

/// Allocate per-band values so that the total payload cost stays within
/// `budget_bits`, proportional to band energies.
///
/// Returns one allocation value per band.
pub fn allocate(energies: &[f64; BANDS], budget_bits: usize) -> [u8; BANDS] {
    let mut alloc = [0u8; BANDS];
    let mut used = 0usize;

    // greedy: repeatedly give the next step to the band with the largest
    // energy per marginal cost
    loop {
        let mut best_band = None;
        let mut best_gain = 0.0f64;
        for b in 0..BANDS {
            let next = match ALLOC_STEPS.iter().position(|&s| s == alloc[b]) {
                Some(i) if i + 1 < ALLOC_STEPS.len() => ALLOC_STEPS[i + 1],
                _ => continue,
            };
            let marginal = band_cost_bits(next) - band_cost_bits(alloc[b]);
            if used + marginal > budget_bits {
                continue;
            }
            let gain = (energies[b] + 1.0) / marginal as f64;
            if gain > best_gain {
                best_gain = gain;
                best_band = Some((b, next, marginal));
            }
        }
        match best_band {
            Some((b, next, marginal)) => {
                alloc[b] = next;
                used += marginal;
            }
            None => break,
        }
    }
    alloc
}

/// Allocate allocation values (per-value bit widths, `ALLOC_STEPS`) so the
/// total payload cost `Σ alloc[b] · n_values_per_ch · n_channels` stays in
/// `budget_bits`, proportional to band energies.
///
/// This is the allocator the native encoder uses; [`allocate`] implements
/// the vendor's frame-size cost mapping for structural compatibility.
pub fn allocate_passband(
    energies: &[f64; BANDS],
    n_values_per_ch: usize,
    n_channels: usize,
    budget_bits: usize,
) -> [u8; BANDS] {
    let unit = n_values_per_ch * n_channels;
    let mut alloc = [0u8; BANDS];
    let mut used = 0usize;
    loop {
        let mut best: Option<(usize, u8, usize)> = None;
        let mut best_gain = 0.0f64;
        for b in 0..BANDS {
            let next = match ALLOC_STEPS.iter().position(|&s| s == alloc[b]) {
                Some(i) if i + 1 < ALLOC_STEPS.len() => ALLOC_STEPS[i + 1],
                _ => continue,
            };
            let marginal = unit * (next - alloc[b]) as usize;
            if used + marginal > budget_bits {
                continue;
            }
            let gain = (energies[b] + 1.0) / marginal as f64;
            if gain > best_gain {
                best_gain = gain;
                best = Some((b, next, marginal));
            }
        }
        match best {
            Some((b, next, marginal)) => {
                alloc[b] = next;
                used += marginal;
            }
            None => break,
        }
    }
    alloc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_mapping_matches_decoder() {
        assert_eq!(band_cost_bits(0), 0);
        assert_eq!(band_cost_bits(2), 6);
        assert_eq!(band_cost_bits(3), 14);
        assert_eq!(band_cost_bits(4), 19);
        assert_eq!(band_cost_bits(9), 29);
    }

    #[test]
    fn allocation_respects_budget_and_favours_energy() {
        let mut e = [1.0; BANDS];
        e[3] = 1e6;
        let budget = 8 * 656 * 8; // one full R2 stereo packet in bits
        let a = allocate(&e, budget);
        let total: usize = a.iter().map(|&v| band_cost_bits(v)).sum();
        assert!(total <= budget);
        assert!(a[3] >= a[0], "loud band got fewer bits: {a:?}");
        assert_eq!(a[3], 9, "loud band should saturate");
    }
}

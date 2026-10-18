//! Vendor arithmetic primitives shared by the ported DSP kernels.
//!
//! Recovered from the decompiled R2 module (`roundConvergent` / `aslSat`
//! appear throughout `SSLMS_Encode_*` and `QuantizationNU_*`):
//!
//! * `roundConvergent(x, n)` — arithmetic right shift by `n` with
//!   round-to-nearest, ties-to-even (NOT plain `>>`).
//! * `asl_sat(x, n)` — arithmetic left shift by `n` with saturation to the
//!   i32 range.
//!
//! SPDX-License-Identifier: MIT

/// `roundConvergent(value, bits)`: convergent (ties-to-even) rounding shift.
#[inline]
pub fn cround(value: i32, bits: u32) -> i32 {
    debug_assert!(bits > 0 && bits < 32);
    let v = value as i64;
    let add = (1i64 << (bits - 1)) - 1 + ((v >> bits) & 1);
    ((v + add) >> bits) as i32
}

/// `aslSat(value, n)`: saturating arithmetic left shift.
#[inline]
pub fn asl_sat(value: i32, n: u32) -> i32 {
    let v = (value as i64) << n;
    if v > i32::MAX as i64 {
        i32::MAX
    } else if v < i32::MIN as i64 {
        i32::MIN
    } else {
        v as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cround_ties_to_even() {
        assert_eq!(cround(8, 3), 1); // 1.0
        assert_eq!(cround(12, 3), 2); // 1.5 -> 2 (even)
        assert_eq!(cround(4, 3), 0); // 0.5 -> 0 (even)
        assert_eq!(cround(-12, 3), -2);
        assert_eq!(cround(-4, 3), 0);
        assert_eq!(cround(7, 3), 1);
    }

    #[test]
    fn asl_sat_clamps() {
        assert_eq!(asl_sat(1, 8), 256);
        assert_eq!(asl_sat(0x01000000, 8), i32::MAX);
        assert_eq!(asl_sat(-0x01000000, 8), i32::MIN);
    }
}

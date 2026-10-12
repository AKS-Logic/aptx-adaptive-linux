//! Output framing (spec `a2dp-integration.md` §7): 8-byte header, payload byte-swapped in
//! 16-bit units.

use crate::tables::{Profile, Row};

pub const HEADER_LEN: usize = 8;

const HQ_LATENCY_TARGET: i64 = 70;
const LL_LATENCY_TARGET: i64 = 69;
const HQ_MIN_SINK_BUFFER: i64 = 20;
const LL_MIN_SINK_BUFFER: i64 = 5;
const MAX_SINK_BUFFER: i64 = 200;

/// Header bytes 0..1, in units of 8 µs (§7.1), with the reference defaults.
///
/// UNCERTAIN: leaves out the "buffered audio" term the reference subtracts, and the R2.2 and
/// TWM adjustments, so it's constant for a given row.
#[must_use]
fn ttp_adjustment(profile: Profile, period_ms: u32) -> u16 {
    let (base, min_buf) = match profile {
        Profile::Hq => (4000 * HQ_LATENCY_TARGET - 62_000, HQ_MIN_SINK_BUFFER),
        Profile::Ll => (1000 * LL_LATENCY_TARGET - 12_000, LL_MIN_SINK_BUFFER),
    };
    let us = base - (i64::from(period_ms) + 1) * 1000;
    let lo = 1000 * min_buf + 10_000;
    let hi = 4000 * MAX_SINK_BUFFER + 10_000;
    (us.clamp(lo, hi) / 8) as u16
}

#[must_use]
pub fn header(profile: Profile, row: &Row) -> [u8; HEADER_LEN] {
    let ttp = ttp_adjustment(profile, row.period_ms).to_le_bytes();
    [
        ttp[0],
        ttp[1],
        row.period_byte(),
        row.cfg,
        0,
        0,
        0,
        profile.mode_byte(),
    ]
}

/// Copies `src` into `dst` with each 16-bit pair swapped. A trailing odd byte is copied as is.
pub fn swap16(src: &[u8], dst: &mut [u8]) {
    let dst = &mut dst[..src.len()];
    let (pairs, tail) = src.as_chunks::<2>();
    let (out, out_tail) = dst.as_chunks_mut::<2>();
    for (o, p) in out.iter_mut().zip(pairs) {
        *o = [p[1], p[0]];
    }
    out_tail.copy_from_slice(tail);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::{DEFAULT_QUALITY, row};

    #[test]
    fn px7_header() {
        let r = row(Profile::Hq, 0, DEFAULT_QUALITY).unwrap();
        assert_eq!(ttp_adjustment(Profile::Hq, 14), 25_375);
        assert_eq!(
            header(Profile::Hq, &r),
            [0x1F, 0x63, 0x38, 0x01, 0, 0, 0, 0xAE]
        );
    }

    #[test]
    fn ll_header() {
        let r = row(Profile::Ll, 0, DEFAULT_QUALITY).unwrap();
        assert_eq!(ttp_adjustment(Profile::Ll, 10), 46_000 / 8);
        let h = header(Profile::Ll, &r);
        assert_eq!(h[2..], [40, 0, 0, 0, 0, 0xAD]);
    }

    #[test]
    fn ttp_clamps_low() {
        assert_eq!(ttp_adjustment(Profile::Ll, 60), 15_000 / 8);
    }

    #[test]
    fn swaps_pairs() {
        let mut dst = [0; 6];
        swap16(&[0x00, 0x81, 0xa1, 0xd3, 0x10], &mut dst);
        assert_eq!(dst, [0x81, 0x00, 0xd3, 0xa1, 0x10, 0]);
    }
}

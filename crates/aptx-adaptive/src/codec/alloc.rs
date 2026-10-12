//! Bit allocation (spec `codec-core.md` §8.2). The reference allocator isn't specified, so this
//! is pluggable: any allocation in {0, 2..16} within the per-band caps decodes.

use super::MAX_WIDTH;
use super::pack::min_pair_bits;

/// Picks one frame's allocation.
pub trait Allocator: Send {
    /// `x` is the frame's subband values, `max.len()` per block. `max` caps each band. `fits`
    /// codes the frame exactly and says whether the result fits the frame's share of the
    /// packet. Writes `max.len()` allocations to `out`.
    ///
    /// The packetiser checks the result: anything invalid (see [`valid`]) or past the packet's
    /// hard limit is replaced by [`floor`].
    fn allocate(
        &mut self,
        x: &[i32],
        max: &[u8],
        fits: &mut dyn FnMut(&[u8]) -> bool,
        out: &mut [u8],
    );
}

/// What [`floor`] and [`WaterFill`] give band 0 of each channel at least, so a block pair
/// costs at least a byte (see [`valid`]).
pub const MIN_LOW_BAND_BITS: u8 = 4;

/// Highest allocation the absolute coding can carry: 4-band pairs have no escape (§7.2).
#[must_use]
pub fn max_alloc(subbands: usize) -> u8 {
    if subbands == 4 { 9 } else { 16 }
}

/// Whether an allocation can be coded and parsed back. A frame's end is only implied by where
/// the data stops (§7.1), and the last byte has up to 7 zero pad bits, so every block pair has
/// to cost at least 8 bits. The exception is all zero on 8 bands, which carries its block count
/// (§7.2).
#[must_use]
pub fn valid(bits: &[u8], subbands: usize) -> bool {
    let top = max_alloc(subbands);
    let codable = bits.iter().all(|&b| b == 0 || (2..=top).contains(&b));
    let zero_escape = subbands == 8 && bits.iter().all(|&b| b == 0);
    codable && (zero_escape || min_pair_bits(subbands, bits) >= 8)
}

/// The cheapest valid allocation: band 0 of each channel at [`MIN_LOW_BAND_BITS`].
pub fn floor(subbands: usize, out: &mut [u8]) {
    for (i, o) in out.iter_mut().enumerate() {
        *o = if i % subbands == 0 {
            MIN_LOW_BAND_BITS
        } else {
            0
        };
    }
}

/// Water-filling on per-band peak level: about one bit per 6 dB above a common water level,
/// with the level searched for the most bits that fit. Not the reference's allocator.
#[derive(Debug, Clone, Copy, Default)]
pub struct WaterFill;

const LEVEL_MIN: i32 = -40;
/// High enough that every band is at its minimum.
const LEVEL_MAX: i32 = 40;

/// Peak magnitude per band in the 24-bit domain, as a bit length.
fn peak_bits(x: &[i32], width: usize) -> [i32; MAX_WIDTH] {
    let mut peaks = [0u32; MAX_WIDTH];
    for block in x.chunks_exact(width) {
        for (p, v) in peaks.iter_mut().zip(block) {
            *p = (*p).max(v.unsigned_abs() >> 8);
        }
    }
    peaks.map(|p| (32 - p.leading_zeros()) as i32)
}

fn at_level(peaks: &[i32], max: &[u8], subbands: usize, level: i32, out: &mut [u8]) {
    for (i, ((o, &p), &m)) in out.iter_mut().zip(peaks).zip(max).enumerate() {
        let m = m.min(max_alloc(subbands));
        let b = (p - level).clamp(0, i32::from(m)) as u8;
        let b = if b == 1 { 2.min(m) } else { b };
        *o = if i % subbands == 0 {
            b.max(MIN_LOW_BAND_BITS)
        } else {
            b
        };
    }
}

impl Allocator for WaterFill {
    fn allocate(
        &mut self,
        x: &[i32],
        max: &[u8],
        fits: &mut dyn FnMut(&[u8]) -> bool,
        out: &mut [u8],
    ) {
        let width = max.len();
        let subbands = width / 2;
        let peaks = peak_bits(x, width);
        // Smallest level (most bits) that fits. LEVEL_MAX is the fallback, fitting or not.
        let (mut lo, mut hi) = (LEVEL_MIN, LEVEL_MAX);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            at_level(&peaks[..width], max, subbands, mid, out);
            if fits(out) {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        at_level(&peaks[..width], max, subbands, lo, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_allocations() {
        let max = [9; 8];
        let x: Vec<i32> = (0..64 * 8).map(|i| (i * 7919 % 100_003) << 8).collect();
        for budget in [0, 50, 200, 1000, 100_000] {
            let mut out = [0; 8];
            WaterFill.allocate(
                &x,
                &max,
                &mut |b| b.iter().map(|&v| usize::from(v)).sum::<usize>() * 64 <= budget,
                &mut out,
            );
            assert!(valid(&out, 4), "{budget}: {out:?}");
            assert!(out.iter().zip(&max).all(|(b, m)| b <= m));
        }
        let mut f = [9; 8];
        floor(4, &mut f);
        assert_eq!(f, [4, 0, 0, 0, 4, 0, 0, 0]);
        assert!(valid(&f, 4));
        assert!(!valid(&[4, 1, 0, 0, 4, 0, 0, 0], 4));
        assert!(!valid(&[10, 0, 0, 0, 4, 0, 0, 0], 4));
        // A pair at 3 + 2 bits costs at least 4 + 3 bits, which pad bits could imitate.
        assert!(!valid(&[3, 0, 0, 0, 2, 0, 0, 0], 4));
        assert!(valid(&[3, 0, 0, 0, 3, 0, 0, 0], 4));
        assert!(!valid(&[0; 8], 4));
        assert!(valid(&[0; 16], 8));
    }
}

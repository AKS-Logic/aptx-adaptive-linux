//! Vendor R2 loudness bit allocator.
//!
//! This is a direct port of the music allocator in the shipped CPH2749
//! `libaptXAdaptiveEnc.so` (decompiled in
//! `audio/aptx-adaptive/analysis/ghidra/out/r2-encoder-full-decomp.c`),
//! using the table contents verified against that ELF.  It implements the
//! two runners that matter for the aptX Adaptive profiles:
//!
//! * [`alloc_ull`] — `axBitAllocLoudnessEncode_Run_ULL`: the ULL/low-latency
//!   path.  `CalculateBitPool` gets a non-zero `ull_scale`, then the shared
//!   `CalculateBitAllocation` chain runs.
//! * [`alloc_96k`] — `axBitAllocLoudnessEncode_Run_music_96k`: the
//!   96 kHz/Hi-Res path.  The vendor walks two independent 8-field
//!   dual-mono channel blocks, each with half the frame pool and the
//!   `8DualMono` rate-4 (96 kHz) offset table; this function reproduces
//!   that layout.  [`alloc_music_96k`] keeps the older single-row variant.
//!
//! Both runners share the same core, exactly as the vendor does:
//!
//! ```text
//! energy -> GetScaleFactors -> GetBitNeed -> GetBitSlice -> GetDistribution
//! ```
//!
//! Two vendor conditions are easy to get backwards from the decompiler's
//! predicate output and are taken from the Hexagon disassembly here:
//! GetDistribution's fill passes top up fields that already carry more than
//! one bit (asm 0xc104 / 0xc198), and DigitalBlack sums the current widths
//! of the non-silent fields (asm 0xbe54), not the scale factors.
//!
//! The returned array has one width per composer field
//! (`channel * 8 + band`): 0 = inactive, 2..=16 = active.  Width 1 is mapped
//! to 2 because the packer's `GetBitAllocation` view does the same (the
//! bitstream's allocation nibble cannot represent width 1 directly).
//!
//! SPDX-License-Identifier: MIT

// The tables are the byte-exact CPH2749 arrays in
// `bitalloc_vendor_tables`; keeping one copy makes the allocator and the
// extraction evidence agree by construction.
use crate::bitalloc_vendor_tables::{
    MIN_WIDTHS_ZERO as MIN_WIDTHS, NMAX_8_250 as MAX_250, NMAX_8_320 as MAX_320,
    NMAX_8_370 as MAX_370, NMAX_8_420 as MAX_420, NMAX_8_520 as MAX_520, NMAX_8_LEN as NELEMS,
    NMAX_8_MAX as MAX_8BAND, NMAX_96K_420 as MAX_96_420, NMAX_96K_500 as MAX_96_500,
    NMAX_96K_600 as MAX_96_600, NMAX_96K_650 as MAX_96_650, NMAX_96K_MAX as MAX_96_MAX,
    N_BANDS_8 as NBANDS, OFFSET_8_STEREO, OFFSET_8_STEREO_RATE4_OVERREAD as OFF_8_STEREO_RATE4,
    OFFSET_96K as OFF_96K,
};

/// 8-band stereo offset table, rate entries 0..3 (re-exported for tests).
const OFF_8_STEREO: [[i8; NELEMS]; 4] = OFFSET_8_STEREO;

/// One frame's allocator parameters.
#[derive(Clone, Copy, Debug)]
struct Params {
    sample_rate: u32,
    /// Bits available to the band payload per 8-sample block.
    pool: i32,
    /// Requested bit rate used to pick the max-width table.
    total_rate: u32,
    /// True for the 96 kHz runner (selects the 96 kHz max ladder and table).
    music_96k: bool,
    /// cfg+0x3c: joint-stereo preparation (zero the second channel's SFs).
    joint: bool,
    /// cfg+0x10 == 3: run DigitalBlack after the distribution.
    mode3: bool,
}

/// Vendor rate-index mapping used by `GetBitNeed`.
fn rate_index(sample_rate: u32) -> usize {
    if sample_rate <= 16_000 {
        0
    } else if sample_rate <= 32_000 {
        1
    } else if sample_rate <= 44_100 {
        2
    } else if sample_rate < 48_001 {
        3
    } else {
        4
    }
}

/// `CalculateBitPool` divisor != 0 (the ULL branch).
///
/// `q = min(80, (bit_rate << 19) / (sample_rate * ull_scale))`; two channel
/// blocks halve it.  `ull_scale` is Q16 (65536 = 1.0).
pub(crate) fn calculate_bit_pool_ull(
    bit_rate: u32,
    sample_rate: u32,
    n_channel_blocks: usize,
    ull_scale: u32,
) -> i32 {
    let denom = u64::from(sample_rate) * u64::from(ull_scale);
    if denom == 0 {
        return 0;
    }
    let q = ((u64::from(bit_rate) << 19) / denom).min(80) as i32;
    if n_channel_blocks == 2 {
        q / 2
    } else {
        q
    }
}

/// `CalculateBitPool` divisor == 0 (ConstantBitRate / music_96k).
///
/// Kept separate from the ULL pool: the 96 kHz runner calls it with the
/// mode-3 target/floor clamp.
#[allow(dead_code)] // exercised by the 96 kHz runner and unit tests
///
/// Clamp the requested rate to `[floor, nch * target]`, then convert bits/s
/// to bits per 8-band block with the vendor's quotient/remainder split.
pub(crate) fn calculate_bit_pool_music(
    bit_rate: u32,
    sample_rate: u32,
    n_channels: usize,
    target_rate: u32,
    floor_rate: u32,
) -> i32 {
    if sample_rate == 0 {
        return 0;
    }
    let nch = if n_channels == 2 { 2u64 } else { 1u64 };
    let cap = u64::from(target_rate) * nch;
    let req = if u64::from(bit_rate) > cap {
        cap
    } else {
        u64::from(bit_rate).max(u64::from(floor_rate))
    };
    let q = req / u64::from(sample_rate);
    let rem = req - q * u64::from(sample_rate);
    (q * NBANDS as u64 + rem * NBANDS as u64 / u64::from(sample_rate)) as i32
}

/// `GetScaleFactors`: `sf = floor(log2(((E + 128) >> 8) - 1))`, clamped to
/// zero for the two smallest magnitude classes.  `E` is the band energy
/// (magnitude-domain, as used by the shipped allocator); negative and
/// non-finite inputs are treated as zero.
fn scale_factor(energy: f64) -> i32 {
    let e = if energy.is_nan() {
        0.0
    } else if energy.is_infinite() {
        if energy.is_sign_positive() {
            i32::MAX as f64
        } else {
            0.0
        }
    } else {
        energy.max(0.0)
    };
    let q = ((e + 128.0) / 256.0).floor();
    if q <= 2.0 {
        return 0;
    }
    let q = q.min(i32::MAX as f64) as u32;
    31 - (q - 1).leading_zeros() as i32
}

/// `GetBitNeed` (the fused music entry point, ceil division).
fn bit_need(sf: &[i32; NELEMS], offset: &[i8; NELEMS]) -> ([i32; NELEMS], i32) {
    let mut need = [0i32; NELEMS];
    let mut max_need = 0i32;
    for i in 0..NELEMS {
        let v = sf[i];
        let n = if v == 0 {
            -5
        } else {
            let d = v - i32::from(offset[i]);
            if d < 1 {
                d
            } else {
                (d + 1) >> 1
            }
        };
        need[i] = n;
        if n > max_need {
            max_need = n;
        }
    }
    (need, max_need)
}

/// `GetBitSlice`: find the water level whose counted width units are just
/// below the pool.
fn bit_slice(pool: i32, need: &[i32; NELEMS], max_w: &[i32; NELEMS], max_need: i32) -> i32 {
    let mut level = max_need + 1;
    let mut next = level - 1;
    let mut prev_count = 0i32;
    let mut total = 0i32;
    let mut count = 0i32;
    if max_need >= -11 {
        loop {
            next = level - 1;
            count = 0;
            for i in 0..NELEMS {
                if level < need[i] && need[i] < max_w[i] + next {
                    count += 1;
                }
                if need[i] == level {
                    count += 2;
                }
            }
            total += prev_count;
            if total + count >= pool || level <= -10 {
                break;
            }
            prev_count = count;
            level = next;
        }
    }
    next + if total + count == pool { -1 } else { 0 }
}

fn joint_adjust(out: &mut [i32; NELEMS], total: i32) -> i32 {
    let old = out[7];
    out[7] = if out[6] < 1 { 0 } else { 2 };
    total + out[7] - old
}

/// `BitAllocLoudness_DigtalBlack` (the vendor's spelling).
fn digital_black(out: &mut [i32; NELEMS], sf: &[i32; NELEMS], max_w: &[i32; NELEMS], cond: bool) {
    let mut sum_low = 0i32;
    let mut sum_high = 0i32;
    for i in 0..NBANDS {
        if sf[i] != 0 {
            sum_low += out[i];
        }
    }
    for i in NBANDS..NELEMS {
        if sf[i] != 0 {
            sum_high += out[i];
        }
    }
    for i in 0..NELEMS {
        if sf[i] == 0 {
            out[i] = 0;
        }
    }
    let mut total = sum_low + sum_high;
    if total == 0 {
        return;
    }
    if cond {
        if sum_high == 0 {
            total += 2;
            out[8] = 2;
        }
        if sum_low == 0 {
            total += 2;
            out[0] = 2;
        }
    }
    if !(1..=6).contains(&total) {
        return;
    }
    let mut remaining = 7 - total;
    // first pass: only fields that are already active
    for i in 0..NELEMS {
        if out[i] != 0 && out[i] < max_w[i] {
            let add = (max_w[i] - out[i]).min(remaining);
            out[i] += add;
            remaining -= add;
            if out[i] == 1 {
                remaining -= 1;
                out[i] = 2;
            }
        }
        if remaining <= 0 {
            break;
        }
    }
    // second pass: from field 0, any field
    if remaining > 0 {
        for i in 0..NELEMS {
            let add = (max_w[i] - out[i]).min(remaining);
            if add > 0 {
                out[i] += add;
                remaining -= add;
                if out[i] == 1 {
                    remaining -= 1;
                    out[i] = 2;
                }
            }
            if remaining <= 0 {
                break;
            }
        }
    }
}

/// `BitAllocLoudness_GetDistribution`.
fn get_distribution(
    pool: i32,
    need: &[i32; NELEMS],
    cutoff: i32,
    max_w: &[i32; NELEMS],
    sf: &[i32; NELEMS],
    joint: bool,
    mode3: bool,
) -> [i32; NELEMS] {
    let mut out = [0i32; NELEMS];
    out[0] = 2;
    out[NBANDS] = 2;
    let mut total = 0i32;
    for i in 0..NELEMS {
        if need[i] >= cutoff + 2 {
            let mut w = need[i] - cutoff;
            if w > max_w[i] {
                w = max_w[i];
            }
            out[i] = w;
        }
        if out[i] < MIN_WIDTHS[i] {
            out[i] = MIN_WIDTHS[i];
        }
        total += out[i];
    }
    if joint {
        total = joint_adjust(&mut out, total);
    }

    // pass 1: band-major, channel-minor
    'band: for band in 0..NBANDS {
        for ch in 0..2 {
            let i = ch * NBANDS + band;
            // Vendor quirk (asm 0xc104): the first fill pass tops up
            // fields that already carry more than one bit, not the empty
            // ones.  Width 0/1 only appears via the "need == level + 1"
            // branch, which admits a fresh two-bit field.
            if out[i] > 1 && out[i] < max_w[i] {
                out[i] += 1;
                total += 1;
            } else if need[i] == cutoff + 1 && total + 1 < pool && max_w[i] > 0 {
                out[i] = 2;
                total += 2;
            }
        }
        if total >= pool {
            break 'band;
        }
    }
    if joint {
        total = joint_adjust(&mut out, total);
    }

    // pass 2: cyclic +1 while below the pool
    let mut rounds = 0usize;
    while total < pool {
        for i in 0..NELEMS {
            // Asm 0xc198: top up active fields; an empty field becomes two
            // bits when the max table allows it.
            if out[i] > 1 {
                if out[i] < max_w[i] {
                    out[i] += 1;
                    total += 1;
                }
            } else if out[i] == 0 && max_w[i] > 0 {
                out[i] = 2;
                total += 2;
            }
            if total >= pool {
                break;
            }
        }
        rounds += 1;
        if rounds > 30 {
            break;
        }
    }
    if joint {
        total = joint_adjust(&mut out, total);
    }

    // over budget: decrement from the highest field down
    if pool < total {
        for i in (0..NELEMS).rev() {
            if out[i] > 2 {
                out[i] -= 1;
                total -= 1;
            }
            if total <= pool {
                break;
            }
        }
    }

    if mode3 {
        digital_black(&mut out, sf, max_w, joint);
    }
    if joint {
        out[7] = if out[6] < 1 { 0 } else { 2 };
    }
    out
}

fn select_max_ull(rate: u32) -> &'static [i32; NELEMS] {
    if rate < 250_000 {
        &MAX_250
    } else if rate < 320_000 {
        &MAX_320
    } else if rate < 370_000 {
        &MAX_370
    } else if rate < 420_000 {
        &MAX_420
    } else if rate < 520_000 {
        &MAX_520
    } else {
        &MAX_8BAND
    }
}

fn select_max_96k(rate: u32) -> &'static [i32; NELEMS] {
    if rate < 420_000 {
        &MAX_96_420
    } else if rate < 500_000 {
        &MAX_96_500
    } else if rate < 600_000 {
        &MAX_96_600
    } else if rate < 650_000 {
        &MAX_96_650
    } else {
        &MAX_96_MAX
    }
}

fn offset_table(p: &Params) -> &'static [i8; NELEMS] {
    if p.music_96k {
        return &OFF_96K;
    }
    match rate_index(p.sample_rate) {
        0..=2 => &OFF_8_STEREO[rate_index(p.sample_rate)],
        3 => &OFF_8_STEREO[3],
        _ => &OFF_8_STEREO_RATE4,
    }
}

/// Convert the frame's summed squared band energy into the peak-magnitude
/// domain the vendor's `GetScaleFactors` consumes.  For a sine of amplitude
/// `A`, `sqrt(2 * sum(x^2) / n) ~= A`; using the peak rather than the RMS
/// keeps transient frames from under-allocating.
fn energy_to_magnitudes(energy: &[f64; NELEMS], calls: usize) -> [f64; NELEMS] {
    let n = calls.max(1) as f64;
    let mut mags = [0.0f64; NELEMS];
    for i in 0..NELEMS {
        let e = energy[i];
        mags[i] = if e.is_finite() && e > 0.0 {
            (2.0 * e / n).sqrt()
        } else if e.is_infinite() {
            f64::INFINITY
        } else {
            0.0
        };
    }
    mags
}

fn allocate(energy: &[f64; NELEMS], p: &Params) -> [u8; NELEMS] {
    let mut sf = [0i32; NELEMS];
    for i in 0..NELEMS {
        sf[i] = scale_factor(energy[i]);
    }
    if p.joint {
        // CalculateBitAllocation zeroes the second channel's SFs before
        // GetBitNeed when cfg+0x3c is set.
        for i in NBANDS..NELEMS {
            sf[i] = 0;
        }
    }
    let offset = offset_table(p);
    let (need, max_need) = bit_need(&sf, offset);
    let rate = p.total_rate.saturating_mul(2);
    let max_w = if p.music_96k {
        select_max_96k(rate)
    } else {
        select_max_ull(rate)
    };
    let cutoff = bit_slice(p.pool, &need, max_w, max_need);
    let out = get_distribution(p.pool, &need, cutoff, max_w, &sf, p.joint, p.mode3);

    let mut widths = [0u8; NELEMS];
    for i in 0..NELEMS {
        let w = out[i].clamp(0, 16) as u8;
        widths[i] = match w {
            0 => 0,
            1 => 2,
            w => w,
        };
    }
    widths
}

/// ULL/low-latency runner (`axBitAllocLoudnessEncode_Run_ULL`).
///
/// `energy` is one value per field (`channel * 8 + band`) in the same
/// magnitude domain as the compressor's band energy; `calls` is the number
/// of 8-sample blocks in the frame and `stream_bits` the composer stream
/// budget.  The resulting widths are the 16 composer fields the R2 frame
/// writer consumes.
pub fn alloc_ull(energy: &[f64; NELEMS], calls: usize, stream_bits: usize) -> [u8; NELEMS] {
    let calls = calls.max(1);
    let samples = (calls * 8) as u64;
    let bit_rate = ((stream_bits as u64) * 48_000 / samples).min(u32::MAX as u64) as u32;
    let pool = calculate_bit_pool_ull(bit_rate, 48_000, 1, 65_536);
    let total_rate = ((48_000u64 * pool.max(0) as u64) >> 3).min(u32::MAX as u64) as u32;
    let mags = energy_to_magnitudes(energy, calls);
    allocate(
        &mags,
        &Params {
            sample_rate: 48_000,
            pool,
            total_rate,
            music_96k: false,
            joint: false,
            mode3: true,
        },
    )
}

/// 96 kHz/Hi-Res runner (`axBitAllocLoudnessEncode_Run_music_96k`).
///
/// The current fixed 48 kHz/160-block integration uses [`alloc_ull`]; this
/// entry point is ready for the Hi-Res profile when the encoder grows its
/// 96 kHz path.
#[allow(dead_code)] // wired when the Hi-Res/96 kHz encoder path lands
///
/// `calls` is the block count (96 for the 8 ms/768-sample Hi-Res frame) and
/// `stream_bits` the composer budget.  Uses the 96 kHz max-width ladder and
/// the `BitAllocOffset_96k` table.
pub fn alloc_music_96k(energy: &[f64; NELEMS], calls: usize, stream_bits: usize) -> [u8; NELEMS] {
    let calls = calls.max(1);
    let samples = (calls * 8) as u64;
    let bit_rate = ((stream_bits as u64) * 96_000 / samples).min(u32::MAX as u64) as u32;
    let pool = calculate_bit_pool_music(bit_rate, 96_000, 2, 350_000, 90_000);
    let total_rate = bit_rate.min(700_000);
    let mags = energy_to_magnitudes(energy, calls);
    allocate(
        &mags,
        &Params {
            sample_rate: 96_000,
            pool,
            total_rate,
            music_96k: true,
            joint: false,
            mode3: true,
        },
    )
}

/// Vendor Hi-Res runner (`axBitAllocLoudnessEncode_Run_music_96k`),
/// faithful channel-block layout.
///
/// The 96 kHz runner walks **two independent 8-field dual-mono blocks**,
/// one per channel: each gets half of the frame pool, block mode 0 selects
/// the `8DualMono` rate-4 (96 kHz) offset table, and GetDistribution is
/// called with one group, so only field 0 of each block receives the
/// mandatory width-2 seed.  The returned row is channel-major
/// (channel * 8 + band).
///
/// [`alloc_music_96k`] remains available as the older single-row variant
/// (one 16-field stereo block); use this function when matching the vendor
/// runner is required.
pub fn alloc_96k(energy: &[f64; NELEMS], calls: usize, stream_bits: usize) -> [u8; NELEMS] {
    let calls = calls.max(1);
    let samples = (calls * 8) as u64;
    let bit_rate = ((stream_bits as u64) * 96_000 / samples).min(u32::MAX as u64) as u32;
    let pool = calculate_bit_pool_music(bit_rate, 96_000, 2, 350_000, 90_000);
    let rate = bit_rate.min(700_000).saturating_mul(2);
    let max_w = select_max_96k(rate);
    let mags = energy_to_magnitudes(energy, calls);
    let half = pool / 2;
    let left = allocate_96k_block(&mags[..NBANDS], half, max_w);
    let right = allocate_96k_block(&mags[NBANDS..], half, max_w);

    let mut widths = [0u8; NELEMS];
    widths[..NBANDS].copy_from_slice(&left);
    widths[NBANDS..].copy_from_slice(&right);
    widths
}

/// One channel block of the 96 kHz runner: 8 bands, one group, dual-mono
/// offset table.  Mirrors the per-block calls in Run_music_96k @ 0xd384.
fn allocate_96k_block(mags: &[f64], pool: i32, max_w: &'static [i32; NELEMS]) -> [u8; NBANDS] {
    let mut sf = [0i32; NELEMS];
    for b in 0..NBANDS {
        sf[b] = scale_factor(mags[b]);
    }
    let (need, max_need) = bit_need(&sf, &OFF_96K);
    let cutoff = bit_slice_8(pool, &need, max_w, max_need);
    let out = get_distribution_8(pool, &need, cutoff, max_w, &sf);
    let mut widths = [0u8; NBANDS];
    for b in 0..NBANDS {
        let w = out[b].clamp(0, 16) as u8;
        widths[b] = match w {
            0 => 0,
            1 => 2,
            w => w,
        };
    }
    widths
}

/// GetBitSlice restricted to the first eight fields (one dual-mono block).
fn bit_slice_8(pool: i32, need: &[i32; NELEMS], max_w: &[i32; NELEMS], max_need: i32) -> i32 {
    let mut level = max_need + 1;
    let mut next = level - 1;
    let mut prev_count = 0i32;
    let mut total = 0i32;
    let mut count = 0i32;
    if max_need >= -11 {
        loop {
            next = level - 1;
            count = 0;
            for i in 0..NBANDS {
                if level < need[i] && need[i] < max_w[i] + next {
                    count += 1;
                }
                if need[i] == level {
                    count += 2;
                }
            }
            total += prev_count;
            if total + count >= pool || level <= -10 {
                break;
            }
            prev_count = count;
            level = next;
        }
    }
    next + if total + count == pool { -1 } else { 0 }
}

/// GetDistribution restricted to one 8-field dual-mono block (nch == 1, no
/// joint coupling, no second-half seeding).
fn get_distribution_8(
    pool: i32,
    need: &[i32; NELEMS],
    cutoff: i32,
    max_w: &[i32; NELEMS],
    sf: &[i32; NELEMS],
) -> [i32; NELEMS] {
    let mut out = [0i32; NELEMS];
    out[0] = 2;
    let mut total = 0i32;
    for i in 0..NBANDS {
        if need[i] >= cutoff + 2 {
            out[i] = (need[i] - cutoff).min(max_w[i]);
        }
        if out[i] < MIN_WIDTHS[i] {
            out[i] = MIN_WIDTHS[i];
        }
        total += out[i];
    }

    // pass 1: band-major (one channel)
    for band in 0..NBANDS {
        if out[band] > 1 && out[band] < max_w[band] {
            out[band] += 1;
            total += 1;
        } else if need[band] == cutoff + 1 && total + 1 < pool && max_w[band] > 0 {
            out[band] = 2;
            total += 2;
        }
        if total >= pool {
            break;
        }
    }

    // pass 2: cyclic +1 while below the pool
    let mut rounds = 0usize;
    while total < pool {
        for i in 0..NBANDS {
            if out[i] > 1 {
                if out[i] < max_w[i] {
                    out[i] += 1;
                    total += 1;
                }
            } else if out[i] == 0 && max_w[i] > 0 {
                out[i] = 2;
                total += 2;
            }
            if total >= pool {
                break;
            }
        }
        rounds += 1;
        if rounds > 30 {
            break;
        }
    }

    // over budget
    if pool < total {
        for i in (0..NBANDS).rev() {
            if out[i] > 2 {
                out[i] -= 1;
                total -= 1;
            }
            if total <= pool {
                break;
            }
        }
    }

    // DigitalBlack with one group: sum the non-silent widths, zero the rest.
    let mut sum = 0i32;
    for i in 0..NBANDS {
        if sf[i] != 0 {
            sum += out[i];
        }
    }
    for i in 0..NBANDS {
        if sf[i] == 0 {
            out[i] = 0;
        }
    }
    if (1..=6).contains(&sum) {
        let mut remaining = 7 - sum;
        for i in 0..NBANDS {
            if out[i] != 0 && out[i] < max_w[i] {
                let add = (max_w[i] - out[i]).min(remaining);
                out[i] += add;
                remaining -= add;
                if out[i] == 1 {
                    remaining -= 1;
                    out[i] = 2;
                }
            }
            if remaining <= 0 {
                break;
            }
        }
        if remaining > 0 {
            for i in 0..NBANDS {
                let add = (max_w[i] - out[i]).min(remaining);
                if add > 0 {
                    out[i] += add;
                    remaining -= add;
                    if out[i] == 1 {
                        remaining -= 1;
                        out[i] = 2;
                    }
                }
                if remaining <= 0 {
                    break;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sum(w: &[u8; NELEMS]) -> usize {
        w.iter().map(|&x| x as usize).sum()
    }

    fn all_active(w: &[u8; NELEMS]) -> bool {
        w.iter().all(|&x| x == 0 || (2..=16).contains(&x))
    }

    #[test]
    fn scale_factors_follow_vendor_log2_rule() {
        assert_eq!(scale_factor(0.0), 0);
        assert_eq!(scale_factor(255.0), 0);
        assert_eq!(scale_factor(256.0), 0);
        assert_eq!(scale_factor(256.0 * 3.0), 1);
        assert_eq!(scale_factor(256.0 * 4.0), 1);
        assert_eq!(scale_factor(256.0 * 5.0), 2);
        assert_eq!(scale_factor(256.0 * 9.0), 3);
        assert_eq!(scale_factor(f64::NAN), 0);
        assert!(scale_factor(f64::INFINITY) >= 22);
    }

    #[test]
    fn bit_need_uses_ceil_for_music_path() {
        let mut sf = [0i32; NELEMS];
        sf[0] = 8; // d = 8, ceil(8/2) = 4
        sf[1] = 9; // d = 9, ceil(9/2) = 5
        sf[2] = 2; // d = 2, ceil(2/2) = 1
        sf[3] = 1; // d = 1, 1
        sf[4] = 0; // silent -> -5
        let off = [0i8; NELEMS];
        let (need, max_need) = bit_need(&sf, &off);
        assert_eq!(
            (need[0], need[1], need[2], need[3], need[4]),
            (4, 5, 1, 1, -5)
        );
        assert_eq!(max_need, 5);
    }

    #[test]
    fn bit_slice_and_distribution_fill_the_pool() {
        let need = [8, 4, -5, -5, -5, -5, -5, -5, 8, 4, -5, -5, -5, -5, -5, -5];
        let max_w = MAX_8BAND;
        let cutoff = bit_slice(32, &need, &max_w, 8);
        let sf = [1i32; NELEMS];
        let out = get_distribution(32, &need, cutoff, &max_w, &sf, false, false);
        let total: i32 = out.iter().sum();
        assert!(
            total >= 30 && total <= 34,
            "total {total}, cutoff {cutoff}, out {out:?}"
        );
        // Both loud fields are active.  The vendor fills band-major then
        // channel-minor, so if the pool runs out mid-scan out[0] can be one
        // unit ahead of out[8]; it is never behind.
        assert!(out[0] >= out[8], "out {out:?}");
        assert!(
            out[0] > 0 && out[8] > 0,
            "the loud pair must be active: {out:?}"
        );
    }

    #[test]
    fn ull_allocation_fits_the_fixed_656_byte_frame() {
        // Frame budget used by encode_frame_vendor: 653 stream bytes over
        // 160 blocks.  Energies approximate a music spectrum.
        let energy = [
            1.0e18, 3.0e16, 8.0e15, 2.0e15, 1.0e15, 4.0e14, 2.0e14, 1.0e14, 9.0e17, 2.0e16, 7.0e15,
            1.5e15, 9.0e14, 3.0e14, 1.5e14, 8.0e13,
        ];
        let w = alloc_ull(&energy, 160, 653 * 8);
        assert!(all_active(&w), "widths {w:?}");
        assert!(sum(&w) >= 16, "too little allocation: {w:?}");
        // The distribution may overshoot by a couple of units; the frame
        // writer's fit loop trims any residual overflow.  It must not return
        // a wild over-allocation.
        assert!(sum(&w) <= 32 + NELEMS, "over budget: {w:?} sum {}", sum(&w));
        assert!(
            w[0] >= w[7],
            "low band should get at least as many bits: {w:?}"
        );
    }

    #[test]
    fn silence_allocates_nothing() {
        let energy = [0.0f64; NELEMS];
        let w = alloc_ull(&energy, 160, 653 * 8);
        assert_eq!(w, [0u8; NELEMS], "silence must be digital black");
    }

    #[test]
    fn one_silent_channel_stays_inactive() {
        let mut energy = [0.0f64; NELEMS];
        for i in 0..NBANDS {
            energy[i] = 1.0e17;
        }
        let w = alloc_ull(&energy, 160, 653 * 8);
        assert!(
            w[..NBANDS].iter().any(|&x| x > 0),
            "left channel active: {w:?}"
        );
        assert!(
            w[NBANDS..].iter().all(|&x| x == 0),
            "right channel silent: {w:?}"
        );
    }

    #[test]
    fn music_96k_runner_uses_its_own_caps_and_fits() {
        let energy = [
            1.0e17, 5.0e16, 2.0e16, 1.0e16, 4.0e15, 1.0e15, 4.0e14, 1.0e14, 9.0e16, 4.0e16, 1.8e16,
            9.0e15, 3.5e15, 9.0e14, 3.5e14, 9.0e13,
        ];
        let w = alloc_music_96k(&energy, 96, 653 * 8);
        assert!(all_active(&w), "widths {w:?}");
        assert!(sum(&w) >= 32, "too little allocation: {w:?}");
        assert!(sum(&w) <= 54 + NELEMS, "over budget: {w:?} sum {}", sum(&w));
        // Every field is still inside the writer's 16-bit ceiling.
        assert!(w.iter().all(|&x| x <= 16));
    }

    #[test]
    fn bit_pool_formulas_match_the_notes() {
        // 360-byte (2880-bit) 10 ms frame at 48 kHz: 480 samples = 60
        // blocks -> 48 bits/block.  The ULL formula with Q16 unity gives the
        // same value.
        assert_eq!(calculate_bit_pool_ull(288_000, 48_000, 1, 65_536), 48);
        assert_eq!(calculate_bit_pool_ull(288_000, 48_000, 2, 65_536), 24);
        // 656-byte, 1200-sample 96 kHz music frame -> ~43.7 bits/block.
        assert_eq!(
            calculate_bit_pool_music(524_800, 96_000, 2, 350_000, 90_000),
            43
        );
        // Clamp: below floor -> floor; above 2*target -> cap.
        assert_eq!(
            calculate_bit_pool_music(10_000, 96_000, 2, 350_000, 90_000),
            7
        );
        assert_eq!(
            calculate_bit_pool_music(2_000_000, 96_000, 2, 350_000, 90_000),
            58
        );
    }

    #[test]
    fn max_table_thresholds_are_strict() {
        assert_eq!(*select_max_ull(249_999), MAX_250);
        assert_eq!(*select_max_ull(250_000), MAX_320);
        assert_eq!(*select_max_ull(419_999), MAX_420);
        assert_eq!(*select_max_ull(420_000), MAX_520);
        assert_eq!(*select_max_ull(520_000), MAX_8BAND);
        assert_eq!(*select_max_96k(419_999), MAX_96_420);
        assert_eq!(*select_max_96k(420_000), MAX_96_500);
        assert_eq!(*select_max_96k(650_000), MAX_96_MAX);
    }

    #[test]
    fn ninety_six_two_block_tone_stays_in_one_channel() {
        let mut e = [0.0f64; NELEMS];
        let amp = (1u64 << 20) as f64;
        e[0] = amp * amp * 120.0;
        let w = alloc_96k(&e, 120, 5248);
        assert!(w[0] >= 2, "left tone width = {}", w[0]);
        assert_eq!(&w[1..], &[0u8; 15], "only the left block is active: {w:?}");
        let sum: usize = w.iter().map(|&v| v as usize).sum();
        assert!(sum <= 43, "96k pool must not be exceeded (sum {sum})");
    }

    #[test]
    fn ninety_six_two_block_dense_uses_both_channels() {
        let mut e = [0.0f64; NELEMS];
        for slot in e.iter_mut() {
            let amp = (1u64 << 17) as f64;
            *slot = amp * amp * 120.0;
        }
        let w = alloc_96k(&e, 120, 5248);
        assert!(w[..8].iter().any(|&v| v > 0), "left half empty: {w:?}");
        assert!(w[8..].iter().any(|&v| v > 0), "right half empty: {w:?}");
        let left: usize = w[..8].iter().map(|&v| v as usize).sum();
        let right: usize = w[8..].iter().map(|&v| v as usize).sum();
        assert!(left <= 21 && right <= 21, "block pools exceeded: {w:?}");
    }

    #[test]
    fn allocation_is_deterministic() {
        let energy = [
            1.0e18, 1.0e16, 1.0e15, 1.0e14, 5.0e13, 2.0e13, 1.0e13, 5.0e12, 8.0e17, 8.0e15, 8.0e14,
            8.0e13, 4.0e13, 1.5e13, 8.0e12, 4.0e12,
        ];
        let a = alloc_ull(&energy, 160, 653 * 8);
        let b = alloc_ull(&energy, 160, 653 * 8);
        assert_eq!(a, b);
    }
}

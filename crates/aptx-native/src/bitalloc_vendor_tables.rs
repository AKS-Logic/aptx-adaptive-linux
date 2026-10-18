//! Vendor aptX Adaptive R2 bit-allocation tables.
//!
//! Byte-exact copies from the CPH2749 encoder module
//! audio/aptx-adaptive/binaries/issue8-attachments/extracted/aptxlibs_CPH2749/
//! libaptXAdaptiveEnc.so (ELF32 QUALCOMM DSP6), SHA-256
//! 1ac881571cd1db4d256113d4448304016de73fedebc059824b1b2ed09599f0c9.
//!
//! Decompilation: audio/aptx-adaptive/analysis/ghidra/out/
//! r2-encoder-full-decomp.c. Ghidra loaded the image at +0x10000, so a decomp
//! address 0x1xxxxx is ELF virtual address 0xxxxx, and a PTR_*_00049xxx
//! symbol is the GOT slot at ELF 0x39xxx. The table pointers were resolved
//! from the .rela.dyn R_HEX_RELATIVE addends; sizes are the readelf -sW
//! symbol sizes. Addresses in comments below are ELF virtual addresses (the
//! file offset is identical for .rodata/.data in this module).
//!
//! Types follow the code: offset tables are sbyte (i8); max/min widths are
//! read through int * (i32). Rows are channel-major, [channel][band].
//!
//! Extraction evidence: audio/aptx-adaptive/analysis/ALLOCATOR-R2-NOTES.md,
//! Appendix B. This file is the table half of the vendor allocator; the
//! state machine that consumes it lives in bitalloc_vendor.rs. It contains
//! no allocator algorithm.
//!
//! BitAllocLoudness_GetDistribution (@0x1c020), GetDistribution_
//! ConditionFocus (@0x1be20) and DigtalBlack (@0x1be40) have no static
//! tables; they consume the max/min pointers placed in the allocation
//! context by GetBitNeed_setup/GetBitNeed. AxPackerCfg0_GetBitAllocation
//! (@0x27908) also has no static tables (see the tail comment).
//!
//! SPDX-License-Identifier: MIT (values extracted)

// ---------------------------------------------------------------------------
// Geometry and selector constants
// ---------------------------------------------------------------------------

/// Bands in the 4-band filter bank.
pub const N_BANDS_4: usize = 4;

/// Bands in the 8-band filter bank.
pub const N_BANDS_8: usize = 8;

/// Channels in one allocation row.
pub const N_CHANNELS: usize = 2;

/// Number of rate classes the offset-table selector can produce (0..=4).
pub const N_RATE_CLASSES: usize = 5;

/// Stride of one 4-band offset-table rate entry: 4 bands x 2 channels.
pub const OFFSET_4_STRIDE: usize = 8;

/// Stride of one 8-band offset-table rate entry: 8 bands x 2 channels.
pub const OFFSET_8_STRIDE: usize = 16;

/// Length of one 4-band max-width table: 4 bands x 2 channels.
pub const NMAX_4_LEN: usize = 8;

/// Length of one 8-band max-width table: 8 bands x 2 channels.
pub const NMAX_8_LEN: usize = 16;

/// Length of one nMinBits context array.
pub const MIN_WIDTHS_LEN: usize = 16;

/// Inclusive upper bounds of rate classes 0..3.
///
/// The class is the first index i with sample_rate <= bound[i], otherwise 4.
/// The decomp compares sample_rate > 16000, < 0x7d01, <= 0xac44 and
/// < 0xbb81 (BitAllocLoudness_GetBitNeed_setup @0x1ba04 and
/// BitAllocLoudness_GetBitNeed @0x1bb80), which is equivalent.
pub const RATE_CLASS_UPPER_BOUNDS: [u32; 4] = [16_000, 32_000, 44_100, 48_000];

/// Block-mode value 2 means stereo; every other supported value uses the
/// dual-mono tables. Read from state + 0x7a by BitAllocLoudness_GetBitNeed
/// (@0x1bb80) and BitAllocLoudness_GetBitAlloc_96k (@0x1c290).
pub const CHANNEL_MODE_STEREO: i32 = 2;

/// Bit need emitted for a band whose input energy is zero.
/// BitAllocLoudness_GetBitNeed (@0x1bb80) and
/// BitAllocLoudness_GetBitAlloc_96k (@0x1c290).
pub const BIT_NEED_EMPTY: i32 = -5;

/// BitAllocLoudness_GetBitNeed_setup (@0x1ba04) returns this when the
/// block-mode byte at state + param * 0xc + 0x78 is greater than 4.
pub const GET_BITNEED_SETUP_ERR_MODE: u32 = 0xf035;

/// AxPackerCfg0_GetBitAllocation (@0x27908) maps an allocation width of 1
/// to this value; all other widths are copied unchanged.
pub const PACKER_MIN_WIDTH: i32 = 2;

/// Compared-bitrate thresholds for the 4-band max-width ladder selected by
/// aptXCompressorConfig0_CalculateBitAllocation (@0x1d0a0).
///
/// The compared value is state + 0x4898 doubled, unless the byte at
/// state + 0xd is 1, in which case it is used unchanged. Ladder order is
/// [250, 320, 370, Max]; the music path starts at 320:
/// < 320_000 -> NMAX_4_320, 320_000..369_999 -> NMAX_4_370,
/// >= 370_000 -> NMAX_4_MAX.
pub const BITRATE_THRESHOLDS_4BAND: [u32; 2] = [320_000, 370_000];

/// Compared-bitrate thresholds for the 8-band max-width ladder selected by
/// aptXCompressorConfig0_CalculateBitAllocation (@0x1d0a0).
///
/// Ladder order is [250, 320, 370, 420, 520, Max]:
/// < 250_000 -> NMAX_8_250, < 320_000 -> NMAX_8_320,
/// < 370_000 -> NMAX_8_370, < 420_000 -> NMAX_8_420,
/// < 520_000 -> NMAX_8_520, else NMAX_8_MAX.
pub const BITRATE_THRESHOLDS_8BAND: [u32; 5] = [250_000, 320_000, 370_000, 420_000, 520_000];

/// Compared-bitrate thresholds for the 96 kHz max-width ladder selected by
/// axBitAllocLoudnessEncode_Run_music_96k (@0x1d384).
///
/// Ladder order is [420, 500, 600, 650, Max]:
/// < 420_000 -> NMAX_96K_420, < 500_000 -> NMAX_96K_500,
/// < 600_000 -> NMAX_96K_600, < 650_000 -> NMAX_96K_650, else NMAX_96K_MAX.
pub const BITRATE_THRESHOLDS_96K: [u32; 4] = [420_000, 500_000, 600_000, 650_000];

// BitAllocLoudness_GetBitAlloc_96k (@0x1c290) scalar immediates. The vendor
// has no symbols for these; the names are descriptive only.

/// Stereo base budget compared at the top of GetBitAlloc_96k (0x24).
pub const GET_96K_BASE_STEREO: u32 = 0x24;

/// Non-stereo base budget compared at the top of GetBitAlloc_96k (0x12).
pub const GET_96K_BASE_MONO: u32 = 0x12;

/// Low-sample-rate stereo cap compared by GetBitAlloc_96k (0x34).
pub const GET_96K_LOW_RATE_CAP_STEREO: u32 = 0x34;

/// Low-sample-rate non-stereo cap compared by GetBitAlloc_96k (0x1a).
pub const GET_96K_LOW_RATE_CAP_MONO: u32 = 0x1a;

/// Low-sample-rate stereo headroom used by GetBitAlloc_96k (0x10).
pub const GET_96K_LOW_RATE_HEADROOM_STEREO: u32 = 0x10;

/// Low-sample-rate non-stereo headroom used by GetBitAlloc_96k (8).
pub const GET_96K_LOW_RATE_HEADROOM_MONO: u32 = 0x8;

/// High-sample-rate stereo cap compared by GetBitAlloc_96k (0x48).
pub const GET_96K_HIGH_RATE_CAP_STEREO: u32 = 0x48;

/// High-sample-rate non-stereo cap compared by GetBitAlloc_96k (0x24).
pub const GET_96K_HIGH_RATE_CAP_MONO: u32 = 0x24;

/// Sample-rate threshold for the high-rate branch of GetBitAlloc_96k
/// (0xbb81 = 48001); the high-rate branch is taken at sample_rate >= this.
pub const GET_96K_HIGH_RATE_SAMPLE_RATE: u32 = 0xbb81;

// ---------------------------------------------------------------------------
// Offset tables (sbyte / i8)
// ---------------------------------------------------------------------------

/// BitAllocOffset4StereoTable - .rodata 0x20360, 32 B (4 x 8 i8).
///
/// Lookup is base + rate_index * OFFSET_4_STRIDE. Selected by
/// BitAllocLoudness_GetBitNeed_setup (@0x1ba04) and
/// BitAllocLoudness_GetBitNeed (@0x1bb80) when nBands == 4 and block mode
/// == 2 (stereo). Each row is [ch0 band0..3, ch1 band0..3].
///
/// Only rate classes 0..3 are stored. A class-4 lookup reads the next symbol
/// (BitAllocOffset8StereoTable) in the linked image; preserved byte-exact.
pub static OFFSET_4_STEREO: [[i8; 8]; 4] = [
    [-1, 0, 0, 0, 0, 0, 0, 0],
    [-1, 0, 0, 1, 0, 0, 0, 0],
    [-3, -2, -1, 1, -2, -1, -1, 2],
    [0, 0, 0, 3, 0, 0, 1, 4],
];

/// BitAllocOffset4DualMonoTable - .rodata 0x203c0, 32 B (4 x 8 i8).
///
/// Lookup is base + rate_index * OFFSET_4_STRIDE. Selected by
/// BitAllocLoudness_GetBitNeed_setup (@0x1ba04) and
/// BitAllocLoudness_GetBitNeed (@0x1bb80) when nBands == 4 and block mode
/// != 2. Each row is [ch0 band0..3, ch1 band0..3].
///
/// Only rate classes 0..3 are stored; a class-4 lookup reads the next symbol
/// (BitAllocOffset8DualMonoTable) in the linked image.
pub static OFFSET_4_DUAL_MONO: [[i8; 8]; 4] = [
    [-1, 0, 0, 0, -1, 0, 0, 0],
    [-2, -1, 0, 0, -2, -1, 0, 0],
    [0, 0, 1, 3, 0, 0, 1, 3],
    [0, 0, 1, 4, 0, 0, 1, 4],
];

/// BitAllocOffset8StereoTable - .rodata 0x20380, 64 B (4 x 16 i8).
///
/// Lookup is base + rate_index * OFFSET_8_STRIDE. Selected by
/// BitAllocLoudness_GetBitNeed_setup (@0x1ba04) and
/// BitAllocLoudness_GetBitNeed (@0x1bb80) when nBands == 8 and block mode
/// == 2 (stereo). Each row is [ch0 band0..7, ch1 band0..7].
///
/// Only rate classes 0..3 are stored. The vendor symbol is 64 B; a class-4
/// lookup runs into BitAllocOffset4DualMonoTable. See
/// OFFSET_8_STEREO_RATE4_OVERREAD for the exact bytes.
pub static OFFSET_8_STEREO: [[i8; 16]; 4] = [
    [-1, 0, 0, 0, 0, 0, 0, 0, -1, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [-3, -2, -1, 0, 1, 2, 1, 3, -2, -1, 0, 0, 0, 0, 1, 3],
    [0, 0, 0, 0, 2, 2, 4, 4, 0, 0, 2, 2, 4, 4, 6, 6],
];

/// Exact bytes read at rate index 4 by BitAllocLoudness_GetBitNeed
/// (@0x1bb80) for 8-band stereo: 8Stereo + 4 * 16 == .rodata 0x203c0, the
/// first two entries of BitAllocOffset4DualMonoTable. Not an ELF symbol;
/// kept separately so a bit-exact port can reproduce the over-read without
/// indexing past OFFSET_8_STEREO.
pub static OFFSET_8_STEREO_RATE4_OVERREAD: [i8; 16] =
    [-1, 0, 0, 0, -1, 0, 0, 0, -2, -1, 0, 0, -2, -1, 0, 0];

/// BitAllocOffset8DualMonoTable - .rodata 0x203e0, 80 B (5 x 16 i8).
///
/// Lookup is base + rate_index * OFFSET_8_STRIDE. Selected by
/// BitAllocLoudness_GetBitNeed_setup (@0x1ba04) and
/// BitAllocLoudness_GetBitNeed (@0x1bb80) when nBands == 8, block mode
/// != 2, and the tonal flag at allocation-context + 0x14 is zero.
/// Rate entry 4 is the clean 96 kHz normal table (same bytes as OFFSET_96K).
pub static OFFSET_8_DUAL_MONO: [[i8; 16]; 5] = [
    [-1, -1, 0, 0, 0, 0, 1, 2, -1, -1, 0, 0, 0, 0, 1, 2],
    [-1, 0, 0, 0, 0, 0, 2, 4, -1, 0, 0, 0, 0, 0, 2, 4],
    [0, 0, 0, 0, 0, 1, 3, 4, 0, 0, 0, 0, 0, 1, 3, 4],
    [0, 0, 0, 0, 0, 1, 2, 3, 0, 0, 0, 0, 0, 1, 2, 3],
    [0, 0, 1, 3, 4, 5, 5, 6, 0, 0, 1, 3, 4, 5, 5, 6],
];

/// BitAllocOffset8DualMonoToneTable - .rodata 0x20430, 80 B (5 x 16 i8).
///
/// Same selector as OFFSET_8_DUAL_MONO but with the tonal flag at
/// allocation-context + 0x14 non-zero. Rate entry 4 equals OFFSET_96K_TONE.
pub static OFFSET_8_DUAL_MONO_TONE: [[i8; 16]; 5] = [
    [-3, -2, 0, 0, 0, 1, 3, 4, -3, -2, 0, 0, 0, 1, 3, 4],
    [-3, -2, 0, 0, 0, 1, 3, 4, -3, -2, 0, 0, 0, 1, 3, 4],
    [-3, -2, 0, 0, 0, 1, 3, 4, -3, -2, 0, 0, 0, 1, 3, 4],
    [-3, -2, 0, 0, 0, 1, 3, 4, -3, -2, 0, 0, 0, 1, 3, 4],
    [-3, -2, -1, 0, 4, 5, 5, 6, -3, -2, -1, 0, 4, 5, 5, 6],
];

/// BitAllocOffset_96k_Table - .rodata 0x20480, 16 B (16 i8).
///
/// Used by BitAllocLoudness_GetBitAlloc_96k (@0x1c290) when the tonal flag
/// (allocation-context + 0x14, param_4[5]) is zero. Layout is
/// [ch0 band0..7, ch1 band0..7]. Equal to OFFSET_8_DUAL_MONO[4].
pub static OFFSET_96K: [i8; 16] = [0, 0, 1, 3, 4, 5, 5, 6, 0, 0, 1, 3, 4, 5, 5, 6];

/// BitAllocOffset_96k_ToneTable - .rodata 0x20490, 16 B (16 i8).
///
/// Used by BitAllocLoudness_GetBitAlloc_96k (@0x1c290) when the tonal flag
/// is non-zero. Layout is [ch0 band0..7, ch1 band0..7]. Equal to
/// OFFSET_8_DUAL_MONO_TONE[4].
pub static OFFSET_96K_TONE: [i8; 16] = [-3, -2, -1, 0, 4, 5, 5, 6, -3, -2, -1, 0, 4, 5, 5, 6];

// ---------------------------------------------------------------------------
// Max-width cap tables (int / i32)
// ---------------------------------------------------------------------------

/// nMaxBitsCfg0_250 - .rodata 0x20500, 32 B (8 x i32).
///
/// Member of the 4-band cap family, but aptXCompressorConfig0_
/// CalculateBitAllocation (@0x1d0a0) starts its 4-band ladder at 320. Kept
/// because it is the remaining 4-band variant in the same .rodata block.
pub static NMAX_4_250: [i32; 8] = [6, 5, 3, 2, 6, 5, 3, 2];

/// nMaxBitsCfg0_320 - .rodata 0x204e0, 32 B (8 x i32).
///
/// 4-band cap selected by aptXCompressorConfig0_CalculateBitAllocation
/// (@0x1d0a0) when compared bitrate < 320000.
pub static NMAX_4_320: [i32; 8] = [7, 6, 5, 4, 7, 6, 5, 4];

/// nMaxBitsCfg0_370 - .rodata 0x204c0, 32 B (8 x i32).
///
/// 4-band cap selected by aptXCompressorConfig0_CalculateBitAllocation
/// (@0x1d0a0) for 320000 <= bitrate < 370000.
pub static NMAX_4_370: [i32; 8] = [8, 7, 6, 5, 8, 7, 6, 5];

/// nMaxBitsCfg0_Max - .rodata 0x204a0, 32 B (8 x i32).
///
/// 4-band cap selected by aptXCompressorConfig0_CalculateBitAllocation
/// (@0x1d0a0) when bitrate >= 370000.
pub static NMAX_4_MAX: [i32; 8] = [9; 8];

/// nMaxBitsCfg0_8Band_250 - .rodata 0x20660, 64 B (16 x i32).
///
/// 8-band cap selected by aptXCompressorConfig0_CalculateBitAllocation
/// (@0x1d0a0) when bitrate < 250000.
pub static NMAX_8_250: [i32; 16] = [6, 5, 5, 4, 4, 4, 2, 2, 6, 5, 5, 4, 4, 4, 2, 2];

/// nMaxBitsCfg0_8Band_320 - .rodata 0x20620, 64 B (16 x i32).
///
/// 8-band cap selected by aptXCompressorConfig0_CalculateBitAllocation
/// (@0x1d0a0) for 250000 <= bitrate < 320000.
pub static NMAX_8_320: [i32; 16] = [7, 7, 6, 6, 5, 5, 4, 4, 7, 7, 6, 6, 5, 5, 4, 4];

/// nMaxBitsCfg0_8Band_370 - .rodata 0x205e0, 64 B (16 x i32).
///
/// 8-band cap selected by aptXCompressorConfig0_CalculateBitAllocation
/// (@0x1d0a0) for 320000 <= bitrate < 370000.
pub static NMAX_8_370: [i32; 16] = [8, 8, 7, 7, 6, 5, 4, 3, 8, 8, 7, 7, 6, 5, 4, 3];

/// nMaxBitsCfg0_8Band_420 - .rodata 0x205a0, 64 B (16 x i32).
///
/// 8-band cap selected by aptXCompressorConfig0_CalculateBitAllocation
/// (@0x1d0a0) for 370000 <= bitrate < 420000.
pub static NMAX_8_420: [i32; 16] = [9; 16];

/// nMaxBitsCfg0_8Band_520 - .rodata 0x20560, 64 B (16 x i32).
///
/// 8-band cap selected by aptXCompressorConfig0_CalculateBitAllocation
/// (@0x1d0a0) for 420000 <= bitrate < 520000.
pub static NMAX_8_520: [i32; 16] = [16, 16, 16, 16, 9, 9, 9, 9, 16, 16, 16, 16, 9, 9, 9, 9];

/// nMaxBitsCfg0_8Band_Max - .rodata 0x20520, 64 B (16 x i32).
///
/// 8-band cap selected by aptXCompressorConfig0_CalculateBitAllocation
/// (@0x1d0a0) when bitrate >= 520000.
pub static NMAX_8_MAX: [i32; 16] = [16; 16];

/// nMaxBitsCfg0_8Band_96k_420 - .rodata 0x207a0, 64 B (16 x i32).
///
/// 96 kHz cap selected by axBitAllocLoudnessEncode_Run_music_96k
/// (@0x1d384) when bitrate < 420000.
pub static NMAX_96K_420: [i32; 16] = [8, 7, 4, 2, 0, 0, 0, 0, 8, 7, 4, 2, 0, 0, 0, 0];

/// nMaxBitsCfg0_8Band_96k_500 - .rodata 0x20760, 64 B (16 x i32).
///
/// 96 kHz cap selected by axBitAllocLoudnessEncode_Run_music_96k
/// (@0x1d384) for 420000 <= bitrate < 500000.
pub static NMAX_96K_500: [i32; 16] = [9, 9, 9, 9, 2, 2, 2, 2, 9, 9, 9, 9, 2, 2, 2, 2];

/// nMaxBitsCfg0_8Band_96k_600 - .rodata 0x20720, 64 B (16 x i32).
///
/// 96 kHz cap selected by axBitAllocLoudnessEncode_Run_music_96k
/// (@0x1d384) for 500000 <= bitrate < 600000.
pub static NMAX_96K_600: [i32; 16] = [12, 10, 9, 9, 4, 4, 4, 4, 12, 10, 9, 9, 4, 4, 4, 4];

/// nMaxBitsCfg0_8Band_96k_650 - .rodata 0x206e0, 64 B (16 x i32).
///
/// 96 kHz cap selected by axBitAllocLoudnessEncode_Run_music_96k
/// (@0x1d384) for 600000 <= bitrate < 650000.
pub static NMAX_96K_650: [i32; 16] = [16, 13, 12, 12, 6, 6, 6, 6, 16, 13, 12, 12, 6, 6, 6, 6];

/// nMaxBitsCfg0_8Band_96k_Max - .rodata 0x206a0, 64 B (16 x i32).
///
/// 96 kHz cap selected by axBitAllocLoudnessEncode_Run_music_96k
/// (@0x1d384) when bitrate >= 650000.
pub static NMAX_96K_MAX: [i32; 16] = [16; 16];

// ---------------------------------------------------------------------------
// Minimum-width context arrays (int / i32)
// ---------------------------------------------------------------------------

/// Shared zero image of the two nMinBits locals. Both symbols are .bss:
/// aptXCompressorConfig0_CalculateBitAllocation.nMinBits at 0x39f58 and
/// axBitAllocLoudnessEncode_Run_music_96k.nMinBits at 0x39f98, 64 B each.
/// There are no initializer bytes in .rodata/.data and no store to either
/// symbol in the decompiled module, so every entry is zero.
///
/// TODO: if a runtime initializer is found outside the decompiled functions,
/// replace the two symbol-specific arrays below with the recovered values.
pub static MIN_WIDTHS_ZERO: [i32; 16] = [0; 16];

/// aptXCompressorConfig0_CalculateBitAllocation.nMinBits - .bss 0x39f58,
/// 64 B (16 x i32), all zero in this build.
///
/// aptXCompressorConfig0_CalculateBitAllocation (@0x1d0a0) stores this
/// array's address at allocation-context + 0x30 (param_4[0xc]);
/// BitAllocLoudness_GetDistribution (@0x1c020) reads it as the per-band
/// minimum width.
pub static N_MIN_BITS_CONFIG0: [i32; 16] = [0; 16];

/// axBitAllocLoudnessEncode_Run_music_96k.nMinBits - .bss 0x39f98,
/// 64 B (16 x i32), all zero in this build.
///
/// axBitAllocLoudnessEncode_Run_music_96k (@0x1d384) stores this array's
/// address at allocation-context + 0x30 (param_4[0xc]);
/// BitAllocLoudness_GetDistribution (@0x1c020) reads it as the per-band
/// minimum width.
pub static N_MIN_BITS_MUSIC_96K: [i32; 16] = [0; 16];

/// g_n8Perceptuallyweight_flat - .rodata 0x20358, 8 B (8 x i8).
///
/// Default offset vector written to allocation-context + 0x2c at the top of
/// BitAllocLoudness_GetBitNeed_setup (@0x1ba04), before the rate/mode table
/// is selected. All eight entries are zero in this build. The voice path is
/// the direct consumer; the active music path uses BitAllocLoudness_GetBitNeed.
pub static PERCEPTUAL_FLAT: [i8; 8] = [0; 8];

// ---------------------------------------------------------------------------
// Allocation-adjacent data not copied here
// ---------------------------------------------------------------------------
//
// Not referenced by the functions above; addresses/sizes recorded for a
// later extraction (all from the same CPH2749 module):
//
//   g_auBitAllocLoudnessCost  .data   0x392f0  40 B  = 10 x i32
//   g_aBitAllocModes0         .data   0x39318  76 B  (config struct array)
//   g_aBitAllocModes4_22      .data   0x39364  76 B  (config struct array)
//   g_aBitAllocModes4_24      .data   0x393b0  76 B  (config struct array)
//   g_aBitAllocModes          .data   0x39400  608 B (config struct array)
//   g_anMinBits_top_two       .data   0x39660  64 B  = 16 x i32
//   g_anMinBits_zero          .bss    0x39f18  64 B  = 16 x i32 (all zero)
//   g_nNumZeros_Music_8*      .data   0x39940..0x39a20 (zero-run models)
//   g_nMaxBitsCfg0_2_32k      .rodata 0x207e0  32 B  = 8 x i32 (voice)
//   g_nMaxBitsCfg0_2_24k      .rodata 0x20800  32 B  = 8 x i32 (voice)
//   g_nBitsCfg0_2_32k_FlatWhite .rodata 0x20820 32 B = 8 x i32 (voice)
//   g_nBitsCfg0_2_24k_FlatWhite_22 .rodata 0x20840 32 B = 8 x i32 (voice)
//   g_nBitsCfg0_2_24k_FlatWhite_24 .rodata 0x20860 32 B = 8 x i32 (voice)
//
// AxPackerCfg0_GetBitAllocation (@0x27908) has no static tables: it copies
// widths from the packer state at +0x14[channel] + 0x94 and maps 1 -> 2.

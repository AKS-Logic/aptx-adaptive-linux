//! 96 kHz Hi-Res producer scaffolding (feature 'hires-96k').
//!
//! The 48 kHz paths ([crate::frame], [crate::encoder]) are untouched: this
//! module is only compiled with the 'hires-96k' feature and nothing in the
//! shipping encoder calls it.  It packages what the shipped artifacts
//! actually prove about the 96 kHz row so a producer can be built and tested
//! without guessing, and it isolates the one thing that is *not* proven (the
//! 8-field allocation syntax) behind [write_alloc8].
//!
//! # What is verified
//!
//! * Rate bits - the A2DP capability body byte 6 uses bit 3 = 44.1 kHz,
//!   bit 4 = 48 kHz, bit 5 = 96 kHz and bit 6 = 192 kHz in Qualcomm ADK 6.4
//!   (repos/adk-aptx-adaptive/a2dp_codec_aptx_adaptive.c:57-61), which is the
//!   stack the QCC5181-class sinks (the Bose QC Ultra) run.  See [CapsRates]
//!   and analysis/192K-PRODUCER.md.
//! * Audio header - 3 rate bits plus a stereo bit: 48 kHz = 2, 88.2 kHz = 4,
//!   96 kHz = 6, 44.1 kHz = 0, and bit 3 set when the frame codes more than
//!   one channel block (AxPackerCfg04_BuildAudioHeader, ELF VA 0x16768).  The
//!   vendor 96 kHz capture carries 6 in every 328-byte frame.
//! * Container - 328-byte payload, frame type 3, length code 0x4f, 16-bit
//!   byte-swapped on the wire, an exp-Golomb continuation count, and the
//!   0x80/count/0xff pad trailer.  All of it is measured against
//!   tools/aptx-mode-vectors/captures/hires.bin by the tests.
//! * Geometry - 96 kHz, 8 ms, 768 samples, 96 blocks, 48 composer steps,
//!   8 fields, 656-byte payload (two 328-byte chunks).
//!
//! # What is not verified
//!
//! The 8-field allocation syntax.  [write_alloc8] mirrors the 16-field
//! writer's pair coding over the first four 8-band tables, but the vendor
//! capture cannot yet be decoded far enough to prove the pair order or the
//! escape behaviour, so this module never emits an allocation into a stream
//! that ships.  [CodecFrameWriter] takes widths from the caller; the
//! verifiable part is the container around them.
//!
//! SPDX-License-Identifier: MIT

use std::collections::VecDeque;

use crate::alloc_dif_tables::{HUFF_DIF_VAL, RANGE_DIF_VAL};
use crate::bitalloc_tables::{TABLE_A, TABLE_B};
use crate::composer::pair_bits;
use crate::entropy::BitWriter;

// ---------------------------------------------------------------------------
// Rates and the A2DP capability byte
// ---------------------------------------------------------------------------

/// 44.1 kHz, the audio header's zero rate code.
pub const RATE_44100: u32 = 44_100;
/// 48 kHz.
pub const RATE_48000: u32 = 48_000;
/// 88.2 kHz.
pub const RATE_88200: u32 = 88_200;
/// 96 kHz, the Hi-Res row this module targets.
pub const RATE_96000: u32 = 96_000;
/// 192 kHz; a capability bit only (no R2 encoder table or audio-header code).
pub const RATE_192000: u32 = 192_000;

/// Capability body byte 6, bit 3: 44.1 kHz.
pub const CAPS_SR_44100: u8 = 0x08;
/// Capability body byte 6, bit 4: 48 kHz.
pub const CAPS_SR_48000: u8 = 0x10;
/// Capability body byte 6, bit 5: 96 kHz.
pub const CAPS_SR_96000: u8 = 0x20;
/// Capability body byte 6, bit 6: 192 kHz.
pub const CAPS_SR_192000: u8 = 0x40;
/// The rate field of capability body byte 6 (bits 7..3).
pub const CAPS_SR_MASK: u8 = 0xf8;
/// The source-type / flag field of capability body byte 6 (bits 2..0).
pub const CAPS_SOURCE_TYPE_MASK: u8 = 0x07;
/// The sink-only flag some sinks set in byte 6 bit 0 (the Bose sets it).
pub const CAPS_SINK_FLAG: u8 = 0x01;

/// The rate bits a sink advertises (capability body byte 6).
///
/// The mapping is Qualcomm's, not this tree's: ADK 6.4
/// (repos/adk-aptx-adaptive/a2dp_codec_aptx_adaptive.c:57-61) defines
/// 1 << 3 = 44.1 kHz, 1 << 4 = 48 kHz, 1 << 5 = 96 kHz and 1 << 6 = 192 kHz,
/// and its parser (getSamplingRates, :106-123) tests them in the order 192,
/// 96, 48, 44.1.  AOSP's vendor header
/// (refs/a2dp_vendor_aptx_adaptive_constants.h) spells 96 kHz as 0xA0 (bit 5
/// plus the 0x80 modifier); bit 5 is the actual rate bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapsRates {
    /// The raw byte 6 value, rate and source-type bits together.
    pub raw: u8,
}

impl CapsRates {
    /// Decode capability body byte 6.
    pub const fn from_byte6(raw: u8) -> Self {
        Self { raw }
    }

    /// The rate field alone (bits 7..3).
    pub const fn rate_bits(&self) -> u8 {
        self.raw & CAPS_SR_MASK
    }

    /// True when the sink advertises the given rate.
    pub const fn has(&self, rate: u32) -> bool {
        let bit = match rate {
            RATE_44100 => CAPS_SR_44100,
            RATE_48000 => CAPS_SR_48000,
            RATE_96000 => CAPS_SR_96000,
            RATE_192000 => CAPS_SR_192000,
            _ => 0,
        };
        bit != 0 && (self.rate_bits() & bit) != 0
    }

    /// Every advertised rate, low to high.
    pub fn rates(&self) -> Vec<u32> {
        [RATE_44100, RATE_48000, RATE_96000, RATE_192000]
            .into_iter()
            .filter(|&r| self.has(r))
            .collect()
    }

    /// The first rate of 'preference' the sink advertises.
    pub fn pick(&self, preference: &[u32]) -> Option<u32> {
        preference.iter().copied().find(|&r| self.has(r))
    }

    /// True when the sink sets the sink-only flag (byte 6 bit 0).
    pub const fn sink_flag(&self) -> bool {
        (self.raw & CAPS_SINK_FLAG) != 0
    }

    /// The source-type field (bits 2..0), echoed by a source in its own element.
    pub const fn source_type(&self) -> u8 {
        self.raw & CAPS_SOURCE_TYPE_MASK
    }
}

// ---------------------------------------------------------------------------
// Audio header and the 96 kHz geometry
// ---------------------------------------------------------------------------

/// The 3-bit rate code of the 4-bit audio header.
///
/// AxPackerCfg04_BuildAudioHeader (ELF VA 0x16768,
/// r2-encoder-full-decomp.c:17223-17242) maps 48 kHz to 2, 88.2 kHz to 4 and
/// 96 kHz to 6; every other rate (44.1 kHz included) takes 0.  There is no
/// 192 kHz code: bit 3 is the stereo flag, so the 4-bit header cannot express
/// 192 kHz at all.
pub const fn rate_code(rate: u32) -> u8 {
    match rate {
        RATE_48000 => 0x2,
        RATE_88200 => 0x4,
        RATE_96000 => 0x6,
        _ => 0x0,
    }
}

/// The 4-bit audio header for a rate and a channel-block count.
///
/// Bit 3 is set only when the frame codes more than one channel block.  The
/// vendor 96 kHz capture uses one 8-field channel block per 328-byte frame, so
/// its header is 6; a hypothetical single 656-byte 96 kHz stereo frame would
/// carry 0xe.  The decomp sets the bit for more than one channel
/// (*(char *)(state + 0x2a4) > 1).
pub const fn audio_header(rate: u32, channel_blocks: usize) -> u8 {
    rate_code(rate) | if channel_blocks > 1 { 0x8 } else { 0x0 }
}

/// Codec payload bytes of one container chunk (one 328-byte frame).
pub const FRAME_BYTES: usize = 328;
/// Bitstream bytes in a chunk: the payload minus the 3-byte module header.
pub const FRAME_STREAM_BYTES: usize = FRAME_BYTES - 3;
/// Codec frame type byte of the 96 kHz 8-band form (config 3).
pub const FRAME_TYPE: u8 = 3;
/// Length code of a 328-byte frame: 328 / 4 - 3.
pub const LENGTH_CODE: u8 = 0x4f;
/// Codec payload bytes of one Hi-Res period (two 328-byte chunks).
pub const PAYLOAD_BYTES: usize = 656;
/// CMFB blocks per codec frame at 96 kHz / 8 ms (768 samples / 8).
pub const BLOCKS_PER_FRAME: usize = 96;
/// Composer steps per codec frame (two blocks per step).
pub const STEPS_PER_FRAME: usize = 48;
/// PCM samples per channel per codec frame.
pub const SAMPLES_PER_FRAME: usize = 768;
/// Frame period in milliseconds.
pub const PERIOD_MS: u32 = 8;
/// Source-type-0 RTP step for one 8 ms payload.
pub const RTP_STEP_US: u32 = 8_000;
/// Container TTP units per millisecond (1/15000 s).
pub const TTP_UNITS_PER_MS: u16 = 15;
/// Container TTP step of one 8 ms payload (15 units/ms x 8 ms).
pub const TTP_STEP_UNITS: u16 = 120;
/// The sample count a source-type-2 RTP timestamp carries (low 16 bits).
pub const RTP_TIMESTAMP_SAMPLES: u16 = 768;

/// Swap 16-bit units in place (the OTA byte order of every aptX Adaptive
/// frame; frame.rs and decode.rs use the same rule).
pub fn swap16_in_place(buf: &mut [u8]) {
    let mut i = 0;
    while i + 1 < buf.len() {
        buf.swap(i, i + 1);
        i += 2;
    }
}

// ---------------------------------------------------------------------------
// The continuation prefix
// ---------------------------------------------------------------------------

/// Bits an exp-Golomb order-0 code for 'carried' takes.
pub const fn continuation_bits(carried: usize) -> usize {
    let v = carried + 1;
    let mut bits = 0usize;
    while (1usize << bits) <= v {
        bits += 1;
    }
    (bits - 1) + bits
}

/// Write the continuation prefix: the exp-Golomb order-0 code of
/// carried + 1, i.e. 'carried' zero bits, then the value in
/// log2(carried + 1) + 1 bits.
///
/// This reproduces the capture: carried = 0 is the single bit 1, and
/// carried = 11 is 000 1100 (7 bits), exactly the 0x19 prefix of hires.bin
/// record 1 frame A.
pub fn write_continuation(w: &mut BitWriter, carried: usize) {
    let v = carried + 1;
    let mut bits = 0usize;
    while (1usize << bits) <= v {
        bits += 1;
    }
    for _ in 0..bits - 1 {
        w.write(0, 1);
    }
    w.write(v as u32, bits as u32);
}

/// Read the continuation prefix at the start of a chunk's bitstream.
///
/// Returns (carried_bytes, bit_offset_after_prefix).
pub fn read_continuation(stream: &[u8]) -> Option<(usize, usize)> {
    let mut bit = 0usize;
    let mut zeros = 0usize;
    loop {
        let byte = *stream.get(bit / 8)?;
        let b = (byte >> (7 - bit % 8)) & 1;
        bit += 1;
        if b == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        } else {
            break;
        }
    }
    let mut value = 1usize << zeros;
    for k in 0..zeros {
        let byte = *stream.get(bit / 8)?;
        let b = (byte >> (7 - bit % 8)) & 1;
        bit += 1;
        value |= usize::from(b) << (zeros - 1 - k);
    }
    Some((value - 1, bit))
}

fn read_bits(bytes: &[u8], bit: usize, n: usize) -> Option<u32> {
    let mut v = 0u32;
    for i in 0..n {
        let at = bit + i;
        let byte = *bytes.get(at / 8)?;
        v = (v << 1) | u32::from((byte >> (7 - at % 8)) & 1);
    }
    Some(v)
}

// ---------------------------------------------------------------------------
// 8-field allocation (UNVERIFIED syntax)
// ---------------------------------------------------------------------------

/// One composer step of the 8-field (one channel block) form.
pub type StepVals8 = [[i32; 2]; 8];

/// The pair-table order used for the four allocation pairs.
///
/// false selects [TABLE_A], true [TABLE_B].  The 16-field writer uses
/// [A, A, B, B] for each channel block; whether the 8-field form keeps that
/// order is the open question this module documents rather than assumes.
pub const PAIR_TABLES_8: [bool; 4] = [false, false, true, true];

/// Write an 8-field absolute allocation (the config-3 form: escapes on).
///
/// **Unverified.**  Mirrors [crate::frame::write_alloc16] over four pairs: the
/// coded width is width - 1 (0 for widths 0 and 1), the pair table is selected
/// by 'pair_tables', a coded nibble of 8 escapes to three extension bits, and
/// an all-zero allocation writes an 8-bit trailer.
pub fn write_alloc8(widths: &[u8; 8], w: &mut BitWriter) {
    write_alloc8_cfg(widths, PAIR_TABLES_8, true, 0, w);
}

/// [write_alloc8] with the pair order, the escapes and the trailer
/// selectable.
pub fn write_alloc8_cfg(
    widths: &[u8; 8],
    pair_tables: [bool; 4],
    escapes: bool,
    trailer: u8,
    w: &mut BitWriter,
) {
    let coded = |f: usize| -> u32 {
        if widths[f] >= 2 {
            u32::from(widths[f] - 1)
        } else {
            0
        }
    };
    let mut sum = 0u32;
    for p in 0..4 {
        let (ca, cb) = (coded(2 * p), coded(2 * p + 1));
        sum += ca + cb;
        let (na, nb) = (ca.min(8), cb.min(8));
        let table = if pair_tables[p] { &TABLE_B } else { &TABLE_A };
        let (code, len) = table[((na << 4) | nb) as usize];
        w.write(u32::from(code), u32::from(len));
        if !escapes {
            continue;
        }
        let mut ext = 0u32;
        let mut nbits = 0u32;
        if ca > 7 {
            ext = (ca - 8) & 7;
            nbits = 3;
        }
        if cb > 7 {
            ext = ((cb - 8) & 7) | (ext << 3);
            nbits += 3;
        }
        if nbits > 0 {
            w.write(ext, nbits);
        }
    }
    if escapes && sum == 0 {
        w.write(u32::from(trailer), 8);
    }
}

/// Write an 8-field differential allocation update.
///
/// **Unverified** in the same sense as [write_alloc8]; the Huffman range
/// tables and the below-min/above-max extension groups are the same ones the
/// validated 16-field writer uses.
pub fn write_alloc_dif8(prev_coded: &mut [u8; 8], widths: &[u8; 8], w: &mut BitWriter) {
    for f in 0..8 {
        let coded = if widths[f] >= 2 { widths[f] - 1 } else { 0 };
        let prev = prev_coded[f].min(15) as usize;
        let (mn, mx) = RANGE_DIF_VAL[prev];
        let newc = i32::from(coded);
        let below = newc <= mn;
        let above = newc >= mx;
        let mut clamped = newc;
        if clamped < mn {
            clamped = mn;
        }
        if clamped > mx {
            clamped = mx;
        }
        let idx = ((clamped - prev as i32) & 0xf) as usize;
        let (code, len) = HUFF_DIF_VAL[prev][idx];
        w.write(u32::from(code), u32::from(len));
        if below {
            w.write(((mn - newc) & 7) as u32, 3);
        } else if above {
            w.write(((newc - mx) & 7) as u32, 3);
        }
        prev_coded[f] = coded;
    }
}

/// Widths for the two channel blocks of one 96 kHz period, from band energy.
///
/// This calls the ported vendor 96 kHz runner
/// (axBitAllocLoudnessEncode_Run_music_96k, ELF VA 0xd384;
/// [crate::bitalloc_vendor::alloc_96k]), which walks two independent 8-field
/// dual-mono blocks with half the frame pool each.  The allocation is the
/// vendored part; the frame that would carry it is the unverified part.
///
/// 'energy' is channel-major (channel * 8 + band), 'calls' the blocks per
/// channel (96) and 'stream_bits' the composer budget.
pub fn widths_96k(energy: &[f64; 16], calls: usize, stream_bits: usize) -> ([u8; 8], [u8; 8]) {
    let row = crate::bitalloc_vendor::alloc_96k(energy, calls, stream_bits);
    let mut left = [0u8; 8];
    let mut right = [0u8; 8];
    left.copy_from_slice(&row[..8]);
    right.copy_from_slice(&row[8..]);
    (left, right)
}

// ---------------------------------------------------------------------------
// Codec frame writer
// ---------------------------------------------------------------------------

/// One 96 kHz codec frame (bitstream only, no container).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecFrame {
    /// The bitstream bytes, byte-aligned.
    pub bits: Vec<u8>,
    /// Composer steps actually written.
    pub steps_written: usize,
}

/// Writes one 8-field codec frame: header, allocation, composer steps and
/// differential updates, fitted into a bit budget.
pub struct CodecFrameWriter {
    /// Previous coded widths for the differential update.
    pub coded: [u8; 8],
    /// The 4-bit audio header (6 for 96 kHz mono/dual-mono).
    pub header: u8,
    /// Pair-table order for the allocation.
    pub pair_tables: [bool; 4],
    /// Frames written.
    pub frames: u64,
    /// Stream bit position where the first step starts.
    pub last_head_end: usize,
}

impl Default for CodecFrameWriter {
    fn default() -> Self {
        Self {
            coded: [0; 8],
            header: audio_header(RATE_96000, 1),
            pair_tables: PAIR_TABLES_8,
            frames: 0,
            last_head_end: 0,
        }
    }
}

impl CodecFrameWriter {
    /// A writer for the vendor 96 kHz shape (header 6, [A, A, B, B]).
    pub fn new() -> Self {
        Self::default()
    }

    /// Write a codec frame, keeping only the steps that fit 'budget_bits'.
    ///
    /// Steps are written whole: a partially written pair desynchronises every
    /// decoder, so the tail is dropped (the encoder refits, as the 48 kHz path
    /// does) rather than cut.
    pub fn write(
        &mut self,
        widths: &[u8; 8],
        desc: &[u8; 8],
        steps: &[StepVals8],
        dif_after: &[usize],
        budget_bits: usize,
    ) -> CodecFrame {
        let mut w = BitWriter::new();
        w.write(u32::from(self.header), 4);
        write_alloc8_cfg(widths, self.pair_tables, true, 0, &mut w);
        self.last_head_end = w.len_bits();
        for f in 0..8 {
            self.coded[f] = if widths[f] >= 2 { widths[f] - 1 } else { 0 };
        }

        let mut pending: Vec<Vec<Vec<bool>>> = Vec::with_capacity(steps.len());
        let mut used = w.len_bits();
        let mut keep = 0usize;
        for (i, s) in steps.iter().enumerate() {
            let mut step_bits = Vec::with_capacity(8);
            let mut bits = 0usize;
            for f in 0..8 {
                if desc[f] == 0 {
                    step_bits.push(Vec::new());
                    continue;
                }
                let pair = pair_bits(
                    desc[f] as usize,
                    (s[f][0] as u32) & 0xffff,
                    (s[f][1] as u32) & 0xffff,
                );
                bits += pair.len();
                step_bits.push(pair);
            }
            let dif = dif_after.contains(&i) && i + 1 < steps.len();
            let extra = bits + if dif { 16 } else { 0 };
            if i > 0 && used + extra > budget_bits {
                break;
            }
            used += extra;
            pending.push(step_bits);
            keep = i + 1;
        }
        for (i, step) in pending.iter().enumerate() {
            for pair in step {
                for bit in pair {
                    w.write(u32::from(*bit), 1);
                }
            }
            if dif_after.contains(&i) && i + 1 < keep {
                write_alloc_dif8(&mut self.coded, widths, &mut w);
            }
        }
        self.frames += 1;
        CodecFrame {
            bits: w.into_bytes(),
            steps_written: keep,
        }
    }
}

// ---------------------------------------------------------------------------
// The 328-byte container
// ---------------------------------------------------------------------------

/// One 328-byte OTA chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// The 328 wire bytes (16-bit byte-swapped).
    pub ota: [u8; FRAME_BYTES],
    /// Carried bytes from the previous codec frame placed at the start.
    pub carried_in: usize,
    /// Codec-frame bytes placed in this chunk.
    pub stream_bytes: usize,
    /// Pad trailer bytes (0 when the chunk is full).
    pub pad_bytes: usize,
    /// Bytes of the frame that did not fit (the next chunk's carry).
    pub carry_out: Vec<u8>,
}

/// Wrap a codec frame's bytes in the 328-byte container.
///
/// Layout (measured on hires.bin): [00][03|pad][4f] followed by the exp-Golomb
/// continuation prefix, the carried bytes of the previous codec frame (if
/// any), the codec frame's bits, and - when there is room - the pad trailer
/// (0x80 marker, zero fill, byte count, 0xff fill, exactly
/// [crate::frame::FrameWriter]'s scheme).
///
/// 'carry' is empty for a self-contained frame, which is what the producer
/// should emit: pad the codec frame into the 325-byte budget rather than spill
/// into the next chunk.  (The vendor capture does spill; see
/// analysis/192K-PRODUCER.md for why the spill's byte alignment is not
/// reproduced here.)
pub fn wrap_chunk(frame: &[u8], carry: &[u8]) -> Chunk {
    assert!(carry.len() + 4 < FRAME_STREAM_BYTES, "carry too large");
    let prefix_bits = continuation_bits(carry.len());
    let prefix_bytes = prefix_bits.div_ceil(8);
    let capacity_bytes = FRAME_STREAM_BYTES - prefix_bytes - carry.len();
    let fit = frame.len().min(capacity_bytes);
    let carry_out = frame[fit..].to_vec();

    let mut w = BitWriter::new();
    write_continuation(&mut w, carry.len());
    for &b in carry {
        w.write(u32::from(b), 8);
    }
    for &b in &frame[..fit] {
        w.write(u32::from(b), 8);
    }
    let content_bytes = w.len_bits().div_ceil(8).min(FRAME_STREAM_BYTES);
    assert!(w.pad_to(content_bytes), "bit writer overflow");
    let pad_bytes = FRAME_STREAM_BYTES - content_bytes;

    let mut raw = vec![0u8; FRAME_BYTES];
    raw[0] = 0x00;
    raw[1] = FRAME_TYPE;
    raw[2] = LENGTH_CODE;
    raw[3..3 + content_bytes].copy_from_slice(w.bytes());
    if pad_bytes > 0 {
        raw[1] |= 0x80;
        let pad = &mut raw[3 + content_bytes..];
        pad.fill(0);
        pad[0] = 0x80;
        let t = pad.len() - 1;
        let ff = t / 255;
        let at = pad.len() - 1 - ff;
        pad[at + 1..].fill(0xFF);
        pad[at] = (t % 255) as u8;
    }
    let mut ota = [0u8; FRAME_BYTES];
    ota.copy_from_slice(&raw);
    swap16_in_place(&mut ota);
    Chunk {
        ota,
        carried_in: carry.len(),
        stream_bytes: fit,
        pad_bytes,
        carry_out,
    }
}

/// A parsed 328-byte chunk (container level only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkInfo {
    /// Frame type byte without the pad flag.
    pub frame_type: u8,
    /// True when the pad flag is set.
    pub padded: bool,
    /// Carried bytes announced by the continuation prefix.
    pub carried: usize,
    /// Bit offset of the 4-bit audio header within the stream bytes.
    pub header_bit: usize,
    /// The 4-bit audio header.
    pub header: u8,
}

/// Parse the container of a 328-byte OTA chunk.
///
/// Validates the length code and frame type, then reads the continuation
/// prefix and the audio header at the position it implies.  This is the
/// verifiable part of a 96 kHz frame; the allocation and composer after the
/// header need the 8-field syntax.
pub fn parse_chunk(ota: &[u8]) -> Option<ChunkInfo> {
    if ota.len() != FRAME_BYTES {
        return None;
    }
    let mut raw = ota.to_vec();
    swap16_in_place(&mut raw);
    if raw[0] != 0 || raw[2] != LENGTH_CODE {
        return None;
    }
    let frame_type = raw[1] & 0x7f;
    let padded = raw[1] & 0x80 != 0;
    let mut body = &raw[3..];
    if padded {
        let mut ff = 0usize;
        for &b in body.iter().rev() {
            if b == 0xFF {
                ff += 1;
            } else {
                break;
            }
        }
        let at = body.len().checked_sub(1 + ff)?;
        let t = 255 * ff + usize::from(body[at]);
        let pad = t + 1;
        if pad > body.len() {
            return None;
        }
        body = &body[..body.len() - pad];
    }
    let (carried, prefix_bits) = read_continuation(body)?;
    let header_bit = prefix_bits + 8 * carried;
    let header = read_bits(body, header_bit, 4)? as u8;
    Some(ChunkInfo {
        frame_type,
        padded,
        carried,
        header_bit,
        header,
    })
}

/// Assemble the 656-byte payload of one Hi-Res period from two chunks.
pub fn assemble_payload(a: &[u8; FRAME_BYTES], b: &[u8; FRAME_BYTES]) -> [u8; PAYLOAD_BYTES] {
    let mut out = [0u8; PAYLOAD_BYTES];
    out[..FRAME_BYTES].copy_from_slice(a);
    out[FRAME_BYTES..].copy_from_slice(b);
    out
}

// ---------------------------------------------------------------------------
// Byte-stream chopper (experimental)
// ---------------------------------------------------------------------------

/// Chops a continuous byte stream into 328-byte chunks with continuation
/// prefixes.
///
/// This reproduces the vendor capture's chunking (C = bytes of the codec frame
/// in progress at the chunk start).  It is **not** used by [wrap_chunk]'s
/// shipping path: the producer pads each codec frame into one chunk so C is
/// always 0.  The chopper exists so the continuation logic can be tested.
#[derive(Default)]
pub struct Chopper {
    queue: VecDeque<u8>,
    frames: VecDeque<(usize, usize)>,
    carry: usize,
}

impl Chopper {
    /// A new empty chopper.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one codec frame (byte-aligned).
    pub fn push_frame(&mut self, frame: &[u8]) {
        assert!(!frame.is_empty(), "empty codec frame");
        self.queue.extend(frame.iter().copied());
        self.frames.push_back((frame.len(), frame.len()));
    }

    /// Bytes not yet chopped.
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    /// True when a chunk can be emitted.
    pub fn has_chunk(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Emit the next chunk, or None when the queue is empty.
    pub fn next_chunk(&mut self) -> Option<Chunk> {
        if self.queue.is_empty() {
            return None;
        }
        let mut carried = self.carry;
        while carried + continuation_bits(carried).div_ceil(8) >= FRAME_STREAM_BYTES {
            carried -= 1;
        }
        let prefix_bytes = continuation_bits(carried).div_ceil(8);
        let mut w = BitWriter::new();
        write_continuation(&mut w, carried);
        for _ in 0..carried {
            let b = self.queue.pop_front()?;
            w.write(u32::from(b), 8);
            self.consume_front();
        }
        let free = FRAME_STREAM_BYTES - prefix_bytes - carried;
        let mut written = 0usize;
        while written < free {
            let Some(b) = self.queue.pop_front() else {
                break;
            };
            w.write(u32::from(b), 8);
            written += 1;
            self.consume_front();
        }
        self.carry = match self.frames.front() {
            Some(&(rem, total)) if rem < total => rem,
            _ => 0,
        };
        let content_bytes = w.len_bits().div_ceil(8);
        let pad_bytes = FRAME_STREAM_BYTES - content_bytes;
        assert!(w.pad_to(content_bytes));
        let mut raw = vec![0u8; FRAME_BYTES];
        raw[0] = 0x00;
        raw[1] = FRAME_TYPE;
        raw[2] = LENGTH_CODE;
        raw[3..3 + content_bytes].copy_from_slice(w.bytes());
        if pad_bytes > 0 {
            raw[1] |= 0x80;
            let pad = &mut raw[3 + content_bytes..];
            pad.fill(0);
            pad[0] = 0x80;
            let t = pad.len() - 1;
            let ff = t / 255;
            let at = pad.len() - 1 - ff;
            pad[at + 1..].fill(0xFF);
            pad[at] = (t % 255) as u8;
        }
        let mut ota = [0u8; FRAME_BYTES];
        ota.copy_from_slice(&raw);
        swap16_in_place(&mut ota);
        Some(Chunk {
            ota,
            carried_in: carried,
            stream_bytes: written,
            pad_bytes,
            carry_out: Vec::new(),
        })
    }

    fn consume_front(&mut self) {
        if let Some((rem, _)) = self.frames.front_mut() {
            *rem -= 1;
            if *rem == 0 {
                self.frames.pop_front();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_byte_matches_adk() {
        let bose = CapsRates::from_byte6(0x51);
        assert_eq!(bose.rates(), vec![RATE_48000, RATE_192000]);
        assert!(bose.sink_flag());
        assert_eq!(bose.source_type(), 0x01);
        assert_eq!(
            CapsRates::from_byte6(0x60).rates(),
            vec![RATE_96000, RATE_192000]
        );
        assert!(CapsRates::from_byte6(0x20).has(RATE_96000));
        assert!(!CapsRates::from_byte6(0x10).has(RATE_96000));
        assert_eq!(
            CapsRates::from_byte6(0x51).pick(&[RATE_96000, RATE_48000]),
            Some(RATE_48000)
        );
    }

    #[test]
    fn audio_header_rule_matches_decomp() {
        assert_eq!(audio_header(RATE_44100, 1), 0x0);
        assert_eq!(audio_header(RATE_48000, 1), 0x2);
        assert_eq!(audio_header(RATE_48000, 2), 0xa);
        assert_eq!(audio_header(RATE_88200, 1), 0x4);
        assert_eq!(audio_header(RATE_96000, 1), 0x6);
        assert_eq!(audio_header(RATE_96000, 2), 0xe);
    }

    #[test]
    fn continuation_code_round_trip() {
        for carried in 0..64usize {
            let mut w = BitWriter::new();
            write_continuation(&mut w, carried);
            assert_eq!(w.len_bits(), continuation_bits(carried));
            let bytes = w.into_bytes();
            assert_eq!(
                read_continuation(&bytes),
                Some((carried, continuation_bits(carried)))
            );
        }
        // 11 -> value 12 -> 000 1100 (7 bits); the capture 0x19 is those
        // bits plus the MSB of the first carried byte.
        let mut w = BitWriter::new();
        write_continuation(&mut w, 11);
        assert_eq!(w.len_bits(), 7);
        assert_eq!(w.bytes()[0] >> 1, 0b000_1100, "capture prefix bits");
    }

    #[test]
    fn pad_trailer_matches_container_scheme() {
        // 323 frame bytes + a 1-bit prefix leave exactly one pad byte, which
        // is the count 0 - the same 0x00 byte hires.bin record 10 frame A
        // carries.
        let chunk = wrap_chunk(&[0u8; 323], &[]);
        assert_eq!(chunk.pad_bytes, 1);
        let mut raw = chunk.ota;
        swap16_in_place(&mut raw);
        assert_ne!(raw[1] & 0x80, 0);
        assert_eq!(*raw.last().unwrap(), 0x00, "one-byte pad is count 0");
    }

    #[test]
    fn chunk_round_trip_with_carry() {
        let mut frame: Vec<u8> = (0u8..200).collect();
        frame[0] = 0x60; // the codec frame starts with the 0110 header
        let carry = vec![0xa5u8; 11];
        let chunk = wrap_chunk(&frame, &carry);
        assert!(chunk.carry_out.is_empty());
        let info = parse_chunk(&chunk.ota).expect("parse");
        assert_eq!(info.carried, 11);
        assert_eq!(info.header_bit, continuation_bits(11) + 88);
        assert_eq!(info.frame_type, FRAME_TYPE);
        assert_eq!(info.header, audio_header(RATE_96000, 1));
    }

    #[test]
    fn chunk_wraps_byte_swap_and_header() {
        let chunk = wrap_chunk(&[0x60, 0xad, 0xbe, 0xef], &[]);
        // wire order: [03|pad][00][first stream byte][4f], the same shape as
        // the capture 03 00 b1 4f
        assert_eq!(chunk.ota[0], 0x83);
        assert_eq!(chunk.ota[1], 0x00);
        assert_eq!(chunk.ota[3], 0x4f);
        let mut raw = chunk.ota;
        swap16_in_place(&mut raw);
        assert_eq!(raw[0], 0x00);
        assert_eq!(raw[1] & 0x7f, 0x03);
        assert_eq!(raw[2], LENGTH_CODE);
        assert_eq!(raw[3] >> 7, 1, "continuation 0");
        assert_eq!((raw[3] >> 3) & 0xf, 6, "96 kHz header at bit 1");
        let mut again = chunk.ota;
        swap16_in_place(&mut again);
        assert_eq!(again, raw, "swap is its own inverse");
    }

    #[test]
    fn payload_is_two_chunks() {
        let a = wrap_chunk(&[0x11; 300], &[]);
        let b = wrap_chunk(&[0x22; 300], &[]);
        let payload = assemble_payload(&a.ota, &b.ota);
        assert_eq!(payload.len(), PAYLOAD_BYTES);
        assert_eq!(&payload[..FRAME_BYTES], &a.ota[..]);
        assert_eq!(&payload[FRAME_BYTES..], &b.ota[..]);
    }

    #[test]
    fn codec_frame_header_and_steps() {
        let widths = [6u8, 4, 3, 2, 2, 0, 0, 0];
        let mut desc = [0u8; 8];
        for f in 0..8 {
            desc[f] = widths[f];
        }
        let steps: Vec<StepVals8> = (0..STEPS_PER_FRAME)
            .map(|i| {
                let mut s = [[0i32; 2]; 8];
                for f in 0..5 {
                    s[f] = [i as i32 * 3 + f as i32, i as i32 * 5 - f as i32];
                }
                s
            })
            .collect();
        let mut fw = CodecFrameWriter::new();
        let frame = fw.write(&widths, &desc, &steps, &[31], FRAME_STREAM_BYTES * 8);
        assert!(frame.steps_written > 0);
        let chunk = wrap_chunk(&frame.bits, &[]);
        let info = parse_chunk(&chunk.ota).expect("parse");
        assert_eq!(info.carried, 0);
        assert_eq!(info.header, 0x6);
        assert_eq!(info.header_bit, 1);
    }

    #[test]
    fn allocation_is_four_pairs() {
        let widths = [6u8, 4, 3, 2, 2, 0, 0, 0];
        let mut w = BitWriter::new();
        write_alloc8(&widths, &mut w);
        assert!(w.len_bits() >= 4 * 4 && w.len_bits() < 4 * 19 + 8);
        let mut no_esc = BitWriter::new();
        write_alloc8_cfg(&[0u8; 8], PAIR_TABLES_8, false, 0, &mut no_esc);
        let mut esc = BitWriter::new();
        write_alloc8_cfg(&[0u8; 8], PAIR_TABLES_8, true, 0, &mut esc);
        assert_eq!(
            esc.len_bits(),
            no_esc.len_bits() + 8,
            "zero allocation writes the 8-bit trailer"
        );
    }

    #[test]
    fn chopper_carries_and_reassembles() {
        let frames: Vec<Vec<u8>> = vec![vec![1u8; 300], vec![2u8; 300], vec![3u8; 100]];
        let mut ch = Chopper::new();
        for f in &frames {
            ch.push_frame(f);
        }
        let mut got: Vec<u8> = Vec::new();
        let mut chunks = 0;
        while let Some(chunk) = ch.next_chunk() {
            chunks += 1;
            let mut raw = chunk.ota;
            swap16_in_place(&mut raw);
            let mut body = &raw[3..];
            if raw[1] & 0x80 != 0 {
                let mut ff = 0;
                for &b in body.iter().rev() {
                    if b == 0xFF {
                        ff += 1;
                    } else {
                        break;
                    }
                }
                let at = body.len() - 1 - ff;
                let t = 255 * ff + usize::from(body[at]);
                body = &body[..body.len() - (t + 1)];
            }
            let (carried, bit) = read_continuation(body).expect("prefix");
            assert_eq!(carried, chunk.carried_in);
            for k in 0..carried {
                got.push(read_bits(body, bit + 8 * k, 8).unwrap() as u8);
            }
            for k in 0..chunk.stream_bytes {
                got.push(read_bits(body, bit + 8 * (carried + k), 8).unwrap() as u8);
            }
        }
        let want: Vec<u8> = frames.concat();
        assert_eq!(chunks, 3);
        assert_eq!(got, want, "chopper reassembly");
    }
}

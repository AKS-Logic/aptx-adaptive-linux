//! Block-count parser for one aptX Adaptive R2 frame (config 3 "HQ" or
//! config 4 "LL").
//!
//! Two payload shapes are accepted:
//!
//!   R2 HQ, 656 bytes (config 3):  [00][03 | 0x80 pad][a1][bitstream ...]
//!   R2 LL, 348 bytes (config 4):  [00][04 | 0x80 pad][54][bitstream ...]
//!
//! and each has two presentations:
//!
//!   raw:  as above
//!   OTA:  raw with every adjacent byte pair swapped
//!
//! The vendor capture records (664 bytes) are the OTA form preceded by an
//! 8-byte SPF container; the native encoder's vendor-frame Packet::payload
//! is the OTA form alone.  parse_frame() accepts either presentation and
//! reports which one it saw.
//!
//! # Frame geometry
//!
//! The bitstream starts with an exp-Golomb count of bytes carried over from
//! the previous frame (always zero for the vendor captures), then a sequence
//! of chunks.  Each chunk carries an allocation -- absolute for the first
//! chunk, differential afterwards -- followed by composer pairs.  One
//! composer pair codes two blocks, and a chunk holds at most 64 blocks.  The
//! last chunk is ended by the frame's pad marker or by the end of the
//! bitstream, so its size can be anything even (the captures use
//! 128 = [64, 64] and 160 = [64, 64, 32]).
//!
//! A block is 8 samples per channel, so total_blocks * 8 is the frame's
//! audio duration in samples at the stream rate.  The plugin currently uses
//! that to advance the source-type-0 RTP timestamp by the frame's real
//! duration instead of a fixed step.  The LL row is 60 blocks = 480
//! samples = 10 ms at 48 kHz.
//!
//! Config 4 (the 348-byte LL payload) codes the allocation without the
//! config-3 escapes: nibble 8 means width 9 with no 3-bit extension, a
//! differential update carries no range extensions, and there is no
//! zero-allocation escape.  The 348-byte LL row therefore uses one chunk of
//! 60 blocks (below the 64-block cap), and its frame type byte is 4.
//!
//! # Provenance
//!
//! The parser is a port of the clean-room decoder in
//! audio/aptx-linux/crates/aptx-adaptive/src/codec/decode.rs, with the
//! allocation, differential-allocation and composer tables that already
//! live in this crate (composer_tables, bitalloc_tables, alloc_dif_tables).
//! It stops at the block geometry: the quantiser values themselves are
//! repeatedly decoded only to advance the bit position.
//!
//! SPDX-License-Identifier: MIT

use std::fmt;

// The tables are included through #[path] for the module's own tests so that
// rustc --test src/frame_blocks.rs can build this file on its own, before the
// crate root declares mod frame_blocks.  A normal crate build uses the
// crate-level modules instead (no duplicate tables).
#[cfg(test)]
#[path = "alloc_dif_tables.rs"]
#[allow(dead_code, non_upper_case_globals)]
mod alloc_dif_tables;
#[cfg(test)]
#[path = "bitalloc_tables.rs"]
#[allow(dead_code, non_upper_case_globals)]
mod bitalloc_tables;
#[cfg(test)]
#[path = "composer_tables.rs"]
#[allow(dead_code, non_upper_case_globals)]
mod composer_tables;

#[cfg(test)]
use self::alloc_dif_tables::{HUFF_DIF_VAL, RANGE_DIF_VAL};
#[cfg(test)]
use self::bitalloc_tables::{TABLE_A, TABLE_B};
#[cfg(test)]
use self::composer_tables::{COMPOSER_T16a, COMPOSER_T16b, COMPOSER_T256, COMPOSER_T64};
#[cfg(not(test))]
use crate::alloc_dif_tables::{HUFF_DIF_VAL, RANGE_DIF_VAL};
#[cfg(not(test))]
use crate::bitalloc_tables::{TABLE_A, TABLE_B};
#[cfg(not(test))]
use crate::composer_tables::{COMPOSER_T16a, COMPOSER_T16b, COMPOSER_T256, COMPOSER_T64};

/// Length of one R2 HQ (config 3) frame payload, without the 8-byte SPF
/// container.
pub const FRAME_BYTES: usize = 656;

/// Length of one R2 LL (config 4) frame payload, without the 8-byte SPF
/// container.
pub const LL_FRAME_BYTES: usize = 348;

/// Samples per channel in one block.
pub const BLOCK_SAMPLES: usize = 8;

/// Maximum number of chunks a frame can describe.
///
/// Vendor chunk sizes are quantised to 8/16/32/48/64 blocks and a frame is
/// at most ~192 blocks, so 24 chunks is the physical maximum; the extra room
/// also lets the parser report short chunks from non-vendor streams.
pub const MAX_CHUNKS: usize = 32;

const HEADER_BYTES: usize = 3;
const FRAME_TYPE_R2: u8 = 3;
const FRAME_TYPE_LL: u8 = 4;
const LENGTH_BYTE: u8 = (FRAME_BYTES / 4 - 3) as u8;
const LL_LENGTH_BYTE: u8 = (LL_FRAME_BYTES / 4 - 3) as u8;
const WIDTHS: usize = 16;
const BANDS: usize = 8;
const MAX_CHUNK_BLOCKS: usize = 64;

/// Pair table from AxPackerCfg04_WriterBitAllocation: false selects table A,
/// true selects table B (8-band stereo: [A, A, B, B, ...]).
const PAIR_TABLE_8BAND: [bool; BANDS] = [false, false, true, true, false, false, true, true];

/// Row selection for the differential-allocation tables (the vendor table
/// has nine distinct rows for the sixteen previous-width values).
const DIFF_INDEX: [usize; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 4, 5, 6, 7, 6, 7, 8];

/// Why a payload could not be parsed as an R2 frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameParseError {
    /// The payload is neither FRAME_BYTES (656) nor LL_FRAME_BYTES (348)
    /// long.
    BadLength {
        /// Observed length in bytes.
        len: usize,
    },
    /// Neither the raw nor the OTA header matched, or the length byte and
    /// frame type do not describe the payload size (`[00][03|pad][a1]` for
    /// 656 bytes, `[00][04|pad][54]` for 348).
    BadHeader,
    /// The pad-length trailer does not describe a well-formed pad region.
    BadPadding,
    /// The bitstream ended before a complete chunk header could be read.
    Truncated,
    /// The 4-bit allocation header is not an 8-band stereo R2 header.
    UnsupportedAllocation {
        /// The header nibble that was read.
        header: u8,
    },
    /// A Huffman code was not found within the maximum code length.
    BadHuffmanCode,
    /// A chunk size is not representable (odd, zero-length or over 64).
    BadChunk {
        /// The block count that was decoded.
        blocks: usize,
    },
    /// The frame describes more than MAX_CHUNKS chunks.
    TooManyChunks,
    /// An allocation width has no codeword entry (outside 0..=16).
    InvalidWidth {
        /// The width that was decoded.
        width: u8,
    },
}

impl fmt::Display for FrameParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::BadLength { len } => {
                write!(
                    f,
                    "R2 frame payload must be {FRAME_BYTES} (HQ) or {LL_FRAME_BYTES} (LL) bytes, got {len}"
                )
            }
            Self::BadHeader => f.write_str("not an aptX Adaptive R2 frame header"),
            Self::BadPadding => f.write_str("malformed R2 pad trailer"),
            Self::Truncated => f.write_str("R2 frame bitstream ends before a complete header"),
            Self::UnsupportedAllocation { header } => {
                write!(f, "unsupported R2 allocation header {header:#x}")
            }
            Self::BadHuffmanCode => f.write_str("invalid Huffman code in R2 frame"),
            Self::BadChunk { blocks } => write!(f, "invalid R2 chunk size {blocks}"),
            Self::TooManyChunks => write!(f, "R2 frame has more than {MAX_CHUNKS} chunks"),
            Self::InvalidWidth { width } => {
                write!(f, "R2 frame allocation width {width} is invalid")
            }
        }
    }
}

impl std::error::Error for FrameParseError {}

/// Frame geometry decoded from one R2 payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameBlocks {
    /// Header frame type: 3 = R2 HQ (config 3), 4 = R2 LL (config 4).
    pub frame_type: u8,
    /// Total codec payload bytes this frame was parsed as (656 or 348).
    pub payload_bytes: usize,
    /// Total number of 8-sample blocks in the frame (the sum of chunks).
    pub total_blocks: u16,
    /// Per-chunk block counts in stream order; only the first chunk_count
    /// entries are meaningful.
    pub chunks: [u16; MAX_CHUNKS],
    /// Number of valid entries in chunks.
    pub chunk_count: u8,
    /// The 4-bit allocation header of the first chunk (0xa for 48 kHz,
    /// 0xe for 96 kHz, ...).
    pub alloc_header: u8,
    /// Bytes declared as carried over from the previous frame (0 for a
    /// self-contained vendor frame).
    pub carried_bytes: u16,
    /// The input was in the OTA (pairwise byte-swapped) presentation.
    pub ota: bool,
}

impl FrameBlocks {
    /// The per-chunk block counts in stream order.
    pub fn chunks(&self) -> &[u16] {
        &self.chunks[..usize::from(self.chunk_count).min(MAX_CHUNKS)]
    }

    /// Audio duration of the frame in samples per channel.
    pub fn samples(&self) -> usize {
        usize::from(self.total_blocks) * BLOCK_SAMPLES
    }
}

/// Parse one R2 frame payload (656-byte HQ or 348-byte LL) in either the raw
/// or the OTA presentation and return its block geometry.
pub fn parse_frame(payload: &[u8]) -> Result<FrameBlocks, FrameParseError> {
    let len = payload.len();
    if len != FRAME_BYTES && len != LL_FRAME_BYTES {
        return Err(FrameParseError::BadLength { len });
    }
    let (frame_type, ota) = frame_header(payload).ok_or(FrameParseError::BadHeader)?;
    let mut buf = [0u8; FRAME_BYTES];
    let raw = &mut buf[..len];
    if ota {
        for (i, pair) in payload.as_chunks::<2>().0.iter().enumerate() {
            raw[2 * i] = pair[1];
            raw[2 * i + 1] = pair[0];
        }
    } else {
        raw.copy_from_slice(payload);
    }
    let mut blocks = parse_raw(raw, frame_type)?;
    blocks.ota = ota;
    Ok(blocks)
}

/// Convenience wrapper for callers that only need the RTP step.
pub fn total_blocks(payload: &[u8]) -> Result<u16, FrameParseError> {
    Ok(parse_frame(payload)?.total_blocks)
}

/// Detect the payload shape and presentation.
///
/// The raw header is `[0][type|pad][len]`; the OTA header is that header
/// after the 16-bit unit swap, so the marker sits at byte 1, the frame type
/// at byte 0 and the length byte at byte 3.  The length byte selects the
/// shape: 0xa1 (656 bytes, frame type 3) or 0x54 (348 bytes, type 4).
fn frame_header(payload: &[u8]) -> Option<(u8, bool)> {
    let (frame_type, length_byte) = match payload.len() {
        FRAME_BYTES => (FRAME_TYPE_R2, LENGTH_BYTE),
        LL_FRAME_BYTES => (FRAME_TYPE_LL, LL_LENGTH_BYTE),
        _ => return None,
    };
    if payload[0] == 0 && payload[1] & 0x7f == frame_type && payload[2] == length_byte {
        return Some((frame_type, false));
    }
    if payload[1] == 0 && payload[0] & 0x7f == frame_type && payload[3] == length_byte {
        return Some((frame_type, true));
    }
    None
}

fn parse_raw(raw: &[u8], frame_type: u8) -> Result<FrameBlocks, FrameParseError> {
    let body = &raw[HEADER_BYTES..];
    let stream = if raw[1] & 0x80 != 0 {
        strip_padding(body)?
    } else {
        body
    };
    let mut bits = Bits::new(stream);

    // Exp-Golomb byte count carried over from the previous frame, then the
    // carried bytes themselves.  They belong to the previous frame; this
    // frame's chunks follow directly after them.
    let mut zeros = 0usize;
    while !bits.bit() {
        zeros += 1;
        if bits.ended {
            return Err(FrameParseError::Truncated);
        }
    }
    // bit() consumed the terminating one; the clean-room reader leaves it in
    // place because the exp-Golomb value includes it.
    bits.pos -= 1;
    let carried_plus_one = bits.get(zeros as u32 + 1);
    if bits.ended {
        return Err(FrameParseError::Truncated);
    }
    let Some(carried) = (carried_plus_one as usize).checked_sub(1) else {
        return Err(FrameParseError::Truncated);
    };
    if carried > stream.len() {
        return Err(FrameParseError::Truncated);
    }
    bits.skip(8 * carried);
    if bits.ended {
        return Err(FrameParseError::Truncated);
    }

    // Config 3 (frame type 3) codes allocation escapes; config 4 (type 4)
    // does not.
    let escapes = frame_type == FRAME_TYPE_R2;

    let mut chunks = [0u16; MAX_CHUNKS];
    let mut chunk_count = 0usize;
    let mut total = 0usize;
    let mut alloc_header = 0u8;
    let mut previous: Option<[u8; WIDTHS]> = None;

    while !bits.ended && !bits.only_padding() {
        let widths = match previous {
            None => {
                let header = bits.get(4) as u8;
                if !valid_alloc_header(header) {
                    return Err(FrameParseError::UnsupportedAllocation { header });
                }
                alloc_header = header;
                read_absolute(&mut bits, escapes)?
            }
            Some(prev) => read_differential(&mut bits, &prev, escapes)?,
        };

        let escape = escapes && widths.iter().all(|&w| w == 0);
        let max = if escape {
            let blocks = bits.get(8) as usize + 1;
            if bits.ended {
                return Err(FrameParseError::Truncated);
            }
            blocks
        } else {
            MAX_CHUNK_BLOCKS
        };
        if max == 0 || max > MAX_CHUNK_BLOCKS || max % 2 != 0 {
            return Err(FrameParseError::BadChunk { blocks: max });
        }

        let mut blocks = 0usize;
        while blocks < max {
            if !escape && bits.only_padding() {
                break;
            }
            let complete = read_pair(&mut bits, &widths)?;
            // The clean-room decoder drops a pair that touches the end of the
            // bitstream, even when the final Huffman code matched a run of
            // implicit zero bits.
            if !complete || bits.ended {
                break;
            }
            blocks += 2;
        }

        if blocks == 0 {
            break; // padding-only tail
        }
        if chunk_count == MAX_CHUNKS {
            return Err(FrameParseError::TooManyChunks);
        }
        chunks[chunk_count] = blocks as u16;
        chunk_count += 1;
        total += blocks;
        previous = Some(widths);

        if escape && bits.only_padding() {
            break;
        }
    }

    Ok(FrameBlocks {
        frame_type,
        payload_bytes: raw.len(),
        total_blocks: total as u16,
        chunks,
        chunk_count: chunk_count as u8,
        alloc_header,
        carried_bytes: carried as u16,
        ota: false,
    })
}

/// Strip the R2 pad trailer: a 0x80 marker, zero fill, then the pad length in
/// the scheme t = 255 * trailing_ff + previous_byte, with the pad region
/// itself starting t + 1 bytes before the end.
fn strip_padding(body: &[u8]) -> Result<&[u8], FrameParseError> {
    let mut ff = 0usize;
    for &b in body.iter().rev() {
        if b == 0xff {
            ff += 1;
        } else {
            break;
        }
    }
    if ff == body.len() {
        return Err(FrameParseError::BadPadding);
    }
    let t = 255 * ff + usize::from(body[body.len() - 1 - ff]);
    if t + 1 > body.len() {
        return Err(FrameParseError::BadPadding);
    }
    let start = body.len() - (t + 1);
    let expected = if t == 0 { 0 } else { 0x80 };
    if body[start] != expected {
        return Err(FrameParseError::BadPadding);
    }
    Ok(&body[..start])
}

/// 8-band stereo configs add the stereo flag (8) to a rate code in the low
/// bits: 44.1 kHz 0x8, 48 kHz 0xa, 88.2 kHz 0xc, 96 kHz 0xe.
fn valid_alloc_header(header: u8) -> bool {
    matches!(header, 0x8 | 0xa | 0xc | 0xe)
}

fn unreduce(coded: i32) -> u8 {
    if coded <= 0 {
        0
    } else {
        u8::try_from(coded).map_or(u8::MAX, |value| value.saturating_add(1))
    }
}

/// Absolute allocation (AxPackerCfg04_WriterBitAllocation): eight
/// Huffman-coded nibble pairs over [A, A, B, B, ...].
///
/// Config 3 follows nibble 8 with a 3-bit extension (width 9..=16); config 4
/// has no extensions, so nibble 8 is width 9 and nothing follows it.
fn read_absolute(bits: &mut Bits<'_>, escapes: bool) -> Result<[u8; WIDTHS], FrameParseError> {
    let mut widths = [0u8; WIDTHS];
    for band in 0..BANDS {
        let table: &[(u16, u8)] = if PAIR_TABLE_8BAND[band] {
            &TABLE_B
        } else {
            &TABLE_A
        };
        let sym = bits.huffman(table).map_err(huffman_error)?;
        let mut high = (sym >> 4) as u32;
        let mut low = (sym & 0x0f) as u32;
        if escapes && high == 8 {
            high += bits.get(3);
        }
        if escapes && low == 8 {
            low += bits.get(3);
        }
        widths[2 * band] = unreduce(high as i32);
        widths[2 * band + 1] = unreduce(low as i32);
    }
    Ok(widths)
}

/// Differential allocation update (AxPackerCfg04_WriterBitAllocation_Dif):
/// per field, a Huffman-coded 4-bit difference from the previous coded
/// width, clamped into the row's range with optional 3-bit extensions.
///
/// Config 4 (no escapes) writes the 4-bit difference directly: the value is
/// the new coded width, with no range reinterpretation and no 3-bit group.
fn read_differential(
    bits: &mut Bits<'_>,
    previous: &[u8; WIDTHS],
    escapes: bool,
) -> Result<[u8; WIDTHS], FrameParseError> {
    let mut widths = [0u8; WIDTHS];
    for field in 0..WIDTHS {
        let prev = previous[field];
        let pv = usize::from(if prev > 1 { prev - 1 } else { prev }).min(15);
        let row = &HUFF_DIF_VAL[DIFF_INDEX[pv]];
        let sym = bits.huffman(row).map_err(huffman_error)? as i32;
        let mut value = (pv as i32 + sym) & 0x0f;
        if escapes {
            let (lo, hi) = RANGE_DIF_VAL[pv];
            if value > hi {
                value -= 16;
            }
            if value == lo {
                value = lo - bits.get(3) as i32;
            } else if value == hi {
                value = hi + bits.get(3) as i32;
            }
        }
        widths[field] = unreduce(value);
    }
    Ok(widths)
}

/// Skip one composer pair (two blocks) using the codeword table for each
/// active allocation width.  Returns false when the bitstream ended before
/// the pair completed.
fn read_pair(bits: &mut Bits<'_>, widths: &[u8; WIDTHS]) -> Result<bool, FrameParseError> {
    for &width in widths {
        if width == 0 {
            continue;
        }
        let (table, extension) = codeword(width)?;
        match bits.huffman(table) {
            Ok(_) => {
                if extension > 0 {
                    bits.get(extension);
                    bits.get(extension);
                }
            }
            Err(HuffEnd::Ended) => return Ok(false),
            Err(HuffEnd::BadCode) => return Err(FrameParseError::BadHuffmanCode),
        }
    }
    Ok(true)
}

/// Huffman table and raw extension-bit count for one allocation width.
type Codeword = (&'static [(u16, u8)], u32);

/// Codeword geometry for one allocation width: (table, extension bits).
/// Width 1 uses table 1 and width 2 table 2 (the vendor never emits width 1,
/// but the decoder table has a distinct entry for it).
fn codeword(width: u8) -> Result<Codeword, FrameParseError> {
    match width {
        1 => Ok((&COMPOSER_T16a, 0)),
        2 => Ok((&COMPOSER_T16b, 0)),
        3 => Ok((&COMPOSER_T64, 0)),
        4..=16 => Ok((&COMPOSER_T256, u32::from(width - 4))),
        _ => Err(FrameParseError::InvalidWidth { width }),
    }
}

fn huffman_error(error: HuffEnd) -> FrameParseError {
    match error {
        HuffEnd::Ended => FrameParseError::Truncated,
        HuffEnd::BadCode => FrameParseError::BadHuffmanCode,
    }
}

/// Why a Huffman lookup stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HuffEnd {
    /// The reader ran past the end of the bitstream.
    Ended,
    /// No code matched within the maximum code length.
    BadCode,
}

/// MSB-first bit reader that mirrors the clean-room decoder: reading past the
/// end yields zero bits and raises a sticky ended flag instead of failing, so
/// a final partial pair can be recognised and dropped.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    ended: bool,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            ended: false,
        }
    }

    fn left(&self) -> usize {
        self.data.len().saturating_mul(8).saturating_sub(self.pos)
    }

    fn bit(&mut self) -> bool {
        let at = self.pos;
        self.pos += 1;
        match self.data.get(at / 8) {
            Some(&byte) => (byte >> (7 - at % 8)) & 1 != 0,
            None => {
                self.ended = true;
                false
            }
        }
    }

    fn get(&mut self, n: u32) -> u32 {
        let mut value = 0u32;
        for _ in 0..n {
            value = (value << 1) | u32::from(self.bit());
        }
        value
    }

    fn skip(&mut self, n: usize) {
        for _ in 0..n {
            self.bit();
        }
    }

    /// The clean-room decoder's end test: fewer than eight bits remain and
    /// all of them are zero.
    fn only_padding(&self) -> bool {
        if self.pos >= self.data.len() * 8 {
            return true;
        }
        let left = self.left();
        if left >= 8 {
            return false;
        }
        self.data[self.pos / 8] & ((1u8 << left) - 1) == 0
    }

    fn huffman(&mut self, table: &[(u16, u8)]) -> Result<usize, HuffEnd> {
        let mut code = 0u32;
        for len in 1..=24u8 {
            code = (code << 1) | u32::from(self.bit());
            for (sym, &(candidate, candidate_len)) in table.iter().enumerate() {
                if candidate_len == len && u32::from(candidate) == code {
                    return Ok(sym);
                }
            }
            if self.ended {
                return Err(HuffEnd::Ended);
            }
        }
        Err(HuffEnd::BadCode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    const RECORD_BYTES: usize = FRAME_BYTES + 8;

    /// Locate audio/aptx-adaptive/out, whether the tests run under cargo or
    /// as a standalone rustc --test binary.
    fn data_dir() -> Option<PathBuf> {
        if let Some(manifest) = option_env!("CARGO_MANIFEST_DIR") {
            let candidate = Path::new(manifest).join("../aptx-adaptive/out");
            if candidate.join("music_clean_cie_r2.bin").exists() {
                return Some(candidate);
            }
        }
        let mut dir = std::env::current_dir().ok()?;
        loop {
            for relative in [
                "audio/aptx-adaptive/out",
                "aptx-adaptive/out",
                "../aptx-adaptive/out",
            ] {
                let candidate = dir.join(relative);
                if candidate.join("music_clean_cie_r2.bin").exists() {
                    return Some(candidate);
                }
            }
            if !dir.pop() {
                return None;
            }
        }
    }

    /// The OTA payload of every 664-byte capture record (container stripped).
    fn payloads(path: &Path) -> std::io::Result<Vec<Vec<u8>>> {
        let data = std::fs::read(path)?;
        assert_eq!(data.len() % RECORD_BYTES, 0, "{}", path.display());
        Ok(data
            .as_chunks::<RECORD_BYTES>()
            .0
            .iter()
            .map(|record| record[RECORD_BYTES - FRAME_BYTES..].to_vec())
            .collect())
    }

    fn unswap(payload: &[u8]) -> Vec<u8> {
        let mut raw = Vec::with_capacity(payload.len());
        for pair in payload.as_chunks::<2>().0 {
            raw.push(pair[1]);
            raw.push(pair[0]);
        }
        raw
    }

    fn check_chunks(frame: &FrameBlocks) {
        assert!(frame.chunk_count > 0, "frame has no chunks");
        let mut sum = 0usize;
        for &blocks in frame.chunks() {
            assert!(blocks > 0, "zero-length chunk");
            assert!(
                blocks <= MAX_CHUNK_BLOCKS as u16,
                "oversized chunk {blocks}"
            );
            assert_eq!(blocks % 2, 0, "odd chunk {blocks}");
            sum += usize::from(blocks);
        }
        assert_eq!(sum, usize::from(frame.total_blocks));
    }

    #[test]
    fn ll_header_selects_the_348_byte_shape() {
        // Raw: [00][04][54], OTA: the same three bytes after the pair swap.
        let mut ll = [0u8; LL_FRAME_BYTES];
        ll[1] = FRAME_TYPE_LL;
        ll[2] = LL_LENGTH_BYTE;
        assert_eq!(LL_LENGTH_BYTE, 0x54);
        assert_eq!(frame_header(&ll), Some((FRAME_TYPE_LL, false)));

        let mut ota = [0u8; LL_FRAME_BYTES];
        ota[0] = FRAME_TYPE_LL;
        ota[3] = LL_LENGTH_BYTE;
        assert_eq!(frame_header(&ota), Some((FRAME_TYPE_LL, true)));

        // A 656-byte header on a 348-byte payload (or vice versa) is not
        // a frame: the length byte and the shape must agree.
        let mut wrong = [0u8; LL_FRAME_BYTES];
        wrong[1] = FRAME_TYPE_R2;
        wrong[2] = LENGTH_BYTE;
        assert_eq!(frame_header(&wrong), None);
        let mut hq = [0u8; FRAME_BYTES];
        hq[1] = FRAME_TYPE_R2;
        hq[2] = LENGTH_BYTE;
        assert_eq!(frame_header(&hq), Some((FRAME_TYPE_R2, false)));
    }

    #[test]
    fn ll_payload_length_is_checked() {
        // Off-by-two lengths are rejected before the header is examined.
        for len in [
            0usize,
            FRAME_BYTES + 8,
            LL_FRAME_BYTES - 1,
            LL_FRAME_BYTES + 1,
        ] {
            let payload = vec![0u8; len];
            assert_eq!(
                parse_frame(&payload),
                Err(FrameParseError::BadLength { len })
            );
        }

        // A 348-byte payload with neither presentation's header is rejected.
        let mut ll = [0u8; LL_FRAME_BYTES];
        ll[1] = FRAME_TYPE_LL;
        ll[2] = LL_LENGTH_BYTE;
        // header is valid, but the empty bitstream cannot carry the
        // exp-Golomb count
        assert_eq!(parse_frame(&ll), Err(FrameParseError::Truncated));
        ll[0] = 0xff;
        assert_eq!(parse_frame(&ll), Err(FrameParseError::BadHeader));
    }

    #[test]
    fn clean_capture_matches_blocks_sidecar() {
        let Some(dir) = data_dir() else {
            eprintln!("skipping: aptx-adaptive/out not found");
            return;
        };
        let payloads = payloads(&dir.join("music_clean_cie_r2.bin")).expect("capture");
        let sidecar: Vec<u16> = std::fs::read_to_string(dir.join("music_clean_cie_r2.blocks"))
            .expect("sidecar")
            .split_whitespace()
            .map(|value| value.parse().expect("block count"))
            .collect();
        assert_eq!(payloads.len(), sidecar.len());

        let mut patterns = BTreeSet::new();
        for (record, payload) in payloads.iter().enumerate() {
            let ota = parse_frame(payload).expect("OTA record parses");
            assert!(ota.ota, "record {record} is the OTA presentation");
            assert_eq!(ota.alloc_header, 0xa, "record {record}");
            assert_eq!(ota.carried_bytes, 0, "record {record}");
            assert_eq!(usize::from(ota.total_blocks), usize::from(sidecar[record]));
            check_chunks(&ota);
            patterns.insert(ota.chunks().to_vec());

            let raw = parse_frame(&unswap(payload)).expect("raw record parses");
            assert!(!raw.ota, "record {record} is the raw presentation");
            assert_eq!(raw.total_blocks, ota.total_blocks, "record {record}");
            assert_eq!(raw.chunks(), ota.chunks(), "record {record}");
        }

        assert!(patterns.contains(&vec![64, 64, 32]), "160-block frames");
        assert!(patterns.contains(&vec![64, 64]), "128-block frames");
        assert!(patterns
            .iter()
            .all(|chunks| matches!(chunks.iter().sum::<u16>(), 128 | 160)));
    }

    #[test]
    fn music10_frames_parse() {
        let Some(dir) = data_dir() else {
            eprintln!("skipping: aptx-adaptive/out not found");
            return;
        };
        let payloads = payloads(&dir.join("music10_r2.bin")).expect("capture");
        let mut totals = BTreeSet::new();
        let mut padded = 0usize;
        for (record, payload) in payloads.iter().enumerate() {
            let frame = parse_frame(payload).expect("record parses");
            assert!(frame.ota, "record {record}");
            check_chunks(&frame);
            assert!(
                matches!(frame.total_blocks, 128 | 160),
                "record {record}: {} blocks",
                frame.total_blocks
            );
            totals.insert(frame.total_blocks);
            let raw = unswap(payload);
            if raw[1] & 0x80 != 0 {
                padded += 1;
            }
            if record % 50 == 0 {
                let parsed = parse_frame(&raw).expect("raw record parses");
                assert_eq!(parsed.chunks(), frame.chunks(), "record {record}");
            }
        }
        assert_eq!(padded, payloads.len(), "music10 capture is padded");
        assert!(totals.contains(&128) && totals.contains(&160));
    }

    #[test]
    fn hq_96k_alloc_header_is_accepted() {
        let Some(dir) = data_dir() else {
            eprintln!("skipping: aptx-adaptive/out not found");
            return;
        };
        let payloads = payloads(&dir.join("music_clean_cie_r2.bin")).expect("capture");
        for payload in payloads.iter().take(8) {
            let baseline = parse_frame(&unswap(payload)).expect("48 kHz frame");
            assert_eq!(baseline.alloc_header, 0xa);

            // Rewrite the 4-bit allocation header nibble to 0xe (96 kHz).
            // The geometry is rate-independent, so nothing else changes.
            let mut raw = unswap(payload);
            raw[3] = 0x80 | (0x0e << 3) | (raw[3] & 0x07);
            let hires = parse_frame(&raw).expect("96 kHz header frame");
            assert_eq!(hires.alloc_header, 0xe);
            assert_eq!(hires.total_blocks, baseline.total_blocks);
            assert_eq!(hires.chunks(), baseline.chunks());
        }
    }

    #[test]
    fn pad_trailer_forms() {
        // Marker, one zero, then the pad length (t = 2): payload is [0xAA].
        assert_eq!(strip_padding(&[0xaa, 0x80, 0x00, 0x02]), Ok(&[0xaa][..]));
        // t = 0 uses a zero byte instead of the 0x80 marker.
        assert_eq!(strip_padding(&[0xaa, 0x00]), Ok(&[0xaa][..]));
        assert_eq!(
            strip_padding(&[0xaa, 0x80, 0x00, 0x03]),
            Err(FrameParseError::BadPadding)
        );
        assert_eq!(
            strip_padding(&[0xff, 0xff]),
            Err(FrameParseError::BadPadding)
        );
    }

    #[test]
    fn rejects_malformed_input() {
        assert_eq!(parse_frame(&[]), Err(FrameParseError::BadLength { len: 0 }));
        assert_eq!(
            parse_frame(&[0u8; FRAME_BYTES + 8]),
            Err(FrameParseError::BadLength {
                len: FRAME_BYTES + 8
            })
        );
        assert_eq!(
            parse_frame(&[0u8; FRAME_BYTES]),
            Err(FrameParseError::BadHeader)
        );

        // Valid header, empty bitstream: the carry exp-Golomb runs off the
        // end.
        let mut raw = [0u8; FRAME_BYTES];
        raw[1] = FRAME_TYPE_R2;
        raw[2] = LENGTH_BYTE;
        assert_eq!(parse_frame(&raw), Err(FrameParseError::Truncated));

        // Pad flag with an inconsistent trailer.
        raw[1] = FRAME_TYPE_R2 | 0x80;
        raw[FRAME_BYTES - 1] = 1;
        raw[FRAME_BYTES - 2] = 0x00; // the marker should be 0x80
        assert_eq!(parse_frame(&raw), Err(FrameParseError::BadPadding));

        // Valid header, but a 0x0 first allocation nibble.
        let mut raw = [0u8; FRAME_BYTES];
        raw[1] = FRAME_TYPE_R2;
        raw[2] = LENGTH_BYTE;
        raw[3] = 0x80; // carry 0, then allocation header 0
        assert_eq!(
            parse_frame(&raw),
            Err(FrameParseError::UnsupportedAllocation { header: 0 })
        );
    }

    #[test]
    fn allocation_headers_match_r2_rates() {
        for header in [0x8u8, 0xa, 0xc, 0xe] {
            assert!(valid_alloc_header(header), "{header:#x}");
        }
        for header in [0x0u8, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x9, 0xb, 0xd, 0xf] {
            assert!(!valid_alloc_header(header), "{header:#x}");
        }
    }
}

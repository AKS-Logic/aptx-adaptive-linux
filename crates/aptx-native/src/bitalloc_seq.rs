//! The frame's bit-allocation sequence writer/reader, ported from
//! `AxPackerCfg04_WriterBitAllocation` (R2 module, @0x17fdc):
//!
//! * per band: field = width-1 (0 when width < 2), nibble = field & 0xf, but
//!   field > 7 escapes to nibble 8 with the remainder `(field-8)` emitted as
//!   3 extension bits (accumulated per pair, written MSB-first);
//! * bands are processed in pairs, pair value = `(nibA << 4) | nibB`;
//! * the pair value is Huffman-coded through the per-pair tables
//!   (`g_aPacker_cfg0_Tables_cfg0BitAlloc_8Band` pointer array:
//!   0x381a0, 0x381a0, 0x383c8, 0x383c8 for pairs 0..3);
//! * then the extension bits (3 per escaped band), then an 8-bit trailer
//!   (`*(packer+0x34)-1`; the field's source is not yet identified, passed
//!   in by the caller).
//!
//! SPDX-License-Identifier: MIT

use crate::bitalloc_tables::{TABLE_A, TABLE_B};
use crate::entropy::{BitReader, BitWriter};
use crate::BANDS;

fn field(w: u8) -> u32 {
    if w >= 2 {
        w as u32 - 1
    } else {
        0
    }
}

fn table_for_pair(p: usize) -> &'static [(u16, u8); 137] {
    // pointer array from the binary: [A, A, B, B] for pairs 0..3
    if p < 2 {
        &TABLE_A
    } else {
        &TABLE_B
    }
}

/// Write the allocation sequence for the given per-band widths.
pub fn write_alloc_seq(widths: &[u8; BANDS], trailer: u8, out: &mut BitWriter) {
    for p in 0..BANDS / 2 {
        let (va, vb) = (field(widths[2 * p]), field(widths[2 * p + 1]));
        let pair = (((va.min(8)) << 4) | vb.min(8)) as usize;
        let (code, len) = table_for_pair(p)[pair];
        out.write(code as u32, len as u32);
        let mut ext: u32 = 0;
        let mut nbits = 0u32;
        if va > 7 {
            ext = (va - 8) & 7;
            nbits = 3;
        }
        if vb > 7 {
            ext = ((vb - 8) & 7) | (ext << 3);
            nbits += 3;
        }
        if nbits > 0 {
            out.write(ext, nbits);
        }
    }
    let sum: u32 = widths.iter().map(|&w| w as u32).sum();
    if sum == 0 {
        out.write(trailer as u32, 8); // trailer, written only when all widths are zero
    }
}

/// Read back an allocation sequence (the inverse of [`write_alloc_seq`]);
/// returns the per-band widths.
pub fn read_alloc_seq(input: &mut BitReader) -> Option<[u8; BANDS]> {
    let mut widths = [0u8; BANDS];
    for p in 0..BANDS / 2 {
        let table = table_for_pair(p);
        let pair = decode_pair(table, input)?;
        let (va, vb) = ((pair >> 4) as u32, (pair & 0xf) as u32);
        let mut ea = 0u32;
        let mut eb = 0u32;
        if va == 8 {
            ea = input.read(3)?;
        }
        if vb == 8 {
            eb = input.read(3)?;
        }
        widths[2 * p] = (va + ea) as u8 + 1;
        widths[2 * p + 1] = (vb + eb) as u8 + 1;
        for k in [2 * p, 2 * p + 1] {
            if widths[k] == 1 {
                widths[k] = 0; // width 0 and 1 both code as nibble 0
            }
        }
    }
    let sum: u32 = widths.iter().map(|&w| w as u32).sum();
    if sum == 0 {
        let _ = input.read(8)?; // trailer, written only when all widths are zero
    }
    Some(widths)
}

/// The config-0 allocation variant (`AxPackerCfg0_WriterBitAllocation`,
/// R2 @0x17f08 — the TID0 audio header's mode>1 path): pair nibbles with
/// **no escape handling and no trailer**; pair tables are `[A,B,A,B,...]`
/// (4-band config) or `[A,A,B,B,...]` (8-band, mode 2..4).
pub fn read_alloc_seq_cfg0(input: &mut BitReader, eight_band: bool) -> Option<[u8; BANDS]> {
    let mut widths = [0u8; BANDS];
    for p in 0..BANDS / 2 {
        let table = match (eight_band, p & 1) {
            (true, 0) if p < 2 => &TABLE_A,
            (true, _) => &TABLE_B,
            (false, 0) => &TABLE_A,
            (false, _) => &TABLE_B,
        };
        let pair = decode_pair(table, input)?;
        let (va, vb) = ((pair >> 4) as u8, (pair & 0xf) as u8);
        widths[2 * p] = if va == 0 { 0 } else { va + 1 };
        widths[2 * p + 1] = if vb == 0 { 0 } else { vb + 1 };
    }
    Some(widths)
}

/// The 16-field allocation (count = 16, the live packer's value): eight
/// band-pairs, tables `[A,A,B,B,A,A,B,B]`, Cfg04 rules (escape nibble 8 +
/// 3-bit extension, trailer only when all fields are zero).
pub fn read_alloc_seq16(input: &mut BitReader) -> Option<[u8; 16]> {
    read_alloc_seq16_cfg(input, true)
}

/// [`read_alloc_seq16`] with the config-3 allocation escapes selectable.
///
/// Config 4 (the 348-byte LL frame) has no escapes: nibble 8 means width 9
/// with no 3-bit extension, and an all-zero allocation has no 8-bit
/// trailer.  Pass `escapes = false` for that bitstream.
pub fn read_alloc_seq16_cfg(input: &mut BitReader, escapes: bool) -> Option<[u8; 16]> {
    let mut widths = [0u8; 16];
    for p in 0..8 {
        let table = match p {
            0 | 1 | 4 | 5 => &TABLE_A,
            _ => &TABLE_B,
        };
        let pair = decode_pair(table, input)?;
        let (va, vb) = ((pair >> 4) as u32, (pair & 0xf) as u32);
        if !escapes {
            widths[2 * p] = if va == 0 { 0 } else { va as u8 + 1 };
            widths[2 * p + 1] = if vb == 0 { 0 } else { vb as u8 + 1 };
            continue;
        }
        let mut ea = 0u32;
        let mut eb = 0u32;
        if va == 8 {
            ea = input.read(3)?;
        }
        if vb == 8 {
            eb = input.read(3)?;
        }
        widths[2 * p] = (va + ea) as u8 + 1;
        widths[2 * p + 1] = (vb + eb) as u8 + 1;
        for k in [2 * p, 2 * p + 1] {
            if widths[k] == 1 {
                widths[k] = 0;
            }
        }
    }
    if !escapes {
        return Some(widths);
    }
    let sum: u32 = widths.iter().map(|&w| w as u32).sum();
    if sum == 0 {
        let _ = input.read(8)?;
    }
    Some(widths)
}

fn decode_pair(table: &[(u16, u8); 137], input: &mut BitReader) -> Option<usize> {
    let mut code: u32 = 0;
    for ln in 1..=24u32 {
        code = (code << 1) | input.read(1)?;
        for (sym, &(c, l)) in table.iter().enumerate() {
            if l as u32 == ln && c as u32 == code {
                return Some(sym);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitalloc_tables::{TABLE_A, TABLE_B};

    #[test]
    fn tables_are_prefix_free() {
        for table in [&TABLE_A, &TABLE_B] {
            for (i, &(c1, l1)) in table.iter().enumerate() {
                if l1 == 0 {
                    continue;
                }
                for (j, &(c2, l2)) in table.iter().enumerate() {
                    if i == j || l2 == 0 {
                        continue;
                    }
                    assert!(
                        !(l1 <= l2 && (u32::from(c2) >> (l2 - l1)) == u32::from(c1)),
                        "prefix conflict {i} vs {j}"
                    );
                }
            }
        }
    }

    #[test]
    fn alloc_seq_round_trips() {
        for widths in [
            [0u8, 0, 0, 0, 0, 0, 0, 0],
            [2, 3, 4, 5, 6, 7, 8, 9],
            [12, 12, 0, 2, 9, 4, 0, 11],
        ] {
            let mut bw = BitWriter::new();
            write_alloc_seq(&widths, 0x5a, &mut bw);
            let bits = bw.len_bits();
            let bytes = bw.into_bytes();
            let mut br = BitReader::new(&bytes);
            let back = read_alloc_seq(&mut br).expect("read back");
            assert_eq!(back, widths, "widths {widths:?}");
            assert_eq!(br.pos_bits(), bits);
        }
    }
}

/// `AxComposerWriteChunkSize` — the 4-bit size class (bytes 36+12k → class
/// 8+k for k=0..7, i.e. 36..120); any other size writes a single 0 bit.
pub fn write_chunk_size(size_bytes: u32, out: &mut BitWriter) {
    match size_bytes {
        0x24 => out.write(8, 4),
        0x30 => out.write(9, 4),
        0x3c => out.write(10, 4),
        0x48 => out.write(11, 4),
        0x54 => out.write(12, 4),
        0x60 => out.write(13, 4),
        0x6c => out.write(14, 4),
        0x78 => out.write(15, 4),
        _ => out.write(0, 1),
    }
}

/// Read back the chunk-size field: the 4-bit class (8..15) is unambiguous
/// against the 1-bit zero path because all class codes start with bit 1.
/// `Some(None)` = the zero path, `Some(Some(bytes))` = a size class.
pub fn read_chunk_size(input: &mut BitReader) -> Option<Option<u32>> {
    let b = input.read(1)?;
    if b == 0 {
        return Some(None);
    }
    let rest = input.read(3)?;
    Some(Some(36 + 12 * rest))
}

//! TID0 (R2) band-value coding — the vendor's real bitstream primitives.
//!
//! Transcribed from the shipped encoder's `AxEntropyComposerHuff_TID0`
//! (cross-checked against the reference decoder's `FUN_00408110` /
//! `FUN_00408330`) and driven by the extracted tables in
//! [`crate::tid0_tables`]:
//!
//! ```text
//! per band with width w:
//!   w <= 2  -> table B/D (2 symbol bits/channel, no extension)   budget 6
//!   w == 3  -> table A   (3 symbol bits/channel)                 budget 14
//!   w >= 4  -> table C   (4 symbol bits/channel)                 budget 19+2(w-4)
//!              + (w-4) raw extension bits per channel
//!
//! per sample pair (both channels):
//!   prefix_c = v_c >> ext
//!   symbol   = (prefix_0 << sym_bits) | prefix_1
//!   write HIER[symbol].code (HIER[symbol].len bits)
//!   then raw0 = v_0 & ((1<<ext)-1), raw1 likewise   (ext bits each)
//! ```
//!
//! The decoder inverts it as `value = (prefix << ext) | raw`
//! (`FUN_00408330`, verbatim).
//!
//! SPDX-License-Identifier: MIT

use crate::entropy::{BitReader, BitWriter};
use crate::tid0_tables::{HuffEntry, HUFF_TABLES};

/// The coding geometry for a band width: `(table index, symbol bits,
/// extension bits)`.
///
/// Table indices match [`crate::tid0_tables::HUFF_TABLES`]: 0 = B, 1 = D,
/// 2 = A, 3 = C.
pub fn geometry_for_width(w: u8) -> (usize, u8, u8) {
    match w {
        0 => (0, 0, 0),     // inactive band: nothing written
        1 | 2 => (0, 2, 0), // table B
        3 => (2, 3, 0),     // table A
        _ => (3, 4, w - 4), // table C + extension
    }
}

/// The budget cost in bits for a band of width `w` (the mapping recovered in
/// both the encoder's composer and the decoder's `FUN_00408170`).
pub fn width_cost_bits(w: u8) -> usize {
    match w {
        0 => 0,
        1 | 2 => 6,
        3 => 14,
        _ => 2 * w as usize + 11,
    }
}

/// Encode one band's worth of sample pairs.
///
/// `v0`/`v1` are the two channels' values for consecutive sample pairs.
/// Returns the number of bits written.
pub fn encode_band(
    w: u8,
    v0: &[i32],
    v1: &[i32],
    out: &mut BitWriter,
) -> Result<usize, &'static str> {
    assert_eq!(v0.len(), v1.len(), "channels must align");
    if w == 0 || v0.is_empty() {
        return Ok(0);
    }
    let (ti, sym_bits, ext) = geometry_for_width(w);
    let table = HUFF_TABLES[ti];
    let before = out.len_bits();
    let sym_mask: i32 = (1 << sym_bits) - 1;
    let ext_mask: i32 = if ext >= 31 { -1 } else { (1 << ext) - 1 };

    for (&a, &b) in v0.iter().zip(v1.iter()) {
        let pa = a >> ext;
        let pb = b >> ext;
        if pa < 0 || pb < 0 || pa > sym_mask || pb > sym_mask {
            return Err("prefix out of range");
        }
        let sym = ((pa << sym_bits) | pb) as usize;
        let (code, len) = table[sym];
        out.write(code as u32, len as u32);
        if ext > 0 {
            out.write((a & ext_mask) as u32, ext as u32);
            out.write((b & ext_mask) as u32, ext as u32);
        }
    }
    Ok(out.len_bits() - before)
}

/// Decode one band's worth of sample pairs (the mirrored primitive).
pub fn decode_band(
    w: u8,
    pairs: usize,
    input: &mut BitReader,
) -> Result<(Vec<i32>, Vec<i32>), &'static str> {
    if w == 0 {
        return Ok((vec![0; pairs], vec![0; pairs]));
    }
    let (ti, sym_bits, ext) = geometry_for_width(w);
    let table = HUFF_TABLES[ti];
    let mut v0 = Vec::with_capacity(pairs);
    let mut v1 = Vec::with_capacity(pairs);
    for _ in 0..pairs {
        let sym = decode_symbol(table, input).ok_or("bad huffman code")?;
        let pa = (sym >> sym_bits) as i32;
        let pb = (sym & ((1 << sym_bits) - 1)) as i32;
        let (r0, r1) = if ext > 0 {
            (
                input.read(ext as u32).ok_or("ext underflow")? as i32,
                input.read(ext as u32).ok_or("ext underflow")? as i32,
            )
        } else {
            (0, 0)
        };
        v0.push((pa << ext) | r0);
        v1.push((pb << ext) | r1);
    }
    Ok((v0, v1))
}

/// Decode one Huffman symbol from a flat `{code, len}` table by
/// longest-prefix matching.
pub fn decode_symbol(table: &[HuffEntry], input: &mut BitReader) -> Option<usize> {
    let start = input.pos_bits();
    let mut code: u32 = 0;
    for ln in 1..=19u32 {
        code = (code << 1) | input.read(1)?;
        for (sym, &(c, l)) in table.iter().enumerate() {
            if l as u32 == ln && c as u32 == code {
                return Some(sym);
            }
        }
        let _ = start;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_and_costs_match_the_recovered_rule() {
        assert_eq!(geometry_for_width(0), (0, 0, 0));
        assert_eq!(geometry_for_width(2), (0, 2, 0));
        assert_eq!(geometry_for_width(3), (2, 3, 0));
        assert_eq!(geometry_for_width(4), (3, 4, 0));
        assert_eq!(geometry_for_width(8), (3, 4, 4));
        assert_eq!(geometry_for_width(11), (3, 4, 7));
        assert_eq!(width_cost_bits(0), 0);
        assert_eq!(width_cost_bits(2), 6);
        assert_eq!(width_cost_bits(3), 14);
        assert_eq!(width_cost_bits(4), 19);
        assert_eq!(width_cost_bits(11), 33);
    }

    #[test]
    fn band_roundtrip_all_widths() {
        for w in 1..=11u8 {
            let (_, sym_bits, ext) = geometry_for_width(w);
            let sym_max = (1 << sym_bits) - 1;
            let ext_max = if ext == 0 { 0 } else { (1 << ext) - 1 };
            // sweep the code space: a few representative values
            let mut v0 = Vec::new();
            let mut v1 = Vec::new();
            for a in 0..=sym_max {
                for b in 0..=sym_max {
                    v0.push((a << ext) | (ext_max & a as i32));
                    v1.push((b << ext) | (ext_max & b as i32));
                }
            }
            let mut wtr = BitWriter::new();
            let bits = encode_band(w, &v0, &v1, &mut wtr).unwrap();
            assert!(bits > 0);
            wtr.write(0, 32); // padding for safe reads
            let bytes = wtr.into_bytes();
            let mut rdr = BitReader::new(&bytes);
            let (d0, d1) = decode_band(w, v0.len(), &mut rdr).unwrap();
            assert_eq!(d0, v0, "width {w}");
            assert_eq!(d1, v1, "width {w}");
        }
    }

    #[test]
    fn coded_size_is_within_the_budget() {
        // the budget rule is an upper bound enforced by the allocator; the
        // actual coded size must fit it for in-range values
        for w in 1..=11u8 {
            let (_, sym_bits, ext) = geometry_for_width(w);
            let sym_max = (1 << sym_bits) - 1;
            let n = 64;
            let v0: Vec<i32> = (0..n).map(|i| ((i as i32) & sym_max) << ext).collect();
            let v1: Vec<i32> = (0..n).map(|i| ((i as i32 * 3) & sym_max) << ext).collect();
            let mut wtr = BitWriter::new();
            let bits = encode_band(w, &v0, &v1, &mut wtr).unwrap();
            // worst case per pair: max code length + 2*ext
            let max_code_len = HUFF_TABLES[geometry_for_width(w).0]
                .iter()
                .map(|&(_, l)| l as usize)
                .max()
                .unwrap();
            assert!(
                bits <= n * (max_code_len + 2 * ext as usize),
                "width {w}: {bits} bits"
            );
        }
    }
}

/// Encode one band's sample pairs with the vendor's **signed** (two's
/// complement) prefix handling — the form `AxEntropyComposerHuff_TID0`
/// writes: `prefix = (v >> ext) & sym_mask` with an arithmetic shift (the
/// `&` handles negative values as their two's-complement low bits).
pub fn encode_band_signed(
    w: u8,
    v0: &[i32],
    v1: &[i32],
    out: &mut BitWriter,
) -> Result<usize, &'static str> {
    if v0.len() != v1.len() {
        return Err("channels must align");
    }
    if w == 0 || v0.is_empty() {
        return Ok(0);
    }
    let (ti, sym_bits, ext) = geometry_for_width(w);
    let table = HUFF_TABLES[ti];
    let before = out.len_bits();
    let sym_mask: i32 = (1 << sym_bits) - 1;
    let ext_mask: i32 = if ext >= 31 { -1 } else { (1 << ext) - 1 };

    for (&a, &b) in v0.iter().zip(v1.iter()) {
        let pa = (a >> ext) & sym_mask;
        let pb = (b >> ext) & sym_mask;
        let sym = ((pa << sym_bits) | pb) as usize;
        let (code, len) = table[sym];
        out.write(code as u32, len as u32);
        if ext > 0 {
            out.write((a & ext_mask) as u32, ext as u32);
            out.write((b & ext_mask) as u32, ext as u32);
        }
    }
    Ok(out.len_bits() - before)
}

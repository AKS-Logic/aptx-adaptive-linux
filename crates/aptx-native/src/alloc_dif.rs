//! Reader for the differential allocation field
//! (`AxPackerCfg04_WriterBitAllocation_Dif`, R2 module @0x27dd4): per band,
//! the coded width is updated by a Huffman-coded 4-bit difference from the
//! previous coded width, clamped to that entry's `RANGE_DIF_VAL` range,
//! with optional 3-bit below-min/above-max extension groups (the writer
//! emits the group when the new coded width reaches or crosses a range
//! bound).
//!
//! SPDX-License-Identifier: MIT

use crate::alloc_dif_tables::{HUFF_DIF_VAL, RANGE_DIF_VAL};
use crate::entropy::BitReader;
use crate::BANDS;

fn decode16(table: &[(u16, u8); 16], input: &mut BitReader) -> Option<usize> {
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

/// Read one differential allocation step.  `prev` holds the previous coded
/// widths (w-1 for w>=2, else 0) and is updated in place; the returned
/// array is the new set of widths.
pub fn read_alloc_dif(prev: &mut [u8; BANDS], input: &mut BitReader) -> Option<[u8; BANDS]> {
    let mut widths = [0u8; BANDS];
    for b in 0..BANDS {
        let p = (prev[b] as usize).min(15);
        let (mn, mx) = RANGE_DIF_VAL[p];
        let idx = decode16(&HUFF_DIF_VAL[p], input)?;
        let diff = if idx > 8 { idx as i32 - 16 } else { idx as i32 };
        let mut cand = p as i32 + diff;
        if cand <= mn {
            let e = input.read(3)? as i32;
            cand = mn - e;
        } else if cand >= mx {
            let e = input.read(3)? as i32;
            cand = mx + e;
        }
        let coded = cand.max(0) as u8;
        prev[b] = coded;
        widths[b] = if coded == 0 { 0 } else { coded + 1 };
    }
    Some(widths)
}

/// Width -> coded width (inverse of the mapping above).
pub fn coded_of(width: u8) -> u8 {
    if width >= 2 {
        width - 1
    } else {
        0
    }
}

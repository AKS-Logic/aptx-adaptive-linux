//! The TID0 composer pair coding (`AxPackerTID0_EntropyComposerWrite_Interleave`,
//! R2 module): per step and per active field, one pair symbol codes the two
//! channels' quantizer values as `(sym0 << sym_bits) | sym1` plus optional
//! raw extension bits.
//!
//! SPDX-License-Identifier: MIT

use crate::composer_tables::{
    COMPOSER_T16a, COMPOSER_T16b, COMPOSER_DESC, COMPOSER_T256, COMPOSER_T64,
};
use crate::entropy::BitReader;

fn table(sel: u8) -> &'static [(u16, u8)] {
    match sel {
        1 => &COMPOSER_T16a,
        2 => &COMPOSER_T16b,
        3 => &COMPOSER_T64,
        _ => &COMPOSER_T256,
    }
}

/// Decode one `(ch0, ch1)` value pair for the given composer descriptor.
pub fn decode_pair(desc: usize, input: &mut BitReader) -> Option<(u32, u32)> {
    let (sel, sym_bits, ext) = COMPOSER_DESC[desc];
    if sel == 0 {
        return None;
    }
    let t = table(sel);
    let mut code = 0u32;
    let mut sym = 0usize;
    let mut found = false;
    for ln in 1..=24u32 {
        code = (code << 1) | input.read(1)?;
        for (i, &(c, l)) in t.iter().enumerate() {
            if l as u32 == ln && c as u32 == code {
                sym = i;
                found = true;
                break;
            }
        }
        if found {
            break;
        }
    }
    if !found {
        return None;
    }
    let sym = sym as u32;
    let a = sym >> sym_bits;
    let b = sym & ((1 << sym_bits) - 1);
    let (r0, r1) = if ext > 0 {
        (input.read(ext as u32)?, input.read(ext as u32)?)
    } else {
        (0, 0)
    };
    Some(((a << ext) | r0, (b << ext) | r1))
}

/// The bits the composer writes for `(v0, v1)` under `desc` (test/validation
/// inverse of [`decode_pair`]).
pub fn pair_bits(desc: usize, v0: u32, v1: u32) -> Vec<bool> {
    let (sel, sym_bits, ext) = COMPOSER_DESC[desc];
    let t = table(sel);
    let idx =
        (((v0 >> ext) & ((1 << sym_bits) - 1)) << sym_bits) | ((v1 >> ext) & ((1 << sym_bits) - 1));
    let (code, len) = t[idx as usize];
    // widen before shifting: the tables hold up to 19-bit codes and `u16 >>
    // 18` is a masked shift in release builds
    let code = code as u32;
    let mut out = Vec::with_capacity(len as usize + 2 * ext as usize);
    for i in (0..len as u32).rev() {
        out.push((code >> i) & 1 == 1);
    }
    if ext > 0 {
        for i in (0..ext).rev() {
            out.push((v0 >> i) & 1 == 1);
        }
        for i in (0..ext).rev() {
            out.push((v1 >> i) & 1 == 1);
        }
    }
    out
}

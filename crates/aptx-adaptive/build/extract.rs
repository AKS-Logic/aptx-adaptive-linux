//! Locators and transforms of `spec/table-extraction.md` §3. Each table comes out in its §4
//! canonical form, keyed by table id.

use std::collections::BTreeMap;

use crate::elf::{Elf, Result, STT_FUNC, Sym, va_add};

pub type Tables = BTreeMap<String, Vec<u32>>;

/// How an array's elements are stored, and the Rust element type it's emitted as.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Elem {
    I8,
    I16,
    I32,
    /// int32 in the file, emitted as `u8`.
    U8,
    /// int32 in the file, emitted as `usize`.
    Usize,
}

impl Elem {
    fn width(self) -> u32 {
        match self {
            Elem::I8 => 1,
            Elem::I16 => 2,
            Elem::I32 | Elem::U8 | Elem::Usize => 4,
        }
    }

    pub fn rust(self) -> &'static str {
        match self {
            Elem::I8 => "i8",
            Elem::I16 => "i16",
            Elem::I32 => "i32",
            Elem::U8 => "u8",
            Elem::Usize => "usize",
        }
    }
}

#[derive(Clone, Copy)]
pub struct Locator {
    pub name: &'static str,
    pub size: u32,
}

const fn loc(name: &'static str, size: u32) -> Locator {
    Locator { name, size }
}

pub struct Array {
    pub id: &'static str,
    loc: Locator,
    pub elem: Elem,
    /// Zero elements the file doesn't store (§3.1).
    pad: usize,
    /// `None` for tables that are only verified.
    pub rust: Option<&'static str>,
}

const fn array(id: &'static str, loc: Locator, elem: Elem, rust: Option<&'static str>) -> Array {
    Array {
        id,
        loc,
        elem,
        pad: 0,
        rust,
    }
}

const fn padded(id: &'static str, loc: Locator, elem: Elem) -> Array {
    Array {
        id,
        loc,
        elem,
        pad: 1,
        rust: None,
    }
}

use Elem::{I8, I32, U8, Usize};

const TAPS_MUSIC_8: Locator = loc("g_nNumZeros_Music_8", 32);

#[rustfmt::skip]
pub const ARRAYS: &[Array] = &[
    array("filterbank_4band_tree.outer_16", loc("anOuterCoeffs_32", 64), I32, Some("OUTER_16")),
    array("filterbank_4band_tree.inner_16", loc("anInnerCoeffs_32", 64), I32, Some("INNER_16")),
    array("filterbank_4band_tree.outer_12", loc("anOuterCoeffs_24", 48), I32, Some("OUTER_12")),
    array("filterbank_4band_tree.inner_12", loc("anInnerCoeffs_24", 48), I32, Some("INNER_12")),
    array("filterbank_4band_tree.outer_16_classic", loc("anOuterCoeffs_32Classic", 64), I32, None),
    array("filterbank_4band_tree.inner_16_classic", loc("anInnerCoeffs_32Classic", 64), I32, None),
    padded("filterbank_4band_tree.outer_12_alt", loc("anOuterCoeffs_22", 44), I32),
    padded("filterbank_4band_tree.inner_12_alt", loc("anInnerCoeffs_22", 44), I32),
    array("filterbank_8band_prototype_256.coef", loc("CMFB_PRPD_Proto_Sym_256Tap_50PcOL", 1024), I32, Some("PROTO_256")),
    array("filterbank_8band_prototype_128.coef", loc("CMFB_PRPD_Proto_Sym_128Tap_50PcOL", 512), I32, Some("PROTO_128")),
    array("quantiser.log_table", loc("nu_logTable", 128), I32, Some("LOG_TABLE")),
    array("predictor_taps.8band_48k", loc("g_nNumZeros_Music_8_Reduced", 32), Usize, Some("TAPS_8BAND_48K")),
    array("predictor_taps.8band_96k", loc("g_nNumZeros_Music_8_96k", 32), Usize, Some("TAPS_8BAND_96K")),
    array("predictor_taps.8band_lowpower", loc("g_nNumZeros_Music_8_LowPower", 32), I32, None),
    array("predictor_taps.8band_full_unused", TAPS_MUSIC_8, I32, None),
    array("allocator.int8.offset_4band_stereo", loc("BitAllocOffset4StereoTable", 32), I8, None),
    array("allocator.int8.offset_4band_dual_mono", loc("BitAllocOffset4DualMonoTable", 32), I8, Some("OFFSET_4BAND_DUAL_MONO")),
    array("allocator.int8.offset_8band_stereo", loc("BitAllocOffset8StereoTable", 64), I8, None),
    array("allocator.int8.offset_8band_dual_mono", loc("BitAllocOffset8DualMonoTable", 80), I8, Some("OFFSET_8BAND_DUAL_MONO")),
    array("allocator.int8.offset_8band_dual_mono_tone", loc("BitAllocOffset8DualMonoToneTable", 80), I8, Some("OFFSET_8BAND_DUAL_MONO_TONE")),
    array("allocator.int32.max_bits_4band_max", loc("nMaxBitsCfg0_Max", 32), U8, Some("MAX_BITS_4BAND_MAX")),
    array("allocator.int32.max_bits_4band_370", loc("nMaxBitsCfg0_370", 32), U8, Some("MAX_BITS_4BAND_370")),
    array("allocator.int32.max_bits_4band_320", loc("nMaxBitsCfg0_320", 32), U8, Some("MAX_BITS_4BAND_320")),
    array("allocator.int32.max_bits_4band_250", loc("nMaxBitsCfg0_250", 32), U8, None),
    array("allocator.int32.max_bits_8band_max", loc("nMaxBitsCfg0_8Band_Max", 64), U8, Some("MAX_BITS_8BAND_MAX")),
    array("allocator.int32.max_bits_8band_520", loc("nMaxBitsCfg0_8Band_520", 64), U8, Some("MAX_BITS_8BAND_520")),
    array("allocator.int32.max_bits_8band_420", loc("nMaxBitsCfg0_8Band_420", 64), U8, Some("MAX_BITS_8BAND_420")),
    array("allocator.int32.max_bits_8band_370", loc("nMaxBitsCfg0_8Band_370", 64), U8, Some("MAX_BITS_8BAND_370")),
    array("allocator.int32.max_bits_8band_320", loc("nMaxBitsCfg0_8Band_320", 64), U8, Some("MAX_BITS_8BAND_320")),
    array("allocator.int32.max_bits_8band_250", loc("nMaxBitsCfg0_8Band_250", 64), U8, Some("MAX_BITS_8BAND_250")),
    array("allocator.int32.max_bits_8band_96k_max", loc("nMaxBitsCfg0_8Band_96k_Max", 64), U8, Some("MAX_BITS_8BAND_96K_MAX")),
    array("allocator.int32.max_bits_8band_96k_650", loc("nMaxBitsCfg0_8Band_96k_650", 64), U8, Some("MAX_BITS_8BAND_96K_650")),
    array("allocator.int32.max_bits_8band_96k_600", loc("nMaxBitsCfg0_8Band_96k_600", 64), U8, Some("MAX_BITS_8BAND_96K_600")),
    array("allocator.int32.max_bits_8band_96k_500", loc("nMaxBitsCfg0_8Band_96k_500", 64), U8, Some("MAX_BITS_8BAND_96K_500")),
    array("allocator.int32.max_bits_8band_96k_420", loc("nMaxBitsCfg0_8Band_96k_420", 64), U8, Some("MAX_BITS_8BAND_96K_420")),
];

pub struct Huffman {
    pub id: &'static str,
    loc: Locator,
    pub rust: &'static str,
}

#[rustfmt::skip]
pub const CODEWORD_TABLES: [Huffman; 4] = [
    Huffman { id: "codeword_huffman.tables.T1", loc: loc("g_aPacker_cfg0_HuffEcstasy_32_2_sqe_K0_0", 64), rust: "T1" },
    Huffman { id: "codeword_huffman.tables.T2", loc: loc("g_aPacker_cfg0_HuffHL2Bit2Sqe_0", 64), rust: "T2" },
    Huffman { id: "codeword_huffman.tables.T3", loc: loc("g_aPacker_cfg0_HuffLlevame_con_tu_babalao_48000_22_3_sqe_K0", 256), rust: "T3" },
    Huffman { id: "codeword_huffman.tables.T4", loc: loc("g_aPacker_cfg0_HuffMindwarp_32_7_sqe_k3_1", 1024), rust: "T4" },
];

#[rustfmt::skip]
const ABS_TABLES: [Huffman; 2] = [
    Huffman { id: "alloc_absolute_huffman.table_a", loc: loc("g_aPacker_cfg0_HuffBitAlloc_0_1", 548), rust: "ABS_A" },
    Huffman { id: "alloc_absolute_huffman.table_b", loc: loc("g_aPacker_cfg0_HuffBitAlloc_2_3", 548), rust: "ABS_B" },
];
const PAIR_TABLES: [(&str, Locator); 2] = [
    (
        "alloc_absolute_huffman.pair_table_8band",
        loc("g_aPacker_cfg0_Tables_cfg0BitAlloc_8Band", 32),
    ),
    (
        "alloc_absolute_huffman.pair_table_4band",
        loc("g_aPacker_cfg0_Tables_cfg0BitAlloc_4Band", 32),
    ),
];
const DIFF_TABLE_COUNT: u32 = 9;
const DIFF_TABLE_SIZE: u32 = 64;
const DIFF_INDEX: Locator = loc("g_aPacker_cfg0_HuffDifVal", 64);
const DIFF_RANGE: Locator = loc("g_aPacker_cfg0_RangeDifVal", 128);

const COMPOSER: Locator = loc("g_axPackerCfg0ComposerTable", 204);
const COMPOSER_RECORDS: u32 = 17;
const COMPOSER_RECORD_SIZE: u32 = 12;

const F_DCT: Locator = loc("CmfbPRPD_RunDCT2_N8", 424);
const F_MOD: Locator = loc(
    "CmfbPRPDAnalysisFilter_RunFastCosineModulation_EvenPolyPrototypeLen",
    448,
);
const F_MOD_DELTA: u32 = 0x98C;
const DCT_DELTAS: [u32; 7] = [0x08, 0x18, 0x38, 0x28, 0xA8, 0x98, 0xE0];
/// `(diff_const, sum_const)` deltas from `F_MOD`, per group.
const FRONTEND_DELTAS: [(u32, u32); 4] = [
    (0x008, 0x000),
    (0x078, 0x068),
    (0x094, 0x0C0),
    (0x120, 0x108),
];

struct Reader<'a> {
    elf: &'a Elf<'a>,
    known_good: bool,
}

impl Reader<'_> {
    fn check(name: &str, sym: Option<Sym>, size: u32) -> Result<Sym> {
        let sym = sym.ok_or_else(|| format!("symbol `{name}` not found"))?;
        if sym.size != size {
            return Err(format!(
                "symbol `{name}` has st_size {}, expected {size}",
                sym.size
            ));
        }
        Ok(sym)
    }

    fn sym(&self, loc: Locator) -> Result<Sym> {
        self.named(loc.name, loc.size)
    }

    fn named(&self, name: &str, size: u32) -> Result<Sym> {
        Self::check(name, self.elf.dynsym(name), size)
    }

    fn raw_named(&self, name: &str, size: u32) -> Result<&[u8]> {
        self.elf
            .bytes_at_va(self.named(name, size)?.value, size as usize)
    }

    fn ints(&self, loc: Locator, elem: Elem) -> Result<Vec<u32>> {
        self.ints_named(loc.name, loc.size, elem)
    }

    /// Sign-extended elements, as u32.
    fn ints_named(&self, name: &str, size: u32, elem: Elem) -> Result<Vec<u32>> {
        Ok(self
            .raw_named(name, size)?
            .chunks_exact(elem.width() as usize)
            .map(|b| match *b {
                [x] => i32::from(x.cast_signed()).cast_unsigned(),
                [lo, hi] => i32::from(i16::from_le_bytes([lo, hi])).cast_unsigned(),
                _ => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            })
            .collect())
    }

    /// `code, len` per entry (§3.6), and the symbol, for matching pointers to it.
    fn huffman(&self, name: &str, size: u32) -> Result<(Vec<u32>, Sym)> {
        let sym = self.named(name, size)?;
        let codes = self
            .elf
            .bytes_at_va(sym.value, size as usize)?
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|&[lo, hi, len, _]| [u32::from(u16::from_le_bytes([lo, hi])), u32::from(len)])
            .collect();
        Ok((codes, sym))
    }

    /// Index into `targets` of the table the pointer slot at `va` points to (§2).
    fn pointer_to(&self, va: u32, targets: &[Sym], what: &str) -> Result<u32> {
        let target = self.elf.pointer(va)?;
        targets
            .iter()
            .position(|s| s.value == target)
            .map(|i| i as u32)
            .ok_or_else(|| {
                format!("{what}: pointer slot at {va:#x} points to none of the expected tables")
            })
    }

    fn pointers(&self, loc: Locator, targets: &[Sym]) -> Result<Vec<u32>> {
        let base = self.sym(loc)?.value;
        (0..loc.size / 4)
            .map(|p| self.pointer_to(va_add(base, 4 * p)?, targets, loc.name))
            .collect()
    }

    fn function(loc: Locator, sym: Option<Sym>) -> Result<u32> {
        let sym = Self::check(loc.name, sym, loc.size)?;
        if sym.kind != STT_FUNC {
            return Err(format!("symbol `{}` is not a function", loc.name));
        }
        Ok(sym.value)
    }

    /// A constant extender and the instruction after it (§3.3).
    fn immediate(&self, base: u32, delta: u32) -> Result<u32> {
        let va = va_add(base, delta)?;
        let w0 = self.elf.u32_at_va(va)?;
        let w1 = self.elf.u32_at_va(va_add(va, 4)?)?;
        if w0 >> 28 != 0 {
            return Err(format!("no constant extender at {va:#x}"));
        }
        let hi = ((((w0 >> 16) & 0xFFF) << 14) | (w0 & 0x3FFF)) << 6;
        let lo = if w1 & 0xC000 == 0 {
            (w1 >> 20) & 0x3F
        } else if w1 >> 24 == 0x78 {
            (w1 >> 5) & 0x3F
        } else {
            return Err(format!(
                "instruction after the extender at {va:#x} is not a transfer immediate"
            ));
        };
        Ok(hi | lo)
    }

    /// `F_MOD` is the one symbol only `.symtab` has (§2, §3.3).
    fn f_mod(&self, f_dct: u32) -> Result<u32> {
        let fallback = va_add(f_dct, F_MOD_DELTA)?;
        match self.elf.symtab(F_MOD.name) {
            Some(sym) => {
                let f_mod = Self::function(F_MOD, Some(sym))?;
                if self.known_good && f_mod != fallback {
                    return Err(format!(
                        "`{}` isn't where the known-good build has it (§6)",
                        F_MOD.name
                    ));
                }
                Ok(f_mod)
            }
            None if self.known_good => Ok(fallback),
            None => Err(format!(
                "`{}` isn't in .symtab (stripped?), and the §3.3 fallback offset is only valid \
                 for the known-good build",
                F_MOD.name
            )),
        }
    }

    fn modulation(&self, t: &mut Tables) -> Result<()> {
        let f_dct = Self::function(F_DCT, self.elf.dynsym(F_DCT.name))?;
        let f_mod = self.f_mod(f_dct)?;
        let dct = DCT_DELTAS
            .iter()
            .map(|&d| self.immediate(f_dct, d))
            .collect::<Result<_>>()?;
        let mut frontend = Vec::new();
        for (g, (diff, sum)) in (0u32..4).zip(FRONTEND_DELTAS) {
            let (diff_out, sum_out) = if g % 2 == 0 {
                (4 + g / 2, g / 2)
            } else {
                (4 - g.div_ceil(2), 7 - (g - 1) / 2)
            };
            frontend.extend([8 + g, 15 - g, 7 - g, g, diff_out]);
            frontend.extend([
                self.immediate(f_mod, diff)?,
                sum_out,
                self.immediate(f_mod, sum)?,
            ]);
        }
        t.insert("filterbank_8band_modulation.dct".into(), dct);
        t.insert("filterbank_8band_modulation.frontend".into(), frontend);
        Ok(())
    }

    fn quantiser(&self, t: &mut Tables) -> Result<()> {
        let mut logdelta_max = Vec::new();
        for bt in 2u32..=9 {
            let stored = (1 << (bt - 1)) + 1;
            for (key, name, elem) in [
                ("threshold", format!("nu_dq{bt}bit16"), I32),
                ("dither", format!("nu_dq{bt}dith16"), I32),
                ("logdelta_incr", format!("nu_q{bt}incr16"), Elem::I16),
            ] {
                let mut v = self.ints_named(&name, stored * elem.width(), elem)?;
                v.push(0);
                t.insert(format!("quantiser.tables.{bt}.{key}"), v);
            }
            let max = self.ints_named(&format!("nMaxLDBuf{}", bt - 1), 4, I32)?[0];
            if max != 6399 - 256 * bt {
                return Err(format!(
                    "quantiser.logdelta_max for Bt {bt} doesn't follow the 6399 − 256·Bt rule"
                ));
            }
            logdelta_max.push(max);
        }
        t.insert("quantiser.logdelta_max".into(), logdelta_max);
        Ok(())
    }

    fn alloc_huffman(&self, t: &mut Tables) -> Result<()> {
        let mut abs = Vec::new();
        for table in &ABS_TABLES {
            let (mut codes, sym) = self.huffman(table.loc.name, table.loc.size)?;
            codes.extend([0, 0]);
            t.insert(table.id.into(), codes);
            abs.push(sym);
        }
        for (id, loc) in PAIR_TABLES {
            t.insert(id.into(), self.pointers(loc, &abs)?);
        }

        let mut tables = Vec::new();
        let mut syms = Vec::new();
        for k in 0..DIFF_TABLE_COUNT {
            let (codes, sym) =
                self.huffman(&format!("{}_{k}", DIFF_INDEX.name), DIFF_TABLE_SIZE)?;
            tables.extend(codes);
            syms.push(sym);
        }
        t.insert("alloc_diff_huffman.tables".into(), tables);
        t.insert(
            "alloc_diff_huffman.table_index".into(),
            self.pointers(DIFF_INDEX, &syms)?,
        );
        t.insert(
            "alloc_diff_huffman.range".into(),
            self.ints(DIFF_RANGE, I32)?,
        );
        Ok(())
    }

    fn codewords(&self, t: &mut Tables) -> Result<()> {
        let mut syms = Vec::new();
        for table in &CODEWORD_TABLES {
            let (codes, sym) = self.huffman(table.loc.name, table.loc.size)?;
            t.insert(table.id.into(), codes);
            syms.push(sym);
        }
        let composer = self.sym(COMPOSER)?.value;
        let mut entries = Vec::new();
        for b in 0..COMPOSER_RECORDS {
            let rec = va_add(composer, b * COMPOSER_RECORD_SIZE)?;
            let table = if self.elf.pointer(rec)? == 0 {
                0
            } else {
                self.pointer_to(rec, &syms, COMPOSER.name)? + 1
            };
            let shifts = self.elf.u32_at_va(va_add(rec, 4)?)?;
            let mask = self.elf.u32_at_va(va_add(rec, 8)?)?;
            entries.extend([table, shifts & 0xFF, (shifts >> 8) & 0xFF, mask & 0xFF]);
        }
        t.insert("codeword_huffman.entries".into(), entries);
        Ok(())
    }
}

/// Every §5 table except `period_interleaved`, which isn't in the binary.
pub fn extract(elf: &Elf, known_good: bool) -> Result<Tables> {
    let r = Reader { elf, known_good };
    let mut t = Tables::new();
    for a in ARRAYS {
        let mut v = r.ints(a.loc, a.elem)?;
        v.resize(v.len() + a.pad, 0);
        t.insert(a.id.into(), v);
    }
    let taps = r.ints(TAPS_MUSIC_8, I32)?;
    t.insert(
        "predictor_taps.4band".into(),
        taps.into_iter().step_by(2).collect(),
    );
    r.modulation(&mut t)?;
    r.quantiser(&mut t)?;
    r.alloc_huffman(&mut t)?;
    r.codewords(&mut t)?;
    Ok(t)
}

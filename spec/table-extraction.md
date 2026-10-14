# Table extraction from the vendor encoder library

Status: dirty-side handoff, 2026-10-15. This spec lets a build script read every numeric table in `spec/tables/*.json`, and every constant array `scripts/gen-tables.py` writes into `codec/tables.rs`, out of a copy of the vendor library that the user supplies.

The document contains no table values. Each table is described by a locator, a layout, a transform and a SHA-256 of its canonical form (§4). The dirty side checked the spec with a throwaway extractor written only from this document. Every hash matched on the known-good build, and also on a copy with `.symtab` stripped (§6).

---

## 1. Supported binaries

| item | value |
|---|---|
| file name on device | `libaptXAdaptiveEnc.so` (DT_SONAME `libaptXAdaptiveEnc-shared.so`) |
| device path | `/vendor/dsp/adsp/libaptXAdaptiveEnc.so`. On Qualcomm phones `/vendor/dsp` is the mount point of the separate `dsp` partition. |
| format | ELF32, little-endian, `ET_DYN`, `e_machine` = 164 (`EM_QDSP6`, Hexagon), `e_flags` = 0x68 |
| what it is | aptX Adaptive R1/R2.x encoder (the `aptXEncode_*` API). The same file also contains aptX classic and HD (`aptxbtenc_*`, `aptxhdbtenc_*`). |
| how to get it | `adb pull /vendor/dsp/adsp/libaptXAdaptiveEnc.so` from a Snapdragon phone with aptX Adaptive. Alternatively, take the `dsp` partition image (ext4) from the phone's full OTA `payload.bin` and copy `adsp/libaptXAdaptiveEnc.so` out of it. |

**Known-good builds:**

| SHA-256 of the whole file | size (bytes) | origin |
|---|---|---|
| `1ac881571cd1db4d256113d4448304016de73fedebc059824b1b2ed09599f0c9` | 271424 | OnePlus 15 (CPH2749) full OTA, build B.R4T3, SPL 2026-02, `dsp` partition, `adsp/libaptXAdaptiveEnc.so` |

**Not supported:** `libaptXAdaptiveEnc3.so` (R3) and `libaptXAdaptiveEnc4.so` (R4) have some of the same symbol names, but both lack several of the tables below: the 4-band filterbank, the predictor tap tables, the 44.1/48 kHz max-bits tables and the per-channel-config offset tables. Their allocator tables are also organised differently. Accept only `libaptXAdaptiveEnc.so`.

**Build-script policy (recommended).**

1. Check the ELF header fields above.
2. If the file hash is in the known-good list, use every locator below as written.
3. If the hash is unknown, still try. Every table except the two in §3.3 is found by `.dynsym` name, which is likely to survive a rebuild. Then require every table hash in §5 to match. An unknown build that passes all of them yields the same tables, so it is safe to use.

---

## 2. General ELF rules

- **Address to file offset:** map a virtual address through the `PT_LOAD` program headers: `off = va − p_vaddr + p_offset`. In the known-good build both `PT_LOAD` segments have `p_vaddr == p_offset`, so an address equals its file offset. Still map through the headers anyway.
- **Symbols:** look each name up in `.dynsym` (via `.dynstr`) first. All locators but one are in `.dynsym`, and that table can't be stripped without breaking the library. Fall back to `.symtab` (via `.strtab`) for the one symbol that only exists there (§3.3). A symbol's `st_value` is a virtual address and its `st_size` is the object size in bytes. Every table locator below states the `st_size` it expects; reject a mismatch.
- **Pointers:** some tables are arrays of pointers (§3.6, §3.7, §3.8), which live in `.data` / `.data.rel.ro`. Resolve each 4-byte slot at virtual address `p` like this:
  - If `.rela.dyn` has an entry with `r_offset == p` and type 6 (`R_HEX_32`), the value is `st_value(dynsym[r_info >> 8]) + r_addend`.
  - Otherwise, if it has type 35 (`R_HEX_RELATIVE`), the value is `r_addend`.
  - Otherwise, read the stored little-endian u32. In the known-good build the stored words already equal the relocated values; that was checked for every slot used.
  - The resolved value is a virtual address. Identify the table it points to by comparing it with the `st_value` of the candidate symbols. Never compare table contents.
- **Endianness:** all multi-byte reads are little-endian.

---

## 3. Per-table locators, layouts and transforms

The JSON key paths below are as in `spec/tables/<file>.json`. "Generated" names the constant `gen-tables.py` writes. "n" is the element count in the JSON. Where the binary holds fewer elements than the JSON, the transform says what to append.

### 3.1 `filterbank_4band_tree.json`

All are `int32` arrays with stride 4, in `.rodata`.

| JSON key | symbol | st_size | binary elements | transform | generated |
|---|---|---|---|---|---|
| `outer_16` | `anOuterCoeffs_32` | 64 | 16 | none | `OUTER_16` |
| `inner_16` | `anInnerCoeffs_32` | 64 | 16 | none | `INNER_16` |
| `outer_12` | `anOuterCoeffs_24` | 48 | 12 | none | `OUTER_12` |
| `inner_12` | `anInnerCoeffs_24` | 48 | 12 | none | `INNER_12` |
| `outer_16_classic` | `anOuterCoeffs_32Classic` | 64 | 16 | none | (not emitted) |
| `inner_16_classic` | `anInnerCoeffs_32Classic` | 64 | 16 | none | (not emitted) |
| `outer_12_alt` | `anOuterCoeffs_22` | 44 | 11 | append one element of value 0 (n = 12) | (not emitted) |
| `inner_12_alt` | `anInnerCoeffs_22` | 44 | 11 | append one element of value 0 (n = 12) | (not emitted) |

### 3.2 `filterbank_8band_prototype_256.json` / `_128.json`

| JSON key | symbol | st_size | layout | transform | generated |
|---|---|---|---|---|---|
| `filterbank_8band_prototype_256.coef` | `CMFB_PRPD_Proto_Sym_256Tap_50PcOL` | 1024 | 256 × int32 | none | `PROTO_256` |
| `filterbank_8band_prototype_128.coef` | `CMFB_PRPD_Proto_Sym_128Tap_50PcOL` | 512 | 128 × int32 | none | `PROTO_128` |

The `format` string field (`"int32"`) is not numeric. The clean side keeps it as is.

### 3.3 `filterbank_8band_modulation.json` (code immediates, not a data table)

These constants are not stored as data. They are 32-bit immediates inside two functions, so the locator is **function symbol + byte delta to an instruction pair**.

**Instruction-pair decoding (Hexagon constant extender).** At `va = st_value(function) + delta`, read two little-endian u32 words, `w0` (at va) and `w1` (at va + 4).

1. `w0` must be a constant extender: bits 31..28 of `w0` are 0. Otherwise reject the build.
2. The extender's 26-bit payload is `x = (((w0 >> 16) & 0xFFF) << 14) | (w0 & 0x3FFF)`, and it gives the upper 26 bits of the constant: `hi = x << 6`.
3. `w1` holds the low 6 bits. There are two encodings:
   - If bits 15..14 of `w1` are not both 0, it is a normal "transfer immediate" instruction. Check that bits 31..24 of `w1` equal 0x78, then take `lo = (w1 >> 5) & 0x3F`.
   - If bits 15..14 of `w1` are both 0, it is a duplex packet whose high sub-instruction loads the immediate. Take `lo = (w1 >> 20) & 0x3F`.
4. The constant is `(hi | lo) mod 2^32`.

Function anchors:

- `F_DCT` = `CmfbPRPD_RunDCT2_N8`, in `.dynsym`, FUNC, st_size 424.
- `F_MOD` = `CmfbPRPDAnalysisFilter_RunFastCosineModulation_EvenPolyPrototypeLen`, which is LOCAL and in `.symtab` only (st_size 448). If `.symtab` is missing, use `F_MOD = st_value(F_DCT) + 0x98C`. That delta holds for the known-good build only.

**`dct.c1` … `dct.c7`** (generated as `DCT`, `[i32; 7]`, ordered K1..K7). Each `c_k` is the decoded constant at `F_DCT + delta_k`, sign-extended from 32 bits (the JSON stores these as signed).

| key | delta from `F_DCT` |
|---|---|
| c1 | 0x08 |
| c2 | 0x18 |
| c3 | 0x38 |
| c4 | 0x28 |
| c5 | 0xA8 |
| c6 | 0x98 |
| c7 | 0xE0 (duplex encoding) |

**`frontend[g]`, g = 0..3** (generated as `FRONTEND`). There are two kinds of field.

The index fields are code structure and do not come from data. Use these rules:

- `u_add = [8 + g, 15 − g]`
- `u_sub = [7 − g, g]`
- `diff_out = 4 + g/2` for even g, and `4 − (g+1)/2` for odd g
- `sum_out = g/2` for even g, and `7 − (g−1)/2` for odd g

The constant fields are decoded at `F_MOD + delta` and kept as **unsigned** 32-bit (the JSON stores them unsigned):

| g | `diff_const` delta | `sum_const` delta |
|---|---|---|
| 0 | 0x008 | 0x000 (duplex encoding) |
| 1 | 0x078 | 0x068 |
| 2 | 0x094 | 0x0C0 |
| 3 | 0x120 | 0x108 |

### 3.4 `quantiser.json`

For each table size Bt = 2..9, n = 2^(Bt−1) + 2. The binary holds n − 1 elements, and the JSON's last element is a zero sentinel the binary doesn't store. Append one element of value 0 to each of the three arrays.

| JSON key | symbol | type | st_size | transform |
|---|---|---|---|---|
| `tables.<Bt>.threshold` | `nu_dq<Bt>bit16` | int32 | 4·(n−1) | append 0 |
| `tables.<Bt>.dither` | `nu_dq<Bt>dith16` | int32 | 4·(n−1) | append 0 |
| `tables.<Bt>.logdelta_incr` | `nu_q<Bt>incr16` | int16, stride 2 | 2·(n−1) | sign-extend each to the output type, then append 0 |
| `log_table` | `nu_logTable` | int32 | 128 (32 elements) | none |
| `logdelta_max.<Bt>` | `nMaxLDBuf<Bt−1>` (so Bt 2 → `nMaxLDBuf1`, …, Bt 9 → `nMaxLDBuf8`) | int32 scalar | 4 | none. It must also equal `6399 − 256·Bt`, the rule already in the JSON and in codec-core §5.4 |

Notes:

- `logdelta_max_rule` is a prose string and isn't extracted.
- Don't confuse these symbols with similarly named ones. `nu_dq<Bt>bit16sum`, `nu_dq<Bt>dith16_diff_rshift1`, `nu_dq4bit24`/`nu_dq5bit24`, `nu_q4incr24`/`nu_q5incr24` and `nMaxLDBuf9` are different tables and are not used.
- Generated: `QUANT[Bt − 2] = { threshold, dither, incr }` with types `i32`/`i32`/`i16`, and `LOG_TABLE`. `logdelta_max` is not emitted; the code uses the formula.

### 3.5 `predictor_taps.json`

All are int32, 8 elements, st_size 32, in `.data`.

| JSON key | symbol | transform | generated |
|---|---|---|---|
| `8band_48k` | `g_nNumZeros_Music_8_Reduced` | none | `TAPS_8BAND_48K` (`usize`) |
| `8band_96k` | `g_nNumZeros_Music_8_96k` | none | `TAPS_8BAND_96K` |
| `8band_lowpower` | `g_nNumZeros_Music_8_LowPower` | none | (not emitted) |
| `8band_full_unused` | `g_nNumZeros_Music_8` | none | (not emitted) |
| `4band` (n = 4) | `g_nNumZeros_Music_8` | **subset:** elements 0, 2, 4, 6, in that order | `TAPS_4BAND` |

The 4-band counts aren't a table in the binary. The library sets them with small immediates in `SSLMS_Encode_Init`. `g_nNumZeros_Music_8` is the same per-band-pair design with each count repeated for two adjacent bands, so the even-index subset reproduces the 4-band counts exactly. The §5 hash checks this.

### 3.6 `alloc_absolute_huffman.json`

**Huffman entry layout (also used in §3.7 and §3.8).** Each entry is 4 bytes:

| bytes | field |
|---|---|
| 0..1 | u16 `code` |
| 2 | u8 `len` |
| 3 | padding, always 0 |

The JSON `{code, len}` maps directly onto these fields. `len` = 0 means an invalid symbol.

| JSON key | symbol | st_size | binary entries | transform | generated |
|---|---|---|---|---|---|
| `table_a` | `g_aPacker_cfg0_HuffBitAlloc_0_1` | 548 | 137 | append one `{code 0, len 0}` (n = 138) | `ABS_A` |
| `table_b` | `g_aPacker_cfg0_HuffBitAlloc_2_3` | 548 | 137 | append one `{code 0, len 0}` (n = 138) | `ABS_B` |
| `pair_table_8band` | `g_aPacker_cfg0_Tables_cfg0BitAlloc_8Band` (`.data`) | 32 | 8 pointers | see below | `PAIR_TABLE_8BAND` |
| `pair_table_4band` | `g_aPacker_cfg0_Tables_cfg0BitAlloc_4Band` (`.data`) | 32 | 8 pointers | see below | `PAIR_TABLE_4BAND` |

**Pair tables.** Resolve each of the 8 pointer slots (§2). A pointer to `g_aPacker_cfg0_HuffBitAlloc_0_1` maps to the string `"A"`; a pointer to `g_aPacker_cfg0_HuffBitAlloc_2_3` maps to `"B"`. Any other target is an error. The generated Rust stores `true` for `"B"`.

### 3.7 `alloc_diff_huffman.json`

| JSON key | locator | transform | generated |
|---|---|---|---|
| `tables[k]`, k = 0..8 | `g_aPacker_cfg0_HuffDifVal_<k>`, 16 Huffman entries each (st_size 64, layout as in §3.6) | none | `DIFF[k]` |
| `table_index[p]`, p = 0..15 | `g_aPacker_cfg0_HuffDifVal` (`.data`, st_size 64): 16 pointer slots | slot p points to `g_aPacker_cfg0_HuffDifVal_<k>`; the value is k | `DIFF_INDEX` |
| `range[p]` | `g_aPacker_cfg0_RangeDifVal` (`.rodata`, st_size 128): 32 × int32 | **de-interleave:** `range[p] = [elem[2p], elem[2p+1]]` (lo, hi) | `DIFF_RANGE` |

### 3.8 `codeword_huffman.json`

**Tables** (Huffman entries as in §3.6, no transform):

| JSON key | symbol | st_size | entries |
|---|---|---|---|
| `tables.T1` | `g_aPacker_cfg0_HuffEcstasy_32_2_sqe_K0_0` | 64 | 16 |
| `tables.T2` | `g_aPacker_cfg0_HuffHL2Bit2Sqe_0` | 64 | 16 |
| `tables.T3` | `g_aPacker_cfg0_HuffLlevame_con_tu_babalao_48000_22_3_sqe_K0` | 256 | 64 |
| `tables.T4` | `g_aPacker_cfg0_HuffMindwarp_32_7_sqe_k3_1` | 1024 | 256 |

**`entries[b]`, b = 0..16** (generated as `CODEWORD`). The source is `g_axPackerCfg0ComposerTable` (`.data.rel.ro`, st_size 204), an array of 17 records of 12 bytes. Record b sits at `st_value + 12·b`:

| record offset | type | maps to |
|---|---|---|
| +0 | pointer slot (§2) | `table`: 0 means `null`; otherwise the T-name of the symbol it points to (the four symbols above) |
| +4 | u32 | `pair_shift` = bits 7..0; `raw_lsbs` = bits 15..8. Bits 31..16 hold a raw-LSB mask that is not in the JSON; ignore it. |
| +8 | u32 | `msb_mask` = bits 7..0. Ignore the other bits. |

The JSON field `bits` is just the record index b.

### 3.9 `allocator.json`

All arrays are in `.rodata` and have no transform. The `int8` arrays are signed bytes.

| JSON key | symbol | type | st_size | generated |
|---|---|---|---|---|
| `int8.offset_4band_stereo` | `BitAllocOffset4StereoTable` | int8 | 32 | (not emitted) |
| `int8.offset_4band_dual_mono` | `BitAllocOffset4DualMonoTable` | int8 | 32 | `OFFSET_4BAND_DUAL_MONO` (`i8`) |
| `int8.offset_8band_stereo` | `BitAllocOffset8StereoTable` | int8 | 64 | (not emitted) |
| `int8.offset_8band_dual_mono` | `BitAllocOffset8DualMonoTable` | int8 | 80 | `OFFSET_8BAND_DUAL_MONO` |
| `int8.offset_8band_dual_mono_tone` | `BitAllocOffset8DualMonoToneTable` | int8 | 80 | `OFFSET_8BAND_DUAL_MONO_TONE` |
| `int32.max_bits_4band_max` | `nMaxBitsCfg0_Max` | int32 | 32 | `MAX_BITS_4BAND_MAX` (`u8`) |
| `int32.max_bits_4band_370` | `nMaxBitsCfg0_370` | int32 | 32 | `MAX_BITS_4BAND_370` |
| `int32.max_bits_4band_320` | `nMaxBitsCfg0_320` | int32 | 32 | `MAX_BITS_4BAND_320` |
| `int32.max_bits_4band_250` | `nMaxBitsCfg0_250` | int32 | 32 | (not emitted) |
| `int32.max_bits_8band_max` | `nMaxBitsCfg0_8Band_Max` | int32 | 64 | `MAX_BITS_8BAND_MAX` |
| `int32.max_bits_8band_520` | `nMaxBitsCfg0_8Band_520` | int32 | 64 | `MAX_BITS_8BAND_520` |
| `int32.max_bits_8band_420` | `nMaxBitsCfg0_8Band_420` | int32 | 64 | `MAX_BITS_8BAND_420` |
| `int32.max_bits_8band_370` | `nMaxBitsCfg0_8Band_370` | int32 | 64 | `MAX_BITS_8BAND_370` |
| `int32.max_bits_8band_320` | `nMaxBitsCfg0_8Band_320` | int32 | 64 | `MAX_BITS_8BAND_320` |
| `int32.max_bits_8band_250` | `nMaxBitsCfg0_8Band_250` | int32 | 64 | `MAX_BITS_8BAND_250` |
| `int32.max_bits_8band_96k_max` | `nMaxBitsCfg0_8Band_96k_Max` | int32 | 64 | `MAX_BITS_8BAND_96K_MAX` |
| `int32.max_bits_8band_96k_650` | `nMaxBitsCfg0_8Band_96k_650` | int32 | 64 | `MAX_BITS_8BAND_96K_650` |
| `int32.max_bits_8band_96k_600` | `nMaxBitsCfg0_8Band_96k_600` | int32 | 64 | `MAX_BITS_8BAND_96K_600` |
| `int32.max_bits_8band_96k_500` | `nMaxBitsCfg0_8Band_96k_500` | int32 | 64 | `MAX_BITS_8BAND_96K_500` |
| `int32.max_bits_8band_96k_420` | `nMaxBitsCfg0_8Band_96k_420` | int32 | 64 | `MAX_BITS_8BAND_96K_420` |

Some tables share content with others: one 8-band max-bits table equals another, and one 4-band table appears inside a longer one. That is expected. Locate tables by symbol, never by content.

Do not use the similarly named `BitAllocOffset_96k_*`, `g_nMaxBitsCfg0_2_*`, `g_nBitsCfg0_2_*` or `g_n8Perceptuallyweight_flat` tables. They are not in the JSON.

### 3.10 `period_interleaved.json`: not in the binary

The wrapper computes this value; there is no table. Rule: for each rate in {44100, 48000, 96000} and each `period_ms` key, `periods[rate][period_ms] = floor(period_ms · rate · 2 / 1000)`, in exact integer arithmetic.

The keys are every distinct period that appears in the HQ and LL mode tables (`a2dp-integration.md` §4.1 and §4.2, the same rows as `crates/aptx-adaptive/src/tables.rs`). Write each key as a decimal string and sort them ascending. `gen-tables.py` does not emit this file.

### 3.11 Non-numeric fields

`note`, `format` and `logdelta_max_rule` are prose and are not extracted. The clean side keeps that text in the generator, or drops it.

---

## 4. Canonical form for hashing

Each **table id** below has one canonical byte string:

> Take the post-transform integers in the order given, map each to `value mod 2^32`, and write it as a little-endian u32. Concatenate them all.

So every element is 4 bytes whatever its source type: an int8 or int16 is sign-extended first, a negative value becomes its 32-bit two's complement, and an unsigned constant is written as is. The canonical form doesn't depend on whether the JSON spells a constant signed or unsigned.

Orders:

- **Plain arrays:** in index order. This covers every array in §3.1, §3.2, §3.4 (except `logdelta_max`), §3.5 and §3.9.
- **`quantiser.logdelta_max`:** the values for Bt = 2, 3, …, 9.
- **`filterbank_8band_modulation.dct`:** c1, c2, …, c7.
- **`filterbank_8band_modulation.frontend`:** for g = 0..3, the 8 values `u_add[0], u_add[1], u_sub[0], u_sub[1], diff_out, diff_const, sum_out, sum_const` (32 values).
- **Huffman tables** (`table_a`, `table_b`, `codeword_huffman.tables.T1`…`T4`): for each entry, `code` then `len`.
- **`alloc_diff_huffman.tables`:** for k = 0..8, then for each of the 16 entries of table k, `code` then `len` (288 values).
- **`pair_table_8band` / `pair_table_4band`:** `"A"` → 0 and `"B"` → 1, in pair order.
- **`alloc_diff_huffman.range`:** for p = 0..15, lo then hi.
- **`codeword_huffman.entries`:** for b = 0..16, the 4 values `table_id, pair_shift, raw_lsbs, msb_mask`, where `table_id` is 0 for null and 1..4 for T1..T4. `bits` is not included; it equals b.
- **`period_interleaved.periods`:** rates 44100, 48000, 96000 in that order, then within each rate the periods in ascending numeric order.

---

## 5. Hashes

SHA-256 of the canonical form (§4). The dirty side computed every hash twice and got the same result both times: once from the current `aptx-linux/spec/tables/*.json` (main @ `1634c90`), and once from the known-good binary using only the locators above.

| table id | SHA-256 |
|---|---|
| `filterbank_4band_tree.outer_16` | `0f2b7814ba28211f30b390707b17bb48f562cb9924d977f77fda9ad6b876d1fb` |
| `filterbank_4band_tree.inner_16` | `751b6796d1d8f83399595c97f45aef18f66ccfe52eda56ccda5343392f19f858` |
| `filterbank_4band_tree.outer_12` | `cd87aced32a09a32a7ccd1976702a0d7506fe74dfd2f67cfb23cd3598cfdd260` |
| `filterbank_4band_tree.inner_12` | `0b450a933fe43ac888f9c37c284113a5dbd77aaa0ee8f43b43fcdf9fe88188d3` |
| `filterbank_4band_tree.outer_16_classic` | `d12bd267bc396aa31dde8653dfc15d3ac90e8bb96c1f111050278de00141acec` |
| `filterbank_4band_tree.inner_16_classic` | `a5fb6b832f2a3b1a4939ecab4b4c1e66d97e055ef303bffb970558d55c07fe13` |
| `filterbank_4band_tree.outer_12_alt` | `66c49d8d02c2b0001b0dd95ac17d8ac537b07fee293003c1ffa20a7e2e7d20ea` |
| `filterbank_4band_tree.inner_12_alt` | `373298a65c73b4798c3fa5505250e1222f88420d866024f91115e275ae49789b` |
| `filterbank_8band_prototype_256.coef` | `3b87ed8dd5dc2532534a31ab4ce8c77bd3e4ac528af1e9f22346116dfb30b299` |
| `filterbank_8band_prototype_128.coef` | `35d85d45532e1805788808aa68eb1db02199a4989324724f6656df622aa7bbb0` |
| `filterbank_8band_modulation.frontend` | `f40bc122f57dcbe5fc974e4061e6df55dce243517cced7d1e0b9bc1c6b67dc72` |
| `filterbank_8band_modulation.dct` | `a5879113d756fa770205934d8ee5d96944dede7d466988c05fc38e4fb4629596` |
| `quantiser.tables.2.threshold` | `95b9d4e1f007b0996c1b88cb306fbe0bda26ca016e42ab97229dd41b1ffea4ec` |
| `quantiser.tables.2.dither` | `32d233939dc3e0493e1dda2279d4efe7e68dd4c17e37d3eecb23ba5809c99679` |
| `quantiser.tables.2.logdelta_incr` | `88b247483eb6fcfd7663c3e273a4b057f9197b35db198b25590006640bd6507e` |
| `quantiser.tables.3.threshold` | `5d1dbd725d01d62fd7360804c9285f9bc6469db5317dcb5441de772cea9fa825` |
| `quantiser.tables.3.dither` | `b125d3e9f43cf91f0c6b7f01d7d41ebbfbe6ea5125cab6f0bc94fc4f655afdf3` |
| `quantiser.tables.3.logdelta_incr` | `830aef381e93aeacbb8ea10663712ec0b75d25d62a164dbe39e0fbd5a1b65cab` |
| `quantiser.tables.4.threshold` | `6c22cccead9e22120213f4a188ca7e5c7f2ddf77b2fd2fdd0b0caa2c95d92a63` |
| `quantiser.tables.4.dither` | `770146e654fc68e2231841c5b91e11f7a4d3f25a5eab5d409d389b06a9438243` |
| `quantiser.tables.4.logdelta_incr` | `f2f6013370bb53434f7e6ed1a5422e31df9d95fd9ffe43947b1e0e5211aa6fa4` |
| `quantiser.tables.5.threshold` | `e0e8513b45cba54bf720c0d017d64d0d30d3f16ae66034f3c16662c15c32f44c` |
| `quantiser.tables.5.dither` | `2b9ec3fdea817566e4699179b25a49460a567a28f27e5e9ab5ddbbb57362b515` |
| `quantiser.tables.5.logdelta_incr` | `0ec4810b688b7d9e89103fbeb72f7c4b5ca752f2af7f974c896238bdf15c3b7b` |
| `quantiser.tables.6.threshold` | `4cffa90c8fc5f4469333fc1883ae3b6222da7102830e1b910de340b358a20cfe` |
| `quantiser.tables.6.dither` | `91f042f4f9219e7a434ba09fac0936f7bd0acc9fced23dbdb1524d1673475b28` |
| `quantiser.tables.6.logdelta_incr` | `911063b0a9d70e0e80d3b56629c386443705247dfc9617f40f2037454c5dfa3d` |
| `quantiser.tables.7.threshold` | `c6966cdb87ea7b93edad2c6dfbdec21ec2417f5eb783f88dcb65fea4af1328d6` |
| `quantiser.tables.7.dither` | `9d751358c3c6fbf8ff1818f931f49c0b5e78408f5bdbd124661b07bccf7bf9a6` |
| `quantiser.tables.7.logdelta_incr` | `8a84b8591fbd19c3db90bec5ea00313105da106782c1bded1229afbc6b47f417` |
| `quantiser.tables.8.threshold` | `40d8e84ea83627b693ce0880df3bfe00f4547936a4c2671f7365a67c17accc8d` |
| `quantiser.tables.8.dither` | `627947ccfbf05a93a9400228d1512538301078e56c948eb9ecce82d1e57b3c7a` |
| `quantiser.tables.8.logdelta_incr` | `3f926f2bda8504498a199d3907a5634f68b3f81246adcf36b3db6ed36cdea11c` |
| `quantiser.tables.9.threshold` | `f95966b5d9bd1d0e23c45edc5273899b139d284c6b27e615c3120833077c9afa` |
| `quantiser.tables.9.dither` | `e36ad0a75add773619a6897dd87e5a2cb3e5bc9d40db66a72d7015cf59709dae` |
| `quantiser.tables.9.logdelta_incr` | `8f33d0f6c5f2a1c8529d420f919b482cfc6dc3e63e7fa66410441923b7692e65` |
| `quantiser.log_table` | `8d90fbab700935537d52d52e35c7f39fc269be932afb2213118141fab3a519b8` |
| `quantiser.logdelta_max` | `6241a067c8091d89378f0591023c0e724d282eb9a82dfca96e564cc58c9f0614` |
| `predictor_taps.4band` | `f365fa122ed552ea81581fbb65f996fdee9fe4d88608cc013c76e41303476c01` |
| `predictor_taps.8band_48k` | `abe26f32dc89274bb015021508465aa0958eff1b91e8933b938f55f29677d412` |
| `predictor_taps.8band_96k` | `42c4681a49eaea1d6bcc39548ab71e2e412b84a06e9d8402c83ca431b70cf2c5` |
| `predictor_taps.8band_lowpower` | `22ffe24b8eb9d05332c85641e905a570a35718ea1544a2a2f06e071adc9beddb` |
| `predictor_taps.8band_full_unused` | `07dbee600fc874c1c895c69884a356362d35e56a4e67532e9274aee5a0071758` |
| `alloc_absolute_huffman.table_a` | `8647c3b0fdeb015ef172fa753b1b35316fbb469f9dab80c7c089662e9abd0752` |
| `alloc_absolute_huffman.table_b` | `6afe8750bf68107ca2b12c9df1d9b5edb8bd71c8b5f7a3adbb05fab153be41ac` |
| `alloc_absolute_huffman.pair_table_8band` | `a5520e2570928d4fef9a1deefe6dd792bfb5b6884b9f5c73f374da39bbdebf83` |
| `alloc_absolute_huffman.pair_table_4band` | `1434edc4f6f956e7c780a2862a4f1faa34acbb2caa6464dcff7cf7136f2defea` |
| `alloc_diff_huffman.tables` | `acd6a6c8557fc0c11a50ad8dce64d9c974f1efe918d2c023718588534a45c9aa` |
| `alloc_diff_huffman.table_index` | `cc6d07384bfd00cd55e7bdbfd0e3cb62596d65af9b81d136c1d10d53bef0fe16` |
| `alloc_diff_huffman.range` | `ed01eb548d1aba1d4464633415974749d781c51d870e152e9154dd70e5358f14` |
| `codeword_huffman.tables.T1` | `d0e31835cbdfebb372e09c3c71a1e08e57798546267b65b09ad36baf0abae69f` |
| `codeword_huffman.tables.T2` | `ec4547a66229698b1557cbcb3e77e07f71f97ba99183bc35d0f050c0a6c4d311` |
| `codeword_huffman.tables.T3` | `248fe7984db77a787283bdf213b3a3b1607f0efc16437d4ff71e3702912bfff2` |
| `codeword_huffman.tables.T4` | `bc38f0301c6e5729d811b046fb93fe9143e17fbe1f4cf3e6a7e0a091e14c8795` |
| `codeword_huffman.entries` | `3f0f300897b3632b0ecae2d69931522f852ef5526013c487212cc339d668002c` |
| `allocator.int8.offset_4band_stereo` | `531401b08a2f5de2f1a3747d78a9ede1dc00b1bf543fc9556f5f13c91649a98d` |
| `allocator.int8.offset_4band_dual_mono` | `da2815f074eb74293859513a5a961707fa5501f51cdead9d0fc69d883d9cef6b` |
| `allocator.int8.offset_8band_stereo` | `ff1e611302543e68e7d349323de5a6bdcbe8c41b7125895ba4c6baa4b569ca0a` |
| `allocator.int8.offset_8band_dual_mono` | `7cd1e074ca5cc56833638605915f583fd65316e18bffe4a72e418904442e8cf7` |
| `allocator.int8.offset_8band_dual_mono_tone` | `e8242038415d25a1fe3f669e949ab9f9458ace6449f66cb25ac1a71cb3d9c7e1` |
| `allocator.int32.max_bits_4band_max` | `6345b5533e60d3e454aa2a1191fb9e259cfd70c5658a66e0a65a4a8022aeb2b8` |
| `allocator.int32.max_bits_4band_370` | `53aa3d7707cfe0f767a95642354f183db8847700b17d8e28bf21c3fd0d248484` |
| `allocator.int32.max_bits_4band_320` | `626a9f3984f5bc0a4f65b66c4bdcf12a7d97b79e327afce934fc049199a6216c` |
| `allocator.int32.max_bits_4band_250` | `6720c26d57b7acb1b3f8a096ffc71487d032d7e5f7e6fc53e4766eb7ae3ea585` |
| `allocator.int32.max_bits_8band_max` | `adcd6fb0b9462fbbf850f84756c5732dbdfee7e7c58022e560d11dc2a874f7db` |
| `allocator.int32.max_bits_8band_520` | `de8303344dc157d092b101272eac1e6a6dcce77e9fbc5a99dff087b91c890b9b` |
| `allocator.int32.max_bits_8band_420` | `bb34e19f96b6dc76bdd4c8adecac4ed915425c1031b69f67ecb2a8500a5edd80` |
| `allocator.int32.max_bits_8band_370` | `598f9987ae29cbbdf3768fe73a6127ab499d737c6ebbb247a822d4fc8bb7318d` |
| `allocator.int32.max_bits_8band_320` | `1d4cfe84f73c3cd85fdac715b15884b9b171e7c1703ad84e8c0f41897521b1b3` |
| `allocator.int32.max_bits_8band_250` | `37d00705030f96217c3555fc439ba04c9818eba296fc8c538a3e16c7786bb664` |
| `allocator.int32.max_bits_8band_96k_max` | `adcd6fb0b9462fbbf850f84756c5732dbdfee7e7c58022e560d11dc2a874f7db` |
| `allocator.int32.max_bits_8band_96k_650` | `ac342507db6899cd301863f2365f4dd82955ffb33532270418c3248dc211058a` |
| `allocator.int32.max_bits_8band_96k_600` | `cac582a21f6147e26cc3809d2bc1b08c59b2aa772f96300c6a93ba35769ba740` |
| `allocator.int32.max_bits_8band_96k_500` | `da928b4678691177b7b72c38d407a43ccd20fc25500a9d3f9627322edc7b3144` |
| `allocator.int32.max_bits_8band_96k_420` | `4c360cc7f54df17381b8dac76096cf58a49477867e7fff11d51c8ccd1b496cfd` |
| `period_interleaved.periods` | `847e1a6ea5ffd95d502c805a45546d236a9ae00d3b4ad1a61bcbc631316b76d8` |

`max_bits_8band_max` and `max_bits_8band_96k_max` have the same hash because the two tables have the same contents. This is correct, not a copy-paste error: they are separate symbols at different addresses.

---

## 6. Per-build locator table (known-good build `1ac88157…`)

This is a fallback for a build script that wants a fixed offset when a symbol is missing, and a cross-check when it isn't. "vaddr = file offset" holds for this build (§2). Use it only when the file hash matches.

| symbol | in | section | vaddr / file offset | offset in section | st_size |
|---|---|---|---|---|---|
| `anOuterCoeffs_32` | .dynsym | `.rodata` | 0x35078 | 0x14D50 | 64 |
| `anInnerCoeffs_32` | .dynsym | `.rodata` | 0x35158 | 0x14E30 | 64 |
| `anOuterCoeffs_24` | .dynsym | `.rodata` | 0x35048 | 0x14D20 | 48 |
| `anInnerCoeffs_24` | .dynsym | `.rodata` | 0x35128 | 0x14E00 | 48 |
| `anOuterCoeffs_32Classic` | .dynsym | `.rodata` | 0x350B8 | 0x14D90 | 64 |
| `anInnerCoeffs_32Classic` | .dynsym | `.rodata` | 0x35198 | 0x14E70 | 64 |
| `anOuterCoeffs_22` | .dynsym | `.rodata` | 0x35018 | 0x14CF0 | 44 |
| `anInnerCoeffs_22` | .dynsym | `.rodata` | 0x350F8 | 0x14DD0 | 44 |
| `CMFB_PRPD_Proto_Sym_256Tap_50PcOL` | .dynsym | `.rodata` | 0x33B00 | 0x137D8 | 1024 |
| `CMFB_PRPD_Proto_Sym_128Tap_50PcOL` | .dynsym | `.rodata` | 0x34A80 | 0x14758 | 512 |
| `CmfbPRPDAnalysisFilter_RunFastCosineModulation_EvenPolyPrototypeLen` | .symtab only | `.text` | 0x107EC | 0x07E4C | 448 |
| `CmfbPRPD_RunDCT2_N8` | .dynsym | `.text` | 0x0FE60 | 0x074C0 | 424 |
| `nu_logTable` | .dynsym | `.rodata` | 0x35328 | 0x15000 | 128 |
| `nMaxLDBuf1` … `nMaxLDBuf8` | .dynsym | `.rodata` | 0x352E0 + 4·(i−1) | 0x14FB8 + 4·(i−1) | 4 |
| `nu_q2incr16` | .dynsym | `.rodata` | 0x353A8 | 0x15080 | 6 |
| `nu_q3incr16` | .dynsym | `.rodata` | 0x353B0 | 0x15088 | 10 |
| `nu_q4incr16` | .dynsym | `.rodata` | 0x353C0 | 0x15098 | 18 |
| `nu_q5incr16` | .dynsym | `.rodata` | 0x353F0 | 0x150C8 | 34 |
| `nu_q6incr16` | .dynsym | `.rodata` | 0x35440 | 0x15118 | 66 |
| `nu_q7incr16` | .dynsym | `.rodata` | 0x35488 | 0x15160 | 130 |
| `nu_q8incr16` | .dynsym | `.rodata` | 0x35510 | 0x151E8 | 258 |
| `nu_q9incr16` | .dynsym | `.rodata` | 0x35618 | 0x152F0 | 514 |
| `nu_dq2bit16` | .dynsym | `.rodata` | 0x35820 | 0x154F8 | 12 |
| `nu_dq3bit16` | .dynsym | `.rodata` | 0x35830 | 0x15508 | 20 |
| `nu_dq4bit16` | .dynsym | `.rodata` | 0x35848 | 0x15520 | 36 |
| `nu_dq5bit16` | .dynsym | `.rodata` | 0x35898 | 0x15570 | 68 |
| `nu_dq6bit16` | .dynsym | `.rodata` | 0x35928 | 0x15600 | 132 |
| `nu_dq7bit16` | .dynsym | `.rodata` | 0x359B0 | 0x15688 | 260 |
| `nu_dq8bit16` | .dynsym | `.rodata` | 0x35AB8 | 0x15790 | 516 |
| `nu_dq9bit16` | .dynsym | `.rodata` | 0x35CC0 | 0x15998 | 1028 |
| `nu_dq2dith16` | .dynsym | `.rodata` | 0x36900 | 0x165D8 | 12 |
| `nu_dq3dith16` | .dynsym | `.rodata` | 0x36910 | 0x165E8 | 20 |
| `nu_dq4dith16` | .dynsym | `.rodata` | 0x36928 | 0x16600 | 36 |
| `nu_dq5dith16` | .dynsym | `.rodata` | 0x36978 | 0x16650 | 68 |
| `nu_dq6dith16` | .dynsym | `.rodata` | 0x36A08 | 0x166E0 | 132 |
| `nu_dq7dith16` | .dynsym | `.rodata` | 0x36A90 | 0x16768 | 260 |
| `nu_dq8dith16` | .dynsym | `.rodata` | 0x36B98 | 0x16870 | 516 |
| `nu_dq9dith16` | .dynsym | `.rodata` | 0x36DA0 | 0x16A78 | 1028 |
| `g_nNumZeros_Music_8` | .dynsym | `.data` | 0x39940 | 0x00660 | 32 |
| `g_nNumZeros_Music_8_Reduced` | .dynsym | `.data` | 0x39960 | 0x00680 | 32 |
| `g_nNumZeros_Music_8_96k` | .dynsym | `.data` | 0x39980 | 0x006A0 | 32 |
| `g_nNumZeros_Music_8_LowPower` | .dynsym | `.data` | 0x39A00 | 0x00720 | 32 |
| `g_aPacker_cfg0_HuffBitAlloc_0_1` | .dynsym | `.rodata` | 0x381A0 | 0x17E78 | 548 |
| `g_aPacker_cfg0_HuffBitAlloc_2_3` | .dynsym | `.rodata` | 0x383C8 | 0x180A0 | 548 |
| `g_aPacker_cfg0_Tables_cfg0BitAlloc_4Band` | .dynsym | `.data` | 0x39840 | 0x00560 | 32 |
| `g_aPacker_cfg0_Tables_cfg0BitAlloc_8Band` | .dynsym | `.data` | 0x39860 | 0x00580 | 32 |
| `g_aPacker_cfg0_HuffDifVal_0` … `_8` | .dynsym | `.rodata` | 0x37F60 + 0x40·k | 0x17C38 + 0x40·k | 64 |
| `g_aPacker_cfg0_HuffDifVal` | .dynsym | `.data` | 0x39880 | 0x005A0 | 64 |
| `g_aPacker_cfg0_RangeDifVal` | .dynsym | `.rodata` | 0x38848 | 0x18520 | 128 |
| `g_aPacker_cfg0_HuffEcstasy_32_2_sqe_K0_0` | .dynsym | `.rodata` | 0x37AE0 | 0x177B8 | 64 |
| `g_aPacker_cfg0_HuffHL2Bit2Sqe_0` | .dynsym | `.rodata` | 0x37F20 | 0x17BF8 | 64 |
| `g_aPacker_cfg0_HuffLlevame_con_tu_babalao_48000_22_3_sqe_K0` | .dynsym | `.rodata` | 0x379E0 | 0x176B8 | 256 |
| `g_aPacker_cfg0_HuffMindwarp_32_7_sqe_k3_1` | .dynsym | `.rodata` | 0x37B20 | 0x177F8 | 1024 |
| `g_axPackerCfg0ComposerTable` | .dynsym | `.data.rel.ro` | 0x390E8 | 0x00048 | 204 |
| `BitAllocOffset4StereoTable` | .dynsym | `.rodata` | 0x20360 | 0x00038 | 32 |
| `BitAllocOffset8StereoTable` | .dynsym | `.rodata` | 0x20380 | 0x00058 | 64 |
| `BitAllocOffset4DualMonoTable` | .dynsym | `.rodata` | 0x203C0 | 0x00098 | 32 |
| `BitAllocOffset8DualMonoTable` | .dynsym | `.rodata` | 0x203E0 | 0x000B8 | 80 |
| `BitAllocOffset8DualMonoToneTable` | .dynsym | `.rodata` | 0x20430 | 0x00108 | 80 |
| `nMaxBitsCfg0_Max` | .dynsym | `.rodata` | 0x204A0 | 0x00178 | 32 |
| `nMaxBitsCfg0_370` | .dynsym | `.rodata` | 0x204C0 | 0x00198 | 32 |
| `nMaxBitsCfg0_320` | .dynsym | `.rodata` | 0x204E0 | 0x001B8 | 32 |
| `nMaxBitsCfg0_250` | .dynsym | `.rodata` | 0x20500 | 0x001D8 | 32 |
| `nMaxBitsCfg0_8Band_Max` | .dynsym | `.rodata` | 0x20520 | 0x001F8 | 64 |
| `nMaxBitsCfg0_8Band_520` | .dynsym | `.rodata` | 0x20560 | 0x00238 | 64 |
| `nMaxBitsCfg0_8Band_420` | .dynsym | `.rodata` | 0x205A0 | 0x00278 | 64 |
| `nMaxBitsCfg0_8Band_370` | .dynsym | `.rodata` | 0x205E0 | 0x002B8 | 64 |
| `nMaxBitsCfg0_8Band_320` | .dynsym | `.rodata` | 0x20620 | 0x002F8 | 64 |
| `nMaxBitsCfg0_8Band_250` | .dynsym | `.rodata` | 0x20660 | 0x00338 | 64 |
| `nMaxBitsCfg0_8Band_96k_Max` | .dynsym | `.rodata` | 0x206A0 | 0x00378 | 64 |
| `nMaxBitsCfg0_8Band_96k_650` | .dynsym | `.rodata` | 0x206E0 | 0x003B8 | 64 |
| `nMaxBitsCfg0_8Band_96k_600` | .dynsym | `.rodata` | 0x20720 | 0x003F8 | 64 |
| `nMaxBitsCfg0_8Band_96k_500` | .dynsym | `.rodata` | 0x20760 | 0x00438 | 64 |
| `nMaxBitsCfg0_8Band_96k_420` | .dynsym | `.rodata` | 0x207A0 | 0x00478 | 64 |

---

## 7. Proof

The dirty side checked this with a throwaway extractor written only from this document: 77 / 77 table ids match on the known-good build, on a copy with `.symtab` stripped, and on a copy taken straight from the OTA `dsp` image.

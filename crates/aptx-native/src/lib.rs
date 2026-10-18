//! # aptx-native — aptX Adaptive codec, rebuilt natively for x86
//!
//! A reconstruction of Qualcomm's aptX Adaptive codec in Rust, built from
//! the shipped Hexagon DSP modules (unstripped), Qualcomm's own x86
//! reference decoders (decompiled), and the extracted coefficient tables.
//! The goal is to remove the `qemu-hexagon` emulation dependency so the
//! codec runs natively anywhere.
//!
//! ## Where the design came from
//!
//! Everything here is derived from evidence, never guessed:
//!
//! * **Symbol tables** of the unstripped Hexagon modules
//!   (`libaptXAdaptiveEnc{,3,4}.so`) give the algorithm's structure —
//!   `CmfbPRPDAnalysisFilter8Bands`, `AxR3Quantizer_*`, `AxBitAlloc_*`,
//!   `AxEntropyComposer*`, `AxFramer*`, …
//! * **Tables** (filter prototypes, modulation matrix, quantisers,
//!   Huffman/Golomb-Rice, bit allocation) extracted byte-exact from the
//!   binaries — see [`tables`] and `analysis/TABLES-EXTRACTED.md`.
//! * **Source-tree map** recovered from the `CRMBuilds/...` path strings,
//!   naming each DSP subsystem (`cmfb/`, `compressor/`, `quantizationNU/`,
//!   `bitAllocLoudness/`, `packer_cfg0/`, …).
//! * The **reference decoders** (`test-decoder.exe`,
//!   `ax3-test-decoder.exe`), decompiled with Ghidra, give the exact
//!   bitstream syntax and the *inverse* of every encoder stage.
//!
//! ## Status
//!
//! | stage | state |
//! |---|---|
//! | CMFB analysis/synthesis bank | ✅ transcription + PR dual, tested |
//! | LPC / prediction | ✅ fixed predictors + autocorrelation/LD, tested |
//! | non-uniform quantiser | ✅ over the extracted nu tables, tested |
//! | bit allocation | ✅ vendor cost mapping + energy allocator, tested |
//! | entropy composer (GR / Huffman) | ✅ bit I/O + Golomb-Rice + band coder, tested |
//! | framer / OTA header | ✅ recovered header + payload sizes, tested; R2 HQ (656 B, config 3) and R2 LL (348 B, config 4, `APTX_NATIVE_LL=1`) |
//! | encoder API + decoder | ✅ `Encoder`/`Decoder`, end-to-end test |
//!
//! The C transcription of the same reconstruction lives in `c-reference/`
//! and stays in sync as the working reference for the port.
//!
//! SPDX-License-Identifier: MIT
#![warn(missing_docs)]

pub mod bitalloc;
pub mod capi;
pub mod cmfb;
pub mod composer;
pub mod composer_tables;
pub mod frame;
pub mod frame_blocks;

pub mod alloc_dif;
pub mod alloc_dif_tables;
pub mod bitalloc_seq;
pub mod bitalloc_tables;
pub mod bitalloc_vendor;
pub mod bitalloc_vendor_tables;
pub mod cmfb_dual;
pub mod decoder;
pub mod dither;
pub mod encoder;
pub mod entropy;
pub mod helper_bridge;
/// 96 kHz Hi-Res producer scaffolding (feature `hires-96k`).  The container
/// and the rate/header mapping are verified against the vendor capture; the
/// 8-field allocation syntax is not, so the module never touches the
/// shipping 48 kHz paths.
#[cfg(feature = "hires-96k")]
pub mod hires;
pub mod huffman;
pub mod lpc;
pub mod nu_tables;
pub mod packer;
pub mod quant;
pub mod quant_nu;
pub mod sslms;
pub mod tables;
pub mod tid0;
pub mod tid0_tables;
pub mod vendor_arith;
pub mod vendor_core;

/// Number of subbands in the aptX Adaptive filter bank.
pub const BANDS: usize = 8;

/// Modulated channels produced by the CMFB demodulator (2 × [`BANDS`]).
pub const MODCH: usize = 16;

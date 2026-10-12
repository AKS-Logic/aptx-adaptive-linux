//! Stage-by-stage checks against `tests/vectors/stages` (see its README).

#![expect(clippy::many_single_char_names, reason = "names follow the spec")]

use std::fs;
use std::path::PathBuf;

use super::decode::{coded, decode};
use super::pack::Packer;
use super::{Engine, Params, Variant};

struct Vectors {
    dir: PathBuf,
    params: Params,
    packet_len: usize,
    n_packets: usize,
    blocks: usize,
    /// (packet, blocks, bits)
    frames: Vec<(usize, usize, Vec<u8>)>,
}

/// Just enough JSON for `meta.json` and `frames.json`: pulls out every integer in order.
fn numbers(s: &str) -> Vec<i64> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars().chain([' ']) {
        if c.is_ascii_digit() || (c == '-' && cur.is_empty()) {
            cur.push(c);
        } else if !cur.is_empty() {
            out.push(cur.parse().unwrap());
            cur.clear();
        }
    }
    out
}

impl Vectors {
    fn load(name: &str, variant: Variant, sample_rate: u32) -> Self {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/vectors/stages")
            .join(name);
        let meta = fs::read_to_string(dir.join("meta.json")).unwrap();
        // Keys appear in the order packet_len, n_packets, blocks, subbands_per_channel.
        let m = numbers(&meta);
        let params = Params {
            variant,
            sample_rate,
        };
        assert_eq!(m[3] as usize, params.subbands());
        let width = params.width();
        let f = numbers(&fs::read_to_string(dir.join("frames.json")).unwrap());
        let frames = f
            .chunks_exact(2 + width)
            .map(|c| {
                (
                    c[0] as usize,
                    c[1] as usize,
                    c[2..].iter().map(|&b| b as u8).collect(),
                )
            })
            .collect();
        Self {
            dir,
            params,
            packet_len: m[0] as usize,
            n_packets: m[1] as usize,
            blocks: m[2] as usize,
            frames,
        }
    }

    fn s32(&self, file: &str) -> Vec<i32> {
        let bytes = fs::read(self.dir.join(file)).unwrap();
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| i32::from_le_bytes(*b))
            .collect()
    }

    /// Allocation for each block.
    fn block_bits(&self) -> Vec<&[u8]> {
        self.frames
            .iter()
            .flat_map(|(_, n, bits)| std::iter::repeat_n(bits.as_slice(), *n))
            .collect()
    }
}

fn first_mismatch(name: &str, what: &str, width: usize, got: &[i32], want: &[i32]) {
    assert!(
        want.iter().any(|&v| v != 0),
        "{name} {what}: vector is all zero"
    );
    if let Some(i) = got.iter().zip(want).position(|(g, w)| g != w) {
        let b = i / width;
        panic!(
            "{name} {what}: block {b} band {}: got {:?}, want {:?}",
            i % width,
            &got[b * width..(b + 1) * width],
            &want[b * width..(b + 1) * width]
        );
    }
    assert_eq!(got.len(), want.len(), "{name} {what} length");
}

fn check_analysis(v: &Vectors, name: &str) {
    let w = v.params.width();
    let pcm = v.s32("pcm_in.s32");
    let want = v.s32("analysis_out.s32");
    let mut e = Engine::new(v.params);
    let mut got = vec![0; v.blocks * w];
    for (p, x) in pcm.chunks_exact(w).zip(got.chunks_exact_mut(w)) {
        e.analyse(p, x);
    }
    first_mismatch(name, "analysis_out", w, &got, &want);
}

fn check_quant_predict(v: &Vectors, name: &str) {
    let w = v.params.width();
    let x = v.s32("analysis_out.s32");
    let bits = v.block_bits();
    assert_eq!(bits.len(), v.blocks);
    let mut e = Engine::new(v.params);
    let (mut codes, mut recon, mut pred) = (Vec::new(), Vec::new(), Vec::new());
    let mut q = vec![0; w];
    for (x, b) in x.chunks_exact(w).zip(bits) {
        let t = e.code_traced(x, b, &mut q);
        codes.extend_from_slice(&t.codes);
        recon.extend_from_slice(&t.recon);
        pred.extend_from_slice(&t.pred_out);
    }
    first_mismatch(name, "quant_codes", w, &codes, &v.s32("quant_codes.s32"));
    first_mismatch(name, "quant_recon", w, &recon, &v.s32("quant_recon.s32"));
    first_mismatch(name, "pred_out", w, &pred, &v.s32("pred_out.s32"));
}

fn check_packets(v: &Vectors, name: &str) {
    let w = v.params.width();
    let codes = v.s32("quant_codes.s32");
    let want = fs::read(v.dir.join("packets.bin")).unwrap();
    let mut packer = Packer::new(v.params, v.packet_len);
    let mut got = Vec::new();
    let mut out = vec![0; v.packet_len];
    let mut block = 0;
    let mut current = 0;
    for (p, n, bits) in &v.frames {
        while current < *p {
            packer.finish(&mut out);
            got.extend_from_slice(&out);
            current += 1;
        }
        packer.push_frame(bits, &codes[block * w..(block + n) * w]);
        block += n;
    }
    packer.finish(&mut out);
    got.extend_from_slice(&out);
    assert_eq!(got.len(), v.n_packets * v.packet_len, "{name} packet count");
    assert_eq!(want.len(), got.len(), "{name} packets.bin length");
    for (i, (g, expect)) in got
        .chunks(v.packet_len)
        .zip(want.chunks(v.packet_len))
        .enumerate()
    {
        if let Some(b) = g.iter().zip(expect).position(|(a, b)| a != b) {
            panic!(
                "{name} packet {i} byte {b}: got {:02x?}, want {:02x?}",
                &g[b..(b + 8).min(g.len())],
                &expect[b..(b + 8).min(g.len())]
            );
        }
    }
}

/// Our parser reads the reference packets back to `frames.json` and the coded codes.
fn check_decode(v: &Vectors, name: &str) {
    let w = v.params.width();
    let codes = v.s32("quant_codes.s32");
    let packets = fs::read(v.dir.join("packets.bin")).unwrap();
    let frames = decode(v.params, &packets, v.packet_len);
    let got: Vec<_> = frames
        .iter()
        .map(|f| (f.packet, f.blocks(), f.bits.clone()))
        .collect();
    let (head, tail) = (&got[..got.len() - 1], &got[got.len() - 1]);
    let (want_head, want_tail) = v.frames.split_at(v.frames.len() - 1);
    assert_eq!(head, want_head, "{name} frames");
    // The last frame may run into a packet that isn't in the vectors.
    let want_tail = &want_tail[0];
    assert_eq!(
        (tail.0, &tail.2),
        (want_tail.0, &want_tail.2),
        "{name} last frame"
    );
    assert!(tail.1 <= want_tail.1, "{name} last frame blocks");
    let mut block = 0;
    for f in &frames {
        let want: Vec<i32> = codes[block * w..(block + f.blocks()) * w]
            .iter()
            .enumerate()
            .map(|(i, &q)| coded(q, f.bits[i % w]))
            .collect();
        assert_eq!(f.codes, want, "{name} codes from block {block}");
        block += f.blocks();
    }
}

macro_rules! stage_tests {
    ($($name:ident: $dir:literal, $variant:expr, $rate:literal;)*) => {
        mod analysis { use super::*; $(#[test] fn $name() { check_analysis(&Vectors::load($dir, $variant, $rate), $dir); })* }
        mod quant_predict { use super::*; $(#[test] fn $name() { check_quant_predict(&Vectors::load($dir, $variant, $rate), $dir); })* }
        mod packets { use super::*; $(#[test] fn $name() { check_packets(&Vectors::load($dir, $variant, $rate), $dir); })* }
        mod decode { use super::*; $(#[test] fn $name() { check_decode(&Vectors::load($dir, $variant, $rate), $dir); })* }
    };
}

stage_tests! {
    cfg0_noise: "cfg0_48k_noise", Variant::Cfg0, 48_000;
    cfg0_sine: "cfg0_48k_sine", Variant::Cfg0, 48_000;
    cfg1_noise: "cfg1_48k_noise", Variant::Cfg1, 48_000;
    cfg3_noise: "cfg3_48k_noise", Variant::Cfg3, 48_000;
    cfg3_sine: "cfg3_48k_sine", Variant::Cfg3, 48_000;
    cfg3_96k_noise: "cfg3_96k_noise", Variant::Cfg3, 96_000;
}

/// End to end on `tests/vectors/r2` (1 s, 364 kbps, 656 bytes): PCM through the encoder with
/// the reference's allocations and frame sizes, read back from its own packets, must give its
/// packets byte for byte.
fn check_end_to_end(name: &str, variant: Variant) {
    let params = Params {
        variant,
        sample_rate: 48_000,
    };
    let w = params.width();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/vectors/r2");
    let level = if variant == Variant::Cfg0 { "r1" } else { "r2" };
    let want = fs::read(dir.join(format!("{name}.{level}.bin"))).unwrap();
    let pcm: Vec<i32> = fs::read(dir.join(format!("{name}.s32le")))
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| i32::from_le_bytes(*b))
        .collect();
    let frames = decode(params, &want, 656);

    let mut engine = Engine::new(params);
    let mut packer = Packer::new(params, 656);
    let mut got = Vec::new();
    let mut out = vec![0; 656];
    let mut blocks = pcm.chunks_exact(w);
    let mut x = vec![0; w];
    let mut current = 0;
    for f in &frames {
        while current < f.packet {
            packer.finish(&mut out);
            got.extend_from_slice(&out);
            current += 1;
        }
        let mut codes = vec![0; f.blocks() * w];
        for q in codes.chunks_exact_mut(w) {
            engine.analyse(blocks.next().unwrap(), &mut x);
            engine.code(&x, &f.bits, q);
        }
        packer.push_frame(&f.bits, &codes);
    }
    packer.finish(&mut out);
    got.extend_from_slice(&out);
    assert_eq!(got.len(), want.len(), "{name}.{level} length");
    // The last packet's continuation isn't in the file, so its final frame is short.
    let last = want.len() - 656;
    if let Some(b) = got[..last].iter().zip(&want).position(|(a, b)| a != b) {
        panic!(
            "{name}.{level}: first difference at packet {} byte {}",
            b / 656,
            b % 656
        );
    }
}

#[test]
fn end_to_end_r1() {
    for name in ["silence", "sine1k", "noise"] {
        check_end_to_end(name, Variant::Cfg0);
    }
}

#[test]
fn end_to_end_r2() {
    for name in ["silence", "sine1k", "noise"] {
        check_end_to_end(name, Variant::Cfg3);
    }
}

//! Raw PCM to packets against the reference vectors, with no reference allocations or frame
//! sizes: `tests/vectors/r2` through the wrapper loop of its README, and the stage vectors'
//! `packets.bin` with one encode call per packet (`codec-core.md` §8.7).

use std::fs;
use std::path::PathBuf;

use crate::{Config, DecoderLevel, Encoder, Profile, WINDOW_SLACK};

fn vectors(dir: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/vectors")
        .join(dir)
}

fn read_s32(path: PathBuf) -> Vec<i32> {
    fs::read(path)
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| i32::from_le_bytes(*b))
        .collect()
}

fn first_difference(what: &str, len: usize, got: &[u8], want: &[u8]) {
    if let Some(b) = got.iter().zip(want).position(|(a, b)| a != b) {
        panic!(
            "{what}: first difference at packet {} byte {}",
            b / len,
            b % len
        );
    }
    assert_eq!(got.len(), want.len(), "{what}: length");
}

/// The README's packet loop: 672-frame blocks, flush when due, deficit and set-bitrate.
/// Returns the packets and whether each was forced.
fn wrapper_loop(
    enc: &mut Encoder,
    pcm: &[i32],
    block: usize,
    period: usize,
    len: usize,
    bitrate: u32,
) -> (Vec<u8>, Vec<bool>) {
    let mut window = vec![0; len + WINDOW_SLACK];
    let (mut packets, mut forced) = (Vec::new(), Vec::new());
    let mut buf = Vec::new();
    let (mut consumed, mut deficit) = (0i64, 0i64);
    for chunk in pcm.chunks(block) {
        buf.extend_from_slice(chunk);
        let p = enc.encode_packet(&buf, &mut window).unwrap();
        buf.drain(..p.consumed);
        consumed += p.consumed as i64;
        let due = consumed >= period as i64 + deficit;
        let written = match p.written {
            0 if !due => continue,
            0 => enc.flush(&mut window).unwrap(),
            n => n,
        };
        assert_eq!(written, len);
        forced.push(p.written == 0);
        packets.extend_from_slice(&window[..len]);
        deficit += period as i64 - consumed;
        consumed = 0;
        enc.set_bitrate(if deficit > 96 {
            bitrate - bitrate / 4
        } else {
            bitrate
        });
    }
    (packets, forced)
}

fn check_r2(name: &str, level: DecoderLevel) {
    let dir = vectors("r2");
    let tag = if level == DecoderLevel::R1 {
        "r1"
    } else {
        "r2"
    };
    let what = format!("{name}.{tag}");
    let pcm = read_s32(dir.join(format!("{name}.s32le")));
    let want = fs::read(dir.join(format!("{what}.bin"))).unwrap();
    let sizes = fs::read_to_string(dir.join(format!("{what}.sizes"))).unwrap();
    let want_forced: Vec<bool> = sizes.lines().map(|l| l.ends_with(" 1")).collect();
    let config = Config {
        profile: Profile::Hq,
        sample_rate: 48_000,
        channels: 2,
        level,
    };
    for block in [672, 48] {
        let mut enc = Encoder::new(config, 364_000).unwrap();
        let (got, forced) = wrapper_loop(&mut enc, &pcm, block * 2, 1344, 656, 364_000);
        first_difference(&format!("{what} ({block}-frame blocks)"), 656, &got, &want);
        assert_eq!(forced, want_forced, "{what}: forced packets");
    }
}

#[test]
fn r2_vectors_r1_from_pcm() {
    for name in ["silence", "sine1k", "noise"] {
        check_r2(name, DecoderLevel::R1);
    }
}

#[test]
fn r2_vectors_r2_from_pcm() {
    for name in ["silence", "sine1k", "noise"] {
        check_r2(name, DecoderLevel::R2);
    }
}

/// One encode call per packet with all remaining input; no flush, no set-bitrate.
fn check_stage(name: &str, config: Config, bitrate: u32) {
    let dir = vectors("stages").join(name);
    let pcm = read_s32(dir.join("pcm_in.s32"));
    let want = fs::read(dir.join("packets.bin")).unwrap();
    let meta = fs::read_to_string(dir.join("meta.json")).unwrap();
    let len: usize = meta
        .split("\"packet_len\": ")
        .nth(1)
        .and_then(|s| s.split(',').next())
        .unwrap()
        .parse()
        .unwrap();
    let mut enc = Encoder::new(config, bitrate).unwrap();
    let mut window = vec![0; len + WINDOW_SLACK];
    let mut got = Vec::new();
    let mut pos = 0;
    while got.len() < want.len() {
        let p = enc.encode_packet(&pcm[pos..], &mut window).unwrap();
        pos += p.consumed;
        if p.written == 0 {
            break;
        }
        got.extend_from_slice(&window[..len]);
    }
    first_difference(name, len, &got, &want);
    assert_eq!(pos, pcm.len(), "{name}: consumed");
}

const fn cfg(profile: Profile, level: DecoderLevel, sample_rate: u32) -> Config {
    Config {
        profile,
        sample_rate,
        channels: 2,
        level,
    }
}

#[test]
fn stages_cfg0_noise() {
    check_stage(
        "cfg0_48k_noise",
        cfg(Profile::Hq, DecoderLevel::R1, 48_000),
        364_000,
    );
}

#[test]
fn stages_cfg0_sine() {
    check_stage(
        "cfg0_48k_sine",
        cfg(Profile::Hq, DecoderLevel::R1, 48_000),
        364_000,
    );
}

#[test]
fn stages_cfg1_noise() {
    check_stage(
        "cfg1_48k_noise",
        cfg(Profile::Ll, DecoderLevel::R1, 48_000),
        279_000,
    );
}

#[test]
fn stages_cfg3_noise() {
    check_stage(
        "cfg3_48k_noise",
        cfg(Profile::Hq, DecoderLevel::R2, 48_000),
        420_000,
    );
}

#[test]
fn stages_cfg3_sine() {
    check_stage(
        "cfg3_48k_sine",
        cfg(Profile::Hq, DecoderLevel::R2, 48_000),
        420_000,
    );
}

#[test]
fn stages_cfg3_96k_noise() {
    check_stage(
        "cfg3_96k_noise",
        cfg(Profile::Hq, DecoderLevel::R2, 96_000),
        420_000,
    );
}

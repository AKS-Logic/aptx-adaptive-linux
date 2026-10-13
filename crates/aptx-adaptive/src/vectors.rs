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

/// One line of `tests/vectors/parity/runs.txt`.
struct Run {
    name: String,
    config: Config,
    bitrate: u32,
    period_ms: u32,
    len: usize,
    chunk_frames: usize,
    ll: bool,
}

fn parity_runs() -> Vec<Run> {
    let text = fs::read_to_string(vectors("parity").join("runs.txt")).unwrap();
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let num = |i: usize| -> u32 { f[i].parse().unwrap() };
            let level = match f[1] {
                "R1" => DecoderLevel::R1,
                _ => DecoderLevel::R2,
            };
            // Profile 3 is HQ; 1 (R1) and 6 (R2) are the LL encoder (§10.D, §10.F).
            let profile = match num(2) {
                3 => Profile::Hq,
                1 | 6 => Profile::Ll,
                p => panic!("profile {p}"),
            };
            Run {
                name: f[0].to_owned(),
                config: cfg(profile, level, num(3)),
                bitrate: num(4),
                period_ms: num(5),
                len: num(6) as usize,
                chunk_frames: num(7) as usize,
                ll: num(8) == 1,
            }
        })
        .collect()
}

/// The parity README's loop, which is the r2 loop with the period in interleaved samples
/// (§10.A) and, in LL, the deficit zeroed after every packet (§10.C).
fn parity_loop(enc: &mut Encoder, run: &Run, pcm: &[i32]) -> (Vec<u8>, Vec<bool>) {
    let period = i64::from(run.period_ms) * i64::from(run.config.sample_rate) * 2 / 1000;
    let len = run.len;
    let mut window = vec![0; len + WINDOW_SLACK];
    let (mut packets, mut forced) = (Vec::new(), Vec::new());
    let mut buf = Vec::new();
    let (mut consumed, mut deficit) = (0i64, 0i64);
    for chunk in pcm.chunks(run.chunk_frames * 2) {
        buf.extend_from_slice(chunk);
        let p = enc.encode_packet(&buf, &mut window).unwrap();
        buf.drain(..p.consumed);
        consumed += p.consumed as i64;
        let due = consumed >= period + deficit;
        let written = match p.written {
            0 if !due => continue,
            0 => enc.flush(&mut window).unwrap(),
            n => n,
        };
        assert_eq!(written, len, "{}", run.name);
        forced.push(p.written == 0);
        packets.extend_from_slice(&window[..len]);
        deficit += period - consumed;
        consumed = 0;
        if run.ll {
            deficit = 0;
        }
        let b = run.bitrate;
        enc.set_bitrate(if deficit > 96 { b - b / 4 } else { b });
    }
    (packets, forced)
}

/// Every run in `tests/vectors/parity`, from raw PCM with its stated chunk size and loop.
#[test]
fn parity_vectors() {
    let dir = vectors("parity");
    let mut failures = Vec::new();
    for run in parity_runs() {
        let pcm = read_s32(dir.join(format!("mix{}.s32le", run.config.sample_rate)));
        let want = fs::read(dir.join(format!("{}.bin", run.name))).unwrap();
        let sizes = fs::read_to_string(dir.join(format!("{}.sizes", run.name))).unwrap();
        let want_forced: Vec<bool> = sizes.lines().map(|l| l.ends_with(" 1")).collect();
        let mut enc = match Encoder::new(run.config, run.bitrate) {
            Ok(e) => e,
            Err(e) => {
                failures.push(format!("{}: {e}", run.name));
                continue;
            }
        };
        let (got, forced) = parity_loop(&mut enc, &run, &pcm);
        let len = run.len;
        if let Some(b) = got.iter().zip(&want).position(|(a, b)| a != b) {
            failures.push(format!(
                "{}: first difference at packet {} byte {}",
                run.name,
                b / len,
                b % len
            ));
        } else if got.len() != want.len() {
            failures.push(format!(
                "{}: {} packets, want {}",
                run.name,
                got.len() / len,
                want.len() / len
            ));
        } else if forced != want_forced {
            failures.push(format!("{}: forced packets differ", run.name));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
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

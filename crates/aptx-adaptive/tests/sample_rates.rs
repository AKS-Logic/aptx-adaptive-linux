use std::fs;
use std::path::PathBuf;

use aptx_adaptive::{Config, DecoderLevel, Error, Profile, Stream, table_row};

fn config(rate: u32, profile: Profile) -> Config {
    Config {
        profile,
        sample_rate: rate,
        channels: 2,
        level: DecoderLevel::R2,
    }
}

fn vectors() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/vectors/parity")
}

#[test]
fn automatic_96k_rows_match_reference_packets() {
    let stream = Stream::new(config(96_000, Profile::Hq), 875).unwrap();
    let low = stream.level_row(1).unwrap();
    assert_eq!(
        (low.bitrate, low.period_ms, low.packet_bytes),
        (128_000, 40, 656)
    );
    for (level, bitrate, period, samples, packets) in [
        (2, 255_000, 20, 3840, 50),
        (3, 340_000, 15, 2880, 66),
        (4, 420_000, 12, 2304, 83),
        (5, 646_000, 8, 1536, 125),
    ] {
        let mut stream = Stream::new(config(96_000, Profile::Hq), 875).unwrap();
        assert!(stream.request_level(level));
        assert!(stream.apply_level_now());
        assert_eq!(stream.row().bitrate, bitrate);
        assert_eq!(stream.row().period_ms, period);
        assert_eq!(stream.samples_per_packet(), samples);
        assert_vector(
            &mut stream,
            96_000,
            96,
            &format!("c3_96k_i5_L{level}"),
            packets,
        );
    }
}

#[test]
fn automatic_44k_rows_keep_exact_odd_periods() {
    for (profile, level, samples, name, packets) in [
        (Profile::Hq, 3, 1675, "c3_44k_i1_L3", 52),
        (Profile::Hq, 5, 1058, "c3_44k_i1_L5", 83),
        (Profile::Ll, 3, 882, "c4_44k_i1_L3", 94),
    ] {
        let mut stream = Stream::new(config(44_100, profile), 875).unwrap();
        assert!(stream.request_level(level));
        assert!(stream.apply_level_now());
        assert_eq!(stream.samples_per_packet(), samples);
        assert_vector(&mut stream, 44_100, 44, name, packets);
    }
}

fn assert_vector(
    stream: &mut Stream<aptx_adaptive::Encoder>,
    rate: u32,
    frames: usize,
    name: &str,
    packets: usize,
) {
    let bytes = fs::read(vectors().join(format!("mix{rate}.s32le"))).unwrap();
    let pcm: Vec<i32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| i32::from_le_bytes(*b))
        .collect();
    let want = fs::read(vectors().join(format!("{name}.bin"))).unwrap();
    let mut got = Vec::new();
    let mut out = [0; 1024];
    for block in pcm.chunks_exact(frames * 2) {
        if let Some(n) = stream.push(block, &mut out).unwrap() {
            for pair in out[..n].as_chunks::<2>().0 {
                got.extend_from_slice(&[pair[1], pair[0]]);
            }
        }
    }
    assert_eq!(got.len(), packets * stream.row().packet_bytes, "{name}");
    assert!(got == want, "{name} payload mismatch");
}

#[test]
fn refused_96k_ll_reset_preserves_hq_stream() {
    let hq = config(96_000, Profile::Hq);
    let ll = config(96_000, Profile::Ll);
    let mut stream = Stream::new(hq, 875).unwrap();
    let mut unchanged = Stream::new(hq, 875).unwrap();
    let pcm = [37; 192];
    let mut out = [0; 1024];
    let mut expected = [0; 1024];
    assert_eq!(stream.push(&pcm, &mut out), Ok(None));
    assert_eq!(unchanged.push(&pcm, &mut expected), Ok(None));
    assert_eq!(table_row(ll, 875), Err(Error::UnsupportedConfig));
    assert_eq!(
        stream.reset(ll, stream.row()),
        Err(Error::UnsupportedConfig)
    );
    assert_eq!(stream.config(), hq);
    for _ in 0..30 {
        assert_eq!(
            stream.push(&pcm, &mut out),
            unchanged.push(&pcm, &mut expected)
        );
        assert_eq!(out, expected);
    }
}

#![allow(clippy::cast_precision_loss, clippy::chunks_exact_to_as_chunks)]

use aptx_adaptive::{Config, DecoderLevel, Encoder, Profile};

fn swap16(src: &[u8], dst: &mut [u8]) {
    for (i, c) in src.chunks_exact(2).enumerate() {
        dst[2 * i] = c[1];
        dst[2 * i + 1] = c[0];
    }
}

fn main() {
    let cfg = Config {
        profile: Profile::Hq,
        sample_rate: 48_000,
        channels: 2,
        level: DecoderLevel::R2,
    };
    let mut e = Encoder::new(cfg, 214_000).expect("encoder");
    e.prepare(656).expect("prepare");
    let mut window = vec![0u8; 656 + 64];
    let mut pcm: Vec<i32> = Vec::new();
    let mut phase = 0usize;
    let mut file = Vec::new();
    let mut packets = 0usize;
    while packets < 160 {
        for _ in 0..1200 {
            let v = (0.30
                * (2.0 * std::f64::consts::PI * 1000.0 * phase as f64 / 48_000.0).sin()
                * (1u64 << 23) as f64) as i32;
            pcm.push(v);
            pcm.push(v);
            phase += 1;
        }
        loop {
            let p = e.encode_packet(&pcm, &mut window).expect("encode");
            let c = p.consumed.min(pcm.len());
            pcm.drain(..c);
            if p.written > 0 {
                file.extend_from_slice(&[0u8; 8]);
                let mut sw = vec![0u8; p.written];
                swap16(&window[..p.written], &mut sw);
                file.extend_from_slice(&sw);
                packets += 1;
                break;
            }
            if c == 0 {
                break;
            }
        }
    }
    let path = "/home/alpha/aks-infra/audio/aptx-native/.scratch/aptx_clean_1k_4s.bin";
    std::fs::write(path, &file).unwrap();
    println!("packets={packets} bytes={} -> {path}", file.len());
}

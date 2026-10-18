#![allow(clippy::cast_precision_loss, clippy::chunks_exact_to_as_chunks)]

use aptx_adaptive::{Config, DecoderLevel, Encoder, Profile};

fn push_sine(queue: &mut Vec<i32>, phase: &mut usize, frames: usize) {
    for _ in 0..frames {
        let v = (0.30
            * (2.0 * std::f64::consts::PI * 1000.0 * (*phase as f64) / 48_000.0).sin()
            * (1u64 << 23) as f64) as i32;
        queue.push(v);
        queue.push(v);
        *phase += 1;
    }
}

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
    for bitrate in [
        200_000u32, 202_000, 204_000, 206_000, 208_000, 210_000, 212_000, 214_000, 216_000,
    ] {
        let mut e = match Encoder::new(cfg, bitrate) {
            Ok(e) => e,
            Err(err) => {
                println!("{bitrate}: new {err:?}");
                continue;
            }
        };
        if e.prepare(656).is_err() {
            println!("{bitrate}: prepare failed");
            continue;
        }
        let mut window = vec![0u8; 656 + 64];
        let mut queue: Vec<i32> = Vec::new();
        let mut phase = 0usize;
        push_sine(&mut queue, &mut phase, 2400);
        let mut file = Vec::new();
        let mut packets = 0usize;
        let mut guard = 0usize;
        while packets < 20 && guard < 10000 {
            guard += 1;
            let p = match e.encode_packet(&queue, &mut window) {
                Ok(p) => p,
                Err(err) => {
                    println!("{bitrate}: enc {err:?}");
                    break;
                }
            };
            let consumed = p.consumed.min(queue.len());
            queue.drain(..consumed);
            if p.written > 0 {
                file.extend_from_slice(&[0u8; 8]);
                let mut swapped = vec![0u8; p.written];
                swap16(&window[..p.written], &mut swapped);
                file.extend_from_slice(&swapped);
                packets += 1;
            }
            push_sine(&mut queue, &mut phase, 1200);
        }
        let path = format!("/home/alpha/aks-infra/audio/aptx-native/.scratch/sweep_{bitrate}.bin");
        std::fs::write(&path, &file).unwrap();
        println!("bitrate {bitrate}: packets {packets} bytes {}", file.len());
    }
}

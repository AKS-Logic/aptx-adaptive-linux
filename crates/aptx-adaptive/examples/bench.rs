//! Measures encoder speed against real time: `cargo run --release -p aptx-adaptive --example bench`.

use std::time::Instant;

use aptx_adaptive::{Config, DecoderLevel, Profile, Stream, packet_budget};

const SECONDS: u32 = 60;
const RATE: u32 = 48_000;

fn main() {
    let mut s = 1u32;
    let pcm: Vec<i32> = (0..SECONDS * RATE * 2)
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s as i32) >> 8
        })
        .collect();
    for (name, profile, level) in [
        ("config 0 (R1 HQ)", Profile::Hq, DecoderLevel::R1),
        ("config 0 (R1 LL row)", Profile::Ll, DecoderLevel::R1),
        ("config 4 (R2 LL)", Profile::Ll, DecoderLevel::R2),
        ("config 3 (R2 HQ)", Profile::Hq, DecoderLevel::R2),
    ] {
        let config = Config {
            profile,
            sample_rate: RATE,
            channels: 2,
            level,
        };
        let mut stream = Stream::new(config, packet_budget(895)).expect("config");
        let mut out = [0; 2048];
        let (mut packets, mut bytes) = (0, 0);
        let start = Instant::now();
        // 1 ms chunks, like the reference source.
        for chunk in pcm.chunks(96) {
            if let Some(n) = stream.push(chunk, &mut out).expect("encode") {
                packets += 1;
                bytes += n;
            }
        }
        let took = start.elapsed().as_secs_f64();
        println!(
            "{name}: {SECONDS} s in {:.1} ms, {:.0}x real time, {packets} packets, {} kbit/s",
            took * 1000.0,
            f64::from(SECONDS) / took,
            bytes * 8 / SECONDS as usize / 1000,
        );
    }
}

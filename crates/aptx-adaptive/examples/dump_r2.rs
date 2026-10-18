#![allow(clippy::cast_precision_loss, clippy::chunks_exact_to_as_chunks)]

use aptx_adaptive::{Config, DecoderLevel, Profile, Stream, table_row};

fn main() {
    let cfg = Config {
        profile: Profile::Hq,
        sample_rate: 48_000,
        channels: 2,
        level: DecoderLevel::R2,
    };
    for budget in [656usize, 660, 664] {
        match table_row(cfg, budget) {
            Ok(row) => println!(
                "budget {budget}: period {} ms, packet {} B, bitrate {}, samples/ch {}, interleaved {}",
                row.period_ms,
                row.packet_bytes,
                row.bitrate,
                row.samples_per_channel,
                row.interleaved_samples(48_000, 2),
            ),
            Err(e) => println!("budget {budget}: {e:?}"),
        }
    }
    for q in 1..=7u8 {
        match aptx_adaptive::table_row_at_level(cfg, 656, q) {
            Ok(row) => println!(
                "quality {q}: period {} ms packet {} bitrate {} samples/ch {}",
                row.period_ms, row.packet_bytes, row.bitrate, row.samples_per_channel
            ),
            Err(e) => println!("quality {q}: {e:?}"),
        }
    }
    let row = (1..=7u8)
        .filter_map(|q| aptx_adaptive::table_row_at_level(cfg, 656, q).ok())
        .find(|r| r.period_ms == 25)
        .expect("25 ms row");
    println!(
        "chosen row: period {} packet {} bitrate {} samples/ch {}",
        row.period_ms, row.packet_bytes, row.bitrate, row.samples_per_channel
    );
    let core = aptx_adaptive::Encoder::new(cfg, row.bitrate).unwrap();
    let mut s = Stream::with_core(core, cfg, row).with_budget(656);
    let n = s.samples_per_packet();
    println!("stream samples_per_packet={n}");
    let mut pcm = Vec::with_capacity(n * 2);
    for i in 0..n {
        let v = (0.30
            * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 48_000.0).sin()
            * (1u64 << 30) as f64) as i32;
        pcm.push(v);
        pcm.push(v);
    }
    let mut out = vec![0u8; 2048];
    for k in 0..3 {
        match s.push(&pcm, &mut out) {
            Ok(Some(len)) => {
                println!("push {k}: packet {len} first {:02x?}", &out[..32.min(len)]);
                std::fs::write(
                    "/home/alpha/aks-infra/audio/aptx-native/.scratch/aptx_linux_frame.bin",
                    &out[..len],
                )
                .unwrap();
            }
            Ok(None) => println!("push {k}: no packet"),
            Err(e) => {
                println!("push {k}: {e:?}");
                break;
            }
        }
    }
}

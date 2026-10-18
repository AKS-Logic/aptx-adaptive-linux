use aptx_adaptive::{table_row_at_level, Config, DecoderLevel, Encoder, Profile, Stream};

fn main() {
    let cfg = Config {
        profile: Profile::Hq,
        sample_rate: 48_000,
        channels: 2,
        level: DecoderLevel::R2,
    };
    let mut row = table_row_at_level(cfg, 656, 2).expect("25 ms row");
    // 214 kbps makes the composer pack exactly 150 blocks (1200 samples) in
    // the 656-byte packet, matching the vendor 25 ms stream.
    row.bitrate = 214_000;
    println!("row period {} packet {} bitrate {} samples/ch {}",
        row.period_ms, row.packet_bytes, row.bitrate, row.samples_per_channel);
    let core = Encoder::new(cfg, row.bitrate).unwrap();
    let mut s = Stream::with_core(core, cfg, row).with_budget(656);
    let per = s.samples_per_packet();
    let secs = 4usize;
    let packets = secs * 1000 / row.period_ms as usize;
    let mut out_file = Vec::new();
    let mut out = vec![0u8; 2048];
    let mut phase = 0usize;
    let mut produced = 0usize;
    while produced < packets {
        let mut pcm = Vec::with_capacity(per * 2);
        for i in 0..per {
            let n = phase + i;
            let v = (0.30
                * (2.0 * std::f64::consts::PI * 1000.0 * n as f64 / 48_000.0).sin()
                * (1u64 << 30) as f64) as i32;
            pcm.push(v);
            pcm.push(v);
        }
        phase += per;
        match s.push(&pcm, &mut out) {
            Ok(Some(len)) => {
                out_file.extend_from_slice(&[0u8; 8]);
                out_file.extend_from_slice(&out[..len]);
                produced += 1;
            }
            Ok(None) => {}
            Err(e) => { eprintln!("error {e:?}"); break; }
        }
    }
    std::fs::write("/home/alpha/aks-infra/audio/aptx-native/.scratch/aptx_linux_1k_4s.bin", &out_file).unwrap();
    println!("wrote packets={produced} bytes={} path=aptx_linux_1k_4s.bin", out_file.len());
}

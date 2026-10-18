use aptx_adaptive::{Config, DecoderLevel, Encoder, Profile};

fn swap16(src: &[u8], dst: &mut [u8]) {
    for (i, c) in src.chunks_exact(2).enumerate() {
        dst[2 * i] = c[1];
        dst[2 * i + 1] = c[0];
    }
}

fn main() {
    let cfg = Config { profile: Profile::Hq, sample_rate: 48_000, channels: 2, level: DecoderLevel::R2 };
    let mut e = Encoder::new(cfg, 214_000).expect("encoder");
    e.prepare(656).expect("prepare");
    let mut window = vec![0u8; 656 + 64];
    let mut file = Vec::new();
    let mut phase = 0usize;
    let mut packets = 0usize;
    let mut written_early = 0usize;
    while packets < 160 {
        let mut pcm = Vec::with_capacity(2400);
        for _ in 0..1200 {
            let v = (0.30
                * (2.0 * std::f64::consts::PI * 1000.0 * phase as f64 / 48_000.0).sin()
                * (1u64 << 23) as f64) as i32;
            pcm.push(v);
            pcm.push(v);
            phase += 1;
        }
        let p = e.encode_packet(&pcm, &mut window).expect("encode");
        let mut w = p.written;
        if w == 0 {
            w = e.flush(&mut window).expect("flush");
        } else {
            written_early += 1;
        }
        if w == 0 {
            eprintln!("no packet at {packets}");
            break;
        }
        file.extend_from_slice(&[0u8; 8]);
        let mut sw = vec![0u8; w];
        swap16(&window[..w], &mut sw);
        file.extend_from_slice(&sw);
        packets += 1;
    }
    let path = "/home/alpha/aks-infra/audio/aptx-native/.scratch/aptx_forced_1k_4s.bin";
    std::fs::write(path, &file).unwrap();
    println!("packets={packets} bytes={} written_early={written_early} -> {path}", file.len());
}

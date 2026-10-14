mod support {
    pub mod alloc_count;
}

use aptx_adaptive::{Config, DecoderLevel, Encoder, Profile, Stream, packet_budget, table_row};
use support::alloc_count::count;

const fn config(level: DecoderLevel, profile: Profile) -> Config {
    Config {
        profile,
        sample_rate: 48_000,
        channels: 2,
        level,
    }
}

#[test]
fn switch_does_not_allocate() {
    for (level, hq_bytes) in [(DecoderLevel::R1, 656), (DecoderLevel::R2, 656)] {
        let budget = packet_budget(895);
        let hq = table_row(config(level, Profile::Hq), budget).unwrap();
        let ll = table_row(config(level, Profile::Ll), budget).unwrap();
        let mut s = Stream::new(config(level, Profile::Hq), budget).unwrap();
        let mut out = vec![0u8; 2048];
        let pcm: Vec<i32> = (0..96).map(|i| ((i * 7919) % 40_000) - 20_000).collect();
        let mut sizes = [[0usize; 2]; 3];
        let run = |s: &mut Stream<Encoder>, out: &mut [u8], counts: &mut [usize; 2]| {
            for _ in 0..1000 {
                match s.push(&pcm, out).unwrap() {
                    Some(n) if n == hq_bytes => counts[0] += 1,
                    Some(348) => counts[1] += 1,
                    Some(n) => panic!("packet of {n}"),
                    None => {}
                }
            }
        };
        let calls = count(|| {
            let [a, b, c] = &mut sizes;
            run(&mut s, &mut out, a);
            s.reset(config(level, Profile::Ll), ll).unwrap();
            run(&mut s, &mut out, b);
            s.reset(config(level, Profile::Hq), hq).unwrap();
            run(&mut s, &mut out, c);
        });
        assert_eq!(calls, 0, "{level:?}: allocations during the switch");
        let per_s = 1000 / hq.period_ms as usize;
        assert!(
            sizes[0][0] + 1 >= per_s && sizes[0][1] == 0,
            "{level:?} {sizes:?}"
        );
        assert!(sizes[1][1] >= 99 && sizes[1][0] <= 1, "{level:?} {sizes:?}");
        assert!(
            sizes[2][0] + 1 >= per_s && sizes[2][1] <= 1,
            "{level:?} {sizes:?}"
        );
    }
}

#[test]
fn new_rate_playback_and_changes_stay_bounded_without_allocations() {
    for (rate, samples) in [(44_100, 88), (96_000, 192)] {
        let hq = Config {
            sample_rate: rate,
            ..config(DecoderLevel::R2, Profile::Hq)
        };
        let ll = Config {
            profile: Profile::Ll,
            ..hq
        };
        let mut stream = Stream::new(hq, 875).unwrap();
        let pcm: Vec<i32> = (0..samples).map(|i| (i * 7919) % 40_000 - 20_000).collect();
        let mut out = [0; 1024];
        let mut packets = [0; 3];
        let mut drained = 0;
        let allocations = count(|| {
            for (phase, total) in packets.iter_mut().enumerate() {
                if rate == 44_100 {
                    let c = if phase == 1 { ll } else { hq };
                    stream.reset(c, table_row(c, 875).unwrap()).unwrap();
                }
                let mut gap = 0;
                for block in 0..10_000 {
                    if block % 1000 == 0 {
                        assert!(stream.request_level(if block % 2000 == 0 { 2 } else { 5 }));
                    }
                    gap += 1;
                    if let Some(n) = stream.push(&pcm, &mut out).unwrap() {
                        assert_eq!(
                            n,
                            if rate == 44_100 && phase == 1 {
                                348
                            } else {
                                656
                            }
                        );
                        assert!(gap <= 30, "{rate} stalled for {gap} blocks");
                        gap = 0;
                        *total += 1;
                    }
                }
            }
            while stream.push(&[], &mut out).unwrap().is_some() {
                drained += 1;
                assert!(drained <= 1, "{rate} queued more than one packet");
            }
        });
        assert_eq!(allocations, 0, "{rate} allocations or buffer growth");
        assert!(
            packets.iter().all(|&n| (300..=1500).contains(&n)),
            "{rate} {packets:?}"
        );
        assert_eq!(stream.config(), hq);
    }
}

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

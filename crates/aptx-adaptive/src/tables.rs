//! Mode and level tables (spec `a2dp-integration.md` §4).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Hq,
    Ll,
}

impl Profile {
    /// Header byte 7 (§4.3).
    #[must_use]
    pub fn mode_byte(self) -> u8 {
        match self {
            Self::Hq => 0xAE,
            Self::Ll => 0xAD,
        }
    }
}

/// Decoder compatibility level (§1.3). Changes the bitstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderLevel {
    /// "1.1 sink", e.g. the Px7 S2e.
    R1,
    R2,
}

pub const DEFAULT_QUALITY: u8 = 3;
const MAX_QUALITY: u8 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub bitrate: u32,
    pub period_ms: u32,
    pub samples_per_channel: u32,
    pub packet_bytes: usize,
    /// Header byte 3.
    pub cfg: u8,
}

impl Row {
    /// Header byte 2.
    ///
    /// # Panics
    /// If the period is over 63 ms, which no table row is.
    #[must_use]
    pub fn period_byte(&self) -> u8 {
        u8::try_from(self.period_ms * 4).expect("table periods are at most 50 ms")
    }

    /// Interleaved samples consumed per packet (§4.3).
    #[must_use]
    pub fn interleaved_samples(&self, rate: u32, channels: u32) -> usize {
        (u64::from(self.period_ms) * u64::from(rate) * u64::from(channels) / 1000) as usize
    }

    /// Rounds the packet size down to a multiple of 4 and fits it in `budget` (§4). The
    /// recomputed bitrate is the spec's "roughly" formula.
    #[must_use]
    pub fn fit(self, budget: usize) -> Self {
        let size = self.packet_bytes & !3;
        if size <= budget {
            return Self {
                packet_bytes: size,
                ..self
            };
        }
        let size = budget & !3;
        let bits_per_s = size as u64 * 8 * 1000 / u64::from(self.period_ms);
        Self {
            packet_bytes: size,
            bitrate: (bits_per_s as u32).saturating_sub(25_000),
            ..self
        }
    }
}

/// Packet budget for a link MTU: room for the 8-byte header, clamped to 128..=2008 (§2).
#[must_use]
pub fn packet_budget(mtu: usize) -> usize {
    mtu.saturating_sub(8).clamp(128, 2008)
}

/// Link state that picks the table index (§4.1, §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent link flags straight from the spec"
)]
pub struct Link {
    pub qhs: bool,
    pub twm: bool,
    /// Only affects HQ.
    pub rate_96k: bool,
    /// Latency state 1 (§8.3). Only affects LL.
    pub enhanced_ll: bool,
}

/// Table index. R1 sinks only look at QHS.
#[must_use]
pub fn index(profile: Profile, level: DecoderLevel, link: Link) -> usize {
    let top = match profile {
        Profile::Hq => link.rate_96k,
        Profile::Ll => link.enhanced_ll,
    };
    match level {
        DecoderLevel::R1 => 2 * usize::from(link.qhs),
        DecoderLevel::R2 => {
            1 + usize::from(link.qhs) + 2 * usize::from(link.twm) + 4 * usize::from(top)
        }
    }
}

/// The table row for a profile, index and quality level 1..=7.
#[must_use]
pub fn row(profile: Profile, index: usize, quality: u8) -> Option<Row> {
    if !(1..=MAX_QUALITY).contains(&quality) {
        return None;
    }
    let (rates, packets) = match profile {
        Profile::Hq => (HQ.get(index)?, HQ_PACKET.get(index)?),
        Profile::Ll => (LL.get(index)?, LL_PACKET.get(index)?),
    };
    let rate = rates[usize::from(quality - 1)];
    let packet = packets[usize::from(quality >= 3)];
    Some(Row {
        bitrate: rate.kbps * 1000,
        period_ms: rate.period_ms,
        samples_per_channel: rate.samples_per_channel,
        packet_bytes: packet.bytes,
        cfg: packet.cfg,
    })
}

#[derive(Clone, Copy)]
struct Rate {
    kbps: u32,
    period_ms: u32,
    samples_per_channel: u32,
}

const fn r(kbps: u32, period_ms: u32, samples_per_channel: u32) -> Rate {
    Rate {
        kbps,
        period_ms,
        samples_per_channel,
    }
}

#[derive(Clone, Copy)]
struct Packet {
    bytes: usize,
    cfg: u8,
}

const fn p(bytes: usize, cfg: u8) -> Packet {
    Packet { bytes, cfg }
}

/// Packet size and cfg for levels 1..=2 and 3..=7.
type Packets = [Packet; 2];

const fn same(p: Packet) -> Packets {
    [p, p]
}

#[rustfmt::skip]
const HQ: [[Rate; 7]; 9] = [
    [r(281, 18, 864), r(318, 16, 768), r(364, 14, 672), r(427, 12, 576), r(427, 12, 576), r(427, 12, 576), r(427, 12, 576)],
    [r(104, 50, 2400), r(204, 25, 1200), r(270, 19, 912), r(340, 15, 720), r(427, 12, 576), r(427, 12, 576), r(427, 12, 576)],
    [r(110, 50, 2400), r(235, 25, 1200), r(290, 20, 960), r(350, 16, 768), r(430, 13, 624), r(430, 13, 624), r(430, 13, 624)],
    [r(104, 50, 2400), r(204, 25, 1200), r(270, 19, 912), r(340, 15, 720), r(427, 12, 576), r(427, 12, 576), r(427, 12, 576)],
    [r(110, 50, 2400), r(235, 25, 1200), r(290, 20, 960), r(350, 16, 768), r(430, 13, 624), r(430, 13, 624), r(430, 13, 624)],
    [r(128, 40, 3840), r(255, 20, 1920), r(340, 15, 1440), r(420, 12, 1152), r(646, 8, 768), r(646, 8, 768), r(646, 8, 768)],
    [r(142, 40, 3840), r(284, 20, 1920), r(460, 16, 1536), r(570, 13, 1248), r(820, 9, 864), r(820, 9, 864), r(820, 9, 864)],
    [r(255, 20, 1920), r(255, 20, 1920), r(340, 15, 1440), r(390, 10, 960), r(510, 10, 960), r(510, 10, 960), r(510, 10, 960)],
    [r(284, 20, 1920), r(284, 20, 1920), r(460, 16, 1536), r(570, 13, 1248), r(650, 10, 960), r(650, 10, 960), r(650, 10, 960)],
];

const HQ_PACKET: [Packets; 9] = [
    same(p(656, 1)),
    same(p(656, 1)),
    same(p(760, 5)),
    same(p(656, 1)),
    same(p(760, 5)),
    same(p(656, 1)),
    [p(760, 5), p(960, 6)],
    same(p(656, 1)),
    [p(760, 5), p(960, 6)],
];

#[rustfmt::skip]
const LL: [[Rate; 7]; 9] = [
    [r(265, 10, 480); 7],
    [r(220, 12, 576), r(220, 12, 576), r(270, 10, 480), r(270, 10, 480), r(270, 10, 480), r(270, 10, 480), r(270, 10, 480)],
    [r(290, 15, 720), r(290, 15, 720), r(320, 12, 576), r(320, 10, 480), r(320, 10, 480), r(320, 10, 480), r(320, 10, 480)],
    [r(214, 13, 624); 7],
    [r(290, 15, 720), r(290, 15, 720), r(320, 13, 624), r(320, 13, 624), r(320, 13, 624), r(320, 13, 624), r(320, 13, 624)],
    [r(300, 8, 384); 7],
    [r(300, 8, 384); 7],
    [r(214, 13, 624); 7],
    [r(290, 15, 720), r(290, 15, 720), r(320, 13, 624), r(320, 13, 624), r(320, 13, 624), r(320, 13, 624), r(320, 13, 624)],
];

const LL_PACKET: [Packets; 9] = [
    same(p(348, 0)),
    same(p(348, 0)),
    same(p(560, 4)),
    same(p(348, 0)),
    same(p(560, 4)),
    same(p(348, 0)),
    same(p(348, 7)),
    same(p(348, 0)),
    same(p(560, 4)),
];

#[cfg(test)]
mod tests {
    use super::*;

    const PX7: Row = Row {
        bitrate: 364_000,
        period_ms: 14,
        samples_per_channel: 672,
        packet_bytes: 656,
        cfg: 1,
    };

    #[test]
    fn r1_hq_default_is_the_px7_row() {
        let idx = index(Profile::Hq, DecoderLevel::R1, Link::default());
        assert_eq!(idx, 0);
        let r = row(Profile::Hq, idx, DEFAULT_QUALITY).unwrap();
        assert_eq!(r, PX7);
        assert_eq!(r.period_byte(), 0x38);
        assert_eq!(r.interleaved_samples(48_000, 2), 1344);
    }

    #[test]
    fn indices() {
        let all = Link {
            qhs: true,
            twm: true,
            rate_96k: true,
            enhanced_ll: true,
        };
        assert_eq!(index(Profile::Hq, DecoderLevel::R1, all), 2);
        assert_eq!(index(Profile::Ll, DecoderLevel::R1, all), 2);
        assert_eq!(index(Profile::Hq, DecoderLevel::R2, Link::default()), 1);
        assert_eq!(index(Profile::Hq, DecoderLevel::R2, all), 8);
        let hq_96k = Link {
            rate_96k: true,
            ..Link::default()
        };
        assert_eq!(index(Profile::Hq, DecoderLevel::R2, hq_96k), 5);
        assert_eq!(index(Profile::Ll, DecoderLevel::R2, hq_96k), 1);
        let ll_enhanced = Link {
            twm: true,
            enhanced_ll: true,
            ..Link::default()
        };
        assert_eq!(index(Profile::Ll, DecoderLevel::R2, ll_enhanced), 7);
        assert_eq!(index(Profile::Hq, DecoderLevel::R2, ll_enhanced), 3);
    }

    #[test]
    fn r2_hq_level_3() {
        let r = row(Profile::Hq, 1, 3).unwrap();
        assert_eq!(
            (
                r.bitrate,
                r.period_ms,
                r.samples_per_channel,
                r.packet_bytes
            ),
            (270_000, 19, 912, 656)
        );
    }

    #[test]
    fn split_packet_sizes_follow_level() {
        assert_eq!(row(Profile::Hq, 6, 2).unwrap().packet_bytes, 760);
        assert_eq!(row(Profile::Hq, 6, 3).unwrap().cfg, 6);
        let ll = row(Profile::Ll, 2, 3).unwrap();
        assert_eq!((ll.bitrate, ll.period_ms, ll.cfg), (320_000, 12, 4));
        assert_eq!(row(Profile::Ll, 2, 4).unwrap().period_ms, 10);
    }

    #[test]
    fn samples_match_period_at_table_rate() {
        for (rows, rate) in [(&HQ[..5], 48), (&HQ[5..], 96), (&LL[..], 48)] {
            for r in rows.iter().flatten() {
                assert_eq!(r.period_ms * rate, r.samples_per_channel);
            }
        }
    }

    #[test]
    fn no_level_0_or_8() {
        assert_eq!(row(Profile::Hq, 0, 0), None);
        assert_eq!(row(Profile::Hq, 0, 8), None);
        assert_eq!(row(Profile::Ll, 9, 3), None);
        assert_eq!(row(Profile::Ll, 6, 7).unwrap().cfg, 7);
    }

    #[test]
    fn budget_clamps() {
        assert_eq!(packet_budget(895), 887);
        assert_eq!(packet_budget(10), 128);
        assert_eq!(packet_budget(4000), 2008);
    }

    #[test]
    fn fit_keeps_rows_that_fit() {
        assert_eq!(PX7.fit(656), PX7);
    }

    #[test]
    fn fit_shrinks_packet_and_bitrate() {
        let r = PX7.fit(498);
        assert_eq!(r.packet_bytes, 496);
        assert_eq!(r.bitrate, 496 * 8000 / 14 - 25_000);
        assert_eq!(r.period_ms, 14);
    }
}

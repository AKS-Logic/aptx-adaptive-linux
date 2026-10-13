//! A2DP capability negotiation (spec `a2dp-integration.md` §1); offsets start at the vendor header.

use aptx_adaptive::DecoderLevel;

use crate::ffi::{SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR, SPA_AUDIO_FORMAT_S24_32_LE};
use crate::ids::{CAPS_LEN, VENDOR_HEADER};
use crate::pod::RawFormat;

pub const STREAM: RawFormat = RawFormat {
    format: SPA_AUDIO_FORMAT_S24_32_LE,
    rate: 48_000,
    positions: &[SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR],
};

const RATES: usize = 6;
const CHANNEL_MODES: usize = 7;

const RATE_MASK: u8 = 0xF8;
const RATE_48K: u8 = 0x10;

pub const SOURCE_TYPE_HOST: u8 = 0x02;

const CHANNEL_STEREO: u8 = 0x02;
const CHANNEL_JOINT_STEREO: u8 = 0x08;
const CHANNEL_MASK: u8 = 0x3F;

const APTX_DATA: usize = 8;
const EXT_VERSION: usize = 15;
const FEATURES: usize = 16;
const R2_DATA_END: usize = 26;

const TARGET_LATENCY_DEFAULTS: [(usize, u8); 4] = [(8, 0x50), (9, 0x64), (10, 0x64), (11, 0x64)];

const TTP_DEFAULTS: [u8; 6] = [0x50, 0x64, 0x64, 0x64, 0xff, 0xff];

const FEATURES_R2_0: u32 = 0x0F00_0000;
const FEATURES_LOW_BYTE: u32 = 0xff;

const R1_DATA: [u8; 9] = {
    let t = TTP_DEFAULTS;
    [t[0], t[1], t[2], t[3], t[4], t[5], 0, 0x00, 0xaa]
};

const fn r2_data(features: u32) -> [u8; R2_DATA_END - APTX_DATA] {
    let f = features.to_le_bytes();
    let t = TTP_DEFAULTS;
    [
        t[0], t[1], t[2], t[3], t[4], t[5], 0, 1, f[0], f[1], f[2], f[3], 0x02, 0x03, 0x03, 0x03,
        0x00, 0xaa,
    ]
}

const LL0_MIN: u8 = 0x50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapsError {
    /// Too short or malformed (`-EINVAL`).
    Invalid,
    /// Well formed but not something we can encode (`-ENOTSUP`).
    Unsupported,
}

/// A checked capability element or configuration; bytes past [`CAPS_LEN`] are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps<'a>(&'a [u8; CAPS_LEN]);

impl<'a> Caps<'a> {
    /// # Errors
    /// `Unsupported` for another vendor codec, `Invalid` if it's under [`CAPS_LEN`] bytes.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, CapsError> {
        match bytes.get(..VENDOR_HEADER.len()) {
            None => return Err(CapsError::Invalid),
            Some(h) if h != VENDOR_HEADER => return Err(CapsError::Unsupported),
            Some(_) => {}
        }
        bytes.first_chunk().map(Self).ok_or(CapsError::Invalid)
    }

    #[must_use]
    pub fn rates(self) -> u8 {
        self.0[RATES] & RATE_MASK
    }

    #[must_use]
    pub fn channel_modes(self) -> u8 {
        self.0[CHANNEL_MODES] & CHANNEL_MASK
    }

    /// Decoder compatibility level (§1.3), determined by the extension version byte.
    #[must_use]
    pub fn decoder_level(self) -> DecoderLevel {
        if self.0[EXT_VERSION] == 1 {
            DecoderLevel::R2
        } else {
            DecoderLevel::R1
        }
    }

    fn features(self) -> u32 {
        let b = self.0;
        u32::from_le_bytes([
            b[FEATURES],
            b[FEATURES + 1],
            b[FEATURES + 2],
            b[FEATURES + 3],
        ])
    }
}

fn element(len: usize, rate: u8, channel_mode: u8, aptx_data: &[u8]) -> Vec<u8> {
    let mut c = vec![0; len];
    c[..VENDOR_HEADER.len()].copy_from_slice(&VENDOR_HEADER);
    c[RATES] = rate;
    c[CHANNEL_MODES] = channel_mode;
    c[APTX_DATA..APTX_DATA + aptx_data.len()].copy_from_slice(aptx_data);
    c
}

#[must_use]
pub fn fill_caps() -> Vec<u8> {
    element(
        CAPS_LEN,
        RATE_48K | SOURCE_TYPE_HOST,
        CHANNEL_STEREO | CHANNEL_JOINT_STEREO,
        &R1_DATA,
    )
}

/// # Errors
/// As for [`Caps::parse`], or `Unsupported` without 48 kHz and stereo or joint stereo.
pub fn select_config(bytes: &[u8]) -> Result<Vec<u8>, CapsError> {
    let caps = Caps::parse(bytes)?;
    if caps.rates() & RATE_48K == 0 {
        return Err(CapsError::Unsupported);
    }
    let mode = [CHANNEL_STEREO, CHANNEL_JOINT_STEREO]
        .into_iter()
        .find(|m| caps.channel_modes() & m != 0)
        .ok_or(CapsError::Unsupported)?;
    let rate = RATE_48K | SOURCE_TYPE_HOST;
    let config = match caps.decoder_level() {
        DecoderLevel::R2 => {
            // Our low byte is 0, so the AND is always 0; that's what §7J asks for.
            let features = (FEATURES_R2_0 & !FEATURES_LOW_BYTE)
                | (FEATURES_R2_0 & caps.features() & FEATURES_LOW_BYTE);
            element(bytes.len(), rate, mode, &r2_data(features))
        }
        DecoderLevel::R1 => {
            let mut c = element(bytes.len(), rate, mode, &caps.0[APTX_DATA..R2_DATA_END]);
            for (i, default) in TARGET_LATENCY_DEFAULTS {
                if c[i] == 0 {
                    c[i] = default;
                }
            }
            c[8] = c[8].max(LL0_MIN);
            c
        }
    };
    Ok(config)
}

///
/// # Errors
/// As for [`Caps::parse`], or `Invalid` unless it picks exactly 48 kHz and exactly one of
/// stereo or joint stereo. The source type bits aren't checked. A sink-initiated stream may
/// carry source type 0, whose link-clock timing a host encoder can't meet (§7E); it's still
/// accepted and streamed with the host (type 2) rules. The level comes from the config's
/// extension byte: PipeWire doesn't pass the sink's caps here or to `init`. See
/// `spec/QUESTIONS.md`.
pub fn validate_config(config: &[u8]) -> Result<DecoderLevel, CapsError> {
    let c = Caps::parse(config)?;
    let mode_ok = matches!(c.channel_modes(), CHANNEL_STEREO | CHANNEL_JOINT_STEREO);
    if c.rates() != RATE_48K || !mode_ok {
        return Err(CapsError::Invalid);
    }
    Ok(c.decoder_level())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmStream {
    pub raw_audio: bool,
    pub format: u32,
    pub rate: u32,
    pub channels: u32,
}

///
/// # Errors
/// As for [`validate_config`], or `Invalid` on a mismatch.
pub fn check_stream(config: &[u8], stream: &PcmStream) -> Result<DecoderLevel, CapsError> {
    let level = validate_config(config)?;
    let matches = stream.raw_audio
        && stream.format == STREAM.format
        && stream.rate == STREAM.rate
        && stream.channels as usize == STREAM.positions.len();
    if matches {
        Ok(level)
    } else {
        Err(CapsError::Invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{PX7_S2E, PX7_S2E_CONFIG};

    const SETUP_PREFS: usize = 20;
    const END_MARKER: usize = 24;

    fn r2_caps(features: u32) -> [u8; CAPS_LEN] {
        let mut c = [0; CAPS_LEN];
        c[..VENDOR_HEADER.len()].copy_from_slice(&VENDOR_HEADER);
        c[RATES] = 0x1a;
        c[CHANNEL_MODES] = 0x22;
        c[APTX_DATA..EXT_VERSION].copy_from_slice(&[0x50, 0x64, 0x64, 0x64, 0xff, 0xff, 0]);
        c[EXT_VERSION] = 1;
        c[FEATURES..SETUP_PREFS].copy_from_slice(&features.to_le_bytes());
        c[SETUP_PREFS..END_MARKER].copy_from_slice(&[1, 2, 3, 4]);
        c[END_MARKER..R2_DATA_END].copy_from_slice(&[0x00, 0xaa]);
        c
    }

    #[test]
    fn px7_s2e_is_r1() {
        let c = Caps::parse(&PX7_S2E).unwrap();
        assert_eq!(c.rates(), 0x18);
        assert_eq!(c.channel_modes(), 0x0a);
        assert_eq!(c.decoder_level(), DecoderLevel::R1);
    }

    #[test]
    fn px7_s2e_select_config_matches_spec() {
        let config = select_config(&PX7_S2E).unwrap();
        assert_eq!(config, PX7_S2E_CONFIG);
        assert_eq!(validate_config(&config), Ok(DecoderLevel::R1));
    }

    #[test]
    fn r2_sinks_get_r2_level() {
        for features in [0x0F00_0000, 0x0F00_0003, 0x0F00_0017, 0x0F00_0083] {
            let c = r2_caps(features);
            assert_eq!(Caps::parse(&c).unwrap().decoder_level(), DecoderLevel::R2);
        }
    }

    #[test]
    fn r1_marker_is_not_a_features_word() {
        let mut c = PX7_S2E;
        assert_eq!(c[FEATURES], 0xaa);
        c[EXT_VERSION] = 2;
        assert_eq!(Caps::parse(&c).unwrap().decoder_level(), DecoderLevel::R1);
    }

    #[test]
    fn parse_ignores_trailing_bytes() {
        let long = [&PX7_S2E[..], &[0x55; 4]].concat();
        let config = select_config(&long).unwrap();
        assert_eq!(config.len(), long.len());
        assert_eq!(config[..CAPS_LEN], PX7_S2E_CONFIG);
        assert_eq!(config[CAPS_LEN..], [0; 4]);
        let long_config = [&PX7_S2E_CONFIG[..], &[0x55; 4]].concat();
        assert_eq!(validate_config(&long_config), Ok(DecoderLevel::R1));
        assert_eq!(select_config(&PX7_S2E[..39]), Err(CapsError::Invalid));
    }

    #[test]
    fn r2_select_config_sends_own_block() {
        let mut caps = r2_caps(0x0F00_0017);
        caps[APTX_DATA..APTX_DATA + 6].copy_from_slice(&[0x37, 0x6a, 0, 0xaf, 0x64, 0x4b]);
        caps[30] = 0x55;
        let config = select_config(&caps).unwrap();
        #[rustfmt::skip]
        let want: [u8; CAPS_LEN] = [
            0xd7, 0, 0, 0, 0xad, 0, 0x12, 0x02,
            0x50, 0x64, 0x64, 0x64, 0xff, 0xff, 0x00, 0x01,
            0x00, 0x00, 0x00, 0x0f, 0x02, 0x03, 0x03, 0x03, 0x00, 0xaa,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(config, want);
        assert_eq!(validate_config(&config), Ok(DecoderLevel::R2));
    }

    #[test]
    fn r2_features_low_byte_is_anded() {
        for sink in [0x0F00_0000, 0x0F00_0003, 0x0F00_0017, 0xFFFF_FFFF] {
            let config = select_config(&r2_caps(sink)).unwrap();
            let got = u32::from_le_bytes(config[FEATURES..SETUP_PREFS].try_into().unwrap());
            assert_eq!(got, 0x0F00_0000, "{sink:#010x}");
        }
    }

    #[test]
    fn r2_1_sink_example() {
        #[rustfmt::skip]
        let caps: [u8; CAPS_LEN] = [
            0xd7, 0, 0, 0, 0xad, 0, 0x19, 0x0a,
            0x50, 0x64, 0x64, 0x64, 0xff, 0xff, 0x00, 0x01,
            0x03, 0x00, 0x00, 0x0f, 0x02, 0x03, 0x03, 0x03, 0x00, 0xaa,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let c = Caps::parse(&caps).unwrap();
        assert_eq!(c.decoder_level(), DecoderLevel::R2);
        assert_eq!(c.features(), 0x0F00_0003);
        let config = select_config(&caps).unwrap();
        assert_eq!(config[RATES..=CHANNEL_MODES], [0x12, CHANNEL_STEREO]);
        assert_eq!(config[FEATURES], 0);
        assert_eq!(config[EXT_VERSION], 1);
        assert_eq!(validate_config(&config), Ok(DecoderLevel::R2));
    }

    #[test]
    fn r1_select_config_echoes_8_to_25() {
        let mut caps = PX7_S2E;
        caps[20] = 0x55;
        caps[25] = 0x66;
        caps[26] = 0x77;
        caps[39] = 0x88;
        let mut want = PX7_S2E_CONFIG;
        want[20] = 0x55;
        want[25] = 0x66;
        assert_eq!(select_config(&caps).unwrap(), want);
        assert_eq!(validate_config(&want), Ok(DecoderLevel::R1));
    }

    #[test]
    fn falls_back_to_joint_stereo() {
        let mut caps = PX7_S2E;
        caps[CHANNEL_MODES] = CHANNEL_JOINT_STEREO | 0x01;
        assert_eq!(
            select_config(&caps).unwrap()[CHANNEL_MODES],
            CHANNEL_JOINT_STEREO
        );
    }

    #[test]
    fn rejects_unusable_caps() {
        let mut no_48k = PX7_S2E;
        no_48k[RATES] = 0x08;
        assert_eq!(select_config(&no_48k), Err(CapsError::Unsupported));
        let mut mono = PX7_S2E;
        mono[CHANNEL_MODES] = 0x01 | 0x04 | 0x10 | 0x20;
        assert_eq!(select_config(&mono), Err(CapsError::Unsupported));
        assert_eq!(select_config(&PX7_S2E[..34]), Err(CapsError::Invalid));
    }

    #[test]
    fn rejects_other_vendor_codecs() {
        let aptx_classic = [0x4f, 0, 0, 0, 0x01, 0, 0x32];
        assert_eq!(select_config(&aptx_classic), Err(CapsError::Unsupported));
        let aptx_hd = [0xD7, 0, 0, 0, 0x24, 0, 0];
        assert_eq!(select_config(&aptx_hd), Err(CapsError::Unsupported));
        assert_eq!(select_config(&VENDOR_HEADER[..2]), Err(CapsError::Invalid));
    }

    #[test]
    fn validate_needs_one_mode_and_48k_only() {
        let mut c = PX7_S2E_CONFIG;
        c[CHANNEL_MODES] = 0x0a;
        assert_eq!(validate_config(&c), Err(CapsError::Invalid));
        c[CHANNEL_MODES] = CHANNEL_STEREO;
        c[RATES] = 0x18;
        assert_eq!(validate_config(&c), Err(CapsError::Invalid));
        c[RATES] = 0x12;
        assert_eq!(validate_config(&c), Ok(DecoderLevel::R1));
    }

    #[test]
    fn select_config_sets_source_type() {
        assert_eq!(PX7_S2E[RATES], 0x19);
        let config = select_config(&PX7_S2E).unwrap();
        assert_eq!(config[RATES], 0x12);
        assert_eq!(config, PX7_S2E_CONFIG);
        let mut caps = PX7_S2E;
        caps[RATES] = 0x17;
        assert_eq!(select_config(&caps).unwrap()[RATES], 0x12);
    }

    #[test]
    fn select_config_replaces_zero_latency() {
        let config = select_config(&PX7_S2E).unwrap();
        assert_eq!(config[8..14], [0x50, 0x6a, 0x64, 0xaf, 0x64, 0x4b]);
        let mut caps = PX7_S2E;
        caps[8] = 0x60;
        assert_eq!(select_config(&caps).unwrap()[8], 0x60);
        let mut caps = PX7_S2E;
        caps[8..14].copy_from_slice(&[0, 0, 0, 0, 0, 0x22]);
        let config = select_config(&caps).unwrap();
        assert_eq!(config[8..14], [0x50, 0x64, 0x64, 0x64, 0, 0x22]);
        caps[8..12].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        assert_eq!(
            select_config(&caps).unwrap()[8..14],
            [0x50, 0x22, 0x33, 0x44, 0, 0x22]
        );
    }

    #[test]
    fn validate_accepts_source_type_0_and_2() {
        for (byte6, ok) in [
            (0x10, true),
            (0x12, true),
            (0x02, false),
            (0x1a, false),
            (0x32, false),
        ] {
            let mut c = PX7_S2E_CONFIG;
            c[RATES] = byte6;
            assert_eq!(validate_config(&c).is_ok(), ok, "{byte6:#04x}");
        }
    }

    #[test]
    fn fill_caps_is_r1_layout_with_source_type_2() {
        let caps = fill_caps();
        #[rustfmt::skip]
        let want: [u8; CAPS_LEN] = [
            0xd7, 0, 0, 0, 0xad, 0, 0x12, 0x0a,
            0x50, 0x64, 0x64, 0x64, 0xff, 0xff, 0x00, 0x00,
            0xaa, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(caps, want);
        assert_eq!(
            Caps::parse(&caps).unwrap().decoder_level(),
            DecoderLevel::R1
        );
        let mut copied = caps.clone();
        copied[CHANNEL_MODES] = CHANNEL_STEREO;
        assert_eq!(validate_config(&copied), Ok(DecoderLevel::R1));
        let config = select_config(&caps).unwrap();
        assert_eq!(config, copied);
    }

    #[test]
    fn validate_accepts_sink_initiated_source_type_0() {
        let mut config = select_config(&fill_caps()).unwrap();
        config[RATES] = RATE_48K;
        assert_eq!(validate_config(&config), Ok(DecoderLevel::R1));
        let mut r2 = select_config(&r2_caps(0x0F00_0003)).unwrap();
        r2[RATES] = RATE_48K;
        assert_eq!(validate_config(&r2), Ok(DecoderLevel::R2));
    }

    #[test]
    fn rate_bits() {
        for (byte6, ok) in [(0x0a, false), (0x12, true), (0x22, false), (0x42, false)] {
            let mut c = PX7_S2E_CONFIG;
            c[RATES] = byte6;
            assert_eq!(validate_config(&c).is_ok(), ok, "{byte6:#04x}");
            let mut caps = PX7_S2E;
            caps[RATES] = byte6 | 0x01;
            assert_eq!(select_config(&caps).is_ok(), ok, "{byte6:#04x}");
        }
    }

    #[test]
    fn check_stream_requires_exact_match() {
        let ok = PcmStream {
            raw_audio: true,
            format: SPA_AUDIO_FORMAT_S24_32_LE,
            rate: 48_000,
            channels: 2,
        };
        assert_eq!(check_stream(&PX7_S2E_CONFIG, &ok), Ok(DecoderLevel::R1));
        for bad in [
            PcmStream {
                raw_audio: false,
                ..ok
            },
            PcmStream { rate: 44_100, ..ok },
            PcmStream { channels: 1, ..ok },
            PcmStream { format: 0, ..ok },
        ] {
            assert_eq!(check_stream(&PX7_S2E_CONFIG, &bad), Err(CapsError::Invalid));
        }
    }
}

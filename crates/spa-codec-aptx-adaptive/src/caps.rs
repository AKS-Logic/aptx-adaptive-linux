//! A2DP capability negotiation (spec `a2dp-integration.md` §1). Offsets are from the start of
//! the vendor header, the spec's "body".

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

const CHANNEL_STEREO: u8 = 0x02;
const CHANNEL_JOINT_STEREO: u8 = 0x08;
const CHANNEL_MASK: u8 = 0x3F;

const APTX_DATA: usize = 8;
const EXT_VERSION: usize = 15;
/// End of the TTP bytes plus the R1 end marker.
const R1_DATA_END: usize = 17;
/// End of the R2 extension block, including its end marker.
const R2_DATA_END: usize = 26;

/// What a source with an R1-only encoder sends from body 8 on (§1.4).
const R1_DEFAULT_DATA: [u8; 9] = [0x50, 0x64, 0x64, 0x64, 0xff, 0xff, 0x00, 0x00, 0xaa];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapsError {
    /// Too short or malformed (`-EINVAL`).
    Invalid,
    /// Well formed but not something we can encode (`-ENOTSUP`).
    Unsupported,
}

/// A capability element or configuration, checked for vendor, codec and length. Bytes past
/// [`CAPS_LEN`] are ignored.
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

    /// Decoder compatibility level (§1.3). Only the extension version byte decides it; on an
    /// R1 sink the features word is really the end marker. R2.2 sinks stay on R2 at 48 kHz.
    #[must_use]
    pub fn decoder_level(self) -> DecoderLevel {
        if self.0[EXT_VERSION] == 1 {
            DecoderLevel::R2
        } else {
            DecoderLevel::R1
        }
    }

    /// Body 8 up to the end marker: TTP bytes, plus on R2 the extension version, features word
    /// and setup preferences, all verbatim as an R2 source does (§1.4). We don't support most
    /// of the features the sink lists, and the spec doesn't say whether echoing them commits
    /// us to anything.
    fn aptx_data(self) -> &'a [u8] {
        let end = match self.decoder_level() {
            DecoderLevel::R1 => R1_DATA_END,
            DecoderLevel::R2 => R2_DATA_END,
        };
        &self.0[APTX_DATA..end]
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

/// Our source endpoint: 48 kHz, stereo or joint stereo, source type 0, and the R1 block a
/// public R1-only source sends.
#[must_use]
pub fn fill_caps() -> Vec<u8> {
    element(
        CAPS_LEN,
        RATE_48K,
        CHANNEL_STEREO | CHANNEL_JOINT_STEREO,
        &R1_DEFAULT_DATA,
    )
}

/// Picks 48 kHz and one 2-channel mode, and echoes the sink's own aptx data (§1.4, §1.5). The
/// result is as long as `caps`, with any bytes past [`CAPS_LEN`] zeroed, because PipeWire
/// only offers the codec when the lengths match.
///
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
    Ok(element(bytes.len(), RATE_48K, mode, caps.aptx_data()))
}

/// Checks a configuration and gives the decoder level the encoder needs.
///
/// # Errors
/// As for [`Caps::parse`], or `Invalid` unless it picks exactly 48 kHz and exactly one of
/// stereo or joint stereo.
pub fn validate_config(config: &[u8]) -> Result<DecoderLevel, CapsError> {
    let c = Caps::parse(config)?;
    let mode_ok = matches!(c.channel_modes(), CHANNEL_STEREO | CHANNEL_JOINT_STEREO);
    if c.rates() != RATE_48K || !mode_ok {
        return Err(CapsError::Invalid);
    }
    Ok(c.decoder_level())
}

/// The PCM stream PipeWire proposes in `init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmStream {
    pub raw_audio: bool,
    pub format: u32,
    pub rate: u32,
    pub channels: u32,
}

/// Checks the stream PipeWire proposes matches what the configuration asks for.
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

    const FEATURES: usize = 16;
    const SETUP_PREFS: usize = 20;
    const END_MARKER: usize = 24;

    /// R2-style caps: 48 kHz + 0x08, source type 2, stereo + TWS+, public TTP defaults,
    /// extension version 1, the given features word, setup prefs, end marker.
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
    fn r2_select_config_echoes_extension() {
        let caps = r2_caps(0x0F00_0003);
        let config = select_config(&caps).unwrap();
        assert_eq!(config.len(), CAPS_LEN);
        assert_eq!(config[RATES..=CHANNEL_MODES], [RATE_48K, CHANNEL_STEREO]);
        assert_eq!(config[8..26], caps[8..26]);
        assert!(config[26..].iter().all(|&b| b == 0));
        assert_eq!(validate_config(&config), Ok(DecoderLevel::R2));
    }

    #[test]
    fn r1_select_config_drops_trailing_bytes() {
        let mut caps = PX7_S2E;
        caps[20] = 0x55;
        assert_eq!(select_config(&caps).unwrap(), PX7_S2E_CONFIG);
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
    fn fill_caps_is_r1_source() {
        let caps = fill_caps();
        assert_eq!(caps.len(), CAPS_LEN);
        assert_eq!(caps[..6], VENDOR_HEADER);
        assert_eq!(
            caps[6..17],
            [0x10, 0x0a, 0x50, 0x64, 0x64, 0x64, 0xff, 0xff, 0, 0, 0xaa]
        );
        assert_eq!(
            Caps::parse(&caps).unwrap().decoder_level(),
            DecoderLevel::R1
        );
        assert!(select_config(&caps).is_ok());
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

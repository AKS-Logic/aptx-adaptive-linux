//! The two aptX Adaptive mode descriptors and their AVDTP bodies.
//!
//! Ported byte-for-byte from `a2dp-codec-aptx-adaptive-modes.h`:
//!
//! | | LL | Hi-Res |
//! |---|---|---|
//! | rate | 48 kHz | 96 kHz, 48 kHz fallback |
//! | frame | 10 ms / 480 samples | 8 ms / 768, 25 ms / 1200 fallback |
//! | native calls (8 samples each) | 60 | 96, 150 |
//! | codec payload | 348 B | 656 B |
//! | TTP lead | 60 ms (window 50-85) | 200 ms |
//! | SSRC, source type 2 | 0xA1 | 0xA3 |
//!
//! With source type 0 (the transport the C plugin runs) every mode sends
//! SSRC 0xAE, which is what the known-good phone capture carries.
#![allow(
    clippy::doc_markdown,
    reason = "protocol identifiers are used as prose in the mode table"
)]

use std::ffi::CStr;

use crate::ffi::media_codec;
use crate::ids::VENDOR_HEADER;

/// Codec-specific AVDTP body, excluding the 3-byte Media Codec header.
pub(crate) const BODY_SIZE: usize = 40;

pub(crate) const OFF_RATE_SOURCE: usize = 6;
pub(crate) const OFF_CHANNEL_MODE: usize = 7;
pub(crate) const OFF_TTP_LL0: usize = 8;
pub(crate) const OFF_EXT_VERSION: usize = 15;
pub(crate) const OFF_FEATURES: usize = 16;
pub(crate) const OFF_SETUP_PREF: usize = 20;
pub(crate) const OFF_END_MARKER: usize = 24;
pub(crate) const OFF_BITS_PER_SAMPLE: usize = 26;

pub(crate) const SR_44100: u8 = 0x08;
pub(crate) const SR_48000: u8 = 0x10;
pub(crate) const SR_96000: u8 = 0x20;
pub(crate) const SR_192000: u8 = 0x40;
pub(crate) const SR_MASK: u8 = 0xF8;

/// Body byte 6 low bits: 0 is controller/link-clock timing, 2 is host timing.
pub(crate) const ST_MASK: u8 = 0x07;
pub(crate) const ST_HOST: u8 = 0x02;

pub(crate) const CH_MONO: u8 = 0x01;
pub(crate) const CH_STEREO: u8 = 0x02;
pub(crate) const CH_JOINT: u8 = 0x08;
pub(crate) const CH_MASK: u8 = 0x3F;

pub(crate) const FEATURES_SOURCE: u32 = 0x0F00_0092;
pub(crate) const FEATURES_UNKNOWN: u32 = 0xFFFF_FFFF;

/// Body bytes 8..13: LL0 60 ms, LL1 80 ms, HQ 100 x 2 ms, TWS 100 x 2 ms, then two unread.
pub(crate) const LATENCY_BYTES: [u8; 6] = [0x3c, 0x50, 0x64, 0x64, 0xff, 0xff];

const EXT_VERSION_R2: u8 = 0x01;
const SETUP_PREF: [u8; 4] = [0x02, 0x03, 0x03, 0x03];
const END_MARKER: [u8; 2] = [0x00, 0xaa];
const BITS_24: u8 = 0x02;

pub(crate) const SSRC_LL0: u32 = 0x0000_00A1;
pub(crate) const SSRC_LL1: u32 = 0x0000_00A2;
pub(crate) const SSRC_HQ: u32 = 0x0000_00A3;
pub(crate) const SSRC_ST0: u32 = 0x0000_00AE;

const LL_TTP_LEAD_MS: u16 = 60;
const LL_TTP_LEAD_HIGH_MS: u16 = 80;
const LL_TTP_WINDOW_MIN_MS: u16 = 50;
const LL_TTP_WINDOW_MAX_MS: u16 = 85;
const HIRES_TTP_LEAD_MS: u16 = 200;

#[cfg(target_pointer_width = "64")]
const _: () = {
    // The vendor header and the two layouts the C header asserts.
    assert!(crate::ids::CAPS_LEN == BODY_SIZE);
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Ll,
    HiRes,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Format {
    pub rate: u32,
    pub samples: u16,
    pub codec_bytes: u16,
    pub frame_bytes: u16,
    pub native_calls: u16,
    pub rate_bits: u8,
    pub bits_per_sample: u8,
}

struct Desc {
    name: &'static CStr,
    description: &'static CStr,
    ssrc: u32,
    ssrc_alt: u32,
    ttp_lead_ms: u16,
    ttp_lead_high_ms: u16,
    ttp_window_min_ms: u16,
    ttp_window_max_ms: u16,
    source_type: u8,
    formats: &'static [Format],
}

const LL_FORMATS: [Format; 1] = [Format {
    rate: 48_000,
    samples: 480,
    codec_bytes: 348,
    frame_bytes: 360,
    native_calls: 60,
    rate_bits: SR_48000,
    bits_per_sample: BITS_24,
}];

const HIRES_FORMATS: [Format; 2] = [
    Format {
        rate: 96_000,
        samples: 768,
        codec_bytes: 656,
        frame_bytes: 668,
        native_calls: 96,
        rate_bits: SR_96000,
        bits_per_sample: BITS_24,
    },
    Format {
        rate: 48_000,
        samples: 1200,
        codec_bytes: 656,
        frame_bytes: 668,
        native_calls: 150,
        rate_bits: SR_48000,
        bits_per_sample: BITS_24,
    },
];

const LL: Desc = Desc {
    name: crate::ids::ADAPTIVE_LL_NAME,
    description: crate::ids::ADAPTIVE_LL_DESCRIPTION,
    ssrc: SSRC_LL0,
    ssrc_alt: SSRC_LL1,
    ttp_lead_ms: LL_TTP_LEAD_MS,
    ttp_lead_high_ms: LL_TTP_LEAD_HIGH_MS,
    ttp_window_min_ms: LL_TTP_WINDOW_MIN_MS,
    ttp_window_max_ms: LL_TTP_WINDOW_MAX_MS,
    source_type: 0,
    formats: &LL_FORMATS,
};

const HIRES: Desc = Desc {
    name: crate::ids::ADAPTIVE_HIRES_NAME,
    description: crate::ids::ADAPTIVE_HIRES_DESCRIPTION,
    ssrc: SSRC_HQ,
    ssrc_alt: 0,
    ttp_lead_ms: HIRES_TTP_LEAD_MS,
    ttp_lead_high_ms: HIRES_TTP_LEAD_MS,
    ttp_window_min_ms: HIRES_TTP_LEAD_MS,
    ttp_window_max_ms: HIRES_TTP_LEAD_MS,
    source_type: 0,
    formats: &HIRES_FORMATS,
};

// The templates only fix the rate byte; `Mode::caps` ORs the source type in.
const LL_CAPS: [u8; BODY_SIZE] = [
    0xd7, 0x00, 0x00, 0x00, 0xad, 0x00, 0x10, 0x0a, 0x3c, 0x50, 0x64, 0x64, 0xff, 0xff, 0x00, 0x01,
    0x92, 0x00, 0x00, 0x0f, 0x02, 0x03, 0x03, 0x03, 0x00, 0xaa, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

const HIRES_CAPS: [u8; BODY_SIZE] = [
    0xd7, 0x00, 0x00, 0x00, 0xad, 0x00, 0x60, 0x0a, 0x3c, 0x50, 0x64, 0x64, 0xff, 0xff, 0x00, 0x01,
    0x92, 0x00, 0x00, 0x0f, 0x02, 0x03, 0x03, 0x03, 0x00, 0xaa, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

impl Mode {
    pub(crate) const DEFAULT: Self = Self::Ll;

    const fn desc(self) -> &'static Desc {
        match self {
            Self::Ll => &LL,
            Self::HiRes => &HIRES,
        }
    }

    pub(crate) const fn name(self) -> &'static CStr {
        self.desc().name
    }

    pub(crate) const fn description(self) -> &'static CStr {
        self.desc().description
    }

    pub(crate) const fn formats(self) -> &'static [Format] {
        self.desc().formats
    }

    /// The mode named by a codec entry, matching the C selector spellings.
    #[must_use]
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "aptx_adaptive_ll" | "ll" | "low-latency" | "low_latency" => Some(Self::Ll),
            "aptx_adaptive_hires"
            | "hires"
            | "hi-res"
            | "hi_res"
            | "high-res"
            | "high_res"
            | "hq" => Some(Self::HiRes),
            _ => None,
        }
    }

    /// Resolve the entry PipeWire handed to a callback; unknown and NULL names
    /// fall back to the default (LL) mode, as in the C plugin.
    #[must_use]
    pub(crate) fn from_codec(codec: *const media_codec) -> Self {
        // SAFETY: PipeWire passes one of our static entries or NULL.
        let Some(codec) = (unsafe { codec.as_ref() }) else {
            return Self::DEFAULT;
        };
        if codec.name.is_null() {
            return Self::DEFAULT;
        }
        // SAFETY: media_codec names are static NUL-terminated strings.
        let name = unsafe { CStr::from_ptr(codec.name) };
        name.to_str()
            .ok()
            .and_then(Self::from_name)
            .unwrap_or(Self::DEFAULT)
    }

    #[must_use]
    pub(crate) fn format(self, rate: u32) -> Option<&'static Format> {
        self.formats().iter().find(|f| f.rate == rate)
    }

    /// First of the mode's formats the sink advertises, in preference order.
    #[must_use]
    pub(crate) fn pick_rate(self, sink_bits: u8) -> u32 {
        self.formats()
            .iter()
            .find(|f| sink_bits & f.rate_bits != 0)
            .map_or(0, |f| f.rate)
    }

    #[must_use]
    pub(crate) fn frame_samples(self, rate: u32) -> u32 {
        self.format(rate).map_or(0, |f| u32::from(f.samples))
    }

    /// Bytes of interleaved stereo S32 PCM in one frame.
    #[must_use]
    pub(crate) fn block_bytes(self, rate: u32) -> Option<usize> {
        self.format(rate).map(|f| usize::from(f.samples) * 2 * 4)
    }

    /// The capability element for one mode, with the source type ORed in.
    #[must_use]
    pub(crate) fn caps(self, source_type: u8) -> [u8; BODY_SIZE] {
        let mut caps = match self {
            Self::Ll => LL_CAPS,
            Self::HiRes => HIRES_CAPS,
        };
        caps[OFF_RATE_SOURCE] |= source_type & ST_MASK;
        caps
    }

    /// The owner's capability element: every rate both profiles can send.
    ///
    /// The LL entry owns the shared endpoint, so its caps advertise 48 kHz
    /// (LL) plus 96 kHz (Hi-Res); the sink then offers both in
    /// SelectConfiguration.
    #[must_use]
    pub(crate) fn advertised_caps(self, source_type: u8) -> [u8; BODY_SIZE] {
        let mut caps = self.caps(source_type);
        let mut bits = 0u8;
        for format in self.formats() {
            bits |= format.rate_bits;
        }
        if self == Self::Ll {
            for format in Self::HiRes.formats() {
                bits |= format.rate_bits;
            }
        }
        caps[OFF_RATE_SOURCE] = bits | (source_type & ST_MASK);
        caps
    }

    /// The 40-byte SetConfiguration body for one mode at one rate.
    #[must_use]
    pub(crate) fn config(
        self,
        rate: u32,
        source_type: u8,
        channel_mode: u8,
        sink_features: u32,
    ) -> Option<[u8; BODY_SIZE]> {
        let format = self.format(rate)?;
        let mut out = [0u8; BODY_SIZE];
        out[..VENDOR_HEADER.len()].copy_from_slice(&VENDOR_HEADER);
        out[OFF_RATE_SOURCE] = format.rate_bits | (source_type & ST_MASK);
        out[OFF_CHANNEL_MODE] = channel_mode & CH_MASK;
        out[OFF_TTP_LL0..OFF_TTP_LL0 + LATENCY_BYTES.len()].copy_from_slice(&LATENCY_BYTES);
        out[OFF_EXT_VERSION] = EXT_VERSION_R2;
        let features = if sink_features == FEATURES_UNKNOWN {
            FEATURES_SOURCE
        } else {
            FEATURES_SOURCE & sink_features
        };
        out[OFF_FEATURES..OFF_FEATURES + 4].copy_from_slice(&features.to_le_bytes());
        out[OFF_SETUP_PREF..OFF_SETUP_PREF + SETUP_PREF.len()].copy_from_slice(&SETUP_PREF);
        out[OFF_END_MARKER..OFF_END_MARKER + END_MARKER.len()].copy_from_slice(&END_MARKER);
        out[OFF_BITS_PER_SAMPLE] = format.bits_per_sample;
        Some(out)
    }

    /// Joint stereo when the sink offers it, then stereo, then mono.
    #[must_use]
    pub(crate) const fn pick_channel_mode(sink_channel_modes: u8) -> u8 {
        if sink_channel_modes & CH_JOINT != 0 {
            CH_JOINT
        } else if sink_channel_modes & CH_STEREO != 0 {
            CH_STEREO
        } else if sink_channel_modes & CH_MONO != 0 {
            CH_MONO
        } else {
            CH_STEREO
        }
    }

    /// The RTP SSRC: 0xAE for source type 0, else the latency-mode marker.
    #[must_use]
    pub(crate) const fn ssrc(self, source_type: u8, ll1: bool) -> u32 {
        if source_type & ST_HOST == 0 {
            return SSRC_ST0;
        }
        let desc = self.desc();
        if ll1 && desc.ssrc_alt != 0 {
            desc.ssrc_alt
        } else {
            desc.ssrc
        }
    }

    #[must_use]
    pub(crate) const fn ttp_lead_ms(self, ll1: bool) -> u16 {
        if ll1 {
            self.desc().ttp_lead_high_ms
        } else {
            self.desc().ttp_lead_ms
        }
    }

    #[must_use]
    pub(crate) const fn ttp_window_ms(self) -> (u16, u16) {
        (self.desc().ttp_window_min_ms, self.desc().ttp_window_max_ms)
    }

    /// The mode's default source type (controller/link-clock timing).
    #[must_use]
    pub(crate) const fn default_source_type(self) -> u8 {
        self.desc().source_type
    }

    #[must_use]
    pub(crate) fn frame_bytes(self, rate: u32) -> u16 {
        self.format(rate).map_or(0, |f| f.frame_bytes)
    }

    /// The encoder payload the native library is expected to return.
    #[must_use]
    pub(crate) fn codec_bytes(self, rate: u32) -> u16 {
        self.format(rate).map_or(0, |f| f.codec_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_templates_and_owner_union() {
        let ll = Mode::Ll.caps(0);
        assert_eq!(ll[OFF_RATE_SOURCE], SR_48000);
        assert_eq!(ll[OFF_CHANNEL_MODE], 0x0a);
        assert_eq!(ll[OFF_TTP_LL0..OFF_TTP_LL0 + 6], LATENCY_BYTES);
        assert_eq!(ll[OFF_EXT_VERSION], 1);
        assert_eq!(ll[OFF_BITS_PER_SAMPLE], 0x02);

        assert_eq!(Mode::HiRes.caps(0)[OFF_RATE_SOURCE], SR_96000 | SR_192000);
        assert_eq!(Mode::Ll.caps(ST_HOST)[OFF_RATE_SOURCE], 0x12);
        assert_eq!(Mode::Ll.advertised_caps(0)[OFF_RATE_SOURCE], 0x30);
        assert_eq!(Mode::Ll.advertised_caps(ST_HOST)[OFF_RATE_SOURCE], 0x32);
        assert_eq!(Mode::HiRes.advertised_caps(0)[OFF_RATE_SOURCE], 0x30);
    }

    #[test]
    fn ll_config_body_matches_the_c_layout() {
        let config = Mode::Ll
            .config(48_000, 0, CH_JOINT, FEATURES_UNKNOWN)
            .unwrap();
        assert_eq!(config.len(), BODY_SIZE);
        assert_eq!(&config[..6], &VENDOR_HEADER);
        assert_eq!(config[OFF_RATE_SOURCE], 0x10);
        assert_eq!(config[OFF_CHANNEL_MODE], 0x08);
        assert_eq!(config[8..14], LATENCY_BYTES);
        assert_eq!(config[OFF_EXT_VERSION], 1);
        assert_eq!(
            config[OFF_FEATURES..OFF_FEATURES + 4],
            [0x92, 0x00, 0x00, 0x0f]
        );
        assert_eq!(config[OFF_SETUP_PREF..OFF_SETUP_PREF + 4], SETUP_PREF);
        assert_eq!(config[OFF_END_MARKER..OFF_END_MARKER + 2], END_MARKER);
        assert_eq!(config[OFF_BITS_PER_SAMPLE], 0x02);
    }

    #[test]
    fn features_are_intersected_with_the_sink_word() {
        let config = Mode::HiRes
            .config(96_000, 0, CH_STEREO, 0x0F00_0003)
            .unwrap();
        assert_eq!(
            u32::from_le_bytes(config[OFF_FEATURES..OFF_FEATURES + 4].try_into().unwrap()),
            0x0F00_0002
        );
        assert_eq!(Mode::HiRes.config(44_100, 0, CH_STEREO, 0), None);
    }

    #[test]
    fn hires_picks_96k_then_the_48k_fallback() {
        assert_eq!(Mode::HiRes.pick_rate(SR_96000), 96_000);
        assert_eq!(Mode::HiRes.pick_rate(SR_48000), 48_000);
        assert_eq!(Mode::HiRes.pick_rate(SR_96000 | SR_48000), 96_000);
        assert_eq!(Mode::HiRes.pick_rate(SR_192000), 0);
        assert_eq!(Mode::Ll.pick_rate(SR_96000), 0);
        assert_eq!(Mode::Ll.pick_rate(SR_48000), 48_000);
    }

    #[test]
    fn channel_mode_preference_and_ssrc() {
        assert_eq!(Mode::pick_channel_mode(0x0a), CH_JOINT);
        assert_eq!(Mode::pick_channel_mode(0x02), CH_STEREO);
        assert_eq!(Mode::pick_channel_mode(0x01), CH_MONO);
        assert_eq!(Mode::pick_channel_mode(0), CH_STEREO);

        assert_eq!(Mode::Ll.ssrc(0, false), SSRC_ST0);
        assert_eq!(Mode::HiRes.ssrc(0, false), SSRC_ST0);
        assert_eq!(Mode::Ll.ssrc(2, false), SSRC_LL0);
        assert_eq!(Mode::Ll.ssrc(2, true), SSRC_LL1);
        assert_eq!(Mode::HiRes.ssrc(2, false), SSRC_HQ);
    }

    #[test]
    fn frame_geometry_and_leads() {
        assert_eq!(Mode::Ll.block_bytes(48_000), Some(480 * 8));
        assert_eq!(Mode::HiRes.block_bytes(96_000), Some(768 * 8));
        assert_eq!(Mode::HiRes.block_bytes(48_000), Some(1200 * 8));
        assert_eq!(Mode::HiRes.frame_bytes(96_000), 668);
        assert_eq!(Mode::Ll.ttp_lead_ms(false), 60);
        assert_eq!(Mode::Ll.ttp_lead_ms(true), 80);
        assert_eq!(Mode::HiRes.ttp_lead_ms(false), 200);
        assert_eq!(Mode::Ll.ttp_window_ms(), (50, 85));
        assert_eq!(Mode::HiRes.ttp_window_ms(), (200, 200));
    }

    #[test]
    fn name_selectors_match_the_c_plugin() {
        assert_eq!(Mode::from_name("aptx_adaptive_ll"), Some(Mode::Ll));
        assert_eq!(Mode::from_name("ll"), Some(Mode::Ll));
        assert_eq!(Mode::from_name("low-latency"), Some(Mode::Ll));
        assert_eq!(Mode::from_name("aptx_adaptive_hires"), Some(Mode::HiRes));
        assert_eq!(Mode::from_name("hires"), Some(Mode::HiRes));
        assert_eq!(Mode::from_name("hq"), Some(Mode::HiRes));
        assert_eq!(Mode::from_name("aptx_adaptive"), None);
        assert_eq!(Mode::from_codec(std::ptr::null()), Mode::Ll);
    }
}

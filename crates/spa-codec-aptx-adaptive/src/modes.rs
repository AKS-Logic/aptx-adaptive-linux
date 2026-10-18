//! aptX Adaptive operating modes: LL (the default) and Hi-Res.
//!
//! The plugin presents two selectable PipeWire/BlueZ profiles on the one
//! aptX Adaptive A2DP endpoint:
//!
//! | | LL (default) | Hi-Res |
//! |---|---|---|
//! | Card profile | a2dp-sink-aptx_adaptive_ll | a2dp-sink-aptx_adaptive_hires |
//! | Codec entry | aptx_adaptive_ll | aptx_adaptive_hires |
//! | Rate | 48 kHz | 96 kHz |
//! | Packet period | 10 ms | 8 ms |
//! | Samples per frame | 480 | 768 |
//! | Native 8-sample blocks | 60 | 96 |
//! | Codec payload | 348 B | 656 B |
//! | RTP packet (payload + 12 B header) | 360 B | 668 B |
//! | TTP target | 50-85 ms (lead 60-80) | 200 ms |
//! | SSRC (source type 2) | 0xA1 (LL0) / 0xA2 (LL1) | 0xA3 |
//!
//! The brief's "360-byte LL frame" is the 360-byte on-air RTP packet
//! (12-byte header plus 348-byte codec payload), not a 360-byte codec
//! payload. Mode::payload_bytes and Mode::rtp_packet_bytes keep the two
//! apart. Likewise Hi-Res is 656 bytes of codec payload and 668 bytes on
//! air.
//!
//! Hi-Res uses the 96 kHz HQ row that carries 656 bytes: 8 ms and 768
//! samples per channel (spec a2dp-integration.md section 4, 96 kHz index 5,
//! level 5; crates/aptx-adaptive/src/tables.rs row 5). A 10 ms / 960-sample
//! variant at 656 bytes would be about 525 kbps, which is not a reference
//! row, so Mode::HiRes uses the 8 ms geometry. The 48 kHz HQ rows are
//! 12-25 ms / 576-1200 samples and do not carry 656 bytes at 96 kHz.
//!
//! The Bose QC Ultra CIE advertises byte 6 = 0x51 (192 kHz plus 48 kHz, not
//! 96 kHz). A 96 kHz Hi-Res stream therefore needs a sink that advertises
//! 0x20 (the Samsung tablet is the intended target), or a 192 kHz variant;
//! LL at 48 kHz is unaffected.
//!
//! SSRC values 0xA1 through 0xA3 are source type 2 markers (spec sections
//! 7F/7H). The current source type 0 / ST0 transport uses 0x0000_00ae for
//! every mode; Mode::ssrc_for selects the right value for either source
//! type.
//!
//! This module is pure data: no I/O, no globals and no allocation. The Lead
//! wires it into lib.rs, caps.rs and session.rs; the byte-level helpers
//! mirror the layouts in caps.rs and ids.rs so the module can be reviewed on
//! its own.
#![allow(
    dead_code,
    reason = "mode table is consumed piecemeal during integration"
)]
#![allow(
    clippy::doc_markdown,
    reason = "protocol identifiers are used as prose in the mode table"
)]

use std::ffi::{CStr, OsString};
use std::fmt;

use aptx_adaptive::{Config, DecoderLevel, Profile};

/// A2DP vendor element length, including the 6-byte vendor header.
pub const CAPS_LEN: usize = 40;

/// A plain RTP fixed header: no CSRCs and no extension.
pub const RTP_HEADER_LEN: usize = 12;

/// aptX Adaptive vendor id, little-endian in the A2DP element.
pub const VENDOR_ID: u32 = 0x0000_00D7;

/// aptX Adaptive codec id, little-endian in the A2DP element.
pub const CODEC_ID: u16 = 0x00AD;

/// The six-byte vendor header at the start of every capability/config element.
pub const VENDOR_HEADER: [u8; 6] = [0xD7, 0x00, 0x00, 0x00, 0xAD, 0x00];

/// The endpoint name shared by both profiles. PipeWire appends the codec entry
/// name when it builds the card profile.
pub const ENDPOINT_NAME: &CStr = c"aptx_adaptive";

/// The device-setting / environment variable that selects the starting mode.
pub const MODE_VAR: &str = "APTX_ADAPTIVE_DEFAULT_MODE";

/// Stereo only: aptX Adaptive has no mono encoder in this plugin.
pub const CHANNELS: u8 = 2;

/// Declared input resolution. The PipeWire sample format is S24_32_LE.
pub const BITS_PER_SAMPLE: u16 = 24;

/// Body byte 26 encoding of [`BITS_PER_SAMPLE`]: 0x02 = 24-bit PCM.
pub const BITS_PER_SAMPLE_BYTE: u8 = 0x02;

/// Bytes per input sample in the S24_32_LE PipeWire format.
pub const PCM_BYTES_PER_SAMPLE: usize = 4;

/// The vendor/native encoder works in 8-sample calls; this is the block size
/// used by the aptx_native path, not the PipeWire input block (which is the
/// GCD of the reachable packet periods).
pub const NATIVE_BLOCK_SAMPLES: u32 = 8;

/// Stereo plus joint stereo (spec section 1.2).
pub const CHANNEL_MODES: u8 = 0x0A;

/// 44.1 kHz rate bit in config byte 6.
pub const RATE_44K: u8 = 0x08;
/// 48 kHz rate bit in config byte 6.
pub const RATE_48K: u8 = 0x10;
/// 96 kHz rate bit in config byte 6.
pub const RATE_96K: u8 = 0x20;
/// 192 kHz rate bit in config byte 6. The Bose CIE advertises this instead of
/// 96 kHz.
pub const RATE_192K: u8 = 0x40;
/// Rate field mask in config byte 6.
pub const RATE_MASK: u8 = 0xF8;
/// Source-type field mask in config byte 6.
pub const SOURCE_TYPE_MASK: u8 = 0x07;

/// 0x000000A1: LL0, the sink's "2.4 GHz Wi-Fi off" latency mode.
pub const SSRC_LL0: u32 = 0x0000_00A1;
/// 0x000000A2: LL1, the sink's "2.4 GHz Wi-Fi on" latency mode.
pub const SSRC_LL1: u32 = 0x0000_00A2;
/// 0x000000A3: HQ.
pub const SSRC_HQ: u32 = 0x0000_00A3;
/// 0x000000A4: TWS. Not used by this source, kept for the full mapping.
pub const SSRC_TWS: u32 = 0x0000_00A4;
/// 0x000000AE: the phone/source-type-0 marker used by the current ST0
/// transport for every mode.
pub const SSRC_ST0: u32 = 0x0000_00AE;

/// Lower end of the LL time-to-play window, in milliseconds.
pub const TTP_LL_MIN_MS: u16 = 50;
/// Upper end of the LL time-to-play window, in milliseconds.
pub const TTP_LL_MAX_MS: u16 = 85;
/// Nominal LL source lead, in milliseconds.
pub const TTP_LL_LEAD_MS: u16 = 60;
/// Upper LL source lead, in milliseconds.
pub const TTP_LL_LEAD_HIGH_MS: u16 = 80;
/// HQ (Hi-Res) target latency, in milliseconds.
pub const TTP_HQ_MS: u16 = 200;

/// The six TTP bytes in the A2DP element (body bytes 8..14): LL0 and LL1
/// target latencies in milliseconds, then the HQ and TWS targets in 2 ms
/// units. The element carries every latency target at once, so both modes
/// send the same body; only the selected mode's lead and the rate byte
/// differ. The 50-85 ms range is the window the LL source accepts for its
/// active lead; the body carries the 60/80 ms leads (the C mode contract's
/// 0x3c/0x50), not the window bounds.
pub const TTP_BODY: [u8; 6] = [
    TTP_LL_LEAD_MS as u8,
    TTP_LL_LEAD_HIGH_MS as u8,
    (TTP_HQ_MS / 2) as u8,
    (TTP_HQ_MS / 2) as u8,
    0xFF,
    0xFF,
];

/// Units per millisecond in the 16-bit container TTP field (1/15000 s).
pub const TTP_UNITS_PER_MS: u16 = 15;

/// Which clock the A2DP configuration says the source will use (byte 6 bits
/// 2..0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SourceType {
    /// Source type 0: controller/link-clock time base. This is the current ST0
    /// transport, whose RTP timestamp is a microsecond time-to-play clock.
    #[default]
    Controller,
    /// Source type 2: host sample-count time base. The sink times playback
    /// itself and the RTP timestamp's low 16 bits are samples per channel.
    Host,
}

impl SourceType {
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Controller => 0,
            Self::Host => 2,
        }
    }

    /// Reads the source-type field of a config byte. The sink-only 0x01 and
    /// the custom-AVRCP 0x04 bits are not source types, so combinations
    /// outside 0 and 2 return None.
    #[must_use]
    pub const fn from_config_byte(byte: u8) -> Option<Self> {
        match byte & SOURCE_TYPE_MASK {
            0 => Some(Self::Controller),
            2 => Some(Self::Host),
            _ => None,
        }
    }
}

/// One selectable aptX Adaptive profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    /// 48 kHz, 10 ms, 480 samples, 348-byte payload, 50-85 ms TTP. The default.
    #[default]
    Ll,
    /// 96 kHz, 8 ms, 768 samples, 656-byte payload, 200 ms TTP.
    HiRes,
}

impl Mode {
    /// Every mode in card-profile order; LL comes first because it is the
    /// default.
    pub const ALL: [Self; 2] = [Self::Ll, Self::HiRes];

    /// The mode a fresh install starts in.
    pub const DEFAULT: Self = Self::Ll;

    /// Short token used by APTX_ADAPTIVE_DEFAULT_MODE, the mode tool and logs.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ll => "ll",
            Self::HiRes => "hires",
        }
    }

    /// Codec entry name, i.e. the media_codec.name field.
    #[must_use]
    pub const fn codec_name(self) -> &'static CStr {
        match self {
            Self::Ll => c"aptx_adaptive_ll",
            Self::HiRes => c"aptx_adaptive_hires",
        }
    }

    /// Mode::codec_name as UTF-8, for dictionary and profile lookups.
    #[must_use]
    pub const fn codec_name_str(self) -> &'static str {
        match self {
            Self::Ll => "aptx_adaptive_ll",
            Self::HiRes => "aptx_adaptive_hires",
        }
    }

    /// Human-readable codec description shown in card profiles.
    #[must_use]
    pub const fn description(self) -> &'static CStr {
        match self {
            Self::Ll => c"aptX Adaptive (Low Latency)",
            Self::HiRes => c"aptX Adaptive Hi-Res",
        }
    }

    /// The PipeWire/BlueZ card profile name, including the a2dp-sink- prefix.
    #[must_use]
    pub const fn card_profile(self) -> &'static str {
        match self {
            Self::Ll => "a2dp-sink-aptx_adaptive_ll",
            Self::HiRes => "a2dp-sink-aptx_adaptive_hires",
        }
    }

    /// Mapping to the codec library's profile enum. Hi-Res is the HQ encoder
    /// at 96 kHz; LL is the LL encoder profile at 48 kHz.
    #[must_use]
    pub const fn profile(self) -> Profile {
        match self {
            Self::Ll => Profile::Ll,
            Self::HiRes => Profile::Hq,
        }
    }

    /// Inverse of Mode::profile. In the two-profile layout the HQ profile is
    /// Hi-Res (96 kHz / 656 bytes). The codec's 48 kHz HQ rows are a rate, not
    /// a third profile, and are not represented by this enum.
    #[must_use]
    pub const fn from_profile(profile: Profile) -> Self {
        match profile {
            Profile::Hq => Self::HiRes,
            Profile::Ll => Self::Ll,
        }
    }

    /// Negotiated sample rate in hertz.
    #[must_use]
    pub const fn rate(self) -> u32 {
        match self {
            Self::Ll => 48_000,
            Self::HiRes => 96_000,
        }
    }

    /// Packet period in milliseconds.
    #[must_use]
    pub const fn period_ms(self) -> u32 {
        match self {
            Self::Ll => 10,
            Self::HiRes => 8,
        }
    }

    /// Samples per channel in one encoded frame.
    #[must_use]
    pub const fn samples_per_frame(self) -> u32 {
        match self {
            Self::Ll => 480,
            Self::HiRes => 768,
        }
    }

    /// Native 8-sample encoder calls per frame. Informational for the
    /// aptx_native/vendor-helper path; the aptx-linux Stream consumes whole
    /// PCM frames.
    #[must_use]
    pub const fn native_blocks_per_frame(self) -> u32 {
        self.samples_per_frame() / NATIVE_BLOCK_SAMPLES
    }

    /// Codec payload bytes in one frame, without the RTP header. LL is 348 B
    /// (the brief's "360-byte frame" includes the 12-byte RTP header); Hi-Res
    /// is 656 B.
    #[must_use]
    pub const fn payload_bytes(self) -> usize {
        match self {
            Self::Ll => 348,
            Self::HiRes => 656,
        }
    }

    /// A whole on-air A2DP packet: the codec payload plus the 12-byte RTP
    /// header. 360 B for LL and 668 B for Hi-Res.
    #[must_use]
    pub const fn rtp_packet_bytes(self) -> usize {
        self.payload_bytes() + RTP_HEADER_LEN
    }

    /// Interleaved S24_32 stereo bytes for one whole encoder frame. The
    /// PipeWire input block may be a divisor of this; media-sink.c reads a
    /// fixed block size covering every reachable packet period.
    #[must_use]
    pub const fn pcm_frame_bytes(self) -> usize {
        self.samples_per_frame() as usize * CHANNELS as usize * PCM_BYTES_PER_SAMPLE
    }

    /// RTP/TTP step for one packet, in microseconds.
    #[must_use]
    pub const fn rtp_step_us(self) -> u32 {
        self.period_ms() * 1_000
    }

    /// The same step as nanoseconds, as the SPA get_interval callback wants.
    #[must_use]
    pub const fn interval_ns(self) -> u64 {
        self.period_ms() as u64 * 1_000_000
    }

    /// Nominal source lead in milliseconds: 60 ms for LL, 200 ms for Hi-Res.
    #[must_use]
    pub const fn ttp_lead_ms(self) -> u16 {
        match self {
            Self::Ll => TTP_LL_LEAD_MS,
            Self::HiRes => TTP_HQ_MS,
        }
    }

    /// Upper end of the source lead: 80 ms for LL, 200 ms for Hi-Res.
    #[must_use]
    pub const fn ttp_lead_high_ms(self) -> u16 {
        match self {
            Self::Ll => TTP_LL_LEAD_HIGH_MS,
            Self::HiRes => TTP_HQ_MS,
        }
    }

    /// The accepted time-to-play window as (low, high) milliseconds. LL is
    /// 50-85 ms; Hi-Res requests the 200 ms HQ target.
    #[must_use]
    pub const fn ttp_window_ms(self) -> (u16, u16) {
        match self {
            Self::Ll => (TTP_LL_MIN_MS, TTP_LL_MAX_MS),
            Self::HiRes => (TTP_HQ_MS, TTP_HQ_MS),
        }
    }

    /// The source lead in 1/15000 s units, (low, high). Useful for the 16-bit
    /// container TTP field when the C plugin or harness uses one; the
    /// aptx-linux RTP transport carries no container.
    #[must_use]
    pub const fn ttp_units(self) -> (u16, u16) {
        (
            self.ttp_lead_ms() * TTP_UNITS_PER_MS,
            self.ttp_lead_high_ms() * TTP_UNITS_PER_MS,
        )
    }

    /// Primary SSRC mode marker for source type 2: 0xA1 for LL (LL0) and 0xA3
    /// for Hi-Res (HQ).
    #[must_use]
    pub const fn ssrc(self) -> u32 {
        match self {
            Self::Ll => SSRC_LL0,
            Self::HiRes => SSRC_HQ,
        }
    }

    /// The alternate LL SSRC, 0xA2 (LL1), or None for Hi-Res. LL1 is the
    /// sink's "2.4 GHz Wi-Fi on" latency mode and is selected per stream, not
    /// per profile.
    #[must_use]
    pub const fn ssrc_alternate(self) -> Option<u32> {
        match self {
            Self::Ll => Some(SSRC_LL1),
            Self::HiRes => None,
        }
    }

    /// SSRC for the negotiated source type. Source type 0 (the current ST0
    /// transport) uses 0x0000_00ae for every mode; source type 2 uses the
    /// per-mode marker.
    #[must_use]
    pub const fn ssrc_for(self, source_type: SourceType) -> u32 {
        match source_type {
            SourceType::Controller => SSRC_ST0,
            SourceType::Host => self.ssrc(),
        }
    }

    /// The mode's rate bit in config byte 6.
    #[must_use]
    pub const fn rate_bit(self) -> u8 {
        match self {
            Self::Ll => RATE_48K,
            Self::HiRes => RATE_96K,
        }
    }

    /// Config byte 6: the rate bits plus the source-type field.
    #[must_use]
    pub const fn rate_byte(self, source_type: SourceType) -> u8 {
        self.rate_bit() | source_type.as_byte()
    }

    /// A complete 40-byte capability element advertising only this mode's
    /// native rate. Use caps_element to advertise several rates at once (for
    /// example 48 kHz and 96 kHz on the shared endpoint).
    #[must_use]
    pub const fn fill_caps(self, source_type: SourceType) -> [u8; CAPS_LEN] {
        caps_element(self.rate_bit(), source_type)
    }

    /// A complete 40-byte R2 SetConfiguration element with this mode's native
    /// rate, the negotiated features word and the fixed setup preferences.
    /// The source sends its own block for R2 sinks (spec section 7J); use
    /// r2_config_element for a multi-rate block.
    #[must_use]
    pub const fn r2_config(self, source_type: SourceType, features: u32) -> [u8; CAPS_LEN] {
        r2_config_element(self.rate_bit(), source_type, features)
    }

    /// The codec-library stream contract for this mode. Hi-Res at 96 kHz is
    /// intended for an R2 sink (the reference selects table index 5); LL at
    /// 48 kHz works with either decoder level.
    #[must_use]
    pub const fn config(self, level: DecoderLevel) -> Config {
        Config {
            profile: self.profile(),
            sample_rate: self.rate(),
            channels: CHANNELS,
            level,
        }
    }

    /// Parses the short mode token used by APTX_ADAPTIVE_DEFAULT_MODE and the
    /// mode tool. Case-insensitive; hq and high-quality are accepted as legacy
    /// names for Hi-Res.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim();
        if ["ll", "low-latency", "low_latency", "lowlatency"]
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
        {
            Some(Self::Ll)
        } else if [
            "hires",
            "hi-res",
            "hi_res",
            "high-res",
            "high_res",
            "highres",
            "hq",
            "high-quality",
            "high_quality",
        ]
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
        {
            Some(Self::HiRes)
        } else {
            None
        }
    }

    /// Parses a codec entry name. The single-codec build called its HQ entry
    /// aptx_adaptive; that name maps to Hi-Res so an installed profile or
    /// setting cannot silently turn into LL.
    #[must_use]
    pub fn from_codec_name(name: &str) -> Option<Self> {
        match name {
            "aptx_adaptive_ll" => Some(Self::Ll),
            "aptx_adaptive_hires" | "aptx_adaptive" => Some(Self::HiRes),
            _ => None,
        }
    }

    /// Parses a card profile name (or a bare codec entry name). The legacy
    /// a2dp-sink-aptx_adaptive profile maps to Hi-Res, matching
    /// Mode::from_codec_name.
    #[must_use]
    pub fn from_card_profile(profile: &str) -> Option<Self> {
        let codec = profile.strip_prefix("a2dp-sink-").unwrap_or(profile);
        Self::from_codec_name(codec)
    }

    /// Reads the starting mode from a caller-supplied environment lookup.
    /// Unset means LL; hq is accepted as the legacy Hi-Res name.
    ///
    /// # Errors
    /// A message when the variable is set to an unknown value or is not valid
    /// UTF-8. The caller should log it and fall back to Mode::DEFAULT.
    pub fn from_env(var: impl Fn(&str) -> Option<OsString>) -> Result<Self, String> {
        match var(MODE_VAR) {
            None => Ok(Self::DEFAULT),
            Some(value) => match value.to_str() {
                Some(name) => Self::from_name(name).ok_or_else(|| {
                    format!(
                        "{MODE_VAR}={} is not ll or hires, using ll",
                        value.display()
                    )
                }),
                None => Err(format!("{MODE_VAR} is not valid UTF-8, using ll")),
            },
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl From<Mode> for Profile {
    fn from(mode: Mode) -> Self {
        mode.profile()
    }
}

impl From<Profile> for Mode {
    fn from(profile: Profile) -> Self {
        Self::from_profile(profile)
    }
}

/// Builds a 40-byte R1-layout capability element. rates is the rate field (one
/// or more of RATE_44K, RATE_48K, RATE_96K, RATE_192K); source_type fills the
/// low three bits of byte 6. The TTP body is the source's own, not the sink's:
/// LL0 60 ms, LL1 80 ms and HQ 200 ms.
#[must_use]
pub const fn caps_element(rates: u8, source_type: SourceType) -> [u8; CAPS_LEN] {
    let mut caps = [0u8; CAPS_LEN];
    let mut i = 0;
    while i < VENDOR_HEADER.len() {
        caps[i] = VENDOR_HEADER[i];
        i += 1;
    }
    caps[6] = (rates & RATE_MASK) | source_type.as_byte();
    caps[7] = CHANNEL_MODES;
    i = 0;
    while i < TTP_BODY.len() {
        caps[8 + i] = TTP_BODY[i];
        i += 1;
    }
    // Byte 14 is reserved; bytes 15/16 are the R1 end marker 0x00 0xAA. Bytes
    // 17..40 stay zero.
    caps[14] = 0x00;
    caps[15] = 0x00;
    caps[16] = 0xAA;
    caps
}

/// Builds a 40-byte R2 SetConfiguration element. The source sends its own
/// block for R2 sinks (spec section 7J): the same TTP body, extension version
/// 1, the negotiated supported-features word, and setup preferences
/// 02 03 03 03. features is the negotiated word; the current source policy
/// starts from 0x0F00_0000 and substitutes low byte 0x92.
#[must_use]
pub const fn r2_config_element(
    rates: u8,
    source_type: SourceType,
    features: u32,
) -> [u8; CAPS_LEN] {
    let mut config = caps_element(rates, source_type);
    config[15] = 0x01;
    let features = features.to_le_bytes();
    config[16] = features[0];
    config[17] = features[1];
    config[18] = features[2];
    config[19] = features[3];
    config[20] = 0x02;
    config[21] = 0x03;
    config[22] = 0x03;
    config[23] = 0x03;
    config[24] = 0x00;
    config[25] = 0xAA;
    // Body byte 26: this tree declares 24-bit PCM. AOSP leaves the reserved
    // byte zero, but the C mode contract and the vendor path write 0x02.
    config[26] = BITS_PER_SAMPLE_BYTE;
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn env(value: Option<&str>) -> impl Fn(&str) -> Option<OsString> {
        move |key| {
            assert_eq!(key, MODE_VAR);
            value.map(OsString::from)
        }
    }

    #[test]
    fn default_and_order() {
        assert_eq!(Mode::default(), Mode::Ll);
        assert_eq!(Mode::DEFAULT, Mode::Ll);
        assert_eq!(Mode::ALL, [Mode::Ll, Mode::HiRes]);
        assert_eq!(Mode::Ll.to_string(), "ll");
        assert_eq!(Mode::HiRes.to_string(), "hires");
    }

    #[test]
    fn names() {
        assert_eq!(Mode::Ll.name(), "ll");
        assert_eq!(Mode::HiRes.name(), "hires");
        assert_eq!(Mode::Ll.codec_name_str(), "aptx_adaptive_ll");
        assert_eq!(Mode::HiRes.codec_name_str(), "aptx_adaptive_hires");
        assert_eq!(Mode::Ll.codec_name().to_bytes(), b"aptx_adaptive_ll");
        assert_eq!(Mode::HiRes.codec_name().to_bytes(), b"aptx_adaptive_hires");
        assert_eq!(
            Mode::Ll.description().to_bytes(),
            b"aptX Adaptive (Low Latency)"
        );
        assert_eq!(
            Mode::HiRes.description().to_bytes(),
            b"aptX Adaptive Hi-Res"
        );
        assert_eq!(Mode::Ll.card_profile(), "a2dp-sink-aptx_adaptive_ll");
        assert_eq!(Mode::HiRes.card_profile(), "a2dp-sink-aptx_adaptive_hires");
        assert_eq!(ENDPOINT_NAME.to_bytes(), b"aptx_adaptive");
    }

    #[test]
    fn geometry() {
        assert_eq!((Mode::Ll.rate(), Mode::HiRes.rate()), (48_000, 96_000));
        assert_eq!((Mode::Ll.period_ms(), Mode::HiRes.period_ms()), (10, 8));
        assert_eq!(
            (
                Mode::Ll.samples_per_frame(),
                Mode::HiRes.samples_per_frame()
            ),
            (480, 768)
        );
        assert_eq!(
            (
                Mode::Ll.native_blocks_per_frame(),
                Mode::HiRes.native_blocks_per_frame()
            ),
            (60, 96)
        );
        assert_eq!(
            (Mode::Ll.pcm_frame_bytes(), Mode::HiRes.pcm_frame_bytes()),
            (3_840, 6_144)
        );
        assert_eq!(
            (Mode::Ll.rtp_step_us(), Mode::HiRes.rtp_step_us()),
            (10_000, 8_000)
        );
        assert_eq!(
            (Mode::Ll.interval_ns(), Mode::HiRes.interval_ns()),
            (10_000_000, 8_000_000)
        );
        assert_eq!(CHANNELS, 2);
        assert_eq!(BITS_PER_SAMPLE, 24);
    }

    #[test]
    fn payload_and_on_air_sizes_differ_by_the_rtp_header() {
        assert_eq!(
            (Mode::Ll.payload_bytes(), Mode::Ll.rtp_packet_bytes()),
            (348, 360)
        );
        assert_eq!(
            (Mode::HiRes.payload_bytes(), Mode::HiRes.rtp_packet_bytes()),
            (656, 668)
        );
        assert_eq!(Mode::Ll.payload_bytes() + RTP_HEADER_LEN, 360);
        assert_eq!(Mode::HiRes.payload_bytes() + RTP_HEADER_LEN, 668);
    }

    #[test]
    fn ttp_targets() {
        assert_eq!(Mode::Ll.ttp_lead_ms(), 60);
        assert_eq!(Mode::Ll.ttp_lead_high_ms(), 80);
        assert_eq!(Mode::Ll.ttp_window_ms(), (50, 85));
        assert_eq!(Mode::Ll.ttp_units(), (900, 1_200));
        assert_eq!(Mode::HiRes.ttp_lead_ms(), 200);
        assert_eq!(Mode::HiRes.ttp_lead_high_ms(), 200);
        assert_eq!(Mode::HiRes.ttp_window_ms(), (200, 200));
        assert_eq!(Mode::HiRes.ttp_units(), (3_000, 3_000));
        assert_eq!(TTP_BODY, [60, 80, 100, 100, 0xFF, 0xFF]);
        assert_eq!(u16::from(TTP_BODY[0]), TTP_LL_LEAD_MS);
        assert_eq!(u16::from(TTP_BODY[1]), TTP_LL_LEAD_HIGH_MS);
        assert_eq!(u16::from(TTP_BODY[2]) * 2, TTP_HQ_MS);
    }

    #[test]
    fn ssrc_markers_and_source_type() {
        assert_eq!(Mode::Ll.ssrc(), 0x0000_00A1);
        assert_eq!(Mode::Ll.ssrc_alternate(), Some(0x0000_00A2));
        assert_eq!(Mode::HiRes.ssrc(), 0x0000_00A3);
        assert_eq!(Mode::HiRes.ssrc_alternate(), None);
        for mode in Mode::ALL {
            assert_eq!(mode.ssrc_for(SourceType::Controller), 0x0000_00AE);
            assert_eq!(mode.ssrc_for(SourceType::Host), mode.ssrc());
        }
        for mode in Mode::ALL {
            assert_eq!(mode.rate_bit() & RATE_MASK, mode.rate_bit());
            assert_eq!(mode.rate_byte(SourceType::Controller), mode.rate_bit());
            assert_eq!(
                mode.rate_byte(SourceType::Host),
                mode.rate_bit() | SourceType::Host.as_byte()
            );
        }
        assert_eq!(Mode::Ll.rate_byte(SourceType::Host), 0x12);
        assert_eq!(Mode::HiRes.rate_byte(SourceType::Host), 0x22);
        assert_eq!(Mode::Ll.rate_byte(SourceType::Controller), 0x10);
        assert_eq!(Mode::HiRes.rate_byte(SourceType::Controller), 0x20);
    }

    #[test]
    fn source_type_parsing() {
        assert_eq!(SourceType::default(), SourceType::Controller);
        assert_eq!(SourceType::Controller.as_byte(), 0);
        assert_eq!(SourceType::Host.as_byte(), 2);
        assert_eq!(SourceType::from_config_byte(0x12), Some(SourceType::Host));
        assert_eq!(
            SourceType::from_config_byte(0x10),
            Some(SourceType::Controller)
        );
        assert_eq!(SourceType::from_config_byte(0x11), None);
        assert_eq!(SourceType::from_config_byte(0x14), None);
        assert_eq!(SourceType::from_config_byte(0x22), Some(SourceType::Host));
    }

    #[test]
    fn caps_are_the_r1_layout() {
        #[rustfmt::skip]
        let ll_controller: [u8; CAPS_LEN] = [
            0xd7, 0x00, 0x00, 0x00, 0xad, 0x00,
            0x10, 0x0a,
            60, 80, 100, 100, 0xff, 0xff,
            0x00, 0x00, 0xaa,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(Mode::Ll.fill_caps(SourceType::Controller), ll_controller);

        let mut ll_host = ll_controller;
        ll_host[6] = 0x12;
        assert_eq!(Mode::Ll.fill_caps(SourceType::Host), ll_host);

        let mut hires = ll_controller;
        hires[6] = 0x20;
        assert_eq!(Mode::HiRes.fill_caps(SourceType::Controller), hires);

        let mut multi = ll_controller;
        multi[6] = 0x32;
        assert_eq!(caps_element(RATE_48K | RATE_96K, SourceType::Host), multi);
        assert_eq!(VENDOR_HEADER, [0xd7, 0, 0, 0, 0xad, 0]);
    }

    #[test]
    fn r2_config_is_the_source_owned_block() {
        #[rustfmt::skip]
        let want: [u8; CAPS_LEN] = [
            0xd7, 0x00, 0x00, 0x00, 0xad, 0x00,
            0x12, 0x0a,
            60, 80, 100, 100, 0xff, 0xff,
            0x00, 0x01,
            0x92, 0x00, 0x00, 0x0f,
            0x02, 0x03, 0x03, 0x03,
            0x00, 0xaa,
            0x02, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ];
        assert_eq!(want[26], BITS_PER_SAMPLE_BYTE);
        assert_eq!(Mode::Ll.r2_config(SourceType::Host, 0x0F00_0092), want);
        assert_eq!(
            r2_config_element(RATE_48K, SourceType::Host, 0x0F00_0092),
            want
        );
    }

    #[test]
    fn config_contract() {
        assert_eq!(
            Mode::Ll.config(DecoderLevel::R1),
            Config {
                profile: Profile::Ll,
                sample_rate: 48_000,
                channels: 2,
                level: DecoderLevel::R1,
            }
        );
        assert_eq!(
            Mode::HiRes.config(DecoderLevel::R2),
            Config {
                profile: Profile::Hq,
                sample_rate: 96_000,
                channels: 2,
                level: DecoderLevel::R2,
            }
        );
        for mode in Mode::ALL {
            assert_eq!(Mode::from_profile(mode.profile()), mode);
            assert_eq!(Profile::from(mode), mode.profile());
            assert_eq!(Mode::from(mode.profile()), mode);
        }
    }

    #[test]
    fn name_mappings() {
        for mode in Mode::ALL {
            assert_eq!(Mode::from_name(mode.name()), Some(mode));
            assert_eq!(Mode::from_codec_name(mode.codec_name_str()), Some(mode));
            assert_eq!(Mode::from_card_profile(mode.card_profile()), Some(mode));
            assert_eq!(Mode::from_profile(mode.profile()), mode);
        }
        assert_eq!(Mode::from_name("LL"), Some(Mode::Ll));
        assert_eq!(Mode::from_name("Low-Latency"), Some(Mode::Ll));
        assert_eq!(Mode::from_name("hires"), Some(Mode::HiRes));
        assert_eq!(Mode::from_name("HI-RES"), Some(Mode::HiRes));
        assert_eq!(Mode::from_name("hq"), Some(Mode::HiRes));
        assert_eq!(Mode::from_name(" high_quality "), Some(Mode::HiRes));
        assert_eq!(Mode::from_name(""), None);
        assert_eq!(Mode::from_codec_name("aptx_adaptive"), Some(Mode::HiRes));
        assert_eq!(
            Mode::from_card_profile("a2dp-sink-aptx_adaptive"),
            Some(Mode::HiRes)
        );
        assert_eq!(Mode::from_codec_name("aptx_adaptive_ll"), Some(Mode::Ll));
        assert_eq!(
            Mode::from_codec_name("aptx_adaptive_hires"),
            Some(Mode::HiRes)
        );
        assert_eq!(Mode::from_codec_name("aptx"), None);
        assert_eq!(Mode::from_card_profile("a2dp-sink"), None);
    }

    #[test]
    fn env_defaults_to_ll_and_accepts_legacy_hq() {
        assert_eq!(Mode::from_env(env(None)), Ok(Mode::Ll));
        assert_eq!(Mode::from_env(env(Some("ll"))), Ok(Mode::Ll));
        assert_eq!(Mode::from_env(env(Some("hires"))), Ok(Mode::HiRes));
        assert_eq!(Mode::from_env(env(Some("hi-res"))), Ok(Mode::HiRes));
        assert_eq!(Mode::from_env(env(Some("hq"))), Ok(Mode::HiRes));
        for bad in ["", "low", "1", "highest"] {
            let err = Mode::from_env(env(Some(bad))).unwrap_err();
            assert!(err.contains("using ll"), "{err}");
        }
    }

    #[test]
    fn source_type_round_trips_through_config_byte() {
        for mode in Mode::ALL {
            for source_type in [SourceType::Controller, SourceType::Host] {
                assert_eq!(
                    SourceType::from_config_byte(mode.rate_byte(source_type)),
                    Some(source_type)
                );
            }
        }
    }
}

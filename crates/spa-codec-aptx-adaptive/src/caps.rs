//! A2DP capability negotiation.
//!
//! PLACEHOLDER: the real aptX Adaptive capability layout comes from the spec. Until then the
//! caps are the 6-byte vendor header followed by an opaque body, and the only supported stream
//! is 48 kHz stereo.

use crate::ffi::{SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR, SPA_AUDIO_FORMAT_S24_32_LE};
use crate::ids::VENDOR_HEADER;
use crate::pod::RawFormat;

pub const STREAM: RawFormat = RawFormat {
    format: SPA_AUDIO_FORMAT_S24_32_LE,
    rate: 48_000,
    positions: &[SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR],
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapsError {
    /// Too short or malformed (`-EINVAL`).
    Invalid,
    /// Well formed but not something we can encode (`-ENOTSUP`).
    Unsupported,
}

/// The PCM stream PipeWire proposes in `init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmStream {
    pub raw_audio: bool,
    pub format: u32,
    pub rate: u32,
    pub channels: u32,
}

fn check_header(caps: &[u8]) -> Result<(), CapsError> {
    match caps.get(..VENDOR_HEADER.len()) {
        None => Err(CapsError::Invalid),
        Some(h) if h != VENDOR_HEADER => Err(CapsError::Unsupported),
        Some(_) => Ok(()),
    }
}

#[must_use]
pub fn fill_caps() -> Vec<u8> {
    VENDOR_HEADER.to_vec()
}

/// Picks a configuration from the remote endpoint's capabilities.
///
/// PipeWire only offers the codec when the returned config is the same length as the caps
/// (`media_codec_check_caps`), so the opaque body is passed through unchanged for now.
///
/// # Errors
/// `Invalid` if the caps are too short, `Unsupported` for a different vendor codec.
pub fn select_config(caps: &[u8]) -> Result<Vec<u8>, CapsError> {
    check_header(caps)?;
    Ok(caps.to_vec())
}

/// Maps a negotiated configuration to the PCM stream PipeWire should feed us.
///
/// # Errors
/// As for [`select_config`].
pub fn validate_config(config: &[u8]) -> Result<RawFormat, CapsError> {
    check_header(config)?;
    Ok(STREAM)
}

/// Checks the stream PipeWire proposes matches what the configuration asks for.
///
/// # Errors
/// As for [`validate_config`], or `Invalid` on a mismatch.
pub fn check_stream(config: &[u8], stream: &PcmStream) -> Result<RawFormat, CapsError> {
    let want = validate_config(config)?;
    let matches = stream.raw_audio
        && stream.format == want.format
        && stream.rate == want.rate
        && stream.channels as usize == want.positions.len();
    if matches {
        Ok(want)
    } else {
        Err(CapsError::Invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps_with_body(body: &[u8]) -> Vec<u8> {
        [&VENDOR_HEADER[..], body].concat()
    }

    #[test]
    fn header_is_little_endian_vendor_and_codec() {
        assert_eq!(VENDOR_HEADER, [0xD7, 0, 0, 0, 0xAD, 0]);
        assert_eq!(fill_caps(), VENDOR_HEADER);
    }

    #[test]
    fn select_config_echoes_remote_caps() {
        let caps = caps_with_body(&[1, 2, 3]);
        assert_eq!(select_config(&caps).unwrap(), caps);
    }

    /// B&W Px7 S2e advertised caps body, `spec/px7s2e-a2dp-caps.md`.
    const PX7_S2E_BODY: [u8; 34] = [
        0x19, 0x0a, 0x37, 0x6a, 0, 0xaf, 0x64, 0x4b, 0, 0, 0xaa, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    #[test]
    fn px7_s2e_config_matches_caps_length() {
        let caps = caps_with_body(&PX7_S2E_BODY);
        assert_eq!(caps.len(), 40);
        assert_eq!(select_config(&caps).unwrap().len(), caps.len());
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
    fn validate_gives_48k_stereo() {
        assert_eq!(validate_config(&fill_caps()).unwrap().rate, 48_000);
    }

    #[test]
    fn check_stream_requires_exact_match() {
        let ok = PcmStream {
            raw_audio: true,
            format: SPA_AUDIO_FORMAT_S24_32_LE,
            rate: 48_000,
            channels: 2,
        };
        assert_eq!(check_stream(&fill_caps(), &ok), Ok(STREAM));
        for bad in [
            PcmStream {
                raw_audio: false,
                ..ok
            },
            PcmStream { rate: 44_100, ..ok },
            PcmStream { channels: 1, ..ok },
            PcmStream { format: 0, ..ok },
        ] {
            assert_eq!(check_stream(&fill_caps(), &bad), Err(CapsError::Invalid));
        }
    }
}

#![forbid(unsafe_code)]

#[cfg(any(test, feature = "fake"))]
pub mod fake;
mod frame;
mod stream;
mod tables;

use std::fmt;

pub use frame::HEADER_LEN;
pub use stream::{Core, Stream, table_row};
pub use tables::{DecoderLevel, Profile, Row, packet_budget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub profile: Profile,
    pub sample_rate: u32,
    pub channels: u8,
    pub level: DecoderLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    UnsupportedConfig,
    /// Encoder window not 12..=2016 bytes plus slack.
    WindowSize,
    /// Caller's buffer can't hold a packet.
    OutputTooSmall,
    Unimplemented,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedConfig => f.write_str("unsupported encoder configuration"),
            Self::WindowSize => f.write_str("encoder window size out of range"),
            Self::OutputTooSmall => f.write_str("output buffer too small"),
            Self::Unimplemented => f.write_str("encoder not implemented yet"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// What one encode call did, in interleaved samples and bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    pub consumed: usize,
    pub written: usize,
}

const MIN_WINDOW: usize = 12;
const MAX_WINDOW: usize = 2016;
/// Room the encoder gets past the packet size (§3.4).
pub const WINDOW_SLACK: usize = 64;

#[derive(Debug)]
#[expect(dead_code, reason = "read once the codec core exists")]
pub struct Encoder {
    config: Config,
    bitrate: u32,
}

impl Encoder {
    /// # Errors
    /// `UnsupportedConfig` for anything but 48 kHz stereo, or a zero bitrate.
    pub fn new(config: Config, bitrate: u32) -> Result<Self> {
        if config.sample_rate != 48_000 || config.channels != 2 || bitrate == 0 {
            return Err(Error::UnsupportedConfig);
        }
        Ok(Self { config, bitrate })
    }

    /// Takes any amount of interleaved 24-bit PCM (sign-extended in `i32`). Writes at most one
    /// packet into `window`, which is the table packet size plus [`WINDOW_SLACK`].
    ///
    /// # Errors
    /// `WindowSize` unless the packet size is 12..=2016 bytes, otherwise `Unimplemented` for
    /// now.
    pub fn encode_packet(&mut self, _pcm: &[i32], window: &mut [u8]) -> Result<Progress> {
        check_window(window)?;
        Err(Error::Unimplemented)
    }

    /// Emits the packet currently being built.
    ///
    /// # Errors
    /// As for [`Encoder::encode_packet`].
    pub fn flush(&mut self, window: &mut [u8]) -> Result<usize> {
        check_window(window)?;
        Err(Error::Unimplemented)
    }

    /// UNCERTAIN: the reference clamps this to a [min, max] range derived at create time,
    /// which the spec doesn't give.
    pub fn set_bitrate(&mut self, bps: u32) {
        self.bitrate = bps;
    }
}

fn check_window(window: &[u8]) -> Result<()> {
    let size = window.len().saturating_sub(WINDOW_SLACK);
    if (MIN_WINDOW..=MAX_WINDOW).contains(&size) {
        Ok(())
    } else {
        Err(Error::WindowSize)
    }
}

impl Core for Encoder {
    fn encode_packet(&mut self, pcm: &[i32], window: &mut [u8]) -> Result<Progress> {
        Encoder::encode_packet(self, pcm, window)
    }
    fn flush(&mut self, window: &mut [u8]) -> Result<usize> {
        Encoder::flush(self, window)
    }
    fn set_bitrate(&mut self, bps: u32) {
        Encoder::set_bitrate(self, bps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEREO_48K: Config = Config {
        profile: Profile::Hq,
        sample_rate: 48_000,
        channels: 2,
        level: DecoderLevel::R1,
    };

    #[test]
    fn rejects_unsupported_config() {
        let cfg = Config {
            sample_rate: 44_100,
            ..STEREO_48K
        };
        assert_eq!(
            Encoder::new(cfg, 364_000).unwrap_err(),
            Error::UnsupportedConfig
        );
        assert_eq!(
            Encoder::new(STEREO_48K, 0).unwrap_err(),
            Error::UnsupportedConfig
        );
    }

    #[test]
    fn encode_is_unimplemented() {
        let mut enc = Encoder::new(STEREO_48K, 364_000).unwrap();
        let mut out = [0u8; 656 + WINDOW_SLACK];
        assert_eq!(
            enc.encode_packet(&[0; 1344], &mut out),
            Err(Error::Unimplemented)
        );
        assert_eq!(enc.flush(&mut out), Err(Error::Unimplemented));
    }

    #[test]
    fn window_bounds() {
        let mut enc = Encoder::new(STEREO_48K, 364_000).unwrap();
        assert_eq!(enc.flush(&mut [0; 11 + 64]), Err(Error::WindowSize));
        assert_eq!(enc.flush(&mut [0; 2017 + 64]), Err(Error::WindowSize));
        assert_eq!(enc.flush(&mut [0; 2016 + 64]), Err(Error::Unimplemented));
        assert_eq!(enc.flush(&mut [0; 12 + 64]), Err(Error::Unimplemented));
    }
}

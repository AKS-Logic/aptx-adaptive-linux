#![forbid(unsafe_code)]

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub sample_rate: u32,
    pub channels: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    UnsupportedConfig,
    InputLength { expected: usize, got: usize },
    OutputTooSmall,
    Unimplemented,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedConfig => f.write_str("unsupported encoder configuration"),
            Self::InputLength { expected, got } => {
                write!(f, "expected {expected} PCM samples, got {got}")
            }
            Self::OutputTooSmall => f.write_str("output buffer too small"),
            Self::Unimplemented => f.write_str("encoder not implemented yet"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Placeholder until the spec defines the frame size.
const SAMPLES_PER_PACKET: usize = 128;

#[derive(Debug)]
pub struct Encoder {
    config: Config,
}

impl Encoder {
    /// # Errors
    /// `UnsupportedConfig` for anything but 48 kHz stereo.
    pub fn new(config: Config) -> Result<Self> {
        if config.sample_rate != 48_000 || config.channels != 2 {
            return Err(Error::UnsupportedConfig);
        }
        Ok(Self { config })
    }

    #[must_use]
    pub fn config(&self) -> Config {
        self.config
    }

    /// PCM frames (samples per channel) consumed by one `encode_packet` call.
    #[must_use]
    pub fn samples_per_packet(&self) -> usize {
        SAMPLES_PER_PACKET
    }

    /// Encoder delay in samples per channel. Placeholder until the spec defines it.
    #[must_use]
    pub fn delay_samples(&self) -> u32 {
        0
    }

    /// Encodes one packet of interleaved PCM (24-bit samples in the low bits of each `i32`)
    /// and returns the number of bytes written to `out`.
    ///
    /// # Errors
    /// `InputLength` if `pcm` isn't exactly one packet, otherwise `Unimplemented` for now.
    pub fn encode_packet(&mut self, pcm: &[i32], _out: &mut [u8]) -> Result<usize> {
        let expected = self.samples_per_packet() * usize::from(self.config.channels);
        if pcm.len() != expected {
            return Err(Error::InputLength {
                expected,
                got: pcm.len(),
            });
        }
        Err(Error::Unimplemented)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEREO_48K: Config = Config {
        sample_rate: 48_000,
        channels: 2,
    };

    #[test]
    fn rejects_unsupported_config() {
        let err = Encoder::new(Config {
            sample_rate: 44_100,
            channels: 2,
        })
        .unwrap_err();
        assert_eq!(err, Error::UnsupportedConfig);
    }

    #[test]
    fn encode_is_unimplemented() {
        let mut enc = Encoder::new(STEREO_48K).unwrap();
        let pcm = vec![0; enc.samples_per_packet() * 2];
        let mut out = [0u8; 1024];
        assert_eq!(enc.encode_packet(&pcm, &mut out), Err(Error::Unimplemented));
    }

    #[test]
    fn encode_checks_input_length() {
        let mut enc = Encoder::new(STEREO_48K).unwrap();
        let mut out = [0u8; 1024];
        assert!(matches!(
            enc.encode_packet(&[0; 3], &mut out),
            Err(Error::InputLength { got: 3, .. })
        ));
    }
}

#![forbid(unsafe_code)]

mod codec;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
mod frame;
mod stream;
mod tables;

use std::fmt;

use codec::packetiser::{self, Packetiser};
use codec::{Params, Variant};

pub use codec::alloc;

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
    /// Encoder window not a 12..=1032-byte packet (a multiple of 4) plus slack, or a different
    /// packet size from the first call.
    WindowSize,
    /// Caller's buffer can't hold a packet.
    OutputTooSmall,
    /// The packet size the link allows can't carry a period of audio.
    PacketTooSmall,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedConfig => f.write_str("unsupported encoder configuration"),
            Self::WindowSize => f.write_str("encoder window size out of range"),
            Self::OutputTooSmall => f.write_str("output buffer too small"),
            Self::PacketTooSmall => f.write_str("packet size too small for a period of audio"),
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
/// The packet header's size byte tops out at 1032 bytes (`codec-core.md` §7.1).
pub const MAX_PACKET: usize = codec::pack::MAX_PACKET;
/// Room the encoder gets past the packet size (§3.4).
pub const WINDOW_SLACK: usize = 64;

pub struct Encoder {
    params: Params,
    bitrate: u32,
    /// Taken by the packetiser on the first call.
    allocator: Option<Box<dyn alloc::Allocator>>,
    /// Built on the first call, once the packet size (the window) is known.
    packetiser: Option<Box<Packetiser>>,
}

impl fmt::Debug for Encoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Encoder")
            .field("params", &self.params)
            .field("bitrate", &self.bitrate)
            .finish_non_exhaustive()
    }
}

/// The bitstream variant for a config (`codec-core.md` §1): HQ at 88.2/96 kHz is config 3
/// whatever the decoder level; LL there isn't allowed.
fn params(config: Config) -> Result<Params> {
    let high_rate = match config.sample_rate {
        44_100 | 48_000 => false,
        88_200 | 96_000 => true,
        _ => return Err(Error::UnsupportedConfig),
    };
    let variant = match (config.profile, config.level, high_rate) {
        (Profile::Hq, _, true) | (Profile::Hq, DecoderLevel::R2, false) => Variant::Cfg3,
        (Profile::Hq, DecoderLevel::R1, false) => Variant::Cfg0,
        (Profile::Ll, DecoderLevel::R1, false) => Variant::Cfg1,
        // Config 4 (R2 LL) isn't specified, and LL is 44.1/48 kHz only.
        (Profile::Ll, _, _) => return Err(Error::UnsupportedConfig),
    };
    Ok(Params {
        variant,
        sample_rate: config.sample_rate,
    })
}

/// The smallest packet that carries `interleaved_samples` of PCM without dropping any, at the
/// cheapest allocation and the longest codewords.
///
/// # Errors
/// As for [`Encoder::new`].
pub fn min_packet_bytes(config: Config, interleaved_samples: usize) -> Result<usize> {
    let params = params(config)?;
    Ok(packetiser::min_packet_bytes(
        params,
        interleaved_samples / params.width(),
    ))
}

impl Encoder {
    /// # Errors
    /// `UnsupportedConfig` for anything but stereo configs 0, 1 and 3 at their sample rates,
    /// or a zero bitrate.
    pub fn new(config: Config, bitrate: u32) -> Result<Self> {
        Self::with_allocator(config, bitrate, Box::new(alloc::WaterFill))
    }

    /// An encoder that picks bit allocations with `allocator`. Allocations it gets wrong are
    /// replaced by the floor (see [`alloc::Allocator`]).
    ///
    /// # Errors
    /// As for [`Encoder::new`].
    pub fn with_allocator(
        config: Config,
        bitrate: u32,
        allocator: Box<dyn alloc::Allocator>,
    ) -> Result<Self> {
        if config.channels != 2 || bitrate == 0 {
            return Err(Error::UnsupportedConfig);
        }
        Ok(Self {
            params: params(config)?,
            bitrate,
            allocator: Some(allocator),
            packetiser: None,
        })
    }

    fn packetiser(&mut self, window: &[u8]) -> Result<&mut Packetiser> {
        let size = check_window(window)?;
        let (params, bitrate) = (self.params, self.bitrate);
        let allocator = &mut self.allocator;
        let p = self.packetiser.get_or_insert_with(|| {
            let allocator = allocator.take().expect("taken once, with the packetiser");
            Box::new(Packetiser::new(params, size, bitrate, allocator))
        });
        if p.packet_len() == size {
            Ok(p)
        } else {
            Err(Error::WindowSize)
        }
    }

    /// Takes any amount of interleaved 24-bit PCM (sign-extended in `i32`). Writes at most one
    /// packet into `window`, which is the table packet size plus [`WINDOW_SLACK`].
    ///
    /// Like the reference in steady state, this leaves packets to [`Encoder::flush`]. It
    /// consumes everything unless several packets' worth is already buffered.
    ///
    /// # Errors
    /// `WindowSize` unless the packet size is a multiple of 4 in 12..=[`MAX_PACKET`] bytes and
    /// the same on every call.
    pub fn encode_packet(&mut self, pcm: &[i32], window: &mut [u8]) -> Result<Progress> {
        let consumed = self.packetiser(window)?.push_pcm(pcm);
        Ok(Progress {
            consumed,
            written: 0,
        })
    }

    /// Emits the packet currently being built.
    ///
    /// # Errors
    /// As for [`Encoder::encode_packet`].
    pub fn flush(&mut self, window: &mut [u8]) -> Result<usize> {
        let p = self.packetiser(window)?;
        let size = p.packet_len();
        p.emit(&mut window[..size]);
        Ok(size)
    }

    /// Blocks of audio (one subband sample per band per channel) dropped so far because a
    /// packet couldn't hold them even at the cheapest allocation. Only happens with packets
    /// near [`min_packet_bytes`].
    #[must_use]
    pub fn dropped_blocks(&self) -> u64 {
        self.packetiser.as_ref().map_or(0, |p| p.dropped())
    }

    /// Picks the allocation caps. UNCERTAIN: the reference clamps this to a [min, max] range
    /// derived at create time, which the spec doesn't give.
    pub fn set_bitrate(&mut self, bps: u32) {
        self.bitrate = bps;
        if let Some(p) = &mut self.packetiser {
            p.set_bitrate(bps);
        }
    }
}

/// The packet size a window is for.
fn check_window(window: &[u8]) -> Result<usize> {
    let size = window.len().saturating_sub(WINDOW_SLACK);
    if (MIN_WINDOW..=MAX_PACKET).contains(&size) && size.is_multiple_of(4) {
        Ok(size)
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
    fn dropped_blocks(&self) -> u64 {
        Encoder::dropped_blocks(self)
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
        for cfg in [
            Config {
                channels: 1,
                ..STEREO_48K
            },
            Config {
                profile: Profile::Ll,
                level: DecoderLevel::R2,
                ..STEREO_48K
            },
            Config {
                profile: Profile::Ll,
                sample_rate: 96_000,
                ..STEREO_48K
            },
            Config {
                sample_rate: 32_000,
                ..STEREO_48K
            },
        ] {
            assert_eq!(
                Encoder::new(cfg, 364_000).unwrap_err(),
                Error::UnsupportedConfig
            );
        }
        assert_eq!(
            Encoder::new(STEREO_48K, 0).unwrap_err(),
            Error::UnsupportedConfig
        );
    }

    /// §1: HQ at 88.2/96 kHz is config 3 for any decoder level, R1 included.
    #[test]
    fn hq_high_rate_is_config_3() {
        for level in [DecoderLevel::R1, DecoderLevel::R2] {
            for sample_rate in [88_200, 96_000] {
                let cfg = Config {
                    sample_rate,
                    level,
                    ..STEREO_48K
                };
                assert_eq!(params(cfg).unwrap().variant, Variant::Cfg3);
                let mut enc = Encoder::new(cfg, 420_000).unwrap();
                let mut out = [0u8; 656 + WINDOW_SLACK];
                enc.encode_packet(&[0; 2880], &mut out).unwrap();
                assert_eq!(enc.flush(&mut out), Ok(656));
                assert_eq!(out[1] & 0x7F, 3);
            }
        }
        let r1_48k = params(STEREO_48K).unwrap();
        assert_eq!(r1_48k.variant, Variant::Cfg0);
    }

    /// An injected allocator is used; one that breaks the rules is overridden by the floor.
    #[test]
    fn takes_an_allocator() {
        struct Bad;
        impl alloc::Allocator for Bad {
            fn allocate(
                &mut self,
                _: &[i32],
                _: &[u8],
                _: &mut dyn FnMut(&[u8]) -> bool,
                out: &mut [u8],
            ) {
                out.fill(1);
            }
        }
        let mut enc = Encoder::with_allocator(STEREO_48K, 364_000, Box::new(Bad)).unwrap();
        let mut out = [0u8; 656 + WINDOW_SLACK];
        enc.encode_packet(&[1000; 1344], &mut out).unwrap();
        enc.flush(&mut out).unwrap();
        // Header 0xA, then pairs (4, 0) twice: ρ = 3, 0.
        let p = enc.packetiser.as_ref().unwrap();
        assert_eq!(p.pending_blocks(), 0);
        assert_eq!(out[3] >> 3, 0b1_1010);
    }

    /// A packet below the minimum drops blocks, and the count shows on the encoder.
    #[test]
    fn counts_dropped_blocks() {
        let mut enc = Encoder::new(STEREO_48K, 364_000).unwrap();
        let mut out = [0u8; 128 + WINDOW_SLACK];
        let mut s = 1u32;
        let pcm: Vec<i32> = (0..1344)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s as i32) >> 10
            })
            .collect();
        assert_eq!(enc.dropped_blocks(), 0);
        for _ in 0..4 {
            enc.encode_packet(&pcm, &mut out).unwrap();
            enc.flush(&mut out).unwrap();
        }
        assert!(enc.dropped_blocks() > 0);
        let fine = Encoder::new(STEREO_48K, 364_000).unwrap();
        assert_eq!(Core::dropped_blocks(&fine), 0);
    }

    #[test]
    fn min_packet_is_under_the_px7_packet() {
        let need = min_packet_bytes(STEREO_48K, 1344).unwrap();
        assert!(need <= 656 && need > 128, "{need}");
    }

    #[test]
    fn encode_buffers_and_flush_emits() {
        let mut enc = Encoder::new(STEREO_48K, 364_000).unwrap();
        let mut out = [0u8; 656 + WINDOW_SLACK];
        assert_eq!(
            enc.encode_packet(&[0; 1344], &mut out),
            Ok(Progress {
                consumed: 1344,
                written: 0
            })
        );
        assert_eq!(enc.flush(&mut out), Ok(656));
        assert_eq!(out[..3], [0x00, 0x81, 0xA1]);
        // Continuation 0 is the single bit 1, then header 0xA.
        assert_eq!(out[3] >> 3, 0b1_1010);
    }

    #[test]
    fn window_bounds() {
        let mut enc = Encoder::new(STEREO_48K, 364_000).unwrap();
        assert_eq!(enc.flush(&mut [0; 11 + 64]), Err(Error::WindowSize));
        assert_eq!(enc.flush(&mut [0; 2017 + 64]), Err(Error::WindowSize));
        assert_eq!(enc.flush(&mut [0; 658 + 64]), Err(Error::WindowSize));
        assert_eq!(enc.flush(&mut [0; 1036 + 64]), Err(Error::WindowSize));
        let mut window = [0; 1032 + 64];
        assert_eq!(enc.flush(&mut window), Ok(1032));
        assert_eq!(window[2], 0xFF);
        // The packet size is fixed by the first call.
        assert_eq!(enc.flush(&mut [0; 12 + 64]), Err(Error::WindowSize));
    }
}

//! Dev-only switches, read from the environment of the process that loads the plugin.
//!
//! - `APTX_ADAPTIVE_REPLAY=/path/to/file.bin`, only with the `dev-replay` feature: ignore PCM and send the file's raw codec packets
//!   (e.g. `tests/vectors/r2/sine1k.r1.bin`) in order, looping. Framing, RTP and timing stay
//!   the real code path. For testing negotiation and framing on a headset before the codec
//!   core exists.
//! - `APTX_ADAPTIVE_FRAMING=header-in-rtp|header-after-rtp`: overrides [`session::FRAMING`].

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use aptx_adaptive::{Core, Encoder, Progress, Result as CoreResult, WINDOW_SLACK};

use crate::session::{self, Framing};

pub const REPLAY_VAR: &str = "APTX_ADAPTIVE_REPLAY";
pub const FRAMING_VAR: &str = "APTX_ADAPTIVE_FRAMING";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub framing: Framing,
    pub replay: Option<PathBuf>,
}

impl Options {
    /// # Errors
    /// A message if a variable is set to something unusable.
    pub fn from_env(var: impl Fn(&str) -> Option<OsString>) -> Result<Self, String> {
        let framing = match var(FRAMING_VAR) {
            None => session::FRAMING,
            Some(v) => match v.to_str() {
                Some("header-in-rtp") => Framing::HeaderInRtp,
                Some("header-after-rtp") => Framing::HeaderAfterRtp,
                _ => {
                    return Err(format!(
                        "{FRAMING_VAR}={} is not header-in-rtp or header-after-rtp",
                        v.display()
                    ));
                }
            },
        };
        let replay = var(REPLAY_VAR).filter(|v| !v.is_empty()).map(PathBuf::from);
        if cfg!(feature = "dev-replay") {
            Ok(Self { framing, replay })
        } else {
            Ok(Self {
                framing,
                replay: None,
            })
        }
    }

    /// What to log at init: nothing unless something differs from a normal build.
    pub fn describe(&self, var: impl Fn(&str) -> Option<OsString>) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(p) = &self.replay {
            parts.push(format!(
                "core: REPLAY of {} (dev only, PCM ignored)",
                p.display()
            ));
        } else if var(REPLAY_VAR).is_some_and(|v| !v.is_empty()) {
            parts.push(format!(
                "{REPLAY_VAR} ignored: built without the dev-replay feature"
            ));
        }
        if self.framing != session::FRAMING {
            parts.push(format!("framing: {:?}", self.framing));
        }
        (!parts.is_empty()).then(|| format!("aptx-adaptive: {}", parts.join("; ")))
    }
}

/// Sends pre-encoded packets instead of encoding. Consumes PCM like the reference in steady
/// state (one period's worth, then waits for the flush), so the packet loop's timing is real.
#[derive(Debug)]
pub struct Replay {
    packets: Vec<u8>,
    packet_bytes: usize,
    next: usize,
    capacity: usize,
    buffered: usize,
}

impl Replay {
    /// `capacity` is interleaved samples per packet.
    ///
    /// # Errors
    /// A message unless `packets` is a non-empty run of `packet_bytes`-sized packets.
    pub fn new(packets: Vec<u8>, packet_bytes: usize, capacity: usize) -> Result<Self, String> {
        if packet_bytes == 0 || packets.is_empty() || !packets.len().is_multiple_of(packet_bytes) {
            return Err(format!(
                "replay file is {} bytes, not a whole number of {packet_bytes}-byte packets",
                packets.len()
            ));
        }
        Ok(Self {
            packets,
            packet_bytes,
            next: 0,
            capacity,
            buffered: 0,
        })
    }

    /// # Errors
    /// A message if the file can't be read or isn't whole packets.
    pub fn from_file(
        path: &std::path::Path,
        packet_bytes: usize,
        capacity: usize,
    ) -> Result<Self, String> {
        let packets = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::new(packets, packet_bytes, capacity)
    }

    fn emit(&mut self, window: &mut [u8]) -> usize {
        let n = self.packet_bytes;
        let start = self.next * n;
        window[..n].copy_from_slice(&self.packets[start..start + n]);
        self.next = (self.next + 1) % (self.packets.len() / n);
        self.buffered = 0;
        n
    }
}

impl Core for Replay {
    fn encode_packet(&mut self, pcm: &[i32], _window: &mut [u8]) -> CoreResult<Progress> {
        let consumed = pcm.len().min(self.capacity - self.buffered);
        self.buffered += consumed;
        Ok(Progress {
            consumed,
            written: 0,
        })
    }

    fn flush(&mut self, window: &mut [u8]) -> CoreResult<usize> {
        if window.len() < self.packet_bytes + WINDOW_SLACK {
            return Err(aptx_adaptive::Error::WindowSize);
        }
        Ok(self.emit(window))
    }

    fn set_bitrate(&mut self, _bps: u32) {}
}

/// The core a session runs on: the real encoder, or a replay in dev.
#[derive(Debug)]
pub enum AnyCore {
    Encoder(Encoder),
    Replay(Replay),
}

impl Core for AnyCore {
    fn encode_packet(&mut self, pcm: &[i32], window: &mut [u8]) -> CoreResult<Progress> {
        match self {
            Self::Encoder(c) => Core::encode_packet(c, pcm, window),
            Self::Replay(c) => c.encode_packet(pcm, window),
        }
    }
    fn flush(&mut self, window: &mut [u8]) -> CoreResult<usize> {
        match self {
            Self::Encoder(c) => Core::flush(c, window),
            Self::Replay(c) => c.flush(window),
        }
    }
    fn set_bitrate(&mut self, bps: u32) {
        match self {
            Self::Encoder(c) => Core::set_bitrate(c, bps),
            Self::Replay(c) => c.set_bitrate(bps),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let m: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).into()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn defaults_are_silent() {
        let e = env(&[]);
        let o = Options::from_env(&e).unwrap();
        assert_eq!(o.framing, session::FRAMING);
        assert_eq!(o.replay, None);
        assert_eq!(o.describe(&e), None);
        let e = env(&[(REPLAY_VAR, ""), (FRAMING_VAR, "header-in-rtp")]);
        let o = Options::from_env(&e).unwrap();
        assert_eq!(o.replay, None);
        assert_eq!(o.describe(&e), None);
    }

    #[test]
    fn parses_framing() {
        let e = env(&[(FRAMING_VAR, "header-after-rtp")]);
        let o = Options::from_env(&e).unwrap();
        assert_eq!(o.framing, Framing::HeaderAfterRtp);
        assert_eq!(
            o.describe(&e).as_deref(),
            Some("aptx-adaptive: framing: HeaderAfterRtp")
        );
        assert!(Options::from_env(env(&[(FRAMING_VAR, "bogus")])).is_err());
    }

    #[cfg(feature = "dev-replay")]
    #[test]
    fn replay_var_with_feature() {
        let e = env(&[(REPLAY_VAR, "/x/y.bin")]);
        let o = Options::from_env(&e).unwrap();
        assert_eq!(o.replay, Some(PathBuf::from("/x/y.bin")));
        assert!(o.describe(&e).unwrap().contains("REPLAY of /x/y.bin"));
    }

    #[cfg(not(feature = "dev-replay"))]
    #[test]
    fn replay_var_ignored_without_feature() {
        let e = env(&[(REPLAY_VAR, "/x/y.bin")]);
        let o = Options::from_env(&e).unwrap();
        assert_eq!(o.replay, None);
        assert!(o.describe(&e).unwrap().contains("ignored"));
    }

    #[test]
    fn rejects_partial_packets() {
        assert!(Replay::new(vec![0; 656 * 2 + 1], 656, 1344).is_err());
        assert!(Replay::new(Vec::new(), 656, 1344).is_err());
        assert!(Replay::new(vec![0; 656], 656, 1344).is_ok());
    }

    #[test]
    fn consumes_one_period_then_waits_for_flush() {
        let mut r = Replay::new((0..8).collect(), 4, 10).unwrap();
        let mut w = [0; 4 + WINDOW_SLACK];
        assert_eq!(r.encode_packet(&[0; 6], &mut w).unwrap().consumed, 6);
        assert_eq!(r.encode_packet(&[0; 6], &mut w).unwrap().consumed, 4);
        assert_eq!(r.encode_packet(&[0; 6], &mut w).unwrap().consumed, 0);
        assert_eq!(r.flush(&mut w), Ok(4));
        assert_eq!(w[..4], [0, 1, 2, 3]);
        assert_eq!(r.flush(&mut w), Ok(4));
        assert_eq!(w[..4], [4, 5, 6, 7]);
        assert_eq!(r.flush(&mut w), Ok(4));
        assert_eq!(w[..4], [0, 1, 2, 3]);
        assert_eq!(
            r.flush(&mut [0; 4 + WINDOW_SLACK - 1]),
            Err(aptx_adaptive::Error::WindowSize)
        );
    }
}

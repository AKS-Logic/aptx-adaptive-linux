//! Dev-only switches, read from the environment of the process that loads the plugin.
//!
//! - `APTX_ADAPTIVE_REPLAY=/path/to/file.bin`, only with the `dev-replay` feature: ignore PCM and send the file's raw codec packets
//!   (e.g. `tests/vectors/r2/sine1k.r1.bin`) in order, looping. Framing, RTP and timing stay
//!   the real code path. For testing negotiation and framing on a headset before the codec
//!   core exists.
//! - `APTX_ADAPTIVE_FRAMING=phone`: the default, [`session::FRAMING`] (spec §7D). With
//!   `dev-replay`, also the superseded `ttp-clock-offset`, `header-in-rtp`, `header-after-rtp`
//!   and `header-in-rtp-swapped`.
//! - `APTX_ADAPTIVE_PT=0..127`, only with `dev-replay`: the RTP payload type.
//! - `APTX_ADAPTIVE_NOSWAP=1`, only with `dev-replay`: send the payload without the 16-bit swap.
//! - `APTX_ADAPTIVE_SCMST=1`, only with `dev-replay`: put a 1-byte SCMS-T content protection
//!   header right after the RTP header, for a sink configured with SCMS-T (needs a bluetoothd that
//!   puts Content Protection in Set Configuration). The byte is [`SCMST_COPY_FREE`] unless
//!   `APTX_ADAPTIVE_SCMST_BYTE=0x..` overrides it.
//! - `APTX_ADAPTIVE_SOURCE_TYPE=0|2`, only with `dev-replay`: the source type `select_config`
//!   puts in byte 6 (spec §7E; default 2).
//! - `APTX_ADAPTIVE_SSRC=0x..`, only with `dev-replay`: the whole 32-bit RTP SSRC, instead of
//!   the §7F latency mode (`0x000000a3` for HQ).
//! - `APTX_ADAPTIVE_TS=samples-per-packet|us|samples`, only with `dev-replay`: the RTP
//!   timestamp. `samples-per-packet` is the default (§7F: `0x000002a0` on every 672-sample
//!   packet); `us` and `samples` are clocks stepping by the packet duration in µs (+14000 per
//!   packet) or in samples (+672).
//! - `APTX_ADAPTIVE_DRIFT_LOG=0`, in every build: don't log the drift line (see `drift`).
//! - `APTX_ADAPTIVE_TTP_ADJUST=off|fixed:<ms>|auto`, only with `dev-replay`: the §7F latency
//!   adjustment in the timestamp's high 16 bits (default off = 0). See `drift`.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use aptx_adaptive::{Core, Encoder, Profile, Progress, Result as CoreResult, WINDOW_SLACK};

use crate::caps::SOURCE_TYPE_HOST;
use crate::drift::{DRIFT_LOG_VAR, TTP_ADJUST_VAR, TtpAdjust};
use crate::session::{self, Framing, TS_UNIT, TsUnit};

pub const REPLAY_VAR: &str = "APTX_ADAPTIVE_REPLAY";
pub const FRAMING_VAR: &str = "APTX_ADAPTIVE_FRAMING";
pub const PT_VAR: &str = "APTX_ADAPTIVE_PT";
pub const NOSWAP_VAR: &str = "APTX_ADAPTIVE_NOSWAP";
pub const SCMST_VAR: &str = "APTX_ADAPTIVE_SCMST";
pub const SCMST_BYTE_VAR: &str = "APTX_ADAPTIVE_SCMST_BYTE";
pub const SOURCE_TYPE_VAR: &str = "APTX_ADAPTIVE_SOURCE_TYPE";
pub const SSRC_VAR: &str = "APTX_ADAPTIVE_SSRC";
pub const TS_VAR: &str = "APTX_ADAPTIVE_TS";

/// The A2DP content protection header for SCMS-T: bit 0 is the L-bit, bit 1 the Cp-bit (Bluetooth
/// Assigned Numbers, A/V content protection, note 3). Cp = 1, L = 0 is "copy freely", what
/// Android sends when it doesn't restrict copying.
pub const SCMST_COPY_FREE: u8 = 0x02;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub framing: Framing,
    pub replay: Option<PathBuf>,
    pub payload_type: u8,
    pub swap: bool,
    /// The SCMS-T header byte to send after the RTP header, if any.
    pub scmst: Option<u8>,
    /// What `select_config` puts in byte 6 bits 0..2. Read again there, since that runs
    /// before `init`; here only so `init` can log it.
    pub source_type: u8,
    /// Replaces the whole RTP SSRC.
    pub ssrc: Option<u32>,
    pub ts: TsUnit,
    pub ttp_adjust: TtpAdjust,
    /// Log the drift line every 10 s. Not a dev switch: read in every build.
    pub drift_log: bool,
}

/// `APTX_ADAPTIVE_SOURCE_TYPE`, for `select_config`: [`SOURCE_TYPE_HOST`] unless overridden in
/// a `dev-replay` build.
///
/// # Errors
/// A message unless it's 0 or 2.
pub fn source_type(var: impl Fn(&str) -> Option<OsString>) -> Result<u8, String> {
    if !cfg!(feature = "dev-replay") {
        return Ok(SOURCE_TYPE_HOST);
    }
    match var(SOURCE_TYPE_VAR) {
        None => Ok(SOURCE_TYPE_HOST),
        Some(v) => match v.to_str() {
            Some("0") => Ok(0),
            Some("2") => Ok(2),
            _ => Err(format!("{SOURCE_TYPE_VAR}={} is not 0 or 2", v.display())),
        },
    }
}

fn hex_u32(var: &str, v: &OsString) -> Result<u32, String> {
    v.to_str()
        .and_then(|s| s.strip_prefix("0x"))
        .filter(|s| !s.is_empty() && s.len() <= 8)
        .and_then(|s| u32::from_str_radix(s, 16).ok())
        .ok_or_else(|| format!("{var}={} is not 0x0..0xffffffff", v.display()))
}

impl Default for Options {
    fn default() -> Self {
        Self {
            framing: session::FRAMING,
            replay: None,
            payload_type: session::RTP_PAYLOAD_TYPE,
            swap: true,
            scmst: None,
            source_type: SOURCE_TYPE_HOST,
            ssrc: None,
            ts: TS_UNIT,
            ttp_adjust: TtpAdjust::Off,
            drift_log: true,
        }
    }
}

impl Options {
    /// # Errors
    /// A message if a variable is set to something unusable.
    pub fn from_env(var: impl Fn(&str) -> Option<OsString>) -> Result<Self, String> {
        let dev = cfg!(feature = "dev-replay");
        let framing = match var(FRAMING_VAR) {
            None => session::FRAMING,
            Some(v) => match v.to_str() {
                Some("phone") => Framing::Phone,
                Some("ttp-clock-offset") if dev => Framing::TtpClockOffset,
                Some("header-in-rtp") if dev => Framing::HeaderInRtp,
                Some("header-after-rtp") if dev => Framing::HeaderAfterRtp,
                Some("header-in-rtp-swapped") if dev => Framing::HeaderInRtpSwapped,
                _ => {
                    let names = if dev {
                        "phone, ttp-clock-offset, header-in-rtp, header-after-rtp or header-in-rtp-swapped"
                    } else {
                        "phone (the others need dev-replay)"
                    };
                    return Err(format!("{FRAMING_VAR}={} is not {names}", v.display()));
                }
            },
        };
        let drift_log = var(DRIFT_LOG_VAR).is_none_or(|v| v != "0");
        if !dev {
            return Ok(Self {
                framing,
                drift_log,
                ..Self::default()
            });
        }
        let replay = var(REPLAY_VAR).filter(|v| !v.is_empty()).map(PathBuf::from);
        let payload_type = match var(PT_VAR) {
            None => session::RTP_PAYLOAD_TYPE,
            Some(v) => v
                .to_str()
                .and_then(|s| s.parse::<u8>().ok())
                .filter(|pt| *pt <= 127)
                .ok_or_else(|| format!("{PT_VAR}={} is not 0..127", v.display()))?,
        };
        let swap = var(NOSWAP_VAR).is_none_or(|v| v != "1");
        let scmst = match var(SCMST_VAR) {
            Some(v) if v == "1" => Some(match var(SCMST_BYTE_VAR) {
                None => SCMST_COPY_FREE,
                Some(b) => b
                    .to_str()
                    .and_then(|s| s.strip_prefix("0x"))
                    .and_then(|s| u8::from_str_radix(s, 16).ok())
                    .ok_or_else(|| format!("{SCMST_BYTE_VAR}={} is not 0x00..0xff", b.display()))?,
            }),
            _ => None,
        };
        let source_type = source_type(&var)?;
        let ssrc = var(SSRC_VAR).map(|v| hex_u32(SSRC_VAR, &v)).transpose()?;
        let ts = match var(TS_VAR) {
            None => TS_UNIT,
            Some(v) => match v.to_str() {
                Some("samples-per-packet") => TsUnit::SamplesPerPacket,
                Some("us") => TsUnit::Micros,
                Some("samples") => TsUnit::Samples,
                _ => {
                    return Err(format!(
                        "{TS_VAR}={} is not samples-per-packet, us or samples",
                        v.display()
                    ));
                }
            },
        };
        let ttp_adjust = var(TTP_ADJUST_VAR)
            .map(|v| TtpAdjust::parse(&v))
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            framing,
            replay,
            payload_type,
            swap,
            scmst,
            source_type,
            ssrc,
            ts,
            ttp_adjust,
            drift_log,
        })
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
        if self.payload_type != session::RTP_PAYLOAD_TYPE {
            parts.push(format!("payload type: {}", self.payload_type));
        }
        if !self.swap {
            parts.push("payload swap: off".to_owned());
        }
        if let Some(b) = self.scmst {
            parts.push(format!("SCMS-T header: {b:#04x}"));
        }
        if self.source_type != SOURCE_TYPE_HOST {
            parts.push(format!("source type: {}", self.source_type));
        }
        if let Some(s) = self.ssrc.filter(|s| *s != session::mode_ssrc(Profile::Hq)) {
            parts.push(format!("SSRC: {s:#010x}"));
        }
        if self.ts != TS_UNIT {
            parts.push(format!("timestamp: {:?}", self.ts));
        }
        if self.ttp_adjust != TtpAdjust::Off {
            parts.push(format!("ttp adjust: {:?}", self.ttp_adjust));
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
    fn prepare(&mut self, row: &aptx_adaptive::Row) -> CoreResult<()> {
        match self {
            Self::Encoder(c) => Core::prepare(c, row),
            Self::Replay(_) => Ok(()),
        }
    }
    /// The replay file holds one profile's packets, so it can't reset to another.
    fn reset(&mut self, config: aptx_adaptive::Config, row: &aptx_adaptive::Row) -> CoreResult<()> {
        match self {
            Self::Encoder(c) => Core::reset(c, config, row),
            Self::Replay(_) => Err(aptx_adaptive::Error::UnsupportedConfig),
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
        let e = env(&[(REPLAY_VAR, ""), (FRAMING_VAR, "phone")]);
        let o = Options::from_env(&e).unwrap();
        assert_eq!(o.replay, None);
        assert_eq!(o.describe(&e), None);
    }

    #[test]
    fn drift_log_switch_in_every_build() {
        assert!(Options::from_env(env(&[])).unwrap().drift_log);
        assert!(
            Options::from_env(env(&[(DRIFT_LOG_VAR, "1")]))
                .unwrap()
                .drift_log
        );
        let e = env(&[(DRIFT_LOG_VAR, "0")]);
        let o = Options::from_env(&e).unwrap();
        assert!(!o.drift_log);
        assert_eq!(o.describe(&e), None);
    }

    #[test]
    fn parses_framing() {
        assert_eq!(session::FRAMING, Framing::Phone);
        let o = Options::from_env(env(&[(FRAMING_VAR, "phone")])).unwrap();
        assert_eq!(o.framing, Framing::Phone);
        assert!(Options::from_env(env(&[(FRAMING_VAR, "bogus")])).is_err());
        assert!(Options::from_env(env(&[(FRAMING_VAR, "ttp-clock")])).is_err());
    }

    #[cfg(feature = "dev-replay")]
    #[test]
    fn parses_old_framings_with_feature() {
        let e = env(&[(FRAMING_VAR, "header-after-rtp")]);
        assert_eq!(
            Options::from_env(&e).unwrap().describe(&e).as_deref(),
            Some("aptx-adaptive: framing: HeaderAfterRtp")
        );
        for (name, framing) in [
            ("ttp-clock-offset", Framing::TtpClockOffset),
            ("header-in-rtp", Framing::HeaderInRtp),
            ("header-after-rtp", Framing::HeaderAfterRtp),
        ] {
            let o = Options::from_env(env(&[(FRAMING_VAR, name)])).unwrap();
            assert_eq!(o.framing, framing);
        }
    }

    #[cfg(feature = "dev-replay")]
    #[test]
    fn replay_var_with_feature() {
        let e = env(&[(REPLAY_VAR, "/x/y.bin")]);
        let o = Options::from_env(&e).unwrap();
        assert_eq!(o.replay, Some(PathBuf::from("/x/y.bin")));
        assert!(o.describe(&e).unwrap().contains("REPLAY of /x/y.bin"));
    }

    #[cfg(feature = "dev-replay")]
    #[test]
    fn ab_switches_with_feature() {
        let e = env(&[
            (FRAMING_VAR, "header-in-rtp-swapped"),
            (PT_VAR, "0"),
            (NOSWAP_VAR, "1"),
        ]);
        let o = Options::from_env(&e).unwrap();
        assert_eq!(o.framing, Framing::HeaderInRtpSwapped);
        assert_eq!(o.payload_type, 0);
        assert!(!o.swap);
        assert_eq!(
            o.describe(&e).as_deref(),
            Some("aptx-adaptive: framing: HeaderInRtpSwapped; payload type: 0; payload swap: off")
        );
        let o = Options::from_env(env(&[(PT_VAR, "127"), (NOSWAP_VAR, "0")])).unwrap();
        assert_eq!((o.payload_type, o.swap), (127, true));
        assert!(Options::from_env(env(&[(PT_VAR, "128")])).is_err());
        assert!(Options::from_env(env(&[(PT_VAR, "x")])).is_err());
    }

    #[cfg(feature = "dev-replay")]
    #[test]
    fn scmst_with_feature() {
        let e = env(&[(SCMST_VAR, "1")]);
        let o = Options::from_env(&e).unwrap();
        assert_eq!(o.scmst, Some(0x02));
        assert_eq!(
            o.describe(&e).as_deref(),
            Some("aptx-adaptive: SCMS-T header: 0x02")
        );
        let o = Options::from_env(env(&[(SCMST_VAR, "1"), (SCMST_BYTE_VAR, "0x00")])).unwrap();
        assert_eq!(o.scmst, Some(0));
        let o = Options::from_env(env(&[(SCMST_VAR, "0"), (SCMST_BYTE_VAR, "0x01")])).unwrap();
        assert_eq!(o.scmst, None);
        assert!(Options::from_env(env(&[(SCMST_VAR, "1"), (SCMST_BYTE_VAR, "2")])).is_err());
        assert!(Options::from_env(env(&[(SCMST_VAR, "1"), (SCMST_BYTE_VAR, "0x100")])).is_err());
    }

    #[cfg(feature = "dev-replay")]
    #[test]
    fn source_type_with_feature() {
        assert_eq!(source_type(env(&[])), Ok(2));
        assert_eq!(source_type(env(&[(SOURCE_TYPE_VAR, "2")])), Ok(2));
        assert_eq!(source_type(env(&[(SOURCE_TYPE_VAR, "0")])), Ok(0));
        for bad in ["1", "3", "", "0x02"] {
            assert!(
                source_type(env(&[(SOURCE_TYPE_VAR, bad)])).is_err(),
                "{bad}"
            );
        }
        let e = env(&[(SOURCE_TYPE_VAR, "0")]);
        let o = Options::from_env(&e).unwrap();
        assert_eq!(o.source_type, 0);
        assert_eq!(
            o.describe(&e).as_deref(),
            Some("aptx-adaptive: source type: 0")
        );
        let e = env(&[(SOURCE_TYPE_VAR, "2")]);
        assert_eq!(Options::from_env(&e).unwrap().describe(&e), None);
    }

    #[cfg(feature = "dev-replay")]
    #[test]
    fn ssrc_with_feature() {
        assert_eq!(Options::from_env(env(&[])).unwrap().ssrc, None);
        let e = env(&[(SSRC_VAR, "0xa1")]);
        let o = Options::from_env(&e).unwrap();
        assert_eq!(o.ssrc, Some(0xa1));
        assert_eq!(
            o.describe(&e).as_deref(),
            Some("aptx-adaptive: SSRC: 0x000000a1")
        );
        let e = env(&[(SSRC_VAR, "0xa3")]);
        assert_eq!(Options::from_env(&e).unwrap().describe(&e), None);
        let o = Options::from_env(env(&[(SSRC_VAR, "0xDEADBEEF")])).unwrap();
        assert_eq!(o.ssrc, Some(0xdead_beef));
        for bad in ["a1", "0x", "0x100000000", "0xzz", "161"] {
            assert!(Options::from_env(env(&[(SSRC_VAR, bad)])).is_err(), "{bad}");
        }
    }

    #[cfg(feature = "dev-replay")]
    #[test]
    fn ts_unit_with_feature() {
        assert_eq!(
            Options::from_env(env(&[])).unwrap().ts,
            TsUnit::SamplesPerPacket
        );
        let e = env(&[(TS_VAR, "samples-per-packet")]);
        let o = Options::from_env(&e).unwrap();
        assert_eq!(o.ts, TsUnit::SamplesPerPacket);
        assert_eq!(o.describe(&e), None);
        let e = env(&[(TS_VAR, "us")]);
        let o = Options::from_env(&e).unwrap();
        assert_eq!(o.ts, TsUnit::Micros);
        assert_eq!(
            o.describe(&e).as_deref(),
            Some("aptx-adaptive: timestamp: Micros")
        );
        let e = env(&[(TS_VAR, "samples")]);
        let o = Options::from_env(&e).unwrap();
        assert_eq!(o.ts, TsUnit::Samples);
        assert_eq!(
            o.describe(&e).as_deref(),
            Some("aptx-adaptive: timestamp: Samples")
        );
        assert!(Options::from_env(env(&[(TS_VAR, "ms")])).is_err());
    }

    #[cfg(feature = "dev-replay")]
    #[test]
    fn ttp_adjust_with_feature() {
        assert_eq!(
            Options::from_env(env(&[])).unwrap().ttp_adjust,
            TtpAdjust::Off
        );
        let e = env(&[(TTP_ADJUST_VAR, "off")]);
        assert_eq!(Options::from_env(&e).unwrap().describe(&e), None);
        let e = env(&[(TTP_ADJUST_VAR, "fixed:-3")]);
        let o = Options::from_env(&e).unwrap();
        assert_eq!(o.ttp_adjust, TtpAdjust::Fixed(-3));
        assert_eq!(
            o.describe(&e).as_deref(),
            Some("aptx-adaptive: ttp adjust: Fixed(-3)")
        );
        let o = Options::from_env(env(&[(TTP_ADJUST_VAR, "auto")])).unwrap();
        assert_eq!(o.ttp_adjust, TtpAdjust::Auto);
        assert!(Options::from_env(env(&[(TTP_ADJUST_VAR, "fixed")])).is_err());
    }

    #[cfg(not(feature = "dev-replay"))]
    #[test]
    fn ab_switches_ignored_without_feature() {
        let e = env(&[
            (PT_VAR, "0"),
            (NOSWAP_VAR, "1"),
            (SCMST_VAR, "1"),
            (SOURCE_TYPE_VAR, "0"),
            (SSRC_VAR, "0xa1"),
            (TS_VAR, "samples"),
            (TTP_ADJUST_VAR, "fixed:5"),
        ]);
        let o = Options::from_env(&e).unwrap();
        assert_eq!(o, Options::default());
        assert_eq!(o.describe(&e), None);
        assert_eq!(source_type(&e), Ok(SOURCE_TYPE_HOST));
        for name in [
            "ttp-clock-offset",
            "header-in-rtp",
            "header-after-rtp",
            "header-in-rtp-swapped",
        ] {
            assert!(Options::from_env(env(&[(FRAMING_VAR, name)])).is_err());
        }
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

//! One encoding stream: RTP framing around an [`aptx_adaptive::Stream`]. Safe Rust, no FFI.
//!
//! On air (spec §7D, §7F, §7H): a 12-byte RTP header, `80 60` (V=2, PT 96), PipeWire's
//! sequence number, timestamp = samples per channel in the packet (TTP adjust 0 in the high
//! 16 bits), SSRC = the latency mode; then the 16-bit byte-swapped codec packet.

use aptx_adaptive::{
    Config, Core, DecoderLevel, Encoder, Error, Profile, Row, Stream, packet_budget, table_row,
};

use std::fmt::Write as _;
use std::io::Write as _;

use crate::dev::{AnyCore, Options, Replay};
use crate::drift::Drift;

/// The §7F SSRC for a profile: the whole 32-bit value selects the sink's latency mode.
pub fn mode_ssrc(profile: Profile) -> u32 {
    match profile {
        Profile::Hq => 0xA3,
        Profile::Ll => 0xA1,
    }
}

pub const RTP_HEADER_LEN: usize = 12;
/// What the phone sends (spec §7D).
const RTP_PAYLOAD_TYPE: u8 = 96;

const BYTES_PER_SAMPLE: usize = 4;

/// The raised-cosine fades (spec §7I, §7K): 4800 frames whatever the rate.
const FADE_FRAMES: u32 = 4800;

/// Gain for frame `j` of a fade-in: 0.5·(1 − cos(π·(j+1)/4801)) (§7I). A fade-out reads the
/// same table backwards.
fn fade_gain(j: u32) -> f64 {
    let x = f64::from(j + 1) / f64::from(FADE_FRAMES + 1);
    0.5 * (1.0 - (std::f64::consts::PI * x).cos())
}

/// The §7I start-of-stream mute, 20 ms, in frames.
fn start_mute_frames(rate: u32) -> u32 {
    rate / 50
}

/// The §7K switch silence: 900 input blocks of 1 ms, in frames.
fn silence_frames(rate: u32) -> u32 {
    900 * rate / 1000
}

/// Where in the silence the encoder is reset (§7K: about 200 blocks of 1 ms in), in frames.
fn reset_frames(rate: u32) -> u32 {
    200 * rate / 1000
}

/// Where the input gain is, frame by frame (§7I start of stream, §7K HQ↔LL switch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fade {
    /// Full level.
    Steady,
    /// Start of stream: frames muted so far, then a fade-in.
    StartMute(u32),
    /// Switch: the next frame of the fade-out.
    Out(u32),
    /// Switch: silent frames so far. The encoder is reset in here.
    Silence(u32),
    /// The next frame of the fade-in.
    In(u32),
}

/// A level-3 stream at 48 kHz stereo (spec §8.4, §7J: an R1 sink always runs at 48 kHz).
fn config(level: DecoderLevel, profile: Profile) -> Config {
    Config {
        profile,
        sample_rate: 48_000,
        channels: 2,
        level,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    NoSpace,
    Unsupported,
    /// A dev setting (see `dev`) that can't be used.
    BadDevSetting,
}

impl From<Error> for SessionError {
    fn from(e: Error) -> Self {
        match e {
            Error::OutputTooSmall | Error::WindowSize | Error::PacketTooSmall => Self::NoSpace,
            Error::UnsupportedConfig => Self::Unsupported,
        }
    }
}

pub struct EncodeResult {
    pub consumed: usize,
    pub written: usize,
    pub flush: bool,
}

pub struct Session<C> {
    stream: Stream<C>,
    /// DEV ONLY: an SCMS-T content protection byte between the RTP header and the payload.
    scmst: Option<u8>,
    /// What's left of the MTU after the SCMS-T byte.
    mtu: usize,
    /// One block of PCM, converted. The block stays the same size for the whole session
    /// (PipeWire reads it once per transport start), so it divides the packet of every
    /// profile we can switch to.
    pcm: Vec<i32>,
    /// Drift instrumentation (see `drift`).
    drift: Drift,
    /// Whether the drift line is logged.
    drift_log: bool,
    /// The table row for HQ and LL on this link, worked out at init so a switch only looks
    /// it up. `None` where the encoder can't run that profile here.
    rows: [Option<Row>; 2],
    /// The profile asked for; differs from the running one during a switch.
    target: Profile,
    fade: Fade,
    /// Switch requests refused since the session started (reported in the drift line).
    refused: u32,
    /// Whether `reduce_bitpool` / `increase_bitpool` change the level. Off for replay, whose
    /// packets are what they are.
    abr: bool,
    /// Whether level changes are logged (off in unit tests).
    log: bool,
    /// The level and row bitrate last logged (or started with).
    logged: (Profile, (u8, u32)),
}

/// `reduce_bitpool` never goes below this level (§7L); level 1 is treated as 2 anyway.
const MIN_LEVEL: u8 = 2;
const MAX_LEVEL: u8 = 7;

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// `profile`'s row on this link, if the encoder can run it and its packets fit the MTU.
fn link_row(level: DecoderLevel, profile: Profile, mtu: usize) -> Option<Row> {
    let row = table_row(config(level, profile), budget(mtu)).ok()?;
    (RTP_HEADER_LEN + row.packet_bytes <= mtu).then_some(row)
}

/// Every row a level change can reach from `row` (§7L): levels 2..=7 of `profile`'s table
/// index with the same packet size.
fn level_rows(level: DecoderLevel, profile: Profile, row: Row) -> impl Iterator<Item = Row> {
    (MIN_LEVEL..=MAX_LEVEL).filter_map(move |l| {
        aptx_adaptive::level_row(config(level, profile), row.packet_bytes, l)
            .ok()
            .filter(|r| r.packet_bytes == row.packet_bytes)
    })
}

/// The info line for a level change, e.g. `aptx-adaptive: level 3 -> 2 (364k -> 318k)`.
pub fn level_line(from: (u8, u32), to: (u8, u32)) -> crate::drift::LineBuf {
    let mut line = crate::drift::LineBuf::new();
    let _ = writeln!(
        line,
        "aptx-adaptive: level {} -> {} ({}k -> {}k)",
        from.0,
        to.0,
        from.1 / 1000,
        to.1 / 1000
    );
    line
}

impl Session<Encoder> {
    /// # Errors
    /// Whatever the encoder rejects, or `NoSpace` if the MTU can't fit a packet.
    #[cfg(test)]
    pub fn new(level: DecoderLevel, profile: Profile, mtu: usize) -> Result<Self, SessionError> {
        let stream = Stream::new(config(level, profile), budget(mtu))?;
        Self::with_stream(stream, mtu)
    }
}

impl Session<AnyCore> {
    /// A level-3 session on the real encoder, or on a replay core if `opts` asks for one. Starts in `profile` if this link can run it, otherwise in HQ: check
    /// [`Session::profile`].
    ///
    /// # Errors
    /// Whatever the encoder rejects, `NoSpace` if the MTU can't fit a packet, or `BadDevSetting` if the replay file can't be used (the reason
    /// goes to stderr).
    pub fn open(
        level: DecoderLevel,
        profile: Profile,
        mtu: usize,
        opts: &Options,
    ) -> Result<Self, SessionError> {
        let mtu = mtu.saturating_sub(usize::from(opts.scmst.is_some()));
        // Replay holds one profile's packets, so it never switches.
        let profile = if opts.replay.is_none() && link_row(level, profile, mtu).is_some() {
            profile
        } else {
            Profile::Hq
        };
        let config = config(level, profile);
        let row = table_row(config, budget(mtu))?;
        let core = match &opts.replay {
            None => AnyCore::Encoder(Encoder::new(config, row.bitrate)?),
            Some(path) => {
                let samples = row.interleaved_samples(config.sample_rate, config.channels.into());
                let replay = Replay::from_file(path, row.packet_bytes, samples).map_err(|e| {
                    let _ = writeln!(std::io::stderr(), "aptx-adaptive: {e}");
                    SessionError::BadDevSetting
                })?;
                AnyCore::Replay(replay)
            }
        };
        let mut s = Self::with_stream(Stream::with_core(core, config, row), mtu)?;
        if opts.replay.is_some() {
            // Whole packets per block, as before, and no switching.
            s.pcm = vec![0; s.stream.samples_per_packet()];
            s.rows[Profile::Ll.index()] = None;
            // The replayed packets are what they are.
            s.fade = Fade::Steady;
            s.abr = false;
        }
        s.scmst = opts.scmst;
        s.drift_log = opts.drift_log;
        Ok(s)
    }
}

/// [`packet_budget`] leaves room for 8 bytes in front of the packet; the RTP header is 12.
fn budget(mtu: usize) -> usize {
    packet_budget(mtu.saturating_sub(RTP_HEADER_LEN - 8))
}

impl<C: Core> Session<C> {
    /// # Errors
    /// `NoSpace` if a packet and its headers don't fit in the MTU. The packet budget never goes
    /// below 128 bytes, so a small enough MTU can't be met.
    pub fn with_stream(stream: Stream<C>, mtu: usize) -> Result<Self, SessionError> {
        if RTP_HEADER_LEN + stream.row().packet_bytes > mtu {
            return Err(SessionError::NoSpace);
        }
        let c = stream.config();
        let mut rows = Profile::ALL.map(|p| link_row(c.level, p, mtu));
        rows[c.profile.index()] = Some(stream.row());
        // Every period a level change can reach too, in either profile.
        let block = Profile::ALL
            .iter()
            .zip(rows)
            .filter_map(|(&p, r)| r.map(|r| (p, r)))
            .flat_map(|(p, r)| std::iter::once(r).chain(level_rows(c.level, p, r)))
            .map(|r| r.interleaved_samples(c.sample_rate, c.channels.into()))
            .fold(0, gcd);
        let (stream_level, stream_bitrate) = (stream.level(), stream.row().bitrate);
        Ok(Self {
            pcm: vec![0; block],
            stream,
            scmst: None,
            mtu,
            drift: Drift::new(c.sample_rate),
            drift_log: true,
            rows,
            target: c.profile,
            fade: Fade::StartMute(0),
            refused: 0,
            abr: true,
            log: !cfg!(test),
            logged: (c.profile, (stream_level, stream_bitrate)),
        })
    }

    /// The quality level running now (§7L).
    #[cfg(test)]
    pub fn level(&self) -> u8 {
        self.stream.level()
    }

    /// `reduce_bitpool` (`down`) or `increase_bitpool`: asks for the next level down (floor 2)
    /// or up whose row has a different bitrate, from the next packet (§7L). Levels sharing a
    /// row are stepped over (R1 HQ 4–7), so the ceiling is the last level with a new row: 4
    /// on R1 HQ. A step onto a row with the same bitrate but another period (R2 LL index 2,
    /// 3 → 4) is never taken. In R1 LL every level is the same row, so nothing changes.
    /// Steps from a pending request, so repeated calls between packets keep stepping. During
    /// an HQ↔LL switch the request waits and the switch's reset to level 3 drops it.
    ///
    /// Returns the level that will be running, like SBC's bitpool. Never allocates.
    ///
    /// # Errors
    /// `Unsupported` for a replay session, which can't change level.
    pub fn step_level(&mut self, down: bool) -> Result<u8, SessionError> {
        if !self.abr {
            return Err(SessionError::Unsupported);
        }
        let from = self
            .stream
            .pending_level()
            .unwrap_or_else(|| self.stream.level());
        let Some(cur) = self.stream.level_row(from) else {
            return Ok(from);
        };
        let mut next = |l: u8| -> Option<Option<u8>> {
            let row = self.stream.level_row(l)?;
            if row.bitrate != cur.bitrate {
                Some(Some(l))
            } else if row.period_ms != cur.period_ms {
                // §7L: equal bitrate, other period: P wouldn't follow the period byte.
                Some(None)
            } else {
                None
            }
        };
        let found = if down {
            (MIN_LEVEL..from).rev().find_map(&mut next)
        } else {
            (from + 1..=MAX_LEVEL).find_map(&mut next)
        };
        let Some(l) = found.flatten().filter(|&l| self.stream.request_level(l)) else {
            return Ok(from);
        };
        // media-sink calls this between packets, so the next packet already uses the new row
        // (§7L "next packet boundary").
        self.stream.hold_level(self.target != self.profile());
        if self.stream.apply_level_now() {
            self.log_level();
        }
        Ok(l)
    }

    /// Whether this link can run `profile`: the encoder has a row for it (R1 LL is config 0,
    /// R2 LL config 4) and its packet fits the MTU. Replay sessions only run HQ.
    pub fn can_run(&self, profile: Profile) -> bool {
        self.rows[profile.index()].is_some()
    }

    /// The profile being switched to, or the running one when no switch is in progress.
    pub fn target(&self) -> Profile {
        self.target
    }

    /// Asks for `profile` (spec §7K). A different profile starts the switch: a 4800-frame
    /// fade-out, 900 ms of silence with the encoder reset in place about 200 ms in (new row,
    /// packet size, period and SSRC from the next packet), then a 4800-frame fade-in.
    /// Packets keep flowing throughout. Asking for the running profile mid-switch reverses it.
    /// Never allocates; call it between packets, as `update_props` is.
    ///
    /// # Errors
    /// `Unsupported` if this link can't run `profile` (a replay core, an MTU too small). The request is counted and nothing changes.
    pub fn request(&mut self, profile: Profile) -> Result<(), SessionError> {
        if !self.can_run(profile) {
            self.refused = self.refused.saturating_add(1);
            return Err(SessionError::Unsupported);
        }
        self.target = profile;
        let switching = profile != self.profile();
        self.fade = match (self.fade, switching) {
            (Fade::Steady, true) => Fade::Out(0),
            (Fade::StartMute(_), true) => Fade::Silence(0),
            // Out(j) plays table entry 4799 − j and In(k) plays entry k: reversing keeps the
            // gain where it is.
            (Fade::Out(j), false) => Fade::In(FADE_FRAMES - 1 - j),
            (Fade::In(k), true) => Fade::Out(FADE_FRAMES - 1 - k),
            (Fade::Silence(_), false) => Fade::In(0),
            (f, _) => f,
        };
        Ok(())
    }

    /// Applies the fade to the block just converted, a frame at a time.
    fn apply_fade(&mut self) {
        if self.fade == Fade::Steady {
            return;
        }
        let rate = self.stream.config().sample_rate;
        let channels = usize::from(self.stream.config().channels);
        let switching = self.target != self.profile();
        for frame in self.pcm.chunks_exact_mut(channels) {
            let (gain, next) = match self.fade {
                Fade::Steady => (1.0, Fade::Steady),
                Fade::StartMute(n) if n + 1 >= start_mute_frames(rate) => (0.0, Fade::In(0)),
                Fade::StartMute(n) => (0.0, Fade::StartMute(n + 1)),
                Fade::Out(j) if j + 1 >= FADE_FRAMES => (fade_gain(0), Fade::Silence(0)),
                Fade::Out(j) => (fade_gain(FADE_FRAMES - 1 - j), Fade::Out(j + 1)),
                Fade::Silence(n) if n + 1 >= silence_frames(rate) && !switching => {
                    (0.0, Fade::In(0))
                }
                Fade::Silence(n) => (0.0, Fade::Silence(n.saturating_add(1))),
                Fade::In(k) if k + 1 >= FADE_FRAMES => (fade_gain(k), Fade::Steady),
                Fade::In(k) => (fade_gain(k), Fade::In(k + 1)),
            };
            self.fade = next;
            for s in frame {
                // Truncated toward zero (§7I).
                *s = (f64::from(*s) * gain) as i32;
            }
        }
    }

    /// Before a packet: the in-place reset, once far enough into the silence (§7K). Level 3,
    /// LL0 and zeroed loop counters come with the new stream state.
    fn maybe_reset(&mut self) {
        let rate = self.stream.config().sample_rate;
        let Fade::Silence(n) = self.fade else {
            return;
        };
        if n < reset_frames(rate) || self.target == self.profile() {
            return;
        }
        let level = self.stream.config().level;
        let row = self.rows[self.target.index()];
        let ok = row.is_some_and(|row| self.stream.reset(config(level, self.target), row).is_ok());
        if !ok {
            self.refused = self.refused.saturating_add(1);
            self.target = self.profile();
        }
    }

    /// Logs a level change at info: one write(2), no allocation, like the drift line. An
    /// HQ↔LL switch (which resets the level to 3) isn't a level change and isn't logged here.
    fn log_level(&mut self) {
        let now = (self.stream.level(), self.stream.row().bitrate);
        if now == self.logged.1 {
            return;
        }
        if self.log && self.logged.0 == self.profile() {
            let _ = std::io::stderr().write(level_line(self.logged.1, now).as_bytes());
        }
        self.logged = (self.profile(), now);
    }

    /// Switch requests refused so far.
    #[cfg(test)]
    pub fn refused(&self) -> u32 {
        self.refused
    }

    /// One block of interleaved `S24_32` PCM: the largest that divides the packet of every
    /// profile the link can run (2 ms, 96 frames, for HQ 14 ms and LL 10 ms). PipeWire only
    /// reads this when the transport starts, so it can't follow a profile switch; a block
    /// that divides both keeps `media-sink.c`'s packet timing (blocks × block size) right
    /// after one.
    pub fn block_size(&self) -> usize {
        self.pcm.len() * BYTES_PER_SAMPLE
    }

    pub fn profile(&self) -> Profile {
        self.stream.config().profile
    }

    fn packet_frames(&self) -> u64 {
        self.stream.samples_per_packet() as u64 / u64::from(self.stream.config().channels)
    }

    /// §7F/§7H: `adjust << 16 | samples per channel in the packet`. A source sends adjust 0;
    /// the sink corrects drift itself.
    fn rtp_timestamp(&self) -> u32 {
        u32::from(u16::try_from(self.packet_frames()).unwrap_or(u16::MAX))
    }

    pub fn interval_ns(&self) -> u64 {
        u64::from(self.stream.row().period_ms) * 1_000_000
    }

    /// The filterbank delay only (§7I): `media-sink.c` adds the packet wait (`packet_delay_ns`)
    /// and the transport delay itself.
    pub fn delay_samples(&self) -> u32 {
        aptx_adaptive::delay_frames(self.stream.config())
            .ok()
            .flatten()
            .unwrap_or(0)
    }

    /// The per-packet bitrate rule lives in the stream loop (spec §5 step 7), and level
    /// changes come from `reduce_bitpool` / `increase_bitpool` ([`Session::step_level`]), so
    /// the backlog here is only logged.
    /// `media-sink.c` calls this right before each `send()`, which makes it the drift probe.
    pub fn abr_process(&mut self, unsent: usize) {
        let frames = self.packet_frames();
        self.drift.on_send_log(
            std::time::Instant::now(),
            unsent,
            frames,
            self.drift_log,
            self.refused,
            self.stream.level(),
        );
    }

    /// Writes the RTP header (and the dev SCMS-T byte) and returns its length. PipeWire's
    /// sample-count `_timestamp` isn't used: §7F defines its own.
    ///
    /// # Errors
    /// `NoSpace` if `dst` can't hold the header.
    pub fn start_encode(
        &mut self,
        dst: &mut [u8],
        seqnum: u16,
        _timestamp: u32,
    ) -> Result<usize, SessionError> {
        // Between packets, after media-sink has sent the last one (and fed `abr_process` its
        // frame count), so the reset's new row starts with this packet's header.
        self.maybe_reset();
        let cp = usize::from(self.scmst.is_some());
        let h = dst
            .get_mut(..RTP_HEADER_LEN + cp)
            .ok_or(SessionError::NoSpace)?;
        h[0] = 2 << 6;
        h[1] = RTP_PAYLOAD_TYPE;
        h[2..4].copy_from_slice(&seqnum.to_be_bytes());
        h[4..8].copy_from_slice(&self.rtp_timestamp().to_be_bytes());
        // Changes exactly when the encoder switches (§7H).
        h[8..12].copy_from_slice(&mode_ssrc(self.profile()).to_be_bytes());
        if let Some(b) = self.scmst {
            h[RTP_HEADER_LEN] = b;
        }
        Ok(RTP_HEADER_LEN + cp)
    }

    /// Takes one block of interleaved native-endian `S24_32` PCM and writes a packet to `dst`
    /// when the loop produces one. Each packet is its own A2DP packet.
    ///
    /// # Errors
    /// Encoder failures, or `NoSpace` if `dst` can't hold a packet.
    pub fn encode(&mut self, src: &[u8], dst: &mut [u8]) -> Result<EncodeResult, SessionError> {
        let block = self.block_size();
        if src.len() < block {
            return Ok(EncodeResult {
                consumed: 0,
                written: 0,
                flush: false,
            });
        }
        let room = self.mtu.saturating_sub(RTP_HEADER_LEN).min(dst.len());
        let (samples, _) = src[..block].as_chunks::<4>();
        for (s, b) in self.pcm.iter_mut().zip(samples) {
            // S24_32 leaves the top byte unspecified; the encoder wants it sign-extended.
            *s = (i32::from_ne_bytes(*b) << 8) >> 8;
        }
        self.apply_fade();
        // §7L: a level request waits while an HQ↔LL switch is in progress.
        self.stream.hold_level(self.target != self.profile());
        let written = self.stream.push(&self.pcm, &mut dst[..room])?;
        if written.is_some() {
            self.log_level();
        }
        Ok(EncodeResult {
            consumed: block,
            written: written.unwrap_or(0),
            flush: written.is_some(),
        })
    }
}

#[cfg(test)]
mod tests {
    use aptx_adaptive::fake::{self, Fake};

    use super::*;

    const MTU: usize = 895;

    /// One packet's worth of `S24_32` PCM.
    fn packet_size<C: Core>(s: &Session<C>) -> usize {
        s.stream.samples_per_packet() * BYTES_PER_SAMPLE
    }

    /// Feeds `src` a block at a time, like `media-sink.c`, until a packet comes out. Returns
    /// the bytes consumed in all and the packet.
    fn encode_packet<C: Core>(
        s: &mut Session<C>,
        src: &[u8],
        dst: &mut [u8],
    ) -> Result<EncodeResult, SessionError> {
        let mut consumed = 0;
        for block in src.chunks_exact(s.block_size()) {
            let r = s.encode(block, dst)?;
            consumed += r.consumed;
            if r.flush {
                return Ok(EncodeResult { consumed, ..r });
            }
        }
        Ok(EncodeResult {
            consumed,
            written: 0,
            flush: false,
        })
    }

    fn session() -> Session<Fake> {
        session_with(MTU, budget(MTU))
    }

    /// A fake session without the start-of-stream fade, so packets carry the input as is.
    fn session_with(mtu: usize, budget: usize) -> Session<Fake> {
        let stream = fake::stream(config(DecoderLevel::R1, Profile::Hq), budget).unwrap();
        let mut s = Session::with_stream(stream, mtu).unwrap();
        s.fade = Fade::Steady;
        s
    }

    /// Sends `n` packets and returns their RTP headers.
    fn headers(s: &mut Session<Fake>, n: usize) -> Vec<[u8; 12]> {
        let src = vec![0u8; packet_size(s)];
        let mut buf = vec![0u8; 2048];
        (0..n)
            .map(|i| {
                let len = s.start_encode(&mut buf, i as u16, 0xdead_beef).unwrap();
                let r = encode_packet(s, &src, &mut buf[len..]).unwrap();
                assert!(r.flush);
                buf[..12].try_into().unwrap()
            })
            .collect()
    }

    fn timestamp(h: &[u8; 12]) -> u32 {
        u32::from_be_bytes(h[4..8].try_into().unwrap())
    }

    /// Spec §7F: `80 60`, PipeWire's sequence, timestamp `00 00 02 a0` (672 samples, adjust
    /// 0) on every packet, SSRC `00 00 00 a3`, then the swapped codec packet with nothing in
    /// front.
    #[test]
    fn phone_packets() {
        let mut s = session();
        let mut src = vec![0u8; packet_size(&s)];
        src[..4].copy_from_slice(&5i32.to_ne_bytes());
        src[4..8].copy_from_slice(&6i32.to_ne_bytes());
        let mut buf = vec![0xffu8; 2048];
        for i in 0..5u16 {
            let len = s.start_encode(&mut buf, i + 1, 0xdead_beef).unwrap();
            assert_eq!(len, 12);
            let r = encode_packet(&mut s, &src, &mut buf[len..]).unwrap();
            assert_eq!((r.written, r.flush), (656, true));
            assert_eq!(
                buf[..12],
                [0x80, 0x60, 0, i as u8 + 1, 0, 0, 0x02, 0xa0, 0, 0, 0, 0xa3]
            );
            assert_eq!(buf[12..15], [6, 5, 0]);
        }
    }

    /// The §7F timestamp is the packet's own sample count per channel, not a fixed 672.
    #[test]
    fn samples_per_packet_follows_packet() {
        for budget in [MTU, 600, 400] {
            let mut s = session_with(MTU, budget);
            let frames = s.stream.samples_per_packet() as u32 / 2;
            let hs = headers(&mut s, 3);
            for h in &hs {
                assert_eq!(timestamp(h), frames);
            }
        }
    }

    /// The layout and length of the phone capture's first packet (spec §7D) with the §7F
    /// fields: 668 bytes, `80 60 00 01 00 00 02 a0 00 00 00 a3 81 00 d0 a1 …`.
    #[test]
    fn matches_phone_capture_first_packet() {
        let opts = Options {
            replay: Some(vector("sine1k.r1.bin")),
            ..Options::default()
        };
        let mut s = Session::open(DecoderLevel::R1, Profile::Hq, MTU, &opts).unwrap();
        let src = vec![0u8; packet_size(&s)];
        let mut buf = vec![0u8; 2048];
        let len = s.start_encode(&mut buf, 1, 0).unwrap();
        let r = encode_packet(&mut s, &src, &mut buf[len..]).unwrap();
        assert!(r.flush);
        let p = &buf[..len + r.written];
        assert_eq!(p.len(), 668);
        assert_eq!(
            p[..12],
            [0x80, 0x60, 0x00, 0x01, 0, 0, 0x02, 0xa0, 0, 0, 0, 0xa3]
        );
        assert_eq!(
            (p[12], p[13], p[14] & 0xf0, p[15]),
            (0x81, 0x00, 0xd0, 0xa1)
        );
    }

    /// The SCMS-T byte goes right after the RTP header and comes out of the MTU budget.
    #[test]
    fn scmst_byte_after_rtp_header() {
        let path = vector("sine1k.r1.bin");
        let opts = Options {
            replay: Some(path),
            scmst: Some(0x02),
            ..Options::default()
        };
        let mut s = Session::open(DecoderLevel::R1, Profile::Hq, MTU, &opts).unwrap();
        let src = vec![0u8; packet_size(&s)];
        let mut buf = vec![0xffu8; 2048];
        let len = s.start_encode(&mut buf, 1, 0).unwrap();
        assert_eq!(len, 13);
        assert_eq!(buf[..2], [0x80, 0x60]);
        assert_eq!(buf[12], 0x02);
        let r = encode_packet(&mut s, &src, &mut buf[len..]).unwrap();
        assert_eq!(r.written, 656);
        assert_eq!((buf[len], buf[len + 1], buf[len + 3]), (0x81, 0x00, 0xa1));
        assert_eq!(
            s.start_encode(&mut [0; 12], 0, 0),
            Err(SessionError::NoSpace)
        );
        // At an MTU that just fits, the byte shrinks the packet.
        let opts = |scmst| Options {
            scmst,
            ..Options::default()
        };
        let row = |o: &Options| {
            Session::open(DecoderLevel::R1, Profile::Hq, 600, o)
                .unwrap()
                .stream
                .row()
                .packet_bytes
        };
        assert_eq!(row(&opts(None)), 588);
        let smaller = row(&opts(Some(0)));
        assert!(smaller < 588);
        let mut s = Session::open(DecoderLevel::R1, Profile::Hq, 600, &opts(Some(0))).unwrap();
        let src = vec![0u8; packet_size(&s)];
        let mut buf = vec![0u8; 2048];
        let len = s.start_encode(&mut buf, 0, 0).unwrap();
        let r = encode_packet(&mut s, &src, &mut buf[len..]).unwrap();
        assert_eq!(len + r.written, 13 + smaller);
        assert!(len + r.written <= 600);
    }

    #[test]
    fn mode_ssrcs() {
        assert_eq!(mode_ssrc(Profile::Hq), 0x0000_00a3);
        assert_eq!(mode_ssrc(Profile::Ll), 0x0000_00a1);
    }

    #[test]
    fn start_encode_needs_room() {
        let mut s = session();
        assert_eq!(
            s.start_encode(&mut [0; 11], 0, 0),
            Err(SessionError::NoSpace)
        );
    }

    #[test]
    fn timing() {
        let s = session();
        // 2 ms: divides both the 14 ms HQ and the 10 ms LL packet.
        assert_eq!(s.block_size(), 96 * 2 * 4);
        assert_eq!(packet_size(&s), 1344 * 4);
        assert_eq!(s.interval_ns(), 14_000_000);
        // Config 0's filterbank (§7I).
        assert_eq!(s.delay_samples(), 90);
    }

    #[test]
    fn one_block_one_swapped_packet() {
        let mut s = session();
        let mut src = vec![0u8; packet_size(&s) + 8];
        src[..4].copy_from_slice(&5i32.to_ne_bytes());
        src[4..8].copy_from_slice(&0x00ff_ff06_i32.to_ne_bytes());
        let mut dst = [0xffu8; 1000];
        let r = encode_packet(&mut s, &src, &mut dst).unwrap();
        assert_eq!((r.consumed, r.written, r.flush), (5376, 656, true));
        assert_eq!(dst[..3], [6, 5, 0]);
        assert_eq!(dst[656], 0xff);
    }

    #[test]
    fn short_input_consumes_nothing() {
        let mut s = session();
        let r = s.encode(&[0; 8], &mut [0; 1000]).unwrap();
        assert_eq!((r.consumed, r.flush), (0, false));
    }

    #[test]
    fn mtu_too_small_fails_at_init() {
        let stream = fake::stream(config(DecoderLevel::R1, Profile::Hq), 2008).unwrap();
        assert!(matches!(
            Session::with_stream(stream, 600),
            Err(SessionError::NoSpace)
        ));
        // The budget floor is 128 bytes, which a tiny MTU can't hold.
        assert!(matches!(
            Session::new(DecoderLevel::R1, Profile::Hq, 100),
            Err(SessionError::NoSpace)
        ));
        // Nor can a packet whose MTU-fitted bitrate is below the encoder's lowest (§8.8).
        let need =
            aptx_adaptive::min_packet_bytes(config(DecoderLevel::R1, Profile::Hq), 14).unwrap();
        let mtu = need + RTP_HEADER_LEN;
        assert!(matches!(
            Session::new(DecoderLevel::R1, Profile::Hq, mtu - 4),
            Err(SessionError::NoSpace)
        ));
        assert_eq!(
            Session::new(DecoderLevel::R1, Profile::Hq, mtu)
                .unwrap()
                .stream
                .row()
                .packet_bytes,
            need
        );
    }

    /// 1 s through the codec the way `media-sink.c` drives it: `start_encode` into a fresh
    /// buffer, `encode` whole blocks until `need_flush`, send, repeat.
    #[test]
    fn one_second_like_media_sink() {
        let mut s = session();
        let header = RTP_HEADER_LEN;
        let frames = 48_000;
        let pcm: Vec<u8> = (0..frames * 2)
            .flat_map(|i: i32| (i / 2).to_ne_bytes())
            .collect();
        let block = s.block_size();
        let mut buffer = vec![0u8; 8192];
        let mut sent = Vec::new();
        let mut used = s.start_encode(&mut buffer, 0, 0).unwrap();
        for src in pcm.chunks_exact(block) {
            let r = s.encode(src, &mut buffer[used..]).unwrap();
            assert_eq!(r.consumed, block);
            used += r.written;
            if r.flush {
                sent.push(buffer[..used].to_vec());
                used = s.start_encode(&mut buffer, sent.len() as u16, 0).unwrap();
            }
        }
        assert_eq!(sent.len(), 71);
        for (i, p) in sent.iter().enumerate() {
            assert_eq!(p.len(), header + 656);
            assert_eq!(p[2..4], (i as u16).to_be_bytes());
            assert!(p.len() <= MTU);
            // The fake writes the low byte of each packet's first sample pair, swapped.
            let first = (i * 672) as u8;
            assert_eq!(p[header..header + 2], [first, first]);
        }
    }

    /// Drives `s` like `media-sink.c` over `pcm` (interleaved `S24_32`): `start_encode`, then
    /// whole blocks until `need_flush`, send, repeat. `before_packet` runs where media-sink
    /// calls `update_props`: before each `start_encode`. Returns every packet sent.
    fn media_sink<C: Core>(
        s: &mut Session<C>,
        pcm: &[u8],
        mut before_packet: impl FnMut(&mut Session<C>, usize),
    ) -> Vec<Vec<u8>> {
        let block = s.block_size();
        let mut buffer = vec![0u8; 8192];
        let mut sent = Vec::new();
        before_packet(s, 0);
        let mut used = s.start_encode(&mut buffer, 0, 0).unwrap();
        for src in pcm.chunks_exact(block) {
            let r = s.encode(src, &mut buffer[used..]).unwrap();
            assert_eq!(r.consumed, block);
            used += r.written;
            if r.flush {
                sent.push(buffer[..used].to_vec());
                before_packet(s, sent.len());
                used = s.start_encode(&mut buffer, sent.len() as u16, 0).unwrap();
            }
        }
        sent
    }

    fn sine_pcm(frames: usize) -> Vec<u8> {
        (0..frames)
            .flat_map(|n| {
                let v =
                    (2.0 * std::f64::consts::PI * 1000.0 * f64::from(n as u32) / 48_000.0).sin();
                let s = (v * 4_000_000.0) as i32;
                [s, s]
            })
            .flat_map(i32::to_ne_bytes)
            .collect()
    }

    /// Spec §7H/§7J for an R1 sink in LL: 1 s of 48 kHz on the real encoder is 100 packets,
    /// every 10 ms, each `80 60 sq sq 00 00 01 e0 00 00 00 a1` and 348 swapped config 0
    /// (HQ bitstream) bytes.
    #[test]
    fn ll_one_second_real_encoder() {
        let mut s = Session::new(DecoderLevel::R1, Profile::Ll, MTU).unwrap();
        assert_eq!(s.profile(), Profile::Ll);
        assert_eq!(s.interval_ns(), 10_000_000);
        assert_eq!(packet_size(&s), 480 * 2 * 4);
        assert_eq!(s.block_size(), 96 * 2 * 4);
        let sent = media_sink(&mut s, &sine_pcm(48_000), |_, _| {});
        assert_eq!(sent.len(), 100);
        for (i, p) in sent.iter().enumerate() {
            assert_eq!(p.len(), 12 + 348, "packet {i}");
            let seq = (i as u16).to_be_bytes();
            assert_eq!(
                p[..12],
                [0x80, 0x60, seq[0], seq[1], 0, 0, 0x01, 0xe0, 0, 0, 0, 0xa1]
            );
            // Swapped `00 01 54 ..` (0x81 if padded): frame type 1, config 0.
            assert_eq!((p[12] & 0x7f, p[13], p[15]), (0x01, 0x00, 84));
        }
    }

    #[test]
    fn ll_fits_the_mtu() {
        // 12 + 348 fits a small MTU that HQ's 656 doesn't.
        let s = Session::new(DecoderLevel::R1, Profile::Ll, 360).unwrap();
        assert_eq!(s.stream.row().packet_bytes, 348);
        // R2 LL is config 4 on LL row 1 (270 kbps / 10 ms / 348 bytes, codec-core §10.F).
        let s = Session::new(DecoderLevel::R2, Profile::Hq, MTU).unwrap();
        assert!(s.can_run(Profile::Ll));
        // 1 ms blocks: the gcd of the 19 ms R2 HQ and 10 ms LL packets.
        assert_eq!(s.block_size(), 48 * 2 * 4);
    }

    /// One block of constant-level PCM, `S24_32`.
    fn level_block<C: Core>(s: &Session<C>, v: i32) -> Vec<u8> {
        std::iter::repeat_n(v, s.block_size() / 4)
            .flat_map(i32::to_ne_bytes)
            .collect()
    }

    /// Feeds `blocks` constant blocks and records, per block, the gain on its first frame
    /// (×1e6) and whether a packet came out.
    fn gains<C: Core>(s: &mut Session<C>, blocks: usize) -> Vec<i32> {
        let src = level_block(s, 1_000_000);
        let mut dst = vec![0u8; 2048];
        (0..blocks)
            .map(|_| {
                s.encode(&src, &mut dst).unwrap();
                s.pcm[0]
            })
            .collect()
    }

    /// §7I: at creation, a 20 ms mute, then the 4800-frame raised cosine
    /// 0.5·(1 − cos(π(j+1)/4801)), truncated toward zero.
    #[test]
    fn start_of_stream_mute_and_fade() {
        let stream = fake::stream(config(DecoderLevel::R1, Profile::Hq), 887).unwrap();
        let mut s = Session::with_stream(stream, MTU).unwrap();
        assert_eq!(s.fade, Fade::StartMute(0));
        // 96-frame blocks: 20 ms is 960 frames, 10 blocks.
        let g = gains(&mut s, 61);
        assert!(g[..10].iter().all(|&v| v == 0), "{g:?}");
        // fade_gain(0) is about 1e-7, so the first fade-in frame still truncates to 0.
        assert_eq!(g[10], (1_000_000.0 * fade_gain(0)) as i32);
        assert_eq!(g[11], (1_000_000.0 * fade_gain(96)) as i32);
        assert!(g[11] > 0);
        assert!(g.windows(2).all(|w| w[1] >= w[0]));
        // 960 + 4800 frames = 60 blocks.
        assert!(g[59] < 1_000_000);
        assert_eq!(s.fade, Fade::Steady);
        assert_eq!(g[60], 1_000_000);
        // The curve stops just short of 1 (π·4800/4801); after it the gain is exactly 1.
        assert!(fade_gain(FADE_FRAMES - 1) > 0.999_999 && fade_gain(FADE_FRAMES - 1) < 1.0);
    }

    /// §7K: HQ → LL is a 4800-frame fade-out, 900 ms of silence with an in-place reset about
    /// 200 ms in, then a 4800-frame fade-in. Packets flow throughout; the SSRC, timestamp and
    /// packet size change together at the reset.
    #[test]
    fn switch_sequence_timing() {
        let mut s = session();
        let src = level_block(&s, 1_000_000);
        let mut dst = vec![0u8; 2048];
        let mut hdr = [0u8; 64];
        assert_eq!(s.request(Profile::Ll), Ok(()));
        assert_eq!((s.profile(), s.target()), (Profile::Hq, Profile::Ll));
        // (block, gain, packet length, SSRC low byte) for every block.
        let mut log = Vec::new();
        for block in 0..600 {
            let r = s.encode(&src, &mut dst).unwrap();
            let mode = if r.flush {
                s.start_encode(&mut hdr, 0, 0).unwrap();
                hdr[11]
            } else {
                0
            };
            log.push((block, s.pcm[0], r.written, mode, s.profile()));
        }
        // Fade-out: 4800 frames = 50 blocks, falling to 0.
        assert_eq!(log[0].1, (1_000_000.0 * fade_gain(4799)) as i32);
        assert!(log[..50].windows(2).all(|w| w[1].1 <= w[0].1));
        // Block 49 starts at fade-out frame 4704, so table entry 4799 − 4704.
        assert_eq!(log[49].1, (1_000_000.0 * fade_gain(95)) as i32);
        // Silence: 900 ms = 43200 frames = 450 blocks.
        assert!(log[50..500].iter().all(|l| l.1 == 0));
        // Fade-in over the next 50 blocks, then full level.
        assert_eq!(log[500].1, (1_000_000.0 * fade_gain(0)) as i32);
        assert_eq!(log[501].1, (1_000_000.0 * fade_gain(96)) as i32);
        assert!(log[549].1 < 1_000_000);
        assert_eq!(log[550].1, 1_000_000);
        // The reset: the first packet ≥ 200 ms into the silence ends the HQ stretch.
        let reset = log.iter().position(|l| l.4 == Profile::Ll).unwrap();
        let silence_start = 50;
        let blocks_in = reset - silence_start;
        // 200 ms = 100 blocks; the reset waits for the next packet (≤ 7 blocks of HQ).
        assert!(
            (100..=107).contains(&blocks_in),
            "reset {blocks_in} blocks in"
        );
        assert!(log[..reset].iter().all(|l| l.2 == 0 || l.2 == 656));
        assert!(log[reset + 1..].iter().all(|l| l.2 == 0 || l.2 == 348));
        let ssrcs: Vec<u8> = log.iter().filter(|l| l.2 != 0).map(|l| l.3).collect();
        let switch = ssrcs.iter().position(|&b| b == 0xa1).unwrap();
        assert!(ssrcs[..switch].iter().all(|&b| b == 0xa3));
        assert!(ssrcs[switch..].iter().all(|&b| b == 0xa1));
        assert_eq!(s.interval_ns(), 10_000_000);
        assert_eq!(s.stream.core().resets, 1);
        // Packets kept flowing: one per 14 ms, then one per 10 ms, never more than a period
        // apart.
        let packets: Vec<usize> = log.iter().filter(|l| l.2 != 0).map(|l| l.0).collect();
        assert!(packets.windows(2).all(|w| w[1] - w[0] <= 7));
    }

    /// Asking for the running profile before the reset reverses the switch without one.
    #[test]
    fn switch_reverses_before_reset() {
        let mut s = session();
        s.request(Profile::Ll).unwrap();
        gains(&mut s, 20);
        assert!(matches!(s.fade, Fade::Out(_)));
        s.request(Profile::Hq).unwrap();
        assert!(matches!(s.fade, Fade::In(_)));
        let g = gains(&mut s, 60);
        assert_eq!(g[59], 1_000_000);
        assert_eq!((s.profile(), s.stream.core().resets), (Profile::Hq, 0));
        // In the silence, it goes straight to the fade-in.
        s.request(Profile::Ll).unwrap();
        gains(&mut s, 60);
        s.request(Profile::Hq).unwrap();
        assert_eq!(s.fade, Fade::In(0));
    }

    /// Reversing mid-fade keeps the gain continuous: the next frame's gain is one table step
    /// from the last.
    #[test]
    fn reversal_keeps_gain_continuous() {
        let mut s = session();
        s.request(Profile::Ll).unwrap();
        let before = *gains(&mut s, 20).last().unwrap();
        s.request(Profile::Hq).unwrap();
        let after = gains(&mut s, 1)[0];
        // After 20 blocks the fade-out would play entry 4799 − 1920 next; the reversed fade
        // plays exactly that, then climbs.
        assert!(after < before);
        assert_eq!(
            after,
            (1_000_000.0 * fade_gain(FADE_FRAMES - 1 - 20 * 96)) as i32
        );
        let next = gains(&mut s, 1)[0];
        assert!(next > after);
    }

    /// No allocation or free on the data thread for a whole HQ → LL switch on the real
    /// encoder: `request` (what `update_props` calls) and every `start_encode`/`encode`
    /// through the fade-out, the silence, the in-place reset and the fade-in.
    #[test]
    fn switch_path_does_not_allocate() {
        let mut s = Session::new(DecoderLevel::R1, Profile::Hq, MTU).unwrap();
        let src: Vec<u8> = sine_pcm(96);
        let mut buf = vec![0u8; 2048];
        // Settle past the start fade first.
        for _ in 0..100 {
            s.encode(&src, &mut buf).unwrap();
        }
        let count = crate::alloc_count::count(|| {
            s.request(Profile::Ll).unwrap();
            let mut used = s.start_encode(&mut buf, 0, 0).unwrap();
            for seq in 0..700u16 {
                let r = s.encode(&src, &mut buf[used..]).unwrap();
                used += r.written;
                if r.flush {
                    used = s.start_encode(&mut buf, seq, 0).unwrap();
                }
            }
        });
        assert_eq!(count, 0);
        assert_eq!((s.profile(), s.fade), (Profile::Ll, Fade::Steady));
    }

    /// What one profile's packets look like: codec packet size, RTP header bytes 4..12
    /// (timestamp and SSRC) and frame type.
    struct Expect {
        size: usize,
        fields: [u8; 8],
        frame_type: u8,
    }

    /// Runs 4 s of a sine through media-sink's loop on the real encoder, asking for LL from
    /// packet 10 to 120 and HQ otherwise, and checks the packets: each is exactly an HQ or an
    /// LL packet (size, RTP timestamp and SSRC, frame type), sequence numbers run on, the
    /// block size never changes, and the stream goes HQ, LL, HQ with no mixing.
    fn check_hq_ll_hq(level: DecoderLevel, hq: &Expect, ll: &Expect) {
        let mut s = Session::new(level, Profile::Hq, MTU).unwrap();
        let block = s.block_size();
        let sent = media_sink(&mut s, &sine_pcm(4 * 48_000), |s, n| {
            let want = if (10..120).contains(&n) {
                Profile::Ll
            } else {
                Profile::Hq
            };
            s.request(want).unwrap();
            assert_eq!(s.block_size(), block);
        });
        let mut runs = String::new();
        for (i, p) in sent.iter().enumerate() {
            let is = |e: &Expect| {
                p.len() == RTP_HEADER_LEN + e.size
                    && p[4..12] == e.fields
                    && p[12] & 0x7f == e.frame_type
            };
            let kind = if is(hq) {
                'H'
            } else if is(ll) {
                'L'
            } else {
                panic!("{level:?} packet {i}: {:02x?}", &p[..14])
            };
            assert_eq!(p[2..4], (i as u16).to_be_bytes());
            if !runs.ends_with(kind) {
                runs.push(kind);
            }
        }
        assert_eq!(runs, "HLH", "{level:?}");
    }

    /// R1: config 0 both ways (§7J), 14 ms / 656 bytes / A3 and 10 ms / 348 bytes / A1.
    #[test]
    fn switch_hq_ll_hq_real_encoder() {
        check_hq_ll_hq(
            DecoderLevel::R1,
            &Expect {
                size: 656,
                fields: [0, 0, 0x02, 0xa0, 0, 0, 0, 0xa3],
                frame_type: 1,
            },
            &Expect {
                size: 348,
                fields: [0, 0, 0x01, 0xe0, 0, 0, 0, 0xa1],
                frame_type: 1,
            },
        );
    }

    /// R2: config 3 (19 ms, 656 bytes, A3) and config 4 (10 ms, 348 bytes, A1), switched in
    /// place with the §7K sequence.
    #[test]
    fn switch_r2_hq_ll_hq_real_encoder() {
        check_hq_ll_hq(
            DecoderLevel::R2,
            &Expect {
                size: 656,
                fields: [0, 0, 0x03, 0x90, 0, 0, 0, 0xa3],
                frame_type: 3,
            },
            &Expect {
                size: 348,
                fields: [0, 0, 0x01, 0xe0, 0, 0, 0, 0xa1],
                frame_type: 4,
            },
        );
    }

    /// `reduce_bitpool` / `increase_bitpool` on R1 HQ (§7L): one level per call between 2
    /// and 4. 4–7 share a row, so 4 is the ceiling. Called between packets (as media-sink
    /// does), the new row is used by the very next packet.
    #[test]
    fn level_steps_r1_hq() {
        let mut s = session();
        assert_eq!(s.step_level(true), Ok(2));
        assert_eq!((s.level(), s.stream.samples_per_packet()), (2, 1536));
        assert_eq!(s.step_level(true), Ok(2));
        headers(&mut s, 1);
        assert_eq!(s.step_level(false), Ok(3));
        assert_eq!(s.step_level(false), Ok(4));
        assert_eq!(s.step_level(false), Ok(4));
        assert_eq!((s.level(), s.stream.samples_per_packet()), (4, 1152));
        // Several steps between packets: each starts from the last, and the packet uses the
        // last one.
        assert_eq!(s.step_level(true), Ok(3));
        assert_eq!(s.step_level(true), Ok(2));
        assert_eq!(s.step_level(false), Ok(3));
        headers(&mut s, 1);
        assert_eq!((s.level(), s.stream.samples_per_packet()), (3, 1344));
        // The 2 ms block divides every reachable period (12, 14, 16 and LL's 10 ms).
        assert_eq!(s.block_size(), 96 * 2 * 4);
    }

    /// Mid-packet (part of a period fed) a step stays pending and is applied after the packet
    /// that's in progress, in the §7K order.
    #[test]
    fn level_step_mid_packet_waits() {
        let mut s = session();
        let src = level_block(&s, 0);
        let mut dst = vec![0u8; 2048];
        assert!(!s.encode(&src, &mut dst).unwrap().flush);
        assert_eq!(s.step_level(true), Ok(2));
        assert_eq!((s.level(), s.stream.pending_level()), (3, Some(2)));
        while !s.encode(&src, &mut dst).unwrap().flush {}
        assert_eq!((s.level(), s.stream.samples_per_packet()), (2, 1536));
    }

    /// R1 LL: every level is the same row, so the callbacks report level 3 and change nothing.
    #[test]
    fn level_steps_r1_ll_do_nothing() {
        let mut s = Session::new(DecoderLevel::R1, Profile::Ll, MTU).unwrap();
        assert_eq!(s.step_level(true), Ok(3));
        assert_eq!(s.step_level(false), Ok(3));
        assert_eq!(s.stream.pending_level(), None);
    }

    /// R2 HQ index 1 (§4.1): 204k / 25 ms at level 2 up to 427k / 12 ms at 5, where 5–7
    /// share a row. R2 LL index 1: 220k / 12 ms at 2, 270k / 10 ms from 3.
    #[test]
    fn level_steps_r2() {
        let mut s = Session::new(DecoderLevel::R2, Profile::Hq, MTU).unwrap();
        for want in [4, 5, 5] {
            assert_eq!(s.step_level(false), Ok(want));
        }
        for want in [4, 3, 2, 2] {
            assert_eq!(s.step_level(true), Ok(want));
        }
        assert_eq!(s.block_size(), 48 * 2 * 4);
        let mut s = Session::new(DecoderLevel::R2, Profile::Ll, MTU).unwrap();
        assert_eq!(s.step_level(false), Ok(3));
        assert_eq!(s.step_level(true), Ok(2));
    }

    /// A replay session has no levels: `-ENOTSUP` like PipeWire's aptX.
    #[test]
    fn level_steps_refused_on_replay() {
        let opts = Options {
            replay: Some(vector("sine1k.r1.bin")),
            ..Options::default()
        };
        let mut s = Session::open(DecoderLevel::R1, Profile::Hq, MTU, &opts).unwrap();
        assert_eq!(s.step_level(true), Err(SessionError::Unsupported));
        assert_eq!(s.stream.pending_level(), None);
    }

    /// §7L on air, on the real encoder: a reduce, then an increase, changes the RTP timestamp
    /// low 16 bits from 0x02A0 (672) to 0x0300 (768) and back, each from the packet after the
    /// call. SSRC, payload type and TTP-adjust bits stay, sequence numbers run on, and each
    /// stretch's packets come every 14 ms or 16 ms of input.
    #[test]
    fn reduce_then_increase_on_air() {
        let mut s = Session::new(DecoderLevel::R1, Profile::Hq, MTU).unwrap();
        let sent = media_sink(&mut s, &sine_pcm(2 * 48_000), |s, n| match n {
            20 => assert_eq!(s.step_level(true), Ok(2)),
            40 => assert_eq!(s.step_level(false), Ok(3)),
            _ => {}
        });
        let mut steps = Vec::new();
        for (i, p) in sent.iter().enumerate() {
            assert_eq!(p.len(), RTP_HEADER_LEN + 656, "packet {i}");
            assert_eq!(p[..2], [0x80, 0x60]);
            assert_eq!(p[2..4], (i as u16).to_be_bytes());
            assert_eq!(p[4..6], [0, 0], "TTP adjust, packet {i}");
            assert_eq!(p[8..12], [0, 0, 0, 0xa3], "SSRC, packet {i}");
            let ts = u16::from_be_bytes([p[6], p[7]]);
            if steps.last().is_none_or(|&(_, t)| t != ts) {
                steps.push((i, ts));
            }
        }
        // The call before packet n's header changes packet n's timestamp.
        assert_eq!(steps, [(0, 0x02a0), (20, 0x0300), (40, 0x02a0)]);
        assert_eq!(s.level(), 3);
    }

    /// §7K/§7L: a step during an HQ↔LL switch waits, and the switch's reset puts the level to
    /// 3 and drops it.
    #[test]
    fn level_waits_for_switch() {
        let mut s = session();
        assert_eq!(s.step_level(false), Ok(4));
        headers(&mut s, 1);
        assert_eq!(s.level(), 4);
        s.request(Profile::Ll).unwrap();
        assert_eq!(s.step_level(true), Ok(3));
        let src = level_block(&s, 0);
        let mut dst = vec![0u8; 2048];
        let mut hdr = [0u8; 64];
        let mut levels = Vec::new();
        for _ in 0..600 {
            if s.encode(&src, &mut dst).unwrap().flush {
                s.start_encode(&mut hdr, 0, 0).unwrap();
                levels.push((s.profile(), s.level()));
            }
        }
        // Level 4 throughout HQ despite the request; 3 from the reset on.
        let ll = levels.iter().position(|l| l.0 == Profile::Ll).unwrap();
        assert!(levels[..ll].iter().all(|&l| l == (Profile::Hq, 4)));
        assert!(levels[ll..].iter().all(|&l| l == (Profile::Ll, 3)));
        assert_eq!(s.stream.pending_level(), None);
        // Back in HQ: level 3, 364k / 14 ms.
        s.request(Profile::Hq).unwrap();
        for _ in 0..600 {
            if s.encode(&src, &mut dst).unwrap().flush {
                s.start_encode(&mut hdr, 0, 0).unwrap();
            }
        }
        assert_eq!((s.profile(), s.level()), (Profile::Hq, 3));
        assert_eq!(s.stream.samples_per_packet(), 1344);
    }

    #[test]
    fn level_log_line() {
        let line = level_line((3, 364_000), (2, 318_000));
        assert_eq!(
            line.as_bytes(),
            b"aptx-adaptive: level 3 -> 2 (364k -> 318k)\n"
        );
    }

    /// A level change on the data thread doesn't allocate: the call, the packet that applies
    /// it, and the log line.
    #[test]
    fn level_change_does_not_allocate() {
        let mut s = Session::new(DecoderLevel::R1, Profile::Hq, MTU).unwrap();
        s.log = true;
        let src: Vec<u8> = sine_pcm(96);
        let mut buf = vec![0u8; 2048];
        for _ in 0..100 {
            s.encode(&src, &mut buf).unwrap();
        }
        let count = crate::alloc_count::count(|| {
            for down in [true, false, false, true] {
                s.step_level(down).unwrap();
                while !s.encode(&src, &mut buf).unwrap().flush {}
                s.start_encode(&mut buf, 0, 0).unwrap();
            }
        });
        assert_eq!(count, 0);
        assert_eq!(s.level(), 3);
    }

    /// A refused request is counted, changes nothing and is reported as the running profile.
    #[test]
    fn refused_switch_keeps_old_profile() {
        let opts = Options {
            replay: Some(vector("sine1k.r1.bin")),
            ..Options::default()
        };
        let mut s = Session::open(DecoderLevel::R1, Profile::Ll, MTU, &opts).unwrap();
        // Replay can't switch: it started in HQ instead.
        assert_eq!(s.profile(), Profile::Hq);
        assert_eq!(s.request(Profile::Ll), Err(SessionError::Unsupported));
        assert_eq!(
            (s.profile(), s.target(), s.refused()),
            (Profile::Hq, Profile::Hq, 1)
        );
        assert_eq!(s.fade, Fade::Steady);
        // An MTU that fits LL but is too small for the encoder to run HQ at all: the block is
        // a whole LL packet and HQ is refused.
        let mut s = Session::new(DecoderLevel::R1, Profile::Ll, 360).unwrap();
        assert_eq!(s.block_size(), 480 * 2 * 4);
        assert!(!s.can_run(Profile::Hq));
        assert_eq!(s.request(Profile::Hq), Err(SessionError::Unsupported));
        assert_eq!((s.profile(), s.target()), (Profile::Ll, Profile::Ll));
        assert_eq!(s.interval_ns(), 10_000_000);
    }

    fn vector(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/vectors/r2")
            .join(name)
    }

    /// 2.5 s of PCM through a replay session: every packet is the next vector packet, swapped,
    /// behind the §7F RTP header, wrapping after 71.
    #[test]
    fn replay_loops_vector_packets() {
        let path = vector("sine1k.r1.bin");
        let file = std::fs::read(&path).unwrap();
        assert_eq!(file.len(), 71 * 656);
        let opts = Options {
            replay: Some(path),
            ..Options::default()
        };
        let mut s = Session::open(DecoderLevel::R1, Profile::Hq, MTU, &opts).unwrap();
        let header = RTP_HEADER_LEN;
        let src = vec![0x55u8; packet_size(&s)];
        let mut buffer = vec![0u8; 8192];
        let mut used = s.start_encode(&mut buffer, 0, 0).unwrap();
        let mut sent = Vec::new();
        for _ in 0..178 {
            let r = encode_packet(&mut s, &src, &mut buffer[used..]).unwrap();
            assert_eq!(r.consumed, src.len());
            assert!(r.flush);
            used += r.written;
            sent.push(buffer[..used].to_vec());
            used = s.start_encode(&mut buffer, sent.len() as u16, 0).unwrap();
        }
        for (i, p) in sent.iter().enumerate() {
            assert_eq!(p.len(), header + 656, "packet {i}");
            assert_eq!(p[2..4], (i as u16).to_be_bytes());
            assert_eq!(p[4..12], [0, 0, 0x02, 0xa0, 0, 0, 0, 0xa3], "packet {i}");
            let want = &file[(i % 71) * 656..][..656];
            let mut swapped = [0u8; 656];
            for (o, w) in swapped
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(want.as_chunks::<2>().0)
            {
                *o = [w[1], w[0]];
            }
            assert_eq!(p[header..], swapped, "packet {i}");
        }
        // R1 packets begin 00 81 a1 d?; on air they're swapped.
        let p = &sent[0][header..];
        assert_eq!((p[0], p[1], p[2] & 0xf0, p[3]), (0x81, 0x00, 0xd0, 0xa1));
    }

    #[test]
    fn replay_rejects_bad_files() {
        let opts = |p: &str| Options {
            replay: Some(vector(p)),
            ..Options::default()
        };
        assert!(matches!(
            Session::open(DecoderLevel::R1, Profile::Hq, MTU, &opts("missing.bin")),
            Err(SessionError::BadDevSetting)
        ));
        // 96000 samples × 4 bytes isn't a multiple of 656.
        assert!(matches!(
            Session::open(DecoderLevel::R1, Profile::Hq, MTU, &opts("sine1k.s32le")),
            Err(SessionError::BadDevSetting)
        ));
    }

    #[test]
    fn open_without_replay_uses_encoder() {
        let mut s = Session::open(DecoderLevel::R1, Profile::Hq, MTU, &Options::default()).unwrap();
        assert_eq!(s.start_encode(&mut [0; 64], 0, 0), Ok(12));
        let src = vec![0u8; packet_size(&s)];
        let r = encode_packet(&mut s, &src, &mut [0; 1000]).unwrap();
        assert_eq!((r.written, r.flush), (656, true));
    }

    #[test]
    fn small_mtu_shrinks_packets() {
        assert_eq!(budget(600), 588);
        let s = Session::new(DecoderLevel::R1, Profile::Hq, 600).unwrap();
        assert_eq!(s.stream.row().packet_bytes, 588);
    }

    /// Real packets from the real encoder: swapped `00 01 a1 ..` (config 0, Lp 656).
    #[test]
    fn real_encoder_packets() {
        let mut s = Session::new(DecoderLevel::R1, Profile::Hq, MTU).unwrap();
        assert_eq!(s.stream.row().packet_bytes, 656);
        let src: Vec<u8> = (0..packet_size(&s) / 4)
            .flat_map(|i| ((i as i32 % 200 - 100) << 12).to_ne_bytes())
            .collect();
        let mut dst = [0u8; 1000];
        let r = encode_packet(&mut s, &src, &mut dst).unwrap();
        assert_eq!((r.written, r.flush), (656, true));
        assert_eq!((dst[0] & 0x7F, dst[1], dst[3]), (0x01, 0x00, 0xA1));
    }
}

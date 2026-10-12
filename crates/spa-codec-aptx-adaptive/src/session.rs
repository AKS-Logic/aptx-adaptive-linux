//! One encoding stream: RTP framing around an [`aptx_adaptive::Stream`]. Safe Rust, no FFI.

use aptx_adaptive::{
    Config, Core, DecoderLevel, Encoder, Error, HEADER_LEN, Profile, Stream, packet_budget,
    table_row,
};

use std::io::Write as _;

use crate::dev::{AnyCore, Options, Replay};

/// How the 8-byte aptX Adaptive header goes on air (spec §7.2, `QUESTIONS.md`). Unconfirmed
/// either way; needs a capture from an Android source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// Spec §7B: the RTP timestamp is a time-to-play clock in µs, advancing by the packet
    /// duration from 0; the SSRC is header bytes 4..7. No payload prefix.
    TtpClock,
    /// Like `TtpClock`, but the clock starts at the TTP offset (header bytes 0..1 × 8 µs).
    TtpClockOffset,
    /// Header bytes 0..3 are the RTP timestamp (constant) and 4..7 the SSRC, most significant
    /// byte first. No payload prefix.
    HeaderInRtp,
    /// Spec §7C: a normal RTP header (timestamp = sample count, as `media-sink.c` passes it;
    /// SSRC 0), then the 8 bytes as a payload prefix, then the swapped packet.
    HeaderAfterRtp,
    /// DEV ONLY (spec §7A.2 option b): like `HeaderInRtp` with each 4-byte half reversed.
    HeaderInRtpSwapped,
}

/// The default; `APTX_ADAPTIVE_FRAMING` overrides it (see `dev`).
pub const FRAMING: Framing = Framing::HeaderAfterRtp;

pub const RTP_HEADER_LEN: usize = 12;
/// UNCERTAIN: the spec doesn't give one. aptX HD uses 96.
pub const RTP_PAYLOAD_TYPE: u8 = 96;
const RTP_SSRC: u32 = 0;

const BYTES_PER_SAMPLE: usize = 4;

/// A fixed HQ level-3 stream at 48 kHz stereo (spec §8.4).
fn config(level: DecoderLevel) -> Config {
    Config {
        profile: Profile::Hq,
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
    framing: Framing,
    payload_type: u8,
    swap: bool,
    mtu: usize,
    pcm: Vec<i32>,
    /// Packets sent so far; drives the TTP clock.
    packets: u32,
}

impl Session<Encoder> {
    /// # Errors
    /// Whatever the encoder rejects, or `NoSpace` if the MTU can't fit a packet.
    #[cfg(test)]
    pub fn new(level: DecoderLevel, mtu: usize) -> Result<Self, SessionError> {
        let stream = Stream::new(config(level), budget(FRAMING, mtu))?;
        Self::with_stream(stream, FRAMING, mtu)
    }
}

impl Session<AnyCore> {
    /// A fixed HQ level-3 session on the real encoder, or on a replay core and with the framing
    /// given by `opts`.
    ///
    /// # Errors
    /// Whatever the encoder rejects, `NoSpace` if the MTU can't fit a packet, or `BadDevSetting` if the replay file can't be used (the reason
    /// goes to stderr).
    pub fn open(level: DecoderLevel, mtu: usize, opts: &Options) -> Result<Self, SessionError> {
        let config = config(level);
        let row = table_row(config, budget(opts.framing, mtu))?;
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
        let mut s = Self::with_stream(Stream::with_core(core, config, row), opts.framing, mtu)?;
        s.payload_type = opts.payload_type;
        s.swap = opts.swap;
        Ok(s)
    }
}

fn header_len(framing: Framing) -> usize {
    match framing {
        Framing::TtpClock
        | Framing::TtpClockOffset
        | Framing::HeaderInRtp
        | Framing::HeaderInRtpSwapped => RTP_HEADER_LEN,
        Framing::HeaderAfterRtp => RTP_HEADER_LEN + HEADER_LEN,
    }
}

/// The spec's MTU − 8 budget assumes only the 8-byte header precedes the packet, so take off
/// whatever extra our RTP framing adds.
fn budget(framing: Framing, mtu: usize) -> usize {
    packet_budget(mtu.saturating_sub(header_len(framing) - HEADER_LEN))
}

impl<C: Core> Session<C> {
    /// # Errors
    /// `NoSpace` if a packet and its headers don't fit in the MTU. The packet budget never goes
    /// below 128 bytes, so a small enough MTU can't be met.
    pub fn with_stream(
        stream: Stream<C>,
        framing: Framing,
        mtu: usize,
    ) -> Result<Self, SessionError> {
        if header_len(framing) + stream.row().packet_bytes > mtu {
            return Err(SessionError::NoSpace);
        }
        Ok(Self {
            pcm: vec![0; stream.samples_per_packet()],
            stream,
            framing,
            payload_type: RTP_PAYLOAD_TYPE,
            swap: true,
            mtu,
            packets: 0,
        })
    }

    /// One packet's worth of interleaved `S24_32` PCM.
    pub fn block_size(&self) -> usize {
        self.pcm.len() * BYTES_PER_SAMPLE
    }

    /// One packet's duration in µs (14000 for 672 frames at 48 kHz).
    fn packet_us(&self) -> u32 {
        let c = self.stream.config();
        let frames = self.stream.samples_per_packet() as u64 / u64::from(c.channels);
        (frames * 1_000_000 / u64::from(c.sample_rate)) as u32
    }

    /// The §7B timestamp of the next packet: `start + n × packet duration`, wrapping.
    fn ttp_clock(&self, start: u32) -> u32 {
        start.wrapping_add(self.packets.wrapping_mul(self.packet_us()))
    }

    pub fn interval_ns(&self) -> u64 {
        u64::from(self.stream.row().period_ms) * 1_000_000
    }

    #[expect(
        clippy::unused_self,
        reason = "the encoder delay isn't in the spec yet"
    )]
    pub fn delay_samples(&self) -> u32 {
        0
    }

    /// The per-packet bitrate rule lives in the stream loop (spec §5 step 7). What a Linux
    /// source should do with socket backlog is an open question, so this does nothing.
    #[expect(
        clippy::unused_self,
        reason = "backlog-driven adaptation is an open spec question"
    )]
    pub fn abr_process(&mut self, _unsent: usize) {}

    /// Writes the RTP header (and the aptX header if it's a prefix) and returns its length.
    ///
    /// # Errors
    /// `NoSpace` if `dst` can't hold the header.
    pub fn start_encode(
        &mut self,
        dst: &mut [u8],
        seqnum: u16,
        timestamp: u32,
    ) -> Result<usize, SessionError> {
        let len = header_len(self.framing);
        let h = dst.get_mut(..len).ok_or(SessionError::NoSpace)?;
        let aptx = self.stream.header();
        h[0] = 2 << 6;
        h[1] = self.payload_type;
        h[2..4].copy_from_slice(&seqnum.to_be_bytes());
        match self.framing {
            Framing::TtpClock | Framing::TtpClockOffset => {
                let start = if self.framing == Framing::TtpClockOffset {
                    u32::from(u16::from_le_bytes([aptx[0], aptx[1]])) * 8
                } else {
                    0
                };
                h[4..8].copy_from_slice(&self.ttp_clock(start).to_be_bytes());
                h[8..12].copy_from_slice(&aptx[4..8]);
            }
            Framing::HeaderInRtp => h[4..12].copy_from_slice(&aptx),
            Framing::HeaderInRtpSwapped => {
                h[4..12].copy_from_slice(&aptx);
                h[4..8].reverse();
                h[8..12].reverse();
            }
            Framing::HeaderAfterRtp => {
                h[4..8].copy_from_slice(&timestamp.to_be_bytes());
                h[8..12].copy_from_slice(&RTP_SSRC.to_be_bytes());
                h[12..].copy_from_slice(&aptx);
            }
        }
        Ok(len)
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
        let room = self
            .mtu
            .saturating_sub(header_len(self.framing))
            .min(dst.len());
        let (samples, _) = src[..block].as_chunks::<4>();
        for (s, b) in self.pcm.iter_mut().zip(samples) {
            // S24_32 leaves the top byte unspecified; the encoder wants it sign-extended.
            *s = (i32::from_ne_bytes(*b) << 8) >> 8;
        }
        let written = self.stream.push(&self.pcm, &mut dst[..room])?;
        if written.is_some() {
            self.packets = self.packets.wrapping_add(1);
        }
        if let (Some(n), false) = (written, self.swap) {
            // The stream always swaps; swapping again undoes it.
            for p in dst[..n].as_chunks_mut::<2>().0 {
                p.swap(0, 1);
            }
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

    fn session(framing: Framing) -> Session<Fake> {
        session_with(framing, MTU, budget(framing, MTU))
    }

    fn session_with(framing: Framing, mtu: usize, budget: usize) -> Session<Fake> {
        let stream = fake::stream(config(DecoderLevel::R1), budget).unwrap();
        Session::with_stream(stream, framing, mtu).unwrap()
    }

    #[test]
    fn default_is_header_after_rtp() {
        assert_eq!(FRAMING, Framing::HeaderAfterRtp);
    }

    /// Sends `n` packets and returns their RTP headers.
    fn headers(s: &mut Session<Fake>, n: usize) -> Vec<[u8; 12]> {
        let src = vec![0u8; s.block_size()];
        let mut buf = vec![0u8; 2048];
        (0..n)
            .map(|i| {
                let len = s.start_encode(&mut buf, i as u16, 0xdead_beef).unwrap();
                let r = s.encode(&src, &mut buf[len..]).unwrap();
                assert!(r.flush);
                buf[..12].try_into().unwrap()
            })
            .collect()
    }

    fn timestamp(h: &[u8; 12]) -> u32 {
        u32::from_be_bytes(h[4..8].try_into().unwrap())
    }

    #[test]
    fn ttp_clock_advances_per_packet() {
        let mut s = session(Framing::TtpClock);
        assert_eq!(s.packet_us(), 14_000);
        let hs = headers(&mut s, 5);
        for (i, h) in hs.iter().enumerate() {
            assert_eq!(timestamp(h), i as u32 * 14_000);
            assert_eq!(h[..4], [0x80, 96, 0, i as u8]);
            assert_eq!(h[8..12], [0, 0, 0, 0xae]);
        }
    }

    #[test]
    fn ttp_clock_offset_starts_at_ttp() {
        let mut s = session(Framing::TtpClockOffset);
        let hs = headers(&mut s, 3);
        for (i, h) in hs.iter().enumerate() {
            assert_eq!(timestamp(h), 203_000 + i as u32 * 14_000);
            assert_eq!(h[8..12], [0, 0, 0, 0xae]);
        }
        assert_eq!(hs[0][4..8], 203_000u32.to_be_bytes());
    }

    #[test]
    fn ttp_clock_wraps() {
        let mut s = session(Framing::TtpClockOffset);
        s.packets = (u32::MAX - 203_000) / 14_000;
        let hs = headers(&mut s, 2);
        let before = timestamp(&hs[0]);
        assert!(before > u32::MAX - 14_000);
        assert_eq!(timestamp(&hs[1]), before.wrapping_add(14_000));
        assert!(timestamp(&hs[1]) < 14_000);
        assert_eq!(hs[1][8..12], [0, 0, 0, 0xae]);

        let mut s = session(Framing::TtpClock);
        s.packets = u32::MAX;
        let hs = headers(&mut s, 2);
        assert_eq!(timestamp(&hs[0]), u32::MAX.wrapping_mul(14_000));
        assert_eq!(timestamp(&hs[1]), 0);
    }

    #[test]
    fn header_in_rtp() {
        let mut s = session(Framing::HeaderInRtp);
        let mut buf = [0xff; 64];
        assert_eq!(s.start_encode(&mut buf, 0x1234, 0xdead_beef), Ok(12));
        assert_eq!(
            buf[..13],
            [
                0x80, 96, 0x12, 0x34, 0x1f, 0x63, 0x38, 0x01, 0, 0, 0, 0xae, 0xff
            ]
        );
    }

    #[test]
    fn header_after_rtp() {
        let mut s = session(Framing::HeaderAfterRtp);
        let mut buf = [0xff; 64];
        assert_eq!(s.start_encode(&mut buf, 0x1234, 0xdead_beef), Ok(20));
        assert_eq!(
            buf[..21],
            [
                0x80, 96, 0x12, 0x34, 0xde, 0xad, 0xbe, 0xef, 0, 0, 0, 0, 0x1f, 0x63, 0x38, 0x01,
                0, 0, 0, 0xae, 0xff
            ]
        );
    }

    /// Spec §7C, driven like `media-sink.c`: the timestamp is the sample count so far.
    #[test]
    fn header_after_rtp_counts_samples() {
        let mut s = session(Framing::HeaderAfterRtp);
        let src = vec![0u8; s.block_size()];
        let mut buf = vec![0u8; 2048];
        let mut sample_count = 0u32;
        for i in 0..4u16 {
            let len = s.start_encode(&mut buf, i, sample_count).unwrap();
            assert_eq!(len, 20);
            assert_eq!(buf[..4], [0x80, 96, 0, i as u8]);
            assert_eq!(buf[4..8], (u32::from(i) * 672).to_be_bytes());
            assert_eq!(buf[8..12], [0, 0, 0, 0]);
            assert_eq!(buf[12..20], [0x1f, 0x63, 0x38, 0x01, 0, 0, 0, 0xae]);
            let r = s.encode(&src, &mut buf[len..]).unwrap();
            assert_eq!(len + r.written, 676);
            sample_count += (r.consumed / BYTES_PER_SAMPLE / 2) as u32;
        }
    }

    #[test]
    fn header_in_rtp_swapped() {
        let mut s = session(Framing::HeaderInRtpSwapped);
        s.payload_type = 0;
        let mut buf = [0xff; 64];
        assert_eq!(s.start_encode(&mut buf, 0x1234, 0xdead_beef), Ok(12));
        assert_eq!(
            buf[..13],
            [
                0x80, 0, 0x12, 0x34, 0x01, 0x38, 0x63, 0x1f, 0xae, 0, 0, 0, 0xff
            ]
        );
    }

    #[test]
    fn noswap_leaves_payload_as_encoded() {
        let mut s = session(FRAMING);
        s.swap = false;
        let mut src = vec![0u8; s.block_size()];
        src[..4].copy_from_slice(&5i32.to_ne_bytes());
        src[4..8].copy_from_slice(&6i32.to_ne_bytes());
        let mut dst = [0xffu8; 1000];
        let r = s.encode(&src, &mut dst).unwrap();
        assert_eq!(r.written, 656);
        assert_eq!(dst[..3], [5, 6, 0]);
        assert_eq!(dst[656], 0xff);
    }

    #[test]
    fn open_applies_dev_options() {
        let opts = Options {
            payload_type: 0,
            swap: false,
            ..Options::default()
        };
        let s = Session::open(DecoderLevel::R1, MTU, &opts).unwrap();
        assert_eq!((s.payload_type, s.swap), (0, false));
    }

    #[test]
    fn start_encode_needs_room() {
        let mut s = session(Framing::HeaderAfterRtp);
        assert_eq!(
            s.start_encode(&mut [0; 19], 0, 0),
            Err(SessionError::NoSpace)
        );
    }

    #[test]
    fn timing() {
        let s = session(FRAMING);
        assert_eq!(s.block_size(), 1344 * 4);
        assert_eq!(s.interval_ns(), 14_000_000);
        assert_eq!(s.delay_samples(), 0);
    }

    #[test]
    fn one_block_one_swapped_packet() {
        let mut s = session(FRAMING);
        let mut src = vec![0u8; s.block_size() + 8];
        src[..4].copy_from_slice(&5i32.to_ne_bytes());
        src[4..8].copy_from_slice(&0x00ff_ff06_i32.to_ne_bytes());
        let mut dst = [0xffu8; 1000];
        let r = s.encode(&src, &mut dst).unwrap();
        assert_eq!((r.consumed, r.written, r.flush), (5376, 656, true));
        assert_eq!(dst[..3], [6, 5, 0]);
        assert_eq!(dst[656], 0xff);
    }

    #[test]
    fn short_input_consumes_nothing() {
        let mut s = session(FRAMING);
        let r = s.encode(&[0; 8], &mut [0; 1000]).unwrap();
        assert_eq!((r.consumed, r.flush), (0, false));
    }

    #[test]
    fn mtu_too_small_fails_at_init() {
        let stream = fake::stream(config(DecoderLevel::R1), 2008).unwrap();
        assert!(matches!(
            Session::with_stream(stream, FRAMING, 600),
            Err(SessionError::NoSpace)
        ));
        // The budget floor is 128 bytes, which a tiny MTU can't hold.
        assert!(matches!(
            Session::new(DecoderLevel::R1, 100),
            Err(SessionError::NoSpace)
        ));
        // Nor can a packet whose MTU-fitted bitrate is below the encoder's lowest (§8.8).
        let need = aptx_adaptive::min_packet_bytes(config(DecoderLevel::R1), 14).unwrap();
        let mtu = need + header_len(FRAMING);
        assert!(matches!(
            Session::new(DecoderLevel::R1, mtu - 4),
            Err(SessionError::NoSpace)
        ));
        assert_eq!(
            Session::new(DecoderLevel::R1, mtu)
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
        for framing in [
            Framing::TtpClock,
            Framing::HeaderInRtp,
            Framing::HeaderAfterRtp,
        ] {
            let mut s = session(framing);
            let header = header_len(framing);
            let frames = 48_000;
            let pcm: Vec<u8> = (0..frames * 2)
                .flat_map(|i: i32| (i / 2).to_ne_bytes())
                .collect();
            let block = s.block_size();
            let mut buffer = vec![0u8; 8192];
            let mut sent = Vec::new();
            let mut used = s.start_encode(&mut buffer, 0, 0).unwrap();
            for (seq, src) in pcm.chunks_exact(block).enumerate() {
                let r = s.encode(src, &mut buffer[used..]).unwrap();
                assert_eq!(r.consumed, block);
                used += r.written;
                if r.flush {
                    sent.push(buffer[..used].to_vec());
                    used = s.start_encode(&mut buffer, seq as u16 + 1, 0).unwrap();
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
    }

    fn vector(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/vectors/r2")
            .join(name)
    }

    /// 2.5 s of PCM through a replay session: every packet is the next vector packet, swapped,
    /// behind the right headers, wrapping after 71.
    #[test]
    fn replay_loops_vector_packets_with_framing() {
        let path = vector("sine1k.r1.bin");
        let file = std::fs::read(&path).unwrap();
        assert_eq!(file.len(), 71 * 656);
        for framing in [
            Framing::TtpClock,
            Framing::HeaderInRtp,
            Framing::HeaderAfterRtp,
        ] {
            let opts = Options {
                framing,
                replay: Some(path.clone()),
                ..Options::default()
            };
            let mut s = Session::open(DecoderLevel::R1, MTU, &opts).unwrap();
            let header = header_len(framing);
            let src = vec![0x55u8; s.block_size()];
            let mut buffer = vec![0u8; 8192];
            let mut used = s.start_encode(&mut buffer, 0, 0).unwrap();
            let mut sent = Vec::new();
            for _ in 0..178 {
                let r = s.encode(&src, &mut buffer[used..]).unwrap();
                assert_eq!(r.consumed, src.len());
                assert!(r.flush);
                used += r.written;
                sent.push(buffer[..used].to_vec());
                used = s.start_encode(&mut buffer, sent.len() as u16, 0).unwrap();
            }
            for (i, p) in sent.iter().enumerate() {
                assert_eq!(p.len(), header + 656, "{framing:?} packet {i}");
                assert_eq!(p[2..4], (i as u16).to_be_bytes());
                let aptx = if framing == Framing::TtpClock {
                    let mut a = [0, 0, 0, 0, 0, 0, 0, 0xae];
                    a[..4].copy_from_slice(&(i as u32 * 14_000).to_be_bytes());
                    a
                } else {
                    [0x1f, 0x63, 0x38, 0x01, 0, 0, 0, 0xae]
                };
                assert_eq!(p[header - 8..header], aptx, "{framing:?} packet {i}");
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
                assert_eq!(p[header..], swapped, "{framing:?} packet {i}");
            }
            // R1 packets begin 00 81 a1 d?; on air they're swapped.
            let p = &sent[0][header..];
            assert_eq!((p[0], p[1], p[2] & 0xf0, p[3]), (0x81, 0x00, 0xd0, 0xa1));
        }
    }

    #[test]
    fn replay_rejects_bad_files() {
        let opts = |p: &str| Options {
            replay: Some(vector(p)),
            ..Options::default()
        };
        assert!(matches!(
            Session::open(DecoderLevel::R1, MTU, &opts("missing.bin")),
            Err(SessionError::BadDevSetting)
        ));
        // 96000 samples × 4 bytes isn't a multiple of 656.
        assert!(matches!(
            Session::open(DecoderLevel::R1, MTU, &opts("sine1k.s32le")),
            Err(SessionError::BadDevSetting)
        ));
    }

    #[test]
    fn open_without_replay_uses_encoder() {
        let opts = Options {
            framing: Framing::HeaderAfterRtp,
            ..Options::default()
        };
        let mut s = Session::open(DecoderLevel::R1, MTU, &opts).unwrap();
        assert_eq!(s.start_encode(&mut [0; 64], 0, 0), Ok(20));
        let src = vec![0u8; s.block_size()];
        let r = s.encode(&src, &mut [0; 1000]).unwrap();
        assert_eq!((r.written, r.flush), (656, true));
    }

    #[test]
    fn small_mtu_shrinks_packets() {
        assert_eq!(budget(Framing::HeaderInRtp, 600), 588);
        assert_eq!(budget(Framing::TtpClock, 600), 588);
        assert_eq!(budget(Framing::HeaderAfterRtp, 600), 580);
        let s = Session::new(DecoderLevel::R1, 600).unwrap();
        assert_eq!(s.stream.row().packet_bytes, 580);
    }

    /// Real packets from the real encoder: swapped `00 01 a1 ..` (config 0, Lp 656).
    #[test]
    fn real_encoder_packets() {
        let mut s = Session::new(DecoderLevel::R1, MTU).unwrap();
        assert_eq!(s.stream.row().packet_bytes, 656);
        let src: Vec<u8> = (0..s.block_size() / 4)
            .flat_map(|i| ((i as i32 % 200 - 100) << 12).to_ne_bytes())
            .collect();
        let mut dst = [0u8; 1000];
        let r = s.encode(&src, &mut dst).unwrap();
        assert_eq!((r.written, r.flush), (656, true));
        assert_eq!((dst[0] & 0x7F, dst[1], dst[3]), (0x01, 0x00, 0xA1));
    }
}

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
    /// The spec's hypothesis: header bytes 0..3 are the RTP timestamp and 4..7 the SSRC, most
    /// significant byte first. No payload prefix.
    HeaderInRtp,
    /// A normal RTP header (timestamp in samples), then the 8 bytes as a payload prefix.
    HeaderAfterRtp,
}

/// The default; `APTX_ADAPTIVE_FRAMING` overrides it (see `dev`).
pub const FRAMING: Framing = Framing::HeaderInRtp;

pub const RTP_HEADER_LEN: usize = 12;
/// UNCERTAIN: the spec doesn't give one. aptX HD uses 96.
const RTP_PAYLOAD_TYPE: u8 = 96;
const RTP_SSRC: u32 = 1;

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
            Error::OutputTooSmall | Error::WindowSize => Self::NoSpace,
            Error::UnsupportedConfig | Error::Unimplemented => Self::Unsupported,
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
    mtu: usize,
    pcm: Vec<i32>,
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
        let row = table_row(config, budget(opts.framing, mtu)).ok_or(Error::UnsupportedConfig)?;
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
        Self::with_stream(Stream::with_core(core, config, row), opts.framing, mtu)
    }
}

fn header_len(framing: Framing) -> usize {
    match framing {
        Framing::HeaderInRtp => RTP_HEADER_LEN,
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
            mtu,
        })
    }

    /// One packet's worth of interleaved `S24_32` PCM.
    pub fn block_size(&self) -> usize {
        self.pcm.len() * BYTES_PER_SAMPLE
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
        h[1] = RTP_PAYLOAD_TYPE;
        h[2..4].copy_from_slice(&seqnum.to_be_bytes());
        match self.framing {
            Framing::HeaderInRtp => h[4..12].copy_from_slice(&aptx),
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
    fn default_is_header_in_rtp() {
        assert_eq!(FRAMING, Framing::HeaderInRtp);
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
                0x80, 96, 0x12, 0x34, 0xde, 0xad, 0xbe, 0xef, 0, 0, 0, 1, 0x1f, 0x63, 0x38, 0x01,
                0, 0, 0, 0xae, 0xff
            ]
        );
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
        assert!(Session::new(DecoderLevel::R1, 140).is_ok());
    }

    /// 1 s through the codec the way `media-sink.c` drives it: `start_encode` into a fresh
    /// buffer, `encode` whole blocks until `need_flush`, send, repeat.
    #[test]
    fn one_second_like_media_sink() {
        for framing in [Framing::HeaderInRtp, Framing::HeaderAfterRtp] {
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
        for framing in [Framing::HeaderInRtp, Framing::HeaderAfterRtp] {
            let opts = Options {
                framing,
                replay: Some(path.clone()),
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
                assert_eq!(
                    p[header - 8..header],
                    [0x1f, 0x63, 0x38, 0x01, 0, 0, 0, 0xae]
                );
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
            framing: FRAMING,
            replay: Some(vector(p)),
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
            replay: None,
        };
        let mut s = Session::open(DecoderLevel::R1, MTU, &opts).unwrap();
        assert_eq!(s.start_encode(&mut [0; 64], 0, 0), Ok(20));
        let src = vec![0u8; s.block_size()];
        assert_eq!(
            s.encode(&src, &mut [0; 1000]).err(),
            Some(SessionError::Unsupported)
        );
    }

    #[test]
    fn small_mtu_shrinks_packets() {
        assert_eq!(budget(Framing::HeaderInRtp, 600), 588);
        assert_eq!(budget(Framing::HeaderAfterRtp, 600), 580);
        let s = Session::new(DecoderLevel::R1, 600).unwrap();
        assert_eq!(s.stream.row().packet_bytes, 588);
    }

    #[test]
    fn real_encoder_is_unimplemented() {
        let mut s = Session::new(DecoderLevel::R1, MTU).unwrap();
        assert_eq!(s.stream.row().packet_bytes, 656);
        let src = vec![0u8; s.block_size()];
        assert_eq!(
            s.encode(&src, &mut [0; 1000]).err(),
            Some(SessionError::Unsupported)
        );
    }
}

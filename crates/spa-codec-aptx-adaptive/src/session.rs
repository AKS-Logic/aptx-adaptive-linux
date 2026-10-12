//! One encoding stream: packetisation around a [`PacketEncoder`]. Safe Rust, no FFI.

use aptx_adaptive::{Config, Encoder, Error};

use crate::pod::RawFormat;

/// UNCERTAIN: whether aptX Adaptive media packets start with an RTP header. aptX HD and
/// LDAC use one, classic aptX doesn't. Flip once the spec says.
pub const USE_RTP: bool = true;
pub const RTP_HEADER_LEN: usize = 12;
const RTP_PAYLOAD_TYPE: u8 = 96;
const RTP_SSRC: u32 = 1;

const BYTES_PER_SAMPLE: usize = 4;

/// The boundary between the PipeWire glue and the codec proper.
pub trait PacketEncoder {
    fn channels(&self) -> usize;
    /// PCM frames consumed per packet.
    fn samples_per_packet(&self) -> usize;
    fn encode_packet(&mut self, pcm: &[i32], out: &mut [u8]) -> Result<usize, Error>;
    fn delay_samples(&self) -> u32;
    /// Bitrate adaptation, given the bytes still queued in the socket.
    ///
    /// # Errors
    /// `Unimplemented` until the spec defines it.
    fn abr_process(&mut self, _unsent: usize) -> Result<(), Error> {
        Err(Error::Unimplemented)
    }
}

impl PacketEncoder for Encoder {
    fn channels(&self) -> usize {
        usize::from(self.config().channels)
    }
    fn samples_per_packet(&self) -> usize {
        Encoder::samples_per_packet(self)
    }
    fn encode_packet(&mut self, pcm: &[i32], out: &mut [u8]) -> Result<usize, Error> {
        Encoder::encode_packet(self, pcm, out)
    }
    fn delay_samples(&self) -> u32 {
        Encoder::delay_samples(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    Invalid,
    NoSpace,
    Unsupported,
}

impl From<Error> for SessionError {
    fn from(e: Error) -> Self {
        match e {
            Error::OutputTooSmall => Self::NoSpace,
            Error::InputLength { .. } => Self::Invalid,
            Error::UnsupportedConfig | Error::Unimplemented => Self::Unsupported,
        }
    }
}

pub struct EncodeResult {
    pub consumed: usize,
    pub written: usize,
    pub flush: bool,
}

pub struct Session<E> {
    encoder: E,
    rate: u32,
    mtu: usize,
    header_len: usize,
    pcm: Vec<i32>,
}

impl Session<Encoder> {
    /// # Errors
    /// Whatever the encoder rejects.
    pub fn new(format: &RawFormat, mtu: usize) -> Result<Self, Error> {
        let channels =
            u8::try_from(format.positions.len()).map_err(|_| Error::UnsupportedConfig)?;
        let encoder = Encoder::new(Config {
            sample_rate: format.rate,
            channels,
        })?;
        Ok(Self::with_encoder(encoder, format.rate, mtu))
    }
}

impl<E: PacketEncoder> Session<E> {
    pub fn with_encoder(encoder: E, rate: u32, mtu: usize) -> Self {
        let pcm = vec![0; encoder.samples_per_packet() * encoder.channels()];
        Self {
            encoder,
            rate,
            mtu,
            header_len: 0,
            pcm,
        }
    }

    pub fn block_size(&self) -> usize {
        self.pcm.len() * BYTES_PER_SAMPLE
    }

    pub fn interval_ns(&self) -> u64 {
        self.encoder.samples_per_packet() as u64 * 1_000_000_000 / u64::from(self.rate)
    }

    pub fn delay_samples(&self) -> u32 {
        self.encoder.delay_samples()
    }

    /// # Errors
    /// Whatever the encoder reports, `Unsupported` while it has no bitrate adaptation.
    pub fn abr_process(&mut self, unsent: usize) -> Result<(), SessionError> {
        Ok(self.encoder.abr_process(unsent)?)
    }

    /// Writes the packet header and returns its length.
    ///
    /// # Errors
    /// `NoSpace` if `dst` can't hold the header plus some payload.
    pub fn start_encode(
        &mut self,
        dst: &mut [u8],
        seqnum: u16,
        timestamp: u32,
    ) -> Result<usize, SessionError> {
        self.header_len = 0;
        if !USE_RTP {
            return Ok(0);
        }
        if dst.len() <= RTP_HEADER_LEN {
            return Err(SessionError::NoSpace);
        }
        let h = &mut dst[..RTP_HEADER_LEN];
        h[0] = 2 << 6;
        h[1] = RTP_PAYLOAD_TYPE;
        h[2..4].copy_from_slice(&seqnum.to_be_bytes());
        h[4..8].copy_from_slice(&timestamp.to_be_bytes());
        h[8..12].copy_from_slice(&RTP_SSRC.to_be_bytes());
        self.header_len = RTP_HEADER_LEN;
        Ok(RTP_HEADER_LEN)
    }

    /// Encodes one block of interleaved native-endian 32-bit PCM into `dst`. One encoder
    /// packet per A2DP packet for now, so every successful call asks for a flush.
    ///
    /// # Errors
    /// Encoder failures, or `NoSpace` if the MTU leaves no room for payload.
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
            .checked_sub(self.header_len)
            .ok_or(SessionError::NoSpace)?
            .min(dst.len());
        if room == 0 {
            return Err(SessionError::NoSpace);
        }
        let (samples, _) = src[..block].as_chunks::<4>();
        for (s, b) in self.pcm.iter_mut().zip(samples) {
            *s = i32::from_ne_bytes(*b);
        }
        let written = self.encoder.encode_packet(&self.pcm, &mut dst[..room])?;
        Ok(EncodeResult {
            consumed: block,
            written,
            flush: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test double: writes the first sample's low byte once per channel.
    struct Stub;

    impl PacketEncoder for Stub {
        fn channels(&self) -> usize {
            2
        }
        fn samples_per_packet(&self) -> usize {
            4
        }
        fn encode_packet(&mut self, pcm: &[i32], out: &mut [u8]) -> Result<usize, Error> {
            if out.len() < 2 {
                return Err(Error::OutputTooSmall);
            }
            out[0] = pcm[0] as u8;
            out[1] = pcm[1] as u8;
            Ok(2)
        }
        fn delay_samples(&self) -> u32 {
            7
        }
    }

    #[test]
    fn rtp_header() {
        let mut s = Session::with_encoder(Stub, 48_000, 100);
        let mut buf = [0xff; 64];
        assert_eq!(
            s.start_encode(&mut buf, 0x1234, 0xdead_beef),
            Ok(RTP_HEADER_LEN)
        );
        assert_eq!(
            buf[..12],
            [0x80, 96, 0x12, 0x34, 0xde, 0xad, 0xbe, 0xef, 0, 0, 0, 1]
        );
    }

    #[test]
    fn encodes_one_block_and_flushes() {
        let mut s = Session::with_encoder(Stub, 48_000, 100);
        assert_eq!(s.block_size(), 32);
        let mut src = vec![0u8; 40];
        src[..4].copy_from_slice(&5i32.to_ne_bytes());
        src[4..8].copy_from_slice(&6i32.to_ne_bytes());
        let mut dst = [0u8; 64];
        let r = s.encode(&src, &mut dst).unwrap();
        assert_eq!((r.consumed, r.written, r.flush), (32, 2, true));
        assert_eq!(dst[..2], [5, 6]);
    }

    #[test]
    fn short_input_consumes_nothing() {
        let mut s = Session::with_encoder(Stub, 48_000, 100);
        let r = s.encode(&[0; 8], &mut [0; 64]).unwrap();
        assert_eq!((r.consumed, r.flush), (0, false));
    }

    #[test]
    fn payload_limited_by_mtu() {
        let mut s = Session::with_encoder(Stub, 48_000, RTP_HEADER_LEN + 1);
        s.start_encode(&mut [0; 64], 0, 0).unwrap();
        assert_eq!(
            s.encode(&[0; 32], &mut [0; 64]).err(),
            Some(SessionError::NoSpace)
        );
    }

    #[test]
    fn interval_and_delay() {
        let s = Session::with_encoder(Stub, 48_000, 100);
        assert_eq!(s.interval_ns(), 83_333);
        assert_eq!(s.delay_samples(), 7);
    }

    #[test]
    fn abr_unsupported_by_default() {
        let mut s = Session::with_encoder(Stub, 48_000, 100);
        assert_eq!(s.abr_process(0), Err(SessionError::Unsupported));
    }

    #[test]
    fn rejects_unsupported_rate() {
        let format = RawFormat {
            rate: 44_100,
            ..crate::caps::STREAM
        };
        assert!(matches!(
            Session::new(&format, 800),
            Err(Error::UnsupportedConfig)
        ));
    }

    #[test]
    fn real_encoder_is_unimplemented() {
        let mut s = Session::new(&crate::caps::STREAM, 800).unwrap();
        let src = vec![0u8; s.block_size()];
        assert_eq!(
            s.encode(&src, &mut [0; 800]).err(),
            Some(SessionError::Unsupported)
        );
    }
}

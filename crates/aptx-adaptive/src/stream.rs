//! Per-packet loop (spec `a2dp-integration.md` §5) around an encoder core.

use crate::frame::{self, HEADER_LEN};
use crate::tables::{self, DEFAULT_QUALITY, Link, Profile, Row};
use crate::{Config, Encoder, Error, Progress, Result, WINDOW_SLACK};

/// What the loop needs from the codec proper.
pub trait Core {
    /// See [`Encoder::encode_packet`].
    ///
    /// # Errors
    /// Encoder failures.
    fn encode_packet(&mut self, pcm: &[i32], window: &mut [u8]) -> Result<Progress>;
    /// See [`Encoder::flush`].
    ///
    /// # Errors
    /// Encoder failures.
    fn flush(&mut self, window: &mut [u8]) -> Result<usize>;
    fn set_bitrate(&mut self, bps: u32);
}

/// Above this many interleaved samples of deficit the bitrate drops by 25%.
const DEFICIT_LIMIT: i64 = 96;

#[derive(Debug)]
pub struct Stream<C> {
    core: C,
    config: Config,
    row: Row,
    samples_per_packet: usize,
    pcm: Vec<i32>,
    window: Vec<u8>,
    consumed: usize,
    deficit: i64,
}

impl Stream<Encoder> {
    /// A fixed level-3 stream: no QHS, TWM or 96 kHz (§8.4). `budget` is the packet budget
    /// from [`crate::packet_budget`].
    ///
    /// # Errors
    /// Whatever the encoder rejects.
    pub fn new(config: Config, budget: usize) -> Result<Self> {
        let row = table_row(config, budget)?;
        let core = Encoder::new(config, row.bitrate)?;
        Ok(Self::with_core(core, config, row))
    }
}

/// The level-3 row with no QHS, TWM or 96 kHz, fitted to `budget`.
///
/// # Errors
/// `UnsupportedConfig` if there's no row or encoder for `config`, `PacketTooSmall` if the
/// fitted row's bitrate is below what the encoder runs at, which would grow the backlog every
/// period (`codec-core.md` §8.8, [`crate::min_packet_bytes`]).
pub fn table_row(config: Config, budget: usize) -> Result<Row> {
    let index = tables::index(config.profile, config.level, Link::default());
    let row = tables::row(config.profile, index, DEFAULT_QUALITY)
        .ok_or(Error::UnsupportedConfig)?
        .fit(budget);
    if row.bitrate < crate::min_bitrate(config)? {
        return Err(Error::PacketTooSmall);
    }
    Ok(row)
}

impl<C: Core> Stream<C> {
    pub fn with_core(core: C, config: Config, row: Row) -> Self {
        Self {
            core,
            config,
            row,
            samples_per_packet: row.interleaved_samples(config.sample_rate, config.channels.into()),
            pcm: Vec::new(),
            window: vec![0; row.packet_bytes + WINDOW_SLACK],
            consumed: 0,
            deficit: 0,
        }
    }

    #[must_use]
    pub fn row(&self) -> Row {
        self.row
    }

    #[must_use]
    pub fn config(&self) -> Config {
        self.config
    }

    /// Interleaved samples per packet.
    #[must_use]
    pub fn samples_per_packet(&self) -> usize {
        self.samples_per_packet
    }

    /// The 8-byte header for every packet of this stream (§7).
    #[must_use]
    pub fn header(&self) -> [u8; HEADER_LEN] {
        frame::header(self.config.profile, &self.row)
    }

    #[cfg(test)]
    pub fn core(&self) -> &C {
        &self.core
    }

    /// Feeds interleaved 24-bit PCM. If a packet is out, writes it byte-swapped (no header) to
    /// `out` and returns its length.
    ///
    /// At most one packet comes out per call. A caller that feeds more than
    /// [`Stream::samples_per_packet`] at a time must then call `push(&[], out)` until it
    /// returns `None`, or the queued input grows without limit.
    ///
    /// # Errors
    /// `OutputTooSmall` if `out` can't hold a packet, or encoder failures.
    pub fn push(&mut self, pcm: &[i32], out: &mut [u8]) -> Result<Option<usize>> {
        if out.len() < self.row.packet_bytes {
            return Err(Error::OutputTooSmall);
        }
        self.pcm.extend_from_slice(pcm);
        let p = self.core.encode_packet(&self.pcm, &mut self.window)?;
        self.pcm.drain(..p.consumed);
        self.consumed += p.consumed;

        let due = self.consumed as i64 >= self.samples_per_packet as i64 + self.deficit;
        let written = match p.written {
            0 if !due => return Ok(None),
            0 => self.core.flush(&mut self.window)?,
            n => n,
        };
        if written == 0 {
            return Ok(None);
        }
        if written > out.len() {
            return Err(Error::OutputTooSmall);
        }
        self.deficit += self.samples_per_packet as i64 - self.consumed as i64;
        self.consumed = 0;
        // LL never carries a deficit (`codec-core.md` §10.C).
        if self.config.profile == Profile::Ll {
            self.deficit = 0;
        }
        frame::swap16(&self.window[..written], out);

        let table = self.row.bitrate;
        self.core.set_bitrate(if self.deficit > DEFICIT_LIMIT {
            table - table / 4
        } else {
            table
        });
        Ok(Some(written))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::fake::{self, Fake};
    use crate::{DecoderLevel, packet_budget};

    const PX7: Config = Config {
        profile: Profile::Hq,
        sample_rate: 48_000,
        channels: 2,
        level: DecoderLevel::R1,
    };

    fn px7() -> Stream<Fake> {
        fake::stream(PX7, packet_budget(895)).unwrap()
    }

    fn vectors() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/vectors/r2")
    }

    fn read_pcm(name: &str) -> Vec<i32> {
        let bytes = fs::read(vectors().join(name)).unwrap();
        let (samples, _) = bytes.as_chunks::<4>();
        samples.iter().map(|b| i32::from_le_bytes(*b)).collect()
    }

    /// Returns the size of every packet out.
    fn run(s: &mut Stream<Fake>, pcm: &[i32], frames_per_block: usize) -> Vec<usize> {
        let mut out = [0; 2008];
        pcm.chunks(frames_per_block * 2)
            .filter_map(|b| s.push(b, &mut out).unwrap())
            .collect()
    }

    #[test]
    fn one_second_gives_71_forced_packets() {
        let pcm = read_pcm("sine1k.s32le");
        assert_eq!(pcm.len(), 96_000);
        for block in [672, 48, 100] {
            let mut s = px7();
            assert_eq!(run(&mut s, &pcm, block), vec![656; 71], "block {block}");
            assert_eq!(s.core().forced, vec![true; 71]);
            assert!(s.core().bitrates.iter().all(|&b| b == 364_000));
        }
    }

    /// Drives the loop the way the vectors were made (672-frame blocks) with a fake that emits
    /// on its own wherever the reference did, and checks the `.sizes` files line for line.
    #[test]
    fn reproduces_vector_sizes() {
        for name in ["silence", "sine1k", "noise"] {
            let pcm = read_pcm(&format!("{name}.s32le"));
            for level in ["r1", "r2"] {
                let path = vectors().join(format!("{name}.{level}.sizes"));
                let want: Vec<(usize, bool)> = fs::read_to_string(path)
                    .unwrap()
                    .lines()
                    .enumerate()
                    .map(|(i, l)| {
                        let f: Vec<usize> = l.split(' ').map(|f| f.parse().unwrap()).collect();
                        assert_eq!(f[0], i);
                        (f[1], f[2] == 1)
                    })
                    .collect();
                let mut s = px7();
                // Assumes unforced packets came out at exactly one period of input; only the real encoder can confirm.
                for (i, _) in want.iter().enumerate().filter(|(_, w)| !w.1) {
                    s.core.early.insert(i, 1344);
                }
                let sizes = run(&mut s, &pcm, 672);
                let got: Vec<_> = sizes.into_iter().zip(s.core().forced.clone()).collect();
                assert_eq!(got, want, "{name}.{level}");
                let bin = fs::read(vectors().join(format!("{name}.{level}.bin"))).unwrap();
                assert_eq!(bin.len(), want.iter().map(|w| w.0).sum::<usize>());
            }
        }
    }

    #[test]
    fn packets_are_swapped() {
        let mut s = px7();
        let mut out = [0; 700];
        let mut pcm = [0; 1344];
        pcm[..2].copy_from_slice(&[0x12, 0x34]);
        assert_eq!(s.push(&pcm, &mut out).unwrap(), Some(656));
        assert_eq!(out[..2], [0x34, 0x12]);
        assert_eq!(s.header(), [0x1F, 0x63, 0x38, 0x01, 0, 0, 0, 0xAE]);
    }

    #[test]
    fn early_packet_carries_deficit_and_lowers_bitrate() {
        let mut s = px7();
        s.core.early.extend([(0, 1000), (1, 1688)]);
        let mut out = [0; 656];
        assert_eq!(s.push(&[0; 1344], &mut out).unwrap(), Some(656));
        assert_eq!(s.deficit, 344);
        assert_eq!(s.core().bitrates, [273_000]);
        // 344 samples carried over; the next packet is due at 1344 + 344 consumed.
        assert_eq!(s.push(&[0; 1000], &mut out).unwrap(), None);
        assert_eq!(s.push(&[0; 344], &mut out).unwrap(), Some(656));
        assert_eq!(s.deficit, 0);
        assert_eq!(s.core().bitrates, [273_000, 364_000]);
    }

    /// `codec-core.md` §10.C: in LL an early packet leaves no deficit and no bitrate drop.
    #[test]
    fn ll_zeroes_the_deficit() {
        let ll = Config {
            profile: Profile::Ll,
            ..PX7
        };
        let mut s = fake::stream(ll, packet_budget(895)).unwrap();
        s.core.early.insert(0, 600);
        let mut out = [0; 656];
        assert_eq!(s.push(&[0; 960], &mut out).unwrap(), Some(348));
        assert_eq!(s.deficit, 0);
        assert_eq!(s.core().bitrates, [265_000]);
    }

    #[test]
    fn rejects_packets_that_cant_hold_a_period() {
        assert_eq!(table_row(PX7, 128).unwrap_err(), Error::PacketTooSmall);
        let need = crate::min_packet_bytes(PX7, 14).unwrap();
        assert_eq!(table_row(PX7, need).unwrap().packet_bytes, need);
        assert!(table_row(PX7, need - 4).is_err());
    }

    /// An R1 sink in LL (config 0 on the LL row, 960-sample period, `codec-core.md` §10.D) fed
    /// 1344 samples a call for 30 s, draining with empty pushes: the queue stays under two
    /// periods. LL drops the deficit (§10.C), so a packet that took more than a period isn't
    /// made up for: there are never more packets than periods, and never fewer than one per
    /// push.
    #[test]
    fn oversized_pushes_stay_bounded_when_drained() {
        let config = Config {
            profile: Profile::Ll,
            ..PX7
        };
        let row = table_row(config, packet_budget(895)).unwrap();
        let mut s = Stream::with_core(Encoder::new(config, row.bitrate).unwrap(), config, row);
        assert_eq!(s.samples_per_packet(), 960);
        let mut out = [0; 2008];
        let chunk: Vec<i32> = (0..1344).map(|i| (i * 2654) % 65_536 - 32_768).collect();
        let (mut fed, mut packets, mut most) = (0, 0, 0);
        while fed < 30 * 96_000 {
            packets += usize::from(s.push(&chunk, &mut out).unwrap().is_some());
            while s.push(&[], &mut out).unwrap().is_some() {
                packets += 1;
            }
            fed += chunk.len();
            most = most.max(s.pcm.len());
        }
        assert!(most < 2 * 960, "queue reached {most}");
        assert!(
            packets <= fed / 960 && packets >= fed / chunk.len(),
            "{packets} packets for {fed}"
        );
    }

    #[test]
    fn rejects_small_output() {
        let mut s = px7();
        assert_eq!(s.push(&[0; 2], &mut [0; 655]), Err(Error::OutputTooSmall));
    }

    /// The real encoder through this loop, 48-frame blocks: every r2 vector byte for byte,
    /// after undoing the swap.
    #[test]
    fn real_encoder_gives_the_vectors() {
        for level in [DecoderLevel::R1, DecoderLevel::R2] {
            let tag = if level == DecoderLevel::R1 {
                "r1"
            } else {
                "r2"
            };
            let config = Config { level, ..PX7 };
            for name in ["silence", "sine1k", "noise"] {
                // The vectors use the index-0 row (364 kbps, 14 ms) for both levels.
                let row = table_row(PX7, 887).unwrap();
                let core = Encoder::new(config, row.bitrate).unwrap();
                let mut s = Stream::with_core(core, config, row);
                assert_eq!(s.samples_per_packet(), 1344);
                let want = fs::read(vectors().join(format!("{name}.{tag}.bin"))).unwrap();
                let mut got = Vec::new();
                let mut out = [0; 656];
                for block in read_pcm(&format!("{name}.s32le")).chunks(96) {
                    if let Some(len) = s.push(block, &mut out).unwrap() {
                        let mut packet = [0; 656];
                        frame::swap16(&out[..len], &mut packet);
                        got.extend_from_slice(&packet);
                    }
                }
                assert!(got == want, "{name}.{tag}");
            }
        }
    }
}

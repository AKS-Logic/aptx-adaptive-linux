//! Turns PCM into packets: buffers blocks, picks frame sizes and allocations, codes them.
//!
//! The reference's allocation policy (§8.2) isn't specified, so it's pluggable. Frame sizes
//! follow what the reference does under flush (`tests/vectors/r2`): every pending block goes
//! into the packet, as 64-block frames and then the (even) remainder, e.g. 64 + 64 + 40 for
//! 672 samples of config 0.
//!
//! Each frame aims at its share of the packet's room. One that overshoots spills into the next
//! packet's continuation, up to [`Packetiser::max_carry`] bytes. Past that the frame falls
//! back to the floor allocation, then to fewer blocks, and anything still left over is
//! dropped, so carry and buffering stay bounded however small the packet is.

use super::alloc::{self, Allocator};
use super::pack::{HEADER_BYTES, Packer, continuation_bits, worst_frame_bits};
use super::{Engine, MAX_WIDTH, Params};

pub const MAX_FRAME_BLOCKS: usize = 64;
/// Blocks the packetiser buffers before it stops taking input, a few periods' worth.
pub const MAX_PENDING_BLOCKS: usize = 1024;

pub struct Packetiser {
    engine: Engine,
    packer: Packer,
    allocator: Box<dyn Allocator>,
    max_bits: &'static [u8],
    /// Subband values waiting to go into a frame, `width` per block.
    pending: Vec<i32>,
    /// Interleaved PCM short of a whole block.
    partial: Vec<i32>,
    /// Scratch: the frame's codes, and a trial engine and codes for sizing candidates.
    codes: Vec<i32>,
    trial: Engine,
    trial_codes: Vec<i32>,
    /// Blocks dropped because even the floor allocation didn't fit.
    dropped: u64,
}

impl std::fmt::Debug for Packetiser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Packetiser")
            .field("params", &self.engine.params())
            .field("pending_blocks", &self.pending_blocks())
            .field("dropped", &self.dropped)
            .finish_non_exhaustive()
    }
}

/// Codes `x` on a copy of `engine` into `codes` (resized, buffer kept).
fn trial_code(trial: &mut Engine, engine: &Engine, x: &[i32], bits: &[u8], codes: &mut Vec<i32>) {
    let w = engine.params().width();
    trial.clone_from(engine);
    codes.resize(x.len(), 0);
    for (xb, qb) in x.chunks_exact(w).zip(codes.chunks_exact_mut(w)) {
        trial.code(xb, bits, qb);
    }
}

impl Packetiser {
    pub fn new(
        params: Params,
        packet_len: usize,
        bitrate: u32,
        allocator: Box<dyn Allocator>,
    ) -> Self {
        let engine = Engine::new(params);
        let blocks = MAX_PENDING_BLOCKS * params.width();
        Self {
            engine,
            packer: Packer::new(params, packet_len),
            allocator,
            max_bits: params.max_bits(bitrate),
            pending: Vec::with_capacity(blocks),
            partial: Vec::with_capacity(params.width()),
            codes: Vec::with_capacity(MAX_FRAME_BLOCKS * params.width()),
            trial: engine,
            trial_codes: Vec::with_capacity(MAX_FRAME_BLOCKS * params.width()),
            dropped: 0,
        }
    }

    pub fn packet_len(&self) -> usize {
        self.packer.packet_len()
    }

    /// Most bytes one packet may push into the next one's continuation.
    pub fn max_carry(&self) -> usize {
        (self.packet_len() - HEADER_BYTES) / 4
    }

    pub fn set_bitrate(&mut self, bitrate: u32) {
        self.max_bits = self.engine.params().max_bits(bitrate);
    }

    fn params(&self) -> Params {
        self.engine.params()
    }

    pub fn pending_blocks(&self) -> usize {
        self.pending.len() / self.params().width()
    }

    /// Blocks dropped so far because even the floor allocation didn't fit.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    #[cfg(test)]
    pub fn carry_len(&self) -> usize {
        self.packer.carry_len()
    }

    /// Runs the filterbank over as much of `pcm` as fits in the buffer. Whole blocks go into
    /// the pending queue, a remainder is held for the next call. Returns the samples taken.
    pub fn push_pcm(&mut self, pcm: &[i32]) -> usize {
        let w = self.params().width();
        let room = (MAX_PENDING_BLOCKS - self.pending_blocks()) * w - self.partial.len();
        let pcm = &pcm[..pcm.len().min(room)];
        let mut x = [0; MAX_WIDTH];
        let mut rest = pcm;
        if !self.partial.is_empty() {
            let need = (w - self.partial.len()).min(rest.len());
            self.partial.extend_from_slice(&rest[..need]);
            rest = &rest[need..];
            if self.partial.len() == w {
                self.engine.analyse(&self.partial, &mut x);
                self.pending.extend_from_slice(&x[..w]);
                self.partial.clear();
            }
        }
        let mut blocks = rest.chunks_exact(w);
        for block in &mut blocks {
            self.engine.analyse(block, &mut x);
            self.pending.extend_from_slice(&x[..w]);
        }
        self.partial.extend_from_slice(blocks.remainder());
        pcm.len()
    }

    /// Codes the `blocks` pending blocks from `start` as one frame. Aims at `target` bits;
    /// must stay within `limit`. Returns false, coding nothing, if even the floor allocation
    /// doesn't fit.
    fn frame(&mut self, start: usize, blocks: usize, target: usize, limit: usize) -> bool {
        let params = self.params();
        let (w, s) = (params.width(), params.subbands());
        let x = &self.pending[start * w..(start + blocks) * w];
        let mut bits = [0u8; MAX_WIDTH];
        let bits = &mut bits[..w];
        {
            let (packer, engine, trial, codes) = (
                &self.packer,
                &self.engine,
                &mut self.trial,
                &mut self.trial_codes,
            );
            let mut size = |alloc: &[u8]| {
                trial_code(trial, engine, x, alloc, codes);
                packer.frame_bits(alloc, codes, blocks)
            };
            self.allocator
                .allocate(x, self.max_bits, &mut |a| size(a) <= target, bits);
            let ok = alloc::valid(bits, s)
                && bits.iter().zip(self.max_bits).all(|(b, m)| b <= m)
                && size(bits) <= limit;
            if !ok {
                alloc::floor(s, bits);
                if size(bits) > limit {
                    return false;
                }
            }
        }
        self.codes.resize(x.len(), 0);
        for (xb, qb) in x.chunks_exact(w).zip(self.codes.chunks_exact_mut(w)) {
            self.engine.code(xb, bits, qb);
        }
        self.packer.push_frame(bits, &self.codes);
        true
    }

    /// Codes the pending blocks into the current packet and closes it into `out`. An odd
    /// block stays pending for the next packet; blocks that can't fit are dropped.
    pub fn emit(&mut self, out: &mut [u8]) {
        let total = self.pending_blocks() & !1;
        let limit_bits = self.packer.capacity_bits() + 8 * self.max_carry();
        let mut start = 0;
        for n in plan(total) {
            let left = total - start;
            let used = self.packer.bit_len();
            let target = self.packer.capacity_bits().saturating_sub(used) * n / left;
            let limit = limit_bits.saturating_sub(used);
            let mut m = n;
            while m > 0 && !self.frame(start, m, target * m / n, limit) {
                // Shrink until the floor fits, which makes this the packet's last frame.
                m = (m / 2) & !1;
            }
            start += m;
            if m < n {
                break;
            }
        }
        self.dropped += (total - start) as u64;
        self.pending.drain(..total * self.params().width());
        self.packer.finish(out);
    }
}

/// Smallest packet that holds `blocks` blocks (one period) at the floor allocation, with no
/// continuation, whatever the codes. Smaller packets still work but drop audio.
pub fn min_packet_bytes(params: Params, blocks: usize) -> usize {
    let mut floor = [0u8; MAX_WIDTH];
    let floor = &mut floor[..params.width()];
    alloc::floor(params.subbands(), floor);
    let bits = continuation_bits(0)
        + plan(blocks)
            .map(|n| worst_frame_bits(params, floor, n))
            .sum::<usize>();
    (HEADER_BYTES + bits.div_ceil(8)).next_multiple_of(4)
}

/// Frame sizes for `pending` blocks: 64s, then the even remainder. An odd block waits.
fn plan(pending: usize) -> impl Iterator<Item = usize> {
    let even = pending & !1;
    (0..even.div_ceil(MAX_FRAME_BLOCKS))
        .map(move |k| (even - k * MAX_FRAME_BLOCKS).min(MAX_FRAME_BLOCKS))
}

#[cfg(test)]
mod tests {
    use super::{Params, plan};

    #[test]
    fn floor_packet_sizes() {
        use crate::codec::Variant;
        let cfg0 = Params {
            variant: Variant::Cfg0,
            sample_rate: 48_000,
        };
        let need = super::min_packet_bytes(cfg0, 168);
        assert!((128..656).contains(&need), "{need}");
        assert!(super::min_packet_bytes(cfg0, 2) < need);
    }

    #[test]
    fn frame_plans() {
        let plan = |n| plan(n).collect::<Vec<_>>();
        assert_eq!(plan(168), [64, 64, 40]);
        assert_eq!(plan(114), [64, 50]);
        assert_eq!(plan(1), [] as [usize; 0]);
        assert_eq!(plan(64), [64]);
        assert_eq!(plan(65), [64]);
        assert_eq!(plan(7), [6]);
    }
}

#[cfg(test)]
mod live {
    use std::collections::VecDeque;
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::codec::Variant;
    use crate::codec::alloc::WaterFill;
    use crate::codec::decode::{Frame, decode};

    const CFG0: Params = Params {
        variant: Variant::Cfg0,
        sample_rate: 48_000,
    };

    fn sine(frames: usize, rate: f64) -> Vec<i32> {
        (0..frames)
            .flat_map(|n| {
                let t = f64::from(n as u32) / rate;
                let l = 0.3 * (2.0 * std::f64::consts::PI * 1000.0 * t).sin();
                let r = 0.2 * (2.0 * std::f64::consts::PI * 3100.0 * t).sin();
                [l, r].map(|v| (v * 8_388_607.0) as i32)
            })
            .collect()
    }

    fn noise(frames: usize) -> Vec<i32> {
        let mut s = 1u32;
        (0..frames * 2)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s as i32) >> 10
            })
            .collect()
    }

    /// Gives back a fixed list of allocations in order, ignoring the budget.
    struct Replay(VecDeque<Vec<u8>>);

    impl Allocator for Replay {
        fn allocate(
            &mut self,
            _: &[i32],
            _: &[u8],
            _: &mut dyn FnMut(&[u8]) -> bool,
            out: &mut [u8],
        ) {
            out.copy_from_slice(&self.0.pop_front().expect("ran out of allocations"));
        }
    }

    /// Spends every bit it's offered and then some: the most it's allowed, ignoring the budget.
    struct Greedy;

    impl Allocator for Greedy {
        fn allocate(
            &mut self,
            _: &[i32],
            max: &[u8],
            _: &mut dyn FnMut(&[u8]) -> bool,
            out: &mut [u8],
        ) {
            out.copy_from_slice(max);
            for b in out.iter_mut() {
                if *b == 1 {
                    *b = 2;
                }
            }
        }
    }

    fn read_s32(path: &PathBuf) -> Vec<i32> {
        fs::read(path)
            .unwrap()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| i32::from_le_bytes(*b))
            .collect()
    }

    /// Encodes `pcm` a period at a time. Returns the packets.
    fn encode(p: &mut Packetiser, period: usize, pcm: &[i32]) -> Vec<u8> {
        let mut packets = Vec::new();
        let mut out = vec![0; p.packet_len()];
        for chunk in pcm.chunks_exact(period) {
            assert_eq!(p.push_pcm(chunk), chunk.len());
            p.emit(&mut out);
            packets.extend_from_slice(&out);
            assert!(p.carry_len() <= p.max_carry());
        }
        packets
    }

    fn check_frames(params: Params, frames: &[Frame], max: &[u8]) {
        for f in frames {
            assert!(alloc::valid(&f.bits, params.subbands()), "{:?}", f.bits);
            assert!(f.bits.iter().zip(max).all(|(b, m)| b <= m), "{:?}", f.bits);
        }
    }

    /// The live path (`push_pcm`, `plan`, budget split, `emit`) with the reference's allocations
    /// gives the reference's packets byte for byte, 64 + 64 + 40 frames each.
    #[test]
    fn replayed_allocations_give_r1_packets() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/vectors/r2");
        for name in ["silence", "sine1k", "noise"] {
            let want = fs::read(dir.join(format!("{name}.r1.bin"))).unwrap();
            let pcm = read_s32(&dir.join(format!("{name}.s32le")));
            let frames = decode(CFG0, &want, 656);
            for k in 0..70 {
                let sizes: Vec<_> = frames
                    .iter()
                    .filter(|f| f.packet == k)
                    .map(Frame::blocks)
                    .collect();
                assert_eq!(sizes, [64, 64, 40], "{name} packet {k}");
            }
            let allocs = frames.iter().map(|f| f.bits.clone()).collect();
            let mut p = Packetiser::new(CFG0, 656, 364_000, Box::new(Replay(allocs)));
            let got = encode(&mut p, 1344, &pcm[..1344 * 70]);
            assert_eq!(p.dropped(), 0);
            for (i, (g, w)) in got.chunks(656).zip(want.chunks(656)).enumerate() {
                assert!(g == w, "{name} packet {i}");
            }
        }
    }

    /// Frames that overshoot spill into the next packet's continuation, which stays bounded
    /// and still parses.
    #[test]
    fn overshoot_carries_and_decodes() {
        let mut p = Packetiser::new(CFG0, 656, 427_000, Box::new(Greedy));
        let packets = encode(&mut p, 1344, &noise(14_400));
        let carried = packets.chunks(656).filter(|pk| pk[3] & 0x80 == 0).count();
        assert!(carried > 0, "no packet had a continuation");
        let frames = decode(CFG0, &packets, 656);
        check_frames(CFG0, &frames, CFG0.max_bits(427_000));
        assert!(
            frames.iter().any(|f| f.bits == [4, 0, 0, 0, 4, 0, 0, 0]),
            "never fell back"
        );
    }

    /// Packets too small for a period at the floor allocation drop blocks rather than grow the
    /// carry or the buffer.
    #[test]
    fn small_packets_stay_bounded() {
        for len in [128, 132, 136, 140] {
            let mut p = Packetiser::new(CFG0, len, 364_000, Box::new(WaterFill));
            let pcm = noise(672 * 2000);
            let mut out = vec![0; len];
            let mut packets = Vec::new();
            for chunk in pcm.as_chunks::<1344>().0 {
                p.push_pcm(chunk);
                p.emit(&mut out);
                assert!(p.carry_len() <= p.max_carry());
                assert!(p.pending_blocks() <= 1);
                assert!(p.pending.capacity() <= MAX_PENDING_BLOCKS * 8);
                if packets.len() < 50 * len {
                    packets.extend_from_slice(&out);
                }
            }
            assert!(p.dropped() > 0, "{len}: expected drops");
            check_frames(CFG0, &decode(CFG0, &packets, len), CFG0.max_bits(364_000));
        }
    }

    #[test]
    fn push_pcm_stops_at_the_buffer_limit() {
        let mut p = Packetiser::new(CFG0, 656, 364_000, Box::new(WaterFill));
        let taken = p.push_pcm(&vec![0; 8 * MAX_PENDING_BLOCKS * 2]);
        assert_eq!(taken, 8 * MAX_PENDING_BLOCKS);
        assert_eq!(p.push_pcm(&[0; 8]), 0);
        // Odd samples carry across calls.
        let mut q = Packetiser::new(CFG0, 656, 364_000, Box::new(WaterFill));
        q.push_pcm(&[1, 2, 3]);
        q.push_pcm(&[4, 5, 6, 7, 8, 9]);
        assert_eq!((q.pending_blocks(), q.partial.len()), (1, 1));
    }

    /// Encodes `pcm` a period at a time with `WaterFill`, then parses the packets back.
    fn run(params: Params, packet_len: usize, bitrate: u32, period: usize, pcm: &[i32]) {
        let mut p = Packetiser::new(params, packet_len, bitrate, Box::new(WaterFill));
        let packets = encode(&mut p, period, pcm);
        assert_eq!(p.dropped(), 0);
        check_frames(
            params,
            &decode(params, &packets, packet_len),
            params.max_bits(bitrate),
        );
    }

    #[test]
    fn cfg0_packets_parse() {
        for pcm in [sine(48_000, 48_000.0), noise(48_000), vec![0; 96_000]] {
            run(CFG0, 656, 364_000, 1344, &pcm);
        }
    }

    #[test]
    fn cfg1_and_cfg3_packets_parse() {
        let cfg1 = Params {
            variant: Variant::Cfg1,
            sample_rate: 48_000,
        };
        run(cfg1, 348, 265_000, 960, &noise(24_000));
        let cfg3 = Params {
            variant: Variant::Cfg3,
            sample_rate: 48_000,
        };
        run(cfg3, 656, 270_000, 1824, &sine(24_000, 48_000.0));
        let cfg3_96k = Params {
            variant: Variant::Cfg3,
            sample_rate: 96_000,
        };
        run(cfg3_96k, 656, 340_000, 2880, &noise(48_000));
    }

    /// The coded subbands follow the input: reconstruction (prediction + difference) against
    /// the analysis output, for a sine at the Px7's rate.
    #[test]
    fn cfg0_reconstruction_tracks_signal() {
        let w = CFG0.width();
        let pcm = sine(9600, 48_000.0);
        let mut engine = Engine::new(CFG0);
        let packer = Packer::new(CFG0, 656);
        let mut x = vec![0; pcm.len() / 2];
        for (p, xb) in pcm.chunks_exact(w).zip(x.chunks_exact_mut(w)) {
            engine.analyse(p, xb);
        }
        // About a packet's payload per 168 blocks, as at 364 kbps.
        let budget = 64 * packer.capacity_bits() / 168;
        let (mut sig, mut err) = (0f64, 0f64);
        let mut pred = vec![0; w];
        let (mut trial, mut codes) = (engine, Vec::new());
        for frame in x.chunks_exact(64 * w) {
            let mut bits = vec![0; w];
            let mut fits = |alloc: &[u8]| {
                trial_code(&mut trial, &engine, frame, alloc, &mut codes);
                packer.frame_bits(alloc, &codes, 64) <= budget
            };
            WaterFill.allocate(frame, CFG0.max_bits(364_000), &mut fits, &mut bits);
            let mut q = vec![0; w];
            for xb in frame.chunks_exact(w) {
                let trace = engine.code_traced(xb, &bits, &mut q);
                for i in 0..w {
                    let want = f64::from(xb[i] >> 8);
                    let got = f64::from(pred[i] + trace.recon[i]);
                    sig += want * want;
                    err += (want - got) * (want - got);
                }
                pred.clone_from(&trace.pred_out);
            }
        }
        let snr = 10.0 * (sig / err).log10();
        assert!(snr > 30.0, "subband SNR {snr:.1} dB");
    }
}

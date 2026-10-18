//! The native encoder: PCM in, aptX Adaptive OTA packets out.
//!
//! Two frame modes share the pipeline ([`FrameMode`]):
//!
//! * [`FrameMode::R2`] -- the 656-byte R2 HQ frame (config 3, 150 calls =
//!   1200 samples = 25 ms at 48 kHz), the default.
//! * [`FrameMode::Ll`] -- the 348-byte R2 LL frame (config 4, 60 calls =
//!   480 samples = 10 ms at 48 kHz, 60 blocks, 360 bytes on air with the
//!   12-byte RTP header).  Selected by [`Config::ll`] or `APTX_NATIVE_LL=1`.
//!
//! Pipeline per frame (default 150 CMFB calls = 25 ms at 48 kHz, matching
//! the period byte 100 of captured R2 streams):
//!
//! ```text
//! PCM (L/R) ── CMFB analysis (dual, per channel) ──► 8 bands × N values
//!      per band: fixed o2 predictor (g_FixedO2) ──► residual
//!      residual normalised by a per-band scale shift
//!      ── non-uniform quantiser (nu_dq levels) at the band's bit width
//!      ── bit-packed payload (allocation nibbles + scale bytes + codes)
//!      ── OTA header (ttp, period, packet type, channel mode, version)
//! ```
//!
//! Prediction is closed-loop: the predictor history holds the values the
//! *decoder* will reconstruct (quantiser output plus prediction), so the
//! two stay in lockstep — the same discipline as `axBlock_*_LPC` in the
//! shipped encoder.
//!
//! The payload is self-describing: a format byte, a frame counter, a call
//! count, one allocation byte and one scale byte per band, then per band
//! the L and R quantiser codes at the allocated width, zero-padded to the
//! fixed transport payload size.  [`crate::decoder::Decoder`] reads exactly
//! this form, which is what the end-to-end test exercises.
//!
//! SPDX-License-Identifier: MIT

use crate::cmfb::Cmfb;
use crate::entropy::BitWriter;
use crate::frame::{FrameShape, LL_FRAME, R2_FRAME};
use crate::packer::*;
use crate::quant::Quantizer;
use crate::vendor_core::{dither, Analysis8, Band, Predictor, TAPS_8BAND_48K};
use crate::BANDS;

/// Payload format marker (our self-describing form).
pub const PAYLOAD_MAGIC: u8 = 0xA1;

/// Codec payload of the low-latency frame (payload-table type 0).
pub const LL_PAYLOAD_BYTES: usize = 348;

/// On-air size of an LL packet: the 12-byte RTP header plus the codec
/// payload (360 bytes).
pub const LL_ON_AIR_BYTES: usize = 12 + LL_PAYLOAD_BYTES;

/// Blocks per LL frame: 480 samples / 8 samples per block.
pub const LL_BLOCKS: usize = 60;

/// LL period byte: 10 ms in 0.25 ms units.
pub const LL_PERIOD: u8 = 40;

/// Lowest LL time-to-play lead, in 1/15000 s units (60 ms).
pub const LL_TTP_MIN_UNITS: u16 = 900;

/// Highest LL time-to-play lead, in 1/15000 s units (80 ms).
pub const LL_TTP_MAX_UNITS: u16 = 1200;

/// LL time-to-play lead used when a stream starts: the low end of the
/// 60-80 ms window.
pub const LL_TTP_START_UNITS: u16 = LL_TTP_MIN_UNITS;

/// Payload presentation the vendor frame writer emits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameMode {
    /// R2 HQ: 656-byte payload (config 3), 150 calls = 25 ms at 48 kHz.
    R2,
    /// R2 LL: 348-byte payload (config 4, frame type 4), 60 calls = 10 ms
    /// at 48 kHz.  This is the R2 sink's LL bitstream; an R1 sink's LL row
    /// is config 0 (frame type 1, 4-band), which this 8-band encoder cannot
    /// produce.  The Bose QC Ultra (1st gen) is an R2 sink.
    Ll,
}

impl FrameMode {
    /// The frame geometry this mode writes.
    pub const fn shape(self) -> FrameShape {
        match self {
            Self::R2 => R2_FRAME,
            Self::Ll => LL_FRAME,
        }
    }

    /// True for the 348-byte LL frame.
    pub const fn is_ll(self) -> bool {
        match self {
            Self::Ll => true,
            Self::R2 => false,
        }
    }
}

/// Encoder configuration.
#[derive(Clone, Debug)]
pub struct Config {
    /// Use the 256-tap window (false = 128-tap).
    pub long_window: bool,
    /// CMFB calls per frame (8 PCM samples per call per channel).
    pub calls_per_frame: usize,
    /// Packet type (selects the payload size).
    pub packet_type: u8,
    /// Period byte (0.25 ms units).
    pub period: u8,
    /// Channel mode byte.
    pub channel_mode: u8,
    /// Version byte.
    pub version: u8,
    /// Emit vendor-structure R2 frames (header + TID0 band stream) instead
    /// of the research payload.  Off by default so the legacy round-trip
    /// tests keep their contract; the C API path turns it on.
    pub vendor_frame: bool,
    /// Payload presentation: the 656-byte R2 HQ frame or the 348-byte LL
    /// frame.  `Encoder::new` normalizes an LL configuration to the LL
    /// geometry (60 calls, payload type 0, period 40).
    pub frame_mode: FrameMode,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            long_window: true,
            calls_per_frame: 150,
            packet_type: 1,
            period: 100,
            channel_mode: CHANNEL_MODE_STEREO,
            version: VERSION_R2,
            vendor_frame: false,
            frame_mode: FrameMode::R2,
        }
    }
}

impl Config {
    /// The transport payload size for this configuration.
    ///
    /// The LL mode always emits its 348-byte payload (payload-table type 0);
    /// the R2 mode keeps the packet-type lookup.
    pub fn payload_size(&self) -> usize {
        match self.frame_mode {
            FrameMode::Ll => LL_PAYLOAD_BYTES,
            FrameMode::R2 => PAYLOAD_SIZES[self.packet_type as usize] as usize,
        }
    }

    /// The 348-byte R2 LL configuration: 60 blocks (480 samples, 10 ms) at
    /// 48 kHz, payload type 0, period byte 40, vendor frame writer on.
    ///
    /// The frame writer emits config-4 syntax (frame type 4, no allocation
    /// escapes).  The analysis bank stays the caller's choice: a bit-exact
    /// config-4 stream needs the 128-tap prototype, which is a separate DSP
    /// change (`long_window = false` is the closest available switch).
    pub fn ll() -> Self {
        Config {
            calls_per_frame: LL_BLOCKS,
            packet_type: 0,
            period: LL_PERIOD,
            vendor_frame: true,
            frame_mode: FrameMode::Ll,
            ..Config::default()
        }
    }
}

const HDR_BYTES: usize = 20;

/// Widest band config 4 can code: the 3-bit allocation escape that would
/// express 10..=16 bits belongs to config 3.
const LL_MAX_WIDTH: u8 = 9;

/// Bits kept free for the allocation head and its differential updates when
/// the greedy allocator fills the stream region.
const HEAD_RESERVE: usize = 320;

/// Width classes go up to 12 in the TID0 geometry (4 symbol bits + 8 ext).
fn width_next_exceeds(w: u8) -> bool {
    w >= 12
} // magic(1) + counter(2) + calls(2) + alloc(8) + scale(8) + spare(1)

/// `x · 2^-shift` for a signed shift (negative shift upscales).
pub fn downscale(x: i64, shift: i32) -> i64 {
    if shift >= 0 {
        x >> shift
    } else {
        x << (-shift)
    }
}

/// `x · 2^shift` — the inverse of [`downscale`].
pub fn upscale(x: i64, shift: i32) -> i64 {
    if shift >= 0 {
        x << shift
    } else {
        x >> (-shift)
    }
}

/// Closed-loop reconstruction: `pred + rec · 2^shift`, saturated to i32.
/// Both encoder and decoder must use this exact function to stay locked.
pub fn reconstruct(pred: i32, rec: f64, shift: i32) -> i32 {
    let x = pred as i64 + upscale(rec as i64, shift);
    x.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// The encoder state.
pub struct Encoder {
    cfg: Config,
    cmfb: [Cmfb; 2],
    /// per channel, per band: last two reconstructed values (predictor state)
    hist: [[[i32; 2]; BANDS]; 2],
    /// PCM accumulation (per channel)
    pcm: [Vec<i32>; 2],
    rtp_seq: u16,
    /// cumulative TTP at 4x resolution (TTP unit = 1/15000 s = period*15/4
    /// ticks; quarter ticks keep the cadence exact for any period byte)
    ttp4: u64,
    frames: u64,
    /// vendor-validated analysis bank, one per channel
    analysis: [Analysis8; 2],
    /// vendor-validated pole-zero sign-sign LMS predictor, per channel per band
    pred: [[Predictor; BANDS]; 2],
    /// vendor-validated adaptive quantiser, per channel per band
    bands: [[Band; BANDS]; 2],
    /// dither seeds, one per channel
    seeds: [i32; 2],
    /// widths used by the last vendor frame (for the round-trip oracle)
    last_widths: [u8; BANDS],
    /// validated vendor frame writer (head + allocation + composer steps)
    framewriter: crate::frame::FrameWriter,
    /// width-allocation backend for vendor frames (APTX_NATIVE_ALLOC)
    alloc_mode: AllocMode,
}

impl Encoder {
    /// Create an encoder.
    pub fn new(cfg: Config) -> Self {
        // Resolve the frame presentation once per stream: APTX_NATIVE_LL
        // wins over the caller's config, and an LL configuration is
        // normalized to its fixed geometry.
        let cfg = resolve_frame_mode(cfg);
        // Resolve the allocation backend once per stream: the env switch is a
        // build/stream-level choice, not a per-frame one.
        let alloc_env = std::env::var("APTX_NATIVE_ALLOC").ok();
        let alloc_mode = parse_alloc_mode(alloc_env.as_deref(), VENDOR_ALLOC_AVAILABLE);
        if alloc_env
            .as_deref()
            .is_some_and(|s| s.trim().eq_ignore_ascii_case("vendor"))
            && !VENDOR_ALLOC_AVAILABLE
        {
            eprintln!(
                "aptx-native: APTX_NATIVE_ALLOC=vendor requested, but the \
                 bitalloc_vendor module is not compiled in; using greedy allocation"
            );
        }
        Encoder {
            cmfb: [Cmfb::new(cfg.long_window), Cmfb::new(cfg.long_window)],
            hist: [[[0; 2]; BANDS]; 2],
            pcm: [Vec::new(), Vec::new()],
            rtp_seq: 0,
            // LL starts at the low end of the 60-80 ms time-to-play window;
            // the R2 counter keeps its historical zero origin.
            ttp4: if cfg.frame_mode.is_ll() {
                u64::from(LL_TTP_START_UNITS) * 4
            } else {
                0
            },
            frames: 0,
            cfg,
            analysis: [Analysis8::new(), Analysis8::new()],
            pred: std::array::from_fn(|_| {
                std::array::from_fn(|b| Predictor::new(TAPS_8BAND_48K[b]))
            }),
            bands: std::array::from_fn(|_| std::array::from_fn(|_| Band::default())),
            seeds: [0; 2],
            last_widths: [0; BANDS],
            framewriter: Default::default(),
            alloc_mode,
        }
    }

    /// The current RTP sequence number (advanced per emitted packet).
    pub fn rtp_seq(&self) -> u16 {
        self.rtp_seq
    }

    /// Widths used by the most recent vendor frame (per band, 0 = inactive).
    pub fn last_widths(&self) -> [u8; BANDS] {
        self.last_widths
    }

    /// Composer steps the last frame actually wrote (the frame budget may
    /// cut the tail; the encoder refits so this equals `calls_per_frame / 2`).
    pub fn last_steps_written(&self) -> usize {
        self.framewriter.last_steps_written
    }

    /// Blocks (8 samples per channel) the most recent vendor frame coded:
    /// the writer's step count times two.  60 for a full LL frame, 150 for
    /// a 150-call R2 frame.  The C plugin uses this as its RTP step.
    pub fn last_blocks(&self) -> usize {
        self.framewriter.last_steps_written * 2
    }

    /// The frame presentation this encoder writes.
    pub fn frame_mode(&self) -> FrameMode {
        self.cfg.frame_mode
    }

    /// Writer-side layout of the most recent frame: `(stream bits before the
    /// first composer pair, stream bit positions of each differential
    /// update)` — for the structural tests.
    pub fn last_frame_layout(&self) -> (usize, Vec<usize>) {
        (
            self.framewriter.last_head_end,
            self.framewriter.last_dif_pos.clone(),
        )
    }

    /// Feed PCM (equal length L/R) and return any completed packets.
    pub fn encode(&mut self, left: &[i32], right: &[i32]) -> Vec<OtaPacket> {
        assert_eq!(left.len(), right.len(), "stereo feeds must match");
        self.pcm[0].extend_from_slice(left);
        self.pcm[1].extend_from_slice(right);

        let per_frame = self.cfg.calls_per_frame * 8;
        let mut out = Vec::new();
        while self.pcm[0].len() >= per_frame && self.pcm[1].len() >= per_frame {
            let l: Vec<i32> = self.pcm[0].drain(..per_frame).collect();
            let r: Vec<i32> = self.pcm[1].drain(..per_frame).collect();
            out.push(self.encode_frame(&l, &r));
        }
        out
    }

    /// Encode one frame's worth of PCM (research payload format; the layout
    /// is the one [`crate::decoder::Decoder`] reads).
    fn encode_frame(&mut self, l: &[i32], r: &[i32]) -> OtaPacket {
        if self.cfg.vendor_frame {
            return self.encode_frame_vendor(l, r);
        }
        let calls = self.cfg.calls_per_frame;

        // 1. CMFB analysis
        let mut vals: [Vec<[i32; BANDS]>; 2] =
            [Vec::with_capacity(calls), Vec::with_capacity(calls)];
        for (ch, src) in [l, r].into_iter().enumerate() {
            for c in 0..calls {
                let mut blk = [0i32; 8];
                blk.copy_from_slice(&src[c * 8..c * 8 + 8]);
                vals[ch].push(self.cmfb[ch].analyze(&blk));
            }
        }

        // 2. open-loop residuals for the per-band energy and peak
        let mut energy = [0f64; BANDS];
        let mut peak = [0i64; BANDS];
        for ch in 0..2 {
            for band in 0..BANDS {
                let mut h = self.hist[ch][band];
                for c in 0..calls {
                    let v = vals[ch][c][band];
                    let pred = 2 * h[0] - h[1];
                    let rr = (v - pred) as i64;
                    peak[band] = peak[band].max(rr.abs());
                    energy[band] += (rr as f64) * (rr as f64);
                    h = [v, h[0]];
                }
            }
        }

        // 3. allocation and per-band scale: the quantiser's top level is
        // 2^(bits-1), so shift the peak residual onto it.
        let want = self.cfg.payload_size();
        let alloc = crate::bitalloc::allocate_passband(&energy, calls, 2, want * 8 - HDR_BYTES * 8);
        let mut shifts = [0i32; BANDS];
        for band in 0..BANDS {
            if alloc[band] == 0 {
                continue;
            }
            // the nu_dq level tables always span the full 16-bit range; more
            // bits only make the steps finer
            let top = 32768i64;
            let p = peak[band].max(1);
            let mut s = 0i32;
            if p > top {
                while s < 30 && (p >> s) > top {
                    s += 1;
                }
            } else {
                while s > -24 && (p << (-s)) < top {
                    s -= 1;
                }
            }
            shifts[band] = s;
        }

        // 4. closed-loop quantise, payload assembly
        let mut bw = BitWriter::new();
        bw.write(PAYLOAD_MAGIC as u32, 8);
        bw.write((self.frames & 0xffff) as u32, 16);
        bw.write(calls as u32, 16);
        for b in 0..BANDS {
            bw.write(alloc[b] as u32, 8);
        }
        for b in 0..BANDS {
            bw.write((shifts[b] as u8) as u32, 8);
        }
        for b in 0..BANDS {
            if alloc[b] == 0 {
                continue;
            }
            let q = Quantizer::new(alloc[b]);
            let s = shifts[b];
            for ch in 0..2 {
                let mut h = self.hist[ch][b];
                for c in 0..calls {
                    let v = vals[ch][c][b];
                    let pred = 2 * h[0] - h[1];
                    let rr = (v - pred) as i64;
                    let (code, _) = q.quantize(downscale(rr, s) as f64);
                    bw.write(code, alloc[b] as u32);
                    let rec = q.dequantize(code);
                    let v_hat = reconstruct(pred, rec, s);
                    h = [v_hat, h[0]];
                }
                self.hist[ch][b] = h;
            }
        }
        let mut payload = bw.into_bytes();
        payload.resize(want, 0);

        let pkt = OtaPacket {
            ttp: (self.ttp4 / 4) as u16,
            period: self.cfg.period,
            packet_type: self.cfg.packet_type,
            channel_mode: self.cfg.channel_mode,
            session: 0,
            version: self.cfg.version,
            payload,
        };
        self.ttp4 = self.ttp4.wrapping_add(self.cfg.period as u64 * 15);
        self.frames = self.frames.wrapping_add(1);
        pkt
    }

    /// Encode one frame into a vendor-structure R2 packet.
    ///
    /// The DSP chain is the vendor-validated one ([`crate::vendor_core`]):
    /// 8-band cosine-modulated analysis, pole-zero sign-sign LMS prediction
    /// and the non-uniform adaptive quantiser, all bit-exact against stage
    /// vectors captured from the reference encoder.  The composer codes the
    /// quantiser codes, a pair per field being two consecutive blocks.
    fn encode_frame_vendor(&mut self, l: &[i32], r: &[i32]) -> OtaPacket {
        let calls = self.cfg.calls_per_frame;
        let shape = self.cfg.frame_mode.shape();

        // 1. Analysis bank: per block, per channel.
        let mut vals: [Vec<[i32; BANDS]>; 2] =
            [Vec::with_capacity(calls), Vec::with_capacity(calls)];
        for (ch, src) in [l, r].into_iter().enumerate() {
            for c in 0..calls {
                let mut blk = [0i32; 8];
                for k in 0..8 {
                    // the codec core works in signed 24-bit PCM; the C API
                    // receives full-scale S32
                    blk[k] = src[c * 8 + k] >> 8;
                }
                let mut o = [0i32; BANDS];
                self.analysis[ch].block(&blk, &mut o);
                vals[ch].push(o);
            }
        }

        // 2. Per-field energy feeds the width-allocation step: the vendor
        //    loudness model on the native path, the greedy search otherwise.
        let steps_n = (calls / 2).max(1);
        let mut xenergy = [0f64; 16];
        for ch in 0..2 {
            for c in 0..calls {
                for b in 0..BANDS {
                    let x = vals[ch][c][b] as f64;
                    xenergy[ch * BANDS + b] += x * x;
                }
            }
        }

        // 3. Width allocation.  Only this step changes: the frame writer,
        //    composer and quantiser below are untouched.
        //
        //    APTX_NATIVE_ALLOC=vendor selects the vendor R2 loudness
        //    allocator (`bitalloc_vendor.rs`, behind the `bitalloc-vendor`
        //    feature); APTX_NATIVE_ALLOC=greedy selects the historical native
        //    allocator.  Unset defaults to vendor when the module is compiled
        //    in and greedy otherwise; vendor always falls back to greedy if
        //    the module is absent.
        //
        //    The budget is the shape's composer region (2760 bits for LL,
        //    5224 for R2 HQ).  Config 4 cannot code a band above 9 bits,
        //    so the LL width space is capped before the writer sees it.
        let max_width: u8 = if shape.escapes { 16 } else { LL_MAX_WIDTH };
        let mut widths = match self.alloc_mode {
            AllocMode::Vendor => match vendor_widths(&xenergy, calls, shape.stream_bits()) {
                Some(w) => w,
                None => self.greedy_widths(&vals, &xenergy, calls, shape.stream_bits(), max_width),
            },
            AllocMode::Greedy => {
                self.greedy_widths(&vals, &xenergy, calls, shape.stream_bits(), max_width)
            }
        };
        for w in widths.iter_mut() {
            *w = (*w).min(max_width);
        }

        // 4. Quantise with the chosen allocation and fit the frame.  If the
        //    estimate was optimistic the writer keeps fewer steps than the
        //    frame holds; reduce the widest field and retry so that every
        //    input block is coded instead of silently dropped.
        let want = self.cfg.payload_size();
        debug_assert_eq!(want, shape.bytes, "payload size and frame shape disagree");
        let mut payload;
        let mut tries = 0usize;
        loop {
            let mut widths16 = [0u8; 16];
            let mut desc16 = [0u8; 16];
            for f in 0..16 {
                if widths[f] >= 2 {
                    widths16[f] = widths[f];
                    desc16[f] = widths[f];
                }
            }
            // Config 4 has no zero-allocation escape: an all-zero
            // allocation leaves the block count unencoded, and a reader
            // then decodes pairs until its 64-block cap.  Code one 2-bit
            // field (all-zero codes) so a silent LL frame still describes
            // exactly the 30 steps = 60 blocks it carries.
            if !shape.escapes && widths16.iter().all(|&w| w == 0) {
                widths16[0] = 2;
                desc16[0] = 2;
            }
            let mut bands = self.bands;
            let mut pred = self.pred;
            let mut seeds = self.seeds;
            let mut codes: [Vec<[i32; BANDS]>; 2] =
                [Vec::with_capacity(calls), Vec::with_capacity(calls)];
            for c in 0..calls {
                let mut qrow = [[0i32; BANDS]; 2];
                let mut rrow = [[0i32; BANDS]; 2];
                for ch in 0..2 {
                    for b in 0..BANDS {
                        let p = pred[ch][b].pr;
                        let (q, rec) =
                            bands[ch][b].quantise(vals[ch][c][b], p, widths16[ch * BANDS + b]);
                        qrow[ch][b] = q;
                        rrow[ch][b] = rec;
                    }
                    dither(&mut seeds[ch], qrow[ch][0], qrow[ch][1], &mut bands[ch]);
                    for b in 0..BANDS {
                        pred[ch][b].run(rrow[ch][b]);
                    }
                }
                codes[0].push(qrow[0]);
                codes[1].push(qrow[1]);
            }
            let mut steps: Vec<[[i32; 2]; 16]> = Vec::with_capacity(steps_n);
            for k in 0..steps_n {
                let t0 = 2 * k;
                let t1 = (2 * k + 1).min(calls - 1);
                let mut sv = [[0i32; 2]; 16];
                for f in 0..16 {
                    if widths16[f] == 0 {
                        continue;
                    }
                    let (ch, b) = (f / BANDS, f % BANDS);
                    sv[f] = [codes[ch][t0][b], codes[ch][t1][b]];
                }
                steps.push(sv);
            }
            payload = match self.cfg.frame_mode {
                FrameMode::R2 => {
                    self.framewriter
                        .write(&widths16, &desc16, &steps, &[31, 63], false)
                }
                // The LL frame is a single 60-block chunk (below the
                // 64-block cap), so it needs no differential update.
                FrameMode::Ll => self.framewriter.write_ll(&widths16, &desc16, &steps, false),
            };
            // commit the state that produced this payload
            self.bands = bands;
            self.pred = pred;
            self.seeds = seeds;
            self.last_widths = [0u8; BANDS];
            for ch in 0..2 {
                for b in 0..BANDS {
                    self.last_widths[b] = self.last_widths[b].max(widths16[ch * BANDS + b]);
                }
            }
            if self.framewriter.last_steps_written >= steps_n {
                break;
            }
            tries += 1;
            if tries >= 24 {
                break;
            }
            let mut widest = 0usize;
            for f in 1..16 {
                if widths[f] > widths[widest] {
                    widest = f;
                }
            }
            widths[widest] = if widths[widest] <= 2 {
                0
            } else {
                widths[widest] - 1
            };
        }
        debug_assert_eq!(payload.len(), want);

        let pkt = OtaPacket {
            ttp: (self.ttp4 / 4) as u16,
            period: self.cfg.period,
            packet_type: self.cfg.packet_type,
            channel_mode: self.cfg.channel_mode,
            session: 0,
            version: self.cfg.version,
            payload,
        };
        self.ttp4 = self.ttp4.wrapping_add(self.cfg.period as u64 * 15);
        self.frames = self.frames.wrapping_add(1);
        pkt
    }

    /// The native greedy width allocation (the historical encoder step).
    ///
    /// For every field and candidate width it runs the real quantiser and the
    /// real pair coder, giving exact composer bit counts and reconstruction
    /// error; it then raises the field with the best distortion reduction per
    /// bit until the packet's stream region is full.  State is copied, so the
    /// closed-loop quantiser history is untouched.
    fn greedy_widths(
        &self,
        vals: &[Vec<[i32; BANDS]>; 2],
        xenergy: &[f64; 16],
        calls: usize,
        stream_bits: usize,
        max_width: u8,
    ) -> [u8; 16] {
        let mut cost = [[0usize; 17]; 16];
        let mut dist = [[0f64; 17]; 16];
        for f in 0..16 {
            dist[f][0] = xenergy[f];
            let (ch, b) = (f / BANDS, f % BANDS);
            for w in 2..=max_width {
                let mut band = self.bands[ch][b];
                let mut pred = self.pred[ch][b];
                let mut bits = 0usize;
                let mut err = 0f64;
                let mut prev: Option<i32> = None;
                for c in 0..calls {
                    let x = vals[ch][c][b];
                    let p = pred.pr;
                    let (q, rec) = band.quantise(x, p, w);
                    let xhat = p.wrapping_mul(256).wrapping_add(rec);
                    let d = (x.wrapping_sub(xhat)) as f64;
                    err += d * d;
                    if let Some(q0) = prev {
                        bits += crate::composer::pair_bits(
                            w as usize,
                            (q0 as u32) & 0xffff,
                            (q as u32) & 0xffff,
                        )
                        .len();
                    }
                    prev = Some(q);
                    pred.run(rec);
                }
                cost[f][w as usize] = bits;
                dist[f][w as usize] = err;
            }
        }

        // Greedy allocation: raise the field that buys the most distortion
        // reduction per composer bit, within one packet's stream region.
        let mut widths = [0u8; 16];
        let mut used = 0usize;
        loop {
            let mut best: Option<(usize, usize, f64)> = None;
            for f in 0..16 {
                let w = widths[f];
                if w >= max_width {
                    continue;
                }
                let nw = if w == 0 { 2 } else { w + 1 };
                let extra = cost[f][nw as usize].saturating_sub(cost[f][w as usize]);
                if used + extra + HEAD_RESERVE > stream_bits {
                    continue;
                }
                let gain = dist[f][w as usize] - dist[f][nw as usize];
                if gain <= 0.0 {
                    continue;
                }
                let score = gain / (extra.max(1) as f64);
                if best.map_or(true, |(_, _, s)| score > s) {
                    best = Some((f, extra, score));
                }
            }
            match best {
                Some((f, extra, _)) => {
                    widths[f] = if widths[f] == 0 { 2 } else { widths[f] + 1 };
                    used += extra;
                }
                None => break,
            }
        }
        widths
    }
}

/// Width-allocation backend for vendor frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AllocMode {
    /// Vendor R2 loudness allocation (bitalloc_vendor.rs).
    Vendor,
    /// Native greedy cost/distortion search (the historical allocator).
    Greedy,
}

/// The vendor allocator module is compiled in unconditionally.
const VENDOR_ALLOC_AVAILABLE: bool = true;

/// Parse APTX_NATIVE_ALLOC (vendor | greedy, case-insensitive).
///
/// Unset, empty or unrecognised values select the build default: vendor when
/// the module is compiled in, greedy otherwise.  A vendor request on a build
/// without the module also degrades to greedy.
fn parse_alloc_mode(raw: Option<&str>, vendor_available: bool) -> AllocMode {
    match raw.map(str::trim) {
        Some(s) if s.eq_ignore_ascii_case("greedy") => AllocMode::Greedy,
        Some(s) if s.eq_ignore_ascii_case("vendor") => {
            if vendor_available {
                AllocMode::Vendor
            } else {
                AllocMode::Greedy
            }
        }
        _ => {
            if vendor_available {
                AllocMode::Vendor
            } else {
                AllocMode::Greedy
            }
        }
    }
}

/// Parse APTX_NATIVE_LL: 1/true/yes/on (case-insensitive) enables the
/// 348-byte LL frame; unset, empty or anything else keeps the config.
fn parse_ll_env(raw: Option<&str>) -> bool {
    match raw.map(str::trim) {
        Some(s) => matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        None => false,
    }
}

/// Resolve the stream's frame mode from the environment.
fn resolve_frame_mode(cfg: Config) -> Config {
    let raw = std::env::var("APTX_NATIVE_LL").ok();
    apply_frame_mode(cfg, parse_ll_env(raw.as_deref()))
}

/// Apply the LL selection (environment or direct) to a config.
///
/// LL has exactly one geometry at 48 kHz: 60 calls (480 samples, 10 ms),
/// payload type 0 (348 bytes) and period byte 40.  A config asking for LL
/// is normalized to it, with a message for each value that had to change,
/// so a caller cannot accidentally pair the LL writer with a 25 ms cadence
/// or the 656-byte payload type.
fn apply_frame_mode(mut cfg: Config, ll: bool) -> Config {
    if ll && cfg.frame_mode != FrameMode::Ll {
        eprintln!(
            "aptx-native: APTX_NATIVE_LL selects the 348-byte LL frame \
             (60 calls = 480 samples = 10 ms at 48 kHz)"
        );
        cfg.frame_mode = FrameMode::Ll;
    }
    if cfg.frame_mode == FrameMode::Ll {
        if !cfg.vendor_frame {
            eprintln!(
                "aptx-native: the LL frame is a vendor-structure frame; enabling vendor_frame"
            );
            cfg.vendor_frame = true;
        }
        if cfg.calls_per_frame != LL_BLOCKS {
            eprintln!(
                "aptx-native: LL frame needs {LL_BLOCKS} calls/frame (480 samples); overriding calls_per_frame={}",
                cfg.calls_per_frame
            );
            cfg.calls_per_frame = LL_BLOCKS;
        }
        if cfg.packet_type != 0 {
            eprintln!(
                "aptx-native: LL frame is payload type 0 (348 bytes); overriding packet_type={}",
                cfg.packet_type
            );
            cfg.packet_type = 0;
        }
        if cfg.period != LL_PERIOD {
            eprintln!(
                "aptx-native: LL frame is 10 ms (period byte {LL_PERIOD}); overriding period={}",
                cfg.period
            );
            cfg.period = LL_PERIOD;
        }
    }
    cfg
}

/// Call the vendor R2 loudness allocator, returning the 16 field widths.
///
/// Contract with bitalloc_vendor.rs (the allocator owner): the entry points
/// return one width per composer field in the allocation writer's narrowband
/// space (0 = inactive, 2..=16 = active).  This bridge passes the per-field
/// energy of the frame's analysis values, the block count, and the payload's
/// composer stream budget in bits.  When the feature is off this is None and
/// the caller falls back to the greedy search.
fn vendor_widths(energy: &[f64; 16], calls: usize, stream_bits: usize) -> Option<[u8; 16]> {
    Some(sanitize_widths(crate::bitalloc_vendor::alloc_ull(
        energy,
        calls,
        stream_bits,
    )))
}

/// Coerce allocator output into the narrowband writer's width space: 0 means
/// inactive, 2..=16 are the widths the allocation sequence can code (width 1
/// codes as 0 and is therefore treated as inactive), and anything above 16 is
/// saturated.
fn sanitize_widths(widths: [u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (o, &w) in out.iter_mut().zip(widths.iter()) {
        *o = match w {
            0 | 1 => 0,
            2..=16 => w,
            _ => 16,
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ll_env_switch() {
        for on in ["1", "true", "TRUE", "yes", "on", " on "] {
            assert!(parse_ll_env(Some(on)), "{on:?} enables LL");
        }
        for off in ["", "0", "false", "no", "off", "bogus", "2"] {
            assert!(!parse_ll_env(Some(off)), "{off:?} keeps R2");
        }
        assert!(!parse_ll_env(None));
    }

    #[test]
    fn ll_config_is_normalized() {
        // Config::ll already carries the LL geometry: nothing changes.
        let ll = Config::ll();
        assert_eq!(ll.frame_mode, FrameMode::Ll);
        assert_eq!(ll.calls_per_frame, LL_BLOCKS);
        assert_eq!(ll.packet_type, 0);
        assert_eq!(ll.period, LL_PERIOD);
        assert_eq!(ll.payload_size(), LL_PAYLOAD_BYTES);
        let same = apply_frame_mode(ll.clone(), false);
        assert_eq!(same.calls_per_frame, LL_BLOCKS);
        assert_eq!(same.packet_type, 0);
        assert_eq!(same.period, LL_PERIOD);

        // The env switch turns a default (656-byte) config into LL and
        // replaces the 25 ms geometry instead of pairing LL with it.
        let default = Config {
            vendor_frame: true,
            ..Config::default()
        };
        let switched = apply_frame_mode(default, true);
        assert_eq!(switched.frame_mode, FrameMode::Ll);
        assert_eq!(switched.calls_per_frame, LL_BLOCKS);
        assert_eq!(switched.packet_type, 0);
        assert_eq!(switched.period, LL_PERIOD);
        assert!(switched.vendor_frame);
        assert_eq!(switched.payload_size(), 348);

        // Without the switch the R2 geometry is untouched.
        let r2 = apply_frame_mode(
            Config {
                vendor_frame: true,
                ..Config::default()
            },
            false,
        );
        assert_eq!(r2.frame_mode, FrameMode::R2);
        assert_eq!(r2.calls_per_frame, 150);
        assert_eq!(r2.payload_size(), 656);
    }

    #[test]
    fn frame_mode_shapes() {
        assert_eq!(FrameMode::R2.shape().bytes, 656);
        assert_eq!(FrameMode::R2.shape().frame_type, 3);
        assert!(FrameMode::R2.shape().escapes);
        assert!(!FrameMode::R2.is_ll());
        assert_eq!(FrameMode::Ll.shape().bytes, 348);
        assert_eq!(FrameMode::Ll.shape().frame_type, 4);
        assert_eq!(FrameMode::Ll.shape().length_byte(), 0x54);
        assert_eq!(FrameMode::Ll.shape().stream_bytes(), 345);
        assert_eq!(FrameMode::Ll.shape().stream_bits(), 2760);
        assert!(!FrameMode::Ll.shape().escapes);
        assert!(FrameMode::Ll.is_ll());
        assert_eq!(LL_ON_AIR_BYTES, 360);
        assert!(LL_TTP_MIN_UNITS < LL_TTP_MAX_UNITS);
        assert_eq!(LL_TTP_START_UNITS, LL_TTP_MIN_UNITS);
    }

    #[test]
    fn alloc_mode_env_switch() {
        // default: vendor when the module exists, greedy otherwise
        assert_eq!(parse_alloc_mode(None, true), AllocMode::Vendor);
        assert_eq!(parse_alloc_mode(None, false), AllocMode::Greedy);
        // explicit values win, case-insensitively
        assert_eq!(parse_alloc_mode(Some("greedy"), true), AllocMode::Greedy);
        assert_eq!(parse_alloc_mode(Some("GREEDY"), true), AllocMode::Greedy);
        assert_eq!(parse_alloc_mode(Some("vendor"), true), AllocMode::Vendor);
        assert_eq!(parse_alloc_mode(Some(" vendor "), true), AllocMode::Vendor);
        // vendor without the module degrades to greedy instead of failing
        assert_eq!(parse_alloc_mode(Some("vendor"), false), AllocMode::Greedy);
        // unknown values keep the build default
        assert_eq!(parse_alloc_mode(Some("bogus"), true), AllocMode::Vendor);
        assert_eq!(parse_alloc_mode(Some("bogus"), false), AllocMode::Greedy);
    }

    #[test]
    fn vendor_widths_enter_the_writer_space() {
        let mut w = [0u8; 16];
        w[0] = 1; // codes as inactive
        w[1] = 2;
        w[2] = 16;
        w[3] = 17; // out of range
        w[4] = 255;
        let s = sanitize_widths(w);
        assert_eq!(s[0], 0);
        assert_eq!(s[1], 2);
        assert_eq!(s[2], 16);
        assert_eq!(s[3], 16);
        assert_eq!(s[4], 16);
        assert!(s.iter().all(|&x| x == 0 || (2..=16).contains(&x)));
    }
}

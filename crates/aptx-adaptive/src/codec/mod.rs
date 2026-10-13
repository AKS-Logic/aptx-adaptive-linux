//! The core encoder (spec `codec-core.md`): filterbank, quantiser, predictor and packer.

pub mod alloc;
mod analysis;
mod arith;
mod bits;
#[cfg(test)]
mod decode;
pub mod pack;
pub mod packetiser;
mod predict;
mod quant;
#[cfg(test)]
mod stages;
mod tables;

use analysis::{Analysis, Cosine8, Tree4};
use predict::Predictor;
use quant::{Band, Mode};

pub const CHANNELS: usize = 2;
/// Most subbands per channel of any variant.
pub const MAX_SUBBANDS: usize = 8;
/// Most values per block.
pub const MAX_WIDTH: usize = MAX_SUBBANDS * CHANNELS;

/// The bitstream variant (§1). Config 2 (speech) isn't supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// R1 HQ.
    Cfg0,
    /// R1 LL, "low-power" mode.
    Cfg1,
    /// R2 HQ.
    Cfg3,
    /// R2 LL, 44.1/48 kHz only (§10.F).
    Cfg4,
}

/// What differs between variants.
struct Descriptor {
    subbands: usize,
    /// Packet byte 1 (§7.1).
    frame_type: u8,
    low_power: bool,
    /// 128-tap filterbank prototype instead of 256 (8 bands only).
    short_proto: bool,
    /// Config 3's allocation escapes, zero-allocation escape and digital-black step; config 4
    /// codes allocations like configs 0/1 (§10.F).
    escapes: bool,
    /// Zero taps per band, 0 = predictor off (§6), at 44.1/48 and 88.2/96 kHz.
    taps: [&'static [usize]; 2],
}

const CFG0: Descriptor = Descriptor {
    subbands: 4,
    frame_type: 1,
    low_power: false,
    short_proto: false,
    escapes: false,
    taps: [&tables::TAPS_4BAND; 2],
};

const CFG1: Descriptor = Descriptor {
    frame_type: 2,
    low_power: true,
    ..CFG0
};

const CFG3: Descriptor = Descriptor {
    subbands: 8,
    frame_type: 3,
    low_power: false,
    short_proto: false,
    escapes: true,
    taps: [&tables::TAPS_8BAND_48K, &tables::TAPS_8BAND_96K],
};

const CFG4: Descriptor = Descriptor {
    frame_type: 4,
    short_proto: true,
    escapes: false,
    ..CFG3
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub variant: Variant,
    pub sample_rate: u32,
}

impl Params {
    fn desc(self) -> &'static Descriptor {
        match self.variant {
            Variant::Cfg0 => &CFG0,
            Variant::Cfg1 => &CFG1,
            Variant::Cfg3 => &CFG3,
            Variant::Cfg4 => &CFG4,
        }
    }

    pub fn subbands(self) -> usize {
        self.desc().subbands
    }

    /// Values per block: subbands of both channels.
    pub fn width(self) -> usize {
        self.subbands() * CHANNELS
    }

    fn low_power(self) -> bool {
        self.desc().low_power
    }

    /// Config 3's allocation escapes and digital-black step (§7.2, §7.3, §8.6 step 5).
    pub fn escapes(self) -> bool {
        self.desc().escapes
    }

    fn high_rate(self) -> usize {
        usize::from(self.sample_rate > 48_000)
    }

    pub fn frame_type(self) -> u8 {
        self.desc().frame_type
    }

    /// The 4-bit absolute allocation header (§7.2), stereo only.
    pub fn alloc_header(self) -> u32 {
        let stereo = 8;
        if self.subbands() == 4 {
            let rate = if self.sample_rate == 48_000 { 2 } else { 0 };
            rate + u32::from(self.low_power()) + stereo
        } else {
            let rate = match self.sample_rate {
                44_100 => 0,
                88_200 => 4,
                96_000 => 6,
                _ => 2,
            };
            rate + stereo
        }
    }

    fn taps(self) -> &'static [usize] {
        self.desc().taps[self.high_rate()]
    }
}

#[derive(Debug, Clone, Copy)]
struct Channel {
    analysis: Analysis,
    bands: [Band; MAX_SUBBANDS],
    predictors: [Option<Predictor>; MAX_SUBBANDS],
    seed: i32,
}

/// Signal path state for both channels: everything before the packer. `Copy`, so a trial
/// copy is a plain memcpy.
#[derive(Debug, Clone, Copy)]
pub struct Engine {
    params: Params,
    channels: [Channel; CHANNELS],
}

/// One block's intermediate values, each `width()` long in list order.
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub struct BlockTrace {
    pub codes: Vec<i32>,
    pub recon: Vec<i32>,
    pub pred_out: Vec<i32>,
}

impl Engine {
    pub fn new(params: Params) -> Self {
        let s = params.subbands();
        let channel = |ch: usize| {
            let mut predictors = [None; MAX_SUBBANDS];
            for (p, &n) in predictors.iter_mut().zip(params.taps()) {
                let off = n == 0 || (params.low_power() && ch > 0);
                *p = (!off).then(|| Predictor::new(n));
            }
            Channel {
                analysis: if s == 4 {
                    Analysis::Tree4(Tree4::new(params.low_power()))
                } else {
                    Analysis::Cosine8(Cosine8::new(params.desc().short_proto))
                },
                bands: [Band::default(); MAX_SUBBANDS],
                predictors,
                seed: 0,
            }
        };
        Self {
            params,
            channels: [channel(0), channel(1)],
        }
    }

    pub fn params(&self) -> Params {
        self.params
    }

    /// Runs the filterbank on one block of interleaved PCM (`subbands()` frames) and writes
    /// `width()` subband values.
    pub fn analyse(&mut self, pcm: &[i32], x: &mut [i32]) {
        let s = self.params.subbands();
        let mut buf = [0i32; MAX_SUBBANDS];
        for (ch, c) in self.channels.iter_mut().enumerate() {
            for (k, b) in buf[..s].iter_mut().enumerate() {
                *b = pcm[k * CHANNELS + ch];
            }
            c.analysis.block(&buf[..s], &mut x[ch * s..(ch + 1) * s]);
        }
    }

    /// Quantises one block of subband values with allocation `bits`, then updates dither and
    /// predictors. Writes the codes to `q` and returns the reconstructions.
    #[expect(clippy::many_single_char_names, reason = "names follow the spec")]
    pub fn code(&mut self, x: &[i32], bits: &[u8], q: &mut [i32]) -> [i32; MAX_WIDTH] {
        let s = self.params.subbands();
        let normal = !self.params.low_power();
        let mut recon = [0i32; MAX_WIDTH];
        let mut mode = if normal {
            Mode::Normal
        } else {
            Mode::LowPower {
                carry: 0,
                top: false,
            }
        };
        for (ch, c) in self.channels.iter_mut().enumerate() {
            for k in 0..s {
                let i = ch * s + k;
                let p = c.predictors[k].as_ref().map_or(0, |p| p.pr);
                if let Mode::LowPower { top, .. } = &mut mode {
                    *top = k == s - 1;
                }
                (q[i], recon[i]) = c.bands[k].quantise(x[i], p, bits[i], &mut mode);
            }
            if normal {
                quant::dither(&mut c.seed, q[ch * s], q[ch * s + 1], &mut c.bands[..s]);
            }
            for (k, p) in c.predictors[..s].iter_mut().enumerate() {
                if let Some(p) = p {
                    p.run(recon[ch * s + k]);
                }
            }
        }
        recon
    }

    /// [`Engine::code`], keeping every intermediate value.
    #[cfg(test)]
    pub fn code_traced(&mut self, x: &[i32], bits: &[u8], q: &mut [i32]) -> BlockTrace {
        let w = self.params.width();
        let s = self.params.subbands();
        let recon = self.code(x, bits, q);
        BlockTrace {
            codes: q[..w].to_vec(),
            recon: recon[..w].to_vec(),
            pred_out: self
                .channels
                .iter()
                .flat_map(|c| {
                    c.predictors[..s]
                        .iter()
                        .map(|p| p.as_ref().map_or(0, |p| p.pr))
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_headers() {
        let p = |variant, sample_rate| Params {
            variant,
            sample_rate,
        };
        assert_eq!(p(Variant::Cfg3, 96_000).alloc_header(), 0xE);
        assert_eq!(p(Variant::Cfg1, 48_000).alloc_header(), 0xB);
        assert_eq!(p(Variant::Cfg4, 48_000).alloc_header(), 0xA);
        assert_eq!(p(Variant::Cfg4, 44_100).alloc_header(), 0x8);
    }
}

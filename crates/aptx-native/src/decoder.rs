//! The matching decoder: OTA packets in, PCM out.
//!
//! Reads exactly the payload form [`crate::encoder::Encoder`] produces
//! (see that module's docs): header (magic, counter, calls, per-band
//! allocation + scale), then per band the L and R quantiser codes.
//! Prediction is closed-loop with the same fixed o2 predictor, and the
//! subbands are synthesised by the CMFB bank transcribed from the shipped
//! decoder.
//!
//! This is the reference for the end-to-end test; it is also the seed of a
//! standalone native decoder for real streams (which additionally needs the
//! vendor's Huffman trees — an open item).
//!
//! SPDX-License-Identifier: MIT

use crate::cmfb::Cmfb;
use crate::encoder::{Config, PAYLOAD_MAGIC};
use crate::entropy::BitReader;
use crate::packer::OtaPacket;
use crate::quant::Quantizer;
use crate::BANDS;

/// Decoder state.
pub struct Decoder {
    cmfb: [Cmfb; 2],
    hist: [[[i32; 2]; BANDS]; 2],
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new(true)
    }
}

impl Decoder {
    /// Create a decoder for the given window family.
    pub fn new(long_window: bool) -> Self {
        Decoder {
            cmfb: [Cmfb::new(long_window), Cmfb::new(long_window)],
            hist: [[[0; 2]; BANDS]; 2],
        }
    }

    /// Decode one packet into per-channel PCM (interleaved by channel:
    /// `[left, right]`).
    pub fn decode(&mut self, pkt: &OtaPacket) -> Option<[Vec<i32>; 2]> {
        let p = &pkt.payload;
        let mut r = BitReader::new(p);
        if r.read(8)? != PAYLOAD_MAGIC as u32 {
            return None;
        }
        let _counter = r.read(16)?;
        let calls = r.read(16)? as usize;
        let mut alloc = [0u8; BANDS];
        for a in alloc.iter_mut() {
            *a = r.read(8)? as u8;
        }
        let mut shifts = [0i32; BANDS];
        for s in shifts.iter_mut() {
            *s = r.read(8)? as u8 as i8 as i32;
        }

        let mut vals: [Vec<[i32; BANDS]>; 2] =
            [vec![[0i32; BANDS]; calls], vec![[0i32; BANDS]; calls]];

        for b in 0..BANDS {
            if alloc[b] == 0 {
                continue;
            }
            let q = Quantizer::new(alloc[b]);
            let shift = shifts[b];
            for ch in 0..2 {
                let mut h = self.hist[ch][b];
                for c in 0..calls {
                    let code = r.read(alloc[b] as u32)?;
                    let rec = q.dequantize(code);
                    let pred = 2 * h[0] - h[1];
                    let v_hat = crate::encoder::reconstruct(pred, rec, shift);
                    vals[ch][c][b] = v_hat;
                    h = [v_hat, h[0]];
                }
                self.hist[ch][b] = h;
            }
        }

        let mut pcm: [Vec<i32>; 2] = [Vec::with_capacity(calls * 8), Vec::with_capacity(calls * 8)];
        for ch in 0..2 {
            for c in 0..calls {
                let o = self.cmfb[ch].synth(&vals[ch][c]);
                pcm[ch].extend_from_slice(&o);
            }
        }
        Some(pcm)
    }
}

/// Convenience: decode a serialized OTA packet (header + payload).
pub fn decode_packet(dec: &mut Decoder, bytes: &[u8]) -> Option<[Vec<i32>; 2]> {
    let pkt = OtaPacket::parse(bytes)?;
    dec.decode(&pkt)
}

/// Build a default config matching the decoder's window family (used by
/// tests and tools).
pub fn default_config_for(long_window: bool) -> Config {
    Config {
        long_window,
        ..Config::default()
    }
}

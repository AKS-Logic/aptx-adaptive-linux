//! A stand-in codec core for testing the packet loop until the real one exists.

use std::collections::HashMap;

use crate::stream::table_row;
use crate::{Config, Core, Error, Progress, Result, Stream, WINDOW_SLACK};

/// Takes input until it holds one packet's worth, then waits for a flush, which is how the
/// reference behaves in steady state (spec §6). A packet is the low bytes of its first two
/// input samples, then zeros, at the table packet size.
#[derive(Debug)]
pub struct Fake {
    capacity: usize,
    buffered: usize,
    first: [i32; 2],
    /// Packet index → buffered samples at which that packet comes out without a flush.
    pub early: HashMap<usize, usize>,
    /// Per packet out: whether it took a flush.
    pub forced: Vec<bool>,
    pub bitrates: Vec<u32>,
}

/// A stream over a [`Fake`] sized for `config`.
///
/// # Errors
/// `UnsupportedConfig` if there's no table row.
pub fn stream(config: Config, budget: usize) -> Result<Stream<Fake>> {
    let row = table_row(config, budget).ok_or(Error::UnsupportedConfig)?;
    let fake = Fake {
        capacity: row.interleaved_samples(config.sample_rate, config.channels.into()),
        buffered: 0,
        first: [0; 2],
        early: HashMap::new(),
        forced: Vec::new(),
        bitrates: Vec::new(),
    };
    Ok(Stream::with_core(fake, config, row))
}

impl Fake {
    fn packet(&mut self, window: &mut [u8], forced: bool) -> usize {
        let size = window.len() - WINDOW_SLACK;
        window[..size].fill(0);
        window[0] = self.first[0] as u8;
        window[1] = self.first[1] as u8;
        self.buffered = 0;
        self.forced.push(forced);
        size
    }
}

impl Core for Fake {
    fn encode_packet(&mut self, pcm: &[i32], window: &mut [u8]) -> Result<Progress> {
        if self.buffered == 0
            && let [a, b, ..] = *pcm
        {
            self.first = [a, b];
        }
        let early = self.early.get(&self.forced.len()).copied();
        let consumed = pcm
            .len()
            .min(early.unwrap_or(self.capacity) - self.buffered);
        self.buffered += consumed;
        let written = if early == Some(self.buffered) {
            self.packet(window, false)
        } else {
            0
        };
        Ok(Progress { consumed, written })
    }

    fn flush(&mut self, window: &mut [u8]) -> Result<usize> {
        Ok(self.packet(window, true))
    }

    fn set_bitrate(&mut self, bps: u32) {
        self.bitrates.push(bps);
    }
}

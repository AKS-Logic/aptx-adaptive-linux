//! One native aptX Adaptive stream: the port of the C plugin's struct impl.
//!
//! The encoder itself is aptx-native's C ABI, the same one libaptx_native
//! exposes to a2dp-codec-aptx-adaptive.c. One handle per stream; the plugin
//! feeds exactly one frame of interleaved stereo S32 per encode() call.
#![allow(
    clippy::doc_markdown,
    reason = "the C ABI names and field names are used as prose"
)]

use std::ffi::OsString;
use std::io::Write as _;
use std::sync::Mutex;

// The codec backend is the workspace's Rust `aptx-native` crate (the same C
// ABI the C plugin used), so no external native library is needed to build.
use aptx_native::capi::{
    NativeEncoder, aptx_native_encoder_encode, aptx_native_encoder_free, aptx_native_encoder_new,
    aptx_native_last_blocks,
};

use crate::ffi::{
    EINVAL, EIO, ENOSPC, NEED_FLUSH_ALL, NEED_FLUSH_NO, SPA_AUDIO_FORMAT_S32_LE,
    SPA_MEDIA_SUBTYPE_RAW, SPA_MEDIA_TYPE_AUDIO, spa_audio_info,
};

use super::env;
use super::mode::{
    BODY_SIZE, Mode, OFF_RATE_SOURCE, SR_44100, SR_48000, SR_96000, SR_192000, SR_MASK, ST_MASK,
};

/// The C plugin's struct rtp_header: a fixed 12-byte RTP header.
pub(crate) const RTP_HEADER_LEN: usize = 12;
pub(crate) const RTP_PAYLOAD_TYPE: u8 = 96;

/// Encoder delay in samples at the input rate (codec_get_delay).
pub(crate) const ENCODER_DELAY_SAMPLES: u32 = 48;

/// Serializes the process-wide APTX_NATIVE_* writes around handle creation.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct Native(*mut NativeEncoder);

impl Drop for Native {
    fn drop(&mut self) {
        // SAFETY: the pointer came from aptx_native_encoder_new and is freed once.
        unsafe { aptx_native_encoder_free(self.0) };
    }
}

#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

unsafe extern "C" {
    fn clock_gettime(clk_id: i32, tp: *mut Timespec) -> i32;
}

const CLOCK_MONOTONIC: i32 = 1;

fn monotonic_us() -> u64 {
    let mut ts = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: CLOCK_MONOTONIC is valid and ts is a writable timespec.
    let rc = unsafe { clock_gettime(CLOCK_MONOTONIC, &raw mut ts) };
    if rc != 0 {
        return 0;
    }
    (ts.tv_sec as u64)
        .saturating_mul(1_000_000)
        .saturating_add((ts.tv_nsec as u64) / 1000)
}

// std::env::set_var and remove_var are unsafe in edition 2024 because another
// thread may read the environment concurrently. Every caller holds ENV_LOCK
// and the encoder reads these variables inside the same critical section,
// mirroring the C plugin's setenv-then-create sequence.
fn set_var(key: &str, value: &str) {
    // SAFETY: see the comment above.
    unsafe { std::env::set_var(key, value) };
}

fn set_var_os(key: &str, value: &OsString) {
    // SAFETY: see the comment above.
    unsafe { std::env::set_var(key, value) };
}

fn unset_var(key: &str) {
    // SAFETY: see the comment above.
    unsafe { std::env::remove_var(key) };
}

fn restore(key: &str, value: Option<OsString>) {
    match value {
        Some(value) => set_var_os(key, &value),
        None => unset_var(key),
    }
}

/// Create the native encoder for one mode/rate, with the C plugin's env
/// contract: APTX_NATIVE_CALLS, APTX_NATIVE_LL and APTX_NATIVE_NO_CONTAINER
/// are read once by the handle constructor.
fn native_start(mode: Mode, rate: u32, mtu: usize, container: u32) -> Result<Native, i32> {
    let Some(format) = mode.format(rate) else {
        return Err(-EINVAL);
    };
    let calls = format.native_calls.to_string();
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let saved_calls = std::env::var_os("APTX_NATIVE_CALLS");
    let saved_ll = std::env::var_os("APTX_NATIVE_LL");
    let saved_no_container = std::env::var_os("APTX_NATIVE_NO_CONTAINER");

    set_var("APTX_NATIVE_CALLS", &calls);
    if mode == Mode::Ll {
        set_var("APTX_NATIVE_LL", "1");
    } else {
        unset_var("APTX_NATIVE_LL");
    }
    if container != 0 {
        unset_var("APTX_NATIVE_NO_CONTAINER");
    } else {
        set_var("APTX_NATIVE_NO_CONTAINER", "1");
    }

    let handle = aptx_native_encoder_new(rate, mtu as u32);

    restore("APTX_NATIVE_CALLS", saved_calls);
    restore("APTX_NATIVE_LL", saved_ll);
    restore("APTX_NATIVE_NO_CONTAINER", saved_no_container);

    if handle.is_null() {
        eprintln!(
            "aptx-adaptive: native encoder rejected rate={rate} ({}); \
             libaptx_native supports 44100/48000 only",
            mode.name().to_string_lossy()
        );
        Err(-EIO)
    } else {
        Ok(Native(handle))
    }
}

fn rate_from_bits(bits: u8) -> u32 {
    match bits & SR_MASK {
        SR_44100 => 44_100,
        SR_48000 => 48_000,
        SR_96000 => 96_000,
        SR_192000 => 192_000,
        _ => 0,
    }
}

/// The TTP/RTP experiment knobs, with the C plugin's defaults.
struct Ttp {
    lead_ms: u32,
    rtp_us: bool,
}

fn parse_ttp_env(mode: Mode) -> Ttp {
    let (min_ms, max_ms) = mode.ttp_window_ms();
    let mut lead = u32::from(mode.ttp_lead_ms(false));
    if let Some(value) = env::value_u32("APTX_ADAPTIVE_TTP_LEAD_MS") {
        if value >= u32::from(min_ms) && value <= u32::from(max_ms) {
            lead = value;
        } else {
            let _ = writeln!(
                std::io::stderr(),
                "aptx-adaptive: ignoring APTX_ADAPTIVE_TTP_LEAD_MS={value} outside {} \
                 window {min_ms}-{max_ms} ms",
                mode.name().to_string_lossy()
            );
        }
    }
    if let Some(value) = std::env::var_os("APTX_ADAPTIVE_TTP_MODE") {
        match value.to_str() {
            Some("us" | "us_wall" | "audio" | "pass") | None => {}
            Some(other) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "aptx-adaptive: ignoring APTX_ADAPTIVE_TTP_MODE={other}"
                );
            }
        }
    }
    Ttp {
        lead_ms: lead,
        rtp_us: env::equals("APTX_ADAPTIVE_RTP_TS", "us"),
    }
}

/// The port of struct impl (one stream).
pub(crate) struct Session {
    native: Native,
    mode: Mode,
    rate: u32,
    source_type: u8,
    container: u32,
    ttp_lead_ms: u32,
    rtp_us: bool,
    /// Source-type-0 microsecond time-to-play clock.
    st0_us: u32,
    st0_started: bool,
    seqnum: u16,
    /// The RTP header start_encode wrote; encode rewrites its timestamp.
    header: *mut u8,
    calls: u64,
    packets: u64,
    prev_wall_us: u64,
    prev_rtp: u32,
}

impl Session {
    pub(crate) fn open(
        mode: Mode,
        config: &[u8],
        info: &spa_audio_info,
        mtu: usize,
    ) -> Result<Self, i32> {
        if config.len() < BODY_SIZE {
            return Err(-EINVAL);
        }
        let raw_ok = info.media_type == SPA_MEDIA_TYPE_AUDIO
            && info.media_subtype == SPA_MEDIA_SUBTYPE_RAW
            && info.raw.format == SPA_AUDIO_FORMAT_S32_LE;
        if !raw_ok {
            return Err(-EINVAL);
        }

        let mut rate = info.raw.rate;
        if rate == 0 {
            rate = rate_from_bits(config[OFF_RATE_SOURCE]);
        }
        if mode.format(rate).is_none() {
            return Err(-EINVAL);
        }
        let source_type = config[OFF_RATE_SOURCE] & ST_MASK;
        let container = env::u32("APTX_ADAPTIVE_CONTAINER", 0);
        let ttp = parse_ttp_env(mode);
        let native = native_start(mode, rate, mtu, container)?;

        if env::is_set("APTX_ADAPTIVE_DEBUG") {
            eprintln!(
                "aptx-adaptive: init mode={} rate={rate} source_type={source_type} \
                 container={container} ttp_lead={} samples={} payload={} frame={}",
                mode.name().to_string_lossy(),
                ttp.lead_ms,
                mode.frame_samples(rate),
                mode.codec_bytes(rate),
                mode.frame_bytes(rate),
            );
        }

        Ok(Self {
            native,
            mode,
            rate,
            source_type,
            container,
            ttp_lead_ms: ttp.lead_ms,
            rtp_us: ttp.rtp_us,
            st0_us: 0,
            st0_started: false,
            seqnum: 0,
            header: std::ptr::null_mut(),
            calls: 0,
            packets: 0,
            prev_wall_us: 0,
            prev_rtp: 0,
        })
    }

    #[must_use]
    pub(crate) fn block_size(&self) -> usize {
        self.mode.block_bytes(self.rate).unwrap_or(0)
    }

    #[must_use]
    pub(crate) fn frame_samples(&self) -> u32 {
        self.mode.frame_samples(self.rate)
    }

    /// Write the RTP header; returns its length or a negated errno.
    pub(crate) fn start_encode(
        &mut self,
        dst: &mut [u8],
        seqnum: u16,
        timestamp: u32,
    ) -> Result<usize, i32> {
        self.seqnum = seqnum;
        let mut timestamp = timestamp;
        if self.rtp_us {
            // Source type 0: the RTP timestamp is an absolute microsecond
            // time-to-play. A host has no link clock, so use monotonic time
            // plus the mode's lead; the sink anchors on the first packet.
            timestamp = (monotonic_us() + u64::from(self.ttp_lead_ms) * 1000) as u32;
        } else if env::is_set("APTX_ADAPTIVE_TS_CONSTANT") {
            let samples = self.frame_samples();
            if samples != 0 {
                timestamp = samples;
            }
        }
        if dst.len() < RTP_HEADER_LEN {
            return Err(-ENOSPC);
        }
        let ssrc = env::u32(
            "APTX_ADAPTIVE_SSRC",
            self.mode.ssrc(self.source_type, false),
        );
        dst[0] = 0x80; // V=2, P=0, X=0, CC=0
        dst[1] = RTP_PAYLOAD_TYPE;
        dst[2..4].copy_from_slice(&seqnum.to_be_bytes());
        dst[4..8].copy_from_slice(&timestamp.to_be_bytes());
        dst[8..12].copy_from_slice(&ssrc.to_be_bytes());
        self.header = dst.as_mut_ptr();
        Ok(RTP_HEADER_LEN)
    }

    /// Consume one frame of interleaved stereo S32 and emit the codec payload.
    ///
    /// Returns the number of source bytes consumed, matching the C plugin:
    /// a full frame is always consumed even when the encoder buffers it.
    #[allow(
        clippy::too_many_lines,
        reason = "the C encode callback is one linear pass; splitting it hides the wire format"
    )]
    pub(crate) fn encode(
        &mut self,
        src: Option<&[u8]>,
        dst: &mut [u8],
        dst_out: &mut usize,
        need_flush: &mut i32,
    ) -> Result<usize, i32> {
        *dst_out = 0;
        let Some(src) = src else {
            *need_flush = NEED_FLUSH_ALL;
            return Ok(0);
        };
        let samples = self.frame_samples();
        if samples == 0 {
            return Err(-EINVAL);
        }
        // One block = one OTA frame. media-sink hands over whole PipeWire
        // quanta, so consume exactly one frame and leave the rest queued.
        let block = self.block_size();
        if src.len() < block {
            return Err(-EINVAL);
        }
        let src = &src[..block];

        self.calls = self.calls.saturating_add(1);
        // SAFETY: the native handle is live; src has samples stereo pairs;
        // dst is writable for dst.len() bytes. capi copies the packet or
        // returns -ENOSPC without writing past the end.
        let written = unsafe {
            aptx_native_encoder_encode(
                self.native.0,
                src.as_ptr().cast(),
                samples as usize,
                dst.as_mut_ptr(),
                dst.len(),
            )
        };

        if written < 0 {
            return Err(written as i32);
        }
        if written == 0 {
            *need_flush = NEED_FLUSH_NO;
            return Ok(block);
        }
        self.packets = self.packets.saturating_add(1);

        // The container-less frame is written whole; tiny returns are not a
        // codec packet and keep the source queued for the next call.
        if written <= 8 {
            *need_flush = NEED_FLUSH_NO;
            return Ok(block);
        }

        if self.rtp_us {
            let ts = self.next_st0_timestamp();
            self.write_timestamp(ts);
            let ttp = ((u64::from(ts) * 15) / 1000) as u16;
            // A container record holds the u16 TTP in its first two bytes.
            // The no-container form starts with the frame header and must not
            // be overwritten.
            if self.container != 0 && dst.len() >= 2 {
                dst[0] = (ttp & 0xff) as u8;
                dst[1] = (ttp >> 8) as u8;
            }
        }

        if env::is_set("APTX_ADAPTIVE_GRID_LOG") {
            self.log_grid(written);
        }

        *dst_out = written as usize;
        *need_flush = NEED_FLUSH_ALL;
        Ok(block)
    }

    /// Advance the source-type-0 clock by the frame's real audio duration.
    ///
    /// The native encoder reports the blocks (8 samples each) it actually
    /// wrote, which is authoritative: 60 blocks = 10 ms at 48 kHz, 96 blocks
    /// = 8 ms at 96 kHz, and the 48 kHz Hi-Res fallback reports 150 = 25 ms.
    fn next_st0_timestamp(&mut self) -> u32 {
        if !self.st0_started {
            let seed = Self::seed_st0();
            self.st0_us = (seed + u64::from(self.ttp_lead_ms) * 1000) as u32;
            self.st0_started = true;
        }
        let ts = self.st0_us;
        let samples = self.frame_samples();
        let mut step = env::u32("APTX_ADAPTIVE_RTP_STEP_US", 0);
        if step == 0 {
            step = (u64::from(samples) * 1_000_000 / u64::from(self.rate)) as u32;
        }
        let blocks = aptx_native_last_blocks();
        if blocks > 0 {
            step = (u64::from(blocks) * 8 * 1_000_000 / u64::from(self.rate)) as u32;
        }
        self.st0_us = ts.wrapping_add(step);
        ts
    }

    /// Seed the source-type-0 clock from the Bluetooth link clock when the
    /// optional APTX_ADAPTIVE_CLOCK_FILE ("<monotonic_us> <clock>") is
    /// present, otherwise from CLOCK_MONOTONIC.
    fn seed_st0() -> u64 {
        let Some(path) = std::env::var_os("APTX_ADAPTIVE_CLOCK_FILE") else {
            return monotonic_us();
        };
        if path.is_empty() {
            return monotonic_us();
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            return monotonic_us();
        };
        let mut fields = text.split_whitespace();
        let Some(mono) = fields.next().and_then(|s| s.parse::<u64>().ok()) else {
            return monotonic_us();
        };
        let clock = fields.next().and_then(|s| s.parse::<u64>().ok());
        let now = monotonic_us();
        let age = if clock.is_some() && mono != 0 && now > mono {
            now - mono
        } else {
            0
        };
        let clk = clock.unwrap_or(mono);
        if clk == 0 {
            return monotonic_us();
        }
        // Link clock ticks are 312.5 us.
        clk.saturating_mul(625) / 2 + age
    }

    fn write_timestamp(&mut self, timestamp: u32) {
        if self.header.is_null() {
            return;
        }
        // SAFETY: header points at the start of the dst slice handed to
        // start_encode; PipeWire keeps that buffer alive until the packet is
        // flushed, and encode runs before that.
        unsafe {
            std::ptr::copy_nonoverlapping(timestamp.to_be_bytes().as_ptr(), self.header.add(4), 4);
        }
    }

    fn log_grid(&mut self, payload: isize) {
        if self.header.is_null() {
            return;
        }
        let mut bytes = [0u8; 4];
        // SAFETY: the header is live and the timestamp field is 4 bytes.
        unsafe { std::ptr::copy_nonoverlapping(self.header.add(4), bytes.as_mut_ptr(), 4) };
        let rtp = u32::from_be_bytes(bytes);
        let now = monotonic_us();
        let d_rtp = rtp.wrapping_sub(self.prev_rtp) as i32;
        let d_wall = now.wrapping_sub(self.prev_wall_us) as i64;
        let audio_us = u64::from(self.frame_samples()) * 1_000_000 / u64::from(self.rate);
        let _ = writeln!(
            std::io::stderr(),
            "aptx-adaptive: grid pkt={} seq={} rtp={rtp} d_rtp={d_rtp} payload={payload} \
             wall={now} d_wall={d_wall} audio_us={audio_us} mode={}",
            self.packets,
            self.seqnum,
            self.mode.name().to_string_lossy(),
        );
        self.prev_rtp = rtp;
        self.prev_wall_us = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::{SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR};
    use crate::ported::mode::{CH_JOINT, FEATURES_UNKNOWN};

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    struct VarGuard {
        key: &'static str,
        saved: Option<OsString>,
    }

    impl VarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let saved = std::env::var_os(key);
            set_var(key, value);
            Self { key, saved }
        }

        fn unset(key: &'static str) -> Self {
            let saved = std::env::var_os(key);
            unset_var(key);
            Self { key, saved }
        }
    }

    impl Drop for VarGuard {
        fn drop(&mut self) {
            restore(self.key, self.saved.take());
        }
    }

    fn info(rate: u32) -> spa_audio_info {
        let mut raw = crate::ffi::spa_audio_info_raw {
            format: SPA_AUDIO_FORMAT_S32_LE,
            flags: 0,
            rate,
            channels: 2,
            position: [0; crate::ffi::SPA_AUDIO_MAX_CHANNELS],
        };
        raw.position[0] = SPA_AUDIO_CHANNEL_FL;
        raw.position[1] = SPA_AUDIO_CHANNEL_FR;
        spa_audio_info {
            media_type: SPA_MEDIA_TYPE_AUDIO,
            media_subtype: SPA_MEDIA_SUBTYPE_RAW,
            raw,
            _union_tail: [0],
        }
    }

    fn config(mode: Mode, rate: u32) -> Vec<u8> {
        mode.config(rate, 0, CH_JOINT, FEATURES_UNKNOWN)
            .expect("mode/rate has a body")
            .to_vec()
    }

    #[test]
    fn ll_and_hires_encode_real_native_frames() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _rtp = VarGuard::set("APTX_ADAPTIVE_RTP_TS", "us");
        let _ll = VarGuard::unset("APTX_NATIVE_LL");
        let _calls = VarGuard::unset("APTX_NATIVE_CALLS");
        let _nc = VarGuard::unset("APTX_NATIVE_NO_CONTAINER");
        let _step = VarGuard::unset("APTX_ADAPTIVE_RTP_STEP_US");

        for (mode, rate, bytes, step) in [
            (Mode::Ll, 48_000, 348usize, 10_000u32),
            (Mode::HiRes, 96_000, 656, 8_000),
            (Mode::HiRes, 48_000, 656, 25_000),
        ] {
            let cfg = config(mode, rate);
            let mut s = Session::open(mode, &cfg, &info(rate), 895).expect("native handle");
            assert_eq!(s.block_size(), (mode.frame_samples(rate) as usize) * 8);
            let mut dst = vec![0u8; 4096];
            assert_eq!(s.start_encode(&mut dst, 7, 1234), Ok(RTP_HEADER_LEN));
            assert_eq!(&dst[..2], [0x80, RTP_PAYLOAD_TYPE]);
            assert_eq!(u32::from_be_bytes(dst[8..12].try_into().unwrap()), 0xAE);
            let pcm = vec![0u8; s.block_size()];
            let (mut out, mut flush) = (0usize, -1);
            assert_eq!(
                s.encode(Some(&pcm), &mut dst[RTP_HEADER_LEN..], &mut out, &mut flush),
                Ok(s.block_size())
            );
            assert_eq!(out, bytes, "{mode:?}@{rate}");
            assert_eq!(flush, NEED_FLUSH_ALL);
            let ts1 = u32::from_be_bytes(dst[4..8].try_into().unwrap());

            s.start_encode(&mut dst, 8, 0).unwrap();
            let (mut out2, mut flush2) = (0usize, -1);
            s.encode(
                Some(&pcm),
                &mut dst[RTP_HEADER_LEN..],
                &mut out2,
                &mut flush2,
            )
            .unwrap();
            assert_eq!(out2, bytes);
            assert_eq!(flush2, NEED_FLUSH_ALL);
            let ts2 = u32::from_be_bytes(dst[4..8].try_into().unwrap());
            assert_eq!(ts2.wrapping_sub(ts1), step, "{mode:?}@{rate} RTP step");
        }
    }

    #[test]
    fn null_source_asks_for_a_flush() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cfg = config(Mode::Ll, 48_000);
        let mut s = Session::open(Mode::Ll, &cfg, &info(48_000), 895).unwrap();
        let mut dst = [0u8; 64];
        let (mut out, mut flush) = (0usize, NEED_FLUSH_NO);
        assert_eq!(s.encode(None, &mut dst, &mut out, &mut flush), Ok(0));
        assert_eq!(out, 0);
        assert_eq!(flush, NEED_FLUSH_ALL);
    }

    #[test]
    fn short_source_and_wrong_format_are_rejected() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cfg = config(Mode::Ll, 48_000);
        let mut bad = info(48_000);
        bad.raw.format = crate::ffi::SPA_AUDIO_FORMAT_S24_32_LE;
        assert!(Session::open(Mode::Ll, &cfg, &bad, 895).is_err());
        assert!(Session::open(Mode::Ll, &cfg[..39], &info(48_000), 895).is_err());

        let mut s = Session::open(Mode::Ll, &cfg, &info(48_000), 895).unwrap();
        let mut dst = [0u8; 64];
        let (mut out, mut flush) = (0usize, 0);
        assert_eq!(
            s.encode(Some(&[0u8; 8]), &mut dst, &mut out, &mut flush),
            Err(-EINVAL)
        );
    }
}

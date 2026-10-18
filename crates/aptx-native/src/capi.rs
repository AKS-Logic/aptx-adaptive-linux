//! C ABI for the PipeWire SPA codec plugin.
//!
//! The plugin (`a2dp-codec-aptx-adaptive.c` in the patched PipeWire tree)
//! calls these instead of spawning the qemu-hexagon helper.  One encoder
//! instance per stream; the plugin feeds interleaved S32 stereo in its
//! `get_block_size` granularity (480 frames) and drains zero or more OTA
//! packets per call.
//!
//! Return convention of `aptx_native_encoder_encode`:
//!   `> 0` bytes of a serialized OTA packet written to `dst`
//!   `0`   input accepted, no packet ready yet
//!   `< 0` error (negated errno)
//!
//! # Helper mode
//!
//! `APTX_NATIVE_HELPER` selects the vendored QEMU helper (see
//! [`crate::helper_bridge`]):
//!
//! * unset / empty / `0` / `false` / `off` / `native` — native encoder only.
//! * `auto` (also `1`, `true`, `yes`, `on`) — try the helper; if discovery,
//!   exec or the startup handshake fails, log the full spawn context and
//!   fall back to the native encoder instead of returning NULL.  This is the
//!   safe default for the helper experiment.
//! * `strict` (also `require`) — require the helper; a failure returns NULL
//!   so a caller that must have vendor frames can detect it.
//!
//! An unsupported rate (anything but 48 kHz) always uses the native encoder,
//! because the helper only implements the 48 kHz R2 form; that is logged in
//! both modes.  The helper returns 664-byte OTA records; this C ABI strips
//! the 8-byte SPF container and forwards the 656-byte payload.  Replay mode
//! takes precedence over the helper.
//!
//! SPDX-License-Identifier: MIT

use crate::encoder::{Config, Encoder};
use crate::frame_blocks::{total_blocks, FRAME_BYTES, LL_FRAME_BYTES};
use crate::helper_bridge::{HelperBridge, HelperConfig, R2_OTA_LEN};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};

/// Bytes of the SPF container in front of every helper OTA record (664 - 656).
const HELPER_CONTAINER_BYTES: usize = R2_OTA_LEN - FRAME_BYTES;

/// The only input rate the shipped helper implements (48 kHz R2).
const HELPER_RATE: u32 = 48_000;

/// How `APTX_NATIVE_HELPER` selects the QEMU helper path.
///
/// `Auto` keeps the stream alive when QEMU cannot start; `Strict` fails
/// closed so a caller that requires vendor frames sees NULL.  A rate the
/// helper cannot serve (anything but 48 kHz) uses the native encoder in both
/// modes, because no helper configuration exists for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelperMode {
    /// Never spawn QEMU.
    Off,
    /// Try the helper; fall back to the native encoder on any failure.
    Auto,
    /// Require the helper; return NULL if it cannot start.
    Strict,
}

impl HelperMode {
    fn parse(raw: Option<&str>) -> Self {
        let value = match raw.map(str::trim) {
            Some(value) if !value.is_empty() => value,
            _ => return Self::Off,
        };
        match value.to_ascii_lowercase().as_str() {
            "0" | "false" | "off" | "no" | "native" => Self::Off,
            "strict" | "require" | "required" => Self::Strict,
            "auto" | "1" | "true" | "yes" | "on" => Self::Auto,
            _ => {
                eprintln!(
                    "aptx-native: unknown APTX_NATIVE_HELPER={value:?}; using auto (native fallback enabled)"
                );
                Self::Auto
            }
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::Strict => "strict",
        }
    }
}

/// Read `APTX_NATIVE_HELPER` once per handle.
fn helper_mode() -> HelperMode {
    let raw = std::env::var("APTX_NATIVE_HELPER").ok();
    HelperMode::parse(raw.as_deref())
}

/// Helper configuration for a native-encoder rate.
///
/// The helper is the Bose 48 kHz R2 configuration.  Other rates (notably the
/// 96 kHz Hi-Res profile) return `None` so they keep using the native
/// encoder instead of failing to start a stream the helper cannot serve.
fn helper_config_for_rate(rate: u32) -> Option<HelperConfig> {
    if rate != HELPER_RATE {
        return None;
    }
    let mut config = HelperConfig::bose();
    config.source_rate = HELPER_RATE;
    config.encoder_rate = HELPER_RATE;
    Some(config)
}

/// Log the first few helper-path failures so a broken link is visible without
/// flooding the journal at frame rate.
fn log_helper_error(seen: &mut u32, message: impl std::fmt::Display) {
    if *seen < 4 {
        eprintln!("aptx-native: helper path: {message}");
    }
    *seen = seen.saturating_add(1);
}

static LAST_BLOCKS: AtomicU32 = AtomicU32::new(0);

/// Blocks (8 samples each) coded by the most recently drained frame.
#[no_mangle]
pub extern "C" fn aptx_native_last_blocks() -> u32 {
    LAST_BLOCKS.load(Ordering::Relaxed)
}

/// Replay mode: stream a recorded vendor R3 stream (664-byte OTA records)
/// through the native plugin path, rewriting the TTP field so the sink's
/// scheduler sees a clean 25 ms cadence.  The plugin now paces one 1102-sample
/// frame per call (25.00 ms at 44.1 kHz) and the OTA headers already carry
/// period 100, so only the TTP (unit 1/15000 s) needs correcting: the recording
/// holds wall-clock values from capture time, not a monotonic frame timeline.
/// This is the transport validation mode; the native encoder's own frame
/// syntax is produced by the normal path.
struct Replay {
    data: Vec<u8>,
    off: usize,
    /// OTA record length: 664 for R2/R3, 768 for the R2.2 (0xaf) form
    record_len: usize,
    /// Leading bytes stripped from each record (8 removes the SPF container so
    /// the A2DP packet is the ADK-style 668 bytes: 12 RTP + 656 frame)
    strip: usize,
    /// TTP advance per record in 1/15000 s units (375 = 25 ms, 420 = 28 ms)
    ttp_step: u16,
    /// next TTP to write (1/15000 s units)
    ttp: u16,
    /// Keep the vendor container TTP untouched instead of rewriting it
    pass_ttp: bool,
    /// Per-record block counts from a sidecar file (one number per OTA record)
    blocks: Vec<u16>,
    block_idx: usize,
}

impl Replay {
    fn load(path: &str) -> Option<Replay> {
        let data = std::fs::read(path).ok()?;
        if data.len() < 664 {
            return None;
        }
        // R2.2 records are 768 bytes (8-byte container + 760-byte payload,
        // packet type 5, period 112).  A 664-byte file is R2/R3.
        let (record_len, ttp_step) = if data.len() % 768 == 0 && data[2] == 112 {
            (768usize, 420u16)
        } else {
            (664usize, 375u16)
        };
        let strip = std::env::var("APTX_NATIVE_REPLAY_STRIP")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        if strip >= record_len {
            return None;
        }
        // Base the first TTP on the wall clock; each packet then advances by
        // the record's audio duration.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        // Anchor the TTP 260 ms AHEAD of the wall clock: a sink drops packets
        // whose play time has already passed on arrival, and a zero-lead
        // anchor leaves no decode/buffer headroom.  3900 units = 260 ms at
        // 1/15000 s.
        let ttp = ((now_ms * 15 + 3900) & 0xffff) as u16;
        let pass_ttp = std::env::var_os("APTX_NATIVE_REPLAY_PASS_TTP").is_some();
        let record_count = data.len() / record_len;
        let blocks: Vec<u16> = std::env::var("APTX_NATIVE_REPLAY_BLOCKS")
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| {
                s.split_whitespace()
                    .filter_map(|x| x.parse::<u16>().ok())
                    .collect::<Vec<_>>()
            })
            .filter(|v| v.len() == record_count)
            .unwrap_or_default();
        eprintln!("aptx-native: replay record={record_len} strip={strip} pass_ttp={pass_ttp} blocks={} ttp_step={ttp_step}", blocks.len());
        Some(Replay {
            data,
            off: 0,
            record_len,
            strip,
            ttp_step,
            ttp,
            pass_ttp,
            blocks,
            block_idx: 0,
        })
    }

    fn next_packet(&mut self, dst: &mut [u8]) -> Option<usize> {
        if self.off + self.record_len > self.data.len() {
            self.off = 0; // loop the recording
        }
        let pkt = &self.data[self.off + self.strip..self.off + self.record_len];
        self.off += self.record_len;
        let n = self.record_len - self.strip;
        if dst.len() < n {
            return None;
        }
        dst[..n].copy_from_slice(pkt);
        if !self.blocks.is_empty() {
            LAST_BLOCKS.store(u32::from(self.blocks[self.block_idx]), Ordering::Relaxed);
            self.block_idx = (self.block_idx + 1) % self.blocks.len();
        }
        if self.strip == 0 && !self.pass_ttp {
            /* container present: rewrite its TTP unless pass-through requested */
            dst[0] = (self.ttp & 0xff) as u8;
            dst[1] = (self.ttp >> 8) as u8;
        }
        self.ttp = self.ttp.wrapping_add(self.ttp_step);
        Some(n)
    }
}

/// Opaque encoder handle handed to C.
pub struct NativeEncoder {
    enc: Encoder,
    calls: u32,
    replay: Option<Replay>,
    /// QEMU vendor-helper path (APTX_NATIVE_HELPER=1); None for native/replay.
    helper: Option<HelperBridge>,
    /// Helper-path failures seen so far, used to rate-limit diagnostics.
    helper_errors: u32,
    /// Emit the 656-byte frame without the 8-byte SPF container: the ADK
    /// A2DP source shape (12 RTP + 656 frame = 668-byte packets).
    no_container: bool,
    /// Serialized packets waiting for the plugin, each with the frame's block
    /// count (8 samples per block) for `aptx_native_last_blocks`.
    queue: VecDeque<(Vec<u8>, u32)>,
    scratch_l: Vec<i32>,
    scratch_r: Vec<i32>,
}

/// Create a native encoder for `rate` (only 48000 is supported by the
/// plugin's config path) returning a handle or null.
///
/// # Safety
/// The returned pointer must be freed with [`aptx_native_encoder_free`].
#[no_mangle]
pub extern "C" fn aptx_native_encoder_new(rate: u32, _mtu: u32) -> *mut NativeEncoder {
    // One frame = 1360 samples at 48 kHz (85 steps x 16, 28.33 ms), the
    // cadence measured on the rig (14400 samples in -> 10 frames out);
    // 1248 samples (156 calls) at 44.1 kHz.
    // One frame is exactly one 25 ms period: 1200 samples at 48 kHz
    // (150 blocks of 8) and 1104 at 44.1 kHz (138 blocks).  The earlier
    // 170/156 values came from a non-streaming capture and put 1360/1248
    // samples of audio into a 25 ms container.
    let calls = std::env::var("APTX_NATIVE_CALLS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or_else(|| match rate {
            44100 => 138,
            48000 => 150,
            96000 => 96,
            192000 => 192,
            _ => 150,
        });
    let no_container = std::env::var_os("APTX_NATIVE_NO_CONTAINER").is_some();
    let cfg = Config {
        calls_per_frame: calls,
        vendor_frame: true,  // vendor-structure R2 frames (header + TID0 stream)
        ..Config::default()  // R2, 25 ms period, ptype 1 (656 B payload)
    };
    let replay = std::env::var("APTX_NATIVE_REPLAY")
        .ok()
        .and_then(|p| Replay::load(&p));
    if replay.is_some() {
        eprintln!("aptx-native: replay mode");
    }
    // Replay owns the transport when requested; otherwise APTX_NATIVE_HELPER
    // selects the opt-in emulation path.  The helper only serves 48 kHz, so
    // Hi-Res keeps its native encoder rather than failing to start.
    let helper_mode = helper_mode();
    let mut helper: Option<HelperBridge> = None;
    if replay.is_none() && helper_mode != HelperMode::Off {
        match helper_config_for_rate(rate) {
            Some(config) => match HelperBridge::spawn(config) {
                Ok(bridge) => {
                    eprintln!(
                        "aptx-native: helper mode={} rate={rate} pid={} (vendor R2, 656-byte payload)",
                        helper_mode.as_str(),
                        bridge.pid()
                    );
                    helper = Some(bridge);
                }
                Err(error) => {
                    if helper_mode == HelperMode::Strict {
                        eprintln!(
                            "aptx-native: helper mode=strict failed for rate={rate}; returning NULL: {error}"
                        );
                        return std::ptr::null_mut();
                    }
                    eprintln!(
                        "aptx-native: helper mode=auto failed for rate={rate}; falling back to the native encoder: {error}"
                    );
                }
            },
            None => {
                eprintln!(
                    "aptx-native: helper mode={} only supports {HELPER_RATE} Hz; using the native encoder for {rate} Hz",
                    helper_mode.as_str()
                );
            }
        }
    }
    eprintln!(
        "aptx-native: encoder created rate={rate} calls/frame={calls} helper={} source={}",
        helper_mode.as_str(),
        if replay.is_some() {
            "replay"
        } else if helper.is_some() {
            "vendor"
        } else {
            "native"
        }
    );
    let h = NativeEncoder {
        enc: Encoder::new(cfg),
        calls: calls as u32,
        replay,
        helper,
        helper_errors: 0,
        no_container,
        queue: VecDeque::new(),
        scratch_l: Vec::with_capacity(480),
        scratch_r: Vec::with_capacity(480),
    };
    Box::into_raw(Box::new(h))
}

/// Destroy an encoder created by [`aptx_native_encoder_new`].
///
/// # Safety
/// `p` must come from [`aptx_native_encoder_new`] and not be used after.
#[no_mangle]
pub unsafe extern "C" fn aptx_native_encoder_free(p: *mut NativeEncoder) {
    if !p.is_null() {
        drop(Box::from_raw(p));
    }
}

/// Feed interleaved stereo S32 (`frames` sample pairs) and drain one packet.
///
/// # Safety
/// `p` must be a live handle; `src` must point to `2 * frames` i32 values;
/// `dst` must be writable for `dst_size` bytes.
#[no_mangle]
pub unsafe extern "C" fn aptx_native_encoder_encode(
    p: *mut NativeEncoder,
    src: *const i32,
    frames: usize,
    dst: *mut u8,
    dst_size: usize,
) -> isize {
    if p.is_null() || src.is_null() || dst.is_null() {
        return -22; // -EINVAL
    }
    let h = &mut *p;
    if let Some(rep) = h.replay.as_mut() {
        let dst = std::slice::from_raw_parts_mut(dst, dst_size);
        return match rep.next_packet(dst) {
            Some(n) => n as isize,
            None => 0, // "input accepted, no packet ready" - never an error
        };
    }
    if let Some(helper) = h.helper.as_mut() {
        // The helper consumes the interleaved S32 chunk directly (one 25 ms
        // frame per plugin call in the LL configuration) and replies with a
        // 664-byte R2 OTA record: 8-byte SPF container + 656-byte payload.
        let inter = std::slice::from_raw_parts(src, frames * 2);
        return match helper.encode_interleaved_s32(inter) {
            Ok(None) => 0, // helper is still buffering
            Ok(Some(packet)) => {
                if packet.len() != R2_OTA_LEN {
                    log_helper_error(
                        &mut h.helper_errors,
                        format!(
                            "helper returned {} bytes, expected {R2_OTA_LEN}",
                            packet.len()
                        ),
                    );
                    return -5; // -EIO
                }
                let payload = &packet[HELPER_CONTAINER_BYTES..R2_OTA_LEN];
                if payload.len() > dst_size {
                    return -28; // -ENOSPC
                }
                std::ptr::copy_nonoverlapping(payload.as_ptr(), dst, payload.len());
                // The plugin advances its RTP timestamp by the frame's real
                // audio duration; 0 makes it fall back to the fixed step.
                let blocks = match total_blocks(payload) {
                    Ok(blocks) => u32::from(blocks),
                    Err(error) => {
                        log_helper_error(
                            &mut h.helper_errors,
                            format!("cannot read frame geometry: {error}"),
                        );
                        0
                    }
                };
                LAST_BLOCKS.store(blocks, Ordering::Relaxed);
                payload.len() as isize
            }
            Err(error) => {
                log_helper_error(&mut h.helper_errors, error);
                -5 // -EIO
            }
        };
    }
    h.scratch_l.clear();
    h.scratch_r.clear();
    let inter = std::slice::from_raw_parts(src, frames * 2);
    for chunk in inter.chunks_exact(2) {
        h.scratch_l.push(chunk[0]);
        h.scratch_r.push(chunk[1]);
    }
    for pkt in h.enc.encode(&h.scratch_l, &h.scratch_r) {
        // The RTP step follows the frame that is actually in the packet:
        // 150 blocks for the 656-byte R2 frame, 60 for the 348-byte LL
        // frame.  Deriving it from the payload keeps the count attached to
        // the packet even when one call drains several frames.
        // Fall back to the configured block count only if the encoder ever
        // produced a payload the parser cannot read.
        let blocks = match total_blocks(&pkt.payload) {
            Ok(blocks) => u32::from(blocks),
            Err(_) => h.calls,
        };
        if h.no_container {
            h.queue.push_back((pkt.payload.clone(), blocks));
        } else {
            h.queue.push_back((pkt.to_bytes(), blocks));
        }
    }
    match h.queue.pop_front() {
        None => 0,
        Some((bytes, blocks)) => {
            if bytes.len() > dst_size {
                return -28; // -ENOSPC
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
            LAST_BLOCKS.store(blocks, Ordering::Relaxed);
            bytes.len() as isize
        }
    }
}

/// Number of 8-sample blocks in one R2 frame payload (raw or OTA).
///
/// Accepts both payload shapes: 656 bytes (R2 HQ, 150 blocks for a 150-call
/// frame) and 348 bytes (R2 LL, 60 blocks).  Returns 0 for unknown/invalid
/// input.  Replay callers pass record + 8 (the 656-byte payload); the
/// 664-byte container form is not accepted.
#[no_mangle]
pub extern "C" fn aptx_native_frame_blocks(payload: *const u8, len: usize) -> u32 {
    if payload.is_null() || (len != FRAME_BYTES && len != LL_FRAME_BYTES) {
        return 0;
    }
    let payload = unsafe { std::slice::from_raw_parts(payload, len) };
    crate::frame_blocks::total_blocks(payload).unwrap_or(0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper_bridge::HelperPaths;
    use std::ffi::OsString;
    use std::process::Command;

    /// Restores the process environment when the test ends, including on panic.
    struct EnvRestore {
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvRestore {
        fn new(vars: &[(&'static str, Option<&'static str>)]) -> Self {
            let saved = vars
                .iter()
                .map(|(key, value)| {
                    let previous = std::env::var_os(key);
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                    (*key, previous)
                })
                .collect();
            Self { saved }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (key, value) in self.saved.drain(..) {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    /// The encoder handle is freed even when an assertion fails mid-test, so a
    /// broken helper link cannot leave QEMU running for the test binary's life.
    struct Handle(*mut NativeEncoder);

    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { aptx_native_encoder_free(self.0) };
        }
    }

    /// qemu-hexagon and the vendored helper are external dependencies.  As in
    /// helper_bridge's own tone test, a missing dependency skips the test
    /// instead of failing an offline build.
    fn helper_available() -> bool {
        let paths = match HelperPaths::discover() {
            Ok(paths) => paths,
            Err(error) => {
                eprintln!("skipping helper C ABI test: {error}");
                return false;
            }
        };
        match Command::new(&paths.qemu).arg("--version").output() {
            Ok(_) => true,
            Err(error) => {
                eprintln!(
                    "skipping helper C ABI test: cannot run {}: {error}",
                    paths.qemu.display()
                );
                false
            }
        }
    }

    /// Serialize this module's tests with the helper bridge's env-mutating
    /// tests: all of them touch the process-wide APTX_* variables.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::helper_bridge::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    #[test]
    fn helper_mode_values_are_parsed() {
        assert_eq!(HelperMode::parse(None), HelperMode::Off);
        for raw in ["", " ", "0", "false", "FALSE", "off", "no", "native"] {
            assert_eq!(HelperMode::parse(Some(raw)), HelperMode::Off, "raw={raw:?}");
        }
        for raw in ["auto", "AUTO", "1", "true", "yes", "on"] {
            assert_eq!(
                HelperMode::parse(Some(raw)),
                HelperMode::Auto,
                "raw={raw:?}"
            );
        }
        for raw in ["strict", "Strict", "require", "required"] {
            assert_eq!(
                HelperMode::parse(Some(raw)),
                HelperMode::Strict,
                "raw={raw:?}"
            );
        }
        assert_eq!(HelperMode::parse(Some("bogus")), HelperMode::Auto);
    }

    #[test]
    fn auto_falls_back_to_native_when_qemu_aborts() {
        let _guard = env_lock();
        if HelperPaths::discover().is_err() {
            eprintln!("skipping auto fallback test: helper checkout unavailable");
            return;
        }
        let _env = EnvRestore::new(&[
            ("APTX_NATIVE_HELPER", Some("auto")),
            ("APTX_NATIVE_REPLAY", None),
            ("APTX_NATIVE_LL", Some("1")),
            ("APTX_NATIVE_CALLS", None),
            ("APTX_NATIVE_NO_CONTAINER", Some("1")),
            ("APTX_QEMU", Some("/bin/false")),
        ]);

        let handle = Handle(aptx_native_encoder_new(HELPER_RATE, 995));
        assert!(
            !handle.0.is_null(),
            "auto mode must return a native encoder when the helper aborts"
        );

        const FRAMES: usize = 480; // one LL call = 10 ms at 48 kHz
        const CALLS: usize = 3;
        let mut dst = vec![0u8; 2048];
        let samples = vec![0i32; FRAMES * 2];
        let mut packets = 0usize;
        for _ in 0..CALLS {
            let written = unsafe {
                aptx_native_encoder_encode(
                    handle.0,
                    samples.as_ptr(),
                    FRAMES,
                    dst.as_mut_ptr(),
                    dst.len(),
                )
            };
            assert!(written >= 0, "native fallback encode failed: {written}");
            if written == 0 {
                continue;
            }
            assert_eq!(
                written as usize, LL_FRAME_BYTES,
                "auto fallback must emit the native 348-byte LL frame"
            );
            assert_eq!(
                aptx_native_frame_blocks(dst.as_ptr(), LL_FRAME_BYTES),
                60,
                "native LL frame geometry"
            );
            assert_eq!(aptx_native_last_blocks(), 60);
            packets += 1;
        }
        assert_eq!(
            packets, CALLS,
            "each 480-frame call should drain one native LL frame"
        );
    }

    #[test]
    fn strict_returns_null_when_qemu_aborts() {
        let _guard = env_lock();
        if HelperPaths::discover().is_err() {
            eprintln!("skipping strict fallback test: helper checkout unavailable");
            return;
        }
        let _env = EnvRestore::new(&[
            ("APTX_NATIVE_HELPER", Some("strict")),
            ("APTX_NATIVE_REPLAY", None),
            ("APTX_QEMU", Some("/bin/false")),
        ]);
        let handle = Handle(aptx_native_encoder_new(HELPER_RATE, 995));
        assert!(
            handle.0.is_null(),
            "strict mode must fail closed when the helper aborts"
        );
    }

    #[test]
    fn auto_survives_a_missing_checkout() {
        let _guard = env_lock();
        let _env = EnvRestore::new(&[
            ("APTX_NATIVE_HELPER", Some("auto")),
            ("APTX_NATIVE_REPLAY", None),
            (
                "APTX_ADAPTIVE_ROOT",
                Some("/nonexistent/aptx-adaptive-test-root"),
            ),
            ("APTX_NATIVE_NO_CONTAINER", Some("1")),
        ]);
        let handle = Handle(aptx_native_encoder_new(HELPER_RATE, 995));
        assert!(
            !handle.0.is_null(),
            "auto mode must survive path-discovery failure"
        );
    }

    #[test]
    fn helper_path_returns_656_byte_packets_with_block_counts() {
        let _guard = env_lock();
        if !helper_available() {
            return;
        }
        // The helper path is opt-in; replay would bypass it, so force it off
        // for this process and restore both variables afterwards.
        let _env = EnvRestore::new(&[
            ("APTX_NATIVE_HELPER", Some("1")),
            ("APTX_NATIVE_REPLAY", None),
        ]);

        let handle = Handle(aptx_native_encoder_new(HELPER_RATE, 995));
        assert!(!handle.0.is_null(), "helper encoder handle was null");

        // One LL call is 1200 frames = 25 ms at 48 kHz = one 656-byte frame.
        const FRAMES: usize = 1200;
        const CALLS: usize = 40; // 1 s, enough to prime the helper encoder
        let mut dst = vec![0u8; 2048];
        let mut samples = Vec::with_capacity(FRAMES * 2);
        let step = 2.0 * std::f64::consts::PI * 440.0 / f64::from(HELPER_RATE);
        let mut phase = 0.0f64;
        let mut packets = 0usize;
        let mut min_blocks = u32::MAX;
        let mut max_blocks = 0u32;

        for _ in 0..CALLS {
            samples.clear();
            for _ in 0..FRAMES {
                let value = (0.5 * f64::from(1u32 << 30) * phase.sin()) as i32;
                samples.push(value);
                samples.push(value);
                phase += step;
            }
            let written = unsafe {
                aptx_native_encoder_encode(
                    handle.0,
                    samples.as_ptr(),
                    FRAMES,
                    dst.as_mut_ptr(),
                    dst.len(),
                )
            };
            assert!(written >= 0, "helper encode failed with {written}");
            if written == 0 {
                continue;
            }
            assert_eq!(
                written as usize, FRAME_BYTES,
                "helper must return the 656-byte payload without the SPF container"
            );
            let blocks = aptx_native_frame_blocks(dst.as_ptr(), FRAME_BYTES);
            assert!(blocks > 0, "helper frame has no valid block geometry");
            assert_eq!(
                aptx_native_last_blocks(),
                blocks,
                "LAST_BLOCKS must match the frame's parsed block count"
            );
            min_blocks = min_blocks.min(blocks);
            max_blocks = max_blocks.max(blocks);
            packets += 1;
        }

        assert!(packets > 0, "helper produced no packets in {CALLS} calls");
        eprintln!(
            "helper C ABI test: {packets} packets of {FRAME_BYTES} bytes from {CALLS} calls; \
             blocks {min_blocks}..={max_blocks}"
        );
    }
}

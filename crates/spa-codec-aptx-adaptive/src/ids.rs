//! What the plugin advertises itself as. Shared with `tests/dlopen.rs`.

use std::ffi::CStr;

pub const INTERFACE_TYPE: &CStr = c"Spa:Pointer:Interface:Bluez5:Codec:Media:Private";
pub const FACTORY_NAME: &CStr = c"api.codec.bluez5.media.lc3plus";
pub const CODEC_NAME: &CStr = c"aptx_adaptive";
pub const CODEC_DESCRIPTION: &CStr = c"aptX Adaptive";
/// The low-latency entry, on the same endpoint, so it gets its own card profile
/// (`docs/adr/0003-ll-profile.md`).
pub const LL_NAME: &CStr = c"aptx_adaptive_ll";
pub const LL_DESCRIPTION: &CStr = c"aptX Adaptive (Low Latency)";

pub const VENDOR_ID: u32 = 0x0000_00D7;
pub const CODEC_ID: u16 = 0x00AD;

/// `a2dp_vendor_codec_t` as it appears at the start of the caps: both fields little-endian.
pub const VENDOR_HEADER: [u8; 6] = {
    let v = VENDOR_ID.to_le_bytes();
    let c = CODEC_ID.to_le_bytes();
    [v[0], v[1], v[2], v[3], c[0], c[1]]
};

/// Send-buffer size in kernel-accounted bytes; the default buffer drops packets on `EAGAIN`.
/// 32 KiB holds about 20 packets (~280 ms), slightly over the HQ target latency (§7F).
pub const SEND_BUF_SIZE: usize = 32 * 1024;

/// LOSC 42 minus the media and codec type bytes.
pub const CAPS_LEN: usize = 40;

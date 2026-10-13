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

/// `media_codec.send_buf_size`: the L2CAP socket's send buffer, in kernel-accounted bytes
/// (`skb` truesize, so a 668-byte packet costs well over 668). Without it media-sink uses
/// 4 × MTU (×2 by the kernel), only a handful of our packets, about 50 ms. And media-sink
/// *drops* every packet `send()` refuses with EAGAIN (`flush_data`: "just skip this packet")
/// while still bumping the sequence number. So a controller stall of ~150 ms at stream start
/// loses a run of packets with a sequence gap, matching the 7-packet hole (25 → 33) in every
/// capture. 32 KiB holds roughly 20 packets, about 280 ms, a little over the HQ target
/// latency (§7F, 200 ms on the Px7): a stall the sink could ride out now delays packets
/// instead of dropping them, and anything longer would have underrun the sink anyway.
pub const SEND_BUF_SIZE: usize = 32 * 1024;

/// LOSC 42 minus the media and codec type bytes.
pub const CAPS_LEN: usize = 40;

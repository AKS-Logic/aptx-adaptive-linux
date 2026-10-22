use std::ffi::CStr;

pub const INTERFACE_TYPE: &CStr = c"Spa:Pointer:Interface:Bluez5:Codec:Media:Private";
pub const FACTORY_NAME_LC3PLUS: &CStr = c"api.codec.bluez5.media.lc3plus";
pub const FACTORY_NAME_APTX: &CStr = c"api.codec.bluez5.media.aptx";
pub const FACTORY_DESCRIPTION: &CStr = c"Bluetooth codec plugin (aptX Adaptive)";
pub const CODEC_NAME: &CStr = c"aptx_adaptive";
pub const CODEC_DESCRIPTION: &CStr = c"aptX Adaptive";
pub const LL_NAME: &CStr = c"aptx_adaptive_ll";
pub const LL_DESCRIPTION: &CStr = c"aptX Adaptive (Low Latency)";

// The native-encoder port of `a2dp-codec-aptx-adaptive.c` uses the C plugin's
// own strings: two media codecs sharing one endpoint, exported by a separate
// SPA factory so the pure-Rust plugin above keeps its existing ABI.
pub const ADAPTIVE_LL_NAME: &CStr = c"aptx_adaptive_ll";
pub const ADAPTIVE_LL_DESCRIPTION: &CStr = c"aptX Adaptive LL";
pub const ADAPTIVE_HIRES_NAME: &CStr = c"aptx_adaptive_hires";
pub const ADAPTIVE_HIRES_DESCRIPTION: &CStr = c"aptX Adaptive Hi-Res";
pub const ADAPTIVE_ENDPOINT: &CStr = c"aptx_adaptive";
// HQ is spec profile 3 ("HQ", not "HD": aptX HD is a different codec).
pub const ADAPTIVE_HQ_NAME: &CStr = c"aptx_adaptive_hq";
pub const ADAPTIVE_HQ_DESCRIPTION: &CStr = c"aptX Adaptive HQ";
pub const ADAPTIVE_HQ_ENDPOINT: &CStr = c"aptx_adaptive_hq";
pub const ADAPTIVE_LL_ENDPOINT: &CStr = c"aptx_adaptive_ll";
pub const FACTORY_NAME_NATIVE: &CStr = c"api.codec.bluez5.media.aptx_adaptive";
/// Own file/factory for the HQ profile: one endpoint per plugin file is the
/// only shape the BlueZ side reliably registers as its own card profile.
pub const FACTORY_NAME_HQ: &CStr = c"api.codec.bluez5.media.aptx_adaptive_hq";
pub const FACTORY_DESCRIPTION_NATIVE: &CStr =
    c"aptX Adaptive native encoder plugin (Rust port of a2dp-codec-aptx-adaptive.c)";

pub const VENDOR_ID: u32 = 0x0000_00D7;
pub const CODEC_ID: u16 = 0x00AD;

pub const VENDOR_HEADER: [u8; 6] = {
    let v = VENDOR_ID.to_le_bytes();
    let c = CODEC_ID.to_le_bytes();
    [v[0], v[1], v[2], v[3], c[0], c[1]]
};

pub const SEND_BUF_SIZE: usize = 32 * 1024;

pub const CAPS_LEN: usize = 40;

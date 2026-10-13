use std::ffi::{c_char, c_int, c_void};

pub const SPA_VERSION_HANDLE: u32 = 0;
pub const SPA_VERSION_HANDLE_FACTORY: u32 = 1;
pub const SPA_VERSION_BLUEZ5_CODEC_MEDIA: u32 = 16;

#[cfg(test)]
pub const SPA_PARAM_ENUM_FORMAT: u32 = 3;
pub const SPA_BLUETOOTH_AUDIO_CODEC_MPEG: u32 = 3;
pub const SPA_BLUETOOTH_AUDIO_CODEC_APTX: u32 = 6;
pub const SPA_BLUETOOTH_AUDIO_CODEC_APTX_HD: u32 = 7;
pub const SPA_BLUETOOTH_AUDIO_CODEC_LDAC: u32 = 8;
pub const SPA_BLUETOOTH_AUDIO_CODEC_LC3PLUS_HR: u32 = 13;
pub const MEDIA_CODEC_A2DP: u32 = 0;
pub const A2DP_CODEC_VENDOR: u8 = 0xFF;
pub const A2DP_MAX_CAPS_SIZE: usize = 254;

pub const ENOENT: c_int = 2;
pub const EINVAL: c_int = 22;
pub const ENOSPC: c_int = 28;
pub const ENOTSUP: c_int = 95;

pub const NEED_FLUSH_NO: c_int = 0;
pub const NEED_FLUSH_ALL: c_int = 1;

pub const SPA_MEDIA_TYPE_AUDIO: u32 = 1;
pub const SPA_MEDIA_SUBTYPE_RAW: u32 = 1;
pub const SPA_AUDIO_FORMAT_S24_32_LE: u32 = 263;
pub const SPA_AUDIO_CHANNEL_FL: u32 = 3;
pub const SPA_AUDIO_CHANNEL_FR: u32 = 4;
pub const SPA_AUDIO_MAX_CHANNELS: usize = 64;

pub const SPA_TYPE_ID: u32 = 3;
pub const SPA_TYPE_INT: u32 = 4;
pub const SPA_TYPE_STRING: u32 = 8;
pub const SPA_TYPE_ARRAY: u32 = 13;
pub const SPA_TYPE_STRUCT: u32 = 14;
pub const SPA_TYPE_OBJECT: u32 = 15;
pub const SPA_TYPE_CHOICE: u32 = 19;
pub const SPA_TYPE_OBJECT_PROP_INFO: u32 = 0x40001;
pub const SPA_TYPE_OBJECT_PROPS: u32 = 0x40002;
pub const SPA_TYPE_OBJECT_FORMAT: u32 = 0x40003;
pub const SPA_CHOICE_ENUM: u32 = 3;
pub const SPA_PARAM_PROP_INFO: u32 = 1;
pub const SPA_PARAM_PROPS: u32 = 2;
pub const SPA_PROP_INFO_ID: u32 = 1;
pub const SPA_PROP_INFO_TYPE: u32 = 3;
pub const SPA_PROP_INFO_LABELS: u32 = 4;
pub const SPA_PROP_INFO_DESCRIPTION: u32 = 7;
pub const SPA_PROP_QUALITY: u32 = 0x10d;
pub const SPA_FORMAT_MEDIA_TYPE: u32 = 1;
pub const SPA_FORMAT_MEDIA_SUBTYPE: u32 = 2;
pub const SPA_FORMAT_AUDIO_FORMAT: u32 = 0x10001;
pub const SPA_FORMAT_AUDIO_RATE: u32 = 0x10003;
pub const SPA_FORMAT_AUDIO_CHANNELS: u32 = 0x10004;
pub const SPA_FORMAT_AUDIO_POSITION: u32 = 0x10005;

#[repr(C)]
pub struct spa_dict_item {
    pub key: *const c_char,
    pub value: *const c_char,
}

#[repr(C)]
pub struct spa_dict {
    pub flags: u32,
    pub n_items: u32,
    pub items: *const spa_dict_item,
}

#[repr(C)]
pub struct spa_callbacks {
    pub funcs: *const c_void,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct spa_interface {
    pub type_: *const c_char,
    pub version: u32,
    pub cb: spa_callbacks,
}

#[repr(C)]
pub struct spa_interface_info {
    pub type_: *const c_char,
}

#[repr(C)]
pub struct spa_support {
    pub type_: *const c_char,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct spa_handle {
    pub version: u32,
    pub get_interface:
        Option<unsafe extern "C" fn(*mut spa_handle, *const c_char, *mut *mut c_void) -> c_int>,
    pub clear: Option<unsafe extern "C" fn(*mut spa_handle) -> c_int>,
}

#[repr(C)]
pub struct spa_handle_factory {
    pub version: u32,
    pub name: *const c_char,
    pub info: *const spa_dict,
    pub get_size: Option<unsafe extern "C" fn(*const spa_handle_factory, *const spa_dict) -> usize>,
    pub init: Option<
        unsafe extern "C" fn(
            *const spa_handle_factory,
            *mut spa_handle,
            *const spa_dict,
            *const spa_support,
            u32,
        ) -> c_int,
    >,
    pub enum_interface_info: Option<
        unsafe extern "C" fn(
            *const spa_handle_factory,
            *mut *const spa_interface_info,
            *mut u32,
        ) -> c_int,
    >,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct spa_pod {
    pub size: u32,
    pub type_: u32,
}

#[repr(C)]
pub struct spa_pod_frame {
    pub pod: spa_pod,
    pub parent: *mut spa_pod_frame,
    pub offset: u32,
    pub flags: u32,
}

#[repr(C)]
pub struct spa_pod_builder_state {
    pub offset: u32,
    pub flags: u32,
    pub frame: *mut spa_pod_frame,
}

#[repr(C)]
pub struct spa_pod_builder_callbacks {
    pub version: u32,
    pub overflow: Option<unsafe extern "C" fn(*mut c_void, u32) -> c_int>,
}

#[repr(C)]
pub struct spa_pod_builder {
    pub data: *mut c_void,
    pub size: u32,
    pub _padding: u32,
    pub state: spa_pod_builder_state,
    pub callbacks: spa_callbacks,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct spa_audio_info_raw {
    pub format: u32,
    pub flags: u32,
    pub rate: u32,
    pub channels: u32,
    pub position: [u32; SPA_AUDIO_MAX_CHANNELS],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct spa_audio_info {
    pub media_type: u32,
    pub media_subtype: u32,
    pub raw: spa_audio_info_raw,
    pub _union_tail: [u32; 1],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct a2dp_vendor_codec_t {
    pub vendor_id: u32,
    pub codec_id: u16,
}

#[repr(C)]
pub struct media_codec_audio_info {
    pub rate: u32,
    pub channels: u32,
}

pub type Opaque = c_void;

#[repr(C)]
pub struct media_codec {
    pub id: u32,
    pub kind: u32,
    pub codec_id: u8,
    pub vendor: a2dp_vendor_codec_t,
    pub name: *const c_char,
    pub description: *const c_char,
    pub endpoint_name: *const c_char,
    pub info: *const spa_dict,
    pub send_buf_size: usize,
    pub duplex_codec: *const media_codec,
    pub stream_pkt: bool,

    pub get_bis_config: Option<
        unsafe extern "C" fn(
            *const media_codec,
            *mut u8,
            *mut u8,
            *mut spa_dict,
            *mut Opaque,
        ) -> c_int,
    >,
    pub fill_caps:
        Option<unsafe extern "C" fn(*const media_codec, u32, *const spa_dict, *mut u8) -> c_int>,
    pub select_config: Option<
        unsafe extern "C" fn(
            *const media_codec,
            u32,
            *const c_void,
            usize,
            *const media_codec_audio_info,
            *const spa_dict,
            *mut u8,
            *mut *mut c_void,
        ) -> c_int,
    >,
    pub enum_config: Option<
        unsafe extern "C" fn(
            *const media_codec,
            u32,
            *const c_void,
            usize,
            u32,
            u32,
            *mut spa_pod_builder,
            *mut *mut spa_pod,
        ) -> c_int,
    >,
    pub validate_config: Option<
        unsafe extern "C" fn(
            *const media_codec,
            u32,
            *const c_void,
            usize,
            *mut spa_audio_info,
        ) -> c_int,
    >,
    pub get_qos: Option<
        unsafe extern "C" fn(
            *const media_codec,
            *const Opaque,
            *const c_void,
            *mut Opaque,
        ) -> c_int,
    >,
    pub get_metadata:
        Option<unsafe extern "C" fn(*const media_codec, *const c_void, *mut u8, usize) -> c_int>,
    pub free_config_data: Option<unsafe extern "C" fn(*const media_codec, *mut c_void)>,
    pub caps_preference_cmp: Option<
        unsafe extern "C" fn(
            *const media_codec,
            u32,
            *const c_void,
            usize,
            *const c_void,
            usize,
            *const media_codec_audio_info,
            *const spa_dict,
        ) -> c_int,
    >,
    pub init_props:
        Option<unsafe extern "C" fn(*const media_codec, u32, *const spa_dict) -> *mut c_void>,
    pub clear_props: Option<unsafe extern "C" fn(*mut c_void)>,
    pub enum_props: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const spa_dict,
            u32,
            u32,
            *mut spa_pod_builder,
            *mut *mut spa_pod,
        ) -> c_int,
    >,
    pub set_props: Option<unsafe extern "C" fn(*mut c_void, *const spa_pod) -> c_int>,
    pub init: Option<
        unsafe extern "C" fn(
            *const media_codec,
            u32,
            *mut c_void,
            usize,
            *const spa_audio_info,
            *mut c_void,
            usize,
        ) -> *mut c_void,
    >,
    pub deinit: Option<unsafe extern "C" fn(*mut c_void)>,
    pub update_props: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int>,
    pub get_block_size: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    pub get_interval: Option<unsafe extern "C" fn(*mut c_void) -> u64>,
    pub abr_process: Option<unsafe extern "C" fn(*mut c_void, usize) -> c_int>,
    pub start_encode:
        Option<unsafe extern "C" fn(*mut c_void, *mut c_void, usize, u16, u32) -> c_int>,
    pub encode: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const c_void,
            usize,
            *mut c_void,
            usize,
            *mut usize,
            *mut c_int,
        ) -> c_int,
    >,
    pub start_decode: Option<
        unsafe extern "C" fn(*mut c_void, *const c_void, usize, *mut u16, *mut u32) -> c_int,
    >,
    pub decode: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const c_void,
            usize,
            *mut c_void,
            usize,
            *mut usize,
        ) -> c_int,
    >,
    pub produce_plc: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> c_int>,
    pub reduce_bitpool: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    pub increase_bitpool: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    pub set_log: Option<unsafe extern "C" fn(*mut Opaque)>,
    pub get_delay: Option<unsafe extern "C" fn(*mut c_void, *mut u32, *mut u32)>,
}

#[repr(C)]
pub struct spa_bluez5_codec_a2dp {
    pub iface: spa_interface,
    pub codecs: *const *const media_codec,
}

pub type SpaHandleFactoryEnumFn =
    unsafe extern "C" fn(*mut *const spa_handle_factory, *mut u32) -> c_int;

pub const RTLD_NOW: c_int = 2;
pub const RTLD_LOCAL: c_int = 0;

#[repr(C)]
pub struct DlInfo {
    pub fname: *const c_char,
    pub fbase: *mut c_void,
    pub sname: *const c_char,
    pub saddr: *mut c_void,
}

// glibc ≥ 2.34 has these in libc and keeps libdl as an empty stub; older glibc needs libdl.
#[link(name = "dl")]
unsafe extern "C" {
    pub fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    pub fn dlclose(handle: *mut c_void) -> c_int;
    pub fn dlerror() -> *const c_char;
    pub fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int;
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    use std::mem::{align_of, offset_of, size_of};

    assert!(size_of::<media_codec>() == 296);
    assert!(align_of::<media_codec>() == 8);
    assert!(offset_of!(media_codec, kind) == 4);
    assert!(offset_of!(media_codec, codec_id) == 8);
    assert!(offset_of!(media_codec, vendor) == 9);
    assert!(offset_of!(media_codec, name) == 16);
    assert!(offset_of!(media_codec, description) == 24);
    assert!(offset_of!(media_codec, endpoint_name) == 32);
    assert!(offset_of!(media_codec, info) == 40);
    assert!(offset_of!(media_codec, send_buf_size) == 48);
    assert!(offset_of!(media_codec, duplex_codec) == 56);
    assert!(offset_of!(media_codec, stream_pkt) == 64);
    assert!(offset_of!(media_codec, get_bis_config) == 72);
    assert!(offset_of!(media_codec, fill_caps) == 80);
    assert!(offset_of!(media_codec, select_config) == 88);
    assert!(offset_of!(media_codec, enum_config) == 96);
    assert!(offset_of!(media_codec, validate_config) == 104);
    assert!(offset_of!(media_codec, get_qos) == 112);
    assert!(offset_of!(media_codec, get_metadata) == 120);
    assert!(offset_of!(media_codec, free_config_data) == 128);
    assert!(offset_of!(media_codec, caps_preference_cmp) == 136);
    assert!(offset_of!(media_codec, init_props) == 144);
    assert!(offset_of!(media_codec, clear_props) == 152);
    assert!(offset_of!(media_codec, enum_props) == 160);
    assert!(offset_of!(media_codec, set_props) == 168);
    assert!(offset_of!(media_codec, init) == 176);
    assert!(offset_of!(media_codec, deinit) == 184);
    assert!(offset_of!(media_codec, update_props) == 192);
    assert!(offset_of!(media_codec, get_block_size) == 200);
    assert!(offset_of!(media_codec, get_interval) == 208);
    assert!(offset_of!(media_codec, abr_process) == 216);
    assert!(offset_of!(media_codec, start_encode) == 224);
    assert!(offset_of!(media_codec, encode) == 232);
    assert!(offset_of!(media_codec, start_decode) == 240);
    assert!(offset_of!(media_codec, decode) == 248);
    assert!(offset_of!(media_codec, produce_plc) == 256);
    assert!(offset_of!(media_codec, reduce_bitpool) == 264);
    assert!(offset_of!(media_codec, increase_bitpool) == 272);
    assert!(offset_of!(media_codec, set_log) == 280);
    assert!(offset_of!(media_codec, get_delay) == 288);

    assert!(size_of::<a2dp_vendor_codec_t>() == 6);
    assert!(size_of::<spa_bluez5_codec_a2dp>() == 40);
    assert!(offset_of!(spa_bluez5_codec_a2dp, codecs) == 32);
    assert!(size_of::<spa_interface>() == 32);
    assert!(size_of::<spa_handle>() == 24);
    assert!(size_of::<spa_handle_factory>() == 48);
    assert!(offset_of!(spa_handle_factory, name) == 8);
    assert!(offset_of!(spa_handle_factory, info) == 16);
    assert!(offset_of!(spa_handle_factory, get_size) == 24);
    assert!(offset_of!(spa_handle_factory, init) == 32);
    assert!(offset_of!(spa_handle_factory, enum_interface_info) == 40);
    assert!(size_of::<spa_dict>() == 16);
    assert!(size_of::<spa_audio_info>() == 284);
    assert!(align_of::<spa_audio_info>() == 4);
    assert!(offset_of!(spa_audio_info, raw) == 8);
    assert!(size_of::<spa_audio_info_raw>() == 272);
    assert!(offset_of!(spa_audio_info_raw, position) == 16);
    assert!(size_of::<spa_pod_builder>() == 48);
    assert!(offset_of!(spa_pod_builder, state) == 16);
    assert!(offset_of!(spa_pod_builder, callbacks) == 32);
    assert!(size_of::<spa_pod_frame>() == 24);
    assert!(offset_of!(spa_pod_frame, parent) == 8);
    assert!(offset_of!(spa_pod_frame, offset) == 16);
    assert!(size_of::<media_codec_audio_info>() == 8);
};

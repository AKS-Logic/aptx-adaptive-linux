//! The two aptX Adaptive media codecs, ported from a2dp-codec-aptx-adaptive.c.
//!
//! The LL entry (SPA id 0x302) owns the shared endpoint and advertises the
//! union of both modes' rates; the Hi-Res entry (0x303) is the alias with
//! fill_caps == NULL, which is how codec-loader.c keeps both card profiles
//! on one endpoint. Both entries share an endpoint_name of "aptx_adaptive".
//!
//! This is an additive factory: the crate's existing pure-Rust plugin keeps
//! its own factories, so installing that plugin is unchanged.
#![allow(
    clippy::doc_markdown,
    reason = "protocol identifiers are used as prose in the mode table"
)]

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;
use std::slice;

use crate::SyncStatic;
use crate::ffi::{
    A2DP_CODEC_VENDOR, A2DP_MAX_CAPS_SIZE, EINVAL, ENOENT, ENOTSUP, MEDIA_CODEC_A2DP,
    SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR, SPA_AUDIO_FORMAT_S32_LE,
    SPA_BLUETOOTH_AUDIO_CODEC_APTX_ADAPTIVE_HIRES, SPA_BLUETOOTH_AUDIO_CODEC_APTX_ADAPTIVE_LL,
    SPA_VERSION_BLUEZ5_CODEC_MEDIA, SPA_VERSION_HANDLE, SPA_VERSION_HANDLE_FACTORY,
    a2dp_vendor_codec_t, media_codec, media_codec_audio_info, spa_audio_info,
    spa_bluez5_codec_a2dp, spa_callbacks, spa_dict, spa_dict_item, spa_handle, spa_handle_factory,
    spa_interface, spa_interface_info, spa_pod, spa_pod_builder, spa_support,
};
use crate::ids;
use crate::pod::{self, RawFormat};

mod env;
mod mode;
mod session;

use mode::{
    BODY_SIZE, CH_JOINT, CH_MASK, FEATURES_UNKNOWN, Mode, OFF_CHANNEL_MODE, OFF_FEATURES,
    OFF_RATE_SOURCE, SR_MASK, ST_MASK,
};
use session::{ENCODER_DELAY_SAMPLES, Session};

/// Source type from APTX_ADAPTIVE_SOURCE_TYPE, else the mode default.
fn configured_source_type(mode: Mode) -> u8 {
    let source = env::value_u32("APTX_ADAPTIVE_SOURCE_TYPE")
        .map_or(mode.default_source_type(), |value| value as u8);
    source & ST_MASK
}

/// Rate to negotiate: the sink's rate bits pick among the mode's formats;
/// APTX_ADAPTIVE_RATE only chooses between the mode's own formats.
fn configured_rate(mode: Mode, caps: Option<&[u8]>) -> u32 {
    let env_rate = env::u32("APTX_ADAPTIVE_RATE", 0);
    let sink_bits = caps
        .filter(|body| body.len() > OFF_RATE_SOURCE)
        .map_or(0, |body| body[OFF_RATE_SOURCE] & SR_MASK);
    let picked = if sink_bits == 0 {
        0
    } else {
        mode.pick_rate(sink_bits)
    };

    if sink_bits != 0 {
        if env_rate != 0
            && let Some(format) = mode.format(env_rate)
            && sink_bits & format.rate_bits != 0
        {
            return env_rate;
        }
        return picked;
    }

    if env_rate != 0 && mode.format(env_rate).is_some() {
        return env_rate;
    }
    mode.formats().first().map_or(0, |f| f.rate)
}

fn configured_channel_mode(caps: Option<&[u8]>) -> u8 {
    if std::env::var_os("APTX_ADAPTIVE_CHANNEL_MODE").is_some() {
        if let Some(value) = env::value_u32("APTX_ADAPTIVE_CHANNEL_MODE") {
            let channel_mode = (value as u8) & CH_MASK;
            if channel_mode != 0 && channel_mode.is_power_of_two() {
                return channel_mode;
            }
        }
        let _ = writeln_stderr("aptx-adaptive: ignoring invalid APTX_ADAPTIVE_CHANNEL_MODE");
    }
    if let Some(body) = caps.filter(|body| body.len() > OFF_CHANNEL_MODE) {
        return Mode::pick_channel_mode(body[OFF_CHANNEL_MODE]);
    }
    CH_JOINT
}

fn caps_features(caps: Option<&[u8]>) -> u32 {
    let Some(body) = caps else {
        return FEATURES_UNKNOWN;
    };
    let Some(bytes) = body.get(OFF_FEATURES..OFF_FEATURES + 4) else {
        return FEATURES_UNKNOWN;
    };
    u32::from_le_bytes(bytes.try_into().expect("four bytes"))
}

fn apply_features_env(config: &mut [u8; BODY_SIZE]) {
    if let Some(value) = env::value_u32("APTX_ADAPTIVE_FEATURES_LOW") {
        config[OFF_FEATURES] = (value & 0xff) as u8;
    }
}

fn writeln_stderr(message: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    writeln!(std::io::stderr(), "{message}")
}

fn write_out(src: &[u8], dst: *mut u8) -> c_int {
    if dst.is_null() || src.len() > A2DP_MAX_CAPS_SIZE {
        return -EINVAL;
    }
    // SAFETY: PipeWire passes a buffer of A2DP_MAX_CAPS_SIZE bytes.
    unsafe { ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len()) };
    src.len() as c_int
}

unsafe extern "C" fn codec_fill_caps(
    codec: *const media_codec,
    _flags: u32,
    _settings: *const spa_dict,
    caps_out: *mut u8,
) -> c_int {
    let mode = Mode::from_codec(codec);
    write_out(
        &mode.advertised_caps(configured_source_type(mode)),
        caps_out,
    )
}

unsafe extern "C" fn codec_select_config(
    codec: *const media_codec,
    _flags: u32,
    caps: *const c_void,
    caps_size: usize,
    _info: *const media_codec_audio_info,
    _settings: *const spa_dict,
    config: *mut u8,
    config_data: *mut *mut c_void,
) -> c_int {
    if !config_data.is_null() {
        // SAFETY: PipeWire passes a writable config_data pointer or null.
        unsafe { *config_data = ptr::null_mut() };
    }
    if caps.is_null() {
        return -EINVAL;
    }
    // SAFETY: PipeWire passes caps_size readable bytes.
    let caps = unsafe { slice::from_raw_parts(caps.cast::<u8>(), caps_size) };
    if caps.len() < BODY_SIZE {
        return -EINVAL;
    }
    let mode = Mode::from_codec(codec);
    let sink_bits = caps[OFF_RATE_SOURCE] & SR_MASK;
    if sink_bits != 0 && mode.pick_rate(sink_bits) == 0 {
        return -EINVAL;
    }
    let rate = configured_rate(mode, Some(caps));
    if mode.format(rate).is_none() {
        return -EINVAL;
    }
    let source_type = configured_source_type(mode);
    let channel_mode = configured_channel_mode(Some(caps));
    let features = caps_features(Some(caps));
    let Some(mut config_body) = mode.config(rate, source_type, channel_mode, features) else {
        return -EINVAL;
    };
    apply_features_env(&mut config_body);
    write_out(&config_body, config)
}

unsafe extern "C" fn codec_enum_config(
    codec: *const media_codec,
    _flags: u32,
    caps: *const c_void,
    caps_size: usize,
    id: u32,
    idx: u32,
    builder: *mut spa_pod_builder,
    param: *mut *mut spa_pod,
) -> c_int {
    // SAFETY: PipeWire passes caps_size readable bytes when caps is non-null.
    let caps = if caps.is_null() {
        None
    } else {
        Some(unsafe { slice::from_raw_parts(caps.cast::<u8>(), caps_size) })
    };
    let mode = Mode::from_codec(codec);
    let rate = configured_rate(mode, caps);
    if idx > 0 || rate == 0 {
        return 0;
    }
    if builder.is_null() || param.is_null() {
        return -EINVAL;
    }
    let format = RawFormat {
        format: SPA_AUDIO_FORMAT_S32_LE,
        rate,
        positions: &[SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR],
    };
    let pod = pod::raw_audio_format(id, &format);
    // SAFETY: builder is non-null and owned by the caller for this call.
    match unsafe { crate::builder_append(builder, &pod) } {
        Ok(pointer) => {
            // SAFETY: param checked non-null.
            unsafe { *param = pointer };
            1
        }
        Err(error) => error,
    }
}

unsafe extern "C" fn codec_init(
    codec: *const media_codec,
    _flags: u32,
    config: *mut c_void,
    config_size: usize,
    info: *const spa_audio_info,
    _props: *mut c_void,
    mtu: usize,
) -> *mut c_void {
    if config.is_null() || info.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: PipeWire passes config_size readable bytes and a valid info.
    let config = unsafe { slice::from_raw_parts(config.cast::<u8>(), config_size) };
    // SAFETY: checked non-null above.
    let info = unsafe { &*info };
    match Session::open(Mode::from_codec(codec), config, info, mtu) {
        // SAFETY: Box owns the session; deinit turns it back into a Box.
        Ok(session) => Box::into_raw(Box::new(session)).cast(),
        Err(_) => ptr::null_mut(),
    }
}

unsafe extern "C" fn codec_deinit(data: *mut c_void) {
    if !data.is_null() {
        // SAFETY: data came from Box::into_raw in codec_init.
        drop(unsafe { Box::from_raw(data.cast::<Session>()) });
    }
}

unsafe extern "C" fn codec_get_block_size(data: *mut c_void) -> c_int {
    if data.is_null() {
        return -EINVAL;
    }
    // SAFETY: PipeWire passes the pointer from init.
    unsafe { &*data.cast::<Session>() }.block_size() as c_int
}

unsafe extern "C" fn codec_start_encode(
    data: *mut c_void,
    dst: *mut c_void,
    dst_size: usize,
    seqnum: u16,
    timestamp: u32,
) -> c_int {
    if data.is_null() || dst.is_null() {
        return -EINVAL;
    }
    // SAFETY: PipeWire passes dst_size writable bytes and the session from init.
    let dst = unsafe { slice::from_raw_parts_mut(dst.cast::<u8>(), dst_size) };
    // SAFETY: see above.
    match unsafe { &mut *data.cast::<Session>() }.start_encode(dst, seqnum, timestamp) {
        Ok(len) => len as c_int,
        Err(error) => error,
    }
}

unsafe extern "C" fn codec_encode(
    data: *mut c_void,
    src: *const c_void,
    src_size: usize,
    dst: *mut c_void,
    dst_size: usize,
    dst_out: *mut usize,
    need_flush: *mut c_int,
) -> c_int {
    if data.is_null() || dst.is_null() || dst_out.is_null() || need_flush.is_null() {
        return -EINVAL;
    }
    // SAFETY: PipeWire passes src_size readable bytes when src is non-null.
    let src = if src.is_null() {
        None
    } else {
        Some(unsafe { slice::from_raw_parts(src.cast::<u8>(), src_size) })
    };
    // SAFETY: PipeWire passes dst_size writable bytes and valid out-pointers.
    let dst = unsafe { slice::from_raw_parts_mut(dst.cast::<u8>(), dst_size) };
    // SAFETY: dst_out/need_flush are non-null and writable.
    let (dst_out, need_flush) = unsafe { (&mut *dst_out, &mut *need_flush) };
    // SAFETY: PipeWire passes the pointer from init.
    match unsafe { &mut *data.cast::<Session>() }.encode(src, dst, dst_out, need_flush) {
        Ok(consumed) => consumed as c_int,
        Err(error) => error,
    }
}

unsafe extern "C" fn codec_abr_process(_data: *mut c_void, _unsent: usize) -> c_int {
    -ENOTSUP
}

unsafe extern "C" fn codec_start_decode(
    _data: *mut c_void,
    _src: *const c_void,
    _src_size: usize,
    _seqnum: *mut u16,
    _timestamp: *mut u32,
) -> c_int {
    -ENOTSUP
}

unsafe extern "C" fn codec_decode(
    _data: *mut c_void,
    _src: *const c_void,
    _src_size: usize,
    _dst: *mut c_void,
    _dst_size: usize,
    _dst_out: *mut usize,
) -> c_int {
    -ENOTSUP
}

unsafe extern "C" fn codec_reduce_bitpool(_data: *mut c_void) -> c_int {
    0
}

unsafe extern "C" fn codec_increase_bitpool(_data: *mut c_void) -> c_int {
    0
}

unsafe extern "C" fn codec_get_delay(_data: *mut c_void, encoder: *mut u32, decoder: *mut u32) {
    // SAFETY: PipeWire passes valid-or-null out-pointers.
    unsafe {
        if let Some(encoder) = encoder.as_mut() {
            *encoder = ENCODER_DELAY_SAMPLES;
        }
        if let Some(decoder) = decoder.as_mut() {
            *decoder = 0;
        }
    }
}

const fn codec(mode: Mode, owner: bool) -> media_codec {
    media_codec {
        id: match mode {
            Mode::Ll => SPA_BLUETOOTH_AUDIO_CODEC_APTX_ADAPTIVE_LL,
            Mode::HiRes => SPA_BLUETOOTH_AUDIO_CODEC_APTX_ADAPTIVE_HIRES,
        },
        kind: MEDIA_CODEC_A2DP,
        codec_id: A2DP_CODEC_VENDOR,
        vendor: a2dp_vendor_codec_t {
            vendor_id: ids::VENDOR_ID,
            codec_id: ids::CODEC_ID,
        },
        name: mode.name().as_ptr(),
        description: mode.description().as_ptr(),
        endpoint_name: ids::ADAPTIVE_ENDPOINT.as_ptr(),
        info: ptr::null(),
        // The C plugin leaves this 0, so media-sink uses its default buffer.
        send_buf_size: 0,
        duplex_codec: ptr::null(),
        stream_pkt: false,
        get_bis_config: None,
        fill_caps: if owner { Some(codec_fill_caps) } else { None },
        select_config: Some(codec_select_config),
        enum_config: Some(codec_enum_config),
        validate_config: None,
        get_qos: None,
        get_metadata: None,
        free_config_data: None,
        caps_preference_cmp: None,
        init_props: None,
        clear_props: None,
        enum_props: None,
        set_props: None,
        init: Some(codec_init),
        deinit: Some(codec_deinit),
        update_props: None,
        get_block_size: Some(codec_get_block_size),
        get_interval: None,
        abr_process: Some(codec_abr_process),
        start_encode: Some(codec_start_encode),
        encode: Some(codec_encode),
        start_decode: Some(codec_start_decode),
        decode: Some(codec_decode),
        produce_plc: None,
        reduce_bitpool: Some(codec_reduce_bitpool),
        increase_bitpool: Some(codec_increase_bitpool),
        set_log: None,
        get_delay: Some(codec_get_delay),
    }
}

static LL_CODEC: SyncStatic<media_codec> = SyncStatic(codec(Mode::Ll, true));
static HIRES_CODEC: SyncStatic<media_codec> = SyncStatic(codec(Mode::HiRes, false));
static CODEC_LIST: SyncStatic<[*const media_codec; 3]> =
    SyncStatic([&raw const LL_CODEC.0, &raw const HIRES_CODEC.0, ptr::null()]);

fn codecs() -> *const *const media_codec {
    CODEC_LIST.0.as_ptr()
}

#[repr(C)]
struct Handle {
    handle: spa_handle,
    codec: spa_bluez5_codec_a2dp,
}

unsafe extern "C" fn handle_get_interface(
    handle: *mut spa_handle,
    type_: *const c_char,
    iface: *mut *mut c_void,
) -> c_int {
    if handle.is_null() || iface.is_null() || type_.is_null() {
        return -EINVAL;
    }
    // SAFETY: type_ is a NUL-terminated string from the caller.
    if unsafe { CStr::from_ptr(type_) } != ids::INTERFACE_TYPE {
        return -ENOENT;
    }
    let this = handle.cast::<Handle>();
    // SAFETY: handle is the first field of a Handle we initialised.
    unsafe { *iface = (&raw mut (*this).codec).cast() };
    0
}

unsafe extern "C" fn handle_clear(handle: *mut spa_handle) -> c_int {
    if handle.is_null() { -EINVAL } else { 0 }
}

unsafe extern "C" fn factory_get_size(
    _factory: *const spa_handle_factory,
    _params: *const spa_dict,
) -> usize {
    size_of::<Handle>()
}

unsafe extern "C" fn factory_init(
    factory: *const spa_handle_factory,
    handle: *mut spa_handle,
    _info: *const spa_dict,
    _support: *const spa_support,
    _n_support: u32,
) -> c_int {
    if factory.is_null() || handle.is_null() {
        return -EINVAL;
    }
    let this = handle.cast::<Handle>();
    // SAFETY: the loader allocates get_size() bytes for the handle, suitably aligned.
    unsafe {
        (*this).handle.version = SPA_VERSION_HANDLE;
        (*this).handle.get_interface = Some(handle_get_interface);
        (*this).handle.clear = Some(handle_clear);
        (&raw mut (*this).codec).write(spa_bluez5_codec_a2dp {
            iface: spa_interface {
                type_: ids::INTERFACE_TYPE.as_ptr(),
                version: SPA_VERSION_BLUEZ5_CODEC_MEDIA,
                cb: spa_callbacks {
                    funcs: ptr::null(),
                    data: this.cast(),
                },
            },
            codecs: codecs(),
        });
    }
    0
}

static INTERFACES: SyncStatic<[spa_interface_info; 1]> = SyncStatic([spa_interface_info {
    type_: ids::INTERFACE_TYPE.as_ptr(),
}]);

unsafe extern "C" fn factory_enum_interface_info(
    factory: *const spa_handle_factory,
    info: *mut *const spa_interface_info,
    index: *mut u32,
) -> c_int {
    if factory.is_null() || info.is_null() || index.is_null() {
        return -EINVAL;
    }
    // SAFETY: checked non-null.
    unsafe {
        match INTERFACES.0.get(*index as usize) {
            Some(interface) => {
                *info = interface;
                *index += 1;
                1
            }
            None => 0,
        }
    }
}

static FACTORY_INFO_ITEMS: SyncStatic<[spa_dict_item; 1]> = SyncStatic([spa_dict_item {
    key: c"factory.description".as_ptr(),
    value: ids::FACTORY_DESCRIPTION_NATIVE.as_ptr(),
}]);

static FACTORY_INFO: SyncStatic<spa_dict> = SyncStatic(spa_dict {
    flags: 0,
    n_items: 1,
    items: FACTORY_INFO_ITEMS.0.as_ptr(),
});

const fn factory(name: &'static CStr) -> spa_handle_factory {
    spa_handle_factory {
        version: SPA_VERSION_HANDLE_FACTORY,
        name: name.as_ptr(),
        info: &raw const FACTORY_INFO.0,
        get_size: Some(factory_get_size),
        init: Some(factory_init),
        enum_interface_info: Some(factory_enum_interface_info),
    }
}

static FACTORY: SyncStatic<spa_handle_factory> = SyncStatic(factory(ids::FACTORY_NAME_NATIVE));

pub(crate) fn factory_ref() -> &'static spa_handle_factory {
    &FACTORY.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::A2DP_MAX_CAPS_SIZE;
    use crate::ported::mode::{CH_JOINT, LATENCY_BYTES};

    fn sink_caps(rate_byte: u8, channel_modes: u8, features: Option<u32>) -> Vec<u8> {
        let mut caps = vec![0u8; BODY_SIZE];
        caps[..6].copy_from_slice(&ids::VENDOR_HEADER);
        caps[OFF_RATE_SOURCE] = rate_byte;
        caps[OFF_CHANNEL_MODE] = channel_modes;
        let features = features.unwrap_or(FEATURES_UNKNOWN);
        caps[OFF_FEATURES..OFF_FEATURES + 4].copy_from_slice(&features.to_le_bytes());
        caps
    }

    #[test]
    fn entries_are_ll_owner_and_hires_alias() {
        let ll = &LL_CODEC.0;
        let hires = &HIRES_CODEC.0;
        assert_eq!(ll.id, 0x302);
        assert_eq!(hires.id, 0x303);
        assert_eq!(ll.kind, MEDIA_CODEC_A2DP);
        assert_eq!(ll.codec_id, A2DP_CODEC_VENDOR);
        // a2dp_vendor_codec_t is packed, so read its fields unaligned.
        // SAFETY: addr_of! keeps the field reference out of the way; both
        // reads are within the static entry.
        let vendor_id = unsafe { ptr::read_unaligned(ptr::addr_of!(ll.vendor.vendor_id)) };
        // SAFETY: as above.
        let codec_id = unsafe { ptr::read_unaligned(ptr::addr_of!(ll.vendor.codec_id)) };
        assert_eq!(vendor_id, 0xD7);
        assert_eq!(codec_id, 0xAD);
        assert!(ll.fill_caps.is_some());
        assert!(hires.fill_caps.is_none());
        // SAFETY: static NUL-terminated names.
        assert_eq!(unsafe { CStr::from_ptr(ll.name) }, ids::ADAPTIVE_LL_NAME);
        assert_eq!(
            unsafe { CStr::from_ptr(hires.name) },
            ids::ADAPTIVE_HIRES_NAME
        );
        assert_eq!(
            unsafe { CStr::from_ptr(ll.endpoint_name) },
            ids::ADAPTIVE_ENDPOINT
        );
        assert_eq!(
            unsafe { CStr::from_ptr(hires.endpoint_name) },
            ids::ADAPTIVE_ENDPOINT
        );
    }

    #[test]
    fn ll_fill_caps_advertises_both_rates() {
        let mut caps = [0u8; A2DP_MAX_CAPS_SIZE];
        // SAFETY: the entry is ours and caps is A2DP_MAX_CAPS_SIZE bytes.
        let len =
            unsafe { codec_fill_caps(&raw const LL_CODEC.0, 0, ptr::null(), caps.as_mut_ptr()) };
        assert_eq!(len, BODY_SIZE as c_int);
        assert_eq!(caps[OFF_RATE_SOURCE], 0x30);
        assert_eq!(caps[OFF_CHANNEL_MODE], 0x0a);
        assert_eq!(caps[8..14], LATENCY_BYTES);
    }

    #[test]
    fn select_config_picks_the_mode_rate_and_channel() {
        let mut out = [0u8; A2DP_MAX_CAPS_SIZE];
        let caps = sink_caps(0x30, 0x0a, Some(0x0F00_0003));
        // SAFETY: the entry is ours; caps/out are valid buffers.
        let len = unsafe {
            codec_select_config(
                &raw const LL_CODEC.0,
                0,
                caps.as_ptr().cast(),
                caps.len(),
                ptr::null(),
                ptr::null(),
                out.as_mut_ptr(),
                ptr::null_mut(),
            )
        };
        assert_eq!(len, BODY_SIZE as c_int);
        assert_eq!(out[OFF_RATE_SOURCE], 0x10);
        assert_eq!(out[OFF_CHANNEL_MODE], CH_JOINT);
        let features = u32::from_le_bytes(out[OFF_FEATURES..OFF_FEATURES + 4].try_into().unwrap());
        assert_eq!(features, 0x0F00_0002);

        // The Hi-Res alias prefers 96 kHz when the sink offers it.
        let caps = sink_caps(0x30, 0x0a, None);
        // SAFETY: as above.
        let len = unsafe {
            codec_select_config(
                &raw const HIRES_CODEC.0,
                0,
                caps.as_ptr().cast(),
                caps.len(),
                ptr::null(),
                ptr::null(),
                out.as_mut_ptr(),
                ptr::null_mut(),
            )
        };
        assert_eq!(len, BODY_SIZE as c_int);
        assert_eq!(out[OFF_RATE_SOURCE], 0x20);
        let features = u32::from_le_bytes(out[OFF_FEATURES..OFF_FEATURES + 4].try_into().unwrap());
        assert_eq!(features, crate::ported::mode::FEATURES_SOURCE);

        // 48 kHz fallback.
        let caps = sink_caps(0x10, 0x02, None);
        // SAFETY: as above.
        let len = unsafe {
            codec_select_config(
                &raw const HIRES_CODEC.0,
                0,
                caps.as_ptr().cast(),
                caps.len(),
                ptr::null(),
                ptr::null(),
                out.as_mut_ptr(),
                ptr::null_mut(),
            )
        };
        assert_eq!(len, BODY_SIZE as c_int);
        assert_eq!(out[OFF_RATE_SOURCE], 0x10);
        assert_eq!(out[OFF_CHANNEL_MODE], 0x02);

        // A sink offering none of the mode's rates is rejected.
        let caps = sink_caps(0x40, 0x02, None);
        // SAFETY: as above.
        let len = unsafe {
            codec_select_config(
                &raw const HIRES_CODEC.0,
                0,
                caps.as_ptr().cast(),
                caps.len(),
                ptr::null(),
                ptr::null(),
                out.as_mut_ptr(),
                ptr::null_mut(),
            )
        };
        assert_eq!(len, -EINVAL);
    }

    #[test]
    fn enum_config_builds_an_s32_stereo_pod() {
        use crate::ffi::{
            SPA_PARAM_ENUM_FORMAT, SPA_TYPE_OBJECT, SPA_TYPE_OBJECT_FORMAT, spa_pod_builder_state,
        };
        let mut data = vec![0u8; 512];
        let mut builder = spa_pod_builder {
            data: data.as_mut_ptr().cast(),
            size: data.len() as u32,
            _padding: 0,
            state: spa_pod_builder_state {
                offset: 0,
                flags: 0,
                frame: ptr::null_mut(),
            },
            callbacks: spa_callbacks {
                funcs: ptr::null(),
                data: ptr::null_mut(),
            },
        };
        let mut param: *mut spa_pod = ptr::null_mut();
        let caps = sink_caps(0x10, 0x0a, None);
        // SAFETY: the builder and param are valid for this call.
        let res = unsafe {
            codec_enum_config(
                &raw const LL_CODEC.0,
                0,
                caps.as_ptr().cast(),
                caps.len(),
                SPA_PARAM_ENUM_FORMAT,
                0,
                &raw mut builder,
                &raw mut param,
            )
        };
        assert_eq!(res, 1);
        assert!(!param.is_null());
        // SAFETY: the pod was just built in data.
        assert_eq!(unsafe { (*param).type_ }, SPA_TYPE_OBJECT);
        // The format object id follows the type field.
        assert_eq!(
            u32::from_ne_bytes(data[8..12].try_into().unwrap()),
            SPA_TYPE_OBJECT_FORMAT
        );
        // The rate property value (SPA_FORMAT_AUDIO_RATE = 0x10003) is 48000.
        let rate_key = 0x0001_0003u32.to_ne_bytes();
        let at = data
            .windows(4)
            .position(|window| window == rate_key)
            .expect("rate property");
        assert_eq!(
            u32::from_ne_bytes(data[at + 16..at + 20].try_into().unwrap()),
            48_000
        );
        // The format property immediately precedes the rate property.
        assert_eq!(
            u32::from_ne_bytes(data[at - 8..at - 4].try_into().unwrap()),
            SPA_AUDIO_FORMAT_S32_LE
        );
        // Channels follows rate: value at key + 8 (header) + 8 (property header).
        assert_eq!(
            u32::from_ne_bytes(data[at + 40..at + 44].try_into().unwrap()),
            2
        );
    }

    #[test]
    fn factory_is_exported_with_the_port_name() {
        let factory = factory_ref();
        assert_eq!(factory.version, SPA_VERSION_HANDLE_FACTORY);
        // SAFETY: static NUL-terminated name.
        assert_eq!(
            unsafe { CStr::from_ptr(factory.name) },
            ids::FACTORY_NAME_NATIVE
        );
        // SAFETY: our statics.
        let list = unsafe { slice::from_raw_parts(codecs(), 3) };
        assert!(!list[0].is_null());
        assert!(!list[1].is_null());
        assert!(list[2].is_null());
    }
}

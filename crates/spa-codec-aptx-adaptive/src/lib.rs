//! PipeWire bluez5 media codec plugin for aptX Adaptive.
//!
//! Loaded in place of LC3plus HR (see `docs/adr/0001-architecture.md`). This file is only the
//! C ABI glue; codec behaviour lives in `caps` and `session`.

mod caps;
mod ffi;
mod ids;
mod pod;
mod session;

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;
use std::slice;

use ffi::{
    A2DP_CODEC_VENDOR, A2DP_MAX_CAPS_SIZE, EINVAL, ENOENT, ENOSPC, ENOTSUP, MEDIA_CODEC_A2DP,
    NEED_FLUSH_ALL, NEED_FLUSH_NO, SPA_BLUETOOTH_AUDIO_CODEC_LC3PLUS_HR, SPA_MEDIA_SUBTYPE_RAW,
    SPA_MEDIA_TYPE_AUDIO, SPA_VERSION_BLUEZ5_CODEC_MEDIA, SPA_VERSION_HANDLE,
    SPA_VERSION_HANDLE_FACTORY, a2dp_vendor_codec_t, media_codec, media_codec_audio_info,
    spa_audio_info, spa_audio_info_raw, spa_bluez5_codec_a2dp, spa_callbacks, spa_dict,
    spa_dict_item, spa_handle, spa_handle_factory, spa_interface, spa_interface_info, spa_pod,
    spa_pod_builder, spa_support,
};
use session::{Session, SessionError};

// A panic in any extern "C" fn below aborts the process instead of unwinding into PipeWire;
// we rely on that rather than catch_unwind.

/// Lets FFI structs holding raw pointers live in statics.
#[repr(transparent)]
struct SyncStatic<T>(T);
// SAFETY: only used for immutable statics whose pointers lead to other statics or
// string literals.
unsafe impl<T> Sync for SyncStatic<T> {}

unsafe extern "C" {
    fn __errno_location() -> *mut c_int;
}

fn set_errno(e: c_int) {
    // SAFETY: glibc and musl both provide a valid thread-local errno pointer.
    unsafe { *__errno_location() = e };
}

static CODEC: SyncStatic<media_codec> = SyncStatic(media_codec {
    id: SPA_BLUETOOTH_AUDIO_CODEC_LC3PLUS_HR,
    kind: MEDIA_CODEC_A2DP,
    codec_id: A2DP_CODEC_VENDOR,
    vendor: a2dp_vendor_codec_t {
        vendor_id: ids::VENDOR_ID,
        codec_id: ids::CODEC_ID,
    },
    name: ids::CODEC_NAME.as_ptr(),
    description: ids::CODEC_DESCRIPTION.as_ptr(),
    endpoint_name: ptr::null(),
    info: ptr::null(),
    send_buf_size: 0,
    duplex_codec: ptr::null(),
    stream_pkt: false,
    get_bis_config: None,
    fill_caps: Some(codec_fill_caps),
    select_config: Some(codec_select_config),
    enum_config: Some(codec_enum_config),
    validate_config: Some(codec_validate_config),
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
    get_interval: Some(codec_get_interval),
    abr_process: Some(codec_abr_process),
    start_encode: Some(codec_start_encode),
    encode: Some(codec_encode),
    start_decode: None,
    decode: None,
    produce_plc: None,
    reduce_bitpool: None,
    increase_bitpool: None,
    set_log: None,
    get_delay: Some(codec_get_delay),
});

static CODECS: SyncStatic<[*const media_codec; 2]> = SyncStatic([&raw const CODEC.0, ptr::null()]);

fn caps_err(e: caps::CapsError) -> c_int {
    match e {
        caps::CapsError::Invalid => -EINVAL,
        caps::CapsError::Unsupported => -ENOTSUP,
    }
}

fn session_err(e: SessionError) -> c_int {
    match e {
        SessionError::Invalid => -EINVAL,
        SessionError::NoSpace => -ENOSPC,
        SessionError::Unsupported => -ENOTSUP,
    }
}

/// # Safety
/// `p` must be null or valid for `len` bytes.
unsafe fn bytes<'a>(p: *const c_void, len: usize) -> Option<&'a [u8]> {
    if p.is_null() {
        None
    } else {
        // SAFETY: caller contract.
        Some(unsafe { slice::from_raw_parts(p.cast::<u8>(), len) })
    }
}

fn write_out(src: &[u8], dst: *mut u8) -> c_int {
    if src.len() > A2DP_MAX_CAPS_SIZE || dst.is_null() {
        return -EINVAL;
    }
    // SAFETY: PipeWire passes a buffer of A2DP_MAX_CAPS_SIZE bytes.
    unsafe { ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len()) };
    src.len() as c_int
}

unsafe extern "C" fn codec_fill_caps(
    _codec: *const media_codec,
    _flags: u32,
    _settings: *const spa_dict,
    caps_out: *mut u8,
) -> c_int {
    write_out(&caps::fill_caps(), caps_out)
}

unsafe extern "C" fn codec_select_config(
    _codec: *const media_codec,
    _flags: u32,
    caps: *const c_void,
    caps_size: usize,
    _info: *const media_codec_audio_info,
    _settings: *const spa_dict,
    config: *mut u8,
    _config_data: *mut *mut c_void,
) -> c_int {
    // SAFETY: PipeWire passes caps_size readable bytes.
    let Some(caps) = (unsafe { bytes(caps, caps_size) }) else {
        return -EINVAL;
    };
    match caps::select_config(caps) {
        Ok(c) => write_out(&c, config),
        Err(e) => caps_err(e),
    }
}

/// Appends `data` to `b` like `spa_pod_builder_raw`: on overflow the offset and frame sizes
/// still advance (so callers can see how much space was needed) and `-ENOSPC` is returned
/// unless the overflow callback grew the buffer.
///
/// # Safety
/// `b` must be a valid, initialised builder.
unsafe fn builder_append(b: *mut spa_pod_builder, data: &[u8]) -> Result<*mut spa_pod, c_int> {
    // SAFETY: caller contract; fields are plain data.
    unsafe {
        let len = u32::try_from(data.len()).map_err(|_| -ENOSPC)?;
        let offset = (*b).state.offset;
        let end = offset.checked_add(len).ok_or(-ENOSPC)?;
        let mut res = 0;
        if end > (*b).size {
            res = -ENOSPC;
            let cbs = &(*b).callbacks;
            let funcs = cbs.funcs.cast::<ffi::spa_pod_builder_callbacks>();
            if let Some(overflow) = funcs.as_ref().and_then(|f| f.overflow)
                && offset <= (*b).size
            {
                res = overflow(cbs.data, end);
            }
        }
        let dst = (*b).data.cast::<u8>().wrapping_add(offset as usize);
        if res == 0 {
            ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
        }
        (*b).state.offset = end;
        let mut f = (*b).state.frame;
        while let Some(frame) = f.as_mut() {
            frame.pod.size += len;
            f = frame.parent;
        }
        if res < 0 {
            return Err(res);
        }
        #[expect(
            clippy::cast_ptr_alignment,
            reason = "builder buffers are 8-byte aligned and pods keep the offset padded to 8"
        )]
        Ok(dst.cast::<spa_pod>())
    }
}

unsafe extern "C" fn codec_enum_config(
    _codec: *const media_codec,
    _flags: u32,
    caps: *const c_void,
    caps_size: usize,
    id: u32,
    idx: u32,
    builder: *mut spa_pod_builder,
    param: *mut *mut spa_pod,
) -> c_int {
    // SAFETY: PipeWire passes caps_size readable bytes.
    let Some(caps) = (unsafe { bytes(caps, caps_size) }) else {
        return -EINVAL;
    };
    let format = match caps::validate_config(caps) {
        Ok(f) => f,
        Err(e) => return caps_err(e),
    };
    if idx > 0 {
        return 0;
    }
    if builder.is_null() || param.is_null() {
        return -EINVAL;
    }
    let pod = pod::raw_audio_format(id, &format);
    // SAFETY: builder checked non-null and owned by the caller for this call.
    match unsafe { builder_append(builder, &pod) } {
        Ok(p) => {
            // SAFETY: param checked non-null.
            unsafe { *param = p };
            1
        }
        Err(e) => e,
    }
}

fn audio_info(format: &pod::RawFormat) -> spa_audio_info {
    let mut raw = spa_audio_info_raw {
        format: format.format,
        flags: 0,
        rate: format.rate,
        channels: format.positions.len() as u32,
        position: [0; ffi::SPA_AUDIO_MAX_CHANNELS],
    };
    raw.position[..format.positions.len()].copy_from_slice(format.positions);
    spa_audio_info {
        media_type: SPA_MEDIA_TYPE_AUDIO,
        media_subtype: SPA_MEDIA_SUBTYPE_RAW,
        raw,
        _union_tail: [0],
    }
}

unsafe extern "C" fn codec_validate_config(
    _codec: *const media_codec,
    _flags: u32,
    caps: *const c_void,
    caps_size: usize,
    info: *mut spa_audio_info,
) -> c_int {
    // SAFETY: PipeWire passes caps_size readable bytes.
    let Some(caps) = (unsafe { bytes(caps, caps_size) }) else {
        return -EINVAL;
    };
    if info.is_null() {
        return -EINVAL;
    }
    match caps::validate_config(caps) {
        Ok(f) => {
            // SAFETY: info checked non-null and points at a struct spa_audio_info.
            unsafe { info.write(audio_info(&f)) };
            0
        }
        Err(e) => caps_err(e),
    }
}

unsafe extern "C" fn codec_init(
    _codec: *const media_codec,
    _flags: u32,
    config: *mut c_void,
    config_size: usize,
    info: *const spa_audio_info,
    _props: *mut c_void,
    mtu: usize,
) -> *mut c_void {
    let res = (|| {
        // SAFETY: PipeWire passes config_size readable bytes.
        let config = unsafe { bytes(config, config_size) }.ok_or(-EINVAL)?;
        // SAFETY: PipeWire passes a valid spa_audio_info or null.
        let info = unsafe { info.as_ref() }.ok_or(-EINVAL)?;
        let stream = caps::PcmStream {
            raw_audio: info.media_type == SPA_MEDIA_TYPE_AUDIO
                && info.media_subtype == SPA_MEDIA_SUBTYPE_RAW,
            format: info.raw.format,
            rate: info.raw.rate,
            channels: info.raw.channels,
        };
        let format = caps::check_stream(config, &stream).map_err(caps_err)?;
        Session::new(&format, mtu).map_err(|e| session_err(e.into()))
    })();
    match res {
        Ok(s) => Box::into_raw(Box::new(s)).cast(),
        Err(e) => {
            set_errno(-e);
            ptr::null_mut()
        }
    }
}

/// # Safety
/// `data` must come from `codec_init` and not have been passed to `codec_deinit`.
unsafe fn session<'a>(data: *mut c_void) -> &'a mut Session<aptx_adaptive::Encoder> {
    // SAFETY: caller contract.
    unsafe { &mut *data.cast() }
}

unsafe extern "C" fn codec_deinit(data: *mut c_void) {
    if !data.is_null() {
        // SAFETY: data came from Box::into_raw in codec_init.
        drop(unsafe { Box::from_raw(data.cast::<Session<aptx_adaptive::Encoder>>()) });
    }
}

unsafe extern "C" fn codec_get_block_size(data: *mut c_void) -> c_int {
    // SAFETY: PipeWire passes the pointer from init.
    unsafe { session(data) }.block_size() as c_int
}

unsafe extern "C" fn codec_get_interval(data: *mut c_void) -> u64 {
    // SAFETY: PipeWire passes the pointer from init.
    unsafe { session(data) }.interval_ns()
}

unsafe extern "C" fn codec_abr_process(data: *mut c_void, unsent: usize) -> c_int {
    // SAFETY: PipeWire passes the pointer from init.
    match unsafe { session(data) }.abr_process(unsent) {
        Ok(()) => 0,
        Err(e) => session_err(e),
    }
}

unsafe extern "C" fn codec_start_encode(
    data: *mut c_void,
    dst: *mut c_void,
    dst_size: usize,
    seqnum: u16,
    timestamp: u32,
) -> c_int {
    if dst.is_null() {
        return -EINVAL;
    }
    // SAFETY: PipeWire passes dst_size writable bytes.
    let dst = unsafe { slice::from_raw_parts_mut(dst.cast::<u8>(), dst_size) };
    // SAFETY: PipeWire passes the pointer from init.
    match unsafe { session(data) }.start_encode(dst, seqnum, timestamp) {
        Ok(n) => n as c_int,
        Err(e) => session_err(e),
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
    if dst_out.is_null() || need_flush.is_null() || dst.is_null() {
        return -EINVAL;
    }
    // SAFETY: checked non-null; PipeWire passes valid out-pointers.
    unsafe { *dst_out = 0 };
    // No fragmentation support: a NULL src only follows NEED_FLUSH_FRAGMENT, which we never set.
    // SAFETY: PipeWire passes src_size readable bytes.
    let Some(src) = (unsafe { bytes(src, src_size) }) else {
        return -EINVAL;
    };
    // SAFETY: PipeWire passes dst_size writable bytes.
    let dst = unsafe { slice::from_raw_parts_mut(dst.cast::<u8>(), dst_size) };
    // SAFETY: PipeWire passes the pointer from init.
    match unsafe { session(data) }.encode(src, dst) {
        Ok(r) => {
            // SAFETY: checked non-null above.
            unsafe {
                *dst_out = r.written;
                *need_flush = if r.flush {
                    NEED_FLUSH_ALL
                } else {
                    NEED_FLUSH_NO
                };
            }
            r.consumed as c_int
        }
        Err(e) => session_err(e),
    }
}

unsafe extern "C" fn codec_get_delay(data: *mut c_void, encoder: *mut u32, decoder: *mut u32) {
    // SAFETY: PipeWire passes the pointer from init and valid-or-null out-pointers.
    unsafe {
        if let Some(e) = encoder.as_mut() {
            *e = session(data).delay_samples();
        }
        if let Some(d) = decoder.as_mut() {
            *d = 0;
        }
    }
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
            codecs: CODECS.0.as_ptr(),
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
            Some(i) => {
                *info = i;
                *index += 1;
                1
            }
            None => 0,
        }
    }
}

static FACTORY_INFO_ITEMS: SyncStatic<[spa_dict_item; 1]> = SyncStatic([spa_dict_item {
    key: c"factory.description".as_ptr(),
    value: c"Bluetooth codec plugin (aptX Adaptive)".as_ptr(),
}]);

static FACTORY_INFO: SyncStatic<spa_dict> = SyncStatic(spa_dict {
    flags: 0,
    n_items: 1,
    items: FACTORY_INFO_ITEMS.0.as_ptr(),
});

static FACTORY: SyncStatic<spa_handle_factory> = SyncStatic(spa_handle_factory {
    version: SPA_VERSION_HANDLE_FACTORY,
    name: ids::FACTORY_NAME.as_ptr(),
    info: &raw const FACTORY_INFO.0,
    get_size: Some(factory_get_size),
    init: Some(factory_init),
    enum_interface_info: Some(factory_enum_interface_info),
});

/// SPA plugin entry point.
///
/// # Safety
/// `factory` and `index` must be valid pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spa_handle_factory_enum(
    factory: *mut *const spa_handle_factory,
    index: *mut u32,
) -> c_int {
    if factory.is_null() || index.is_null() {
        return -EINVAL;
    }
    // SAFETY: checked non-null.
    unsafe {
        if *index != 0 {
            return 0;
        }
        *factory = &raw const FACTORY.0;
        *index += 1;
    }
    1
}

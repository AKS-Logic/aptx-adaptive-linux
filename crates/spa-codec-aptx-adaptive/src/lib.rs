//! PipeWire bluez5 media codec plugin for aptX Adaptive.
//!
//! C ABI glue for the plugin; codec behaviour lives in `caps` and `session`.

#[cfg(test)]
#[path = "../../aptx-adaptive/tests/support/alloc_count.rs"]
mod alloc_count;
mod caps;
mod dev;
mod drift;
mod ffi;
#[cfg(test)]
mod fixtures;
mod ids;
mod pod;
mod props;
mod session;
mod slot;

use std::ffi::{CStr, c_char, c_int, c_void};
use std::io::Write as _;
use std::ptr;
use std::slice;
use std::sync::Once;

use ffi::{
    A2DP_CODEC_VENDOR, A2DP_MAX_CAPS_SIZE, EINVAL, ENOENT, ENOSPC, ENOTSUP, MEDIA_CODEC_A2DP,
    NEED_FLUSH_ALL, NEED_FLUSH_NO, SPA_BLUETOOTH_AUDIO_CODEC_MPEG, SPA_MEDIA_SUBTYPE_RAW,
    SPA_MEDIA_TYPE_AUDIO, SPA_VERSION_BLUEZ5_CODEC_MEDIA, SPA_VERSION_HANDLE,
    SPA_VERSION_HANDLE_FACTORY, a2dp_vendor_codec_t, media_codec, media_codec_audio_info,
    spa_audio_info, spa_audio_info_raw, spa_bluez5_codec_a2dp, spa_callbacks, spa_dict,
    spa_dict_item, spa_handle, spa_handle_factory, spa_interface, spa_interface_info, spa_pod,
    spa_pod_builder, spa_support,
};
use props::Props;
use session::{Session, SessionError};
use slot::{DefaultMode, Slot};

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

const fn codec(mode: slot::DefaultMode, owner: Option<slot::Slot>) -> media_codec {
    let (name, description) = match mode {
        slot::DefaultMode::Hq => (ids::CODEC_NAME, ids::CODEC_DESCRIPTION),
        slot::DefaultMode::Ll => (ids::LL_NAME, ids::LL_DESCRIPTION),
    };
    let id = match owner {
        Some(slot) => slot.id(),
        None => SPA_BLUETOOTH_AUDIO_CODEC_MPEG,
    };
    let fill_caps = match owner {
        Some(_) => Some(codec_fill_caps as _),
        None => None,
    };
    media_codec {
        id,
        kind: MEDIA_CODEC_A2DP,
        codec_id: A2DP_CODEC_VENDOR,
        vendor: a2dp_vendor_codec_t {
            vendor_id: ids::VENDOR_ID,
            codec_id: ids::CODEC_ID,
        },
        name: name.as_ptr(),
        description: description.as_ptr(),
        endpoint_name: ids::CODEC_NAME.as_ptr(),
        info: ptr::null(),
        send_buf_size: ids::SEND_BUF_SIZE,
        duplex_codec: ptr::null(),
        stream_pkt: false,
        get_bis_config: None,
        fill_caps,
        select_config: Some(codec_select_config),
        enum_config: Some(codec_enum_config),
        validate_config: Some(codec_validate_config),
        get_qos: None,
        get_metadata: None,
        free_config_data: None,
        caps_preference_cmp: None,
        init_props: Some(codec_init_props),
        clear_props: Some(codec_clear_props),
        enum_props: Some(codec_enum_props),
        set_props: Some(codec_set_props),
        init: Some(codec_init),
        deinit: Some(codec_deinit),
        update_props: Some(codec_update_props),
        get_block_size: Some(codec_get_block_size),
        get_interval: Some(codec_get_interval),
        abr_process: Some(codec_abr_process),
        start_encode: Some(codec_start_encode),
        encode: Some(codec_encode),
        start_decode: None,
        decode: None,
        produce_plc: None,
        reduce_bitpool: Some(codec_reduce_bitpool),
        increase_bitpool: Some(codec_increase_bitpool),
        set_log: None,
        get_delay: Some(codec_get_delay),
    }
}

static HQ_LDAC: SyncStatic<media_codec> = SyncStatic(codec(DefaultMode::Hq, Some(Slot::Ldac)));
static HQ_LC3PLUS_HR: SyncStatic<media_codec> =
    SyncStatic(codec(DefaultMode::Hq, Some(Slot::Lc3plusHr)));
static HQ_ALIAS: SyncStatic<media_codec> = SyncStatic(codec(DefaultMode::Hq, None));
static LL_LDAC: SyncStatic<media_codec> = SyncStatic(codec(DefaultMode::Ll, Some(Slot::Ldac)));
static LL_LC3PLUS_HR: SyncStatic<media_codec> =
    SyncStatic(codec(DefaultMode::Ll, Some(Slot::Lc3plusHr)));
static LL_ALIAS: SyncStatic<media_codec> = SyncStatic(codec(DefaultMode::Ll, None));

type CodecList = SyncStatic<[*const media_codec; 3]>;
static CODECS_HQ_LDAC: CodecList =
    SyncStatic([&raw const HQ_LDAC.0, &raw const LL_ALIAS.0, ptr::null()]);
static CODECS_HQ_LC3PLUS_HR: CodecList = SyncStatic([
    &raw const HQ_LC3PLUS_HR.0,
    &raw const LL_ALIAS.0,
    ptr::null(),
]);
static CODECS_LL_LDAC: CodecList =
    SyncStatic([&raw const LL_LDAC.0, &raw const HQ_ALIAS.0, ptr::null()]);
static CODECS_LL_LC3PLUS_HR: CodecList = SyncStatic([
    &raw const LL_LC3PLUS_HR.0,
    &raw const HQ_ALIAS.0,
    ptr::null(),
]);

fn entry_profile(codec: *const media_codec) -> Option<aptx_adaptive::Profile> {
    let is =
        |list: &[&SyncStatic<media_codec>]| list.iter().any(|c| ptr::eq(codec, &raw const c.0));
    if is(&[&LL_LDAC, &LL_LC3PLUS_HR, &LL_ALIAS]) {
        Some(aptx_adaptive::Profile::Ll)
    } else if is(&[&HQ_ALIAS]) {
        Some(aptx_adaptive::Profile::Hq)
    } else {
        None
    }
}

fn codecs() -> *const *const media_codec {
    static BAD_ENV_LOGGED: Once = Once::new();
    let var = |k: &str| std::env::var_os(k);
    let slot = Slot::from_env(var);
    let mode = DefaultMode::from_env(var);
    BAD_ENV_LOGGED.call_once(|| {
        for e in [slot.as_ref().err(), mode.as_ref().err()]
            .into_iter()
            .flatten()
        {
            let _ = writeln!(std::io::stderr(), "aptx-adaptive: {e}");
        }
    });
    let list = match (
        slot.unwrap_or(Slot::DEFAULT),
        mode.unwrap_or(DefaultMode::DEFAULT),
    ) {
        (Slot::Ldac, DefaultMode::Hq) => &CODECS_HQ_LDAC,
        (Slot::Lc3plusHr, DefaultMode::Hq) => &CODECS_HQ_LC3PLUS_HR,
        (Slot::Ldac, DefaultMode::Ll) => &CODECS_LL_LDAC,
        (Slot::Lc3plusHr, DefaultMode::Ll) => &CODECS_LL_LC3PLUS_HR,
    };
    list.0.as_ptr()
}

fn caps_err(e: caps::CapsError) -> c_int {
    match e {
        caps::CapsError::Invalid => -EINVAL,
        caps::CapsError::Unsupported => -ENOTSUP,
    }
}

fn session_err(e: SessionError) -> c_int {
    match e {
        SessionError::NoSpace => -ENOSPC,
        SessionError::Unsupported => -ENOTSUP,
        SessionError::BadDevSetting => -EINVAL,
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
    if let Err(e) = caps::validate_config(caps) {
        return caps_err(e);
    }
    if idx > 0 {
        return 0;
    }
    if builder.is_null() || param.is_null() {
        return -EINVAL;
    }
    let pod = pod::raw_audio_format(id, &caps::STREAM);
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
        Ok(_) => {
            // SAFETY: info checked non-null and points at a struct spa_audio_info.
            unsafe { info.write(audio_info(&caps::STREAM)) };
            0
        }
        Err(e) => caps_err(e),
    }
}

/// # Safety
/// `dict` must be null or a valid `spa_dict` whose strings are NUL-terminated.
unsafe fn dict_lookup<'a>(dict: *const spa_dict, key: &str) -> Option<&'a str> {
    // SAFETY: caller contract.
    let d = unsafe { dict.as_ref() }?;
    if d.items.is_null() {
        return None;
    }
    // SAFETY: a spa_dict holds n_items items.
    let items = unsafe { slice::from_raw_parts(d.items, d.n_items as usize) };
    items.iter().find_map(|it| {
        if it.key.is_null() || it.value.is_null() {
            return None;
        }
        // SAFETY: caller contract.
        let (k, v) = unsafe { (CStr::from_ptr(it.key), CStr::from_ptr(it.value)) };
        (k.to_bytes() == key.as_bytes())
            .then(|| v.to_str().ok())
            .flatten()
    })
}

/// # Safety
/// `props` must be null or come from `codec_init_props` and not have been cleared.
/// `set_props` runs on the main thread and `update_props` on the data thread; [`Props`] is
/// atomics only, so a shared reference is all either needs.
unsafe fn shared_props<'a>(props: *mut c_void) -> Option<&'a Props> {
    // SAFETY: caller contract.
    unsafe { props.cast::<Props>().as_ref() }
}

unsafe extern "C" fn codec_init_props(
    codec: *const media_codec,
    _flags: u32,
    settings: *const spa_dict,
) -> *mut c_void {
    let p = entry_profile(codec).map_or_else(
        || {
            // SAFETY: PipeWire passes the device settings dict or null.
            let mode = unsafe { dict_lookup(settings, props::MODE_SETTING) };
            let (p, msg) = Props::from_setting(mode);
            if let Some(msg) = msg {
                let _ = writeln!(std::io::stderr(), "{msg}");
            }
            p
        },
        Props::new,
    );
    Box::into_raw(Box::new(p)).cast()
}

unsafe extern "C" fn codec_clear_props(props: *mut c_void) {
    if !props.is_null() {
        // SAFETY: props came from Box::into_raw in codec_init_props.
        drop(unsafe { Box::from_raw(props.cast::<Props>()) });
    }
}

unsafe extern "C" fn codec_enum_props(
    props: *mut c_void,
    _settings: *const spa_dict,
    id: u32,
    idx: u32,
    builder: *mut spa_pod_builder,
    param: *mut *mut spa_pod,
) -> c_int {
    // SAFETY: PipeWire passes the pointer from init_props.
    let Some(props) = (unsafe { shared_props(props) }) else {
        return -EINVAL;
    };
    let pod = match props.enum_param(id, idx) {
        Err(()) => return -ENOENT,
        Ok(None) => return 0,
        Ok(Some(pod)) => pod,
    };
    if builder.is_null() || param.is_null() {
        return -EINVAL;
    }
    // SAFETY: builder checked non-null and owned by the caller for this call.
    match unsafe { builder_append(builder, &pod) } {
        Ok(pod) => {
            // SAFETY: param checked non-null.
            unsafe { *param = pod };
            1
        }
        Err(e) => e,
    }
}

unsafe extern "C" fn codec_set_props(props: *mut c_void, param: *const spa_pod) -> c_int {
    // SAFETY: PipeWire passes the pointer from init_props.
    let Some(props) = (unsafe { shared_props(props) }) else {
        return -EINVAL;
    };
    // SAFETY: a non-null param is a whole pod: an 8-byte header and `size` bytes of body.
    let pod = unsafe { param.as_ref() }
        .map(|h| unsafe { bytes(param.cast(), h.size as usize + size_of::<spa_pod>()) });
    let pod = match pod {
        None => None,
        Some(Some(b)) => Some(b),
        Some(None) => return -EINVAL,
    };
    c_int::from(props.set(pod))
}

unsafe extern "C" fn codec_update_props(data: *mut c_void, props: *mut c_void) -> c_int {
    // SAFETY: PipeWire passes the pointers from init and init_props.
    let (Some(props), false) = (unsafe { shared_props(props) }, data.is_null()) else {
        return 0;
    };
    // SAFETY: PipeWire passes the pointer from init.
    let s = unsafe { session(data) };
    let res = s.request(props.wanted());
    props.report(s.target(), s.can_run(aptx_adaptive::Profile::Ll));
    match res {
        Ok(()) => 0,
        Err(e) => session_err(e),
    }
}

unsafe extern "C" fn codec_init(
    codec: *const media_codec,
    _flags: u32,
    config: *mut c_void,
    config_size: usize,
    info: *const spa_audio_info,
    props: *mut c_void,
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
        let level = caps::check_stream(config, &stream).map_err(caps_err)?;
        let var = |k: &str| std::env::var_os(k);
        // Write errors are ignored so logging cannot panic here.
        let opts = dev::Options::from_env(var).map_err(|e| {
            let _ = writeln!(std::io::stderr(), "aptx-adaptive: {e}");
            -EINVAL
        })?;
        if let Some(msg) = opts.describe(var) {
            let _ = writeln!(std::io::stderr(), "{msg}");
        }
        // SAFETY: PipeWire passes the pointer from init_props, or null.
        let props = unsafe { shared_props(props) };
        let wanted = props.map_or_else(
            || entry_profile(codec).unwrap_or(aptx_adaptive::Profile::Hq),
            Props::wanted,
        );
        let s: Session<dev::AnyCore> =
            Session::open(level, wanted, mtu, &opts).map_err(session_err)?;
        if s.profile() != wanted {
            let _ = writeln!(
                std::io::stderr(),
                "aptx-adaptive: this link can't run {wanted:?}; starting in {:?}",
                s.profile()
            );
        }
        if let Some(p) = props {
            p.report(s.profile(), s.can_run(aptx_adaptive::Profile::Ll));
        }
        Ok::<_, c_int>(s)
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
unsafe fn session<'a>(data: *mut c_void) -> &'a mut Session<dev::AnyCore> {
    // SAFETY: caller contract.
    unsafe { &mut *data.cast() }
}

unsafe extern "C" fn codec_deinit(data: *mut c_void) {
    if !data.is_null() {
        // SAFETY: data came from Box::into_raw in codec_init.
        drop(unsafe { Box::from_raw(data.cast::<Session<dev::AnyCore>>()) });
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
    unsafe { session(data) }.abr_process(unsent);
    0
}

unsafe extern "C" fn codec_reduce_bitpool(data: *mut c_void) -> c_int {
    // SAFETY: PipeWire passes the pointer from init.
    unsafe { step_level(data, true) }
}

unsafe extern "C" fn codec_increase_bitpool(data: *mut c_void) -> c_int {
    // SAFETY: PipeWire passes the pointer from init.
    unsafe { step_level(data, false) }
}

/// # Safety
/// As for [`session`].
unsafe fn step_level(data: *mut c_void, down: bool) -> c_int {
    // SAFETY: caller contract; PipeWire passes the pointer from init, as to every other
    // session callback.
    match unsafe { session(data) }.step_level(down) {
        Ok(l) => c_int::from(l),
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

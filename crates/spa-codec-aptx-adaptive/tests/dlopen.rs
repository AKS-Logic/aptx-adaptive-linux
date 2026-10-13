//! Loads the built cdylib and walks it the way PipeWire's `codec-loader.c` does.

#[expect(
    dead_code,
    reason = "shared with the cdylib; the test only touches part of the FFI surface"
)]
#[path = "../src/ffi.rs"]
mod ffi;
#[path = "../src/fixtures.rs"]
mod fixtures;
#[path = "../src/ids.rs"]
mod ids;

use std::ffi::{CStr, c_int, c_void};
use std::path::PathBuf;
use std::process::Command;
use std::ptr;
use std::sync::OnceLock;

use ffi::{
    A2DP_CODEC_VENDOR, A2DP_MAX_CAPS_SIZE, EINVAL, ENOENT, ENOTSUP, MEDIA_CODEC_A2DP,
    SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR, SPA_AUDIO_FORMAT_S24_32_LE,
    SPA_BLUETOOTH_AUDIO_CODEC_LC3PLUS_HR, SPA_BLUETOOTH_AUDIO_CODEC_LDAC,
    SPA_BLUETOOTH_AUDIO_CODEC_MPEG, SPA_MEDIA_SUBTYPE_RAW, SPA_MEDIA_TYPE_AUDIO,
    SPA_PARAM_ENUM_FORMAT, SPA_PARAM_PROP_INFO, SPA_PARAM_PROPS, SPA_PROP_QUALITY, SPA_TYPE_INT,
    SPA_TYPE_OBJECT, SPA_TYPE_OBJECT_PROP_INFO, SPA_TYPE_OBJECT_PROPS,
    SPA_VERSION_BLUEZ5_CODEC_MEDIA, SPA_VERSION_HANDLE_FACTORY, media_codec,
    media_codec_audio_info, spa_audio_info, spa_bluez5_codec_a2dp, spa_dict, spa_dict_item,
    spa_handle, spa_handle_factory, spa_pod, spa_pod_builder,
};
use fixtures::{PX7_S2E, PX7_S2E_CONFIG};
use ids::{CAPS_LEN, VENDOR_HEADER};

const MTU: usize = 895;
/// 2 ms of stereo `S24_32`: divides both the 14 ms HQ and the 10 ms LL packet (spec
/// `a2dp-integration.md` §4.1, §4.2).
const BLOCK_SIZE: c_int = 96 * 2 * 4;

type EnumFn = unsafe extern "C" fn(*mut *const spa_handle_factory, *mut u32) -> c_int;

/// Builds the cdylib once per test run. `cargo test` doesn't build cdylib targets for
/// integration tests, so do it here rather than relying on a prior `cargo build`.
fn plugin_path() -> &'static PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        // Match the profile of this test binary, since the .so is loaded from beside it.
        let mut build = Command::new(cargo);
        build.args(["build", "-p", env!("CARGO_PKG_NAME")]);
        if !cfg!(debug_assertions) {
            build.arg("--release");
        }
        let status = build
            .arg("--manifest-path")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .status()
            .unwrap();
        assert!(status.success(), "cargo build of the plugin failed");
        std::env::current_exe()
            .unwrap()
            .with_file_name("libspa_codec_aptx_adaptive.so")
    })
}

/// The two entries, in the plugin's order: the one that owns the endpoint first, then the
/// alias. Tests other than the env ones run with `APTX_ADAPTIVE_DEFAULT_MODE` unset, so `codec`
/// is the HQ owner and `ll` the LL alias.
struct Loaded {
    _lib: libloading::Library,
    _handle: Vec<u64>,
    /// Owns the endpoint (`fill_caps`, the slot's id): what a fresh connection binds.
    codec: &'static media_codec,
    /// The alias on the same endpoint.
    ll: &'static media_codec,
}

fn load() -> Loaded {
    // SAFETY: our own plugin, no global constructors with side effects.
    let lib = unsafe { libloading::Library::new(plugin_path()) }.unwrap();
    // SAFETY: symbol type matches the C declaration.
    let enum_fn = unsafe { lib.get::<EnumFn>(b"spa_handle_factory_enum\0") }.unwrap();

    let mut factory = ptr::null();
    let mut index = 0;
    // SAFETY: valid out-pointers.
    assert_eq!(unsafe { enum_fn(&raw mut factory, &raw mut index) }, 1);
    // SAFETY: plugin returned a static factory.
    let f = unsafe { &*factory };
    assert_eq!(f.version, SPA_VERSION_HANDLE_FACTORY);
    // SAFETY: NUL-terminated static string.
    assert_eq!(unsafe { CStr::from_ptr(f.name) }, ids::FACTORY_NAME);
    // SAFETY: valid out-pointers.
    assert_eq!(unsafe { enum_fn(&raw mut factory, &raw mut index) }, 0);

    // SAFETY: calling the factory as the loader would.
    let size = unsafe { f.get_size.unwrap()(f, ptr::null()) };
    let mut handle = vec![0u64; size.div_ceil(8)];
    let h = handle.as_mut_ptr().cast::<spa_handle>();
    // SAFETY: buffer is get_size bytes, 8-aligned.
    assert_eq!(
        unsafe { f.init.unwrap()(f, h, ptr::null(), ptr::null(), 0) },
        0
    );

    let mut iface: *mut c_void = ptr::null_mut();
    // SAFETY: handle initialised above.
    let res =
        unsafe { (*h).get_interface.unwrap()(h, ids::INTERFACE_TYPE.as_ptr(), &raw mut iface) };
    assert_eq!(res, 0);
    // SAFETY: plugin returned its interface struct.
    let iface = unsafe { &*iface.cast::<spa_bluez5_codec_a2dp>() };
    assert_eq!(iface.iface.version, SPA_VERSION_BLUEZ5_CODEC_MEDIA);
    // SAFETY: NULL-terminated array of static codecs.
    let (codec, ll) = unsafe {
        assert!((*iface.codecs.add(2)).is_null());
        (&**iface.codecs, &**iface.codecs.add(1))
    };
    Loaded {
        _lib: lib,
        _handle: handle,
        codec,
        ll,
    }
}

fn select(codec: &media_codec, caps: &[u8]) -> (c_int, Vec<u8>) {
    let info = media_codec_audio_info {
        rate: 48_000,
        channels: 2,
    };
    let mut config = vec![0u8; A2DP_MAX_CAPS_SIZE];
    // SAFETY: arguments match the C contract.
    let n = unsafe {
        codec.select_config.unwrap()(
            codec,
            0,
            caps.as_ptr().cast(),
            caps.len(),
            &raw const info,
            ptr::null(),
            config.as_mut_ptr(),
            ptr::null_mut(),
        )
    };
    config.truncate(n.max(0) as usize);
    (n, config)
}

fn validated_info(codec: &media_codec, config: &[u8]) -> spa_audio_info {
    // SAFETY: zeroed spa_audio_info is valid plain data.
    let mut info: spa_audio_info = unsafe { std::mem::zeroed() };
    // SAFETY: arguments match the C contract.
    let res = unsafe {
        codec.validate_config.unwrap()(
            codec,
            0,
            config.as_ptr().cast(),
            config.len(),
            &raw mut info,
        )
    };
    assert_eq!(res, 0);
    info
}

fn init(codec: &media_codec, config: &mut [u8], info: &spa_audio_info) -> *mut c_void {
    init_with_props(codec, config, info, ptr::null_mut())
}

fn init_with_props(
    codec: &media_codec,
    config: &mut [u8],
    info: &spa_audio_info,
    props: *mut c_void,
) -> *mut c_void {
    // SAFETY: arguments match the C contract.
    unsafe {
        codec.init.unwrap()(
            codec,
            0,
            config.as_mut_ptr().cast(),
            config.len(),
            info,
            props,
            MTU,
        )
    }
}

/// Checks both entries against `APTX_ADAPTIVE_SLOT` and `APTX_ADAPTIVE_DEFAULT_MODE`. The
/// default mode's entry owns the endpoint; the other is the alias.
#[test]
fn codec_metadata() {
    let l = load();
    let owner = l.codec;
    let want = match std::env::var("APTX_ADAPTIVE_SLOT").as_deref() {
        Ok("lc3plus_hr") => SPA_BLUETOOTH_AUDIO_CODEC_LC3PLUS_HR,
        // Unset, and anything unknown, falls back to the LDAC slot.
        _ => SPA_BLUETOOTH_AUDIO_CODEC_LDAC,
    };
    assert_eq!(owner.id, want);
    assert!(owner.fill_caps.is_some());
    let ll_default = std::env::var("APTX_ADAPTIVE_DEFAULT_MODE").as_deref() == Ok("ll");
    let (hq, ll) = if ll_default {
        (l.ll, l.codec)
    } else {
        (l.codec, l.ll)
    };
    // SAFETY: static strings.
    unsafe {
        assert_eq!(CStr::from_ptr(hq.name), ids::CODEC_NAME);
        assert_eq!(CStr::from_ptr(hq.name), c"aptx_adaptive");
        assert_eq!(CStr::from_ptr(hq.description), ids::CODEC_DESCRIPTION);
        assert_eq!(CStr::from_ptr(ll.name), ids::LL_NAME);
        assert_eq!(CStr::from_ptr(ll.name), c"aptx_adaptive_ll");
        assert_eq!(CStr::from_ptr(ll.description), ids::LL_DESCRIPTION);
        assert_eq!(
            CStr::from_ptr(ll.description),
            c"aptX Adaptive (Low Latency)"
        );
    }
    for c in [owner, l.ll] {
        assert_eq!(c.kind, MEDIA_CODEC_A2DP);
        assert_eq!(c.codec_id, A2DP_CODEC_VENDOR);
        let (vid, cid) = (c.vendor.vendor_id, c.vendor.codec_id);
        assert_eq!((vid, cid), (ids::VENDOR_ID, ids::CODEC_ID));
        // SAFETY: static string.
        assert_eq!(unsafe { CStr::from_ptr(c.endpoint_name) }, ids::CODEC_NAME);
        assert!(c.encode.is_some() && c.decode.is_none());
        // A bigger socket buffer than media-sink's 4 × MTU default, so a start-up stall doesn't
        // drop packets.
        assert_eq!(c.send_buf_size, ids::SEND_BUF_SIZE);
        assert!(c.abr_process.is_some());
    }
    check_alias(l.ll, owner);
    if std::env::var_os(SLOT_CHILD_VAR).is_some() {
        std::process::exit(SLOT_CHILD_OK);
    }
}

/// The second entry is a shared-endpoint alias of the owner, like PipeWire's `sbc_xq`:
/// - its own id (MPEG), so `bluez5-device.c` lists a separate profile for it;
/// - no `fill_caps`, so `codec-loader.c` keeps it next to the owner, `bluez5-dbus.c` registers
///   no second endpoint, and a connection with no preferred codec never binds it;
/// - the owner's endpoint name, so a `SetConfiguration` on that endpoint can bind to it.
///
/// Every callback is the owner's.
fn check_alias(alias: &media_codec, owner: &media_codec) {
    assert_eq!(alias.id, SPA_BLUETOOTH_AUDIO_CODEC_MPEG);
    assert_ne!(alias.id, owner.id);
    assert!(alias.fill_caps.is_none());
    // SAFETY: static strings.
    unsafe {
        assert_eq!(
            CStr::from_ptr(alias.endpoint_name),
            CStr::from_ptr(owner.endpoint_name)
        );
    }
    assert_eq!((alias.kind, alias.codec_id), (owner.kind, owner.codec_id));
    assert_eq!(alias.send_buf_size, owner.send_buf_size);
    assert!(alias.duplex_codec.is_null() && !alias.stream_pkt);
    let same = |a: Option<usize>, b: Option<usize>| a.is_some() && a == b;
    macro_rules! same_fn {
        ($($f:ident),*) => {$(
            assert!(
                same(alias.$f.map(|f| f as usize), owner.$f.map(|f| f as usize)),
                stringify!($f)
            );
        )*};
    }
    same_fn!(
        select_config,
        enum_config,
        validate_config,
        init_props,
        clear_props,
        enum_props,
        set_props,
        init,
        deinit,
        update_props,
        get_block_size,
        get_interval,
        abr_process,
        start_encode,
        encode,
        get_delay
    );
    assert!(alias.decode.is_none());
}

const SLOT_CHILD_VAR: &str = "APTX_ADAPTIVE_TEST_SLOT_CHILD";
/// Exit status of a child that ran `codec_metadata` and passed. A child that matched no test
/// exits 0, so it can't pass by accident.
const SLOT_CHILD_OK: i32 = 42;

/// Runs test `name` in a child with `env` and expects it to pass. The slot and default mode
/// are read from the environment at factory init, and tests can't safely set them in-process.
fn run_child(name: &str, env: &[(&str, &str)]) {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", name, "--test-threads=1"])
        .env_remove("APTX_ADAPTIVE_SLOT")
        .env_remove("APTX_ADAPTIVE_DEFAULT_MODE")
        .env(SLOT_CHILD_VAR, "1");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.code() == Some(SLOT_CHILD_OK),
        "{name} {env:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn slot_and_default_mode_follow_env() {
    for slot in ["ldac", "lc3plus_hr", "bogus"] {
        for mode in ["hq", "ll", "bogus"] {
            run_child(
                "codec_metadata",
                &[
                    ("APTX_ADAPTIVE_SLOT", slot),
                    ("APTX_ADAPTIVE_DEFAULT_MODE", mode),
                ],
            );
        }
    }
}

/// With `APTX_ADAPTIVE_DEFAULT_MODE=ll` the LL entry owns the endpoint, so a fresh connection
/// binds it: it starts in LL with no props and, unlike the HQ owner, ignores
/// `bluez5.a2dp.aptx-adaptive.mode = hq`. The HQ alias starts in HQ, even with `mode = ll`.
/// The `quality` prop still switches either.
#[test]
fn ll_default_streams() {
    if std::env::var_os(SLOT_CHILD_VAR).is_none() {
        run_child(
            "ll_default_streams",
            &[("APTX_ADAPTIVE_DEFAULT_MODE", "ll")],
        );
        return;
    }
    let l = load();
    let (ll, hq) = (l.codec, l.ll);
    // SAFETY: static strings.
    unsafe {
        assert_eq!(CStr::from_ptr(ll.name), c"aptx_adaptive_ll");
        assert_eq!(CStr::from_ptr(hq.name), c"aptx_adaptive");
    }
    assert!(ll.fill_caps.is_some() && hq.fill_caps.is_none());
    for (c, mode, quality) in [(ll, c"hq", 1), (hq, c"ll", 0)] {
        let props = init_props(c, Some(mode));
        assert_eq!(
            enum_props(c, props, SPA_PARAM_PROPS, 0).1,
            quality_props(quality).to_vec()
        );
        // SAFETY: props from init_props.
        unsafe { c.clear_props.unwrap()(props) };
    }
    for (c, interval) in [(ll, 10_000_000), (hq, 14_000_000)] {
        let mut config = PX7_S2E_CONFIG;
        let info = validated_info(c, &config);
        let data = init(c, &mut config, &info);
        assert!(!data.is_null());
        // SAFETY: data from init.
        unsafe {
            assert_eq!(c.get_interval.unwrap()(data), interval);
            c.deinit.unwrap()(data);
        }
    }
    // The LL owner switches live to HQ and resets back to LL.
    let props = init_props(ll, None);
    // SAFETY: props from init_props.
    unsafe {
        let hq_q = quality_props(0);
        assert_eq!(ll.set_props.unwrap()(props, hq_q.as_ptr().cast()), 1);
        assert_eq!(ll.set_props.unwrap()(props, ptr::null()), 1);
        assert_eq!(
            enum_props(ll, props, SPA_PARAM_PROPS, 0).1,
            quality_props(1).to_vec()
        );
        ll.clear_props.unwrap()(props);
    }
    std::process::exit(SLOT_CHILD_OK);
}

#[test]
fn caps_round_trip() {
    let l = load();
    let c = l.codec;
    let mut caps = [0u8; A2DP_MAX_CAPS_SIZE];
    // SAFETY: caps is A2DP_MAX_CAPS_SIZE.
    let n = unsafe { c.fill_caps.unwrap()(c, 0, ptr::null(), caps.as_mut_ptr()) };
    assert_eq!(n as usize, CAPS_LEN);
    assert_eq!(caps[..6], VENDOR_HEADER);
    // Source type 2 at 48 kHz, R1 layout (end marker at body 15/16).
    assert_eq!((caps[6], caps[15], caps[16]), (0x12, 0x00, 0xaa));

    let (n, config) = select(c, &PX7_S2E);
    assert_eq!(n as usize, PX7_S2E.len());
    assert_eq!(config, PX7_S2E_CONFIG);

    let (n, _) = select(c, &[&VENDOR_HEADER[..], &[0x55, 0xAA]].concat());
    assert_eq!(n, -EINVAL);

    let (n, _) = select(c, &[0x4F, 0, 0, 0, 0x01, 0, 0]);
    assert_eq!(n, -ENOTSUP);

    let info = validated_info(c, &config);
    assert_eq!(
        (info.media_type, info.media_subtype, info.raw.format),
        (
            SPA_MEDIA_TYPE_AUDIO,
            SPA_MEDIA_SUBTYPE_RAW,
            SPA_AUDIO_FORMAT_S24_32_LE
        )
    );
    assert_eq!((info.raw.rate, info.raw.channels), (48_000, 2));
    assert_eq!(
        info.raw.position[..2],
        [SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR]
    );
}

fn enum_config(
    c: &media_codec,
    b: &mut spa_pod_builder,
    param: &mut *mut spa_pod,
    idx: u32,
) -> c_int {
    // SAFETY: arguments match the C contract.
    unsafe {
        c.enum_config.unwrap()(
            c,
            0,
            PX7_S2E_CONFIG.as_ptr().cast(),
            PX7_S2E_CONFIG.len(),
            SPA_PARAM_ENUM_FORMAT,
            idx,
            b,
            param,
        )
    }
}

fn builder(buf: &mut [u64], size: u32) -> spa_pod_builder {
    // SAFETY: zeroed builder has null frame and callbacks.
    let mut b: spa_pod_builder = unsafe { std::mem::zeroed() };
    b.data = buf.as_mut_ptr().cast();
    b.size = size;
    b
}

#[test]
fn enum_config_builds_pod() {
    let l = load();
    let mut buf = [0u64; 64];
    let mut b = builder(&mut buf, 512);
    let mut param: *mut spa_pod = ptr::null_mut();
    assert_eq!(enum_config(l.codec, &mut b, &mut param, 0), 1);
    assert_eq!(param.cast::<u8>(), buf.as_mut_ptr().cast::<u8>());
    // SAFETY: param points into buf.
    let pod = unsafe { *param };
    assert_eq!(pod.type_, SPA_TYPE_OBJECT);
    assert_eq!(b.state.offset, 8 + pod.size);
    assert_eq!(enum_config(l.codec, &mut b, &mut param, 1), 0);
}

#[test]
fn enum_config_reports_overflow() {
    let l = load();
    let mut buf = [0u64; 4];
    let mut b = builder(&mut buf, 32);
    let mut param: *mut spa_pod = ptr::null_mut();
    assert_eq!(enum_config(l.codec, &mut b, &mut param, 0), -ffi::ENOSPC);
    assert!(
        b.state.offset > 32,
        "offset advances past size like spa_pod_builder_raw"
    );
    assert_eq!(buf, [0; 4]);
}

#[test]
fn init_and_encode_a_real_packet() {
    let l = load();
    let c = l.codec;
    let mut config = PX7_S2E_CONFIG;
    let info = validated_info(c, &config);
    let data = init(c, &mut config, &info);
    assert!(!data.is_null());

    // SAFETY: data from init, valid until deinit.
    unsafe {
        let block = c.get_block_size.unwrap()(data);
        assert_eq!(block, BLOCK_SIZE);
        let mut out = [0u8; 1024];
        let hdr = c.start_encode.unwrap()(data, out.as_mut_ptr().cast(), out.len(), 1, 0);
        assert_eq!(hdr, 12);
        // Spec §7F: 80 60, sequence, timestamp 672 samples (adjust 0), SSRC a3 (HQ), no
        // payload prefix.
        assert_eq!(
            out[..12],
            [0x80, 0x60, 0, 1, 0, 0, 0x02, 0xa0, 0, 0, 0, 0xa3]
        );
        // 14 ms of 2 ms blocks.
        assert_eq!(encode_until_flush(c, data, &mut out[12..], 7), (656, 7));
        // Swapped config 0 packet: 00 01 a1 .. becomes 01 00 .. a1 (0x80 on byte 1 if padded).
        assert_eq!((out[12] & 0x7f, out[13], out[15]), (0x01, 0x00, 0xa1));
        assert_eq!(c.abr_process.unwrap()(data, 0), 0);
        let mut delay = 99;
        c.get_delay.unwrap()(data, &raw mut delay, ptr::null_mut());
        assert_eq!(delay, 90);
        c.deinit.unwrap()(data);
    }
}

/// An R2.1 sink gets a config the plugin can stream: the R2 HQ level-3 row (19 ms, 912 samples
/// per channel, spec §4.1 idx 1) and the config 3 bitstream.
#[test]
fn r2_config_streams() {
    let l = load();
    let c = l.codec;
    let mut caps = [0u8; CAPS_LEN];
    caps[..6].copy_from_slice(&VENDOR_HEADER);
    caps[6..26].copy_from_slice(&[
        0x19, 0x0a, 0x50, 0x64, 0x64, 0x64, 0xff, 0xff, 0x00, 0x01, 0x03, 0x00, 0x00, 0x0f, 0x02,
        0x03, 0x03, 0x03, 0x00, 0xaa,
    ]);
    let (n, mut config) = select(c, &caps);
    assert_eq!(n as usize, CAPS_LEN);
    assert_eq!(config[15], 1, "R2 extension");
    let info = validated_info(c, &config);
    let data = init(c, &mut config, &info);
    assert!(!data.is_null());

    // SAFETY: data from init, valid until deinit.
    unsafe {
        // 1 ms: divides both the 19 ms HQ and the 10 ms LL (config 4) packet.
        let block = c.get_block_size.unwrap()(data);
        assert_eq!(block, 48 * 2 * 4);
        let mut out = [0u8; 1024];
        assert_eq!(
            c.start_encode.unwrap()(data, out.as_mut_ptr().cast(), out.len(), 1, 0),
            12
        );
        assert_eq!(out[4..12], [0, 0, 0x03, 0x90, 0, 0, 0, 0xa3]);
        // 19 ms of 1 ms blocks.
        assert_eq!(encode_until_flush(c, data, &mut out[12..], 19), (656, 19));
        // Swapped config 3 packet: 00 83 a1 .. (spec §3.2).
        assert_eq!((out[12] & 0x7f, out[13], out[15]), (0x03, 0x00, 0xa1));
        c.deinit.unwrap()(data);
    }
}

/// Feeds silence a block at a time (the codec's own block size) until a packet comes out,
/// like `media-sink.c`, writing the packet to `out`. Returns its length and the number of
/// blocks it took. Panics if no packet comes out within `max_blocks`.
///
/// # Safety
/// `data` must come from `init`.
unsafe fn encode_until_flush(
    c: &media_codec,
    data: *mut c_void,
    out: &mut [u8],
    max_blocks: usize,
) -> (usize, usize) {
    // SAFETY: caller contract.
    let block = unsafe { c.get_block_size.unwrap()(data) };
    let pcm = vec![0u8; block as usize];
    for n in 1..=max_blocks {
        let (mut written, mut flush) = (0usize, 0);
        // SAFETY: caller contract; buffers are valid for their lengths.
        let res = unsafe {
            c.encode.unwrap()(
                data,
                pcm.as_ptr().cast(),
                pcm.len(),
                out.as_mut_ptr().cast(),
                out.len(),
                &raw mut written,
                &raw mut flush,
            )
        };
        assert_eq!(res, block);
        if flush != 0 {
            return (written, n);
        }
        assert_eq!(written, 0);
    }
    panic!("no packet in {max_blocks} blocks");
}

fn init_props(c: &media_codec, mode: Option<&CStr>) -> *mut c_void {
    let items = mode.map(|m| spa_dict_item {
        key: c"bluez5.a2dp.aptx-adaptive.mode".as_ptr(),
        value: m.as_ptr(),
    });
    let other = spa_dict_item {
        key: c"bluez5.a2dp.ldac.quality".as_ptr(),
        value: c"hq".as_ptr(),
    };
    let items: Vec<spa_dict_item> = std::iter::once(other).chain(items).collect();
    let dict = spa_dict {
        flags: 0,
        n_items: items.len() as u32,
        items: items.as_ptr(),
    };
    // SAFETY: a valid dict that outlives the call.
    let p = unsafe { c.init_props.unwrap()(c, 0, &raw const dict) };
    assert!(!p.is_null());
    p
}

/// `spa_pod_builder_add_object(b, SPA_TYPE_OBJECT_Props, SPA_PARAM_Props, SPA_PROP_quality,
/// SPA_POD_Int(v))`.
fn quality_props(v: i32) -> [u32; 10] {
    [
        32,
        SPA_TYPE_OBJECT,
        SPA_TYPE_OBJECT_PROPS,
        SPA_PARAM_PROPS,
        SPA_PROP_QUALITY,
        0,
        4,
        SPA_TYPE_INT,
        v as u32,
        0,
    ]
}

/// Enumerates `id` at `idx` and returns the result code and the pod's words.
fn enum_props(c: &media_codec, props: *mut c_void, id: u32, idx: u32) -> (c_int, Vec<u32>) {
    let mut buf = [0u64; 64];
    let mut b = builder(&mut buf, 512);
    let mut param: *mut spa_pod = ptr::null_mut();
    // SAFETY: arguments match the C contract.
    let res =
        unsafe { c.enum_props.unwrap()(props, ptr::null(), id, idx, &raw mut b, &raw mut param) };
    if res != 1 {
        return (res, Vec::new());
    }
    // SAFETY: param points at a pod in buf.
    let words = unsafe {
        let n = ((*param).size as usize + 8) / 4;
        std::slice::from_raw_parts(param.cast::<u32>(), n).to_vec()
    };
    (res, words)
}

#[test]
fn props_are_ldac_style() {
    let l = load();
    let c = l.codec;
    assert!(c.init_props.is_some() && c.update_props.is_some());
    let props = init_props(c, None);
    let (res, info) = enum_props(c, props, SPA_PARAM_PROP_INFO, 0);
    assert_eq!(res, 1);
    assert_eq!(
        info[1..4],
        [
            SPA_TYPE_OBJECT,
            SPA_TYPE_OBJECT_PROP_INFO,
            SPA_PARAM_PROP_INFO
        ]
    );
    assert_eq!(enum_props(c, props, SPA_PARAM_PROP_INFO, 1).0, 0);
    let (res, current) = enum_props(c, props, SPA_PARAM_PROPS, 0);
    assert_eq!((res, current), (1, quality_props(0).to_vec()));
    assert_eq!(enum_props(c, props, SPA_PARAM_ENUM_FORMAT, 0).0, -ENOENT);
    // SAFETY: props from init_props.
    unsafe {
        let ll = quality_props(1);
        assert_eq!(c.set_props.unwrap()(props, ll.as_ptr().cast()), 1);
        assert_eq!(c.set_props.unwrap()(props, ll.as_ptr().cast()), 0);
        assert_eq!(enum_props(c, props, SPA_PARAM_PROPS, 0).1, ll.to_vec());
        // NULL resets to the device setting.
        assert_eq!(c.set_props.unwrap()(props, ptr::null()), 1);
        assert_eq!(
            enum_props(c, props, SPA_PARAM_PROPS, 0).1,
            quality_props(0).to_vec()
        );
        c.clear_props.unwrap()(props);
    }
    let props = init_props(c, Some(c"ll"));
    assert_eq!(
        enum_props(c, props, SPA_PARAM_PROPS, 0).1,
        quality_props(1).to_vec()
    );
    // SAFETY: props from init_props.
    unsafe { c.clear_props.unwrap()(props) };
}

/// One packet's RTP fields and size, driving the codec like `media-sink.c`: `update_props`
/// (if the props changed) then `start_encode`, then blocks until a flush.
///
/// # Safety
/// `data` and `props` must come from `init` and `init_props`.
unsafe fn next_packet(
    c: &media_codec,
    data: *mut c_void,
    props: *mut c_void,
    changed: bool,
    seq: u16,
) -> (usize, [u8; 8]) {
    let mut out = [0u8; 1024];
    // SAFETY: caller contract.
    unsafe {
        if changed {
            assert_eq!(c.update_props.unwrap()(data, props), 0);
        }
        let hdr = c.start_encode.unwrap()(data, out.as_mut_ptr().cast(), out.len(), seq, 0);
        assert_eq!(hdr, 12);
        let fields: [u8; 8] = out[4..12].try_into().unwrap();
        // At most one 14 ms HQ packet's worth of blocks.
        let (written, _) = encode_until_flush(c, data, &mut out[12..], 7);
        (written, fields)
    }
}

const HQ_FIELDS: [u8; 8] = [0, 0, 0x02, 0xa0, 0, 0, 0, 0xa3];
const LL_FIELDS: [u8; 8] = [0, 0, 0x01, 0xe0, 0, 0, 0, 0xa1];

/// Starting with `bluez5.a2dp.aptx-adaptive.mode = ll`, then switching LL → HQ through
/// `set_props` + `update_props` the way `media-sink.c` does: Props reports HQ at once, the
/// packets change (size, timestamp, SSRC, interval) together at the in-place reset about
/// 300 ms later (§7K: after a 100 ms fade-out and 200 ms of silence), and never mix.
#[test]
fn mode_setting_and_runtime_switch() {
    let l = load();
    let c = l.codec;
    let props = init_props(c, Some(c"ll"));
    let mut config = PX7_S2E_CONFIG;
    let info = validated_info(c, &config);
    let data = init_with_props(c, &mut config, &info, props);
    assert!(!data.is_null());
    // SAFETY: data and props from init and init_props, valid until deinit / clear_props.
    unsafe {
        assert_eq!(c.get_block_size.unwrap()(data), BLOCK_SIZE);
        assert_eq!(c.get_interval.unwrap()(data), 10_000_000);
        // Past the start-of-stream mute and fade-in (20 + 100 ms, §7I).
        for seq in 0..15 {
            assert_eq!(next_packet(c, data, props, false, seq), (348, LL_FIELDS));
        }
        let hq = quality_props(0);
        assert_eq!(c.set_props.unwrap()(props, hq.as_ptr().cast()), 1);
        assert_eq!(enum_props(c, props, SPA_PARAM_PROPS, 0).1, hq.to_vec());
        let mut kinds = String::new();
        let mut changed = true;
        for seq in 15..130 {
            let (size, fields) = next_packet(c, data, props, changed, seq);
            changed = false;
            kinds.push(match (size, fields) {
                (348, LL_FIELDS) => 'L',
                (656, HQ_FIELDS) => 'H',
                other => panic!("packet {seq}: {other:?}"),
            });
            assert_eq!(c.get_block_size.unwrap()(data), BLOCK_SIZE);
        }
        let switch = kinds.find('H').unwrap();
        // About 300 ms of 10 ms LL packets first, then HQ only.
        assert!((29..=32).contains(&switch), "{kinds}");
        assert!(!kinds[switch..].contains('L'), "{kinds}");
        assert_eq!(c.get_interval.unwrap()(data), 14_000_000);
        assert_eq!(enum_props(c, props, SPA_PARAM_PROPS, 0).1, hq.to_vec());
        c.deinit.unwrap()(data);
        c.clear_props.unwrap()(props);
    }
}

/// An R2 sink runs LL as config 4 (codec-core §10.F): a `ll` setting starts in LL (10 ms,
/// 348-byte config 4 packets, SSRC A1) with 1 ms blocks, and LL is offered.
#[test]
fn r2_sink_runs_ll() {
    let l = load();
    let c = l.codec;
    let props = init_props(c, Some(c"ll"));
    let mut config = PX7_S2E_CONFIG;
    // The R2 layout: extension version 1, end marker at 24..25 (§1.1).
    config[15] = 1;
    config[16] = 0;
    config[24..26].copy_from_slice(&[0, 0xaa]);
    let info = validated_info(c, &config);
    let data = init_with_props(c, &mut config, &info, props);
    assert!(!data.is_null());
    // SAFETY: data and props from init and init_props.
    unsafe {
        assert_eq!(
            enum_props(c, props, SPA_PARAM_PROPS, 0).1,
            quality_props(1).to_vec()
        );
        let (_, info) = enum_props(c, props, SPA_PARAM_PROP_INFO, 0);
        let as_bytes: Vec<u8> = info.iter().flat_map(|w| w.to_ne_bytes()).collect();
        assert!(as_bytes.windows(11).any(|w| w == b"Low latency"));
        assert_eq!(c.get_block_size.unwrap()(data), 48 * 2 * 4);
        assert_eq!(c.get_interval.unwrap()(data), 10_000_000);
        let mut out = [0u8; 1024];
        assert_eq!(
            c.start_encode.unwrap()(data, out.as_mut_ptr().cast(), out.len(), 0, 0),
            12
        );
        assert_eq!(out[4..12], LL_FIELDS);
        // 10 ms of 1 ms blocks.
        assert_eq!(encode_until_flush(c, data, &mut out[12..], 10), (348, 10));
        // Swapped config 4 packet: frame type 4.
        assert_eq!(out[12] & 0x7f, 4);
        c.deinit.unwrap()(data);
        c.clear_props.unwrap()(props);
    }
}

/// The LL profile picks the same configuration as the main entry: PipeWire's support check
/// (`media_codec_check_caps`) and the switch both call the alias's `select_config`, which must
/// return the full caps size.
#[test]
fn ll_entry_selects_same_config() {
    let l = load();
    assert_eq!(select(l.ll, &PX7_S2E), select(l.codec, &PX7_S2E));
    assert_eq!(select(l.ll, &PX7_S2E).0 as usize, PX7_S2E.len());
}

/// Created through the LL alias, the stream starts in LL even with `mode = hq` (or nothing)
/// in the device settings, and the `quality` prop still switches it live. A reset goes back to
/// LL, the profile's own mode.
#[test]
fn ll_entry_starts_in_ll_and_still_switches() {
    let l = load();
    let c = l.ll;
    for mode in [None, Some(c"hq")] {
        let props = init_props(c, mode);
        assert_eq!(
            enum_props(c, props, SPA_PARAM_PROPS, 0).1,
            quality_props(1).to_vec()
        );
        // SAFETY: props from init_props.
        unsafe { c.clear_props.unwrap()(props) };
    }
    let props = init_props(c, Some(c"hq"));
    let mut config = PX7_S2E_CONFIG;
    let info = validated_info(c, &config);
    let data = init_with_props(c, &mut config, &info, props);
    assert!(!data.is_null());
    // SAFETY: data and props from init and init_props, valid until deinit / clear_props.
    unsafe {
        assert_eq!(c.get_interval.unwrap()(data), 10_000_000);
        assert_eq!(next_packet(c, data, props, false, 0).1, LL_FIELDS);
        let hq = quality_props(0);
        assert_eq!(c.set_props.unwrap()(props, hq.as_ptr().cast()), 1);
        assert_eq!(enum_props(c, props, SPA_PARAM_PROPS, 0).1, hq.to_vec());
        let mut changed = true;
        let mut fields = LL_FIELDS;
        for seq in 1..60 {
            fields = next_packet(c, data, props, changed, seq).1;
            changed = false;
        }
        assert_eq!(fields, HQ_FIELDS);
        assert_eq!(c.get_interval.unwrap()(data), 14_000_000);
        assert_eq!(c.set_props.unwrap()(props, ptr::null()), 1);
        assert_eq!(
            enum_props(c, props, SPA_PARAM_PROPS, 0).1,
            quality_props(1).to_vec()
        );
        c.deinit.unwrap()(data);
        c.clear_props.unwrap()(props);
    }
    // No props at all: the alias still starts in LL, the main entry in HQ.
    for (codec, interval) in [(l.ll, 10_000_000), (l.codec, 14_000_000)] {
        let mut config = PX7_S2E_CONFIG;
        let info = validated_info(codec, &config);
        let data = init(codec, &mut config, &info);
        assert!(!data.is_null());
        // SAFETY: data from init.
        unsafe {
            assert_eq!(codec.get_interval.unwrap()(data), interval);
            codec.deinit.unwrap()(data);
        }
    }
}

#[test]
fn init_returns_null_and_sets_errno_on_rejected_stream() {
    let l = load();
    let c = l.codec;
    let mut config = PX7_S2E_CONFIG;
    let mut info = validated_info(c, &config);
    info.raw.rate = 44_100;
    let data = init(c, &mut config, &info);
    assert!(data.is_null());
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(EINVAL));
}

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
    A2DP_CODEC_VENDOR, A2DP_MAX_CAPS_SIZE, EINVAL, ENOTSUP, MEDIA_CODEC_A2DP, SPA_AUDIO_CHANNEL_FL,
    SPA_AUDIO_CHANNEL_FR, SPA_AUDIO_FORMAT_S24_32_LE, SPA_BLUETOOTH_AUDIO_CODEC_LC3PLUS_HR,
    SPA_BLUETOOTH_AUDIO_CODEC_LDAC, SPA_MEDIA_SUBTYPE_RAW, SPA_MEDIA_TYPE_AUDIO,
    SPA_PARAM_ENUM_FORMAT, SPA_TYPE_OBJECT, SPA_VERSION_BLUEZ5_CODEC_MEDIA,
    SPA_VERSION_HANDLE_FACTORY, media_codec, media_codec_audio_info, spa_audio_info,
    spa_bluez5_codec_a2dp, spa_handle, spa_handle_factory, spa_pod, spa_pod_builder,
};
use fixtures::{PX7_S2E, PX7_S2E_CONFIG};
use ids::{CAPS_LEN, VENDOR_HEADER};

const MTU: usize = 895;
/// One 14 ms HQ packet of stereo `S24_32` (spec `a2dp-integration.md` §4.1).
const BLOCK_SIZE: c_int = 672 * 2 * 4;

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

struct Loaded {
    _lib: libloading::Library,
    _handle: Vec<u64>,
    codec: &'static media_codec,
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
    let codec = unsafe {
        assert!((*iface.codecs.add(1)).is_null());
        &**iface.codecs
    };
    Loaded {
        _lib: lib,
        _handle: handle,
        codec,
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
    // SAFETY: arguments match the C contract.
    unsafe {
        codec.init.unwrap()(
            codec,
            0,
            config.as_mut_ptr().cast(),
            config.len(),
            info,
            ptr::null_mut(),
            MTU,
        )
    }
}

#[test]
fn codec_metadata() {
    let l = load();
    let c = l.codec;
    let want = match std::env::var("APTX_ADAPTIVE_SLOT").as_deref() {
        Ok("lc3plus_hr") => SPA_BLUETOOTH_AUDIO_CODEC_LC3PLUS_HR,
        // Unset, and anything unknown, falls back to the LDAC slot.
        _ => SPA_BLUETOOTH_AUDIO_CODEC_LDAC,
    };
    assert_eq!(c.id, want);
    assert_eq!(c.kind, MEDIA_CODEC_A2DP);
    assert_eq!(c.codec_id, A2DP_CODEC_VENDOR);
    let (vid, cid) = (c.vendor.vendor_id, c.vendor.codec_id);
    assert_eq!((vid, cid), (ids::VENDOR_ID, ids::CODEC_ID));
    // SAFETY: static strings.
    unsafe {
        assert_eq!(CStr::from_ptr(c.name), ids::CODEC_NAME);
        assert_eq!(CStr::from_ptr(c.description), ids::CODEC_DESCRIPTION);
    }
    assert!(c.encode.is_some() && c.decode.is_none());
    // A bigger socket buffer than media-sink's 4 × MTU default, so a start-up stall doesn't drop packets.
    assert_eq!(c.send_buf_size, ids::SEND_BUF_SIZE);
    assert!(c.abr_process.is_some());
}

/// Re-runs `codec_metadata` in a child with each `APTX_ADAPTIVE_SLOT`, since the slot is read
/// from the environment at factory init and tests can't safely set it in-process.
#[test]
fn slot_follows_env() {
    for slot in ["ldac", "lc3plus_hr", "bogus"] {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "codec_metadata", "--test-threads=1"])
            .env("APTX_ADAPTIVE_SLOT", slot)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "slot {slot}: {stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
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
        let pcm = vec![0u8; block as usize];
        let (mut written, mut flush) = (0usize, 0);
        let res = c.encode.unwrap()(
            data,
            pcm.as_ptr().cast(),
            pcm.len(),
            out.as_mut_ptr().add(12).cast(),
            out.len() - 12,
            &raw mut written,
            &raw mut flush,
        );
        assert_eq!(res, block);
        assert_eq!((written, flush), (656, 1));
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
        let block = c.get_block_size.unwrap()(data);
        assert_eq!(block, 912 * 2 * 4);
        let mut out = [0u8; 1024];
        assert_eq!(
            c.start_encode.unwrap()(data, out.as_mut_ptr().cast(), out.len(), 1, 0),
            12
        );
        assert_eq!(out[4..12], [0, 0, 0x03, 0x90, 0, 0, 0, 0xa3]);
        let pcm = vec![0u8; block as usize];
        let (mut written, mut flush) = (0usize, 0);
        let res = c.encode.unwrap()(
            data,
            pcm.as_ptr().cast(),
            pcm.len(),
            out.as_mut_ptr().add(12).cast(),
            out.len() - 12,
            &raw mut written,
            &raw mut flush,
        );
        assert_eq!(res, block);
        assert_eq!((written, flush), (656, 1));
        // Swapped config 3 packet: 00 83 a1 .. (spec §3.2).
        assert_eq!((out[12] & 0x7f, out[13], out[15]), (0x03, 0x00, 0xa1));
        c.deinit.unwrap()(data);
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

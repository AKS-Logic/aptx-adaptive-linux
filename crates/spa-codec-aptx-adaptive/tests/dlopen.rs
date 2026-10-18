#[expect(
    dead_code,
    reason = "shared with the cdylib; the test only touches part of the FFI surface"
)]
#[path = "../src/ffi.rs"]
mod ffi;
#[path = "../src/fixtures.rs"]
mod fixtures;
#[expect(dead_code, reason = "the test uses a subset of the shared ids")]
#[path = "../src/ids.rs"]
mod ids;

use std::ffi::{CStr, c_int, c_void};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;
use std::sync::OnceLock;

use ffi::{
    A2DP_CODEC_VENDOR, A2DP_MAX_CAPS_SIZE, EINVAL, ENOENT, ENOTSUP, MEDIA_CODEC_A2DP,
    SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR, SPA_AUDIO_FORMAT_S24_32_LE,
    SPA_BLUETOOTH_AUDIO_CODEC_APTX, SPA_BLUETOOTH_AUDIO_CODEC_APTX_HD,
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
const BLOCK_SIZE: c_int = 96 * 2 * 4;

type EnumFn = unsafe extern "C" fn(*mut *const spa_handle_factory, *mut u32) -> c_int;

/// Builds and locates the cdylib once per test run.
fn plugin_path() -> &'static PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
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

/// Our two entries in plugin order (endpoint owner, then alias), and the whole export list,
/// which in the `aptx` slot goes on with the real aptX plugin's codecs.
struct Loaded {
    _lib: libloading::Library,
    _handle: Vec<u64>,
    codec: &'static media_codec,
    ll: &'static media_codec,
    all: Vec<&'static media_codec>,
}

fn load() -> Loaded {
    load_from(plugin_path(), slot_factory_name())
}

fn slot_factory_name() -> &'static CStr {
    match std::env::var("APTX_ADAPTIVE_SLOT").as_deref() {
        Ok("ldac" | "lc3plus_hr") => ids::FACTORY_NAME_LC3PLUS,
        _ => ids::FACTORY_NAME_APTX,
    }
}

/// Loads a codec plugin the way `codec-loader.c` does and returns its codec list.
fn load_from(path: &Path, factory_name: &CStr) -> Loaded {
    // SAFETY: a PipeWire codec plugin, no global constructors with side effects.
    let lib = unsafe { libloading::Library::new(path) }.unwrap();
    // SAFETY: symbol type matches the C declaration.
    let enum_fn = unsafe { lib.get::<EnumFn>(b"spa_handle_factory_enum\0") }.unwrap();

    let mut names = Vec::new();
    let mut found = None;
    let mut index = 0;
    loop {
        let mut factory = ptr::null();
        // SAFETY: valid out-pointers.
        match unsafe { enum_fn(&raw mut factory, &raw mut index) } {
            0 => break,
            1 => {}
            r => panic!("spa_handle_factory_enum: {r}"),
        }
        // SAFETY: plugin returned a static factory.
        let f = unsafe { &*factory };
        assert_eq!(f.version, SPA_VERSION_HANDLE_FACTORY);
        // SAFETY: NUL-terminated static string.
        let name = unsafe { CStr::from_ptr(f.name) };
        names.push(name);
        if name == factory_name {
            found = Some(f);
        }
    }
    let f = found.unwrap_or_else(|| panic!("no {factory_name:?} in {names:?}"));
    // SAFETY: plugins point info at a static dict.
    let desc = unsafe { (*(*f.info).items).value };
    // SAFETY: static string.
    let ours = unsafe { CStr::from_ptr(desc) } == ids::FACTORY_DESCRIPTION;
    assert_eq!(ours, !path.starts_with(SYSTEM_PLUGIN_DIR));
    if ours {
        assert_eq!(
            names,
            [
                ids::FACTORY_NAME_APTX,
                ids::FACTORY_NAME_LC3PLUS,
                ids::FACTORY_NAME_NATIVE,
            ],
            "both file names' factories plus the native-encoder port"
        );
    }

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
    let mut all = Vec::new();
    // SAFETY: NULL-terminated array of codecs that live as long as the library.
    unsafe {
        while let Some(c) = (*iface.codecs.add(all.len())).as_ref() {
            all.push(c);
        }
    }
    Loaded {
        _lib: lib,
        _handle: handle,
        codec: all[0],
        ll: all.get(1).copied().unwrap_or(all[0]),
        all,
    }
}

fn select(codec: &media_codec, caps: &[u8]) -> (c_int, Vec<u8>) {
    select_preferred(codec, caps, Some(48_000))
}

fn select_preferred(codec: &media_codec, caps: &[u8], rate: Option<u32>) -> (c_int, Vec<u8>) {
    let info = rate.map(|rate| media_codec_audio_info { rate, channels: 2 });
    let mut config = vec![0u8; A2DP_MAX_CAPS_SIZE];
    // SAFETY: arguments match the C contract.
    let n = unsafe {
        codec.select_config.unwrap()(
            codec,
            0,
            caps.as_ptr().cast(),
            caps.len(),
            info.as_ref().map_or(ptr::null(), ptr::from_ref),
            ptr::null(),
            config.as_mut_ptr(),
            ptr::null_mut(),
        )
    };
    config.truncate(n.max(0) as usize);
    (n, config)
}

fn r2_caps(rates: u8, features: u8) -> [u8; CAPS_LEN] {
    #[rustfmt::skip]
    let mut caps = [
        0xd7, 0, 0, 0, 0xad, 0, 0, 0x0a,
        0x50, 0x64, 0x64, 0x64, 0xff, 0xff, 0, 1,
        0, 0, 0, 0x0f, 2, 3, 3, 3, 0, 0xaa,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    caps[6] = rates | 1;
    caps[16] = features;
    caps
}

#[test]
fn rate_preference_and_safe_fallbacks() {
    let l = load();
    for (rates, features, preferred, expected) in [
        (0x38, 0x03, None, 0x12),
        (0x38, 0x03, Some(0), 0x12),
        (0x38, 0x03, Some(48_000), 0x12),
        (0x38, 0x03, Some(44_100), 0x0a),
        (0x38, 0x03, Some(96_000), 0x22),
        (0x38, 0x03, Some(192_000), 0x12),
        (0x28, 0x03, Some(48_000), 0x0a),
        (0x20, 0x03, None, 0x22),
        (0x38, 0x83, Some(44_100), 0x0a),
        (0x28, 0x80, Some(44_100), 0x0a),
        (0x38, 0xff, Some(96_000), 0x22),
    ] {
        for c in [l.codec, l.ll] {
            let (n, config) = select_preferred(c, &r2_caps(rates, features), preferred);
            assert_eq!(
                n, 40,
                "rates={rates:#x} features={features:#x} preferred={preferred:?}"
            );
            assert_eq!(
                config[6], expected,
                "{rates:#x} {features:#x} {preferred:?}"
            );
            assert_eq!(
                config[7..26],
                [
                    2, 0x50, 0x64, 0x64, 0x64, 0xff, 0xff, 0, 1, 0x92, 0, 0, 0x0f, 2, 3, 3,
                    3, 0, 0xaa,
                ]
            );
        }
    }
    for preferred in [None, Some(44_100), Some(96_000)] {
        assert_eq!(
            select_preferred(l.codec, &PX7_S2E, preferred).1,
            PX7_S2E_CONFIG
        );
    }
    // baizhu945's fork: 44.1 kHz is offered to 2.2 sinks and the alternate
    // 0x40 rate bit is accepted, so these are now valid configurations.
    for (caps, want) in [
        (r2_caps(0x08, 0x80), 0x0a),
        (r2_caps(0x08, 0x83), 0x0a),
        (r2_caps(0x40, 0), 0x42),
    ] {
        let (n, config) = select_preferred(l.codec, &caps, Some(44_100));
        assert_eq!(n, 40);
        assert_eq!(config[6], want);
    }
}

#[test]
fn negotiated_rates_drive_format_and_init() {
    let l = load();
    for (rate, byte6, block, interval, timestamp, calls) in [
        (44_100, 0x0a, 352, 19_000_000, 837u16, 20),
        (96_000, 0x22, 768, 15_000_000, 1440u16, 15),
    ] {
        let (n, mut config) = select_preferred(l.codec, &r2_caps(0x38, 3), Some(rate));
        assert_eq!(n, 40);
        assert_eq!(config[6], byte6);
        let info = validated_info(l.codec, &config);
        assert_eq!((info.raw.rate, info.raw.channels), (rate, 2));
        assert_eq!(info.raw.format, SPA_AUDIO_FORMAT_S24_32_LE);
        let mut buf = [0u64; 64];
        let mut b = builder(&mut buf, 512);
        let mut param = ptr::null_mut();
        // SAFETY: buffers and configuration satisfy the callback contract.
        unsafe {
            assert_eq!(
                l.codec.enum_config.unwrap()(
                    l.codec,
                    0,
                    config.as_ptr().cast(),
                    config.len(),
                    SPA_PARAM_ENUM_FORMAT,
                    0,
                    &raw mut b,
                    &raw mut param,
                ),
                1
            );
            let words =
                std::slice::from_raw_parts(param.cast::<u32>(), ((*param).size as usize + 8) / 4);
            assert!(
                words
                    .windows(5)
                    .any(|w| w == [ffi::SPA_FORMAT_AUDIO_RATE, 0, 4, SPA_TYPE_INT, rate])
            );
        }
        let data = init(l.codec, &mut config, &info);
        assert!(!data.is_null());
        // SAFETY: data from init and valid buffers.
        unsafe {
            assert_eq!(l.codec.get_block_size.unwrap()(data), block);
            assert_eq!(l.codec.get_interval.unwrap()(data), interval);
            let mut out = [0; 1024];
            assert_eq!(
                l.codec.start_encode.unwrap()(data, out.as_mut_ptr().cast(), out.len(), 7, 99),
                12
            );
            let [hi, lo] = timestamp.to_be_bytes();
            assert_eq!(out[..12], [0x80, 0x60, 0, 7, 0, 0, hi, lo, 0, 0, 0, 0xa3]);
            assert_eq!(
                encode_until_flush(l.codec, data, &mut out[12..], calls),
                (656, calls)
            );
            assert_eq!(out[12] & 0x7f, 3);
            assert_eq!(
                l.codec.start_encode.unwrap()(data, out.as_mut_ptr().cast(), out.len(), 8, 0),
                12
            );
            assert_eq!(
                encode_until_flush(l.codec, data, &mut out[12..], calls).0,
                656
            );
            let (name, index) = if rate == 44_100 {
                ("c3_44k_i1_L3", 38)
            } else {
                ("c3_96k_i5_L3", 47)
            };
            let reference = std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/vectors/parity")
                    .join(format!("{name}.bin")),
            )
            .unwrap();
            let swapped: Vec<u8> = reference[index * 656..][..656]
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|p| [p[1], p[0]])
                .collect();
            assert_eq!(out[12..668], swapped, "{rate} digital-black payload");
            let (mut encoder, mut decoder) = (99, 99);
            l.codec.get_delay.unwrap()(data, &raw mut encoder, &raw mut decoder);
            assert_eq!((encoder, decoder), (255, 0));
            l.codec.deinit.unwrap()(data);
        }
    }
}

#[test]
fn new_rate_init_rejects_pcm_mismatches() {
    let l = load();
    for rate in [44_100, 96_000] {
        let (_, mut config) = select_preferred(l.codec, &r2_caps(0x38, 3), Some(rate));
        let info = validated_info(l.codec, &config);
        let data = init(l.codec, &mut config, &info);
        assert!(!data.is_null());
        // SAFETY: initialized session returned by init.
        unsafe { l.codec.deinit.unwrap()(data) };
        for mismatch in [48_000, if rate == 44_100 { 96_000 } else { 44_100 }] {
            let mut bad = info;
            bad.raw.rate = mismatch;
            assert!(init(l.codec, &mut config, &bad).is_null());
            assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(EINVAL));
        }
        for bad in [
            spa_audio_info {
                media_type: 0,
                ..info
            },
            spa_audio_info {
                media_subtype: 0,
                ..info
            },
            spa_audio_info {
                raw: ffi::spa_audio_info_raw {
                    format: 0,
                    ..info.raw
                },
                ..info
            },
            spa_audio_info {
                raw: ffi::spa_audio_info_raw {
                    channels: 1,
                    ..info.raw
                },
                ..info
            },
        ] {
            assert!(init(l.codec, &mut config, &bad).is_null());
            assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(EINVAL));
        }
    }
}

#[test]
fn alternate_44k_and_2p2_44k_are_accepted() {
    let l = load();
    // 0x40 is the alternate 44.1 kHz bit; baizhu945's fork accepts it so the
    // MOMENTUM 5 (0x71 caps) can negotiate 44.1 kHz instead of falling to 48k.
    for (r2, byte6, features, want) in [
        (true, 0x42, 0x03, 0x42),
        (true, 0x0a, 0x80, 0x0a),
        (true, 0x0a, 0x83, 0x0a),
    ] {
        let mut config = if r2 { r2_caps(0x10, features) } else { PX7_S2E_CONFIG };
        config[6] = byte6;
        config[7] = 2;
        let mut info = validated_info(l.codec, &config);
        // SAFETY: valid buffers.
        let rc = unsafe {
            l.codec.validate_config.unwrap()(
                l.codec, 0, config.as_ptr().cast(), config.len(), &raw mut info,
            )
        };
        assert_eq!(rc, 0, "byte6={byte6:#x} features={features:#x}");
        let (n, out) = select_preferred(l.codec, &config[..], None);
        assert_eq!(n, 40);
        if want != 0 {
            assert_eq!(out[6], want, "byte6={byte6:#x} features={features:#x}");
        }
    }
}

#[test]
fn invalid_rate_masks_and_r1_non48_are_rejected() {
    let l = load();
    for (r2, byte6, features) in [
        (true, 0x02, 0),
        (true, 0x1a, 0),
        (true, 0x2a, 0),
        (true, 0x32, 0),
        (true, 0x82, 0),
        (true, 0xa2, 0),
        (false, 0x0a, 0),
        (false, 0x22, 0),
    ] {
        let mut config = if r2 {
            r2_caps(0x10, features)
        } else {
            PX7_S2E_CONFIG
        };
        config[6] = byte6;
        config[7] = 2;
        let mut info = validated_info(l.codec, &PX7_S2E_CONFIG);
        // SAFETY: valid buffers, malformed contents are intentional.
        unsafe {
            assert_eq!(
                l.codec.validate_config.unwrap()(
                    l.codec,
                    0,
                    config.as_ptr().cast(),
                    config.len(),
                    &raw mut info
                ),
                -EINVAL
            );
        }
        assert!(init(l.codec, &mut config, &info).is_null());
    }
    for (rate, features) in [
        (0x12, 0xff),
        (0x22, 0xff),
        (0x0a, 0x17),
        (0x08, 0),
        (0x20, 0),
    ] {
        let mut config = r2_caps(0x10, features);
        config[6] = rate;
        config[7] = 2;
        assert_eq!(validated_info(l.codec, &config).raw.channels, 2);
    }
    for mode in [0, 1, 4, 0x0a, 0x20] {
        let mut config = r2_caps(0x08, 0);
        config[7] = mode;
        let info = validated_info(l.codec, &PX7_S2E_CONFIG);
        assert!(init(l.codec, &mut config, &info).is_null());
    }
}

#[test]
fn new_rate_encode_consumes_only_fixed_whole_frame_blocks() {
    let l = load();
    for (rate, block) in [(44_100, 352), (96_000, 768)] {
        let (_, mut config) = select_preferred(l.codec, &r2_caps(0x38, 0), Some(rate));
        let info = validated_info(l.codec, &config);
        let data = init(l.codec, &mut config, &info);
        assert!(!data.is_null());
        let src = vec![0; block * 2 + 4];
        let mut out = [0; 1024];
        let (mut written, mut flush) = (99, 99);
        // SAFETY: initialized session and buffers of the supplied lengths.
        unsafe {
            assert_eq!(
                l.codec.encode.unwrap()(
                    data,
                    src.as_ptr().cast(),
                    block - 4,
                    out.as_mut_ptr().cast(),
                    out.len(),
                    &raw mut written,
                    &raw mut flush
                ),
                0
            );
            assert_eq!((written, flush), (0, 0));
            let mut packets = 0;
            let mut gap = 0;
            for i in 0..2000 {
                if i % 200 == 0 {
                    let step = if i % 400 == 0 {
                        l.codec.reduce_bitpool.unwrap()
                    } else {
                        l.codec.increase_bitpool.unwrap()
                    };
                    assert_eq!(step(data), if i % 400 == 0 { 2 } else { 3 });
                }
                assert_eq!(
                    l.codec.encode.unwrap()(
                        data,
                        src.as_ptr().cast(),
                        src.len(),
                        out.as_mut_ptr().cast(),
                        out.len(),
                        &raw mut written,
                        &raw mut flush
                    ),
                    block as c_int
                );
                assert_eq!(l.codec.get_block_size.unwrap()(data), block as c_int);
                gap += 1;
                if flush != 0 {
                    assert_eq!(written, 656);
                    assert!(gap <= 30, "{rate} stalled for {gap} blocks");
                    gap = 0;
                    packets += 1;
                } else {
                    assert_eq!(written, 0);
                }
            }
            assert!((70..=140).contains(&packets), "{rate} {packets} packets");
            l.codec.deinit.unwrap()(data);
        }
    }
}

#[test]
fn new_rate_abr_callbacks_preserve_fixed_blocks() {
    let l = load();
    for (rate, block, periods) in [
        (44_100, 352, [837u16, 1102, 837, 661, 529, 661, 837]),
        (96_000, 768, [1440u16, 1920, 1440, 1152, 768, 1152, 1440]),
    ] {
        let (_, mut config) = select_preferred(l.codec, &r2_caps(0x38, 3), Some(rate));
        let info = validated_info(l.codec, &config);
        let data = init(l.codec, &mut config, &info);
        assert!(!data.is_null());
        // SAFETY: initialized session and correctly sized buffers.
        unsafe {
            for (i, frames) in periods.into_iter().enumerate() {
                if i != 0 {
                    let (callback, level) = match i {
                        1 => (l.codec.reduce_bitpool.unwrap(), 2),
                        2 => (l.codec.increase_bitpool.unwrap(), 3),
                        3 => (l.codec.increase_bitpool.unwrap(), 4),
                        4 => (l.codec.increase_bitpool.unwrap(), 5),
                        5 => (l.codec.reduce_bitpool.unwrap(), 4),
                        _ => (l.codec.reduce_bitpool.unwrap(), 3),
                    };
                    assert_eq!(callback(data), level);
                }
                let mut out = [0; 1024];
                assert_eq!(
                    l.codec.start_encode.unwrap()(
                        data,
                        out.as_mut_ptr().cast(),
                        out.len(),
                        i as u16,
                        99
                    ),
                    12
                );
                let [hi, lo] = frames.to_be_bytes();
                assert_eq!(
                    out[..12],
                    [0x80, 0x60, 0, i as u8, 0, 0, hi, lo, 0, 0, 0, 0xa3]
                );
                assert_eq!(encode_until_flush(l.codec, data, &mut out[12..], 30).0, 656);
                assert_eq!(out[12] & 0x7f, 3);
                assert_eq!(l.codec.get_block_size.unwrap()(data), block);
                assert_eq!(l.codec.abr_process.unwrap()(data, 0), 0);
            }
            l.codec.deinit.unwrap()(data);
        }
    }
}

#[test]
fn rate_44k_switches_hq_ll_hq_without_changing_input_blocks() {
    let l = load();
    let c = l.codec;
    let props = init_props(c, None);
    let (_, mut config) = select_preferred(c, &r2_caps(0x38, 3), Some(44_100));
    let info = validated_info(c, &config);
    let data = init_with_props(c, &mut config, &info, props);
    assert!(!data.is_null());
    let mut runs = String::new();
    // SAFETY: initialized session, properties and valid buffers throughout.
    unsafe {
        for seq in 0..330u16 {
            if seq == 15 || seq == 150 {
                let quality = quality_props(i32::from(seq == 15));
                assert_eq!(c.set_props.unwrap()(props, quality.as_ptr().cast()), 1);
                assert_eq!(c.update_props.unwrap()(data, props), 0);
            }
            let mut out = [0; 1024];
            assert_eq!(
                c.start_encode.unwrap()(data, out.as_mut_ptr().cast(), out.len(), seq, 99),
                12
            );
            let kind = match out[4..12] {
                [0, 0, 0x03, 0x45, 0, 0, 0, 0xa3] => 'H',
                [0, 0, 0x01, 0xb9, 0, 0, 0, 0xa1] => 'L',
                _ => panic!("44k fields {:02x?}", &out[4..12]),
            };
            if !runs.ends_with(kind) {
                runs.push(kind);
            }
            let (size, _) = encode_until_flush(c, data, &mut out[12..], 22);
            assert_eq!(size, if kind == 'H' { 656 } else { 348 });
            assert_eq!(out[12] & 0x7f, if kind == 'H' { 3 } else { 4 });
            assert_eq!(c.get_block_size.unwrap()(data), 352);
            assert_eq!(out[2..4], seq.to_be_bytes());
        }
        assert_eq!(runs, "HLH");
        assert_eq!(c.get_interval.unwrap()(data), 19_000_000);
        assert_eq!(enum_props(c, props, SPA_PARAM_PROPS, 0).1, quality_props(0));
        c.deinit.unwrap()(data);
        c.clear_props.unwrap()(props);
    }
    let mut config = r2_caps(0x08, 0);
    config[6] = 0x0a;
    config[7] = 2;
    let data = init(l.ll, &mut config, &info);
    assert!(!data.is_null());
    // SAFETY: session returned by init.
    unsafe {
        assert_eq!(l.ll.get_interval.unwrap()(data), 10_000_000);
        assert_eq!(l.ll.get_block_size.unwrap()(data), 352);
        l.ll.deinit.unwrap()(data);
    }
}

#[test]
fn rate_96k_ll_is_unavailable_and_refused_without_changing_hq() {
    let l = load();
    let c = l.codec;
    let props = init_props(c, Some(c"ll"));
    let (_, mut config) = select_preferred(c, &r2_caps(0x38, 3), Some(96_000));
    let info = validated_info(c, &config);
    let data = init_with_props(c, &mut config, &info, props);
    assert!(!data.is_null());
    // SAFETY: session and properties returned by init callbacks.
    unsafe {
        assert_eq!(enum_props(c, props, SPA_PARAM_PROPS, 0).1, quality_props(0));
        let (_, labels) = enum_props(c, props, SPA_PARAM_PROP_INFO, 0);
        let labels: Vec<u8> = labels.iter().flat_map(|w| w.to_ne_bytes()).collect();
        assert!(!labels.windows(11).any(|w| w == b"Low latency"));
        assert_eq!(c.update_props.unwrap()(data, props), -ENOTSUP);
        assert_eq!(enum_props(c, props, SPA_PARAM_PROPS, 0).1, quality_props(0));
        let ll = quality_props(1);
        assert_eq!(c.set_props.unwrap()(props, ll.as_ptr().cast()), 1);
        assert_eq!(c.update_props.unwrap()(data, props), 0);
        assert_eq!(enum_props(c, props, SPA_PARAM_PROPS, 0).1, quality_props(0));
        for seq in 0..100u16 {
            let mut out = [0; 1024];
            assert_eq!(
                c.start_encode.unwrap()(data, out.as_mut_ptr().cast(), out.len(), seq, 99),
                12
            );
            assert_eq!(out[4..12], [0, 0, 0x05, 0xa0, 0, 0, 0, 0xa3]);
            assert_eq!(encode_until_flush(c, data, &mut out[12..], 15), (656, 15));
            assert_eq!(out[12] & 0x7f, 3);
            assert_eq!(c.get_block_size.unwrap()(data), 768);
        }
        c.deinit.unwrap()(data);
        c.clear_props.unwrap()(props);
    }
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

/// Checks both entries against the configured slot and default mode.
#[test]
fn codec_metadata() {
    let l = load();
    let owner = l.codec;
    let (want, want_alias) = match std::env::var("APTX_ADAPTIVE_SLOT").as_deref() {
        Ok("lc3plus_hr") => (
            SPA_BLUETOOTH_AUDIO_CODEC_LC3PLUS_HR,
            SPA_BLUETOOTH_AUDIO_CODEC_MPEG,
        ),
        Ok("ldac") => (
            SPA_BLUETOOTH_AUDIO_CODEC_LDAC,
            SPA_BLUETOOTH_AUDIO_CODEC_MPEG,
        ),
        _ => (
            SPA_BLUETOOTH_AUDIO_CODEC_APTX_HD,
            SPA_BLUETOOTH_AUDIO_CODEC_APTX,
        ),
    };
    assert_eq!(owner.id, want);
    assert!(owner.fill_caps.is_some());
    if want_alias == SPA_BLUETOOTH_AUDIO_CODEC_MPEG {
        assert_eq!(l.all.len(), 2);
    }
    for c in &l.all[2..] {
        // SAFETY: static strings.
        assert_ne!(unsafe { CStr::from_ptr(c.name) }, ids::CODEC_NAME);
    }
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
        assert_eq!(c.send_buf_size, ids::SEND_BUF_SIZE);
        assert!(c.abr_process.is_some());
    }
    check_alias(l.ll, owner, want_alias);
    if std::env::var_os(SLOT_CHILD_VAR).is_some() {
        std::process::exit(SLOT_CHILD_OK);
    }
}

fn check_alias(alias: &media_codec, owner: &media_codec, id: u32) {
    assert_eq!(alias.id, id);
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
        reduce_bitpool,
        increase_bitpool,
        get_delay
    );
    assert!(alias.decode.is_none());
}

const SLOT_CHILD_VAR: &str = "APTX_ADAPTIVE_TEST_SLOT_CHILD";
const SLOT_CHILD_OK: i32 = 42;

/// Runs one test in a child process with `env`; returns its stderr.
fn run_child(name: &str, env: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", name, "--test-threads=1", "--nocapture"])
        .env_remove("APTX_ADAPTIVE_SLOT")
        .env_remove("APTX_ADAPTIVE_DEFAULT_MODE")
        .env(SLOT_CHILD_VAR, "1");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.code() == Some(SLOT_CHILD_OK),
        "{name} {env:?}: {}{stderr}",
        String::from_utf8_lossy(&out.stdout),
    );
    stderr
}

#[test]
fn slot_and_default_mode_follow_env() {
    for slot in ["aptx", "ldac", "lc3plus_hr", "bogus"] {
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
        assert_eq!(
            out[..12],
            [0x80, 0x60, 0, 1, 0, 0, 0x02, 0xa0, 0, 0, 0, 0xa3]
        );
        assert_eq!(encode_until_flush(c, data, &mut out[12..], 7), (656, 7));
        assert_eq!((out[12] & 0x7f, out[13], out[15]), (0x01, 0x00, 0xa1));
        assert_eq!(c.abr_process.unwrap()(data, 0), 0);
        let mut delay = 99;
        c.get_delay.unwrap()(data, &raw mut delay, ptr::null_mut());
        assert_eq!(delay, 90);
        c.deinit.unwrap()(data);
    }
}

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
        assert_eq!(block, 48 * 2 * 4);
        let mut out = [0u8; 1024];
        assert_eq!(
            c.start_encode.unwrap()(data, out.as_mut_ptr().cast(), out.len(), 1, 0),
            12
        );
        assert_eq!(out[4..12], [0, 0, 0x03, 0x90, 0, 0, 0, 0xa3]);
        assert_eq!(encode_until_flush(c, data, &mut out[12..], 19), (656, 19));
        assert_eq!((out[12] & 0x7f, out[13], out[15]), (0x03, 0x00, 0xa1));
        c.deinit.unwrap()(data);
    }
}

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
        let (written, _) = encode_until_flush(c, data, &mut out[12..], 7);
        (written, fields)
    }
}

const HQ_FIELDS: [u8; 8] = [0, 0, 0x02, 0xa0, 0, 0, 0, 0xa3];
const LL_FIELDS: [u8; 8] = [0, 0, 0x01, 0xe0, 0, 0, 0, 0xa1];

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
        assert!((29..=32).contains(&switch), "{kinds}");
        assert!(!kinds[switch..].contains('L'), "{kinds}");
        assert_eq!(c.get_interval.unwrap()(data), 14_000_000);
        assert_eq!(enum_props(c, props, SPA_PARAM_PROPS, 0).1, hq.to_vec());
        c.deinit.unwrap()(data);
        c.clear_props.unwrap()(props);
    }
}

#[test]
fn r2_sink_runs_ll() {
    let l = load();
    let c = l.codec;
    let props = init_props(c, Some(c"ll"));
    let mut config = PX7_S2E_CONFIG;
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
        assert_eq!(encode_until_flush(c, data, &mut out[12..], 10), (348, 10));
        assert_eq!(out[12] & 0x7f, 4);
        c.deinit.unwrap()(data);
        c.clear_props.unwrap()(props);
    }
}

#[test]
fn ll_entry_selects_same_config() {
    let l = load();
    assert_eq!(select(l.ll, &PX7_S2E), select(l.codec, &PX7_S2E));
    assert_eq!(select(l.ll, &PX7_S2E).0 as usize, PX7_S2E.len());
}

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

#[test]
fn bitpool_callbacks_step_the_level() {
    let l = load();
    let c = l.codec;
    for codec in [l.codec, l.ll] {
        assert!(codec.reduce_bitpool.is_some() && codec.increase_bitpool.is_some());
    }
    let mut config = PX7_S2E_CONFIG;
    let info = validated_info(c, &config);
    let data = init(c, &mut config, &info);
    assert!(!data.is_null());
    let reduce = c.reduce_bitpool.unwrap();
    let increase = c.increase_bitpool.unwrap();
    // SAFETY: data from init, valid until deinit.
    unsafe {
        let packet = |seq: u16| {
            let mut out = [0u8; 1024];
            let hdr = c.start_encode.unwrap()(data, out.as_mut_ptr().cast(), out.len(), seq, 0);
            assert_eq!(hdr, 12);
            let fields: [u8; 8] = out[4..12].try_into().unwrap();
            let (written, _) = encode_until_flush(c, data, &mut out[12..], 8);
            assert_eq!(written, 656);
            fields
        };
        let ts = |low: u16| {
            let [a, b] = low.to_be_bytes();
            [0, 0, a, b, 0, 0, 0, 0xa3]
        };
        assert_eq!(packet(0), ts(0x02a0));
        assert_eq!(reduce(data), 2);
        assert_eq!(packet(1), ts(0x0300));
        assert_eq!(reduce(data), 2);
        assert_eq!(packet(2), ts(0x0300));
        assert_eq!(increase(data), 3);
        assert_eq!(packet(3), ts(0x02a0));
        assert_eq!(increase(data), 4);
        assert_eq!(increase(data), 4);
        assert_eq!(packet(4), ts(0x0240));
        assert_eq!(reduce(data), 3);
        assert_eq!(packet(5), ts(0x02a0));
        assert_eq!(c.get_block_size.unwrap()(data), BLOCK_SIZE);
        c.deinit.unwrap()(data);
    }
}

#[test]
fn bitpool_callbacks_in_r1_ll_change_nothing() {
    let l = load();
    let c = l.codec;
    let props = init_props(c, Some(c"ll"));
    let mut config = PX7_S2E_CONFIG;
    let info = validated_info(c, &config);
    let data = init_with_props(c, &mut config, &info, props);
    assert!(!data.is_null());
    // SAFETY: data and props from init and init_props.
    unsafe {
        for seq in 0..3 {
            assert_eq!(c.reduce_bitpool.unwrap()(data), 3);
            assert_eq!(c.increase_bitpool.unwrap()(data), 3);
            assert_eq!(next_packet(c, data, props, false, seq), (348, LL_FIELDS));
        }
        c.deinit.unwrap()(data);
        c.clear_props.unwrap()(props);
    }
}

const SYSTEM_PLUGIN_DIR: &str = env!("APTX_ADAPTIVE_SYSTEM_PLUGINDIR");
const APTX_LIB: &str = "bluez5/libspa-codec-bluez5-aptx.so";
const PLUGIN_DIR_VAR: &str = "APTX_ADAPTIVE_TEST_PLUGIN_DIR";

/// The installed PipeWire aptX plugin, if it has our codec ABI version.
fn real_aptx() -> Option<PathBuf> {
    let path = Path::new(SYSTEM_PLUGIN_DIR).join(APTX_LIB);
    if !path.exists() {
        eprintln!("skipped: no {}", path.display());
        return None;
    }
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "real_aptx_abi_probe", "--test-threads=1"])
        .env(SLOT_CHILD_VAR, "1");
    if cmd.status().unwrap().code() == Some(SLOT_CHILD_OK) {
        Some(path)
    } else {
        eprintln!("skipped: {} has another codec ABI", path.display());
        None
    }
}

/// Child only: exits OK if the real aptX plugin's interface version is ours.
#[test]
fn real_aptx_abi_probe() {
    if std::env::var_os(SLOT_CHILD_VAR).is_none() {
        return;
    }
    let l = load_from(
        &Path::new(SYSTEM_PLUGIN_DIR).join(APTX_LIB),
        ids::FACTORY_NAME_APTX,
    );
    assert!(!l.all.is_empty());
    std::process::exit(SLOT_CHILD_OK);
}

/// A plugin dir holding only our plugin, installed under the aptX file name.
fn own_plugin_dir(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.scratch/dlopen-tests")
        .join(format!("{tag}-{}", std::process::id()));
    let lib = dir.join(APTX_LIB);
    std::fs::create_dir_all(lib.parent().unwrap()).unwrap();
    std::fs::copy(plugin_path(), &lib).unwrap();
    dir
}

const CODEC_ORDER: [u32; 8] = [
    0x200, // LC3
    SPA_BLUETOOTH_AUDIO_CODEC_LDAC,
    SPA_BLUETOOTH_AUDIO_CODEC_APTX_HD,
    SPA_BLUETOOTH_AUDIO_CODEC_APTX,
    4,  // AAC
    18, // OPUS_G
    SPA_BLUETOOTH_AUDIO_CODEC_LC3PLUS_HR,
    SPA_BLUETOOTH_AUDIO_CODEC_MPEG,
];

/// `codec_order_cmp` from PipeWire 1.6.9's `codec-loader.c`, abridged to the ids above
/// (every other id ranks after them).
fn codec_order_cmp(a: &&media_codec, b: &&media_codec) -> std::cmp::Ordering {
    let order = |c: &media_codec| {
        CODEC_ORDER
            .iter()
            .position(|&id| id == c.id)
            .unwrap_or(CODEC_ORDER.len())
    };
    let (pa, pb) = (ptr::from_ref(*a), ptr::from_ref(*b));
    order(a).cmp(&order(b)).then(pa.cmp(&pb))
}

fn as_bytes(c: &media_codec) -> &[u8] {
    // SAFETY: a media_codec is plain data, size_of bytes long.
    unsafe { std::slice::from_raw_parts(ptr::from_ref(c).cast(), size_of::<media_codec>()) }
}

fn fill_caps(c: &media_codec) -> Vec<u8> {
    let mut caps = [0u8; A2DP_MAX_CAPS_SIZE];
    // SAFETY: caps is A2DP_MAX_CAPS_SIZE bytes.
    let n = unsafe { c.fill_caps.unwrap()(c, 0, ptr::null(), caps.as_mut_ptr()) };
    assert!(n > 0);
    caps[..n as usize].to_vec()
}

#[test]
#[ignore = "requires a stock aptX plugin; this host installs the adaptive plugin as the aptX plugin"]
fn aptx_slot_passes_real_codecs_through() {
    let Some(dir) = std::env::var_os(PLUGIN_DIR_VAR) else {
        let Some(real) = real_aptx() else { return };
        let ours = own_plugin_dir("passthrough");
        let search = format!(
            "{}:{}",
            ours.display(),
            real.parent().unwrap().parent().unwrap().display()
        );
        for mode in ["hq", "ll"] {
            let log = run_child(
                "aptx_slot_passes_real_codecs_through",
                &[
                    ("APTX_ADAPTIVE_SLOT", "aptx"),
                    ("APTX_ADAPTIVE_DEFAULT_MODE", mode),
                    ("SPA_PLUGIN_DIR", &search),
                    (PLUGIN_DIR_VAR, ours.to_str().unwrap()),
                ],
            );
            assert!(log.contains("aptX passthrough: 6 codecs from"), "{log}");
            assert!(log.contains(SYSTEM_PLUGIN_DIR), "{log}");
            assert!(!log.contains("unavailable"), "{log}");
        }
        std::fs::remove_dir_all(ours).unwrap();
        return;
    };
    let l = load_from(&Path::new(&dir).join(APTX_LIB), ids::FACTORY_NAME_APTX);
    let real = load_from(
        &Path::new(SYSTEM_PLUGIN_DIR).join(APTX_LIB),
        ids::FACTORY_NAME_APTX,
    );

    let ll_default = std::env::var("APTX_ADAPTIVE_DEFAULT_MODE").as_deref() == Ok("ll");
    let names: Vec<&CStr> = l
        .all
        .iter()
        // SAFETY: static strings.
        .map(|c| unsafe { CStr::from_ptr(c.name) })
        .collect();
    let (owner_name, alias_name) = if ll_default {
        (c"aptx_adaptive_ll", c"aptx_adaptive")
    } else {
        (c"aptx_adaptive", c"aptx_adaptive_ll")
    };
    assert_eq!(
        names,
        [
            owner_name,
            alias_name,
            c"aptx_hd",
            c"aptx",
            c"aptx_ll",
            c"aptx_ll",
            c"aptx_ll_duplex",
            c"aptx_ll_duplex",
        ]
    );
    let (owner, alias) = (l.all[0], l.all[1]);
    assert_eq!(
        (owner.id, alias.id),
        (
            SPA_BLUETOOTH_AUDIO_CODEC_APTX_HD,
            SPA_BLUETOOTH_AUDIO_CODEC_APTX
        )
    );
    assert!(owner.fill_caps.is_some() && alias.fill_caps.is_none());

    let copies = &l.all[2..];
    assert_eq!(copies.len(), real.all.len());
    for (copy, orig) in copies.iter().zip(&real.all) {
        assert!(!ptr::eq(*copy, *orig), "a copy, not the real static");
        assert_eq!(as_bytes(copy), as_bytes(orig), "same fields and pointers");
        assert_eq!(
            copy.encode.map(|f| f as usize),
            orig.encode.map(|f| f as usize)
        );
        for o in [owner, alias] {
            assert!(ptr::from_ref(o) < ptr::from_ref(*copy));
        }
    }
    let hd = copies[0];
    assert_eq!(hd.id, SPA_BLUETOOTH_AUDIO_CODEC_APTX_HD);
    let caps = fill_caps(hd);
    assert_eq!(caps, fill_caps(real.all[0]));
    assert_eq!(caps.len(), 11);
    assert_eq!(caps[..6], [0xD7, 0, 0, 0, 0x24, 0]);
    assert_eq!(fill_caps(copies[1])[..6], [0x4F, 0, 0, 0, 0x01, 0]);

    let mut sorted = l.all.clone();
    sorted.sort_by(codec_order_cmp);
    let first = |id: u32| *sorted.iter().find(|c| c.id == id).unwrap();
    assert!(ptr::eq(first(SPA_BLUETOOTH_AUDIO_CODEC_APTX_HD), owner));
    assert!(ptr::eq(first(SPA_BLUETOOTH_AUDIO_CODEC_APTX), alias));
    let pos = |c: &media_codec| sorted.iter().position(|s| ptr::eq(*s, c)).unwrap();
    assert!(pos(owner) < pos(copies[0]) && pos(alias) < pos(copies[1]));
    std::process::exit(SLOT_CHILD_OK);
}

#[test]
fn aptx_slot_without_real_plugin_exports_only_ours() {
    let Some(dir) = std::env::var_os(PLUGIN_DIR_VAR) else {
        let ours = own_plugin_dir("alone");
        for mode in ["hq", "ll"] {
            let log = run_child(
                "aptx_slot_without_real_plugin_exports_only_ours",
                &[
                    ("APTX_ADAPTIVE_SLOT", "aptx"),
                    ("APTX_ADAPTIVE_DEFAULT_MODE", mode),
                    ("SPA_PLUGIN_DIR", ours.to_str().unwrap()),
                    (PLUGIN_DIR_VAR, ours.to_str().unwrap()),
                ],
            );
            assert!(
                log.contains("aptx-adaptive: aptX/aptX HD unavailable: no real"),
                "{log}"
            );
        }
        std::fs::remove_dir_all(ours).unwrap();
        return;
    };
    let l = load_from(&Path::new(&dir).join(APTX_LIB), ids::FACTORY_NAME_APTX);
    assert_eq!(l.all.len(), 2);
    assert_eq!(
        (l.codec.id, l.ll.id),
        (
            SPA_BLUETOOTH_AUDIO_CODEC_APTX_HD,
            SPA_BLUETOOTH_AUDIO_CODEC_APTX
        )
    );
    check_alias(l.ll, l.codec, SPA_BLUETOOTH_AUDIO_CODEC_APTX);
    std::process::exit(SLOT_CHILD_OK);
}

//! Load the built cdylib the way codec-loader.c does and drive the ported
//! aptX Adaptive factory end to end: two media_codec entries, capability and
//! configuration bodies, a real native LL and Hi-Res encode, and the RTP
//! header/timing.
#![allow(clippy::doc_markdown, reason = "C ABI names are used as prose")]

#[expect(
    dead_code,
    reason = "shared with the cdylib; the test only touches part of the FFI surface"
)]
#[path = "../src/ffi.rs"]
mod ffi;
#[expect(dead_code, reason = "the test uses a subset of the shared ids")]
#[path = "../src/ids.rs"]
mod ids;

use std::ffi::{CStr, c_int, c_void};
use std::path::PathBuf;
use std::process::Command;
use std::ptr;
use std::sync::OnceLock;

use ffi::{
    A2DP_MAX_CAPS_SIZE, NEED_FLUSH_ALL, SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR,
    SPA_AUDIO_FORMAT_S32_LE, SPA_AUDIO_MAX_CHANNELS, SPA_BLUETOOTH_AUDIO_CODEC_APTX_ADAPTIVE_HIRES,
    SPA_BLUETOOTH_AUDIO_CODEC_APTX_ADAPTIVE_LL, SPA_MEDIA_SUBTYPE_RAW, SPA_MEDIA_TYPE_AUDIO,
    SPA_VERSION_BLUEZ5_CODEC_MEDIA, SPA_VERSION_HANDLE_FACTORY, media_codec, spa_audio_info,
    spa_audio_info_raw, spa_bluez5_codec_a2dp, spa_handle, spa_handle_factory,
};

const MTU: usize = 895;
const RTP_HEADER_LEN: usize = 12;
const LL_BLOCK: usize = 480 * 8;
const HIRES_BLOCK: usize = 768 * 8;

type EnumFn = unsafe extern "C" fn(*mut *const spa_handle_factory, *mut u32) -> c_int;

fn plugin_path() -> &'static PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args(["build", "-p", env!("CARGO_PKG_NAME"), "--offline"])
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

fn audio_info(rate: u32) -> spa_audio_info {
    let mut raw = spa_audio_info_raw {
        format: SPA_AUDIO_FORMAT_S32_LE,
        flags: 0,
        rate,
        channels: 2,
        position: [0; SPA_AUDIO_MAX_CHANNELS],
    };
    raw.position[0] = SPA_AUDIO_CHANNEL_FL;
    raw.position[1] = SPA_AUDIO_CHANNEL_FR;
    spa_audio_info {
        media_type: SPA_MEDIA_TYPE_AUDIO,
        media_subtype: SPA_MEDIA_SUBTYPE_RAW,
        raw,
        _union_tail: [0],
    }
}

struct Plugin {
    _lib: libloading::Library,
    _handle: Vec<u64>,
    ll: &'static media_codec,
    hires: &'static media_codec,
}

fn load() -> Plugin {
    let path = plugin_path();
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
        if name == ids::FACTORY_NAME_NATIVE {
            found = Some(f);
        }
    }
    let f = found.unwrap_or_else(|| panic!("no native factory in {names:?}"));

    // SAFETY: the factory is a static the library owns.
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
    assert_eq!(all.len(), 2, "LL owner plus Hi-Res alias");
    Plugin {
        _lib: lib,
        _handle: handle,
        ll: all[0],
        hires: all[1],
    }
}

fn fill_caps(codec: &media_codec) -> Vec<u8> {
    let mut caps = vec![0u8; A2DP_MAX_CAPS_SIZE];
    // SAFETY: the codec is a static entry and caps is A2DP_MAX_CAPS_SIZE bytes.
    let n = unsafe { codec.fill_caps.unwrap()(codec, 0, ptr::null(), caps.as_mut_ptr()) };
    assert_eq!(n, 40);
    caps.truncate(n as usize);
    caps
}

fn select_config(codec: &media_codec, caps: &[u8]) -> Vec<u8> {
    let mut config = vec![0u8; A2DP_MAX_CAPS_SIZE];
    // SAFETY: arguments match the C contract.
    let n = unsafe {
        codec.select_config.unwrap()(
            codec,
            0,
            caps.as_ptr().cast(),
            caps.len(),
            ptr::null(),
            ptr::null(),
            config.as_mut_ptr(),
            ptr::null_mut(),
        )
    };
    assert_eq!(n, 40);
    config.truncate(n as usize);
    config
}

struct Stream {
    codec: &'static media_codec,
    data: *mut c_void,
}

impl Stream {
    fn open(codec: &'static media_codec, config: &[u8], rate: u32) -> Self {
        let info = audio_info(rate);
        // SAFETY: config and info are valid for the call.
        let data = unsafe {
            codec.init.unwrap()(
                codec,
                0,
                config.as_ptr().cast_mut().cast(),
                config.len(),
                &raw const info,
                ptr::null_mut(),
                MTU,
            )
        };
        assert!(!data.is_null(), "codec_init failed");
        Self { codec, data }
    }

    fn block_size(&self) -> usize {
        // SAFETY: data came from codec_init.
        let n = unsafe { self.codec.get_block_size.unwrap()(self.data) };
        assert!(n > 0);
        n as usize
    }

    fn packet(&mut self, seqnum: u16) -> Vec<u8> {
        let mut packet = vec![0u8; 4096];
        // SAFETY: packet is writable for its length.
        let n = unsafe {
            self.codec.start_encode.unwrap()(
                self.data,
                packet.as_mut_ptr().cast(),
                packet.len(),
                seqnum,
                0,
            )
        };
        assert_eq!(n, RTP_HEADER_LEN as c_int);
        packet
    }

    fn encode(&mut self, src: &[u8], packet: &mut [u8]) -> (usize, usize, c_int) {
        let mut out = 0usize;
        let mut flush = -1;
        // SAFETY: src/packet are valid for their lengths, out/flush are writable.
        let consumed = unsafe {
            self.codec.encode.unwrap()(
                self.data,
                src.as_ptr().cast(),
                src.len(),
                packet[RTP_HEADER_LEN..].as_mut_ptr().cast(),
                packet.len() - RTP_HEADER_LEN,
                &raw mut out,
                &raw mut flush,
            )
        };
        (consumed as usize, out, flush)
    }

    fn delay(&self) -> (u32, u32) {
        let (mut encoder, mut decoder) = (0u32, 0u32);
        // SAFETY: data came from codec_init, out-pointers are valid.
        unsafe { self.codec.get_delay.unwrap()(self.data, &raw mut encoder, &raw mut decoder) };
        (encoder, decoder)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: data came from codec_init and is deinitialised once.
        unsafe { self.codec.deinit.unwrap()(self.data) };
    }
}

fn rtp_timestamp(packet: &[u8]) -> u32 {
    u32::from_be_bytes(packet[4..8].try_into().unwrap())
}

#[test]
fn ported_factory_exposes_two_entries_and_encodes() {
    // SAFETY: this test binary is one process and no other thread reads the
    // environment; the plugin restores its own APTX_NATIVE_* variables.
    unsafe { std::env::set_var("APTX_ADAPTIVE_RTP_TS", "us") };
    let plugin = load();

    assert_eq!(plugin.ll.id, SPA_BLUETOOTH_AUDIO_CODEC_APTX_ADAPTIVE_LL);
    assert_eq!(
        plugin.hires.id,
        SPA_BLUETOOTH_AUDIO_CODEC_APTX_ADAPTIVE_HIRES
    );
    assert!(plugin.ll.fill_caps.is_some());
    assert!(plugin.hires.fill_caps.is_none());
    // SAFETY: static NUL-terminated strings.
    unsafe {
        assert_eq!(
            CStr::from_ptr(plugin.ll.endpoint_name),
            ids::ADAPTIVE_ENDPOINT
        );
        assert_eq!(
            CStr::from_ptr(plugin.hires.endpoint_name),
            ids::ADAPTIVE_ENDPOINT
        );
        assert_eq!(CStr::from_ptr(plugin.ll.name), ids::ADAPTIVE_LL_NAME);
        assert_eq!(CStr::from_ptr(plugin.hires.name), ids::ADAPTIVE_HIRES_NAME);
    }

    // The LL owner advertises the 48 kHz + 96 kHz union.
    let caps = fill_caps(plugin.ll);
    assert_eq!(caps[6], 0x30);
    assert_eq!(caps[7], 0x0a);

    // LL: 48 kHz, 10 ms, 348 bytes, 0xAE SSRC, 10000 us RTP step.
    let ll_config = select_config(plugin.ll, &caps);
    assert_eq!(ll_config[6], 0x10);
    assert_eq!(ll_config[7], 0x08);
    let mut ll = Stream::open(plugin.ll, &ll_config, 48_000);
    assert_eq!(ll.block_size(), LL_BLOCK);
    assert_eq!(ll.delay(), (48, 0));
    let mut packet = ll.packet(1);
    assert_eq!(&packet[..2], [0x80, 96]);
    assert_eq!(u32::from_be_bytes(packet[8..12].try_into().unwrap()), 0xAE);
    let pcm = vec![0u8; LL_BLOCK];
    let (consumed, out, flush) = ll.encode(&pcm, &mut packet);
    assert_eq!(consumed, LL_BLOCK);
    assert_eq!(out, 348, "LL codec payload");
    assert_eq!(flush, NEED_FLUSH_ALL);
    let ts1 = rtp_timestamp(&packet);
    let mut packet2 = ll.packet(2);
    let (_, out2, flush2) = ll.encode(&pcm, &mut packet2);
    assert_eq!(out2, 348);
    assert_eq!(flush2, NEED_FLUSH_ALL);
    assert_eq!(rtp_timestamp(&packet2).wrapping_sub(ts1), 10_000);

    // Hi-Res: 96 kHz, 8 ms, 656 bytes, 8000 us RTP step. The sink caps
    // advertise only 96 kHz so the primary format wins.
    let mut hires_caps = caps.clone();
    hires_caps[6] = 0x20;
    let hires_config = select_config(plugin.hires, &hires_caps);
    assert_eq!(hires_config[6], 0x20);
    let mut hires = Stream::open(plugin.hires, &hires_config, 96_000);
    assert_eq!(hires.block_size(), HIRES_BLOCK);
    let mut hires_packet = hires.packet(3);
    let hires_pcm = vec![0u8; HIRES_BLOCK];
    let (hires_consumed, hires_out, hires_flush) = hires.encode(&hires_pcm, &mut hires_packet);
    assert_eq!(hires_consumed, HIRES_BLOCK);
    assert_eq!(hires_out, 656, "Hi-Res codec payload");
    assert_eq!(hires_flush, NEED_FLUSH_ALL);
    let ts1 = rtp_timestamp(&hires_packet);
    let mut hires_packet2 = hires.packet(4);
    let (_, hires_out2, _) = hires.encode(&hires_pcm, &mut hires_packet2);
    assert_eq!(hires_out2, 656);
    assert_eq!(rtp_timestamp(&hires_packet2).wrapping_sub(ts1), 8_000);
}

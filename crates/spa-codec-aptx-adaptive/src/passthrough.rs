//! The `aptx` slot (ADR 0004): our file replaces PipeWire's aptX plugin, so we load the
//! real one and export its codecs after ours.
//!
//! The real `media_codec`s are copied by value into [`Table::real`], right after our two
//! entries in the same `#[repr(C)]` struct. `codec-loader.c` breaks ties between equal ids by
//! comparing the entry pointers, so ours (lower addresses) sort first. Pointers inside the
//! copies (callbacks, strings, `duplex_codec`) still lead into the real library, which stays
//! loaded for the life of the process.

use std::cell::UnsafeCell;
use std::ffi::{CStr, CString, OsStr, c_void};
use std::io::Write as _;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Once;

use crate::ffi::{
    A2DP_CODEC_VENDOR, DlInfo, MEDIA_CODEC_A2DP, RTLD_LOCAL, RTLD_NOW,
    SPA_BLUETOOTH_AUDIO_CODEC_APTX, SPA_BLUETOOTH_AUDIO_CODEC_APTX_HD,
    SPA_VERSION_BLUEZ5_CODEC_MEDIA, SpaHandleFactoryEnumFn, media_codec, spa_bluez5_codec_a2dp,
    spa_dict, spa_handle, spa_handle_factory, spa_support,
};
use crate::ids;

/// The real plugin exports 6 entries in PipeWire 1.6.9.
pub const MAX_REAL: usize = 16;
/// The real handle is a `spa_handle` plus a `spa_bluez5_codec_a2dp` (64 bytes in 1.6.9).
const MAX_HANDLE_SIZE: usize = 4096;
pub const REAL_LIB: &str = "bluez5/libspa-codec-bluez5-aptx.so";
/// PipeWire's compiled-in plugin dir, used when `SPA_PLUGIN_DIR` is unset (as PipeWire does).
pub const SYSTEM_PLUGIN_DIR: &str = env!("APTX_ADAPTIVE_SYSTEM_PLUGINDIR");

#[repr(C)]
pub struct Table {
    ours: [media_codec; 2],
    real: UnsafeCell<[MaybeUninit<media_codec>; MAX_REAL]>,
    list: UnsafeCell<[*const media_codec; 2 + MAX_REAL + 1]>,
    once: Once,
}

// SAFETY: `real` and `list` are written only inside `once`, and read only after it completes.
unsafe impl Sync for Table {}

impl Table {
    pub const fn new(owner: media_codec, alias: media_codec) -> Self {
        Self {
            ours: [owner, alias],
            real: UnsafeCell::new([const { MaybeUninit::uninit() }; MAX_REAL]),
            list: UnsafeCell::new([ptr::null(); 2 + MAX_REAL + 1]),
            once: Once::new(),
        }
    }

    pub fn ours(&self) -> [*const media_codec; 2] {
        [&raw const self.ours[0], &raw const self.ours[1]]
    }

    /// The NULL-terminated export list, loading the real plugin on first use.
    pub fn codecs(
        &self,
        info: *const spa_dict,
        support: *const spa_support,
        n_support: u32,
    ) -> *const *const media_codec {
        self.once.call_once(|| {
            let res = find_and_load(info, support, n_support);
            // SAFETY: inside `once`, nobody else touches the cells.
            unsafe { self.fill(res) };
        });
        self.list.get().cast()
    }

    /// # Safety
    /// Only from inside `self.once`.
    unsafe fn fill(&self, res: Result<Loaded, String>) {
        // SAFETY: caller contract.
        let list = unsafe { &mut *self.list.get() };
        let [owner, alias] = self.ours();
        list[0] = owner;
        list[1] = alias;
        match res {
            Ok(l) => {
                let real = self.real.get().cast::<media_codec>();
                for (i, &c) in l.codecs.iter().enumerate() {
                    // SAFETY: `c` passed `validate`, and i < MAX_REAL; a bitwise copy keeps
                    // every pointer inside it.
                    unsafe {
                        real.add(i).write(ptr::read(c));
                        list[2 + i] = real.add(i);
                    }
                }
                log(&format!(
                    "aptX passthrough: {} codecs from {}",
                    l.codecs.len(),
                    l.path.display()
                ));
            }
            Err(e) => log(&format!("aptX/aptX HD unavailable: {e}")),
        }
    }
}

fn log(msg: &str) {
    let _ = writeln!(std::io::stderr(), "aptx-adaptive: {msg}");
}

struct Loaded {
    path: PathBuf,
    codecs: Vec<*const media_codec>,
}

/// Where to look, in order: `SPA_PLUGIN_DIR` (colon-separated, like PipeWire) or, if it's
/// unset, PipeWire's own default dir.
pub fn candidates(spa_plugin_dir: Option<&OsStr>, system_dir: &Path) -> Vec<PathBuf> {
    match spa_plugin_dir {
        Some(dirs) => dirs
            .as_bytes()
            .split(|&b| b == b':')
            .filter(|d| !d.is_empty())
            .map(|d| Path::new(OsStr::from_bytes(d)).join(REAL_LIB))
            .collect(),
        None => vec![system_dir.join(REAL_LIB)],
    }
}

fn file_id(p: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(p).ok().map(|m| (m.dev(), m.ino()))
}

fn own_path() -> Option<PathBuf> {
    // SAFETY: zeroed DlInfo is plain data; dladdr fills it for an address in our object.
    unsafe {
        let mut info: DlInfo = std::mem::zeroed();
        let addr = crate::spa_handle_factory_enum as *const c_void;
        if crate::ffi::dladdr(addr, &raw mut info) == 0 || info.fname.is_null() {
            return None;
        }
        Some(PathBuf::from(OsStr::from_bytes(
            CStr::from_ptr(info.fname).to_bytes(),
        )))
    }
}

fn dlerror() -> String {
    // SAFETY: dlerror returns null or a NUL-terminated string.
    unsafe {
        let e = crate::ffi::dlerror();
        if e.is_null() {
            "unknown dlopen error".into()
        } else {
            CStr::from_ptr(e).to_string_lossy().into_owned()
        }
    }
}

fn find_and_load(
    info: *const spa_dict,
    support: *const spa_support,
    n_support: u32,
) -> Result<Loaded, String> {
    let me = own_path().and_then(|p| file_id(&p));
    let dir = std::env::var_os("SPA_PLUGIN_DIR");
    let paths = candidates(dir.as_deref(), Path::new(SYSTEM_PLUGIN_DIR));
    let mut errors = Vec::new();
    for path in &paths {
        let Some(id) = file_id(path) else { continue };
        if me == Some(id) {
            continue;
        }
        let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
            continue;
        };
        // SAFETY: a NUL-terminated path.
        let lib = unsafe { crate::ffi::dlopen(c_path.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
        if lib.is_null() {
            errors.push(dlerror());
            continue;
        }
        // SAFETY: lib is a fresh dlopen handle.
        match unsafe { probe(lib, info, support, n_support) } {
            Ok(codecs) => {
                return Ok(Loaded {
                    path: path.clone(),
                    codecs,
                });
            }
            Err(e) => {
                // SAFETY: nothing from lib is kept on failure.
                unsafe { crate::ffi::dlclose(lib) };
                errors.push(format!("{}: {e}", path.display()));
            }
        }
    }
    if errors.is_empty() {
        let tried: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        Err(format!(
            "no real {REAL_LIB} found (looked in {})",
            tried.join(", ")
        ))
    } else {
        Err(errors.join("; "))
    }
}

const REAL_FACTORY: &CStr = c"api.codec.bluez5.media.aptx";

/// Finds the real factory, makes a handle and reads its codec list. On success the handle
/// and the library stay alive for the life of the process.
///
/// # Safety
/// `lib` must be a valid dlopen handle.
unsafe fn probe(
    lib: *mut c_void,
    info: *const spa_dict,
    support: *const spa_support,
    n_support: u32,
) -> Result<Vec<*const media_codec>, String> {
    // SAFETY: caller contract; the symbol has the C type of spa_handle_factory_enum.
    unsafe {
        let sym = crate::ffi::dlsym(lib, c"spa_handle_factory_enum".as_ptr());
        if sym.is_null() {
            return Err("no spa_handle_factory_enum".into());
        }
        let enum_fn: SpaHandleFactoryEnumFn = std::mem::transmute(sym);
        let mut factory: *const spa_handle_factory = ptr::null();
        let mut index = 0u32;
        let f = loop {
            if enum_fn(&raw mut factory, &raw mut index) <= 0 || index > 64 {
                return Err(format!("no factory {}", REAL_FACTORY.to_string_lossy()));
            }
            if let Some(f) = factory.as_ref()
                && f.version >= 1
                && !f.name.is_null()
                && CStr::from_ptr(f.name) == REAL_FACTORY
            {
                break f;
            }
        };
        // Another copy of this plugin would look for the real one itself; don't init it.
        if is_ours(f.info) {
            return Err("it's another copy of the aptX Adaptive plugin".into());
        }
        let (Some(get_size), Some(init)) = (f.get_size, f.init) else {
            return Err("factory without get_size/init".into());
        };
        let size = get_size(f, info);
        if !(size_of::<spa_handle>()..=MAX_HANDLE_SIZE).contains(&size) {
            return Err(format!("unexpected handle size {size}"));
        }
        let mut mem = vec![0u64; size.div_ceil(8)].into_boxed_slice();
        let h = mem.as_mut_ptr().cast::<spa_handle>();
        let res = init(f, h, info, support, n_support);
        if res < 0 {
            return Err(format!("factory init failed ({res})"));
        }
        let clear = || {
            if let Some(c) = (*h).clear {
                c(h);
            }
        };
        let mut iface: *mut c_void = ptr::null_mut();
        let got = (*h)
            .get_interface
            .map_or(-1, |g| g(h, ids::INTERFACE_TYPE.as_ptr(), &raw mut iface));
        if got < 0 || iface.is_null() {
            clear();
            return Err("no Bluez5 codec interface".into());
        }
        let iface = &*iface.cast::<spa_bluez5_codec_a2dp>();
        if iface.iface.version != SPA_VERSION_BLUEZ5_CODEC_MEDIA {
            clear();
            return Err(format!(
                "codec ABI version {} != {SPA_VERSION_BLUEZ5_CODEC_MEDIA}",
                iface.iface.version
            ));
        }
        let mut codecs = Vec::new();
        if !iface.codecs.is_null() {
            while codecs.len() <= MAX_REAL {
                let c = *iface.codecs.add(codecs.len());
                if c.is_null() {
                    break;
                }
                codecs.push(c);
            }
        }
        if let Err(e) = validate(&codecs) {
            clear();
            return Err(e);
        }
        // The copies point into the handle's library, so the handle (a few dozen bytes) and
        // our dlopen reference live as long as the process.
        Box::leak(mem);
        Ok(codecs)
    }
}

/// # Safety
/// `info` must be null or a valid `spa_dict`.
unsafe fn is_ours(info: *const spa_dict) -> bool {
    // SAFETY: caller contract.
    let desc = unsafe { crate::dict_lookup(info, "factory.description") };
    desc == ids::FACTORY_DESCRIPTION.to_str().ok()
}

/// Checks that the list looks like PipeWire 1.6's aptX plugin with our struct layout.
///
/// The ABI version check already covers the struct layout; this guards against a plugin that
/// has the version but not the contents we expect. Every entry needs a name, A2DP kind,
/// vendor codec type and the callbacks PipeWire calls on any A2DP codec; `aptx_hd` and `aptx`
/// must also have their ids and vendor ids where we expect them.
fn validate(codecs: &[*const media_codec]) -> Result<(), String> {
    if codecs.len() > MAX_REAL {
        return Err(format!("more than {MAX_REAL} codecs"));
    }
    let mut found = [false; 2];
    for &c in codecs {
        // SAFETY: entries of a NULL-terminated list from the plugin.
        let c = unsafe { &*c };
        let (v, cid) = (c.vendor.vendor_id, c.vendor.codec_id);
        if c.codec_id == A2DP_CODEC_VENDOR && (v, cid) == (ids::VENDOR_ID, ids::CODEC_ID) {
            return Err("it's another copy of the aptX Adaptive plugin".into());
        }
        if c.name.is_null() {
            return Err("unexpected codec layout (entry without a name)".into());
        }
        // SAFETY: checked non-null; PipeWire codec names are static C strings.
        let name = unsafe { CStr::from_ptr(c.name) };
        if c.kind != MEDIA_CODEC_A2DP
            || c.codec_id != A2DP_CODEC_VENDOR
            || c.select_config.is_none()
            || c.enum_config.is_none()
            || c.init.is_none()
            || c.deinit.is_none()
        {
            return Err(format!(
                "unexpected codec layout ({} is kind {}, type {:#x})",
                name.to_string_lossy(),
                c.kind,
                c.codec_id
            ));
        }
        let want = match name.to_bytes() {
            b"aptx_hd" => Some((0, SPA_BLUETOOTH_AUDIO_CODEC_APTX_HD, 0xD7, 0x24)),
            b"aptx" => Some((1, SPA_BLUETOOTH_AUDIO_CODEC_APTX, 0x4F, 0x01)),
            _ => None,
        };
        if let Some((slot, id, vendor, codec)) = want {
            if c.id != id
                || (v, cid) != (vendor, codec)
                || c.fill_caps.is_none()
                || c.encode.is_none()
            {
                return Err(format!(
                    "unexpected codec layout ({} has id {}, vendor {v:#x}/{cid:#x})",
                    name.to_string_lossy(),
                    c.id
                ));
            }
            found[slot] = true;
        }
    }
    match found {
        [true, true] => Ok(()),
        [false, _] => Err("no aptx_hd codec".into()),
        [_, false] => Err("no aptx codec".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_follow_spa_plugin_dir() {
        let sys = Path::new("/sys/spa");
        assert_eq!(
            candidates(None, sys),
            [PathBuf::from("/sys/spa/bluez5/libspa-codec-bluez5-aptx.so")]
        );
        assert_eq!(
            candidates(Some(OsStr::new("/a::/b/")), sys),
            [
                PathBuf::from("/a/bluez5/libspa-codec-bluez5-aptx.so"),
                PathBuf::from("/b/bluez5/libspa-codec-bluez5-aptx.so"),
            ]
        );
        assert_eq!(candidates(Some(OsStr::new("")), sys), Vec::<PathBuf>::new());
    }

    #[test]
    fn validate_rejects_missing_or_wrong_entries() {
        assert_eq!(validate(&[]), Err("no aptx_hd codec".into()));
        let ours = crate::codec_for_test();
        assert!(validate(&[&raw const ours]).unwrap_err().contains("copy"));
        let mut hd = crate::codec_for_test();
        hd.name = c"aptx_hd".as_ptr();
        hd.vendor.codec_id = 0x24;
        assert!(validate(&[&raw const hd]).unwrap_err().contains("layout"));
        hd.id = SPA_BLUETOOTH_AUDIO_CODEC_APTX_HD;
        assert_eq!(validate(&[&raw const hd]), Err("no aptx codec".into()));
        let mut aptx = crate::codec_for_test();
        aptx.name = c"aptx".as_ptr();
        aptx.vendor.vendor_id = 0x4F;
        aptx.vendor.codec_id = 0x01;
        aptx.id = SPA_BLUETOOTH_AUDIO_CODEC_APTX;
        assert_eq!(validate(&[&raw const hd, &raw const aptx]), Ok(()));

        let mut other = crate::codec_for_test();
        other.name = c"aptx_ll".as_ptr();
        other.vendor.vendor_id = 0x0A;
        let check = |o: &media_codec| validate(&[&raw const hd, &raw const aptx, o]);
        assert_eq!(check(&other), Ok(()));
        other.kind = 1;
        assert!(check(&other).unwrap_err().contains("aptx_ll is kind 1"));
        other.kind = MEDIA_CODEC_A2DP;
        other.init = None;
        assert!(check(&other).unwrap_err().contains("layout"));
        other.name = ptr::null();
        assert!(check(&other).unwrap_err().contains("without a name"));
    }
}

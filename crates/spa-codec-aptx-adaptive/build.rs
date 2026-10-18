//! Bakes in PipeWire's plugin dir, where the `aptx` slot looks for the real aptX plugin
//! when `SPA_PLUGIN_DIR` is unset (ADR 0004).

use std::process::Command;

const FALLBACK: &str = "/usr/lib/spa-0.2";

fn main() {
    println!("cargo::rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo::rerun-if-env-changed=PKG_CONFIG_LIBDIR");
    let dir = Command::new("pkg-config")
        .args(["--variable=plugindir", "libspa-0.2"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| FALLBACK.to_owned());
    println!("cargo::rustc-env=APTX_ADAPTIVE_SYSTEM_PLUGINDIR={dir}");

    // The ported native-encoder codec links the same libaptx_native.so as the
    // C plugin. It is in /usr/lib here; APTX_NATIVE_LIB_DIR points the linker
    // at a build tree instead.
    println!("cargo::rerun-if-env-changed=APTX_NATIVE_LIB_DIR");
    if let Ok(dir) = std::env::var("APTX_NATIVE_LIB_DIR") {
        println!("cargo::rustc-link-search=native={dir}");
    }
}

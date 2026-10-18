//! Environment parsing shared by the ported plugin.
//!
//! The C plugin reads its transport experiments from the process environment;
//! these helpers keep the same `strtoul(..., 0)` spelling.

use std::ffi::OsString;

/// Parse like C `strtoul(s, NULL, 0)`: decimal, `0x` hex or leading-zero octal.
fn parse(value: &OsString) -> Option<u32> {
    let text = value.to_str()?.trim();
    if text.is_empty() {
        return None;
    }
    let (radix, digits) =
        if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            (16, hex)
        } else if text.len() > 1 && text.starts_with('0') {
            (8, &text[1..])
        } else {
            (10, text)
        };
    u32::from_str_radix(digits, radix).ok()
}

/// A numeric environment variable, or `None` when unset or malformed.
pub(crate) fn value_u32(name: &str) -> Option<u32> {
    std::env::var_os(name).as_ref().and_then(parse)
}

/// A numeric environment variable, or `default` when unset or malformed.
pub(crate) fn u32(name: &str, default: u32) -> u32 {
    value_u32(name).unwrap_or(default)
}

/// Whether a variable is present at all (the C plugin's `getenv() != NULL`).
pub(crate) fn is_set(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

/// Whether a variable is present and equal to `value`.
pub(crate) fn equals(name: &str, value: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v == value)
}

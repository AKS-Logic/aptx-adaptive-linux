use std::ffi::OsString;

use crate::ffi::{
    SPA_BLUETOOTH_AUDIO_CODEC_APTX, SPA_BLUETOOTH_AUDIO_CODEC_APTX_HD,
    SPA_BLUETOOTH_AUDIO_CODEC_LC3PLUS_HR, SPA_BLUETOOTH_AUDIO_CODEC_LDAC,
    SPA_BLUETOOTH_AUDIO_CODEC_MPEG,
};

pub const SLOT_VAR: &str = "APTX_ADAPTIVE_SLOT";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// Replaces PipeWire's aptX plugin file and passes its codecs through (ADR 0004).
    Aptx,
    Ldac,
    Lc3plusHr,
}

impl Slot {
    pub const DEFAULT: Self = Self::Aptx;

    /// The endpoint owner's id.
    pub const fn id(self) -> u32 {
        match self {
            Self::Aptx => SPA_BLUETOOTH_AUDIO_CODEC_APTX_HD,
            Self::Ldac => SPA_BLUETOOTH_AUDIO_CODEC_LDAC,
            Self::Lc3plusHr => SPA_BLUETOOTH_AUDIO_CODEC_LC3PLUS_HR,
        }
    }

    /// The alias entry's id.
    pub const fn alias_id(self) -> u32 {
        match self {
            Self::Aptx => SPA_BLUETOOTH_AUDIO_CODEC_APTX,
            Self::Ldac | Self::Lc3plusHr => SPA_BLUETOOTH_AUDIO_CODEC_MPEG,
        }
    }

    /// # Errors
    /// A message if the variable is set to anything but `aptx`, `ldac` or `lc3plus_hr`.
    pub fn from_env(var: impl Fn(&str) -> Option<OsString>) -> Result<Self, String> {
        match var(SLOT_VAR) {
            None => Ok(Self::DEFAULT),
            Some(v) => match v.to_str() {
                Some("aptx") => Ok(Self::Aptx),
                Some("ldac") => Ok(Self::Ldac),
                Some("lc3plus_hr") => Ok(Self::Lc3plusHr),
                _ => Err(format!(
                    "{SLOT_VAR}={} is not aptx, ldac or lc3plus_hr, using aptx",
                    v.display()
                )),
            },
        }
    }
}

/// Which factory PipeWire asked for. `codec-loader.c` asks each plugin file for the factory
/// named after the file, and we export both, so this says which file we were loaded as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Factory {
    Aptx,
    Lc3plus,
}

/// The slot to run: the aptX file is always the `aptx` slot; the LC3plus file runs `ldac` or
/// `lc3plus_hr`, and `ldac` when the variable is unset, as before the `aptx` slot existed.
///
/// # Errors
/// The slot to use anyway and a message, if the variable is bad or doesn't fit the file.
pub fn resolve(
    env: Result<Slot, String>,
    set: bool,
    factory: Factory,
) -> Result<Slot, (Slot, String)> {
    match (env, factory) {
        (Ok(Slot::Aptx), Factory::Aptx) => Ok(Slot::Aptx),
        (Ok(_), Factory::Aptx) if !set => Ok(Slot::Aptx),
        (Ok(s @ (Slot::Ldac | Slot::Lc3plusHr)), Factory::Lc3plus) => Ok(s),
        (Ok(_), Factory::Lc3plus) if !set => Ok(Slot::Ldac),
        (Ok(s), Factory::Aptx) => Err((
            Slot::Aptx,
            format!("{SLOT_VAR}={s:?} doesn't fit the aptX plugin file, using aptx"),
        )),
        (Ok(s), Factory::Lc3plus) => Err((
            Slot::Ldac,
            format!("{SLOT_VAR}={s:?} doesn't fit the LC3plus plugin file, using ldac"),
        )),
        (Err(e), Factory::Aptx) => Err((Slot::Aptx, format!("{e} (aptX plugin file: aptx)"))),
        (Err(_), Factory::Lc3plus) => Err((
            Slot::Ldac,
            format!("{SLOT_VAR} is not ldac or lc3plus_hr, using ldac (LC3plus plugin file)"),
        )),
    }
}

pub const DEFAULT_MODE_VAR: &str = "APTX_ADAPTIVE_DEFAULT_MODE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultMode {
    Hq,
    Ll,
}

impl DefaultMode {
    pub const DEFAULT: Self = Self::Hq;

    /// # Errors
    /// A message if the variable is set to anything but `hq` or `ll`.
    pub fn from_env(var: impl Fn(&str) -> Option<OsString>) -> Result<Self, String> {
        match var(DEFAULT_MODE_VAR) {
            None => Ok(Self::DEFAULT),
            Some(v) => match v.to_str() {
                Some("hq") => Ok(Self::Hq),
                Some("ll") => Ok(Self::Ll),
                _ => Err(format!(
                    "{DEFAULT_MODE_VAR}={} is not hq or ll, using hq",
                    v.display()
                )),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(v: Option<&str>) -> impl Fn(&str) -> Option<OsString> {
        move |k| {
            assert_eq!(k, SLOT_VAR);
            v.map(OsString::from)
        }
    }

    fn mode_env(v: Option<&str>) -> impl Fn(&str) -> Option<OsString> {
        move |k| {
            assert_eq!(k, DEFAULT_MODE_VAR);
            v.map(OsString::from)
        }
    }

    #[test]
    fn default_mode() {
        assert_eq!(DefaultMode::from_env(mode_env(None)), Ok(DefaultMode::Hq));
        assert_eq!(
            DefaultMode::from_env(mode_env(Some("hq"))),
            Ok(DefaultMode::Hq)
        );
        assert_eq!(
            DefaultMode::from_env(mode_env(Some("ll"))),
            Ok(DefaultMode::Ll)
        );
        for v in ["", "LL", "low", "1"] {
            assert!(DefaultMode::from_env(mode_env(Some(v))).is_err(), "{v:?}");
        }
    }

    #[test]
    fn defaults_to_aptx() {
        assert_eq!(Slot::from_env(env(None)), Ok(Slot::Aptx));
        assert_eq!(Slot::DEFAULT.id(), 7);
        assert_eq!(Slot::DEFAULT.alias_id(), 6);
    }

    #[test]
    fn parses_all_slots() {
        assert_eq!(Slot::from_env(env(Some("aptx"))), Ok(Slot::Aptx));
        assert_eq!(Slot::from_env(env(Some("ldac"))), Ok(Slot::Ldac));
        assert_eq!(Slot::from_env(env(Some("lc3plus_hr"))), Ok(Slot::Lc3plusHr));
        assert_eq!((Slot::Ldac.id(), Slot::Ldac.alias_id()), (8, 3));
        assert_eq!((Slot::Lc3plusHr.id(), Slot::Lc3plusHr.alias_id()), (13, 3));
    }

    #[test]
    fn slot_follows_the_factory() {
        let bad = || Err("bad".to_owned());
        assert_eq!(resolve(Ok(Slot::Aptx), true, Factory::Aptx), Ok(Slot::Aptx));
        assert_eq!(
            resolve(Ok(Slot::Aptx), false, Factory::Aptx),
            Ok(Slot::Aptx)
        );
        assert_eq!(
            resolve(Ok(Slot::Aptx), false, Factory::Lc3plus),
            Ok(Slot::Ldac)
        );
        for s in [Slot::Ldac, Slot::Lc3plusHr] {
            assert_eq!(resolve(Ok(s), true, Factory::Lc3plus), Ok(s));
            assert_eq!(
                resolve(Ok(s), true, Factory::Aptx).unwrap_err().0,
                Slot::Aptx
            );
        }
        assert_eq!(
            resolve(Ok(Slot::Aptx), true, Factory::Lc3plus)
                .unwrap_err()
                .0,
            Slot::Ldac
        );
        assert_eq!(
            resolve(bad(), true, Factory::Aptx).unwrap_err().0,
            Slot::Aptx
        );
        assert_eq!(
            resolve(bad(), true, Factory::Lc3plus).unwrap_err().0,
            Slot::Ldac
        );
    }

    #[test]
    fn rejects_anything_else() {
        for v in ["", "LDAC", "lc3plus", "aptx_hd", "APTX"] {
            assert!(Slot::from_env(env(Some(v))).is_err(), "{v:?}");
        }
    }
}

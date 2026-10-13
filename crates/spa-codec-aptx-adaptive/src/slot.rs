use std::ffi::OsString;

use crate::ffi::{SPA_BLUETOOTH_AUDIO_CODEC_LC3PLUS_HR, SPA_BLUETOOTH_AUDIO_CODEC_LDAC};

pub const SLOT_VAR: &str = "APTX_ADAPTIVE_SLOT";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Ldac,
    Lc3plusHr,
}

impl Slot {
    pub const DEFAULT: Self = Self::Ldac;

    pub const fn id(self) -> u32 {
        match self {
            Self::Ldac => SPA_BLUETOOTH_AUDIO_CODEC_LDAC,
            Self::Lc3plusHr => SPA_BLUETOOTH_AUDIO_CODEC_LC3PLUS_HR,
        }
    }

    /// # Errors
    /// A message if the variable is set to anything but `ldac` or `lc3plus_hr`.
    pub fn from_env(var: impl Fn(&str) -> Option<OsString>) -> Result<Self, String> {
        match var(SLOT_VAR) {
            None => Ok(Self::DEFAULT),
            Some(v) => match v.to_str() {
                Some("ldac") => Ok(Self::Ldac),
                Some("lc3plus_hr") => Ok(Self::Lc3plusHr),
                _ => Err(format!(
                    "{SLOT_VAR}={} is not ldac or lc3plus_hr, using ldac",
                    v.display()
                )),
            },
        }
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
    fn defaults_to_ldac() {
        assert_eq!(Slot::from_env(env(None)), Ok(Slot::Ldac));
        assert_eq!(Slot::DEFAULT.id(), 8);
    }

    #[test]
    fn parses_both_slots() {
        assert_eq!(Slot::from_env(env(Some("ldac"))), Ok(Slot::Ldac));
        assert_eq!(Slot::from_env(env(Some("lc3plus_hr"))), Ok(Slot::Lc3plusHr));
        assert_eq!(Slot::Lc3plusHr.id(), 13);
    }

    #[test]
    fn rejects_anything_else() {
        for v in ["", "LDAC", "lc3plus", "aptx_hd"] {
            assert!(Slot::from_env(env(Some(v))).is_err(), "{v:?}");
        }
    }
}

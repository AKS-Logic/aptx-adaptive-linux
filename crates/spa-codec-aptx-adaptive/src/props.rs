//! Exposes HQ or low-latency profile as `SPA_PROP_quality` (spec §2 "Profile").
//! PipeWire callbacks share state between main and data threads through atomics.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use aptx_adaptive::Profile;

use crate::ffi::{SPA_PARAM_PROP_INFO, SPA_PARAM_PROPS, SPA_PROP_QUALITY};
use crate::pod;

/// Device setting for the profile at stream start: `hq` (the default) or `ll`.
pub const MODE_SETTING: &str = "bluez5.a2dp.aptx-adaptive.mode";

const DESCRIPTION: &str = "aptX Adaptive mode";

/// `SPA_PROP_quality` values: the profile's [`Profile::index`].
pub const HQ: i32 = 0;
pub const LL: i32 = 1;

const LABELS: [(i32, &str); 2] = [(HQ, "HQ"), (LL, "Low latency")];

fn value(p: Profile) -> i32 {
    p.index() as i32
}

fn profile(v: i32) -> Option<Profile> {
    usize::try_from(v).ok().and_then(Profile::from_index)
}

fn from_u8(v: u8) -> Profile {
    Profile::from_index(v.into()).unwrap_or(Profile::Hq)
}

fn to_u8(p: Profile) -> u8 {
    p.index() as u8
}

#[derive(Debug)]
pub struct Props {
    /// From the device setting; what a reset (`set_props` with no param) goes back to.
    initial: Profile,
    /// What the user asked for. Main thread writes, data thread reads.
    wanted: AtomicU8,
    /// Profile reported as running or being switched to.
    running: AtomicU8,
    /// Whether the stream can offer LL. Data thread writes, main thread reads.
    ll_offered: AtomicBool,
}

impl Props {
    /// Starting in `initial`, which is also what a reset goes back to.
    pub fn new(initial: Profile) -> Self {
        Self {
            initial,
            wanted: AtomicU8::new(to_u8(initial)),
            running: AtomicU8::new(to_u8(initial)),
            ll_offered: AtomicBool::new(true),
        }
    }

    /// Parses the device setting, defaulting to HQ for unknown values.
    pub fn from_setting(mode: Option<&str>) -> (Self, Option<String>) {
        let (p, msg) = match mode.map(str::trim) {
            None | Some("hq") => (Profile::Hq, None),
            Some("ll") => (Profile::Ll, None),
            Some(v) => (
                Profile::Hq,
                Some(format!(
                    "aptx-adaptive: {MODE_SETTING}={v} is not hq or ll; using hq"
                )),
            ),
        };
        (Self::new(p), msg)
    }

    /// The profile asked for.
    pub fn wanted(&self) -> Profile {
        from_u8(self.wanted.load(Ordering::Acquire))
    }

    /// The profile reported: the one running, or being switched to.
    pub fn running(&self) -> Profile {
        from_u8(self.running.load(Ordering::Acquire))
    }

    /// Called by the stream (data thread) with what it's running and whether it can run LL.
    pub fn report(&self, running: Profile, ll_offered: bool) {
        self.ll_offered.store(ll_offered, Ordering::Release);
        self.running.store(to_u8(running), Ordering::Release);
    }

    /// The `PropInfo` or `Props` pod for `id` at `idx`: `Ok(None)` past the end, `Err` for an
    /// id we don't have. LL is only listed while it's offered.
    pub fn enum_param(&self, id: u32, idx: u32) -> Result<Option<Vec<u8>>, ()> {
        let current = value(self.running());
        let pod = match id {
            SPA_PARAM_PROP_INFO => {
                let labels = if self.ll_offered.load(Ordering::Acquire) {
                    &LABELS[..]
                } else {
                    &LABELS[..1]
                };
                pod::enum_prop_info(id, SPA_PROP_QUALITY, DESCRIPTION, current, labels)
            }
            SPA_PARAM_PROPS => pod::int_props(id, SPA_PROP_QUALITY, current),
            _ => return Err(()),
        };
        Ok((idx == 0).then_some(pod))
    }

    /// Applies a Props pod or resets to the device setting; returns whether the request changed.
    pub fn set(&self, param: Option<&[u8]>) -> bool {
        let want = match param {
            None => Some(self.initial),
            Some(p) => pod::props_int(p, SPA_PROP_QUALITY).and_then(profile),
        };
        let Some(mut want) = want else {
            return false;
        };
        if want == Profile::Ll && !self.ll_offered.load(Ordering::Acquire) {
            want = Profile::Hq;
        }
        self.running.store(to_u8(want), Ordering::Release);
        self.wanted.swap(to_u8(want), Ordering::AcqRel) != to_u8(want)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quality(v: i32) -> Vec<u8> {
        pod::int_props(SPA_PARAM_PROPS, SPA_PROP_QUALITY, v)
    }

    #[test]
    fn values_follow_profile_index() {
        for p in Profile::ALL {
            assert_eq!(profile(value(p)), Some(p));
            assert_eq!(from_u8(to_u8(p)), p);
        }
        assert_eq!((value(Profile::Hq), value(Profile::Ll)), (HQ, LL));
        assert_eq!(profile(-1), None);
        assert_eq!(profile(2), None);
    }

    #[test]
    fn setting() {
        assert_eq!(Props::from_setting(None).0.wanted(), Profile::Hq);
        assert_eq!(Props::from_setting(Some("hq")).0.wanted(), Profile::Hq);
        let (p, msg) = Props::from_setting(Some("ll"));
        assert_eq!(
            (p.wanted(), p.running(), msg),
            (Profile::Ll, Profile::Ll, None)
        );
        let (p, msg) = Props::from_setting(Some("fast"));
        assert_eq!(p.wanted(), Profile::Hq);
        assert!(msg.unwrap().contains("fast"));
    }

    /// The low-latency profile's props: LL from the start, a reset goes back to LL, and HQ is
    /// still one `set` away.
    #[test]
    fn starting_in_ll() {
        let p = Props::new(Profile::Ll);
        assert_eq!((p.wanted(), p.running()), (Profile::Ll, Profile::Ll));
        assert_eq!(
            p.enum_param(SPA_PARAM_PROPS, 0).unwrap().unwrap(),
            quality(LL)
        );
        assert!(p.set(Some(&quality(HQ))));
        assert_eq!(p.wanted(), Profile::Hq);
        assert!(p.set(None));
        assert_eq!(p.wanted(), Profile::Ll);
    }

    #[test]
    fn set_and_reset() {
        let (p, _) = Props::from_setting(Some("ll"));
        assert!(p.set(Some(&quality(HQ))));
        assert_eq!(p.wanted(), Profile::Hq);
        assert!(!p.set(Some(&quality(HQ))));
        assert!(!p.set(Some(&quality(7))));
        assert!(!p.set(Some(&pod::int_props(SPA_PARAM_PROPS, 0x1_000e, LL))));
        assert!(p.set(None));
        assert_eq!(p.wanted(), Profile::Ll);
    }

    /// After a refused switch the stream reports what it runs, and Props shows that.
    #[test]
    fn reports_running_profile() {
        let (p, _) = Props::from_setting(None);
        assert!(p.set(Some(&quality(LL))));
        assert_eq!(p.running(), Profile::Ll);
        p.report(Profile::Hq, false);
        assert_eq!(
            p.enum_param(SPA_PARAM_PROPS, 0).unwrap().unwrap(),
            quality(HQ)
        );
        let info = p.enum_param(SPA_PARAM_PROP_INFO, 0).unwrap().unwrap();
        let only_hq = pod::enum_prop_info(
            SPA_PARAM_PROP_INFO,
            SPA_PROP_QUALITY,
            DESCRIPTION,
            HQ,
            &LABELS[..1],
        );
        assert_eq!(info, only_hq);
        assert!(p.set(Some(&quality(LL))));
        assert_eq!((p.wanted(), p.running()), (Profile::Hq, Profile::Hq));
        assert!(!p.set(Some(&quality(LL))));
    }

    #[test]
    fn enum_params() {
        let (p, _) = Props::from_setting(None);
        assert!(p.enum_param(SPA_PARAM_PROP_INFO, 0).unwrap().is_some());
        assert_eq!(p.enum_param(SPA_PARAM_PROP_INFO, 1), Ok(None));
        assert_eq!(
            p.enum_param(SPA_PARAM_PROPS, 0).unwrap().unwrap(),
            quality(HQ)
        );
        assert_eq!(p.enum_param(3, 0), Err(()));
    }
}

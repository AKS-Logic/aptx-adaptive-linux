//! Clock drift instrumentation and the optional §7F TTP adjustment.
//!
//! With source type 2 the sink advances its time-to-play purely by the sample count in each
//! packet (spec §7F). If the host audio clock that paces our encoding runs faster or slower than
//! the sink's, the sink's buffered latency creeps until it leaves its window and resets, an
//! audible glitch. This module measures how fast audio is sent against `CLOCK_MONOTONIC`
//! ([`Instant`] on Linux) and the socket backlog PipeWire reports through `abr_process`, and logs
//! a line to stderr every [`REPORT_INTERVAL`] (in release builds too, for diagnosing stutter;
//! `APTX_ADAPTIVE_DRIFT_LOG=0` silences it). The sink's Delay Report isn't visible to a
//! `media_codec` (only `media-sink.c` sees the transport), so it isn't used.
//!
//! The timestamp's high 16 bits are a signed latency adjustment in ms (§7F). Per §7G a source
//! shouldn't correct drift: with a proper target latency the sink does it itself, and a non-zero
//! adjust changes the target and makes the sink speed up or slow down. So the adjust is
//! [`TtpAdjust::Off`] (0) unless a `dev-replay` build sets
//! `APTX_ADAPTIVE_TTP_ADJUST=off|fixed:<ms>|auto`.

use std::ffi::OsStr;
use std::fmt::{self, Write as _};
use std::io::Write as _;
use std::time::{Duration, Instant};

pub const TTP_ADJUST_VAR: &str = "APTX_ADAPTIVE_TTP_ADJUST";
/// `APTX_ADAPTIVE_DRIFT_LOG=0` silences the drift line, in every build.
pub const DRIFT_LOG_VAR: &str = "APTX_ADAPTIVE_DRIFT_LOG";

/// How often a drift line is logged and the auto controller steps.
pub const REPORT_INTERVAL: Duration = Duration::from_secs(10);

/// A pause in sending longer than this restarts the measurement (and the auto adjust).
const GAP: Duration = Duration::from_secs(1);

/// The §7F timestamp: `adjust_ms << 16 | samples per channel in the packet`.
pub fn ttp_timestamp(adjust_ms: i16, frames: u16) -> u32 {
    (u32::from(adjust_ms.cast_unsigned()) << 16) | u32::from(frames)
}

/// What goes in the timestamp's high 16 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TtpAdjust {
    /// 0, the default. Keep it: a source shouldn't correct drift (§7G).
    #[default]
    Off,
    /// DEV ONLY. A constant, in signed ms.
    Fixed(i16),
    /// DEV ONLY. Follows the measured drift, see [`Auto`].
    Auto,
}

impl TtpAdjust {
    /// Parses `off`, `fixed:<signed ms>` or `auto`.
    ///
    /// # Errors
    /// A message for anything else.
    pub fn parse(v: &OsStr) -> Result<Self, String> {
        let bad = || {
            format!(
                "{TTP_ADJUST_VAR}={} is not off, fixed:<ms> or auto",
                v.display()
            )
        };
        match v.to_str().ok_or_else(bad)? {
            "off" => Ok(Self::Off),
            "auto" => Ok(Self::Auto),
            s => s
                .strip_prefix("fixed:")
                .and_then(|n| n.parse::<i16>().ok())
                .map(Self::Fixed)
                .ok_or_else(bad),
        }
    }
}

/// The auto TTP adjust controller, stepped once per [`REPORT_INTERVAL`].
///
/// Input: `drift_ms`, how far the audio we've sent has run ahead of `CLOCK_MONOTONIC` since the
/// measurement settled (positive = host audio clock fast). Assuming the sink's clock tracks
/// `CLOCK_MONOTONIC` to well under that, it is how much the sink's buffered latency has grown,
/// so the adjust that re-centres it is `-drift_ms`: a fast source gets a negative adjust.
/// UNCERTAIN: how the sink reacts to a change is still open (`spec/QUESTIONS.md`).
///
/// Algorithm: the target is `-drift_ms`. Each step moves the
/// adjust by at most 1 ms towards the target, and only if the target is more than
/// [`Auto::DEADBAND_MS`] away. So stepping from `a` to `a + 1` needs a target above `a + 1.5`,
/// and stepping back needs one below `a - 0.5`: jitter under 2 ms around a boundary can't make
/// it flap. At most 1 ms per report, so at most ±1 ms per 10 s, and the adjust is clamped to
/// ±[`Auto::LIMIT_MS`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Auto {
    adjust_ms: i16,
}

impl Auto {
    pub const LIMIT_MS: i16 = 50;
    pub const DEADBAND_MS: f64 = 1.5;

    pub fn adjust_ms(self) -> i16 {
        self.adjust_ms
    }

    pub fn step(&mut self, drift_ms: f64) -> i16 {
        let target = if drift_ms.is_nan() { 0.0 } else { -drift_ms };
        let err = target - f64::from(self.adjust_ms);
        if err > Self::DEADBAND_MS {
            self.adjust_ms += 1;
        } else if err < -Self::DEADBAND_MS {
            self.adjust_ms -= 1;
        }
        self.adjust_ms = self.adjust_ms.clamp(-Self::LIMIT_MS, Self::LIMIT_MS);
        self.adjust_ms
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Backlog {
    min: usize,
    max: usize,
    sum: u64,
    n: u64,
}

impl Backlog {
    fn add(&mut self, unsent: usize) {
        if self.n == 0 {
            self.min = unsent;
            self.max = unsent;
        } else {
            self.min = self.min.min(unsent);
            self.max = self.max.max(unsent);
        }
        self.sum += unsent as u64;
        self.n += 1;
    }
}

/// One [`REPORT_INTERVAL`]'s summary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Report {
    /// Host audio clock against `CLOCK_MONOTONIC`, `None` in the first (settling) interval.
    pub ppm: Option<f64>,
    /// Audio sent ahead of `CLOCK_MONOTONIC` since the first interval, in ms.
    pub drift_ms: Option<f64>,
    /// min/avg/max of `abr_process(unsent)`, in bytes. Which quantity this is depends on
    /// `media-sink.c`: with TX timestamping it's payload sent but not yet completed by the
    /// controller; without, it's `TIOCOUTQ`, socket memory in use (kernel `skb` accounting, so
    /// larger than the payload). The plugin can't tell which, hence "PipeWire unsent".
    pub backlog: (usize, usize, usize),
    /// The adjust in use, `None` when it's off.
    pub adjust_ms: Option<i16>,
    /// HQ/LL switch requests refused since the stream started (logged when non-zero; the
    /// data thread doesn't log them itself).
    pub refused_switches: u32,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("aptx-adaptive: drift: host clock ")?;
        match self.ppm {
            Some(p) => write!(f, "{p:+.1} ppm vs monotonic")?,
            None => f.write_str("settling")?,
        }
        let (min, avg, max) = self.backlog;
        write!(
            f,
            ", backlog (PipeWire unsent) {min}/{avg}/{max} bytes (min/avg/max)"
        )?;
        if let Some(d) = self.drift_ms {
            write!(f, ", audio ahead {d:+.1} ms")?;
        }
        if let Some(a) = self.adjust_ms {
            write!(f, ", ttp adjust {a:+} ms")?;
        }
        if self.refused_switches > 0 {
            write!(f, ", refused mode switches {}", self.refused_switches)?;
        }
        Ok(())
    }
}

/// Measures the host audio clock and backlog, and owns the TTP adjust.
///
/// Fed from `abr_process`, which `media-sink.c` calls in `send_buffer` right before each
/// `send()`, so the clock is read at the moment of each send (what a btmon capture measures).
/// At each send, *ahead* = audio sent before this packet − time since the first send. The flush
/// timer paces sends by the graph clock, plus wakeup jitter, so *ahead* is noisy by a few
/// 100 µs; its minimum over an interval is steady to about one wakeup's jitter. The first
/// interval's minimum is the baseline (absorbing start-up bursts); after that `drift_ms` = this
/// interval's minimum − baseline and ppm = `drift_ms` / time since the first report. Positive =
/// the host audio clock runs fast, i.e. we send more audio than real time.
#[derive(Debug, Clone)]
pub struct Drift {
    rate: u32,
    mode: TtpAdjust,
    auto: Auto,
    anchor: Option<Instant>,
    last: Option<Instant>,
    frames: u64,
    next_report: Option<Instant>,
    /// Time of the first report and the minimum *ahead* (s) in its interval.
    baseline: Option<(Instant, f64)>,
    min_ahead: f64,
    backlog: Backlog,
}

impl Drift {
    pub fn new(rate: u32, mode: TtpAdjust) -> Self {
        Self {
            rate,
            mode,
            auto: Auto::default(),
            anchor: None,
            last: None,
            frames: 0,
            next_report: None,
            baseline: None,
            min_ahead: f64::INFINITY,
            backlog: Backlog::default(),
        }
    }

    /// The §7F adjust for the next packet, in ms.
    pub fn adjust_ms(&self) -> i16 {
        match self.mode {
            TtpAdjust::Off => 0,
            TtpAdjust::Fixed(ms) => ms,
            TtpAdjust::Auto => self.auto.adjust_ms(),
        }
    }

    fn restart(&mut self, now: Instant) {
        *self = Self::new(self.rate, self.mode);
        self.anchor = Some(now);
        self.next_report = Some(now + REPORT_INTERVAL);
    }

    /// A packet of `frames` (per channel) is about to be sent at `now` with `unsent` bytes
    /// already in the socket. Returns a report once per interval.
    pub fn on_send(&mut self, now: Instant, unsent: usize, frames: u64) -> Option<Report> {
        if self
            .last
            .is_none_or(|last| now.saturating_duration_since(last) > GAP)
        {
            self.restart(now);
        }
        self.last = Some(now);
        self.backlog.add(unsent);
        let anchor = self.anchor?;
        #[expect(
            clippy::cast_precision_loss,
            reason = "exact to well past a year of frames at 48 kHz"
        )]
        let ahead = self.frames as f64 / f64::from(self.rate) - (now - anchor).as_secs_f64();
        self.min_ahead = self.min_ahead.min(ahead);
        self.frames += frames;
        let next = self.next_report?;
        if now < next {
            return None;
        }
        self.next_report = Some(next + REPORT_INTERVAL);
        let (ppm, drift_ms) = match self.baseline {
            None => {
                self.baseline = Some((now, self.min_ahead));
                (None, None)
            }
            Some((t0, base)) => {
                let drift = self.min_ahead - base;
                let span = (now - t0).as_secs_f64();
                (Some(drift / span * 1e6), Some(drift * 1e3))
            }
        };
        self.min_ahead = f64::INFINITY;
        if let (TtpAdjust::Auto, Some(d)) = (self.mode, drift_ms) {
            self.auto.step(d);
        }
        let b = std::mem::take(&mut self.backlog);
        Some(Report {
            ppm,
            drift_ms,
            backlog: (b.min, (b.sum / b.n.max(1)) as usize, b.max),
            adjust_ms: (self.mode != TtpAdjust::Off).then(|| self.adjust_ms()),
            refused_switches: 0,
        })
    }

    /// [`Self::on_send`], with the report (plus `refused` mode switches) written to stderr
    /// (the service's journal) unless `log` is off.
    pub fn on_send_log(
        &mut self,
        now: Instant,
        unsent: usize,
        frames: u64,
        log: bool,
        refused: u32,
    ) {
        if let Some(mut r) = self.on_send(now, unsent, frames)
            && log
        {
            r.refused_switches = refused;
            let mut line = LineBuf::new();
            let _ = writeln!(line, "{r}");
            // One write(2), so the line can't interleave with other threads' output. Unbuffered
            // stderr, no allocation; a short write loses the tail, which is fine for a log.
            let _ = std::io::stderr().write(line.as_bytes());
        }
    }
}

/// A fixed stack buffer for one log line; output past its end is dropped.
struct LineBuf {
    buf: [u8; 320],
    len: usize,
}

impl LineBuf {
    fn new() -> Self {
        Self {
            buf: [0; 320],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl fmt::Write for LineBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        if n < s.len() { Err(fmt::Error) } else { Ok(()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_with_adjust() {
        assert_eq!(ttp_timestamp(0, 672), 0x0000_02a0);
        let ts = ttp_timestamp(-3, 672);
        assert_eq!(ts >> 16, 0xfffd);
        assert_eq!(ts & 0xffff, 672);
        assert_eq!(ts.to_be_bytes(), [0xff, 0xfd, 0x02, 0xa0]);
        assert_eq!(ttp_timestamp(50, 672), 0x0032_02a0);
        assert_eq!(ttp_timestamp(i16::MIN, 0), 0x8000_0000);
    }

    #[test]
    fn parses_mode() {
        let p = |s: &str| TtpAdjust::parse(OsStr::new(s));
        assert_eq!(p("off"), Ok(TtpAdjust::Off));
        assert_eq!(p("auto"), Ok(TtpAdjust::Auto));
        assert_eq!(p("fixed:-3"), Ok(TtpAdjust::Fixed(-3)));
        assert_eq!(p("fixed:+12"), Ok(TtpAdjust::Fixed(12)));
        for bad in ["", "on", "fixed:", "fixed:x", "fixed:40000", "-3"] {
            assert!(p(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn auto_steps_one_ms_against_drift() {
        let mut a = Auto::default();
        // Source fast: sink latency grows, adjust goes negative.
        assert_eq!(a.step(10.0), -1);
        assert_eq!(a.step(10.0), -2);
        assert_eq!(a.step(-10.0), -1);
        assert_eq!(a.step(-10.0), 0);
        assert_eq!(a.step(-10.0), 1);
    }

    #[test]
    fn auto_clamps() {
        let mut a = Auto::default();
        for _ in 0..200 {
            a.step(1000.0);
        }
        assert_eq!(a.adjust_ms(), -Auto::LIMIT_MS);
        for _ in 0..200 {
            a.step(f64::NEG_INFINITY);
        }
        assert_eq!(a.adjust_ms(), Auto::LIMIT_MS);
        a.step(f64::NAN);
        assert_eq!(a.adjust_ms(), Auto::LIMIT_MS - 1);
    }

    #[test]
    fn auto_hysteresis() {
        let mut a = Auto::default();
        // Inside the deadband: hold.
        for d in [1.4, -1.4, 0.9, -0.5] {
            assert_eq!(a.step(d), 0);
        }
        assert_eq!(a.step(1.6), -1);
        // Jitter around the new step doesn't bring it back...
        for d in [0.0, 1.2, -0.4, 2.4, 0.6] {
            assert_eq!(a.step(d), -1, "{d}");
        }
        // ...a real swing does.
        assert_eq!(a.step(-0.6), 0);
        assert_eq!(a.step(2.6), -1);
    }

    /// Sends a 672-frame packet every `period` for `secs`, like media-sink's flush timer.
    fn run(d: &mut Drift, t0: Instant, period: Duration, secs: u64) -> Vec<Report> {
        let n = Duration::from_secs(secs).as_nanos() / period.as_nanos();
        (0..n)
            .filter_map(|i| d.on_send(t0 + period * i as u32, 656 * (i as usize % 3), 672))
            .collect()
    }

    #[test]
    fn exact_clock_is_zero_ppm() {
        let mut d = Drift::new(48_000, TtpAdjust::Off);
        let r = run(&mut d, Instant::now(), Duration::from_millis(14), 31);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].ppm, None);
        let (min, avg, max) = r[0].backlog;
        assert_eq!((min, max), (0, 1312));
        assert!((650..=660).contains(&avg), "{avg}");
        assert_eq!(r[0].adjust_ms, None);
        for x in &r[1..] {
            assert!(x.ppm.unwrap().abs() < 0.1, "{x:?}");
        }
        assert_eq!(d.adjust_ms(), 0);
    }

    /// The capture's case: the host sends audio 80 ppm faster than real time, so packets of
    /// 14 ms of audio go out every 14 ms / 1.00008.
    #[test]
    fn fast_host_clock_measures_and_auto_goes_negative() {
        let period = Duration::from_nanos((14_000_000.0 / 1.000_08_f64).round() as u64);
        let mut d = Drift::new(48_000, TtpAdjust::Auto);
        let r = run(&mut d, Instant::now(), period, 101);
        assert_eq!(r.len(), 10);
        let last = r.last().unwrap();
        assert!((last.ppm.unwrap() - 80.0).abs() < 2.0, "{last:?}");
        assert!(last.to_string().contains("host clock +80."), "{last}");
        // 90 s at 80 ppm = 7.2 ms; the controller follows at 1 ms per report, from the
        // second report on, and holds within the deadband.
        assert!((last.drift_ms.unwrap() - 7.2).abs() < 0.3, "{last:?}");
        let adj: Vec<i16> = r.iter().map(|x| x.adjust_ms.unwrap()).collect();
        assert!(
            adj.windows(2).all(|w| w[1] <= w[0] && w[0] - w[1] <= 1),
            "{adj:?}"
        );
        assert!((-7..=-4).contains(&adj[9]), "{adj:?}");
        assert_eq!(d.adjust_ms(), adj[9]);
    }

    /// Sends jittered by up to ±1 ms (timer wakeups) don't bias the estimate.
    #[test]
    fn jitter_doesnt_bias() {
        let mut d = Drift::new(48_000, TtpAdjust::Off);
        let t0 = Instant::now() + Duration::from_millis(5);
        let mut reports = Vec::new();
        for i in 0..(41_000 / 14) {
            let jitter = Duration::from_micros(i * 7919 % 1000);
            let t = t0 + Duration::from_millis(14 * i) + jitter;
            reports.extend(d.on_send(t, 0, 672));
        }
        for r in &reports[1..] {
            assert!(r.ppm.unwrap().abs() < 100.0, "{r:?}");
        }
    }

    #[test]
    fn gap_restarts() {
        let mut d = Drift::new(48_000, TtpAdjust::Auto);
        let t0 = Instant::now();
        let period = Duration::from_millis(13);
        run(&mut d, t0, period, 60);
        assert_ne!(d.adjust_ms(), 0);
        let r = run(&mut d, t0 + Duration::from_secs(70), period, 11);
        assert_eq!(d.adjust_ms(), 0);
        assert_eq!(r[0].ppm, None);
    }

    #[test]
    fn fixed_adjust() {
        let d = Drift::new(48_000, TtpAdjust::Fixed(-3));
        assert_eq!(d.adjust_ms(), -3);
    }

    #[test]
    fn report_line() {
        let r = Report {
            ppm: Some(-12.34),
            drift_ms: Some(-0.5),
            backlog: (0, 312, 1312),
            adjust_ms: None,
            refused_switches: 0,
        };
        assert_eq!(
            r.to_string(),
            "aptx-adaptive: drift: host clock -12.3 ppm vs monotonic, backlog (PipeWire unsent) 0/312/1312 bytes (min/avg/max), audio ahead -0.5 ms"
        );
        let r = Report {
            ppm: None,
            drift_ms: None,
            backlog: (0, 0, 0),
            adjust_ms: Some(2),
            refused_switches: 0,
        };
        assert_eq!(
            r.to_string(),
            "aptx-adaptive: drift: host clock settling, backlog (PipeWire unsent) 0/0/0 bytes (min/avg/max), ttp adjust +2 ms"
        );
        let r = Report {
            refused_switches: 3,
            ..r
        };
        assert!(r.to_string().ends_with(", refused mode switches 3"));
        // The longest line fits the stack buffer.
        let r = Report {
            ppm: Some(-1e9),
            drift_ms: Some(-1e9),
            backlog: (usize::MAX, usize::MAX, usize::MAX),
            adjust_ms: Some(i16::MIN),
            refused_switches: u32::MAX,
        };
        let mut line = LineBuf::new();
        assert!(writeln!(line, "{r}").is_ok());
        assert_eq!(line.as_bytes(), format!("{r}\n").as_bytes());
    }

    #[test]
    fn line_buf_truncates() {
        let mut line = LineBuf::new();
        assert!(line.write_str(&"x".repeat(400)).is_err());
        assert_eq!(line.as_bytes().len(), 320);
    }
}

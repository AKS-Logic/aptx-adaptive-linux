use std::fmt::{self, Write as _};
use std::io::Write as _;
use std::time::{Duration, Instant};

/// `APTX_ADAPTIVE_DRIFT_LOG=0` silences the drift line, in every build.
pub const DRIFT_LOG_VAR: &str = "APTX_ADAPTIVE_DRIFT_LOG";

pub const REPORT_INTERVAL: Duration = Duration::from_secs(10);

const GAP: Duration = Duration::from_secs(1);

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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Report {
    /// Host audio clock drift against `CLOCK_MONOTONIC`; absent in the settling interval.
    pub ppm: Option<f64>,
    /// Audio sent ahead of `CLOCK_MONOTONIC`, in ms.
    pub drift_ms: Option<f64>,
    pub backlog: (usize, usize, usize),
    pub refused_switches: u32,
    /// The quality level running when the line is logged (§7L), if known.
    pub level: Option<u8>,
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
        if let Some(l) = self.level {
            write!(f, ", level {l}")?;
        }
        if self.refused_switches > 0 {
            write!(f, ", refused mode switches {}", self.refused_switches)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Drift {
    rate: u32,
    anchor: Option<Instant>,
    last: Option<Instant>,
    frames: u64,
    next_report: Option<Instant>,
    baseline: Option<(Instant, f64)>,
    min_ahead: f64,
    backlog: Backlog,
}

impl Drift {
    pub fn new(rate: u32) -> Self {
        Self {
            rate,
            anchor: None,
            last: None,
            frames: 0,
            next_report: None,
            baseline: None,
            min_ahead: f64::INFINITY,
            backlog: Backlog::default(),
        }
    }

    fn restart(&mut self, now: Instant) {
        *self = Self::new(self.rate);
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
        let b = std::mem::take(&mut self.backlog);
        Some(Report {
            ppm,
            drift_ms,
            backlog: (b.min, (b.sum / b.n.max(1)) as usize, b.max),
            refused_switches: 0,
            level: None,
        })
    }

    /// [`Self::on_send`], with the report (plus `refused` mode switches and the running `level`)
    /// written to stderr (the service's journal) unless `log` is off.
    pub fn on_send_log(
        &mut self,
        now: Instant,
        unsent: usize,
        frames: u64,
        log: bool,
        refused: u32,
        level: u8,
    ) {
        if let Some(mut r) = self.on_send(now, unsent, frames)
            && log
        {
            r.refused_switches = refused;
            r.level = Some(level);
            let mut line = LineBuf::new();
            let _ = writeln!(line, "{r}");
            let _ = std::io::stderr().write(line.as_bytes());
        }
    }
}

pub struct LineBuf {
    buf: [u8; 320],
    len: usize,
}

impl LineBuf {
    pub fn new() -> Self {
        Self {
            buf: [0; 320],
            len: 0,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
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

    fn run(d: &mut Drift, t0: Instant, period: Duration, secs: u64) -> Vec<Report> {
        let n = Duration::from_secs(secs).as_nanos() / period.as_nanos();
        (0..n)
            .filter_map(|i| d.on_send(t0 + period * i as u32, 656 * (i as usize % 3), 672))
            .collect()
    }

    #[test]
    fn exact_clock_is_zero_ppm() {
        let mut d = Drift::new(48_000);
        let r = run(&mut d, Instant::now(), Duration::from_millis(14), 31);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].ppm, None);
        let (min, avg, max) = r[0].backlog;
        assert_eq!((min, max), (0, 1312));
        assert!((650..=660).contains(&avg), "{avg}");
        for x in &r[1..] {
            assert!(x.ppm.unwrap().abs() < 0.1, "{x:?}");
        }
    }

    #[test]
    fn fast_host_clock_measures() {
        let period = Duration::from_nanos((14_000_000.0 / 1.000_08_f64).round() as u64);
        let mut d = Drift::new(48_000);
        let r = run(&mut d, Instant::now(), period, 101);
        assert_eq!(r.len(), 10);
        let last = r.last().unwrap();
        assert!((last.ppm.unwrap() - 80.0).abs() < 2.0, "{last:?}");
        assert!(last.to_string().contains("host clock +80."), "{last}");
        assert!((last.drift_ms.unwrap() - 7.2).abs() < 0.3, "{last:?}");
    }

    #[test]
    fn jitter_doesnt_bias() {
        let mut d = Drift::new(48_000);
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
        let mut d = Drift::new(48_000);
        let t0 = Instant::now();
        let period = Duration::from_millis(13);
        let r = run(&mut d, t0, period, 60);
        assert!(r.last().unwrap().ppm.is_some());
        let r = run(&mut d, t0 + Duration::from_secs(70), period, 11);
        assert_eq!(r[0].ppm, None);
    }

    #[test]
    fn report_line() {
        let r = Report {
            ppm: Some(-12.34),
            drift_ms: Some(-0.5),
            backlog: (0, 312, 1312),
            refused_switches: 0,
            level: None,
        };
        assert_eq!(
            r.to_string(),
            "aptx-adaptive: drift: host clock -12.3 ppm vs monotonic, backlog (PipeWire unsent) 0/312/1312 bytes (min/avg/max), audio ahead -0.5 ms"
        );
        let r = Report {
            ppm: None,
            drift_ms: None,
            backlog: (0, 0, 0),
            refused_switches: 0,
            level: None,
        };
        assert_eq!(
            r.to_string(),
            "aptx-adaptive: drift: host clock settling, backlog (PipeWire unsent) 0/0/0 bytes (min/avg/max)"
        );
        let r = Report {
            refused_switches: 3,
            ..r
        };
        assert!(r.to_string().ends_with(", refused mode switches 3"));
        let r = Report {
            ppm: Some(-1e9),
            drift_ms: Some(-1e9),
            backlog: (usize::MAX, usize::MAX, usize::MAX),
            refused_switches: u32::MAX,
            level: Some(u8::MAX),
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

//! Strictly increasing wall-clock milliseconds, plus a dependency-free UTC
//! formatter used in handoff draft text.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Wall clock that never returns the same value twice within one process, so
/// "most recently updated" ordering stays stable even inside a single
/// millisecond.
#[derive(Debug, Default)]
pub struct MonotonicClock {
    last: AtomicI64,
}

impl MonotonicClock {
    pub fn new() -> Self {
        Self {
            last: AtomicI64::new(0),
        }
    }

    pub fn now_ms(&self) -> i64 {
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        loop {
            let prev = self.last.load(Ordering::SeqCst);
            let next = if wall > prev { wall } else { prev + 1 };
            if self
                .last
                .compare_exchange(prev, next, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return next;
            }
        }
    }
}

/// `2026-09-08T09:14:03Z` from epoch milliseconds. Days-from-civil, proleptic
/// Gregorian; UTC only, no local-time or leap-second claims.
pub fn format_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if mth <= 2 { y + 1 } else { y };

    format!("{year:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_757_322_309_000), "2025-09-08T09:05:09Z");
        assert_eq!(format_utc(1_788_858_309_000), "2026-09-08T09:05:09Z");
    }

    #[test]
    fn clock_is_strictly_increasing() {
        let c = MonotonicClock::new();
        let mut prev = c.now_ms();
        for _ in 0..1000 {
            let next = c.now_ms();
            assert!(next > prev, "{next} must exceed {prev}");
            prev = next;
        }
    }
}

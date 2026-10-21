//! The data clock (ADR-0048): the time data stores and memory stores read
//! for `CreatedTime`, `UpdatedTime` and expiry.

use re_core::time::{SystemTime, UNIX_EPOCH};

/// `2026-01-01T00:00:00Z`, the default start of a fixed clock, in Unix
/// milliseconds.
pub const DEFAULT_EPOCH_MS: i64 = 1_767_225_600_000;

/// Where the data clock's time comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    /// Wall-clock time (`real run`).
    Wall,
    /// A fixed start plus the simulation time, so runs repeat (`real verify`,
    /// replays, `play.run` and tests).
    Fixed {
        /// The start, in Unix milliseconds.
        epoch_ms: i64,
    },
}

impl Default for Clock {
    fn default() -> Clock {
        Clock::Fixed {
            epoch_ms: DEFAULT_EPOCH_MS,
        }
    }
}

impl Clock {
    /// The time in Unix milliseconds, `sim_secs` seconds of simulation into
    /// the run.
    pub fn now_ms(self, sim_secs: f64) -> i64 {
        match self {
            Clock::Wall => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            Clock::Fixed { epoch_ms } => {
                let secs = if sim_secs.is_finite() {
                    sim_secs.max(0.0)
                } else {
                    0.0
                };
                epoch_ms + (secs * 1000.0).floor() as i64
            }
        }
    }
}

/// Reads `YYYY-MM-DDTHH:MM:SSZ` (UTC, the only form `[compat.data] epoch`
/// takes) as Unix milliseconds.
pub fn parse_epoch(text: &str) -> Option<i64> {
    let b = text.trim().as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' {
        return None;
    }
    if b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let s = std::str::from_utf8(&b[r]).ok()?;
        if !s.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, s) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) || h > 23 || mi > 59 || s > 59 {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    Some(((days * 24 + h) * 60 + mi) * 60_000 + s * 1000)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epochs_parse_as_utc() {
        assert_eq!(parse_epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_epoch("2026-01-01T00:00:00Z"), Some(DEFAULT_EPOCH_MS));
        assert_eq!(parse_epoch("2000-02-29T12:30:05Z"), Some(951_827_405_000));
        assert_eq!(parse_epoch("2001-02-29T00:00:00Z"), None);
        assert_eq!(parse_epoch("2026-01-01 00:00:00Z"), None);
        assert_eq!(parse_epoch("2026-01-01T00:00:00"), None);
        assert_eq!(parse_epoch("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn a_fixed_clock_follows_simulation_time() {
        let c = Clock::Fixed { epoch_ms: 1000 };
        assert_eq!(c.now_ms(0.0), 1000);
        assert_eq!(c.now_ms(1.5), 2500);
        assert_eq!(c.now_ms(f64::NAN), 1000);
    }
}

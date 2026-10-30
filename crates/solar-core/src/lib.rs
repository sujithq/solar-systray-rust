//! Solar surplus logic ported from madebyq/solar-agent (MIT).
//!
//! The modules mirror `daemon/src/solar/*.mjs` and are checked against the same shared
//! fixtures, so the tray shows exactly what the Solar Agent device and desktop show.

pub mod agent;
pub mod config;
pub mod daily_energy;
pub mod decision;
pub mod dsmr;
pub mod mock;
pub mod night_baseline;
pub mod peak_forecast;
pub mod present;
pub mod reading;
pub mod runtime;
pub mod service;
pub mod simulation;
pub mod slimmelezer;
pub mod sun;
pub mod sungrow;

pub use agent::{AgentData, AgentLink};
pub use config::SolarConfig;
pub use reading::Reading;
pub use runtime::{Runtime, Snapshot};
pub use service::SolarService;

/// JavaScript `Math.round`: halves round up, towards positive infinity.
pub fn js_round(value: f64) -> f64 {
    let floor = value.floor();
    if value - floor >= 0.5 { floor + 1.0 } else { floor }
}

/// Milliseconds since the Unix epoch, like `Date.now()`.
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The calendar (year, month, day) of a day count from 1970-01-01.
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `Date.UTC(year, month - 1, day, hour, minute, second)` in milliseconds.
pub fn utc_ms(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> i64 {
    (days_from_civil(year, month, day) * 86400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64) * 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_round_matches_math_round() {
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(-2.6), -3.0);
        assert_eq!(js_round(0.49), 0.0);
    }

    #[test]
    fn civil_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 6, 14), 20618);
        for days in [-1000, 0, 59, 20617, 20618, 30000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(utc_ms(2026, 6, 14, 10, 0, 0), 1_781_431_200_000);
    }
}

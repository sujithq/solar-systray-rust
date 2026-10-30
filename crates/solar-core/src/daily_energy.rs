//! Volatile meter-day import/export counters (port of `daily-energy.mjs`).

use crate::reading::{EnergyTotals, Reading};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyEnergyResult {
    pub available: bool,
    pub import_wh: Option<i64>,
    pub export_wh: Option<i64>,
    pub since_minute: Option<i64>,
    pub reason: Cow<'static, str>,
}

impl DailyEnergyResult {
    pub fn unavailable(reason: &'static str) -> Self {
        DailyEnergyResult {
            available: false,
            import_wh: None,
            export_wh: None,
            since_minute: None,
            reason: Cow::Borrowed(reason),
        }
    }
}

fn counters(totals: &EnergyTotals) -> [i64; 4] {
    [totals.import_tariff1_wh, totals.import_tariff2_wh, totals.export_tariff1_wh, totals.export_tariff2_wh]
}

fn valid_totals(totals: Option<&EnergyTotals>) -> Option<[i64; 4]> {
    totals.map(counters).filter(|values| values.iter().all(|v| (0..=999_999_999).contains(v)))
}

#[derive(Debug, Clone)]
pub struct DailyEnergy {
    day: Option<i64>,
    offset_minutes: Option<i64>,
    since_minute: Option<i64>,
    last_unix_ms: i64,
    baseline: [i64; 4],
    last: [i64; 4],
    invalid_reason: Option<&'static str>,
    result: DailyEnergyResult,
}

impl Default for DailyEnergy {
    fn default() -> Self {
        DailyEnergy {
            day: None,
            offset_minutes: None,
            since_minute: None,
            last_unix_ms: 0,
            baseline: [0; 4],
            last: [0; 4],
            invalid_reason: None,
            result: DailyEnergyResult::unavailable("missing-reading"),
        }
    }
}

impl DailyEnergy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn current(&self, fresh: bool) -> DailyEnergyResult {
        if fresh { self.result.clone() } else { DailyEnergyResult::unavailable("stale") }
    }

    pub fn update(&mut self, reading: Option<&Reading>, fresh: bool) -> DailyEnergyResult {
        if !fresh {
            return self.unavailable("stale");
        }
        let Some(reading) = reading else { return self.unavailable("missing-reading") };
        let (Some(day), Some(minute), Some(offset), Some(meter_time)) =
            (reading.meter_day, reading.meter_minute_of_day, reading.meter_offset_minutes, reading.meter_time)
        else {
            return self.unavailable("missing-meter-time");
        };
        if day < 0 || !(0..1440).contains(&minute) || !matches!(offset, 60 | 120) {
            return self.unavailable("missing-meter-time");
        }
        let Some(values) = valid_totals(reading.energy_totals_wh.as_ref()) else {
            return self.unavailable("missing-or-invalid-totals");
        };
        if self.day.is_none_or(|current| day > current) {
            self.start_day(day, offset, minute, meter_time, values);
            return self.current(true);
        }
        if self.day.is_some_and(|current| day < current) {
            return self.invalidate("clock-discontinuity");
        }
        if let Some(reason) = self.invalid_reason {
            return self.unavailable(reason);
        }
        if Some(offset) != self.offset_minutes || meter_time < self.last_unix_ms {
            return self.invalidate("clock-discontinuity");
        }
        if values.iter().zip(self.last).any(|(value, last)| *value < last) {
            return self.invalidate("counter-decreased");
        }
        self.last = values;
        self.last_unix_ms = meter_time;
        self.result = DailyEnergyResult {
            available: true,
            import_wh: Some(values[0] + values[1] - self.baseline[0] - self.baseline[1]),
            export_wh: Some(values[2] + values[3] - self.baseline[2] - self.baseline[3]),
            since_minute: self.since_minute,
            reason: Cow::Borrowed("available"),
        };
        self.current(true)
    }

    fn start_day(&mut self, day: i64, offset: i64, minute: i64, meter_time: i64, values: [i64; 4]) {
        self.day = Some(day);
        self.offset_minutes = Some(offset);
        self.since_minute = Some(minute);
        self.last_unix_ms = meter_time;
        self.baseline = values;
        self.last = values;
        self.invalid_reason = None;
        self.result = DailyEnergyResult {
            available: true,
            import_wh: Some(0),
            export_wh: Some(0),
            since_minute: Some(minute),
            reason: Cow::Borrowed("available"),
        };
    }

    fn unavailable(&mut self, reason: &'static str) -> DailyEnergyResult {
        self.result = DailyEnergyResult::unavailable(reason);
        self.current(true)
    }

    fn invalidate(&mut self, reason: &'static str) -> DailyEnergyResult {
        self.invalid_reason = Some(reason);
        self.unavailable(reason)
    }
}

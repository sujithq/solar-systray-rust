//! Quarter-hour capacity peak forecast (port of `peak-forecast.mjs`).
//!
//! With D = 1-0:1.4.0 demand, I = import, P = max(monthly peak, 2.5 kW), t seconds into the
//! quarter and left = 900 - t: projected = D + I * left / 900, the import that lands exactly on
//! P is (P - D) * 900 / left, and D reaches P after (P - D) * 900 / I seconds.

use crate::reading::Reading;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

pub const QUARTER_SECONDS: i64 = 900;
pub const MIN_ELAPSED_SECONDS: i64 = 60;
pub const LOCKED_IN_SECONDS: i64 = 5;
pub const MINIMUM_PEAK_W: f64 = 2500.0;
pub const WARN_RATIO: f64 = 1.0;
pub const WARN_HOLD_SECONDS: i64 = 30;
pub const CLEAR_RATIO: f64 = 0.95;
pub const MAX_METER_GAP_SECONDS: i64 = 30;
pub const PLAUSIBILITY_TOLERANCE_W: f64 = 150.0;
pub const PLAUSIBILITY_START_SECONDS: i64 = 10;
pub const DEMAND_RESET_W: f64 = 5.0;

/// One forecaster input; `None` is a missing value.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PeakSample {
    pub offline: bool,
    pub meter_seconds: Option<f64>,
    pub import_w: Option<f64>,
    pub demand_w: Option<f64>,
    pub peak_w: Option<f64>,
    pub meter_month: Option<f64>,
    pub peak_month: Option<f64>,
}

impl PeakSample {
    pub fn offline() -> Self {
        PeakSample { offline: true, ..Default::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Forecast {
    pub available: bool,
    pub reason: Cow<'static, str>,
    pub projected_w: Option<f64>,
    pub comparison_w: Option<f64>,
    pub comparison_is_minimum: bool,
    pub seconds_left: Option<f64>,
    pub reduce_w: Option<f64>,
    pub crossing_seconds: Option<f64>,
    pub warning: bool,
}

impl Forecast {
    pub fn unavailable(reason: &'static str) -> Self {
        Forecast {
            available: false,
            reason: Cow::Borrowed(reason),
            projected_w: None,
            comparison_w: None,
            comparison_is_minimum: false,
            seconds_left: None,
            reduce_w: None,
            crossing_seconds: None,
            warning: false,
        }
    }
}

fn finite(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite())
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Observation {
    pub max_import_w: Option<f64>,
    pub observed_from: Option<i64>,
}

/// One projection without hysteresis.
pub fn forecast_quarter(sample: Option<&PeakSample>, observation: Observation) -> Forecast {
    let Some(sample) = sample.filter(|s| !s.offline) else { return Forecast::unavailable("offline") };
    let (Some(meter_seconds), Some(meter_month)) = (finite(sample.meter_seconds), finite(sample.meter_month)) else {
        return Forecast::unavailable("no-meter-time");
    };
    let Some(import_w) = finite(sample.import_w).filter(|v| *v >= 0.0) else {
        return Forecast::unavailable("invalid-input");
    };
    if sample.demand_w.is_some_and(|v| !v.is_finite() || v < 0.0)
        || sample.peak_w.is_some_and(|v| !v.is_finite() || v < 0.0)
    {
        return Forecast::unavailable("invalid-input");
    }
    let Some(demand) = finite(sample.demand_w) else { return Forecast::unavailable("no-demand") };
    let Some(peak_w) = finite(sample.peak_w) else { return Forecast::unavailable("no-peak") };
    let Some(peak_month) = finite(sample.peak_month) else { return Forecast::unavailable("no-peak-time") };
    if peak_month < meter_month {
        return Forecast::unavailable("previous-month-peak");
    }
    if peak_month > meter_month {
        return Forecast::unavailable("invalid-input");
    }
    let second = meter_seconds.floor() as i64;
    let t = second - second.div_euclid(QUARTER_SECONDS) * QUARTER_SECONDS;
    let left = QUARTER_SECONDS - t;
    if t < MIN_ELAPSED_SECONDS {
        return Forecast::unavailable("too-early");
    }
    if left <= LOCKED_IN_SECONDS {
        return Forecast::unavailable("locked-in");
    }
    let comparison = peak_w.max(MINIMUM_PEAK_W);
    if demand >= comparison {
        return Forecast::unavailable("peak-reached");
    }
    let (t, left, quarter) = (t as f64, left as f64, QUARTER_SECONDS as f64);
    if let (Some(from), Some(max_import)) = (observation.observed_from, finite(observation.max_import_w))
        && from <= PLAUSIBILITY_START_SECONDS
        && demand > max_import * t / quarter + PLAUSIBILITY_TOLERANCE_W
    {
        return Forecast::unavailable("implausible-demand");
    }
    let projected = demand + import_w * left / quarter;
    let allowed = (comparison - demand) * quarter / left;
    let crossing = if import_w > 0.0 { Some((comparison - demand) * quarter / import_w) } else { None };
    Forecast {
        available: true,
        reason: Cow::Borrowed("ok"),
        projected_w: Some(projected),
        comparison_w: Some(comparison),
        comparison_is_minimum: peak_w < MINIMUM_PEAK_W,
        seconds_left: Some(left),
        reduce_w: Some((import_w - allowed).max(0.0)),
        crossing_seconds: crossing.filter(|c| *c < left),
        warning: false,
    }
}

/// Follows one meter through its quarters and adds the warning hysteresis.
#[derive(Debug, Clone, Default)]
pub struct PeakForecaster {
    quarter: Option<i64>,
    max_import_w: f64,
    observed_from: Option<i64>,
    previous_second: Option<i64>,
    previous_demand_w: Option<f64>,
    warning: bool,
    above_since: Option<i64>,
    last: Option<Forecast>,
}

impl PeakForecaster {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn restart_quarter(&mut self, quarter: i64, t: i64) {
        self.quarter = Some(quarter);
        self.max_import_w = 0.0;
        self.observed_from = Some(t);
        self.warning = false;
        self.above_since = None;
    }

    pub fn update(&mut self, sample: Option<&PeakSample>) -> Forecast {
        let meter_seconds = sample.filter(|s| !s.offline).and_then(|s| finite(s.meter_seconds));
        let (Some(sample), Some(meter_seconds)) = (sample, meter_seconds) else {
            self.reset();
            return forecast_quarter(sample, Observation::default());
        };
        let second = meter_seconds.floor() as i64;
        if let Some(last) = &self.last
            && Some(second) == self.previous_second
        {
            return last.clone();
        }
        if let Some(previous) = self.previous_second
            && (second < previous || second - previous > MAX_METER_GAP_SECONDS)
        {
            self.reset();
            self.previous_second = Some(second);
            let result = Forecast::unavailable("clock-jump");
            self.last = Some(result.clone());
            return result;
        }
        self.previous_second = Some(second);
        let quarter = second.div_euclid(QUARTER_SECONDS);
        let t = second - quarter * QUARTER_SECONDS;
        let demand = finite(sample.demand_w);
        let demand_fell = matches!((demand, self.previous_demand_w), (Some(d), Some(p)) if d < p - DEMAND_RESET_W);
        if Some(quarter) != self.quarter || demand_fell {
            self.restart_quarter(quarter, t);
        }
        if let Some(import) = finite(sample.import_w).filter(|v| *v >= 0.0) {
            self.max_import_w = self.max_import_w.max(import);
        }
        if demand.is_some() {
            self.previous_demand_w = demand;
        }
        let mut result = forecast_quarter(
            Some(sample),
            Observation { max_import_w: Some(self.max_import_w), observed_from: self.observed_from },
        );
        if !result.available {
            self.warning = false;
            self.above_since = None;
        } else {
            let projected = result.projected_w.unwrap();
            let comparison = result.comparison_w.unwrap();
            if self.warning {
                if projected < CLEAR_RATIO * comparison {
                    self.warning = false;
                    self.above_since = None;
                }
            } else if projected > WARN_RATIO * comparison {
                let since = *self.above_since.get_or_insert(second);
                if second - since >= WARN_HOLD_SECONDS {
                    self.warning = true;
                }
            } else {
                self.above_since = None;
            }
        }
        result.warning = self.warning;
        self.last = Some(result.clone());
        result
    }
}

/// The forecaster input for a parsed reading; `fresh` is false for a missing or stale one.
pub fn peak_forecast_sample(reading: Option<&Reading>, fresh: bool) -> PeakSample {
    let Some(reading) = reading.filter(|_| fresh) else { return PeakSample::offline() };
    PeakSample {
        offline: false,
        meter_seconds: reading.meter_time.map(|t| t as f64 / 1000.0),
        import_w: Some(reading.grid_import_w),
        demand_w: reading.current_demand_w,
        peak_w: reading.monthly_peak_w,
        meter_month: reading.meter_month.map(|m| m as f64),
        peak_month: reading.monthly_peak_month.map(|m| m as f64),
    }
}

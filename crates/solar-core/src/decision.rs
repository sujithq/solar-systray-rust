//! The GO/no-GO decision engine and solar level (port of `decision.mjs`).

use crate::config::SolarConfig;
use crate::js_round;
use crate::reading::{Quality, Reading};
use crate::sun::{Sun, SunPosition, sun_state};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum State {
    Excellent,
    Good,
    Limited,
    Importing,
    Idle,
    Offline,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Excellent => "EXCELLENT",
            State::Good => "GOOD",
            State::Limited => "LIMITED",
            State::Importing => "IMPORTING",
            State::Idle => "IDLE",
            State::Offline => "OFFLINE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Level {
    Night,
    Importing,
    Producing,
    Surplus,
    MaxSurplus,
    Offline,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Night => "NIGHT",
            Level::Importing => "IMPORTING",
            Level::Producing => "PRODUCING",
            Level::Surplus => "SURPLUS",
            Level::MaxSurplus => "MAX_SURPLUS",
            Level::Offline => "OFFLINE",
        }
    }

    /// Character modes: idle 0, surprise 1, working 2, complete 3, attention 4, sleep 5.
    pub fn character_mode(self) -> u8 {
        match self {
            Level::Night => 5,
            Level::Importing => 4,
            Level::Producing => 0,
            Level::Surplus => 2,
            Level::MaxSurplus => 3,
            Level::Offline => 1,
        }
    }
}

pub fn valid_reading(reading: Option<&Reading>) -> bool {
    let Some(r) = reading else { return false };
    r.quality == Quality::Valid
        && [r.grid_import_w, r.grid_export_w].iter().all(|v| v.is_finite() && (0.0..=1_000_000.0).contains(v))
}

/// A valid reading that is not from the future and not older than the stale window.
pub fn fresh_reading(reading: Option<&Reading>, now: i64, stale_after_seconds: f64) -> bool {
    valid_reading(reading)
        && reading.is_some_and(|r| now >= r.timestamp && ((now - r.timestamp) as f64) <= stale_after_seconds * 1000.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GaugeScale {
    pub peak_w: f64,
    pub max_w: f64,
}

/// The import gauge's halfway tick: the configured peak, raised to a higher meter monthly peak.
pub fn import_gauge_scale(import_peak_w: f64, import_gauge_max_w: f64, monthly_peak_w: Option<f64>) -> GaugeScale {
    let meter_peak_w = match monthly_peak_w {
        Some(w) if w.is_finite() && w > 0.0 => js_round(w),
        _ => 0.0,
    };
    let peak_w = import_peak_w.max(meter_peak_w);
    let max_w = if import_gauge_max_w > peak_w { import_gauge_max_w } else { (peak_w * 2.0).min(86400.0) };
    GaugeScale { peak_w, max_w }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evaluation {
    pub appliance: String,
    pub appliance_name: String,
    pub appliance_peak_w: f64,
    pub required_w: f64,
    pub start_threshold_w: f64,
    pub safe_to_start: bool,
    pub surplus_w: Option<f64>,
    pub grid_import_w: Option<f64>,
    pub stable_seconds: f64,
    pub level: Level,
    pub sun: Sun,
    pub sun_elevation: Option<f64>,
    pub minimum_export_w: f64,
    pub max_surplus_w: f64,
    pub import_peak_w: f64,
    pub import_gauge_max_w: f64,
    pub gauge_import_peak_w: f64,
    pub gauge_import_max_w: f64,
    pub current_demand_w: Option<f64>,
    pub monthly_peak_w: Option<f64>,
    pub monthly_peak_reached: bool,
    pub state: State,
    pub reason: Cow<'static, str>,
    pub character_mode: u8,
}

#[derive(Debug, Clone)]
pub struct DecisionEngine {
    config: SolarConfig,
    appliance: String,
    level: Level,
    stable_since: Option<i64>,
    previous_timestamp: Option<i64>,
    previous_source: Option<String>,
    ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownAppliance;

impl std::fmt::Display for UnknownAppliance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Unknown appliance")
    }
}

impl std::error::Error for UnknownAppliance {}

impl DecisionEngine {
    pub fn new(config: SolarConfig) -> Self {
        let appliance = config.decision.default_appliance.clone();
        DecisionEngine {
            config,
            appliance,
            level: Level::Offline,
            stable_since: None,
            previous_timestamp: None,
            previous_source: None,
            ready: false,
        }
    }

    pub fn reset(&mut self) {
        self.stable_since = None;
        self.previous_timestamp = None;
        self.previous_source = None;
        self.ready = false;
    }

    pub fn appliance(&self) -> &str {
        &self.appliance
    }

    pub fn select_appliance(&mut self, id: &str) -> Result<(), UnknownAppliance> {
        if !self.config.appliances.contains_key(id) {
            return Err(UnknownAppliance);
        }
        self.appliance = id.to_string();
        self.reset();
        Ok(())
    }

    /// Hysteresis keeps the level from flickering around the minimum and maximum thresholds.
    fn next_level(&self, reading: &Reading, sun: Sun) -> Level {
        let d = &self.config.decision;
        let previous = self.level;
        let export_w = reading.grid_export_w;
        if sun == Sun::Down {
            return Level::Night;
        }
        if reading.grid_import_w > 0.0 {
            return Level::Importing;
        }
        if export_w >= d.max_surplus_w
            || (previous == Level::MaxSurplus && export_w >= d.max_surplus_w - d.level_hysteresis_w)
        {
            return Level::MaxSurplus;
        }
        if export_w > d.minimum_export_w + d.level_hysteresis_w
            || (matches!(previous, Level::Surplus | Level::MaxSurplus) && export_w > d.minimum_export_w)
        {
            return Level::Surplus;
        }
        Level::Producing
    }

    /// Evaluates with the sun computed from the configured location.
    pub fn evaluate_at(&mut self, reading: Option<&Reading>, now: i64) -> Evaluation {
        let sun = sun_state(&self.config.location, now);
        self.evaluate(reading, now, sun)
    }

    pub fn evaluate(&mut self, reading: Option<&Reading>, now: i64, sun: impl Into<SunPosition>) -> Evaluation {
        let sun = sun.into();
        let d = self.config.decision.clone();
        let profile = &self.config.appliances[&self.appliance];
        let required_w = profile.peak_w + d.safety_margin_w;
        let fresh = fresh_reading(reading, now, d.stale_after_seconds);
        let fresh_value = if fresh { reading } else { None };
        let gauge =
            import_gauge_scale(d.import_peak_w, d.import_gauge_max_w, fresh_value.and_then(|r| r.monthly_peak_w));
        let monthly_peak_reached = fresh_value.is_some_and(|r| match (r.current_demand_w, r.monthly_peak_w) {
            (Some(demand), Some(peak)) => demand.is_finite() && peak.is_finite() && peak > 0.0 && demand >= peak,
            _ => false,
        });
        let mut result = Evaluation {
            appliance: self.appliance.clone(),
            appliance_name: profile.name.clone(),
            appliance_peak_w: profile.peak_w,
            required_w,
            start_threshold_w: required_w + d.hysteresis_w,
            safe_to_start: false,
            surplus_w: None,
            grid_import_w: None,
            stable_seconds: 0.0,
            level: Level::Offline,
            sun: sun.state,
            sun_elevation: sun.elevation,
            minimum_export_w: d.minimum_export_w,
            max_surplus_w: d.max_surplus_w,
            import_peak_w: d.import_peak_w,
            import_gauge_max_w: d.import_gauge_max_w,
            gauge_import_peak_w: gauge.peak_w,
            gauge_import_max_w: gauge.max_w,
            current_demand_w: fresh_value.and_then(|r| r.current_demand_w),
            monthly_peak_w: fresh_value.and_then(|r| r.monthly_peak_w),
            monthly_peak_reached,
            state: State::Offline,
            reason: Cow::Borrowed("No fresh, validated meter reading"),
            character_mode: Level::Offline.character_mode(),
        };
        let finish = |mut result: Evaluation, state: State, reason: &'static str| {
            result.state = state;
            result.reason = Cow::Borrowed(reason);
            result.character_mode = result.level.character_mode();
            result
        };
        let Some(reading) = reading.filter(|_| fresh) else {
            self.reset();
            self.level = Level::Offline;
            return finish(result, State::Offline, "No fresh, validated meter reading");
        };
        self.level = self.next_level(reading, sun.state);
        result.level = self.level;
        let gap = self.previous_timestamp.map_or(0, |previous| reading.timestamp - previous);
        if gap < 0
            || gap as f64 > d.stale_after_seconds * 1000.0
            || self.previous_source.as_deref() != Some(reading.source.as_str())
        {
            self.reset();
        }
        self.previous_timestamp = Some(reading.timestamp);
        self.previous_source = Some(reading.source.clone());
        result.surplus_w = Some(reading.grid_export_w);
        result.grid_import_w = Some(reading.grid_import_w);
        if reading.grid_import_w > 0.0 || reading.grid_export_w <= d.minimum_export_w {
            self.stable_since = None;
            self.ready = false;
            return if reading.grid_import_w > 0.0 {
                finish(result, State::Importing, "Grid electricity is being imported")
            } else {
                finish(result, State::Idle, "No meaningful solar surplus")
            };
        }
        // Hysteresis is above the safety floor: never retain GO below peak + margin.
        let threshold = required_w + if self.ready { 0.0 } else { d.hysteresis_w };
        if reading.grid_export_w <= threshold {
            self.stable_since = None;
            self.ready = false;
            return finish(result, State::Limited, "Insufficient surplus for the selected appliance");
        }
        let since = *self.stable_since.get_or_insert(reading.timestamp);
        result.stable_seconds = ((reading.timestamp - since) as f64 / 1000.0).max(0.0);
        self.ready = self.ready || result.stable_seconds >= d.stable_duration_seconds;
        if !self.ready {
            return finish(result, State::Limited, "Waiting for stable surplus");
        }
        result.safe_to_start = true;
        let state =
            if reading.grid_export_w > required_w + d.excellent_extra_w { State::Excellent } else { State::Good };
        finish(result, state, "OK to start based on present surplus")
    }
}

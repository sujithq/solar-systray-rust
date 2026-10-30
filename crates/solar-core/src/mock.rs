//! Simulated meters (port of the mock parts of `providers.mjs`).

use crate::reading::Reading;
use crate::{js_round, utc_ms};

/// A synthetic quarter-hour peak meter profile; `steps` are (seconds since start, import W).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeakScenario {
    pub peak_w: f64,
    pub start_second: i64,
    pub start_demand_w: Option<f64>,
    pub rollover: bool,
    pub no_time: bool,
    pub steps: &'static [(i64, f64)],
}

const PLAIN: PeakScenario = PeakScenario {
    peak_w: 2800.0,
    start_second: 120,
    start_demand_w: None,
    rollover: false,
    no_time: false,
    steps: &[(0, 3400.0)],
};

pub fn peak_scenario(name: &str) -> Option<PeakScenario> {
    Some(match name {
        "peak-approaching" => PLAIN,
        "peak-avoided" => PeakScenario { steps: &[(0, 3400.0), (60, 800.0)], ..PLAIN },
        "peak-exceeded" => {
            PeakScenario { start_second: 780, start_demand_w: Some(2900.0), steps: &[(0, 3000.0)], ..PLAIN }
        }
        "peak-rollover" => PeakScenario { rollover: true, ..PLAIN },
        "peak-no-time" => PeakScenario { no_time: true, ..PLAIN },
        _ => return None,
    })
}

/// Scenario names in menu order with their (import W, export W); `None` is an offline meter.
pub const MOCK_SCENARIOS: &[(&str, Option<(f64, f64)>)] = &[
    ("max", Some((0.0, 5600.0))),
    ("excellent", Some((0.0, 4200.0))),
    ("good", Some((0.0, 2600.0))),
    ("limited", Some((0.0, 900.0))),
    ("importing", Some((650.0, 0.0))),
    ("idle", Some((0.0, 0.0))),
    ("producing-importing", Some((650.0, 0.0))),
    ("inverter-offline", Some((650.0, 0.0))),
    ("night", Some((0.0, 0.0))),
    ("offline", None),
    ("peak-approaching", Some((3400.0, 0.0))),
    ("peak-avoided", Some((3400.0, 0.0))),
    ("peak-exceeded", Some((3000.0, 0.0))),
    ("peak-rollover", Some((3400.0, 0.0))),
    ("peak-no-time", Some((3400.0, 0.0))),
];

pub fn mock_scenario(name: &str) -> Option<Option<(f64, f64)>> {
    MOCK_SCENARIOS.iter().find(|(id, _)| *id == name).map(|(_, powers)| *powers)
}

/// A human label for a scenario id.
pub fn scenario_label(name: &str) -> &'static str {
    match name {
        "max" => "Max surplus",
        "excellent" => "Excellent",
        "good" => "Good",
        "limited" => "Limited",
        "importing" => "Importing",
        "idle" => "Idle",
        "producing-importing" => "Producing, importing",
        "inverter-offline" => "Inverter offline",
        "night" => "Night",
        "offline" => "Meter offline",
        "peak-approaching" => "Peak approaching",
        "peak-avoided" => "Peak avoided",
        "peak-exceeded" => "Peak exceeded",
        "peak-rollover" => "Peak from last month",
        "peak-no-time" => "Peak without meter clock",
        "custom" => "Custom",
        _ => "Unknown",
    }
}

/// A synthetic peak meter; `elapsed_seconds` counts from when the scenario was chosen. The
/// meter clock starts `start_second` into a quarter on 14 June 2026 (or 1 July for the
/// rollover) and 1-0:1.4.0 adds import / 900 every second, restarting at each quarter.
#[derive(Debug, Clone)]
pub struct PeakMeter {
    profile: PeakScenario,
    base: i64,
    elapsed: i64,
    demand_w: f64,
}

impl PeakMeter {
    pub fn new(profile: PeakScenario) -> Self {
        // 2026-06-14 12:00 CEST and 2026-07-01 00:00 CEST, both quarter starts.
        let base = if profile.rollover { utc_ms(2026, 6, 30, 22, 0, 0) } else { utc_ms(2026, 6, 14, 10, 0, 0) } / 1000;
        let mut meter = PeakMeter { profile, base, elapsed: 0, demand_w: 0.0 };
        meter.demand_w = profile.start_demand_w.unwrap_or(meter.import_at(0) * profile.start_second as f64 / 900.0);
        meter
    }

    fn import_at(&self, elapsed: i64) -> f64 {
        self.profile.steps.iter().fold(0.0, |watts, &(from, value)| if elapsed >= from { value } else { watts })
    }

    pub fn reading(&mut self, elapsed_seconds: i64, timestamp: i64) -> Reading {
        while self.elapsed < elapsed_seconds {
            let second = self.base + self.profile.start_second + self.elapsed + 1;
            self.demand_w += self.import_at(self.elapsed) / 900.0;
            if second % 900 == 0 {
                self.demand_w = 0.0;
            }
            self.elapsed += 1;
        }
        let meter_seconds = self.base + self.profile.start_second + self.elapsed;
        let clock = !self.profile.no_time;
        let mut reading = Reading::new(timestamp, self.import_at(self.elapsed), 0.0, "mock");
        reading.current_demand_w = Some(js_round(self.demand_w));
        reading.monthly_peak_w = Some(self.profile.peak_w);
        if clock {
            reading.meter_time = Some(meter_seconds * 1000);
            reading.meter_month = Some(if self.profile.rollover { 202607 } else { 202606 });
            // The rollover keeps June's peak, captured on 18 June at 19:00:12 CEST.
            reading.monthly_peak_at =
                Some(if self.profile.rollover { utc_ms(2026, 6, 18, 17, 0, 12) } else { (self.base - 3600) * 1000 });
            reading.monthly_peak_month = Some(202606);
        }
        reading
    }
}

#[derive(Debug, Clone)]
pub struct MockProvider {
    scenario: String,
    /// Signed watts for the "custom" scenario: negative imports, positive exports.
    watts: Option<i64>,
    peak_meter: Option<PeakMeter>,
    peak_started_at: i64,
    latest: Option<Reading>,
}

impl Default for MockProvider {
    fn default() -> Self {
        MockProvider { scenario: "excellent".into(), watts: None, peak_meter: None, peak_started_at: 0, latest: None }
    }
}

impl MockProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scenario(&self) -> &str {
        &self.scenario
    }

    pub fn watts(&self) -> Option<i64> {
        self.watts
    }

    pub fn latest(&self) -> Option<&Reading> {
        self.latest.as_ref()
    }

    pub fn set_scenario(&mut self, scenario: &str, now: i64) -> Result<Option<Reading>, &'static str> {
        if mock_scenario(scenario).is_none() {
            return Err("Unknown simulation scenario");
        }
        self.scenario = scenario.to_string();
        self.watts = None;
        self.peak_meter = peak_scenario(scenario).map(PeakMeter::new);
        self.peak_started_at = now;
        Ok(self.sample(now))
    }

    /// A custom reading, sampled every second like a meter; the value is checked by the caller.
    pub fn set_power(&mut self, watts: i64, now: i64) -> Option<Reading> {
        self.scenario = "custom".into();
        self.watts = Some(watts);
        self.peak_meter = None;
        self.sample(now)
    }

    /// Takes a new sample; `None` means the simulated meter is offline.
    pub fn sample(&mut self, now: i64) -> Option<Reading> {
        if let Some(meter) = &mut self.peak_meter {
            self.latest = Some(meter.reading((now - self.peak_started_at).div_euclid(1000), now));
            return self.latest.clone();
        }
        let powers = match (self.scenario.as_str(), self.watts) {
            ("custom", Some(watts)) => Some(((-watts).max(0) as f64, watts.max(0) as f64)),
            (scenario, _) => mock_scenario(scenario).flatten(),
        };
        self.latest = powers.map(|(import, export)| Reading::new(now, import, export, "mock"));
        self.latest.clone()
    }

    pub fn clear(&mut self) {
        self.latest = None;
    }
}

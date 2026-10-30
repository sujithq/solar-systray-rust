//! Ties the meter, inverter, decision engine and forecasts together (port of `runtime.mjs`).

use crate::config::SolarConfig;
use crate::daily_energy::{DailyEnergy, DailyEnergyResult};
use crate::decision::{DecisionEngine, Evaluation, fresh_reading};
use crate::mock::MockProvider;
use crate::night_baseline::NightBaseline;
use crate::peak_forecast::{Forecast, PeakForecaster, peak_forecast_sample};
use crate::reading::Reading;
use crate::simulation::{Preset, simulated_watts, simulation_presets};
use crate::slimmelezer::{MeterEvent, MeterStats, SlimmeLezer};
use crate::sun::{Sun, SunTimes, sun_times};
use crate::sungrow::{InverterSnapshot, InverterStatus, Powers, SungrowState};
use serde::{Deserialize, Serialize};

pub enum Meter {
    Mock(MockProvider),
    SlimmeLezer(SlimmeLezer),
}

/// What the tray shows. It deserializes from a Solar Agent snapshot too; that has no
/// `presets` or addresses, so [`crate::agent`] fills them in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    #[serde(flatten)]
    pub evaluation: Evaluation,
    pub peak_forecast: Forecast,
    pub daily_energy: DailyEnergyResult,
    pub simulated: bool,
    pub scenario: Option<String>,
    pub simulated_w: Option<i64>,
    #[serde(default)]
    pub presets: Vec<Preset>,
    pub stable_duration_seconds: f64,
    pub location_configured: bool,
    pub sunrise: Option<i64>,
    pub sunset: Option<i64>,
    pub diagnostics: MeterStats,
    pub meter_address: Option<String>,
    pub inverter_address: Option<String>,
    pub inverter: InverterSnapshot,
    pub night_baseline_w: Option<f64>,
    pub night_baseline_learned: bool,
    pub night_baseline_samples: usize,
    pub reading_at: Option<i64>,
    pub checked_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlError(pub String);

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ControlError {}

pub struct Runtime {
    config: SolarConfig,
    meter: Meter,
    inverter: SungrowState,
    engine: DecisionEngine,
    forecaster: PeakForecaster,
    daily_energy: DailyEnergy,
    night_baseline: NightBaseline,
    sun_times: Option<(i64, SunTimes)>,
}

impl Runtime {
    pub fn new(config: SolarConfig) -> Self {
        let meter = if config.is_mock() {
            Meter::Mock(MockProvider::new())
        } else {
            Meter::SlimmeLezer(SlimmeLezer::new(config.energy_source.allow_legacy_without_crc))
        };
        Runtime {
            engine: DecisionEngine::new(config.clone()),
            config,
            meter,
            inverter: SungrowState::default(),
            forecaster: PeakForecaster::new(),
            daily_energy: DailyEnergy::new(),
            night_baseline: NightBaseline::new(),
            sun_times: None,
        }
    }

    pub fn config(&self) -> &SolarConfig {
        &self.config
    }

    pub fn appliance(&self) -> &str {
        self.engine.appliance()
    }

    fn mock(&self) -> Option<&MockProvider> {
        match &self.meter {
            Meter::Mock(mock) => Some(mock),
            Meter::SlimmeLezer(_) => None,
        }
    }

    pub fn scenario(&self) -> Option<&str> {
        self.mock().map(MockProvider::scenario)
    }

    fn fresh(&self, reading: Option<&Reading>, now: i64) -> bool {
        fresh_reading(reading, now, self.config.decision.stale_after_seconds)
    }

    pub fn invalidate(&mut self) {
        self.engine.reset();
        self.forecaster.reset();
    }

    pub fn observe(&mut self, reading: &Reading, now: i64) {
        let fresh = self.fresh(Some(reading), now);
        self.daily_energy.update(Some(reading), fresh);
        self.evaluate(Some(reading), now);
        self.forecast(Some(reading), now);
    }

    fn dispatch(&mut self, events: Vec<MeterEvent>, now: i64) {
        for event in events {
            match event {
                MeterEvent::Reading(reading) => self.observe(&reading, now),
                MeterEvent::Diagnostic => self.invalidate(),
            }
        }
    }

    // Simulations choose day or night themselves so every level can be shown at any time.
    fn evaluate(&mut self, reading: Option<&Reading>, now: i64) -> Evaluation {
        match self.mock().map(|mock| mock.scenario() == "night") {
            Some(night) => self.engine.evaluate(reading, now, if night { Sun::Down } else { Sun::Up }),
            None => self.engine.evaluate_at(reading, now),
        }
    }

    // Every reading feeds the forecaster so its hold time follows the meter clock, not polling.
    fn forecast(&mut self, reading: Option<&Reading>, now: i64) -> Forecast {
        let fresh = self.fresh(reading, now);
        self.forecaster.update(Some(&peak_forecast_sample(reading, fresh)))
    }

    // Crossings move by about a minute a day, so they are worked out once a minute.
    fn sun_times(&mut self, now: i64) -> SunTimes {
        let minute = now.div_euclid(60_000);
        match self.sun_times {
            Some((cached, times)) if cached == minute => times,
            _ => {
                let times = sun_times(&self.config.location, now);
                self.sun_times = Some((minute, times));
                times
            }
        }
    }

    // ---- Meter link (SlimmeLezer) ----

    pub fn meter_connected(&mut self) {
        if let Meter::SlimmeLezer(link) = &mut self.meter {
            link.connected();
            self.invalidate();
        }
    }

    pub fn meter_bytes(&mut self, bytes: &[u8], now: i64) {
        if let Meter::SlimmeLezer(link) = &mut self.meter {
            let events = link.feed(bytes, now);
            self.dispatch(events, now);
        }
    }

    pub fn meter_issue(&mut self, code: &str) {
        if let Meter::SlimmeLezer(link) = &mut self.meter {
            link.issue(code);
            self.invalidate();
        }
    }

    /// The socket closed; returns the reconnect delay in milliseconds while running.
    pub fn meter_closed(&mut self, running: bool, now: i64) -> Option<u64> {
        let Meter::SlimmeLezer(link) = &mut self.meter else { return None };
        let (events, delay) = link.closed(running);
        self.dispatch(events, now);
        delay
    }

    /// Feeds a reading as if the meter had produced it (simulators and tests).
    pub fn push_reading(&mut self, reading: Reading, now: i64) {
        if let Meter::SlimmeLezer(link) = &mut self.meter {
            link.set_latest(Some(reading.clone()));
        }
        self.observe(&reading, now);
    }

    // ---- Simulation ----

    /// One simulated meter sample, taken every second.
    pub fn mock_tick(&mut self, now: i64) {
        let Meter::Mock(mock) = &mut self.meter else { return };
        match mock.sample(now) {
            Some(reading) => self.observe(&reading, now),
            None => self.invalidate(),
        }
    }

    // ---- Inverter ----

    pub fn next_inverter_transaction(&mut self) -> u16 {
        self.inverter.next_transaction()
    }

    pub fn inverter_result(&mut self, result: Result<Powers, String>, now: i64) {
        self.inverter.record(result, now);
    }

    fn inverter_snapshot(&self, now: i64) -> InverterSnapshot {
        let Some(mock) = self.mock() else {
            return self.inverter.snapshot(self.config.inverter_enabled(), now);
        };
        let timestamp = Some(mock.latest().map_or(now, |r| r.timestamp));
        let offline = |issue: &str| InverterSnapshot::without_reading(InverterStatus::Offline, Some(issue.into()));
        let reading = |status, pv: f64, ac: f64| InverterSnapshot {
            status,
            pv_power_w: Some(pv),
            active_power_w: Some(ac),
            household_use_w: None,
            timestamp,
            issue: None,
        };
        match mock.scenario() {
            "offline" => offline("Simulated meter and inverter offline"),
            "inverter-offline" => offline("Simulated inverter offline"),
            "night" => reading(InverterStatus::Standby, 0.0, 0.0),
            "producing-importing" => reading(InverterStatus::Online, 3600.0, 3500.0),
            _ => InverterSnapshot::disabled(),
        }
    }

    fn household_use(
        &self,
        reading: Option<&Reading>,
        fresh: bool,
        sun: Sun,
        inverter: &InverterSnapshot,
    ) -> Option<f64> {
        let reading = reading.filter(|_| fresh && !self.config.is_mock())?;
        if sun == Sun::Down {
            return Some(reading.grid_import_w);
        }
        let active = inverter
            .active_power_w
            .filter(|w| inverter.status == InverterStatus::Online && w.is_finite() && *w >= 0.0)?;
        Some((active + reading.grid_import_w - reading.grid_export_w).max(0.0))
    }

    // ---- Snapshot and controls ----

    pub fn snapshot(&mut self, now: i64) -> Snapshot {
        let reading = match &mut self.meter {
            Meter::Mock(mock) => mock.latest().cloned(),
            Meter::SlimmeLezer(link) => {
                let (reading, events) = link.latest(now);
                self.dispatch(events, now);
                reading
            }
        };
        let reading = reading.as_ref();
        let fresh = self.fresh(reading, now);
        let evaluation = self.evaluate(reading, now);
        let mut inverter = self.inverter_snapshot(now);
        let household = self.household_use(reading, fresh, evaluation.sun, &inverter);
        inverter.household_use_w = household;
        let (night_baseline_w, night_baseline_learned, night_baseline_samples) = if self.config.is_mock() {
            (None, false, 0)
        } else {
            let watts = if evaluation.sun == Sun::Down {
                reading.filter(|_| fresh).map(|r| r.grid_import_w)
            } else {
                household
            };
            let state = self.night_baseline.update(evaluation.sun, watts, reading.map(|r| r.timestamp));
            (Some(state.baseline_w), state.learned, state.samples)
        };
        let peak_forecast = self.forecast(reading, now);
        let times = self.sun_times(now);
        let mock = self.mock();
        let diagnostics = match &self.meter {
            Meter::Mock(_) => MeterStats { simulated: true, ..MeterStats::default() },
            Meter::SlimmeLezer(link) => link.stats().clone(),
        };
        let presets = if mock.is_some() {
            simulation_presets(&self.config.decision, &self.config.appliances[&evaluation.appliance])
        } else {
            Vec::new()
        };
        Snapshot {
            peak_forecast,
            daily_energy: self.daily_energy.current(fresh),
            simulated: self.config.is_mock(),
            scenario: mock.map(|m| m.scenario().to_string()),
            simulated_w: mock.filter(|m| m.scenario() == "custom").and_then(MockProvider::watts),
            presets,
            stable_duration_seconds: self.config.decision.stable_duration_seconds,
            location_configured: self.config.location.latitude.is_some(),
            sunrise: times.sunrise,
            sunset: times.sunset,
            diagnostics,
            meter_address: (!self.config.is_mock())
                .then(|| format!("{}:{}", self.config.energy_source.host, self.config.energy_source.port)),
            inverter_address: self
                .config
                .inverter_enabled()
                .then(|| format!("{}:{}", self.config.inverter_source.host, self.config.inverter_source.port)),
            inverter,
            night_baseline_w,
            night_baseline_learned,
            night_baseline_samples,
            reading_at: reading.map(|r| r.timestamp),
            checked_at: now,
            evaluation,
        }
    }

    pub fn select_appliance(&mut self, id: &str) -> Result<(), ControlError> {
        self.engine.select_appliance(id).map_err(|_| ControlError("Invalid appliance or simulation scenario".into()))
    }

    pub fn set_scenario(&mut self, scenario: &str, now: i64) -> Result<(), ControlError> {
        let Meter::Mock(mock) = &mut self.meter else {
            return Err(ControlError("Invalid appliance or simulation scenario".into()));
        };
        if crate::mock::mock_scenario(scenario).is_none() {
            return Err(ControlError("Invalid appliance or simulation scenario".into()));
        }
        self.engine.reset();
        self.forecaster.reset();
        match mock.set_scenario(scenario, now) {
            Ok(Some(reading)) => self.observe(&reading, now),
            Ok(None) => self.invalidate(),
            Err(error) => return Err(ControlError(error.into())),
        }
        Ok(())
    }

    pub fn set_simulated_watts(&mut self, watts: i64, now: i64) -> Result<(), ControlError> {
        let Meter::Mock(mock) = &mut self.meter else {
            return Err(ControlError("Choose Simulation (DEMO) first.".into()));
        };
        let watts = simulated_watts(watts).map_err(|e| ControlError(e.into()))?;
        // Changing a custom value is like the meter changing: stability and start hysteresis carry on.
        let was_custom = mock.scenario() == "custom";
        let reading = mock.set_power(watts, now);
        if !was_custom {
            self.engine.reset();
        }
        if let Some(reading) = reading {
            self.observe(&reading, now);
        }
        Ok(())
    }
}

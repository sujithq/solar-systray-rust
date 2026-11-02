//! Recorded atomic night samples, isolated from the live decision engine.

use crate::config::SolarConfig;
use crate::home_assistant::belgian_meter_calendar;
use crate::night_baseline::{DEFAULT_NIGHT_BASELINE_W, MIN_NIGHT_SAMPLES, median};
use crate::runtime::Runtime;
use crate::service::{lock, pause};
use crate::sun::{Sun, sun_state, sun_times};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::sync::{Mutex, atomic::AtomicBool};
use std::time::Duration;

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;
pub const ATOMIC_PROVENANCE: &str = "HA recorder atomic grid import; first sample per UTC minute median";
pub const REPORTED_PROVENANCE: &str = "HA reported grid import estimate; held UTC-minute-start median; full recorded availability, physical acquisition unverified";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NightWindow {
    pub start: i64,
    pub end: i64,
}

/// Only full sunset-to-sunrise nights are eligible, never a night still in progress.
pub fn completed_window(config: &SolarConfig, now: i64) -> Option<NightWindow> {
    let mut latest = None;
    for days in (1..=3).rev() {
        let sunset = sun_times(&config.location, now - days * DAY).sunset?;
        let sunrise = sun_times(&config.location, sunset + MINUTE).sunrise?;
        if sunset < sunrise && sunrise <= now && sunrise - sunset <= DAY {
            // Solar crossings are rounded to the minute. Exclude both boundary
            // minutes so rounding cannot admit daytime readings.
            latest = Some(NightWindow { start: sunset + MINUTE, end: sunrise - MINUTE });
        }
    }
    latest
}

pub fn refresh_minute(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 5 || bytes[2] != b':' || ![0, 1, 3, 4].iter().all(|i| bytes[*i].is_ascii_digit()) {
        return None;
    }
    let hour = text[..2].parse::<i64>().ok()?;
    let minute = text[3..].parse::<i64>().ok()?;
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

/// Startup always catches up. The local date prevents a repeated DST hour running twice.
#[derive(Default)]
pub struct Schedule {
    last_day: Option<i64>,
}

impl Schedule {
    pub fn due(&mut self, now: i64, refresh_at: &str) -> bool {
        let Some(calendar) = belgian_meter_calendar(now) else { return false };
        let Some(minute) = refresh_minute(refresh_at) else { return false };
        let due = self.last_day.is_none()
            || (calendar.minute >= minute && self.last_day.is_some_and(|day| day < calendar.day));
        if due {
            self.last_day = Some(if calendar.minute < minute { calendar.day - 1 } else { calendar.day });
        }
        due
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedBaseline {
    pub window: NightWindow,
    pub calculated_at: i64,
    pub watts: f64,
    pub samples: usize,
    pub unit: String,
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryStatus {
    pub baseline: Option<RecordedBaseline>,
    pub issue: Option<String>,
}

impl Default for HistoryStatus {
    fn default() -> Self {
        Self { baseline: None, issue: Some("history-awaiting-startup".into()) }
    }
}

impl HistoryStatus {
    pub fn watts(&self) -> f64 {
        self.baseline.as_ref().map_or(DEFAULT_NIGHT_BASELINE_W, |baseline| baseline.watts)
    }
}

// Keep source/config identity in the private cache only; never include credentials.
fn identity(config: &SolarConfig) -> Value {
    json!({
        "endpoint": config.home_assistant.endpoint,
        "entity": config.home_assistant.atomic_grid,
        "reportedEntity": config.home_assistant.grid_import,
        "tokenReference": config.home_assistant.token_env,
        "latitude": config.location.latitude,
        "longitude": config.location.longitude,
        "refreshAt": config.home_assistant.night_baseline_refresh_at,
        "timezone": "Europe/Brussels",
        "statistic": "atomic-first-or-reported-held-utc-minute-median-v2"
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    version: u8,
    identity: Value,
    baseline: RecordedBaseline,
}

pub fn load_cache(path: &Path, config: &SolarConfig, now: i64) -> Result<Option<RecordedBaseline>, &'static str> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("history-cache-read"),
    };
    let mut bytes = Vec::new();
    file.take(8193).read_to_end(&mut bytes).map_err(|_| "history-cache-read")?;
    if bytes.len() > 8192 {
        return Err("history-cache-invalid");
    }
    let cache: Cache = serde_json::from_slice(&bytes).map_err(|_| "history-cache-invalid")?;
    let b = &cache.baseline;
    if cache.version != 1 || cache.identity != identity(config) {
        return Err("history-cache-source-mismatch");
    }
    if !b.watts.is_finite()
        || !(0.0..=1_000_000.0).contains(&b.watts)
        || !(MIN_NIGHT_SAMPLES..=1441).contains(&b.samples)
        || b.unit != "W"
        || ![ATOMIC_PROVENANCE, REPORTED_PROVENANCE].contains(&b.provenance.as_str())
        || b.window.start <= 0
        || b.window.end <= b.window.start
        || b.window.end - b.window.start > DAY
        || b.window.start.rem_euclid(MINUTE) != 0
        || b.window.end.rem_euclid(MINUTE) != 0
        || b.samples as i64 != (b.window.end - b.window.start) / MINUTE
        || b.window.end > b.calculated_at
        || b.calculated_at > now
    {
        return Err("history-cache-invalid");
    }
    Ok(Some(cache.baseline))
}

pub fn save_cache(path: &Path, config: &SolarConfig, baseline: &RecordedBaseline) -> Result<(), &'static str> {
    let cache = Cache { version: 1, identity: identity(config), baseline: baseline.clone() };
    let bytes = serde_json::to_vec(&cache).map_err(|_| "history-cache-write")?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes).map_err(|_| "history-cache-write")?;
    std::fs::rename(&temporary, path).map_err(|_| "history-cache-write")
}

/// Preserve one real reading per minute, without interpolating or averaging gaps.
pub struct NightSamples {
    window: NightWindow,
    minute_values: BTreeMap<i64, f64>,
    previous: Option<(String, u64, i64, i64)>,
    first_at: Option<i64>,
    issue: Option<&'static str>,
    records: usize,
    retired_sessions: HashSet<String>,
}

impl NightSamples {
    pub fn new(window: NightWindow) -> Self {
        Self {
            window,
            minute_values: BTreeMap::new(),
            previous: None,
            first_at: None,
            issue: None,
            records: 0,
            retired_sessions: HashSet::new(),
        }
    }

    pub(crate) fn add(&mut self, session: String, sequence: u64, meter_at: i64, received_at: i64, import_w: f64) {
        self.records += 1;
        if self.records > 100_000 {
            self.reject("history-record-limit");
            return;
        }
        if meter_at < self.window.start || meter_at >= self.window.end {
            return;
        }
        if self.retired_sessions.contains(&session) {
            self.reject("history-gap-or-replay");
            return;
        }
        if let Some((old_session, old_sequence, old_meter, old_received)) = &self.previous
            && (meter_at <= *old_meter
                || received_at <= *old_received
                || meter_at - *old_meter >= 30_000
                || (session == *old_session && sequence <= *old_sequence))
        {
            self.reject("history-gap-or-replay");
            return;
        }
        if let Some((old_session, _, _, _)) = &self.previous
            && old_session != &session
        {
            if self.retired_sessions.len() >= 64 {
                self.reject("history-session-limit");
                return;
            }
            self.retired_sessions.insert(old_session.clone());
        }
        self.first_at.get_or_insert(meter_at);
        self.minute_values.entry(meter_at.div_euclid(MINUTE)).or_insert(import_w);
        self.previous = Some((session, sequence, meter_at, received_at));
    }

    pub(crate) fn reject(&mut self, issue: &'static str) {
        self.issue.get_or_insert(issue);
    }

    pub fn finish(self, now: i64) -> Result<RecordedBaseline, &'static str> {
        if let Some(issue) = self.issue {
            return Err(issue);
        }
        let (Some(first), Some((_, _, last, _))) = (self.first_at, self.previous) else {
            return Err("history-insufficient-minute-samples");
        };
        if first - self.window.start >= 30_000 || self.window.end - last >= 30_000 {
            return Err("history-incomplete-night");
        }
        if self.minute_values.len() < MIN_NIGHT_SAMPLES {
            return Err("history-insufficient-minute-samples");
        }
        let values: Vec<_> = self.minute_values.into_values().collect();
        let watts = median(&values);
        Ok(RecordedBaseline {
            window: self.window,
            calculated_at: now,
            watts,
            samples: values.len(),
            unit: "W".into(),
            provenance: ATOMIC_PROVENANCE.into(),
        })
    }
}

/// Sample HA's reported held state at minute starts, not new physical readings.
pub(crate) struct ReportedNightSamples {
    window: NightWindow,
    entity: String,
    values: Vec<f64>,
    held: Option<f64>,
    through: i64,
    records: usize,
}

impl ReportedNightSamples {
    pub(crate) fn new(window: NightWindow, entity: String) -> Result<Self, &'static str> {
        if entity.is_empty()
            || window.start.rem_euclid(MINUTE) != 0
            || window.end.rem_euclid(MINUTE) != 0
            || window.end <= window.start
            || window.end - window.start > DAY
        {
            return Err("reported-night-window-or-mapping");
        }
        Ok(Self { window, entity, values: Vec::new(), held: None, through: window.start, records: 0 })
    }

    pub(crate) fn append(&mut self, result: &Value, start: i64, end: i64) -> Result<(), &'static str> {
        if start != self.through || end <= start || end > self.window.end {
            return Err("reported-night-window");
        }
        let object = result.as_object().ok_or("reported-night-shape")?;
        if object.keys().any(|id| id != &self.entity) {
            return Err("reported-night-entity");
        }
        let rows = object.get(&self.entity).and_then(Value::as_array).ok_or("reported-night-missing-boundary")?;
        if rows.is_empty() || rows.len() > 10_000 {
            return Err("reported-night-missing-boundary");
        }
        let mut next_minute = start;
        let mut previous_at = None;
        for (index, row) in rows.iter().enumerate() {
            self.records += 1;
            if self.records > 100_000 {
                return Err("reported-night-record-limit");
            }
            let at = row["lu"].as_f64().filter(|v| v.is_finite()).ok_or("reported-night-time")? * 1000.0;
            if at < start as f64 || at >= end as f64 || previous_at.is_some_and(|last| at <= last) {
                return Err("reported-night-time");
            }
            let state = row["s"].as_str().ok_or("reported-night-invalid-power")?;
            if matches!(state, "unknown" | "unavailable") {
                return Err("reported-night-unavailable-coverage");
            }
            let attributes = &row["a"];
            // Only raw power is eligible, never cumulative energy or statistics.
            if attributes["unit_of_measurement"] != "kW"
                || attributes["device_class"] != "power"
                || attributes["state_class"] != "measurement"
            {
                return Err("reported-night-unit-or-class");
            }
            let watts = state.parse::<f64>().map_err(|_| "reported-night-invalid-power")? * 1000.0;
            if !watts.is_finite() || !(0.0..=1_000_000.0).contains(&watts) {
                return Err("reported-night-invalid-power");
            }
            if index == 0 {
                if (at - start as f64).abs() > 0.01 {
                    return Err("reported-night-missing-boundary");
                }
                if self.held.is_some_and(|held| held != watts) {
                    return Err("reported-night-boundary-mismatch");
                }
            } else {
                while (next_minute as f64) < at {
                    self.values.push(self.held.ok_or("reported-night-missing-boundary")?);
                    next_minute += MINUTE;
                }
            }
            self.held = Some(watts);
            previous_at = Some(at);
        }
        while next_minute < end {
            self.values.push(self.held.ok_or("reported-night-missing-boundary")?);
            next_minute += MINUTE;
        }
        self.through = end;
        Ok(())
    }

    pub(crate) fn finish(self, now: i64) -> Result<RecordedBaseline, &'static str> {
        if self.through != self.window.end
            || self.values.len() < MIN_NIGHT_SAMPLES
            || self.values.len() as i64 != (self.window.end - self.window.start) / MINUTE
        {
            return Err("reported-night-incomplete-coverage");
        }
        Ok(RecordedBaseline {
            window: self.window,
            calculated_at: now,
            watts: median(&self.values),
            samples: self.values.len(),
            unit: "W".into(),
            provenance: REPORTED_PROVENANCE.into(),
        })
    }
}

pub(crate) fn worker(
    runtime: &Mutex<Runtime>,
    stop: &AtomicBool,
    path: &Path,
    config: &SolarConfig,
    mut status: HistoryStatus,
) {
    let mut schedule = Schedule::default();
    loop {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let now = crate::now_ms();
        if schedule.due(now, &config.home_assistant.night_baseline_refresh_at) {
            let result = (|| {
                let window = completed_window(config, now).ok_or("history-no-completed-night-or-location")?;
                if status.baseline.as_ref().is_some_and(|b| b.window == window) {
                    return Ok(None);
                }
                lock(runtime).set_baseline_history(HistoryStatus {
                    baseline: status.baseline.clone(),
                    issue: Some("history-loading".into()),
                });
                let baseline = match crate::home_assistant::recorded_night(config, stop, window) {
                    Ok(baseline) => baseline,
                    Err(issue) if reported_fallback_eligible(issue) => {
                        if stop.load(std::sync::atomic::Ordering::Relaxed) {
                            return Err("history-cancelled");
                        }
                        lock(runtime).set_baseline_history(HistoryStatus {
                            baseline: status.baseline.clone(),
                            issue: Some(format!("reported-night-loading-after-{issue}")),
                        });
                        crate::home_assistant::recorded_reported_night(config, stop, window)?
                    }
                    Err(issue) => return Err(issue),
                };
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err("history-cancelled");
                }
                save_cache(path, config, &baseline)?;
                Ok(Some(baseline))
            })();
            match result {
                Ok(baseline) => {
                    if let Some(baseline) = baseline {
                        status.baseline = Some(baseline);
                    }
                    status.issue = None;
                }
                Err(issue) => status.issue = Some(issue.into()),
            }
            lock(runtime).set_baseline_history(status.clone());
        }
        if !pause(stop, Duration::from_secs(30)) {
            return;
        }
    }
}

fn reported_fallback_eligible(issue: &str) -> bool {
    matches!(
        issue,
        "history-missing-acquisition"
            | "history-invalid-acquisition"
            | "history-invalid-atomic-sample"
            | "history-insufficient-minute-samples"
            | "history-incomplete-night"
            | "history-gap-or-replay"
            | "history-recorder-time-mismatch"
    )
}

pub(crate) fn validate_night_sample(config: &SolarConfig, at: i64) -> bool {
    sun_state(&config.location, at).state == Sun::Down
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reported_row(at: f64, value: &str) -> Value {
        json!({"lu":at/1000.0,"s":value,"a":{
            "unit_of_measurement":"kW","device_class":"power","state_class":"measurement"
        }})
    }

    #[test]
    fn reported_power_uses_held_minute_starts_not_event_median_or_hourly_means() {
        let start = 1_800_000_000_000;
        let window = NightWindow { start, end: start + 40 * MINUTE };
        let mut samples = ReportedNightSamples::new(window, "sensor.grid".into()).unwrap();
        let mut from = start;
        while from < window.end {
            let end = (from + 15 * MINUTE).min(window.end);
            let rows = if from == start {
                json!([reported_row(from as f64, "0.300")])
            } else if from == start + 15 * MINUTE {
                json!([reported_row(from as f64, "0.300"), reported_row((start + 20 * MINUTE) as f64 - 0.25, "0.320")])
            } else {
                json!([reported_row(from as f64, "0.320")])
            };
            samples.append(&json!({"sensor.grid":rows}), from, end).unwrap();
            from = end;
        }
        let baseline = samples.finish(window.end + 1).unwrap();
        assert_eq!(baseline.samples, 40);
        assert_eq!(baseline.watts, 310.0);
        assert_eq!(baseline.provenance, REPORTED_PROVENANCE);
    }

    #[test]
    fn reported_power_rejects_outages_missing_coverage_and_nonpower_inputs() {
        let start = 1_800_000_000_000;
        let window = NightWindow { start, end: start + 40 * MINUTE };
        for fault in ["unknown", "unavailable", "negative", "unit", "missing", "order"] {
            let mut rows = json!([reported_row(start as f64, "0.300")]);
            match fault {
                "unknown" | "unavailable" => {
                    rows.as_array_mut().unwrap().push(reported_row((start + MINUTE) as f64, fault))
                }
                "negative" => rows[0]["s"] = json!("-0.1"),
                "unit" => rows[0]["a"]["unit_of_measurement"] = json!("kWh"),
                "missing" => rows[0]["lu"] = json!((start + MINUTE) as f64 / 1000.0),
                _ => rows.as_array_mut().unwrap().push(reported_row(start as f64, "0.300")),
            }
            let mut samples = ReportedNightSamples::new(window, "sensor.grid".into()).unwrap();
            assert!(samples.append(&json!({"sensor.grid":rows}), start, start + 15 * MINUTE).is_err(), "{fault}");
        }
        let mut samples = ReportedNightSamples::new(window, "sensor.grid".into()).unwrap();
        samples
            .append(&json!({"sensor.grid":[reported_row(start as f64,"0.300")]}), start, start + 15 * MINUTE)
            .unwrap();
        assert_eq!(samples.finish(window.end + 1), Err("reported-night-incomplete-coverage"));
        assert!(!reported_fallback_eligible("history-auth-rejected"));
        assert!(!reported_fallback_eligible("history-response-timeout"));
        assert!(reported_fallback_eligible("history-missing-acquisition"));
    }

    #[test]
    fn reported_night_counts_elapsed_utc_minutes_across_both_brussels_dst_transitions() {
        let config = SolarConfig::from_json(&json!({
            "location":{"latitude":50.85,"longitude":4.35}
        }))
        .unwrap();
        for date in ["2026-03-29T12:00:00Z", "2026-10-25T12:00:00Z"] {
            let now = chrono::DateTime::parse_from_rfc3339(date).unwrap().timestamp_millis();
            let window = completed_window(&config, now).unwrap();
            let mut samples = ReportedNightSamples::new(window, "sensor.grid".into()).unwrap();
            let mut start = window.start;
            while start < window.end {
                let end = (start + 15 * MINUTE).min(window.end);
                samples.append(&json!({"sensor.grid":[reported_row(start as f64,"0.250")]}), start, end).unwrap();
                start = end;
            }
            let baseline = samples.finish(now).unwrap();
            assert_eq!(baseline.samples as i64, (window.end - window.start) / MINUTE);
            assert_eq!(baseline.watts, 250.0);
        }
    }

    fn samples(step: i64, gap: bool) -> NightSamples {
        let start = 1_800_000_000_000;
        let mut samples = NightSamples::new(NightWindow { start, end: start + 40 * MINUTE });
        for offset in (0..40 * MINUTE).step_by(step as usize) {
            if gap && (10 * MINUTE..12 * MINUTE).contains(&offset) {
                continue;
            }
            let minute = offset / MINUTE;
            let watts = if offset % MINUTE == 0 { 300.0 + (minute % 2) as f64 * 20.0 } else { 1000.0 };
            samples.add("fixture-session".into(), offset as u64 + 1, start + offset, start + offset, watts);
        }
        samples
    }

    #[test]
    fn recorded_statistic_is_median_of_real_first_minute_samples_not_all_readings() {
        let baseline = samples(10_000, false).finish(1_800_010_000_000).unwrap();
        assert_eq!(baseline.watts, 310.0);
        assert_eq!(baseline.samples, 40);
    }

    #[test]
    fn gaps_and_five_minute_statistics_cannot_qualify_as_minute_coverage() {
        assert_eq!(samples(10_000, true).finish(1_800_010_000_000), Err("history-gap-or-replay"));
        assert_eq!(samples(300_000, false).finish(1_800_010_000_000), Err("history-gap-or-replay"));
        let mut unavailable = samples(10_000, false);
        unavailable.reject("history-invalid-atomic-sample");
        assert_eq!(unavailable.finish(1_800_010_000_000), Err("history-invalid-atomic-sample"));
    }
}

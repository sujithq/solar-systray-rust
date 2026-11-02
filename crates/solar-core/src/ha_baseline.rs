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
        "tokenReference": config.home_assistant.token_env,
        "latitude": config.location.latitude,
        "longitude": config.location.longitude,
        "refreshAt": config.home_assistant.night_baseline_refresh_at,
        "timezone": "Europe/Brussels",
        "statistic": "first-atomic-grid-import-per-utc-minute-median-v1"
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
        || b.provenance != "HA recorder atomic grid import; first sample per UTC minute median"
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
            provenance: "HA recorder atomic grid import; first sample per UTC minute median".into(),
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
                if config.home_assistant.atomic_grid.is_empty() {
                    return Err("history-needs-atomic-grid");
                }
                let window = completed_window(config, now).ok_or("history-no-completed-night-or-location")?;
                if status.baseline.as_ref().is_some_and(|b| b.window == window) {
                    return Ok(None);
                }
                lock(runtime).set_baseline_history(HistoryStatus {
                    baseline: status.baseline.clone(),
                    issue: Some("history-loading".into()),
                });
                let baseline = crate::home_assistant::recorded_night(config, stop, window)?;
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

pub(crate) fn validate_night_sample(config: &SolarConfig, at: i64) -> bool {
    sun_state(&config.location, at).state == Sun::Down
}

#[cfg(test)]
mod tests {
    use super::*;

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

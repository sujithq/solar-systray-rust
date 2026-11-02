//! Recorded cumulative counters never enter the live measurement/decision path.

use crate::daily_energy::DailyEnergyResult;
use crate::home_assistant::belgian_meter_calendar;
use crate::reading::EnergyTotals;
use crate::service::{lock, pause};
use crate::{Runtime, SolarConfig};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::Read;
use std::path::Path;
use std::sync::{Mutex, atomic::AtomicBool};
use std::time::{Duration, Instant};

const GAP: i64 = 30_000;
const RETRY: i64 = 15 * 60_000;
const MAX_PENDING: usize = 10_000;
const PROVENANCE: &str = "HA recorder/live atomic four-counter chain; no interpolation";

/// UTC instants of Brussels-local midnight and the following midnight (23/24/25h).
pub fn day_bounds(at: i64) -> Option<(i64, i64)> {
    let day = belgian_meter_calendar(at)?.day;
    let midnight = |day: i64| {
        [60, 120].into_iter().find_map(|offset| {
            let utc = day * 86_400_000 - offset * 60_000;
            let c = belgian_meter_calendar(utc)?;
            (c.day == day && c.minute == 0).then_some(utc)
        })
    };
    Some((midnight(day)?, midnight(day + 1)?))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CounterSample {
    pub session: String,
    pub sequence: u64,
    pub meter_at: i64,
    pub received_at: i64,
    pub totals: EnergyTotals,
}

impl CounterSample {
    fn values(&self) -> [i64; 4] {
        let t = &self.totals;
        [t.import_tariff1_wh, t.import_tariff2_wh, t.export_tariff1_wh, t.export_tariff2_wh]
    }

    fn valid(&self) -> bool {
        !self.session.is_empty()
            && self.session.len() <= 128
            && self.session.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && (1..=9_007_199_254_740_991).contains(&self.sequence)
            && self.meter_at > 0
            && self.received_at > 0
            && self.meter_at.abs_diff(self.received_at) < GAP as u64
            && self.meter_at - self.received_at <= 5000
            && self.values().iter().all(|v| (0..=999_999_999).contains(v))
    }

    fn follows(&self, old: &Self) -> Result<(), &'static str> {
        if self.values().iter().zip(old.values()).any(|(new, old)| *new < old) {
            return Err("daily-history-counter-decreased");
        }
        if self.session != old.session {
            return Err("daily-history-session-changed");
        }
        if self.sequence <= old.sequence || self.meter_at <= old.meter_at || self.received_at <= old.received_at {
            return Err("daily-history-replay");
        }
        if self.meter_at - old.meter_at >= GAP || self.received_at - old.received_at >= GAP {
            return Err("daily-history-gap");
        }
        if self.sequence != old.sequence + 1 {
            return Err("daily-history-sequence-gap");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CounterBaseline {
    pub first: CounterSample,
    pub last: CounterSample,
}

impl CounterBaseline {
    fn append(&mut self, sample: CounterSample) -> Result<(), &'static str> {
        if sample == self.last {
            return Ok(());
        }
        sample.follows(&self.last)?;
        self.last = sample;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyCoverage {
    pub midnight: bool,
    pub since: i64,
    pub through: i64,
    pub day_start: i64,
    pub day_end: i64,
    pub provenance: String,
    pub history_issue: Option<String>,
}

#[derive(Clone)]
pub(crate) struct HistoryRequest {
    id: u64,
    pub anchor: CounterSample,
    pub seed: Option<CounterBaseline>,
}

impl HistoryRequest {
    pub fn window(&self) -> Option<(i64, i64)> {
        let (midnight, _) = day_bounds(self.anchor.meter_at)?;
        Some((self.seed.as_ref().map_or(midnight - GAP, |s| s.last.received_at - 1), self.anchor.received_at + 1))
    }
}

pub(crate) struct HistorySamples {
    request: HistoryRequest,
    chain: Option<CounterBaseline>,
    records: usize,
}

impl HistorySamples {
    pub fn new(request: HistoryRequest) -> Self {
        Self { chain: request.seed.clone(), request, records: 0 }
    }

    pub fn add(&mut self, sample: CounterSample) -> Result<(), &'static str> {
        self.records += 1;
        if self.records > 100_000 {
            return Err("daily-history-record-limit");
        }
        let (midnight, _) = day_bounds(self.request.anchor.meter_at).ok_or("daily-history-calendar")?;
        if sample.meter_at < midnight {
            return Ok(());
        }
        if sample.meter_at > self.request.anchor.meter_at {
            return Err("daily-history-anchor-mismatch");
        }
        if !sample.valid() {
            return Err("daily-history-invalid-counters");
        }
        if let Some(chain) = &mut self.chain {
            chain.append(sample)?;
        } else {
            if sample.meter_at != midnight {
                return Err("daily-history-no-midnight-sample");
            }
            self.chain = Some(CounterBaseline { first: sample.clone(), last: sample });
        }
        Ok(())
    }

    pub fn finish(self) -> Result<CounterBaseline, &'static str> {
        let chain = self.chain.ok_or("daily-history-no-midnight-sample")?;
        if chain.last != self.request.anchor {
            return Err("daily-history-anchor-not-recorded");
        }
        Ok(chain)
    }
}

#[derive(Default)]
pub struct HaDaily {
    chain: Option<CounterBaseline>,
    seed: Option<CounterBaseline>,
    live: Option<CounterSample>,
    pending: Option<HistoryRequest>,
    buffered: Vec<CounterSample>,
    next_id: u64,
    last_request: Option<i64>,
    history_issue: Option<String>,
    invalid_day: Option<i64>,
}

impl HaDaily {
    pub(crate) fn restore(&mut self, baseline: CounterBaseline) {
        self.seed = Some(baseline);
        self.history_issue = Some("daily-history-cache-awaiting-bridge".into());
    }

    pub(crate) fn observe(&mut self, sample: CounterSample) {
        let Some((day, end)) = day_bounds(sample.meter_at) else {
            self.invalidate_live("daily-history-calendar");
            return;
        };
        if !sample.valid() {
            self.invalidate_live("daily-history-invalid-counters");
            return;
        }
        if self.live.as_ref() == Some(&sample) {
            return;
        }
        if self.seed.as_ref().is_some_and(|seed| day_bounds(seed.first.meter_at) != Some((day, end))) {
            self.seed = None;
        }
        if self.live.as_ref().is_some_and(|old| day_bounds(old.meter_at) != Some((day, end)))
            || self.chain.as_ref().is_some_and(|chain| day_bounds(chain.first.meter_at) != Some((day, end)))
            || self.invalid_day.is_some_and(|invalid| invalid != day)
        {
            self.chain = None;
            self.seed = None;
            self.pending = None;
            self.buffered.clear();
            self.last_request = None;
            self.invalid_day = None;
            self.history_issue = None;
        }
        if let Some(request) = &self.pending
            && sample.meter_at > request.anchor.meter_at
        {
            if self.buffered.len() >= MAX_PENDING {
                self.pending = None;
                self.buffered.clear();
                self.history_issue = Some("daily-history-buffer-limit".into());
            } else {
                self.buffered.push(sample.clone());
            }
        }
        if let Some(chain) = &mut self.chain
            && let Err(issue) = chain.append(sample.clone())
        {
            if self.seed.is_none() {
                self.seed = Some(chain.clone());
            }
            if issue == "daily-history-counter-decreased" {
                self.invalid_day = Some(day);
                self.seed = None;
                self.pending = None;
                self.buffered.clear();
            }
            self.history_issue = Some(issue.into());
            self.chain = None;
        }
        if self.invalid_day != Some(day) && self.chain.is_none() {
            self.chain = Some(CounterBaseline { first: sample.clone(), last: sample.clone() });
        }
        self.live = Some(sample);
    }

    pub(crate) fn invalidate_live(&mut self, issue: &'static str) {
        if self.seed.is_none() {
            self.seed = self.chain.clone();
        }
        self.chain = None;
        self.live = None;
        self.pending = None;
        self.buffered.clear();
        self.last_request = None;
        self.history_issue = Some(issue.into());
    }

    pub(crate) fn request(&mut self, now: i64) -> Option<HistoryRequest> {
        let live = self.live.as_ref()?;
        let (day, end) = day_bounds(live.meter_at)?;
        if now < live.received_at
            || now - live.received_at >= GAP
            || self.invalid_day == Some(day)
            || self.pending.is_some()
            || self.chain.as_ref().is_some_and(|chain| chain.first.meter_at == day)
            || self.last_request.is_some_and(|last| now - last < RETRY)
        {
            return None;
        }
        self.next_id += 1;
        let seed = self
            .seed
            .as_ref()
            .filter(|seed| day_bounds(seed.first.meter_at) == Some((day, end)) && seed.last.meter_at <= live.meter_at)
            .cloned();
        let request = HistoryRequest { id: self.next_id, anchor: live.clone(), seed };
        self.pending = Some(request.clone());
        self.buffered.clear();
        self.last_request = Some(now);
        self.history_issue = Some("daily-history-loading".into());
        Some(request)
    }

    pub(crate) fn complete(&mut self, request: &HistoryRequest, result: Result<CounterBaseline, &'static str>) {
        if self.pending.as_ref().is_none_or(|pending| pending.id != request.id) {
            return;
        }
        self.pending = None;
        let result = result.and_then(|mut chain| {
            if chain.last != request.anchor || day_bounds(chain.first.meter_at) != day_bounds(request.anchor.meter_at) {
                return Err("daily-history-anchor-mismatch");
            }
            for point in self.buffered.drain(..) {
                chain.append(point)?;
            }
            if self.live.as_ref() != Some(&chain.last) {
                return Err("daily-history-live-race");
            }
            Ok(chain)
        });
        self.buffered.clear();
        match result {
            Ok(chain) => {
                self.seed = None;
                self.chain = Some(chain);
                self.history_issue = None;
            }
            Err(issue) => {
                if matches!(
                    issue,
                    "daily-history-counter-decreased"
                        | "daily-history-session-changed"
                        | "daily-history-gap"
                        | "daily-history-sequence-gap"
                        | "daily-history-replay"
                        | "daily-history-missing-atomic-counters"
                ) {
                    self.seed = None;
                }
                self.history_issue = Some(issue.into());
            }
        }
    }

    pub fn coverage(&self) -> Option<DailyCoverage> {
        let chain = self.chain.as_ref()?;
        let (day_start, day_end) = day_bounds(chain.first.meter_at)?;
        Some(DailyCoverage {
            midnight: chain.first.meter_at == day_start,
            since: chain.first.meter_at,
            through: chain.last.meter_at,
            day_start,
            day_end,
            provenance: PROVENANCE.into(),
            history_issue: self.history_issue.clone(),
        })
    }

    pub fn history_issue(&self) -> Option<&str> {
        self.history_issue.as_deref()
    }

    pub(crate) fn set_issue(&mut self, issue: &'static str) {
        self.history_issue = Some(issue.into());
    }

    pub(crate) fn baseline(&self) -> Option<CounterBaseline> {
        if self.invalid_day.is_some() { None } else { self.seed.clone().or_else(|| self.chain.clone()) }
    }

    pub fn current(&self, fresh: bool) -> DailyEnergyResult {
        if !fresh {
            return DailyEnergyResult::unavailable("stale");
        }
        let Some(chain) = &self.chain else {
            return DailyEnergyResult::unavailable(if self.invalid_day.is_some() {
                "counter-decreased"
            } else {
                "missing-or-invalid-totals"
            });
        };
        let first = chain.first.values();
        let last = chain.last.values();
        DailyEnergyResult {
            available: true,
            import_wh: Some(last[0] + last[1] - first[0] - first[1]),
            export_wh: Some(last[2] + last[3] - first[2] - first[3]),
            since_minute: belgian_meter_calendar(chain.first.meter_at).map(|c| c.minute),
            reason: "available".into(),
        }
    }
}

fn identity(config: &SolarConfig) -> Value {
    json!({"endpoint":config.home_assistant.endpoint, "entity":config.home_assistant.atomic_grid,
        "tokenReference":config.home_assistant.token_env, "timezone":"Europe/Brussels",
        "policy":"exact-midnight-four-counters-continuous-under-30s-v1"})
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    version: u8,
    identity: Value,
    provenance: String,
    unit: String,
    baseline: CounterBaseline,
}

pub fn load_cache(path: &Path, config: &SolarConfig, now: i64) -> Result<Option<CounterBaseline>, &'static str> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("daily-history-cache-read"),
    };
    let mut bytes = Vec::new();
    file.take(8193).read_to_end(&mut bytes).map_err(|_| "daily-history-cache-read")?;
    if bytes.len() > 8192 {
        return Err("daily-history-cache-invalid");
    }
    let cache: Cache = serde_json::from_slice(&bytes).map_err(|_| "daily-history-cache-invalid")?;
    if cache.version != 1 || cache.identity != identity(config) {
        return Err("daily-history-cache-source-mismatch");
    }
    let b = &cache.baseline;
    let bounds = day_bounds(now).ok_or("daily-history-cache-invalid")?;
    if cache.provenance != PROVENANCE
        || cache.unit != "Wh"
        || !b.first.valid()
        || !b.last.valid()
        || b.first.session != b.last.session
        || b.first.sequence > b.last.sequence
        || b.first.meter_at > b.last.meter_at
        || b.first.received_at > b.last.received_at
        || (b.first != b.last
            && (b.first.sequence == b.last.sequence
                || b.first.meter_at == b.last.meter_at
                || b.first.received_at == b.last.received_at))
        || b.first.values().iter().zip(b.last.values()).any(|(a, b)| *a > b)
        || day_bounds(b.first.meter_at) != Some(bounds)
        || day_bounds(b.last.meter_at) != Some(bounds)
        || b.last.received_at > now
        || b.last.meter_at > now
    {
        return Err("daily-history-cache-invalid");
    }
    Ok(Some(cache.baseline))
}

fn save_cache(path: &Path, config: &SolarConfig, baseline: &CounterBaseline) -> Result<(), &'static str> {
    let cache = Cache {
        version: 1,
        identity: identity(config),
        provenance: PROVENANCE.into(),
        unit: "Wh".into(),
        baseline: baseline.clone(),
    };
    let bytes = serde_json::to_vec(&cache).map_err(|_| "daily-history-cache-write")?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes).map_err(|_| "daily-history-cache-write")?;
    std::fs::rename(temporary, path).map_err(|_| "daily-history-cache-write")
}

pub(crate) fn worker(runtime: &Mutex<Runtime>, stop: &AtomicBool, path: Option<&Path>, config: &SolarConfig) {
    let mut saved = None;
    let mut reported_at = None;
    let status_path = path.map(|path| path.with_extension("reported-history-status.json"));
    let mut status = crate::ha_reported_energy::HistoryProgress::waiting(crate::now_ms());
    publish_progress(runtime, status_path.as_deref(), &mut status);
    // The live worker starts after this worker. Do not sleep 30 seconds or
    // begin a potentially 180-second atomic bridge ahead of the first report.
    if !await_reported_transport(runtime, stop) {
        status.phase = "cancelled".into();
        publish_progress(runtime, status_path.as_deref(), &mut status);
        return;
    }
    loop {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let now = crate::now_ms();
        if reported_at.is_none_or(|at| now - at >= 60_000) && lock(runtime).reported_history_ready(now) {
            lock(runtime).begin_reported_history();
            let end = now - 10_000;
            let started = Instant::now();
            status.query_started_at = Some(now);
            status.total_chunks = day_bounds(end).map_or(0, |(start, _)| ((end - start + 299_999) / 300_000) as u32);
            status.completed_chunks = 0;
            status.elapsed_ms = 0;
            status.phase = "starting".into();
            status.issue = None;
            publish_progress(runtime, status_path.as_deref(), &mut status);
            let result = crate::home_assistant::recorded_reported_energy(config, stop, end, |phase, chunks| {
                status.phase = phase.into();
                status.completed_chunks = chunks;
                status.elapsed_ms = started.elapsed().as_millis() as u64;
                publish_progress(runtime, status_path.as_deref(), &mut status);
            });
            status.elapsed_ms = started.elapsed().as_millis() as u64;
            status.issue = result.as_ref().err().map(|issue| (*issue).into());
            status.phase = if stop.load(std::sync::atomic::Ordering::Relaxed) {
                "cancelled"
            } else if result.is_ok() {
                "completed"
            } else {
                "failed"
            }
            .into();
            status.last_result = Some(crate::ha_reported_energy::HistoryOutcome {
                query_started_at: now,
                elapsed_ms: status.elapsed_ms,
                completed_chunks: status.completed_chunks,
                total_chunks: status.total_chunks,
                phase: status.phase.clone(),
                issue: status.issue.clone(),
            });
            publish_progress(runtime, status_path.as_deref(), &mut status);
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            lock(runtime).set_reported_energy(result, crate::now_ms());
            reported_at = Some(crate::now_ms());
        }
        let request = lock(runtime).daily_history_request(crate::now_ms());
        if let Some(request) = request {
            // Recorder commits can lag live events. Keep buffering real live
            // samples while allowing a bounded grace period, then still require
            // the exact anchor in history (time alone is never evidence).
            if !pause(stop, Duration::from_secs(10)) {
                return;
            }
            let result = crate::home_assistant::recorded_daily(config, stop, &request);
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            lock(runtime).complete_daily_history(&request, result);
        }
        if let Some(path) = path {
            let baseline = lock(runtime).daily_counter_baseline();
            if baseline != saved || baseline.is_none() {
                if let Some(baseline) = &baseline {
                    match save_cache(path, config, baseline) {
                        Ok(()) => saved = Some(baseline.clone()),
                        Err(issue) => lock(runtime).daily_history_issue(issue),
                    }
                } else {
                    saved = None;
                    // A reset must not leave a restorable earlier baseline.
                    if let Err(error) = std::fs::remove_file(path)
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        lock(runtime).daily_history_issue("daily-history-cache-write");
                    }
                }
            }
        }
        if !pause(stop, Duration::from_secs(30)) {
            return;
        }
    }
}

fn await_reported_transport(runtime: &Mutex<Runtime>, stop: &AtomicBool) -> bool {
    while !lock(runtime).reported_history_ready(crate::now_ms()) {
        if !pause(stop, Duration::from_millis(200)) {
            return false;
        }
    }
    !stop.load(std::sync::atomic::Ordering::Relaxed)
}

fn publish_progress(
    runtime: &Mutex<Runtime>,
    path: Option<&Path>,
    status: &mut crate::ha_reported_energy::HistoryProgress,
) {
    status.updated_at = crate::now_ms();
    status.persistence_issue = None;
    status.persistence_issue = path.and_then(|path| status.save(path).err()).map(String::from);
    lock(runtime).reported_history_progress(status.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_reported_query_waits_for_live_initialization_without_thirty_second_sleep_or_atomic_work() {
        let config = SolarConfig::from_json(&json!({
            "energySource":{"type":"home-assistant"},"homeAssistant":{"atomicGrid":"sensor.fixture"}
        }))
        .unwrap();
        let runtime = std::sync::Arc::new(Mutex::new(Runtime::new(config)));
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let (send, recv) = std::sync::mpsc::channel();
        let worker = {
            let runtime = runtime.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                send.send(await_reported_transport(&runtime, &stop)).unwrap();
            })
        };
        assert!(recv.recv_timeout(Duration::from_millis(50)).is_err());
        {
            let mut runtime = lock(&runtime);
            runtime.ha_connected();
            runtime.ha_ready();
        }
        assert!(recv.recv_timeout(Duration::from_secs(2)).unwrap(), "first query admitted promptly after readiness");
        worker.join().unwrap();
        assert!(lock(&runtime).snapshot(crate::now_ms()).reading_at.is_none(), "transport readiness is not freshness");
    }

    #[test]
    fn reported_status_file_preserves_terminal_failure_across_a_following_refresh() {
        let directory =
            std::env::temp_dir().join(format!("solar-history-progress-{}-{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("status.json");
        let config = SolarConfig::from_json(&json!({
            "energySource":{"type":"home-assistant"},"homeAssistant":{"atomicGrid":"sensor.fixture"}
        }))
        .unwrap();
        let runtime = Mutex::new(Runtime::new(config));
        let mut status = crate::ha_reported_energy::HistoryProgress::waiting(1000);
        status.phase = "querying".into();
        status.query_started_at = Some(2000);
        status.completed_chunks = 7;
        status.total_chunks = 176;
        status.last_result = Some(crate::ha_reported_energy::HistoryOutcome {
            query_started_at: 1500,
            elapsed_ms: 180_000,
            completed_chunks: 43,
            total_chunks: 175,
            phase: "failed".into(),
            issue: Some("history-response-timeout".into()),
        });
        publish_progress(&runtime, Some(&path), &mut status);
        let restored: crate::ha_reported_energy::HistoryProgress =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(restored, status);
        assert_eq!(restored.last_result.unwrap().completed_chunks, 43);
        assert!(status.persistence_issue.is_none());
        publish_progress(&runtime, Some(&directory.join("missing").join("status.json")), &mut status);
        assert_eq!(status.persistence_issue.as_deref(), Some("reported-history-status-write"));
        assert_eq!(
            lock(&runtime).snapshot(crate::now_ms()).reported_history.unwrap().persistence_issue,
            status.persistence_issue
        );
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    fn midnight() -> i64 {
        crate::utc_ms(2027, 1, 14, 23, 0, 0)
    }

    fn point(n: i64) -> CounterSample {
        CounterSample {
            session: "fixture-session".into(),
            sequence: (n + 1) as u64,
            meter_at: midnight() + n * 10_000,
            received_at: midnight() + n * 10_000 + 100,
            totals: EnergyTotals {
                import_tariff1_wh: 100_000 + n,
                import_tariff2_wh: 50_000,
                export_tariff1_wh: 20_000 + n * 2,
                export_tariff2_wh: 0,
            },
        }
    }

    fn history(request: &HistoryRequest, first: i64, last: i64) -> CounterBaseline {
        let mut history = HistorySamples::new(request.clone());
        for n in first..=last {
            history.add(point(n)).unwrap();
        }
        history.finish().unwrap()
    }

    #[test]
    fn startup_recovers_midnight_and_joins_live_updates_during_query() {
        let mut daily = HaDaily::default();
        daily.observe(point(60));
        assert_eq!(daily.current(true).import_wh, Some(0));
        assert!(!daily.coverage().unwrap().midnight);
        let request = daily.request(point(60).received_at).unwrap();
        daily.observe(point(61));
        daily.observe(point(62));
        daily.complete(&request, Ok(history(&request, 0, 60)));
        let coverage = daily.coverage().unwrap();
        assert!(coverage.midnight);
        assert_eq!(coverage.since, midnight());
        assert_eq!(coverage.through, point(62).meter_at);
        assert_eq!(daily.current(true).import_wh, Some(62));
        assert_eq!(daily.current(true).export_wh, Some(124));
        assert!(daily.request(point(62).received_at).is_none());
        assert!(!daily.current(false).available, "history never counts as live freshness");
    }

    #[test]
    fn later_arbitrary_sample_or_missing_anchor_cannot_claim_midnight() {
        let mut daily = HaDaily::default();
        daily.observe(point(60));
        let request = daily.request(point(60).received_at).unwrap();
        let mut samples = HistorySamples::new(request.clone());
        assert_eq!(samples.add(point(1)), Err("daily-history-no-midnight-sample"));
        let mut samples = HistorySamples::new(request.clone());
        for n in 0..60 {
            samples.add(point(n)).unwrap();
        }
        assert_eq!(samples.finish(), Err("daily-history-anchor-not-recorded"));
        daily.complete(&request, Err("daily-history-no-midnight-sample"));
        assert!(!daily.coverage().unwrap().midnight);
        assert_eq!(daily.coverage().unwrap().since, point(60).meter_at);
        assert_eq!(daily.current(true).import_wh, Some(0));
        assert!(daily.request(point(60).received_at + 1000).is_none(), "bounded retry");
    }

    #[test]
    fn unavailable_gap_replay_and_reset_cannot_bridge_counters() {
        for (mut second, issue) in [
            (point(3), "daily-history-gap"),
            (point(2), "daily-history-sequence-gap"),
            (point(0), "daily-history-replay"),
            (point(1), "daily-history-session-changed"),
            (point(1), "daily-history-counter-decreased"),
        ] {
            if issue.ends_with("session-changed") {
                second.session = "new-session".into();
            }
            if issue.ends_with("counter-decreased") {
                second.totals.import_tariff1_wh = 99_999;
            }
            if issue.ends_with("replay") {
                second.totals.export_tariff2_wh = 1;
            }
            let mut chain = CounterBaseline { first: point(0), last: point(0) };
            assert_eq!(chain.append(second), Err(issue));
        }
        let mut daily = HaDaily::default();
        daily.observe(point(0));
        daily.observe(point(1));
        daily.invalidate_live("daily-history-live-unavailable");
        assert!(!daily.current(true).available);
        daily.observe(point(4));
        assert!(!daily.coverage().unwrap().midnight);
        let request = daily.request(point(4).received_at).unwrap();
        // A recorded bridge can restore genuine continuity across a transport outage.
        daily.complete(&request, Ok(history(&request, 1, 4)));
        assert!(daily.coverage().unwrap().midnight);
    }

    #[test]
    fn declines_invalidate_day_and_late_async_results_never_cross_days_or_disconnects() {
        let mut daily = HaDaily::default();
        daily.observe(point(60));
        let request = daily.request(point(60).received_at).unwrap();
        let recovered = history(&request, 0, 60);
        let mut decreased = point(61);
        decreased.totals.import_tariff1_wh = 1;
        daily.observe(decreased);
        daily.complete(&request, Ok(recovered.clone()));
        assert_eq!(daily.current(true).reason, "counter-decreased");
        assert!(daily.baseline().is_none());
        daily.invalidate_live("daily-history-disconnected");
        let mut next = point(0);
        next.meter_at += 86_400_000;
        next.received_at += 86_400_000;
        next.session = "next-day".into();
        daily.observe(next);
        daily.complete(&request, Ok(recovered));
        assert_eq!(daily.current(true).import_wh, Some(0));
        assert_eq!(daily.coverage().unwrap().since, midnight() + 86_400_000);
    }

    #[test]
    fn cache_restores_partial_or_midnight_only_after_incremental_recorded_bridge() {
        let path = std::env::temp_dir().join(format!("solar-daily-fixture-{}.json", std::process::id()));
        let config = SolarConfig::from_json(&json!({"energySource":{"type":"home-assistant"},
                    "homeAssistant":{"atomicGrid":"sensor.fixture_atomic"}}))
        .unwrap();
        for first in [0, 10] {
            let baseline = CounterBaseline { first: point(first), last: point(60) };
            save_cache(&path, &config, &baseline).unwrap();
            let cached = load_cache(&path, &config, point(90).received_at).unwrap().unwrap();
            let mut daily = HaDaily::default();
            daily.restore(cached);
            assert!(!daily.current(true).available);
            daily.observe(point(90));
            let request = daily.request(point(90).received_at).unwrap();
            assert_eq!(request.window().unwrap().0, point(60).received_at - 1);
            daily.complete(&request, Ok(history(&request, 60, 90)));
            assert_eq!(daily.current(true).import_wh, Some(90 - first));
            assert_eq!(daily.coverage().unwrap().midnight, first == 0);
        }
        let mut other = config.clone();
        other.home_assistant.atomic_grid = "sensor.different".into();
        assert_eq!(load_cache(&path, &other, point(90).received_at), Err("daily-history-cache-source-mismatch"));
        assert_eq!(load_cache(&path, &config, point(90).received_at + 86_400_000), Err("daily-history-cache-invalid"));
        std::fs::write(&path, b"{}").unwrap();
        assert_eq!(load_cache(&path, &config, point(90).received_at), Err("daily-history-cache-invalid"));
        std::fs::write(&path, vec![b' '; 8193]).unwrap();
        assert_eq!(load_cache(&path, &config, point(90).received_at), Err("daily-history-cache-invalid"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn cumulative_day_continuity_survives_both_dst_transitions_without_offset_reset() {
        for date in ["2027-03-28T12:00:00Z", "2027-10-31T12:00:00Z"] {
            let at = chrono::DateTime::parse_from_rfc3339(date).unwrap().timestamp_millis();
            let (start, end) = day_bounds(at).unwrap();
            let mut daily = HaDaily::default();
            for (n, meter_at) in (start..end).step_by(10_000).enumerate() {
                let mut sample = point(n as i64);
                sample.meter_at = meter_at;
                sample.received_at = meter_at + 100;
                daily.observe(sample);
            }
            assert!(daily.coverage().unwrap().midnight, "{date}");
            assert_eq!(daily.coverage().unwrap().day_end - start, end - start);
            assert_eq!(daily.current(true).import_wh, Some((end - start) / 10_000 - 1));
            assert!(daily.history_issue().is_none());
        }
    }

    #[test]
    fn delayed_results_after_disconnect_are_ignored_and_pending_buffers_are_bounded() {
        let mut daily = HaDaily::default();
        daily.observe(point(60));
        let request = daily.request(point(60).received_at).unwrap();
        let recovered = history(&request, 0, 60);
        daily.invalidate_live("daily-history-disconnected");
        daily.complete(&request, Ok(recovered.clone()));
        assert!(daily.coverage().is_none());
        daily.observe(point(62));
        let second = daily.request(point(62).received_at).unwrap();
        daily.complete(&request, Ok(recovered));
        assert!(daily.pending.is_some(), "old response cannot replace a newer request");
        daily.buffered = vec![point(63); MAX_PENDING];
        daily.observe(point(63));
        assert!(daily.pending.is_none());
        assert!(daily.buffered.is_empty());
        daily.complete(&second, Err("fixture-late-error"));
        assert_eq!(daily.history_issue(), Some("daily-history-buffer-limit"));
    }
}

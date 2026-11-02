//! Independent WebSocket-only Home Assistant feed. Atomic telegram sequences
//! provide acquisition evidence; separate entity changes remain provisional.

use crate::config::ConfigError;
use crate::reading::{EnergyTotals, Reading};
use crate::slimmelezer::MeterStats;
use crate::sungrow::{InverterSnapshot, InverterStatus};
use crate::{civil_from_days, days_from_civil};
use serde_json::{Value, json};
use std::net::{IpAddr, SocketAddr};

pub const MAX_MESSAGE: usize = 1_048_576;
pub const HOUSEHOLD_TOPOLOGY_SINGLE_GRID_INVERTER: &str = "single_grid_connected_inverter";
const GRID_IMPORT: usize = 0;
const GRID_EXPORT: usize = 1;
const INVERTER_AC: usize = 2;
const INVERTER_DC: usize = 3;
const CURRENT_DEMAND: usize = 4;
const MONTHLY_PEAK: usize = 5;
const ENERGY_IMPORT_TARIFF_1: usize = 6;
const ENERGY_IMPORT_TARIFF_2: usize = 7;
const ENERGY_EXPORT_TARIFF_1: usize = 8;
const ENERGY_EXPORT_TARIFF_2: usize = 9;
const SAMPLE_COUNT: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub struct HaConfig {
    pub endpoint: String,
    pub token_env: String,
    pub grid_import: String,
    pub grid_export: String,
    pub atomic_grid: String,
    pub active_power: String,
    pub dc_power: String,
    pub inverter_stale_seconds: f64,
    pub grid_skew_seconds: f64,
    pub current_demand: String,
    pub monthly_peak: String,
    pub energy_import_tariff1: String,
    pub energy_import_tariff2: String,
    pub energy_export_tariff1: String,
    pub energy_export_tariff2: String,
    pub meter_data_stale_seconds: f64,
    pub meter_data_max_event_skew_seconds: f64,
    pub household_use_enabled: bool,
    pub household_topology: String,
    pub ac_generation_sign: i8,
    pub household_max_event_skew_seconds: f64,
}

impl HaConfig {
    pub fn from_json(v: &Value) -> Result<Self, ConfigError> {
        let error = || ConfigError("Invalid Home Assistant endpoint, credential reference or entity settings".into());
        let text = |key: &str| v[key].as_str().ok_or_else(error);
        let config = Self {
            endpoint: text("endpoint")?.into(),
            token_env: text("tokenEnv")?.into(),
            grid_import: text("gridImport")?.into(),
            grid_export: text("gridExport")?.into(),
            atomic_grid: text("atomicGrid")?.into(),
            active_power: text("activePower")?.into(),
            dc_power: text("dcPower")?.into(),
            inverter_stale_seconds: v["inverterStaleSeconds"].as_f64().ok_or_else(error)?,
            grid_skew_seconds: v["gridSkewSeconds"].as_f64().ok_or_else(error)?,
            current_demand: text("currentDemand")?.into(),
            monthly_peak: text("monthlyPeak")?.into(),
            energy_import_tariff1: text("energyImportTariff1")?.into(),
            energy_import_tariff2: text("energyImportTariff2")?.into(),
            energy_export_tariff1: text("energyExportTariff1")?.into(),
            energy_export_tariff2: text("energyExportTariff2")?.into(),
            meter_data_stale_seconds: v["meterDataStaleSeconds"].as_f64().ok_or_else(error)?,
            meter_data_max_event_skew_seconds: v["meterDataMaxEventSkewSeconds"].as_f64().ok_or_else(error)?,
            household_use_enabled: v["householdUse"]["enabled"].as_bool().ok_or_else(error)?,
            household_topology: v["householdUse"]["topology"].as_str().ok_or_else(error)?.into(),
            ac_generation_sign: v["householdUse"]["generationSign"]
                .as_i64()
                .and_then(|sign| i8::try_from(sign).ok())
                .ok_or_else(error)?,
            household_max_event_skew_seconds: v["householdUse"]["maxEventSkewSeconds"].as_f64().ok_or_else(error)?,
        };
        config.address().map_err(|_| error())?;
        let env = config.token_env.as_bytes();
        let entity = |s: &str| {
            s.starts_with("sensor.")
                && (8..=128).contains(&s.len())
                && s[7..].bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        };
        let meter_entities = [
            &config.current_demand,
            &config.monthly_peak,
            &config.energy_import_tariff1,
            &config.energy_import_tariff2,
            &config.energy_export_tariff1,
            &config.energy_export_tariff2,
        ];
        let mut unique_entities = vec![&config.grid_import, &config.grid_export];
        unique_entities.extend(
            [&config.atomic_grid, &config.active_power, &config.dc_power].into_iter().filter(|id| !id.is_empty()),
        );
        unique_entities.extend(meter_entities.iter().copied().filter(|id| !id.is_empty()));
        let entities_are_unique =
            unique_entities.iter().enumerate().all(|(index, id)| !unique_entities[..index].contains(id));
        if env.is_empty()
            || env.len() > 128
            || !(env[0].is_ascii_uppercase() || env[0] == b'_')
            || !env.iter().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_')
            || !entity(&config.grid_import)
            || !entity(&config.grid_export)
            || (!config.active_power.is_empty() && !entity(&config.active_power))
            || (!config.dc_power.is_empty() && !entity(&config.dc_power))
            || meter_entities.iter().any(|id| !id.is_empty() && !entity(id))
            || !entities_are_unique
            || (!config.atomic_grid.is_empty()
                && (!entity(&config.atomic_grid)
                    || config.atomic_grid == config.grid_import
                    || config.atomic_grid == config.grid_export
                    || config.atomic_grid == config.active_power
                    || config.atomic_grid == config.dc_power))
            || config.grid_import == config.grid_export
            || config.active_power == config.grid_import
            || config.active_power == config.grid_export
            || config.dc_power == config.grid_import
            || config.dc_power == config.grid_export
            || (!config.active_power.is_empty() && config.active_power == config.dc_power)
            || !config.inverter_stale_seconds.is_finite()
            || !(1.0..=86400.0).contains(&config.inverter_stale_seconds)
            || !config.grid_skew_seconds.is_finite()
            || !(0.0..=30.0).contains(&config.grid_skew_seconds)
            || !config.meter_data_stale_seconds.is_finite()
            || !(1.0..=86400.0).contains(&config.meter_data_stale_seconds)
            || !config.meter_data_max_event_skew_seconds.is_finite()
            || !(0.0..=30.0).contains(&config.meter_data_max_event_skew_seconds)
            || !config.household_max_event_skew_seconds.is_finite()
            || !(0.0..=30.0).contains(&config.household_max_event_skew_seconds)
            || !matches!(config.ac_generation_sign, -1..=1)
            || (config.household_use_enabled
                && (config.atomic_grid.is_empty()
                    || config.active_power.is_empty()
                    || config.household_topology != HOUSEHOLD_TOPOLOGY_SINGLE_GRID_INVERTER
                    || !matches!(config.ac_generation_sign, -1 | 1)))
            || (!config.household_topology.is_empty()
                && config.household_topology != HOUSEHOLD_TOPOLOGY_SINGLE_GRID_INVERTER)
        {
            return Err(error());
        }
        Ok(config)
    }

    /// IP literals avoid unbounded operating-system DNS resolution in the worker.
    pub fn address(&self) -> Result<SocketAddr, &'static str> {
        let u = url::Url::parse(&self.endpoint).map_err(|_| "ha-endpoint")?;
        if self.endpoint.len() > 512
            || !self.endpoint.bytes().all(|b| (0x21..=0x7e).contains(&b))
            || !matches!(u.scheme(), "ws" | "wss")
            || u.path() != "/api/websocket"
            || u.query().is_some()
            || u.fragment().is_some()
            || !u.username().is_empty()
            || u.password().is_some()
        {
            return Err("ha-endpoint");
        }
        let host = u.host_str().ok_or("ha-endpoint")?.trim_matches(['[', ']']);
        let ip: IpAddr = host.parse().map_err(|_| "ha-endpoint-ip-required")?;
        if ip.is_unspecified() || ip.is_multicast() {
            return Err("ha-endpoint");
        }
        let port = u.port_or_known_default().filter(|p| *p > 0).ok_or("ha-endpoint")?;
        Ok(SocketAddr::new(ip, port))
    }

    pub fn to_json(&self) -> Value {
        json!({
            "endpoint": self.endpoint, "tokenEnv": self.token_env,
            "gridImport": self.grid_import, "gridExport": self.grid_export,
            "atomicGrid": self.atomic_grid,
            "activePower": self.active_power, "dcPower": self.dc_power,
            "inverterStaleSeconds": self.inverter_stale_seconds,
            "gridSkewSeconds": self.grid_skew_seconds,
            "currentDemand": self.current_demand,
            "monthlyPeak": self.monthly_peak,
            "energyImportTariff1": self.energy_import_tariff1,
            "energyImportTariff2": self.energy_import_tariff2,
            "energyExportTariff1": self.energy_export_tariff1,
            "energyExportTariff2": self.energy_export_tariff2,
            "meterDataStaleSeconds": self.meter_data_stale_seconds,
            "meterDataMaxEventSkewSeconds": self.meter_data_max_event_skew_seconds,
            "householdUse": {
                "enabled": self.household_use_enabled,
                "topology": self.household_topology,
                "generationSign": self.ac_generation_sign,
                "maxEventSkewSeconds": self.household_max_event_skew_seconds
            }
        })
    }

    fn entity_index(&self, id: &str) -> Option<usize> {
        [
            &self.grid_import,
            &self.grid_export,
            &self.active_power,
            &self.dc_power,
            &self.current_demand,
            &self.monthly_peak,
            &self.energy_import_tariff1,
            &self.energy_import_tariff2,
            &self.energy_export_tariff1,
            &self.energy_export_tariff2,
        ]
        .iter()
        .position(|entity| !entity.is_empty() && entity.as_str() == id)
    }

    fn entity_name(&self, index: usize) -> Option<&str> {
        [
            self.grid_import.as_str(),
            self.grid_export.as_str(),
            self.active_power.as_str(),
            self.dc_power.as_str(),
            self.current_demand.as_str(),
            self.monthly_peak.as_str(),
            self.energy_import_tariff1.as_str(),
            self.energy_import_tariff2.as_str(),
            self.energy_export_tariff1.as_str(),
            self.energy_export_tariff2.as_str(),
        ]
        .get(index)
        .copied()
        .filter(|entity| !entity.is_empty())
    }

    fn relevant(&self, id: &str) -> bool {
        (if self.atomic_grid.is_empty() {
            id == self.grid_import || id == self.grid_export
        } else {
            id == self.atomic_grid
        }) || self.entity_index(id).is_some_and(|index| index >= INVERTER_AC)
    }
}

#[derive(Clone)]
struct Sample {
    value: f64,
    at: i64,
    reported_at: i64,
    received: Instant,
    age_at_receive: i64,
}

impl Sample {
    fn age(&self, now: i64) -> i64 {
        let elapsed = self.received.elapsed().as_millis().min(i64::MAX as u128) as i64;
        now.saturating_sub(self.at).max(self.age_at_receive.saturating_add(elapsed))
    }
}

#[derive(Clone, PartialEq)]
struct AtomicPayload {
    session: String,
    sequence: u64,
    meter_at: i64,
    publisher_at: i64,
    import_w: f64,
    export_w: f64,
}

#[derive(serde::Deserialize)]
struct AtomicAttributes {
    schema_version: u64,
    session: String,
    sample_sequence: u64,
    meter_timestamp: String,
    received_at: String,
    import_w: f64,
    export_w: f64,
}

fn utc_timestamp(text: &str) -> Result<i64, &'static str> {
    if text.len() > 40 {
        return Err("ha-atomic-time");
    }
    let time = chrono::DateTime::parse_from_rfc3339(text).map_err(|_| "ha-atomic-time")?;
    if time.offset().local_minus_utc() != 0 || text.ends_with("-00:00") {
        return Err("ha-atomic-time");
    }
    Ok(time.timestamp_millis())
}

#[derive(Clone, Copy)]
struct BelgianCalendar {
    month: i64,
    day: i64,
    minute: i64,
    offset_minutes: i64,
}

fn last_sunday(year: i64, month: u32) -> i64 {
    let (next_year, next_month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let last_day = days_from_civil(next_year, next_month, 1) - 1;
    last_day - (last_day + 4).rem_euclid(7)
}

fn belgian_meter_calendar(utc_ms: i64) -> Option<BelgianCalendar> {
    let utc_seconds = utc_ms.div_euclid(1000);
    let (year, _, _) = civil_from_days(utc_seconds.div_euclid(86_400));
    if !(2000..=9998).contains(&year) {
        return None;
    }
    // Belgium follows the EU rule: UTC+2 from 01:00 UTC on the last Sunday
    // in March until 01:00 UTC on the last Sunday in October.
    let summer_start_seconds = last_sunday(year, 3) * 86_400 + 3600;
    let summer_end_seconds = last_sunday(year, 10) * 86_400 + 3600;
    let offset_minutes = if (summer_start_seconds..summer_end_seconds).contains(&utc_seconds) { 120 } else { 60 };
    let local_seconds = utc_seconds + offset_minutes * 60;
    let local_day = local_seconds.div_euclid(86_400);
    let (local_year, month, _) = civil_from_days(local_day);
    Some(BelgianCalendar {
        month: local_year * 100 + month as i64,
        day: local_day,
        minute: local_seconds.rem_euclid(86_400) / 60,
        offset_minutes,
    })
}

fn atomic_payload(value: &Value, now: i64) -> Result<AtomicPayload, &'static str> {
    let state = value["state"].as_str().ok_or("ha-atomic-unavailable")?;
    if matches!(state, "unavailable" | "unknown") {
        return Err("ha-atomic-unavailable");
    }
    if !value["attributes"]["available"].is_null() && value["attributes"]["available"] != true {
        return Err("ha-atomic-unavailable");
    }
    let a: AtomicAttributes = serde_json::from_value(value["attributes"].clone()).map_err(|_| "ha-atomic-schema")?;
    if a.schema_version != 1
        || a.sample_sequence == 0
        || a.sample_sequence > 9_007_199_254_740_991
        || state != a.sample_sequence.to_string()
        || a.session.is_empty()
        || a.session.len() > 128
        || !a.session.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("ha-atomic-schema");
    }
    if [a.import_w, a.export_w].iter().any(|w| !w.is_finite() || !(0.0..=1_000_000.0).contains(w)) {
        return Err("ha-atomic-power");
    }
    let meter_at = utc_timestamp(&a.meter_timestamp)?;
    let publisher_at = utc_timestamp(&a.received_at)?;
    for at in [meter_at, publisher_at] {
        if at <= 0 || now.saturating_sub(at) >= 30_000 || at.saturating_sub(now) > 5000 {
            return Err("ha-atomic-time");
        }
    }
    if meter_at.saturating_sub(publisher_at) > 5000 || publisher_at.saturating_sub(meter_at) >= 30_000 {
        return Err("ha-atomic-time");
    }
    Ok(AtomicPayload {
        session: a.session,
        sequence: a.sample_sequence,
        meter_at,
        publisher_at,
        import_w: a.import_w,
        export_w: a.export_w,
    })
}

struct AtomicSample {
    payload: AtomicPayload,
    received: Instant,
    age_at_receive: i64,
}

impl AtomicSample {
    fn age(&self, now: i64) -> i64 {
        let at = self.payload.meter_at.min(self.payload.publisher_at);
        now.saturating_sub(at)
            .max(self.age_at_receive.saturating_add(self.received.elapsed().as_millis().min(i64::MAX as u128) as i64))
    }
}

#[derive(Default)]
struct AtomicGrid {
    high_water: Option<AtomicPayload>,
    retired_sessions: std::collections::HashSet<String>,
    sample: Option<AtomicSample>,
    observation: Option<Observation>,
    last_issue: Option<&'static str>,
    reset: bool,
}

impl AtomicGrid {
    fn invalidate(&mut self, issue: &str) {
        self.sample = None;
        self.reset = true;
        // Only fixed client codes reach diagnostics; transport errors live in MeterStats.
        self.last_issue = Some(if issue == "ha-atomic-unavailable" {
            "ha-atomic-unavailable"
        } else {
            "ha-awaiting-new-atomic-sample"
        });
    }

    fn apply(&mut self, value: &Value, now: i64, live: bool) {
        let stamp = |name: &str| {
            value[name]
                .as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.timestamp_millis())
        };
        self.observation = Some(Observation {
            changed: stamp("last_changed"),
            updated: stamp("last_updated"),
            reported: stamp("last_reported"),
            received: Instant::now(),
        });
        let result = atomic_payload(value, now).and_then(|payload| {
            if self.retired_sessions.contains(&payload.session) {
                return Err("ha-atomic-retired-session");
            }
            if let Some(old) = &self.high_water {
                if old.session == payload.session {
                    // Cached identical snapshots are not fresh samples. They may
                    // retain the watermark, but never retain eligibility.
                    if !live && *old == payload {
                        return Ok(payload);
                    }
                    if payload.sequence <= old.sequence
                        || payload.meter_at <= old.meter_at
                        || payload.publisher_at <= old.publisher_at
                    {
                        return Err("ha-atomic-replay");
                    }
                } else {
                    if self.retired_sessions.len() >= 64 {
                        return Err("ha-atomic-session-limit");
                    }
                    // Session restart resets sequence, not the meter's physical
                    // clock. Do not accept an older telegram from a new session.
                    if payload.meter_at <= old.meter_at || payload.publisher_at <= old.publisher_at {
                        return Err("ha-atomic-replay");
                    }
                    self.retired_sessions.insert(old.session.clone());
                    self.reset = true;
                }
            } else {
                self.reset = true;
            }
            Ok(payload)
        });
        match result {
            Ok(payload) => {
                self.high_water = Some(payload.clone());
                if live {
                    self.sample = Some(AtomicSample {
                        age_at_receive: now.saturating_sub(payload.meter_at.min(payload.publisher_at)).max(0),
                        received: Instant::now(),
                        payload,
                    });
                    self.last_issue = None;
                } else {
                    self.invalidate("snapshot");
                }
            }
            Err(issue) => {
                self.invalidate(issue);
                self.last_issue = Some(issue);
            }
        }
    }

    fn issue(&self, now: i64, stale_seconds: f64) -> &'static str {
        if let Some(issue) = self.last_issue {
            return issue;
        }
        let Some(sample) = &self.sample else {
            return "ha-awaiting-new-atomic-sample";
        };
        if sample.payload.meter_at > now || sample.payload.publisher_at > now {
            return "ha-atomic-future";
        }
        if sample.age(now) as f64 >= stale_seconds.min(30.0) * 1000.0 {
            return "ha-atomic-stale";
        }
        ""
    }

    fn reading(&self, now: i64, stale_seconds: f64) -> Option<Reading> {
        if !self.issue(now, stale_seconds).is_empty() {
            return None;
        }
        let sample = self.sample.as_ref()?;
        Some(Reading::new(
            sample.payload.meter_at,
            sample.payload.import_w,
            sample.payload.export_w,
            "home-assistant-atomic",
        ))
    }

    fn diagnostic(&self, entity: String, now: i64, stale_seconds: f64) -> EntityDiagnostic {
        let o = self.observation.as_ref();
        let h = self.high_water.as_ref();
        let issue = self.issue(now, stale_seconds);
        EntityDiagnostic {
            entity,
            ha_last_changed: o.and_then(|o| o.changed),
            ha_last_updated: o.and_then(|o| o.updated),
            ha_last_reported: o.and_then(|o| o.reported),
            received_age_ms: o.map(|o| o.received.elapsed().as_millis().min(u64::MAX as u128) as u64),
            evidence_at: self.sample.as_ref().map(|s| s.payload.meter_at),
            age_ms: self.sample.as_ref().map(|s| s.age(now)),
            issue: (!issue.is_empty()).then(|| issue.into()),
            sample_sequence: h.map(|h| h.sequence),
            meter_timestamp: h.map(|h| h.meter_at),
            publisher_received_at: h.map(|h| h.publisher_at),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityDiagnostic {
    pub entity: String,
    pub ha_last_changed: Option<i64>,
    pub ha_last_updated: Option<i64>,
    pub ha_last_reported: Option<i64>,
    pub received_age_ms: Option<u64>,
    pub evidence_at: Option<i64>,
    pub age_ms: Option<i64>,
    pub issue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meter_timestamp: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_received_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HaDiagnostics {
    pub connected: bool,
    pub initialized: bool,
    pub evidence: String,
    pub entities: Vec<EntityDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_power_entity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dc_power_entity: Option<String>,
    #[serde(default)]
    pub household_use_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub household_use_issue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_forecast_issue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_energy_issue: Option<String>,
}

#[derive(Clone)]
struct Observation {
    changed: Option<i64>,
    updated: Option<i64>,
    reported: Option<i64>,
    received: Instant,
}

pub struct HaState {
    config: HaConfig,
    samples: [Option<Sample>; SAMPLE_COUNT],
    // High-water marks survive removal/reconnect so replay cannot renew old samples.
    history: [Option<Sample>; SAMPLE_COUNT],
    observations: [Option<Observation>; SAMPLE_COUNT],
    issues: [Option<String>; SAMPLE_COUNT],
    live_changes: [bool; SAMPLE_COUNT],
    atomic: AtomicGrid,
    pub stats: MeterStats,
    ready: bool,
}

impl HaState {
    pub fn new(config: HaConfig) -> Self {
        Self {
            config,
            samples: std::array::from_fn(|_| None),
            history: std::array::from_fn(|_| None),
            observations: std::array::from_fn(|_| None),
            issues: std::array::from_fn(|_| None),
            live_changes: [false; SAMPLE_COUNT],
            atomic: AtomicGrid::default(),
            stats: MeterStats::default(),
            ready: false,
        }
    }
}

use crate::runtime::Runtime;
use crate::service::{lock, pause};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

// Bound the upgrade/TLS phase as well as individual socket calls.
struct BoundedSocket {
    socket: TcpStream,
    deadline: Option<Instant>,
    budget: usize,
}

impl Read for BoundedSocket {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        if self.deadline.is_some_and(|d| Instant::now() >= d) || self.budget == 0 {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "HA handshake limit"));
        }
        let size = b.len().min(self.budget);
        let n = self.socket.read(&mut b[..size])?;
        if self.deadline.is_some() {
            self.budget -= n;
        }
        Ok(n)
    }
}

impl Write for BoundedSocket {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "HA handshake deadline"));
        }
        self.socket.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.socket.flush()
    }
}

type Socket = WebSocket<MaybeTlsStream<BoundedSocket>>;

fn transport(ws: &mut Socket) -> Result<&mut BoundedSocket, &'static str> {
    match ws.get_mut() {
        MaybeTlsStream::Plain(s) => Ok(s),
        MaybeTlsStream::Rustls(s) => Ok(&mut s.sock),
        _ => Err("ha-transport"),
    }
}

fn send(ws: &mut Socket, value: Value) -> Result<(), &'static str> {
    transport(ws)?.deadline = Some(Instant::now() + Duration::from_secs(3));
    ws.send(Message::Text(value.to_string().into())).map_err(|_| "ha-write")
}

fn open(config: &HaConfig) -> Result<Socket, &'static str> {
    let socket = TcpStream::connect_timeout(&config.address()?, Duration::from_secs(3)).map_err(|_| "ha-connect")?;
    socket.set_read_timeout(Some(Duration::from_millis(500))).map_err(|_| "ha-timeout")?;
    socket.set_write_timeout(Some(Duration::from_secs(3))).map_err(|_| "ha-timeout")?;
    let socket = BoundedSocket { socket, deadline: Some(Instant::now() + Duration::from_secs(8)), budget: 65_536 };
    let limits = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE))
        .max_write_buffer_size(MAX_MESSAGE);
    let (mut ws, _) = tungstenite::client_tls_with_config(config.endpoint.as_str(), socket, Some(limits), None)
        .map_err(|_| "ha-upgrade-or-tls")?;
    let stream = transport(&mut ws)?;
    stream.deadline = None;
    stream.budget = usize::MAX;
    Ok(ws)
}

fn run_session(runtime: &Mutex<Runtime>, stop: &AtomicBool, config: &HaConfig) -> Result<(), &'static str> {
    // Resolve a credential reference at connection time; never put credentials in config/debug/errors.
    let token = std::env::var(&config.token_env).map_err(|_| "ha-token-missing")?;
    if token.is_empty() || token.len() > 8192 || token.bytes().any(|b| b.is_ascii_control()) {
        return Err("ha-token-invalid");
    }
    run_authenticated_session(runtime, stop, config, &token, Duration::from_secs(20))
}

fn run_authenticated_session(
    runtime: &Mutex<Runtime>,
    stop: &AtomicBool,
    config: &HaConfig,
    token: &str,
    ping_interval: Duration,
) -> Result<(), &'static str> {
    let mut ws = open(config)?;
    let mut phase = 0;
    let mut deadline = Instant::now() + Duration::from_secs(10);
    let mut last_message = Instant::now();
    let mut next_ping = Instant::now() + ping_interval;
    let mut ping_deadline: Option<Instant> = None;
    let mut next_command_id = 3_u64;
    let mut pending_ping_id = None;
    let mut buffered: Vec<(String, Value)> = Vec::new();
    let mut buffered_bytes = 0;
    let relevant = |id: &str| config.relevant(id);
    while !stop.load(Ordering::Relaxed) {
        if (phase < 4 && Instant::now() >= deadline)
            || ping_deadline.is_some_and(|d| Instant::now() >= d)
            || last_message.elapsed() >= Duration::from_secs(60)
        {
            return Err("ha-response-timeout");
        }
        if phase == 4 && pending_ping_id.is_none() && Instant::now() >= next_ping {
            let id = next_command_id;
            if id > 9_007_199_254_740_991 {
                return Err("ha-command-id-exhausted");
            }
            next_command_id += 1;
            send(&mut ws, json!({"id":id,"type":"ping"}))?;
            pending_ping_id = Some(id);
            ping_deadline = Some(Instant::now() + Duration::from_secs(10));
            next_ping = Instant::now() + ping_interval;
        }
        // A socket timeout alone does not bound a fragmented/trickling frame:
        // force read() to yield so cancellation and total deadlines are checked.
        transport(&mut ws)?.deadline = Some(Instant::now() + Duration::from_millis(500));
        let message = match ws.read() {
            Ok(m) => m,
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(_) => return Err("ha-read-or-protocol"),
        };
        last_message = Instant::now();
        let text = match message {
            Message::Text(t) => t,
            Message::Ping(_) | Message::Pong(_) => {
                ws.flush().map_err(|_| "ha-write")?;
                continue;
            }
            Message::Close(_) => return Err("ha-closed"),
            _ => return Err("ha-message-type"),
        };
        let v: Value = serde_json::from_str(&text).map_err(|_| "ha-json")?;
        let previous_phase = phase;
        match (phase, v["type"].as_str()) {
            (0, Some("auth_required")) => {
                send(&mut ws, json!({"type":"auth","access_token":token}))?;
                phase = 1;
            }
            (1, Some("auth_ok")) => {
                lock(runtime).ha_connected();
                send(&mut ws, json!({"id":1,"type":"subscribe_events","event_type":"state_changed"}))?;
                phase = 2;
            }
            (_, Some("auth_invalid")) => return Err("ha-auth-rejected"),
            (2, Some("result")) if v["id"] == 1 => {
                if v["success"] != true {
                    return Err("ha-subscription-rejected");
                }
                send(&mut ws, json!({"id":2,"type":"get_states"}))?;
                phase = 3;
            }
            (3, Some("result")) if v["id"] == 2 => {
                if v["success"] != true {
                    return Err("ha-snapshot-rejected");
                }
                let states = v["result"].as_array().filter(|s| s.len() <= 10_000).ok_or("ha-snapshot-shape")?;
                let now = crate::now_ms();
                let mut runtime = lock(runtime);
                let mut snapshot_times = std::collections::HashMap::new();
                for state in states {
                    if let Some(id) = state["entity_id"].as_str().filter(|id| relevant(id)) {
                        if let Some(at) =
                            state["last_changed"].as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        {
                            snapshot_times.insert(id.to_string(), at.timestamp_millis());
                        }
                        runtime.ha_snapshot_state(id, state, now);
                    }
                }
                for (id, state) in buffered.drain(..) {
                    if let (Some(snapshot_at), Some(event_at)) = (
                        snapshot_times.get(&id),
                        state["last_changed"].as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()),
                    ) && event_at.timestamp_millis() < *snapshot_at
                    {
                        continue;
                    }
                    runtime.ha_state(&id, &state, now);
                }
                runtime.ha_ready();
                phase = 4;
            }
            (2..=4, Some("event")) if v["id"] == 1 => {
                if v["event"]["event_type"] != "state_changed" {
                    return Err("ha-event-shape");
                }
                let data = &v["event"]["data"];
                let id = data["entity_id"].as_str().ok_or("ha-event-shape")?;
                if relevant(id) {
                    let state = &data["new_state"];
                    if !state.is_null() && state["entity_id"] != id {
                        return Err("ha-event-entity");
                    }
                    if phase < 4 {
                        buffered_bytes += text.len();
                        if buffered.len() >= 64 || buffered_bytes > MAX_MESSAGE {
                            return Err("ha-initialization-overflow");
                        }
                        buffered.push((id.to_string(), state.clone()));
                    } else {
                        lock(runtime).ha_state(id, state, crate::now_ms());
                    }
                }
            }
            (4, Some("pong")) if pending_ping_id.is_some() && v["id"].as_u64() == pending_ping_id => {
                pending_ping_id = None;
                ping_deadline = None;
            }
            (4, Some("result"))
                if pending_ping_id.is_some() && v["id"].as_u64() == pending_ping_id && v["success"] == false =>
            {
                return Err("ha-heartbeat-rejected");
            }
            _ => return Err("ha-unexpected-response"),
        }
        if phase < 4 && phase != previous_phase {
            deadline = Instant::now() + Duration::from_secs(10);
        }
    }
    // Dropping closes the socket without a potentially blocking close handshake.
    Ok(())
}

pub(crate) fn worker(runtime: &Mutex<Runtime>, stop: &AtomicBool, config: &HaConfig) {
    let mut backoff = 1;
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let result = run_session(runtime, stop, config);
        if stop.load(Ordering::Relaxed) {
            return;
        }
        lock(runtime).ha_disconnect(result.err().unwrap_or("ha-disconnected"));
        if started.elapsed() >= Duration::from_secs(60) {
            backoff = 1;
        }
        if !pause(stop, Duration::from_secs(backoff)) {
            return;
        }
        backoff = (backoff * 2).min(30);
    }
}

impl HaState {
    fn feature_issue(&self, now: i64, required: &[usize], require_same_day: bool) -> Option<&'static str> {
        if !self.stats.connected || !self.ready {
            return Some("ha-meter-data-disconnected");
        }
        if self.config.atomic_grid.is_empty() {
            return Some("ha-meter-data-needs-atomic-grid");
        }
        if !self.atomic.issue(now, 30.0).is_empty() {
            return Some("ha-meter-data-grid-unavailable");
        }
        let Some(grid) = self.atomic.sample.as_ref() else {
            return Some("ha-meter-data-grid-unavailable");
        };
        let Some(calendar) = belgian_meter_calendar(grid.payload.meter_at) else {
            return Some("ha-meter-clock-unsupported");
        };
        for &index in required {
            if self.config.entity_name(index).is_none() {
                return Some("ha-meter-data-not-mapped");
            }
            if self.issues[index].is_some() {
                return Some("ha-meter-data-invalid");
            }
            if !self.live_changes[index] {
                return Some("ha-meter-data-awaiting-live-change");
            }
            let Some(sample) = self.samples[index].as_ref() else {
                return Some("ha-meter-data-unavailable");
            };
            if now < sample.at || sample.age(now) as f64 > self.config.meter_data_stale_seconds * 1000.0 {
                return Some("ha-meter-data-stale");
            }
            if sample.at.abs_diff(grid.payload.publisher_at) as f64
                > self.config.meter_data_max_event_skew_seconds * 1000.0
            {
                return Some("ha-meter-data-event-proximity");
            }
            if let Some(sample_calendar) = belgian_meter_calendar(sample.at) {
                if index == MONTHLY_PEAK && sample_calendar.month != calendar.month {
                    return Some("ha-meter-data-month-boundary");
                }
                if require_same_day && sample_calendar.day != calendar.day {
                    return Some("ha-meter-data-day-boundary");
                }
            } else {
                return Some("ha-meter-clock-unsupported");
            }
        }
        None
    }

    /// Adds optional HA meter values only when each live state-change event is
    /// bounded in age and close to this atomic callback. This is not a same-
    /// telegram guarantee; it is an explicitly provisional event-proximity gate.
    pub fn enrich_reading(&self, reading: &mut Reading, now: i64) {
        if reading.source != "home-assistant-atomic" {
            return;
        }
        let Some(calendar) = belgian_meter_calendar(reading.timestamp) else {
            return;
        };
        reading.meter_time = Some(reading.timestamp);
        reading.meter_month = Some(calendar.month);
        reading.meter_day = Some(calendar.day);
        reading.meter_minute_of_day = Some(calendar.minute);
        reading.meter_offset_minutes = Some(calendar.offset_minutes);

        if self.feature_issue(now, &[CURRENT_DEMAND], false).is_none() {
            reading.current_demand_w = self.samples[CURRENT_DEMAND].as_ref().map(|sample| sample.value);
        }
        if self.feature_issue(now, &[MONTHLY_PEAK], false).is_none() {
            reading.monthly_peak_w = self.samples[MONTHLY_PEAK].as_ref().map(|sample| sample.value);
            reading.monthly_peak_month = Some(calendar.month);
        }
        if self
            .feature_issue(
                now,
                &[ENERGY_IMPORT_TARIFF_1, ENERGY_IMPORT_TARIFF_2, ENERGY_EXPORT_TARIFF_1, ENERGY_EXPORT_TARIFF_2],
                true,
            )
            .is_none()
        {
            let value = |index: usize| self.samples[index].as_ref().map(|sample| sample.value as i64);
            if let (
                Some(import_tariff1_wh),
                Some(import_tariff2_wh),
                Some(export_tariff1_wh),
                Some(export_tariff2_wh),
            ) = (
                value(ENERGY_IMPORT_TARIFF_1),
                value(ENERGY_IMPORT_TARIFF_2),
                value(ENERGY_EXPORT_TARIFF_1),
                value(ENERGY_EXPORT_TARIFF_2),
            ) {
                reading.energy_totals_wh =
                    Some(EnergyTotals { import_tariff1_wh, import_tariff2_wh, export_tariff1_wh, export_tariff2_wh });
            }
        }
    }

    pub fn peak_data_issue(&self, now: i64) -> Option<&'static str> {
        self.feature_issue(now, &[CURRENT_DEMAND, MONTHLY_PEAK], false)
    }

    pub fn daily_data_issue(&self, now: i64) -> Option<&'static str> {
        self.feature_issue(
            now,
            &[ENERGY_IMPORT_TARIFF_1, ENERGY_IMPORT_TARIFF_2, ENERGY_EXPORT_TARIFF_1, ENERGY_EXPORT_TARIFF_2],
            true,
        )
    }

    pub fn diagnostics(&self, now: i64, meter_stale_seconds: f64) -> HaDiagnostics {
        let mut ids = if self.config.atomic_grid.is_empty() {
            vec![
                (self.config.grid_import.as_str(), Some(GRID_IMPORT)),
                (self.config.grid_export.as_str(), Some(GRID_EXPORT)),
            ]
        } else {
            vec![(self.config.atomic_grid.as_str(), None)]
        };
        ids.extend(
            (INVERTER_AC..SAMPLE_COUNT).filter_map(|index| self.config.entity_name(index).map(|id| (id, Some(index)))),
        );
        HaDiagnostics {
            connected: self.stats.connected,
            initialized: self.ready,
            evidence: if self.config.atomic_grid.is_empty() {
                "provisional HA value-change evidence; physical acquisition unverified"
            } else {
                "atomic checksum-accepted telegram sequence; physical measurement validation pending"
            }
            .into(),
            active_power_entity: (!self.config.active_power.is_empty()).then(|| self.config.active_power.clone()),
            dc_power_entity: (!self.config.dc_power.is_empty()).then(|| self.config.dc_power.clone()),
            household_use_enabled: self.config.household_use_enabled,
            household_use_issue: self.household_use_value(now, meter_stale_seconds).err().map(str::to_string),
            peak_forecast_issue: self.peak_data_issue(now).map(str::to_string),
            daily_energy_issue: self.daily_data_issue(now).map(str::to_string),
            entities: ids
                .iter()
                .map(|(id, index)| {
                    if index.is_none() {
                        return self.atomic.diagnostic((*id).to_string(), now, meter_stale_seconds);
                    }
                    let i = index.unwrap();
                    let o = self.observations[i].as_ref();
                    let s = self.samples[i].as_ref();
                    EntityDiagnostic {
                        entity: (*id).to_string(),
                        ha_last_changed: o.and_then(|o| o.changed),
                        ha_last_updated: o.and_then(|o| o.updated),
                        ha_last_reported: o.and_then(|o| o.reported),
                        received_age_ms: o.map(|o| o.received.elapsed().as_millis().min(u64::MAX as u128) as u64),
                        evidence_at: s.map(|s| s.at),
                        age_ms: s.map(|s| s.age(now)),
                        issue: self.issues[i].clone().or_else(|| {
                            let limit = match i {
                                GRID_IMPORT | GRID_EXPORT => meter_stale_seconds,
                                INVERTER_AC | INVERTER_DC => self.config.inverter_stale_seconds,
                                _ => self.config.meter_data_stale_seconds,
                            };
                            if !self.stats.connected {
                                Some("ha-disconnected".into())
                            } else if s.is_none() {
                                Some("ha-entity-unavailable".into())
                            } else if s.is_some_and(|s| now < s.at || s.age(now) as f64 > limit * 1000.0) {
                                Some("ha-entity-stale".into())
                            } else if i < INVERTER_AC && !self.live_changes[i] {
                                Some("ha-awaiting-new-grid-evidence".into())
                            } else if i >= CURRENT_DEMAND && !self.live_changes[i] {
                                Some("ha-awaiting-live-meter-value-change".into())
                            } else {
                                None
                            }
                        }),
                        sample_sequence: None,
                        meter_timestamp: None,
                        publisher_received_at: None,
                    }
                })
                .collect(),
        }
    }

    pub fn disconnect(&mut self, issue: &str) {
        self.atomic.invalidate(issue);
        self.samples = std::array::from_fn(|_| None);
        self.live_changes = [false; SAMPLE_COUNT];
        self.ready = false;
        self.stats.connected = false;
        self.stats.errors += 1;
        self.stats.reconnects += 1;
        self.stats.last_issue = Some(issue.into());
    }

    pub fn connected(&mut self) {
        self.stats.connected = true;
    }

    pub fn ready(&mut self) {
        self.ready = true;
    }

    pub fn relevant(&self, id: &str) -> bool {
        self.config.relevant(id)
    }

    pub fn state(&mut self, id: &str, value: &Value, now: i64) {
        self.apply(id, value, now, true);
    }

    pub fn snapshot_state(&mut self, id: &str, value: &Value, now: i64) {
        self.apply(id, value, now, false);
    }

    fn apply(&mut self, id: &str, value: &Value, now: i64, live: bool) {
        if !self.config.atomic_grid.is_empty() {
            if id == self.config.atomic_grid {
                self.atomic.apply(value, now, live);
                if live && self.atomic.last_issue.is_some() {
                    self.stats.errors += 1;
                }
                return;
            }
            if self.config.entity_index(id).is_none_or(|index| index < INVERTER_AC) {
                return;
            }
        }
        let Some(index) = self.config.entity_index(id) else { return };
        let stamp = |name: &str| {
            value[name]
                .as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.timestamp_millis())
        };
        self.observations[index] = Some(Observation {
            changed: stamp("last_changed"),
            updated: stamp("last_updated"),
            reported: stamp("last_reported"),
            received: Instant::now(),
        });
        let parsed = (|| {
            let state = value["state"].as_str().ok_or("ha-entity-unavailable")?;
            if matches!(state, "unavailable" | "unknown") {
                return Err("ha-entity-unavailable");
            }
            let raw = state.parse::<f64>().map_err(|_| "ha-invalid-meter-value")?;
            let unit = value["attributes"]["unit_of_measurement"].as_str().ok_or("ha-unit")?;
            let normalized = match index {
                GRID_IMPORT | GRID_EXPORT | CURRENT_DEMAND | MONTHLY_PEAK if unit == "kW" => raw * 1000.0,
                INVERTER_AC if unit == "W" => raw,
                INVERTER_DC if unit == "W" => raw,
                ENERGY_IMPORT_TARIFF_1..=ENERGY_EXPORT_TARIFF_2 if unit == "kWh" => {
                    if value["attributes"]["state_class"] != "total_increasing"
                        || value["attributes"]["device_class"] != "energy"
                    {
                        return Err("ha-counter-class");
                    }
                    let wh = raw * 1000.0;
                    let rounded = wh.round();
                    if (wh - rounded).abs() > 0.000_001 {
                        return Err("ha-counter-precision");
                    }
                    rounded
                }
                _ => return Err("ha-unit"),
            };
            let valid_value = match index {
                INVERTER_AC => (-1_000_000.0..=1_000_000.0).contains(&normalized),
                ENERGY_IMPORT_TARIFF_1..=ENERGY_EXPORT_TARIFF_2 => (0.0..=999_999_999.0).contains(&normalized),
                _ => (0.0..=1_000_000.0).contains(&normalized),
            };
            if !raw.is_finite() || !normalized.is_finite() || !valid_value {
                return Err("ha-invalid-meter-value");
            }
            let at = chrono::DateTime::parse_from_rfc3339(value["last_changed"].as_str().ok_or("ha-timestamp")?)
                .map_err(|_| "ha-timestamp")?
                .timestamp_millis();
            if at <= 0 || at > now {
                return Err("ha-timestamp");
            }
            for key in ["last_updated", "last_reported"] {
                if !value[key].is_null() {
                    let time = stamp(key).ok_or("ha-timestamp")?;
                    if time < at || time > now {
                        return Err("ha-timestamp");
                    }
                }
            }
            Ok(Sample { value: normalized, at, reported_at: at, received: Instant::now(), age_at_receive: now - at })
        })();
        match parsed {
            Ok(mut sample) => {
                let changed = self.history[index].as_ref().is_none_or(|old| old.value != sample.value);
                if let Some(old) = &self.history[index] {
                    if sample.reported_at < old.reported_at
                        || (sample.reported_at == old.reported_at && sample.value != old.value)
                    {
                        self.samples[index] = None;
                        self.live_changes[index] = false;
                        self.issues[index] = Some("ha-out-of-order".into());
                        if index < 2 {
                            self.stats.last_issue = Some("ha-out-of-order".into());
                            self.stats.errors += 1;
                        }
                        return;
                    }
                    if sample.value == old.value {
                        sample.at = old.at;
                        sample.received = old.received;
                        sample.age_at_receive = old.age_at_receive;
                    }
                }
                self.history[index] = Some(sample.clone());
                self.samples[index] = Some(sample);
                self.issues[index] = None;
                if live && changed {
                    self.live_changes[index] = true;
                }
            }
            Err(issue) => {
                self.samples[index] = None;
                self.live_changes[index] = false;
                self.issues[index] = Some(issue.into());
                if index < 2 {
                    self.stats.errors += 1;
                    self.stats.last_issue = Some(issue.into());
                }
            }
        }
    }

    pub fn grid(&mut self, now: i64, stale_seconds: f64) -> Option<Reading> {
        if !self.config.atomic_grid.is_empty() {
            let reading =
                if self.ready && self.stats.connected { self.atomic.reading(now, stale_seconds) } else { None };
            if let Some(reading) = &reading {
                if self.stats.last_reading_at != Some(reading.timestamp) {
                    self.stats.telegrams += 1;
                }
                self.stats.last_reading_at = Some(reading.timestamp);
                self.stats.last_issue = None;
            } else if self.stats.connected {
                self.stats.last_issue = Some(
                    if !self.ready { "ha-awaiting-initialization" } else { self.atomic.issue(now, stale_seconds) }
                        .into(),
                );
            }
            return reading;
        }
        let result = (|| {
            if !self.ready || !self.stats.connected {
                return Err("ha-disconnected");
            }
            if !self.live_changes[0] || !self.live_changes[1] {
                return Err("ha-awaiting-new-grid-evidence");
            }
            let import = self.samples[0].as_ref().ok_or("ha-grid-unavailable")?;
            let export = self.samples[1].as_ref().ok_or("ha-grid-unavailable")?;
            if [import, export].iter().any(|s| now < s.at || s.age(now) as f64 > stale_seconds * 1000.0) {
                return Err("ha-grid-stale");
            }
            if (import.at - export.at).unsigned_abs() as f64 > self.config.grid_skew_seconds * 1000.0 {
                return Err("ha-grid-skew");
            }
            // A changed member advances the coherent pair; each member's age is
            // independently checked above, so this cannot conceal an old zero.
            Ok(Reading::new(import.at.max(export.at), import.value, export.value, "home-assistant"))
        })();
        match result {
            Ok(reading) => {
                if self.stats.last_reading_at != Some(reading.timestamp) {
                    self.stats.telegrams += 1;
                }
                self.stats.last_reading_at = Some(reading.timestamp);
                self.stats.last_issue = None;
                Some(reading)
            }
            Err(issue) => {
                if self.stats.connected || self.stats.last_issue.is_none() {
                    self.stats.last_issue = Some(issue.into());
                }
                None
            }
        }
    }

    pub fn inverter(&self, now: i64) -> InverterSnapshot {
        if self.config.active_power.is_empty() && self.config.dc_power.is_empty() {
            return InverterSnapshot::disabled();
        }
        let is_fresh = |s: &Sample| {
            self.ready
                && self.stats.connected
                && now >= s.at
                && s.age(now) as f64 <= self.config.inverter_stale_seconds * 1000.0
        };
        let ac = self.samples[2].as_ref().filter(|s| is_fresh(s));
        let dc = self.samples[3].as_ref().filter(|s| is_fresh(s));
        let active_power_w = ac.map(|s| s.value);
        let pv_power_w = dc.map(|s| s.value);
        if active_power_w.is_none() && pv_power_w.is_none() {
            return InverterSnapshot::without_reading(
                InverterStatus::Offline,
                Some("ha-inverter-unavailable-or-stale".into()),
            );
        }
        InverterSnapshot {
            status: InverterStatus::Online,
            active_power_w,
            pv_power_w,
            household_use_w: None,
            timestamp: ac.map(|s| s.at).into_iter().chain(dc.map(|s| s.at)).max(),
            issue: None,
        }
    }

    fn household_use_inputs(
        &self,
        now: i64,
        meter_stale_seconds: f64,
    ) -> Result<(&AtomicSample, &Sample), &'static str> {
        if !self.config.household_use_enabled {
            return Err("ha-household-disabled");
        }
        if self.config.household_topology != HOUSEHOLD_TOPOLOGY_SINGLE_GRID_INVERTER
            || !matches!(self.config.ac_generation_sign, -1 | 1)
        {
            return Err("ha-household-topology");
        }
        if self.config.atomic_grid.is_empty() || self.config.active_power.is_empty() {
            return Err("ha-household-mapping");
        }
        if !self.stats.connected || !self.ready {
            return Err("ha-household-disconnected");
        }
        if !self.atomic.issue(now, meter_stale_seconds.min(30.0)).is_empty() {
            return Err("ha-household-grid-unavailable");
        }
        let grid = self.atomic.sample.as_ref().ok_or("ha-household-grid-unavailable")?;
        let ac = self.samples[2].as_ref().ok_or("ha-household-ac-unavailable")?;
        if !self.live_changes[2] {
            return Err("ha-household-awaiting-live-ac");
        }
        if now < ac.at || ac.age(now) as f64 > self.config.inverter_stale_seconds * 1000.0 {
            return Err("ha-household-ac-stale");
        }
        let event_skew_ms = grid.payload.publisher_at.abs_diff(ac.at) as f64;
        if event_skew_ms > self.config.household_max_event_skew_seconds * 1000.0 {
            return Err("ha-household-event-skew");
        }
        Ok((grid, ac))
    }

    fn household_use_from_inputs(&self, grid: &AtomicSample, ac: &Sample) -> Result<f64, &'static str> {
        let watts =
            grid.payload.import_w - grid.payload.export_w + ac.value * f64::from(self.config.ac_generation_sign);
        if !watts.is_finite() || watts < 0.0 {
            return Err("ha-household-out-of-range");
        }
        Ok(watts)
    }

    fn household_use_value(&self, now: i64, meter_stale_seconds: f64) -> Result<f64, &'static str> {
        let (grid, ac) = self.household_use_inputs(now, meter_stale_seconds)?;
        self.household_use_from_inputs(grid, ac)
    }

    /// Household use is an opt-in estimate. The HA event proximity gate is not
    /// physical sample coherence: AC has no acquisition timestamp.
    pub fn household_use(
        &self,
        reading: Option<&Reading>,
        now: i64,
        meter_stale_seconds: f64,
    ) -> Result<f64, &'static str> {
        let (grid, ac) = self.household_use_inputs(now, meter_stale_seconds)?;
        let reading = reading.ok_or("ha-household-grid-unavailable")?;
        if reading.source != "home-assistant-atomic"
            || reading.timestamp != grid.payload.meter_at
            || reading.grid_import_w != grid.payload.import_w
            || reading.grid_export_w != grid.payload.export_w
        {
            return Err("ha-household-grid-unavailable");
        }
        self.household_use_from_inputs(grid, ac)
    }

    pub fn take_atomic_reset(&mut self) -> bool {
        std::mem::take(&mut self.atomic.reset)
    }
}

#[cfg(test)]
mod heartbeat_tests {
    use super::*;
    use crate::SolarConfig;
    use std::net::TcpListener;
    use std::sync::Arc;

    #[test]
    fn atomic_utc_meter_clock_uses_brussels_local_day_month_and_dst_transitions() {
        let before_spring = belgian_meter_calendar(crate::utc_ms(2027, 3, 28, 0, 59, 0)).unwrap();
        let after_spring = belgian_meter_calendar(crate::utc_ms(2027, 3, 28, 1, 0, 0)).unwrap();
        assert_eq!(before_spring.offset_minutes, 60);
        assert_eq!(before_spring.minute, 119);
        assert_eq!(after_spring.offset_minutes, 120);
        assert_eq!(after_spring.minute, 180);

        let before_autumn = belgian_meter_calendar(crate::utc_ms(2027, 10, 31, 0, 59, 0)).unwrap();
        let after_autumn = belgian_meter_calendar(crate::utc_ms(2027, 10, 31, 1, 0, 0)).unwrap();
        assert_eq!(before_autumn.offset_minutes, 120);
        assert_eq!(before_autumn.minute, 179);
        assert_eq!(after_autumn.offset_minutes, 60);
        assert_eq!(after_autumn.minute, 120);

        let new_year = belgian_meter_calendar(crate::utc_ms(2026, 12, 31, 23, 30, 0)).unwrap();
        assert_eq!(new_year.month, 202701);
        assert_eq!(new_year.minute, 30);
    }

    fn write(ws: &mut WebSocket<TcpStream>, value: Value) {
        ws.send(Message::Text(value.to_string().into())).unwrap();
    }

    fn read(ws: &mut WebSocket<TcpStream>) -> Value {
        serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap()
    }

    // Matches HA ActiveConnection.async_handle: a command ID must be strictly
    // larger than the last accepted ID; subscription events keep their old ID.
    fn accept_id(ws: &mut WebSocket<TcpStream>, request: &Value, last: &mut u64) -> bool {
        let id = request["id"].as_u64().unwrap();
        if id <= *last {
            write(
                ws,
                json!({"id":id,"type":"result","success":false,
                "error":{"code":"id_reuse","message":"Identifier values have to increase."}}),
            );
            return false;
        }
        *last = id;
        true
    }

    #[test]
    fn real_ha_ordering_rejects_old_second_heartbeat() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(socket).unwrap();
            let mut last = 2;
            let first = read(&mut ws);
            assert!(accept_id(&mut ws, &first, &mut last));
            write(&mut ws, json!({"id":first["id"],"type":"pong"}));
            let second = read(&mut ws);
            assert!(!accept_id(&mut ws, &second, &mut last));
        });
        let (mut ws, _) =
            tungstenite::client(format!("ws://{address}/api/websocket"), TcpStream::connect(address).unwrap()).unwrap();
        write(&mut ws, json!({"id":3,"type":"ping"}));
        assert_eq!(read(&mut ws), json!({"id":3,"type":"pong"}));
        write(&mut ws, json!({"id":3,"type":"ping"}));
        let rejected = read(&mut ws);
        assert_eq!(rejected["type"], "result");
        assert_eq!(rejected["error"]["code"], "id_reuse");
        server.join().unwrap();
    }

    #[test]
    fn multiple_correlated_heartbeats_preserve_atomic_subscription_and_reject_bad_responses() {
        for scenario in ["valid", "wrong-pong", "duplicate-pong", "rejected", "unexpected-result"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let config = SolarConfig::from_json(&json!({
                "energySource":{"type":"home-assistant"},
                "homeAssistant":{"endpoint":format!("ws://{address}/api/websocket"),"atomicGrid":"sensor.dsmr_atomic_grid_sample"}
            })).unwrap();
            let runtime = Arc::new(Mutex::new(Runtime::new(config.clone())));
            let stop = Arc::new(AtomicBool::new(false));
            let server_stop = stop.clone();
            let server_runtime = runtime.clone();
            let server = std::thread::spawn(move || {
                let (socket, _) = listener.accept().unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut ws = tungstenite::accept(socket).unwrap();
                write(&mut ws, json!({"type":"auth_required"}));
                assert_eq!(read(&mut ws)["type"], "auth");
                write(&mut ws, json!({"type":"auth_ok"}));
                let mut last = 0;
                let subscribe = read(&mut ws);
                assert!(accept_id(&mut ws, &subscribe, &mut last));
                assert_eq!(subscribe["id"], 1);
                write(&mut ws, json!({"id":1,"type":"result","success":true}));
                let snapshot = read(&mut ws);
                assert!(accept_id(&mut ws, &snapshot, &mut last));
                assert_eq!(snapshot["id"], 2);
                write(&mut ws, json!({"id":2,"type":"result","success":true,"result":[]}));
                let base = crate::now_ms() - 1000;
                for n in 0..4 {
                    let ping = read(&mut ws);
                    assert_eq!(ping["type"], "ping");
                    assert!(accept_id(&mut ws, &ping, &mut last), "fixed client reused command ID");
                    assert_eq!(ping["id"], n + 3);
                    let id = ping["id"].as_u64().unwrap();
                    match scenario {
                        "wrong-pong" => {
                            write(&mut ws, json!({"id":id+1,"type":"pong"}));
                            return;
                        }
                        "rejected" => {
                            write(
                                &mut ws,
                                json!({"id":id,"type":"result","success":false,
                            "error":{"message":"untrusted server text"}}),
                            );
                            return;
                        }
                        "unexpected-result" => {
                            write(&mut ws, json!({"id":id,"type":"result","success":true}));
                            return;
                        }
                        _ => {}
                    }
                    // Interleaved subscription event still uses ID1, with new
                    // acquisition sequence even though both powers stay zero.
                    let at = chrono::DateTime::from_timestamp_millis(base + n * 100).unwrap().to_rfc3339();
                    write(
                        &mut ws,
                        json!({"id":1,"type":"event","event":{"event_type":"state_changed",
                        "data":{"entity_id":"sensor.dsmr_atomic_grid_sample","new_state":{
                            "entity_id":"sensor.dsmr_atomic_grid_sample","state":(n+1).to_string(),
                            "attributes":{"schema_version":1,"session":"synthetic-heartbeat",
                                "sample_sequence":n+1,"meter_timestamp":at,"received_at":at,
                                "import_w":0,"export_w":0}}}}}),
                    );
                    write(&mut ws, json!({"id":id,"type":"pong"}));
                    if scenario == "duplicate-pong" {
                        write(&mut ws, json!({"id":id,"type":"pong"}));
                        return;
                    }
                }
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    let s = lock(&server_runtime).snapshot(crate::now_ms());
                    if s.home_assistant.as_ref().unwrap().entities[0].sample_sequence == Some(4) {
                        break;
                    }
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert!(lock(&server_runtime).snapshot(crate::now_ms()).reading_at.is_some());
                server_stop.store(true, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(600));
            });
            let result = run_authenticated_session(
                &runtime,
                &stop,
                &config.home_assistant,
                "synthetic-token",
                Duration::from_millis(60),
            );
            match scenario {
                "valid" => assert_eq!(result, Ok(())),
                "rejected" => assert_eq!(result, Err("ha-heartbeat-rejected")),
                _ => assert_eq!(result, Err("ha-unexpected-response")),
            }
            server.join().unwrap();
        }
    }
}

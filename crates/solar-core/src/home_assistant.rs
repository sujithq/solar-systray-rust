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
const SAMPLE_COUNT: usize = 4;

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
    pub night_baseline_enabled: bool,
    pub night_baseline_refresh_at: String,
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
            night_baseline_enabled: v["nightBaseline"]["enabled"].as_bool().ok_or_else(error)?,
            night_baseline_refresh_at: v["nightBaseline"]["refreshAt"].as_str().ok_or_else(error)?.into(),
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
            || crate::ha_baseline::refresh_minute(&config.night_baseline_refresh_at).is_none()
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
            "nightBaseline": {
                "enabled": self.night_baseline_enabled,
                "refreshAt": self.night_baseline_refresh_at
            },
            "householdUse": {
                "enabled": self.household_use_enabled,
                "topology": self.household_topology,
                "generationSign": self.ac_generation_sign,
                "maxEventSkewSeconds": self.household_max_event_skew_seconds
            }
        })
    }

    fn entity_index(&self, id: &str) -> Option<usize> {
        [&self.grid_import, &self.grid_export, &self.active_power, &self.dc_power]
            .iter()
            .position(|entity| !entity.is_empty() && entity.as_str() == id)
    }

    fn entity_name(&self, index: usize) -> Option<&str> {
        [self.grid_import.as_str(), self.grid_export.as_str(), self.active_power.as_str(), self.dc_power.as_str()]
            .get(index)
            .copied()
            .filter(|entity| !entity.is_empty())
    }

    fn relevant(&self, id: &str) -> bool {
        (if self.atomic_grid.is_empty() {
            id == self.grid_import || id == self.grid_export
        } else {
            id == self.atomic_grid
        }) || self.entity_index(id).is_some_and(|index| matches!(index, INVERTER_AC | INVERTER_DC))
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
    optional: AtomicOptional,
}

#[derive(Clone, PartialEq)]
struct AtomicOptional {
    demand: Result<f64, &'static str>,
    peak: Result<(f64, i64), &'static str>,
    counters: Result<EnergyTotals, &'static str>,
}

fn atomic_optional(attributes: &Value, meter_at: i64) -> AtomicOptional {
    let status = &attributes["optional_status"];
    let group = |name: &str, fields: &[&str]| -> Result<(), &'static str> {
        if status.is_null() {
            return Err("ha-atomic-optional-not-published");
        }
        match status[name].as_str() {
            Some("available") if fields.iter().all(|field| !attributes[*field].is_null()) => Ok(()),
            Some("missing" | "invalid")
                if fields.iter().all(|field| attributes.get(*field).is_some_and(Value::is_null)) =>
            {
                Err(if status[name] == "missing" { "ha-atomic-optional-missing" } else { "ha-atomic-optional-invalid" })
            }
            _ => Err("ha-atomic-optional-invalid"),
        }
    };
    // Schema-v1 suffixes define W and Wh, never HA display-unit metadata.
    let power = |field: &str| {
        attributes[field]
            .as_i64()
            .filter(|value| (0..=1_000_000).contains(value))
            .map(|value| value as f64)
            .ok_or("ha-atomic-optional-invalid")
    };
    let demand = group("demand", &["demand_w"]).and_then(|_| power("demand_w"));
    let peak = (|| {
        group("monthly_peak", &["monthly_peak_w", "monthly_peak_at", "monthly_peak_month"])?;
        let watts = power("monthly_peak_w")?;
        let at = utc_timestamp(attributes["monthly_peak_at"].as_str().ok_or("ha-atomic-optional-invalid")?)
            .map_err(|_| "ha-atomic-optional-invalid")?;
        let calendar = belgian_meter_calendar(at).ok_or("ha-atomic-optional-invalid")?;
        let month = attributes["monthly_peak_month"].as_str().ok_or("ha-atomic-optional-invalid")?;
        let expected = format!("{:04}-{:02}", calendar.month / 100, calendar.month % 100);
        if at <= 0 || at > meter_at || month != expected {
            return Err("ha-atomic-optional-invalid");
        }
        Ok((watts, calendar.month))
    })();
    let counters = (|| {
        let mut totals = [0_i64; 4];
        for (index, name) in
            ["import_tariff_1", "import_tariff_2", "export_tariff_1", "export_tariff_2"].iter().enumerate()
        {
            let field = format!("{name}_wh");
            group(name, &[&field])?;
            totals[index] = attributes[&field]
                .as_i64()
                .filter(|value| (0..=999_999_999).contains(value))
                .ok_or("ha-atomic-optional-invalid")?;
        }
        Ok(EnergyTotals {
            import_tariff1_wh: totals[0],
            import_tariff2_wh: totals[1],
            export_tariff1_wh: totals[2],
            export_tariff2_wh: totals[3],
        })
    })();
    AtomicOptional { demand, peak, counters }
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
pub(crate) struct BelgianCalendar {
    month: i64,
    pub(crate) day: i64,
    pub(crate) minute: i64,
    pub(crate) offset_minutes: i64,
}

fn last_sunday(year: i64, month: u32) -> i64 {
    let (next_year, next_month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let last_day = days_from_civil(next_year, next_month, 1) - 1;
    last_day - (last_day + 4).rem_euclid(7)
}

pub(crate) fn belgian_meter_calendar(utc_ms: i64) -> Option<BelgianCalendar> {
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
        optional: atomic_optional(&value["attributes"], meter_at),
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

/// Separate read-only connection: history never enters HaState or live freshness.
pub(crate) fn recorded_night(
    config: &crate::SolarConfig,
    stop: &AtomicBool,
    window: crate::ha_baseline::NightWindow,
) -> Result<crate::ha_baseline::RecordedBaseline, &'static str> {
    let ha = &config.home_assistant;
    let token = std::env::var(&ha.token_env).map_err(|_| "history-token-missing")?;
    if token.is_empty() || token.len() > 8192 || token.bytes().any(|b| b.is_ascii_control()) {
        return Err("history-token-invalid");
    }
    recorded_night_with_token(config, stop, window, &token)
}

fn recorded_night_with_token(
    config: &crate::SolarConfig,
    stop: &AtomicBool,
    window: crate::ha_baseline::NightWindow,
    token: &str,
) -> Result<crate::ha_baseline::RecordedBaseline, &'static str> {
    let mut samples = crate::ha_baseline::NightSamples::new(window);
    query_history(config, stop, token, window.start, window.end, 32 * MAX_MESSAGE, |result, start, end| {
        append_recorded_states(&mut samples, config, result, start, end)
    })?;
    samples.finish(crate::now_ms())
}

pub(crate) fn recorded_reported_night(
    config: &crate::SolarConfig,
    stop: &AtomicBool,
    window: crate::ha_baseline::NightWindow,
    progress: impl FnMut(crate::ha_baseline::NightCoverage),
) -> Result<crate::ha_baseline::RecordedBaseline, &'static str> {
    let token = std::env::var(&config.home_assistant.token_env).map_err(|_| "history-token-missing")?;
    if token.is_empty() || token.len() > 8192 || token.bytes().any(|b| b.is_ascii_control()) {
        return Err("history-token-invalid");
    }
    recorded_reported_night_with_token(config, stop, window, &token, progress)
}

fn recorded_reported_night_with_token(
    config: &crate::SolarConfig,
    stop: &AtomicBool,
    window: crate::ha_baseline::NightWindow,
    token: &str,
    mut progress: impl FnMut(crate::ha_baseline::NightCoverage),
) -> Result<crate::ha_baseline::RecordedBaseline, &'static str> {
    let entity = config.home_assistant.grid_import.clone();
    let mut samples = crate::ha_baseline::ReportedNightSamples::new(window, entity.clone())?;
    progress(samples.coverage(None));
    let query = query_entity_history(
        config,
        stop,
        token,
        HistoryQuery {
            start: window.start,
            end: window.end,
            budget: 32 * MAX_MESSAGE,
            chunk_minutes: 15,
            entities: vec![entity],
            include_start: true,
        },
        |result, start, end| {
            let result = samples.append(result, start, end);
            progress(samples.coverage(result.as_ref().err().copied()));
            result
        },
        |_, _| {},
    );
    if let Err(issue) = query {
        progress(samples.coverage(Some(issue)));
        return Err(issue);
    }
    let mut coverage = samples.coverage(None);
    let result = samples.finish(crate::now_ms());
    coverage.issue = result.as_ref().err().map(|issue| (*issue).into());
    progress(coverage);
    result
}

pub(crate) fn recorded_daily(
    config: &crate::SolarConfig,
    stop: &AtomicBool,
    request: &crate::ha_daily::HistoryRequest,
) -> Result<crate::ha_daily::CounterBaseline, &'static str> {
    let token = std::env::var(&config.home_assistant.token_env).map_err(|_| "history-token-missing")?;
    if token.is_empty() || token.len() > 8192 || token.bytes().any(|b| b.is_ascii_control()) {
        return Err("history-token-invalid");
    }
    recorded_daily_with_token(config, stop, request, &token)
}

fn recorded_daily_with_token(
    config: &crate::SolarConfig,
    stop: &AtomicBool,
    request: &crate::ha_daily::HistoryRequest,
    token: &str,
) -> Result<crate::ha_daily::CounterBaseline, &'static str> {
    let (start, end) = request.window().ok_or("daily-history-calendar")?;
    if end <= start || end - start > 25 * 3_600_000 + 60_000 {
        return Err("daily-history-window");
    }
    let mut samples = crate::ha_daily::HistorySamples::new(request.clone());
    let (midnight, _) = crate::ha_daily::day_bounds(request.anchor.meter_at).ok_or("daily-history-calendar")?;
    query_history(config, stop, token, start, end, 128 * MAX_MESSAGE, |result, start, end| {
        for state in recorded_states(result, &config.home_assistant.atomic_grid)? {
            let payload = recorded_payload(state, start, end)?;
            if payload.meter_at < midnight {
                continue;
            }
            let totals = payload.optional.counters.map_err(|_| "daily-history-missing-atomic-counters")?;
            samples.add(crate::ha_daily::CounterSample {
                session: payload.session,
                sequence: payload.sequence,
                meter_at: payload.meter_at,
                received_at: payload.publisher_at,
                totals,
            })?;
        }
        Ok(())
    })?;
    samples.finish()
}

fn query_history(
    config: &crate::SolarConfig,
    stop: &AtomicBool,
    token: &str,
    start: i64,
    end: i64,
    budget: usize,
    mut append: impl FnMut(&Value, i64, i64) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    let chunk_minutes = if budget > 32 * MAX_MESSAGE { 5 } else { 15 };
    query_entity_history(
        config,
        stop,
        token,
        HistoryQuery {
            start,
            end,
            budget,
            chunk_minutes,
            entities: vec![config.home_assistant.atomic_grid.clone()],
            include_start: false,
        },
        &mut append,
        |_, _| {},
    )
}

struct HistoryQuery {
    start: i64,
    end: i64,
    budget: usize,
    chunk_minutes: i64,
    entities: Vec<String>,
    include_start: bool,
}

pub(crate) fn recorded_reported_energy(
    config: &crate::SolarConfig,
    stop: &AtomicBool,
    end: i64,
    progress: impl FnMut(&'static str, u32),
) -> Result<crate::ha_reported_energy::ReportedEnergy, &'static str> {
    let token = std::env::var(&config.home_assistant.token_env).map_err(|_| "history-token-missing")?;
    if token.is_empty() || token.len() > 8192 || token.bytes().any(|b| b.is_ascii_control()) {
        return Err("history-token-invalid");
    }
    recorded_reported_with_token(config, stop, end, &token, progress)
}

fn recorded_reported_with_token(
    config: &crate::SolarConfig,
    stop: &AtomicBool,
    end: i64,
    token: &str,
    progress: impl FnMut(&'static str, u32),
) -> Result<crate::ha_reported_energy::ReportedEnergy, &'static str> {
    let (start, _) = crate::ha_daily::day_bounds(end).ok_or("reported-energy-calendar")?;
    let ha = &config.home_assistant;
    let entities = [
        ha.energy_import_tariff1.clone(),
        ha.energy_import_tariff2.clone(),
        ha.energy_export_tariff1.clone(),
        ha.energy_export_tariff2.clone(),
    ];
    let mut samples = crate::ha_reported_energy::ReportedSamples::new(start, end, entities.clone())?;
    query_entity_history(
        config,
        stop,
        token,
        HistoryQuery {
            start,
            end,
            budget: 128 * MAX_MESSAGE,
            chunk_minutes: 5,
            entities: entities.to_vec(),
            include_start: true,
        },
        |result, start, end| samples.append(result, start, end),
        progress,
    )?;
    samples.finish()
}

fn query_entity_history(
    config: &crate::SolarConfig,
    stop: &AtomicBool,
    token: &str,
    query: HistoryQuery,
    mut append: impl FnMut(&Value, i64, i64) -> Result<(), &'static str>,
    mut progress: impl FnMut(&'static str, u32),
) -> Result<(), &'static str> {
    let HistoryQuery { start, end, mut budget, chunk_minutes: chunk, entities, include_start } = query;
    let ha = &config.home_assistant;
    let overall = Instant::now() + Duration::from_secs(180);
    progress("connecting", 0);
    let mut ws = open(ha).map_err(|_| "history-connect-or-tls")?;
    let read = |ws: &mut Socket, budget: &mut usize| -> Result<Value, &'static str> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err("history-cancelled");
            }
            if Instant::now() >= deadline || Instant::now() >= overall {
                return Err("history-response-timeout");
            }
            transport(ws)?.deadline = Some(Instant::now() + Duration::from_millis(500));
            match ws.read() {
                Ok(Message::Text(text)) => {
                    if text.len() > *budget {
                        return Err("history-byte-limit");
                    }
                    *budget -= text.len();
                    return serde_json::from_str(&text).map_err(|_| "history-json");
                }
                Ok(Message::Ping(_) | Message::Pong(_)) => ws.flush().map_err(|_| "history-write")?,
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                _ => return Err("history-read-or-protocol"),
            }
        }
    };
    progress("authenticating", 0);
    if read(&mut ws, &mut budget)?["type"] != "auth_required" {
        return Err("history-auth-protocol");
    }
    send(&mut ws, json!({"type":"auth","access_token":token}))?;
    if read(&mut ws, &mut budget)?["type"] != "auth_ok" {
        return Err("history-auth-rejected");
    }
    let mut start = start;
    let mut id = 1_u64;
    progress("querying", 0);
    while start < end {
        // Smaller daily chunks keep extended attributes below the 1 MiB frame limit.
        let chunk_end = (start + chunk * 60_000).min(end);
        let iso = |at| chrono::DateTime::from_timestamp_millis(at).map(|time| time.to_rfc3339());
        send(
            &mut ws,
            json!({
                "id":id,"type":"history/history_during_period",
                "start_time":iso(start),"end_time":iso(chunk_end),
                "entity_ids":entities,
                "include_start_time_state":include_start,"significant_changes_only":false,
                "minimal_response":false,"no_attributes":false
            }),
        )?;
        let result = read(&mut ws, &mut budget)?;
        if result["type"] != "result" || result["id"].as_u64() != Some(id) {
            return Err("history-response-correlation");
        }
        if result["success"] != true {
            return Err("history-query-rejected");
        }
        append(&result["result"], start, chunk_end)?;
        progress("querying", id as u32);
        start = chunk_end;
        id += 1;
    }
    Ok(())
}

fn recorded_states<'a>(result: &'a Value, entity: &str) -> Result<&'a [Value], &'static str> {
    let object = result.as_object().ok_or("history-response-shape")?;
    if object.keys().any(|key| key != entity) {
        return Err("history-response-entity");
    }
    match object.get(entity) {
        None => Ok(&[]),
        Some(states) => {
            states.as_array().filter(|s| s.len() <= 10_000).map(Vec::as_slice).ok_or("history-response-limit")
        }
    }
}

fn recorded_payload(state: &Value, start: i64, end: i64) -> Result<AtomicPayload, &'static str> {
    let normalized = json!({"state":state["s"],"attributes":state["a"]});
    let received_at = state["a"]["received_at"].as_str().ok_or("history-missing-acquisition")?;
    let received_at = utc_timestamp(received_at).map_err(|_| "history-invalid-acquisition")?;
    let payload = atomic_payload(&normalized, received_at).map_err(|_| "history-invalid-atomic-sample")?;
    let updated = state["lu"].as_f64().filter(|v| v.is_finite()).ok_or("history-missing-recorder-time")? * 1000.0;
    if !(start as f64..=end as f64).contains(&updated) || (updated - received_at as f64).abs() >= 30_000.0 {
        return Err("history-recorder-time-mismatch");
    }
    Ok(payload)
}

fn append_recorded_states(
    samples: &mut crate::ha_baseline::NightSamples,
    config: &crate::SolarConfig,
    result: &Value,
    start: i64,
    end: i64,
) -> Result<(), &'static str> {
    for state in recorded_states(result, &config.home_assistant.atomic_grid)? {
        // HA's history WebSocket API returns compressed s/a/lu/lc states.
        // Attributes must remain present; statistics/means cannot replace them.
        let payload = recorded_payload(state, start, end)?;
        if !crate::ha_baseline::validate_night_sample(config, payload.meter_at) {
            samples.reject("history-daytime-sample");
            continue;
        }
        samples.add(payload.session, payload.sequence, payload.meter_at, payload.publisher_at, payload.import_w);
    }
    Ok(())
}

#[cfg(test)]
mod recorded_history_tests {
    use super::*;
    use crate::ha_baseline::{NightSamples, NightWindow};
    use std::net::TcpListener;

    fn iso(at: i64) -> String {
        chrono::DateTime::from_timestamp_millis(at).unwrap().to_rfc3339()
    }

    fn config() -> crate::SolarConfig {
        crate::SolarConfig::from_json(&json!({
            "energySource":{"type":"home-assistant"},
            "homeAssistant":{"atomicGrid":"sensor.fixture_atomic"},
            "location":{"latitude":50.85,"longitude":4.35}
        }))
        .unwrap()
    }

    fn state(at: i64, sequence: u64) -> Value {
        json!({"s":sequence.to_string(),"lu":at as f64 / 1000.0,"a":{
            "schema_version":1,"session":"fixture-session","sample_sequence":sequence,
            "meter_timestamp":iso(at),"received_at":iso(at),"import_w":320.0,"export_w":0.0
        }})
    }

    fn counter_state(at: i64, midnight: i64) -> Value {
        let sequence = ((at - midnight) / 10_000 + 1) as u64;
        let mut state = state(at, sequence);
        state["a"]["import_tariff_1_wh"] = json!(100_000 + sequence);
        state["a"]["import_tariff_2_wh"] = json!(0);
        state["a"]["export_tariff_1_wh"] = json!(20_000 + sequence);
        state["a"]["export_tariff_2_wh"] = json!(0);
        state["a"]["optional_status"] = json!({
            "demand":"missing","monthly_peak":"missing","import_tariff_1":"available",
            "import_tariff_2":"available","export_tariff_1":"available","export_tariff_2":"available"
        });
        state
    }

    #[test]
    fn reported_night_queries_only_power_with_held_states_and_preserves_static_minutes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config();
        config.home_assistant.endpoint = format!("ws://{}/api/websocket", listener.local_addr().unwrap());
        let start = utc_timestamp("2026-10-25T00:00:00Z").unwrap();
        let window = NightWindow { start, end: start + 40 * 60_000 };
        let entity = config.home_assistant.grid_import.clone();
        let server =
            std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut socket = tungstenite::accept(stream).unwrap();
                socket.send(Message::Text(json!({"type":"auth_required"}).to_string().into())).unwrap();
                let _ = socket.read().unwrap();
                socket.send(Message::Text(json!({"type":"auth_ok"}).to_string().into())).unwrap();
                for n in 0..3 {
                    let query: Value = serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
                    assert_eq!(query["entity_ids"], json!([entity]));
                    assert_eq!(query["include_start_time_state"], true);
                    assert_eq!(query["no_attributes"], false);
                    assert_eq!(query["start_time"], iso(start + n * 15 * 60_000));
                    socket.send(Message::Text(json!({"id":query["id"],"type":"result","success":true,
                    "result":{entity.clone():[{"s":"0.321","lu":(start+n*15*60_000)as f64/1000.0,
                        "a":{"unit_of_measurement":"kW","device_class":"power","state_class":"measurement"}}]}
                }).to_string().into())).unwrap();
                }
            });
        let baseline =
            recorded_reported_night_with_token(&config, &AtomicBool::new(false), window, "fixture-token", |_| {})
                .unwrap();
        assert_eq!(baseline.watts, 321.0);
        assert_eq!(baseline.samples, 40);
        assert_eq!(baseline.provenance, crate::ha_baseline::REPORTED_PROVENANCE);
        server.join().unwrap();
    }

    #[test]
    fn original_tariff_history_requests_held_boundary_states_and_accepts_static_values() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config();
        config.home_assistant.endpoint = format!("ws://{}/api/websocket", listener.local_addr().unwrap());
        let start = utc_timestamp("2027-01-14T23:00:00Z").unwrap();
        let end = start + 650_000;
        let ids = [
            config.home_assistant.energy_import_tariff1.clone(),
            config.home_assistant.energy_import_tariff2.clone(),
            config.home_assistant.energy_export_tariff1.clone(),
            config.home_assistant.energy_export_tariff2.clone(),
        ];
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            ws.send(Message::Text(json!({"type":"auth_required"}).to_string().into())).unwrap();
            let _ = ws.read().unwrap();
            ws.send(Message::Text(json!({"type":"auth_ok"}).to_string().into())).unwrap();
            for n in 0..3 {
                let query: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
                assert_eq!(query["entity_ids"], json!(ids));
                assert_eq!(query["include_start_time_state"], true);
                assert_eq!(query["minimal_response"], false);
                assert_eq!(query["no_attributes"], false);
                assert_eq!(query["significant_changes_only"], false);
                let from = utc_timestamp(query["start_time"].as_str().unwrap()).unwrap();
                assert_eq!(from, start + n * 300_000);
                let mut result = serde_json::Map::new();
                for id in &ids {
                    // HA projects an older retained state to the requested boundary.
                    result.insert(
                        id.clone(),
                        json!([{"s":"10.000","lu":from as f64/1000.0,
                        "a":{"unit_of_measurement":"kWh","device_class":"energy","state_class":"total_increasing"}}]),
                    );
                }
                ws.send(Message::Text(
                    json!({"id":query["id"],"type":"result","success":true,"result":result}).to_string().into(),
                ))
                .unwrap();
            }
        });
        let mut progress = Vec::new();
        let result =
            recorded_reported_with_token(&config, &AtomicBool::new(false), end, "fixture-token", |phase, n| {
                progress.push((phase, n));
            })
            .unwrap();
        assert_eq!(progress.first(), Some(&("connecting", 0)));
        assert_eq!(progress.get(1), Some(&("authenticating", 0)));
        assert_eq!(progress.last(), Some(&("querying", 3)));
        assert_eq!(result.import_wh, 0);
        assert_eq!(result.export_wh, 0);
        assert_eq!(result.as_of, end);
        server.join().unwrap();
    }

    #[test]
    fn daily_websocket_history_recovers_midnight_without_feeding_live_freshness() {
        for fault in ["none", "old-schema", "no-midnight", "gap", "unavailable", "wrong-entity"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut config = config();
            config.home_assistant.endpoint = format!("ws://{}/api/websocket", listener.local_addr().unwrap());
            let start = utc_timestamp("2026-10-24T22:00:00Z").unwrap();
            let anchor = start + 40 * 60_000;
            let mut runtime = crate::Runtime::new(config.clone());
            runtime.ha_connected();
            runtime.ha_ready();
            let value = counter_state(anchor, start);
            runtime.ha_state("sensor.fixture_atomic", &json!({"state":value["s"],"attributes":value["a"]}), anchor);
            let request = runtime.daily_history_request(anchor).unwrap();
            let server = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut ws = tungstenite::accept(stream).unwrap();
                ws.send(Message::Text(json!({"type":"auth_required"}).to_string().into())).unwrap();
                let _ = ws.read().unwrap();
                ws.send(Message::Text(json!({"type":"auth_ok"}).to_string().into())).unwrap();
                while let Ok(message) = ws.read() {
                    let query: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                    assert_eq!(query["type"], "history/history_during_period");
                    assert_eq!(query["entity_ids"], json!(["sensor.fixture_atomic"]));
                    for flag in
                        ["minimal_response", "significant_changes_only", "no_attributes", "include_start_time_state"]
                    {
                        assert_eq!(query[flag], false);
                    }
                    let from = utc_timestamp(query["start_time"].as_str().unwrap()).unwrap();
                    let to = utc_timestamp(query["end_time"].as_str().unwrap()).unwrap();
                    assert!(to - from <= 5 * 60_000);
                    let mut states = Vec::new();
                    for at in (start..=anchor).step_by(10_000) {
                        if at < from || at >= to {
                            continue;
                        }
                        if fault == "no-midnight" && at == start {
                            continue;
                        }
                        if fault == "gap" && (start + 60_000..start + 90_000).contains(&at) {
                            continue;
                        }
                        let mut value = counter_state(at, start);
                        if fault == "old-schema" {
                            value = state(at, (at - start) as u64 + 1);
                        }
                        if fault == "unavailable" && at == start + 60_000 {
                            value["s"] = json!("unavailable");
                        }
                        states.push(value);
                    }
                    let entity = if fault == "wrong-entity" { "sensor.wrong" } else { "sensor.fixture_atomic" };
                    let result = json!({"id":query["id"],"type":"result","success":true,"result":{entity:states}});
                    if ws.send(Message::Text(result.to_string().into())).is_err() {
                        break;
                    }
                }
            });
            let result = recorded_daily_with_token(&config, &AtomicBool::new(false), &request, "fixture-token");
            let expected = match fault {
                "none" => None,
                "old-schema" => Some("daily-history-missing-atomic-counters"),
                "no-midnight" => Some("daily-history-no-midnight-sample"),
                "gap" => Some("daily-history-gap"),
                "unavailable" => Some("history-invalid-atomic-sample"),
                _ => Some("history-response-entity"),
            };
            assert_eq!(result.as_ref().err().copied(), expected, "{fault}");
            runtime.complete_daily_history(&request, result);
            let snapshot = runtime.snapshot(anchor);
            assert_eq!(snapshot.reading_at, Some(anchor), "history cannot advance live measurement time");
            assert_eq!(snapshot.daily_coverage.as_ref().unwrap().midnight, fault == "none");
            assert_eq!(snapshot.daily_energy.import_wh, Some(if fault == "none" { 240 } else { 0 }));
            assert!(!runtime.snapshot(anchor + 30_000).daily_energy.available);
            server.join().unwrap();
        }
    }

    #[test]
    fn websocket_history_requests_real_attributes_in_bounded_correlated_chunks() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config();
        config.home_assistant.endpoint = format!("ws://{}/api/websocket", listener.local_addr().unwrap());
        let start = chrono::DateTime::parse_from_rfc3339("2026-10-25T00:00:00Z").unwrap().timestamp_millis();
        let window = NightWindow { start, end: start + 40 * 60_000 };
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            socket.send(Message::Text(json!({"type":"auth_required"}).to_string().into())).unwrap();
            let auth: Value = serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(auth["type"], "auth");
            socket.send(Message::Text(json!({"type":"auth_ok"}).to_string().into())).unwrap();
            let mut from = start;
            let mut command_id = 1;
            while from < window.end {
                let query: Value = serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
                assert_eq!(query["id"], command_id);
                assert_eq!(query["type"], "history/history_during_period");
                assert_eq!(query["entity_ids"], json!(["sensor.fixture_atomic"]));
                assert_eq!(query["no_attributes"], false);
                assert_eq!(query["minimal_response"], false);
                assert_eq!(query["significant_changes_only"], false);
                assert_eq!(query["include_start_time_state"], false);
                assert_eq!(query["start_time"], iso(from));
                let to = (from + 15 * 60_000).min(window.end);
                assert_eq!(query["end_time"], iso(to));
                let states: Vec<_> = (from..to).step_by(10_000).map(|at| state(at, (at - start) as u64 + 1)).collect();
                let result = json!({"id":command_id,"type":"result","success":true,
                    "result":{"sensor.fixture_atomic":states}});
                socket.send(Message::Text(result.to_string().into())).unwrap();
                command_id += 1;
                from = to;
            }
        });
        let baseline = recorded_night_with_token(&config, &AtomicBool::new(false), window, "synthetic-token").unwrap();
        assert_eq!(baseline.watts, 320.0);
        assert_eq!(baseline.samples, 40);
        server.join().unwrap();
    }

    #[test]
    fn malformed_unavailable_or_statistics_history_never_becomes_a_baseline() {
        let config = config();
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-25T00:00:00Z").unwrap().timestamp_millis();
        let mut bad = vec![
            json!({"s":"unknown","lu":at as f64 / 1000.0}),
            json!({"s":"unavailable","lu":at as f64 / 1000.0}),
            json!({"s":"320","lu":at as f64 / 1000.0}),
        ];
        let mut invalid = state(at, 1);
        invalid["a"]["import_w"] = json!(-1);
        bad.push(invalid);
        let mut delayed = state(at, 1);
        delayed["lu"] = json!((at + 30_000) as f64 / 1000.0);
        bad.push(delayed);
        for value in bad {
            let mut samples = NightSamples::new(NightWindow { start: at, end: at + 40 * 60_000 });
            assert!(
                append_recorded_states(
                    &mut samples,
                    &config,
                    &json!({"sensor.fixture_atomic":[value]}),
                    at,
                    at + 60_000
                )
                .is_err()
            );
        }
        let mut samples = NightSamples::new(NightWindow { start: at, end: at + 40 * 60_000 });
        assert_eq!(
            append_recorded_states(&mut samples, &config, &json!({"sensor.wrong":[]}), at, at + 60_000),
            Err("history-response-entity")
        );
    }

    #[test]
    fn history_result_ids_and_success_are_not_silently_accepted() {
        for (result, expected) in [
            (json!({"id":2,"type":"result","success":true,"result":{}}), "history-response-correlation"),
            (
                json!({"id":1,"type":"result","success":false,"error":{"message":"untrusted server text"}}),
                "history-query-rejected",
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut config = config();
            config.home_assistant.endpoint = format!("ws://{}/api/websocket", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut socket = tungstenite::accept(stream).unwrap();
                socket.send(Message::Text(json!({"type":"auth_required"}).to_string().into())).unwrap();
                let _ = socket.read().unwrap();
                socket.send(Message::Text(json!({"type":"auth_ok"}).to_string().into())).unwrap();
                let _ = socket.read().unwrap();
                socket.send(Message::Text(result.to_string().into())).unwrap();
            });
            let window = NightWindow { start: 1_800_000_000_000, end: 1_800_002_400_000 };
            assert_eq!(
                recorded_night_with_token(&config, &AtomicBool::new(false), window, "synthetic-token"),
                Err(expected)
            );
            server.join().unwrap();
        }
    }
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
    pub(crate) fn counter_sample(&self, now: i64) -> Option<crate::ha_daily::CounterSample> {
        let payload = self.optional_payload(now).ok()?;
        Some(crate::ha_daily::CounterSample {
            session: payload.session.clone(),
            sequence: payload.sequence,
            meter_at: payload.meter_at,
            received_at: payload.publisher_at,
            totals: *payload.optional.counters.as_ref().ok()?,
        })
    }

    fn optional_payload(&self, now: i64) -> Result<&AtomicPayload, &'static str> {
        if !self.stats.connected || !self.ready {
            return Err("ha-meter-data-disconnected");
        }
        if self.config.atomic_grid.is_empty() {
            return Err("ha-meter-data-needs-atomic-grid");
        }
        if !self.atomic.issue(now, 30.0).is_empty() {
            return Err("ha-meter-data-grid-unavailable");
        }
        self.atomic.sample.as_ref().map(|sample| &sample.payload).ok_or("ha-meter-data-grid-unavailable")
    }

    /// Optional values belong exclusively to this accepted atomic sequence.
    pub fn enrich_reading(&self, reading: &mut Reading, now: i64) {
        if reading.source != "home-assistant-atomic" {
            return;
        }
        let Ok(payload) = self.optional_payload(now) else { return };
        if payload.meter_at != reading.timestamp {
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

        reading.current_demand_w = payload.optional.demand.ok();
        reading.monthly_peak_w = None;
        reading.monthly_peak_month = None;
        if let Ok((watts, month)) = payload.optional.peak
            && month == calendar.month
        {
            reading.monthly_peak_w = Some(watts);
            reading.monthly_peak_month = Some(month);
        }
        reading.energy_totals_wh = payload.optional.counters.as_ref().ok().cloned();
    }

    pub fn peak_data_issue(&self, now: i64) -> Option<&'static str> {
        (|| {
            let payload = self.optional_payload(now)?;
            payload.optional.demand?;
            let (_, month) = payload.optional.peak?;
            let calendar = belgian_meter_calendar(payload.meter_at).ok_or("ha-meter-clock-unsupported")?;
            if month != calendar.month {
                return Err("ha-atomic-optional-previous-month");
            }
            Ok(())
        })()
        .err()
    }

    pub fn daily_data_issue(&self, now: i64) -> Option<&'static str> {
        self.optional_payload(now)
            .and_then(|payload| payload.optional.counters.as_ref().map(|_| ()).map_err(|issue| *issue))
            .err()
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
            (INVERTER_AC..=INVERTER_DC).filter_map(|index| self.config.entity_name(index).map(|id| (id, Some(index)))),
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
                            let limit =
                                if i < INVERTER_AC { meter_stale_seconds } else { self.config.inverter_stale_seconds };
                            if !self.stats.connected {
                                Some("ha-disconnected".into())
                            } else if s.is_none() {
                                Some("ha-entity-unavailable".into())
                            } else if s.is_some_and(|s| now < s.at || s.age(now) as f64 > limit * 1000.0) {
                                Some("ha-entity-stale".into())
                            } else if i < INVERTER_AC && !self.live_changes[i] {
                                Some("ha-awaiting-new-grid-evidence".into())
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
            if self.config.entity_index(id).is_none_or(|index| !matches!(index, INVERTER_AC | INVERTER_DC)) {
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
                GRID_IMPORT | GRID_EXPORT if unit == "kW" => raw * 1000.0,
                INVERTER_AC if unit == "W" => raw,
                INVERTER_DC if unit == "W" => raw,
                _ => return Err("ha-unit"),
            };
            let valid_value = match index {
                INVERTER_AC => (-1_000_000.0..=1_000_000.0).contains(&normalized),
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

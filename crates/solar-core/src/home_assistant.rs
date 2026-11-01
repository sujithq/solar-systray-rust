//! Independent WebSocket-only Home Assistant feed. HA state changes are provisional
//! acquisition evidence; unchanged states cannot keep a recommendation alive.

use crate::config::ConfigError;
use crate::reading::Reading;
use crate::slimmelezer::MeterStats;
use crate::sungrow::{InverterSnapshot, InverterStatus};
use serde_json::{Value, json};
use std::net::{IpAddr, SocketAddr};

pub const MAX_MESSAGE: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq)]
pub struct HaConfig {
    pub endpoint: String,
    pub token_env: String,
    pub grid_import: String,
    pub grid_export: String,
    pub active_power: String,
    pub inverter_stale_seconds: f64,
    pub grid_skew_seconds: f64,
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
            active_power: text("activePower")?.into(),
            inverter_stale_seconds: v["inverterStaleSeconds"].as_f64().ok_or_else(error)?,
            grid_skew_seconds: v["gridSkewSeconds"].as_f64().ok_or_else(error)?,
        };
        config.address().map_err(|_| error())?;
        let env = config.token_env.as_bytes();
        let entity = |s: &str| {
            s.starts_with("sensor.")
                && (8..=128).contains(&s.len())
                && s[7..].bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        };
        if env.is_empty()
            || env.len() > 128
            || !(env[0].is_ascii_uppercase() || env[0] == b'_')
            || !env.iter().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_')
            || !entity(&config.grid_import)
            || !entity(&config.grid_export)
            || (!config.active_power.is_empty() && !entity(&config.active_power))
            || config.grid_import == config.grid_export
            || config.active_power == config.grid_import
            || config.active_power == config.grid_export
            || !config.inverter_stale_seconds.is_finite()
            || !(1.0..=86400.0).contains(&config.inverter_stale_seconds)
            || !config.grid_skew_seconds.is_finite()
            || !(0.0..=30.0).contains(&config.grid_skew_seconds)
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
            "activePower": self.active_power, "inverterStaleSeconds": self.inverter_stale_seconds,
            "gridSkewSeconds": self.grid_skew_seconds
        })
    }
}

#[derive(Clone)]
struct Sample {
    watts: f64,
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
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HaDiagnostics {
    pub connected: bool,
    pub initialized: bool,
    pub evidence: String,
    pub entities: Vec<EntityDiagnostic>,
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
    samples: [Option<Sample>; 3],
    // High-water marks survive removal/reconnect so replay cannot renew old samples.
    history: [Option<Sample>; 3],
    observations: [Option<Observation>; 3],
    issues: [Option<String>; 3],
    live_changes: [bool; 3],
    pub stats: MeterStats,
    ready: bool,
}

impl HaState {
    pub fn new(config: HaConfig) -> Self {
        Self {
            config,
            samples: [None, None, None],
            history: [None, None, None],
            observations: [None, None, None],
            issues: [None, None, None],
            live_changes: [false; 3],
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
    let mut ws = open(config)?;
    let mut phase = 0;
    let mut deadline = Instant::now() + Duration::from_secs(10);
    let mut last_message = Instant::now();
    let mut next_ping = Instant::now() + Duration::from_secs(20);
    let mut ping_deadline: Option<Instant> = None;
    let mut buffered: Vec<(String, Value)> = Vec::new();
    let mut buffered_bytes = 0;
    let relevant = |id: &str| {
        id == config.grid_import
            || id == config.grid_export
            || (!config.active_power.is_empty() && id == config.active_power)
    };
    while !stop.load(Ordering::Relaxed) {
        if (phase < 4 && Instant::now() >= deadline)
            || ping_deadline.is_some_and(|d| Instant::now() >= d)
            || last_message.elapsed() >= Duration::from_secs(60)
        {
            return Err("ha-response-timeout");
        }
        if phase == 4 && Instant::now() >= next_ping {
            send(&mut ws, json!({"id":3,"type":"ping"}))?;
            ping_deadline = Some(Instant::now() + Duration::from_secs(10));
            next_ping = Instant::now() + Duration::from_secs(20);
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
            (4, Some("pong")) if v["id"] == 3 && ping_deadline.is_some() => {
                ping_deadline = None;
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
    pub fn diagnostics(&self, now: i64, meter_stale_seconds: f64) -> HaDiagnostics {
        let ids = [&self.config.grid_import, &self.config.grid_export, &self.config.active_power];
        HaDiagnostics {
            connected: self.stats.connected,
            initialized: self.ready,
            evidence: "provisional HA value-change evidence; physical acquisition unverified".into(),
            entities: ids
                .iter()
                .enumerate()
                .filter(|(_, id)| !id.is_empty())
                .map(|(i, id)| {
                    let o = self.observations[i].as_ref();
                    let s = self.samples[i].as_ref();
                    EntityDiagnostic {
                        entity: (*id).clone(),
                        ha_last_changed: o.and_then(|o| o.changed),
                        ha_last_updated: o.and_then(|o| o.updated),
                        ha_last_reported: o.and_then(|o| o.reported),
                        received_age_ms: o.map(|o| o.received.elapsed().as_millis().min(u64::MAX as u128) as u64),
                        evidence_at: s.map(|s| s.at),
                        age_ms: s.map(|s| s.age(now)),
                        issue: self.issues[i].clone().or_else(|| {
                            let limit = if i < 2 { meter_stale_seconds } else { self.config.inverter_stale_seconds };
                            if !self.stats.connected {
                                Some("ha-disconnected".into())
                            } else if s.is_none() {
                                Some("ha-entity-unavailable".into())
                            } else if s.is_some_and(|s| now < s.at || s.age(now) as f64 > limit * 1000.0) {
                                Some("ha-entity-stale".into())
                            } else if i < 2 && !self.live_changes[i] {
                                Some("ha-awaiting-new-grid-evidence".into())
                            } else {
                                None
                            }
                        }),
                    }
                })
                .collect(),
        }
    }

    pub fn disconnect(&mut self, issue: &str) {
        self.samples = [None, None, None];
        self.live_changes = [false; 3];
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
        id == self.config.grid_import
            || id == self.config.grid_export
            || (!self.config.active_power.is_empty() && id == self.config.active_power)
    }

    pub fn state(&mut self, id: &str, value: &Value, now: i64) {
        self.apply(id, value, now, true);
    }

    pub fn snapshot_state(&mut self, id: &str, value: &Value, now: i64) {
        self.apply(id, value, now, false);
    }

    fn apply(&mut self, id: &str, value: &Value, now: i64, live: bool) {
        let index = if id == self.config.grid_import {
            0
        } else if id == self.config.grid_export {
            1
        } else if !self.config.active_power.is_empty() && id == self.config.active_power {
            2
        } else {
            return;
        };
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
            let watts = state.parse::<f64>().map_err(|_| "ha-invalid-power")?;
            let unit = value["attributes"]["unit_of_measurement"].as_str().ok_or("ha-unit")?;
            if unit != if index < 2 { "kW" } else { "W" } {
                return Err("ha-unit");
            }
            let watts = watts * if index < 2 { 1000.0 } else { 1.0 };
            if !watts.is_finite() || !(0.0..=1_000_000.0).contains(&watts) {
                return Err("ha-invalid-power");
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
            Ok(Sample { watts, at, reported_at: at, received: Instant::now(), age_at_receive: now - at })
        })();
        match parsed {
            Ok(mut sample) => {
                let changed = self.history[index].as_ref().is_none_or(|old| old.watts != sample.watts);
                if let Some(old) = &self.history[index] {
                    if sample.reported_at < old.reported_at
                        || (sample.reported_at == old.reported_at && sample.watts != old.watts)
                    {
                        self.samples[index] = None;
                        self.live_changes[index] = false;
                        self.issues[index] = Some("ha-out-of-order".into());
                        self.stats.last_issue = Some("ha-out-of-order".into());
                        self.stats.errors += 1;
                        return;
                    }
                    if sample.watts == old.watts {
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
                self.stats.errors += 1;
                self.stats.last_issue = Some(issue.into());
            }
        }
    }

    pub fn grid(&mut self, now: i64, stale_seconds: f64) -> Option<Reading> {
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
            Ok(Reading::new(import.at.max(export.at), import.watts, export.watts, "home-assistant"))
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
        if self.config.active_power.is_empty() {
            return InverterSnapshot::disabled();
        }
        match &self.samples[2] {
            Some(s)
                if self.ready
                    && self.stats.connected
                    && now >= s.at
                    && s.age(now) as f64 <= self.config.inverter_stale_seconds * 1000.0 =>
            {
                InverterSnapshot {
                    status: InverterStatus::Online,
                    active_power_w: Some(s.watts),
                    pv_power_w: None,
                    household_use_w: None,
                    timestamp: Some(s.at),
                    issue: None,
                }
            }
            _ => InverterSnapshot::without_reading(InverterStatus::Offline, Some("ha-ac-unavailable-or-stale".into())),
        }
    }
}

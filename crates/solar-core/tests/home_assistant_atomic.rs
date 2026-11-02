//! Decision-grade atomic telegram fixtures; no HA deployment or hardware access.

use serde_json::{Value, json};
use solar_core::decision::State;
use solar_core::home_assistant::HaState;
use solar_core::present::solar_info;
use solar_core::{Runtime, SolarConfig, SolarService};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};

const GRID: &str = "sensor.dsmr_atomic_grid_sample";
const AC: &str = "sensor.sg5_0rs_local_total_active_power";
const DC: &str = "sensor.sg5_0rs_local_total_dc_power";
const AT: i64 = 1_800_000_000_000;

fn iso(at: i64) -> String {
    chrono::DateTime::from_timestamp_millis(at).unwrap().to_rfc3339()
}

fn config() -> SolarConfig {
    SolarConfig::from_json(&json!({
        "energySource":{"type":"home-assistant"},
        "homeAssistant":{"atomicGrid":GRID,"activePower":AC,"dcPower":DC}
    }))
    .unwrap()
}

fn sample(session: &str, sequence: u64, at: i64, import_w: f64, export_w: f64) -> Value {
    json!({"entity_id":GRID,"state":sequence.to_string(),
        "last_changed":iso(at),"last_updated":iso(at),"last_reported":iso(at),
        "attributes":{"schema_version":1,"session":session,"sample_sequence":sequence,
            "meter_timestamp":iso(at),"received_at":iso(at),"import_w":import_w,"export_w":export_w}})
}

fn online() -> HaState {
    let mut state = HaState::new(config().home_assistant);
    state.connected();
    state.ready();
    state
}

fn runtime() -> Runtime {
    let mut runtime = Runtime::new(config());
    runtime.ha_connected();
    runtime.ha_ready();
    runtime
}

#[test]
fn atomic_mapping_is_explicit_configurable_and_never_falls_back_to_paired_entities() {
    let config = config();
    assert_eq!(config.decision.stale_after_seconds, 30.0);
    assert_eq!(config.decision.stable_duration_seconds, 300.0);
    assert_eq!(config.home_assistant.inverter_stale_seconds, 90.0);
    assert_eq!(SolarConfig::from_json(&config.to_json()).unwrap(), config);
    assert!(SolarConfig::default().home_assistant.atomic_grid.is_empty());
    for bad in [
        "sensor.bad\nmapping",
        "binary_sensor.bad",
        "sensor.",
        AC,
        "sensor.electricity_meter_power_consumption",
        "sensor.electricity_meter_power_production",
    ] {
        assert!(
            SolarConfig::from_json(&json!({"homeAssistant":{"atomicGrid":bad,"activePower":AC,"dcPower":DC}})).is_err()
        );
    }
    assert!(
        SolarConfig::from_json(&json!({"homeAssistant":{"atomicGrid":"sensor.dsmr_atomic_grid_sample_2"}})).is_ok()
    );
    let mut state = online();
    let pair = |id| {
        json!({"entity_id":id,"state":"3","last_changed":iso(AT),
        "attributes":{"unit_of_measurement":"kW"}})
    };
    for id in ["sensor.electricity_meter_power_consumption", "sensor.electricity_meter_power_production"] {
        assert!(!state.relevant(id));
        state.state(id, &pair(id), AT);
    }
    assert!(state.grid(AT, 30.0).is_none());
    assert!(state.relevant(GRID));
    state.state(GRID, &sample("synthetic-session-a", 1, AT, 0.0, 3000.0), AT);
    assert_eq!(state.grid(AT, 30.0).unwrap().grid_export_w, 3000.0, "payload powers are W, not kW");
    state.state(GRID, &json!({"state":"unavailable","attributes":{"schema_version":1}}), AT);
    assert!(state.grid(AT, 30.0).is_none());
}

#[test]
fn constant_zero_samples_remain_fresh_with_advancing_acquisition_sequences() {
    let mut state = online();
    for sequence in 1..=41 {
        let at = AT + (sequence as i64 - 1) * 10_000;
        state.state(GRID, &sample("synthetic-session-a", sequence, at, 0.0, 0.0), at);
        let reading = state.grid(at, 30.0).unwrap();
        assert_eq!(reading.grid_import_w, 0.0);
        assert_eq!(reading.grid_export_w, 0.0);
        assert_eq!(reading.timestamp, at);
        let diagnostics = state.diagnostics(at, 30.0);
        assert_eq!(diagnostics.entities[0].sample_sequence, Some(sequence));
        assert_eq!(diagnostics.entities[0].meter_timestamp, Some(at));
        assert_eq!(diagnostics.entities[0].publisher_received_at, Some(at));
    }
}

#[test]
fn unchanged_surplus_earns_full_300_second_go_without_weakening_freshness() {
    let mut runtime = runtime();
    for sequence in 1..=31 {
        let at = AT + (sequence as i64 - 1) * 10_000;
        runtime.ha_state(GRID, &sample("synthetic-session-a", sequence, at, 0.0, 3000.0), at);
        let s = runtime.snapshot(at);
        assert_eq!(s.evaluation.safe_to_start, sequence == 31);
        assert_eq!(s.evaluation.stable_seconds, (sequence - 1) as f64 * 10.0);
        assert!(!s.daily_energy.available && !s.peak_forecast.available);
        assert!(s.evaluation.current_demand_w.is_none() && s.evaluation.monthly_peak_w.is_none());
    }
    let s = runtime.snapshot(AT + 300_000);
    assert!(s.evaluation.safe_to_start);
    assert!(s.inverter.household_use_w.is_none());
    assert!(solar_info(&s).section("Connections").unwrap().value("Evidence").unwrap().contains("atomic"));
    assert_eq!(solar_info(&s).section("Connections").unwrap().value("Atomic sample sequence"), Some("31"));
    assert_eq!(runtime.snapshot(AT + 330_000).evaluation.state, State::Offline, "30s cutoff is exclusive");
    runtime.ha_state(GRID, &sample("synthetic-session-a", 32, AT + 331_000, 0.0, 3000.0), AT + 331_000);
    assert!(!runtime.snapshot(AT + 331_000).evaluation.safe_to_start, "outage earns stability again");
}

#[test]
fn snapshot_reconnect_and_session_changes_reset_go_and_require_new_live_samples() {
    let mut runtime = runtime();
    runtime.ha_snapshot_state(GRID, &sample("synthetic-session-a", 1, AT, 0.0, 3000.0), AT);
    assert_eq!(runtime.snapshot(AT).evaluation.state, State::Offline);
    for sequence in 2..=32 {
        let at = AT + (sequence as i64 - 1) * 10_000;
        runtime.ha_state(GRID, &sample("synthetic-session-a", sequence, at, 0.0, 3000.0), at);
    }
    assert!(runtime.snapshot(AT + 310_000).evaluation.safe_to_start);
    runtime.ha_state(GRID, &sample("synthetic-session-b", 1, AT + 311_000, 0.0, 3000.0), AT + 311_000);
    let s = runtime.snapshot(AT + 311_000);
    assert!(!s.evaluation.safe_to_start);
    assert_eq!(s.evaluation.stable_seconds, 0.0);
    runtime.ha_disconnect("ha-read-or-protocol");
    assert_eq!(runtime.snapshot(AT + 311_000).evaluation.state, State::Offline);
    runtime.ha_connected();
    runtime.ha_ready();
    runtime.ha_snapshot_state(GRID, &sample("synthetic-session-b", 1, AT + 311_000, 0.0, 3000.0), AT + 312_000);
    assert_eq!(runtime.snapshot(AT + 312_000).evaluation.state, State::Offline);
    runtime.ha_state(GRID, &sample("synthetic-session-b", 2, AT + 313_000, 0.0, 3000.0), AT + 313_000);
    assert_eq!(runtime.snapshot(AT + 313_000).evaluation.stable_seconds, 0.0);
    runtime.ha_state(GRID, &sample("synthetic-session-a", 33, AT + 314_000, 0.0, 3000.0), AT + 314_000);
    assert_eq!(runtime.snapshot(AT + 314_000).evaluation.state, State::Offline, "retired sessions cannot return");
}

#[test]
fn replays_and_nonadvancing_sequence_meter_or_arrival_time_fail_closed() {
    let original = sample("synthetic-session-a", 10, AT, 0.0, 3000.0);
    let mut bad = vec![
        original.clone(),
        sample("synthetic-session-a", 9, AT + 1000, 0.0, 3000.0),
        sample("synthetic-session-a", 11, AT, 0.0, 3000.0),
        sample("synthetic-session-a", 11, AT - 1, 0.0, 3000.0),
    ];
    let mut same_received = sample("synthetic-session-a", 11, AT + 1, 0.0, 3000.0);
    same_received["attributes"]["received_at"] = json!(iso(AT));
    bad.push(same_received);
    bad.push(sample("synthetic-session-b", 1, AT - 1, 0.0, 3000.0));
    for value in bad {
        let mut state = online();
        state.state(GRID, &original, AT);
        assert!(state.grid(AT, 30.0).is_some());
        state.state(GRID, &value, AT + 2000);
        assert!(state.grid(AT + 2000, 30.0).is_none());
        assert_eq!(state.diagnostics(AT + 2000, 30.0).entities[0].issue.as_deref(), Some("ha-atomic-replay"));
    }
}

#[test]
fn malformed_missing_unavailable_and_untrusted_times_cannot_retain_power() {
    let good = sample("synthetic-session-a", 11, AT, 0.0, 3000.0);
    let mut bad = vec![Value::Null, json!({"state":"unavailable"}), json!({"state":"unknown"})];
    for key in
        ["schema_version", "session", "sample_sequence", "meter_timestamp", "received_at", "import_w", "export_w"]
    {
        let mut v = good.clone();
        v["attributes"].as_object_mut().unwrap().remove(key);
        bad.push(v);
    }
    for (key, value) in [
        ("schema_version", json!(2)),
        ("schema_version", json!(true)),
        ("session", json!("")),
        ("session", json!("bad\nsession")),
        ("session", json!("x".repeat(129))),
        ("sample_sequence", json!(0)),
        ("sample_sequence", json!(-1)),
        ("sample_sequence", json!(1.1)),
        ("sample_sequence", json!(9_007_199_254_740_992_u64)),
        ("sample_sequence", json!("11")),
        ("meter_timestamp", json!("2027-01-15T08:00:00")),
        ("meter_timestamp", json!("2027-01-15T08:00:00+02:00")),
        ("received_at", json!("bad")),
        ("received_at", json!("2027-01-15T08:00:00-00:00")),
        ("import_w", json!(-1)),
        ("export_w", json!(1_000_001)),
        ("export_w", json!("NaN")),
        ("import_w", Value::Null),
        ("export_w", json!(false)),
        ("available", json!(false)),
        ("available", json!("true")),
    ] {
        let mut v = good.clone();
        v["attributes"][key] = value;
        bad.push(v);
    }
    for value in [json!("12"), json!("11.0"), json!(11), json!("011")] {
        let mut v = good.clone();
        v["state"] = value;
        bad.push(v);
    }
    bad.push(sample("synthetic-session-a", 11, AT - 30_000, 0.0, 3000.0));
    bad.push(sample("synthetic-session-a", 11, AT + 5001, 0.0, 3000.0));
    let mut future_received = good.clone();
    future_received["attributes"]["received_at"] = json!(iso(AT + 5001));
    bad.push(future_received);
    for value in bad {
        let mut state = online();
        state.state(GRID, &sample("synthetic-session-a", 10, AT - 1000, 0.0, 3000.0), AT);
        assert!(state.grid(AT, 30.0).is_some());
        state.state(GRID, &value, AT);
        assert!(state.grid(AT, 30.0).is_none(), "{value}");
        assert!(state.diagnostics(AT, 30.0).entities[0].issue.is_some());
    }
}

#[test]
fn exact_expiry_future_bounds_and_monotonic_silence_are_enforced() {
    let mut state = online();
    state.state(GRID, &sample("synthetic-session-a", 1, AT, 0.0, 3000.0), AT);
    assert!(state.grid(AT + 29_999, 30.0).is_some());
    assert!(state.grid(AT + 30_000, 30.0).is_none());
    assert!(state.grid(AT + 30_000, 90.0).is_none(), "atomic publisher contract cannot be weakened past30s");
    let mut state = online();
    state.state(GRID, &sample("synthetic-session-a", 1, AT - 29_999, 0.0, 3000.0), AT);
    assert!(state.grid(AT, 30.0).is_some());
    thread::sleep(Duration::from_millis(5));
    assert!(state.grid(AT - 1, 30.0).is_none(), "wall clock rollback cannot reset monotonic age");
    let mut state = online();
    state.state(GRID, &sample("synthetic-session-a", 1, AT + 5000, 0.0, 3000.0), AT);
    assert!(state.grid(AT, 30.0).is_none(), "tolerated future samples cannot yet enable GO");
    assert!(state.grid(AT + 5000, 30.0).is_some());
}

#[test]
fn optional_ac_dc_and_unrelated_clocks_never_renew_or_invalidate_atomic_grid() {
    let mut state = online();
    state.state(GRID, &sample("synthetic-session-a", 1, AT, 0.0, 3000.0), AT);
    state.state(AC, &json!({"state":"unavailable"}), AT);
    state.state(
        DC,
        &json!({"state":"4200","attributes":{"unit_of_measurement":"W"},
        "last_changed":iso(AT)}),
        AT,
    );
    assert!(state.grid(AT, 30.0).is_some());
    assert_eq!(state.inverter(AT).active_power_w, None);
    assert_eq!(state.inverter(AT).pv_power_w, Some(4200.0));
    assert_eq!(state.grid(AT + 20_000, 30.0).unwrap().timestamp, AT);
    state.state("sensor.electricity_meter_timestamp", &json!({"state":iso(AT+20_000)}), AT + 20_000);
    state.state(
        AC,
        &json!({"state":"3500","last_changed":iso(AT+20_000),
        "attributes":{"unit_of_measurement":"W"}}),
        AT + 20_000,
    );
    assert_eq!(state.grid(AT + 20_000, 30.0).unwrap().timestamp, AT);
    assert_eq!(state.inverter(AT + 20_000).active_power_w, Some(3500.0));
    assert!(state.inverter(AT + 20_000).household_use_w.is_none());
    assert!(state.grid(AT + 30_000, 30.0).is_none());
}

#[test]
fn session_retirement_cache_is_bounded() {
    let mut state = online();
    for i in 0..65 {
        state.state(GRID, &sample(&format!("synthetic-session-{i}"), 1, AT + i, 0.0, 3000.0), AT + i);
        assert!(state.grid(AT + i, 30.0).is_some());
    }
    state.state(GRID, &sample("synthetic-session-overflow", 1, AT + 65, 0.0, 3000.0), AT + 65);
    assert!(state.grid(AT + 65, 30.0).is_none());
    assert_eq!(state.diagnostics(AT + 65, 30.0).entities[0].issue.as_deref(), Some("ha-atomic-session-limit"));
}

fn write(ws: &mut WebSocket<TcpStream>, value: Value) {
    ws.send(Message::Text(value.to_string().into())).unwrap();
}
fn request(ws: &mut WebSocket<TcpStream>) -> Value {
    serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap()
}
fn event(ws: &mut WebSocket<TcpStream>, value: Value) {
    write(
        ws,
        json!({"id":1,"type":"event","event":{"event_type":"state_changed","data":{"entity_id":GRID,"new_state":value}}}),
    );
}
fn wait(service: &SolarService, predicate: impl Fn(&solar_core::Snapshot) -> bool) -> solar_core::Snapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let s = service.snapshot();
        if predicate(&s) {
            return s;
        }
        assert!(Instant::now() < deadline, "atomic websocket fixture deadline: {:?}", s.diagnostics);
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn websocket_atomic_snapshot_sequence_unavailability_and_reconnect() {
    if std::env::var("SOLAR_ATOMIC_FIXTURE_CHILD").as_deref() != Ok("1") {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "websocket_atomic_snapshot_sequence_unavailability_and_reconnect", "--nocapture"])
            .env("SOLAR_ATOMIC_FIXTURE_CHILD", "1")
            .env("SOLAR_ATOMIC_FIXTURE_TOKEN", "synthetic-token")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (send, next) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        for connection in 0..2 {
            let (socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut ws = tungstenite::accept(socket).unwrap();
            write(&mut ws, json!({"type":"auth_required"}));
            assert_eq!(request(&mut ws)["access_token"], "synthetic-token");
            write(&mut ws, json!({"type":"auth_ok"}));
            assert_eq!(request(&mut ws)["type"], "subscribe_events");
            write(&mut ws, json!({"id":1,"type":"result","success":true}));
            assert_eq!(request(&mut ws)["type"], "get_states");
            let at = solar_core::now_ms() - 1000;
            let sequence = 1 + connection * 10;
            write(
                &mut ws,
                json!({"id":2,"type":"result","success":true,"result":[sample("synthetic-session-a",sequence,at,0.0,3000.0)]}),
            );
            next.recv_timeout(Duration::from_secs(5)).unwrap();
            event(&mut ws, sample("synthetic-session-a", sequence + 1, at + 100, 0.0, 3000.0));
            next.recv_timeout(Duration::from_secs(5)).unwrap();
            event(&mut ws, sample("synthetic-session-a", sequence + 2, at + 200, 0.0, 3000.0));
            next.recv_timeout(Duration::from_secs(5)).unwrap();
            event(&mut ws, json!({"state":"unavailable","attributes":{"schema_version":1}}));
            next.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    });
    let config=SolarConfig::from_json(&json!({
        "energySource":{"type":"home-assistant"},"decision":{"stableDurationSeconds":0},
        "homeAssistant":{"atomicGrid":GRID,"endpoint":format!("ws://127.0.0.1:{port}/api/websocket"),"tokenEnv":"SOLAR_ATOMIC_FIXTURE_TOKEN"}
    })).unwrap();
    let service = SolarService::start(config);
    for connection in 0..2 {
        let s = wait(&service, |s| {
            s.home_assistant.as_ref().is_some_and(|h| h.initialized) && s.diagnostics.reconnects >= connection
        });
        assert_eq!(s.evaluation.state, State::Offline, "snapshot never enables GO");
        send.send(()).unwrap();
        let sequence = 2 + connection * 10;
        let s = wait(&service, |s| {
            s.evaluation.safe_to_start
                && s.home_assistant.as_ref().unwrap().entities[0].sample_sequence == Some(sequence)
        });
        let at = s.reading_at.unwrap();
        send.send(()).unwrap();
        let s = wait(&service, |s| s.reading_at.is_some_and(|t| t > at));
        assert!(s.evaluation.safe_to_start, "unchanged powers advance on new sequence");
        send.send(()).unwrap();
        wait(&service, |s| s.evaluation.state == State::Offline);
        send.send(()).unwrap();
    }
    service.stop();
    server.join().unwrap();
}

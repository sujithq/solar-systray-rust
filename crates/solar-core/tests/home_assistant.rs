//! Synthetic HA-only fixtures; no device access or real credentials.

use serde_json::{Value, json};
use solar_core::decision::{DecisionEngine, State};
use solar_core::home_assistant::{HOUSEHOLD_TOPOLOGY_SINGLE_GRID_INVERTER, HaState, MAX_MESSAGE};
use solar_core::present::solar_info;
use solar_core::sun::Sun;
use solar_core::{Runtime, SolarConfig, SolarService};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};

const IMPORT: &str = "sensor.electricity_meter_power_consumption";
const EXPORT: &str = "sensor.electricity_meter_power_production";
const AC: &str = "sensor.sg5_0rs_local_total_active_power";
const DC: &str = "sensor.sg5_0rs_local_total_dc_power";
const AT: i64 = 1_800_000_000_000;

fn config() -> SolarConfig {
    SolarConfig::from_json(&json!({
        "energySource":{"type":"home-assistant"},
        "homeAssistant":{"activePower":AC,"dcPower":DC},
        "decision":{"stableDurationSeconds":2}
    }))
    .unwrap()
}

fn household_config(sign: i64, max_event_skew_seconds: f64) -> SolarConfig {
    SolarConfig::from_json(&json!({
        "energySource":{"type":"home-assistant"},
        "homeAssistant":{
            "atomicGrid":"sensor.dsmr_atomic_grid_sample",
            "activePower":AC,
            "householdUse":{
                "enabled":true,
                "topology":HOUSEHOLD_TOPOLOGY_SINGLE_GRID_INVERTER,
                "generationSign":sign,
                "maxEventSkewSeconds":max_event_skew_seconds
            }
        }
    }))
    .unwrap()
}

fn entity(id: &str, state: &str, unit: &str, at: i64) -> Value {
    let at = chrono::DateTime::from_timestamp_millis(at).unwrap().to_rfc3339();
    json!({"entity_id":id,"state":state,"attributes":{"unit_of_measurement":unit},
        "last_changed":at,"last_updated":at,"last_reported":at})
}

fn atomic_sample(sequence: u64, meter_at: i64, received_at: i64, import_w: u64, export_w: u64) -> Value {
    let timestamp = |at| chrono::DateTime::from_timestamp_millis(at).unwrap().to_rfc3339();
    json!({
        "entity_id":"sensor.dsmr_atomic_grid_sample",
        "state":sequence.to_string(),
        "attributes":{
            "schema_version":1,
            "session":"household-contract",
            "sample_sequence":sequence,
            "meter_timestamp":timestamp(meter_at),
            "received_at":timestamp(received_at),
            "import_w":import_w,
            "export_w":export_w
        }
    })
}

fn online() -> HaState {
    let mut state = HaState::new(config().home_assistant);
    state.connected();
    state.ready();
    state
}

fn pair(state: &mut HaState, at: i64, import: &str, export: &str) {
    state.state(IMPORT, &entity(IMPORT, import, "kW", at), at);
    state.state(EXPORT, &entity(EXPORT, export, "kW", at), at);
}

#[test]
fn validates_endpoint_references_entities_and_round_trip() {
    let config = config();
    assert!(config.is_ha());
    assert!(!config.inverter_enabled());
    assert_eq!(config.decision.stale_after_seconds, 30.0);
    assert_eq!(config.home_assistant.inverter_stale_seconds, 90.0);
    assert!(!config.home_assistant.household_use_enabled);
    assert_eq!(config.home_assistant.ac_generation_sign, 0);
    assert_eq!(SolarConfig::from_json(&config.to_json()).unwrap(), config);
    for (key, value) in [
        ("endpoint", json!("https://127.0.0.1:8123/api/websocket")),
        ("endpoint", json!("ws://user:secret@127.0.0.1:8123/api/websocket")),
        ("endpoint", json!("ws://127.0.0.1:8123/api/websocket?token=secret")),
        ("endpoint", json!("ws://127.0.0.1:8123/api/states")),
        ("endpoint", json!("ws://example.invalid:8123/api/websocket")),
        ("tokenEnv", json!("bad\nreference")),
        ("gridImport", json!("sensor.bad\nvalue")),
        ("gridImport", json!(EXPORT)),
        ("activePower", json!(IMPORT)),
        ("dcPower", json!(EXPORT)),
        ("dcPower", json!("sensor.bad\nvalue")),
        ("gridSkewSeconds", json!(31)),
        ("inverterStaleSeconds", json!(0)),
    ] {
        let v = json!({"energySource":{"type":"home-assistant"}, "homeAssistant":{key:value}});
        let error = SolarConfig::from_json(&v).unwrap_err().to_string();
        assert!(!error.contains("secret"));
    }

    assert!(
        SolarConfig::from_json(&json!({"energySource":{"type":"home-assistant"},
            "homeAssistant":{"activePower":AC,"dcPower":AC}}))
        .is_err()
    );
    assert!(SolarConfig::from_json(&json!({"homeAssistant":{"token":"secret"}})).is_err());
    assert!(SolarConfig::default().to_json().get("homeAssistant").is_none(), "legacy JSON remains compatible");
}

#[test]
fn household_use_configuration_requires_explicit_supported_topology_sign_and_atomic_ac() {
    let base = json!({
        "energySource":{"type":"home-assistant"},
        "homeAssistant":{
            "atomicGrid":"sensor.dsmr_atomic_grid_sample",
            "activePower":AC,
            "householdUse":{
                "enabled":true,
                "topology":HOUSEHOLD_TOPOLOGY_SINGLE_GRID_INVERTER,
                "generationSign":1,
                "maxEventSkewSeconds":5
            }
        }
    });
    let parsed = SolarConfig::from_json(&base).unwrap();
    assert!(parsed.home_assistant.household_use_enabled);
    assert_eq!(parsed.home_assistant.ac_generation_sign, 1);
    assert_eq!(SolarConfig::from_json(&parsed.to_json()).unwrap(), parsed);

    for (key, value) in [
        ("topology", json!("")),
        ("topology", json!("unknown_topology")),
        ("generationSign", json!(0)),
        ("generationSign", json!(2)),
        ("maxEventSkewSeconds", json!(31)),
    ] {
        let mut invalid = base.clone();
        invalid["homeAssistant"]["householdUse"][key] = value;
        assert!(SolarConfig::from_json(&invalid).is_err(), "{key}");
    }

    for missing in ["atomicGrid", "activePower"] {
        let mut invalid = base.clone();
        invalid["homeAssistant"][missing] = json!("");
        assert!(SolarConfig::from_json(&invalid).is_err(), "{missing}");
    }
    let partial = json!({
        "energySource":{"type":"home-assistant"},
        "homeAssistant":{"householdUse":{"enabled":true}}
    });
    assert!(SolarConfig::from_json(&partial).is_err(), "partial opt-in retains safe defaults and fails closed");
    let unknown = json!({
        "energySource":{"type":"home-assistant"},
        "homeAssistant":{"householdUse":{"notASetting":true}}
    });
    assert!(SolarConfig::from_json(&unknown).is_err());
}

#[test]
fn normalizes_independent_grid_ac_and_dc_without_household_substitution() {
    let mut state = online();
    pair(&mut state, AT, "0.1", "2.8");
    let reading = state.grid(AT, 30.0).unwrap();
    assert_eq!(reading.grid_import_w, 100.0);
    assert_eq!(reading.grid_export_w, 2800.0);
    assert_eq!(reading.grid_import_w - reading.grid_export_w, -2700.0);
    assert!(reading.energy_totals_wh.is_none());
    assert!(reading.meter_time.is_none());
    state.state(AC, &entity(AC, "3200", "W", AT), AT);
    state.state(DC, &entity(DC, "3600", "W", AT), AT);
    assert_eq!(state.inverter(AT).active_power_w, Some(3200.0));
    assert_eq!(state.inverter(AT).pv_power_w, Some(3600.0));
    assert_eq!(state.inverter(AT).household_use_w, None);
    assert_eq!(state.inverter(AT + 90_000).active_power_w, Some(3200.0));
    assert_eq!(state.inverter(AT + 90_001).active_power_w, None);
    state.state(AC, &entity(AC, "unavailable", "W", AT), AT);
    assert!(state.grid(AT, 30.0).is_some(), "optional AC cannot invalidate grid");
    assert!(state.inverter(AT).active_power_w.is_none());
    assert_eq!(state.inverter(AT).pv_power_w, Some(3600.0), "invalid AC cannot invalidate DC");
    let diagnostics = state.diagnostics(AT, 30.0);
    assert_eq!(diagnostics.entities[2].entity, AC);
    assert!(diagnostics.entities[2].issue.is_some());
    assert_eq!(diagnostics.entities[3].entity, DC);
    assert!(diagnostics.entities[3].issue.is_none());
}

#[test]
fn gated_household_use_uses_signed_net_grid_plus_sign_normalized_ac_only() {
    let config = household_config(-1, 5.0);
    let mut state = HaState::new(config.home_assistant);
    state.connected();
    state.ready();

    let meter_at = AT - 10_000;
    let callback_at = AT;
    let now = AT + 1_000;
    state.state("sensor.dsmr_atomic_grid_sample", &atomic_sample(1, meter_at, callback_at, 1_000, 200), callback_at);
    state.state(AC, &entity(AC, "-500", "W", callback_at), callback_at);
    let reading = state.grid(now, 30.0).unwrap();

    assert_eq!(reading.timestamp, meter_at);
    assert_eq!(
        state.household_use(Some(&reading), now, 30.0),
        Ok(1_300.0),
        "the event-proximity gate uses received_at, not the independent meter clock"
    );

    let config = household_config(1, 5.0);
    let mut positive_ac = HaState::new(config.home_assistant);
    positive_ac.connected();
    positive_ac.ready();
    positive_ac.state(
        "sensor.dsmr_atomic_grid_sample",
        &atomic_sample(1, meter_at, callback_at, 1_000, 200),
        callback_at,
    );
    positive_ac.state(AC, &entity(AC, "500", "W", callback_at), callback_at);
    let reading = positive_ac.grid(now, 30.0).unwrap();
    assert_eq!(positive_ac.household_use(Some(&reading), now, 30.0), Ok(1_300.0));
}

#[test]
fn household_use_fails_closed_on_missing_ac_skew_staleness_and_reconnect_snapshot() {
    let config = household_config(1, 5.0);
    let mut state = HaState::new(config.home_assistant);
    state.connected();
    state.ready();
    let first = atomic_sample(1, AT, AT, 1_000, 200);
    state.state("sensor.dsmr_atomic_grid_sample", &first, AT);
    let reading = state.grid(AT, 30.0).unwrap();
    assert_eq!(state.household_use(Some(&reading), AT, 30.0), Err("ha-household-ac-unavailable"));
    state.state(DC, &entity(DC, "800", "W", AT), AT);
    assert_eq!(state.household_use(Some(&reading), AT, 30.0), Err("ha-household-ac-unavailable"));

    state.state(AC, &entity(AC, "500", "W", AT), AT + 6_000);
    let skewed_grid = atomic_sample(2, AT + 6_000, AT + 6_000, 1_000, 200);
    state.state("sensor.dsmr_atomic_grid_sample", &skewed_grid, AT + 6_000);
    let skewed_reading = state.grid(AT + 6_000, 30.0).unwrap();
    assert_eq!(state.household_use(Some(&skewed_reading), AT + 6_000, 30.0), Err("ha-household-event-skew"));
    assert!(state.grid(AT + 6_000, 30.0).is_some(), "AC skew cannot suppress grid monitoring");

    state.state(AC, &entity(AC, "600", "W", AT + 7_000), AT + 7_000);
    let reading = state.grid(AT + 7_000, 30.0).unwrap();
    assert_eq!(state.household_use(Some(&reading), AT + 7_000, 30.0), Ok(1_400.0));

    state.disconnect("ha-read-or-protocol");
    state.connected();
    state.ready();
    state.snapshot_state(AC, &entity(AC, "600", "W", AT + 7_000), AT + 8_000);
    let reconnected_grid = atomic_sample(3, AT + 8_000, AT + 8_000, 1_000, 200);
    state.state("sensor.dsmr_atomic_grid_sample", &reconnected_grid, AT + 8_000);
    let reading = state.grid(AT + 8_000, 30.0).unwrap();
    assert_eq!(
        state.household_use(Some(&reading), AT + 8_000, 30.0),
        Err("ha-household-awaiting-live-ac"),
        "snapshot AC may not restore derived household use after reconnect"
    );

    state.state(AC, &entity(AC, "700", "W", AT + 8_000), AT + 8_000);
    let reading = state.grid(AT + 8_000, 30.0).unwrap();
    assert_eq!(state.household_use(Some(&reading), AT + 8_000, 30.0), Ok(1_500.0));
    let fresh_grid = atomic_sample(4, AT + 98_001, AT + 98_001, 1_000, 200);
    state.state("sensor.dsmr_atomic_grid_sample", &fresh_grid, AT + 98_001);
    let reading = state.grid(AT + 98_001, 30.0).unwrap();
    assert_eq!(
        state.household_use(Some(&reading), AT + 98_001, 30.0),
        Err("ha-household-ac-stale"),
        "unchanged cached AC ages out independently while newer atomic samples keep grid fresh"
    );
    assert!(state.grid(AT + 98_001, 30.0).is_some(), "stale optional AC cannot suppress grid decisions");
}

#[test]
fn household_use_rejects_negative_or_out_of_range_derived_values() {
    let config = household_config(1, 5.0);
    let mut state = HaState::new(config.home_assistant);
    state.connected();
    state.ready();
    state.state("sensor.dsmr_atomic_grid_sample", &atomic_sample(1, AT, AT, 0, 1_000), AT);
    state.state(AC, &entity(AC, "500", "W", AT), AT);
    let reading = state.grid(AT, 30.0).unwrap();
    assert_eq!(state.household_use(Some(&reading), AT, 30.0), Err("ha-household-out-of-range"));
}

#[test]
fn ac_and_dc_updates_expire_independently_and_never_renew_atomic_grid() {
    let config = SolarConfig::from_json(&json!({
        "energySource":{"type":"home-assistant"},
        "homeAssistant":{"atomicGrid":"sensor.dsmr_atomic_grid_sample","activePower":AC,"dcPower":DC}
    }))
    .unwrap();
    assert_eq!(config.home_assistant.inverter_stale_seconds, 90.0);
    let mut state = HaState::new(config.home_assistant.clone());
    state.connected();
    state.ready();
    let grid = json!({"entity_id":"sensor.dsmr_atomic_grid_sample","state":"1",
        "attributes":{"schema_version":1,"session":"atomic","sample_sequence":1,
        "meter_timestamp":chrono::DateTime::from_timestamp_millis(AT).unwrap().to_rfc3339(),
        "received_at":chrono::DateTime::from_timestamp_millis(AT).unwrap().to_rfc3339(),
        "import_w":0,"export_w":0}});
    state.state("sensor.dsmr_atomic_grid_sample", &grid, AT);
    state.state(AC, &entity(AC, "3200", "W", AT), AT);
    state.state(DC, &entity(DC, "3600", "W", AT + 20_000), AT + 20_000);
    assert!(state.relevant(AC) && state.relevant(DC));
    assert_eq!(
        state.grid(AT + 20_000, 30.0).map(|r| r.timestamp),
        Some(AT),
        "{:?}",
        state.diagnostics(AT + 20_000, 30.0)
    );
    assert_eq!(state.inverter(AT + 90_000).active_power_w, Some(3200.0));
    assert_eq!(state.inverter(AT + 90_000).pv_power_w, Some(3600.0));
    assert_eq!(state.inverter(AT + 90_001).active_power_w, None);
    assert_eq!(state.inverter(AT + 90_001).pv_power_w, Some(3600.0), "DC age is independent");
    assert!(state.grid(AT + 90_001, 30.0).is_none(), "inverter samples cannot renew atomic grid freshness");
    assert_eq!(state.inverter(AT + 110_001).pv_power_w, None);
    assert!(state.grid(AT + 110_001, 30.0).is_none());
}

#[test]
fn malformed_or_unavailable_optional_inverter_only_disables_its_display() {
    let mut state = online();
    pair(&mut state, AT, "0", "1.5");
    state.state(AC, &entity(AC, "2500", "W", AT), AT);
    let valid_grid = state.grid(AT, 30.0).unwrap().timestamp;
    for (watts, unit) in [("NaN", "W"), ("inf", "W"), ("-1", "W"), ("1000001", "W"), ("2500", "kW"), ("unknown", "W")] {
        state.state(DC, &entity(DC, watts, unit, AT), AT);
        assert_eq!(state.inverter(AT).active_power_w, Some(2500.0));
        assert_eq!(state.inverter(AT).pv_power_w, None, "{watts} {unit}");
        assert_eq!(state.grid(AT, 30.0).unwrap().timestamp, valid_grid);
    }
    state.state(DC, &entity(DC, "3600", "W", AT), AT);
    assert_eq!(state.inverter(AT).pv_power_w, Some(3600.0));
    state.state(DC, &Value::Null, AT);
    assert_eq!(state.inverter(AT).active_power_w, Some(2500.0));
    assert_eq!(state.inverter(AT).pv_power_w, None);
    assert_eq!(state.grid(AT, 30.0).unwrap().timestamp, valid_grid);
    state.state(DC, &entity(DC, "3600", "W", AT), AT);
    state.state(AC, &entity(AC, "-2500", "W", AT + 1), AT + 1);
    assert_eq!(state.inverter(AT + 1).active_power_w, Some(-2500.0), "signed AC is preserved without abs()");
    assert_eq!(state.inverter(AT + 1).pv_power_w, Some(3600.0));
    assert_eq!(state.grid(AT + 1, 30.0).unwrap().timestamp, valid_grid);
    for (i, (watts, unit)) in [("-1000001", "W"), ("NaN", "W"), ("2500", "kW")].into_iter().enumerate() {
        let at = AT + 2 + i as i64;
        state.state(AC, &entity(AC, watts, unit, at), at);
        assert_eq!(state.inverter(at).active_power_w, None, "{watts} {unit}");
        assert_eq!(state.inverter(at).pv_power_w, Some(3600.0));
        assert_eq!(state.grid(at, 30.0).unwrap().timestamp, valid_grid);
    }
    assert_eq!(state.stats.errors, 0, "optional inverter failures do not count as grid/client failures");
}

#[test]
fn cached_inverter_snapshots_after_reconnect_do_not_renew_last_state_change_age() {
    let mut state = online();
    pair(&mut state, AT, "0", "1.5");
    state.state(AC, &entity(AC, "2500", "W", AT), AT);
    state.state(DC, &entity(DC, "3600", "W", AT), AT);
    assert_eq!(state.inverter(AT + 89_000).active_power_w, Some(2500.0));
    state.disconnect("ha-read-or-protocol");
    state.connected();
    state.ready();
    state.snapshot_state(AC, &entity(AC, "2500", "W", AT), AT + 89_000);
    state.snapshot_state(DC, &entity(DC, "3600", "W", AT), AT + 89_000);
    assert_eq!(state.inverter(AT + 89_000).pv_power_w, Some(3600.0));
    assert_eq!(state.inverter(AT + 90_001).active_power_w, None);
    assert_eq!(state.inverter(AT + 90_001).pv_power_w, None);
}

#[test]
fn repeated_inverter_state_and_last_reported_do_not_renew_freshness() {
    let mut state = online();
    state.state(AC, &entity(AC, "2500", "W", AT), AT);
    state.state(DC, &entity(DC, "3600", "W", AT), AT);
    let mut repeated_ac = entity(AC, "2500", "W", AT);
    repeated_ac["last_updated"] = json!(chrono::DateTime::from_timestamp_millis(AT + 60_000).unwrap().to_rfc3339());
    repeated_ac["last_reported"] = repeated_ac["last_updated"].clone();
    state.state(AC, &repeated_ac, AT + 60_000);
    state.state(DC, &entity(DC, "3600", "W", AT + 60_000), AT + 60_000);
    assert_eq!(state.inverter(AT + 89_000).active_power_w, Some(2500.0));
    assert_eq!(state.inverter(AT + 90_001).active_power_w, None);
    assert_eq!(state.inverter(AT + 89_000).pv_power_w, Some(3600.0));
}

#[test]
fn cached_snapshot_unchanged_reports_and_unrelated_events_cannot_enable_or_renew_go() {
    let mut state = online();
    state.snapshot_state(IMPORT, &entity(IMPORT, "0", "kW", AT), AT);
    state.snapshot_state(EXPORT, &entity(EXPORT, "2.8", "kW", AT), AT);
    assert!(state.grid(AT, 30.0).is_none(), "snapshot alone must not enable GO");
    pair(&mut state, AT + 1000, "0.001", "2.9");
    pair(&mut state, AT + 2000, "0", "3.0");
    let at = state.grid(AT + 2000, 30.0).unwrap().timestamp;
    pair(&mut state, AT + 30_000, "0", "3.0");
    state.state(AC, &entity(AC, "3500", "W", AT + 30_000), AT + 30_000);
    state.state("sensor.unrelated", &entity("sensor.unrelated", "5", "kW", AT + 30_000), AT + 30_000);
    assert_eq!(state.grid(AT + 30_000, 30.0).unwrap().timestamp, at);
    assert!(state.grid(AT + 32_001, 30.0).is_none());
    let d = state.diagnostics(AT + 32_001, 30.0);
    assert_eq!(d.entities[0].ha_last_changed, Some(AT + 30_000));
    assert_eq!(d.entities[0].evidence_at, Some(AT + 2000));
    assert_eq!(d.entities[0].issue.as_deref(), Some("ha-entity-stale"));
}

#[test]
fn required_invalid_removed_missing_future_and_out_of_order_states_fail_closed() {
    for value in [
        entity(EXPORT, "unavailable", "kW", AT),
        entity(EXPORT, "unknown", "kW", AT),
        entity(EXPORT, "NaN", "kW", AT),
        entity(EXPORT, "inf", "kW", AT),
        entity(EXPORT, "-1", "kW", AT),
        entity(EXPORT, "1001", "kW", AT),
        entity(EXPORT, "3000", "W", AT),
        entity(EXPORT, "bad", "kW", AT),
        entity(EXPORT, "3", "kW", AT + 1),
        Value::Null,
        json!({"state":"3","attributes":{"unit_of_measurement":"kW"},"last_changed":"bad"}),
    ] {
        let mut state = online();
        pair(&mut state, AT - 1000, "0", "2.8");
        assert!(state.grid(AT, 30.0).is_some());
        state.state(EXPORT, &value, AT);
        assert!(state.grid(AT, 30.0).is_none(), "{value}");
    }
    let mut state = online();
    state.state(IMPORT, &entity(IMPORT, "0", "kW", AT), AT);
    assert!(state.grid(AT, 30.0).is_none());
    pair(&mut state, AT, "0", "2.8");
    state.state(EXPORT, &entity(EXPORT, "2.9", "kW", AT - 1), AT);
    assert!(state.grid(AT, 30.0).is_none());
    assert_eq!(state.diagnostics(AT, 30.0).entities[1].issue.as_deref(), Some("ha-out-of-order"));
}

#[test]
fn independent_expiry_and_pair_skew_are_bounded_at_exact_thresholds() {
    let mut state = online();
    state.state(IMPORT, &entity(IMPORT, "0", "kW", AT), AT);
    state.state(EXPORT, &entity(EXPORT, "2.8", "kW", AT + 5000), AT + 5000);
    assert!(state.grid(AT + 30_000, 30.0).is_some());
    assert!(state.grid(AT + 30_001, 30.0).is_none());
    state.state(EXPORT, &entity(EXPORT, "2.9", "kW", AT + 5001), AT + 5001);
    assert!(state.grid(AT + 5001, 30.0).is_none());
    assert_eq!(state.stats.last_issue.as_deref(), Some("ha-grid-skew"));
}

#[test]
fn outages_reset_stability_and_reconnect_requires_new_changes() {
    let mut state = online();
    let mut engine = DecisionEngine::new(config());
    pair(&mut state, AT, "0", "2.8");
    let reading = state.grid(AT, 30.0).unwrap();
    assert!(!engine.evaluate(Some(&reading), AT, Sun::Up).safe_to_start);
    // A real import change to zero after an import interval earns stability again.
    pair(&mut state, AT + 1000, "0.001", "2.9");
    let reading = state.grid(AT + 1000, 30.0).unwrap();
    assert_eq!(engine.evaluate(Some(&reading), AT + 1000, Sun::Up).state, State::Importing);
    pair(&mut state, AT + 2000, "0", "3.0");
    let reading = state.grid(AT + 2000, 30.0).unwrap();
    assert!(!engine.evaluate(Some(&reading), AT + 2000, Sun::Up).safe_to_start);
    state.state(EXPORT, &entity(EXPORT, "3.1", "kW", AT + 4000), AT + 4000);
    let reading = state.grid(AT + 4000, 30.0).unwrap();
    assert!(
        engine.evaluate(Some(&reading), AT + 4000, Sun::Up).safe_to_start,
        "changed export can advance a still-coherent pair"
    );
    state.disconnect("ha-auth-rejected");
    assert_eq!(engine.evaluate(state.grid(AT + 4000, 30.0).as_ref(), AT + 4000, Sun::Up).state, State::Offline);
    assert_eq!(state.stats.last_issue.as_deref(), Some("ha-auth-rejected"));
    state.connected();
    state.ready();
    state.snapshot_state(IMPORT, &entity(IMPORT, "0", "kW", AT + 2000), AT + 4000);
    state.snapshot_state(EXPORT, &entity(EXPORT, "3.1", "kW", AT + 4000), AT + 4000);
    assert!(state.grid(AT + 4000, 30.0).is_none());
    pair(&mut state, AT + 5000, "0.001", "3.2");
    pair(&mut state, AT + 6000, "0", "3.3");
    let reading = state.grid(AT + 6000, 30.0).unwrap();
    assert!(!engine.evaluate(Some(&reading), AT + 6000, Sun::Up).safe_to_start);
    state.state(EXPORT, &entity(EXPORT, "3.4", "kW", AT + 8000), AT + 8000);
    let reading = state.grid(AT + 8000, 30.0).unwrap();
    assert!(
        engine.evaluate(Some(&reading), AT + 8000, Sun::Up).safe_to_start,
        "recovery earns a new full stability interval"
    );
}

#[test]
fn runtime_gates_partial_features_and_observes_transient_imports_between_ui_ticks() {
    let mut runtime = Runtime::new(config());
    runtime.ha_connected();
    runtime.ha_ready();
    runtime.ha_state(IMPORT, &entity(IMPORT, "0", "kW", AT), AT);
    runtime.ha_state(EXPORT, &entity(EXPORT, "2.8", "kW", AT), AT);
    let snapshot = runtime.snapshot(AT);
    assert!(!snapshot.daily_energy.available);
    assert!(!snapshot.peak_forecast.available);
    assert!(snapshot.evaluation.current_demand_w.is_none());
    assert!(snapshot.evaluation.monthly_peak_w.is_none());
    let info = solar_info(&snapshot);
    assert!(info.section("Quarter-hour peak").unwrap().value("Forecast").unwrap().contains("not mapped"));
    assert!(info.section("Today").unwrap().value("Daily energy").unwrap().contains("not mapped"));
    runtime.ha_state(IMPORT, &entity(IMPORT, "0.001", "kW", AT + 2000), AT + 2000);
    runtime.ha_state(IMPORT, &entity(IMPORT, "0", "kW", AT + 3000), AT + 3000);
    runtime.ha_state(EXPORT, &entity(EXPORT, "2.9", "kW", AT + 3000), AT + 3000);
    let snapshot = runtime.snapshot(AT + 3000);
    assert!(!snapshot.evaluation.safe_to_start);
    assert_eq!(snapshot.evaluation.stable_seconds, 0.0);
    runtime.ha_state(AC, &entity(AC, "3200", "W", AT + 3000), AT + 3000);
    let snapshot = runtime.snapshot(AT + 3000);
    assert!(snapshot.inverter.household_use_w.is_none());
    assert_eq!(solar_info(&snapshot).section("Power now").unwrap().value("Inverter output (AC)"), Some("3.2 kW"));
    assert!(solar_info(&snapshot).section("Power now").unwrap().value("Solar panels (PV)").is_none());
}

#[test]
fn runtime_displays_household_use_only_after_explicit_gates_pass() {
    let mut runtime = Runtime::new(household_config(-1, 5.0));
    runtime.ha_connected();
    runtime.ha_ready();
    runtime.ha_state("sensor.dsmr_atomic_grid_sample", &atomic_sample(1, AT - 10_000, AT, 1_000, 200), AT);
    runtime.ha_state(AC, &entity(AC, "-500", "W", AT), AT);
    let snapshot = runtime.snapshot(AT + 1_000);
    assert_eq!(snapshot.inverter.household_use_w, Some(1_300.0));
    let info = solar_info(&snapshot);
    assert_eq!(info.section("Power now").unwrap().value("Household use"), Some("1.3 kW"));
    assert!(
        info.section("Connections")
            .unwrap()
            .value("Household calculation")
            .unwrap()
            .to_ascii_lowercase()
            .contains("enabled")
    );
}

#[test]
fn stale_optional_ac_does_not_prevent_atomic_grid_from_earning_go() {
    let mut config = household_config(-1, 5.0);
    config.decision.stable_duration_seconds = 300.0;
    let mut runtime = Runtime::new(config);
    runtime.ha_connected();
    runtime.ha_ready();
    for second in 0..=301 {
        let now = AT + second * 1000;
        runtime.ha_state("sensor.dsmr_atomic_grid_sample", &atomic_sample(second as u64 + 1, now, now, 0, 3_000), now);
        if second == 0 {
            runtime.ha_state(AC, &entity(AC, "-3500", "W", now), now);
        }
        let snapshot = runtime.snapshot(now);
        if second < 300 {
            assert!(!snapshot.evaluation.safe_to_start, "premature GO at {second}s");
        }
        if second == 301 {
            assert!(snapshot.evaluation.safe_to_start, "grid-only GO must remain available after AC expiry");
            assert!(snapshot.inverter.household_use_w.is_none(), "AC state-change evidence expired after 90s");
            assert_eq!(
                snapshot.home_assistant.as_ref().unwrap().household_use_issue.as_deref(),
                Some("ha-household-ac-stale")
            );
        }
    }
}

fn write(ws: &mut WebSocket<TcpStream>, v: Value) {
    ws.send(Message::Text(v.to_string().into())).unwrap();
}

fn request(ws: &mut WebSocket<TcpStream>) -> Value {
    serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap()
}

fn event(ws: &mut WebSocket<TcpStream>, id: &str, value: Value) {
    write(
        ws,
        json!({"id":1,"type":"event","event":{"event_type":"state_changed",
        "data":{"entity_id":id,"new_state":value}}}),
    );
}

fn wait(service: &SolarService, predicate: impl Fn(&solar_core::Snapshot) -> bool) -> solar_core::Snapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let s = service.snapshot();
        if predicate(&s) {
            return s;
        }
        assert!(Instant::now() < deadline, "HA fixture deadline: {:?}", s.diagnostics);
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn production_heartbeat_ids_advance_past_second_ping_without_reconnect() {
    if std::env::var("SOLAR_HA_HEARTBEAT_CHILD").as_deref() != Ok("1") {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "production_heartbeat_ids_advance_past_second_ping_without_reconnect", "--nocapture"])
            .env("SOLAR_HA_HEARTBEAT_CHILD", "1")
            .env("SOLAR_HA_FIXTURE_TOKEN", "synthetic-test-token")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (done, completed) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(25))).unwrap();
        let mut ws = tungstenite::accept(socket).unwrap();
        write(&mut ws, json!({"type":"auth_required"}));
        assert_eq!(request(&mut ws)["type"], "auth");
        write(&mut ws, json!({"type":"auth_ok"}));
        assert_eq!(request(&mut ws)["id"], 1);
        write(&mut ws, json!({"id":1,"type":"result","success":true}));
        assert_eq!(request(&mut ws)["id"], 2);
        write(&mut ws, json!({"id":2,"type":"result","success":true,"result":[]}));
        for id in [3, 4] {
            assert_eq!(request(&mut ws), json!({"id":id,"type":"ping"}), "HA rejects reused heartbeat IDs");
            let at = chrono::DateTime::from_timestamp_millis(solar_core::now_ms() - 1000).unwrap().to_rfc3339();
            event(
                &mut ws,
                "sensor.dsmr_atomic_grid_sample",
                json!({"entity_id":"sensor.dsmr_atomic_grid_sample","state":(id-2).to_string(),
                    "attributes":{"schema_version":1,"session":"synthetic-production-heartbeat",
                        "sample_sequence":id-2,"meter_timestamp":at,"received_at":at,
                        "import_w":0,"export_w":0}}),
            );
            write(&mut ws, json!({"id":id,"type":"pong"}));
        }
        done.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    let cfg = SolarConfig::from_json(&json!({
        "energySource":{"type":"home-assistant"},
        "homeAssistant":{"endpoint":format!("ws://{address}/api/websocket"),
            "tokenEnv":"SOLAR_HA_FIXTURE_TOKEN","atomicGrid":"sensor.dsmr_atomic_grid_sample"}
    }))
    .unwrap();
    let service = SolarService::start(cfg);
    completed.recv_timeout(Duration::from_secs(50)).unwrap();
    let s = wait(&service, |s| s.home_assistant.as_ref().is_some_and(|h| h.entities[0].sample_sequence == Some(2)));
    let stats = &s.diagnostics;
    assert_eq!(stats.errors, 0);
    assert_eq!(stats.reconnects, 0);
    assert!(s.reading_at.is_some());
    assert!(!s.evaluation.safe_to_start, "two samples cannot bypass 300-second stability");
    service.stop();
    release.send(()).unwrap();
    server.join().unwrap();
}

#[test]
fn websocket_worker_round_trip_and_failures() {
    // Environment mutations are isolated to a child process, never concurrent test threads.
    if std::env::var("SOLAR_HA_FIXTURE_CHILD").as_deref() != Ok("1") {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "websocket_worker_round_trip_and_failures", "--nocapture"])
            .env("SOLAR_HA_FIXTURE_CHILD", "1")
            .env("SOLAR_HA_FIXTURE_TOKEN", "synthetic-test-token")
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
    for scenario in [
        "live",
        "reconnect",
        "auth",
        "wrong-id",
        "oversized",
        "malformed",
        "subscription-rejected",
        "snapshot-shape",
        "trickle-stop",
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let devices = TcpListener::bind("127.0.0.1:0").unwrap();
        devices.set_nonblocking(true).unwrap();
        let device_port = devices.local_addr().unwrap().port();
        let (release, released) = std::sync::mpsc::channel();
        let (signal, signalled) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            socket.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut ws = tungstenite::accept(socket).unwrap();
            write(&mut ws, json!({"type":"auth_required","ha_version":"synthetic"}));
            let auth = request(&mut ws);
            assert_eq!(auth["type"], "auth");
            assert_eq!(auth["access_token"], "synthetic-test-token");
            if scenario == "auth" {
                write(&mut ws, json!({"type":"auth_invalid","message":"DO NOT DISPLAY synthetic-test-token"}));
                return;
            }
            write(&mut ws, json!({"type":"auth_ok"}));
            assert_eq!(request(&mut ws), json!({"id":1,"type":"subscribe_events","event_type":"state_changed"}));
            if scenario == "subscription-rejected" {
                write(
                    &mut ws,
                    json!({"id":1,"type":"result","success":false,"error":{"message":"synthetic-test-token"}}),
                );
                return;
            }
            write(&mut ws, json!({"id":1,"type":"result","success":true,"result":null}));
            assert_eq!(request(&mut ws), json!({"id":2,"type":"get_states"}));
            if scenario == "trickle-stop" {
                // A partial frame receives bytes continuously: socket idle
                // timeouts alone cannot bound cancellation.
                ws.get_mut().write_all(&[0x81, 126, 2, 0, b'{']).unwrap();
                ws.get_mut().set_read_timeout(Some(Duration::from_millis(10))).unwrap();
                signal.send(()).unwrap();
                let deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    let mut b = [0];
                    match ws.get_mut().read(&mut b) {
                        Ok(0) => break,
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                            ) =>
                        {
                            break;
                        }
                        Err(e) if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => {
                        }
                        other => panic!("unexpected client traffic: {other:?}"),
                    }
                    if ws.get_mut().write_all(b" ").is_err() {
                        break;
                    }
                    assert!(Instant::now() < deadline, "trickled frame prevented worker cancellation");
                    thread::sleep(Duration::from_millis(30));
                }
                return;
            }
            if scenario == "wrong-id" {
                write(&mut ws, json!({"id":999,"type":"result","success":true,"result":[]}));
                return;
            }
            if scenario == "oversized" {
                let _ = ws.send(Message::Text("x".repeat(MAX_MESSAGE + 1).into()));
                return;
            }
            if scenario == "malformed" {
                ws.send(Message::Text("{invalid".into())).unwrap();
                return;
            }
            if scenario == "snapshot-shape" {
                write(&mut ws, json!({"id":2,"type":"result","success":true,"result":{}}));
                return;
            }
            let now = solar_core::now_ms();
            // This older buffered event must not replace the newer snapshot.
            event(&mut ws, EXPORT, entity(EXPORT, "2.5", "kW", now - 2000));
            write(
                &mut ws,
                json!({"id":2,"type":"result","success":true,"result":[
                entity(IMPORT,"0.001","kW",now-1000),entity(EXPORT,"2.6","kW",now-1000)]}),
            );
            released.recv_timeout(Duration::from_secs(5)).unwrap();
            event(&mut ws, IMPORT, entity(IMPORT, "0", "kW", now - 500));
            event(&mut ws, EXPORT, entity(EXPORT, "2.7", "kW", now - 500));
            ws.send(Message::Ping(vec![1, 2, 3].into())).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
            event(&mut ws, EXPORT, Value::Null);
            released.recv_timeout(Duration::from_secs(5)).unwrap();
            if scenario == "reconnect" {
                drop(ws);
                let (socket, _) = listener.accept().unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut ws = tungstenite::accept(socket).unwrap();
                write(&mut ws, json!({"type":"auth_required"}));
                assert_eq!(request(&mut ws)["access_token"], "synthetic-test-token");
                write(&mut ws, json!({"type":"auth_ok"}));
                assert_eq!(request(&mut ws)["id"], 1);
                write(&mut ws, json!({"id":1,"type":"result","success":true,"result":null}));
                assert_eq!(request(&mut ws)["id"], 2);
                write(
                    &mut ws,
                    json!({"id":2,"type":"result","success":true,"result":[
                    entity(IMPORT,"0","kW",now-500),entity(EXPORT,"2.7","kW",now-500)]}),
                );
                released.recv_timeout(Duration::from_secs(5)).unwrap();
                let at = solar_core::now_ms();
                event(&mut ws, IMPORT, entity(IMPORT, "0.001", "kW", at - 2));
                event(&mut ws, EXPORT, entity(EXPORT, "2.8", "kW", at - 2));
                event(&mut ws, IMPORT, entity(IMPORT, "0", "kW", at - 1));
                event(&mut ws, EXPORT, entity(EXPORT, "2.9", "kW", at - 1));
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        });
        let cfg = SolarConfig::from_json(&json!({
            "energySource":{"type":"home-assistant","host":"127.0.0.1","port":device_port},
            "inverterSource":{"host":"127.0.0.1","port":device_port},
            "homeAssistant":{"endpoint":format!("ws://127.0.0.1:{port}/api/websocket"),"tokenEnv":"SOLAR_HA_FIXTURE_TOKEN"},
            "decision":{"stableDurationSeconds":0}
        })).unwrap();
        let service = SolarService::start(cfg);
        if scenario == "trickle-stop" {
            signalled.recv_timeout(Duration::from_secs(5)).unwrap();
            service.stop();
        } else if scenario == "live" || scenario == "reconnect" {
            let s = wait(&service, |s| s.home_assistant.as_ref().is_some_and(|h| h.initialized));
            assert!(!s.evaluation.safe_to_start, "snapshot cannot enable a decision");
            release.send(()).unwrap();
            let s = wait(&service, |s| s.evaluation.surplus_w == Some(2700.0));
            let at = s.reading_at;
            assert!(s.evaluation.safe_to_start);
            assert_eq!(service.snapshot().reading_at, at, "ping/snapshot cannot change evidence time");
            service.select_appliance("washingMachine").unwrap();
            assert_eq!(service.appliance(), "washingMachine");
            assert!(service.set_scenario("excellent").is_err());
            release.send(()).unwrap();
            wait(&service, |s| s.evaluation.state == State::Offline);
            release.send(()).unwrap();
            if scenario == "reconnect" {
                let s = wait(&service, |s| {
                    s.diagnostics.reconnects > 0 && s.home_assistant.as_ref().is_some_and(|h| h.initialized)
                });
                assert!(!s.evaluation.safe_to_start, "reconnect snapshot cannot restore GO");
                release.send(()).unwrap();
                wait(&service, |s| s.evaluation.surplus_w == Some(2900.0));
                release.send(()).unwrap();
            }
        } else {
            let s = wait(&service, |s| s.diagnostics.errors > 0);
            assert_eq!(s.evaluation.state, State::Offline);
            assert!(!format!("{s:?}").contains("synthetic-test-token"), "no server message/token leakage");
        }
        assert!(
            matches!(devices.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "HA starts no direct workers"
        );
        service.stop();
        server.join().unwrap();
    }
}

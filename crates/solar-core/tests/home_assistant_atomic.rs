//! Decision-grade atomic telegram fixtures; no HA deployment or hardware access.

use serde_json::{Value, json};
use solar_core::decision::State;
use solar_core::home_assistant::HaState;
use solar_core::present::{solar_info, tooltip};
use solar_core::{Runtime, SolarConfig, SolarService};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};

const GRID: &str = "sensor.dsmr_atomic_grid_sample";
const AC: &str = "sensor.sg5_0rs_local_total_active_power";
const DC: &str = "sensor.sg5_0rs_local_total_dc_power";
const DEMAND: &str = "sensor.electricity_meter_current_average_demand";
const PEAK: &str = "sensor.electricity_meter_maximum_demand_current_month";
const IMPORT_1: &str = "sensor.electricity_meter_energy_consumption_tarif_1";
const IMPORT_2: &str = "sensor.electricity_meter_energy_consumption_tarif_2";
const EXPORT_1: &str = "sensor.electricity_meter_energy_production_tarif_1";
const EXPORT_2: &str = "sensor.electricity_meter_energy_production_tarif_2";
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

fn full_sample(sequence: u64, at: i64) -> Value {
    let mut value = sample("meter-features", sequence, at, 800.0, 100.0);
    let attributes = value["attributes"].as_object_mut().unwrap();
    attributes.extend(
        json!({
            "demand_w":2000, "monthly_peak_w":5000,
            "monthly_peak_at":"2027-01-15T07:00:00Z", "monthly_peak_month":"2027-01",
            "import_tariff_1_wh":100000,"import_tariff_2_wh":50000,
            "export_tariff_1_wh":20000,"export_tariff_2_wh":10000,
            "optional_status":{"demand":"available","monthly_peak":"available",
                "import_tariff_1":"available","import_tariff_2":"available",
                "export_tariff_1":"available","export_tariff_2":"available"}
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    value
}

fn meter_value(id: &str, value: &str, unit: &str, at: i64) -> Value {
    let mut attributes = json!({"unit_of_measurement":unit});
    if matches!(id, IMPORT_1 | IMPORT_2 | EXPORT_1 | EXPORT_2) {
        attributes["device_class"] = json!("energy");
        attributes["state_class"] = json!("total_increasing");
    }
    json!({
        "entity_id":id,"state":value,"last_changed":iso(at),
        "last_updated":iso(at),"last_reported":iso(at),"attributes":attributes
    })
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
    assert_eq!(config.home_assistant.current_demand, DEMAND);
    assert_eq!(config.home_assistant.monthly_peak, PEAK);
    assert_eq!(config.home_assistant.energy_import_tariff1, IMPORT_1);
    assert_eq!(config.home_assistant.energy_import_tariff2, IMPORT_2);
    assert_eq!(config.home_assistant.energy_export_tariff1, EXPORT_1);
    assert_eq!(config.home_assistant.energy_export_tariff2, EXPORT_2);
    assert_eq!(config.home_assistant.meter_data_stale_seconds, 90.0);
    assert_eq!(config.home_assistant.meter_data_max_event_skew_seconds, 30.0);
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
    for id in [DEMAND, PEAK, IMPORT_1, IMPORT_2, EXPORT_1, EXPORT_2] {
        assert!(!state.relevant(id), "obsolete optional mappings must not be subscribed");
    }
    state.state(GRID, &sample("synthetic-session-a", 1, AT, 0.0, 3000.0), AT);
    assert_eq!(state.grid(AT, 30.0).unwrap().grid_export_w, 3000.0, "payload powers are W, not kW");
    state.state(GRID, &json!({"state":"unavailable","attributes":{"schema_version":1}}), AT);
    assert!(state.grid(AT, 30.0).is_none());
}

#[test]
fn atomic_optional_fields_feed_existing_peak_and_partial_day_engines() {
    let config = SolarConfig::from_json(&json!({
        "energySource":{"type":"home-assistant"},
        "homeAssistant":{"atomicGrid":GRID,"meterDataStaleSeconds":10}
    }))
    .unwrap();
    let mut runtime = Runtime::new(config);
    runtime.ha_connected();
    runtime.ha_ready();
    let at = AT + 5 * 60_000;
    runtime.ha_state(GRID, &full_sample(1, at - 1000), at);
    for (id, value, unit) in [
        (DEMAND, "2.0", "kW"),
        (PEAK, "5.0", "kW"),
        (IMPORT_1, "100.000", "kWh"),
        (IMPORT_2, "50.000", "kWh"),
        (EXPORT_1, "20.000", "kWh"),
        (EXPORT_2, "10.000", "kWh"),
    ] {
        runtime.ha_state(id, &meter_value(id, value, unit, at), at);
    }

    let next_at = at + 1000;
    runtime.ha_state(GRID, &full_sample(2, next_at), next_at);
    let first = runtime.snapshot(next_at);
    assert_eq!(first.evaluation.current_demand_w, Some(2000.0));
    assert_eq!(first.evaluation.monthly_peak_w, Some(5000.0));
    assert!(first.peak_forecast.available, "{:?}", first.peak_forecast.reason);
    assert!(first.daily_energy.available);
    assert_eq!(first.daily_energy.import_wh, Some(0));
    assert_eq!(first.daily_energy.export_wh, Some(0));
    assert_eq!(
        solar_info(&first).section("Today").unwrap().value("Coverage"),
        Some("Partial today; baseline starts during this run")
    );
    assert!(tooltip(&first).contains("Partial today in "));

    let counter_at = at + 2000;
    let mut counters = full_sample(3, counter_at);
    counters["attributes"]["import_tariff_1_wh"] = json!(100001);
    runtime.ha_state(GRID, &counters, counter_at);
    let partial = runtime.snapshot(counter_at);
    assert_eq!(partial.daily_energy.import_wh, Some(1));
    assert_eq!(partial.daily_energy.export_wh, Some(0));
    assert_eq!(partial.daily_energy.since_minute, Some(9 * 60 + 4));

    let stale_at = at + 11_000;
    runtime.ha_state(GRID, &sample("meter-features", 4, stale_at, 800.0, 100.0), stale_at);
    let stale = runtime.snapshot(stale_at);
    assert!(stale.evaluation.state != State::Offline, "optional meter-data expiry cannot invalidate grid");
    assert!(!stale.peak_forecast.available);
    assert!(!stale.daily_energy.available);
    assert_eq!(
        stale.home_assistant.as_ref().unwrap().daily_energy_issue.as_deref(),
        Some("ha-atomic-optional-not-published")
    );
}

#[test]
fn counter_decrease_invalidates_partial_totals_without_affecting_atomic_grid() {
    let mut runtime = runtime();
    let at = AT + 5 * 60_000;
    runtime.ha_state(GRID, &full_sample(1, at), at);
    for (id, value, unit) in [
        (DEMAND, "2.0", "kW"),
        (PEAK, "5.0", "kW"),
        (IMPORT_1, "100.000", "kWh"),
        (IMPORT_2, "50.000", "kWh"),
        (EXPORT_1, "20.000", "kWh"),
        (EXPORT_2, "10.000", "kWh"),
    ] {
        runtime.ha_state(id, &meter_value(id, value, unit, at), at);
    }
    assert!(runtime.snapshot(at).daily_energy.available);

    let next_at = at + 1000;
    let mut decreased = full_sample(2, next_at);
    decreased["attributes"]["import_tariff_1_wh"] = json!(99999);
    runtime.ha_state(GRID, &decreased, next_at);
    runtime.ha_state(IMPORT_1, &meter_value(IMPORT_1, "99.999", "kWh", next_at), next_at);
    let snapshot = runtime.snapshot(next_at);
    assert!(!snapshot.daily_energy.available);
    assert_eq!(snapshot.daily_energy.reason, "counter-decreased");
    assert_ne!(snapshot.evaluation.state, State::Offline, "bad optional counters cannot invalidate grid");
}

#[test]
fn separate_optional_values_cannot_fill_a_grid_only_atomic_publisher() {
    let mut runtime = runtime();
    let at = AT + 5 * 60_000;
    for (id, value, unit) in [
        (DEMAND, "0.0", "kW"),
        (PEAK, "5.0", "kW"),
        (IMPORT_1, "100.000", "kWh"),
        (IMPORT_2, "50.000", "kWh"),
        (EXPORT_1, "20.000", "kWh"),
        (EXPORT_2, "10.000", "kWh"),
    ] {
        runtime.ha_snapshot_state(id, &meter_value(id, value, unit, at), at);
    }

    runtime.ha_state(GRID, &sample("meter-features", 1, at, 0.0, 0.0), at);
    let first = runtime.snapshot(at);
    assert_eq!(first.daily_energy.reason, "ha-atomic-optional-not-published");
    assert_eq!(first.peak_forecast.reason, "ha-atomic-optional-not-published");
    let info = solar_info(&first);
    assert_eq!(info.section("Today").unwrap().value("Why"), Some("atomic publisher has no optional-field contract"));
    assert_eq!(
        info.section("Quarter-hour peak").unwrap().value("Forecast"),
        Some("atomic publisher has no optional-field contract")
    );

    let next_at = at + 1000;
    runtime.ha_state(GRID, &sample("meter-features", 2, next_at, 0.0, 0.0), next_at);
    for (id, value, unit) in [
        (DEMAND, "0.0", "kW"),
        (PEAK, "5.0", "kW"),
        (IMPORT_1, "100.000", "kWh"),
        (IMPORT_2, "50.000", "kWh"),
        (EXPORT_1, "20.000", "kWh"),
        (EXPORT_2, "10.000", "kWh"),
    ] {
        runtime.ha_state(id, &meter_value(id, value, unit, at), next_at);
    }
    let next = runtime.snapshot(next_at);
    assert_eq!(next.daily_energy.reason, "ha-atomic-optional-not-published");
    assert_eq!(next.peak_forecast.reason, "ha-atomic-optional-not-published");
    assert_ne!(next.evaluation.state, State::Offline, "the optional-data gate cannot invalidate the grid");
}

#[test]
fn atomic_peak_occurrence_month_is_not_assumed_from_telegram_month() {
    let mut state = online();
    let grid_time = chrono::DateTime::parse_from_rfc3339("2027-01-31T22:59:50Z").unwrap().timestamp_millis();
    let next_day = grid_time + 15_000;
    state.state(GRID, &full_sample(1, grid_time), grid_time);
    for (id, value, unit) in [
        (DEMAND, "2.0", "kW"),
        (PEAK, "5.0", "kW"),
        (IMPORT_1, "100.000", "kWh"),
        (IMPORT_2, "50.000", "kWh"),
        (EXPORT_1, "20.000", "kWh"),
        (EXPORT_2, "10.000", "kWh"),
    ] {
        state.state(id, &meter_value(id, value, unit, grid_time), grid_time);
    }
    assert!(state.peak_data_issue(grid_time).is_none());
    assert!(state.daily_data_issue(grid_time).is_none());

    state.state(GRID, &full_sample(2, next_day), next_day);
    assert_eq!(state.peak_data_issue(next_day), Some("ha-atomic-optional-previous-month"));
    for (id, value, unit) in [
        (IMPORT_1, "100.001", "kWh"),
        (IMPORT_2, "50.001", "kWh"),
        (EXPORT_1, "20.001", "kWh"),
        (EXPORT_2, "10.001", "kWh"),
    ] {
        state.state(id, &meter_value(id, value, unit, next_day), next_day);
    }
    assert!(state.daily_data_issue(next_day).is_none());
    let mut reading = state.grid(next_day, 30.0).unwrap();
    state.enrich_reading(&mut reading, next_day);
    assert!(reading.monthly_peak_w.is_none(), "old peak must not become the current-month peak");
    assert!(reading.energy_totals_wh.is_some(), "same-telegram counters are valid across a local day boundary");
    assert!(state.grid(next_day, 30.0).is_some(), "optional boundary mismatch cannot invalidate grid");
}

#[test]
fn atomic_optional_snapshots_cannot_restore_features_until_a_new_live_sequence() {
    let mut state = online();
    let at = AT + 5 * 60_000;
    state.state(GRID, &full_sample(1, at - 1000), at);
    for (id, value, unit) in [
        (DEMAND, "2.0", "kW"),
        (PEAK, "5.0", "kW"),
        (IMPORT_1, "100.000", "kWh"),
        (IMPORT_2, "50.000", "kWh"),
        (EXPORT_1, "20.000", "kWh"),
        (EXPORT_2, "10.000", "kWh"),
    ] {
        state.state(id, &meter_value(id, value, unit, at), at);
    }
    assert!(state.daily_data_issue(at).is_none());
    assert!(state.peak_data_issue(at).is_none());

    state.disconnect("ha-read-or-protocol");
    state.connected();
    state.ready();
    let reconnect_at = at + 1000;
    state.snapshot_state(GRID, &full_sample(2, reconnect_at), reconnect_at);
    for (id, value, unit) in [
        (DEMAND, "2.0", "kW"),
        (PEAK, "5.0", "kW"),
        (IMPORT_1, "100.000", "kWh"),
        (IMPORT_2, "50.000", "kWh"),
        (EXPORT_1, "20.000", "kWh"),
        (EXPORT_2, "10.000", "kWh"),
    ] {
        state.snapshot_state(id, &meter_value(id, value, unit, at), reconnect_at);
    }
    assert!(state.grid(reconnect_at, 30.0).is_none());
    assert_eq!(state.daily_data_issue(reconnect_at), Some("ha-meter-data-grid-unavailable"));
    assert_eq!(state.peak_data_issue(reconnect_at), Some("ha-meter-data-grid-unavailable"));

    let live_at = reconnect_at + 1000;
    state.state(GRID, &full_sample(3, live_at), live_at);
    for (id, value, unit) in [
        (DEMAND, "2.0", "kW"),
        (PEAK, "5.0", "kW"),
        (IMPORT_1, "100.000", "kWh"),
        (IMPORT_2, "50.000", "kWh"),
        (EXPORT_1, "20.000", "kWh"),
        (EXPORT_2, "10.000", "kWh"),
    ] {
        state.state(id, &meter_value(id, value, unit, at), live_at);
    }
    assert!(state.peak_data_issue(live_at).is_none());
    assert!(state.daily_data_issue(live_at).is_none());

    for (id, value, unit) in [
        (DEMAND, "2.1", "kW"),
        (PEAK, "5.1", "kW"),
        (IMPORT_1, "100.001", "kWh"),
        (IMPORT_2, "50.001", "kWh"),
        (EXPORT_1, "20.001", "kWh"),
        (EXPORT_2, "10.001", "kWh"),
    ] {
        state.state(id, &meter_value(id, value, unit, live_at), live_at);
    }
    assert!(state.daily_data_issue(live_at).is_none());
    assert!(state.peak_data_issue(live_at).is_none());
}

#[test]
fn atomic_optional_fields_ignore_separate_entity_metadata_and_old_age_limits() {
    let mut state = online();
    let at = AT + 5 * 60_000;
    state.state(GRID, &full_sample(1, at - 1000), at);
    state.state(DEMAND, &meter_value(DEMAND, "2.0", "kW", at), at);
    state.state(PEAK, &meter_value(PEAK, "5.0", "kW", at), at);
    for (id, value) in [(IMPORT_1, "100.000"), (IMPORT_2, "50.000"), (EXPORT_1, "20.000"), (EXPORT_2, "10.000")] {
        state.state(id, &meter_value(id, value, "kWh", at), at);
    }
    let mut reading = state.grid(at, 30.0).unwrap();
    state.enrich_reading(&mut reading, at);
    assert_eq!(reading.current_demand_w, Some(2000.0));
    assert_eq!(reading.monthly_peak_w, Some(5000.0));
    assert_eq!(reading.meter_month, Some(202701));
    assert_eq!(reading.meter_offset_minutes, Some(60));
    assert_eq!(reading.meter_minute_of_day, Some(9 * 60 + 4));
    assert!(reading.energy_totals_wh.is_some());
    assert!(state.daily_data_issue(at).is_none());
    assert!(state.peak_data_issue(at).is_none());

    let too_far = at + 31_000;
    state.state(GRID, &full_sample(2, too_far), too_far);
    let mut reading = state.grid(too_far, 30.0).unwrap();
    state.enrich_reading(&mut reading, too_far);
    assert_eq!(reading.current_demand_w, Some(2000.0));
    assert!(reading.energy_totals_wh.is_some());
    assert!(state.daily_data_issue(too_far).is_none());

    let bad_config = SolarConfig::from_json(&json!({
        "energySource":{"type":"home-assistant"},
        "homeAssistant":{"currentDemand":"sensor.invalid\nentity","meterDataStaleSeconds":90}
    }));
    assert!(bad_config.is_err());
    let invalid_class = meter_value(IMPORT_1, "100.000", "kWh", at);
    let mut invalid_class = invalid_class;
    invalid_class["attributes"]["state_class"] = json!("measurement");
    state.snapshot_state(IMPORT_1, &invalid_class, too_far);
    state.state(GRID, &full_sample(3, too_far + 1000), too_far + 1000);
    assert!(state.daily_data_issue(too_far + 1000).is_none());
    assert!(state.grid(too_far + 1000, 30.0).is_some());
}

#[test]
fn unchanged_atomic_optionals_remain_acquired_but_expire_with_the_grid() {
    let mut runtime = runtime();
    for sequence in 1..=41 {
        let at = AT + 5 * 60_000 + (sequence as i64 - 1) * 10_000;
        let mut sample = full_sample(sequence, at);
        sample["attributes"]["demand_w"] = json!(0);
        sample["attributes"]["import_w"] = json!(0);
        sample["attributes"]["export_w"] = json!(3000);
        runtime.ha_state(GRID, &sample, at);
        let snapshot = runtime.snapshot(at);
        assert!(snapshot.daily_energy.available);
        assert_eq!(snapshot.daily_energy.import_wh, Some(0));
        assert_eq!(snapshot.daily_energy.export_wh, Some(0));
        assert_eq!(snapshot.evaluation.current_demand_w, Some(0.0));
        assert!(snapshot.home_assistant.as_ref().unwrap().peak_forecast_issue.is_none());
    }
    let at = AT + 5 * 60_000 + 400_000;
    assert!(runtime.snapshot(at).evaluation.safe_to_start);
    let stale = runtime.snapshot(at + 30_000);
    assert!(!stale.peak_forecast.available);
    assert!(!stale.daily_energy.available);
    assert!(!stale.evaluation.safe_to_start);
}

#[test]
fn malformed_atomic_optionals_are_isolated_and_never_carried_forward() {
    for (field, invalid) in [
        ("demand_w", json!(-1)),
        ("demand_w", json!(1_000_001)),
        ("demand_w", json!(0.5)),
        ("demand_w", json!("NaN")),
        ("demand_w", Value::Null),
        ("monthly_peak_w", json!(-1)),
        ("monthly_peak_w", json!(1_000_001)),
        ("monthly_peak_w", json!(1.5)),
        ("monthly_peak_at", json!("2027-01-16T00:00:00Z")),
        ("monthly_peak_at", json!("2027-01-15T07:00:00+01:00")),
        ("monthly_peak_month", json!("2027-02")),
        ("monthly_peak_month", json!("2027-1")),
        ("monthly_peak_at", Value::Null),
        ("import_tariff_1_wh", json!(-1)),
        ("import_tariff_2_wh", json!(1_000_000_000)),
        ("export_tariff_1_wh", json!(1.5)),
        ("export_tariff_2_wh", json!("10000")),
    ] {
        let mut state = online();
        state.state(GRID, &full_sample(1, AT), AT);
        let mut bad = full_sample(2, AT + 1000);
        bad["attributes"][field] = invalid;
        state.state(GRID, &bad, AT + 1000);
        let mut reading = state.grid(AT + 1000, 30.0).expect(field);
        state.enrich_reading(&mut reading, AT + 1000);
        let counter = field.contains("tariff");
        assert_eq!(state.daily_data_issue(AT + 1000).is_some(), counter, "{field}");
        assert_eq!(state.peak_data_issue(AT + 1000).is_some(), !counter, "{field}");
        assert_eq!(reading.energy_totals_wh.is_none(), counter, "{field}");
        assert_eq!(reading.current_demand_w.is_none(), field == "demand_w", "{field}");
        assert_eq!(reading.monthly_peak_w.is_none(), field.starts_with("monthly_peak"), "{field}");
    }
}

#[test]
fn optional_status_null_contract_and_grid_only_compatibility_are_explicit() {
    for (group, fields) in [
        ("demand", vec!["demand_w"]),
        ("monthly_peak", vec!["monthly_peak_w", "monthly_peak_at", "monthly_peak_month"]),
        ("import_tariff_1", vec!["import_tariff_1_wh"]),
        ("import_tariff_2", vec!["import_tariff_2_wh"]),
        ("export_tariff_1", vec!["export_tariff_1_wh"]),
        ("export_tariff_2", vec!["export_tariff_2_wh"]),
    ] {
        for status in ["missing", "invalid", "unexpected"] {
            let mut state = online();
            let mut value = full_sample(1, AT);
            value["attributes"]["optional_status"][group] = json!(status);
            for field in &fields {
                value["attributes"][*field] = Value::Null;
            }
            state.state(GRID, &value, AT);
            assert!(state.grid(AT, 30.0).is_some());
            let issue = if group.contains("tariff") { state.daily_data_issue(AT) } else { state.peak_data_issue(AT) };
            assert_eq!(
                issue,
                Some(if status == "missing" { "ha-atomic-optional-missing" } else { "ha-atomic-optional-invalid" })
            );

            // A missing/invalid declaration cannot conceal a non-null field.
            value["attributes"][fields[0]] = json!(0);
            value["state"] = json!("2");
            value["attributes"]["sample_sequence"] = json!(2);
            value["attributes"]["meter_timestamp"] = json!(iso(AT + 1000));
            value["attributes"]["received_at"] = json!(iso(AT + 1000));
            state.state(GRID, &value, AT + 1000);
            let issue = if group.contains("tariff") {
                state.daily_data_issue(AT + 1000)
            } else {
                state.peak_data_issue(AT + 1000)
            };
            assert_eq!(issue, Some("ha-atomic-optional-invalid"));
        }
    }
    let mut state = online();
    state.state(GRID, &full_sample(1, AT), AT);
    state.state(GRID, &sample("meter-features", 2, AT + 1000, 0.0, 0.0), AT + 1000);
    assert_eq!(state.daily_data_issue(AT + 1000), Some("ha-atomic-optional-not-published"));
    assert_eq!(state.peak_data_issue(AT + 1000), Some("ha-atomic-optional-not-published"));
}

#[test]
fn malformed_status_objects_and_omitted_fields_do_not_reject_grid() {
    for status in [json!({}), json!([]), json!("available"), json!(true)] {
        let mut state = online();
        let mut value = full_sample(1, AT);
        value["attributes"]["optional_status"] = status;
        state.state(GRID, &value, AT);
        assert!(state.grid(AT, 30.0).is_some());
        assert_eq!(state.peak_data_issue(AT), Some("ha-atomic-optional-invalid"));
        assert_eq!(state.daily_data_issue(AT), Some("ha-atomic-optional-invalid"));
    }
    let mut state = online();
    let mut value = full_sample(1, AT);
    value["attributes"]["optional_status"]["demand"] = json!("missing");
    value["attributes"].as_object_mut().unwrap().remove("demand_w");
    state.state(GRID, &value, AT);
    assert!(state.grid(AT, 30.0).is_some());
    assert_eq!(state.peak_data_issue(AT), Some("ha-atomic-optional-invalid"));
    assert!(state.daily_data_issue(AT).is_none());
}

#[test]
fn partial_counters_restart_on_brussels_day_and_reconnect_not_across_resets() {
    let mut runtime = runtime();
    let at = chrono::DateTime::parse_from_rfc3339("2027-01-15T22:59:50Z").unwrap().timestamp_millis();
    runtime.ha_state(GRID, &full_sample(1, at), at);
    let next_day = at + 15_000;
    let mut value = full_sample(2, next_day);
    value["attributes"]["import_tariff_1_wh"] = json!(101000);
    runtime.ha_state(GRID, &value, next_day);
    let snapshot = runtime.snapshot(next_day);
    assert_eq!(snapshot.daily_energy.import_wh, Some(0), "new day starts a partial baseline, not a midnight estimate");
    assert_eq!(snapshot.daily_energy.since_minute, Some(0));
    value = full_sample(3, next_day + 1000);
    value["attributes"]["import_tariff_1_wh"] = json!(101010);
    runtime.ha_state(GRID, &value, next_day + 1000);
    assert_eq!(runtime.snapshot(next_day + 1000).daily_energy.import_wh, Some(10));
    runtime.ha_disconnect("ha-read-or-protocol");
    runtime.ha_connected();
    runtime.ha_ready();
    value = full_sample(4, next_day + 2000);
    value["attributes"]["import_tariff_1_wh"] = json!(101100);
    runtime.ha_snapshot_state(GRID, &value, next_day + 2000);
    assert!(!runtime.snapshot(next_day + 2000).daily_energy.available);
    value = full_sample(5, next_day + 3000);
    value["attributes"]["import_tariff_1_wh"] = json!(101200);
    runtime.ha_state(GRID, &value, next_day + 3000);
    assert_eq!(runtime.snapshot(next_day + 3000).daily_energy.import_wh, Some(0));
}

#[test]
fn peak_month_uses_brussels_occurrence_time_in_summer_not_utc_month() {
    let at = chrono::DateTime::parse_from_rfc3339("2027-03-31T22:00:10Z").unwrap().timestamp_millis();
    let mut state = online();
    let mut value = full_sample(1, at);
    value["attributes"]["monthly_peak_at"] = json!("2027-03-31T22:00:00Z");
    value["attributes"]["monthly_peak_month"] = json!("2027-04");
    state.state(GRID, &value, at);
    assert!(state.peak_data_issue(at).is_none());
    let mut reading = state.grid(at, 30.0).unwrap();
    state.enrich_reading(&mut reading, at);
    assert_eq!(reading.monthly_peak_month, Some(202704));
    assert_eq!(reading.meter_offset_minutes, Some(120));

    // Replayed sequence cannot change optional values or supply fresh evidence.
    value["attributes"]["demand_w"] = json!(10000);
    state.state(GRID, &value, at + 1000);
    assert!(state.grid(at + 1000, 30.0).is_none());
    assert_eq!(state.peak_data_issue(at + 1000), Some("ha-meter-data-grid-unavailable"));
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

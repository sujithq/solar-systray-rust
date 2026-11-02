use serde_json::json;
use solar_core::ha_daily::day_bounds;
use solar_core::present::{solar_info, tooltip};
use solar_core::{Runtime, SolarConfig};

const GRID: &str = "sensor.fixture_atomic";

fn time(text: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(text).unwrap().timestamp_millis()
}

fn sample(n: u64, at: i64, total: i64) -> serde_json::Value {
    let iso = chrono::DateTime::from_timestamp_millis(at).unwrap().to_rfc3339();
    json!({"entity_id":GRID,"state":n.to_string(),"attributes":{
        "schema_version":1,"session":"fixture-session","sample_sequence":n,
        "meter_timestamp":iso,"received_at":iso,"import_w":0,"export_w":3000,
        "demand_w":0,"monthly_peak_w":2500,
        "monthly_peak_at":"2027-01-14T20:00:00Z","monthly_peak_month":"2027-01",
        "import_tariff_1_wh":total,"import_tariff_2_wh":0,
        "export_tariff_1_wh":total,"export_tariff_2_wh":0,
        "optional_status":{"demand":"available","monthly_peak":"available",
            "import_tariff_1":"available","import_tariff_2":"available",
            "export_tariff_1":"available","export_tariff_2":"available"}
    }})
}

fn runtime() -> Runtime {
    let mut runtime = Runtime::new(
        SolarConfig::from_json(&json!({
            "energySource":{"type":"home-assistant"},"homeAssistant":{"atomicGrid":GRID}
        }))
        .unwrap(),
    );
    runtime.ha_connected();
    runtime.ha_ready();
    runtime
}

#[test]
fn brussels_midnight_bounds_include_short_and_long_dst_days() {
    for (at, start, end, hours) in [
        ("2027-03-28T12:00:00Z", "2027-03-27T23:00:00Z", "2027-03-28T22:00:00Z", 23),
        ("2027-10-31T12:00:00Z", "2027-10-30T22:00:00Z", "2027-10-31T23:00:00Z", 25),
        ("2027-01-15T12:00:00Z", "2027-01-14T23:00:00Z", "2027-01-15T23:00:00Z", 24),
    ] {
        let bounds = day_bounds(time(at)).unwrap();
        assert_eq!(bounds, (time(start), time(end)));
        assert_eq!(bounds.1 - bounds.0, hours * 3_600_000);
    }
}

#[test]
fn live_exact_midnight_is_full_but_later_samples_show_precise_partial_coverage() {
    let midnight = time("2027-01-14T23:00:00Z");
    let mut full = runtime();
    full.ha_state(GRID, &sample(1, midnight, 1000), midnight);
    full.ha_state(GRID, &sample(2, midnight + 10_000, 1010), midnight + 10_000);
    let s = full.snapshot(midnight + 10_000);
    assert_eq!(s.daily_energy.import_wh, Some(10));
    assert!(s.daily_coverage.as_ref().unwrap().midnight);
    assert_eq!(
        solar_info(&s).section("Today").unwrap().value("Coverage"),
        Some("Today from validated Brussels midnight")
    );

    let mut partial = runtime();
    let at = midnight + 60_000 + 42_500;
    partial.ha_state(GRID, &sample(1, at, 1000), at);
    partial.ha_state(GRID, &sample(2, at + 10_000, 1005), at + 10_000);
    let s = partial.snapshot(at + 10_000);
    let info = solar_info(&s);
    assert_eq!(info.section("Today").unwrap().value("Coverage"), Some("Partial today; exact observed baseline"));
    assert_eq!(
        info.section("Today").unwrap().value("Baseline sample"),
        Some("2027-01-15 00:01:42.500 Europe/Brussels UTC+01:00")
    );
    assert_eq!(s.daily_energy.import_wh, Some(5));
    assert!(tooltip(&s).contains("Partial today"));
    assert!(!partial.snapshot(at + 40_000).daily_energy.available, "history cannot renew freshness");
}

#[test]
fn live_counter_reset_session_and_day_boundaries_do_not_leak_arithmetic() {
    let at = time("2027-01-15T22:59:40Z");
    let mut runtime = runtime();
    runtime.ha_state(GRID, &sample(1, at, 1000), at);
    runtime.ha_state(GRID, &sample(2, at + 10_000, 1), at + 10_000);
    assert_eq!(runtime.snapshot(at + 10_000).daily_energy.reason, "counter-decreased");
    assert!(runtime.snapshot(at + 10_000).peak_forecast.available, "counter failure does not suppress forecast");
    runtime.ha_state(GRID, &sample(3, at + 20_000, 2), at + 20_000);
    let next = runtime.snapshot(at + 20_000);
    assert_eq!(next.daily_energy.import_wh, Some(0));
    assert!(next.daily_coverage.unwrap().midnight);
    let mut changed = sample(1, at + 30_000, 3);
    changed["attributes"]["session"] = json!("new-session");
    runtime.ha_state(GRID, &changed, at + 30_000);
    let s = runtime.snapshot(at + 30_000);
    assert_eq!(s.daily_energy.import_wh, Some(0));
    assert!(!s.daily_coverage.unwrap().midnight);
    assert_eq!(s.daily_history_issue.as_deref(), Some("daily-history-session-changed"));
}

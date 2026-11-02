use serde_json::json;
use solar_core::ha_reported_energy::ReportedEnergy;
use solar_core::present::solar_info;
use solar_core::{Runtime, SolarConfig};

#[test]
fn reported_tariff_energy_has_its_own_provenance_without_creating_a_live_reading() {
    let start = solar_core::utc_ms(2027, 1, 14, 23, 0, 0);
    let mut runtime = Runtime::new(
        SolarConfig::from_json(&json!({
            "energySource":{"type":"home-assistant"},"homeAssistant":{"atomicGrid":"sensor.fixture"}
        }))
        .unwrap(),
    );
    let mut snapshot = runtime.snapshot(start + 60_000);
    assert!(
        solar_info(&snapshot).section("Today").unwrap().value("HA reported energy").unwrap().contains("Pending:"),
        "startup must not silently hide the pending primary reported source"
    );
    snapshot.reported_energy = Some(ReportedEnergy {
        day_start: start,
        as_of: start + 50_000,
        import_wh: 2100,
        export_wh: 600,
        recorded_points: [start; 4],
    });
    let info = solar_info(&snapshot);
    let today = info.section("Today").unwrap();
    assert_eq!(today.value("Imported"), Some("2.100 kWh"));
    assert_eq!(today.value("Exported"), Some("0.600 kWh"));
    assert!(today.value("Coverage").unwrap().contains("held midnight boundary"));
    assert!(today.value("Source").unwrap().contains("not atomic acquisition"));
    assert!(snapshot.reading_at.is_none());
    assert!(!snapshot.evaluation.safe_to_start);
    let restored: solar_core::Snapshot = serde_json::from_value(serde_json::to_value(snapshot).unwrap()).unwrap();
    assert_eq!(restored.reported_energy.unwrap().import_wh, 2100);
}

#[test]
fn reported_history_loading_and_errors_are_visible_without_a_report() {
    let mut runtime = Runtime::new(
        SolarConfig::from_json(&json!({
            "energySource":{"type":"home-assistant"},"homeAssistant":{"atomicGrid":"sensor.fixture"}
        }))
        .unwrap(),
    );
    let mut snapshot = runtime.snapshot(solar_core::utc_ms(2027, 1, 15, 12, 0, 0));
    for (status, text) in [
        ("reported-energy-loading", "Loading original tariff recorder history"),
        ("history-response-timeout", "history-response-timeout"),
    ] {
        snapshot.reported_energy_issue = Some(status.into());
        for live in [false, true] {
            snapshot.evaluation.state =
                if live { solar_core::decision::State::Idle } else { solar_core::decision::State::Offline };
            snapshot.evaluation.surplus_w = live.then_some(0.0);
            snapshot.daily_energy.available = true;
            snapshot.daily_energy.import_wh = Some(0);
            snapshot.daily_energy.export_wh = Some(4);
            snapshot.daily_energy.since_minute = Some(14 * 60 + 26);
            let info = solar_info(&snapshot);
            let today = info.section("Today").unwrap();
            assert!(today.value("HA reported energy").unwrap().contains(text));
            if live {
                assert_eq!(today.value("Coverage"), Some("Partial today; exact observed baseline"));
                assert_eq!(today.value("Exported"), Some("0.004 kWh"));
            }
        }
        assert!(snapshot.reading_at.is_none());
        assert!(!snapshot.evaluation.safe_to_start);
    }
}

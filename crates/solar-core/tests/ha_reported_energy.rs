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

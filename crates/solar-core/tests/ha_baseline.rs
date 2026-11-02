//! Recorded baseline scheduling and private cache, without HA or device access.

use serde_json::json;
use solar_core::ha_baseline::{NightWindow, RecordedBaseline, Schedule, completed_window, load_cache, save_cache};
use solar_core::{SolarConfig, SolarService};

fn time(text: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(text).unwrap().timestamp_millis()
}

fn config() -> SolarConfig {
    SolarConfig::from_json(&json!({
        "energySource":{"type":"home-assistant"},
        "homeAssistant":{"atomicGrid":"sensor.dsmr_atomic_grid_sample"},
        "location":{"latitude":50.85,"longitude":4.35}
    }))
    .unwrap()
}

#[test]
fn history_settings_default_to_six_and_validate_round_trip() {
    let c = config();
    assert!(c.home_assistant.night_baseline_enabled);
    assert_eq!(c.home_assistant.night_baseline_refresh_at, "06:00");
    assert_eq!(SolarConfig::from_json(&c.to_json()).unwrap(), c);
    for bad in ["6:00", "24:00", "12:60", "aa:bb", "06:00\n", " 6:00"] {
        assert!(SolarConfig::from_json(&json!({"homeAssistant":{"nightBaseline":{"refreshAt":bad}}})).is_err());
    }
}

#[test]
fn startup_catches_up_and_daily_schedule_uses_brussels_not_pc_time() {
    let mut schedule = Schedule::default();
    assert!(schedule.due(time("2026-01-03T03:00:00Z"), "06:00"));
    assert!(!schedule.due(time("2026-01-03T04:59:59Z"), "06:00"));
    assert!(schedule.due(time("2026-01-03T05:00:00Z"), "06:00"));
    assert!(!schedule.due(time("2026-01-03T05:00:30Z"), "06:00"));
    assert!(schedule.due(time("2026-01-04T06:00:00Z"), "06:00"), "catches a missed running tick");
    let mut later = Schedule::default();
    assert!(later.due(time("2026-06-03T03:00:00Z"), "07:15"));
    assert!(!later.due(time("2026-06-03T05:14:00Z"), "07:15"));
    assert!(later.due(time("2026-06-03T05:15:00Z"), "07:15"));
    let mut restart = Schedule::default();
    assert!(restart.due(time("2026-01-04T12:00:00Z"), "06:00"));
}

#[test]
fn dst_schedule_runs_once_on_repeated_hour_and_catches_nonexistent_hour() {
    let mut autumn = Schedule::default();
    assert!(autumn.due(time("2026-10-25T00:30:00Z"), "02:30"));
    assert!(!autumn.due(time("2026-10-25T01:30:00Z"), "02:30"));
    let mut spring = Schedule::default();
    assert!(spring.due(time("2026-03-29T00:59:00Z"), "02:30"));
    assert!(spring.due(time("2026-03-29T01:00:00Z"), "02:30"), "first valid minute after clock jumps");
}

#[test]
fn window_never_uses_an_unfinished_winter_night_or_summer_daytime() {
    let c = config();
    let before_sunrise = time("2026-11-02T05:00:00Z");
    let previous = completed_window(&c, before_sunrise).unwrap();
    assert!(previous.end < time("2026-11-01T12:00:00Z"));
    let after_sunrise = time("2026-11-02T12:00:00Z");
    let current = completed_window(&c, after_sunrise).unwrap();
    assert!(current.end > previous.end && current.end <= after_sunrise);
    let summer = completed_window(&c, time("2026-06-15T04:00:00Z")).unwrap();
    assert!(summer.end < time("2026-06-15T04:00:00Z"));
    for at in [summer.start, summer.end - 1, previous.start, previous.end - 1] {
        assert_eq!(solar_core::sun::sun_state(&c.location, at).state, solar_core::sun::Sun::Down);
    }
    let mut no_location = c;
    no_location.location.latitude = None;
    no_location.location.longitude = None;
    assert!(completed_window(&no_location, after_sunrise).is_none());
}

#[test]
fn cache_restores_baseline_before_live_data_and_rejects_source_changes() {
    let c = config();
    let now = time("2026-11-02T12:00:00Z");
    let dir = std::env::temp_dir().join(format!("solar-recorded-baseline-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("baseline.json");
    let window = completed_window(&c, now).unwrap();
    let baseline = RecordedBaseline {
        window,
        calculated_at: now,
        watts: 321.0,
        samples: ((window.end - window.start) / 60_000) as usize,
        unit: "W".into(),
        provenance: "HA recorder atomic grid import; first sample per UTC minute median".into(),
    };
    save_cache(&path, &c, &baseline).unwrap();
    save_cache(&path, &c, &baseline).unwrap();
    assert_eq!(load_cache(&path, &c, now).unwrap(), Some(baseline.clone()));
    // Missing location prevents any network history query in this test, while
    // retaining a matching private cache tests startup independently of live data.
    let mut offline = c.clone();
    offline.location.latitude = None;
    offline.location.longitude = None;
    offline.home_assistant.token_env = format!("SOLAR_BASELINE_TEST_MISSING_{}", std::process::id());
    save_cache(&path, &offline, &baseline).unwrap();
    let service = SolarService::start_with_baseline_store(offline, Some(path.clone()));
    let snapshot = service.snapshot();
    assert_eq!(snapshot.night_baseline_w, Some(321.0));
    assert!(snapshot.night_baseline_learned);
    assert!(snapshot.reading_at.is_none());
    assert!(!snapshot.evaluation.safe_to_start, "history must never count as a fresh live sample");
    for _ in 0..100 {
        let snapshot = service.snapshot();
        if snapshot.night_baseline_history.as_ref().unwrap().issue.as_deref()
            == Some("history-no-completed-night-or-location")
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let failed = service.snapshot();
    assert_eq!(failed.night_baseline_w, Some(321.0), "failed refresh retains the cached valid baseline");
    assert_eq!(
        failed.night_baseline_history.as_ref().unwrap().issue.as_deref(),
        Some("history-no-completed-night-or-location")
    );
    service.stop();
    let history_status_path = path.with_extension("daily-energy.reported-history-status.json");
    for _ in 0..200 {
        let cancelled = std::fs::read(&history_status_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|status| status["phase"] == "cancelled");
        if cancelled {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    save_cache(&path, &c, &baseline).unwrap();
    for mutation in 0..4 {
        let mut changed = c.clone();
        match mutation {
            0 => changed.home_assistant.atomic_grid = "sensor.different_atomic_source".into(),
            1 => changed.home_assistant.endpoint = "ws://127.0.0.1:8124/api/websocket".into(),
            2 => changed.location.latitude = Some(49.0),
            _ => changed.home_assistant.night_baseline_refresh_at = "07:00".into(),
        }
        assert_eq!(load_cache(&path, &changed, now), Err("history-cache-source-mismatch"));
    }
    let mut invalid = baseline;
    invalid.unit = "kW".into();
    save_cache(&path, &c, &invalid).unwrap();
    assert_eq!(load_cache(&path, &c, now), Err("history-cache-invalid"));
    std::fs::write(&path, vec![b'x'; 8193]).unwrap();
    assert_eq!(load_cache(&path, &c, now), Err("history-cache-invalid"));
    std::fs::remove_file(path).unwrap();
    std::fs::remove_file(history_status_path).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

#[test]
fn empty_history_is_not_a_zero_baseline() {
    let at = time("2026-11-02T12:00:00Z");
    let empty = solar_core::ha_baseline::NightSamples::new(NightWindow { start: at - 3_600_000, end: at });
    assert_eq!(empty.finish(at), Err("history-insufficient-minute-samples"));
}

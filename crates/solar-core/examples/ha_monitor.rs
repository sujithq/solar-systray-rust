//! Opt-in live acceptance; launched privately with an existing process credential.

use serde_json::json;
use solar_core::{SolarConfig, SolarService};
use std::thread;
use std::time::{Duration, Instant};

fn main() {
    if std::env::var("SOLAR_HA_MONITOR_APPROVED").as_deref() != Ok("1") {
        eprintln!("Monitoring requires the private launcher and an existing credential.");
        std::process::exit(2);
    }
    if std::env::var("SOLAR_HA_TOKEN").ok().is_none_or(|s| s.is_empty()) {
        eprintln!("Existing process credential is missing; monitoring not started.");
        std::process::exit(2);
    }
    let config = SolarConfig::from_json(&json!({
        "energySource": {"type": "home-assistant"},
        "homeAssistant": {
            "endpoint": "ws://127.0.0.1:8123/api/websocket",
            "tokenEnv": "SOLAR_HA_TOKEN",
            "atomicGrid": "sensor.dsmr_atomic_grid_sample"
        },
        "decision": {"staleAfterSeconds": 30, "stableDurationSeconds": 300}
    }))
    .expect("fixed monitoring configuration is valid");
    assert!(config.is_ha() && !config.inverter_enabled());
    let service = SolarService::start(config.clone());
    let started = Instant::now();
    let mut readings = 0_u64;
    let mut unchanged = 0_u64;
    let mut missing = 0_u64;
    let mut unsafe_go = 0_u64;
    let mut first_sample_at = None;
    let mut last_sample_at = None;
    let mut previous: Option<(u64, i64, Option<f64>, Option<f64>)> = None;
    let mut go_observed = false;
    let mut initialized = false;
    let mut protocol_errors = 0;
    let mut expired_go = false;
    eprintln!("Monitoring HA-only atomic samples for 310 seconds; no device controls.");
    while started.elapsed() < Duration::from_secs(310) {
        let snapshot = service.snapshot();
        let ha = snapshot.home_assistant.as_ref().expect("HA diagnostics");
        initialized |= ha.initialized;
        protocol_errors = snapshot.diagnostics.errors;
        go_observed |= snapshot.evaluation.safe_to_start;
        if snapshot.evaluation.safe_to_start && snapshot.evaluation.stable_seconds < 300.0 {
            unsafe_go += 1;
        }
        if let Some(at) = snapshot.reading_at {
            let entity = &ha.entities[0];
            if entity.age_ms.is_some_and(|age| age >= 30_000) && snapshot.evaluation.safe_to_start {
                expired_go = true;
            }
            if let Some(sequence) = entity.sample_sequence
                && previous.as_ref().is_none_or(|p| p.0 != sequence || p.1 != at)
            {
                let powers = (snapshot.evaluation.grid_import_w, snapshot.evaluation.surplus_w);
                if previous.as_ref().is_some_and(|p| p.2 == powers.0 && p.3 == powers.1) {
                    unchanged += 1;
                }
                readings += 1;
                first_sample_at.get_or_insert(at);
                last_sample_at = Some(at);
                previous = Some((sequence, at, powers.0, powers.1));
            }
        } else {
            missing += 1;
            if snapshot.evaluation.safe_to_start {
                unsafe_go += 1;
            }
        }
        thread::sleep(Duration::from_millis(200));
    }
    // Stop only this client's worker. Cached readings must expire without any
    // interruption of HA, DSMR acquisition or other clients.
    service.stop();
    eprintln!("Client stopped; checking the unchanged 30-second stale cutoff.");
    thread::sleep(Duration::from_secs(31));
    let stale = service.snapshot();
    let stale_cleared = stale.reading_at.is_none() && !stale.evaluation.safe_to_start;
    drop(service);
    let recovery = SolarService::start(config);
    let reconnect_started = Instant::now();
    let mut recovery_live = false;
    let mut snapshot_go = false;
    let mut recovery_reset = true;
    while reconnect_started.elapsed() < Duration::from_secs(15) {
        let s = recovery.snapshot();
        if s.reading_at.is_none() && s.evaluation.safe_to_start {
            snapshot_go = true;
        }
        if s.reading_at.is_some() {
            recovery_live = true;
            recovery_reset &= !s.evaluation.safe_to_start && s.evaluation.stable_seconds < 300.0;
        }
        thread::sleep(Duration::from_millis(200));
    }
    recovery.stop();
    drop(recovery);
    let span = match (first_sample_at, last_sample_at) {
        (Some(first), Some(last)) => (last - first).max(0) / 1000,
        _ => 0,
    };
    let passed = initialized
        && readings > 1
        && span >= 300
        && unchanged > 0
        && stale_cleared
        && recovery_live
        && recovery_reset
        && !snapshot_go
        && unsafe_go == 0
        && !expired_go
        && protocol_errors == 0;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "monitoring_only": true,
            "ha_only_no_fallback": true,
            "measurement_correctness": "user working assumption; not physical calibration",
            "monitor_seconds": started.elapsed().as_secs().min(310),
            "accepted_observed_samples": readings,
            "observed_meter_span_seconds": span,
            "unchanged_power_new_sequences": unchanged,
            "unavailable_observations": missing,
            "protocol_errors": protocol_errors,
            "go_observed_under_existing_predicates": go_observed,
            "unsafe_go_observations": unsafe_go,
            "client_stop_stale_cleared": stale_cleared,
            "new_client_live_recovered": recovery_live,
            "new_client_stability_reset": recovery_reset,
            "snapshot_go_observed": snapshot_go,
            "acceptance_passed": passed,
            "stale_seconds": 30,
            "go_stability_seconds": 300
        }))
        .expect("sanitized report serializes")
    );
    if !passed {
        std::process::exit(1);
    }
}

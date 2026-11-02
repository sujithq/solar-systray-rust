//! Opt-in live acceptance; launched privately with an existing process credential.

use serde_json::json;
use solar_core::decision::State;
use solar_core::home_assistant::EntityDiagnostic;
use solar_core::{SolarConfig, SolarService};
use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration, Instant};

const AC_ENTITY: &str = "sensor.sg5_0rs_local_total_active_power";
const DC_ENTITY: &str = "sensor.sg5_0rs_local_total_dc_power";

#[derive(Default)]
struct InverterPolls {
    fresh: u64,
    stale: u64,
    unavailable: u64,
    invalid: u64,
    max_state_change_age_ms: Option<i64>,
}

impl InverterPolls {
    fn observe(&mut self, entity: Option<&EntityDiagnostic>) {
        let Some(entity) = entity else { return };
        if let Some(age) = entity.age_ms {
            self.max_state_change_age_ms = Some(self.max_state_change_age_ms.unwrap_or(0).max(age));
        }
        match entity.issue.as_deref() {
            None if entity.age_ms.is_some_and(|age| (0..=90_000).contains(&age)) => self.fresh += 1,
            Some("ha-entity-stale") => self.stale += 1,
            Some("ha-entity-unavailable" | "ha-disconnected") | None => self.unavailable += 1,
            Some(_) => self.invalid += 1,
        }
    }

    fn report(&self, mapped: bool) -> serde_json::Value {
        json!({
            "mapped": mapped,
            "fresh_state_change_polls": self.fresh,
            "stale_polls": self.stale,
            "unavailable_polls": self.unavailable,
            "invalid_polls": self.invalid,
            "maximum_last_known_state_change_age_seconds": self.max_state_change_age_ms.map(|ms| ms as f64 / 1000.0)
        })
    }
}

#[derive(Default)]
struct Continuity {
    since: Option<Duration>,
    previous: Option<Duration>,
    longest: Duration,
}

impl Continuity {
    fn observe(&mut self, now: Duration, eligible: bool) {
        if self.previous.is_some_and(|previous| now.saturating_sub(previous) > Duration::from_millis(500)) {
            self.since = None;
        }
        self.previous = Some(now);
        if eligible {
            let since = *self.since.get_or_insert(now);
            self.longest = self.longest.max(now.saturating_sub(since));
        } else {
            self.since = None;
        }
    }
}

fn client_code(code: Option<&str>) -> &'static str {
    match code {
        Some("ha-atomic-time") => "ha-atomic-time",
        Some("ha-atomic-schema") => "ha-atomic-schema",
        Some("ha-atomic-power") => "ha-atomic-power",
        Some("ha-atomic-replay") => "ha-atomic-replay",
        Some("ha-atomic-retired-session") => "ha-atomic-retired-session",
        Some("ha-atomic-session-limit") => "ha-atomic-session-limit",
        Some("ha-atomic-unavailable") => "ha-atomic-unavailable",
        Some("ha-atomic-stale") => "ha-atomic-stale",
        Some("ha-atomic-future") => "ha-atomic-future",
        Some("ha-awaiting-new-atomic-sample") => "ha-awaiting-new-atomic-sample",
        Some("ha-awaiting-initialization") => "ha-awaiting-initialization",
        Some("ha-response-timeout") => "ha-response-timeout",
        Some("ha-read-or-protocol") => "ha-read-or-protocol",
        Some("ha-connect") => "ha-connect",
        Some("ha-upgrade-or-tls") => "ha-upgrade-or-tls",
        Some("ha-auth-rejected") => "ha-auth-rejected",
        Some("ha-token-missing") => "ha-token-missing",
        Some("ha-token-invalid") => "ha-token-invalid",
        Some("ha-closed") => "ha-closed",
        Some("ha-json") => "ha-json",
        Some("ha-unexpected-response") => "ha-unexpected-response",
        Some("ha-heartbeat-rejected") => "ha-heartbeat-rejected",
        Some("ha-command-id-exhausted") => "ha-command-id-exhausted",
        Some("ha-subscription-rejected") => "ha-subscription-rejected",
        Some("ha-snapshot-rejected") => "ha-snapshot-rejected",
        Some("ha-snapshot-shape") => "ha-snapshot-shape",
        Some("ha-event-shape") => "ha-event-shape",
        Some("ha-event-entity") => "ha-event-entity",
        Some("ha-initialization-overflow") => "ha-initialization-overflow",
        Some("ha-write") => "ha-write",
        Some("ha-message-type") => "ha-message-type",
        Some("ha-timeout") => "ha-timeout",
        Some("ha-transport") => "ha-transport",
        Some("ha-disconnected") => "ha-disconnected",
        None => "no-current-issue",
        _ => "unclassified-client-code",
    }
}

fn predicate(snapshot: &solar_core::Snapshot) -> &'static str {
    let e = &snapshot.evaluation;
    match e.state {
        State::Offline => "unavailable",
        State::Importing => "grid-import",
        State::Idle => "no-meaningful-surplus",
        State::Limited if e.surplus_w.is_some_and(|w| w > e.start_threshold_w) => "waiting-stability",
        State::Limited => "insufficient-surplus",
        State::Good | State::Excellent if e.safe_to_start => "go",
        _ => "other-existing-predicate",
    }
}

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
            "atomicGrid": "sensor.dsmr_atomic_grid_sample",
            "activePower": AC_ENTITY,
            "dcPower": DC_ENTITY,
            "inverterStaleSeconds": 90
        },
        "decision": {"staleAfterSeconds": 30, "stableDurationSeconds": 300}
    }))
    .expect("fixed monitoring configuration is valid");
    assert!(config.is_ha() && !config.inverter_enabled());
    let ac_mapped = !config.home_assistant.active_power.is_empty();
    let dc_mapped = !config.home_assistant.dc_power.is_empty();
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
    let mut fresh_continuity = Continuity::default();
    let mut candidate_continuity = Continuity::default();
    let mut issue_observations = BTreeMap::<&str, u64>::new();
    let mut error_deltas = BTreeMap::<&str, u64>::new();
    let mut predicates = BTreeMap::<&str, u64>::new();
    let mut reconnects = 0;
    let mut max_stable_seconds = 0.0_f64;
    let mut ac_polls = InverterPolls::default();
    let mut dc_polls = InverterPolls::default();
    eprintln!(
        "Monitoring HA-only atomic grid plus optional AC/DC state-change ages for 310 seconds; no device controls."
    );
    while started.elapsed() < Duration::from_secs(310) {
        let snapshot = service.snapshot();
        let ha = snapshot.home_assistant.as_ref().expect("HA diagnostics");
        ac_polls.observe(ha.entities.iter().find(|e| e.entity == AC_ENTITY));
        dc_polls.observe(ha.entities.iter().find(|e| e.entity == DC_ENTITY));
        initialized |= ha.initialized;
        let code = client_code(snapshot.diagnostics.last_issue.as_deref());
        *issue_observations.entry(code).or_default() += 1;
        let delta = snapshot.diagnostics.errors.saturating_sub(protocol_errors);
        if delta > 0 {
            *error_deltas.entry(code).or_default() += delta;
        }
        protocol_errors = snapshot.diagnostics.errors;
        reconnects = snapshot.diagnostics.reconnects;
        let category = predicate(&snapshot);
        *predicates.entry(category).or_default() += 1;
        let fresh = snapshot.reading_at.is_some()
            && ha
                .entities
                .first()
                .is_some_and(|e| e.issue.is_none() && e.age_ms.is_some_and(|age| (0..30_000).contains(&age)));
        fresh_continuity.observe(started.elapsed(), fresh);
        candidate_continuity.observe(started.elapsed(), fresh && matches!(category, "waiting-stability" | "go"));
        max_stable_seconds = max_stable_seconds.max(snapshot.evaluation.stable_seconds);
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
        && fresh_continuity.longest >= Duration::from_secs(300)
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
            "client_errors": protocol_errors,
            "reconnect_count": reconnects,
            "issue_poll_observations": issue_observations,
            "error_deltas_by_latest_observed_code": error_deltas,
            "error_attribution": "poll-sampled latest client code; intermediate errors can be coalesced or superseded",
            "optional_inverter_freshness_basis": "last-known HA state-change time plus monotonic receive age; no acquisition timestamp; cached snapshots, last_reported and heartbeats do not renew age",
            "optional_inverter_ac_observations": ac_polls.report(ac_mapped),
            "optional_inverter_dc_observations": dc_polls.report(dc_mapped),
            "decision_predicate_poll_observations": predicates,
            "longest_observed_continuous_fresh_seconds": fresh_continuity.longest.as_secs_f64(),
            "longest_observed_continuous_candidate_seconds": candidate_continuity.longest.as_secs_f64(),
            "maximum_engine_stable_seconds": max_stable_seconds,
            "continuity_evidence": "200ms polls; gaps over500ms reset continuity; not proof of unsampled transitions",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuity_requires_unbroken_eligible_polls_and_bounds_observation_gaps() {
        let mut c = Continuity::default();
        for ms in [0, 200, 400] {
            c.observe(Duration::from_millis(ms), true);
        }
        assert_eq!(c.longest, Duration::from_millis(400));
        c.observe(Duration::from_millis(600), false);
        c.observe(Duration::from_millis(800), true);
        c.observe(Duration::from_millis(1000), true);
        assert_eq!(c.longest, Duration::from_millis(400));
        c.observe(Duration::from_millis(2000), true);
        c.observe(Duration::from_millis(2200), true);
        assert_eq!(c.longest, Duration::from_millis(400));
    }

    #[test]
    fn diagnostic_codes_never_echo_unknown_text() {
        assert_eq!(client_code(Some("ha-atomic-time")), "ha-atomic-time");
        assert_eq!(client_code(Some("private-token-or-server-message")), "unclassified-client-code");
        assert_eq!(client_code(None), "no-current-issue");
    }

    #[test]
    fn optional_inverter_observations_are_aggregated_without_power_or_entity_values() {
        let mut polls = InverterPolls::default();
        polls.observe(Some(&EntityDiagnostic {
            entity: "private-entity-id".into(),
            ha_last_changed: None,
            ha_last_updated: None,
            ha_last_reported: None,
            received_age_ms: None,
            evidence_at: None,
            age_ms: Some(12_000),
            issue: None,
            sample_sequence: None,
            meter_timestamp: None,
            publisher_received_at: None,
        }));
        polls.observe(Some(&EntityDiagnostic {
            entity: "private-entity-id".into(),
            ha_last_changed: None,
            ha_last_updated: None,
            ha_last_reported: None,
            received_age_ms: None,
            evidence_at: None,
            age_ms: Some(91_000),
            issue: Some("ha-entity-stale".into()),
            sample_sequence: None,
            meter_timestamp: None,
            publisher_received_at: None,
        }));
        let report = polls.report(true).to_string();
        assert!(report.contains("\"fresh_state_change_polls\":1"));
        assert!(report.contains("\"stale_polls\":1"));
        assert!(report.contains("\"maximum_last_known_state_change_age_seconds\":91.0"));
        assert!(!report.contains("private-entity-id"));
    }
}

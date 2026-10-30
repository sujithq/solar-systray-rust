//! Port of solar-agent `daemon/test/solar-info.test.ts`, plus the tray tooltip and runtime wiring.

use solar_core::SolarConfig;
use solar_core::daily_energy::DailyEnergyResult;
use solar_core::decision::{Level, State};
use solar_core::mock::MOCK_SCENARIOS;
use solar_core::peak_forecast::Forecast;
use solar_core::present::{
    Info, TOOLTIP_MAX, Tone, countdown, fit_tooltip, headline, kilowatt_hours, solar_info, to_fixed, tooltip,
};
use solar_core::runtime::{Runtime, Snapshot};
use solar_core::sun::Sun;
use solar_core::sungrow::{InverterSnapshot, InverterStatus};

const NOW: i64 = 1_800_000_000_000;

fn snapshot(change: impl FnOnce(&mut Snapshot)) -> Snapshot {
    let mut s = Runtime::new(SolarConfig::default()).snapshot(NOW);
    s.simulated = false;
    s.scenario = None;
    s.meter_address = Some("192.0.2.10:23".into());
    s.diagnostics.connected = true;
    let e = &mut s.evaluation;
    e.state = State::Good;
    e.reason = "".into();
    e.level = Level::Surplus;
    e.sun = Sun::Up;
    e.start_threshold_w = 1950.0;
    e.safe_to_start = true;
    e.surplus_w = Some(2264.0);
    e.grid_import_w = Some(0.0);
    e.stable_seconds = 60.0;
    e.appliance_name = "Dishwasher".into();
    e.appliance_peak_w = 1500.0;
    e.current_demand_w = Some(0.0);
    e.monthly_peak_w = Some(3600.0);
    e.monthly_peak_reached = false;
    s.stable_duration_seconds = 60.0;
    s.location_configured = true;
    s.sunrise = None;
    s.sunset = None;
    s.peak_forecast = Forecast {
        available: true,
        reason: "ok".into(),
        projected_w: Some(0.0),
        comparison_w: Some(3600.0),
        seconds_left: Some(340.0),
        ..Forecast::unavailable("too-early")
    };
    s.daily_energy = DailyEnergyResult {
        available: true,
        import_wh: Some(0),
        export_wh: Some(1326),
        since_minute: Some(842),
        reason: "available".into(),
    };
    s.inverter = InverterSnapshot {
        status: InverterStatus::Online,
        pv_power_w: Some(3120.0),
        active_power_w: Some(2980.0),
        household_use_w: Some(716.0),
        timestamp: Some(1),
        issue: None,
    };
    s.night_baseline_w = Some(350.0);
    s.night_baseline_learned = true;
    s.night_baseline_samples = 0;
    change(&mut s);
    s
}

fn rows(info: &Info, title: &str) -> Vec<(String, String)> {
    info.section(title)
        .unwrap_or_else(|| panic!("no {title}"))
        .rows
        .iter()
        .map(|r| (r.label.clone(), r.value.clone()))
        .collect()
}

fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected.iter().map(|(l, v)| (l.to_string(), v.to_string())).collect()
}

fn value(info: &Info, title: &str, label: &str) -> String {
    info.section(title).and_then(|s| s.value(label)).unwrap_or_else(|| panic!("no {title}/{label}")).to_string()
}

#[test]
fn info_shows_the_device_screen_as_readable_sections() {
    let info = solar_info(&snapshot(|_| {}));
    assert_eq!((info.headline.title.as_str(), info.headline.reason.as_str()), ("GO - START NOW", "Surplus is stable"));
    assert_eq!(info.headline.tone, Tone::Good);
    assert_eq!(
        rows(&info, "Power now"),
        pairs(&[
            ("Grid export", "2264 W"),
            ("Grid import", "0 W"),
            ("Solar panels (PV)", "3.1 kW"),
            ("Inverter output (AC)", "3.0 kW"),
            ("Household use", "0.7 kW"),
            ("Extra vs night", "+366 W"),
            ("Night baseline", "350 W (learned)"),
        ])
    );
    let extra = info.sections[0].rows.iter().find(|r| r.label == "Extra vs night").unwrap();
    assert_eq!(extra.tone, Tone::Warn);
    assert_eq!(rows(&info, "Appliance"), pairs(&[("Dishwasher", "1500 W"), ("Starts at", "1950 W export")]));
    assert_eq!(
        rows(&info, "Quarter-hour peak"),
        pairs(&[
            ("Quarter-hour demand", "0.0 kW"),
            ("Monthly peak", "3.6 kW"),
            ("Forecast", "0.0 kW / peak 3.6 kW"),
            ("Quarter ends in", "5:40"),
        ])
    );
    assert_eq!(
        rows(&info, "Today"),
        pairs(&[("Imported", "0.000 kWh"), ("Exported", "1.326 kWh"), ("Since", "14:02 (meter time)")])
    );
    assert_eq!(value(&info, "Connections", "Meter"), "192.0.2.10:23 (live)");
    assert!(!info.simulated);
}

#[test]
fn headline_follows_the_device_decision_order() {
    let title = |change: fn(&mut Snapshot)| headline(&snapshot(change));
    assert_eq!(
        title(|s| {
            s.evaluation.state = State::Offline;
            s.evaluation.surplus_w = None;
        })
        .title,
        "METER OFFLINE"
    );
    assert_eq!(
        title(|s| {
            s.evaluation.safe_to_start = false;
            s.evaluation.level = Level::Night;
            s.evaluation.state = State::Idle;
        })
        .title,
        "NIGHT"
    );
    let importing = title(|s| {
        s.evaluation.safe_to_start = false;
        s.evaluation.state = State::Importing;
        s.evaluation.grid_import_w = Some(820.0);
    });
    assert_eq!(
        (importing.title.as_str(), importing.reason.as_str(), importing.tone),
        ("IMPORTING", "Importing 820 W from grid", Tone::Bad)
    );
    let soon = title(|s| {
        s.evaluation.safe_to_start = false;
        s.evaluation.state = State::Limited;
        s.evaluation.stable_seconds = 15.0;
    });
    assert_eq!(
        (soon.title.as_str(), soon.reason.as_str(), soon.tone),
        ("GO IN 0:45", "Stable for 0:15 of 1:00", Tone::Warn)
    );
    let wait = title(|s| {
        s.evaluation.safe_to_start = false;
        s.evaluation.state = State::Limited;
        s.evaluation.surplus_w = Some(900.0);
    });
    assert_eq!(
        (wait.title.as_str(), wait.reason.as_str(), wait.tone),
        ("WAIT FOR SURPLUS", "Need 1950 W export, have 900 W", Tone::Warn)
    );
    let idle = title(|s| {
        s.evaluation.safe_to_start = false;
        s.evaluation.state = State::Idle;
        s.evaluation.surplus_w = Some(0.0);
    });
    assert_eq!(idle.title, "NO SURPLUS");
}

#[test]
fn info_explains_unavailable_totals_forecast_warnings_and_offline_readings() {
    let unavailable = solar_info(&snapshot(|s| s.daily_energy = DailyEnergyResult::unavailable("missing-meter-time")));
    assert_eq!(
        rows(&unavailable, "Today"),
        pairs(&[("Daily energy", "Unavailable"), ("Why", "the meter sends no clock")])
    );

    let warning = solar_info(&snapshot(|s| {
        s.peak_forecast = Forecast {
            available: true,
            reason: "ok".into(),
            projected_w: Some(4200.0),
            comparison_w: Some(3600.0),
            seconds_left: Some(125.0),
            reduce_w: Some(800.0),
            warning: true,
            ..Forecast::unavailable("too-early")
        }
    }));
    assert_eq!(value(&warning, "Quarter-hour peak", "Forecast"), "4.2 kW / peak 3.6 kW");
    assert_eq!(value(&warning, "Quarter-hour peak", "Reduce import"), "0.8 kW, 2:05 left");

    let offline = solar_info(&snapshot(|s| {
        s.evaluation.state = State::Offline;
        s.evaluation.surplus_w = None;
        s.inverter =
            InverterSnapshot::without_reading(InverterStatus::Offline, Some("Sungrow response timed out".into()));
        s.diagnostics.connected = false;
        s.diagnostics.last_issue = Some("ECONNREFUSED".into());
    }));
    assert_eq!(value(&offline, "Power now", "Grid export"), "-- W");
    assert_eq!(value(&offline, "Power now", "Solar panels (PV)"), "Sungrow unavailable");
    assert_eq!(value(&offline, "Quarter-hour peak", "Peak"), "No meter reading");
    assert_eq!(value(&offline, "Today", "Daily energy"), "Unavailable");
    assert_eq!(value(&offline, "Connections", "Meter"), "192.0.2.10:23 (offline: ECONNREFUSED)");
}

#[test]
fn info_labels_the_fallback_baseline_while_learning() {
    let first_run = solar_info(&snapshot(|s| {
        s.night_baseline_w = Some(200.0);
        s.night_baseline_learned = false;
        s.evaluation.sun = Sun::Down;
    }));
    assert_eq!(value(&first_run, "Power now", "Night baseline"), "200 W default; learning overnight");
    assert_eq!(value(&first_run, "Power now", "Extra vs night"), "+516 W");
}

#[test]
fn info_marks_simulated_data() {
    let simulated = solar_info(&snapshot(|s| {
        s.simulated = true;
        s.meter_address = None;
    }));
    assert!(simulated.simulated);
    assert_eq!(value(&simulated, "Connections", "Meter"), "Simulated");
    assert_eq!(kilowatt_hours(12_345), "12.345 kWh");
    assert_eq!(countdown(0.0), "0:00");
}

#[test]
fn to_fixed_rounds_exact_ties_away_from_zero_like_javascript() {
    assert_eq!(to_fixed(0.25, 1), "0.3");
    assert_eq!(to_fixed(2.5, 0), "3");
    assert_eq!(to_fixed(-2.5, 0), "-3");
    assert_eq!(to_fixed(1.005, 2), "1.00");
    assert_eq!(to_fixed(3.14159, 3), "3.142");
}

#[test]
fn tooltip_is_compact_and_fits_the_shell_limit() {
    let text = tooltip(&snapshot(|_| {}));
    assert!(text.starts_with("GO - START NOW\nExport 2264 W | PV 3.1 kW\n"), "{text}");
    assert!(text.encode_utf16().count() <= TOOLTIP_MAX, "{text}");
    let offline = tooltip(&snapshot(|s| {
        s.evaluation.state = State::Offline;
        s.evaluation.surplus_w = None;
        s.diagnostics.last_issue = Some("ECONNREFUSED".into());
    }));
    assert_eq!(offline, "METER OFFLINE\nNo recent meter reading\nLast issue: ECONNREFUSED");
    let long = fit_tooltip(vec!["x".repeat(100), "y".repeat(100)]);
    assert_eq!(long, "x".repeat(100));
    let huge = fit_tooltip(vec!["z".repeat(300)]);
    assert_eq!(huge.encode_utf16().count(), TOOLTIP_MAX);
    assert!(huge.ends_with("..."));
}

#[test]
fn simulation_runs_every_scenario_through_the_runtime() {
    let mut runtime = Runtime::new(SolarConfig::default());
    let mut now = NOW;
    for (name, _) in MOCK_SCENARIOS {
        runtime.set_scenario(name, now).unwrap();
        for _ in 0..5 {
            now += 1000;
            runtime.mock_tick(now);
        }
        let s = runtime.snapshot(now);
        assert!(s.simulated, "{name}");
        assert_eq!(s.scenario.as_deref(), Some(*name));
        assert!(s.meter_address.is_none());
        let text = tooltip(&s);
        assert!(text.encode_utf16().count() <= TOOLTIP_MAX, "{name}: {text}");
        assert!(text.contains("[SIM]"), "{name}: {text}");
        solar_info(&s);
    }
    assert!(runtime.set_scenario("nope", now).is_err());
}

#[test]
fn simulated_excellent_surplus_reaches_go_after_the_stable_window() {
    let mut runtime = Runtime::new(SolarConfig::default());
    runtime.set_scenario("excellent", NOW).unwrap();
    let mut s = runtime.snapshot(NOW);
    for second in 0..=300 {
        runtime.mock_tick(NOW + second * 1000);
        s = runtime.snapshot(NOW + second * 1000);
        if second < 300 {
            assert!(!s.evaluation.safe_to_start, "GO too early at {second}s");
        }
    }
    assert!(s.evaluation.safe_to_start);
    assert_eq!(headline(&s).title, "GO - START NOW");
    runtime.set_simulated_watts(-500, NOW + 301_000).unwrap();
    let importing = runtime.snapshot(NOW + 301_000);
    assert_eq!(importing.evaluation.state, State::Importing);
    assert_eq!(importing.simulated_w, Some(-500));
}

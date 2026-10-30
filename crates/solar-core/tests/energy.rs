//! Port of solar-agent `solar/test/energy.test.mjs`, against the shared fixtures.

use serde_json::{Value, json};
use solar_core::config::SolarConfig;
use solar_core::daily_energy::DailyEnergy;
use solar_core::decision::{DecisionEngine, Level, State, import_gauge_scale};
use solar_core::dsmr::{DsmrStream, ParseOptions, StreamEvent, crc16, parse_telegram, synthetic_telegram};
use solar_core::reading::{EnergyTotals, Quality, Reading};
use solar_core::sun::{Location, Sun, sun_elevation, sun_state, sun_times};
use solar_core::utc_ms;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn json_lines(name: &str) -> Vec<Value> {
    fixture(name).lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn reading(timestamp: i64, export_w: f64) -> Reading {
    Reading::new(timestamp, 0.0, export_w, "test")
}

fn with_import(mut r: Reading, import_w: f64) -> Reading {
    r.grid_import_w = import_w;
    r
}

fn config(input: Value) -> SolarConfig {
    SolarConfig::from_json(&input).unwrap()
}

fn fast_config() -> SolarConfig {
    config(json!({"decision": {"stableDurationSeconds": 5, "staleAfterSeconds": 3}}))
}

fn checksum(body: &str) -> String {
    format!("{body}{:04x}\r\n", crc16(body.as_bytes()))
}

fn parse(text: &str) -> Result<Reading, &'static str> {
    parse_telegram(text, &ParseOptions::default()).map_err(|e| e.0)
}

fn parse_with(text: &str, allow_legacy: bool) -> Result<Reading, &'static str> {
    let options = ParseOptions { allow_legacy_without_crc: allow_legacy, ..ParseOptions::default() };
    parse_telegram(text, &options).map_err(|e| e.0)
}

fn parse_date(iso: &str) -> i64 {
    // "YYYY-MM-DDTHH:MM:SSZ"
    let n = |range: std::ops::Range<usize>| iso[range].parse::<u32>().unwrap();
    utc_ms(n(0..4) as i64, n(5..7), n(8..10), n(11..13), n(14..16), n(17..19))
}

#[test]
fn crc_matches_the_crc16_arc_check_vector() {
    assert_eq!(crc16(b"123456789"), 0xBB3D);
}

#[test]
fn shared_decision_vectors() {
    let vectors: Value = serde_json::from_str(&fixture("decision-vectors.json")).unwrap();
    let mut engine = DecisionEngine::new(config(json!({"decision": vectors["decision"]})));
    for sample in vectors["samples"].as_array().unwrap() {
        let time = sample["time"].as_i64().unwrap();
        let value = (!sample["offline"].as_bool().unwrap_or(false)).then(|| {
            with_import(reading(time, sample["exportW"].as_f64().unwrap()), sample["importW"].as_f64().unwrap())
        });
        let sun = match sample["sun"].as_str().unwrap_or("up") {
            "down" => Sun::Down,
            "unknown" => Sun::Unknown,
            _ => Sun::Up,
        };
        let result = engine.evaluate(value.as_ref(), time, sun);
        assert_eq!(result.state.as_str(), sample["state"].as_str().unwrap(), "state at {time}ms");
        assert_eq!(result.safe_to_start, sample["go"].as_bool().unwrap(), "GO at {time}ms");
        assert_eq!(result.level.as_str(), sample["level"].as_str().unwrap(), "level at {time}ms");
    }
}

#[test]
fn sun_elevation_matches_shared_vectors() {
    let vectors: Value = serde_json::from_str(&fixture("sun-vectors.json")).unwrap();
    for sample in vectors["samples"].as_array().unwrap() {
        let label = sample["label"].as_str().unwrap();
        let time = sample["time"].as_i64().unwrap();
        let (lat, lon) = (sample["latitude"].as_f64().unwrap(), sample["longitude"].as_f64().unwrap());
        let elevation = sun_elevation(time as f64, lat, lon);
        assert!((elevation - sample["elevation"].as_f64().unwrap()).abs() < 0.01, "{label}: {elevation}");
        assert_eq!(
            sun_state(&Location::new(lat, lon), time).state.as_str(),
            sample["sun"].as_str().unwrap(),
            "{label}"
        );
    }
    assert_eq!(sun_state(&Location::default(), 0).state, Sun::Unknown);
}

#[test]
fn next_sunrise_and_sunset_match_published_brussels_times() {
    let brussels = Location::new(50.85, 4.35);
    let near = |actual: Option<i64>, expected: &str, label: &str| {
        let actual = actual.unwrap_or_else(|| panic!("{label}: missing"));
        assert!((actual - parse_date(expected)).abs() <= 2 * 60_000, "{label}: {actual}");
    };
    let summer = sun_times(&brussels, parse_date("2024-06-21T00:00:00Z"));
    near(summer.sunrise, "2024-06-21T03:29:00Z", "summer sunrise");
    near(summer.sunset, "2024-06-21T20:00:00Z", "summer sunset");
    let winter = sun_times(&brussels, parse_date("2024-12-21T12:00:00Z"));
    near(winter.sunset, "2024-12-21T15:40:00Z", "winter sunset today");
    near(winter.sunrise, "2024-12-22T07:44:00Z", "winter sunrise tomorrow");
    let polar = sun_times(&Location::new(69.65, 18.96), parse_date("2024-06-21T00:00:00Z"));
    assert_eq!((polar.sunrise, polar.sunset), (None, None), "polar day");
    let none = sun_times(&Location::default(), 0);
    assert_eq!((none.sunrise, none.sunset), (None, None));
}

#[test]
fn each_solar_level_maps_to_its_character_track() {
    let mut engine = DecisionEngine::new(config(json!({"decision": {"stableDurationSeconds": 0}})));
    let cases = [
        (Some(reading(0, 0.0)), Sun::Down, Level::Night, 5),
        (Some(with_import(reading(1, 0.0), 300.0)), Sun::Up, Level::Importing, 4),
        (Some(reading(2, 50.0)), Sun::Up, Level::Producing, 0),
        (Some(reading(3, 2000.0)), Sun::Up, Level::Surplus, 2),
        (Some(reading(4, 5000.0)), Sun::Up, Level::MaxSurplus, 3),
        (None, Sun::Up, Level::Offline, 1),
    ];
    for (value, sun, level, mode) in cases {
        let now = value.as_ref().map_or(5, |r| r.timestamp);
        let result = engine.evaluate(value.as_ref(), now, sun);
        assert_eq!(result.level, level);
        assert_eq!(result.character_mode, mode);
    }
}

#[test]
fn configured_location_decides_night_from_the_clock() {
    let mut engine = DecisionEngine::new(config(json!({"location": {"latitude": 50.85, "longitude": 4.35}})));
    let night = parse_date("2024-12-21T23:00:00Z");
    let day = parse_date("2024-12-21T12:00:00Z");
    assert_eq!(engine.evaluate_at(Some(&reading(night, 0.0)), night).level, Level::Night);
    let result = engine.evaluate_at(Some(&with_import(reading(day, 0.0), 200.0)), day);
    assert_eq!(result.level, Level::Importing);
    assert_eq!(result.sun, Sun::Up);
    assert!(result.sun_elevation.unwrap() > 10.0);
}

#[test]
fn extracts_synthetic_import_export_with_crc_and_version() {
    let options = ParseOptions { timestamp: 1000, ..ParseOptions::default() };
    let value = parse_telegram(&synthetic_telegram(350.0, 2450.0, ""), &options).unwrap();
    assert_eq!(value.grid_import_w, 350.0);
    assert_eq!(value.grid_export_w, 2450.0);
    assert_eq!(value.timestamp, 1000);
    assert_eq!(value.dsmr_version.as_deref(), Some("5.0"));
    assert_eq!(value.checksum, Some("verified"));
    assert_eq!(value.current_demand_w, None);
    assert_eq!(value.monthly_peak_w, None);
}

#[test]
fn parses_quarter_hour_demand_and_monthly_peak() {
    let fields =
        "1-0:1.7.0(00.500*kW)\r\n1-0:2.7.0(00.000*kW)\r\n1-0:1.4.0(03.150*kW)\r\n1-0:1.6.0(260612183000S)(03.150*kW)";
    let mut value = parse(&checksum(&format!("/SYNTHETIC\r\n{fields}\r\n!"))).unwrap();
    assert_eq!(value.current_demand_w, Some(3150.0));
    assert_eq!(value.monthly_peak_w, Some(3150.0));
    value.timestamp = 1000;
    value.source = "test".into();
    let evaluate = |r: &Reading, now| DecisionEngine::new(SolarConfig::default()).evaluate(Some(r), now, Sun::Up);
    assert!(evaluate(&value, 1000).monthly_peak_reached);
    let lower = Reading { current_demand_w: Some(3149.0), ..value.clone() };
    assert!(!evaluate(&lower, 1000).monthly_peak_reached);
    let zero = Reading { monthly_peak_w: Some(0.0), current_demand_w: Some(0.0), ..value.clone() };
    assert!(!evaluate(&zero, 1000).monthly_peak_reached);
    assert!(!evaluate(&value, 100_000).monthly_peak_reached, "stale demand does not warn");
}

#[test]
fn import_gauge_peak_follows_a_higher_fresh_meter_peak() {
    let scale = |peak: Option<f64>, now: i64| {
        let mut r = with_import(reading(1000, 0.0), 3000.0);
        r.monthly_peak_w = peak;
        let result = DecisionEngine::new(SolarConfig::default()).evaluate(Some(&r), now, Sun::Up);
        (result.gauge_import_peak_w, result.gauge_import_max_w)
    };
    assert_eq!(scale(Some(3150.0), 1000), (3150.0, 11000.0));
    assert_eq!(scale(Some(1800.0), 1000), (2500.0, 11000.0));
    assert_eq!(scale(None, 1000), (2500.0, 11000.0));
    assert_eq!(scale(Some(12000.0), 1000), (12000.0, 24000.0));
    assert_eq!(scale(Some(3150.0), 100_000), (2500.0, 11000.0), "stale peaks are ignored");
    let offline = DecisionEngine::new(SolarConfig::default()).evaluate(None, 1000, Sun::Up);
    assert_eq!((offline.gauge_import_peak_w, offline.gauge_import_max_w), (2500.0, 11000.0));
    let big = import_gauge_scale(2500.0, 11000.0, Some(50000.0));
    assert_eq!((big.peak_w, big.max_w), (50000.0, 86400.0));
}

#[test]
fn fluvius_five_digit_version_is_decoded() {
    let text = checksum("/SYNTHETIC\r\n0-0:96.1.4(50217)\r\n1-0:1.7.0(0*kW)\r\n1-0:2.7.0(2*kW)\r\n!");
    let value = parse(&text).unwrap();
    assert_eq!(value.p1_version.as_deref(), Some("50217"));
    assert_eq!(value.dsmr_version.as_deref(), Some("5.0.2"));
    let bad = checksum("/SYNTHETIC\r\n0-0:96.1.4(5021)\r\n1-0:1.7.0(0*kW)\r\n1-0:2.7.0(2*kW)\r\n!");
    assert_eq!(parse(&bad).unwrap_err(), "version-format");
    // The JS test edits the text without fixing the CRC; the version check comes first.
    assert_eq!(parse(&text.replace("(50217)", "(5021)")).unwrap_err(), "version-format");
}

#[test]
fn cumulative_energy_registers_match_shared_vectors() {
    for sample in json_lines("energy-register-vectors.json") {
        let label = sample["label"].as_str().unwrap();
        let s = |k: &str| sample[k].as_str().unwrap().to_string();
        let body = format!(
            "/X\r\n0-0:1.0.0(260614120000S)\r\n1-0:1.7.0(0*kW)\r\n1-0:2.7.0(0*kW)\r\n1-0:1.8.1({})\r\n1-0:1.8.2({})\r\n1-0:2.8.1({})\r\n1-0:2.8.2({})\r\n!",
            s("i1"),
            s("i2"),
            s("e1"),
            s("e2")
        );
        let value = parse(&checksum(&body)).unwrap();
        if !sample["valid"].as_bool().unwrap() {
            assert_eq!(value.energy_totals_wh, None, "{label}");
            continue;
        }
        let n = |k: &str| sample[k].as_i64().unwrap();
        let expected = EnergyTotals {
            import_tariff1_wh: n("i1Wh"),
            import_tariff2_wh: n("i2Wh"),
            export_tariff1_wh: n("e1Wh"),
            export_tariff2_wh: n("e2Wh"),
        };
        assert_eq!(value.energy_totals_wh, Some(expected), "{label}");
    }
    let missing = parse(&checksum(
        "/X\r\n1-0:1.7.0(0*kW)\r\n1-0:2.7.0(0*kW)\r\n1-0:1.8.1(000001.000*kWh)\r\n1-0:1.8.2(000001.000*kWh)\r\n1-0:2.8.1(000001.000*kWh)\r\n!",
    ))
    .unwrap();
    assert_eq!(missing.energy_totals_wh, None);
}

#[test]
fn meter_clock_exposes_local_day_minute_and_offset() {
    let value = parse(&checksum("/X\r\n0-0:1.0.0(260614120000S)\r\n1-0:1.7.0(0*kW)\r\n1-0:2.7.0(0*kW)\r\n!")).unwrap();
    assert_eq!(value.meter_day, Some(utc_ms(2026, 6, 14, 0, 0, 0) / 86_400_000));
    assert_eq!(value.meter_minute_of_day, Some(720));
    assert_eq!(value.meter_offset_minutes, Some(120));
}

#[test]
fn daily_energy_follows_shared_vectors() {
    let mut current_case = String::new();
    let mut totals = DailyEnergy::new();
    for sample in json_lines("daily-energy-vectors.json") {
        let case = sample["case"].as_str().unwrap().to_string();
        if case != current_case {
            current_case = case.clone();
            totals = DailyEnergy::new();
        }
        let clock = sample["clock"].as_bool() != Some(false);
        let has_totals = sample["totals"].as_bool() != Some(false);
        let fresh = sample["fresh"].as_bool() != Some(false);
        let mut r = Reading::new(0, 0.0, 0.0, "test");
        if clock {
            r.meter_day = sample["day"].as_i64();
            r.meter_minute_of_day = sample["minute"].as_i64();
            r.meter_offset_minutes = sample["offset"].as_i64();
            r.meter_time = sample["meterTime"].as_i64();
        }
        if has_totals {
            r.energy_totals_wh = Some(EnergyTotals {
                import_tariff1_wh: sample["i1"].as_i64().unwrap(),
                import_tariff2_wh: sample["i2"].as_i64().unwrap(),
                export_tariff1_wh: sample["e1"].as_i64().unwrap(),
                export_tariff2_wh: sample["e2"].as_i64().unwrap(),
            });
        }
        let result = totals.update(Some(&r), fresh);
        assert_eq!(result.available, sample["available"].as_bool().unwrap(), "{case} availability");
        assert_eq!(result.reason, sample["reason"].as_str().unwrap(), "{case} reason");
        if result.available {
            assert_eq!(result.import_wh, sample["importWh"].as_i64(), "{case} import");
            assert_eq!(result.export_wh, sample["exportWh"].as_i64(), "{case} export");
            assert_eq!(result.since_minute, sample["since"].as_i64(), "{case} baseline");
        }
    }
}

#[test]
fn parses_power_units_and_versions() {
    for (label, fields, expected) in [
        ("integer watts", "1-0:1.7.0(0*W)\r\n1-0:2.7.0(2450*W)", 2450.0),
        ("fractional kW", "1-0:1.7.0(0*kW)\r\n1-0:2.7.0(2.4505*kW)", 2450.5),
        ("Belgian version", "0-0:96.1.4(50)\r\n1-0:1.7.0(0*kW)\r\n1-0:2.7.0(2*kW)", 2000.0),
    ] {
        let value = parse(&checksum(&format!("/SYNTHETIC\r\n{fields}\r\n!"))).unwrap();
        assert_eq!(value.grid_export_w, expected, "{label}");
    }
}

fn stream_values(
    parser: &mut DsmrStream,
    bytes: &[u8],
    now: i64,
    values: &mut Vec<Reading>,
    issues: &mut Vec<&'static str>,
) {
    for event in parser.feed(bytes, now) {
        match event {
            StreamEvent::Reading(r) => values.push(*r),
            StreamEvent::Issue(issue) => issues.push(issue),
        }
    }
}

#[test]
fn all_split_positions_and_multiple_telegrams_per_chunk() {
    let telegram = synthetic_telegram(0.0, 2450.0, "").into_bytes();
    let double = [telegram.clone(), telegram.clone()].concat();
    for i in 0..=telegram.len() {
        let (mut values, mut issues) = (Vec::new(), Vec::new());
        let mut parser = DsmrStream::new(false);
        stream_values(&mut parser, &telegram[..i], 1000, &mut values, &mut issues);
        stream_values(&mut parser, &telegram[i..], 1000, &mut values, &mut issues);
        stream_values(&mut parser, &double, 2000, &mut values, &mut issues);
        assert!(issues.is_empty(), "split {i}: {issues:?}");
        assert_eq!(values.len(), 3, "split {i}");
    }
}

#[test]
fn byte_by_byte_input_and_initial_noise() {
    let (mut values, mut issues) = (Vec::new(), Vec::new());
    let mut parser = DsmrStream::new(false);
    for byte in format!("boot noise\r\n{}", synthetic_telegram(0.0, 1200.0, "")).bytes() {
        stream_values(&mut parser, &[byte], 100, &mut values, &mut issues);
    }
    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(values.len(), 1);
}

#[test]
fn printable_delimiters_inside_a_message_do_not_break_framing() {
    let text = checksum("/SYNTHETIC\r\n0-0:96.13.0(Message with / and !)\r\n1-0:1.7.0(0*kW)\r\n1-0:2.7.0(2*kW)\r\n!");
    let (mut values, mut issues) = (Vec::new(), Vec::new());
    stream_values(&mut DsmrStream::new(false), text.as_bytes(), 0, &mut values, &mut issues);
    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].grid_export_w, 2000.0);
}

#[test]
fn rejects_invalid_telegrams() {
    let base = synthetic_telegram(0.0, 2450.0, "");
    let missing_export = checksum(&format!("{}!", &base[..base.find("1-0:2.7.0").unwrap()]));
    let cases: Vec<(&str, String, &str)> = vec![
        ("CRC failure", base.replace("2.450", "2.451"), "checksum"),
        ("missing export", missing_export, "missing-power"),
        ("wrong units", checksum("/S\r\n1-0:1.7.0(0*kW)\r\n1-0:2.7.0(2*kWh)\r\n!"), "power-format"),
        ("negative power", checksum("/S\r\n1-0:1.7.0(0*kW)\r\n1-0:2.7.0(-2*kW)\r\n!"), "power-format"),
        (
            "duplicate field",
            checksum("/S\r\n1-0:1.7.0(0*kW)\r\n1-0:2.7.0(2*kW)\r\n1-0:2.7.0(3*kW)\r\n!"),
            "duplicate-power",
        ),
        ("out of range", checksum("/S\r\n1-0:1.7.0(0*kW)\r\n1-0:2.7.0(999999999999999999999*kW)\r\n!"), "power-range"),
        ("bad framing", base[1..].to_string(), "framing"),
        ("non-ASCII", base.replace("SOLAR", "\u{ff}"), "encoding"),
    ];
    for (label, text, expected) in cases {
        let error = parse(&text).unwrap_err();
        assert!(error.contains(expected), "{label}: {error}");
    }
}

#[test]
fn legacy_telegrams_require_opt_in_and_a_pre_4_version() {
    let legacy = "/S\r\n1-3:0.2.8(30)\r\n1-0:1.7.0(0*kW)\r\n1-0:2.7.0(2*kW)\r\n!\r\n";
    assert_eq!(parse(legacy).unwrap_err(), "checksum-required");
    assert_eq!(parse_with(legacy, true).unwrap().checksum, Some("legacy-unchecked"));
    assert_eq!(parse_with(&legacy.replace("(30)", "(50)"), true).unwrap_err(), "checksum-required");
}

#[test]
fn bounded_framing_rejects_and_recovers() {
    let (mut values, mut issues) = (Vec::new(), Vec::new());
    let mut parser = DsmrStream::new(false);
    stream_values(&mut parser, b"/partial", 0, &mut values, &mut issues);
    stream_values(&mut parser, synthetic_telegram(0.0, 1000.0, "").as_bytes(), 10, &mut values, &mut issues);
    stream_values(&mut parser, format!("/{}", "x".repeat(17000)).as_bytes(), 20, &mut values, &mut issues);
    stream_values(&mut parser, b"/waiting", 30, &mut values, &mut issues);
    let mut events = Vec::new();
    parser.expire(16000, &mut events);
    for event in events {
        if let StreamEvent::Issue(issue) = event {
            issues.push(issue);
        }
    }
    stream_values(&mut parser, &[47, 255], 17000, &mut values, &mut issues);
    stream_values(&mut parser, synthetic_telegram(0.0, 2000.0, "").as_bytes(), 18000, &mut values, &mut issues);
    assert_eq!(issues, ["incomplete", "oversized", "frame-timeout", "encoding"]);
    assert_eq!(values.len(), 2);
    assert_eq!(parser.len(), 0);
}

#[test]
fn go_requires_fresh_samples_for_the_whole_window() {
    let mut engine = DecisionEngine::new(fast_config());
    for t in [0, 1000, 2000, 4000] {
        assert!(!engine.evaluate_at(Some(&reading(t, 4000.0)), t).safe_to_start);
    }
    let go = engine.evaluate_at(Some(&reading(5000, 4000.0)), 5000);
    assert_eq!(go.state, State::Excellent);
    assert!(go.safe_to_start);
    assert_eq!(go.level, Level::Surplus);
    assert_eq!(go.character_mode, 2);
}

#[test]
fn polling_one_reading_cannot_manufacture_stability() {
    let mut engine = DecisionEngine::new(config(json!({"decision": {"stableDurationSeconds": 5}})));
    engine.evaluate_at(Some(&reading(0, 4000.0)), 0);
    assert!(!engine.evaluate_at(Some(&reading(0, 4000.0)), 5000).safe_to_start);
}

#[test]
fn exact_start_and_retention_thresholds_never_permit_go() {
    let mut engine = DecisionEngine::new(config(json!({"decision": {"stableDurationSeconds": 0}})));
    for (t, export, go) in
        [(0, 2450.0, false), (1, 2451.0, true), (2, 2301.0, true), (3, 2300.0, false), (4, 2400.0, false)]
    {
        assert_eq!(engine.evaluate_at(Some(&reading(t, export)), t).safe_to_start, go, "{export} W");
    }
}

#[test]
fn a_dip_resets_the_stability_window() {
    let mut engine = DecisionEngine::new(fast_config());
    engine.evaluate_at(Some(&reading(0, 4000.0)), 0);
    engine.evaluate_at(Some(&reading(2000, 4000.0)), 2000);
    assert_eq!(engine.evaluate_at(Some(&reading(3000, 2000.0)), 3000).state, State::Limited);
    assert_eq!(engine.evaluate_at(Some(&reading(4000, 4000.0)), 4000).stable_seconds, 0.0);
    assert!(!engine.evaluate_at(Some(&reading(6000, 4000.0)), 6000).safe_to_start);
}

#[test]
fn bad_data_is_offline_not_zero_surplus() {
    let invalid = Reading { quality: Quality::Invalid, ..reading(0, 4000.0) };
    let cases = [
        ("missing", None, 0),
        ("invalid quality", Some(invalid), 0),
        ("stale", Some(reading(0, 4000.0)), 3001),
        ("negative power", Some(reading(0, -1.0)), 0),
        ("NaN", Some(reading(0, f64::NAN)), 0),
        ("future", Some(reading(1000, 4000.0)), 0),
    ];
    for (label, value, now) in cases {
        let result = DecisionEngine::new(fast_config()).evaluate_at(value.as_ref(), now);
        assert_eq!(result.state, State::Offline, "{label}");
        assert_eq!(result.surplus_w, None, "{label}");
        assert!(!result.safe_to_start, "{label}");
        assert_eq!(result.character_mode, 1, "{label}");
    }
}

#[test]
fn import_takes_priority_and_small_export_is_idle() {
    let mut engine = DecisionEngine::new(fast_config());
    assert_eq!(engine.evaluate_at(Some(&with_import(reading(0, 4000.0), 1.0)), 0).state, State::Importing);
    assert_eq!(engine.evaluate_at(Some(&reading(1, 100.0)), 1).state, State::Idle);
}

#[test]
fn gaps_source_changes_and_backward_timestamps_reset_stability() {
    let other = Reading { source: "different".into(), ..reading(2000, 4000.0) };
    for value in [reading(4001, 4000.0), other, reading(0, 4000.0)] {
        let mut engine = DecisionEngine::new(fast_config());
        engine.evaluate_at(Some(&reading(0, 4000.0)), 0);
        engine.evaluate_at(Some(&reading(1000, 4000.0)), 1000);
        assert_eq!(engine.evaluate_at(Some(&value), value.timestamp).stable_seconds, 0.0);
    }
}

#[test]
fn appliance_selection_and_safety_margin() {
    let mut engine =
        DecisionEngine::new(config(json!({"decision": {"stableDurationSeconds": 0, "safetyMarginW": 500}})));
    assert!(!engine.evaluate_at(Some(&reading(0, 2600.0)), 0).safe_to_start);
    engine.select_appliance("heatPumpDryer").unwrap();
    let result = engine.evaluate_at(Some(&reading(1, 2600.0)), 1);
    assert_eq!(result.required_w, 1700.0);
    assert_eq!(result.state, State::Good);
    assert!(engine.select_appliance("__proto__").unwrap_err().to_string().contains("Unknown"));
}

#[test]
fn rejects_invalid_configuration() {
    for input in [
        json!({"energySource": {"port": 0}}),
        json!({"energySource": {"type": "cloud"}}),
        json!({"decision": {"stableDurationSeconds": -1}}),
        json!({"decision": {"staleAfterSeconds": 0}}),
        json!({"decision": {"typo": 2}}),
        json!({"decision": {"defaultAppliance": "missing"}}),
        json!({"appliances": {}}),
        json!({"appliances": {"a": {"name": "A", "peakW": null}}}),
        json!({"decision": {"maxSurplusW": 300}}),
        json!({"location": {"latitude": 50}}),
        json!({"location": {"latitude": 91, "longitude": 0}}),
        json!({"location": {"latitude": 0, "longitude": 181}}),
        json!({"location": {"latitude": "50", "longitude": 4}}),
        json!({"location": {"city": "x"}}),
    ] {
        assert!(SolarConfig::from_json(&input).is_err(), "{input}");
    }
}

#[test]
fn configuration_round_trips_through_json() {
    let original = config(json!({
        "energySource": {"type": "slimmelezer", "host": "192.0.2.50", "port": 23},
        "inverterSource": {"host": "192.0.2.30"},
        "location": {"latitude": 50.85, "longitude": 4.35},
        "decision": {"importPeakW": 4000}
    }));
    let again = SolarConfig::from_json(&original.to_json()).unwrap();
    assert_eq!(again, original);
    assert!(!again.is_mock());
    assert!(again.inverter_enabled());
}

#[test]
fn default_meter_host_is_a_documentation_address() {
    let defaults = SolarConfig::default();
    assert!(defaults.is_mock());
    assert_eq!(defaults.energy_source.host, "192.0.2.10", "RFC 5737 TEST-NET-1, not a real meter");
}

//! Port of solar-agent `solar/test/peak-forecast.test.mjs`.

use serde_json::Value;
use solar_core::config::SolarConfig;
use solar_core::decision::DecisionEngine;
use solar_core::dsmr::{ParseOptions, crc16, parse_telegram};
use solar_core::mock::{PeakMeter, peak_scenario};
use solar_core::peak_forecast::{
    Forecast, MINIMUM_PEAK_W, Observation, PeakForecaster, PeakSample, forecast_quarter, peak_forecast_sample,
};
use solar_core::reading::Reading;
use solar_core::sun::Sun;
use solar_core::utc_ms;

fn telegram(fields: &str) -> String {
    let body = format!("/SYNTHETIC\r\n1-0:1.7.0(01.000*kW)\r\n1-0:2.7.0(00.000*kW)\r\n{fields}\r\n!");
    format!("{body}{:04x}\r\n", crc16(body.as_bytes()))
}

fn parse(fields: &str) -> Reading {
    parse_telegram(&telegram(fields), &ParseOptions::default()).unwrap()
}

fn near(actual: Option<f64>, expected: f64, label: &str) {
    let actual = actual.unwrap_or_else(|| panic!("{label}: missing"));
    assert!((actual - expected).abs() <= 0.5, "{label}: {actual} != {expected}");
}

fn quarter(sample: &PeakSample) -> Forecast {
    forecast_quarter(Some(sample), Observation::default())
}

#[test]
fn shared_vectors_pin_projection_reasons_and_hysteresis() {
    let path = format!("{}/tests/fixtures/peak-forecast-vectors.json", env!("CARGO_MANIFEST_DIR"));
    let vectors: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut name = String::new();
    let mut forecaster = PeakForecaster::new();
    for step in vectors["steps"].as_array().unwrap() {
        let case = step["case"].as_str().unwrap();
        if case != name {
            name = case.to_string();
            forecaster = PeakForecaster::new();
        }
        let label = format!("{case} at {}", step["time"]);
        let result = forecaster.update(Some(&PeakSample {
            offline: step["offline"].as_bool().unwrap_or(false),
            meter_seconds: step["time"].as_f64(),
            import_w: step["importW"].as_f64(),
            demand_w: step["demandW"].as_f64(),
            peak_w: step["peakW"].as_f64(),
            meter_month: step["meterMonth"].as_f64(),
            peak_month: step["peakMonth"].as_f64(),
        }));
        assert_eq!(result.reason, step["reason"].as_str().unwrap(), "{label}");
        assert_eq!(result.available, step["available"].as_bool().unwrap(), "{label}");
        assert_eq!(result.warning, step["warning"].as_bool().unwrap(), "{label}");
        if !result.available {
            assert_eq!(
                [
                    result.projected_w,
                    result.comparison_w,
                    result.reduce_w,
                    result.seconds_left,
                    result.crossing_seconds
                ],
                [None; 5],
                "{label}"
            );
            continue;
        }
        near(result.projected_w, step["projectedW"].as_f64().unwrap(), &format!("{label} projected"));
        assert_eq!(result.comparison_w, step["comparisonW"].as_f64(), "{label}");
        assert_eq!(result.comparison_is_minimum, step["minimum"].as_bool().unwrap(), "{label}");
        assert_eq!(result.seconds_left, step["secondsLeft"].as_f64(), "{label}");
        near(result.reduce_w, step["reduceW"].as_f64().unwrap(), &format!("{label} reduce"));
        match step["crossingSeconds"].as_f64() {
            None => assert_eq!(result.crossing_seconds, None, "{label}"),
            Some(expected) => near(result.crossing_seconds, expected, &format!("{label} crossing")),
        }
    }
}

#[test]
fn projection_uses_the_full_quarter_in_watts() {
    let start = (utc_ms(2026, 6, 14, 10, 0, 0) / 1000) as f64;
    let sample = PeakSample {
        meter_seconds: Some(start + 600.0),
        import_w: Some(3000.0),
        demand_w: Some(2000.0),
        peak_w: Some(2800.0),
        meter_month: Some(202606.0),
        peak_month: Some(202606.0),
        ..PeakSample::default()
    };
    let result = quarter(&sample);
    assert_eq!(result.projected_w, Some(3000.0));
    assert_eq!(result.seconds_left, Some(300.0));
    assert_eq!(result.reduce_w, Some(600.0));
    assert_eq!(result.crossing_seconds, Some(240.0));
    assert_eq!(quarter(&PeakSample { import_w: Some(2400.0), ..sample }).crossing_seconds, None);
    assert_eq!(quarter(&PeakSample { meter_seconds: Some(start + 600.9), ..sample }).seconds_left, Some(300.0));
    assert_eq!(MINIMUM_PEAK_W, 2500.0);
}

#[test]
fn parser_reads_meter_clock_and_peak_month_summer_and_winter() {
    let summer = parse("0-0:1.0.0(260614120700S)\r\n1-0:1.4.0(00.500*kW)\r\n1-0:1.6.0(260603194512S)(03.150*kW)");
    assert_eq!(summer.meter_time, Some(utc_ms(2026, 6, 14, 10, 7, 0)));
    assert_eq!(summer.meter_month, Some(202606));
    assert_eq!(summer.monthly_peak_w, Some(3150.0));
    assert_eq!(summer.monthly_peak_at, Some(utc_ms(2026, 6, 3, 17, 45, 12)));
    assert_eq!(summer.monthly_peak_month, Some(202606));
    let winter = parse("0-0:1.0.0(261201000500W)\r\n1-0:1.6.0(261201000058W)(02.100*kW)");
    assert_eq!(winter.meter_time, Some(utc_ms(2026, 11, 30, 23, 5, 0)));
    assert_eq!(winter.meter_month, Some(202612), "the local month, not the UTC month");
    assert_eq!(winter.monthly_peak_at, Some(utc_ms(2026, 11, 30, 23, 0, 58)));
    assert_eq!(winter.monthly_peak_month, Some(202611), "a capture just after midnight is last month's peak");
    let reading = Reading { timestamp: 0, current_demand_w: Some(300.0), ..winter };
    assert_eq!(quarter(&peak_forecast_sample(Some(&reading), true)).reason, "previous-month-peak");
    let later = parse("0-0:1.0.0(261201001600W)\r\n1-0:1.6.0(261201001502W)(02.100*kW)");
    assert_eq!(later.monthly_peak_month, Some(202612), "the first quarter of the month");
}

#[test]
fn malformed_duplicate_or_missing_clock_keeps_the_reading() {
    let bad = parse("0-0:1.0.0(260614120700S)\r\n1-0:1.6.0(260631194512S)(03.150*kW)");
    assert_eq!(bad.monthly_peak_w, Some(3150.0));
    assert_eq!((bad.monthly_peak_at, bad.monthly_peak_month), (None, None));
    let reading = Reading { current_demand_w: Some(300.0), ..bad };
    assert_eq!(quarter(&peak_forecast_sample(Some(&reading), true)).reason, "no-peak-time");
    assert_eq!(parse("1-0:1.6.0(260603194512X)(03.150*kW)").monthly_peak_w, None);
    let duplicate = parse("0-0:1.0.0(260614120700S)\r\n0-0:1.0.0(260614120701S)");
    assert_eq!((duplicate.meter_time, duplicate.meter_month), (None, None));
    let missing = parse("1-0:1.4.0(00.500*kW)");
    assert_eq!(
        (missing.meter_time, missing.meter_month, missing.monthly_peak_at, missing.monthly_peak_month),
        (None, None, None, None)
    );
    assert_eq!(missing.grid_import_w, 1000.0);
}

#[test]
fn stale_or_missing_readings_report_offline() {
    assert_eq!(peak_forecast_sample(None, true), PeakSample::offline());
    let reading = Reading::new(0, 1.0, 0.0, "test");
    assert_eq!(peak_forecast_sample(Some(&reading), false), PeakSample::offline());
    let mut forecaster = PeakForecaster::new();
    assert_eq!(forecaster.update(Some(&peak_forecast_sample(None, false))).reason, "offline");
    assert!(!forecaster.update(None).available);
}

fn run(name: &str, seconds: i64) -> (Forecast, Reading) {
    let mut meter = PeakMeter::new(peak_scenario(name).unwrap());
    let mut forecaster = PeakForecaster::new();
    let mut last = None;
    for second in 0..=seconds {
        let reading = meter.reading(second, 1000 + second * 1000);
        let result = forecaster.update(Some(&peak_forecast_sample(Some(&reading), true)));
        last = Some((result, reading));
    }
    last.unwrap()
}

#[test]
fn simulated_peak_meters_show_each_forecast_case() {
    let (approaching, _) = run("peak-approaching", 40);
    assert!(approaching.available);
    assert!(approaching.warning);
    assert_eq!(approaching.comparison_w, Some(2800.0));
    assert!(approaching.reduce_w.unwrap() > 0.0);
    assert!(!run("peak-approaching", 20).0.warning, "the warning waits 30 s");
    let (avoided, _) = run("peak-avoided", 70);
    assert!(!avoided.warning);
    assert!(avoided.projected_w.unwrap() < 0.95 * 2800.0);
    assert!(run("peak-avoided", 50).0.warning);
    let (exceeded, reading) = run("peak-exceeded", 5);
    assert_eq!(exceeded.reason, "peak-reached");
    let decision = DecisionEngine::new(SolarConfig::default()).evaluate(Some(&reading), reading.timestamp, Sun::Up);
    assert!(decision.monthly_peak_reached);
    assert!(!decision.safe_to_start);
    assert_eq!(run("peak-rollover", 5).0.reason, "previous-month-peak");
    assert_eq!(run("peak-no-time", 5).0.reason, "no-meter-time");
    let mut boundary = PeakMeter::new(peak_scenario("peak-exceeded").unwrap());
    assert!(boundary.reading(119, 0).current_demand_w.unwrap() > 2900.0);
    assert_eq!(boundary.reading(120, 0).current_demand_w, Some(0.0));
    assert_eq!(boundary.reading(121, 0).current_demand_w, Some(3.0));
}

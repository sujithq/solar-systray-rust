//! Port of solar-agent `daemon/test/night-baseline.test.mjs` and `daemon/test/sungrow.test.ts`.

use serde_json::json;
use solar_core::config::{InverterSource, SolarConfig};
use solar_core::night_baseline::{BaselineState, DEFAULT_NIGHT_BASELINE_W, NightBaseline};
use solar_core::reading::Reading;
use solar_core::runtime::Runtime;
use solar_core::sun::Sun;
use solar_core::sungrow::{InverterStatus, Powers, SungrowState, build_request, parse_response, read_once};
use std::io::{Read, Write};
use std::net::TcpListener;

fn state(baseline_w: f64, learned: bool, samples: usize) -> BaselineState {
    BaselineState { baseline_w, learned, samples }
}

#[test]
fn night_baseline_uses_the_median_of_one_minute_samples() {
    let mut baseline = NightBaseline::new();
    assert_eq!(baseline.update(Sun::Up, None, None), state(DEFAULT_NIGHT_BASELINE_W, false, 0));
    let start = 1_800_000_000_000_i64;
    for minute in 0..35 {
        let sample = if minute == 34 { 1200.0 } else { 300.0 + (minute % 2) as f64 * 20.0 };
        baseline.update(Sun::Down, Some(sample), Some(start + minute * 60_000));
        baseline.update(Sun::Down, Some(sample + 100.0), Some(start + minute * 60_000 + 5_000));
    }
    assert_eq!(baseline.update(Sun::Up, Some(800.0), Some(start + 35 * 60_000)), state(320.0, true, 0));
    assert_eq!(baseline.update(Sun::Up, Some(850.0), Some(start + 36 * 60_000)), state(320.0, true, 0));
}

#[test]
fn an_incomplete_night_keeps_the_previous_baseline() {
    let mut baseline = NightBaseline::new();
    let start = 1_800_000_000_000_i64;
    for minute in 0..30 {
        baseline.update(Sun::Down, Some(320.0), Some(start + minute * 60_000));
    }
    assert_eq!(baseline.update(Sun::Up, Some(900.0), Some(start + 30 * 60_000)), state(320.0, true, 0));
    for minute in 0..29 {
        baseline.update(Sun::Down, Some(700.0), Some(start + (40 + minute) * 60_000));
    }
    assert_eq!(baseline.update(Sun::Up, Some(900.0), Some(start + 70 * 60_000)), state(320.0, true, 0));
}

#[test]
fn household_use_requires_live_meter_data_and_fresh_ac_output() {
    let config = SolarConfig::from_json(&json!({
        "energySource": {"type": "slimmelezer", "host": "127.0.0.1"},
        "inverterSource": {"host": "127.0.0.1"}
    }))
    .unwrap();
    let mut runtime = Runtime::new(config);
    let now = 1_800_000_000_000_i64;
    runtime.inverter_result(Ok(Powers { pv_power_w: 900, active_power_w: 800 }), now);
    runtime.push_reading(Reading::new(now, 0.0, 432.0, "meter"), now);
    assert_eq!(runtime.snapshot(now).inverter.household_use_w, Some(368.0));
    runtime.push_reading(Reading::new(now, 200.0, 432.0, "meter"), now);
    assert_eq!(runtime.snapshot(now).inverter.household_use_w, Some(568.0));
    runtime.push_reading(Reading::new(now - 120_000, 200.0, 432.0, "meter"), now);
    assert_eq!(runtime.snapshot(now).inverter.household_use_w, None);
}

fn response(transaction: u16, unit: u8, pv: i32, active: i32) -> Vec<u8> {
    let mut frame = vec![0u8; 41];
    frame[0..2].copy_from_slice(&transaction.to_be_bytes());
    frame[4..6].copy_from_slice(&35u16.to_be_bytes());
    frame[6] = unit;
    frame[7] = 4;
    frame[8] = 32;
    let mut put32 = |value: i32, offset: usize| {
        let raw = value as u32;
        frame[offset..offset + 2].copy_from_slice(&((raw & 0xffff) as u16).to_be_bytes());
        frame[offset + 2..offset + 4].copy_from_slice(&((raw >> 16) as u16).to_be_bytes());
    };
    put32(pv, 9);
    put32(active, 37);
    frame
}

#[test]
fn sungrow_response_decodes_low_word_first_watts() {
    assert_eq!(
        parse_response(&response(1, 1, 3577, 3517), 1, 1),
        Ok(Powers { pv_power_w: 3577, active_power_w: 3517 })
    );
    assert_eq!(
        parse_response(&response(1, 1, 3577, -840), 1, 1),
        Ok(Powers { pv_power_w: 3577, active_power_w: -840 })
    );
    let mut exception = response(1, 1, 3577, 3517);
    exception[7] = 0x84;
    let mut short_count = response(1, 1, 3577, 3517);
    short_count[8] = 30;
    for (label, bad) in [
        ("zeros", vec![0u8; 41]),
        ("transaction", response(2, 1, 3577, 3517)),
        ("unit", response(1, 2, 3577, 3517)),
        ("exception", exception),
        ("byte count", short_count),
        ("implausible PV", response(1, 1, 1_000_001, 3517)),
    ] {
        assert!(parse_response(&bad, 1, 1).is_err(), "{label}");
    }
}

#[test]
fn sungrow_request_reads_sixteen_registers_from_5016() {
    let request = build_request(0x1234, 7);
    let word = |at: usize| u16::from_be_bytes([request[at], request[at + 1]]);
    assert_eq!(word(0), 0x1234);
    assert_eq!(word(2), 0);
    assert_eq!(word(4), 6);
    assert_eq!(request[6], 7);
    assert_eq!(request[7], 4);
    assert_eq!(word(8), 5016);
    assert_eq!(word(10), 16);
}

#[test]
fn sungrow_polls_the_configured_unit_and_reports_stale_readings_offline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = [0u8; 12];
        socket.read_exact(&mut request).unwrap();
        socket.write_all(&response(u16::from_be_bytes([request[0], request[1]]), 7, 3577, 3517)).unwrap();
        request
    });
    let source = InverterSource { host: "127.0.0.1".into(), port, unit_id: 7 };
    let mut sungrow = SungrowState::default();
    let transaction = sungrow.next_transaction();
    let now = 1_000_000;
    sungrow.record(read_once(&source, transaction), now);
    let request = server.join().unwrap();
    assert_eq!(request[6], 7);
    let snapshot = sungrow.snapshot(true, now);
    assert_eq!(snapshot.status, InverterStatus::Online);
    assert_eq!((snapshot.pv_power_w, snapshot.active_power_w), (Some(3577.0), Some(3517.0)));
    assert_eq!((snapshot.household_use_w, snapshot.timestamp, snapshot.issue), (None, Some(now), None));
    let stale = sungrow.snapshot(true, now + 90_001);
    assert_eq!(stale.status, InverterStatus::Offline);
    assert_eq!(stale.pv_power_w, None);
}

#[test]
fn sungrow_connection_errors_are_reported() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let error = read_once(&InverterSource { host: "127.0.0.1".into(), port, unit_id: 1 }, 1).unwrap_err();
    assert!(error.starts_with("Sungrow connection"), "{error}");
}

#[test]
fn sungrow_source_is_optional_and_validated() {
    assert_eq!(SolarConfig::default().inverter_source.host, "");
    assert!(!SolarConfig::default().inverter_enabled());
    for source in [
        json!({"host": "inverter.local"}),
        json!({"host": "192.0.2.30", "port": 0}),
        json!({"host": "192.0.2.30", "unitId": 248}),
    ] {
        assert!(SolarConfig::from_json(&json!({"inverterSource": source})).is_err(), "{source}");
    }
}

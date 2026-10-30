//! End-to-end: the threaded service against a fake SlimmeLezer (raw TCP telegrams) and a fake Sungrow.

use serde_json::json;
use solar_core::SolarConfig;
use solar_core::decision::State;
use solar_core::dsmr::synthetic_telegram;
use solar_core::present::{solar_info, tooltip};
use solar_core::service::SolarService;
use solar_core::sungrow::InverterStatus;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

fn fake_meter(stop: Arc<AtomicBool>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let telegram = synthetic_telegram(0.0, 2450.0, "");
        while !stop.load(Ordering::Relaxed) {
            // Split each telegram to exercise reassembly across reads.
            let (a, b) = telegram.split_at(telegram.len() / 2);
            if socket.write_all(a.as_bytes()).is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
            if socket.write_all(b.as_bytes()).is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(200));
        }
    });
    port
}

fn fake_inverter() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(mut socket) = socket else { return };
            let mut request = [0u8; 12];
            if socket.read_exact(&mut request).is_err() {
                continue;
            }
            let mut frame = vec![0u8; 41];
            frame[0..2].copy_from_slice(&request[0..2]);
            frame[4..6].copy_from_slice(&35u16.to_be_bytes());
            frame[6] = request[6];
            frame[7] = 4;
            frame[8] = 32;
            frame[9..11].copy_from_slice(&3000u16.to_be_bytes());
            frame[37..39].copy_from_slice(&2900u16.to_be_bytes());
            let _ = socket.write_all(&frame);
        }
    });
    port
}

fn wait_for(service: &SolarService, what: &str, done: impl Fn(&solar_core::Snapshot) -> bool) -> solar_core::Snapshot {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = service.snapshot();
        if done(&snapshot) {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}: {}", tooltip(&snapshot));
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn service_reads_a_live_meter_and_inverter() {
    let stop = Arc::new(AtomicBool::new(false));
    let meter_port = fake_meter(stop.clone());
    let inverter_port = fake_inverter();
    let config = SolarConfig::from_json(&json!({
        "energySource": {"type": "slimmelezer", "host": "127.0.0.1", "port": meter_port},
        "inverterSource": {"host": "127.0.0.1", "port": inverter_port}
    }))
    .unwrap();
    let service = SolarService::start(config);
    let s = wait_for(&service, "a live meter and inverter", |s| {
        s.evaluation.surplus_w.is_some() && s.inverter.status == InverterStatus::Online
    });
    assert_eq!(s.evaluation.surplus_w, Some(2450.0));
    assert_ne!(s.evaluation.state, State::Offline);
    assert!(!s.simulated);
    assert_eq!(s.meter_address.as_deref(), Some(format!("127.0.0.1:{meter_port}").as_str()));
    assert_eq!(s.inverter.pv_power_w, Some(3000.0));
    assert_eq!(s.inverter.household_use_w, Some(450.0));
    assert!(s.diagnostics.connected);
    let info = solar_info(&s);
    let meter = info.section("Connections").unwrap().value("Meter").unwrap();
    assert!(meter.ends_with("(live)"), "{meter}");
    assert!(tooltip(&s).contains("Export 2450 W | PV 3.0 kW"), "{}", tooltip(&s));
    service.select_appliance("washingMachine").unwrap();
    assert_eq!(service.appliance(), "washingMachine");
    assert!(service.set_scenario("excellent").is_err(), "scenarios are for simulation only");
    stop.store(true, Ordering::Relaxed);
    service.stop();
}

#[test]
fn service_reports_an_unreachable_meter() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let config =
        SolarConfig::from_json(&json!({"energySource": {"type": "slimmelezer", "host": "127.0.0.1", "port": port}}))
            .unwrap();
    let service = SolarService::start(config);
    let s = wait_for(&service, "a connection issue", |s| s.diagnostics.last_issue.is_some());
    assert_eq!(s.evaluation.state, State::Offline);
    assert_eq!(s.inverter.status, InverterStatus::Disabled);
    assert!(s.inverter_address.is_none());
    let meter = solar_info(&s).section("Connections").unwrap().value("Meter").unwrap().to_string();
    assert!(meter.contains("(offline: ECONNREFUSED)"), "{meter}");
}

#[test]
fn service_simulates_without_network() {
    let service = SolarService::start(SolarConfig::default());
    let s = wait_for(&service, "a simulated reading", |s| s.evaluation.surplus_w.is_some());
    assert!(s.simulated);
    assert_eq!(s.scenario.as_deref(), Some("excellent"));
    service.set_simulated_watts(1234).unwrap();
    assert_eq!(service.snapshot().evaluation.surplus_w, Some(1234.0));
    service.set_scenario("night").unwrap();
    assert_eq!(service.snapshot().scenario.as_deref(), Some("night"));
}

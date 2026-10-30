//! Background threads that keep a [`Runtime`] fed: the meter link (or the 1 Hz simulator)
//! and the Sungrow poller.

use crate::config::{InverterSource, SolarConfig};
use crate::now_ms;
use crate::runtime::{ControlError, Runtime, Snapshot};
use crate::slimmelezer::{CONNECT_TIMEOUT_MS, IDLE_TIMEOUT_MS};
use crate::sungrow::{SUNGROW_POLL_MS, io_error_code, read_once, resolve};
use std::io::{ErrorKind, Read};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

const READ_SLICE: Duration = Duration::from_millis(500);

pub struct SolarService {
    runtime: Arc<Mutex<Runtime>>,
    stop: Arc<AtomicBool>,
}

fn lock(runtime: &Mutex<Runtime>) -> MutexGuard<'_, Runtime> {
    runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Sleeps up to `duration`, waking early when `stop` is set. Returns false once stopped.
pub(crate) fn pause(stop: &AtomicBool, duration: Duration) -> bool {
    let deadline = Instant::now() + duration;
    while !stop.load(Ordering::Relaxed) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        thread::sleep(left.min(Duration::from_millis(200)));
    }
    false
}

impl SolarService {
    pub fn start(config: SolarConfig) -> Self {
        let mock = config.is_mock();
        let inverter = config.inverter_enabled().then(|| config.inverter_source.clone());
        let source = (config.energy_source.host.clone(), config.energy_source.port);
        let runtime = Arc::new(Mutex::new(Runtime::new(config)));
        let stop = Arc::new(AtomicBool::new(false));
        let spawn = |name: &str, body: Box<dyn FnOnce() + Send>| {
            thread::Builder::new().name(name.into()).spawn(body).expect("spawn worker thread");
        };
        {
            let (runtime, stop) = (runtime.clone(), stop.clone());
            if mock {
                spawn("solar-simulator", Box::new(move || simulator(&runtime, &stop)));
            } else {
                spawn("solar-meter", Box::new(move || meter(&runtime, &stop, &source.0, source.1)));
            }
        }
        if let Some(inverter) = inverter {
            let (runtime, stop) = (runtime.clone(), stop.clone());
            spawn("solar-inverter", Box::new(move || sungrow(&runtime, &stop, &inverter)));
        }
        SolarService { runtime, stop }
    }

    pub fn snapshot(&self) -> Snapshot {
        lock(&self.runtime).snapshot(now_ms())
    }

    pub fn appliance(&self) -> String {
        lock(&self.runtime).appliance().to_string()
    }

    pub fn select_appliance(&self, id: &str) -> Result<(), ControlError> {
        lock(&self.runtime).select_appliance(id)
    }

    pub fn set_scenario(&self, scenario: &str) -> Result<(), ControlError> {
        lock(&self.runtime).set_scenario(scenario, now_ms())
    }

    pub fn set_simulated_watts(&self, watts: i64) -> Result<(), ControlError> {
        lock(&self.runtime).set_simulated_watts(watts, now_ms())
    }

    /// Signals the threads to stop; they exit within about a second without being joined.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for SolarService {
    fn drop(&mut self) {
        self.stop();
    }
}

fn simulator(runtime: &Mutex<Runtime>, stop: &AtomicBool) {
    loop {
        lock(runtime).mock_tick(now_ms());
        if !pause(stop, Duration::from_secs(1)) {
            return;
        }
    }
}

fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    let address = resolve(host, port).map_err(|e| io_error_code(&e))?;
    TcpStream::connect_timeout(&address, Duration::from_millis(CONNECT_TIMEOUT_MS)).map_err(|error| {
        if error.kind() == ErrorKind::TimedOut { "connect-timeout".into() } else { io_error_code(&error) }
    })
}

fn meter(runtime: &Mutex<Runtime>, stop: &AtomicBool, host: &str, port: u16) {
    while !stop.load(Ordering::Relaxed) {
        match connect(host, port) {
            Err(code) => lock(runtime).meter_issue(&code),
            Ok(socket) => {
                lock(runtime).meter_connected();
                read_meter(runtime, stop, socket);
            }
        }
        let running = !stop.load(Ordering::Relaxed);
        let delay = lock(runtime).meter_closed(running, now_ms());
        match delay {
            Some(ms) if pause(stop, Duration::from_millis(ms)) => {}
            _ => return,
        }
    }
}

fn read_meter(runtime: &Mutex<Runtime>, stop: &AtomicBool, mut socket: TcpStream) {
    if let Err(error) = socket.set_read_timeout(Some(READ_SLICE)) {
        lock(runtime).meter_issue(&io_error_code(&error));
        return;
    }
    let mut last_data = Instant::now();
    let mut buffer = [0u8; 4096];
    while !stop.load(Ordering::Relaxed) {
        match socket.read(&mut buffer) {
            Ok(0) => return,
            Ok(n) => {
                last_data = Instant::now();
                lock(runtime).meter_bytes(&buffer[..n], now_ms());
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                if last_data.elapsed() >= Duration::from_millis(IDLE_TIMEOUT_MS) {
                    lock(runtime).meter_issue("serial-idle-timeout");
                    return;
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => {
                lock(runtime).meter_issue(&io_error_code(&error));
                return;
            }
        }
    }
}

fn sungrow(runtime: &Mutex<Runtime>, stop: &AtomicBool, source: &InverterSource) {
    loop {
        let transaction = lock(runtime).next_inverter_transaction();
        let result = read_once(source, transaction);
        if stop.load(Ordering::Relaxed) {
            return;
        }
        lock(runtime).inverter_result(result, now_ms());
        if !pause(stop, Duration::from_millis(SUNGROW_POLL_MS as u64)) {
            return;
        }
    }
}

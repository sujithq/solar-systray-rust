//! Sungrow inverter over Modbus TCP (port of `sungrow.mjs`).

use crate::config::InverterSource;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

pub const SUNGROW_POLL_MS: i64 = 30_000;
pub const SUNGROW_STALE_MS: i64 = 90_000;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REGISTER_START: u16 = 5016;
const REGISTER_COUNT: u16 = 16;
const PV_POWER_OFFSET: usize = 0;
const ACTIVE_POWER_OFFSET: usize = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Powers {
    pub pv_power_w: i64,
    pub active_power_w: i64,
}

fn word(buffer: &[u8], offset: usize) -> u32 {
    u16::from_be_bytes([buffer[offset], buffer[offset + 1]]) as u32
}

/// Two registers, low word first.
fn decode_words(buffer: &[u8], offset: usize) -> u32 {
    (word(buffer, offset + 2) << 16) | word(buffer, offset)
}

pub fn build_request(transaction: u16, unit_id: u8) -> [u8; 12] {
    let mut request = [0u8; 12];
    request[0..2].copy_from_slice(&transaction.to_be_bytes());
    request[4..6].copy_from_slice(&6u16.to_be_bytes());
    request[6] = unit_id;
    request[7] = 4;
    request[8..10].copy_from_slice(&REGISTER_START.to_be_bytes());
    request[10..12].copy_from_slice(&REGISTER_COUNT.to_be_bytes());
    request
}

pub fn parse_response(buffer: &[u8], transaction: u16, unit_id: u8) -> Result<Powers, String> {
    if buffer.len() != 41
        || word(buffer, 0) != transaction as u32
        || word(buffer, 2) != 0
        || word(buffer, 4) != 35
        || buffer[6] != unit_id
    {
        return Err("invalid Modbus response".into());
    }
    if buffer[7] == 0x84 {
        return Err(format!("Modbus exception {}", buffer[8]));
    }
    if buffer[7] != 4 || buffer[8] as u16 != REGISTER_COUNT * 2 {
        return Err("unexpected Modbus response".into());
    }
    let pv_power_w = decode_words(buffer, 9 + PV_POWER_OFFSET * 2) as i64;
    let active_power_w = decode_words(buffer, 9 + ACTIVE_POWER_OFFSET * 2) as i32 as i64;
    if pv_power_w > 1_000_000 || active_power_w.abs() > 1_000_000 {
        return Err("Sungrow power is out of range".into());
    }
    Ok(Powers { pv_power_w, active_power_w })
}

/// A Node-style error code, so diagnostics read the same as the reference implementation.
pub fn io_error_code(error: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match error.kind() {
        ConnectionRefused => "ECONNREFUSED".into(),
        ConnectionReset => "ECONNRESET".into(),
        ConnectionAborted => "ECONNABORTED".into(),
        TimedOut | WouldBlock => "ETIMEDOUT".into(),
        HostUnreachable => "EHOSTUNREACH".into(),
        NetworkUnreachable => "ENETUNREACH".into(),
        NotFound => "ENOTFOUND".into(),
        AddrNotAvailable => "EADDRNOTAVAIL".into(),
        _ => error.raw_os_error().map_or_else(|| "tcp-error".into(), |code| format!("os-error-{code}")),
    }
}

pub(crate) fn resolve(host: &str, port: u16) -> std::io::Result<SocketAddr> {
    (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no address"))
}

/// One request/response exchange with a 3 s connect timeout and a 3 s response deadline.
pub fn read_once(source: &InverterSource, transaction: u16) -> Result<Powers, String> {
    let failed = |error: std::io::Error| format!("Sungrow connection failed: {}", io_error_code(&error));
    let address = resolve(&source.host, source.port).map_err(failed)?;
    let mut socket = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).map_err(|error| {
        if error.kind() == std::io::ErrorKind::TimedOut { "Sungrow connection timed out".into() } else { failed(error) }
    })?;
    socket.write_all(&build_request(transaction, source.unit_id)).map_err(failed)?;
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    let mut response = Vec::with_capacity(64);
    let mut chunk = [0u8; 300];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("Sungrow response timed out".into());
        }
        socket.set_read_timeout(Some(remaining)).map_err(failed)?;
        match socket.read(&mut chunk) {
            Ok(0) => return Err("Sungrow connection closed before a response".into()),
            Ok(n) => response.extend_from_slice(&chunk[..n]),
            Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return Err("Sungrow response timed out".into());
            }
            Err(error) => return Err(failed(error)),
        }
        if response.len() >= 6 {
            let total = 6 + word(&response, 4) as usize;
            if total > 260 {
                return Err("oversized Modbus response".into());
            }
            if response.len() >= total {
                return parse_response(&response[..total], transaction, source.unit_id);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InverterStatus {
    Online,
    Offline,
    Standby,
    Disabled,
}

impl InverterStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            InverterStatus::Online => "online",
            InverterStatus::Offline => "offline",
            InverterStatus::Standby => "standby",
            InverterStatus::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InverterSnapshot {
    pub status: InverterStatus,
    pub pv_power_w: Option<f64>,
    pub active_power_w: Option<f64>,
    pub household_use_w: Option<f64>,
    pub timestamp: Option<i64>,
    pub issue: Option<String>,
}

impl InverterSnapshot {
    pub fn disabled() -> Self {
        Self::without_reading(InverterStatus::Disabled, None)
    }

    pub fn without_reading(status: InverterStatus, issue: Option<String>) -> Self {
        InverterSnapshot {
            status,
            pv_power_w: None,
            active_power_w: None,
            household_use_w: None,
            timestamp: None,
            issue,
        }
    }
}

/// The latest poll result of one inverter.
#[derive(Debug, Clone, Default)]
pub struct SungrowState {
    pub latest: Option<(Powers, i64)>,
    pub issue: Option<String>,
    pub transaction: u16,
}

impl SungrowState {
    pub fn next_transaction(&mut self) -> u16 {
        self.transaction = self.transaction.wrapping_add(1);
        self.transaction
    }

    pub fn record(&mut self, result: Result<Powers, String>, now: i64) {
        match result {
            Ok(powers) => {
                self.latest = Some((powers, now));
                self.issue = None;
            }
            Err(issue) => self.issue = Some(issue),
        }
    }

    pub fn snapshot(&self, enabled: bool, now: i64) -> InverterSnapshot {
        match self.latest {
            Some((powers, at)) if now >= at && now - at <= SUNGROW_STALE_MS => InverterSnapshot {
                status: InverterStatus::Online,
                pv_power_w: Some(powers.pv_power_w as f64),
                active_power_w: Some(powers.active_power_w as f64),
                household_use_w: None,
                timestamp: Some(at),
                issue: None,
            },
            _ if enabled => InverterSnapshot::without_reading(InverterStatus::Offline, self.issue.clone()),
            _ => InverterSnapshot::disabled(),
        }
    }
}

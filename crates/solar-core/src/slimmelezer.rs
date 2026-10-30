//! SlimmeLezer (ESPHome / esp-link) P1 telnet link state (port of `SlimmeLezerProvider`).
//!
//! The socket I/O runs on the service's meter thread; this type only holds the framing
//! parser, the latest reading and the diagnostics, so it can be driven from tests.

use crate::dsmr::{DsmrStream, StreamEvent};
use crate::reading::Reading;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

pub const CONNECT_TIMEOUT_MS: u64 = 3_000;
pub const IDLE_TIMEOUT_MS: u64 = 20_000;
pub const MIN_BACKOFF_MS: u64 = 1_000;
pub const MAX_BACKOFF_MS: u64 = 30_000;

/// Link diagnostics; a simulated meter reports only `simulated`, so missing fields default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MeterStats {
    pub simulated: bool,
    pub connected: bool,
    pub bytes: u64,
    pub telegrams: u64,
    pub errors: u64,
    pub reconnects: u64,
    pub last_issue: Option<String>,
    pub last_reading_at: Option<i64>,
    pub dsmr_version: Option<String>,
    pub p1_version: Option<String>,
    pub checksum: Option<Cow<'static, str>>,
    pub interval_ms: Option<i64>,
}

/// What the link reports to the runtime: a validated reading or a diagnostic that
/// invalidates the decision state.
#[derive(Debug, Clone, PartialEq)]
pub enum MeterEvent {
    Reading(Box<Reading>),
    Diagnostic,
}

#[derive(Debug)]
pub struct SlimmeLezer {
    parser: DsmrStream,
    latest: Option<Reading>,
    stats: MeterStats,
    backoff_ms: u64,
}

impl SlimmeLezer {
    pub fn new(allow_legacy_without_crc: bool) -> Self {
        SlimmeLezer {
            parser: DsmrStream::new(allow_legacy_without_crc),
            latest: None,
            stats: MeterStats::default(),
            backoff_ms: MIN_BACKOFF_MS,
        }
    }

    pub fn stats(&self) -> &MeterStats {
        &self.stats
    }

    fn handle(&mut self, events: Vec<StreamEvent>, out: &mut Vec<MeterEvent>) {
        for event in events {
            match event {
                StreamEvent::Reading(reading) => {
                    self.stats.telegrams += 1;
                    self.stats.interval_ms = self.stats.last_reading_at.map(|at| reading.timestamp - at);
                    self.stats.last_reading_at = Some(reading.timestamp);
                    self.stats.dsmr_version = reading.dsmr_version.clone();
                    self.stats.p1_version = reading.p1_version.clone();
                    self.stats.checksum = reading.checksum.map(Cow::Borrowed);
                    self.stats.last_issue = None;
                    self.backoff_ms = MIN_BACKOFF_MS;
                    self.latest = Some((*reading).clone());
                    out.push(MeterEvent::Reading(reading));
                }
                StreamEvent::Issue(code) => out.push(self.issue(code)),
            }
        }
    }

    /// Every issue drops the latest reading, so nothing stale is ever shown as live.
    pub fn issue(&mut self, code: &str) -> MeterEvent {
        self.latest = None;
        self.stats.errors += 1;
        self.stats.last_issue = Some(code.to_string());
        MeterEvent::Diagnostic
    }

    pub fn connected(&mut self) -> MeterEvent {
        self.stats.connected = true;
        MeterEvent::Diagnostic
    }

    pub fn feed(&mut self, bytes: &[u8], now: i64) -> Vec<MeterEvent> {
        self.stats.bytes += bytes.len() as u64;
        let events = self.parser.feed(bytes, now);
        let mut out = Vec::new();
        self.handle(events, &mut out);
        out
    }

    /// The socket closed; returns the events and, while running, the reconnect delay.
    pub fn closed(&mut self, running: bool) -> (Vec<MeterEvent>, Option<u64>) {
        self.stats.connected = false;
        self.latest = None;
        let mut events = Vec::new();
        self.parser.reset(running, &mut events);
        let mut out = Vec::new();
        self.handle(events, &mut out);
        out.push(MeterEvent::Diagnostic);
        if !running {
            return (out, None);
        }
        let delay = self.backoff_ms;
        self.backoff_ms = (self.backoff_ms * 2).min(MAX_BACKOFF_MS);
        self.stats.reconnects += 1;
        (out, Some(delay))
    }

    /// Expires a stuck partial frame, then returns the latest reading.
    pub fn latest(&mut self, now: i64) -> (Option<Reading>, Vec<MeterEvent>) {
        let mut events = Vec::new();
        self.parser.expire(now, &mut events);
        let mut out = Vec::new();
        self.handle(events, &mut out);
        (self.latest.clone(), out)
    }

    /// Replaces the latest reading directly, as if the parser had produced it.
    pub fn set_latest(&mut self, reading: Option<Reading>) {
        self.latest = reading;
    }
}

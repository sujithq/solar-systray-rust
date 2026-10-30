//! DSMR P1 telegram parser and byte framing (port of `dsmr.mjs`).

use crate::reading::{EnergyTotals, Quality, Reading};
use crate::{civil_from_days, days_from_civil};

pub const MAX_TELEGRAM_BYTES: usize = 16384;

/// DSMR CRC-16/ARC: initial 0, reflected polynomial 0xA001, through '!'.
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in bytes {
        crc ^= byte as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xa001 } else { crc >> 1 };
        }
    }
    crc
}

/// A rejected telegram; the code matches the Solar Agent diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError(pub &'static str);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone)]
pub struct ParseOptions<'a> {
    pub allow_legacy_without_crc: bool,
    pub timestamp: i64,
    pub source: &'a str,
}

impl Default for ParseOptions<'_> {
    fn default() -> Self {
        ParseOptions { allow_legacy_without_crc: false, timestamp: crate::now_ms(), source: "slimmelezer" }
    }
}

fn month_from_days(days: i64) -> i64 {
    let (year, month, _) = civil_from_days(days);
    year * 100 + month as i64
}

struct LocalStamp {
    unix_ms: i64,
    local_seconds: i64,
    offset_minutes: i64,
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// "YYMMDDhhmmssX": local time with S (summer, UTC+2) or W (winter, UTC+1).
fn local_stamp(stamp: &str) -> Option<LocalStamp> {
    let bytes = stamp.as_bytes();
    if bytes.len() != 13 || !all_digits(&stamp[..12]) || !matches!(bytes[12], b'S' | b'W') {
        return None;
    }
    let field = |i: usize| stamp[i..i + 2].parse::<u32>().unwrap();
    let (year, month, day, hour, minute, second) = (field(0), field(2), field(4), field(6), field(8), field(10));
    let full_year = 2000 + year as i64;
    let month_days = [31, if full_year % 4 == 0 { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(1..=12).contains(&month)
        || day < 1
        || day > month_days[month as usize - 1]
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let local_seconds =
        days_from_civil(full_year, month, day) * 86400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64;
    let offset_minutes = if bytes[12] == b'S' { 120 } else { 60 };
    Some(LocalStamp { unix_ms: (local_seconds - offset_minutes * 60) * 1000, local_seconds, offset_minutes })
}

/// `\d+(?:\.\d{1,6})?` as a whole string.
fn decimal(s: &str) -> Option<f64> {
    let (whole, fraction) = match s.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (s, None),
    };
    if !all_digits(whole) {
        return None;
    }
    if let Some(f) = fraction
        && (f.is_empty() || f.len() > 6 || !all_digits(f))
    {
        return None;
    }
    s.parse::<f64>().ok()
}

/// `(\d+(?:\.\d{1,6})?)\*(kW|W)` as a whole string, in watts (possibly infinite).
fn power_value(s: &str) -> Option<f64> {
    let (number, unit) = s.split_once('*')?;
    let scale = match unit {
        "kW" => 1000.0,
        "W" => 1.0,
        _ => return None,
    };
    Some(decimal(number)? * scale)
}

fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line))
}

fn lines_with_prefix<'a>(text: &'a str, prefix: &str) -> Vec<&'a str> {
    lines(text).filter(|line| line.starts_with(prefix)).collect()
}

/// Checks `^\/[^\r\n]+\r?\n([\s\S]*\r?\n)!([0-9a-fA-F]{4})?\r?\n$` and returns (body, crc hex).
fn framing(text: &str) -> Option<(&str, Option<&str>)> {
    let bytes = text.as_bytes();
    let header_break = bytes.iter().position(|&b| b == b'\r' || b == b'\n')?;
    if header_break < 2 {
        return None;
    }
    let body_start = match bytes[header_break] {
        b'\r' if bytes.get(header_break + 1) == Some(&b'\n') => header_break + 2,
        b'\n' => header_break + 1,
        _ => return None,
    };
    let mut tail = text.strip_suffix('\n')?;
    tail = tail.strip_suffix('\r').unwrap_or(tail);
    let (bang, crc) = if tail.ends_with('!') {
        (tail.len() - 1, None)
    } else if tail.len() >= 5
        && tail.as_bytes()[tail.len() - 5] == b'!'
        && tail[tail.len() - 4..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        (tail.len() - 5, Some(&tail[tail.len() - 4..]))
    } else {
        return None;
    };
    if bang <= body_start || bytes[bang - 1] != b'\n' {
        return None;
    }
    Some((&text[body_start..bang], crc))
}

fn version_line(line: &str) -> Option<(&'static str, &str)> {
    for obis in ["1-3:0.2.8", "0-0:96.1.4"] {
        if let Some(rest) = line.strip_prefix(obis) {
            let rest = rest.strip_prefix("*255").unwrap_or(rest);
            if rest.starts_with('(') {
                return Some((obis, rest));
            }
        }
    }
    None
}

/// Parses one complete telegram, from '/' through the CRC line.
pub fn parse_telegram(text: &str, options: &ParseOptions) -> Result<Reading, ParseError> {
    if !text.starts_with('/')
        || text.len() > MAX_TELEGRAM_BYTES
        || !text.bytes().all(|b| (0x20..=0x7e).contains(&b) || b == b'\r' || b == b'\n')
    {
        return Err(ParseError("encoding-or-framing"));
    }
    let (body, crc) = framing(text).ok_or(ParseError("framing"))?;
    let body_bytes = body.as_bytes();
    let line_start_delimiter = body_bytes.iter().enumerate().any(|(i, &b)| {
        (b == b'/' || b == b'!') && (i == 0 || body_bytes[i - 1] == b'\n' || body_bytes[i - 1] == b'\r')
    });
    if line_start_delimiter {
        return Err(ParseError("framing"));
    }

    let versions: Vec<(&'static str, &str)> = lines(text).filter_map(version_line).collect();
    if versions.len() > 1 {
        return Err(ParseError("duplicate-version"));
    }
    let mut p1_version: Option<String> = None;
    let mut version: Option<String> = None;
    if let Some((obis, rest)) = versions.first() {
        let digits = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')).unwrap_or("");
        let valid = all_digits(digits) && (digits.len() == 2 || digits.len() == 5);
        if !valid || (*obis == "1-3:0.2.8" && digits.len() != 2) {
            return Err(ParseError("version-format"));
        }
        // Fluvius e-MUCS section 3.3: XXXYY = DSMR XXX, e-MUCS YY.
        let significant = &digits[..if digits.len() == 5 { 3 } else { 2 }];
        version = Some(significant.chars().map(String::from).collect::<Vec<_>>().join("."));
        p1_version = Some(digits.to_string());
    }

    if let Some(hex) = crc {
        let end = text.rfind('!').unwrap() + 1;
        if crc16(&text.as_bytes()[..end]) != u16::from_str_radix(hex, 16).unwrap() {
            return Err(ParseError("checksum"));
        }
    } else {
        let legacy = p1_version.as_deref().is_some_and(|v| v.len() == 2 && v.parse::<u32>().unwrap() < 40);
        if !options.allow_legacy_without_crc || !legacy {
            return Err(ParseError("checksum-required"));
        }
    }

    let power = |obis: &str, optional: bool| -> Result<Option<f64>, ParseError> {
        let found = lines_with_prefix(text, &format!("{obis}("));
        if found.len() != 1 {
            if optional {
                return Ok(None);
            }
            return Err(ParseError(if found.is_empty() { "missing-power" } else { "duplicate-power" }));
        }
        let line = found[0];
        let open = line.find('(').unwrap();
        let value = line[open + 1..].strip_suffix(')').and_then(power_value);
        let Some(watts) = value else {
            return if optional { Ok(None) } else { Err(ParseError("power-format-or-unit")) };
        };
        if !watts.is_finite() || watts > 1_000_000.0 {
            return if optional { Ok(None) } else { Err(ParseError("power-range")) };
        }
        Ok(Some(watts))
    };

    let grid_import_w = power("1-0:1.7.0", false)?.unwrap();
    let grid_export_w = power("1-0:2.7.0", false)?.unwrap();
    let current_demand_w = power("1-0:1.4.0", true)?;
    let (monthly_peak_w, monthly_peak_at, monthly_peak_month) = monthly_peak(text);
    let clock = meter_clock(text);

    Ok(Reading {
        timestamp: options.timestamp,
        grid_import_w,
        grid_export_w,
        current_demand_w,
        monthly_peak_w,
        meter_time: clock.map(|c| c.unix_ms),
        meter_month: clock.map(|c| month_from_days(c.local_seconds.div_euclid(86400))),
        meter_day: clock.map(|c| c.local_seconds.div_euclid(86400)),
        meter_minute_of_day: clock.map(|c| c.local_seconds.rem_euclid(86400) / 60),
        meter_offset_minutes: clock.map(|c| c.offset_minutes),
        energy_totals_wh: energy_totals(text),
        monthly_peak_at,
        monthly_peak_month,
        source: options.source.to_string(),
        quality: Quality::Valid,
        dsmr_version: version,
        p1_version,
        checksum: Some(if crc.is_some() { "verified" } else { "legacy-unchecked" }),
    })
}

fn monthly_peak(text: &str) -> (Option<f64>, Option<i64>, Option<i64>) {
    let none = (None, None, None);
    let found = lines_with_prefix(text, "1-0:1.6.0(");
    if found.len() != 1 {
        return none;
    }
    let rest = &found[0]["1-0:1.6.0(".len()..];
    if rest.len() < 15 || !rest.is_char_boundary(13) {
        return none;
    }
    let (stamp, rest) = rest.split_at(13);
    let stamp_ok = all_digits(&stamp[..12]) && matches!(stamp.as_bytes()[12], b'S' | b'W');
    let Some(value) = rest.strip_prefix(")(").and_then(|r| r.strip_suffix(')')) else { return none };
    if !stamp_ok {
        return none;
    }
    let Some(watts) = power_value(value) else { return none };
    if !watts.is_finite() || watts > 1_000_000.0 {
        return none;
    }
    // The capture time is just after the end of the quarter-hour that set the peak, so the
    // peak belongs to the month of the quarter before the one holding the capture time.
    let Some(capture) = local_stamp(stamp) else { return (Some(watts), None, None) };
    let quarter_start = capture.local_seconds.div_euclid(900) * 900 - 900;
    (Some(watts), Some(capture.unix_ms), Some(month_from_days(quarter_start.div_euclid(86400))))
}

#[derive(Clone, Copy)]
struct Clock {
    unix_ms: i64,
    local_seconds: i64,
    offset_minutes: i64,
}

fn meter_clock(text: &str) -> Option<Clock> {
    let found = lines_with_prefix(text, "0-0:1.0.0(");
    if found.len() != 1 {
        return None;
    }
    let stamp = found[0]["0-0:1.0.0(".len()..].strip_suffix(')')?;
    let s = local_stamp(stamp)?;
    Some(Clock { unix_ms: s.unix_ms, local_seconds: s.local_seconds, offset_minutes: s.offset_minutes })
}

fn energy_totals(text: &str) -> Option<EnergyTotals> {
    let mut values = [0i64; 4];
    for (slot, obis) in ["1-0:1.8.1", "1-0:1.8.2", "1-0:2.8.1", "1-0:2.8.2"].iter().enumerate() {
        let prefix = format!("{obis}(");
        let found = lines_with_prefix(text, &prefix);
        if found.len() != 1 {
            return None;
        }
        let inner = found[0][prefix.len()..].strip_suffix("*kWh)")?;
        let (whole, fraction) = inner.split_once('.')?;
        if !all_digits(whole) || whole.len() > 6 || fraction.len() != 3 || !all_digits(fraction) {
            return None;
        }
        values[slot] = whole.parse::<i64>().ok()? * 1000 + fraction.parse::<i64>().ok()?;
    }
    Some(EnergyTotals {
        import_tariff1_wh: values[0],
        import_tariff2_wh: values[1],
        export_tariff1_wh: values[2],
        export_tariff2_wh: values[3],
    })
}

/// What the byte stream produced: a validated reading or a diagnostic code.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Reading(Box<Reading>),
    Issue(&'static str),
}

/// Byte-oriented framing that bounds storage and exposes only validated measurements.
#[derive(Debug, Clone)]
pub struct DsmrStream {
    allow_legacy_without_crc: bool,
    max_frame_age_ms: i64,
    buffer: Vec<u8>,
    started_at: i64,
    ending: bool,
    header_complete: bool,
}

impl DsmrStream {
    pub fn new(allow_legacy_without_crc: bool) -> Self {
        DsmrStream {
            allow_legacy_without_crc,
            max_frame_age_ms: 15_000,
            buffer: Vec::with_capacity(MAX_TELEGRAM_BYTES),
            started_at: 0,
            ending: false,
            header_complete: false,
        }
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn reset(&mut self, report_incomplete: bool, events: &mut Vec<StreamEvent>) {
        if report_incomplete && !self.buffer.is_empty() {
            events.push(StreamEvent::Issue("incomplete"));
        }
        self.buffer.clear();
        self.ending = false;
        self.header_complete = false;
    }

    pub fn expire(&mut self, now: i64, events: &mut Vec<StreamEvent>) {
        if !self.buffer.is_empty() && now - self.started_at > self.max_frame_age_ms {
            self.reset(false, events);
            events.push(StreamEvent::Issue("frame-timeout"));
        }
    }

    pub fn feed(&mut self, chunk: &[u8], now: i64) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        self.expire(now, &mut events);
        for &byte in chunk {
            let line_start = self.buffer.last().is_none_or(|&b| b == b'\n');
            if byte == b'/' && (line_start || !self.header_complete) {
                self.reset(true, &mut events);
                self.started_at = now;
                self.buffer.push(byte);
                continue;
            }
            if self.buffer.is_empty() {
                continue;
            }
            if (byte < 32 && byte != b'\n' && byte != b'\r') || byte > 126 {
                self.reset(false, &mut events);
                events.push(StreamEvent::Issue("encoding"));
                continue;
            }
            if self.buffer.len() == MAX_TELEGRAM_BYTES {
                self.reset(false, &mut events);
                events.push(StreamEvent::Issue("oversized"));
                continue;
            }
            self.buffer.push(byte);
            if byte == b'!' && line_start {
                self.ending = true;
            }
            if byte == b'\n' {
                self.header_complete = true;
            }
            if byte == b'\n' && self.ending {
                let text = String::from_utf8_lossy(&self.buffer).into_owned();
                self.reset(false, &mut events);
                let options = ParseOptions {
                    allow_legacy_without_crc: self.allow_legacy_without_crc,
                    timestamp: now,
                    source: "slimmelezer",
                };
                match parse_telegram(&text, &options) {
                    Ok(reading) => events.push(StreamEvent::Reading(Box::new(reading))),
                    Err(error) => events.push(StreamEvent::Issue(error.0)),
                }
            }
        }
        events
    }
}

/// Synthetic DSMR 5 telegram without meter identifiers, addresses or cumulative usage.
pub fn synthetic_telegram(import_w: f64, export_w: f64, extra_fields: &str) -> String {
    let extra = if extra_fields.is_empty() {
        String::new()
    } else {
        let trimmed =
            extra_fields.strip_suffix('\n').map(|s| s.strip_suffix('\r').unwrap_or(s)).unwrap_or(extra_fields);
        format!("{trimmed}\r\n")
    };
    let body = format!(
        "/SOLAR-SYNTHETIC\r\n\r\n1-3:0.2.8(50)\r\n1-0:1.7.0({:.3}*kW)\r\n1-0:2.7.0({:.3}*kW)\r\n{extra}!",
        import_w / 1000.0,
        export_w / 1000.0
    );
    format!("{body}{:04X}\r\n", crc16(body.as_bytes()))
}

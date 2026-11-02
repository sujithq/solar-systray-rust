//! What the tray shows: the device info screen as readable sections (port of
//! `solar-info-lines.js`), plus a compact tooltip.

use crate::decision::{Level, State};
use crate::js_round;
use crate::runtime::Snapshot;
use crate::sun::Sun;
use crate::sungrow::InverterStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Text,
    Muted,
    Good,
    Warn,
    Bad,
    Accent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub value: String,
    pub tone: Tone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: &'static str,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Headline {
    pub title: String,
    pub reason: String,
    pub tone: Tone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub simulated: bool,
    pub headline: Headline,
    pub sections: Vec<Section>,
}

impl Info {
    pub fn section(&self, title: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.title == title)
    }
}

impl Section {
    pub fn value(&self, label: &str) -> Option<&str> {
        self.rows.iter().find(|r| r.label == label).map(|r| r.value.as_str())
    }
}

pub fn forecast_reason_text(reason: &str) -> &'static str {
    match reason {
        "ok" => "available",
        "offline" => "no meter reading",
        "invalid-input" => "invalid meter data",
        "no-meter-time" => "no meter clock",
        "no-demand" => "no demand reading",
        "no-peak" => "no monthly peak",
        "no-peak-time" => "no peak time",
        "previous-month-peak" => "peak is last month's",
        "too-early" => "quarter just started",
        "locked-in" => "quarter locked in",
        "peak-reached" => "peak already reached",
        "implausible-demand" => "demand not as expected",
        "clock-jump" => "meter clock jumped",
        "ha-meter-data-disconnected" => "Home Assistant disconnected",
        "ha-meter-data-needs-atomic-grid" => "atomic meter clock required",
        "ha-meter-data-grid-unavailable" => "atomic meter data unavailable",
        "ha-meter-clock-unsupported" => "unsupported meter date",
        "ha-meter-data-not-mapped" => "meter entity not mapped",
        "ha-meter-data-invalid" => "invalid meter entity",
        "ha-meter-data-no-acquisition-evidence" => "unchanged HA value; no acquisition evidence",
        "ha-atomic-optional-not-published" => "atomic publisher has no optional-field contract",
        "ha-atomic-optional-missing" => "required field missing from this telegram",
        "ha-atomic-optional-invalid" => "invalid atomic optional field/status",
        "ha-atomic-optional-previous-month" => "atomic peak belongs to a previous month",
        "ha-meter-data-unavailable" => "meter entity unavailable",
        "ha-meter-data-stale" => "meter entity state-change is stale",
        "ha-meter-data-event-proximity" => "outside HA event-proximity window",
        "ha-meter-data-month-boundary" => "peak belongs to another meter month",
        "ha-meter-data-day-boundary" => "counter event belongs to another meter day",
        _ => "unavailable",
    }
}

pub fn daily_reason_text(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "stale" => "the meter reading is old",
        "missing-reading" => "no meter reading yet",
        "missing-meter-time" => "the meter sends no clock",
        "missing-or-invalid-totals" => "the meter sends no energy totals",
        "clock-discontinuity" => "the meter clock changed; totals restart tomorrow",
        "counter-decreased" => "a meter total went down; totals restart tomorrow",
        "ha-meter-data-disconnected" => "Home Assistant is disconnected",
        "ha-meter-data-needs-atomic-grid" => "an atomic meter sample is required",
        "ha-meter-data-grid-unavailable" => "the atomic meter sample is unavailable",
        "ha-meter-clock-unsupported" => "the meter timestamp is outside the supported calendar",
        "ha-meter-data-not-mapped" => "one or more energy counters are not mapped",
        "ha-meter-data-invalid" => "an energy counter is invalid",
        "ha-meter-data-no-acquisition-evidence" => {
            "unchanged counters have no acquisition evidence; atomic samples are required"
        }
        "ha-atomic-optional-not-published" => "atomic publisher has no optional-field contract",
        "ha-atomic-optional-missing" => "one or more counters are missing from this telegram",
        "ha-atomic-optional-invalid" => "invalid atomic counter/status",
        "ha-meter-data-unavailable" => "an energy counter is unavailable",
        "ha-meter-data-stale" => "an energy counter state-change is stale",
        "ha-meter-data-event-proximity" => "counter events are outside the HA event-proximity window",
        "ha-meter-data-day-boundary" => "counter event does not match the meter day",
        _ => return None,
    })
}

/// JavaScript `Number.prototype.toFixed`: exact ties round away from zero.
pub fn to_fixed(value: f64, digits: usize) -> String {
    let sign = if value < 0.0 { "-" } else { "" };
    let abs = value.abs();
    let scaled = abs * 2f64.powi(digits as i32 + 1);
    let tie = scaled.fract() == 0.0 && scaled % 2.0 == 1.0;
    let abs = if tie { abs + 0.5 / 10f64.powi(digits as i32) } else { abs };
    format!("{sign}{abs:.digits$}")
}

pub fn watts(value: f64) -> String {
    format!("{} W", js_round(value) as i64)
}

pub fn kilowatts(value: f64) -> String {
    format!("{} kW", to_fixed(value / 1000.0, 1))
}

pub fn kilowatt_hours(wh: i64) -> String {
    format!("{}.{:03} kWh", wh.div_euclid(1000), wh.rem_euclid(1000))
}

pub fn countdown(seconds: f64) -> String {
    let whole = seconds.ceil().max(0.0) as i64;
    format!("{}:{:02}", whole / 60, whole % 60)
}

pub fn minute_text(minute: i64) -> String {
    format!("{:02}:{:02}", minute.div_euclid(60) % 24, minute.rem_euclid(60))
}

/// Local wall-clock HH:MM, or `--:--`.
pub fn clock(time: Option<i64>) -> String {
    use chrono::{Local, TimeZone};
    time.and_then(|t| Local.timestamp_millis_opt(t).single())
        .map_or_else(|| "--:--".into(), |t| t.format("%H:%M").to_string())
}

fn row(label: impl Into<String>, value: impl Into<String>, tone: Tone) -> Row {
    Row { label: label.into(), value: value.into(), tone }
}

fn live(s: &Snapshot) -> bool {
    s.evaluation.state != State::Offline && s.evaluation.surplus_w.is_some()
}

/// The headline and reason, decided as on the device.
pub fn headline(s: &Snapshot) -> Headline {
    let e = &s.evaluation;
    let make = |title: String, reason: String, tone| Headline { title, reason, tone };
    let Some(export_w) = e.surplus_w.filter(|_| e.state != State::Offline) else {
        return make("METER OFFLINE".into(), "No recent meter reading".into(), Tone::Bad);
    };
    if e.safe_to_start {
        return make("GO - START NOW".into(), "Surplus is stable".into(), Tone::Good);
    }
    if e.level == Level::Night {
        return make("NIGHT".into(), "Sun is down".into(), Tone::Muted);
    }
    if e.state == State::Importing {
        let reason = format!("Importing {} from grid", watts(e.grid_import_w.unwrap_or(0.0)));
        return make("IMPORTING".into(), reason, Tone::Bad);
    }
    let need = format!("Need {} export, have {}", watts(e.start_threshold_w), watts(export_w));
    if e.state == State::Limited && export_w > e.start_threshold_w {
        let total = s.stable_duration_seconds;
        let stable = e.stable_seconds.min(total);
        return make(
            format!("GO IN {}", countdown(total - stable)),
            format!("Stable for {} of {}", countdown(stable), countdown(total)),
            Tone::Warn,
        );
    }
    if e.state == State::Limited {
        return make("WAIT FOR SURPLUS".into(), need, Tone::Warn);
    }
    make("NO SURPLUS".into(), need, Tone::Muted)
}

fn power_rows(s: &Snapshot) -> Vec<Row> {
    let e = &s.evaluation;
    let mut rows = if live(s) {
        vec![
            row("Grid export", watts(e.surplus_w.unwrap_or(0.0)), Tone::Text),
            row("Grid import", watts(e.grid_import_w.unwrap_or(0.0)), Tone::Text),
        ]
    } else {
        vec![row("Grid export", "-- W", Tone::Muted), row("Grid import", "-- W", Tone::Muted)]
    };
    let inverter = &s.inverter;
    let household = inverter.household_use_w.filter(|w| w.is_finite());
    if let Some(ha) = &s.home_assistant {
        let ac = match (ha.active_power_entity.is_some(), inverter.active_power_w) {
            (_, Some(ac)) => (kilowatts(ac), Tone::Text),
            (false, None) => ("Not mapped".into(), Tone::Muted),
            (true, None) => ("Unavailable or stale".into(), Tone::Warn),
        };
        rows.push(row("Inverter output (AC)", ac.0, ac.1));
        let dc = match (ha.dc_power_entity.is_some(), inverter.pv_power_w) {
            (_, Some(dc)) => (kilowatts(dc), Tone::Text),
            (false, None) => ("Not mapped".into(), Tone::Muted),
            (true, None) => ("Unavailable or stale".into(), Tone::Warn),
        };
        rows.push(row("Solar panels (DC)", dc.0, dc.1));
        let household_row = household.map_or_else(
            || {
                let issue = ha.household_use_issue.as_deref();
                let tone = if issue == Some("ha-household-disabled") { Tone::Muted } else { Tone::Warn };
                (household_unavailable(issue).to_string(), tone)
            },
            |watts| (kilowatts(watts), Tone::Text),
        );
        rows.push(row("Household use", household_row.0, household_row.1));
    } else {
        match (inverter.status, inverter.pv_power_w) {
            (InverterStatus::Disabled, _) => {}
            (InverterStatus::Online, Some(pv)) => {
                rows.push(row("Solar panels (PV)", kilowatts(pv), Tone::Text));
                if let Some(ac) = inverter.active_power_w {
                    rows.push(row("Inverter output (AC)", kilowatts(ac), Tone::Text));
                }
            }
            (InverterStatus::Standby, _) => rows.push(row("Solar panels (PV)", "No reading (night)", Tone::Muted)),
            _ => rows.push(row("Solar panels (PV)", "Sungrow unavailable", Tone::Warn)),
        }
    }
    if s.home_assistant.is_none()
        && let Some(household) = household
    {
        rows.push(row("Household use", kilowatts(household), Tone::Text));
    }
    if !s.simulated {
        let baseline = s.night_baseline_w.filter(|w| w.is_finite());
        if let Some(baseline) = baseline {
            match household {
                Some(household) => {
                    let above = js_round(household - baseline) as i64;
                    let value = format!("{}{above} W", if above > 0 { "+" } else { "" });
                    rows.push(row("Extra vs night", value, if above >= 300 { Tone::Warn } else { Tone::Muted }));
                }
                None if s.home_assistant.is_some() => rows.push(row(
                    "Extra vs night",
                    household_unavailable(s.home_assistant.as_ref().and_then(|ha| ha.household_use_issue.as_deref())),
                    Tone::Muted,
                )),
                None => rows.push(row("Extra vs night", "Unavailable (no inverter reading)", Tone::Muted)),
            }
        }
        let text = baseline.map_or_else(|| "Unavailable".to_string(), watts);
        let state = if let Some(history) = &s.night_baseline_history {
            if history.baseline.is_some() {
                format!("{text} (recorded night)")
            } else {
                format!("{text} default; no valid recorded night")
            }
        } else if s.night_baseline_learned {
            format!("{text} (learned)")
        } else if !s.location_configured {
            format!("{text} default; set location to learn")
        } else if e.sun == Sun::Down {
            format!("{text} default; learning overnight")
        } else {
            format!("{text} default; waiting for overnight data")
        };
        rows.push(row("Night baseline", state, Tone::Muted));
        if let Some(history) = &s.night_baseline_history {
            if let Some(issue) = &history.issue {
                rows.push(row("Baseline status", issue, Tone::Warn));
            }
            if let Some(baseline) = &history.baseline {
                let age_hours = (s.checked_at - baseline.window.end).max(0) as f64 / 3_600_000.0;
                rows.push(row(
                    "Baseline age",
                    format!(
                        "{age_hours:.1}h since night end{}",
                        if age_hours > 36.0 { " (stale; retained)" } else { "" }
                    ),
                    if age_hours > 36.0 { Tone::Warn } else { Tone::Muted },
                ));
                let utc = |at| {
                    chrono::DateTime::from_timestamp_millis(at)
                        .map(|time| time.format("%Y-%m-%d %H:%M UTC").to_string())
                        .unwrap_or_else(|| "Invalid date".into())
                };
                rows.push(row(
                    "Baseline night",
                    format!("{} - {}", utc(baseline.window.start), utc(baseline.window.end)),
                    Tone::Muted,
                ));
                rows.push(row("Baseline source", &baseline.provenance, Tone::Muted));
                rows.push(row("Baseline samples", format!("{} actual minute samples", baseline.samples), Tone::Muted));
            }
        }
    }
    rows
}

fn household_unavailable(issue: Option<&str>) -> &'static str {
    match issue {
        Some("ha-household-disabled") => "Unavailable (disabled by configuration)",
        Some("ha-household-event-skew") => "Unavailable (HA event proximity gate)",
        Some("ha-household-ac-stale") => "Unavailable (AC last-known state is stale)",
        Some("ha-household-awaiting-live-ac") => "Unavailable (awaiting live AC value change)",
        Some("ha-household-out-of-range") => "Unavailable (derived value out of range)",
        Some("ha-household-topology" | "ha-household-mapping") => "Unavailable (topology/sign configuration required)",
        Some("ha-household-disconnected" | "ha-household-grid-unavailable" | "ha-household-ac-unavailable") => {
            "Unavailable (required input is stale or unavailable)"
        }
        _ => "Unavailable (HA household-use gates not met)",
    }
}

fn peak_rows(s: &Snapshot) -> Vec<Row> {
    let e = &s.evaluation;
    let mut rows = Vec::new();
    if live(s)
        && let Some(demand) = e.current_demand_w
    {
        let tone = if e.monthly_peak_reached { Tone::Warn } else { Tone::Text };
        rows.push(row("Quarter-hour demand", kilowatts(demand), tone));
    }
    if live(s)
        && let Some(peak) = e.monthly_peak_w
    {
        let tone = if e.monthly_peak_reached { Tone::Warn } else { Tone::Text };
        rows.push(row("Monthly peak", kilowatts(peak), tone));
    }
    let f = &s.peak_forecast;
    if live(s) {
        match (f.available, f.projected_w, f.comparison_w, f.seconds_left) {
            (true, Some(projected), Some(comparison), Some(left)) => {
                let kind = if f.comparison_is_minimum { "min" } else { "peak" };
                let tone = if f.warning { Tone::Warn } else { Tone::Text };
                rows.push(row(
                    "Forecast",
                    format!("{} / {kind} {}", kilowatts(projected), kilowatts(comparison)),
                    tone,
                ));
                if f.warning {
                    let reduce = f.reduce_w.unwrap_or(0.0);
                    rows.push(row(
                        "Reduce import",
                        format!("{}, {} left", kilowatts(reduce), countdown(left)),
                        Tone::Warn,
                    ));
                } else {
                    rows.push(row("Quarter ends in", countdown(left), Tone::Muted));
                }
            }
            _ => rows.push(row("Forecast", forecast_reason_text(&f.reason), Tone::Muted)),
        }
    }
    if rows.is_empty() {
        rows.push(row("Peak", "No meter reading", Tone::Muted));
    }
    rows
}

fn today_rows(s: &Snapshot) -> Vec<Row> {
    let d = &s.daily_energy;
    match (live(s) && d.available, d.import_wh, d.export_wh, d.since_minute) {
        (true, Some(import), Some(export), Some(since)) => {
            let mut rows = Vec::new();
            if s.home_assistant.is_some() {
                rows.push(row("Coverage", "Partial today; baseline starts during this run", Tone::Warn));
            }
            rows.extend([
                row("Imported", kilowatt_hours(import), Tone::Text),
                row("Exported", kilowatt_hours(export), Tone::Text),
                row(
                    "Since",
                    format!(
                        "{} ({})",
                        minute_text(since),
                        if s.home_assistant.is_some() { "meter local time" } else { "meter time" }
                    ),
                    Tone::Muted,
                ),
            ]);
            rows
        }
        _ => {
            let mut rows = vec![row("Daily energy", "Unavailable", if live(s) { Tone::Warn } else { Tone::Muted })];
            if let Some(why) = daily_reason_text(&d.reason) {
                rows.push(row("Why", why, Tone::Muted));
            }
            rows
        }
    }
}

fn connection_rows(s: &Snapshot) -> Vec<Row> {
    let mut rows = vec![row("Sun", format!("Sunrise {} | Sunset {}", clock(s.sunrise), clock(s.sunset)), Tone::Muted)];
    if let Some(ha) = &s.home_assistant {
        rows.push(row("Source", "Home Assistant (WebSocket only; no fallback)", Tone::Text));
        rows.push(row(
            "HA transport",
            if ha.connected { "Authenticated" } else { "Disconnected" },
            if ha.connected { Tone::Good } else { Tone::Bad },
        ));
        rows.push(row(
            "Initialization",
            if ha.initialized { "Subscribed and initialized" } else { "Awaiting auth/subscription/snapshot" },
            Tone::Muted,
        ));
        rows.push(row("Evidence", &ha.evidence, Tone::Warn));
        rows.push(row(
            "Meter calendar",
            "Atomic UTC timestamp converted to Europe/Brussels local time (DST-aware)",
            Tone::Muted,
        ));
        rows.push(row(
            "Optional meter evidence",
            "Same accepted atomic telegram; missing/invalid fields never carried forward",
            Tone::Warn,
        ));
        if let Some(issue) = &s.diagnostics.last_issue {
            rows.push(row("Grid status", issue, Tone::Warn));
        }
        if let Some(issue) = &ha.peak_forecast_issue {
            rows.push(row("Quarter-hour data", issue, Tone::Warn));
        }
        if let Some(issue) = &ha.daily_energy_issue {
            rows.push(row("Daily energy data", issue, Tone::Warn));
        }
        for entity in &ha.entities {
            let value = entity.issue.clone().unwrap_or_else(|| match entity.age_ms {
                Some(ms) if entity.sample_sequence.is_some() => format!("Telegram age {:.1}s", ms as f64 / 1000.0),
                Some(ms) => format!("Last-known HA state-change age {:.1}s", ms as f64 / 1000.0),
                None => "Unavailable".into(),
            });
            rows.push(row(&entity.entity, value, if entity.issue.is_some() { Tone::Warn } else { Tone::Muted }));
            if let Some(sequence) = entity.sample_sequence {
                rows.push(row("Atomic sample sequence", sequence.to_string(), Tone::Muted));
            }
        }
        if ha.active_power_entity.is_some() || ha.dc_power_entity.is_some() {
            rows.push(row(
                "Inverter freshness",
                "Last-known HA state-change only; acquisition timestamp unavailable",
                Tone::Warn,
            ));
        }
        let household_status = if !ha.household_use_enabled {
            "Disabled by default; explicit topology/sign opt-in required"
        } else if ha.household_use_issue.is_none() {
            "Enabled; provisional HA event proximity gate passed, physical coherence unverified"
        } else {
            household_unavailable(ha.household_use_issue.as_deref())
        };
        rows.push(row(
            "Household calculation",
            household_status,
            if ha.household_use_enabled { Tone::Warn } else { Tone::Muted },
        ));
        return rows;
    }
    match &s.meter_address {
        None => rows.push(row("Meter", "Simulated", Tone::Muted)),
        Some(address) => {
            let d = &s.diagnostics;
            let (state, tone) = if live(s) {
                ("live".to_string(), Tone::Good)
            } else if d.connected {
                ("connected, no valid telegram".to_string(), Tone::Warn)
            } else {
                (d.last_issue.as_ref().map_or_else(|| "connecting".to_string(), |i| format!("offline: {i}")), Tone::Bad)
            };
            rows.push(row("Meter", format!("{address} ({state})"), tone));
            if let Some(version) = &d.dsmr_version {
                rows.push(row("Telegrams", format!("DSMR {version}, {} received", d.telegrams), Tone::Muted));
            }
        }
    }
    if let Some(address) = &s.inverter_address {
        let (state, tone) = match (s.inverter.status, &s.inverter.issue) {
            (InverterStatus::Online, _) => ("online".to_string(), Tone::Good),
            (_, Some(issue)) => (format!("offline: {issue}"), Tone::Warn),
            _ => ("waiting for first poll".to_string(), Tone::Muted),
        };
        rows.push(row("Inverter", format!("{address} ({state})"), tone));
    }
    rows
}

pub fn solar_info(s: &Snapshot) -> Info {
    let e = &s.evaluation;
    let appliance = vec![
        row(
            if e.appliance_name.is_empty() { "Appliance" } else { &e.appliance_name },
            watts(e.appliance_peak_w),
            Tone::Accent,
        ),
        row("Starts at", format!("{} export", watts(e.start_threshold_w)), Tone::Muted),
    ];
    Info {
        simulated: s.simulated,
        headline: headline(s),
        sections: vec![
            Section { title: "Power now", rows: power_rows(s) },
            Section { title: "Appliance", rows: appliance },
            Section { title: "Quarter-hour peak", rows: peak_rows(s) },
            Section { title: "Today", rows: today_rows(s) },
            Section { title: "Connections", rows: connection_rows(s) },
        ],
    }
}

/// The notification-area tooltip limit (128 UTF-16 units including the terminator).
pub const TOOLTIP_MAX: usize = 127;

/// A compact multi-line summary that fits the tooltip; later lines are dropped first.
pub fn tooltip(s: &Snapshot) -> String {
    let e = &s.evaluation;
    let head = headline(s);
    let mut lines = Vec::new();
    let sim = if s.simulated { " [SIM]" } else { "" };
    lines.push(format!("{}{sim}", head.title));
    if live(s) {
        let mut power = match e.grid_import_w {
            Some(import) if import > 0.0 => format!("Import {}", watts(import)),
            _ => format!("Export {}", watts(e.surplus_w.unwrap_or(0.0))),
        };
        if s.home_assistant.is_some() {
            if let Some(ac) = s.inverter.active_power_w {
                power.push_str(&format!(" | AC {}", kilowatts(ac)));
            }
            if let Some(dc) = s.inverter.pv_power_w {
                power.push_str(&format!(" | DC {}", kilowatts(dc)));
            }
        } else if let (InverterStatus::Online, Some(pv)) = (s.inverter.status, s.inverter.pv_power_w) {
            power.push_str(&format!(" | PV {}", kilowatts(pv)));
        }
        lines.push(power);
        if !e.safe_to_start && head.title != "IMPORTING" {
            lines.push(head.reason.clone());
        } else {
            lines.push(format!("{} needs {} export", e.appliance_name, watts(e.start_threshold_w)));
        }
        let f = &s.peak_forecast;
        if e.monthly_peak_reached {
            lines.push("Monthly peak reached".into());
        } else if f.warning {
            lines.push(format!(
                "Peak! Reduce {}, {} left",
                kilowatts(f.reduce_w.unwrap_or(0.0)),
                countdown(f.seconds_left.unwrap_or(0.0))
            ));
        } else if let (true, Some(projected), Some(comparison)) = (f.available, f.projected_w, f.comparison_w) {
            lines.push(format!("Quarter {} / {}", kilowatts(projected), kilowatts(comparison)));
        }
        let d = &s.daily_energy;
        if let (true, Some(import), Some(export)) = (d.available, d.import_wh, d.export_wh) {
            lines.push(format!(
                "{} in {} / out {}",
                if s.home_assistant.is_some() { "Partial today" } else { "Today" },
                kilowatt_hours(import).trim_end_matches(" kWh"),
                kilowatt_hours(export)
            ));
        }
    } else {
        lines.push(head.reason);
        if let Some(issue) = &s.diagnostics.last_issue {
            lines.push(format!("Last issue: {issue}"));
        }
    }
    fit_tooltip(lines)
}

/// Joins lines and drops trailing ones (then truncates) to fit [`TOOLTIP_MAX`] UTF-16 units.
pub fn fit_tooltip(mut lines: Vec<String>) -> String {
    let units = |s: &str| s.encode_utf16().count();
    loop {
        let text = lines.join("\n");
        if units(&text) <= TOOLTIP_MAX {
            return text;
        }
        if lines.len() > 1 {
            lines.pop();
            continue;
        }
        let mut out = String::new();
        for c in text.chars() {
            if units(&out) + c.len_utf16() > TOOLTIP_MAX - 3 {
                break;
            }
            out.push(c);
        }
        out.push_str("...");
        return out;
    }
}

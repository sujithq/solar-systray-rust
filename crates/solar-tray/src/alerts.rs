//! Turns snapshot changes into notifications. Alerts fire on edges (GO gained, a new peak
//! warning, ...), never repeatedly for a steady state, and the first snapshot only sets the
//! baseline so starting the tray does not announce the current state.

use crate::settings::Notifications;
use crate::view::live;
use solar_core::Snapshot;
use solar_core::present::{countdown, headline, kilowatts, watts};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    GoGained,
    GoLost,
    PeakForecast,
    MonthlyPeak,
    MeterOffline,
}

impl Kind {
    pub const ALL: [Kind; 5] =
        [Kind::GoGained, Kind::GoLost, Kind::PeakForecast, Kind::MonthlyPeak, Kind::MeterOffline];

    pub fn label(self) -> &'static str {
        match self {
            Kind::GoGained => "GO: safe to start",
            Kind::GoLost => "GO ended",
            Kind::PeakForecast => "Quarter-hour peak warning",
            Kind::MonthlyPeak => "Monthly peak reached",
            Kind::MeterOffline => "Meter offline / back online",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub kind: Kind,
    pub title: String,
    pub body: String,
    pub warning: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen {
    go: bool,
    peak_warning: bool,
    peak_reached: bool,
}

#[derive(Debug, Default)]
pub struct Alerts {
    seen: Option<Seen>,
    offline_since: Option<i64>,
    offline_reported: bool,
}

fn duration(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as i64;
    if seconds < 90 { format!("{seconds} s") } else { format!("{} min", (seconds + 30) / 60) }
}

fn alert(kind: Kind, title: impl Into<String>, body: impl Into<String>, warning: bool) -> Alert {
    Alert { kind, title: title.into(), body: body.into(), warning }
}

impl Alerts {
    /// Alerts for this snapshot in priority order, filtered by the notification settings.
    /// State is tracked even for disabled kinds, so enabling one later does not replay old edges.
    pub fn update(&mut self, s: &Snapshot, notifications: &Notifications, offline_after_seconds: u64) -> Vec<Alert> {
        let e = &s.evaluation;
        let live = live(s);
        let now = Seen {
            go: live && e.safe_to_start,
            peak_warning: live && s.peak_forecast.warning,
            peak_reached: live && e.monthly_peak_reached,
        };
        let mut out = Vec::new();

        if live {
            if self.offline_reported {
                let head = headline(s);
                out.push(alert(
                    Kind::MeterOffline,
                    "Meter back online",
                    format!("{}: {}", head.title, head.reason),
                    false,
                ));
            }
            self.offline_since = None;
            self.offline_reported = false;
        } else {
            let since = *self.offline_since.get_or_insert(s.checked_at);
            let offline_ms = s.checked_at - since;
            if !self.offline_reported && offline_ms >= offline_after_seconds as i64 * 1000 {
                self.offline_reported = true;
                let issue = s.diagnostics.last_issue.as_ref().map_or_else(String::new, |i| format!(" ({i})"));
                let body = format!("No meter reading for {}{issue}.", duration(offline_ms as f64 / 1000.0));
                out.push(alert(Kind::MeterOffline, "Meter offline", body, true));
            }
        }

        if let Some(before) = self.seen.replace(now) {
            let f = &s.peak_forecast;
            if now.peak_warning && !before.peak_warning {
                let reduce = kilowatts(f.reduce_w.unwrap_or(0.0));
                let left = countdown(f.seconds_left.unwrap_or(0.0));
                let limit = kilowatts(f.comparison_w.unwrap_or(0.0));
                let body = if f.comparison_is_minimum {
                    format!("Reduce import by {reduce} within {left} to stay under the {limit} minimum.")
                } else {
                    format!("Reduce import by {reduce} within {left} to stay under this month's peak of {limit}.")
                };
                out.push(alert(Kind::PeakForecast, "Quarter-hour peak warning", body, true));
            }
            if now.peak_reached && !before.peak_reached {
                let demand = kilowatts(e.current_demand_w.unwrap_or(0.0));
                let peak = kilowatts(e.monthly_peak_w.unwrap_or(0.0));
                let body = format!(
                    "Quarter-hour demand {demand} has reached this month's peak of {peak}; more import raises it."
                );
                out.push(alert(Kind::MonthlyPeak, "Monthly peak reached", body, true));
            }
            if now.go && !before.go {
                let body = format!(
                    "Exporting {}, stable for {}. Needs {} export.",
                    watts(e.surplus_w.unwrap_or(0.0)),
                    duration(e.stable_seconds),
                    watts(e.start_threshold_w)
                );
                out.push(alert(
                    Kind::GoGained,
                    format!("GO: start the {}", e.appliance_name.to_lowercase()),
                    body,
                    false,
                ));
            }
            if before.go && !now.go {
                let head = headline(s);
                out.push(alert(Kind::GoLost, "GO ended", format!("{}: {}", head.title, head.reason), false));
            }
        }

        let prefix = if s.simulated { "[SIM] " } else { "" };
        out.into_iter()
            .filter(|a| notifications.allows(a.kind))
            .map(|mut a| {
                a.title.insert_str(0, prefix);
                a
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solar_core::decision::{Level, State};
    use solar_core::peak_forecast::Forecast;
    use solar_core::{Runtime, SolarConfig};

    const NOW: i64 = 1_800_000_000_000;

    fn live_snapshot(at: i64, change: impl FnOnce(&mut Snapshot)) -> Snapshot {
        let mut s = Runtime::new(SolarConfig::default()).snapshot(at);
        s.simulated = false;
        s.checked_at = at;
        let e = &mut s.evaluation;
        e.state = State::Good;
        e.level = Level::Surplus;
        e.surplus_w = Some(2300.0);
        e.grid_import_w = Some(0.0);
        e.safe_to_start = false;
        e.stable_seconds = 30.0;
        e.start_threshold_w = 2150.0;
        e.appliance_name = "Dishwasher".into();
        e.current_demand_w = Some(1200.0);
        e.monthly_peak_w = Some(3600.0);
        e.monthly_peak_reached = false;
        s.peak_forecast = Forecast::unavailable("too-early");
        change(&mut s);
        s
    }

    fn offline_snapshot(at: i64) -> Snapshot {
        live_snapshot(at, |s| {
            s.evaluation.state = State::Offline;
            s.evaluation.surplus_w = None;
            s.diagnostics.last_issue = Some("ECONNREFUSED".into());
        })
    }

    fn go(at: i64) -> Snapshot {
        live_snapshot(at, |s| {
            s.evaluation.safe_to_start = true;
            s.evaluation.stable_seconds = 300.0;
        })
    }

    fn all() -> Notifications {
        Notifications::default()
    }

    #[test]
    fn the_first_snapshot_only_sets_the_baseline() {
        let mut alerts = Alerts::default();
        assert!(alerts.update(&go(NOW), &all(), 120).is_empty());
        assert!(alerts.update(&go(NOW + 1000), &all(), 120).is_empty());
    }

    #[test]
    fn go_is_announced_when_gained_and_when_lost() {
        let mut alerts = Alerts::default();
        alerts.update(&live_snapshot(NOW, |_| {}), &all(), 120);
        let gained = alerts.update(&go(NOW + 1000), &all(), 120);
        assert_eq!(gained.len(), 1);
        assert_eq!(gained[0].kind, Kind::GoGained);
        assert_eq!(gained[0].title, "GO: start the dishwasher");
        assert_eq!(gained[0].body, "Exporting 2300 W, stable for 5 min. Needs 2150 W export.");
        assert!(alerts.update(&go(NOW + 2000), &all(), 120).is_empty(), "steady GO stays quiet");
        let lost = alerts.update(&live_snapshot(NOW + 3000, |s| s.evaluation.state = State::Limited), &all(), 120);
        assert_eq!(lost.len(), 1);
        assert_eq!(lost[0].kind, Kind::GoLost);
        assert!(lost[0].body.starts_with("GO IN"), "{}", lost[0].body);
    }

    #[test]
    fn peak_warnings_fire_once_per_rising_edge() {
        let warn = |at| {
            live_snapshot(at, |s| {
                s.peak_forecast = Forecast {
                    available: true,
                    reason: "ok".into(),
                    projected_w: Some(3900.0),
                    comparison_w: Some(3600.0),
                    comparison_is_minimum: false,
                    seconds_left: Some(185.0),
                    reduce_w: Some(1200.0),
                    warning: true,
                    ..Forecast::unavailable("ok")
                };
            })
        };
        let mut alerts = Alerts::default();
        alerts.update(&live_snapshot(NOW, |_| {}), &all(), 120);
        let first = alerts.update(&warn(NOW + 1000), &all(), 120);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].body, "Reduce import by 1.2 kW within 3:05 to stay under this month's peak of 3.6 kW.");
        assert!(first[0].warning);
        assert!(alerts.update(&warn(NOW + 2000), &all(), 120).is_empty());
        alerts.update(&live_snapshot(NOW + 3000, |_| {}), &all(), 120);
        let mut minimum = warn(NOW + 4000);
        minimum.peak_forecast.comparison_is_minimum = true;
        minimum.peak_forecast.comparison_w = Some(2500.0);
        let again = alerts.update(&minimum, &all(), 120);
        assert_eq!(again[0].body, "Reduce import by 1.2 kW within 3:05 to stay under the 2.5 kW minimum.");
    }

    #[test]
    fn monthly_peak_is_announced_when_reached() {
        let mut alerts = Alerts::default();
        alerts.update(&live_snapshot(NOW, |_| {}), &all(), 120);
        let reached = alerts.update(
            &live_snapshot(NOW + 1000, |s| {
                s.evaluation.monthly_peak_reached = true;
                s.evaluation.current_demand_w = Some(3700.0);
            }),
            &all(),
            120,
        );
        assert_eq!(reached.len(), 1);
        assert_eq!(reached[0].kind, Kind::MonthlyPeak);
        assert!(reached[0].body.contains("3.7 kW") && reached[0].body.contains("3.6 kW"));
    }

    #[test]
    fn meter_outages_are_reported_after_the_delay_and_on_recovery() {
        let mut alerts = Alerts::default();
        assert!(alerts.update(&offline_snapshot(NOW), &all(), 120).is_empty());
        assert!(alerts.update(&offline_snapshot(NOW + 119_000), &all(), 120).is_empty());
        let offline = alerts.update(&offline_snapshot(NOW + 120_000), &all(), 120);
        assert_eq!(offline.len(), 1);
        assert_eq!(offline[0].title, "Meter offline");
        assert_eq!(offline[0].body, "No meter reading for 2 min (ECONNREFUSED).");
        assert!(alerts.update(&offline_snapshot(NOW + 500_000), &all(), 120).is_empty(), "reported once");
        let back = alerts.update(&live_snapshot(NOW + 501_000, |_| {}), &all(), 120);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].title, "Meter back online");
        assert!(!back[0].warning);
    }

    #[test]
    fn a_short_outage_stays_quiet() {
        let mut alerts = Alerts::default();
        alerts.update(&live_snapshot(NOW, |_| {}), &all(), 120);
        assert!(alerts.update(&offline_snapshot(NOW + 1000), &all(), 120).is_empty());
        assert!(alerts.update(&live_snapshot(NOW + 60_000, |_| {}), &all(), 120).is_empty());
    }

    #[test]
    fn settings_mute_alerts_but_edges_are_still_tracked() {
        let mut alerts = Alerts::default();
        let muted = Notifications { enabled: false, ..all() };
        alerts.update(&live_snapshot(NOW, |_| {}), &all(), 120);
        assert!(alerts.update(&go(NOW + 1000), &muted, 120).is_empty());
        assert!(alerts.update(&go(NOW + 2000), &all(), 120).is_empty(), "the GO edge was consumed while muted");
        let no_lost = Notifications { go_lost: false, ..all() };
        assert!(alerts.update(&live_snapshot(NOW + 3000, |_| {}), &no_lost, 120).is_empty());
    }

    #[test]
    fn simulated_alerts_are_marked() {
        let mut alerts = Alerts::default();
        alerts.update(&live_snapshot(NOW, |s| s.simulated = true), &all(), 120);
        let gained = alerts.update(
            &live_snapshot(NOW + 1000, |s| {
                s.simulated = true;
                s.evaluation.safe_to_start = true;
            }),
            &all(),
            120,
        );
        assert!(gained[0].title.starts_with("[SIM] GO"), "{}", gained[0].title);
    }
}

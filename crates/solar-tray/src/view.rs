//! Chooses the tray icon for a snapshot.

use crate::icon::{self, Face, Glyph, IconSpec, Rgb};
use crate::settings::IconStyle;
use solar_core::Snapshot;
use solar_core::decision::{Level, State};
use solar_core::js_round;
use solar_core::present::to_fixed;

/// A fresh meter reading is available (the same test the info screen uses).
pub fn live(s: &Snapshot) -> bool {
    s.evaluation.state != State::Offline && s.evaluation.surplus_w.is_some()
}

pub fn level_color(level: Level) -> Rgb {
    match level {
        Level::Offline => icon::OFFLINE,
        Level::Night => icon::NIGHT,
        Level::Importing => icon::IMPORTING,
        Level::Producing => icon::PRODUCING,
        Level::Surplus => icon::SURPLUS,
        Level::MaxSurplus => icon::MAX_SURPLUS,
    }
}

fn glyph(level: Level) -> Glyph {
    match level {
        Level::Offline => Glyph::Offline,
        Level::Night => Glyph::Night,
        Level::Importing => Glyph::Importing,
        Level::Producing | Level::Surplus | Level::MaxSurplus => Glyph::Sun,
    }
}

/// Watts as a short kW figure that fits a 16 px icon: `0.0`..`9.9`, then `10`..`99`.
pub fn compact_kw(watts: f64) -> String {
    let kw = watts.max(0.0) / 1000.0;
    if kw < 9.95 { to_fixed(kw, 1) } else { format!("{}", js_round(kw).min(99.0) as i64) }
}

/// Import while importing, otherwise export; the background colour tells them apart.
pub fn power_text(s: &Snapshot) -> String {
    if !live(s) {
        return "--".into();
    }
    let e = &s.evaluation;
    match e.grid_import_w {
        Some(import) if import > 0.0 => compact_kw(import),
        _ => compact_kw(e.surplus_w.unwrap_or(0.0)),
    }
}

pub fn icon_spec(s: &Snapshot, style: IconStyle) -> IconSpec {
    let live = live(s);
    let level = if live { s.evaluation.level } else { Level::Offline };
    let color = level_color(level);
    let face = match style {
        IconStyle::Level => Face::Symbol { glyph: glyph(level), color },
        IconStyle::Power => Face::Number { text: power_text(s), background: color },
    };
    IconSpec {
        face,
        go: live && s.evaluation.safe_to_start,
        warning: live && (s.peak_forecast.warning || s.evaluation.monthly_peak_reached),
    }
}

/// Shown when the config file cannot be used.
pub fn error_spec() -> IconSpec {
    IconSpec { face: Face::Symbol { glyph: Glyph::Offline, color: icon::OFFLINE }, go: false, warning: true }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solar_core::{Runtime, SolarConfig};

    const NOW: i64 = 1_800_000_000_000;

    fn scenario(name: &str) -> Snapshot {
        let mut runtime = Runtime::new(SolarConfig::default());
        runtime.set_scenario(name, NOW).unwrap();
        for second in 1..=3 {
            runtime.mock_tick(NOW + second * 1000);
        }
        runtime.snapshot(NOW + 3000)
    }

    #[test]
    fn compact_kilowatts_fit_three_characters() {
        assert_eq!(compact_kw(0.0), "0.0");
        assert_eq!(compact_kw(-20.0), "0.0");
        assert_eq!(compact_kw(450.0), "0.5", "ties round up like the device");
        assert_eq!(compact_kw(2264.0), "2.3");
        assert_eq!(compact_kw(9940.0), "9.9");
        assert_eq!(compact_kw(9950.0), "10");
        assert_eq!(compact_kw(42_400.0), "42");
        assert_eq!(compact_kw(250_000.0), "99");
    }

    #[test]
    fn level_style_follows_the_solar_level() {
        let face = |name| icon_spec(&scenario(name), IconStyle::Level).face;
        assert_eq!(face("max"), Face::Symbol { glyph: Glyph::Sun, color: icon::MAX_SURPLUS });
        assert_eq!(face("importing"), Face::Symbol { glyph: Glyph::Importing, color: icon::IMPORTING });
        assert_eq!(face("night"), Face::Symbol { glyph: Glyph::Night, color: icon::NIGHT });
        assert_eq!(face("offline"), Face::Symbol { glyph: Glyph::Offline, color: icon::OFFLINE });
    }

    #[test]
    fn power_style_shows_export_or_import() {
        let text = |name| match icon_spec(&scenario(name), IconStyle::Power).face {
            Face::Number { text, .. } => text,
            other => panic!("{other:?}"),
        };
        assert_eq!(text("excellent"), "4.2");
        assert_eq!(text("importing"), "0.7");
        assert_eq!(text("offline"), "--");
    }

    #[test]
    fn badges_need_a_live_reading() {
        let mut s = scenario("excellent");
        assert!(!icon_spec(&s, IconStyle::Level).go, "not stable yet");
        s.evaluation.safe_to_start = true;
        s.peak_forecast.warning = true;
        let spec = icon_spec(&s, IconStyle::Level);
        assert!(spec.go && spec.warning);
        s.evaluation.state = State::Offline;
        let spec = icon_spec(&s, IconStyle::Level);
        assert!(!spec.go && !spec.warning);
    }
}

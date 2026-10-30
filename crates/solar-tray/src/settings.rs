//! The tray's own config file: `%LOCALAPPDATA%\Solar Systray\config.json`, holding the solar
//! settings (same shape as solar-agent) and tray preferences. On first run it is seeded from
//! solar-agent's `companion.json` when that file is valid; that file is never modified.

use crate::alerts::Kind;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use solar_core::SolarConfig;
use std::path::{Path, PathBuf};

pub const APP_DIR: &str = "Solar Systray";
pub const COMPANION: &str = r"Solar Agent Data\companion.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IconStyle {
    /// A coloured symbol per solar level.
    #[default]
    Level,
    /// The current export (or import) in kW on the level colour.
    Power,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Notifications {
    pub enabled: bool,
    pub go_gained: bool,
    pub go_lost: bool,
    pub peak_forecast: bool,
    pub monthly_peak: bool,
    pub meter_offline: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Notifications {
            enabled: true,
            go_gained: true,
            go_lost: true,
            peak_forecast: true,
            monthly_peak: true,
            meter_offline: true,
        }
    }
}

impl Notifications {
    /// The switch for one kind, or the master switch for `None`.
    pub fn flag_mut(&mut self, kind: Option<Kind>) -> &mut bool {
        match kind {
            None => &mut self.enabled,
            Some(Kind::GoGained) => &mut self.go_gained,
            Some(Kind::GoLost) => &mut self.go_lost,
            Some(Kind::PeakForecast) => &mut self.peak_forecast,
            Some(Kind::MonthlyPeak) => &mut self.monthly_peak,
            Some(Kind::MeterOffline) => &mut self.meter_offline,
        }
    }

    pub fn wants(&self, kind: Kind) -> bool {
        match kind {
            Kind::GoGained => self.go_gained,
            Kind::GoLost => self.go_lost,
            Kind::PeakForecast => self.peak_forecast,
            Kind::MonthlyPeak => self.monthly_peak,
            Kind::MeterOffline => self.meter_offline,
        }
    }

    pub fn allows(&self, kind: Kind) -> bool {
        self.enabled && self.wants(kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct TraySettings {
    pub icon_style: IconStyle,
    /// The selected appliance id; `None` uses `decision.defaultAppliance`.
    pub appliance: Option<String>,
    pub notifications: Notifications,
    pub offline_notify_after_seconds: u64,
}

impl Default for TraySettings {
    fn default() -> Self {
        TraySettings {
            icon_style: IconStyle::Level,
            appliance: None,
            notifications: Notifications::default(),
            offline_notify_after_seconds: 120,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Seed {
    Companion(PathBuf),
    Defaults,
}

#[derive(Debug, Clone)]
pub struct Loaded {
    /// The `solar` section as written in the file, kept verbatim when tray settings are saved.
    pub solar_json: Value,
    pub solar: SolarConfig,
    pub tray: TraySettings,
    pub seeded: Option<Seed>,
}

pub fn local_app_data() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub fn config_path(base: &Path) -> PathBuf {
    base.join(APP_DIR).join("config.json")
}

pub fn companion_path(base: &Path) -> PathBuf {
    base.join(COMPANION)
}

/// Parses and validates the config file.
pub fn parse(text: &str) -> Result<(Value, SolarConfig, TraySettings), String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    let object = value.as_object().ok_or("the file must contain a JSON object")?;
    if let Some(key) = object.keys().find(|k| *k != "solar" && *k != "tray") {
        return Err(format!("unknown section \"{key}\" (expected \"solar\" and \"tray\")"));
    }
    let solar_json = object.get("solar").cloned().unwrap_or_else(|| json!({}));
    let solar = SolarConfig::from_json(&solar_json).map_err(|e| format!("solar: {e}"))?;
    let tray = match object.get("tray") {
        Some(tray) => TraySettings::deserialize(tray).map_err(|e| format!("tray: {e}"))?,
        None => TraySettings::default(),
    };
    Ok((solar_json, solar, tray))
}

/// The solar settings from solar-agent's `companion.json`, normalised, or `None` when unusable.
pub fn seed_from_companion(text: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(text).ok()?;
    let object = value.as_object()?;
    if object.keys().any(|k| k != "mode" && k != "solar") {
        return None;
    }
    let mut solar = match object.get("solar") {
        Some(Value::Object(solar)) => solar.clone(),
        Some(_) => return None,
        None => Map::new(),
    };
    // solar-agent's companion settings default to the live meter, not the simulation.
    let source = solar.entry("energySource").or_insert_with(|| json!({}));
    if let Value::Object(source) = source {
        source.entry("type").or_insert_with(|| json!("slimmelezer"));
    }
    SolarConfig::from_json(&Value::Object(solar)).ok().map(|config| config.to_json())
}

pub fn render(solar_json: &Value, tray: &TraySettings) -> String {
    let document = json!({ "solar": solar_json, "tray": tray });
    format!("{}\n", serde_json::to_string_pretty(&document).expect("settings serialise"))
}

/// Writes through a temporary file so a crash never leaves a truncated config.
pub fn save(path: &Path, solar_json: &Value, tray: &TraySettings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, render(solar_json, tray))?;
    std::fs::rename(&temporary, path)
}

/// Saves tray preferences, keeping the `solar` section currently on disk so hand edits that
/// have not been reloaded yet survive; `solar_json` is used when the file cannot be read.
pub fn save_tray(path: &Path, solar_json: &Value, tray: &TraySettings) -> std::io::Result<()> {
    let on_disk = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')).ok())
        .and_then(|value| value.get("solar").cloned());
    save(path, on_disk.as_ref().unwrap_or(solar_json), tray)
}

/// Loads the config file, creating it on first run from the companion file or the defaults.
pub fn load_or_seed(path: &Path, companion: Option<&Path>) -> Result<Loaded, String> {
    let failed = |message: String| format!("{}: {message}", path.display());
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let (solar_json, solar, tray) = parse(text.trim_start_matches('\u{feff}')).map_err(failed)?;
            Ok(Loaded { solar_json, solar, tray, seeded: None })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let from_companion = companion
                .and_then(|p| std::fs::read_to_string(p).ok().map(|text| (p, text)))
                .and_then(|(p, text)| seed_from_companion(text.trim_start_matches('\u{feff}')).map(|v| (p, v)));
            let (solar_json, seeded) = match from_companion {
                Some((p, value)) => (value, Seed::Companion(p.to_path_buf())),
                None => (SolarConfig::default().to_json(), Seed::Defaults),
            };
            let solar = SolarConfig::from_json(&solar_json).map_err(|e| failed(format!("solar: {e}")))?;
            let tray = TraySettings::default();
            save(path, &solar_json, &tray).map_err(|e| failed(format!("cannot create the file: {e}")))?;
            Ok(Loaded { solar_json, solar, tray, seeded: Some(seeded) })
        }
        Err(error) => Err(failed(format!("cannot read the file: {error}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("solar-systray-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empty_sections_use_defaults() {
        let (solar_json, solar, tray) = parse("{}").unwrap();
        assert_eq!(solar_json, json!({}));
        assert!(solar.is_mock());
        assert_eq!(tray, TraySettings::default());
    }

    #[test]
    fn tray_settings_parse_from_camel_case() {
        let text = r#"{"solar": {"energySource": {"type": "slimmelezer", "host": "192.0.2.50"}},
            "tray": {"iconStyle": "power", "appliance": "heatPumpDryer",
                     "notifications": {"goLost": false}, "offlineNotifyAfterSeconds": 60}}"#;
        let (_, solar, tray) = parse(text).unwrap();
        assert!(!solar.is_mock());
        assert_eq!(tray.icon_style, IconStyle::Power);
        assert_eq!(tray.appliance.as_deref(), Some("heatPumpDryer"));
        assert!(!tray.notifications.go_lost);
        assert!(tray.notifications.go_gained);
        assert_eq!(tray.offline_notify_after_seconds, 60);
    }

    #[test]
    fn rejects_typos_and_invalid_solar_settings() {
        assert!(parse("not json").unwrap_err().contains("invalid JSON"));
        assert!(parse("[]").is_err());
        assert!(parse(r#"{"slar": {}}"#).unwrap_err().contains("unknown section"));
        assert!(parse(r#"{"tray": {"iconStyle": "huge"}}"#).unwrap_err().starts_with("tray:"));
        assert!(parse(r#"{"tray": {"notifications": {"goGaind": true}}}"#).is_err());
        assert!(parse(r#"{"solar": {"energySource": {"port": 0}}}"#).unwrap_err().starts_with("solar:"));
    }

    #[test]
    fn rendered_settings_round_trip() {
        let solar_json = json!({"energySource": {"type": "slimmelezer", "host": "198.51.100.5"}});
        let tray =
            TraySettings { icon_style: IconStyle::Power, appliance: Some("dishwasher".into()), ..Default::default() };
        let (again_json, _, again_tray) = parse(&render(&solar_json, &tray)).unwrap();
        assert_eq!(again_json, solar_json, "the solar section is kept verbatim");
        assert_eq!(again_tray, tray);
    }

    #[test]
    fn companion_settings_seed_the_live_meter() {
        let seeded =
            seed_from_companion(r#"{"mode": "solar", "solar": {"energySource": {"host": "192.0.2.77"}}}"#).unwrap();
        let config = SolarConfig::from_json(&seeded).unwrap();
        assert!(!config.is_mock(), "a missing type means the SlimmeLezer");
        assert_eq!(config.energy_source.host, "192.0.2.77");
        let explicit =
            seed_from_companion(r#"{"mode": "agents", "solar": {"energySource": {"type": "mock"}}}"#).unwrap();
        assert!(SolarConfig::from_json(&explicit).unwrap().is_mock());
        assert!(seed_from_companion(r#"{"mode": "solar"}"#).is_some());
        assert!(seed_from_companion("{").is_none());
        assert!(seed_from_companion(r#"{"mode": "solar", "extra": 1}"#).is_none());
        assert!(seed_from_companion(r#"{"solar": {"energySource": {"port": 0}}}"#).is_none());
    }

    #[test]
    fn first_run_seeds_from_the_companion_and_never_touches_it() {
        let dir = temp_dir("seed");
        let companion = dir.join("companion.json");
        let original = r#"{"mode":"solar","solar":{"energySource":{"host":"192.0.2.88"}}}"#;
        std::fs::write(&companion, original).unwrap();
        let path = config_path(&dir);
        let loaded = load_or_seed(&path, Some(&companion)).unwrap();
        assert_eq!(loaded.seeded, Some(Seed::Companion(companion.clone())));
        assert_eq!(loaded.solar.energy_source.host, "192.0.2.88");
        assert!(path.exists());
        assert_eq!(std::fs::read_to_string(&companion).unwrap(), original);
        let again = load_or_seed(&path, Some(&companion)).unwrap();
        assert_eq!(again.seeded, None);
        assert_eq!(again.solar, loaded.solar);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_run_without_a_companion_starts_in_simulation() {
        let dir = temp_dir("defaults");
        let path = config_path(&dir);
        let loaded = load_or_seed(&path, Some(&dir.join("missing.json"))).unwrap();
        assert_eq!(loaded.seeded, Some(Seed::Defaults));
        assert!(loaded.solar.is_mock());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_tray_preferences_keeps_hand_edited_solar_settings() {
        let dir = temp_dir("save-tray");
        let path = config_path(&dir);
        let loaded = load_or_seed(&path, None).unwrap();
        let edited = r#"{"solar": {"energySource": {"type": "slimmelezer", "host": "203.0.113.1"}}, "tray": {}}"#;
        std::fs::write(&path, edited).unwrap();
        let tray = TraySettings { icon_style: IconStyle::Power, ..Default::default() };
        save_tray(&path, &loaded.solar_json, &tray).unwrap();
        let (solar_json, _, saved) = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(solar_json["energySource"]["host"], "203.0.113.1");
        assert_eq!(saved.icon_style, IconStyle::Power);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn notification_switches_gate_each_kind() {
        let mut notifications = Notifications::default();
        assert!(Kind::ALL.iter().all(|kind| notifications.allows(*kind)));
        *notifications.flag_mut(Some(Kind::GoLost)) = false;
        assert!(!notifications.allows(Kind::GoLost) && notifications.allows(Kind::GoGained));
        *notifications.flag_mut(None) = false;
        assert!(Kind::ALL.iter().all(|kind| !notifications.allows(*kind)));
        assert!(notifications.wants(Kind::GoGained), "kinds keep their own setting");
    }

    #[test]
    fn a_broken_file_is_reported_with_its_path() {
        let dir = temp_dir("broken");
        let path = config_path(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "\u{feff}{\"solar\": {\"decision\": {\"typo\": 1}}}").unwrap();
        let error = load_or_seed(&path, None).unwrap_err();
        assert!(error.contains("config.json") && error.contains("solar:"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

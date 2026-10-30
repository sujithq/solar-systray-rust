//! Solar configuration with the same defaults and validation as `config.mjs`.

use crate::sun::Location;
use indexmap::IndexMap;
use serde_json::{Map, Value, json};
use std::net::Ipv4Addr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Mock,
    SlimmeLezer,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Mock => "mock",
            SourceKind::SlimmeLezer => "slimmelezer",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnergySource {
    pub kind: SourceKind,
    pub host: String,
    pub port: u16,
    pub allow_legacy_without_crc: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InverterSource {
    /// IPv4 address, or empty when no inverter is polled.
    pub host: String,
    pub port: u16,
    pub unit_id: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecisionConfig {
    pub safety_margin_w: f64,
    pub stable_duration_seconds: f64,
    pub stale_after_seconds: f64,
    pub minimum_export_w: f64,
    pub hysteresis_w: f64,
    pub excellent_extra_w: f64,
    pub max_surplus_w: f64,
    pub level_hysteresis_w: f64,
    pub import_peak_w: f64,
    pub import_gauge_max_w: f64,
    pub gauge_thickness: String,
    pub display_refresh_seconds: u32,
    pub default_appliance: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DevicePower {
    pub sleep_enabled: bool,
    pub sleep_start: String,
    pub sleep_end: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Appliance {
    pub name: String,
    pub peak_w: f64,
}

/// The Solar Agent `SolarConfiguration`, validated.
#[derive(Debug, Clone, PartialEq)]
pub struct SolarConfig {
    pub energy_source: EnergySource,
    pub inverter_source: InverterSource,
    pub decision: DecisionConfig,
    pub location: Location,
    pub device_power: DevicePower,
    pub appliances: IndexMap<String, Appliance>,
}

/// The import gauge maximum when none is saved: 11,000 W, or twice a larger peak (at most 86,400 W).
pub fn default_import_gauge_max_w(import_peak_w: f64) -> f64 {
    if import_peak_w < 11000.0 { 11000.0 } else { (import_peak_w * 2.0).min(86400.0) }
}

pub const GAUGE_THICKNESSES: [&str; 3] = ["thin", "normal", "bold"];

pub fn defaults() -> Value {
    json!({
        "energySource": { "type": "mock", "host": "192.0.2.10", "port": 23, "allowLegacyWithoutCrc": false },
        "inverterSource": { "host": "", "port": 502, "unitId": 1 },
        "decision": {
            "safetyMarginW": 300, "stableDurationSeconds": 300, "staleAfterSeconds": 30,
            "minimumExportW": 100, "hysteresisW": 150, "excellentExtraW": 1000,
            "maxSurplusW": 5000, "levelHysteresisW": 200,
            "importPeakW": 2500,
            "importGaugeMaxW": 11000,
            "gaugeThickness": "normal",
            "displayRefreshSeconds": 5,
            "defaultAppliance": "dishwasher"
        },
        "location": { "latitude": null, "longitude": null },
        "devicePower": { "sleepEnabled": true, "sleepStart": "22:00", "sleepEnd": "06:00" },
        "appliances": {
            "dishwasher": { "name": "Dishwasher", "peakW": 2000 },
            "washingMachine": { "name": "Washing machine", "peakW": 2200 },
            "heatPumpDryer": { "name": "Heat-pump dryer", "peakW": 1200 },
            "conventionalDryer": { "name": "Conventional dryer", "peakW": 2500 }
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

fn fail<T>(message: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError(message.into()))
}

fn object<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String, Value>, ConfigError> {
    value.as_object().ok_or_else(|| ConfigError(format!("{name} must be an object")))
}

fn finite(value: &Value) -> Option<f64> {
    value.as_f64().filter(|v| v.is_finite())
}

fn integer(value: &Value) -> Option<i64> {
    finite(value).filter(|v| v.fract() == 0.0).map(|v| v as i64)
}

fn clock_time(value: &Value) -> bool {
    let Some(s) = value.as_str() else { return false };
    let b = s.as_bytes();
    b.len() == 5
        && b[2] == b':'
        && b.iter().enumerate().all(|(i, c)| i == 2 || c.is_ascii_digit())
        && ((b[0] == b'0' || b[0] == b'1') || (b[0] == b'2' && b[1] <= b'3'))
        && b[3] <= b'5'
}

fn valid_meter_host(host: &str) -> bool {
    (1..=253).contains(&host.len()) && host.bytes().all(|b| b.is_ascii_alphanumeric() || b".:-".contains(&b))
}

fn valid_appliance_id(id: &str) -> bool {
    let b = id.as_bytes();
    (1..=40).contains(&b.len())
        && b[0].is_ascii_alphabetic()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
}

impl Default for SolarConfig {
    fn default() -> Self {
        SolarConfig::from_json(&json!({})).expect("defaults are valid")
    }
}

impl SolarConfig {
    /// Merges a partial configuration over the defaults and validates it like `configuration()`.
    pub fn from_json(input: &Value) -> Result<SolarConfig, ConfigError> {
        let input = object(input, "configuration")?;
        let defaults = defaults();
        let defaults = defaults.as_object().unwrap();
        for key in input.keys() {
            if !defaults.contains_key(key) {
                return fail(format!("Unknown configuration section: {key}"));
            }
        }
        let mut config = defaults.clone();
        for section in ["energySource", "inverterSource", "decision", "location", "devicePower"] {
            if let Some(value) = input.get(section) {
                let given = object(value, section)?;
                let target = config.get_mut(section).unwrap().as_object_mut().unwrap();
                for (key, value) in given {
                    if !target.contains_key(key) {
                        return fail(format!("Unknown {section} setting: {key}"));
                    }
                    target.insert(key.clone(), value.clone());
                }
            }
        }
        let gauge_given = input.get("decision").and_then(|d| d.get("importGaugeMaxW")).is_some();
        if !gauge_given && let Some(peak) = finite(&config["decision"]["importPeakW"]) {
            config["decision"]
                .as_object_mut()
                .unwrap()
                .insert("importGaugeMaxW".into(), json!(default_import_gauge_max_w(peak)));
        }
        if let Some(appliances) = input.get("appliances") {
            config.insert("appliances".into(), appliances.clone());
        }

        let power = &config["devicePower"];
        let sleep_enabled = power["sleepEnabled"].as_bool();
        if sleep_enabled.is_none()
            || !clock_time(&power["sleepStart"])
            || !clock_time(&power["sleepEnd"])
            || power["sleepStart"] == power["sleepEnd"]
        {
            return fail("Device sleep needs an enabled flag and different start/end times in HH:MM format");
        }

        let source = &config["energySource"];
        let kind = match source["type"].as_str() {
            Some("mock") => SourceKind::Mock,
            Some("slimmelezer") => SourceKind::SlimmeLezer,
            _ => return fail("energySource.type must be mock or slimmelezer"),
        };
        let Some(host) = source["host"].as_str().filter(|h| valid_meter_host(h)) else {
            return fail("Invalid meter host");
        };
        let Some(port) = integer(&source["port"]).filter(|p| (1..=65535).contains(p)) else {
            return fail("Invalid meter port");
        };
        let Some(allow_legacy) = source["allowLegacyWithoutCrc"].as_bool() else {
            return fail("allowLegacyWithoutCrc must be boolean");
        };

        let inverter = &config["inverterSource"];
        let Some(inverter_host) = inverter["host"].as_str().filter(|h| h.is_empty() || h.parse::<Ipv4Addr>().is_ok())
        else {
            return fail("Inverter host must be an IPv4 address or empty to disable");
        };
        let Some(inverter_port) = integer(&inverter["port"]).filter(|p| (1..=65535).contains(p)) else {
            return fail("Invalid inverter port");
        };
        let Some(unit_id) = integer(&inverter["unitId"]).filter(|u| (1..=247).contains(u)) else {
            return fail("Inverter unit ID must be from 1 to 247");
        };

        let decision = config["decision"].as_object().unwrap();
        for (key, value) in decision {
            match key.as_str() {
                "defaultAppliance" => {}
                "gaugeThickness" => {
                    if !value.as_str().is_some_and(|v| GAUGE_THICKNESSES.contains(&v)) {
                        return fail("gaugeThickness must be thin, normal or bold");
                    }
                }
                "displayRefreshSeconds" => {
                    if !integer(value).is_some_and(|v| (1..=60).contains(&v)) {
                        return fail("displayRefreshSeconds must be a whole number from 1 to 60");
                    }
                }
                _ => {
                    if !finite(value).is_some_and(|v| (0.0..=86400.0).contains(&v)) {
                        return fail(format!("Invalid decision.{key}"));
                    }
                }
            }
        }
        let number = |key: &str| finite(&decision[key]).unwrap();
        let decision_config = DecisionConfig {
            safety_margin_w: number("safetyMarginW"),
            stable_duration_seconds: number("stableDurationSeconds"),
            stale_after_seconds: number("staleAfterSeconds"),
            minimum_export_w: number("minimumExportW"),
            hysteresis_w: number("hysteresisW"),
            excellent_extra_w: number("excellentExtraW"),
            max_surplus_w: number("maxSurplusW"),
            level_hysteresis_w: number("levelHysteresisW"),
            import_peak_w: number("importPeakW"),
            import_gauge_max_w: number("importGaugeMaxW"),
            gauge_thickness: decision["gaugeThickness"].as_str().unwrap().to_string(),
            display_refresh_seconds: integer(&decision["displayRefreshSeconds"]).unwrap() as u32,
            default_appliance: match decision["defaultAppliance"].as_str() {
                Some(id) => id.to_string(),
                None => return fail("Unknown default appliance"),
            },
        };
        if decision_config.stale_after_seconds <= 0.0 {
            return fail("staleAfterSeconds must be positive");
        }
        if decision_config.import_peak_w <= 0.0 {
            return fail("importPeakW must be positive");
        }
        if decision_config.import_gauge_max_w <= decision_config.import_peak_w {
            return fail("importGaugeMaxW must exceed importPeakW");
        }
        if decision_config.max_surplus_w <= decision_config.minimum_export_w + decision_config.level_hysteresis_w {
            return fail("maxSurplusW must exceed minimumExportW plus levelHysteresisW");
        }

        let location = &config["location"];
        let read_coordinate = |value: &Value| -> Result<Option<f64>, ConfigError> {
            match value {
                Value::Null => Ok(None),
                other => match finite(other) {
                    Some(v) => Ok(Some(v)),
                    None => fail("Latitude must be -90 to 90 and longitude -180 to 180"),
                },
            }
        };
        if location["latitude"].is_null() != location["longitude"].is_null() {
            return fail("Set both latitude and longitude, or neither");
        }
        let latitude = read_coordinate(&location["latitude"])?;
        let longitude = read_coordinate(&location["longitude"])?;
        if let (Some(lat), Some(lon)) = (latitude, longitude)
            && (lat.abs() > 90.0 || lon.abs() > 180.0)
        {
            return fail("Latitude must be -90 to 90 and longitude -180 to 180");
        }

        let profiles = object(&config["appliances"], "appliances")?;
        if profiles.is_empty() || profiles.len() > 20 {
            return fail("Provide 1 to 20 appliance profiles");
        }
        let mut appliances = IndexMap::new();
        for (id, profile) in profiles {
            let profile = object(profile, &format!("appliance {id}"))?;
            let name = profile
                .get("name")
                .and_then(Value::as_str)
                .filter(|n| (1..=40).contains(&n.len()) && n.bytes().all(|b| (0x20..=0x7e).contains(&b)));
            let peak_w = profile.get("peakW").and_then(finite).filter(|p| *p > 0.0 && *p <= 100000.0);
            match (valid_appliance_id(id), name, peak_w) {
                (true, Some(name), Some(peak_w)) => {
                    appliances.insert(id.clone(), Appliance { name: name.to_string(), peak_w });
                }
                _ => return fail(format!("Invalid appliance profile: {id}")),
            }
        }
        if !appliances.contains_key(&decision_config.default_appliance) {
            return fail("Unknown default appliance");
        }

        Ok(SolarConfig {
            energy_source: EnergySource {
                kind,
                host: host.to_string(),
                port: port as u16,
                allow_legacy_without_crc: allow_legacy,
            },
            inverter_source: InverterSource {
                host: inverter_host.to_string(),
                port: inverter_port as u16,
                unit_id: unit_id as u8,
            },
            decision: decision_config,
            location: Location { latitude, longitude },
            device_power: DevicePower {
                sleep_enabled: sleep_enabled.unwrap(),
                sleep_start: power["sleepStart"].as_str().unwrap().to_string(),
                sleep_end: power["sleepEnd"].as_str().unwrap().to_string(),
            },
            appliances,
        })
    }

    /// The full configuration as Solar Agent JSON.
    pub fn to_json(&self) -> Value {
        let d = &self.decision;
        let appliances: Map<String, Value> =
            self.appliances.iter().map(|(id, a)| (id.clone(), json!({ "name": a.name, "peakW": a.peak_w }))).collect();
        json!({
            "energySource": {
                "type": self.energy_source.kind.as_str(),
                "host": self.energy_source.host,
                "port": self.energy_source.port,
                "allowLegacyWithoutCrc": self.energy_source.allow_legacy_without_crc
            },
            "inverterSource": {
                "host": self.inverter_source.host,
                "port": self.inverter_source.port,
                "unitId": self.inverter_source.unit_id
            },
            "decision": {
                "safetyMarginW": d.safety_margin_w, "stableDurationSeconds": d.stable_duration_seconds,
                "staleAfterSeconds": d.stale_after_seconds, "minimumExportW": d.minimum_export_w,
                "hysteresisW": d.hysteresis_w, "excellentExtraW": d.excellent_extra_w,
                "maxSurplusW": d.max_surplus_w, "levelHysteresisW": d.level_hysteresis_w,
                "importPeakW": d.import_peak_w, "importGaugeMaxW": d.import_gauge_max_w,
                "gaugeThickness": d.gauge_thickness, "displayRefreshSeconds": d.display_refresh_seconds,
                "defaultAppliance": d.default_appliance
            },
            "location": { "latitude": self.location.latitude, "longitude": self.location.longitude },
            "devicePower": {
                "sleepEnabled": self.device_power.sleep_enabled,
                "sleepStart": self.device_power.sleep_start,
                "sleepEnd": self.device_power.sleep_end
            },
            "appliances": appliances
        })
    }

    pub fn is_mock(&self) -> bool {
        self.energy_source.kind == SourceKind::Mock
    }

    pub fn inverter_enabled(&self) -> bool {
        !self.is_mock() && !self.inverter_source.host.is_empty()
    }
}

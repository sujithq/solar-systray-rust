use serde::Serialize;

/// Cumulative meter registers in watt-hours (1-0:1.8.1/2 import, 1-0:2.8.1/2 export).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnergyTotals {
    pub import_tariff1_wh: i64,
    pub import_tariff2_wh: i64,
    pub export_tariff1_wh: i64,
    pub export_tariff2_wh: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Valid,
    Invalid,
}

/// One validated meter measurement, as produced by the DSMR parser or a simulation.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reading {
    /// Local receive time in Unix milliseconds.
    pub timestamp: i64,
    pub grid_import_w: f64,
    pub grid_export_w: f64,
    pub current_demand_w: Option<f64>,
    pub monthly_peak_w: Option<f64>,
    pub meter_time: Option<i64>,
    pub meter_month: Option<i64>,
    pub meter_day: Option<i64>,
    pub meter_minute_of_day: Option<i64>,
    pub meter_offset_minutes: Option<i64>,
    pub energy_totals_wh: Option<EnergyTotals>,
    pub monthly_peak_at: Option<i64>,
    pub monthly_peak_month: Option<i64>,
    pub source: String,
    pub quality: Quality,
    pub dsmr_version: Option<String>,
    pub p1_version: Option<String>,
    pub checksum: Option<&'static str>,
}

impl Reading {
    /// A plain import/export reading without meter clock, peak or totals.
    pub fn new(timestamp: i64, grid_import_w: f64, grid_export_w: f64, source: &str) -> Self {
        Reading {
            timestamp,
            grid_import_w,
            grid_export_w,
            current_demand_w: None,
            monthly_peak_w: None,
            meter_time: None,
            meter_month: None,
            meter_day: None,
            meter_minute_of_day: None,
            meter_offset_minutes: None,
            energy_totals_wh: None,
            monthly_peak_at: None,
            monthly_peak_month: None,
            source: source.to_string(),
            quality: Quality::Valid,
            dsmr_version: None,
            p1_version: None,
            checksum: None,
        }
    }
}

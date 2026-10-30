//! Custom simulated readings: signed watts where negative is import and positive export
//! (port of `simulation.mjs`).

use crate::config::{Appliance, DecisionConfig};
use crate::js_round;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// The largest import or export a validated reading may carry.
pub const SIMULATION_LIMIT_W: i64 = 1_000_000;

pub fn simulated_watts(value: i64) -> Result<i64, &'static str> {
    if value.abs() > SIMULATION_LIMIT_W {
        return Err("Enter whole watts from -1,000,000 (import) to 1,000,000 (export).");
    }
    Ok(value)
}

fn clamp(watts: f64) -> i64 {
    let limit = SIMULATION_LIMIT_W as f64;
    js_round(watts).clamp(-limit, limit) as i64
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    pub id: Cow<'static, str>,
    pub label: String,
    pub watts: i64,
}

/// Presets on, or just past, the configured thresholds of the selected appliance, so each level
/// and start decision can be reached.
pub fn simulation_presets(decision: &DecisionConfig, profile: &Appliance) -> Vec<Preset> {
    let required_w = profile.peak_w + decision.safety_margin_w;
    let start_threshold_w = required_w + decision.hysteresis_w;
    let name = &profile.name;
    let presets: [(&'static str, String, f64); 15] = [
        ("import-above-gauge-max", "Above import gauge maximum".into(), -(decision.import_gauge_max_w * 1.1)),
        ("import-gauge-max", "Import gauge maximum".into(), -decision.import_gauge_max_w),
        ("import-above-peak", "Above import peak".into(), -(decision.import_peak_w * 1.25)),
        ("import-peak", "Import peak".into(), -decision.import_peak_w),
        ("import-half-peak", "Half import peak".into(), -decision.import_peak_w / 2.0),
        ("zero", "Near zero".into(), 0.0),
        ("minimum-export", "Minimum export".into(), decision.minimum_export_w),
        ("surplus", "Surplus".into(), decision.minimum_export_w + decision.level_hysteresis_w + 1.0),
        ("appliance-below-peak", format!("Below {name} peak"), profile.peak_w - 1.0),
        ("appliance-peak", format!("{name} peak"), profile.peak_w),
        ("appliance-hold", format!("{name} keeps GO"), required_w + 1.0),
        ("appliance-start", format!("{name} start"), start_threshold_w + 1.0),
        ("excellent", "Excellent".into(), required_w + decision.excellent_extra_w + 1.0),
        ("max-surplus", "Max surplus".into(), decision.max_surplus_w),
        ("strong-export", "Strong export".into(), decision.max_surplus_w * 1.5),
    ];
    presets.into_iter().map(|(id, label, watts)| Preset { id: Cow::Borrowed(id), label, watts: clamp(watts) }).collect()
}

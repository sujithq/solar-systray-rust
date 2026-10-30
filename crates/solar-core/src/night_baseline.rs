//! Learns a volatile median household-use baseline from an observed night (port of `night-baseline.mjs`).

use crate::sun::Sun;
use serde::Serialize;

pub const MIN_NIGHT_SAMPLES: usize = 30;
pub const SAMPLE_INTERVAL_MS: i64 = 60_000;
pub const DEFAULT_NIGHT_BASELINE_W: f64 = 200.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineState {
    pub baseline_w: f64,
    pub learned: bool,
    pub samples: usize,
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 { sorted[middle] } else { (sorted[middle - 1] + sorted[middle]) / 2.0 }
}

#[derive(Debug, Clone)]
pub struct NightBaseline {
    samples: Vec<f64>,
    last_sample_at: Option<i64>,
    previous_night_w: f64,
    learned: bool,
    was_night: Option<bool>,
}

impl Default for NightBaseline {
    fn default() -> Self {
        NightBaseline {
            samples: Vec::new(),
            last_sample_at: None,
            previous_night_w: DEFAULT_NIGHT_BASELINE_W,
            learned: false,
            was_night: None,
        }
    }
}

impl NightBaseline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> BaselineState {
        BaselineState { baseline_w: self.previous_night_w, learned: self.learned, samples: self.samples.len() }
    }

    pub fn update(&mut self, sun: Sun, household_use_w: Option<f64>, timestamp: Option<i64>) -> BaselineState {
        match sun {
            Sun::Down => {
                if self.was_night != Some(true) {
                    self.samples.clear();
                    self.last_sample_at = None;
                }
                if let (Some(watts), Some(timestamp)) = (household_use_w, timestamp)
                    && watts.is_finite()
                    && watts >= 0.0
                {
                    let bucket = timestamp.div_euclid(SAMPLE_INTERVAL_MS);
                    if Some(bucket) != self.last_sample_at {
                        self.samples.push(watts);
                        self.last_sample_at = Some(bucket);
                    }
                }
                self.was_night = Some(true);
            }
            Sun::Up => {
                if self.was_night == Some(true) && self.samples.len() >= MIN_NIGHT_SAMPLES {
                    self.previous_night_w = median(&self.samples);
                    self.learned = true;
                }
                self.samples.clear();
                self.last_sample_at = None;
                self.was_night = Some(false);
            }
            Sun::Unknown => {}
        }
        self.state()
    }
}

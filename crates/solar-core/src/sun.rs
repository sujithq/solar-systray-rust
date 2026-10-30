//! NOAA solar-position approximation, computed locally (port of `sun.mjs`).

use crate::js_round;
use serde::{Deserialize, Serialize};

pub const HORIZON_DEGREES: f64 = -0.833;
const MINUTE_MS: i64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sun {
    Up,
    Down,
    /// No location configured; treated as up.
    Unknown,
}

impl Sun {
    pub fn as_str(self) -> &'static str {
        match self {
            Sun::Up => "up",
            Sun::Down => "down",
            Sun::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SunPosition {
    pub state: Sun,
    pub elevation: Option<f64>,
}

impl From<Sun> for SunPosition {
    fn from(state: Sun) -> Self {
        SunPosition { state, elevation: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct Location {
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

impl Location {
    pub fn new(latitude: f64, longitude: f64) -> Self {
        Location { latitude: Some(latitude), longitude: Some(longitude) }
    }

    fn valid(&self) -> Option<(f64, f64)> {
        match (self.latitude, self.longitude) {
            (Some(lat), Some(lon)) if lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0 => {
                Some((lat, lon))
            }
            _ => None,
        }
    }
}

fn radians(degrees: f64) -> f64 {
    degrees * std::f64::consts::PI / 180.0
}

fn degrees(value: f64) -> f64 {
    value * 180.0 / std::f64::consts::PI
}

pub fn sun_elevation(time_ms: f64, latitude: f64, longitude: f64) -> f64 {
    let t = (time_ms / 86_400_000.0 + 2_440_587.5 - 2_451_545.0) / 36525.0;
    let mean_longitude = (280.46646 + t * (36000.76983 + t * 0.0003032)) % 360.0;
    let mean_anomaly = 357.52911 + t * (35999.05029 - 0.0001537 * t);
    let eccentricity = 0.016708634 - t * (0.000042037 + 0.0000001267 * t);
    let m = radians(mean_anomaly);
    let center = m.sin() * (1.914602 - t * (0.004817 + 0.000014 * t))
        + (2.0 * m).sin() * (0.019993 - 0.000101 * t)
        + (3.0 * m).sin() * 0.000289;
    let omega = radians(125.04 - 1934.136 * t);
    let apparent_longitude = radians(mean_longitude + center - 0.00569 - 0.00478 * omega.sin());
    let mean_obliquity = 23.0 + (26.0 + (21.448 - t * (46.815 + t * (0.00059 - t * 0.001813))) / 60.0) / 60.0;
    let obliquity = radians(mean_obliquity + 0.00256 * omega.cos());
    let declination = (obliquity.sin() * apparent_longitude.sin()).asin();
    let y = (obliquity / 2.0).tan().powi(2);
    let l = radians(mean_longitude);
    let equation_of_time = 4.0
        * degrees(
            y * (2.0 * l).sin() - 2.0 * eccentricity * m.sin() + 4.0 * eccentricity * y * m.sin() * (2.0 * l).cos()
                - 0.5 * y * y * (4.0 * l).sin()
                - 1.25 * eccentricity * eccentricity * (2.0 * m).sin(),
        );
    let minutes = ((time_ms % 86_400_000.0) + 86_400_000.0) % 86_400_000.0 / 60000.0;
    let solar_time = ((minutes + equation_of_time + 4.0 * longitude) % 1440.0 + 1440.0) % 1440.0;
    let hour_angle = radians(solar_time / 4.0 - 180.0);
    let lat = radians(latitude);
    let cos_zenith = lat.sin() * declination.sin() + lat.cos() * declination.cos() * hour_angle.cos();
    90.0 - degrees(cos_zenith.clamp(-1.0, 1.0).acos())
}

/// Up, down or unknown (no location); the elevation is rounded to 0.1 degree.
pub fn sun_state(location: &Location, time_ms: i64) -> SunPosition {
    let Some((lat, lon)) = location.valid() else { return Sun::Unknown.into() };
    let elevation = sun_elevation(time_ms as f64, lat, lon);
    SunPosition {
        state: if elevation < HORIZON_DEGREES { Sun::Down } else { Sun::Up },
        elevation: Some(js_round(elevation * 10.0) / 10.0),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct SunTimes {
    pub sunrise: Option<i64>,
    pub sunset: Option<i64>,
}

/// The next sunrise and sunset within 24 hours, rounded to the minute; none at the poles.
pub fn sun_times(location: &Location, time_ms: i64) -> SunTimes {
    let mut times = SunTimes::default();
    let Some((lat, lon)) = location.valid() else { return times };
    let above = |t: f64| sun_elevation(t, lat, lon) >= HORIZON_DEGREES;
    let step = (10 * MINUTE_MS) as f64;
    let start_ms = time_ms as f64;
    let mut previous = above(start_ms);
    let mut start = start_ms;
    while start < start_ms + (1440 * MINUTE_MS) as f64 {
        let end = start + step;
        let current = above(end);
        let slot = if current { &mut times.sunrise } else { &mut times.sunset };
        if current != previous && slot.is_none() {
            let (mut low, mut high) = (start, end);
            while high - low > 1000.0 {
                let middle = (low + high) / 2.0;
                if above(middle) == current {
                    high = middle;
                } else {
                    low = middle;
                }
            }
            *slot = Some(js_round(high / MINUTE_MS as f64) as i64 * MINUTE_MS);
        }
        previous = current;
        start += step;
    }
    times
}

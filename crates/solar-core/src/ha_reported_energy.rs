//! HA recorder's held boundary states describe reported energy, not acquisition freshness.

use crate::ha_daily::day_bounds;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportedEnergy {
    pub day_start: i64,
    pub as_of: i64,
    pub import_wh: i64,
    pub export_wh: i64,
    pub recorded_points: [i64; 4],
}

pub(crate) struct ReportedSamples {
    start: i64,
    end: i64,
    entities: [String; 4],
    first: [Option<i64>; 4],
    last: [Option<i64>; 4],
    last_at: [Option<i64>; 4],
    records: usize,
}

impl ReportedSamples {
    pub fn new(start: i64, end: i64, entities: [String; 4]) -> Result<Self, &'static str> {
        if day_bounds(start).is_none_or(|(midnight, next)| midnight != start || end <= start || end > next)
            || entities.iter().any(String::is_empty)
        {
            return Err("reported-energy-window-or-mapping");
        }
        Ok(Self { start, end, entities, first: [None; 4], last: [None; 4], last_at: [None; 4], records: 0 })
    }

    pub fn append(&mut self, result: &Value, chunk_start: i64, chunk_end: i64) -> Result<(), &'static str> {
        let object = result.as_object().ok_or("reported-energy-shape")?;
        if object.keys().any(|id| !self.entities.contains(id)) {
            return Err("reported-energy-entity");
        }
        for (index, entity) in self.entities.iter().enumerate() {
            let rows = object.get(entity).and_then(Value::as_array).ok_or("reported-energy-missing-entity")?;
            if rows.is_empty() || rows.len() > 10_000 {
                return Err("reported-energy-missing-boundary");
            }
            for (position, row) in rows.iter().enumerate() {
                self.records += 1;
                if self.records > 100_000 {
                    return Err("reported-energy-record-limit");
                }
                let at = row["lu"].as_f64().filter(|v| v.is_finite()).ok_or("reported-energy-time")? * 1000.0;
                if at < chunk_start as f64 || at >= chunk_end as f64 || at.fract().abs() > 0.01 {
                    return Err("reported-energy-time");
                }
                let at = at.round() as i64;
                let attributes = &row["a"];
                if attributes["unit_of_measurement"] != "kWh"
                    || attributes["device_class"] != "energy"
                    || attributes["state_class"] != "total_increasing"
                {
                    return Err("reported-energy-unit-or-class");
                }
                let state = row["s"].as_str().ok_or("reported-energy-invalid-state")?;
                let kwh = state.parse::<f64>().map_err(|_| "reported-energy-invalid-state")?;
                let wh = kwh * 1000.0;
                if !wh.is_finite() || !(0.0..=999_999_999.0).contains(&wh) || (wh - wh.round()).abs() > 0.000_001 {
                    return Err("reported-energy-invalid-state");
                }
                let wh = wh.round() as i64;
                if position == 0 {
                    if at != chunk_start {
                        return Err("reported-energy-missing-boundary");
                    }
                    // This is the recorder's projected held start-state, NOT a
                    // newly acquired reading. Consecutive chunks must agree.
                    if self.last[index].is_some_and(|last| last != wh) {
                        return Err("reported-energy-boundary-mismatch");
                    }
                    self.first[index].get_or_insert(wh);
                } else if self.last_at[index].is_some_and(|last| at <= last) {
                    return Err("reported-energy-order");
                }
                if self.last[index].is_some_and(|last| wh < last) {
                    return Err("reported-energy-reset");
                }
                self.last[index] = Some(wh);
                self.last_at[index] = Some(at);
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Result<ReportedEnergy, &'static str> {
        let mut delta = [0; 4];
        let mut recorded_points = [0; 4];
        for i in 0..4 {
            delta[i] = self.last[i].ok_or("reported-energy-missing-entity")?
                - self.first[i].ok_or("reported-energy-missing-boundary")?;
            recorded_points[i] = self.last_at[i].ok_or("reported-energy-time")?;
        }
        Ok(ReportedEnergy {
            day_start: self.start,
            as_of: self.end,
            import_wh: delta[0] + delta[1],
            export_wh: delta[2] + delta[3],
            recorded_points,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn row(at: i64, value: &str) -> Value {
        json!({"lu":at as f64/1000.0,"s":value,"a":{"unit_of_measurement":"kWh",
            "device_class":"energy","state_class":"total_increasing"}})
    }
    fn setup() -> (i64, [String; 4]) {
        (crate::utc_ms(2027, 1, 14, 23, 0, 0), ["sensor.a", "sensor.b", "sensor.c", "sensor.d"].map(String::from))
    }
    #[test]
    fn held_midnight_unchanged_tariffs_and_partial_hour_are_reported_not_atomic() {
        let (start, ids) = setup();
        let mut samples = ReportedSamples::new(start, start + 360_000, ids.clone()).unwrap();
        let mut result = serde_json::Map::new();
        for id in &ids {
            result.insert(id.clone(), json!([row(start, "10.000"), row(start + 60_000, "10.010")]));
        }
        samples.append(&Value::Object(result), start, start + 300_000).unwrap();
        let mut result = serde_json::Map::new();
        for id in &ids {
            result.insert(id.clone(), json!([row(start + 300_000, "10.010")]));
        }
        samples.append(&Value::Object(result), start + 300_000, start + 360_000).unwrap();
        let total = samples.finish().unwrap();
        assert_eq!(total.import_wh, 20);
        assert_eq!(total.export_wh, 20);
        assert_eq!(total.as_of, start + 360_000);
    }
    #[test]
    fn missing_boundary_unknown_wrong_units_reset_and_inconsistent_chunks_fail() {
        let (start, ids) = setup();
        for (value, at, expected) in [
            ("unknown", start, "reported-energy-invalid-state"),
            ("10", start + 1000, "reported-energy-missing-boundary"),
            ("-1", start, "reported-energy-invalid-state"),
        ] {
            let mut s = ReportedSamples::new(start, start + 60_000, ids.clone()).unwrap();
            let r = json!({ids[0].clone():[row(at,value)]});
            assert_eq!(s.append(&r, start, start + 60_000), Err(expected));
        }
        let mut s = ReportedSamples::new(start, start + 60_000, ids.clone()).unwrap();
        assert_eq!(
            s.append(&json!({ids[0].clone():[row(start,"10"),row(start+1000,"0")]}), start, start + 60_000),
            Err("reported-energy-reset")
        );
    }

    #[test]
    fn static_tariffs_are_valid_but_missing_entities_units_and_chunk_mismatches_are_not() {
        let (start, ids) = setup();
        let make = |at, value: &str| {
            let mut object = serde_json::Map::new();
            for id in &ids {
                object.insert(id.clone(), json!([row(at, value)]));
            }
            Value::Object(object)
        };
        let mut s = ReportedSamples::new(start, start + 600_000, ids.clone()).unwrap();
        s.append(&make(start, "10"), start, start + 300_000).unwrap();
        s.append(&make(start + 300_000, "10"), start + 300_000, start + 600_000).unwrap();
        assert_eq!(s.finish().unwrap().import_wh, 0);
        for fault in ["missing", "unit", "class", "boundary", "order"] {
            let mut s = ReportedSamples::new(start, start + 600_000, ids.clone()).unwrap();
            let mut result = make(start, "10");
            let expected = match fault {
                "missing" => {
                    result.as_object_mut().unwrap().remove(&ids[3]);
                    "reported-energy-missing-entity"
                }
                "unit" => {
                    result[&ids[0]][0]["a"]["unit_of_measurement"] = json!("W");
                    "reported-energy-unit-or-class"
                }
                "class" => {
                    result[&ids[0]][0]["a"]["state_class"] = json!("measurement");
                    "reported-energy-unit-or-class"
                }
                "boundary" => {
                    s.append(&result, start, start + 300_000).unwrap();
                    result = make(start + 300_000, "11");
                    assert_eq!(
                        s.append(&result, start + 300_000, start + 600_000),
                        Err("reported-energy-boundary-mismatch")
                    );
                    continue;
                }
                _ => {
                    result[&ids[0]] =
                        json!([row(start, "10"), row(start + 1000, "10.001"), row(start + 1000, "10.002")]);
                    "reported-energy-order"
                }
            };
            assert_eq!(s.append(&result, start, start + 300_000), Err(expected), "{fault}");
        }
    }

    #[test]
    fn reported_result_is_separate_from_live_state_and_old_day_results_are_rejected() {
        let (start, _) = setup();
        let mut runtime = crate::Runtime::new(
            crate::SolarConfig::from_json(&serde_json::json!({
                "energySource":{"type":"home-assistant"},"homeAssistant":{"atomicGrid":"sensor.fixture"}
            }))
            .unwrap(),
        );
        let report = ReportedEnergy {
            day_start: start,
            as_of: start + 60_000,
            import_wh: 2100,
            export_wh: 200,
            recorded_points: [start; 4],
        };
        runtime.set_reported_energy(Ok(report.clone()), start + 70_000);
        let s = runtime.snapshot(start + 70_000);
        assert!(!s.evaluation.safe_to_start);
        assert!(s.reading_at.is_none());
        assert_eq!(s.reported_energy, Some(report.clone()));
        let info = crate::present::solar_info(&s);
        assert_eq!(info.section("Today").unwrap().value("Imported"), Some("2.100 kWh"));
        assert!(info.section("Today").unwrap().value("Coverage").unwrap().contains("reported"));
        runtime.set_reported_energy(Err("fixture-error"), start + 80_000);
        assert!(runtime.snapshot(start + 80_000).reported_energy.is_some(), "retain as-of on refresh failure");
        runtime.set_reported_energy(Ok(report), start + 86_400_000);
        let next = runtime.snapshot(start + 86_400_000);
        assert!(next.reported_energy.is_none());
        assert_eq!(next.reported_energy_issue.as_deref(), Some("reported-energy-old-day-result"));
    }

    #[test]
    fn tariff_deltas_sum_by_direction_without_abs_or_hour_rounding_on_dst_days() {
        for date in ["2027-03-28T20:32:15Z", "2027-10-31T21:32:15Z"] {
            let end = chrono::DateTime::parse_from_rfc3339(date).unwrap().timestamp_millis();
            let (start, _) = day_bounds(end).unwrap();
            let (_, ids) = setup();
            let mut s = ReportedSamples::new(start, end, ids.clone()).unwrap();
            let mut result = serde_json::Map::new();
            // Inactive second import tariff, independently split export tariffs.
            for (i, (first, last)) in
                [("100.000", "102.100"), ("50.000", "50.000"), ("20.000", "20.500"), ("10.000", "10.100")]
                    .into_iter()
                    .enumerate()
            {
                result.insert(ids[i].clone(), json!([row(start, first), row(end - 1000, last)]));
            }
            s.append(&Value::Object(result), start, end).unwrap();
            let result = s.finish().unwrap();
            assert_eq!(result.import_wh, 2100);
            assert_eq!(result.export_wh, 600);
            assert_eq!(result.as_of, end);

            let mut negative = ReportedSamples::new(start, end, ids.clone()).unwrap();
            let mut result = serde_json::Map::new();
            for id in &ids {
                result.insert(id.clone(), json!([row(start, "1.000")]));
            }
            result.insert(ids[2].clone(), json!([row(start, "-20.000")]));
            assert_eq!(negative.append(&Value::Object(result), start, end), Err("reported-energy-invalid-state"));
        }
    }
}

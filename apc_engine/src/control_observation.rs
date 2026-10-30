//! Reusable advisory snapshots from actual validated engine observations.
use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};
use serde::{Serialize, Serializer};

use crate::{config::UnifiedModel, monitoring::{CvStatistics, RollingPerformance}};

fn serialize_timestamp<S: Serializer>(value: &DateTime<Utc>, serializer: S) -> std::result::Result<S::Ok, S::Error> {
    serializer.collect_str(&value.format("%+"))
}

#[derive(Serialize)]
struct UnavailableObservation<'a> {
    schema_version: u8,
    source: &'static str,
    time_basis: &'static str,
    #[serde(serialize_with = "serialize_timestamp")]
    observed_at: DateTime<Utc>,
    engine_id: &'a str,
    state: &'static str,
    reason: &'a str,
}

pub fn unavailable_json(engine_id: &str, reason: &str) -> Result<String> {
    Ok(serde_json::to_string(&UnavailableObservation {
        schema_version: 1, source: "engine_opc_snapshot", time_basis: "engine_receipt",
        observed_at: Utc::now(), engine_id, state: "unavailable", reason,
    })?)
}

#[derive(Serialize)]
struct ObservedCv {
    name: String,
    unit: String,
    pv: f64,
    target: f64,
    low: f64,
    high: f64,
    #[serde(flatten)]
    statistics: CvStatistics,
}

#[derive(Serialize)]
struct ObservedMv {
    name: String,
    unit: String,
    applied_sp: f64,
    total_variation: f64,
}

#[derive(Serialize)]
struct ObservedDv {
    name: String,
    unit: String,
    pv: f64,
}

#[derive(Serialize)]
pub struct RuntimeObservation<'a> {
    schema_version: u8,
    source: &'static str,
    time_basis: &'static str,
    #[serde(serialize_with = "serialize_timestamp")]
    observed_at: DateTime<Utc>,
    engine_id: &'a str,
    sample_time_seconds: f64,
    requested_mode: u8,
    samples: usize,
    window_samples: usize,
    complete: bool,
    cv: Vec<ObservedCv>,
    mv: Vec<ObservedMv>,
    dv: Vec<ObservedDv>,
}

impl<'a> RuntimeObservation<'a> {
    pub fn new(engine_id: &'a str, model: &UnifiedModel, window_samples: usize) -> Self {
        Self {
            schema_version: 1, source: "engine_opc_snapshot", time_basis: "engine_receipt",
            observed_at: DateTime::<Utc>::UNIX_EPOCH, engine_id,
            sample_time_seconds: model.tuning.sample_time,
            requested_mode: 0, samples: 0, window_samples, complete: false,
            cv: model.variables.cvs.iter().map(|v| ObservedCv {
                name: v.name.clone(), unit: v.units.clone(), pv: 0.0, target: 0.0, low: 0.0, high: 0.0,
                statistics: CvStatistics::default(),
            }).collect(),
            mv: model.variables.mvs.iter().map(|v| ObservedMv {
                name: v.name.clone(), unit: v.units.clone(), applied_sp: 0.0, total_variation: 0.0,
            }).collect(),
            dv: model.variables.dvs.iter().map(|v| ObservedDv {
                name: v.name.clone(), unit: v.units.clone(), pv: 0.0,
            }).collect(),
        }
    }

    /// Only publish after this succeeds. The caller supplies the receipt time of
    /// this input snapshot, not a guessed PLC timestamp or the telemetry-write time.
    pub fn update(&mut self, observed_at: DateTime<Utc>, mode: f64, performance: &RollingPerformance,
        pvs: &[f64], targets: &[f64], low: &[f64], high: &[f64], applied_sp: &[f64], dvs: &[f64]) -> Result<()> {
        ensure!([0.0, 1.0, 2.0].contains(&mode) && performance.samples() > 0,
            "Observation needs a valid mode and measured samples");
        ensure!(pvs.len() == self.cv.len() && targets.len() == self.cv.len()
            && low.len() == self.cv.len() && high.len() == self.cv.len()
            && applied_sp.len() == self.mv.len() && dvs.len() == self.dv.len()
            && performance.mv_total_variation().len() == self.mv.len(), "Observation dimensions differ");
        self.observed_at = observed_at;
        self.requested_mode = mode as u8;
        self.samples = performance.samples();
        self.window_samples = performance.window_samples();
        self.complete = self.samples == self.window_samples;
        let mut statistics = performance.cv_statistics();
        for (i, value) in self.cv.iter_mut().enumerate() {
            value.statistics = statistics.next().ok_or_else(|| anyhow::anyhow!("Missing CV observations"))?;
            value.pv = pvs[i]; value.target = targets[i]; value.low = low[i]; value.high = high[i];
        }
        ensure!(statistics.next().is_none(), "Extra CV observations");
        for (i, value) in self.mv.iter_mut().enumerate() {
            value.applied_sp = applied_sp[i]; value.total_variation = performance.mv_total_variation()[i];
        }
        for (value, pv) in self.dv.iter_mut().zip(dvs) { value.pv = *pv; }
        Ok(())
    }
}

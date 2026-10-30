//! Atomic advisory horizons from the native solve, never command acknowledgements.
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use clarabel::solver::SolverStatus;
use serde::{Serialize, Serializer};
use crate::config::UnifiedModel;

fn serialize_timestamp<S: Serializer>(value: &DateTime<Utc>, serializer: S) -> std::result::Result<S::Ok, S::Error> {
    serializer.collect_str(&value.format("%Y-%m-%dT%H:%M:%S%.fZ"))
}

#[derive(Serialize)]
struct Envelope<'a> {
    schema_version: u8,
    source: &'static str,
    time_basis: &'static str,
    #[serde(serialize_with = "serialize_timestamp")]
    issued_at: DateTime<Utc>,
    engine_id: &'a str,
    instance_id: &'a str,
    sequence: u64,
    controller_sha256: &'a str,
    // The digest identifies parsed model bytes, not live targets/limits/weights.
    live_tuning: bool,
    sample_time: f64,
    cv_count: usize,
    mv_count: usize,
    prediction_horizon: usize,
    control_horizon: usize,
    layout: &'static str,
    cv_first_offset_steps: usize,
    mv_first_offset_steps: usize,
    cv: &'a [f64],
    mv_base: &'a [f64],
    mv_delta: &'a [f64],
}

/// One reusable serialization buffer, with no cloned prediction or plan arrays.
/// Keep this owner across OPC reconnects; a new process gets a new instance ID.
pub struct LiveForecast {
    sample_time: f64,
    cv_count: usize,
    mv_count: usize,
    prediction_horizon: usize,
    control_horizon: usize,
    engine_id: String,
    instance_id: String,
    controller_sha256: String,
    live_tuning: bool,
    sequence: u64,
    buffer: Vec<u8>,
}

impl LiveForecast {
    pub fn new(model: &UnifiedModel, engine_id: &str, instance_id: &str,
        controller_sha256: &str, live_tuning: bool) -> Self {
        let capacity = 768 + 25 * (model.variables.cvs.len() * model.tuning.prediction_horizon
            + model.variables.mvs.len() * (model.tuning.control_horizon + 1));
        Self { sample_time: model.tuning.sample_time, cv_count: model.variables.cvs.len(),
            mv_count: model.variables.mvs.len(), prediction_horizon: model.tuning.prediction_horizon,
            control_horizon: model.tuning.control_horizon, engine_id: engine_id.into(), instance_id: instance_id.into(),
            controller_sha256: controller_sha256.into(), live_tuning,
            sequence: 0, buffer: Vec::with_capacity(capacity) }
    }

    /// Same process sequence continues across model cutover; old metadata never
    /// labels a new-model horizon and consumers need not invent a new instance.
    pub fn rebind(&mut self, model: &UnifiedModel, digest: &str) {
        self.sample_time = model.tuning.sample_time;
        self.cv_count = model.variables.cvs.len();
        self.mv_count = model.variables.mvs.len();
        self.prediction_horizon = model.tuning.prediction_horizon;
        self.control_horizon = model.tuning.control_horizon;
        self.controller_sha256.clear(); self.controller_sha256.push_str(digest);
        self.live_tuning = false;
        self.buffer.clear();
    }

    pub fn sequence(&self) -> u64 { self.sequence }

    /// `issued_at` is captured at solve completion, not when OPC/HMI receives it.
    /// Any error clears the prior payload; callers publish JSON null, never reuse it.
    pub fn encode(&mut self, issued_at: DateTime<Utc>, status: SolverStatus,
        cv: &[f64], mv_base: &[f64], mv_delta: &[f64]) -> Result<&str> {
        self.buffer.clear();
        ensure!(matches!(status, SolverStatus::Solved | SolverStatus::AlmostSolved), "Forecast requires a solved horizon");
        let nc = self.cv_count;
        let nm = self.mv_count;
        let np = self.prediction_horizon;
        let nu = self.control_horizon;
        ensure!(cv.len() == nc * np && mv_base.len() == nm && mv_delta.len() == nm * nu,
            "Live forecast dimensions differ from controller");
        ensure!(cv.iter().chain(mv_base).chain(mv_delta).all(|value| value.is_finite()),
            "Nonfinite live forecast");
        let sequence = self.sequence.checked_add(1).context("Live forecast sequence exhausted")?;
        serde_json::to_writer(&mut self.buffer, &Envelope {
            schema_version: 1, source: "engine_solver", time_basis: "utc", issued_at,
            engine_id: &self.engine_id, instance_id: &self.instance_id, sequence,
            controller_sha256: &self.controller_sha256, live_tuning: self.live_tuning,
            sample_time: self.sample_time, cv_count: nc, mv_count: nm,
            prediction_horizon: np, control_horizon: nu, layout: "step-major",
            cv_first_offset_steps: 1, mv_first_offset_steps: 0, cv, mv_base, mv_delta,
        })?;
        self.sequence = sequence;
        Ok(std::str::from_utf8(&self.buffer)?)
    }
}

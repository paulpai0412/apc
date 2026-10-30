//! Isolated, finite-memory step-response simulation. No OPC or production writes.
//! `g[k]` is the response at elapsed `k * sample_time`; prehistory is steady.
//! The FIR uses differences of g on absolute deviations, retaining final gain.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs::File, io::BufReader};
use crate::config::UnifiedModel;

pub type Responses = Vec<Vec<Vec<f64>>>;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct History {
    pub sample_time: f64,
    pub time_seconds: Vec<f64>,
    pub cv: Vec<Vec<f64>>,
    pub mv: Vec<Vec<f64>>,
    pub dv: Vec<Vec<f64>>,
}

pub fn same_sample_time(a: f64, b: f64) -> bool {
    a.is_finite() && b.is_finite() && a > 0.0 && b > 0.0
        && (a - b).abs() <= 1e-9 * a.max(b)
}

impl History {
    pub fn load(path: &str) -> Result<Self> {
        serde_json::from_reader(BufReader::new(File::open(path)?)).context("Invalid history JSON")
    }

    pub fn validate(&self, model: &UnifiedModel) -> Result<()> {
        ensure!(same_sample_time(self.sample_time, model.tuning.sample_time), "History/model sample_time mismatch or invalid sample_time");
        let n = self.time_seconds.len();
        ensure!(n >= 2, "History requires at least two rows");
        ensure!(self.time_seconds.iter().all(|x| x.is_finite()), "Nonfinite timestamp");
        for pair in self.time_seconds.windows(2) {
            ensure!(same_sample_time(pair[1] - pair[0], self.sample_time), "History timestamps must strictly increase uniformly by sample_time");
        }
        for (name, rows, width) in [
            ("cv", &self.cv, model.variables.cvs.len()),
            ("mv", &self.mv, model.variables.mvs.len()),
            ("dv", &self.dv, model.variables.dvs.len()),
        ] {
            ensure!(rows.len() == n, "{name} row count mismatch");
            for row in rows {
                ensure!(row.len() == width && row.iter().all(|x| x.is_finite()), "{name} dimension mismatch or nonfinite value");
            }
        }
        Ok(())
    }
}

/// Validate dimensions before invoking the existing controller's indexing code.
pub fn responses(model: &UnifiedModel, require_settled: bool) -> Result<(Responses, Responses)> {
    model.validate_control_strategy()?;
    let nc = model.variables.cvs.len();
    let nm = model.variables.mvs.len();
    let nd = model.variables.dvs.len();
    let p = model.tuning.prediction_horizon;
    ensure!(nc > 0 && nm > 0 && p > 0, "Need CVs, MVs and a positive prediction horizon");
    ensure!(model.tuning.sample_time.is_finite() && model.tuning.sample_time > 0.0, "Invalid model sample_time");
    ensure!(!model.variables.cvs.iter().any(|cv| cv.is_integrating), "Integrating CVs are unsupported by the finite-response twin");
    for names in [model.variables.cvs.iter().map(|v| &v.name).collect::<Vec<_>>(),
        model.variables.mvs.iter().map(|v| &v.name).collect(),
        model.variables.dvs.iter().map(|v| &v.name).collect()] {
        let mut seen = HashSet::new();
        ensure!(names.iter().all(|name| !name.trim().is_empty() && seen.insert(*name)), "Variable names must be nonempty and unique within each role");
    }
    let generate = |gain: &[Vec<f64>], tau: &[Vec<f64>], delay: &[Vec<f64>], width: usize| -> Result<Responses> {
        if width == 0 {
            ensure!(gain.is_empty() && tau.is_empty() && delay.is_empty(), "Unexpected parametric channels");
            return Ok(Vec::new());
        }
        ensure!(gain.len() == nc && tau.len() == nc && delay.len() == nc, "Parametric row count mismatch");
        let mut out = Vec::with_capacity(nc);
        for i in 0..nc {
            ensure!(gain[i].len() == width && tau[i].len() == width && delay[i].len() == width, "Parametric column count mismatch");
            let mut row = Vec::with_capacity(width);
            for j in 0..width {
                let (g, t, d) = (gain[i][j], tau[i][j], delay[i][j]);
                ensure!(g.is_finite() && t.is_finite() && d.is_finite() && t >= 0.0 && d >= 0.0, "Invalid FOPDT parameters");
                let curve: Vec<f64> = (0..p).map(|k| {
                    let elapsed = k as f64 * model.tuning.sample_time - d;
                    if elapsed < 0.0 { 0.0 } else if t < 1e-6 { g } else { g * (1.0 - (-elapsed / t).exp()) }
                }).collect();
                if require_settled && g != 0.0 {
                    ensure!((curve[p - 1] - g).abs() <= 0.001 * g.abs(), "FOPDT horizon does not reach steady state; extend plant horizon");
                }
                row.push(curve);
            }
            out.push(row);
        }
        Ok(out)
    };
    let (mv, dv) = match model.metadata.model_type.as_str() {
        "parametric" => (generate(&model.physics.gain, &model.physics.tau, &model.physics.dead_time, nm)?,
            generate(&model.physics.gain_dv, &model.physics.tau_dv, &model.physics.dead_time_dv, nd)?),
        "step_response" => (model.physics.step_coefficients.clone(), model.physics.dv_coefficients.clone()),
        other => anyhow::bail!("Unsupported model_type: {other}"),
    };
    for (name, matrix, width) in [("MV", &mv, nm), ("DV", &dv, nd)] {
        if width == 0 && matrix.is_empty() { continue; }
        ensure!(matrix.len() == nc, "{name} coefficient row count mismatch");
        for row in matrix {
            ensure!(row.len() == width, "{name} coefficient column count mismatch");
            for curve in row {
                ensure!(!curve.is_empty() && curve.iter().all(|x| x.is_finite()), "{name} coefficients must be nonempty and finite");
                if require_settled {
                    ensure!(curve.len() >= 3, "{name} response needs at least three samples to establish a settled tail");
                    let scale = curve.iter().fold(0.0_f64, |s, x| s.max(x.abs()));
                    let tail = &curve[curve.len() - (curve.len() / 10).max(3)..];
                    let lo = tail.iter().copied().fold(f64::INFINITY, f64::min);
                    let hi = tail.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    ensure!(hi - lo <= 0.001 * scale.max(1e-12), "{name} response has no settled tail; extend or reidentify plant horizon");
                }
            }
        }
    }
    Ok((mv, dv))
}

pub struct StepResponseTwin {
    impulse: Vec<f64>,
    inputs: Vec<f64>,
    baseline_inputs: Vec<f64>,
    baseline_cv: Vec<f64>,
    cv: Vec<f64>,
    num_mv: usize,
    num_inputs: usize,
    horizon: usize,
    head: usize,
}

impl StepResponseTwin {
    pub fn new(model: &UnifiedModel, cv: &[f64], mv: &[f64], dv: &[f64]) -> Result<Self> {
        let (m, d) = responses(model, true)?;
        ensure!(cv.len() == model.variables.cvs.len() && mv.len() == model.variables.mvs.len() && dv.len() == model.variables.dvs.len(), "Baseline dimensions mismatch");
        ensure!(cv.iter().chain(mv).chain(dv).all(|x| x.is_finite()), "Nonfinite baseline");
        let horizon = m.iter().chain(&d).flat_map(|row| row.iter()).map(Vec::len).max().unwrap();
        let num_inputs = mv.len() + dv.len();
        let mut impulse = vec![0.0; cv.len() * num_inputs * horizon];
        for i in 0..cv.len() {
            for j in 0..num_inputs {
                let curve = if j < mv.len() { &m[i][j] } else { &d[i][j - mv.len()] };
                for k in 0..horizon {
                    let current = curve.get(k).copied().unwrap_or_else(|| *curve.last().unwrap());
                    let previous = if k == 0 { 0.0 } else { curve.get(k - 1).copied().unwrap_or_else(|| *curve.last().unwrap()) };
                    let h = current - previous;
                    ensure!(h.is_finite(), "Overflow in step response differences");
                    impulse[(i * num_inputs + j) * horizon + k] = h;
                }
            }
        }
        Ok(Self { impulse, inputs: vec![0.0; num_inputs * horizon], baseline_inputs: mv.iter().chain(dv).copied().collect(),
            baseline_cv: cv.to_vec(), cv: cv.to_vec(), num_mv: mv.len(), num_inputs, horizon, head: 0 })
    }

    pub fn output(&self) -> &[f64] { &self.cv }

    fn check_inputs(&self, mv: &[f64], dv: &[f64]) -> Result<()> {
        ensure!(mv.len() == self.num_mv && mv.len() + dv.len() == self.num_inputs, "Input dimensions mismatch");
        ensure!(mv.iter().chain(dv).enumerate().all(|(i, x)| x.is_finite() && (*x - self.baseline_inputs[i]).is_finite()), "Nonfinite input deviation");
        Ok(())
    }

    /// Historical row sampling: inputs change at this sample, so g[0] applies now.
    /// No allocation and no measurement correction after the initial CV baseline.
    pub fn sample(&mut self, mv: &[f64], dv: &[f64]) -> Result<&[f64]> {
        self.check_inputs(mv, dv)?;
        self.head = (self.head + 1) % self.horizon;
        for (j, value) in mv.iter().chain(dv).enumerate() {
            self.inputs[self.head * self.num_inputs + j] = value - self.baseline_inputs[j];
        }
        for i in 0..self.cv.len() {
            let mut y = self.baseline_cv[i];
            for j in 0..self.num_inputs {
                for k in 0..self.horizon {
                    let slot = (self.head + self.horizon - k) % self.horizon;
                    y += self.impulse[(i * self.num_inputs + j) * self.horizon + k] * self.inputs[slot * self.num_inputs + j];
                }
            }
            ensure!(y.is_finite(), "Nonfinite simulated CV");
            self.cv[i] = y;
        }
        Ok(&self.cv)
    }

    /// Observe y[t] first, then apply MV[t] for the entire next interval.
    /// The first next observation includes g[1]; DV[t+1] enters with g[0].
    /// No instantaneous controller algebraic loop even when g[0] is nonzero.
    pub fn advance(&mut self, command: &[f64], next_dv: &[f64]) -> Result<&[f64]> {
        self.check_inputs(command, next_dv)?;
        for (j, value) in command.iter().enumerate() {
            self.inputs[self.head * self.num_inputs + j] = value - self.baseline_inputs[j];
        }
        self.sample(command, next_dv)
    }
}

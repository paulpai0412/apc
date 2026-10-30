//! Offline free-run validation and feedback experiments with separate plant/controller inputs.
use anyhow::{ensure, Result};
use clarabel::solver::SolverStatus;
use serde::Serialize;
use crate::{config::{OptimizationMode, UnifiedModel}, dmc::DmcController, twin::{responses, same_sample_time, History, StepResponseTwin}};

#[derive(Debug, Serialize)]
pub struct ValidationResult {
    pub score: f64,
    pub cv_rmse: Vec<f64>,
    pub trace: History,
}

#[derive(Debug, Serialize)]
pub struct ClosedLoopResult {
    pub control_strategy: crate::config::ControlStrategy,
    pub score: f64,
    pub iae: Vec<f64>,
    pub mv_total_variation: Vec<f64>,
    pub cv_violation_seconds: Vec<f64>,
    pub solver_failures: usize,
    pub trace: History,
}

fn cv_ranges(model: &UnifiedModel) -> Result<Vec<f64>> {
    model.variables.cvs.iter().map(|cv| {
        let l = &cv.limits;
        ensure!([l.low_low, l.low, l.target, l.high, l.high_high].iter().all(|v| v.is_finite())
            && l.low_low <= l.low && l.low < l.high && l.high <= l.high_high
            && l.target >= l.low && l.target <= l.high && (l.high - l.low).is_finite(), "Invalid CV limits for {}", cv.name);
        Ok(l.high - l.low)
    }).collect()
}

pub fn validate(plant: &UnifiedModel, history: &History) -> Result<ValidationResult> {
    history.validate(plant)?;
    let ranges = cv_ranges(plant)?;
    let mut twin = StepResponseTwin::new(plant, &history.cv[0], &history.mv[0], &history.dv[0])?;
    let mut trace = History { sample_time: history.sample_time, time_seconds: history.time_seconds.clone(),
        cv: Vec::with_capacity(history.cv.len()), mv: history.mv.clone(), dv: history.dv.clone() };
    trace.cv.push(twin.output().to_vec());
    let mut rmse = vec![0.0_f64; ranges.len()];
    for t in 1..history.cv.len() {
        let cv = twin.sample(&history.mv[t], &history.dv[t])?;
        for i in 0..cv.len() {
            // hypot accumulation avoids overflow from squaring large finite residuals.
            rmse[i] = rmse[i].hypot(cv[i] - history.cv[t][i]);
        }
        trace.cv.push(cv.to_vec());
    }
    for value in &mut rmse { *value /= ((history.cv.len() - 1) as f64).sqrt(); }
    let score = rmse.iter().zip(&ranges).map(|(e, r)| e / r).sum::<f64>() / ranges.len() as f64;
    ensure!(score.is_finite() && rmse.iter().all(|x| x.is_finite()), "Nonfinite validation metric");
    Ok(ValidationResult { score, cv_rmse: rmse, trace })
}

fn compatible(plant: &UnifiedModel, controller: &UnifiedModel) -> Result<()> {
    ensure!(same_sample_time(plant.tuning.sample_time, controller.tuning.sample_time), "Plant/controller sample_time mismatch");
    ensure!(plant.variables.cvs.iter().map(|v| (&v.name, &v.units))
        .eq(controller.variables.cvs.iter().map(|v| (&v.name, &v.units)))
        && plant.variables.mvs.iter().map(|v| (&v.name, &v.units))
        .eq(controller.variables.mvs.iter().map(|v| (&v.name, &v.units)))
        && plant.variables.dvs.iter().map(|v| (&v.name, &v.units))
        .eq(controller.variables.dvs.iter().map(|v| (&v.name, &v.units))),
        "Plant/controller variable names, units and ordering must match");
    Ok(())
}

pub fn closed_loop(plant: &UnifiedModel, controller: &UnifiedModel, history: &History) -> Result<ClosedLoopResult> {
    history.validate(plant)?;
    compatible(plant, controller)?;
    let ranges = cv_ranges(controller)?;
    let tuning = &controller.tuning;
    ensure!(tuning.prediction_horizon >= 5 && tuning.control_horizon > 0 && tuning.control_horizon <= tuning.prediction_horizon,
        "Controller requires prediction_horizon >= 5 and 1 <= control_horizon <= prediction_horizon");
    ensure!(tuning.solver_tolerance.is_finite() && tuning.solver_tolerance > 0.0 && tuning.max_iterations > 0
        && tuning.terminal_weight_factor.is_finite() && tuning.terminal_weight_factor >= 0.0, "Invalid solver tuning");
    for cv in &controller.variables.cvs {
        ensure!([cv.weight, cv.alpha, cv.ece_factor, cv.slack_weight].iter().all(|x| x.is_finite())
            && cv.weight >= 0.0 && (0.0..=1.0).contains(&cv.alpha) && cv.ece_factor > 0.0 && cv.slack_weight >= 0.0,
            "Invalid CV tuning for {}", cv.name);
    }
    for (i, mv) in controller.variables.mvs.iter().enumerate() {
        let l = &mv.limits;
        ensure!([l.low_low, l.low, l.high, l.high_high, mv.weight_r, mv.max_move, mv.target_weight].iter().all(|x| x.is_finite())
            && l.low_low <= l.low && l.low < l.high && l.high <= l.high_high && (l.high - l.low).is_finite()
            && mv.weight_r > 0.0 && mv.max_move > 0.0 && mv.target_weight >= 0.0
            && mv.target.map_or(true, |x| x.is_finite()), "Invalid MV limits/tuning for {}", mv.name);
        ensure!((l.low..=l.high).contains(&history.mv[0][i]), "Initial MV outside controller operating limits");
    }
    // main.rs currently omits parametric DV feedforward. Do not evaluate a
    // stronger controller than production; require explicit step responses.
    ensure!(controller.metadata.model_type != "parametric" || controller.variables.dvs.is_empty(),
        "Production engine does not load parametric DV responses; export step_response before closed-loop controller evaluation");
    let (coefficients, dv_coefficients) = responses(controller, false)?;
    let mut twin = StepResponseTwin::new(plant, &history.cv[0], &history.mv[0], &history.dv[0])?;
    let mut dmc = DmcController::new_from_coefficients(coefficients, dv_coefficients, controller)?;
    let cvs = &controller.variables.cvs;
    let mvs = &controller.variables.mvs;
    let targets: Vec<_> = cvs.iter().map(|v| v.limits.target).collect();
    let cv_min: Vec<_> = cvs.iter().map(|v| v.limits.low).collect();
    let cv_max: Vec<_> = cvs.iter().map(|v| v.limits.high).collect();
    let cv_weights: Vec<_> = cvs.iter().map(|v| v.weight).collect();
    let cv_alphas: Vec<_> = cvs.iter().map(|v| v.alpha).collect();
    let mv_min: Vec<_> = mvs.iter().map(|v| v.limits.low).collect();
    let mv_max: Vec<_> = mvs.iter().map(|v| v.limits.high).collect();
    let mv_weights: Vec<_> = mvs.iter().map(|v| v.weight_r).collect();
    let mv_targets: Vec<_> = mvs.iter().enumerate().map(|(i, v)| v.target.unwrap_or(history.mv[0][i])).collect();
    let n = history.cv.len();
    let mut trace = History { sample_time: history.sample_time, time_seconds: history.time_seconds.clone(),
        cv: Vec::with_capacity(n), mv: Vec::with_capacity(n), dv: history.dv.clone() };
    trace.cv.push(twin.output().to_vec());
    trace.mv.push(history.mv[0].clone());
    let mut command = history.mv[0].clone();
    let mut iae = vec![0.0; cvs.len()];
    let mut violations = vec![0.0; cvs.len()];
    let mut variation = vec![0.0; mvs.len()];
    for t in 0..n - 1 {
        let result = dmc.next_move(twin.output(), &targets, &command, &history.dv[t], &mv_targets,
            &mv_min, &mv_max, &cv_min, &cv_max, &cv_weights, &cv_alphas, &mv_weights);
        ensure!(matches!(result.status, SolverStatus::Solved | SolverStatus::AlmostSolved),
            "Solver failed at sample {t}: {:?}; trial invalid", result.status);
        ensure!(result.next_move.len() == mvs.len() && result.objective_value.is_finite()
            && result.next_move.iter().chain(&result.predicted_pvs).chain(&result.future_plan).chain(&result.cv_bias).all(|x| x.is_finite()),
            "Nonfinite or malformed solver result at sample {t}");
        for (j, mv) in mvs.iter().enumerate() {
            let proposed = command[j] + result.next_move[j];
            let eps = 1e-7 * (mv_max[j] - mv_min[j]).max(1.0);
            ensure!(proposed.is_finite() && proposed >= mv_min[j] - eps && proposed <= mv_max[j] + eps
                && result.next_move[j].abs() <= mv.max_move + eps, "Solver violated hard MV constraints at sample {t}");
            // Remove only floating-point feasibility tolerance, never hide infeasible trials.
            let applied = proposed.clamp((command[j] - mv.max_move).max(mv_min[j]), (command[j] + mv.max_move).min(mv_max[j]));
            variation[j] += (applied - command[j]).abs();
            command[j] = applied;
        }
        let cv = twin.advance(&command, &history.dv[t + 1])?;
        for i in 0..cvs.len() {
            let error = match &cvs[i].optimization_mode {
                OptimizationMode::Target { .. } => (cv[i] - targets[i]).abs(),
                OptimizationMode::Zone => (cv_min[i] - cv[i]).max(0.0) + (cv[i] - cv_max[i]).max(0.0),
                OptimizationMode::Maximize => (cv[i] - cv_max[i]).abs(),
                OptimizationMode::Minimize => (cv[i] - cv_min[i]).abs(),
            };
            iae[i] += error * history.sample_time;
            if cv[i] < cv_min[i] || cv[i] > cv_max[i] { violations[i] += history.sample_time; }
        }
        trace.cv.push(cv.to_vec());
        // MV row t+1 is the command held over [t, t+1], not a new historical MV.
        trace.mv.push(command.clone());
    }
    let duration = (n - 1) as f64 * history.sample_time;
    let tracking = iae.iter().zip(&ranges).map(|(e, r)| e / r / duration).sum::<f64>() / cvs.len() as f64;
    let constraint = violations.iter().map(|v| v / duration).sum::<f64>() / cvs.len() as f64;
    let movement = variation.iter().enumerate().map(|(j, v)| v / (mv_max[j] - mv_min[j]) / (n - 1) as f64).sum::<f64>() / mvs.len() as f64;
    let score = tracking + constraint + 0.01 * movement;
    ensure!(duration.is_finite() && score.is_finite() && iae.iter().chain(&variation).chain(&violations).all(|x| x.is_finite()), "Nonfinite closed-loop metric");
    Ok(ClosedLoopResult { control_strategy: controller.control_strategy.clone(), score, iae, mv_total_variation: variation, cv_violation_seconds: violations, solver_failures: 0, trace })
}

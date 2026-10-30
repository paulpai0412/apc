//! Behavioral feedback regressions, not substitutes for the full-quarter acceptance campaign.
mod test_helpers;

use authrity_apc_engine::{config::UnifiedModel, dmc::DmcController};
use clarabel::solver::SolverStatus;

// Independent discrete plant with gain/time-constant mismatch against the controller.
struct Plant {
    pv: f64,
    delayed: [f64; 2],
    cursor: usize,
}

impl Plant {
    fn new() -> Self { Self { pv: 50.0, delayed: [50.0; 2], cursor: 0 } }

    fn step(&mut self, applied_sp: f64, disturbance: f64) {
        let delayed = self.delayed[self.cursor];
        self.delayed[self.cursor] = applied_sp;
        self.cursor = (self.cursor + 1) % self.delayed.len();
        let equilibrium = 50.0 + 0.55 * (delayed - 50.0) + disturbance;
        self.pv += (1.0 - (-1.0_f64 / 6.0).exp()) * (equilibrium - self.pv);
    }
}

fn model() -> UnifiedModel {
    let mut model = test_helpers::create_simple_1x1_model();
    // Memory deliberately exceeds the prediction horizon; no feedback from planned moves.
    model.physics.step_coefficients[0][0] = (0..64).map(|k| {
        if k <= 2 { 0.0 } else { 0.5 * (1.0 - (-((k - 2) as f64) / 5.0).exp()) }
    }).collect();
    model
}

fn apply(controller: &mut DmcController, model: &UnifiedModel, pv: f64, target: f64, mv: &mut f64) {
    let result = controller.next_move(&[pv], &[target], &[*mv], &[], &[50.0],
        &[10.0], &[90.0], &[20.0], &[80.0], &[1.0], &[0.0], &[0.1]);
    assert!(matches!(result.status, SolverStatus::Solved | SolverStatus::AlmostSolved));
    let movement = result.next_move[0];
    assert!(movement.abs() <= model.variables.mvs[0].max_move + 1e-5);
    *mv += movement;
    assert!((10.0 - 1e-5..=90.0 + 1e-5).contains(mv));
}

#[test]
fn applied_increments_track_both_setpoint_directions_despite_model_mismatch() {
    let model = model();
    let mut controller = DmcController::new_from_coefficients(
        model.physics.step_coefficients.clone(), vec![], &model).unwrap();
    let mut plant = Plant::new();
    let mut mv = 50.0;
    for target in [52.0, 48.0] {
        let mut final_error_squared = 0.0;
        for step in 0..180 {
            apply(&mut controller, &model, plant.pv, target, &mut mv);
            plant.step(mv, 0.0);
            if step >= 150 { final_error_squared += (plant.pv - target).powi(2); }
        }
        assert!((final_error_squared / 30.0).sqrt() < 0.1,
            "Feedback must settle at each requested target, not merely remain finite");
        let required_mv = 50.0 + (target - 50.0) / 0.55;
        assert!((mv - required_mv).abs() < 0.2);
    }
}

fn moments(sum: f64, square_sum: f64, samples: usize) -> (f64, f64) {
    let mean = sum / samples as f64;
    (mean, square_sum / samples as f64 - mean * mean)
}

#[test]
fn feedback_reduces_measured_cv_variance_without_hiding_tracking_bias() {
    let model = model();
    let mut controller = DmcController::new_from_coefficients(
        model.physics.step_coefficients.clone(), vec![], &model).unwrap();
    let mut controlled = Plant::new();
    let mut baseline = Plant::new();
    let mut mv = 50.0;
    let mut controlled_sum = 0.0;
    let mut controlled_squared = 0.0;
    let mut baseline_sum = 0.0;
    let mut baseline_squared = 0.0;
    let mut samples = 0;
    for step in 0..1200 {
        let disturbance = 2.0 * (std::f64::consts::TAU * step as f64 / 100.0).sin();
        apply(&mut controller, &model, controlled.pv, 50.0, &mut mv);
        controlled.step(mv, disturbance);
        baseline.step(50.0, disturbance);
        if step >= 200 {
            let measured = controlled.pv - 50.0;
            let reference = baseline.pv - 50.0;
            controlled_sum += measured;
            controlled_squared += measured * measured;
            baseline_sum += reference;
            baseline_squared += reference * reference;
            samples += 1;
        }
    }
    let (bias, variance) = moments(controlled_sum, controlled_squared, samples);
    let (_, baseline_variance) = moments(baseline_sum, baseline_squared, samples);
    assert!(bias.abs() < 0.1, "A quiet but off-target CV is not an improvement");
    assert!(variance < 0.75 * baseline_variance,
        "Physical CV variance must improve under identical external disturbance: {variance} vs {baseline_variance}");
}

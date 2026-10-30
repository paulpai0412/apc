mod test_helpers;

use authrity_apc_engine::{config::{DvConfig, DvLimits, DvNodes, DvLimitNodes}, twin::{History, StepResponseTwin}, validation};
use test_helpers::{create_simple_1x1_model, create_2x2_model};

fn history(rows: usize) -> History {
    History { sample_time: 1.0, time_seconds: (0..rows).map(|i| i as f64).collect(),
        cv: vec![vec![50.0]; rows], mv: vec![vec![50.0]; rows], dv: vec![vec![]; rows] }
}

#[test]
fn steady_gain_survives_long_after_fir_horizon() {
    let model = create_simple_1x1_model();
    let mut twin = StepResponseTwin::new(&model, &[50.0], &[50.0], &[]).unwrap();
    for _ in 0..1000 { twin.sample(&[60.0], &[]).unwrap(); }
    assert_eq!(twin.output(), &[55.0]);
    for _ in 0..1000 { twin.sample(&[50.0], &[]).unwrap(); }
    assert_eq!(twin.output(), &[50.0]);
}

#[test]
fn historical_step_uses_elapsed_zero_and_preserves_delay() {
    let model = create_simple_1x1_model();
    let mut twin = StepResponseTwin::new(&model, &[50.0], &[50.0], &[]).unwrap();
    // Step applied at sample one, coefficients are [0,0,0.1,0.25,0.4,0.5,...].
    for expected in [50.0, 50.0, 51.0, 52.5, 54.0, 55.0, 55.0] {
        assert_eq!(twin.sample(&[60.0], &[]).unwrap(), &[expected]);
    }
}

#[test]
fn plant_memory_is_independent_of_controller_prediction_horizon() {
    let mut model = create_simple_1x1_model();
    model.tuning.prediction_horizon = 3;
    model.physics.step_coefficients[0][0] = vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
    let mut twin = StepResponseTwin::new(&model, &[50.0], &[50.0], &[]).unwrap();
    for expected in [50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 60.0, 60.0, 60.0, 60.0] {
        assert_eq!(twin.sample(&[60.0], &[]).unwrap(), &[expected]);
    }
    model.tuning.prediction_horizon = 20;
    model.physics.step_coefficients[0][0] = vec![0.0, 1.0, 1.0, 1.0];
    let mut twin = StepResponseTwin::new(&model, &[50.0], &[50.0], &[]).unwrap();
    assert_eq!(twin.sample(&[60.0], &[]).unwrap(), &[50.0]);
    for _ in 0..30 {
        assert_eq!(twin.sample(&[60.0], &[]).unwrap(), &[60.0]);
    }
}

#[test]
fn held_feedback_command_is_one_interval_old_at_next_measurement() {
    let mut model = create_simple_1x1_model();
    model.physics.step_coefficients[0][0] = vec![0.25, 0.5, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
    let mut twin = StepResponseTwin::new(&model, &[50.0], &[50.0], &[]).unwrap();
    assert_eq!(twin.output(), &[50.0]);
    assert_eq!(twin.advance(&[60.0], &[]).unwrap(), &[55.0]);
    assert_eq!(twin.advance(&[60.0], &[]).unwrap(), &[60.0]);
}

#[test]
fn mimo_disturbance_and_mv_paths_superpose_with_distinct_baselines() {
    let mut model = create_2x2_model();
    model.variables.dvs.push(DvConfig { name: "DV1".into(), description: "Synthetic disturbance".into(), units: "%".into(),
        limits: DvLimits { low: 0.0, high: 100.0 }, node_ids: DvNodes { pv: "DV1:PV".into(),
            limits: DvLimitNodes { low: "DV1:Low".into(), high: "DV1:High".into() } } });
    let curve = |g: f64| { let mut v = vec![g; 10]; v[0] = 0.0; v };
    model.physics.step_coefficients = vec![vec![curve(2.0), curve(-1.0)], vec![curve(0.5), curve(3.0)]];
    model.physics.dv_coefficients = vec![vec![curve(4.0)], vec![curve(-2.0)]];
    let mut twin = StepResponseTwin::new(&model, &[10.0, 20.0], &[5.0, 8.0], &[3.0]).unwrap();
    assert_eq!(twin.sample(&[7.0, 9.0], &[4.0]).unwrap(), &[10.0, 20.0]);
    assert_eq!(twin.sample(&[7.0, 9.0], &[4.0]).unwrap(), &[17.0, 22.0]);
    for _ in 0..100 { twin.sample(&[7.0, 9.0], &[4.0]).unwrap(); }
    assert_eq!(twin.output(), &[17.0, 22.0]);
}

#[test]
fn invalid_histories_are_rejected_before_simulation() {
    let model = create_simple_1x1_model();
    let good = history(4);
    assert!(good.validate(&model).is_ok());
    let mut shifted = good.clone();
    shifted.time_seconds.iter_mut().for_each(|t| *t += 42.0);
    assert!(shifted.validate(&model).is_ok());
    for broken in 0..7 {
        let mut h = good.clone();
        match broken {
            0 => h.sample_time = 0.0,
            1 => h.time_seconds[2] = h.time_seconds[1],
            2 => h.time_seconds[2] += 0.1,
            3 => h.cv[2][0] = f64::NAN,
            4 => { h.mv.pop(); },
            5 => h.dv[0].push(1.0),
            _ => h.time_seconds[1] = f64::INFINITY,
        }
        assert!(h.validate(&model).is_err(), "case {broken}");
    }
}

#[test]
fn integrating_and_nonsettling_plants_are_rejected() {
    let mut model = create_simple_1x1_model();
    model.variables.cvs[0].is_integrating = true;
    assert!(StepResponseTwin::new(&model, &[50.0], &[50.0], &[]).is_err());
    model.variables.cvs[0].is_integrating = false;
    model.physics.step_coefficients[0][0] = (0..10).map(|i| i as f64).collect();
    assert!(StepResponseTwin::new(&model, &[50.0], &[50.0], &[]).is_err());
    model.physics.step_coefficients[0][0] = vec![f64::NAN; 10];
    assert!(StepResponseTwin::new(&model, &[50.0], &[50.0], &[]).is_err());
}

#[test]
fn parametric_delays_beyond_horizon_are_not_silently_zero_gain() {
    let mut model = create_simple_1x1_model();
    model.metadata.model_type = "parametric".into();
    model.physics.gain = vec![vec![2.0]];
    model.physics.tau = vec![vec![1.0]];
    model.physics.dead_time = vec![vec![100.0]];
    assert!(StepResponseTwin::new(&model, &[50.0], &[50.0], &[]).is_err());
    model.physics.dead_time[0][0] = 0.0;
    model.tuning.prediction_horizon = 30;
    assert!(StepResponseTwin::new(&model, &[50.0], &[50.0], &[]).is_ok());
}

#[test]
fn free_run_does_not_correct_against_recorded_measurements() {
    let model = create_simple_1x1_model();
    let mut h = history(15);
    h.cv[5][0] = 70.0;
    let result = validation::validate(&model, &h).unwrap();
    assert!(result.trace.cv.iter().all(|cv| cv == &[50.0]));
    assert!((result.cv_rmse[0] - 20.0 / 14.0_f64.sqrt()).abs() < 1e-12);
}

#[test]
fn imported_history_provenance_does_not_block_free_run() {
    let model = create_simple_1x1_model();
    let mut input = history(15);
    for mv in &mut input.mv[1..] { mv[0] = 60.0; }
    let mut document = serde_json::to_value(&input).unwrap();
    document["provenance"] = serde_json::json!({
        "synthetic": true, "source_url": "synthetic://regression", "license": "Apache-2.0"
    });
    let imported: History = serde_json::from_value(document).unwrap();
    let result = validation::validate(&model, &imported).unwrap();
    assert_eq!(result.trace.cv[1], vec![50.0]);
    assert_eq!(result.trace.cv[9], vec![55.0]);
}

#[test]
fn feedback_uses_simulated_measurements_and_enforces_mv_constraints() {
    let plant = create_simple_1x1_model();
    let mut controller = plant.clone();
    controller.variables.cvs[0].limits.target = 60.0;
    let clean = history(20);
    let mut polluted = clean.clone();
    for row in &mut polluted.cv[1..] { row[0] = -10000.0; }
    for row in &mut polluted.mv[1..] { row[0] = -10000.0; }
    let a = validation::closed_loop(&plant, &controller, &clean).unwrap();
    let b = validation::closed_loop(&plant, &controller, &polluted).unwrap();
    assert_eq!(a.trace.cv, b.trace.cv);
    assert_eq!(a.trace.mv, b.trace.mv);
    assert!(a.trace.mv[1][0] > 50.0);
    assert!(a.score.is_finite());
    assert_eq!(a.solver_failures, 0);
    for pair in a.trace.mv.windows(2) {
        assert!((10.0..=90.0).contains(&pair[1][0]));
        assert!((pair[1][0] - pair[0][0]).abs() <= 5.0 + 1e-12);
    }
}

#[test]
fn mismatched_controller_order_and_initial_bounds_are_rejected() {
    let plant = create_simple_1x1_model();
    let mut controller = plant.clone();
    controller.variables.mvs[0].name = "different actuator".into();
    assert!(validation::closed_loop(&plant, &controller, &history(3)).is_err());
    controller = plant.clone();
    controller.tuning.sample_time = 2.0;
    assert!(validation::closed_loop(&plant, &controller, &history(3)).is_err());
    controller = plant.clone();
    let mut h = history(3);
    h.mv[0][0] = 100.0;
    assert!(validation::closed_loop(&plant, &controller, &h).is_err());
}

#[test]
fn replay_cli_rejects_remote_endpoints_before_loading_files() {
    for endpoint in ["opc.tcp://example.com:4840/", "opc.tcp://10.0.0.1:4840/", "opc.tcp://localhost:4840/"] {
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_replay_plant"))
            .args(["--plant", "does-not-exist.json", "--history", "does-not-exist.json", "--opc", endpoint,
                "--namespace-index", "2", "--namespace-uri", "urn:test", "--pki", ".", "--certificate", "none.der",
                "--private-key", "none.pem", "--commissioning-profile", "unread-profile.json",
                "--acknowledge-isolated-simulator", "--acknowledge-ideal-actuator"])
            .output().unwrap();
        assert!(!result.status.success());
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(error.contains("loopback"), "{error}");
    }
}

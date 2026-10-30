mod test_helpers;
use authrity_apc_engine::{config::{ControlMethod, ControlStrategy, MvOptimizationMode, OptimizationMode, QualityBand, UnifiedModel}, dmc::{DmcController, DmcResult}};
use clarabel::solver::SolverStatus;

fn model() -> UnifiedModel {
    let mut m = test_helpers::create_simple_1x1_model();
    m.tuning.prediction_horizon = 3;
    m.tuning.control_horizon = 1;
    m.variables.cvs[0].slack_weight = 0.0;
    m.variables.mvs[0].max_move = 20.0;
    m.variables.mvs[0].weight_r = 1.0;
    m.physics.step_coefficients = vec![vec![vec![0.2, 0.6, 1.0, 1.0]]];
    m
}
fn scan(d: &mut DmcController, pv: f64, mv: f64, target: f64, weight: f64) -> DmcResult {
    d.next_move(&[pv], &[target], &[mv], &[], &[50.0], &[10.0], &[90.0], &[20.0], &[80.0], &[weight], &[0.0], &[1.0])
}
fn controller(m: &UnifiedModel) -> DmcController {
    DmcController::new_from_coefficients(m.physics.step_coefficients.clone(), vec![], m).unwrap()
}
#[test]
fn live_cv_weight_matches_fresh_hessian_and_ece_terminal_scaling() {
    let mut m = model();
    m.tuning.terminal_weight_factor = 4.0;
    m.variables.cvs[0].ece_factor = 2.0;
    let mut d = controller(&m);
    let dynamic = scan(&mut d, 50.0, 50.0, 51.0, 7.0);
    m.variables.cvs[0].weight = 7.0;
    let fresh = scan(&mut controller(&m), 50.0, 50.0, 51.0, 7.0);
    assert!((dynamic.next_move[0] - fresh.next_move[0]).abs() < 1e-5);
    // Analytic scalar unconstrained QP: sum(q*s)/(1+sum(q*s*s)).
    let expected = (7.0/4.0)*(0.2+0.6+4.0) / (1.0+(7.0/4.0)*(0.04+0.36+4.0));
    assert!((dynamic.next_move[0]-expected).abs() < 1e-4);
}
#[test]
fn actual_applied_move_not_unapplied_plan_drives_prediction() {
    let m = model();
    let mut held = controller(&m);
    let mut applied = controller(&m);
    scan(&mut held, 50.0, 50.0, 60.0, 1.0);
    scan(&mut applied, 50.0, 50.0, 60.0, 1.0);
    let h = scan(&mut held, 50.0, 50.0, 50.0, 0.0);
    let a = scan(&mut applied, 50.2, 51.0, 50.2, 0.0);
    assert!(h.predicted_pvs.iter().all(|v| (v-50.0).abs()<1e-5));
    assert!((a.predicted_pvs[0]-50.6).abs()<1e-4);
    assert!((a.predicted_pvs[1]-51.0).abs()<1e-4);
}

#[test]
fn reconnect_reset_discards_observed_increments_and_previous_prediction() {
    let m = model();
    let mut reused = controller(&m);
    scan(&mut reused, 50.0, 50.0, 55.0, 1.0);
    scan(&mut reused, 50.2, 51.0, 55.0, 1.0);
    reused.reset_observation_history();
    let restarted = scan(&mut reused, 47.0, 40.0, 49.0, 1.0);
    let fresh = scan(&mut controller(&m), 47.0, 40.0, 49.0, 1.0);
    assert_solved(&restarted);
    assert_solved(&fresh);
    assert!(restarted.cv_bias.iter().all(|v| *v == 0.0));
    for (actual, expected) in restarted.predicted_pvs.iter().zip(&fresh.predicted_pvs) {
        assert!((actual - expected).abs() < 1e-4);
    }
    assert!((restarted.next_move[0] - fresh.next_move[0]).abs() < 1e-4);
}

#[test]
fn live_envelope_preserves_native_horizon_utc_and_restart_identity() {
    use authrity_apc_engine::live_forecast::LiveForecast;
    use chrono::{DateTime, Utc};
    let m=model();
    let mut d=controller(&m);
    let solved=scan(&mut d,50.0,50.0,51.0,1.0);
    assert_solved(&solved);
    let issued=DateTime::parse_from_rfc3339("2026-10-28T01:02:03.125Z").unwrap().with_timezone(&Utc);
    let digest="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let mut publisher=LiveForecast::new(&m,"mill","instance-one",digest,false);
    let first:serde_json::Value=serde_json::from_str(publisher.encode(issued,solved.status,
        &solved.predicted_pvs,&[50.0],&solved.future_plan).unwrap()).unwrap();
    assert_eq!(first["schema_version"],1);
    assert_eq!(first["source"],"engine_solver");assert_eq!(first["time_basis"],"utc");
    assert_eq!(first["engine_id"],"mill");assert_eq!(first["instance_id"],"instance-one");
    assert_eq!(first["controller_sha256"],digest);assert_eq!(first["live_tuning"],false);
    assert_eq!(first["sequence"],1);assert_eq!(first["sample_time"],1.0);
    assert_eq!(first["cv_count"],1);assert_eq!(first["mv_count"],1);
    assert_eq!(first["prediction_horizon"],3);assert_eq!(first["control_horizon"],1);
    assert_eq!(first["layout"],"step-major");
    assert_eq!(first["cv_first_offset_steps"],1);assert_eq!(first["mv_first_offset_steps"],0);
    assert_eq!(DateTime::parse_from_rfc3339(first["issued_at"].as_str().unwrap()).unwrap().with_timezone(&Utc),issued);
    assert_eq!(first["mv_base"],serde_json::json!([50.0]));
    // Independent scalar optimum: du=(.2+.6+1)/(1+.2²+.6²+1²)=.75.
    assert!((first["mv_delta"][0].as_f64().unwrap()-0.75).abs()<1e-4);
    for (i,expected) in [50.15,50.45,50.75].into_iter().enumerate() {
        assert!((first["cv"][i].as_f64().unwrap()-expected).abs()<1e-4);
    }
    let native_cv:serde_json::Value=serde_json::from_slice(&serde_json::to_vec(&solved.predicted_pvs).unwrap()).unwrap();
    let native_delta:serde_json::Value=serde_json::from_slice(&serde_json::to_vec(&solved.future_plan).unwrap()).unwrap();
    assert_eq!(first["cv"],native_cv);assert_eq!(first["mv_delta"],native_delta);
    assert!(publisher.encode(issued,solved.status,&solved.predicted_pvs[..2],&[50.0],&solved.future_plan).is_err());
    assert!(publisher.encode(issued,solved.status,&[f64::NAN;3],&[50.0],&solved.future_plan).is_err());
    let mut failing=m.clone();failing.tuning.max_iterations=1;
    let failed=scan(&mut controller(&failing),50.0,50.0,51.0,1.0);
    assert!(!matches!(failed.status,SolverStatus::Solved|SolverStatus::AlmostSolved));
    assert!(publisher.encode(issued,failed.status,&failed.predicted_pvs,&[50.0],&failed.future_plan).is_err());
    // Reconnects reuse this owner/sequence; a new process must change instance ID.
    d.reset_observation_history();
    let next=scan(&mut d,50.0,50.0,51.0,1.0);
    let later=issued+chrono::Duration::seconds(1);
    let second:serde_json::Value=serde_json::from_str(publisher.encode(later,next.status,
        &next.predicted_pvs,&[50.0],&next.future_plan).unwrap()).unwrap();
    assert_eq!(second["sequence"],2);assert_eq!(second["instance_id"],first["instance_id"]);
    let mut restarted=LiveForecast::new(&m,"mill","instance-two",digest,true);
    let third:serde_json::Value=serde_json::from_str(restarted.encode(later,next.status,
        &next.predicted_pvs,&[50.0],&next.future_plan).unwrap()).unwrap();
    assert_eq!(third["sequence"],1);assert_ne!(third["instance_id"],first["instance_id"]);
    assert_eq!(third["live_tuning"],true);
}

#[test]
fn invalid_model_timing_and_incomplete_economic_targets_fail_before_history_update() {
    for sample in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e-20, f64::MAX] {
        let mut m = model();
        m.tuning.sample_time = sample;
        assert!(DmcController::new_from_coefficients(m.physics.step_coefficients.clone(), vec![], &m).is_err());
    }
    let mut d = controller(&model());
    let invalid = d.next_move(&[50.0], &[51.0], &[50.0], &[], &[],
        &[10.0], &[90.0], &[20.0], &[80.0], &[1.0], &[0.0], &[1.0]);
    assert_eq!(invalid.status, SolverStatus::Unsolved);
    let valid = scan(&mut d, 50.0, 50.0, 51.0, 1.0);
    assert_solved(&valid);
    assert_eq!(valid.cv_bias, vec![0.0]);
}
#[test]
fn one_step_horizon_and_infeasible_bounds_are_explicit() {
    let mut m = model(); m.tuning.prediction_horizon=1;
    let mut d=controller(&m);
    let ok=scan(&mut d,50.0,50.0,51.0,1.0);
    assert!(matches!(ok.status,SolverStatus::Solved|SolverStatus::AlmostSolved));
    let bad=d.next_move(&[50.0],&[51.0],&[50.0],&[],&[50.0],&[70.0],&[60.0],&[20.0],&[80.0],&[1.0],&[0.0],&[1.0]);
    assert!(!matches!(bad.status,SolverStatus::Solved|SolverStatus::AlmostSolved));
}

fn soft_band_model() -> UnifiedModel {
    let mut m = model();
    m.physics.step_coefficients = vec![vec![vec![1.0; 3]]];
    m.control_strategy = ControlStrategy {
        method: ControlMethod::SoftBand,
        version: 1,
        bands: vec![QualityBand { half_width: 1.0, tracking_scale: 0.01, quadratic_penalty: 100.0 }],
    };
    m
}

fn assert_solved(result: &DmcResult) {
    assert!(matches!(result.status, SolverStatus::Solved | SolverStatus::AlmostSolved), "{:?}", result.status);
}

#[test]
fn soft_band_reduces_in_band_movement_but_retains_small_target_tracking() {
    let soft = soft_band_model();
    let mut standard = soft.clone();
    standard.control_strategy = ControlStrategy::default();
    let baseline = scan(&mut controller(&standard), 50.0, 50.0, 50.5, 1.0);
    let banded = scan(&mut controller(&soft), 50.0, 50.0, 50.5, 1.0);
    assert_solved(&baseline);
    assert_solved(&banded);
    // Three unit-gain prediction samples and unit move suppression give 3*0.5/4.
    assert!((baseline.next_move[0] - 0.375).abs() < 1e-4);
    assert!(banded.next_move[0] > 0.0);
    assert!(banded.next_move[0] < baseline.next_move[0] * 0.1);
    assert!(banded.predicted_pvs.iter().all(|y| (y - 50.5).abs() < 1.0));
}

#[test]
fn quality_slack_corrects_both_sides_without_cv_tracking_cost() {
    for direction in [-1.0, 1.0] {
        let mut strong = soft_band_model();
        strong.variables.cvs[0].ece_factor = 2.0;
        let mut weak = strong.clone();
        weak.control_strategy.bands[0].quadratic_penalty = 0.01;
        let corrected = scan(&mut controller(&strong), 50.0, 50.0, 50.0 + 4.0 * direction, 0.0);
        let weakly_corrected = scan(&mut controller(&weak), 50.0, 50.0, 50.0 + 4.0 * direction, 0.0);
        assert_solved(&corrected);
        assert_solved(&weakly_corrected);
        // Quality excess is 3: min u² + 3*(100/4)*(3-u)² gives u=225/76.
        assert!((corrected.next_move[0] * direction - 225.0 / 76.0).abs() < 1e-3);
        assert!(weakly_corrected.next_move[0].abs() < 0.1);
    }
}

#[test]
fn both_methods_preserve_hard_move_and_cumulative_mv_bounds_outside_band() {
    for method in [ControlMethod::StandardDmc, ControlMethod::SoftBand] {
        for direction in [-1.0, 1.0] {
            let mut m = soft_band_model();
            if method == ControlMethod::StandardDmc { m.control_strategy = ControlStrategy::default(); }
            m.tuning.control_horizon = 3;
            m.variables.mvs[0].max_move = 0.25;
            let result = controller(&m).next_move(&[50.0], &[50.0 + 4.0 * direction], &[50.0], &[], &[50.0],
            &[49.5], &[50.5], &[20.0], &[80.0], &[1.0], &[0.0], &[1.0]);
            assert_solved(&result);
            assert!(result.next_move[0] * direction > 0.24);
            let mut mv = 50.0;
            for movement in &result.future_plan {
                assert!(movement.abs() <= 0.25001);
                mv += movement;
                assert!((49.49999..=50.50001).contains(&mv));
            }
            assert!((mv - 50.0) * direction > 0.49);
        }
    }
}

#[test]
fn quality_band_does_not_replace_operating_limit_slack() {
    let mut m = soft_band_model();
    m.control_strategy.bands[0].half_width = 2.0;
    let loose = controller(&m).next_move(&[50.0], &[50.0], &[50.0], &[], &[50.0],
        &[10.0], &[90.0], &[20.0], &[49.0], &[1.0], &[0.0], &[1.0]);
    m.variables.cvs[0].slack_weight = 1000.0;
    let constrained = controller(&m).next_move(&[50.0], &[50.0], &[50.0], &[], &[50.0],
        &[10.0], &[90.0], &[20.0], &[49.0], &[1.0], &[0.0], &[1.0]);
    assert_solved(&loose);
    assert_solved(&constrained);
    assert!(loose.next_move[0].abs() < 1e-3);
    assert!(constrained.next_move[0] < -0.99);
    assert!(constrained.predicted_pvs.iter().all(|y| *y > 48.0));
}

#[test]
fn quality_band_follows_live_target_not_alpha_filtered_reference() {
    let mut d = controller(&soft_band_model());
    let first = d.next_move(&[50.0], &[54.0], &[50.0], &[], &[50.0],
        &[10.0], &[90.0], &[20.0], &[80.0], &[1.0], &[1.0], &[1.0]);
    let second = d.next_move(&[50.0], &[46.0], &[50.0], &[], &[50.0],
        &[10.0], &[90.0], &[20.0], &[80.0], &[1.0], &[1.0], &[1.0]);
    assert_solved(&first);
    assert_solved(&second);
    assert!(first.next_move[0] > 2.9);
    assert!(second.next_move[0] < -2.9);
}

#[test]
fn invalid_strategy_settings_fail_before_solver_construction() {
    let reject = |m: UnifiedModel| {
        assert!(DmcController::new_from_coefficients(m.physics.step_coefficients.clone(), vec![], &m).is_err());
    };
    let mut m = soft_band_model(); m.control_strategy.version = 2; reject(m);
    let mut m = soft_band_model(); m.control_strategy.method = ControlMethod::StandardDmc; reject(m);
    let mut m = soft_band_model(); m.control_strategy.bands.clear(); reject(m);
    let mut m = soft_band_model(); m.control_strategy.bands.push(m.control_strategy.bands[0].clone()); reject(m);
    for value in [0.0, -1.0, f64::INFINITY, f64::NAN] {
        let mut m = soft_band_model(); m.control_strategy.bands[0].half_width = value; reject(m);
        let mut m = soft_band_model(); m.control_strategy.bands[0].tracking_scale = value; reject(m);
        let mut m = soft_band_model(); m.control_strategy.bands[0].quadratic_penalty = value; reject(m);
    }
    let mut m = soft_band_model(); m.control_strategy.bands[0].tracking_scale = 1.01; reject(m);
    for mode in [OptimizationMode::Zone, OptimizationMode::Maximize, OptimizationMode::Minimize] {
        let mut m = soft_band_model(); m.variables.cvs[0].optimization_mode = mode; reject(m);
    }
    for mode in [MvOptimizationMode::Maximize, MvOptimizationMode::Minimize] {
        let mut m = soft_band_model(); m.variables.mvs[0].optimization_mode = mode; reject(m);
    }
    let mut allowed = soft_band_model();
    allowed.variables.mvs[0].target = Some(51.0);
    allowed.variables.mvs[0].target_weight = 0.5;
    assert!(allowed.validate_control_strategy().is_ok());
}

#[test]
fn strategy_json_is_strict_and_omitted_strategy_defaults_to_standard() {
    let legacy = serde_json::json!({
        "metadata": {"name":"legacy","description":"","version":"1","model_type":"step_response"},
        "tuning": {"prediction_horizon":3,"control_horizon":1,"sample_time":1.0,"solver_tolerance":1e-6,"max_iterations":100},
        "variables": {"cvs":[],"mvs":[]}, "physics": {}
    });
    let model: UnifiedModel = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(model.control_strategy.method, ControlMethod::StandardDmc);
    assert_eq!(model.control_strategy.version, 1);
    assert!(model.control_strategy.bands.is_empty());
    for strategy in [
        serde_json::json!({"method":"unknown","version":1,"bands":[]}),
        serde_json::json!({"method":"soft_band","version":1,"bands":[],"mhe":true}),
        serde_json::json!({"method":"soft_band","bands":[]}),
        serde_json::json!({"method":"soft_band","version":1,"bands":[{"half_width":1,"tracking_scale":0.01,"quadratic_penalty":100,"extra":0}]}),
    ] {
        let mut invalid = legacy.clone();
        invalid["control_strategy"] = strategy;
        assert!(serde_json::from_value::<UnifiedModel>(invalid).is_err());
    }
}

mod test_helpers;
use authrity_apc_engine::{config::*, dmc::{DmcController, DmcResult}, native_lifecycle::{self, Action, Commissioning, Native, Request},
    native_handoff::{self, History}, replay_authority::IdealAuthority, live_forecast::LiveForecast};
use serde_json::json;
use std::time::{Duration, Instant};

fn model() -> UnifiedModel {
    let mut model = test_helpers::create_simple_1x1_model();
    model.variables.mvs[0].node_ids.applied_sp = Some("MV1:AppliedSP".into());
    model.variables.cvs[0].node_ids.limits.ll = "CV1:HardLow".into();
    model.variables.mvs[0].node_ids.limits.ll = "MV1:HardLow".into();
    model
}
fn profile(model: &UnifiedModel) -> Commissioning {
    let cv = &model.variables.cvs[0]; let mv = &model.variables.mvs[0];
    serde_json::from_value(json!({
        "profile_hash":"a".repeat(64), "model_sha256":"b".repeat(64), "engine_id":model.metadata.name,
        "endpoint":"opc.tcp://127.0.0.1:4855", "acquisition_sequence_node":"Test:AcquisitionSequence",
        "history_capacity_samples":64,"maximum_age_ms":5000,"approved_by":"commissioner","commissioning_evidence":"isolated-test-contract",
        "application_tolerance":0.001,
        "variables":[
            {"id":cv.name,"role":"CV","unit":cv.units,"pv_tag":cv.node_ids.pv,
             "target_tag":cv.node_ids.target,"low_tag":cv.node_ids.limits.low,"high_tag":cv.node_ids.limits.high,
             "hard_low":cv.limits.low_low,"hard_high":cv.limits.high_high},
            {"id":mv.name,"role":"MV","unit":mv.units,"pv_tag":mv.node_ids.pv,
             "command_tag":mv.node_ids.sp,"applied_tag":mv.node_ids.applied_sp,"mode_tag":mv.node_ids.mode,
             "op_tag":mv.node_ids.op,
             "mode_target_tag":mv.node_ids.mode_target,"low_tag":mv.node_ids.limits.low,"high_tag":mv.node_ids.limits.high,
             "command_coordinate":"applied-setpoint-percent","feedback_coordinate":"applied-setpoint-percent",
             "remote_mode":7.0,"hard_low":mv.limits.low_low,"hard_high":mv.limits.high_high,"max_move":mv.max_move}
        ],
        "handoff":{"release_tag":"Test:Release","release_value":1.0,"acquire_value":0.0,
            "acknowledgement_tag":"Test:Released","acknowledgement_value":1.0,"timeout_ms":5000,
            "watchdog_evidence":"ideal producer test","watchdog_tag":"Test:Watchdog","watchdog_timeout_ms":5000,"local_mode":2.0}
    })).unwrap()
}
fn request(native: &Native, id: &str, action: Action) -> Request {
    let status = native.status();
    Request { operation_id:id.into(), profile_hash:status.profile_hash, expected_version:status.model_sha256,
        expected_epoch:status.epoch, expected_instance:status.instance_id,
        expires_at:(chrono::Utc::now()+chrono::Duration::seconds(30)).to_rfc3339(), reason:"isolated regression".into(), action }
}
fn solve(controller: &mut DmcController, pvs: &[f64], mvs: &[f64], dvs: &[f64]) -> DmcResult {
    controller.next_move(pvs, &[50.0], mvs, dvs, &[50.0], &[10.0], &[90.0], &[20.0], &[80.0], &[1.0], &[0.0], &[0.1])
}
fn controller(model: &UnifiedModel) -> DmcController {
    DmcController::new_from_coefficients(model.physics.step_coefficients.clone(), model.physics.dv_coefficients.clone(), model).unwrap()
}
fn observed(native: &Native, sequence: f64, acquired_after: Instant) {
    native.acquired(sequence, acquired_after, &[50.0], true);
    native.observe(true, (12,11), false, Instant::now(), chrono::Utc::now(), sequence);
}

#[test]
fn native_requires_explicit_arm_and_rejects_duplicate_stale_and_restart_envelopes() {
    let model = model(); let profile = profile(&model);
    profile.validate_model(&model).unwrap();
    let native = Native::new("instance-one", &profile.model_sha256, Some(&profile));
    observed(&native, 2.0, Instant::now());
    assert!(!native.status().armed);
    assert!(native.validate(&request(&native,"engage-before-arm",Action::Engage)).is_err());
    let arm = request(&native,"arm",Action::Arm);
    native.validate(&arm).unwrap(); native.arm();
    assert!(native.validate(&arm).is_err());
    let mut stale = request(&native,"stale-epoch",Action::Engage); stale.expected_epoch -= 1;
    assert!(native.validate(&stale).is_err());
    let engage = request(&native,"engage",Action::Engage);
    native.validate(&engage).unwrap(); native.engage(); native.dispatched();
    assert!(native.status().pending_application);
    assert_eq!(native.status().mode,"armed");
    native.applied(false);
    assert_eq!(native.status().mode,"armed", "PID actual remote mode is required separately from SP readback");
    native.dispatched(); native.applied(true);
    assert_eq!(native.status().mode,"engaged");
    let old = request(&native,"old-instance",Action::Hold);
    let restarted = Native::new("instance-two", &profile.model_sha256, Some(&profile));
    assert!(restarted.validate(&old).is_err());
    assert!(!restarted.status().armed);
}

#[test]
fn stale_acquisition_and_failed_confirmation_latch_without_auto_rearm() {
    let model=model(); let profile=profile(&model); let native=Native::new("i", &profile.model_sha256, Some(&profile));
    assert!(native.validate(&request(&native,"no-history",Action::Arm)).is_err());
    observed(&native,2.0,Instant::now());
    native.validate(&request(&native,"arm",Action::Arm)).unwrap(); native.arm(); native.engage(); native.dispatched();
    native.fault("independent applied-SP confirmation timed out");
    assert!(!native.status().armed); assert!(!native.status().dcs_handoff);
    observed(&native,4.0,Instant::now());
    assert_eq!(native.status().mode,"hold");
    assert!(native.validate(&request(&native,"rearm-without-monitor",Action::Arm)).is_err());
    assert!(native.validate(&request(&native,"reengage",Action::Engage)).is_err());
    let mut expired=request(&native,"expired",Action::Hold);
    expired.expires_at=(chrono::Utc::now()-chrono::Duration::seconds(1)).to_rfc3339();
    assert!(native.validate(&expired).is_err());
}

#[test]
fn profile_rejects_wrong_coordinates_shared_feedback_and_unapproved_bounds() {
    let model=model(); let good=profile(&model);
    let mut wrong=good.clone(); wrong.variables[1].feedback_coordinate=Some("pid-output-percent".into());
    assert!(wrong.validate_model(&model).is_err());
    let mut shared=good.clone(); shared.variables[1].applied_tag=shared.variables[1].command_tag.clone();
    assert!(shared.validate_model(&model).is_err());
    let mut next=model.clone(); next.variables.mvs[0].max_move *= 2.0;
    assert!(native_handoff::compatible(&model,&next,&good).is_err());
    let mut rebound=model.clone(); rebound.variables.mvs[0].node_ids.sp="other:SP".into();
    assert!(native_handoff::compatible(&model,&rebound,&good).is_err());
    let mut tuned=model.clone(); tuned.variables.cvs[0].weight=2.0; tuned.variables.cvs[0].alpha=0.5;
    tuned.variables.mvs[0].weight_r=0.2; tuned.tuning.prediction_horizon=12;
    native_handoff::compatible(&model,&tuned,&good).unwrap();
    let mut aliased=model.clone(); aliased.variables.mvs[0].node_ids.mode_target=aliased.variables.mvs[0].node_ids.mode.clone();
    let mut aliased_profile=good.clone(); aliased_profile.variables[1].mode_target_tag=Some(aliased.variables.mvs[0].node_ids.mode.clone());
    assert!(aliased_profile.validate_model(&aliased).is_err(),"Mode request readback cannot impersonate actual PID mode");
    let mut aliased_protocol=good.clone();aliased_protocol.handoff.release_tag=model.variables.mvs[0].node_ids.sp.clone();
    assert!(aliased_protocol.validate_model(&model).is_err(),"Release must not write the MV command coordinate");
    let mut aliased_protocol=good.clone();aliased_protocol.handoff.watchdog_tag=good.acquisition_sequence_node.clone();
    assert!(aliased_protocol.validate_model(&model).is_err(),"Native liveness must not impersonate producer acquisition");
    let mut aliased_protocol=good.clone();aliased_protocol.handoff.acknowledgement_tag=model.variables.cvs[0].node_ids.target.clone();
    assert!(aliased_protocol.validate_model(&model).is_err(),"Release ACK must not be an operator setting");
}

#[test]
fn transient_model_handoff_reconstructs_mv_and_dv_memory_without_old_predictions() {
    let mut next=model();
    next.variables.dvs.push(DvConfig { name:"DV1".into(),description:"measured disturbance".into(),units:"%".into(),
        limits:DvLimits{low:0.0,high:100.0},node_ids:DvNodes{pv:"DV1:PV".into(),limits:DvLimitNodes{low:"DV1:Low".into(),high:"DV1:High".into()}} });
    next.physics.step_coefficients[0][0]=vec![0.0,0.0,0.2,0.5,0.7,0.8,0.8,0.8,0.8,0.8];
    next.physics.dv_coefficients=vec![vec![vec![0.0,0.0,0.0,0.3,0.6,0.6,0.6,0.6,0.6,0.6]]];
    let mut full=controller(&next); let mut history=History::new(64);
    for t in 0..20 {
        let mv=50.0+if t>=17 {2.0}else{0.0}; let dv=20.0+if t>=18 {3.0}else{0.0};
        solve(&mut full,&[50.0],&[mv],&[dv]); history.observe(&[50.0],&[mv],&[dv],t+1);
    }
    let mut rebuilt=controller(&next); history.reconstruct(&mut rebuilt).unwrap();
    let reference=solve(&mut full,&[50.4],&[52.0],&[23.0]);
    let restored=solve(&mut rebuilt,&[50.4],&[52.0],&[23.0]);
    for (a,b) in reference.predicted_pvs.iter().zip(&restored.predicted_pvs) { assert!((a-b).abs()<1e-7); }
    assert!(restored.cv_bias.iter().all(|value| value.is_finite()));
    let mut zero=controller(&next); let shortcut=solve(&mut zero,&[50.4],&[52.0],&[23.0]);
    assert!(shortcut.predicted_pvs.iter().zip(&restored.predicted_pvs).any(|(a,b)|(a-b).abs()>1e-3),
        "PV-as-steady/zero-history initialization must not match a transient reconstruction");
    let mut missing=History::new(64); missing.observe(&[50.0],&[50.0],&[20.0],1);
    assert!(missing.reconstruct(&mut controller(&next)).is_err());
    history.observe(&[50.0],&[50.0],&[20.0],1);
    assert!(history.reconstruct(&mut controller(&next)).is_err(), "A discontinuity discards old usable history");
}

#[test]
fn reconstructed_estimator_excludes_unexecuted_candidate_actions() {
    let mut next=model();next.physics.step_coefficients[0][0]=vec![1.0;10];
    let mut history=History::new(64);
    for t in 0..20 { history.observe(&[45.0],&[50.0],&[],t+1); }
    let mut rebuilt=controller(&next);history.reconstruct(&mut rebuilt).unwrap();
    let result=solve(&mut rebuilt,&[45.0],&[50.0],&[]);
    assert!(result.next_move[0]>0.1,"This model would propose a nonzero counterfactual action");
    assert!(result.cv_bias[0].abs()<1e-10,"Historical estimator must not include that unexecuted action");
}

#[test]
fn candidate_first_action_and_horizon_are_independently_bounded() {
    let model=model(); let prediction=vec![50.0;model.tuning.prediction_horizon];
    native_handoff::validate_first_action(&model,&[0.5],&prediction,&[1.0]).unwrap();
    assert!(native_handoff::validate_first_action(&model,&[1.001],&prediction,&[1.0]).is_err());
    assert!(native_handoff::validate_first_action(&model,&[0.0],&prediction,&[model.variables.mvs[0].max_move+1.0]).is_err());
    let mut unsafe_prediction=prediction; unsafe_prediction[3]=model.variables.cvs[0].limits.high_high+0.1;
    assert!(native_handoff::validate_first_action(&model,&[0.1],&unsafe_prediction,&[1.0]).is_err());
}

#[test]
fn fixture_watchdog_and_release_require_explicit_new_acquire_cycle() {
    let model=model();let profile=profile(&model);let h=&profile.handoff;let now=Instant::now();let mut gate=IdealAuthority::default();
    assert!(!gate.observe(h,1.0,1.0,&[2.0],&[7.0],now).unwrap());
    assert!(gate.observe(h,0.0,2.0,&[7.0],&[7.0],now+Duration::from_secs(1)).unwrap());
    assert!(!gate.observe(h,0.0,2.0,&[7.0],&[7.0],now+Duration::from_secs(7)).unwrap());
    assert!(!gate.observe(h,0.0,3.0,&[7.0],&[7.0],now+Duration::from_secs(8)).unwrap(), "Renewed heartbeat cannot rearm a tripped actuator");
    assert!(!gate.observe(h,1.0,4.0,&[7.0],&[7.0],now+Duration::from_secs(9)).unwrap());
    assert!(gate.observe(h,0.0,5.0,&[7.0],&[7.0],now+Duration::from_secs(10)).unwrap());
    assert!(!gate.observe(h,1.0,6.0,&[7.0],&[7.0],now+Duration::from_secs(11)).unwrap());
}

#[test]
fn forecast_cutover_changes_digest_and_horizons_without_restarting_sequence() {
    let mut model=model();let mut forecast=LiveForecast::new(&model,"engine","instance","old",false);
    let first:serde_json::Value=serde_json::from_str(forecast.encode(chrono::Utc::now(),clarabel::solver::SolverStatus::Solved,
        &vec![50.0;10],&[50.0],&[0.0;3]).unwrap()).unwrap();
    model.tuning.prediction_horizon=8;model.tuning.control_horizon=2;
    forecast.rebind(&model,"new");
    let next:serde_json::Value=serde_json::from_str(forecast.encode(chrono::Utc::now(),clarabel::solver::SolverStatus::Solved,
        &vec![50.0;8],&[50.0],&[0.0;2]).unwrap()).unwrap();
    assert_eq!(next["controller_sha256"],"new");assert_eq!(next["prediction_horizon"],8);
    assert_eq!(next["control_horizon"],2);assert_eq!(next["instance_id"],first["instance_id"]);
    assert!(next["sequence"].as_u64().unwrap()>first["sequence"].as_u64().unwrap());
}

#[tokio::test]
async fn private_jsonl_transport_records_original_result_and_enforces_one_writer() {
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncBufReadExt,AsyncWriteExt,BufReader};
    let root=std::env::temp_dir().join(format!("authrity-native-{}-{}",std::process::id(),chrono::Utc::now().timestamp_nanos_opt().unwrap()));
    std::fs::create_dir(&root).unwrap();std::fs::set_permissions(&root,std::fs::Permissions::from_mode(0o700)).unwrap();
    let instance=root.join("instance");std::fs::create_dir(&instance).unwrap();
    std::fs::set_permissions(&instance,std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket=instance.join("a.sock");let model=model();let profile=profile(&model);let native=Native::new("i",&profile.model_sha256,Some(&profile));
    let locks=native_lifecycle::lock_writers(&socket,&profile).unwrap();
    assert!(native_lifecycle::lock_writers(&socket,&profile).is_err());
    native.own_writers(locks).unwrap();
    native.acquired(2.0,Instant::now(),&[50.0],true);
    assert!(native.status().safe_baseline);
    assert!(!native.status().healthy,"Released acquisition is not a qualified model solve");
    native.acquired(4.0,Instant::now(),&[50.0],false);
    native.released(false);
    assert!(!native.status().safe_baseline,"A sticky old release ACK cannot certify a current baseline");
    native.acquired(6.0,Instant::now()-Duration::from_secs(6),&[50.0],true);
    assert!(!native.status().safe_baseline,"A released but stale snapshot cannot clear unknown intent");
    native.acquired(8.0,Instant::now(),&[50.0],true);
    assert!(native.status().safe_baseline);
    let mut receiver=native_lifecycle::listen(&socket,native.clone()).unwrap();
    observed(&native,10.0,Instant::now());
    let request=request(&native,"arm-over-socket",Action::Arm);
    let stream=tokio::net::UnixStream::connect(&socket).await.unwrap();let(mut reader,mut writer)=stream.into_split();
    writer.write_all(format!("{}\n",json!({"kind":"command","request":request})).as_bytes()).await.unwrap();
    let envelope=tokio::time::timeout(Duration::from_secs(2),receiver.recv()).await.unwrap().unwrap();
    native.validate(&envelope.request).unwrap();native.arm();native.finish(envelope,true,true,"armed locally");
    let mut line=String::new();BufReader::new(&mut reader).read_line(&mut line).await.unwrap();
    let result:serde_json::Value=serde_json::from_str(&line).unwrap();assert_eq!(result["confirmed"],true);
    let mut query=tokio::net::UnixStream::connect(&socket).await.unwrap();
    query.write_all(b"{\"kind\":\"operation\",\"operation_id\":\"arm-over-socket\"}\n").await.unwrap();
    line.clear();BufReader::new(query).read_line(&mut line).await.unwrap();
    let original:serde_json::Value=serde_json::from_str(&line).unwrap();assert_eq!(original,result);
    assert!(std::fs::metadata(instance.join("operations.ndjson")).unwrap().len()>0);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn current_acquisition_is_separate_from_slow_model_solve_age() {
    let mut model=model();model.tuning.sample_time=60.0;
    let profile=profile(&model);let native=Native::new("slow",&profile.model_sha256,Some(&profile));
    native.model_metadata(&model);
    observed(&native,2.0,Instant::now()-Duration::from_secs(6));
    assert!(!native.status().healthy);
    assert!(native.validate(&request(&native,"stale-acquired",Action::Arm)).is_err());
    let before=native.status().observed_at;
    native.acquired(4.0,Instant::now(),&[50.0],true);
    assert!(native.status().healthy);
    assert_eq!(native.status().observed_at,before,"Inter-scan acquisition must not invent another model solve");
    assert_eq!(native.status().history_samples,12,"Inter-scan polls cannot warm model history");
    assert_eq!(native.status().maximum_solve_age_ms,75_000);
    assert_eq!(native.status().solve_acquisition_sequence,Some(2));
    native.validate(&request(&native,"fresh-acquired",Action::Arm)).unwrap();
    native.observe(true,(12,11),false,Instant::now()-Duration::from_secs(76),
        chrono::Utc::now()-chrono::Duration::seconds(76),4.0);
    native.acquired(6.0,Instant::now(),&[50.0],true);
    assert!(!native.status().healthy,"Fresh acquisition cannot revive an expired model solve");
}

#[test]
fn approved_cold_start_keeps_original_profile_and_requires_independent_provenance() {
    use authrity_apc_engine::native_approval;
    use sha2::{Digest,Sha256};
    let profile=json!({"profile_hash":"a".repeat(64),"model_sha256":"c".repeat(64),
        "target":{"plant":"cement","line":"one","area":"mill","group":"MPC"},
        "operating_envelope":"commissioned-envelope","equipment_instance_ids":["mill-one"]});
    let model_hash="b".repeat(64);let candidate="d".repeat(64);
    let manifest=json!({"schema_version":1,"id":candidate,"model_sha256":model_hash,
        "deployment_profile_hash":profile["profile_hash"],"target":profile["target"],
        "operating_envelope":profile["operating_envelope"],"equipment_instance_ids":profile["equipment_instance_ids"],
        "author_user_id":1,"author_identity_realm":"site-a","authority":"review_only",
        "assessment":{"eligible":true,"reasons":[]},"model_test":{"passed":true,"used_for_selection":false}});
    let bytes=serde_json::to_string(&manifest).unwrap();
    let proof=json!({"kind":"manual_review","candidate_hash":candidate,"model_sha256":model_hash,
        "manifest_sha256":format!("{:x}",Sha256::digest(bytes.as_bytes())),
        "profile_hash":profile["profile_hash"],"target":profile["target"],"operating_envelope":profile["operating_envelope"],
        "equipment_instance_ids":profile["equipment_instance_ids"],"author_id":1,"approver_id":2,
        "author_identity_realm":"site-a","approver_identity_realm":"site-a",
        "expires_at":(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339()});
    let bundle=json!({"approval":proof,"manifest_json":bytes});
    native_approval::validate(&bundle,&profile,&model_hash).unwrap();
    assert_eq!(profile["model_sha256"],"c".repeat(64),"Approval must not rewrite the baseline commissioning identity");
    for (field,value) in [("approver_id",json!(1)),("approver_identity_realm",json!("other-site")),
        ("profile_hash",json!("e".repeat(64))),("model_sha256",json!("e".repeat(64))),
        ("expires_at",json!((chrono::Utc::now()-chrono::Duration::seconds(1)).to_rfc3339()))] {
        let mut invalid=bundle.clone();invalid["approval"][field]=value;
        assert!(native_approval::validate(&invalid,&profile,&model_hash).is_err(),"accepted invalid {field}");
    }
    let mut corrupted=bundle;corrupted["manifest_json"]=json!("{}");
    assert!(native_approval::validate(&corrupted,&profile,&model_hash).is_err());
}

#[test]
fn restarted_native_identity_does_not_depend_on_pid_or_wall_clock() {
    let first=authrity_apc_engine::native_lifecycle::new_instance_id().unwrap();
    let second=authrity_apc_engine::native_lifecycle::new_instance_id().unwrap();
    assert_eq!(first.len(),36);assert_eq!(second.len(),36);assert_ne!(first,second);
    assert_eq!(&first[14..15],"4");assert!(matches!(&first[19..20],"8"|"9"|"a"|"b"));
}

#[test]
fn pre_acquire_alignment_rejects_stale_register_and_changed_physical_baseline() {
    native_handoff::validate_alignment(&[50.0],&[50.0],&[50.0],0.001).unwrap();
    assert!(native_handoff::validate_alignment(&[50.0],&[45.0],&[50.0],0.001).is_err());
    assert!(native_handoff::validate_alignment(&[50.0],&[50.0],&[51.0],0.001).is_err());
    assert!(native_handoff::validate_alignment(&[50.0],&[],&[50.0],0.001).is_err());
}

#[test]
fn forecast_and_native_status_share_exact_solve_identity() {
    use authrity_apc_engine::live_forecast::LiveForecast;
    let model=model();let profile=profile(&model);
    let native=Native::new("one-instance",&profile.model_sha256,Some(&profile));
    native.model_metadata(&model);native.acquired(2.0,Instant::now(),&[50.0],true);
    let mut forecast=LiveForecast::new(&model,&profile.engine_id,"one-instance",&profile.model_sha256,false);
    let issued=chrono::Utc::now();
    native.observe(true,(12,11),false,Instant::now(),issued,2.0);
    let sequence=forecast.sequence()+1;
    let payload:serde_json::Value=serde_json::from_str(forecast.encode(issued,clarabel::solver::SolverStatus::Solved,
        &vec![50.0;model.tuning.prediction_horizon],&[50.0],&vec![0.0;model.tuning.control_horizon]).unwrap()).unwrap();
    native.forecast_sequence(sequence);
    assert_eq!(payload["issued_at"].as_str(),native.status().observed_at.as_deref());
    assert_eq!(payload["sequence"].as_u64(),native.status().forecast_sequence);
    assert_eq!(payload["instance_id"],native.status().instance_id);
    native.acquired(4.0,Instant::now(),&[50.0],true);
    assert_eq!(native.status().forecast_sequence,Some(sequence),"Acquisition polling never renews a forecast");
    native.cutover(&"b".repeat(64));
    assert!(!native.status().healthy);assert!(native.status().forecast_sequence.is_none());
    assert!(native.status().observed_at.is_none(),"Cutover cannot relabel an old model solve");
}

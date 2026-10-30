mod test_helpers;
use authrity_apc_engine::{campaign::{closed_loop,replay},campaign_plant::{GenerativePlant,PathSpec,PlantSpec,Scenario}};

fn spec()->PlantSpec {PlantSpec{sample_time:1.0,baseline_cv:vec![50.0],baseline_mv:vec![50.0],baseline_dv:vec![],actuator_tau:vec![0.0],actuator_low:vec![10.0],actuator_high:vec![90.0],actuator_rate:vec![2.0],paths:vec![vec![PathSpec{gain:1.0,tau1:0.0,tau2:0.0,delay_seconds:1.0}]],nonlinear_strength:vec![0.0],nonlinear_scale:vec![10.0]}}
fn scenario(n:usize)->Scenario {Scenario{sample_time:1.0,time_seconds:(0..n).map(|t|t as f64).collect(),cv:vec![vec![50.0];n],mv:vec![vec![55.0];n],dv:vec![vec![];n],targets:vec![vec![52.0];n],measurement_noise:vec![],process_noise:vec![],gain_scale:vec![],faults:vec![]}}
#[test]
fn delayed_plant_actuator_noise_and_replay_coordinates_are_distinct(){
    let p=spec();let mut plant=GenerativePlant::new(p.clone()).unwrap();
    plant.advance(&[60.0],&[],1.0,None).unwrap();
    assert_eq!(plant.actuators(),&[52.0]);assert_eq!(plant.output(),&[50.0]);
    plant.advance(&[60.0],&[],2.0,Some(&[0.5])).unwrap();
    assert_eq!(plant.actuators(),&[54.0]);assert_eq!(plant.output(),&[54.5]);
    let mut sensor=[0.0];plant.observe(Some(&[0.25]),&mut sensor);assert_eq!(sensor,[54.75]);assert_eq!(plant.output(),&[54.5]);
    let r=replay(&p,&scenario(4)).unwrap();
    assert_eq!(r["mv"][0][0],50.0);assert_eq!(r["command_mv"][0][0],55.0);assert_eq!(r["cv"].as_array().unwrap().len(),4);
}
#[test]
fn fractional_deadtime_interpolates_zero_prehistory(){
    let mut p=spec();p.paths[0][0].delay_seconds=0.5;
    let mut plant=GenerativePlant::new(p).unwrap();plant.advance(&[52.0],&[],1.0,None).unwrap();
    assert_eq!(plant.output(),&[51.0]);
    plant.advance(&[52.0],&[],1.0,None).unwrap();assert_eq!(plant.output(),&[52.0]);
}
#[test]
fn feedback_solves_every_row_and_tracks_dynamic_targets(){
    let mut m=test_helpers::create_simple_1x1_model();m.variables.mvs[0].max_move=2.0;
    let mut s=scenario(20);s.targets[10..].fill(vec![49.0]);
    let result=closed_loop(&spec(),&m,&s,None,&[],None).unwrap();
    assert_eq!(result.status,"valid","{:?}",result.reason);assert_eq!(result.samples,20);assert_eq!(result.solver.calls,20);
    assert_eq!(result.trace.targets[10],vec![49.0]);assert!(result.metrics.mv_hard_violations.iter().all(|n|*n==0));
    assert!(result.trace.command_mv.iter().zip(&result.trace.mv).any(|(command,actual)|command!=actual));
}
#[test]
fn sample_mismatch_and_failed_solver_do_not_become_successful_holds(){
    let mut s=scenario(10);s.sample_time=2.0;assert!(replay(&spec(),&s).is_err());
    let mut m=test_helpers::create_simple_1x1_model();m.tuning.max_iterations=1;
    let result=closed_loop(&spec(),&m,&scenario(10),None,&[],None).unwrap();
    assert_eq!(result.status,"invalid");assert!(result.reason.is_some());assert_eq!(result.solver.failures,1);assert_eq!(result.samples,0);
}

#[test]
fn campaign_executes_and_reports_both_strategies_with_feedback() {
    use authrity_apc_engine::config::{ControlMethod, ControlStrategy, QualityBand};
    let mut plant = spec();
    plant.paths[0][0].delay_seconds = 0.0;
    let mut model = test_helpers::create_simple_1x1_model();
    model.tuning.prediction_horizon = 3;
    model.tuning.control_horizon = 1;
    model.physics.step_coefficients = vec![vec![vec![1.0; 3]]];
    model.variables.mvs[0].max_move = 0.5;
    model.variables.mvs[0].weight_r = 1.0;
    let mut schedule = scenario(30);
    schedule.targets[..10].fill(vec![50.5]);
    schedule.targets[10..].fill(vec![54.0]);
    let standard = closed_loop(&plant, &model, &schedule, None, &[], None).unwrap();
    model.control_strategy = ControlStrategy {
        method: ControlMethod::SoftBand, version: 1,
        bands: vec![QualityBand { half_width: 1.0, tracking_scale: 0.01, quadratic_penalty: 100.0 }],
    };
    let soft = closed_loop(&plant, &model, &schedule, None, &[], None).unwrap();
    assert_eq!(standard.control_strategy.method, ControlMethod::StandardDmc);
    assert_eq!(soft.control_strategy.method, ControlMethod::SoftBand);
    assert!((soft.trace.command_mv[0][0] - 50.0).abs()
        < (standard.trace.command_mv[0][0] - 50.0).abs() * 0.1);
    for result in [&standard, &soft] {
        assert_eq!(result.status, "valid", "{:?}", result.reason);
        assert_eq!(result.samples, 30);
        assert_eq!(result.solver.calls, 30);
        assert_eq!(result.solver.failures, 0);
        assert_eq!(result.metrics.mv_hard_violations, vec![0]);
        assert!(result.trace.cv.last().unwrap()[0] > 52.9);
        let mut previous = 50.0;
        for command in &result.trace.command_mv {
            assert!((10.0..=90.0).contains(&command[0]));
            assert!((command[0] - previous).abs() <= 0.50001);
            previous = command[0];
        }
    }
}

struct ArchiveDir(std::path::PathBuf);
impl ArchiveDir {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("authrity-forecast-{}-{timestamp}-{sequence}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> std::path::PathBuf { self.0.join(name) }
}
impl Drop for ArchiveDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn read_forecast(path: &std::path::Path) -> (serde_json::Value, Vec<serde_json::Value>) {
    use std::io::{BufRead, Seek, SeekFrom};
    let mut index_path = path.as_os_str().to_os_string();
    index_path.push(".index.json");
    let index: serde_json::Value = serde_json::from_slice(&std::fs::read(index_path).unwrap()).unwrap();
    let mut input = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let mut line = String::new();
    input.read_line(&mut line).unwrap();
    let header: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(index["header"], header);
    let records = index["offsets"].as_array().unwrap().iter().map(|offset| {
        input.seek(SeekFrom::Start(offset.as_u64().unwrap())).unwrap();
        line.clear();
        input.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }).collect::<Vec<_>>();
    assert_eq!(index["samples"].as_u64().unwrap() as usize, records.len());
    (header, records)
}

fn without_timing(result: &authrity_apc_engine::campaign::CampaignResult) -> serde_json::Value {
    let mut value = serde_json::to_value(result).unwrap();
    for section in ["solver", "metrics"] {
        for field in ["mean_solve_ms", "max_solve_ms"] {
            value[section].as_object_mut().unwrap().remove(field);
        }
    }
    value["trace"].as_object_mut().unwrap().remove("solve_ms");
    value
}

#[test]
fn archived_mimo_forecasts_have_physical_timestamps_and_absolute_plans_without_changing_control() {
    let dir = ArchiveDir::new();
    let path = dir.path("mimo.ndjson");
    let mut model = test_helpers::create_2x2_model();
    model.tuning.sample_time = 2.5;
    model.tuning.prediction_horizon = 4;
    model.tuning.control_horizon = 2;
    model.physics.step_coefficients = vec![
        vec![vec![1.0; 4], vec![0.0; 4]],
        vec![vec![0.0; 4], vec![2.0; 4]],
    ];
    let plant = PlantSpec {
        sample_time: 2.5, baseline_cv: vec![50.0, 45.0], baseline_mv: vec![50.0, 40.0], baseline_dv: vec![],
        actuator_tau: vec![0.0; 2], actuator_low: vec![10.0; 2], actuator_high: vec![90.0; 2], actuator_rate: vec![100.0; 2],
        paths: vec![
            vec![PathSpec {gain: 1.0, tau1: 0.0, tau2: 0.0, delay_seconds: 0.0}, PathSpec {gain: 0.0, tau1: 0.0, tau2: 0.0, delay_seconds: 0.0}],
            vec![PathSpec {gain: 0.0, tau1: 0.0, tau2: 0.0, delay_seconds: 0.0}, PathSpec {gain: 2.0, tau1: 0.0, tau2: 0.0, delay_seconds: 0.0}],
        ],
        nonlinear_strength: vec![0.0; 2], nonlinear_scale: vec![10.0; 2],
    };
    let mut schedule = scenario(6);
    schedule.sample_time = 2.5;
    schedule.time_seconds = (0..6).map(|i| 12.5 + i as f64 * 2.5).collect();
    schedule.cv.fill(vec![50.0, 45.0]);
    schedule.mv.fill(vec![50.0, 40.0]);
    schedule.targets.fill(vec![52.0, 41.0]);
    let plain = closed_loop(&plant, &model, &schedule, None, &[], None).unwrap();
    let recorded = closed_loop(&plant, &model, &schedule, None, &[], Some(&path)).unwrap();
    assert_eq!(recorded.status, "valid", "{:?}", recorded.reason);
    assert_eq!(without_timing(&plain), without_timing(&recorded));
    let (header, records) = read_forecast(&path);
    assert_eq!(header["cv_first_offset_steps"], 1);
    assert_eq!(header["mv_first_offset_steps"], 0);
    assert_eq!(header["layout"], "step-major");
    let (_,replies)=run_session_fixture(&dir,"mimo-session.json",&plant,&model,&schedule,&[
        serde_json::json!({"request_id":"first","command":"advance","expected_samples":0,"count":1}),
        serde_json::json!({"request_id":"paused","command":"snapshot"}),
        serde_json::json!({"request_id":"last","command":"advance","expected_samples":1,"count":5}),
        serde_json::json!({"request_id":"terminal","command":"snapshot"}),
    ],None);
    assert_eq!(replies[0]["latest_forecast"]["issued_at_seconds"],12.5);
    assert_eq!(replies[0]["latest_forecast"]["sample_time"],2.5);
    assert_eq!(replies[0]["latest_forecast"]["cv"],records[0]["cv"]);
    assert_eq!(replies[0]["latest_forecast"]["mv_delta"],records[0]["mv_delta"]);
    assert_eq!(replies[0]["latest_forecast"],replies[1]["latest_forecast"]);
    assert_eq!(replies[2]["latest_forecast"]["issued_at_seconds"],25.0);
    assert_eq!(replies[2]["latest_forecast"]["cv"],records[5]["cv"]);
    assert_eq!(replies[2]["latest_forecast"]["mv_delta"],records[5]["mv_delta"]);
    assert_eq!(replies[2]["latest_forecast"],replies[3]["latest_forecast"]);
    for (sample, record) in records.iter().enumerate() {
        assert_eq!(record["sample"], sample);
        let issued = record["issued_at_seconds"].as_f64().unwrap();
        assert_eq!(issued, schedule.time_seconds[sample]);
        let mut absolute = [record["mv_base"][0].as_f64().unwrap(), record["mv_base"][1].as_f64().unwrap()];
        let previous = if sample == 0 { &plant.baseline_mv } else { &recorded.trace.command_mv[sample - 1] };
        assert_eq!(absolute.as_slice(), previous.as_slice());
        for step in 0..4 {
            if step < 2 {
                for (j, value) in absolute.iter_mut().enumerate() {
                    *value += record["mv_delta"][step * 2 + j].as_f64().unwrap();
                }
            }
            let plan_time = issued + step as f64 * 2.5;
            let cv_valid_time = issued + (step + 1) as f64 * 2.5;
            assert_eq!(cv_valid_time - plan_time, 2.5);
            for j in 0..2 {
                // This exact static plant makes every horizon point independently
                // checkable from the cumulative plan, including the held terminal move.
                let expected = plant.baseline_cv[j] + [1.0, 2.0][j] * (absolute[j] - plant.baseline_mv[j]);
                let prediction = record["cv"][step * 2 + j].as_f64().unwrap();
                assert!((prediction - expected).abs() < 1e-6, "sample {sample} step {step} CV {j}");
                if step == 0 {
                    assert!((absolute[j] - recorded.trace.command_mv[sample][j]).abs() < 1e-6);
                    assert_eq!(plan_time, recorded.trace.time_seconds[sample]);
                    if sample + 1 < records.len() {
                        assert_eq!(cv_valid_time, recorded.trace.time_seconds[sample + 1]);
                        assert!((prediction - recorded.trace.cv[sample + 1][j]).abs() < 1e-6);
                    }
                }
            }
        }
    }
    assert_eq!(records.len(), schedule.time_seconds.len());
}

#[test]
fn forecast_excludes_failed_scans_and_finalizes_only_the_successful_prefix() {
    use authrity_apc_engine::campaign_plant::Fault;
    let dir = ArchiveDir::new();
    let mut model = test_helpers::create_simple_1x1_model();
    model.tuning.max_iterations = 1;
    let failed_path = dir.path("failed.ndjson");
    let failed = closed_loop(&spec(), &model, &scenario(5), None, &[], Some(&failed_path)).unwrap();
    assert_eq!(failed.status, "invalid");
    assert_eq!(failed.solver.failures, 1);
    assert!(read_forecast(&failed_path).1.is_empty());

    model.tuning.max_iterations = 100;
    model.variables.mvs[0].max_move = 2.0;
    let mut schedule = scenario(5);
    // Direct mode rejects transport fault requests rather than pretending COM IO succeeded.
    schedule.faults.push(Fault { sample: 3, kind: "disconnect".into(), items: vec![] });
    let path = dir.path("prefix.ndjson");
    let result = closed_loop(&spec(), &model, &schedule, None, &[], Some(&path)).unwrap();
    assert_eq!(result.status, "invalid");
    assert_eq!(result.samples, 3);
    let records = read_forecast(&path).1;
    assert_eq!(records.len(), 3);
    assert_eq!(records.last().unwrap()["sample"], 2);
}

#[test]
fn forecast_paths_never_overwrite_existing_data_or_index() {
    let dir = ArchiveDir::new();
    let model = test_helpers::create_simple_1x1_model();
    let path = dir.path("existing.ndjson");
    std::fs::write(&path, b"immutable data").unwrap();
    assert!(closed_loop(&spec(), &model, &scenario(2), None, &[], Some(&path)).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"immutable data");
    let other = dir.path("other.ndjson");
    let index = dir.path("other.ndjson.index.json");
    std::fs::write(&index, b"immutable index").unwrap();
    assert!(closed_loop(&spec(), &model, &scenario(2), None, &[], Some(&other)).is_err());
    assert_eq!(std::fs::read(&index).unwrap(), b"immutable index");
    assert!(closed_loop(&spec(), &model, &scenario(2), None, &[], Some(&dir.path("missing/forecast.ndjson"))).is_err());
}

#[test]
fn campaign_cli_streams_forecasts_and_reports_unusable_archive_paths() {
    let dir = ArchiveDir::new();
    let plant = dir.path("plant.json");
    let controller = dir.path("controller.json");
    let schedule_path = dir.path("scenario.json");
    let model = serde_json::json!({
        "metadata": {"name":"ForecastCLI", "description":"Archive integration fixture", "version":"1", "model_type":"step_response"},
        "tuning": {"prediction_horizon":10, "control_horizon":3, "sample_time":1.0, "solver_tolerance":1e-6, "max_iterations":100, "terminal_weight_factor":1.0},
        "variables": {
            "cvs": [{"name":"CV", "description":"Measured output", "units":"%", "weight":1.0,
                "limits":{"low_low":10.0,"low":20.0,"target":50.0,"high":80.0,"high_high":90.0},
                "node_ids":{"pv":"CV:PV","target":"CV:Target","prediction":"CV:Prediction","limits":{"low":"CV:Low","high":"CV:High","ll":"CV:LL","hh":"CV:HH"}}}],
            "mvs": [{"name":"MV", "description":"Applied input", "units":"%", "weight_r":0.1, "max_move":2.0,
                "limits":{"low_low":0.0,"low":10.0,"high":90.0,"high_high":100.0},
                "node_ids":{"pv":"MV:PV","sp":"MV:SP","op":"MV:OP","mode":"MV:Mode","mode_target":"MV:ModeTarget","future_plan":"MV:FuturePlan","limits":{"low":"MV:Low","high":"MV:High","ll":"MV:LL","hh":"MV:HH"}}}]
        },
        "physics":{"step_coefficients":[[[0.0,0.0,0.1,0.25,0.4,0.5,0.5,0.5,0.5,0.5]]]}
    });
    let schedule = scenario(4);
    std::fs::write(&plant, serde_json::to_vec(&spec()).unwrap()).unwrap();
    std::fs::write(&controller, serde_json::to_vec(&model).unwrap()).unwrap();
    std::fs::write(&schedule_path, serde_json::to_vec(&serde_json::json!({
        "sample_time": schedule.sample_time, "time_seconds": schedule.time_seconds,
        "cv": schedule.cv, "mv": schedule.mv, "dv": schedule.dv, "targets": schedule.targets,
    })).unwrap()).unwrap();
    let invoke = |output: &std::path::Path, forecast: &std::path::Path| {
        std::process::Command::new(env!("CARGO_BIN_EXE_mpc_campaign"))
            .arg("closed-loop").arg("--plant").arg(&plant)
            .arg("--controller").arg(&controller).arg("--scenario").arg(&schedule_path)
            .arg("--output").arg(output).arg("--forecast-output").arg(forecast)
            .output().unwrap()
    };
    let output = dir.path("result.json");
    let forecast = dir.path("cli.ndjson");
    let completed = invoke(&output, &forecast);
    assert!(completed.status.success(), "{}", String::from_utf8_lossy(&completed.stderr));
    let result: serde_json::Value = serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    let (_, records) = read_forecast(&forecast);
    assert_eq!(result["status"], "valid");
    assert_eq!(records.len(), 4);
    assert_eq!(records[0]["mv_base"][0], 50.0);
    assert_eq!(records[3]["cv"][0], result["trace"]["predicted_cv"][3][0]);

    let failed_output = dir.path("invalid.json");
    let failed = invoke(&failed_output, &dir.path("absent/forecast.ndjson"));
    assert!(!failed.status.success());
    let result: serde_json::Value = serde_json::from_slice(&std::fs::read(&failed_output).unwrap()).unwrap();
    assert_eq!(result["status"], "invalid");
    assert_eq!(result["samples"], 0);
    assert!(result["reason"].as_str().unwrap().contains("Forecast output must be a new file"));

    use std::io::Write;
    let session_output=dir.path("session-cli.json");
    let session_forecast=dir.path("session-cli.ndjson");
    let mut child=std::process::Command::new(env!("CARGO_BIN_EXE_mpc_campaign"))
        .arg("session").arg("--plant").arg(&plant)
        .arg("--controller").arg(&controller).arg("--scenario").arg(&schedule_path)
        .arg("--output").arg(&session_output).arg("--forecast-output").arg(&session_forecast)
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped()).spawn().unwrap();
    {
        let mut input=child.stdin.take().unwrap();
        for request in [
            serde_json::json!({"request_id":"initial","command":"snapshot"}),
            serde_json::json!({"request_id":"run","command":"advance","expected_samples":0,"count":4}),
            serde_json::json!({"request_id":"final","command":"snapshot"}),
            serde_json::json!({"request_id":"close","command":"close"}),
        ] {writeln!(input,"{request}").unwrap();}
    }
    let completed=child.wait_with_output().unwrap();
    assert!(completed.status.success(),"{}",String::from_utf8_lossy(&completed.stderr));
    let replies:Vec<serde_json::Value>=std::str::from_utf8(&completed.stdout).unwrap().lines()
        .map(|line|serde_json::from_str(line).unwrap()).collect();
    assert_eq!(replies.len(),4);
    assert_eq!(replies[0]["status"],"paused");assert_eq!(replies[0]["samples"],0);
    for reply in &replies[1..] {assert_eq!(reply["status"],"completed");assert_eq!(reply["samples"],4);}
    let frozen=std::fs::read(&session_output).unwrap();
    let session_result:serde_json::Value=serde_json::from_slice(&frozen).unwrap();
    assert_eq!(session_result["status"],"valid");
    assert_eq!(read_forecast(&session_forecast).1.len(),4);
    let rejected=std::process::Command::new(env!("CARGO_BIN_EXE_mpc_campaign"))
        .arg("session").arg("--plant").arg(&plant)
        .arg("--controller").arg(&controller).arg("--scenario").arg(&schedule_path)
        .arg("--output").arg(&session_output).output().unwrap();
    assert!(!rejected.status.success());
    assert_eq!(std::fs::read(&session_output).unwrap(),frozen);
}

#[test]
fn stateful_campaign_is_exactly_chunk_invariant_and_rejections_do_not_scan() {
    use authrity_apc_engine::campaign::CampaignRunner;
    let plant=spec();
    let mut model=test_helpers::create_simple_1x1_model();
    model.variables.mvs[0].max_move=2.0;
    let mut schedule=scenario(300);
    schedule.targets[100..200].fill(vec![49.0]);
    schedule.measurement_noise=(0..300).map(|i|vec![(i%7) as f64*0.001]).collect();
    schedule.process_noise=(0..300).map(|i|vec![(i%3) as f64*0.002]).collect();
    schedule.gain_scale=(0..300).map(|i|if i<150 {1.0} else {1.1}).collect();
    let batch=closed_loop(&plant,&model,&schedule,None,&[],None).unwrap();
    assert_eq!(batch.status,"valid","{:?}",batch.reason);
    for chunk in [1,7,256,4096] {
        let mut runner=CampaignRunner::new(&plant,&model,&schedule,None,&[],None).unwrap();
        while !runner.is_finished() {
            let before=runner.samples();
            assert!(runner.advance(before+1,1).is_err());
            assert!(runner.advance(before,0).is_err());
            assert!(runner.advance(before,4097).is_err());
            assert_eq!(runner.samples(),before);
            runner.advance(before,chunk).unwrap();
        }
        assert!(runner.advance(runner.samples(),1).is_err());
        let result=runner.finish(None);
        assert_eq!(without_timing(&batch),without_timing(&result),"chunk {chunk}");
    }
}

fn run_session_fixture(
    dir:&ArchiveDir,name:&str,plant:&PlantSpec,
    model:&authrity_apc_engine::config::UnifiedModel,schedule:&Scenario,
    requests:&[serde_json::Value],forecast:Option<&std::path::Path>,
)->(serde_json::Value,Vec<serde_json::Value>) {
    use authrity_apc_engine::campaign::{CampaignRunner,session};
    let path=dir.path(name);
    let file=std::fs::OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
    let mut artifact=std::io::BufWriter::new(file);
    let journal_path=dir.path(&format!("{name}.trace.ndjson"));
    let journal=std::fs::OpenOptions::new().write(true).create_new(true).open(&journal_path).unwrap();
    let runner=CampaignRunner::new(plant,model,schedule,None,&[],forecast).unwrap();
    let input=requests.iter().map(|r|format!("{r}\n")).collect::<String>();
    let mut output=Vec::new();
    session::run(runner,&mut artifact,journal,std::io::Cursor::new(input),&mut output).unwrap();
    let mut journal_samples=0;
    for line in std::fs::read_to_string(journal_path).unwrap().lines() {
        let batch:serde_json::Value=serde_json::from_str(line).unwrap();
        assert_eq!(batch["first_sample"],journal_samples);
        journal_samples=batch["samples"].as_u64().unwrap();
        assert_eq!(batch["trace"]["cv"].as_array().unwrap().len(),(journal_samples-batch["first_sample"].as_u64().unwrap())as usize);
    }
    let result=serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let responses=std::str::from_utf8(&output).unwrap().lines().map(|line|serde_json::from_str(line).unwrap()).collect();
    (result,responses)
}

#[test]
fn jsonl_session_snapshots_preserve_state_and_complete_full_result_and_forecasts() {
    use serde_json::json;
    let dir=ArchiveDir::new();let plant=spec();let schedule=scenario(300);
    let mut model=test_helpers::create_simple_1x1_model();model.variables.mvs[0].max_move=2.0;
    let batch_forecast=dir.path("batch.ndjson");
    let session_forecast=dir.path("session.ndjson");
    let batch=closed_loop(&plant,&model,&schedule,None,&[],Some(&batch_forecast)).unwrap();
    let requests=vec![
        json!({"request_id":"initial","command":"snapshot"}),
        json!({"request_id":"missing","command":"advance","count":1}),
        json!({"request_id":"negative","command":"advance","expected_samples":0,"count":-1}),
        json!({"request_id":"zero","command":"advance","expected_samples":0,"count":0}),
        json!({"request_id":"large","command":"advance","expected_samples":0,"count":4097}),
        json!({"request_id":"first","command":"advance","expected_samples":0,"count":3}),
        json!({"request_id":"paused","command":"snapshot"}),
        json!({"request_id":"stale","command":"advance","expected_samples":0,"count":3}),
        json!({"request_id":"chunk","command":"advance","expected_samples":3,"count":296}),
        json!({"request_id":"complete","command":"advance","expected_samples":299,"count":4096}),
        json!({"request_id":"terminal","command":"snapshot"}),
        json!({"request_id":"close","command":"close"}),
    ];
    let (mut result,replies)=run_session_fixture(&dir,"result.json",&plant,&model,&schedule,&requests,Some(&session_forecast));
    assert_eq!(replies.len(),requests.len());
    for (request,reply) in requests.iter().zip(&replies) {assert_eq!(request["request_id"],reply["request_id"]);}
    for reply in &replies[..5] {
        assert_eq!(reply["status"],"paused");assert_eq!(reply["samples"],0);
        assert!(reply["frame"].is_null());assert_eq!(reply["recent_trace"]["time_seconds"],json!([]));
        assert!(reply["latest_forecast"].is_null());
    }
    for reply in &replies[1..5] {assert!(reply["error"].is_string());}
    assert_eq!(replies[5]["samples"],3);
    assert_eq!(replies[5]["frame"],replies[6]["frame"]);
    assert_eq!(replies[6]["recent_trace"],replies[7]["recent_trace"]);
    assert_eq!(replies[5]["latest_forecast"],replies[6]["latest_forecast"]);
    assert_eq!(replies[6]["latest_forecast"],replies[7]["latest_forecast"]);
    assert_eq!(replies[7]["status"],"paused");assert!(replies[7]["error"].is_string());
    assert_eq!(replies[8]["samples"],299);
    for reply in &replies[9..] {
        assert_eq!(reply["status"],"completed");assert_eq!(reply["samples"],300);
        assert_eq!(reply["recent_trace"]["time_seconds"].as_array().unwrap().len(),256);
        assert_eq!(reply["recent_trace"]["time_seconds"][0],44.0);
        assert_eq!(reply["frame"]["time_seconds"],299.0);
        assert_eq!(reply["frame"]["predicted_cv"],result["trace"]["predicted_cv"][299]);
    }
    assert_eq!(result["status"],"valid");
    for section in ["solver","metrics"] {
        for field in ["mean_solve_ms","max_solve_ms"] {result[section].as_object_mut().unwrap().remove(field);}
    }
    result["trace"].as_object_mut().unwrap().remove("solve_ms");
    // Parse both sides identically, independently of serde_json's float parser mode.
    let expected:serde_json::Value=serde_json::from_slice(&serde_json::to_vec(&without_timing(&batch)).unwrap()).unwrap();
    assert_eq!(result,expected);
    assert_eq!(read_forecast(&batch_forecast),read_forecast(&session_forecast));
    assert_eq!(std::fs::read(&batch_forecast).unwrap(),std::fs::read(&session_forecast).unwrap());
    let (header,records)=read_forecast(&session_forecast);
    for reply in &replies[5..] {
        let sample=reply["samples"].as_u64().unwrap() as usize-1;
        let forecast=&reply["latest_forecast"];
        for (key,value) in header.as_object().unwrap() {assert_eq!(&forecast[key],value,"{key}");}
        for (key,value) in records[sample].as_object().unwrap() {assert_eq!(&forecast[key],value,"{key}");}
        assert_eq!(forecast["issued_at_seconds"],reply["frame"]["time_seconds"]);
        assert_eq!(forecast["cv"][0],reply["frame"]["predicted_cv"][0]);
    }
    assert_eq!(replies[9]["latest_forecast"],replies[10]["latest_forecast"]);
    assert_eq!(replies[10]["latest_forecast"],replies[11]["latest_forecast"]);
}

#[test]
fn session_close_eof_and_real_solver_failure_never_report_completion() {
    use serde_json::json;
    let dir=ArchiveDir::new();let plant=spec();let schedule=scenario(10);
    let mut model=test_helpers::create_simple_1x1_model();model.variables.mvs[0].max_move=2.0;
    for close in [false,true] {
        let mut requests=vec![json!({"request_id":"step","command":"advance","expected_samples":0,"count":3})];
        if close {requests.push(json!({"request_id":"close","command":"close"}));}
        let (result,replies)=run_session_fixture(&dir,if close {"closed.json"} else {"eof.json"},&plant,&model,&schedule,&requests,None);
        assert_eq!(result["status"],"invalid");assert_eq!(result["samples"],3);
        assert!(result["reason"].as_str().unwrap().contains("before completion"));
        assert_eq!(replies[0]["status"],"paused");
        assert_eq!(replies[0]["latest_forecast"]["sample"],2);
        if close {
            assert_eq!(replies[1]["status"],"closed");
            assert_eq!(replies[0]["latest_forecast"],replies[1]["latest_forecast"]);
        }
    }
    model.tuning.max_iterations=1;
    let requests=vec![
        json!({"request_id":"fail","command":"advance","expected_samples":0,"count":10}),
        json!({"request_id":"snapshot","command":"snapshot"}),
        json!({"request_id":"close","command":"close"}),
    ];
    let (result,replies)=run_session_fixture(&dir,"failed.json",&plant,&model,&schedule,&requests,None);
    assert_eq!(result["status"],"invalid");assert_eq!(result["solver"]["failures"],1);
    assert_eq!(result["samples"],0);
    for reply in replies {
        assert_eq!(reply["status"],"failed");assert_eq!(reply["samples"],0);
        assert!(reply["frame"].is_null());assert!(reply["latest_forecast"].is_null());
    }
}

#[test]
fn session_discards_previous_horizon_when_a_later_scan_fails() {
    use authrity_apc_engine::campaign_plant::Fault;
    use serde_json::json;
    let dir=ArchiveDir::new();let plant=spec();let mut schedule=scenario(5);
    let mut model=test_helpers::create_simple_1x1_model();model.variables.mvs[0].max_move=2.0;
    schedule.faults.push(Fault {sample:3,kind:"disconnect".into(),items:vec![]});
    let requests=[
        json!({"request_id":"valid","command":"advance","expected_samples":0,"count":3}),
        json!({"request_id":"failed","command":"advance","expected_samples":3,"count":1}),
        json!({"request_id":"terminal","command":"snapshot"}),
    ];
    let (_,replies)=run_session_fixture(&dir,"failed-prefix.json",&plant,&model,&schedule,&requests,None);
    assert_eq!(replies[0]["latest_forecast"]["sample"],2);
    for reply in &replies[1..] {
        assert_eq!(reply["status"],"failed");assert_eq!(reply["samples"],3);
        assert!(reply["latest_forecast"].is_null());
    }
}

#[test]
fn session_broken_response_pipe_persists_invalid_partial_result() {
    use authrity_apc_engine::campaign::{CampaignRunner,session};
    struct BrokenOutput;
    impl std::io::Write for BrokenOutput {
        fn write(&mut self,_:&[u8])->std::io::Result<usize> {Err(std::io::ErrorKind::BrokenPipe.into())}
        fn flush(&mut self)->std::io::Result<()> {Ok(())}
    }
    let dir=ArchiveDir::new();let path=dir.path("broken.json");
    let plant=spec();let model=test_helpers::create_simple_1x1_model();let schedule=scenario(10);
    let runner=CampaignRunner::new(&plant,&model,&schedule,None,&[],None).unwrap();
    let mut artifact=std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    let journal=std::fs::File::create(dir.path("broken.trace.ndjson")).unwrap();
    let input=std::io::Cursor::new(b"{\"request_id\":\"snapshot\",\"command\":\"snapshot\"}\n");
    assert!(session::run(runner,&mut artifact,journal,input,BrokenOutput).is_err());
    let result:serde_json::Value=serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(result["status"],"invalid");assert_eq!(result["samples"],0);
    assert!(result["reason"].as_str().unwrap().contains("IO interrupted"));
}

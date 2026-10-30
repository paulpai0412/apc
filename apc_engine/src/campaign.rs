//! Full-resolution campaign replay and feedback evaluation; failed scans never count as holds.
use anyhow::{ensure,Context,Result};
use clarabel::solver::SolverStatus;
use serde::Serialize;
use serde_json::{json,Value};
use std::{path::Path,time::Instant};
use crate::{campaign_plant::{GenerativePlant,PlantSpec,Scenario},campaign_transport::Transport,config::UnifiedModel,dmc::DmcController,twin::{responses,same_sample_time}};
#[path="campaign_forecast.rs"]
mod forecast;
#[path="campaign_session.rs"]
pub mod session;

#[derive(Serialize,Default)]
pub struct Trace {
    pub time_seconds:Vec<f64>,pub cv:Vec<Vec<f64>>,pub mv:Vec<Vec<f64>>,pub dv:Vec<Vec<f64>>,pub targets:Vec<Vec<f64>>,pub command_mv:Vec<Vec<f64>>,
    pub predicted_cv:Vec<Vec<f64>>,pub quality:Vec<Vec<u16>>,pub solve_ms:Vec<f64>,pub solver_status:Vec<String>,
}
impl Trace {fn with_capacity(n:usize)->Self{Self{time_seconds:Vec::with_capacity(n),cv:Vec::with_capacity(n),mv:Vec::with_capacity(n),dv:Vec::with_capacity(n),targets:Vec::with_capacity(n),command_mv:Vec::with_capacity(n),predicted_cv:Vec::with_capacity(n),quality:Vec::with_capacity(n),solve_ms:Vec::with_capacity(n),solver_status:Vec::with_capacity(n)}}}
#[derive(Serialize,Default)]
pub struct SolverMetrics {pub calls:usize,pub failures:usize,pub mean_solve_ms:f64,pub max_solve_ms:f64}
#[derive(Serialize)]
pub struct Metrics {
    pub score:Option<f64>,pub cv_rmse:Vec<f64>,pub cv_iae:Vec<f64>,pub cv_violation_seconds:Vec<f64>,
    pub mv_total_variation:Vec<f64>,pub mv_hard_violations:Vec<usize>,pub solver_calls:usize,pub solver_failures:usize,pub mean_solve_ms:f64,pub max_solve_ms:f64,
}
#[derive(Serialize)]
pub struct CampaignResult {pub status:String,pub reason:Option<String>,pub sample_time:f64,pub samples:usize,pub requested_samples:usize,pub transport:String,pub control_strategy:crate::config::ControlStrategy,pub metrics:Metrics,pub solver:SolverMetrics,pub trace:Trace,pub fault_assertions:Vec<Value>,pub transport_stats:Value,pub provenance:Value}

pub fn replay(spec:&PlantSpec,scenario:&Scenario)->Result<Value> {
    let mut plant=GenerativePlant::new(spec.clone())?;scenario.validate(spec)?;
    ensure!(scenario.faults.is_empty(),"Replay does not support transport faults");
    let n=scenario.time_seconds.len();let mut cv=Vec::with_capacity(n);let mut mv=Vec::with_capacity(n);
    let mut observed=vec![0.0;spec.baseline_cv.len()];
    for t in 0..n {
        plant.observe(scenario.measurement_noise.get(t).map(Vec::as_slice),&mut observed);
        ensure!(observed.iter().all(|v|v.is_finite()),"Nonfinite observation at sample {t}");
        cv.push(observed.clone());mv.push(plant.actuators().to_vec());
        plant.advance(&scenario.mv[t],&scenario.dv[t],scenario.gain(t),scenario.process_noise.get(t).map(Vec::as_slice))?;
    }
    Ok(json!({"sample_time":spec.sample_time,"time_seconds":scenario.time_seconds,"cv":cv,"mv":mv,"dv":scenario.dv,"command_mv":scenario.mv,"targets":scenario.targets,
        "provenance":{"synthetic":true,"plant":"independent-second-order-cascade","mv_coordinate":"physical actuator at observation t","command_mv_coordinate":"command applied during interval [t,t+1)","initialization":"steady plant baselines; zero path states","process_noise":"additive physical output at t+1 from schedule row t","measurement_noise":"sensor-only addition at observation t"}}))
}

fn validate_controller(model:&UnifiedModel,spec:&PlantSpec)->Result<()> {
    let t=&model.tuning;
    ensure!(same_sample_time(t.sample_time,spec.sample_time),"Controller/plant sample_time mismatch");
    ensure!(model.variables.cvs.len()==spec.baseline_cv.len()&&model.variables.mvs.len()==spec.baseline_mv.len()&&model.variables.dvs.len()==spec.baseline_dv.len(),"Controller/plant dimensions mismatch");
    ensure!(t.prediction_horizon>=3&&t.control_horizon>0&&t.control_horizon<=t.prediction_horizon&&t.max_iterations>0&&t.solver_tolerance.is_finite()&&t.solver_tolerance>0.0&&t.terminal_weight_factor.is_finite()&&t.terminal_weight_factor>=0.0,"Invalid controller tuning");
    for cv in &model.variables.cvs {ensure!([cv.weight,cv.ece_factor,cv.alpha,cv.slack_weight,cv.limits.low,cv.limits.high,cv.limits.target].iter().all(|v|v.is_finite())&&cv.weight>=0.0&&cv.ece_factor>0.0&&(0.0..=1.0).contains(&cv.alpha)&&cv.slack_weight>=0.0&&cv.limits.low<cv.limits.high,"Invalid CV tuning/limits");}
    for (j,mv) in model.variables.mvs.iter().enumerate(){ensure!([mv.weight_r,mv.max_move,mv.target_weight,mv.limits.low,mv.limits.high].iter().all(|v|v.is_finite())&&mv.weight_r>0.0&&mv.max_move>0.0&&mv.target_weight>=0.0&&mv.target.map_or(true,|v|v.is_finite())&&mv.limits.low<mv.limits.high&&(mv.limits.low..=mv.limits.high).contains(&spec.baseline_mv[j]),"Invalid MV tuning/limits or baseline");}
    Ok(())
}

/// The single owner of plant, solver and transport state. A snapshot never enters
/// this runner; both batch and interactive execution call the same scan method.
pub struct CampaignRunner<'a> {
    spec: &'a PlantSpec, model: &'a UnifiedModel, scenario: &'a Scenario,
    plant: GenerativePlant, dmc: DmcController, transport: Transport,
    archive: Option<forecast::ForecastArchive>, trace: Trace, solver: SolverMetrics,
    metrics: Metrics, solve_total: f64,
    latest_forecast: Option<forecast::LatestForecast>,
    cv_min: Vec<f64>, cv_max: Vec<f64>, cv_weights: Vec<f64>, cv_alphas: Vec<f64>,
    default_targets: Vec<f64>, mv_min: Vec<f64>, mv_max: Vec<f64>,
    mv_weights: Vec<f64>, mv_targets: Vec<f64>, command: Vec<f64>,
    proposed: Vec<f64>, observed: Vec<f64>, values: Vec<f64>, readback: Vec<f64>,
    reason: Option<String>, assertions: Vec<Value>,
    active_fault: Option<&'a crate::campaign_plant::Fault>, uses_bridge: bool,
}

impl<'a> CampaignRunner<'a> {
    pub fn new(spec:&'a PlantSpec,model:&'a UnifiedModel,scenario:&'a Scenario,bridge:Option<&str>,bridge_args:&[String],forecast_output:Option<&Path>)->Result<Self> {
        let plant=GenerativePlant::new(spec.clone())?;scenario.validate(spec)?;validate_controller(model,spec)?;
        let (coefficients,dv_coefficients)=responses(model,false)?;
        let dmc=DmcController::new_from_coefficients(coefficients,dv_coefficients,model)?;
        let nc=spec.baseline_cv.len();let nm=spec.baseline_mv.len();let nd=spec.baseline_dv.len();let n=scenario.time_seconds.len();
        let cv_min=model.variables.cvs.iter().map(|v|v.limits.low).collect();let cv_max=model.variables.cvs.iter().map(|v|v.limits.high).collect();
        let cv_weights=model.variables.cvs.iter().map(|v|v.weight).collect();let cv_alphas=model.variables.cvs.iter().map(|v|v.alpha).collect();
        let default_targets:Vec<_>=model.variables.cvs.iter().map(|v|v.limits.target).collect();
        let mv_min=model.variables.mvs.iter().map(|v|v.limits.low).collect();let mv_max=model.variables.mvs.iter().map(|v|v.limits.high).collect();
        let mv_weights=model.variables.mvs.iter().map(|v|v.weight_r).collect();let mv_targets=model.variables.mvs.iter().enumerate().map(|(j,v)|v.target.unwrap_or(spec.baseline_mv[j])).collect();
        let archive=forecast_output.map(|path|forecast::ForecastArchive::create(path,model,n)).transpose()?;
        let trace=Trace::with_capacity(n);let solver=SolverMetrics::default();
        let metrics=Metrics{score:None,cv_rmse:vec![0.0;nc],cv_iae:vec![0.0;nc],cv_violation_seconds:vec![0.0;nc],mv_total_variation:vec![0.0;nm],mv_hard_violations:vec![0;nm],solver_calls:0,solver_failures:0,mean_solve_ms:0.0,max_solve_ms:0.0};
        let command=spec.baseline_mv.clone();let proposed=command.clone();let observed=vec![0.0;nc];
        let values=vec![0.0;2*nc+nm+nd];let readback=values.clone();
        let first_targets=scenario.targets.first().unwrap_or(&default_targets);
        let transport=Transport::new(bridge,bridge_args,&spec.baseline_cv,&spec.baseline_mv,&spec.baseline_dv,first_targets,(scenario.time_seconds[0]*1000.0).round() as i64)?;
        Ok(Self{spec,model,scenario,plant,dmc,transport,archive,trace,solver,metrics,solve_total:0.0,latest_forecast:None,
            cv_min,cv_max,cv_weights,cv_alphas,default_targets,mv_min,mv_max,mv_weights,mv_targets,
            command,proposed,observed,values,readback,reason:None,assertions:Vec::new(),active_fault:None,uses_bridge:bridge.is_some()})
    }

    pub fn samples(&self)->usize {self.trace.time_seconds.len()}
    pub fn is_finished(&self)->bool {self.reason.is_some()||self.samples()==self.scenario.time_seconds.len()}

    /// Validate the complete request before touching any numerical/IO state.
    pub fn advance(&mut self,expected_samples:usize,count:usize)->Result<()> {
        ensure!(expected_samples==self.samples(),"Expected samples {expected_samples}, actual {}",self.samples());
        ensure!((1..=4096).contains(&count),"Advance count must be between 1 and 4096");
        ensure!(!self.is_finished(),"Campaign is already terminal");
        for _ in 0..count {if self.is_finished(){break;}self.scan();}
        Ok(())
    }

    fn scan(&mut self) {
        let t=self.samples();
        let Self{spec,model,scenario,plant,dmc,transport,archive,latest_forecast,trace,solver,metrics,solve_total,
            cv_min,cv_max,cv_weights,cv_alphas,default_targets,mv_min,mv_max,mv_weights,mv_targets,
            command,proposed,observed,values,readback,reason,assertions,active_fault,..}=self;
        let (spec,model,scenario)=(*spec,*model,*scenario);
        let nc=spec.baseline_cv.len();let nm=spec.baseline_mv.len();let nd=spec.baseline_dv.len();
        if let Some(latest)=latest_forecast.as_mut() {latest.invalidate();}
        let mut transport_failure=false;
        let scan:Result<()>= (|| {
            let stamp=(scenario.time_seconds[t]*1000.0).round() as i64;
            for fault in scenario.faults.iter().filter(|f|f.sample==t) {transport.fault(fault)?;*active_fault=Some(fault);}
            plant.observe(scenario.measurement_noise.get(t).map(Vec::as_slice),observed);
            let targets=scenario.targets.get(t).unwrap_or(default_targets);
            values[..nc].copy_from_slice(observed);values[nc..nc+nm].copy_from_slice(plant.actuators());values[nc+nm..nc+nm+nd].copy_from_slice(&scenario.dv[t]);values[nc+nm+nd..].copy_from_slice(targets);
            transport.inputs(values,stamp,readback).map_err(|error|{transport_failure=true;error})?;
            let pvs=&readback[..nc];let actual=&readback[nc..nc+nm];let dvs=&readback[nc+nm..nc+nm+nd];let targets=&readback[nc+nm+nd..];
            let start=Instant::now();
            let result=dmc.next_move(pvs,targets,command,dvs,mv_targets,mv_min,mv_max,cv_min,cv_max,cv_weights,cv_alphas,mv_weights);
            let elapsed=start.elapsed().as_secs_f64()*1000.0;solver.calls+=1;*solve_total+=elapsed;solver.max_solve_ms=solver.max_solve_ms.max(elapsed);
            if !matches!(result.status,SolverStatus::Solved|SolverStatus::AlmostSolved){solver.failures+=1;anyhow::bail!("Solver {:?} at sample {t}",result.status);}
            ensure!(result.next_move.len()==nm&&result.next_move.iter().chain(&result.predicted_pvs).all(|v|v.is_finite())&&result.objective_value.is_finite(),"Nonfinite solver result at sample {t}");
            for j in 0..nm {
                proposed[j]=command[j]+result.next_move[j];
                let eps=1e-7*(mv_max[j]-mv_min[j]).max(1.0);
                let violation=proposed[j]<mv_min[j]-eps||proposed[j]>mv_max[j]+eps||result.next_move[j].abs()>model.variables.mvs[j].max_move+eps;
                if violation {metrics.mv_hard_violations[j]+=1;}
                ensure!(!violation,"Hard MV command constraint violation at sample {t}, MV {j}");
                let previous_actual=trace.mv.last().map_or(spec.baseline_mv[j],|r|r[j]);
                let actuator_violation=actual[j]<spec.actuator_low[j]-eps||actual[j]>spec.actuator_high[j]+eps
                    ||(actual[j]-previous_actual).abs()>spec.actuator_rate[j]*spec.sample_time+eps;
                if actuator_violation {metrics.mv_hard_violations[j]+=1;}
                ensure!(!actuator_violation,"Physical actuator bound/rate violation at sample {t}, MV {j}");
                // Clip only already-validated floating-point feasibility residue.
                proposed[j]=proposed[j].clamp((command[j]-model.variables.mvs[j].max_move).max(mv_min[j]),(command[j]+model.variables.mvs[j].max_move).min(mv_max[j]));
            }
            transport.apply(proposed,stamp,command).map_err(|error|{transport_failure=true;error})?;
            // A scan counts only after all IO, solver and safety checks succeeded.
            for i in 0..nc {let error=pvs[i]-targets[i];metrics.cv_rmse[i]=metrics.cv_rmse[i].hypot(error);metrics.cv_iae[i]+=error.abs()*spec.sample_time;if pvs[i]<cv_min[i]||pvs[i]>cv_max[i]{metrics.cv_violation_seconds[i]+=spec.sample_time;}}
            for j in 0..nm {let previous=trace.command_mv.last().map_or(spec.baseline_mv[j],|r|r[j]);metrics.mv_total_variation[j]+=(command[j]-previous).abs();}
            trace.time_seconds.push(scenario.time_seconds[t]);trace.cv.push(pvs.to_vec());trace.mv.push(actual.to_vec());trace.dv.push(dvs.to_vec());trace.targets.push(targets.to_vec());trace.command_mv.push(command.clone());trace.predicted_cv.push(result.predicted_pvs[..nc].to_vec());trace.quality.push(transport.quality[..nc+nm+nd].to_vec());trace.solve_ms.push(elapsed);trace.solver_status.push(format!("{:?}",result.status));
            plant.advance(command,dvs,scenario.gain(t),scenario.process_noise.get(t).map(Vec::as_slice))?;
            // The newly appended command is this scan's output; the preceding one is
            // the exact base passed to next_move, not physical actuator feedback.
            let previous=trace.command_mv.len().checked_sub(2).map_or(spec.baseline_mv.as_slice(),|i|trace.command_mv[i].as_slice());
            if let Some(archive)=archive.as_mut() {
                archive.record(t,scenario.time_seconds[t],&result.predicted_pvs,previous,&result.future_plan)?;
            }
            if let Some(latest)=latest_forecast.as_mut() {
                latest.capture(t,scenario.time_seconds[t],result.predicted_pvs,previous,result.future_plan)?;
            }
            Ok(())
        })();
        if let Err(error)=scan {
            let message=format!("sample {t}: {error:#}");
            if let Some(f)=active_fault {assertions.push(json!({"sample":t,"kind":f.kind,"detected":transport_failure,"reason":message,"writes_after_detection":0,"command_blocked":true}));}
            *reason=Some(message);
        }
    }

    /// Consume the runner once; no partial run can produce a valid archive.
    pub fn finish(self,interruption:Option<String>)->CampaignResult {
    let Self{spec,model,scenario,mut transport,archive,trace,mut solver,mut metrics,solve_total,
        cv_min,cv_max,mv_min,mv_max,mut reason,assertions,uses_bridge,..}=self;
    let n=scenario.time_seconds.len();let nc=spec.baseline_cv.len();let nm=spec.baseline_mv.len();
    if reason.is_none()&&trace.time_seconds.len()!=n {
        reason=Some(interruption.unwrap_or_else(||"Campaign interrupted before all samples completed".into()));
    }
    if let Err(error)=transport.close(){
        let message=format!("DA close failed: {error:#}");
        reason=Some(match reason {Some(previous)=>format!("{previous}; {message}"),None=>message});
    }
    if reason.is_none()&&!scenario.faults.is_empty(){reason=Some("Scheduled transport fault was not detected".into());}
    if let Some(archive)=archive {
        if let Err(error)=archive.finish() {
            let message=format!("Forecast archive incomplete: {error:#}");
            reason=Some(match reason {Some(previous)=>format!("{previous}; {message}"),None=>message});
        }
    }
    let count=trace.time_seconds.len();
    solver.mean_solve_ms=if solver.calls>0{solve_total/solver.calls as f64}else{0.0};
    metrics.solver_calls=solver.calls;metrics.solver_failures=solver.failures;metrics.mean_solve_ms=solver.mean_solve_ms;metrics.max_solve_ms=solver.max_solve_ms;
    if count>0 {
        for v in &mut metrics.cv_rmse{*v/=(count as f64).sqrt();}
        let duration=count as f64*spec.sample_time;
        let tracking=metrics.cv_iae.iter().enumerate().map(|(i,v)|v/(cv_max[i]-cv_min[i])/duration).sum::<f64>()/nc as f64;
        let violations=metrics.cv_violation_seconds.iter().sum::<f64>()/duration/nc as f64;
        let movement=metrics.mv_total_variation.iter().enumerate().map(|(j,v)|v/(mv_max[j]-mv_min[j])/count as f64).sum::<f64>()/nm as f64;
        let score=tracking+violations+0.01*movement;
        if score.is_finite(){metrics.score=Some(score);}else{reason=Some("Nonfinite campaign metrics".into());}
    }
    CampaignResult{status:if reason.is_some(){"invalid"}else{"valid"}.into(),reason,sample_time:spec.sample_time,samples:count,requested_samples:n,transport:if uses_bridge{"opc-da"}else{"direct"}.into(),control_strategy:model.control_strategy.clone(),metrics,solver,trace,fault_assertions:assertions,transport_stats:transport.stats,
        provenance:json!({"synthetic":true,"plant":"independent-second-order-cascade","controller":"DmcController/Clarabel","controller_input_coordinate":"successfully read-back applied command; actuator dynamics belong in learned FIR","trace_mv_coordinate":"physical actuator at observation t","trace_command_mv_coordinate":"command applied over [t,t+1)","score":"mean normalized IAE + mean CV violation fraction + 0.01 mean normalized command variation per scan","warmup_excluded_samples":0})}
    }
}

pub fn closed_loop(spec:&PlantSpec,model:&UnifiedModel,scenario:&Scenario,bridge:Option<&str>,bridge_args:&[String],forecast_output:Option<&Path>)->Result<CampaignResult> {
    let mut runner=CampaignRunner::new(spec,model,scenario,bridge,bridge_args,forecast_output)?;
    while !runner.is_finished(){runner.scan();}
    Ok(runner.finish(None))
}

pub fn load_json<T:serde::de::DeserializeOwned>(path:&str)->Result<T>{serde_json::from_reader(std::io::BufReader::new(std::fs::File::open(path).with_context(||format!("Opening {path}"))?)).with_context(||format!("Parsing {path}"))}

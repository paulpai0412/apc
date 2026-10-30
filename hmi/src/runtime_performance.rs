//! Full-resolution engineering evidence. No chart decimation and no inferred energy.
use std::collections::{BTreeMap,BTreeSet};
use axum::{extract::{Query, State}, Json};
use chrono::{DateTime,Utc};
use serde::Deserialize;
use serde_json::{json,Value};
use crate::{auth::CurrentUser,state::AppState};
use super::{invalid,DeploymentProfile,Result};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PerformanceQuery {
    profile_hash:Option<String>, baseline_start:Option<DateTime<Utc>>,baseline_end:Option<DateTime<Utc>>,
    candidate_start:Option<DateTime<Utc>>,candidate_end:Option<DateTime<Utc>>,
}
#[derive(Clone,Debug,serde::Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceContract {
    /// Exact historian tags for product, load regime, raw-material and control-mode classification.
    pub regime_tags:Vec<String>, pub sample_interval_ms:u64, pub minimum_samples:usize,
    /// Original operator-owned setting tag -> independent acquisition snapshot tag.
    pub sampled_tags:BTreeMap<String,String>,
}
#[derive(Default)]
struct Moments { n:usize, mean:f64,m2:f64,squared_error:f64,error:f64,violations:usize,movement:f64,saturation:usize,previous:Option<f64> }
impl Moments {
    fn add(&mut self,value:f64,target:f64,low:f64,high:f64) {
        self.n+=1;let d=value-self.mean;self.mean+=d/self.n as f64;self.m2+=d*(value-self.mean);
        self.error+=value-target;self.squared_error+=(value-target).powi(2);
        self.violations+=usize::from(value<low||value>high);
        self.saturation+=usize::from(value<=low||value>=high);
        if let Some(previous)=self.previous {self.movement+=(value-previous).abs();}self.previous=Some(value);
    }
    fn value(&self,role:&str,unit:&str)->Value {
        if self.n<2{return json!({"valid_samples":self.n,"unit":unit,"variance":null,"bias":null,"rmse":null});}
        json!({"valid_samples":self.n,"unit":unit,"mean":self.mean,"variance":self.m2/(self.n-1)as f64,
            "bias":if role=="CV"{Some(self.error/self.n as f64)}else{None},
            "rmse":if role=="CV"{Some((self.squared_error/self.n as f64).sqrt())}else{None},
            "violation_fraction":self.violations as f64/self.n as f64,
            "total_variation":if role=="MV"{Some(self.movement)}else{None},
            "saturation_fraction":if role=="MV"{Some(self.saturation as f64/self.n as f64)}else{None}})
    }
}
async fn window(state:&AppState,p:&DeploymentProfile,contract:&PerformanceContract,start:DateTime<Utc>,end:DateTime<Utc>)->Result<Value> {
    if start>=end || end-start>chrono::Duration::days(31) || end>Utc::now(){return Err(invalid("Performance windows must be past, ordered and at most 31 days"));}
    let mut nodes=contract.regime_tags.clone();
    for v in &p.variables {
        if v.role=="DV"{continue;}
        nodes.push(if v.role=="MV"{v.applied_tag.clone().ok_or_else(||invalid("Independent MV feedback required"))?}else{v.pv_tag.clone()});
        if v.role=="CV" {
            let target=v.target_tag.as_ref().and_then(|tag|contract.sampled_tags.get(tag))
                .ok_or_else(||invalid("Missing independently sampled CV target mapping"))?;
            nodes.push(target.clone());
        }
        for tag in [&v.low_tag,&v.high_tag].into_iter().flatten(){
            nodes.push(contract.sampled_tags.get(tag).ok_or_else(||invalid("Missing independently sampled operating-limit mapping"))?.clone());
        }
    }
    nodes.sort();nodes.dedup();
    let history=crate::process_trends::query_scoped(&state.historian_config,start,end,&nodes,&[],None,&p.endpoint).await.map_err(|e|invalid(e.to_string()))?;
    let mut rows:BTreeMap<DateTime<Utc>,BTreeMap<&str,f64>>=BTreeMap::new();let mut bad=0usize;let mut invalid_times=BTreeSet::new();
    for point in &history.points {
        let time=DateTime::parse_from_rfc3339(&point.timestamp).map_err(|_|invalid("Invalid historian clock"))?.with_timezone(&Utc);
        if !point.evidence_valid||point.status.is_none_or(|s|s&0xc0000000!=0)||point.value.is_none(){bad+=1;invalid_times.insert(time);continue;}
        if rows.entry(time).or_default().insert(point.node_id,point.value.unwrap()).is_some(){return Err(invalid("Duplicate historian identity/time; cannot calculate evidence"));}
    }
    let mut metrics:BTreeMap<&str,Moments>=p.variables.iter().filter(|v|v.role!="DV").map(|v|(v.id.as_str(),Moments::default())).collect();
    let mut context:Option<BTreeMap<String,f64>>=None;let mut incompatible=false;let mut valid=0usize;let mut last=None;let mut gaps=0usize;
    for (time,row) in rows {
        if invalid_times.contains(&time){continue;}
        let mut current=BTreeMap::new();let mut values=Vec::new();
        let mut complete=true;
        for tag in &contract.regime_tags {if let Some(value)=row.get(tag.as_str()){current.insert(tag.clone(),*value);}else{complete=false;}}
        for v in p.variables.iter().filter(|v|v.role!="DV") {
            let tag=if v.role=="MV"{v.applied_tag.as_deref().unwrap()}else{&v.pv_tag};
            let value=row.get(tag).copied();
            let sampled=|tag:Option<&str>|tag.and_then(|tag|contract.sampled_tags.get(tag)).and_then(|tag|row.get(tag.as_str())).copied();
            let low=sampled(v.low_tag.as_deref());
            let high=sampled(v.high_tag.as_deref());
            let target=if v.role=="CV"{sampled(v.target_tag.as_deref())}else{value};
            match(value,target,low,high) {
                (Some(value),Some(target),Some(low),Some(high)) if low<high&&low>=v.hard_low&&high<=v.hard_high=>{
                    if v.role=="CV"{current.insert(format!("target:{}",v.id),target);}
                    current.insert(format!("low:{}",v.id),low);current.insert(format!("high:{}",v.id),high);
                    values.push((v.id.as_str(),value,target,low,high));
                },_=>complete=false,
            }
        }
        if !complete {continue;}
        if let Some(previous)=&context {if previous!=&current{incompatible=true;}}else{context=Some(current);}
        if let Some(previous)=last {if time.signed_duration_since(previous).num_milliseconds()>contract.sample_interval_ms as i64*2{gaps+=1;for metric in metrics.values_mut(){metric.previous=None;}}}
        last=Some(time);valid+=1;
        for (id,value,target,low,high) in values {metrics.get_mut(id).unwrap().add(value,target,low,high);}
    }
    let expected=(end-start).num_milliseconds().max(0)as f64/contract.sample_interval_ms as f64;
    let coverage=if expected>0.0{Some((valid as f64/expected).min(1.0))}else{None};
    let metrics:Vec<_>=p.variables.iter().filter(|v|v.role!="DV").map(|v|json!({"variable_id":v.id,"role":v.role,"evidence":metrics[ v.id.as_str()].value(&v.role,&v.unit)})).collect();
    Ok(json!({"start":start,"end":end,"valid_samples":valid,"raw_samples":history.raw_samples,"coverage":coverage,
        "bad_samples":bad,"gaps":gaps,"context":context,"context_changed":incompatible,"metrics":metrics,
        "qualified":valid>=contract.minimum_samples&&!incompatible&&gaps==0&&bad==0&&coverage.is_some_and(|c|c>=0.95),
        "method":"complete exact-timestamp engineering samples; sample variance; no interpolation","warnings":history.warnings}))
}
pub(super) async fn performance(State(state):State<AppState>,user:CurrentUser,Query(query):Query<PerformanceQuery>)->Result<Json<Value>> {
    if state.settings.research.enabled{return Err(invalid("Isolated research cannot query field runtime performance sources"));}
    let (Some(hash),Some(bs),Some(be),Some(cs),Some(ce))=(query.profile_hash,query.baseline_start,query.baseline_end,query.candidate_start,query.candidate_end)else{
        return Ok(Json(json!({"status":"selection_required","reason":"Select a commissioned profile and two precise nonoverlapping evidence windows","metrics":[],"improvement":null})));
    };
    let p=state.runtime.profile(&hash,&user,"runtime.view")?;
    let contract=p.performance.as_ref().ok_or_else(||invalid("Profile lacks commissioned regime tags and performance sampling contract"))?;
    if !(be<=cs||ce<=bs){return Err(invalid("Baseline and candidate windows must not overlap"));}
    if !state.historian_config.enabled{return Err(invalid("Historian disabled; evidence is unavailable, not zero"));}
    let baseline=window(&state,p,contract,bs,be).await?;let candidate=window(&state,p,contract,cs,ce).await?;
    let comparable=baseline["qualified"]==true&&candidate["qualified"]==true&&baseline["context"]==candidate["context"];
    Ok(Json(json!({"status":if comparable{"comparable"}else{"incompatible_or_insufficient"},"profile_hash":hash,
        "baseline":baseline,"candidate":candidate,"comparable":comparable,
        "energy_intensity":null,"causal_improvement":null,
        "limitations":["No measured power/production contract: MV motion is not energy","Observational windows do not establish causal improvement","Exact-time pairing never forward-fills missing process values"]})))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn engineering_metrics_preserve_units_and_motion_is_not_energy(){
        let mut metric=Moments::default();for value in [1.0,2.0,3.0]{metric.add(value,2.0,1.0,3.0);}
        let cv=metric.value("CV","t/h");assert_eq!(cv["variance"],1.0);assert_eq!(cv["bias"],0.0);
        let mv=metric.value("MV","rpm");assert_eq!(mv["total_variation"],2.0);assert!(mv.get("energy").is_none());
    }
}

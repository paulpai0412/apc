//! Independent nonlinear second-order plant, not a controller FIR replay.
//! Row t observes physical state before command u[t]. Actuator update is
//! a' = clamp(a + clamp((u-a)*(1-exp(-dt/tau)), +/-rate*dt), low, high).
//! Zero tau means instantaneous response. Delayed actuator/DV deviations use
//! floor(delay/dt) and linear interpolation with the immediately older sample.
//! Each path then updates x1'=e1*x1+(1-e1)*input and x2'=e2*x2+(1-e2)*x1',
//! where ei=exp(-dt/taui). z=sum(gain*gain_scale[t]*x2') and physical output is
//! baseline+z+strength*scale*tanh(z/scale)+process_noise[t] at time t+1.
//! Measurement noise[t] is added only when observing row t, never to plant state.
//! Prehistory deviations and path states are zero; actuator/CV start at baselines.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use crate::twin::same_sample_time;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PathSpec { pub gain:f64, pub tau1:f64, pub tau2:f64, pub delay_seconds:f64 }
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PlantSpec {
    pub sample_time:f64, pub baseline_cv:Vec<f64>, pub baseline_mv:Vec<f64>, pub baseline_dv:Vec<f64>,
    pub actuator_tau:Vec<f64>, pub actuator_low:Vec<f64>, pub actuator_high:Vec<f64>, pub actuator_rate:Vec<f64>,
    pub paths:Vec<Vec<PathSpec>>, pub nonlinear_strength:Vec<f64>, pub nonlinear_scale:Vec<f64>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Fault {pub sample:usize,pub kind:String,#[serde(default)]pub items:Vec<String>}
#[derive(Debug, Deserialize)]
pub struct Scenario {
    pub sample_time:f64,pub time_seconds:Vec<f64>,pub cv:Vec<Vec<f64>>,pub mv:Vec<Vec<f64>>,pub dv:Vec<Vec<f64>>,
    #[serde(default)]pub targets:Vec<Vec<f64>>, #[serde(default)]pub measurement_noise:Vec<Vec<f64>>,
    #[serde(default)]pub process_noise:Vec<Vec<f64>>,#[serde(default)]pub gain_scale:Vec<f64>,
    #[serde(default)]pub faults:Vec<Fault>,
}
fn rows(rows:&[Vec<f64>],n:usize,width:usize,name:&str,optional:bool)->Result<()> {
    ensure!((optional&&rows.is_empty()) || (rows.len()==n&&rows.iter().all(|r|r.len()==width&&r.iter().all(|x|x.is_finite()))),"{name}: invalid rows, dimensions or nonfinite values"); Ok(())
}
impl Scenario {
    pub fn validate(&self,p:&PlantSpec)->Result<()> {
        ensure!(same_sample_time(self.sample_time,p.sample_time),"Scenario/plant sample time mismatch");
        let n=self.time_seconds.len();ensure!(n>0,"Empty scenario");
        ensure!(self.time_seconds.iter().all(|v|v.is_finite())&&self.time_seconds.windows(2).all(|v|same_sample_time(v[1]-v[0],p.sample_time)),"Invalid scenario timeline");
        rows(&self.cv,n,p.baseline_cv.len(),"cv",false)?; rows(&self.mv,n,p.baseline_mv.len(),"mv",false)?; rows(&self.dv,n,p.baseline_dv.len(),"dv",false)?;
        rows(&self.targets,n,p.baseline_cv.len(),"targets",true)?;rows(&self.measurement_noise,n,p.baseline_cv.len(),"measurement_noise",true)?;rows(&self.process_noise,n,p.baseline_cv.len(),"process_noise",true)?;
        ensure!(self.gain_scale.is_empty()||(self.gain_scale.len()==n&&self.gain_scale.iter().all(|v|v.is_finite()&&*v>0.0)),"Invalid gain_scale");
        ensure!(self.faults.iter().all(|f| f.sample<n&&matches!(f.kind.as_str(),"bad_quality"|"disconnect"|"write_denied")),"Invalid fault schedule"); Ok(())
    }
    pub fn gain(&self,t:usize)->f64 {self.gain_scale.get(t).copied().unwrap_or(1.0)}
}
struct PathState {gain:f64,e1:f64,e2:f64,delay:usize,fraction:f64,history:Vec<f64>,head:usize,x1:f64,x2:f64}
pub struct GenerativePlant {spec:PlantSpec,paths:Vec<Vec<PathState>>,actuator_fraction:Vec<f64>,mv:Vec<f64>,cv:Vec<f64>}
impl GenerativePlant {
    pub fn new(spec:PlantSpec)->Result<Self> {
        let nc=spec.baseline_cv.len();let nm=spec.baseline_mv.len();let nd=spec.baseline_dv.len();
        ensure!(nc>0&&nm>0&&spec.sample_time.is_finite()&&spec.sample_time>0.0,"Invalid plant dimensions/sample time");
        for (name,v,n) in [("baseline_cv",&spec.baseline_cv,nc),("baseline_mv",&spec.baseline_mv,nm),("baseline_dv",&spec.baseline_dv,nd),("actuator_tau",&spec.actuator_tau,nm),("actuator_low",&spec.actuator_low,nm),("actuator_high",&spec.actuator_high,nm),("actuator_rate",&spec.actuator_rate,nm),("nonlinear_strength",&spec.nonlinear_strength,nc),("nonlinear_scale",&spec.nonlinear_scale,nc)] {
            ensure!(v.len()==n&&v.iter().all(|x|x.is_finite()),"Invalid plant {name}");
        }
        ensure!(spec.paths.len()==nc&&spec.paths.iter().all(|r|r.len()==nm+nd),"Plant path dimensions mismatch");
        for j in 0..nm {ensure!(spec.actuator_tau[j]>=0.0&&spec.actuator_rate[j]>0.0&&spec.actuator_low[j]<spec.actuator_high[j]&&(spec.actuator_low[j]..=spec.actuator_high[j]).contains(&spec.baseline_mv[j]),"Invalid actuator {j}");}
        ensure!(spec.nonlinear_scale.iter().all(|v|*v>0.0),"Nonlinear scales must be positive");
        let decay=|tau:f64|if tau==0.0 {0.0}else{(-spec.sample_time/tau).exp()};
        let mut paths=Vec::with_capacity(nc);
        for row in &spec.paths {let mut states=Vec::with_capacity(nm+nd);for p in row {
            ensure!([p.gain,p.tau1,p.tau2,p.delay_seconds].iter().all(|v|v.is_finite())&&p.tau1>=0.0&&p.tau2>=0.0&&p.delay_seconds>=0.0,"Invalid path dynamics");
            let d=p.delay_seconds/spec.sample_time;ensure!(d<=1_000_000.0,"Path delay exceeds one million samples");
            let delay=d.floor() as usize;
            states.push(PathState{gain:p.gain,e1:decay(p.tau1),e2:decay(p.tau2),delay,fraction:d-delay as f64,history:vec![0.0;delay+2],head:0,x1:0.0,x2:0.0});
        }paths.push(states);}
        let actuator_fraction=spec.actuator_tau.iter().map(|t|1.0-decay(*t)).collect();
        Ok(Self{mv:spec.baseline_mv.clone(),cv:spec.baseline_cv.clone(),spec,paths,actuator_fraction})
    }
    pub fn output(&self)->&[f64] {&self.cv}
    pub fn actuators(&self)->&[f64] {&self.mv}
    pub fn observe(&self,noise:Option<&[f64]>,out:&mut[f64]) {for i in 0..out.len(){out[i]=self.cv[i]+noise.map_or(0.0,|r|r[i]);}}
    pub fn advance(&mut self,command:&[f64],dv:&[f64],gain:f64,noise:Option<&[f64]>)->Result<()> {
        let s=&self.spec;let nm=self.mv.len();
        ensure!(command.len()==nm&&dv.len()==s.baseline_dv.len()&&command.iter().chain(dv).all(|v|v.is_finite())&&gain.is_finite()&&gain>0.0,"Invalid plant inputs");
        ensure!(noise.map_or(true,|r|r.len()==self.cv.len()&&r.iter().all(|v|v.is_finite())),"Invalid plant disturbance");
        for j in 0..nm {
            let move_limit=s.actuator_rate[j]*s.sample_time;
            let delta=((command[j]-self.mv[j])*self.actuator_fraction[j]).clamp(-move_limit,move_limit);
            self.mv[j]=(self.mv[j]+delta).clamp(s.actuator_low[j],s.actuator_high[j]);
        }
        for i in 0..self.cv.len() {
            let mut z=0.0;
            for (j,p) in self.paths[i].iter_mut().enumerate() {
                let u=if j<nm {self.mv[j]-s.baseline_mv[j]}else{dv[j-nm]-s.baseline_dv[j-nm]};
                p.head=(p.head+1)%p.history.len();p.history[p.head]=u;
                let index=(p.head+p.history.len()-p.delay)%p.history.len();
                let older=(index+p.history.len()-1)%p.history.len();
                let delayed=(1.0-p.fraction)*p.history[index]+p.fraction*p.history[older];
                p.x1=p.e1*p.x1+(1.0-p.e1)*delayed;
                p.x2=p.e2*p.x2+(1.0-p.e2)*p.x1;
                z+=p.gain*gain*p.x2;
            }
            self.cv[i]=s.baseline_cv[i]+z+s.nonlinear_strength[i]*s.nonlinear_scale[i]*(z/s.nonlinear_scale[i]).tanh()+noise.map_or(0.0,|r|r[i]);
        }
        ensure!(self.cv.iter().chain(&self.mv).all(|v|v.is_finite()),"Nonfinite generative plant state");Ok(())
    }
}

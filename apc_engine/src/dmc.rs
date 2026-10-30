//! Constrained FIR DMC. State contains observed input increments, never an unexecuted plan.
use crate::config::{ControlMethod, MvOptimizationMode, OptimizationMode, UnifiedModel};
use anyhow::{ensure, Result};
use clarabel::{algebra::CscMatrix, solver::{DefaultSettings, DefaultSolver, IPSolver, SolverStatus, SupportedConeT}};
use nalgebra::DMatrix;

#[derive(Debug, Clone)]
pub struct DmcResult {
    pub next_move: Vec<f64>,
    pub future_plan: Vec<f64>,
    pub predicted_pvs: Vec<f64>,
    pub status: SolverStatus,
    pub cv_bias: Vec<f64>,
    pub objective_value: f64,
}

pub struct DmcController {
    model: UnifiedModel,
    matrix_a: DMatrix<f64>,
    constraints: CscMatrix<f64>,
    cv_gram: Vec<DMatrix<f64>>,
    cached_p: CscMatrix<f64>,
    cached_cv_weights: Vec<f64>,
    cached_mv_weights: Vec<f64>,
    solver: Option<DefaultSolver<f64>>,
    free_workspace: Vec<f64>,
    gradient_workspace: Vec<f64>,
    bounds_workspace: Vec<f64>,
    history_response: Vec<f64>,
    tail_slopes: Vec<f64>,
    previous_mv: Option<Vec<f64>>,
    previous_dv: Option<Vec<f64>>,
    previous_prediction: Option<Vec<f64>>,
    // Fixed-size ring; age zero is the newly observed increment.
    increments: Vec<f64>,
    old_increments: Vec<f64>,
    history_len: usize,
    cursor: usize,
    observed_samples: usize,
}

fn coefficient(curve: &[f64], index: usize, integrating: bool) -> f64 {
    if index < curve.len() { return curve[index]; }
    let last = *curve.last().unwrap_or(&0.0);
    if integrating && curve.len() >= 2 {
        last + (index + 1 - curve.len()) as f64 * (last - curve[curve.len()-2])
    } else { last }
}

impl DmcController {
    pub fn new_from_coefficients(curves: Vec<Vec<Vec<f64>>>, mut dv_curves: Vec<Vec<Vec<f64>>>, model: &UnifiedModel) -> Result<Self> {
        model.validate_control_strategy()?;
        model.sample_interval()?;
        let nc = model.variables.cvs.len(); let nm = model.variables.mvs.len();
        let nd = model.variables.dvs.len();
        let p = model.tuning.prediction_horizon; let m = model.tuning.control_horizon;
        // Production parametric models historically omit DV feedforward.
        if nd > 0 && dv_curves.is_empty() { dv_curves=vec![vec![vec![0.0;p];nd];nc]; }
        ensure!(nc > 0 && nm > 0 && p > 0 && m > 0 && m <= p, "Invalid DMC dimensions");
        ensure!(curves.len()==nc && curves.iter().all(|row| row.len()==nm && row.iter().all(|c| !c.is_empty() && c.iter().all(|v| v.is_finite()))), "Invalid MV response dimensions");
        ensure!(nd==0 || (dv_curves.len()==nc && dv_curves.iter().all(|row| row.len()==nd && row.iter().all(|c| !c.is_empty() && c.iter().all(|v|v.is_finite())))), "Invalid DV response dimensions");
        let nu=nm*m; let ny=nc*p;
        let nq=if model.control_strategy.method==ControlMethod::SoftBand {ny}else{0};
        let mut a=DMatrix::zeros(ny,nu);
        for t in 0..p { for i in 0..nc { for j in 0..nm { for k in 0..=t.min(m-1) {
            a[(t*nc+i,k*nm+j)]=coefficient(&curves[i][j],t-k,model.variables.cvs[i].is_integrating);
        }}}}
        let mut constraints=DMatrix::zeros(4*nu+3*ny+3*nq,nu+ny+nq);
        for q in 0..nu { constraints[(q,q)]=1.0; constraints[(nu+q,q)]=-1.0; }
        for k in 0..m { for j in 0..nm { for h in 0..=k {
            constraints[(2*nu+k*nm+j,h*nm+j)]=1.0;
            constraints[(3*nu+k*nm+j,h*nm+j)]=-1.0;
        }}}
        for r in 0..ny {
            for c in 0..nu { constraints[(4*nu+r,c)]=a[(r,c)]; constraints[(4*nu+ny+r,c)]=-a[(r,c)]; }
            constraints[(4*nu+r,nu+r)]=-1.0; constraints[(4*nu+ny+r,nu+r)]=-1.0;
            constraints[(4*nu+2*ny+r,nu+r)]=-1.0;
        }
        // Quality-band slack is distinct from the existing operating-limit slack.
        for r in 0..nq {
            let row=4*nu+3*ny; let column=nu+ny+r;
            for c in 0..nu {
                constraints[(row+r,c)]=a[(r,c)];
                constraints[(row+nq+r,c)]=-a[(r,c)];
            }
            constraints[(row+r,column)]=-1.0;
            constraints[(row+nq+r,column)]=-1.0;
            constraints[(row+2*nq+r,column)]=-1.0;
        }
        // Per-CV Gram matrices make live weight changes O(nc*nu²), not O(ny*nu²).
        let mut grams=vec![DMatrix::zeros(nu,nu);nc];
        for i in 0..nc { for t in 0..p {
            let scale=if t+1==p {model.tuning.terminal_weight_factor} else {1.0};
            let scale=scale/model.variables.cvs[i].ece_factor.powi(2);
            let scale=scale*model.control_strategy.bands.get(i).map_or(1.0,|band|band.tracking_scale);
            for r in 0..nu { for c in 0..nu { grams[i][(r,c)]+=2.0*scale*a[(t*nc+i,r)]*a[(t*nc+i,c)]; }}
        }}
        let history_len=curves.iter().chain(dv_curves.iter()).flat_map(|r|r.iter()).map(Vec::len).max().unwrap_or(1);
        let ni=nm+nd;
        let mut history_response=vec![0.0;ny*ni*history_len];
        let mut tail_slopes=vec![0.0;nc*ni];
        for i in 0..nc {for j in 0..ni {
            let curve=if j<nm {&curves[i][j]}else{&dv_curves[i][j-nm]};
            let integrating=model.variables.cvs[i].is_integrating;
            if integrating&&curve.len()>=2 {tail_slopes[i*ni+j]=curve[curve.len()-1]-curve[curve.len()-2];}
            for t in 0..p {for age in 0..history_len {
                // MV readback has acted for one interval; newly measured DV acts now.
                let elapsed=age+usize::from(j<nm);
                let now=if elapsed==0 {0.0}else{coefficient(curve,elapsed-1,integrating)};
                history_response[((t*nc+i)*ni+j)*history_len+age]=coefficient(curve,elapsed+t,integrating)-now;
            }}
        }}
        Ok(Self { model:model.clone(), matrix_a:a, constraints:to_csc(&constraints), cv_gram:grams,
            cached_p:to_csc(&DMatrix::zeros(nu+ny+nq,nu+ny+nq)), cached_cv_weights:vec![], cached_mv_weights:vec![],
            solver:None,free_workspace:vec![0.0;ny],gradient_workspace:vec![0.0;nu+ny+nq],bounds_workspace:Vec::with_capacity(4*nu+3*ny+3*nq),
            history_response,tail_slopes,previous_mv:None,previous_dv:None,previous_prediction:None,
            increments:vec![0.0;history_len*(nm+nd)],old_increments:vec![0.0;nm+nd],history_len,cursor:0,observed_samples:0 })
    }
    /// A gap is not one sample interval: discard increments and bias before observing again.
    pub fn reset_observation_history(&mut self) {
        self.previous_mv = None;
        self.previous_dv = None;
        self.previous_prediction = None;
        self.increments.fill(0.0);
        self.old_increments.fill(0.0);
        self.free_workspace.fill(0.0);
        self.cursor = 0;
        self.observed_samples = 0;
    }
    /// The longest new-model MV/DV kernel, not an arbitrary fixed warm-up count.
    /// Integrating tails need a separately qualified state estimator; finite input
    /// history alone does not establish their prehistory slope.
    pub fn history_ready(&self) -> bool {
        self.observed_samples > self.history_len
            && self.tail_slopes.iter().all(|slope| *slope == 0.0)
    }

    pub fn history_samples(&self) -> (usize, usize) {
        (self.observed_samples, self.history_len.saturating_add(1))
    }

    /// Rebuild from independently observed PV and applied-coordinate MV/DV
    /// history. Seed the estimator from the new model's free response, never an
    /// old prediction or a hypothetical candidate move that was not dispatched.
    pub fn restore_observations<'a>(&mut self, samples: impl IntoIterator<Item = (&'a [f64], &'a [f64], &'a [f64])>) -> Result<()> {
        self.reset_observation_history();
        let mut last_pvs = None;
        for (pvs, mvs, dvs) in samples {
            ensure!(pvs.len() == self.model.variables.cvs.len()
                && mvs.len() == self.model.variables.mvs.len() && dvs.len() == self.model.variables.dvs.len()
                && pvs.iter().chain(mvs).chain(dvs).all(|v| v.is_finite()), "Invalid applied-coordinate observation history");
            self.observe_inputs(mvs, dvs);
            last_pvs = Some(pvs);
        }
        ensure!(self.history_ready(), "Insufficient new-model history or unqualified integrating tail");
        self.rebuild_free_response(last_pvs.expect("qualified history is nonempty"));
        ensure!(self.free_workspace.iter().all(|value| value.is_finite()), "Nonfinite reconstructed free response");
        self.previous_prediction = Some(self.free_workspace[..self.model.variables.cvs.len()].to_vec());
        Ok(())
    }

    fn observe_inputs(&mut self, current_mvs: &[f64], current_dvs: &[f64]) {
        let nm = self.model.variables.mvs.len();
        let ni = nm + self.model.variables.dvs.len();
        self.cursor = (self.cursor + self.history_len - 1) % self.history_len;
        for j in 0..ni {
            let idx = self.cursor * ni + j;
            self.old_increments[j] += self.increments[idx];
            self.increments[idx] = if j < nm {
                self.previous_mv.as_ref().map_or(0.0, |v| current_mvs[j] - v[j])
            } else {
                self.previous_dv.as_ref().map_or(0.0, |v| current_dvs[j - nm] - v[j - nm])
            };
        }
        match &mut self.previous_mv { Some(v) => v.copy_from_slice(current_mvs), None => self.previous_mv = Some(current_mvs.to_vec()) }
        match &mut self.previous_dv { Some(v) => v.copy_from_slice(current_dvs), None => self.previous_dv = Some(current_dvs.to_vec()) }
        self.observed_samples = self.observed_samples.saturating_add(1);
    }

    fn rebuild_free_response(&mut self, current_pvs: &[f64]) {
        let nc = self.model.variables.cvs.len();
        let ni = self.model.variables.mvs.len() + self.model.variables.dvs.len();
        for (i, pv) in current_pvs.iter().enumerate() {
            for t in 0..self.model.tuning.prediction_horizon {
                let mut y = *pv;
                for j in 0..ni {
                    let kernel = ((t * nc + i) * ni + j) * self.history_len;
                    // Split at the ring wrap to avoid a modulo per multiply.
                    let recent = self.history_len - self.cursor;
                    for age in 0..recent {
                        y += self.increments[(self.cursor + age) * ni + j] * self.history_response[kernel + age];
                    }
                    for age in recent..self.history_len {
                        y += self.increments[(age - recent) * ni + j] * self.history_response[kernel + age];
                    }
                    y += self.old_increments[j] * (t + 1) as f64 * self.tail_slopes[i * ni + j];
                }
                self.free_workspace[t * nc + i] = y;
            }
        }
    }


    /// Observations update history from measured effective inputs; solving does not grant actuation authority.
    #[allow(clippy::too_many_arguments)]
    pub fn next_move(&mut self,current_pvs:&[f64],targets:&[f64],current_mvs:&[f64],current_dvs:&[f64],mv_targets:&[f64],dynamic_min:&[f64],dynamic_max:&[f64],cv_min:&[f64],cv_max:&[f64],cv_weights:&[f64],cv_alphas:&[f64],mv_weights:&[f64])->DmcResult {
        let nc=self.model.variables.cvs.len(); let nm=self.model.variables.mvs.len(); let nd=self.model.variables.dvs.len();
        let p=self.model.tuning.prediction_horizon; let m=self.model.tuning.control_horizon;
        let nu=nm*m; let ny=nc*p;
        let nq=if self.model.control_strategy.method==ControlMethod::SoftBand {ny}else{0};
        let failed=|status| DmcResult{next_move:vec![0.0;nm],future_plan:vec![0.0;nu],predicted_pvs:vec![0.0;ny],status,cv_bias:vec![0.0;nc],objective_value:0.0};
        for (slice,len) in [(current_pvs,nc),(targets,nc),(current_mvs,nm),(current_dvs,nd),(mv_targets,nm),(dynamic_min,nm),(dynamic_max,nm),(cv_min,nc),(cv_max,nc),(cv_weights,nc),(cv_alphas,nc),(mv_weights,nm)] {
            if slice.len()!=len || slice.iter().any(|v|!v.is_finite()) { return failed(SolverStatus::Unsolved); }
        }
        if cv_weights.iter().chain(mv_weights).any(|v|*v<0.0) || cv_alphas.iter().any(|v| !(0.0..=1.0).contains(v)) {return failed(SolverStatus::Unsolved);}
        if dynamic_min.iter().zip(dynamic_max).any(|(lo,hi)| lo >= hi)
            || cv_min.iter().zip(cv_max).any(|(lo,hi)| lo >= hi) {return failed(SolverStatus::Unsolved);}
        // Readback at this scan is authoritative, including external/manual changes and failed writes.
        self.observe_inputs(current_mvs, current_dvs);
        self.rebuild_free_response(current_pvs);
        let free=&mut self.free_workspace;
        let mut bias=vec![0.0;nc];
        for i in 0..nc {
            if let Some(prev)=&self.previous_prediction {bias[i]=current_pvs[i]-prev[i];}
        }
        let q=&mut self.gradient_workspace;
        q.fill(0.0);
        for t in 0..p { for i in 0..nc {
            let target=match self.model.variables.cvs[i].optimization_mode {
                OptimizationMode::Target{..}=>targets[i], OptimizationMode::Maximize=>cv_max[i], OptimizationMode::Minimize=>cv_min[i],
                OptimizationMode::Zone=>current_pvs[i].clamp(cv_min[i],cv_max[i]),
            };
            let reference=target-(target-current_pvs[i])*cv_alphas[i].powi((t+1) as i32);
            let weight=cv_weights[i]/self.model.variables.cvs[i].ece_factor.powi(2)*if t+1==p {self.model.tuning.terminal_weight_factor}else{1.0};
            let weight=weight*self.model.control_strategy.bands.get(i).map_or(1.0,|band|band.tracking_scale);
            for c in 0..nu {q[c]-=2.0*self.matrix_a[(t*nc+i,c)]*weight*(reference-free[t*nc+i]);}
        }}
        if self.cached_cv_weights!=cv_weights || self.cached_mv_weights!=mv_weights {
            let mut h=DMatrix::zeros(nu+ny+nq,nu+ny+nq);
            for i in 0..nc {for r in 0..nu {for c in 0..nu {h[(r,c)]+=cv_weights[i]*self.cv_gram[i][(r,c)];}}}
            for k in 0..m {for j in 0..nm {h[(k*nm+j,k*nm+j)]+=2.0*mv_weights[j];}}
            for t in 0..p {for i in 0..nc {h[(nu+t*nc+i,nu+t*nc+i)]=2.0*self.model.variables.cvs[i].slack_weight;}}
            for r in 0..nq {
                let i=r%nc;
                h[(nu+ny+r,nu+ny+r)]=2.0*self.model.control_strategy.bands[i].quadratic_penalty/self.model.variables.cvs[i].ece_factor.powi(2);
            }
            for j in 0..nm {
                let mv=&self.model.variables.mvs[j];
                let w=match mv.optimization_mode {MvOptimizationMode::Target{..}=>mv.target_weight,_=>if mv.target_weight>0.0 {mv.target_weight}else{1.0}};
                for a in 0..m {for b in 0..m {h[(a*nm+j,b*nm+j)]+=2.0*w*(m-a.max(b)) as f64;}}
            }
            self.cached_p=to_csc(&h); self.cached_cv_weights=cv_weights.to_vec(); self.cached_mv_weights=mv_weights.to_vec();
            // Changing weights may change sparsity; rebuild only on such changes.
            self.solver=None;
        }
        for j in 0..nm {
            let mv=&self.model.variables.mvs[j];
            let (target,w)=match mv.optimization_mode {
                MvOptimizationMode::Target{..}=>(mv_targets.get(j).copied().unwrap_or(current_mvs[j]),mv.target_weight),
                MvOptimizationMode::Maximize=>(dynamic_max[j],if mv.target_weight>0.0 {mv.target_weight}else{1.0}),
                MvOptimizationMode::Minimize=>(dynamic_min[j],if mv.target_weight>0.0 {mv.target_weight}else{1.0}),
            };
            for k in 0..m {q[k*nm+j]-=2.0*w*(target-current_mvs[j])*(m-k) as f64;}
        }
        let bounds=&mut self.bounds_workspace;
        bounds.clear();
        for _ in 0..2 {for _ in 0..m {for mv in &self.model.variables.mvs {bounds.push(mv.max_move);}}}
        for _ in 0..m {for j in 0..nm {bounds.push(dynamic_max[j]-current_mvs[j]);}}
        for _ in 0..m {for j in 0..nm {bounds.push(current_mvs[j]-dynamic_min[j]);}}
        for r in 0..ny {bounds.push(cv_max[r%nc]-free[r]);}
        for r in 0..ny {bounds.push(free[r]-cv_min[r%nc]);}
        bounds.resize(4*nu+3*ny,0.0);
        for r in 0..nq {
            let i=r%nc;
            bounds.push(targets[i]+self.model.control_strategy.bands[i].half_width-free[r]);
        }
        for r in 0..nq {
            let i=r%nc;
            bounds.push(free[r]-targets[i]+self.model.control_strategy.bands[i].half_width);
        }
        bounds.resize(4*nu+3*ny+3*nq,0.0);
        if let Some(solver)=&mut self.solver {
            if solver.update_q(&*q).is_err() || solver.update_b(&*bounds).is_err() {
                return failed(SolverStatus::Unsolved);
            }
        } else {
            let mut settings=DefaultSettings::default();
            settings.verbose=false; settings.presolve_enable=false;settings.input_sparse_dropzeros=false;
            settings.max_iter=self.model.tuning.max_iterations.min(u32::MAX as usize) as u32;
            settings.tol_gap_abs=self.model.tuning.solver_tolerance;
            settings.tol_gap_rel=self.model.tuning.solver_tolerance;
            settings.tol_feas=self.model.tuning.solver_tolerance;
            let cones=[SupportedConeT::NonnegativeConeT(bounds.len())];
            self.solver=match DefaultSolver::new(&self.cached_p,q,&self.constraints,bounds,&cones,settings) {
                Ok(s)=>Some(s),Err(_)=>return failed(SolverStatus::Unsolved)
            };
        }
        let solver=self.solver.as_mut().expect("solver initialized above");
        solver.solve();
        if !matches!(solver.solution.status,SolverStatus::Solved|SolverStatus::AlmostSolved) {self.previous_prediction=None; return failed(solver.solution.status);}
        if solver.solution.x.len() != nu + ny + nq || !solver.solution.obj_val.is_finite()
            || solver.solution.x.iter().any(|v| !v.is_finite()) {
            self.previous_prediction = None;
            return failed(SolverStatus::Unsolved);
        }
        let plan=solver.solution.x[..nu].to_vec();
        let mut prediction=free.clone();
        for r in 0..ny {for c in 0..nu {prediction[r]+=self.matrix_a[(r,c)]*plan[c];}}
        if prediction.iter().chain(&bias).any(|v| !v.is_finite()) {
            self.previous_prediction = None;
            return failed(SolverStatus::Unsolved);
        }
        // Only a diagnostic one-step expectation; never used as state/free response.
        match &mut self.previous_prediction {Some(v)=>v.copy_from_slice(&prediction[..nc]),None=>self.previous_prediction=Some(prediction[..nc].to_vec())}
        DmcResult{next_move:plan[..nm].to_vec(),future_plan:plan,predicted_pvs:prediction,status:solver.solution.status,cv_bias:bias,objective_value:solver.solution.obj_val}
    }
}
fn to_csc(dense:&DMatrix<f64>)->CscMatrix<f64> {
    let mut columns=Vec::with_capacity(dense.ncols()+1); let mut rows=Vec::new(); let mut values=Vec::new(); columns.push(0);
    for c in 0..dense.ncols() {for r in 0..dense.nrows() {let v=dense[(r,c)]; if v!=0.0 {rows.push(r);values.push(v);}} columns.push(values.len());}
    CscMatrix::new(dense.nrows(),dense.ncols(),columns,rows,values)
}

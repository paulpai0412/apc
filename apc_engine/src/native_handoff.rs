//! Bounded observed-history reconstruction for approved in-epoch model handoff.
use std::collections::VecDeque;
use anyhow::{ensure, Result};
use crate::{config::UnifiedModel, dmc::DmcController, native_lifecycle::Commissioning};

pub struct Sample { pub pvs: Vec<f64>, pub mvs: Vec<f64>, pub dvs: Vec<f64> }
pub struct History { samples: VecDeque<Sample>, capacity: usize }
impl History {
    pub fn new(capacity: usize) -> Self { Self { samples: VecDeque::with_capacity(capacity), capacity } }
    pub fn clear(&mut self) { self.samples.clear(); }
    pub fn observe(&mut self, pvs: &[f64], mvs: &[f64], dvs: &[f64], observed_samples: usize) {
        if observed_samples <= 1 { self.clear(); }
        if self.samples.len() == self.capacity {
            let mut sample = self.samples.pop_front().expect("full history");
            sample.pvs.copy_from_slice(pvs); sample.mvs.copy_from_slice(mvs); sample.dvs.copy_from_slice(dvs);
            self.samples.push_back(sample);
        } else {
            self.samples.push_back(Sample { pvs: pvs.to_vec(), mvs: mvs.to_vec(), dvs: dvs.to_vec() });
        }
    }
    pub fn reconstruct(&self, controller: &mut DmcController) -> Result<()> {
        let required = controller.history_samples().1;
        ensure!(self.capacity >= required, "Commissioned history capacity is smaller than candidate kernel; stopped reconfiguration required");
        ensure!(self.samples.len() >= required, "Missing sufficient contiguous new-model PV/MV/DV history");
        controller.restore_observations(self.samples.iter()
            .map(|s| (s.pvs.as_slice(), s.mvs.as_slice(), s.dvs.as_slice())))?;
        Ok(())
    }
}

/// A hot cutover cannot silently rebind physical coordinates, timing, or live
/// settings. A different commissioning topology needs an explicit stopped start.
pub fn compatible(current: &UnifiedModel, next: &UnifiedModel, profile: &Commissioning) -> Result<()> {
    profile.validate_model(next)?;
    ensure!(current.tuning.sample_time == next.tuning.sample_time, "Hot handoff cannot change scan interval");
    ensure!(current.variables.cvs.len() == next.variables.cvs.len()
        && current.variables.mvs.len() == next.variables.mvs.len()
        && current.variables.dvs.len() == next.variables.dvs.len(), "Hot handoff variable dimensions differ");
    for (old, new) in current.variables.cvs.iter().zip(&next.variables.cvs) {
        ensure!(old.name == new.name && old.units == new.units && old.node_ids == new.node_ids && old.limits == new.limits,
            "Hot handoff cannot rebind CV coordinates or operating limits");
    }
    for (old, new) in current.variables.mvs.iter().zip(&next.variables.mvs) {
        ensure!(old.name == new.name && old.units == new.units && old.node_ids == new.node_ids && old.limits == new.limits,
            "Hot handoff cannot rebind MV command/applied coordinates or operating limits");
    }
    for (old, new) in current.variables.dvs.iter().zip(&next.variables.dvs) {
        ensure!(old.name == new.name && old.units == new.units && old.node_ids == new.node_ids && old.limits == new.limits,
            "Hot handoff cannot rebind DV coordinates or operating limits");
    }
    Ok(())
}

pub fn validate_first_action(model: &UnifiedModel, moves: &[f64], prediction: &[f64], approved: &[f64]) -> Result<()> {
    ensure!(moves.len() == model.variables.mvs.len() && approved.len() == moves.len()
        && prediction.len() == model.variables.cvs.len() * model.tuning.prediction_horizon, "Candidate first-action dimensions differ");
    ensure!(moves.iter().zip(approved).zip(&model.variables.mvs).all(|((value, bound), mv)|
        value.is_finite() && bound.is_finite() && *bound > 0.0 && *bound <= mv.max_move && value.abs() <= *bound),
        "Approved candidate first action exceeded its independently approved bound");
    for row in prediction.chunks_exact(model.variables.cvs.len()) {
        ensure!(row.iter().zip(&model.variables.cvs).all(|(value, cv)|
            value.is_finite() && *value >= cv.limits.low_low && *value <= cv.limits.high_high),
            "Candidate horizon exceeds hard CV safety envelope");
    }
    Ok(())
}

/// A remote-mode request must not expose a stale SP register to the actuator.
/// These vectors come from an independently released, fresh acquired baseline.
pub fn validate_alignment(baseline: &[f64], command_readback: &[f64], applied: &[f64], tolerance: f64) -> Result<()> {
    ensure!(!baseline.is_empty() && baseline.len() == command_readback.len() && baseline.len() == applied.len()
        && tolerance.is_finite() && tolerance >= 0.0, "Invalid pre-acquire alignment coordinates");
    ensure!(baseline.iter().zip(command_readback).zip(applied).all(|((expected, command), actual)|
        expected.is_finite() && command.is_finite() && actual.is_finite()
            && (expected-command).abs() <= tolerance && (expected-actual).abs() <= tolerance),
        "SP register or independently applied baseline changed before remote acquisition");
    Ok(())
}

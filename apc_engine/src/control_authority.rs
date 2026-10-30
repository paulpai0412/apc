//! Command authority is separate from solver success and PID output telemetry.
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAction {
    Idle,
    Observe,
    Align,
    Control,
    Hold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldReason {
    AwaitingOperatorReset,
    InvalidMode,
    LostRemoteCascade,
    InvalidInput,
    InvalidCommand,
    UnconfirmedWrite,
    SolverFailure,
    StaleAcquisition,
    DiscontinuousScan,
}

/// Startup/reconnect/fault requires a complete healthy Monitor solve, then explicit Engage.
/// A server acknowledgement is neither an observation nor evidence of command application.
pub struct ControlAuthority {
    armed: bool,
    engaged: bool,
    reason: Option<HoldReason>,
}

impl Default for ControlAuthority {
    fn default() -> Self {
        Self { armed: false, engaged: false, reason: Some(HoldReason::AwaitingOperatorReset) }
    }
}

impl ControlAuthority {
    pub fn trip(&mut self, reason: HoldReason) {
        self.armed = false;
        self.engaged = false;
        self.reason = Some(reason);
    }

    pub fn reason(&self) -> Option<HoldReason> {
        self.reason
    }
    /// Called only after a fresh, complete, valid Monitor scan and successful solve.
    pub fn healthy_monitor(&mut self) {
        self.armed = true;
        self.engaged = false;
        self.reason = None;
    }


    pub fn evaluate(&mut self, operating_mode: f64, actual: &[f64], requested: &[f64]) -> ControlAction {
        let valid = |value: f64, highest: f64| value.is_finite() && value.fract() == 0.0 && value >= 0.0 && value <= highest;
        if !valid(operating_mode, 2.0) || actual.is_empty() || actual.len() != requested.len()
            || actual.iter().chain(requested).any(|value| !valid(*value, 3.0)) {
            self.trip(HoldReason::InvalidMode);
            return ControlAction::Hold;
        }
        if operating_mode != 2.0 {
            self.armed = false;
            self.engaged = false;
            if operating_mode == 0.0 {
                self.reason = Some(HoldReason::AwaitingOperatorReset);
                return ControlAction::Idle;
            }
            return ControlAction::Observe;
        }
        if !self.armed {
            return ControlAction::Hold;
        }
        if actual.iter().zip(requested).all(|(actual, requested)| *actual == 3.0 && *requested == 3.0) {
            self.engaged = true;
            self.reason = None;
            return ControlAction::Control;
        }
        if self.engaged || requested.iter().any(|mode| *mode != 3.0) {
            self.trip(HoldReason::LostRemoteCascade);
            return ControlAction::Hold;
        }
        ControlAction::Align
    }
}

pub struct CommandLimits<'a> {
    pub low: &'a [f64],
    pub high: &'a [f64],
    pub low_low: &'a [f64],
    pub high_high: &'a [f64],
    pub max_move: &'a [f64],
    /// Absolute engineering-unit tolerance for numerical solver boundary residue.
    pub tolerance: f64,
}

impl CommandLimits<'_> {
    /// Convert a whole proposed move vector to bounded absolute applied-SP coordinates.
    /// The caller must discard the entire vector on error, without issuing any write.
    pub fn absolute_commands(&self, applied_sp: &[f64], moves: &mut [f64]) -> Result<(), &'static str> {
        let count = applied_sp.len();
        if count == 0 || moves.len() != count
            || [self.low.len(), self.high.len(), self.low_low.len(), self.high_high.len(), self.max_move.len()].iter().any(|length| *length != count) {
            return Err("Command and engineering-limit dimensions differ");
        }
        if !self.tolerance.is_finite() || self.tolerance < 0.0 {
            return Err("Invalid command-boundary tolerance");
        }
        for i in 0..count {
            let current = applied_sp[i];
            if [current, moves[i], self.low[i], self.high[i], self.low_low[i], self.high_high[i], self.max_move[i]]
                .iter().any(|value| !value.is_finite()) {
                return Err("Nonfinite command, feedback or limit");
            }
            if self.low_low[i] > self.low[i] || self.low[i] >= self.high[i]
                || self.high[i] > self.high_high[i] || self.max_move[i] <= 0.0 {
                return Err("Invalid engineering-limit order or maximum move");
            }
            if current < self.low_low[i] || current > self.high_high[i] {
                return Err("Applied SP is outside hard engineering limits");
            }
            let low = self.low[i].max(current - self.max_move[i]);
            let high = self.high[i].min(current + self.max_move[i]);
            if low > high {
                return Err("No feasible move can satisfy operating and movement limits");
            }
            let proposed = current + moves[i];
            if !proposed.is_finite() {
                return Err("Proposed absolute SP is nonfinite");
            }
            let bounded = proposed.clamp(low, high);
            if (bounded - proposed).abs() > self.tolerance {
                return Err("Proposed move materially violates engineering limits");
            }
            moves[i] = bounded;
        }
        Ok(())
    }
}

/// New measurements may arrive during a bounded solve. Command coordinates, limits,
/// tuning, targets and operator modes must still match the solved contract.
pub fn dispatch_contract_matches(
    initial: &[f64],
    current: &[f64],
    measurement_ranges: &[std::ops::Range<usize>],
) -> bool {
    initial.len() == current.len()
        && measurement_ranges.iter().all(|range| range.start <= range.end && range.end <= initial.len())
        && initial.iter().zip(current).enumerate().all(|(index, (before, after))| {
            before.is_finite() && after.is_finite()
                && (before == after || measurement_ranges.iter().any(|range| range.contains(&index)))
        })
}

/// No OPC source/server timestamp is implicitly interpreted as an acquisition timestamp.
/// The external producer publishes odd before acquiring process values, even when done.
/// Stable even values bracket a snapshot; a newer even value proves an acquisition after
/// our previous observation. Local monotonic time bounds that evidence without clock sync.
#[derive(Default)]
pub struct AcquisitionEvidence {
    previous: Option<(u64, Instant)>,
    acquired_after: Option<Instant>,
}

impl AcquisitionEvidence {
    pub fn observe(&mut self, before: f64, after: f64, now: Instant, maximum_age: Duration) -> Result<Instant, &'static str> {
        let valid = |v: f64| v.is_finite() && v >= 0.0 && v <= (1_u64 << 53) as f64 && v.fract() == 0.0;
        if !valid(before) || !valid(after) || before != after || (after as u64) % 2 != 0 {
            self.previous = None;
            self.acquired_after = None;
            return Err("Acquisition sequence missing, torn, odd or invalid");
        }
        let sequence = after as u64;
        let previous = self.previous.replace((sequence, now));
        match previous {
            Some((old, observed)) if sequence > old => self.acquired_after = Some(observed),
            Some((old, _)) if sequence == old => {}
            _ => self.acquired_after = None,
        }
        match self.acquired_after {
            Some(observed) if now.duration_since(observed) <= maximum_age => Ok(observed),
            _ => Err("No observed acquisition advancement within the allowed age; healthy monitoring required"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Confirmation { None, Waiting, Applied }

/// Preserve expected and pre-command effective SPs, never PID OP or server acknowledgement.
pub struct PendingCommands {
    expected: Vec<f64>,
    previous: Vec<f64>,
    issued: Option<Instant>,
}

impl PendingCommands {
    pub fn new(count: usize) -> Self {
        Self { expected: vec![0.0; count], previous: vec![0.0; count], issued: None }
    }

    pub fn begin(&mut self, expected: &[f64], previous: &[f64], now: Instant) -> Result<(), &'static str> {
        if self.issued.is_some() || expected.len() != self.expected.len() || previous.len() != self.previous.len()
            || expected.iter().chain(previous).any(|v| !v.is_finite()) {
            return Err("Cannot replace an unresolved command or issue an invalid command vector");
        }
        self.expected.copy_from_slice(expected);
        self.previous.copy_from_slice(previous);
        self.issued = Some(now);
        Ok(())
    }

    pub fn deadline(&self, timeout: Duration) -> Option<Instant> {
        self.issued.and_then(|issued| issued.checked_add(timeout))
    }

    pub fn expire_if_due(&mut self, now: Instant, timeout: Duration) -> bool {
        if self.deadline(timeout).is_some_and(|deadline| now >= deadline) {
            self.issued = None;
            true
        } else { false }
    }

    pub fn io_budget(&self, now: Instant, maximum: Duration, timeout: Duration) -> Duration {
        self.deadline(timeout).map_or(maximum, |deadline| maximum.min(deadline.saturating_duration_since(now)))
    }

    pub fn check(&mut self, feedback: &[f64], now: Instant, tolerance: f64, deadline: Duration) -> Result<Confirmation, &'static str> {
        let Some(issued) = self.issued else { return Ok(Confirmation::None); };
        if feedback.len() != self.expected.len() || !tolerance.is_finite() || tolerance < 0.0
            || feedback.iter().any(|v| !v.is_finite()) {
            self.issued = None;
            return Err("Invalid effective-SP confirmation sample");
        }
        // Late matching feedback is still a deadline violation.
        if now.duration_since(issued) > deadline {
            self.issued = None;
            return Err("Effective-SP application confirmation deadline exceeded");
        }
        if feedback.iter().zip(&self.expected).all(|(a,b)| (a-b).abs() <= tolerance) {
            self.issued = None;
            return Ok(Confirmation::Applied);
        }
        if feedback.iter().zip(self.expected.iter().zip(&self.previous))
            .any(|(value,(expected,previous))| (value-expected).abs() > tolerance && (value-previous).abs() > tolerance) {
            self.issued = None;
            return Err("Effective-SP feedback differs from both previous and issued SP");
        }
        if now.duration_since(issued) == deadline {
            self.issued = None;
            return Err("Effective-SP application confirmation deadline reached");
        }
        Ok(Confirmation::Waiting)
    }
}

/// Validate every engineering vector before any index, solver update or authority transition.
pub fn validate_process_group(values: &[f64], targets: Option<&[f64]>, low: &[f64], high: &[f64],
    low_low: &[f64], high_high: &[f64], expected: usize) -> Result<(), &'static str> {
    if [values, low, high, low_low, high_high].iter().any(|v| v.len() != expected || v.iter().any(|x| !x.is_finite()))
        || targets.is_some_and(|v| v.len() != expected || v.iter().any(|x| !x.is_finite())) {
        return Err("Incomplete or nonfinite engineering vectors");
    }
    for i in 0..expected {
        if !crate::config::ordered_limits(low_low[i], low[i], high[i], high_high[i])
            || values[i] < low_low[i] || values[i] > high_high[i]
            || targets.is_some_and(|t| t[i] < low[i] || t[i] > high[i]) {
            return Err("Invalid limit ordering, feedback or target outside engineering limits");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_measurement_updates_do_not_invalidate_an_unchanged_dispatch_contract() {
        // mode, CV, target, applied SP, OP, DV, low, high
        let initial = [2.0, 10.0, 12.0, 100.0, 45.0, 20.0, 5.0, 15.0];
        let latest = [2.0, 10.2, 12.0, 100.0, 46.0, 20.1, 5.0, 15.0];
        let measurements = [1..2, 4..6];
        assert!(dispatch_contract_matches(&initial, &latest, &measurements));
        for index in [0, 2, 3, 6, 7] {
            let mut changed = latest;
            changed[index] += 1.0;
            assert!(!dispatch_contract_matches(&initial, &changed, &measurements));
        }
        let mut invalid = latest;
        invalid[1] = f64::NAN;
        assert!(!dispatch_contract_matches(&initial, &invalid, &measurements));
        assert!(!dispatch_contract_matches(&initial, &latest[..7], &measurements));
        assert!(!dispatch_contract_matches(&initial, &latest, &[0..9]));
    }

    #[test]
    fn startup_and_failed_writes_require_observation_then_operator_reengagement() {
        let mut authority = ControlAuthority::default();
        assert_eq!(authority.evaluate(2.0, &[3.0, 3.0], &[3.0, 3.0]), ControlAction::Hold);
        assert_eq!(authority.reason(), Some(HoldReason::AwaitingOperatorReset));
        assert_eq!(authority.evaluate(1.0, &[3.0, 3.0], &[3.0, 3.0]), ControlAction::Observe);
        assert_eq!(authority.evaluate(2.0, &[3.0, 3.0], &[3.0, 3.0]), ControlAction::Hold);
        authority.healthy_monitor();
        assert_eq!(authority.evaluate(2.0, &[3.0, 3.0], &[3.0, 3.0]), ControlAction::Control);
        authority.trip(HoldReason::UnconfirmedWrite);
        assert_eq!(authority.evaluate(2.0, &[3.0, 3.0], &[3.0, 3.0]), ControlAction::Hold);
        assert_eq!(authority.reason(), Some(HoldReason::UnconfirmedWrite));
        assert_eq!(authority.evaluate(1.0, &[3.0, 3.0], &[3.0, 3.0]), ControlAction::Observe);
        authority.healthy_monitor();
        assert_eq!(authority.evaluate(2.0, &[3.0, 3.0], &[3.0, 3.0]), ControlAction::Control);
    }

    #[test]
    fn a_coupled_plan_requires_every_pid_and_withdrawal_latches_hold() {
        let mut authority = ControlAuthority::default();
        authority.evaluate(1.0, &[3.0, 1.0], &[3.0, 3.0]);
        authority.healthy_monitor();
        assert_eq!(authority.evaluate(2.0, &[3.0, 1.0], &[3.0, 3.0]), ControlAction::Align);
        assert_eq!(authority.evaluate(2.0, &[3.0, 3.0], &[3.0, 3.0]), ControlAction::Control);
        assert_eq!(authority.evaluate(2.0, &[3.0, 3.0], &[3.0, 1.0]), ControlAction::Hold);
        assert_eq!(authority.reason(), Some(HoldReason::LostRemoteCascade));
        assert_eq!(authority.evaluate(2.0, &[3.0, 3.0], &[3.0, 3.0]), ControlAction::Hold);
    }

    #[test]
    fn fractional_unknown_and_nonfinite_modes_never_authorize_commands() {
        for mode in [-1.0, 2.9, 3.0, f64::NAN, f64::INFINITY] {
            let mut authority = ControlAuthority::default();
            authority.evaluate(1.0, &[3.0], &[3.0]);
            authority.healthy_monitor();
            assert_eq!(authority.evaluate(mode, &[3.0], &[3.0]), ControlAction::Hold);
        }
        let mut authority = ControlAuthority::default();
        authority.evaluate(1.0, &[3.0], &[3.0]);
        authority.healthy_monitor();
        assert_eq!(authority.evaluate(2.0, &[3.5], &[3.0]), ControlAction::Hold);
        assert_eq!(authority.evaluate(2.0, &[], &[]), ControlAction::Hold);
    }

    #[test]
    fn absolute_commands_use_applied_setpoint_units_and_enforce_the_whole_vector() {
        let limits = CommandLimits { low: &[80.0, 800.0], high: &[120.0, 1000.0],
            low_low: &[70.0, 700.0], high_high: &[130.0, 1100.0], max_move: &[1.5, 8.0], tolerance: 1e-6 };
        let mut moves = [1.0, -2.0];
        limits.absolute_commands(&[100.0, 900.0], &mut moves).unwrap();
        assert_eq!(moves, [101.0, 898.0]);
        let mut excessive = [1.0, 8.01];
        assert!(limits.absolute_commands(&[100.0, 900.0], &mut excessive).is_err());
        let mut wrong_coordinate = [1.0, 1.0];
        assert!(limits.absolute_commands(&[40.0, 60.0], &mut wrong_coordinate).is_err());
        let mut boundary_residue = [1.5 + 5e-7, 8.0];
        limits.absolute_commands(&[100.0, 900.0], &mut boundary_residue).unwrap();
        assert_eq!(boundary_residue, [101.5, 908.0]);
    }

    #[test]
    fn initial_unchanged_torn_regressed_and_stale_acquisitions_fail_closed() {
        let start = Instant::now();
        let age = Duration::from_secs(2);
        let mut evidence = AcquisitionEvidence::default();
        assert!(evidence.observe(10.0, 10.0, start, age).is_err());
        assert!(evidence.observe(10.0, 10.0, start + Duration::from_millis(100), age).is_err());
        assert!(evidence.observe(12.0, 12.0, start + Duration::from_secs(1), age).is_ok());
        assert!(evidence.observe(12.0, 12.0, start + Duration::from_millis(1500), age).is_ok());
        assert!(evidence.observe(12.0, 12.0, start + Duration::from_secs(4), age).is_err());
        assert!(evidence.observe(14.0, 16.0, start + Duration::from_secs(5), age).is_err());
        assert!(evidence.observe(16.0, 16.0, start + Duration::from_secs(6), age).is_err());
        assert!(evidence.observe(18.0, 18.0, start + Duration::from_secs(7), age).is_ok());
        assert!(evidence.observe(2.0, 2.0, start + Duration::from_secs(8), age).is_err());
        for value in [3.0, -2.0, f64::NAN, f64::INFINITY, 1.5] {
            assert!(evidence.observe(value, value, start + Duration::from_secs(9), age).is_err());
        }
    }

    #[test]
    fn partial_acknowledgement_is_not_application_and_blocks_replacement() {
        let now = Instant::now();
        let mut pending = PendingCommands::new(2);
        pending.begin(&[101.0, 902.0], &[100.0, 900.0], now).unwrap();
        let deadline = Duration::from_secs(2);
        assert_eq!(pending.check(&[101.0, 900.0], now + Duration::from_secs(1), 0.01, deadline).unwrap(), Confirmation::Waiting);
        assert!(pending.begin(&[102.0, 904.0], &[101.0, 900.0], now).is_err());
        assert!(pending.check(&[101.0, 900.0], now + deadline, 0.01, deadline).is_err());
        let mut authority = ControlAuthority::default();
        authority.healthy_monitor();
        authority.trip(HoldReason::UnconfirmedWrite);
        assert_eq!(authority.evaluate(2.0, &[3.0;2], &[3.0;2]), ControlAction::Hold);
        assert_eq!(authority.evaluate(1.0, &[3.0;2], &[3.0;2]), ControlAction::Observe);
        authority.healthy_monitor();
        assert_eq!(authority.evaluate(2.0, &[3.0;2], &[3.0;2]), ControlAction::Control);
        pending.begin(&[101.0, 902.0], &[101.0, 900.0], now + deadline).unwrap();
        assert_eq!(pending.check(&[101.005, 901.995], now + deadline, 0.01, deadline).unwrap(), Confirmation::Applied);
    }

    #[test]
    fn confirmation_rejects_op_coordinates_mismatch_and_late_matching_feedback() {
        let now = Instant::now();
        let mut pending = PendingCommands::new(1);
        pending.begin(&[101.0], &[100.0], now).unwrap();
        assert!(pending.check(&[40.0], now, 0.01, Duration::from_secs(1)).is_err());
        pending.begin(&[101.0], &[100.0], now).unwrap();
        assert!(pending.check(&[101.0], now + Duration::from_secs(2), 0.01, Duration::from_secs(1)).is_err());
        pending.begin(&[101.0], &[100.0], now).unwrap();
        assert!(pending.check(&[], now, 0.01, Duration::from_secs(1)).is_err());
    }

    #[test]
    fn all_engineering_vectors_must_be_complete_finite_and_ordered() {
        assert!(validate_process_group(&[5.0], Some(&[5.0]), &[1.0], &[9.0], &[0.0], &[10.0], 1).is_ok());
        assert!(validate_process_group(&[5.0], Some(&[]), &[1.0], &[9.0], &[0.0], &[10.0], 1).is_err());
        assert!(validate_process_group(&[5.0], Some(&[f64::NAN]), &[1.0], &[9.0], &[0.0], &[10.0], 1).is_err());
        assert!(validate_process_group(&[5.0], None, &[9.0], &[1.0], &[0.0], &[10.0], 1).is_err());
        assert!(validate_process_group(&[11.0], None, &[1.0], &[9.0], &[0.0], &[10.0], 1).is_err());
        assert!(validate_process_group(&[5.0], Some(&[10.0]), &[1.0], &[9.0], &[0.0], &[10.0], 1).is_err());
    }

    #[test]
    fn deadline_interrupts_waiting_without_another_feedback_or_write_ack() {
        let now = Instant::now();
        let deadline = Duration::from_secs(2);
        let mut pending = PendingCommands::new(1);
        pending.begin(&[101.0], &[100.0], now).unwrap();
        assert_eq!(pending.deadline(deadline), Some(now + deadline));
        assert_eq!(pending.io_budget(now + Duration::from_secs(1), Duration::from_secs(10), deadline), Duration::from_secs(1));
        assert!(!pending.expire_if_due(now + Duration::from_secs(1), deadline));
        assert!(pending.expire_if_due(now + deadline, deadline));
        assert_eq!(pending.deadline(deadline), None);
    }
    #[test]
    fn idle_failed_monitor_and_actual_pid_mode_loss_never_auto_engage() {
        let mut authority = ControlAuthority::default();
        assert_eq!(authority.evaluate(0.0, &[3.0;2], &[3.0;2]), ControlAction::Idle);
        assert_eq!(authority.evaluate(2.0, &[3.0;2], &[3.0;2]), ControlAction::Hold);
        assert_eq!(authority.evaluate(1.0, &[3.0;2], &[3.0;2]), ControlAction::Observe);
        authority.trip(HoldReason::SolverFailure);
        assert_eq!(authority.evaluate(2.0, &[3.0;2], &[3.0;2]), ControlAction::Hold);
        authority.evaluate(1.0, &[3.0;2], &[3.0;2]);
        authority.healthy_monitor();
        assert_eq!(authority.evaluate(2.0, &[3.0;2], &[3.0;2]), ControlAction::Control);
        assert_eq!(authority.evaluate(2.0, &[3.0, 1.0], &[3.0;2]), ControlAction::Hold);
        assert_eq!(authority.reason(), Some(HoldReason::LostRemoteCascade));
        assert_eq!(authority.evaluate(2.0, &[3.0;2], &[3.0;2]), ControlAction::Hold);
    }
}

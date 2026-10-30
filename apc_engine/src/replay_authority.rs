//! Explicit isolated ideal-actuator authority, not a vendor PID mode emulator.
use std::time::{Duration, Instant};
use anyhow::{ensure, Result};
use crate::native_lifecycle::Handoff;

pub struct IdealAuthority {
    active: bool, released: bool, last_watchdog: Option<(f64, Instant)>,
}
impl Default for IdealAuthority {
    fn default() -> Self { Self { active: false, released: false, last_watchdog: None } }
}
impl IdealAuthority {
    /// A watchdog expiry requires a newly observed explicit release before acquire.
    /// Returning false means hold the last accepted applied SP, never follow SP.
    pub fn observe(&mut self, contract: &Handoff, request: f64, watchdog: f64,
        targets: &[f64], remote: &[f64], now: Instant) -> Result<bool> {
        ensure!(request.is_finite() && watchdog.is_finite() && watchdog >= 0.0 && watchdog.fract() == 0.0
            && targets.len() == remote.len() && !targets.is_empty()
            && targets.iter().all(|v| v.is_finite()), "Invalid ideal authority acquisition");
        if watchdog > 0.0 && self.last_watchdog.is_none_or(|(old, _)| watchdog != old) {
            self.last_watchdog = Some((watchdog, now));
        }
        let alive = self.last_watchdog.is_some_and(|(_, at)| now.duration_since(at) <= Duration::from_millis(contract.watchdog_timeout_ms));
        if self.active && !alive { self.active = false; self.released = false; }
        if request == contract.release_value { self.active = false; self.released = true; }
        else if request == contract.acquire_value {
            if self.released && alive && targets == remote { self.active = true; }
            if targets != remote { self.active = false; self.released = false; }
        } else { self.active = false; self.released = false; return Err(anyhow::anyhow!("Unknown explicit fixture authority request")); }
        Ok(self.active)
    }
}

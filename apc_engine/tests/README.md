# APC Engine Test Suite

This folder contains integration tests for the APC engine.

## Running Tests

```powershell
cd apc_engine
cargo test              # Run all tests
cargo test dmc          # Run only DMC tests
cargo test --test dmc_basic_tests  # Run specific test file
```

## Test Organization

- `test_helpers.rs` - Shared utilities for creating test models
- `dmc_basic_tests.rs` - Core DMC algorithm tests (1x1, 2x2 control)
- `constraint_tests.rs` - MV/CV constraint handling
- `integration_tests.rs` - End-to-end controller behaviors
- `modelloader_tests.rs` - Model loading and validation
- `campaign_dmc_tests.rs` - Analytic live-weight/ECE/terminal objective, observed versus unexecuted moves, one-step horizons, infeasible bounds, and both control strategies (quality-band behavior, independent operating slack, hard bounds, live targets, strict settings)
- `campaign_tests.rs` - Independent second-order plant timing, fractional deadtime, actuator constraints, noise coordinates, full-row closed loop, and explicit solver failure

## Campaign regression and evaluation

From the repository root:

```fish
cargo test --manifest-path apc_engine/Cargo.toml --test campaign_dmc_tests --test campaign_tests
cargo build --release --manifest-path apc_engine/Cargo.toml --bins
```

`mpc_campaign replay --plant PLANT.json --scenario SCENARIO.json --output HISTORY.json`
generates every scheduled row from an independent second-order plant. `history.mv`
is the measured actuator at row t; `history.command_mv` is the command applied over
the following interval. Identification of the SP-to-CV controller must use the
command coordinate, including actuator lag in the fitted response.

`mpc_campaign closed-loop --plant PLANT.json --controller MODEL.json --scenario SCENARIO.json --output RESULT.json`
uses the real `DmcController` and Clarabel on every scan. Add
`--transport opc-da --bridge EXECUTABLE --bridge-arg ARG` (repeat `--bridge-arg`)
to use the synchronous COM DA bridge. Each scan updates simulator-owned inputs,
reads them through COM, solves, writes command items through COM, checks readback,
and advances the plant. No external production connection is required.

Outputs must be new paths. Invalid configurations, solver failures, bad quality,
stale timestamps, failed HRESULTs, hard-constraint violations, or readback mismatch
produce an invalid JSON artifact and nonzero exit, never a successful hold.
Full traces retain dynamic targets, actual actuators, applied commands, quality,
solver status and solve durations. Runtime score uses fixed normalized IAE,
CV violation fraction, and command total variation; its exact formula is included
in provenance. No metric depends on the candidate's objective weights.

Plant equations and scan timing are documented in `src/campaign_plant.rs`.
The controller uses bounded observed-input history rather than storing an
unexecuted future plan, and reuses the solver and workspaces when weights are
unchanged. These tests do not substitute for the full calendar campaign,
actual-COM parity, and explicit fault/restart-recovery evaluation.

## Control strategies

Omitting `control_strategy` preserves the standard DMC objective. An explicit
standard configuration is `{"method":"standard_dmc","version":1,"bands":[]}`.
The optional strategy is fixed when a controller is constructed, not hot-switched.
The fallible controller constructor rejects invalid settings before creating a QP.

For soft-band control, use `{"method":"soft_band","version":1,"bands":[{"half_width":1.0,"tracking_scale":0.01,"quadratic_penalty":100.0}]}`
with one band per CV in model order. Half widths are positive engineering-unit
distances from each live target. Tracking scales must be in `(0,1]`, and quadratic
penalties must be positive; all values must be finite. Version must be 1.
Only Target CV/MV optimization modes are supported; Zone and economic
Minimize/Maximize modes are rejected. Existing MV target tracking remains allowed.
Unknown strategy fields (including estimator options) are rejected.

Soft-band control scales the existing alpha-filtered CV tracking cost by
`tracking_scale`, keeping ECE normalization, terminal weighting, and live CV weights.
It adds a separate nonnegative quality slack per CV/prediction step, constrained
to cover excursions outside `live target ± half_width`, with cost
`quadratic_penalty * (quality_slack / ece_factor)^2`. The band itself is not
alpha-filtered. Existing CV operating limits and their slack costs remain
independent. MV magnitude and delta-MV constraints stay hard. No estimator is added.
Campaign and twin closed-loop artifacts include the actual `control_strategy`.

The numerical tests compare the two real QPs, check the analytic quality-excess
solution independently of CV tracking, and test both signs of hard MV/rate limits.
They do not validate the existing production OP-read/SP-write coordinate mapping;
campaign acceptance uses read-back applied commands with the documented FIR coordinate.

## Writing New Tests

All tests import the public API:

```rust
use apc_engine::dmc::DmcController;
use apc_engine::config::UnifiedModel;
mod test_helpers;  // Import shared helpers

#[test]
fn test_my_scenario() {
    let model = test_helpers::create_1x1_model();
    let controller = DmcController::new_from_coefficients(...).unwrap();
    // Test code here
}
```

## Notes

- Tests target the public API in `apc_engine` and use helper models from `test_helpers.rs`.

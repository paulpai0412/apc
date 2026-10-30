# Copilot Instructions - AuTHRity (code-derived)

## Architecture & Data Flow
- Multi-process system around OPC UA: `opcua_server` (UA server + hot-reload API), `virtual_plant` (simulator), `apc_engine` (MPC worker), `controller_host` (supervisor API), `hmi` (Rust backend + React frontend).
- `controller_host` deploys model JSONs and spawns `apc_engine` with CLI args (`--model`, `--opc`, `--pki`) and tracks process state; see [controller_host/src/main.rs](controller_host/src/main.rs).
- HMI streams OPC `OpcUpdate` messages over `/ws`. Writes use the authenticated, CSRF-protected `POST /api/runtime/write` and wait for checked OPC item acceptance; see `hmi/src/opc_worker.rs`, `hmi/src/web_routes.rs`, and `hmi/frontend/src/lib/opcWrite.ts`. Queueing or subscription updates are not write acknowledgments.
- `hmi` backend also acts as orchestration proxy: deployment bundle route (`/api/deploy`) forwards JSON to supervisor and YAML to OPC hot-reload API, and controller lifecycle proxy routes (`/api/prox/controllers/*`) forward start/stop/list with supervisor API key.
- Infrastructure endpoints exist in HMI (`/api/infrastructure`) backed by `config/hosts.json`; model list/node map are resolved from configured remote OPC server first.

## Control Loop (APC Engine)
- `apc_engine` loads a `UnifiedModel` JSON, generates step response from FOPDT when `model_type == "parametric"`, or uses explicit coefficients for `"step_response"`; see [apc_engine/src/config.rs](apc_engine/src/config.rs) and [apc_engine/src/modelloader.rs](apc_engine/src/modelloader.rs).
- Each scan validates a fresh, coherent external acquisition snapshot before `DmcController::next_move`. Monitor solves without normal MV dispatch; Engage additionally requires explicit arming, actual/requested PID RCAS modes, bounded solve-to-write age, and command-application confirmation.
- Model inputs use optional `node_ids.applied_sp`, otherwise SP register readback; PID OP is separate telemetry. Shared SP readback is not independent physical-application evidence. Bumpless alignment requires separate applied-SP feedback.
- A supplied model digest freezes CV weights/alphas and MV weights unless manual live tuning is explicitly enabled; immutable-version starts reject that override. Limits remain live. See `apc_engine/src/control_authority.rs` and the commissioning contract in `DIGITAL_TWIN_VALIDATION.md`.
- Runtime auth to OPC UA is controlled by CLI (`--auth-mode username|x509` with optional `--username`/`--password`) and is injected by `controller_host` start payload.

## OPC UA Naming & Integration
- Node IDs are defined in model JSON: CVs include `pv`, `target`, `prediction`; MVs include `sp`, optional `applied_sp`, `op`, `mode`, `mode_target`, `future_plan`; DVs include `pv` and `limits`; see [apc_engine/src/config.rs](apc_engine/src/config.rs).
- System nodes are prefixed by engine ID (model name or file stem), e.g. `{engine}:OperatingMode`, `{engine}:Heartbeat`, `{engine}:SolverStatus` in [apc_engine/src/main.rs](apc_engine/src/main.rs).

## HMI Frontend Conventions
- `/ws` accepts `{type:"REFRESH"}` and streams measurements. `writeOpcNode` owns HTTP writes and pending state; only an explicit checked response reports server acceptance. PID application is a separate engine gate.
- The original React Autoresearch workspace owns cleaning, model qualification, adaptive comparison and immutable candidate selection; the live HMI can browse/install approved versions without enabling research. See `validation/README.md` for artifact and API contracts.
- Model YAML is fetched via `/api/model?file=...` and tags are classified as CV/MV/DV by presence of `:Prediction` or `:FuturePlan`; see [hmi/frontend/src/App.tsx](hmi/frontend/src/App.tsx).
- Model Generator supports `parametric` and `step_response` workflows, plus Step Response Tool that computes coefficients from historian data via `/api/stepresponse/calculate`.
- In current UI, step-response coefficients are imported/attached and summarized, but there is no dedicated coefficient curve editor view yet.

## Model Schema Notes (Important)
- `UnifiedModel` now includes richer optimization/tuning fields in CV/MV structs (e.g. `optimization_mode`, `slack_weight`, `is_integrating`, optional MV `target` + `target_weight`). Preserve backward compatibility when adding parser/export changes.
- Physics supports both MV and DV parametric matrices (`gain/tau/dead_time` and `gain_dv/tau_dv/dead_time_dv`) and explicit response matrices (`step_coefficients`, `dv_coefficients`).
- Loader behavior remains mode-gated today: `parametric` generates coefficients from FOPDT; `step_response` consumes provided coefficients.

## Historian & Step Response
- HMI historian can query QuestDB trends (`/api/trends`) and provide available tags (`/api/stepresponse/tags`).
- Step response calculation endpoint normalizes CV deltas by detected MV step size, resamples to controller sample time, and returns raw + fitted response arrays for visualization and export.

## Simulator & OPC UA Server
- `virtual_plant` simulates Debutanizer, CSTR and pass balancing. It owns odd/even acquisition counters and sampled applied-SP/DV feedback; it does not initialize operator SP, modes, targets, limits or tuning. Provision nodes before starting it; see `virtual_plant/src/main.rs` and `virtual_plant/config/settings.toml`.
- `opcua_server` builds a UA server and exposes a hot-reload API to load YAML model nodes at runtime; see [opcua_server/src/main.rs](opcua_server/src/main.rs).

## Collaboration Preferences (User-Specific)
- The assistant is authorized to execute shell commands directly for scoped implementation and verification: local dependency/toolchain setup, builds, tests, isolated services, browser validation, and recording. Execute routine steps instead of asking the user to run them.
- Use an available shell with matching syntax. Bash is supported; use fish only when installed and appropriate for the script.
- Use trusted project scripts and official dependencies. Prefer user-owned installation directories and verify published checksums when available.
- Run integration and closed-loop checks against verified loopback endpoints with fresh disposable directories and databases. Start, stop, or interrupt only processes owned by that validation workspace.
- Obtain explicit target-and-action approval for privileged or system-wide changes, production access or control writes, destructive changes outside owned disposable artifacts, credential access or rotation, pushes/publication, and spending.
- Follow the host's tool and security restrictions. Preserve command logs and observed results; unexecuted checks remain unverified.

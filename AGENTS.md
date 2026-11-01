# Repository Guidelines

Guidance for AI assistants (and contributors) working in this repository.

## Project Overview

AuTHRity-APC is an OPC UA–based Advanced Process Control platform: a Dynamic Matrix Control (DMC) engine, an OPC UA server with YAML hot-reload, a plant simulator, a controller supervisor, and a web HMI with historian. Apache-2.0. Not certified for safety-critical use — outputs must be validated in the DCS/PLC (`README.md`).

Five **standalone** Rust binaries — **no cargo workspace**; each crate has its own `Cargo.toml` + `Cargo.lock` and builds from its own directory:

| Component | Binary | Role |
|---|---|---|
| `opcua_server/` | `authrity-opcua-server` | OPC UA server (ns=2), YAML node models, hot-reload REST API |
| `virtual_plant/` | `authrity-virtual-plant` | Simulator: Debutanizer + CSTR + PassBalancing; OPC UA client |
| `apc_engine/` | `authrity-apc-engine` | DMC/MPC controller (dual lib+bin); spawned per controller |
| `controller_host/` | `controller_host` | Supervisor HTTP API; stores model JSONs; spawns/kills engines by PID |
| `hmi/` | `authrity-hmi` | Axum backend: OPC UA ↔ WebSocket bridge, proxies, auth/RBAC, QuestDB historian |
| `hmi/frontend/` | — | React 19 + TypeScript + Vite SPA served by `hmi` |

## Architecture & Data Flow

```
virtual_plant ──writes PV/OP──▶ opcua_server ◀──read/write scan── apc_engine (per controller)
                                     ▲   ▲                              spawned by
                                     │   └────────── subscription ──────┤ controller_host
                                     │                                  ▲
                       hmi backend ◀─┘   deploy/lifecycle proxy (x-api-key)
                          │ WS OpcUpdate broadcast / WRITE commands
                     hmi/frontend (Zustand store)
hmi historian task ──ILP──▶ QuestDB (process_data table)
```

Key flows (evidence in parentheses):

1. **Plant → Server**: `virtual_plant` writes PVs/OPs every `cycle_time_ms`, reads SP/Mode back (`virtual_plant/src/main.rs:352-490`).
2. **Engine scan** (`apc_engine/src/main.rs`): reads PVs, targets, MV **OP** (feedback, not SP), modes, DVs, limits, and **live tuning** (weights/alphas) from OPC every scan → `DmcController::next_move` (nalgebra + clarabel QP, `apc_engine/src/dmc.rs`) → writes `{engine}:SolverStatus`, `{cv}:Prediction`, `{mv}:FuturePlan`, and mode-gated SPs. On (re)connect the engine **pushes model tuning to OPC** — OPC tuning values do not persist across engine restarts.
3. **Lifecycle**: frontend → `POST /api/prox/controllers/:id/start` → `controller_host` spawns `apc_engine --model … --opc … --pki … --auth-mode …` (`controller_host/src/main.rs:530-563`); stop is OS `kill`/`taskkill` by PID.
4. **HMI ingest**: `opc_worker` subscription (1s publish/250ms sampling) → `broadcast::Sender<OpcUpdate>` → `/ws` per-socket tasks; browser writes `{type:"WRITE",nodeId,value}` → permission check → `mpsc` `OpcCommand` (`hmi/src/opc_worker.rs`, `hmi/src/web_routes.rs:862-942`).
5. **Deploy**: frontend → `POST /api/deploy` → HMI forwards `nodes_yaml` to OPC hot-reload `:9090/api/nodes` and `model_json` to supervisor `:8080/api/deploy` (`hmi/src/web_routes.rs:760-858`). Hot-reload is **add/update only** — nodes are never removed.
6. **Historian**: second broadcast subscriber → QuestDB ILP, table `process_data`; skips node ids starting `_` or `System:` (`hmi/src/historian.rs`).

### Mode protocol — do not break

- PID `Mode`: 0=Operator, 1=Auto, 2=Cascade, **3=Remote Cascade**. Engine writes SP **only** when actual `Mode==3` **and** `OperatingMode==2`; if `ModeTarget==3` but mode≠3 it writes a bumpless SP = current OP (`apc_engine/src/main.rs:1085-1135`).
- `OperatingMode`: 0=Idle (heartbeat only), 1=Monitor (solve, no SP writes), 2=Engage. Solver failures auto-demote: 2 consecutive → Monitor, 10 → Idle.
- Proposed SP outside HH/LL safety limits is skipped.

### Ports

| Port | Component |
|---|---|
| 4855 | OPC UA endpoint (`opc.tcp://…:4855`) |
| 9090 | OPC server hot-reload/security REST API |
| 8080 | controller_host supervisor API (`x-api-key` auth) |
| 3000 | HMI HTTP + WebSocket (**hardcoded**, `hmi/src/main.rs:131`) |
| 9009 / 9000 | QuestDB ILP write / HTTP query (optional historian) |

## Key Directories

- `apc_engine/src/` — `main.rs` (CLI, reconnect + scan loop), `config.rs` (`UnifiedModel` schema), `dmc.rs` (QP solver), `modelloader.rs` (FOPDT→step curves / explicit coefficients), `opc_interface.rs` (strict bulk read/write helpers), `lib.rs` (re-exports for tests).
- `controller_host/src/main.rs` — whole supervisor: auth middleware, deploy/start/stop, engine discovery (`--engine-bin` → sibling exe → `../apc_engine/target/debug/…`).
- `opcua_server/src/` — `bridges/opcua.rs` (YAML→address space), `bridges/web_api.rs` (hot-reload, security config, `/api/admin/restart` re-execs the process).
- `virtual_plant/src/plant/` — `debutanizer.rs`, `cstr.rs`, `pass_balancing.rs` models.
- `hmi/src/` — `opc_worker.rs`, `web_routes.rs` (~1.9k lines: REST + WS + proxies), `historian.rs`, `auth.rs` (argon2 + sqlx SQLite RBAC), `infrastructure.rs` (remote-host registry seeded from `config/hosts.json`).
- `hmi/frontend/src/` — `App.tsx` (WS lifecycle, CV/MV/DV classification), `store/tagStore.ts` (Zustand), `hooks/useWriteNode.ts`, `lib/api.ts` (CSRF-aware fetch), `components/` (one file per view).
- `Build_scripts/` — `linux/`, `windows/`, `linux_cross_compile/`, `shared/` build + portable-bundle scripts (see Development Commands).
- Docs: `README.md`, `SYSTEM_OVERVIEW.md`, `CONFIGURATION_GUIDE.md`, `apc_engine/APC_ENGINE_OVERVIEW.md`, `apc_engine/DCS_ADAPTER_GUIDE.md`, `hmi/QUESTDB_SETUP.md`. (`roadmap/` is gitignored local notes.)

## Development Commands

**The user runs all terminal commands manually (fish shell). Never execute commands on their behalf — provide exact commands and wait for output.**

Prereqs: Rust 1.70+, Node.js 18+, optional QuestDB for historian.

```bash
# Local run — 4 terminals, in this order, each from its crate dir (configs are CWD-relative!)
cd opcua_server && cargo run
cd virtual_plant && cargo run
cd controller_host && cargo run
cd hmi && cargo run                 # serves frontend/dist on :3000

# Frontend (must be built before hmi serves it)
cd hmi/frontend && npm install && npm run build
npm run dev                         # vite dev server, proxies /api and /ws to :3000

# Full release build (from repo root)
./Build_scripts/linux/build_all.sh
# Windows: powershell -ExecutionPolicy Bypass -File .\Build_scripts\windows\build_all.ps1
# Cross-compile Linux→Windows: Build_scripts/linux_cross_compile/build_windows.sh
# Portable bundle: Build_scripts/linux/copy_portable.sh portable-linux && cd portable-linux && ./start_all.sh
```

- First run of `opcua_server` seeds `config/serversettings.env` from `.sample` (YAML-format ServerConfig despite the `.env` name). Edit IPs there and in `hmi/config/settings.toml`, `virtual_plant/config/settings.toml` before starting.
- `apc_engine` CLI: `--model/-m` (required), `--opc/-o`, `--pki`, `--auth-mode username|x509`, `--security-policy`, `--message-mode`, `--username/--password` — normally injected by controller_host.
- Only env var: `HOST_API_KEY` (controller_host API key, default `secret-deploy-key` — change in prod, likewise bootstrap `admin`/`password`).
- ⚠️ `README.md` references `just build-all` — **no justfile exists**; use `Build_scripts/`.

## Code Conventions & Common Patterns

- **Async**: every binary is `#[tokio::main]` (tokio "1", `features=["full"]`). Actor-style reconnect loops (`loop { connect; inner loop; backoff }`) in engine/plant/hmi worker; `tokio::select!` multiplexes command channels vs timers; broadcast lag tolerated (`RecvError::Lagged(_) => continue`).
- **Errors**: `anyhow::Result` + `.context()` throughout Rust — **no thiserror anywhere**. Axum 0.7 handlers return `(StatusCode, String/Json)`.
- **Logging**: `log`+`env_logger` (engine, plant, server), `tracing` (controller_host); emoji-prefixed `println!` operational logging is the house style — don't introduce a new logging convention.
- **OPC UA**: `async-opcua 0.17`; string NodeIds at **ns=2**, pattern `{Tag}:{Suffix}` (`Reflux:SP`, `T1_Dev:PV`). Engine system nodes: `{engine_id}:OperatingMode|Heartbeat|SolverStatus|ExecutionTimeMs|NextRun|SaveConfiguration|…`. Pseudo-tags never on the wire: `System:*` (backend-synthesized), `_systemPrefix` (frontend).
- **Model identity (fail-fast)**: model JSON `metadata.name` MUST equal the OPC YAML node prefix — engine aborts if `{name}:OperatingMode` is unreadable; controller_host deploy derives the folder from the same name.
- **Model schema back-compat**: `UnifiedModel` (`apc_engine/src/config.rs`) uses `#[serde(default)]` on optional fields — preserve this when extending CV/MV/DV structs (`optimization_mode`, `slack_weight`, `is_integrating`, MV `target`…).
- **Strict reads**: `opc_interface::read_bulk` errors on any bad status/non-numeric variant — one bad node fails the whole read, and the engine treats that as comms loss → reconnect. All engine writes are `Variant::Double` (arrays for `:Prediction`/`:FuturePlan`, bool only for `SaveConfiguration`).
- **Duplicated helper**: `opc_interface.rs` is copy-pasted between `apc_engine` and `virtual_plant` (plant version also accepts Boolean) — fix both or diverge deliberately.
- **Frontend**: Zustand `useTagStore` is the only tag state; components must not subscribe the whole `tags` map (re-render storm — use selectors/`getState()`). Write-ack = next update for that node clears `pendingWrites`. CV/MV/DV classification depends on YAML containing `:Prediction`/`:FuturePlan` nodes. Mutations send `x-csrf-token`; role gating via `hasRoleAtLeast` (min `operator` to write). Styling is Tailwind CSS **v4** (CSS-first, no tailwind.config) + `index.css`; `App.css` is dead Vite scaffold. Charts: recharts; icons: lucide-react.
- **Config files are CWD-relative** — always run binaries from their crate root. Secrets (`**/pki/**`, `**/users/**`, `serversettings.env`, `hmi.db`, `*_saved_*_model.json`) are gitignored.
- **Security defaults** (change in prod): `admin`/`password`, `secret-deploy-key`, `trust_server_certs(true)`; HMI cookies are `Secure` + `SameSite::Strict` — plain HTTP deployments break login (front with HTTPS in prod).

## Important Files

| Path | Why |
|---|---|
| `apc_engine/src/config.rs` | `UnifiedModel` model JSON schema (node ids, tuning, physics) |
| `apc_engine/src/dmc.rs` | DMC QP solver (nalgebra + clarabel) |
| `controller_host/src/main.rs` | Supervisor API, engine spawn/kill, deploy |
| `opcua_server/src/bridges/web_api.rs` | Hot-reload + security endpoints |
| `hmi/src/web_routes.rs` | All REST/WS routes incl. proxies and `/api/deploy` |
| `hmi/src/opc_worker.rs` | OPC subscription → broadcast → WS bridge |
| `hmi/frontend/src/App.tsx` + `store/tagStore.ts` | Frontend data flow core |
| `hmi/config/settings.toml` | `[services]` proxy targets, `[historian]` QuestDB, `[hmi_auth]` sqlite |
| `hmi/config/hosts.json` | Infrastructure registry seed (supervisors, OPC servers) |
| `opcua_server/config/serversettings.env.sample` | Server config template (YAML), user tokens |
| `opcua_server/models/*.yaml` | OPC node definitions per plant |
| `controller_host/models/<ControllerId>/*.json` | Deployed controller models |
| `apc_engine/config/model.json`, `VP_Debutanizer.json` | Sample `UnifiedModel` JSONs |

## Runtime/Tooling Preferences

- **Rust**: edition 2021, no toolchain pin/MSRV file. Key deps: `async-opcua 0.17`, `tokio 1`, `axum 0.7`, `serde`, `nalgebra 0.32` + `clarabel 0.11.1` (engine), `sqlx 0.8`/`argon2`/`questdb-rs`/`tower-sessions` (hmi). Known version skew: `tower-http` 0.6 vs 0.5 (hmi), `toml` 0.8 vs 0.9 — don't "fix" casually.
- **Frontend**: npm only (`package-lock.json`, lockfileVersion 3 — no bun/pnpm/yarn). React 19, TypeScript ~5.9 (strict), Vite aliased to `npm:rolldown-vite@7.2.5` via `overrides`, Tailwind v4, Zustand 5. Scripts: `dev` / `build` (`tsc -b && vite build`) / `lint` / `preview` — **no test script**.
- **Shells**: user runs fish; scripts exist for bash (`Build_scripts/linux`) and PowerShell (`Build_scripts/windows`).
- **No CI**: `.github/` has no workflows — nothing auto-enforces tests/lint.
- **cargo-deny**: `deny.toml` (licenses only) in every crate root; hmi's adds a `webpki-roots` exception. New deps must use allowed licenses (Apache-2.0/MIT/BSD/ISC/MPL-2.0/Zlib/Unicode-3.0).
- Dead weight to not copy: `rust-embed` dep in `hmi` (unused; serves `frontend/dist` from disk), `virtual_plant/models/` (legacy, unreferenced), `approx` dev-dep (unused), `parse_legacy_stepresponse` (dead code).

## Testing & QA

- **Only `apc_engine` has a real test suite.** Integration tests in `apc_engine/tests/` import the lib as `authrity_apc_engine::…` (package name, via `src/lib.rs`):

```bash
cd apc_engine && cargo test                          # all
cargo test dmc                                       # name filter
cargo test --test dmc_basic_tests                    # one file
```

  Suites: `dmc_basic_tests.rs` (14: solver init, MIMO coupling, DV feedforward, infeasibility, CV/MV optimization modes), `constraint_tests.rs` (MV limit respect), `integration_tests.rs` (closed-loop vs inline `FopdtPlant`: tracking, disturbance rejection, 100+ cycle stability), `modelloader_tests.rs` (12: parametric/step_response validation, dimension mismatches). Shared builders in `tests/test_helpers.rs` (`create_simple_1x1_model`, `create_2x2_model`, `create_model_with_dv`). All synchronous `#[test]` — **no `#[tokio::test]` in the repo**. ⚠️ `apc_engine/tests/README.md` examples are stale (`apc_engine::…`, `create_1x1_model`) — trust the code.
- **hmi backend**: unit tests only, in `hmi/src/auth.rs` (`#[cfg(test)]` — write-classification and role gates). Run: `cd hmi && cargo test`.
- **No tests at all**: `controller_host`, `opcua_server`, `virtual_plant`, `hmi/frontend` (no runner configured). `opcua_server/validation/test_payload.yaml` is a manual hot-reload curl payload, not an automated test.
- **Lint**: `cd hmi/frontend && npm run lint` (ESLint flat config, ts/tsx). No rustfmt/clippy config files — follow surrounding style.
- **Coverage**: no tooling configured and no coverage expectations. No CI gates.
- When changing behavior: mirror existing test style (sync `#[test]`, shared builders via `mod test_helpers;`), and verify with the run order above — a full smoke is opcua_server + virtual_plant + controller_host + hmi, deploy a model, start the controller, watch `{engine}:SolverStatus` in the HMI.

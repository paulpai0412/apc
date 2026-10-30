# Historical data, local closed-loop research, and tuning

These tools import and clean local process history, fit observational model candidates, and evaluate actual MPC feedback against an independent frozen simulated plant. `research.py` adds adaptive, result-dependent search and standard DMC / Soft-band comparison in the original React HMI. Its explicitly configured native COM DA path writes only to the local simulator. No tool automatically deploys candidates or connects to production.

**The included fixture is synthetic software-test data, not cement mill data, kiln data, or evidence of plant fidelity.** A real cement-mill dataset remains a prerequisite until its identity, chronology, variables, provenance, and license have been verified. See [dataset evidence](DATASETS.md) and the [system design](../DIGITAL_TWIN_VALIDATION.md).

## Requirements and boundaries

- Python 3 with NumPy `>=1.24,<3` for identification, model qualification and research evaluation (`requirements.txt`). Import and fixture generation use the standard library.
- A user-built `mpc_validate` Rust binary for free-run and actual closed-loop evaluation.
- The repository defaults to user-run fish commands. The user explicitly authorized assistant-run isolated local fish verification for this five-round acceptance; the commands below remain reproducible instructions, not evidence of success by themselves.
- No production credentials, deployment, or production endpoints are required.
- The native local DA integration is in `opc_da_simulator/`; Python orchestrates its actual bridge through Rust. ABB-specific mode/units, network DCOM, and real plant authorization remain separate requirements.

## Canonical history

`examples/history.schema.json` describes the interchange structure:

```json
{
  "sample_time": 1.0,
  "time_seconds": [0.0, 1.0],
  "cv": [[0.0], [0.0]],
  "mv": [[0.0], [0.5]],
  "dv": [[], []],
  "provenance": {"source_url": "file://explicit-export.csv", "license": "unknown", "synthetic": false}
}
```

Rows are samples; columns follow `model.variables.cvs`, `mvs`, and `dvs` order. All groups have the same number of rows, including empty DV rows when no DVs exist. Values must be finite; timestamps must increase uniformly by the positive `sample_time`, which must match the model. At least two samples are required. Initial time need not be zero. The runtime enforces cross-field constraints not expressible in the JSON schema.

**Initial condition:** the first row is assumed to be a steady operating point and input levels before it equal the first-row levels. This must be justified for real data; the importer cannot infer it. A new closed-loop scenario is a separate experiment initialized from its first row, not an arbitrary continuation of an unrecorded process state.

## Local CSV import

`examples/columns.json` is a complete synthetic mapping example. Supply an explicit mapping; no dataset is selected or fetched automatically:

```json
{
  "timestamp": {"column": "timestamp", "format": "iso8601"},
  "sample_time": 10,
  "encoding": "gbk",
  "delimiter": ",",
  "cv": ["quality"],
  "mv": ["feed"],
  "dv": [],
  "provenance": {"source_url": "file://approved-export.csv", "license": "unknown", "synthetic": false}
}
```

`timestamp.format` is `seconds` for numeric seconds or `iso8601` for timezone-qualified timestamps. ISO times become elapsed seconds relative to the first record, with `time_origin_iso8601` preserved in provenance; this avoids losing subsecond precision in floating-point Unix timestamps. `encoding` is configurable, including GBK. Each mapped column must be distinct and columns must be listed in model order. The importer rejects missing/nonfinite measurements, duplicate headers/timestamps, blank records, inconsistent field counts, nonuniform spacing, and gaps. It does not sort, interpolate, or drop records. Provenance requires explicit `source_url` and `license`; `unknown` records missing license verification rather than granting permission. Output records the input SHA-256 and mapped columns.

```fish
python3 validation/import_history.py --csv approved-export.csv --mapping columns.json --model baseline.json --output history.json
```

## Saved QuestDB export or explicit loopback export

`export_questdb.py` can read a saved QuestDB `/exec` JSON response offline, or query an explicitly supplied loopback URL. Its mapping is **different from the CSV mapping**: entries are historian tag/NodeId strings, not CSV headers.

```json
{"sample_time": 10, "cv": ["CV:PV"], "mv": ["MV:AppliedSP"], "dv": []}
```

Order must match the model. The saved response must contain `columns` describing `timestamp`, `node_id`, `value`, and `status`, plus a `dataset` row array. `node_id` is the exact configured OPC identifier: case, punctuation, commas and prefixes are significant. The exporter rejects ambiguous legacy `tag`/`field` rows rather than reconstructing an identity that may already have collided. Use a new historian table when migrating the old schema; do not relabel old rows as verified exact-node data. The exporter rejects nonzero status, duplicate timestamp/node observations, stale samples, and truncated results. It uses causal zero-order hold (the most recent earlier observation), bounded by explicit `--max-age`; it does not look ahead or linearly interpolate. This explicit asynchronous-export policy differs from the strict already-gridded CSV importer.

The MV history must use the accepted lower-PID setpoint in the controller's engineering units. A valve OP percentage or delayed actuator realization is not interchangeable with this model input, even when both are available in the historian.

The current HMI historian stores valid **OPC UA source timestamps** and the exact source endpoint; it does not substitute receipt time for missing or unset source clocks. Legacy receipt-timestamp rows are not source-time evidence. Clock alignment, provenance, configured deadband and resulting coverage still require assessment before treating exported records as physical event chronology.

```fish
python3 validation/export_questdb.py --response saved-exec.json --mapping questdb-map.json --start 2025-01-01T00:00:00Z --end 2025-01-01T01:00:00Z --max-age 20 --output new-history.json
```

Optional explicit local access, only when the user intends it:

```fish
python3 validation/export_questdb.py --url http://127.0.0.1:9000 --mapping questdb-map.json --start 2025-01-01T00:00:00Z --end 2025-01-01T01:00:00Z --max-age 20 --output new-history.json --table process_data --max-rows 1000000
```

No automatic live database access occurs when using `--response`. Export alone does not establish correct variable selection, consistent engineering units, causality, or a steady initial condition.

## Identification and evidence limits

`identify.py` takes a baseline UnifiedModel and canonical history. It splits rows chronologically into train/validation/test, defaulting to 60%/20%/20%, with no random shuffle. Only training CVs determine coefficients. Validation error chooses a finite memory/ridge candidate. The final test interval is reported only after selection, without refitting. Input history before a split may initialize its causal response; later recorded CVs are never fed into model predictions.

The model is a causal FIR on input levels relative to the initial operating point:

`y[t] - y[0] = sum(h[lag] * (u[t-lag] - u[0]), lag=1..memory)`

Here `u` contains both MVs and DVs. The emitted step coefficients satisfy `g[0]=0`, `g[k]=sum(h[1..k])`, and retain their settled value after memory. Thus input `u[t]` first changes output at `t+1`; an instantaneous controller algebraic loop is not introduced. Full CV-by-MV and CV-by-DV matrices are emitted at the prediction horizon.

Identification rejects integrating flags, constant/unexcited inputs, rank-deficient/collinear lag matrices, insufficient training rows, excessive conditioning, out-of-bound coefficients, and an unsettled impulse tail. It assumes stable linear finite-memory behavior. The tail diagnostic **does not prove physical settling or exclude every nonstationary/nonlinear process**; a candidate must still pass independent plant/data validation. Observational prediction does not prove causal control authority.

Default search: memories `6 8 12`, ridge strengths `0 0.001 0.1`. There are at most 100 combinations; memory is limited to 256 samples and must leave a settled plateau in the prediction horizon. CLI options expose train/validation fractions, maximum condition, tail fraction, and coefficient bound. The report includes candidates, rejection reasons, selected parameters, split indices, untouched test RMSE/MAE, one-step persistence, frozen-origin persistence, assumptions, and input hashes. Safety limits, rates, tags, tuning, and economic objective configuration are preserved; only model metadata/physics are changed. Artifacts are marked **candidate only**.

## Closed-loop tuning

`mpc_validate validate` is free-run historical prediction, **not closed-loop validation**. `mpc_validate closed-loop` invokes the actual DMC controller in feedback with the plant twin and replays DVs. Plant and controller are separate files; names, units, order, and sample times must agree. Controller commands are held through the next plant sample. Later recorded CVs must not drive the closed-loop plant output.

Controllers with DVs must use explicit `step_response` coefficients: the existing production entrypoint does not load parametric DV coefficients. The offline evaluator rejects that combination rather than silently evaluating a controller stronger than production. The independent plant may still use parametric MV/DV curves.

`tune.py` calls this real binary with a subprocess argument list and `shell=False`. It snapshots the plant and both scenarios into a new output directory. It never changes the plant while tuning. It changes only CV `weight`, CV `alpha`, and MV `weight_r`, with bounds recorded in the report; it does not tune limits, maximum moves, tags, slack/economic objective weights, or dynamics. Search starts with the baseline and tries deterministic coordinate candidates, capped by `--max-trials` (default 32; maximum 128).

Selection uses the engine's fixed normalized score, not the candidate's changed tuning weights. Every trial retains the full parameters, invocation, metrics/trace, process output, and failure record. Nonzero process exit, solver failure, timeout, or invalid metrics makes a trial invalid, not a successful zero-move trial. The chosen controller is fixed **before** held-out baseline/candidate evaluation; held-out results do not select or refit candidates. Selection and held-out scenarios must differ, but the tool cannot prove real-world statistical independence: the user must provide a genuinely reserved experiment/window, avoid overlapping causal history, and not iteratively retune on its results.

The output directory must not exist, preventing stale result reuse. The run retains `controller-NNN.json`, `trial-NNN.json`, engine results, frozen inputs, `candidate.json`, and `report.json`. Failure trials remain in the report. Held-out failure exits unsuccessfully and marks the retained candidate unvalidated. Files are ordinary local artifacts, not a tamper-proof audit store; preserve them and their input hashes when comparing experiments. No tuning step deploys anything.

## Complete synthetic smoke path (fish)

Use an existing environment with NumPy, or explicitly prepare one yourself:

```fish
python3 -m venv /tmp/authrity-validation-venv
source /tmp/authrity-validation-venv/bin/activate.fish
python3 -m pip install -r validation/requirements.txt
```

From the repository root, the following uses fresh temporary directories and an isolated Rust target directory:

```fish
set fixture (mktemp -d)
set build_dir (mktemp -d)
cargo build --manifest-path apc_engine/Cargo.toml --bins --target-dir $build_dir
cargo test --manifest-path apc_engine/Cargo.toml --target-dir $build_dir
set validator $build_dir/debug/mpc_validate

python3 validation/generate_fixture.py --output-dir $fixture
python3 validation/import_history.py --csv $fixture/history.csv --mapping $fixture/columns.json --model $fixture/baseline.json --output $fixture/history.json
python3 validation/export_questdb.py --response $fixture/questdb-response.json --mapping $fixture/questdb-mapping.json --start 2024-01-01T00:00:00Z --end 2024-01-01T00:10:00Z --max-age 0 --output $fixture/exported-history.json
python3 validation/identify.py --model $fixture/baseline.json --history $fixture/history.json --output $fixture/identified.json --report $fixture/identification-report.json
$validator validate --plant $fixture/identified.json --history $fixture/history.json --output $fixture/free-run.json
$validator closed-loop --plant $fixture/plant.json --controller $fixture/identified.json --history $fixture/selection.json --output $fixture/closed-loop.json
python3 validation/tune.py --binary $validator --plant $fixture/plant.json --controller $fixture/identified.json --selection-history $fixture/selection.json --heldout-history $fixture/heldout.json --output-dir $fixture/tuning

env MPC_VALIDATE_BIN=$validator python3 -m unittest discover -s validation -p 'test_*.py'
```

`generate_fixture.py` creates a known synthetic plant, a deliberately mismatched baseline controller, excitation CSV, mapping, separate selection/held-out synthetic scenarios, and a matching saved QuestDB response/mapping. The saved response spans 600 one-second samples from `2024-01-01T00:00:00Z`; the exporter command uses the exclusive end `00:10:00Z` and zero permitted staleness. Its output has the same numeric arrays as the CSV import, with export-specific provenance. The known plant is frozen throughout tuning; fitting its synthetic history is a software consistency test, not independent real-plant fidelity evidence.

The test command runs importer/identification/tuning-bound and exporter tests plus actual Rust-engine cases with no mock engine. Without `MPC_VALIDATE_BIN`, real-engine cases are explicitly skipped. Intended assertions, **not claimed execution results**, include:

- Synthetic MV response `[0, 0.5, 0.8, 1, 1, ...]` and DV response `[0, -0.2, -0.3, -0.4, -0.4, ...]` recovered within `1e-7`.
- Rust free-run CV RMSE below `1e-7`, agreeing with Python coefficient timing.
- Modifying held-out identification CVs cannot change the selected model.
- Rejection of missing/duplicate/gapped/nonfinite history and unidentifiable/unsettled fits.
- Only permitted tuning fields change; successful closed-loop results report zero solver failures.
- Real-engine rejected trials remain invalid records rather than silently becoming successful trials.
- The generated QuestDB response round-trips to the same canonical numeric history with `max_age=0`.

None of these synthetic invariants establishes readiness for cement-mill operation, online optimization, or ABB/OPC DA integration.

## Isolated OPC UA adapter

`replay_plant` is a separate Rust binary, not a Python tool and not an OPC DA server.
Its actual OPC session requires a user-provisioned isolated simulator server,
pre-created model nodes, the correct namespace index/URI, and mutually trusted
X509 application/user certificates. The repository server's hot-reload API
binds to all interfaces; use an isolated VM/network. A loopback client URL alone
does not prove the server is not a production gateway.

Inspect the complete CLI without connecting:

```fish
$build_dir/debug/replay_plant --help
```

For a provisioned test server, supply all of these options:

| Option | Required value |
|---|---|
| `--plant`, `--history` | Frozen model and canonical scenario files |
| `--opc` | Exact advertised `opc.tcp://` URL with literal loopback IP and port |
| `--namespace-index`, `--namespace-uri` | Matching entry from Server NamespaceArray |
| `--pki`, `--certificate`, `--private-key` | Dedicated isolated-client trust store, DER certificate and PEM key |
| `--acknowledge-isolated-simulator` | Confirms all nodes are simulation-only, not forwarded to ABB |
| `--acknowledge-ideal-actuator` | Confirms SP/PV/OP are the same engineering coordinate with ideal tracking |
| `--commissioning-profile` | Required single-profile JSON, identical to the native authority's approved physical bindings, modes, application feedback and watchdog contract |
| `--regime-config` | Required when the profile declares performance monitoring: explicit synthetic regime tags and independent sampled copies of operator settings |

The adapter pins the endpoint URL, requires Basic256Sha256/SignAndEncrypt,
checks namespace URI and per-item status/counts, and fails on malformed reads,
rejected writes, invalid MV commands or sample-clock overruns.
The physical simulation clock runs while the native controller is in Monitor;
it does not wait for an operator to write an `OperatingMode` node. The producer
owns actual actuator modes, applied-SP feedback, release acknowledgments and the
independent authority watchdog. It does not own the engine's requested modes.
Use the commissioned Runtime lifecycle to start Monitor, accumulate real history,
then explicitly Arm and Engage. Exit/Stop requires the native release handshake;
neither a queued write nor a shared SP-register readback proves DCS takeover.
The isolated fixture keeps its physical Ts=1s and the quarter-derived controller's
Ts=60s separate. It does not shorten physical time constants or fabricate warmup.

`trace.mv[t+1]` in offline results is the command held over `[t,t+1]`, while the
historical input format records inputs at each timestamp. Do not feed a
closed-loop result trace into free-run validation without accounting for that
different command alignment. IAE and violation duration use right endpoints;
the score includes a fixed 0.01 normalized movement penalty.

The small offline fixtures do not claim OPC UA, DA/DCOM, or ABB plant validation.
The full campaign below separately exercises the native local DA transport;
remote DCOM and authorized ABB commissioning remain distinct gates.

## Full calendar-quarter synthetic campaign

`campaign.py` runs the native campaign backend; the original React modeling
workspace exposes its research, comparison and digital-twin results. For the
integrated M1–M9 / R1–R7 acceptance launcher and post-test narrated video contract,
see `CEMENT_MILL_IMPLEMENTATION.md`. This is separate from the small fixtures above. The checked example
`validation/examples/cement_campaign.json` requests **2026-01-01 UTC through
2026-04-01 UTC (exclusive), at 60 seconds: 129,600 actual scans**. Calendar month
addition, not a fixed 90-day constant, derives all boundaries. Changing the start
to a leap-year quarter changes the sample count accordingly.

The generator is explicitly **synthetic cement-grinding-like**, not a fitted
industrial plant. Fineness (m²/kg) and differential pressure (mbar) respond to fresh
feed (t/h), separator speed (rpm), clinker hardness (kWh/t), and feed moisture (%).
Every numerical truth parameter is in the example configuration: second-order
paths, delays, actuator bounds/lag/rate (engineering units per second), mild
nonlinearity and gain variation. Seeded independent streams generate PRBS/ramp
excitation, operating regimes, smooth measured disturbances, colored unmeasured
process noise, and independent sensor noise. Rust generates every historical CV;
Python does not substitute a truth-coefficient controller.

Build `mpc_campaign` and the native COM bridge using their component instructions.
Python requires only the existing NumPy dependency (`validation/requirements.txt`);
there are no GPU, CUDA, SciPy, or machine-learning-framework requirements. From
the repository root, after setting a dedicated local Wine prefix and actual
bridge executable path:

```fish
set -lx WINEPREFIX /absolute/path/to/isolated-wine-prefix
set -lx OPENBLAS_NUM_THREADS 2
python validation/campaign.py \
    --config validation/examples/cement_campaign.json \
    --binary target/release/mpc_campaign \
    --output-root validation/runs \
    --bridge /absolute/path/to/wine \
    --bridge-arg /absolute/path/to/opc_da_bridge.exe \
    --stage all
```

The Wine launcher and executable are configurable; arguments are passed directly
without a shell. Repeat `--bridge-arg` for each argument (use
`--bridge-arg=VALUE` when VALUE begins with a hyphen). Use only the isolated local
simulator; the runner does not connect to or deploy to a production plant.

For staged execution, choose `--stage prepare`, `history`, `identify`, `tune`,
`evaluate`, `transport`, or `report`, in that order; every invocation after the
first needs `--resume`. A subsequent `--stage all --resume` reuses completed stages
only after checking configuration, source, executable, input, and output hashes.
Unreceipted partial files or changed hashes are errors, not evidence of completed
work. Preserve them for diagnosis and choose a new output root/id for changed
experiments. Do not overwrite old traces or change the configuration mid-campaign.

The stages are:

1. Freeze every exogenous signal for the entire three-calendar-month timeline.
2. Invoke the real Rust `replay` to produce historical actual actuators and CVs.
3. Fit FIR candidates on the first month and choose memory/ridge on the second.
   Estimation uses recorded **command MV**, including actuator dynamics in the
   identified response; raw history retains separate actual MV and command MV.
   Engineering templates contain no truth path gains/time constants/delays.
   Physical excitation, rank/condition, finite coefficients, and settled-tail
   diagnostics are required. Full and held-out RMSE are compared with persistence,
   whose previous-CV information is explicitly different from FIR input history.
4. Evaluate baseline plus the configured finite tuning candidates on **all scans
   of the second month**, from the same physical baseline, using actual Rust MPC.
   The baseline is eligible to win. Never select on the third month.
5. Run baseline and chosen tuning continuously over **all three months**. Report
   full-quarter and untouched third-month metrics separately.
6. Repeat the complete chosen-controller trajectory through the native COM DA
   bridge; compare every CV/MV/DV/target/command/timestamp at configured tolerances.
   Execute actual bad-quality, disconnect, and denied-write injections, preserving
   their rejected traces. Recovery is a **fresh bridge/controller process**, not
   a claim of transparent reconnect.
7. Build `validation/runs/<id>/report.json`, per-directory `index.json`, raw JSON
   traces, process return-code/log receipts, and SHA-256 artifact inventory. Point
   the HMI simulation-report root at `validation/runs`.

The fixed comparison score is the sum of mean squared CV error divided by CV
range, mean squared outside-limit excursion divided by CV range, and mean squared
command movement divided by MV range. Tuning weights do not enter that score.
IAE is engineering units·seconds; violation duration is seconds; movement is
engineering units; hard-violation counts include command/actual range violations
and command move-limit violations. All control metrics use every scan; only FIR
metrics exclude the initial selected-memory rows. Chart downsampling happens in
the HMI after simulation and never replaces control scans or metric computation.

`report.status` is `passed` only after actual process completion, full timelines,
finite numerical traces, solver and actuator checks, artifact integrity, native
COM equivalence, fault detection, and restart recovery all pass. Missing work is
`incomplete`, never fake success; failures remain visible. A successful numerical
campaign does not imply improvement, causality certification, remote DCOM/ABB
compatibility, industrial safety, or production readiness.

Deterministic unit/invariant checks (no runtime/DA mocks) are available with:

```fish
python -m unittest discover -s validation -p 'test_*.py'
```

### Archived campaign artifacts

The old report page and links are no longer rendered. Existing campaign manifests and listed artifact downloads remain available through the authenticated `/api/simulation-reports` API with `ReadView` authorization. New interactive research uses the original React **Autoresearch** workspace described below; it does not pretend that a legacy campaign manifest is a new research run.

Add the following sections to a complete HMI settings file. These relative paths
assume the backend is launched from `hmi/`:

```toml
[reports]
directory = "../validation/runs"

[web]
bind_address = "127.0.0.1:3000"
assets_directory = "frontend/dist"
```

`AUTHRITY_HMI_CONFIG` selects an alternative complete settings file; otherwise
the existing `config/settings.toml` default remains. For isolated report-only
operation, set `connect_opcua = false` in the existing `[runtime]` section,
`enabled = false` in `[historian]`, and use a new SQLite database plus isolated
infrastructure endpoints. Do not copy production credentials or reuse a plant
configuration merely to view a report. The normal first-login password update
and session authorization remain enforced.

Legacy manifests retain actual completion/failure state, full-resolution traces,
model fitting, tuning trials, source hashes and safety checks. The retained API
does not manufacture a demo dataset when files are absent. Current research
results expose per-channel engineering scales, real forecasts and full-period
metrics through the research workspace; display reduction never replaces
control scans or metric computation.

The legacy finite-response twin retains the complete supplied response memory,
independently of the controller prediction horizon. Short settled paths hold
their final gain; longer paths are not silently truncated to the prediction
window. Command SP and actual actuator traces remain distinct in the campaign.



## Original React Autoresearch workspace

The standalone **Simulation Reports** sidebar entry and archived-report links are removed from the original React. Existing report artifacts and read APIs remain for traceability. **Autoresearch** owns data preparation, research results and explicit immutable-model selection; research settings never write live tuning tags.

1. **匯入資料:** upload CSV or canonical history JSON, select an immutable original dataset, or read a configured PostgreSQL source over an explicit half-open UTC interval.
2. **清理與預覽:** select timestamps, CV/MV/DV columns and engineering units. Configure sorting/deduplication, explicit DA/UA/boolean quality encoding, missing/outlier treatment, resampling, and split boundaries. Preview original/cleaned curves and quality counts; focus the first numeric cleaning difference, including non-extreme changes retained during display reduction.
3. **儲存清理資料並用於研究:** materialize a new full-resolution dataset. Unresolved long gaps remain invalid and prevent research use. This is actual cleaning, not just a log of recommended actions.
4. **研究設計:** select the cleaned dataset or an explicitly generated synthetic history; configure engineering values, methods, quality bands, adaptive rounds, budget, and parameter bounds. Saving creates a new immutable study version.
5. **啟動本機研究:** the backend launches a persistent local worker. Closing the browser does not cancel it. Stage and completed-task counts reflect actual operations, not estimated time.
6. **結果與模型選用:** compare physical CV variance, tracking bias, violation gates and MV movement first; inspect candidate ancestry and untouched test-month traces when needed. Select a qualified immutable version explicitly. Installation into a configured supervisor is a separate confirmed action; it never starts control. Cancellation stops the worker's controller process and retains partial evidence, not a passing result.

### Dataset semantics

`datasets.py` is the new interactive cleaning interface; the earlier strict `import_history.py` remains a separate CLI and still rejects imperfect grids. Raw uploaded bytes are immutable. Cleaned datasets have a new ID, source hash, normalized policy, `history.json`, `cleaning.json`, and display preview. New clean dataset summaries expose the saved normalized signal mappings as `suggested_signals`, including exact source columns, names, roles, units, conversions, and quality columns/encodings, for the read-only engineering source table. Later cleaning edits create another dataset; legacy summaries without these mappings remain unknown rather than inferring source tags.

Unit conversion is `engineering = source * scale + offset`. A configured quality column uses its explicit encoding: DA Good (`quality & 0xC0 == 0xC0`), UA Good severity (`status & 0xC0000000 == 0`), or boolean true/t/1. These encodings are not interchangeable; the backward-compatible default is DA. Training-sigma bounds use training samples only. Linear interpolation stays within each chronological split; forward fill is causal. Maximum gap is elapsed time, not a row count. Missing values are never concealed by dropping time slots.

The research study must match cleaned signal names/order/units, sample time, three-calendar-month length, and cleaning split boundaries. The current identification assumes a steady initial point and finite stable response. Map the input coordinate used by the model: in the provided local campaign this is commanded MV, not physical actuator OP.

### Method contract

The optional top-level `UnifiedModel.control_strategy` defaults to standard DMC:

```json
{"method":"standard_dmc","version":1,"bands":[]}
```

For Soft-band, provide one band per CV:

```json
{"method":"soft_band","version":1,"bands":[
  {"half_width":1.5,"tracking_scale":0.05,"quadratic_penalty":1.0},
  {"half_width":0.6,"tracking_scale":0.05,"quadratic_penalty":1.0}
]}
```

These are example tuning values, not constants imported from the thesis. `half_width` uses each CV's engineering units. Existing target tracking is scaled; independent quality slack penalizes excursions outside the target-centered band. Existing operating-limit slack and hard MV/rate bounds remain. Target modes are required for this strategy; unknown methods/versions or invalid combinations fail instead of falling back. No MHE, automatic 30-minute delay, or live hot-switching is implied.

`research.py` selects methods independently. Each round proposes two novel bounded coordinate candidates around the best earlier result; unsuccessful rounds contract the step. Budget must cover `method_count * (1 + 2 * rounds)`, including each baseline. Every candidate runs the entire selection month. All method winners are frozen before final-quarter/test evaluation. Quality limits, rate limits, simulator parameters, and scoring are not search variables. This is bounded adaptive numerical research, not an LLM that edits controller source.

### Isolated HMI configuration

`[research]` defaults to disabled. Enabling requires `runtime.connect_opcua=false` and `historian.enabled=false` in that HMI instance. Configure absolute administrator-owned paths:

```toml
[research]
enabled = true
directory = "/srv/authrity/local-research"
python = "/srv/authrity/venv/bin/python"
script = "/srv/authrity/AuTHRity-APC/validation/research.py"
binary = "/srv/authrity/target/release/mpc_campaign"
bridge = "/usr/bin/wine"
bridge_args = ["/srv/authrity/AuTHRity-APC/opc_da_simulator/build/opc_da_bridge.exe"]
command_timeout_seconds = 120
```

Paths above are deployment examples; use actual installed paths. On native Windows, configure the bridge executable directly and no Wine argument. On Linux, use the dedicated Wine prefix in the HMI service environment. This feature does not create or authorize a production connection.

Administrators can add bounded PostgreSQL sources:

```toml
[[research.postgres_sources]]
id = "cement-kiln"
name = "水泥窯歷史資料"
description = "Approved read-only process history"
dsn_env = "AUTHRITY_RESEARCH_PG_DSN"
schema = "public"
table = "kiln_history"
timestamp_column = "recorded_at"
columns = ["recorded_at", "kiln_temperature", "fuel_feed", "quality"]
max_rows = 100000
statement_timeout_seconds = 30
```

Set the named environment variable on the HMI service using a dedicated read-only database account and the required PostgreSQL TLS settings. The browser receives source labels/columns/row caps, never the DSN or environment-variable name. It cannot submit SQL, database credentials, or relation names. The timestamp must be `timestamptz`; numeric, boolean, and text columns are retained without rounding or prefiltering quality. Inputs require explicit timezones; the UI uses UTC. Import executes one repeatable-read, read-only transaction. Row, cell, byte, and time limits reject the entire import instead of silently truncating. Defaults are 32 MiB, 8 million cells, and at most 500,000 rows, subject to the lower per-source cap.

Raw metadata records the configured source ID, relation, ordered columns, UTC window/read time, and row count. Cleaned history retains that provenance and the raw-content hash. This is a frozen snapshot, not a live historian subscription. PostgreSQL views still obey transaction read-only enforcement, including functions attempting writes.

Research endpoints use the existing session, CSRF checks, audit, and engineer/admin permission:

| Endpoint | Action |
|---|---|
| `GET /api/research` | Capabilities, defaults, datasets, studies, and runs |
| `GET /api/research/sources` | Discover administrator-configured PostgreSQL sources without credentials |
| `POST /api/research/import-postgres` | Import a bounded read-only snapshot by source ID and UTC window |
| `POST /api/research/import` | Persist original bytes |
| `POST /api/research/preview` | Execute cleaning and return actual before/after data |
| `POST /api/research/clean` | Materialize a new cleaned dataset |
| `POST /api/research/studies` | Save immutable study version |
| `POST /api/research/runs` | Start local worker using a study ID |
| `GET /api/research/runs/:id` | Persisted progress, candidates, results, errors |
| `POST /api/research/runs/:id/cancel` | Request cancellation |
| `GET /api/research/runs/:id/series?method=...&variant=...&lead_steps=1` | Independent-engineering-axis DTO; optional relative-second `start`/`end` window |
| `GET /api/research/runs/:id/forecast?method=...&variant=...&at=...` | Recorded full horizon issued at/before the relative-second anchor |
| `GET /api/research/versions` | Browse verified immutable control versions, including from a research-disabled live HMI |
| `POST /api/research/versions` | Explicitly select and freeze a qualified completed candidate |
| `GET /api/research/versions/:id` | Verified version metadata and qualification evidence |
| `GET /api/research/versions/:id/model` | Exact frozen model bytes, not a regenerated JSON model |
| `GET /api/control/installation-target` | Read the administrator-configured supervisor target |
| `POST /api/control/versions/:id/install` | Explicit target-bound installation; verify returned artifact identity; never auto-start |
| `POST /api/runtime/write` | Authenticated, CSRF-protected live scalar write with checked OPC item acceptance; not PID application proof |

Imports are limited to a 32 MiB HTTP JSON request. Executable paths, filesystem roots, and bridge arguments cannot be supplied by browser requests. One active run per local root; interrupted workers are marked failed rather than silently replayed. A cancelled/failed trial is not evidence of controller success.

### Complete acceptance entrypoint

Build the Rust binaries, install `requirements.txt` in an isolated Python environment, and build/register the native DA simulator as documented in `../opc_da_simulator/README.md`. Write an administrator-owned runtime JSON containing absolute `binary`, `bridge`, `bridge_args`, and `local_only:true`; this has the same executable meaning as the HMI configuration.

```fish
python validation/verify_autoresearch.py --runtime-file local-runtime.json
```

Alternatively, set `AUTHRITY_RESEARCH_RUNTIME` to that JSON and run the agreed entrypoint:

```fish
python validation/verify_autoresearch.py
```

The command generates a full three-month Rust history, introduces explicit source-data defects to exercise actual CSV cleaning, materializes the repaired dataset, and uses it for real identification and adaptive research. Both methods then execute full-quarter direct and native COM closed loops. It verifies zero hard MV/rate violations and solver failures, actual per-scan DA calls, method-dependent control, candidate ancestry, month isolation, and artifact hashes. It returns zero only when these machine checks pass; it does not require Soft-band to beat standard DMC.

Each attempt uses a fresh directory; the command prints its root and run ID. Recheck immutable evidence without repeating the quarter:

```fish
python validation/verify_autoresearch.py --root /absolute/printed/attempt-root --check-run RUN_ID
```

Changed engine/source hashes require a fresh experiment. `acceptance.json` records the checks. Original React desktop/mobile browser interaction is an additional required verification, not something this numeric command pretends to perform. Point the isolated HMI research directory to the desired attempt root to inspect its real study, datasets, and run.

Frozen evidence must be checked against its original executable and the source files recorded for that run. Do not rebuild over the old executable first. For the historical Round 2 run in this checkout, the captured source directory is from after candidate selection, **not a startup snapshot**. Use module execution so that the archive wins import resolution:

```fish
cd /home/timmypai/apps/AuTHRity-APC
env PYTHONPATH="$PWD/validation/verified-runs/product-five-rounds/round2/identified-history-sources:$PWD/validation" \
    OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1 \
    /tmp/authrity-campaign-tools.kti2k3/venv/bin/python -m verify_autoresearch \
    --root validation/verified-runs/product-five-rounds/workspace \
    --check-run 6987fdbcebad44cbbf2e1a8e87d78219
```

This verifies that archived experiment, not current source changes. A new build uses a separate target directory and needs its own complete three-month acceptance run and original-React interaction.


### Independent engineering-scale charts

The original **controller → Trends** tab and **Autoresearch → 結果與報告** use the shared `ProcessTrendChart`. The default is one physical variable per lane with a shared time cursor/window. Desktop overlays permit at most three variables; narrow screens use lanes. Two CVs never share a Y domain merely because their units match. A variable's actual, forecast, references, and compared methods use that variable's one domain. PID OP and mode codes stay separate from PV/SP.

Each axis has **細節**, **全部**, **手動**, and **鎖定此軸** controls. Detail excludes boundary lines from automatic scaling but reports clipped limits. All includes configured boundaries. Manual bounds must be finite and ordered. Hiding a curve does not change the axis domain. Axis settings are retained independently. Expand **資料與範圍提示** for unavailable sources and off-screen values. Legends distinguish method/result version; overlay legends also identify the physical variable.
Colors group the displayed physical variable and method/result version; line patterns distinguish actual, prediction, command, plan, reference, and boundary types. The legend uses the same patterns as the curves. Unavailable/clipped-source notices sit above the plot. Editing a signal's unit keeps newly configured variables visible without resetting the other axes. Repeated historian zooms retain the original selected window's relative-time origin.

Research history defaults to the selected COM DA trajectories. Enable baseline/native variants when needed. CV forecasts are aligned to **valid time**, not placed on their issue-time measurements. For lead `k`, valid time is `issued_at + k * sample_time`. Move the common cursor and choose **以目前游標載入完整未來預測** to inspect the original saved horizon, corresponding later recorded observations, and cumulative absolute MV plan. Reference lines marked **起算時參考** are held from that scan; they are not invented future measurements. The chart origin is the frozen scenario's `config.start`, not the date the experiment ran.
The running-study view puts actual trial scores before version selection. Completed-study chart options are under **調整比較方法、結果版本、預測與時間範圍**, so controls do not displace the engineering curves. Version preservation/installation remains an explicit action after reviewing the evidence.

New `mpc_campaign closed-loop --forecast-output PATH` executions write an exclusive-create NDJSON archive and `PATH.index.json`. The header records sampling, dimensions, horizons, step-major layout, CV first offset `+1`, and MV first offset `0`. Records contain sample/issue time, full CV predictions, pre-move MV base, and solver MV increments. Research command receipts fingerprint both files. Failed writes do not become successful campaigns; interrupted archives without a finalized index are unavailable. Old runs are not rewritten or retroactively reconstructed.

Quality specifications (`spec_low`, `spec_high`) are optional display-only CV study fields. They remain distinct from operating constraints and the controller's Soft-band. DV forecasts are unavailable unless actually recorded. Historical OPC UA `Prediction`/`FuturePlan` arrays lack solver issue-time/sample-interval metadata in the existing historian; this chart deliberately does not turn their receipt times into forecast valid times. Existing live `PredictionGraph` is unchanged.

Historian `/api/trends` now returns `{points, rawSamples, gapThresholdMs, warnings}` rather than the old flat averaged array. Its sole React consumer is migrated. It retains per-signal extrema, setting/quality changes, explicit missing samples, and gaps identified **before** display reduction. The `max_points` budget is soft for those transitions; excessive raw/display requests ask the user to narrow the window. The step-response identification data endpoint is unchanged. OPC UA timestamps are receipt/logging times; unknown engineering units must be entered in the signal settings and are not silently converted.

Cursor readouts use exact timestamps and retain quality flags. Forecast residuals require actual and forecast values from the same method/result version at the same valid time, both with good quality. Neither interpolates missing evidence. Zoom into a smaller window to load detailed matching samples. Bad values are red points, uncertain values hollow points, unknown quality gray points, and missing data breaks the line. These display reductions never replace full-resolution control metrics.

Run chart edge-case coverage without adding a test framework:

```fish
cd hmi/frontend
node --test tests/processTrend.test.ts
cd ../../validation
python -m unittest test_research_trends test_research -v
```

The complete acceptance entrypoint additionally checks all six full-quarter forecast archives, immutable hashes, index/scan correspondence, and issue/valid-time alignment. Browser interaction remains a separate requirement.

## Five-round product work: Round 1 evidence

`verified-runs/product-five-rounds/round1/verification.json` records the actual original React workflow, real PostgreSQL 17.11, and API checks. A three-calendar-month source has 129,601 raw rows including a duplicate; CSV and PostgreSQL produce identical 129,600-row cleaned CV/MV/DV/time arrays. All pressure-unit conversions were checked, not just a sampled chart. A non-extreme pressure correction at second 7,260 was absent from the old reduced display and is now retained and focusable.

The same run verified offset/microsecond timestamps, exact decimal text/null/quoted text, exclusive interval end, table/view equivalence, authentication/CSRF, immutable provenance, and rejection of unauthorized, missing, non-TLS, oversized, timed-out, naïve-timestamp, or write-attempt sources without publishing partial datasets. Actual database write-audit count remained zero. Desktop and 390-pixel mobile import/error-recovery screenshots are beside the evidence. The complete 71-test Python suite, including actual Rust-engine integration, and 26 HMI tests passed; the React build passed with its existing bundle-size warning. These are data-ingress/cleaning results, not proof of factory control or completion of the remaining four product rounds.

## Model-to-control commissioning contract

See [the current product contract](../DIGITAL_TWIN_VALIDATION.md#第-35-輪產品化契約與本機驗收) for source implementation and exercised acceptance. Do not use a historical test count as proof of the current build.

- A frozen model version is reviewable evidence, not site authorization. Installation is append-only and exact-byte checked. Start verifies those bytes again; immutable starts cannot enable live weight overrides.
- Provision the configured CV/MV/DV nodes, a producer-owned `{engine}:AcquisitionSequence` Double, and `{engine}:ControlObservation` String. The producer publishes odd → complete acquisition → next even. A constant initial value or engine heartbeat does not establish freshness.
- Generator import/export preserves controller identity, ordered dimensions, sampling, exact physical IDs and solver settings. Node YAML carries an explicit `model_binding`; HMI roles and permissions come from this binding, never from a name suffix. Missing or colliding mappings fail closed. Node deployment refreshes the controller catalog but does not install a model, start a process or grant control.
- Historian samples retain exact `node_id`, original numeric precision and OPC status. Internal HMI metadata is explicitly marked and excluded; physical IDs beginning with `_` or `System:` remain valid. These are HMI receipt timestamps, not proof of PLC source timing.
- Map the accepted lower-PID SP to `applied_sp` when available. SP-register fallback is explicitly weaker evidence; OP is never substituted for model input. Review ABB mode semantics and unit conversion at the integration boundary.
- Start in Monitor, establish healthy coherent data and valid model/tuning/limits, then explicitly request Engage. Each MV needs the actual/requested remote-cascade contract. Fault recovery returns through observation and a new operator decision; it never silently resumes control.
- The live UI shows measured rolling CV variance, tracking bias, violations and applied-SP movement. Stale/disconnected/invalid observations are hidden, and incomplete windows are labelled. These observations do not independently establish causal improvement over a baseline.
- An OPC Good write reply means server acceptance only. The engine separately checks application feedback. The UI does not use unsolicited tag updates or timeouts as success and does not automatically repeat unknown-outcome writes.
- A supervisor stop request is delivered to the monitor that owns the child handle, not an external PID utility. Only confirmed child exit releases the slot. An operator-requested termination becomes `Stopped`; an unrelated nonzero/signal exit remains `Failed`. Stopping the engine does not change lower-PID mode or establish a safe plant shutdown by itself.
- Manual snapshots require an explicit output directory (`--model-output-dir`, supplied by supervisor). They preserve controller identity but are unqualified manual models, not edited immutable versions.
- Infrastructure API responses contain resolved usernames but no password dictionary or per-client passwords. Credential normalization must preserve saved credentials across reloads; omitted passwords preserve the stored secret rather than restoring a built-in default.
- For isolated services, set Controller Host `--bind-address 127.0.0.1` and OPC management `[api] bind_address = "127.0.0.1"`; configure the OPC UA listener separately in `serversettings.env`. Malformed management configuration fails rather than silently binding a default interface.
- The native engine honors its configured `--pki` root and rejects untrusted server certificates before initialization or MV writes. It never auto-trusts a server. This OPC library looks up trusted certificates by `CertificateStore::cert_file_name` (subject plus thumbprint), not an arbitrary filename. A matching entry in `rejected` takes precedence. Verify the certificate against the intended server before moving it to `trusted`; never blanket-trust rejected certificates.

The historical identification and autoresearch plant assumptions remain stable, finite-memory local dynamics. This is a configurable workflow for qualifying individual cement-process operating regimes, not proof that one fitted mill model controls every cement process. Integrating/nonstationary regimes and unexcited observational data must not be silently accepted.


## Current five-round build and full revalidation

These fish commands reproduce verification in this checkout. The user explicitly authorized the assistant to run this isolated acceptance. They use the existing private toolchain and a **new target directory**, leaving the historical executable untouched. The Rust/Python/TypeScript tests are regressions, not substitutes for the complete campaign or browser/control-flow acceptance.

```fish
cd /home/timmypai/apps/AuTHRity-APC
function verify_authrity_product_five_rounds
    set -l tools /tmp/authrity-campaign-tools.kti2k3
    set -lx CARGO_HOME "$tools/cargo"
    set -lx RUSTUP_HOME "$tools/rustup"
    set -lx CARGO_TARGET_DIR "$tools/target-product-rounds"
    set -lx CARGO_BUILD_JOBS 1
    set -lx PATH "$tools/cargo/bin" /home/timmypai/.nvm/versions/node/v24.18.0/bin $PATH
    set -lx PKG_CONFIG "$tools/sysroot/usr/bin/pkgconf"
    set -lx LD_LIBRARY_PATH "$tools/sysroot/usr/lib/x86_64-linux-gnu" $LD_LIBRARY_PATH
    for crate in apc_engine controller_host hmi virtual_plant opcua_server
        command cargo test --release --manifest-path "$crate/Cargo.toml" --all-targets
        or return
        command cargo build --release --manifest-path "$crate/Cargo.toml" --bins
        or return
    end
    env PYTHONPATH="$PWD/validation" OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1 \
        MPC_VALIDATE_BIN="$CARGO_TARGET_DIR/release/mpc_validate" \
        "$tools/venv/bin/python" -m unittest discover -s validation -p 'test_*.py' -v
    or return
    command npm --prefix hmi/frontend run build
    or return
    command node --test hmi/frontend/tests/*.test.ts
end
verify_authrity_product_five_rounds
```

Only after that succeeds, run the entire quarter with the new binary and the already registered **private simulator** Wine prefix. This does not start any live OPC UA or factory controller:

```fish
cd /home/timmypai/apps/AuTHRity-APC
begin
    set -l tools /tmp/authrity-campaign-tools.kti2k3
    set -lx WINEPREFIX "$tools/wineprefix"
    set -lx WINEARCH win64
    set -lx WINEDEBUG -all
    set -lx WINESERVER "$tools/sysroot/usr/lib/x86_64-linux-gnu/wine/wineserver"
    set -lx WINEDLLPATH "$tools/sysroot/usr/lib/x86_64-linux-gnu/wine"
    set -lx LD_LIBRARY_PATH "$tools/sysroot/usr/lib/x86_64-linux-gnu" $LD_LIBRARY_PATH
    env PYTHONPATH="$PWD/validation" OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1 \
        "$tools/venv/bin/python" -m verify_autoresearch \
        --root validation/verified-runs/product-five-rounds/current-source-acceptance \
        --runtime-file validation/verified-runs/product-five-rounds/runtime-current-source.json
end
```

Keep the research sources and executable unchanged while the run is active. It records an attempt root/run ID and ends with machine-check evidence or the actual failure. Afterwards the rebuilt original React and isolated services still require model-selection/install/start, Monitor→Engage→stop, fault/recovery and desktop/mobile acceptance. No command above claims those interactions have happened.


### Completed current-source acceptance

The authorized local acceptance completed on 2026-10-27. Its index is
[`verified-runs/product-five-rounds/current-source-checkpoint.json`](verified-runs/product-five-rounds/current-source-checkpoint.json).
The current-source full-quarter run is `888c83de64c64f1ab04d9210c58762e1`:
129,600 scans per method, six adaptive rounds, 26 trials, three gain scenarios,
94/94 work items and 16/16 execution checks. Its independent verifier result is
`current-source-acceptance/attempt-1793073242021806812/runs/888c83de64c64f1ab04d9210c58762e1/acceptance.json`
under that evidence root. Complete traces and forecasts remain archived.

Round 3 records exact identity, immutable installation and native control.
Round 4 records measured observations, stale acquisition, actual bad OPC quality,
frozen-tuning mismatch, separate accepted-SP timeout/recovery, PID authority
withdrawal and explicit shutdown. It also preserves the failing and corrected
supervisor stop behavior. Round 5 records the original React desktop/mobile
surfaces, independent axes, result recommendations and final database import.
These are actual service/browser interactions, not assertions made by the
numerical verifier.

The original React workspaces are research at `http://127.0.0.1:43179` and
control/historian at `http://127.0.0.2:43178`, with isolated test login
`admin` / `admin`. Research stays disconnected from OPC UA and historian;
the configured PostgreSQL source is read-only and contains synthetic data.
Do not merge these services by removing the research isolation check.
The test engines are stopped; neither the accepted model nor a green simulation
report grants factory control authority.

## Comparison, diagnostic and selectable trend views

In the original React workspace, open **Autoresearch → 4 結果與報告**,
select the completed run, then **查看曲線**.

- **基準 ↔ 優化** compares one method's native baseline and optimized traces
  in separate panels. **方法比較** shows optimized methods only.
  **進階疊圖** allows explicit method and native/COM DA source selection.
- Open **選線** beside a variable to select each concrete signal independently.
  **恢復預設**, **全部線**, and **清空** affect that variable only.
  Process and diagnostic views preserve their own selections across reloads.
- Different variables retain independent engineering scales. Comparison panels
  for the same variable share their scale unless explicitly unlocked.
  The mobile layout stacks those panels without sharing unrelated Y axes.
- The full-period overview shows observed Good-sample medians and min/max
  envelopes, not confidence intervals. Its phase boundaries come from the
  recorded study. Drag or keyboard-adjust the brush, then press
  **套用總覽選取**. Time fields, event focus, and the shared cursor stay aligned.
- **預測診斷** separates actual-versus-forecast plots from residuals.
  **預測資料與精確時間範圍** selects a fixed forecast lead or one archived
  solver horizon. Missing forecasts and unconfigured specifications remain
  unavailable; no substitute predictions or quality limits are invented.
- Selected-window statistics use complete raw samples before display
  reduction. Windows include both endpoint timestamps. Violation/coverage
  durations are comparable Good samples multiplied by the scan period, not
  interpolated continuous-time duration. No comparable evidence means null,
  not zero. MV movement requires consecutive Good commands within the window;
  movement is not energy consumption. Zooming never changes frozen scoring,
  held-out validation, model selection, or installed control settings.

Actual-quarter API comparisons, original React desktop/mobile interactions,
historian quality-marker regression, and test/build results are recorded in
[`verified-runs/chart-presentation/`](verified-runs/chart-presentation/).
The renderer update does not constitute a new numerical optimization run or
factory qualification. The selected immutable model remains unchanged.

## Digital twin workbench

The original React workflow now has **Autoresearch → 5 數位孿生**.
It uses the selected research run; it is not a separate report website or a
production control console. Rebuild the HMI as well as the frontend: the new
session routes do not exist in an already-running older HMI executable.

- **已記錄試驗 A/B** reads the actual archived selection-validation trajectories
  of two trials, including non-winning valid trials. This is not a full-period
  rerun, and missing archived forecast horizons remain unavailable.
- **新模擬與工作階段** freezes the selected trial's exact controller bytes and the
  chosen selection/full scenario in a new directory. Tuning multipliers are not
  applied twice. Full-scenario reuse is exploratory and does not grant a new
  untouched-test qualification or deployment approval.
- **單步推進** executes one real Rust MPC/plant scan. **連續推進** sends serial,
  bounded advance requests; its speed is scans per update, not a changed model
  sample time. **暫停推進** stops new requests after the current finite batch.
  A lost response stops playback and retrieves state rather than retrying a
  potentially applied command.
- The MV/DV → dynamic plant → CV view displays actual returned values, units,
  quality and simulation time. It is model topology, not an invented equipment
  P&ID or simulated sensors that the model does not contain.
- During execution, charts explicitly show at most the latest 256 completed
  scans. Completed sessions reopen full-resolution archived trajectories.
  Comparing completed sessions requires matching plant/scenario hashes,
  source clock and variable contracts.
- **CV／MV／DV 時間對齊區塊** puts CV above MV above DV with common time
  geometry. Each variable retains its engineering scale. Up to two variables
  use left/right axes; larger groups and mobile displays use sublanes.
  Per-line selection and same-variable comparison scaling remain available.

`campaign::CampaignRunner` owns the shared batch/session scan state.
`mpc_campaign session` accepts JSONL snapshot/advance/close commands.
Advance requires the exact `expected_samples` and a count in `1..4096`.
An interrupted run is not a valid completed result. The HMI serial owner
limits active sessions, expires idle sessions, and releases native/bridge
processes; after restart it never reconstructs hidden plant/controller state
from a single CV sample.

`validation/verify_twin.py` runs complete frozen scenarios through both batch
and session entrypoints, optionally including actual COM DA. It compares
every time/CV/MV/DV/target/command/prediction/quality sample, checks read-only
snapshots and rejected duplicate advances, and preserves independent artifacts.
`verify_autoresearch.py --plant-source identified_history` generates and
cleans synthetic history before fitting SOPDT dynamics and running the existing
complete adaptive search. It removes the generation plant/scenario from the
identification study; engineering limits remain explicitly those of the
synthetic fixture, not approvals for a factory.

Design, safety rules and acceptance criteria:
[`../DIGITAL_TWIN_VALIDATION.md`](../DIGITAL_TWIN_VALIDATION.md#數位孿生工作台辨識--閉環調參--選定候選重驗).
The current workbench evidence directory is
[`verified-runs/digital-twin-workbench/`](verified-runs/digital-twin-workbench/).
Its checkpoint files distinguish observed checks from pending runtime
acceptance; source implementation alone does not mean the full workbench has
passed.


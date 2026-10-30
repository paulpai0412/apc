"""Local-only, artifact-backed research worker used by the original HMI.

The worker never deploys. Configuration and executable paths come from the
administrator, not an HTTP request. Chronological validation selects candidates;
the final test interval is evaluated only after every method's winner is frozen.
"""
import argparse
import copy
import hashlib
import importlib
import json
import math
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import time
import uuid
from datetime import datetime, timezone

import numpy as np

from campaign import (check_config, controller_template, generate_scenario,
                      slice_scenario, validate_result)
from common import fingerprint, load_json, parse_json, validate_history
from identify import frozen_prediction_metrics
from research_identification import identify_control
from research_trends import process_trends
from research_design import history_draft, match_history, study_splits, validate_engineering
from research_evaluation import aggregate_evaluations, objective_defaults, research_metrics, validate_objective
from research_plant import fit_history_plant, modelling_settings, scenario_from_history

METHODS = ("standard_dmc", "soft_band")
TERMINAL = ("completed", "failed", "cancelled")
HERE = Path(__file__).resolve().parent
SOURCE_MODULES = ("research.py", "research_design.py", "research_evaluation.py", "research_plant.py",
                  "research_trends.py", "research_workbench.py", "research_policy.py", "research_identification.py",
                  "research_sources.py", "research_policy_candidates.py", "datasets.py", "identify.py", "campaign.py", "common.py")


def now():
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def atomic_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + "." + uuid.uuid4().hex + ".tmp")
    try:
        temporary.write_text(json.dumps(value, ensure_ascii=False, allow_nan=False, separators=(",", ":")), encoding="utf-8")
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def safe_id(value):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,96}", value):
        raise ValueError("Invalid artifact ID")
    return value


def run_dir(root, run_id):
    return root / "runs" / safe_id(run_id)


def runtime_settings():
    settings = json.loads(os.environ.get("AUTHRITY_RESEARCH_RUNTIME", "{}"))
    if settings.get("local_only") is not True:
        raise ValueError("Research requires administrator-configured local_only runtime")
    for key in ("binary", "bridge"):
        path = Path(settings.get(key, ""))
        if not path.is_absolute() or not path.is_file():
            raise ValueError(f"Configure an existing absolute research {key} executable")
    args = settings.get("bridge_args", [])
    if not isinstance(args, list) or any(not isinstance(arg, str) for arg in args):
        raise ValueError("bridge_args must be administrator-configured strings")
    timeout = settings.setdefault("native_timeout_seconds", 3600)
    if type(timeout) is not int or not 1 <= timeout <= 86400:
        raise ValueError("Administrator native_timeout_seconds must be an integer in [1, 86400]")
    return settings


def defaults():
    config = load_json(HERE / "examples" / "cement_campaign.json")
    config.update(process_area="cement_grinding", engineering_confirmed=True,
                  plant_source="independent_synthetic", mv_coordinate="pid_setpoint")
    config["modelling"] = modelling_settings(config)
    scenarios = [{"id": "nominal", "gain_multiplier": 1.0},
                 {"id": "gain_low", "gain_multiplier": 0.85},
                 {"id": "gain_high", "gain_multiplier": 1.15}]
    objective = objective_defaults(config)
    objective["scenarios"] = [scenario["id"] for scenario in scenarios]
    bands = [{"half_width": (cv["high"] - cv["low"]) * 0.025,
              "tracking_scale": 0.05, "quadratic_penalty": 1.0}
             for cv in config["variables"]["cv"]]
    return {"campaign": config, "research": {
        "methods": list(METHODS), "rounds": 6, "max_trials": 26,
        "initial": {"cv_weight_scale": 1.0, "mv_weight_scale": 1.0, "alpha": 0.3},
        "bounds": {"cv_weight_scale": [0.1, 10.0], "mv_weight_scale": [0.02, 20.0], "alpha": [0.0, 0.95]},
        "step_factor": 1.5, "bands": bands, "objective": objective, "scenarios": scenarios}}


def numeric(value, label):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        raise ValueError(f"{label} must be a finite number")
    return value


def validate_study(payload):
    result = copy.deepcopy(payload)
    name = result.get("name", "").strip()
    if not name or len(name) > 120:
        raise ValueError("Give this study a name of 1–120 characters")
    config = validate_engineering(result["config"])
    source = config.get("plant_source")
    if source == "independent_synthetic":
        check_config(config)
    elif source == "identified_history":
        if not result.get("dataset_id") or "plant" in config or "scenario" in config:
            raise ValueError("History-derived studies require a cleaned dataset and cannot carry unrelated synthetic plant/scenario settings")
        study_splits(config)
    else:
        raise ValueError("Select an explicit independent-synthetic or history-derived evaluation plant")
    config["modelling"] = modelling_settings(config)
    research = result["research"]
    if research.get("selection_mode", "automatic") not in ("automatic", "manual"):
        raise ValueError("selection_mode must be automatic or manual before sealed validation")
    if result.get("model_id"):
        safe_id(result["model_id"])
    methods = research["methods"]
    if not isinstance(methods, list) or not methods or len(set(methods)) != len(methods) or any(m not in METHODS for m in methods):
        raise ValueError("Select supported, unique control methods")
    rounds, budget = research["rounds"], research["max_trials"]
    if isinstance(rounds, bool) or not isinstance(rounds, int) or not 3 <= rounds <= 10:
        raise ValueError("Adaptive rounds must be an integer from 3 to 10 to cover every search coordinate")
    required = len(methods) * (1 + 2 * rounds)
    if isinstance(budget, bool) or not isinstance(budget, int) or not required <= budget <= 128:
        raise ValueError(f"max_trials must be {required}–128 for the requested complete adaptive rounds")
    factor = numeric(research["step_factor"], "step_factor")
    if not 1 < factor <= 4:
        raise ValueError("step_factor must be greater than 1 and at most 4")
    for key in ("cv_weight_scale", "mv_weight_scale", "alpha"):
        bounds = research["bounds"][key]
        if not isinstance(bounds, list) or len(bounds) != 2:
            raise ValueError(f"{key} needs lower and upper bounds")
        lo, hi = [numeric(v, key) for v in bounds]
        if not lo < hi or (key == "alpha" and not 0 <= lo < hi < 1) or (key != "alpha" and not 0 < lo < hi <= 1e6):
            raise ValueError(f"Invalid bounds for {key}")
        if not lo <= numeric(research["initial"][key], key) <= hi:
            raise ValueError(f"Initial {key} is outside its bounds")
    scenarios = research.get("scenarios")
    if not isinstance(scenarios, list) or not 3 <= len(scenarios) <= 8:
        raise ValueError("Freeze 3–8 nominal and uncertainty scenarios before search")
    ids, multipliers = [], []
    for scenario in scenarios:
        if not isinstance(scenario, dict) or set(scenario) != {"id", "gain_multiplier"}:
            raise ValueError("Each scenario requires only id and gain_multiplier")
        identifier = safe_id(scenario["id"])
        gain = numeric(scenario["gain_multiplier"], "gain_multiplier")
        if not 0.25 <= gain <= 2:
            raise ValueError("Scenario gain multipliers must be within [0.25, 2]")
        ids.append(identifier)
        multipliers.append(gain)
    if len(set(ids)) != len(ids) or len(set(multipliers)) != len(multipliers) or "nominal" not in ids or multipliers[ids.index("nominal")] != 1:
        raise ValueError("Scenario IDs/gains must be unique and nominal gain must be one")
    if not min(multipliers) < 1 < max(multipliers):
        raise ValueError("Uncertainty scenarios must cover both lower and higher plant gain")
    objective = validate_objective(research.get("objective"), config)
    if objective["scenarios"] != ids:
        raise ValueError("Objective scenario coverage differs from the frozen scenario definitions")
    for variable, gate in zip(config["variables"]["cv"], objective["cv"]):
        low, high = variable.get("spec_low"), variable.get("spec_high")
        expected = None if low is None and high is None else {"low": low, "high": high}
        if gate["quality_spec"] != expected:
            raise ValueError(f"{variable['name']}: chart and objective quality specifications must match")
    research["objective"] = objective
    if "soft_band" in methods:
        bands = research["bands"]
        if len(bands) != len(config["variables"]["cv"]):
            raise ValueError("Provide one quality band per CV")
        for cv, band in zip(config["variables"]["cv"], bands):
            for field in ("half_width", "tracking_scale", "quadratic_penalty"):
                if numeric(band[field], field) <= 0:
                    raise ValueError(f"{field} must be positive")
            if band["tracking_scale"] > 1:
                raise ValueError("Quality-band tracking_scale must not exceed one")
            if not cv["low"] <= cv["target"] - band["half_width"] < cv["target"] + band["half_width"] <= cv["high"]:
                raise ValueError(f"{cv['name']}: quality band must fit within the operating limits")
    result["name"] = name
    if result.get("dataset_id"):
        safe_id(result["dataset_id"])
    return result


def load_run(root, run_id):
    return load_json(run_dir(root, run_id) / "run.json")


def release_active(root, run_id):
    lock = root / "active.json"
    if lock.exists() and load_json(lock).get("id") == run_id:
        lock.unlink()


def reconcile_active(root):
    lock = root / "active.json"
    if not lock.exists():
        return
    try:
        active = load_json(lock)
    except (ValueError, OSError):
        if time.time() - lock.stat().st_mtime < 120:
            raise ValueError("Another research launch is in progress")
        raise ValueError("Research launch record is damaged; inspect active.json before starting another worker")
    run = load_run(root, active["id"])
    if run["status"] in TERMINAL:
        release_active(root, run["id"])
        return
    pid = active.get("pid")
    alive = False
    if pid:
        try:
            os.kill(pid, 0)
            alive = True
        except ProcessLookupError:
            pass
        except PermissionError:
            alive = True
    elif time.time() - active["created_epoch"] < 120:
        alive = True
    if not alive:
        fail_run(root, {"id": run["id"], "error": "Research worker exited or did not start; no automatic replay of partial work"})


def fail_run(root, payload):
    directory = run_dir(root, payload["id"])
    record = load_run(root, payload["id"])
    if record["status"] not in TERMINAL:
        record.update(status="failed", error=str(payload.get("error", "Worker failed"))[:4000], completed_at=now())
        atomic_json(directory / "run.json", record)
    release_active(root, record["id"])
    return record


def create_run(root, payload):
    runtime = runtime_settings()
    reconcile_active(root)
    study = validate_study(load_json(root / "studies" / (safe_id(payload["study_id"]) + ".json")))
    dataset_bytes = None
    if study.get("dataset_id"):
        dataset_bytes = (root / "datasets" / safe_id(study["dataset_id"]) / "history.json").read_bytes()
        if hashlib.sha256(dataset_bytes).hexdigest() != study.get("dataset_sha256"):
            raise ValueError("The dataset no longer matches the saved study; create and review a new version")
        match_history(study["config"], json.loads(dataset_bytes))
    run_id = safe_id(payload["reserved_run_id"]) if payload.get("reserved_run_id") else uuid.uuid4().hex
    directory = run_dir(root, run_id)
    directory.mkdir(parents=True, exist_ok=False)
    if dataset_bytes is not None:
        (directory / "history.json").write_bytes(dataset_bytes)
    if study.get("model_id"):
        from research_workbench import load_model
        atomic_json(directory / "frozen-model.json", load_model(root, study["model_id"]))
    record = {"id": run_id, "study_id": study["id"], "name": study["name"], "status": "queued",
              "stage": "queued", "created_at": now(), "progress": {"completed": 0, "total": 0},
              "trials": [], "checks": [], "scope": study["config"]["plant_source"] + "; local simulation only, never factory authority", "study": study}
    atomic_json(directory / "run.json", record)
    atomic_json(directory / "study.json", study)
    atomic_json(directory / "runtime.json", runtime)
    try:
        with (root / "active.json").open("x", encoding="utf-8") as stream:
            json.dump({"id": run_id, "created_epoch": time.time()}, stream)
    except FileExistsError as error:
        record.update(status="failed", error="Another research run is active", completed_at=now())
        atomic_json(directory / "run.json", record)
        raise ValueError("Another research run is active; cancel or wait for it") from error
    return record


def parameter_key(parameters):
    return tuple(parameters[key] for key in ("cv_weight_scale", "mv_weight_scale", "alpha"))


def next_candidates(best, research, round_index, contractions, seen):
    """Bounded coordinate search with cumulative contraction after rejected rounds."""
    keys = ("mv_weight_scale", "cv_weight_scale", "alpha")
    factor = research["step_factor"] ** (0.5 ** contractions)
    result = []
    for offset in range(3):
        key = keys[(round_index - 1 + offset) % 3]
        lo, hi = research["bounds"][key]
        for direction in (-1, 1):
            candidate = copy.deepcopy(best)
            if key == "alpha":
                value = best[key] + direction * (hi - lo) * (factor - 1) / 4
            else:
                value = best[key] * factor ** direction
            candidate[key] = round(min(hi, max(lo, value)), 12)
            signature = parameter_key(candidate)
            if signature in seen:
                continue
            seen.add(signature)
            result.append(candidate)
            if len(result) == 2:
                return result
    raise ValueError("Search bounds exhausted before the required rounds; widen bounds or reduce rounds")


def make_controller(model, method, parameters, bands):
    result = copy.deepcopy(model)
    for cv in result["variables"]["cvs"]:
        cv["weight"] *= parameters["cv_weight_scale"]
        cv["alpha"] = parameters["alpha"]
    for mv in result["variables"]["mvs"]:
        mv["weight_r"] *= parameters["mv_weight_scale"]
    result["control_strategy"] = {"method": method, "version": 1, "bands": copy.deepcopy(bands) if method == "soft_band" else []}
    return result


class Cancelled(Exception):
    pass


class ResearchRun:
    def __init__(self, root, run_id):
        self.root = root
        self.directory = run_dir(root, run_id)
        self.record = load_run(root, run_id)
        if self.record["status"] != "queued":
            raise ValueError("Only a queued research run can start; partial runs are never silently replayed")
        study_bytes = (self.directory / "study.json").read_bytes()
        self.study = parse_json(study_bytes)
        self.study_sha256 = hashlib.sha256(study_bytes).hexdigest()
        self.config = self.study["config"]
        self.research = self.study["research"]
        runtime_bytes = (self.directory / "runtime.json").read_bytes()
        self.runtime = parse_json(runtime_bytes)
        self.runtime_sha256 = hashlib.sha256(runtime_bytes).hexdigest()
        self.splits = study_splits(self.config)
        scenarios = len(self.research["scenarios"])
        preparation = 2 + int(self.config["plant_source"] == "identified_history")
        self.record["progress"]["total"] = preparation + len(self.research["methods"]) * ((1 + 2 * self.research["rounds"]) * scenarios + 2 * scenarios + 1)
        self.completed = 0

    def execution_hashes(self):
        sources = {}
        for name in SOURCE_MODULES:
            path = Path(__file__) if name == "research.py" else Path(importlib.import_module(Path(name).stem).__file__)
            sources[name] = fingerprint(path)
        hashes = {"engine": fingerprint(self.runtime["binary"]), "study": fingerprint(self.directory / "study.json"),
                  "runtime": fingerprint(self.directory / "runtime.json"), "sources": sources}
        if self.study.get("model_id"):
            hashes["frozen_model"] = fingerprint(self.directory / "frozen-model.json")
        return hashes

    def begin_provenance(self):
        hashes = self.execution_hashes()
        if hashes["study"] != self.study_sha256 or hashes["runtime"] != self.runtime_sha256:
            raise RuntimeError("Frozen study/runtime changed after loading; start a new run")
        self.record["provenance"] = dict(hashes, captured_at=now(),
            source_hash_timing="worker_start_boundary_checked_files",
            integrity="Filesystem hashes checked before/after native invocations and at completion; not loaded-memory or signed attestation")

    def check_execution_inputs(self):
        expected = self.record.get("provenance", {})
        current = self.execution_hashes()
        if any(expected.get(key) != value for key, value in current.items()):
            # Fatal run failure, not a rejected trial that adaptive search can ignore.
            raise RuntimeError("Research code, engine or frozen configuration changed during execution; start a new run")

    def update(self, stage, complete=False):
        if complete:
            self.completed += 1
        self.record.update(stage=stage, updated_at=now())
        self.record["progress"]["completed"] = self.completed
        atomic_json(self.directory / "run.json", self.record)

    def check_cancelled(self):
        if (self.directory / "cancel.request").exists():
            raise Cancelled("Cancelled by user; partial trials are not selected or deployed")

    def invoke(self, name, scenario_name, controller_name=None, transport=False, *, plant_name="plant.json", forecast=True):
        self.check_cancelled()
        self.check_execution_inputs()
        self.update(name)
        output = self.directory / (name + ".json")
        command = [self.runtime["binary"], "closed-loop" if controller_name else "replay",
                   "--plant", str(self.directory / plant_name), "--scenario", str(self.directory / scenario_name), "--output", str(output)]
        if controller_name:
            command += ["--controller", str(self.directory / controller_name)]
            if forecast:
                command += ["--forecast-output", str(self.directory / (name + ".forecast.ndjson"))]
        if transport:
            command += ["--transport", "opc-da", "--bridge", self.runtime["bridge"]]
            for arg in self.runtime.get("bridge_args", []):
                command += ["--bridge-arg", arg]
        receipt = {"argv": command, "started_at": now(), "controller_sha256": fingerprint(self.directory / controller_name) if controller_name else None,
                   "plant_sha256": fingerprint(self.directory / plant_name),
                   "scenario_sha256": fingerprint(self.directory / scenario_name), "engine_sha256": self.record["provenance"]["engine"]}

        def persist_receipt():
            for key, suffix in (("output_sha256", ".json"), ("forecast_sha256", ".forecast.ndjson"),
                                ("forecast_index_sha256", ".forecast.ndjson.index.json")):
                artifact = self.directory / (name + suffix)
                if artifact.is_file():
                    receipt[key] = fingerprint(artifact)
            atomic_json(self.directory / (name + ".command.json"), receipt)

        with (self.directory / (name + ".stdout.log")).open("wb") as stdout, (self.directory / (name + ".stderr.log")).open("wb") as stderr:
            process = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True)
            started = time.monotonic()
            try:
                while process.poll() is None:
                    self.check_cancelled()
                    timeout = self.runtime.get("native_timeout_seconds", 3600)
                    if time.monotonic() - started > timeout:
                        raise ValueError(f"{name}: native execution exceeded administrator timeout ({timeout}s)")
                    time.sleep(0.2)
            except BaseException:
                if os.name == "posix":
                    os.killpg(process.pid, signal.SIGTERM)
                else:
                    process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    if os.name == "posix":
                        os.killpg(process.pid, signal.SIGKILL)
                    else:
                        process.kill()
                    process.wait()
                receipt.update(returncode=process.returncode, completed_at=now(), cancelled=True)
                persist_receipt()
                raise
        receipt.update(returncode=process.returncode, completed_at=now())
        persist_receipt()
        self.check_execution_inputs()
        self.update(name, complete=True)
        if process.returncode != 0 or not output.exists():
            detail = (self.directory / (name + ".stderr.log")).read_text(errors="replace")[-3000:]
            raise ValueError(f"{name}: controller process failed ({process.returncode}): {detail}")
        result = load_json(output)
        if controller_name and forecast:
            archive = self.directory / (name + ".forecast.ndjson")
            index = Path(str(archive) + ".index.json")
            if not archive.is_file() or not index.is_file() or index.stat().st_size == 0:
                raise ValueError(f"{name}: controller did not finalize its forecast archive and index")
        return result

    def prepare_and_identify(self):
        train_end, val_end = self.splits[0]["last"], self.splits[1]["last"]
        history = None
        if self.study.get("dataset_id"):
            history_bytes = (self.directory / "history.json").read_bytes()
            if hashlib.sha256(history_bytes).hexdigest() != self.study["dataset_sha256"]:
                raise ValueError("Frozen cleaned history differs from the selected dataset")
            history = parse_json(history_bytes)
            match_history(self.config, history)
            if self.study.get("model_id"):
                from research_workbench import digest
                self.frozen_model = load_json(self.directory / "frozen-model.json")
                current, frozen = copy.deepcopy(self.config), copy.deepcopy(self.frozen_model["config"])
                current.pop("start", None)
                frozen.pop("start", None)
                if digest(current) != digest(frozen):
                    raise ValueError("Frozen model engineering/Ts/memory/limits differ; create a new identification branch")
            self.record["dataset_sha256"] = self.study["dataset_sha256"]
            self.update("load-cleaned-history", complete=True)
        if self.config["plant_source"] == "identified_history":
            self.update("qualify-history-plant")
            if hasattr(self, "frozen_model"):
                plant, plant_evidence = self.frozen_model["evaluation_plant"], self.frozen_model["plant_evidence"]
            else:
                plant, plant_evidence = fit_history_plant(self.config, history, train_end, val_end)
            self.update("qualify-history-plant", complete=True)
        else:
            plant = copy.deepcopy(self.config["plant"])
            plant_evidence = {"status": "declared_independent_synthetic", "factory_validation": False,
                              "source": "Explicit engineering simulation, not an inferred factory model"}
        scenario = scenario_from_history(self.config, history, plant) if self.config["plant_source"] == "identified_history" else generate_scenario(self.config)
        scenario["provenance"]["evaluation_plant"] = self.config["plant_source"]
        atomic_json(self.directory / "plant.json", plant)
        atomic_json(self.directory / "plant-evidence.json", plant_evidence)
        atomic_json(self.directory / "scenario.json", scenario)
        self.plants = {}
        for definition in self.research["scenarios"]:
            name = definition["id"]
            filename = "plant.json" if name == "nominal" else "plant-" + name + ".json"
            if name != "nominal":
                alternate = copy.deepcopy(plant)
                for row in alternate["paths"]:
                    for path in row:
                        path["gain"] *= definition["gain_multiplier"]
                atomic_json(self.directory / filename, alternate)
            self.plants[name] = filename
        selection = slice_scenario(scenario, train_end, val_end)
        atomic_json(self.directory / "selection-scenario.json", selection)
        if history is None:
            history = self.invoke("history", "scenario.json")
            validate_history(history, controller_template(self.config))
            history["mv"] = history["command_mv"]
        self.check_cancelled()
        self.update("identify")
        options = dict(self.config["identification"])
        minimum_std = np.asarray(options.pop("minimum_input_std"), dtype=float)
        inputs = np.concatenate((history["mv"][:train_end], history["dv"][:train_end]), axis=1)
        input_std = inputs.std(axis=0)
        if minimum_std.shape != input_std.shape or np.any(input_std < minimum_std):
            raise ValueError("Training input excitation is below configured thresholds")
        if hasattr(self, "frozen_model") and self.research.get("policy_action") != "reidentify_and_tune":
            model, report = copy.deepcopy(self.frozen_model["model"]), copy.deepcopy(self.frozen_model["report"])
            observed = {key: history[key][:val_end] for key in ("time_seconds", "cv", "mv", "dv")}
            observed["sample_time"] = history["sample_time"]
            report["validation"] = frozen_prediction_metrics(model, observed, train_end)["test"]
            report["frozen_model_id"] = self.frozen_model["id"]
            report["qualification_dataset_id"] = self.frozen_model["dataset_id"]
            report["refitted"] = False
        else:
            if self.research.get("policy_action") == "reidentify_and_tune":
                from research_sources import qualify_generated_history
                qualify_generated_history(self.root, history)
            model, report = identify_control(self.config, history, train_end, val_end)
        ranges = np.array([cv["high"] - cv["low"] for cv in self.config["variables"]["cv"]])
        normalized = np.asarray(report["validation"]["rmse"]) / ranges
        if np.any(normalized > self.config["modelling"]["validation_max_normalized_rmse"]):
            raise ValueError(f"Controller model failed chronological free-run qualification: normalized CV RMSE {normalized.tolist()}")
        report.update(input_coordinate="Effective lower-PID setpoint history; lower-loop dynamics included in identified response",
                      truth_access="History and engineering template only; no simulator paths",
                      input_std=input_std.tolist(), minimum_input_std=minimum_std.tolist(),
                      validation_normalized_rmse=normalized.tolist(), plant=plant_evidence)
        atomic_json(self.directory / "baseline-controller.json", model)
        atomic_json(self.directory / "identification-development.json", report)
        self.update("identify", complete=True)
        self.history = history
        return model, report, scenario, selection

    def evaluate_trial(self, model, method, parameters, round_index, parent, selection, baseline=None):
        trial_id = f"{method}-trial-{len(self.record['trials']):03d}"
        filename = trial_id + "-controller.json"
        controller = make_controller(model, method, parameters, self.research["bands"])
        atomic_json(self.directory / filename, controller)
        trial = {"id": trial_id, "method": method, "round": round_index, "parent_trial": parent,
                 "parameters": parameters, "controller": filename, "valid": True,
                 "score": None, "scenario_metrics": {}, "scenario_samples": {}, "failures": []}
        trial["effective_tuning"] = {
            "tuning": controller["tuning"], "control_strategy": controller["control_strategy"],
            "cvs": [{key: cv[key] for key in ("name", "weight", "alpha", "slack_weight", "limits")} for cv in controller["variables"]["cvs"]],
            "mvs": [{key: mv[key] for key in ("name", "weight_r", "max_move", "limits")} for mv in controller["variables"]["mvs"]]}
        self.record["trials"].append(trial)
        self.update(trial_id + "-started")
        for scenario_id, plant_name in self.plants.items():
            name = trial_id if scenario_id == "nominal" else trial_id + "-" + scenario_id
            try:
                result = self.invoke(name, "selection-scenario.json", filename, plant_name=plant_name, forecast=False)
                valid, reason = validate_result(result, selection, self.config)
                trial["scenario_samples"][scenario_id] = len(result.get("trace", {}).get("time_seconds", []))
                if not valid:
                    raise ValueError(reason)
                trial["scenario_metrics"][scenario_id] = research_metrics(result["trace"], self.config, self.research["objective"])
            except ValueError as error:
                trial["valid"] = False
                trial["failures"].append(f"{scenario_id}: {error}")
        assessment = aggregate_evaluations(trial["scenario_metrics"], baseline["scenario_metrics"] if baseline else {},
                                           self.config, self.research["objective"], baseline=baseline is None)
        trial.update(eligible=trial["valid"] and assessment["eligible"], score=assessment["score"],
                     assessment=assessment, samples=trial["scenario_samples"].get("nominal"),
                     reason="; ".join(trial["failures"] + assessment["reasons"]))
        trial["metrics"] = trial["scenario_metrics"].get("nominal")
        trial["status"] = "completed" if trial["valid"] else "failed"
        from research_workbench import pareto_trials
        self.record["development"] = pareto_trials(self.record["trials"])
        self.update(trial_id)
        atomic_json(self.directory / "trials.json", self.record["trials"])
        return trial

    def execute(self):
        active = load_json(self.root / "active.json")
        if active["id"] != self.record["id"]:
            raise ValueError("Run does not own the local execution lease")
        active["pid"] = os.getpid()
        atomic_json(self.root / "active.json", active)
        self.record.update(status="running", started_at=now())
        self.update("prepare")
        try:
            self.begin_provenance()
            self.update("provenance-captured")
            model, identification, scenario, selection = self.prepare_and_identify()
            winners = {}
            # Complete selection for ALL methods before reading any final test score.
            for method in self.research["methods"]:
                initial = copy.deepcopy(self.research["initial"])
                baseline = self.evaluate_trial(model, method, initial, 0, None, selection)
                if not baseline["valid"]:
                    raise ValueError(f"{method}: a numerically valid matching baseline is required")
                best = baseline if baseline["eligible"] else None
                seen = {parameter_key(initial)}
                contractions = 0
                for round_index in range(1, self.research["rounds"] + 1):
                    self.check_cancelled()
                    incumbent = best or baseline
                    proposals = next_candidates(incumbent["parameters"], self.research, round_index, contractions, seen)
                    previous_score = best["score"] if best else math.inf
                    parent = incumbent["id"]
                    for parameters in proposals:
                        trial = self.evaluate_trial(model, method, parameters, round_index, parent, selection, baseline)
                        if trial["eligible"] and (best is None or trial["score"] < best["score"]):
                            best = trial
                    improved = best is not None and best["score"] < previous_score
                    contractions = 0 if improved else contractions + 1
                chosen = best or baseline
                coverage = {key: sorted({trial["parameters"][key] for trial in self.record["trials"] if trial["method"] == method})
                            for key in initial}
                if any(len(values) < 2 for values in coverage.values()):
                    raise ValueError(f"{method}: search did not cover every declared tuning coordinate")
                winners[method] = {"baseline": baseline, "best": chosen, "coverage": coverage,
                                   "selection_recommendation": "candidate" if chosen["id"] != baseline["id"] else "retain_baseline"}
            from research_workbench import exposure, exposure_status
            if self.research.get("selection_mode", "automatic") == "manual":
                self.update("awaiting_selection")
                request_path = self.directory / "selection-request.json"
                while not request_path.exists():
                    self.check_cancelled()
                    time.sleep(0.5)
                decision = load_json(request_path)
                for method, selected_id in decision["selections"].items():
                    chosen = next(trial for trial in self.record["trials"] if trial["id"] == selected_id and trial["method"] == method)
                    winners[method]["best"] = chosen
                    winners[method]["selection_recommendation"] = "retain_baseline" if chosen["round"] == 0 else "candidate"
                self.record["selection_decision"] = decision
            else:
                self.record["selection_decision"] = {"mode": "automatic", "reason": "Frozen minimum-score objective before sealed validation"}
            self.update("selection_frozen")
            atomic_json(self.directory / "selection.json", {"winners": winners, "objective": self.research["objective"],
                        "policy": "Only complete chronological validation scenarios select candidates; every winner frozen before test evaluation"})
            if self.study.get("dataset_id"):
                summary = load_json(self.root / "datasets" / self.study["dataset_id"] / "summary.json")
                exposure(self.root, summary["source_sha256"], "sealed_historical_and_counterfactual_validation", run_id=self.record["id"])
                self.record["holdout"] = exposure_status(self.root, summary["source_sha256"])
            else:
                self.record["holdout"] = {"independent": False, "status": "synthetic_temporal_holdout", "reason": "Same frozen plant/uncertainty family; not independent factory evidence"}
            identification.update(frozen_prediction_metrics(model, self.history, self.splits[2]["first"]))
            atomic_json(self.directory / "identification.json", identification)
            test_rmse = np.asarray(identification["test"]["rmse"]) / np.array([v["high"] - v["low"] for v in self.config["variables"]["cv"]])
            model_test = {"passed": bool(np.all(test_rmse <= self.config["modelling"]["validation_max_normalized_rmse"])),
                          "normalized_rmse": test_rmse.tolist(), "used_for_selection": False}
            self.record["results"] = {"methods": [], "variables": self.config["variables"], "splits": self.splits,
                                      "identification": identification, "model_test": model_test,
                                      "objective": self.research["objective"], "scope": self.record["scope"]}
            for method, chosen in winners.items():
                row = {"method": method, "selected_trial": chosen["best"]["id"], "parameters": chosen["best"]["parameters"],
                       "selection_recommendation": chosen["selection_recommendation"], "coverage": chosen["coverage"],
                       "scenario_tests": {"baseline": {}, "optimized": {}}}
                for variant, selected in (("baseline", chosen["baseline"]), ("optimized", chosen["best"]), ("transport", chosen["best"])):
                    name = method + "-" + variant
                    result = self.invoke(name, "scenario.json", selected["controller"], transport=variant == "transport")
                    valid, reason = validate_result(result, scenario, self.config)
                    self.record["checks"].append({"name": name + "-full-period", "status": "passed" if valid else "failed", "detail": reason})
                    if not valid:
                        raise ValueError(f"{name}: {reason}")
                    trace = result["trace"]
                    row[variant] = {"full": research_metrics(trace, self.config, self.research["objective"]),
                                    "test": research_metrics(trace, self.config, self.research["objective"], self.splits[2]["first"], self.splits[2]["last"])}
                    if variant != "transport":
                        row["scenario_tests"][variant]["nominal"] = row[variant]["test"]
                    if variant == "transport":
                        direct = load_json(self.directory / (method + "-optimized.json"))["trace"]
                        differences = {}
                        for key in ("time_seconds", "cv", "mv", "dv", "targets", "command_mv"):
                            a, b = np.asarray(direct[key]), np.asarray(trace[key])
                            differences[key] = float(np.max(np.abs(a - b))) if a.size else 0.0
                            if not np.allclose(a, b, atol=self.config["transport"]["absolute_tolerance"], rtol=self.config["transport"]["relative_tolerance"]):
                                raise ValueError(f"{method}: COM/direct trajectory mismatch in {key}")
                        self.record["checks"].append({"name": method + "-actual-com-equivalence", "status": "passed", "detail": differences})
                for scenario_id, plant_name in self.plants.items():
                    if scenario_id == "nominal":
                        continue
                    for variant, selected in (("baseline", chosen["baseline"]), ("optimized", chosen["best"])):
                        name = method + "-" + variant + "-" + scenario_id
                        result = self.invoke(name, "scenario.json", selected["controller"], plant_name=plant_name, forecast=False)
                        valid, reason = validate_result(result, scenario, self.config)
                        self.record["checks"].append({"name": name + "-full-period", "status": "passed" if valid else "failed", "detail": reason})
                        if not valid:
                            raise ValueError(f"{name}: {reason}")
                        row["scenario_tests"][variant][scenario_id] = research_metrics(result["trace"], self.config, self.research["objective"], self.splits[2]["first"], self.splits[2]["last"])
                row["assessment"] = aggregate_evaluations(row["scenario_tests"]["optimized"], row["scenario_tests"]["baseline"],
                                                         self.config, self.research["objective"])
                row["recommendation"] = "candidate_for_review" if row["assessment"]["eligible"] and model_test["passed"] and chosen["selection_recommendation"] == "candidate" else "do_not_promote"
                self.record["results"]["methods"].append(row)
                self.update(method + "-complete")
            self.check_execution_inputs()
            self.record.update(status="completed", completed_at=now())
            self.update("completed")
        except Cancelled as error:
            if self.record["trials"] and self.record["trials"][-1].get("status") is None:
                self.record["trials"][-1].update(status="cancelled", valid=False, eligible=False, reason=str(error))
                atomic_json(self.directory / "trials.json", self.record["trials"])
            self.record.update(status="cancelled", error=str(error), completed_at=now())
            self.update("cancelled")
        except Exception as error:
            if self.record["trials"] and self.record["trials"][-1].get("status") is None:
                self.record["trials"][-1].update(status="failed", valid=False, eligible=False, reason=str(error))
                atomic_json(self.directory / "trials.json", self.record["trials"])
            self.record.update(status="failed", error=str(error), completed_at=now())
            self.update("failed")
        finally:
            release_active(self.root, self.record["id"])
        return self.record


def series(root, payload, snapshot=False):
    run = load_run(root, payload["id"])
    method = payload.get("method", "standard_dmc")
    variant = payload.get("variant", "transport")
    if method not in METHODS or variant not in ("baseline", "optimized", "transport"):
        raise ValueError("Unknown method or trajectory variant")
    return process_trends(run_dir(root, run["id"]), run, method, variant, payload, snapshot)


def execute(root, operation, payload):
    root = Path(root).resolve()
    root.mkdir(parents=True, exist_ok=True)
    if operation.startswith("twin-"):
        from research_twin import execute as twin_execute
        return twin_execute(root, operation, payload)
    if operation == "publisher-candidates":
        from research_policy_candidates import publisher_candidates
        return {"candidate_ids": publisher_candidates(root, payload["policy_id"], payload["study_id"])}
    if operation == "publisher-policies":
        from research_policy_candidates import publisher_policies
        return {"policies": publisher_policies(root)}
    if operation.startswith("policy-"):
        from research_policy import execute as policy_execute
        return policy_execute(root, operation, payload)
    if operation in ("draft-save", "draft-list"):
        from research_drafts import execute as draft_execute
        return draft_execute(root, operation, payload)
    if operation == "import-snapshot":
        from research_workbench import import_snapshot
        return import_snapshot(root, payload)
    if operation == "import-run-history":
        from research_sources import import_run_history
        return import_run_history(root, payload)
    if operation == "identify-study":
        from research_workbench import identify_study
        return identify_study(root, payload)
    if operation == "list-models":
        from research_workbench import list_models
        return {"models": list_models(root)}
    if operation == "get-model":
        from research_workbench import load_model
        return load_model(root, payload["id"])
    if operation == "select-trials":
        from research_workbench import select_trials
        return select_trials(root, payload)
    if operation in ("import", "preview", "clean"):
        from datasets import execute as dataset_execute
        return dataset_execute(root, operation, payload)
    if operation in ("create-version", "list-versions", "get-version", "version-model", "version-bundle"):
        from control_versions import create_version, get_version, get_version_bundle, get_version_model, list_versions
        if operation == "create-version":
            allowed = {"run_id", "method", "selected_by", "author_user_id", "author_identity_realm", "deployment_profile_hash", "target", "equipment_instance_ids", "operating_envelope"}
            if not {"run_id", "method", "selected_by"} <= set(payload) or set(payload) - allowed:
                raise ValueError("Unexpected version selection fields")
            return create_version(root, {key: value for key, value in payload.items() if key != "selected_by"}, payload["selected_by"])
        if operation == "list-versions":
            return {"versions": list_versions(root)}
        if operation == "get-version":
            return get_version(root, payload["id"])
        if operation == "version-bundle":
            return get_version_bundle(root, payload["id"])
        model = get_version_model(root, payload["id"])
        return {"model_json": model.decode("utf-8"), "model_sha256": hashlib.sha256(model).hexdigest()}
    if operation == "bootstrap":
        from datasets import list_datasets
        reconcile_active(root)
        studies = [load_json(path) for path in sorted((root / "studies").glob("*.json"), reverse=True)]
        runs = []
        for path in sorted((root / "runs").glob("*/run.json"), reverse=True):
            record = load_json(path)
            runs.append({key: record[key] for key in ("id", "study_id", "name", "status", "stage", "progress", "created_at")})
        from research_workbench import capabilities
        return {"schema_version": 1, "enabled": True, "capabilities": {**capabilities(), "methods": [
            {"id": "standard_dmc", "label": "標準 DMC", "description": "現有追蹤與操作軟限制"},
            {"id": "soft_band", "label": "Soft-band MPC", "description": "GuruPrasath 2011 衍生：品質帶內低敏感度；非 MHE"}]},
            "defaults": defaults(), "datasets": list_datasets(root), "studies": studies, "runs": runs}
    if operation == "draft-study":
        directory = root / "datasets" / safe_id(payload["dataset_id"])
        draft = history_draft(load_json(directory / "summary.json"), load_json(directory / "history.json"), defaults())
        return draft
    if operation == "save-study":
        study = validate_study(payload)
        if study.get("dataset_id"):
            history_path = root / "datasets" / study["dataset_id"] / "history.json"
            content = history_path.read_bytes()
            match_history(study["config"], parse_json(content))
            study["dataset_sha256"] = hashlib.sha256(content).hexdigest()
        if study.get("model_id"):
            from research_workbench import digest, load_model
            model = load_model(root, study["model_id"])
            if digest(study["config"]) != model["engineering_sha256"]:
                raise ValueError("Engineering configuration differs from frozen model; identify a new branch")
        study.update(id=uuid.uuid4().hex, created_at=now(), previous_id=payload.get("id"))
        atomic_json(root / "studies" / (study["id"] + ".json"), study)
        return study
    if operation == "create-run":
        return create_run(root, payload)
    if operation == "run":
        return ResearchRun(root, payload["id"]).execute()
    if operation == "detail":
        reconcile_active(root)
        record = load_run(root, payload["id"])
        if record["status"] not in TERMINAL and (run_dir(root, record["id"]) / "cancel.request").exists():
            record["status"] = "cancel_requested"
        if record.get("study", {}).get("policy_id"):
            from research_policy_candidates import seal_status
            record["policy_candidate"] = seal_status(root, record["id"])
        return record
    if operation == "cancel-run":
        record = load_run(root, payload["id"])
        if record["status"] not in TERMINAL:
            atomic_json(run_dir(root, record["id"]) / "cancel.request", {"requested_at": now()})
            record["status"] = "cancel_requested"
        return record
    if operation == "fail-run":
        return fail_run(root, payload)
    if operation == "series":
        return series(root, payload)
    if operation == "forecast":
        return series(root, payload, snapshot=True)
    raise ValueError("Unsupported research operation")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True)
    parser.add_argument("--operation", required=True)
    args = parser.parse_args()
    try:
        text = sys.stdin.read(40 * 1024 * 1024 + 1)
        if len(text) > 40 * 1024 * 1024:
            raise ValueError("Research request exceeds 40 MiB")
        payload = parse_json(text or "{}")
        if not isinstance(payload, dict):
            raise ValueError("Research request must be an object")
        result = execute(Path(args.root), args.operation, payload)
        print(json.dumps(result, ensure_ascii=False, allow_nan=False))
        if args.operation == "run" and result["status"] == "failed":
            return 1
        return 0
    except Exception as error:
        print(json.dumps({"error": str(error)}, ensure_ascii=False))
        return 1


if __name__ == "__main__":
    sys.exit(main())

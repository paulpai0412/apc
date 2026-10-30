"""Reproducible calendar-month experiments using the real Rust MPC and COM bridge.

All generated signals, full-resolution traces, process receipts and hashes are retained.
No measured-plant claims, automatic deployment, GPU, or production endpoint is involved.
"""
import argparse
import calendar
import copy
from datetime import datetime, timezone
import json
import math
from pathlib import Path
import re
import shutil
import subprocess

import numpy as np

from common import fingerprint, load_json, main_guard, save_json, validate_history
from identify import identify


STAGES = ("prepare", "history", "identify", "tune", "evaluate", "transport", "report")
SIGNALS = ("time_seconds", "cv", "mv", "dv", "targets", "measurement_noise", "process_noise", "gain_scale")


def iso(value):
    return value.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def parse_date(value):
    result = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if result.tzinfo is None or result.utcoffset().total_seconds() != 0:
        raise ValueError("Campaign boundaries must specify UTC")
    return result


def add_months(value, months):
    index = value.year * 12 + value.month - 1 + months
    year, month = divmod(index, 12)
    return value.replace(year=year, month=month + 1,
                         day=min(value.day, calendar.monthrange(year, month + 1)[1]))


def calendar_splits(config):
    start = parse_date(config["start"])
    boundaries = [add_months(start, month) for month in range(4)]
    dt = config["sample_time"]
    indices = [(boundary - start).total_seconds() / dt for boundary in boundaries]
    if any(not float(index).is_integer() for index in indices):
        raise ValueError("Calendar boundaries must align exactly with the scan period")
    return [{"name": name, "start": iso(boundaries[i]), "end": iso(boundaries[i + 1]),
             "samples": int(indices[i + 1] - indices[i]), "first": int(indices[i]), "last": int(indices[i + 1])}
            for i, name in enumerate(("train", "validation", "test"))]


def check_config(config):
    if config["schema_version"] != 1 or config["months"] != 3:
        raise ValueError("Campaign schema 1 requires exactly three calendar months")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,95}", config["id"]):
        raise ValueError("Campaign id must be a safe path component")
    def finite_tree(value):
        if isinstance(value, dict):
            for child in value.values():
                finite_tree(child)
        elif isinstance(value, list):
            for child in value:
                finite_tree(child)
        elif isinstance(value, (int, float)) and not math.isfinite(value):
            raise ValueError("Configuration contains a non-finite number")
    finite_tree(config)
    dt = config["sample_time"]
    if isinstance(dt, bool) or dt <= 0 or config["plant"]["sample_time"] != dt:
        raise ValueError("Positive matching plant/controller scan periods required")
    if not isinstance(config["seed"], int) or config["seed"] < 0:
        raise ValueError("Seed must be a nonnegative integer")
    splits = calendar_splits(config)
    if min(split["samples"] for split in splits) < 2:
        raise ValueError("Each calendar split needs at least two scans")
    counts = {group: len(config["variables"][group]) for group in ("cv", "mv", "dv")}
    if not counts["cv"] or not counts["mv"]:
        raise ValueError("At least one CV and MV required")
    plant = config["plant"]
    for group in counts:
        if len(plant["baseline_" + group]) != counts[group]:
            raise ValueError("Plant baseline dimensions differ from variables")
        for variable in config["variables"][group]:
            if variable["low"] >= variable["high"]:
                raise ValueError("Variable limits must be ordered")
    for key in ("actuator_tau", "actuator_low", "actuator_high", "actuator_rate"):
        if len(plant[key]) != counts["mv"]:
            raise ValueError("Actuator dimension mismatch")
    if len(plant["paths"]) != counts["cv"] or any(len(row) != counts["mv"] + counts["dv"] for row in plant["paths"]):
        raise ValueError("Truth path dimensions differ from variables")
    for row in plant["paths"]:
        for path in row:
            if min(path["tau1"], path["tau2"], path["delay_seconds"]) < 0:
                raise ValueError("Truth time constants and delays must be nonnegative")
    for key in ("nonlinear_strength", "nonlinear_scale"):
        if len(plant[key]) != counts["cv"]:
            raise ValueError("Nonlinear CV dimension mismatch")
    if any(value <= 0 for value in plant["nonlinear_scale"]):
        raise ValueError("Nonlinear scales must be positive")
    for i, variable in enumerate(config["variables"]["mv"]):
        if variable["max_move"] <= 0 or plant["actuator_tau"][i] < 0 or plant["actuator_rate"][i] <= 0:
            raise ValueError("Actuator rate and controller max_move must be positive")
        if not variable["low"] <= plant["baseline_mv"][i] <= variable["high"]:
            raise ValueError("Initial actuator outside controller limits")
        if plant["actuator_low"][i] != variable["low"] or plant["actuator_high"][i] != variable["high"]:
            raise ValueError("Physical/controller actuator ranges must match")
    scenario = config["scenario"]
    dimensions = {"mv": ("prbs_hold_samples", "mv_prbs_amplitude", "mv_ramp_amplitude", "mv_ramp_period_hours", "mv_regime_amplitude"),
                  "dv": ("dv_amplitude", "dv_period_hours", "dv_colored_std"),
                  "cv": ("process_noise_std", "sensor_noise_std", "target_amplitude")}
    for group, keys in dimensions.items():
        for key in keys:
            if len(scenario[key]) != counts[group] or any(value < 0 for value in scenario[key]):
                raise ValueError(f"Scenario {key} has invalid dimensions/sign")
    for key in ("dv_correlation", "process_correlation"):
        if not 0 <= scenario[key] < 1:
            raise ValueError("Noise correlation must be in [0,1)")
    if not 0 <= scenario["gain_variation"] < 1:
        raise ValueError("Gain variation must be in [0,1)")
    for key in ("regime_hours", "gain_period_hours", "target_regime_hours"):
        if scenario[key] <= 0:
            raise ValueError("Scenario periods must be positive")
    for key in ("prbs_hold_samples", "mv_ramp_period_hours", "dv_period_hours"):
        if any(value <= 0 for value in scenario[key]):
            raise ValueError("Excitation periods must be positive")
    if any(not isinstance(value, int) for value in scenario["prbs_hold_samples"]):
        raise ValueError("PRBS hold durations must be integer scans")
    if not isinstance(scenario["steady_samples"], int) or not 1 <= scenario["steady_samples"] < splits[0]["samples"]:
        raise ValueError("Steady initial interval must be within training month")
    ids = [candidate["id"] for candidate in config["candidates"]]
    if not 1 <= len(ids) <= 100 or len(set(ids)) != len(ids) or "baseline" in ids:
        raise ValueError("Provide 1..100 unique candidate ids, excluding baseline")
    for candidate in config["candidates"]:
        if not re.fullmatch(r"[A-Za-z0-9_-]+", candidate["id"]) or candidate["cv_weight_scale"] <= 0 or candidate["mv_weight_scale"] <= 0 or not 0 <= candidate["alpha"] < 1:
            raise ValueError("Invalid finite tuning candidate")
    if any(config["transport"][key] < 0 for key in ("absolute_tolerance", "relative_tolerance")):
        raise ValueError("Comparison tolerances must be nonnegative")
    fault = config["faults"]
    if set(fault["kinds"]) != {"bad_quality", "disconnect", "write_denied"}:
        raise ValueError("All three explicit transport faults are required")
    if not 1 <= fault["start_sample"] < fault["samples"] <= splits[0]["samples"]:
        raise ValueError("Fault interval must fit within training interval")
    return config


def colored_noise(rng, count, std, correlation):
    values = rng.normal(size=(count, len(std))) * np.asarray(std) * math.sqrt(1 - correlation ** 2)
    for index in range(1, count):
        values[index] += correlation * values[index - 1]
    return values


def generate_scenario(config):
    """Independent RNG streams keep sensor/process disturbances separate from inputs."""
    splits = calendar_splits(config)
    count = splits[-1]["last"]
    dt, settings, plant = config["sample_time"], config["scenario"], config["plant"]
    rng_input, rng_dv, rng_process, rng_sensor, rng_targets = [np.random.default_rng(seed) for seed in np.random.SeedSequence(config["seed"]).spawn(5)]
    t = np.arange(count, dtype=float) * dt
    indices = np.arange(count)
    mv = np.tile(plant["baseline_mv"], (count, 1)).astype(float)
    regime = (t / (settings["regime_hours"] * 3600)).astype(int)
    for j, variable in enumerate(config["variables"]["mv"]):
        hold = settings["prbs_hold_samples"][j]
        prbs = rng_input.choice([-1.0, 1.0], size=(count + hold - 1) // hold)[indices // hold]
        phase = t / (settings["mv_ramp_period_hours"][j] * 3600)
        ramp = 4 * np.abs(phase - np.floor(phase + 0.5)) - 1
        regimes = rng_input.uniform(-1, 1, size=regime[-1] + 1)[regime]
        mv[:, j] += prbs * settings["mv_prbs_amplitude"][j] + ramp * settings["mv_ramp_amplitude"][j] + regimes * settings["mv_regime_amplitude"][j]
        mv[:, j] = np.clip(mv[:, j], variable["low"], variable["high"])
    dv = np.tile(plant["baseline_dv"], (count, 1)).astype(float)
    dv += colored_noise(rng_dv, count, settings["dv_colored_std"], settings["dv_correlation"])
    for j, variable in enumerate(config["variables"]["dv"]):
        dv[:, j] += settings["dv_amplitude"][j] * np.sin(2 * np.pi * t / (settings["dv_period_hours"][j] * 3600))
        dv[:, j] = np.clip(dv[:, j], variable["low"], variable["high"])
    process = colored_noise(rng_process, count, settings["process_noise_std"], settings["process_correlation"])
    sensor = rng_sensor.normal(size=(count, len(plant["baseline_cv"]))) * settings["sensor_noise_std"]
    target_regime = (t / (settings["target_regime_hours"] * 3600)).astype(int)
    targets = np.tile([variable["target"] for variable in config["variables"]["cv"]], (count, 1)).astype(float)
    targets += rng_targets.uniform(-1, 1, size=(target_regime[-1] + 1, targets.shape[1]))[target_regime] * settings["target_amplitude"]
    gain = 1 + settings["gain_variation"] * np.sin(2 * np.pi * t / (settings["gain_period_hours"] * 3600))
    steady = settings["steady_samples"]
    mv[:steady], dv[:steady] = plant["baseline_mv"], plant["baseline_dv"]
    process[:steady], sensor[:steady], gain[:steady] = 0, 0, 1
    targets[:steady] = [variable["target"] for variable in config["variables"]["cv"]]
    return {"sample_time": dt, "time_seconds": t.tolist(), "cv": np.tile(plant["baseline_cv"], (count, 1)).tolist(),
            "mv": mv.tolist(), "dv": dv.tolist(), "targets": targets.tolist(),
            "measurement_noise": sensor.tolist(), "process_noise": process.tolist(), "gain_scale": gain.tolist(),
            "provenance": {"synthetic": True, "seed": config["seed"], "start": config["start"],
                           "cv_role": "Physical baseline initialization only, not measurements; Rust replay generates every observed CV",
                           "noise": "Independent seeded process/sensor/input streams; all exogenous values frozen before selection"}}


def slice_scenario(scenario, first, last):
    result = {key: value for key, value in scenario.items() if key not in SIGNALS and key != "faults"}
    for key in SIGNALS:
        if key in scenario:
            result[key] = scenario[key][first:last]
    origin = result["time_seconds"][0]
    result["time_seconds"] = [value - origin for value in result["time_seconds"]]
    result["provenance"] = {**scenario.get("provenance", {}), "source_first_sample": first,
                            "source_last_sample_exclusive": last, "initialization": "Fresh physical baseline; no future recorded CV state"}
    return result


def controller_template(config):
    """Uses engineering limits/tuning only, never truth gains, delays, or time constants."""
    groups = {}
    for group in ("cv", "mv", "dv"):
        rows = []
        for variable in config["variables"][group]:
            name = variable["name"]
            limits = {"low": variable["low"], "high": variable["high"],
                      "low_low": variable.get("hard_low", variable["low"]), "high_high": variable.get("hard_high", variable["high"])}
            if "node_ids" in variable:
                # Explicit pre-frozen profile evidence is copied exactly, never
                # merged with synthetic aliases or applied to Runtime settings.
                ids = copy.deepcopy(variable["node_ids"])
            else:
                ids = {key: f"{name}:{key}" for key in ("pv", "target", "prediction", "sp", "op", "mode", "mode_target", "future_plan")}
                ids["limits"] = {key: f"{name}:{key}" for key in ("low", "high", "ll", "hh")}
            row = {"name": name, "description": "Engineering contract for isolated research; runtime mapping requires independent profile approval", "units": variable["unit"], "limits": limits, "node_ids": ids}
            if group == "cv":
                row.update(weight=variable["weight"], alpha=variable["alpha"], ece_factor=1.0, slack_weight=variable["slack_weight"], is_integrating=False,
                           optimization_mode={"type": "Target", "value": variable["target"]})
                limits["target"] = variable["target"]
            elif group == "mv":
                row.update(weight_r=variable["weight_r"], max_move=variable["max_move"], target_weight=0.0)
            rows.append(row)
        groups[group + "s"] = rows
    return {"metadata": {"name": config["id"], "version": "1.0", "model_type": "step_response", "description": config["description"]},
            "tuning": {**config["controller"], "sample_time": config["sample_time"]}, "variables": groups, "physics": {}}


def tuned_model(baseline, parameters):
    model = copy.deepcopy(baseline)
    for cv in model["variables"]["cvs"]:
        cv["weight"] *= parameters["cv_weight_scale"]
        cv["alpha"] = parameters["alpha"]
    for mv in model["variables"]["mvs"]:
        mv["weight_r"] *= parameters["mv_weight_scale"]
    return model


def trace_metrics(trace, config, first=0, last=None):
    """Physical full-resolution metrics; no tuned weights enter the score."""
    cv = np.asarray(trace["cv"][first:last], dtype=float)
    target = np.asarray(trace["targets"][first:last], dtype=float)
    mv = np.asarray(trace["mv"][first:last], dtype=float)
    command = np.asarray(trace["command_mv"][first:last], dtype=float)
    if not len(cv) or not all(np.isfinite(array).all() for array in (cv, target, mv, command)):
        raise ValueError("Trace has no finite complete samples")
    cvs, mvs = config["variables"]["cv"], config["variables"]["mv"]
    cv_low, cv_high = np.array([v["low"] for v in cvs]), np.array([v["high"] for v in cvs])
    mv_low, mv_high = np.array([v["low"] for v in mvs]), np.array([v["high"] for v in mvs])
    error = cv - target
    violation = np.maximum(cv_low - cv, 0) + np.maximum(cv - cv_high, 0)
    delta = np.diff(command, axis=0)
    movement = np.mean((delta / (mv_high - mv_low)) ** 2) if len(delta) else 0.0
    score = float(np.mean((error / (cv_high - cv_low)) ** 2) + np.mean((violation / (cv_high - cv_low)) ** 2) + movement)
    hard = np.sum((command < mv_low - 1e-8) | (command > mv_high + 1e-8) | (mv < mv_low - 1e-8) | (mv > mv_high + 1e-8), axis=0)
    hard += np.sum(np.abs(delta) > np.array([v["max_move"] for v in mvs]) + 1e-7, axis=0)
    metrics = {"score": score, "cv_rmse": np.sqrt(np.mean(error ** 2, axis=0)).tolist(),
               "cv_iae": (np.sum(np.abs(error), axis=0) * config["sample_time"]).tolist(),
               "cv_violation_seconds": (np.sum(violation > 0, axis=0) * config["sample_time"]).tolist(),
               "mv_total_variation": np.sum(np.abs(delta), axis=0).tolist(), "mv_hard_violations": hard.tolist(), "samples": len(cv)}
    if "solve_ms" in trace and "solver_status" in trace:
        timings = np.asarray(trace["solve_ms"][first:last], dtype=float)
        statuses = trace["solver_status"][first:last]
        if len(timings) != len(cv) or len(statuses) != len(cv) or not np.isfinite(timings).all() or np.any(timings < 0):
            raise ValueError("Invalid per-scan solver telemetry")
        metrics.update(solver_calls=len(timings), solver_failures=sum(status not in ("Solved", "AlmostSolved") for status in statuses),
                       mean_solve_ms=float(np.mean(timings)), max_solve_ms=float(np.max(timings)))
    return metrics


def validate_result(result, scenario, config):
    trace = result.get("trace", {})
    count = len(scenario["time_seconds"])
    if result.get("status") != "valid":
        return False, result.get("reason", "Runtime marked result invalid")
    for key, width in (("cv", len(config["variables"]["cv"])), ("mv", len(config["variables"]["mv"])),
                       ("dv", len(config["variables"]["dv"])), ("targets", len(config["variables"]["cv"])),
                       ("command_mv", len(config["variables"]["mv"]))):
        array = np.asarray(trace.get(key, []), dtype=float)
        if array.shape != (count, width) or not np.isfinite(array).all():
            return False, f"Incomplete/non-finite {key} trace"
    if trace.get("time_seconds") != scenario["time_seconds"]:
        return False, "Trace scan timeline differs from requested scenario"
    solver = result.get("solver", {})
    if solver.get("calls") != count or solver.get("failures") != 0:
        return False, "Missing solver calls or solver failures"
    if any(trace_metrics(trace, config)["mv_hard_violations"]):
        return False, "Actuator range/move constraint violations"
    return True, "All requested scans completed, finite traces, solver calls and actuator constraints checked"


class Campaign:
    def __init__(self, args):
        self.config_path = Path(args.config).resolve()
        self.config = check_config(load_json(self.config_path))
        self.root = Path(args.output_root).resolve() / self.config["id"]
        self.root.mkdir(parents=True, exist_ok=True)
        self.resume = args.resume
        resolved = shutil.which(args.binary)
        self.binary = str(Path(resolved or args.binary).resolve())
        self.bridge = args.bridge
        self.bridge_args = args.bridge_arg
        self.splits = calendar_splits(self.config)
        self.identity = {"config_sha256": fingerprint(self.config_path),
                         "source_sha256": {name: fingerprint(Path(__file__).with_name(name)) for name in ("campaign.py", "identify.py", "common.py")}}
        manifest = self.root / "experiment.json"
        if manifest.exists():
            if load_json(manifest)["identity"] != self.identity:
                raise ValueError("Existing run has different configuration/code; use a new experiment id/output root")
            if not self.resume:
                raise ValueError("Run already exists; use --resume for checked reuse or a new output root")
        else:
            save_json(manifest, {"identity": self.identity, "created_at": iso(datetime.now(timezone.utc)), "config": self.config})

    def path(self, name):
        return self.root / name

    def stage(self, name, inputs, outputs, action, extra=None):
        signature = {"identity": self.identity, "inputs": {str(path.relative_to(self.root)) if path.is_relative_to(self.root) else str(path): fingerprint(path) for path in inputs}, "extra": extra}
        receipt_path = self.path(name + ".receipt.json")
        if receipt_path.exists():
            receipt = load_json(receipt_path)
            if receipt["signature"] != signature:
                raise ValueError(f"{name}: resume inputs changed; choose a new run directory")
            for output, expected in receipt["outputs"].items():
                if fingerprint(self.path(output)) != expected:
                    raise ValueError(f"{name}: artifact hash mismatch: {output}")
            return
        if any(path.exists() for path in outputs):
            raise ValueError(f"{name}: unreceipted partial output exists; preserve it and use a new run directory")
        action()
        save_json(receipt_path, {"signature": signature, "outputs": {str(path.relative_to(self.root)): fingerprint(path) for path in outputs}, "completed_at": iso(datetime.now(timezone.utc))})

    def prepare(self):
        def action():
            scenario = generate_scenario(self.config)
            save_json(self.path("plant.json"), self.config["plant"])
            save_json(self.path("scenario.json"), scenario)
            selection = self.splits[1]
            save_json(self.path("selection-scenario.json"), slice_scenario(scenario, selection["first"], selection["last"]))
        self.stage("prepare", [], [self.path(name) for name in ("plant.json", "scenario.json", "selection-scenario.json")], action)

    def runtime(self, name, mode, scenario_name, controller_name=None, transport=False):
        scenario_path, output_path = self.path(scenario_name), self.path(name + ".json")
        command = [self.binary, mode, "--plant", str(self.path("plant.json")), "--scenario", str(scenario_path), "--output", str(output_path)]
        inputs = [Path(self.binary), self.path("plant.json"), scenario_path]
        if controller_name:
            inputs.append(self.path(controller_name))
            command += ["--controller", str(self.path(controller_name))]
        if transport:
            if not self.bridge:
                raise ValueError("OPC DA stage requires --bridge and optional repeated --bridge-arg")
            bridge_executable = shutil.which(self.bridge) or self.bridge
            inputs.append(Path(bridge_executable).resolve())
            command += ["--transport", "opc-da", "--bridge", self.bridge]
            for argument in self.bridge_args:
                command += ["--bridge-arg", argument]
                if Path(argument).is_file():
                    inputs.append(Path(argument).resolve())
        command_path = self.path(name + ".command.json")
        stdout_path, stderr_path = self.path(name + ".stdout.log"), self.path(name + ".stderr.log")
        def action():
            started = iso(datetime.now(timezone.utc))
            with stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
                result = subprocess.run(command, stdout=stdout, stderr=stderr, check=False)
            save_json(command_path, {"argv": command, "returncode": result.returncode, "started_at": started, "completed_at": iso(datetime.now(timezone.utc))})
            if not output_path.exists():
                raise ValueError(f"{name}: runtime exited {result.returncode} without a trace; see {stderr_path}")
        self.stage(name, inputs, [output_path, command_path, stdout_path, stderr_path], action, {"command": command})
        return load_json(output_path), load_json(command_path)

    def history(self):
        history, command = self.runtime("history", "replay", "scenario.json")
        if command["returncode"] != 0:
            raise ValueError("Historical Rust replay did not complete successfully")
        validate_history(history, controller_template(self.config))
        if len(history["time_seconds"]) != self.splits[-1]["last"]:
            raise ValueError("Historical replay omitted requested scans")

    def identification(self):
        def action():
            history = load_json(self.path("history.json"))
            validate_history(history, controller_template(self.config))
            history["mv"] = history["command_mv"]
            options = dict(self.config["identification"])
            minimum_std = np.asarray(options.pop("minimum_input_std"))
            train_end, val_end = self.splits[0]["last"], self.splits[1]["last"]
            inputs = np.concatenate((history["mv"][:train_end], history["dv"][:train_end]), axis=1)
            std = inputs.std(axis=0)
            if std.shape != minimum_std.shape or np.any(std < minimum_std):
                raise ValueError("Training physical excitation below configured standard-deviation thresholds")
            model, report = identify(controller_template(self.config), history, split_indices=(train_end, val_end), **options)
            report["excitation"] = {"input_std": std.tolist(), "minimum_std": minimum_std.tolist(), "passed": bool(np.all(std >= minimum_std)), "coordinate": "command_mv plus measured DV; actuator dynamics included in identified response"}
            report["input_coordinate"] = "Scheduled actuator commands; raw history.mv remains actual measured actuator"
            report["truth_access"] = "FIR estimator receives history and engineering model template, never truth path coefficients"
            report["warmup_excluded_samples"] = report["selection"]["memory"]
            save_json(self.path("baseline-controller.json"), model)
            save_json(self.path("identification.json"), report)
        self.stage("identify", [self.path("history.json")], [self.path("baseline-controller.json"), self.path("identification.json")], action)

    def tune(self):
        baseline = load_json(self.path("baseline-controller.json"))
        models = [("baseline", {}, "baseline-controller.json")]
        for parameters in self.config["candidates"]:
            name = "candidate-" + parameters["id"] + ".json"
            self.stage("model-" + parameters["id"], [self.path("baseline-controller.json")], [self.path(name)], lambda p=parameters, n=name: save_json(self.path(n), tuned_model(baseline, p)), parameters)
            models.append((parameters["id"], parameters, name))
        records = []
        scenario = load_json(self.path("selection-scenario.json"))
        for candidate_id, parameters, name in models:
            result, command = self.runtime("selection-" + candidate_id, "closed-loop", "selection-scenario.json", name)
            valid, reason = validate_result(result, scenario, self.config)
            valid = valid and command["returncode"] == 0
            records.append({"id": candidate_id, "parameters": parameters, "status": "valid" if valid else "invalid", "reason": reason,
                            "score": trace_metrics(result["trace"], self.config)["score"] if valid else None, "controller": name,
                            "samples": len(result.get("trace", {}).get("time_seconds", []))})
        valid = [record for record in records if record["status"] == "valid"]
        if not valid:
            save_json(self.path("failed-trials.json"), records)
            raise ValueError("No valid full-selection-month candidate; failures preserved")
        winner = min(valid, key=lambda record: record["score"])
        inputs = [self.path("selection-" + candidate_id + ".json") for candidate_id, _, _ in models]
        def action():
            save_json(self.path("trials.json"), {"trials": records, "selected": winner["id"], "policy": "Minimum fixed dimensionless full second-calendar-month score; baseline is eligible; March never used"})
            save_json(self.path("optimized-controller.json"), load_json(self.path(winner["controller"])))
        self.stage("selection", inputs, [self.path("trials.json"), self.path("optimized-controller.json")], action)

    def evaluate(self):
        for run, controller in (("baseline", "baseline-controller.json"), ("optimized", "optimized-controller.json")):
            self.runtime(run, "closed-loop", "scenario.json", controller)

    def transport(self):
        self.runtime("transport", "closed-loop", "scenario.json", "optimized-controller.json", transport=True)
        scenario = load_json(self.path("scenario.json"))
        faults = self.config["faults"]
        for kind in faults["kinds"]:
            name = "fault-" + kind
            scenario_name = name + "-scenario.json"
            def action(k=kind, n=scenario_name):
                sliced = slice_scenario(scenario, 0, faults["samples"])
                # Disconnection is server-wide; item selection is only valid for item faults.
                items = [] if k == "disconnect" else ["Command:0" if k == "write_denied" else "CV:0"]
                sliced["faults"] = [{"sample": faults["start_sample"], "kind": k, "items": items}]
                save_json(self.path(n), sliced)
            self.stage(name + "-scenario", [self.path("scenario.json")], [self.path(scenario_name)], action)
            self.runtime(name, "closed-loop", scenario_name, "optimized-controller.json", transport=True)
            recovery_name = "recovery-" + kind + "-scenario.json"
            self.stage("recovery-" + kind + "-scenario", [self.path("scenario.json")], [self.path(recovery_name)],
                       lambda n=recovery_name: save_json(self.path(n), slice_scenario(scenario, 0, faults["samples"])))
            self.runtime("recovery-" + kind, "closed-loop", recovery_name, "optimized-controller.json", transport=True)

    def report(self):
        build_report(self)


def build_report(campaign):
    config, root = campaign.config, campaign.root
    checks, runs = [], []
    def check(name, passed, detail):
        checks.append({"name": name, "status": "not_run" if passed is None else "passed" if passed else "failed", "detail": detail})
    scenario = load_json(campaign.path("scenario.json")) if campaign.path("scenario.json").exists() else None
    check("complete_calendar_scenario", len(scenario["time_seconds"]) == campaign.splits[-1]["last"] if scenario else None,
          "Exactly three calendar months, every configured scan; no shortened evaluation")
    receipt_failures = []
    receipts = sorted(root.glob("*.receipt.json"))
    for path in receipts:
        receipt = load_json(path)
        if receipt["signature"]["identity"] != campaign.identity:
            receipt_failures.append(path.name + ": configuration/code identity")
        for input_path, expected in receipt["signature"]["inputs"].items():
            source = Path(input_path) if Path(input_path).is_absolute() else campaign.path(input_path)
            if not source.is_file() or fingerprint(source) != expected:
                receipt_failures.append(input_path)
        for output, expected in receipt["outputs"].items():
            if not campaign.path(output).is_file() or fingerprint(campaign.path(output)) != expected:
                receipt_failures.append(output)
    check("artifact_hashes", not receipt_failures if receipts else None, "All completed-stage output SHA-256 hashes checked" if not receipt_failures else ", ".join(receipt_failures))
    history_path, history_command = campaign.path("history.json"), campaign.path("history.command.json")
    history_complete = None
    if history_path.exists() and history_command.exists() and scenario:
        history = load_json(history_path)
        history_complete = (load_json(history_command)["returncode"] == 0
                            and campaign.path("history.receipt.json").exists()
                            and history.get("time_seconds") == scenario["time_seconds"])
        try:
            validate_history(history, controller_template(config))
        except (ValueError, KeyError, TypeError):
            history_complete = False
    check("historical_replay_complete", history_complete, "Actual Rust replay process completion and full finite historical timeline required")
    identification = load_json(campaign.path("identification.json")) if campaign.path("identification.json").exists() else {}
    check("identification_excitation", identification.get("excitation", {}).get("passed"), "Physical standard deviations and normalized FIR rank/condition checked on training month only")
    check("identification_finite_metrics", all(np.isfinite(identification[key]["rmse"]).all() for key in ("train", "validation", "test", "full")) if identification else None,
          "Prediction and persistence metrics are computed, not an assumed improvement; first FIR-memory rows excluded")
    trial_data = load_json(campaign.path("trials.json")) if campaign.path("trials.json").exists() else {}
    trials = trial_data.get("trials", [])
    check("full_month_selection", all(row["samples"] == campaign.splits[1]["samples"] and row["status"] == "valid" for row in trials) if trials else None,
          "Each finite candidate and baseline evaluated on every scan of the selection month; invalid trials remain failures")
    results = {}
    for name, kind, transport in (("baseline", "baseline", "direct"), ("optimized", "optimized", "direct"), ("transport", "transport", "opc-da")):
        path = campaign.path(name + ".json")
        if not path.exists() or scenario is None:
            check(name + "_complete", None, "Required full timeline run has not completed")
            continue
        result = load_json(path)
        results[name] = result
        valid, reason = validate_result(result, scenario, config)
        command_path = campaign.path(name + ".command.json")
        command_ok = command_path.exists() and load_json(command_path)["returncode"] == 0 and campaign.path(name + ".receipt.json").exists()
        check(name + "_complete", valid and command_ok, reason + "; process receipt required")
        trace = result.get("trace", {})
        if len(trace.get("time_seconds", [])):
            metrics = trace_metrics(trace, config)
            solver = result.get("solver", {})
            metrics.update(solver_calls=solver.get("calls", 0), solver_failures=solver.get("failures", 0), mean_solve_ms=solver.get("mean_solve_ms", 0), max_solve_ms=solver.get("max_solve_ms", 0))
            test = campaign.splits[2]
            test_metrics = trace_metrics(trace, config, test["first"], test["last"]) if len(trace["time_seconds"]) >= test["last"] else None
            runs.append({"id": name, "label": {"baseline": "Identified model / conservative baseline", "optimized": "Selection-month chosen tuning", "transport": "Chosen tuning / native COM DA"}[name],
                         "kind": kind, "transport": transport, "status": "valid" if valid and command_ok else "invalid", "reason": reason, "metrics": metrics,
                         "test_metrics": test_metrics, "samples": len(trace["time_seconds"]), "trace_file": name + ".json"})
    if "optimized" in results and "transport" in results:
        tolerance = config["transport"]
        differences, equivalent = {}, True
        for key in ("time_seconds", "cv", "mv", "dv", "targets", "command_mv"):
            direct = np.asarray(results["optimized"].get("trace", {}).get(key, []))
            com = np.asarray(results["transport"].get("trace", {}).get(key, []))
            same_shape = direct.shape == com.shape and direct.size > 0
            equivalent &= same_shape and np.allclose(direct, com, atol=tolerance["absolute_tolerance"], rtol=tolerance["relative_tolerance"])
            differences[key] = float(np.max(np.abs(direct - com))) if same_shape else None
        check("direct_com_equivalence", bool(equivalent), json.dumps({"maximum_absolute_differences": differences, **tolerance}))
    else:
        check("direct_com_equivalence", None, "Requires both full direct and native COM DA results")
    fault_evidence = []
    for kind in config["faults"]["kinds"]:
        path = campaign.path("fault-" + kind + ".json")
        recovery_path = campaign.path("recovery-" + kind + ".json")
        if not path.exists() or not recovery_path.exists():
            check("fault_" + kind, None, "Actual COM fault rejection and process-restart recovery have not both run")
            continue
        fault, recovery = load_json(path), load_json(recovery_path)
        assertions = fault.get("fault_assertions", [])
        fault_command_path = campaign.path("fault-" + kind + ".command.json")
        observed = (fault.get("status") == "invalid" and bool(assertions)
                    and fault_command_path.exists() and load_json(fault_command_path)["returncode"] == 1
                    and campaign.path("fault-" + kind + ".receipt.json").exists())
        # A random crash is not a successful fault test: match the injected event and actual detection.
        assertion_passed = all(isinstance(item, dict) and item.get("detected") is True
                               and item.get("kind") == kind and item.get("sample") == config["faults"]["start_sample"]
                               for item in assertions)
        assertion_passed = assertion_passed and all(item.get("writes_after_detection") == 0 and item.get("command_blocked") is True
                                                   for item in assertions if isinstance(item, dict))
        stopped = len(fault.get("trace", {}).get("time_seconds", [])) <= config["faults"]["start_sample"] + 1
        recovery_scenario = load_json(campaign.path("recovery-" + kind + "-scenario.json"))
        recovered, reason = validate_result(recovery, recovery_scenario, config)
        recovery_command = load_json(campaign.path("recovery-" + kind + ".command.json"))
        check("fault_" + kind, observed and assertion_passed and stopped and recovered and recovery_command["returncode"] == 0
              and campaign.path("recovery-" + kind + ".receipt.json").exists(),
              f"Runtime assertions={assertions}; recovery is a fresh bridge/controller process, not transparent reconnection. {reason}")
        fault_evidence.append({"kind": kind, "assertions": assertions, "failure_reason": fault.get("reason"), "recovery": "process restart", "recovered": recovered})
    artifacts = [{"name": path.name, "path": path.name, "sha256": fingerprint(path), "bytes": path.stat().st_size}
                 for path in sorted(root.iterdir()) if path.is_file() and path.name not in ("report.json", "index.json")]
    artifact_hashes = {row["path"]: row["sha256"] for row in artifacts}
    status = "passed" if checks and all(row["status"] == "passed" for row in checks) else "failed" if any(row["status"] == "failed" for row in checks) else "incomplete"
    experiment = load_json(campaign.path("experiment.json"))
    report = {"schema_version": 1, "id": config["id"], "title": config["title"], "created_at": experiment["created_at"], "status": status,
              "synthetic": True, "description": config["description"],
              "period": {"start": campaign.splits[0]["start"], "end": campaign.splits[-1]["end"], "sample_time": config["sample_time"], "samples": campaign.splits[-1]["last"]},
              "provenance": {"seed": config["seed"], **campaign.identity, "history_sha256": artifact_hashes.get("history.json"), "plant_sha256": artifact_hashes.get("plant.json"),
                             "controller_sha256": artifact_hashes.get("optimized-controller.json"), "runtime_sha256": fingerprint(campaign.binary) if Path(campaign.binary).exists() else None,
                             "score_definition": "mean((CV-target)/CV_range)^2 + mean(outside_CV_range/CV_range)^2 + mean(delta_command/MV_range)^2; independent of tuned weights",
                             "warmup": "Closed-loop metrics include every scan; identification excludes first selected-memory rows only"},
              "splits": [{key: value for key, value in split.items() if key not in ("first", "last")} for split in campaign.splits],
              "variables": config["variables"], "identification": identification, "runs": runs, "trials": trials, "checks": checks,
              "fault_evidence": fault_evidence,
              "limitations": ["All plant parameters and histories are synthetic assumptions, not measured cement grinding data.",
                              "Stable finite-memory FIR is an approximation of the nonlinear, gain-varying, noisy generator.",
                              "One-step persistence uses the previous measured CV; FIR uses input history only. Their information sets differ.",
                              "Selection uses the entire second calendar month from a fresh baseline; final runs are uninterrupted three-month trajectories.",
                              "The third month is untouched by model/tuning selection; held-out performance can worsen and is reported without a guaranteed improvement.",
                              "Native synchronous local COM DA interoperability under Wine is not ABB hardware, remote DCOM, or industrial safety certification.",
                              "Fault recovery here means a new server/controller process; automatic in-process reconnect is not claimed."], "artifacts": artifacts}
    save_json(campaign.path("report.json"), report)
    save_json(campaign.path("index.json"), {"reports": [{key: report[key] for key in ("id", "title", "created_at", "status", "synthetic")} | report["period"]]})
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", default=str(Path(__file__).with_name("examples") / "cement_campaign.json"))
    parser.add_argument("--output-root", default=str(Path(__file__).with_name("runs")))
    parser.add_argument("--binary", default="target/release/mpc_campaign")
    parser.add_argument("--bridge", help="Native COM bridge launcher, e.g. wine")
    parser.add_argument("--bridge-arg", action="append", default=[], help="Repeated bridge argument; use --bridge-arg=VALUE for leading hyphens")
    parser.add_argument("--stage", choices=("all",) + STAGES, default="all")
    parser.add_argument("--resume", action="store_true", help="Reuse only completed stages whose configuration/input/output hashes match")
    args = parser.parse_args()
    if args.stage in ("all", "transport") and not args.bridge:
        parser.error("--bridge is required for actual native COM DA evaluation")
    campaign = Campaign(args)
    stages = STAGES if args.stage == "all" else (args.stage,)
    try:
        for stage in stages:
            getattr(campaign, "identification" if stage == "identify" else stage)()
    except (ValueError, KeyError, TypeError, OSError):
        # Preserve failures and make partial evidence visible in the original HMI.
        if campaign.path("scenario.json").exists():
            build_report(campaign)
        raise
    if args.stage in ("all", "report"):
        report = load_json(campaign.path("report.json"))
        print(json.dumps({"report": str(campaign.path("report.json")), "status": report["status"]}))
        if report["status"] != "passed":
            raise SystemExit(1)


if __name__ == "__main__":
    main_guard(main)

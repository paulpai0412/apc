"""Engineering study contracts independent of the synthetic cement example."""
import copy
import math
import re
from datetime import datetime, timedelta, timezone

import numpy as np

from campaign import calendar_splits, controller_template
from common import validate_history
from research_evaluation import objective_defaults
from research_plant import modelling_settings


PROCESS_AREAS = ("raw_grinding", "kiln", "cooler", "cement_grinding", "custom")


def finite(value, label):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        raise ValueError(f"{label} must be a finite engineering value")
    return value


def validate_engineering(config):
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,95}", config.get("id", "")):
        raise ValueError("Give the process a safe model identifier")
    if finite(config.get("sample_time"), "sample_time") <= 0:
        raise ValueError("Sample time must be positive")
    if config.get("process_area", "custom") not in PROCESS_AREAS:
        raise ValueError("Unknown cement process area")
    if config.get("model_family", "fir") not in ("fir", "fopdt", "sopdt"):
        raise ValueError("Control model family must be FIR, FOPDT or SOPDT")
    if config.get("evaluation_model_family", "sopdt") not in ("fopdt", "sopdt"):
        raise ValueError("Native evaluation plant supports FOPDT or SOPDT paths, not FIR plant recurrence")
    if config.get("mv_coordinate") != "pid_setpoint":
        raise ValueError("This control path requires effective lower-PID setpoint history, not PV or actuator OP")
    if config.get("engineering_confirmed") is not True:
        raise ValueError("Confirm engineering units, targets, operating limits and MV move limits; observed data ranges are not equipment safety limits")
    variables = config["variables"]
    if not variables["cv"] or not variables["mv"]:
        raise ValueError("At least one explicitly mapped CV and MV is required")
    names, physical_nodes = set(), set()
    for role in ("cv", "mv", "dv"):
        for variable in variables[role]:
            name = variable.get("name")
            if not isinstance(name, str) or not name.strip() or len(name) > 128 or name in names:
                raise ValueError("Signal names must be nonempty, unique across CV/MV/DV, and at most 128 characters")
            names.add(name)
            if not isinstance(variable.get("unit"), str) or not variable["unit"].strip():
                raise ValueError(f"{name}: specify engineering units; use 1 for dimensionless values")
            if variable.get("source_column") is not None and (not isinstance(variable["source_column"], str) or not variable["source_column"].strip()):
                raise ValueError(f"{name}: source_column must be an exact nonempty raw field, not an inferred tag")
            if variable.get("measurement_kind") not in (None, "fast", "delayed_lab", "estimated"):
                raise ValueError(f"{name}: measurement_kind must be fast, delayed_lab or estimated")
            if variable.get("measurement_delay_seconds") is not None and finite(variable["measurement_delay_seconds"], f"{name} measurement delay") < 0:
                raise ValueError(f"{name}: measurement delay cannot be negative")
            if role == "cv" and (variable.get("measurement_kind") in ("delayed_lab", "estimated") or (variable.get("measurement_delay_seconds") or 0) > 0):
                raise ValueError(f"{name}: causal delayed-lab/estimator control qualification is unavailable; save an engineering draft or use these values for supervision, not fast control CV feedback")
            low, high = (finite(variable.get(key), f"{name} {key}") for key in ("low", "high"))
            if low >= high:
                raise ValueError(f"{name}: operating limits must be ordered")
            hard_low = finite(variable.get("hard_low", low), f"{name} hard_low")
            hard_high = finite(variable.get("hard_high", high), f"{name} hard_high")
            if not hard_low <= low < high <= hard_high:
                raise ValueError(f"{name}: operating limits must remain inside the explicit safety envelope")
            if "node_ids" in variable:
                nodes = variable["node_ids"]
                required = {"cv": {"pv", "target", "prediction", "limits"},
                            "mv": {"pv", "sp", "op", "mode", "mode_target", "future_plan", "limits"},
                            "dv": {"pv", "limits"}}[role]
                optional = {"applied_sp", "target"} if role == "mv" else set()
                if not isinstance(nodes, dict) or not required <= set(nodes) or set(nodes) - required - optional:
                    raise ValueError(f"{name}: supply the exact role-specific profile node_ids; no synthesized aliases")
                limits = nodes["limits"]
                allowed_limits = {"low", "high"} if role == "dv" else {"low", "high", "ll", "hh"}
                if not isinstance(limits, dict) or not {"low", "high"} <= set(limits) or set(limits) - allowed_limits:
                    raise ValueError(f"{name}: invalid profile limit node mapping")
                identifiers = [value for key, value in nodes.items() if key != "limits"] + list(limits.values())
                if any(not isinstance(value, str) or not 1 <= len(value.strip()) <= 512 for value in identifiers):
                    raise ValueError(f"{name}: node IDs must be explicit bounded nonempty strings")
                physical = nodes["sp" if role == "mv" else "pv"]
                if physical in physical_nodes:
                    raise ValueError(f"{name}: duplicate physical input/output mapping")
                physical_nodes.add(physical)
                if role == "mv" and nodes.get("applied_sp") == nodes["sp"]:
                    raise ValueError(f"{name}: shared SP readback is not independent applied-SP evidence")
            if role == "cv":
                target = finite(variable.get("target"), f"{name} target")
                if not low <= target <= high:
                    raise ValueError(f"{name}: target must be within operating limits")
                if variable.get("is_integrating", False):
                    raise ValueError(f"{name}: settled FIR/SOPDT identification cannot qualify an integrating process")
                for field in ("weight", "slack_weight"):
                    if finite(variable.get(field), f"{name} {field}") <= 0:
                        raise ValueError(f"{name}: {field} must be positive")
                if not 0 <= finite(variable.get("alpha"), f"{name} alpha") < 1:
                    raise ValueError(f"{name}: alpha must be in [0, 1)")
                specs = [variable.get(field) for field in ("spec_low", "spec_high")]
                for spec in specs:
                    if spec is not None:
                        finite(spec, f"{name} quality specification")
                if all(spec is not None for spec in specs) and specs[0] >= specs[1]:
                    raise ValueError(f"{name}: quality specifications must be ordered")
            elif role == "mv":
                for field in ("max_move", "weight_r"):
                    if finite(variable.get(field), f"{name} {field}") <= 0:
                        raise ValueError(f"{name}: {field} must be positive")
    tuning = config["controller"]
    horizon = tuning.get("prediction_horizon")
    moves = tuning.get("control_horizon")
    if isinstance(horizon, bool) or not isinstance(horizon, int) or not 3 <= horizon <= 1024:
        raise ValueError("Prediction horizon must be 3–1024 scans")
    if isinstance(moves, bool) or not isinstance(moves, int) or not 1 <= moves <= horizon:
        raise ValueError("Control horizon must be within the prediction horizon")
    iterations = tuning.get("max_iterations")
    if isinstance(iterations, bool) or not isinstance(iterations, int) or not 1 <= iterations <= 10000:
        raise ValueError("Solver iteration limit must be 1–10000")
    if finite(tuning.get("solver_tolerance"), "solver_tolerance") <= 0 or finite(tuning.get("terminal_weight_factor"), "terminal_weight_factor") < 0:
        raise ValueError("Invalid solver tolerance or terminal weight")
    thresholds = config["identification"].get("minimum_input_std", [])
    if len(thresholds) != len(variables["mv"]) + len(variables["dv"]) or any(finite(value, "minimum_input_std") <= 0 for value in thresholds):
        raise ValueError("Provide a positive excitation threshold for every MV and DV")
    for key in ("absolute_tolerance", "relative_tolerance"):
        if finite(config["transport"].get(key), key) < 0:
            raise ValueError("Transport comparison tolerances must be nonnegative")
    return config


def study_splits(config):
    window = config.get("data_window")
    if window is None:
        return calendar_splits(config)
    boundaries = [0, window["train_end"], window["validation_end"], window["samples"]]
    if any(isinstance(value, bool) or not isinstance(value, int) for value in boundaries) or any(b - a < 2 for a, b in zip(boundaries, boundaries[1:])):
        raise ValueError("Dataset chronological splits must contain at least two samples each")
    dt = finite(config["sample_time"], "sample_time")
    origin = None
    if config["start"] != "relative":
        try:
            origin = datetime.fromisoformat(str(config["start"]).replace("Z", "+00:00"))
        except ValueError as error:
            raise ValueError("Study start must be relative or an ISO timestamp with a UTC offset") from error
        if origin.tzinfo is None:
            raise ValueError("Absolute study start must include its UTC offset")
        origin = origin.astimezone(timezone.utc)
    def label(index):
        return (origin + timedelta(seconds=index * dt)).isoformat() if origin else f"t+{index * dt:g}s"
    return [{"name": name, "first": first, "last": last, "samples": last - first,
             "start": label(first), "end": label(last)}
            for name, first, last in zip(("train", "validation", "test"), boundaries, boundaries[1:])]


def match_history(config, history):
    if history.get("usable") is not True:
        raise ValueError("Choose a materialized dataset without unresolved gaps")
    validate_history(history, controller_template(config))
    splits = study_splits(config)
    if len(history["time_seconds"]) != splits[-1]["last"]:
        raise ValueError("Dataset length differs from the frozen study interval")
    cleaning = history.get("provenance", {}).get("cleaning", {})
    for field, split in (("train_end", splits[0]), ("validation_end", splits[1])):
        if cleaning.get(field) != split["last"] * config["sample_time"]:
            raise ValueError(f"Dataset {field} differs from the study split; reclean rather than reuse future-derived cleaning statistics")
    for role in ("cv", "mv", "dv"):
        actual = [(signal["name"], signal.get("unit", "")) for signal in history.get("signals", []) if signal["role"] == role]
        expected = [(signal["name"], signal["unit"]) for signal in config["variables"][role]]
        if actual != expected:
            raise ValueError(f"Cleaned {role} names/order/units differ from the engineering model")
        mapped = {signal["name"]: signal["column"] for signal in cleaning.get("signals", []) if signal["role"] == role}
        for signal in config["variables"][role]:
            if signal.get("source_column") is not None and signal["source_column"] != mapped.get(signal["name"]):
                raise ValueError(f"{signal['name']}: raw source_column differs from frozen cleaning lineage")
    return splits


def history_draft(dataset, history, defaults):
    """Observed training ranges are editable suggestions, never approved limits."""
    if history.get("usable") is not True:
        raise ValueError("Clean unresolved data gaps before creating a study")
    dt = finite(history["sample_time"], "sample_time")
    cleaning = history["provenance"]["cleaning"]
    endpoints = [finite(cleaning[key], key) / dt for key in ("train_end", "validation_end")]
    if any(not value.is_integer() for value in endpoints):
        raise ValueError("Cleaning split boundaries must align to scans")
    train_end, validation_end = map(int, endpoints)
    if not 2 <= train_end < validation_end < len(history["time_seconds"]):
        raise ValueError("Training, validation and test intervals must all contain data")
    config = {key: copy.deepcopy(defaults["campaign"][key]) for key in ("controller", "identification", "transport")}
    config.update(id="process-" + dataset["id"], title=dataset["name"], description="Observational history model; engineering limits require explicit review",
                  sample_time=dt, start="relative" if cleaning.get("time_format") == "seconds" else str(history["provenance"].get("time_origin", "relative")),
                  process_area="custom", engineering_confirmed=False, plant_source="identified_history", mv_coordinate="pid_setpoint",
                  data_window={"samples": len(history["time_seconds"]), "train_end": train_end, "validation_end": validation_end})
    variables, ranges, thresholds = {}, [], []
    for role in ("cv", "mv", "dv"):
        signals = [signal for signal in history["signals"] if signal["role"] == role]
        values = np.asarray(history[role][:train_end], dtype=float)
        if values.shape != (train_end, len(signals)) or not np.isfinite(values).all():
            raise ValueError(f"Training {role} values are incomplete")
        variables[role] = []
        for index, signal in enumerate(signals):
            column = values[:, index]
            low, high = float(column.min()), float(column.max())
            if low == high:
                raise ValueError(f"{signal['name']}: training data has no variation; collect excitation before identification")
            span = high - low
            variable = {"name": signal["name"], "unit": signal["unit"], "low": low, "high": high}
            if role == "cv":
                variable.update(target=float(np.median(column)), weight=1.0 / span ** 2, alpha=0.3, slack_weight=100.0)
            elif role == "mv":
                delta = np.abs(np.diff(column))
                variable.update(max_move=float(np.quantile(delta[delta > 0], 0.95)), weight_r=1.0 / span ** 2)
            if role != "cv":
                thresholds.append(float(column.std()) * 0.01)
            variables[role].append(variable)
            ranges.append({"name": signal["name"], "role": role, "unit": signal["unit"], "training_min": low, "training_max": high})
    config["variables"] = variables
    config["identification"]["minimum_input_std"] = thresholds
    research = copy.deepcopy(defaults["research"])
    research["bands"] = [{"half_width": (v["high"] - v["low"]) * 0.025, "tracking_scale": 0.05, "quadratic_penalty": 1.0} for v in variables["cv"]]
    research["objective"] = objective_defaults(config)
    research["objective"]["scenarios"] = [scenario["id"] for scenario in research["scenarios"]]
    config["modelling"] = modelling_settings(config)
    config["engineering_basis"] = {"source": "training observations only", "approved_limits": False, "ranges": ranges}
    match_history(config, history)
    return {"id": "", "name": dataset["name"] + " 控制研究", "dataset_id": dataset["id"], "config": config,
            "research": research, "draft_evidence": {"source": "training observations only", "approved_limits": False, "ranges": ranges}}

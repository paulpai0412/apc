"""Bounded offline tuning through the real mpc_validate engine; never deploys."""
import argparse
import copy
import json
import math
from pathlib import Path
import subprocess

from common import compatible_models, fingerprint, load_json, main_guard, save_json, validate_history


BOUNDS = {"weight": (0.000001, 1000000.0), "alpha": (0.0, 0.95), "weight_r": (0.000001, 1000000.0)}


def tuning_parameters(model):
    return {"cv_weight": [cv["weight"] for cv in model["variables"]["cvs"]],
            "cv_alpha": [cv.get("alpha", 0.0) for cv in model["variables"]["cvs"]],
            "mv_weight_r": [mv["weight_r"] for mv in model["variables"]["mvs"]]}


def bounded_candidates(model, max_trials=32):
    if not 1 <= max_trials <= 128:
        raise ValueError("max_trials must be in [1,128], including baseline")
    for group, field in (("cvs", "weight"), ("cvs", "alpha"), ("mvs", "weight_r")):
        low, high = BOUNDS[field]
        for variable in model["variables"][group]:
            value = variable.get(field, 0.0)
            if isinstance(value, bool) or not math.isfinite(value) or not low <= value <= high:
                raise ValueError(f"Baseline {field} outside declared tuning bounds [{low}, {high}]")
    candidates = [copy.deepcopy(model)]
    seen = {json.dumps(tuning_parameters(model), sort_keys=True)}
    for group, field in (("cvs", "weight"), ("cvs", "alpha"), ("mvs", "weight_r")):
        for index, variable in enumerate(model["variables"][group]):
            original = variable.get(field, 0.0)
            values = (0.2, 0.6) if field == "alpha" else (original * 0.5, original * 2)
            for value in values:
                low, high = BOUNDS[field]
                if not low <= value <= high:
                    continue
                candidate = copy.deepcopy(model)
                candidate["variables"][group][index][field] = value
                key = json.dumps(tuning_parameters(candidate), sort_keys=True)
                if key not in seen and len(candidates) < max_trials:
                    seen.add(key)
                    candidates.append(candidate)
    return candidates


def evaluate(binary, plant, controller, history, output, timeout):
    command = [str(binary), "closed-loop", "--plant", str(plant), "--controller", str(controller),
               "--history", str(history), "--output", str(output)]
    record = {"command": command, "valid": False}
    try:
        process = subprocess.run(command, shell=False, capture_output=True, text=True, timeout=timeout, check=False)
        record.update(returncode=process.returncode, stdout=process.stdout, stderr=process.stderr)
        if process.returncode != 0:
            record["error"] = "Engine returned nonzero (including solver failure)"
            return record
        result = load_json(output)
        if isinstance(result.get("score"), bool) or not isinstance(result.get("score"), (int, float)) or not math.isfinite(result["score"]):
            raise ValueError("Engine must return a finite score")
        if result.get("solver_failures") != 0:
            raise ValueError("Engine reported solver failures")
        for key in ("iae", "mv_total_variation", "cv_violation_seconds"):
            if not isinstance(result.get(key), list) or any(not isinstance(value, (int, float)) or not math.isfinite(value) or value < 0 for value in result[key]):
                raise ValueError(f"Missing/invalid engine metric: {key}")
        if not isinstance(result.get("trace"), dict):
            raise ValueError("Engine trace missing")
        record.update(valid=True, metrics=result)
    except subprocess.TimeoutExpired as error:
        record.update(error=f"Engine timed out after {timeout} seconds",
                      stdout=(error.stdout or b"").decode(errors="replace") if isinstance(error.stdout, bytes) else error.stdout,
                      stderr=(error.stderr or b"").decode(errors="replace") if isinstance(error.stderr, bytes) else error.stderr)
    except (ValueError, OSError, KeyError, TypeError) as error:
        record["error"] = str(error)
    return record


def tune(binary, plant_path, controller_path, selection_path, heldout_path, output_dir, max_trials=32, timeout=120):
    binary = Path(binary).resolve()
    if not binary.is_file():
        raise ValueError("Provide an existing user-built mpc_validate binary")
    if not math.isfinite(timeout) or timeout <= 0:
        raise ValueError("timeout must be finite and positive")
    plant, controller = load_json(plant_path), load_json(controller_path)
    compatible_models(plant, controller)
    selection, heldout = load_json(selection_path), load_json(heldout_path)
    validate_history(selection, plant)
    validate_history(heldout, plant)
    # Ignore provenance when detecting accidental reuse of the exact same scenario.
    data_keys = ("sample_time", "time_seconds", "cv", "mv", "dv")
    if all(selection[key] == heldout[key] for key in data_keys):
        raise ValueError("Selection and held-out scenarios must differ; never select on the test scenario")
    candidates = bounded_candidates(controller, max_trials)
    directory = Path(output_dir).resolve()
    directory.mkdir(parents=True, exist_ok=False)
    frozen_plant = directory / "frozen-plant.json"
    frozen_selection = directory / "selection-history.json"
    frozen_heldout = directory / "heldout-history.json"
    save_json(frozen_plant, plant)
    save_json(frozen_selection, selection)
    save_json(frozen_heldout, heldout)
    report = {"status": "candidate_only", "deployment": "Never deploys or contacts OPC/production",
              "selection_policy": "Deterministic bounded coordinate candidates; minimum engine fixed normalized score on selection scenario only",
              "bounds": BOUNDS, "max_trials": max_trials, "trials": [],
              "fidelity": "Synthetic/self-consistency evidence is not plant fidelity; independent plant validation required",
              "identical_plant_controller": plant == controller,
              "provenance": {"plant_sha256": fingerprint(plant_path), "controller_sha256": fingerprint(controller_path),
                             "selection_sha256": fingerprint(selection_path), "heldout_sha256": fingerprint(heldout_path),
                             "engine_sha256": fingerprint(binary)}}
    best = None
    for index, candidate in enumerate(candidates):
        candidate_path = directory / f"controller-{index:03d}.json"
        save_json(candidate_path, candidate)
        trial = evaluate(binary, frozen_plant, candidate_path, frozen_selection, directory / f"selection-{index:03d}.json", timeout)
        trial.update(index=index, parameters=tuning_parameters(candidate), controller=str(candidate_path))
        report["trials"].append(trial)
        save_json(directory / f"trial-{index:03d}.json", trial)
        if trial["valid"] and (best is None or trial["metrics"]["score"] < report["trials"][best]["metrics"]["score"]):
            best = index
        save_json(directory / "report.json", report)
    if best is None:
        report["status"] = "no_valid_candidate"
        save_json(directory / "report.json", report)
        raise ValueError("No valid engine trials; failure records retained")
    # Winner fixed before either held-out engine invocation; no refit or test selection.
    selected = directory / "candidate.json"
    save_json(selected, candidates[best])
    report["selected_trial"] = best
    baseline_test = evaluate(binary, frozen_plant, directory / "controller-000.json", frozen_heldout, directory / "heldout-baseline.json", timeout)
    selected_test = baseline_test if best == 0 else evaluate(binary, frozen_plant, selected, frozen_heldout, directory / "heldout-candidate.json", timeout)
    report["heldout"] = {"baseline": baseline_test, "candidate": selected_test,
                         "policy": "Reported only after selection, never used to choose parameters"}
    if not baseline_test["valid"] or not selected_test["valid"]:
        report["status"] = "candidate_only_heldout_failed"
    save_json(directory / "report.json", report)
    if report["status"] != "candidate_only":
        raise ValueError("Held-out evaluation failed; retained candidate is NOT validated")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--plant", required=True)
    parser.add_argument("--controller", required=True)
    parser.add_argument("--selection-history", required=True)
    parser.add_argument("--heldout-history", required=True)
    parser.add_argument("--output-dir", required=True, help="Must not exist; prevents stale trial results")
    parser.add_argument("--max-trials", type=int, default=32)
    parser.add_argument("--timeout", type=float, default=120)
    args = parser.parse_args()
    tune(args.binary, args.plant, args.controller, args.selection_history, args.heldout_history,
         args.output_dir, args.max_trials, args.timeout)


if __name__ == "__main__":
    main_guard(main)

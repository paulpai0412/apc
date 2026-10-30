"""Complete local research acceptance; never substitutes a shortened simulation.

Run with the project's Python environment and administrator-configured
AUTHRITY_RESEARCH_RUNTIME JSON (binary, bridge, bridge_args, local_only).
A frozen run may be rechecked with --check-run ID without rerunning experiments.
UI browser acceptance is additional and is not claimed by this numeric verifier.
"""
import argparse
import csv
import io
import importlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

import numpy as np

from campaign import calendar_splits, generate_scenario, validate_result
from common import fingerprint, load_json
from research import (METHODS, atomic_json, defaults, execute,
                      parameter_key, run_dir, runtime_settings)

from research_design import match_history, study_splits
from research_evaluation import aggregate_evaluations, research_metrics
from research_trends import ForecastArchive

def require(condition, message):
    if not condition:
        raise ValueError(message)


def import_clean_history(root, config, runtime):
    """Generate real Rust plant history, then exercise CSV mapping and cleaning."""
    source = root / "acceptance-source"
    source.mkdir(parents=True, exist_ok=False)
    atomic_json(source / "plant.json", config["plant"])
    scenario = generate_scenario(config)
    atomic_json(source / "scenario.json", scenario)
    command = [runtime["binary"], "replay", "--plant", str(source / "plant.json"),
               "--scenario", str(source / "scenario.json"), "--output", str(source / "history.json")]
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    atomic_json(source / "command.json", {"argv": command, "returncode": result.returncode,
                                          "stdout": result.stdout, "stderr": result.stderr})
    require(result.returncode == 0, "Real Rust history generation failed: " + result.stderr)
    history = load_json(source / "history.json")
    require(len(history["time_seconds"]) == calendar_splits(config)[-1]["last"], "Historical generation omitted scans")
    signals = []
    for role in ("cv", "mv", "dv"):
        for variable in config["variables"][role]:
            signals.append({"column": variable["name"], "name": variable["name"], "role": role,
                            "unit": variable["unit"], "scale": 1.0, "offset": 0.0,
                            "quality_column": "quality", "min": variable["low"], "max": variable["high"]})
    buffer = io.StringIO(newline="")
    writer = csv.writer(buffer)
    writer.writerow(["time_seconds", "quality"] + [s["column"] for s in signals])
    rows = []
    for i, seconds in enumerate(history["time_seconds"]):
        # MV identification uses the command coordinate, not actuator output.
        values = history["cv"][i] + history["command_mv"][i] + history["dv"][i]
        row = [seconds, 192, *values]
        if i == 120:
            row[1] = 0  # A real cleaning input defect, not a substituted control result.
        if i == 180:
            row[2] = ""
        rows.append(row)
    # Deliberate order/duplicate defects verify that sorting and deduplication act.
    rows[250], rows[251] = rows[251], rows[250]
    rows.insert(301, list(rows[300]))
    writer.writerows(rows)
    raw = execute(root, "import", {"name": "Three-month generated history with explicit cleaning defects", "format": "csv", "content": buffer.getvalue()})
    raw_id = raw.get("id") or raw["dataset"]["id"]
    splits = calendar_splits(config)
    cleaning = {"time_column": "time_seconds", "time_format": "seconds", "sample_time": config["sample_time"],
                "train_end": splits[0]["last"] * config["sample_time"], "validation_end": splits[1]["last"] * config["sample_time"],
                "duplicate_policy": "last", "missing_policy": "forward_fill", "max_gap_seconds": 2 * config["sample_time"],
                "outlier_policy": "none", "sigma": 4.0, "signals": signals}
    payload = {"dataset_id": raw_id, "cleaning": cleaning}
    preview = execute(root, "preview", payload)
    require(preview["usable"], "Expected short input defects to be cleanable: " + json.dumps(preview.get("warnings")))
    materialized = execute(root, "clean", payload)
    dataset = materialized["dataset"]
    cleaned = load_json(root / "datasets" / dataset["id"] / "history.json")
    require(cleaned["usable"] and len(cleaned["time_seconds"]) == len(history["time_seconds"]), "Cleaning changed the required full timeline")
    require(np.allclose(cleaned["cv"][120], cleaned["cv"][119]), "Bad-quality sample was not removed and causally filled")
    require(cleaned["cv"][180][0] == cleaned["cv"][179][0], "Missing CV was not causally filled")
    require(np.allclose(cleaned["mv"][400], history["command_mv"][400]), "MV column mapping did not preserve command coordinates")
    atomic_json(source / "cleaning-evidence.json", {"raw_id": raw_id, "cleaned_id": dataset["id"], "cleaning": cleaning,
                                                   "preview_summary": preview["summary"], "history_sha256": fingerprint(source / "history.json")})
    return dataset["id"]


def check_run(root, run_id):
    directory = run_dir(root, run_id)
    record = execute(root, "detail", {"id": run_id})
    require(record["status"] == "completed", "Research did not complete: " + str(record.get("error", record["status"])))
    config = record["study"]["config"]
    research = record["study"]["research"]
    splits = study_splits(config)
    require(set(research["methods"]) == set(METHODS), "Acceptance requires both control methods")
    if "data_window" in config:
        history_path = directory / "history.json"
        history = load_json(history_path)
        require(record["study"]["dataset_sha256"] == fingerprint(history_path), "Frozen cleaned history was modified")
        require(match_history(config, history) == splits, "Acceptance must use the entire declared dataset and its original cleaning splits")
        del history
    else:
        require(config["months"] == 3, "Generated acceptance requires three calendar months")
    scenario = load_json(directory / "scenario.json")
    require(len(scenario["time_seconds"]) == splits[-1]["last"], "Full scenario is incomplete")
    selection = load_json(directory / "selection.json")
    require(selection["objective"] == research["objective"], "Objective changed after candidate selection")
    selection_scenario = load_json(directory / "selection-scenario.json")
    require(record["provenance"]["study"] == fingerprint(directory / "study.json"), "Frozen study was modified")
    runtime = load_json(directory / "runtime.json")
    require(record["provenance"]["engine"] == fingerprint(runtime["binary"]), "Engine changed after this run; new acceptance required")
    if "runtime" in record["provenance"]:
        require(record["provenance"]["runtime"] == fingerprint(directory / "runtime.json"), "Frozen runtime was modified")
    for name, expected in record["provenance"]["sources"].items():
        # Verify the evaluator actually imported, including a user-selected
        # archived source directory on PYTHONPATH; never silently use new code.
        module = importlib.import_module(Path(name).stem)
        require(fingerprint(Path(module.__file__)) == expected, f"Loaded evaluator differs from recorded source: {name}")
    methods = {}
    for method in METHODS:
        row = next(row for row in record["results"]["methods"] if row["method"] == method)
        trials = [trial for trial in record["trials"] if trial["method"] == method]
        require({trial["round"] for trial in trials} == set(range(research["rounds"] + 1)) and research["rounds"] >= 3, f"{method}: incomplete adaptive coverage")
        for parameter in research["initial"]:
            require(len({trial["parameters"][parameter] for trial in trials}) >= 2, f"{method}: parameter never explored: {parameter}")
        signatures = [parameter_key(trial["parameters"]) for trial in trials]
        require(len(signatures) == len(set(signatures)), f"{method}: duplicate trials charged as new candidates")
        seen = {}
        for trial in trials:
            if trial["round"] > 0:
                parent = seen.get(trial["parent_trial"])
                if parent is None or parent["round"] >= trial["round"]:
                    raise ValueError("Candidate lacks a preceding experiment parent")
                earlier_eligible = [row for row in seen.values() if row["eligible"] and row["round"] < trial["round"]]
                incumbent = min(earlier_eligible, key=lambda row: row["score"]) if earlier_eligible else trials[0]
                require(parent["id"] == incumbent["id"], "Candidate was not generated around the previous eligible incumbent")
                require(trial["parameters"] != parent["parameters"], "Adaptive candidate did not change any parameter")
            seen[trial["id"]] = trial
            if trial["valid"]:
                require(trial["samples"] == splits[1]["samples"], "Selection used a shortened validation interval")
                for definition in research["scenarios"]:
                    scenario_id = definition["id"]
                    name = trial["id"] if scenario_id == "nominal" else trial["id"] + "-" + scenario_id
                    result = load_json(directory / (name + ".json"))
                    receipt = load_json(directory / (name + ".command.json"))
                    valid, reason = validate_result(result, selection_scenario, config)
                    require(valid, f"{name}: {reason}")
                    require(receipt["returncode"] == 0 and receipt["output_sha256"] == fingerprint(directory / (name + ".json")), "Trial output differs from actual engine receipt")
                    require(trial["scenario_samples"][scenario_id] == splits[1]["samples"], "Uncertainty trial omitted validation scans")
                    require(receipt["controller_sha256"] == fingerprint(directory / trial["controller"]), "Trial did not execute its declared controller")
                    require(receipt["engine_sha256"] == record["provenance"]["engine"], "Trial used a different engine build")
                    plant_name = "plant.json" if scenario_id == "nominal" else "plant-" + scenario_id + ".json"
                    require(receipt["plant_sha256"] == fingerprint(directory / plant_name), "Trial used a different uncertainty plant")
                    require(receipt["scenario_sha256"] == fingerprint(directory / "selection-scenario.json"), "Trial used a different validation schedule")
                    actual_metrics = research_metrics(result["trace"], config, research["objective"])
                    require(actual_metrics == trial["scenario_metrics"][scenario_id], "Trial score/metrics differ from complete actual trace")
                assessment = aggregate_evaluations(trial["scenario_metrics"], trials[0]["scenario_metrics"], config, research["objective"], baseline=trial["round"] == 0)
                require(assessment == trial["assessment"], "Candidate acceptance did not apply every frozen gate")
        winner = selection["winners"][method]["best"]
        eligible = [trial for trial in trials if trial["eligible"]]
        if research.get("selection_mode", "automatic") == "manual":
            decision = load_json(directory / "selection-request.json")
            require(decision == record.get("selection_decision") and decision.get("reason") and decision.get("selected_by"),
                    "Missing immutable attributed pre-sealed engineering decision")
            expected = next((trial for trial in trials if trial["id"] == decision["selections"].get(method)), None)
            require(expected is not None and expected["valid"] and (expected["eligible"] or expected["round"] == 0),
                    "Manual selection is neither eligible nor the retained baseline")
        else:
            expected = min(eligible, key=lambda trial: trial["score"]) if eligible else trials[0]
        require(winner["id"] == expected["id"], "Winner differs from the frozen pre-sealed selection policy")
        require(row["selected_trial"] == winner["id"], "Report selected a different controller from the frozen winner")
        controller = load_json(directory / winner["controller"])
        require(controller["control_strategy"]["method"] == method, "Controller method is only a UI label")
        for variant in ("baseline", "optimized", "transport"):
            name = method + "-" + variant
            result = load_json(directory / (name + ".json"))
            valid, reason = validate_result(result, scenario, config)
            require(valid, name + ": " + reason)
            receipt = load_json(directory / (name + ".command.json"))
            require(receipt["returncode"] == 0 and receipt["output_sha256"] == fingerprint(directory / (name + ".json")), "Incomplete or changed execution artifact")
            require(receipt["scenario_sha256"] == fingerprint(directory / "scenario.json"), "Not all methods saw the same scenario")
            selected = selection["winners"][method]["baseline"] if variant == "baseline" else winner
            require(receipt["controller_sha256"] == fingerprint(directory / selected["controller"]), "Final execution did not use the frozen selection")
            require(receipt["engine_sha256"] == record["provenance"]["engine"], "Final execution used a different engine build")
            measured = {"full": research_metrics(result["trace"], config, research["objective"]),
                        "test": research_metrics(result["trace"], config, research["objective"], splits[2]["first"], splits[2]["last"])}
            require(measured == row[variant], "Reported metrics differ from actual full and held-out traces")
            require(result["control_strategy"]["method"] == method, "Engine did not report actual requested method")
            archive_path = directory / (name + ".forecast.ndjson")
            require(receipt.get("forecast_sha256") == fingerprint(archive_path), "Forecast recording changed after execution")
            require(receipt.get("forecast_index_sha256") == fingerprint(Path(str(archive_path) + ".index.json")), "Forecast index changed after execution")
            archive = ForecastArchive(archive_path, config["sample_time"], len(config["variables"]["cv"]), len(config["variables"]["mv"]))
            require(len(archive.offsets) == splits[-1]["last"], "Full-horizon recording omitted scans")
            for sample in (0, splits[1]["first"], splits[-1]["last"] - 1):
                seconds = scenario["time_seconds"][sample]
                archived = archive.at(seconds)
                require(archived["sample"] == sample and archived["issued_at_seconds"] == seconds, "Forecast archive time/index mismatch")
                require(np.allclose(archived["cv"][:len(config["variables"]["cv"])], result["trace"]["predicted_cv"][sample]), "Archived first prediction differs from the executed controller")
            if variant == "transport":
                require(result["transport"] == "opc-da", "Actual COM DA transport not used")
                stats = result["transport_stats"]
                count = splits[-1]["last"]
                require(stats["write_calls"] == count and stats["update_calls"] == count and stats["read_calls"] >= 2 * count,
                        "Native DA did not exchange every scan")
                require(stats["read_errors"] == stats["write_errors"] == stats["bad_quality_items"] == 0, "Unexpected DA transport faults")
                direct = load_json(directory / (method + "-optimized.json"))["trace"]
                for field in ("time_seconds", "cv", "mv", "dv", "targets", "command_mv"):
                    require(np.allclose(direct[field], result["trace"][field],
                                        atol=config["transport"]["absolute_tolerance"], rtol=config["transport"]["relative_tolerance"]),
                            f"{method}: complete COM/direct trajectories differ in {field}")
                del direct
                methods[method] = np.asarray(result["trace"]["command_mv"])
        for definition in research["scenarios"]:
            scenario_id = definition["id"]
            if scenario_id == "nominal":
                continue
            for variant in ("baseline", "optimized"):
                name = method + "-" + variant + "-" + scenario_id
                result = load_json(directory / (name + ".json"))
                valid, reason = validate_result(result, scenario, config)
                require(valid, name + ": " + reason)
                receipt = load_json(directory / (name + ".command.json"))
                require(receipt["output_sha256"] == fingerprint(directory / (name + ".json")), "Uncertainty execution artifact changed")
                selected = selection["winners"][method]["baseline"] if variant == "baseline" else winner
                require(receipt["returncode"] == 0 and receipt["controller_sha256"] == fingerprint(directory / selected["controller"]), "Uncertainty execution did not use the frozen selection")
                require(receipt["plant_sha256"] == fingerprint(directory / ("plant-" + scenario_id + ".json")), "Uncertainty execution used a different plant")
                require(receipt["scenario_sha256"] == fingerprint(directory / "scenario.json"), "Uncertainty execution shortened or changed the full schedule")
                require(receipt["engine_sha256"] == record["provenance"]["engine"], "Uncertainty execution used a different engine build")
                metrics = research_metrics(result["trace"], config, research["objective"], splits[2]["first"], splits[2]["last"])
                require(metrics == row["scenario_tests"][variant][scenario_id], "Held-out uncertainty metrics differ from full trace")
        assessment = aggregate_evaluations(row["scenario_tests"]["optimized"], row["scenario_tests"]["baseline"], config, research["objective"])
        require(assessment == row["assessment"], "Held-out recommendation omitted declared scenarios or gates")
        if row["recommendation"] == "candidate_for_review":
            require(assessment["eligible"] and record["results"]["model_test"]["passed"], "Failed test gates cannot recommend promotion")
        chart = execute(root, "series", {"id": run_id, "method": method})
        require(chart["rawSamples"] == splits[-1]["last"] and chart["provenance"].startswith("Simulated COM DA"), "Chart does not derive from complete native transport result")
        actual = next(series for series in chart["series"] if series["variableId"] == "cv:0" and series["kind"] == "actual")
        require(actual["points"][0]["time"] == chart["originTime"], "Chart does not start at the declared data origin")
        if config["start"] == "relative":
            require(chart["timeBasis"] == "relative" and chart["originTime"] == 0, "Relative history acquired an invented calendar date")
        forecast = execute(root, "forecast", {"id": run_id, "method": method, "at": scenario["time_seconds"][-1]})
        prediction = next(series for series in forecast["series"] if series["variableId"] == "cv:0" and series["kind"] == "prediction")
        require(prediction["available"] and prediction["points"][0]["time"] - prediction["points"][0]["issuedAt"] == config["sample_time"] * 1000,
                "Historical full horizon is unavailable or shifted to the wrong valid time")
    require(not np.array_equal(methods[METHODS[0]], methods[METHODS[1]]),
            "Methods produced identical control trajectories; strategy behavior not demonstrated")
    require(all(check["status"] == "passed" for check in record["checks"]), "One or more full-period checks failed")
    evidence = {"status": "passed", "run_id": run_id, "samples_per_method": splits[-1]["last"], "methods": list(METHODS),
                "adaptive_rounds": research["rounds"], "trial_count": len(record["trials"]), "checks": record["checks"],
                "duration_seconds": splits[-1]["last"] * config["sample_time"], "scenarios": research["scenarios"],
                "scope": record["scope"] + "; complete declared interval and actual COM DA; not production certification",
                "ui_verification": "Separate actual desktop/mobile browser workflow required; not asserted by this numerical command"}
    atomic_json(directory / "acceptance.json", evidence)
    return evidence


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=str(Path(__file__).with_name("runs") / "autoresearch"))
    parser.add_argument("--runtime-file", help="Administrator runtime JSON, or set AUTHRITY_RESEARCH_RUNTIME")
    parser.add_argument("--check-run", help="Verify frozen full-run artifacts without repeating the experiment")
    parser.add_argument("--plant-source", choices=("independent_synthetic", "identified_history"),
                        default="independent_synthetic",
                        help="Evaluate declared independent dynamics or identify SOPDT dynamics from the generated, cleaned history")
    args = parser.parse_args()
    root = Path(args.root).resolve()
    root.mkdir(parents=True, exist_ok=True)
    if args.runtime_file:
        os.environ["AUTHRITY_RESEARCH_RUNTIME"] = json.dumps(load_json(args.runtime_file))
    try:
        if args.check_run:
            evidence = check_run(root, args.check_run)
        else:
            # Each full acceptance attempt has fresh data and a fresh artifact root.
            attempt = root / ("attempt-" + str(time.time_ns()))
            attempt.mkdir()
            runtime = runtime_settings()
            settings = defaults()
            dataset_id = import_clean_history(attempt, settings["campaign"], runtime)
            if args.plant_source == "identified_history":
                # Generation used independently configured physics. Identification
                # receives only cleaned observations and the same engineering limits.
                settings["campaign"]["plant_source"] = "identified_history"
                settings["campaign"].pop("plant")
                settings["campaign"].pop("scenario")
            study = execute(attempt, "save-study", {"name": "Complete three-month acceptance", "dataset_id": dataset_id,
                                                    "config": settings["campaign"], "research": settings["research"]})
            run = execute(attempt, "create-run", {"study_id": study["id"]})
            print(json.dumps({"stage": "started", "root": str(attempt), "run_id": run["id"]}), flush=True)
            execute(attempt, "run", {"id": run["id"]})
            evidence = check_run(attempt, run["id"])
            evidence["root"] = str(attempt)
        print(json.dumps(evidence, ensure_ascii=False), flush=True)
        return 0
    except Exception as error:
        print(json.dumps({"status": "failed", "error": str(error)}, ensure_ascii=False), flush=True)
        return 1


if __name__ == "__main__":
    sys.exit(main())

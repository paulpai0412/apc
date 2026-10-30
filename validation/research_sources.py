"""Read-only import of server-owned native open-loop synthetic replay evidence.

An uploaded assertion is never evidence of exogenous excitation. This boundary
accepts only existing completed synthetic runs, checks their replay receipt and
original bytes, and keeps command coordinates distinct from actuator output.
"""
import copy
import json
from pathlib import Path

import numpy as np

from common import fingerprint, load_json


def verify_generated_source(root, source):
    from research import load_run, run_dir
    required = {"kind", "run_id", "history_sha256", "receipt_sha256", "config_sha256", "input_coordinate"}
    if not isinstance(source, dict) or set(source) != required or source.get("kind") != "generated_synthetic" or source["input_coordinate"] != "command_mv":
        raise ValueError("Automatic dynamic refitting requires verified server-generated open-loop excitation; uploaded/closed-loop observations are not causal qualification")
    run = load_run(root, source["run_id"])
    directory = run_dir(Path(root), run["id"])
    if run["status"] != "completed" or run["study"]["config"]["plant_source"] != "independent_synthetic" or run["study"].get("dataset_id"):
        raise ValueError("Synthetic provenance requires a completed independent-synthetic native run generated without imported history")
    for field, name in (("history_sha256", "history.json"), ("receipt_sha256", "history.command.json"), ("config_sha256", "study.json")):
        if fingerprint(directory / name) != source[field]:
            raise ValueError("Server synthetic source bytes/receipt changed")
    if run["provenance"].get("study") != source["config_sha256"] or load_json(directory / "study.json") != run["study"]:
        raise ValueError("Synthetic engineering configuration differs from the executed study")
    receipt = load_json(directory / "history.command.json")
    argv = receipt.get("argv")
    if (receipt.get("returncode") != 0 or receipt.get("cancelled") or receipt.get("controller_sha256") is not None
            or not isinstance(argv, list) or len(argv) < 2 or argv[1] != "replay"
            or receipt.get("output_sha256") != source["history_sha256"]
            or receipt.get("engine_sha256") != run["provenance"]["engine"]
            or receipt.get("plant_sha256") != fingerprint(directory / "plant.json")
            or receipt.get("scenario_sha256") != fingerprint(directory / "scenario.json")):
        raise ValueError("Native source receipt does not prove completed open-loop replay")
    history, scenario = load_json(directory / "history.json"), load_json(directory / "scenario.json")
    if history["time_seconds"] != scenario["time_seconds"] or not np.array_equal(history["command_mv"], scenario["mv"]):
        raise ValueError("Synthetic excitation differs from the predeclared open-loop input scenario")
    return run, history


def qualify_generated_history(root, history):
    run, recorded = verify_generated_source(root, history.get("provenance", {}).get("source"))
    expected = [{"name": row["name"], "unit": row["unit"], "role": role}
                for role in ("cv", "mv", "dv") for row in run["study"]["config"]["variables"][role]]
    if history.get("signals") != expected or history.get("sample_time") != recorded["sample_time"]:
        raise ValueError("Automatic refitting cannot change verified source roles, units or sampling")
    for field, original in (("time_seconds", "time_seconds"), ("cv", "cv"), ("mv", "command_mv"), ("dv", "dv")):
        if not np.array_equal(history[field], recorded[original]):
            raise ValueError("Automatic refitting requires complete unmodified generated measurements/commands; cleaned alterations require renewed causal review")
    return {"qualified": True, "basis": "verified_server_synthetic_open_loop", "source_run_id": run["id"],
            "physical_authority": False, "input_coordinate": "command_mv"}


def import_run_history(root, payload):
    from datasets import execute as dataset_execute
    from research import load_run, run_dir
    if set(payload) != {"id"}:
        raise ValueError("Native history import accepts only the existing run ID")
    run = load_run(root, payload["id"])
    if run["status"] != "completed" or run["study"]["config"]["plant_source"] != "independent_synthetic" or run["study"].get("dataset_id"):
        raise ValueError("Import requires a completed independent-synthetic run generated without imported history")
    directory = run_dir(Path(root), run["id"])
    source = {"kind": "generated_synthetic", "run_id": run["id"], "input_coordinate": "command_mv",
              "history_sha256": fingerprint(directory / "history.json"),
              "receipt_sha256": fingerprint(directory / "history.command.json"),
              "config_sha256": fingerprint(directory / "study.json")}
    run, history = verify_generated_source(root, source)
    content = {key: copy.deepcopy(history[key]) for key in ("sample_time", "time_seconds", "cv", "dv")}
    content["mv"] = history["command_mv"]
    content["signals"] = [{"name": row["name"], "unit": row["unit"], "role": role}
                          for role in ("cv", "mv", "dv") for row in run["study"]["config"]["variables"][role]]
    return dataset_execute(root, "import", {"name": run["name"] + " / verified open-loop history", "format": "history",
                           "content": json.dumps(content, ensure_ascii=False, allow_nan=False, separators=(",", ":")), "source": source})

"""Generate small deterministic SYNTHETIC fixtures, never real cement measurements."""
import argparse
import copy
import csv
import random
from datetime import datetime, timedelta, timezone
from pathlib import Path

from common import load_json, main_guard, save_json

EXAMPLES = Path(__file__).resolve().parent / "examples"


def synthetic_history(count=600, seed=73, scenario=False):
    if count < 2:
        raise ValueError("At least two synthetic rows required")
    rng = random.Random(seed)
    mv = [0.0]
    dv = [0.0]
    for t in range(1, count):
        mv.append(0.0 if scenario else rng.uniform(-0.8, 0.8))
        dv.append((0.4 if (t // 15 + seed) % 2 else -0.4) if scenario else rng.uniform(-0.7, 0.7))
    cv = []
    for t in range(count):
        value = 0.0
        for lag, gain in enumerate((0.5, 0.3, 0.2), 1):
            value += gain * (mv[t - lag] if t >= lag else mv[0])
        for lag, gain in enumerate((-0.2, -0.1, -0.1), 1):
            value += gain * (dv[t - lag] if t >= lag else dv[0])
        cv.append([value])
    return {"sample_time": 1.0, "time_seconds": list(range(count)), "cv": cv,
            "mv": [[x] for x in mv], "dv": [[x] for x in dv],
            "provenance": {"synthetic": True, "source_url": "synthetic://deterministic-FIR",
                           "license": "Repository license; generated synthetic software test fixture",
                           "seed": seed, "description": "SOFTWARE FIXTURE ONLY, NOT cement/mill/kiln data"}}


def generate(directory):
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    model = load_json(EXAMPLES / "synthetic_model.json")
    save_json(directory / "plant.json", model)
    controller = copy.deepcopy(model)
    controller["metadata"]["description"] = "SYNTHETIC mismatched baseline controller; NOT a production model"
    controller["physics"]["step_coefficients"][0][0] = [x * 0.8 for x in model["physics"]["step_coefficients"][0][0]]
    save_json(directory / "baseline.json", controller)
    history = synthetic_history()
    with (directory / "history.csv").open("w", encoding="utf-8", newline="") as stream:
        writer = csv.writer(stream)
        writer.writerow(["time_seconds", "SyntheticQuality", "SyntheticFeed", "SyntheticDisturbance"])
        for t, cv, mv, dv in zip(history["time_seconds"], history["cv"], history["mv"], history["dv"]):
            writer.writerow([t, *cv, *mv, *dv])
    save_json(directory / "selection.json", synthetic_history(120, seed=10, scenario=True))
    save_json(directory / "heldout.json", synthetic_history(150, seed=11, scenario=True))
    save_json(directory / "columns.json", load_json(EXAMPLES / "columns.json"))
    mapping = {"sample_time": history["sample_time"]}
    for group in ("cv", "mv", "dv"):
        node_key = "sp" if group == "mv" else "pv"
        mapping[group] = [variable["node_ids"][node_key] for variable in model["variables"][group + "s"]]
    base_time = datetime(2024, 1, 1, tzinfo=timezone.utc)
    rows = []
    for index, seconds in enumerate(history["time_seconds"]):
        timestamp = (base_time + timedelta(seconds=seconds)).isoformat().replace("+00:00", "Z")
        for group in ("cv", "mv", "dv"):
            for node, value in zip(mapping[group], history[group][index]):
                rows.append([timestamp, node, value, 0])
    save_json(directory / "questdb-mapping.json", mapping)
    save_json(directory / "questdb-response.json", {
        "columns": [{"name": name, "type": kind} for name, kind in
                    (("timestamp", "TIMESTAMP"), ("node_id", "SYMBOL"),
                     ("value", "DOUBLE"), ("status", "LONG"))],
        "dataset": rows, "count": len(rows), "provenance": history["provenance"]})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", required=True)
    args = parser.parse_args()
    generate(args.output_dir)


if __name__ == "__main__":
    main_guard(main)

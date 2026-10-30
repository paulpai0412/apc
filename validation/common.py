"""Strict, offline-only interchange for historical MPC experiments."""
import hashlib
import json
import math
from pathlib import Path


def parse_json(content):
    def invalid(value):
        raise ValueError(f"Non-finite JSON number: {value}")
    return json.loads(content, parse_constant=invalid)


def load_json(path):
    with open(path, encoding="utf-8") as stream:
        return parse_json(stream.read())


def save_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, allow_nan=False)
        stream.write("\n")


def fingerprint(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def finite(value, label):
    if isinstance(value, bool):
        raise ValueError(f"{label}: boolean is not numeric")
    try:
        result = float(value)
    except (ValueError, TypeError) as error:
        raise ValueError(f"{label}: missing or invalid number") from error
    if not math.isfinite(result):
        raise ValueError(f"{label}: non-finite number")
    return result


def names(model, group):
    return [variable["name"] for variable in model["variables"].get(group + "s", [])]


def validate_history(history, model):
    def numeric(value, label):
        if not isinstance(value, (int, float)) or isinstance(value, bool):
            raise ValueError(f"{label}: canonical history requires JSON numbers")
        return finite(value, label)
    dt = numeric(history["sample_time"], "sample_time")
    if dt <= 0 or not math.isclose(dt, finite(model["tuning"]["sample_time"], "model sample_time"), rel_tol=1e-9, abs_tol=0):
        raise ValueError("Positive sample_time must match model")
    times = [numeric(value, "time_seconds") for value in history["time_seconds"]]
    if len(times) < 2:
        raise ValueError("History needs at least two rows")
    for previous, current in zip(times, times[1:]):
        if current <= previous or not math.isclose(current - previous, dt, rel_tol=1e-9, abs_tol=0):
            raise ValueError("Timestamps must be strictly increasing and uniformly spaced; gaps/duplicates rejected")
    for group in ("cv", "mv", "dv"):
        rows = history[group]
        width = len(names(model, group))
        if len(rows) != len(times) or any(len(row) != width for row in rows):
            raise ValueError(f"{group}: row count/width differs from model order")
        for row in rows:
            for value in row:
                numeric(value, group)
    if not names(model, "cv") or not names(model, "mv"):
        raise ValueError("At least one CV and MV required")
    return history


def compatible_models(plant, controller):
    for group in ("cv", "mv", "dv"):
        if names(plant, group) != names(controller, group):
            raise ValueError(f"Plant/controller {group} names and order must match")
        if [item.get("units") for item in plant["variables"].get(group + "s", [])] != [item.get("units") for item in controller["variables"].get(group + "s", [])]:
            raise ValueError(f"Plant/controller {group} engineering units must match")
    if plant["tuning"]["sample_time"] != controller["tuning"]["sample_time"]:
        raise ValueError("Plant/controller sample times must match")


def main_guard(main):
    try:
        main()
    except (ValueError, KeyError, TypeError, OSError) as error:
        raise SystemExit(f"error: {error}") from error

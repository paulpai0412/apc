"""Local immutable dataset import and split-safe cleaning for research.

Only server-generated IDs address storage. Preview and materialization execute
exactly the same pipeline; display reduction never changes canonical history.
Unresolved values stay as JSON null on the uniform timeline and make it unusable.
"""
import bisect
import csv
import hashlib
import io
import json
import math
import re
import statistics
import uuid
from datetime import datetime, timezone
from pathlib import Path

from common import finite, load_json

MAX_INPUT_BYTES = 32 * 1024 * 1024
MAX_ROWS = 500_000
MAX_COLUMNS = 128
MAX_CELLS = 8_000_000
MAX_SIGNALS = 32
ID_PATTERN = re.compile(r"ds_[0-9a-f]{32}\Z")
DISPLAY_POINTS = 2000

def _number(value):
    try:
        return finite(value, "signal")
    except ValueError:
        return None


def _json(value):
    return json.dumps(value, ensure_ascii=False, allow_nan=False, separators=(",", ":"))


def _store(root, summary, files):
    parent = Path(root) / "datasets"
    parent.mkdir(parents=True, exist_ok=True)
    identifier = "ds_" + uuid.uuid4().hex
    directory = parent / identifier
    directory.mkdir()  # Never reuse or overwrite a prior dataset.
    summary = dict(summary, id=identifier, created_at=datetime.now(timezone.utc).isoformat())
    try:
        for name, content in files.items():
            with (directory / name).open("x", encoding="utf-8", newline="") as stream:
                stream.write(content)
        # Publish metadata last: incomplete writes are not visible in the list.
        with (directory / "summary.json").open("x", encoding="utf-8") as stream:
            stream.write(_json(summary))
    except BaseException:
        # Remove only files created by this operation, never existing datasets.
        for path in directory.iterdir():
            path.unlink()
        directory.rmdir()
        raise
    return summary


def _dataset(root, identifier):
    if not isinstance(identifier, str) or not ID_PATTERN.fullmatch(identifier):
        raise ValueError("dataset_id must be a server-generated dataset ID, not a path")
    directory = Path(root) / "datasets" / identifier
    if directory.is_symlink() or not (directory / "summary.json").is_file():
        raise ValueError("Dataset not found; import the local file first")
    return directory, load_json(directory / "summary.json")


def list_datasets(root):
    directory = Path(root) / "datasets"
    if not directory.exists():
        return []
    result = []
    for path in directory.iterdir():
        if ID_PATTERN.fullmatch(path.name) and path.is_dir() and not path.is_symlink():
            if (path / "summary.json").is_file():
                result.append(load_json(path / "summary.json"))
    return sorted(result, key=lambda item: (item["created_at"], item["id"]), reverse=True)


def _parse(content, format_name):
    if not isinstance(content, str) or not content.strip():
        raise ValueError("Import requires nonempty UTF-8 text content")
    if len(content.encode("utf-8")) > MAX_INPUT_BYTES:
        raise ValueError("Import exceeds the 32 MiB limit; select fewer columns or a shorter interval")
    if format_name == "csv":
        try:
            reader = csv.reader(io.StringIO(content.lstrip("\ufeff"), newline=""), strict=True)
            columns = next(reader, [])
            if not columns or len(columns) > MAX_COLUMNS or any(not c.strip() for c in columns):
                raise ValueError("CSV requires 1–128 nonempty column headers")
            if len(set(columns)) != len(columns):
                raise ValueError("CSV column headers must be unique")
            rows = []
            for number, row in enumerate(reader, 2):
                if len(row) != len(columns):
                    raise ValueError(f"CSV row {number}: expected {len(columns)} fields, got {len(row)}")
                rows.append(row)
                if len(rows) > MAX_ROWS or len(rows) * len(columns) > MAX_CELLS:
                    raise ValueError("CSV exceeds 500000 rows or 8000000 cells; reduce the import")
        except csv.Error as error:
            raise ValueError(f"Invalid CSV: {error}") from error
        extras = {}
    elif format_name == "history":
        try:
            history = json.loads(content, parse_constant=lambda value: (_ for _ in ()).throw(ValueError(f"Invalid JSON number: {value}")))
        except (ValueError, RecursionError) as error:
            raise ValueError(f"Invalid canonical history JSON: {error}") from error
        if not isinstance(history, dict):
            raise ValueError("Canonical history must be a JSON object")
        times = history.get("time_seconds")
        if not isinstance(times, list) or not 2 <= len(times) <= MAX_ROWS:
            raise ValueError("Canonical time_seconds must contain 2–500000 rows")
        sample_time = finite(history.get("sample_time"), "history sample_time")
        if sample_time <= 0:
            raise ValueError("History sample_time must be positive")
        columns, suggested, matrices = ["time_seconds"], [], []
        metadata = history.get("signals", [])
        if not isinstance(metadata, list) or any(not isinstance(s, dict) for s in metadata):
            raise ValueError("History signals metadata must be a list of objects")
        for role in ("cv", "mv", "dv"):
            matrix = history.get(role)
            if not isinstance(matrix, list) or len(matrix) != len(times) or not all(isinstance(row, list) for row in matrix):
                raise ValueError(f"History {role} must have one array per timestamp")
            width = len(matrix[0])
            if (role != "dv" and not width) or any(len(row) != width for row in matrix):
                raise ValueError(f"History {role} rows must have consistent width, with at least one CV and MV")
            role_metadata = [item for item in metadata if item.get("role") == role]
            if role_metadata and len(role_metadata) != width:
                raise ValueError(f"History {role} signals metadata does not match matrix width")
            for index in range(width):
                item = role_metadata[index] if role_metadata else {}
                column = item.get("name", f"{role}_{index}")
                if not isinstance(column, str) or not column or column in columns:
                    raise ValueError("History signal names must be nonempty, unique, and not time_seconds")
                columns.append(column)
                suggested.append({"column": column, "name": column, "role": role,
                                  "unit": item.get("unit", ""), "scale": 1, "offset": 0})
            for row in matrix:
                if any(value is not None and (isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value)) for value in row):
                    raise ValueError(f"History {role} requires finite JSON numbers or null gaps")
            matrices.append(matrix)
        if len(columns) > MAX_COLUMNS or len(times) * len(columns) > MAX_CELLS:
            raise ValueError("History exceeds the column/cell limit")
        for time in times:
            if isinstance(time, bool) or not isinstance(time, (int, float)) or not math.isfinite(time):
                raise ValueError("History time_seconds requires finite JSON numbers")
        rows = [[time, *matrices[0][i], *matrices[1][i], *matrices[2][i]] for i, time in enumerate(times)]
        extras = {"sample_time": sample_time, "suggested_signals": suggested,
                  "suggested_time_column": "time_seconds", "suggested_time_format": "seconds"}
    else:
        raise ValueError("format must be csv or history")
    if len(rows) < 2:
        raise ValueError("At least two data rows are required")
    return columns, rows, extras


def _source_metadata(value, columns, row_count):
    if value is None:
        return {"kind": "file"}
    if isinstance(value, dict) and set(value) == {"kind", "manifest"} and value["kind"] == "captured_acquisition":
        # Validated by the dedicated captured-file import boundary, not a live reader.
        from research_workbench import canonical
        if len(canonical(value)) > 4096:
            raise ValueError("Captured source metadata exceeds its bound")
        return value
    if isinstance(value, dict) and value.get("kind") == "generated_synthetic":
        required = {"kind", "run_id", "history_sha256", "receipt_sha256", "config_sha256", "input_coordinate"}
        if set(value) != required or value["input_coordinate"] != "command_mv" or not re.fullmatch(r"[A-Za-z0-9_-]{1,96}", value["run_id"]):
            raise ValueError("Invalid server-generated synthetic source contract")
        if any(not isinstance(value[key], str) or not re.fullmatch(r"[0-9a-f]{64}", value[key]) for key in ("history_sha256", "receipt_sha256", "config_sha256")):
            raise ValueError("Generated source requires exact history, receipt and configuration digests")
        return value
    required = {"kind", "source_id", "schema", "table", "timestamp_column", "columns",
                "start", "end", "read_at", "row_count"}
    if not isinstance(value, dict) or set(value) != required or value.get("kind") != "postgres":
        raise ValueError("Database source must contain only the approved snapshot metadata, never credentials")
    if type(value["row_count"]) is not int or value["row_count"] != row_count:
        raise ValueError("Database snapshot row_count does not match the imported data")
    if value["columns"] != columns or value["timestamp_column"] not in columns:
        raise ValueError("Database snapshot columns must match the imported header and timestamp")
    for key in ("source_id", "schema", "table", "timestamp_column"):
        if not isinstance(value[key], str) or not value[key].strip() or len(value[key]) > 200:
            raise ValueError(f"Database source {key} must be a nonempty name of at most 200 characters")
    metadata = dict(value)
    dates = {}
    for key in ("start", "end", "read_at"):
        if not isinstance(value[key], str):
            raise ValueError(f"Database source {key} requires an ISO timestamp")
        try:
            date = datetime.fromisoformat(value[key].replace("Z", "+00:00"))
            if date.tzinfo is None or date.utcoffset() is None:
                raise ValueError("explicit timezone required")
            dates[key] = date.astimezone(timezone.utc)
        except (ValueError, OverflowError) as error:
            raise ValueError(f"Database source {key} requires an ISO timestamp with timezone") from error
        metadata[key] = dates[key].isoformat()
    if dates["start"] >= dates["end"]:
        raise ValueError("Database source start must be earlier than end")
    return metadata


def _configuration(value, columns):
    if not isinstance(value, dict):
        raise ValueError("cleaning must be an object with timestamp, splits, and signal mappings")
    config = dict(value)
    if not isinstance(config.get("time_column"), str) or config["time_column"] not in columns:
        raise ValueError("Select an existing time_column")
    choices = {"time_format": ("iso", "seconds"), "duplicate_policy": ("first", "last", "mean"),
               "missing_policy": ("reject", "forward_fill", "linear"),
               "outlier_policy": ("none", "bounds", "train_sigma")}
    for key, options in choices.items():
        if config.get(key) not in options:
            raise ValueError(f"{key} must be one of: {', '.join(options)}")
    for key in ("sample_time", "train_end", "validation_end", "max_gap_seconds"):
        config[key] = finite(config.get(key), key)
    config["sigma"] = finite(config.get("sigma", 3), "sigma")
    if config["sample_time"] <= 0 or config["max_gap_seconds"] < 0 or config["sigma"] <= 0:
        raise ValueError("sample_time and sigma must be positive; max_gap_seconds cannot be negative")
    if not 0 < config["train_end"] < config["validation_end"]:
        raise ValueError("Splits require 0 < train_end < validation_end, in relative seconds")
    regimes = config.get("regimes", [])
    if not isinstance(regimes, list) or len(regimes) > 128:
        raise ValueError("Provide at most 128 operating-regime intervals")
    for regime in regimes:
        if not isinstance(regime, dict) or set(regime) != {"start", "end", "label", "exclude", "reason"}:
            raise ValueError("Regimes require start, end, label, exclude and reason")
        if not 0 <= finite(regime["start"], "regime start") < finite(regime["end"], "regime end"):
            raise ValueError("Regime interval must be ordered nonnegative relative seconds")
        if type(regime["exclude"]) is not bool or any(not isinstance(regime[key], str) or not 1 <= len(regime[key].strip()) <= 500 for key in ("label", "reason")):
            raise ValueError("Regime labels/reasons must be bounded text and exclude must be boolean")
    config["regimes"] = regimes
    signals = config.get("signals")
    if not isinstance(signals, list) or not 2 <= len(signals) <= MAX_SIGNALS:
        raise ValueError("Map 2–32 signals, including at least one CV and one MV")
    normalized, used_names, used_columns = [], set(), {config["time_column"]}
    for signal in signals:
        if not isinstance(signal, dict):
            raise ValueError("Each signal mapping must be an object")
        signal = dict(signal)
        column, name = signal.get("column"), signal.get("name")
        if not isinstance(column, str) or column not in columns or column in used_columns:
            raise ValueError("Each signal must map a distinct existing column, not the timestamp")
        if not isinstance(name, str) or not name.strip() or len(name) > 128 or name in used_names:
            raise ValueError("Signal names must be nonempty, unique, and at most 128 characters")
        if signal.get("role") not in ("cv", "mv", "dv"):
            raise ValueError(f"{name}: role must be cv, mv, or dv")
        if not isinstance(signal.get("unit", ""), str) or len(signal.get("unit", "")) > 64:
            raise ValueError(f"{name}: unit must be a string of at most 64 characters")
        signal["unit"] = signal.get("unit", "")
        for key, default in (("scale", 1), ("offset", 0)):
            signal[key] = finite(signal.get(key, default), f"{name} {key}")
        if signal["scale"] == 0:
            raise ValueError(f"{name}: scale cannot be zero")
        quality = signal.get("quality_column")
        if quality is not None and quality not in columns:
            raise ValueError(f"{name}: quality_column does not exist")
        signal["quality_format"] = signal.get("quality_format", "da")
        if signal["quality_format"] not in ("da", "ua", "boolean"):
            raise ValueError(f"{name}: quality_format must be da, ua, or boolean")
        for key in ("min", "max"):
            if signal.get(key) is not None:
                signal[key] = finite(signal[key], f"{name} {key}")
        if signal.get("min") is not None and signal.get("max") is not None and signal["min"] > signal["max"]:
            raise ValueError(f"{name}: minimum exceeds maximum (bounds use converted engineering units)")
        if config["outlier_policy"] == "bounds" and signal.get("min") is None and signal.get("max") is None:
            raise ValueError(f"{name}: bounds detection requires min or max in converted units")
        normalized.append(signal)
        used_names.add(name)
        used_columns.add(column)
    if not all(any(s["role"] == role for s in normalized) for role in ("cv", "mv")):
        raise ValueError("Map at least one CV and one MV")
    config["signals"] = normalized
    return config


def _quality_good(value, encoding):
    if encoding == "boolean":
        if isinstance(value, str):
            return value.strip().lower() in ("true", "t", "1")
        return value is True or value == 1
    if isinstance(value, str):
        try:
            value = int(value.strip(), 0) if value.strip().lower().startswith("0x") else float(value)
        except ValueError:
            return False
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        return False
    if value != int(value):
        return False
    word = int(value)
    if encoding == "ua":
        return 0 <= word <= 0xFFFFFFFF and (word & 0xC0000000) == 0
    return 0 <= word <= 0xFFFF and (word & 0xC0) == 0xC0


def _stats(values):
    values = [value for value in values if value is not None]
    if not values:
        return {"count": 0, "mean": None, "std": None, "min": None, "max": None}
    return {"count": len(values), "mean": statistics.fmean(values),
            "std": statistics.pstdev(values), "min": min(values), "max": max(values)}


def _display(times, raw, clean, signals):
    # Reserve each signal's first actual change and nearby samples before reducing
    # the rest. Otherwise a non-extreme imputation can disappear from the focus view.
    selected = {0, len(times) - 1}
    for raw_values, clean_values in zip(raw, clean):
        first_change = next((i for i, (before, after) in enumerate(zip(raw_values, clean_values))
                             if before != after), None)
        if first_change is not None:
            selected.update(range(max(0, first_change - 10), min(len(times), first_change + 11)))
    # Remaining buckets preserve extrema and null witnesses within the same budget.
    buckets = max(1, (DISPLAY_POINTS - len(selected)) // (2 + 12 * len(signals)))
    size = max(1, math.ceil(len(times) / buckets))
    for start in range(0, len(times), size):
        stop = min(start + size, len(times))
        selected.update((start, stop - 1))
        for matrix in (raw, clean):
            for values in matrix:
                valid = [i for i in range(start, stop) if values[i] is not None]
                if valid:
                    selected.add(min(valid, key=lambda i: values[i]))
                    selected.add(max(valid, key=lambda i: values[i]))
                missing = [i for i in range(start, stop) if values[i] is None]
                if missing:
                    first, last = missing[0], missing[-1]
                    selected.update((first, last))
                    if first > start:
                        selected.add(first - 1)
                    if last + 1 < stop:
                        selected.add(last + 1)
    return [{"time": times[i],
             "raw": {signal["name"]: raw[j][i] for j, signal in enumerate(signals)},
             "clean": {signal["name"]: clean[j][i] for j, signal in enumerate(signals)}} for i in sorted(selected)]


def _pipeline(root, payload):
    directory, source = _dataset(root, payload.get("dataset_id"))
    if source["kind"] != "raw":
        raise ValueError("Select the original raw dataset to change cleaning; cleaned datasets are immutable")
    with (directory / "source.txt").open(encoding="utf-8", newline="") as stream:
        content = stream.read()
    if hashlib.sha256(content.encode("utf-8")).hexdigest() != source["source_sha256"]:
        raise ValueError("Raw source hash changed; re-import the original file")
    columns, rows, _ = _parse(content, source["format"])
    config = _configuration(payload.get("cleaning"), columns)
    time_index = columns.index(config["time_column"])
    parsed = []
    for number, row in enumerate(rows, 2):
        try:
            if config["time_format"] == "seconds":
                time = finite(row[time_index], f"timestamp row {number}")
            else:
                time = datetime.fromisoformat(str(row[time_index]).replace("Z", "+00:00"))
                if time.tzinfo is None or time.utcoffset() is None:
                    raise ValueError("ISO timestamps require an explicit timezone (for example Z)")
                time = time.astimezone(timezone.utc)
        except (ValueError, TypeError, OverflowError) as error:
            raise ValueError(f"Invalid timestamp at CSV/data row {number}: {error}") from error
        parsed.append((time, row))
    out_of_order = any(a[0] > b[0] for a, b in zip(parsed, parsed[1:]))
    parsed.sort(key=lambda pair: pair[0])  # Stable: first/last refer to source order.
    origin = parsed[0][0]
    grouped_times, groups = [], []
    for time, row in parsed:
        relative = (time - origin).total_seconds() if isinstance(time, datetime) else time - origin
        if not math.isfinite(relative):
            raise ValueError("Timestamp span is too large")
        if grouped_times and relative == grouped_times[-1]:
            groups[-1].append(row)
        else:
            grouped_times.append(relative)
            groups.append([row])
    duration, dt = grouped_times[-1], config["sample_time"]
    count_ratio = duration / dt
    if not math.isfinite(count_ratio) or count_ratio >= MAX_ROWS or count_ratio * len(config["signals"]) > MAX_CELLS:
        raise ValueError("Resampling exceeds 500000 rows or 8000000 values; increase sample_time")
    count = math.floor(count_ratio + 1e-9) + 1
    times = [index * dt for index in range(count)]
    train_end, validation_end = config["train_end"], config["validation_end"]
    split = lambda time: 0 if time < train_end else 1 if time < validation_end else 2
    exclusions = [row for row in config["regimes"] if row["exclude"]]
    excluded = lambda time: any(row["start"] <= time < row["end"] for row in exclusions)
    crosses_exclusion = lambda start, end: any(start < row["end"] and end >= row["start"] for row in exclusions)
    split_counts = {"train": sum(time < train_end for time in times),
                    "validation": sum(train_end <= time < validation_end for time in times),
                    "test": sum(time >= validation_end for time in times)}
    if count < 3 or not all(split_counts.values()):
        raise ValueError("Each chronological train/validation/test split needs at least one grid row; adjust split endpoints")
    warnings = []
    duplicates = len(rows) - len(groups)
    if out_of_order:
        warnings.append("Input timestamps were sorted chronologically before cleaning.")
    if duplicates:
        warnings.append(f"Resolved {duplicates} duplicate rows using {config['duplicate_policy']} policy.")
    if not math.isclose(times[-1], duration, rel_tol=1e-12, abs_tol=1e-9):
        warnings.append("Final source timestamp is off-grid; canonical history ends at the last complete sample interval.")
    raw_series, clean_series, signal_stats = [], [], []
    for signal in config["signals"]:
        column = columns.index(signal["column"])
        quality_index = columns.index(signal["quality_column"]) if signal.get("quality_column") is not None else None
        raw_values, values = [], []
        missing_count = bad_quality_count = 0
        for group in groups:
            raw_group, good_group = [], []
            for row in group:
                value = _number(row[column])
                if value is not None:
                    value = value * signal["scale"] + signal["offset"]
                    if not math.isfinite(value):
                        raise ValueError(f"{signal['name']}: unit conversion overflow; check scale and offset")
                else:
                    missing_count += 1
                good_quality = quality_index is None or _quality_good(row[quality_index], signal["quality_format"])
                if not good_quality:
                    bad_quality_count += 1
                raw_group.append(value)
                good_group.append(value if good_quality else None)
            def aggregate(items):
                if config["duplicate_policy"] == "mean":
                    present = [item for item in items if item is not None]
                    return statistics.fmean(present) if present else None
                return items[0 if config["duplicate_policy"] == "first" else -1]
            raw_values.append(aggregate(raw_group))
            values.append(aggregate(good_group))
        values = [None if excluded(time) else value for time, value in zip(grouped_times, values)]
        training = _stats([value for time, value in zip(grouped_times, values) if time < train_end])
        if config["outlier_policy"] == "train_sigma" and training["count"] < 2:
            raise ValueError(f"{signal['name']}: train_sigma needs at least two good training observations")
        outlier_count = 0
        for i, value in enumerate(values):
            if value is None:
                continue
            outlier = False
            if config["outlier_policy"] == "bounds":
                outlier = ((signal.get("min") is not None and value < signal["min"]) or
                           (signal.get("max") is not None and value > signal["max"]))
            elif config["outlier_policy"] == "train_sigma":
                outlier = abs(value - training["mean"]) > config["sigma"] * training["std"]
            if outlier:
                values[i] = None
                outlier_count += 1
        # Valid anchors are partitioned BEFORE filling, so no interpolation or
        # forward carry can transfer observations across an evaluation boundary.
        anchors = [[], [], []]
        for time, value in zip(grouped_times, values):
            if value is not None:
                anchors[split(time)].append((time, value))
        anchor_times = [[pair[0] for pair in part] for part in anchors]
        raw_grid, clean_grid = [], []
        filled_count = 0
        for time in times:
            part = split(time)
            index = bisect.bisect_right(grouped_times, time) - 1
            observed = index >= 0 and split(grouped_times[index]) == part and time - grouped_times[index] < dt - dt * 1e-10
            raw_grid.append(raw_values[index] if observed else None)
            value = values[index] if observed else None
            # Causal last-observation resampling is bounded by actual elapsed
            # time too; max_gap=0 permits only exact samples.
            if value is not None and (time - grouped_times[index] > config["max_gap_seconds"] + dt * 1e-10 or crosses_exclusion(grouped_times[index], time)):
                value = None
            if value is None and config["missing_policy"] != "reject" and not excluded(time):
                position = bisect.bisect_right(anchor_times[part], time)
                before = anchors[part][position - 1] if position else None
                after = anchors[part][position] if position < len(anchors[part]) else None
                if config["missing_policy"] == "forward_fill":
                    if before and time - before[0] <= config["max_gap_seconds"] + dt * 1e-10 and not crosses_exclusion(before[0], time):
                        value = before[1]
                elif before and after and after[0] - before[0] <= config["max_gap_seconds"] + dt * 1e-10 and not crosses_exclusion(before[0], after[0]):
                    fraction = (time - before[0]) / (after[0] - before[0])
                    value = before[1] * (1 - fraction) + after[1] * fraction
                if value is not None:
                    filled_count += 1
            if excluded(time):
                value = None
            if value is not None and not math.isfinite(value):
                raise ValueError(f"{signal['name']}: cleaning produced a non-finite value; check engineering units")
            clean_grid.append(value)
        unresolved = sum(value is None for value in clean_grid)
        if unresolved:
            warnings.append(f"{signal['name']}: {unresolved} unresolved grid values; dataset cannot be used for research. Adjust mapping/gap policy or import complete measurements.")
        if bad_quality_count:
            warnings.append(f"{signal['name']}: {bad_quality_count} source values rejected by OPC DA quality.")
        if outlier_count:
            warnings.append(f"{signal['name']}: {outlier_count} outliers rejected ({config['outlier_policy']}).")
        raw_series.append(raw_grid)
        clean_series.append(clean_grid)
        signal_stats.append({"name": signal["name"], "role": signal["role"], "unit": signal["unit"],
                             "source_count": len(rows), "missing_count": missing_count,
                             "bad_quality_count": bad_quality_count, "outlier_count": outlier_count,
                             "filled_count": filled_count, "unresolved_count": unresolved,
                             "training": training, "raw": _stats(raw_grid), "clean": _stats(clean_grid)})
    usable = all(item["unresolved_count"] == 0 for item in signal_stats)
    signals = [{key: signal[key] for key in ("name", "role", "unit")} for signal in config["signals"]]
    provenance = {"source_id": source["id"], "source_sha256": source["source_sha256"],
                  "cleaning_sha256": hashlib.sha256(_json(config).encode("utf-8")).hexdigest(),
                  "time_origin": origin.isoformat() if isinstance(origin, datetime) else origin,
                  "cleaning": config, "cleaner_version": 2,
                  "source": source.get("source", {"kind": "file"})}
    history = {"sample_time": dt, "time_seconds": times, "signals": signals,
               "usable": usable, "valid_rows": [all(values[i] is not None for values in clean_series) for i in range(count)],
               "provenance": provenance}
    for role in ("cv", "mv", "dv"):
        indexes = [i for i, signal in enumerate(signals) if signal["role"] == role]
        history[role] = [[clean_series[index][row] for index in indexes] for row in range(count)]
    summary = {"row_count": count, "source_row_count": len(rows), "duplicate_count": duplicates,
               "sample_time": dt, "duration_seconds": times[-1], "source_duration_seconds": duration,
               "split_counts": split_counts, "signal_stats": signal_stats,
               "source_sha256": source["source_sha256"], "cleaning": config, "usable": usable,
               "source": provenance["source"]}
    series = _display(times, raw_series, clean_series, signals)
    preview = {"dataset_id": source["id"], "summary": summary, "warnings": warnings, "usable": usable,
               "series": series, "signals": signals, "cleaning": config,
               "display": {"decimated": len(series) < count, "row_count": len(series), "full_row_count": count,
                           "method": "bucket extrema and null witnesses; raw and clean use converted units"}}
    return preview, history


def execute(root: Path, operation: str, payload: dict) -> dict:
    """Public filesystem-backed import/preview/clean API; never executes commands."""
    if not isinstance(payload, dict):
        raise ValueError("Request payload must be a JSON object")
    if operation == "list":
        return {"datasets": list_datasets(root)}
    if operation == "import":
        name = payload.get("name", "Imported dataset")
        if not isinstance(name, str) or not name.strip() or len(name) > 200:
            raise ValueError("Dataset name must contain 1–200 characters")
        content, format_name = payload.get("content"), payload.get("format")
        columns, rows, extras = _parse(content, format_name)
        summary = {"name": name, "kind": "raw", "format": format_name, "columns": columns,
                   "row_count": len(rows), "source_sha256": hashlib.sha256(content.encode("utf-8")).hexdigest(),
                   "usable": False, "source": _source_metadata(payload.get("source"), columns, len(rows)), **extras}
        for existing in list_datasets(root):
            if existing["kind"] == "raw" and existing["source_sha256"] == summary["source_sha256"] and existing.get("source") == summary["source"]:
                return dict(existing, duplicate=True)
        return _store(root, summary, {"source.txt": content})
    if operation in ("preview", "clean"):
        preview, history = _pipeline(root, payload)
        from research_workbench import exposure, exposure_status
        exposure(root, preview["summary"]["source_sha256"], "cleaning_preview_including_test")
        preview["exposure"] = exposure_status(root, preview["summary"]["source_sha256"])
        if operation == "clean":
            _, source = _dataset(root, payload["dataset_id"])
            summary = dict(preview["summary"], name=source["name"] + " (clean)", kind="clean",
                           source_id=source["id"], columns=["time_seconds"] + [s["name"] for s in preview["signals"]],
                           signals=preview["signals"],
                           suggested_signals=[dict(signal) for signal in preview["cleaning"]["signals"]],
                           warnings=preview["warnings"], format="history")
            encoded = _json(history)
            summary["history_sha256"] = hashlib.sha256(encoded.encode("utf-8")).hexdigest()
            preview["dataset"] = _store(root, summary, {"history.json": encoded,
                                                      "cleaning.json": _json(preview["cleaning"]),
                                                      "preview.json": _json(preview)})
        return preview
    raise ValueError("Dataset operation must be import, preview, clean, or list")

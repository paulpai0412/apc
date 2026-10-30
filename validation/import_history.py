"""Import a local CSV; never fetch, interpolate, reorder, or silently drop rows."""
import argparse
import csv
from datetime import datetime

from common import finite, fingerprint, load_json, main_guard, names, save_json, validate_history


def import_csv(path, mapping, model):
    timestamp = mapping["timestamp"]
    encoding = mapping.get("encoding", "utf-8-sig")
    provenance = dict(mapping["provenance"])
    if not provenance.get("source_url") or not provenance.get("license"):
        raise ValueError("Explicit provenance source_url and license are required (use 'unknown' if unverified)")
    columns = [timestamp["column"]]
    for group in ("cv", "mv", "dv"):
        if len(mapping[group]) != len(names(model, group)):
            raise ValueError(f"{group} mapping must list columns in model order")
        columns.extend(mapping[group])
    if len(set(columns)) != len(columns):
        raise ValueError("Each timestamp/CV/MV/DV must map to a distinct column")
    history = {"sample_time": finite(mapping["sample_time"], "sample_time"),
               "time_seconds": [], "cv": [], "mv": [], "dv": []}
    origin = None
    with open(path, encoding=encoding, newline="") as stream:
        reader = csv.reader(stream, delimiter=mapping.get("delimiter", ","), strict=True)
        fields = next(reader, [])
        if len(set(fields)) != len(fields) or not set(columns).issubset(fields):
            raise ValueError("CSV headers must be unique and include every mapped column")
        for number, values in enumerate(reader, 2):
            if len(values) != len(fields):
                raise ValueError(f"CSV row {number}: blank row or inconsistent field count")
            row = dict(zip(fields, values))
            raw_time = row[timestamp["column"]]
            if timestamp["format"] == "seconds":
                time = finite(raw_time, f"timestamp row {number}")
            elif timestamp["format"] == "iso8601":
                parsed = datetime.fromisoformat(raw_time.replace("Z", "+00:00"))
                if parsed.tzinfo is None or parsed.utcoffset() is None:
                    raise ValueError("ISO8601 timestamps require explicit timezone")
                if origin is None:
                    origin = parsed
                # Subtract datetimes before floating conversion; epoch floats lose
                # subsecond precision and fail the Rust uniform-grid contract.
                time = (parsed - origin).total_seconds()
            else:
                raise ValueError("timestamp.format must be seconds or iso8601")
            history["time_seconds"].append(time)
            for group in ("cv", "mv", "dv"):
                history[group].append([finite(row[column], f"row {number} {column}") for column in mapping[group]])
    provenance.update({"input_sha256": fingerprint(path), "importer": "strict-csv",
                       "encoding": encoding, "columns_in_model_order": {g: mapping[g] for g in ("cv", "mv", "dv")}})
    if origin is not None:
        provenance["time_origin_iso8601"] = origin.isoformat()
    history["provenance"] = provenance
    return validate_history(history, model)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--csv", required=True)
    parser.add_argument("--mapping", required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    save_json(args.output, import_csv(args.csv, load_json(args.mapping), load_json(args.model)))


if __name__ == "__main__":
    main_guard(main)

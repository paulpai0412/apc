"""Research-only artifacts: captured files, model qualification and exposure ledger.

No network client, controller command or deployment authority lives here.
"""
import copy
import hashlib
import json
from pathlib import Path
import re
import os
import sqlite3
import uuid
from datetime import datetime, timezone

import numpy as np

from common import fingerprint, load_json


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, allow_nan=False, separators=(",", ":")).encode("utf-8")


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def capabilities():
    return {
        "identification": [
            {"id": "fir", "supported": True, "scope": "control_model", "reason": "Chronological ridge FIR, excitation/rank/settled-tail gates; observational, not causal certification"},
            {"id": "fopdt", "supported": True, "scope": "control_model", "reason": "Training-only FIR path approximation with tau2 constrained to zero throughout bounded fitting and chronological validation"},
            {"id": "sopdt", "supported": True, "scope": "control_model", "reason": "Training-only two-pole path fitting with fractional delay and chronological qualification; not independent factory truth"}],
        "evaluation_model_families": ["fopdt", "sopdt"],
        "continuous_research": True, "automatic_reidentification": True,
        "automatic_reidentification_scope": "verified_server_synthetic_open_loop_only",
        "automatic_reidentification_observational": False,
        "policy_actions": ["frozen_model_tuning", "reidentify_and_tune"], "captured_snapshot_import": True,
        "search_fields": ["cv_weight_scale", "mv_weight_scale", "alpha"],
        "fixed_during_search": ["sample_time", "model_memory", "prediction_horizon", "control_horizon", "evaluation_plant", "objective", "splits", "limits"],
        "manual_presealed_selection": True, "runtime_write": False,
        "delayed_lab_control": False, "causal_estimator": False,
    }


def exposure(root, source_hash, reason, *, run_id=None):
    """Append an actual exposure; metadata browsing does not call this function."""
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(root / "research-exposure.sqlite3", timeout=30) as db:
        db.execute("CREATE TABLE IF NOT EXISTS exposures (id INTEGER PRIMARY KEY, source_hash TEXT NOT NULL, reason TEXT NOT NULL, run_id TEXT, created_at TEXT NOT NULL)")
        db.execute("INSERT INTO exposures(source_hash,reason,run_id,created_at) VALUES(?,?,?,?)",
                   (source_hash, reason, run_id, datetime.now(timezone.utc).isoformat()))


def exposure_status(root, source_hash):
    path = Path(root) / "research-exposure.sqlite3"
    if not path.exists():
        return {"independent": False, "status": "unverified_external_history", "events": []}
    with sqlite3.connect(path, timeout=30) as db:
        rows = db.execute("SELECT reason,run_id,created_at FROM exposures WHERE source_hash=? ORDER BY id DESC LIMIT 100", (source_hash,)).fetchall()
    return {"independent": False, "status": "exploratory" if rows else "unverified_external_history",
            "events": [{"reason": row[0], "run_id": row[1], "created_at": row[2]} for row in rows],
            "reason": "Imported history has no independently attested untouched holdout; previews and sealed evaluations are recorded, never relabelled untouched"}


def import_snapshot(root, payload):
    from datasets import execute as dataset_execute
    if set(payload) != {"name", "content", "manifest"}:
        raise ValueError("Captured snapshot requires name, original UTF-8 content and manifest only")
    manifest = payload["manifest"]
    required = {"schema_version", "source_id", "captured_at", "start", "end", "format", "sha256", "read_only"}
    if not isinstance(manifest, dict) or set(manifest) != required or type(manifest["schema_version"]) is not int or manifest["schema_version"] != 1 or manifest["read_only"] is not True:
        raise ValueError("Captured snapshot manifest must declare schema_version=1 and read_only=true; no connection or write targets")
    if not isinstance(manifest["source_id"], str) or not re.fullmatch(r"[A-Za-z0-9_.:-]{1,200}", manifest["source_id"]):
        raise ValueError("Captured source_id must be a bounded identifier, never a URL or DSN")
    dates = {}
    for field in ("captured_at", "start", "end"):
        value = datetime.fromisoformat(str(manifest[field]).replace("Z", "+00:00"))
        if value.tzinfo is None:
            raise ValueError("Captured timestamps must include timezone offsets")
        dates[field] = value
    if dates["start"] >= dates["end"] or dates["captured_at"] < dates["end"]:
        raise ValueError("Captured snapshot requires start < end <= captured_at")
    content = payload["content"]
    if not isinstance(content, str) or hashlib.sha256(content.encode("utf-8")).hexdigest() != manifest["sha256"]:
        raise ValueError("Captured original content SHA-256 mismatch")
    return dataset_execute(root, "import", {"name": payload["name"], "format": manifest["format"], "content": content,
                           "source": {"kind": "captured_acquisition", "manifest": manifest}})


def load_model(root, identifier):
    from research import safe_id
    path = Path(root) / "models" / (safe_id(identifier) + ".json")
    artifact = load_json(path)
    expected = artifact.pop("artifact_sha256")
    if digest(artifact) != expected:
        raise ValueError("Frozen identification artifact integrity mismatch")
    artifact["artifact_sha256"] = expected
    if digest(artifact["model"]) != artifact["model_sha256"] or digest(artifact["evaluation_plant"]) != artifact["evaluation_plant_sha256"]:
        raise ValueError("Frozen model/plant digest mismatch")
    return artifact


def list_models(root):
    result = []
    fields = ("id", "study_id", "created_at", "dataset_id", "model_sha256", "artifact_sha256", "evaluation_plant_sha256", "qualification")
    for path in sorted((Path(root) / "models").glob("*.json"), reverse=True):
        artifact = load_model(root, path.stem)
        result.append({field: artifact[field] for field in fields})
    return sorted(result, key=lambda row: row["created_at"], reverse=True)


def identify_study(root, payload):
    from research import atomic_json, now, safe_id, validate_study
    from research_design import match_history, study_splits
    from research_plant import fit_history_plant
    if set(payload) != {"study_id"}:
        raise ValueError("Identification requires only the saved study_id; its frozen model_family selects the actual fitter")
    if sum(1 for _ in (Path(root) / "models").glob("*.json")) >= 1000:
        raise ValueError("Identification artifact capacity reached; administrator retention review required")
    study = validate_study(load_json(Path(root) / "studies" / (safe_id(payload["study_id"]) + ".json")))
    if not study.get("dataset_id"):
        raise ValueError("Separate model qualification requires a materialized cleaned dataset")
    path = Path(root) / "datasets" / safe_id(study["dataset_id"]) / "history.json"
    if fingerprint(path) != study["dataset_sha256"]:
        raise ValueError("Cleaned dataset changed since engineering freeze")
    history = load_json(path)
    config = study["config"]
    match_history(config, history)
    splits = study_splits(config)
    train_end, val_end = splits[0]["last"], splits[1]["last"]
    options = dict(config["identification"])
    thresholds = np.asarray(options.pop("minimum_input_std"))
    inputs = np.concatenate((history["mv"][:train_end], history["dv"][:train_end]), axis=1)
    spread = inputs.std(axis=0)
    if thresholds.shape != spread.shape or np.any(spread < thresholds):
        raise ValueError("Training input excitation below engineering thresholds; unknown coupling is not zero gain")
    from research_identification import identify_control
    model, report = identify_control(config, history, train_end, val_end)
    normalized = np.asarray(report["validation"]["rmse"]) / np.asarray([v["high"] - v["low"] for v in config["variables"]["cv"]])
    if np.any(normalized > config["modelling"]["validation_max_normalized_rmse"]):
        raise ValueError("Historical control model failed chronological normalized RMSE qualification")
    if config["plant_source"] == "identified_history":
        plant, evidence = fit_history_plant(config, history, train_end, val_end)
    else:
        plant, evidence = copy.deepcopy(config["plant"]), {"status": "declared_independent_synthetic", "factory_validation": False}
    report.update(input_std=spread.tolist(), minimum_input_std=thresholds.tolist(), validation_normalized_rmse=normalized.tolist())
    causal = {"qualified": False, "reason": "Observational source has no verified exogenous excitation contract"}
    if history.get("provenance", {}).get("source", {}).get("kind") == "generated_synthetic":
        from research_sources import qualify_generated_history
        try:
            causal = qualify_generated_history(root, history)
        except (ValueError, KeyError, OSError) as error:
            causal = {"qualified": False, "reason": str(error)}
    artifact = {"id": "model_" + uuid.uuid4().hex, "study_id": study["id"], "created_at": now(),
                "dataset_id": study["dataset_id"], "dataset_sha256": study["dataset_sha256"],
                "engineering_sha256": digest(config), "config": config, "model": model, "report": report,
                "model_sha256": digest(model), "evaluation_plant": plant, "evaluation_plant_sha256": digest(plant),
                "plant_evidence": evidence, "qualification": {"historical_prediction": True, "test_accessed": False,
                "factory_authority": False, "automatic_reidentification": causal["qualified"], "identifiability": causal},
                "sources": {name: fingerprint(Path(__file__).with_name(name)) for name in
                            ("identify.py", "research_identification.py", "research_plant.py", "research_sources.py", "research_design.py", "research_workbench.py")}}
    summary = load_json(Path(root) / "datasets" / study["dataset_id"] / "summary.json")
    artifact["holdout"] = exposure_status(root, summary["source_sha256"])
    artifact["artifact_sha256"] = digest(artifact)
    atomic_json(Path(root) / "models" / (artifact["id"] + ".json"), artifact)
    return artifact


def pareto_trials(trials):
    """Worst-scenario normalized objectives; eligibility cannot be offset by score."""
    vectors = {}
    for trial in trials:
        if not trial.get("valid") or not trial.get("eligible"):
            continue
        scenarios = list(trial["scenario_metrics"].values())
        keys = ("variance", "bias", "movement")
        vectors[trial["id"]] = [max(item["score_components"][key] for item in scenarios) for key in keys]
    frontier = [identifier for identifier, vector in vectors.items() if not any(
        all(a <= b for a, b in zip(other, vector)) and any(a < b for a, b in zip(other, vector))
        for candidate, other in vectors.items() if candidate != identifier)]
    return {"objectives": ["worst_scenario_variance", "worst_scenario_bias", "worst_scenario_movement"],
            "vectors": vectors, "nondominated_trial_ids": frontier, "scope": "development counterfactual closed-loop scenarios only; not historical prediction or factory improvement"}


def select_trials(root, payload):
    from research import load_run, now, run_dir
    if set(payload) != {"id", "selections", "reason", "selected_by"}:
        raise ValueError("Selection requires run id, per-method trial selections, reason and authenticated attribution")
    record = load_run(root, payload["id"])
    path = run_dir(root, record["id"]) / "selection-request.json"
    if path.exists():
        prior = load_json(path)
        if prior["selections"] != payload["selections"] or prior["reason"] != payload["reason"]:
            raise ValueError("Development selection is already frozen")
        return prior
    if record.get("stage") != "awaiting_selection" or record.get("status") != "running":
        raise ValueError("Selection is available only before sealed validation; exposed results cannot select another winner")
    if not isinstance(payload["reason"], str) or not 1 <= len(payload["reason"].strip()) <= 2000:
        raise ValueError("A bounded engineering selection reason is required")
    selections = payload["selections"]
    methods = record["study"]["research"]["methods"]
    if not isinstance(selections, dict) or set(selections) != set(methods):
        raise ValueError("Select exactly one development trial per frozen method")
    for method, identifier in selections.items():
        trial = next((row for row in record["trials"] if row["id"] == identifier and row["method"] == method), None)
        if trial is None or not trial.get("valid") or not (trial.get("eligible") or trial["round"] == 0):
            raise ValueError("Choose an eligible development trial or retain the numerically valid baseline")
    value = dict(payload, selected_at=now())
    # Publish complete bytes atomically and exclusively. The worker never sees
    # a half-written decision; simultaneous browsers cannot replace a winner.
    temporary = path.with_name(".selection-" + uuid.uuid4().hex)
    try:
        with temporary.open("xb") as stream:
            stream.write(canonical(value))
            stream.flush()
            os.fsync(stream.fileno())
        try:
            os.link(temporary, path)
        except FileExistsError:
            prior = load_json(path)
            if prior["selections"] != selections or prior["reason"] != payload["reason"]:
                raise ValueError("Development selection is already frozen")
            return prior
        return value
    finally:
        temporary.unlink(missing_ok=True)

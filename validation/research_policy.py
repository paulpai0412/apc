"""Durable bounded new-data scheduling, with no deployment authority.

SQLite serializes policy edits/claims. A claim is committed BEFORE creating a run;
a restart never repeats an uncertain launch. Rust owns the actual existing worker.
"""
import copy
import json
from pathlib import Path
import shutil
import sqlite3
import time
import uuid
from datetime import datetime

import numpy as np
from common import fingerprint, load_json
from research_workbench import digest, load_model


def database(root):
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(root / "research-policy.sqlite3", timeout=30)
    db.row_factory = sqlite3.Row
    db.executescript("""
        CREATE TABLE IF NOT EXISTS policies(id TEXT PRIMARY KEY, definition TEXT NOT NULL, state TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS claims(key TEXT PRIMARY KEY, policy_id TEXT NOT NULL, run_id TEXT NOT NULL, created_at REAL NOT NULL);
        CREATE TABLE IF NOT EXISTS audit(id INTEGER PRIMARY KEY, policy_id TEXT NOT NULL, created_at REAL NOT NULL, reason TEXT NOT NULL, dataset_id TEXT, run_id TEXT);
        CREATE TABLE IF NOT EXISTS scheduler(id INTEGER PRIMARY KEY CHECK(id=1), heartbeat REAL NOT NULL, status TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS decision_keys(key TEXT PRIMARY KEY);
        CREATE TABLE IF NOT EXISTS drift_windows(policy_id TEXT NOT NULL, source_hash TEXT NOT NULL, start REAL NOT NULL, end REAL NOT NULL, above INTEGER NOT NULL, evidence TEXT NOT NULL, PRIMARY KEY(policy_id,source_hash));
    """)
    return db


def _record(db, row):
    result = json.loads(row["definition"])
    result["definition_sha256"] = digest(result)
    result["state"] = json.loads(row["state"])
    result["revision"] = result["state"]["revision"]
    result["enabled"] = result["state"]["enabled"]
    result["state"]["audit"] = [dict(value) for value in db.execute(
        "SELECT created_at,reason,dataset_id,run_id FROM audit WHERE policy_id=? ORDER BY id DESC LIMIT 100", (result["id"],))]
    result["state"]["drift_windows"] = [
        {**dict(value), "evidence": json.loads(value["evidence"])} for value in db.execute(
            "SELECT source_hash,start,end,above,evidence FROM drift_windows WHERE policy_id=? ORDER BY end DESC LIMIT 20", (result["id"],))]
    return result


def list_policies(root):
    with database(root) as db:
        policies = [_record(db, row) for row in db.execute("SELECT * FROM policies ORDER BY rowid DESC")]
        row = db.execute("SELECT heartbeat,status FROM scheduler WHERE id=1").fetchone()
        scheduler = dict(row) if row else {"heartbeat": None, "status": "not_started"}
        scheduler["fresh"] = bool(row and time.time() - row["heartbeat"] < 90)
        scheduler.update(interval_seconds=30, actions=["frozen_model_tuning", "reidentify_and_tune"], deploy_authority=False,
                         automatic_reidentification_scope="verified_server_synthetic_open_loop_only")
        return {"policies": policies, "scheduler": scheduler}


def _audit(db, identifier, state, reason, dataset=None, run=None):
    decision = {"reason": reason, "dataset_id": dataset, "run_id": run}
    key = digest({"policy": identifier, "revision": state["revision"], "runs_started": state["runs_started"], **decision})
    if db.execute("INSERT OR IGNORE INTO decision_keys VALUES(?)", (key,)).rowcount:
        db.execute("INSERT INTO audit(policy_id,created_at,reason,dataset_id,run_id) VALUES(?,?,?,?,?)",
                   (identifier, time.time(), reason, dataset, run))
    state["last_decision"] = decision
    db.execute("UPDATE policies SET state=? WHERE id=?", (json.dumps(state), identifier))


def save_policy(root, payload):
    from research import now, safe_id, validate_study
    allowed = {"id", "name", "study_id", "model_id", "action", "trigger", "enabled", "minimum_rows", "max_filled_fraction", "cooldown_seconds", "max_runs", "max_trials_total", "created_by", "publication_template_version_id"}
    if set(payload) - allowed:
        raise ValueError("Unknown policy fields; research policies cannot configure networks, executable code or deployment")
    if not isinstance(payload.get("name"), str) or not 1 <= len(payload["name"].strip()) <= 120 or type(payload.get("enabled")) is not bool:
        raise ValueError("Policy requires a bounded name and boolean enabled")
    for field, lower, upper in (("minimum_rows", 10, 500000), ("cooldown_seconds", 30, 31536000), ("max_runs", 1, 1000), ("max_trials_total", 1, 128000)):
        if type(payload.get(field)) is not int or not lower <= payload[field] <= upper:
            raise ValueError(f"{field} must be an integer in [{lower}, {upper}]")
    fraction = payload.get("max_filled_fraction")
    if isinstance(fraction, bool) or not isinstance(fraction, (int, float)) or not 0 <= fraction <= 0.2:
        raise ValueError("max_filled_fraction must be in [0, 0.2]")
    trigger = payload.get("trigger", {"kind": "new_data"})
    if not isinstance(trigger, dict) or trigger.get("kind") not in ("new_data", "persistent_drift"):
        raise ValueError("Trigger kind must be new_data or persistent_drift")
    if trigger["kind"] == "new_data":
        if set(trigger) != {"kind"}:
            raise ValueError("New-data trigger accepts only its kind")
    else:
        if set(trigger) != {"kind", "normalized_rmse", "minimum_windows", "minimum_seconds"}:
            raise ValueError("Persistent drift requires normalized_rmse, minimum_windows and minimum_seconds")
        threshold = trigger["normalized_rmse"]
        if isinstance(threshold, bool) or not isinstance(threshold, (int, float)) or not 0 < threshold <= 1:
            raise ValueError("Drift normalized_rmse must be within (0,1]")
        if type(trigger["minimum_windows"]) is not int or not 2 <= trigger["minimum_windows"] <= 20 or type(trigger["minimum_seconds"]) is not int or not 1 <= trigger["minimum_seconds"] <= 31536000:
            raise ValueError("Drift persistence requires 2–20 windows and 1–31536000 seconds")
    study = validate_study(load_json(Path(root) / "studies" / (safe_id(payload["study_id"]) + ".json")))
    action = payload.get("action", "frozen_model_tuning")
    if action not in ("frozen_model_tuning", "reidentify_and_tune"):
        raise ValueError("Unsupported policy action")
    if action == "reidentify_and_tune":
        from research_sources import qualify_generated_history
        qualify_generated_history(root, load_json(Path(root) / "datasets" / safe_id(study["dataset_id"]) / "history.json"))
    model_id = payload.get("model_id") or study.get("model_id")
    if not model_id:
        candidates = [load_model(root, path.stem) for path in (Path(root) / "models").glob("*.json")]
        candidates = [model for model in candidates if model["study_id"] == study["id"]]
        if len(candidates) != 1:
            raise ValueError("Select one frozen qualified model_id; continuous research never refits observational dynamics")
        model_id = candidates[0]["id"]
    model = load_model(root, model_id)
    if model["engineering_sha256"] != digest(study["config"]):
        raise ValueError("Frozen model engineering contract differs from policy study")
    if study["config"]["plant_source"] != "identified_history":
        raise ValueError("New-data policies require a history-driven study; synthetic runs use explicit manual scenarios")
    template = None
    if payload.get("publication_template_version_id"):
        if action != "frozen_model_tuning":
            raise ValueError("Automatic candidate sealing cannot accompany dynamic reidentification")
        from research_policy_candidates import publication_template
        template = publication_template(root, payload, study, model)
    trials = len(study["research"]["methods"]) * (1 + 2 * study["research"]["rounds"])
    if payload["max_trials_total"] < trials:
        raise ValueError("Policy trial budget cannot fund even one complete frozen study")
    definition = dict(payload, id="policy_" + uuid.uuid4().hex, model_id=model_id, created_at=now(),
                      previous_id=payload.get("id"), study_sha256=fingerprint(Path(root) / "studies" / (study["id"] + ".json")),
                      model_sha256=model["artifact_sha256"], trials_per_run=trials, action=action, trigger=trigger,
                      candidate_mode="automatic_review_only" if template else "manual_only_unbound")
    state = {"enabled": payload["enabled"], "revision": 1, "runs_started": 0, "trials_reserved": 0,
             "last_started_at": None, "last_decision": None}
    with database(root) as db:
        db.execute("BEGIN IMMEDIATE")
        if db.execute("SELECT count(*) FROM policies").fetchone()[0] >= 64:
            raise ValueError("Policy capacity reached; administrator retention review required")
        if payload.get("id"):
            old = db.execute("SELECT * FROM policies WHERE id=?", (safe_id(payload["id"]),)).fetchone()
            if old is None:
                raise ValueError("Previous policy does not exist")
            old_state = json.loads(old["state"])
            old_state.update(enabled=False, revision=old_state["revision"] + 1)
            _audit(db, old["id"], old_state, "superseded_pause_new_work")
        db.execute("INSERT INTO policies VALUES(?,?,?)", (definition["id"], json.dumps(definition), json.dumps(state)))
        _audit(db, definition["id"], state, "enabled" if state["enabled"] else "paused_new_work")
        return _record(db, db.execute("SELECT * FROM policies WHERE id=?", (definition["id"],)).fetchone())


def set_state(root, payload):
    from research import safe_id
    if set(payload) != {"id", "enabled", "expected_revision"} or type(payload["enabled"]) is not bool or type(payload["expected_revision"]) is not int:
        raise ValueError("Policy state requires id, enabled and expected_revision")
    with database(root) as db:
        db.execute("BEGIN IMMEDIATE")
        row = db.execute("SELECT * FROM policies WHERE id=?", (safe_id(payload["id"]),)).fetchone()
        if row is None:
            raise ValueError("Policy not found")
        state = json.loads(row["state"])
        if state["revision"] != payload["expected_revision"]:
            raise ValueError("Policy revision changed; refresh before changing new-work state")
        state.update(enabled=payload["enabled"], revision=state["revision"] + 1)
        _audit(db, row["id"], state, "enabled" if state["enabled"] else "paused_new_work")
        return _record(db, db.execute("SELECT * FROM policies WHERE id=?", (row["id"],)).fetchone())


def _candidate(root, definition, study, dataset):
    from research_design import match_history
    if dataset["kind"] != "clean" or dataset["created_at"] <= definition["created_at"]:
        return "no_new_clean_data", None
    if dataset.get("usable") is not True:
        return "unresolved_data_quality", None
    if dataset["row_count"] < definition["minimum_rows"]:
        return "insufficient_rows", None
    if not dataset.get("signal_stats") or any(row["filled_count"] / dataset["row_count"] > definition["max_filled_fraction"] for row in dataset["signal_stats"]):
        return "excessive_imputation", None
    path = Path(root) / "datasets" / dataset["id"] / "history.json"
    if fingerprint(path) != dataset["history_sha256"]:
        return "dataset_integrity_changed", None
    history = load_json(path)
    candidate = copy.deepcopy(study)
    # Time origin may advance; dimensions, physical scales, splits and all control
    # parameters must remain exactly the previously reviewed frozen contract.
    candidate["config"]["start"] = "relative" if history["provenance"]["cleaning"]["time_format"] == "seconds" else history["provenance"]["time_origin"]
    try:
        splits = match_history(candidate["config"], history)
    except (ValueError, KeyError) as error:
        return "incompatible_engineering_or_split: " + str(error)[:500], None
    # Require a different source interval/content, not another cleaning of old bytes.
    original = load_json(Path(root) / "datasets" / study["dataset_id"] / "summary.json")
    if dataset["source_sha256"] == original["source_sha256"]:
        return "same_source_not_new_data", None
    if definition["action"] == "reidentify_and_tune":
        from research_sources import qualify_generated_history
        try:
            qualify_generated_history(root, history)
        except (ValueError, OSError, KeyError) as error:
            return "closed_loop_identifiability_not_qualified: " + str(error)[:500], None
    train_end = splits[0]["last"]
    inputs = np.concatenate((history["mv"][:train_end], history["dv"][:train_end]), axis=1)
    if np.any(inputs.std(axis=0) < candidate["config"]["identification"]["minimum_input_std"]):
        return "insufficient_input_excitation_keep_frozen_model", None
    candidate.update(dataset_id=dataset["id"], dataset_sha256=dataset["history_sha256"], model_id=definition["model_id"])
    candidate["research"]["selection_mode"] = "automatic"
    candidate["research"]["policy_action"] = definition["action"]
    return None, candidate


def _source_start(root, history):
    source = history.get("provenance", {}).get("source", {})
    if source.get("kind") == "captured_acquisition":
        value = source["manifest"]["start"]
    elif source.get("kind") == "postgres":
        value = source["start"]
    elif source.get("kind") == "generated_synthetic":
        from research_sources import verify_generated_source
        run, _ = verify_generated_source(root, source)
        value = run["study"]["config"]["start"]
    else:
        value = history.get("provenance", {}).get("time_origin")
    if not isinstance(value, str):
        raise ValueError("source_interval_unknown")
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("source_interval_unknown")
    return parsed.timestamp()


def _drift_gate(db, root, definition, study, dataset):
    from research_design import study_splits
    trigger = definition["trigger"]
    if trigger["kind"] == "new_data":
        return None
    history = load_json(Path(root) / "datasets" / dataset["id"] / "history.json")
    original = load_json(Path(root) / "datasets" / study["dataset_id"] / "history.json")
    try:
        start = _source_start(root, history)
        original_end = _source_start(root, original) + len(original["time_seconds"]) * original["sample_time"]
    except (ValueError, KeyError, TypeError):
        return "source_interval_unknown"
    if start < original_end:
        return "drift_source_not_after_frozen_training_history"
    config = dict(study["config"], start="relative" if history["provenance"]["cleaning"]["time_format"] == "seconds" else history["provenance"]["time_origin"])
    validation = study_splits(config)[1]
    first, last = validation["first"], validation["last"]
    start, end = start + first * history["sample_time"], start + last * history["sample_time"]
    prior = db.execute("SELECT * FROM drift_windows WHERE policy_id=? AND source_hash=?",
                       (definition["id"], dataset["source_sha256"])).fetchone()
    if prior is None:
        if db.execute("SELECT 1 FROM drift_windows WHERE policy_id=? AND start<? AND end>?",
                      (definition["id"], end, start)).fetchone():
            return "overlapping_drift_windows"
        from identify import frozen_prediction_metrics
        model = load_model(root, definition["model_id"])["model"]
        observed = {"sample_time": history["sample_time"],
                    **{key: history[key][:last] for key in ("time_seconds", "cv", "mv", "dv")}}
        metrics = frozen_prediction_metrics(model, observed, first)["test"]
        ranges = np.asarray([row["high"] - row["low"] for row in study["config"]["variables"]["cv"]])
        normalized = np.asarray(metrics["rmse"]) / ranges
        above = bool(np.any(normalized > trigger["normalized_rmse"]))
        evidence = {"normalized_rmse": normalized.tolist(), "test_accessed": False,
                    "relative_interval": [first, last], "scope": "Frozen-model historical validation residual; not closed-loop improvement"}
        db.execute("INSERT INTO drift_windows VALUES(?,?,?,?,?,?)",
                   (definition["id"], dataset["source_sha256"], start, end, int(above), json.dumps(evidence)))
    rows = list(db.execute("SELECT start,end,above FROM drift_windows WHERE policy_id=? AND end<=? ORDER BY end DESC LIMIT ?",
                          (definition["id"], end, trigger["minimum_windows"])))
    if not rows or not rows[0]["above"]:
        return "no_validation_residual_drift"
    if len(rows) < trigger["minimum_windows"] or any(not row["above"] for row in rows):
        return "drift_not_sustained_across_qualified_windows"
    # Duration is observed validation time only, never unobserved gaps or test.
    if sum(row["end"] - row["start"] for row in rows) < trigger["minimum_seconds"]:
        return "insufficient_observed_drift_duration"
    return None


def tick(root):
    from datasets import list_datasets
    from research import atomic_json, create_run, now, reconcile_active
    root = Path(root)
    with database(root) as db:
        db.execute("INSERT OR REPLACE INTO scheduler VALUES(1,?,?)", (time.time(), "evaluating"))
        db.commit()
        db.execute("BEGIN IMMEDIATE")
        reconcile_active(root)
        datasets = sorted(list_datasets(root), key=lambda dataset: dataset["created_at"])
        for row in list(db.execute("SELECT * FROM policies ORDER BY rowid")):
            definition, state = json.loads(row["definition"]), json.loads(row["state"])
            if not state["enabled"]:
                continue
            if (root / "active.json").exists():
                _audit(db, row["id"], state, "worker_busy")
                continue
            if state["runs_started"] >= definition["max_runs"] or state["trials_reserved"] + definition["trials_per_run"] > definition["max_trials_total"]:
                _audit(db, row["id"], state, "budget_exhausted")
                continue
            if state["last_started_at"] is not None and time.time() - state["last_started_at"] < definition["cooldown_seconds"]:
                _audit(db, row["id"], state, "cooldown")
                continue
            if shutil.disk_usage(root).free < 512 * 1024 * 1024:
                _audit(db, row["id"], state, "insufficient_disk_capacity")
                continue
            study_path = root / "studies" / (definition["study_id"] + ".json")
            if fingerprint(study_path) != definition["study_sha256"] or load_model(root, definition["model_id"])["artifact_sha256"] != definition["model_sha256"]:
                _audit(db, row["id"], state, "frozen_inputs_changed")
                continue
            study = load_json(study_path)
            decision = "no_new_clean_data"
            for dataset in datasets:
                decision, candidate = _candidate(root, definition, study, dataset)
                if candidate is None:
                    if decision != "no_new_clean_data":
                        _audit(db, row["id"], state, decision, dataset["id"])
                    continue
                key = digest({"policy": row["id"], "model": definition["model_sha256"], "source": dataset["source_sha256"]})
                if db.execute("SELECT 1 FROM claims WHERE key=?", (key,)).fetchone():
                    _audit(db, row["id"], state, "duplicate_source_claim", dataset["id"])
                    continue
                drift_reason = _drift_gate(db, root, definition, study, dataset)
                if drift_reason:
                    _audit(db, row["id"], state, drift_reason, dataset["id"])
                    continue
                run_id = key[:32]
                candidate.update(id="study_" + key[:32], previous_id=study["id"], created_at=now(), policy_id=row["id"],
                                 policy_origin_study_id=definition["study_id"], policy_revision=state["revision"],
                                 policy_definition_sha256=digest(definition))
                atomic_json(root / "studies" / (candidate["id"] + ".json"), candidate)
                # Durable reservation precedes side effects. Unknown/crashed launches
                # consume budget and remain deduplicated, never automatic replay.
                db.execute("INSERT INTO claims VALUES(?,?,?,?)", (key, row["id"], run_id, time.time()))
                state.update(runs_started=state["runs_started"] + 1, trials_reserved=state["trials_reserved"] + definition["trials_per_run"], last_started_at=time.time())
                _audit(db, row["id"], state, "launch_reserved", dataset["id"], run_id)
                db.commit()
                try:
                    run = create_run(root, {"study_id": candidate["id"], "reserved_run_id": run_id})
                except Exception as error:
                    db.execute("BEGIN IMMEDIATE")
                    state = json.loads(db.execute("SELECT state FROM policies WHERE id=?", (row["id"],)).fetchone()[0])
                    _audit(db, row["id"], state, "launch_failed: " + str(error)[:500], dataset["id"], run_id)
                    return {"run": None, "reason": "launch_failed"}
                db.execute("BEGIN IMMEDIATE")
                state = json.loads(db.execute("SELECT state FROM policies WHERE id=?", (row["id"],)).fetchone()[0])
                _audit(db, row["id"], state, "scheduled_" + definition["action"], dataset["id"], run_id)
                db.execute("INSERT OR REPLACE INTO scheduler VALUES(1,?,?)", (time.time(), "worker_scheduled"))
                return {"run": run, "policy_id": row["id"]}
            if decision == "no_new_clean_data":
                _audit(db, row["id"], state, decision)
        db.execute("INSERT OR REPLACE INTO scheduler VALUES(1,?,?)", (time.time(), "idle"))
        return {"run": None}


def execute(root, operation, payload):
    if operation == "policy-finalize":
        from research_policy_candidates import finalize_run
        return finalize_run(root, payload["id"])
    if operation == "policy-reconcile":
        from research_policy_candidates import reconcile_seals
        return reconcile_seals(root)
    if operation == "policy-heartbeat":
        with database(root) as db:
            db.execute("INSERT OR REPLACE INTO scheduler VALUES(1,?,?)", (time.time(), "worker_busy"))
        return {"status": "worker_busy"}
    if operation == "policy-list":
        return list_policies(root)
    if operation == "policy-save":
        return save_policy(root, payload)
    if operation == "policy-state":
        return set_state(root, payload)
    if operation == "policy-tick":
        return tick(root)
    raise ValueError("Unknown policy operation")

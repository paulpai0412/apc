"""Freeze reviewed local research evidence, never deploy or grant factory authority.

Only the four public functions below form this storage boundary. The caller must
supply selected_by from its authenticated identity, not from the request body.
Archived receipts are checked without executing or opening their argv paths.
Run source hashes were collected at completion, not at process startup. Hashes
provide content integrity and lineage, not signatures or independent plant proof.
"""
import hashlib
import json
import math
import os
from pathlib import Path
import re
import tempfile
from datetime import datetime, timezone

import numpy as np

from campaign import slice_scenario, validate_result
from identify import frozen_prediction_metrics
from research import METHODS, make_controller, parameter_key, safe_id, validate_study
from research_design import match_history, study_splits
from research_evaluation import aggregate_evaluations, research_metrics
from research_trends import ForecastArchive


_DIGEST = re.compile(r"[0-9a-f]{64}")


def _require(condition, message):
    if not condition:
        raise ValueError(message)


def _canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, allow_nan=False, separators=(",", ":")).encode("utf-8")


def _decode(data):
    def pairs(items):
        result = {}
        for key, value in items:
            _require(key not in result, f"Duplicate JSON field: {key}")
            result[key] = value
        return result

    def invalid(value):
        raise ValueError(f"Non-finite JSON number: {value}")

    return json.loads(data, object_pairs_hook=pairs, parse_constant=invalid)


def _digest(value):
    _require(isinstance(value, str) and _DIGEST.fullmatch(value), "Invalid SHA-256 digest")
    return value


def _path(root, *components):
    """Keep every artifact beneath the configured root, including symlink parents."""
    path = root
    for component in components:
        _require(isinstance(component, str) and re.fullmatch(r"[A-Za-z0-9_.-]+", component)
                 and component not in (".", ".."), "Invalid artifact path component")
        path = path / component
        _require(not path.is_symlink(), "Symlink artifacts are not allowed")
    _require(path.resolve().is_relative_to(root), "Artifact escapes workspace")
    return path


def _hash(path):
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


class _Evidence:
    def __init__(self, root, run_id):
        self.root = root
        self.directory = _path(root, "runs", run_id)
        self.hashes = {}
        self.stamps = {}

    def path(self, name):
        return _path(self.root, "runs", self.directory.name, name)

    def _remember(self, name, before, digest):
        path = self.path(name)
        after = path.stat()
        stamp = (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns)
        previous = (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns)
        _require(previous == stamp, f"Evidence changed while reading: {name}")
        if name in self.hashes:
            _require(self.hashes[name] == digest and self.stamps[name] == stamp, f"Evidence changed: {name}")
        self.hashes[name], self.stamps[name] = digest, stamp

    def data(self, name):
        path = self.path(name)
        before = path.stat()
        data = path.read_bytes()
        self._remember(name, before, hashlib.sha256(data).hexdigest())
        return data

    def json(self, name):
        return _decode(self.data(name))

    def hash(self, name):
        if name not in self.hashes:
            path = self.path(name)
            before = path.stat()
            self._remember(name, before, _hash(path))
        return self.hashes[name]

    def unchanged(self):
        # Catch concurrent edits without rereading gigabytes of forecast archives.
        for name, stamp in self.stamps.items():
            current = self.path(name).stat()
            _require(stamp == (current.st_dev, current.st_ino, current.st_size, current.st_mtime_ns, current.st_ctime_ns),
                     f"Evidence changed during selection: {name}")


def _receipt(evidence, name, controller, plant, scenario, engine):
    receipt = evidence.json(name + ".command.json")
    _require(type(receipt.get("returncode")) is int and receipt["returncode"] == 0
             and not receipt.get("cancelled", False), f"Incomplete execution receipt: {name}")
    expected = {"output_sha256": evidence.hash(name + ".json"), "engine_sha256": engine,
                "controller_sha256": evidence.hash(controller) if controller else None,
                "plant_sha256": evidence.hash(plant), "scenario_sha256": evidence.hash(scenario)}
    for key, value in expected.items():
        _require(receipt.get(key) == value, f"{name}: receipt {key} differs from frozen evidence")
    return receipt


def _execution(evidence, name, controller, plant, scenario_name, scenario, config, engine, method):
    result = evidence.json(name + ".json")
    receipt = _receipt(evidence, name, controller, plant, scenario_name, engine)
    valid, reason = validate_result(result, scenario, config)
    _require(valid, f"{name}: {reason}")
    _require(result.get("control_strategy", {}).get("method") == method, f"{name}: executed method mismatch")
    return result, receipt


def _forecast(evidence, name, receipt, result, config, controller):
    filename = name + ".forecast.ndjson"
    _require(receipt.get("forecast_sha256") == evidence.hash(filename)
             and receipt.get("forecast_index_sha256") == evidence.hash(filename + ".index.json"),
             f"{name}: forecast receipt mismatch")
    archive = ForecastArchive(evidence.path(filename), config["sample_time"],
                              len(config["variables"]["cv"]), len(config["variables"]["mv"]))
    trace = result["trace"]
    count = len(trace["time_seconds"])
    _require(len(archive.offsets) == count, f"{name}: incomplete forecast recording")
    for key in ("prediction_horizon", "control_horizon"):
        _require(archive.header[key] == controller["tuning"][key], f"{name}: forecast horizon differs from controller")
    predicted = np.asarray(trace.get("predicted_cv", []), dtype=float)
    _require(predicted.shape == (count, len(config["variables"]["cv"])) and np.isfinite(predicted).all(),
             f"{name}: incomplete predicted trajectory")
    for i, row in enumerate(archive.rows()):
        _require(row["sample"] == i and row["issued_at_seconds"] == trace["time_seconds"][i],
                 f"{name}: forecast timeline mismatch")
        _require(np.allclose(row["cv"][:predicted.shape[1]], predicted[i], rtol=1e-12, atol=1e-12),
                 f"{name}: forecast first prediction differs from executed controller")


def _structure(record, study, selection, method):
    _require(record.get("status") == "completed" and record.get("stage") == "completed", "Only completed research runs can be selected")
    _require(record.get("study") == study and record.get("study_id") == study["id"], "Frozen study/run relationship differs")
    safe_id(study["id"])
    if study.get("dataset_id") is not None:
        safe_id(study["dataset_id"])
    validated = validate_study(study)
    config, research = study["config"], study["research"]
    _require(validated["research"]["objective"] == research["objective"], "Frozen objective is not canonical")
    _require(method in research["methods"], "Method was not part of the frozen study")
    results = record["results"]
    _require(selection["objective"] == results["objective"] == research["objective"], "Frozen selection objective differs")
    _require(set(selection["winners"]) == set(research["methods"]), "Frozen winners omit a study method")
    rows = results["methods"]
    _require(len(rows) == len(research["methods"]) and {row["method"] for row in rows} == set(research["methods"]),
             "Run methods must uniquely match the complete frozen study")
    row = next(row for row in rows if row["method"] == method)
    _require(row.get("recommendation") == "candidate_for_review" and row.get("selection_recommendation") == "candidate",
             "Only a qualified non-baseline candidate_for_review can be selected")
    total = 2 + int(config["plant_source"] == "identified_history") + len(research["methods"]) * (
        (1 + 2 * research["rounds"]) * len(research["scenarios"]) + 2 * len(research["scenarios"]) + 1)
    progress = record["progress"]
    _require(type(progress["completed"]) is int and type(progress["total"]) is int
             and progress["completed"] == progress["total"] == total, "Incomplete expected research work")
    trials = record["trials"]
    _require(len(trials) == len(research["methods"]) * (1 + 2 * research["rounds"])
             and len({trial["id"] for trial in trials}) == len(trials), "Incomplete or duplicate trial coverage")
    checks = record["checks"]
    expected_checks = set()
    for declared_method in research["methods"]:
        method_trials = [trial for trial in trials if trial["method"] == declared_method]
        _require([sum(trial["round"] == r for trial in method_trials) for r in range(research["rounds"] + 1)]
                 == [1] + [2] * research["rounds"], "Incomplete adaptive round coverage")
        for variant in ("baseline", "optimized", "transport"):
            expected_checks.add(f"{declared_method}-{variant}-full-period")
        expected_checks.add(declared_method + "-actual-com-equivalence")
        for definition in research["scenarios"]:
            if definition["id"] != "nominal":
                for variant in ("baseline", "optimized"):
                    expected_checks.add(f"{declared_method}-{variant}-{definition['id']}-full-period")
    _require(len(checks) == len(expected_checks) and {check["name"] for check in checks} == expected_checks
             and all(check["status"] == "passed" for check in checks), "Incomplete or failed final execution checks")
    splits = study_splits(config)
    _require(results["splits"] == splits and results["variables"] == config["variables"], "Result interval/variables differ from study")
    model_test = results["model_test"]
    _require(model_test.get("passed") is True and model_test.get("used_for_selection") is False,
             "A passing non-selection held-out model test is required")
    return config, research, splits, row


def _selection(evidence, record, selection, method, config, research, scenario, engine, base_model):
    trials = [trial for trial in record["trials"] if trial["method"] == method]
    _require(trials[0]["round"] == 0 and trials[0]["parent_trial"] is None
             and trials[0]["parameters"] == research["initial"], "Missing initial baseline trial")
    baseline = trials[0]
    seen = {}
    signatures = set()
    for trial in trials:
        identifier = safe_id(trial["id"])
        _require(trial["controller"] == identifier + "-controller.json", "Trial controller path is not its frozen artifact")
        parameters = trial["parameters"]
        _require(set(parameters) == set(research["initial"]), "Trial tuning coordinates differ from study")
        for key, value in parameters.items():
            _require(type(value) in (int, float) and math.isfinite(value)
                     and research["bounds"][key][0] <= value <= research["bounds"][key][1], "Trial parameter exceeds frozen bounds")
        signature = parameter_key(parameters)
        _require(signature not in signatures, "Duplicate adaptive trial parameters")
        signatures.add(signature)
        if trial["round"] > 0:
            earlier = [item for item in seen.values() if item["eligible"] and item["round"] < trial["round"]]
            incumbent = min(earlier, key=lambda item: item["score"]) if earlier else baseline
            _require(trial["parent_trial"] == incumbent["id"], "Trial parent differs from preceding eligible incumbent")
        controller = evidence.json(trial["controller"])
        _require(controller == make_controller(base_model, method, parameters, research["bands"]),
                 "Trial controller does not derive from the frozen identified model and parameters")
        if trial["valid"] is not True:
            _require(trial.get("eligible") is False and trial.get("failures") and trial["round"] > 0,
                     "Failed trial cannot be a baseline or eligible candidate")
            failures, successful = [], {}
            for definition in research["scenarios"]:
                scenario_id = definition["id"]
                name = identifier if scenario_id == "nominal" else identifier + "-" + scenario_id
                receipt = evidence.json(name + ".command.json")
                _require(receipt["controller_sha256"] == evidence.hash(trial["controller"])
                         and receipt["engine_sha256"] == engine, "Failed trial receipt identity mismatch")
                if receipt.get("returncode") != 0 or receipt.get("cancelled"):
                    failures.append(scenario_id)
                    continue
                result = evidence.json(name + ".json")
                _require(receipt.get("output_sha256") == evidence.hash(name + ".json"), "Failed trial output changed")
                valid, _ = validate_result(result, scenario, config)
                if not valid:
                    failures.append(scenario_id)
                    continue
                try:
                    successful[scenario_id] = research_metrics(result["trace"], config, research["objective"])
                except ValueError:
                    failures.append(scenario_id)
            _require(failures and successful == trial["scenario_metrics"], "Failed trial lacks archived failure evidence")
            seen[identifier] = trial
            continue
        metrics = {}
        for definition in research["scenarios"]:
            scenario_id = definition["id"]
            name = identifier if scenario_id == "nominal" else identifier + "-" + scenario_id
            plant = "plant.json" if scenario_id == "nominal" else "plant-" + scenario_id + ".json"
            result, _ = _execution(evidence, name, trial["controller"], plant, "selection-scenario.json",
                                   scenario, config, engine, method)
            metrics[scenario_id] = research_metrics(result["trace"], config, research["objective"])
            _require(trial["scenario_samples"][scenario_id] == len(scenario["time_seconds"]), "Trial omitted validation scans")
        _require(metrics == trial["scenario_metrics"] and trial["samples"] == len(scenario["time_seconds"])
                 and trial["metrics"] == metrics["nominal"], "Trial metrics differ from complete validation traces")
        assessment = aggregate_evaluations(metrics, baseline["scenario_metrics"], config, research["objective"], baseline=trial["round"] == 0)
        _require(trial["assessment"] == assessment and trial["eligible"] is assessment["eligible"]
                 and trial["score"] == assessment["score"], "Trial acceptance differs from frozen all-scenario gates")
        seen[identifier] = trial
    chosen = selection["winners"][method]
    eligible = [trial for trial in trials if trial["eligible"]]
    _require(eligible, "No eligible frozen candidate")
    if research.get("selection_mode", "automatic") == "manual":
        decision = evidence.json("selection-request.json")
        _require(decision == record.get("selection_decision") and decision.get("reason") and decision.get("selected_by"),
                 "Missing immutable pre-sealed engineering selection")
        winner = next((trial for trial in eligible if trial["id"] == decision["selections"].get(method)), None)
        _require(winner is not None, "Manual selection is not an eligible development trial")
    else:
        winner = min(eligible, key=lambda trial: trial["score"])
    _require(chosen["baseline"] == baseline and chosen["best"] == winner and winner["id"] != baseline["id"]
             and chosen["selection_recommendation"] == "candidate", "Frozen winner differs from pre-sealed selection")
    coverage = {key: sorted({trial["parameters"][key] for trial in trials}) for key in research["initial"]}
    _require(chosen["coverage"] == coverage and all(len(values) >= 2 for values in coverage.values()), "Incomplete tuning coordinate coverage")
    return baseline, winner


def _qualify(root, run_id, method):
    evidence = _Evidence(root, run_id)
    record = evidence.json("run.json")
    _require(record["id"] == run_id, "Run ID differs from containing directory")
    study = evidence.json("study.json")
    selection = evidence.json("selection.json")
    config, research, splits, row = _structure(record, study, selection, method)
    provenance = record["provenance"]
    engine = _digest(provenance["engine"])
    _require(provenance["study"] == evidence.hash("study.json"), "Frozen study hash differs from completed run")
    _require(isinstance(provenance["sources"], dict) and provenance["sources"], "Missing source provenance")
    for name, digest in provenance["sources"].items():
        _require(isinstance(name, str) and re.fullmatch(r"[A-Za-z0-9_-]+\.py", name), "Invalid source provenance name")
        _digest(digest)
    if "runtime" in provenance:
        _require(provenance["runtime"] == evidence.hash("runtime.json"), "Frozen execution runtime changed")
    _require(evidence.json("trials.json") == record["trials"], "Frozen trial log differs from completed run")
    scenario = evidence.json("scenario.json")
    count = splits[-1]["last"]
    _require(scenario["sample_time"] == config["sample_time"]
             and scenario["time_seconds"] == [i * config["sample_time"] for i in range(count)], "Incomplete or shifted full scenario")
    _require(scenario.get("provenance", {}).get("evaluation_plant") == config["plant_source"],
             "Scenario evaluation provenance differs from frozen study")
    selection_scenario = evidence.json("selection-scenario.json")
    _require(selection_scenario == slice_scenario(scenario, splits[1]["first"], splits[1]["last"]),
             "Selection scenario is not the frozen chronological validation interval")
    plant = evidence.json("plant.json")
    evidence.json("plant-evidence.json")
    if config["plant_source"] == "independent_synthetic":
        _require(plant == config["plant"], "Evaluation plant differs from independent frozen study")
    for definition in research["scenarios"]:
        if definition["id"] != "nominal":
            alternate = evidence.json("plant-" + definition["id"] + ".json")
            expected_paths = [[dict(path, gain=path["gain"] * definition["gain_multiplier"]) for path in paths] for paths in plant["paths"]]
            _require(alternate == dict(plant, paths=expected_paths), "Uncertainty plant differs from declared gain scenario")
    base_model = evidence.json("baseline-controller.json")
    if study.get("model_id"):
        _require(provenance.get("frozen_model") == evidence.hash("frozen-model.json"), "Frozen identification artifact changed")
        frozen_model = evidence.json("frozen-model.json")
        _require(frozen_model["id"] == study["model_id"] and plant == frozen_model["evaluation_plant"],
                 "Policy changed the frozen model identity or evaluation plant")
        if research.get("policy_action") != "reidentify_and_tune":
            _require(base_model == frozen_model["model"], "Frozen-model tuning refitted or replaced its control model")
    history = evidence.json("history.json")
    _require(len(history["time_seconds"]) == count, "Held-out history does not cover the complete study")
    if study.get("dataset_id"):
        _require(study["dataset_sha256"] == evidence.hash("history.json"), "Frozen cleaned history hash differs")
        match_history(config, history)
    else:
        _receipt(evidence, "history", None, "plant.json", "scenario.json", engine)
        history["mv"] = history["command_mv"]
    identification = evidence.json("identification.json")
    _require(identification == record["results"]["identification"], "Held-out identification report differs from run")
    measured = frozen_prediction_metrics(base_model, history, splits[2]["first"])
    _require(all(identification.get(key) == value for key, value in measured.items()), "Held-out model metrics differ from frozen history")
    normalized = (np.asarray(measured["test"]["rmse"]) / np.asarray([cv["high"] - cv["low"] for cv in config["variables"]["cv"]])).tolist()
    model_test = {"passed": all(value <= config["modelling"]["validation_max_normalized_rmse"] for value in normalized),
                  "normalized_rmse": normalized, "used_for_selection": False}
    _require(model_test == record["results"]["model_test"] and model_test["passed"], "Held-out model qualification failed")
    del history
    baseline, winner = _selection(evidence, record, selection, method, config, research, selection_scenario, engine, base_model)
    _require(row["selected_trial"] == winner["id"] and row["parameters"] == winner["parameters"]
             and row["coverage"] == selection["winners"][method]["coverage"], "Report does not identify the frozen winner")
    model_bytes = evidence.data(winner["controller"])
    controller = _decode(model_bytes)
    test_metrics = {"baseline": {}, "optimized": {}}
    direct = None
    for variant, selected in (("baseline", baseline), ("optimized", winner), ("transport", winner)):
        name = method + "-" + variant
        result, receipt = _execution(evidence, name, selected["controller"], "plant.json", "scenario.json", scenario, config, engine, method)
        trace = result["trace"]
        measured = {"full": research_metrics(trace, config, research["objective"]),
                    "test": research_metrics(trace, config, research["objective"], splits[2]["first"], splits[2]["last"])}
        _require(measured == row[variant], f"{name}: reported metrics differ from complete held-out/full traces")
        _forecast(evidence, name, receipt, result, config, controller)
        if variant != "transport":
            test_metrics[variant]["nominal"] = measured["test"]
        if variant == "optimized":
            direct = {key: np.asarray(trace[key]) for key in ("time_seconds", "cv", "mv", "dv", "targets", "command_mv")}
        if variant == "transport":
            stats = result["transport_stats"]
            _require(result["transport"] == "opc-da" and stats["write_calls"] == stats["update_calls"] == count
                     and stats["read_calls"] >= 2 * count and stats["read_errors"] == stats["write_errors"] == stats["bad_quality_items"] == 0,
                     "Incomplete or faulty actual COM DA transport evidence")
            for key, native in direct.items():
                _require(np.allclose(native, trace[key], atol=config["transport"]["absolute_tolerance"], rtol=config["transport"]["relative_tolerance"]),
                         f"Complete native/COM trajectories differ: {key}")
            direct = None
    for definition in research["scenarios"]:
        scenario_id = definition["id"]
        if scenario_id == "nominal":
            continue
        for variant, selected in (("baseline", baseline), ("optimized", winner)):
            name = method + "-" + variant + "-" + scenario_id
            result, _ = _execution(evidence, name, selected["controller"], "plant-" + scenario_id + ".json",
                                   "scenario.json", scenario, config, engine, method)
            test_metrics[variant][scenario_id] = research_metrics(result["trace"], config, research["objective"], splits[2]["first"], splits[2]["last"])
    _require(test_metrics == row["scenario_tests"], "Held-out uncertainty metrics differ from complete traces")
    assessment = aggregate_evaluations(test_metrics["optimized"], test_metrics["baseline"], config, research["objective"])
    _require(assessment == row["assessment"] and assessment["eligible"] is True, "Held-out all-scenario assessment is not eligible")
    transported = dict(test_metrics["optimized"], nominal=row["transport"]["test"])
    _require(aggregate_evaluations(transported, test_metrics["baseline"], config, research["objective"])["eligible"],
             "Selected COM DA held-out trajectory fails the frozen acceptance gates")
    evidence.unchanged()
    return evidence, record, study, winner, row, model_bytes


def _write_synced(path, data):
    with path.open("xb") as handle:
        handle.write(data)
        handle.flush()
        os.fsync(handle.fileno())


def create_version(root, payload, selected_by):
    """Return a frozen manifest with optional reference-only Runtime binding.

    Full archived evidence is requalified synchronously. No executable is run.
    Storage is append-only: private staging is atomically renamed. Published
    directories are always nonempty, so rename cannot replace an existing version.
    """
    binding_fields = {"deployment_profile_hash", "target", "equipment_instance_ids", "operating_envelope"}
    author_fields = {"author_user_id", "author_identity_realm"}
    _require(isinstance(payload, dict) and {"run_id", "method"} <= set(payload)
             and not set(payload) - {"run_id", "method"} - binding_fields - author_fields, "Unexpected candidate selection field")
    binding = {key: payload[key] for key in binding_fields if key in payload}
    author = {key: payload[key] for key in author_fields if key in payload}
    if author:
        _require(set(author) == author_fields and type(author["author_user_id"]) is int and author["author_user_id"] > 0,
                 "Complete server-owned author ID and identity realm required")
        _digest(author["author_identity_realm"])
    if binding:
        _require(set(binding) == binding_fields and author, "Candidate Runtime binding and server author must be complete; missing commissioning values cannot be guessed")
        _digest(binding["deployment_profile_hash"])
        target = binding["target"]
        _require(isinstance(target, dict) and set(target) == {"plant", "line", "area", "group"}
                 and all(isinstance(value, str) and 1 <= len(value.strip()) <= 128 for value in target.values()), "Exact Runtime target required")
        equipment = binding["equipment_instance_ids"]
        _require(isinstance(equipment, list) and 1 <= len(equipment) <= 128
                 and all(isinstance(value, str) and 1 <= len(value) <= 128 for value in equipment)
                 and len(set(equipment)) == len(equipment), "Exact unique equipment instance set required")
        binding["equipment_instance_ids"] = sorted(equipment)
        _require(isinstance(binding["operating_envelope"], str) and 1 <= len(binding["operating_envelope"].strip()) <= 200,
                 "An approved operating-envelope identifier is required")
    run_id = safe_id(payload["run_id"])
    method = payload["method"]
    _require(isinstance(method, str) and method in METHODS, "Unknown control method")
    _require(isinstance(selected_by, str) and selected_by.strip() == selected_by and 0 < len(selected_by) <= 256,
             "An authenticated selecting identity is required")
    root = Path(root).resolve()
    try:
        evidence, record, study, winner, row, model_bytes = _qualify(root, run_id, method)
        manifest = {"schema_version": 1, "created_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
                    "selected_by": selected_by, "run_id": run_id, "study_id": study["id"], "dataset_id": study.get("dataset_id"),
                    "process_area": study["config"]["process_area"], "method": method, "trial_id": winner["id"],
                    "model_sha256": hashlib.sha256(model_bytes).hexdigest(), "authority": "review_only",
                    "evidence_scope": study["config"]["plant_source"] + "; complete declared local simulation, native/COM DA parity and uncertainty scenarios; explicit review selection only, never deployment or factory authorization",
                    "parameters": winner["parameters"], "variables": study["config"]["variables"],
                    "metrics": {"baseline": row["baseline"]["test"], "selected": row["transport"]["test"]},
                    "assessment": row["assessment"], "model_test": record["results"]["model_test"],
                    "source_hashes": record["provenance"]["sources"], "evidence_hashes": evidence.hashes,
                    "objective": study["research"]["objective"], "splits": record["results"]["splits"],
                    "provenance": {"engine_sha256": record["provenance"]["engine"], "study_sha256": record["provenance"]["study"],
                                   "source_hash_timing": record["provenance"].get("source_hash_timing", "completion_time_files; not a startup source snapshot"),
                                   "verification": "Stored receipts and complete trajectories rechecked with current evaluation helpers; installed binaries and sources need not match archived completion hashes",
                                   "integrity": "Content hashes, not signed attestations or independent factory validation"}}
        manifest.update(binding)
        manifest.update(author)
        if study.get("policy_id"):
            from research_policy_candidates import policy_lineage
            manifest.update(policy_lineage(root, record))
        manifest["holdout"] = record.get("holdout", {"independent": False, "status": "legacy_exposure_not_attested"})
        manifest["runtime_profile_policy"] = "Reference and comparison only; candidate cannot override tags, units, modes, hard limits, max_move or handoff"
        manifest["id"] = hashlib.sha256(_canonical(manifest)).hexdigest()
        versions = _path(root, "versions")
        versions.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix=".pending-", dir=versions) as temporary:
            staged = Path(temporary) / "version"
            staged.mkdir()
            _write_synced(staged / "model.json", model_bytes)
            _write_synced(staged / "version.json", _canonical(manifest))
            target = _path(root, "versions", manifest["id"])
            _require(not target.exists(), "Version ID already exists; immutable artifacts cannot be overwritten")
            evidence.unchanged()
            # Never publish an empty directory. Concurrent publishers can only
            # encounter a nonempty target, which rename must not overwrite.
            # No lock file can survive a killed publisher and block future work.
            staged.rename(target)
        return manifest
    except (KeyError, TypeError, IndexError, OSError) as error:
        raise ValueError(f"Incomplete or invalid frozen version evidence: {error}") from error


def _load_version(root, version_id):
    _digest(version_id)
    root = Path(root).resolve()
    raw = _path(root, "versions", version_id, "version.json").read_bytes()
    manifest = _decode(raw)
    _require(isinstance(manifest, dict) and manifest.get("id") == version_id, "Version manifest ID mismatch")
    content = {key: value for key, value in manifest.items() if key != "id"}
    _require(hashlib.sha256(_canonical(content)).hexdigest() == version_id and raw == _canonical(manifest), "Version manifest integrity failure")
    _require(type(manifest.get("schema_version")) is int and manifest["schema_version"] == 1
             and manifest.get("authority") == "review_only", "A control version can only grant review_only authority")
    model = _path(root, "versions", version_id, "model.json").read_bytes()
    _require(hashlib.sha256(model).hexdigest() == _digest(manifest["model_sha256"]), "Version model integrity failure")
    return manifest, model


def get_version(root, version_id):
    """Return one verified manifest; corruption is an error, never repaired."""
    try:
        return _load_version(root, version_id)[0]
    except (KeyError, TypeError, OSError) as error:
        raise ValueError(f"Unreadable control version: {error}") from error


def get_version_model(root, version_id):
    """Return exact original controller JSON bytes after manifest/model checks."""
    try:
        return _load_version(root, version_id)[1]
    except (KeyError, TypeError, OSError) as error:
        raise ValueError(f"Unreadable control version: {error}") from error


def get_version_bundle(root, version_id):
    """Export verified, exact artifact bytes; this grants no installation authority."""
    try:
        manifest, model = _load_version(root, version_id)
        manifest_bytes = _canonical(manifest)
        return {"version_id": manifest["id"], "model_sha256": manifest["model_sha256"],
                "model_json": model.decode("utf-8"), "manifest_json": manifest_bytes.decode("utf-8"),
                "manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest()}
    except (KeyError, TypeError, OSError, UnicodeError) as error:
        raise ValueError(f"Unreadable control version: {error}") from error


def list_versions(root):
    """Return newest-first verified manifests; never hide a corrupt published ID."""
    root = Path(root).resolve()
    directory = _path(root, "versions")
    if not directory.exists():
        return []
    result = []
    for path in directory.iterdir():
        if path.name.startswith(".pending-"):
            continue
        _digest(path.name)
        result.append(get_version(root, path.name))
    return sorted(result, key=lambda item: (item["created_at"], item["id"]), reverse=True)

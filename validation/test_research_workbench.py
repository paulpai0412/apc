"""Consumer-boundary regressions. No native process, network or live control is run."""
import copy
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from common import load_json
from datasets import execute as datasets
from research import atomic_json, defaults, execute, fail_run, ResearchRun, validate_study
from research_policy import database, list_policies, save_policy, set_state, tick
from research_workbench import capabilities, digest, load_model, pareto_trials, select_trials
from test_research_plant import fixture


class WorkbenchTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def history_study(self):
        engineering, history, _ = fixture(1, 1, 0)
        config = defaults()["campaign"]
        for key in ("plant", "scenario"):
            config.pop(key, None)
        config.update({key: value for key, value in engineering.items() if key not in ("controller", "identification")})
        config.update(start="relative", plant_source="identified_history", engineering_confirmed=True,
                      data_window={"samples": 1600, "train_end": 1000, "validation_end": 1300})
        config["controller"].update(engineering["controller"])
        config["identification"].update(engineering["identification"], minimum_input_std=[0.01])
        history["signals"] = [{"name": row["name"], "unit": row["unit"], "role": role}
                              for role in ("cv", "mv", "dv") for row in config["variables"][role]]
        raw = datasets(self.root, "import", {"name": "Synthetic engineering fixture", "format": "history", "content": json.dumps(history)})
        cleaning = {"time_column": "time_seconds", "time_format": "seconds", "sample_time": 2,
                    "train_end": 2000, "validation_end": 2600, "duplicate_policy": "first",
                    "missing_policy": "reject", "max_gap_seconds": 0, "outlier_policy": "none",
                    "signals": raw["suggested_signals"]}
        clean = datasets(self.root, "clean", {"dataset_id": raw["id"], "cleaning": cleaning})["dataset"]
        research = defaults()["research"]
        research["bands"] = [{"half_width": 0.5, "tracking_scale": 0.05, "quadratic_penalty": 1.0}]
        from research_evaluation import objective_defaults
        research["objective"] = objective_defaults(config)
        research["objective"]["scenarios"] = [row["id"] for row in research["scenarios"]]
        study = execute(self.root, "save-study", {"name": "Qualified fixture", "dataset_id": clean["id"], "config": config, "research": research})
        return study, history, cleaning

    def model_policy(self):
        study, history, cleaning = self.history_study()
        model = execute(self.root, "identify-study", {"study_id": study["id"]})
        policy = save_policy(self.root, {"name": "New-data tuning", "study_id": study["id"], "model_id": model["id"],
                    "enabled": True, "minimum_rows": 1000, "max_filled_fraction": 0.01,
                    "cooldown_seconds": 30, "max_runs": 3, "max_trials_total": 100})
        return study, model, policy, history, cleaning

    def new_clean(self, history, cleaning, suffix):
        changed = copy.deepcopy(history)
        changed["cv"] = [[row[0] + suffix] for row in changed["cv"]]
        raw = datasets(self.root, "import", {"name": "Later captured fixture", "format": "history", "content": json.dumps(changed)})
        return datasets(self.root, "clean", {"dataset_id": raw["id"], "cleaning": cleaning})["dataset"]

    def test_capabilities_do_not_conflate_loading_with_identification(self):
        families = {row["id"]: row for row in capabilities()["identification"]}
        self.assertTrue(families["fir"]["supported"])
        self.assertTrue(families["fopdt"]["supported"])
        self.assertEqual(families["sopdt"]["scope"], "control_model")
        self.assertFalse(capabilities()["automatic_reidentification_observational"])
        self.assertEqual(capabilities()["automatic_reidentification_scope"], "verified_server_synthetic_open_loop_only")
        self.assertFalse(capabilities()["runtime_write"])

    def test_captured_file_preserves_crlf_bom_and_rejects_mutated_digest(self):
        content = "\ufefftime,cv,mv\r\n0,1,2\r\n1,2,3\r\n"
        manifest = {"schema_version": 1, "source_id": "capture:mill-1", "captured_at": "2026-02-01T00:00:00Z",
                    "start": "2026-01-01T00:00:00Z", "end": "2026-01-02T00:00:00Z", "format": "csv",
                    "sha256": hashlib.sha256(content.encode()).hexdigest(), "read_only": True}
        payload = {"name": "Captured", "content": content, "manifest": manifest}
        result = execute(self.root, "import-snapshot", payload)
        self.assertEqual((self.root / "datasets" / result["id"] / "source.txt").read_bytes(), content.encode())
        repeated = execute(self.root, "import-snapshot", payload)
        self.assertEqual(result["id"], repeated["id"])
        self.assertTrue(repeated["duplicate"])
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            execute(self.root, "import-snapshot", dict(payload, content=content + " "))
        with self.assertRaises(ValueError):
            execute(self.root, "import-snapshot", dict(payload, manifest=dict(manifest, dsn="forbidden")))

    def test_excluded_regime_is_not_filled_and_preview_records_exposure(self):
        content = "time,cv,mv\n" + "".join(f"{i},{i},{i + 1}\n" for i in range(12))
        raw = datasets(self.root, "import", {"name": "Regimes", "format": "csv", "content": content})
        config = {"time_column": "time", "time_format": "seconds", "sample_time": 1, "train_end": 5,
                  "validation_end": 9, "duplicate_policy": "first", "missing_policy": "forward_fill",
                  "max_gap_seconds": 3, "outlier_policy": "none", "signals": [
                      {"column": "cv", "name": "cv", "role": "cv", "unit": "1"},
                      {"column": "mv", "name": "mv", "role": "mv", "unit": "1"}],
                  "regimes": [{"start": 2, "end": 4, "label": "Shutdown", "exclude": True, "reason": "Known unavailable process"}]}
        preview = datasets(self.root, "clean", {"dataset_id": raw["id"], "cleaning": config})
        history = load_json(self.root / "datasets" / preview["dataset"]["id"] / "history.json")
        self.assertFalse(history["usable"])
        self.assertIsNone(history["cv"][2][0])
        self.assertIsNone(history["cv"][3][0])
        self.assertEqual(history["cv"][4][0], 4)
        self.assertFalse(preview["exposure"]["independent"])
        self.assertEqual(preview["exposure"]["events"][0]["reason"], "cleaning_preview_including_test")
        self.assertEqual((self.root / "datasets" / raw["id"] / "source.txt").read_bytes(), content.encode())

    def test_drafts_are_revision_checked_and_cannot_launch_as_studies(self):
        payload = {"name": "Unconfirmed engineering", "config": {"engineering_confirmed": False}, "research": {}}
        first = execute(self.root, "draft-save", payload)
        self.assertFalse(first["runnable"])
        second = execute(self.root, "draft-save", dict(payload, id=first["id"], expected_revision=1, name="Changed"))
        self.assertEqual(second["revision"], 2)
        with self.assertRaisesRegex(ValueError, "revision"):
            execute(self.root, "draft-save", dict(payload, id=first["id"], expected_revision=1))
        self.assertEqual(execute(self.root, "draft-list", {})["drafts"], [second])
        self.assertFalse((self.root / "studies").exists())

    def test_qualification_is_real_immutable_and_separate_from_tuning(self):
        study, _, _ = self.history_study()
        artifact = execute(self.root, "identify-study", {"study_id": study["id"]})
        self.assertFalse(artifact["qualification"]["test_accessed"])
        self.assertFalse(artifact["qualification"]["factory_authority"])
        self.assertIsNone(artifact["report"]["test"])
        self.assertEqual(len(artifact["model"]["physics"]["step_coefficients"]), 1)
        self.assertEqual(len(artifact["evaluation_plant"]["paths"][0]), 1)
        self.assertFalse((self.root / "runs").exists())
        attached = execute(self.root, "save-study", dict(study, model_id=artifact["id"]))
        self.assertNotEqual(attached["id"], study["id"])
        self.assertEqual(attached["model_id"], artifact["id"])
        changed = copy.deepcopy(artifact)
        changed["model"]["physics"]["step_coefficients"][0][0][2] += 1
        atomic_json(self.root / "models" / (artifact["id"] + ".json"), changed)
        with self.assertRaisesRegex(ValueError, "integrity"):
            load_model(self.root, artifact["id"])

    def test_new_data_policy_reserves_real_run_and_restart_deduplicates(self):
        study, model, policy, history, cleaning = self.model_policy()
        self.assertIsNone(tick(self.root)["run"])
        incomplete = copy.deepcopy(history)
        incomplete["cv"][5][0] = None
        raw = datasets(self.root, "import", {"name": "Unresolved quality", "format": "history", "content": json.dumps(incomplete)})
        datasets(self.root, "clean", {"dataset_id": raw["id"], "cleaning": cleaning})
        self.assertIsNone(tick(self.root)["run"])
        self.assertTrue(any(row["reason"] == "unresolved_data_quality" for row in list_policies(self.root)["policies"][0]["state"]["audit"]))
        self.new_clean(history, cleaning, 0.001)
        # Existing file identities satisfy CLI configuration checks. This test
        # exercises creation only and never executes these paths as programs.
        identity = str(Path(__file__).resolve())
        runtime = json.dumps({"local_only": True, "binary": identity, "bridge": identity})
        with patch.dict(os.environ, {"AUTHRITY_RESEARCH_RUNTIME": runtime}):
            decision = tick(self.root)
        run = decision["run"]
        self.assertIsNotNone(run)
        self.assertEqual(run["status"], "queued")
        self.assertEqual(run["study"]["model_id"], model["id"])
        self.assertEqual(run["study"]["research"]["objective"], study["research"]["objective"])
        frozen = load_json(self.root / "runs" / run["id"] / "frozen-model.json")
        self.assertEqual(frozen["model_sha256"], model["model_sha256"])
        self.assertFalse(any((self.root / "runs" / run["id"]).glob("*.command.json")))
        fail_run(self.root, {"id": run["id"], "error": "Test stops at durable queue boundary; native execution is separate integration coverage"})
        with database(self.root) as db:
            state = json.loads(db.execute("SELECT state FROM policies WHERE id=?", (policy["id"],)).fetchone()[0])
            state["last_started_at"] = 0
            db.execute("UPDATE policies SET state=? WHERE id=?", (json.dumps(state), policy["id"]))
        self.assertIsNone(tick(self.root)["run"])
        saved = list_policies(self.root)["policies"][0]
        self.assertEqual(saved["state"]["runs_started"], 1)
        self.assertTrue(any(row["reason"] == "duplicate_source_claim" for row in saved["state"]["audit"]))
        paused = set_state(self.root, {"id": policy["id"], "enabled": False, "expected_revision": 1})
        self.assertEqual(paused["revision"], 2)
        with self.assertRaisesRegex(ValueError, "revision"):
            set_state(self.root, {"id": policy["id"], "enabled": True, "expected_revision": 1})
        self.new_clean(history, cleaning, 0.002)
        self.assertIsNone(tick(self.root)["run"])

    def test_pareto_and_manual_selection_freeze_before_holdout(self):
        def trial(identifier, vector, eligible=True):
            return {"id": identifier, "method": "standard_dmc", "round": 1, "valid": True, "eligible": eligible,
                    "scenario_metrics": {"nominal": {"score_components": dict(zip(("variance", "bias", "movement"), vector))}}}
        trials = [trial("quality", [1, 1, 3]), trial("conservative", [2, 1, 1]), trial("dominated", [3, 2, 4]), trial("unsafe", [0, 0, 0], False)]
        frontier = pareto_trials(trials)
        self.assertEqual(set(frontier["nondominated_trial_ids"]), {"quality", "conservative"})
        run = {"id": "choice", "status": "running", "stage": "awaiting_selection", "trials": trials,
               "study": {"research": {"methods": ["standard_dmc"]}}}
        atomic_json(self.root / "runs" / "choice" / "run.json", run)
        request = {"id": "choice", "selections": {"standard_dmc": "conservative"}, "reason": "Lower actuator movement under frozen gates", "selected_by": "engineer"}
        chosen = select_trials(self.root, request)
        self.assertEqual(chosen["selections"], request["selections"])
        atomic_json(self.root / "runs" / "choice" / "run.json", dict(run, stage="completed", status="completed"))
        self.assertEqual(select_trials(self.root, request), chosen)
        with self.assertRaisesRegex(ValueError, "already frozen"):
            select_trials(self.root, dict(request, selections={"standard_dmc": "quality"}))
        self.assertFalse((self.root / "runs" / "choice" / "identification.json").exists())

    def test_drift_requires_absolute_nonoverlapping_validation_windows(self):
        from datetime import datetime, timedelta, timezone
        from research_policy import _drift_gate
        study, history, cleaning = self.history_study()

        def capture(day, offset):
            changed = copy.deepcopy(history)
            for row in changed["cv"][1000:1300]:
                row[0] += offset
            content = json.dumps(changed)
            start = datetime(2026, 1, day, tzinfo=timezone.utc)
            end = start + timedelta(seconds=3200)
            manifest = {"schema_version": 1, "source_id": "mill-readonly", "start": start.isoformat(),
                        "end": end.isoformat(), "captured_at": end.isoformat(), "format": "history",
                        "sha256": hashlib.sha256(content.encode()).hexdigest(), "read_only": True}
            raw = execute(self.root, "import-snapshot", {"name": "Captured mill history", "content": content, "manifest": manifest})
            return datasets(self.root, "clean", {"dataset_id": raw["id"], "cleaning": cleaning})["dataset"]

        original = capture(1, 0)
        study = execute(self.root, "save-study", dict(study, dataset_id=original["id"]))
        model = execute(self.root, "identify-study", {"study_id": study["id"]})
        policy = save_policy(self.root, {"name": "Persistent residual drift", "study_id": study["id"], "model_id": model["id"],
                             "enabled": True, "minimum_rows": 1000, "max_filled_fraction": 0.01, "cooldown_seconds": 30,
                             "max_runs": 3, "max_trials_total": 100,
                             "trigger": {"kind": "persistent_drift", "normalized_rmse": 0.001, "minimum_windows": 2, "minimum_seconds": 1000}})
        first, second, overlap = capture(2, 0.1), capture(3, 0.2), capture(3, 0.3)
        with database(self.root) as db:
            self.assertEqual(_drift_gate(db, self.root, policy, study, first), "drift_not_sustained_across_qualified_windows")
            self.assertIsNone(_drift_gate(db, self.root, policy, study, second))
            self.assertEqual(_drift_gate(db, self.root, policy, study, overlap), "overlapping_drift_windows")
        observed = list_policies(self.root)["policies"][0]["state"]["drift_windows"]
        self.assertEqual(len(observed), 2)
        self.assertTrue(all(row["above"] and not row["evidence"]["test_accessed"] for row in observed))
        with self.assertRaisesRegex(ValueError, "verified server-generated"):
            save_policy(self.root, {"name": "Not causal", "study_id": study["id"], "model_id": model["id"],
                                   "enabled": True, "minimum_rows": 1000, "max_filled_fraction": 0.01,
                                   "cooldown_seconds": 30, "max_runs": 3, "max_trials_total": 100, "action": "reidentify_and_tune"})

    def test_calendar_history_policy_uses_each_new_windows_actual_month_boundaries(self):
        from datetime import datetime, timedelta
        from research_design import study_splits
        original, history, _ = self.history_study()
        config = copy.deepcopy(original["config"])
        config.pop("data_window")
        config.update(start="2026-05-01T00:00:00Z", sample_time=8640)

        def capture(origin, validation_offset):
            window_config = dict(config, start=origin)
            splits = study_splits(window_config)
            first, last, count = splits[1]["first"], splits[1]["last"], splits[-1]["last"]
            start = datetime.fromisoformat(origin.replace("Z", "+00:00"))
            lines = ["timestamp,quality,feed"]
            for index in range(count):
                value = history["cv"][index][0] + (validation_offset if first <= index < last else 0)
                lines.append(f"{(start + timedelta(seconds=index * 8640)).isoformat()},{value},{history['mv'][index][0]}")
            content = "\n".join(lines) + "\n"
            end = start + timedelta(seconds=count * 8640)
            manifest = {"schema_version": 1, "source_id": "calendar-readonly", "start": origin,
                        "end": end.isoformat(), "captured_at": end.isoformat(), "format": "csv",
                        "sha256": hashlib.sha256(content.encode()).hexdigest(), "read_only": True}
            raw = execute(self.root, "import-snapshot", {"name": origin, "content": content, "manifest": manifest})
            cleaning = {"time_column": "timestamp", "time_format": "iso", "sample_time": 8640,
                        "train_end": first * 8640, "validation_end": last * 8640,
                        "duplicate_policy": "first", "missing_policy": "reject", "max_gap_seconds": 0,
                        "outlier_policy": "none",
                        "signals": [{"column": column, "name": config["variables"][role][0]["name"],
                                     "unit": config["variables"][role][0]["unit"], "role": role}
                                    for role, column in (("cv", "quality"), ("mv", "feed"))]}
            return datasets(self.root, "clean", {"dataset_id": raw["id"], "cleaning": cleaning})["dataset"]

        baseline = capture(config["start"], 0)
        study = execute(self.root, "save-study", {"name": "Calendar history", "dataset_id": baseline["id"],
                                                "config": config, "research": original["research"]})
        self.assertNotIn("data_window", study["config"])
        model = execute(self.root, "identify-study", {"study_id": study["id"]})
        policy = save_policy(self.root, {"name": "Calendar residual policy", "study_id": study["id"], "model_id": model["id"],
                             "enabled": True, "minimum_rows": 100, "max_filled_fraction": 0.01,
                             "cooldown_seconds": 30, "max_runs": 3, "max_trials_total": 100,
                             "trigger": {"kind": "persistent_drift", "normalized_rmse": 0.001,
                                         "minimum_windows": 2, "minimum_seconds": 1}})
        capture("2026-08-01T00:00:00Z", 0.1)
        latest = capture("2026-11-01T00:00:00Z", 0.2)
        # Only durable queue creation is exercised; no fixture path is executed.
        identity = str(Path(__file__).resolve())
        with patch.dict(os.environ, {"AUTHRITY_RESEARCH_RUNTIME": json.dumps({"local_only": True, "binary": identity, "bridge": identity})}):
            run = tick(self.root)["run"]
        self.assertIsNotNone(run)
        self.assertEqual(run["status"], "queued")
        self.assertEqual(run["study"]["dataset_id"], latest["id"])
        self.assertNotIn("data_window", run["study"]["config"])
        saved = next(row for row in list_policies(self.root)["policies"] if row["id"] == policy["id"])
        intervals = {tuple(row["evidence"]["relative_interval"]) for row in saved["state"]["drift_windows"]}
        # August has 31 training days; November has 30. Both three-month
        # histories have 92 days, so total length alone cannot catch this bug.
        self.assertEqual(intervals, {(310, 610), (300, 610)})
        self.assertTrue(all(not row["evidence"]["test_accessed"] for row in saved["state"]["drift_windows"]))

    def test_explicit_profile_mapping_and_hard_limits_survive_freeze_without_aliases(self):
        from campaign import controller_template
        settings = defaults()
        study = {"name": "Exact engineering", "config": settings["campaign"], "research": settings["research"]}
        cv = study["config"]["variables"]["cv"][0]
        cv["node_ids"] = {"pv": "mill:quality:PV", "target": "mill:quality:Target", "prediction": "mill:quality:Prediction",
                          "limits": {"low": "mill:quality:Low", "high": "mill:quality:High", "ll": "mill:quality:LL", "hh": "mill:quality:HH"}}
        cv["hard_low"], cv["hard_high"] = cv["low"] - 1, cv["high"] + 1
        validated = validate_study(study)
        model = controller_template(validated["config"])
        self.assertEqual(model["variables"]["cvs"][0]["node_ids"], cv["node_ids"])
        self.assertNotIn("sp", model["variables"]["cvs"][0]["node_ids"])
        self.assertEqual(model["variables"]["cvs"][0]["limits"]["low_low"], cv["hard_low"])
        self.assertEqual(model["variables"]["cvs"][0]["limits"]["high_high"], cv["hard_high"])
        cv["node_ids"].pop("prediction")
        with self.assertRaisesRegex(ValueError, "role-specific"):
            validate_study(study)

    def test_slow_lab_feedback_and_unsupported_selection_modes_fail_closed(self):
        settings = defaults()
        study = {"name": "Engineering", "config": settings["campaign"], "research": settings["research"]}
        study["config"]["variables"]["cv"][0]["measurement_kind"] = "delayed_lab"
        with self.assertRaisesRegex(ValueError, "delayed-lab"):
            validate_study(study)
        study["config"]["variables"]["cv"][0]["measurement_kind"] = "fast"
        study["research"]["selection_mode"] = "after_test_rank"
        with self.assertRaisesRegex(ValueError, "selection_mode"):
            validate_study(study)


if __name__ == "__main__":
    unittest.main()

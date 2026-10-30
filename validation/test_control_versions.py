"""Public control-version boundaries using small, real on-disk evidence artifacts."""
import copy
import hashlib
import json
import multiprocessing
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from campaign import controller_template, slice_scenario
from common import fingerprint, load_json, save_json
from control_versions import create_version, get_version, get_version_bundle, get_version_model, list_versions
from identify import frozen_prediction_metrics
from research import defaults, make_controller, next_candidates, parameter_key
from research_design import study_splits
from research_evaluation import aggregate_evaluations, research_metrics


def interrupt_publication(root, payload):
    # Terminate after both staged files are flushed but before atomic publication.
    # Unlike a raised exception, this cannot run any finally/cleanup handlers.
    with patch.object(Path, "rename", side_effect=lambda *_: os._exit(23)):
        create_version(root, payload, "reviewer")


class ControlVersionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.directory = self.root / "runs" / "run-1"
        self.directory.mkdir(parents=True)
        self.method = "standard_dmc"
        settings = defaults()
        self.config = settings["campaign"]
        self.config.update(sample_time=86400)
        self.config["plant"]["sample_time"] = 86400
        self.config["scenario"]["steady_samples"] = 1
        self.config["faults"].update(samples=3, start_sample=1)
        self.config["controller"].update(prediction_horizon=4, control_horizon=2)
        self.research = settings["research"]
        self.research.update(methods=[self.method], rounds=3, max_trials=7)
        self.study = {"id": "study-1", "name": "Deterministic receipt fixture", "dataset_id": None,
                      "config": self.config, "research": self.research}
        self.splits = study_splits(self.config)
        count = self.splits[-1]["last"]
        plant = self.config["plant"]
        self.scenario = {"sample_time": 86400, "time_seconds": [i * 86400 for i in range(count)],
                         "cv": [plant["baseline_cv"][:] for _ in range(count)],
                         "mv": [plant["baseline_mv"][:] for _ in range(count)],
                         "dv": [plant["baseline_dv"][:] for _ in range(count)],
                         "targets": [plant["baseline_cv"][:] for _ in range(count)],
                         "provenance": {"evaluation_plant": "independent_synthetic"}}
        self.selection_scenario = slice_scenario(self.scenario, self.splits[1]["first"], self.splits[1]["last"])
        self.put("study.json", self.study)
        self.put("scenario.json", self.scenario)
        self.put("selection-scenario.json", self.selection_scenario)
        for definition in self.research["scenarios"]:
            alternate = copy.deepcopy(plant)
            for row in alternate["paths"]:
                for path in row:
                    path["gain"] *= definition["gain_multiplier"]
            self.put(self.plant_name(definition["id"]), alternate)
        self.put("plant-evidence.json", {"status": "declared_independent_synthetic", "factory_validation": False})
        self.model = controller_template(self.config)
        self.model["physics"] = {"step_coefficients": [[[0.0] * 4 for _ in range(2)] for _ in range(2)],
                                 "dv_coefficients": [[[0.0] * 4 for _ in range(2)] for _ in range(2)]}
        self.put("baseline-controller.json", self.model)
        self.put("history.json", {**self.scenario, "command_mv": self.scenario["mv"]})
        self.engine_hash = "a" * 64  # Archived engine identity; no installed binary is required.
        self.receipt("history", "scenario.json", None, "plant.json")
        identification = frozen_prediction_metrics(self.model, self.scenario, self.splits[2]["first"])
        self.put("identification.json", identification)
        self.trials = []
        initial = self.research["initial"]
        baseline = self.add_trial(initial, 0, None, 1.0)
        best = baseline
        seen = {parameter_key(initial)}
        contractions = 0
        for round_index in range(1, 4):
            parent = best
            candidates = next_candidates(parent["parameters"], self.research, round_index, contractions, seen)
            for parameters in candidates:
                amplitude = 0.25 if len(self.trials) == 1 else 0.6
                trial = self.add_trial(parameters, round_index, parent["id"], amplitude)
                if trial["eligible"] and trial["score"] < best["score"]:
                    best = trial
            contractions = 0 if best is not parent else contractions + 1
        self.winner = best
        coverage = {key: sorted({t["parameters"][key] for t in self.trials}) for key in initial}
        self.selection = {"winners": {self.method: {"baseline": baseline, "best": best, "coverage": coverage,
                                                    "selection_recommendation": "candidate"}},
                          "objective": self.research["objective"],
                          "policy": "Only chronological validation selects; winners frozen before test"}
        self.put("selection.json", self.selection)
        self.put("trials.json", self.trials)
        self.row = {"method": self.method, "selected_trial": best["id"], "parameters": best["parameters"],
                    "selection_recommendation": "candidate", "recommendation": "candidate_for_review", "coverage": coverage,
                    "scenario_tests": {"baseline": {}, "optimized": {}}}
        checks = []
        for variant in ("baseline", "optimized", "transport"):
            selected = baseline if variant == "baseline" else best
            name = self.method + "-" + variant
            result = self.execution(name, self.scenario, "scenario.json", selected["controller"],
                                    "nominal", 1.0 if variant == "baseline" else 0.25, forecast=True,
                                    transport=variant == "transport")
            self.row[variant] = {"full": research_metrics(result["trace"], self.config, self.research["objective"]),
                                 "test": self.heldout_metrics(result)}
            if variant != "transport":
                self.row["scenario_tests"][variant]["nominal"] = self.row[variant]["test"]
            checks.append({"name": name + "-full-period", "status": "passed"})
        checks.append({"name": self.method + "-actual-com-equivalence", "status": "passed"})
        for definition in self.research["scenarios"]:
            scenario_id = definition["id"]
            if scenario_id == "nominal":
                continue
            for variant, selected, amplitude in (("baseline", baseline, 1.0), ("optimized", best, 0.25)):
                name = self.method + "-" + variant + "-" + scenario_id
                result = self.execution(name, self.scenario, "scenario.json", selected["controller"], scenario_id, amplitude)
                self.row["scenario_tests"][variant][scenario_id] = self.heldout_metrics(result)
                checks.append({"name": name + "-full-period", "status": "passed"})
        self.row["assessment"] = aggregate_evaluations(self.row["scenario_tests"]["optimized"],
                                                      self.row["scenario_tests"]["baseline"], self.config, self.research["objective"])
        total = 2 + 7 * 3 + 2 * 3 + 1
        self.record = {"id": "run-1", "study_id": "study-1", "study": self.study, "status": "completed",
                       "stage": "completed", "completed_at": "2026-01-01T00:00:00Z", "scope": "independent_synthetic; local simulation only",
                       "progress": {"completed": total, "total": total}, "trials": self.trials, "checks": checks,
                       "provenance": {"study": fingerprint(self.directory / "study.json"), "engine": self.engine_hash,
                                      "sources": {"research.py": "b" * 64}},
                       "results": {"methods": [self.row], "variables": self.config["variables"], "splits": self.splits,
                                   "objective": self.research["objective"], "identification": identification,
                                   "model_test": {"passed": True, "used_for_selection": False, "normalized_rmse": [0.0, 0.0]}}}
        self.put("run.json", self.record)
        self.payload = {"run_id": "run-1", "method": self.method}

    def put(self, name, value):
        save_json(self.directory / name, value)

    def plant_name(self, scenario_id):
        return "plant.json" if scenario_id == "nominal" else "plant-" + scenario_id + ".json"

    def receipt(self, name, scenario_name, controller_name, plant_name, forecast=False):
        value = {"returncode": 0, "output_sha256": fingerprint(self.directory / (name + ".json")),
                 "scenario_sha256": fingerprint(self.directory / scenario_name),
                 "plant_sha256": fingerprint(self.directory / plant_name), "engine_sha256": self.engine_hash,
                 "controller_sha256": fingerprint(self.directory / controller_name) if controller_name else None}
        if forecast:
            value.update(forecast_sha256=fingerprint(self.directory / (name + ".forecast.ndjson")),
                         forecast_index_sha256=fingerprint(self.directory / (name + ".forecast.ndjson.index.json")))
        self.put(name + ".command.json", value)

    def execution(self, name, scenario, scenario_name, controller_name, scenario_id, amplitude, forecast=False, transport=False):
        count = len(scenario["time_seconds"])
        trace = {"time_seconds": scenario["time_seconds"], "mv": scenario["mv"], "command_mv": scenario["mv"],
                 "dv": scenario["dv"], "targets": scenario["targets"],
                 "cv": [[target + (-1 if i % 2 else 1) * amplitude for target in targets]
                        for i, targets in enumerate(scenario["targets"])],
                 "predicted_cv": scenario["targets"], "solve_ms": [0.1] * count, "solver_status": ["Solved"] * count}
        result = {"status": "valid", "control_strategy": {"method": self.method}, "solver": {"calls": count, "failures": 0},
                  "trace": trace, "transport": "opc-da" if transport else "direct"}
        if transport:
            result["transport_stats"] = {"write_calls": count, "update_calls": count, "read_calls": 2 * count,
                                         "read_errors": 0, "write_errors": 0, "bad_quality_items": 0}
        self.put(name + ".json", result)
        if forecast:
            header = {"schema_version": 1, "layout": "step-major", "cv_first_offset_steps": 1,
                      "mv_first_offset_steps": 0, "sample_time": 86400, "cv_count": 2, "mv_count": 2,
                      "prediction_horizon": 4, "control_horizon": 2}
            lines = [json.dumps(header).encode() + b"\n"]
            offsets, offset = [], len(lines[0])
            for i, seconds in enumerate(trace["time_seconds"]):
                row = {"sample": i, "issued_at_seconds": seconds, "cv": trace["predicted_cv"][i] * 4,
                       "mv_base": trace["mv"][i], "mv_delta": [0.0] * 4}
                line = json.dumps(row).encode() + b"\n"
                offsets.append(offset)
                lines.append(line)
                offset += len(line)
            (self.directory / (name + ".forecast.ndjson")).write_bytes(b"".join(lines))
            self.put(name + ".forecast.ndjson.index.json", {"schema_version": 1, "header": header, "offsets": offsets, "samples": count})
        self.receipt(name, scenario_name, controller_name, self.plant_name(scenario_id), forecast)
        return result

    def add_trial(self, parameters, round_index, parent, amplitude):
        identifier = f"{self.method}-trial-{len(self.trials):03d}"
        controller = identifier + "-controller.json"
        self.put(controller, make_controller(self.model, self.method, parameters, self.research["bands"]))
        metrics, samples = {}, {}
        for definition in self.research["scenarios"]:
            scenario_id = definition["id"]
            name = identifier if scenario_id == "nominal" else identifier + "-" + scenario_id
            result = self.execution(name, self.selection_scenario, "selection-scenario.json", controller, scenario_id, amplitude)
            metrics[scenario_id] = research_metrics(result["trace"], self.config, self.research["objective"])
            samples[scenario_id] = self.splits[1]["samples"]
        baseline = self.trials[0]["scenario_metrics"] if self.trials else {}
        assessment = aggregate_evaluations(metrics, baseline, self.config, self.research["objective"], baseline=round_index == 0)
        trial = {"id": identifier, "method": self.method, "round": round_index, "parent_trial": parent,
                 "parameters": parameters, "controller": controller, "valid": True, "eligible": assessment["eligible"],
                 "score": assessment["score"], "assessment": assessment, "samples": self.splits[1]["samples"],
                 "scenario_metrics": metrics, "scenario_samples": samples, "metrics": metrics["nominal"], "failures": [], "reason": ""}
        self.trials.append(trial)
        return trial

    def heldout_metrics(self, result):
        split = self.splits[2]
        return research_metrics(result["trace"], self.config, self.research["objective"], split["first"], split["last"])

    def test_selected_controller_is_frozen_byte_exact_and_review_only(self):
        original = (self.directory / self.winner["controller"]).read_bytes()
        version = create_version(self.root, self.payload, "reviewer@example.test")
        self.assertEqual(version["authority"], "review_only")
        self.assertEqual(version["trial_id"], "standard_dmc-trial-001")
        self.assertEqual(version["selected_by"], "reviewer@example.test")
        self.assertEqual(version["model_sha256"], hashlib.sha256(original).hexdigest())
        self.assertEqual(version["model_test"]["normalized_rmse"], [0.0, 0.0])
        self.assertEqual(version["metrics"]["selected"]["samples"], 31)
        self.assertEqual(get_version(self.root, version["id"]), version)
        self.assertEqual(get_version_model(self.root, version["id"]), original)
        self.assertEqual(list_versions(self.root), [version])
        self.assertIn("standard_dmc-transport.command.json", version["evidence_hashes"])
        bundle = get_version_bundle(self.root, version["id"])
        self.assertEqual(bundle["model_json"].encode("utf-8"), original)
        stored_manifest = (self.root / "versions" / version["id"] / "version.json").read_bytes()
        self.assertEqual(bundle["manifest_json"].encode("utf-8"), stored_manifest)
        self.assertEqual(hashlib.sha256(stored_manifest).hexdigest(), bundle["manifest_sha256"])

    def test_modified_model_is_refused_by_every_read_boundary(self):
        version = create_version(self.root, self.payload, "reviewer")
        model = self.root / "versions" / version["id"] / "model.json"
        model.write_bytes(model.read_bytes() + b"\n")
        for operation in (lambda: get_version(self.root, version["id"]),
                          lambda: get_version_model(self.root, version["id"]),
                          lambda: get_version_bundle(self.root, version["id"]),
                          lambda: list_versions(self.root)):
            with self.assertRaises(ValueError):
                operation()

    def test_manifest_digest_binds_identity_metrics_and_lineage(self):
        version = create_version(self.root, self.payload, "reviewer")
        path = self.root / "versions" / version["id"] / "version.json"
        original = path.read_bytes()
        for field, value in (("selected_by", "someone-else"), ("authority", "factory"),
                             ("metrics", {}), ("source_hashes", {}), ("evidence_hashes", {})):
            with self.subTest(field=field):
                changed = json.loads(original)
                changed[field] = value
                path.write_text(json.dumps(changed, sort_keys=True, ensure_ascii=False, separators=(",", ":")))
                for operation in (lambda: get_version(self.root, version["id"]),
                                  lambda: get_version_model(self.root, version["id"]),
                                  lambda: list_versions(self.root)):
                    with self.assertRaises(ValueError):
                        operation()
                path.write_bytes(original)

    def test_rehashed_manifest_cannot_escalate_review_authority(self):
        version = create_version(self.root, self.payload, "reviewer")
        model = get_version_model(self.root, version["id"])
        content = {key: value for key, value in version.items() if key != "id"}
        content["authority"] = "factory_control"
        canonical = lambda value: json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()
        identifier = hashlib.sha256(canonical(content)).hexdigest()
        directory = self.root / "versions" / identifier
        directory.mkdir()
        (directory / "model.json").write_bytes(model)
        (directory / "version.json").write_bytes(canonical(dict(content, id=identifier)))
        with self.assertRaises(ValueError):
            get_version_model(self.root, identifier)
        with self.assertRaises(ValueError):
            list_versions(self.root)

    def test_client_cannot_supply_model_selector_identity_or_authority(self):
        for field, value in (("model", self.model), ("trial_id", self.trials[0]["id"]),
                             ("authority", "factory_control"), ("selected_by", "administrator"),
                             ("binary", "/bin/anything")):
            with self.subTest(field=field), self.assertRaises(ValueError):
                create_version(self.root, dict(self.payload, **{field: value}), "reviewer")
        self.assertFalse((self.root / "versions").exists())

    def test_path_traversal_and_unknown_methods_are_refused(self):
        for identifier in ("../run-1", "/run-1", "run-1/../run-1", "..", "a\\b", ""):
            with self.subTest(identifier=identifier):
                with self.assertRaises(ValueError):
                    create_version(self.root, dict(self.payload, run_id=identifier), "reviewer")
                with self.assertRaises(ValueError):
                    get_version(self.root, identifier)
        with self.assertRaises(ValueError):
            create_version(self.root, dict(self.payload, method="../standard_dmc"), "reviewer")

    def test_symlink_run_and_controller_artifacts_are_refused(self):
        link = self.root / "runs" / "alias"
        link.symlink_to(self.directory, target_is_directory=True)
        with self.assertRaises(ValueError):
            create_version(self.root, dict(self.payload, run_id="alias"), "reviewer")
        controller = self.directory / self.winner["controller"]
        outside = self.root / "outside-controller.json"
        outside.write_bytes(controller.read_bytes())
        controller.unlink()
        controller.symlink_to(outside)
        with self.assertRaises(ValueError):
            create_version(self.root, self.payload, "reviewer")

    def test_rejected_incomplete_and_unqualified_runs_are_refused(self):
        original = copy.deepcopy(self.record)
        changes = [
            lambda record: record.update(status="running"),
            lambda record: record["progress"].update(completed=29),
            lambda record: record["results"]["methods"][0].update(recommendation="do_not_promote"),
            lambda record: record["results"]["model_test"].update(passed=False),
            lambda record: record["results"]["model_test"].update(used_for_selection=True),
            lambda record: record["checks"].pop(),
            lambda record: record["trials"].pop(),
            lambda record: record["results"]["methods"].append(copy.deepcopy(record["results"]["methods"][0])),
        ]
        for i, change in enumerate(changes):
            with self.subTest(change=i):
                changed = copy.deepcopy(original)
                change(changed)
                self.put("run.json", changed)
                with self.assertRaises(ValueError):
                    create_version(self.root, self.payload, "reviewer")
        self.assertFalse((self.root / "versions").exists())

    def test_native_com_uncertainty_and_selection_receipts_bind_selected_bytes(self):
        for name in ("standard_dmc-optimized", "standard_dmc-transport", "standard_dmc-optimized-gain_low",
                     "standard_dmc-trial-001", "standard_dmc-trial-001-gain_high"):
            with self.subTest(execution=name):
                receipt_name = name + ".command.json"
                original = load_json(self.directory / receipt_name)
                self.put(receipt_name, dict(original, controller_sha256="f" * 64))
                with self.assertRaises(ValueError):
                    create_version(self.root, self.payload, "reviewer")
                self.put(receipt_name, original)

    def test_arbitrary_frozen_controller_and_baseline_cannot_be_selected(self):
        controller_path = self.directory / self.winner["controller"]
        controller = load_json(controller_path)
        controller["variables"]["cvs"][0]["weight"] *= 2
        self.put(self.winner["controller"], controller)
        with self.assertRaises(ValueError):
            create_version(self.root, self.payload, "reviewer")
        self.put(self.winner["controller"], make_controller(self.model, self.method, self.winner["parameters"], self.research["bands"]))
        self.selection["winners"][self.method]["best"] = self.trials[0]
        self.row["selected_trial"] = self.trials[0]["id"]
        self.put("selection.json", self.selection)
        self.put("run.json", self.record)
        with self.assertRaises(ValueError):
            create_version(self.root, self.payload, "reviewer")

    def test_objective_and_study_cannot_be_changed_after_selection(self):
        self.selection["objective"] = copy.deepcopy(self.selection["objective"])
        self.selection["objective"]["coefficients"]["variance"] = 2
        self.put("selection.json", self.selection)
        with self.assertRaises(ValueError):
            create_version(self.root, self.payload, "reviewer")
        self.selection["objective"] = self.research["objective"]
        self.put("selection.json", self.selection)
        self.study["name"] = "Changed frozen study"
        self.put("study.json", self.study)
        self.put("run.json", self.record)
        with self.assertRaises(ValueError):
            create_version(self.root, self.payload, "reviewer")

    def test_full_trajectory_and_forecast_cannot_be_truncated(self):
        name = "standard_dmc-transport"
        result = load_json(self.directory / (name + ".json"))
        result["trace"]["cv"].pop()
        self.put(name + ".json", result)
        self.receipt(name, "scenario.json", self.winner["controller"], "plant.json", forecast=True)
        with self.assertRaises(ValueError):
            create_version(self.root, self.payload, "reviewer")

    def test_forecast_must_contain_every_scan_even_with_rehashed_receipt(self):
        name = "standard_dmc-transport"
        index_name = name + ".forecast.ndjson.index.json"
        index = load_json(self.directory / index_name)
        index["offsets"].pop()
        index["samples"] -= 1
        self.put(index_name, index)
        self.receipt(name, "scenario.json", self.winner["controller"], "plant.json", forecast=True)
        with self.assertRaises(ValueError):
            create_version(self.root, self.payload, "reviewer")

    def test_ineligible_uncertainty_is_not_hidden_by_nominal_review_label(self):
        name = "standard_dmc-optimized-gain_low"
        result = self.execution(name, self.scenario, "scenario.json", self.winner["controller"], "gain_low", 2.0)
        self.row["scenario_tests"]["optimized"]["gain_low"] = self.heldout_metrics(result)
        self.put("run.json", self.record)
        with self.assertRaises(ValueError):
            create_version(self.root, self.payload, "reviewer")

    def test_com_acceptance_cannot_be_bypassed_with_loose_parity_tolerance(self):
        self.config["transport"]["absolute_tolerance"] = 10
        self.put("study.json", self.study)
        self.record["provenance"]["study"] = fingerprint(self.directory / "study.json")
        name = "standard_dmc-transport"
        result = self.execution(name, self.scenario, "scenario.json", self.winner["controller"], "nominal", 2.0,
                                forecast=True, transport=True)
        self.row["transport"] = {"full": research_metrics(result["trace"], self.config, self.research["objective"]),
                                 "test": self.heldout_metrics(result)}
        self.put("run.json", self.record)
        with self.assertRaises(ValueError):
            create_version(self.root, self.payload, "reviewer")

    def test_history_derived_run_retains_dataset_and_observational_scope(self):
        self.config.update(plant_source="identified_history", start="relative",
                           data_window={"samples": 90, "train_end": 31, "validation_end": 59})
        del self.config["plant"]
        del self.config["scenario"]
        history = load_json(self.directory / "history.json")
        history.update(usable=True,
                       signals=[{"role": role, "name": variable["name"], "unit": variable["unit"]}
                                for role in ("cv", "mv", "dv") for variable in self.config["variables"][role]],
                       provenance={"cleaning": {"train_end": 31 * 86400, "validation_end": 59 * 86400}})
        self.put("history.json", history)
        self.study.update(dataset_id="dataset-1", dataset_sha256=fingerprint(self.directory / "history.json"))
        self.scenario["provenance"]["evaluation_plant"] = "identified_history"
        self.put("scenario.json", self.scenario)
        self.put("selection-scenario.json", slice_scenario(self.scenario, 31, 59))
        for path in self.directory.glob("*.command.json"):
            receipt = load_json(path)
            scenario_name = "selection-scenario.json" if "-trial-" in path.name else "scenario.json"
            receipt["scenario_sha256"] = fingerprint(self.directory / scenario_name)
            save_json(path, receipt)
        self.put("study.json", self.study)
        self.record["provenance"]["study"] = fingerprint(self.directory / "study.json")
        self.record["progress"] = {"completed": 31, "total": 31}
        self.record["results"]["splits"] = study_splits(self.config)
        self.put("run.json", self.record)
        version = create_version(self.root, self.payload, "reviewer")
        self.assertEqual(version["dataset_id"], "dataset-1")
        self.assertTrue(version["evidence_scope"].startswith("identified_history;"))
        self.assertEqual(version["authority"], "review_only")
        self.assertEqual(version["metrics"]["selected"]["samples"], 31)

    def test_forged_passing_model_test_is_recomputed_from_history(self):
        history = load_json(self.directory / "history.json")
        for row in history["cv"][self.splits[2]["first"]:]:
            row[0] += 20
        self.put("history.json", history)
        self.receipt("history", "scenario.json", None, "plant.json")
        with self.assertRaises(ValueError):
            create_version(self.root, self.payload, "reviewer")

    def test_frozen_version_survives_removal_of_original_run_artifacts(self):
        version = create_version(self.root, self.payload, "reviewer")
        original = get_version_model(self.root, version["id"])
        (self.directory / "run.json").unlink()
        (self.directory / self.winner["controller"]).unlink()
        self.assertEqual(get_version(self.root, version["id"]), version)
        self.assertEqual(get_version_model(self.root, version["id"]), original)

    def test_interrupted_publication_preserves_visibility_and_future_selection(self):
        process = multiprocessing.get_context("spawn").Process(
            target=interrupt_publication, args=(self.root, self.payload))
        process.start()
        try:
            process.join(timeout=60)
            self.assertEqual(process.exitcode, 23)
        finally:
            if process.is_alive():
                process.kill()
                process.join()
        self.assertEqual(list_versions(self.root), [])
        version = create_version(self.root, self.payload, "reviewer")
        self.assertEqual(list_versions(self.root), [version])
        self.assertEqual(get_version_model(self.root, version["id"]),
                         (self.directory / self.winner["controller"]).read_bytes())


if __name__ == "__main__":
    unittest.main()

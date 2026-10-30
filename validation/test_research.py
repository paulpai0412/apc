"""Behavioral guards for bounded adaptive search and immutable studies."""
import copy
import json
from pathlib import Path
import tempfile
import unittest

from common import load_json
from research import ResearchRun, atomic_json, defaults, execute, next_candidates, parameter_key, validate_study


class ResearchContractTests(unittest.TestCase):
    def study(self):
        settings = defaults()
        return {"name": "Study", "dataset_id": None, "config": settings["campaign"], "research": settings["research"]}

    def queued_worker(self, root):
        directory = root / "runs" / "identity-check"
        directory.mkdir(parents=True)
        engine = root / "engine-artifact"
        engine.write_bytes(b"opaque native executable identity; never executed by these rejection tests")
        atomic_json(directory / "run.json", {"id": "identity-check", "status": "queued", "progress": {}})
        atomic_json(directory / "study.json", self.study())
        atomic_json(directory / "runtime.json", {"binary": str(engine)})
        return ResearchRun(root, "identity-check"), engine

    def test_changed_execution_inputs_cannot_reach_a_native_launch(self):
        for artifact in ("engine", "study.json", "runtime.json"):
            with self.subTest(artifact=artifact), tempfile.TemporaryDirectory() as temporary:
                worker, engine = self.queued_worker(Path(temporary))
                worker.begin_provenance()
                path = engine if artifact == "engine" else worker.directory / artifact
                path.write_bytes(path.read_bytes() + b"\n")
                with self.assertRaises(RuntimeError):
                    worker.invoke("rejected", "absent-scenario.json")
                self.assertFalse((worker.directory / "rejected.command.json").exists())

    def test_study_loaded_before_provenance_cannot_be_silently_replaced(self):
        with tempfile.TemporaryDirectory() as temporary:
            worker, _ = self.queued_worker(Path(temporary))
            path = worker.directory / "study.json"
            replacement = self.study()
            replacement["name"] = "Different frozen study"
            atomic_json(path, replacement)
            with self.assertRaises(RuntimeError):
                worker.begin_provenance()

    def test_nonfinite_frozen_json_is_rejected_before_a_worker_starts(self):
        with tempfile.TemporaryDirectory() as temporary:
            worker, _ = self.queued_worker(Path(temporary))
            invalid = self.study()
            invalid["config"]["variables"]["cv"][0]["target"] = float("nan")
            (worker.directory / "study.json").write_text(json.dumps(invalid), encoding="utf-8")
            with self.assertRaises(ValueError):
                ResearchRun(Path(temporary), "identity-check")

    def test_required_rounds_cannot_be_silently_truncated_by_budget(self):
        study = self.study()
        study["research"].update(rounds=3, max_trials=13)
        with self.assertRaises(ValueError):
            validate_study(study)

    def test_quality_band_cannot_replace_or_exceed_operating_limits(self):
        study = self.study()
        study["research"]["bands"][0]["half_width"] = 100
        with self.assertRaises(ValueError):
            validate_study(study)

    def test_quality_specs_cannot_disagree_with_scoring_or_use_invalid_bounds(self):
        study = self.study()
        cv = study["config"]["variables"]["cv"][0]
        cv.update(spec_low=cv["low"] - 10, spec_high=cv["high"] + 10)
        with self.assertRaises(ValueError):
            validate_study(study)
        study["research"]["objective"]["cv"][0]["quality_spec"] = {"low": cv["spec_low"], "high": cv["spec_high"]}
        for field, value in (("spec_low", True), ("spec_high", "50"), ("spec_low", float("nan"))):
            invalid = copy.deepcopy(study)
            invalid["config"]["variables"]["cv"][0][field] = value
            with self.assertRaises(ValueError):
                validate_study(invalid)
        cv["spec_low"] = cv["spec_high"]
        with self.assertRaises(ValueError):
            validate_study(study)

    def test_saving_changes_creates_new_snapshot_not_mutating_old_study(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first = execute(root, "save-study", self.study())
            modified = copy.deepcopy(first)
            modified["research"]["initial"]["mv_weight_scale"] = 2
            second = execute(root, "save-study", modified)
            self.assertNotEqual(first["id"], second["id"])
            self.assertEqual(second["previous_id"], first["id"])
            self.assertEqual(load_json(root / "studies" / (first["id"] + ".json"))["research"]["initial"]["mv_weight_scale"], 1)
            self.assertEqual(load_json(root / "studies" / (second["id"] + ".json"))["research"]["initial"]["mv_weight_scale"], 2)

    def test_search_moves_around_observed_incumbent_and_contracts_after_no_improvement(self):
        research = defaults()["research"]
        research["step_factor"] = 2.0
        incumbent = {"cv_weight_scale": 1.0, "mv_weight_scale": 4.0, "alpha": 0.3}
        improved = next_candidates(incumbent, research, 1, 0, {parameter_key(incumbent)})
        contracted = next_candidates(incumbent, research, 1, 1, {parameter_key(incumbent)})
        further = next_candidates(incumbent, research, 1, 3, {parameter_key(incumbent)})
        self.assertEqual([row["mv_weight_scale"] for row in improved], [2.0, 8.0])
        self.assertGreater(contracted[0]["mv_weight_scale"], 2.0)
        self.assertLess(contracted[1]["mv_weight_scale"], 8.0)
        self.assertGreater(further[0]["mv_weight_scale"], contracted[0]["mv_weight_scale"])
        self.assertLess(further[1]["mv_weight_scale"], contracted[1]["mv_weight_scale"])
        self.assertEqual(incumbent["mv_weight_scale"], 4.0)

    def test_search_does_not_charge_duplicate_clipped_candidates(self):
        research = defaults()["research"]
        incumbent = dict(research["initial"], mv_weight_scale=research["bounds"]["mv_weight_scale"][0])
        seen = {parameter_key(incumbent)}
        proposals = next_candidates(incumbent, research, 1, 0, seen)
        self.assertEqual(len(proposals), 2)
        self.assertNotEqual(parameter_key(proposals[0]), parameter_key(proposals[1]))
        for proposal in proposals:
            self.assertNotEqual(proposal, incumbent)
            for key, value in proposal.items():
                low, high = research["bounds"][key]
                self.assertLessEqual(low, value)
                self.assertLessEqual(value, high)

    def test_unknown_method_rejected_instead_of_falling_back(self):
        study = self.study()
        study["research"]["methods"] = ["mhe"]
        with self.assertRaises(ValueError):
            validate_study(study)


if __name__ == "__main__":
    unittest.main()

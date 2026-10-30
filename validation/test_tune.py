import copy
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

from common import load_json, save_json
from generate_fixture import EXAMPLES, generate
from tune import bounded_candidates, tune, tuning_parameters


class TuningBoundsTests(unittest.TestCase):
    def test_search_is_bounded_deterministic_and_changes_only_tuning(self):
        model = load_json(EXAMPLES / "synthetic_model.json")
        candidates = bounded_candidates(model, max_trials=5)
        self.assertEqual(candidates, bounded_candidates(model, max_trials=5))
        self.assertEqual(candidates[0], model)
        self.assertEqual(len(candidates), 5)
        for candidate in candidates:
            restored = copy.deepcopy(candidate)
            for source, target in zip(model["variables"]["cvs"], restored["variables"]["cvs"]):
                target["weight"], target["alpha"] = source["weight"], source["alpha"]
            for source, target in zip(model["variables"]["mvs"], restored["variables"]["mvs"]):
                target["weight_r"] = source["weight_r"]
            self.assertEqual(restored, model)

    def test_invalid_baseline_tuning_and_unbounded_trials_rejected(self):
        model = load_json(EXAMPLES / "synthetic_model.json")
        with self.assertRaises(ValueError):
            bounded_candidates(model, 129)
        model["variables"]["cvs"][0]["alpha"] = 2
        with self.assertRaises(ValueError):
            bounded_candidates(model)


@unittest.skipUnless(os.environ.get("MPC_VALIDATE_BIN"), "Set MPC_VALIDATE_BIN to exercise real Rust engine; no mock engine used")
class RealEngineTuningTests(unittest.TestCase):
    def test_import_identify_and_rust_free_run_agree_on_timing(self):
        from identify import identify
        from import_history import import_csv
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            generate(directory)
            baseline = load_json(directory / "baseline.json")
            history = import_csv(directory / "history.csv", load_json(directory / "columns.json"), baseline)
            candidate, _ = identify(baseline, history, memories=[6], ridges=[0])
            save_json(directory / "identified.json", candidate)
            save_json(directory / "history.json", history)
            process = subprocess.run([os.environ["MPC_VALIDATE_BIN"], "validate",
                                      "--plant", str(directory / "identified.json"),
                                      "--history", str(directory / "history.json"),
                                      "--output", str(directory / "validation.json")],
                                     shell=False, capture_output=True, text=True, timeout=120, check=False)
            self.assertEqual(process.returncode, 0, process.stderr)
            result = load_json(directory / "validation.json")
            self.assertLess(result["cv_rmse"][0], 1e-7)

    def test_real_engine_trials_and_heldout(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            generate(directory)
            result = tune(os.environ["MPC_VALIDATE_BIN"], directory / "plant.json", directory / "baseline.json",
                          directory / "selection.json", directory / "heldout.json", directory / "tuning", max_trials=3)
            self.assertEqual(result["status"], "candidate_only")
            self.assertEqual(len(result["trials"]), 3)
            self.assertTrue(result["heldout"]["candidate"]["valid"])
            self.assertEqual(result["heldout"]["candidate"]["metrics"]["solver_failures"], 0)
            selected = load_json(directory / "tuning" / "candidate.json")
            self.assertEqual(tuning_parameters(selected), result["trials"][result["selected_trial"]]["parameters"])

    def test_same_scenario_is_not_reused_as_test(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            generate(directory)
            with self.assertRaisesRegex(ValueError, "scenarios must differ"):
                tune(os.environ["MPC_VALIDATE_BIN"], directory / "plant.json", directory / "baseline.json",
                     directory / "selection.json", directory / "selection.json", directory / "tuning")

    def test_invalid_engine_trials_are_retained(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            generate(directory)
            # Real binary rejects an integrating plant; failures must not become zero-move successes.
            plant = load_json(directory / "plant.json")
            plant["variables"]["cvs"][0]["is_integrating"] = True
            save_json(directory / "invalid-plant.json", plant)
            with self.assertRaisesRegex(ValueError, "No valid engine trials"):
                tune(os.environ["MPC_VALIDATE_BIN"], directory / "invalid-plant.json", directory / "baseline.json",
                     directory / "selection.json", directory / "heldout.json", directory / "tuning", max_trials=1)
            report = load_json(directory / "tuning" / "report.json")
            self.assertEqual(report["status"], "no_valid_candidate")
            self.assertFalse(report["trials"][0]["valid"])
            self.assertNotEqual(report["trials"][0]["returncode"], 0)


if __name__ == "__main__":
    unittest.main()

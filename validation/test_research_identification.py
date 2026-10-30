"""Numerical family/leakage tests; no native process or factory evidence claimed."""
import copy
from pathlib import Path
import tempfile
import unittest

import numpy as np

from research_identification import identify_control
from research_plant import _fit_path, MODELLING_DEFAULTS
from research_sources import verify_generated_source
from test_research_plant import fixture, independent_response


class ControlFamilyTests(unittest.TestCase):
    def test_fopdt_has_one_fitted_pole_and_sopdt_has_two_real_paths(self):
        for family in ("fopdt", "sopdt"):
            with self.subTest(family=family):
                config, history, truth = fixture(1, 1, 1)
                config["model_family"] = family
                if family == "fopdt":
                    for row in truth["paths"]:
                        for path in row:
                            path["tau2"] = 0.0
                    history["cv"] = independent_response(truth, np.concatenate((history["mv"], history["dv"]), axis=1)).tolist()
                model, report = identify_control(config, history, 1000, 1300)
                self.assertEqual(report["family"], family)
                self.assertIsNone(report["test"])
                self.assertLess(max(report["validation"]["rmse"]), 0.03)
                self.assertEqual(len(model["physics"]["step_coefficients"][0]), 1)
                self.assertEqual(len(model["physics"]["dv_coefficients"][0]), 1)
                self.assertTrue(all(path["delay_seconds"] >= 0 for path in report["parametric_paths"][0]))
                if family == "fopdt":
                    self.assertTrue(all(path["tau2"] == 0 for path in report["parametric_paths"][0]))
                poisoned = copy.deepcopy(history)
                poisoned["cv"][1300:] = [[float("nan")]] * 300
                poisoned["mv"][1300:] = [[float("nan")]] * 300
                same_model, same_report = identify_control(config, poisoned, 1000, 1300)
                self.assertEqual(model, same_model)
                self.assertEqual(report, same_report)

    def test_fopdt_does_not_silently_use_second_pole_to_fit_sopdt(self):
        # Equal slow poles cannot be silently represented by enabling a hidden
        # second pole in the supposedly first-order optimizer.
        time = np.arange(80, dtype=float)
        response = 1 - np.exp(-time / 8) * (1 + time / 8)
        strict = dict(MODELLING_DEFAULTS, path_family="fopdt", path_max_relative_rmse=0.001, path_max_relative_error=0.002)
        with self.assertRaisesRegex(ValueError, "FOPDT"):
            _fit_path(response, strict, 1.0)

    def test_observational_upload_cannot_assert_automatic_causal_authority(self):
        with tempfile.TemporaryDirectory() as directory:
            for source in ({"kind": "file"}, {"kind": "captured_acquisition", "causal": True},
                           {"kind": "generated_synthetic", "run_id": "unknown"}):
                with self.subTest(source=source), self.assertRaises(ValueError):
                    verify_generated_source(Path(directory), source)


if __name__ == "__main__":
    unittest.main()

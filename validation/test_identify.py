import copy
import unittest

import numpy as np

from common import load_json
from generate_fixture import EXAMPLES, synthetic_history
from identify import chronological_split, frozen_prediction_metrics, identify, lag_matrix


class IdentificationTests(unittest.TestCase):
    def setUp(self):
        self.model = load_json(EXAMPLES / "synthetic_model.json")
        self.history = synthetic_history()

    def test_chronological_split_is_contiguous(self):
        self.assertEqual(chronological_split(100), (60, 80))
        with self.assertRaises(ValueError):
            chronological_split(3)

    def test_first_input_has_one_sample_delay(self):
        result = lag_matrix(np.array([[10.0], [12.0], [12.0], [12.0]]), 2)
        np.testing.assert_array_equal(result, [[0, 0], [0, 0], [2, 0], [2, 2]])

    def test_recovers_known_response_and_keeps_safety_configuration(self):
        candidate, report = identify(self.model, self.history, memories=[6, 8], ridges=[0, 0.01])
        np.testing.assert_allclose(candidate["physics"]["step_coefficients"][0][0][:6], [0, .5, .8, 1, 1, 1], atol=1e-7)
        np.testing.assert_allclose(candidate["physics"]["dv_coefficients"][0][0][:6], [0, -.2, -.3, -.4, -.4, -.4], atol=1e-7)
        self.assertEqual(candidate["variables"], self.model["variables"])
        self.assertEqual(candidate["tuning"], self.model["tuning"])
        self.assertLess(report["test"]["rmse"][0], 1e-7)
        self.assertLess(report["test"]["rmse"][0], report["test_one_step_persistence"]["rmse"][0])
        self.assertEqual(report["status"], "candidate_only")
        curve = candidate["physics"]["step_coefficients"][0][0]
        self.assertEqual(curve[-1], curve[-3])

    def test_untouched_test_outputs_cannot_change_selected_model(self):
        first, report = identify(self.model, self.history, memories=[6], ridges=[0, .01])
        changed = copy.deepcopy(self.history)
        for row in changed["cv"][report["splits"]["test"][0]:]:
            row[0] += 1000
        second, other = identify(self.model, changed, memories=[6], ridges=[0, .01])
        self.assertEqual(first, second)
        self.assertEqual(report["selection"], other["selection"])
        self.assertGreater(other["test"]["rmse"][0], 999)

    def test_explicit_calendar_indices_override_fraction_split(self):
        first, report = identify(self.model, self.history, memories=[6], ridges=[0, .01],
                                 split_indices=(250, 400))
        self.assertEqual(report["splits"], {"train": [0, 250], "validation": [250, 400],
                                           "test": [400, len(self.history["cv"])]})
        self.assertEqual(report["validation"]["samples"], 150)
        self.assertEqual(report["full"]["samples"], len(self.history["cv"]) - 6)
        changed = copy.deepcopy(self.history)
        for row in changed["cv"][400:]:
            row[0] += 1000
        second, other = identify(self.model, changed, memories=[6], ridges=[0, .01],
                                 split_indices=(250, 400))
        self.assertEqual(first, second)
        self.assertEqual(report["selection"], other["selection"])
        self.assertGreater(other["test"]["rmse"][0], 999)

    def test_deferred_selection_never_reads_test_and_frozen_prediction_preserves_timing(self):
        poisoned = copy.deepcopy(self.history)
        poisoned["cv"][400:] = [[float("nan")]] * (len(poisoned["cv"]) - 400)
        candidate, report = identify(self.model, poisoned, memories=[6], ridges=[0],
                                     split_indices=(250, 400), evaluate_test=False)
        self.assertIsNone(report["test"])
        self.assertIsNone(report["full"])
        heldout = frozen_prediction_metrics(candidate, self.history, 400)
        self.assertLess(heldout["test"]["rmse"][0], 1e-7)
        changed = copy.deepcopy(self.history)
        changed["cv"][450][0] += 10
        other = frozen_prediction_metrics(candidate, changed, 400)
        self.assertAlmostEqual(other["test"]["rmse"][0], 10 / np.sqrt(len(changed["cv"]) - 400), places=6)

    def test_invalid_explicit_indices_are_rejected(self):
        for boundaries in ((0, 10), (20, 10), (10, len(self.history["cv"])), (2.5, 10)):
            with self.subTest(boundaries=boundaries), self.assertRaises(ValueError):
                identify(self.model, self.history, memories=[6], split_indices=boundaries)

    def test_constant_and_collinear_inputs_rejected(self):
        for mode in ("constant", "collinear"):
            history = copy.deepcopy(self.history)
            history["dv"] = [[0.0] for _ in history["dv"]] if mode == "constant" else copy.deepcopy(history["mv"])
            with self.subTest(mode=mode), self.assertRaises(ValueError):
                identify(self.model, history, memories=[6], ridges=[0])

    def test_integrating_flag_and_unsettled_tail_rejected(self):
        self.model["variables"]["cvs"][0]["is_integrating"] = True
        with self.assertRaises(ValueError):
            identify(self.model, self.history, memories=[6], ridges=[0])
        self.model["variables"]["cvs"][0]["is_integrating"] = False
        # True impulse remains nonzero at the declared end of memory: truncation is rejected.
        for t, row in enumerate(self.history["cv"]):
            row[0] = sum(self.history["mv"][t - lag][0] + .3 * self.history["dv"][t - lag][0]
                         for lag in range(1, 7) if t >= lag)
        with self.assertRaisesRegex(ValueError, "Unsettled"):
            identify(self.model, self.history, memories=[6], ridges=[0])


if __name__ == "__main__":
    unittest.main()

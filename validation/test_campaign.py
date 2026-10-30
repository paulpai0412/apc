import copy
from datetime import datetime, timezone
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

import numpy as np

from campaign import (Campaign, add_months, calendar_splits, check_config, controller_template,
                      generate_scenario, slice_scenario, trace_metrics, tuned_model, validate_result)
from common import load_json, save_json


EXAMPLE = Path(__file__).with_name("examples") / "cement_campaign.json"


class CampaignTests(unittest.TestCase):
    def setUp(self):
        self.config = load_json(EXAMPLE)

    def hourly_config(self):
        config = copy.deepcopy(self.config)
        config["sample_time"] = config["plant"]["sample_time"] = 3600.0
        config["scenario"]["steady_samples"] = 2
        return check_config(config)

    def test_default_is_three_actual_calendar_months(self):
        splits = calendar_splits(check_config(self.config))
        self.assertEqual([row["samples"] for row in splits], [44640, 40320, 44640])
        self.assertEqual(splits[-1]["end"], "2026-04-01T00:00:00Z")
        self.assertEqual(splits[-1]["last"], 129600)
        self.config["start"] = "2024-01-01T00:00:00Z"
        self.assertEqual(calendar_splits(self.config)[1]["samples"], 29 * 24 * 60)

    def test_calendar_addition_clamps_month_end_not_fixed_days(self):
        start = datetime(2024, 1, 31, tzinfo=timezone.utc)
        self.assertEqual(add_months(start, 1).day, 29)
        self.assertEqual(add_months(start, 2).day, 31)

    def test_rejects_incomplete_period_and_unaligned_scans(self):
        for key, value in (("months", 2), ("id", "../escape"), ("sample_time", 0)):
            changed = copy.deepcopy(self.config)
            changed[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                check_config(changed)
        self.config["sample_time"] = self.config["plant"]["sample_time"] = 61
        with self.assertRaisesRegex(ValueError, "align"):
            check_config(self.config)

    def test_generator_is_seeded_independent_and_bounded(self):
        config = self.hourly_config()
        first = generate_scenario(config)
        self.assertEqual(first, generate_scenario(config))
        changed = copy.deepcopy(config)
        changed["scenario"]["sensor_noise_std"] = [value * 2 for value in changed["scenario"]["sensor_noise_std"]]
        second = generate_scenario(changed)
        for field in ("mv", "dv", "process_noise", "targets", "gain_scale"):
            self.assertEqual(first[field], second[field])
        np.testing.assert_allclose(np.asarray(first["measurement_noise"]) * 2, second["measurement_noise"])
        for group in ("mv", "dv"):
            values = np.asarray(first[group])
            for i, variable in enumerate(config["variables"][group]):
                self.assertTrue(np.all(values[:, i] >= variable["low"]))
                self.assertTrue(np.all(values[:, i] <= variable["high"]))
        self.assertEqual(first["mv"][0], config["plant"]["baseline_mv"])
        self.assertEqual(first["measurement_noise"][0], [0, 0])
        self.assertEqual(first["process_noise"][0], [0, 0])

    def test_selection_contains_every_second_month_row(self):
        config = self.hourly_config()
        scenario = generate_scenario(config)
        split = calendar_splits(config)[1]
        selected = slice_scenario(scenario, split["first"], split["last"])
        self.assertEqual(len(selected["time_seconds"]), split["samples"])
        self.assertEqual(selected["time_seconds"][0], 0)
        self.assertEqual(selected["dv"], scenario["dv"][split["first"]:split["last"]])
        self.assertEqual(selected["process_noise"], scenario["process_noise"][split["first"]:split["last"]])

    def test_controller_template_does_not_consume_truth_paths(self):
        baseline = controller_template(self.config)
        changed = copy.deepcopy(self.config)
        changed["plant"]["paths"][0][0]["gain"] *= 999
        changed["plant"]["paths"][1][1]["delay_seconds"] *= 100
        self.assertEqual(baseline, controller_template(changed))
        self.assertEqual(baseline["physics"], {})
        candidate = tuned_model(baseline, self.config["candidates"][0])
        self.assertEqual(candidate["variables"]["mvs"][0]["limits"], baseline["variables"]["mvs"][0]["limits"])
        self.assertEqual(candidate["variables"]["mvs"][0]["max_move"], baseline["variables"]["mvs"][0]["max_move"])
        self.assertNotEqual(candidate["variables"]["mvs"][0]["weight_r"], baseline["variables"]["mvs"][0]["weight_r"])

    def test_metrics_use_physical_time_and_are_weight_independent(self):
        trace = {"cv": [[361, 42], [359, 38]], "targets": [[360, 40], [360, 40]],
                 "mv": [[100, 900], [101, 904]], "command_mv": [[100, 900], [101, 904]],
                 "dv": [[14, 1.2], [14, 1.2]], "time_seconds": [0, 60]}
        metrics = trace_metrics(trace, self.config)
        self.assertEqual(metrics["cv_rmse"], [1, 2])
        self.assertEqual(metrics["cv_iae"], [120, 240])
        self.assertEqual(metrics["mv_total_variation"], [1, 4])
        self.assertEqual(metrics["mv_hard_violations"], [0, 0])
        changed = copy.deepcopy(self.config)
        changed["variables"]["cv"][0]["weight"] *= 1000
        self.assertEqual(metrics, trace_metrics(trace, changed))
        trace["command_mv"][1][0] = 120
        self.assertEqual(trace_metrics(trace, self.config)["mv_hard_violations"][0], 1)

    def test_missing_solver_calls_cannot_pass_numerical_validation(self):
        scenario = slice_scenario(generate_scenario(self.hourly_config()), 0, 2)
        trace = {key: scenario[key] for key in ("cv", "mv", "dv", "targets", "time_seconds")}
        trace["command_mv"] = trace["mv"]
        result = {"status": "valid", "trace": trace, "solver": {"calls": 1, "failures": 0}}
        valid, reason = validate_result(result, scenario, self.config)
        self.assertFalse(valid)
        self.assertIn("solver", reason)

    def test_resume_checks_hashes_and_does_not_run_completed_action(self):
        with tempfile.TemporaryDirectory() as directory:
            config_path = Path(directory) / "config.json"
            save_json(config_path, self.config)
            args = SimpleNamespace(config=str(config_path), output_root=str(Path(directory) / "runs"),
                                   resume=False, binary="missing-runtime", bridge=None, bridge_arg=[])
            campaign = Campaign(args)
            output = campaign.path("deterministic.json")
            campaign.stage("unit-stage", [], [output], lambda: save_json(output, {"value": 2}))
            args.resume = True
            resumed = Campaign(args)
            resumed.stage("unit-stage", [], [output], lambda: self.fail("Completed stage must not run again"))
            save_json(output, {"value": 3})
            with self.assertRaisesRegex(ValueError, "hash mismatch"):
                resumed.stage("unit-stage", [], [output], lambda: self.fail("Corrupt stage must not run silently"))


if __name__ == "__main__":
    unittest.main()

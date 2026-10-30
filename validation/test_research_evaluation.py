"""Public evaluator contracts; fixtures are arithmetic examples, not plant evidence."""
import copy
import unittest

import numpy as np

from research_evaluation import (aggregate_evaluations, assess_candidate,
                                 objective_defaults, research_metrics, validate_objective)


def configuration():
    return {"sample_time": 2.0, "variables": {
        "cv": [{"name": "quality", "unit": "%", "low": -10.0, "high": 10.0,
                "target": 0.0, "weight": 1.0}],
        "mv": [{"name": "feed", "unit": "t/h", "low": -10.0, "high": 10.0,
                "max_move": 10.0, "weight_r": 1.0}], "dv": []}}


def trace(cv, command=None, targets=None):
    cv = np.asarray(cv, dtype=float)
    if cv.ndim == 1:
        cv = cv[:, None]
    count = len(cv)
    command = np.zeros((count, 1)) if command is None else np.asarray(command, dtype=float).reshape(count, -1)
    targets = np.zeros_like(cv) if targets is None else np.asarray(targets, dtype=float).reshape(cv.shape)
    return {"cv": cv.tolist(), "targets": targets.tolist(), "mv": command.tolist(),
            "command_mv": command.tolist(), "time_seconds": (np.arange(count) * 2).tolist(),
            "solve_ms": [1.0] * count, "solver_status": ["Solved"] * count}


class ResearchEvaluationTests(unittest.TestCase):
    def setUp(self):
        self.config = configuration()
        self.policy = objective_defaults(self.config)

    def metrics(self, values, command=None, targets=None):
        return research_metrics(trace(values, command, targets), self.config, self.policy)

    def test_flat_but_biased_cv_cannot_win(self):
        reference = self.metrics([-1, 1, -1, 1])
        candidate = self.metrics([0.5] * 4)
        result = assess_candidate(candidate, reference, self.config, self.policy)
        self.assertFalse(result["eligible"])
        self.assertTrue(any("bias" in reason for reason in result["reasons"]))
        self.assertEqual(candidate["cv_variance"], [0.0])
        self.assertEqual(candidate["cv_tracking_bias"], [0.5])

    def test_variance_improvement_with_excessive_movement_loses(self):
        reference = self.metrics([-1, 1, -1, 1], [0, 1, 0, 1])
        candidate = self.metrics([-0.1, 0.1, -0.1, 0.1], [0, 2, 0, 2])
        result = assess_candidate(candidate, reference, self.config, self.policy)
        self.assertFalse(result["eligible"])
        self.assertTrue(any("movement increase" in reason for reason in result["reasons"]))
        self.assertEqual(candidate["mv_total_variation"], [6.0])
        self.assertEqual(candidate["mv_total_variation_rate"], [1.0])

    def test_physical_units_and_independent_scales(self):
        self.config["variables"]["cv"].append({
            "name": "temperature", "unit": "C", "low": -1000.0, "high": 1000.0,
            "target": 0.0, "weight": 999.0})
        self.policy = objective_defaults(self.config)
        self.policy["cv"][0]["scale"] = 2
        self.policy["cv"][1]["scale"] = 100
        self.policy["mv"][0]["scale"] = 4
        measured = self.metrics([[-1, -100], [1, 100]], [0, 2])
        self.assertEqual(measured["cv_mean"], [0.0, 0.0])
        self.assertEqual(measured["cv_std"], [1.0, 100.0])
        self.assertEqual(measured["cv_variance"], [1.0, 10000.0])
        self.assertEqual(measured["score_components"]["variance"], 0.625)
        self.assertEqual(measured["mv_normalized_movement"], [0.25])
        self.assertEqual(measured["mv_normalized_total_variation"], [0.5])
        self.assertAlmostEqual(measured["score"], 0.65)
        self.config["variables"]["cv"][0]["weight"] = 1000000
        self.config["variables"]["mv"][0]["weight_r"] = 1000000
        self.assertEqual(self.metrics([[-1, -100], [1, 100]], [0, 2])["score"], measured["score"])

    def test_quality_specs_are_distinct_from_operating_limits(self):
        self.policy["cv"][0]["quality_spec"] = {"low": -0.5, "high": 0.5}
        measured = self.metrics([-1, 0, 1, 0])
        self.assertEqual(measured["cv_violation_seconds"], [0.0])
        self.assertEqual(measured["cv_operating_violation_fraction"], [0.0])
        self.assertEqual(measured["cv_spec_violation_seconds"], [4.0])
        self.assertEqual(measured["cv_spec_violation_fraction"], [0.5])
        self.assertEqual(measured["cv_spec_excursion_mean_square"], [0.125])
        result = assess_candidate(measured, measured, self.config, self.policy, baseline=True)
        self.assertFalse(result["eligible"])
        self.assertTrue(any("quality specification" in reason for reason in result["reasons"]))
        self.policy["cv"][0]["quality_spec"] = {"low": -20, "high": 20}
        measured = self.metrics([-11, 11])
        self.assertEqual(measured["cv_operating_violation_fraction"], [1.0])
        self.assertEqual(measured["cv_spec_violation_fraction"], [0.0])
        result = assess_candidate(measured, measured, self.config, self.policy, baseline=True)
        self.assertTrue(any("operating violation" in reason for reason in result["reasons"]))

    def test_missing_specs_remain_null_not_assessed(self):
        measured = self.metrics([-1, 1])
        self.assertIsNone(self.policy["cv"][0]["quality_spec"])
        self.assertEqual(measured["quality_assessed"], [False])
        for field in ("cv_spec_violation_seconds", "cv_spec_violation_fraction", "cv_spec_excursion_mean_square"):
            self.assertEqual(measured[field], [None])
        self.assertIsNone(measured["score_components"]["quality"])
        self.config["variables"]["cv"][0]["spec_high"] = 0.5
        self.policy = objective_defaults(self.config)
        self.assertEqual(self.policy["cv"][0]["quality_spec"], {"low": None, "high": 0.5})
        self.assertEqual(self.metrics([-1, 1])["cv_spec_violation_fraction"], [0.5])

    def test_zero_baselines_use_absolute_gates_and_null_ratios(self):
        zero = self.metrics([0, 0, 0, 0])
        equal = assess_candidate(zero, zero, self.config, self.policy)
        self.assertTrue(equal["eligible"])
        for change in (equal["improvements"]["cv"][0]["tracking_error_variance_reduction"],
                       equal["improvements"]["mv"][0]["total_variation_increase"]):
            self.assertEqual(change["absolute"], 0.0)
            self.assertIsNone(change["fraction"])
            self.assertTrue(change["baseline_zero"])
        candidate = self.metrics([-0.1, 0.1, -0.1, 0.1], [0, 0.01, 0, 0.01])
        result = assess_candidate(candidate, zero, self.config, self.policy)
        self.assertFalse(result["eligible"])
        self.assertTrue(any("variance no-worsening" in reason for reason in result["reasons"]))
        self.assertTrue(any("movement increase" in reason for reason in result["reasons"]))

    def test_changing_targets_do_not_reward_frozen_output(self):
        targets = [-2, 2, -2, 2]
        reference = self.metrics(targets, targets=targets)
        frozen = self.metrics([0, 0, 0, 0], targets=targets)
        self.assertEqual(reference["cv_variance"], [4.0])
        self.assertEqual(reference["cv_tracking_error_variance"], [0.0])
        self.assertEqual(frozen["cv_variance"], [0.0])
        self.assertEqual(frozen["cv_tracking_error_variance"], [4.0])
        self.assertGreater(frozen["score"], reference["score"])
        result = assess_candidate(frozen, reference, self.config, self.policy)
        self.assertFalse(result["eligible"])
        changes = result["improvements"]["cv"][0]
        self.assertEqual(changes["physical_variance_reduction"]["absolute"], 4.0)
        self.assertEqual(changes["tracking_error_variance_reduction"]["absolute"], -4.0)
        self.assertIsNone(changes["tracking_error_variance_reduction"]["fraction"])

    def test_minimum_improvement_and_each_cv_are_gated(self):
        reference = self.metrics([-1, 1, -1, 1])
        result = assess_candidate(reference, reference, self.config, self.policy)
        self.assertFalse(result["eligible"])
        self.assertTrue(any("minimum" in reason for reason in result["reasons"]))
        self.assertTrue(assess_candidate(reference, reference, self.config, self.policy, baseline=True)["eligible"])
        self.assertTrue(assess_candidate(self.metrics([-0.9, 0.9, -0.9, 0.9]), reference, self.config, self.policy)["eligible"])
        self.config["variables"]["cv"].append(dict(self.config["variables"]["cv"][0], name="second"))
        self.policy = objective_defaults(self.config)
        reference = self.metrics([[-2, -1], [2, 1]])
        candidate = self.metrics([[-0.1, -1.1], [0.1, 1.1]])
        result = assess_candidate(candidate, reference, self.config, self.policy)
        self.assertFalse(result["eligible"])
        self.assertTrue(any("CV second:" in reason for reason in result["reasons"]))

    def test_multiscenario_rejection_retains_all_failures(self):
        self.policy["scenarios"] = ["nominal", "gain_low", "gain_high"]
        reference = self.metrics([-1, 1, -1, 1])
        good = self.metrics([-0.1, 0.1, -0.1, 0.1])
        bad_trace = trace([1, 1, 1, 1], [0, 11, 0, 11])
        bad_trace["solver_status"][0] = "Failed"
        bad = research_metrics(bad_trace, self.config, self.policy)
        result = aggregate_evaluations({"nominal": good, "gain_low": bad},
                                      {name: reference for name in self.policy["scenarios"]},
                                      self.config, self.policy)
        self.assertFalse(result["eligible"])
        self.assertIsNone(result["score"])
        for fragment in ("gain_low:", "solver", "bias", "hard actuator", "movement increase", "gain_high: Missing"):
            self.assertTrue(any(fragment in reason for reason in result["reasons"]), fragment)
        complete = aggregate_evaluations({name: good for name in self.policy["scenarios"]},
                                         {name: reference for name in self.policy["scenarios"]},
                                         self.config, self.policy)
        self.assertTrue(complete["eligible"])
        self.assertAlmostEqual(complete["score"], good["score"])

    def test_complete_rejected_scenarios_keep_a_numerical_score(self):
        reference = self.metrics([-1, 1, -1, 1])
        biased = self.metrics([0.5, 0.5, 0.5, 0.5])
        result = aggregate_evaluations({"nominal": biased}, {"nominal": reference},
                                      self.config, self.policy)
        self.assertFalse(result["eligible"])
        self.assertEqual(result["score"], biased["score"])

    def test_missing_and_undeclared_baselines_cannot_be_ignored(self):
        reference = self.metrics([-1, 1, -1, 1])
        improved = self.metrics([-0.1, 0.1, -0.1, 0.1])
        result = aggregate_evaluations({"nominal": improved, "unexpected": improved},
                                      {"unexpected": reference}, self.config, self.policy)
        self.assertFalse(result["eligible"])
        for fragment in ("Undeclared candidate", "Undeclared baseline", "Missing matching baseline"):
            self.assertTrue(any(fragment in reason for reason in result["reasons"]))

    def test_matching_baseline_targets_and_telemetry_are_required(self):
        reference = self.metrics([-1, 1, -1, 1])
        candidate = self.metrics([-0.1, 0.1, -0.1, 0.1], targets=[1, 1, 1, 1])
        result = assess_candidate(candidate, reference, self.config, self.policy)
        self.assertTrue(any("targets_sha256" in reason for reason in result["reasons"]))
        data = trace([-0.1, 0.1, -0.1, 0.1])
        del data["solver_status"]
        candidate = research_metrics(data, self.config, self.policy)
        result = assess_candidate(candidate, reference, self.config, self.policy)
        self.assertTrue(any("telemetry" in reason for reason in result["reasons"]))

    def test_interval_metrics_use_full_resolution_and_exclusive_end(self):
        data = trace([9, -1, 1, -1, 9], [9, 0, 2, 0, 9])
        measured = research_metrics(data, self.config, self.policy, 1, 4)
        self.assertEqual(measured["samples"], 3)
        self.assertEqual(measured["duration_seconds"], 6.0)
        self.assertEqual(measured["movement_duration_seconds"], 4.0)
        self.assertEqual(measured["mv_total_variation"], [4.0])
        self.assertEqual(measured["window"], {"first": 1, "last": 4})
        data["time_seconds"][2] = 5
        with self.assertRaisesRegex(ValueError, "full-resolution"):
            research_metrics(data, self.config, self.policy)

    def test_policy_validation_and_copy_do_not_mutate_frozen_inputs(self):
        normalized = validate_objective(self.policy, self.config)
        normalized["cv"][0]["scale"] = 1
        self.assertEqual(self.policy["cv"][0]["scale"], 20)
        for field, value in (("scale", 0), ("max_abs_bias", float("nan")),
                             ("max_spec_violation_fraction", 1.1),
                             ("min_variance_improvement_fraction", True)):
            invalid = copy.deepcopy(self.policy)
            invalid["cv"][0][field] = value
            with self.assertRaises(ValueError):
                validate_objective(invalid, self.config)
        invalid = copy.deepcopy(self.policy)
        invalid["cv"][0]["quality_spec"] = {"low": 2, "high": 1}
        with self.assertRaisesRegex(ValueError, "ordered"):
            validate_objective(invalid, self.config)
        invalid = copy.deepcopy(self.policy)
        invalid["scenarios"] = ["nominal", "nominal"]
        with self.assertRaisesRegex(ValueError, "unique"):
            validate_objective(invalid, self.config)


if __name__ == "__main__":
    unittest.main()

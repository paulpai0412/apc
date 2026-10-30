"""Numerical and leakage regression tests for observational hypothesis fitting."""
import copy
import unittest

import numpy as np

from research_plant import (MODELLING_DEFAULTS, _fit_path, _free_run,
                            fit_history_plant, modelling_settings, scenario_from_history)


def fixture(nc=2, nm=2, nd=1):
    config = {"id": "history_test", "description": "Test engineering mapping", "sample_time": 2.0,
              "controller": {"prediction_horizon": 40},
              "identification": {"memories": [24], "ridges": [0.0], "tail_fraction": 0.05},
              "variables": {"cv": [], "mv": [], "dv": []}}
    for group, count in (("cv", nc), ("mv", nm), ("dv", nd)):
        for j in range(count):
            variable = {"name": f"{group}_{j}", "unit": f"mapped_{group}_unit", "low": -10.0, "high": 10.0}
            if group == "cv":
                variable.update(target=0.2 * j, weight=1.0, alpha=0.3, slack_weight=100.0)
            if group == "mv":
                variable.update(max_move=10.0, weight_r=1.0)
            config["variables"][group].append(variable)
    rng = np.random.default_rng(2026)
    commands = rng.uniform(-1, 1, size=(1600, nm + nd))
    commands[0] = 0
    paths = [[{"gain": (-1.0 if (i + j) % 2 else 1.0) * (0.5 + 0.2 * j),
               "tau1": 0.8, "tau2": 1.6, "delay_seconds": 0.7}
              for j in range(nm + nd)] for i in range(nc)]
    if nc > 1 and nm + nd > 1:
        paths[1][0]["gain"] = 0.0
    plant = {"sample_time": 2.0, "baseline_cv": [0.0] * nc, "baseline_mv": [0.0] * nm,
             "baseline_dv": [0.0] * nd, "actuator_tau": [0.0] * nm,
             "actuator_low": [-10.0] * nm, "actuator_high": [10.0] * nm,
             "actuator_rate": [5.0] * nm, "paths": paths,
             "nonlinear_strength": [0.0] * nc, "nonlinear_scale": [20.0] * nc}
    cv = independent_response(plant, commands)
    history = {"sample_time": 2.0, "time_seconds": (np.arange(len(commands)) * 2.0).tolist(),
               "cv": cv.tolist(), "mv": commands[:, :nm].tolist(), "dv": commands[:, nm:].tolist()}
    return config, history, plant


def independent_response(plant, commands):
    """Scalar reference recurrence, separate from fitter and its vectorized replay."""
    count = len(commands)
    outputs = np.tile(plant["baseline_cv"], (count, 1)).astype(float)
    baseline = plant["baseline_mv"] + plant["baseline_dv"]
    for i, row in enumerate(plant["paths"]):
        for j, path in enumerate(row):
            a = 0.0 if path["tau1"] == 0 else np.exp(-plant["sample_time"] / path["tau1"])
            b = 0.0 if path["tau2"] == 0 else np.exp(-plant["sample_time"] / path["tau2"])
            lag = path["delay_seconds"] / plant["sample_time"]
            whole, fraction = int(lag), lag % 1
            x1 = x2 = 0.0
            for t in range(count - 1):
                u = commands[t - whole, j] - baseline[j] if t >= whole else 0.0
                older = commands[t - whole - 1, j] - baseline[j] if t > whole else 0.0
                x1 = a * x1 + (1 - a) * ((1 - fraction) * u + fraction * older)
                x2 = b * x2 + (1 - b) * x1
                outputs[t + 1, i] += path["gain"] * x2
    return outputs


class HistoryPlantTests(unittest.TestCase):
    def test_multivariable_fit_recovers_dynamics_and_zero_path(self):
        config, history, truth = fixture()
        plant, evidence = fit_history_plant(config, history, 1000, 1300)
        self.assertFalse(evidence["independent_physical_truth"])
        self.assertEqual(evidence["fit_interval"], [0, 1000])
        self.assertLess(max(evidence["validation"]["normalized_rmse"]), 0.001)
        self.assertTrue(evidence["path_approximations"][1]["paths"][0]["zero_gain"])
        self.assertEqual(plant["paths"][1][0]["gain"], 0.0)
        commands = np.zeros((32, 3))
        commands[:, 0] = 1.0
        expected = independent_response(truth, commands)
        actual = _free_run(plant, commands[:, :2], commands[:, 2:])
        np.testing.assert_allclose(actual, expected, atol=0.01)
        self.assertGreater(abs(actual[1, 0]), 0.01)
        self.assertLess(abs(actual[1, 0]), abs(actual[10, 0]))

    def test_final_test_values_and_truth_configuration_never_enter_fit(self):
        config, history, _ = fixture(1, 1, 0)
        first_plant, first_evidence = fit_history_plant(config, history, 1000, 1300)
        poisoned = copy.deepcopy(history)
        poisoned["cv"][1300:] = [[float("nan")]] * 300
        poisoned["mv"][1300:] = [[float("nan")]] * 300
        poisoned["dv"][1300:] = [["not a mapped DV"]] * 300
        config["plant"] = {"paths": "MUST NEVER BE READ", "sample_time": -99}
        second_plant, second_evidence = fit_history_plant(config, poisoned, 1000, 1300)
        self.assertEqual(first_plant, second_plant)
        self.assertEqual(first_evidence, second_evidence)
        self.assertEqual(second_plant["baseline_dv"], [])
        self.assertEqual(len(second_plant["paths"][0]), 1)

    def test_validation_changes_qualification_not_fitted_parameters(self):
        config, history, _ = fixture(1, 1, 0)
        first, _ = fit_history_plant(config, history, 1000, 1300)
        changed = copy.deepcopy(history)
        changed["cv"][1000:1300] = [[row[0] + 0.1] for row in history["cv"][1000:1300]]
        second, evidence = fit_history_plant(config, changed, 1000, 1300)
        self.assertEqual(first, second)
        self.assertGreater(evidence["validation"]["rmse"][0], 0.09)
        changed["cv"][1000:1300] = [[row[0] + 5] for row in history["cv"][1000:1300]]
        with self.assertRaisesRegex(ValueError, "free-run qualification"):
            fit_history_plant(config, changed, 1000, 1300)

    def test_scenario_uses_no_recorded_cv_or_target_feedback(self):
        config, history, plant = fixture(2, 2, 2)
        del history["cv"]
        history["targets"] = [[float("nan")]]
        scenario = scenario_from_history(config, history, plant)
        self.assertEqual(scenario["cv"][1200], plant["baseline_cv"])
        self.assertEqual(scenario["targets"][1200], [0.0, 0.2])
        self.assertEqual(scenario["mv"], history["mv"])
        self.assertEqual(scenario["dv"], history["dv"])
        self.assertFalse(scenario["provenance"]["independent_physical_truth"])
        history["mv"][100][0] = 99
        self.assertNotEqual(scenario["mv"][100][0], 99)

    def test_fractional_delay_and_observation_before_command(self):
        _, _, plant = fixture(1, 1, 0)
        plant["paths"] = [[{"gain": 2.0, "tau1": 0.0, "tau2": 0.0, "delay_seconds": 3.0}]]
        actual = _free_run(plant, np.ones((6, 1)), np.empty((6, 0)))
        np.testing.assert_allclose(actual[:, 0], [0, 0, 1, 2, 2, 2], atol=1e-12)
        path, record = _fit_path(np.r_[0, 0, 1, np.full(21, 2.0)], MODELLING_DEFAULTS, 2.0)
        self.assertAlmostEqual(path["delay_seconds"], 3.0, places=4)
        self.assertAlmostEqual(path["gain"], 2.0, places=5)
        self.assertLess(record["relative_rmse"], 1e-6)

    def test_engineering_rate_limit_is_applied_without_extra_actuator_lag(self):
        _, _, plant = fixture(1, 1, 0)
        plant["actuator_rate"] = [0.25]
        plant["paths"] = [[{"gain": 1.0, "tau1": 0.0, "tau2": 0.0, "delay_seconds": 0.0}]]
        prediction = _free_run(plant, [[2.0]] * 6, np.empty((6, 0)))
        np.testing.assert_allclose(prediction[:, 0], [0, 0.5, 1, 1.5, 2, 2])

    def test_future_move_budget_does_not_rewrite_historical_commands(self):
        config, history, _ = fixture(1, 1, 0)
        config["variables"]["mv"][0]["max_move"] = 0.001
        plant, evidence = fit_history_plant(config, history, 1000, 1300)
        prediction = _free_run(plant, history["mv"], history["dv"])
        np.testing.assert_allclose(prediction[1000:1300], history["cv"][1000:1300], atol=0.02)
        self.assertLess(evidence["validation"]["normalized_rmse"][0], 0.001)

    def test_unsettled_noncausal_nonfinite_and_wrong_shape_are_rejected(self):
        for response in ([0.0] + [1.0, -1.0] * 12, list(np.arange(24, dtype=float)),
                         [1.0] * 24, [0.0, float("nan")] + [1.0] * 22):
            with self.subTest(response=response[:3]):
                with self.assertRaises(ValueError):
                    _fit_path(response, MODELLING_DEFAULTS, 2.0)
        config, history, _ = fixture(1, 1, 0)
        config["variables"]["cv"][0]["is_integrating"] = True
        with self.assertRaisesRegex(ValueError, "Integrating"):
            fit_history_plant(config, history, 1000, 1300)
        config["variables"]["cv"][0]["is_integrating"] = False
        history["cv"] = [[float(i)] for i in range(1600)]
        with self.assertRaises(ValueError):
            fit_history_plant(config, history, 1000, 1300)

    def test_modelling_bounds_are_explicit_and_strict(self):
        for supplied in ({"unrecognized": 1}, {"max_tau_samples": -1},
                         {"validation_max_normalized_rmse": 0.9}, {"max_gain": float("inf")},
                         {"max_delay_samples": True}):
            with self.subTest(settings=supplied):
                with self.assertRaises(ValueError):
                    modelling_settings({"modelling": supplied})


if __name__ == "__main__":
    unittest.main()

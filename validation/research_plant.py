"""Qualified observational SOPDT hypotheses, never independent physical plant truth.

Only the outer training CVs enter identification. Outer validation CVs are used
once for qualification, not parameter fitting; final-test CVs are never accessed.
The discrete dynamics intentionally match campaign_plant.rs, including its
updated-x1 cascade and observation-before-command scan convention.
"""
import numpy as np

from campaign import controller_template
from common import validate_history
from identify import identify


MODELLING_DEFAULTS = {
    "validation_max_normalized_rmse": 0.05,
    "path_max_relative_rmse": 0.08,
    "path_max_relative_error": 0.20,
    "max_tau_samples": 64.0,
    "max_delay_samples": 64.0,
    "max_gain": 1e6,
    "zero_gain_tolerance": 1e-9,
}


def modelling_settings(config):
    """Validate explicit bounded settings under config.modelling; return defaults too.

    Taus/delays are bounded in scans, gains in mapped output/input units. RMSE
    qualification is per CV, divided by its declared high-low engineering range.
    Zero tolerance is an absolute numerical step-coefficient tolerance, not a
    claim that an unexcited physical coupling is absent.
    """
    supplied = config.get("modelling", {})
    if not isinstance(supplied, dict) or set(supplied) - set(MODELLING_DEFAULTS):
        raise ValueError("Unknown observational modelling settings")
    result = {**MODELLING_DEFAULTS, **supplied}
    bounds = {"validation_max_normalized_rmse": (1e-6, 0.25),
              "path_max_relative_rmse": (1e-6, 0.25),
              "path_max_relative_error": (1e-6, 0.5),
              "max_tau_samples": (0.0, 256.0),
              "max_delay_samples": (0.0, 256.0),
              "max_gain": (1e-12, 1e9),
              "zero_gain_tolerance": (0.0, 1e-6)}
    for key, value in result.items():
        lo, hi = bounds[key]
        if (isinstance(value, bool) or not isinstance(value, (int, float))
                or not np.isfinite(value) or not lo <= value <= hi):
            raise ValueError(f"modelling.{key} must be finite in [{lo}, {hi}]")
    return result


def _engineering(config):
    dt = config["sample_time"]
    if isinstance(dt, bool) or not np.isfinite(dt) or dt <= 0:
        raise ValueError("Positive finite sample_time required")
    for group in ("cv", "mv", "dv"):
        variables = config["variables"][group]
        if group != "dv" and not variables:
            raise ValueError("At least one CV and MV required")
        for variable in variables:
            lo, hi = variable["low"], variable["high"]
            if not np.isfinite([lo, hi]).all() or lo >= hi:
                raise ValueError("Finite ordered engineering limits required")
            if not variable.get("name") or not isinstance(variable.get("unit"), str):
                raise ValueError("Explicit variable names and engineering units required")
            if group == "mv" and (not np.isfinite(variable["max_move"]) or variable["max_move"] <= 0):
                raise ValueError("Positive finite engineering MV max_move required")
            if group == "cv":
                if variable.get("is_integrating", False):
                    raise ValueError("Integrating CVs cannot use a settled SOPDT hypothesis")
                if not np.isfinite(variable["target"]) or not lo <= variable["target"] <= hi:
                    raise ValueError("Configured CV target must lie within engineering limits")
    return controller_template(config)


def _step_shapes(taus, count):
    """Unit-gain undelayed responses, with time constants expressed in scans."""
    taus = np.asarray(taus, dtype=float).reshape(-1, 2)
    decay = np.zeros_like(taus)
    positive = taus > 0
    decay[positive] = np.exp(-1.0 / taus[positive])
    state = np.zeros_like(taus)
    result = np.zeros((len(taus), count))
    for t in range(1, count):
        state[:, 0] = decay[:, 0] * state[:, 0] + (1 - decay[:, 0])
        state[:, 1] = decay[:, 1] * state[:, 1] + (1 - decay[:, 1]) * state[:, 0]
        result[:, t] = state[:, 1]
    return result


def _delayed(shapes, delay):
    indices = np.arange(shapes.shape[1]) - int(np.floor(delay))
    fraction = delay - np.floor(delay)
    newer = shapes[:, np.maximum(indices, 0)]
    older = shapes[:, np.maximum(indices - 1, 0)]
    return (1 - fraction) * newer + fraction * older


def _fit_path(response, settings, sample_time):
    response = np.asarray(response, dtype=float)
    if response.ndim != 1 or len(response) < 4 or not np.isfinite(response).all() or response[0] != 0:
        raise ValueError("Nonfinite or noncausal FIR step response")
    magnitude = float(np.max(np.abs(response)))
    if magnitude <= settings["zero_gain_tolerance"]:
        return {"gain": 0.0, "tau1": 0.0, "tau2": 0.0, "delay_seconds": 0.0}, {
            "zero_gain": True, "relative_rmse": 0.0, "relative_max_error": 0.0,
            "absolute_max_error": magnitude, "settled_gain_relative_error": 0.0}
    # A bounded deterministic multistart grid followed by coordinate refinement;
    # gain is solved analytically for each shape, not searched independently.
    maximum_tau = settings["max_tau_samples"]
    maximum_delay = min(settings["max_delay_samples"], len(response) - 2)
    grid = np.unique(np.r_[0.0, np.geomspace(min(0.05, maximum_tau), maximum_tau, 12)]) if maximum_tau > 0 else np.array([0.0])
    family = settings.get("path_family", "sopdt")
    if family not in ("fopdt", "sopdt"):
        raise ValueError("Unsupported parametric path family")
    taus = np.array([(a, 0.0) for a in grid]) if family == "fopdt" else np.array([(a, b) for i, a in enumerate(grid) for b in grid[i:]])
    shapes = _step_shapes(taus, len(response))

    def scores(values):
        denominator = np.sum(values * values, axis=1)
        gains = np.clip((values @ response) / np.maximum(denominator, 1e-30),
                        -settings["max_gain"], settings["max_gain"])
        errors = np.mean((values * gains[:, None] - response) ** 2, axis=1)
        return errors, gains

    best = None
    delays = np.unique(np.r_[np.arange(0.0, maximum_delay + 1e-12, 0.5), maximum_delay])
    for delay in delays:
        errors, gains = scores(_delayed(shapes, delay))
        index = int(np.argmin(errors))
        candidate = (float(errors[index]), np.array([*taus[index], delay]), float(gains[index]))
        if best is None or candidate[0] < best[0]:
            best = candidate
    steps = np.array([max(0.1, best[1][0] / 2), max(0.1, best[1][1] / 2), 0.25])
    upper = np.array([maximum_tau, 0.0 if family == "fopdt" else maximum_tau, maximum_delay])
    for _ in range(100):
        improved = False
        for coordinate in range(3):
            for direction in (-1, 1):
                parameters = best[1].copy()
                parameters[coordinate] = np.clip(parameters[coordinate] + direction * steps[coordinate], 0, upper[coordinate])
                shape = _delayed(_step_shapes([parameters[:2]], len(response)), parameters[2])
                errors, gains = scores(shape)
                if errors[0] < best[0]:
                    best = (float(errors[0]), parameters, float(gains[0]))
                    improved = True
        if not improved:
            steps /= 2
            if np.max(steps) < 1e-5:
                break
    error, parameters, gain = best
    fitted = gain * _delayed(_step_shapes([parameters[:2]], len(response)), parameters[2])[0]
    relative_rmse = float(np.sqrt(error) / magnitude)
    relative_max = float(np.max(np.abs(fitted - response)) / magnitude)
    settled_error = float(abs(gain - response[-1]) / magnitude)
    if (relative_rmse > settings["path_max_relative_rmse"]
            or relative_max > settings["path_max_relative_error"] or settled_error > 0.05):
        raise ValueError(f"FIR path is not a qualified settled causal {family.upper()} approximation "
                         f"(relative RMSE={relative_rmse:.6g}, max={relative_max:.6g}, unsettled={settled_error:.6g})")
    return {"gain": gain, "tau1": float(parameters[0] * sample_time),
            "tau2": float(parameters[1] * sample_time), "delay_seconds": float(parameters[2] * sample_time)}, {
                "zero_gain": False, "relative_rmse": relative_rmse,
                "relative_max_error": relative_max, "absolute_max_error": relative_max * magnitude,
                "settled_gain_relative_error": settled_error}


def _free_run(plant, mv, dv):
    """Zero-prehistory free run; no observed CVs are accepted as an argument."""
    mv, dv = np.asarray(mv, dtype=float), np.asarray(dv, dtype=float)
    count = len(mv)
    actuators = np.empty_like(mv)
    previous = np.asarray(plant["baseline_mv"], dtype=float)
    move = np.asarray(plant["actuator_rate"]) * plant["sample_time"]
    for t in range(count):
        previous = np.clip(previous + np.clip(mv[t] - previous, -move, move),
                           plant["actuator_low"], plant["actuator_high"])
        actuators[t] = previous
    inputs = np.concatenate((actuators - plant["baseline_mv"], dv - plant["baseline_dv"]), axis=1)
    paths = plant["paths"]
    gains = np.array([[path["gain"] for path in row] for row in paths])
    tau1 = np.array([[path["tau1"] for path in row] for row in paths])
    tau2 = np.array([[path["tau2"] for path in row] for row in paths])
    decay1, decay2 = np.zeros_like(tau1), np.zeros_like(tau2)
    decay1[tau1 > 0] = np.exp(-plant["sample_time"] / tau1[tau1 > 0])
    decay2[tau2 > 0] = np.exp(-plant["sample_time"] / tau2[tau2 > 0])
    delays = np.array([[path["delay_seconds"] for path in row] for row in paths]) / plant["sample_time"]
    whole, fraction = np.floor(delays).astype(int), delays % 1
    columns = np.arange(inputs.shape[1])
    x1, x2 = np.zeros_like(gains), np.zeros_like(gains)
    result = np.tile(plant["baseline_cv"], (count, 1)).astype(float)
    for t in range(count - 1):
        index = t - whole
        current = np.where(index >= 0, inputs[np.maximum(index, 0), columns], 0.0)
        older = np.where(index >= 1, inputs[np.maximum(index - 1, 0), columns], 0.0)
        delayed = (1 - fraction) * current + fraction * older
        x1 = decay1 * x1 + (1 - decay1) * delayed
        x2 = decay2 * x2 + (1 - decay2) * x1
        result[t + 1] += np.sum(gains * x2, axis=1)
    return result


def fit_history_plant(config, history, train_end, validation_end):
    """Return (Rust PlantSpec dictionary, auditable qualification evidence).

    Raises ValueError rather than substituting a configured/default truth plant.
    An internal 70/20/10 chronological split is entirely inside outer training;
    the last internal interval is diagnostic only, as required by identify().
    """
    settings = modelling_settings(config)
    settings["path_family"] = config.get("evaluation_model_family", "sopdt")
    model = _engineering(config)
    count = len(history["time_seconds"])
    if (type(train_end) is not int or type(validation_end) is not int
            or not 8 <= train_end < validation_end < count or validation_end - train_end < 2):
        raise ValueError("Training, validation and untouched test intervals required")
    # Slice BEFORE validation/conversion: even malformed final-test CVs cannot
    # leak into either fitting or qualification. Do not validate the full history.
    observed = {"sample_time": history["sample_time"],
                **{key: history[key][:validation_end] for key in ("time_seconds", "cv", "mv", "dv")}}
    validate_history(observed, model)
    training = {"sample_time": observed["sample_time"],
                **{key: observed[key][:train_end] for key in ("time_seconds", "cv", "mv", "dv")}}
    inner_train, inner_validation = int(train_end * 0.7), int(train_end * 0.9)
    identification = config["identification"]
    candidate, fir_evidence = identify(
        model, training, memories=identification.get("memories", [6, 8, 12]),
        ridges=identification.get("ridges", [0.0, 0.001, 0.1]),
        max_condition=identification.get("max_condition", 1e6),
        tail_fraction=identification.get("tail_fraction", 0.05),
        max_coefficient=identification.get("max_coefficient", 1e6),
        split_indices=(inner_train, inner_validation))
    paths, path_evidence = [], []
    for i, variable in enumerate(config["variables"]["cv"]):
        row, records = [], []
        responses = candidate["physics"]["step_coefficients"][i] + candidate["physics"]["dv_coefficients"][i]
        for j, response in enumerate(responses):
            try:
                path, record = _fit_path(response, settings, config["sample_time"])
            except ValueError as error:
                raise ValueError(f"CV {variable['name']}, input {j}: {error}") from error
            row.append(path)
            records.append({"input_index": j, **record})
        paths.append(row)
        path_evidence.append({"cv": variable["name"], "paths": records})
    cvs, mvs = config["variables"]["cv"], config["variables"]["mv"]
    ranges = np.array([v["high"] - v["low"] for v in cvs])
    plant = {"sample_time": config["sample_time"], "baseline_cv": list(training["cv"][0]),
             "baseline_mv": list(training["mv"][0]), "baseline_dv": list(training["dv"][0]),
             "actuator_tau": [0.0] * len(mvs), "actuator_low": [v["low"] for v in mvs],
             "actuator_high": [v["high"] for v in mvs],
             "actuator_rate": [(v["high"] - v["low"]) / config["sample_time"] for v in mvs],
             "paths": paths, "nonlinear_strength": [0.0] * len(cvs), "nonlinear_scale": ranges.tolist()}
    if any(not v["low"] <= value <= v["high"] for v, value in zip(mvs, plant["baseline_mv"])):
        raise ValueError("Initial recorded MV lies outside engineering limits")
    commands = np.asarray(observed["mv"], dtype=float)
    if np.any(commands < plant["actuator_low"]) or np.any(commands > plant["actuator_high"]):
        raise ValueError("Recorded MV commands exceed the declared operating envelope; select a matching operating regime or correct the engineering limits")
    prediction = _free_run(plant, observed["mv"], observed["dv"])
    actual = np.asarray(observed["cv"][train_end:validation_end], dtype=float)
    residual = actual - prediction[train_end:validation_end]
    rmse = np.sqrt(np.mean(residual ** 2, axis=0))
    normalized = rmse / ranges
    if not np.isfinite(prediction).all() or np.any(normalized > settings["validation_max_normalized_rmse"]):
        raise ValueError(f"Observational {settings['path_family'].upper()} failed chronological free-run qualification: normalized CV RMSE {normalized.tolist()}")
    evidence = {
        "status": "qualified_observational_hypothesis", "independent_physical_truth": False,
        "model_class": f"stable linear discrete {settings['path_family'].upper()} with fractional transport delay",
        "input_coordinate": "recorded MV commands and recorded DVs in explicitly mapped engineering units",
        "timing": "Observe y[t] before command[t]; first response y[t+1]; fractional delay interpolates current/older input; x2 uses updated x1",
        "settings": settings, "fit_interval": [0, train_end],
        "coefficient_fit_interval": [fir_evidence["selection"]["memory"], inner_train],
        "internal_selection_interval": [inner_train, inner_validation],
        "internal_diagnostic_interval": [inner_validation, train_end],
        "validation_interval": [train_end, validation_end], "test_cv_accessed": False,
        "initialization": "First training CV/MV/DV; assumed steady prehistory; uninterrupted free run from row zero",
        "actuator_policy": "Instantaneous effective command coordinate; lower-loop dynamics belong in fitted paths. Future MPC max_move never clips historical commands during qualification",
        "variables": {group: [{"name": v["name"], "unit": v["unit"]} for v in config["variables"][group]] for group in ("cv", "mv", "dv")},
        "fir_identification": fir_evidence, "path_approximations": path_evidence,
        "validation": {"samples": validation_end - train_end, "rmse": rmse.tolist(),
                       "normalized_rmse": normalized.tolist(), "engineering_range": ranges.tolist(),
                       "mae": np.mean(np.abs(residual), axis=0).tolist(),
                       "max_absolute_error": np.max(np.abs(residual), axis=0).tolist()},
        "limitations": ["Observational prediction is hypothesis evidence, not factory conformity or causal control authority",
                        "Closed-loop feedback, correlated/unmeasured disturbances and operator actions can confound fitted paths",
                        "Steady initial state, stable linear dynamics and representative excitation are assumptions, not physical guarantees",
                        "Zero-gain paths mean numerically absent fitted coupling, not proven absence of physical influence",
                        "Qualification uses recorded inputs without CV correction; counterfactual closed-loop behavior remains unverified"]}
    return plant, evidence


def scenario_from_history(config, history, plant):
    """Freeze recorded commands/DVs and configured targets, never recorded CV feedback."""
    model = _engineering(config)
    count = len(history["time_seconds"])
    scenario = {"sample_time": history["sample_time"], "time_seconds": list(history["time_seconds"]),
                "cv": [list(plant["baseline_cv"]) for _ in range(count)],
                "mv": [list(row) for row in history["mv"]], "dv": [list(row) for row in history["dv"]],
                "targets": [[v["target"] for v in config["variables"]["cv"]] for _ in range(count)],
                "measurement_noise": [], "process_noise": [], "gain_scale": [],
                "provenance": {"synthetic": False, "evidence_kind": "observational_hypothesis",
                               "cv_role": "Frozen initial baseline only; Rust hypothesis plant generates CVs without recorded CV feedback",
                               "inputs": "Frozen mapped recorded MV commands and DVs", "independent_physical_truth": False}}
    validate_history(scenario, model)
    if plant["sample_time"] != scenario["sample_time"]:
        raise ValueError("Hypothesis plant/scenario sample_time mismatch")
    return scenario

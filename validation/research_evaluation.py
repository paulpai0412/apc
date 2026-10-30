"""Frozen engineering objectives for full-resolution Rust replay traces.

These are observational/identified-twin metrics, not independent physical-plant
validation. Population variance of the physical CV is reported separately from
variance of CV minus the contemporaneous target. Only the latter enters ranking
and variance gates: a changing target must not reward a frozen process output.
All scales, coefficients, gates, specs and scenario names belong in the study
snapshot before search. Controller tuning weights never enter this objective.
"""
import copy
import hashlib
import math

import numpy as np

from campaign import trace_metrics


def objective_defaults(config):
    """Conservative editable defaults; absent quality specifications stay absent."""
    cvs = []
    for variable in config["variables"]["cv"]:
        span = variable["high"] - variable["low"]
        low, high = variable.get("spec_low"), variable.get("spec_high")
        cvs.append({"name": variable["name"], "scale": span,
                    "max_abs_bias": 0.02 * span,
                    "max_operating_violation_fraction": 0.0,
                    "max_spec_violation_fraction": 0.0,
                    "max_variance_increase_fraction": 0.0,
                    "min_variance_improvement_fraction": 0.01,
                    "quality_spec": None if low is None and high is None else {"low": low, "high": high}})
    return validate_objective({"schema_version": 1, "scenarios": ["nominal"],
                               "coefficients": {"variance": 1.0, "bias": 1.0, "quality": 10.0, "movement": 0.1},
                               "cv": cvs, "mv": [{"name": v["name"], "scale": v["high"] - v["low"],
                                                   "max_movement_increase_fraction": 0.1}
                                                  for v in config["variables"]["mv"]]}, config)


def _number(value, label, *, minimum=None, maximum=None, positive=False):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        raise ValueError(f"{label} must be a finite number")
    if positive and value <= 0 or minimum is not None and value < minimum or maximum is not None and value > maximum:
        raise ValueError(f"{label} is outside its allowed range")
    return float(value)


def _keys(value, expected, label):
    if not isinstance(value, dict) or set(value) != set(expected):
        raise ValueError(f"{label} requires exactly: {', '.join(sorted(expected))}")


def validate_objective(policy, config):
    """Return an independent JSON-ready policy; reject omissions/typos, not silently default them."""
    _keys(policy, ("schema_version", "scenarios", "coefficients", "cv", "mv"), "Objective")
    if type(policy["schema_version"]) is not int or policy["schema_version"] != 1:
        raise ValueError("Objective schema_version must be 1")
    result = copy.deepcopy(policy)
    scenarios = result["scenarios"]
    if not isinstance(scenarios, list) or not scenarios or any(not isinstance(s, str) or not s.strip() for s in scenarios):
        raise ValueError("Objective scenarios must be a nonempty list of names")
    if len(set(scenarios)) != len(scenarios):
        raise ValueError("Objective scenarios must be unique")
    _number(config["sample_time"], "sample_time", positive=True)
    _keys(result["coefficients"], ("variance", "bias", "quality", "movement"), "Objective coefficients")
    for name, value in result["coefficients"].items():
        result["coefficients"][name] = _number(value, name, minimum=0, positive=name in ("variance", "bias"))
    for group in ("cv", "mv"):
        rows, variables = result[group], config["variables"][group]
        if not variables or not isinstance(rows, list) or len(rows) != len(variables):
            raise ValueError(f"Objective {group} dimensions must match nonempty configured variables")
        names = [v["name"] for v in variables]
        if len(set(names)) != len(names):
            raise ValueError(f"Configured {group} names must be unique")
        for row, variable in zip(rows, variables):
            expected = {"name", "scale", "max_movement_increase_fraction"} if group == "mv" else {
                "name", "scale", "max_abs_bias", "max_operating_violation_fraction", "max_spec_violation_fraction",
                "max_variance_increase_fraction", "min_variance_improvement_fraction", "quality_spec"}
            _keys(row, expected, f"Objective {group}")
            if row["name"] != variable["name"]:
                raise ValueError(f"Objective {group} names/order must match configured variables")
            low = _number(variable["low"], "Operating low")
            high = _number(variable["high"], "Operating high")
            if low >= high:
                raise ValueError("Operating limits must be ordered")
            for field in expected - {"name", "quality_spec"}:
                maximum = 1 if field in ("max_operating_violation_fraction", "max_spec_violation_fraction", "min_variance_improvement_fraction") else None
                row[field] = _number(row[field], f"{row['name']} {field}", minimum=0, maximum=maximum, positive=field == "scale")
            if group == "mv":
                _number(variable["max_move"], "max_move", positive=True)
                continue
            spec = row["quality_spec"]
            if spec is not None:
                _keys(spec, ("low", "high"), "Quality specification")
                for side in ("low", "high"):
                    if spec[side] is not None:
                        spec[side] = _number(spec[side], f"Quality spec {side}")
                if spec["low"] is None and spec["high"] is None:
                    raise ValueError("An unspecified quality_spec must be null")
                if spec["low"] is not None and spec["high"] is not None and spec["low"] >= spec["high"]:
                    raise ValueError("Quality specifications must be ordered")
    return result


def research_metrics(trace, config, policy, first=0, last=None):
    """Measure [first,last) scans; movement uses only transitions within that window.

    Score = mean(normalized error variance) * variance coefficient
          + mean(normalized squared signed bias) * bias coefficient
          + mean(spec violation fraction + normalized squared excursion) * quality coefficient
          + mean(normalized squared command delta) * movement coefficient.
    Quality contributions use the fixed total CV count; absent specs are null,
    not inferred from operating limits. With no specs the quality component is null
    and contributes nothing. Scenarios are equally weighted by the aggregator.
    """
    policy = validate_objective(policy, config)
    count = len(trace["cv"])
    last = count if last is None else last
    if type(first) is not int or type(last) is not int or not 0 <= first < last <= count:
        raise ValueError("Metric window must be a nonempty in-range integer interval")
    arrays = {}
    for key, width in (("cv", len(policy["cv"])), ("targets", len(policy["cv"])),
                       ("mv", len(policy["mv"])), ("command_mv", len(policy["mv"]))):
        array = np.asarray(trace[key][first:last], dtype=float)
        if len(trace[key]) != count or array.shape != (last - first, width) or not np.isfinite(array).all():
            raise ValueError(f"Incomplete/non-finite {key} trace")
        arrays[key] = array
    dt = float(config["sample_time"])
    time = np.asarray(trace["time_seconds"][first:last], dtype=float)
    if len(trace["time_seconds"]) != count or time.shape != (last - first,) or not np.isfinite(time).all():
        raise ValueError("Incomplete/non-finite trace timeline")
    if len(time) > 1 and not np.allclose(np.diff(time), dt, rtol=1e-9, atol=1e-9):
        raise ValueError("Trace must retain every full-resolution scan")
    result = trace_metrics(trace, config, first, last)
    result["campaign_score"] = result["score"]
    cv, targets, command = arrays["cv"], arrays["targets"], arrays["command_mv"]
    error = cv - targets
    scale = np.asarray([row["scale"] for row in policy["cv"]])
    mv_scale = np.asarray([row["scale"] for row in policy["mv"]])
    variance, bias = np.var(error, axis=0), np.mean(error, axis=0)
    delta = np.diff(command, axis=0)
    variation = np.asarray(result["mv_total_variation"])
    movement_duration = max(len(cv) - 1, 0) * dt
    normalized_movement = np.mean((delta / mv_scale) ** 2, axis=0) if len(delta) else np.zeros(len(mv_scale))
    seconds, fractions, excursions, quality_terms = [], [], [], []
    for j, row in enumerate(policy["cv"]):
        spec = row["quality_spec"]
        if spec is None:
            seconds.append(None)
            fractions.append(None)
            excursions.append(None)
            quality_terms.append(None)
            continue
        distance = np.zeros(len(cv))
        if spec["low"] is not None:
            distance += np.maximum(spec["low"] - cv[:, j], 0)
        if spec["high"] is not None:
            distance += np.maximum(cv[:, j] - spec["high"], 0)
        fraction = float(np.mean(distance > 0))
        seconds.append(float(np.count_nonzero(distance) * dt))
        fractions.append(fraction)
        excursions.append(float(np.mean(distance ** 2)))
        quality_terms.append(fraction + float(np.mean((distance / scale[j]) ** 2)))
    components = {"variance": float(np.mean(variance / scale ** 2)),
                  "bias": float(np.mean((bias / scale) ** 2)),
                  "quality": sum(value for value in quality_terms if value is not None) / len(scale) if any(value is not None for value in quality_terms) else None,
                  "movement": float(np.mean(normalized_movement))}
    result.update(score=sum(policy["coefficients"][name] * value for name, value in components.items() if value is not None),
                  score_components=components, sample_time=dt, duration_seconds=len(cv) * dt,
                  movement_duration_seconds=movement_duration, window={"first": first, "last": last},
                  targets_sha256=hashlib.sha256(np.asarray(targets, dtype="<f8").tobytes()).hexdigest(),
                  timeline_sha256=hashlib.sha256(np.asarray(time, dtype="<f8").tobytes()).hexdigest(),
                  cv_mean=np.mean(cv, axis=0).tolist(), cv_std=np.std(cv, axis=0).tolist(),
                  cv_variance=np.var(cv, axis=0).tolist(), cv_tracking_error_variance=variance.tolist(),
                  cv_tracking_bias=bias.tolist(), cv_operating_violation_fraction=(np.asarray(result["cv_violation_seconds"]) / (len(cv) * dt)).tolist(),
                  cv_spec_violation_seconds=seconds, cv_spec_violation_fraction=fractions,
                  cv_spec_excursion_mean_square=excursions,
                  quality_assessed=[row["quality_spec"] is not None for row in policy["cv"]],
                  mv_total_variation_rate=(variation / movement_duration if movement_duration else np.zeros(len(mv_scale))).tolist(),
                  mv_normalized_total_variation=(variation / mv_scale).tolist(),
                  mv_normalized_movement=normalized_movement.tolist(),
                  variance_gate_basis="tracking_error_variance",
                  evidence_scope="Replay/observational evidence, not independent physical-plant validation")
    return result


def _change(reference, current, *, reduction):
    difference = reference - current if reduction else current - reference
    return {"absolute": difference, "fraction": difference / reference if reference != 0 else None,
            "baseline_zero": reference == 0}


def assess_candidate(metrics, baseline_metrics, config, policy, *, baseline=False):
    """Apply every gate; baseline skips only relative-improvement/movement gates.

    Zero reference variance permits only zero candidate variance. Zero reference
    movement permits only zero candidate movement. Relative changes are null in
    both cases: neither epsilon denominators nor invented percentages are used.
    A zero minimum-improvement fraction disables that minimum; the independently
    configured no-worsening/increase gate still applies.
    """
    policy = validate_objective(policy, config)
    reasons, improvements = [], {"cv": [], "mv": []}
    if metrics.get("samples", 0) <= 0:
        reasons.append("No evaluated samples")
    if metrics.get("solver_calls") != metrics.get("samples") or metrics.get("solver_failures") != 0:
        reasons.append("Missing per-scan solver telemetry or solver failures")
    if isinstance(metrics.get("score"), bool) or not isinstance(metrics.get("score"), (int, float)) or not math.isfinite(metrics["score"]):
        reasons.append("Missing/non-finite objective score")
    reference = metrics if baseline else baseline_metrics
    if not baseline:
        if not isinstance(reference, dict):
            reasons.append("Missing matching baseline metrics")
            reference = None
        else:
            for field in ("samples", "sample_time", "window", "targets_sha256", "timeline_sha256"):
                if field not in metrics or field not in reference or metrics[field] != reference[field]:
                    reasons.append(f"Baseline {field} does not match evaluated interval/scenario")
            if reference.get("solver_calls") != reference.get("samples") or reference.get("solver_failures") != 0:
                reasons.append("Matching baseline has missing solver telemetry or solver failures")
    def vector(source, field, width, *, nullable=False):
        values = source.get(field) if isinstance(source, dict) else None
        if not isinstance(values, list) or len(values) != width or any(
                not (nullable and v is None) and (isinstance(v, bool) or not isinstance(v, (int, float)) or not math.isfinite(v)) for v in values):
            reasons.append(f"Missing/invalid {field}")
            return [None] * width
        return values
    cv_count, mv_count = len(policy["cv"]), len(policy["mv"])
    bias = vector(metrics, "cv_tracking_bias", cv_count)
    operating = vector(metrics, "cv_operating_violation_fraction", cv_count)
    specs = vector(metrics, "cv_spec_violation_fraction", cv_count, nullable=True)
    variance = vector(metrics, "cv_tracking_error_variance", cv_count)
    raw_variance = vector(metrics, "cv_variance", cv_count)
    old_variance = vector(reference, "cv_tracking_error_variance", cv_count)
    old_raw_variance = vector(reference, "cv_variance", cv_count)
    reference_hard = vector(reference, "mv_hard_violations", mv_count)
    if not baseline and any(value is not None and value != 0 for value in reference_hard):
        reasons.append("Matching baseline has hard actuator constraint violations")
    for j, row in enumerate(policy["cv"]):
        name = row["name"]
        if bias[j] is not None and abs(bias[j]) > row["max_abs_bias"]:
            reasons.append(f"CV {name}: absolute tracking bias exceeds engineering-unit limit")
        if operating[j] is not None and not 0 <= operating[j] <= row["max_operating_violation_fraction"]:
            reasons.append(f"CV {name}: operating violation fraction exceeds limit")
        if row["quality_spec"] is not None:
            if specs[j] is None:
                reasons.append(f"CV {name}: configured quality specification was not assessed")
            elif not 0 <= specs[j] <= row["max_spec_violation_fraction"]:
                reasons.append(f"CV {name}: quality specification violation fraction exceeds limit")
        for label, values in (("tracking-error variance", variance), ("physical CV variance", raw_variance),
                              ("baseline tracking-error variance", old_variance), ("baseline physical CV variance", old_raw_variance)):
            if values[j] is not None and values[j] < 0:
                reasons.append(f"CV {name}: negative {label}")
        if variance[j] is not None and old_variance[j] is not None:
            improvements["cv"].append({"name": name,
                "tracking_error_variance_reduction": _change(old_variance[j], variance[j], reduction=True),
                "physical_variance_reduction": _change(old_raw_variance[j], raw_variance[j], reduction=True) if old_raw_variance[j] is not None and raw_variance[j] is not None else None})
            if not baseline:
                if variance[j] > old_variance[j] * (1 + row["max_variance_increase_fraction"]):
                    reasons.append(f"CV {name}: tracking-error variance no-worsening gate failed")
                if row["min_variance_improvement_fraction"] > 0 and variance[j] > old_variance[j] * (1 - row["min_variance_improvement_fraction"]):
                    reasons.append(f"CV {name}: minimum tracking-error variance improvement not reached")
    hard = vector(metrics, "mv_hard_violations", mv_count)
    movement = vector(metrics, "mv_total_variation", mv_count)
    old_movement = vector(reference, "mv_total_variation", mv_count)
    for j, row in enumerate(policy["mv"]):
        name = row["name"]
        if hard[j] is not None and hard[j] != 0:
            reasons.append(f"MV {name}: hard actuator range/move constraint violation")
        if movement[j] is not None and old_movement[j] is not None:
            improvements["mv"].append({"name": name, "total_variation_increase": _change(old_movement[j], movement[j], reduction=False)})
            if movement[j] < 0 or old_movement[j] < 0:
                reasons.append(f"MV {name}: negative command movement")
            elif not baseline and movement[j] > old_movement[j] * (1 + row["max_movement_increase_fraction"]):
                reasons.append(f"MV {name}: command movement increase exceeds matching baseline limit")
    return {"eligible": not reasons, "reasons": reasons, "improvements": improvements}


def aggregate_evaluations(metrics_by_scenario, baseline_by_scenario, config, policy, *, baseline=False):
    """Equal-scenario mean score, independent of eligibility; all gates must pass.

    A complete numerically finite rejected trial still has a score for reporting.
    Missing/non-finite scenario scores yield null, never a mean of the good subset.
    """
    policy = validate_objective(policy, config)
    reasons, scenarios, scores = [], {}, []
    expected = set(policy["scenarios"])
    for name in sorted(set(metrics_by_scenario) - expected):
        reasons.append(f"Undeclared candidate scenario: {name}")
    if not baseline:
        for name in sorted(set(baseline_by_scenario) - expected):
            reasons.append(f"Undeclared baseline scenario: {name}")
    for name in policy["scenarios"]:
        metrics = metrics_by_scenario.get(name)
        if metrics is None:
            assessment = {"eligible": False, "reasons": ["Missing declared scenario metrics"], "improvements": {"cv": [], "mv": []}}
        else:
            assessment = assess_candidate(metrics, baseline_by_scenario.get(name), config, policy, baseline=baseline)
            if isinstance(metrics.get("score"), (int, float)) and not isinstance(metrics["score"], bool) and math.isfinite(metrics["score"]):
                scores.append(metrics["score"])
        scenarios[name] = assessment
        reasons.extend(f"{name}: {reason}" for reason in assessment["reasons"])
    return {"eligible": not reasons, "score": float(np.mean(scores)) if len(scores) == len(policy["scenarios"]) else None,
            "reasons": reasons, "scenarios": scenarios}

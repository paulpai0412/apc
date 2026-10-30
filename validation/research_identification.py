"""Real FIR/FOPDT/SOPDT control identification with separate evaluation-plant identity.

Parametric fits use only outer training FIR responses, then qualify against
outer validation. Exact discrete responses are exported to the existing engine;
loading a preconfigured parametric matrix is never called identification.
"""
import copy

import numpy as np

from campaign import controller_template
from identify import identify, frozen_prediction_metrics
from research_plant import fit_history_plant, _step_shapes, _delayed


def identify_control(config, history, train_end, validation_end):
    family = config.get("model_family", "fir")
    if family not in ("fir", "fopdt", "sopdt"):
        raise ValueError("Unsupported control identification family")
    template = controller_template(config)
    if family == "fir":
        options = dict(config["identification"])
        options.pop("minimum_input_std", None)
        model, report = identify(template, history, split_indices=(train_end, validation_end), evaluate_test=False, **options)
        report["family"] = family
        return model, report
    fitting = copy.deepcopy(config)
    fitting["evaluation_model_family"] = family
    parametric, evidence = fit_history_plant(fitting, history, train_end, validation_end)
    model = copy.deepcopy(template)
    model["metadata"]["model_type"] = "step_response"
    model["metadata"]["description"] = f"CANDIDATE ONLY: identified discrete {family.upper()} exported as exact step responses; " + model["metadata"].get("description", "")
    horizon, dt = model["tuning"]["prediction_horizon"], config["sample_time"]
    mv_count = len(config["variables"]["mv"])
    step, disturbance = [], []
    for row in parametric["paths"]:
        responses = []
        for path in row:
            shape = _delayed(_step_shapes([[path["tau1"] / dt, path["tau2"] / dt]], horizon), path["delay_seconds"] / dt)[0]
            response = path["gain"] * shape
            if abs(path["gain"] - response[-1]) > config["identification"].get("tail_fraction", 0.05) * max(abs(path["gain"]), 1e-10):
                raise ValueError(f"{family.upper()} response has not settled within declared model memory; increase engineering horizon, never truncate dynamics")
            responses.append(response.tolist())
        step.append(responses[:mv_count])
        disturbance.append(responses[mv_count:])
    model["physics"] = {"step_coefficients": step, "dv_coefficients": disturbance}
    def observed(end):
        return {"sample_time": history["sample_time"], **{key: history[key][:end] for key in ("time_seconds", "cv", "mv", "dv")}}
    memory = evidence["fir_identification"]["selection"]["memory"]
    train_metrics = frozen_prediction_metrics(model, observed(train_end), memory)["test"]
    validation = frozen_prediction_metrics(model, observed(validation_end), train_end)["test"]
    report = {"status": "candidate_only", "family": family, "causality": "Observational prediction alone does not establish causal control authority",
              "selection": evidence["fir_identification"]["selection"], "candidates": evidence["fir_identification"]["candidates"],
              "splits": {"train": [0, train_end], "validation": [train_end, validation_end], "test": [validation_end, len(history["time_seconds"])]},
              "train": train_metrics, "validation": validation, "test": None, "full": None,
              "parametric_paths": parametric["paths"], "parameter_units": {"gain": "mapped CV unit / mapped input unit", "tau1": "seconds", "tau2": "seconds", "delay_seconds": "seconds"},
              "parameter_fit": evidence, "test_policy": "Deferred: final test values not accessed; no family choice after sealed validation",
              "input_order": [row["name"] for role in ("mv", "dv") for row in config["variables"][role]],
              "output_order": [row["name"] for row in config["variables"]["cv"]],
              "assumptions": evidence["limitations"], "timing": evidence["timing"]}
    return model, report

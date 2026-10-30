"""Finite-memory observational candidate identification; not causal certification."""
import argparse
import copy

import numpy as np

from common import fingerprint, load_json, main_guard, save_json, validate_history


def chronological_split(count, train_fraction=0.6, validation_fraction=0.2):
    if not (0 < train_fraction < 1 and 0 < validation_fraction < 1 and train_fraction + validation_fraction < 1):
        raise ValueError("Train/validation fractions must leave a nonempty test interval")
    train_end = int(count * train_fraction)
    validation_end = int(count * (train_fraction + validation_fraction))
    if min(train_end, validation_end - train_end, count - validation_end) < 2:
        raise ValueError("Each chronological split needs at least two samples")
    return train_end, validation_end


def lag_matrix(inputs, memory):
    """Input at t acts no earlier than t+1; first-row levels extend into prehistory."""
    centered = inputs - inputs[0]
    design = np.zeros((len(inputs), inputs.shape[1] * memory))
    for lag in range(1, memory + 1):
        design[lag:, (lag - 1) * inputs.shape[1]:lag * inputs.shape[1]] = centered[:-lag]
    return design


def metrics(actual, predicted):
    error = actual - predicted
    return {"rmse": np.sqrt(np.mean(error * error, axis=0)).tolist(),
            "mae": np.mean(np.abs(error), axis=0).tolist(), "samples": len(actual)}


def identify(model, history, memories=(6, 8, 12), ridges=(0.0, 0.001, 0.1),
             train_fraction=0.6, validation_fraction=0.2, max_condition=1e6,
             tail_fraction=0.05, max_coefficient=1e6, split_indices=None, evaluate_test=True):
    original_count = len(history["time_seconds"])
    if not evaluate_test:
        if split_indices is None or len(split_indices) != 2 or type(split_indices[1]) is not int:
            raise ValueError("Deferred test evaluation requires explicit chronological split indices")
        end = split_indices[1]
        history = {"sample_time": history["sample_time"],
                   **{key: history[key][:end] for key in ("time_seconds", "cv", "mv", "dv")}}
    validate_history(history, model)
    if any(cv.get("is_integrating", False) for cv in model["variables"]["cvs"]):
        raise ValueError("Integrating CVs are not supported by settled finite-memory identification")
    if not (0 < tail_fraction < 1 and np.isfinite(max_condition) and max_condition > 1
            and np.isfinite(max_coefficient) and max_coefficient > 0):
        raise ValueError("Invalid identification bounds")
    horizon = model["tuning"]["prediction_horizon"]
    plateau = max(3, horizon // 10)
    if not memories or not ridges or len(memories) * len(ridges) > 100:
        raise ValueError("Provide 1..100 finite memory/ridge combinations")
    if any(not isinstance(m, int) or m < 2 or m > 256 or m > horizon - plateau for m in memories):
        raise ValueError("Memory must be 2..256 and leave at least max(3,horizon/10) settled coefficients")
    if any(not np.isfinite(r) or r < 0 or r > 1e6 for r in ridges):
        raise ValueError("Ridge values must be finite in [0, 1e6]")
    outputs = np.asarray(history["cv"], dtype=float)
    inputs = np.concatenate((np.asarray(history["mv"], dtype=float), np.asarray(history["dv"], dtype=float)), axis=1)
    if split_indices is None:
        train_end, val_end = chronological_split(len(outputs), train_fraction, validation_fraction)
    else:
        train_end, val_end = split_indices
        if not (type(train_end) is int and type(val_end) is int
                and 2 <= train_end < val_end <= len(outputs)
                and val_end - train_end >= 2
                and (not evaluate_test or len(outputs) - val_end >= 2)):
            raise ValueError("Explicit chronological splits need training/validation and a nonempty test unless test evaluation is deferred")
    scale_y = outputs[:train_end].std(axis=0)
    if np.any(scale_y < 1e-10):
        raise ValueError("Constant training CV: no identifiable dynamic response")
    response = outputs - outputs[0]
    candidates = []
    best = None
    for memory in memories:
        design = lag_matrix(inputs, memory)
        train = design[memory:train_end]
        if len(train) < 5 * train.shape[1]:
            candidates.append({"memory": memory, "valid": False, "reason": "Need >=5 training rows per FIR coefficient"})
            continue
        scale = np.sqrt(np.mean(train * train, axis=0))
        if np.any(scale < 1e-10):
            candidates.append({"memory": memory, "valid": False, "reason": "Constant/unexcited input"})
            continue
        normalized = train / scale
        singular = np.linalg.svd(normalized, compute_uv=False)
        condition = float(singular[0] / singular[-1]) if singular[-1] > 0 else float("inf")
        if not np.isfinite(condition) or condition > max_condition:
            candidates.append({"memory": memory, "valid": False, "reason": "Rank deficient/collinear or insufficient persistent excitation"})
            continue
        for ridge in ridges:
            record = {"memory": memory, "ridge": ridge, "condition": condition, "valid": False}
            # Ridge on dimensionless input columns; no intercept fitted from later CVs.
            augmented = np.concatenate((normalized, np.sqrt(ridge) * np.eye(normalized.shape[1])))
            target = np.concatenate((response[memory:train_end], np.zeros((normalized.shape[1], outputs.shape[1]))))
            coefficients = np.linalg.lstsq(augmented, target, rcond=None)[0] / scale[:, None]
            impulse = coefficients.reshape(memory, inputs.shape[1], outputs.shape[1])
            tail = np.sum(np.abs(impulse[-max(1, memory // 4):]), axis=0)
            magnitude = np.sum(np.abs(impulse), axis=0)
            # Every observed path needs a settled impulse tail; exact zero paths are allowed only with PE.
            if np.any(tail > tail_fraction * np.maximum(magnitude, 1e-10)):
                record["reason"] = "Unsettled FIR tail: increase declared memory; do not truncate integrating dynamics"
            elif not np.all(np.isfinite(coefficients)) or np.max(np.abs(np.cumsum(impulse, axis=0))) > max_coefficient:
                record["reason"] = "Non-finite or out-of-bound step coefficients"
            else:
                prediction = outputs[0] + design @ coefficients
                score = float(np.mean(np.mean((outputs[train_end:val_end] - prediction[train_end:val_end]) ** 2, axis=0) / scale_y ** 2))
                record.update(valid=True, validation_score=score,
                              train=metrics(outputs[memory:train_end], prediction[memory:train_end]),
                              validation=metrics(outputs[train_end:val_end], prediction[train_end:val_end]))
                if best is None or score < best[0]:
                    best = (score, memory, ridge, impulse, prediction)
            candidates.append(record)
    if best is None:
        reasons = sorted({record.get("reason", "invalid") for record in candidates})
        raise ValueError("No identifiable settled candidate: " + "; ".join(reasons))
    score, memory, ridge, impulse, prediction = best
    result = copy.deepcopy(model)
    result["metadata"]["model_type"] = "step_response"
    result["metadata"]["description"] = "CANDIDATE ONLY: observational finite-memory fit; " + result["metadata"].get("description", "")
    step = np.zeros((horizon, inputs.shape[1], outputs.shape[1]))
    step[1:memory + 1] = np.cumsum(impulse, axis=0)
    step[memory + 1:] = step[memory]
    mv_count = len(model["variables"]["mvs"])
    result["physics"] = {"step_coefficients": step[:, :mv_count].transpose(2, 1, 0).tolist(),
                         "dv_coefficients": step[:, mv_count:].transpose(2, 1, 0).tolist()}
    report = {"status": "candidate_only", "causality": "Observational prediction does not establish causal control authority",
              "assumptions": ["First sample is steady state; earlier inputs equal first-row levels",
                              "Stable linear time-invariant finite-memory plant; no integrating dynamics",
                              "FIR tail check is a diagnostic, not proof of physical settling"],
              "timing": "g[0]=0; u[t] first changes y[t+1]; constant tail retained",
              "splits": {"train": [0, train_end], "validation": [train_end, val_end], "test": [val_end, original_count]},
              "selection": {"memory": memory, "ridge": ridge, "validation_score": score},
              "candidates": candidates,
              "full": metrics(outputs[memory:], prediction[memory:]) if evaluate_test else None,
              "train": metrics(outputs[memory:train_end], prediction[memory:train_end]),
              "validation": metrics(outputs[train_end:val_end], prediction[train_end:val_end]),
              "full_one_step_persistence": metrics(outputs[memory:], outputs[memory - 1:-1]) if evaluate_test else None,
              "train_one_step_persistence": metrics(outputs[memory:train_end], outputs[memory - 1:train_end - 1]),
              "validation_one_step_persistence": metrics(outputs[train_end:val_end], outputs[train_end - 1:val_end - 1]),
              "test": metrics(outputs[val_end:], prediction[val_end:]) if evaluate_test else None,
              "test_one_step_persistence": metrics(outputs[val_end:], outputs[val_end - 1:-1]) if evaluate_test else None,
              "test_frozen_origin_persistence": metrics(outputs[val_end:], np.broadcast_to(outputs[val_end - 1], outputs[val_end:].shape)) if evaluate_test else None,
              "test_policy": "Test evaluated after model selection; no refit" if evaluate_test else "Deferred: test values were not read; evaluate only after all controller candidates are frozen"}
    return result, report


def frozen_prediction_metrics(model, history, first):
    """Evaluate an already frozen FIR with input history, never later CV feedback."""
    validate_history(history, model)
    outputs = np.asarray(history["cv"], dtype=float)
    inputs = np.concatenate((np.asarray(history["mv"], dtype=float), np.asarray(history["dv"], dtype=float)), axis=1)
    if type(first) is not int or not 1 <= first < len(outputs):
        raise ValueError("Held-out prediction needs a nonempty later interval")
    step = np.concatenate((np.asarray(model["physics"]["step_coefficients"], dtype=float),
                           np.asarray(model["physics"]["dv_coefficients"], dtype=float).reshape(len(outputs[0]), len(model["variables"]["dvs"]), -1)
                           if model["variables"]["dvs"] else np.empty((len(outputs[0]), 0, model["tuning"]["prediction_horizon"]))), axis=1)
    if np.any(step[:, :, 0] != 0):
        raise ValueError("Identified historical prediction requires a strictly causal first coefficient")
    impulse = np.diff(step, axis=2)
    centered = inputs - inputs[0]
    prediction = np.tile(outputs[0], (len(outputs), 1))
    for cv in range(outputs.shape[1]):
        for channel in range(inputs.shape[1]):
            prediction[1:, cv] += np.convolve(centered[:, channel], impulse[cv, channel], mode="full")[:len(outputs) - 1]
    return {"test": metrics(outputs[first:], prediction[first:]),
            "test_one_step_persistence": metrics(outputs[first:], outputs[first - 1:-1]),
            "test_frozen_origin_persistence": metrics(outputs[first:], np.broadcast_to(outputs[first - 1], outputs[first:].shape)),
            "test_policy": "Evaluated after all control-method winners were frozen; no refit and no future CV feedback"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", required=True)
    parser.add_argument("--history", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--report", required=True)
    parser.add_argument("--memories", type=int, nargs="+", default=[6, 8, 12])
    parser.add_argument("--ridges", type=float, nargs="+", default=[0, 0.001, 0.1])
    parser.add_argument("--train-fraction", type=float, default=0.6)
    parser.add_argument("--validation-fraction", type=float, default=0.2)
    parser.add_argument("--max-condition", type=float, default=1e6)
    parser.add_argument("--tail-fraction", type=float, default=0.05)
    parser.add_argument("--max-coefficient", type=float, default=1e6)
    args = parser.parse_args()
    model, report = identify(load_json(args.model), load_json(args.history), args.memories, args.ridges,
                             args.train_fraction, args.validation_fraction, args.max_condition,
                             args.tail_fraction, args.max_coefficient)
    report["provenance"] = {"baseline_sha256": fingerprint(args.model), "history_sha256": fingerprint(args.history)}
    save_json(args.output, model)
    save_json(args.report, report)


if __name__ == "__main__":
    main_guard(main)

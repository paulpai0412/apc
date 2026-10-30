"""Dataset-derived process design must not infer safety approval or use test statistics."""
import copy
import unittest

import numpy as np

from research import defaults, validate_study
from research_design import history_draft, match_history, study_splits, validate_engineering


class ResearchDesignTests(unittest.TestCase):
    def history(self, roles=(2, 1, 0)):
        count, dt = 900, 2.0
        t = np.arange(count)
        history = {"sample_time": dt, "time_seconds": (t * dt).tolist(), "usable": True,
                   "signals": [], "provenance": {"time_origin": "2026-01-01T08:00:00+08:00",
                   "cleaning": {"train_end": 600.0, "validation_end": 1200.0}}}
        for role, width in zip(("cv", "mv", "dv"), roles):
            history[role] = np.array([10 + 2 * j + np.sin(t / (7 + j)) for j in range(width)]).T.tolist() if width else [[] for _ in t]
            history["signals"].extend({"name": f"{role}{j}", "role": role, "unit": "unit"} for j in range(width))
        return history

    def draft(self, history):
        return history_draft({"id": "ds_source", "name": "窯爐資料"}, history, defaults())

    def test_training_only_suggestions_require_explicit_engineering_approval(self):
        history = self.history()
        original = self.draft(history)
        changed = copy.deepcopy(history)
        changed["cv"][600:] = [[10000, -10000] for _ in range(300)]
        changed["mv"][600:] = [[10000] for _ in range(300)]
        self.assertEqual(self.draft(changed), original)
        with self.assertRaises(ValueError):
            validate_study(original)
        original["config"]["engineering_confirmed"] = True
        validated = validate_study(original)
        self.assertNotIn("plant", validated["config"])
        self.assertNotIn("scenario", validated["config"])

    def test_mapped_dimensions_and_non_calendar_splits_are_preserved(self):
        for roles in ((1, 1, 0), (3, 2, 2)):
            with self.subTest(roles=roles):
                history = self.history(roles)
                draft = self.draft(history)
                draft["config"]["engineering_confirmed"] = True
                validate_study(draft)
                splits = match_history(draft["config"], history)
                self.assertEqual([(s["first"], s["last"]) for s in splits], [(0, 300), (300, 600), (600, 900)])
                self.assertEqual(splits[0]["start"], "2026-01-01T00:00:00+00:00")
                self.assertEqual([v["name"] for role in ("cv", "mv", "dv") for v in draft["config"]["variables"][role]], [v["name"] for v in history["signals"]])
                altered = copy.deepcopy(history)
                altered["signals"][0]["unit"] = "different unit"
                with self.assertRaises(ValueError):
                    match_history(draft["config"], altered)

    def test_clipping_or_reusing_a_future_cleaning_split_is_rejected(self):
        history = self.history()
        config = self.draft(history)["config"]
        config["data_window"]["train_end"] = 250
        with self.assertRaises(ValueError):
            match_history(config, history)
        config["data_window"]["train_end"] = 0
        with self.assertRaises(ValueError):
            study_splits(config)

    def test_relative_time_is_explicit_and_invalid_absolute_origins_are_rejected(self):
        config = self.draft(self.history())["config"]
        config["start"] = "relative"
        self.assertEqual([(s["start"], s["end"]) for s in study_splits(config)],
                         [("t+0s", "t+600s"), ("t+600s", "t+1200s"), ("t+1200s", "t+1800s")])
        for origin in ("2026-01-01T00:00:00", "not-a-date"):
            with self.subTest(origin=origin):
                config["start"] = origin
                with self.assertRaises(ValueError):
                    study_splits(config)

    def test_cleaned_elapsed_origin_is_not_interpreted_as_a_calendar_date(self):
        for source_origin in (0.0, 120.5, "120.5"):
            with self.subTest(source_origin=source_origin):
                history = self.history()
                history["provenance"]["time_origin"] = source_origin
                history["provenance"]["cleaning"]["time_format"] = "seconds"
                draft = self.draft(history)
                self.assertEqual(draft["config"]["start"], "relative")
                self.assertEqual(study_splits(draft["config"])[0]["start"], "t+0s")

    def test_control_coordinate_and_engineering_boundaries_are_not_guessed(self):
        config = self.draft(self.history())["config"]
        config["engineering_confirmed"] = True
        for field, value in (("mv_coordinate", "actuator_output"), ("sample_time", False)):
            altered = copy.deepcopy(config)
            altered[field] = value
            with self.assertRaises(ValueError):
                validate_engineering(altered)
        config["variables"]["cv"][0]["is_integrating"] = True
        with self.assertRaises(ValueError):
            validate_engineering(config)


if __name__ == "__main__":
    unittest.main()

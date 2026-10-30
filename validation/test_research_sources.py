"""Archive-contract fixtures only: these tests never claim native execution evidence."""
import copy
from pathlib import Path
import tempfile
import unittest

from common import fingerprint, load_json
from datasets import execute as datasets
from research import atomic_json
from research_sources import import_run_history, qualify_generated_history, verify_generated_source


class GeneratedSourceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.directory = self.root / "runs" / "archive-contract-fixture"
        self.directory.mkdir(parents=True)
        history = {"sample_time": 1, "time_seconds": list(range(6)), "cv": [[0], [0], [1], [1], [2], [2]],
                   "mv": [[99]] * 6, "command_mv": [[0], [1], [1], [2], [2], [3]], "dv": [[]] * 6}
        config = {"plant_source": "independent_synthetic", "start": "2026-01-01T00:00:00Z",
                  "variables": {"cv": [{"name": "quality", "unit": "1"}], "mv": [{"name": "feed", "unit": "t/h"}], "dv": []}}
        study = {"config": config}
        atomic_json(self.directory / "study.json", study)
        atomic_json(self.directory / "history.json", history)
        atomic_json(self.directory / "scenario.json", {"time_seconds": history["time_seconds"], "mv": history["command_mv"]})
        atomic_json(self.directory / "plant.json", {"fixture": "archive validation only; not a native PlantSpec"})
        atomic_json(self.directory / "run.json", {"id": self.directory.name, "name": "Archive-contract fixture", "status": "completed", "study": study,
                    "provenance": {"engine": "0" * 64, "study": fingerprint(self.directory / "study.json")}})
        atomic_json(self.directory / "history.command.json", {"returncode": 0, "controller_sha256": None,
                    "argv": ["archive-contract-fixture-not-an-executable", "replay"], "engine_sha256": "0" * 64,
                    "output_sha256": fingerprint(self.directory / "history.json"),
                    "plant_sha256": fingerprint(self.directory / "plant.json"), "scenario_sha256": fingerprint(self.directory / "scenario.json")})

    def test_import_uses_verified_command_coordinate_not_actuator_telemetry(self):
        raw = import_run_history(self.root, {"id": self.directory.name})
        self.assertEqual(raw["source"]["kind"], "generated_synthetic")
        clean = datasets(self.root, "clean", {"dataset_id": raw["id"], "cleaning": {
            "time_column": "time_seconds", "time_format": "seconds", "sample_time": 1, "train_end": 2, "validation_end": 4,
            "duplicate_policy": "first", "missing_policy": "reject", "max_gap_seconds": 0, "outlier_policy": "none", "signals": raw["suggested_signals"]}})
        history = load_json(self.root / "datasets" / clean["dataset"]["id"] / "history.json")
        self.assertEqual(history["mv"][1], [1])
        self.assertTrue(qualify_generated_history(self.root, history)["qualified"])
        self.assertFalse(qualify_generated_history(self.root, history)["physical_authority"])
        changed = copy.deepcopy(history)
        changed["signals"][0]["role"] = "mv"
        with self.assertRaisesRegex(ValueError, "roles"):
            qualify_generated_history(self.root, changed)
        changed = copy.deepcopy(history)
        changed["cv"][2][0] += 0.1
        with self.assertRaisesRegex(ValueError, "unmodified"):
            qualify_generated_history(self.root, changed)

    def test_changed_replay_bytes_and_closed_loop_receipts_cannot_qualify(self):
        raw = import_run_history(self.root, {"id": self.directory.name})
        receipt = load_json(self.directory / "history.command.json")
        receipt["argv"][1] = "closed-loop"
        atomic_json(self.directory / "history.command.json", receipt)
        with self.assertRaisesRegex(ValueError, "changed"):
            verify_generated_source(self.root, raw["source"])
        source = dict(raw["source"], receipt_sha256=fingerprint(self.directory / "history.command.json"))
        with self.assertRaisesRegex(ValueError, "open-loop replay"):
            verify_generated_source(self.root, source)


if __name__ == "__main__":
    unittest.main()

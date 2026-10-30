import copy
import json
import tempfile
import unittest
from pathlib import Path

from datasets import execute, list_datasets


class DatasetTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.cleaning = {
            "time_column": "time", "time_format": "seconds", "sample_time": 1,
            "train_end": 3, "validation_end": 5, "duplicate_policy": "last",
            "missing_policy": "reject", "max_gap_seconds": 2,
            "outlier_policy": "none", "sigma": 3,
            "signals": [
                {"column": "y", "name": "quality", "role": "cv", "unit": "C", "scale": 1, "offset": 0},
                {"column": "u", "name": "feed", "role": "mv", "unit": "%", "scale": 1, "offset": 0},
            ],
        }

    def import_csv(self, rows, header="time,y,u"):
        return execute(self.root, "import", {"name": "local measurements", "format": "csv",
                                            "content": header + "\n" + rows + "\n"})

    def clean(self, dataset, operation="preview", cleaning=None):
        return execute(self.root, operation, {"dataset_id": dataset["id"],
                                              "cleaning": cleaning or self.cleaning})

    def test_real_conversion_sorting_and_immutable_materialization(self):
        raw = self.import_csv("5,50,5\n0,0,0\n2,20,2\n1,10,1\n4,40,4\n3,30,3")
        source_path = self.root / "datasets" / raw["id"] / "source.txt"
        original = source_path.read_bytes()
        self.cleaning["signals"][0].update(scale=2, offset=10, unit="converted")
        preview = self.clean(raw)
        self.assertTrue(preview["usable"])
        self.assertEqual([p["clean"]["quality"] for p in preview["series"]], [10, 30, 50, 70, 90, 110])
        first = self.clean(raw, "clean")
        second = self.clean(raw, "clean")
        self.assertNotEqual(first["dataset"]["id"], second["dataset"]["id"])
        history = json.loads((self.root / "datasets" / first["dataset"]["id"] / "history.json").read_text())
        self.assertEqual(history["cv"], [[10], [30], [50], [70], [90], [110]])
        self.assertEqual(history["time_seconds"], [0, 1, 2, 3, 4, 5])
        self.assertEqual(history["signals"][0]["unit"], "converted")
        self.assertEqual(source_path.read_bytes(), original)
        self.assertEqual(len(list_datasets(self.root)), 3)
        self.assertEqual(first["summary"]["source_sha256"], raw["source_sha256"])

    def test_clean_save_list_preserves_exact_signal_sources_and_quality(self):
        for encoding, good, bad in (("da", "192", "0"), ("ua", "0", "2147483648"),
                                    ("boolean", "true", "false")):
            with self.subTest(encoding=encoding):
                mappings = [
                    {"column": column, "name": name, "role": role, "unit": unit,
                     "scale": 1, "offset": 0, "quality_column": "quality", "quality_format": encoding}
                    for column, name, role, unit in (
                        ("mill_kw", "power", "cv", "kW"),
                        ("mill_dp", "pressure", "cv", "kPa"),
                        ("feed_sp", "feed", "mv", "t/h"),
                        ("separator_sp", "separator", "mv", "rpm"),
                        ("clinker_pct", "clinker", "dv", "%"),
                        ("moisture_pct", "moisture", "dv", "%"),
                    )
                ]
                content = "time," + ",".join(item["column"] for item in mappings) + ",quality\r\n"
                content += "\r\n".join(
                    f"{i}," + ",".join(str(i + index) for index in range(6)) + f",{bad if i == 2 else good}"
                    for i in (0, 1, 1, 2, 3, 4, 5)
                ) + "\r\n"
                raw = execute(self.root, "import", {"name": encoding, "format": "csv", "content": content})
                cleaning = dict(self.cleaning, signals=copy.deepcopy(mappings),
                                missing_policy="forward_fill", max_gap_seconds=120)
                preview = self.clean(raw, cleaning=cleaning)
                saved = self.clean(raw, "clean", cleaning)
                self.assertTrue(saved["usable"])
                self.assertEqual(saved["summary"]["source_row_count"], 7)
                self.assertEqual(saved["summary"]["row_count"], 6)
                self.assertEqual(preview["summary"], saved["summary"])
                self.assertEqual(saved["dataset"]["suggested_signals"], mappings)
                listed = {item["id"]: item for item in execute(self.root, "list", {})["datasets"]}
                selected = listed[saved["dataset"]["id"]]
                for signal in saved["signals"]:
                    mapping = next(item for item in selected["suggested_signals"]
                                   if item["name"] == signal["name"] and item["role"] == signal["role"])
                    self.assertEqual(mapping, next(item for item in mappings if item["name"] == signal["name"]))
                self.assertEqual(selected["source_id"], raw["id"])
                self.assertEqual(selected["source_sha256"], raw["source_sha256"])
                directory = self.root / "datasets" / selected["id"]
                originals = {name: (directory / name).read_bytes()
                             for name in ("summary.json", "history.json", "cleaning.json", "preview.json")}
                history = json.loads(originals["history.json"])
                self.assertEqual(history["provenance"]["cleaning"]["signals"], mappings)
                self.assertEqual(json.loads(originals["cleaning.json"])["signals"], mappings)
                cleaning["signals"][0].update(name="revised", column="mill_dp", unit="revised",
                                              quality_column=None, quality_format="boolean")
                cleaning["signals"][1]["column"] = "mill_kw"
                revised = self.clean(raw, "clean", cleaning)
                self.assertNotEqual(revised["dataset"]["id"], selected["id"])
                self.assertEqual({item["id"]: item for item in execute(self.root, "list", {})["datasets"]}
                                 [selected["id"]], selected)
                self.assertEqual({name: (directory / name).read_bytes() for name in originals}, originals)
                self.assertEqual((self.root / "datasets" / raw["id"] / "source.txt").read_bytes(),
                                 content.encode("utf-8"))

    def test_list_keeps_legacy_missing_source_mappings_unknown(self):
        legacy = {
            "id": "ds_" + "0" * 32, "name": "legacy", "kind": "clean", "format": "history",
            "created_at": "2026-01-01T00:00:00+00:00", "row_count": 6,
            "columns": ["time_seconds", "quality", "feed"],
            "signals": [{"name": "quality", "role": "cv", "unit": "C"},
                        {"name": "feed", "role": "mv", "unit": "%"}],
        }
        directory = self.root / "datasets" / legacy["id"]
        directory.mkdir(parents=True)
        summary_path = directory / "summary.json"
        summary_path.write_text(json.dumps(legacy), encoding="utf-8")
        original = summary_path.read_bytes()
        self.assertEqual(execute(self.root, "list", {})["datasets"], [legacy])
        self.assertNotIn("suggested_signals", list_datasets(self.root)[0])
        self.assertEqual(summary_path.read_bytes(), original)

    def test_interpolation_and_forward_fill_never_cross_splits(self):
        raw = self.import_csv("0,0,1\n1,,1\n2,2,1\n3,,1\n4,40,1\n5,,1\n6,60,1")
        self.cleaning.update(missing_policy="linear", max_gap_seconds=100)
        result = self.clean(raw, "clean")
        self.assertEqual([p["clean"]["quality"] for p in result["series"]], [0, 1, 2, None, 40, None, 60])
        self.assertFalse(result["usable"])
        history = json.loads((self.root / "datasets" / result["dataset"]["id"] / "history.json").read_text())
        self.assertEqual(history["cv"], [[0], [1], [2], [None], [40], [None], [60]])
        self.assertEqual(history["valid_rows"], [True, True, True, False, True, False, True])
        self.cleaning["missing_policy"] = "forward_fill"
        self.assertEqual([p["clean"]["quality"] for p in self.clean(raw)["series"]], [0, 0, 2, None, 40, None, 60])
        changed = self.import_csv("0,0,1\n1,,1\n2,2,1\n3,,1\n4,4000,1\n5,,1\n6,6000,1")
        self.cleaning["missing_policy"] = "linear"
        self.assertEqual(self.clean(changed)["series"][:3], result["series"][:3])

    def test_training_sigma_never_uses_validation_or_test_statistics(self):
        self.cleaning.update(outlier_policy="train_sigma", sigma=2)
        raw = self.import_csv("0,0,1\n1,1,1\n2,2,1\n3,1000,1\n4,1,1\n5,-1000,1")
        preview = self.clean(raw)
        stats = preview["summary"]["signal_stats"][0]
        self.assertEqual(stats["training"]["count"], 3)
        self.assertEqual(stats["training"]["mean"], 1)
        self.assertAlmostEqual(stats["training"]["std"], (2 / 3) ** 0.5)
        self.assertEqual(stats["outlier_count"], 2)
        self.assertEqual([p["clean"]["quality"] for p in preview["series"]], [0, 1, 2, None, 1, None])
        changed = self.import_csv("0,0,1\n1,1,1\n2,2,1\n3,999999,1\n4,1,1\n5,-999999,1")
        self.assertEqual(self.clean(changed)["summary"]["signal_stats"][0]["training"], stats["training"])

    def test_long_gaps_keep_the_entire_uniform_timeline(self):
        raw = self.import_csv("0,0,1\n1,1,1\n7,7,1\n8,8,1\n9,9,1")
        self.cleaning.update(train_end=8, validation_end=9, missing_policy="linear", max_gap_seconds=2)
        result = self.clean(raw, "clean")
        self.assertFalse(result["dataset"]["usable"])
        history = json.loads((self.root / "datasets" / result["dataset"]["id"] / "history.json").read_text())
        self.assertEqual(history["time_seconds"], list(range(10)))
        self.assertEqual(history["cv"], [[0], [1], [None], [None], [None], [None], [None], [7], [8], [9]])
        self.assertEqual(result["summary"]["signal_stats"][0]["unresolved_count"], 5)
        self.cleaning["missing_policy"] = "forward_fill"
        self.assertEqual([p["clean"]["quality"] for p in self.clean(raw)["series"]], [0, 1, 1, 1, None, None, None, 7, 8, 9])

    def test_da_quality_words_include_numeric_csv_and_null_quality(self):
        raw = self.import_csv("0,0,1,192\n1,1,1,0xC0\n2,2,1,216\n3,3,1,64\n4,4,1,\n5,5,1,192.0", "time,y,u,q")
        self.cleaning["signals"][0]["quality_column"] = "q"
        preview = self.clean(raw)
        self.assertEqual([p["raw"]["quality"] for p in preview["series"]], [0, 1, 2, 3, 4, 5])
        self.assertEqual([p["clean"]["quality"] for p in preview["series"]], [0, 1, 2, None, None, 5])
        self.assertEqual(preview["summary"]["signal_stats"][0]["bad_quality_count"], 2)
        self.assertFalse(preview["usable"])

    def test_explicit_ua_and_boolean_quality_encodings(self):
        cases = [
            ("ua", ["0", "0x00960000", "2147483648", "1073741824", "4294967296", "0"],
             [0, 1, None, None, None, 5]),
            ("boolean", ["true", "false", "1", "0", "unknown", "t"],
             [0, None, 2, None, None, 5]),
        ]
        for encoding, statuses, expected in cases:
            with self.subTest(encoding=encoding):
                raw = self.import_csv("\n".join(f"{i},{i},1,{quality}" for i, quality in enumerate(statuses)),
                                      "time,y,u,q")
                self.cleaning["signals"][0].update(quality_column="q", quality_format=encoding)
                preview = self.clean(raw)
                self.assertEqual([point["clean"]["quality"] for point in preview["series"]], expected)

    def test_duplicate_policies_are_stable_and_mean_filters_bad_quality(self):
        raw = self.import_csv("0,0,1,192\n1,2,1,192\n1,4,1,0\n1,8,1,192\n2,2,1,192\n3,3,1,192\n4,4,1,192\n5,5,1,192", "time,y,u,q")
        self.cleaning["signals"][0]["quality_column"] = "q"
        for policy, expected in (("first", 2), ("last", 8), ("mean", 5)):
            with self.subTest(policy=policy):
                self.cleaning["duplicate_policy"] = policy
                preview = self.clean(raw)
                self.assertEqual(preview["series"][1]["clean"]["quality"], expected)
                self.assertEqual(preview["summary"]["duplicate_count"], 2)
        self.assertAlmostEqual(preview["series"][1]["raw"]["quality"], 14 / 3)

    def test_bounds_apply_after_unit_conversion(self):
        raw = self.import_csv("0,0,1\n1,1,1\n2,2,1\n3,3,1\n4,4,1\n5,5,1")
        self.cleaning["outlier_policy"] = "bounds"
        self.cleaning["signals"][0].update(scale=10, offset=100, min=100, max=130)
        self.cleaning["signals"][1].update(min=0, max=100)
        preview = self.clean(raw)
        self.assertEqual([p["clean"]["quality"] for p in preview["series"]], [100, 110, 120, 130, None, None])
        self.assertEqual(preview["summary"]["signal_stats"][0]["outlier_count"], 2)

    def test_resampling_uses_past_observation_not_nearest_future(self):
        raw = self.import_csv("0,0,1\n0.9,9,1\n1.1,11,1\n2,20,1\n3,30,1\n4,40,1\n5,50,1")
        preview = self.clean(raw)
        self.assertEqual([p["clean"]["quality"] for p in preview["series"]], [0, 9, 20, 30, 40, 50])
        self.assertEqual(preview["summary"]["row_count"], 6)
        self.assertEqual(preview["summary"]["source_row_count"], 7)

    def test_canonical_history_import_preserves_signal_metadata_and_nulls(self):
        history = {"sample_time": 1, "time_seconds": [100, 101, 102, 103, 104, 105],
                   "cv": [[1], [None], [3], [4], [5], [6]], "mv": [[10]] * 6, "dv": [[]] * 6,
                   "signals": [{"name": "temperature", "role": "cv", "unit": "C"},
                               {"name": "command", "role": "mv", "unit": "%"}]}
        raw = execute(self.root, "import", {"name": "canonical", "format": "history", "content": json.dumps(history)})
        self.assertEqual(raw["columns"], ["time_seconds", "temperature", "command"])
        cleaning = copy.deepcopy(self.cleaning)
        cleaning.update(time_column="time_seconds", signals=raw["suggested_signals"])
        result = self.clean(raw, "clean", cleaning)
        output = json.loads((self.root / "datasets" / result["dataset"]["id"] / "history.json").read_text())
        self.assertEqual(output["cv"], history["cv"])
        self.assertEqual(output["mv"], history["mv"])
        self.assertEqual(output["provenance"]["time_origin"], 100)
        self.assertFalse(output["usable"])

    def test_iso_timezones_crlf_and_original_source_hash(self):
        content = "time,y,u\r\n" + "\r\n".join(f"2026-01-01T00:00:0{i}+08:00,{i},1" for i in range(6)) + "\r\n"
        raw = execute(self.root, "import", {"name": "ISO", "format": "csv", "content": content})
        self.cleaning["time_format"] = "iso"
        result = self.clean(raw)
        self.assertTrue(result["usable"])
        self.assertEqual([p["time"] for p in result["series"]], list(range(6)))
        self.assertEqual(result["summary"]["source_sha256"], raw["source_sha256"])

    def test_ids_split_and_resampling_limits_fail_actionably(self):
        for identifier in ("../outside", "/etc/passwd", "", None):
            with self.subTest(identifier=identifier), self.assertRaisesRegex(ValueError, "server-generated"):
                execute(self.root, "preview", {"dataset_id": identifier, "cleaning": self.cleaning})
        raw = self.import_csv("0,0,1\n1,1,1\n2,2,1\n3,3,1\n4,4,1\n5,5,1")
        for change, message in (({"train_end": 5, "validation_end": 3}, "train_end"),
                                ({"validation_end": 10}, "split"),
                                ({"sample_time": 0.000001}, "500000")):
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, message):
                self.clean(raw, cleaning=dict(self.cleaning, **change))
        cleaned = self.clean(raw, "clean")
        with self.assertRaisesRegex(ValueError, "original raw"):
            self.clean(cleaned["dataset"])

    def test_decimated_graph_keeps_gap_and_spike_full_history_unchanged(self):
        rows = "\n".join(f"{i},{'' if i == 2111 else 999 if i == 2222 else 1},1" for i in range(6000))
        raw = self.import_csv(rows)
        self.cleaning.update(train_end=3000, validation_end=4500)
        result = self.clean(raw, "clean")
        self.assertTrue(result["display"]["decimated"])
        self.assertLessEqual(len(result["series"]), 2000)
        by_time = {point["time"]: point for point in result["series"]}
        self.assertIsNone(by_time[2111]["clean"]["quality"])
        self.assertEqual(by_time[2222]["clean"]["quality"], 999)
        history = json.loads((self.root / "datasets" / result["dataset"]["id"] / "history.json").read_text())
        self.assertEqual(len(history["cv"]), 6000)
        self.assertEqual(history["time_seconds"][-1], 5999)

    def test_first_cleaning_change_survives_display_reduction_with_context(self):
        rows = "\n".join(f"{i},{i},1,{0 if i == 120 else 192}" for i in range(6000))
        raw = self.import_csv(rows, "time,y,u,q")
        self.cleaning.update(train_end=3000, validation_end=4500, missing_policy="forward_fill")
        self.cleaning["signals"][0]["quality_column"] = "q"
        result = self.clean(raw)
        changed = [point for point in result["series"] if point["raw"]["quality"] != point["clean"]["quality"]]
        self.assertEqual(changed[0]["time"], 120)
        self.assertEqual(changed[0]["clean"]["quality"], 119)
        self.assertTrue({119, 120, 121}.issubset({point["time"] for point in result["series"]}))
        self.assertLessEqual(len(result["series"]), 2000)

    def test_import_rejects_ambiguous_csv_and_preview_rejects_naive_iso(self):
        for content, message in (("time,y,y\n0,1,2\n1,2,3", "unique"),
                                 ("time,y,u\n0,1\n1,2,3", "fields"),
                                 ("time,y,u\n0,1,2", "two data rows")):
            with self.subTest(content=content), self.assertRaisesRegex(ValueError, message):
                execute(self.root, "import", {"name": "invalid", "format": "csv", "content": content})
        raw = self.import_csv("\n".join(f"2026-01-01T00:00:0{i},{i},1" for i in range(6)))
        self.cleaning["time_format"] = "iso"
        with self.assertRaisesRegex(ValueError, "timezone"):
            self.clean(raw)

    def test_database_snapshot_rejects_inconsistent_or_secret_provenance(self):
        source = {
            "kind": "postgres", "source_id": "mill_history", "schema": "public",
            "table": "measurements", "timestamp_column": "time",
            "columns": ["time", "y", "u"], "row_count": 6,
            "start": "2026-01-01T00:00:00Z", "end": "2026-01-01T00:00:06Z",
            "read_at": "2026-01-02T00:00:00Z",
        }
        content = "time,y,u\n" + "\n".join(f"2026-01-01T00:00:0{i}Z,{i},1" for i in range(6))
        for change in ({"row_count": 5}, {"columns": ["time", "u", "y"]},
                       {"password": "must-not-be-persisted"}, {"start": "2026-01-01T00:00:00"}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                execute(self.root, "import", {"name": "database snapshot", "format": "csv",
                                              "content": content, "source": dict(source, **change)})
        self.assertEqual(list_datasets(self.root), [])

    def test_sigma_rejects_insufficient_good_training_data(self):
        raw = self.import_csv("0,,1\n1,1,1\n2,,1\n3,3,1\n4,4,1\n5,5,1")
        self.cleaning["outlier_policy"] = "train_sigma"
        with self.assertRaisesRegex(ValueError, "two good training"):
            self.clean(raw)




if __name__ == "__main__":
    unittest.main()

import tempfile
import unittest
from pathlib import Path

from import_history import import_csv


class CsvImportTests(unittest.TestCase):
    def setUp(self):
        self.model = {"tuning": {"sample_time": 1}, "variables": {
            "cvs": [{"name": "quality"}], "mvs": [{"name": "feed"}], "dvs": []}}
        self.mapping = {"timestamp": {"column": "time", "format": "seconds"},
                        "sample_time": 1, "encoding": "gbk", "cv": ["质量"], "mv": ["feed"], "dv": [],
                        "provenance": {"source_url": "synthetic://unit-test", "license": "CC0-1.0", "synthetic": True}}

    def run_import(self, text):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "data.csv"
            path.write_text(text, encoding="gbk")
            return import_csv(path, self.mapping, self.model)

    def test_explicit_encoding_and_order(self):
        result = self.run_import("time,质量,feed\n10,2,3\n11,4,5\n")
        self.assertEqual(result["time_seconds"], [10, 11])
        self.assertEqual(result["cv"], [[2], [4]])
        self.assertTrue(result["provenance"]["synthetic"])

    def test_missing_duplicate_gap_and_nonfinite_rejected(self):
        for rows in ("0,1,2\n0,2,3", "0,1,2\n2,2,3", "0,,2\n1,2,3", "0,nan,2\n1,2,3"):
            with self.subTest(rows=rows), self.assertRaises(ValueError):
                self.run_import("time,质量,feed\n" + rows + "\n")

    def test_duplicate_headers_rejected(self):
        with self.assertRaises(ValueError):
            self.run_import("time,质量,feed,feed\n0,1,2,2\n1,2,3,3\n")

    def test_naive_iso_timestamp_rejected(self):
        self.mapping["timestamp"]["format"] = "iso8601"
        with self.assertRaises(ValueError):
            self.run_import("time,质量,feed\n2025-01-01T00:00:00,1,2\n2025-01-01T00:00:01,2,3\n")

    def test_subsecond_iso_grid_preserves_interval_precision(self):
        self.mapping["timestamp"]["format"] = "iso8601"
        self.mapping["sample_time"] = self.model["tuning"]["sample_time"] = 0.1
        history = self.run_import(
            "time,质量,feed\n2025-01-01T00:00:00Z,1,2\n"
            "2025-01-01T00:00:00.1Z,2,3\n2025-01-01T00:00:00.2Z,3,4\n")
        self.assertEqual(history["time_seconds"], [0.0, 0.1, 0.2])
        self.assertEqual(history["provenance"]["time_origin_iso8601"], "2025-01-01T00:00:00+00:00")


if __name__ == "__main__":
    unittest.main()

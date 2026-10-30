"""Quality and temporal-boundary regressions; no network access."""
import sqlite3
import unittest

from export_questdb import convert, query, timestamp


class ExportHistoryTests(unittest.TestCase):
    def setUp(self):
        self.mapping = {"sample_time": 10, "cv": ["CV:PV"], "mv": ["MV:AppliedSP"], "dv": []}
        self.start = timestamp("2024-01-01T00:00:00Z")
        self.end = timestamp("2024-01-01T00:00:30Z")
        self.response = {
            "columns": [{"name": name} for name in ["timestamp", "node_id", "value", "status"]],
            "dataset": [
                ["2024-01-01T00:00:00Z", "CV:PV", 5, 0],
                ["2024-01-01T00:00:00Z", "MV:AppliedSP", 10, 0],
                ["2024-01-01T00:00:11Z", "CV:PV", 8, 0],
                ["2024-01-01T00:00:11Z", "MV:AppliedSP", 20, 0],
            ],
        }

    def export(self):
        return convert(self.response, self.mapping, self.start, self.end, 15)

    def test_hold_is_causal_not_nearest_or_future_value(self):
        history = self.export()
        self.assertEqual(history["time_seconds"], [0, 10, 20])
        self.assertEqual(history["cv"], [[5], [5], [8]])
        self.assertEqual(history["mv"], [[10], [10], [20]])
        self.assertEqual(history["dv"], [[], [], []])

    def test_stale_measurement_rejected(self):
        self.response["dataset"] = self.response["dataset"][:2]
        with self.assertRaisesRegex(ValueError, "stale"):
            self.export()

    def test_no_backfill_before_first_measurement(self):
        self.response["dataset"][0][0] = "2024-01-01T00:00:01Z"
        with self.assertRaisesRegex(ValueError, "missing/stale CV:PV"):
            self.export()

    def test_bad_quality_between_grid_points_not_hidden(self):
        self.response["dataset"].append(["2024-01-01T00:00:01Z", "CV:PV", 6, 2147483648])
        with self.assertRaisesRegex(ValueError, "non-Good"):
            self.export()

    def test_duplicate_timestamp_is_ambiguous(self):
        self.response["dataset"].append(["2024-01-01T00:00:00Z", "CV:PV", 999, 0])
        with self.assertRaisesRegex(ValueError, "duplicate timestamp"):
            self.export()

    def test_nonfinite_value_rejected(self):
        self.response["dataset"][0][2] = float("nan")
        with self.assertRaisesRegex(ValueError, "finite"):
            self.export()

    def test_truncated_response_rejected(self):
        self.response["count"] = 100
        with self.assertRaisesRegex(ValueError, "truncated"):
            self.export()

    def test_sample_interval_must_fit_window(self):
        self.mapping["sample_time"] = 11
        with self.assertRaisesRegex(ValueError, "integer number"):
            self.export()

    def test_exact_node_identity_survives_punctuation_and_reserved_prefixes(self):
        self.mapping = {"sample_time": 10, "cv": ["PV", "unknown:PV"], "mv": ["Mill,FeedSP"],
                        "dv": ["_Hardness", "System:Moisture:PV"]}
        nodes = self.mapping["cv"] + self.mapping["mv"] + self.mapping["dv"]
        self.response["dataset"] = [
            [instant, node, index * 10 + offset + 0.123456789, 0]
            for instant, offset in [("2024-01-01T00:00:00Z", 1), ("2024-01-01T00:00:11Z", 2)]
            for index, node in enumerate(nodes)
        ]
        history = self.export()
        self.assertEqual(history["cv"], [[1.123456789, 11.123456789], [1.123456789, 11.123456789], [2.123456789, 12.123456789]])
        self.assertEqual(history["mv"], [[21.123456789], [21.123456789], [22.123456789]])
        self.assertEqual(history["dv"], [[31.123456789, 41.123456789], [31.123456789, 41.123456789], [32.123456789, 42.123456789]])

    def test_legacy_split_identity_cannot_be_inferred(self):
        self.response["columns"] = [{"name": name} for name in ["timestamp", "tag", "field", "value", "status"]]
        self.response["dataset"] = [["2024-01-01T00:00:00Z", "unknown", "PV", 1, 0]]
        with self.assertRaises(ValueError):
            self.export()

    def test_query_preserves_identity_preroll_and_exclusive_end(self):
        self.mapping["cv"] = ["PV"]
        self.mapping["mv"] = ["Feed'O:SP"]
        with sqlite3.connect(":memory:") as database:
            self.addCleanup(database.close)
            database.execute("CREATE TABLE process_data(timestamp TEXT, node_id TEXT, value REAL, status INTEGER)")
            database.executemany("INSERT INTO process_data VALUES (?, ?, ?, ?)", [
                ("2023-12-31T23:59:44Z", "PV", -1, 0),
                ("2023-12-31T23:59:45Z", "PV", 1, 0),
                ("2024-01-01T00:00:00Z", "Feed'O:SP", 2, 0),
                ("2024-01-01T00:00:05Z", "unknown:PV", 999, 0),
                ("2024-01-01T00:00:10Z", "PV", 3, 0),
                ("2024-01-01T00:00:20Z", "PV", 4, 0),
                ("2024-01-01T00:00:30Z", "PV", 5, 0),
            ])
            rows = database.execute(query(self.mapping, "process_data", self.start, self.end, 15, 100)).fetchall()
            self.assertEqual([(row[1], row[2]) for row in rows], [("PV", 1), ("Feed'O:SP", 2), ("PV", 3), ("PV", 4)])
            limited = database.execute(query(self.mapping, "process_data", self.start, self.end, 15, 2)).fetchall()
            self.assertEqual(limited, rows[:3])
        with self.assertRaises(ValueError):
            query(self.mapping, "process_data; DROP TABLE process_data", self.start, self.end, 15, 100)


if __name__ == "__main__":
    unittest.main()

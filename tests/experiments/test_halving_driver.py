"""Deterministic protocol and statistics tests; native CLI tests cover the worker."""

import importlib.util
import pathlib
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "halving_driver", pathlib.Path(__file__).resolve().parents[2] / "scripts/bench-halving.py"
)
DRIVER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DRIVER)


class ProtocolTests(unittest.TestCase):
    def test_counterbalance_places_each_arm_in_each_position(self):
        orders = [DRIVER.arm_order(i) for i in range(4)]
        for position in range(4):
            self.assertEqual({row[position] for row in orders}, set(DRIVER.ARMS))
        pairs = [(row[i], row[i + 1]) for row in orders for i in range(3)]
        self.assertEqual(len(set(pairs)), 12)

    def test_uncertainty_clusters_repetitions_by_seed(self):
        # Duplicating timings within a seed must not create independent evidence.
        result = DRIVER.distribution({0: [1.0] * 8, 17: [3.0] * 8, 101: [5.0] * 8})
        self.assertEqual(result["seedMeans"], [1.0, 3.0, 5.0])
        self.assertEqual(result["mean"], 3.0)
        self.assertEqual(result["pairs"], 24)
        self.assertEqual(result["bootstrap95"], DRIVER.distribution({0: [1.0], 17: [3.0], 101: [5.0]})["bootstrap95"])

    def test_hashes_detect_same_length_edits_and_extra_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            (root / "a.bin").write_bytes(b"abcd")
            before = DRIVER.inventory(root, ".bin")
            self.assertEqual(before, DRIVER.inventory(root, ".bin"))
            (root / "a.bin").write_bytes(b"abce")
            self.assertNotEqual(before, DRIVER.inventory(root, ".bin"))
            (root / "a.bin").write_bytes(b"abcd")
            (root / "b.bin").write_bytes(b"abcd")
            self.assertNotEqual(before, DRIVER.inventory(root, ".bin"))

    def test_protocol_requires_separate_splits_and_balanced_repetitions(self):
        cases = [{"id": "c", "split": "calibration", "kind": "synthetic"},
                 {"id": "v", "split": "validation", "kind": "synthetic"}]
        config = {"schema": "neat-halving-config/1", "cases": cases,
                  "seeds": [0, 17, 101], "repetitions": 4, "cost": "MSE",
                  "splitJustification": "independent cohorts",
                  "limits": {"maxCorpusBytes": 1024, "maxRecordEvaluations": 10000,
                             "trialSeconds": 10, "totalSeconds": 120}}
        DRIVER.validate_config(config)
        with self.assertRaisesRegex(ValueError, "multiple of four"):
            DRIVER.validate_config(dict(config, repetitions=3))
        with self.assertRaisesRegex(ValueError, "calibration and validation"):
            DRIVER.validate_config(dict(config, cases=cases[:1]))
        with self.assertRaisesRegex(ValueError, "distinct seeds"):
            DRIVER.validate_config(dict(config, seeds=[0, 0, 17]))


if __name__ == "__main__":
    unittest.main()

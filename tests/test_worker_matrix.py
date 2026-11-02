"""Guard benchmark fairness and correctness rejection without scanning any files."""
from collections import Counter
from contextlib import redirect_stderr
import io
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

BENCHMARKS = Path(__file__).resolve().parents[1] / "benchmarks"
sys.path.insert(0, str(BENCHMARKS))
import scan_bench as bench
import worker_matrix as matrix
sys.path.pop(0)


class WorkerValidationTests(unittest.TestCase):
    def test_worker_range_includes_detected_cpu_and_core_endpoints(self):
        for value in ("1", "4", "8", "18", "64"):
            self.assertEqual(bench.worker_count(value), int(value))
        for value in ("0", "65", "-1", "1.5", "auto", "", "many"):
            with self.subTest(value=value), self.assertRaises(Exception):
                bench.worker_count(value)

    def test_both_pairwise_cli_worker_options_accept_cpu_counts(self):
        with patch.object(sys, "argv", ["scan_bench.py", "--binary", "owned-runner",
                                      "--workers", "18", "--reference-workers", "64"]):
            args = bench.parse_args()
        self.assertEqual((args.workers, args.reference_workers), (18, 64))

    def test_cli_rejects_out_of_range_without_starting_runner(self):
        for flag in ("--workers", "--reference-workers"):
            for value in ("0", "65"):
                with self.subTest(flag=flag, value=value), redirect_stderr(io.StringIO()), \
                     patch.object(sys, "argv", ["scan_bench.py", "--binary", "owned-runner", flag, value]), \
                     self.assertRaises(SystemExit):
                    bench.parse_args()


class MatrixFairnessTests(unittest.TestCase):
    def test_twelve_trials_balance_positions_and_each_directed_neighbour(self):
        labels = ("workers-4", "workers-8", "workers-cpu")
        for offset in range(6):
            orders = matrix.measured_orders(labels, 12, offset)
            self.assertEqual(len(orders), 12)
            self.assertEqual(set(Counter(orders).values()), {2})
            for position in range(3):
                self.assertEqual(Counter(order[position] for order in orders), dict.fromkeys(labels, 4))
            neighbours = Counter(pair for order in orders for pair in zip(order, order[1:]))
            self.assertEqual(len(neighbours), 6)
            self.assertEqual(set(neighbours.values()), {4})

    def test_unbalanced_trial_count_is_rejected(self):
        for count in (0, 5, 7, 9):
            with self.subTest(count=count), self.assertRaises(ValueError):
                matrix.measured_orders(("4", "8", "cpu"), count)

    def test_worker_clamping_and_inventory_fail_but_occupancy_drift_is_separate(self):
        expected = dict(nodes=3, files=2, directories=1, logical_bytes=9, expected_complete=True)
        run = dict(expected, complete=True, cancelled=False, scan_mode="async", scan_workers=18,
                   filesystem_permits=18, validation_digest="a" * 64, occupancy_digest="b" * 64)
        fingerprints = {}
        self.assertEqual(matrix.validate_run(run, expected, 18, fingerprints), [])
        self.assertEqual(matrix.validate_run(dict(run, occupancy_digest="c" * 64), expected, 18, fingerprints), [])
        drift = dict(run, scan_workers=8, filesystem_permits=8)
        errors = matrix.validate_run(drift, expected, 18, fingerprints)
        self.assertEqual(len(errors), 2)
        wrong_inventory = dict(run, files=1)
        self.assertTrue(matrix.validate_run(wrong_inventory, expected, 18, fingerprints))


if __name__ == "__main__":
    unittest.main()

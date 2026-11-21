"""Independent small-fixture checks for the synthetic review prototype."""
import sqlite3
import tempfile
import unittest
from pathlib import Path

from ironclad_banking import benchmark, claim, commit, inspect, setup


class BankingPrototype(unittest.TestCase):
    def test_three_partitions_and_exact_results(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "records.db"
            result = benchmark(path, 2001)
            self.assertTrue(result["ok"])
            self.assertEqual(result["partitions"], 3)
            self.assertEqual(result["reviewed"], 2001)
            self.assertEqual(result["wrong_masks"], 0)
            self.assertEqual(result["flag_counts"], {
                "missing_kyc": 20, "negative_balance_test_rule": 19,
                "duplicate_reference": 34,
            })

    def test_reject_existing_database(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "records.db"
            path.write_bytes(b"existing")
            with self.assertRaises(ValueError):
                benchmark(path, 1)
            self.assertEqual(path.read_bytes(), b"existing")

    def test_invalid_count_refused(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "records.db"
            for n in (0, -1, 1000001):
                with self.assertRaises(ValueError):
                    benchmark(path, n)
                self.assertFalse(path.exists())

    def test_wrong_owner_and_incomplete_partition_refused(self):
        with sqlite3.connect(":memory:") as db:
            setup(db, 10)
            job = claim(db, "owner", 10)
            rows = inspect(db, job)
            with self.assertRaises(PermissionError):
                commit(db, job, "other", 11, rows)
            with self.assertRaises(ValueError):
                commit(db, job, "owner", 11, rows[:-1])
            self.assertEqual(db.execute("SELECT COUNT(*) FROM review").fetchone()[0], 0)
            commit(db, job, "owner", 11, rows)
            self.assertEqual(db.execute("SELECT COUNT(*) FROM review").fetchone()[0], 10)

    def test_duplicate_pair_across_partition_boundary(self):
        # Craft a duplicate on opposite sides of a partition boundary.
        with sqlite3.connect(":memory:") as db:
            setup(db, 1001)
            db.execute("UPDATE customer SET customer_ref=1000 WHERE id=1001")
            db.commit()
            first = claim(db, "a", 0)
            rows = inspect(db, first)
            self.assertEqual(rows[-1][1] & 4, 4)
            commit(db, first, "a", 1, rows)
            second = claim(db, "a", 1)
            self.assertEqual(inspect(db, second)[0][1] & 4, 4)


if __name__ == "__main__":
    unittest.main()

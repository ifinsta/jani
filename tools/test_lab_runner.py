"""Offline contract tests for tools/lab_runner.py; never start Docker."""
import tempfile
import unittest
from pathlib import Path

import lab_runner as lab


class LabRunnerTests(unittest.TestCase):
    def test_signing_contract_is_domain_separated_and_length_prefixed(self):
        proof = {
            "version": lab.VERSION, "tenantId": "t",
            "sourceRunId": "r", "findingId": "mir-" + "a" * 24,
            "scenarioId": "case", "iteration": 0,
            "revisionSha256": "a" * 64, "suiteSha256": "b" * 64,
            "evaluatorSha256": "c" * 64, "environmentSha256": "d" * 64,
            "passed": 20, "failed": 0, "skipped": 0,
            "durationMs": 100, "completed": True,
        }
        wire = lab.signature_bytes(proof)
        self.assertTrue(wire.startswith(b"jani-lab/independent-trial/v1\0"))
        self.assertEqual(wire[30:38], len(lab.VERSION).to_bytes(8, "big"))
        changed = dict(proof, passed=19)
        self.assertNotEqual(wire, lab.signature_bytes(changed))

    def test_snapshot_is_reproducible_and_does_not_mount_env(self):
        with tempfile.TemporaryDirectory() as base:
            root = Path(base)
            source = root / "input"
            source.mkdir()
            (source / "Cargo.toml").write_text("[package]\nname='x'\n")
            (source / ".env").write_text("SECRET=must_not_copy")
            (source / ".git").mkdir()
            (source / ".git" / "config").write_text("private")
            digest_a = lab.copy_and_digest(source, root / "a")
            digest_b = lab.copy_and_digest(source, root / "b")
            self.assertEqual(digest_a, digest_b)
            self.assertFalse((root / "a" / ".env").exists())
            self.assertFalse((root / "a" / ".git").exists())
            self.assertEqual((root / "a" / "Cargo.toml").stat().st_mode & 0o444, 0o444)

    def test_source_symlinks_are_denied(self):
        with tempfile.TemporaryDirectory() as base:
            root = Path(base)
            source = root / "input"
            source.mkdir()
            (source / "Cargo.toml").write_text("cargo")
            (source / "leak").symlink_to(source / "Cargo.toml")
            with self.assertRaises(lab.TrialError):
                lab.copy_and_digest(source, root / "output")

    def test_test_summary_contract(self):
        transcript = "test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out"
        self.assertEqual(lab.RESULT.findall(transcript), [("ok", "20", "0", "0")])


if __name__ == "__main__":
    unittest.main()

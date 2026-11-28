"""Falsification controls for the local synthetic runtime runner."""

import base64
from contextlib import redirect_stderr
import gzip
import hashlib
from io import StringIO
import json
import os
from pathlib import Path
import socket
import sys
import tempfile
import unittest
from unittest.mock import patch
from urllib.parse import urlsplit

from synthetic.generate import case, case_line, generate
from synthetic.runtime_adapters import Plan, check_tool_events, choose, execute
from synthetic.runtime_harness import MAX_OUTPUT, CommandResult, RuntimeFixture, loopback_model
from synthetic.runtime_runner import main, run, run_case


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


class RuntimeOracleTests(unittest.TestCase):
    def test_admission_uses_actual_generator_rows(self):
        self.assertEqual(choose(case(12158)), (Plan("audit.corrupt_hash_chain", True), None))
        self.assertEqual(choose(case(376846)), (Plan("audit.corrupt_hash_chain", False), None))
        self.assertEqual(choose(case(11790)), (None, "PROFILE_NOT_IMPLEMENTED"))
        self.assertEqual(choose(case(23628)), (Plan("tool_broker.write_without_grant", False), None))
        self.assertEqual(choose(case(71100)), (Plan("tool_broker.write_without_grant", True), None))
        self.assertEqual(choose(case(1)), (None, "NO_RUNTIME_ADAPTER"))

    def test_exact_invocation_and_resource_are_required(self):
        result = {"status": "denied", "invocationId": "inv-1"}
        event = {"invocation_id": "inv-1", "action": "repo.write", "decision": "deny",
                 "reason_code": "operator_write_denied", "resource_digest": sha(b"repo:subject.txt")}
        args = ([result], "repo.write", "repo:subject.txt", "operator_write_denied")
        check_tool_events([event], *args)
        for field, value in (("invocation_id", "inv-2"), ("resource_digest", sha(b"repo:other")),
                             ("reason_code", "write_scope_denied")):
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "exact tool invocation"):
                check_tool_events([{**event, field: value}], *args)

    def test_failed_startup_cannot_count_as_the_expected_denial(self):
        row = case(23628)
        plan, _ = choose(row)
        with patch("synthetic.runtime_harness.RuntimeFixture.run", return_value=(
                CommandResult(1, b"", b"operator_write_denied", None), [])):
            result = run_case(Path(sys.executable), row, plan, 3)
        self.assertEqual(result["status"], "FAILED")
        self.assertIn("did not finish", result["reason"])

    def test_command_timeout_and_output_limit_are_failures(self):
        cases = [(0.2, "import time; time.sleep(30)", "timeout"),
                 (10, f"import os; os.write(1, b'x' * {MAX_OUTPUT + 1024})", "output limit")]
        for timeout, script, failure in cases:
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as root:
                fixture = RuntimeFixture(Path(sys.executable), Path(root), case(382), timeout)
                with self.assertRaisesRegex(ValueError, failure):
                    fixture.invoke("-c", script)
                attempt = fixture.commands[0][1]
                self.assertEqual(attempt.failure, failure)
                self.assertLessEqual(len(attempt.stdout), MAX_OUTPUT)

    def test_loopback_records_non_model_connections_and_closes_partial_requests(self):
        with loopback_model([]) as (endpoint, trace):
            target = urlsplit(endpoint)
            with socket.create_connection((target.hostname, target.port), timeout=2) as conn:
                conn.sendall(b"GET / HTTP/1.0\r\n\r\n")
                self.assertIn(b"501", conn.recv(1024))
            incomplete = socket.create_connection((target.hostname, target.port), timeout=2)
            self.addCleanup(incomplete.close)
            incomplete.sendall(b"POST /v1/chat/completions HTTP/1.0\r\n")
        self.assertEqual(trace.requests, [])
        self.assertEqual(trace.connections, 2)

    def test_compressed_digest_failure_is_checked_before_any_executable_call(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            corpus = root / "corpus"
            manifest = generate(corpus, count=382, shard_size=382, quiet=True)
            shard = corpus / manifest["shards"][0]["file"]
            raw = bytearray(shard.read_bytes())
            raw[4] ^= 1  # Alter only gzip metadata. The decompressed rows still match exactly.
            shard.write_bytes(raw)
            binary = root / "fake-binary"
            binary.write_bytes(b"would not be executable")
            output = root / "report"
            with patch("synthetic.runtime_runner.run_case") as execute_case:
                with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                    run(binary, sha(binary.read_bytes()), corpus, sha((corpus / "manifest.json").read_bytes()),
                        output, 382, 382, 1, 5)
                execute_case.assert_not_called()
            self.assertFalse((output / "summary.json").exists())

    def test_seal_contains_exact_row_and_report_digests(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            corpus = root / "corpus"
            generate(corpus, count=12158, shard_size=12158, quiet=True)
            binary = root / "unneeded"
            binary.write_bytes(b"fixture")
            output = root / "report"
            args = (binary, sha(binary.read_bytes()), corpus, sha((corpus / "manifest.json").read_bytes()),
                    output, 12157, 12158, 1, 5)
            with patch("synthetic.runtime_runner.run_case", return_value={"status": "FAILED"}) as execute_case:
                summary = run(*args)
                self.assertEqual(execute_case.call_count, 1)
                with self.assertRaisesRegex(ValueError, "already exists"):
                    run(*args)
                self.assertEqual(execute_case.call_count, 1)
            ledger = output / "observations.jsonl.gz"
            self.assertEqual(summary["observations_sha256"], sha(ledger.read_bytes()))
            self.assertEqual(summary, json.loads((output / "summary.json").read_bytes()))
            with gzip.open(ledger, "rt") as records:
                skipped, failed = map(json.loads, records)
            self.assertEqual(skipped["status"], "SKIPPED")
            self.assertEqual(failed["status"], "FAILED")
            self.assertEqual(failed["input_sha256"], sha(case_line(case(12158))))
            self.assertEqual(failed["execution"], "local-runtime")
            self.assertFalse(summary["production_ready"])

    def test_invalid_deflate_exits_as_invalid_input(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            corpus = root / "corpus"
            manifest = generate(corpus, count=1, shard_size=1, quiet=True)
            shard = corpus / manifest["shards"][0]["file"]
            raw = bytearray(shard.read_bytes())
            self.assertEqual(raw[3], 0)
            raw[10] = 7  # Reserved DEFLATE block type, with an intact gzip header.
            shard.write_bytes(raw)
            binary = root / "unused-binary"
            binary.write_bytes(b"would not be executable")
            output = root / "report"
            argv = ["runtime_runner", "--binary", str(binary), "--binary-sha256", sha(binary.read_bytes()),
                    "--corpus", str(corpus), "--manifest-sha256", sha((corpus / "manifest.json").read_bytes()),
                    "--output", str(output), "--first", "1", "--last", "1"]
            with patch.object(sys, "argv", argv), redirect_stderr(StringIO()) as diagnostic:
                with self.assertRaises(SystemExit) as error:
                    main()
            self.assertEqual(error.exception.code, 2)
            self.assertIn("invalid block type", diagnostic.getvalue())
            self.assertFalse((output / "summary.json").exists())

    @unittest.skipUnless(os.environ.get("JANI_LOCAL_RUNTIME_BIN"), "set JANI_LOCAL_RUNTIME_BIN to a built Jani CLI")
    def test_native_faults_and_positive_controls(self):
        binary = Path(os.environ["JANI_LOCAL_RUNTIME_BIN"]).resolve(strict=True)
        indices = (376846, 23589, 23612, 23635, 23628, 71100, 12156, 494604, 788999)
        for index in indices:
            row = case(index)
            with self.subTest(index=index, hook=row["replay"]["fault_hook"]):
                plan, reason = choose(row)
                self.assertIsNone(reason)
                result = run_case(binary, row, plan, 30)
                self.assertEqual(result["status"], "PASSED", result.get("reason"))
                self.assertEqual(result["observed_decision"], row["oracle"]["decision"])
                for command in result["trace"]["commands"]:
                    for stream in ("stdout", "stderr"):
                        self.assertEqual(sha(base64.b64decode(command[stream]["base64"])), command[stream]["sha256"])

        # The default corpus has no eligible incomplete-line row for this profile.
        # Exercise the implementation separately from any count of corpus executions.
        row = case(382)
        with tempfile.TemporaryDirectory() as directory:
            fixture = RuntimeFixture(binary, Path(directory), row, 30)
            decision, observed = execute(fixture, Plan("audit.incomplete_jsonl", False), row)
            self.assertEqual(decision, "FAIL_CLOSED")
            self.assertTrue(observed["refused_at_runtime_startup"])
            self.assertEqual(observed["provider_requests_after_fault"], 0)
            self.assertEqual(observed["provider_connections_after_fault"], 0)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            corpus = root / "corpus"
            output = root / "report"
            generate(corpus, count=12158, shard_size=12158, quiet=True)
            with binary.open("rb") as file:
                binary_digest = hashlib.file_digest(file, "sha256").hexdigest()
            summary = run(binary, binary_digest, corpus, sha((corpus / "manifest.json").read_bytes()),
                          output, 12158, 12158, 1, 30)
            self.assertEqual(summary["results"], {"PASSED": 1, "FAILED": 0, "SKIPPED": 0})
            with gzip.open(output / "observations.jsonl.gz", "rt") as file:
                observed = json.loads(file.readline())
                self.assertFalse(file.readline())
            self.assertEqual(observed["input_sha256"], sha(case_line(case(12158))))
            self.assertEqual(observed["observed_decision"], "PERMIT")


if __name__ == "__main__":
    unittest.main()

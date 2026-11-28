import base64
import copy
import gzip
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from synthetic.attestation import ED25519_SPKI, NATIVE_SIGNATURE_CONTEXT, ORACLE_GATE, SIGNATURE_CONTEXT, OpenSSLVerifier, Trust
from synthetic.generate import case, generate
from synthetic.oracle_evidence import check_observation
from synthetic.readiness import MANDATORY, evaluate


def sha256(data):
    return hashlib.sha256(data).hexdigest()


OPENSSL = shutil.which("openssl")
COMMIT = "a" * 40
CAMPAIGN = "fixture-campaign"
LINUX_GATE = "linux_security_tests"


class ReadinessInputTests(unittest.TestCase):
    def setUp(self):
        workspace = tempfile.TemporaryDirectory()
        self.addCleanup(workspace.cleanup)
        self.corpus = Path(workspace.name)
        self.receipts = self.corpus / "receipts.json"
        generate(self.corpus, count=2, quiet=True)

    def test_unsigned_legacy_metadata_never_qualifies(self):
        old = {gate: {"passed": True, "source_commit": COMMIT,
                      "artifact_sha256": "a" * 64} for gate in MANDATORY}
        old.update(executed_cases=5_000_000, oracle_passed=True)
        self.receipts.write_text(json.dumps(old), encoding="utf-8")
        result = evaluate(self.corpus, self.receipts, source_commit=COMMIT, campaign_id=CAMPAIGN)
        self.assertEqual(result["passed_gates"], 0)
        self.assertEqual(result["executed_cases"], 0)
        self.assertIn("signed release evidence", result["input_errors"][0])

    def test_duplicate_and_nonfinite_documents_fail_closed(self):
        for bad_document in ('{"gates": {}, "gates": {}}', '{"passed": NaN}'):
            with self.subTest(document=bad_document):
                self.receipts.write_text(bad_document, encoding="utf-8")
                result = evaluate(self.corpus, self.receipts, source_commit=COMMIT, campaign_id=CAMPAIGN)
                self.assertEqual(result["passed_gates"], 0)
                self.assertTrue(result["input_errors"])

    def test_malformed_input_exits_with_a_readiness_report(self):
        self.receipts.write_text("{invalid json", encoding="utf-8")
        child = subprocess.run(
            [sys.executable, "-m", "synthetic.readiness", "--corpus", str(self.corpus),
             "--receipts", str(self.receipts)], capture_output=True, text=True, timeout=10,
        )
        self.assertEqual(child.returncode, 2, child.stderr)
        self.assertFalse(json.loads(child.stdout)["ready"])


@unittest.skipUnless(OPENSSL, "requires a trusted OpenSSL 3 executable")
class SignedReadinessTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        workspace = tempfile.TemporaryDirectory()
        cls.addClassCleanup(workspace.cleanup)
        cls.keys = Path(workspace.name).resolve()
        cls.openssl = Path(OPENSSL).resolve()
        cls.signing_environment = OpenSSLVerifier(cls.openssl, cls.keys).env
        cls.public = {}
        for name in ("linux", "windows", "macos", "matrix", "review"):
            key = cls.keys / f"{name}.pem"
            cls.openssl_command("genpkey", "-algorithm", "ED25519", "-out", str(key))
            der = cls.openssl_command("pkey", "-in", str(key), "-pubout", "-outform", "DER")
            if not der.startswith(ED25519_SPKI) or len(der) != len(ED25519_SPKI) + 32:
                raise AssertionError("OpenSSL returned an unexpected Ed25519 public key")
            cls.public[name] = der[len(ED25519_SPKI):].hex()

    @classmethod
    def openssl_command(cls, *args):
        return subprocess.run([str(cls.openssl), *args], cwd=cls.keys, env=cls.signing_environment,
                              capture_output=True, timeout=10, check=True).stdout

    def setUp(self):
        workspace = tempfile.TemporaryDirectory()
        self.addCleanup(workspace.cleanup)
        self.root = Path(workspace.name).resolve()
        self.corpus = self.root / "corpus"
        self.evidence = self.root / "evidence"
        self.evidence.mkdir()
        self.manifest = generate(self.corpus, count=6, shard_size=2, quiet=True)
        self.trust = self.root / "trust.json"
        self.receipts = self.evidence / "receipts.json"
        self.digest = sha256((self.corpus / "manifest.json").read_bytes())
        self.binaries = {"linux": "1" * 64, "windows": "2" * 64, "macos": "3" * 64}
        self.generator = sha256(b"approved fixture generator snapshot")
        self.settings = {
            "schema": "jani.release-trust/v1", "corpus_manifest_sha256": self.digest,
            "generator_sha256": self.generator, "binary_sha256": self.binaries,
            "runners": {name: {"public_key": self.public[name], "platform": name, "gates": list(MANDATORY)}
                        for name in self.binaries},
            "reviewers": {"review": {"public_key": self.public["review"], "gates": list(MANDATORY)}},
        }
        self.settings["runners"]["matrix"] = {"public_key": self.public["matrix"], "platform": "matrix",
                                                 "gates": [ORACLE_GATE]}
        self.trust.write_text(json.dumps(self.settings), encoding="utf-8")
        self.bundle = {"schema": "jani.release-evidence/v2", "gates": {}}
        self.observations = []
        for shard in self.manifest["shards"]:
            with gzip.open(self.corpus / shard["file"], "rb") as rows:
                for line in rows:
                    fixture = json.loads(line)
                    self.observations.append({
                        "id": fixture["id"], "input_sha256": sha256(line), "execution": "runtime",
                        "adapter": "fixture.binary", "status": "PASSED", "decision": fixture["oracle"]["decision"],
                        "invariant": fixture["oracle"]["invariant"], "fault_hook": fixture["replay"]["fault_hook"],
                        "invariant_satisfied": True, "audit_verified": True,
                        "no_unapproved_side_effect": True, "no_secret_leak": True,
                        "platform": "linux", "runner_id": "linux", "binary_sha256": self.binaries["linux"],
                        "stdout_sha256": "4" * 64, "stderr_sha256": "5" * 64,
                        "audit_sha256": "6" * 64, "effective_policy_sha256": "7" * 64,
                    })

    def payload(self, gate, filename, *, runner=None):
        runner = runner or {ORACLE_GATE: "matrix", "windows_security_tests": "windows"}.get(gate, "linux")
        artifact = (self.evidence / filename).read_bytes()
        return {
            "schema": "jani.release-gate/v1", "gate": gate, "source_commit": COMMIT, "campaign_id": CAMPAIGN,
            "corpus_manifest_sha256": self.digest, "execution": "runtime", "result": "passed",
            "generator_sha256": self.generator,
            "review_decision": "approved", "runner_id": runner, "reviewer_id": "review", "platform": runner,
            "binary_sha256": self.binaries if runner == "matrix" else {runner: self.binaries[runner]},
            "environment": "ephemeral-test-fixture", "command": ["fixture", "verify"],
            "started_at": "2026-01-01T00:00:00Z", "finished_at": "2026-01-01T00:01:00Z",
            "reviewed_at": "2026-01-01T00:02:00Z",
            "artifact": {"path": filename, "bytes": len(artifact), "sha256": sha256(artifact)},
        }

    def sign(self, payload):
        raw = json.dumps(payload).encode("utf-8")
        envelope = {"payload_b64": base64.b64encode(raw).decode("ascii")}
        for role in ("runner", "reviewer"):
            envelope[f"{role}_signature"] = self.sign_bytes(SIGNATURE_CONTEXT + raw, payload[f"{role}_id"])
        self.bundle["gates"][payload["gate"]] = envelope
        return envelope

    def sign_bytes(self, data, signer):
        signed = self.root / "fixture.bin"
        signed.write_bytes(data)
        return self.openssl_command("pkeyutl", "-sign", "-rawin", "-inkey", str(self.keys / f"{signer}.pem"),
                                    "-in", str(signed)).hex()

    def signed_log(self, gate=LINUX_GATE, *, runner=None):
        name = f"{gate}.log"
        (self.evidence / name).write_bytes(b"fixture test log\n")
        return self.sign(self.payload(gate, name, runner=runner))

    def signed_ledger(self, rows=None, *, transform=lambda data: data, native_rows=None):
        filename = "oracle.jsonl.gz"
        observations = self.observations if rows is None else rows
        def encode(row):
            return json.dumps(row).encode("utf-8") + b"\n"
        raw = b"".join(map(encode, observations))
        (self.evidence / filename).write_bytes(transform(gzip.compress(raw)))
        batches = {}
        for row in observations if native_rows is None else native_rows:
            batches.setdefault(row["runner_id"], []).append(encode(row))
        payload = self.payload(ORACLE_GATE, filename)
        payload["oracle_runs"] = {}
        for runner, lines in batches.items():
            statement = {
                "schema": "jani.release.oracle-runner/v1", "runner_id": runner, "platform": runner,
                "campaign_id": CAMPAIGN, "source_commit": COMMIT, "corpus_manifest_sha256": self.digest,
                "generator_sha256": self.generator,
                "binary_sha256": self.binaries[runner], "observed_cases": len(lines),
                "observations_sha256": sha256(b"".join(lines)),
                "environment": "fixture-runtime", "command": ["fixture", "generate-observations"],
                "oracle_method": "fixture-observed-outcome",
                "started_at": "2026-01-01T00:00:05Z", "finished_at": "2026-01-01T00:00:50Z",
            }
            encoded = json.dumps(statement).encode("utf-8")
            payload["oracle_runs"][runner] = {
                "payload_b64": base64.b64encode(encoded).decode("ascii"),
                "signature": self.sign_bytes(NATIVE_SIGNATURE_CONTEXT + encoded, runner),
            }
        return self.sign(payload)

    def check(self, **overrides):
        self.receipts.write_text(json.dumps(self.bundle), encoding="utf-8")
        arguments = {"source_commit": COMMIT, "campaign_id": CAMPAIGN,
                     "trust": self.trust, "openssl_bin": self.openssl}
        arguments.update(overrides)
        with patch("synthetic.readiness.MINIMUM_RELEASE_CASES", self.manifest["records"]):
            return evaluate(self.corpus, self.receipts, **arguments)

    def test_complete_signed_fixture_requires_distinct_runner_and_review(self):
        for index, row in enumerate(self.observations):
            platform = ("linux", "windows", "macos")[index % 3]
            row.update(platform=platform, runner_id=platform, binary_sha256=self.binaries[platform])
        for gate in MANDATORY:
            if gate == ORACLE_GATE:
                self.signed_ledger()
            else:
                self.signed_log(gate)
        report = self.check()
        self.assertTrue(report["ready"], report)
        self.assertEqual(report["passed_gates"], len(MANDATORY))
        self.assertEqual(report["executed_cases"], self.manifest["records"])
        self.assertFalse(self.check(trust=None)["ready"])
        self.assertFalse(self.check(openssl_bin=None)["ready"])
        self.assertFalse(self.check(source_commit="b" * 40)["ready"])
        self.assertFalse(self.check(campaign_id="other-run")["ready"])

    def test_tampered_payload_and_replaced_artifact_fail(self):
        envelope = self.signed_log()
        self.assertEqual(self.check()["passed_gates"], 1)
        payload = json.loads(base64.b64decode(envelope["payload_b64"]))
        payload["command"] = ["other-command"]
        envelope["payload_b64"] = base64.b64encode(json.dumps(payload).encode("utf-8")).decode("ascii")
        self.assertIn("signature verification", self.check()["gate_errors"][LINUX_GATE])
        self.signed_log()
        (self.evidence / f"{LINUX_GATE}.log").write_bytes(b"Fixture test log\n")
        self.assertIn("SHA-256 mismatch", self.check()["gate_errors"][LINUX_GATE])

    def test_key_alias_wrong_gate_platform_and_binary_fail(self):
        self.signed_log("windows_security_tests", runner="linux")
        self.assertIn("runner platform", self.check()["gate_errors"]["windows_security_tests"])
        self.signed_log()
        payload = self.payload(LINUX_GATE, f"{LINUX_GATE}.log")
        payload["binary_sha256"] = {"linux": "c" * 64}
        self.sign(payload)
        self.assertIn("binary SHA-256", self.check()["gate_errors"][LINUX_GATE])
        self.settings["reviewers"]["review"]["public_key"] = self.public["linux"]
        self.trust.write_text(json.dumps(self.settings), encoding="utf-8")
        self.assertIn("must be distinct", self.check()["input_errors"][0])

    def test_gate_and_evidence_path_cannot_be_reused_outside_their_scope(self):
        self.bundle["gates"]["docs_claims"] = self.signed_log()
        self.assertIn("release context", self.check()["gate_errors"]["docs_claims"])
        external = self.root / "outside.log"
        external.write_bytes(b"fixture test log\n")
        self.sign(self.payload(LINUX_GATE, "../outside.log"))
        self.assertIn("invalid evidence artifact path", self.check()["gate_errors"][LINUX_GATE])

    def test_bundle_cannot_supply_its_own_trust_settings(self):
        self.signed_log()
        nested = self.evidence / "trust.json"
        shutil.copyfile(self.trust, nested)
        report = self.check(trust=nested)
        self.assertIn("must be outside", report["input_errors"][0])
        self.assertEqual(report["passed_gates"], 0)

    def test_matrix_signers_cannot_substitute_native_runner_rows(self):
        self.signed_ledger()
        self.assertTrue(self.check()["oracle_evidence_sufficient"])
        replacement = copy.deepcopy(self.observations)
        replacement[0]["stdout_sha256"] = "f" * 64
        self.signed_ledger(replacement, native_rows=self.observations)
        report = self.check()
        self.assertIn("signed native runner digest", report["gate_errors"][ORACLE_GATE])
        self.assertEqual(report["executed_cases"], 0)

    def test_native_batches_require_the_approved_generator_and_experiment_window(self):
        envelope = self.signed_ledger()
        original = json.loads(base64.b64decode(envelope["payload_b64"]))
        for field, value, reason in (("generator_sha256", "f" * 64, "release context"),
                                     ("finished_at", "2026-01-01T00:03:00Z", "campaign window")):
            with self.subTest(field=field):
                payload = copy.deepcopy(original)
                native = payload["oracle_runs"]["linux"]
                statement = json.loads(base64.b64decode(native["payload_b64"]))
                statement[field] = value
                raw = json.dumps(statement).encode("utf-8")
                native.update(payload_b64=base64.b64encode(raw).decode("ascii"),
                              signature=self.sign_bytes(NATIVE_SIGNATURE_CONTEXT + raw, "linux"))
                self.sign(payload)
                report = self.check()
                self.assertIn(reason, report["gate_errors"][ORACLE_GATE])
                self.assertEqual(report["executed_cases"], 0)

    def test_real_native_profile_fixtures_require_their_named_platform(self):
        trust = Trust.from_record(self.settings, MANDATORY)
        for index, platform in ((874, "windows"), (971, "macos")):
            with self.subTest(platform=platform):
                fixture = case(index)
                self.assertEqual(fixture["profile"], f"{platform}_limited")
                raw = json.dumps(fixture).encode("utf-8") + b"\n"
                observed = {**self.observations[0], "id": fixture["id"], "input_sha256": sha256(raw),
                            "decision": fixture["oracle"]["decision"], "invariant": fixture["oracle"]["invariant"],
                            "fault_hook": fixture["replay"]["fault_hook"]}
                with self.assertRaisesRegex(ValueError, "platform"):
                    check_observation(index, fixture, raw, observed, trust)
                observed.update(platform=platform, runner_id=platform, binary_sha256=self.binaries[platform])
                self.assertEqual(check_observation(index, fixture, raw, observed, trust), platform)

    def test_corrupt_duplicate_and_partial_coverage_yields_zero(self):
        for rows in (self.observations[:-1], self.observations + self.observations[-1:],
                     self.observations[:-1] + self.observations[:1], self.observations[::-1]):
            with self.subTest(length=len(rows), first=rows[0]["id"], last=rows[-1]["id"]):
                self.signed_ledger(rows)
                report = self.check()
                self.assertIn(ORACLE_GATE, report["gate_errors"], report)
                self.assertEqual(report["executed_cases"], 0)
        for transform in (lambda data: data[:-1], lambda data: data + b"unexpected"):
            self.signed_ledger(transform=transform)
            self.assertEqual(self.check()["executed_cases"], 0)

    def test_final_case_requires_observed_decision_input_runtime_and_safety(self):
        for field, value in (("decision", "UNKNOWN"), ("input_sha256", "4" * 64), ("no_secret_leak", 1),
                             ("execution", "simulation"), ("audit_verified", False), ("platform", "windows")):
            with self.subTest(field=field):
                bad = copy.deepcopy(self.observations)
                bad[-1][field] = value
                self.signed_ledger(bad)
                report = self.check()
                self.assertEqual(report["executed_cases"], 0)
                self.assertIn("case 6:", report["gate_errors"][ORACLE_GATE])

    def test_corpus_pin_shard_digest_and_release_minimum_are_enforced(self):
        self.signed_ledger()
        report = self.check()
        self.assertTrue(report["oracle_evidence_sufficient"], report)
        real_minimum = evaluate(self.corpus, self.receipts, source_commit=COMMIT, campaign_id=CAMPAIGN,
                                trust=self.trust, openssl_bin=self.openssl)
        self.assertIn("minimum release coverage", real_minimum["gate_errors"][ORACLE_GATE])
        path = self.corpus / self.manifest["shards"][0]["file"]
        with path.open("ab") as stream:
            stream.write(b"\0")
        self.assertIn("artifact size mismatch", self.check()["gate_errors"][ORACLE_GATE])
        manifest = self.corpus / "manifest.json"
        manifest.write_bytes(manifest.read_bytes() + b"\n")
        self.assertIn("operator-approved digest", self.check()["input_errors"][0])

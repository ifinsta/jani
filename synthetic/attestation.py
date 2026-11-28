"""Operator-pinned trust roots and detached Ed25519 release attestations."""

import base64
from dataclasses import dataclass
from datetime import datetime
import os
from pathlib import Path
import re
import subprocess

from .evidence_io import Artifact, decode_object, sha256_string


SIGNATURE_CONTEXT = b"jani.release.gate/v1\0"
NATIVE_SIGNATURE_CONTEXT = b"jani.release.oracle-runner/v1\0"
# RFC 8410 SubjectPublicKeyInfo header for an Ed25519 public key.
ED25519_SPKI = bytes.fromhex("302a300506032b6570032100")
OS_NAMES = {"linux", "windows", "macos"}
ORACLE_GATE = "synthetic_oracle_execution"
FIXED_GATE_PLATFORMS = {
    "linux_security_tests": "linux", "windows_security_tests": "windows", ORACLE_GATE: "matrix",
}


def identifier(value):
    return isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}", value) is not None


@dataclass(frozen=True)
class Key:
    public: bytes
    gates: frozenset[str]
    platform: str | None


@dataclass(frozen=True)
class NativeObservationBatch:
    count: int
    sha256: str


@dataclass(frozen=True)
class Trust:
    manifest_sha256: str
    generator_sha256: str
    binaries: dict[str, str]
    runners: dict[str, Key]
    reviewers: dict[str, Key]

    @classmethod
    def from_record(cls, value, mandatory):
        if value.get("schema") != "jani.release-trust/v1":
            raise ValueError("expected jani.release-trust/v1")
        manifest = value.get("corpus_manifest_sha256")
        if not sha256_string(manifest):
            raise ValueError("trust settings must pin the approved corpus manifest SHA-256")
        generator = value.get("generator_sha256")
        if not sha256_string(generator):
            raise ValueError("trust settings must pin the approved generator SHA-256")
        binaries = value.get("binary_sha256")
        if (not isinstance(binaries, dict) or not {"linux", "windows"} <= binaries.keys()
                or not binaries.keys() <= OS_NAMES or not all(map(sha256_string, binaries.values()))):
            raise ValueError("trust settings need pinned Linux and Windows binary SHA-256 values")

        seen_ids = set()
        seen_keys = set()

        def keys(table, runner):
            if not isinstance(table, dict) or not table:
                raise ValueError("trust settings require both runner and reviewer keys")
            accepted = {}
            for name, record in table.items():
                if not identifier(name) or not isinstance(record, dict):
                    raise ValueError("invalid trust identity")
                material = record.get("public_key")
                if not sha256_string(material):
                    raise ValueError("invalid Ed25519 public key")
                key = bytes.fromhex(material)
                if name in seen_ids or key in seen_keys:
                    raise ValueError("trust identities and public keys must be distinct")
                grades = record.get("gates")
                if (not isinstance(grades, list) or not grades or not all(
                        isinstance(gate, str) and gate in mandatory for gate in grades)):
                    raise ValueError("trusted key has an invalid gate scope")
                platform = record.get("platform")
                if runner:
                    if not isinstance(platform, str) or platform not in binaries.keys() | {"matrix"}:
                        raise ValueError("invalid runner platform")
                    if platform == "matrix" and set(grades) != {ORACLE_GATE}:
                        raise ValueError("matrix runner authority is limited to the oracle gate")
                elif platform is not None:
                    raise ValueError("reviewer keys must be separate from runner platforms")
                seen_ids.add(name)
                seen_keys.add(key)
                accepted[name] = Key(key, frozenset(grades), platform)
            return accepted

        return cls(manifest, generator, binaries, keys(value.get("runners"), True),
                   keys(value.get("reviewers"), False))


class OpenSSLVerifier:
    def __init__(self, executable, scratch):
        executable = Path(executable)
        if not executable.is_absolute():
            raise ValueError("--openssl-bin must be an absolute path to trusted OpenSSL")
        self.executable = executable.resolve(strict=True)
        if not self.executable.is_file():
            raise ValueError("OpenSSL executable is unavailable")
        self.scratch = scratch
        self.env = {name: value for name, value in os.environ.items()
                    if not name.upper().startswith("OPENSSL_")}
        config = scratch / "openssl.cnf"
        config.write_bytes(b"# Load the default provider without ambient configuration.\n")
        self.env["OPENSSL_CONF"] = str(config)

    def verify(self, public, raw, signature, *, context=SIGNATURE_CONTEXT):
        if not isinstance(signature, str) or not re.fullmatch(r"[0-9a-f]{128}", signature):
            raise ValueError("invalid Ed25519 signature encoding")
        key_file = self.scratch / "public.der"
        payload_file = self.scratch / "signed.bin"
        sig_file = self.scratch / "signature.bin"
        key_file.write_bytes(ED25519_SPKI + public)
        payload_file.write_bytes(context + raw)
        sig_file.write_bytes(bytes.fromhex(signature))
        output = subprocess.run(
            [str(self.executable), "pkeyutl", "-verify", "-rawin", "-pubin", "-keyform", "DER",
             "-inkey", str(key_file), "-in", str(payload_file), "-sigfile", str(sig_file)],
            cwd=self.scratch, env=self.env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, timeout=10, check=False,
        )
        if output.returncode != 0:
            raise ValueError("Ed25519 signature verification failed")


def receipt_time(value):
    if not isinstance(value, str) or not re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ", value):
        raise ValueError("receipt timestamps must be UTC RFC 3339 values")
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def check_environment(payload):
    environment = payload.get("environment")
    command = payload.get("command")
    if (not isinstance(environment, str) or not environment.strip() or not isinstance(command, list)
            or not command or not all(isinstance(argument, str) and argument for argument in command)):
        raise ValueError("receipt requires its test environment and command")


def native_observation_batches(entries, source_commit, campaign, trust, verifier, window):
    if not isinstance(entries, dict) or not entries:
        raise ValueError("oracle gate is missing signed native runner batches")
    batches = {}
    for name, envelope in entries.items():
        if not identifier(name) or not isinstance(envelope, dict) or not isinstance(envelope.get("payload_b64"), str):
            raise ValueError("invalid native runner envelope")
        runner = trust.runners.get(name)
        if runner is None or runner.platform not in OS_NAMES or ORACLE_GATE not in runner.gates:
            raise ValueError("native oracle runner is unauthorized")
        raw = base64.b64decode(envelope["payload_b64"], validate=True)
        if len(raw) > 16 * 1024:
            raise ValueError("native runner payload exceeds its limit")
        data = decode_object(raw)
        fields = {"schema": "jani.release.oracle-runner/v1", "runner_id": name, "platform": runner.platform,
                  "campaign_id": campaign, "source_commit": source_commit,
                  "corpus_manifest_sha256": trust.manifest_sha256, "generator_sha256": trust.generator_sha256,
                  "binary_sha256": trust.binaries[runner.platform]}
        if any(data.get(field) != expected for field, expected in fields.items()):
            raise ValueError("native runner statement does not match the release context")
        start, end = receipt_time(data.get("started_at")), receipt_time(data.get("finished_at"))
        if not window[0] <= start <= end <= window[1] or not identifier(data.get("oracle_method")):
            raise ValueError("native runner must identify its oracle method and run within the campaign window")
        check_environment(data)
        count, digest = data.get("observed_cases"), data.get("observations_sha256")
        if type(count) is not int or count <= 0 or not sha256_string(digest):
            raise ValueError("invalid native runner batch count or digest")
        verifier.verify(runner.public, raw, envelope.get("signature"), context=NATIVE_SIGNATURE_CONTEXT)
        batches[name] = NativeObservationBatch(count, digest)
    return batches


def verify_gate(envelope, gate, source_commit, campaign, trust, verifier):
    if not isinstance(envelope, dict) or not isinstance(envelope.get("payload_b64"), str):
        raise ValueError("missing signed gate envelope")
    raw = base64.b64decode(envelope["payload_b64"], validate=True)
    if len(raw) > 256 * 1024:
        raise ValueError("signed gate payload exceeds its limit")
    payload = decode_object(raw)
    fields = {"schema": "jani.release-gate/v1", "gate": gate, "source_commit": source_commit,
              "campaign_id": campaign, "corpus_manifest_sha256": trust.manifest_sha256,
              "generator_sha256": trust.generator_sha256,
              "execution": "runtime", "result": "passed", "review_decision": "approved"}
    if any(payload.get(name) != expected for name, expected in fields.items()):
        raise ValueError("gate, release context, or approval does not match")
    runner = payload.get("runner_id")
    reviewer = payload.get("reviewer_id")
    if not identifier(runner) or not identifier(reviewer):
        raise ValueError("missing runner or reviewer identity")
    runner_key = trust.runners.get(runner)
    reviewer_key = trust.reviewers.get(reviewer)
    if not runner_key or not reviewer_key or gate not in runner_key.gates or gate not in reviewer_key.gates:
        raise ValueError("runner or reviewer is unauthorized for this gate")
    platform = runner_key.platform
    if payload.get("platform") != platform or FIXED_GATE_PLATFORMS.get(gate, platform) != platform:
        raise ValueError("gate runner platform does not match")
    binaries = trust.binaries if platform == "matrix" else {platform: trust.binaries[platform]}
    if payload.get("binary_sha256") != binaries:
        raise ValueError("gate binary SHA-256 does not match the release candidate")
    started, finished, reviewed = (receipt_time(payload.get(field))
                                   for field in ("started_at", "finished_at", "reviewed_at"))
    if not started <= finished <= reviewed:
        raise ValueError("gate execution and review timestamps are out of order")
    check_environment(payload)
    artifact = Artifact.from_record(payload.get("artifact"))
    verifier.verify(runner_key.public, raw, envelope.get("runner_signature"))
    verifier.verify(reviewer_key.public, raw, envelope.get("reviewer_signature"))
    batches = {}
    if gate == ORACLE_GATE:
        batches = native_observation_batches(payload.get("oracle_runs"), source_commit, campaign,
                                              trust, verifier, (started, finished))
    return artifact, batches

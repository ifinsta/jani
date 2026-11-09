#!/usr/bin/env python3
"""Independent Jani Lab trial runner.

Run only under a trusted operator, NEVER from Jani's model tool or a candidate
checkout. Requires Docker/OCI isolation, a pinned trusted image, external test
suite, explicit expected test count, OpenSSL Ed25519 key and runner-file pin.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time

VERSION = "jani-lab/v1"
MAX_FILES = 20000
MAX_FILE_BYTES = 64 * 1024 * 1024
MAX_TOTAL_BYTES = 512 * 1024 * 1024
IGNORE_DIRS = frozenset({".git", ".jani", "target", "__pycache__", "node_modules"})
SKIP_FILES = frozenset({".env", ".netrc", "id_rsa", "id_ed25519"})
RESULT = re.compile(r"(?m)^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;")

class TrialError(Exception):
    pass

def hex_digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()

def assert_external(path: Path, roots: list[Path], label: str) -> None:
    resolved = path.resolve(strict=True)
    for root in roots:
        if resolved == root or root in resolved.parents:
            raise TrialError(f"{label} must not live inside a baseline, candidate or suite")

def copy_and_digest(source: Path, dest: Path) -> str:
    """Create a sanitized immutable-input snapshot; refuse symlinks and huge trees."""
    hasher = hashlib.sha256(b"jani-lab/source-tree/v1\0")
    count = 0
    total = 0
    dest.mkdir(mode=0o700, parents=True)
    for parent, directories, files in os.walk(source, followlinks=False):
        directories.sort()
        files.sort()
        for name in list(directories):
            item = Path(parent) / name
            if item.is_symlink():
                raise TrialError(f"symlink in experiment tree: {item}")
            if name in IGNORE_DIRS:
                directories.remove(name)
        for name in files:
            item = Path(parent) / name
            if item.is_symlink() or not item.is_file():
                raise TrialError(f"non-regular file in experiment tree: {item}")
            if name in SKIP_FILES or name.startswith(".env."):
                continue
            size = item.stat().st_size
            count += 1
            total += size
            if count > MAX_FILES or size > MAX_FILE_BYTES or total > MAX_TOTAL_BYTES:
                raise TrialError("trial source exceeds bounded input limits")
            rel = item.relative_to(source)
            data = item.read_bytes()
            if len(data) != size:
                raise TrialError(f"file modified during snapshot: {item}")
            encoded = rel.as_posix().encode("utf-8")
            hasher.update(len(encoded).to_bytes(8, "big"))
            hasher.update(encoded)
            hasher.update(len(data).to_bytes(8, "big"))
            hasher.update(hashlib.sha256(data).digest())
            out = dest / rel
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_bytes(data)
            out.chmod(item.stat().st_mode & 0o777)
    if count == 0:
        raise TrialError("empty experiment tree")
    return hasher.hexdigest()

def signature_bytes(proof: dict) -> bytes:
    values = [
        proof["version"], proof["tenantId"], proof["sourceRunId"],
        proof["findingId"], proof["scenarioId"], str(proof["iteration"]),
        proof["revisionSha256"], proof["suiteSha256"],
        proof["evaluatorSha256"], proof["environmentSha256"],
        str(proof["passed"]), str(proof["failed"]), str(proof["skipped"]),
        str(proof["durationMs"]), str(proof["completed"]).lower(),
    ]
    result = bytearray(b"jani-lab/independent-trial/v1\0")
    for value in values:
        encoded = value.encode("utf-8")
        result += len(encoded).to_bytes(8, "big") + encoded
    return bytes(result)

def sign(proof: dict, key: Path, temporary: Path) -> str:
    message = temporary / "message.bin"
    message.write_bytes(signature_bytes(proof))
    message.chmod(0o600)
    result = subprocess.run(
        ["openssl", "pkeyutl", "-sign", "-rawin", "-inkey", str(key), "-in", str(message)],
        capture_output=True, check=False, timeout=15,
    )
    message.unlink(missing_ok=True)
    if result.returncode != 0 or len(result.stdout) != 64:
        raise TrialError("independent Ed25519 signing failed")
    return result.stdout.hex()

def run_trial(
    source: Path, suite: Path, image: str, expected_tests: int, timeout: int,
    iteration: int,
) -> tuple[int, int, int, int]:
    # Docker daemon must itself be administered by the trusted verifier host.
    name = f"jani-lab-{os.getpid()}-{iteration}-{os.urandom(4).hex()}"
    command = [
        "docker", "run", "--rm", "--name", name,
        "--network", "none", "--read-only",
        "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
        "--pids-limit", "256", "--memory", "2g", "--cpus", "2",
        "--user", "65534:65534",
        "--tmpfs", "/tmp:rw,nosuid,size=1073741824",
        "--mount", f"type=bind,source={source},target=/work,readonly",
        "--mount", f"type=bind,source={suite},target=/suite,readonly",
        "--workdir", "/suite",
        "--env", "HOME=/tmp",
        "--env", "CARGO_NET_OFFLINE=true",
        "--env", "CARGO_TARGET_DIR=/tmp/jani-target",
        image, "cargo", "test", "--manifest-path", "/suite/Cargo.toml",
        "--offline", "--locked", "--workspace", "--all-targets", "--quiet",
    ]
    started = time.monotonic_ns()
    try:
        with tempfile.TemporaryFile(mode="w+b") as output:
            try:
                process = subprocess.run(
                    command, stdout=output, stderr=subprocess.STDOUT,
                    check=False, timeout=timeout,
                )
            except subprocess.TimeoutExpired as error:
                raise TrialError("experiment container timed out") from error
            elapsed = max(1, (time.monotonic_ns() - started) // 1_000_000)
            if output.tell() > 4 * 1024 * 1024:
                raise TrialError("trial output exceeds 4 MiB")
            output.seek(0)
            transcript = output.read().decode("utf-8", errors="replace")
        matches = RESULT.findall(transcript)
        if not matches:
            raise TrialError("trusted runner could not find a complete Rust test summary")
        passed = sum(int(row[1]) for row in matches)
        failed = sum(int(row[2]) for row in matches)
        skipped = sum(int(row[3]) for row in matches)
        if passed + failed + skipped != expected_tests:
            raise TrialError("executed test count differs from externally pinned expected count")
        if process.returncode == 0 and failed == 0:
            return passed, failed, skipped, elapsed
        if process.returncode == 101 and failed > 0:
            return passed, failed, skipped, elapsed
        raise TrialError("test runner exit status disagrees with observed test results")
    finally:
        subprocess.run(
            ["docker", "rm", "--force", name],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            timeout=15, check=False,
        )

def read_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    for field in ("baseline", "candidate", "suite", "signing-key", "output"):
        parser.add_argument("--" + field, required=True, type=Path)
    for field in ("tenant", "source-run-id", "finding-id", "scenario", "key-id", "image",
                  "expected-runner-sha256"):
        parser.add_argument("--" + field, required=True)
    parser.add_argument("--expected-tests", required=True, type=int)
    parser.add_argument("--timeout", type=int, default=900)
    return parser.parse_args()

def main() -> int:
    args = read_args()
    sources = [args.baseline.resolve(strict=True),
               args.candidate.resolve(strict=True), args.suite.resolve(strict=True)]
    for path in sources:
        if not path.is_dir():
            raise TrialError("all experiment inputs must be directories")
    for i, path in enumerate(sources):
        for other in sources[i + 1:]:
            if path == other or path in other.parents or other in path.parents:
                raise TrialError("baseline, candidate and external suite must be separate")
    key = args.signing_key.resolve(strict=True)
    output = args.output.resolve()
    assert_external(key, sources, "private signing key")
    for root in sources:
        if output == root or root in output.parents:
            raise TrialError("evidence output cannot be written into experiment inputs")
    if not re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", args.tenant) or not re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", args.source_run_id):
        raise TrialError("invalid tenant or source run ID")
    if not re.fullmatch(r"mir-[a-f0-9]{24}", args.finding_id):
        raise TrialError("finding must be a real Mirror-format ID")
    for value in (args.scenario, args.key_id):
        if not re.fullmatch(r"[A-Za-z0-9_.-]{1,64}", value):
            raise TrialError("invalid scenario or verifier key ID")
    if not re.fullmatch(r"[A-Za-z0-9./_:-]+@sha256:[a-f0-9]{64}", args.image):
        raise TrialError("experiment image must be an immutable OCI SHA-256 reference")
    if not 10 <= args.expected_tests <= 100000 or not 10 <= args.timeout <= 3600:
        raise TrialError("expected tests or time limit outside approved bounds")
    executable = Path(__file__).resolve(strict=True)
    actual_runner_digest = hex_digest(executable.read_bytes())
    if args.expected_runner_sha256 != actual_runner_digest:
        raise TrialError("independent runner digest does not match operator-approved pin")
    assert_external(executable, sources, "trusted verifier script")
    with tempfile.TemporaryDirectory(prefix="jani-lab-trusted-") as base:
        tmp = Path(base)
        rev_base = copy_and_digest(sources[0], tmp / "baseline")
        rev_candidate = copy_and_digest(sources[1], tmp / "candidate")
        suite_digest = copy_and_digest(sources[2], tmp / "suite")
        if rev_base == rev_candidate:
            raise TrialError("candidate source tree matches baseline")
        image_digest = hex_digest(
            (args.image + "|network=none|readonly|no-new-privileges|pids=256|mem=2g|cpus=2").encode()
        )
        result = {
            "version": VERSION, "tenantId": args.tenant,
            "sourceRunId": args.source_run_id, "findingId": args.finding_id,
            "baseline": [], "candidate": [],
        }
        for iteration in range(3):
            for kind, revision, directory in (
                ("baseline", rev_base, tmp / "baseline"),
                ("candidate", rev_candidate, tmp / "candidate"),
            ):
                passed, failed, skipped, duration_ms = run_trial(
                    directory, tmp / "suite", args.image,
                    args.expected_tests, args.timeout, iteration,
                )
                proof = {
                    "version": VERSION, "tenantId": args.tenant,
                    "sourceRunId": args.source_run_id,
                    "findingId": args.finding_id,
                    "scenarioId": args.scenario, "iteration": iteration,
                    "revisionSha256": revision,
                    "suiteSha256": suite_digest,
                    "evaluatorSha256": actual_runner_digest,
                    "environmentSha256": image_digest,
                    "passed": passed, "failed": failed, "skipped": skipped,
                    "durationMs": duration_ms, "completed": True,
                }
                result[kind].append({
                    "proof": proof, "keyId": args.key_id,
                    "signature": sign(proof, key, tmp),
                })
        # Write once, only after all six receipts exist; never expose the key.
        parent = output.parent
        parent.mkdir(parents=True, exist_ok=True)
        fd, name = tempfile.mkstemp(prefix=".jani-lab-", suffix=".json", dir=parent)
        try:
            os.fchmod(fd, 0o600)
            with os.fdopen(fd, "w", encoding="utf-8") as file:
                json.dump(result, file, separators=(",", ":"), sort_keys=True)
                file.write("\n")
                file.flush()
                os.fsync(file.fileno())
            os.replace(name, output)
        finally:
            if os.path.exists(name):
                os.unlink(name)
    print(json.dumps({"status": "six_signed_trials", "output": str(output),
                      "runnerSha256": actual_runner_digest, "autoApplied": False}))
    return 0

if __name__ == "__main__":
    try:
        sys.exit(main())
    except (TrialError, OSError, subprocess.SubprocessError) as error:
        print(f"jani-lab-runner: {error}", file=sys.stderr)
        sys.exit(1)

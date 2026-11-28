"""Run a small suite of CLI preflight checks in disposable directories."""

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

from .generate import case


@dataclass(frozen=True)
class Probe:
    args: tuple[str, ...]
    exit_code: int
    stream: str
    diagnostic: bytes


SUPPORTED = {
    ("cli", "missing_argument"): Probe(("plugins", "inspect"), 1, "stderr", b"plugins inspect requires a plugin id"),
    ("cli", "unexpected_subcommand"): Probe(("unsupported-command",), 1, "stderr", b"unknown command: unsupported-command"),
    ("cli", "unknown_mode"): Probe(("doctor", "--mode", "not-a-mode"), 1, "stderr", b"unknown mode: not-a-mode"),
    ("cli", "help_requires_key"): Probe(("--help",), 0, "stdout", b"Rust-native, skill-aware AI coding agent"),
}
INHERITED_ENV = {"PATH", "SYSTEMROOT", "WINDIR", "TEMP", "TMP", "TMPDIR", "LANG", "LC_ALL"}


def run_probe(binary: Path, index: int, seed=20261025, *, seen=None):
    row = case(index, seed)
    key = (row["domain"], row["fault"])
    record = {"id": row["id"], "fault_hook": row["replay"]["fault_hook"], "execution": "preflight"}
    probe = SUPPORTED.get(key)
    if probe is None:
        return {**record, "status": "SKIPPED", "reason": "NO_ADAPTER_FOR_FAULT"}
    if row["mutation"] != "baseline" or row["secondary"] is not None or row["tertiary"] is not None:
        return {**record, "status": "SKIPPED", "reason": "NO_ADAPTER_FOR_VARIANT"}
    if seen is not None:
        if key in seen:
            return {**record, "status": "SKIPPED", "reason": "PREFLIGHT_ALREADY_RUN"}
        seen.add(key)
    env = {key: value for key, value in os.environ.items() if key.upper() in INHERITED_ENV}
    start = time.monotonic()
    with tempfile.TemporaryDirectory() as workdir:
        env["JANI_USER_SETTINGS"] = str(Path(workdir) / "absent-user-settings.json")
        try:
            result = subprocess.run(
                [str(binary.resolve()), *probe.args, "--repo", workdir, "--state-dir", str(Path(workdir) / "state")],
                cwd=workdir, stdin=subprocess.DEVNULL, capture_output=True, timeout=8, env=env, check=False,
            )
        except subprocess.TimeoutExpired:
            return {**record, "status": "FAILED", "reason": "PROBE_TIMEOUT"}
        except OSError:
            return {**record, "status": "FAILED", "reason": "PROBE_START_FAILED"}
    matched = result.returncode == probe.exit_code and probe.diagnostic in getattr(result, probe.stream)
    return {**record, "status": "PASSED" if matched else "FAILED", "exit": result.returncode,
            "oracle": "exit_code_and_diagnostic", "elapsed_s": round(time.monotonic() - start, 6),
            "stdout_sha256": hashlib.sha256(result.stdout).hexdigest(),
            "stderr_sha256": hashlib.sha256(result.stderr).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--limit", type=int, default=20_000)
    parser.add_argument("--seed", type=int, default=20261025)
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        raise SystemExit("Jani executable not found")
    if not 1 <= args.limit <= 100_000:
        raise SystemExit("invalid limit")
    counts = {"PASSED": 0, "FAILED": 0, "SKIPPED": 0}
    seen = set()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    try:
        with args.output.open("x", encoding="utf-8") as output:
            for index in range(1, args.limit + 1):
                row = run_probe(binary, index, args.seed, seen=seen)
                counts[row["status"]] += 1
                output.write(json.dumps(row, sort_keys=True) + "\n")
    except FileExistsError:
        raise SystemExit("preflight report output already exists") from None
    print(json.dumps({"cases": args.limit, "results": counts, "source": "real_binary_probe",
                      "execution": "preflight", "production_ready": False}))
    raise SystemExit(1 if counts["FAILED"] else 0)


if __name__ == "__main__":
    main()

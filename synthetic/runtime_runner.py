"""Execute mapped synthetic corpus rows against a pinned local Jani binary."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile
import time
import zlib

from .evidence_io import checked_artifact, read_object, regular_input, sha256_string
from .generate import case, case_line
from .oracle_evidence import MAX_RECORD_BYTES, json_line, manifest_size, shards
from .runtime_adapters import choose, execute
from .runtime_harness import RuntimeFixture, encoded


def checked_rows(corpus, manifest, start, end):
    # Finish authentication of one shard before yielding any case from it.
    inventory = list(shards(manifest))
    if type(manifest.get("seed")) is not int:
        raise ValueError("corpus manifest has no integer generator seed")
    for artifact, first, last in inventory:
        if last < start or end < first:
            continue
        with tempfile.SpooledTemporaryFile(max_size=4 * 1024 * 1024) as selected:
            with checked_artifact(corpus, artifact) as data, gzip.GzipFile(fileobj=data) as rows:
                with gzip.GzipFile(fileobj=selected, mode="wb", compresslevel=1) as copied:
                    for index in range(first, last + 1):
                        raw = rows.readline(MAX_RECORD_BYTES + 1)
                        if raw != case_line(case(index, manifest["seed"])):
                            raise ValueError(f"case {index} does not match the deterministic generator bytes")
                        if start <= index <= end:
                            copied.write(raw)
                    if rows.read(1):
                        raise ValueError("fixture shard has extra rows")
            selected.seek(0)
            with gzip.GzipFile(fileobj=selected, mode="rb") as copied:
                for _ in range(max(start, first), min(end, last) + 1):
                    row, raw = json_line(copied)
                    yield row, hashlib.sha256(raw).hexdigest()


def stage_binary(source, target, digest):
    checksum = hashlib.sha256()
    with regular_input(source) as (stream, size), target.open("xb") as file:
        remaining = size
        while remaining:
            chunk = stream.read(min(remaining, 1024 * 1024))
            if not chunk:
                raise ValueError("Jani executable changed while being staged")
            checksum.update(chunk)
            file.write(chunk)
            remaining -= len(chunk)
        if stream.read(1) or checksum.hexdigest() != digest:
            raise ValueError("Jani executable does not match the selected SHA-256")
    target.chmod(stat.S_IRUSR | stat.S_IXUSR)


def run_case(binary, row, plan, timeout):
    start = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="jani-runtime-fixture-") as temp:
        fixture = RuntimeFixture(binary, Path(temp), row, timeout)
        try:
            decision, facts = execute(fixture, plan, row)
            result = {"status": "PASSED", "observed_decision": decision, "observed": facts}
            if decision != row["oracle"]["decision"]:
                result.update(status="FAILED", reason="observed decision differs from the pinned fixture oracle")
        except (ValueError, OSError, EOFError, KeyError, TypeError, AttributeError, RecursionError) as error:
            result = {"status": "FAILED", "reason": str(error)}
        trace = fixture.trace()
        if trace["capture_errors"]:
            result["status"] = "FAILED"
            result.setdefault("reason", "could not retain all bounded trace files")
        return {**result, "trace": trace, "elapsed_s": round(time.monotonic() - start, 6)}


def run(binary, binary_sha256, corpus, manifest_sha256, output, first, last, max_executions, timeout):
    if not sha256_string(manifest_sha256) or not sha256_string(binary_sha256):
        raise ValueError("manifest and binary SHA-256 pins must be lowercase hexadecimal")
    manifest, raw_manifest = read_object(corpus / "manifest.json")
    if hashlib.sha256(raw_manifest).hexdigest() != manifest_sha256:
        raise ValueError("corpus manifest SHA-256 does not match the selected pin")
    size = manifest_size(manifest)
    if not 1 <= first <= last <= size or max_executions < 1 or not 0 < timeout <= 180:
        raise ValueError("invalid case range, execution limit, or command timeout")
    if max_executions > last - first + 1:
        max_executions = last - first + 1

    counts = {"PASSED": 0, "FAILED": 0, "SKIPPED": 0}
    attempts = 0
    output = output.absolute()
    output.parent.resolve(strict=True)
    if output.resolve().is_relative_to(corpus.resolve(strict=True)):
        raise ValueError("run output must be outside the fixture corpus")
    if output.exists() or output.is_symlink():
        raise ValueError("run output directory already exists")
    with tempfile.TemporaryDirectory(prefix="jani-runtime-binary-") as staged:
        target = Path(staged) / ("jani.exe" if os.name == "nt" else "jani")
        stage_binary(binary, target, binary_sha256)
        try:
            output.mkdir(mode=0o700)
            ledger = output / "observations.jsonl.gz"
            with ledger.open("xb") as file:
                with gzip.GzipFile(fileobj=file, mode="wb", compresslevel=1, mtime=0, filename="") as report:
                    for row, input_sha256 in checked_rows(corpus, manifest, first, last):
                        plan, skipped = choose(row)
                        if plan is not None and attempts >= max_executions:
                            skipped = "EXECUTION_LIMIT_REACHED"
                        if skipped:
                            result = {"status": "SKIPPED", "reason": skipped}
                        else:
                            attempts += 1
                            result = run_case(target, row, plan, timeout)
                        counts[result["status"]] += 1
                        record = {"id": row["id"], "input_sha256": input_sha256,
                                  "fault_hook": row["replay"]["fault_hook"], "expected": row["oracle"],
                                  "mutation": row["mutation"], "profile": row["profile"],
                                  "execution": "local-runtime" if result["status"] != "SKIPPED" else "not-executed",
                                  **result}
                        report.write(encoded(record) + b"\n")
                file.flush()
                os.fsync(file.fileno())
            with target.open("rb") as file:
                if hashlib.file_digest(file, "sha256").hexdigest() != binary_sha256:
                    raise ValueError("staged Jani executable changed during the run")
        finally:
            # Windows cannot remove a read-only staged file during TemporaryDirectory cleanup.
            if target.exists():
                target.chmod(stat.S_IRUSR | stat.S_IWUSR)
    with ledger.open("rb") as file:
        evidence_sha256 = hashlib.file_digest(file, "sha256").hexdigest()
    summary = {"schema": "jani.local-runtime-report/v1", "first": first, "last": last,
               "corpus_manifest_sha256": manifest_sha256, "binary_sha256": binary_sha256,
               "observations_sha256": evidence_sha256, "results": counts, "attempted": attempts,
               "host": {"system": os.name, "platform": os.sys.platform}, "production_ready": False}
    with (output / ".pending-summary").open("xb") as file:
        file.write(encoded(summary) + b"\n")
        file.flush()
        os.fsync(file.fileno())
    (output / ".pending-summary").rename(output / "summary.json")
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--binary-sha256", required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--manifest-sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--first", type=int, required=True)
    parser.add_argument("--last", type=int, required=True)
    parser.add_argument("--max-executions", type=int, default=32)
    parser.add_argument("--timeout", type=int, default=30)
    args = parser.parse_args()
    try:
        result = run(**vars(args))
    except (ValueError, OSError, EOFError, RecursionError, zlib.error) as error:
        parser.exit(2, f"runtime runner: {error}\n")
    print(json.dumps(result, sort_keys=True))
    if result["results"]["FAILED"] or not result["attempted"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()

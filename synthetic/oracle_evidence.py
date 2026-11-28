"""Stream an authenticated execution ledger against the pinned corpus."""

import gzip
import hashlib

from .attestation import ORACLE_GATE, identifier
from .catalog import OUTCOMES, PROFILES
from .evidence_io import Artifact, checked_artifact, decode_object, sha256_string


MAX_RECORD_BYTES = 16 * 1024
MAX_SHARDS = 4096
MAX_CASES = 20_000_000
PROFILE_PLATFORMS = {"linux_secure": "linux", "windows_limited": "windows", "macos_limited": "macos"}
REQUIRED_OBSERVATIONS = ("invariant_satisfied", "audit_verified", "no_unapproved_side_effect", "no_secret_leak")
DECISIONS = set(OUTCOMES) | {"PERMIT"}
PROFILE_NAMES = frozenset(PROFILES)


def manifest_size(manifest):
    count = manifest.get("records")
    if (manifest.get("schema") != "jani.synthetic.manifest/v1"
            or type(count) is not int or not 0 < count <= MAX_CASES):
        raise ValueError("expected a v1 corpus manifest with a supported integer record count")
    return count


def shards(manifest):
    descriptors = manifest.get("shards")
    if not isinstance(descriptors, list) or not 0 < len(descriptors) <= MAX_SHARDS:
        raise ValueError("invalid corpus shard inventory")
    paths = set()
    first = 1
    for item in descriptors:
        artifact = Artifact.from_record(item, path_field="file")
        start, end, rows = (item.get(field) for field in ("first", "last", "rows"))
        if (artifact.path in paths or any(type(value) is not int for value in (start, end, rows))
                or start != first or not start <= end <= manifest["records"] or rows != end - start + 1):
            raise ValueError("corpus shard ranges must be distinct and contiguous")
        paths.add(artifact.path)
        yield artifact, start, end
        first = end + 1
    if first - 1 != manifest["records"]:
        raise ValueError("corpus shard ranges do not cover the declared record count")


def json_line(stream):
    raw = stream.readline(MAX_RECORD_BYTES + 1)
    if not raw.endswith(b"\n") or len(raw) > MAX_RECORD_BYTES:
        raise ValueError("missing or out-of-bounds evidence row")
    return decode_object(raw), raw


def check_observation(index, expected, raw_fixture, observed, trust):
    label = f"JANI-{index:010d}"
    oracle, replay = expected.get("oracle"), expected.get("replay")
    if not isinstance(oracle, dict) or not isinstance(replay, dict) or expected.get("id") != label:
        raise ValueError("fixture lacks its canonical case ID or oracle")
    if observed.get("id") != label or observed.get("input_sha256") != hashlib.sha256(raw_fixture).hexdigest():
        raise ValueError("observation does not match the next exact corpus case")
    decision, hook = oracle.get("decision"), replay.get("fault_hook")
    invariant = oracle.get("invariant")
    if (not isinstance(decision, str) or decision not in DECISIONS
            or not identifier(hook) or not identifier(invariant)
            or observed.get("decision") != decision or observed.get("fault_hook") != hook
            or observed.get("invariant") != invariant):
        raise ValueError("observation does not match the pinned fault or oracle")
    if (observed.get("execution") != "runtime" or observed.get("status") != "PASSED"
            or not identifier(observed.get("adapter"))
            or any(observed.get(field) is not True for field in REQUIRED_OBSERVATIONS)):
        raise ValueError("observation is missing a passing runtime and safety result")
    runner_id = observed.get("runner_id")
    runner = trust.runners.get(runner_id) if identifier(runner_id) else None
    platform = observed.get("platform")
    profile = expected.get("profile")
    if (runner is None or ORACLE_GATE not in runner.gates or runner.platform != platform
            or not isinstance(platform, str) or platform not in trust.binaries
            or not isinstance(profile, str) or profile not in PROFILE_NAMES
            or PROFILE_PLATFORMS.get(profile, platform) != platform
            or observed.get("binary_sha256") != trust.binaries[platform]):
        raise ValueError("observation runner, platform, or release binary does not match the case")
    for field in ("stdout_sha256", "stderr_sha256", "audit_sha256", "effective_policy_sha256"):
        if not sha256_string(observed.get(field)):
            raise ValueError(f"observation is missing {field}")
    return runner_id


def verify_execution(corpus, manifest, evidence_root, artifact, trust, batches):
    if sum(batch.count for batch in batches.values()) != manifest["records"]:
        raise ValueError("native runner batches do not cover the declared case count")
    inventory = list(shards(manifest))
    counts = dict.fromkeys(batches, 0)
    digests = {runner: hashlib.sha256() for runner in batches}
    with checked_artifact(evidence_root, artifact) as source, gzip.GzipFile(fileobj=source) as ledger:
        for fixture, first, end in inventory:
            with checked_artifact(corpus, fixture) as data, gzip.GzipFile(fileobj=data) as rows:
                for index in range(first, end + 1):
                    try:
                        expected, raw_fixture = json_line(rows)
                        observed, raw_observation = json_line(ledger)
                        runner = check_observation(index, expected, raw_fixture, observed, trust)
                        if runner not in batches or counts[runner] >= batches[runner].count:
                            raise ValueError("observation has no matching native signature batch")
                        digests[runner].update(raw_observation)
                        counts[runner] += 1
                    except (ValueError, OSError, EOFError, RecursionError) as error:
                        raise ValueError(f"case {index}: {error}") from error
                if rows.read(1):
                    raise ValueError("fixture shard has extra rows")
        if ledger.read(1):
            raise ValueError("execution ledger has extra rows")
    for runner, batch in batches.items():
        if counts[runner] != batch.count or digests[runner].hexdigest() != batch.sha256:
            raise ValueError("execution rows do not match the signed native runner digest")
    return manifest["records"]

"""Verify independently attested release gates and exact runtime coverage."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import zlib

if not __package__:
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from synthetic.attestation import ORACLE_GATE, OpenSSLVerifier, Trust, identifier, verify_gate
from synthetic.evidence_io import checked_artifact, read_object
from synthetic.oracle_evidence import manifest_size, verify_execution

MANDATORY = (
    'rust_workspace_tests', 'linux_security_tests', 'windows_security_tests',
    'mcp_source_conformance', 'sandbox_isolation', 'crash_recovery',
    'data_governance_egress', 'authz_revocation', 'human_only_commit',
    'no_side_effect_replay', 'audit_integrity', 'budget_and_approval',
    'synthetic_oracle_execution', 'load_soak_latency', 'operator_disaster_recovery',
    'docs_claims',
)
MINIMUM_RELEASE_CASES = 5_000_000
EVIDENCE_ERRORS = (OSError, ValueError, RuntimeError, EOFError, zlib.error, subprocess.TimeoutExpired)


def evaluate(corpus: Path, receipts: Path | None = None, *, source_commit: str | None = None,
             campaign_id: str | None = None, trust: Path | None = None,
             artifact_root: Path | None = None, openssl_bin: Path | None = None):
    errors = []
    gate_errors = {}
    passed = set()
    records = 0
    executed = 0
    manifest_digest = None
    try:
        manifest, raw = read_object(corpus / "manifest.json")
        records = manifest_size(manifest)
        manifest_digest = hashlib.sha256(raw).hexdigest()
    except EVIDENCE_ERRORS as error:
        errors.append(f"corpus manifest: {error}")

    commit = None
    if isinstance(source_commit, str) and re.fullmatch(r"[0-9a-fA-F]{40}(?:[0-9a-fA-F]{24})?", source_commit):
        commit = source_commit.lower()
    elif receipts is not None or source_commit is not None:
        errors.append("a full expected source commit is required to evaluate receipts")
    if (receipts is not None or campaign_id is not None) and not identifier(campaign_id):
        errors.append("an expected release campaign ID is required to evaluate receipts")

    if receipts is not None:
        try:
            evidence, _ = read_object(receipts)
            gates = evidence.get("gates")
            if (evidence.get("schema") != "jani.release-evidence/v2" or not isinstance(gates, dict)
                    or not gates.keys() <= set(MANDATORY)):
                raise ValueError("expected a v2 signed release evidence bundle with known gate names")
            if trust is None or openssl_bin is None:
                raise ValueError("signed evidence requires --trust and --openssl-bin")
            root = (artifact_root if artifact_root is not None else receipts.parent).resolve(strict=True)
            untrusted_roots = (root, receipts.parent.resolve(strict=True), corpus.resolve(strict=True))
            for location in (trust, openssl_bin):
                if any(location.resolve(strict=True).is_relative_to(directory) for directory in untrusted_roots):
                    raise ValueError("trust settings and OpenSSL must be outside the receipt, artifact and corpus directories")
            settings, _ = read_object(trust)
            roots = Trust.from_record(settings, MANDATORY)
            if manifest_digest != roots.manifest_sha256:
                raise ValueError("corpus manifest does not match the operator-approved digest")
            if not errors:
                with tempfile.TemporaryDirectory(prefix="jani-release-check-") as workspace:
                    verifier = OpenSSLVerifier(openssl_bin, Path(workspace).resolve())
                    for gate in MANDATORY:
                        if gate not in gates:
                            continue
                        try:
                            artifact, batches = verify_gate(gates[gate], gate, commit, campaign_id, roots, verifier)
                            if gate == ORACLE_GATE:
                                if records < MINIMUM_RELEASE_CASES:
                                    raise ValueError("corpus is below the minimum release coverage")
                                executed = verify_execution(corpus, manifest, root, artifact, roots, batches)
                            else:
                                with checked_artifact(root, artifact):
                                    pass
                            passed.add(gate)
                        except EVIDENCE_ERRORS as error:
                            gate_errors[gate] = str(error)
        except EVIDENCE_ERRORS as error:
            errors.append(f"receipts: {error}")

    missing = [gate for gate in MANDATORY if gate not in passed]
    coverage_ok = ORACLE_GATE in passed
    return {
        "schema": "jani.readiness/v1",
        "ready": not errors and not missing and coverage_ok,
        "generated_cases": records,
        "minimum_release_cases": MINIMUM_RELEASE_CASES,
        "executed_cases": executed,
        "source_commit": commit,
        "campaign_id": campaign_id if identifier(campaign_id) else None,
        "corpus_manifest_sha256": manifest_digest,
        "oracle_evidence_sufficient": coverage_ok,
        "passed_gates": len(passed),
        "mandatory_gates": len(MANDATORY),
        "failed_or_missing_gates": missing,
        "gate_errors": gate_errors,
        "input_errors": errors,
        "disclaimer": (
            "The selected runners and reviewers attest to what occurred in their environments."
        ),
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--receipts", type=Path)
    parser.add_argument("--source-commit", help="full commit ID expected for every gate receipt")
    parser.add_argument("--campaign-id", help="operator-selected ID for this release campaign")
    parser.add_argument("--trust", type=Path, help="operator-controlled release trust settings")
    parser.add_argument("--artifact-root", type=Path, help="defaults to the receipt file's directory")
    parser.add_argument("--openssl-bin", type=Path, help="absolute path to a trusted OpenSSL executable")
    args = parser.parse_args()
    report = evaluate(args.corpus, args.receipts, source_commit=args.source_commit,
                      campaign_id=args.campaign_id, trust=args.trust,
                      artifact_root=args.artifact_root, openssl_bin=args.openssl_bin)
    print(json.dumps(report, indent=2))
    raise SystemExit(0 if report["ready"] else 2)


if __name__ == "__main__":
    main()

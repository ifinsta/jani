# Jani Lab — evidence-bound self-improvement experiments (v1)

**Status:** implemented partial; advisory evaluation only. This is **not** autonomous source rewriting, a sandbox executor or a production certification.

Jani Lab turns verified Mirror findings into testable hypotheses and compares independently signed benchmark results. It cannot run candidate code, sign a receipt, grant permissions, merge code, modify organizational policy, deploy an artifact or approve itself.

## Operator workflow

1. Run Jani on real tasks with its existing broker, policy and audit journal.
2. Review a finding with `jani mirror critique RUN_ID`.
3. Create an evidence-linked hypothesis: `jani lab propose RUN_ID`.
4. Run the independent `tools/lab_runner.py` outside the baseline, candidate and suite directories. Pin its SHA-256 digest from a trusted copy and supply a separate read-only Rust test suite (a Cargo workspace whose dependencies reference the target under `/work`). The runner snapshots and sanitizes source inputs, then executes three baseline/candidate pairs with a pinned OCI image, no network, read-only mounts, dropped capabilities and limits. Its output is a six-receipt document. **The pinned image must include an offline Rust 1.95 toolchain and dependencies.** A container reduces exposure; it does not establish perfect host isolation.
5. Give the verifier an Ed25519 private key inaccessible to Jani's model or candidate workspace. The verifier uses OpenSSL on its trusted host to sign results and does not mount the key in a trial container.
6. Evaluate the independently signed local JSON:

```bash
jani lab evaluate trial-pair.json --lab-key verifier:ED25519_PUBLIC_KEY_HEX \
  --state-dir .jani
```

The command requires the original Mirror finding to still be present in a verified Jani audit journal. Enterprise mode additionally requires a signed identity/policy permitting `lab.evaluate` on both `lab:tenant:TENANT_ID` (before the receipt is read) and `lab:run:RUN_ID` (for the named finding); `lab.propose` is separately granted on `lab:run:RUN_ID`.

The result is `review_candidate` only when all receipts are signed and the three paired trials demonstrate either a stable regression fix or at least 10% faster completion in **every** pair, with at least ten tests each and no loss of tested coverage. Otherwise it is `not_proven` or a hard validation error. Both results carry `requiresHumanApproval: true` and `autoApplied: false`.

## Trusted runner example

The sample paths must be **disjoint**. Keep the trusted runner in an operator-owned installation directory, not in either candidate checkout. The three source trees must contain no credentials intended to be supplied to the test container. The runner excludes common secret files but is not a complete data-loss-prevention system.

```bash
sha256sum /opt/trusted/jani/lab_runner.py
python3 /opt/trusted/jani/lab_runner.py \\
  --baseline /srv/lab/baseline --candidate /srv/lab/candidate \\
  --suite /srv/lab/immutable-regression-suite \\
  --image 'registry.example.org/rust-lab@sha256:REPLACE_WITH_64_HEX_DIGEST' \\
  --expected-tests 200 --timeout 900 \\
  --tenant local --source-run-id RUN_ID --finding-id MIRROR_FINDING_ID \\
  --scenario regression-01 --key-id verifier \\
  --signing-key /opt/trusted/keys/jani-lab-ed25519.pem \\
  --expected-runner-sha256 RUNNER_SHA256_FROM_TRUSTED_INSTALL \\
  --output /srv/lab/evidence/six-trials.json

jani lab evaluate /srv/lab/evidence/six-trials.json \\
  --lab-key verifier:ED25519_PUBLIC_KEY_HEX
```

The OCI image shown is a placeholder, not a published Jani runner image. A trusted operator must provide and verify the real image digest and independent regression suite. The test count is pinned separately: a failed compilation, unexpected test count, executor timeout or missing summary aborts the trial rather than producing a successful receipt.

The runner relies on Docker, OpenSSL and Python 3.11+ on a trusted host. It does not run as a Jani native tool, is not an autonomous background daemon, cannot deploy anything and has no embedded signing credentials.

## Scope of verification

`jani lab evaluate` verifies cryptographic provenance and **reported** matched outcomes. The verifier-side runner executes actual tests, but cargo test summaries are not a replacement for a fully authenticated inventory of individual test IDs, mutation testing, adversarial workloads or signed build provenance. A malicious candidate may try to influence its test output. Treat its result as a useful engineering signal requiring independent review, not as a security clearance.

## Signed trial format

`trial-pair.json` is one object with **exact** top-level keys:

```json
{
  "version": "jani-lab/v1",
  "tenantId": "local",
  "sourceRunId": "run-123",
  "findingId": "mir-0123456789abcdef01234567",
  "baseline": [
    { "proof": { "version": "jani-lab/v1", "tenantId": "local",
      "sourceRunId": "run-123", "findingId": "mir-0123456789abcdef01234567",
      "scenarioId": "matched-regression", "iteration": 0,
      "revisionSha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "suiteSha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "evaluatorSha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
      "environmentSha256": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
      "passed": 19, "failed": 1, "skipped": 0, "durationMs": 10000, "completed": true },
      "keyId": "verifier", "signature": "REPLACE_WITH_REAL_ED25519_SIGNATURE" }
  ],
  "candidate": []
}
```

This is an **illustrative shape, not a valid submission**. The real submission contains exactly three baseline and three candidate receipts, iterations 0, 1, 2, each with a distinct signature. Source finding IDs must match the verified run. Revisions, suite/evaluator/environment digests are 64 lowercase hexadecimal SHA-256 characters; the two revisions must differ. The verifier identity and test counts must be comparable across each pair. The signing key is never stored by Jani.

### Signature bytes (not JSON canonicalization)

To sign a proof, start with the literal UTF-8 bytes `jani-lab/independent-trial/v1` followed by one zero byte. Append each of the following fields in this order, encoded as a UTF-8 string preceded by its **unsigned 64-bit big-endian byte length**:

```text
version
tenantId
sourceRunId
findingId
scenarioId
iteration (decimal)
revisionSha256
suiteSha256
evaluatorSha256
environmentSha256
passed (decimal)
failed (decimal)
skipped (decimal)
durationMs (decimal)
completed ("true" or "false")
```

Sign the resulting bytes with Ed25519. Encode the 64-byte signature as lowercase hex in `signature`. `keyId` selects the independent trusted key passed as `--lab-key keyId:PUBLIC_KEY_HEX`; it is not itself a grant and is not part of the signed proof. Do not reuse policy-signing private keys as trial signer keys.

## Existing enforcement

- The command is operator-facing only; **no new model-callable tool** is registered.
- `lab.propose` and `lab.evaluate` are policy-gated read operations; in enterprise mode policies must explicitly grant them.
- Proposal sources come from a journal whose existing hash chain is verified by Mirror; cross-tenant findings are excluded.
- The evaluation refuses unknown fields, untrusted signing keys, invalid signatures, missing/replayed matching trials, unpinned environments, changed test totals, skipped-test regressions, unverified finding IDs and changed comparison inputs.
- Results are *review recommendations*, not authorization or rollout receipts. They do not claim to prove that no code was malicious or that the tests adequately cover the requirements.

## Still unimplemented

1. Production-hardened independent experiment runner with structured trusted test-ID inventories, adversarial workloads, fault injection, proven container confinement and measured cost budgets (the current external OCI runner is a prototype).
2. Trusted verifier identity lifecycle, issuer revocation, nonce replay cache and independent timestamp attestations.
3. Durable tamper-evident experiment registry, multi-run ranking, resource budgets, model-driven patch generation and feedback into the next planning cycle.
4. Signed human review clearance plus progressive promotion and rollback.
5. Reproducible, independently witnessed acceptance suites and an actual supervised trial on a live model session.

**Do not use this first slice to claim self-modifying autonomy or production readiness.** Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace --all-targets` in an environment with Rust 1.95 and dependencies available. A passing build alone does not substitute for the independent runner.

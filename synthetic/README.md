# Jani synthetic fault corpus — 5 million reproducible cases

This **testing input** exercises 23 mapped Jani subsystem domains, 371 distinct catalogued failure hooks, 32 mutation variants, 17 execution/deployment profiles, and two-/three-fault combinations. The default output is 50 gzip-compressed JSONL shards of 100,000 cases, plus integrity manifest and coverage report.

**Generated fixtures are not evidence Jani passed a test.** Every record says `replay.executed=false`. Test runners must implement fault hooks against a real Jani binary, execute them in disposable sandboxes and compare observed effects and independently verified receipts against the oracle. Some hooks describe as-yet-unimplemented capabilities. Oracle decisions are hypotheses for executable adapters to validate, not proven correctness. The "clean_valid_context" variant is an intended positive-control setup in which the primary fault hook is neutralized; other variants inject the catalogued failure.

## Usage

From repository root (Python 3.10+, standard library only):

```bash
python -m unittest discover -s synthetic/tests -v
python -m synthetic.generate sample --count 10
python -m synthetic.generate generate --output synthetic/generated/full_5m --count 5000000 --shard-size 100000 --seed 20261025
python -m synthetic.generate verify --output synthetic/generated/full_5m
python -m synthetic.readiness --corpus synthetic/generated/full_5m
```

`synthetic/generated/` is ignored by Git. No secrets, production credentials or personal customer information should be put into generated fixtures. Generator fails if a destination has existing shards to prevent cross-seed mixtures.

Each row provides deterministic unique ID, primary/secondary/tertiary fault hooks, domain/source path, mutation, profile, synthetic principal/tenant/agent/task/resource, time/concurrency/payload edge parameters, expected decision/security invariant and explicit unexecuted status. The corpus represents 11,872 primary fault × mutation templates before profiles and composite failures.

## Test campaigns and gates

1. **L0 — corpus integrity:** all rows, sequence and SHA-256 hashes; source mapping, valid schemas, reproducible seeds.
2. **L1 — subsystem behavior:** Rust unit/property tests across Jani's authority boundaries.
3. **L2 — true CLI integration:** signed fake IdP/MCP/provider, grants, policy and evidence receipts; independent observed outcomes.
4. **L3 — injected failures:** interruptions between effects and commits; stale leases, clock skew, filesystem and tool escape, queue floods.
5. **L4 — load/soak:** concurrent workers, high-memory/CPU/IO pressure, large audits and repeated cold recoveries.
6. **L5 — independent release acceptance:** security review, recorded worst-case latency/throughput, verified backup/restore and operator approval.

The separate `synthetic/readiness.py` evaluator **always fails closed** absent externally recorded execution receipts for all mandatory gates. Generated rows do not claim that Jani was executed, tested under load, or proven production ready.

## Readiness evidence requirements

A readiness receipt JSON must include `executed_cases`, `oracle_passed` and entries for all mandatory gates with `passed: true`, `source_commit` and `artifact_sha256`. Those fields must originate from trusted test infrastructure, not an LLM or the synthetic corpus; independent cryptographic verification and signoff remain an external requirement. The receipt schema is intentionally minimal and must be upgraded before regulated deployments.

See `docs/TESTING.md` for existing Jani testing. On host platforms without the required isolation backend, high-risk scenarios must be rejected—not downgraded to weaker confinement.

## Runtime integration status

The generated scenarios are intentionally independent of Jani execution. The initial
`synthetic/runner.py` supports only four safe CLI preflight hooks; unsupported
hooks are explicitly SKIPPED. The 371 named fault hooks are a campaign backlog,
**not 371 implemented runtime injectors**. To qualify for release, integrate every
high/critical relevant hook with an isolated executable adapter and collect
independently reproducible pass/fail evidence.

See [Readiness gates](READINESS_GATES.md) for the release campaign matrix.

---
id: JANI-EVAL-001
title: "Eval: measuring whether the agent finishes the job"
status: implemented-partial
owner: backend/server
version: 1.0.0
last-reviewed: 2026-11-25
source:
  - backend/server/src/eval.rs
  - eval/starter/suite.json
tests:
  - backend/server/tests/eval.rs
---

# Eval

A governed agent that cannot finish work is only a well-behaved one. `jani eval` measures completion on a suite of tasks, without asking the model whether it succeeded.

```
jani eval validate eval/starter                        # no model needed
jani eval run eval/starter --repeat 3 --out results/a \
    --exec smart --sandbox auto --model MODEL          # needs a model key
jani eval compare results/a/results.json results/b/results.json
```

## A case

```
<case>/case.json     {"prompt", "accept":[...], "hidden":[...], "writePaths":[...], "tags":[...], "repairRounds": N}
<case>/repo/         the starting files (a git repository is made from them for each run)
<case>/hidden/       copied in after the agent finishes, for the hidden checks
<case>/reference/    files that solve the task; used only by `validate`
```

- **Visible checks** (`accept`) are what the agent is told and can run. They also drive the repair loop of [DELIVERY.md](DELIVERY.md).
- **Hidden checks** (`hidden`) it never sees. A run is **solved** only if they pass on the delivered branch, run by the harness afterwards. Special-casing the visible test fails here (`passed_visible_failed_hidden`); there is a test for exactly that.
- **`writePaths`** are what the agent may edit. Keep the tests out of it.

## `validate`: no model, no trust

Proves each case is a fair question: the visible checks fail before any work, the hidden checks fail before any work (they test something), and the reference solution passes both. A case that cannot fail or cannot be solved is reported and the command exits non-zero. Run it whenever you add a case.

## `run`

Each run gets a fresh repository and state directory under `--out`, then goes through the full delivery loop. Per run it records: outcome, solved, whether tests were touched, rounds used, tokens in and out, tool calls, denials, commands refused for lack of an operator, seconds, and a link to the delivery report. `results.json` holds all of it; `report.md` summarises.

Metrics:

- **solved rate** (pass@1): mean over all runs.
- **pass@n**: fraction of cases solved at least once across `--repeat`.
- **clean**: solved without changing a test file.
- **failure causes**: `visible_checks_failed`, `passed_visible_failed_hidden`, `asked_instead_of_finishing` (the agent asked a question nobody could answer; see [ASK.md](ASK.md)), `harness_or_model_error`.

The eval uses `--exec smart` unless you pass another mode. With no sandbox, builds and tests are *refused* (no operator to ask), which understates the model; the report says so. Use `--sandbox auto` where bubblewrap is available.

## The starter suite

`eval/starter` has eight small Python tasks (five bug fixes, two features, one multi-file rename) with unittest checks. It is deliberately easy to run and cheap, so it can be repeated. It measures whether the loop works end to end and ranks models roughly; it is not a benchmark of general engineering ability. Add cases from your own repositories to measure what you care about.

## What this has and has not shown

The harness is verified with scripted models only: one that applies the reference (scores 100%), one that claims success and changes nothing (0%), one that overfits the visible test (visible passes, hidden fails, not solved), and repeats with mixed results. **No result from a real model is recorded here.** The completion rate of any real model on this suite has not been measured; run it to find out.

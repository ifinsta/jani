---
id: JANI-DELIVERY-001
title: "Delivery: work that someone other than the model checked"
status: implemented-partial
owner: backend/server
version: 1.0.0
last-reviewed: 2026-11-25
source:
  - backend/server/src/deliver.rs
tests:
  - backend/server/tests/deliver.rs
---

# Delivery

`jani --print` ends when the model stops talking. `jani deliver` ends when the work has been checked by something that is not the model.

```
jani deliver "make add() correct" \
  --accept "python3 -m unittest -q" \
  --allow-write --write-path calc.py \
  --exec smart --sandbox auto
```

1. **Isolate.** In a git repository with a commit, the work happens on a new branch (`jani/<id>`, or `--branch`) in a worktree under `<state-dir>/deliveries/<id>/tree`. The operator's checkout is never touched. Without git it happens in place (`--no-worktree` forces that).
2. **Work.** The agent runs as it always does: same policy, sandbox, execution modes, consent prompts and journal. The prompt tells it the acceptance checks, to run them itself, and not to change the tests.
3. **Verify, independently.** After the agent finishes, the **host** runs every `--accept` command (argv, no shell; in the same sandbox profile as the session). The model cannot run, skip, edit or report them: they are not a tool. Each is journaled as `acceptance.run` (`acceptance_passed` / `acceptance_failed`).
4. **Repair.** If a check fails, its command, exit status and output tail go back to the agent as a new turn of the **same conversation**, up to `--repair-rounds` (default 3).
5. **Commit.** Changes inside the operator's `--write-path` scope are committed on the branch with the outcome in the message. Only the write scope is staged, so build output and stray files are never committed. Repository hooks and fsmonitor are disabled for the host's git calls, because they would run repository code unconfined. `--no-commit` leaves the changes uncommitted.
6. **Publish (optional).** `--push REMOTE` pushes the branch; `--pr` then opens a pull request with `gh pr create`. Neither happens unless you ask.
7. **Report.** `<state-dir>/deliveries/<id>/report.md` and `report.json`.

## Outcomes

| Outcome | Meaning | Exit |
|---|---|---|
| `verified` | At least one check was declared and every check passed on the last round | 0 |
| `unverified` | No `--accept` given. Only the agent's own word | 0, with a warning on stderr |
| `failed` | Checks still failing after the last round, or the agent's turn failed | non-zero |

A model that says "all tests pass" without fixing anything is `failed`: the claim is quoted in the report, not believed (there is a test for exactly this).

## What the report flags

- Every check on every round, with its result and time.
- The diff against the base commit.
- **Files that look like tests that the agent changed.** The cheapest way to pass a check is to weaken it. A `verified` delivery that edited tests says so, in bold, and `testFilesChanged` is in the JSON. The strong defense is to keep the tests out of `--write-path`; the flag is the backstop.
- Tokens, the number of rounds, and whether the audit chain verifies.

## Limits

- "Verified" means the commands exited 0. It is only as good as the checks.
- Acceptance runs on the host's toolchain. If you use `--sandbox auto`, the worktree is read-only apart from the write scope and build output directories, and there is no network, so checks that write elsewhere or download will fail.
- Python caches bytecode by source mtime (seconds) and size. A repair that rewrites a file within the same second and byte length can run stale code. Set `PYTHONDONTWRITEBYTECODE=1` for the checks if you hit this.
- `--pr` needs `gh` installed and logged in, and was only tested against a stand-in `gh`. Reacting to CI results after the pull request is open is not implemented.
- The worktree is left in place for review. Remove it with `git worktree remove <path>`.

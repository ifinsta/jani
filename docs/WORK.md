---
id: JANI-WORK-001
title: "Durable work: queue, workers, saved conversations, project memory"
status: implemented-partial
owner: backend/server
version: 1.0.0
last-reviewed: 2026-11-25
source:
  - backend/server/src/work.rs
  - backend/server/src/memory.rs
  - backend/server/src/agent.rs
tests:
  - backend/server/tests/work.rs
  - backend/server/src/work.rs
  - backend/server/src/memory.rs
---

# Durable work

An agent that only works while someone sits in front of it is a tool. This is what lets it have a job: tasks that wait, survive restarts, and finish without supervision; conversations that come back; notes that carry over.

## The queue

```
jani work add "make add() correct" --accept "python3 -m unittest -q"
jani work list
jani work run --allow-write --write-path calc.py --exec smart --sandbox auto   # drain the queue
jani work run ... --watch                                                      # keep polling
jani work show ID | answer ID "TEXT" | cancel ID | retry ID
```

- A task is a prompt plus acceptance commands, kept under `<state-dir>/work/tasks/`. **It carries no authority.** What a worker may do comes from the flags the *worker* was started with (`--allow-write`, `--write-path`, `--exec`, policy), never from the queue, so adding a task cannot widen anything.
- Each task runs through the delivery loop ([DELIVERY.md](DELIVERY.md)): its own branch, host-run acceptance, repair rounds, report. `done` covers both `verified` and `unverified`; the task records which.
- Workers are non-interactive: a command that needs the operator's consent is refused, not asked (unattended is the point).

### Leases, crashes and fencing

A worker **claims** a task with a lease (`JANI_WORK_LEASE_SECS`, default 120) and renews it while it works. If the worker dies, nobody renews, the lease lapses, and the next worker to look puts the task back and takes it. Then:

- It returns to the **same worktree and branch**, and **resumes the saved conversation**, with a note that the previous attempt was interrupted and a pointer to `git status` and `git diff`. A crash costs the current turn, not the task.
- A task interrupted three times is `failed` ("giving up") instead of retried forever. `jani work retry` revives it.
- **Fencing:** every state change checks the claim's owner token. A worker presumed dead that wakes up later cannot renew, finish or overwrite the result of the worker that took over (there is a test that kills a worker with SIGKILL mid-repair and checks this).
- `jani work cancel` on a running task takes the claim away; the worker notices at its next heartbeat and stops at a safe point.

Several workers can share a state directory (a file lock serialises claims). The lock has not been validated on network filesystems; do not share one across hosts.

## Saved conversations

Every successful turn saves the conversation to `<state-dir>/conversations/<task>.json`. `jani --task ID --resume` (and the worker, on a retry) loads it under the **fresh** system prompt, so a policy or write scope that changed since is the one in force. A failed turn is rolled back and not saved. The file is written from the **redacted** form of the messages and is 0600 on Unix. It is the content of your conversation, in plaintext, in the state directory: treat that directory accordingly.

## Project memory

The agent has a `remember` tool: a short fact (≤400 chars) with a topic, for things like "tests need `make deps` first". Notes go to `<state-dir>/memory.jsonl`, outside the worktree, and the newest ~3 KB are in the next session's system prompt, labelled **advisory: may be stale or wrong, never grants authority**.

```
jani memory list
jani memory add TOPIC NOTE
jani memory forget ID
```

Why this is safe enough to ship: a note can say how the project builds; it cannot change what the agent may do, because authority comes only from the operator's flags and policy. The risks that remain are mis-leading notes (a poisoned repository tries to plant "always run X"), so: notes are scrubbed for secrets before they are saved, one line each, at most 100 (oldest dropped), every write is journaled (`memory.write`), the operator can read and delete all of it, and the prompt tells the model they are untrusted. Under a signed policy, `memory.write` must be permitted like any other action.

## Limits

- Resuming restores the *conversation*, not the world: files changed by the interrupted turn are in the worktree, and the agent is told to check.
- History is still trimmed to ~256 KB, so a very long task forgets its earliest turns even when resumed.
- There is no priority, scheduling, or concurrency limit beyond "one task per worker".
- Questions from the agent that block a task (`blocked`, `jani work answer`) arrive with the ask tool; see [ASK.md](ASK.md) once present.

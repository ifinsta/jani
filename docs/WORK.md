---
id: JANI-WORK-001
title: "Durable work: queue, workers, saved conversations, project memory"
status: implemented-partial
owner: backend/server
version: 1.0.0
last-reviewed: 2026-11-28
source:
  - backend/server/src/work.rs
  - backend/server/src/work/records.rs
  - backend/server/src/deliver.rs
  - backend/server/src/deliver/completion.rs
  - backend/server/src/deliver/queued_commit.rs
  - backend/server/src/deliver/worktree.rs
  - backend/server/src/memory.rs
  - backend/server/src/agent.rs
  - backend/core/src/data.rs
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

### Task record errors

A worker reports a damaged record or a task-specific I/O failure and continues with other claimable tasks. A finite run exits nonzero if it encountered a task record fault. A watch worker keeps polling and reports a stable damaged record once while it remains unreadable. `--once` still stops after one delivery. A saved acceptance command with invalid syntax is marked failed before delivery begins.

`work list` reports an invalid record as an error. `work show ID` reads one record so that healthy tasks remain inspectable. Invalid committed records remain on disk for diagnosis. Generated task IDs and the existing task file format are preserved.

Place queue records and locks in ordinary local directories. The queue refuses redirected record paths and lock directories.

### Leases, crashes and fencing

A worker **claims** a task with a lease (`JANI_WORK_LEASE_SECS`, default 120) and renews it while it works. If the worker dies, nobody renews, the lease lapses, and the next worker to look puts the task back and takes it. Then:

- It returns to the **same worktree and branch**, and **resumes the checkpointed conversation**, with a note that the previous attempt was interrupted and a pointer to `git status` and `git diff`. A crash costs the current turn, not the task.
- Three expired leases exhaust the interruption budget. Answering a question keeps that budget intact. `jani work retry` resets the budget and resumes an existing delivery.
- The worker records the repository, scope, branch and starting commit before asking Git to create the checkout. On retry it checks the live Git registration and directory. A clean legacy checkout can be recovered without workspace metadata. A conflicting branch, missing ready checkout or ambiguous partial checkout requires operator review.
- Renewing or finishing requires a current claim token and an unexpired lease. The conversation checkpoint is also fenced by epoch. A worker holds a local task lock while using its worktree and report. Other workers skip that task while the lock is held and can claim unrelated work. A replacement acquires the task lock before changing an expired claim or spending its interruption budget.
- `jani work cancel` revokes the running claim. The worker notices at its next heartbeat and checks for cancellation before starting another acceptance check or finalizing the delivery. Operations already in progress may continue to their next safe point.

Several workers running the same queue protocol can share a local state directory. Stop workers before upgrading from a version that predates per-task execution locks. The lock has not been validated on network filesystems.

### Finishing an interrupted delivery

For a queued linked worktree, the worker prepares a local commit from the scoped paths in a separate Git index. It saves the exact commit and the acceptance results for the current delivery generation, then records their digest in the journal before updating the branch. The update requires the recorded parent. An interrupted worker can finish publishing the same commit, audit result, and reports from that evidence before completing the queue entry. Model requests and acceptance checks stay in the original run. The worker checks its claim, identity, policy, scope and kill switch during recovery. Unrelated staged entries remain in the real index.

A lease interruption keeps the saved generation eligible for recovery, even if it exhausts the normal interruption budget. Retrying a cancellation or failure during local finalization also keeps that generation. Answering a question or retrying a recorded failed outcome starts a new generation. Missing or inconsistent evidence, a moved branch or changed worker authority requires reconciliation.

This recovery covers local finalization after the journaled intent. A crash earlier in model or tool execution, remote side effects and an initial commit in an in-place Git checkout have separate recovery limits.

## Saved conversations

Conversations are checkpointed in `<state-dir>/tasks/state.sqlite3` after each turn, while waiting for an approval or answer, and when a turn fails or is cancelled. `jani resume --task ID "prompt"` restores a conversation. The store reads the checkpoint and claims its new epoch in one transaction. An older worker cannot overwrite the newer checkpoint. Queue recovery starts a new conversation only when the checkpoint is absent. It reports corrupt and killed checkpoints as errors.

Two properties added on top of the task store:

- **The checkpoint is redacted.** Local storage scrubs detected secrets even when the egress policy refuses to send the conversation to a model. If verification still detects a secret, the checkpoint keeps task metadata and omits the conversation. Prior checkpoint files retain the contents written by their original binary.
- **A resumed conversation runs under the fresh system prompt**, so a policy, write scope or project note that changed since the checkpoint is the one the model is told.

The checkpoint is the content of your conversation in a database in the state directory: treat that directory accordingly.

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
- Questions from the agent that block a task (`blocked`, `jani work answer`) are described in [ASK.md](ASK.md).

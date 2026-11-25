---
id: JANI-ASK-001
title: "Ask before guessing"
status: implemented-partial
owner: backend/server
version: 1.0.0
last-reviewed: 2026-11-25
source:
  - backend/server/src/broker.rs
  - backend/server/src/consent.rs
  - backend/server/src/agent.rs
tests:
  - backend/server/tests/ask.rs
  - backend/server/tests/ui.rs
  - backend/server/tests/pty/tui_e2e.py
  - backend/server/tests/pty/line_e2e.py
---

# Ask before guessing

A good colleague asks "which of these two did you mean?" before spending four hours on the wrong one. The agent has an `ask` tool for that: a question (≤500 characters) with up to six suggested answers (≤100 characters each). The system prompt tells it when to use it (the request is ambiguous in a way that changes what it builds, or the decision is the operator's) and when not to (anything it can learn by reading the code or running the tests, and small details: take a default and say so).

`ask` is a read-level action (`agent.ask`): it changes nothing, so it needs no write or exec grant. Under a work contract or signed policy it must be permitted like any other action, as must `memory.write`; a contract that lists neither will refuse them and the model carries on without.

## What happens next depends on who is there

| Where the agent runs | The question |
|---|---|
| **Web workspace** | A card in the terminal: the question, one button per option (keys `1`..`9`), a text box for your own words, and **Leave for later**. Cancelling the turn leaves it unanswered. |
| **Terminal UI** | A panel above the composer: press a digit to pick an option, or type your own words and press Enter; Esc leaves it for later. A digit typed mid-sentence is text, and choosing or sending is only possible while the panel is fully on screen (a clipped layout disables it; Esc still declines). |
| **`--print` and the line REPL**, with a person at a terminal | Asked on stderr, answered from the terminal itself (as the consent prompt is): a number picks an option, anything else is your words, an empty line leaves it for later. |
| **Nobody there** (CI, pipes, `jani work run`) | The question is recorded and **the turn ends on it**. Nothing more is guessed. |

When a question is left unanswered, the turn's result is the question itself ("I need your input before I continue: ..." with the options), and the journal records the run as completed with reason `needs_input`.

## In the work queue

A worker that meets an unanswered question marks the task **`blocked`**, keeps its worktree and saved conversation, runs **no acceptance checks and commits nothing**, and moves on to the next task. A blocked task is not claimed again.

```
jani work list                       # shows blocked tasks with their question and options
jani work answer ID "in place"       # answer in your own words, or paste an option
```

The answer puts the task back in the queue. The next worker resumes the conversation (the question and its "waiting" result are in it), is given the answer, and carries on.

## In evals

An agent that asks instead of finishing is scored as not solved, with the failure cause `asked_instead_of_finishing`, because an unattended eval has nobody to answer. Look at those cases: either the task description is genuinely ambiguous (fix the case) or the model is asking too much (fix the prompt or the model).

## Limits

- The agent decides when to ask; nothing forces it to. A model that never asks will guess as before. Measure with a suite that contains deliberately ambiguous tasks.
- Questions and answers are text. There is no form, attachment, or follow-up thread.
- The answer is the operator's word and goes to the model as a tool result (secret-scrubbed like any tool output).

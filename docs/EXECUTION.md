---
id: JANI-EXEC-001
title: "Command execution"
status: implemented-partial
owner: project
version: 1.0.0
last-reviewed: 2026-11-24
source:
  - backend/server/src/execpolicy.rs
  - backend/server/src/broker.rs
  - backend/server/src/consent.rs
  - backend/server/src/tui.rs
tests:
  - backend/server/src/execpolicy.rs
  - backend/server/src/broker.rs
  - backend/server/tests/e2e.rs
  - backend/server/tests/ui.rs
---

# Command execution

An agent that can only read files cannot do engineering, and one that can run anything is a liability. `--exec` is the operator's choice between them. Authority is the operator's: the model, the worktree and the settings files can lower it, never raise it.

| `--exec` | What the model may run |
|---|---|
| `off` (default) | Nothing. |
| `allowlist` (also `--allow-exec`) | The fixed list: `cargo test/check/clippy/fmt --check`, a few git commands, and runners named with `--exec-program`. Nothing else. |
| `smart` | Classified per command, below. |

## Smart mode

Commands are an argv list, never a shell string, so there is no `a && b`, redirection or expansion to reason about. Each command is classified, in this order:

1. **Hard deny.** Never run, never asked, no rule or answer can grant it: privilege escalation (`sudo`, `su`, `runas`), disk and system tools (`mkfs`, `dd`, `shutdown`, `reg`, `net`, `mount`), `rm` of anything outside the worktree, force-push or mirror-push, publishing (`cargo publish`, `npm publish`, logins).
2. **Operator deny rules.** A prefix you denied.
3. **The fixed list** (as `allowlist`, including its `git add`/`git commit` scope checks).
4. **Operator allow rules and session grants.**
5. **Safe inspection runs on its own**, with or without a sandbox: `ls`, `cat`, `head`, `tail`, `wc`, `grep`, `rg`, `fd`, `find`, `tree`, `diff`, `sort`, `stat`, `git status/diff/log/show/blame/ls-files...`. Every path argument must stay inside the worktree (no absolute paths, `..`, `~`, drive letters). Flags that make a reader write or run something are refused (`find -exec/-delete`, `sort -o`, `rg --pre`, `git diff --output`).
6. **Builds and tests run on their own only when a sandbox confines them**: `cargo build/check/test/clippy/run`, `go build/test`, `npm|pnpm|yarn run/test`, `make`, `pytest`, `python -m pytest`, `mvn`, `gradle`, `dotnet build/test`, compilers. "Confined" means the sandbox makes the worktree read-only apart from the write scope and cuts the network (bubblewrap on Linux, `--sandbox auto`). Running project code unconfined is exactly as safe as the project, so without a sandbox these ask first. Trust follows containment, not a name. A build gets its output directory (`target/`, an existing `build/`, `dist/`...) writable and nothing else; the sandbox, not the classifier, is what stops `make leak` from writing `leak.txt`.
7. **Everything else asks the operator**: package installs, scripts (`python script.py`, `node server.js`), file mutators (`rm`, `mv`, `touch`, `chmod`), shells and wrappers (`sh -c`, `env`, `timeout`, which hide the real command), a program named by a path (`./ls` is never mistaken for `ls`), git writes. Network tools are *refused* when the sandbox has no network, since asking would be pointless.

## Who answers, and how

- **Web workspace (`jani ui`).** A card appears in the terminal: **Allow once** (`y`), **Allow for this session** (`s`), **Always allow "<prefix>"** (`a`), **Deny** (`n`). The agent waits (15 minutes, or until you cancel the turn, which answers no). A decline tells the model not to try a variant.
- **Terminal UI (`jani` / `jani repl` on a terminal).** A panel opens above the composer with the command and why it needs you: `y` once, `s` this session, `a` always (saves a rule), `n` or Esc to deny. Other keys are ignored while it is open, so nothing you were typing answers it. Ctrl+C cancels the turn and denies. Each answer is also written into the transcript.
- **`--print` and the line-based REPL** (used when stdout is not a terminal), when a person is at an interactive terminal: the question goes to stderr and the answer is read from the terminal itself, as `sudo` and `ssh` do (Unix: `/dev/tty`, or stdin reopened when there is no controlling terminal; Windows: the console input buffer `CONIN$`, and for terminals that are not consoles, such as mintty, stdin), so stdout stays clean for the result and a line the REPL or a script was about to read is never taken as an answer. `y` once, `s` session, `a` always, `n` or Enter deny; end of input and three unrecognised answers deny.
- **No terminal to ask** (CI, pipes, `echo task | jani --print`): the command is not run and the model is told the operator can allow it with `jani exec allow`. It is never silently dropped or silently run.
- **Under a signed policy bundle** (managed mode), an unclassified command needs a *signed approval from a second person* (class `exec-unclassified`), exactly like any other approval. A keyboard answer does not stand in for one.

"Always" writes an operator rule to `<state-dir>/exec-rules.json`, recording who made it. Rules are prefix matches on whole tokens (`npm install` matches `npm install react`, not `npm installer`) and are never created by the model or by the settings files.

```
jani exec rules                          # list, numbered
jani exec allow -- cargo build           # allow a prefix
jani exec deny  -- npm install evil-pkg  # deny beats allow
jani exec forget 2                       # remove rule 2
jani exec check --exec smart --sandbox auto -- cargo test
                                         # what would happen, and why
```

A rules file that cannot be read stops all commands rather than reading as "no rules", because a corrupt file could be hiding a deny.

## Audit

Every command's journal event carries *why* it ran or was refused: `exec_allowlist`, `exec_safe_inspection`, `exec_confined_build`, `exec_operator_rule`, `exec_session_grant`, `exec_operator_once`, `exec_operator_session`, `exec_operator_always`; and `exec_policy_denied`, `exec_consent_required`, `exec_operator_declined`, `exec_rules_unreadable`, `exec_approval_required` for the refusals.

## Limits to know

- The classifier is a policy, not a proof. The strong boundary is the sandbox; without one, only the inspection tier and what you explicitly allow run unattended.
- Windows has no filesystem-confining backend (Job Objects limit resources only), so builds ask there until you allow them (`jani exec allow -- cargo build`, or "Always" in the card).
- Rules live in the state directory; an unconfined build or test running as you could edit that file. Under a confining sandbox it cannot. If that matters, run builds sandboxed.
- A build tool's first run needs its dependencies already present: the sandbox has no network. `make`-style projects need their output directory to exist (or `--write-path`).
- **Windows prompt: compiled and linted, not run.** The `CONIN$` and stdin-fallback paths build and pass clippy `-D warnings` for `x86_64-pc-windows-gnu`, and the answer parser is tested with Windows line endings and a stray Ctrl+Z, but no Windows console was available to exercise them. The Unix path is exercised by `backend/server/tests/pty/`. The terminal UI (crossterm) and the web card do not depend on any of this.

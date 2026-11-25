# Jani web workspace (`jani ui`)

**Status:** implemented for one trusted local machine; supervised prototype. It is a browser view over the state the CLI already reads and writes. It adds no new authority, no remote access and no hosted control plane.

`jani ui` serves a single-page workspace on `127.0.0.1`. It shows a run as a graph of the broker's decisions, an inspector with the journal events behind each node, approvals, the audit journal, budgets, and an interactive terminal that runs the same session as bare `jani`.

```bash
jani ui --repo . --state-dir .jani                    # read-only authority
jani ui --repo . --allow-write --write-path src       # same flags as `jani repl`
```

The command prints one URL. Open it as printed, including the part after `#`:

```text
Jani workspace: http://127.0.0.1:41873/#token=…
```

`--port N` selects a fixed port; the default is an ephemeral one.

## Layout

The workspace follows the layout of a code editor. A **title bar** has menus (View, Go, Help), a command box, and toggles for the sidebar, panel, inspector and focus mode. An **activity bar** of icons on the left opens each screen as a closable **editor tab** beside the always-present *Run graph* and *Terminal* tabs. The **sidebar** (runs, authority, model and identity, skills; collapsible sections) and the right-hand **inspector** are optional, as is the bottom **panel** (approvals, audit journal, budget). A **status bar** shows the worktree, mode, authority, journal chain state and budget. Panes can be dragged to resize, and the arrangement is remembered in the browser.

| Shortcut | Action |
| --- | --- |
| Ctrl+Shift+P or Ctrl+K | Command palette: open any screen, switch run, toggle panes, run `/status` |
| Ctrl+B | Sidebar |
| Ctrl+J | Panel |
| Ctrl+Shift+Z | Focus mode: hide everything but the editor |

By default the sidebar, inspector and panel are closed so the graph and terminal have the screen: at 1919×884 the editor column is 89% of the window and the graph canvas 82%.

## Screens

A tab strip switches between ten screens. **Workspace** is the run graph, inspector, terminal and the approvals, journal and budget docks. The other nine are full-page views, each backed by the subsystem the matching command already uses, run with this process's own configuration, identity and policy.

| Screen | Same data as | What it adds |
| --- | --- | --- |
| Workspace | `inspect`, `evidence`, `approvals`, `budget`, bare `jani` | Run graph of broker decisions, per-node journal events, approvals with detail rows, a searchable journal (by text, decision, run; pageable to 1,000 events), budget meters, a live session. |
| Recovery | `recovery status` | Runs that did not finish cleanly and why none is ever "safe to replay". |
| Policy | `policy explain`, `policy check`, settings | Effective policy, data-egress rules, budgets, rules, model profiles, and a "would this be allowed?" checker that evaluates without running or journaling anything. |
| Evidence | `evidence RUN_ID`, `audit verify` | Receipts for the latest 30 runs, chain verification, and a JSON export of the receipt plus the run's events. |
| Mirror | `mirror workflows`, `recommendations`, `history`, `critique`, `compare` | Rule-based findings, per-run critique with timeline, and descriptive run comparison. `mirror record` is not offered. |
| Agents | `agent status`, `alerts list` | Agents with lifecycle phase and the alert queue. Read-only. |
| Lab | `lab propose` | Hypotheses linked to evidence. `lab evaluate` stays on the command line because it needs an independently signed file. |
| System | `sandbox doctor`/`check`, `plugins`, MCP and key configuration | What each confinement backend actually confines, plugin verification, key counts (never key material). |
| Native tools | `native TOOL OPERATION` | The twelve tools and their operations. Only operations that neither change state nor are operator-only can be run. |
| Updates | `update check` | Release source and version. The check is an outbound request and runs only when you press the button. |

Journal events record a resource **digest**, not the path or command text, so the graph shows digests and reason codes. It does not reconstruct file names the journal never stored. The journal has no per-call model events, so the agent node reports tool calls and the model node reports profile switches. Operations that share the journal but never start an agent run (native tools, for example) are not listed as runs.

Even a read-only native operation is governed: it passes policy and identity and records `started` and `completed` events in the journal.

## What stays on the command line

The browser never offers: `mirror record`, native operations that change state (`index build`, `context pin`/`assemble`, `planner create`/`report`, `pruner run`, `checkpoint create`, `workspace snapshot`, `evidence record`), operator-only operations (`planner verify`, `workspace rollback`, `verifier run`), agent registration and alert send/claim/resolve, `lab evaluate`, `update install`, and anything that would change authority or policy.

## Authority is the process's authority

Scopes are frozen when `jani ui` starts, exactly as in the terminal session. The page cannot grant write or command rights, change policy or widen a budget. To change authority, restart with different `--allow-write`, `--write-path`, `--allow-exec`, `--exec-program`, `--policy`, `--identity` or `--contract` flags.

## Approvals keep separation of duties

A tool call that needs approval creates a request and stops. Nothing runs until a **different, verified** principal decides it and the requesting session attaches it with `/approval ID`.

- The browser can decide a request only when the process was started with a verified identity (`--identity FILE --identity-key ID:HEX`). Otherwise the request is refused with HTTP 403.
- The store still enforces its own rules: the requester cannot approve their own action, the role must match, and expired requests are refused. The UI surfaces the refusal text.
- In practice, run one `jani ui` as the operator and a second, on another port and with the approver's identity, against the same `--state-dir`.

## Trust boundary

- Binds `127.0.0.1` only. There is no flag to listen elsewhere.
- Every `/api/` call needs the random per-process token in `X-Jani-Token`. It is printed in the URL fragment so it never reaches a server log or `Referer`, and the page removes it from the address bar after reading it.
- The `Host` header must be exactly `127.0.0.1:PORT` or `localhost:PORT`, which blocks DNS rebinding.
- `POST` requires `Content-Type: application/json` and, if an `Origin` is sent, the same origin.
- The page is served with a nonce-based Content-Security-Policy, `default-src 'none'`, `connect-src 'self'`, no third-party hosts and no framing.
- Requests are bounded: 8 KiB per line, 64 headers, 64 KiB per body, 32 concurrent connections, 10 s read timeout.
- Transcript and journal text is rendered as text, never as HTML.

Anyone who can read the printed URL, or who can run code as your user on the same machine, can use the workspace with the authority the process was started with. Treat the URL like a credential. This is a convenience for a single operator, not a multi-user service.

## Limits

- No authentication beyond the token; no TLS; no remote or multi-user mode. `jani ui` itself stays loopback-only by design. A hosted, multi-user prototype is a separate front end, [jani-cloud](../cloud/README.md), that signs people in with ifinsta and runs one `jani ui` per person behind it.
- One agent session per process. A turn that is already waiting on the model cannot be preempted; Esc cancels cooperatively between model calls and tool calls.
- The graph shows 40 tool calls at a time with a pager for earlier ones. The journal view loads 200 events at a time, up to 1,000 matches; narrow with the filters beyond that.
- Verification: `backend/server/tests/ui.rs` covers the trust boundary, the refusals above, paging and filtering, and the real session wiring. Browser behaviour was exercised by hand against a scripted local model provider, in light and dark, at desktop and 400 px width. Not exercised: Windows, macOS, a live model provider, enterprise mode with a signed policy bundle, and an executable plugin (none exist).

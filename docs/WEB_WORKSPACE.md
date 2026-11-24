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

## What each view reads

| View | Source | Notes |
| --- | --- | --- |
| Runs, graph, Events | `journal.jsonl`, grouped by `run_id` and `invocation_id` | The journal chain is verified on every read. A chain that fails verification is refused, not shown. |
| Receipt | `jani evidence RUN_ID` | Carries the same caveat: audit integrity is not independent proof that the outcome is correct. |
| Approvals | the approval store in `--state-dir` | Shows capability, resource digest, risk, requester, required role, status and expiry. |
| Budget | one ledger per task under `budgets/` | Tool calls, tokens and spend against the persisted limits. |
| Authority, model, identity | the process's own configuration | The API key is never returned; only whether one is configured. |
| Terminal | one live agent session | The same slash commands as the terminal UI: `/help`, `/status`, `/model`, `/skills`, `/inspect`, `/budget`, `/approvals`, `/approval ID`, `/new`, `/exit`. |

Journal events record a resource **digest**, not the path or command text, so the graph shows digests and reason codes. It does not reconstruct file names the journal never stored. The journal has no per-call model events, so the agent node reports tool calls and the model node reports profile switches.

## Authority is the process's authority

Scopes are frozen when `jani ui` starts, exactly as in the terminal session. The page cannot grant write or command rights, change policy or widen a budget. To change authority, restart with different `--allow-write`, `--write-path`, `--allow-exec`, `--exec-program`, `--policy`, `--identity` or `--contract` flags.

## Approvals keep separation of duties

A tool call that needs approval creates a request and stops. Nothing runs until a **different, verified** principal decides it and the requesting session attaches it with `/approval ID`.

- The browser can decide a request only when the process was started with a verified identity (`--identity FILE --policy-key ID:HEX`). Otherwise the request is refused with HTTP 403.
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

- No authentication beyond the token; no TLS; no remote or multi-user mode.
- One agent session per process. A turn that is already waiting on the model cannot be preempted; Esc cancels cooperatively between model calls and tool calls.
- The graph shows the latest 40 tool calls of a run. The journal view shows the latest 500 events.
- Verification: `backend/server/tests/ui.rs` covers the trust boundary and the real session wiring. Browser behaviour was exercised by hand against a scripted local model provider; no live-provider engineering session is recorded.

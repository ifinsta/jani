# jani-cloud

A prototype that runs Jani as a hosted coding assistant in **one container**,
signing people in with ifinsta (`ifinsta/siwi` against `ifinsta/accounts`).

> **Status: prototype.** It has been exercised end to end against a real
> `accounts-server`, but it is not production ready. See
> [what is and is not verified](#what-is-and-is-not-verified).

## How it works

```text
browser ──https──▶ reverse proxy (TLS) ──▶ jani-cloud :8080
                                              │  /siwi/*   siwi: sign in with ifinsta (BFF, server-side session)
                                              │  /         signed out: sign-in page   signed in: the person's workspace
                                              │  /api/*    signed in only; same-origin enforced
                                              ▼
                              one `jani ui` per person, on 127.0.0.1, own token
                              own worktree + state + audit journal, own Unix user,
                              optionally inside bubblewrap
```

- **Login** is `siwi`: the standard ifinsta modal (passkey, password, sign-up),
  with popup and full-page fallbacks. Tokens stay on the server.
- **One address.** `/` is the sign-in page when signed out and the workspace
  when signed in, so signing in is just a reload.
- **The workspace token never reaches the browser.** The gateway adds it, drops
  cookies, rewrites `Origin`, and forwards only `/` and `/api/*`.
- **Journal attribution.** Each workspace is started with
  `--principal ifinsta-<24 hex of sha256(sub)>`, so every audit event names a
  stable pseudonym, never an email.
- **Authority is the operator's.** Flags in `JANI_CLOUD_JANI_ARGS` apply to every
  person. The default is read-only. The browser cannot widen it.
- **Idle workspaces stop** (default 30 minutes; a hidden tab does not count as
  activity). Files and journal stay; the next visit starts a fresh process over
  the same directory. Signing out stops it immediately.
- **No workspace outlives the gateway.** Graceful shutdown stops them all, and on
  Linux each child also dies with its parent even if the gateway is SIGKILLed.

## Register Jani with accounts

Jani is a new ifinsta service. On the accounts host:

```bash
ACCOUNTS_ISSUER=https://accounts.example.com/oidc accounts-server register-service jani \
  https://jani.example.com --client-id jani-cloud \
  --frame-origin https://jani.example.com --logout-path / --format json
```

The output contains the client secret; keep it secret. `--frame-origin` is what
allows the embedded sign-in modal on your origin. Copy the printed values into
the environment. **accounts prints `SIWI_*` names; this build reads the
`IFINSTA_*` names** from the siwi/1 profile: `SIWI_ISSUER` → `IFINSTA_ISSUER`,
`SIWI_CLIENT_ID` → `IFINSTA_CLIENT_ID`, `SIWI_CLIENT_SECRET` →
`IFINSTA_CLIENT_SECRET`, `SIWI_REDIRECT_URI` → `IFINSTA_REDIRECT_URI`,
`SIWI_POST_LOGOUT_REDIRECT_URI` → `IFINSTA_POST_LOGOUT_REDIRECT_URI`.
(`SIWI_UI_ORIGIN` is discovered from the issuer.) Register the exact origin
people browse.

## Run it

Layout: `jani/`, `siwi/` side by side (and `accounts/` for the tests).

```bash
cargo build --release --manifest-path jani/Cargo.toml -p jani
cargo build --release --manifest-path jani/cloud/Cargo.toml          # add --features redis for a shared store
cp jani/cloud/.env.example .env && $EDITOR .env
set -a && . ./.env && set +a && ./jani/cloud/target/release/jani-cloud
```

Or build the image (`docker build -f jani/cloud/Dockerfile -t jani-cloud .`) and
adapt `docker-compose.example.yml`. Terminate TLS in front of it.

### Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `IFINSTA_*` | required | siwi/1 relying-party settings, as above. `IFINSTA_REDIS_URL` + `IFINSTA_STORE_KEYS` select a shared encrypted store. |
| `JANI_CLOUD_BIND` | `127.0.0.1:8080` | Listen address. The image sets `0.0.0.0:8080`. |
| `JANI_CLOUD_DATA` | `./data` | Per-person directories live under `users/`. |
| `JANI_CLOUD_JANI_BIN` | `jani` | The `jani` binary. |
| `JANI_CLOUD_JANI_ARGS` | none | Authority and model flags for every workspace, e.g. `--allow-write --write-path src --api-base … --model …`. |
| `JANI_CLOUD_PASSTHROUGH_ENV` | `JANI_API_KEY,JANI_API_BASE,JANI_MODEL` | Variables copied into each workspace. Nothing else is inherited. |
| `JANI_CLOUD_TEMPLATE` | none | A directory copied into each new worktree. |
| `JANI_CLOUD_SANDBOX` | `bwrap` | `bwrap` (required, refuses to start without it), `auto`, or `off`. |
| `JANI_CLOUD_UID_BASE` | none | Run each person as their own Unix user starting here. Needs root. |
| `JANI_CLOUD_ALLOW_UNISOLATED` | unset | Set to `1` to run with neither layer. Development only. |
| `JANI_CLOUD_MAX_ACTIVE` | `8` | Concurrent workspaces. Beyond it, people get a clear 503. |
| `JANI_CLOUD_IDLE_SECONDS` | `1800` | Stop a workspace after this long without a request. |

The gateway **refuses to start** with no isolation configured, or with a
required layer missing.

## Isolation, honestly

| Layer | What it gives | Needs |
| --- | --- | --- |
| Unix user per person (`JANI_CLOUD_UID_BASE`) | Files are mode 0700 and owned by that user; processes cannot signal each other. | root |
| bubblewrap | The system is read-only, only that person's directory is writable at `/work`, other users' files and the host's `/home` do not exist, separate PID/IPC/UTS namespaces. | `bwrap`, user namespaces |
| Jani's broker | Policy, per-call approvals, budgets, hash-chained audit journal. | always |

**What it does not do.** The network namespace is shared: the workspace must
reach the model API and the gateway reaches it over loopback, so another process
on the same machine can reach a workspace's port. It cannot use it without that
workspace's token, which only the gateway holds, but this is not network
isolation. There is no per-person CPU, memory or disk quota. A person who can
run commands can use the container's resources and network. Keep
`--allow-exec` off unless you accept that.

## Approvals

Jani's approvals require a *different, verified* principal to decide. This
prototype signs people in but does not yet issue Jani identity assertions, so a
person cannot approve their own held action. Use read-only or scoped-write
authority (as the tests do), or run a second authority path. Mapping an accounts
role to a signed Jani identity is the main follow-up.

## Tests

```bash
cargo test --manifest-path jani/cloud/Cargo.toml                       # unit tests
cd jani/cloud/e2e && npm i && npx playwright install chromium
ACCOUNTS_BIN=…/accounts-server GATEWAY_BIN=…/jani-cloud JANI_BIN=…/jani npm test
```

The end-to-end suite starts a real `accounts-server`, registers Jani with
`register-service`, signs two people in through the actual accounts login (the
modal and the redirect), and checks: sign-in by button and by redirect, no token
in the page or storage, same-origin enforcement, only `/` and `/api/*` forwarded,
journal attribution, two people with separate state and Unix users, sign-out
(local and everywhere) with a dead replayed cookie, idle stop and restart,
capacity limit, and no workspace surviving a stopped or SIGKILLed gateway.
`E2E_SANDBOX=bwrap` runs every workspace inside bubblewrap and checks from
outside what each can see; `E2E_NO_UID=1` drops the per-person Unix users.

## What is and is not verified

Verified here (13 end-to-end tests, each of three isolation configurations
passed repeatedly: Unix user only; bubblewrap only; both):

- Real sign-in against a real `accounts-server`, embedded modal → popup, and redirect.
- Per-person workspaces, state, journal attribution and Unix users.
- bubblewrap: system read-only, only `/work` mounted, no `/home`.
- Idle stop, restart on demand, capacity 503, stop with or without the gateway.

Not verified:

- **The Dockerfile and compose file were never built or run.** There was no Docker.
- bubblewrap *inside* a container, which needs relaxed seccomp or `SYS_ADMIN`.
- TLS and a reverse proxy; the `__Host-` cookie path (tests use loopback HTTP).
- `--features redis`, multi-instance operation, and surviving a restart.
- A live model provider (tests use a scripted one), load, or abuse resistance.
- Back-channel logout end to end (siwi's own suite covers it); a workspace
  otherwise stops at its idle limit after a provider-side logout.
- Windows and macOS, and the production accounts deployment.

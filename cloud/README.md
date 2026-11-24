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
                              one `jani ui` per person, on a Unix socket in their own
                              directory (no network port), own token
                              own worktree + state + audit journal, own Unix user,
                              inside bubblewrap with its own empty network and quotas
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

### Running the image

The image is built in Debian bookworm and runs on Ubuntu 24.04 (258 MB). It runs
as root so the gateway can give each person a Unix user and a cgroup. It holds
**no language toolchains**: build `FROM jani-cloud` and install what your people's
agents need (python, node, compilers).

bubblewrap creates namespaces, which Docker's defaults forbid. Running the image
showed that all of these are needed, and what fails without each:

| Flag | Without it |
|---|---|
| `--security-opt seccomp=unconfined` | `bwrap: Creating new namespace failed` or `pivot_root: Operation not permitted` |
| `--security-opt systempaths=unconfined` | `bwrap: Can't mount proc on /newroot/proc` (Docker masks `/proc`) |
| `--cap-add SYS_ADMIN` | no namespaces or mounts |
| `--cap-add NET_ADMIN` | `bwrap: loopback: Failed RTM_NEWADDR` when the network is isolated |
| `--security-opt apparmor=unconfined` | only on hosts that enforce AppArmor (not exercised here) |

With `SYS_ADMIN` the gateway also remounts the container's read-only cgroup
hierarchies read-write (it logs `remounted a read-only cgroup hierarchy`), which
Docker otherwise leaves read-only, so memory, process and CPU limits apply. This
changes the mount inside the container only. `--privileged` would also work, but
it grants far more than this needs. These flags are broad: **the container
boundary is weaker than a default container.** The per-person isolation (Unix
user, bubblewrap, empty network namespace, cgroup) is what the tests exercise;
treat the container as one layer, and prefer a dedicated VM or node for it.

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
| `JANI_CLOUD_SETTINGS` | none | A Jani settings file for every workspace: permission rules (which actions ask first), model profiles. Mounted read-only into each sandbox. Do not put secrets in it: the person's own commands can read it. |
| `JANI_CLOUD_SANDBOX` | `bwrap` | `bwrap` (required, refuses to start without it), `auto`, or `off`. |
| `JANI_CLOUD_UID_BASE` | none | Run each person as their own Unix user starting here. Needs root. |
| `JANI_CLOUD_ALLOW_UNISOLATED` | unset | Set to `1` to run with neither layer. Development only. |
| `JANI_CLOUD_NET` | `auto` | `isolated` (own empty network namespace; outbound only through the allowlist proxy), `shared`, or `auto` (isolated when bubblewrap is used). |
| `JANI_CLOUD_EGRESS_ALLOW` | none | Extra destinations beyond the model provider, as `host:port` or `host:*` (comma separated). The provider from `--api-base` is allowed automatically. |
| `JANI_CLOUD_QUOTAS` | `auto` | `require` refuses to start unless every hard limit can be enforced; `auto` enforces what the host allows and logs the rest; `off`. |
| `JANI_CLOUD_LIMIT_MEMORY_MB` / `_PIDS` / `_CPUS` / `_OPEN_FILES` / `_DISK_MB` | 1024 / 256 / none / 1024 / 2048 | Per person. `0` means no limit. |
| `JANI_CLOUD_DISK_CHECK_SECONDS` | `10` | How often disk use is checked. |
| `JANI_CLOUD_APPROVERS` / `JANI_CLOUD_APPROVERS_FILE` | none | Who may approve other people's held actions: `sub:<ifinsta subject>` or `email:<verified address>`, comma or newline separated. The file is re-read when it changes. |
| `JANI_CLOUD_MAX_ACTIVE` | `8` | Concurrent workspaces. Beyond it, people get a clear 503. |
| `JANI_CLOUD_IDLE_SECONDS` | `1800` | Stop a workspace after this long without a request. |

The gateway **refuses to start** with no isolation configured, or with a
required layer missing.

## Isolation, honestly

| Layer | What it gives | Needs |
| --- | --- | --- |
| Unix socket | A workspace listens on a socket in its own 0700 directory. There is no TCP port for another process to find, and the gateway adds a per-process token on top. | always |
| Unix user per person (`JANI_CLOUD_UID_BASE`) | Files are mode 0700 and owned by that user; processes cannot signal each other. | root |
| bubblewrap | The system is read-only, only that person's directory is writable at `/work`, other users' files and the host's `/home` do not exist, separate PID/IPC/UTS namespaces. | `bwrap`, user namespaces |
| Own network namespace (default with bubblewrap) | The workspace has only a loopback interface. It cannot reach the host's loopback ports, the gateway, other workspaces, the internal network or the internet. | bubblewrap |
| Egress allowlist proxy | The one way out, over a socket the gateway serves: `CONNECT` and plain `http://` to exactly the destinations allowed (by default the model provider). Everything else gets a 403 and is logged with the workspace. Names that resolve to loopback, private or link-local addresses are refused unless that exact address is allowed. | bubblewrap |
| Resource limits | Memory, processes and CPU through cgroups; open files, largest file and core dumps through rlimits; processes per Unix user. A workspace joins its cgroup and gets its limits between fork and exec, before it drops privileges. | cgroups (v1 tested) |
| Disk quota | The gateway checks each workspace's disk use every few seconds. Over the limit it is stopped and refused until it is back under; the person sees why and can delete their worktree files and start again (the journal is kept). A single file is also capped at the limit. | always |
| Jani's broker | Policy, per-call approvals, budgets, hash-chained audit journal. | always |

**Where each stops.**

- *Network.* With `JANI_CLOUD_NET=shared`, or without bubblewrap, a workspace shares the container's network and can reach anything it can. The gateway logs this at startup. Even when isolated, allowing a destination allows everything the person's own code can send there.
- *Memory and CPU* are hard limits only where cgroups are writable. cgroup v2 is implemented but **untested**; v1 is tested. `JANI_CLOUD_QUOTAS=require` turns "not enforceable" into a refusal to start.
- *Processes per Unix user* (`RLIMIT_NPROC`) only bites with per-person Unix users; the cgroup limit is the one that applies otherwise.
- *Disk* is a soft quota. A person can briefly exceed it between checks; the single-file cap bounds how much.
- No network egress *bandwidth* limit, no per-person request rate limit on the gateway beyond siwi's, and no limit on how many people sign in.

## Approvals

Jani's approvals need a *different* person to decide. In the cloud that person is an **approver**.

- **Who is an approver** is an operator decision, because accounts has no roles or groups. List them in `JANI_CLOUD_APPROVERS` / `JANI_CLOUD_APPROVERS_FILE` as `sub:<ifinsta subject>` or `email:<address>`. An `email:` entry only counts when accounts reports the address as **verified**: an unverified address proves nothing about who owns it. To find a subject, have the person sign in and open `/cloud/me`; the file is re-read on change, so no restart is needed.
- **The flow.** The person's assistant asks to do something the settings file says needs approval (for example `Write(./src/**)`). It is held, and their workspace says it is waiting for an approver. An approver opens **Approvals** (a link appears in their header when something is waiting), sees who asked, the action, the place and the risk, and decides with two clicks. The requester then attaches the decision with one button, and the action runs once.
- **Separation of duties is Jani's own.** The decision is made by Jani's approval store, which enforces the role, expiry and the rule that a requester can never approve their own action. An approver who is also the requester is refused with the store's own message.
- **The write is made as the requester's Unix user**, by a helper process, so the gateway never writes into a user-controlled directory with its own privileges. The file stays theirs.
- **Every decision is journaled** by the gateway in `data/audit` as a hash-chained record (who, which approval, which digest, approve or reject). `jani audit verify --state-dir data/audit` checks it.

**What this does not protect against.**

- **Approvers see what is being done and where, not the content.** Jani records a digest of the payload, not the payload.
- **The approval store is a file in the person's own directory.** A person who can run arbitrary commands as their own Unix user can edit it, so approvals are a control on the *assistant and the UI session*, not on a person with a shell. Keep `--allow-exec` off, or accept that.
- A degraded identity session (provider outage grace window) is still accepted for approving; there is no step-up requirement yet.
- The approver list is not tied to accounts roles; if accounts gains them, map them here.

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

Verified here: 28 end-to-end tests, each passing in all three of: Unix users only; bubblewrap only; bubblewrap plus Unix users.

**The built Docker image** (bubblewrap plus Unix users, gateway inside the container, flags above) passes 27 of them; the 28th ("killed") cannot be driven through the harness, which signals the `docker run` client rather than the gateway. Checked by hand instead: `kill -9` of `jani-cloud` inside the container makes `tini` exit, the container stops, and no `jani` process remains. Run it with `e2e/docker/gateway.sh` (see its header; `e2e/docker/Dockerfile.test` adds python and pytest, which the probes run inside the workspace).

- Real sign-in against a real `accounts-server`: embedded modal, popup and redirect.
- Per-person workspaces, state, journal attribution and Unix users.
- bubblewrap: system read-only, only `/work` mounted, no `/home`.
- **Network isolation, from inside the sandbox:** only loopback exists; the gateway's and the provider's host ports are unreachable; the internet is unreachable; the allowlist proxy reaches the model provider and returns 403 for any other host, for both `http://` and `CONNECT`, and for `localhost` by name.
- **Limits, from inside the sandbox:** a workspace over its memory limit is killed (the kernel logs the cgroup OOM kill) and the workspace survives; process, open-file and file-size limits hold; CPU is held below the configured share; the workspace sits in its own cgroup with the configured rlimits; filling the disk stops the workspace, shows why, and reset restores it with the journal kept.
- A **negative control**: the same network probe fails against a deliberately shared network, so a pass means something.
- **Approvals:** held, approved by a second person through the real approver page, attached, executed once; the store records the approver; an unverified email grants nothing; self-approval is refused by the store; CSRF and malformed targets are refused; the audit journal verifies.
- Idle stop and restart, capacity 503, and no workspace outliving the gateway however it ends.

A lesson worth keeping: Jani's per-tool `ok` flag means "the command ran", not "it succeeded". The first version of the network and limit probes trusted it and passed vacuously. The probes now write a JUnit report into the workspace and the test reads that, and the negative control proves it can fail.

Not verified:

- **cgroup v2** (implemented, untested). Only a v1 host was available.
- The container on a host that enforces AppArmor, rootless Docker, and Kubernetes (where `SYS_ADMIN` and `systempaths` are usually refused).
- The compose file itself was not run (its flags were taken from the `docker run` that was).
- TLS and a reverse proxy; the `__Host-` cookie path (tests use loopback HTTP).
- `--features redis`, multi-instance operation, and surviving a restart.
- A live model provider (tests use a scripted one), load, or abuse resistance.
- Back-channel logout end to end (siwi's own suite covers it).
- Windows and macOS, and the production accounts deployment.

# Proposal: OS-level execution sandbox for Jani

**Status:** proposal — not implemented, not scheduled
**Date:** 2026-10-25
**Scope:** confine commands requested through `workspace_command` (and their descendants)

## Summary

Jani currently enforces its permission model entirely in Rust: default-deny writes and
commands, relative-path validation, symlink refusal, credential-name blocking, and an
executable/subcommand allowlist. Those checks constrain **Jani's own tool calls**. They do
not constrain the code those calls run.

This proposal adds an optional, **fail-closed OS-level sandbox** around command execution.
The Rust policy layer stays and remains authoritative; the sandbox is a second boundary
that protects the host from repository code. When a sandbox is requested and cannot be
established, Jani refuses to run the command rather than silently degrading.

## Motivation

The current security model stops Jani from doing the wrong thing through its own
interfaces. It does not stop an allowlisted command from doing the wrong thing:

1. `cargo test` can execute `build.rs`, and `npm test` can execute `postinstall` scripts.
   Both run with the operator's full account privileges.
2. Symlink checks are not atomic. A hostile process racing replacement can redirect a
   write after validation (already documented in `README.md`).
3. Child processes can read `~/.ssh`, cloud credential files, other repositories, and the
   operator's shell history. `protected_component` blocks known credential *names* inside
   the worktree, not arbitrary reads elsewhere on the host.
4. Children can open network connections and exfiltrate whatever they can read.
5. `git commit` can run repository hooks, which are repository-controlled code.

Real-world coding-agent deployments increasingly run model-proposed commands against
untrusted or partially trusted checkouts. In that setting, the absence of a sandbox is the
largest remaining gap, and it is listed as such in the README limitations.

## Goals

- Confine every model-requested command, and all of its descendants, to the worktree plus
  a minimal set of toolchain and system paths.
- Deny reads of credential locations and unrelated host files, including paths reachable
  only through environment variables such as `HOME`.
- Provide an explicit network policy, with deny-by-default as the intended steady state.
- Fail closed: an explicitly requested sandbox that cannot be established produces an error
  before the child is spawned.
- Keep the existing Rust checks for portable semantics, precise error messages, and
  defence in depth.
- Leave default (unsandboxed) behavior unchanged so existing operator workflows do not
  break on upgrade.

## Non-goals

- Sandboxing the Jani process itself or its HTTPS model client.
- Defending against kernel or hypervisor exploits.
- Replacing OS accounts, ACLs, or container orchestration on multi-user hosts.
- Executing `SKILL.md` content — skills remain advisory text, never privileged code.
- Providing a general-purpose shell. Execution stays argv-only with no shell.

## Design

### Layering

```
operator flags / env
        │
        ▼
[ Layer 1: Rust policy ]      allow_write, write_path, allow_exec, exec_programs,
        │                     validate(), verify_staged_paths()
        ▼
[ Layer 2: OS sandbox ]       filesystem view, network policy, process limits,
        │                     credentials isolation (this proposal)
        ▼
[ Layer 3: container ]        optional, operator-supplied, for CI and untrusted repos
```

Layer 1 is unchanged. Layer 2 is advisory to Layer 1 in the sense that it may be stricter
than the policy but must never be looser.

### Integration point

All model-visible commands already funnel through `exec::run`, which calls
`process::execute` (`src/exec.rs:9`, `src/process.rs:62`). The change is localized there:

```rust
pub enum SandboxMode { Off, Auto, Bwrap, AppContainer }

pub struct SandboxPolicy {
    pub mode: SandboxMode,
    pub writable: Vec<PathBuf>,   // worktree scopes resolved to host paths
    pub network: NetworkPolicy,   // Deny | Loopback | Host
    pub runtime: PathBuf,         // per-run temp root
}

pub fn execute(
    cmd: &mut Command,
    cwd: &Path,
    max_secs: u64,
    policy: &SandboxPolicy,
) -> JResult<ProcessOutput>;
```

`Workspace` needs no changes: file editing remains a pure Rust path operation with its own
scope checks.

### Sandbox profile

| Resource | Default |
| --- | --- |
| Worktree | read-write (only paths under approved `--write-path` scopes if the backend supports per-path mounts; otherwise read-write worktree and rely on Layer 1 for scope) |
| `.git` | read-write for `git add` / `git commit`; hooks execute inside the sandbox |
| Toolchain | read-only: rustup/cargo home, compiler sysroot, system libraries |
| Dependency caches | read-only where possible; writable only when the operator opts in |
| Temp | per-run directory, writable, `HOME` and `TMPDIR` redirected into it |
| Env | minimal allowlist (`PATH`, `LANG`, toolchain vars); provider keys already removed |
| Network | deny (with `Loopback` and `Host` as explicit, documented downgrades) |
| Devices | `/dev/null`, `/dev/zero`, `/dev/urandom` only |
| Process count / CPU / memory | bounded via backend facilities |

Setting `HOME` to a per-run directory is important: it removes accidental reads of
`~/.ssh`, `~/.gitconfig`, `~/.npmrc`, and shell history without maintaining a growing
denylist.

### Platform backends

**Linux — bubblewrap (primary).** Preferred because it is a single unprivileged binary with
`--unshare-all`, `--die-with-parent`, `--clearenv`, explicit bind mounts, and `--unshare-net`
for default-deny networking. Fallback when user namespaces are unavailable: Landlock for
path restrictions plus seccomp for syscall filtering, with the network policy documented as
weaker.

**Windows — AppContainer + Job Object (primary).** Run the child in an AppContainer with no
capability SIDs (no network unless explicitly granted), and place it in a Job Object with
process-count, memory, and CPU-time limits. Filesystem access is granted by ACL on the
worktree, per-run temp root, and read-only toolchain directories. A reusable AppContainer
profile with a per-run staging directory avoids repeated profile creation cost.

**macOS — sandbox-exec (deferred).** An SBPL profile can express the same read/write/network
intent. It is deprecated by Apple but still functional; it is a phase-3 target, not a
blocker for the first release.

**No backend available.** `--sandbox auto` reports unavailability and exits non-zero when a
command is attempted; `--sandbox off` preserves today's behavior with a visible warning in
`doctor`. There is no implicit fallback.

### Git hooks

`git commit` necessarily writes `.git` and may invoke repository hooks. Under the sandbox,
hooks execute with the same confinement as any other child: worktree + per-run temp, no
network, stripped credentials. This is the intended behavior — the sandbox exists precisely
so repository-controlled code cannot reach the host. Operators who want hooks disabled
entirely can be given a separate opt-in later; it is out of scope here.

## CLI and configuration

```bash
jani run "fix the failing test" \
  --allow-exec --exec-program npm \
  --write-path src --allow-write \
  --sandbox auto
```

```
--sandbox MODE     off | auto | bwrap | appcontainer   (JANI_SANDBOX; default: off in 0.2)
--sandbox-net P    deny | loopback | host              (default: deny)
```

Rules:

- `--sandbox` without `--allow-exec` is accepted but inert; `doctor` notes it.
- An explicit backend that is unavailable is an error, not a downgrade.
- `auto` selects the platform backend if available, otherwise refuses to execute.
- `doctor` prints the selected backend, availability probe result, network policy, and
  writable roots.

## Observability

`workspace_command` results gain a `sandbox` object:

```json
{"mode": "auto", "backend": "bwrap", "enforced": true, "network": "deny"}
```

`enforced` is `true` only after a post-spawn verification probe succeeds. Jani must never
report a sandbox as enforced based on configuration alone.

## Threat coverage

| Threat | Current | With sandbox |
| --- | --- | --- |
| Build script writes outside worktree | not prevented | blocked (FS view) |
| Reads of `~/.ssh`, cloud credentials, other repos | not prevented | blocked (FS view + redirected `HOME`) |
| Network exfiltration by dependency or test code | not prevented | blocked (deny policy) |
| Symlink race after validation | partial | mitigated (confinement independent of path checks) |
| Fork bomb / memory exhaustion | partial (wall clock only) | bounded (Job Object / cgroup / rlimit) |
| Output flooding | mitigated (4 MB cutoff) | unchanged |
| Repository git hooks | unrestricted | confined |
| Kernel exploit from child | not prevented | not prevented |
| Operator deliberately running unsandboxed | allowed | still allowed with `--sandbox off` |

## Performance

- Process spawn overhead: a few milliseconds for bubblewrap; AppContainer setup is heavier,
  mitigated by a reusable profile.
- Build and test throughput should be close to native; filesystem mediation is the main
  variable.
- Per-run temp roots add small setup/teardown cost and must be cleaned up on every exit path.

## Testing plan

**Unit**

- Flag parsing, mode selection, and `doctor` output.
- `--sandbox` explicit + unavailable → error before spawn.
- Profile construction never includes `HOME`, credential directories, or the provider key.
- `enforced` is false unless verification passes.

**Integration (per supported OS)**

- Read outside worktree (`HOME`, parent directory, another temp path) is denied.
- Write outside worktree is denied even when not addressed through Jani tools.
- Network deny: an outbound connection attempt fails; loopback works when permitted.
- Git hook that writes to `$HOME` fails; normal commit still succeeds.
- Process-count and CPU-time limits terminate a fork/loop bomb.
- Normal `cargo test` and one non-Rust runner (e.g. `npm test`) pass inside the sandbox.
- Sandboxed and unsandboxed runs produce the same exit-code/`command_succeeded` semantics.

**CI**

- Extend the existing Ubuntu/Windows matrix; macOS is added only with phase 3.
- Sandbox tests are marked so contributors on unsupported hosts can still run the core suite.
- A negative test asserts that `--sandbox bwrap` on a host without it fails closed.

**Manual acceptance**

- Follow `docs/TESTING.md` with a deliberately hostile test crate (build script reads
  `$HOME`, opens a socket, spawns children) and confirm each attempt is blocked.

## Rollout

| Phase | Deliverable | Exit criterion |
| --- | --- | --- |
| 0 | Config, `doctor` reporting, `SandboxPolicy` plumbing, no enforcement | compiling, off by default, tests green |
| 1 | Linux bubblewrap with FS confinement and network deny | hostile-crate acceptance passes in CI |
| 2 | Windows AppContainer + Job Object | same acceptance on Windows CI |
| 3 | macOS sandbox-exec profile | same acceptance on macOS |
| 4 | Network allowlist or proxy-based egress policy | documented policy; deny remains default |
| 5 | Consider `auto` as default in a minor release | no regression reports over one release cycle |

## Risks and mitigations

- **bubblewrap unavailable** in restricted containers or hosts without user namespaces →
  detect at startup, fail closed, document the requirement.
- **AppContainer ACL complexity** for toolchains and caches → reusable profile plus a
  per-run staging directory; start with read-only toolchain mounts.
- **Network deny breaks dependency resolution** → pre-warmed caches, vendored dependencies,
  or an explicit `--sandbox-net host` downgrade that `doctor` and tool results label.
- **False sense of security** → documentation states that kernel bugs, same-user ACLs, and
  deliberately unsandboxed runs remain out of scope.
- **CI flakiness and platform drift** → probe once per run, keep sandbox tests isolated, and
  make unsupported-host skips explicit rather than silent.
- **Scope creep into a container manager** → Jani never pulls images or manages containers;
  Layer 3 stays operator-supplied.

## Alternatives considered

1. **Containers only.** Strongest isolation, but adds a Docker/Podman dependency, image
   lifecycle, and volume-mapping complexity. Better as an operator-supplied Layer 3.
2. **seccomp alone.** Good syscall filtering, no path-level control; cannot express "read
   the worktree but not `$HOME`".
3. **Landlock alone.** Strong path control on modern kernels, no network mediation and no
   coverage on older kernels or other OSes.
4. **Separate OS user for agent runs.** Effective but requires administrator setup and
   breaks developer workflows; useful as an additional deployment recommendation.
5. **Status quo.** Acceptable only when every checkout and every command is trusted.

## Open questions

1. Should `auto` become the default in 0.3, or stay opt-in until the acceptance suite has
   run against real projects for a release?
2. Is deny-by-default network acceptable for the primary use case, with caches pre-warmed,
   or does the default need a scoped package-registry allowlist from day one?
3. Per-path writable mounts (strict) versus read-write worktree with Layer 1 scope
   enforcement (simpler, weaker) for the first Linux release?
4. For Windows, is AppContainer sufficient, or should Windows Sandbox / WSL containers be
   the documented path for untrusted repositories?
5. Should sandbox policy be storable in a repository-level config file, or remain
   operator-only to prevent a repository from choosing its own confinement?

## Definition of done

- `--sandbox` exists on Linux and Windows, defaults to off, and fails closed when requested
  and unavailable.
- `doctor` reports backend, availability, network policy, and writable roots.
- Tool results report `enforced` truthfully after a verification probe.
- The hostile-crate acceptance test passes on both CI platforms.
- Documentation states limitations without overclaiming, and the README limitation list
  links here.

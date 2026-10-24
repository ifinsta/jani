# Testing and acceptance plan

## Verification gates

```bash
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --release
```

## Mandatory integration trials (disposable clone only)

| Trial | Expected outcome |
|---|---|
| `jani --version`, `jani --help`, `jani skills`, `jani doctor` | Finite output, no model call required |
| `hivewire probe` and `hivewire probe --live` | Adapter version detected; model returns READY using a real provider |
| Missing API key | Jani exits with clear error before model request |
| No task brief / no `SWARM_TASK` | All write/exec/merge operations refused |
| Approved task with `--allow-write --write-path src` | Only non-symlink files under src/ writable through source tool |
| `../`, `.git/`, `.agents/`, `.env` and symlink escape | Refused |
| Different `SWARM_EPOCH` | Guard denies effects |
| Paused, expired or interrupted assignment | Guard denies effects; work preserved |
| `cargo test` while plan incomplete | Hivewire metered tool refuses |
| Worker does not own task | Hivewire tool transition refuses |
| `git add` outside configured edit scope | Rust refuses |
| Command with shell, arbitrary executable, `git -C` | Rust refuses |
| Failed test or merge gate | No claim of completion; target unchanged |
| `inbox_read` | No transport acknowledgement, no token exposed to LLM |
| `inbox_ack` without opt-in/current receipt | Refused |
| Killed worker mid-task | Hivewire retains worktree/checkpoint/recovery state |
| Two agents edit overlapping scopes | Hivewire claim/scope/merge policies tested |
| Linux and Windows supervised spawn | Verify argv formatting, script path, worktree, environment and logs |
| Installed CLI protocol mismatch | Fail closed or explicitly refuse unsupported versions |

The contract of the current public Hivewire repository specifies CLI protocol 2, bus 1, and database schema 13. Confirm against `hivewire --json protocol` on the installed copy. Do not label Jani a verified adapter or production-safe until the applicable tests have actually passed.

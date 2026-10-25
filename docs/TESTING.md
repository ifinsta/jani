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
| Missing API key | Jani exits with a clear error before the model request |
| Read-only run | No writes or commands occur |
| `--allow-write --write-path src` | Only non-symlink files under `src/` are writable |
| `../`, `.git/`, `.agents/`, `.env` and symlink escape | Refused |
| Write outside the approved scope | Refused |
| `workspace_command` with shell, arbitrary executable, `git -C`, `git --no-index` | Refused |
| `cargo clippy --fix`, `cargo fmt` without `--check` | Refused |
| `git add -A` / `-u` or directory paths | Refused |
| `git commit` with pre-staged files outside scope | Refused before commit |
| Command timeout and output caps | Process is bounded; oversized output is truncated |
| Transient provider error (408/429/5xx) | Limited retry, then a clear failure |
| Two `SKILL.md` files with the same name | Earlier discovery directory wins |
| Provider credentials in child environment | Not inherited |
| Linux and Windows builds | Toolchain gates pass on both |

An allowlisted test runner can still execute malicious project code. Use a disposable checkout and OS/container isolation for untrusted repositories.

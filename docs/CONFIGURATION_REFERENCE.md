---
id: JANI-OPS-002
title: "Configuration reference"
status: implemented-partial
owner: backend/core/src/config.rs
version: 1.2.0
last-reviewed: 2026-10-26
source:
  - backend/core/src/config.rs
  - backend/core/src/settings.rs
tests: []
---

# Configuration reference

Configuration is resolved in the Rust CLI parser. This page explains where values come from and what is rejected; it is not an independent source of defaults. When the parser and this page disagree, the parser and its tests win.

## How configuration is resolved

1. Built-in defaults.
2. User-wide settings from `~/.jani/settings.json` (`%USERPROFILE%\.jani\settings.json` on Windows).
3. Worktree settings from `<worktree>/.jani/settings.json`, or the file named by `--settings` / `JANI_SETTINGS`.
4. Environment variables.
5. CLI flags.

Each layer overrides the one below it. Settings files do not merge: Jani uses the first file found in the order explicit path, worktree file, user file, and ignores the others. A missing default settings file is fine; an explicitly named file that does not exist is an error. Relative settings and `JANI_USER_SETTINGS` paths resolve against the worktree selected by `--repo`. `JANI_USER_SETTINGS` overrides the user-wide path, which is useful for tests and non-standard home directories.

| Source | Selection | Missing behavior |
| --- | --- | --- |
| CLI | `--settings FILE` | Error |
| Environment | `JANI_SETTINGS` | Falls through to the worktree file |
| Default | `<worktree>/.jani/settings.json` | Falls through to the user file |
| User | `~/.jani/settings.json` (`JANI_USER_SETTINGS` overrides) | Ignored if absent |

A user-wide file is a convenience for personal defaults such as a model profile; project files shadow it entirely. Keep provider keys in environment variables or a user-only file, never in a committed worktree file.

## Settings file

The file is optional, capped at 128 KiB, and strict. Unknown fields fail closed. The version is `1`, keys are camelCase, and at most 32 model profiles may be defined. The shape follows Claude Code conventions: `$schema`, a top-level `model` selector and a `permissions` block. `$schema` is accepted for editor tooling and does not affect resolution.

```json
{
  "$schema": "./settings.schema.json",
  "version": 1,
  "model": "fast",
  "models": {
    "fast": {
      "model": "claude-sonnet-5-5",
      "apiBase": "https://api.anthropic.com/v1",
      "apiKeyEnv": "ANTHROPIC_API_KEY"
    },
    "deep": {
      "model": "gpt-5",
      "apiBase": "https://api.openai.com/v1",
      "apiKey": "explicitly-configured-inline-key"
    }
  },
  "maxSteps": 20,
  "permissions": {
    "defaultMode": "default",
    "allow": ["Bash(cargo test *)"],
    "ask": ["Bash(git push *)"],
    "deny": ["Read(./.env)", "Read(./.env.*)"]
  }
}
```

| Field | Required | Constraint |
| --- | --- | --- |
| `version` | Yes | Must be `1` |
| `$schema` | No | Short non-empty string, at most 512 bytes |
| `model` | No | Profile name or raw model ID, at most 256 bytes |
| `defaultModel` | When `models` is non-empty and `model` is absent | Must name an existing profile |
| `apiBase`, `apiKey`, `apiKeyEnv`, `protocol`, `maxOutputTokens` | No | Defaults inherited by profiles that do not set their own |
| `models` | No | Map of profile name to profile; at most 32 |
| Profile name | Yes, per entry | 1-64 characters: ASCII letters, digits, `-` or `_` |
| `models.<name>.model` | Yes | Non-empty model ID, at most 256 bytes |
| `models.<name>.apiBase` | No | Non-empty, no control characters; HTTPS unless loopback HTTP is explicitly allowed |
| `models.<name>.apiKey` | No | Non-empty, no whitespace or control characters, at most 8192 bytes; accepted, warned about, never printed |
| `models.<name>.apiKeyEnv` | No | Valid environment-variable name |
| `models.<name>.protocol` | No | `openai` or `anthropic` |
| `models.<name>.maxOutputTokens` | No | Integer from 1 through 200000 |
| `maxSteps` | No | Integer from 1 through 80 |
| `permissions.defaultMode` | No | `default`, `acceptEdits` or `plan`; `bypassPermissions` is refused |
| `permissions.allow`, `ask`, `deny` | No | Up to 128 rules each, at most 512 bytes per rule |

Select a profile with `--model-profile NAME`. Without it, a top-level `model` naming a profile applies, then `defaultModel`. A top-level `model` that does not name a profile is used as a raw model ID. Settings can change the model, API base, credential source, transport and step limit, and add narrowing permission rules; they cannot grant write or command authority.

Permission rules use `Read(./path)`, `Write(./path)` and `Bash(command *)`. Evaluation is deny > plan read-only > ask > per-category allowlist; a `deny` on a directory covers its subtree. An `ask` match adds an external approval of class `settings-permission`; it cannot waive a policy approval. If any `allow` rule exists for a category, unmatched calls in that category are denied. Path rules reject absolute paths and `..`. Unsupported tools and malformed rules fail closed.

Rules map to the model-facing tools: `Read` covers `workspace_read` and `workspace_list` (directory), `Write` covers `workspace_write`, and `Bash` covers `workspace_command`. Native capabilities are separate `native.read` / `native.control` actions governed by the work contract and signed policy; their repository index excludes `.git`, `.jani`, `.env`, `target`, `claims`, generated directories and protected path components, but a project `Read` deny rule is not applied as an index filter.

The full example is [settings.example.json](schemas/settings.example.json) and the JSON Schema is [settings.schema.json](schemas/settings.schema.json); field notes and signing formats are in [schema notes](schemas/README.md). The worktree `.jani/settings.json` is unignored so a project can commit non-secret settings; journals, approvals and ledgers remain untracked, and the user-wide file lives outside the repository.

## Credentials

- A profile that sets `apiKeyEnv` reads that variable first. If it is unset, a profile with an inline `apiKey` uses that key; otherwise the key is reported missing, and Jani does not borrow `JANI_API_KEY` or `OPENAI_API_KEY` from another provider.
- A selected profile with an inline key and no `apiKeyEnv` uses the inline key rather than a generic environment key, so one provider's key is not sent to another endpoint.
- With no profile-bound credential, a run reads `JANI_API_KEY`, then `OPENAI_API_KEY`, then a top-level inline key.
- `JANI_MODEL` and `JANI_API_BASE` override the selected profile when set; CLI flags override both. `JANI_PROTOCOL` and `JANI_MAX_OUTPUT_TOKENS` behave the same way.
- When no protocol is configured, `api.anthropic.com` selects the Anthropic Messages API; every other base selects the OpenAI-compatible chat API.
- Inline keys are redacted from `Debug`, diagnostics and errors. `doctor` reports only the source (`settings (inline)`, `env NAME` or `missing`). Never commit a live key; prefer `apiKeyEnv` and a secret manager. Keep signing keys separate from verification keys and out of child environments and diagnostics.

## Workspace and effects

- `--repo` selects a worktree; `--state-dir` selects persistent state.
- `--allow-write` requires explicit relative `--write-path` scopes. There is no implicit full-repository editing.
- `--allow-exec` permits only approved commands; `--exec-program` adds a constrained list of extra test runners.
- `JANI_WRITE_PATHS`, `JANI_EXEC_PROGRAMS` and `JANI_SKILL_DIRS` provide environment equivalents where supported.

## Enterprise inputs

- Enterprise runs require a signed `--policy`, `--policy-key` trust roots, a signed `--identity` and a restricting `--contract`.
- A work contract and enterprise policy narrow operator scope; neither can widen it.
- `--sandbox-mode` and network restrictions must not be used to weaken required confinement. `jani sandbox doctor` reports support, not certification.

## Operational rules

- No remote configuration control plane exists. Review every flag and its precedence directly against the parser when shipping a release.
- `--help` and `--version` return without reading credentials or settings, so a broken settings file cannot block diagnosis.
- Configuration errors fail closed rather than falling back to a less restrictive mode.

## Acceptance

Verify `--help` without credentials, rejection of invalid scopes or programs, refusal of enterprise runs missing required authority, refusal of unknown settings fields, `bypassPermissions` and malformed permission rules, profile selection without a settings file, CLI-over-environment-over-settings precedence, inline credentials accepted without ever appearing in output, and settings permits that cannot widen `--allow-write` or `--allow-exec`.

## Change control

Source changes require review against negative tests; model assessments do not replace execution evidence.

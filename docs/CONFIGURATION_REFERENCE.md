---
id: JANI-OPS-002
title: "Configuration reference"
status: implemented-partial
owner: backend/core/src/config.rs
version: 1.1.0
last-reviewed: 2026-10-25
source:
  - backend/core/src/config.rs
  - backend/core/src/settings.rs
tests: []
---

# Configuration reference

Configuration is resolved in the Rust CLI parser. This page explains where values come from and what is rejected; it is not an independent source of defaults. When the parser and this page disagree, the parser and its tests win.

## How configuration is resolved

1. Built-in defaults.
2. `.jani/settings.json`, or the file named by `--settings` / `JANI_SETTINGS`.
3. Environment variables.
4. CLI flags.

Each layer overrides the one below it. A missing default settings file is fine; an explicitly named file that does not exist is an error. Relative settings paths resolve against the worktree selected by `--repo`.

| Source | Selection | Missing behavior |
| --- | --- | --- |
| CLI | `--settings FILE` | Error |
| Environment | `JANI_SETTINGS` | Falls through to the default path |
| Default | `<worktree>/.jani/settings.json` | Ignored if absent |

## Settings file

The file is optional, capped at 64 KiB, and strict. Unknown fields fail closed. The version is `1`, keys are camelCase, and at most 32 model profiles may be defined. An inline `apiKey` or `api_key` field is rejected with a message pointing to `apiKeyEnv`.

```json
{
  "version": 1,
  "defaultModel": "fast",
  "models": {
    "fast": {
      "model": "gpt-5-mini",
      "apiBase": "https://api.openai.com/v1",
      "apiKeyEnv": "JANI_API_KEY"
    }
  },
  "maxSteps": 20
}
```

| Field | Required | Constraint |
| --- | --- | --- |
| `version` | Yes | Must be `1` |
| `defaultModel` | When `models` is non-empty | Must name an existing profile |
| `models` | No | Map of profile name to profile; at most 32 |
| Profile name | Yes, per entry | 1-64 characters: ASCII letters, digits, `-` or `_` |
| `models.<name>.model` | Yes | Non-empty model ID |
| `models.<name>.apiBase` | No | Non-empty, no control characters; HTTPS unless loopback HTTP is explicitly allowed |
| `models.<name>.apiKeyEnv` | No | Valid environment-variable name |
| `maxSteps` | No | Integer from 1 through 80 |

Select a profile with `--model-profile NAME`. Without it, `defaultModel` applies. Settings can change the model, API base, credential source and step limit; they cannot grant write or command authority.

The full example is [settings.example.json](schemas/settings.example.json); field notes and signing formats are in [schema notes](schemas/README.md). Only `.jani/settings.json` is unignored, so journals, approvals and ledgers remain untracked.

## Credentials

- A profile that sets `apiKeyEnv` reads **only** that variable. If it is missing, the key is reported missing; Jani does not borrow `JANI_API_KEY` or `OPENAI_API_KEY` from another provider.
- A run without a profile-bound variable reads `JANI_API_KEY`, then `OPENAI_API_KEY`.
- `JANI_MODEL` and `JANI_API_BASE` override the selected profile when set; CLI flags override both.
- Never store live API keys in repository fixtures or commit them. Keep signing keys separate from verification keys and out of child environments and diagnostics.

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

Verify `--help` without credentials, rejection of invalid scopes or programs, refusal of enterprise runs missing required authority, rejection of inline or unknown settings fields, profile selection without a settings file, and CLI-over-environment-over-settings precedence.

## Change control

Source changes require review against negative tests; model assessments do not replace execution evidence.

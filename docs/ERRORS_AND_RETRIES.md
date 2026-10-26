---
id: JANI-CORE-003
title: "Errors and retries"
status: implemented-partial
owner: backend/server/src/model.rs
version: 1.1.0
last-reviewed: 2026-10-27
source:
  - backend/server/src/model.rs
  - backend/server/src/update.rs
tests:
  - backend/server/tests/update_e2e.rs
---

# Errors and retries

## Failure categories
| Error class | Handling | Retriable? |
| --- | --- | --- |
| Authorization denial | Audit and return coded denial | No |
| Schema or argument invalid | Reject before effect | Only after correction |
| Unsupported tool/platform | Refuse, no silent fallback | Only after capability becomes available |
| Model HTTP transient | Bound attempts and preserve input | Selected 408/429/5xx only |
| Update fetch transient | Bound attempts with exponential backoff | Network errors, invalid JSON, 408/429/5xx only |
| Update digest/size mismatch | Refuse before replacing a target | No |
| MCP outcome unknown | Reconcile with authoritative source | Not without reconciliation |
| Corrupt authoritative state | Halt mutable work, preserve for investigation | No automatic retry |
| Budget exhausted | Abort work, preserve accumulated usage | No reset on restart |
## Implemented specifics
- The model HTTP client has an 180-second request timeout and at most three attempts for selected status codes.
- The update client has a 20-second connect timeout, a 180-second request timeout and four attempts for transient network errors, invalid metadata and 408/429/5xx, with exponential backoff from `JANI_UPDATE_RETRY_BASE_MS` capped at 8 seconds. Redirects are refused; a retry never accepts a mismatched size or digest.
- External MCP operations do not automatically retry an uncertain remote effect.
- Broker, policy and sandbox errors are not permission prompts and cannot be handled by widening scope autonomously.
## Required next step
- Standardize typed error codes with correlation ID, affected resource and execution phase. Avoid leaking provider tokens in errors.
## Acceptance
- Simulate timeouts, 429, malformed JSON, journal unavailability, unknown tools and uncertain side effects.

## Change control
Source changes require review against negative tests; model assessments do not replace execution evidence.

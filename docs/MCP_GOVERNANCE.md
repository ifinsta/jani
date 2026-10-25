# Jani MCP Governance Contract (JMGC) v1

Status: **implemented first trust-boundary slice in PR #4; compilation/CI pending**. This is not a general-purpose MCP client or proof of regulatory compliance.

## Non-negotiable authorization rules

1. Every protected request must identify a **verified initiating human**. The human has a signed Jani IdentityAssertion verified using \`--policy-key\`.
2. A separately signed, short-lived task delegation binds **initiator, tenant, agent, task, purpose, roles, entitlements, exact resources, permitted operations and maximum data classification**. A model/tool/server cannot self-issue rights. The issuing service must actually evaluate the initiating user's rights.
3. An independently signed MCP governance contract declares each operation, its concrete MCP tool name, required role(s), all required entitlements, execution mode, a closed JSON input schema and request/response sensitivity limits. Missing contract or incomplete declaration is refused.
4. The effective right is the intersection of the initiator's verified authorization, task delegation, Jani's independent organizational policy, the operation contract, the precise resource, classification and the source-system's independently enforced authorization.
5. **\`user_commit\` is uncallable through Jani.** \`read\` and \`prepare\` may be requested only with valid authority. The source application must conduct a separate, freshly authenticated user submission bound to exact record versions and proposed payloads. Merely invoking \`prepare\` does not constitute a user commit.
6. Every MCP result must carry \`result._meta["com.ifinsta.jani/governance"]\`: a **source-signed** receipt binding the actual payload digest, request digest, initiator, tenant, agent, task, action, resource, role, required permissions, classification, applied filtering flag and short expiry. Missing, modified, unsupported or untrusted evidence blocks disclosure.
7. The source MCP server must still validate its own **OAuth access token** (issuer, audience, scopes, validity, revocation) and recheck permissions and row/field filters against its authoritative records. **It must not trust request metadata or a self-declared role.** Signed delegations provide a separate authorization assertion that the source must independently verify.
8. No Hivewire coordinator, mandatory message bus or external agent runtime is required.

## Code layout

\`\`\`text
backend/
  core/         signed identity + policy, classification and audit
  mcp/          signed contract / delegation / response attestation verifier
  server/       guarded MCP CLI + pinned HTTPS 2026-07-28 tools/call transport
  store/        existing attention / Mirror persistence (not MCP authorization state)
contract/mcp/   contract, delegation and receipt JSON schemas
tools/          architectural conformance checks
\`\`\`

## CLI (initial safe integration)

\`\`\`bash
jani mcp verify \
  --mcp-contract mcp-contract.json \
  --mcp-delegation task-grant.json \
  --mcp-contract-key contract:HEX_PUBLIC_KEY \
  --mcp-delegation-key authorization:HEX_PUBLIC_KEY \
  --identity initiator-identity.json --policy-key identity:HEX_PUBLIC_KEY \
  --tenant tenant-1 --principal user-1 --task task-1 \
  --purpose SOFTWARE_DEV --mcp-agent jani

jani mcp authorize organization.news.read organization:news \
  --mcp-contract mcp-contract.json --mcp-delegation task-grant.json \
  --mcp-arguments news-query.json \
  --mcp-contract-key contract:HEX_PUBLIC_KEY \
  --mcp-delegation-key authorization:HEX_PUBLIC_KEY \
  --identity initiator-identity.json --policy-key identity:HEX_PUBLIC_KEY \
  --tenant tenant-1 --principal user-1 --task task-1 --purpose SOFTWARE_DEV

# Valid signed, audience-bound OAuth token in a secure local file.
# The source MCP server MUST independently verify it and enforce scopes.
jani mcp call organization.news.read organization:news \
  --mcp-contract mcp-contract.json --mcp-delegation task-grant.json \
  --mcp-arguments news-query.json --mcp-token-file /secure/mcp-token \
  --mcp-contract-key contract:HEX_PUBLIC_KEY \
  --mcp-delegation-key authorization:HEX_PUBLIC_KEY \
  --mcp-receipt-key mcp-server:HEX_PUBLIC_KEY \
  --identity initiator-identity.json --policy-key identity:HEX_PUBLIC_KEY \
  --tenant tenant-1 --principal user-1 --task task-1 --purpose SOFTWARE_DEV

# Offline verification and governed disclosure test with a saved JSON-RPC result:
jani mcp validate-response organization.news.read organization:news \
  --mcp-response response.json \
  --mcp-contract mcp-contract.json --mcp-delegation task-grant.json \
  --mcp-arguments news-query.json \
  --mcp-contract-key contract:HEX_PUBLIC_KEY \
  --mcp-delegation-key authorization:HEX_PUBLIC_KEY \
  --mcp-receipt-key mcp-server:HEX_PUBLIC_KEY \
  --identity initiator-identity.json --policy-key identity:HEX_PUBLIC_KEY \
  --tenant tenant-1 --principal user-1 --task task-1 --purpose SOFTWARE_DEV
\`\`\`

The arguments JSON **must** contain a \`resource\` property whose value exactly equals the requested resource. Arbitrary wildcard resource grants are disallowed.

## MCP transport profile and compatibility boundary

The first network path supports a tightly constrained subset of **MCP 2026-07-28 Streamable HTTP**: one \`tools/call\` request per HTTPS POST with mandatory modern \`_meta\`, \`MCP-Protocol-Version\`, \`Mcp-Method\`, \`Mcp-Name\` and no redirects or automatic retries. Bounded JSON and request-scoped SSE responses are accepted. The extension carries the complete **signed delegation** plus signed contract/request digests in \`params._meta\`. Transport metadata alone conveys **no permissions**. Unsupported MCP versions/methods, unsigned responses and arbitrary source endpoints fail closed.

The first gateway does **not** implement OAuth login, refresh-token management, protected-resource metadata discovery, token introspection, all MCP lifecycle methods, \`tools/list\` discovery, negotiated extensions, custom header annotation processing, or the broader set of MCP client capabilities. It accepts a provided OAuth bearer token stored in a restricted file. Organizations should connect an external trusted OAuth credential broker for production and must configure the MCP server to validate audience, issuer, token binding, expiry and scopes. A token is *never* forwarded to a different destination, and Jani does not follow redirects.

**The MCP source must actually implement JMGC** by verifying the signed delegation at request time and signing the result receipt. An ordinary MCP server that omits role/entitlement proof, required resource filters or output classifications is deliberately not usable through this strict gateway.

## Prepared remediation and human-only commit

An organization can declare:

\`\`\`json
{
  "records.remediation.prepare": {
    "tool": "remediation_prepare",
    "mode": "prepare",
    "requiredRoles": ["verifier"],
    "requiredEntitlements": ["records.propose"],
    "inputSchema": {
      "type": "object",
      "properties": {"resource": {"type": "string"}},
      "required": ["resource"],
      "additionalProperties": false
    },
    "maxRequestClassification": "RESTRICTED",
    "maxResponseClassification": "RESTRICTED"
  },
  "records.update": {
    "tool": "records_update",
    "mode": "user_commit",
    "requiredRoles": ["record-editor"],
    "requiredEntitlements": ["records.update"],
    "inputSchema": {
      "type": "object",
      "properties": {"resource": {"type": "string"}},
      "required": ["resource"],
      "additionalProperties": false
    },
    "maxRequestClassification": "RESTRICTED",
    "maxResponseClassification": "RESTRICTED"
  }
}
\`\`\`

Even if a trusted issuer signs a grant containing \`records.update\`, Jani refuses its agent invocation. The source application must expose a **separate user-authenticated commit path**; an agent must never replay the initiating user's credential into that commit path. The source must independently bind the submitted patch to a prior proposal digest, row version(s), expiry and a one-time nonce, and must re-evaluate permissions on commit. Those source-side controls are an integration obligation, **not implemented by Jani's CLI**.

## Error semantics

- \`CONTRACT_MISSING\`, \`CONTRACT_SIGNATURE_MISSING\`, \`CONTRACT_SIGNATURE_INVALID\`, \`CONTRACT_UNSUPPORTED_OPERATION\`: no trustworthy declared authority
- \`AUTH_CONTEXT_MISSING\`, \`DELEGATION_BINDING_MISMATCH\`, \`DELEGATION_EXPIRED_OR_INVALID\`: missing or outdated initiator/task authority
- \`ENTITLEMENT_PROOF_MISSING\`, \`PERMISSION_DENIED\`, \`DELEGATION_SCOPE_DENIED\`: insufficient signed role/permission/resource authority
- \`USER_COMMIT_REQUIRED\`: agent attempted the initiating-user-only final action
- \`DATA_CLASSIFICATION_DENIED\`, \`DATA_CLASSIFICATION_MISMATCH_OR_SECRET\`: payload or disclosure exceeds the task's data ceiling
- \`CONTRACT_RESPONSE_VIOLATION\`: unsigned, unfiltered, inconsistent, altered or untrusted response
- \`MCP_HTTP_REQUEST_FAILED_OR_UNCERTAIN\`: outcome uncertain; **do not blindly retry preparations**; reconcile at the source first

## Current security and reliability limitations

- Cryptographic identity and delegation are required even in local-unmanaged mode; enterprise additionally requires signed organizational policy rules for \`mcp.<operation>\` with resource \`mcp:<server-id>:<exact-resource>\`.
- The agent binding is currently a signed **logical agent identifier**. A distinct attested process identity, issuer trust partitioning, authorization revocation lookup, certificate pinning and task-owner lease fencing need a later hardening phase.
- Jani verifies the issuer-provided role/entitlement **assertion**, not the organization's live HR/permission database. The trusted issuer must guarantee that it is current. Grants expire within one hour; hard revocation requires an authorization-service check not yet implemented.
- The operation's \`prepare\` mode is certified by a trusted contract, but only the source can guarantee that its tool is truly non-mutating. Certified server conformance and integration security testing are still required.
- Signed output classifications are checked against Jani's basic detectors. The classifier does **not** recognize every sensitive business secret, and full provenance-aware taint tracking remains a separate requirement.
- The network path and source-signed receipt protocol require a cooperating MCP provider. They have not been tested against a live third-party production MCP.
- This slice does not yet offer MCP tools directly inside the LLM-managed tool palette: **only the governed Jani MCP CLI entry point may invoke them**. This avoids routing unverified sensitive results back into the model until source-to-model classification inheritance is implemented.
- There is no autonomous record commit, payment release or privilege promotion.

## Recommended required validation

\`\`\`bash
cargo fmt --all -- --check
cargo run -p jani-check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p jani
\`\`\`

Add an authenticated test MCP server that verifies its tokens and signed grants and issues signed results; fault-inject missing roles, revoked rights, forged entitlements, wrong tenant, replayed receipts, mismatched payload digests, malicious server output, partial HTTP responses, and attempts to bypass user-only commit. Cross-host and banking operation must remain disabled until these security gates pass.

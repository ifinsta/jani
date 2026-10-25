//! Authorized Jani MCP gateway command. The agent cannot acquire commit rights.
//! Only signed grants, signed contracts and signed result receipts are accepted.
use crate::config::{Config, JResult};
use crate::data::{json_strings, Governance};
use crate::identity;
use crate::journal::{EventInput, Journal};
use crate::policy::PolicyEngine;
use crate::types::{
    new_id, now_unix, sha256_hex, ActionRequest, Classification, Decision, Effect, Provenance,
    RiskTier,
};
use jani_mcp::{
    authorize, verify_response, Context, Delegation, McpContract, Mode, MCP_VERSION, PROTOCOL,
};
use reqwest::{blocking::Client, header, redirect::Policy};
use serde_json::{json, Value};
use std::{fs, io::Read, path::Path, time::Duration};

const MAX_JSON: u64 = 1_048_576;
const MAX_ARGS: u64 = 65_536;

fn read_file(path: &Path, limit: u64) -> JResult<String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("MCP_FILE_UNAVAILABLE: {e}"))?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > limit {
        return Err("MCP_FILE_UNSAFE_OR_TOO_LARGE".into());
    }
    fs::read_to_string(path).map_err(|e| format!("MCP_FILE_UNAVAILABLE: {e}"))
}

fn json_file(path: &Path, limit: u64) -> JResult<Value> {
    serde_json::from_str(&read_file(path, limit)?).map_err(|_| "MCP_INVALID_JSON".into())
}

fn audit(cfg: &Config, action: &str, allowed: bool, code: &str) -> JResult<()> {
    // Only non-sensitive codes and digests are journaled, never request bodies,
    // role credentials, bearer tokens or MCP response contents.
    let mut journal = Journal::open(&cfg.state_dir)?;
    let d = sha256_hex(format!("{action}:{code}").as_bytes());
    journal.record(EventInput {
        tenant_id: &cfg.tenant,
        task_id: cfg.task_id.as_deref().unwrap_or("mcp-gateway"),
        run_id: "mcp-gateway",
        invocation_id: "mcp-gateway",
        actor_id: &cfg.principal,
        worker_id: &cfg.mcp_agent,
        action,
        resource_digest: &d,
        decision: if allowed { "permit" } else { "deny" },
        reason_code: code,
        policy_revision: "mcp-governance-v1",
        approval_id: None,
        sandbox_receipt_id: None,
        data_classification: Classification::Internal,
    })?;
    Ok(())
}

fn token_file(path: &Path) -> JResult<String> {
    let meta = fs::symlink_metadata(path).map_err(|_| "MCP_TOKEN_UNAVAILABLE".to_string())?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 8192 {
        return Err("MCP_TOKEN_UNSAFE".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("MCP_TOKEN_FILE_PERMISSIONS_MUST_BE_0600".into());
        }
    }
    let token = read_file(path, 8192)?;
    let token = token.trim();
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return Err("MCP_TOKEN_INVALID".into());
    }
    Ok(token.into())
}

fn parse_reply(body: &str, mime: &str, rpc_id: &Value) -> JResult<Value> {
    if mime.starts_with("application/json") {
        let reply: Value = serde_json::from_str(body).map_err(|_| "MCP_RPC_RESPONSE_INVALID")?;
        if reply.get("id") != Some(rpc_id) {
            return Err("MCP_RPC_ID_MISMATCH".into());
        }
        return Ok(reply);
    }
    if !mime.starts_with("text/event-stream") {
        return Err("MCP_RPC_UNSUPPORTED_RESPONSE_TYPE".into());
    }
    let normalized = body.replace("\r\n", "\n");
    for block in normalized.split("\n\n") {
        let mut payload = String::new();
        for line in block.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                payload.push_str(data.trim_start());
            }
        }
        if let Ok(value) = serde_json::from_str::<Value>(&payload) {
            if value.get("id") == Some(rpc_id) {
                return Ok(value);
            }
        }
    }
    Err("MCP_RPC_MISSING_FINAL_RESPONSE".into())
}

pub fn send(
    contract: &McpContract,
    grant: &Delegation,
    call: &jani_mcp::AuthorizedCall,
    token: &str,
) -> JResult<Value> {
    // Never use caller-selected endpoints or cross-host redirects.
    // No automatic retries: a timeout after prepare is ambiguous.
    let client = Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "MCP_HTTP_CLIENT_UNAVAILABLE")?;
    let rpc_id = json!(new_id("mcp-rpc"));
    let body = json!({
        "jsonrpc":"2.0","id":rpc_id,"method":"tools/call",
        "params":{
            "name":call.tool,"arguments":call.arguments,
            "_meta":{
                "io.modelcontextprotocol/protocolVersion": MCP_VERSION,
                "io.modelcontextprotocol/clientInfo":{"name":"jani","version":env!("CARGO_PKG_VERSION")},
                "io.modelcontextprotocol/clientCapabilities":{},
                "com.ifinsta.jani/delegation":grant,
                "com.ifinsta.jani/contractDigest":call.contract_digest,
                "com.ifinsta.jani/requestDigest":call.request_digest
            }
        }
    });
    let mut resp = client
        .post(&contract.server.endpoint)
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("MCP-Protocol-Version", MCP_VERSION)
        .header("Mcp-Method", "tools/call")
        .header("Mcp-Name", &call.tool)
        .bearer_auth(token)
        .json(&body)
        .send()
        .map_err(|_| "MCP_HTTP_REQUEST_FAILED_OR_UNCERTAIN")?;
    if !resp.status().is_success() {
        // Don't include untrusted server response bodies in security logs.
        return Err(format!("MCP_HTTP_STATUS_{}", resp.status().as_u16()));
    }
    let mime = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut buffer = Vec::new();
    (&mut resp)
        .take(MAX_JSON + 1)
        .read_to_end(&mut buffer)
        .map_err(|_| "MCP_RESPONSE_READ_FAILED")?;
    if buffer.len() as u64 > MAX_JSON {
        return Err("MCP_RESPONSE_TOO_LARGE".into());
    }
    let text = String::from_utf8(buffer).map_err(|_| "MCP_RESPONSE_NOT_UTF8")?;
    parse_reply(&text, &mime, &rpc_id)
}

fn verified_release(
    governance: &Governance,
    contract: &McpContract,
    delegation: &Delegation,
    call: &jani_mcp::AuthorizedCall,
    response: &Value,
    receipt_keys: &[String],
) -> JResult<Value> {
    let verified = verify_response(
        contract,
        delegation,
        call,
        response,
        receipt_keys,
        now_unix(),
    )?;
    // The server's signed label is authoritative as a floor. Independently
    // inspect visible response content for obvious secrets/PII; never allow the
    // server to label detected sensitive content more permissively.
    let mut local_policy = governance.policy.clone();
    local_policy.source_default = Classification::Public;
    local_policy.redact_secrets = false;
    let inspector = Governance::new(
        local_policy,
        call.tenant.clone(),
        call.initiator.clone(),
        call.purpose.clone(),
    );
    let raw = json_strings(&verified["result"]);
    let scan = inspector.classify(
        &Provenance {
            kind: "mcp-result".into(),
            resource: call.resource.clone(),
            revision: None,
        },
        &raw,
    );
    let label: Classification = serde_json::from_value(verified["classification"].clone())
        .map_err(|_| "DATA_CLASSIFICATION_MISSING")?;
    if scan.secrets_found > 0 || scan.original > label {
        return Err("DATA_CLASSIFICATION_MISMATCH_OR_SECRET".into());
    }
    Ok(verified)
}

pub fn command(cfg: &Config) -> JResult<Value> {
    let result = command_inner(cfg);
    if let Err(error) = &result {
        // Denials are best-effort audited; an unverified request never reaches the network.
        let code = error.split(':').next().unwrap_or("MCP_DENIED");
        let _ = audit(cfg, "mcp.denied", false, code);
    }
    result
}

fn command_inner(cfg: &Config) -> JResult<Value> {
    let sub = cfg.sub.as_deref().ok_or("MCP subcommand required")?;
    if !matches!(sub, "verify" | "authorize" | "call" | "validate-response") {
        return Err("CONTRACT_UNSUPPORTED_OPERATION".into());
    }
    let identity = identity::load(cfg)?;
    if !identity.verified || identity.subject_type != "human" {
        return Err("AUTH_CONTEXT_MISSING: verified initiating human required".into());
    }
    let task_id = cfg
        .task_id
        .as_deref()
        .ok_or("AUTH_CONTEXT_MISSING: --task required")?;
    let contract_file = cfg
        .mcp_contract_path
        .as_deref()
        .ok_or("CONTRACT_MISSING: --mcp-contract")?;
    let delegation_file = cfg
        .mcp_delegation_path
        .as_deref()
        .ok_or("ENTITLEMENT_PROOF_MISSING: --mcp-delegation")?;
    let contract: McpContract = serde_json::from_value(json_file(contract_file, MAX_JSON)?)
        .map_err(|_| "CONTRACT_MALFORMED")?;
    let delegation: Delegation = serde_json::from_value(json_file(delegation_file, MAX_JSON)?)
        .map_err(|_| "DELEGATION_MALFORMED")?;
    let ctx = Context {
        tenant: &identity.tenant,
        initiator: &identity.principal,
        agent: &cfg.mcp_agent,
        task_id,
        purpose: &cfg.purpose,
        now: now_unix(),
        contract_keys: &cfg.mcp_contract_keys,
        delegation_keys: &cfg.mcp_delegation_keys,
    };
    contract.verify(ctx.contract_keys)?;
    delegation.verify(&ctx)?;
    if sub == "verify" {
        return Ok(json!({
            "valid":true,"server":contract.server.id,"operations":contract.operations.keys().collect::<Vec<_>>(),
            "initiator":identity.principal,"agent":ctx.agent,"taskId":task_id,
            "purpose":ctx.purpose,"protocol":PROTOCOL
        }));
    }
    let op = cfg
        .positional
        .first()
        .ok_or("CONTRACT_UNSUPPORTED_OPERATION: operation required")?;
    let resource = cfg.positional.get(1).ok_or("CONTRACT_RESOURCE_MISSING")?;
    if !op
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
    {
        return Err("CONTRACT_UNSUPPORTED_OPERATION".into());
    }
    let spec = contract
        .operations
        .get(op)
        .ok_or("CONTRACT_UNSUPPORTED_OPERATION")?;
    if spec.mode == Mode::UserCommit {
        return Err("USER_COMMIT_REQUIRED: final submission cannot be performed by Jani".into());
    }
    let engine = PolicyEngine::load(cfg)?;
    let effect = if spec.mode == Mode::Read {
        Effect::Read
    } else {
        Effect::Control
    };
    let action = format!("mcp.{op}");
    let request = ActionRequest {
        task_id: task_id.into(),
        invocation_id: new_id("mcp"),
        tenant_id: identity.tenant.clone(),
        principal_id: identity.principal.clone(),
        action: action.clone(),
        resource: format!("mcp:{}:{resource}", contract.server.id),
        effect,
        risk: if spec.mode == Mode::Read {
            RiskTier::R1
        } else {
            RiskTier::R2
        },
        arguments_digest: sha256_hex(format!("{op}:{resource}").as_bytes()),
        idempotency_key: None,
    };
    if engine.evaluate(cfg, &request).decision != Decision::Permit {
        return Err(
            "PERMISSION_DENIED: organization policy did not authorize MCP operation".into(),
        );
    }
    let args = json_file(
        cfg.mcp_input_path
            .as_deref()
            .ok_or("CONTRACT_ARGUMENTS_MISSING: --mcp-arguments")?,
        MAX_ARGS,
    )?;
    // No secrets can enter the MCP request through arguments. An organizational
    // source classification is a floor; detected PII may elevate it.
    let data_policy = engine.data_policy();
    if !data_policy
        .allowed_purposes
        .iter()
        .any(|p| p == "*" || p == &cfg.purpose)
    {
        return Err("DATA_PURPOSE_DENIED".into());
    }
    let governance = Governance::new(
        data_policy,
        identity.tenant.clone(),
        identity.principal.clone(),
        cfg.purpose.clone(),
    );
    let raw = serde_json::to_string(&args).map_err(|e| e.to_string())?;
    let class = governance.classify(
        &Provenance {
            kind: "mcp-input".into(),
            resource: resource.to_string(),
            revision: None,
        },
        &raw,
    );
    if class.secrets_found > 0
        || class.effective > spec.max_request_classification
        || class.effective > delegation.max_classification
    {
        return Err("DATA_CLASSIFICATION_DENIED: request arguments exceed permitted class".into());
    }
    let call = authorize(&contract, &delegation, &ctx, op, resource, args)?;
    if sub == "authorize" {
        audit(cfg, &action, true, "MCP_AUTHORIZED_NO_NETWORK")?;
        return Ok(json!({
            "authorized":true, "serverId":call.server_id, "operation":call.operation,
            "resource":call.resource, "role":call.role,
            "entitlements":call.entitlements, "requestDigest":call.request_digest,
            "mode":spec.mode, "networkInvoked":false,
        }));
    }
    if sub == "validate-response" {
        let response = json_file(
            cfg.mcp_response_path
                .as_deref()
                .ok_or("CONTRACT_RESPONSE_MISSING: --mcp-response")?,
            MAX_JSON,
        )?;
        let result = verified_release(
            &governance,
            &contract,
            &delegation,
            &call,
            &response,
            &cfg.mcp_receipt_keys,
        )?;
        audit(cfg, &action, true, "MCP_RESPONSE_VERIFIED_OFFLINE")?;
        return Ok(result);
    }
    // Prior durable audit is mandatory before network access, including prepare.
    audit(cfg, &action, true, "MCP_REQUEST_AUTHORIZED_INTENT")?;
    let token = token_file(
        cfg.mcp_token_path
            .as_deref()
            .ok_or("MCP_OAUTH_TOKEN_REQUIRED: --mcp-token-file")?,
    )?;
    let response = send(&contract, &delegation, &call, &token)?;
    let result = verified_release(
        &governance,
        &contract,
        &delegation,
        &call,
        &response,
        &cfg.mcp_receipt_keys,
    )?;
    audit(cfg, &action, true, "MCP_RESPONSE_VERIFIED")?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jsonrpc_sse_parser_refuses_missing_matching_response() {
        let id = json!("x");
        let body = "event: notification\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n\
                    data: {\"jsonrpc\":\"2.0\",\"id\":\"x\",\"result\":{\"resultType\":\"complete\"}}\n\n";
        assert!(parse_reply(body, "text/event-stream", &id).is_ok());
        assert!(parse_reply(body, "text/event-stream", &json!("other")).is_err());
        assert!(parse_reply("{\"id\":\"no\"}", "application/json", &id).is_err());
    }
}

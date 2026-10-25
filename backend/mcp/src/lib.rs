//! Jani MCP Governance Contract v1.
//! Fail-closed signed contracts, initiator delegation and response attestations.
//! MCP transport/OAuth are separate from this authority kernel.
use jani_core::crypto;
use jani_core::types::{sha256_hex, Classification};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const PROTOCOL: &str = "jani.mcp.governance/v1";
pub const MCP_VERSION: &str = "2026-07-28";
pub const RECEIPT_META: &str = "com.ifinsta.jani/governance";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Signature {
    pub key_id: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    pub id: String,
    pub endpoint: String,
    pub audience: String,
    pub authorization_issuer: String,
    pub receipt_key_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Read,
    Prepare,
    UserCommit,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub tool: String,
    pub mode: Mode,
    pub required_roles: Vec<String>,
    pub required_entitlements: Vec<String>,
    pub input_schema: Value,
    pub max_request_classification: Classification,
    pub max_response_classification: Classification,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpContract {
    pub api_version: String,
    pub kind: String,
    pub server: Server,
    pub operations: BTreeMap<String, Operation>,
    pub signature: Option<Signature>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delegation {
    pub api_version: String,
    pub kind: String,
    pub tenant: String,
    pub initiator: String,
    pub agent: String,
    pub task_id: String,
    pub purpose: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub roles: Vec<String>,
    pub entitlements: Vec<String>,
    pub resources: Vec<String>,
    pub operations: Vec<String>,
    pub max_classification: Classification,
    pub signature: Option<Signature>,
}

#[derive(Clone, Debug)]
pub struct Context<'a> {
    pub tenant: &'a str,
    pub initiator: &'a str,
    pub agent: &'a str,
    pub task_id: &'a str,
    pub purpose: &'a str,
    pub now: u64,
    pub contract_keys: &'a [String],
    pub delegation_keys: &'a [String],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizedCall {
    pub version: String,
    pub server_id: String,
    pub tool: String,
    pub operation: String,
    pub resource: String,
    pub tenant: String,
    pub initiator: String,
    pub agent: String,
    pub task_id: String,
    pub purpose: String,
    pub role: String,
    pub entitlements: Vec<String>,
    pub contract_digest: String,
    pub delegation_digest: String,
    pub arguments: Value,
    pub request_digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseReceipt {
    pub request_digest: String,
    pub result_digest: String,
    pub tenant: String,
    pub initiator: String,
    pub agent: String,
    pub task_id: String,
    pub operation: String,
    pub resource: String,
    pub purpose: String,
    pub role: String,
    pub entitlements: Vec<String>,
    pub classification: Classification,
    pub filtered: bool,
    pub expires_at: u64,
    pub signature: Option<Signature>,
}

fn unsigned<T: Clone + Serialize>(
    value: &T,
    strip: impl FnOnce(&mut T),
) -> Result<Vec<u8>, String> {
    let mut clone = value.clone();
    strip(&mut clone);
    serde_json::to_vec(&clone).map_err(|e| e.to_string())
}

impl McpContract {
    pub fn verify(&self, keys: &[String]) -> Result<(), String> {
        if self.api_version != PROTOCOL || self.kind != "McpGovernanceContract" {
            return Err("CONTRACT_VERSION_UNSUPPORTED".into());
        }
        if self.server.id.is_empty() || self.server.audience.is_empty()
            || self.server.receipt_key_id.is_empty() || self.server.authorization_issuer.is_empty() {
            return Err("CONTRACT_MISSING_SERVER_TRUST".into());
        }
        let url = reqwest::Url::parse(&self.server.endpoint)
            .map_err(|_| "CONTRACT_ENDPOINT_INVALID".to_string())?;
        if url.scheme() != "https" || url.host_str().is_none()
            || url.username() != "" || url.password().is_some() || url.fragment().is_some() {
            return Err("CONTRACT_ENDPOINT_MUST_USE_HTTPS".into());
        }
        if self.server.audience != self.server.endpoint {
            return Err("CONTRACT_AUDIENCE_MISMATCH".into());
        }
        if self.operations.is_empty() || self.operations.len() > 128 {
            return Err("CONTRACT_OPERATIONS_INVALID".into());
        }
        for (name, operation) in &self.operations {
            if !identifier(name) || !identifier(&operation.tool)
                || operation.required_roles.is_empty()
                || operation.required_entitlements.is_empty()
                || operation.required_roles.iter().any(|x| !identifier(x))
                || operation.required_entitlements.iter().any(|x| !identifier(x))
                || !schema_is_closed(&operation.input_schema, 0)
                || operation.input_schema.get("type").and_then(Value::as_str) != Some("object")
                || operation.input_schema.get("additionalProperties") != Some(&Value::Bool(false))
                || operation.input_schema.pointer("/properties/resource/type").and_then(Value::as_str) != Some("string")
                || !operation.input_schema.get("required").and_then(Value::as_array)
                     .is_some_and(|a| a.iter().any(|x| x.as_str()==Some("resource"))) {
                return Err(format!("CONTRACT_UNSUPPORTED_OPERATION: {name}"));
            }
        }
        let sig = self.signature.as_ref().ok_or("CONTRACT_SIGNATURE_MISSING")?;
        crypto::verify_signature(keys, &sig.key_id, &unsigned(self, |x| x.signature = None)?, &sig.value)
            .map_err(|_| "CONTRACT_SIGNATURE_INVALID".into())
    }

    pub fn digest(&self) -> Result<String, String> {
        Ok(sha256_hex(&serde_json::to_vec(self).map_err(|e| e.to_string())?))
    }
}

impl Delegation {
    pub fn verify(&self, context: &Context<'_>) -> Result<(), String> {
        if self.api_version != PROTOCOL || self.kind != "McpDelegation" {
            return Err("DELEGATION_VERSION_UNSUPPORTED".into());
        }
        if self.tenant != context.tenant || self.initiator != context.initiator ||
            self.agent != context.agent || self.task_id != context.task_id ||
            self.purpose != context.purpose {
            return Err("DELEGATION_BINDING_MISMATCH".into());
        }
        if self.issued_at > context.now.saturating_add(30) ||
           self.expires_at <= context.now ||
           self.expires_at.saturating_sub(self.issued_at) > 3600 {
            return Err("DELEGATION_EXPIRED_OR_INVALID".into());
        }
        if self.roles.is_empty() || self.entitlements.is_empty()
            || self.resources.is_empty() || self.operations.is_empty()
            || self.roles.iter().any(|x| !identifier(x))
            || self.entitlements.iter().any(|x| !identifier(x))
            || self.resources.iter().any(|x| x.is_empty() || x.len() > 180 || x == "*")
            || self.operations.iter().any(|x| !identifier(x)) {
            return Err("DELEGATION_ENTITLEMENT_PROOF_MISSING".into());
        }
        let sig = self.signature.as_ref().ok_or("DELEGATION_SIGNATURE_MISSING")?;
        crypto::verify_signature(context.delegation_keys, &sig.key_id,
            &unsigned(self, |x| x.signature = None)?, &sig.value)
            .map_err(|_| "DELEGATION_SIGNATURE_INVALID".into())
    }

    pub fn digest(&self) -> Result<String, String> {
        Ok(sha256_hex(&serde_json::to_vec(self).map_err(|e| e.to_string())?))
    }
}

fn schema_is_closed(value: &Value, depth: usize) -> bool {
    if depth > 8 { return false; }
    let Some(obj) = value.as_object() else { return false; };
    if obj.keys().any(|k| !matches!(k.as_str(),
        "type" | "properties" | "required" | "additionalProperties" |
        "items" | "enum" | "minimum" | "maximum")) { return false; }
    if let Some(properties) = obj.get("properties") {
        let Some(props) = properties.as_object() else { return false; };
        if props.len() > 64 || props.values().any(|v| !schema_is_closed(v,depth+1)) {
            return false;
        }
    }
    if let Some(items) = obj.get("items") {
        if !schema_is_closed(items, depth+1) { return false; }
    }
    if let Some(required) = obj.get("required") {
        if required.as_array().is_none_or(|r| r.iter().any(|v| !v.is_string())) {
            return false;
        }
    }
    if obj.get("additionalProperties").is_some_and(|v| v != &Value::Bool(false)) {
        return false;
    }
    true
}

fn identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 96
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-'|b'_'|b'.'|b':'))
}

/// Only exact resources are supported in v1; wildcard authorization belongs to
/// a future, independently evaluated resource authorization service.
pub fn authorize(
    contract: &McpContract, delegation: &Delegation,
    context: &Context<'_>, operation: &str, resource: &str, arguments: Value,
) -> Result<AuthorizedCall, String> {
    contract.verify(context.contract_keys)?;
    delegation.verify(context)?;
    let spec = contract.operations.get(operation)
        .ok_or("CONTRACT_UNSUPPORTED_OPERATION")?;
    if spec.mode == Mode::UserCommit {
        return Err("USER_COMMIT_REQUIRED: Jani never possesses final submission authority".into());
    }
    if !delegation.operations.iter().any(|x| x == operation) ||
       !delegation.resources.iter().any(|x| x == resource) {
        return Err("DELEGATION_SCOPE_DENIED".into());
    }
    let role = spec.required_roles.iter().find(|role|
        delegation.roles.iter().any(|granted| granted == *role)
    ).ok_or("ENTITLEMENT_PROOF_MISSING: required verified role")?;
    if spec.required_entitlements.iter().any(|ent|
        !delegation.entitlements.iter().any(|granted| granted == ent)
    ) {
        return Err("PERMISSION_DENIED: missing required permission".into());
    }
    jani_core::schema::validate(&arguments, &spec.input_schema)
        .map_err(|_| "CONTRACT_ARGUMENTS_SCHEMA_VIOLATION")?;
    let object = arguments.as_object().ok_or("CONTRACT_ARGUMENTS_INVALID: expected object")?;
    if object.get("resource").and_then(Value::as_str) != Some(resource) {
        return Err("CONTRACT_RESOURCE_ARGUMENT_MISMATCH".into());
    }
    if resource.is_empty() || resource.len() > 180 || resource == "*" {
        return Err("CONTRACT_RESOURCE_INVALID".into());
    }
    let mut call = AuthorizedCall {
        version: PROTOCOL.into(), server_id: contract.server.id.clone(),
        tool: spec.tool.clone(), operation: operation.into(), resource: resource.into(),
        tenant: context.tenant.into(), initiator: context.initiator.into(),
        agent: context.agent.into(), task_id: context.task_id.into(),
        purpose: context.purpose.into(), role: role.to_string(),
        entitlements: spec.required_entitlements.clone(),
        contract_digest: contract.digest()?,
        delegation_digest: delegation.digest()?,
        arguments, request_digest: String::new(),
    };
    call.request_digest = sha256_hex(&serde_json::to_vec(&call).map_err(|e| e.to_string())?);
    Ok(call)
}

/// Verified receipts must be signed by an explicitly trusted MCP server key and
/// bind the actual result payload to the exact authorized request.
pub fn verify_response(
    contract: &McpContract, grant: &Delegation, call: &AuthorizedCall,
    response: &Value, keys: &[String], now: u64,
) -> Result<Value, String> {
    let top = response.as_object().ok_or("CONTRACT_RESPONSE_VIOLATION")?;
    if top.get("jsonrpc").and_then(Value::as_str) != Some("2.0") ||
       top.get("error").is_some() {
        return Err("MCP_RPC_ERROR_OR_INVALID_RESPONSE".into());
    }
    let result = top.get("result").and_then(Value::as_object)
        .ok_or("CONTRACT_RESPONSE_VIOLATION: result missing")?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) ||
       result.get("resultType").and_then(Value::as_str) != Some("complete") {
        return Err("CONTRACT_RESPONSE_VIOLATION: incomplete or failed result".into());
    }
    let meta = result.get("_meta").and_then(Value::as_object)
        .ok_or("CONTRACT_RESPONSE_VIOLATION: governance metadata missing")?;
    let attestation = meta.get(RECEIPT_META)
        .ok_or("ENTITLEMENT_PROOF_MISSING: signed response receipt missing")?;
    let receipt: ResponseReceipt = serde_json::from_value(attestation.clone())
        .map_err(|_| "CONTRACT_RESPONSE_VIOLATION: malformed receipt")?;
    let mut payload = result.clone();
    let drop_meta = if let Some(Value::Object(metadata)) = payload.get_mut("_meta") {
        metadata.remove(RECEIPT_META);
        metadata.is_empty()
    } else { false };
    if drop_meta { payload.remove("_meta"); }
    let digest = sha256_hex(&serde_json::to_vec(&Value::Object(payload.clone()))
        .map_err(|e| e.to_string())?);
    if receipt.request_digest != call.request_digest ||
       receipt.result_digest != digest || receipt.tenant != call.tenant ||
       receipt.initiator != call.initiator || receipt.agent != call.agent ||
       receipt.task_id != call.task_id || receipt.operation != call.operation ||
       receipt.resource != call.resource || receipt.purpose != call.purpose ||
       receipt.role != call.role || receipt.entitlements != call.entitlements ||
       receipt.expires_at <= now || receipt.expires_at > now.saturating_add(300) ||
       !receipt.filtered {
        return Err("CONTRACT_RESPONSE_VIOLATION: incorrect scope or payload".into());
    }
    let max = contract.operations.get(&call.operation)
        .ok_or("CONTRACT_UNSUPPORTED_OPERATION")?.max_response_classification;
    if receipt.classification > max || receipt.classification > grant.max_classification
        || receipt.classification == Classification::Secret {
        return Err("DATA_CLASSIFICATION_DENIED".into());
    }
    let sig = receipt.signature.as_ref().ok_or("CONTRACT_RESPONSE_VIOLATION: unsigned receipt")?;
    if sig.key_id != contract.server.receipt_key_id {
        return Err("CONTRACT_RESPONSE_VIOLATION: incorrect receipt signing key".into());
    }
    crypto::verify_signature(keys, &sig.key_id,
        &unsigned(&receipt, |x| x.signature = None)?, &sig.value)
        .map_err(|_| "CONTRACT_RESPONSE_VIOLATION: untrusted receipt")?;
    Ok(json!({"result":Value::Object(payload),"classification":receipt.classification,
        "requestDigest":call.request_digest,"receiptVerified":true,
        "operation":call.operation,"resource":call.resource}))
}


#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use jani_core::types::Classification;
    use serde_json::{json, Value};

    fn signature(secret: u8, bytes: &[u8], key_id: &str) -> Signature {
        let key = SigningKey::from_bytes(&[secret; 32]);
        Signature { key_id: key_id.into(), value: hex::encode(key.sign(bytes).to_bytes()) }
    }

    fn trust(secret: u8, key_id: &str) -> Vec<String> {
        let key = SigningKey::from_bytes(&[secret; 32]);
        vec![format!("{key_id}:{}", hex::encode(key.verifying_key().to_bytes()))]
    }

    fn sample() -> (McpContract, Delegation) {
        let mut operations = BTreeMap::new();
        operations.insert("organization.news.read".into(), Operation {
            tool:"news_read".into(),mode:Mode::Read,
            required_roles:vec!["employee".into()],
            required_entitlements:vec!["news.read".into()],
            input_schema:json!({"type":"object","properties":{"resource":{"type":"string"},"limit":{"type":"integer"}},"required":["resource"],"additionalProperties":false}),
            max_request_classification:Classification::Internal,
            max_response_classification:Classification::Confidential,
        });
        operations.insert("records.remediation.prepare".into(), Operation {
            tool:"remediation_prepare".into(),mode:Mode::Prepare,
            required_roles:vec!["verifier".into()],
            required_entitlements:vec!["records.propose".into()],
            input_schema:json!({"type":"object","properties":{"resource":{"type":"string"}},"required":["resource"],"additionalProperties":false}),
            max_request_classification:Classification::Restricted,
            max_response_classification:Classification::Restricted,
        });
        operations.insert("records.update".into(), Operation {
            tool:"records_update".into(),mode:Mode::UserCommit,
            required_roles:vec!["admin".into()],
            required_entitlements:vec!["records.update".into()],
            input_schema:json!({"type":"object","properties":{"resource":{"type":"string"}},"required":["resource"],"additionalProperties":false}),
            max_request_classification:Classification::Restricted,
            max_response_classification:Classification::Restricted,
        });
        let mut contract = McpContract {
            api_version:PROTOCOL.into(),kind:"McpGovernanceContract".into(),
            server:Server {
                id:"organization-records".into(),
                endpoint:"https://records.example.com/mcp".into(),
                audience:"https://records.example.com/mcp".into(),
                authorization_issuer:"https://auth.example.com".into(),
                receipt_key_id:"mcp-server".into(),
            },
            operations,signature:None,
        };
        contract.signature = Some(signature(0x11,
            &unsigned(&contract, |x| x.signature=None).unwrap(), "contract"));
        let mut grant = Delegation {
            api_version:PROTOCOL.into(),kind:"McpDelegation".into(),
            tenant:"tenant-1".into(), initiator:"user-1".into(), agent:"jani".into(),
            task_id:"task-1".into(), purpose:"DATA_REMEDIATION".into(),
            issued_at:100, expires_at:300,
            roles:vec!["employee".into(),"verifier".into()],
            entitlements:vec!["news.read".into(),"records.propose".into()],
            resources:vec!["organization:news".into(),"records:case-3".into()],
            operations:vec!["organization.news.read".into(),"records.remediation.prepare".into()],
            max_classification:Classification::Restricted,
            signature:None,
        };
        grant.signature=Some(signature(0x22,
            &unsigned(&grant,|x| x.signature=None).unwrap(),"authorization"));
        (contract,grant)
    }

    fn ctx<'a>(contract_keys: &'a [String], delegation_keys: &'a [String]) -> Context<'a> {
        Context {
            tenant:"tenant-1",initiator:"user-1",agent:"jani",
            task_id:"task-1",purpose:"DATA_REMEDIATION",now:200,
            contract_keys,delegation_keys,
        }
    }

    fn read_call(contract:&McpContract, grant:&Delegation) -> AuthorizedCall {
        let c=trust(0x11,"contract");let d=trust(0x22,"authorization");
        authorize(contract, grant,&ctx(&c,&d),
            "organization.news.read","organization:news",
            json!({"resource":"organization:news","limit":5})).unwrap()
    }

    #[test]
    fn permits_organization_news_only_with_verified_delegation() {
        let (c,d)=sample();
        let call=read_call(&c,&d);
        assert_eq!(call.role,"employee");
        assert_eq!(call.entitlements,vec!["news.read"]);
        assert_eq!(call.resource,"organization:news");
        assert_eq!(call.agent,"jani");
    }

    #[test]
    fn prohibits_user_only_commit_under_all_grants() {
        let (c,mut d)=sample();
        d.roles.push("admin".into());
        d.entitlements.push("records.update".into());
        d.operations.push("records.update".into());
        d.signature=None;
        d.signature=Some(signature(0x22,&unsigned(&d,|x| x.signature=None).unwrap(),"authorization"));
        let keys=trust(0x11,"contract");let grant_keys=trust(0x22,"authorization");
        let result=authorize(&c,&d,&ctx(&keys,&grant_keys),"records.update","records:case-3",
            json!({"resource":"records:case-3"}));
        assert!(result.unwrap_err().contains("USER_COMMIT_REQUIRED"));
    }

    #[test]
    fn denies_forged_expired_scope_missing_role_missing_permission_and_wildcard() {
        let (c,d)=sample();
        let keys=trust(0x11,"contract");let grant_keys=trust(0x22,"authorization");
        let context=ctx(&keys,&grant_keys);
        let mut tampered=d.clone();
        tampered.entitlements.push("records.update".into());
        assert!(authorize(&c,&tampered,&context,"organization.news.read","organization:news",
            json!({"resource":"organization:news"})).is_err());
        let mut expired=ctx(&keys,&grant_keys);
        expired.now=301;
        assert!(authorize(&c,&d,&expired,"organization.news.read","organization:news",
            json!({"resource":"organization:news"})).is_err());
        let wrong_user=Context{initiator:"attacker",..ctx(&keys,&grant_keys)};
        assert!(authorize(&c,&d,&wrong_user,"organization.news.read","organization:news",
            json!({"resource":"organization:news"})).is_err());
        assert!(authorize(&c,&d,&context,"records.remediation.prepare","records:other",
            json!({"resource":"records:other"})).is_err());
        assert!(authorize(&c,&d,&context,"unknown.write","organization:news",
            json!({"resource":"organization:news"})).is_err());
        assert!(authorize(&c,&d,&context,"organization.news.read","organization:news",
            json!({"resource":"organization:other"})).is_err());
        assert!(authorize(&c,&d,&context,"organization.news.read","*",
            json!({"resource":"*"})).is_err());

        let mut no_role=d.clone();
        no_role.roles=vec!["reader".into()];
        no_role.signature=None;
        no_role.signature=Some(signature(0x22,&unsigned(&no_role,|x|x.signature=None).unwrap(),"authorization"));
        assert!(authorize(&c,&no_role,&context,"organization.news.read","organization:news",
            json!({"resource":"organization:news"})).unwrap_err().contains("ENTITLEMENT"));

        let mut no_permission=d.clone();
        no_permission.entitlements=vec!["other.read".into()];
        no_permission.signature=None;
        no_permission.signature=Some(signature(0x22,&unsigned(&no_permission,|x|x.signature=None).unwrap(),"authorization"));
        assert!(authorize(&c,&no_permission,&context,"organization.news.read","organization:news",
            json!({"resource":"organization:news"})).unwrap_err().contains("PERMISSION_DENIED"));
    }

    fn signed_response(call:&AuthorizedCall, classification:Classification) -> Value {
        let body=json!({"resultType":"complete","content":[{"type":"text","text":"organization news"}],
            "isError":false});
        let mut receipt=ResponseReceipt {
            request_digest:call.request_digest.clone(),
            result_digest:sha256_hex(&serde_json::to_vec(&body).unwrap()),
            tenant:call.tenant.clone(),initiator:call.initiator.clone(),
            agent:call.agent.clone(),task_id:call.task_id.clone(),
            operation:call.operation.clone(),resource:call.resource.clone(),
            purpose:call.purpose.clone(),role:call.role.clone(),
            entitlements:call.entitlements.clone(),
            classification, filtered:true,expires_at:220,signature:None,
        };
        receipt.signature=Some(signature(0x33,&unsigned(&receipt,|r|r.signature=None).unwrap(),"mcp-server"));
        let mut body=body;
        let mut receipt_meta = serde_json::Map::new();
        receipt_meta.insert(RECEIPT_META.into(), json!(receipt));
        body.as_object_mut().unwrap().insert("_meta".into(), Value::Object(receipt_meta));
        json!({"jsonrpc":"2.0","id":"rpc-1","result":body})
    }

    #[test]
    fn verifies_signed_filtered_classified_response_and_denies_tampering() {
        let (c,d)=sample();
        let call=read_call(&c,&d);
        let keys=trust(0x33,"mcp-server");
        let response=signed_response(&call,Classification::Internal);
        let verified=verify_response(&c,&d,&call,&response,&keys,200).unwrap();
        assert_eq!(verified["receiptVerified"],true);
        assert_eq!(verified["classification"],"INTERNAL");
        let mut tampered=response.clone();
        tampered["result"]["content"][0]["text"]=json!("a different secret");
        assert!(verify_response(&c,&d,&call,&tampered,&keys,200).is_err());
        let mut unclassified=response.clone();
        unclassified["result"]["_meta"]=json!({});
        assert!(verify_response(&c,&d,&call,&unclassified,&keys,200).is_err());
        let classified=signed_response(&call,Classification::Restricted);
        assert!(verify_response(&c,&d,&call,&classified,&keys,200).is_err());
        assert!(verify_response(&c,&d,&call,&response,&keys,221).is_err());
        assert!(verify_response(&c,&d,&call,&response,&trust(0x44,"mcp-server"),200).is_err());
    }
}

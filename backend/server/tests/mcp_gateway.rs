//! Contract conformance from the real Jani CLI without model credentials.
//! Creates short-lived signed fixtures locally; signing keys never enter Jani.
use ed25519_dalek::{Signer, SigningKey};
use jani_core::identity::{IdentityAssertion, IdentitySignature};
use jani_core::types::{now_unix, Classification};
use jani_mcp::{Delegation, McpContract, Mode, Operation, Server, Signature, PROTOCOL};
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, path::PathBuf};

mod support;

fn key(seed: u8, label: &str) -> String {
    let sk = SigningKey::from_bytes(&[seed; 32]);
    format!("{label}:{}", hex::encode(sk.verifying_key().to_bytes()))
}
fn sign<T: Serialize>(val: &T, seed: u8, label: &str) -> Signature {
    let sk = SigningKey::from_bytes(&[seed; 32]);
    Signature {
        key_id: label.into(),
        value: hex::encode(sk.sign(&serde_json::to_vec(val).unwrap()).to_bytes()),
    }
}
fn dir() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "jani-mcp-contract-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}
#[test]
fn verified_initiator_can_read_news_but_not_commit_records() {
    let root = dir();
    let now = now_unix();
    let mut identity = IdentityAssertion {
        api_version: "jani.dev/v1alpha1".into(),
        kind: "IdentityAssertion".into(),
        principal: "user-7".into(),
        tenant: "org-1".into(),
        subject_type: "human".into(),
        issuer: "idp".into(),
        issued_at: now - 5,
        expires_at: now + 600,
        nonce: "nonce-test".into(),
        signature: None,
    };
    let sig = sign(
        &{
            let mut i = identity.clone();
            i.signature = None;
            i
        },
        0x11,
        "identity",
    );
    identity.signature = Some(IdentitySignature {
        key_id: sig.key_id,
        value: sig.value,
    });
    let mut operations = BTreeMap::new();
    operations.insert("organization.news.read".into(),Operation{
        tool:"news_read".into(),mode:Mode::Read,required_roles:vec!["employee".into()],
        required_entitlements:vec!["news.read".into()],
        input_schema:json!({"type":"object","properties":{"resource":{"type":"string"}},"required":["resource"],"additionalProperties":false}),
        max_request_classification:Classification::Internal,
        max_response_classification:Classification::Internal,
    });
    operations.insert("records.update".into(),Operation{
        tool:"records_update".into(),mode:Mode::UserCommit,required_roles:vec!["editor".into()],
        required_entitlements:vec!["records.update".into()],
        input_schema:json!({"type":"object","properties":{"resource":{"type":"string"}},"required":["resource"],"additionalProperties":false}),
        max_request_classification:Classification::Restricted,
        max_response_classification:Classification::Restricted,
    });
    let mut contract = McpContract {
        api_version: PROTOCOL.into(),
        kind: "McpGovernanceContract".into(),
        server: Server {
            id: "org-data".into(),
            endpoint: "https://records.example.com/mcp".into(),
            audience: "https://records.example.com/mcp".into(),
            authorization_issuer: "https://auth.example.com".into(),
            receipt_key_id: "server".into(),
        },
        operations,
        signature: None,
    };
    let unsigned = {
        let mut c = contract.clone();
        c.signature = None;
        c
    };
    contract.signature = Some(sign(&unsigned, 0x22, "contract"));
    let mut grant = Delegation {
        api_version: PROTOCOL.into(),
        kind: "McpDelegation".into(),
        tenant: "org-1".into(),
        initiator: "user-7".into(),
        agent: "jani".into(),
        task_id: "task-1".into(),
        purpose: "SOFTWARE_DEV".into(),
        issued_at: now - 5,
        expires_at: now + 300,
        roles: vec!["employee".into(), "editor".into()],
        entitlements: vec!["news.read".into(), "records.update".into()],
        resources: vec!["organization:news".into()],
        operations: vec!["organization.news.read".into(), "records.update".into()],
        max_classification: Classification::Restricted,
        signature: None,
    };
    let unsigned = {
        let mut g = grant.clone();
        g.signature = None;
        g
    };
    grant.signature = Some(sign(&unsigned, 0x33, "authorization"));
    let id_path = root.join("identity.json");
    let contract_path = root.join("contract.json");
    let grant_path = root.join("grant.json");
    let args_path = root.join("args.json");
    fs::write(&id_path, serde_json::to_vec(&identity).unwrap()).unwrap();
    fs::write(&contract_path, serde_json::to_vec(&contract).unwrap()).unwrap();
    fs::write(&grant_path, serde_json::to_vec(&grant).unwrap()).unwrap();
    fs::write(
        &args_path,
        serde_json::to_vec(&json!({"resource":"organization:news"})).unwrap(),
    )
    .unwrap();
    let run = |op: &str, resource: &str| {
        support::jani()
            .args([
                "mcp",
                "authorize",
                op,
                resource,
                "--repo",
                root.to_str().unwrap(),
                "--state-dir",
                root.join(".jani").to_str().unwrap(),
                "--tenant",
                "org-1",
                "--principal",
                "user-7",
                "--task",
                "task-1",
                "--purpose",
                "SOFTWARE_DEV",
                "--identity",
                id_path.to_str().unwrap(),
                "--policy-key",
                &key(0x11, "identity"),
                "--mcp-contract",
                contract_path.to_str().unwrap(),
                "--mcp-delegation",
                grant_path.to_str().unwrap(),
                "--mcp-arguments",
                args_path.to_str().unwrap(),
                "--mcp-contract-key",
                &key(0x22, "contract"),
                "--mcp-delegation-key",
                &key(0x33, "authorization"),
            ])
            .env_remove("JANI_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("JANI_POLICY")
            .env_remove("JANI_MODE")
            .output()
            .expect("execute governed MCP CLI")
    };
    let permitted = run("organization.news.read", "organization:news");
    assert!(
        permitted.status.success(),
        "{}",
        String::from_utf8_lossy(&permitted.stderr)
    );
    let result: Value = serde_json::from_slice(&permitted.stdout).unwrap();
    assert_eq!(result["authorized"], true);
    assert_eq!(result["role"], "employee");
    assert_eq!(result["networkInvoked"], false);
    assert!(result.get("arguments").is_none());

    let forbidden = run("records.update", "organization:news");
    assert!(!forbidden.status.success());
    assert!(String::from_utf8_lossy(&forbidden.stderr).contains("USER_COMMIT_REQUIRED"));

    let unsupported = run("undeclared.write", "organization:news");
    assert!(!unsupported.status.success());
    assert!(String::from_utf8_lossy(&unsupported.stderr).contains("CONTRACT_UNSUPPORTED_OPERATION"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unsigned_mcp_contract_fails_closed() {
    let root = dir();
    let result = support::jani()
        .args(["mcp", "verify", "--repo", root.to_str().unwrap()])
        .env_remove("JANI_POLICY")
        .env_remove("JANI_IDENTITY")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("AUTH_CONTEXT_MISSING"));
    fs::remove_dir_all(root).unwrap();
}

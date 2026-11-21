//! IRONCLAD S004: actual native kernels; all identities and signing keys are
//! synthetic test fixtures, not real banks or production authority.
use ed25519_dalek::{Signer, SigningKey};
use jani_core::assignment::{self, AssignmentContext, AssignmentGrant, SignedAssignment};
use jani_core::identity::{IdentityAssertion, IdentitySignature, VerifiedIdentity};
use jani_core::types::{sha256_hex, Classification};
use jani_mcp::{
    self as mcp, AuthorizedCall, Context, Delegation, McpContract, Mode, Operation,
    ResponseReceipt, Server, Signature, PROTOCOL, RECEIPT_META,
};
use jani_store::dispatch::{DispatchStore, Offer, Review, Role};
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, path::PathBuf};

const NOW: u64 = 10000;
fn key(seed: u8, label: &str) -> String {
    format!(
        "{label}:{}",
        hex::encode(
            SigningKey::from_bytes(&[seed; 32])
                .verifying_key()
                .to_bytes()
        )
    )
}
fn sig<T: Serialize>(v: &T, seed: u8, label: &str) -> Signature {
    Signature {
        key_id: label.into(),
        value: hex::encode(
            SigningKey::from_bytes(&[seed; 32])
                .sign(&serde_json::to_vec(v).unwrap())
                .to_bytes(),
        ),
    }
}
fn assignment(g: AssignmentGrant) -> SignedAssignment {
    let signature = hex::encode(
        SigningKey::from_bytes(&[42; 32])
            .sign(&g.signing_bytes().unwrap())
            .to_bytes(),
    );
    SignedAssignment {
        grant: g,
        key_id: "dispatch".into(),
        signature,
    }
}
fn fixture(n: usize) -> (AssignmentContext, Vec<u8>, VerifiedIdentity) {
    let tenant = format!("bank-{}", n % 5);
    let task_id = format!("T{n:04}");
    let contract = serde_json::to_vec(
        &json!({"apiVersion":"jani.dev/v1alpha1","kind":"WorkContract",
      "metadata":{"tenant":tenant,"principal":"operator","taskId":task_id,"workspace":"synthetic"},
      "spec":{"purpose":"REVIEW","capabilities":["repo.read"]}}),
    )
    .unwrap();
    let c = AssignmentContext {
        tenant: tenant.clone(),
        principal: "operator".into(),
        task_id,
        agent: format!("jani-{}", n % 3),
        workspace: "synthetic".into(),
        purpose: "REVIEW".into(),
        epoch: 1,
        base_commit: "a".repeat(40),
        policy_sha256: "b".repeat(64),
        contract_sha256: sha256_hex(&contract),
    };
    // Exercise actual identity signature verifier. Fixed NOW is for assignment tests;
    // the identity verifier uses the real OS clock.
    let real = jani_core::types::now_unix();
    let mut a = IdentityAssertion {
        api_version: "jani.dev/v1alpha1".into(),
        kind: "IdentityAssertion".into(),
        principal: c.principal.clone(),
        tenant: c.tenant.clone(),
        subject_type: "human".into(),
        issuer: "fixture-idp".into(),
        issued_at: real - 1,
        expires_at: real + 600,
        nonce: format!("identity-{n}"),
        signature: None,
    };
    let s = sig(&a, 41, "identity");
    a.signature = Some(IdentitySignature {
        key_id: s.key_id,
        value: s.value,
    });
    a.verify(&[key(41, "identity")]).unwrap();
    let i = VerifiedIdentity {
        principal: a.principal,
        tenant: a.tenant,
        subject_type: a.subject_type,
        issuer: a.issuer,
        verified: true,
        expires_at: Some(NOW + 600),
    };
    (c, contract, i)
}
fn grant(c: &AssignmentContext, nonce: &str) -> AssignmentGrant {
    AssignmentGrant {
        context: c.clone(),
        issued_at: NOW - 1,
        expires_at: NOW + 60,
        nonce: nonce.into(),
    }
}
fn offer(c: &AssignmentContext) -> Offer {
    Offer {
        tenant: c.tenant.clone(),
        department: "risk".into(),
        task: c.task_id.clone(),
        operator: c.principal.clone(),
        agent: c.agent.clone(),
        epoch: c.epoch,
        expires: NOW + 60,
        context: serde_json::to_string(c).unwrap(),
    }
}
// Simulated trusted manager: derive expected from stored offer in the same
// transaction as nonce consumption; no worker chooses the expected context.
fn admit(
    store: &mut DispatchStore,
    c: &AssignmentContext,
    bytes: &[u8],
    i: &VerifiedIdentity,
    s: &SignedAssignment,
    now: u64,
) -> Result<(), String> {
    store.admit(&c.tenant, &c.task_id, c.epoch, &i.principal, now, |o| {
        let expected: AssignmentContext =
            serde_json::from_str(&o.context).map_err(|e| e.to_string())?;
        if o.tenant != expected.tenant
            || o.task != expected.task_id
            || o.agent != expected.agent
            || o.operator != expected.principal
            || o.epoch != expected.epoch
            || s.grant.expires_at > o.expires
        {
            return Err("lease/context mismatch".into());
        }
        assignment::verify(s, &expected, bytes, i, &[key(42, "dispatch")], now)?;
        Ok((s.grant.nonce.clone(), s.grant.expires_at))
    })
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(jani_core::types::new_id("ironclad"));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn provision(s: &mut DispatchStore, tenant: &str) {
    for (p, r) in [
        ("manager", Role::Manager),
        ("supervisor", Role::Supervisor),
        ("operator", Role::Operator),
        ("reviewer", Role::Reviewer),
        ("auditor", Role::Auditor),
        ("other-operator", Role::Operator),
    ] {
        s.provision(tenant, "risk", p, r).unwrap();
    }
}
#[test]
fn manager_approval_signed_admission_replay_and_independent_review() {
    let t = Temp::new();
    let path = t.0.join("dispatch.db");
    let (c, b, i) = fixture(1);
    let mut s = DispatchStore::open(&path).unwrap();
    provision(&mut s, &c.tenant);
    let o = offer(&c);
    let g = assignment(grant(&c, "nonce-one"));
    assert!(s.propose("auditor", &o, NOW).is_err());
    assert!(s.propose("operator", &o, NOW).is_err());
    s.propose("manager", &o, NOW).unwrap();
    assert!(admit(&mut s, &c, &b, &i, &g, NOW).is_err());
    assert!(s.approve(&c.tenant, &c.task_id, 1, "manager", NOW).is_err());
    s.approve(&c.tenant, &c.task_id, 1, "supervisor", NOW)
        .unwrap();
    let mut bad = g.clone();
    bad.signature = "00".repeat(64);
    assert!(admit(&mut s, &c, &b, &i, &bad, NOW).is_err());
    admit(&mut s, &c, &b, &i, &g, NOW).unwrap();
    assert!(admit(&mut s, &c, &b, &i, &g, NOW).is_err());
    let e = "c".repeat(64);
    let r = "d".repeat(64);
    assert!(s
        .submit(&c.tenant, &c.task_id, 1, "other-operator", &e, NOW)
        .is_err());
    s.submit(&c.tenant, &c.task_id, 1, "operator", &e, NOW)
        .unwrap();
    assert!(s
        .review(
            &c.tenant,
            &c.task_id,
            1,
            "operator",
            Review {
                expected: &e,
                review_digest: &r,
                accept: true
            },
            NOW
        )
        .is_err());
    assert!(s
        .review(
            &c.tenant,
            &c.task_id,
            1,
            "reviewer",
            Review {
                expected: &e,
                review_digest: &e,
                accept: true
            },
            NOW
        )
        .is_err());
    s.review(
        &c.tenant,
        &c.task_id,
        1,
        "reviewer",
        Review {
            expected: &e,
            review_digest: &r,
            accept: true,
        },
        NOW,
    )
    .unwrap();
    drop(s);
    let mut s = DispatchStore::open(&path).unwrap();
    let (l, n) = s.audit(&c.tenant, &c.task_id, "auditor").unwrap();
    assert_eq!(l.state, "accepted");
    assert_eq!(n, 5);
    assert!(s.audit("bank-9", &c.task_id, "auditor").is_err());
    let (c2, b2, i2) = fixture(6);
    assert_eq!(c.tenant, c2.tenant);
    s.propose("manager", &offer(&c2), NOW).unwrap();
    s.approve(&c2.tenant, &c2.task_id, 1, "supervisor", NOW)
        .unwrap();
    assert!(admit(
        &mut s,
        &c2,
        &b2,
        &i2,
        &assignment(grant(&c2, "nonce-one")),
        NOW
    )
    .is_err());
    admit(
        &mut s,
        &c2,
        &b2,
        &i2,
        &assignment(grant(&c2, "nonce-two")),
        NOW,
    )
    .unwrap();
    s.revoke(&c2.tenant, &c2.task_id, 1, "supervisor", NOW)
        .unwrap();
    assert!(s
        .submit(&c2.tenant, &c2.task_id, 1, "operator", &e, NOW)
        .is_err());
    let mut next = offer(&c2);
    next.epoch = 2;
    assert!(s.propose("manager", &next, NOW).is_err());
    assert_eq!(
        s.audit(&c2.tenant, &c2.task_id, "auditor").unwrap().0.state,
        "uncertain"
    );
}
#[test]
fn revocation_and_department_fences() {
    let t = Temp::new();
    let mut s = DispatchStore::open(&t.0.join("dispatch.db")).unwrap();
    let (c, b, i) = fixture(9);
    provision(&mut s, &c.tenant);
    s.provision(&c.tenant, "sales", "foreign-manager", Role::Manager)
        .unwrap();
    assert!(s.propose("foreign-manager", &offer(&c), NOW).is_err());
    s.propose("manager", &offer(&c), NOW).unwrap();
    s.approve(&c.tenant, &c.task_id, 1, "supervisor", NOW)
        .unwrap();
    s.disable(&c.tenant, "risk", "supervisor").unwrap();
    assert!(admit(&mut s, &c, &b, &i, &assignment(grant(&c, "nonce-r")), NOW).is_err());
}

fn mcp_fixture(c: &AssignmentContext, resource: &str) -> (McpContract, Delegation) {
    let mut operations = BTreeMap::new();
    for (name, tool, mode, role, entitlement) in [
        (
            "bank.record.read",
            "record_read",
            Mode::Read,
            "operator",
            "records.read",
        ),
        (
            "bank.record.remediation",
            "remediation_prepare",
            Mode::Prepare,
            "operator",
            "records.propose",
        ),
        (
            "bank.record.commit",
            "record_commit",
            Mode::UserCommit,
            "operator",
            "records.commit",
        ),
    ] {
        operations.insert(name.into(),Operation{tool:tool.into(),mode,required_roles:vec![role.into()],
            required_entitlements:vec![entitlement.into()],
            input_schema:json!({"type":"object","properties":{"resource":{"type":"string"}},"required":["resource"],"additionalProperties":false}),
            max_request_classification:Classification::Internal,max_response_classification:Classification::Internal});
    }
    let mut contract = McpContract {
        api_version: PROTOCOL.into(),
        kind: "McpGovernanceContract".into(),
        server: Server {
            id: "synthetic-bank".into(),
            endpoint: "https://synthetic.invalid/mcp".into(),
            audience: "https://synthetic.invalid/mcp".into(),
            authorization_issuer: "https://synthetic.invalid/idp".into(),
            receipt_key_id: "source".into(),
        },
        operations,
        signature: None,
    };
    contract.signature = Some(sig(&contract, 43, "contract"));
    let mut d = Delegation {
        api_version: PROTOCOL.into(),
        kind: "McpDelegation".into(),
        tenant: c.tenant.clone(),
        initiator: c.principal.clone(),
        agent: c.agent.clone(),
        task_id: c.task_id.clone(),
        purpose: c.purpose.clone(),
        issued_at: NOW - 1,
        expires_at: NOW + 60,
        roles: vec!["operator".into()],
        entitlements: vec![
            "records.read".into(),
            "records.propose".into(),
            "records.commit".into(),
        ],
        resources: vec![resource.into()],
        operations: vec![
            "bank.record.read".into(),
            "bank.record.remediation".into(),
            "bank.record.commit".into(),
        ],
        max_classification: Classification::Internal,
        signature: None,
    };
    resign(&mut d);
    (contract, d)
}
fn resign(d: &mut Delegation) {
    d.signature = None;
    d.signature = Some(sig(d, 44, "delegation"));
}
fn context<'a>(c: &'a AssignmentContext, keys: &'a [String], grants: &'a [String]) -> Context<'a> {
    Context {
        tenant: &c.tenant,
        initiator: &c.principal,
        agent: &c.agent,
        task_id: &c.task_id,
        purpose: &c.purpose,
        now: NOW,
        contract_keys: keys,
        delegation_keys: grants,
    }
}
// Independent synthetic source scope. A real server must instead validate
// OAuth and live database row ACLs; this is an in-process tools/call fixture.
fn authorize_source(
    c: &AssignmentContext,
    contract: &McpContract,
    d: &Delegation,
    call: &AuthorizedCall,
    n: usize,
) -> Result<Value, String> {
    authorize_source_at(c, contract, d, call, n, NOW)
}
fn authorize_source_at(
    c: &AssignmentContext,
    contract: &McpContract,
    d: &Delegation,
    call: &AuthorizedCall,
    n: usize,
    now: u64,
) -> Result<Value, String> {
    let actual_tenant = format!("bank-{}", n % 5);
    let actual_resource = format!("customer:{actual_tenant}:{n}");
    if c.tenant != actual_tenant
        || c.principal != "operator"
        || call.resource != actual_resource
        || call.operation != "bank.record.read" && call.operation != "bank.record.remediation"
    {
        return Err("SOURCE_SCOPE_DENIED".into());
    }
    let keys = [key(43, "contract")];
    let grants = [key(44, "delegation")];
    let mut source_context = context(c, &keys, &grants);
    source_context.now = now;
    let authorized = mcp::authorize(
        contract,
        d,
        &source_context,
        &call.operation,
        &actual_resource,
        call.arguments.clone(),
    )?;
    if authorized.request_digest != call.request_digest {
        return Err("SOURCE_REQUEST_MISMATCH".into());
    }
    let data = if call.operation == "bank.record.read" {
        json!({"syntheticId":n,"kycComplete":!n.is_multiple_of(97),"balanceRuleFlag":n.is_multiple_of(101)})
    } else {
        json!({"syntheticId":n,"proposalOnly":true,"committed":false})
    };
    let payload = json!({"resultType":"complete","isError":false,"structuredContent":data});
    Ok(receipt(
        call,
        payload,
        Classification::Internal,
        true,
        now + 30,
        45,
    ))
}
fn receipt(
    call: &AuthorizedCall,
    mut payload: Value,
    classification: Classification,
    filtered: bool,
    expiry: u64,
    seed: u8,
) -> Value {
    let mut r = ResponseReceipt {
        request_digest: call.request_digest.clone(),
        result_digest: sha256_hex(&serde_json::to_vec(&payload).unwrap()),
        tenant: call.tenant.clone(),
        initiator: call.initiator.clone(),
        agent: call.agent.clone(),
        task_id: call.task_id.clone(),
        operation: call.operation.clone(),
        resource: call.resource.clone(),
        purpose: call.purpose.clone(),
        role: call.role.clone(),
        entitlements: call.entitlements.clone(),
        classification,
        filtered,
        expires_at: expiry,
        signature: None,
    };
    r.signature = Some(sig(&r, seed, "source"));
    payload["_meta"] = json!({RECEIPT_META: r});
    json!({"jsonrpc":"2.0","id":"synthetic-rpc","result":payload})
}

#[test]
fn adversarial_matrix_2000_cases() {
    let families = [
        "assignment-valid",
        "assignment-tenant",
        "assignment-task",
        "assignment-agent",
        "assignment-epoch",
        "assignment-expired",
        "assignment-forged",
        "assignment-contract-bytes",
        "mcp-read-valid",
        "mcp-cross-resource",
        "mcp-user-commit",
        "mcp-unsigned",
        "mcp-role",
        "mcp-unknown-field",
        "mcp-receipt-valid",
        "mcp-receipt-tamper",
        "mcp-receipt-wrong-key",
        "mcp-receipt-unfiltered",
        "mcp-receipt-after-grant-expiry",
        "mcp-receipt-beyond-grant",
    ];
    let mut counts = BTreeMap::new();
    let mut results = Vec::new();
    let started = std::time::Instant::now();
    for n in 1..=100 {
        let (c, b, i) = fixture(n);
        let keyset = [key(42, "dispatch")];
        let resource = format!("customer:{}:{n}", c.tenant);
        let (contract, d) = mcp_fixture(&c, &resource);
        let keys = [key(43, "contract")];
        let grants = [key(44, "delegation")];
        let sources = [key(45, "source")];
        let ctx = context(&c, &keys, &grants);
        let call = mcp::authorize(
            &contract,
            &d,
            &ctx,
            "bank.record.read",
            &resource,
            json!({"resource":resource}),
        )
        .unwrap();
        let response = authorize_source(&c, &contract, &d, &call, n).unwrap();
        for (idx, family) in families.iter().enumerate() {
            let expected = matches!(idx, 0 | 8 | 14);
            let ok = match idx {
                0..=7 => {
                    let mut g = grant(&c, &format!("case-{n}-{idx}"));
                    let mut bytes = b.clone();
                    match idx {
                        1 => g.context.tenant = "bank-foreign".into(),
                        2 => g.context.task_id = "other".into(),
                        3 => g.context.agent = "other-worker".into(),
                        4 => g.context.epoch = 2,
                        5 => g.expires_at = NOW,
                        7 => bytes.push(b' '),
                        _ => {}
                    }
                    let mut signed = assignment(g);
                    if idx == 6 {
                        signed.signature = "00".repeat(64);
                    }
                    assignment::verify(&signed, &c, &bytes, &i, &keyset, NOW).is_ok()
                }
                8..=13 => {
                    let mut grant = d.clone();
                    let mut op = "bank.record.read";
                    let mut res = resource.as_str();
                    let mut args = json!({"resource":resource});
                    match idx {
                        9 => {
                            res = "customer:bank-foreign:9";
                            args = json!({"resource":res});
                        }
                        10 => op = "bank.record.commit",
                        11 => grant.signature = None,
                        12 => {
                            grant.roles = vec!["auditor".into()];
                            resign(&mut grant);
                        }
                        13 => args["override"] = json!(true),
                        _ => {}
                    }
                    mcp::authorize(&contract, &grant, &ctx, op, res, args).is_ok()
                }
                14..=19 => {
                    let payload = json!({"resultType":"complete","isError":false,"structuredContent":{"syntheticId":n}});
                    let mut resp = response.clone();
                    let mut now = NOW;
                    match idx {
                        15 => resp["result"]["structuredContent"]["syntheticId"] = json!(99999),
                        16 => {
                            resp = receipt(
                                &call,
                                payload,
                                Classification::Internal,
                                true,
                                NOW + 30,
                                46,
                            )
                        }
                        17 => {
                            resp = receipt(
                                &call,
                                payload,
                                Classification::Internal,
                                false,
                                NOW + 30,
                                45,
                            )
                        }
                        18 => {
                            now = NOW + 60;
                            resp = receipt(
                                &call,
                                payload,
                                Classification::Internal,
                                true,
                                NOW + 90,
                                45,
                            );
                        }
                        19 => {
                            resp = receipt(
                                &call,
                                payload,
                                Classification::Internal,
                                true,
                                NOW + 61,
                                45,
                            )
                        }
                        _ => {}
                    }
                    mcp::verify_response(&contract, &d, &call, &resp, &sources, now).is_ok()
                }
                _ => unreachable!(),
            };
            if ok == expected {
                *counts.entry(*family).or_insert(0) += 1;
            }
            results.push(json!({"id":format!("S004-{n:03}-{idx:02}"),"family":family,"expectedAllow":expected,"actualAllow":ok,"passed":ok==expected}));
        }
    }
    let passed = results.iter().filter(|v| v["passed"] == true).count();
    let report = json!({"suite":"parameterized-native-adversarial-matrix","cases":results.len(),"passed":passed,
        "failed":results.len()-passed,"families":counts,"seconds":started.elapsed().as_secs_f64(),"results":results});
    if let Ok(path) = std::env::var("IRONCLAD_MATRIX_REPORT") {
        fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    println!(
        "IRONCLAD_MATRIX {}",
        json!({"cases":results.len(),"passed":passed,"failed":results.len()-passed,"families":counts})
    );
    assert_eq!(passed, 2000);
}
#[test]
fn simulated_source_filters_rows_and_only_prepares_remediation() {
    let (c, _, _) = fixture(17);
    let resource = "customer:bank-2:17";
    let (contract, grant) = mcp_fixture(&c, resource);
    let keys = [key(43, "contract")];
    let grants = [key(44, "delegation")];
    let sources = [key(45, "source")];
    let ctx = context(&c, &keys, &grants);
    let call = mcp::authorize(
        &contract,
        &grant,
        &ctx,
        "bank.record.remediation",
        resource,
        json!({"resource":resource}),
    )
    .unwrap();
    let response = authorize_source(&c, &contract, &grant, &call, 17).unwrap();
    let verified =
        mcp::verify_response(&contract, &grant, &call, &response, &sources, NOW).unwrap();
    assert_eq!(verified["result"]["structuredContent"]["committed"], false);
    assert!(authorize_source(&c, &contract, &grant, &call, 18).is_err());
    assert!(mcp::authorize(
        &contract,
        &grant,
        &ctx,
        "bank.record.commit",
        resource,
        json!({"resource":resource})
    )
    .is_err());
}

#[test]
fn admission_expires_at_signed_grant_and_old_epoch_cannot_commit() {
    let t = Temp::new();
    let path = t.0.join("dispatch.db");
    let (c, b, i) = fixture(21);
    let mut s = DispatchStore::open(&path).unwrap();
    provision(&mut s, &c.tenant);
    s.propose("manager", &offer(&c), NOW).unwrap();
    s.revoke(&c.tenant, &c.task_id, 1, "supervisor", NOW)
        .unwrap();
    let mut next = c.clone();
    next.epoch = 2;
    let mut o = offer(&next);
    o.expires = NOW + 120;
    s.propose("manager", &o, NOW).unwrap();
    s.approve(&next.tenant, &next.task_id, 2, "supervisor", NOW)
        .unwrap();
    let old = assignment(grant(&c, "stale"));
    assert!(admit(&mut s, &next, &b, &i, &old, NOW).is_err());
    let mut g = grant(&next, "current");
    g.expires_at = NOW + 5;
    admit(&mut s, &next, &b, &i, &assignment(g), NOW).unwrap();
    assert_eq!(
        s.audit(&next.tenant, &next.task_id, "auditor")
            .unwrap()
            .0
            .offer
            .expires,
        NOW + 5
    );
    assert!(s
        .submit(
            &next.tenant,
            &next.task_id,
            1,
            "operator",
            &"c".repeat(64),
            NOW
        )
        .is_err());
    assert!(s
        .submit(
            &next.tenant,
            &next.task_id,
            2,
            "operator",
            &"c".repeat(64),
            NOW + 5
        )
        .is_err());
}
#[test]
fn concurrent_valid_admission_commits_once() {
    use std::sync::{Arc, Barrier};
    let t = Temp::new();
    let path = t.0.join("dispatch.db");
    let (c, b, i) = fixture(25);
    let mut s = DispatchStore::open(&path).unwrap();
    provision(&mut s, &c.tenant);
    s.propose("manager", &offer(&c), NOW).unwrap();
    s.approve(&c.tenant, &c.task_id, 1, "supervisor", NOW)
        .unwrap();
    drop(s);
    let g = assignment(grant(&c, "race"));
    let gate = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let (path, c, b, i, g, gate) = (
                path.clone(),
                c.clone(),
                b.clone(),
                i.clone(),
                g.clone(),
                gate.clone(),
            );
            std::thread::spawn(move || {
                let mut s = DispatchStore::open(&path).unwrap();
                gate.wait();
                admit(&mut s, &c, &b, &i, &g, NOW).is_ok()
            })
        })
        .collect();
    let count = handles
        .into_iter()
        .map(|h| usize::from(h.join().unwrap()))
        .sum::<usize>();
    assert_eq!(count, 1);
    let s = DispatchStore::open(&path).unwrap();
    let (l, n) = s.audit(&c.tenant, &c.task_id, "auditor").unwrap();
    assert_eq!(l.state, "running");
    assert_eq!(n, 3);
}
#[test]
fn response_cannot_rebind_to_changed_call_or_grant() {
    let (c, _, _) = fixture(31);
    let resource = "customer:bank-1:31";
    let (contract, d) = mcp_fixture(&c, resource);
    let keys = [key(43, "contract")];
    let grants = [key(44, "delegation")];
    let sources = [key(45, "source")];
    let call = mcp::authorize(
        &contract,
        &d,
        &context(&c, &keys, &grants),
        "bank.record.read",
        resource,
        json!({"resource":resource}),
    )
    .unwrap();
    let response = authorize_source(&c, &contract, &d, &call, 31).unwrap();
    assert!(mcp::verify_response(&contract, &d, &call, &response, &sources, NOW).is_ok());
    let mut altered = call.clone();
    altered.arguments = json!({"resource":"customer:bank-foreign:31"});
    assert!(mcp::verify_response(&contract, &d, &altered, &response, &sources, NOW).is_err());
    let mut d2 = d.clone();
    d2.expires_at += 1;
    resign(&mut d2);
    assert!(mcp::verify_response(&contract, &d2, &call, &response, &sources, NOW).is_err());
}

mod support;

#[test]
fn real_jani_cli_discloses_only_verified_synthetic_banking_response() {
    let t = Temp::new();
    let (c, _, _) = fixture(37);
    let now = jani_core::types::now_unix();
    let resource = "customer:bank-2:37";
    let (contract, mut d) = mcp_fixture(&c, resource);
    d.issued_at = now - 1;
    d.expires_at = now + 120;
    resign(&mut d);
    let keys = [key(43, "contract")];
    let grants = [key(44, "delegation")];
    let mut ctx = context(&c, &keys, &grants);
    ctx.now = now;
    let args = json!({"resource":resource});
    let call = mcp::authorize(
        &contract,
        &d,
        &ctx,
        "bank.record.read",
        resource,
        args.clone(),
    )
    .unwrap();
    let response = authorize_source_at(&c, &contract, &d, &call, 37, now).unwrap();
    let mut i = IdentityAssertion {
        api_version: "jani.dev/v1alpha1".into(),
        kind: "IdentityAssertion".into(),
        principal: c.principal.clone(),
        tenant: c.tenant.clone(),
        subject_type: "human".into(),
        issuer: "fixture-idp".into(),
        issued_at: now - 1,
        expires_at: now + 240,
        nonce: "cli-fixture".into(),
        signature: None,
    };
    let s = sig(&i, 41, "identity");
    i.signature = Some(IdentitySignature {
        key_id: s.key_id,
        value: s.value,
    });
    for (name, value) in [
        ("identity.json", serde_json::to_value(&i).unwrap()),
        ("contract.json", serde_json::to_value(&contract).unwrap()),
        ("grant.json", serde_json::to_value(&d).unwrap()),
        ("args.json", args),
        ("response.json", response.clone()),
    ] {
        fs::write(t.0.join(name), serde_json::to_vec(&value).unwrap()).unwrap();
    }
    // The default local policy refused REVIEW. Do not loosen it: provide
    // an explicitly signed, tenant-specific synthetic enterprise policy.
    let mut policy: jani_core::policy::PolicyBundle = serde_json::from_value(json!({
        "apiVersion":"jani.dev/v1alpha1","kind":"OrganizationPolicy",
        "metadata":{"organization":"synthetic-bank","revision":"s004", "issuer":"fixture-policy",
          "tenant":c.tenant,"issuedAt":now-1,"expiresAt":now+240},
        "spec":{"rules":[{"action":"mcp.bank.record.read","resource":format!("mcp:synthetic-bank:{resource}")}],
          "data":{"allowedPurposes":["REVIEW"]}}
    })).unwrap();
    let s = sig(&policy, 47, "policy");
    policy.signature = Some(jani_core::policy::PolicySignature {
        key_id: s.key_id,
        value: s.value,
    });
    fs::write(
        t.0.join("policy.json"),
        serde_json::to_vec(&policy).unwrap(),
    )
    .unwrap();
    let run = |op: &str| {
        let mut command = support::jani();
        command
            .current_dir(&t.0)
            .args([
                "mcp",
                "validate-response",
                op,
                resource,
                "--tenant",
                &c.tenant,
                "--principal",
                &c.principal,
                "--task",
                &c.task_id,
                "--purpose",
                &c.purpose,
                "--mcp-agent",
                &c.agent,
                "--identity",
                "identity.json",
                "--mode",
                "enterprise",
                "--policy",
                "policy.json",
                "--policy-key",
                &key(47, "policy"),
                "--policy-key",
                &key(41, "identity"),
                "--mcp-contract",
                "contract.json",
                "--mcp-contract-key",
                &key(43, "contract"),
                "--mcp-delegation",
                "grant.json",
                "--mcp-delegation-key",
                &key(44, "delegation"),
                "--mcp-arguments",
                "args.json",
                "--mcp-response",
                "response.json",
                "--mcp-receipt-key",
                &key(45, "source"),
            ])
            .env_remove("JANI_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("JANI_POLICY")
            .env_remove("JANI_MODE")
            .output()
            .unwrap()
    };
    let output = run("bank.record.read");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let v: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(v["receiptVerified"], true);
    assert_eq!(v["result"]["structuredContent"]["syntheticId"], 37);
    let mut bad = response;
    bad["result"]["structuredContent"]["syntheticId"] = json!(38);
    fs::write(t.0.join("response.json"), serde_json::to_vec(&bad).unwrap()).unwrap();
    let rejected = run("bank.record.read");
    assert!(!rejected.status.success());
    assert!(!String::from_utf8_lossy(&rejected.stdout).contains("structuredContent"));
    assert!(!run("bank.record.commit").status.success());
}

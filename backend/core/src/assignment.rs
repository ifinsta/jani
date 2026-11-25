//! Detached assignment authorization for the future Hivewire dispatch boundary.
//! Verification alone never launches work, grants capabilities, or consumes a lease.
use crate::identity::VerifiedIdentity;
use crate::types::{sha256_hex, valid_id, WorkContract};
use serde::{Deserialize, Serialize};

const DOMAIN: &[u8] = b"jani.assignment-authorization/v1\0";
const MAX_TTL: u64 = 900;
const MAX_CONTRACT_BYTES: usize = 65536;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssignmentContext {
    pub tenant: String,
    pub principal: String,
    pub task_id: String,
    pub agent: String,
    pub workspace: String,
    pub purpose: String,
    pub epoch: u64,
    pub base_commit: String,
    pub policy_sha256: String,
    pub contract_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssignmentGrant {
    pub context: AssignmentContext,
    pub issued_at: u64,
    pub expires_at: u64,
    pub nonce: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedAssignment {
    pub grant: AssignmentGrant,
    pub key_id: String,
    pub signature: String,
}

fn digest(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl AssignmentGrant {
    pub fn signing_bytes(&self) -> Result<Vec<u8>, String> {
        let mut bytes = DOMAIN.to_vec();
        bytes.extend(serde_json::to_vec(self).map_err(|e| e.to_string())?);
        Ok(bytes)
    }
}

/// The caller must independently obtain expected context and trusted issuer keys,
/// and separately enforce live lease ownership, revocation, nonce consumption,
/// workspace identity and the runtime's existing policy/contract intersection.
/// Never construct expected context by echoing the untrusted signed envelope.
pub fn verify(
    signed: &SignedAssignment,
    expected: &AssignmentContext,
    contract_bytes: &[u8],
    initiator: &VerifiedIdentity,
    trusted_issuers: &[String],
    now: u64,
) -> Result<(), String> {
    let g = &signed.grant;
    let c = &g.context;
    if c != expected {
        return Err("assignment context mismatch".into());
    }
    for id in [
        &c.tenant,
        &c.principal,
        &c.task_id,
        &c.agent,
        &c.workspace,
        &c.purpose,
        &g.nonce,
        &signed.key_id,
    ] {
        if !valid_id(id, 128) {
            return Err("invalid assignment identifier".into());
        }
    }
    if c.epoch == 0
        || !digest(&c.base_commit, 40)
        || !digest(&c.policy_sha256, 64)
        || !digest(&c.contract_sha256, 64)
    {
        return Err("invalid assignment epoch or digest".into());
    }
    if g.issued_at > now
        || g.expires_at <= now
        || g.expires_at <= g.issued_at
        || g.expires_at - g.issued_at > MAX_TTL
    {
        return Err("assignment lifetime invalid".into());
    }
    if !initiator.verified
        || initiator.subject_type != "human"
        || initiator.tenant != c.tenant
        || initiator.principal != c.principal
        || initiator
            .expires_at
            .is_none_or(|expiry| expiry < g.expires_at)
    {
        return Err("verified initiating human identity required".into());
    }
    if contract_bytes.len() > MAX_CONTRACT_BYTES || sha256_hex(contract_bytes) != c.contract_sha256
    {
        return Err("assignment work contract digest mismatch or oversized contract".into());
    }
    let contract: WorkContract =
        serde_json::from_slice(contract_bytes).map_err(|_| "invalid work contract JSON")?;
    if contract.api_version != "jani.dev/v1alpha1"
        || contract.kind != "WorkContract"
        || contract.metadata.tenant != c.tenant
        || contract.metadata.principal != c.principal
        || contract.metadata.task_id != c.task_id
        || contract.metadata.workspace != c.workspace
        || contract.spec.purpose != c.purpose
    {
        return Err("assignment work contract context mismatch".into());
    }
    crate::crypto::verify_signature(
        trusted_issuers,
        &signed.key_id,
        &g.signing_bytes()?,
        &signed.signature,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: u64 = 1000;
    const CONTRACT: &[u8] = br#"{"apiVersion":"jani.dev/v1alpha1","kind":"WorkContract","metadata":{"tenant":"bank-a","principal":"operator-1","taskId":"T1","workspace":"repo-a"},"spec":{"purpose":"REVIEW","capabilities":["repo.read"]}}"#;

    fn fixture() -> (AssignmentGrant, VerifiedIdentity, Vec<String>) {
        let context = AssignmentContext {
            tenant: "bank-a".into(),
            principal: "operator-1".into(),
            task_id: "T1".into(),
            agent: "jani-1".into(),
            workspace: "repo-a".into(),
            purpose: "REVIEW".into(),
            epoch: 1,
            base_commit: "a".repeat(40),
            policy_sha256: "b".repeat(64),
            contract_sha256: sha256_hex(CONTRACT),
        };
        let grant = AssignmentGrant {
            context,
            issued_at: 999,
            expires_at: 1100,
            nonce: "unique-1".into(),
        };
        let identity = VerifiedIdentity {
            principal: "operator-1".into(),
            tenant: "bank-a".into(),
            subject_type: "human".into(),
            issuer: "test-idp".into(),
            roles: Vec::new(),
            groups: Vec::new(),
            approval_scopes: Vec::new(),
            verified: true,
            expires_at: Some(1200),
        };
        (
            grant,
            identity,
            vec![format!(
                "issuer:{}",
                crate::crypto::public_key_hex(&"11".repeat(32)).unwrap()
            )],
        )
    }

    fn sign(grant: AssignmentGrant) -> SignedAssignment {
        let signature =
            crate::crypto::sign_hex(&"11".repeat(32), &grant.signing_bytes().unwrap()).unwrap();
        SignedAssignment {
            grant,
            signature,
            key_id: "issuer".into(),
        }
    }

    #[test]
    fn valid_assignment_verifies() {
        let (g, i, k) = fixture();
        assert!(verify(&sign(g.clone()), &g.context, CONTRACT, &i, &k, NOW).is_ok());
    }

    #[test]
    fn context_fields_are_bound() {
        let (g, i, k) = fixture();
        let signed = sign(g.clone());
        let mut variants: Vec<AssignmentContext> = Vec::new();
        macro_rules! change {
            ($field:ident, $value:expr) => {{
                let mut c = g.context.clone();
                c.$field = $value;
                variants.push(c);
            }};
        }
        change!(tenant, "bank-b".into());
        change!(principal, "other".into());
        change!(task_id, "T2".into());
        change!(agent, "jani-2".into());
        change!(workspace, "repo-b".into());
        change!(purpose, "UPDATE".into());
        change!(epoch, 2);
        change!(base_commit, "c".repeat(40));
        change!(policy_sha256, "d".repeat(64));
        change!(contract_sha256, "e".repeat(64));
        for c in variants {
            assert!(verify(&signed, &c, CONTRACT, &i, &k, NOW).is_err());
        }
    }

    #[test]
    fn invalid_lifetimes_and_zero_epoch_denied() {
        let (g, i, k) = fixture();
        for (issued, expires) in [
            (1001, 1100),
            (999, 1000),
            (0, 1100),
            (999, 999),
            (1000, 999),
        ] {
            let mut v = g.clone();
            v.issued_at = issued;
            v.expires_at = expires;
            assert!(verify(&sign(v.clone()), &v.context, CONTRACT, &i, &k, NOW).is_err());
        }
        let mut v = g.clone();
        v.context.epoch = 0;
        assert!(verify(&sign(v.clone()), &v.context, CONTRACT, &i, &k, NOW).is_err());
    }

    #[test]
    fn initiator_must_be_verified_human_with_matching_unexpired_authority() {
        let (g, i, k) = fixture();
        for v in (0..6).map(|n| {
            let mut x = i.clone();
            match n {
                0 => x.verified = false,
                1 => x.subject_type = "worker".into(),
                2 => x.tenant = "bank-b".into(),
                3 => x.principal = "other".into(),
                4 => x.expires_at = Some(1050),
                _ => x.expires_at = None,
            };
            x
        }) {
            assert!(verify(&sign(g.clone()), &g.context, CONTRACT, &v, &k, NOW).is_err());
        }
    }

    #[test]
    fn untrusted_keys_and_tampered_signatures_denied() {
        let (g, i, k) = fixture();
        let mut signed = sign(g.clone());
        assert!(verify(&signed, &g.context, CONTRACT, &i, &[], NOW).is_err());
        let wrong = vec![format!(
            "issuer:{}",
            crate::crypto::public_key_hex(&"22".repeat(32)).unwrap()
        )];
        assert!(verify(&signed, &g.context, CONTRACT, &i, &wrong, NOW).is_err());
        signed.grant.nonce = "changed".into();
        assert!(verify(&signed, &g.context, CONTRACT, &i, &k, NOW).is_err());
        signed = sign(g.clone());
        signed.key_id = "other".into();
        assert!(verify(&signed, &g.context, CONTRACT, &i, &k, NOW).is_err());
    }

    #[test]
    fn contract_must_match_exact_bytes_and_context() {
        let (g, i, k) = fixture();
        let mut changed = CONTRACT.to_vec();
        changed.push(b' ');
        assert!(verify(&sign(g.clone()), &g.context, &changed, &i, &k, NOW).is_err());
        let mut value: serde_json::Value = serde_json::from_slice(CONTRACT).unwrap();
        value["metadata"]["tenant"] = "bank-b".into();
        let unrelated = serde_json::to_vec(&value).unwrap();
        let mut v = g.clone();
        v.context.contract_sha256 = sha256_hex(&unrelated);
        assert!(verify(&sign(v.clone()), &v.context, &unrelated, &i, &k, NOW).is_err());
    }

    #[test]
    fn malformed_identifiers_and_digests_denied() {
        let (g, i, k) = fixture();
        let mut v = g.clone();
        v.nonce = "../escape".into();
        assert!(verify(&sign(v.clone()), &v.context, CONTRACT, &i, &k, NOW).is_err());
        let mut v = g.clone();
        v.context.base_commit = "A".repeat(40);
        assert!(verify(&sign(v.clone()), &v.context, CONTRACT, &i, &k, NOW).is_err());
        let mut v = g.clone();
        v.context.policy_sha256 = "b".repeat(63);
        assert!(verify(&sign(v.clone()), &v.context, CONTRACT, &i, &k, NOW).is_err());
    }
}

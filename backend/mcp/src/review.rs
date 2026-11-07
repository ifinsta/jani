//! Independent Ed25519 review-clearance verification for the Jani/Hivewire bridge.
//! Signing keys belong to an external human/KMS. Jani ONLY verifies public keys.
//! A validated signature does not itself run tests or approve a user-only action.
use crate::Signature;
use jani_core::{crypto, types::sha256_hex};
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "jani.review-clearance/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Clearance {
    pub version: String,
    pub tenant: String,
    pub task_id: String,
    pub author: String,
    pub reviewer: String,
    pub base_commit: String,
    pub head_commit: String,
    pub policy_sha256: String,
    pub catalog_sha256: String,
    pub review_sha256: String,
    pub runtime_sha256: String,
    pub regression_sha256: String,
    pub nonce: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub review_accepted: bool,
    pub runtime_passed: bool,
    pub regression_passed: bool,
    pub signature: Option<Signature>,
}

pub struct Expected<'a> {
    pub tenant: &'a str,
    pub task_id: &'a str,
    pub author: &'a str,
    pub base_commit: &'a str,
    pub head_commit: &'a str,
    pub policy_sha256: &'a str,
    pub catalog_sha256: &'a str,
    pub review_sha256: &'a str,
    pub runtime_sha256: &'a str,
    pub regression_sha256: &'a str,
    pub now: u64,
    /// ONLY key ids admitted by an administrator, not by the change author.
    pub trusted_keys: &'a [String],
}

fn hex_digest(s: &str, bytes: usize) -> bool {
    s.len() == bytes * 2 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

/// Domain-separate signed bytes from all other Jani MCP signatures.
pub fn signing_bytes(receipt: &Clearance) -> Result<Vec<u8>, String> {
    let mut unsigned = receipt.clone();
    unsigned.signature = None;
    let mut result = b"jani.review-clearance/v1\0".to_vec();
    result.extend(serde_json::to_vec(&unsigned).map_err(|e| e.to_string())?);
    Ok(result)
}

/// Verify without disclosing either source bodies or private signing keys.
/// Replay prevention belongs to the caller's durable nonce/receipt store.
pub fn verify(receipt: &Clearance, expected: &Expected<'_>) -> Result<String, String> {
    if receipt.version != VERSION {
        return Err("REVIEW_VERSION_UNSUPPORTED".into());
    }
    if !identifier(&receipt.tenant)
        || !identifier(&receipt.task_id)
        || !identifier(&receipt.author)
        || !identifier(&receipt.reviewer)
        || receipt.author == receipt.reviewer
        || receipt.tenant != expected.tenant
        || receipt.task_id != expected.task_id
        || receipt.author != expected.author
    {
        return Err("REVIEW_IDENTITY_OR_CONTEXT_MISMATCH".into());
    }
    if !hex_digest(&receipt.base_commit, 20)
        || !hex_digest(&receipt.head_commit, 20)
        || receipt.base_commit == receipt.head_commit
        || receipt.base_commit != expected.base_commit
        || receipt.head_commit != expected.head_commit
    {
        return Err("REVIEW_SOURCE_REVISION_MISMATCH".into());
    }
    let pairs = [
        (&receipt.policy_sha256, expected.policy_sha256),
        (&receipt.catalog_sha256, expected.catalog_sha256),
        (&receipt.review_sha256, expected.review_sha256),
        (&receipt.runtime_sha256, expected.runtime_sha256),
        (&receipt.regression_sha256, expected.regression_sha256),
    ];
    if pairs
        .iter()
        .any(|(actual, expected)| !hex_digest(actual, 32) || actual != expected)
    {
        return Err("REVIEW_EVIDENCE_OR_POLICY_MISMATCH".into());
    }
    if receipt.nonce.len() < 24
        || receipt.nonce.len() > 128
        || !receipt
            .nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err("REVIEW_NONCE_INVALID".into());
    }
    if receipt.issued_at > expected.now.saturating_add(60)
        || receipt.expires_at <= expected.now
        || receipt.expires_at <= receipt.issued_at
        || receipt.expires_at - receipt.issued_at > 86_400
    {
        return Err("REVIEW_RECEIPT_EXPIRED_OR_CLOCK_INVALID".into());
    }
    if !receipt.review_accepted || !receipt.runtime_passed || !receipt.regression_passed {
        return Err("REVIEW_EVIDENCE_NOT_ACCEPTED".into());
    }
    let sig = receipt
        .signature
        .as_ref()
        .ok_or("REVIEW_SIGNATURE_MISSING")?;
    if sig.key_id != receipt.reviewer {
        return Err("REVIEW_SIGNER_NOT_INDEPENDENT_REVIEWER".into());
    }
    let bytes = signing_bytes(receipt)?;
    crypto::verify_signature(expected.trusted_keys, &sig.key_id, &bytes, &sig.value)
        .map_err(|_| "REVIEW_SIGNATURE_UNTRUSTED".to_string())?;
    Ok(sha256_hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    fn fixture() -> (Clearance, Vec<String>) {
        let secret = SigningKey::from_bytes(&[0x39; 32]);
        let pubkey = hex::encode(secret.verifying_key().to_bytes());
        let mut r = Clearance {
            version: VERSION.into(),
            tenant: "tenant-a".into(),
            task_id: "task-a".into(),
            author: "author-a".into(),
            reviewer: "reviewer-b".into(),
            base_commit: "a".repeat(40),
            head_commit: "b".repeat(40),
            policy_sha256: "1".repeat(64),
            catalog_sha256: "2".repeat(64),
            review_sha256: "3".repeat(64),
            runtime_sha256: "4".repeat(64),
            regression_sha256: "5".repeat(64),
            nonce: "unique-nonce-0123456789abcdef".into(),
            issued_at: 100,
            expires_at: 200,
            review_accepted: true,
            runtime_passed: true,
            regression_passed: true,
            signature: None,
        };
        let signature = secret.sign(&signing_bytes(&r).unwrap());
        r.signature = Some(Signature {
            key_id: "reviewer-b".into(),
            value: hex::encode(signature.to_bytes()),
        });
        (r, vec![format!("reviewer-b:{pubkey}")])
    }
    fn check(r: &Clearance, keys: &[String], now: u64) -> Result<String, String> {
        verify(
            r,
            &Expected {
                tenant: "tenant-a",
                task_id: "task-a",
                author: "author-a",
                base_commit: &"a".repeat(40),
                head_commit: &"b".repeat(40),
                policy_sha256: &"1".repeat(64),
                catalog_sha256: &"2".repeat(64),
                review_sha256: &"3".repeat(64),
                runtime_sha256: &"4".repeat(64),
                regression_sha256: &"5".repeat(64),
                now,
                trusted_keys: keys,
            },
        )
    }
    #[test]
    fn verifies_signed_independent_clearance_and_rejects_tampering() {
        let (receipt, keys) = fixture();
        assert!(check(&receipt, &keys, 150).is_ok());
        let mut changed = receipt.clone();
        changed.head_commit = "c".repeat(40);
        assert!(check(&changed, &keys, 150).is_err());
        changed = receipt.clone();
        changed.review_accepted = false;
        assert!(check(&changed, &keys, 150).is_err());
        changed = receipt.clone();
        changed.reviewer = changed.author.clone();
        assert!(check(&changed, &keys, 150).is_err());
        changed = receipt.clone();
        changed.runtime_sha256 = "6".repeat(64);
        assert!(check(&changed, &keys, 150).is_err());
        assert!(check(&receipt, &keys, 201).is_err());
        assert!(check(&receipt, &[], 150).is_err());
        changed = receipt.clone();
        changed.signature.as_mut().unwrap().key_id = "author-a".into();
        assert!(check(&changed, &keys, 150).is_err());
    }
}

use crate::config::Config;
use crate::crypto;
use crate::types::{now_unix, RuntimeMode};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentitySignature {
    #[serde(default)]
    pub key_id: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityAssertion {
    pub api_version: String,
    pub kind: String,
    pub principal: String,
    pub tenant: String,
    pub subject_type: String,
    #[serde(default)]
    pub issuer: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub nonce: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<IdentitySignature>,
}

impl IdentityAssertion {
    pub fn signing_bytes(&self) -> Result<Vec<u8>, String> {
        let mut clone = self.clone();
        clone.signature = None;
        serde_json::to_vec(&clone).map_err(|e| e.to_string())
    }

    #[cfg(test)]
    pub fn sign(&mut self, secret_hex: &str, key_id: &str) -> Result<(), String> {
        let bytes = self.signing_bytes()?;
        self.signature = Some(IdentitySignature {
            key_id: key_id.to_string(),
            value: crypto::sign_hex(secret_hex, &bytes)?,
        });
        Ok(())
    }

    pub fn verify(&self, trusted_keys: &[String]) -> Result<(), String> {
        if self.api_version != "jani.dev/v1alpha1" {
            return Err("unsupported identity apiVersion; expected jani.dev/v1alpha1".into());
        }
        if self.kind != "IdentityAssertion" {
            return Err("identity kind must be IdentityAssertion".into());
        }
        if self.subject_type != "human" && self.subject_type != "worker" {
            return Err("identity subjectType must be human or worker".into());
        }
        if self.principal.trim().is_empty() || self.tenant.trim().is_empty() {
            return Err("identity requires principal and tenant".into());
        }
        let now = now_unix();
        if self.expires_at <= now {
            return Err("identity assertion has expired".into());
        }
        if self.issued_at > now.saturating_add(300) {
            return Err("identity assertion was issued in the future".into());
        }
        if self.nonce.trim().is_empty() {
            return Err("identity assertion requires a nonce".into());
        }
        let signature = self
            .signature
            .as_ref()
            .ok_or("identity assertion is unsigned")?;
        let bytes = self.signing_bytes()?;
        crypto::verify_signature(trusted_keys, &signature.key_id, &bytes, &signature.value)
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedIdentity {
    pub principal: String,
    pub tenant: String,
    pub subject_type: String,
    pub issuer: String,
    pub verified: bool,
    pub expires_at: Option<u64>,
}

impl VerifiedIdentity {
    pub fn local(cfg: &Config) -> Self {
        Self {
            principal: cfg.principal.clone(),
            tenant: cfg.tenant.clone(),
            subject_type: "human".into(),
            issuer: "local-unmanaged".into(),
            verified: false,
            expires_at: None,
        }
    }
}

pub fn load(cfg: &Config) -> Result<VerifiedIdentity, String> {
    let Some(path) = cfg.identity_path.as_ref() else {
        if cfg.mode == RuntimeMode::Enterprise {
            return Err("enterprise mode requires a signed identity assertion".into());
        }
        return Ok(VerifiedIdentity::local(cfg));
    };
    let assertion = load_assertion(path)?;
    assertion.verify(&cfg.policy_keys)?;
    if assertion.tenant != cfg.tenant {
        return Err(format!(
            "identity tenant '{}' does not match requested tenant '{}'",
            assertion.tenant, cfg.tenant
        ));
    }
    if assertion.principal != cfg.principal {
        return Err(format!(
            "identity principal '{}' does not match requested principal '{}'",
            assertion.principal, cfg.principal
        ));
    }
    Ok(VerifiedIdentity {
        principal: assertion.principal,
        tenant: assertion.tenant,
        subject_type: assertion.subject_type,
        issuer: assertion.issuer,
        verified: true,
        expires_at: Some(assertion.expires_at),
    })
}

pub fn load_assertion(path: &Path) -> Result<IdentityAssertion, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read identity assertion {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("invalid identity assertion JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::public_key_hex;

    fn assertion() -> IdentityAssertion {
        IdentityAssertion {
            api_version: "jani.dev/v1alpha1".into(),
            kind: "IdentityAssertion".into(),
            principal: "human-82".into(),
            tenant: "org-example".into(),
            subject_type: "human".into(),
            issuer: "idp-example".into(),
            issued_at: now_unix().saturating_sub(10),
            expires_at: now_unix().saturating_add(600),
            nonce: "nonce-1".into(),
            signature: None,
        }
    }

    #[test]
    fn signed_identity_verifies_and_tampering_fails() {
        let secret = "33".repeat(32);
        let public = public_key_hex(&secret).unwrap();
        let mut a = assertion();
        a.sign(&secret, "default").unwrap();
        assert!(a.verify(&[format!("default:{public}")]).is_ok());

        let mut tampered = a.clone();
        tampered.principal = "human-99".into();
        assert!(tampered.verify(&[format!("default:{public}")]).is_err());
    }

    #[test]
    fn unsupported_identity_api_version_is_rejected() {
        let mut a = assertion();
        a.api_version = "jani.dev/v2".into();
        let err = a.verify(&[]).unwrap_err();
        assert!(err.contains("apiVersion"));
    }

    #[test]
    fn expired_identity_is_rejected() {
        let secret = "44".repeat(32);
        let public = public_key_hex(&secret).unwrap();
        let mut a = assertion();
        a.expires_at = now_unix().saturating_sub(1);
        a.sign(&secret, "default").unwrap();
        assert!(a.verify(&[format!("default:{public}")]).is_err());
    }
}

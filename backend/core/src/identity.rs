use crate::config::Config;
use crate::crypto;
use crate::types::{now_unix, RuntimeMode};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::Path;

const IDENTITY_DOMAIN: &[u8] = b"jani.identity-assertion/v1\0";
const NONCES_FILE: &str = "identity-nonces.json";
const MAX_NONCE_ENTRIES: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentitySignature {
    #[serde(default)]
    pub key_id: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityAssertion {
    pub api_version: String,
    pub kind: String,
    pub principal: String,
    pub tenant: String,
    pub subject_type: String,
    #[serde(default)]
    pub issuer: String,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub approval_scopes: Vec<String>,
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
        let mut bytes = IDENTITY_DOMAIN.to_vec();
        bytes.extend(crypto::canonical_serialize(&clone)?);
        Ok(bytes)
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
        self.verify_with_revoked(trusted_keys, &[])
    }

    pub fn verify_with_revoked(
        &self,
        trusted_keys: &[String],
        revoked_keys: &[String],
    ) -> Result<(), String> {
        if self.api_version != "jani.dev/v1alpha1" {
            return Err("unsupported identity apiVersion; expected jani.dev/v1alpha1".into());
        }
        if self.kind != "IdentityAssertion" {
            return Err("identity kind must be IdentityAssertion".into());
        }
        if !matches!(
            self.subject_type.as_str(),
            "human" | "worker" | "employee_bot"
        ) {
            return Err("identity subjectType must be human, worker, or employee_bot".into());
        }
        if self.principal.trim().is_empty() || self.tenant.trim().is_empty() {
            return Err("identity requires principal and tenant".into());
        }
        let now = now_unix();
        if self.expires_at <= self.issued_at {
            return Err("identity assertion expiry must be after issuance".into());
        }
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
        crypto::verify_signature_with_revoked(
            trusted_keys,
            revoked_keys,
            &signature.key_id,
            &bytes,
            &signature.value,
        )
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedIdentity {
    pub principal: String,
    pub tenant: String,
    pub subject_type: String,
    pub issuer: String,
    pub roles: Vec<String>,
    pub groups: Vec<String>,
    pub approval_scopes: Vec<String>,
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
            roles: Vec::new(),
            groups: Vec::new(),
            approval_scopes: Vec::new(),
            verified: false,
            expires_at: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NonceEntry {
    expires_at: u64,
    #[serde(default)]
    task_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NonceCache {
    #[serde(default)]
    entries: BTreeMap<String, NonceEntry>,
}

fn update_nonces(
    state_dir: &Path,
    mutate: impl FnOnce(&mut NonceCache, u64) -> Result<(), String>,
) -> Result<(), String> {
    fs::create_dir_all(state_dir)
        .map_err(|e| format!("cannot create state directory {}: {e}", state_dir.display()))?;
    let path = state_dir.join(NONCES_FILE);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("lock"))
        .map_err(|e| format!("cannot open identity nonce lock: {e}"))?;
    lock.lock()
        .map_err(|e| format!("cannot lock identity nonce cache: {e}"))?;
    let res = (|| {
        let now = now_unix();
        let mut cache: NonceCache = match fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|e| format!("corrupt identity nonce cache: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => NonceCache::default(),
            Err(e) => return Err(format!("cannot read identity nonce cache: {e}")),
        };
        cache.entries.retain(|_, v| v.expires_at > now);
        if cache.entries.len() >= MAX_NONCE_ENTRIES {
            return Err("identity nonce cache is full".into());
        }
        mutate(&mut cache, now)?;
        let json = serde_json::to_vec(&cache).map_err(|e| e.to_string())?;
        let tmp = path.with_extension(format!("tmp.{}", crate::process::unique_id()));
        fs::write(&tmp, &json).map_err(|e| format!("cannot write nonce cache: {e}"))?;
        if let Err(e) = fs::rename(&tmp, &path) {
            let _ = fs::remove_file(&tmp);
            return Err(format!("cannot persist nonce cache: {e}"));
        }
        Ok(())
    })();
    let _ = lock.unlock();
    res
}

/// Strictly consumes a nonce once; subsequent presentations with the same scope+nonce are denied.
pub fn claim_unique_nonce(
    state_dir: &Path,
    scope: &str,
    nonce: &str,
    expires_at: u64,
) -> Result<(), String> {
    let key = format!("{scope}\0{nonce}");
    update_nonces(state_dir, |cache, now| {
        if expires_at <= now {
            return Err("nonce is already expired".into());
        }
        if cache.entries.contains_key(&key) {
            return Err("identity or grant nonce has already been consumed".into());
        }
        cache.entries.insert(
            key,
            NonceEntry {
                expires_at,
                task_id: None,
            },
        );
        Ok(())
    })
}

/// Records an identity assertion nonce bound to `task_id` (or unique if `task_id` is None).
pub fn consume_assertion_nonce(
    state_dir: &Path,
    assertion: &IdentityAssertion,
    task_id: Option<&str>,
    strict_single_use: bool,
) -> Result<(), String> {
    let key = format!(
        "id:{}\0{}\0{}\0{}",
        assertion.issuer, assertion.tenant, assertion.principal, assertion.nonce
    );
    update_nonces(state_dir, |cache, now| {
        if assertion.expires_at <= now {
            return Err("identity assertion expired before nonce consumption".into());
        }
        if let Some(existing) = cache.entries.get(&key) {
            if strict_single_use || existing.task_id.as_deref() != task_id || task_id.is_none() {
                return Err("identity assertion nonce replay rejected".into());
            }
            return Ok(());
        }
        cache.entries.insert(
            key,
            NonceEntry {
                expires_at: assertion.expires_at,
                task_id: task_id.map(str::to_string),
            },
        );
        Ok(())
    })
}

pub fn load(cfg: &Config) -> Result<VerifiedIdentity, String> {
    load_with_assertion(cfg).map(|(identity, _)| identity)
}

pub fn load_with_assertion(
    cfg: &Config,
) -> Result<(VerifiedIdentity, Option<IdentityAssertion>), String> {
    let Some(path) = cfg.identity_path.as_ref() else {
        if cfg.mode == RuntimeMode::Enterprise {
            return Err("enterprise mode requires a signed identity assertion".into());
        }
        return Ok((VerifiedIdentity::local(cfg), None));
    };
    if cfg.identity_keys.is_empty() {
        return Err(
            "identity verification requires --identity-key (or JANI_IDENTITY_KEYS) trust roots; policy keys are not accepted"
                .into(),
        );
    }
    let assertion = load_assertion(path)?;
    assertion.verify_with_revoked(&cfg.identity_keys, &cfg.revoked_keys)?;
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
    let identity = VerifiedIdentity {
        principal: assertion.principal.clone(),
        tenant: assertion.tenant.clone(),
        subject_type: assertion.subject_type.clone(),
        issuer: assertion.issuer.clone(),
        roles: assertion.roles.clone(),
        groups: assertion.groups.clone(),
        approval_scopes: assertion.approval_scopes.clone(),
        verified: true,
        expires_at: Some(assertion.expires_at),
    };
    Ok((identity, Some(assertion)))
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
            roles: vec!["reviewer".into()],
            groups: vec!["eng".into()],
            approval_scopes: vec!["write-review".into()],
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

        assert!(a
            .verify_with_revoked(&[format!("default:{public}")], &["default".into()])
            .is_err());
    }

    #[test]
    fn identity_keys_are_isolated_from_policy_keys_and_nonces_block_replay() {
        let dir =
            std::env::temp_dir().join(format!("jani-id-nonce-{}", crate::process::unique_id()));
        fs::create_dir_all(&dir).unwrap();
        let secret = "33".repeat(32);
        let public = public_key_hex(&secret).unwrap();
        let mut a = assertion();
        a.sign(&secret, "id-1").unwrap();
        let id_path = dir.join("identity.json");
        fs::write(&id_path, serde_json::to_vec(&a).unwrap()).unwrap();

        let mut cfg = Config::baseline();
        cfg.identity_path = Some(id_path.clone());
        cfg.tenant = a.tenant.clone();
        cfg.principal = a.principal.clone();
        cfg.policy_keys = vec![format!("id-1:{public}")];
        assert!(load(&cfg).is_err());

        cfg.identity_keys = vec![format!("id-1:{public}")];
        let loaded = load(&cfg).unwrap();
        assert_eq!(loaded.roles, vec!["reviewer".to_string()]);

        assert!(claim_unique_nonce(&dir, "test", "nonce-1", now_unix() + 60).is_ok());
        assert!(claim_unique_nonce(&dir, "test", "nonce-1", now_unix() + 60).is_err());
        assert!(consume_assertion_nonce(&dir, &a, Some("task-1"), false).is_ok());
        assert!(consume_assertion_nonce(&dir, &a, Some("task-1"), false).is_ok());
        assert!(consume_assertion_nonce(&dir, &a, Some("task-2"), false).is_err());
        assert!(consume_assertion_nonce(&dir, &a, Some("task-1"), true).is_err());

        let _ = fs::remove_dir_all(dir);
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

//! Offline Ed25519 issuer CLI for Jani policies, identities, plugins, and assignment grants.
use ed25519_dalek::{Signer, SigningKey};
use jani_core::assignment::{AssignmentGrant, SignedAssignment};
use jani_core::identity::{IdentityAssertion, IdentitySignature};
use jani_core::policy::{PolicyBundle, PolicySignature};
use jani_core::types::{PluginIntegrity, PluginManifest};
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

fn usage() -> &'static str {
    "jani-issuer <keygen|sign-identity|sign-policy|sign-plugin|sign-assignment> [OPTIONS]"
}

fn take_arg(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn read_signing_key(path: &Path) -> Result<SigningKey, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("cannot read signing key {}: {e}", path.display()))?;
    let bytes =
        hex::decode(raw.trim()).map_err(|_| "signing key file must contain 64 hex chars")?;
    let array: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "signing key must decode to 32 bytes")?;
    Ok(SigningKey::from_bytes(&array))
}

fn write_secret(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(path)
        .map_err(|e| format!("cannot create secret key {}: {e}", path.display()))?;
    file.write_all(content.as_bytes())
        .map_err(|e| e.to_string())
}

fn generate_key(key_id: &str, out: &Path) -> Result<serde_json::Value, String> {
    if key_id.trim().is_empty() {
        return Err("--key-id cannot be empty".into());
    }
    let mut secret = [0u8; 32];
    getrandom::getrandom(&mut secret).map_err(|e| format!("CSPRNG error: {e}"))?;
    let signing = SigningKey::from_bytes(&secret);
    let public_hex = hex::encode(signing.verifying_key().to_bytes());
    write_secret(out, &format!("{}\n", hex::encode(secret)))?;
    Ok(json!({
        "keyId": key_id,
        "publicKey": format!("{key_id}:{public_hex}"),
        "secretFile": out.display().to_string(),
    }))
}

fn sign_bytes_hex(key: &SigningKey, bytes: &[u8]) -> String {
    hex::encode(key.sign(bytes).to_bytes())
}

fn sign_file(
    sub: &str,
    input: &Path,
    key_path: &Path,
    key_id: &str,
    out: &Path,
) -> Result<serde_json::Value, String> {
    if key_id.trim().is_empty() {
        return Err("--key-id cannot be empty".into());
    }
    let key = read_signing_key(key_path)?;
    let raw = fs::read_to_string(input)
        .map_err(|e| format!("cannot read input {}: {e}", input.display()))?;
    let output_json = match sub {
        "sign-identity" => {
            let mut assertion: IdentityAssertion =
                serde_json::from_str(&raw).map_err(|e| format!("invalid identity JSON: {e}"))?;
            assertion.signature = None;
            let sig = sign_bytes_hex(&key, &assertion.signing_bytes()?);
            assertion.signature = Some(IdentitySignature {
                key_id: key_id.into(),
                value: sig,
            });
            serde_json::to_string_pretty(&assertion).map_err(|e| e.to_string())?
        }
        "sign-policy" => {
            let mut bundle: PolicyBundle =
                serde_json::from_str(&raw).map_err(|e| format!("invalid policy JSON: {e}"))?;
            bundle.signature = None;
            let sig = sign_bytes_hex(&key, &bundle.signing_bytes()?);
            bundle.signature = Some(PolicySignature {
                key_id: key_id.into(),
                value: sig,
            });
            serde_json::to_string_pretty(&bundle).map_err(|e| e.to_string())?
        }
        "sign-plugin" => {
            let mut manifest: PluginManifest =
                serde_json::from_str(&raw).map_err(|e| format!("invalid plugin JSON: {e}"))?;
            manifest.integrity = None;
            let digest = manifest.computed_digest();
            let sig = sign_bytes_hex(&key, digest.as_bytes());
            manifest.integrity = Some(PluginIntegrity {
                digest,
                signature: sig,
                key_id: key_id.into(),
            });
            serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?
        }
        "sign-assignment" => {
            let grant: AssignmentGrant =
                serde_json::from_str(&raw).map_err(|e| format!("invalid assignment JSON: {e}"))?;
            let sig = sign_bytes_hex(&key, &grant.signing_bytes()?);
            let signed = SignedAssignment {
                grant,
                key_id: key_id.into(),
                signature: sig,
            };
            serde_json::to_string_pretty(&signed).map_err(|e| e.to_string())?
        }
        _ => return Err(format!("unknown issuer subcommand: {sub}")),
    };
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let tmp = out.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp, format!("{output_json}\n")).map_err(|e| e.to_string())?;
    fs::rename(&tmp, out).map_err(|e| e.to_string())?;
    Ok(json!({
        "signed": sub,
        "keyId": key_id,
        "output": out.display().to_string(),
        "publicKey": format!("{key_id}:{}", hex::encode(key.verifying_key().to_bytes())),
    }))
}

fn run(args: &[String]) -> Result<serde_json::Value, String> {
    let sub = args.first().map(String::as_str).ok_or_else(usage)?;
    if matches!(sub, "--help" | "-h" | "help") {
        return Ok(json!({"usage": usage()}));
    }
    let (mut input, mut key, mut key_id, mut out) = (None, None, None, None);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--in" => input = Some(PathBuf::from(take_arg(args, &mut i, "--in")?)),
            "--key" => key = Some(PathBuf::from(take_arg(args, &mut i, "--key")?)),
            "--key-id" => key_id = Some(take_arg(args, &mut i, "--key-id")?),
            "--out" => out = Some(PathBuf::from(take_arg(args, &mut i, "--out")?)),
            other => return Err(format!("unknown option: {other}")),
        }
        i += 1;
    }
    if sub == "keygen" {
        return generate_key(
            &key_id.ok_or("--key-id is required")?,
            &out.ok_or("--out is required")?,
        );
    }
    sign_file(
        sub,
        &input.ok_or("--in is required")?,
        &key.ok_or("--key is required")?,
        &key_id.ok_or("--key-id is required")?,
        &out.ok_or("--out is required")?,
    )
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(value) => {
            println!("{}", serde_json::to_string_pretty(&value).unwrap());
        }
        Err(err) => {
            eprintln!("jani-issuer: {err}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jani_core::policy::{PolicyMetadata, PolicySpec};
    use jani_core::types::{now_unix, PluginMetadata, PluginRuntime, PluginSpec};

    #[test]
    fn keygen_and_sign_round_trip_with_core_verifiers() {
        let dir = std::env::temp_dir().join(format!("jani-issuer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join("id.key");
        let generated = generate_key("corp-1", &key_path).unwrap();
        let pub_spec = generated["publicKey"].as_str().unwrap().to_string();

        let id_in = dir.join("id-raw.json");
        let id_out = dir.join("id.json");
        let assertion = IdentityAssertion {
            api_version: "jani.dev/v1alpha1".into(),
            kind: "IdentityAssertion".into(),
            principal: "bot-1".into(),
            tenant: "acme".into(),
            subject_type: "employee_bot".into(),
            issuer: "jani-issuer".into(),
            roles: vec!["developer".into()],
            groups: vec!["eng".into()],
            approval_scopes: vec![],
            issued_at: now_unix().saturating_sub(5),
            expires_at: now_unix().saturating_add(600),
            nonce: "n-1".into(),
            signature: None,
        };
        fs::write(&id_in, serde_json::to_vec(&assertion).unwrap()).unwrap();
        sign_file("sign-identity", &id_in, &key_path, "corp-1", &id_out).unwrap();
        let loaded_id = jani_core::identity::load_assertion(&id_out).unwrap();
        assert!(loaded_id.verify(std::slice::from_ref(&pub_spec)).is_ok());

        let pol_in = dir.join("pol-raw.json");
        let pol_out = dir.join("pol.json");
        let policy = PolicyBundle {
            api_version: "jani.dev/v1alpha1".into(),
            kind: "OrganizationPolicy".into(),
            metadata: PolicyMetadata {
                organization: "acme".into(),
                revision: "r1".into(),
                issuer: "sec".into(),
                tenant: Some("acme".into()),
                issued_at: Some(now_unix()),
                expires_at: Some(now_unix() + 600),
            },
            spec: PolicySpec::default(),
            signature: None,
        };
        fs::write(&pol_in, serde_json::to_vec(&policy).unwrap()).unwrap();
        sign_file("sign-policy", &pol_in, &key_path, "corp-1", &pol_out).unwrap();
        assert!(PolicyBundle::load(&pol_out, std::slice::from_ref(&pub_spec)).is_ok());

        let plug_in = dir.join("plug-raw.json");
        let plug_out = dir.join("plug.json");
        let manifest = PluginManifest {
            api_version: "plugins.jani.dev/v1alpha1".into(),
            kind: "Plugin".into(),
            metadata: PluginMetadata {
                id: "com.acme.git".into(),
                name: "Git".into(),
                version: "1.0.0".into(),
                publisher: "acme".into(),
            },
            spec: PluginSpec {
                kind: jani_core::types::PluginKind::Connector,
                runtime: PluginRuntime {
                    runtime_type: "sidecar".into(),
                    artifact: "connector.py".into(),
                    artifact_sha256: None,
                    entrypoint: "serve".into(),
                },
                capabilities: vec![],
                requested_permissions: Default::default(),
            },
            integrity: None,
        };
        fs::write(&plug_in, serde_json::to_vec(&manifest).unwrap()).unwrap();
        sign_file("sign-plugin", &plug_in, &key_path, "corp-1", &plug_out).unwrap();
        let signed_plug = PluginManifest::load(&plug_out).unwrap();
        let int = signed_plug.integrity.as_ref().unwrap();
        assert!(jani_core::crypto::verify_signature(
            &[pub_spec],
            &int.key_id,
            int.digest.as_bytes(),
            &int.signature
        )
        .is_ok());

        let _ = fs::remove_dir_all(&dir);
    }
}

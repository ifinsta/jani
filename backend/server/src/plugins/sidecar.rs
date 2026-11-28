use super::credential_broker::CredentialBroker;
use super::RegisteredPlugin;
use crate::config::Config;
use crate::data::{redact_json, redact_text, Governance};
use crate::sandbox::{SandboxManager, SandboxPlan};
use crate::types::{CapabilityDescriptor, SandboxReceipt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const MAX_SIDECAR_SECS: u64 = 30;
const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;

struct PrivateWorkspace {
    path: PathBuf,
    executable: PathBuf,
}

impl PrivateWorkspace {
    fn new(python: bool) -> Result<Self, String> {
        // Keep the filesystem socket address within the Unix path length limit.
        let path = Path::new("/tmp").join(crate::types::new_id("jani-sidecar"));
        DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|error| format!("cannot create private sidecar directory: {error}"))?;
        let executable = path.join(if python { "sidecar.py" } else { "sidecar" });
        Ok(Self { path, executable })
    }

    fn stage(&self, source: &Path, expected_digest: &str) -> Result<(), String> {
        let input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(source)
            .map_err(|error| format!("cannot open sidecar artifact: {error}"))?;
        let metadata = input.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_ARTIFACT_BYTES {
            return Err("sidecar artifact must be a regular file of at most 64 MiB".into());
        }
        let mut output = OpenOptions::new()
            .mode(0o500)
            .write(true)
            .create_new(true)
            .open(&self.executable)
            .map_err(|error| format!("cannot stage sidecar artifact: {error}"))?;
        let mut digest = Sha256::new();
        let mut source = input.take(MAX_ARTIFACT_BYTES + 1);
        let mut buffer = [0; 32 * 1024];
        loop {
            let count = source
                .read(&mut buffer)
                .map_err(|error| error.to_string())?;
            if count == 0 {
                break;
            }
            let chunk = &buffer[..count];
            digest.update(chunk);
            output.write_all(chunk).map_err(|error| error.to_string())?;
        }
        if source.limit() == 0 || hex::encode(digest.finalize()) != expected_digest {
            return Err("sidecar artifact does not match its signed SHA-256".into());
        }
        Ok(())
    }
}

impl Drop for PrivateWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.executable);
        let _ = fs::remove_dir(&self.path);
    }
}

pub(super) fn invoke(
    cfg: &Config,
    (plugin, capability): (&RegisteredPlugin, &CapabilityDescriptor),
    args: &Value,
    plan: &SandboxPlan,
    governance: &Governance,
    control: crate::process::ProcessGate<'_>,
) -> Result<(Value, SandboxReceipt), String> {
    let cancelled = control.cancelled;
    let source = plugin
        .artifact_path
        .as_deref()
        .ok_or("sidecar artifact is missing")?;
    let runtime = &plugin.manifest.spec.runtime;
    let digest = runtime
        .artifact_sha256
        .as_deref()
        .filter(|digest| super::valid_artifact_digest(digest))
        .ok_or("sidecar requires a signed artifactSha256")?;
    let python = source
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("py"));
    let workspace = PrivateWorkspace::new(python)?;
    workspace.stage(source, digest)?;

    let mut invocation_cfg = cfg.clone();
    invocation_cfg.root.clone_from(&workspace.path);
    invocation_cfg.allow_write = false;
    invocation_cfg.write_paths.clear();
    let program = workspace.executable.to_string_lossy().into_owned();
    let argv = if python {
        vec!["python3".into(), program]
    } else {
        vec![program]
    };
    let (mut cmd, isolation) =
        SandboxManager::new(&invocation_cfg).command_for_sidecar(plan, &argv)?;
    cmd.env_clear();
    let deadline = Instant::now() + Duration::from_secs(MAX_SIDECAR_SECS);
    let permissions = plugin.manifest.spec.requested_permissions.as_ref();
    let broker = permissions
        .filter(|permissions| !permissions.network_services.is_empty())
        .map(|permissions| {
            let secrets = connector_credentials(cfg, &permissions.secrets)?;
            CredentialBroker::start(
                &workspace.path,
                permissions.network_services.clone(),
                secrets,
                governance.clone(),
                capability.effect,
                deadline,
                control.authority,
            )
        })
        .transpose()?;

    let mut request = json!({
        "jsonrpc": "2.0", "id": 1, "method": capability.name,
        "entrypoint": runtime.entrypoint, "pluginId": plugin.manifest.metadata.id, "params": args,
    });
    if let Some(broker) = &broker {
        request["janiBroker"] = broker.context();
    }
    let mut input = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
    input.push(b'\n');

    let outcome = crate::process::execute_with_admission(
        &mut cmd,
        &workspace.path,
        MAX_SIDECAR_SECS,
        isolation,
        &cfg.credential_env_names,
        Some(&input),
        control,
    );
    // Revoke the broker before publishing any output, even when the child failed.
    let mut broker = broker;
    if let Some(broker) = &mut broker {
        broker.stop();
    }
    let output = outcome?;
    if cancelled.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
        return Err("sidecar plugin cancelled by operator".into());
    }
    let plugin_id = &plugin.manifest.metadata.id;
    if output.timed_out {
        return Err(format!(
            "sidecar plugin '{plugin_id}' exceeded its execution limits"
        ));
    }
    if output.code != Some(0) {
        let stderr = broker.as_ref().map_or_else(
            || output.stderr.clone(),
            |broker| broker.scrub_lease(&output.stderr),
        );
        let (clean, _) = redact_text(stderr.trim());
        return Err(format!(
            "sidecar plugin '{plugin_id}' exited with {:?}: {clean}",
            output.code
        ));
    }
    if output.stdout.len() >= crate::process::EXCERPT_LIMIT {
        return Err("sidecar response exceeds the JSON-RPC output limit".into());
    }
    let mut result = parse_result(output.stdout.trim())?;
    if let Some(broker) = &broker {
        broker.scrub_result(&mut result);
    }
    redact_json(&mut result);
    if let Some(schema) = &capability.output_schema {
        crate::schema::validate(&result, schema)
            .map_err(|error| format!("sidecar output failed declared schema: {error}"))?;
    }
    Ok((result, plan.receipt(None)))
}

fn connector_credentials(
    cfg: &Config,
    names: &[String],
) -> Result<BTreeMap<String, String>, String> {
    let mut credentials = BTreeMap::new();
    for name in names {
        if [crate::env::API_KEY, crate::env::OPENAI_API_KEY]
            .into_iter()
            .chain(cfg.credential_env_names.iter().map(String::as_str))
            .any(|reserved| reserved.eq_ignore_ascii_case(name))
        {
            return Err("model credentials cannot be used by the sidecar broker".into());
        }
        let value = std::env::var(name).ok();
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            credentials.insert(name.clone(), value);
        }
    }
    Ok(credentials)
}

fn parse_result(output: &str) -> Result<Value, String> {
    let mut rpc: Value =
        serde_json::from_str(output).map_err(|_| "sidecar plugin returned invalid JSON-RPC")?;
    if rpc["jsonrpc"] != "2.0" || rpc["id"] != 1 {
        return Err("sidecar response does not match the JSON-RPC request".into());
    }
    if rpc.get("error").is_some() {
        return Err("sidecar plugin returned a JSON-RPC error".into());
    }
    rpc.as_object_mut()
        .and_then(|object| object.remove("result"))
        .ok_or_else(|| "sidecar response is missing its result".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_must_match_the_request_and_be_one_message() {
        assert_eq!(
            parse_result(r#"{"id":1,"jsonrpc":"2.0","result":null}"#).unwrap(),
            Value::Null
        );
        for malformed in [
            r#"{"id":2,"jsonrpc":"2.0","result":true}"#,
            r#"{"id":1,"jsonrpc":"2.0","result":true,"error":{}}"#,
            r#"{"id":1,"jsonrpc":"2.0","result":true} {}"#,
        ] {
            assert!(parse_result(malformed).is_err());
        }
    }

    #[test]
    fn staging_copies_the_verified_bytes() {
        let upstream = PrivateWorkspace::new(true).unwrap();
        fs::write(&upstream.executable, b"original").unwrap();
        let child = PrivateWorkspace::new(true).unwrap();
        let digest = crate::types::sha256_hex(b"original");
        child.stage(&upstream.executable, &digest).unwrap();
        fs::write(&upstream.executable, b"different").unwrap();
        assert_eq!(fs::read(&child.executable).unwrap(), b"original");
        let later = PrivateWorkspace::new(true).unwrap();
        assert!(later.stage(&upstream.executable, &digest).is_err());
    }
}

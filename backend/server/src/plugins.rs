use crate::config::{validate_relative, Config};
use crate::crypto;
use crate::data::{redact_json, redact_text, Governance};
use crate::sandbox::{SandboxManager, SandboxPlan};
use crate::types::{
    CapabilityDescriptor, NetworkPolicy, PluginManifest, SandboxProfile, SandboxReceipt,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const MAX_SIDECAR_TIMEOUT_SECS: u64 = 30;
const MAX_PROXY_BODY_BYTES: usize = 2 * 1024 * 1024;

pub struct RegisteredPlugin {
    pub manifest: PluginManifest,
    pub digest: String,
    pub source: PathBuf,
    pub signature_key: String,
    pub artifact_path: Option<PathBuf>,
    pub plugin_dir: Option<PathBuf>,
}

#[derive(Default)]
pub struct PluginRegistry {
    pub plugins: BTreeMap<String, RegisteredPlugin>,
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

impl PluginRegistry {
    pub fn load(cfg: &Config) -> Result<Self, String> {
        let Some(dir) = cfg.plugin_dir.as_ref() else {
            return Ok(Self::default());
        };
        if !dir.is_dir() {
            return Err(format!("plugin directory not found: {}", dir.display()));
        }
        if cfg.plugin_keys.is_empty() {
            return Err("a plugin directory requires --plugin-key trust roots".into());
        }
        let mut entries = fs::read_dir(dir)
            .map_err(|e| format!("cannot read plugin directory: {e}"))?
            .flatten()
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.path());
        if entries.len() > 128 {
            return Err("plugin directory exceeds the 128 manifest limit".into());
        }
        let mut registry = Self::default();
        for entry in entries {
            let path = entry.path();
            let kind = entry
                .file_type()
                .map_err(|e| format!("cannot inspect {}: {e}", path.display()))?;
            if kind.is_symlink() {
                return Err(format!(
                    "symlinked plugin manifests are refused: {}",
                    path.display()
                ));
            }
            if !kind.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let size = entry.metadata().map_err(|e| e.to_string())?.len();
            if size > 512_000 {
                return Err(format!("plugin manifest too large: {}", path.display()));
            }
            let manifest = PluginManifest::load(&path)?;
            if manifest.api_version != "plugins.jani.dev/v1alpha1" {
                return Err(format!(
                    "unsupported plugin apiVersion in {}",
                    path.display()
                ));
            }
            if !valid_identifier(&manifest.metadata.id) {
                return Err(format!(
                    "invalid plugin id '{}' in {}",
                    manifest.metadata.id,
                    path.display()
                ));
            }
            let allowed_runtime = matches!(
                manifest.spec.runtime.runtime_type.as_str(),
                "builtin" | "wasm" | "sidecar"
            );
            if !allowed_runtime {
                return Err(format!(
                    "plugin {} requests unsupported runtime '{}'; native dynamic loading is refused",
                    manifest.metadata.id, manifest.spec.runtime.runtime_type
                ));
            }
            let digest = manifest.computed_digest();
            let integrity = manifest
                .integrity
                .as_ref()
                .ok_or_else(|| format!("plugin {} is not signed", manifest.metadata.id))?;
            if integrity.digest != digest {
                return Err(format!(
                    "plugin {} digest does not match its manifest",
                    manifest.metadata.id
                ));
            }
            let signature_bytes = hex::decode(integrity.signature.trim())
                .map_err(|_| format!("plugin {} signature is not hex", manifest.metadata.id))?;
            if signature_bytes.len() != 64 {
                return Err(format!(
                    "plugin {} signature must be 64 bytes",
                    manifest.metadata.id
                ));
            }
            crypto::verify_signature_with_revoked(
                &cfg.plugin_keys,
                &cfg.revoked_keys,
                &integrity.key_id,
                digest.as_bytes(),
                &integrity.signature,
            )
            .map_err(|e| format!("plugin {} signature rejected: {e}", manifest.metadata.id))?;

            let mut names = std::collections::BTreeSet::new();
            for capability in &manifest.spec.capabilities {
                let namespaced = capability.namespaced(&manifest.metadata.id);
                if !valid_identifier(&capability.name) {
                    return Err(format!(
                        "plugin {} has an invalid capability name",
                        manifest.metadata.id
                    ));
                }
                if !names.insert(namespaced.clone()) {
                    return Err(format!("duplicate capability {namespaced}"));
                }
                if let Some(input) = &capability.input_schema {
                    if !input.is_object() {
                        return Err(format!(
                            "capability {namespaced} has a non-object input schema"
                        ));
                    }
                }
            }
            if registry.plugins.contains_key(&manifest.metadata.id) {
                return Err(format!("duplicate plugin id {}", manifest.metadata.id));
            }
            let (artifact_path, resolved_plugin_dir) =
                if manifest.spec.runtime.runtime_type == "sidecar" {
                    let rel = manifest.spec.runtime.artifact.trim();
                    if rel.is_empty() {
                        return Err(format!(
                            "sidecar plugin {} requires runtime.artifact",
                            manifest.metadata.id
                        ));
                    }
                    validate_relative(Path::new(rel))?;
                    let base = dir
                        .canonicalize()
                        .map_err(|e| format!("cannot resolve plugin_dir {}: {e}", dir.display()))?;
                    let joined = base.join(rel);
                    let meta = fs::symlink_metadata(&joined).map_err(|e| {
                        format!(
                            "sidecar artifact {} missing for {}: {e}",
                            joined.display(),
                            manifest.metadata.id
                        )
                    })?;
                    if meta.file_type().is_symlink() || !meta.is_file() {
                        return Err(format!(
                            "sidecar artifact {} must be a non-symlink regular file",
                            joined.display()
                        ));
                    }
                    let canon = joined
                        .canonicalize()
                        .map_err(|e| format!("cannot resolve sidecar artifact: {e}"))?;
                    if !canon.starts_with(&base) {
                        return Err("sidecar artifact escapes plugin_dir".into());
                    }
                    (Some(canon), Some(base))
                } else {
                    (None, None)
                };
            registry.plugins.insert(
                manifest.metadata.id.clone(),
                RegisteredPlugin {
                    digest,
                    source: path,
                    signature_key: integrity.key_id.clone(),
                    artifact_path,
                    plugin_dir: resolved_plugin_dir,
                    manifest,
                },
            );
        }
        Ok(registry)
    }

    pub fn list(&self) -> Value {
        let plugins = self
            .plugins
            .values()
            .map(|plugin| {
                json!({
                    "id": plugin.manifest.metadata.id,
                    "name": plugin.manifest.metadata.name,
                    "version": plugin.manifest.metadata.version,
                    "publisher": plugin.manifest.metadata.publisher,
                    "kind": plugin.manifest.spec.kind,
                    "runtime": plugin.manifest.spec.runtime.runtime_type,
                    "digest": plugin.digest,
                    "capabilities": plugin.manifest.spec.capabilities.len(),
                    "invocable": plugin.manifest.spec.runtime.runtime_type == "sidecar",
                })
            })
            .collect::<Vec<_>>();
        json!({"plugins": plugins})
    }

    pub fn inspect(&self, id: &str) -> Result<Value, String> {
        let plugin = self
            .plugins
            .get(id)
            .ok_or_else(|| format!("unknown plugin '{id}'"))?;
        Ok(json!({
            "id": plugin.manifest.metadata.id,
            "name": plugin.manifest.metadata.name,
            "version": plugin.manifest.metadata.version,
            "publisher": plugin.manifest.metadata.publisher,
            "kind": plugin.manifest.spec.kind,
            "runtime": plugin.manifest.spec.runtime.runtime_type,
            "digest": plugin.digest,
            "signatureKey": plugin.signature_key,
            "source": plugin.source.display().to_string(),
            "capabilities": plugin.manifest.spec.capabilities,
            "requestedPermissions": plugin.manifest.spec.requested_permissions,
            "invocable": false,
            "invocationNote": "manifest is approved and verified; out-of-process plugin execution is not implemented in this build"
        }))
    }

    pub fn verify(&self, id: Option<&str>) -> Result<Value, String> {
        match id {
            Some(id) => {
                self.inspect(id)?;
                Ok(json!({"ok": true, "verified": [id]}))
            }
            None => Ok(json!({
                "ok": true,
                "verified": self.plugins.keys().collect::<Vec<_>>(),
            })),
        }
    }

    pub fn capability_summary(&self) -> String {
        if self.plugins.is_empty() {
            return "none".into();
        }
        let mut items = Vec::new();
        for plugin in self.plugins.values() {
            for capability in &plugin.manifest.spec.capabilities {
                items.push(capability.namespaced(&plugin.manifest.metadata.id));
            }
        }
        if items.is_empty() {
            "none".into()
        } else {
            items.join(", ")
        }
    }

    pub fn capability(
        &self,
        plugin_id: &str,
        cap_name: &str,
    ) -> Result<(&RegisteredPlugin, &CapabilityDescriptor), String> {
        let plugin = self
            .plugins
            .get(plugin_id)
            .ok_or_else(|| format!("unknown verified plugin '{plugin_id}'"))?;
        let cap = plugin
            .manifest
            .spec
            .capabilities
            .iter()
            .find(|c| c.name == cap_name)
            .ok_or_else(|| {
                format!(
                    "plugin '{}' does not declare capability '{}'",
                    plugin_id, cap_name
                )
            })?;
        Ok((plugin, cap))
    }

    pub fn invoke_sidecar(
        &self,
        cfg: &Config,
        plugin_id: &str,
        cap_name: &str,
        args: &Value,
        governance: &Governance,
        cancelled: Option<&AtomicBool>,
    ) -> Result<(Value, SandboxReceipt), String> {
        self.invoke_sidecar_with_secrets(
            cfg, plugin_id, cap_name, args, governance, cancelled, None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn invoke_sidecar_with_secrets(
        &self,
        cfg: &Config,
        plugin_id: &str,
        cap_name: &str,
        args: &Value,
        governance: &Governance,
        cancelled: Option<&AtomicBool>,
        secret_overrides: Option<&BTreeMap<String, String>>,
    ) -> Result<(Value, SandboxReceipt), String> {
        let (plugin, cap) = self.capability(plugin_id, cap_name)?;
        if plugin.manifest.spec.runtime.runtime_type != "sidecar" {
            return Err(format!(
                "plugin '{}' runtime '{}' cannot be executed directly; only signed 'sidecar' plugins are invocable",
                plugin_id, plugin.manifest.spec.runtime.runtime_type
            ));
        }
        enforce_human_commit_invariant(cap, args)?;
        if let Some(schema) = cap.input_schema.as_ref() {
            crate::schema::validate(args, schema)
                .map_err(|e| format!("plugin argument schema validation failed: {e}"))?;
        }
        let artifact = plugin
            .artifact_path
            .as_ref()
            .ok_or("sidecar plugin has no verified artifact path")?;
        let plugin_dir = plugin
            .plugin_dir
            .as_ref()
            .ok_or("sidecar plugin has no verified plugin directory")?;
        let meta = fs::symlink_metadata(artifact)
            .map_err(|e| format!("cannot stat sidecar artifact {}: {e}", artifact.display()))?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err("sidecar artifact was replaced with a symlink or non-file".into());
        }
        let canon = artifact
            .canonicalize()
            .map_err(|e| format!("cannot resolve sidecar artifact: {e}"))?;
        if !canon.starts_with(plugin_dir) {
            return Err("sidecar artifact escaped plugin_dir".into());
        }

        let perms = plugin.manifest.spec.requested_permissions.as_ref();
        let allowed_hosts = perms
            .map(|p| p.network_services.clone())
            .unwrap_or_default();
        let mut secrets = BTreeMap::new();
        if let Some(perms) = perms {
            for name in &perms.secrets {
                let val = secret_overrides
                    .and_then(|map| map.get(name).cloned())
                    .or_else(|| std::env::var(name).ok().filter(|v| !v.is_empty()));
                if let Some(secret) = val {
                    secrets.insert(name.clone(), secret);
                }
            }
        }

        let broker = if !allowed_hosts.is_empty() {
            Some(CredentialBroker::start(
                allowed_hosts,
                secrets,
                governance.clone(),
            )?)
        } else {
            None
        };

        let argv = sidecar_argv(&canon)?;
        let manager = SandboxManager::new(cfg);
        let profile = if cfg!(windows) {
            SandboxProfile::TrustedDev
        } else if broker.is_some() {
            SandboxProfile::EnterpriseConnected
        } else {
            SandboxProfile::EnterpriseRestricted
        };
        let network = if broker.is_some() {
            NetworkPolicy::Loopback
        } else {
            NetworkPolicy::Deny
        };
        let plan: SandboxPlan = manager.prepare(profile, network)?;
        let extra_ro = [plugin_dir.clone()];
        let (mut cmd, isolation) = manager.command_with_readonly(&plan, &argv, &[], &extra_ro)?;
        cmd.env_clear();
        for key in safe_runtime_env_keys() {
            if let Ok(val) = std::env::var(key) {
                if !cfg.credential_env_names.iter().any(|c| c == key) {
                    cmd.env(key, val);
                }
            }
        }
        cmd.env("JANI_PLUGIN_ID", plugin_id);
        cmd.env(
            "JANI_PLUGIN_ENTRYPOINT",
            &plugin.manifest.spec.runtime.entrypoint,
        );
        if let Some(ref b) = broker {
            cmd.env("JANI_BROKER_PROXY", format!("http://{}", b.addr));
            cmd.env("JANI_BROKER_LEASE", &b.lease);
        }

        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": cap.name,
            "entrypoint": plugin.manifest.spec.runtime.entrypoint,
            "params": args,
        });
        let mut req_bytes = serde_json::to_vec(&req).map_err(|e| e.to_string())?;
        req_bytes.push(b'\n');

        let out_res = crate::process::execute_with_stdin_cancel(
            &mut cmd,
            plugin_dir,
            MAX_SIDECAR_TIMEOUT_SECS,
            isolation,
            &cfg.credential_env_names,
            Some(&req_bytes),
            cancelled,
        );
        if let Some(b) = broker {
            b.stop();
        }
        let out = out_res?;
        if cancelled.is_some_and(|c| c.load(Ordering::SeqCst)) {
            return Err("sidecar plugin cancelled by operator".into());
        }
        if out.timed_out {
            return Err(format!(
                "sidecar plugin '{plugin_id}' timed out after {MAX_SIDECAR_TIMEOUT_SECS}s"
            ));
        }
        if out.code != Some(0) {
            let (scrubbed, _) = redact_text(out.stderr.trim());
            return Err(format!(
                "sidecar plugin '{plugin_id}' exited with {:?}: {scrubbed}",
                out.code
            ));
        }

        let line = out
            .stdout
            .lines()
            .find(|l| !l.trim().is_empty())
            .ok_or("sidecar plugin returned empty JSON-RPC response")?;
        let rpc: Value = serde_json::from_str(line)
            .map_err(|e| format!("sidecar plugin returned invalid JSON-RPC: {e}"))?;
        if rpc.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err("sidecar response missing jsonrpc 2.0".into());
        }
        if let Some(err) = rpc.get("error").filter(|v| !v.is_null()) {
            let mut err_val = err.clone();
            redact_json(&mut err_val);
            return Err(format!("sidecar plugin error: {err_val}"));
        }
        let mut result = rpc
            .get("result")
            .cloned()
            .ok_or("sidecar JSON-RPC response missing 'result'")?;
        redact_json(&mut result);
        if let Some(schema) = cap.output_schema.as_ref() {
            crate::schema::validate(&result, schema)
                .map_err(|e| format!("sidecar output failed declared schema: {e}"))?;
        }
        Ok((result, plan.receipt(None)))
    }
}

pub fn is_commit_or_merge_cap(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("commit")
        || lower.contains("merge")
        || lower.contains("force_push")
        || lower.contains("push")
        || lower.contains("delete")
}

pub fn enforce_human_commit_invariant(
    cap: &CapabilityDescriptor,
    args: &Value,
) -> Result<(), String> {
    if args.get("auto_merge").and_then(Value::as_bool) == Some(true)
        || args.get("mode").and_then(Value::as_str) == Some("auto_merge")
    {
        return Err(
            "USER_COMMIT_REQUIRED: autonomous commit or merge is forbidden; human decision is required"
                .into(),
        );
    }
    if is_commit_or_merge_cap(&cap.name) && cap.approval_class.as_deref() == Some("auto") {
        return Err("USER_COMMIT_REQUIRED: commit/merge capabilities cannot auto-approve".into());
    }
    Ok(())
}

fn safe_runtime_env_keys() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &[
            "PATH",
            "SYSTEMROOT",
            "WINDIR",
            "COMSPEC",
            "PATHEXT",
            "TEMP",
            "TMP",
        ]
    }
    #[cfg(not(windows))]
    {
        &["PATH", "TMPDIR", "LANG", "LC_ALL"]
    }
}

fn sidecar_argv(artifact: &Path) -> Result<Vec<String>, String> {
    let ext = artifact
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let path_str = artifact.to_string_lossy().into_owned();
    if ext == "py" {
        let python = if cfg!(windows) { "python" } else { "python3" };
        Ok(vec![python.into(), path_str])
    } else {
        Ok(vec![path_str])
    }
}

fn random_lease_hex() -> Result<String, String> {
    let mut bytes = [0u8; 24];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("CSPRNG failed: {e}"))?;
    Ok(hex::encode(bytes))
}

struct CredentialBroker {
    addr: SocketAddr,
    lease: String,
    shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl CredentialBroker {
    fn start(
        allowed_hosts: Vec<String>,
        secrets: BTreeMap<String, String>,
        governance: Governance,
    ) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("cannot bind credential broker proxy: {e}"))?;
        let _ = listener.set_nonblocking(true);
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let lease = random_lease_hex()?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop_flag = shutdown.clone();
        let expected_lease = lease.clone();
        let handle = std::thread::spawn(move || {
            let client = reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(15))
                .build()
                .ok();
            while !stop_flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = handle_broker_request(
                            &mut stream,
                            &expected_lease,
                            &allowed_hosts,
                            &secrets,
                            &governance,
                            client.as_ref(),
                        );
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(15));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            addr,
            lease,
            shutdown,
            handle: Some(handle),
        })
    }

    fn stop(mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn constant_eq(a: &str, b: &str) -> bool {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let len = ab.len().max(bb.len());
    let mut diff = ab.len() ^ bb.len();
    for i in 0..len {
        diff |= usize::from(ab.get(i).copied().unwrap_or(0) ^ bb.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

fn target_allowed(url: &str, allowed_hosts: &[String]) -> Result<String, String> {
    if url.chars().any(char::is_control) || url.contains('@') || url.contains('\\') {
        return Err("malformed target URL".into());
    }
    let authority = crate::net::url_authority(url);
    if authority.is_empty() {
        return Err("target URL missing authority".into());
    }
    let host_only = crate::net::authority_host(authority);
    if host_only == "169.254.169.254" || host_only.starts_with("169.254.") {
        return Err("link-local metadata host is refused".into());
    }
    let scheme_ok = url.starts_with("https://")
        || (url.starts_with("http://")
            && crate::net::is_loopback_authority(authority)
            && crate::net::authority_has_port(authority));
    if !scheme_ok {
        return Err("broker target must use https:// or an explicit loopback URL".into());
    }
    let permitted = allowed_hosts.iter().any(|allowed| {
        let allowed_auth = if allowed.contains("://") {
            crate::net::url_authority(allowed)
        } else {
            allowed.as_str()
        };
        allowed_auth.eq_ignore_ascii_case(authority) || allowed_auth.eq_ignore_ascii_case(host_only)
    });
    if !permitted {
        return Err(format!(
            "host '{authority}' is not in requestedPermissions.networkServices"
        ));
    }
    Ok(url.to_string())
}

fn write_http(stream: &mut TcpStream, status: u16, reason: &str, body: &[u8]) {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
}

fn handle_broker_request(
    stream: &mut TcpStream,
    expected_lease: &str,
    allowed_hosts: &[String],
    secrets: &BTreeMap<String, String>,
    governance: &Governance,
    client: Option<&reqwest::blocking::Client>,
) -> Result<(), String> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut req_line = String::new();
    reader.read_line(&mut req_line).map_err(|e| e.to_string())?;
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let _path = parts.next().unwrap_or("");
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    let mut content_len = 0usize;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().to_string();
            if key == "content-length" {
                content_len = val.parse().unwrap_or(0).min(MAX_PROXY_BODY_BYTES);
            }
            headers.insert(key, val);
        }
    }
    let mut body = vec![0u8; content_len];
    if content_len > 0 {
        reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    }

    let lease = headers
        .get("x-jani-lease")
        .map(String::as_str)
        .unwrap_or("");
    if !constant_eq(lease, expected_lease) {
        write_http(
            stream,
            401,
            "Unauthorized",
            br#"{"error":"invalid broker lease"}"#,
        );
        return Ok(());
    }
    let target = headers
        .get("x-jani-target-url")
        .map(String::as_str)
        .unwrap_or("");
    let target_url = match target_allowed(target, allowed_hosts) {
        Ok(u) => u,
        Err(msg) => {
            let err = json!({"error": msg}).to_string();
            write_http(stream, 403, "Forbidden", err.as_bytes());
            return Ok(());
        }
    };
    let Some(client) = client else {
        write_http(
            stream,
            500,
            "Internal Server Error",
            br#"{"error":"http client unavailable"}"#,
        );
        return Ok(());
    };
    let req_method =
        reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::POST);
    let mut req = client.request(req_method, &target_url);
    if !body.is_empty() {
        req = req.body(body);
    }
    if let Some(ct) = headers.get("content-type") {
        req = req.header("Content-Type", ct.as_str());
    }
    let secret_name = headers.get("x-jani-secret-name").map(String::as_str);
    let chosen_secret = secret_name
        .and_then(|name| secrets.get(name))
        .or_else(|| secrets.values().next());
    if let Some(secret) = chosen_secret {
        let header_name = headers
            .get("x-jani-auth-header")
            .map(String::as_str)
            .unwrap_or("Authorization");
        let header_val = if header_name.eq_ignore_ascii_case("authorization") {
            format!("Bearer {secret}")
        } else {
            secret.clone()
        };
        req = req.header(header_name, header_val);
    }

    match req.send() {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let raw = resp.bytes().map(|b| b.to_vec()).unwrap_or_default();
            let mut text = String::from_utf8_lossy(&raw).into_owned();
            for secret in secrets.values() {
                if !secret.is_empty() {
                    text = text.replace(secret, "[REDACTED]");
                }
            }
            let scrubbed = match serde_json::from_str::<Value>(&text) {
                Ok(mut parsed) => {
                    redact_json(&mut parsed);
                    parsed.to_string()
                }
                Err(_) => {
                    let (clean, _) = redact_text(&text);
                    let classified =
                        governance.classify_tool_output("connector-response", &target_url, &clean);
                    json!({
                        "body": clean,
                        "classification": classified.effective.as_str(),
                    })
                    .to_string()
                }
            };
            write_http(stream, status, "OK", scrubbed.as_bytes());
        }
        Err(err) => {
            let body = json!({"error": err.to_string()}).to_string();
            write_http(stream, 502, "Bad Gateway", body.as_bytes());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PluginIntegrity, PluginKind, PluginMetadata, PluginRuntime, PluginSpec};

    fn manifest() -> PluginManifest {
        PluginManifest {
            api_version: "plugins.jani.dev/v1alpha1".into(),
            kind: "Plugin".into(),
            metadata: PluginMetadata {
                id: "com.example.issues".into(),
                name: "Issue tools".into(),
                version: "1.0.0".into(),
                publisher: "example".into(),
            },
            spec: PluginSpec {
                kind: PluginKind::Tool,
                runtime: PluginRuntime {
                    runtime_type: "builtin".into(),
                    artifact: String::new(),
                    entrypoint: "issues.list".into(),
                },
                capabilities: vec![],
                requested_permissions: None,
            },
            integrity: None,
        }
    }

    fn cfg_with_dir(dir: &Path, public: &str) -> Config {
        let mut cfg = Config::baseline();
        cfg.plugin_dir = Some(dir.to_path_buf());
        cfg.plugin_keys = vec![format!("default:{public}")];
        cfg
    }

    #[test]
    fn signed_manifests_load_and_tampering_is_refused() {
        let secret = "77".repeat(32);
        let public = crate::crypto::public_key_hex(&secret).unwrap();
        let dir =
            std::env::temp_dir().join(format!("jani-plugins-{}", crate::process::unique_id()));
        fs::create_dir_all(&dir).unwrap();

        let mut manifest = manifest();
        let digest = manifest.computed_digest();
        let signature = crate::crypto::sign_hex(&secret, digest.as_bytes()).unwrap();
        manifest.integrity = Some(PluginIntegrity {
            digest,
            signature,
            key_id: "default".into(),
        });
        let path = dir.join("issues.json");
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let cfg = cfg_with_dir(&dir, &public);
        let registry = PluginRegistry::load(&cfg).unwrap();
        assert!(registry.plugins.contains_key("com.example.issues"));

        let tampered = fs::read_to_string(&path)
            .unwrap()
            .replace("Issue tools", "Other tools");
        fs::write(&path, tampered).unwrap();
        assert!(PluginRegistry::load(&cfg).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn unsigned_and_native_plugins_are_refused() {
        let secret = "88".repeat(32);
        let public = crate::crypto::public_key_hex(&secret).unwrap();
        let dir =
            std::env::temp_dir().join(format!("jani-plugins-{}", crate::process::unique_id()));
        fs::create_dir_all(&dir).unwrap();
        let cfg = cfg_with_dir(&dir, &public);

        let unsigned = manifest();
        fs::write(
            dir.join("unsigned.json"),
            serde_json::to_vec(&unsigned).unwrap(),
        )
        .unwrap();
        assert!(PluginRegistry::load(&cfg).is_err());

        let mut native = manifest();
        native.spec.runtime.runtime_type = "native".into();
        let digest = native.computed_digest();
        native.integrity = Some(PluginIntegrity {
            digest: digest.clone(),
            signature: crate::crypto::sign_hex(&secret, digest.as_bytes()).unwrap(),
            key_id: "default".into(),
        });
        fs::write(
            dir.join("unsigned.json"),
            serde_json::to_vec(&native).unwrap(),
        )
        .unwrap();
        assert!(PluginRegistry::load(&cfg).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn digest_helper_is_stable() {
        let manifest = manifest();
        assert_eq!(
            manifest.computed_digest(),
            crate::types::sha256_hex(&serde_json::to_vec(&manifest).unwrap())
        );
    }

    #[test]
    fn revoked_plugin_keys_and_autonomous_merges_are_refused() {
        let secret = "99".repeat(32);
        let public = crate::crypto::public_key_hex(&secret).unwrap();
        let dir =
            std::env::temp_dir().join(format!("jani-plugins-rev-{}", crate::process::unique_id()));
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("vcs.py");
        fs::write(&script, "print('{}')").unwrap();

        let mut m = manifest();
        m.metadata.id = "com.acme.vcs".into();
        m.spec.kind = PluginKind::Connector;
        m.spec.runtime = PluginRuntime {
            runtime_type: "sidecar".into(),
            artifact: "vcs.py".into(),
            entrypoint: "serve".into(),
        };
        m.spec.capabilities = vec![CapabilityDescriptor {
            name: "pr.merge".into(),
            version: 1,
            effect: crate::types::Effect::ExternalWrite,
            risk_tier: crate::types::RiskTier::R3,
            approval_class: None,
            idempotency: None,
            input_schema: None,
            output_schema: None,
            resources: vec![],
            data_classes: vec![],
        }];
        let digest = m.computed_digest();
        m.integrity = Some(PluginIntegrity {
            digest: digest.clone(),
            signature: crate::crypto::sign_hex(&secret, digest.as_bytes()).unwrap(),
            key_id: "revoked-k1".into(),
        });
        fs::write(dir.join("vcs.json"), serde_json::to_vec(&m).unwrap()).unwrap();

        let mut cfg = cfg_with_dir(&dir, &public);
        cfg.plugin_keys = vec![format!("revoked-k1:{public}")];
        cfg.revoked_keys = vec!["revoked-k1".into()];
        assert!(PluginRegistry::load(&cfg)
            .err()
            .unwrap()
            .contains("revoked"));

        cfg.revoked_keys.clear();
        let registry = PluginRegistry::load(&cfg).unwrap();
        let (_, cap) = registry.capability("com.acme.vcs", "pr.merge").unwrap();
        assert!(
            enforce_human_commit_invariant(cap, &json!({"auto_merge": true}))
                .unwrap_err()
                .contains("USER_COMMIT_REQUIRED")
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn sidecar_plugin_invokes_via_zero_secret_broker_and_scrubs_responses() {
        let dir = std::env::temp_dir().join(format!(
            "jani-plugins-sidecar-{}",
            crate::process::unique_id()
        ));
        fs::create_dir_all(&dir).unwrap();

        // Start a local mock API server simulating GitHub/Jira/Slack endpoint.
        let mock_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mock_addr = mock_listener.local_addr().unwrap();
        let server_handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = mock_listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut header_block = String::new();
                loop {
                    let mut line = String::new();
                    let n = reader.read_line(&mut line).unwrap_or(0);
                    if n == 0 || line == "\r\n" || line == "\n" {
                        break;
                    }
                    header_block.push_str(&line);
                }
                let saw_auth = header_block.lines().any(|l| {
                    l.eq_ignore_ascii_case("authorization: bearer super-secret-connector-tok")
                });
                let body = json!({
                    "ok": true,
                    "sawInjectedBearer": saw_auth,
                    "echoedSecret": "super-secret-connector-tok"
                })
                .to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let py_script = r#"import json, os, sys, urllib.request, urllib.error
req = json.loads(sys.stdin.readline())
env_leaked = "CONNECTOR_SECRET" in os.environ
proxy = os.environ["JANI_BROKER_PROXY"]
lease = os.environ["JANI_BROKER_LEASE"]
mock_url = req["params"]["url"]

# Allowed host call through zero-secret proxy
r = urllib.request.Request(
    proxy + "/proxy",
    data=b'{"title":"ok"}',
    headers={
        "X-Jani-Lease": lease,
        "X-Jani-Target-Url": mock_url,
        "X-Jani-Secret-Name": "CONNECTOR_SECRET",
        "Content-Type": "application/json",
    },
    method="POST",
)
with urllib.request.urlopen(r, timeout=5) as resp:
    allowed_body = json.loads(resp.read().decode())

# Disallowed metadata SSRF host must fail with 403
blocked_status = None
try:
    r2 = urllib.request.Request(
        proxy + "/proxy",
        headers={"X-Jani-Lease": lease, "X-Jani-Target-Url": "http://169.254.169.254/latest"},
        method="GET",
    )
    urllib.request.urlopen(r2, timeout=5)
except urllib.error.HTTPError as e:
    blocked_status = e.code

print(json.dumps({
    "jsonrpc": "2.0",
    "id": req["id"],
    "result": {
        "envLeaked": env_leaked,
        "allowed": allowed_body,
        "blockedStatus": blocked_status
    }
}))
"#;
        fs::write(dir.join("connector.py"), py_script).unwrap();

        let secret = "66".repeat(32);
        let public = crate::crypto::public_key_hex(&secret).unwrap();
        let mut m = manifest();
        m.metadata.id = "com.acme.connector".into();
        m.spec.kind = PluginKind::Connector;
        m.spec.runtime = PluginRuntime {
            runtime_type: "sidecar".into(),
            artifact: "connector.py".into(),
            entrypoint: "serve".into(),
        };
        m.spec.capabilities = vec![CapabilityDescriptor {
            name: "comment.create".into(),
            version: 1,
            effect: crate::types::Effect::ExternalWrite,
            risk_tier: crate::types::RiskTier::R2,
            approval_class: Some("external-write".into()),
            idempotency: Some("keyed".into()),
            input_schema: None,
            output_schema: None,
            resources: vec!["pr:*".into()],
            data_classes: vec![],
        }];
        m.spec.requested_permissions = Some(crate::types::RequestedPermissions {
            network_services: vec![mock_addr.to_string()],
            secrets: vec!["CONNECTOR_SECRET".into()],
            data_purposes: vec!["engineering".into()],
            fs_access: "none".into(),
        });
        let digest = m.computed_digest();
        m.integrity = Some(PluginIntegrity {
            digest: digest.clone(),
            signature: crate::crypto::sign_hex(&secret, digest.as_bytes()).unwrap(),
            key_id: "default".into(),
        });
        fs::write(dir.join("connector.json"), serde_json::to_vec(&m).unwrap()).unwrap();

        let cfg = cfg_with_dir(&dir, &public);
        let registry = PluginRegistry::load(&cfg).unwrap();
        let governance = Governance::new(Default::default(), "acme", "bot-1", "engineering");
        let mut secrets = BTreeMap::new();
        secrets.insert(
            "CONNECTOR_SECRET".into(),
            "super-secret-connector-tok".into(),
        );
        let (result, _receipt) = registry
            .invoke_sidecar_with_secrets(
                &cfg,
                "com.acme.connector",
                "comment.create",
                &json!({"url": format!("http://{mock_addr}/repos/org/repo/issues/1/comments")}),
                &governance,
                None,
                Some(&secrets),
            )
            .unwrap();
        assert_eq!(result["envLeaked"], false);
        assert_eq!(result["allowed"]["sawInjectedBearer"], true);
        assert_eq!(result["allowed"]["echoedSecret"], "[REDACTED]");
        assert_eq!(result["blockedStatus"], 403);
        let _ = server_handle.join();
        let _ = fs::remove_dir_all(dir);
    }
}

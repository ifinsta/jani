use crate::config::{validate_relative, Config};
use crate::crypto;
use crate::data::Governance;
use crate::sandbox::SandboxManager;
use crate::types::{CapabilityDescriptor, PluginManifest, SandboxProfile, SandboxReceipt};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
#[cfg(target_os = "linux")]
mod credential_broker;
#[cfg(target_os = "linux")]
mod sidecar;

pub struct RegisteredPlugin {
    pub manifest: PluginManifest,
    pub digest: String,
    pub source: PathBuf,
    pub signature_key: String,
    pub artifact_path: Option<PathBuf>,
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
            let artifact_path = if manifest.spec.runtime.runtime_type == "sidecar" {
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
                Some(canon)
            } else {
                None
            };
            registry.plugins.insert(
                manifest.metadata.id.clone(),
                RegisteredPlugin {
                    digest,
                    source: path,
                    signature_key: integrity.key_id.clone(),
                    artifact_path,
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
                    "invocable": sidecar_has_signed_artifact(plugin),
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
            "invocable": sidecar_has_signed_artifact(plugin),
            "invocationNote": "sidecar execution requires signed artifact bytes and a host sandbox with filesystem and network confinement"
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
        control: crate::process::ProcessGate<'_>,
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
        control
            .authority
            .require_current()
            .map_err(|e| e.to_string())?;
        if control
            .cancelled
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
        {
            return Err("sidecar plugin cancelled by operator".into());
        }
        let permissions = plugin.manifest.spec.requested_permissions.as_ref();
        if permissions.is_some_and(|permissions| {
            !permissions.fs_access.is_empty() && permissions.fs_access != "none"
        }) {
            return Err(
                "sidecar filesystem access beyond the private runtime is unsupported".into(),
            );
        }
        let connected =
            permissions.is_some_and(|permissions| !permissions.network_services.is_empty());
        let profile = if connected {
            SandboxProfile::EnterpriseConnected
        } else {
            SandboxProfile::EnterpriseRestricted
        };
        let plan = SandboxManager::new(cfg).prepare_sidecar(profile)?;
        if !plan.filesystem_confined || !plan.network_confined {
            return Err("sidecar plugin requires filesystem and network isolation".into());
        }
        #[cfg(target_os = "linux")]
        {
            sidecar::invoke(cfg, (plugin, cap), args, &plan, governance, control)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = governance;
            Err("sidecar sandbox backend is unavailable on this host".into())
        }
    }
}

fn sidecar_has_signed_artifact(plugin: &RegisteredPlugin) -> bool {
    plugin.artifact_path.is_some()
        && cfg!(target_os = "linux")
        && plugin.manifest.spec.runtime.runtime_type == "sidecar"
        && plugin
            .manifest
            .spec
            .runtime
            .artifact_sha256
            .as_deref()
            .is_some_and(valid_artifact_digest)
}

fn valid_artifact_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
                    artifact_sha256: None,
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
            artifact_sha256: None,
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

    fn signed_sidecar(dir: &Path, content: &str, services: Vec<String>) -> Config {
        let secret = "66".repeat(32);
        let public = crate::crypto::public_key_hex(&secret).unwrap();
        fs::write(dir.join("connector.py"), content).unwrap();
        let mut plugin = manifest();
        plugin.spec.runtime = PluginRuntime {
            runtime_type: "sidecar".into(),
            artifact: "connector.py".into(),
            artifact_sha256: Some(crate::types::sha256_hex(content.as_bytes())),
            entrypoint: "serve".into(),
        };
        plugin.spec.capabilities = vec![CapabilityDescriptor {
            name: "records.list".into(),
            version: 1,
            effect: crate::types::Effect::Read,
            risk_tier: crate::types::RiskTier::R1,
            approval_class: None,
            idempotency: None,
            input_schema: None,
            output_schema: None,
            resources: vec![],
            data_classes: vec![],
        }];
        plugin.spec.requested_permissions = Some(crate::types::RequestedPermissions {
            network_services: services,
            secrets: vec!["FIXTURE_CONNECTOR_KEY".into()],
            data_purposes: vec![],
            fs_access: "none".into(),
        });
        let digest = plugin.computed_digest();
        plugin.integrity = Some(PluginIntegrity {
            signature: crate::crypto::sign_hex(&secret, digest.as_bytes()).unwrap(),
            digest,
            key_id: "default".into(),
        });
        fs::write(
            dir.join("connector.json"),
            serde_json::to_vec(&plugin).unwrap(),
        )
        .unwrap();
        cfg_with_dir(dir, &public)
    }

    #[test]
    fn sidecar_refuses_missing_isolation_for_local_and_connected_profiles() {
        let dir = std::env::temp_dir().join(format!(
            "jani-sidecar-denial-{}",
            crate::process::unique_id()
        ));
        fs::create_dir(&dir).unwrap();
        let governance = Governance::new(Default::default(), "demo", "bot", "engineering");
        for services in [vec![], vec!["https://example.invalid".into()]] {
            let mut cfg = signed_sidecar(
                &dir,
                "raise RuntimeError('fixture must never run')",
                services,
            );
            cfg.sandbox_explicit = true;
            let registry = PluginRegistry::load(&cfg).unwrap();
            let error = registry
                .invoke_sidecar(
                    &cfg,
                    "com.example.issues",
                    "records.list",
                    &json!({}),
                    &governance,
                    crate::process::ProcessGate::default(),
                )
                .unwrap_err();
            assert!(error.contains("requires an OS backend"), "{error}");
            #[cfg(windows)]
            {
                cfg.sandbox_mode = crate::types::SandboxMode::WindowsJob;
                let error = registry
                    .invoke_sidecar(
                        &cfg,
                        "com.example.issues",
                        "records.list",
                        &json!({}),
                        &governance,
                        crate::process::ProcessGate::default(),
                    )
                    .unwrap_err();
                assert!(error.contains("requires filesystem confinement"), "{error}");
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires a working Linux Bubblewrap backend"]
    fn sidecar_bwrap_brokers_requests_inside_the_network_boundary() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        assert_eq!(
            std::env::var("FIXTURE_CONNECTOR_KEY").as_deref(),
            Ok("mock-connector-key")
        );

        let dir =
            std::env::temp_dir().join(format!("jani-sidecar-e2e-{}", crate::process::unique_id()));
        fs::create_dir(&dir).unwrap();
        let marker = dir.join("outside-the-sidecar");
        fs::write(&marker, "private fixture").unwrap();
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let origin = server.local_addr().unwrap();
        let upstream = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(25);
            let mut stream = loop {
                match server.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("no authorized broker request: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let saw_key = BufReader::new(stream.try_clone().unwrap())
                .lines()
                .map(Result::unwrap)
                .take_while(|line| !line.is_empty())
                .any(|line| line.eq_ignore_ascii_case("authorization: Bearer mock-connector-key"));
            assert!(saw_key);
            let body = br#"{"response":"\u006dock-connector-key"}"#;
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            stream.write_all(body).unwrap();
        });
        let script = r#"
import http.client, json, os, socket, sys

request = json.loads(sys.stdin.readline())
broker = request["janiBroker"]
assert broker["transport"] == "http+unix"
origin = request["params"]["origin"]
def proxy(target, key, method="GET"):
    with socket.socket(socket.AF_UNIX) as sock:
        sock.settimeout(6)
        sock.connect(broker["socket"])
        client = http.client.HTTPConnection("localhost")
        client.sock = sock
        client.request(method, "/proxy", headers={
            "X-Jani-Lease": broker["lease"],
            "X-Jani-Target-Url": target,
            "X-Jani-Secret-Name": key,
        })
        response = client.getresponse()
        return response.status, json.loads(response.read())
try:
    socket.create_connection(("127.0.0.1", origin), timeout=0.5).close()
    direct = True
except OSError:
    direct = False
allowed, body = proxy(f"http://127.0.0.1:{origin}/records", "FIXTURE_CONNECTOR_KEY")
wrong_key, _ = proxy(f"http://127.0.0.1:{origin}/records", "UNKNOWN_FIXTURE_KEY")
wrong_port, _ = proxy("http://127.0.0.1:9/records", "FIXTURE_CONNECTOR_KEY")
write_attempt, _ = proxy(f"http://127.0.0.1:{origin}/records", "FIXTURE_CONNECTOR_KEY", "POST")
print(json.dumps({"jsonrpc":"2.0","id":request["id"],"result":{
    "allowed":allowed,"body":body,"direct":direct,"wrongKey":wrong_key,"wrongPort":wrong_port,
    "writeAttempt":write_attempt,
    "artifactIsReadOnly":not os.access(__file__, os.W_OK),
    "hostFileVisible":os.path.exists(request["params"]["marker"]),
    "credentialInEnv":"FIXTURE_CONNECTOR_KEY" in os.environ,
    "lease":broker["lease"],
}}))
"#;
        let mut cfg = signed_sidecar(&dir, script, vec![format!("http://{origin}")]);
        cfg.sandbox_mode = crate::types::SandboxMode::Bwrap;
        cfg.root = dir.clone();
        cfg.allow_write = true;
        cfg.write_paths.push(PathBuf::from("outside-the-sidecar"));
        let registry = PluginRegistry::load(&cfg).unwrap();
        let governance = Governance::new(Default::default(), "demo", "bot", "engineering");
        let (result, receipt) = registry
            .invoke_sidecar(
                &cfg,
                "com.example.issues",
                "records.list",
                &json!({"origin": origin.port(), "marker": marker}),
                &governance,
                crate::process::ProcessGate::default(),
            )
            .unwrap();
        upstream.join().unwrap();
        assert_eq!(
            result,
            json!({
                "allowed":200,"body":{"response":"[REDACTED]"},"direct":false,"wrongKey":403,"wrongPort":403,
                "writeAttempt":403,
                "artifactIsReadOnly":true,"hostFileVisible":false,"credentialInEnv":false,"lease":"[REDACTED]",
            })
        );
        assert!(receipt.filesystem_confined && receipt.network_confined && receipt.process_tree);
        fs::remove_dir_all(dir).unwrap();
    }
}

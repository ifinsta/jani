use crate::config::Config;
use crate::crypto;
use crate::types::PluginManifest;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

pub struct RegisteredPlugin {
    pub manifest: PluginManifest,
    pub digest: String,
    pub source: PathBuf,
    pub signature_key: String,
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
            crypto::verify_signature(
                &cfg.plugin_keys,
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
            registry.plugins.insert(
                manifest.metadata.id.clone(),
                RegisteredPlugin {
                    digest,
                    source: path,
                    signature_key: integrity.key_id.clone(),
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
                    "invocable": false,
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
}

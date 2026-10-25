//! Project settings loaded from `.jani/settings.json`.
//!
//! Settings supply model profiles, a harness step limit and Claude-style
//! permission rules. They cannot grant write or command authority: a settings
//! `allow` rule is a filter below the operator's CLI grants, and the broker
//! still evaluates every call against the contract and signed policy.
//!
//! A profile may carry an inline `apiKey`. That key is deliberately kept out of
//! `Debug` output and validation errors; `doctor` reports only where a key came
//! from. `apiKeyEnv` is preferred because it keeps the secret out of the file.

use crate::config::JResult;
use crate::permissions::{Mode, Policy as PermissionPolicy};
use crate::types::ModelProtocol;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::Path;

const MAX_SETTINGS_BYTES: u64 = 128 * 1024;
const MAX_PROFILES: usize = 32;
const MAX_STEPS: usize = 80;
const MAX_KEY_BYTES: usize = 8192;
const MAX_MODEL_ID_BYTES: usize = 256;
const MAX_URL_BYTES: usize = 2048;
const MAX_PERMISSION_RULES: usize = 128;
const MAX_PERMISSION_RULE_BYTES: usize = 512;
const MAX_OUTPUT_TOKENS: usize = 200_000;

#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Settings {
    #[serde(rename = "$schema", default)]
    pub schema: Option<String>,
    pub version: u32,
    /// Top-level selector: a profile name or a raw model ID, as in Claude Code.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub api_base: Option<String>,
    #[serde(default)]
    pub api_key: Option<Secret>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub max_output_tokens: Option<usize>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelProfile>,
    #[serde(default)]
    pub max_steps: Option<usize>,
    #[serde(default)]
    pub permissions: Option<Permissions>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelProfile {
    pub model: String,
    #[serde(default)]
    pub api_base: Option<String>,
    #[serde(default)]
    pub api_key: Option<Secret>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub max_output_tokens: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Permissions {
    #[serde(default)]
    pub default_mode: Option<String>,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

impl fmt::Debug for Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Settings")
            .field("version", &self.version)
            .field("model", &self.model)
            .field("default_model", &self.default_model)
            .field("api_base", &self.api_base)
            .field("api_key", &self.api_key)
            .field("api_key_env", &self.api_key_env)
            .field("protocol", &self.protocol)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("models", &self.models)
            .field("max_steps", &self.max_steps)
            .field("permissions", &self.permissions)
            .finish()
    }
}

impl fmt::Debug for ModelProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelProfile")
            .field("model", &self.model)
            .field("api_base", &self.api_base)
            .field("api_key", &self.api_key)
            .field("api_key_env", &self.api_key_env)
            .field("protocol", &self.protocol)
            .field("max_output_tokens", &self.max_output_tokens)
            .finish()
    }
}

impl Settings {
    pub fn load(path: &Path) -> JResult<Self> {
        let meta = fs::metadata(path).map_err(|e| format!("settings {}: {e}", path.display()))?;
        if !meta.is_file() {
            return Err(format!("settings {} is not a regular file", path.display()));
        }
        if meta.len() > MAX_SETTINGS_BYTES {
            return Err(format!(
                "settings {} exceeds {MAX_SETTINGS_BYTES} bytes",
                path.display()
            ));
        }
        let text =
            fs::read_to_string(path).map_err(|e| format!("settings {}: {e}", path.display()))?;
        let settings: Settings = serde_json::from_str(&text)
            .map_err(|e| format!("invalid settings JSON in {}: {e}", path.display()))?;
        settings
            .validate()
            .map_err(|e| format!("settings {}: {e}", path.display()))?;
        Ok(settings)
    }

    pub fn profile(&self, name: &str) -> JResult<&ModelProfile> {
        self.models
            .get(name)
            .ok_or_else(|| format!("settings has no model profile '{name}'"))
    }

    /// Resolve the profile selected by `--model-profile`, an explicit top-level
    /// `model` that names a profile, or the legacy `defaultModel`.
    pub fn selected_profile(&self, requested: Option<&str>) -> JResult<Option<String>> {
        if let Some(name) = requested {
            self.profile(name)?;
            return Ok(Some(name.to_string()));
        }
        if let Some(model) = self.model.as_deref() {
            if self.models.contains_key(model) {
                return Ok(Some(model.to_string()));
            }
        }
        Ok(self.default_model.clone())
    }

    /// A top-level `model` that is not a profile name is a raw model ID.
    pub fn model_override(&self) -> Option<&str> {
        self.model
            .as_deref()
            .filter(|model| !self.models.contains_key(*model))
    }

    pub fn permissions(&self) -> JResult<Option<PermissionPolicy>> {
        let Some(raw) = self.permissions.as_ref() else {
            return Ok(None);
        };
        let mode = match raw.default_mode.as_deref() {
            Some(value) => Mode::parse(value)?,
            None => Mode::Default,
        };
        PermissionPolicy::compile(mode, &raw.allow, &raw.ask, &raw.deny).map(Some)
    }

    pub fn warnings(&self) -> Vec<String> {
        let mut locations = Vec::new();
        if self.api_key.is_some() {
            locations.push("top-level apiKey".to_string());
        }
        for (name, profile) in &self.models {
            if profile.api_key.is_some() {
                locations.push(format!("models.{name}.apiKey"));
            }
        }
        locations
            .into_iter()
            .map(|location| {
                format!(
                    "{location} stores a provider key inline; keep .jani/settings.json out of version control or use apiKeyEnv"
                )
            })
            .collect()
    }

    fn validate(&self) -> JResult<()> {
        if self.version != 1 {
            return Err(format!(
                "unsupported settings version {}; expected 1",
                self.version
            ));
        }
        if let Some(schema) = &self.schema {
            if schema.trim().is_empty()
                || schema.len() > 512
                || schema.chars().any(char::is_control)
            {
                return Err("$schema must be a short non-empty string".into());
            }
        }
        if self.models.len() > MAX_PROFILES {
            return Err(format!(
                "settings define more than {MAX_PROFILES} model profiles"
            ));
        }
        if let Some(steps) = self.max_steps {
            if !(1..=MAX_STEPS).contains(&steps) {
                return Err(format!("maxSteps must be 1..{MAX_STEPS}"));
            }
        }
        validate_common_model_settings(
            self.api_base.as_deref(),
            self.api_key.as_ref(),
            self.api_key_env.as_deref(),
            self.protocol.as_deref(),
            self.max_output_tokens,
            "settings",
        )?;
        if let Some(model) = &self.model {
            if model.trim().is_empty()
                || model.len() > MAX_MODEL_ID_BYTES
                || model.chars().any(char::is_control)
            {
                return Err("model must be a non-empty profile name or model ID".into());
            }
        }
        for (name, profile) in &self.models {
            validate_profile_name(name)?;
            profile.validate(name)?;
        }
        // Claude-style configurations select a profile with the top-level
        // `model` key; `defaultModel` remains accepted as the explicit form.
        let default = self.default_model.as_ref().or_else(|| {
            self.model
                .as_ref()
                .filter(|model| self.models.contains_key(*model))
        });
        match default {
            Some(name) => {
                validate_profile_name(name)?;
                if !self.models.contains_key(name) {
                    return Err(format!("defaultModel '{name}' is not defined in models"));
                }
            }
            // A raw top-level `model` is complete on its own: it selects the
            // model ID while the top-level transport and credential apply, and
            // profiles stay reachable through `--model-profile`.
            None if !self.models.is_empty() && self.model.is_none() => {
                return Err(
                    "settings define model profiles but no defaultModel or profile selector".into(),
                );
            }
            None => {}
        }
        if let Some(permissions) = &self.permissions {
            permissions.validate()?;
        }
        Ok(())
    }
}

impl ModelProfile {
    fn validate(&self, name: &str) -> JResult<()> {
        if self.model.trim().is_empty()
            || self.model.len() > MAX_MODEL_ID_BYTES
            || self.model.chars().any(char::is_control)
        {
            return Err(format!("model profile '{name}' has an invalid model ID"));
        }
        validate_common_model_settings(
            self.api_base.as_deref(),
            self.api_key.as_ref(),
            self.api_key_env.as_deref(),
            self.protocol.as_deref(),
            self.max_output_tokens,
            &format!("model profile '{name}'"),
        )
    }
}

impl Permissions {
    fn validate(&self) -> JResult<()> {
        for (label, rules) in [
            ("allow", &self.allow),
            ("ask", &self.ask),
            ("deny", &self.deny),
        ] {
            if rules.len() > MAX_PERMISSION_RULES {
                return Err(format!(
                    "permissions.{label} has more than {MAX_PERMISSION_RULES} rules"
                ));
            }
            for rule in rules {
                if rule.trim().is_empty() || rule.len() > MAX_PERMISSION_RULE_BYTES {
                    return Err(format!(
                        "permissions.{label} rules must be 1..={MAX_PERMISSION_RULE_BYTES} bytes"
                    ));
                }
            }
        }
        Ok(())
    }
}

fn validate_common_model_settings(
    api_base: Option<&str>,
    api_key: Option<&Secret>,
    api_key_env: Option<&str>,
    protocol: Option<&str>,
    max_output_tokens: Option<usize>,
    label: &str,
) -> JResult<()> {
    if let Some(base) = api_base {
        if base.trim().is_empty()
            || base.len() > MAX_URL_BYTES
            || base.trim() != base
            || base.chars().any(char::is_control)
        {
            return Err(format!("{label} has an invalid apiBase"));
        }
    }
    if let Some(env_name) = api_key_env {
        validate_env_name(env_name).map_err(|e| format!("{label}: {e}"))?;
    }
    if let Some(key) = api_key {
        let value = key.expose();
        if value.is_empty()
            || value.len() > MAX_KEY_BYTES
            || value.chars().any(char::is_whitespace)
            || value.chars().any(char::is_control)
        {
            return Err(format!(
                "{label} has an invalid apiKey; the value is not echoed"
            ));
        }
    }
    if let Some(protocol) = protocol {
        if ModelProtocol::parse(protocol).is_none() {
            return Err(format!(
                "{label} has unknown protocol '{protocol}'; use openai or anthropic"
            ));
        }
    }
    if let Some(tokens) = max_output_tokens {
        if !(1..=MAX_OUTPUT_TOKENS).contains(&tokens) {
            return Err(format!(
                "{label} maxOutputTokens must be 1..{MAX_OUTPUT_TOKENS}"
            ));
        }
    }
    Ok(())
}

fn validate_profile_name(name: &str) -> JResult<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("model profile names must be 1..=64 ASCII letters, digits, '-' or '_'".into());
    }
    Ok(())
}

fn validate_env_name(name: &str) -> JResult<()> {
    let mut chars = name.chars();
    let first_ok = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_');
    if !first_ok || name.len() > 128 || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err("apiKeyEnv must be a valid environment variable name".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jani-settings-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_settings(body: &str) -> (PathBuf, PathBuf) {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        fs::write(&path, body).unwrap();
        (dir, path)
    }

    const VALID: &str = r#"{
        "$schema": "./settings.schema.json",
        "version": 1,
        "model": "fast",
        "models": {
            "fast": {
                "model": "gpt-5-mini",
                "apiBase": "https://api.openai.com/v1",
                "apiKeyEnv": "JANI_API_KEY"
            },
            "deep": {"model": "gpt-5"}
        },
        "maxSteps": 30,
        "permissions": {
            "defaultMode": "default",
            "allow": ["Bash(cargo test *)"],
            "ask": ["Bash(git push *)"],
            "deny": ["Read(./.env)", "Read(./.env.*)"]
        }
    }"#;

    #[test]
    fn loads_profiles_limits_and_permissions() {
        let (dir, path) = write_settings(VALID);
        let settings = Settings::load(&path).unwrap();
        assert_eq!(settings.model.as_deref(), Some("fast"));
        assert_eq!(
            settings.selected_profile(None).unwrap().as_deref(),
            Some("fast")
        );
        assert_eq!(settings.max_steps, Some(30));
        assert_eq!(settings.profile("fast").unwrap().model, "gpt-5-mini");
        assert_eq!(
            settings.profile("fast").unwrap().api_key_env.as_deref(),
            Some("JANI_API_KEY")
        );
        let policy = settings.permissions().unwrap().unwrap();
        assert_eq!(policy.mode(), Mode::Default);
        assert!(policy.describe().contains("deny=2"));
        assert!(settings.warnings().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn inline_api_key_is_accepted_but_never_debug_printed() {
        let (dir, path) = write_settings(
            r#"{"version":1,"models":{"fast":{"model":"m","apiKey":"fixture-inline-credential"}},"defaultModel":"fast"}"#,
        );
        let settings = Settings::load(&path).unwrap();
        assert_eq!(
            settings
                .profile("fast")
                .unwrap()
                .api_key
                .as_ref()
                .unwrap()
                .expose(),
            "fixture-inline-credential"
        );
        let debug = format!("{settings:?}");
        assert!(!debug.contains("fixture-inline-credential"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
        let warnings = settings.warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("apiKeyEnv"), "{warnings:?}");
        assert!(!warnings[0].contains("fixture-inline-credential"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn top_level_model_selects_a_profile_or_names_a_raw_model() {
        let (dir, path) = write_settings(
            r#"{"version":1,"model":"deep","defaultModel":"fast","models":{"fast":{"model":"a"},"deep":{"model":"b"}}}"#,
        );
        let settings = Settings::load(&path).unwrap();
        assert_eq!(
            settings.selected_profile(None).unwrap().as_deref(),
            Some("deep")
        );
        assert!(settings.model_override().is_none());
        fs::remove_dir_all(dir).unwrap();

        let (dir, path) = write_settings(
            r#"{"version":1,"model":"claude-sonnet-5-5","defaultModel":"fast","models":{"fast":{"model":"a","apiBase":"https://api.anthropic.com/v1","protocol":"anthropic"}}}"#,
        );
        let settings = Settings::load(&path).unwrap();
        assert_eq!(
            settings.selected_profile(None).unwrap().as_deref(),
            Some("fast")
        );
        assert_eq!(settings.model_override(), Some("claude-sonnet-5-5"));
        fs::remove_dir_all(dir).unwrap();

        let (dir, path) =
            write_settings(r#"{"version":1,"model":"raw-model","models":{"fast":{"model":"a"}}}"#);
        let settings = Settings::load(&path).unwrap();
        assert!(settings.selected_profile(None).unwrap().is_none());
        assert_eq!(settings.model_override(), Some("raw-model"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_fields_are_rejected_but_schema_is_allowed() {
        let (dir, path) = write_settings(r#"{"version":1,"typo":true}"#);
        let error = Settings::load(&path).unwrap_err();
        assert!(error.contains("unknown field"), "{error}");
        fs::remove_dir_all(dir).unwrap();

        let (dir, path) = write_settings(r#"{"$schema":"x","version":1}"#);
        assert!(Settings::load(&path).is_ok());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn profiles_require_an_existing_default() {
        let (dir, path) = write_settings(r#"{"version":1,"models":{"fast":{"model":"m"}}}"#);
        let error = Settings::load(&path).unwrap_err();
        assert!(error.contains("no defaultModel"), "{error}");
        fs::remove_dir_all(dir).unwrap();

        let (dir, path) = write_settings(
            r#"{"version":1,"models":{"fast":{"model":"m"}},"defaultModel":"deep"}"#,
        );
        let error = Settings::load(&path).unwrap_err();
        assert!(error.contains("not defined"), "{error}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unsupported_version_and_bad_limits_fail_closed() {
        let (dir, path) = write_settings(r#"{"version":2}"#);
        let error = Settings::load(&path).unwrap_err();
        assert!(error.contains("unsupported settings version"), "{error}");
        fs::remove_dir_all(dir).unwrap();

        let (dir, path) = write_settings(r#"{"version":1,"maxSteps":81}"#);
        let error = Settings::load(&path).unwrap_err();
        assert!(error.contains("maxSteps must be 1..80"), "{error}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_credential_reference_is_rejected() {
        let (dir, path) = write_settings(
            r#"{"version":1,"models":{"fast":{"model":"m","apiKeyEnv":"NOT-VALID"}},"defaultModel":"fast"}"#,
        );
        let error = Settings::load(&path).unwrap_err();
        assert!(error.contains("apiKeyEnv"), "{error}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_inline_key_is_rejected_without_echoing_it() {
        let (dir, path) = write_settings(
            r#"{"version":1,"models":{"fast":{"model":"m","apiKey":"bad value with spaces"}},"defaultModel":"fast"}"#,
        );
        let error = Settings::load(&path).unwrap_err();
        assert!(error.contains("invalid apiKey"), "{error}");
        assert!(!error.contains("bad value"), "{error}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bypass_permissions_and_bad_rules_are_refused() {
        let (dir, path) =
            write_settings(r#"{"version":1,"permissions":{"defaultMode":"bypassPermissions"}}"#);
        let error = Settings::load(&path).unwrap().permissions().unwrap_err();
        assert!(error.contains("never bypasses"), "{error}");
        fs::remove_dir_all(dir).unwrap();

        let (dir, path) = write_settings(
            r#"{"version":1,"permissions":{"allow":["WebFetch(https://example.com)"]}}"#,
        );
        let error = Settings::load(&path).unwrap().permissions().unwrap_err();
        assert!(error.contains("unsupported tool"), "{error}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn shipped_example_loads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("docs")
            .join("schemas")
            .join("settings.example.json");
        let settings = Settings::load(&path).unwrap();
        assert_eq!(
            settings.selected_profile(None).unwrap().as_deref(),
            Some("fast")
        );
        assert!(settings.profile("deep").is_ok());
        assert!(settings.permissions().unwrap().is_some());
    }
}

//! Project settings loaded from `.jani/settings.json`.
//!
//! Settings supply model profiles and a harness step limit. They cannot grant
//! write or command authority, and they never carry provider credentials inline:
//! `apiKeyEnv` names the environment variable that holds the key.

use crate::config::JResult;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
const MAX_PROFILES: usize = 32;
const MAX_STEPS: usize = 80;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Settings {
    pub version: u32,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelProfile>,
    #[serde(default)]
    pub max_steps: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelProfile {
    pub model: String,
    #[serde(default)]
    pub api_base: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
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
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| format!("invalid settings JSON in {}: {e}", path.display()))?;
        reject_inline_secrets(&value).map_err(|e| format!("settings {}: {e}", path.display()))?;
        let settings: Settings = serde_json::from_value(value)
            .map_err(|e| format!("invalid settings in {}: {e}", path.display()))?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn profile(&self, name: &str) -> JResult<&ModelProfile> {
        self.models
            .get(name)
            .ok_or_else(|| format!("settings has no model profile '{name}'"))
    }

    fn validate(&self) -> JResult<()> {
        if self.version != 1 {
            return Err(format!(
                "unsupported settings version {}; expected 1",
                self.version
            ));
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
        for (name, profile) in &self.models {
            validate_profile_name(name)?;
            profile.validate(name)?;
        }
        match &self.default_model {
            Some(name) => {
                validate_profile_name(name)?;
                if !self.models.contains_key(name) {
                    return Err(format!("defaultModel '{name}' is not defined in models"));
                }
            }
            None if !self.models.is_empty() => {
                return Err("settings define model profiles but no defaultModel".into());
            }
            None => {}
        }
        Ok(())
    }
}

impl ModelProfile {
    fn validate(&self, name: &str) -> JResult<()> {
        if self.model.trim().is_empty()
            || self.model.len() > 128
            || self.model.chars().any(char::is_control)
        {
            return Err(format!("model profile '{name}' has an invalid model ID"));
        }
        if let Some(base) = &self.api_base {
            if base.trim().is_empty() || base.chars().any(char::is_control) {
                return Err(format!("model profile '{name}' has an invalid apiBase"));
            }
        }
        if let Some(env_name) = &self.api_key_env {
            validate_env_name(env_name).map_err(|e| format!("model profile '{name}': {e}"))?;
        }
        Ok(())
    }
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

fn reject_inline_secrets(value: &Value) -> JResult<()> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "apiKey" || key == "api_key" {
                    return Err(
                        "inline provider credentials are not allowed; use apiKeyEnv to name an environment variable"
                            .into(),
                    );
                }
                reject_inline_secrets(child)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                reject_inline_secrets(item)?;
            }
        }
        _ => {}
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
        "version": 1,
        "defaultModel": "fast",
        "models": {
            "fast": {
                "model": "gpt-5-mini",
                "apiBase": "https://api.openai.com/v1",
                "apiKeyEnv": "JANI_API_KEY"
            },
            "deep": {"model": "gpt-5"}
        },
        "maxSteps": 30
    }"#;

    #[test]
    fn loads_profiles_and_limits() {
        let (dir, path) = write_settings(VALID);
        let settings = Settings::load(&path).unwrap();
        assert_eq!(settings.default_model.as_deref(), Some("fast"));
        assert_eq!(settings.max_steps, Some(30));
        assert_eq!(settings.profile("fast").unwrap().model, "gpt-5-mini");
        assert_eq!(
            settings.profile("fast").unwrap().api_key_env.as_deref(),
            Some("JANI_API_KEY")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn inline_api_key_is_rejected() {
        let (dir, path) = write_settings(
            r#"{"version":1,"models":{"fast":{"model":"m","apiKey":"fixture-inline-credential"}},"defaultModel":"fast"}"#,
        );
        let error = Settings::load(&path).unwrap_err();
        assert!(error.contains("apiKeyEnv"), "{error}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let (dir, path) = write_settings(r#"{"version":1,"typo":true}"#);
        let error = Settings::load(&path).unwrap_err();
        assert!(error.contains("unknown field"), "{error}");
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
    fn shipped_example_loads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("docs")
            .join("schemas")
            .join("settings.example.json");
        let settings = Settings::load(&path).unwrap();
        assert_eq!(settings.default_model.as_deref(), Some("fast"));
        assert!(settings.profile("deep").is_ok());
    }
}

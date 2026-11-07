//! Environment-variable names that configure the agent.
//!
//! The whole external environment surface lives in this one list: `apply_env`
//! reads it, `process` strips the sensitive names before spawning children,
//! and operators can audit it here instead of grep'ing for JANI_* literals.

pub const API_KEY: &str = "JANI_API_KEY";
pub const OPENAI_API_KEY: &str = "OPENAI_API_KEY";
pub const MODEL: &str = "JANI_MODEL";
pub const OPENAI_MODEL: &str = "OPENAI_MODEL";
pub const API_BASE: &str = "JANI_API_BASE";
pub const PROTOCOL: &str = "JANI_PROTOCOL";
pub const MAX_OUTPUT_TOKENS: &str = "JANI_MAX_OUTPUT_TOKENS";
pub const MODE: &str = "JANI_MODE";
pub const TENANT: &str = "JANI_TENANT";
pub const PRINCIPAL: &str = "JANI_PRINCIPAL";
pub const PURPOSE: &str = "JANI_PURPOSE";
pub const PROVIDER_CLASS: &str = "JANI_PROVIDER_CLASS";
pub const PROVIDER_REGION: &str = "JANI_PROVIDER_REGION";
pub const WRITE_PATHS: &str = "JANI_WRITE_PATHS";
pub const EXEC_PROGRAMS: &str = "JANI_EXEC_PROGRAMS";
pub const POLICY: &str = "JANI_POLICY";
pub const POLICY_KEYS: &str = "JANI_POLICY_KEYS";
pub const IDENTITY: &str = "JANI_IDENTITY";
pub const CONTRACT: &str = "JANI_CONTRACT";
pub const STATE_DIR: &str = "JANI_STATE_DIR";
pub const SANDBOX: &str = "JANI_SANDBOX";
pub const SANDBOX_NET: &str = "JANI_SANDBOX_NET";
pub const PLUGIN_DIR: &str = "JANI_PLUGIN_DIR";
pub const PLUGIN_KEYS: &str = "JANI_PLUGIN_KEYS";
pub const SETTINGS: &str = "JANI_SETTINGS";
pub const USER_SETTINGS: &str = "JANI_USER_SETTINGS";
pub const SKILL_DIRS: &str = "JANI_SKILL_DIRS";
pub const UPDATE_SOURCE: &str = "JANI_UPDATE_SOURCE";
pub const UPDATE_RETRY_BASE_MS: &str = "JANI_UPDATE_RETRY_BASE_MS";
pub const MODEL_RETRY_BASE_MS: &str = "JANI_MODEL_RETRY_BASE_MS";

use crate::permissions::Policy as PermissionPolicy;
use crate::settings::Settings;
use crate::types::{ModelProtocol, NetworkPolicy, RuntimeMode, SandboxMode};
use std::env;
use std::fmt;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

pub type JResult<T> = Result<T, String>;

/// A model the running session may switch to. Profiles resolved from settings
/// carry their own transport and credential binding; the key is never rendered.
#[derive(Clone)]
pub struct ModelChoice {
    pub label: String,
    pub model: String,
    pub api_base: String,
    pub protocol: ModelProtocol,
    pub max_output_tokens: usize,
    pub profile: Option<String>,
    pub api_key_source: String,
    key: Option<String>,
}

impl fmt::Debug for ModelChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelChoice")
            .field("label", &self.label)
            .field("model", &self.model)
            .field("api_base", &self.api_base)
            .field("protocol", &self.protocol)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("profile", &self.profile)
            .field("api_key_source", &self.api_key_source)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl ModelChoice {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            label: cfg
                .settings_profile
                .clone()
                .unwrap_or_else(|| cfg.model.clone()),
            model: cfg.model.clone(),
            api_base: cfg.api_base.clone(),
            protocol: cfg.protocol,
            max_output_tokens: cfg.max_output_tokens,
            profile: cfg.settings_profile.clone(),
            api_key_source: cfg.api_key_source.clone(),
            key: cfg.api_key.clone(),
        }
    }

    pub fn api_key(&self) -> Option<&str> {
        self.key.as_deref()
    }
}

#[derive(Clone)]
pub struct Config {
    pub command: String,
    pub sub: Option<String>,
    pub positional: Vec<String>,
    pub prompt: Option<String>,
    pub model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub api_key_source: String,
    pub protocol: ModelProtocol,
    pub protocol_explicit: bool,
    pub max_output_tokens: usize,
    pub model_profile: Option<String>,
    pub model_choices: Vec<ModelChoice>,
    pub settings_path: Option<PathBuf>,
    pub settings_profile: Option<String>,
    pub settings_warnings: Vec<String>,
    pub permissions: Option<PermissionPolicy>,
    pub max_steps: usize,
    pub root: PathBuf,
    pub allow_write: bool,
    pub allow_exec: bool,
    pub write_paths: Vec<PathBuf>,
    pub exec_programs: Vec<String>,
    pub mode: RuntimeMode,
    pub tenant: String,
    pub principal: String,
    pub purpose: String,
    pub contract_path: Option<PathBuf>,
    pub policy_path: Option<PathBuf>,
    pub policy_keys: Vec<String>,
    pub identity_path: Option<PathBuf>,
    pub state_dir: PathBuf,
    pub sandbox_mode: SandboxMode,
    pub sandbox_explicit: bool,
    pub sandbox_net: NetworkPolicy,
    pub plugin_dir: Option<PathBuf>,
    pub plugin_keys: Vec<String>,
    pub mcp_contract_path: Option<PathBuf>,
    pub mcp_delegation_path: Option<PathBuf>,
    pub mcp_input_path: Option<PathBuf>,
    pub mcp_response_path: Option<PathBuf>,
    pub mcp_token_path: Option<PathBuf>,
    pub mcp_agent: String,
    pub mcp_contract_keys: Vec<String>,
    pub mcp_delegation_keys: Vec<String>,
    pub mcp_receipt_keys: Vec<String>,
    pub approval_ids: Vec<String>,
    pub provider_region: String,
    pub provider_class: String,
    pub action: Option<String>,
    pub resource: Option<String>,
    pub task_id: Option<String>,
    pub run_id: Option<String>,
    pub approve: bool,
    pub reject: bool,
    pub role: Option<String>,
}

fn default_command(interactive: bool) -> &'static str {
    if interactive {
        "repl"
    } else {
        "help"
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    env::var(name).ok().filter(|s| !s.trim().is_empty())
}

fn take_value(args: &[String], i: &mut usize, flag: &str) -> JResult<String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

pub fn help() -> &'static str {
    r#"Jani — Rust-native, skill-aware AI coding agent

USAGE:
  jani                              Interactive terminal (stdin must be a terminal)
  jani --version
  jani --help
  jani doctor [OPTIONS]
  jani skills [OPTIONS]
  jani run "TASK" [OPTIONS]
  jani --print "TASK" [OPTIONS]
  jani repl [OPTIONS]
  jani sandbox doctor|check [OPTIONS]
  jani plugins list|inspect|verify [ID] [OPTIONS]
  jani policy check|explain [OPTIONS]
  jani audit verify [--run RUN_ID] [OPTIONS]
  jani inspect RUN_ID [OPTIONS]
  jani approvals list|decide APPROVAL_ID --approve|--reject [OPTIONS]
  jani budget show [--task TASK_ID] [OPTIONS]
  jani identity whoami [OPTIONS]
  jani contract validate FILE [OPTIONS]
  jani agent register|status|sleep|wake AGENT [OPTIONS]
  jani alerts send AGENT EVENT SOURCE REFERENCE SEVERITY DEDUPE_KEY [OPTIONS]
  jani alerts list|claim AGENT [OPTIONS]
  jani alerts accept|retry AGENT ALERT_ID RECEIPT [OPTIONS]
  jani alerts resolve AGENT ALERT_ID EVIDENCE_REF [OPTIONS]
  jani wake poll|once|watch AGENT [OPTIONS]
  jani mirror inspect|critique|recommendations|record RUN_ID [OPTIONS]
  jani mirror workflows|history [OPTIONS]
  jani mirror compare RUN_A RUN_B [OPTIONS]
  jani mcp verify|authorize|call|validate-response OP RESOURCE [OPTIONS]
  jani native TOOL OPERATION [ARGUMENTS] [--task TASK] [OPTIONS]

Jani has no embedded coordination engine. It works directly in a Git
worktree. Skills are advisory. Every tool call passes through a Rust
policy enforcement point; enterprise mode is deny-first and fail-closed.

OPTIONS:
  --mode MODE             local-unmanaged (default) or enterprise
  --tenant ID             Requested tenant; enterprise derives it from --identity
  --principal ID          Requested principal; enterprise derives it from --identity
  --purpose PURPOSE       Data purpose (default SOFTWARE_DEV)
  --contract FILE         JSON work contract (can only restrict, never grant)
  --policy FILE           Signed organization policy bundle (required in enterprise)
  --policy-key KEY        Trusted policy/manifest key, ID:HEX or HEX; repeatable
  --identity FILE         Signed identity assertion (required for enterprise runs)
  --state-dir PATH        Journal, approvals and receipts (default .jani)
  --sandbox MODE          off | auto | bwrap | windows-job (default off locally, auto in enterprise)
  --sandbox-net POLICY    deny | loopback | host (default deny)
  --plugin-dir PATH       Trusted, administrator-approved plugin manifest directory
  --plugin-key KEY        Trusted plugin publisher key, ID:HEX or HEX; repeatable
  --mcp-contract FILE     Signed MCP governance contract
  --mcp-delegation FILE   Signed task-specific initiator grant
  --mcp-contract-key KEY  Trusted contract signer's Ed25519 public key
  --mcp-delegation-key K  Trusted authorization signer's public key
  --mcp-receipt-key KEY   Trusted MCP response signer public key
  --mcp-agent ID          Delegated agent identity label (default jani)
  --mcp-arguments FILE    Tool arguments JSON (must include exact resource)
  --mcp-response FILE     MCP response JSON for offline receipt validation
  --mcp-token-file FILE   OAuth access token file for HTTP call (not shown in logs)
  --approval ID           Pre-approved request ID to consume; repeatable
  --provider-region R     Provider processing region label
  --provider-class C      Provider trust class label
  --model MODEL           Model ID (JANI_MODEL)
  --api-base URL          API base (JANI_API_BASE)
  --protocol P            openai or anthropic (auto-detected from API host)
  --max-output-tokens N   Provider output ceiling (default 4096, 1..200000)
  --settings FILE         Settings JSON (default .jani/settings.json, then ~/.jani/settings.json; JANI_SETTINGS)
  --model-profile NAME    Model profile from settings (default top-level model)
  --repo PATH             Assigned Git worktree; defaults to current directory
  --max-steps N           Maximum model turns (default 20, ceiling 80)
  --allow-write           Enable gated file editing
  --write-path PATH       Approved relative edit scope; repeat as necessary
  --allow-exec            Enable allowlisted test/git commands
  --exec-program NAME     Additional test runner: pytest/python/node/go/dotnet/npm

COMMAND OPTIONS:
  --action NAME           Action to evaluate for `policy check`
  --resource RESOURCE     Canonical resource for `policy check`
  --run RUN_ID            Journal run to verify or inspect
  --task TASK_ID          Task to show in `budget show`
  --approve / --reject    Decision for `approvals decide`

ENVIRONMENT:
  JANI_API_KEY or OPENAI_API_KEY    AI provider credential
  JANI_API_BASE                     https://api.openai.com/v1 by default
  JANI_MODEL or OPENAI_MODEL        Model ID (default gpt-5)
  JANI_PROTOCOL                     openai or anthropic
  JANI_MAX_OUTPUT_TOKENS            Provider output ceiling
  JANI_WRITE_PATHS                  Comma-separated relative edit scope
  JANI_EXEC_PROGRAMS                Optional extra test runners
  JANI_SKILL_DIRS                   Extra skill directories, path-list separated
  JANI_MODE, JANI_TENANT, JANI_PRINCIPAL, JANI_PURPOSE
  JANI_POLICY, JANI_POLICY_KEYS, JANI_IDENTITY, JANI_CONTRACT
  JANI_STATE_DIR, JANI_SANDBOX, JANI_SANDBOX_NET
  JANI_PLUGIN_DIR, JANI_PLUGIN_KEYS, JANI_PROVIDER_REGION, JANI_PROVIDER_CLASS
  JANI_SETTINGS            Alternate settings file
  JANI_USER_SETTINGS       Override the user-wide settings path

SETTINGS:
  Jani loads settings from --settings / JANI_SETTINGS when given, otherwise
  <worktree>/.jani/settings.json when present, otherwise the user-wide
  ~/.jani/settings.json (%USERPROFILE%\.jani\settings.json on Windows;
  JANI_USER_SETTINGS overrides that path). Only the first file found is used,
  with no merging. It supplies a top-level model selector (a profile name or
  model ID), named model profiles, maxSteps and Claude-style permissions. A
  profile may set model, apiBase, protocol, maxOutputTokens, apiKeyEnv or an
  inline apiKey. Precedence is CLI, then environment, then settings; a
  profile's own credential binding is authoritative so a generic key is never
  sent to another provider.
  permissions.defaultMode is default, acceptEdits or plan; allow/ask/deny take
  rules such as Bash(cargo test *) and Read(./.env). Settings rules can only
  narrow the CLI grants, contract and signed policy: they never enable writes,
  commands or policy approvals. Prefer apiKeyEnv over an inline apiKey.

Skills are advisory instructions, not an executable security policy.
Tool flags are enforced in Rust. Execution is NOT an OS sandbox unless a
verified backend is selected and attested.
"#
}

#[derive(Default)]
struct CredentialBinding {
    profile_selected: bool,
    env_name: Option<String>,
    inline_key: Option<String>,
}

fn resolve_credential(
    profile_selected: bool,
    env_name: Option<&str>,
    inline_key: Option<&str>,
) -> (Option<String>, String) {
    if let Some(name) = env_name {
        return match env_nonempty(name) {
            Some(value) => (Some(value), format!("env {name}")),
            None => match inline_key {
                Some(value) => (Some(value.to_string()), "settings (inline)".into()),
                None => (None, format!("env {name} is empty")),
            },
        };
    }
    if profile_selected && inline_key.is_some() {
        return (inline_key.map(str::to_string), "settings (inline)".into());
    }
    if let Some((name, value)) = env_nonempty("JANI_API_KEY")
        .map(|value| ("JANI_API_KEY", value))
        .or_else(|| env_nonempty("OPENAI_API_KEY").map(|value| ("OPENAI_API_KEY", value)))
    {
        return (Some(value), format!("env {name}"));
    }
    if inline_key.is_some() {
        return (inline_key.map(str::to_string), "settings (inline)".into());
    }
    (None, "no credential configured".into())
}

/// The same rule startup applies to `apiBase`, reused when a session switches to
/// a settings profile at runtime.
pub fn validate_api_base(api_base: &str) -> JResult<()> {
    if !api_base.starts_with("https://")
        && !api_base.starts_with("http://localhost:")
        && !api_base.starts_with("http://127.0.0.1:")
    {
        return Err("API base must be HTTPS or an explicit localhost URL".into());
    }
    if api_base.contains('?') || api_base.contains('#') {
        return Err("API base must not contain query or fragment".into());
    }
    Ok(())
}

impl Config {
    pub fn parse() -> JResult<Self> {
        Self::parse_from(
            env::args().skip(1).collect(),
            std::io::stdin().is_terminal(),
        )
    }

    fn parse_from(mut args: Vec<String>, interactive: bool) -> JResult<Self> {
        // Resolve the bare default before parsing. Setting the command afterwards
        // would take the early help/version path inside `from_args`, silently
        // skipping project settings and environment overrides.
        if args.is_empty() {
            args.push(default_command(interactive).into());
        }
        Self::from_args(&args)
    }

    pub fn baseline() -> Self {
        let root = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            command: "help".into(),
            sub: None,
            positional: Vec::new(),
            prompt: None,
            model: "gpt-5".into(),
            api_base: "https://api.openai.com/v1".into(),
            api_key: None,
            api_key_source: "no credential configured".into(),
            protocol: ModelProtocol::OpenAi,
            protocol_explicit: false,
            max_output_tokens: 4096,
            model_profile: None,
            model_choices: Vec::new(),
            settings_path: None,
            settings_profile: None,
            settings_warnings: Vec::new(),
            permissions: None,
            max_steps: 20,
            root: root.clone(),
            allow_write: false,
            allow_exec: false,
            write_paths: Vec::new(),
            exec_programs: Vec::new(),
            mode: RuntimeMode::LocalUnmanaged,
            tenant: "local".into(),
            principal: "local-operator".into(),
            purpose: "SOFTWARE_DEV".into(),
            contract_path: None,
            policy_path: None,
            policy_keys: Vec::new(),
            identity_path: None,
            state_dir: root.join(".jani"),
            sandbox_mode: SandboxMode::Off,
            sandbox_explicit: false,
            sandbox_net: NetworkPolicy::Deny,
            plugin_dir: None,
            plugin_keys: Vec::new(),
            mcp_contract_path: None,
            mcp_delegation_path: None,
            mcp_input_path: None,
            mcp_response_path: None,
            mcp_token_path: None,
            mcp_agent: "jani".into(),
            mcp_contract_keys: Vec::new(),
            mcp_delegation_keys: Vec::new(),
            mcp_receipt_keys: Vec::new(),
            approval_ids: Vec::new(),
            provider_region: "local".into(),
            provider_class: "openai-compatible".into(),
            action: None,
            resource: None,
            task_id: None,
            run_id: None,
            approve: false,
            reject: false,
            role: None,
        }
    }

    pub fn from_args(args: &[String]) -> JResult<Self> {
        // Parse once for command discovery so --help/--version never depend on a
        // project settings file. A second pass applies CLI values after settings
        // and environment defaults.
        let mut first = Self::baseline();
        Self::parse_args(&mut first, args)?;
        if matches!(first.command.as_str(), "version" | "help") {
            first.api_base = first.api_base.trim_end_matches('/').to_string();
            return Ok(first);
        }

        let loaded = Self::load_project_settings(&first)?;
        if first.model_profile.is_some() && loaded.is_none() {
            return Err("--model-profile requires a settings file with model profiles".into());
        }

        let mut cfg = Self::baseline();
        let mut credential = CredentialBinding::default();
        if let Some((path, settings)) = &loaded {
            cfg.settings_path = Some(path.clone());
            cfg.settings_warnings = settings.warnings();
            cfg.permissions = settings.permissions()?;
            if let Some(steps) = settings.max_steps {
                cfg.max_steps = steps;
            }

            let selected = settings.selected_profile(first.model_profile.as_deref())?;
            let profile = selected
                .as_deref()
                .map(|name| settings.profile(name))
                .transpose()?;
            // An explicit `--model-profile` wins over the top-level selector;
            // otherwise a raw top-level `model` names the model ID directly.
            let model_override = if first.model_profile.is_some() {
                None
            } else {
                settings.model_override()
            };

            if let Some(value) = profile
                .and_then(|profile| profile.protocol.as_deref())
                .or(settings.protocol.as_deref())
            {
                cfg.protocol = ModelProtocol::parse(value)
                    .ok_or_else(|| format!("unknown protocol '{value}'"))?;
                cfg.protocol_explicit = true;
            }
            cfg.api_base = profile
                .and_then(|profile| profile.api_base.clone())
                .or_else(|| settings.api_base.clone())
                .unwrap_or_else(|| cfg.api_base.clone());
            cfg.max_output_tokens = profile
                .and_then(|profile| profile.max_output_tokens)
                .or(settings.max_output_tokens)
                .unwrap_or(cfg.max_output_tokens);

            // A selected profile owns its credential binding. A generic
            // JANI_API_KEY must not silently replace a key written for a
            // different provider endpoint.
            credential = CredentialBinding {
                profile_selected: selected.is_some(),
                env_name: profile
                    .and_then(|profile| profile.api_key_env.clone())
                    .or_else(|| settings.api_key_env.clone()),
                inline_key: profile
                    .and_then(|profile| {
                        profile.api_key.as_ref().map(|key| key.expose().to_string())
                    })
                    .or_else(|| {
                        settings
                            .api_key
                            .as_ref()
                            .map(|key| key.expose().to_string())
                    }),
            };

            if let Some(model) = model_override {
                cfg.model = model.to_string();
            } else if let Some(profile) = profile {
                cfg.model = profile.model.clone();
            }
            cfg.settings_profile = selected;
        }

        Self::apply_env(&mut cfg, &credential)?;
        Self::parse_args(&mut cfg, args)?;
        cfg.settings_path = loaded.as_ref().map(|(path, _)| path.clone());
        Self::finalize(&mut cfg)?;
        if let Some((_, settings)) = &loaded {
            cfg.model_choices = Self::model_choices(&cfg, settings);
        }
        Ok(cfg)
    }

    /// Every settings profile becomes a runtime-selectable model. A profile owns
    /// the fields it sets; unspecified fields inherit the startup value, and a
    /// profile's credential binding is resolved with the same rules as startup so
    /// a generic environment key never silently replaces another provider's key.
    fn model_choices(cfg: &Config, settings: &Settings) -> Vec<ModelChoice> {
        settings
            .models
            .iter()
            .map(|(name, profile)| {
                let api_base = profile
                    .api_base
                    .clone()
                    .or_else(|| settings.api_base.clone())
                    .unwrap_or_else(|| cfg.api_base.clone());
                let protocol = profile
                    .protocol
                    .as_deref()
                    .or(settings.protocol.as_deref())
                    .and_then(ModelProtocol::parse)
                    .unwrap_or_else(|| ModelProtocol::detect(&api_base));
                let max_output_tokens = profile
                    .max_output_tokens
                    .or(settings.max_output_tokens)
                    .unwrap_or(cfg.max_output_tokens);
                let (key, api_key_source) = resolve_credential(
                    true,
                    profile
                        .api_key_env
                        .as_deref()
                        .or(settings.api_key_env.as_deref()),
                    profile
                        .api_key
                        .as_ref()
                        .map(|key| key.expose())
                        .or_else(|| settings.api_key.as_ref().map(|key| key.expose())),
                );
                ModelChoice {
                    label: name.clone(),
                    model: profile.model.clone(),
                    api_base,
                    protocol,
                    max_output_tokens,
                    profile: Some(name.clone()),
                    api_key_source,
                    key,
                }
            })
            .collect()
    }

    fn load_project_settings(cfg: &Config) -> JResult<Option<(PathBuf, Settings)>> {
        let explicit = cfg
            .settings_path
            .clone()
            .or_else(|| env_nonempty("JANI_SETTINGS").map(PathBuf::from));
        Self::resolve_settings(&cfg.root, explicit, Self::user_settings_path())
    }

    fn resolve_settings(
        root: &Path,
        explicit: Option<PathBuf>,
        user: Option<PathBuf>,
    ) -> JResult<Option<(PathBuf, Settings)>> {
        if let Some(path) = explicit {
            let path = if path.is_relative() {
                root.join(path)
            } else {
                path
            };
            if !path.exists() {
                return Err(format!("settings file not found: {}", path.display()));
            }
            let settings = Settings::load(&path)?;
            return Ok(Some((path, settings)));
        }
        let user = user.map(|path| {
            if path.is_relative() {
                root.join(path)
            } else {
                path
            }
        });
        for path in std::iter::once(root.join(".jani").join("settings.json")).chain(user) {
            if path.exists() {
                let settings = Settings::load(&path)?;
                return Ok(Some((path, settings)));
            }
        }
        Ok(None)
    }

    fn user_settings_path() -> Option<PathBuf> {
        if let Some(path) = env_nonempty("JANI_USER_SETTINGS").map(PathBuf::from) {
            return Some(path);
        }
        let home = if cfg!(windows) {
            env::var_os("USERPROFILE").or_else(|| env::var_os("HOME"))
        } else {
            env::var_os("HOME").or_else(|| env::var_os("USERPROFILE"))
        }?;
        Some(PathBuf::from(home).join(".jani").join("settings.json"))
    }

    fn apply_env(cfg: &mut Config, credential: &CredentialBinding) -> JResult<()> {
        if let Some(value) = env_nonempty("JANI_MODEL").or_else(|| env_nonempty("OPENAI_MODEL")) {
            cfg.model = value;
        }
        if let Some(value) = env_nonempty("JANI_API_BASE") {
            cfg.api_base = value;
        }
        if let Some(value) = env_nonempty("JANI_PROTOCOL") {
            cfg.protocol = ModelProtocol::parse(&value)
                .ok_or_else(|| format!("unknown JANI_PROTOCOL: {value}"))?;
            cfg.protocol_explicit = true;
        }
        if let Some(value) = env_nonempty("JANI_MAX_OUTPUT_TOKENS") {
            cfg.max_output_tokens = value
                .parse()
                .map_err(|_| "JANI_MAX_OUTPUT_TOKENS must be a positive integer".to_string())?;
            if !(1..=200_000).contains(&cfg.max_output_tokens) {
                return Err("JANI_MAX_OUTPUT_TOKENS must be 1..200000".into());
            }
        }

        let (key, source) = resolve_credential(
            credential.profile_selected,
            credential.env_name.as_deref(),
            credential.inline_key.as_deref(),
        );
        cfg.api_key = key;
        cfg.api_key_source = source;

        cfg.mode = match env_nonempty("JANI_MODE") {
            Some(value) => {
                RuntimeMode::parse(&value).ok_or_else(|| format!("unknown JANI_MODE: {value}"))?
            }
            None => RuntimeMode::LocalUnmanaged,
        };
        cfg.tenant = env_nonempty("JANI_TENANT").unwrap_or_else(|| "local".into());
        cfg.principal = env_nonempty("JANI_PRINCIPAL").unwrap_or_else(|| "local-operator".into());
        cfg.purpose = env_nonempty("JANI_PURPOSE").unwrap_or_else(|| "SOFTWARE_DEV".into());
        cfg.policy_path = env_nonempty("JANI_POLICY").map(PathBuf::from);
        cfg.policy_keys = env_nonempty("JANI_POLICY_KEYS")
            .map(|v| split_list(&v))
            .unwrap_or_default();
        cfg.identity_path = env_nonempty("JANI_IDENTITY").map(PathBuf::from);
        cfg.contract_path = env_nonempty("JANI_CONTRACT").map(PathBuf::from);
        cfg.state_dir = env_nonempty("JANI_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| cfg.root.join(".jani"));
        cfg.sandbox_mode = match env_nonempty("JANI_SANDBOX") {
            Some(value) => SandboxMode::parse(&value)
                .ok_or_else(|| format!("unknown JANI_SANDBOX: {value}"))?,
            None => SandboxMode::Off,
        };
        cfg.sandbox_explicit = env_nonempty("JANI_SANDBOX").is_some();
        cfg.sandbox_net = match env_nonempty("JANI_SANDBOX_NET") {
            Some(value) => NetworkPolicy::parse(&value)
                .ok_or_else(|| format!("unknown JANI_SANDBOX_NET: {value}"))?,
            None => NetworkPolicy::Deny,
        };
        cfg.plugin_dir = env_nonempty("JANI_PLUGIN_DIR").map(PathBuf::from);
        cfg.plugin_keys = env_nonempty("JANI_PLUGIN_KEYS")
            .map(|v| split_list(&v))
            .unwrap_or_default();
        cfg.provider_region =
            env_nonempty("JANI_PROVIDER_REGION").unwrap_or_else(|| "local".into());
        cfg.provider_class =
            env_nonempty("JANI_PROVIDER_CLASS").unwrap_or_else(|| "openai-compatible".into());
        if let Some(paths) = env_nonempty("JANI_WRITE_PATHS") {
            cfg.write_paths = paths.split(',').map(PathBuf::from).collect();
        }
        if let Some(programs) = env_nonempty("JANI_EXEC_PROGRAMS") {
            cfg.exec_programs = split_list(&programs);
        }
        Ok(())
    }

    fn parse_args(cfg: &mut Config, args: &[String]) -> JResult<()> {
        let mut i = 0;
        let mut command_seen = false;
        while i < args.len() {
            let a = args[i].as_str();
            // Once a command is selected, bare words belong to it: an operation such as
            // `jani native simulator run` must not be re-parsed as the top-level `run`.
            if command_seen && !a.starts_with('-') {
                cfg.positional.push(a.to_string());
                i += 1;
                continue;
            }
            match a {
                "--version" | "version" => {
                    cfg.command = "version".into();
                    command_seen = true;
                }
                "--help" | "-h" | "help" => {
                    cfg.command = "help".into();
                    command_seen = true;
                }
                "doctor" => {
                    cfg.command = "doctor".into();
                    command_seen = true;
                }
                "skills" => {
                    cfg.command = "skills".into();
                    command_seen = true;
                }
                "run" => {
                    cfg.command = "run".into();
                    command_seen = true;
                    if let Some(next) = args.get(i + 1) {
                        if !next.starts_with("--") {
                            cfg.prompt = Some(take_value(args, &mut i, "run")?);
                        }
                    }
                }
                "--print" => {
                    cfg.command = "run".into();
                    command_seen = true;
                    let value = take_value(args, &mut i, a)?;
                    if value.starts_with("--") {
                        return Err("--print requires a prompt, not an option".into());
                    }
                    cfg.prompt = Some(value);
                }
                "repl" => {
                    cfg.command = "repl".into();
                    command_seen = true;
                }
                "sandbox" | "plugins" | "policy" | "audit" | "approvals" | "budget"
                | "identity" | "contract" | "agent" | "alerts" | "wake" | "mirror" | "mcp"
                | "native" => {
                    cfg.command = a.into();
                    command_seen = true;
                    if let Some(next) = args.get(i + 1) {
                        if !next.starts_with('-') {
                            cfg.sub = Some(take_value(args, &mut i, a)?);
                        }
                    }
                }
                "inspect" => {
                    cfg.command = "inspect".into();
                    command_seen = true;
                    if let Some(next) = args.get(i + 1) {
                        if !next.starts_with('-') {
                            cfg.positional.push(take_value(args, &mut i, "inspect")?);
                        }
                    }
                }
                "--mode" => {
                    let value = take_value(args, &mut i, a)?;
                    cfg.mode = RuntimeMode::parse(&value)
                        .ok_or_else(|| format!("unknown mode: {value}"))?;
                }
                "--tenant" => cfg.tenant = take_value(args, &mut i, a)?,
                "--principal" => cfg.principal = take_value(args, &mut i, a)?,
                "--purpose" => cfg.purpose = take_value(args, &mut i, a)?,
                "--contract" => {
                    cfg.contract_path = Some(PathBuf::from(take_value(args, &mut i, a)?))
                }
                "--policy" => cfg.policy_path = Some(PathBuf::from(take_value(args, &mut i, a)?)),
                "--policy-key" => cfg.policy_keys.push(take_value(args, &mut i, a)?),
                "--identity" => {
                    cfg.identity_path = Some(PathBuf::from(take_value(args, &mut i, a)?))
                }
                "--state-dir" => cfg.state_dir = PathBuf::from(take_value(args, &mut i, a)?),
                "--sandbox" => {
                    let value = take_value(args, &mut i, a)?;
                    cfg.sandbox_mode = SandboxMode::parse(&value)
                        .ok_or_else(|| format!("unknown sandbox: {value}"))?;
                    cfg.sandbox_explicit = true;
                }
                "--sandbox-net" => {
                    let value = take_value(args, &mut i, a)?;
                    cfg.sandbox_net = NetworkPolicy::parse(&value)
                        .ok_or_else(|| format!("unknown network policy: {value}"))?;
                }
                "--plugin-dir" => {
                    cfg.plugin_dir = Some(PathBuf::from(take_value(args, &mut i, a)?))
                }
                "--plugin-key" => cfg.plugin_keys.push(take_value(args, &mut i, a)?),
                "--mcp-contract" => {
                    cfg.mcp_contract_path = Some(PathBuf::from(take_value(args, &mut i, a)?))
                }
                "--mcp-delegation" => {
                    cfg.mcp_delegation_path = Some(PathBuf::from(take_value(args, &mut i, a)?))
                }
                "--mcp-arguments" => {
                    cfg.mcp_input_path = Some(PathBuf::from(take_value(args, &mut i, a)?))
                }
                "--mcp-response" => {
                    cfg.mcp_response_path = Some(PathBuf::from(take_value(args, &mut i, a)?))
                }
                "--mcp-token-file" => {
                    cfg.mcp_token_path = Some(PathBuf::from(take_value(args, &mut i, a)?))
                }
                "--mcp-agent" => cfg.mcp_agent = take_value(args, &mut i, a)?,
                "--mcp-contract-key" => cfg.mcp_contract_keys.push(take_value(args, &mut i, a)?),
                "--mcp-delegation-key" => {
                    cfg.mcp_delegation_keys.push(take_value(args, &mut i, a)?)
                }
                "--mcp-receipt-key" => cfg.mcp_receipt_keys.push(take_value(args, &mut i, a)?),
                "--approval" => cfg.approval_ids.push(take_value(args, &mut i, a)?),
                "--provider-region" => cfg.provider_region = take_value(args, &mut i, a)?,
                "--provider-class" => cfg.provider_class = take_value(args, &mut i, a)?,
                "--model" => cfg.model = take_value(args, &mut i, a)?,
                "--api-base" => cfg.api_base = take_value(args, &mut i, a)?,
                "--protocol" => {
                    let value = take_value(args, &mut i, a)?;
                    cfg.protocol = ModelProtocol::parse(&value)
                        .ok_or_else(|| format!("unknown protocol: {value}"))?;
                    cfg.protocol_explicit = true;
                }
                "--max-output-tokens" => {
                    cfg.max_output_tokens = take_value(args, &mut i, a)?.parse().map_err(|_| {
                        "--max-output-tokens must be a positive integer".to_string()
                    })?;
                    if !(1..=200_000).contains(&cfg.max_output_tokens) {
                        return Err("--max-output-tokens must be 1..200000".into());
                    }
                }
                "--settings" => {
                    cfg.settings_path = Some(PathBuf::from(take_value(args, &mut i, a)?))
                }
                "--model-profile" => cfg.model_profile = Some(take_value(args, &mut i, a)?),
                "--repo" => cfg.root = PathBuf::from(take_value(args, &mut i, a)?),
                "--max-steps" => {
                    cfg.max_steps = take_value(args, &mut i, a)?
                        .parse()
                        .map_err(|_| "--max-steps must be a positive integer".to_string())?;
                    if !(1..=80).contains(&cfg.max_steps) {
                        return Err("--max-steps must be 1..80".into());
                    }
                }
                "--allow-write" => cfg.allow_write = true,
                "--allow-exec" => cfg.allow_exec = true,
                "--write-path" => cfg
                    .write_paths
                    .push(PathBuf::from(take_value(args, &mut i, a)?)),
                "--exec-program" => cfg.exec_programs.push(take_value(args, &mut i, a)?),
                "--action" => cfg.action = Some(take_value(args, &mut i, a)?),
                "--resource" => cfg.resource = Some(take_value(args, &mut i, a)?),
                "--run" => cfg.run_id = Some(take_value(args, &mut i, a)?),
                "--task" => cfg.task_id = Some(take_value(args, &mut i, a)?),
                "--approve" => cfg.approve = true,
                "--reject" => cfg.reject = true,
                "--role" => cfg.role = Some(take_value(args, &mut i, a)?),
                other if other.starts_with('-') => {
                    return Err(format!("unknown option: {other}"));
                }
                other => cfg.positional.push(other.to_string()),
            }
            i += 1;
        }

        if !command_seen && !cfg.positional.is_empty() {
            return Err(format!("unknown command: {}", cfg.positional[0]));
        }
        Ok(())
    }

    fn finalize(cfg: &mut Config) -> JResult<()> {
        cfg.root = cfg
            .root
            .canonicalize()
            .map_err(|e| format!("invalid worktree {}: {e}", cfg.root.display()))?;
        if !cfg.root.is_dir() {
            return Err("--repo is not a directory".into());
        }
        if cfg.state_dir.is_relative() {
            cfg.state_dir = cfg.root.join(&cfg.state_dir);
        }
        if cfg.command == "run" && cfg.prompt.as_ref().is_none_or(|p| p.trim().is_empty()) {
            return Err("run requires a non-empty prompt".into());
        }
        if cfg.command == "repl" && cfg.run_id.is_some() {
            return Err("--run is not supported with repl; each turn gets its own run id".into());
        }
        if cfg.command == "repl" && !cfg.positional.is_empty() {
            return Err("repl takes no prompt argument; submit prompts inside the session".into());
        }
        for p in &cfg.write_paths {
            validate_relative(p)?;
        }
        cfg.write_paths.dedup();
        for program in &cfg.exec_programs {
            if !matches!(
                program.as_str(),
                "pytest" | "python" | "python3" | "node" | "go" | "dotnet" | "npm"
            ) {
                return Err(format!("unsupported opt-in test runner {program}"));
            }
        }
        if cfg.allow_write && cfg.write_paths.is_empty() {
            return Err("--allow-write requires --write-path or JANI_WRITE_PATHS; no implicit full-repository editing".into());
        }
        validate_api_base(&cfg.api_base)?;
        cfg.api_base = cfg.api_base.trim_end_matches('/').to_string();
        if !cfg.protocol_explicit {
            cfg.protocol = ModelProtocol::detect(&cfg.api_base);
        }

        if cfg.mode == RuntimeMode::Enterprise {
            if cfg.policy_path.is_none() {
                return Err("enterprise mode requires a signed --policy bundle".into());
            }
            if cfg.policy_keys.is_empty() {
                return Err("enterprise mode requires --policy-key trust roots".into());
            }
            if cfg.sandbox_net != NetworkPolicy::Deny {
                return Err("enterprise mode cannot weaken network policy from the CLI".into());
            }
            if (cfg.command == "run" || cfg.command == "repl") && cfg.identity_path.is_none() {
                return Err("enterprise runs require a signed --identity assertion".into());
            }
            if cfg.contract_path.is_none() && (cfg.command == "run" || cfg.command == "repl") {
                return Err(
                    "enterprise runs require --contract for a bounded work contract".into(),
                );
            }
            if cfg.sandbox_mode == SandboxMode::Off {
                // Reads remain available; the broker refuses code execution without isolation.
            }
        } else if cfg.sandbox_mode == SandboxMode::Off {
            cfg.sandbox_mode = SandboxMode::Off;
        }
        if cfg.command == "run" && cfg.mode == RuntimeMode::LocalUnmanaged {
            // Local runs keep the pre-runtime defaults; sandbox is opt-in.
        }

        Ok(())
    }

    pub fn is_enterprise(&self) -> bool {
        self.mode == RuntimeMode::Enterprise
    }
}

pub fn protected_component(component: &str) -> bool {
    let s = component.to_ascii_lowercase();
    matches!(
        s.as_str(),
        ".git"
            | ".agents"
            | ".ssh"
            | ".env"
            | ".netrc"
            | ".npmrc"
            | ".git-credentials"
            | ".aws"
            | ".azure"
            | "id_rsa"
            | "id_dsa"
            | "id_ecdsa"
            | "id_ed25519"
            | "credentials"
            | "credentials.txt"
    ) || s.starts_with(".env.")
        || s.ends_with(".pem")
        || s.ends_with(".key")
        || s.ends_with(".p12")
        || s.ends_with(".pfx")
        || s.ends_with(".keystore")
        || s.ends_with(".jks")
        || s.contains("private_key")
        || s.contains("secret")
}

pub fn validate_relative(p: &Path) -> JResult<()> {
    use std::path::Component;
    if p.as_os_str().is_empty() || p.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!(
            "relative path with no traversal required: {}",
            p.display()
        ));
    }
    for part in p.components() {
        let s = part.as_os_str().to_string_lossy().to_ascii_lowercase();
        if protected_component(&s) {
            return Err(format!("protected path component: {s}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    // Unit tests must never read the developer's real user-wide settings.
    fn isolate() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            std::env::set_var(
                "JANI_USER_SETTINGS",
                std::env::temp_dir().join(format!(
                    "jani-config-no-user-settings-{}.json",
                    std::process::id()
                )),
            );
        });
    }

    #[test]
    fn traversal_is_rejected() {
        assert!(validate_relative(Path::new("../secrets")).is_err());
        assert!(validate_relative(Path::new(".git/config")).is_err());
        assert!(validate_relative(Path::new("src/main.rs")).is_ok());
    }

    #[test]
    fn subcommand_operands_are_not_reparsed_as_commands() {
        let args: Vec<String> = ["native", "simulator", "run", "42", "--tenant", "team-1"]
            .iter()
            .map(|value| value.to_string())
            .collect();
        isolate();
        let cfg = Config::from_args(&args).unwrap();
        assert_eq!(cfg.command, "native");
        assert_eq!(cfg.sub.as_deref(), Some("simulator"));
        assert_eq!(cfg.positional, vec!["run", "42"]);
        assert_eq!(cfg.tenant, "team-1");
    }

    #[test]
    fn unknown_command_is_rejected() {
        isolate();
        let result = Config::from_args(&["unsupported-command".to_string()]);
        assert!(matches!(&result, Err(message) if message.contains("unknown command")));
    }

    #[test]
    fn bare_invocation_selects_the_terminal_or_help() {
        isolate();
        assert_eq!(default_command(true), "repl");
        assert_eq!(default_command(false), "help");

        let interactive = Config::parse_from(Vec::new(), true).unwrap();
        assert_eq!(interactive.command, "repl");
        assert!(interactive.positional.is_empty());

        let piped = Config::parse_from(Vec::new(), false).unwrap();
        assert_eq!(piped.command, "help");

        let explicit = Config::parse_from(vec!["--help".into()], true).unwrap();
        assert_eq!(explicit.command, "help");
    }

    #[test]
    fn repl_is_a_command_without_a_prompt() {
        isolate();
        let cfg = Config::from_args(&["repl".to_string()]).unwrap();
        assert_eq!(cfg.command, "repl");
        assert!(cfg.prompt.is_none());
    }

    #[test]
    fn repl_rejects_a_fixed_run_id() {
        let args: Vec<String> = ["repl", "--run", "run-fixed"]
            .iter()
            .map(|value| value.to_string())
            .collect();
        isolate();
        let error = match Config::from_args(&args) {
            Ok(_) => panic!("expected repl to reject a fixed run id"),
            Err(error) => error,
        };
        assert!(
            error.contains("--run is not supported with repl"),
            "{error}"
        );
    }

    #[test]
    fn repl_rejects_a_positional_prompt() {
        let args: Vec<String> = ["repl", "read", "src/one.txt"]
            .iter()
            .map(|value| value.to_string())
            .collect();
        isolate();
        let error = match Config::from_args(&args) {
            Ok(_) => panic!("expected repl to reject a positional prompt"),
            Err(error) => error,
        };
        assert!(error.contains("repl takes no prompt"), "{error}");
    }

    #[test]
    fn enterprise_repl_requires_identity_and_contract() {
        let base: Vec<String> = [
            "repl",
            "--mode",
            "enterprise",
            "--policy",
            "policy.json",
            "--policy-key",
            "test-key",
        ]
        .iter()
        .map(|value| value.to_string())
        .collect();
        isolate();
        let error = match Config::from_args(&base) {
            Ok(_) => panic!("expected enterprise repl to require an identity"),
            Err(error) => error,
        };
        assert!(error.contains("--identity"), "{error}");
    }

    #[test]
    fn protected_components() {
        assert!(protected_component(".env"));
        assert!(protected_component(".env.production"));
        assert!(protected_component("id_rsa"));
        assert!(protected_component("server.key"));
        assert!(protected_component("bundle.p12"));
        assert!(protected_component(".netrc"));
        assert!(protected_component("my_secret_notes.md"));
        assert!(!protected_component("src"));
        assert!(!protected_component("main.rs"));
        assert!(!protected_component("keyboard.rs"));
    }

    const SETTINGS: &str = r#"{
        "version": 1,
        "defaultModel": "fast",
        "models": {
            "fast": {"model": "profile-model", "apiBase": "https://settings.test/v1"},
            "deep": {"model": "deep-model", "apiBase": "https://settings.test/v1"}
        },
        "maxSteps": 30
    }"#;

    fn temp_workspace(settings: Option<&str>) -> PathBuf {
        isolate();
        let root = std::env::temp_dir().join(format!(
            "jani-config-settings-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join(".jani")).unwrap();
        if let Some(body) = settings {
            std::fs::write(root.join(".jani").join("settings.json"), body).unwrap();
        }
        root
    }

    fn args_for(root: &Path, extra: &[&str]) -> Vec<String> {
        let mut args = vec!["doctor".to_string(), "--repo".into()];
        args.push(root.to_str().unwrap().into());
        args.extend(extra.iter().map(|value| value.to_string()));
        args
    }

    #[test]
    fn settings_profile_supplies_model_defaults() {
        let root = temp_workspace(Some(SETTINGS));
        let cfg = Config::from_args(&args_for(&root, &[])).unwrap();
        assert_eq!(cfg.model, "profile-model");
        assert_eq!(cfg.api_base, "https://settings.test/v1");
        assert_eq!(cfg.max_steps, 30);
        assert_eq!(cfg.settings_profile.as_deref(), Some("fast"));
        assert_eq!(
            cfg.settings_path.as_ref().unwrap().file_name().unwrap(),
            "settings.json"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn settings_profiles_become_runtime_model_choices() {
        let root = temp_workspace(Some(
            r#"{
                "version": 1,
                "model": "fast",
                "apiBase": "https://shared.test/v1",
                "maxOutputTokens": 4096,
                "models": {
                    "fast": {"model": "fast-model"},
                    "deep": {
                        "model": "deep-model",
                        "apiBase": "https://api.anthropic.com",
                        "protocol": "anthropic",
                        "maxOutputTokens": 16000,
                        "apiKey": "unit-test-deep-key"
                    }
                }
            }"#,
        ));
        let cfg = Config::from_args(&args_for(&root, &[])).unwrap();
        assert_eq!(cfg.model_choices.len(), 2);
        let fast = cfg
            .model_choices
            .iter()
            .find(|choice| choice.label == "fast")
            .unwrap();
        assert_eq!(fast.model, "fast-model");
        assert_eq!(fast.api_base, "https://shared.test/v1");
        assert_eq!(fast.protocol, ModelProtocol::OpenAi);
        assert_eq!(fast.max_output_tokens, 4096);
        assert_eq!(fast.profile.as_deref(), Some("fast"));

        let deep = cfg
            .model_choices
            .iter()
            .find(|choice| choice.label == "deep")
            .unwrap();
        assert_eq!(deep.model, "deep-model");
        assert_eq!(deep.api_base, "https://api.anthropic.com");
        assert_eq!(deep.protocol, ModelProtocol::Anthropic);
        assert_eq!(deep.max_output_tokens, 16000);
        assert_eq!(deep.api_key(), Some("unit-test-deep-key"));
        assert!(!format!("{deep:?}").contains("unit-test-deep-key"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_model_flags_override_settings() {
        let root = temp_workspace(Some(SETTINGS));
        let cfg = Config::from_args(&args_for(
            &root,
            &[
                "--model",
                "cli-model",
                "--api-base",
                "https://cli.test/v1",
                "--max-steps",
                "12",
            ],
        ))
        .unwrap();
        assert_eq!(cfg.model, "cli-model");
        assert_eq!(cfg.api_base, "https://cli.test/v1");
        assert_eq!(cfg.max_steps, 12);
        assert_eq!(cfg.settings_profile.as_deref(), Some("fast"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn model_profile_flag_selects_a_named_profile() {
        let root = temp_workspace(Some(SETTINGS));
        let cfg = Config::from_args(&args_for(&root, &["--model-profile", "deep"])).unwrap();
        assert_eq!(cfg.model, "deep-model");
        assert_eq!(cfg.settings_profile.as_deref(), Some("deep"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn model_profile_requires_settings() {
        let root = temp_workspace(None);
        let error = match Config::from_args(&args_for(&root, &["--model-profile", "fast"])) {
            Ok(_) => panic!("expected --model-profile to fail without settings"),
            Err(error) => error,
        };
        assert!(error.contains("requires a settings file"), "{error}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn broken_settings_fail_closed_but_help_still_works() {
        let root = temp_workspace(Some("{not json"));
        let error = match Config::from_args(&args_for(&root, &[])) {
            Ok(_) => panic!("expected invalid settings to fail"),
            Err(error) => error,
        };
        assert!(error.contains("invalid settings JSON"), "{error}");
        let help = Config::from_args(&args_for(&root, &["--help"])).unwrap();
        assert_eq!(help.command, "help");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_settings_path_is_used() {
        let root = temp_workspace(None);
        std::fs::write(root.join("custom.json"), SETTINGS).unwrap();
        let cfg = Config::from_args(&args_for(&root, &["--settings", "custom.json"])).unwrap();
        assert_eq!(cfg.model, "profile-model");
        assert_eq!(
            cfg.settings_path.as_ref().unwrap().file_name().unwrap(),
            "custom.json"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    const USER_SETTINGS: &str = r#"{
        "version": 1,
        "model": "user-profile",
        "models": {
            "user-profile": {"model": "user-model", "apiBase": "https://user.test/v1"}
        }
    }"#;

    fn temp_settings_file(name: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "jani-config-{name}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn user_settings_are_used_when_the_worktree_has_none() {
        let root = temp_workspace(None);
        let user = temp_settings_file("user", USER_SETTINGS);
        let (path, settings) = Config::resolve_settings(&root, None, Some(user.clone()))
            .unwrap()
            .expect("user settings should be found");
        assert_eq!(path, user);
        assert_eq!(
            settings.selected_profile(None).unwrap().as_deref(),
            Some("user-profile")
        );
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_file(user).unwrap();
    }

    #[test]
    fn project_settings_take_precedence_over_user_settings() {
        let root = temp_workspace(Some(SETTINGS));
        let user = temp_settings_file("shadowed-user", USER_SETTINGS);
        let (path, settings) = Config::resolve_settings(&root, None, Some(user.clone()))
            .unwrap()
            .expect("project settings should be found");
        assert_eq!(path, root.join(".jani").join("settings.json"));
        assert_eq!(
            settings.selected_profile(None).unwrap().as_deref(),
            Some("fast")
        );
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_file(user).unwrap();
    }

    #[test]
    fn explicit_settings_take_precedence_over_project_and_user() {
        let root = temp_workspace(Some(SETTINGS));
        let explicit = temp_settings_file("explicit", r#"{"version":1,"model":"explicit-model"}"#);
        let user = temp_settings_file("ignored-user", USER_SETTINGS);
        let (path, settings) =
            Config::resolve_settings(&root, Some(explicit.clone()), Some(user.clone()))
                .unwrap()
                .expect("explicit settings should be found");
        assert_eq!(path, explicit);
        assert_eq!(settings.model_override(), Some("explicit-model"));
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_file(explicit).unwrap();
        std::fs::remove_file(user).unwrap();
    }

    #[test]
    fn missing_explicit_settings_fail_even_with_user_settings() {
        let root = temp_workspace(None);
        let user = temp_settings_file("fallback", USER_SETTINGS);
        let missing = root.join("missing.json");
        let error = match Config::resolve_settings(&root, Some(missing), Some(user.clone())) {
            Ok(_) => panic!("expected explicit settings to fail closed"),
            Err(error) => error,
        };
        assert!(error.contains("settings file not found"), "{error}");
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_file(user).unwrap();
    }

    #[test]
    fn broken_user_settings_fail_closed() {
        let root = temp_workspace(None);
        let user = temp_settings_file("broken-user", "{not json");
        let error = match Config::resolve_settings(&root, None, Some(user.clone())) {
            Ok(_) => panic!("expected invalid user settings to fail closed"),
            Err(error) => error,
        };
        assert!(error.contains("invalid settings JSON"), "{error}");
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_file(user).unwrap();
    }

    #[test]
    fn user_settings_path_honors_the_environment_override() {
        isolate();
        let expected = std::env::var("JANI_USER_SETTINGS").unwrap();
        assert_eq!(Config::user_settings_path(), Some(PathBuf::from(expected)));
    }
}

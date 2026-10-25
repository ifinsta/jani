use crate::types::{NetworkPolicy, RuntimeMode, SandboxMode};
use std::env;
use std::path::{Path, PathBuf};

pub type JResult<T> = Result<T, String>;

#[derive(Clone)]
pub struct Config {
    pub command: String,
    pub sub: Option<String>,
    pub positional: Vec<String>,
    pub prompt: Option<String>,
    pub model: String,
    pub api_base: String,
    pub api_key: Option<String>,
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
  jani --version
  jani --help
  jani doctor [OPTIONS]
  jani skills [OPTIONS]
  jani run "TASK" [OPTIONS]
  jani --print "TASK" [OPTIONS]
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
  --model MODEL           OpenAI-compatible tool-call-capable model (JANI_MODEL)
  --api-base URL          API base ending in /v1 (JANI_API_BASE)
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
  JANI_WRITE_PATHS                  Comma-separated relative edit scope
  JANI_EXEC_PROGRAMS                Optional extra test runners
  JANI_SKILL_DIRS                   Extra skill directories, path-list separated
  JANI_MODE, JANI_TENANT, JANI_PRINCIPAL, JANI_PURPOSE
  JANI_POLICY, JANI_POLICY_KEYS, JANI_IDENTITY, JANI_CONTRACT
  JANI_STATE_DIR, JANI_SANDBOX, JANI_SANDBOX_NET
  JANI_PLUGIN_DIR, JANI_PLUGIN_KEYS, JANI_PROVIDER_REGION, JANI_PROVIDER_CLASS

Skills are advisory instructions, not an executable security policy.
Tool flags are enforced in Rust. Execution is NOT an OS sandbox unless a
verified backend is selected and attested.
"#
}

impl Config {
    pub fn parse() -> JResult<Self> {
        Self::from_args(&env::args().skip(1).collect::<Vec<_>>())
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
        let mut cfg = Self::baseline();
        cfg.model = env_nonempty("JANI_MODEL")
            .or_else(|| env_nonempty("OPENAI_MODEL"))
            .unwrap_or_else(|| "gpt-5".to_string());
        cfg.api_base = env_nonempty("JANI_API_BASE")
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        cfg.api_key = env_nonempty("JANI_API_KEY").or_else(|| env_nonempty("OPENAI_API_KEY"));
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

        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            match a {
                "--version" | "version" => cfg.command = "version".into(),
                "--help" | "-h" | "help" => cfg.command = "help".into(),
                "doctor" => cfg.command = "doctor".into(),
                "skills" => cfg.command = "skills".into(),
                "run" => {
                    cfg.command = "run".into();
                    if let Some(next) = args.get(i + 1) {
                        if !next.starts_with("--") {
                            cfg.prompt = Some(take_value(args, &mut i, "run")?);
                        }
                    }
                }
                "--print" => {
                    cfg.command = "run".into();
                    let value = take_value(args, &mut i, a)?;
                    if value.starts_with("--") {
                        return Err("--print requires a prompt, not an option".into());
                    }
                    cfg.prompt = Some(value);
                }
                "sandbox" | "plugins" | "policy" | "audit" | "approvals" | "budget"
                | "identity" | "contract" | "agent" | "alerts" | "wake" | "mirror" | "mcp" => {
                    cfg.command = a.into();
                    if let Some(next) = args.get(i + 1) {
                        if !next.starts_with('-') {
                            cfg.sub = Some(take_value(args, &mut i, a)?);
                        }
                    }
                }
                "inspect" => {
                    cfg.command = "inspect".into();
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
                "--mcp-contract" => cfg.mcp_contract_path = Some(PathBuf::from(take_value(args, &mut i, a)?)),
                "--mcp-delegation" => cfg.mcp_delegation_path = Some(PathBuf::from(take_value(args, &mut i, a)?)),
                "--mcp-arguments" => cfg.mcp_input_path = Some(PathBuf::from(take_value(args, &mut i, a)?)),
                "--mcp-response" => cfg.mcp_response_path = Some(PathBuf::from(take_value(args, &mut i, a)?)),
                "--mcp-token-file" => cfg.mcp_token_path = Some(PathBuf::from(take_value(args, &mut i, a)?)),
                "--mcp-agent" => cfg.mcp_agent = take_value(args, &mut i, a)?,
                "--mcp-contract-key" => cfg.mcp_contract_keys.push(take_value(args, &mut i, a)?),
                "--mcp-delegation-key" => cfg.mcp_delegation_keys.push(take_value(args, &mut i, a)?),
                "--mcp-receipt-key" => cfg.mcp_receipt_keys.push(take_value(args, &mut i, a)?),
                "--approval" => cfg.approval_ids.push(take_value(args, &mut i, a)?),
                "--provider-region" => cfg.provider_region = take_value(args, &mut i, a)?,
                "--provider-class" => cfg.provider_class = take_value(args, &mut i, a)?,
                "--model" => cfg.model = take_value(args, &mut i, a)?,
                "--api-base" => cfg.api_base = take_value(args, &mut i, a)?,
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

        if matches!(cfg.command.as_str(), "version" | "help") {
            cfg.api_base = cfg.api_base.trim_end_matches('/').to_string();
            return Ok(cfg);
        }

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
        if !cfg.api_base.starts_with("https://")
            && !cfg.api_base.starts_with("http://localhost:")
            && !cfg.api_base.starts_with("http://127.0.0.1:")
        {
            return Err("API base must be HTTPS or an explicit localhost URL".into());
        }
        if cfg.api_base.contains('?') || cfg.api_base.contains('#') {
            return Err("API base must not contain query or fragment".into());
        }
        cfg.api_base = cfg.api_base.trim_end_matches('/').to_string();

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
            if cfg.command == "run" && cfg.identity_path.is_none() {
                return Err("enterprise runs require a signed --identity assertion".into());
            }
            if cfg.contract_path.is_none() && cfg.command == "run" {
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

        Ok(cfg)
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

    #[test]
    fn traversal_is_rejected() {
        assert!(validate_relative(Path::new("../secrets")).is_err());
        assert!(validate_relative(Path::new(".git/config")).is_err());
        assert!(validate_relative(Path::new("src/main.rs")).is_ok());
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
}

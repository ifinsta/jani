use crate::config::Config;
use crate::crypto;
use crate::schema::glob_match;
use crate::types::*;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicySignature {
    #[serde(default)]
    pub key_id: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyMetadata {
    pub organization: String,
    pub revision: String,
    #[serde(default)]
    pub issuer: String,
    #[serde(default)]
    pub tenant: Option<String>,
    #[serde(default)]
    pub issued_at: Option<u64>,
    #[serde(default)]
    pub expires_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyDefaults {
    #[serde(default = "deny_default")]
    pub action: String,
    #[serde(default = "restricted_profile_default")]
    pub sandbox_profile: SandboxProfile,
    #[serde(default = "deny_network_default")]
    pub network: NetworkPolicy,
    #[serde(default = "deny_default")]
    pub data_egress: String,
    #[serde(default = "required_default")]
    pub audit: String,
}

fn deny_default() -> String {
    "deny".into()
}
fn required_default() -> String {
    "required".into()
}
fn restricted_profile_default() -> SandboxProfile {
    SandboxProfile::EnterpriseRestricted
}
fn deny_network_default() -> NetworkPolicy {
    NetworkPolicy::Deny
}

impl Default for PolicyDefaults {
    fn default() -> Self {
        Self {
            action: deny_default(),
            sandbox_profile: restricted_profile_default(),
            network: deny_network_default(),
            data_egress: deny_default(),
            audit: required_default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRule {
    pub action: String,
    #[serde(default = "any_resource")]
    pub resource: String,
    #[serde(default)]
    pub max_risk: Option<RiskTier>,
    #[serde(default)]
    pub effect: Option<Effect>,
    #[serde(default)]
    pub approval: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub sandbox_profile: Option<SandboxProfile>,
}

fn any_resource() -> String {
    "*".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataPolicySpec {
    #[serde(default = "default_source_class")]
    pub source_default: Classification,
    #[serde(default = "default_model_classes")]
    pub allowed_model_classes: Vec<Classification>,
    #[serde(default)]
    pub allowed_providers: Vec<String>,
    #[serde(default)]
    pub allowed_regions: Vec<String>,
    #[serde(default = "default_purposes")]
    pub allowed_purposes: Vec<String>,
    #[serde(default = "true_default")]
    pub redact_secrets: bool,
    #[serde(default)]
    pub allow_restricted_egress: bool,
    #[serde(default = "true_default")]
    pub inspect_outputs: bool,
}

fn default_source_class() -> Classification {
    Classification::Internal
}
fn default_model_classes() -> Vec<Classification> {
    vec![Classification::Public, Classification::Internal]
}
fn default_purposes() -> Vec<String> {
    vec!["SOFTWARE_DEV".into()]
}
fn true_default() -> bool {
    true
}

impl Default for DataPolicySpec {
    fn default() -> Self {
        Self {
            source_default: default_source_class(),
            allowed_model_classes: default_model_classes(),
            allowed_providers: vec![],
            allowed_regions: vec![],
            allowed_purposes: default_purposes(),
            redact_secrets: true,
            allow_restricted_egress: false,
            inspect_outputs: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PricingSpec {
    #[serde(default)]
    pub usd_per_1k_input_tokens: f64,
    #[serde(default)]
    pub usd_per_1k_output_tokens: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyBudgets {
    #[serde(default)]
    pub max_tool_calls: Option<u32>,
    #[serde(default)]
    pub max_wall_seconds: Option<u64>,
    #[serde(default)]
    pub max_usd: Option<f64>,
    #[serde(default)]
    pub pricing: PricingSpec,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyOutage {
    #[serde(default = "true_default")]
    pub allow_offline_read: bool,
    #[serde(default = "offline_lease_default")]
    pub max_offline_lease_seconds: u64,
    #[serde(default)]
    pub allow_offline_external_mutations: bool,
}

fn offline_lease_default() -> u64 {
    900
}

impl Default for PolicyOutage {
    fn default() -> Self {
        Self {
            allow_offline_read: true,
            max_offline_lease_seconds: offline_lease_default(),
            allow_offline_external_mutations: false,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicySpec {
    #[serde(default)]
    pub defaults: PolicyDefaults,
    #[serde(default)]
    pub rules: Vec<PolicyRule>,
    #[serde(default)]
    pub denied_actions: Vec<String>,
    #[serde(default)]
    pub data: DataPolicySpec,
    #[serde(default)]
    pub budgets: PolicyBudgets,
    #[serde(default)]
    pub policy_outage: PolicyOutage,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyBundle {
    pub api_version: String,
    pub kind: String,
    pub metadata: PolicyMetadata,
    pub spec: PolicySpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<PolicySignature>,
}

impl PolicyBundle {
    pub fn signing_bytes(&self) -> Result<Vec<u8>, String> {
        let mut clone = self.clone();
        clone.signature = None;
        serde_json::to_vec(&clone).map_err(|e| e.to_string())
    }

    #[cfg(test)]
    pub fn sign(&mut self, secret_hex: &str, key_id: &str) -> Result<(), String> {
        let bytes = self.signing_bytes()?;
        self.signature = Some(PolicySignature {
            key_id: key_id.to_string(),
            value: crypto::sign_hex(secret_hex, &bytes)?,
        });
        Ok(())
    }

    pub fn verify(&self, trusted_keys: &[String]) -> Result<(), String> {
        let signature = self.signature.as_ref().ok_or("policy bundle is unsigned")?;
        crypto::verify_signature(
            trusted_keys,
            &signature.key_id,
            &self.signing_bytes()?,
            &signature.value,
        )
    }

    pub fn load(path: &Path, trusted_keys: &[String]) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read policy {}: {e}", path.display()))?;
        let bundle: Self =
            serde_json::from_str(&text).map_err(|e| format!("invalid policy JSON: {e}"))?;
        if bundle.api_version != "jani.dev/v1alpha1" {
            return Err("unsupported policy apiVersion; expected jani.dev/v1alpha1".into());
        }
        if bundle.kind != "OrganizationPolicy" {
            return Err("policy kind must be OrganizationPolicy".into());
        }
        if bundle.metadata.revision.trim().is_empty() {
            return Err("policy revision is required".into());
        }
        if bundle.spec.defaults.action != "deny" {
            return Err("policy defaults.action must remain deny".into());
        }
        if let Some(rule) = bundle.spec.rules.iter().find(|rule| !rule.roles.is_empty()) {
            return Err(format!(
                "policy rule '{}' scopes by roles, but identity assertions carry no roles",
                rule.action
            ));
        }
        if let Some(expires) = bundle.metadata.expires_at {
            if expires <= now_unix() {
                return Err("policy bundle has expired".into());
            }
        }
        bundle.verify(trusted_keys)?;
        Ok(bundle)
    }
}

#[derive(Clone, Debug)]
pub struct EffectivePolicy {
    pub bundle: Option<PolicyBundle>,
    pub revision: String,
    pub tenant: Option<String>,
    pub enterprise: bool,
}

#[derive(Clone, Debug)]
pub struct PolicyEngine {
    pub effective: EffectivePolicy,
}

impl PolicyEngine {
    pub fn load(cfg: &Config) -> Result<Self, String> {
        if let Some(path) = cfg.policy_path.as_ref() {
            let bundle = PolicyBundle::load(path, &cfg.policy_keys)?;
            if let Some(tenant) = bundle.metadata.tenant.as_ref() {
                if tenant != &cfg.tenant {
                    return Err(format!(
                        "policy is bound to tenant '{tenant}', not '{}'",
                        cfg.tenant
                    ));
                }
            }
            Ok(Self {
                effective: EffectivePolicy {
                    revision: bundle.metadata.revision.clone(),
                    tenant: bundle.metadata.tenant.clone(),
                    bundle: Some(bundle),
                    enterprise: cfg.is_enterprise(),
                },
            })
        } else if cfg.is_enterprise() {
            Err("enterprise mode requires a signed policy bundle".into())
        } else {
            Ok(Self {
                effective: EffectivePolicy {
                    bundle: None,
                    revision: "local-baseline".into(),
                    tenant: None,
                    enterprise: false,
                },
            })
        }
    }

    pub fn data_policy(&self) -> DataPolicySpec {
        self.effective
            .bundle
            .as_ref()
            .map(|b| b.spec.data.clone())
            .unwrap_or_else(|| DataPolicySpec {
                source_default: Classification::Internal,
                allowed_model_classes: vec![
                    Classification::Public,
                    Classification::Internal,
                    Classification::Confidential,
                ],
                allowed_providers: vec!["*".into()],
                allowed_regions: vec!["*".into()],
                allowed_purposes: vec!["SOFTWARE_DEV".into()],
                redact_secrets: true,
                allow_restricted_egress: false,
                inspect_outputs: true,
            })
    }

    pub fn default_sandbox_profile(&self) -> SandboxProfile {
        self.effective
            .bundle
            .as_ref()
            .map(|b| b.spec.defaults.sandbox_profile)
            .unwrap_or(SandboxProfile::TrustedDev)
    }

    pub fn default_network(&self) -> NetworkPolicy {
        self.effective
            .bundle
            .as_ref()
            .map(|b| b.spec.defaults.network)
            .unwrap_or(NetworkPolicy::Deny)
    }

    pub fn budget_limits(&self) -> PolicyBudgets {
        self.effective
            .bundle
            .as_ref()
            .map(|b| b.spec.budgets.clone())
            .unwrap_or_default()
    }

    pub fn evaluate(&self, cfg: &Config, request: &ActionRequest) -> PolicyDecision {
        let decision_id = new_id("dec");
        let revision = self.effective.revision.clone();

        let deny = |code: &str, reason: &str| PolicyDecision {
            decision: Decision::Deny,
            reason_code: code.into(),
            decision_id: decision_id.clone(),
            policy_revision: revision.clone(),
            constraints: Constraints {
                reason: Some(reason.into()),
                ..Default::default()
            },
        };
        let permit = |code: &str, constraints: Constraints| PolicyDecision {
            decision: Decision::Permit,
            reason_code: code.into(),
            decision_id: decision_id.clone(),
            policy_revision: revision.clone(),
            constraints,
        };

        // Operator flags are a ceiling. A policy can only narrow them.
        match request.effect {
            Effect::LocalWrite | Effect::ExternalWrite | Effect::Irreversible => {
                if !cfg.allow_write && request.effect == Effect::LocalWrite {
                    return deny("operator_write_denied", "file editing is not enabled");
                }
                if request.action.starts_with("repo.write") && !cfg.allow_write {
                    return deny("operator_write_denied", "file editing is not enabled");
                }
            }
            _ => {}
        }
        if request.action == "command.exec" && !cfg.allow_exec {
            return deny("operator_exec_denied", "command execution is not enabled");
        }
        if request.action.starts_with("repo.write") {
            let path = request
                .resource
                .strip_prefix("repo:")
                .unwrap_or(&request.resource);
            if !cfg
                .write_paths
                .iter()
                .any(|scope| std::path::Path::new(path).starts_with(scope))
            {
                return deny(
                    "write_scope_denied",
                    "resource is outside the operator-approved write scope",
                );
            }
        }

        let Some(bundle) = self.effective.bundle.as_ref() else {
            let constraints = Constraints {
                sandbox_profile: Some(if cfg.sandbox_mode == SandboxMode::Off {
                    "trusted-dev".into()
                } else {
                    "development-confined".into()
                }),
                network: Some(cfg.sandbox_net.as_str().into()),
                ..Default::default()
            };
            return match request.effect {
                Effect::Read | Effect::ModelInvoke => permit("local_baseline_read", constraints),
                Effect::LocalWrite | Effect::Control => {
                    permit("local_baseline_operator_grant", constraints)
                }
                Effect::ExternalWrite | Effect::Irreversible => deny(
                    "local_external_write_denied",
                    "external writes require a signed enterprise policy",
                ),
            };
        };

        if bundle
            .spec
            .denied_actions
            .iter()
            .any(|pattern| glob_match(pattern, &request.action))
        {
            return deny(
                "policy_explicit_deny",
                "action is explicitly denied by policy",
            );
        }

        let matching = bundle.spec.rules.iter().find(|rule| {
            let action_ok = glob_match(&rule.action, &request.action);
            let resource_ok = glob_match(&rule.resource, &request.resource)
                || glob_match(
                    &rule.resource,
                    &format!("{}:{}", request.action, request.resource),
                );
            let risk_ok = rule.max_risk.is_none_or(|max| request.risk <= max);
            let effect_ok = rule.effect.is_none_or(|effect| effect == request.effect);
            // Roles are not part of verified identity, so a role-scoped rule can
            // never be satisfied. Treat it as non-matching (default deny).
            let role_ok = rule.roles.is_empty();
            action_ok && resource_ok && risk_ok && effect_ok && role_ok
        });

        let Some(rule) = matching else {
            return deny(
                "policy_default_deny",
                "no signed policy rule permits this action",
            );
        };

        let approval_required = rule.approval.is_some()
            || request.risk >= RiskTier::R3
            || matches!(request.effect, Effect::ExternalWrite | Effect::Irreversible);
        if approval_required {
            return PolicyDecision {
                decision: Decision::RequireApproval,
                reason_code: "approval_required".into(),
                decision_id: decision_id.clone(),
                policy_revision: revision.clone(),
                constraints: Constraints {
                    sandbox_profile: Some(
                        rule.sandbox_profile
                            .unwrap_or(bundle.spec.defaults.sandbox_profile)
                            .as_str()
                            .into(),
                    ),
                    network: Some(bundle.spec.defaults.network.as_str().into()),
                    approval_class: Some(
                        rule.approval
                            .clone()
                            .unwrap_or_else(|| format!("risk-{}", request.risk.as_str())),
                    ),
                    reason: Some("signed approval is required before execution".into()),
                },
            };
        }

        permit(
            "policy_rule_permit",
            Constraints {
                sandbox_profile: Some(
                    rule.sandbox_profile
                        .unwrap_or(bundle.spec.defaults.sandbox_profile)
                        .as_str()
                        .into(),
                ),
                network: Some(bundle.spec.defaults.network.as_str().into()),
                approval_class: None,
                reason: None,
            },
        )
    }

    pub fn describe(&self) -> String {
        match self.effective.bundle.as_ref() {
            None => format!(
                "mode=local-unmanaged revision={} rules=baseline signed=false",
                self.effective.revision
            ),
            Some(bundle) => format!(
                "mode={} organization={} revision={} tenant={} rules={} denied={} signed=true",
                if self.effective.enterprise {
                    "enterprise"
                } else {
                    "local-unmanaged"
                },
                bundle.metadata.organization,
                bundle.metadata.revision,
                bundle.metadata.tenant.clone().unwrap_or_else(|| "*".into()),
                bundle.spec.rules.len(),
                bundle.spec.denied_actions.len()
            ),
        }
    }
}

/// Effective limits after intersecting the signed policy with an optional contract.
#[derive(Clone, Debug, Default)]
pub struct EffectiveLimits {
    pub max_tool_calls: Option<u32>,
    pub max_wall_seconds: Option<u64>,
    pub max_usd: Option<f64>,
    pub usd_per_1k_input_tokens: f64,
    pub usd_per_1k_output_tokens: f64,
    pub sandbox_profile: Option<SandboxProfile>,
    pub write_paths: Vec<String>,
    pub capabilities: Vec<String>,
}

pub fn effective_limits(
    engine: &PolicyEngine,
    contract: Option<&WorkContract>,
) -> Result<EffectiveLimits, String> {
    let budgets = engine.budget_limits();
    let mut limits = EffectiveLimits {
        max_tool_calls: budgets.max_tool_calls,
        max_wall_seconds: budgets.max_wall_seconds,
        max_usd: budgets.max_usd,
        usd_per_1k_input_tokens: budgets.pricing.usd_per_1k_input_tokens,
        usd_per_1k_output_tokens: budgets.pricing.usd_per_1k_output_tokens,
        sandbox_profile: Some(engine.default_sandbox_profile()),
        ..Default::default()
    };
    if let Some(contract) = contract {
        if let Some(v) = contract.spec.budget.max_tool_calls {
            limits.max_tool_calls = Some(limits.max_tool_calls.map_or(v, |x| x.min(v)));
        }
        if let Some(v) = contract.spec.budget.deadline_seconds {
            limits.max_wall_seconds = Some(limits.max_wall_seconds.map_or(v, |x| x.min(v)));
        }
        if let Some(v) = contract.spec.budget.max_usd {
            limits.max_usd = Some(limits.max_usd.map_or(v, |x| x.min(v)));
        }
        if let Some(profile) = contract.spec.sandbox_profile {
            let policy_profile = limits
                .sandbox_profile
                .unwrap_or(engine.default_sandbox_profile());
            if policy_profile.requires_isolation() && !profile.requires_isolation() {
                return Err(
                    "work contract cannot weaken the sandbox profile required by signed policy"
                        .into(),
                );
            }
            limits.sandbox_profile = Some(profile);
        }
        limits.write_paths = contract.spec.write_paths.clone();
        limits.capabilities = contract.spec.capabilities.clone();
    }
    if limits.max_usd.is_some()
        && limits.usd_per_1k_input_tokens == 0.0
        && limits.usd_per_1k_output_tokens == 0.0
    {
        return Err("a USD budget requires token pricing in the signed policy".into());
    }
    Ok(limits)
}

pub fn contract_matches_identity(
    contract: &WorkContract,
    identity: &crate::identity::VerifiedIdentity,
) -> Result<(), String> {
    if contract.metadata.tenant != identity.tenant {
        return Err("work contract tenant does not match verified identity".into());
    }
    if contract.metadata.principal != identity.principal {
        return Err("work contract principal does not match verified identity".into());
    }
    Ok(())
}

pub fn contract_allows_action(
    contract: Option<&WorkContract>,
    action: &str,
    resource: &str,
) -> bool {
    let Some(contract) = contract else {
        return true;
    };
    // When a contract is present its capability list is authoritative. An
    // omitted list grants nothing rather than silently inheriting everything.
    let action_resource = format!("{action}:{resource}");
    contract.spec.capabilities.iter().any(|cap| {
        glob_match(cap, action) || glob_match(cap, resource) || glob_match(cap, &action_resource)
    })
}

/// Work contracts may narrow the operator's write scope but never widen it.
pub fn contract_allows_write(contract: Option<&WorkContract>, path: &str) -> bool {
    let Some(contract) = contract else {
        return true;
    };
    // Likewise, a contract that names no writable path authorizes no writes.
    let rel = std::path::Path::new(path);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return false;
    }
    contract
        .spec
        .write_paths
        .iter()
        .any(|scope| glob_match(scope, path) || glob_match(&format!("{scope}/**"), path))
}

/// Action taxonomy used by the broker and `jani policy check`.
pub fn action_profile(action: &str) -> (Effect, RiskTier) {
    if action.starts_with("repo.write") || action.starts_with("plugin.write") {
        (Effect::LocalWrite, RiskTier::R2)
    } else if action == "command.exec" {
        (Effect::Control, RiskTier::R2)
    } else if action.starts_with("external.") || action.starts_with("connector.") {
        (Effect::ExternalWrite, RiskTier::R3)
    } else if action == "model.invoke" {
        (Effect::ModelInvoke, RiskTier::R1)
    } else {
        (Effect::Read, RiskTier::R0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn test_bundle() -> PolicyBundle {
        PolicyBundle {
            api_version: "jani.dev/v1alpha1".into(),
            kind: "OrganizationPolicy".into(),
            metadata: PolicyMetadata {
                organization: "org-example".into(),
                revision: "pol-1".into(),
                issuer: "test".into(),
                tenant: Some("local".into()),
                issued_at: Some(now_unix()),
                expires_at: Some(now_unix() + 600),
            },
            spec: PolicySpec {
                rules: vec![
                    PolicyRule {
                        action: "repo.read".into(),
                        resource: "repo:src/**".into(),
                        max_risk: Some(RiskTier::R1),
                        effect: Some(Effect::Read),
                        approval: None,
                        roles: vec![],
                        sandbox_profile: None,
                    },
                    PolicyRule {
                        action: "repo.write".into(),
                        resource: "repo:src/**".into(),
                        max_risk: Some(RiskTier::R2),
                        effect: Some(Effect::LocalWrite),
                        approval: None,
                        roles: vec![],
                        sandbox_profile: None,
                    },
                    PolicyRule {
                        action: "command.exec".into(),
                        resource: "*".into(),
                        max_risk: Some(RiskTier::R4),
                        effect: Some(Effect::Control),
                        approval: None,
                        roles: vec![],
                        sandbox_profile: None,
                    },
                ],
                ..Default::default()
            },
            signature: None,
        }
    }

    fn engine(bundle: PolicyBundle) -> PolicyEngine {
        PolicyEngine {
            effective: EffectivePolicy {
                revision: bundle.metadata.revision.clone(),
                tenant: bundle.metadata.tenant.clone(),
                bundle: Some(bundle),
                enterprise: true,
            },
        }
    }

    fn req(action: &str, resource: &str, effect: Effect, risk: RiskTier) -> ActionRequest {
        ActionRequest {
            task_id: "task-1".into(),
            invocation_id: new_id("inv"),
            tenant_id: "local".into(),
            principal_id: "human".into(),
            action: action.into(),
            resource: resource.into(),
            effect,
            risk,
            arguments_digest: "digest".into(),
            idempotency_key: None,
        }
    }

    #[test]
    fn signed_bundle_round_trips() {
        let secret = "55".repeat(32);
        let public = crate::crypto::public_key_hex(&secret).unwrap();
        let mut bundle = test_bundle();
        bundle.sign(&secret, "default").unwrap();
        assert!(bundle.verify(&[format!("default:{public}")]).is_ok());
        let temp =
            std::env::temp_dir().join(format!("jani-policy-{}.json", crate::process::unique_id()));
        std::fs::write(&temp, serde_json::to_vec(&bundle).unwrap()).unwrap();
        assert!(PolicyBundle::load(&temp, &[format!("default:{public}")]).is_ok());
        let _ = std::fs::remove_file(temp);
    }

    #[test]
    fn unsupported_policy_api_version_is_rejected() {
        let mut bundle = test_bundle();
        bundle.api_version = "jani.dev/v2".into();
        let temp = std::env::temp_dir().join(format!(
            "jani-policy-version-{}.json",
            crate::process::unique_id()
        ));
        std::fs::write(&temp, serde_json::to_vec(&bundle).unwrap()).unwrap();
        let err = PolicyBundle::load(&temp, &[]).unwrap_err();
        assert!(err.contains("apiVersion"));
        let _ = std::fs::remove_file(temp);
    }

    #[test]
    fn role_scoped_rules_are_rejected_and_never_match() {
        let secret = "77".repeat(32);
        let public = crate::crypto::public_key_hex(&secret).unwrap();
        let mut bundle = test_bundle();
        bundle.spec.rules[0].roles = vec!["reviewer".into()];
        bundle.sign(&secret, "default").unwrap();
        let temp = std::env::temp_dir().join(format!(
            "jani-policy-roles-{}.json",
            crate::process::unique_id()
        ));
        std::fs::write(&temp, serde_json::to_vec(&bundle).unwrap()).unwrap();
        let err = PolicyBundle::load(&temp, &[format!("default:{public}")]).unwrap_err();
        assert!(err.contains("roles"));
        let _ = std::fs::remove_file(temp);

        let engine = engine(bundle);
        let cfg = Config::baseline();
        assert_eq!(
            engine
                .evaluate(
                    &cfg,
                    &req("repo.read", "repo:src/main.rs", Effect::Read, RiskTier::R0)
                )
                .decision,
            Decision::Deny
        );
    }

    #[test]
    fn policy_defaults_deny_and_intersects_operator_flags() {
        let secret = "66".repeat(32);
        let public = crate::crypto::public_key_hex(&secret).unwrap();
        let mut bundle = test_bundle();
        bundle.sign(&secret, "default").unwrap();
        let engine = engine(bundle);
        let mut cfg = Config::baseline();
        cfg.policy_keys = vec![format!("default:{public}")];
        cfg.allow_write = true;
        cfg.write_paths = vec![std::path::PathBuf::from("src")];

        assert_eq!(
            engine
                .evaluate(
                    &cfg,
                    &req("repo.read", "repo:src/main.rs", Effect::Read, RiskTier::R0)
                )
                .decision,
            Decision::Permit
        );
        assert_eq!(
            engine
                .evaluate(
                    &cfg,
                    &req(
                        "repo.read",
                        "repo:tests/main.rs",
                        Effect::Read,
                        RiskTier::R0
                    )
                )
                .decision,
            Decision::Deny
        );
        cfg.allow_write = false;
        assert_eq!(
            engine
                .evaluate(
                    &cfg,
                    &req(
                        "repo.write",
                        "repo:src/main.rs",
                        Effect::LocalWrite,
                        RiskTier::R1
                    )
                )
                .decision,
            Decision::Deny
        );
        cfg.allow_write = true;
        assert_eq!(
            engine
                .evaluate(
                    &cfg,
                    &req(
                        "repo.write",
                        "repo:tests/main.rs",
                        Effect::LocalWrite,
                        RiskTier::R1
                    )
                )
                .decision,
            Decision::Deny
        );
    }

    fn contract(capabilities: Vec<String>, write_paths: Vec<String>) -> WorkContract {
        WorkContract {
            api_version: "jani.dev/v1alpha1".into(),
            kind: "WorkContract".into(),
            metadata: ContractMetadata {
                task_id: "task-1".into(),
                tenant: "local".into(),
                principal: "human".into(),
                workspace: ".".into(),
            },
            spec: ContractSpec {
                purpose: "SOFTWARE_DEV".into(),
                capabilities,
                write_paths,
                sandbox_profile: None,
                data_classification: None,
                budget: ContractBudget::default(),
                acceptance: vec![],
            },
        }
    }

    #[test]
    fn a_contract_that_omits_capabilities_grants_nothing() {
        let empty = contract(vec![], vec![]);
        assert!(!contract_allows_action(
            Some(&empty),
            "repo.read",
            "repo:src/main.rs"
        ));
        assert!(!contract_allows_write(Some(&empty), "src/main.rs"));

        let scoped = contract(vec!["repo.read".into()], vec!["src".into()]);
        assert!(contract_allows_action(
            Some(&scoped),
            "repo.read",
            "repo:src/main.rs"
        ));
        assert!(!contract_allows_action(
            Some(&scoped),
            "repo.write",
            "repo:src/main.rs"
        ));
        assert!(contract_allows_write(Some(&scoped), "src/main.rs"));
        assert!(!contract_allows_write(Some(&scoped), "src/../escape.rs"));
    }

    #[test]
    fn high_risk_and_external_writes_require_approval() {
        let engine = engine(test_bundle());
        let mut cfg = Config::baseline();
        cfg.allow_exec = true;
        assert_eq!(
            engine
                .evaluate(
                    &cfg,
                    &req(
                        "command.exec",
                        "command:cargo",
                        Effect::Control,
                        RiskTier::R4
                    )
                )
                .decision,
            Decision::RequireApproval
        );
        assert_eq!(
            engine
                .evaluate(
                    &cfg,
                    &req("repo.read", "repo:src/main.rs", Effect::Read, RiskTier::R0)
                )
                .decision,
            Decision::Permit
        );
    }
}

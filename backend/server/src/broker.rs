use crate::approvals::{ApprovalStore, NewApproval};
use crate::budget::BudgetLedger;
use crate::config::Config;
use crate::data::{redact_json, Governance};
use crate::exec;
use crate::identity::VerifiedIdentity;
use crate::journal::{EventInput, Journal};
use crate::permissions::{Request as PermissionRequest, Verdict as PermissionVerdict};
use crate::policy::{action_profile, contract_allows_action, contract_allows_write, PolicyEngine};
use crate::skills::Catalog;
use crate::types::{
    canonical_resource, json_digest, new_id, sha256_hex, ActionRequest, Classification,
    Constraints, Decision, Effect, NetworkPolicy, PolicyDecision, RiskTier, SandboxProfile,
    ToolResult,
};
use crate::workspace::Workspace;
use serde_json::{json, Value};

pub struct Broker<'a> {
    pub cfg: &'a Config,
    pub policy: &'a PolicyEngine,
    pub identity: &'a VerifiedIdentity,
    pub governance: &'a Governance,
    pub contract: Option<&'a crate::types::WorkContract>,
    pub catalog: &'a Catalog,
    pub journal: &'a mut Journal,
    pub approvals: &'a mut ApprovalStore,
    pub budget: &'a mut BudgetLedger,
    pub run_id: String,
    pub task_id: String,
    pub default_profile: SandboxProfile,
    pub default_network: NetworkPolicy,
    pub approval_ids: &'a [String],
}

struct Call {
    action: &'static str,
    effect: Effect,
    risk: RiskTier,
    resource: String,
}

fn get_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string argument '{key}'"))
}

fn get_argv(value: &Value) -> Result<Vec<String>, String> {
    value
        .get("argv")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing array 'argv'".to_string())?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "argv requires string elements".to_string())
        })
        .collect()
}

fn safe_label(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '*' | '?'))
    {
        return Err("resource label contains unsupported characters".into());
    }
    Ok(())
}

impl<'a> Broker<'a> {
    fn call_for(&self, tool: &str, args: &Value) -> Result<Call, String> {
        match tool {
            "skill_read" => {
                let name = get_str(args, "name")?;
                safe_label(name)?;
                Ok(Call {
                    action: "skill.read",
                    effect: Effect::Read,
                    risk: RiskTier::R0,
                    resource: format!("skill:{name}"),
                })
            }
            "workspace_list" => {
                let directory = args.get("directory").and_then(Value::as_str).unwrap_or(".");
                Ok(Call {
                    action: "repo.list",
                    effect: Effect::Read,
                    risk: RiskTier::R0,
                    resource: canonical_resource("repo", directory)?,
                })
            }
            "workspace_read" => Ok(Call {
                action: "repo.read",
                effect: Effect::Read,
                risk: RiskTier::R0,
                resource: canonical_resource("repo", get_str(args, "path")?)?,
            }),
            "workspace_write" => {
                get_str(args, "content")?;
                Ok(Call {
                    action: "repo.write",
                    effect: Effect::LocalWrite,
                    risk: RiskTier::R2,
                    resource: canonical_resource("repo", get_str(args, "path")?)?,
                })
            }
            "native_tool" => {
                let native = get_str(args, "tool")?;
                let operation = get_str(args, "operation")?;
                if !jani_native::TOOLS.contains(&native) {
                    return Err("unknown native capability".into());
                }
                if operation == "verify" && native == "planner"
                    || operation == "rollback" && native == "workspace"
                    || operation == "run" && native == "verifier"
                {
                    return Err("native privileged action is not available to the model".into());
                }
                let is_mutation = jani_native::mutation(native, operation);
                Ok(Call {
                    action: if is_mutation {
                        "native.control"
                    } else {
                        "native.read"
                    },
                    effect: if is_mutation {
                        Effect::Control
                    } else {
                        Effect::Read
                    },
                    risk: if is_mutation {
                        RiskTier::R1
                    } else {
                        RiskTier::R0
                    },
                    resource: format!(
                        "native:{}:{}:{native}:{operation}",
                        self.identity.tenant, self.task_id
                    ),
                })
            }
            "workspace_command" => {
                let argv = get_argv(args)?;
                let program = argv.first().ok_or("empty executable")?;
                safe_label(program)?;
                let (effect, risk) = action_profile("command.exec");
                Ok(Call {
                    action: "command.exec",
                    effect,
                    risk,
                    resource: format!("command:{program}"),
                })
            }
            other => Err(format!("unknown tool: {other}")),
        }
    }

    fn permission_request<'v>(
        &self,
        tool: &str,
        args: &'v Value,
    ) -> Result<Option<PermissionRequest<'v>>, String> {
        match tool {
            "workspace_read" => Ok(Some(PermissionRequest::Read {
                path: get_str(args, "path")?,
                directory: false,
            })),
            "workspace_list" => Ok(Some(PermissionRequest::Read {
                path: args.get("directory").and_then(Value::as_str).unwrap_or("."),
                directory: true,
            })),
            "workspace_write" => Ok(Some(PermissionRequest::Write {
                path: get_str(args, "path")?,
            })),
            "workspace_command" => Ok(Some(PermissionRequest::Command {
                line: get_argv(args)?.join(" "),
            })),
            _ => Ok(None),
        }
    }

    // Arguments intentionally mirror the journal schema; splitting them would
    // make call sites harder to audit.
    #[allow(clippy::too_many_arguments)]
    fn audit(
        &mut self,
        invocation_id: &str,
        call: &Call,
        decision: &str,
        reason_code: &str,
        classification: Classification,
        approval_id: Option<&str>,
        receipt_id: Option<&str>,
    ) -> Result<(), String> {
        self.journal.record(EventInput {
            tenant_id: &self.identity.tenant,
            task_id: &self.task_id,
            run_id: &self.run_id,
            invocation_id,
            actor_id: &self.identity.principal,
            worker_id: &self.identity.principal,
            action: call.action,
            resource_digest: &sha256_hex(call.resource.as_bytes()),
            decision,
            reason_code,
            policy_revision: &self.policy.effective.revision,
            approval_id,
            sandbox_receipt_id: receipt_id,
            data_classification: classification,
        })?;
        Ok(())
    }

    fn deny(
        &mut self,
        invocation_id: &str,
        call: &Call,
        decision: &PolicyDecision,
        reason: String,
    ) -> ToolResult {
        let audit = self.audit(
            invocation_id,
            call,
            "deny",
            &decision.reason_code,
            Classification::Internal,
            None,
            None,
        );
        let error = match audit {
            Ok(()) => reason,
            Err(audit_error) => format!("{reason}; audit unavailable: {audit_error}"),
        };
        ToolResult::denied(invocation_id, call.effect, &decision.decision_id, error)
    }

    fn reject(
        &mut self,
        invocation_id: &str,
        action: &'static str,
        resource: String,
        reason_code: &str,
        reason: String,
    ) -> ToolResult {
        let call = Call {
            action,
            effect: Effect::Read,
            risk: RiskTier::R0,
            resource,
        };
        let audit = self.audit(
            invocation_id,
            &call,
            "deny",
            reason_code,
            Classification::Internal,
            None,
            None,
        );
        let error = match audit {
            Ok(()) => reason,
            Err(audit_error) => format!("{reason}; audit unavailable: {audit_error}"),
        };
        ToolResult::denied(invocation_id, call.effect, "no-decision", error)
    }

    fn consume_approval(
        &mut self,
        call: &Call,
        payload_digest: &str,
        risk: RiskTier,
        approval_class: &str,
    ) -> Result<Option<String>, String> {
        let ids = self.approval_ids.to_vec();
        for id in ids {
            let Some(request) = self.approvals.find(&id) else {
                continue;
            };
            if request.tenant_id != self.identity.tenant || request.task_id != self.task_id {
                continue;
            }
            if request.capability != call.action || request.resource != call.resource {
                continue;
            }
            if request.payload_digest != payload_digest {
                continue;
            }
            let consumed = self
                .approvals
                .consume(&id, call.action, &call.resource, payload_digest);
            if let Ok(consumed) = consumed {
                return Ok(Some(consumed.approval_id));
            }
        }
        if let Some(existing) = self.approvals.find_pending(
            &self.identity.tenant,
            &self.task_id,
            call.action,
            &call.resource,
            payload_digest,
        ) {
            return Ok(Some(existing.approval_id.clone()));
        }
        let created = self.approvals.create(NewApproval {
            tenant_id: &self.identity.tenant,
            task_id: &self.task_id,
            requester: &self.identity.principal,
            capability: call.action,
            resource: &call.resource,
            payload_digest,
            risk,
            approval_class,
            required_role: "approver",
            ttl_seconds: 900,
        })?;
        Ok(Some(created.approval_id))
    }

    pub fn invoke(&mut self, invocation_id: &str, tool: &str, args: &Value) -> ToolResult {
        let known = matches!(
            tool,
            "skill_read"
                | "workspace_list"
                | "workspace_read"
                | "workspace_write"
                | "workspace_command"
                | "native_tool"
        );
        if !known {
            return self.reject(
                invocation_id,
                "tool.unknown",
                format!("tool:{tool}"),
                "unknown_tool",
                format!("unknown tool: {tool}"),
            );
        }
        if let Some(schema) = crate::model::tool_schema(tool) {
            if let Err(error) = crate::schema::validate(args, &schema) {
                return self.reject(
                    invocation_id,
                    "tool.invalid_arguments",
                    format!("tool:{tool}"),
                    "invalid_tool_arguments",
                    format!("invalid tool arguments: {error}"),
                );
            }
        }
        let call = match self.call_for(tool, args) {
            Ok(call) => call,
            Err(error) => {
                return self.reject(
                    invocation_id,
                    "tool.invalid_arguments",
                    format!("tool:{tool}"),
                    "invalid_tool_request",
                    error,
                );
            }
        };
        let payload_digest = json_digest(args);

        if !contract_allows_action(self.contract, call.action, &call.resource) {
            let decision = PolicyDecision {
                decision: Decision::Deny,
                reason_code: "contract_capability_denied".into(),
                decision_id: new_id("dec"),
                policy_revision: self.policy.effective.revision.clone(),
                constraints: Constraints::default(),
            };
            return self.deny(
                invocation_id,
                &call,
                &decision,
                "work contract does not grant this capability".into(),
            );
        }
        if call.action == "repo.write" {
            let path = call
                .resource
                .strip_prefix("repo:")
                .unwrap_or(&call.resource);
            if !contract_allows_write(self.contract, path) {
                let decision = PolicyDecision {
                    decision: Decision::Deny,
                    reason_code: "contract_write_scope_denied".into(),
                    decision_id: new_id("dec"),
                    policy_revision: self.policy.effective.revision.clone(),
                    constraints: Constraints::default(),
                };
                return self.deny(
                    invocation_id,
                    &call,
                    &decision,
                    "resource is outside the signed work-contract write scope".into(),
                );
            }
        }

        // Project permission rules are a narrowing layer. They run before the
        // signed policy so a deny is recorded with its own reason, and an ask
        // can add an approval requirement but never remove one.
        let mut settings_ask: Option<String> = None;
        {
            let cfg = self.cfg;
            if let Some(policy) = cfg.permissions.as_ref() {
                let verdict = match self.permission_request(tool, args) {
                    Ok(request) => policy.check(request.as_ref(), call.effect),
                    Err(error) => PermissionVerdict::Deny {
                        rule: "settings request".into(),
                        reason: format!(
                            "settings permissions could not evaluate this call: {error}"
                        ),
                    },
                };
                match verdict {
                    PermissionVerdict::Deny { rule, reason } => {
                        let decision = PolicyDecision {
                            decision: Decision::Deny,
                            reason_code: "settings_permission_denied".into(),
                            decision_id: new_id("dec"),
                            policy_revision: self.policy.effective.revision.clone(),
                            constraints: Constraints {
                                reason: Some(format!("{reason} ({rule})")),
                                ..Default::default()
                            },
                        };
                        return self.deny(
                            invocation_id,
                            &call,
                            &decision,
                            format!("{reason} ({rule})"),
                        );
                    }
                    PermissionVerdict::Ask { rule } => settings_ask = Some(rule),
                    PermissionVerdict::Allow | PermissionVerdict::Unrestricted => {}
                }
            }
        }

        let request = ActionRequest {
            task_id: self.task_id.clone(),
            invocation_id: invocation_id.to_string(),
            tenant_id: self.identity.tenant.clone(),
            principal_id: self.identity.principal.clone(),
            action: call.action.into(),
            resource: call.resource.clone(),
            effect: call.effect,
            risk: call.risk,
            arguments_digest: payload_digest.clone(),
            idempotency_key: None,
        };
        let mut decision = self.policy.evaluate(self.cfg, &request);
        if decision.decision == Decision::Permit {
            if let Some(rule) = settings_ask.as_ref() {
                decision = PolicyDecision {
                    decision: Decision::RequireApproval,
                    reason_code: "settings_permission_approval_required".into(),
                    decision_id: decision.decision_id.clone(),
                    policy_revision: decision.policy_revision.clone(),
                    constraints: Constraints {
                        approval_class: Some("settings-permission".into()),
                        reason: Some(format!(
                            "settings permission rule '{rule}' requires approval"
                        )),
                        ..decision.constraints.clone()
                    },
                };
            }
        }
        let mut approved_id: Option<String> = None;
        match decision.decision {
            Decision::Deny => {
                return self.deny(
                    invocation_id,
                    &call,
                    &decision,
                    decision
                        .constraints
                        .reason
                        .clone()
                        .unwrap_or_else(|| "denied by policy".into()),
                );
            }
            Decision::RequireApproval => {
                let approval_class = decision
                    .constraints
                    .approval_class
                    .clone()
                    .unwrap_or_else(|| format!("risk-{}", call.risk.as_str()));
                match self.consume_approval(&call, &payload_digest, call.risk, &approval_class) {
                    Ok(Some(approval_id)) => {
                        let Some((status, required_role, expires_at)) =
                            self.approvals.find(&approval_id).map(|request| {
                                (
                                    request.status.clone(),
                                    request.required_role.clone(),
                                    request.expires_at,
                                )
                            })
                        else {
                            return ToolResult::denied(
                                invocation_id,
                                call.effect,
                                &decision.decision_id,
                                "approval store lost the request",
                            );
                        };
                        if status != crate::types::ApprovalStatus::Approved
                            && status != crate::types::ApprovalStatus::Consumed
                        {
                            let _ = self.audit(
                                invocation_id,
                                &call,
                                "approval_required",
                                "approval_required",
                                Classification::Internal,
                                Some(&approval_id),
                                None,
                            );
                            return ToolResult {
                                invocation_id: invocation_id.into(),
                                status: "approval_required".into(),
                                effect: call.effect,
                                data: Some(json!({
                                    "approvalId": approval_id,
                                    "approvalClass": approval_class,
                                    "requiredRole": required_role,
                                    "expiresAt": expires_at,
                                })),
                                error: Some(
                                    "this action requires a signed human approval before it can run"
                                        .into(),
                                ),
                                decision_id: decision.decision_id,
                                classification: Classification::Internal,
                                sandbox: None,
                                approval_id: Some(approval_id),
                                retryable: false,
                            };
                        }
                        // Approved and consumed: fall through to execution.
                        approved_id = Some(approval_id);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        return ToolResult::denied(
                            invocation_id,
                            call.effect,
                            &decision.decision_id,
                            error,
                        );
                    }
                }
            }
            Decision::Permit => {}
        }

        let approval_id = approved_id;

        if let Err(error) = self.budget.reserve_tool_call() {
            let audit = self.audit(
                invocation_id,
                &call,
                "deny",
                "budget_exhausted",
                Classification::Internal,
                approval_id.as_deref(),
                None,
            );
            let error = match audit {
                Ok(()) => error,
                Err(audit_error) => format!("{error}; audit unavailable: {audit_error}"),
            };
            return ToolResult::denied(invocation_id, call.effect, &decision.decision_id, error);
        }
        if let Err(error) = self.audit(
            invocation_id,
            &call,
            "permit",
            &decision.reason_code,
            Classification::Internal,
            approval_id.as_deref(),
            None,
        ) {
            return ToolResult::denied(
                invocation_id,
                call.effect,
                &decision.decision_id,
                format!("audit unavailable; refusing to execute: {error}"),
            );
        }

        let execution = self.execute(tool, args, &decision);
        let (data, receipt, error) = match execution {
            Ok(output) => output,
            Err(error) => {
                let _ = self.audit(
                    invocation_id,
                    &call,
                    "failed",
                    "execution_failed",
                    Classification::Internal,
                    approval_id.as_deref(),
                    None,
                );
                return ToolResult {
                    invocation_id: invocation_id.into(),
                    status: "error".into(),
                    effect: call.effect,
                    data: None,
                    error: Some(error),
                    decision_id: decision.decision_id,
                    classification: Classification::Internal,
                    sandbox: None,
                    approval_id,
                    retryable: false,
                };
            }
        };

        let raw = crate::data::json_strings(&data);
        let classified = self
            .governance
            .classify_tool_output("tool-output", &call.resource, &raw);
        let mut data = data;
        let redacted_fields = redact_json(&mut data);
        if classified.original != classified.effective {
            if let Some(object) = data.as_object_mut() {
                object.insert(
                    "_governance".into(),
                    json!({
                        "originalClassification": classified.original.as_str(),
                        "effectiveClassification": classified.effective.as_str(),
                        "secretsRedacted": classified.secrets_found,
                        "fieldsRedacted": redacted_fields,
                    }),
                );
            }
        }

        if let Some(max) = self
            .contract
            .and_then(|contract| contract.spec.data_classification)
        {
            if classified.effective > max {
                let _ = self.audit(
                    invocation_id,
                    &call,
                    "failed",
                    "contract_data_classification_denied",
                    classified.effective,
                    approval_id.as_deref(),
                    receipt.as_ref().map(|r| r.sandbox_id.as_str()),
                );
                return ToolResult {
                    invocation_id: invocation_id.into(),
                    status: "denied".into(),
                    effect: call.effect,
                    data: None,
                    error: Some(format!(
                        "result classification {} exceeds the work contract ceiling {}",
                        classified.effective, max
                    )),
                    decision_id: decision.decision_id,
                    classification: classified.effective,
                    sandbox: receipt,
                    approval_id,
                    retryable: false,
                };
            }
        }

        let receipt_id = receipt.as_ref().map(|r| r.sandbox_id.clone());
        let audit_error = self
            .audit(
                invocation_id,
                &call,
                if error.is_some() {
                    "failed"
                } else {
                    "executed"
                },
                if error.is_some() {
                    "tool_reported_error"
                } else {
                    "ok"
                },
                classified.effective,
                approval_id.as_deref(),
                receipt_id.as_deref(),
            )
            .err();

        ToolResult {
            invocation_id: invocation_id.into(),
            status: if error.is_some() {
                "error".into()
            } else {
                "ok".into()
            },
            effect: call.effect,
            data: Some(data),
            error: match (error, audit_error) {
                (Some(error), Some(audit)) => Some(format!(
                    "{error}; audit append failed after execution: {audit}"
                )),
                (Some(error), None) => Some(error),
                (None, Some(audit)) => {
                    Some(format!("action executed but audit append failed: {audit}"))
                }
                (None, None) => None,
            },
            decision_id: decision.decision_id,
            classification: classified.effective,
            sandbox: receipt,
            approval_id,
            retryable: false,
        }
    }

    fn execute(
        &mut self,
        tool: &str,
        args: &Value,
        decision: &PolicyDecision,
    ) -> Result<(Value, Option<crate::types::SandboxReceipt>, Option<String>), String> {
        let workspace = Workspace { cfg: self.cfg };
        match tool {
            "skill_read" => {
                let name = get_str(args, "name")?;
                let body = self.catalog.read(name)?;
                Ok((json!({"skill": name, "contents": body}), None, None))
            }
            "workspace_list" => {
                let directory = args.get("directory").and_then(Value::as_str).unwrap_or(".");
                Ok((json!({"files": workspace.list(directory)?}), None, None))
            }
            "workspace_read" => {
                let path = get_str(args, "path")?;
                Ok((json!({"contents": workspace.read(path)?}), None, None))
            }
            "workspace_write" => {
                let path = get_str(args, "path")?;
                let content = get_str(args, "content")?;
                Ok((
                    json!({"result": workspace.write(path, content)?}),
                    None,
                    None,
                ))
            }
            "native_tool" => {
                let name = get_str(args, "tool")?;
                let operation = get_str(args, "operation")?;
                let extra = args
                    .get("args")
                    .and_then(Value::as_array)
                    .ok_or("native args must be an array")?;
                if extra.len() > 5 {
                    return Err("native argument count exceeds limit".into());
                }
                let mut parts = vec![operation.to_string()];
                for item in extra {
                    parts.push(
                        item.as_str()
                            .ok_or("native args must be strings")?
                            .to_owned(),
                    );
                }
                if name == "planner" && operation == "verify"
                    || name == "workspace" && operation == "rollback"
                    || name == "verifier" && operation == "run"
                {
                    return Err(
                        "native tool cannot perform user-only or ungoverned operations".into(),
                    );
                }
                let data = jani_native::run(
                    &self.cfg.root,
                    &self.cfg.state_dir,
                    &self.identity.tenant,
                    &self.task_id,
                    name,
                    &parts,
                    false,
                )?;
                Ok((data, None, None))
            }
            "workspace_command" => {
                let argv = get_argv(args)?;
                let timeout = args
                    .get("timeout_secs")
                    .and_then(Value::as_u64)
                    .ok_or("timeout_secs must be a positive integer")?;
                let profile = decision
                    .constraints
                    .sandbox_profile
                    .as_deref()
                    .and_then(SandboxProfile::parse)
                    .unwrap_or(self.default_profile);
                let network = decision
                    .constraints
                    .network
                    .as_deref()
                    .and_then(NetworkPolicy::parse)
                    .unwrap_or(self.default_network);
                let (value, receipt) =
                    exec::run_confined(self.cfg, &argv, timeout, profile, network)?;
                Ok((value, Some(receipt), None))
            }
            _ => Err("unknown tool".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::identity::VerifiedIdentity;
    use crate::journal;
    use crate::policy::{
        EffectiveLimits, EffectivePolicy, PolicyBundle, PolicyMetadata, PolicyRule, PolicySpec,
    };
    use crate::skills::Catalog;
    use crate::types::{ContractBudget, ContractMetadata, ContractSpec, WorkContract};
    use std::fs;
    use std::path::PathBuf;

    fn policy(approval_on_write: bool) -> PolicyEngine {
        let rule = |action: &str,
                    resource: &str,
                    effect: Effect,
                    risk: RiskTier,
                    approval: Option<&str>| PolicyRule {
            action: action.into(),
            resource: resource.into(),
            max_risk: Some(risk),
            effect: Some(effect),
            approval: approval.map(str::to_string),
            roles: vec![],
            sandbox_profile: None,
        };
        let bundle = PolicyBundle {
            api_version: "jani.dev/v1alpha1".into(),
            kind: "OrganizationPolicy".into(),
            metadata: PolicyMetadata {
                organization: "org-test".into(),
                revision: "pol-test".into(),
                issuer: "test".into(),
                tenant: Some("t1".into()),
                issued_at: None,
                expires_at: None,
            },
            spec: PolicySpec {
                rules: vec![
                    rule("repo.read", "repo:src/**", Effect::Read, RiskTier::R1, None),
                    rule(
                        "repo.write",
                        "**",
                        Effect::LocalWrite,
                        RiskTier::R2,
                        approval_on_write.then_some("write-review"),
                    ),
                ],
                ..Default::default()
            },
            signature: None,
        };
        PolicyEngine {
            effective: EffectivePolicy {
                revision: bundle.metadata.revision.clone(),
                tenant: bundle.metadata.tenant.clone(),
                bundle: Some(bundle),
                enterprise: true,
            },
        }
    }

    fn contract(
        capabilities: &[&str],
        write_paths: &[&str],
        ceiling: Option<Classification>,
    ) -> WorkContract {
        WorkContract {
            api_version: "jani.dev/v1alpha1".into(),
            kind: "WorkContract".into(),
            metadata: ContractMetadata {
                task_id: "task-1".into(),
                tenant: "t1".into(),
                principal: "p1".into(),
                workspace: ".".into(),
            },
            spec: ContractSpec {
                purpose: "SOFTWARE_DEV".into(),
                capabilities: capabilities.iter().map(|c| c.to_string()).collect(),
                write_paths: write_paths.iter().map(|p| p.to_string()).collect(),
                sandbox_profile: None,
                data_classification: ceiling,
                budget: ContractBudget::default(),
                acceptance: vec![],
            },
        }
    }

    struct TestEnv {
        dir: PathBuf,
        cfg: Config,
        identity: VerifiedIdentity,
        policy: PolicyEngine,
        governance: Governance,
        contract: Option<WorkContract>,
        catalog: Catalog,
        journal: Journal,
        approvals: ApprovalStore,
        budget: BudgetLedger,
    }

    impl TestEnv {
        fn new(
            contract: Option<WorkContract>,
            limits: &EffectiveLimits,
            approval_on_write: bool,
        ) -> Self {
            let dir =
                std::env::temp_dir().join(format!("jani-broker-{}", crate::process::unique_id()));
            fs::create_dir_all(dir.join("src")).unwrap();
            fs::write(dir.join("src/one.txt"), "hello\n").unwrap();

            let mut cfg = Config::baseline();
            cfg.root = dir.clone();
            cfg.state_dir = dir.clone();
            cfg.tenant = "t1".into();
            cfg.principal = "p1".into();
            cfg.task_id = Some("task-1".into());
            cfg.allow_write = true;
            cfg.write_paths = vec![PathBuf::from("src")];

            let identity = VerifiedIdentity {
                principal: "p1".into(),
                tenant: "t1".into(),
                subject_type: "human".into(),
                issuer: "test".into(),
                verified: true,
                expires_at: None,
            };
            let policy = policy(approval_on_write);
            let governance = Governance::new(policy.data_policy(), "t1", "p1", "SOFTWARE_DEV");
            let journal = Journal::open(&dir).unwrap();
            let approvals = ApprovalStore::open(&dir).unwrap();
            let budget = BudgetLedger::new(&dir, "task-1", limits).unwrap();
            Self {
                dir,
                cfg,
                identity,
                policy,
                governance,
                contract,
                catalog: Catalog::default(),
                journal,
                approvals,
                budget,
            }
        }

        fn broker(&mut self) -> Broker<'_> {
            Broker {
                cfg: &self.cfg,
                policy: &self.policy,
                identity: &self.identity,
                governance: &self.governance,
                contract: self.contract.as_ref(),
                catalog: &self.catalog,
                journal: &mut self.journal,
                approvals: &mut self.approvals,
                budget: &mut self.budget,
                run_id: "run-test".into(),
                task_id: "task-1".into(),
                default_profile: SandboxProfile::TrustedDev,
                default_network: NetworkPolicy::Deny,
                approval_ids: &self.cfg.approval_ids,
            }
        }
    }

    fn events(dir: &std::path::Path) -> Vec<crate::types::AuditEvent> {
        journal::read_events(&dir.join(journal::JOURNAL_FILE)).unwrap()
    }

    fn cleanup(env: TestEnv) {
        let dir = env.dir.clone();
        drop(env);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn malformed_and_unknown_tool_requests_are_audited_and_denied() {
        let mut env = TestEnv::new(None, &EffectiveLimits::default(), false);
        {
            let mut broker = env.broker();
            let malformed =
                broker.invoke("inv-1", "workspace_write", &json!({"path": "src/two.txt"}));
            assert_eq!(malformed.status, "denied");
            assert!(malformed
                .error
                .as_deref()
                .unwrap()
                .contains("invalid tool arguments"));
            let unknown = broker.invoke("inv-2", "shell", &json!({}));
            assert_eq!(unknown.status, "denied");
            assert!(unknown.error.as_deref().unwrap().contains("unknown tool"));
        }
        let events = events(&env.dir);
        assert!(events
            .iter()
            .any(|e| e.action == "tool.invalid_arguments" && e.decision == "deny"));
        assert!(events
            .iter()
            .any(|e| e.action == "tool.unknown" && e.reason_code == "unknown_tool"));
        cleanup(env);
    }

    #[test]
    fn contract_capabilities_and_write_scope_are_enforced() {
        let mut env = TestEnv::new(
            Some(contract(&["repo.read"], &["src"], None)),
            &EffectiveLimits::default(),
            false,
        );
        {
            let mut broker = env.broker();
            let read = broker.invoke("inv-1", "workspace_read", &json!({"path": "src/one.txt"}));
            assert_eq!(read.status, "ok");
            let write = broker.invoke(
                "inv-2",
                "workspace_write",
                &json!({"path": "src/two.txt", "content": "x"}),
            );
            assert_eq!(write.status, "denied");
            assert!(write
                .error
                .as_deref()
                .unwrap()
                .contains("work contract does not grant"));
        }
        cleanup(env);

        let mut env = TestEnv::new(
            Some(contract(&["repo.write"], &["tests"], None)),
            &EffectiveLimits::default(),
            false,
        );
        {
            let mut broker = env.broker();
            let write = broker.invoke(
                "inv-3",
                "workspace_write",
                &json!({"path": "src/two.txt", "content": "x"}),
            );
            assert_eq!(write.status, "denied");
            assert!(write
                .error
                .as_deref()
                .unwrap()
                .contains("signed work-contract write scope"));
        }
        cleanup(env);
    }

    #[test]
    fn tool_output_secrets_are_redacted_and_marked() {
        let mut env = TestEnv::new(None, &EffectiveLimits::default(), false);
        fs::write(
            env.dir.join("src/notes.txt"),
            "password = \"supersecret123\"\n",
        )
        .unwrap();
        let result = {
            let mut broker = env.broker();
            broker.invoke("inv-1", "workspace_read", &json!({"path": "src/notes.txt"}))
        };
        assert_eq!(result.status, "ok");
        let data = result.data.unwrap();
        let contents = data["contents"].as_str().unwrap();
        assert!(contents.contains("[REDACTED]"));
        assert!(!contents.contains("supersecret123"));
        assert_eq!(data["_governance"]["secretsRedacted"], 1);
        assert_eq!(result.classification, Classification::Confidential);
        cleanup(env);
    }

    #[test]
    fn contract_classification_ceiling_blocks_output() {
        let mut env = TestEnv::new(
            Some(contract(
                &["repo.read"],
                &[],
                Some(Classification::Internal),
            )),
            &EffectiveLimits::default(),
            false,
        );
        fs::write(env.dir.join("src/pii.txt"), "contact alice@example.com\n").unwrap();
        let result = {
            let mut broker = env.broker();
            broker.invoke("inv-1", "workspace_read", &json!({"path": "src/pii.txt"}))
        };
        assert_eq!(result.status, "denied");
        assert!(result.data.is_none());
        assert!(result
            .error
            .as_deref()
            .unwrap()
            .contains("exceeds the work contract ceiling"));
        assert!(events(&env.dir).iter().any(|e| {
            e.reason_code == "contract_data_classification_denied" && e.decision == "failed"
        }));
        cleanup(env);
    }

    #[test]
    fn exhausted_budget_denies_and_audits() {
        let limits = EffectiveLimits {
            max_tool_calls: Some(1),
            ..Default::default()
        };
        let mut env = TestEnv::new(None, &limits, false);
        {
            let mut broker = env.broker();
            let first = broker.invoke("inv-1", "workspace_read", &json!({"path": "src/one.txt"}));
            assert_eq!(first.status, "ok");
            let second = broker.invoke("inv-2", "workspace_read", &json!({"path": "src/one.txt"}));
            assert_eq!(second.status, "denied");
            assert!(second
                .error
                .as_deref()
                .unwrap()
                .contains("tool-call budget exhausted"));
        }
        assert!(events(&env.dir)
            .iter()
            .any(|e| e.reason_code == "budget_exhausted" && e.decision == "deny"));
        cleanup(env);
    }

    #[test]
    fn approvals_are_exactly_bound_and_single_use() {
        let mut env = TestEnv::new(None, &EffectiveLimits::default(), true);
        let args = json!({"path": "src/approved.txt", "content": "ok"});
        let approval_id = {
            let mut broker = env.broker();
            let pending = broker.invoke("inv-1", "workspace_write", &args);
            assert_eq!(pending.status, "approval_required");
            pending.data.unwrap()["approvalId"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert!(env
            .approvals
            .decide(&approval_id, "p1", "approver", true)
            .is_err());
        env.approvals
            .decide(&approval_id, "p2", "approver", true)
            .unwrap();
        env.cfg.approval_ids = vec![approval_id.clone()];
        {
            let mut broker = env.broker();
            let executed = broker.invoke("inv-2", "workspace_write", &args);
            assert_eq!(executed.status, "ok");
            assert_eq!(executed.approval_id.as_deref(), Some(approval_id.as_str()));
        }
        {
            let mut broker = env.broker();
            let reused = broker.invoke("inv-3", "workspace_write", &args);
            assert_eq!(reused.status, "approval_required");
            assert_ne!(
                reused.data.unwrap()["approvalId"].as_str().unwrap(),
                approval_id
            );
        }
        cleanup(env);
    }

    #[test]
    fn operator_write_scope_denies_even_when_policy_allows() {
        let mut env = TestEnv::new(None, &EffectiveLimits::default(), false);
        let result = {
            let mut broker = env.broker();
            broker.invoke(
                "inv-1",
                "workspace_write",
                &json!({"path": "tests/escape.rs", "content": "x"}),
            )
        };
        assert_eq!(result.status, "denied");
        assert!(result
            .error
            .as_deref()
            .unwrap()
            .contains("operator-approved write scope"));
        cleanup(env);
    }

    fn permission_policy(
        mode: crate::permissions::Mode,
        allow: &[&str],
        ask: &[&str],
        deny: &[&str],
    ) -> crate::permissions::Policy {
        let owned = |rules: &[&str]| {
            rules
                .iter()
                .map(|rule| rule.to_string())
                .collect::<Vec<_>>()
        };
        crate::permissions::Policy::compile(mode, &owned(allow), &owned(ask), &owned(deny)).unwrap()
    }

    #[test]
    fn settings_deny_blocks_reads_and_is_audited() {
        let mut env = TestEnv::new(None, &EffectiveLimits::default(), false);
        env.cfg.permissions = Some(permission_policy(
            crate::permissions::Mode::Default,
            &[],
            &[],
            &["Read(./src/one.txt)"],
        ));
        let result = {
            let mut broker = env.broker();
            broker.invoke("inv-1", "workspace_read", &json!({"path": "src/one.txt"}))
        };
        assert_eq!(result.status, "denied");
        assert!(result
            .error
            .as_deref()
            .unwrap()
            .contains("settings permission rule"));
        assert!(events(&env.dir)
            .iter()
            .any(|e| e.reason_code == "settings_permission_denied"));
        cleanup(env);
    }

    #[test]
    fn settings_allowlist_restricts_commands_without_widening_them() {
        let mut env = TestEnv::new(None, &EffectiveLimits::default(), false);
        env.cfg.allow_exec = true;
        env.cfg.permissions = Some(permission_policy(
            crate::permissions::Mode::Default,
            &["Bash(cargo test *)"],
            &[],
            &[],
        ));
        let result = {
            let mut broker = env.broker();
            broker.invoke(
                "inv-1",
                "workspace_command",
                &json!({"argv": ["cargo", "fmt", "--check"], "timeout_secs": 30}),
            )
        };
        assert_eq!(result.status, "denied");
        assert!(result
            .error
            .as_deref()
            .unwrap()
            .contains("settings allowlist"));
        cleanup(env);

        let mut env = TestEnv::new(None, &EffectiveLimits::default(), false);
        env.cfg.allow_exec = false;
        env.cfg.permissions = Some(permission_policy(
            crate::permissions::Mode::Default,
            &["Bash(cargo test *)"],
            &[],
            &[],
        ));
        let result = {
            let mut broker = env.broker();
            broker.invoke(
                "inv-2",
                "workspace_command",
                &json!({"argv": ["cargo", "test"], "timeout_secs": 30}),
            )
        };
        assert_eq!(result.status, "denied");
        assert!(result
            .error
            .as_deref()
            .unwrap()
            .contains("command execution is not enabled"));
        cleanup(env);
    }

    #[test]
    fn settings_ask_forces_an_external_approval() {
        let mut env = TestEnv::new(None, &EffectiveLimits::default(), false);
        env.cfg.permissions = Some(permission_policy(
            crate::permissions::Mode::Default,
            &[],
            &["Write(./src/**)"],
            &[],
        ));
        let result = {
            let mut broker = env.broker();
            broker.invoke(
                "inv-1",
                "workspace_write",
                &json!({"path": "src/asked.txt", "content": "x"}),
            )
        };
        assert_eq!(result.status, "approval_required");
        assert_eq!(
            result.data.unwrap()["approvalClass"].as_str(),
            Some("settings-permission")
        );
        cleanup(env);
    }

    #[test]
    fn plan_mode_blocks_writes_and_commands() {
        let mut env = TestEnv::new(None, &EffectiveLimits::default(), false);
        env.cfg.allow_exec = true;
        env.cfg.permissions = Some(permission_policy(
            crate::permissions::Mode::Plan,
            &[],
            &[],
            &[],
        ));
        let write = {
            let mut broker = env.broker();
            broker.invoke(
                "inv-1",
                "workspace_write",
                &json!({"path": "src/plan.txt", "content": "x"}),
            )
        };
        assert_eq!(write.status, "denied");
        assert!(write
            .error
            .as_deref()
            .unwrap()
            .contains("plan mode is read-only"));
        let command = {
            let mut broker = env.broker();
            broker.invoke(
                "inv-2",
                "workspace_command",
                &json!({"argv": ["cargo", "check"], "timeout_secs": 30}),
            )
        };
        assert_eq!(command.status, "denied");
        assert!(command
            .error
            .as_deref()
            .unwrap()
            .contains("plan mode is read-only"));
        let read = {
            let mut broker = env.broker();
            broker.invoke("inv-3", "workspace_read", &json!({"path": "src/one.txt"}))
        };
        assert_eq!(read.status, "ok");
        cleanup(env);
    }
}

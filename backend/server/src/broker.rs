use crate::approvals::{ApprovalStore, NewApproval};
use crate::budget::BudgetLedger;
use crate::config::{Config, ExecMode};
use crate::consent::{Consent, ConsentAnswer, ConsentRequest, Question};
use crate::data::{redact_json, Governance};
use crate::exec;
use crate::execpolicy::{self, Basis, Verdict};
use crate::identity::VerifiedIdentity;
use crate::journal::{decision as journal_decision, reason, EventInput, Journal};
use crate::permissions::{Request as PermissionRequest, Verdict as PermissionVerdict};
use crate::policy::{action_profile, contract_allows_action, contract_allows_write, PolicyEngine};
use crate::skills::Catalog;
use crate::tools::{action, Tool};
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
    /// Who to ask when `--exec smart` meets a command it cannot classify as safe.
    pub consent: Option<&'a dyn Consent>,
    /// Command prefixes the operator allowed "for this session".
    pub session_exec: &'a mut Vec<Vec<String>>,
    /// Set when the agent asked a question nobody could answer yet; the turn ends on it.
    pub asked: &'a mut Option<Question>,
}

struct Call {
    action: &'static str,
    effect: Effect,
    risk: RiskTier,
    resource: String,
}

/// Bounded `args` array accepted by the `native_tool` call.
const MAX_NATIVE_ARGS: usize = 5;

fn get_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string argument '{key}'"))
}

const MAX_QUESTION_CHARS: usize = 500;
const MAX_OPTIONS: usize = 6;
const MAX_OPTION_CHARS: usize = 100;

fn question_from(args: &Value) -> Result<Question, String> {
    let text = get_str(args, "question")?.trim().to_string();
    if text.is_empty() || text.chars().count() > MAX_QUESTION_CHARS {
        return Err(format!(
            "the question must be 1..={MAX_QUESTION_CHARS} characters"
        ));
    }
    let options: Vec<String> = match args.get("options") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty() && s.chars().count() <= MAX_OPTION_CHARS)
                    .ok_or_else(|| format!("each option must be 1..={MAX_OPTION_CHARS} characters"))
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("options must be a list of strings".into()),
    };
    if options.len() > MAX_OPTIONS {
        return Err(format!(
            "at most {MAX_OPTIONS} options; ask a narrower question"
        ));
    }
    Ok(Question { text, options })
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
    fn call_for(&self, tool: Tool, args: &Value) -> Result<Call, String> {
        match tool {
            Tool::SkillRead => {
                let name = get_str(args, "name")?;
                safe_label(name)?;
                Ok(Call {
                    action: action::SKILL_READ,
                    effect: Effect::Read,
                    risk: RiskTier::R0,
                    resource: format!("skill:{name}"),
                })
            }
            Tool::List => {
                let directory = args.get("directory").and_then(Value::as_str).unwrap_or(".");
                Ok(Call {
                    action: action::REPO_LIST,
                    effect: Effect::Read,
                    risk: RiskTier::R0,
                    resource: canonical_resource("repo", directory)?,
                })
            }
            Tool::Read => Ok(Call {
                action: action::REPO_READ,
                effect: Effect::Read,
                risk: RiskTier::R0,
                resource: canonical_resource("repo", get_str(args, "path")?)?,
            }),
            Tool::Write => {
                get_str(args, "content")?;
                Ok(Call {
                    action: action::REPO_WRITE,
                    effect: Effect::LocalWrite,
                    risk: RiskTier::R2,
                    resource: canonical_resource("repo", get_str(args, "path")?)?,
                })
            }
            Tool::NativeTool => {
                let native = get_str(args, "tool")?;
                let operation = get_str(args, "operation")?;
                if !jani_native::TOOLS.contains(&native) {
                    return Err("unknown native capability".into());
                }
                if jani_native::privileged(native, operation) {
                    return Err("native privileged action is not available to the model".into());
                }
                let is_mutation = jani_native::mutation(native, operation);
                Ok(Call {
                    action: if is_mutation {
                        action::NATIVE_CONTROL
                    } else {
                        action::NATIVE_READ
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
            Tool::Ask => {
                question_from(args)?;
                Ok(Call {
                    action: action::AGENT_ASK,
                    // Asking changes nothing: read-level, so it needs no write or exec grant.
                    effect: Effect::Read,
                    risk: RiskTier::R0,
                    resource: "question".into(),
                })
            }
            Tool::Remember => {
                get_str(args, "topic")?;
                get_str(args, "note")?;
                Ok(Call {
                    action: action::MEMORY_WRITE,
                    // Writes to the operator's state directory, not the worktree, so it is
                    // a control action: it needs a policy that permits `memory.write`, not
                    // the operator's file-editing grant.
                    effect: Effect::Control,
                    risk: RiskTier::R1,
                    resource: "memory:notes".into(),
                })
            }
            Tool::Command => {
                let argv = get_argv(args)?;
                let program = argv.first().ok_or("empty executable")?;
                safe_label(program)?;
                let (effect, risk) = action_profile(action::COMMAND_EXEC);
                Ok(Call {
                    action: action::COMMAND_EXEC,
                    effect,
                    risk,
                    resource: format!("command:{program}"),
                })
            }
        }
    }

    fn permission_request<'v>(
        &self,
        tool: Tool,
        args: &'v Value,
    ) -> Result<Option<PermissionRequest<'v>>, String> {
        match tool {
            Tool::Read => Ok(Some(PermissionRequest::Read {
                path: get_str(args, "path")?,
                directory: false,
            })),
            Tool::List => Ok(Some(PermissionRequest::Read {
                path: args.get("directory").and_then(Value::as_str).unwrap_or("."),
                directory: true,
            })),
            Tool::Write => Ok(Some(PermissionRequest::Write {
                path: get_str(args, "path")?,
            })),
            Tool::Command => Ok(Some(PermissionRequest::Command {
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

    /// `--exec smart`: classify the command and, when it is not safe on its own,
    /// get the operator's answer. Returns what the sandbox may additionally
    /// write, or the finished (refused) result.
    fn smart_exec(
        &mut self,
        invocation_id: &str,
        call: &Call,
        args: &Value,
        decision: &mut PolicyDecision,
    ) -> Result<exec::ExecGrant, Box<ToolResult>> {
        let refuse = |this: &mut Self, code: &str, reason: String| {
            let denial = PolicyDecision {
                decision: Decision::Deny,
                reason_code: code.into(),
                decision_id: decision.decision_id.clone(),
                policy_revision: decision.policy_revision.clone(),
                constraints: Constraints {
                    reason: Some(reason.clone()),
                    ..Default::default()
                },
            };
            Box::new(this.deny(invocation_id, call, &denial, reason))
        };
        let argv = match get_argv(args) {
            Ok(argv) => argv,
            Err(error) => return Err(refuse(self, "invalid_tool_request", error)),
        };
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
        // Trust follows containment: only a sandbox that makes the worktree
        // read-only (apart from the write scope) and cuts the network counts.
        let (confined, network_cut) = execpolicy::confinement(self.cfg, profile, network);
        let rules = match execpolicy::load_rules(&self.cfg.state_dir) {
            Ok(rules) => rules,
            Err(error) => {
                return Err(refuse(
                    self,
                    "exec_rules_unreadable",
                    format!(
                        "operator command rules could not be read, so no command runs: {error}"
                    ),
                ))
            }
        };
        let verdict = execpolicy::classify(
            &argv,
            &execpolicy::Context {
                cfg: self.cfg,
                confined,
                network_cut,
                rules: &rules,
                session: self.session_exec,
            },
        );
        let granted = || exec::ExecGrant {
            preauthorized: true,
            extra_writable: if confined {
                execpolicy::build_outputs(&argv, &self.cfg.root)
            } else {
                Vec::new()
            },
        };
        match verdict {
            Verdict::Allow(basis) => {
                decision.reason_code = basis.code().into();
                Ok(granted())
            }
            Verdict::Deny { reason } => Err(refuse(self, "exec_policy_denied", reason)),
            Verdict::Ask { reason, suggest } => {
                let line = argv.join(" ");
                if self.policy.effective.bundle.is_some() {
                    // Managed mode: only a signed approval from a second person counts.
                    if decision.decision == Decision::Permit {
                        *decision = PolicyDecision {
                            decision: Decision::RequireApproval,
                            reason_code: "exec_approval_required".into(),
                            decision_id: decision.decision_id.clone(),
                            policy_revision: decision.policy_revision.clone(),
                            constraints: Constraints {
                                approval_class: Some("exec-unclassified".into()),
                                reason: Some(format!("`{line}`: {reason}")),
                                ..decision.constraints.clone()
                            },
                        };
                    }
                    return Ok(granted());
                }
                let Some(consent) = self.consent else {
                    return Err(refuse(
                        self,
                        "exec_consent_required",
                        format!(
                            "`{line}` was not run: {reason}. Nobody is attached who can allow it. Tell the operator; they can allow it with `jani exec allow -- {}` or run it in `jani ui`. Do not try a variant of the command to get around this",
                            suggest.join(" ")
                        ),
                    ));
                };
                match consent.ask(&ConsentRequest {
                    argv: argv.clone(),
                    reason: reason.clone(),
                    suggest: suggest.clone(),
                }) {
                    ConsentAnswer::Deny => Err(refuse(
                        self,
                        "exec_operator_declined",
                        format!("the operator declined `{line}`. Do not retry it or a variant; continue without it or report what you need"),
                    )),
                    ConsentAnswer::Once => {
                        decision.reason_code = Basis::OperatorOnce.code().into();
                        Ok(granted())
                    }
                    ConsentAnswer::Session => {
                        self.session_exec.push(suggest);
                        decision.reason_code = Basis::OperatorSession.code().into();
                        Ok(granted())
                    }
                    ConsentAnswer::Always => {
                        match execpolicy::add_rule(
                            &self.cfg.state_dir,
                            true,
                            suggest,
                            &self.identity.principal,
                        ) {
                            Ok(_) => {
                                decision.reason_code = Basis::OperatorAlways.code().into();
                                Ok(granted())
                            }
                            Err(error) => Err(refuse(
                                self,
                                "exec_rule_not_saved",
                                format!("the rule could not be saved, so `{line}` was not run: {error}"),
                            )),
                        }
                    }
                }
            }
        }
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
            journal_decision::DENY,
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
            journal_decision::DENY,
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
        let Some(tool) = Tool::from_name(tool) else {
            return self.reject(
                invocation_id,
                "tool.unknown",
                format!("tool:{tool}"),
                "unknown_tool",
                format!("unknown tool: {tool}"),
            );
        };
        if let Some(schema) = crate::model::tool_schema(tool.name()) {
            if let Err(error) = crate::schema::validate(args, &schema) {
                return self.reject(
                    invocation_id,
                    action::TOOL_INVALID_ARGUMENTS,
                    format!("tool:{}", tool.name()),
                    reason::INVALID_TOOL_ARGUMENTS,
                    format!("invalid tool arguments: {error}"),
                );
            }
        }
        let call = match self.call_for(tool, args) {
            Ok(call) => call,
            Err(error) => {
                return self.reject(
                    invocation_id,
                    action::TOOL_INVALID_ARGUMENTS,
                    format!("tool:{}", tool.name()),
                    reason::INVALID_TOOL_REQUEST,
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
        if call.action == action::REPO_WRITE {
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
        let mut exec_grant: Option<exec::ExecGrant> = None;
        if matches!(tool, Tool::Command)
            && self.cfg.exec_mode == ExecMode::Smart
            && decision.decision != Decision::Deny
        {
            match self.smart_exec(invocation_id, &call, args, &mut decision) {
                Ok(grant) => exec_grant = Some(grant),
                Err(result) => return *result,
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
                                journal_decision::APPROVAL_REQUIRED,
                                journal_decision::APPROVAL_REQUIRED,
                                Classification::Internal,
                                Some(&approval_id),
                                None,
                            );
                            return ToolResult {
                                invocation_id: invocation_id.into(),
                                status: crate::types::tool_status::APPROVAL_REQUIRED.into(),
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
                journal_decision::DENY,
                reason::BUDGET_EXHAUSTED,
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
            journal_decision::PERMIT,
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

        let execution = self.execute(tool, args, &decision, exec_grant.unwrap_or_default());
        let (data, receipt, error) = match execution {
            Ok(output) => output,
            Err(error) => {
                let _ = self.audit(
                    invocation_id,
                    &call,
                    journal_decision::FAILED,
                    reason::EXECUTION_FAILED,
                    Classification::Internal,
                    approval_id.as_deref(),
                    None,
                );
                return ToolResult {
                    invocation_id: invocation_id.into(),
                    status: crate::types::tool_status::ERROR.into(),
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
                    journal_decision::FAILED,
                    reason::CONTRACT_DATA_CLASSIFICATION_DENIED,
                    classified.effective,
                    approval_id.as_deref(),
                    receipt.as_ref().map(|r| r.sandbox_id.as_str()),
                );
                return ToolResult {
                    invocation_id: invocation_id.into(),
                    status: crate::types::tool_status::DENIED.into(),
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
                    journal_decision::FAILED
                } else {
                    journal_decision::EXECUTED
                },
                if error.is_some() {
                    reason::TOOL_REPORTED_ERROR
                } else {
                    reason::OK
                },
                classified.effective,
                approval_id.as_deref(),
                receipt_id.as_deref(),
            )
            .err();

        ToolResult {
            invocation_id: invocation_id.into(),
            status: if error.is_some() {
                crate::types::tool_status::ERROR.into()
            } else {
                crate::types::tool_status::OK.into()
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
        tool: Tool,
        args: &Value,
        decision: &PolicyDecision,
        grant: exec::ExecGrant,
    ) -> Result<(Value, Option<crate::types::SandboxReceipt>, Option<String>), String> {
        let workspace = Workspace { cfg: self.cfg };
        match tool {
            Tool::SkillRead => {
                let name = get_str(args, "name")?;
                let body = self.catalog.read(name)?;
                Ok((json!({"skill": name, "contents": body}), None, None))
            }
            Tool::List => {
                let directory = args.get("directory").and_then(Value::as_str).unwrap_or(".");
                Ok((json!({"files": workspace.list(directory)?}), None, None))
            }
            Tool::Read => {
                let path = get_str(args, "path")?;
                Ok((json!({"contents": workspace.read(path)?}), None, None))
            }
            Tool::Write => {
                let path = get_str(args, "path")?;
                let content = get_str(args, "content")?;
                Ok((
                    json!({"result": workspace.write(path, content)?}),
                    None,
                    None,
                ))
            }
            Tool::NativeTool => {
                let name = get_str(args, "tool")?;
                let operation = get_str(args, "operation")?;
                let extra = args
                    .get("args")
                    .and_then(Value::as_array)
                    .ok_or("native args must be an array")?;
                if extra.len() > MAX_NATIVE_ARGS {
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
                if jani_native::privileged(name, operation) {
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
            Tool::Ask => {
                let question = question_from(args)?;
                match self.consent.and_then(|c| c.ask_question(&question)) {
                    Some(answer) => Ok((json!({"answer": answer}), None, None)),
                    None => {
                        // Nobody can answer now. The turn ends on the question and the
                        // work continues when someone does.
                        *self.asked = Some(question);
                        Ok((
                            json!({
                                "status": "waiting_for_operator",
                                "note": "Nobody is available to answer right now. Your question was recorded and this turn is ending. Do not guess: when the operator answers you will continue from here."
                            }),
                            None,
                            None,
                        ))
                    }
                }
            }
            Tool::Remember => {
                // Notes are secret-scrubbed like any model output before they are kept,
                // and what is stored is what comes back, so the model sees the redaction.
                let note = self
                    .governance
                    .inspect_model_output(get_str(args, "note")?, "memory-note")?;
                let saved = crate::memory::add(
                    &self.cfg.state_dir,
                    get_str(args, "topic")?,
                    &note,
                    &self.task_id,
                )?;
                Ok((
                    json!({"saved": true, "id": saved.id, "topic": saved.topic, "note": saved.note}),
                    None,
                    None,
                ))
            }
            Tool::Command => {
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
                    exec::run_confined(self.cfg, &argv, timeout, profile, network, &grant)?;
                Ok((value, Some(receipt), None))
            }
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
        session_exec: Vec<Vec<String>>,
        asked: Option<Question>,
        consent: Option<Box<dyn Consent>>,
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
                session_exec: Vec::new(),
                asked: None,
                consent: None,
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
                consent: self.consent.as_deref(),
                session_exec: &mut self.session_exec,
                asked: &mut self.asked,
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
            let malformed = broker.invoke("inv-1", "write", &json!({"path": "src/two.txt"}));
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
            let read = broker.invoke("inv-1", "read", &json!({"path": "src/one.txt"}));
            assert_eq!(read.status, "ok");
            let write = broker.invoke(
                "inv-2",
                "write",
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
                "write",
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
            broker.invoke("inv-1", "read", &json!({"path": "src/notes.txt"}))
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
            broker.invoke("inv-1", "read", &json!({"path": "src/pii.txt"}))
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
            let first = broker.invoke("inv-1", "read", &json!({"path": "src/one.txt"}));
            assert_eq!(first.status, "ok");
            let second = broker.invoke("inv-2", "read", &json!({"path": "src/one.txt"}));
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
            let pending = broker.invoke("inv-1", "write", &args);
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
            let executed = broker.invoke("inv-2", "write", &args);
            assert_eq!(executed.status, "ok");
            assert_eq!(executed.approval_id.as_deref(), Some(approval_id.as_str()));
        }
        {
            let mut broker = env.broker();
            let reused = broker.invoke("inv-3", "write", &args);
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
                "write",
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
            broker.invoke("inv-1", "read", &json!({"path": "src/one.txt"}))
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
                "command",
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
                "command",
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
                "write",
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
                "write",
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
                "command",
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
            broker.invoke("inv-3", "read", &json!({"path": "src/one.txt"}))
        };
        assert_eq!(read.status, "ok");
        cleanup(env);
    }

    // ---- smart command execution ---------------------------------------

    #[cfg(unix)]
    use crate::config::ExecMode;
    #[cfg(unix)]
    use crate::consent::{Consent, ConsentAnswer, ConsentRequest};
    #[cfg(unix)]
    use std::sync::{Arc, Mutex};

    #[cfg(unix)]
    /// Answers from a script and remembers what it was asked.
    struct Scripted {
        answers: Mutex<std::collections::VecDeque<ConsentAnswer>>,
        asked: Arc<Mutex<Vec<Vec<String>>>>,
    }

    #[cfg(unix)]
    impl Consent for Scripted {
        fn ask(&self, request: &ConsentRequest) -> ConsentAnswer {
            self.asked.lock().unwrap().push(request.argv.clone());
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(ConsentAnswer::Deny)
        }
    }

    #[cfg(unix)]
    fn smart_env(answers: &[ConsentAnswer]) -> (TestEnv, Arc<Mutex<Vec<Vec<String>>>>) {
        let mut env = TestEnv::new(None, &EffectiveLimits::default(), false);
        // Local, unmanaged: no signed bundle, so the operator answers at the keyboard.
        env.policy.effective.bundle = None;
        env.policy.effective.enterprise = false;
        env.cfg.exec_mode = ExecMode::Smart;
        env.cfg.allow_exec = true;
        let asked = Arc::new(Mutex::new(Vec::new()));
        env.consent = Some(Box::new(Scripted {
            answers: Mutex::new(answers.iter().copied().collect()),
            asked: asked.clone(),
        }));
        (env, asked)
    }

    #[cfg(unix)]
    fn command(broker: &mut Broker<'_>, id: &str, argv: &[&str]) -> ToolResult {
        broker.invoke(id, "command", &json!({"argv": argv, "timeout_secs": 20}))
    }

    #[cfg(unix)]
    fn permit_reasons(dir: &std::path::Path) -> Vec<String> {
        events(dir)
            .iter()
            .filter(|e| e.action == action::COMMAND_EXEC)
            .map(|e| format!("{}:{}", e.decision, e.reason_code))
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn smart_mode_runs_inspection_unasked_and_journals_why() {
        let (mut env, asked) = smart_env(&[]);
        {
            let mut broker = env.broker();
            let result = command(&mut broker, "inv-1", &["ls", "src"]);
            assert_eq!(result.status, "ok", "{:?}", result.error);
            assert!(result.data.unwrap()["stdout"]
                .as_str()
                .unwrap()
                .contains("one.txt"));
        }
        assert!(asked.lock().unwrap().is_empty(), "inspection must not ask");
        assert!(permit_reasons(&env.dir)
            .iter()
            .any(|r| r == "permit:exec_safe_inspection"));
        cleanup(env);
    }

    #[cfg(unix)]
    #[test]
    fn smart_mode_refuses_the_unknown_when_nobody_can_answer_and_does_not_run_it() {
        let (mut env, _) = smart_env(&[]);
        env.consent = None;
        {
            let mut broker = env.broker();
            let result = command(&mut broker, "inv-1", &["touch", "src/made.txt"]);
            assert_eq!(result.status, "denied");
            let error = result.error.unwrap();
            assert!(error.contains("jani exec allow"), "{error}");
            assert!(error.contains("Do not try a variant"), "{error}");
        }
        assert!(
            !env.dir.join("src/made.txt").exists(),
            "the command must not have run"
        );
        assert!(permit_reasons(&env.dir)
            .iter()
            .any(|r| r == "deny:exec_consent_required"));
        cleanup(env);
    }

    #[cfg(unix)]
    #[test]
    fn the_operators_answer_decides_and_always_becomes_a_saved_rule() {
        let (mut env, asked) = smart_env(&[
            ConsentAnswer::Deny,
            ConsentAnswer::Once,
            ConsentAnswer::Session,
            ConsentAnswer::Always,
        ]);
        {
            let mut broker = env.broker();
            // Declined: not run, and the model is told not to retry variants.
            let declined = command(&mut broker, "inv-1", &["touch", "src/a.txt"]);
            assert_eq!(declined.status, "denied");
            assert!(declined.error.unwrap().contains("declined"));
            // Once: runs, and the next identical command asks again.
            assert_eq!(
                command(&mut broker, "inv-2", &["touch", "src/b.txt"]).status,
                "ok"
            );
            // Session: runs now and for the same prefix without asking again.
            assert_eq!(
                command(&mut broker, "inv-3", &["touch", "src/c.txt"]).status,
                "ok"
            );
            assert_eq!(
                command(&mut broker, "inv-4", &["touch", "src/c.txt"]).status,
                "ok"
            );
            // Different prefix still asks (this one answers Always).
            assert_eq!(
                command(&mut broker, "inv-5", &["mkdir", "src/d"]).status,
                "ok"
            );
            // `mkdir -p src/d` is not the saved prefix `mkdir src/d`, so it asks again; the script
            // is out of answers and the default is to decline.
            assert_eq!(
                command(&mut broker, "inv-6", &["mkdir", "-p", "src/d"]).status,
                "denied"
            );
        }
        // Asked: touch a (Deny), touch b (Once), touch c (Session), mkdir d (Always), mkdir -p d (default Deny).
        // The second `touch src/c.txt` was covered by the session grant and did not ask.
        assert_eq!(
            asked.lock().unwrap().len(),
            5,
            "{:?}",
            asked.lock().unwrap()
        );
        assert!(!env.dir.join("src/a.txt").exists());
        assert!(env.dir.join("src/b.txt").exists() && env.dir.join("src/c.txt").exists());
        let rules = execpolicy::load_rules(&env.cfg.state_dir).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].argv, vec!["mkdir", "src/d"]);
        assert_eq!(rules[0].by, "p1", "the rule records who made it");
        let reasons = permit_reasons(&env.dir);
        for want in [
            "deny:exec_operator_declined",
            "permit:exec_operator_once",
            "permit:exec_operator_session",
            "permit:exec_session_grant",
            "permit:exec_operator_always",
        ] {
            assert!(
                reasons.iter().any(|r| r == want),
                "{want} missing in {reasons:?}"
            );
        }
        // A saved rule outlives the session: a fresh broker with nobody to ask still runs it.
        env.consent = None;
        env.session_exec.clear();
        {
            let mut broker = env.broker();
            assert_eq!(
                command(&mut broker, "inv-9", &["mkdir", "src/d"]).status,
                "ok"
            );
        }
        assert!(permit_reasons(&env.dir)
            .iter()
            .any(|r| r == "permit:exec_operator_rule"));
        cleanup(env);
    }

    #[cfg(unix)]
    #[test]
    fn hard_denies_are_never_put_to_the_operator() {
        let (mut env, asked) = smart_env(&[ConsentAnswer::Always, ConsentAnswer::Always]);
        {
            let mut broker = env.broker();
            for (i, argv) in [vec!["sudo", "ls"], vec!["rm", "-rf", "../.."]]
                .iter()
                .enumerate()
            {
                let result = command(&mut broker, &format!("inv-{i}"), argv);
                assert_eq!(result.status, "denied", "{argv:?}");
            }
        }
        assert!(
            asked.lock().unwrap().is_empty(),
            "a hard deny is not a question"
        );
        assert!(execpolicy::load_rules(&env.cfg.state_dir)
            .unwrap()
            .is_empty());
        cleanup(env);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_operator_rules_stop_all_commands_rather_than_reading_as_empty() {
        let (mut env, _) = smart_env(&[]);
        fs::write(env.cfg.state_dir.join("exec-rules.json"), "{ not json").unwrap();
        {
            let mut broker = env.broker();
            let result = command(&mut broker, "inv-1", &["ls"]);
            assert_eq!(result.status, "denied");
            assert!(result.error.unwrap().contains("could not be read"));
        }
        cleanup(env);
    }

    #[cfg(unix)]
    #[test]
    fn under_a_signed_policy_the_unknown_needs_a_signed_approval_not_a_keyboard_answer() {
        let (mut env, asked) = smart_env(&[ConsentAnswer::Always]);
        // Put the signed bundle back, with a rule that permits command.exec.
        env.policy = policy(false);
        if let Some(bundle) = env.policy.effective.bundle.as_mut() {
            bundle.spec.rules.push(PolicyRule {
                action: "command.exec".into(),
                resource: "**".into(),
                effect: None,
                max_risk: None,
                approval: None,
                roles: vec![],
                sandbox_profile: None,
            });
        }
        {
            let mut broker = env.broker();
            let result = command(&mut broker, "inv-1", &["touch", "src/x.txt"]);
            assert_eq!(result.status, "approval_required", "{:?}", result.error);
            assert_eq!(result.data.unwrap()["approvalClass"], "exec-unclassified");
        }
        assert!(
            asked.lock().unwrap().is_empty(),
            "keyboard consent must not stand in for a signed approval"
        );
        assert!(!env.dir.join("src/x.txt").exists());
        cleanup(env);
    }
}

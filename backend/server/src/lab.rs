//! Evidence-bound, signed and non-executing self-improvement experiments.
//! The agent cannot sign its own success or promote a candidate.
use crate::config::{Config, JResult};
use crate::{crypto, identity, mirror, policy, types};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use types::{ActionRequest, Decision, Effect, RiskTier};

const VERSION: &str = "jani-lab/v1";
const REPEATS: usize = 3;
const MAX_INPUT_BYTES: u64 = 64 * 1024;
const MIN_TESTS: u32 = 10;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Proof {
    version: String,
    tenant_id: String,
    source_run_id: String,
    finding_id: String,
    scenario_id: String,
    iteration: u8,
    revision_sha256: String,
    suite_sha256: String,
    evaluator_sha256: String,
    environment_sha256: String,
    passed: u32,
    failed: u32,
    skipped: u32,
    duration_ms: u64,
    completed: bool,
}

impl Proof {
    // Domain-separated, length-prefixed, fixed-field-order wire format.
    // No JSON canonicalization, delimiters or attacker-controlled field order.
    fn signing_bytes(&self) -> Vec<u8> {
        let parts = [
            self.version.clone(),
            self.tenant_id.clone(),
            self.source_run_id.clone(),
            self.finding_id.clone(),
            self.scenario_id.clone(),
            self.iteration.to_string(),
            self.revision_sha256.clone(),
            self.suite_sha256.clone(),
            self.evaluator_sha256.clone(),
            self.environment_sha256.clone(),
            self.passed.to_string(),
            self.failed.to_string(),
            self.skipped.to_string(),
            self.duration_ms.to_string(),
            self.completed.to_string(),
        ];
        let mut bytes = b"jani-lab/independent-trial/v1\0".to_vec();
        for part in parts {
            bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
            bytes.extend_from_slice(part.as_bytes());
        }
        bytes
    }

    fn total(&self) -> Result<u32, String> {
        self.passed
            .checked_add(self.failed)
            .and_then(|n| n.checked_add(self.skipped))
            .ok_or("trial test count overflow".into())
    }

    fn validate(&self) -> JResult<()> {
        let id = |value: &str| types::valid_id(value, 128);
        if self.version != VERSION
            || !id(&self.tenant_id)
            || !id(&self.source_run_id)
            || !id(&self.finding_id)
            || !id(&self.scenario_id)
            || !self.finding_id.starts_with("mir-")
        {
            return Err("trial has invalid version or identifiers".into());
        }
        for digest in [
            &self.revision_sha256,
            &self.suite_sha256,
            &self.evaluator_sha256,
            &self.environment_sha256,
        ] {
            if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
                return Err("trial digest must be a lower-case 64-digit SHA-256".into());
            }
        }
        if !self.completed || self.duration_ms == 0 || self.total()? < MIN_TESTS {
            return Err("incomplete trial, zero duration or insufficient tests".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SignedTrial {
    proof: Proof,
    key_id: String,
    signature: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Submission {
    version: String,
    tenant_id: String,
    source_run_id: String,
    finding_id: String,
    baseline: Vec<SignedTrial>,
    candidate: Vec<SignedTrial>,
}

fn input_json(path: &Path) -> JResult<Submission> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > MAX_INPUT_BYTES {
        return Err("Lab input must be a regular, non-symlinked JSON file of at most 64 KiB".into());
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err("Lab input changed size while being read".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("invalid Lab submission: {e}"))
}

fn validate_receipt(
    receipt: &SignedTrial,
    submission: &Submission,
    keys: &[String],
) -> JResult<()> {
    receipt.proof.validate()?;
    if receipt.proof.tenant_id != submission.tenant_id
        || receipt.proof.source_run_id != submission.source_run_id
        || receipt.proof.finding_id != submission.finding_id
        || !types::valid_id(&receipt.key_id, 64)
    {
        return Err("trial is not bound to the authorized tenant and Mirror finding".into());
    }
    crypto::verify_signature(
        keys,
        &receipt.key_id,
        &receipt.proof.signing_bytes(),
        &receipt.signature,
    )
    .map_err(|e| format!("independent trial signature rejected: {e}"))
}

fn evaluate(submission: &Submission, keys: &[String]) -> JResult<Value> {
    if submission.version != VERSION
        || !types::valid_id(&submission.tenant_id, 128)
        || !types::valid_id(&submission.source_run_id, 128)
        || !types::valid_id(&submission.finding_id, 128)
        || submission.baseline.len() != REPEATS
        || submission.candidate.len() != REPEATS
    {
        return Err("Lab requires a valid finding and exactly three baseline/candidate trials".into());
    }
    if keys.is_empty() {
        return Err("Lab requires independent verifier trust roots (--lab-key)".into());
    }
    let first = &submission.baseline[0];
    let mut unique_signatures = BTreeSet::new();
    for receipt in submission.baseline.iter().chain(&submission.candidate) {
        validate_receipt(receipt, submission, keys)?;
        let proof = &receipt.proof;
        if proof.scenario_id != first.proof.scenario_id
            || proof.suite_sha256 != first.proof.suite_sha256
            || proof.evaluator_sha256 != first.proof.evaluator_sha256
            || proof.environment_sha256 != first.proof.environment_sha256
            || receipt.key_id != first.key_id
            || !unique_signatures.insert(receipt.signature.clone())
        {
            return Err("trials must share a verifier, scenario, suite and environment with distinct signed executions".into());
        }
    }
    let base_rev = &submission.baseline[0].proof.revision_sha256;
    let candidate_rev = &submission.candidate[0].proof.revision_sha256;
    if base_rev == candidate_rev {
        return Err("baseline and candidate revisions must differ".into());
    }
    let mut fixes_failure = true;
    let mut improves_speed = true;
    let mut baseline_clean = true;
    for i in 0..REPEATS {
        let base = &submission.baseline[i].proof;
        let candidate = &submission.candidate[i].proof;
        if base.iteration as usize != i
            || candidate.iteration as usize != i
            || &base.revision_sha256 != base_rev
            || &candidate.revision_sha256 != candidate_rev
            || base.total()? != candidate.total()?
            || candidate.skipped > base.skipped
        {
            return Err("mismatched repetitions, revision or executed test coverage".into());
        }
        baseline_clean &= base.failed == 0;
        fixes_failure &= base.failed > 0 && candidate.failed == 0 && candidate.passed > base.passed;
        improves_speed &= base.failed == 0
            && candidate.failed == 0
            && (candidate.duration_ms as u128) * 100 <= (base.duration_ms as u128) * 90;
    }
    let reason = if fixes_failure {
        "stable_regression_fixed"
    } else if baseline_clean && improves_speed {
        "stable_performance_improvement"
    } else {
        "improvement_not_demonstrated"
    };
    let recommended = reason != "improvement_not_demonstrated";
    let digest = types::sha256_hex(
        serde_json::to_vec(&json!({
            "version": VERSION,
            "tenantId": submission.tenant_id,
            "sourceRunId": submission.source_run_id,
            "findingId": submission.finding_id,
            "baselineRevision": base_rev,
            "candidateRevision": candidate_rev,
            "reason": reason,
            "signatures": submission.baseline.iter().chain(&submission.candidate)
                .map(|r| r.signature.as_str()).collect::<Vec<_>>(),
        })).map_err(|e| e.to_string())?.as_slice()
    );
    Ok(json!({
        "version": VERSION,
        "tenantId": submission.tenant_id,
        "sourceRunId": submission.source_run_id,
        "findingId": submission.finding_id,
        "scenarioId": first.proof.scenario_id,
        "baselineRevision": base_rev,
        "candidateRevision": candidate_rev,
        "verifierKeyId": first.key_id,
        "repetitions": REPEATS,
        "decision": if recommended { "review_candidate" } else { "not_proven" },
        "reason": reason,
        "evidenceDigest": digest,
        "signatureVerified": true,
        "requiresHumanApproval": true,
        "autoApplied": false,
        "note": "A signed independent comparison is advisory; no source, policy, deployment or approval is modified."
    }))
}

fn authorize(cfg: &Config, sub: &str, resource: &str, effect: Effect) -> JResult<String> {
    let operator = identity::load(cfg)?;
    let verdict = policy::PolicyEngine::load(cfg)?.evaluate(
        cfg,
        &ActionRequest {
            task_id: cfg.task_id.clone().unwrap_or_else(|| "lab-review".into()),
            invocation_id: types::new_id("lab"),
            tenant_id: operator.tenant.clone(),
            principal_id: operator.principal,
            action: format!("lab.{sub}"),
            resource: resource.into(),
            effect,
            risk: RiskTier::R0,
            arguments_digest: types::sha256_hex(resource.as_bytes()),
            idempotency_key: None,
        },
    );
    if verdict.decision != Decision::Permit {
        return Err(format!("Lab authorization denied: {}", verdict.reason_code));
    }
    Ok(operator.tenant)
}

pub fn command(cfg: &Config) -> JResult<Value> {
    match cfg.sub.as_deref() {
        Some("propose") => {
            let run = cfg.positional.first().ok_or("lab propose requires RUN_ID")?;
            if !types::valid_id(run, 128) {
                return Err("invalid Lab run ID".into());
            }
            let tenant = authorize(cfg, "propose", &format!("lab:run:{run}"), Effect::Read)?;
            let events = mirror::read_journal(cfg)?;
            let report = jani_mirror::run_report(&events, &tenant, run)?;
            Ok(json!({
                "version": VERSION,
                "tenantId": tenant,
                "sourceRunId": run,
                "status": "hypotheses_only",
                "proposals": report.findings.iter().map(|f| json!({
                    "findingId":f.id,
                    "code":f.code,
                    "observation":f.observation,
                    "hypothesis":f.recommendation,
                    "evidence":f.evidence,
                    "requiredComparisons": REPEATS,
                    "evaluation": "same scenario, fixed suite/environment, independent signed test results",
                    "autoApplied": false
                })).collect::<Vec<_>>()
            }))
        }
        Some("evaluate") => {
            let path = cfg.positional.first().ok_or("lab evaluate requires FILE")?;
            let submission = input_json(Path::new(path))?;
            let tenant = authorize(
                cfg,
                "evaluate",
                &format!("lab:run:{}", submission.source_run_id),
                Effect::Read,
            )?;
            if submission.tenant_id != tenant {
                return Err("cross-tenant Lab submission refused".into());
            }
            // A signed result alone cannot invent a finding. Verify the source
            // journal and reconstruct the finding from the trusted Mirror rules.
            let events = mirror::read_journal(cfg)?;
            let report = jani_mirror::run_report(&events, &tenant, &submission.source_run_id)?;
            if !report.findings.iter().any(|f| {
                f.id == submission.finding_id && !f.evidence.is_empty()
            }) {
                return Err("Lab finding is not backed by current verified audit evidence".into());
            }
            evaluate(&submission, &cfg.lab_keys)
        }
        _ => Err("lab commands: propose RUN_ID | evaluate SIGNED_TRIALS.json".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jani_core::crypto::{public_key_hex, sign_hex};

    const SECRET: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    fn digest(ch: char) -> String { ch.to_string().repeat(64) }
    fn make(iteration: u8, candidate: bool, fix: bool) -> SignedTrial {
        let proof = Proof {
            version: VERSION.into(),
            tenant_id: "tenant-one".into(),
            source_run_id: "run-1".into(),
            finding_id: "mir-abcdefabcdefabcdefabcdef".into(),
            scenario_id: "rust-refactor".into(),
            iteration,
            revision_sha256: digest(if candidate { 'b' } else { 'a' }),
            suite_sha256: digest('c'),
            evaluator_sha256: digest('d'),
            environment_sha256: digest('e'),
            passed: if candidate || !fix { 20 } else { 19 },
            failed: if candidate || !fix { 0 } else { 1 },
            skipped: 0,
            duration_ms: if candidate { 80 } else { 100 },
            completed: true,
        };
        let signature = sign_hex(SECRET, &proof.signing_bytes()).unwrap();
        SignedTrial { proof, key_id: "verifier".into(), signature }
    }
    fn submission(fix: bool) -> Submission {
        Submission {
            version: VERSION.into(), tenant_id: "tenant-one".into(),
            source_run_id: "run-1".into(),
            finding_id: "mir-abcdefabcdefabcdefabcdef".into(),
            baseline: (0..3).map(|i| make(i, false, fix)).collect(),
            candidate: (0..3).map(|i| make(i, true, fix)).collect(),
        }
    }
    fn roots() -> Vec<String> {
        vec![format!("verifier:{}", public_key_hex(SECRET).unwrap())]
    }
    #[test]
    fn signed_regression_fix_is_only_a_review_proposal() {
        let result = evaluate(&submission(true), &roots()).unwrap();
        assert_eq!(result["decision"], "review_candidate");
        assert_eq!(result["reason"], "stable_regression_fixed");
        assert_eq!(result["autoApplied"], false);
    }
    #[test]
    fn signed_performance_comparison_never_auto_applies() {
        let result = evaluate(&submission(false), &roots()).unwrap();
        assert_eq!(result["reason"], "stable_performance_improvement");
        assert_eq!(result["requiresHumanApproval"], true);
    }
    #[test]
    fn altered_result_or_incorrect_signer_is_rejected() {
        let mut s = submission(true);
        s.candidate[2].proof.failed = 1;
        assert!(evaluate(&s, &roots()).is_err());
        let mut s = submission(true);
        s.candidate[0].key_id = "candidate".into();
        assert!(evaluate(&s, &roots()).is_err());
    }
    #[test]
    fn swapped_suite_or_missing_repetition_is_rejected() {
        let mut s = submission(false);
        s.candidate.pop();
        assert!(evaluate(&s, &roots()).is_err());
        let mut s = submission(false);
        s.candidate[1].proof.suite_sha256 = digest('f');
        let signed = s.candidate[1].proof.signing_bytes();
        s.candidate[1].signature = sign_hex(SECRET, &signed).unwrap();
        assert!(evaluate(&s, &roots()).is_err());
    }
    #[test]
    fn no_benefit_is_not_proven() {
        let mut s = submission(false);
        for c in &mut s.candidate {
            c.proof.duration_ms = 100;
            c.signature = sign_hex(SECRET, &c.proof.signing_bytes()).unwrap();
        }
        assert_eq!(evaluate(&s, &roots()).unwrap()["decision"], "not_proven");
    }
}

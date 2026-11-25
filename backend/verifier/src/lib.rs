//! Model-free work-contract verifier and signed receipt issuer.
//!
//! Evaluates structured `acceptanceChecks` declared on a `WorkContract` without
//! invoking any LLM, checks modified repository paths against `writePaths`, and
//! optionally signs the resulting `VerificationReceipt` with Ed25519.

use ed25519_dalek::{Signer, SigningKey};
use jani_core::config::validate_relative;
use jani_core::crypto::canonical_serialize;
use jani_core::policy::contract_allows_write;
use jani_core::process::{execute_with_scrubbed, unique_id, ProcessIsolation};
use jani_core::schema::glob_match;
use jani_core::types::{
    new_id, now_unix, sha256_hex, verification_receipt_path, AcceptanceCheck,
    AcceptanceCheckOutcome, VerificationReceipt, VerificationSignature, WorkContract,
};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::Command;

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("invalid target path {}", path.display()))?;
    let tmp = path.with_file_name(format!(".{name}.tmp-{}", unique_id()));
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(&tmp)
        .map_err(|e| format!("cannot stage {}: {e}", tmp.display()))?;
    f.write_all(bytes).map_err(|e| e.to_string())?;
    let _ = f.sync_all();
    drop(f);
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", path.display())
    })
}

const MAX_CONTENT_CHECK_BYTES: u64 = 4 * 1024 * 1024;
const DEFAULT_CHECK_TIMEOUT_SECS: u64 = 120;
const MAX_CHECK_TIMEOUT_SECS: u64 = 600;

pub struct VerifyOptions<'a> {
    pub root: &'a Path,
    pub state_dir: &'a Path,
    pub contract_path: &'a Path,
    pub signing_key_path: Option<&'a Path>,
    pub key_id: Option<&'a str>,
    pub verifier_id: &'a str,
    pub credential_env_names: &'a [String],
    pub out_path: Option<&'a Path>,
}

fn read_signing_key(path: &Path) -> Result<SigningKey, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("cannot read verifier signing key {}: {e}", path.display()))?;
    let bytes = hex::decode(raw.trim()).map_err(|_| "signing key must contain 64 hex chars")?;
    let array: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "signing key must decode to 32 bytes")?;
    Ok(SigningKey::from_bytes(&array))
}

fn parse_porcelain_line(line: &str) -> Option<String> {
    let body = line.get(3..)?.trim();
    if body.is_empty() {
        return None;
    }
    let path = body
        .rsplit_once(" -> ")
        .map(|(_, new)| new)
        .unwrap_or(body)
        .trim_matches('"')
        .replace('\\', "/");
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

fn verifier_isolation() -> ProcessIsolation {
    #[cfg(windows)]
    {
        ProcessIsolation::JobObject
    }
    #[cfg(not(windows))]
    {
        ProcessIsolation::Direct
    }
}

fn collect_changed_files(
    canon_root: &Path,
    credential_env_names: &[String],
) -> Result<Vec<String>, String> {
    if !canon_root.join(".git").exists() {
        return Ok(Vec::new());
    }
    let mut cmd = Command::new("git");
    cmd.args(["status", "--porcelain", "--untracked-files=all"]);
    let out = execute_with_scrubbed(
        &mut cmd,
        canon_root,
        30,
        verifier_isolation(),
        credential_env_names,
    )?;
    if out.timed_out || out.code != Some(0) {
        return Err(format!(
            "git status --porcelain failed (code={:?}): {}",
            out.code,
            out.stderr.trim()
        ));
    }
    let mut files: Vec<String> = out
        .stdout
        .lines()
        .filter_map(parse_porcelain_line)
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

fn resolve_worktree_file(canon_root: &Path, rel: &str) -> Result<std::path::PathBuf, String> {
    validate_relative(Path::new(rel))?;
    let joined = canon_root.join(rel);
    let canon =
        fs::canonicalize(&joined).map_err(|e| format!("cannot resolve path '{rel}': {e}"))?;
    if !canon.starts_with(canon_root) {
        return Err(format!("path '{rel}' escapes the worktree"));
    }
    Ok(canon)
}

fn evaluate_check(
    check: &AcceptanceCheck,
    canon_root: &Path,
    changed_files: &[String],
    credential_env_names: &[String],
) -> AcceptanceCheckOutcome {
    let fail = |detail: String| AcceptanceCheckOutcome {
        id: check.id.clone(),
        kind: check.kind.clone(),
        passed: false,
        detail,
    };
    let pass = |detail: String| AcceptanceCheckOutcome {
        id: check.id.clone(),
        kind: check.kind.clone(),
        passed: true,
        detail,
    };

    match check.kind.as_str() {
        "file_exists" => {
            let Some(rel) = check.path.as_deref() else {
                return fail("file_exists check requires 'path'".into());
            };
            match resolve_worktree_file(canon_root, rel) {
                Ok(path) if path.exists() => pass(format!("exists: {rel}")),
                Ok(_) => fail(format!("missing path: {rel}")),
                Err(err) => fail(err),
            }
        }
        "file_contains" => {
            let Some(rel) = check.path.as_deref() else {
                return fail("file_contains check requires 'path'".into());
            };
            let Some(pattern) = check.pattern.as_deref() else {
                return fail("file_contains check requires 'pattern'".into());
            };
            let path = match resolve_worktree_file(canon_root, rel) {
                Ok(p) => p,
                Err(err) => return fail(err),
            };
            let meta = match fs::metadata(&path) {
                Ok(m) if m.is_file() => m,
                Ok(_) => return fail(format!("'{rel}' is not a regular file")),
                Err(err) => return fail(format!("cannot stat '{rel}': {err}")),
            };
            if meta.len() > MAX_CONTENT_CHECK_BYTES {
                return fail(format!(
                    "'{rel}' exceeds {MAX_CONTENT_CHECK_BYTES} byte content check limit"
                ));
            }
            match fs::read_to_string(&path) {
                Ok(text) if text.contains(pattern) => {
                    pass(format!("matched required text in {rel}"))
                }
                Ok(_) => fail(format!("'{rel}' does not contain required pattern")),
                Err(err) => fail(format!("cannot read '{rel}' as UTF-8: {err}")),
            }
        }
        "max_changed_files" => {
            let Some(limit) = check.limit else {
                return fail("max_changed_files check requires 'limit'".into());
            };
            if changed_files.len() <= limit {
                pass(format!(
                    "{} changed files <= limit {limit}",
                    changed_files.len()
                ))
            } else {
                fail(format!(
                    "{} changed files exceed limit {limit}",
                    changed_files.len()
                ))
            }
        }
        "forbidden_path_glob" => {
            let Some(pattern) = check.pattern.as_deref() else {
                return fail("forbidden_path_glob check requires 'pattern'".into());
            };
            let hits: Vec<&str> = changed_files
                .iter()
                .filter(|path| glob_match(pattern, path))
                .map(String::as_str)
                .collect();
            if hits.is_empty() {
                pass(format!("no changed paths matched forbidden '{pattern}'"))
            } else {
                fail(format!(
                    "changed paths matched forbidden glob '{pattern}': {}",
                    hits.join(", ")
                ))
            }
        }
        "command" => {
            if check.argv.is_empty() {
                return fail("command check requires non-empty 'argv'".into());
            }
            if check
                .argv
                .iter()
                .any(|arg| arg.is_empty() || arg.chars().any(char::is_control))
            {
                return fail("command argv contains empty or control-character argument".into());
            }
            let timeout = check
                .timeout_secs
                .unwrap_or(DEFAULT_CHECK_TIMEOUT_SECS)
                .clamp(1, MAX_CHECK_TIMEOUT_SECS);
            let mut cmd = Command::new(&check.argv[0]);
            cmd.args(&check.argv[1..]);
            match execute_with_scrubbed(
                &mut cmd,
                canon_root,
                timeout,
                verifier_isolation(),
                credential_env_names,
            ) {
                Ok(out) if !out.timed_out && out.code == Some(0) => {
                    pass(format!("command `{}` exited 0", check.argv.join(" ")))
                }
                Ok(out) if out.timed_out => fail(format!(
                    "command `{}` timed out after {timeout}s",
                    check.argv.join(" ")
                )),
                Ok(out) => {
                    let tail = if out.stderr.trim().is_empty() {
                        out.stdout.trim()
                    } else {
                        out.stderr.trim()
                    };
                    let snippet: String = tail.chars().take(240).collect();
                    fail(format!(
                        "command `{}` failed (exit={:?}): {snippet}",
                        check.argv.join(" "),
                        out.code
                    ))
                }
                Err(err) => fail(format!("cannot spawn `{}`: {err}", check.argv[0])),
            }
        }
        other => fail(format!("unsupported acceptance check kind '{other}'")),
    }
}

/// Evaluate a signed or structured `WorkContract` against `opts.root`, persist
/// the `VerificationReceipt` in `opts.state_dir`, and optionally sign it.
pub fn verify_contract(opts: &VerifyOptions<'_>) -> Result<VerificationReceipt, String> {
    let canon_root = fs::canonicalize(opts.root)
        .map_err(|e| format!("cannot resolve repo root {}: {e}", opts.root.display()))?;
    let contract = WorkContract::load(opts.contract_path)?;
    for wp in &contract.spec.write_paths {
        validate_relative(Path::new(wp))?;
    }
    let contract_digest = sha256_hex(&canonical_serialize(&contract)?);

    let changed_files = collect_changed_files(&canon_root, opts.credential_env_names)?;
    let out_of_scope_files: Vec<String> = changed_files
        .iter()
        .filter(|path| !contract_allows_write(Some(&contract), path))
        .cloned()
        .collect();

    let mut checks = Vec::with_capacity(contract.spec.acceptance_checks.len());
    for check in &contract.spec.acceptance_checks {
        checks.push(evaluate_check(
            check,
            &canon_root,
            &changed_files,
            opts.credential_env_names,
        ));
    }

    let passed = out_of_scope_files.is_empty() && checks.iter().all(|c| c.passed);
    let mut receipt = VerificationReceipt {
        api_version: "jani.dev/v1alpha1".into(),
        kind: "VerificationReceipt".into(),
        verification_id: new_id("ver"),
        tenant_id: contract.metadata.tenant.clone(),
        task_id: contract.metadata.task_id.clone(),
        contract_digest,
        verified_at: now_unix(),
        passed,
        verifier_id: opts.verifier_id.to_string(),
        checks,
        changed_files,
        out_of_scope_files,
        signature: None,
    };

    if let Some(key_path) = opts.signing_key_path {
        let key_id = opts
            .key_id
            .filter(|id| !id.trim().is_empty())
            .ok_or("--key-id is required when signing with --signing-key")?;
        let signing = read_signing_key(key_path)?;
        let bytes = receipt.signing_bytes()?;
        let signature = hex::encode(signing.sign(&bytes).to_bytes());
        receipt.signature = Some(VerificationSignature {
            key_id: key_id.to_string(),
            value: signature,
        });
    }

    let encoded = serde_json::to_string_pretty(&receipt).map_err(|e| e.to_string())?;
    let stored_path = verification_receipt_path(opts.state_dir, &receipt.task_id)?;
    write_atomic(&stored_path, encoded.as_bytes())?;
    if let Some(out) = opts.out_path {
        write_atomic(out, encoded.as_bytes())?;
    }
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jani_core::crypto::public_key_hex;
    use jani_core::types::{ContractBudget, ContractMetadata, ContractSpec};

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("jani-verifier-{tag}-{}", new_id("t")));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn contract(task_id: &str, write_paths: &[&str], checks: Vec<AcceptanceCheck>) -> WorkContract {
        WorkContract {
            api_version: "jani.dev/v1alpha1".into(),
            kind: "WorkContract".into(),
            metadata: ContractMetadata {
                task_id: task_id.into(),
                tenant: "acme".into(),
                principal: "bot-1".into(),
                workspace: String::new(),
            },
            spec: ContractSpec {
                purpose: "testing".into(),
                capabilities: vec!["repo.read".into(), "repo.write".into()],
                write_paths: write_paths.iter().map(|s| s.to_string()).collect(),
                sandbox_profile: None,
                data_classification: None,
                budget: ContractBudget::default(),
                acceptance: vec![],
                acceptance_checks: checks,
            },
        }
    }

    #[test]
    fn passes_valid_checks_signs_and_persists_receipt() {
        let dir = temp_dir("pass");
        let repo = dir.join("repo");
        let state = dir.join("state");
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("src/lib.rs"), "pub fn answer() -> u32 { 42 }\n").unwrap();

        let secret = "55".repeat(32);
        let pub_hex = public_key_hex(&secret).unwrap();
        let key_path = dir.join("verifier.key");
        fs::write(&key_path, &secret).unwrap();

        let c = contract(
            "task-pass",
            &["src"],
            vec![
                AcceptanceCheck {
                    id: "c1".into(),
                    kind: "file_exists".into(),
                    argv: vec![],
                    path: Some("src/lib.rs".into()),
                    pattern: None,
                    limit: None,
                    timeout_secs: None,
                },
                AcceptanceCheck {
                    id: "c2".into(),
                    kind: "file_contains".into(),
                    argv: vec![],
                    path: Some("src/lib.rs".into()),
                    pattern: Some("answer()".into()),
                    limit: None,
                    timeout_secs: None,
                },
            ],
        );
        let contract_path = dir.join("contract.json");
        fs::write(&contract_path, serde_json::to_vec(&c).unwrap()).unwrap();

        let receipt = verify_contract(&VerifyOptions {
            root: &repo,
            state_dir: &state,
            contract_path: &contract_path,
            signing_key_path: Some(&key_path),
            key_id: Some("ver-1"),
            verifier_id: "jani-verifier-test",
            credential_env_names: &[],
            out_path: None,
        })
        .unwrap();

        assert!(receipt.passed);
        assert_eq!(receipt.checks.len(), 2);
        let trusted = vec![format!("ver-1:{pub_hex}")];
        assert!(receipt.verify(&trusted).is_ok());

        let mut tampered = receipt.clone();
        tampered.passed = false;
        assert!(tampered.verify(&trusted).is_err());

        let saved = verification_receipt_path(&state, "task-pass").unwrap();
        assert!(saved.is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fails_when_changed_file_is_outside_write_scope_or_check_fails() {
        let dir = temp_dir("fail");
        let repo = dir.join("repo");
        let state = dir.join("state");
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::create_dir_all(repo.join("docs")).unwrap();

        // Initialize git repo so changed/untracked files are checked against write_paths.
        let init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status();
        if init.is_ok_and(|s| s.success()) {
            fs::write(repo.join("docs/out.md"), "rogue edit").unwrap();
        }
        fs::write(repo.join("src/mod.rs"), "pub fn wrong() {}\n").unwrap();

        let c = contract(
            "task-fail",
            &["src"],
            vec![AcceptanceCheck {
                id: "contains".into(),
                kind: "file_contains".into(),
                argv: vec![],
                path: Some("src/mod.rs".into()),
                pattern: Some("expected_symbol".into()),
                limit: None,
                timeout_secs: None,
            }],
        );
        let contract_path = dir.join("contract.json");
        fs::write(&contract_path, serde_json::to_vec(&c).unwrap()).unwrap();

        let receipt = verify_contract(&VerifyOptions {
            root: &repo,
            state_dir: &state,
            contract_path: &contract_path,
            signing_key_path: None,
            key_id: None,
            verifier_id: "jani-verifier-test",
            credential_env_names: &[],
            out_path: None,
        })
        .unwrap();

        assert!(!receipt.passed);
        assert!(!receipt.checks[0].passed);
        if repo.join(".git").exists() {
            assert!(
                receipt
                    .out_of_scope_files
                    .iter()
                    .any(|p| p == "docs/out.md"),
                "{:?}",
                receipt.out_of_scope_files
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }
}

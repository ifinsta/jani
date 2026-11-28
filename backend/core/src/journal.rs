use crate::types::{new_id, now_unix, sha256_hex, AuditEvent, Classification};
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::Path;

pub const JOURNAL_FILE: &str = "journal.jsonl";

/// Canonical `EventInput.decision` values. Producers across four crates write
/// these and mirror/evidence/recovery match them, so the vocabulary lives
/// beside the event record rather than being re-typed at every call site.
/// `EventInput.policy_revision` marker for records produced by the runtime
/// itself rather than evaluated against a signed policy bundle.
pub const RUNTIME_REVISION: &str = "runtime";

pub mod decision {
    pub const STARTED: &str = "started";
    pub const COMPLETED: &str = "completed";
    pub const FAILED: &str = "failed";
    pub const DENY: &str = "deny";
    pub const PERMIT: &str = "permit";
    pub const EXECUTED: &str = "executed";
    /// Internal informational events (e.g. `agent.model` profile switches)
    /// that are neither permits nor denials of a governed action.
    pub const ALLOWED: &str = "allowed";
    /// Recorded when execution pauses on an unsigned approval grant.
    pub const APPROVAL_REQUIRED: &str = "approval_required";
}

/// Canonical `EventInput.reason_code` values for codes produced inside the
/// workspace. Codes produced by the policy engine live in `policy` itself.
pub mod reason {
    pub const RUN_STARTED: &str = "run_started";
    pub const CANCELLED_BY_OPERATOR: &str = "cancelled_by_operator";
    pub const MODEL_OR_GOVERNANCE_FAILURE: &str = "model_or_governance_failure";
    pub const EMPTY_ANSWER: &str = "empty_answer";
    pub const FINAL_ANSWER: &str = "final_answer";
    pub const STEP_LIMIT: &str = "step_limit";
    /// A turn used its step allowance, or the model kept repeating one call, and
    /// was closed with a handoff instead of being thrown away. Recorded as completed.
    pub const CHECKPOINT: &str = "step_checkpoint";
    pub const STALLED: &str = "repeated_calls";
    /// A turn ended because the agent asked the operator a question nobody could answer
    /// yet. Recorded as completed: the question is the deliverable of that turn.
    pub const NEEDS_INPUT: &str = "needs_input";
    /// Acceptance checks run by the host after a delivery attempt.
    pub const ACCEPTANCE_PASSED: &str = "acceptance_passed";
    pub const ACCEPTANCE_FAILED: &str = "acceptance_failed";
    /// How a delivery ended. Only `delivery_verified` means checks the host ran passed.
    pub const DELIVERY_VERIFIED: &str = "delivery_verified";
    pub const DELIVERY_UNVERIFIED: &str = "delivery_unverified";
    pub const DELIVERY_FAILED: &str = "delivery_failed";
    pub const OK: &str = "ok";
    pub const BUDGET_EXHAUSTED: &str = "budget_exhausted";
    pub const EXECUTION_FAILED: &str = "execution_failed";
    pub const TOOL_REPORTED_ERROR: &str = "tool_reported_error";
    pub const CONTRACT_DATA_CLASSIFICATION_DENIED: &str = "contract_data_classification_denied";
    /// Recorded on `agent.model` events when the operator switches profile.
    pub const MODEL_SWITCHED: &str = "model_switched";
    /// Broker-side rejections before a call plan exists.
    pub const UNKNOWN_TOOL: &str = "unknown_tool";
    pub const INVALID_TOOL_ARGUMENTS: &str = "invalid_tool_arguments";
    pub const INVALID_TOOL_REQUEST: &str = "invalid_tool_request";
    /// Synthetic reason attached to deny-audit fallbacks when no policy code
    /// exists yet (malformed arguments, unknown tools).
    pub const INVALID_REQUEST: &str = "invalid_request";
}

pub struct Journal {
    file: File,
    anchor: Option<File>,
    previous_digest: String,
    observed_len: u64,
}

pub struct EventInput<'a> {
    pub tenant_id: &'a str,
    pub task_id: &'a str,
    pub run_id: &'a str,
    pub invocation_id: &'a str,
    pub actor_id: &'a str,
    pub worker_id: &'a str,
    pub action: &'a str,
    pub resource_digest: &'a str,
    pub decision: &'a str,
    pub reason_code: &'a str,
    pub policy_revision: &'a str,
    pub approval_id: Option<&'a str>,
    pub sandbox_receipt_id: Option<&'a str>,
    pub data_classification: Classification,
}

impl Journal {
    pub fn open(state_dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(state_dir)
            .map_err(|e| format!("cannot create state directory {}: {e}", state_dir.display()))?;
        let path = state_dir.join(JOURNAL_FILE);
        // `read` is needed so the handle can be locked on every platform.
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("cannot open audit journal {}: {e}", path.display()))?;
        match file.lock() {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {}
            Err(e) => return Err(format!("cannot lock audit journal: {e}")),
        }
        let opened = (|| {
            let previous_digest = verify_appendable_suffix(&mut file, 0, String::new())?;
            let observed_len = file
                .metadata()
                .map_err(|e| format!("cannot inspect audit journal: {e}"))?
                .len();
            Ok::<_, String>((previous_digest, observed_len))
        })();
        let _ = file.unlock();
        let (previous_digest, observed_len) = opened?;
        Ok(Self {
            file,
            anchor: None,
            previous_digest,
            observed_len,
        })
    }

    pub fn open_with_anchor(state_dir: &Path, anchor_path: Option<&Path>) -> Result<Self, String> {
        let mut journal = Self::open(state_dir)?;
        if let Some(path) = anchor_path {
            journal.set_anchor(path)?;
        }
        Ok(journal)
    }

    pub fn set_anchor(&mut self, anchor_path: &Path) -> Result<(), String> {
        if let Ok(meta) = fs::symlink_metadata(anchor_path) {
            if meta.file_type().is_symlink() {
                return Err(format!(
                    "symlinked audit anchor refused: {}",
                    anchor_path.display()
                ));
            }
        }
        if let Some(parent) = anchor_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create audit anchor directory: {e}"))?;
            }
        }
        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts
            .open(anchor_path)
            .map_err(|e| format!("cannot open audit anchor {}: {e}", anchor_path.display()))?;
        self.anchor = Some(file);
        Ok(())
    }

    /// Append one event. Other writers (a second `Journal` in this process, or
    /// another process) may have appended since this handle last wrote, so the
    /// chain head is re-read under an exclusive file lock every time. Without
    /// that, two writers each chain to a stale head and the journal fails
    /// verification.
    pub fn record(&mut self, input: EventInput<'_>) -> Result<AuditEvent, String> {
        match self.file.lock() {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {}
            Err(e) => return Err(format!("cannot lock audit journal: {e}")),
        }
        let result = self.append_locked(input);
        let _ = self.file.unlock();
        result
    }

    fn append_locked(&mut self, input: EventInput<'_>) -> Result<AuditEvent, String> {
        // Verify newly appended bytes from the last trusted chain head using
        // the locked handle. A second handle would conflict on Windows.
        let current_len = self
            .file
            .metadata()
            .map_err(|e| format!("cannot inspect audit journal: {e}"))?
            .len();
        if current_len < self.observed_len {
            return Err("audit journal was truncated while open; refusing to append".into());
        }
        let previous_digest = if current_len > self.observed_len {
            verify_appendable_suffix(
                &mut self.file,
                self.observed_len,
                self.previous_digest.clone(),
            )?
        } else {
            self.previous_digest.clone()
        };
        let mut event = AuditEvent {
            event_version: "1.0".into(),
            event_id: new_id("evt"),
            tenant_id: input.tenant_id.into(),
            task_id: input.task_id.into(),
            run_id: input.run_id.into(),
            invocation_id: input.invocation_id.into(),
            actor_id: input.actor_id.into(),
            worker_id: input.worker_id.into(),
            action: input.action.into(),
            resource_digest: input.resource_digest.into(),
            decision: input.decision.into(),
            reason_code: input.reason_code.into(),
            policy_revision: input.policy_revision.into(),
            approval_id: input.approval_id.map(str::to_string),
            sandbox_receipt_id: input.sandbox_receipt_id.map(str::to_string),
            data_classification: input.data_classification,
            timestamp: now_unix(),
            previous_event_digest: previous_digest,
            event_digest: String::new(),
        };
        event.event_digest = digest_for(&event)?;
        let line = serde_json::to_string(&event).map_err(|e| e.to_string())?;
        self.file
            .write_all(format!("{line}\n").as_bytes())
            .and_then(|_| self.file.sync_data())
            .map_err(|e| format!("cannot append audit event: {e}"))?;
        if let Some(anchor) = self.anchor.as_mut() {
            let entry = format!(
                "{} {} {}\n",
                event.timestamp, event.event_id, event.event_digest
            );
            anchor
                .write_all(entry.as_bytes())
                .and_then(|_| anchor.sync_data())
                .map_err(|e| format!("cannot write audit anchor: {e}"))?;
        }
        self.previous_digest = event.event_digest.clone();
        self.observed_len = current_len + line.len() as u64 + 1;
        Ok(event)
    }
}

pub fn read_run(path: &Path, run_id: &str) -> Result<Vec<AuditEvent>, String> {
    let events = read_events(path)?;
    Ok(events
        .into_iter()
        .filter(|event| event.run_id == run_id)
        .collect())
}

fn event_bytes_without_digest(event: &AuditEvent) -> Result<Vec<u8>, String> {
    let mut clone = event.clone();
    clone.event_digest = String::new();
    serde_json::to_vec(&clone).map_err(|e| e.to_string())
}

fn digest_for(event: &AuditEvent) -> Result<String, String> {
    let mut bytes = event.previous_event_digest.clone().into_bytes();
    bytes.extend(event_bytes_without_digest(event)?);
    Ok(sha256_hex(&bytes))
}

fn read_complete_line(reader: &mut impl BufRead, line: &mut String) -> Result<usize, String> {
    line.clear();
    let count = reader.read_line(line).map_err(|error| error.to_string())?;
    if count != 0 && !line.ends_with('\n') {
        return Err("incomplete line".into());
    }
    Ok(count)
}

fn verify_appendable_suffix(
    file: &mut File,
    start: u64,
    mut previous: String,
) -> Result<String, String> {
    file.seek(SeekFrom::Start(start))
        .map_err(|e| format!("cannot seek audit journal: {e}"))?;
    let mut reader = BufReader::new(file);
    let mut position = start;
    let mut line = String::new();
    loop {
        let count = read_complete_line(&mut reader, &mut line)
            .map_err(|error| format!("invalid audit journal at byte {position}: {error}"))?;
        if count == 0 {
            return Ok(previous);
        }
        if !line.trim().is_empty() {
            let event: AuditEvent = serde_json::from_str(&line)
                .map_err(|e| format!("invalid audit event at byte {position}: {e}"))?;
            previous = verify_next(&event, &previous).map_err(|reason| {
                format!("audit journal integrity failed at byte {position}: {reason}")
            })?;
        }
        position += count as u64;
    }
}

fn lock_shared(file: &File) -> Result<(), String> {
    match file.lock_shared() {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => Ok(()),
        Err(e) => Err(format!("cannot lock audit journal: {e}")),
    }
}

pub fn read_events(path: &Path) -> Result<Vec<AuditEvent>, String> {
    if !path.is_file() {
        return Err(format!("journal not found: {}", path.display()));
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    lock_shared(&file)?;
    let result = read_events_from(&file);
    let _ = file.unlock();
    result
}

fn read_events_from(file: &File) -> Result<Vec<AuditEvent>, String> {
    let mut events = Vec::new();
    visit_complete_lines(file, |index, line| {
        if line.trim().is_empty() {
            return Ok(());
        }
        let event: AuditEvent = serde_json::from_str(line)
            .map_err(|e| format!("journal line {index} is invalid: {e}"))?;
        events.push(event);
        Ok(())
    })?;
    Ok(events)
}

fn visit_complete_lines(
    file: &File,
    mut visit: impl FnMut(usize, &str) -> Result<(), String>,
) -> Result<(), String> {
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    for index in 1.. {
        if read_complete_line(&mut reader, &mut line)
            .map_err(|error| format!("journal line {index} is invalid: {error}"))?
            == 0
        {
            break;
        }
        visit(index, &line)?;
    }
    Ok(())
}

pub fn read_verified_events(path: &Path) -> Result<Vec<AuditEvent>, String> {
    let events = read_events(path)?;
    if let Some((line, reason)) = first_integrity_failure(&events)? {
        return Err(format!(
            "audit journal failed integrity verification at line {line}: {reason}"
        ));
    }
    Ok(events)
}

/// Verify the whole chain while retaining only the events from one run.
pub fn read_verified_run(path: &Path, run_id: &str) -> Result<Vec<AuditEvent>, String> {
    let file = File::open(path).map_err(|error| format!("cannot open audit journal: {error}"))?;
    lock_shared(&file)?;
    let result = (|| {
        let mut selected = Vec::new();
        let mut previous = String::new();
        visit_complete_lines(&file, |index, line| {
            if line.trim().is_empty() {
                return Ok(());
            }
            let event: AuditEvent = serde_json::from_str(line)
                .map_err(|error| format!("journal line {index} is invalid: {error}"))?;
            previous = verify_next(&event, &previous).map_err(|reason| {
                format!("audit journal failed integrity verification at line {index}: {reason}")
            })?;
            if event.run_id == run_id {
                selected.push(event);
            }
            Ok(())
        })?;
        Ok(selected)
    })();
    let _ = file.unlock();
    result
}

pub fn verify_next(event: &AuditEvent, expected_prev: &str) -> Result<String, String> {
    if event.previous_event_digest != expected_prev {
        return Err("previous_event_digest does not match the preceding event".into());
    }
    if event.event_digest != digest_for(event)? {
        return Err("event digest mismatch".into());
    }
    Ok(event.event_digest.clone())
}

fn first_integrity_failure(events: &[AuditEvent]) -> Result<Option<(usize, &'static str)>, String> {
    let mut previous = String::new();
    for (index, event) in events.iter().enumerate() {
        if event.previous_event_digest != previous {
            return Ok(Some((
                index + 1,
                "previous_event_digest does not match the preceding event",
            )));
        }
        if event.event_digest != digest_for(event)? {
            return Ok(Some((index + 1, "event digest mismatch")));
        }
        previous = event.event_digest.clone();
    }
    Ok(None)
}

pub fn verify(state_dir: &Path) -> Result<Value, String> {
    let path = state_dir.join(JOURNAL_FILE);
    let events = read_events(&path)?;
    if let Some((failure_line, reason)) = first_integrity_failure(&events)? {
        return Ok(json!({
            "ok": false,
            "events": events.len(),
            "failure_line": failure_line,
            "reason": reason,
        }));
    }
    let previous = events
        .last()
        .map(|event| event.event_digest.clone())
        .unwrap_or_default();
    Ok(json!({
        "ok": true,
        "events": events.len(),
        "last_digest": previous,
        "path": path.display().to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jani-journal-{}", crate::process::unique_id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn interleaved_writers_keep_one_verifiable_chain() {
        let dir = temp_dir();
        // Two handles on one journal, as when a live session and a separate
        // governed operation both append. Each used to chain to a stale head.
        let mut session = Journal::open(&dir).unwrap();
        session.record(input("run-a", "agent.run")).unwrap();
        let mut other = Journal::open(&dir).unwrap();
        other
            .record(input("native-tool", "native.index.build"))
            .unwrap();
        session.record(input("run-a", "repo.read")).unwrap();
        other
            .record(input("native-tool", "native.index.build"))
            .unwrap();
        session.record(input("run-a", "agent.run")).unwrap();
        let check = verify(&dir).unwrap();
        assert_eq!(check["ok"], true, "{check}");
        assert_eq!(check["events"], 5);
    }

    fn input<'a>(run: &'a str, action: &'a str) -> EventInput<'a> {
        EventInput {
            tenant_id: "tenant-a",
            task_id: "task-1",
            run_id: run,
            invocation_id: "inv-1",
            actor_id: "human",
            worker_id: "worker-1",
            action,
            resource_digest: "sha256:abc",
            decision: "permit",
            reason_code: "ok",
            policy_revision: "pol-1",
            approval_id: None,
            sandbox_receipt_id: None,
            data_classification: Classification::Internal,
        }
    }

    #[test]
    fn journal_hash_chain_verifies_and_detects_tampering() {
        let dir = temp_dir();
        let mut journal = Journal::open(&dir).unwrap();
        journal.record(input("run-1", "repo.read")).unwrap();
        journal.record(input("run-1", "repo.write")).unwrap();
        drop(journal);
        assert_eq!(verify(&dir).unwrap()["ok"], true);

        let path = dir.join(JOURNAL_FILE);
        assert_eq!(read_verified_events(&path).unwrap().len(), 2);
        let mut lines = fs::read_to_string(&path).unwrap();
        lines = lines.replace("repo.write", "repo.read");
        fs::write(&path, lines).unwrap();
        assert_eq!(verify(&dir).unwrap()["ok"], false);
        assert!(read_verified_events(&path).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn verifying_one_run_still_checks_events_from_other_runs() {
        let dir = temp_dir();
        let mut journal = Journal::open(&dir).unwrap();
        journal.record(input("selected", "repo.read")).unwrap();
        journal.record(input("other", "unrelated.read")).unwrap();
        journal.record(input("selected", "repo.write")).unwrap();
        drop(journal);
        let path = dir.join(JOURNAL_FILE);
        let selected = read_verified_run(&path, "selected").unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|event| event.action.as_str())
                .collect::<Vec<_>>(),
            ["repo.read", "repo.write"]
        );
        let corrupted = fs::read_to_string(&path)
            .unwrap()
            .replace("unrelated.read", "unrelated.write");
        fs::write(&path, corrupted).unwrap();
        assert!(read_verified_run(&path, "selected")
            .unwrap_err()
            .contains("line 2"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn append_verification_handles_large_prefixes_and_trailing_blank_lines() {
        let dir = temp_dir();
        let mut journal = Journal::open(&dir).unwrap();
        let event = journal.record(input("run-1", "repo.read")).unwrap();
        drop(journal);
        let path = dir.join(JOURNAL_FILE);
        let line = fs::read_to_string(&path).unwrap();
        let mut padded = vec![b' '; 20 * 1024];
        padded.extend_from_slice(line.as_bytes());
        padded.extend_from_slice(b"\r\n \t\r\n");
        fs::write(&path, padded).unwrap();

        let mut file = OpenOptions::new().read(true).open(&path).unwrap();
        assert_eq!(
            verify_appendable_suffix(&mut file, 0, String::new()).unwrap(),
            event.event_digest
        );
        drop(file);

        let mut journal = Journal::open(&dir).unwrap();
        journal.record(input("run-2", "repo.read")).unwrap();
        drop(journal);
        assert_eq!(verify(&dir).unwrap()["ok"], true);
        assert_eq!(verify(&dir).unwrap()["events"], 2);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn writers_refuse_corrupt_prefixes_and_unverified_external_appends() {
        let dir = temp_dir();
        let path = dir.join(JOURNAL_FILE);
        let mut first = Journal::open(&dir).unwrap();
        let original = first.record(input("run-first", "repo.read")).unwrap();
        let prefix = fs::read_to_string(&path).unwrap();
        let mut another = Journal::open(&dir).unwrap();
        let appended = another.record(input("run-other", "repo.read")).unwrap();
        drop(another);
        let valid = fs::read_to_string(&path).unwrap();
        let invalid_suffix = valid.replacen(&appended.event_digest, &"0".repeat(64), 1);
        fs::write(&path, &invalid_suffix).unwrap();
        assert!(first
            .record(input("run-rejected", "repo.read"))
            .unwrap_err()
            .contains("integrity failed"));
        assert_eq!(fs::read_to_string(&path).unwrap(), invalid_suffix);

        let invalid_prefix = valid.replacen(&original.event_digest, &"0".repeat(64), 1);
        fs::write(&path, &invalid_prefix).unwrap();
        assert!(Journal::open(&dir)
            .err()
            .unwrap()
            .contains("integrity failed"));
        assert_eq!(fs::read_to_string(&path).unwrap(), invalid_prefix);

        fs::write(&path, &valid).unwrap();
        // Failed suffix validation must not change the in-memory chain head.
        first.record(input("run-recovered", "repo.read")).unwrap();
        assert_eq!(verify(&dir).unwrap()["events"], 3);
        fs::write(&path, &prefix).unwrap();
        assert!(first
            .record(input("run-truncated", "repo.read"))
            .unwrap_err()
            .contains("truncated while open"));
        assert_eq!(fs::read_to_string(&path).unwrap(), prefix);
        drop(first);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn writers_and_readers_require_a_complete_record_delimiter() {
        let dir = temp_dir();
        let path = dir.join(JOURNAL_FILE);
        let mut journal = Journal::open(&dir).unwrap();
        journal.record(input("run-1", "repo.read")).unwrap();
        drop(journal);
        let valid = fs::read_to_string(&path).unwrap();
        let missing_newline = valid.strip_suffix('\n').unwrap();
        fs::write(&path, missing_newline).unwrap();
        assert!(Journal::open(&dir)
            .err()
            .unwrap()
            .contains("incomplete line"));
        for error in [
            verify(&dir).unwrap_err(),
            read_verified_events(&path).unwrap_err(),
            read_verified_run(&path, "run-1").unwrap_err(),
        ] {
            assert!(error.contains("journal line 1 is invalid: incomplete line"));
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), missing_newline);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn large_tail_events_survive_interleaved_appends() {
        let dir = temp_dir();
        let mut first = Journal::open(&dir).unwrap();
        let digest = "x".repeat(20 * 1024);
        first
            .record(EventInput {
                resource_digest: &digest,
                ..input("run-1", "repo.read")
            })
            .unwrap();
        let mut second = Journal::open(&dir).unwrap();
        second.record(input("run-2", "repo.read")).unwrap();
        first.record(input("run-1", "repo.read")).unwrap();
        drop(second);
        drop(first);

        assert_eq!(verify(&dir).unwrap()["ok"], true);
        assert_eq!(
            read_verified_events(&dir.join(JOURNAL_FILE)).unwrap().len(),
            3
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn same_length_external_tampering_still_fails_verification_after_append() {
        let dir = temp_dir();
        let mut journal = Journal::open(&dir).unwrap();
        journal.record(input("run-1", "repo.read")).unwrap();
        let path = dir.join(JOURNAL_FILE);
        let original = fs::read_to_string(&path).unwrap();
        let tampered = original.replace("repo.read", "repo.test");
        assert_eq!(tampered.len(), original.len());
        fs::write(&path, tampered).unwrap();

        journal.record(input("run-1", "repo.read")).unwrap();
        drop(journal);
        assert_eq!(verify(&dir).unwrap()["ok"], false);
        assert!(read_verified_events(&path).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn journal_continues_the_chain_across_reopen() {
        let dir = temp_dir();
        let mut journal = Journal::open(&dir).unwrap();
        journal.record(input("run-1", "repo.read")).unwrap();
        drop(journal);
        let mut journal = Journal::open(&dir).unwrap();
        journal.record(input("run-2", "repo.read")).unwrap();
        drop(journal);
        let result = verify(&dir).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["events"], 2);
        let _ = fs::remove_dir_all(dir);
    }
}

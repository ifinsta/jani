//! Standalone native toolkit, deliberately not a second ungated execution runtime.
mod cargo_graph;
mod memory;
mod plans;
mod repo;
pub mod state;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

pub fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub(crate) fn metadata_is_link(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub(crate) fn read_worktree_file_unchecked(
    root: &Path,
    relative: &Path,
    max_bytes: u64,
) -> Result<Vec<u8>, String> {
    jani_core::config::validate_relative(relative)?;
    for component in relative.components() {
        let name = component.as_os_str().to_string_lossy();
        if jani_core::config::protected_component(&name) {
            return Err("protected native source path".into());
        }
        #[cfg(windows)]
        if name.contains(':') || name.trim_end_matches(['.', ' ']) != name {
            return Err("ambiguous Windows native source path".into());
        }
    }
    let target = root.join(relative);
    let mut cursor = root.to_path_buf();
    for component in relative.components() {
        cursor.push(component.as_os_str());
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata_is_link(&metadata) => {
                return Err("symlinked native source component refused".into())
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.to_string()),
        }
    }
    let metadata = fs::symlink_metadata(&target).map_err(|e| e.to_string())?;
    if metadata_is_link(&metadata) || !metadata.is_file() || metadata.len() > max_bytes {
        return Err("unsafe or oversized native source file".into());
    }
    let resolved = target.canonicalize().map_err(|e| e.to_string())?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    if !resolved.starts_with(root) {
        return Err("native source escapes the worktree".into());
    }
    let file = File::open(target).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max_bytes {
        return Err("native source file exceeds limit".into());
    }
    Ok(bytes)
}

impl state::State {
    pub(crate) fn read_worktree_file(
        &self,
        relative: &Path,
        max_bytes: u64,
    ) -> Result<Vec<u8>, String> {
        self.read_allowed(&relative.to_string_lossy().replace('\\', "/"), false)?;
        read_worktree_file_unchecked(&self.root, relative, max_bytes)
    }
}

pub const TOOLS: &[&str] = &[
    "index",
    "context",
    "planner",
    "pruner",
    "checkpoint",
    "verifier",
    "impact",
    "scheduler",
    "workspace",
    "evidence",
    "optimizer",
    "simulator",
];

pub fn mutation(tool: &str, operation: &str) -> bool {
    matches!(
        (tool, operation),
        ("index", "build")
            | ("context", "pin")
            | ("context", "assemble")
            | ("planner", "create")
            | ("planner", "report")
            | ("planner", "verify")
            | ("pruner", "run")
            | ("checkpoint", "create")
            | ("workspace", "snapshot")
            | ("evidence", "record")
    )
}

/// Operations reserved for operators and the governed broker: they either
/// execute outside the recorded evidence loop (`planner verify`,
/// `verifier run`) or rewind workspace state (`workspace rollback`). The
/// broker denies them to the model; this predicate is its single definition.
pub fn privileged(tool: &str, operation: &str) -> bool {
    matches!(
        (tool, operation),
        ("planner", "verify") | ("workspace", "rollback") | ("verifier", "run")
    )
}

/// Deterministic default for `simulator run` when no seed is supplied; a fixed
/// value so replayed simulator output is comparable across runs.
const SIMULATOR_DEFAULT_SEED: u64 = 20261025;
pub fn run(
    root: &Path,
    state_dir: &Path,
    tenant: &str,
    task: &str,
    tool: &str,
    args: &[String],
    human_approved: bool,
) -> Result<Value, String> {
    run_inner(
        root,
        state_dir,
        tenant,
        task,
        tool,
        args,
        human_approved,
        None,
    )
}

// Keep every broker authority input explicit at this native execution boundary.
#[allow(clippy::too_many_arguments)]
pub fn run_governed(
    root: &Path,
    state_dir: &Path,
    tenant: &str,
    task: &str,
    tool: &str,
    args: &[String],
    human_approved: bool,
    read_policy: Option<&jani_core::permissions::Policy>,
) -> Result<Value, String> {
    run_inner(
        root,
        state_dir,
        tenant,
        task,
        tool,
        args,
        human_approved,
        read_policy.cloned(),
    )
}

#[allow(clippy::too_many_arguments)]
fn run_inner(
    root: &Path,
    state_dir: &Path,
    tenant: &str,
    task: &str,
    tool: &str,
    args: &[String],
    human_approved: bool,
    read_policy: Option<jani_core::permissions::Policy>,
) -> Result<Value, String> {
    if !TOOLS.contains(&tool) {
        return Err(format!("unknown native tool: {tool}"));
    }
    let op = args.first().map(String::as_str).unwrap_or("");
    let rest = &args[usize::from(!args.is_empty())..];
    let mut s = state::State::open(root, state_dir, tenant, task)?;
    s.read_policy = read_policy;
    match tool {
        "index" => match op {
            "build" => repo::build(&mut s),
            "search" => repo::search(&s, rest.first().ok_or("index search requires query")?),
            "cargo-graph" => cargo_graph::import(
                &s,
                rest.first()
                    .ok_or("index cargo-graph requires RELATIVE_METADATA_JSON")?,
            ),
            _ => Err(
                "index commands: build, search QUERY, cargo-graph RELATIVE_METADATA_JSON".into(),
            ),
        },
        "context" => memory::context(&mut s, op, rest),
        "planner" => plans::planner(&mut s, op, rest, human_approved),
        "pruner" => memory::pruner(&mut s, op),
        "checkpoint" => plans::checkpoint(&mut s, op),
        "verifier" => repo::verifier(&s, op),
        "impact" => match op {
            "analyze" => repo::impact(&s, rest.first().ok_or("impact analyze requires PATH")?),
            _ => Err("impact commands: analyze PATH".into()),
        },
        "scheduler" => match op {
            "next" => plans::scheduler(&s),
            _ => Err("scheduler commands: next".into()),
        },
        "workspace" => repo::workspace(&mut s, op),
        "evidence" => memory::evidence(&mut s, op, rest),
        "optimizer" => match op {
            "review" => memory::optimizer(&s),
            _ => Err("optimizer commands: review".into()),
        },
        "simulator" => match op {
            "run" => {
                let seed = rest
                    .first()
                    .map(|x| x.parse::<u64>())
                    .transpose()
                    .map_err(|_| "simulator seed must be an integer")?
                    .unwrap_or(SIMULATOR_DEFAULT_SEED);
                plans::simulator(&s, seed)
            }
            _ => Err("simulator commands: run [SEED]".into()),
        },
        _ => Ok(json!({"unreachable":true})),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn all_tools_registered() {
        assert_eq!(TOOLS.len(), 12);
        let mut names = TOOLS.to_vec();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 12);
    }
    #[test]
    fn isolated_repo_index_and_plan() {
        let root = std::env::temp_dir().join(format!("jani-native-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "pub fn greeting() {}").unwrap();
        fs::write(
            root.join("plan.json"),
            r#"{"nodes":[{"id":"one","deps":[],"acceptance":"compile"}]}"#,
        )
        .unwrap();
        let state = root.join(".jani");
        let call = |tool: &str, params: Vec<&str>| {
            run(
                &root,
                &state,
                "tenant-a",
                "task-1",
                tool,
                &params.into_iter().map(str::to_string).collect::<Vec<_>>(),
                false,
            )
        };
        assert_eq!(call("index", vec!["build"]).unwrap()["updated"], 2);
        assert!(
            call("index", vec!["search", "greeting"]).unwrap()["totalMatches"]
                .as_u64()
                .unwrap()
                >= 1
        );
        assert!(call("planner", vec!["create", "plan.json"]).unwrap()["created"] == true);
        assert_eq!(call("scheduler", vec!["next"]).unwrap()["count"], 1);
        assert!(call("simulator", vec!["run", "22"]).unwrap()["actualExecutions"] == 0);
        assert!(run(
            &root,
            &state,
            "tenant-b",
            "task-1",
            "planner",
            &["inspect".into()],
            false
        )
        .is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn pruning_keeps_pinned_references_and_evidence() {
        let root =
            std::env::temp_dir().join(format!("jani-native-retention-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn keep() {}").unwrap();
        let state = root.join(".jani");
        let run_tool = |tool: &str, params: &[&str]| {
            run(
                &root,
                &state,
                "tenant",
                "job",
                tool,
                &params.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                false,
            )
        };
        run_tool("index", &["build"]).unwrap();
        run_tool("context", &["pin", "src/lib.rs"]).unwrap();
        run_tool("context", &["assemble", "keep", "500"]).unwrap();
        run_tool("evidence", &["record", "receipt-1", "src/lib.rs"]).unwrap();
        let before = run_tool("context", &["show"]).unwrap();
        assert!(!before["items"].as_array().unwrap().is_empty());
        let pruned = run_tool("pruner", &["run"]).unwrap();
        assert_eq!(pruned["authoritativeEvidencePruned"], false);
        assert_eq!(pruned["pinsKept"], 1);
        let after = run_tool("context", &["show"]).unwrap();
        assert!(after["items"].as_array().unwrap().is_empty());
        assert_eq!(
            run_tool("evidence", &["check", "receipt-1"]).unwrap()["digestMatches"],
            true
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn checkpoint_detects_plan_changes_and_cannot_resume_model() {
        let root =
            std::env::temp_dir().join(format!("jani-native-checkpoint-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("plan.json"),
            r#"{"nodes":[{"id":"work","deps":[],"acceptance":"done"}]}"#,
        )
        .unwrap();
        let dir = root.join(".jani");
        run(
            &root,
            &dir,
            "t",
            "task",
            "planner",
            &["create".into(), "plan.json".into()],
            false,
        )
        .unwrap();
        run(
            &root,
            &dir,
            "t",
            "task",
            "checkpoint",
            &["create".into()],
            false,
        )
        .unwrap();
        let r = run(
            &root,
            &dir,
            "t",
            "task",
            "checkpoint",
            &["resume".into()],
            false,
        )
        .unwrap();
        assert_eq!(r["agentExecutionResumed"], false);
        assert_eq!(r["externalSideEffectsReplayed"], false);
        fs::remove_dir_all(root).unwrap();
    }

    fn temp_tree(label: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "jani-native-{label}-{}-{stamp}",
            std::process::id()
        ))
    }

    fn symlink_dir(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).unwrap();
            true
        }
        #[cfg(windows)]
        match std::os::windows::fs::symlink_dir(target, link) {
            Ok(()) => true,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
                ) || error.raw_os_error() == Some(1314) =>
            {
                false
            }
            Err(error) => panic!("cannot create test directory symlink: {error}"),
        }
    }

    fn symlink_file(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).unwrap();
            true
        }
        #[cfg(windows)]
        match std::os::windows::fs::symlink_file(target, link) {
            Ok(()) => true,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
                ) || error.raw_os_error() == Some(1314) =>
            {
                false
            }
            Err(error) => panic!("cannot create test file symlink: {error}"),
        }
    }

    #[test]
    fn native_inputs_reject_symlinked_components_and_leaf_swaps() {
        let root = temp_tree("links");
        let external = temp_tree("external");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("swap")).unwrap();
        fs::create_dir_all(&external).unwrap();
        fs::write(root.join("src/evidence.txt"), "original evidence").unwrap();
        fs::write(root.join("swap/file.txt"), "original swapped evidence").unwrap();
        fs::write(external.join("note.txt"), "external evidence").unwrap();
        fs::write(
            external.join("plan.json"),
            r#"{"nodes":[{"id":"external","deps":[],"acceptance":"outside"}]}"#,
        )
        .unwrap();
        let state = root.join(".jani");
        let call = |tool: &str, args: &[&str]| {
            run(
                &root,
                &state,
                "tenant",
                "task",
                tool,
                &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                false,
            )
        };
        call("evidence", &["record", "receipt", "src/evidence.txt"]).unwrap();
        assert_eq!(
            call("evidence", &["check", "receipt"]).unwrap()["digestMatches"],
            true
        );
        call("evidence", &["record", "swap-receipt", "swap/file.txt"]).unwrap();
        if !symlink_dir(&external, &root.join("linked")) {
            fs::remove_dir_all(&root).unwrap();
            fs::remove_dir_all(&external).unwrap();
            return;
        }
        assert!(call("planner", &["create", "linked/plan.json"]).is_err());
        assert!(call("evidence", &["record", "linked-note", "linked/note.txt"]).is_err());
        assert!(symlink_file(
            &external.join("note.txt"),
            &root.join("leaf.txt")
        ));
        assert!(call("evidence", &["record", "leaf-note", "leaf.txt"]).is_err());
        fs::remove_dir_all(root.join("swap")).unwrap();
        assert!(symlink_dir(&external, &root.join("swap")));
        assert!(call("evidence", &["check", "swap-receipt"]).is_err());
        fs::remove_file(root.join("src/evidence.txt")).unwrap();
        assert!(symlink_file(
            &external.join("note.txt"),
            &root.join("src/evidence.txt")
        ));
        assert!(call("evidence", &["check", "receipt"]).is_err());
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&external).unwrap();
    }

    #[test]
    fn governed_reads_filter_existing_metadata_and_refuse_new_source_reads() {
        let root = temp_tree("read-policy");
        fs::create_dir_all(root.join("src/private")).unwrap();
        fs::write(root.join("src/public.rs"), "pub fn visible() {}\n").unwrap();
        fs::write(
            root.join("src/private/hidden_test.rs"),
            "pub fn hidden_symbol() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/private/plan.json"),
            r#"{"nodes":[{"id":"private","deps":[],"acceptance":"hidden"}]}"#,
        )
        .unwrap();
        let state_dir = root.join(".jani");
        let call = |tool: &str, args: &[&str]| {
            run(
                &root,
                &state_dir,
                "tenant",
                "task",
                tool,
                &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                false,
            )
        };
        call("index", &["build"]).unwrap();
        call("context", &["pin", "src/private/hidden_test.rs"]).unwrap();
        call(
            "evidence",
            &["record", "hidden-receipt", "src/private/hidden_test.rs"],
        )
        .unwrap();
        call("context", &["assemble", "hidden_symbol", "400"]).unwrap();
        assert_eq!(
            call("index", &["search", "hidden_symbol"]).unwrap()["totalMatches"],
            1
        );

        let deny = jani_core::permissions::Policy::compile(
            jani_core::permissions::Mode::Default,
            &[],
            &[],
            &["Read(./src/private/**)".to_string()],
        )
        .unwrap();
        let governed = |tool: &str, args: &[&str], policy: &jani_core::permissions::Policy| {
            run_governed(
                &root,
                &state_dir,
                "tenant",
                "task",
                tool,
                &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                false,
                Some(policy),
            )
        };
        assert_eq!(
            governed("index", &["search", "hidden_symbol"], &deny).unwrap()["totalMatches"],
            0
        );
        assert_eq!(
            governed("evidence", &["list"], &deny).unwrap()["evidence"],
            json!([])
        );
        assert_eq!(
            governed("context", &["show"], &deny).unwrap()["items"],
            json!([])
        );
        assert_eq!(
            governed("verifier", &["select"], &deny).unwrap()["candidateTests"],
            json!([])
        );
        for (tool, args) in [
            ("planner", vec!["create", "src/private/plan.json"]),
            (
                "evidence",
                vec!["record", "asked-receipt", "src/private/hidden_test.rs"],
            ),
            ("evidence", vec!["check", "hidden-receipt"]),
            ("impact", vec!["analyze", "src/private/hidden_test.rs"]),
        ] {
            assert!(governed(tool, &args, &deny).is_err(), "{tool} {args:?}");
        }
        let status = governed("workspace", &["status"], &deny).unwrap();
        assert!(!status.to_string().contains("hidden_test.rs"));
        governed("index", &["build"], &deny).unwrap();
        assert_eq!(
            governed("index", &["search", "hidden_symbol"], &deny).unwrap()["totalMatches"],
            0
        );

        let ask = jani_core::permissions::Policy::compile(
            jani_core::permissions::Mode::Default,
            &[],
            &["Read(./src/private/**)".to_string()],
            &[],
        )
        .unwrap();
        assert_eq!(
            governed("index", &["search", "hidden_symbol"], &ask).unwrap()["totalMatches"],
            0
        );
        assert!(governed(
            "evidence",
            &["record", "ask-receipt", "src/private/hidden_test.rs"],
            &ask
        )
        .is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn evidence_rechecks_size_and_protected_paths() {
        let root = temp_tree("evidence-limits");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("receipt.txt"), "small").unwrap();
        fs::write(root.join(".env"), "protected").unwrap();
        let state = root.join(".jani");
        let call = |args: &[&str]| {
            run(
                &root,
                &state,
                "tenant",
                "task",
                "evidence",
                &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                false,
            )
        };
        call(&["record", "receipt", "receipt.txt"]).unwrap();
        assert!(call(&["record", "secret", ".env"]).is_err());
        fs::write(root.join("receipt.txt"), vec![b'x'; 2_000_001]).unwrap();
        assert!(call(&["check", "receipt"]).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}

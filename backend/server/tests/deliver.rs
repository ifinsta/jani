//! `jani deliver` end to end: a real git repository, a scripted model, and the
//! host (not the model) deciding whether the work is accepted.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

mod support;
use support::provider::{say, start, write};

const BUGGY: &str = "def add(a, b):\n    return a - b\n";
const FIXED: &str = "def add(a, b):\n    return a + b\n";
const TEST: &str = "import unittest\nfrom calc import add\n\nclass T(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n\nif __name__ == '__main__':\n    unittest.main()\n";

fn run(dir: &Path, program: &str, args: &[&str]) -> String {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A git repository with a bug and a test that catches it.
fn fixture() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("jani-deliver-{}-{stamp:x}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("calc.py"), BUGGY).unwrap();
    std::fs::write(dir.join("test_calc.py"), TEST).unwrap();
    std::fs::write(dir.join(".gitignore"), ".jani/\n__pycache__/\n").unwrap();
    run(&dir, "git", &["init", "-q", "-b", "main"]);
    run(&dir, "git", &["add", "-A"]);
    run(
        &dir,
        "git",
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "initial",
        ],
    );
    dir
}

fn deliver(dir: &Path, base: &str, extra: &[&str], path_prefix: Option<&Path>) -> Output {
    let mut command = support::jani();
    command
        .args(["deliver", "make add() correct", "--repo"])
        .arg(dir)
        .args(["--state-dir"])
        .arg(dir.join(".jani"))
        .args([
            "--allow-write",
            "--write-path",
            "calc.py",
            "--write-path",
            "test_calc.py",
        ])
        .args(extra)
        .env("JANI_API_KEY", "test-key")
        .env("JANI_API_BASE", base)
        .env("JANI_MODEL", "scripted")
        // A repair rewrites calc.py within the same second and byte length; stale bytecode would hide it.
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env_remove("OPENAI_API_KEY")
        .env_remove("JANI_POLICY")
        .env_remove("JANI_IDENTITY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(prefix) = path_prefix {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![prefix.to_path_buf()];
        paths.extend(std::env::split_paths(&path));
        command.env("PATH", std::env::join_paths(paths).unwrap());
    }
    command.output().unwrap()
}

fn summary(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "no summary: {} / {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn journal(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join(".jani/journal.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn delivery_preserves_staged_changes_outside_the_write_scope() {
    let dir = fixture();
    std::fs::write(dir.join("operator-note.txt"), "separate work").unwrap();
    run(&dir, "git", &["add", "operator-note.txt"]);
    let model = start(vec![write(0, "calc.py", FIXED), say("Fixed.")]);
    let output = deliver(&dir, &model.base, &["--no-worktree"], None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        run(
            &dir,
            "git",
            &["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"]
        )
        .trim(),
        "calc.py"
    );
    assert_eq!(
        run(&dir, "git", &["diff", "--cached", "--name-only"]).trim(),
        "operator-note.txt"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn delivery_commits_a_scoped_deletion() {
    let dir = fixture();
    std::fs::remove_file(dir.join("calc.py")).unwrap();
    let model = start(vec![say("Reviewed the deletion.")]);
    let output = deliver(&dir, &model.base, &["--no-worktree"], None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        run(
            &dir,
            "git",
            &["diff-tree", "--no-commit-id", "--name-status", "-r", "HEAD"]
        )
        .trim(),
        "D\tcalc.py"
    );
    let summary = summary(&output);
    let report_path = Path::new(summary["report"].as_str().unwrap()).with_extension("json");
    let report: Value = serde_json::from_slice(&std::fs::read(report_path).unwrap()).unwrap();
    assert_eq!(report["changedFiles"], json!(["calc.py"]));
    assert!(report["commit"].is_string());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn delivery_rejects_additions_that_would_remove_an_out_of_scope_ancestor() {
    for committed in [false, true] {
        let dir = fixture();
        let parent = dir.join("component");
        std::fs::write(&parent, "operator's file").unwrap();
        run(&dir, "git", &["add", "component"]);
        if committed {
            run(
                &dir,
                "git",
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "-qm",
                    "save the operator file",
                ],
            );
        }
        let before = run(&dir, "git", &["rev-parse", "HEAD"]);
        std::fs::remove_file(&parent).unwrap();
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(parent.join("approved.txt"), "implementation").unwrap();
        let model = start(vec![say("Reviewed the file layout.")]);
        let output = deliver(
            &dir,
            &model.base,
            &["--no-worktree", "--write-path", "component/approved.txt"],
            None,
        );
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("implicitly remove a file outside its write scope: component"),
            "{error}"
        );
        assert_eq!(run(&dir, "git", &["rev-parse", "HEAD"]), before);
        assert_eq!(run(&dir, "git", &["show", ":component"]), "operator's file");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn a_staged_addition_missing_from_the_workspace_does_not_abort_delivery() {
    let dir = fixture();
    std::fs::write(dir.join("draft.txt"), "staged draft").unwrap();
    run(&dir, "git", &["add", "draft.txt"]);
    std::fs::remove_file(dir.join("draft.txt")).unwrap();
    let before = run(&dir, "git", &["rev-parse", "HEAD"]);
    let model = start(vec![say("No net change.")]);
    let output = deliver(
        &dir,
        &model.base,
        &["--no-worktree", "--write-path", "draft.txt"],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(summary(&output)["commit"].is_null());
    assert_eq!(run(&dir, "git", &["rev-parse", "HEAD"]), before);
    assert_eq!(
        run(&dir, "git", &["diff", "--cached", "--name-only"]).trim(),
        "draft.txt"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn delivery_commits_scoped_paths_from_a_nested_checkout_directory() {
    let dir = fixture();
    let nested = dir.join("nested");
    std::fs::create_dir(&nested).unwrap();
    for file in ["calc.py", "test_calc.py"] {
        std::fs::rename(dir.join(file), nested.join(file)).unwrap();
    }
    run(&dir, "git", &["add", "-A"]);
    run(
        &dir,
        "git",
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "move the package",
        ],
    );
    let model = start(vec![
        write(0, "calc.py", FIXED),
        say("Fixed the nested package."),
    ]);
    let output = deliver(
        &dir,
        &model.base,
        &[
            "--repo",
            nested.to_str().unwrap(),
            "--accept",
            "python3 -m unittest -q",
        ],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = summary(&output);
    assert_eq!(result["outcome"], "verified");
    let branch = result["branch"].as_str().unwrap();
    assert_eq!(
        run(&dir, "git", &["show", &format!("{branch}:nested/calc.py")]),
        FIXED
    );
    assert_eq!(
        std::fs::read_to_string(nested.join("calc.py")).unwrap(),
        BUGGY
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_failing_check_is_fed_back_and_the_second_attempt_is_verified_and_committed_on_a_branch() {
    let dir = fixture();
    let model = start(vec![
        write(0, "calc.py", "def add(a, b):\n    return a * b\n"), // wrong fix
        say("Fixed add()."),
        write(1, "calc.py", FIXED), // the repair
        say("Now it adds."),
    ]);
    let output = deliver(
        &dir,
        &model.base,
        &["--accept", "python3 -m unittest -q"],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = summary(&output);
    assert_eq!(result["outcome"], "verified");
    assert_eq!(result["rounds"], 2);
    // Round 1 failed and round 2 passed, both run by the host.
    let checks = result["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 2);
    assert_eq!(
        (checks[0]["round"].clone(), checks[0]["passed"].clone()),
        (json!(1), json!(false))
    );
    assert_eq!(
        (checks[1]["round"].clone(), checks[1]["passed"].clone()),
        (json!(2), json!(true))
    );
    // The model was told what failed, with the output, and told not to touch the tests.
    let requests = model.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 4);
    let third = requests[2].to_string();
    assert!(
        third.contains("did not all pass") && third.contains("python3 -m unittest -q"),
        "{third}"
    );
    assert!(
        third.contains("AssertionError") || third.contains("FAILED"),
        "the failing output reaches the model"
    );
    assert!(third.contains("not the checks or the tests"));

    // The work is on its own branch; the operator's checkout is untouched.
    let branch = result["branch"].as_str().unwrap();
    assert!(branch.starts_with("jani/"), "{branch}");
    assert_eq!(
        std::fs::read_to_string(dir.join("calc.py")).unwrap(),
        BUGGY,
        "the checkout must be untouched"
    );
    assert_eq!(
        run(&dir, "git", &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "main"
    );
    assert_eq!(
        run(&dir, "git", &["show", &format!("{branch}:calc.py")]),
        FIXED
    );
    let message = run(&dir, "git", &["log", "-1", "--format=%B", branch]);
    assert!(
        message.contains("Outcome: VERIFIED") && message.contains("run by the host"),
        "{message}"
    );
    assert_eq!(
        run(&dir, "git", &["log", "-1", "--format=%an", branch]).trim(),
        "Jani"
    );

    // The report and the journal agree.
    let report = std::fs::read_to_string(result["report"].as_str().unwrap()).unwrap();
    assert!(
        report.contains("VERIFIED: every acceptance check the host ran passed"),
        "{report}"
    );
    assert!(
        report.contains("failed (exit 1)") && report.contains("| 2 |"),
        "{report}"
    );
    let events = journal(&dir);
    let acceptance: Vec<(&str, &str)> = events
        .iter()
        .filter(|e| e["action"] == "acceptance.run")
        .map(|e| {
            (
                e["decision"].as_str().unwrap(),
                e["reason_code"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        acceptance,
        [
            ("failed", "acceptance_failed"),
            ("executed", "acceptance_passed")
        ]
    );
    assert!(events
        .iter()
        .any(|e| e["action"] == "delivery.run" && e["reason_code"] == "delivery_verified"));
    let audit = support::jani()
        .args(["audit", "verify", "--state-dir"])
        .arg(dir.join(".jani"))
        .output()
        .unwrap();
    assert!(
        audit.status.success(),
        "the chain must verify with delivery events in it"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn negative_control_a_model_that_claims_success_without_fixing_anything_fails() {
    let dir = fixture();
    let model = start(vec![
        say("All done, add() is fixed and the tests pass!"),
        say("I checked again, it is definitely fixed."),
    ]);
    let output = deliver(
        &dir,
        &model.base,
        &["--accept", "python3 -m unittest -q", "--repair-rounds", "1"],
        None,
    );
    assert!(!output.status.success(), "a false claim must not exit 0");
    let result = summary(&output);
    assert_eq!(result["outcome"], "failed");
    assert_eq!(result["rounds"], 2);
    assert!(result["checks"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["passed"] == false));
    assert_eq!(
        result["commit"],
        Value::Null,
        "nothing changed, nothing committed"
    );
    let report = std::fs::read_to_string(result["report"].as_str().unwrap()).unwrap();
    assert!(
        report.contains("FAILED: the work was not accepted"),
        "{report}"
    );
    assert!(
        report.contains("definitely fixed"),
        "the claim is quoted, not believed"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn without_acceptance_checks_the_outcome_is_unverified_never_verified() {
    let dir = fixture();
    let model = start(vec![
        write(0, "calc.py", FIXED),
        say("Changed add() to add."),
    ]);
    let output = deliver(&dir, &model.base, &[], None);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("UNVERIFIED"));
    let result = summary(&output);
    assert_eq!(result["outcome"], "unverified");
    assert!(result["checks"].as_array().unwrap().is_empty());
    let message = run(
        &dir,
        "git",
        &[
            "log",
            "-1",
            "--format=%B",
            result["branch"].as_str().unwrap(),
        ],
    );
    assert!(message.contains("Outcome: UNVERIFIED"), "{message}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn weakening_the_test_passes_the_check_but_is_called_out_for_review() {
    let dir = fixture();
    let cheat = "import unittest\n\nclass T(unittest.TestCase):\n    def test_add(self):\n        self.assertTrue(True)\n";
    let model = start(vec![
        write(0, "test_calc.py", cheat),
        say("Tests pass now."),
    ]);
    let output = deliver(
        &dir,
        &model.base,
        &["--accept", "python3 -m unittest -q"],
        None,
    );
    assert!(output.status.success());
    let result = summary(&output);
    assert_eq!(
        result["outcome"], "verified",
        "the check itself passed; the report must not hide how"
    );
    assert_eq!(result["testFilesChanged"], json!(["test_calc.py"]));
    let report = std::fs::read_to_string(result["report"].as_str().unwrap()).unwrap();
    assert!(
        report.contains("Review these: the agent changed files that look like tests"),
        "{report}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn only_the_write_scope_is_committed_and_repository_hooks_never_run() {
    let dir = fixture();
    // The acceptance command leaves a stray file in the worktree, outside the write scope.
    std::fs::write(dir.join("noise.py"), "open('stray.log', 'w').write('x')\n").unwrap();
    run(&dir, "git", &["add", "noise.py"]);
    run(
        &dir,
        "git",
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "noise",
        ],
    );
    // Installed after the fixture's own commits, so only jani's commit could trigger it.
    // A post-commit hook would run arbitrary repository code on the host.
    let hook = dir.join(".git/hooks/post-commit");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\ntouch {}\n", dir.join("HOOK_RAN").display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let model = start(vec![write(0, "calc.py", FIXED), say("done")]);
    let output = deliver(
        &dir,
        &model.base,
        &[
            "--accept",
            "python3 noise.py",
            "--accept",
            "python3 -m unittest -q",
        ],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = summary(&output);
    let branch = result["branch"].as_str().unwrap();
    let files = run(&dir, "git", &["show", "--name-only", "--format=", branch]);
    assert_eq!(
        files.trim(),
        "calc.py",
        "only the write scope is committed, never stray output"
    );
    assert!(
        !dir.join("HOOK_RAN").exists(),
        "hooks must not run on the host"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn push_and_pull_request_go_where_the_operator_pointed_and_nowhere_else() {
    let dir = fixture();
    let remote = std::env::temp_dir().join(format!("jani-deliver-remote-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&remote);
    std::fs::create_dir_all(&remote).unwrap();
    run(&remote, "git", &["init", "-q", "--bare"]);
    run(
        &dir,
        "git",
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    // A stand-in for `gh` that records how it was called.
    let bin = std::env::temp_dir().join(format!("jani-deliver-bin-{}", std::process::id()));
    std::fs::create_dir_all(&bin).unwrap();
    let log = bin.join("gh.log");
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\necho \"$@\" > {}\necho https://example.test/pull/7\n",
            log.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let model = start(vec![write(0, "calc.py", FIXED), say("done")]);
    let output = deliver(
        &dir,
        &model.base,
        &[
            "--accept",
            "python3 -m unittest -q",
            "--push",
            "origin",
            "--pr",
        ],
        Some(&bin),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = summary(&output);
    assert_eq!(result["pushed"], true);
    assert_eq!(result["pullRequest"], "https://example.test/pull/7");
    let branch = result["branch"].as_str().unwrap();
    assert!(
        run(&remote, "git", &["branch", "--list", branch]).contains(branch),
        "the branch reached the remote"
    );
    let called = std::fs::read_to_string(&log).unwrap();
    assert!(
        called.contains("pr create") && called.contains(&format!("--head {branch}")),
        "{called}"
    );
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(remote);
    let _ = std::fs::remove_dir_all(bin);
}

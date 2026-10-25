use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn workspace() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "jani-native-cli-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/hello.rs"), "pub fn hello() {}").unwrap();
    fs::write(root.join("plan.json"),json!({"nodes":[
        {"id":"inspect","deps":[],"status":"planned","acceptance":"source found","priority":3},
        {"id":"finish","deps":["inspect"],"status":"planned","acceptance":"tests passed","priority":2}
    ]}).to_string()).unwrap();
    root
}
fn invoke(root: &Path, tool: &str, args: &[&str]) -> (bool, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jani"));
    command.arg("native").arg(tool);
    for arg in args {
        command.arg(arg);
    }
    command.args([
        "--repo",
        root.to_str().unwrap(),
        "--state-dir",
        root.join(".jani").to_str().unwrap(),
        "--tenant",
        "team-1",
        "--task",
        "task-1",
    ]);
    command
        .env_remove("JANI_MODE")
        .env_remove("JANI_POLICY")
        .env_remove("JANI_IDENTITY")
        .env_remove("JANI_STATE_DIR")
        .env_remove("JANI_API_KEY")
        .env_remove("OPENAI_API_KEY");
    let output = command.output().unwrap();
    (
        output.status.success(),
        if output.status.success() {
            String::from_utf8(output.stdout).unwrap()
        } else {
            String::from_utf8_lossy(&output.stderr).into_owned()
        },
    )
}
#[test]
fn all_twelve_native_tools_have_governed_cli_paths() {
    let root = workspace();
    for (tool, args) in [
        ("index", vec!["build"]),
        ("index", vec!["search", "hello"]),
        ("context", vec!["pin", "src/hello.rs"]),
        ("context", vec!["assemble", "hello", "800"]),
        ("context", vec!["show"]),
        ("planner", vec!["create", "plan.json"]),
        ("pruner", vec!["preview"]),
        ("checkpoint", vec!["create"]),
        ("verifier", vec!["select"]),
        ("impact", vec!["analyze", "src/hello.rs"]),
        ("scheduler", vec!["next"]),
        ("workspace", vec!["status"]),
        ("evidence", vec!["record", "ev-1", "src/hello.rs"]),
        ("optimizer", vec!["review"]),
        ("simulator", vec!["run", "123"]),
    ] {
        let (ok, output) = invoke(&root, tool, &args);
        assert!(ok, "{tool} {:?} failed: {output}", args);
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert!(!parsed.is_null());
    }
    assert_eq!(
        serde_json::from_str::<Value>(&invoke(&root, "scheduler", &["next"]).1).unwrap()["count"],
        1
    );
    for (tool, args) in [
        ("planner", vec!["verify", "inspect", "--approve"]),
        ("verifier", vec!["run"]),
        ("workspace", vec!["rollback"]),
    ] {
        let (ok, _) = invoke(&root, tool, &args);
        assert!(!ok, "{tool} {:?} bypassed safeguard", args);
    }
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn native_tenant_scoping_and_mutations_are_not_implicitly_authorized() {
    let root = workspace();
    assert!(invoke(&root, "index", &["build"]).0);
    assert!(invoke(&root, "planner", &["create", "plan.json"]).0);
    let output = Command::new(env!("CARGO_BIN_EXE_jani"))
        .args([
            "native",
            "planner",
            "inspect",
            "--repo",
            root.to_str().unwrap(),
            "--state-dir",
            root.join(".jani").to_str().unwrap(),
            "--tenant",
            "team-2",
            "--task",
            "task-1",
        ])
        .env_remove("JANI_MODE")
        .env_remove("JANI_POLICY")
        .env_remove("JANI_IDENTITY")
        .output()
        .unwrap();
    assert!(!output.status.success(), "cross-tenant plan disclosed");
    fs::remove_dir_all(root).unwrap();
}

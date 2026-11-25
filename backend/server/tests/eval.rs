//! `jani eval`: the harness must tell a model that solves the task from one that
//! only looks like it did. Each case below is checked against a scripted model
//! with a known behaviour, including the ones that should score zero.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

mod support;
use support::provider::{say, start, write};

fn starter() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/starter")
}

fn temp(name: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("jani-eval-{name}-{}-{stamp:x}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn reference(case: &str, file: &str) -> String {
    std::fs::read_to_string(starter().join(case).join("reference").join(file)).unwrap()
}

/// A suite of just the named starter cases.
fn suite_of(cases: &[&str]) -> PathBuf {
    let dir = temp("suite");
    for case in cases {
        copy(&starter().join(case), &dir.join(case));
    }
    dir
}

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if src.is_dir() {
            copy(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

fn eval(args: &[&str], base: Option<&str>) -> Output {
    let mut command = support::jani();
    command
        .arg("eval")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
        .env("JANI_API_KEY", "test-key")
        .env("JANI_MODEL", "scripted");
    if let Some(base) = base {
        command.env("JANI_API_BASE", base);
    }
    command
        .env_remove("OPENAI_API_KEY")
        .env_remove("JANI_POLICY")
        .env_remove("JANI_IDENTITY");
    command.output().unwrap()
}

fn results(out: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(out.join("results.json")).unwrap()).unwrap()
}

fn run_of<'a>(results: &'a Value, case: &str) -> &'a Value {
    results["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["case"] == case)
        .unwrap()
}

#[test]
fn the_shipped_starter_suite_is_sound_without_any_model() {
    let output = eval(&["validate", starter().to_str().unwrap()], None);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report.as_array().unwrap().len(), 8);
    assert!(report
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["sound"] == true));
}

#[test]
fn validate_rejects_a_case_that_cannot_fail_or_cannot_be_solved() {
    let dir = temp("unsound");
    // Already passing: nothing to fix, and the "hidden" check tests nothing.
    std::fs::create_dir_all(dir.join("trivial/repo")).unwrap();
    std::fs::write(dir.join("trivial/repo/t.py"), "def f(): return 1\n").unwrap();
    std::fs::write(dir.join("trivial/repo/test_t.py"), "import unittest\nfrom t import f\nclass T(unittest.TestCase):\n    def test(self): self.assertEqual(f(), 1)\n").unwrap();
    std::fs::write(
        dir.join("trivial/case.json"),
        r#"{"prompt":"do nothing","accept":["python3 -m unittest -q"],"writePaths":["t.py"]}"#,
    )
    .unwrap();
    // Unsolvable: the reference does not fix it.
    copy(&starter().join("slugify"), &dir.join("broken-reference"));
    std::fs::write(
        dir.join("broken-reference/reference/slugify.py"),
        "def slugify(t):\n    return t\n",
    )
    .unwrap();
    let output = eval(&["validate", dir.to_str().unwrap()], None);
    assert!(
        !output.status.success(),
        "unsound cases must fail validation"
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let problems = |case: &str| -> String {
        report
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["case"] == case)
            .unwrap()["problems"]
            .to_string()
    };
    assert!(
        problems("trivial").contains("already pass"),
        "{}",
        problems("trivial")
    );
    assert!(
        problems("trivial").contains("no reference"),
        "{}",
        problems("trivial")
    );
    assert!(
        problems("broken-reference").contains("reference solution fails"),
        "{}",
        problems("broken-reference")
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_model_that_solves_the_tasks_scores_full_marks_and_one_that_does_nothing_scores_zero() {
    let suite = suite_of(&["chunk-last", "slugify"]);

    // Oracle: writes the reference solution, says it is done.
    let oracle = start(vec![
        write(0, "chunks.py", &reference("chunk-last", "chunks.py")),
        say("Fixed chunk()."),
        write(1, "slugify.py", &reference("slugify", "slugify.py")),
        say("Implemented slugify()."),
    ]);
    let out_a = temp("out-oracle").join("r");
    let output = eval(
        &[
            "run",
            suite.to_str().unwrap(),
            "--out",
            out_a.to_str().unwrap(),
            "--repair-rounds",
            "1",
        ],
        Some(&oracle.base),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let a = results(&out_a);
    assert_eq!(a["summary"]["solvedRate"], 1.0);
    assert_eq!(a["summary"]["solvedCleanRate"], 1.0);
    assert_eq!(a["summary"]["passAtN"], 1.0);
    assert_eq!(run_of(&a, "chunk-last")["hiddenPassed"], true);
    assert!(
        run_of(&a, "slugify")["tokensIn"].as_u64().unwrap() > 0,
        "tokens are measured, not assumed"
    );
    assert!(run_of(&a, "slugify")["toolCalls"].as_u64().unwrap() >= 1);
    let report = std::fs::read_to_string(out_a.join("report.md")).unwrap();
    assert!(report.contains("Solved 2 of 2 runs (100%)"), "{report}");

    // Negative control: claims success, changes nothing.
    let idle = start(vec![
        say("Done, it is fixed."),
        say("Still fixed."),
        say("Done, it is implemented."),
        say("Still implemented."),
    ]);
    let out_b = temp("out-idle").join("r");
    let output = eval(
        &[
            "run",
            suite.to_str().unwrap(),
            "--out",
            out_b.to_str().unwrap(),
            "--repair-rounds",
            "1",
        ],
        Some(&idle.base),
    );
    assert!(
        output.status.success(),
        "a failing model is a result, not a harness error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let b = results(&out_b);
    assert_eq!(b["summary"]["solvedRate"], 0.0);
    assert_eq!(b["summary"]["failures"]["visible_checks_failed"], 2);
    assert_eq!(run_of(&b, "chunk-last")["failure"], "visible_checks_failed");

    // Compare reads the two results and shows the gap.
    let compared = Command::new(env!("CARGO_BIN_EXE_jani"))
        .args(["eval", "compare"])
        .arg(out_b.join("results.json"))
        .arg(out_a.join("results.json"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&compared.stdout);
    assert!(text.contains("| solved | 0% | 100% | +100 pts |"), "{text}");
    for dir in [suite, out_a, out_b] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn passing_the_visible_test_by_special_casing_it_does_not_count_as_solved() {
    let suite = suite_of(&["chunk-last"]);
    // Satisfies the one test the agent can see (and nothing else).
    let overfit = "def chunk(items, n):\n    if items == [1, 2, 3, 4] and n == 2:\n        return [[1, 2], [3, 4]]\n    return []\n";
    let model = start(vec![write(0, "chunks.py", overfit), say("Fixed.")]);
    let out = temp("out-overfit").join("r");
    let output = eval(
        &[
            "run",
            suite.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ],
        Some(&model.base),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let r = results(&out);
    let run = run_of(&r, "chunk-last");
    assert_eq!(run["outcome"], "verified", "the visible checks did pass");
    assert_eq!(run["hiddenPassed"], false, "the hidden checks caught it");
    assert_eq!(run["solved"], false);
    assert_eq!(run["failure"], "passed_visible_failed_hidden");
    let report = std::fs::read_to_string(out.join("report.md")).unwrap();
    assert!(report.contains("passed_visible_failed_hidden"), "{report}");
    for dir in [suite, out] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn repeats_measure_variation_and_a_used_output_directory_is_refused() {
    let suite = suite_of(&["slugify"]);
    // Run 1 solves it; run 2 does nothing.
    let model = start(vec![
        write(0, "slugify.py", &reference("slugify", "slugify.py")),
        say("done"),
        say("done"),
        say("done again"),
    ]);
    let out = temp("out-repeat").join("r");
    let args = [
        "run",
        suite.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--repeat",
        "2",
        "--repair-rounds",
        "1",
    ];
    let output = eval(&args, Some(&model.base));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let r = results(&out);
    assert_eq!(
        r["summary"]["solvedRate"], 0.5,
        "pass@1 is the mean over repeats"
    );
    assert_eq!(
        r["summary"]["passAtN"], 1.0,
        "pass@n is whether any run solved it"
    );
    assert_eq!(r["runs"].as_array().unwrap().len(), 2);
    let again = eval(&args, Some(&model.base));
    assert!(
        !again.status.success()
            && String::from_utf8_lossy(&again.stderr).contains("already holds results")
    );
    for dir in [suite, out] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

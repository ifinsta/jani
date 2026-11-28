use super::{ensure_active, git, git_command, git_ok, git_result, worktree};
use crate::config::JResult;
use crate::process::execute;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

#[derive(Deserialize, Serialize)]
pub(super) struct Commit {
    pub parent: String,
    pub oid: String,
    pub paths: Vec<String>,
    index_before: String,
    index_after: String,
}

struct ScratchIndex(PathBuf);

impl Drop for ScratchIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn run(root: &Path, hooks: &Path, index: Option<&ScratchIndex>, args: &[&str]) -> JResult<String> {
    let mut command = git_command(hooks, args);
    command
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1");
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", &index.0);
    }
    git_result(execute(&mut command, root, 120)?, args)
}

fn pathspecs(paths: impl Iterator<Item = impl AsRef<str>>) -> Vec<String> {
    paths
        .map(|path| format!(":(top,literal){}", path.as_ref()))
        .collect()
}

fn index_entries(
    root: &Path,
    hooks: &Path,
    index: Option<&ScratchIndex>,
    paths: &[String],
) -> JResult<String> {
    let specs = pathspecs(paths.iter());
    let mut args = vec!["ls-files", "--stage", "-z", "--"];
    args.extend(specs.iter().map(String::as_str));
    let result = run(root, hooks, index, &args)?;
    worktree::check_git_paths(&result)?;
    let selected: BTreeSet<_> = paths.iter().map(String::as_str).collect();
    let mut exact = String::new();
    for entry in result.split_terminator('\0') {
        let (fields, path) = entry.split_once('\t').ok_or("invalid Git index entry")?;
        if selected.contains(path) {
            if !fields.ends_with(" 0") {
                return Err("cannot finalize a delivery with unmerged index entries".into());
            }
            exact.push_str(entry);
            exact.push('\0');
        }
    }
    Ok(exact)
}

pub(super) fn prepare(
    root: &Path,
    hooks: &Path,
    state_dir: &Path,
    changes: &[worktree::Change],
    message: &str,
    cancel: &AtomicBool,
) -> JResult<Option<Commit>> {
    ensure_active(cancel)?;
    let parent = git_ok(root, hooks, &["rev-parse", "--verify", "HEAD^{commit}"])?
        .trim()
        .to_string();
    if !worktree::commit_id(&parent) {
        return Err("Git did not return the parent commit ID".into());
    }
    let index = ScratchIndex(state_dir.join(crate::types::new_id("candidate-index")));
    run(root, hooks, Some(&index), &["read-tree", &parent])?;
    let paths = pathspecs(changes.iter().map(|change| &change.path));
    let mut args = vec!["add", "--force", "--all", "--"];
    args.extend(paths.iter().map(String::as_str));
    run(root, hooks, Some(&index), &args)?;
    let actual = run(
        root,
        hooks,
        Some(&index),
        &[
            "diff",
            "--cached",
            "--name-only",
            "--no-renames",
            "--no-ext-diff",
            "--no-relative",
            "--ignore-submodules=none",
            "-z",
            &parent,
        ],
    )?;
    worktree::check_git_paths(&actual)?;
    let allowed: BTreeSet<_> = changes.iter().map(|change| change.path.as_str()).collect();
    let paths: Vec<String> = actual.split_terminator('\0').map(str::to_string).collect();
    if paths.is_empty() {
        return Ok(None);
    }
    if paths.iter().any(|path| !allowed.contains(path.as_str())) {
        return Err(
            "the candidate commit changes paths outside the selected delivery scope".into(),
        );
    }
    let tree = run(root, hooks, Some(&index), &["write-tree"])?;
    let index_before = index_entries(root, hooks, None, &paths)?;
    let index_after = index_entries(root, hooks, Some(&index), &paths)?;
    ensure_active(cancel)?;
    let oid = run(
        root,
        hooks,
        None,
        &[
            "-c",
            "user.name=Jani",
            "-c",
            "user.email=jani@localhost",
            "commit-tree",
            "--no-gpg-sign",
            tree.trim(),
            "-p",
            &parent,
            "-m",
            message,
        ],
    )?
    .trim()
    .to_string();
    if !worktree::commit_id(&oid) {
        return Err("Git did not return the candidate commit ID".into());
    }
    Ok(Some(Commit {
        parent,
        oid,
        paths,
        index_before,
        index_after,
    }))
}

impl Commit {
    pub(super) fn apply(
        &self,
        workspace: &worktree::Workspace,
        hooks: &Path,
        check_claim: &dyn Fn() -> JResult<()>,
    ) -> JResult<()> {
        let root = workspace
            .linked_root()
            .ok_or("the saved commit requires a linked worktree")?;
        let branch = workspace
            .branch
            .as_deref()
            .ok_or("the saved commit has no branch")?;
        let reference = format!("refs/heads/{branch}");
        if !worktree::commit_id(&self.parent)
            || !worktree::commit_id(&self.oid)
            || self.paths.is_empty()
        {
            return Err("invalid saved delivery commit".into());
        }
        check_claim()?;
        workspace.verify_scope(hooks)?;
        let symbolic = git(root, hooks, &["symbolic-ref", "-q", &reference])?;
        if symbolic.timed_out || symbolic.code != Some(1) {
            return Err("the delivery branch is unavailable or symbolic".into());
        }
        let head = git_ok(root, hooks, &["rev-parse", "--verify", &reference])?;
        let head = head.trim();
        if head != self.parent && head != self.oid {
            return Err("the delivery branch moved; reconcile it before retrying".into());
        }
        let index = index_entries(root, hooks, None, &self.paths)?;
        if index != self.index_before && index != self.index_after {
            return Err(
                "the selected Git index entries changed; reconcile them before retrying".into(),
            );
        }
        if head == self.parent {
            check_claim()?;
            git_ok(
                root,
                hooks,
                &[
                    "update-ref",
                    "--no-deref",
                    &reference,
                    &self.oid,
                    &self.parent,
                ],
            )?;
        }
        if index != self.index_after {
            check_claim()?;
            let entries: Vec<_> = self
                .index_after
                .split_terminator('\0')
                .map(|entry| {
                    let (fields, path) =
                        entry.split_once('\t').ok_or("invalid saved index entry")?;
                    let (mode, oid) = fields
                        .strip_suffix(" 0")
                        .and_then(|fields| fields.split_once(' '))
                        .ok_or("invalid saved index object")?;
                    Ok::<_, String>((mode, oid, path))
                })
                .collect::<JResult<_>>()?;
            let after_paths: BTreeSet<_> = entries.iter().map(|(_, _, path)| *path).collect();
            let deletions: Vec<_> = self
                .paths
                .iter()
                .filter(|path| !after_paths.contains(path.as_str()))
                .map(|path| format!("./{path}"))
                .collect();
            let mut args = vec!["update-index", "--add", "--no-replace"];
            if !deletions.is_empty() {
                args.push("--force-remove");
                args.extend(deletions.iter().map(String::as_str));
                args.push("--no-force-remove");
            }
            for (mode, oid, path) in entries {
                args.extend(["--cacheinfo", mode, oid, path]);
            }
            run(root, hooks, None, &args)?;
            if index_entries(root, hooks, None, &self.paths)? != self.index_after {
                return Err("Git did not restore the recorded delivery index entries".into());
            }
        }
        Ok(())
    }
}

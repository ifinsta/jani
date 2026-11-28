use super::{git, git_ok};
use crate::config::JResult;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

pub(super) struct Workspace {
    pub root: PathBuf,
    pub branch: Option<String>,
    pub base: Option<String>,
    pub in_git: bool,
    linked: Option<LinkedWorktree>,
}

struct LinkedWorktree {
    tree: PathBuf,
    repository: PathBuf,
}

impl Workspace {
    pub fn linked_root(&self) -> Option<&Path> {
        self.linked.as_ref().map(|linked| linked.tree.as_path())
    }

    pub fn verify_scope(&self, hooks: &Path) -> JResult<()> {
        if let (Some(linked), Some(branch)) = (&self.linked, &self.branch) {
            verify_scope(&self.root, hooks, &linked.tree, &linked.repository, branch)?;
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
struct Record {
    branch: String,
    base: Option<String>,
    prefix: PathBuf,
    #[serde(default)]
    repository: Option<PathBuf>,
    #[serde(default)]
    pending: bool,
}

fn metadata(path: &Path) -> JResult<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("cannot inspect {}: {err}", path.display())),
    }
}

fn load_record(path: &Path) -> JResult<Option<Record>> {
    let Some(metadata) = metadata(path)? else {
        return Ok(None);
    };
    if !metadata.is_file() {
        return Err(format!("invalid workspace metadata at {}", path.display()));
    }
    let bytes = fs::read(path).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|err| format!("invalid workspace metadata at {}: {err}", path.display()))
}

fn save_record(dir: &Path, path: &Path, record: &Record) -> JResult<()> {
    let temporary = dir.join(format!("workspace-{}.tmp", crate::types::new_id("write")));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        let bytes = serde_json::to_vec(record)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        fs::File::open(dir)?.sync_all()?;
        Ok::<(), std::io::Error>(())
    })();
    if let Err(err) = result {
        let _ = fs::remove_file(temporary);
        return Err(format!(
            "cannot persist workspace metadata at {}: {err}",
            path.display()
        ));
    }
    Ok(())
}

fn canonical(path: &Path) -> JResult<PathBuf> {
    path.canonicalize()
        .map_err(|err| format!("cannot resolve {}: {err}", path.display()))
}

fn git_path(root: &Path, hooks: &Path, arg: &str) -> JResult<PathBuf> {
    let path = git_ok(root, hooks, &["rev-parse", arg])?;
    canonical(&root.join(path.trim_end_matches(['\r', '\n'])))
}

pub(super) fn commit_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn check_git_paths(stdout: &str) -> JResult<()> {
    if stdout.len() >= crate::process::EXCERPT_LIMIT
        || (!stdout.is_empty() && !stdout.ends_with('\0'))
        || stdout.contains('\u{fffd}')
    {
        return Err("Git path output is incomplete or cannot be represented as UTF-8".into());
    }
    Ok(())
}

pub(super) struct Change {
    pub path: String,
    pub untracked: bool,
    addition: bool,
}

impl Change {
    pub fn pathspec(&self) -> String {
        format!(":(top,literal){}", self.path)
    }
}

pub(super) fn scoped_changes(root: &Path, hooks: &Path, paths: &[PathBuf]) -> JResult<Vec<Change>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec![
        "--literal-pathspecs",
        "status",
        "--porcelain=v1",
        "-z",
        "--no-renames",
        "--untracked-files=all",
        "--",
    ];
    for path in paths {
        args.push(path.to_str().ok_or("write scope is not valid UTF-8")?);
    }
    let stdout = git_ok(root, hooks, &args)?;
    check_git_paths(&stdout)?;
    let mut changes = BTreeMap::new();
    for entry in stdout.split_terminator('\0') {
        let path = entry
            .get(3..)
            .filter(|path| !path.is_empty() && entry.as_bytes().get(2) == Some(&b' '))
            .ok_or("Git returned an invalid status entry")?;
        // An index-only addition erased from the working directory has no net change.
        if entry.starts_with("AD ") {
            continue;
        }
        let untracked = entry.starts_with("?? ");
        let change = changes.entry(path).or_insert_with(|| Change {
            path: path.to_string(),
            untracked: false,
            addition: false,
        });
        change.untracked |= untracked;
        change.addition |= untracked || entry.starts_with('A');
    }
    Ok(changes.into_values().collect())
}

/// Git removes an indexed file implicitly when an addition replaces it with a
/// directory. Refuse the operation unless that removal was selected explicitly.
pub(super) fn check_implicit_deletions(
    root: &Path,
    hooks: &Path,
    changes: &[Change],
) -> JResult<()> {
    let selected: BTreeSet<_> = changes.iter().map(|change| change.path.as_str()).collect();
    let ancestors: BTreeSet<_> = changes
        .iter()
        .filter(|change| change.addition)
        .flat_map(|change| {
            change
                .path
                .match_indices('/')
                .map(|(end, _)| &change.path[..end])
        })
        .filter(|path| !selected.contains(path))
        .collect();
    if ancestors.is_empty() {
        return Ok(());
    }
    let pathspecs: Vec<_> = ancestors
        .iter()
        .map(|path| format!(":(top,literal){path}"))
        .collect();
    for staged in [false, true] {
        let mut args = vec![
            "diff",
            "--name-only",
            "--no-renames",
            "--no-ext-diff",
            "--no-relative",
            "--diff-filter=D",
            "-z",
        ];
        if staged {
            args.push("--cached");
        }
        args.push("--");
        args.extend(pathspecs.iter().map(String::as_str));
        let stdout = git_ok(root, hooks, &args)?;
        check_git_paths(&stdout)?;
        if let Some(path) = stdout
            .split_terminator('\0')
            .find(|path| ancestors.contains(path))
        {
            return Err(format!(
                "delivery would implicitly remove a file outside its write scope: {path}"
            ));
        }
    }
    Ok(())
}

fn scoped_root(tree: &Path, prefix: &Path) -> JResult<PathBuf> {
    let expected = canonical(tree.parent().ok_or("worktree has no parent")?)?.join("tree");
    if canonical(tree)? != expected {
        return Err("delivery tree does not resolve to its own directory".into());
    }
    let scoped = expected.join(prefix);
    if canonical(&scoped)? != scoped || !scoped.is_dir() {
        return Err("delivery scope does not resolve to its own directory".into());
    }
    Ok(scoped)
}

fn verify_scope(
    scope: &Path,
    hooks: &Path,
    tree: &Path,
    repository: &Path,
    branch: &str,
) -> JResult<()> {
    if canonical(scope)? != scope
        || git_path(scope, hooks, "--show-toplevel")? != tree
        || git_path(scope, hooks, "--git-common-dir")? != repository
        || git_ok(scope, hooks, &["symbolic-ref", "HEAD"])?.trim_end()
            != format!("refs/heads/{branch}")
    {
        return Err(
            "delivery directory no longer belongs to the expected branch and worktree".into(),
        );
    }
    Ok(())
}

fn verify_registration(root: &Path, hooks: &Path, tree: &Path, branch: &str) -> JResult<()> {
    let (list, delimiter, field_separator) =
        match git_ok(root, hooks, &["worktree", "list", "--porcelain", "-z"]) {
            Ok(list) => {
                check_git_paths(&list)?;
                (list, "\0\0", '\0')
            }
            Err(_) => {
                let list = git_ok(root, hooks, &["worktree", "list", "--porcelain"])?;
                if list.len() >= crate::process::EXCERPT_LIMIT || list.contains('\u{fffd}') {
                    return Err(
                        "Git worktree list is incomplete or cannot be represented as UTF-8".into(),
                    );
                }
                (list.replace("\r\n", "\n"), "\n\n", '\n')
            }
        };
    let expected_ref = format!("refs/heads/{branch}");
    let canonical_tree = canonical(tree)?;
    let has_registration = list.split(delimiter).any(|entry| {
        let mut fields = entry.split(field_separator);
        let Some(path) = fields
            .next()
            .and_then(|field| field.strip_prefix("worktree "))
        else {
            return false;
        };
        fields.any(|field| field.strip_prefix("branch ") == Some(expected_ref.as_str()))
            && Path::new(path)
                .canonicalize()
                .is_ok_and(|path| path == canonical_tree)
    });
    if !has_registration {
        return Err(format!(
            "delivery tree is not the registered worktree for branch '{branch}'"
        ));
    }
    Ok(())
}

pub(super) fn prepare(
    root: &Path,
    dir: &Path,
    hooks: &Path,
    branch: String,
    use_worktree: bool,
    resume: bool,
) -> JResult<Workspace> {
    let path = dir.join("workspace.json");
    let tree = dir.join("tree");
    let tree_present = metadata(&tree)?.is_some();
    let existing = load_record(&path)?;
    let recovering = tree_present || existing.is_some();
    if recovering && (!resume || !use_worktree) {
        return Err("delivery workspace already exists and requires a retry".into());
    }
    let inside = git(root, hooks, &["rev-parse", "--is-inside-work-tree"])
        .is_ok_and(|out| !out.timed_out && out.code == Some(0) && out.stdout.trim() == "true");
    let head = if inside {
        git(root, hooks, &["rev-parse", "--verify", "HEAD^{commit}"])
            .ok()
            .filter(|out| !out.timed_out && out.code == Some(0))
            .map(|out| out.stdout.trim().to_string())
            .filter(|value| commit_id(value))
    } else {
        None
    };
    if !use_worktree || !inside || (head.is_none() && !recovering) {
        if recovering {
            return Err("cannot recover an isolated delivery without its Git repository".into());
        }
        return Ok(Workspace {
            root: root.to_path_buf(),
            branch: None,
            base: head,
            in_git: inside,
            linked: None,
        });
    }

    let repository = git_path(root, hooks, "--git-common-dir")?;
    let top = git_path(root, hooks, "--show-toplevel")?;
    let root = canonical(root)?;
    let prefix = root
        .strip_prefix(&top)
        .map_err(|_| "delivery root is outside the Git checkout")?;
    if git_ok(&root, hooks, &["check-ref-format", "--branch", &branch])?.trim_end() != branch {
        return Err(format!("'{branch}' is not a literal branch name"));
    }

    let mut record = if let Some(record) = existing {
        if record.branch != branch
            || record.prefix != prefix
            || record
                .repository
                .as_ref()
                .is_some_and(|saved| saved != &repository)
            || record.base.as_deref().is_some_and(|base| !commit_id(base))
        {
            return Err("workspace metadata does not match this delivery".into());
        }
        record
    } else {
        Record {
            branch: branch.clone(),
            base: if tree_present { None } else { head },
            prefix: prefix.to_path_buf(),
            repository: Some(repository.clone()),
            pending: true,
        }
    };

    let scope = if tree_present {
        let scope = scoped_root(&tree, prefix)?;
        verify_registration(&root, hooks, &tree, &branch)?;
        if record.pending {
            let clean = git_ok(
                &tree,
                hooks,
                &[
                    "--no-optional-locks",
                    "status",
                    "--porcelain",
                    "--untracked-files=no",
                ],
            )?
            .is_empty();
            let head = git_ok(&tree, hooks, &["rev-parse", "--verify", "HEAD^{commit}"])?;
            if !clean
                || record
                    .base
                    .as_deref()
                    .is_some_and(|base| base != head.trim())
            {
                return Err(
                    "interrupted checkout differs from its starting state and needs review".into(),
                );
            }
        }
        scope
    } else {
        if !record.pending || record.repository.as_ref() != Some(&repository) {
            return Err("the original delivery worktree is missing and needs review".into());
        }
        let base = record
            .base
            .as_deref()
            .ok_or("workspace intent has no starting commit")?;
        save_record(dir, &path, &record)?;
        let tree_path = tree.to_str().ok_or("worktree path is not valid UTF-8")?;
        git_ok(
            &top,
            hooks,
            &["worktree", "add", "-b", &branch, tree_path, base],
        )?;
        scoped_root(&tree, prefix)?
    };
    let workspace = Workspace {
        root: scope,
        branch: Some(branch),
        base: record.base.clone(),
        in_git: true,
        linked: Some(LinkedWorktree {
            tree: canonical(&tree)?,
            repository,
        }),
    };
    workspace.verify_scope(hooks)?;
    if record.pending {
        record.pending = false;
        save_record(dir, &path, &record)?;
    }
    Ok(workspace)
}

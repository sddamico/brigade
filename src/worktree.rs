//! Worktrees give each writer on a team its own checkout, on a branch of its
//! own, so their changes can be reviewed and merged one at a time.

use crate::settings::Settings;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    /// The main checkout the worktree belongs to.
    pub repo: PathBuf,
    /// The commit the run started from.
    pub base: String,
    /// The main checkout had uncommitted changes, which the worktree doesn't
    /// have, since it starts from HEAD.
    #[serde(default)]
    pub left_out_uncommitted: bool,
    /// The run continues in a worktree an earlier run made, so it isn't this
    /// run's to remove.
    #[serde(default)]
    pub reused: bool,
}

/// What became of a run's worktree once the run ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum WorktreeState {
    Committed {
        commit: String,
    },
    Unchanged,
    Uncommitted {
        error: String,
    },
    /// A failed or cancelled run that left nothing behind: worktree and branch removed.
    Removed,
    /// A failed or cancelled run's worktree, kept because it has changes or
    /// belongs to an earlier run.
    Kept,
}

/// Creates a worktree on a new `brigade/<run id>` branch from the HEAD of the
/// repo containing `cwd`, and returns it with the directory to run in: the same
/// subdirectory of the worktree that `cwd` is of the repo.
pub fn create(settings: &Settings, cwd: &Path, run_id: &str) -> Result<(Worktree, PathBuf), String> {
    let root =
        toplevel(cwd).ok_or_else(|| format!("--worktree needs a git repo, and {} isn't in one", cwd.display()))?;
    if git(&root, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_err() {
        return Err(format!(
            "--worktree needs a commit to branch from, and {} has none yet",
            root.display()
        ));
    }
    let prefix = git(cwd, &["rev-parse", "--show-prefix"])?.trim().to_string();
    let left_out_uncommitted = !git(&root, &["status", "--porcelain"])?.trim().is_empty();
    let repo = main_checkout(&root)?;
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let path = settings.worktrees_dir().join(name).join(run_id);
    let branch = format!("brigade/{run_id}");
    let path_arg = path.to_string_lossy();
    git(&root, &["worktree", "add", "--quiet", "-b", &branch, &path_arg, "HEAD"])?;
    let base = git(&path, &["rev-parse", "HEAD"])?.trim().to_string();
    let run_cwd = path.join(prefix);
    let worktree = Worktree {
        path,
        branch,
        repo,
        base,
        left_out_uncommitted,
        reused: false,
    };
    Ok((worktree, run_cwd))
}

/// The brigade worktree `cwd` is in, when a run continues in one an earlier run
/// made.
pub fn existing(settings: &Settings, cwd: &Path) -> Option<Worktree> {
    if !cwd.starts_with(settings.worktrees_dir()) {
        return None;
    }
    let path = toplevel(cwd)?;
    let branch = git(&path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()?
        .trim()
        .to_string();
    if !branch.starts_with("brigade/") {
        return None;
    }
    Some(Worktree {
        repo: main_checkout(&path).ok()?,
        base: git(&path, &["rev-parse", "HEAD"]).ok()?.trim().to_string(),
        path,
        branch,
        left_out_uncommitted: false,
        reused: true,
    })
}

/// Commits everything a finished write run changed, on the worktree's branch.
pub fn commit(worktree: &Worktree, message: &str) -> WorktreeState {
    match git(&worktree.path, &["status", "--porcelain"]) {
        Ok(status) if status.trim().is_empty() => return WorktreeState::Unchanged,
        Ok(_) => {}
        Err(error) => return WorktreeState::Uncommitted { error },
    }
    let committed = git(&worktree.path, &["add", "--all"])
        .and_then(|_| git(&worktree.path, &["commit", "--quiet", "-m", message]))
        .and_then(|_| git(&worktree.path, &["rev-parse", "--short", "HEAD"]));
    match committed {
        Ok(commit) => WorktreeState::Committed {
            commit: commit.trim().to_string(),
        },
        Err(error) => WorktreeState::Uncommitted { error },
    }
}

/// After a run failed or was cancelled: removes its worktree and branch if the
/// run left nothing in them, so an abandoned run doesn't litter the repo.
pub fn tidy(worktree: &Worktree) -> WorktreeState {
    if worktree.reused {
        return WorktreeState::Kept;
    }
    let clean = git(&worktree.path, &["status", "--porcelain"]).is_ok_and(|s| s.trim().is_empty());
    let moved = git(&worktree.path, &["rev-parse", "HEAD"]).map_or(true, |head| head.trim() != worktree.base);
    if !clean || moved {
        return WorktreeState::Kept;
    }
    let path = worktree.path.to_string_lossy();
    let removed = git(&worktree.repo, &["worktree", "remove", &path])
        .and_then(|_| git(&worktree.repo, &["branch", "--quiet", "-D", &worktree.branch]));
    match removed {
        Ok(_) => WorktreeState::Removed,
        Err(_) => WorktreeState::Kept,
    }
}

fn toplevel(dir: &Path) -> Option<PathBuf> {
    git(dir, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(|root| PathBuf::from(root.trim()))
}

/// The repo a checkout belongs to, which for a worktree is the main checkout
/// rather than the worktree itself.
fn main_checkout(root: &Path) -> Result<PathBuf, String> {
    let common = git(root, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let common = PathBuf::from(common.trim());
    Ok(match common.file_name() {
        Some(name) if name == ".git" => common.parent().map(Path::to_path_buf).unwrap_or(common),
        _ => common,
    })
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("can't run git: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

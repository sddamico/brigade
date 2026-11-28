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
    /// The user's checkout the worktree stands in for, which the agent must
    /// leave alone, and its HEAD and branch when the run started.
    pub checkout: PathBuf,
    pub checkout_head: String,
    /// Empty for a detached HEAD.
    pub checkout_branch: String,
}

/// What a run did to its worktree, and to the user's checkout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorktreeReport {
    pub worktree: WorktreeState,
    /// How the checkout changed during the run, which it never should.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_changed: Option<String>,
}

/// What became of a run's worktree once the run ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum WorktreeState {
    Committed {
        commit: String,
        /// A branch the agent made and committed on, which points at the same commit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_branch: Option<String>,
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
    let branch_ref = format!("refs/heads/{branch}");
    let branch_existed = git(&root, &["rev-parse", "--verify", "--quiet", &branch_ref]).is_ok();
    if let Err(error) = git(&root, &["worktree", "add", "--quiet", "-b", &branch, &path_arg, "HEAD"]) {
        // `worktree add -b` makes the branch before it makes the directory.
        if !branch_existed {
            let _ = git(&root, &["branch", "--quiet", "-D", &branch]);
        }
        return Err(error);
    }
    let base = git(&path, &["rev-parse", "HEAD"])?.trim().to_string();
    // Deleting the branch deletes this with it.
    let _ = git(&path, &["config", &base_key(&branch), &base]);
    // `git rev-parse --show-prefix` is empty at the repo root, and joining it
    // would leave a trailing slash in the paths brigade prints.
    let run_cwd = if prefix.is_empty() {
        path.clone()
    } else {
        path.join(prefix)
    };
    let (checkout_head, checkout_branch) = position(&root)?;
    let worktree = Worktree {
        path,
        branch,
        repo,
        base,
        left_out_uncommitted,
        reused: false,
        checkout: root,
        checkout_head,
        checkout_branch,
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
    let repo = main_checkout(&path).ok()?;
    let (checkout_head, checkout_branch) = position(&repo).ok()?;
    let base = match git(&path, &["config", &base_key(&branch)]) {
        Ok(base) => base,
        // A worktree made before brigade recorded its base.
        Err(_) => git(&path, &["rev-parse", "HEAD"]).ok()?,
    };
    Some(Worktree {
        base: base.trim().to_string(),
        path,
        branch,
        checkout: repo.clone(),
        repo,
        left_out_uncommitted: false,
        reused: true,
        checkout_head,
        checkout_branch,
    })
}

/// A checkout's HEAD commit and branch.
fn position(checkout: &Path) -> Result<(String, String), String> {
    Ok((
        git(checkout, &["rev-parse", "HEAD"])?.trim().to_string(),
        git(checkout, &["branch", "--show-current"])?.trim().to_string(),
    ))
}

/// How the user's checkout moved while the run had its worktree: a switched
/// branch or a new HEAD means the agent worked there instead.
pub fn checkout_change(worktree: &Worktree) -> Option<String> {
    let (head, branch) = position(&worktree.checkout).ok()?;
    let name = |branch: &str| match branch {
        "" => "a detached HEAD".to_string(),
        branch => format!("branch {branch}"),
    };
    if branch != worktree.checkout_branch {
        Some(format!(
            "it's on {} now, not {}",
            name(&branch),
            name(&worktree.checkout_branch)
        ))
    } else if head != worktree.checkout_head {
        Some(format!(
            "its HEAD moved from {} to {}",
            &worktree.checkout_head[..7],
            &head[..7]
        ))
    } else {
        None
    }
}

/// Commits whatever a finished write run left uncommitted, on the worktree's
/// branch. An agent that committed its work itself has nothing left to commit,
/// but its commits still count as the run's changes.
pub fn commit(worktree: &Worktree, message: &str) -> WorktreeState {
    let dirty = match git(&worktree.path, &["status", "--porcelain"]) {
        Ok(status) => !status.trim().is_empty(),
        Err(error) => return WorktreeState::Uncommitted { error },
    };
    if dirty
        && let Err(error) = git(&worktree.path, &["add", "--all"])
            .and_then(|_| git(&worktree.path, &["commit", "--quiet", "-m", message]))
    {
        return WorktreeState::Uncommitted { error };
    }
    match git(&worktree.path, &["rev-parse", "HEAD"]) {
        Ok(head) if head.trim() == worktree.base => WorktreeState::Unchanged,
        Ok(head) => match reclaim_branch(worktree) {
            Ok(agent_branch) => WorktreeState::Committed {
                commit: head.trim().chars().take(7).collect(),
                agent_branch,
            },
            Err(error) => WorktreeState::Uncommitted { error },
        },
        Err(error) => WorktreeState::Uncommitted { error },
    }
}

/// An agent may switch to a branch of its own (users' instructions often name
/// a branch convention) and leave its commits there. The run's branch is moved
/// to that commit and checked out again, so the branch the reply names has the
/// work. Returns the agent's branch, if it made one.
fn reclaim_branch(worktree: &Worktree) -> Result<Option<String>, String> {
    let current = git(&worktree.path, &["branch", "--show-current"])?.trim().to_string();
    if current == worktree.branch {
        return Ok(None);
    }
    git(&worktree.path, &["branch", "--force", &worktree.branch, "HEAD"])?;
    git(&worktree.path, &["checkout", "--quiet", &worktree.branch])?;
    Ok((!current.is_empty()).then_some(current))
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

/// Where the commit a brigade worktree started from is kept.
fn base_key(branch: &str) -> String {
    format!("branch.{branch}.brigadeBase")
}

/// For a pruned run's worktree that no remaining run uses: removes it and its
/// branch if it's still clean at the commit it started from, whichever run
/// last worked in it. A worktree whose starting commit isn't known is kept.
pub fn tidy_abandoned(worktree: &Worktree) -> WorktreeState {
    let recorded = git(&worktree.path, &["config", &base_key(&worktree.branch)]).map(|b| b.trim().to_string());
    let base = match recorded {
        Ok(base) => base,
        Err(_) if !worktree.reused => worktree.base.clone(),
        Err(_) => return WorktreeState::Kept,
    };
    tidy(&Worktree {
        base,
        reused: false,
        ..worktree.clone()
    })
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
        // The subcommand only: a commit's arguments carry its whole message.
        let command: Vec<&str> = args
            .iter()
            .copied()
            .take_while(|a| !a.starts_with('-'))
            .take(2)
            .collect();
        Err(format!(
            "git {} failed: {}",
            command.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

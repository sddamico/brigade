use crate::runs::Worktree;
use crate::settings::Settings;
use std::path::Path;
use std::process::Command;

/// Creates a worktree on a new `brigade/<run id>` branch from the HEAD of the
/// repo containing `cwd`, so parallel writers each get their own checkout.
pub fn create(settings: &Settings, cwd: &Path, run_id: &str) -> Result<Worktree, String> {
    let root = git(cwd, &["rev-parse", "--show-toplevel"])
        .map_err(|_| format!("--worktree needs a git repo, and {} isn't in one", cwd.display()))?;
    let root = Path::new(root.trim());
    let repo = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let path = settings.worktrees_dir().join(repo).join(run_id);
    let branch = format!("brigade/{run_id}");
    git(
        root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            &branch,
            &path.to_string_lossy(),
            "HEAD",
        ],
    )?;
    Ok(Worktree { path, branch })
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

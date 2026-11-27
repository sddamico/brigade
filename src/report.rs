//! What brigade prints. Forwarder agents hand this output back verbatim, so the
//! footer carries everything the orchestrating agent needs to continue a run.

use crate::harness::{Access, Effort};
use crate::roster::{Entry, Source};
use crate::runs::{Outcome, RunRecord, State};
use crate::worktree::WorktreeState;
use serde_json::json;
use std::path::Path;

/// A detached run's wait slice. It stays under the 2 minute default timeout of
/// Claude Code's shell calls, which a forwarder may not raise.
pub const WAIT_SLICE_SECS: u64 = 100;

pub fn done(record: &RunRecord, session: Option<&str>, message: &str, worktree: Option<&WorktreeState>) -> String {
    let agent = &record.agent;
    let mut out = format!(
        "{message}\n\n[brigade] {} ran in {} ({}, {}, {}) as run {}\n",
        agent.name,
        agent.harness,
        record.model,
        effort_label(record.effort),
        record.access.as_str(),
        record.id,
    );
    if let Some(session) = session {
        // A resumed session keeps whatever this run chose over the roster.
        let mut choices = String::new();
        if record.model != agent.model {
            choices.push_str(&format!(" --model {}", record.model));
        }
        if let Some(effort) = record.effort.filter(|_| record.effort != agent.effort) {
            choices.push_str(&format!(" --effort {effort}"));
        }
        if record.access == Access::Write {
            choices.push_str(" --write");
        }
        out.push_str(&format!(
            "[brigade] continue it with: brigade run {} --resume {session} --cwd {}{choices}\n",
            agent.name,
            record.cwd.display(),
        ));
    }
    out.push_str(&worktree_lines(record, worktree));
    out
}

pub fn failed(record: &RunRecord, error: &str, logs: &Path, worktree: Option<&WorktreeState>) -> String {
    format!(
        "[brigade] {} failed: {error}\n{}[brigade] logs: {}\n",
        record.agent.name,
        worktree_lines(record, worktree),
        logs.display()
    )
}

pub fn cancelled(record: &RunRecord, reason: &str, logs: &Path, worktree: Option<&WorktreeState>) -> String {
    format!(
        "[brigade] run {} was cancelled: {reason}\n{}[brigade] logs: {}\n",
        record.id,
        worktree_lines(record, worktree),
        logs.display()
    )
}

/// Where a run's changes ended up, so an orchestrator relaying only the text
/// still knows what to merge or clean up.
fn worktree_lines(record: &RunRecord, state: Option<&WorktreeState>) -> String {
    let Some(worktree) = &record.worktree else {
        return String::new();
    };
    let (path, branch) = (worktree.path.display(), &worktree.branch);
    let mut out = match state {
        Some(WorktreeState::Committed { commit }) => {
            format!("[brigade] its changes are committed as {commit} on branch {branch}, in worktree {path}\n")
        }
        Some(WorktreeState::Unchanged) => {
            format!("[brigade] it changed nothing; its worktree {path} is on branch {branch}\n")
        }
        Some(WorktreeState::Uncommitted { error }) => format!(
            "[brigade] its changes are uncommitted in worktree {path} on branch {branch}, because committing failed: {error}\n"
        ),
        Some(WorktreeState::Removed) => {
            format!("[brigade] it left no changes, so its worktree and branch {branch} were removed\n")
        }
        Some(WorktreeState::Kept) | None => {
            format!("[brigade] its worktree {path} on branch {branch} is still there\n")
        }
    };
    if worktree.left_out_uncommitted && state != Some(&WorktreeState::Removed) {
        out.push_str(&format!(
            "[brigade] note: the worktree started from HEAD, so uncommitted changes in {} aren't in it\n",
            worktree.repo.display()
        ));
    }
    out
}

fn effort_label(effort: Option<Effort>) -> String {
    match effort {
        Some(effort) => format!("effort {effort}"),
        None => "the harness's default effort".to_string(),
    }
}

pub fn started(record: &RunRecord) -> String {
    let place = match &record.worktree {
        Some(worktree) => format!(
            ", in worktree {} on branch {}",
            worktree.path.display(),
            worktree.branch
        ),
        None => String::new(),
    };
    format!(
        "[brigade] started run {id} ({} in {}{place}). Wait for it with: brigade wait {id} --timeout {WAIT_SLICE_SECS}\n",
        record.agent.name,
        record.agent.harness,
        id = record.id,
    )
}

pub fn still_running(record: &RunRecord, waited: u64) -> String {
    format!(
        "[brigade] run {id} is still running after {waited}s. Wait again with: brigade wait {id} --timeout {waited}\n",
        id = record.id,
    )
}

pub fn lost(record: &RunRecord, logs: &Path) -> String {
    format!(
        "[brigade] run {} stopped without a result; its worker process is gone\n{}[brigade] logs: {}\n",
        record.id,
        worktree_lines(record, None),
        logs.display(),
    )
}

pub fn state_json(record: &RunRecord, state: &State, logs: &Path, worktree: Option<&WorktreeState>) -> String {
    let mut value = match state {
        State::Finished(Outcome::Done { session, message }) => {
            json!({ "run": record, "status": "done", "session": session, "message": message })
        }
        State::Finished(Outcome::Failed { error }) => {
            json!({ "run": record, "status": "failed", "error": error, "logs": logs })
        }
        State::Finished(Outcome::Cancelled { reason }) => {
            json!({ "run": record, "status": "cancelled", "reason": reason, "logs": logs })
        }
        State::Running => json!({ "run": record, "status": "running" }),
        State::Lost => json!({ "run": record, "status": "lost", "logs": logs }),
    };
    if let Some(worktree) = worktree {
        value["worktree"] = json!(worktree);
    }
    serde_json::to_string_pretty(&value).expect("json values serialize") + "\n"
}

pub fn roster(entries: &[Entry], user_file: &Path) -> String {
    let mut out = String::new();
    for entry in entries {
        let agent = &entry.agent;
        let source = match entry.source {
            Source::BuiltIn => String::new(),
            Source::User => format!(", from {}", user_file.display()),
        };
        out.push_str(&format!(
            "{} ({}, {}, {}{source}): {}\n",
            agent.name,
            agent.harness,
            agent.model,
            effort_label(agent.effort),
            agent.description,
        ));
    }
    out
}

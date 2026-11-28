//! What brigade prints. Forwarder agents hand this output back verbatim, so the
//! footer carries everything the orchestrating agent needs to continue a run.

use crate::harness::{Access, Effort};
use crate::roster::{Entry, Source};
use crate::runs::{Outcome, RunRecord, State};
use crate::worktree::{WorktreeReport, WorktreeState};
use serde_json::json;
use std::path::Path;

/// A detached run's wait slice. It stays under the 2 minute default timeout of
/// Claude Code's shell calls, which a forwarder may not raise.
pub const WAIT_SLICE_SECS: u64 = 100;

pub fn done(record: &RunRecord, session: Option<&str>, message: &str, worktree: Option<&WorktreeReport>) -> String {
    let agent = &record.agent;
    let mut out = format!(
        "{message}\n\n{}[brigade] {} ran in {} ({}, {}, {}) as run {}\n",
        fell_back(record),
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

pub fn failed(record: &RunRecord, error: &str, logs: &Path, worktree: Option<&WorktreeReport>) -> String {
    format!(
        "{}[brigade] {} failed: {error}\n{}[brigade] logs: {}\n",
        fell_back(record),
        record.agent.name,
        worktree_lines(record, worktree),
        logs.display()
    )
}

/// Hosts often answer without loading the skill, so the advice on what to do
/// next travels in the reply itself.
pub fn limited(record: &RunRecord, error: &str, logs: &Path, worktree: Option<&WorktreeReport>) -> String {
    let harness = record.agent.harness.product();
    format!(
        "{}[brigade] {} hit a usage limit in {harness}: {error}\n\
         [brigade] {harness}'s other agents usually share this limit, so don't retry, switch to another agent \
         or answer in its place. Tell the user what it said, including when it resets if it says, and let them choose what to do\n{}[brigade] logs: {}\n",
        fell_back(record),
        record.agent.name,
        worktree_lines(record, worktree),
        logs.display()
    )
}

/// Says which agent a run started on when a usage limit made it switch, and
/// when that agent's limit resets.
fn fell_back(record: &RunRecord) -> String {
    match &record.fell_back_from {
        Some(from) => format!(
            "[brigade] {} hit a usage limit in {}, so {} ran instead: {}\n",
            from.agent,
            from.harness.product(),
            record.agent.name,
            from.error
        ),
        None => String::new(),
    }
}

pub fn cancelled(record: &RunRecord, reason: &str, logs: &Path, worktree: Option<&WorktreeReport>) -> String {
    format!(
        "{}[brigade] run {} was cancelled: {reason}\n{}[brigade] logs: {}\n",
        fell_back(record),
        record.id,
        worktree_lines(record, worktree),
        logs.display()
    )
}

/// Where a run's changes ended up, so an orchestrator relaying only the text
/// still knows what to merge or clean up.
fn worktree_lines(record: &RunRecord, report: Option<&WorktreeReport>) -> String {
    let Some(worktree) = &record.worktree else {
        return String::new();
    };
    let (path, branch) = (worktree.path.display(), &worktree.branch);
    let mut out = match report.and_then(|r| r.checkout_changed.as_ref()) {
        Some(change) => format!(
            "[brigade] warning: the agent worked in the user's checkout {} instead of only its worktree, and {change}. \
             Check that checkout before carrying on\n",
            worktree.checkout.display()
        ),
        None => String::new(),
    };
    let state = report.map(|r| &r.worktree);
    out.push_str(&match state {
        Some(WorktreeState::Committed { commit, agent_branch }) => {
            let mut out =
                format!("[brigade] its changes are committed as {commit} on branch {branch}, in worktree {path}\n");
            if let Some(own) = agent_branch {
                out.push_str(&format!(
                    "[brigade] the agent also made branch {own} at the same commit; delete it too once {branch} is merged\n"
                ));
            }
            out
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
    });
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

pub fn state_json(record: &RunRecord, state: &State, logs: &Path, worktree: Option<&WorktreeReport>) -> String {
    let mut value = match state {
        State::Finished(Outcome::Done { session, message }) => {
            json!({ "run": record, "status": "done", "session": session, "message": message })
        }
        State::Finished(Outcome::Failed { error }) => {
            json!({ "run": record, "status": "failed", "error": error, "logs": logs })
        }
        State::Finished(Outcome::Limited { error }) => {
            json!({ "run": record, "status": "limited", "error": error, "logs": logs })
        }
        State::Finished(Outcome::Cancelled { reason }) => {
            json!({ "run": record, "status": "cancelled", "reason": reason, "logs": logs })
        }
        State::Running => json!({ "run": record, "status": "running" }),
        State::Lost => json!({ "run": record, "status": "lost", "logs": logs }),
    };
    if let Some(report) = worktree {
        value["worktree"] = json!(report.worktree);
        if let Some(change) = &report.checkout_changed {
            value["checkout_changed"] = json!(change);
        }
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
        let fallback = match &entry.fallback {
            Some(fallback) => format!("; falls back to {fallback} on a usage limit"),
            None => String::new(),
        };
        out.push_str(&format!(
            "{} ({}, {}, {}{source}): {}{fallback}\n",
            agent.name,
            agent.harness,
            agent.model,
            effort_label(agent.effort),
            agent.description,
        ));
    }
    out
}

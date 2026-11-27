//! What brigade prints. Forwarder agents hand this output back verbatim, so the
//! footer carries everything the orchestrating agent needs to continue a run.

use crate::harness::Access;
use crate::roster::{Entry, Source};
use crate::runs::{Outcome, RunRecord, State};
use serde_json::json;
use std::path::Path;

/// A detached run's wait slice. It stays under the 10 minute cap Claude Code
/// puts on a foreground shell call.
pub const WAIT_SLICE_SECS: u64 = 540;

pub fn done(record: &RunRecord, session: Option<&str>, message: &str) -> String {
    let agent = &record.agent;
    let mut out = format!(
        "{message}\n\n[brigade] {} ran in {} ({}, effort {}, {}) as run {}\n",
        agent.name,
        agent.harness,
        agent.model,
        record.effort,
        record.access.as_str(),
        record.id,
    );
    if let Some(session) = session {
        let write = if record.access == Access::Write { " --write" } else { "" };
        out.push_str(&format!(
            "[brigade] continue it with: brigade run {} --resume {session} --cwd {}{write}\n",
            agent.name,
            record.cwd.display(),
        ));
    }
    if let Some(worktree) = &record.worktree {
        out.push_str(&format!(
            "[brigade] its changes are in worktree {} on branch {}\n",
            worktree.path.display(),
            worktree.branch,
        ));
    }
    out
}

pub fn failed(record: &RunRecord, error: &str, logs: &Path) -> String {
    format!(
        "[brigade] {} failed: {error}\n[brigade] logs: {}\n",
        record.agent.name,
        logs.display()
    )
}

pub fn started(record: &RunRecord) -> String {
    format!(
        "[brigade] started run {id} ({} in {}). Wait for it with: brigade wait {id} --timeout {WAIT_SLICE_SECS}\n",
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
        "[brigade] run {} stopped without a result; its worker process is gone\n[brigade] logs: {}\n",
        record.id,
        logs.display(),
    )
}

pub fn state_json(record: &RunRecord, state: &State, logs: &Path) -> String {
    let value = match state {
        State::Finished(Outcome::Done { session, message }) => {
            json!({ "run": record, "status": "done", "session": session, "message": message })
        }
        State::Finished(Outcome::Failed { error }) => {
            json!({ "run": record, "status": "failed", "error": error, "logs": logs })
        }
        State::Running => json!({ "run": record, "status": "running" }),
        State::Lost => json!({ "run": record, "status": "lost", "logs": logs }),
    };
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
            "{} ({}, {}, default effort {}{source}): {}\n",
            agent.name, agent.harness, agent.model, agent.effort, agent.description,
        ));
    }
    out
}

//! What brigade prints. Forwarder agents hand this output back verbatim, so the
//! footer carries everything the orchestrating agent needs to continue a run.

use crate::accounts::{Limit, local_time};
use crate::capacity::AccountCapacity;
use crate::harness::{Access, Effort};
use crate::roster::{Benchmark, BenchmarkSource, Entry, Roster, ServedModel, Source};
use crate::runs::{Outcome, Run, RunRecord, State};
use crate::worktree::{WorktreeReport, WorktreeState};
use serde_json::json;
use std::collections::BTreeMap;
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
            "[brigade] continue it with: brigade run {} --account {} --resume {session} --cwd {}{choices}\n",
            agent.name,
            agent.account.name,
            shell_quote(&record.cwd.to_string_lossy()),
        ));
    }
    out.push_str(&worktree_lines(record, worktree));
    out
}

pub fn failed(record: &RunRecord, error: &str, logs: &Path, worktree: Option<&WorktreeReport>) -> String {
    format!(
        "{}[brigade] {} failed: {error}\n{}{}[brigade] logs: {}\n",
        fell_back(record),
        record.agent.name,
        harness_session(record),
        worktree_lines(record, worktree),
        logs.display()
    )
}

/// The session a run that didn't finish left behind, so it can be resumed or
/// deleted.
fn harness_session(record: &RunRecord) -> String {
    match &record.harness_session {
        Some(session) => format!(
            "[brigade] its {} session is {session}\n",
            record.agent.harness.product()
        ),
        None => String::new(),
    }
}

/// Quoted for a POSIX shell when it needs to be, so a printed command can be
/// run as it stands.
pub fn shell_quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:@%+=,".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// Hosts often answer without loading the skill, so the advice on what to do
/// next travels in the reply itself.
pub fn limited(record: &RunRecord, outcome: &Outcome, logs: &Path, worktree: Option<&WorktreeReport>) -> String {
    let Outcome::Limited {
        error,
        ran,
        until,
        retry_at,
        model,
    } = outcome
    else {
        unreachable!("only limited outcomes are reported as limited")
    };
    let (agent, harness, account) = (
        &record.agent.name,
        record.agent.harness.product(),
        &record.agent.account.name,
    );
    let limited = match model {
        Some(model) => format!("{model} on the {account} account"),
        None => format!("the {account} account"),
    };
    let what = if *ran {
        format!("[brigade] {agent} hit a usage limit in {harness}: {error}\n")
    } else {
        format!("[brigade] {agent} didn't run: {limited} is still limited: {error}\n")
    };
    let when = match (until, retry_at) {
        (Some(until), _) => format!("[brigade] {limited} is limited until {}\n", local_time(*until)),
        (None, Some(retry)) => format!(
            "[brigade] {harness} gave no reset time, so brigade lets one run try {limited} again after {}\n",
            local_time(*retry)
        ),
        (None, None) => String::new(),
    };
    let shared = match model {
        Some(model) => {
            format!("runs on {model} share this limit, though other models on the {account} account may not")
        }
        None => format!("agents on the {account} account share this limit"),
    };
    format!(
        "{}{what}{when}[brigade] {shared}, so don't retry, switch to another agent or answer in its place. Tell the \
         user what it said and let them choose what to do; `brigade capacity` shows which agents are free\n{}{}\
         [brigade] logs: {}\n",
        fell_back(record),
        harness_session(record),
        worktree_lines(record, worktree),
        logs.display()
    )
}

/// Says which agent a run started on when a usage limit made it switch, and
/// when that agent's limit resets.
/// Every route or agent the run left behind, each with where it went next and
/// why, and how long it waited for capacity.
fn fell_back(record: &RunRecord) -> String {
    let hops = &record.fell_back_from;
    let mut out = String::new();
    for (i, from) in hops.iter().enumerate() {
        let (agent, account) = match hops.get(i + 1) {
            Some(next) => (&next.agent, &next.account),
            None => (&record.agent.name, &record.agent.account.name),
        };
        let to = format!("{agent} ran on the {account} account instead");
        out.push_str(&if from.limited {
            format!(
                "[brigade] {} hit a usage limit on the {} account, so {to}: {}\n",
                from.agent, from.account, from.error
            )
        } else {
            format!("[brigade] {}, so {to}\n", from.error)
        });
    }
    if let Some(waited) = &record.waited {
        let took = match waited.secs {
            0..120 => format!("{}s", waited.secs),
            secs => format!("{} min", secs / 60),
        };
        out.push_str(&format!("[brigade] it waited {took} {} before it ran\n", waited.why));
    }
    out
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
            "[brigade] warning: the user's checkout {} changed during the run: {change}. Unless the user did that, \
             the agent worked there instead of in its worktree. Check that checkout before carrying on\n",
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

/// `waited` is how long this waiter waited. The hint always offers the
/// standard slice, which fits a host's shell-call timeout.
pub fn still_running(record: &RunRecord, waited: u64) -> String {
    format!(
        "[brigade] run {id} is still running after {waited}s. Wait again with: brigade wait {id} --timeout \
         {WAIT_SLICE_SECS}\n",
        id = record.id,
    )
}

/// `why` says what the run waits for: the account's limit or a free slot.
pub fn still_waiting(record: &RunRecord, why: &str, waited: u64) -> String {
    format!(
        "[brigade] run {id} is still waiting for the {account} account, {why}, after {waited}s. Wait again with: \
         brigade wait {id} --timeout {WAIT_SLICE_SECS}\n",
        id = record.id,
        account = record.agent.account.name,
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
        State::Finished(Outcome::Limited {
            error,
            ran,
            until,
            retry_at,
            model,
        }) => json!({
            "run": record,
            "status": "limited",
            "error": error,
            "ran": ran,
            "until": until,
            "retry_at": retry_at,
            "model": model,
            "logs": logs,
        }),
        State::Finished(Outcome::Cancelled { reason }) => {
            json!({ "run": record, "status": "cancelled", "reason": reason, "logs": logs })
        }
        State::Waiting => json!({ "run": record, "status": "waiting" }),
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

/// One line per agent, then one per route: the account, harness and model it
/// runs on there.
/// One line per agent (what it's best for, its score, its fallback), then one
/// per route. An agent on an account that reports which model it serves takes
/// that model's description, since it runs whatever is loaded. `serving` is the
/// model each such account last reported.
pub fn roster(roster: &Roster, serving: &BTreeMap<String, Vec<String>>, user_file: &Path) -> String {
    let agents: Vec<(&Entry, Option<&ServedModel>)> = roster
        .agents()
        .into_iter()
        .map(|entry| (entry, served_model(roster, serving, &entry.agent.name)))
        .collect();
    let mut sources: Vec<&BenchmarkSource> = Vec::new();
    for (entry, served) in &agents {
        if let Some(benchmark) = benchmark_of(entry, *served)
            && !sources.iter().any(|s| s.key == benchmark.source.key)
        {
            sources.push(&benchmark.source);
        }
    }
    let mut out = String::new();
    for source in &sources {
        out.push_str(&format!(
            "Scores ({}) are from the {} ({}), read {}.\n",
            source.key, source.name, source.url, source.read
        ));
    }
    if !sources.is_empty() {
        out.push('\n');
    }
    for (entry, served) in agents {
        let agent = &entry.agent;
        let best_for = served
            .and_then(|m| m.best_for.as_deref())
            .or(agent.best_for.as_deref())
            .map(|b| format!(" Best for {b}."))
            .unwrap_or_default();
        let score = benchmark_of(entry, served).map(score).unwrap_or_default();
        let fallback = match &entry.fallback {
            Some(fallback) => format!(" Falls back to {fallback} on a usage limit."),
            None => String::new(),
        };
        let said = format!("{best_for}{score}{fallback}");
        if said.is_empty() {
            out.push_str(&format!("{}\n", agent.name));
        } else {
            out.push_str(&format!("{}:{said}\n", agent.name));
        }
        for route in roster.routes(&agent.name) {
            let source = match route.source {
                Source::BuiltIn => String::new(),
                Source::User => format!(", from {}", user_file.display()),
            };
            let agent = &route.agent;
            let serves = serving
                .get(&agent.account.name)
                .map(|ids| format!(", serving {}", ids.join(", ")))
                .unwrap_or_default();
            out.push_str(&format!(
                "  {} account: {} ({}, {}{source}{serves})\n",
                agent.account.name,
                agent.description,
                agent.model,
                effort_label(agent.effort),
            ));
        }
    }
    out
}

/// The roster's entry for the model an agent's server reports, if it has one.
pub fn served_model<'a>(
    roster: &'a Roster,
    serving: &BTreeMap<String, Vec<String>>,
    agent: &str,
) -> Option<&'a ServedModel> {
    roster
        .routes(agent)
        .iter()
        .filter_map(|route| serving.get(&route.agent.account.name))
        // A server listing several models may list every one it can load, so
        // the list doesn't say which one runs.
        .find_map(|ids| match ids.as_slice() {
            [id] => roster.served_model(id),
            _ => None,
        })
}

fn benchmark_of<'a>(entry: &'a Entry, served: Option<&'a ServedModel>) -> Option<&'a Benchmark> {
    match served {
        Some(model) => model.benchmark.as_ref(),
        None => entry.agent.benchmark.as_ref(),
    }
}

fn score(benchmark: &Benchmark) -> String {
    let effort = benchmark.effort.map(|e| format!(" at effort {e}")).unwrap_or_default();
    let per_task = match (benchmark.cost_per_task, benchmark.minutes_per_task) {
        (Some(cost), Some(minutes)) => format!(", ${cost:.2} and {minutes:.0} min a task"),
        (Some(cost), None) => format!(", ${cost:.2} a task"),
        (None, Some(minutes)) => format!(", {minutes:.0} min a task"),
        (None, None) => String::new(),
    };
    format!(
        " Scores {} ({}){effort}{per_task}.",
        benchmark.intelligence, benchmark.source.key
    )
}

pub fn capacity(accounts: &[AccountCapacity], now: u64) -> String {
    let mut out = String::new();
    for account in accounts {
        let slots = match account.max_runs {
            Some(max) => format!("{} of {max} runs going", account.running),
            None => format!("{} running", account.running),
        };
        let state = match &account.limit {
            Some(limit) => limited_until(limit),
            None if !account.agents.is_empty() && account.agents.iter().all(|a| a.limit.is_some()) => {
                "every model limited".to_string()
            }
            None if account.agents.is_empty() => "no agents".to_string(),
            None if !account.free => "full".to_string(),
            None => "free".to_string(),
        };
        let names: Vec<&str> = account.agents.iter().map(|a| a.name.as_str()).collect();
        out.push_str(&format!("{} ({}): {state}, {slots}", account.account, names.join(", ")));
        if !account.serving.is_empty() {
            out.push_str(&format!(", serving {}", account.serving.join(", ")));
        }
        if account.waiting > 0 {
            out.push_str(&format!(", {} waiting", account.waiting));
        }
        out.push('\n');
        if let Some(limit) = &account.limit {
            out.push_str(&format!("  {}\n", limit.message));
        }
        for agent in &account.agents {
            if let Some(limit) = &agent.limit {
                out.push_str(&format!(
                    "  {} ({}): {}: {}\n",
                    agent.name,
                    agent.model,
                    limited_until(limit),
                    limit.message
                ));
            }
        }
        if let Some(usage) = &account.usage {
            let window = usage.window.as_deref().unwrap_or("usage");
            let used = usage
                .utilization
                .map(|u| format!("{:.0}% used", u * 100.0))
                .unwrap_or_else(|| usage.status.clone());
            let resets = usage
                .resets_at
                .map(|r| format!(", resets {}", local_time(r)))
                .unwrap_or_default();
            let age = now.saturating_sub(usage.at) / 60;
            out.push_str(&format!("  {window} window {used}{resets} (as of {age} min ago)\n"));
        }
        if let Some(read_at) = account.quotas_read_at {
            let age = now.saturating_sub(read_at) / 60;
            for quota in &account.quotas {
                let resets = quota
                    .resets_at
                    .map(|r| format!(", resets {}", local_time(r)))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "  {}: {:.0}% used{resets} (read {age} min ago)\n",
                    quota.name,
                    quota.used * 100.0
                ));
            }
        }
    }
    out
}

fn limited_until(limit: &Limit) -> String {
    match limit.until {
        Some(until) => format!("limited until {}", local_time(until)),
        None => format!("limited, tries again after {}", local_time(limit.retry_at)),
    }
}

pub fn runs(runs: &[Run], now: u64) -> String {
    let mut out = String::new();
    for run in runs {
        let record = &run.record;
        out.push_str(&format!(
            "{}  {}  {}  {}  {}\n",
            record.id,
            record.agent.name,
            record.agent.account.name,
            state_name(&run.state),
            age(now.saturating_sub(record.started_at)),
        ));
    }
    out
}

pub fn state_name(state: &State) -> &'static str {
    match state {
        State::Finished(Outcome::Done { .. }) => "done",
        State::Finished(Outcome::Failed { .. }) => "failed",
        State::Finished(Outcome::Limited { .. }) => "limited",
        State::Finished(Outcome::Cancelled { .. }) => "cancelled",
        State::Waiting => "waiting",
        State::Running => "running",
        State::Lost => "lost",
    }
}

fn age(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s ago"),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::shell_quote;

    #[test]
    fn shell_quote_leaves_plain_paths_and_quotes_the_rest() {
        assert_eq!(shell_quote("/home/me/work-1"), "/home/me/work-1");
        assert_eq!(shell_quote("/home/me/my project"), "'/home/me/my project'");
        assert_eq!(shell_quote("/tmp/it's"), "'/tmp/it'\\''s'");
        assert_eq!(shell_quote("/tmp/$HOME"), "'/tmp/$HOME'");
    }
}

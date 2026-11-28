//! What each account can take right now, for `brigade capacity` and the
//! scheduler that reads it.

use crate::accounts::{self, Limit, Quota, Usage};
use crate::invocation::expand;
use crate::roster::Roster;
use crate::runs::{self, State};
use crate::settings::Settings;
use crate::usage;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Serialize)]
pub struct AccountCapacity {
    pub account: String,
    pub agents: Vec<AgentCapacity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_runs: Option<u32>,
    pub running: usize,
    pub waiting: usize,
    /// A limit in effect now on the whole account.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<Limit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// The quotas the harness reported when last asked, by `capacity --refresh`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub quotas: Vec<Quota>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quotas_read_at: Option<u64>,
    /// Whether a run on at least one of its agents would start at once.
    pub free: bool,
}

#[derive(Debug, Serialize)]
pub struct AgentCapacity {
    pub name: String,
    pub model: String,
    /// A limit in effect now on this agent's model alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<Limit>,
    /// Whether a run on this agent would start at once.
    pub free: bool,
}

/// Every account the roster's agents use, plus any brigade has state for.
pub fn capacity(
    settings: &Settings,
    roster: &Roster,
    now: u64,
    var: &dyn Fn(&str) -> Option<String>,
) -> Vec<AccountCapacity> {
    // Per account: its agents with their models, and its max_runs.
    type Members = (Vec<(String, String)>, Option<u32>);
    let mut accounts: BTreeMap<String, Members> = BTreeMap::new();
    for entry in roster.entries() {
        let agent = &entry.agent;
        let slot = accounts.entry(agent.account.name.clone()).or_default();
        let model = expand(&agent.model, var).unwrap_or_else(|_| agent.model.clone());
        slot.0.push((agent.name.clone(), model));
        slot.1 = agent.account.max_runs;
    }
    for name in accounts::known(settings) {
        accounts.entry(name).or_default();
    }
    let runs = runs::list(settings);
    accounts
        .into_iter()
        .map(|(account, (agents, max_runs))| {
            let count = |state: State| {
                runs.iter()
                    .filter(|r| r.record.agent.account.name == account && r.state == state)
                    .count()
            };
            let (running, waiting) = (count(State::Running), count(State::Waiting));
            let state = accounts::load(settings, &account);
            // A limit or used-up quota on every model is the account's.
            let limit = state.limit_on("", now).map(|(limit, _)| limit);
            let free = limit.is_none() && max_runs.is_none_or(|max| running < max as usize);
            let agents: Vec<AgentCapacity> = agents
                .into_iter()
                .map(|(name, model)| {
                    let limit = state
                        .limit_on(&model, now)
                        .filter(|(_, scope)| scope.is_some())
                        .map(|(limit, _)| limit);
                    AgentCapacity {
                        free: free && limit.is_none(),
                        name,
                        model,
                        limit,
                    }
                })
                .collect();
            let free = agents.iter().any(|a| a.free);
            AccountCapacity {
                account,
                agents,
                max_runs,
                running,
                waiting,
                limit,
                usage: state.usage,
                quotas: state.quotas,
                quotas_read_at: state.quotas_read_at,
                free,
            }
        })
        .collect()
}

/// Reads every account's quotas from its harness, all at once since each read
/// takes a few seconds, and records them. Returns the accounts it couldn't
/// read, with why.
pub fn refresh(settings: &Settings, roster: &Roster, now: u64) -> Vec<(String, String)> {
    let var = |name: &str| std::env::var(name).ok();
    let readable: Vec<_> = roster.accounts().iter().filter(|a| a.harness.is_some()).collect();
    let results: Vec<(String, Result<Vec<Quota>, String>)> = std::thread::scope(|scope| {
        let reads: Vec<_> = readable
            .iter()
            .map(|account| scope.spawn(move || (account.name.clone(), usage::read(account, &var, now))))
            .collect();
        reads.into_iter().filter_map(|read| read.join().ok()).collect()
    });
    let mut failures = Vec::new();
    for (account, result) in results {
        match result.and_then(|quotas| accounts::record_quotas(settings, &account, quotas, now)) {
            Ok(()) => {}
            Err(error) => failures.push((account, error)),
        }
    }
    failures
}

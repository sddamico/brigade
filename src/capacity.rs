//! What each account can take right now, for `brigade capacity` and the
//! scheduler that reads it.

use crate::accounts::{self, Limit, Usage};
use crate::invocation::expand;
use crate::roster::Roster;
use crate::runs::{self, State};
use crate::settings::Settings;
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
    /// Whether the account has a free slot and no account-wide limit.
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
            let limit = state.limit.clone().filter(|l| now < l.free_at());
            let free = limit.is_none() && max_runs.is_none_or(|max| running < max as usize);
            let agents = agents
                .into_iter()
                .map(|(name, model)| {
                    let limit = state.models.get(&model).filter(|l| now < l.free_at()).cloned();
                    AgentCapacity {
                        free: free && limit.is_none(),
                        name,
                        model,
                        limit,
                    }
                })
                .collect();
            AccountCapacity {
                account,
                agents,
                max_runs,
                running,
                waiting,
                limit,
                usage: state.usage,
                free,
            }
        })
        .collect()
}

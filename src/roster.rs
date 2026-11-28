//! The roster: a catalog of the agents brigade knows, and the accounts that
//! offer them. An account opts into catalog agents by name, or defines agents
//! of its own. An agent several accounts offer has a route through each, and a
//! run takes the first route with capacity, in account order.

use crate::custom::CustomHarness;
use crate::harness::{BUILT_IN_NAMES, Effort, Harness};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

const BUILT_IN: &str = include_str!("../roster.toml");

/// The name of the generic forwarder every harness gets, so no roster agent may
/// take it.
pub const FORWARD: &str = "forward";

/// One route: an agent as one account offers it, which is everything a run
/// needs to start it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub name: String,
    pub harness: Harness,
    pub model: String,
    /// None leaves the effort to the harness's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// The efforts this model takes, where it takes fewer than its harness.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<Effort>,
    pub description: String,
    /// What the agent is best at, for whoever picks agents for tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_for: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub benchmark: Option<Benchmark>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    pub account: Account,
}

impl Agent {
    pub fn efforts(&self) -> &[Effort] {
        if self.efforts.is_empty() {
            self.harness.efforts()
        } else {
            &self.efforts
        }
    }

    pub fn supports(&self, effort: Effort) -> bool {
        self.efforts().contains(&effort)
    }

    /// The efforts it takes, as error messages name them.
    pub fn effort_levels(&self) -> String {
        match self.efforts() {
            [] => "no effort levels".to_string(),
            levels => {
                let names: Vec<&str> = levels.iter().map(|e| e.as_str()).collect();
                format!("effort {}", names.join(", "))
            }
        }
    }
}

/// How the agent's model scored on the roster's benchmark source, for comparing
/// agents when picking one for a task.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Benchmark {
    /// Its score on the source's index.
    pub intelligence: f64,
    /// What one of the index's tasks cost, in US dollars.
    pub cost_per_task: f64,
    pub minutes_per_task: f64,
    /// The effort the figures were measured at.
    pub effort: Effort,
}

/// Where the catalog's benchmark figures come from, and when they were read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkSource {
    pub name: String,
    pub url: String,
    pub read: String,
}

/// The credentials a route's runs draw on, and so share usage limits with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub name: String,
    /// How many runs the account serves at once; no cap when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runs: Option<u32>,
}

/// An account as the roster declares it, for reading its usage.
#[derive(Clone, Debug)]
pub struct AccountDef {
    pub name: String,
    /// The harness its catalog agents run in; an account of only its own
    /// agents may have none.
    pub harness: Option<Harness>,
    pub env: BTreeMap<String, String>,
    pub args: Vec<String>,
    pub source: Source,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    BuiltIn,
    User,
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    #[serde(flatten)]
    pub agent: Agent,
    pub source: Source,
    /// The agent a run switches to when every route of this one is limited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
}

#[derive(Debug)]
pub struct Roster {
    /// Every route, in account order.
    entries: Vec<Entry>,
    benchmark_source: Option<BenchmarkSource>,
    accounts: Vec<AccountDef>,
    /// The harnesses the user roster declares.
    harnesses: Vec<CustomHarness>,
}

#[derive(Debug)]
pub enum RosterError {
    Read {
        path: PathBuf,
        error: std::io::Error,
    },
    Parse {
        origin: String,
        message: String,
    },
    Invalid {
        origin: String,
        agent: String,
        problem: String,
    },
    InvalidAccount {
        origin: String,
        account: String,
        problem: String,
    },
    InvalidHarness {
        origin: String,
        harness: String,
        problem: String,
    },
}

impl fmt::Display for RosterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RosterError::Read { path, error } => write!(f, "can't read {}: {error}", path.display()),
            RosterError::Parse { origin, message } => write!(f, "{origin}: {message}"),
            RosterError::Invalid { origin, agent, problem } => write!(f, "{origin}: agent `{agent}` {problem}"),
            RosterError::InvalidAccount {
                origin,
                account,
                problem,
            } => write!(f, "{origin}: account `{account}` {problem}"),
            RosterError::InvalidHarness {
                origin,
                harness,
                problem,
            } => write!(f, "{origin}: harness `{harness}` {problem}"),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RosterFile {
    #[serde(default)]
    benchmark_source: Option<BenchmarkSource>,
    #[serde(default)]
    catalog: Vec<CatalogEntry>,
    #[serde(default)]
    account: Vec<AccountEntry>,
    #[serde(default)]
    harness: Vec<CustomHarness>,
    /// Agent name to the agent its runs switch to on a usage limit.
    #[serde(default)]
    fallbacks: BTreeMap<String, String>,
}

/// An agent any account can opt into: its model in each harness that has it.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogEntry {
    name: String,
    description: String,
    #[serde(default)]
    best_for: Option<String>,
    #[serde(default)]
    benchmark: Option<Benchmark>,
    /// Harness name to the model id that harness calls it.
    models: BTreeMap<String, String>,
    /// Harness name to the efforts the model takes there, where fewer than the
    /// harness's own.
    #[serde(default)]
    efforts: BTreeMap<String, Vec<Effort>>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountEntry {
    name: String,
    /// The harness its catalog agents run in.
    #[serde(default)]
    harness: Option<String>,
    /// Catalog agents the account offers.
    #[serde(default)]
    agents: Vec<String>,
    /// Agents of the account's own.
    #[serde(default)]
    agent: Vec<OwnAgent>,
    #[serde(default)]
    max_runs: Option<u32>,
    /// For every run on the account, e.g. CLAUDE_CONFIG_DIR for a second login.
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnAgent {
    name: String,
    /// The account's harness when unset.
    #[serde(default)]
    harness: Option<String>,
    model: String,
    #[serde(default)]
    effort: Option<Effort>,
    #[serde(default)]
    efforts: Vec<Effort>,
    description: String,
    #[serde(default)]
    best_for: Option<String>,
    #[serde(default)]
    benchmark: Option<Benchmark>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    args: Vec<String>,
}

impl Roster {
    pub fn built_in() -> Roster {
        Roster::build(
            parse(BUILT_IN, "built-in roster").expect("the built-in roster parses"),
            None,
        )
        .unwrap_or_else(|e| panic!("the built-in roster is invalid: {e}"))
    }

    /// The built-in roster with the user roster at `user_file` laid over it:
    /// its catalog entries and accounts replace the built-in ones of the same
    /// name and add the rest. A missing user roster is not an error.
    pub fn load(user_file: &Path) -> Result<Roster, RosterError> {
        let built_in = parse(BUILT_IN, "built-in roster")?;
        let user = match std::fs::read_to_string(user_file) {
            Ok(text) => {
                let origin = user_file.display().to_string();
                Some((parse(&text, &origin)?, origin))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(RosterError::Read {
                    path: user_file.to_path_buf(),
                    error,
                });
            }
        };
        Roster::build(built_in, user)
    }

    fn build(built_in: RosterFile, user: Option<(RosterFile, String)>) -> Result<Roster, RosterError> {
        const BUILT_IN_ORIGIN: &str = "built-in roster";
        let mut catalog: Vec<(CatalogEntry, &str)> =
            built_in.catalog.into_iter().map(|c| (c, BUILT_IN_ORIGIN)).collect();
        let mut accounts: Vec<(AccountEntry, Source)> =
            built_in.account.into_iter().map(|a| (a, Source::BuiltIn)).collect();
        let benchmark_source = user
            .as_ref()
            .and_then(|(file, _)| file.benchmark_source.clone())
            .or(built_in.benchmark_source);
        let (harnesses, fallbacks, user_origin) = match &user {
            Some((file, origin)) => (file.harness.clone(), file.fallbacks.clone(), origin.as_str()),
            None => (Vec::new(), BTreeMap::new(), BUILT_IN_ORIGIN),
        };
        if let Some((file, origin)) = &user {
            check_harnesses(&file.harness, origin)?;
            for entry in &file.catalog {
                match catalog.iter_mut().find(|(c, _)| c.name == entry.name) {
                    Some(existing) => *existing = (entry.clone(), origin),
                    None => catalog.push((entry.clone(), origin)),
                }
            }
            for account in &file.account {
                match accounts.iter_mut().find(|(a, _)| a.name == account.name) {
                    Some(existing) => *existing = (account.clone(), Source::User),
                    None => accounts.push((account.clone(), Source::User)),
                }
            }
        }
        let harness_named = |name: &str| {
            Harness::built_in(name).or_else(|| {
                harnesses
                    .iter()
                    .find(|h| h.name == name)
                    .map(|h| Harness::Custom(Box::new(h.clone())))
            })
        };
        for (entry, origin) in &catalog {
            check_agent_name(&entry.name, origin)?;
            for harness in entry.models.keys().chain(entry.efforts.keys()) {
                if harness_named(harness).is_none() {
                    return Err(RosterError::Invalid {
                        origin: origin.to_string(),
                        agent: entry.name.clone(),
                        problem: format!("names `{harness}`, which isn't a harness brigade knows"),
                    });
                }
            }
        }

        let mut entries = Vec::new();
        let mut defs = Vec::new();
        let mut seen_accounts = HashSet::new();
        for (account, source) in accounts {
            let origin = match source {
                Source::BuiltIn => BUILT_IN_ORIGIN,
                Source::User => user_origin,
            };
            let invalid = |problem: String| RosterError::InvalidAccount {
                origin: origin.to_string(),
                account: account.name.clone(),
                problem,
            };
            if !is_valid_name(&account.name) {
                return Err(invalid("needs a name of lowercase letters, digits and hyphens".into()));
            }
            if !seen_accounts.insert(account.name.clone()) {
                return Err(invalid("is defined twice".into()));
            }
            if account.max_runs == Some(0) {
                return Err(invalid("has max_runs 0, so it could never run anything".into()));
            }
            let harness = match &account.harness {
                Some(name) => Some(
                    harness_named(name)
                        .ok_or_else(|| invalid(format!("runs in `{name}`, which isn't a harness brigade knows")))?,
                ),
                None => None,
            };
            let reference = Account {
                name: account.name.clone(),
                max_runs: account.max_runs,
            };
            let mut names = HashSet::new();
            for name in &account.agents {
                let harness = harness
                    .clone()
                    .ok_or_else(|| invalid(format!("offers `{name}` but names no harness to run it in")))?;
                let (entry, _) = catalog
                    .iter()
                    .find(|(c, _)| &c.name == name)
                    .ok_or_else(|| invalid(format!("offers `{name}`, which isn't in the catalog")))?;
                let model = entry
                    .models
                    .get(harness.name())
                    .ok_or_else(|| invalid(format!("offers `{name}`, which the catalog has no {harness} model for")))?;
                if !names.insert(name.clone()) {
                    return Err(invalid(format!("offers `{name}` twice")));
                }
                entries.push(Entry {
                    agent: Agent {
                        name: name.clone(),
                        description: format!("{} in {}", entry.description, harness.product()),
                        efforts: entry.efforts.get(harness.name()).cloned().unwrap_or_default(),
                        harness,
                        model: model.clone(),
                        effort: None,
                        best_for: entry.best_for.clone(),
                        benchmark: entry.benchmark.clone(),
                        env: account.env.clone(),
                        args: account.args.clone(),
                        account: reference.clone(),
                    },
                    source,
                    fallback: None,
                });
            }
            for own in &account.agent {
                check_agent_name(&own.name, origin)?;
                let invalid = |problem: String| RosterError::Invalid {
                    origin: origin.to_string(),
                    agent: own.name.clone(),
                    problem,
                };
                let harness = match own.harness.as_deref().or(account.harness.as_deref()) {
                    Some(name) => harness_named(name)
                        .ok_or_else(|| invalid(format!("runs in `{name}`, which isn't a harness brigade knows")))?,
                    None => return Err(invalid("names no harness, and its account doesn't either".into())),
                };
                if !names.insert(own.name.clone()) {
                    return Err(invalid(format!("is offered twice by account `{}`", account.name)));
                }
                let mut env = account.env.clone();
                env.extend(own.env.clone());
                let agent = Agent {
                    name: own.name.clone(),
                    harness,
                    model: own.model.clone(),
                    effort: own.effort,
                    efforts: own.efforts.clone(),
                    description: own.description.clone(),
                    best_for: own.best_for.clone(),
                    benchmark: own.benchmark.clone(),
                    env,
                    args: account.args.iter().chain(&own.args).cloned().collect(),
                    account: reference.clone(),
                };
                if let Some(effort) = own.effort.filter(|e| !agent.supports(*e)) {
                    return Err(invalid(unsupported_effort(&agent, effort)));
                }
                entries.push(Entry {
                    agent,
                    source,
                    fallback: None,
                });
            }
            defs.push(AccountDef {
                name: account.name.clone(),
                harness,
                env: account.env.clone(),
                args: account.args.clone(),
                source,
            });
        }

        let mut roster = Roster {
            entries,
            benchmark_source,
            accounts: defs,
            harnesses,
        };
        roster.set_fallbacks(fallbacks, user_origin)?;
        Ok(roster)
    }

    /// Fallbacks name agents from the whole roster, built-in ones included, so
    /// they're checked once every account is in.
    fn set_fallbacks(&mut self, fallbacks: BTreeMap<String, String>, origin: &str) -> Result<(), RosterError> {
        for (name, fallback) in fallbacks {
            let invalid = |problem: String| RosterError::Invalid {
                origin: format!("{origin} [fallbacks]"),
                agent: name.clone(),
                problem,
            };
            if name == fallback {
                return Err(invalid("falls back to itself".into()));
            }
            if self.get(&fallback).is_none() {
                return Err(invalid(format!(
                    "falls back to `{fallback}`, which isn't in the roster"
                )));
            }
            if self.get(&name).is_none() {
                return Err(invalid("isn't in the roster".into()));
            }
            for entry in self.entries.iter_mut().filter(|e| e.agent.name == name) {
                entry.fallback = Some(fallback.clone());
            }
        }
        Ok(())
    }

    pub fn benchmark_source(&self) -> Option<&BenchmarkSource> {
        self.benchmark_source.as_ref()
    }

    pub fn harnesses(&self) -> &[CustomHarness] {
        &self.harnesses
    }

    pub fn accounts(&self) -> &[AccountDef] {
        &self.accounts
    }

    /// The agent's routes, in account order.
    pub fn routes(&self, name: &str) -> Vec<&Entry> {
        self.entries.iter().filter(|e| e.agent.name == name).collect()
    }

    /// The agent's first route.
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.agent.name == name)
    }

    /// Every route.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Each agent's first route, in the order the agents first appear.
    pub fn agents(&self) -> Vec<&Entry> {
        let mut seen = HashSet::new();
        self.entries
            .iter()
            .filter(|e| seen.insert(e.agent.name.as_str()))
            .collect()
    }

    pub fn names(&self) -> Vec<&str> {
        self.agents().into_iter().map(|e| e.agent.name.as_str()).collect()
    }
}

fn parse(text: &str, origin: &str) -> Result<RosterFile, RosterError> {
    toml::from_str(text).map_err(|e| RosterError::Parse {
        origin: origin.to_string(),
        message: e.to_string(),
    })
}

fn check_harnesses(harnesses: &[CustomHarness], origin: &str) -> Result<(), RosterError> {
    let mut seen = HashSet::new();
    for harness in harnesses {
        let invalid = |problem: String| RosterError::InvalidHarness {
            origin: origin.to_string(),
            harness: harness.name.clone(),
            problem,
        };
        if !is_valid_name(&harness.name) {
            return Err(invalid("needs a name of lowercase letters, digits and hyphens".into()));
        }
        if BUILT_IN_NAMES.contains(&harness.name.as_str()) {
            return Err(invalid("has a built-in harness's name".into()));
        }
        if !seen.insert(harness.name.as_str()) {
            return Err(invalid("is defined twice".into()));
        }
        harness.check().map_err(invalid)?;
    }
    Ok(())
}

fn check_agent_name(name: &str, origin: &str) -> Result<(), RosterError> {
    let invalid = |problem: String| RosterError::Invalid {
        origin: origin.to_string(),
        agent: name.to_string(),
        problem,
    };
    if !is_valid_name(name) {
        return Err(invalid("needs a name of lowercase letters, digits and hyphens".into()));
    }
    if name == FORWARD {
        return Err(invalid(format!(
            "can't be called `{FORWARD}`, the generic forwarder's name"
        )));
    }
    Ok(())
}

pub fn unsupported_effort(agent: &Agent, effort: Effort) -> String {
    format!("has effort `{effort}`, but takes {}", agent.effort_levels())
}

fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn user_roster(text: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(text.as_bytes()).unwrap();
        file
    }

    fn route_accounts<'a>(roster: &'a Roster, name: &str) -> Vec<&'a str> {
        roster
            .routes(name)
            .iter()
            .map(|e| e.agent.account.name.as_str())
            .collect()
    }

    #[test]
    fn an_agent_offered_by_two_accounts_gets_a_route_through_each_in_account_order() {
        let file = user_roster(
            r#"
            [[account]]
            name = "agy"
            harness = "agy"
            agents = ["neon", "hydrogen", "opus"]
            "#,
        );
        let roster = Roster::load(file.path()).unwrap();
        assert_eq!(route_accounts(&roster, "opus"), ["claude", "agy"]);
        let via_agy = &roster.routes("opus")[1].agent;
        assert_eq!(via_agy.model, "claude-opus-5-5");
        assert_eq!(via_agy.description, "Claude Opus in Antigravity");
        assert_eq!(via_agy.efforts(), [Effort::Low, Effort::Medium, Effort::High]);
        assert_eq!(
            via_agy.benchmark.as_ref().map(|b| b.intelligence),
            Some(59.0),
            "a route carries its agent's figures"
        );
        assert_eq!(roster.names().iter().filter(|n| **n == "opus").count(), 1);
    }

    #[test]
    fn accounts_opt_in_and_out_and_define_their_own_agents() {
        let file = user_roster(
            r#"
            [[account]]
            name = "claude"
            harness = "claude"
            agents = ["fable", "opus"]

            [[account]]
            name = "local"
            max_runs = 2

            [[account.agent]]
            name = "claude-local"
            harness = "claude"
            model = "$LOCAL_MODEL"
            effort = "medium"
            description = "Claude Code on a local model"
            env = { ANTHROPIC_BASE_URL = "$LOCAL_MODEL_SERVER" }

            [[account.agent]]
            name = "codex-local"
            harness = "codex"
            model = "$LOCAL_MODEL"
            description = "Codex on a local model"
            "#,
        );
        let roster = Roster::load(file.path()).unwrap();
        assert!(
            roster.get("sonnet").is_none(),
            "the user's claude account leaves sonnet out"
        );
        let local = roster.get("claude-local").unwrap();
        assert_eq!(local.source, Source::User);
        assert_eq!(local.agent.env["ANTHROPIC_BASE_URL"], "$LOCAL_MODEL_SERVER");
        assert_eq!(local.agent.account.max_runs, Some(2));
        assert_eq!(roster.get("codex-local").unwrap().agent.account.name, "local");
        assert_eq!(roster.get("sol").unwrap().source, Source::BuiltIn);
    }

    #[test]
    fn missing_user_roster_leaves_the_built_ins() {
        let roster = Roster::load(Path::new("/nonexistent/brigade/roster.toml")).unwrap();
        assert_eq!(roster.names(), Roster::built_in().names());
        assert_eq!(
            roster.names(),
            ["fable", "opus", "sonnet", "sol", "astra", "neon", "hydrogen"]
        );
    }

    #[test]
    fn rejects_accounts_and_agents_that_cant_work() {
        for (text, problem) in [
            (
                "[[account]]\nname = \"x\"\nharness = \"codex\"\nagents = [\"neon\"]\n",
                "account `x` offers `neon`, which the catalog has no codex model for",
            ),
            (
                "[[account]]\nname = \"x\"\nharness = \"codex\"\nagents = [\"nope\"]\n",
                "account `x` offers `nope`, which isn't in the catalog",
            ),
            (
                "[[account]]\nname = \"x\"\nagents = [\"sol\"]\n",
                "account `x` offers `sol` but names no harness to run it in",
            ),
            (
                "[[account]]\nname = \"x\"\nharness = \"agy\"\n[[account.agent]]\nname = \"n\"\nmodel = \"m\"\neffort = \"ultra\"\ndescription = \"x\"\n",
                "agent `n` has effort `ultra`, but takes effort low, medium, high, xhigh",
            ),
            (
                "[[account]]\nname = \"x\"\nharness = \"codex\"\n[[account.agent]]\nname = \"forward\"\nmodel = \"m\"\ndescription = \"x\"\n",
                "agent `forward` can't be called `forward`",
            ),
            (
                "[[account]]\nname = \"x\"\nmax_runs = 0\n",
                "account `x` has max_runs 0",
            ),
        ] {
            let file = user_roster(text);
            let error = Roster::load(file.path()).unwrap_err().to_string();
            assert!(error.contains(problem), "{error}");
        }
    }

    #[test]
    fn rejects_unknown_fields() {
        let file = user_roster("[[account]]\nname = \"x\"\nsandbox = \"none\"\n");
        assert!(matches!(Roster::load(file.path()), Err(RosterError::Parse { .. })));
    }

    #[test]
    fn fallbacks_name_agents_from_the_whole_roster() {
        let file = user_roster(
            r#"
            [[account]]
            name = "agy"
            harness = "agy"
            agents = ["neon", "opus"]

            [fallbacks]
            sol = "opus"
            opus = "sol"
            "#,
        );
        let roster = Roster::load(file.path()).unwrap();
        assert_eq!(roster.get("sol").unwrap().fallback.as_deref(), Some("opus"));
        assert!(
            roster
                .routes("opus")
                .iter()
                .all(|e| e.fallback.as_deref() == Some("sol"))
        );
        assert_eq!(roster.get("neon").unwrap().fallback, None);

        for (fallbacks, problem) in [
            ("sol = \"sol\"", "agent `sol` falls back to itself"),
            (
                "sol = \"nope\"",
                "agent `sol` falls back to `nope`, which isn't in the roster",
            ),
            ("nope = \"sol\"", "agent `nope` isn't in the roster"),
        ] {
            let file = user_roster(&format!("[fallbacks]\n{fallbacks}\n"));
            let error = Roster::load(file.path()).unwrap_err().to_string();
            assert!(error.contains(problem), "{error}");
        }
    }
}

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub name: String,
    pub harness: Harness,
    pub model: String,
    /// None leaves the effort to the harness's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    pub description: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    pub account: Account,
}

/// The credentials an agent's runs draw on, and so share usage limits with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub name: String,
    /// How many runs the account serves at once; no cap when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runs: Option<u32>,
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
    /// The agent a run switches to when this one hits its harness's usage limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
}

#[derive(Debug)]
pub struct Roster {
    entries: Vec<Entry>,
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
            RosterError::Invalid { origin, agent, problem } => {
                write!(f, "{origin}: agent `{agent}` {problem}")
            }
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
    agent: Vec<RosterAgent>,
    #[serde(default)]
    harness: Vec<CustomHarness>,
    #[serde(default)]
    account: Vec<Account>,
    /// Agent name to the agent its runs switch to on a usage limit.
    #[serde(default)]
    fallbacks: BTreeMap<String, String>,
}

/// An agent as the roster file writes it: its harness by name, which may be
/// one the same file declares.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RosterAgent {
    name: String,
    harness: String,
    model: String,
    #[serde(default)]
    effort: Option<Effort>,
    description: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    args: Vec<String>,
    /// Defaults to the harness's name.
    #[serde(default)]
    account: Option<String>,
}

struct ParsedFile {
    agents: Vec<Agent>,
    harnesses: Vec<CustomHarness>,
    accounts: Vec<Account>,
    fallbacks: BTreeMap<String, String>,
}

impl Roster {
    pub fn built_in() -> Roster {
        let file =
            parse_file(BUILT_IN, "built-in roster").unwrap_or_else(|e| panic!("the built-in roster is invalid: {e}"));
        Roster {
            harnesses: file.harnesses,
            entries: file
                .agents
                .into_iter()
                .map(|agent| Entry {
                    agent,
                    source: Source::BuiltIn,
                    fallback: None,
                })
                .collect(),
        }
    }

    /// The built-in roster with the user roster at `user_file` laid over it. A
    /// missing user roster is not an error.
    pub fn load(user_file: &Path) -> Result<Roster, RosterError> {
        let mut roster = Roster::built_in();
        match std::fs::read_to_string(user_file) {
            Ok(text) => {
                let origin = user_file.display().to_string();
                let file = parse_file(&text, &origin)?;
                roster.harnesses = file.harnesses;
                for agent in file.agents {
                    roster.put(Entry {
                        agent,
                        source: Source::User,
                        fallback: None,
                    });
                }
                roster.set_fallbacks(file.fallbacks, &origin)?;
                roster.set_accounts(&file.accounts);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(RosterError::Read {
                    path: user_file.to_path_buf(),
                    error,
                });
            }
        }
        Ok(roster)
    }

    fn put(&mut self, entry: Entry) {
        match self.entries.iter_mut().find(|e| e.agent.name == entry.agent.name) {
            Some(existing) => *existing = entry,
            None => self.entries.push(entry),
        }
    }

    /// Fallbacks name agents from the whole roster, built-in ones included, so
    /// they're checked once the user's agents are in.
    /// Account settings apply to built-in agents too, so they're laid on once
    /// the user's agents are in.
    fn set_accounts(&mut self, accounts: &[Account]) {
        for entry in &mut self.entries {
            if let Some(account) = accounts.iter().find(|a| a.name == entry.agent.account.name) {
                entry.agent.account.max_runs = account.max_runs;
            }
        }
    }

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
            let entry = self
                .entries
                .iter_mut()
                .find(|e| e.agent.name == name)
                .ok_or_else(|| invalid("isn't in the roster".into()))?;
            entry.fallback = Some(fallback);
        }
        Ok(())
    }

    pub fn harnesses(&self) -> &[CustomHarness] {
        &self.harnesses
    }

    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.agent.name == name)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.agent.name.as_str()).collect()
    }
}

fn parse_file(text: &str, origin: &str) -> Result<ParsedFile, RosterError> {
    let file: RosterFile = toml::from_str(text).map_err(|e| RosterError::Parse {
        origin: origin.to_string(),
        message: e.to_string(),
    })?;
    let mut seen = HashSet::new();
    for harness in &file.harness {
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
    let mut seen = HashSet::new();
    let mut agents = Vec::new();
    for raw in file.agent {
        let invalid = |problem: String| RosterError::Invalid {
            origin: origin.to_string(),
            agent: raw.name.clone(),
            problem,
        };
        if !is_valid_name(&raw.name) {
            return Err(invalid("needs a name of lowercase letters, digits and hyphens".into()));
        }
        if raw.name == FORWARD {
            return Err(invalid(format!(
                "can't be called `{FORWARD}`, the generic forwarder's name"
            )));
        }
        if !seen.insert(raw.name.clone()) {
            return Err(invalid("is defined twice".into()));
        }
        let harness = Harness::built_in(&raw.harness)
            .or_else(|| {
                file.harness
                    .iter()
                    .find(|h| h.name == raw.harness)
                    .map(|h| Harness::Custom(Box::new(h.clone())))
            })
            .ok_or_else(|| {
                invalid(format!(
                    "runs in `{}`, which isn't a harness brigade knows",
                    raw.harness
                ))
            })?;
        if let Some(effort) = raw.effort.filter(|e| !harness.supports(*e)) {
            return Err(invalid(unsupported_effort(&harness, effort)));
        }
        let account = raw.account.unwrap_or_else(|| harness.name().to_string());
        if !is_valid_name(&account) {
            return Err(invalid(format!(
                "has account `{account}`, which needs a name of lowercase letters, digits and hyphens"
            )));
        }
        agents.push(Agent {
            account: Account {
                name: account,
                max_runs: None,
            },
            name: raw.name,
            harness,
            model: raw.model,
            effort: raw.effort,
            description: raw.description,
            env: raw.env,
            args: raw.args,
        });
    }
    let mut seen = HashSet::new();
    for account in &file.account {
        let invalid = |problem: &str| RosterError::Parse {
            origin: origin.to_string(),
            message: format!("account `{}` {problem}", account.name),
        };
        if !is_valid_name(&account.name) {
            return Err(invalid("needs a name of lowercase letters, digits and hyphens"));
        }
        if !seen.insert(account.name.as_str()) {
            return Err(invalid("is defined twice"));
        }
        if account.max_runs == Some(0) {
            return Err(invalid("has max_runs 0, so it could never run anything"));
        }
    }
    Ok(ParsedFile {
        agents,
        accounts: file.account,
        harnesses: file.harness,
        fallbacks: file.fallbacks,
    })
}

pub fn unsupported_effort(harness: &Harness, effort: Effort) -> String {
    format!("has effort `{effort}`, but {harness} takes {}", harness.effort_levels())
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

    #[test]
    fn user_agents_extend_and_replace_built_ins() {
        let file = user_roster(
            r#"
            [[agent]]
            name = "claude-local"
            harness = "claude"
            model = "$LOCAL_MODEL"
            effort = "medium"
            description = "Claude Code on a local model"
            env = { ANTHROPIC_BASE_URL = "$LOCAL_MODEL_SERVER" }

            [[agent]]
            name = "sol"
            harness = "codex"
            model = "gpt-6.1-sol"
            effort = "high"
            description = "pinned to the previous Sol"
            "#,
        );
        let roster = Roster::load(file.path()).unwrap();

        let local = roster.get("claude-local").unwrap();
        assert_eq!(local.source, Source::User);
        assert_eq!(local.agent.env["ANTHROPIC_BASE_URL"], "$LOCAL_MODEL_SERVER");

        let sol = roster.get("sol").unwrap();
        assert_eq!((sol.source, sol.agent.model.as_str()), (Source::User, "gpt-6.1-sol"));
        assert_eq!(roster.names().iter().filter(|n| **n == "sol").count(), 1);
        assert_eq!(roster.get("opus").unwrap().source, Source::BuiltIn);
    }

    #[test]
    fn missing_user_roster_leaves_the_built_ins() {
        let roster = Roster::load(Path::new("/nonexistent/brigade/roster.toml")).unwrap();
        assert_eq!(roster.names(), Roster::built_in().names());
    }

    #[test]
    fn rejects_an_effort_the_harness_does_not_take() {
        let file = user_roster(
            r#"
            [[agent]]
            name = "neon-ultra"
            harness = "agy"
            model = "gemini-4-neon"
            effort = "ultra"
            description = "x"
            "#,
        );
        let error = Roster::load(file.path()).unwrap_err().to_string();
        assert!(
            error.contains("effort `ultra`, but agy takes effort low, medium, high, xhigh"),
            "{error}"
        );
    }

    #[test]
    fn rejects_the_forwarder_name_and_bad_names() {
        for name in ["forward", "Sol", "2x", "neon_low"] {
            let file = user_roster(&format!(
                "[[agent]]\nname = \"{name}\"\nharness = \"codex\"\nmodel = \"m\"\neffort = \"low\"\ndescription = \"x\"\n"
            ));
            assert!(
                matches!(Roster::load(file.path()), Err(RosterError::Invalid { .. })),
                "{name} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_unknown_fields() {
        let file = user_roster(
            "[[agent]]\nname = \"x\"\nharness = \"codex\"\nmodel = \"m\"\neffort = \"low\"\ndescription = \"x\"\nsandbox = \"none\"\n",
        );
        assert!(matches!(Roster::load(file.path()), Err(RosterError::Parse { .. })));
    }
    #[test]
    fn fallbacks_name_agents_from_the_whole_roster() {
        let file = user_roster(
            r#"
            [[agent]]
            name = "claude-local"
            harness = "claude"
            model = "local"
            description = "x"

            [fallbacks]
            sol = "opus"
            claude-local = "sonnet"
            "#,
        );
        let roster = Roster::load(file.path()).unwrap();
        assert_eq!(roster.get("sol").unwrap().fallback.as_deref(), Some("opus"));
        assert_eq!(roster.get("claude-local").unwrap().fallback.as_deref(), Some("sonnet"));
        assert_eq!(roster.get("opus").unwrap().fallback, None);

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

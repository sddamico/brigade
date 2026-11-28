use crate::harness::{Effort, Harness};
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
}

impl fmt::Display for RosterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RosterError::Read { path, error } => write!(f, "can't read {}: {error}", path.display()),
            RosterError::Parse { origin, message } => write!(f, "{origin}: {message}"),
            RosterError::Invalid { origin, agent, problem } => {
                write!(f, "{origin}: agent `{agent}` {problem}")
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RosterFile {
    #[serde(default)]
    agent: Vec<Agent>,
    /// Agent name to the agent its runs switch to on a usage limit.
    #[serde(default)]
    fallbacks: BTreeMap<String, String>,
}

impl Roster {
    pub fn built_in() -> Roster {
        let file =
            parse_file(BUILT_IN, "built-in roster").unwrap_or_else(|e| panic!("the built-in roster is invalid: {e}"));
        Roster {
            entries: file
                .agent
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
                for agent in file.agent {
                    roster.put(Entry {
                        agent,
                        source: Source::User,
                        fallback: None,
                    });
                }
                roster.set_fallbacks(file.fallbacks, &origin)?;
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

fn parse_file(text: &str, origin: &str) -> Result<RosterFile, RosterError> {
    let file: RosterFile = toml::from_str(text).map_err(|e| RosterError::Parse {
        origin: origin.to_string(),
        message: e.to_string(),
    })?;
    let invalid = |agent: &Agent, problem: String| RosterError::Invalid {
        origin: origin.to_string(),
        agent: agent.name.clone(),
        problem,
    };
    let mut seen = HashSet::new();
    for agent in &file.agent {
        if !is_valid_name(&agent.name) {
            return Err(invalid(
                agent,
                "needs a name of lowercase letters, digits and hyphens".into(),
            ));
        }
        if agent.name == FORWARD {
            return Err(invalid(
                agent,
                format!("can't be called `{FORWARD}`, the generic forwarder's name"),
            ));
        }
        if !seen.insert(agent.name.as_str()) {
            return Err(invalid(agent, "is defined twice".into()));
        }
        if let Some(effort) = agent.effort.filter(|e| !agent.harness.supports(*e)) {
            return Err(invalid(agent, unsupported_effort(agent.harness, effort)));
        }
    }
    Ok(file)
}

pub fn unsupported_effort(harness: Harness, effort: Effort) -> String {
    let levels: Vec<&str> = harness.efforts().iter().map(|e| e.as_str()).collect();
    format!("has effort `{effort}`, but {harness} takes {}", levels.join(", "))
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
            error.contains("effort `ultra`, but agy takes low, medium, high, xhigh"),
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

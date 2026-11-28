use crate::harness::Harness;
use std::path::PathBuf;
use std::time::Duration;

/// Runs started by a brigade worker are one level deeper than their parent.
/// Past this depth brigade refuses, so a team can't fan out without bound.
pub const MAX_DEPTH: u32 = 2;

pub const DEPTH_VAR: &str = "BRIGADE_DEPTH";

/// How long a run survives with nobody waiting on it. Forwarders wait again
/// within seconds of a wait slice ending, so only a stopped waiter lets it lapse.
const DEFAULT_LEASE_SECS: u64 = 90;

/// Everything brigade reads from its environment, read in one place.
#[derive(Clone, Debug)]
pub struct Settings {
    pub roster_file: PathBuf,
    pub state_dir: PathBuf,
    pub depth: u32,
    pub lease: Duration,
    /// Where each harness finds the user's own subagents.
    pub claude_agents_dir: PathBuf,
    pub codex_agents_dir: PathBuf,
    pub agy_agents_dir: PathBuf,
}

impl Settings {
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Result<Settings, String> {
        let set = |name: &str| var(name).filter(|v| !v.is_empty());
        let home = set("HOME").map(PathBuf::from).ok_or("HOME is not set")?;
        let config = set("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let state = set("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"));
        let depth = match set(DEPTH_VAR) {
            Some(value) => value
                .parse()
                .map_err(|_| format!("{DEPTH_VAR} is `{value}`, not a number"))?,
            None => 0,
        };
        let lease = match set("BRIGADE_LEASE_SECS") {
            Some(value) => value
                .parse()
                .map_err(|_| format!("BRIGADE_LEASE_SECS is `{value}`, not a number of seconds"))?,
            None => DEFAULT_LEASE_SECS,
        };
        let claude = set("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"));
        let codex = set("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        Ok(Settings {
            roster_file: config.join("brigade/roster.toml"),
            state_dir: state.join("brigade"),
            depth,
            lease: Duration::from_secs(lease),
            claude_agents_dir: claude.join("agents"),
            codex_agents_dir: codex.join("agents"),
            // agy only looks at the top of this directory, not in subdirectories.
            agy_agents_dir: home.join(".gemini/config/agents"),
        })
    }

    pub fn agents_dir(&self, host: Harness) -> &PathBuf {
        match host {
            Harness::Claude => &self.claude_agents_dir,
            Harness::Codex => &self.codex_agents_dir,
            Harness::Agy => &self.agy_agents_dir,
        }
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.state_dir.join("runs")
    }

    pub fn worktrees_dir(&self) -> PathBuf {
        self.state_dir.join("worktrees")
    }
}

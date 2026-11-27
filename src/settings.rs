use std::path::PathBuf;

/// Runs started by a brigade worker are one level deeper than their parent.
/// Past this depth brigade refuses, so a team can't fan out without bound.
pub const MAX_DEPTH: u32 = 2;

pub const DEPTH_VAR: &str = "BRIGADE_DEPTH";

/// Everything brigade reads from its environment, read in one place.
#[derive(Clone, Debug)]
pub struct Settings {
    pub roster_file: PathBuf,
    pub state_dir: PathBuf,
    pub depth: u32,
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
        Ok(Settings {
            roster_file: config.join("brigade/roster.toml"),
            state_dir: state.join("brigade"),
            depth,
        })
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.state_dir.join("runs")
    }

    pub fn worktrees_dir(&self) -> PathBuf {
        self.state_dir.join("worktrees")
    }
}

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
    Ultra,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
            Effort::Ultra => "ultra",
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    Claude,
    Codex,
    Agy,
}

impl Harness {
    pub fn program(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Agy => "agy",
        }
    }

    pub fn product(self) -> &'static str {
        match self {
            Harness::Claude => "Claude Code",
            Harness::Codex => "Codex",
            Harness::Agy => "Antigravity",
        }
    }

    /// The effort levels the harness's CLI accepts. agy's Gemini 4 families only
    /// come in low through xhigh tiers.
    pub fn efforts(self) -> &'static [Effort] {
        use Effort::*;
        match self {
            Harness::Claude => &[Low, Medium, High, Xhigh, Max],
            Harness::Codex => &[Low, Medium, High, Xhigh, Max, Ultra],
            Harness::Agy => &[Low, Medium, High, Xhigh],
        }
    }

    pub fn supports(self, effort: Effort) -> bool {
        self.efforts().contains(&effort)
    }
}

impl fmt::Display for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.program())
    }
}

/// Whether a run may change files. Read-only runs remove the harness's file-edit
/// tools; Codex additionally sandboxes its shell read-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Access {
    ReadOnly,
    Write,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Access::ReadOnly => "read-only",
            Access::Write => "write",
        }
    }
}

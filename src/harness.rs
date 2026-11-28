use crate::custom::CustomHarness;
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

/// The CLI an agent runs in. The three built-in harnesses are handled in code,
/// quirks and all; a custom one is declared in the user's roster.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    Claude,
    Codex,
    Agy,
    Custom(Box<CustomHarness>),
}

/// The built-in harnesses' names, which a custom harness can't take.
pub const BUILT_IN_NAMES: [&str; 3] = ["claude", "codex", "agy"];

impl Harness {
    pub fn built_in(name: &str) -> Option<Harness> {
        match name {
            "claude" => Some(Harness::Claude),
            "codex" => Some(Harness::Codex),
            "agy" => Some(Harness::Agy),
            _ => None,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Agy => "agy",
            Harness::Custom(custom) => &custom.name,
        }
    }

    pub fn program(&self) -> &str {
        match self {
            Harness::Custom(custom) => &custom.program,
            built_in => built_in.name(),
        }
    }

    pub fn product(&self) -> &str {
        match self {
            Harness::Claude => "Claude Code",
            Harness::Codex => "Codex",
            Harness::Agy => "Antigravity",
            Harness::Custom(custom) => custom.product.as_deref().unwrap_or(&custom.name),
        }
    }

    /// The effort levels the harness's CLI accepts. agy's Gemini 4 families only
    /// come in low through xhigh tiers.
    pub fn efforts(&self) -> &[Effort] {
        use Effort::*;
        match self {
            Harness::Claude => &[Low, Medium, High, Xhigh, Max],
            Harness::Codex => &[Low, Medium, High, Xhigh, Max, Ultra],
            Harness::Agy => &[Low, Medium, High, Xhigh],
            Harness::Custom(custom) => &custom.efforts,
        }
    }

    pub fn supports(&self, effort: Effort) -> bool {
        self.efforts().contains(&effort)
    }
}

impl fmt::Display for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
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

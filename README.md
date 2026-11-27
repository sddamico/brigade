# brigade

Tag in a named agent from Claude Code, Codex or Antigravity (agy) and have it run in its own CLI on its own model. Ask for "a sol subagent" from Claude Code and you get GPT-6.2 Sol running in Codex; ask for "a team of three neon subagents" from Codex and you get three Gemini 4 Neon runs in agy.

The same roster works from all three harnesses:

| Agent | Runs in | Model |
|---|---|---|
| `opus`, `fable`, `sonnet` | Claude Code | Claude Opus, Fable, Sonnet |
| `sol`, `astra` | Codex | `gpt-6.2-sol`, `gpt-6.2-astra` |
| `neon`, `hydrogen` | agy | `gemini-4-neon`, `gemini-4-hydrogen` |

You can add your own agents, or change these, in `~/.config/brigade/roster.toml`.

## How it works

brigade is two pieces:

- A CLI, `brigade`, that runs a task on a roster agent headless (`claude -p`, `codex exec`, `agy -p`) and prints its final reply, plus a session id to continue it with
- A plugin for all three harnesses. It has a skill that teaches the host agent the roster and how to start agents, and a subagent definition per agent in each harness's own format

When the host is the agent's own harness and can pin the model, the subagent is native: `brigade:opus` in Claude Code is a Claude subagent on Opus, and `sol` in Codex is a Codex role on `gpt-6.2-sol`. Everything else is a forwarder, a subagent on the host's cheapest model that hands the task to `brigade run` and returns the reply unchanged. agy can only pin model tiers, not model ids, so every agent is a forwarder there.

| Host | Starts an agent with | Native agents |
|---|---|---|
| Claude Code | `Agent(subagent_type: "brigade:<name>")` | opus, fable, sonnet |
| Codex | `spawn_agent(agent_type: "<name>")` | sol, astra |
| agy | `invoke_subagent(TypeName: "<name>")` | none |

Runs are read-only unless asked to write. A run with `--worktree` gets its own git worktree on a `brigade/<run id>` branch, which is how a team of writers avoids editing the same checkout. See [GLOSSARY.md](GLOSSARY.md) for the terms.

## Install

The CLI, with cargo or mise:

```sh
cargo install --git https://github.com/sddamico/brigade --tag v0.1.0
# or, in ~/.config/mise/config.toml:
# "cargo:https://github.com/sddamico/brigade" = "tag:v0.1.0"
```

The plugin, from this repo's marketplace:

```sh
claude plugin marketplace add https://github.com/sddamico/brigade.git
claude plugin install brigade@brigade

codex plugin marketplace add https://github.com/sddamico/brigade.git
codex plugin add brigade@brigade

agy plugin install ~/.claude/plugins/cache/brigade/brigade/0.1.0
```

Codex plugins can't ship subagent roles, so link the roles from Codex's copy of the plugin into `~/.codex/agents`, which Codex searches recursively. Re-point it after a plugin update:

```sh
ln -sfn ~/.codex/plugins/cache/brigade/brigade/0.1.0/codex/agents ~/.codex/agents/brigade
```

Each agent needs its harness's CLI installed and logged in.

## Use

From inside any of the three harnesses, ask for an agent by name: "spin up a sol subagent to review this diff", "have opus check the migration", "use a team of three neon subagents to research these libraries".

From a shell:

```sh
brigade list
brigade run sol "why does the login test flake?"
brigade run astra --write --effort high <<'EOF'
Fix the flaky login test in tests/auth_test.rs; the race is in the session fixture.
EOF
brigade run neon --write --worktree "add a --json flag to the export command"
brigade run sol --resume <session> --cwd <dir> "now add a regression test"

id=$(brigade run hydrogen --detach "audit the error handling in src/" | awk '{print $4}')
brigade wait "$id" --timeout 540
```

Each reply ends with `[brigade]` lines naming the run, and for a session the command that continues it.

## Your own agents

The user roster uses the same format as [roster.toml](roster.toml). An entry with the name of a built-in agent replaces it. `env` and `args` go to the harness's CLI, and `$VAR` and `${VAR}` expand from brigade's environment. For example, Claude Code on a local model behind an Anthropic-compatible server:

```toml
[[agent]]
name = "claude-local"
harness = "claude"
model = "$LOCAL_MODEL"
effort = "medium"
description = "Claude Code on the local model"
args = ["--disallowedTools", "Artifact"]

[agent.env]
ANTHROPIC_BASE_URL = "$LOCAL_MODEL_SERVER"
ANTHROPIC_DEFAULT_HAIKU_MODEL = "$LOCAL_MODEL"
ANTHROPIC_SMALL_FAST_MODEL = "$LOCAL_MODEL"
CLAUDE_CODE_MAX_CONTEXT_TOKENS = "196608"
```

User agents don't get a subagent of their own; the generic `forward` subagent runs them in every harness.

## Development

```sh
cargo test                      # unit tests, CLI tests against fake harness CLIs, generated-file drift
cargo run --example generate    # rewrite plugin/ after changing roster.toml, src/render.rs or the skill
```

`plugin/` is generated from `roster.toml` and committed, because marketplaces install straight from git. Edit the skill's prose in `plugin/skills/brigade/SKILL.md`; the roster table inside it is generated.

## License

MIT

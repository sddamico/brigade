# brigade

Tag in a named agent from Claude Code, Codex or Antigravity (agy) and have it run in its own CLI on its own model. Ask for "a sol subagent" from Claude Code and you get GPT-6.2 Sol running in Codex; ask for "a team of three neon subagents" from Codex and you get three Gemini 4 Neon runs in agy.

The same roster works from all three harnesses. By default each account offers these:

| Account | Agents | Models |
|---|---|---|
| `claude` (Claude Code) | `fable`, `opus`, `sonnet` | Claude Fable, Opus, Sonnet |
| `codex` (Codex) | `sol`, `astra` | `gpt-6.2-sol`, `gpt-6.2-astra` |
| `agy` (Antigravity) | `neon`, `hydrogen` | `gemini-4-neon`, `gemini-4-hydrogen` |

The catalog has more (haiku, terra, carbon, argon, flash, and opus and sonnet through agy) for an account to opt into, and you can add accounts and agents of your own, in `~/.config/brigade/roster.toml`.

## How it works

brigade is two pieces:

- A CLI, `brigade`, that runs a task on a roster agent headless (`claude -p`, `codex exec`, `agy -p`) and prints its final reply, plus a session id to continue it with
- A plugin for all three harnesses. It has a skill that teaches the host agent the roster and how to start agents, and a subagent definition per agent in each harness's own format

brigade only stands in for agents that live in another CLI. An agent from the host's own family stays on the host's ordinary subagents: opus in Claude Code is the Agent tool with `model: "opus"`, and sol in Codex is `spawn_agent` with `model: "gpt-6.2-sol"`. Every other agent gets a forwarder, a subagent on a small model of the host's that hands the task to `brigade run` and returns the reply unchanged.

| Host | Its own agents (no brigade) | Through a brigade forwarder |
|---|---|---|
| Claude Code | opus, fable, sonnet | sol, astra, neon, hydrogen: `Agent(subagent_type: "brigade:<name>")` |
| Codex | sol, astra | opus, fable, sonnet, neon, hydrogen: `spawn_agent(agent_type: "<name>")` |
| agy | neon, hydrogen | opus, fable, sonnet, sol, astra: `invoke_subagent(TypeName: "<name>")` |

agy's own subagents can only pick a model tier, so a native "neon subagent" runs on the session's model; when that isn't neon, the generic `forward` forwarder runs neon exactly.

Runs are read-only unless asked to write, and use the agent's model at the harness's default effort unless the call picks a model or effort. A run with `--worktree` gets its own git worktree on a `brigade/<run id>` branch, which is how a team of writers avoids editing the same checkout. See [GLOSSARY.md](GLOSSARY.md) for the terms.

## Install

The CLI, with cargo or mise:

```sh
cargo install --git https://github.com/sddamico/brigade --tag v0.9.4
# or, in ~/.config/mise/config.toml:
# "cargo:https://github.com/sddamico/brigade" = "tag:v0.9.4"
```

The plugin, from this repo's marketplace:

```sh
claude plugin marketplace add https://github.com/sddamico/brigade.git
claude plugin install brigade@brigade

codex plugin marketplace add https://github.com/sddamico/brigade.git
codex plugin add brigade@brigade

agy plugin install ~/.claude/plugins/cache/brigade/brigade/0.9.4
```

Codex plugins can't ship subagent roles, so link the roles from Codex's copy of the plugin into `~/.codex/agents`, which Codex searches recursively. Re-point it after a plugin update:

```sh
ln -sfn ~/.codex/plugins/cache/brigade/brigade/0.9.4/codex/agents ~/.codex/agents/brigade
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
brigade run sol --model gpt-6.1-sol --effort high "same question, previous Sol"
brigade run sol --resume <session> --cwd <dir> "now add a regression test"
brigade run sol --fallback opus "review this diff"   # opus takes over if sol hits its usage limit

id=$(brigade run hydrogen --detach "audit the error handling in src/" | awk '{print $4}')
brigade wait "$id" --timeout 100
brigade cancel "$id"
```

Each reply ends with `[brigade]` lines naming the run, and for a session the command that continues it. A run that hits its harness's usage limit says so in a `[brigade] <agent> hit a usage limit` line, with when the account frees up, and exits with 4 rather than 1.

Every run belongs to whoever waits on it. `brigade wait` and a foreground `brigade run` renew the run's lease while they wait; a run nobody has waited on for 90 seconds is cancelled, along with everything its harness started. So stopping a forwarder subagent, or the session it was in, stops the agent it started. Ctrl-C, SIGTERM or closing the terminal on a foreground run, and `brigade cancel <run id>`, stop one at once. If brigade's own worker process dies, the next `brigade wait` stops what the harness left running and reports the run as failed.

A run's directory under `$XDG_STATE_HOME/brigade/runs` (`~/.local/state/brigade/runs`) holds its task and the harness's whole output, readable only by you. Finished runs are deleted after a week; `brigade runs --prune` deletes them all now.

A write run in a worktree commits its changes with the task's first line as the subject. A read-only run never commits, even inside a worktree. A run that changed nothing keeps its worktree, so its session can be continued there, until the run is deleted.

brigade can't start a run from inside a host's sandbox, since the agent's CLI needs the network and its own config. Its forwarders ask to run outside it: with `BypassSandbox` in agy, and with `sandbox_permissions: "require_escalated"` in Codex, which asks you to approve it unless your approval settings already allow `brigade`.

## Accounts and routes

The roster is a catalog of agents, and the accounts that offer them. An account is a set of credentials runs draw on and share limits with: a Claude subscription, a ChatGPT plan, an agy login, a local model server. It names its harness, opts into catalog agents, and can define agents of its own. A user roster's accounts replace built-in ones of the same name and add the rest:

```toml
# agy's Claude allowance is separate from its Gemini one, so opus runs there
# when the Claude subscription's opus is limited.
[[account]]
name = "agy"
harness = "agy"
agents = ["neon", "hydrogen", "opus"]

# Only these from the Claude subscription.
[[account]]
name = "claude"
harness = "claude"
agents = ["fable", "opus"]

# A second Claude login.
[[account]]
name = "claude-work"
harness = "claude"
agents = ["opus", "sonnet"]
env = { CLAUDE_CONFIG_DIR = "$HOME/.claude-work" }

# Claude Code on a model server of your own (any Anthropic-compatible
# endpoint), which serves two runs at once.
[[account]]
name = "local"
max_runs = 2

[[account.agent]]
name = "local-claude"
harness = "claude"
model = "$MY_MODEL"
effort = "medium"   # optional; without it the harness's default applies
description = "Claude Code on my own model server"
best_for = "private work that mustn't leave the machine"
env = { ANTHROPIC_BASE_URL = "$MY_MODEL_SERVER" }
```

An agent more than one account offers has a route through each. `brigade run opus` takes the first route with capacity, in account order: built-in accounts first, a user account that replaces one keeping its place, then the user's others. A route that's limited or full is passed over without spending a call, and a run that hits a limit moves to the next route. `--account <account>` pins a route. `env` and `args` go to the harness's CLI, and `$VAR` and `${VAR}` expand from brigade's environment. Catalog entries of your own (`[[catalog]]`, as in [roster.toml](roster.toml)) add agents any account can opt into. `brigade list` shows every agent, what it's best for, its routes, and how its model scored on a benchmark: by default the Artificial Analysis Intelligence Index score, and the cost and minutes of one of its tasks. Each `benchmark = { source = "aa", ... }` names one of the roster's `[benchmark_sources]`, so your own figures stay labelled as yours.

An account on a server that switches models, like a local one, names its model list with `models_url`. `brigade capacity --refresh` asks it which model is loaded, and the account's agents take that model's `[[served_model]]` entry, so switching models needs no roster edit. That takes a server that lists only the model it has loaded: when it lists several, as a router listing every model it can load does, the agents keep their own description. A failed read forgets what the server served before:

```toml
[benchmark_sources.mine]
name = "My own runs of the index's tasks"
url = "https://example.com/my-evals"
read = "2026-11-27"

# One entry per model the server can load, by the id its /v1/models reports.
[[served_model]]
id = "example-model-30b"
best_for = "private, simple or high-volume work"
benchmark = { source = "mine", intelligence = 30 }

[[account]]
name = "local"
models_url = "$MY_MODEL_ENDPOINT/v1/models"
```

## Limits and quotas

brigade remembers when a route hits a usage limit and until when, so the next run on it doesn't spend a call. A limit covers that model alone, since usage can run out for one model and not another (agy's Gemini models and its Claude models draw on separate allowances), except where it covers the account: Claude Code's five-hour and seven-day windows, and a ChatGPT plan's limit. A run whose routes are all limited:

- with a fallback, goes straight to the fallback
- with `--wait`, waits until a route frees up, and waits out a limit it hits while running, too
- otherwise ends at once as limited, naming the reset time

The reset time comes from Claude Code's `rate_limit_event`s (it streams them with every run, along with how much of the usage window is gone), from Codex's limit message, or, where the harness gives none, from a backoff: 30 minutes, doubling each time the one run let through to try it is limited again. A run that gets through clears its model's limit and any on the account.

`brigade capacity --refresh` reads each account's quotas from its harness, without spending a run: Claude Code's `/usage`, agy's `/usage` and the Codex app server's rate limits. They count every session on the account, not just brigade's, so usage from your other sessions shows. A used-up quota holds back every model it covers until it resets. brigade reads them only when asked.

```sh
brigade capacity                 # every account and agent: free, full or limited until when
brigade capacity --refresh       # read every account's quotas first (a few seconds)
brigade capacity --json
brigade capacity --clear agy     # forget a limit you know is over, like topped-up credits
brigade runs --state waiting     # runs and their state: waiting, running, done, failed, limited, cancelled
brigade run sol --wait "..."     # wait out sol's limit instead of ending as limited
brigade run opus --account agy "..."
```

## Your own agents

`brigade install-agents` gives each of your agents a subagent of its own in all three harnesses, so a host can pick it by name like a built-in one. It writes them to `~/.claude/agents`, `~/.codex/agents` and `~/.gemini/config/agents`, and only ever rewrites or removes files it wrote. Run it again after changing the roster or upgrading brigade. Until then, the generic `forward` subagent runs your agents.

A `[fallbacks]` table names the agent a run switches to when every route of its agent is limited. The fallback gets the same task and reports the switch.

```toml
[fallbacks]
sol = "opus"
astra = "opus"
opus = "sol"
```

`--fallback <agent>` picks one for a single run and `--no-fallback` turns it off.

## Your own harnesses

A CLI brigade doesn't know can join through a `[[harness]]` table in your roster, which says how to start it and where its output keeps the reply. Agents then run in it by name, and with a `[harness.host]` section it can tag in brigade agents too.

```toml
[[harness]]
name = "muse"
product = "Muse Code"               # what replies call it; the name when unset
program = "muse-code"
args = ["exec", "--json", "--model", "{model}"]
efforts = ["low", "medium", "high"]
effort_args = ["--effort", "{effort}"]
read_only_args = ["--read-only"]
write_args = ["--yolo"]
resume_args = ["--resume", "{session}"]

[harness.output]
format = "json"
reply = "/result"
session = "/session_id"
error = "/error"
limit_signs = ["usage limit", "RESOURCE_EXHAUSTED"]

[harness.host]
agents_dir = "~/.muse/agents"
file = "{name}.md"
template = """
---
# {marker}
name: {name}
description: {description}
---

{instructions}
"""

[[account]]
name = "muse"
harness = "muse"

[[account.agent]]
name = "muse"
model = "muse-2"
description = "Muse Code on its own model"
```

- **Command line**: `args`, which must use `{model}`, then `effort_args` when the run has an effort (one of `efforts`, from brigade's low to ultra), `read_only_args` or `write_args`, `resume_args` for a resumed run, and the agent's own `args`. `{prompt}` in an argument puts the task there instead of on stdin, and `{cwd}` is the run's directory
- **Read-only**: without `read_only_args`, a read-only run is read-only only because its task says so
- **Output**: `format` is `json` (one object on stdout), `jsonl` (an event per line: the session from the first event that has one, the reply and error from the last) or `text` (stdout is the reply, and a failing exit status a failure). `reply`, `session` and `error` are JSON pointers
- **Failures and limits**: a non-empty error fails the run. One that contains any of `limit_signs`, or a stderr that does, is a usage limit, which gets the same handling and fallbacks as the built-in harnesses' limits
- **Hosting**: `brigade install-agents` writes a forwarder for every agent from another harness, plus `forward`, into `agents_dir` (`~/` and `$VAR` expand). It names each file from `file` and fills `template` from `{name}`, `{description}` (quoted, so it fits YAML and TOML), `{instructions}` and `{marker}`, the line brigade knows its own files by, which goes in a comment. `shell_tool` names the harness's shell tool in the instructions, `shell` by default. brigade cleans up a host's directory only while its host section is in the roster
- The built-in harnesses' names, claude, codex and agy, are taken

## Development

```sh
cargo test                      # unit tests, CLI tests against fake harness CLIs, generated-file drift
cargo run --example generate    # rewrite plugin/ after changing roster.toml, src/render.rs or the skill
```

`plugin/` is generated from `roster.toml` and committed, because marketplaces install straight from git. Edit the skill's prose in `plugin/skills/brigade/SKILL.md`; the roster table inside it is generated.

## License

MIT

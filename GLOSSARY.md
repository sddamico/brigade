# Glossary

**Roster**: the agents brigade knows. The built-in roster is `roster.toml` in this repo; the user roster at `~/.config/brigade/roster.toml` (or `$XDG_CONFIG_HOME/brigade/roster.toml`) adds agents or replaces built-in ones by name.

**Agent**: one roster entry: a name, the harness it runs in and its model, and optionally an effort and extra environment and CLI arguments for that harness. With no effort in the roster or on the call, the harness's own default effort applies.

**Harness**: a CLI an agent runs in: Claude Code (`claude`), Codex (`codex`) or Antigravity (`agy`), or a custom harness.

**Custom harness**: a harness the user declares in a `[[harness]]` table of their roster: its command line as templates, and where its output keeps the reply, session and errors. Agents run in it like a built-in one. A `[harness.host]` section, with an agents directory and a forwarder template, lets it host brigade agents too.

**Host**: the harness the orchestrating agent is working in when it tags an agent in: one of the built-in harnesses, or a custom harness with a host section.

**Native subagent**: the host's ordinary subagent, used for an agent from the host's own family (opus in Claude Code, sol in Codex, neon in agy). brigade ships no definition for these and isn't involved.

**Forwarder**: a subagent definition whose only job is to start a run through the brigade CLI and return the reply. A host gets one for each agent from another harness. Forwarders run on a small model of the host's: sonnet in Claude Code, gpt-6-luna in Codex, the flash tier in agy. `forward` is the generic forwarder, which takes the agent's name from the request and covers user-roster agents. `brigade install-agents` gives each user-roster agent a forwarder of its own, in each harness's user-level agents directory.

**Run**: one task handed to one agent through the brigade CLI. Each run has an id (`<agent>-<hex>`) and a directory under `~/.local/state/brigade/runs/` holding the request, the harness's logs and the outcome.

**Read-only run**: the default. The harness's file-edit tools are removed and the task says not to change files; Codex also sandboxes its shell read-only.

**Write run**: a run started with `--write`, in which the agent may change files.

**Session**: the harness's own conversation id (Claude Code session, Codex thread, agy conversation). Resuming a session continues that conversation.

**Detached run**: a run started with `--detach`, which returns its id at once; `brigade wait` collects the reply. Forwarders use these so a run can outlast the host's limit on one shell call.

**Worker**: the process that runs a run's harness CLI, in a session of its own, with the harness in a process group so cancelling reaches everything the harness started.

**Lease**: a run's claim to stay alive, renewed by whoever waits on it. A worker whose lease lapses (90 seconds by default, `BRIGADE_LEASE_SECS`) cancels its run.

**Cancelled run**: a run stopped by `brigade cancel`, by Ctrl-C on a foreground run, or by its lease lapsing.

**Usage limit**: the harness's account running out of usage, such as Claude Code's session or weekly limit, Codex's usage limit or agy's quota. A run that hits one ends as limited, with exit code 4, apart from other failures, because retrying it before the limit resets fails the same way.

**Fallback**: the agent a run switches to when its agent hits a usage limit, chosen ahead of time in the user roster's `[fallbacks]` table or with `--fallback` on the call. The fallback gets the same task, in the same directory or worktree, on its own model and effort. Only one switch is made: a fallback's own fallback is never followed, and a resumed session never falls back.

**Team**: several runs of one agent at once, one task each.

**Worktree**: a git worktree brigade makes for a run started with `--worktree`, on branch `brigade/<run id>`, so the members of a write team don't edit the same checkout.

**Depth**: how many brigade runs deep a worker is. The host's own session is depth 0; brigade refuses to start a run at depth 2 or more.

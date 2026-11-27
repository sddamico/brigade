# Glossary

**Roster**: the agents brigade knows. The built-in roster is `roster.toml` in this repo; the user roster at `~/.config/brigade/roster.toml` (or `$XDG_CONFIG_HOME/brigade/roster.toml`) adds agents or replaces built-in ones by name.

**Agent**: one roster entry: a name, the harness it runs in, its model and default effort, and optionally extra environment and CLI arguments for that harness.

**Harness**: a CLI an agent runs in: Claude Code (`claude`), Codex (`codex`) or Antigravity (`agy`).

**Host**: the harness the orchestrating agent is working in when it tags an agent in.

**Native subagent**: the host's own subagent definition, pinned to the agent's model, so no brigade run is involved. A host gets one for an agent that runs in the same harness, when the host can pin that model: Claude Code and Codex can, agy can't.

**Forwarder**: a subagent definition whose only job is to start a run through the brigade CLI and return the reply. Forwarders use the host's cheapest model. `forward` is the generic forwarder, which takes the agent's name from the request and covers user-roster agents.

**Run**: one task handed to one agent through the brigade CLI. Each run has an id (`<agent>-<hex>`) and a directory under `~/.local/state/brigade/runs/` holding the request, the harness's logs and the outcome.

**Read-only run**: the default. The harness's file-edit tools are removed and the task says not to change files; Codex also sandboxes its shell read-only.

**Write run**: a run started with `--write`, in which the agent may change files.

**Session**: the harness's own conversation id (Claude Code session, Codex thread, agy conversation). Resuming a session continues that conversation.

**Detached run**: a run started with `--detach`, which returns its id at once; `brigade wait` collects the reply. Forwarders use these so a run can outlast the host's limit on one shell call.

**Team**: several runs of one agent at once, one task each.

**Worktree**: a git worktree brigade makes for a run started with `--worktree`, on branch `brigade/<run id>`, so the members of a write team don't edit the same checkout.

**Depth**: how many brigade runs deep a worker is. The host's own session is depth 0; brigade refuses to start a run at depth 2 or more.

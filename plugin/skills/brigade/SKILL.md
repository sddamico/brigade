---
name: brigade
description: Tag in a named agent from the brigade roster (opus, fable, sonnet, sol, astra, neon, hydrogen, or one from the user's own roster such as claude-local), each running in its own CLI (Claude Code, Codex or Antigravity), alone or as a team. Use when the user asks to spin up, ask, tag in or send a named agent ("spin up a sol subagent", "have opus review this", "use a team of neon subagents", "get claude-local to try it"), or asks which agents brigade has.
---

# brigade

A brigade agent is a named worker that runs in one harness's CLI on one model. You hand it a task and get back its reply. `brigade list` prints the whole roster, including agents the user added in `~/.config/brigade/roster.toml`.

<!-- roster: generated from roster.toml -->
| Agent | Runs in | Model | Effort | What it is |
|---|---|---|---|---|
| `opus` | Claude Code | `opus` | harness default | Claude Opus in Claude Code |
| `fable` | Claude Code | `fable` | harness default | Claude Fable in Claude Code |
| `sonnet` | Claude Code | `sonnet` | harness default | Claude Sonnet in Claude Code |
| `sol` | Codex | `gpt-6.2-sol` | harness default | GPT-6.2 Sol in Codex, OpenAI's go-to model for coding and everyday work |
| `astra` | Codex | `gpt-6.2-astra` | harness default | GPT-6.2 Astra in Codex, OpenAI's most capable model for complex coding |
| `neon` | Antigravity | `gemini-4-neon` | harness default | Gemini 4 Neon in Antigravity |
| `hydrogen` | Antigravity | `gemini-4-hydrogen` | harness default | Gemini 4 Hydrogen in Antigravity |
<!-- /roster -->

## Briefing an agent

The agent knows only what you put in the task. Write it for a colleague who hasn't seen this conversation: the goal, the files and commands that matter, what you've already ruled out, and what you want back.

Runs are read-only unless the task has to change files. Ask for a write run only then. Name a model or effort only when the user did: without one an agent runs on its roster model, at the harness's default effort.

## Starting one, by harness

An agent from your own harness's family runs on your ordinary subagents; brigade only adds the agents that live in another CLI.

### In Claude Code

- opus, fable and sonnet: use the Agent tool as usual, with `model: "opus"` (or `fable`, `sonnet`) and `effort` if one was asked for
- sol, astra, neon and hydrogen: `Agent(subagent_type: "brigade:<name>")`, a forwarder that runs the agent's own CLI. Put the routing in the prompt in plain words: "this is a write run", "use model `<id>`", "use effort high", "use your own worktree", or "continue session `<id>` in `<dir>`"
- An agent from the user's roster, such as claude-local: `brigade:forward`, with `agent: <name>` as the first line of the prompt

### In Codex

- sol and astra: `spawn_agent` with `model: "gpt-6.2-sol"` (or `gpt-6.2-astra`) and `reasoning_effort` if one was asked for
- opus, fable, sonnet, neon and hydrogen: `spawn_agent` with `agent_type: "<name>"`, a forwarder role, with routing in the message as above
- An agent from the user's roster: `agent_type: "forward"`, with `agent: <name>` as the first line of the message
- Collect replies with `wait_agent`

### In Antigravity

- neon and hydrogen: `invoke_subagent` with `TypeName: "self"`. agy subagents can only pick a tier, not a model id, so `Model: "inherit"` keeps the session's own model. When the session is on another model than the one asked for, use `forward` with `agent: neon` instead
- opus, fable, sonnet, sol and astra: `invoke_subagent` with `TypeName: "<name>"`, a forwarder, with routing in the prompt as above
- An agent from the user's roster: `TypeName: "forward"`, with `agent: <name>` at the top of the prompt

### From a shell

Any harness can also run brigade directly:

```
brigade run <agent> [--model <id>] [--effort <level>] [--write] [--worktree] [--cwd <dir>] [--resume <session>] <<'BRIGADE_TASK'
<the task>
BRIGADE_TASK
```

That waits for the reply. `--detach` returns a run id at once instead; `brigade wait <run id> --timeout 100` collects it, and says so if the run is still going.

## Teams

A team is several runs of one agent at once, one task each. Split the work into pieces that don't depend on each other, start every member in the same turn, then collect the replies and combine them yourself. Check with the user before starting more than five.

Read-only members can share the checkout. Write members each need their own worktree: `isolation: "worktree"` on Claude Code's Agent tool, `Workspace: "branch"` on agy's `invoke_subagent`, and "use your own worktree" for a forwarder, which then runs `brigade run --worktree`. Codex subagents have no worktree option, so a Codex write team of sol or astra goes through `forward` with "use your own worktree". A forwarder's reply names its worktree and branch. Review and merge those branches, then remove each worktree with `git worktree remove <path>`.

Runs nest at most two deep: a brigade agent can start one more level of agents, and those can't start any.

## Stopping

Stopping a forwarder stops its run: a run that nobody has waited on for 90 seconds is cancelled, along with everything its harness started. `brigade cancel <run id>` stops one at once.

## Following up

A reply that came through brigade ends with `[brigade]` lines: the run, and for a session the `brigade run ... --resume <session> --cwd <dir>` command that continues it. Pass the session and directory back to a forwarder ("continue session `<id>` in `<dir>`") or run that command yourself. To follow up with one of your own subagents, use your harness's tool (`SendMessage` in Claude Code, `followup_task` in Codex).

## Reporting back

The user sees only what you relay. Say which agent did what, and pass on findings with their evidence. Don't present an agent's claims as checked unless you checked them.

---
name: brigade
description: Tag in a named agent from the brigade roster (opus, fable, sonnet, sol, astra, neon, hydrogen, or one from the user's own roster such as claude-local), each running in its own CLI (Claude Code, Codex or Antigravity), alone or as a team. Use when the user asks to spin up, ask, tag in or send a named agent ("spin up a sol subagent", "have opus review this", "use a team of neon subagents", "get claude-local to try it"), or asks which agents brigade has.
---

# brigade

A brigade agent is a named worker that runs in one harness's CLI on one model. You hand it a task and get back its reply. `brigade list` prints the whole roster, including agents the user added in `~/.config/brigade/roster.toml`.

<!-- roster: generated from roster.toml -->
| Agent | Runs in | Model | Default effort | What it is |
|---|---|---|---|---|
| `opus` | Claude Code | `opus` | medium | Claude Opus in Claude Code |
| `fable` | Claude Code | `fable` | medium | Claude Fable in Claude Code |
| `sonnet` | Claude Code | `sonnet` | medium | Claude Sonnet in Claude Code |
| `sol` | Codex | `gpt-6.2-sol` | medium | GPT-6.2 Sol in Codex, OpenAI's go-to model for coding and everyday work |
| `astra` | Codex | `gpt-6.2-astra` | medium | GPT-6.2 Astra in Codex, OpenAI's most capable model for complex coding |
| `neon` | Antigravity | `gemini-4-neon` | medium | Gemini 4 Neon in Antigravity |
| `hydrogen` | Antigravity | `gemini-4-hydrogen` | medium | Gemini 4 Hydrogen in Antigravity |
<!-- /roster -->

## Briefing an agent

The agent knows only what you put in the task. Write it for a colleague who hasn't seen this conversation: the goal, the files and commands that matter, what you've already ruled out, and what you want back.

Runs are read-only unless the task has to change files. Ask for a write run only then.

## Starting one, by harness

### In Claude Code

Every roster agent has a subagent named `brigade:<name>`, e.g. `Agent(subagent_type: "brigade:sol", prompt: ...)`.

- `brigade:opus`, `brigade:fable` and `brigade:sonnet` are native Claude subagents on that model. Set the call's `effort` to change their effort, and `isolation: "worktree"` to give one its own worktree
- The others are forwarders that run the agent's own CLI through brigade. Put the routing in the prompt in plain words: "this is a write run", "use effort high", "use your own worktree", or "continue session `<id>` in `<dir>`"
- An agent from the user's roster has no subagent of its own. Use `brigade:forward` and start the prompt with `agent: <name>`

### In Codex

- `sol` and `astra` are native roles: `spawn_agent` with `agent_type: "sol"`, plus `reasoning_effort` to change the effort (the roster default is medium). They work in your checkout
- Every other roster agent is a forwarder role of the same name, e.g. `agent_type: "neon"`, with routing in the message as above
- For an agent from the user's roster, or a sol or astra that needs its own worktree, use `agent_type: "forward"` and start the message with `agent: <name>`
- Collect replies with `wait_agent`

### In Antigravity

agy can't pin a subagent to a model id, so every roster agent is a forwarder here, neon and hydrogen included: `invoke_subagent` with `TypeName: "neon"`, or `TypeName: "forward"` with `agent: <name>` at the top of the prompt.

### From a shell

Any harness can also run brigade directly:

```
brigade run <agent> [--effort <level>] [--write] [--worktree] [--cwd <dir>] [--resume <session>] <<'BRIGADE_TASK'
<the task>
BRIGADE_TASK
```

That waits for the reply. `--detach` returns a run id at once instead; `brigade wait <run id> --timeout 540` collects it, and says so if the run is still going.

## Teams

A team is several runs of one agent at once, one task each. Split the work into pieces that don't depend on each other, start every member in the same turn, then collect the replies and combine them yourself. Check with the user before starting more than five.

Read-only members can share the checkout. Write members each need their own worktree: `isolation: "worktree"` for native Claude subagents, and "use your own worktree" for forwarders, which then run `brigade run --worktree`. A forwarder's reply names its worktree and branch. Review and merge those branches, then remove each worktree with `git worktree remove <path>`.

Runs nest at most two deep: a brigade agent can start one more level of agents, and those can't start any.

## Following up

A reply that came through brigade ends with `[brigade]` lines: the run, and for a session the `brigade run ... --resume <session> --cwd <dir>` command that continues it. Pass the session and directory back to a forwarder ("continue session `<id>` in `<dir>`") or run that command yourself. To follow up with a native subagent, use the harness's own tool (`SendMessage` in Claude Code, `followup_task` in Codex).

## Reporting back

The user sees only what you relay. Say which agent did what, and pass on findings with their evidence. Don't present an agent's claims as checked unless you checked them.

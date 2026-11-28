---
name: brigade
description: Tag in a named agent from the brigade roster (opus, fable, sonnet, sol, astra, neon, hydrogen, or one from the user's own roster), each running in its own CLI (Claude Code, Codex or Antigravity), alone or as a team. Use when the user asks to spin up, ask, tag in or send a named agent ("spin up a sol subagent", "have opus review this", "use a team of neon subagents"), or asks which agents brigade has.
---

# brigade

A brigade agent is a named worker that runs in one harness's CLI on one model. You hand it a task and get back its reply. `brigade list` prints the whole roster, including agents the user added in `~/.config/brigade/roster.toml`.

<!-- roster: generated from roster.toml -->
| Agent | Runs in | Model | Effort | What it is |
|---|---|---|---|---|
| `fable` | Claude Code | `fable` | harness default | Claude Fable in Claude Code |
| `opus` | Claude Code | `opus` | harness default | Claude Opus in Claude Code |
| `sonnet` | Claude Code | `sonnet` | harness default | Claude Sonnet in Claude Code |
| `sol` | Codex | `gpt-6.2-sol` | harness default | GPT-6.2 Sol in Codex |
| `astra` | Codex | `gpt-6.2-astra` | harness default | GPT-6.2 Astra in Codex |
| `neon` | Antigravity | `gemini-4-neon` | harness default | Gemini 4 Neon in Antigravity |
| `hydrogen` | Antigravity | `gemini-4-hydrogen` | harness default | Gemini 4 Hydrogen in Antigravity |
<!-- /roster -->

## Briefing an agent

The agent knows only what you put in the task. Write it for a colleague who hasn't seen this conversation: the goal, the files and commands that matter, what you've already ruled out, and what you want back.

Write the task for the agent itself, not for the forwarder that carries it: no "via brigade", "ask sol to" or "return the [brigade] lines". The forwarder hands the task over as written and returns the `[brigade]` lines anyway, and an agent asked to run brigade starts a second run of itself.

Runs are read-only unless the task has to change files. Ask for a write run only then. Name a model or effort only when the user did: without one an agent runs on its roster model, at the harness's default effort.

## Starting one, by harness

An agent from your own harness's family runs on your ordinary subagents; brigade only adds the agents that live in another CLI.

### In Claude Code

- opus, fable and sonnet: use the Agent tool as usual, with `model: "opus"` (or `fable`, `sonnet`) and `effort` if one was asked for
- sol, astra, neon and hydrogen: `Agent(subagent_type: "brigade:<name>")`, a forwarder that runs the agent's own CLI. Put the routing in the prompt in plain words: "this is a write run", "use model `<id>`", "use effort high", "use your own worktree", or "continue session `<id>` in `<dir>`". Don't set `isolation` on a forwarder's Agent call: ask for a worktree in the prompt and brigade makes it
- An agent from the user's roster: its own subagent, `Agent(subagent_type: "<name>")`, once the user has run `brigade install-agents`. Otherwise `brigade:forward`, with `agent: <name>` as the first line of the prompt

### In Codex

- sol and astra: `spawn_agent` with `model: "gpt-6.2-sol"` (or `gpt-6.2-astra`) and `reasoning_effort` if one was asked for
- opus, fable, sonnet, neon and hydrogen: `spawn_agent` with `agent_type: "<name>"`, a forwarder role, with routing in the message as above
- An agent from the user's roster: `agent_type: "<name>"` once the user has run `brigade install-agents`, otherwise `agent_type: "forward"`, with `agent: <name>` as the first line of the message
- Collect replies with `wait_agent`
- Codex runs only a few subagents at once (`[agents] max_threads` in config.toml, 4 by default, counting the session itself). Team members beyond the free slots can run from the shell with `brigade run --detach` and `brigade wait`

### In Antigravity

- neon and hydrogen: `invoke_subagent` with `TypeName: "self"`. agy subagents can only pick a tier, not a model id, so `Model: "inherit"` keeps the session's own model. When the session is on another model than the one asked for, use `forward` with `agent: neon` instead
- opus, fable, sonnet, sol and astra: `invoke_subagent` with `TypeName: "<name>"`, a forwarder, with routing in the prompt as above
- An agent from the user's roster: `TypeName: "<name>"` once the user has run `brigade install-agents`, otherwise `TypeName: "forward"`, with `agent: <name>` at the top of the prompt

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

Read-only members can share the checkout. Write members each need their own worktree: `isolation: "worktree"` on Claude Code's Agent tool for Claude's own subagents, `Workspace: "branch"` on agy's `invoke_subagent`, and "use your own worktree" in a forwarder's prompt, which then runs `brigade run --write --worktree`. Codex subagents have no worktree option, so a Codex write team of sol or astra goes through `forward` with "use your own worktree".

In a worktree member's brief, give paths relative to the repo root, not the checkout's absolute path: brigade tells the agent where its worktree is, and an agent handed the checkout's path may work there instead.

A brigade worktree starts from HEAD on a branch `brigade/<run id>`, and brigade commits the run's changes on that branch when it finishes; the reply's `[brigade]` lines name the commit, branch and worktree. Merge or cherry-pick each branch, then remove it with `git worktree remove <path>` and `git branch -d <branch>`. A worktree has none of the checkout's uncommitted changes or gitignored files (`.env`, `node_modules`, build output), so commit what members need first, and expect them to install dependencies. A failed or cancelled run that changed nothing has its worktree and branch removed for it.

Runs nest at most two deep: a brigade agent can start one more level of agents, and those can't start any.

## Stopping

Stopping a forwarder stops its run: a run that nobody has waited on for 90 seconds is cancelled, along with everything its harness started. `brigade cancel <run id>` stops one at once.

## When a run fails

Pass the error on as brigade gave it.

A run that ends with `[brigade] <agent> hit a usage limit in <harness>` (exit code 4 from the shell) ran out of usage. brigade remembers the limit: the next run on that model, or on the whole account when the limit covers it, ends at once with `<agent> didn't run: ... is still limited`, without spending a call, until the reset time on the `... is limited until ...` line. Don't retry it, and don't swap in a different agent either. Tell the user which agent hit its limit and when it resets, and let them choose what to do. On a team, report what the other members finished.

An agent more than one account offers (opus on both the Claude subscription and agy, say) has a route through each, and brigade runs it on the first with capacity: a route that's limited or full is passed over without spending a call, and the reply says which account ran it (`<agent> hit a usage limit on the <account> account, so <agent> ran on the <other> account instead`). "Use the agy account" in the request pins a route (`--account agy`).

`brigade capacity` shows every account and agent: free, full (at its `max_runs`), or limited and until when, plus how much of Claude's usage window is gone; `brigade capacity --refresh` first reads every account's quotas from its harness, free, counting usage from every session on the account. `--wait` on a run holds it until a route frees up instead of ending as limited; a run whose routes are only full always waits for a slot. `brigade runs` lists runs and their state.

The user can choose a fallback ahead of time: for every run of an agent in the `[fallbacks]` table of their roster (`brigade list` shows them), or for one request ("fall back to opus if sol hits its limit"), which goes into the forwarder's prompt in those words, or `--fallback <agent>` from the shell. "Don't fall back" turns it off for a request. A limited run then hands the same task to the fallback, and its `[brigade]` lines start with `<agent> hit a usage limit in <harness>, so <fallback> ran instead`. Tell the user about the switch. Its `continue it with` line names the fallback, so follow-ups go to the fallback. A resumed session never falls back, since only its own harness can continue it.

## Following up

A reply that came through brigade ends with `[brigade]` lines: the run, and for a session the `brigade run ... --resume <session> --cwd <dir>` command that continues it. Send the follow-up to the same forwarder (`SendMessage` in Claude Code, `followup_task` in Codex); it continues the session itself. A new forwarder needs the session and directory in its prompt ("continue session `<id>` in `<dir>`"), or you can run that command yourself. Follow up with your own native subagents the same way.

## Reporting back

A forwarder's reply always includes the `[brigade]` lines. One without them didn't come from brigade: the forwarder answered by itself. Don't use it or pass it on; start the agent again, or run `brigade run` from the shell.

The user sees only what you relay. Say which agent did what, and pass on findings with their evidence. Don't present an agent's claims as checked unless you checked them.

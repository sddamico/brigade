# Glossary

**Roster**: the agents brigade knows, and the accounts that offer them. The built-in roster is `roster.toml` in this repo; the user roster at `~/.config/brigade/roster.toml` (or `$XDG_CONFIG_HOME/brigade/roster.toml`) is laid over it, its catalog entries and accounts replacing built-in ones by name and adding the rest.

**Catalog**: the agents any account can opt into: each one's model id in every harness that has it, the efforts it takes there, and what it's best for.

**Benchmark**: how an agent's model scored on a benchmark source (the Artificial Analysis Intelligence Index by default): its index score, and where known the cost and minutes of one index task and the effort measured at. It's for comparing agents when picking one for a task, and goes stale, so its source names when it was read.

**Benchmark source**: where benchmark figures come from and when they were read, under a key that each benchmark names (`aa` for the built-in Artificial Analysis figures).

**Served model**: the model a server that switches models has loaded, as its `/v1/models` reports it to `capacity --refresh`. When the server lists exactly one model, an account with `models_url` gives its agents the matching `[[served_model]]` entry's best-for and benchmark.

**Route**: an agent as one account offers it: the harness, model and account a run on it uses. An agent several accounts offer has a route through each, and a run takes the first route with capacity, in account order.

**Agent**: a named intelligence a task can go to, like opus or sol: a catalog entry, or an account's own agent with its model, harness and optionally an effort and extra environment and CLI arguments. With no effort in the roster or on the call, the harness's own default effort applies.

**Harness**: a CLI an agent runs in: Claude Code (`claude`), Codex (`codex`) or Antigravity (`agy`), or a custom harness.

**Custom harness**: a harness the user declares in a `[[harness]]` table of their roster: its command line as templates, and where its output keeps the reply, session and errors. Agents run in it like a built-in one. A `[harness.host]` section, with an agents directory and a forwarder template, lets it host brigade agents too.

**Host**: the harness the orchestrating agent is working in when it tags an agent in: one of the built-in harnesses, or a custom harness with a host section.

**Native subagent**: the host's ordinary subagent, used for an agent from the host's own family (opus in Claude Code, sol in Codex, neon in agy). brigade ships no definition for these and isn't involved.

**Forwarder**: a subagent definition whose only job is to start a run through the brigade CLI and return the reply. A host gets one for each agent from another harness. Forwarders run on a small model of the host's: sonnet in Claude Code and gpt-6.1-terra in Codex. In agy they run on the session's own model, since agy's small tiers are all Gemini, whose allowance runs out apart from its Claude one. `forward` is the generic forwarder, which takes the agent's name from the request and covers user-roster agents. `brigade install-agents` gives each user-roster agent a forwarder of its own, in each harness's user-level agents directory.

**Run**: one task handed to one agent through the brigade CLI. Each run has an id (`<agent>-<hex>`, naming the agent asked for, which stays the id after a fallback) and a private directory under `~/.local/state/brigade/runs/` holding the request, the harness's logs (one set per route it tried) and the outcome. Finished runs are deleted after a week.

**Read-only run**: the default. The harness's file-edit tools are removed and the task says not to change files; Codex also sandboxes its shell read-only.

**Write run**: a run started with `--write`, in which the agent may change files.

**Session**: the harness's own conversation id (Claude Code session, Codex thread, agy conversation). Resuming a session continues that conversation.

**Detached run**: a run started with `--detach`, which returns its id at once; `brigade wait` collects the reply. Forwarders use these so a run can outlast the host's limit on one shell call.

**Worker**: the process that runs a run's harness CLI, in a session of its own, with the harness in a process group so cancelling reaches everything the harness started.

**Lease**: a run's claim to stay alive, renewed by whoever waits on it. A worker whose lease lapses (90 seconds by default, `BRIGADE_LEASE_SECS`) cancels its run.

**Cancelled run**: a run stopped by `brigade cancel`, by Ctrl-C on a foreground run, or by its lease lapsing.

**Usage limit**: the harness's account running out of usage, such as Claude Code's session or weekly limit, Codex's usage limit or agy's quota. A run that hits one ends as limited, with exit code 4, apart from other failures, because retrying it before the limit resets fails the same way.

**Fallback**: the agent a run switches to when its agent hits a usage limit, chosen ahead of time in the user roster's `[fallbacks]` table or with `--fallback` on the call. The fallback gets the same task, in the same directory or worktree, on its own model and effort. Only one switch is made: a fallback's own fallback is never followed, and a resumed session never falls back.

**Account**: credentials runs draw on, and so share usage limits and slots with: a Claude subscription, a ChatGPT plan, an agy login, a local model server. It names its harness, the catalog agents it offers and agents of its own, `max_runs`, and environment and arguments for every run on it.

**Quota**: a usage window a set of models on an account share, as the harness reports it when brigade reads it (`capacity --refresh`): Claude Code's weekly windows, agy's model groups, Codex's five-hour and weekly limits. A used-up quota holds back every model it covers until it resets.

**Capacity**: whether an account can take another run now: not while it's limited, or while it has its `max_runs` running.

**Limit state**: what brigade remembers about a usage limit: since when, until when if the harness said, otherwise when it lets one run try again, and the harness's message. It covers one model on an account, or the whole account when the limit spans it (Claude Code's five-hour and seven-day windows, and a ChatGPT plan's limit). A run that gets through clears its model's limit and any on the account.

**Waiting run**: a run brigade has accepted but holds until its account has capacity: for a slot on a full account, or for a limit to reset when the run asked to `--wait`.

**Team**: several runs of one agent at once, one task each.

**Worktree**: a git worktree brigade makes for a run started with `--worktree`, on branch `brigade/<run id>`, so the members of a write team don't edit the same checkout.

**Depth**: how many brigade runs deep a worker is. The host's own session is depth 0; brigade refuses to start a run at depth 2 or more.

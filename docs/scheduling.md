# Scheduling across accounts

## Goal

A user runs one long-lived scheduling agent. It takes in work, breaks it into tasks, and hands each task to an agent on whichever account has capacity for it. When an account hits a usage limit, work stops going there and moves to another account, or waits for the reset. Tasks keep moving without the user watching limits.

## Decisions

- **Split decisions.** brigade does the mechanical part: it never starts a run on an account it knows is limited or full, holds a run until capacity frees, and records when each limit resets. The scheduling agent decides what each task needs and which intelligence fits.
- **No queue in brigade.** The scheduling agent keeps its own task list. brigade's only queue is runs waiting for capacity.
- **CLI and skill.** Every command the scheduler needs prints `--json`, and a scheduler skill teaches the workflow. An MCP server can come later.

## Terms

- **Account**: the credentials an agent's runs draw on, and so share limits with. sol and astra share a ChatGPT account; opus, fable and sonnet a Claude subscription; agents on a local model server share that server.
- **Capacity**: whether an account can take another run now. Not when it's limited, or when it already has its `max_runs` running.
- **Limit state**: what brigade remembers about a limit: since when, until when (if known) and the harness's message. It covers one model on an account, or the whole account when the harness says so (Claude Code's five-hour and seven-day windows): usage can run out for one model and not another, as agy credits did for hydrogen while neon still worked.
- **Waiting run**: a run brigade has accepted but holds until its account has capacity.
- **Scheduler**: the long-lived agent that assigns tasks using brigade's commands.

## Where reset times come from

| Harness | Signal | Reset time |
|---|---|---|
| Claude Code | `--output-format stream-json` emits `rate_limit_event` on every run: status (allowed, warning, rejected), limit type, `resetsAt` | Exact, and warns before the limit |
| Codex | Limit message ("try again at Nov 27th, 2026 12:09 AM"); `codex app-server`'s `account/rateLimits/read` gives usage and resets without spending tokens | From the message; exact from the app server (phase 2) |
| agy | stderr `AGY_ERROR` `RESOURCE_EXHAUSTED`, no reset time | Unknown: retried after a backoff, or cleared by the user |
| Custom | `limit_signs` | Unknown unless the message has a time brigade can read |

## Phase 1: accounts and limit state

- **Roster**: agents get `account`. It defaults to the harness name (`claude`, `codex`, `agy`, or the custom harness's). An agent on other credentials, like one on a local model server, names its own. An optional `[[account]]` table sets `max_runs`, e.g. a local server that serves two runs at once
- **Limit state** lives in `~/.local/state/brigade/accounts/<account>.json`:
  - written when a run ends limited, or when Claude reports `rejected`
  - cleared once the reset time passes, or when a run on the account succeeds
  - with no reset time, the account counts as limited for a backoff (30 minutes, then doubling), then gets one probing run
- **Claude Code** runs switch to `stream-json`, so every run records `rate_limit_event`. The reply comes from the stream's final `result` event, which has the same fields as today's single object
- **Codex** limit messages and Claude's `resets ...` text are read into a reset time where the harness gives none in structured form
- **Before a run starts**, brigade checks its account:
  - limited, with a fallback: falls back at once, without spending a call
  - limited or full, with `--wait`: the run waits, then starts when capacity frees. It's kept alive by its waiter's lease, like any run
  - limited, otherwise: ends at once as limited, naming the reset time
  - full, otherwise: waits, since a free slot is usually minutes away
- **`brigade capacity [--json]`** shows, per account:
  - its agents
  - runs going, out of `max_runs`
  - limited since and until, with the message
  - Claude's last utilization warning
- **`brigade capacity --clear <account>`** is for a limit the user knows is over, like agy credits topped up
- **`brigade runs [--json] [--state ...]`** lists runs with agent, account, state (waiting, running, done, failed, limited, cancelled) and age
- **Limited replies** name the reset time when brigade knows it: `[brigade] sol hit a usage limit in Codex; the chatgpt account is limited until 00:09`

**Done when**:
- A second run on a limited account doesn't spend a call
- A limited account with a fallback switches at once
- A full local server queues runs and starts each one as a slot frees
- `brigade capacity` shows all of it
- Verified with real Claude and Codex limits where possible, and fake CLIs for the rest

## Phase 2

Revised after phase 1, once it turned out every harness reports usage for free and agy offers Claude models on a separate allowance.

1. **Quotas and routes** (0.7.0)
   - Accounts own agents: each opts into catalog agents or defines its own
   - An agent several accounts offer has a route through each, and a run takes the first with capacity. Opus runs on agy's Claude allowance when the subscription's opus is limited
   - `brigade capacity --refresh` reads quotas from Claude Code's `/usage`, agy's `/usage` (model groups) and the Codex app server's `account/rateLimits/read`. They count usage from every session on the account, not just brigade's. Reads happen only on request
2. **`brigade agents` and the "full brigade" skill**: one command, `--json`, listing every agent with its routes, what it's best for, its quotas and whether it can run now. The skill triggers on "you have the full brigade available to you", for any orchestrator, and points it there
3. **`brigade mcp`**: tools for agents, capacity, run, wait, cancel, runs and `next_event`, plus the MCP Events extension (SEP-3415: `events/list`, `events/poll`, `events/stream`) over a persistent event log, so a scheduler that reconnects catches up. `next_event` is for hosts without Events support; Claude Code shows none yet, Codex appears to have it
4. **The scheduler**: an agent definition and skill. It keeps its task list, reads `brigade agents`, starts runs (or launches agents through other tools, such as workspace managers, mapping agents to their profiles itself), collects results from events, and reports

## Later

- `brigade mcp`, exposing capacity, runs and watch as typed tools
- Cost or quota budgets per account

## Risks

- Reading reset times from messages breaks when a CLI rewords them. Structured sources come first, and a message brigade can't read leaves the reset unknown and falls back to the backoff rather than guessing
- `stream-json` changes how Claude runs are read. The CLI tests' fake claude and a real run both need it
- Account state is shared across concurrent brigade processes, so slots are claimed under a lock file

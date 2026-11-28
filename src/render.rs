//! Renders the plugin from the built-in roster: each harness's agent
//! definitions, the plugin manifests and the skill's roster table. The output is
//! committed, because marketplaces install the plugin straight from git;
//! `cargo run --example generate` rewrites it and a test fails on drift.

use crate::custom;
use crate::harness::Harness;
use crate::report::WAIT_SLICE_SECS;
use crate::roster::{Agent, FORWARD, Roster, Source};
use serde::Serialize;
use serde_json::json;
use std::path::PathBuf;

pub const PLUGIN_DIR: &str = "plugin";
pub const SKILL_FILE: &str = "plugin/skills/brigade/SKILL.md";
const ROSTER_START: &str = "<!-- roster: generated from roster.toml -->";
const ROSTER_END: &str = "<!-- /roster -->";

const DESCRIPTION: &str = "Tag in named agents (sol, neon, opus...) that each run in their own CLI \
    (Claude Code, Codex or Antigravity), singly or as a team, from any of the three";
const REPOSITORY: &str = "https://github.com/sddamico/brigade";
const AUTHOR: &str = "Stephen D'Amico";
const AUTHOR_URL: &str = "https://github.com/sddamico";
const KEYWORDS: [&str; 5] = ["subagents", "multi-agent", "codex", "antigravity", "claude-code"];

/// Forwarders only pass a task along, so each harness runs them on a small
/// model, the smallest that relays replies faithfully.
// haiku paraphrased long replies, and once a [brigade] line, despite the instructions.
const CLAUDE_FORWARDER_MODEL: &str = "sonnet";
// gpt-6-luna reworded about one relay in four.
const CODEX_FORWARDER_MODEL: &str = "gpt-6.1-terra";
const CODEX_FORWARDER_EFFORT: &str = "low";
// The session's own model: agy's tiers all draw on its Gemini allowance, which
// runs out apart from its Claude one, and a forwarder that can't start leaves a
// Claude-hosted session with no brigade at all.
const AGY_FORWARDER_TIER: &str = "inherit";

/// Directories whose every file is generated, so the generator deletes what it
/// no longer writes.
pub const GENERATED_DIRS: [&str; 3] = ["plugin/claude/agents", "plugin/codex/agents", "plugin/agents"];

pub const HOSTS: [Harness; 3] = [Harness::Claude, Harness::Codex, Harness::Agy];

/// Marks a forwarder `brigade install-agents` wrote, so it only ever rewrites
/// or removes its own files among the user's other agents.
pub const INSTALLED_MARK: &str =
    "Written by `brigade install-agents` from your brigade roster, which rewrites or removes it.";

/// Whether `host` gets a brigade forwarder for `agent`. An agent from the host's
/// own family runs on the host's ordinary subagents instead, so brigade never
/// stands in for them.
pub fn forwards(host: &Harness, agent: &Agent) -> bool {
    agent.harness.name() != host.name()
}

fn forwarded<'a>(host: &Harness, agents: &[&'a Agent]) -> Vec<&'a Agent> {
    agents.iter().copied().filter(|a| forwards(host, a)).collect()
}

pub fn plugin_files(roster: &Roster, version: &str, skill_template: &str) -> Result<Vec<(PathBuf, String)>, String> {
    let agents: Vec<&Agent> = roster.agents().iter().map(|e| &e.agent).collect();
    let mut files = vec![
        (PathBuf::from(".claude-plugin/marketplace.json"), marketplace(version)),
        (
            plugin_path(".claude-plugin/plugin.json"),
            claude_manifest(version, &agents),
        ),
        (plugin_path(".codex-plugin/plugin.json"), codex_manifest(version)),
        (plugin_path(".antigravity-plugin/plugin.json"), agy_manifest(version)),
        (PathBuf::from(SKILL_FILE), skill(skill_template, &agents)?),
    ];
    for host in &HOSTS {
        for agent in forwarded(host, &agents) {
            let (path, contents) = match host {
                Harness::Claude => (format!("claude/agents/{}.md", agent.name), claude_agent(agent, &agents)),
                Harness::Codex => (format!("codex/agents/{}.toml", agent.name), codex_agent(agent, &agents)),
                Harness::Agy => (format!("agents/{}.md", agent.name), agy_agent(agent, &agents)),
                Harness::Custom(_) => unreachable!("the plugin is for the built-in hosts"),
            };
            files.push((plugin_path(&path), contents));
        }
    }
    files.push((
        plugin_path(&format!("claude/agents/{FORWARD}.md")),
        claude_forward(&agents),
    ));
    files.push((
        plugin_path(&format!("codex/agents/{FORWARD}.toml")),
        codex_forward(&agents),
    ));
    files.push((plugin_path(&format!("agents/{FORWARD}.md")), agy_forward(&agents)));
    Ok(files)
}

/// One forwarder `brigade install-agents` writes into a host's agents directory.
pub struct Installed {
    pub host: Harness,
    pub file: String,
    pub contents: String,
}

/// The forwarders the published plugin can't ship, because they come from the
/// user's roster:
/// - in the built-in hosts, one for each of the user's own agents. Every host
///   gets one, since an agent's env and args set it apart from the host's own
///   subagents. A user entry that replaces a built-in agent keeps the plugin's
///   forwarder
/// - in a custom harness that can host, one for each agent from another
///   harness, plus `forward`, as the plugin gives the built-in hosts
pub fn installed_forwarders(roster: &Roster) -> Vec<Installed> {
    let agents: Vec<&Agent> = roster.agents().iter().map(|e| &e.agent).collect();
    let built_in = Roster::built_in();
    let mut files = Vec::new();
    for entry in roster.agents() {
        let agent = &entry.agent;
        if entry.source != Source::User || built_in.get(&agent.name).is_some() {
            continue;
        }
        for host in HOSTS {
            let (file, contents) = match host {
                Harness::Claude => (format!("{}.md", agent.name), marked(&claude_agent(agent, &agents))),
                Harness::Codex => (
                    format!("{}.toml", agent.name),
                    format!("# {INSTALLED_MARK}\n{}", codex_agent(agent, &agents)),
                ),
                Harness::Agy => (format!("{}.md", agent.name), marked(&agy_agent(agent, &agents))),
                Harness::Custom(_) => unreachable!("HOSTS are the built-in hosts"),
            };
            files.push(Installed { host, file, contents });
        }
    }
    for host in roster.harnesses() {
        let Some(spec) = &host.host else { continue };
        let host = Harness::Custom(Box::new(host.clone()));
        let mut write = |name: &str, description: String, instructions: String| {
            files.push(Installed {
                host: host.clone(),
                file: custom::fill(&spec.file, &[("name", name)]),
                contents: custom::fill(
                    &spec.template,
                    &[
                        ("name", name),
                        ("description", &yaml_str(&description)),
                        ("marker", INSTALLED_MARK),
                        ("instructions", &instructions),
                    ],
                ),
            })
        };
        for agent in forwarded(&host, &agents) {
            write(
                &agent.name,
                forwarder_description(agent, &host),
                forwarder_body(Some(agent), &host, &agents, &spec.shell_tool),
            );
        }
        write(
            FORWARD,
            forward_description(&host, &agents),
            forwarder_body(None, &host, &agents, &spec.shell_tool),
        );
    }
    files
}

fn marked(markdown: &str) -> String {
    let rest = markdown
        .strip_prefix("---\n")
        .expect("forwarders start with frontmatter");
    format!("---\n# {INSTALLED_MARK}\n{rest}")
}

fn plugin_path(rel: &str) -> PathBuf {
    PathBuf::from(PLUGIN_DIR).join(rel)
}

fn pretty(value: serde_json::Value) -> String {
    serde_json::to_string_pretty(&value).expect("json values serialize") + "\n"
}

fn author() -> serde_json::Value {
    json!({ "name": AUTHOR, "url": AUTHOR_URL })
}

fn marketplace(version: &str) -> String {
    pretty(json!({
        "name": "brigade",
        "owner": author(),
        "description": DESCRIPTION,
        "plugins": [{
            "name": "brigade",
            "source": "./plugin",
            "description": DESCRIPTION,
            "version": version,
        }],
    }))
}

fn claude_manifest(version: &str, agents: &[&Agent]) -> String {
    let files: Vec<String> = forwarded(&Harness::Claude, agents)
        .iter()
        .map(|a| a.name.as_str())
        .chain([FORWARD])
        .map(|name| format!("./claude/agents/{name}.md"))
        .collect();
    pretty(json!({
        "name": "brigade",
        "version": version,
        "description": DESCRIPTION,
        "author": author(),
        "homepage": REPOSITORY,
        "repository": REPOSITORY,
        "license": "MIT",
        "keywords": KEYWORDS,
        "agents": files,
        "skills": "./skills/",
    }))
}

fn codex_manifest(version: &str) -> String {
    pretty(json!({
        "name": "brigade",
        "version": version,
        "description": DESCRIPTION,
        "author": author(),
        "homepage": REPOSITORY,
        "repository": REPOSITORY,
        "license": "MIT",
        "keywords": KEYWORDS,
        "skills": "./skills/",
        "interface": {
            "displayName": "Brigade",
            "shortDescription": "Tag in agents that run in their own CLI",
            "longDescription": DESCRIPTION,
            "developerName": AUTHOR,
            "category": "Developer Tools",
            "capabilities": ["Skills", "Subagents"],
            "websiteURL": REPOSITORY,
            "defaultPrompt": [
                "Use $brigade to have opus review this change.",
                "Use $brigade to send a team of three neon subagents to research this.",
            ],
        },
    }))
}

fn agy_manifest(version: &str) -> String {
    pretty(json!({
        "name": "brigade",
        "displayName": "Brigade",
        "version": version,
        "description": DESCRIPTION,
        "author": author(),
        "homepage": REPOSITORY,
        "repository": REPOSITORY,
        "license": "MIT",
        "keywords": KEYWORDS,
    }))
}

fn asks_for(name: &str) -> String {
    format!(
        "Use when the user asks for {name} by name: \"spin up {name}\", \"ask {name}\", \
         \"have {name} review this\", \"a team of {name} subagents\"."
    )
}

fn forwarder_description(agent: &Agent, host: &Harness) -> String {
    let best_for = agent
        .best_for
        .as_deref()
        .map(|b| format!(" (best for {b})"))
        .unwrap_or_default();
    format!(
        "{}{best_for}, run in its own CLI through brigade. {} Write the prompt as the agent's own task; its reply comes \
         back with the [brigade] lines without asking. Say in the prompt if the task should change files, \
         which model or effort to use, which session to continue, or which agent to fall back to on a usage limit.{}",
        agent.description,
        asks_for(&agent.name),
        worktree_hint(host),
    )
}

/// Hosts read subagent descriptions every time but load the skill only
/// sometimes, so the one thing that doubles up worktrees is said here.
fn worktree_hint(host: &Harness) -> &'static str {
    match host {
        Harness::Claude => {
            " For its own worktree, write \"use your own worktree\" in the prompt and leave the Agent call's \
             `isolation` unset; brigade makes the worktree."
        }
        Harness::Agy => {
            " For its own worktree, write \"use your own worktree\" in the prompt and leave `Workspace` unset; \
             brigade makes the worktree."
        }
        Harness::Codex => " For its own worktree, write \"use your own worktree\" in the message.",
        Harness::Custom(_) => " For its own worktree, write \"use your own worktree\" in the prompt.",
    }
}

/// The host's own family, which it runs on its ordinary subagents, and how.
/// agy's subagents can't pick a model, and brigade doesn't know whether a
/// custom host's can, so their forwarders cover their own family too (neon from
/// a session on another model).
fn native_family<'a>(host: &Harness, agents: &[&'a Agent]) -> Option<(Vec<&'a str>, &'static str)> {
    let how = match host {
        Harness::Claude => "the Agent tool with `model` set",
        Harness::Codex => "`spawn_agent` with `model` set",
        Harness::Agy | Harness::Custom(_) => return None,
    };
    Some((own_family(host, agents), how))
}

fn own_family<'a>(host: &Harness, agents: &[&'a Agent]) -> Vec<&'a str> {
    agents
        .iter()
        .filter(|a| !forwards(host, a))
        .map(|a| a.name.as_str())
        .collect()
}

fn forward_description(host: &Harness, agents: &[&Agent]) -> String {
    let mut description = "Hands a task to a brigade agent that has no subagent of its own here, such as one from \
        the user's roster (~/.config/brigade/roster.toml). Start the prompt with `agent: <name>`; \
        `brigade list` shows the roster."
        .to_string();
    match native_family(host, agents) {
        Some((names, how)) => {
            description.push_str(&format!(" Not for {}: run those on {how}.", names.join(", ")));
        }
        None => {
            let names = own_family(host, agents);
            if !names.is_empty() {
                description.push_str(&format!(
                    " Also for {} when this session runs on another model.",
                    crate::report::listing(&names, "or")
                ));
            }
        }
    }
    description.push_str(worktree_hint(host));
    description
}

/// The instructions every forwarder follows. `agent` is None for the generic
/// forwarder, which reads the agent's name from the request.
fn forwarder_body(agent: Option<&Agent>, host: &Harness, agents: &[&Agent], shell_tool: &str) -> String {
    let (intro, name, effort) = match agent {
        Some(agent) => {
            let levels: Vec<&str> = agent.efforts().iter().map(|e| e.as_str()).collect();
            (
                format!(
                    "You hand one task to the brigade agent `{}`, which runs in {}, and pass back its reply.",
                    agent.name,
                    agent.harness.product(),
                ),
                agent.name.clone(),
                format!(
                    "when it names one of: {}. If it names an effort that isn't on that list, don't run anything: \
                     reply that {} doesn't take that effort and which ones it does",
                    levels.join(", "),
                    agent.name
                ),
            )
        }
        None => {
            let mut intro = "You hand one task to a brigade roster agent and pass back its reply. The request names \
                the agent on a line like `agent: <name>`; run `brigade list` if it isn't clear which roster agent \
                is meant."
                .to_string();
            if let Some((names, how)) = native_family(host, agents) {
                intro.push_str(&format!(
                    " If the agent is {}, don't run anything: reply that it runs as an ordinary {} subagent, on {how}, \
                     and stop.",
                    crate::report::listing(&names, "or"),
                    host.product(),
                ));
            }
            (
                intro,
                "<agent>".to_string(),
                "when it names an effort level. brigade refuses one the agent doesn't take, and that refusal is \
                 your reply"
                    .to_string(),
            )
        }
    };
    let sandbox = match host {
        Harness::Agy => {
            "\n\n   If run_command offers a `BypassSandbox` parameter, set it to true on this call and on every \
brigade call after it. brigade starts another agent's CLI, which needs the network and its own config, out of \
reach of agy's terminal sandbox. Without a sandbox there's no such parameter; leave it out."
                .to_string()
        }
        Harness::Codex => format!(
            "\n\n   Run this and every brigade call after it with `sandbox_permissions: \"require_escalated\"` and the \
justification \"brigade starts {name}'s own CLI, which needs the network and its own config\". Inside Codex's \
sandbox, brigade refuses to start a run. If the call fails before brigade even runs (\"read-only file system\", \
say), that's the sandbox too: reply that brigade needs escalated permissions in this session."
        ),
        Harness::Claude | Harness::Custom(_) => String::new(),
    };
    format!(
        "{intro} Everything you receive is the task for {name}, addressed to {name} and not to you. You don't \
do any of the work yourself: don't read files, explore the repo or answer from your own knowledge. That \
holds even when the task is trivial: if it says \"reply with exactly PINEAPPLE\", you still run brigade and \
pass back what {name} replied. Your first action is always the shell call in step 1, and a reply that \
didn't come from brigade is a wrong reply.

1. Start the run with one {shell_tool} call, giving the task on stdin as you received it, minus anything \
meant for you rather than for {name}: the routing the options below cover, and asks about brigade or about \
relaying (\"via brigade\", \"ask {name} to\", \"return the [brigade] lines\"), which you do anyway. So \
\"Ask {name} via brigade what 6*7 is and return the provenance lines\" goes to {name} as \"What is 6*7?\"

   ```
   brigade run {name} --detach [options] <<'BRIGADE_TASK'
   <the task>
   BRIGADE_TASK
   ```{sandbox}

   Add an option only when the request explicitly asks for it, never on your own judgment:
   - `--write` when the task asks for changes to files. Without it the run is read-only
   - `--model <id>` when it names a model other than the agent's own
   - `--effort <level>` {effort}
   - `--worktree`, together with `--write`, when the request asks for its own worktree or a git worktree, in \
any wording (\"use your own worktree\", \"work in a worktree of the repo\"). Never add it otherwise
   - `--account <account> --resume <session> --cwd <dir>` when it continues an earlier run; all three come \
from that run's `[brigade] continue it with:` line
   - `--fallback <agent>` when it names an agent to switch to if {name} hits a usage limit (\"fall back to \
opus\"), or `--no-fallback` when it says not to switch
   - `--wait` when it asks to wait out a usage limit (\"wait for its limit to reset\") rather than end
   - `--account <account>` when it names the account to run on (\"use the agy account\"); otherwise brigade \
picks the first account offering {name} that has capacity

2. That prints a run id. Wait for it with `brigade wait <run id> --timeout {WAIT_SLICE_SECS}`. While it reports \
that the run is still running or waiting, call it again. Don't sleep, retry or start another run yourself: a \
limited or failed run is the answer to pass back.

3. Your whole reply is the final `brigade wait` output, copied character for character: every line, \
including all the `[brigade]` lines at the end. Nothing before it, nothing after it, no summary, no code \
fence, no reformatting. The `[brigade]` lines tell the caller how to continue the session and where the \
changes are. If brigade fails, reply with its error output the same way.

4. You may get follow-up messages after your reply. Each one is a new task for {name} in the same session: \
run it through steps 1 to 3, adding the `--account <account> --resume <session> --cwd <dir>` (and any \
`--write`, `--model` or `--effort`) from your previous run's `[brigade] continue it with:` line. When that line names another agent \
than {name}, because the run fell back to it, run that agent. Never answer a follow-up from what you \
remember.
"
    )
}

/// JSON strings are valid YAML double-quoted scalars, which keeps colons and
/// quotes in descriptions from breaking the frontmatter.
fn yaml_str(value: &str) -> String {
    serde_json::to_string(value).expect("strings serialize")
}

fn claude_agent(agent: &Agent, agents: &[&Agent]) -> String {
    claude_forwarder(
        &agent.name,
        &forwarder_description(agent, &Harness::Claude),
        &forwarder_body(Some(agent), &Harness::Claude, agents, "Bash"),
    )
}

fn claude_forward(agents: &[&Agent]) -> String {
    claude_forwarder(
        FORWARD,
        &forward_description(&Harness::Claude, agents),
        &forwarder_body(None, &Harness::Claude, agents, "Bash"),
    )
}

fn claude_forwarder(name: &str, description: &str, body: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: {}\nmodel: {CLAUDE_FORWARDER_MODEL}\ntools: Bash\n---\n\n{body}",
        yaml_str(description),
    )
}

#[derive(Serialize)]
struct CodexRole<'a> {
    name: &'a str,
    description: &'a str,
    model: &'a str,
    model_reasoning_effort: &'a str,
    developer_instructions: &'a str,
}

fn codex_agent(agent: &Agent, agents: &[&Agent]) -> String {
    codex_role(&CodexRole {
        name: &agent.name,
        description: &forwarder_description(agent, &Harness::Codex),
        model: CODEX_FORWARDER_MODEL,
        model_reasoning_effort: CODEX_FORWARDER_EFFORT,
        developer_instructions: &forwarder_body(Some(agent), &Harness::Codex, agents, "shell"),
    })
}

fn codex_forward(agents: &[&Agent]) -> String {
    codex_role(&CodexRole {
        name: FORWARD,
        description: &forward_description(&Harness::Codex, agents),
        model: CODEX_FORWARDER_MODEL,
        model_reasoning_effort: CODEX_FORWARDER_EFFORT,
        developer_instructions: &forwarder_body(None, &Harness::Codex, agents, "shell"),
    })
}

fn codex_role(role: &CodexRole) -> String {
    toml::to_string(role).expect("roles serialize")
}

fn agy_agent(agent: &Agent, agents: &[&Agent]) -> String {
    agy_forwarder(
        &agent.name,
        &forwarder_description(agent, &Harness::Agy),
        &forwarder_body(Some(agent), &Harness::Agy, agents, "run_command"),
    )
}

fn agy_forward(agents: &[&Agent]) -> String {
    agy_forwarder(
        FORWARD,
        &forward_description(&Harness::Agy, agents),
        &forwarder_body(None, &Harness::Agy, agents, "run_command"),
    )
}

fn agy_forwarder(name: &str, description: &str, body: &str) -> String {
    // The forwarder's shell call starts other CLIs, which need the network and
    // their own config dirs, so it can't run in agy's terminal sandbox.
    format!(
        "---\nname: {name}\ndescription: {}\nmodel: {AGY_FORWARDER_TIER}\ntools: [run_command]\n\
         commandExecutionPolicy: eager\nmainAgent: false\n---\n\n{body}",
        yaml_str(description),
    )
}

fn skill(template: &str, agents: &[&Agent]) -> Result<String, String> {
    let start = template
        .find(ROSTER_START)
        .ok_or_else(|| format!("{SKILL_FILE} has no `{ROSTER_START}` marker"))?;
    let end = template
        .find(ROSTER_END)
        .ok_or_else(|| format!("{SKILL_FILE} has no `{ROSTER_END}` marker"))?;
    let mut table = String::from("| Agent | Runs in | Model | Effort | What it is |\n|---|---|---|---|---|\n");
    for agent in agents {
        table.push_str(&format!(
            "| `{}` | {} | `{}` | {} | {} |\n",
            agent.name,
            agent.harness.product(),
            agent.model,
            agent.effort.map_or("harness default".to_string(), |e| e.to_string()),
            agent.description,
        ));
    }
    Ok(format!(
        "{}{ROSTER_START}\n{table}{}",
        &template[..start],
        &template[end..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn hosts_get_forwarders_only_for_other_harnesses() {
        let skill = format!("{ROSTER_START}\n{ROSTER_END}\n");
        let files = plugin_files(&Roster::built_in(), "0.0.0", &skill).unwrap();
        let names_in = |dir: &str| -> Vec<String> {
            let mut names: Vec<String> = files
                .iter()
                .filter(|(path, _)| path.parent() == Some(Path::new(dir)))
                .map(|(path, _)| path.file_stem().unwrap().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        assert_eq!(
            names_in("plugin/claude/agents"),
            ["astra", "forward", "hydrogen", "neon", "sol"]
        );
        assert_eq!(
            names_in("plugin/codex/agents"),
            ["fable", "forward", "hydrogen", "neon", "opus", "sonnet"]
        );
        assert_eq!(
            names_in("plugin/agents"),
            ["astra", "fable", "forward", "opus", "sol", "sonnet"]
        );

        let manifest = &files
            .iter()
            .find(|(p, _)| p.ends_with(".claude-plugin/plugin.json"))
            .unwrap()
            .1;
        let listed: serde_json::Value = serde_json::from_str(manifest).unwrap();
        assert_eq!(listed["agents"].as_array().unwrap().len(), 5);
    }
}

//! Renders the plugin from the built-in roster: each harness's agent
//! definitions, the plugin manifests and the skill's roster table. The output is
//! committed, because marketplaces install the plugin straight from git;
//! `cargo run --example generate` rewrites it and a test fails on drift.

use crate::harness::Harness;
use crate::roster::{Agent, FORWARD, Roster};
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

/// Forwarders only pass a task along, so each harness runs them on its cheapest
/// model.
const CLAUDE_FORWARDER_MODEL: &str = "haiku";
const CODEX_FORWARDER_MODEL: &str = "gpt-6-luna";
const CODEX_FORWARDER_EFFORT: &str = "low";
const AGY_FORWARDER_TIER: &str = "flash_lite";

/// Whether `host` runs `agent` itself, as a subagent pinned to the agent's
/// model. agy can't: its agent files take a model tier, never a model id.
pub fn is_native(host: Harness, agent: &Agent) -> bool {
    host == agent.harness && host != Harness::Agy && agent.env.is_empty() && agent.args.is_empty()
}

pub fn plugin_files(roster: &Roster, version: &str, skill_template: &str) -> Result<Vec<(PathBuf, String)>, String> {
    let agents: Vec<&Agent> = roster.entries().iter().map(|e| &e.agent).collect();
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
    for agent in &agents {
        files.push((
            plugin_path(&format!("claude/agents/{}.md", agent.name)),
            claude_agent(agent),
        ));
        files.push((
            plugin_path(&format!("codex/agents/{}.toml", agent.name)),
            codex_agent(agent),
        ));
        files.push((plugin_path(&format!("agents/{}.md", agent.name)), agy_agent(agent)));
    }
    files.push((plugin_path(&format!("claude/agents/{FORWARD}.md")), claude_forward()));
    files.push((plugin_path(&format!("codex/agents/{FORWARD}.toml")), codex_forward()));
    files.push((plugin_path(&format!("agents/{FORWARD}.md")), agy_forward()));
    Ok(files)
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
    let files: Vec<String> = agents
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
                "Use $brigade to spin up a sol subagent to review this change.",
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

fn native_description(agent: &Agent, effort_hint: &str) -> String {
    format!(
        "{}, running here as a native subagent. {} {effort_hint} (default {}).",
        agent.description,
        asks_for(&agent.name),
        agent.effort,
    )
}

fn forwarder_description(agent: &Agent) -> String {
    format!(
        "{}, run in its own CLI through brigade. {} Say in the prompt if the task should change files, \
         which effort to use (default {}), or which session to continue.",
        agent.description,
        asks_for(&agent.name),
        agent.effort,
    )
}

fn forward_description() -> String {
    "Hands a task to any brigade roster agent by name, including ones from the user's own roster \
     (~/.config/brigade/roster.toml) that have no subagent of their own. Start the prompt with \
     `agent: <name>`; `brigade list` shows the roster."
        .to_string()
}

const NATIVE_BODY: &str = "You are {name}, a brigade agent: {description}. Another agent handed you this task. \
Do it, then reply with a report it can act on: what you found or changed, with file paths, and anything you \
couldn't finish.\n";

fn native_body(agent: &Agent) -> String {
    NATIVE_BODY
        .replace("{name}", &agent.name)
        .replace("{description}", &agent.description)
}

/// The instructions every forwarder follows. `agent` is None for the generic
/// forwarder, which reads the agent's name from the request.
fn forwarder_body(agent: Option<&Agent>, shell_tool: &str) -> String {
    let (intro, name, levels) = match agent {
        Some(agent) => {
            let levels: Vec<&str> = agent.harness.efforts().iter().map(|e| e.as_str()).collect();
            (
                format!(
                    "You hand one task to the brigade agent `{}`, which runs in {}, and pass back its reply.",
                    agent.name,
                    agent.harness.product(),
                ),
                agent.name.clone(),
                levels.join(", "),
            )
        }
        None => (
            "You hand one task to a brigade roster agent and pass back its reply. The request names the \
             agent on a line like `agent: claude-local`; run `brigade list` if it isn't clear which roster \
             agent is meant."
                .to_string(),
            "<agent>".to_string(),
            "see `brigade list` for the agent's harness".to_string(),
        ),
    };
    format!(
        "{intro} You don't do any of the work yourself: don't read files, explore the repo or answer from \
your own knowledge.

1. Start the run with one {shell_tool} call, giving the task on stdin as you received it, minus the routing \
instructions the options below cover:

   ```
   brigade run {name} --detach [options] <<'BRIGADE_TASK'
   <the task>
   BRIGADE_TASK
   ```

   Add options only when the request asks for them:
   - `--write` when the task asks for changes to files. Without it the run is read-only
   - `--effort <level>` when it names a reasoning effort ({levels})
   - `--worktree` when it asks for its own worktree, as each member of a write team should
   - `--resume <session> --cwd <dir>` when it continues an earlier run; both come from that run's \
`[brigade] continue it with:` line

2. That prints a run id. Wait for it with `brigade wait <run id> --timeout 540`. While it reports that the run \
is still running, call it again.

3. Reply with the final `brigade wait` output exactly as printed, with no commentary before or after. If \
brigade fails, reply with its error output instead.
"
    )
}

/// JSON strings are valid YAML double-quoted scalars, which keeps colons and
/// quotes in descriptions from breaking the frontmatter.
fn yaml_str(value: &str) -> String {
    serde_json::to_string(value).expect("strings serialize")
}

fn claude_agent(agent: &Agent) -> String {
    if is_native(Harness::Claude, agent) {
        let description = native_description(agent, "Set the Agent call's effort to change its reasoning effort");
        format!(
            "---\nname: {}\ndescription: {}\nmodel: {}\neffort: {}\n---\n\n{}",
            agent.name,
            yaml_str(&description),
            agent.model,
            agent.effort,
            native_body(agent),
        )
    } else {
        claude_forwarder(
            &agent.name,
            &forwarder_description(agent),
            &forwarder_body(Some(agent), "Bash"),
        )
    }
}

fn claude_forward() -> String {
    claude_forwarder(FORWARD, &forward_description(), &forwarder_body(None, "Bash"))
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
    #[serde(skip_serializing_if = "Option::is_none")]
    model_reasoning_effort: Option<&'a str>,
    developer_instructions: &'a str,
}

fn codex_agent(agent: &Agent) -> String {
    if is_native(Harness::Codex, agent) {
        // No model_reasoning_effort: a role that sets one locks it, and the
        // spawn call's reasoning_effort should win.
        let description = native_description(agent, "Pass reasoning_effort on spawn_agent to change its effort");
        codex_role(&CodexRole {
            name: &agent.name,
            description: &description,
            model: &agent.model,
            model_reasoning_effort: None,
            developer_instructions: &native_body(agent),
        })
    } else {
        codex_role(&CodexRole {
            name: &agent.name,
            description: &forwarder_description(agent),
            model: CODEX_FORWARDER_MODEL,
            model_reasoning_effort: Some(CODEX_FORWARDER_EFFORT),
            developer_instructions: &forwarder_body(Some(agent), "shell"),
        })
    }
}

fn codex_forward() -> String {
    codex_role(&CodexRole {
        name: FORWARD,
        description: &forward_description(),
        model: CODEX_FORWARDER_MODEL,
        model_reasoning_effort: Some(CODEX_FORWARDER_EFFORT),
        developer_instructions: &forwarder_body(None, "shell"),
    })
}

fn codex_role(role: &CodexRole) -> String {
    toml::to_string(role).expect("roles serialize")
}

fn agy_agent(agent: &Agent) -> String {
    agy_forwarder(
        &agent.name,
        &forwarder_description(agent),
        &forwarder_body(Some(agent), "run_command"),
    )
}

fn agy_forward() -> String {
    agy_forwarder(FORWARD, &forward_description(), &forwarder_body(None, "run_command"))
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
    let mut table = String::from("| Agent | Runs in | Model | Default effort | What it is |\n|---|---|---|---|---|\n");
    for agent in agents {
        table.push_str(&format!(
            "| `{}` | {} | `{}` | {} | {} |\n",
            agent.name,
            agent.harness.product(),
            agent.model,
            agent.effort,
            agent.description,
        ));
    }
    Ok(format!(
        "{}{ROSTER_START}\n{table}{}",
        &template[..start],
        &template[end..]
    ))
}

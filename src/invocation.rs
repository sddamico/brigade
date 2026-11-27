//! How a run becomes a command line for its harness's CLI, and how that CLI's
//! output becomes a reply.

use crate::harness::{Access, Effort, Harness};
use crate::roster::Agent;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Prepended to read-only tasks. Claude Code and agy keep their shell on a
/// read-only run, so the agent is told as well as restricted.
pub const READ_ONLY_NOTE: &str =
    "This is a read-only run: investigate and report back, but don't create, edit or delete files.\n\n";

const CLAUDE_EDIT_TOOLS: &str = "Edit,Write,NotebookEdit";
const AGY_READ_ONLY_SETTINGS: &str = r#"{"permissions":{"deny":["write_file(*)","edit_file(*)"]}}"#;

pub struct Request<'a> {
    pub agent: &'a Agent,
    pub effort: Effort,
    pub access: Access,
    pub cwd: &'a Path,
    pub resume: Option<&'a str>,
    pub task: &'a str,
    /// Where Codex writes its final message (`codex exec -o`).
    pub last_message_file: &'a Path,
}

#[derive(Debug, PartialEq)]
pub struct Invocation {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    pub stdin: String,
}

#[derive(Debug, PartialEq)]
pub struct UnsetVar(pub String);

#[derive(Debug, PartialEq)]
pub struct Reply {
    pub session: Option<String>,
    pub message: String,
}

pub fn build(req: &Request, var: &dyn Fn(&str) -> Option<String>) -> Result<Invocation, UnsetVar> {
    let agent = req.agent;
    let model = expand(&agent.model, var)?;
    let extra = agent
        .args
        .iter()
        .map(|a| expand(a, var))
        .collect::<Result<Vec<_>, _>>()?;
    let env = agent
        .env
        .iter()
        .map(|(k, v)| Ok((k.clone(), expand(v, var)?)))
        .collect::<Result<Vec<_>, UnsetVar>>()?;
    let prompt = match req.access {
        Access::ReadOnly => format!("{READ_ONLY_NOTE}{}", req.task),
        Access::Write => req.task.to_string(),
    };
    let effort = req.effort.as_str();
    let read_only = req.access == Access::ReadOnly;

    let mut args = Vec::new();
    let mut stdin = String::new();
    match agent.harness {
        Harness::Claude => {
            args.extend(strings(&[
                "-p",
                "--model",
                &model,
                "--effort",
                effort,
                "--output-format",
                "json",
                "--dangerously-skip-permissions",
            ]));
            if read_only {
                args.extend(strings(&["--disallowedTools", CLAUDE_EDIT_TOOLS]));
            }
            if let Some(session) = req.resume {
                args.extend(strings(&["--resume", session]));
            }
            args.extend(extra);
            stdin = prompt;
        }
        Harness::Codex => {
            let effort_config = format!("model_reasoning_effort={effort}");
            let cwd = req.cwd.to_string_lossy();
            let last_message = req.last_message_file.to_string_lossy();
            args.extend(strings(&[
                "exec",
                "-m",
                &model,
                "-c",
                &effort_config,
                "-C",
                &cwd,
                "--skip-git-repo-check",
                "--json",
                "-o",
                &last_message,
            ]));
            if read_only {
                args.extend(strings(&["-s", "read-only"]));
            } else {
                args.push("--dangerously-bypass-approvals-and-sandbox".into());
            }
            args.extend(extra);
            // exec's own flags only apply when they come before `resume`.
            if let Some(session) = req.resume {
                args.extend(strings(&["resume", session]));
            }
            args.push("-".into());
            stdin = prompt;
        }
        Harness::Agy => {
            // agy has no stdin prompt, and a bare `-p <prompt>` is rejected when
            // the prompt starts with a dash.
            args.push(format!("--print={prompt}"));
            args.extend(strings(&[
                "--model",
                &model,
                "--effort",
                effort,
                "--output-format",
                "json",
                "--dangerously-skip-permissions",
            ]));
            if read_only {
                args.extend(strings(&["--settings", AGY_READ_ONLY_SETTINGS]));
            }
            if let Some(session) = req.resume {
                args.extend(strings(&["--conversation", session]));
            }
            args.extend(extra);
        }
    }

    Ok(Invocation {
        program: agent.harness.program().to_string(),
        args,
        env,
        cwd: req.cwd.to_path_buf(),
        stdin,
    })
}

/// Reads the harness's final output. `last_message` is the file Codex wrote its
/// final message to, when it wrote one.
pub fn parse(harness: Harness, stdout: &str, last_message: Option<&str>) -> Result<Reply, String> {
    match harness {
        Harness::Claude => {
            let v: Value = serde_json::from_str(stdout.trim())
                .map_err(|_| "Claude Code didn't print a JSON result".to_string())?;
            let session = v["session_id"].as_str().map(String::from);
            let result = v["result"].as_str().unwrap_or_default().to_string();
            let subtype = v["subtype"].as_str().unwrap_or("no result");
            if v["is_error"].as_bool() == Some(true) || subtype != "success" {
                return Err(if result.is_empty() {
                    format!("Claude Code ended with {subtype}")
                } else {
                    result
                });
            }
            Ok(Reply {
                session,
                message: result,
            })
        }
        Harness::Codex => {
            let mut session = None;
            let mut completed = false;
            let mut failure = None;
            let mut last_agent_message = None;
            for v in stdout
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            {
                match v["type"].as_str() {
                    Some("thread.started") => session = v["thread_id"].as_str().map(String::from),
                    Some("turn.completed") => completed = true,
                    Some("turn.failed") => {
                        failure = Some(v["error"]["message"].as_str().unwrap_or("the turn failed").to_string())
                    }
                    Some("error") => failure = v["message"].as_str().map(String::from),
                    // Item-level errors (a plugin's hook config failing to parse,
                    // say) are warnings; the turn carries on.
                    Some("item.completed") if v["item"]["type"] == "agent_message" => {
                        last_agent_message = v["item"]["text"].as_str().map(String::from)
                    }
                    _ => {}
                }
            }
            if !completed {
                return Err(failure.unwrap_or_else(|| "Codex stopped before finishing the turn".to_string()));
            }
            let message = last_message
                .filter(|m| !m.trim().is_empty())
                .map(String::from)
                .or(last_agent_message)
                .unwrap_or_default();
            Ok(Reply {
                session,
                message: message.trim_end().to_string(),
            })
        }
        Harness::Agy => {
            let v: Value = serde_json::from_str(stdout.trim())
                .map_err(|_| "Antigravity didn't print a JSON result".to_string())?;
            let session = v["conversation_id"].as_str().map(String::from);
            let status = v["status"].as_str().unwrap_or("no status");
            if status != "SUCCESS" {
                return Err(v["error"]
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| format!("Antigravity ended with {status}")));
            }
            let message = v["response"].as_str().unwrap_or_default().trim_end().to_string();
            Ok(Reply { session, message })
        }
    }
}

/// Expands `$NAME` and `${NAME}` from `var`. A lone `$` is kept as is.
pub fn expand(value: &str, var: &dyn Fn(&str) -> Option<String>) -> Result<String, UnsetVar> {
    let mut out = String::new();
    let mut rest = value;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let (name, tail) = match after.strip_prefix('{') {
            Some(braced) => match braced.find('}') {
                Some(end) => (&braced[..end], &braced[end + 1..]),
                None => ("", after),
            },
            None => {
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                (&after[..end], &after[end..])
            }
        };
        if name.is_empty() {
            out.push('$');
            rest = after;
            continue;
        }
        out.push_str(&var(name).ok_or_else(|| UnsetVar(name.to_string()))?);
        rest = tail;
    }
    out.push_str(rest);
    Ok(out)
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn agent(harness: Harness, model: &str) -> Agent {
        Agent {
            name: "a".into(),
            harness,
            model: model.into(),
            effort: Effort::Medium,
            description: String::new(),
            env: BTreeMap::new(),
            args: Vec::new(),
        }
    }

    fn invocation(agent: &Agent, access: Access, resume: Option<&str>) -> Invocation {
        let req = Request {
            agent,
            effort: Effort::High,
            access,
            cwd: Path::new("/repo"),
            resume,
            task: "do it",
            last_message_file: Path::new("/runs/r/last-message.txt"),
        };
        build(&req, &|_| None).unwrap()
    }

    fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
        args.windows(2).any(|w| w[0] == flag && w[1] == value)
    }

    #[test]
    fn codex_read_only_runs_in_the_read_only_sandbox() {
        let inv = invocation(&agent(Harness::Codex, "gpt-6.2-sol"), Access::ReadOnly, None);
        assert!(has_pair(&inv.args, "-s", "read-only"));
        assert!(!inv.args.iter().any(|a| a.contains("bypass")));
        assert!(has_pair(&inv.args, "-c", "model_reasoning_effort=high"));
        assert!(inv.stdin.starts_with(READ_ONLY_NOTE));
    }

    #[test]
    fn codex_write_bypasses_the_sandbox_and_sends_the_task_unchanged() {
        let inv = invocation(&agent(Harness::Codex, "gpt-6.2-sol"), Access::Write, None);
        assert!(
            inv.args
                .contains(&"--dangerously-bypass-approvals-and-sandbox".to_string())
        );
        assert!(!inv.args.contains(&"-s".to_string()));
        assert_eq!(inv.stdin, "do it");
    }

    #[test]
    fn codex_resume_comes_after_exec_flags_and_before_the_stdin_prompt() {
        let inv = invocation(&agent(Harness::Codex, "gpt-6.2-sol"), Access::Write, Some("thread-1"));
        let n = inv.args.len();
        assert_eq!(&inv.args[n - 3..], ["resume", "thread-1", "-"]);
        let resume_at = n - 3;
        for flag in ["-m", "-C", "-o", "--json", "--dangerously-bypass-approvals-and-sandbox"] {
            let at = inv.args.iter().position(|a| a == flag).unwrap();
            assert!(at < resume_at, "{flag} must precede resume");
        }
    }

    #[test]
    fn claude_read_only_removes_the_edit_tools_alongside_agent_args() {
        let mut a = agent(Harness::Claude, "opus");
        a.args = vec!["--disallowedTools".into(), "Artifact".into()];
        let inv = invocation(&a, Access::ReadOnly, Some("s-1"));
        assert!(has_pair(&inv.args, "--disallowedTools", CLAUDE_EDIT_TOOLS));
        assert!(has_pair(&inv.args, "--disallowedTools", "Artifact"));
        assert!(has_pair(&inv.args, "--resume", "s-1"));
        assert!(inv.stdin.ends_with("do it"));
    }

    #[test]
    fn claude_write_keeps_the_edit_tools() {
        let inv = invocation(&agent(Harness::Claude, "opus"), Access::Write, None);
        assert!(!inv.args.contains(&"--disallowedTools".to_string()));
    }

    #[test]
    fn agy_takes_the_prompt_in_the_print_flag_and_denies_writes_when_read_only() {
        let inv = invocation(&agent(Harness::Agy, "gemini-4-neon"), Access::ReadOnly, Some("c-1"));
        assert_eq!(inv.args[0], format!("--print={READ_ONLY_NOTE}do it"));
        assert!(has_pair(&inv.args, "--settings", AGY_READ_ONLY_SETTINGS));
        assert!(has_pair(&inv.args, "--conversation", "c-1"));
        assert!(has_pair(&inv.args, "--effort", "high"));
        assert!(inv.stdin.is_empty());

        let write = invocation(&agent(Harness::Agy, "gemini-4-neon"), Access::Write, None);
        assert!(!write.args.contains(&"--settings".to_string()));
    }

    #[test]
    fn agent_model_env_and_args_expand_variables() {
        let mut a = agent(Harness::Claude, "$LOCAL_MODEL");
        a.env
            .insert("ANTHROPIC_BASE_URL".into(), "${LOCAL_MODEL_SERVER}/v1".into());
        let req = Request {
            agent: &a,
            effort: Effort::Medium,
            access: Access::Write,
            cwd: Path::new("/repo"),
            resume: None,
            task: "t",
            last_message_file: Path::new("/x"),
        };
        let vars = |name: &str| match name {
            "LOCAL_MODEL" => Some("local".to_string()),
            "LOCAL_MODEL_SERVER" => Some("http://box:8787".to_string()),
            _ => None,
        };
        let inv = build(&req, &vars).unwrap();
        assert!(has_pair(&inv.args, "--model", "local"));
        assert_eq!(
            inv.env,
            vec![("ANTHROPIC_BASE_URL".to_string(), "http://box:8787/v1".to_string())]
        );

        assert_eq!(build(&req, &|_| None), Err(UnsetVar("LOCAL_MODEL".into())));
    }

    #[test]
    fn expand_keeps_a_lone_dollar() {
        assert_eq!(expand("cost $ 5 ${", &|_| None).unwrap(), "cost $ 5 ${");
    }

    #[test]
    fn parses_claude_success_and_error() {
        let ok = r#"{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"s-1"}"#;
        assert_eq!(
            parse(Harness::Claude, ok, None),
            Ok(Reply {
                session: Some("s-1".into()),
                message: "done".into()
            })
        );
        let err = r#"{"type":"result","subtype":"error_max_turns","is_error":true,"session_id":"s-1"}"#;
        assert_eq!(
            parse(Harness::Claude, err, None),
            Err("Claude Code ended with error_max_turns".into())
        );
    }

    #[test]
    fn parses_codex_and_ignores_item_errors() {
        let out = [
            r#"{"type":"thread.started","thread_id":"t-1"}"#,
            r#"{"type":"item.completed","item":{"id":"item_0","type":"error","message":"failed to parse plugin hooks config"}}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"from stream"}}"#,
            r#"{"type":"turn.completed","usage":{}}"#,
        ]
        .join("\n");
        assert_eq!(
            parse(Harness::Codex, &out, Some("from file\n")),
            Ok(Reply {
                session: Some("t-1".into()),
                message: "from file".into()
            })
        );
        assert_eq!(parse(Harness::Codex, &out, None).unwrap().message, "from stream");
    }

    #[test]
    fn codex_without_a_completed_turn_is_a_failure() {
        let out = [
            r#"{"type":"thread.started","thread_id":"t-1"}"#,
            r#"{"type":"turn.failed","error":{"message":"model not found"}}"#,
        ]
        .join("\n");
        assert_eq!(parse(Harness::Codex, &out, None), Err("model not found".into()));
    }

    #[test]
    fn parses_agy_success_and_error() {
        let ok = r#"{"conversation_id":"c-1","status":"SUCCESS","response":"pong\n","num_turns":1}"#;
        assert_eq!(
            parse(Harness::Agy, ok, None),
            Ok(Reply {
                session: Some("c-1".into()),
                message: "pong".into()
            })
        );
        let err = r#"{"conversation_id":"c-1","status":"ERROR","error":"quota exhausted","finished_reason":"error"}"#;
        assert_eq!(parse(Harness::Agy, err, None), Err("quota exhausted".into()));
    }
}

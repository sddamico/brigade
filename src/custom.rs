//! Harnesses the user declares in their roster: how to start the CLI and read
//! its reply, as templates instead of code, so a CLI brigade doesn't know can
//! join without a release.

use crate::harness::{Access, Effort};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomHarness {
    pub name: String,
    /// The name replies use for it ("hit a usage limit in Muse Code"); the
    /// harness's name when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    pub program: String,
    /// Passed on every run. `{model}` is required. `{prompt}` puts the task in an
    /// argument; without it the task goes on stdin.
    pub args: Vec<String>,
    /// The levels of brigade's scale the CLI takes; none means no `--effort`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<Effort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effort_args: Vec<String>,
    /// Without these, a read-only run is read-only only because its task says so.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_only_args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resume_args: Vec<String>,
    pub output: OutputSpec,
    /// How the harness hosts brigade agents, if it can.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostSpec>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSpec {
    pub format: OutputFormat,
    /// JSON pointers (`/result`) into the output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Text that, in the error or on stderr, means the account hit a usage limit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limit_signs: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// One JSON object on stdout.
    Json,
    /// A JSON event per line: the session from the first event that has one, the
    /// reply and error from the last.
    Jsonl,
    /// Plain text: stdout is the reply, and a failing exit status is a failure.
    Text,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSpec {
    /// Where the harness finds the user's own subagents. `~/` and `$VAR` expand.
    pub agents_dir: String,
    /// The forwarder's file name, from `{name}`.
    pub file: String,
    /// The forwarder file, from `{name}`, `{description}` (quoted, so it fits
    /// YAML and TOML), `{instructions}` and `{marker}` (a line brigade knows its
    /// files by; put it in a comment).
    pub template: String,
    /// The harness's shell tool, which the forwarder's instructions name.
    #[serde(default = "default_shell_tool")]
    pub shell_tool: String,
}

fn default_shell_tool() -> String {
    "shell".to_string()
}

pub struct Args<'a> {
    pub model: &'a str,
    pub effort: Option<Effort>,
    pub access: Access,
    pub resume: Option<&'a str>,
    pub cwd: &'a str,
}

impl CustomHarness {
    pub fn check(&self) -> Result<(), String> {
        if self.program.trim().is_empty() {
            return Err("needs a program".into());
        }
        let fields: [(&str, &[String], &[&str]); 5] = [
            ("args", &self.args, &["model", "prompt", "cwd"]),
            ("effort_args", &self.effort_args, &["effort"]),
            ("read_only_args", &self.read_only_args, &[]),
            ("write_args", &self.write_args, &[]),
            ("resume_args", &self.resume_args, &["session"]),
        ];
        for (field, values, allowed) in fields {
            for value in values {
                if let Some(unknown) = placeholders(value).into_iter().find(|p| !allowed.contains(p)) {
                    return Err(format!(
                        "has `{{{unknown}}}` in {field}, which takes {}",
                        listed(allowed)
                    ));
                }
            }
        }
        let uses = |values: &[String], name: &str| values.iter().any(|v| placeholders(v).contains(&name));
        if !uses(&self.args, "model") {
            return Err("needs `{model}` in args".into());
        }
        if !self.efforts.is_empty() && !uses(&self.effort_args, "effort") {
            return Err("lists efforts, so it needs `{effort}` in effort_args".into());
        }
        if !self.resume_args.is_empty() && !uses(&self.resume_args, "session") {
            return Err("needs `{session}` in resume_args".into());
        }
        let output = &self.output;
        if output.format != OutputFormat::Text && output.reply.is_none() {
            return Err("needs output.reply, a JSON pointer to the reply".into());
        }
        for pointer in [&output.reply, &output.session, &output.error].into_iter().flatten() {
            if !pointer.starts_with('/') {
                return Err(format!("has output pointer `{pointer}`, which should start with `/`"));
            }
        }
        if let Some(host) = &self.host {
            if !placeholders(&host.file).contains(&"name") {
                return Err("needs `{name}` in host.file".into());
            }
            let used = placeholders(&host.template);
            for needed in ["marker", "instructions"] {
                if !used.contains(&needed) {
                    return Err(format!("needs `{{{needed}}}` in host.template"));
                }
            }
            let allowed = ["name", "description", "instructions", "marker"];
            if let Some(unknown) = used.into_iter().find(|p| !allowed.contains(p)) {
                return Err(format!(
                    "has `{{{unknown}}}` in host.template, which takes {}",
                    listed(&allowed)
                ));
            }
        }
        Ok(())
    }

    /// The command line for a run, and whether the prompt went into it rather
    /// than on stdin.
    pub fn args(&self, args: &Args, prompt: &str) -> (Vec<String>, bool) {
        let effort = args.effort.map(|e| e.as_str()).unwrap_or_default();
        let values = [
            ("model", args.model),
            ("prompt", prompt),
            ("cwd", args.cwd),
            ("effort", effort),
            ("session", args.resume.unwrap_or_default()),
        ];
        let mut out: Vec<String> = self.args.iter().map(|a| fill(a, &values)).collect();
        if args.effort.is_some() {
            out.extend(self.effort_args.iter().map(|a| fill(a, &values)));
        }
        out.extend(
            match args.access {
                Access::ReadOnly => &self.read_only_args,
                Access::Write => &self.write_args,
            }
            .iter()
            .cloned(),
        );
        if args.resume.is_some() {
            out.extend(self.resume_args.iter().map(|a| fill(a, &values)));
        }
        let in_args = self.args.iter().any(|a| placeholders(a).contains(&"prompt"));
        (out, in_args)
    }
}

/// What a custom harness's run left, read by its output spec.
pub enum Parsed {
    Reply {
        session: Option<String>,
        message: String,
    },
    Limited(String),
    Failed(String),
    /// Nothing usable; the caller adds the exit status and stderr.
    NoResult(String),
}

pub fn parse(harness: &CustomHarness, stdout: &str, stderr: &str, succeeded: bool) -> Parsed {
    let spec = &harness.output;
    let limited = |error: &str| spec.limit_signs.iter().any(|s| error.contains(s) || stderr.contains(s));
    let events: Vec<Value> = match spec.format {
        OutputFormat::Text => {
            if !succeeded {
                return match spec.limit_signs.iter().find(|s| stderr.contains(s.as_str())) {
                    Some(_) => Parsed::Limited(last_line(stderr)),
                    None => Parsed::NoResult(format!("`{}` failed", harness.program)),
                };
            }
            return Parsed::Reply {
                session: None,
                message: stdout.trim_end().to_string(),
            };
        }
        OutputFormat::Json => match serde_json::from_str(stdout.trim()) {
            Ok(value) => vec![value],
            Err(_) => return Parsed::NoResult(format!("`{}` didn't print a JSON result", harness.program)),
        },
        OutputFormat::Jsonl => stdout.lines().filter_map(|l| serde_json::from_str(l).ok()).collect(),
    };
    let find = |pointer: &Option<String>, last: bool| -> Option<String> {
        let pointer = pointer.as_deref()?;
        let found = |v: &Value| v.pointer(pointer).and_then(text);
        if last {
            events.iter().rev().find_map(found)
        } else {
            events.iter().find_map(found)
        }
    };
    if let Some(error) = find(&spec.error, true) {
        return if limited(&error) {
            Parsed::Limited(error)
        } else {
            Parsed::Failed(error)
        };
    }
    match find(&spec.reply, true) {
        Some(message) => Parsed::Reply {
            session: find(&spec.session, false),
            message: message.trim_end().to_string(),
        },
        None => Parsed::NoResult(format!(
            "`{}` printed no reply at {}",
            harness.program,
            spec.reply.as_deref().unwrap_or_default()
        )),
    }
}

/// A JSON value as text; null, false and empty strings say nothing.
fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Object(_) | Value::Array(_) => Some(value.to_string()),
        _ => None,
    }
}

fn last_line(text: &str) -> String {
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .to_string()
}

/// The `{name}` placeholders in `template`. Braces around anything else, like
/// JSON in an argument, are left alone.
pub fn placeholders(template: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        rest = &rest[start + 1..];
        if let Some(end) = rest.find('}') {
            let name = &rest[..end];
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                found.push(name);
            }
        }
    }
    found
}

pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (name, value) in values {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

fn listed(names: &[&str]) -> String {
    if names.is_empty() {
        return "no placeholders".into();
    }
    names
        .iter()
        .map(|n| format!("`{{{n}}}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn muse(format: OutputFormat) -> CustomHarness {
        CustomHarness {
            name: "muse".into(),
            product: Some("Muse Code".into()),
            program: "muse-code".into(),
            args: vec!["exec".into(), "--model".into(), "{model}".into()],
            efforts: vec![Effort::Low, Effort::High],
            effort_args: vec!["--effort={effort}".into()],
            read_only_args: vec!["--read-only".into()],
            write_args: vec!["--yolo".into()],
            resume_args: vec!["--resume".into(), "{session}".into()],
            output: OutputSpec {
                format,
                reply: Some("/result".into()),
                session: Some("/session_id".into()),
                error: Some("/error".into()),
                limit_signs: vec!["usage limit".into()],
            },
            host: None,
        }
    }

    #[test]
    fn builds_the_command_line_from_the_templates() {
        let harness = muse(OutputFormat::Json);
        let args = Args {
            model: "muse-2",
            effort: Some(Effort::High),
            access: Access::ReadOnly,
            resume: Some("s-1"),
            cwd: "/w",
        };
        let (line, in_args) = harness.args(&args, "do it");
        assert_eq!(
            line,
            [
                "exec",
                "--model",
                "muse-2",
                "--effort=high",
                "--read-only",
                "--resume",
                "s-1"
            ]
        );
        assert!(!in_args);

        let mut by_arg = muse(OutputFormat::Json);
        by_arg.args.push("--prompt={prompt}".into());
        let fresh = Args {
            effort: None,
            access: Access::Write,
            resume: None,
            ..args
        };
        let (line, in_args) = by_arg.args(&fresh, "do it");
        assert_eq!(line, ["exec", "--model", "muse-2", "--prompt=do it", "--yolo"]);
        assert!(in_args);
    }

    #[test]
    fn check_catches_templates_that_cant_work() {
        assert_eq!(muse(OutputFormat::Json).check(), Ok(()));
        type Breaks = fn(&mut CustomHarness);
        let broken: [(Breaks, &str); 5] = [
            (|h| h.args = vec!["exec".into()], "needs `{model}` in args"),
            (|h| h.args.push("{modle}".into()), "has `{modle}` in args"),
            (
                |h| h.effort_args = vec!["--effort".into()],
                "needs `{effort}` in effort_args",
            ),
            (|h| h.output.reply = None, "needs output.reply"),
            (
                |h| h.output.session = Some("session_id".into()),
                "should start with `/`",
            ),
        ];
        for (breaks, problem) in broken {
            let mut harness = muse(OutputFormat::Json);
            breaks(&mut harness);
            let error = harness.check().unwrap_err();
            assert!(error.contains(problem), "{error}");
        }
        let mut json_arg = muse(OutputFormat::Json);
        json_arg.args.push(r#"--settings={"deny":["edit"]}"#.into());
        assert_eq!(json_arg.check(), Ok(()), "JSON in an argument isn't a placeholder");
    }

    #[test]
    fn reads_replies_errors_and_limits() {
        let json = muse(OutputFormat::Json);
        match parse(&json, r#"{"result":"done\n","session_id":"s-1"}"#, "", true) {
            Parsed::Reply { session, message } => {
                assert_eq!((session.as_deref(), message.as_str()), (Some("s-1"), "done"))
            }
            _ => panic!("expected a reply"),
        }
        assert!(matches!(
            parse(&json, r#"{"error":"you hit your usage limit"}"#, "", false),
            Parsed::Limited(e) if e == "you hit your usage limit"
        ));
        assert!(matches!(parse(&json, r#"{"error":"bad model"}"#, "", false), Parsed::Failed(e) if e == "bad model"));
        assert!(matches!(parse(&json, "not json", "", false), Parsed::NoResult(_)));

        let jsonl = muse(OutputFormat::Jsonl);
        let stream = "{\"session_id\":\"s-1\"}\n{\"result\":\"partial\"}\nnoise\n{\"result\":\"final\"}\n";
        match parse(&jsonl, stream, "", true) {
            Parsed::Reply { session, message } => {
                assert_eq!((session.as_deref(), message.as_str()), (Some("s-1"), "final"))
            }
            _ => panic!("expected a reply"),
        }

        let text = muse(OutputFormat::Text);
        assert!(matches!(parse(&text, "hello\n", "", true), Parsed::Reply { message, .. } if message == "hello"));
        assert!(matches!(
            parse(&text, "", "warming up\nyou hit your usage limit\n", false),
            Parsed::Limited(e) if e == "you hit your usage limit"
        ));
        assert!(matches!(parse(&text, "half", "boom", false), Parsed::NoResult(_)));
    }
}

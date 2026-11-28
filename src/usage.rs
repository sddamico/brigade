//! Reads an account's quotas from its harness without spending a run:
//! Claude Code's `/usage`, agy's `/usage` and the Codex app server's rate
//! limits. They count every session on the account, not just brigade's.

use crate::accounts::{self, Quota};
use crate::harness::Harness;
use crate::invocation::expand;
use crate::roster::AccountDef;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// The account's quotas, as its harness reports them now.
pub fn read(account: &AccountDef, var: &dyn Fn(&str) -> Option<String>, now: u64) -> Result<Vec<Quota>, String> {
    let env = account
        .env
        .iter()
        .map(|(k, v)| expand(v, var).map(|v| (k.clone(), v)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("needs ${}, which isn't set", e.0))?;
    let args = account
        .args
        .iter()
        .map(|a| expand(a, var))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("needs ${}, which isn't set", e.0))?;
    match &account.harness {
        Some(Harness::Claude) => {
            let out = output(Command::new("claude").args(["-p", "/usage"]).args(&args).envs(env))?;
            Ok(claude_quotas(&out, now))
        }
        Some(Harness::Agy) => {
            let out = output(
                Command::new("agy")
                    .args(["--print=/usage", "--output-format", "json"])
                    .args(&args)
                    .envs(env),
            )?;
            agy_quotas(&out)
        }
        Some(Harness::Codex) => codex_quotas(&codex_rate_limits(&args, env)?),
        _ => Err("has no harness brigade can read usage from".into()),
    }
}

fn output(command: &mut Command) -> Result<String, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("can't run `{program}`: {e}"))?;
    let deadline = Instant::now() + READ_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                return Err(format!(
                    "`{program}` didn't report usage within {}s",
                    READ_TIMEOUT.as_secs()
                ));
            }
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("can't read `{program}`: {e}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Claude Code's `/usage` text: "Current week (all models): 37% used · resets
/// Dec 2, 2am (America/New_York)". A window named after a model covers that
/// model alone.
fn claude_quotas(text: &str, now: u64) -> Vec<Quota> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (name, rest) = line.split_once(": ")?;
            if !name.starts_with("Current ") {
                return None;
            }
            let (used, resets) = rest.split_once("% used")?;
            let used: f64 = used.trim().parse().ok()?;
            let resets_at = resets
                .split_once("resets ")
                .and_then(|(_, when)| accounts::claude_reset(when, now));
            let models = name
                .split_once('(')
                .map(|(_, model)| model.trim_end_matches(')').to_lowercase())
                .filter(|model| model != "all models")
                .map(|model| vec![model]);
            Some(Quota {
                name: name.to_string(),
                used: used / 100.0,
                resets_at,
                models,
            })
        })
        .collect()
}

/// agy's `/usage` JSON: model groups, each with weekly and five-hour buckets.
fn agy_quotas(text: &str) -> Result<Vec<Quota>, String> {
    let v: Value = serde_json::from_str(text.trim()).map_err(|_| "agy didn't print its usage as JSON".to_string())?;
    let groups = v["command"]["data"]["groups"]
        .as_array()
        .ok_or("agy's usage has no model groups")?;
    let mut quotas = Vec::new();
    for group in groups {
        let name = group["name"].as_str().unwrap_or("models");
        let models: Vec<String> = group["description"]
            .as_str()
            .and_then(|d| d.split_once(": "))
            .map(|(_, list)| {
                list.split(", ")
                    .filter(|m| !m.contains('('))
                    .map(|m| m.trim().to_lowercase().replace(' ', "-"))
                    .collect()
            })
            .unwrap_or_default();
        for bucket in group["buckets"].as_array().into_iter().flatten() {
            if bucket["disabled"].as_bool() == Some(true) {
                continue;
            }
            let Some(remaining) = bucket["remaining_fraction"].as_f64() else {
                continue;
            };
            quotas.push(Quota {
                name: format!("{name}, {}", bucket["window"].as_str().unwrap_or("window")),
                used: 1.0 - remaining,
                resets_at: bucket["reset_time"].as_str().and_then(accounts::utc_epoch),
                models: (!models.is_empty()).then(|| models.clone()),
            });
        }
    }
    Ok(quotas)
}

/// Asks the Codex app server for the account's rate limits.
fn codex_rate_limits(args: &[String], env: Vec<(String, String)>) -> Result<Value, String> {
    let mut child = Command::new("codex")
        .arg("app-server")
        .args(args)
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("can't run `codex app-server`: {e}"))?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"clientInfo": {"name": "brigade", "version": env!("CARGO_PKG_VERSION")}}}),
        json!({"jsonrpc": "2.0", "method": "initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "account/rateLimits/read"}),
    ];
    for request in requests {
        let _ = writeln!(stdin, "{request}");
    }
    let stdout = child.stdout.take().expect("stdout is piped");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line)
                && v["id"] == 2
            {
                let _ = tx.send(v);
                return;
            }
        }
    });
    let reply = rx.recv_timeout(READ_TIMEOUT);
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let reply = reply.map_err(|_| "the Codex app server didn't report rate limits".to_string())?;
    match reply.get("result") {
        Some(result) => Ok(result.clone()),
        None => Err(format!("the Codex app server said: {}", reply["error"])),
    }
}

/// The app server's limits: a five-hour and a weekly window per limit, for
/// every model unless the limit names one.
fn codex_quotas(result: &Value) -> Result<Vec<Quota>, String> {
    let limits = result["rateLimitsByLimitId"]
        .as_object()
        .ok_or("the Codex app server's reply has no rate limits")?;
    let mut quotas = Vec::new();
    for (id, limit) in limits {
        let models = limit["normalModelSlug"].as_str().map(|slug| vec![slug.to_string()]);
        for key in ["primary", "secondary"] {
            let window = &limit[key];
            let Some(used) = window["usedPercent"].as_f64() else {
                continue;
            };
            let length = match window["windowDurationMins"].as_u64() {
                Some(300) => "5-hour".to_string(),
                Some(10080) => "weekly".to_string(),
                Some(mins) => format!("{mins}-minute"),
                None => key.to_string(),
            };
            quotas.push(Quota {
                name: format!("{id}, {length}"),
                used: used / 100.0,
                resets_at: window["resetsAt"].as_u64(),
                models: models.clone(),
            });
        }
    }
    Ok(quotas)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_claudes_usage_text() {
        let text = "You are currently using your subscription to power your Claude Code usage\n\n\
                    Current week (all models): 37% used · resets Dec 2, 2am (America/New_York)\n\
                    Current week (Fable): 23% used · resets Dec 2, 2am (America/New_York)\n\n\
                    Last 24h · 2319 requests · 22 sessions\n  96%  Check which models your subagents use\n";
        let now = accounts::local_epoch_for_tests(2026, 10, 27, 22, 0);
        let quotas = claude_quotas(text, now);
        assert_eq!(quotas.len(), 2);
        assert_eq!(quotas[0].name, "Current week (all models)");
        assert_eq!(quotas[0].models, None);
        assert!((quotas[0].used - 0.37).abs() < 1e-9);
        assert_eq!(
            accounts::local_time(quotas[0].resets_at.unwrap())[..16],
            *"2026-12-02 02:00"
        );
        assert_eq!(quotas[1].models, Some(vec!["fable".to_string()]));
    }

    #[test]
    fn reads_agys_usage_groups() {
        let text = r#"{"status":"SUCCESS","command":{"name":"usage","data":{"groups":[
            {"name":"Gemini Models","description":"Models within this group: Gemini 4 Neon, Gemini 3.8 Flash, Orchestrator (Neon)",
             "buckets":[{"window":"weekly","remaining_fraction":0,"reset_time":"2026-12-03T12:42:51Z"},
                        {"window":"5h","disabled":true,"remaining_fraction":0.64}]},
            {"name":"Claude and GPT models","description":"Models within this group: Claude Opus 5.5",
             "buckets":[{"window":"5h","remaining_fraction":0.93,"reset_time":"2026-11-28T08:41:54Z"}]}]}}}"#;
        let quotas = agy_quotas(text).unwrap();
        assert_eq!(quotas.len(), 2, "the disabled bucket is left out");
        assert_eq!(quotas[0].name, "Gemini Models, weekly");
        assert_eq!(quotas[0].used, 1.0);
        assert_eq!(quotas[0].resets_at, Some(1_796_301_771));
        assert_eq!(
            quotas[0].models,
            Some(vec!["gemini-4-neon".to_string(), "gemini-3.8-flash".to_string()])
        );
        assert_eq!(quotas[1].models, Some(vec!["claude-opus-5.5".to_string()]));
    }

    #[test]
    fn reads_codexs_rate_limits() {
        let result = json!({"rateLimitsByLimitId": {"codex": {
            "normalModelSlug": null,
            "primary": {"usedPercent": 100, "windowDurationMins": 300, "resetsAt": 1795847191},
            "secondary": {"usedPercent": 69, "windowDurationMins": 10080, "resetsAt": 1795968029}}}});
        let quotas = codex_quotas(&result).unwrap();
        assert_eq!(quotas[0].name, "codex, 5-hour");
        assert_eq!((quotas[0].used, quotas[0].resets_at), (1.0, Some(1795847191)));
        assert_eq!(quotas[1].name, "codex, weekly");
        assert_eq!(quotas[1].models, None);
    }
}

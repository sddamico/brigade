//! Drives the brigade binary against fake claude, codex and agy CLIs that
//! record how they were called and print canned output.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const FAKE_CLI: &str = r#"#!/bin/sh
name=$(basename "$0")
log="$FAKE_LOG/$name"
mkdir -p "$log"
echo $$ > "$log/pid"
echo call >> "$log/calls"
: > "$log/args"
for arg in "$@"; do printf '%s\n--\n' "$arg" >> "$log/args"; done
if [ "$name" = codex ] && [ "$1" = app-server ]; then
  head -n 3 > /dev/null
  echo '{"jsonrpc":"2.0","id":1,"result":{}}'
  echo '{"jsonrpc":"2.0","id":2,"result":{"rateLimitsByLimitId":{"codex":{"normalModelSlug":null,"primary":{"usedPercent":100,"windowDurationMins":300,"resetsAt":1900000000},"secondary":{"usedPercent":40,"windowDurationMins":10080,"resetsAt":1900100000}}}}}'
  exit 0
fi
if [ "$name" = claude ] && [ "$2" = /usage ]; then
  printf 'Current week (all models): 37%% used · resets Dec 2, 2am (America/New_York)\nCurrent week (Fable): 23%% used · resets Dec 2, 2am (America/New_York)\n'
  exit 0
fi
if [ "$name" = agy ] && [ "$1" = --print=/usage ]; then
  printf '%s\n' '{"status":"SUCCESS","command":{"name":"usage","data":{"groups":[{"name":"Gemini Models","description":"Models within this group: Gemini 4 Neon, Gemini 4 Hydrogen","buckets":[{"window":"weekly","remaining_fraction":0,"reset_time":"2030-03-17T17:46:40Z"}]},{"name":"Claude and GPT models","description":"Models within this group: Claude Opus 5.5","buckets":[{"window":"5h","remaining_fraction":0.9,"reset_time":"2030-03-17T17:46:40Z"}]}]}}}'
  exit 0
fi
pwd > "$log/cwd"
cat > "$log/stdin"
printf '%s\n%s\n' "${BRIGADE_DEPTH:-}" "${ANTHROPIC_BASE_URL:-}" > "$log/env"
if [ -n "$FAKE_DETACHED_CHILD" ]; then
  python3 -c 'import os, time; os.setsid(); time.sleep(60)' &
  echo $! > "$log/detached-child"
fi
[ -n "$FAKE_SLEEP" ] && sleep "$FAKE_SLEEP"
[ -n "$FAKE_WRITE" ] && printf 'changed\n' > "$FAKE_WRITE"
[ -n "$FAKE_BRANCH" ] && git checkout -qb "$FAKE_BRANCH"
[ -n "$FAKE_ESCAPE" ] && git -C "$FAKE_ESCAPE" checkout -qb escaped
[ -n "$FAKE_COMMIT" ] && printf 'mine\n' > "$FAKE_COMMIT" && git add -A && git commit -qm "agent's own commit"
if [ -n "$FAKE_FAIL" ]; then echo "fake $name: $FAKE_FAIL" >&2; exit 1; fi
case " $FAKE_LIMIT " in *" $name "*)
  case "$name" in
    claude)
      printf '{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":%s,"rateLimitType":"five_hour"}}\n' "${FAKE_RESETS_AT:-1900000000}"
      printf '%s\n' '{"type":"result","subtype":"success","is_error":true,"api_error_status":429,"api_error":"rate_limit","result":"You'"'"'ve hit your session limit · resets 1am (America/New_York)","session_id":"claude-session"}' ;;
    codex)
      echo '{"type":"thread.started","thread_id":"codex-thread"}'
      echo '{"type":"turn.failed","error":{"message":"You’ve hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), visit https://chatgpt.com/settings/usage to purchase more credits or try again at Nov 14th, 2030 7:50 AM."}}' ;;
    agy)
      printf '%s\n' '{"conversation_id":"agy-conversation","status":"ERROR","response":"","error":"Your AI credits balance is too low to continue.","finished_reason":"error"}'
      printf '%s\n' 'AGY_ERROR: {"status":"RESOURCE_EXHAUSTED","error_code":429,"retryable":false}' >&2 ;;
    muse-code) printf '%s\n' '{"error":"you hit your usage limit until 5pm"}' ;;
  esac
  exit 1 ;;
esac
case "$name" in
  claude)
    echo '{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":1900000000,"rateLimitType":"seven_day","unifiedWindows":{"seven_day":{"utilization":0.82,"resetsAt":1900000000,"status":"allowed_warning"}}}}'
    echo '{"type":"result","subtype":"success","is_error":false,"result":"claude reply","session_id":"claude-session"}' ;;
  codex)
    out=""; prev=""
    for arg in "$@"; do [ "$prev" = "-o" ] && out="$arg"; prev="$arg"; done
    printf 'codex reply\n' > "$out"
    echo '{"type":"thread.started","thread_id":"codex-thread"}'
    echo '{"type":"turn.completed","usage":{}}' ;;
  agy) printf '%s\n' '{"conversation_id":"agy-conversation","status":"SUCCESS","response":"agy reply\n"}' ;;
  muse-code) printf '%s\n' '{"result":"muse reply\n","session_id":"muse-session"}' ;;
esac
"#;

struct Sandbox {
    _dir: tempfile::TempDir,
    // Canonical, since macOS temp dirs sit behind the /var -> /private/var link.
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("fake-cli");
        fs::write(&fake, FAKE_CLI).unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        for name in ["claude", "codex", "agy", "muse-code"] {
            std::os::unix::fs::symlink(&fake, bin.join(name)).unwrap();
        }
        fs::create_dir_all(root.join("work")).unwrap();
        Sandbox { _dir: dir, root }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn work(&self) -> PathBuf {
        self.root().join("work").canonicalize().unwrap()
    }

    fn user_roster(&self, text: &str) {
        let path = self.root().join("config/brigade/roster.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_brigade"));
        command
            .args(args)
            .current_dir(self.work())
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", self.root().join("bin").display()))
            .env("HOME", self.root())
            .env("XDG_CONFIG_HOME", self.root().join("config"))
            .env("XDG_STATE_HOME", self.root().join("state"))
            .env("FAKE_LOG", self.root().join("calls"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .stdin(Stdio::null());
        command
    }

    fn brigade(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn brigade_with_stdin(&self, args: &[&str], stdin: &str) -> Output {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    fn call(&self, cli: &str, file: &str) -> String {
        fs::read_to_string(self.root().join("calls").join(cli).join(file)).unwrap()
    }

    fn args(&self, cli: &str) -> Vec<String> {
        self.call(cli, "args")
            .split("\n--\n")
            .filter(|a| !a.is_empty())
            .map(String::from)
            .collect()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn is_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

impl Sandbox {
    /// The pid of the fake harness once it has started.
    fn harness_pid(&self, cli: &str) -> i32 {
        let path = self.root().join("calls").join(cli).join("pid");
        for _ in 0..100 {
            if let Some(pid) = fs::read_to_string(&path).ok().and_then(|p| p.trim().parse().ok()) {
                return pid;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("{cli} never started");
    }
}

fn run_id(started: &str) -> String {
    started.split_whitespace().nth(3).unwrap().to_string()
}

#[test]
fn read_only_codex_run_prints_the_reply_and_how_to_continue() {
    let sandbox = Sandbox::new();
    let output = sandbox.brigade(&["run", "sol", "--effort", "high", "find", "the", "bug"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let out = stdout(&output);
    assert!(out.starts_with("codex reply\n"), "{out}");
    assert!(
        out.contains("[brigade] sol ran in codex (gpt-6.2-sol, effort high, read-only)"),
        "{out}"
    );
    let work = sandbox.work();
    assert!(
        out.contains(&format!(
            "brigade run sol --account codex --resume codex-thread --cwd {} --effort high\n",
            work.display()
        )),
        "{out}"
    );

    let args = sandbox.args("codex");
    assert!(args.windows(2).any(|w| w == ["-s", "read-only"]), "{args:?}");
    assert!(args.windows(2).any(|w| w == ["-m", "gpt-6.2-sol"]), "{args:?}");
    assert!(sandbox.call("codex", "stdin").ends_with("find the bug"));
    assert_eq!(sandbox.call("codex", "env").lines().next(), Some("1"));
    assert_eq!(sandbox.call("codex", "cwd").trim(), work.to_str().unwrap());
}

#[test]
fn write_run_on_a_resumed_claude_session_takes_the_task_from_stdin() {
    let sandbox = Sandbox::new();
    let output = sandbox.brigade_with_stdin(&["run", "opus", "--write", "--resume", "s-0"], "apply the fix\n");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("--resume claude-session --cwd"));
    assert!(stdout(&output).contains(" --write\n"));

    let args = sandbox.args("claude");
    assert!(args.windows(2).any(|w| w == ["--resume", "s-0"]), "{args:?}");
    assert!(!args.contains(&"--disallowedTools".to_string()), "{args:?}");
    assert_eq!(sandbox.call("claude", "stdin"), "apply the fix");
}

#[test]
fn user_roster_agents_get_their_env_expanded() {
    let sandbox = Sandbox::new();
    sandbox.user_roster(
        r#"
        [[account]]
        name = "local"

        [[account.agent]]
        name = "lab-claude"
        harness = "claude"
        model = "local"
        effort = "medium"
        description = "Claude Code on a local model"
        env = { ANTHROPIC_BASE_URL = "$LAB_SERVER" }
        "#,
    );

    let listed = stdout(&sandbox.brigade(&["list"]));
    assert!(
        listed.contains("lab-claude\n  local account: Claude Code on a local model (local, effort medium, from "),
        "{listed}"
    );

    let unset = sandbox.brigade(&["run", "lab-claude", "hello"]);
    assert!(!unset.status.success());
    assert!(
        stderr(&unset).contains("`lab-claude` needs $LAB_SERVER, which isn't set"),
        "{}",
        stderr(&unset)
    );

    let output = sandbox
        .command(&["run", "lab-claude", "hello"])
        .env("LAB_SERVER", "http://box:8787")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(sandbox.call("claude", "env").lines().nth(1), Some("http://box:8787"));
}

#[test]
fn detached_runs_report_running_until_they_finish() {
    let sandbox = Sandbox::new();
    let started = sandbox
        .command(&["run", "neon", "--detach", "research", "it"])
        .env("FAKE_SLEEP", "2")
        .output()
        .unwrap();
    assert!(started.status.success(), "{}", stderr(&started));
    let id = run_id(&stdout(&started));

    let early = sandbox.brigade(&["wait", &id, "--timeout", "0"]);
    assert_eq!(early.status.code(), Some(3));
    assert!(stdout(&early).contains("is still running"), "{}", stdout(&early));

    let done = sandbox.brigade(&["wait", &id]);
    assert!(done.status.success(), "{}", stderr(&done));
    assert!(stdout(&done).starts_with("agy reply\n"), "{}", stdout(&done));
    assert!(sandbox.args("agy")[0].starts_with("--print=This is a read-only run"));
}

#[test]
fn a_failing_harness_reports_its_error_and_logs() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command(&["run", "astra", "go"])
        .env("FAKE_FAIL", "disk full")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(
        err.contains("[brigade] astra failed: `codex` exited with exit status: 1"),
        "{err}"
    );
    assert!(err.contains("fake codex: disk full"), "{err}");
    assert!(err.contains("[brigade] logs: "), "{err}");
}

#[test]
fn a_usage_limit_is_its_own_outcome() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command(&["run", "sol", "go"])
        .env("FAKE_LIMIT", "codex")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    let err = stderr(&output);
    assert!(
        err.contains("[brigade] sol hit a usage limit in Codex: You’ve hit your usage limit."),
        "{err}"
    );
    assert!(err.contains("try again at Nov 14th, 2030 7:50 AM."), "{err}");

    let json = sandbox
        .command(&["run", "neon", "--json", "go"])
        .env("FAKE_LIMIT", "agy")
        .output()
        .unwrap();
    assert_eq!(json.status.code(), Some(4));
    let value: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
    assert_eq!(value["status"], "limited");
    assert_eq!(value["error"], "Your AI credits balance is too low to continue.");
}

#[test]
fn a_limited_run_switches_to_its_fallback() {
    let sandbox = Sandbox::new();
    sandbox.user_roster("[fallbacks]\nsol = \"opus\"\n");

    let output = sandbox
        .command(&["run", "sol", "go"])
        .env("FAKE_LIMIT", "codex")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.starts_with("claude reply\n"), "{out}");
    assert!(
        out.contains("[brigade] sol hit a usage limit on the codex account, so opus ran on the claude account instead: You’ve hit your usage limit."),
        "{out}"
    );
    assert!(
        out.contains("continue it with: brigade run opus --account claude --resume claude-session"),
        "{out}"
    );
    let args = sandbox.args("claude");
    assert!(args.windows(2).any(|pair| pair == ["--model", "opus"]), "{args:?}");

    let started = sandbox
        .command(&["run", "sol", "--detach", "go"])
        .env("FAKE_LIMIT", "codex")
        .output()
        .unwrap();
    let waited = sandbox.brigade(&["wait", &run_id(&stdout(&started))]);
    assert!(
        stdout(&waited).contains("[brigade] opus ran in claude"),
        "{}",
        stdout(&waited)
    );

    let both = sandbox
        .command(&["run", "sol", "go"])
        .env("FAKE_LIMIT", "codex claude")
        .output()
        .unwrap();
    assert_eq!(both.status.code(), Some(4));
    let err = stderr(&both);
    assert!(
        err.contains("sol hit a usage limit on the codex account, so opus ran on the claude account instead"),
        "{err}"
    );
    assert!(
        err.contains("[brigade] opus hit a usage limit in Claude Code: You've hit your session limit"),
        "{err}"
    );
}

#[test]
fn a_call_can_pick_or_refuse_the_fallback() {
    let sandbox = Sandbox::new();
    sandbox.user_roster("[fallbacks]\nsol = \"opus\"\n");

    let refused = sandbox
        .command(&["run", "sol", "--no-fallback", "go"])
        .env("FAKE_LIMIT", "codex")
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(4));

    let picked = sandbox
        .command(&["run", "sol", "--fallback", "neon", "go"])
        .env("FAKE_LIMIT", "codex")
        .output()
        .unwrap();
    assert!(picked.status.success(), "{}", stderr(&picked));
    assert!(stdout(&picked).starts_with("agy reply\n"), "{}", stdout(&picked));
    assert!(
        stdout(&picked).contains("so neon ran on the agy account instead"),
        "{}",
        stdout(&picked)
    );

    let resumed = sandbox
        .command(&["run", "sol", "--resume", "codex-thread", "go"])
        .env("FAKE_LIMIT", "codex")
        .output()
        .unwrap();
    assert_eq!(
        resumed.status.code(),
        Some(4),
        "a resumed session can't move to another agent"
    );
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

impl Sandbox {
    /// Makes the sandbox's work dir a repo with one commit and a `pkg/` subdirectory.
    fn repo(&self) -> PathBuf {
        let work = self.work();
        git_in(&work, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(work.join("pkg")).unwrap();
        fs::write(work.join("pkg/inner.txt"), "v1\n").unwrap();
        git_in(&work, &["add", "-A"]);
        git_in(&work, &["commit", "-q", "-m", "start"]);
        work
    }
}

#[test]
fn a_write_run_in_a_worktree_commits_its_changes_on_its_branch() {
    let sandbox = Sandbox::new();
    let work = sandbox.repo();
    let output = sandbox
        .command(&["run", "hydrogen", "--write", "--worktree", "build", "it"])
        .current_dir(work.join("pkg"))
        .env("FAKE_WRITE", "made.txt")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);

    // It ran in the same subdirectory of the worktree that it was started from.
    let cwd = PathBuf::from(sandbox.call("agy", "cwd").trim());
    let worktree = cwd.parent().unwrap().to_path_buf();
    assert!(cwd.ends_with("pkg"), "{}", cwd.display());
    assert!(worktree.starts_with(sandbox.root().join("state/brigade/worktrees/work")));

    let branch = git_in(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"])
        .trim()
        .to_string();
    assert!(branch.starts_with("brigade/hydrogen-"), "{branch}");
    assert!(
        out.contains(&format!("on branch {branch}, in worktree {}", worktree.display())),
        "{out}"
    );
    assert!(out.contains("[brigade] its changes are committed as "), "{out}");
    assert_eq!(
        git_in(&work, &["log", "--format=%s", &format!("main..{branch}")]).trim(),
        format!("brigade: hydrogen run {}", branch.trim_start_matches("brigade/"))
    );
    assert!(git_in(&worktree, &["status", "--porcelain"]).is_empty());
    assert!(!work.join("pkg/made.txt").exists(), "the main checkout was changed");
    assert!(!sandbox.args("agy").contains(&"--settings".to_string()));

    // A follow-up in the worktree still names it.
    let resume_line = out
        .lines()
        .find(|l| l.starts_with("[brigade] continue it with: "))
        .unwrap();
    let mut words: Vec<String> = resume_line
        .trim_start_matches("[brigade] continue it with: brigade ")
        .split(' ')
        .map(String::from)
        .collect();
    words.push("more".into());
    let args: Vec<&str> = words.iter().map(String::as_str).collect();
    let followup = sandbox.command(&args).env("FAKE_WRITE", "more.txt").output().unwrap();
    assert!(followup.status.success(), "{}", stderr(&followup));
    assert!(
        stdout(&followup).contains(&format!("on branch {branch}, in worktree {}", worktree.display())),
        "{}",
        stdout(&followup)
    );
}

#[test]
fn a_worktree_needs_a_write_run_and_a_commit_to_start_from() {
    let sandbox = Sandbox::new();
    let read_only = sandbox.brigade(&["run", "neon", "--worktree", "look"]);
    assert_eq!(read_only.status.code(), Some(2));
    assert!(stderr(&read_only).contains("--write"), "{}", stderr(&read_only));

    git_in(&sandbox.work(), &["init", "-q", "-b", "main"]);
    let empty = sandbox.brigade(&["run", "neon", "--write", "--worktree", "go"]);
    assert!(
        stderr(&empty).contains("--worktree needs a commit to branch from"),
        "{}",
        stderr(&empty)
    );
    let runs = sandbox.root().join("state/brigade/runs");
    assert_eq!(
        fs::read_dir(&runs).map(|d| d.count()).unwrap_or(0),
        0,
        "a refused run left a run dir"
    );
}

#[test]
fn a_failed_worktree_run_cleans_up_only_when_it_changed_nothing() {
    let sandbox = Sandbox::new();
    let work = sandbox.repo();
    let failed = sandbox
        .command(&["run", "sol", "--write", "--worktree", "go"])
        .env("FAKE_FAIL", "boom")
        .output()
        .unwrap();
    assert!(
        stderr(&failed).contains("it left no changes, so its worktree and branch brigade/sol-"),
        "{}",
        stderr(&failed)
    );
    assert!(!git_in(&work, &["branch", "--list", "brigade/*"]).contains("brigade/"));

    let messy = sandbox
        .command(&["run", "sol", "--write", "--worktree", "go"])
        .env("FAKE_FAIL", "boom")
        .env("FAKE_WRITE", "half.txt")
        .output()
        .unwrap();
    assert!(stderr(&messy).contains("is still there"), "{}", stderr(&messy));
    assert!(git_in(&work, &["branch", "--list", "brigade/*"]).contains("brigade/sol-"));
}

#[test]
fn a_worktree_run_notes_uncommitted_changes_it_left_out() {
    let sandbox = Sandbox::new();
    let work = sandbox.repo();
    fs::write(work.join("pkg/inner.txt"), "v2 in progress\n").unwrap();
    let output = sandbox.brigade(&["run", "opus", "--write", "--worktree", "go"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("note: the worktree started from HEAD, so uncommitted changes in"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn runs_refuse_to_nest_past_the_depth_cap() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command(&["run", "sol", "x"])
        .env("BRIGADE_DEPTH", "2")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("runs nest at most 2 deep"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn unknown_agents_and_unsupported_efforts_are_rejected() {
    let sandbox = Sandbox::new();
    let unknown = stderr(&sandbox.brigade(&["run", "gpt", "x"]));
    assert!(
        unknown.contains("no agent `gpt` in the roster; try one of: fable, opus, sonnet"),
        "{unknown}"
    );
    let effort = stderr(&sandbox.brigade(&["run", "neon", "--effort", "ultra", "x"]));
    assert!(
        effort.contains("neon runs in agy, which takes effort low, medium, high, xhigh"),
        "{effort}"
    );
}

#[test]
fn cancel_stops_the_harness_and_records_why() {
    let sandbox = Sandbox::new();
    let started = sandbox
        .command(&["run", "neon", "--detach", "--write", "long", "job"])
        .env("FAKE_SLEEP", "30")
        .output()
        .unwrap();
    let id = run_id(&stdout(&started));
    let harness = sandbox.harness_pid("agy");
    assert!(is_alive(harness));

    let cancelled = sandbox.brigade(&["cancel", &id]);
    assert!(cancelled.status.success(), "{}", stderr(&cancelled));
    assert!(
        stderr(&cancelled).contains("was cancelled: cancelled with brigade cancel"),
        "{}",
        stderr(&cancelled)
    );
    assert!(!is_alive(harness), "the harness outlived its cancel");

    let waited = sandbox.brigade(&["wait", &id]);
    assert_eq!(waited.status.code(), Some(1));
    assert!(stderr(&waited).contains("was cancelled"), "{}", stderr(&waited));
}

#[test]
fn cancel_reaches_commands_the_harness_started_in_their_own_session() {
    let sandbox = Sandbox::new();
    let started = sandbox
        .command(&["run", "astra", "--detach", "job"])
        .env("FAKE_SLEEP", "30")
        .env("FAKE_DETACHED_CHILD", "1")
        .output()
        .unwrap();
    let id = run_id(&stdout(&started));
    sandbox.harness_pid("codex");
    let path = sandbox.root().join("calls/codex/detached-child");
    let mut detached = None;
    for _ in 0..50 {
        detached = fs::read_to_string(&path)
            .ok()
            .and_then(|p| p.trim().parse::<i32>().ok());
        if detached.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let detached = detached.expect("the fake started its detached child");
    assert!(is_alive(detached));

    assert!(sandbox.brigade(&["cancel", &id]).status.success());
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!is_alive(detached), "a command in its own session outlived the cancel");
}

#[test]
fn a_run_nobody_waits_on_is_cancelled_when_its_lease_lapses() {
    let sandbox = Sandbox::new();
    let started = sandbox
        .command(&["run", "sol", "--detach", "long", "job"])
        .env("FAKE_SLEEP", "30")
        .env("BRIGADE_LEASE_SECS", "1")
        .output()
        .unwrap();
    let id = run_id(&stdout(&started));
    let harness = sandbox.harness_pid("codex");
    std::thread::sleep(std::time::Duration::from_secs(4));

    assert!(!is_alive(harness), "the harness outlived its lease");
    let waited = sandbox.brigade(&["wait", &id]);
    assert!(
        stderr(&waited).contains("was cancelled: nobody waited on it for 1s"),
        "{}",
        stderr(&waited)
    );
}

#[test]
fn waiting_on_a_run_keeps_its_lease() {
    let sandbox = Sandbox::new();
    let started = sandbox
        .command(&["run", "sol", "--detach", "job"])
        .env("FAKE_SLEEP", "3")
        .env("BRIGADE_LEASE_SECS", "1")
        .output()
        .unwrap();
    let id = run_id(&stdout(&started));
    let done = sandbox
        .command(&["wait", &id])
        .env("BRIGADE_LEASE_SECS", "1")
        .output()
        .unwrap();
    assert!(done.status.success(), "{}", stderr(&done));
    assert!(stdout(&done).starts_with("codex reply\n"));
}

#[test]
fn ctrl_c_on_a_foreground_run_cancels_it() {
    let sandbox = Sandbox::new();
    let child = sandbox
        .command(&["run", "opus", "job"])
        .env("FAKE_SLEEP", "30")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let harness = sandbox.harness_pid("claude");
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130));
    assert!(
        stderr(&output).contains("was cancelled: interrupted"),
        "{}",
        stderr(&output)
    );
    assert!(!is_alive(harness), "the harness outlived Ctrl-C");
}

#[test]
fn the_call_can_choose_the_model_and_otherwise_leaves_effort_to_the_harness() {
    let sandbox = Sandbox::new();
    let output = sandbox.brigade(&["run", "sol", "--model", "gpt-6.1-sol", "go"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.contains("(gpt-6.1-sol, the harness's default effort, read-only)"),
        "{out}"
    );
    assert!(out.contains(" --model gpt-6.1-sol\n"), "{out}");
    let args = sandbox.args("codex");
    assert!(args.windows(2).any(|w| w == ["-m", "gpt-6.1-sol"]), "{args:?}");
    assert!(
        !args.iter().any(|a| a.starts_with("model_reasoning_effort")),
        "{args:?}"
    );
}

#[test]
fn a_waiter_whose_caller_was_killed_stops_holding_the_run() {
    let sandbox = Sandbox::new();
    let started = sandbox
        .command(&["run", "neon", "--detach", "job"])
        .env("FAKE_SLEEP", "30")
        .env("BRIGADE_LEASE_SECS", "2")
        .output()
        .unwrap();
    let id = run_id(&stdout(&started));
    let harness = sandbox.harness_pid("agy");

    // A stand-in for a forwarder's shell: it waits on the run, then is killed
    // outright, leaving its `brigade wait` orphaned.
    let waiter_pid = sandbox.root().join("waiter.pid");
    let mut caller = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#""$BRIGADE" wait "$RUN" > /dev/null 2>&1 & echo $! > "$WAITER_PID"; wait"#)
        .env("BRIGADE", env!("CARGO_BIN_EXE_brigade"))
        .env("RUN", &id)
        .env("WAITER_PID", &waiter_pid)
        .env("HOME", sandbox.root())
        .env("XDG_STATE_HOME", sandbox.root().join("state"))
        .env("XDG_CONFIG_HOME", sandbox.root().join("config"))
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(1));
    caller.kill().unwrap();
    let _ = caller.wait();
    let waiter: i32 = fs::read_to_string(&waiter_pid).unwrap().trim().parse().unwrap();

    std::thread::sleep(std::time::Duration::from_secs(5));
    assert!(!is_alive(waiter), "the orphaned waiter kept going");
    assert!(!is_alive(harness), "the run outlived its killed caller");
}

#[test]
fn an_agent_that_commits_its_own_work_still_counts_as_changing_something() {
    let sandbox = Sandbox::new();
    let work = sandbox.repo();
    let output = sandbox
        .command(&["run", "neon", "--write", "--worktree", "go"])
        .env("FAKE_COMMIT", "self.txt")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("[brigade] its changes are committed as "), "{out}");
    assert!(!out.contains("changed nothing"), "{out}");
    let branch = out.split("on branch ").nth(1).unwrap().split(',').next().unwrap();
    assert_eq!(
        git_in(&work, &["log", "--format=%s", &format!("main..{branch}")]).trim(),
        "agent's own commit"
    );
}

#[test]
fn a_worktree_agent_is_told_to_leave_the_checkout_alone_and_caught_when_it_doesnt() {
    let sandbox = Sandbox::new();
    let work = sandbox.repo();
    let output = sandbox
        .command(&["run", "sol", "--write", "--worktree", "go"])
        .env("FAKE_ESCAPE", &work)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let brief = sandbox.call("codex", "stdin");
    assert!(
        brief.starts_with("You're working in a git worktree made for this task, at "),
        "{brief}"
    );
    assert!(
        brief.contains(&format!("{} is the user's own checkout", work.display())),
        "{brief}"
    );
    let out = stdout(&output);
    assert!(
        out.contains(&format!(
            "[brigade] warning: the user's checkout {} changed during the run: it's on branch escaped now, not branch main. Unless the user did that, the agent worked there",
            work.display()
        )),
        "{out}"
    );

    let clean = sandbox.brigade(&["run", "sol", "--write", "--worktree", "go"]);
    assert!(!stdout(&clean).contains("warning"), "{}", stdout(&clean));
}

#[test]
fn work_committed_on_the_agents_own_branch_lands_on_the_runs_branch() {
    let sandbox = Sandbox::new();
    let work = sandbox.repo();
    let output = sandbox
        .command(&["run", "neon", "--write", "--worktree", "go"])
        .env("FAKE_BRANCH", "sd-own")
        .env("FAKE_COMMIT", "self.txt")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    let branch = out.split("on branch ").nth(1).unwrap().split(',').next().unwrap();
    assert!(branch.starts_with("brigade/neon-"), "{out}");
    assert_eq!(
        git_in(&work, &["log", "--format=%s", &format!("main..{branch}")]).trim(),
        "agent's own commit"
    );
    assert!(
        out.contains("[brigade] the agent also made branch sd-own at the same commit"),
        "{out}"
    );
    let path = out.split("in worktree ").nth(1).unwrap().lines().next().unwrap();
    assert_eq!(git_in(Path::new(path), &["branch", "--show-current"]).trim(), branch);
}

#[test]
fn install_agents_gives_each_user_agent_a_subagent_in_every_harness() {
    let sandbox = Sandbox::new();
    let roster = r#"
[[catalog]]
name = "sol"
description = "pinned to the previous Sol"
models = { codex = "gpt-6.1-sol" }

[[account]]
name = "local"

[[account.agent]]
name = "lab-claude"
harness = "claude"
model = "local"
description = "Claude Code on the local model"
"#;
    sandbox.user_roster(roster);
    let home = sandbox.root();
    let theirs = home.join(".claude/agents/mine.md");
    fs::create_dir_all(theirs.parent().unwrap()).unwrap();
    fs::write(&theirs, "---\nname: mine\n---\n").unwrap();

    let installed = sandbox.brigade(&["install-agents"]);
    assert!(installed.status.success(), "{}", stderr(&installed));
    let files = [
        ".claude/agents/lab-claude.md",
        ".codex/agents/lab-claude.toml",
        ".gemini/config/agents/lab-claude.md",
    ];
    for file in files {
        let text = fs::read_to_string(home.join(file)).unwrap();
        assert!(text.contains("brigade run lab-claude"), "{file}: {text}");
    }
    let claude = fs::read_to_string(home.join(files[0])).unwrap();
    assert!(
        claude.starts_with("---\n# Written by `brigade install-agents`"),
        "{claude}"
    );
    assert!(claude.contains("\nname: lab-claude\n"), "{claude}");
    let codex: toml::Value = toml::from_str(&fs::read_to_string(home.join(files[1])).unwrap()).unwrap();
    assert_eq!(codex["name"].as_str(), Some("lab-claude"));
    assert!(
        !home.join(".claude/agents/sol.md").exists(),
        "a replaced built-in keeps the plugin's forwarder"
    );

    let again = sandbox.brigade(&["install-agents"]);
    assert_eq!(
        stdout(&again),
        "the subagents for your roster's agents are up to date\n"
    );

    sandbox.user_roster("");
    let removed = sandbox.brigade(&["install-agents"]);
    assert_eq!(stdout(&removed).matches("removed ").count(), 3, "{}", stdout(&removed));
    assert!(files.iter().all(|file| !home.join(file).exists()));
    assert!(theirs.exists(), "the user's own agents stay");

    fs::write(home.join(files[0]), "someone else's").unwrap();
    sandbox.user_roster(roster);
    let kept = sandbox.brigade(&["install-agents"]);
    assert!(stderr(&kept).contains("brigade didn't write it"), "{}", stderr(&kept));
    assert_eq!(fs::read_to_string(home.join(files[0])).unwrap(), "someone else's");
}

const MUSE_ROSTER: &str = r#"
[[harness]]
name = "muse"
product = "Muse Code"
program = "muse-code"
args = ["exec", "--json", "--model", "{model}"]
efforts = ["low", "high"]
effort_args = ["--effort", "{effort}"]
read_only_args = ["--read-only"]
write_args = ["--yolo"]
resume_args = ["--resume", "{session}"]

[harness.output]
format = "json"
reply = "/result"
session = "/session_id"
error = "/error"
limit_signs = ["usage limit"]

[harness.host]
agents_dir = "~/.muse/agents"
file = "{name}.md"
template = """
---
# {marker}
name: {name}
description: {description}
---

{instructions}
"""

[[account]]
name = "muse"
harness = "muse"

[[account.agent]]
name = "muse"
model = "muse-2"
description = "Muse Code's own model"
"#;

#[test]
fn a_custom_harness_runs_agents_from_its_roster_declaration() {
    let sandbox = Sandbox::new();
    sandbox.user_roster(MUSE_ROSTER);

    let output = sandbox.brigade(&["run", "muse", "--effort", "high", "go"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.starts_with("muse reply\n\n[brigade] muse ran in muse"), "{out}");
    assert!(
        out.contains("continue it with: brigade run muse --account muse --resume muse-session"),
        "{out}"
    );
    assert_eq!(
        sandbox.args("muse-code"),
        ["exec", "--json", "--model", "muse-2", "--effort", "high", "--read-only"]
    );
    assert!(
        sandbox
            .call("muse-code", "stdin")
            .starts_with("This is a read-only run")
    );

    let resumed = sandbox.brigade(&["run", "muse", "--write", "--resume", "muse-session", "again"]);
    assert!(resumed.status.success(), "{}", stderr(&resumed));
    assert_eq!(
        sandbox.args("muse-code"),
        [
            "exec",
            "--json",
            "--model",
            "muse-2",
            "--yolo",
            "--resume",
            "muse-session"
        ]
    );

    let effort = sandbox.brigade(&["run", "muse", "--effort", "ultra", "go"]);
    assert!(
        stderr(&effort).contains("muse runs in muse, which takes effort low, high"),
        "{}",
        stderr(&effort)
    );

    let limited = sandbox
        .command(&["run", "muse", "go"])
        .env("FAKE_LIMIT", "muse-code")
        .output()
        .unwrap();
    assert_eq!(limited.status.code(), Some(4));
    assert!(
        stderr(&limited).contains("[brigade] muse hit a usage limit in Muse Code: you hit your usage limit until 5pm"),
        "{}",
        stderr(&limited)
    );
}

#[test]
fn a_custom_harness_with_a_host_section_gets_forwarders_for_every_other_agent() {
    let sandbox = Sandbox::new();
    sandbox.user_roster(MUSE_ROSTER);
    let installed = sandbox.brigade(&["install-agents"]);
    assert!(installed.status.success(), "{}", stderr(&installed));

    let dir = sandbox.root().join(".muse/agents");
    let sol = fs::read_to_string(dir.join("sol.md")).unwrap();
    assert!(sol.starts_with("---\n# Written by `brigade install-agents`"), "{sol}");
    assert!(
        sol.contains("\nname: sol\ndescription: \"GPT-6.2 Sol in Codex"),
        "{sol}"
    );
    assert!(sol.contains("brigade run sol"), "{sol}");
    for name in ["opus", "fable", "sonnet", "astra", "neon", "hydrogen", "forward"] {
        assert!(dir.join(format!("{name}.md")).exists(), "{name}");
    }
    assert!(!dir.join("muse.md").exists(), "a host runs its own agents itself");
    let forward = fs::read_to_string(dir.join("forward.md")).unwrap();
    assert!(
        forward.contains("Also for muse when this session runs on another model"),
        "{forward}"
    );
    assert!(
        sandbox.root().join(".claude/agents/muse.md").exists(),
        "the built-in hosts get muse"
    );

    let bad = MUSE_ROSTER.replace("\"{model}\"]", "\"{modle}\"]");
    sandbox.user_roster(&bad);
    let rejected = sandbox.brigade(&["list"]);
    assert!(
        stderr(&rejected).contains("harness `muse` has `{modle}` in args, which takes `{model}`, `{prompt}`, `{cwd}`"),
        "{}",
        stderr(&rejected)
    );
}

fn calls(sandbox: &Sandbox, cli: &str) -> usize {
    fs::read_to_string(sandbox.root().join("calls").join(cli).join("calls"))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

#[test]
fn a_limited_account_is_remembered_so_the_next_run_spends_no_call() {
    let sandbox = Sandbox::new();
    let limited = sandbox
        .command(&["run", "sol", "go"])
        .env("FAKE_LIMIT", "codex")
        .output()
        .unwrap();
    assert_eq!(limited.status.code(), Some(4));
    assert!(
        stderr(&limited).contains("[brigade] the codex account is limited until 2030-11-14 07:50"),
        "{}",
        stderr(&limited)
    );

    // A ChatGPT plan's limit covers every model on it.
    let again = sandbox.brigade(&["run", "astra", "go"]);
    assert_eq!(again.status.code(), Some(4));
    assert!(
        stderr(&again)
            .contains("[brigade] astra didn't run: the codex account is still limited: You’ve hit your usage limit"),
        "{}",
        stderr(&again)
    );
    assert_eq!(calls(&sandbox, "codex"), 1, "the second run never called codex");

    let capacity = stdout(&sandbox.brigade(&["capacity"]));
    assert!(
        capacity.contains("codex (sol, astra): limited until 2030-11-14 07:50"),
        "{capacity}"
    );
    let json: serde_json::Value = serde_json::from_str(&stdout(&sandbox.brigade(&["capacity", "--json"]))).unwrap();
    let codex = json
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["account"] == "codex")
        .unwrap();
    assert_eq!(codex["free"], false);
    assert_eq!(codex["limit"]["until"].as_u64(), json_until(&sandbox));

    let cleared = sandbox.brigade(&["capacity", "--clear", "codex"]);
    assert_eq!(stdout(&cleared), "forgot the limit on the codex account\n");
    assert!(sandbox.brigade(&["run", "sol", "go"]).status.success());
    assert_eq!(calls(&sandbox, "codex"), 2);

    let traversal = sandbox.brigade(&["capacity", "--clear", "../../victim"]);
    assert!(
        stderr(&traversal).contains("there's no ../../victim account"),
        "{}",
        stderr(&traversal)
    );
    assert!(!sandbox.root().join("state/victim.json").exists());
}

#[test]
fn an_agy_limit_is_on_its_model_alone() {
    let sandbox = Sandbox::new();
    let limited = sandbox
        .command(&["run", "neon", "go"])
        .env("FAKE_LIMIT", "agy")
        .output()
        .unwrap();
    assert_eq!(limited.status.code(), Some(4));
    assert!(
        stderr(&limited).contains("lets one run try gemini-4-neon on the agy account again after"),
        "{}",
        stderr(&limited)
    );
    assert!(
        sandbox.brigade(&["run", "hydrogen", "go"]).status.success(),
        "hydrogen's model isn't limited"
    );

    let capacity = stdout(&sandbox.brigade(&["capacity"]));
    assert!(capacity.contains("agy (neon, hydrogen): free, 0 running"), "{capacity}");
    assert!(
        capacity.contains("  neon (gemini-4-neon): limited, tries again after "),
        "{capacity}"
    );
    let json: serde_json::Value = serde_json::from_str(&stdout(&sandbox.brigade(&["capacity", "--json"]))).unwrap();
    let agy = json.as_array().unwrap().iter().find(|a| a["account"] == "agy").unwrap();
    let neon = &agy["agents"][0];
    assert_eq!((&agy["free"], &neon["free"]), (&true.into(), &false.into()));
}

fn json_until(sandbox: &Sandbox) -> Option<u64> {
    let text = fs::read_to_string(sandbox.root().join("state/brigade/accounts/codex.json")).unwrap();
    serde_json::from_str::<serde_json::Value>(&text).unwrap()["limit"]["until"].as_u64()
}

#[test]
fn a_known_limited_account_falls_back_at_once() {
    let sandbox = Sandbox::new();
    sandbox.user_roster("[fallbacks]\nsol = \"opus\"\n");
    let first = sandbox
        .command(&["run", "sol", "go"])
        .env("FAKE_LIMIT", "codex")
        .output()
        .unwrap();
    assert!(first.status.success(), "{}", stderr(&first));

    let second = sandbox.brigade(&["run", "sol", "go"]);
    assert!(second.status.success(), "{}", stderr(&second));
    assert!(stdout(&second).starts_with("claude reply\n"), "{}", stdout(&second));
    assert!(
        stdout(&second)
            .contains("sol hit a usage limit on the codex account, so opus ran on the claude account instead")
    );
    assert_eq!(calls(&sandbox, "codex"), 1, "the known limit skipped codex");
}

#[test]
fn claude_reports_usage_and_the_reset_in_its_stream() {
    let sandbox = Sandbox::new();
    assert!(sandbox.brigade(&["run", "opus", "go"]).status.success());
    let capacity = stdout(&sandbox.brigade(&["capacity"]));
    assert!(
        capacity.contains("claude (fable, opus, sonnet): free, 0 running"),
        "{capacity}"
    );
    assert!(
        capacity.contains("  seven_day window 82% used, resets 2030-03-17"),
        "{capacity}"
    );

    let limited = sandbox
        .command(&["run", "opus", "go"])
        .env("FAKE_LIMIT", "claude")
        .env("FAKE_RESETS_AT", "1900000000")
        .output()
        .unwrap();
    assert_eq!(limited.status.code(), Some(4));
    assert!(
        stderr(&limited).contains("[brigade] the claude account is limited until 2030-03-17"),
        "{}",
        stderr(&limited)
    );
}

#[test]
fn wait_holds_a_run_until_its_accounts_limit_resets() {
    let sandbox = Sandbox::new();
    let reset = brigade::runs::now() + 4;
    let limited = sandbox
        .command(&["run", "opus", "go"])
        .env("FAKE_LIMIT", "claude")
        .env("FAKE_RESETS_AT", reset.to_string())
        .output()
        .unwrap();
    assert_eq!(limited.status.code(), Some(4));

    let refused = sandbox.brigade(&["run", "opus", "go"]);
    assert_eq!(refused.status.code(), Some(4), "without --wait it ends at once");

    let started = Instant::now();
    let waited = sandbox.brigade(&["run", "opus", "--wait", "go"]);
    assert!(waited.status.success(), "{}", stderr(&waited));
    assert!(started.elapsed() >= Duration::from_secs(2), "it waited for the reset");
    assert!(
        stdout(&waited).contains("[brigade] it waited ")
            && stdout(&waited).contains("for a usage limit to reset before it ran"),
        "{}",
        stdout(&waited)
    );
    assert_eq!(calls(&sandbox, "claude"), 2);
}

#[test]
fn a_full_account_holds_runs_until_a_slot_frees() {
    let sandbox = Sandbox::new();
    sandbox.user_roster(
        "[[account]]\nname = \"codex\"\nharness = \"codex\"\nagents = [\"sol\", \"astra\"]\nmax_runs = 1\n",
    );
    let first = sandbox
        .command(&["run", "sol", "--detach", "go"])
        .env("FAKE_SLEEP", "3")
        .output()
        .unwrap();
    let first = run_id(&stdout(&first));
    std::thread::sleep(Duration::from_millis(800));
    let second = run_id(&stdout(&sandbox.brigade(&["run", "astra", "--detach", "go"])));

    let early = sandbox.brigade(&["wait", &second, "--timeout", "1"]);
    assert_eq!(early.status.code(), Some(3));
    assert!(
        stdout(&early).contains(&format!(
            "run {second} is still waiting for the codex account, which is already running as many as it takes (1)"
        )),
        "{}",
        stdout(&early)
    );
    let waiting = stdout(&sandbox.brigade(&["runs", "--state", "waiting"]));
    assert!(
        waiting.contains(&format!("{second}  astra  codex  waiting")),
        "{waiting}"
    );
    let capacity = stdout(&sandbox.brigade(&["capacity"]));
    assert!(
        capacity.contains("codex (sol, astra): full, 1 of 1 runs going, 1 waiting"),
        "{capacity}"
    );

    assert!(sandbox.brigade(&["wait", &first]).status.success());
    let done = sandbox.brigade(&["wait", &second]);
    assert!(done.status.success(), "{}", stderr(&done));
    assert_eq!(calls(&sandbox, "codex"), 2);
}

#[test]
fn an_agent_on_two_accounts_runs_on_whichever_has_capacity() {
    let sandbox = Sandbox::new();
    sandbox
        .user_roster("[[account]]\nname = \"agy\"\nharness = \"agy\"\nagents = [\"neon\", \"hydrogen\", \"opus\"]\n");

    let first = sandbox
        .command(&["run", "opus", "go"])
        .env("FAKE_LIMIT", "claude")
        .output()
        .unwrap();
    assert!(first.status.success(), "{}", stderr(&first));
    let out = stdout(&first);
    assert!(out.starts_with("agy reply\n"), "{out}");
    assert!(
        out.contains("[brigade] opus hit a usage limit on the claude account, so opus ran on the agy account instead"),
        "{out}"
    );
    assert!(
        out.contains("continue it with: brigade run opus --account agy --resume agy-conversation"),
        "{out}"
    );
    let args = sandbox.args("agy");
    assert!(
        args.windows(2).any(|pair| pair == ["--model", "claude-opus-5-5"]),
        "{args:?}"
    );

    let second = sandbox.brigade(&["run", "opus", "go"]);
    assert!(stdout(&second).starts_with("agy reply\n"), "{}", stdout(&second));
    assert_eq!(calls(&sandbox, "claude"), 1, "the known limit skipped the claude route");

    let pinned = sandbox.brigade(&["run", "opus", "--account", "claude", "go"]);
    assert_eq!(pinned.status.code(), Some(4), "a pinned route doesn't move");
    let unknown = sandbox.brigade(&["run", "opus", "--account", "codex", "go"]);
    assert!(
        stderr(&unknown).contains("the codex account doesn't offer opus"),
        "{}",
        stderr(&unknown)
    );
}

#[test]
fn when_a_backoff_runs_out_one_run_tries_the_limit_and_the_rest_wait() {
    let sandbox = Sandbox::new();
    let limited = sandbox
        .command(&["run", "neon", "go"])
        .env("FAKE_LIMIT", "agy")
        .output()
        .unwrap();
    assert_eq!(limited.status.code(), Some(4));
    // Wind the backoff back so it has run out.
    let state = sandbox.root().join("state/brigade/accounts/agy.json");
    let mut value: serde_json::Value = serde_json::from_str(&fs::read_to_string(&state).unwrap()).unwrap();
    value["models"]["gemini-4-neon"]["retry_at"] = (brigade::runs::now() - 1).into();
    fs::write(&state, value.to_string()).unwrap();

    let waiting: Vec<String> = (0..3)
        .map(|_| {
            let started = sandbox
                .command(&["run", "neon", "--wait", "--detach", "go"])
                .env("FAKE_LIMIT", "agy")
                .output()
                .unwrap();
            run_id(&stdout(&started))
        })
        .collect();
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        for id in &waiting {
            let _ = sandbox.brigade(&["wait", id, "--timeout", "0"]);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert_eq!(calls(&sandbox, "agy"), 2, "one probe after the first limit, not three");
    for id in &waiting {
        let state = sandbox.brigade(&["wait", id, "--timeout", "0"]);
        assert_eq!(state.status.code(), Some(3), "a run asked to wait keeps waiting");
        sandbox.brigade(&["cancel", id]);
    }
    let backoff = serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&state).unwrap()).unwrap();
    assert_eq!(
        backoff["models"]["gemini-4-neon"]["backoff_secs"], 3600,
        "the failed probe doubled it"
    );
}

#[test]
fn refresh_reads_quotas_and_a_used_up_one_holds_runs_back() {
    let sandbox = Sandbox::new();
    let refreshed = sandbox.brigade(&["capacity", "--refresh"]);
    assert!(refreshed.status.success(), "{}", stderr(&refreshed));
    let out = stdout(&refreshed);
    assert!(out.contains("codex (sol, astra): limited until 2030-03-17"), "{out}");
    assert!(out.contains("  codex, 5-hour: 100% used, resets 2030-03-17"), "{out}");
    assert!(out.contains("  Current week (all models): 37% used, resets "), "{out}");
    assert!(
        out.contains("  neon (gemini-4-neon): limited until 2030-03-17"),
        "{out}"
    );

    let codex_calls = calls(&sandbox, "codex");
    let held = sandbox.brigade(&["run", "sol", "go"]);
    assert_eq!(held.status.code(), Some(4));
    assert!(
        stderr(&held).contains("sol didn't run: the codex account is still limited: codex, 5-hour is used up"),
        "{}",
        stderr(&held)
    );
    assert_eq!(calls(&sandbox, "codex"), codex_calls, "the used-up quota spent no call");
}

#[test]
fn a_local_agent_takes_the_benchmark_of_the_model_its_server_serves() {
    let sandbox = Sandbox::new();
    let models = sandbox.root().join("models.json");
    fs::write(&models, r#"{"data":[{"id":"lab-model-a","object":"model"}]}"#).unwrap();
    sandbox.user_roster(&format!(
        r#"
[benchmark_sources.aa-local]
name = "Artificial Analysis, full-precision weights"
url = "https://artificialanalysis.ai/models/lab-model-a"
read = "2026-11-27"

[[served_model]]
id = "lab-model-a"
best_for = "private, simple or high-volume work"
benchmark = {{ source = "aa-local", intelligence = 28 }}

[[account]]
name = "local"
models_url = "file://{}"

[[account.agent]]
name = "lab-claude"
harness = "claude"
model = "local"
description = "Claude Code on the local model"
"#,
        models.display()
    ));

    let before = stdout(&sandbox.brigade(&["list"]));
    assert!(
        before.contains("lab-claude\n  local account:"),
        "nothing known before a refresh: {before}"
    );

    let refreshed = sandbox.brigade(&["capacity", "--refresh"]);
    assert!(
        stdout(&refreshed).contains("local (lab-claude): free, 0 running, serving lab-model-a"),
        "{}",
        stdout(&refreshed)
    );
    let listed = stdout(&sandbox.brigade(&["list"]));
    assert!(
        listed.contains("Scores (aa-local) are from the Artificial Analysis, full-precision weights"),
        "{listed}"
    );
    assert!(
        listed.contains("lab-claude: Best for private, simple or high-volume work. Scores 28 (aa-local).\n"),
        "{listed}"
    );
    assert!(
        listed.contains("(local, the harness's default effort, from "),
        "{listed}"
    );
    assert!(listed.contains(", serving lab-model-a)"), "{listed}");
    let json: serde_json::Value = serde_json::from_str(&stdout(&sandbox.brigade(&["list", "--json"]))).unwrap();
    let local = json
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "lab-claude")
        .unwrap();
    assert_eq!(local["served_model"]["benchmark"]["intelligence"], 28.0);
    assert_eq!(local["served_model"]["benchmark"]["source"]["key"], "aa-local");

    fs::write(&models, r#"{"data":[{"id":"lab-model-b"}]}"#).unwrap();
    sandbox.brigade(&["capacity", "--refresh"]);
    let switched = stdout(&sandbox.brigade(&["list"]));
    assert!(
        switched.contains("lab-claude\n"),
        "a model with no entry says nothing: {switched}"
    );
    assert!(switched.contains(", serving lab-model-b)"), "{switched}");
}

#[test]
fn every_route_a_run_leaves_behind_gets_its_own_line() {
    let sandbox = Sandbox::new();
    sandbox.user_roster(
        "[[account]]\nname = \"agy\"\nharness = \"agy\"\nagents = [\"neon\", \"opus\"]\n\n[fallbacks]\nopus = \"sol\"\n",
    );
    let output = sandbox
        .command(&["run", "opus", "go"])
        .env("FAKE_LIMIT", "claude agy")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.contains("[brigade] opus hit a usage limit on the claude account, so opus ran on the agy account instead"),
        "{out}"
    );
    assert!(
        out.contains("[brigade] opus hit a usage limit on the agy account, so sol ran on the codex account instead: Your AI credits"),
        "{out}"
    );

    let resume = sandbox.brigade(&["run", "opus", "--resume", "s-1", "go"]);
    assert!(
        stderr(&resume)
            .contains("opus is offered by the claude and agy accounts; say which one the session is on with --account"),
        "{}",
        stderr(&resume)
    );
}

#[test]
fn rosters_that_cant_work_are_refused() {
    let sandbox = Sandbox::new();
    for (roster, problem) in [
        (
            "[[account]]\nname = \"a\"\n[[account]]\nname = \"a\"\n",
            "account `a` is defined twice",
        ),
        (
            "[[account]]\nname = \"x\"\nharness = \"codex\"\n[[account.agent]]\nname = \"opus\"\nmodel = \"m\"\ndescription = \"imposter\"\n",
            "agent `opus` has a catalog agent's name",
        ),
        (
            "[[catalog]]\nname = \"z\"\ndescription = \"z\"\nmodels = { agy = \"m\" }\nefforts = { agy = [\"ultra\"] }\n",
            "agent `z` takes effort `ultra` in agy",
        ),
        (
            "[[catalog]]\nname = \"z\"\ndescription = \"z\"\nmodels = { agy = \"m\" }\nbenchmark = { source = \"aa\", intelligence = 1000 }\n",
            "agent `z` scores 1000, outside 0 to 100",
        ),
        (
            "[[catalog]]\nname = \"z\"\ndescription = \"z\"\nmodels = { agy = \"m\" }\nbenchmark = { source = \"nope\", intelligence = 1 }\n",
            "has a benchmark from `nope`, which isn't a benchmark source",
        ),
        (
            "[benchmark_sources.mine]\nname = \"m\"\nurl = \"u\"\nread = \"yesterday\"\n",
            "benchmark source `mine` was read on `yesterday`",
        ),
    ] {
        sandbox.user_roster(roster);
        let listed = sandbox.brigade(&["list"]);
        assert!(!listed.status.success(), "{roster}");
        assert!(stderr(&listed).contains(problem), "{}", stderr(&listed));
    }
}

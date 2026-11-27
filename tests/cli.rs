//! Drives the brigade binary against fake claude, codex and agy CLIs that
//! record how they were called and print canned output.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const FAKE_CLI: &str = r#"#!/bin/sh
name=$(basename "$0")
log="$FAKE_LOG/$name"
mkdir -p "$log"
echo $$ > "$log/pid"
: > "$log/args"
for arg in "$@"; do printf '%s\n--\n' "$arg" >> "$log/args"; done
pwd > "$log/cwd"
cat > "$log/stdin"
printf '%s\n%s\n' "${BRIGADE_DEPTH:-}" "${ANTHROPIC_BASE_URL:-}" > "$log/env"
if [ -n "$FAKE_DETACHED_CHILD" ]; then
  python3 -c 'import os, time; os.setsid(); time.sleep(60)' &
  echo $! > "$log/detached-child"
fi
[ -n "$FAKE_SLEEP" ] && sleep "$FAKE_SLEEP"
if [ -n "$FAKE_FAIL" ]; then echo "fake $name: $FAKE_FAIL" >&2; exit 1; fi
case "$name" in
  claude) echo '{"type":"result","subtype":"success","is_error":false,"result":"claude reply","session_id":"claude-session"}' ;;
  codex)
    out=""; prev=""
    for arg in "$@"; do [ "$prev" = "-o" ] && out="$arg"; prev="$arg"; done
    printf 'codex reply\n' > "$out"
    echo '{"type":"thread.started","thread_id":"codex-thread"}'
    echo '{"type":"turn.completed","usage":{}}' ;;
  agy) printf '%s\n' '{"conversation_id":"agy-conversation","status":"SUCCESS","response":"agy reply\n"}' ;;
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
        for name in ["claude", "codex", "agy"] {
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
            "brigade run sol --resume codex-thread --cwd {} --effort high\n",
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
        [[agent]]
        name = "claude-local"
        harness = "claude"
        model = "local"
        effort = "medium"
        description = "Claude Code on a local model"
        env = { ANTHROPIC_BASE_URL = "$LOCAL_MODEL_SERVER" }
        "#,
    );

    let listed = stdout(&sandbox.brigade(&["list"]));
    assert!(
        listed.contains("claude-local (claude, local, effort medium, from "),
        "{listed}"
    );

    let unset = sandbox.brigade(&["run", "claude-local", "hello"]);
    assert!(!unset.status.success());
    assert!(
        stderr(&unset).contains("`claude-local` needs $LOCAL_MODEL_SERVER, which isn't set"),
        "{}",
        stderr(&unset)
    );

    let output = sandbox
        .command(&["run", "claude-local", "hello"])
        .env("LOCAL_MODEL_SERVER", "http://box:8787")
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
        .env("FAKE_FAIL", "quota exhausted")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(
        err.contains("[brigade] astra failed: `codex` exited with exit status: 1"),
        "{err}"
    );
    assert!(err.contains("fake codex: quota exhausted"), "{err}");
    assert!(err.contains("[brigade] logs: "), "{err}");
}

#[test]
fn write_team_members_get_their_own_worktree() {
    let sandbox = Sandbox::new();
    let work = sandbox.work();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&work)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .status()
            .unwrap();
        assert!(status.success());
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["commit", "-q", "--allow-empty", "-m", "start"]);

    let output = sandbox.brigade(&["run", "hydrogen", "--write", "--worktree", "build", "it"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    let worktree = PathBuf::from(sandbox.call("agy", "cwd").trim());
    assert!(
        worktree.starts_with(sandbox.root().join("state/brigade/worktrees/work")),
        "{}",
        worktree.display()
    );
    assert!(
        out.contains(&format!(
            "its changes are in worktree {} on branch brigade/hydrogen-",
            worktree.display()
        )),
        "{out}"
    );
    assert!(!sandbox.args("agy").contains(&"--settings".to_string()));
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
        unknown.contains("no agent `gpt` in the roster; try one of: opus, fable"),
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

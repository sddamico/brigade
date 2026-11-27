//! A run is one task handed to one agent. Each gets a directory under the state
//! dir holding its request, the harness's logs and its outcome, which is what
//! lets `brigade wait` pick up a detached run from another process.

use crate::harness::{Access, Effort};
use crate::invocation::{self, Request};
use crate::roster::Agent;
use crate::settings::{DEPTH_VAR, Settings};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    pub agent: Agent,
    pub effort: Effort,
    pub access: Access,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Worktree>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
    /// The depth the worker runs at, one more than whoever started it.
    pub depth: u32,
    pub started_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Outcome {
    Done { session: Option<String>, message: String },
    Failed { error: String },
}

#[derive(Debug, PartialEq)]
pub enum State {
    Finished(Outcome),
    Running,
    /// The worker process is gone and never wrote an outcome.
    Lost,
}

pub struct RunDir {
    path: PathBuf,
}

impl RunDir {
    /// Claims a fresh run id for `agent` and creates its directory.
    pub fn allocate(settings: &Settings, agent: &str) -> Result<(String, RunDir), String> {
        let runs = settings.runs_dir();
        fs::create_dir_all(&runs).map_err(|e| format!("can't create {}: {e}", runs.display()))?;
        loop {
            let id = format!("{agent}-{}", short_token());
            let path = runs.join(&id);
            match fs::create_dir(&path) {
                Ok(()) => return Ok((id, RunDir { path })),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("can't create {}: {e}", path.display())),
            }
        }
    }

    pub fn open(settings: &Settings, id: &str) -> Result<(RunRecord, RunDir), String> {
        let dir = RunDir {
            path: settings.runs_dir().join(id),
        };
        let text = fs::read_to_string(dir.file("run.json")).map_err(|_| format!("there's no run `{id}`"))?;
        let record = serde_json::from_str(&text).map_err(|e| format!("run `{id}` has a bad run.json: {e}"))?;
        Ok((record, dir))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    pub fn write_request(&self, record: &RunRecord, task: &str) -> Result<(), String> {
        write_atomic(&self.file("task.txt"), task.as_bytes())?;
        write_atomic(
            &self.file("run.json"),
            &serde_json::to_vec_pretty(record).expect("records serialize"),
        )
    }

    pub fn task(&self) -> Result<String, String> {
        fs::read_to_string(self.file("task.txt")).map_err(|e| format!("can't read the run's task: {e}"))
    }

    pub fn write_pid(&self, pid: u32) -> Result<(), String> {
        write_atomic(&self.file("pid"), pid.to_string().as_bytes())
    }

    fn write_outcome(&self, outcome: &Outcome) -> Result<(), String> {
        write_atomic(
            &self.file("result.json"),
            &serde_json::to_vec_pretty(outcome).expect("outcomes serialize"),
        )
    }

    fn outcome(&self) -> Option<Outcome> {
        let text = fs::read_to_string(self.file("result.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn state(&self) -> State {
        if let Some(outcome) = self.outcome() {
            return State::Finished(outcome);
        }
        let pid = fs::read_to_string(self.file("pid"))
            .ok()
            .and_then(|p| p.trim().parse::<i32>().ok());
        match pid {
            Some(pid) if !is_alive(pid) => match self.outcome() {
                Some(outcome) => State::Finished(outcome),
                None => State::Lost,
            },
            // No pid yet means the starter hasn't recorded its worker yet.
            _ => State::Running,
        }
    }

    pub fn wait(&self, timeout: Option<Duration>) -> State {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            let state = self.state();
            if state != State::Running || deadline.is_some_and(|d| Instant::now() >= d) {
                return state;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

/// Runs the harness CLI for `record` to completion and records the outcome.
pub fn execute(record: &RunRecord, dir: &RunDir, var: &dyn Fn(&str) -> Option<String>) -> Outcome {
    let outcome = match dir.task() {
        Ok(task) => run_harness(record, dir, &task, var),
        Err(error) => Outcome::Failed { error },
    };
    if let Err(error) = dir.write_outcome(&outcome) {
        return Outcome::Failed { error };
    }
    outcome
}

fn run_harness(record: &RunRecord, dir: &RunDir, task: &str, var: &dyn Fn(&str) -> Option<String>) -> Outcome {
    let last_message_file = dir.file("last-message.txt");
    let request = Request {
        agent: &record.agent,
        effort: record.effort,
        access: record.access,
        cwd: &record.cwd,
        resume: record.resume.as_deref(),
        task,
        last_message_file: &last_message_file,
    };
    let invocation = match invocation::build(&request, var) {
        Ok(invocation) => invocation,
        Err(invocation::UnsetVar(name)) => {
            return Outcome::Failed {
                error: format!("`{}` needs ${name}, which isn't set", record.agent.name),
            };
        }
    };

    let (stdout_path, stderr_path) = (dir.file("stdout.log"), dir.file("stderr.log"));
    let (stdout, stderr) = match (fs::File::create(&stdout_path), fs::File::create(&stderr_path)) {
        (Ok(out), Ok(err)) => (out, err),
        (Err(e), _) | (_, Err(e)) => {
            return Outcome::Failed {
                error: format!("can't create the run's logs: {e}"),
            };
        }
    };
    let mut command = Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .envs(invocation.env.iter().map(|(k, v)| (k, v)))
        .env(DEPTH_VAR, record.depth.to_string())
        .current_dir(&invocation.cwd)
        .stdin(if invocation.stdin.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(stdout)
        .stderr(stderr);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Outcome::Failed {
                error: format!("`{}` isn't on PATH", invocation.program),
            };
        }
        Err(e) => {
            return Outcome::Failed {
                error: format!("can't start `{}`: {e}", invocation.program),
            };
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        // A harness that exits early closes the pipe; its exit status tells the story.
        let _ = stdin.write_all(invocation.stdin.as_bytes());
    }
    let status = match child.wait() {
        Ok(status) => status,
        Err(e) => {
            return Outcome::Failed {
                error: format!("lost track of `{}`: {e}", invocation.program),
            };
        }
    };

    let stdout = fs::read_to_string(&stdout_path).unwrap_or_default();
    let last_message = fs::read_to_string(&last_message_file).ok();
    match invocation::parse(record.agent.harness, &stdout, last_message.as_deref()) {
        Ok(reply) => Outcome::Done {
            session: reply.session,
            message: reply.message,
        },
        Err(error) if status.success() => Outcome::Failed { error },
        Err(error) => {
            let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
            let tail = tail_lines(&stderr, 20);
            let detail = if tail.is_empty() {
                String::new()
            } else {
                format!("\n{tail}")
            };
            Outcome::Failed {
                error: format!("`{}` exited with {status}: {error}{detail}", invocation.program),
            }
        }
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn short_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    format!("{:06x}", (nanos ^ std::process::id().rotate_left(13)) & 0xff_ffff)
}

fn is_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents)
        .and_then(|()| fs::rename(&tmp, path))
        .map_err(|e| format!("can't write {}: {e}", path.display()))
}

fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

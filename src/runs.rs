//! A run is one task handed to one agent. Every run executes in a worker
//! process in a session of its own, which starts the harness CLI in a process
//! group so cancelling stops everything the harness started. The run's
//! directory holds its request, the logs, the outcome and the lease.
//!
//! The lease is how a run notices nobody wants it any more: whoever waits on a
//! run (a forwarder's `brigade wait`, or a foreground `brigade run`) renews it,
//! and a worker whose lease lapses cancels its run. Stopping a forwarder
//! subagent, or the session it belongs to, stops the waiting and so the work.

use crate::accounts::{self, Limit, Usage};
use crate::harness::{Access, Effort, Harness};
use crate::invocation::{self, ParseFailure, Request};
use crate::process::Tree;
use crate::roster::Agent;
use crate::settings::{DEPTH_VAR, Settings};
use crate::worktree::{self, Worktree, WorktreeReport};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const POLL: Duration = Duration::from_millis(500);
/// How often a waiting run looks for a slot on its account. Each look takes the
/// account's lock and reads every run, so it's slower than the lease checks.
const SLOT_POLL: Duration = Duration::from_secs(2);
/// Claude Code's usage windows that span every model on the account.
const ACCOUNT_WINDOWS: [&str; 2] = ["five_hour", "seven_day"];
/// How long a harness gets to exit after SIGTERM before it is killed.
const TERM_GRACE: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    pub agent: Agent,
    /// The model the run uses: the agent's, unless the call chose another.
    pub model: String,
    /// None leaves the effort to the harness's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    pub access: Access,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Worktree>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
    /// The depth the worker runs at, one more than whoever started it.
    pub depth: u32,
    pub started_at: u64,
    /// The agent the task goes to instead if `agent` hits a usage limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Agent>,
    /// Set once the run has switched to its fallback, which `agent` now is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fell_back_from: Option<FellBack>,
    /// Wait for a limited account to free up instead of ending as limited.
    #[serde(default)]
    pub wait: bool,
}

/// The agent a run started on before a usage limit made it switch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FellBack {
    pub agent: String,
    pub harness: Harness,
    /// The harness's message about the limit, which says when it resets.
    pub error: String,
}

impl RunRecord {
    fn fall_back(mut self, fallback: Agent, error: &str) -> RunRecord {
        self.fell_back_from = Some(FellBack {
            agent: self.agent.name.clone(),
            harness: self.agent.harness,
            error: error.to_string(),
        });
        // The call's model and effort were chosen for the first agent's harness.
        self.model = fallback.model.clone();
        self.effort = fallback.effort;
        self.agent = fallback;
        self.fallback = None;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Outcome {
    Done {
        session: Option<String>,
        message: String,
    },
    Failed {
        error: String,
    },
    /// The harness's account ran out of usage, so retrying now would fail too.
    Limited {
        error: String,
        /// False when brigade knew the account was limited and didn't start
        /// the harness at all.
        ran: bool,
        /// When the account frees up, if the harness said.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<u64>,
        /// With no reset time, when brigade lets one run try the account again.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_at: Option<u64>,
        /// The model the limit is on, when it isn't on the whole account.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    Cancelled {
        reason: String,
    },
}

#[derive(Debug, PartialEq)]
pub enum State {
    Finished(Outcome),
    /// Accepted, but held until its account has capacity.
    Waiting,
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
        let record = dir.record().map_err(|_| format!("there's no run `{id}`"))?;
        Ok((record, dir))
    }

    /// The run as it stands, which names the fallback agent once it has switched.
    pub fn record(&self) -> Result<RunRecord, String> {
        let text = fs::read_to_string(self.file("run.json")).map_err(|e| format!("can't read run.json: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("bad run.json: {e}"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Drops a run that never started.
    pub fn discard(self) {
        let _ = fs::remove_dir_all(&self.path);
    }

    pub fn worktree_report(&self) -> Option<WorktreeReport> {
        let text = fs::read_to_string(self.file("worktree.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// A new run waits until its worker takes a slot on the account, so it
    /// isn't counted against the account's runs before then.
    pub fn write_request(&self, record: &RunRecord, task: &str) -> Result<(), String> {
        self.renew_lease()?;
        self.mark_waiting()?;
        write_atomic(&self.file("task.txt"), task.as_bytes())?;
        self.write_record(record)
    }

    fn mark_waiting(&self) -> Result<(), String> {
        write_atomic(&self.file("waiting"), b"")
    }

    fn is_waiting(&self) -> bool {
        self.file("waiting").exists()
    }

    fn write_record(&self, record: &RunRecord) -> Result<(), String> {
        write_atomic(
            &self.file("run.json"),
            &serde_json::to_vec_pretty(record).expect("records serialize"),
        )
    }

    fn task(&self) -> Result<String, String> {
        fs::read_to_string(self.file("task.txt")).map_err(|e| format!("can't read the run's task: {e}"))
    }

    fn write_pid(&self, name: &str, pid: u32) -> Result<(), String> {
        write_atomic(&self.file(name), pid.to_string().as_bytes())
    }

    fn pid(&self, name: &str) -> Option<i32> {
        fs::read_to_string(self.file(name)).ok()?.trim().parse().ok()
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

    pub fn renew_lease(&self) -> Result<(), String> {
        write_atomic(&self.file("lease"), now().to_string().as_bytes())
    }

    fn lease_age(&self) -> Duration {
        fs::metadata(self.file("lease"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .unwrap_or(Duration::MAX)
    }

    fn cancel_reason(&self) -> Option<String> {
        fs::read_to_string(self.file("cancel")).ok()
    }

    pub fn state(&self) -> State {
        if let Some(outcome) = self.outcome() {
            return State::Finished(outcome);
        }
        match self.pid("pid") {
            Some(pid) if !is_alive(pid) => match self.outcome() {
                Some(outcome) => State::Finished(outcome),
                None => State::Lost,
            },
            // No pid yet means the starter hasn't recorded its worker yet.
            _ if self.is_waiting() => State::Waiting,
            _ => State::Running,
        }
    }

    /// Waits for the run to finish, renewing its lease meanwhile. Returns
    /// `Running` once `timeout` passes, or as soon as `interrupted` says so.
    pub fn wait(&self, timeout: Option<Duration>, interrupted: &dyn Fn() -> bool) -> State {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            let _ = self.renew_lease();
            let state = self.state();
            let going = matches!(state, State::Running | State::Waiting);
            if !going || interrupted() || deadline.is_some_and(|d| Instant::now() >= d) {
                return state;
            }
            std::thread::sleep(POLL);
        }
    }

    /// Asks the worker to stop the run, and waits for it to say it has. A run
    /// whose worker is already gone has its harness stopped here instead.
    pub fn cancel(&self, reason: &str) -> Result<State, String> {
        if let State::Finished(outcome) = self.state() {
            return Ok(State::Finished(outcome));
        }
        write_atomic(&self.file("cancel"), reason.as_bytes())?;
        let deadline = Instant::now() + TERM_GRACE * 3;
        while Instant::now() < deadline {
            match self.state() {
                State::Running | State::Waiting => std::thread::sleep(POLL),
                State::Lost => break,
                finished => return Ok(finished),
            }
        }
        if let Some(harness) = self.pid("harness.pid") {
            Tree::of(harness).signal(libc::SIGKILL);
        }
        let outcome = Outcome::Cancelled {
            reason: reason.to_string(),
        };
        self.write_outcome(&outcome)?;
        Ok(State::Finished(outcome))
    }
}

/// Starts the worker for a run in a session of its own, so it outlives the
/// shell call that started it.
pub fn spawn_worker(record: &RunRecord, dir: &RunDir) -> Result<Child, String> {
    let exe = std::env::current_exe().map_err(|e| format!("can't find the brigade binary: {e}"))?;
    let log = fs::File::create(dir.file("worker.log")).map_err(|e| format!("can't create the worker log: {e}"))?;
    let mut command = Command::new(exe);
    command
        .args(["__exec", &record.id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = command.spawn().map_err(|e| format!("can't start the worker: {e}"))?;
    dir.write_pid("pid", child.id())?;
    Ok(child)
}

/// The worker's job: takes a slot on the run's account, runs the harness CLI
/// until it exits, the run is cancelled or its lease lapses, and records the
/// outcome. A limited account, known beforehand or hit during the run, hands
/// the run to its fallback when it has one.
pub fn execute(
    settings: &Settings,
    record: RunRecord,
    dir: &RunDir,
    lease: Duration,
    var: &dyn Fn(&str) -> Option<String>,
) -> Outcome {
    let mut record = record;
    let task = dir.task();
    let outcome = match &task {
        Err(error) => Outcome::Failed { error: error.clone() },
        Ok(task) => loop {
            let outcome = match take_slot(settings, &record, dir, lease) {
                Slot::Taken => run_harness(settings, &record, dir, task, lease, var),
                Slot::Limited(limit, model) => Outcome::Limited {
                    error: limit.message,
                    ran: false,
                    until: limit.until,
                    retry_at: limit.until.is_none().then_some(limit.retry_at),
                    model,
                },
                Slot::Ended(outcome) => outcome,
            };
            match (&outcome, record.fallback.clone()) {
                (Outcome::Limited { error, .. }, Some(fallback)) => {
                    record = record.fall_back(fallback, error);
                    // Written before the fallback starts, so a waiter reports the agent that runs.
                    // The run gives up its slot and waits for one on the fallback's account.
                    if let Err(error) = dir.write_record(&record).and_then(|_| dir.mark_waiting()) {
                        break Outcome::Failed { error };
                    }
                }
                _ => break outcome,
            }
        },
    };
    let record = &record;
    if let Some(worktree) = &record.worktree {
        let state = match &outcome {
            Outcome::Done { .. } => worktree::commit(worktree, &commit_message(record, task.as_deref().unwrap_or(""))),
            Outcome::Failed { .. } | Outcome::Limited { .. } | Outcome::Cancelled { .. } => worktree::tidy(worktree),
        };
        let report = WorktreeReport {
            worktree: state,
            checkout_changed: worktree::checkout_change(worktree),
        };
        let _ = write_atomic(
            &dir.file("worktree.json"),
            &serde_json::to_vec_pretty(&report).expect("reports serialize"),
        );
    }
    // A `brigade cancel` that gave up on the worker may have recorded the
    // outcome already.
    if dir.outcome().is_none()
        && let Err(error) = dir.write_outcome(&outcome)
    {
        return Outcome::Failed { error };
    }
    outcome
}

enum Slot {
    Taken,
    /// The account, or the model alone when it names one, is limited and the
    /// run won't wait for it.
    Limited(Limit, Option<String>),
    Ended(Outcome),
}

/// Waits until the run's account can take it, unless the account is limited
/// and the run has a fallback to go to or wasn't asked to wait.
fn take_slot(settings: &Settings, record: &RunRecord, dir: &RunDir, lease: Duration) -> Slot {
    let mut next_look = Instant::now();
    loop {
        if let Some(reason) = stop_reason(dir, lease) {
            return Slot::Ended(Outcome::Cancelled { reason });
        }
        if Instant::now() >= next_look {
            match claim(settings, record, dir) {
                Ok(Claim::Taken) => return Slot::Taken,
                Ok(Claim::Limited(limit, model)) if !record.wait || record.fallback.is_some() => {
                    return Slot::Limited(limit, model);
                }
                Ok(_) => next_look = Instant::now() + SLOT_POLL,
                Err(error) => return Slot::Ended(Outcome::Failed { error }),
            }
        }
        std::thread::sleep(POLL);
    }
}

enum Claim {
    Taken,
    Limited(Limit, Option<String>),
    Full,
}

/// Takes one of the account's slots under its lock, so two runs can't both
/// take the last one.
fn claim(settings: &Settings, record: &RunRecord, dir: &RunDir) -> Result<Claim, String> {
    let account = &record.agent.account;
    let _lock = accounts::lock(settings, &account.name)?;
    let state = accounts::load(settings, &account.name);
    if let Some(limit) = state.limit_on(&record.model, now()) {
        let model = (state.limit.as_ref() != Some(limit)).then(|| record.model.clone());
        return Ok(Claim::Limited(limit.clone(), model));
    }
    if let Some(max) = account.max_runs {
        let running = list(settings)
            .into_iter()
            .filter(|run| run.record.agent.account.name == account.name && run.state == State::Running)
            .count();
        if running >= max as usize {
            return Ok(Claim::Full);
        }
    }
    fs::remove_file(dir.file("waiting")).map_err(|e| format!("can't take a slot: {e}"))?;
    Ok(Claim::Taken)
}

fn stop_reason(dir: &RunDir, lease: Duration) -> Option<String> {
    dir.cancel_reason()
        .or_else(|| (dir.lease_age() > lease).then(|| format!("nobody waited on it for {}s", lease.as_secs())))
}

/// A run as `brigade runs` and the account checks see it.
pub struct Run {
    pub record: RunRecord,
    pub dir: RunDir,
    pub state: State,
}

/// Every run brigade has a directory for, oldest first.
pub fn list(settings: &Settings) -> Vec<Run> {
    let Ok(entries) = fs::read_dir(settings.runs_dir()) else {
        return Vec::new();
    };
    let mut runs: Vec<Run> = entries
        .flatten()
        .filter_map(|entry| {
            let dir = RunDir { path: entry.path() };
            let record = dir.record().ok()?;
            let state = dir.state();
            Some(Run { record, dir, state })
        })
        .collect();
    runs.sort_by_key(|run| run.record.started_at);
    runs
}

fn run_harness(
    settings: &Settings,
    record: &RunRecord,
    dir: &RunDir,
    task: &str,
    lease: Duration,
    var: &dyn Fn(&str) -> Option<String>,
) -> Outcome {
    let last_message_file = dir.file("last-message.txt");
    let request = Request {
        agent: &record.agent,
        model: &record.model,
        effort: record.effort,
        access: record.access,
        cwd: &record.cwd,
        resume: record.resume.as_deref(),
        task,
        last_message_file: &last_message_file,
        worktree: record.worktree.as_ref(),
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
        .stderr(stderr)
        .process_group(0);
    #[cfg(target_os = "linux")]
    // SAFETY: prctl is async-signal-safe. The harness dies with the worker
    // rather than running on unwatched if the worker is killed.
    unsafe {
        command.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
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
    let _ = dir.write_pid("harness.pid", child.id());
    if let Some(mut stdin) = child.stdin.take() {
        let prompt = invocation.stdin.clone();
        // A harness that exits early closes the pipe; its exit status tells the story.
        std::thread::spawn(move || {
            let _ = stdin.write_all(prompt.as_bytes());
        });
    }

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                return Outcome::Failed {
                    error: format!("lost track of `{}`: {e}", invocation.program),
                };
            }
        }
        if let Some(reason) = stop_reason(dir, lease) {
            stop(&mut child);
            return Outcome::Cancelled { reason };
        }
        std::thread::sleep(POLL);
    };

    let stdout = fs::read_to_string(&stdout_path).unwrap_or_default();
    let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
    let last_message = fs::read_to_string(&last_message_file).ok();
    let usage = invocation::usage(&record.agent.harness, &stdout);
    let outcome = match invocation::parse(
        &record.agent.harness,
        &stdout,
        &stderr,
        last_message.as_deref(),
        status.success(),
    ) {
        Ok(reply) => Outcome::Done {
            session: reply.session,
            message: reply.message,
        },
        // The harness's own error already says it all; its stderr would repeat it.
        Err(ParseFailure::Limited(error)) => Outcome::Limited {
            error,
            ran: true,
            until: None,
            retry_at: None,
            model: None,
        },
        Err(ParseFailure::Reported(error)) => Outcome::Failed { error },
        Err(ParseFailure::NoResult(error)) if status.success() => Outcome::Failed { error },
        Err(ParseFailure::NoResult(error)) => {
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
    };
    note_account(settings, record, outcome, usage)
}

/// Records what the run showed about its account, and fills a limited
/// outcome in with when the account frees up.
fn note_account(settings: &Settings, record: &RunRecord, outcome: Outcome, usage: Option<Usage>) -> Outcome {
    let account = &record.agent.account.name;
    let now = now();
    let rejected = usage.as_ref().filter(|u| u.status == "rejected");
    let rejected_until = rejected.and_then(|u| u.resets_at);
    // Claude Code names the window it hit. Its five-hour and seven-day windows
    // cover the whole account; anything else, and every other harness's limit,
    // is taken to be the model's own.
    let account_wide = rejected
        .and_then(|u| u.window.as_deref())
        .is_some_and(|w| ACCOUNT_WINDOWS.contains(&w));
    let model = (!account_wide).then_some(record.model.as_str());
    if let Some(usage) = usage {
        let _ = accounts::record_usage(settings, account, Usage { at: now, ..usage });
    }
    match outcome {
        Outcome::Limited { error, ran, .. } => {
            let until = rejected_until.or_else(|| accounts::reset_in_message(&error));
            let _ = accounts::record_limit(settings, account, model, &error, until, now);
            let state = accounts::load(settings, account);
            let limit = state.limit_on(&record.model, now);
            Outcome::Limited {
                error,
                ran,
                until,
                retry_at: limit.filter(|l| l.until.is_none()).map(|l| l.retry_at),
                model: model.map(String::from),
            }
        }
        Outcome::Done { .. } => {
            let _ = accounts::record_success(settings, account, &record.model);
            outcome
        }
        outcome => outcome,
    }
}

fn commit_message(record: &RunRecord, task: &str) -> String {
    let summary: String = task.lines().next().unwrap_or("").chars().take(72).collect();
    format!("brigade: {} run {}\n\n{summary}\n", record.agent.name, record.id)
}

/// SIGTERM to everything the harness started, then SIGKILL to whatever is left.
fn stop(child: &mut Child) {
    let pid = child.id() as i32;
    let mut tree = Tree::of(pid);
    tree.signal(libc::SIGTERM);
    let deadline = Instant::now() + TERM_GRACE;
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Whatever the harness started while it was shutting down.
    tree.merge(Tree::of(pid));
    tree.signal(libc::SIGKILL);
    let _ = child.wait();
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

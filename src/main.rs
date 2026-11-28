use brigade::accounts;
use brigade::capacity;
use brigade::harness::{Access, Effort};
use brigade::install::{self, Change};
use brigade::invocation::{UnsetVar, expand};
use brigade::process::Lineage;
use brigade::report::{self, WAIT_SLICE_SECS};
use brigade::roster::{Agent, Roster};
use brigade::runs::{self, Outcome, RunDir, RunRecord, State};
use brigade::settings::{MAX_DEPTH, Settings};
use brigade::worktree;
use clap::{Args, Parser, Subcommand};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const STILL_RUNNING: u8 = 3;
const LIMITED: u8 = 4;
const INTERRUPTED_EXIT: u8 = 130;
const ORPHAN_CHECK_EVERY: Duration = Duration::from_secs(2);

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Tag in named agents (sol, neon, opus...) that run in their own CLI.
#[derive(Parser)]
#[command(name = "brigade", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the roster: the built-in agents plus your own from ~/.config/brigade/roster.toml
    List {
        #[arg(long)]
        json: bool,
    },
    /// Hand a task to a roster agent and print its reply
    Run(RunArgs),
    /// Wait for a detached run and print its reply
    Wait {
        run: String,
        /// Give up after this many seconds and report the run as still running
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    /// Stop a run and everything its harness started
    Cancel {
        run: String,
        #[arg(long)]
        json: bool,
    },
    /// Show which accounts can take runs now: limits and when they reset, and runs going
    Capacity {
        /// Forget the limit on this account, e.g. once agy credits are topped up
        #[arg(long, value_name = "ACCOUNT")]
        clear: Option<String>,
        /// Read every account's quotas from its harness first. Free, but takes a few seconds
        #[arg(long, conflicts_with = "clear")]
        refresh: bool,
        #[arg(long)]
        json: bool,
    },
    /// List runs, oldest first
    Runs {
        /// Only runs in this state
        #[arg(long, value_parser = ["waiting", "running", "done", "failed", "limited", "cancelled", "lost"])]
        state: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Give each agent from your own roster a subagent of its own in Claude Code, Codex and agy. Run it again
    /// after changing the roster or upgrading brigade
    InstallAgents,
    #[command(name = "__exec", hide = true)]
    Exec { run: String },
}

#[derive(Args)]
struct RunArgs {
    /// A roster agent, e.g. sol or neon (see `brigade list`)
    agent: String,
    /// Model to use instead of the agent's own
    #[arg(long)]
    model: Option<String>,
    /// Reasoning effort; defaults to the agent's roster effort, or else the harness's own default
    #[arg(long, value_enum)]
    effort: Option<Effort>,
    /// Let the agent change files; runs are read-only otherwise
    #[arg(long)]
    write: bool,
    /// Directory to run in; defaults to the current one
    #[arg(long, value_name = "DIR")]
    cwd: Option<PathBuf>,
    /// Run in a new git worktree on its own branch, made from the HEAD of the cwd's repo. Needs --write;
    /// brigade commits the run's changes on that branch when it finishes
    #[arg(long, conflicts_with = "resume", requires = "write")]
    worktree: bool,
    /// Continue the session a previous run printed
    #[arg(long, value_name = "SESSION")]
    resume: Option<String>,
    /// Run on this account's route alone, when more than one account offers the agent
    #[arg(long, value_name = "ACCOUNT")]
    account: Option<String>,
    /// Agent to hand the task to if this one hits its harness's usage limit, in place of the roster's fallback
    #[arg(long, value_name = "AGENT", conflicts_with_all = ["resume", "no_fallback"])]
    fallback: Option<String>,
    /// Don't switch to the roster's fallback agent on a usage limit
    #[arg(long)]
    no_fallback: bool,
    /// If the agent's account is limited, wait for it to free up instead of ending as limited
    #[arg(long)]
    wait: bool,
    /// Start the run in the background, print its id and return. It's cancelled if nobody waits on it for 90s
    #[arg(long)]
    detach: bool,
    #[arg(long)]
    json: bool,
    /// The task; read from stdin when omitted or `-`
    task: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli.command) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("brigade: {error}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(command: Command) -> Result<ExitCode, String> {
    let settings = Settings::from_env(env_var)?;
    match command {
        Command::List { json } => {
            let roster = Roster::load(&settings.roster_file).map_err(|e| e.to_string())?;
            // What each switching server last said it had loaded (`capacity --refresh`).
            let serving: BTreeMap<String, Vec<String>> = roster
                .accounts()
                .iter()
                .filter(|account| account.models_url.is_some())
                .map(|account| (account.name.clone(), accounts::load(&settings, &account.name).serving))
                .filter(|(_, models)| !models.is_empty())
                .collect();
            if json {
                let routes: Vec<serde_json::Value> = roster
                    .entries()
                    .iter()
                    .map(|entry| {
                        let mut route = serde_json::to_value(entry).expect("entries serialize");
                        if let Some(models) = serving.get(&entry.agent.account.name) {
                            route["serving"] = serde_json::json!(models);
                        }
                        if let Some(model) = report::served_model(&roster, &serving, &entry.agent.name) {
                            route["served_model"] = serde_json::json!(model);
                        }
                        route
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&routes).expect("routes serialize"));
            } else {
                print!("{}", report::roster(&roster, &serving, &settings.roster_file));
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Run(args) => run(&settings, args),
        Command::Wait { run, timeout, json } => {
            let (_, dir) = RunDir::open(&settings, &run)?;
            let state = dir.wait(timeout.map(Duration::from_secs), &orphan_check());
            print_state(&settings, &dir, &state, json, timeout.unwrap_or(WAIT_SLICE_SECS))
        }
        Command::Cancel { run, json } => {
            let (_, dir) = RunDir::open(&settings, &run)?;
            let state = dir.cancel("cancelled with brigade cancel")?;
            let _ = print_state(&settings, &dir, &state, json, 0)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Capacity { clear, refresh, json } => {
            if let Some(account) = clear {
                let roster = Roster::load(&settings.roster_file).map_err(|e| e.to_string())?;
                let known = roster.accounts().iter().any(|a| a.name == account)
                    || accounts::known(&settings).contains(&account);
                if !known {
                    return Err(format!("there's no {account} account"));
                }
                let had = accounts::clear(&settings, &account)?;
                println!(
                    "{}",
                    if had {
                        format!("forgot the limit on the {account} account")
                    } else {
                        format!("the {account} account had no limit to forget")
                    }
                );
                return Ok(ExitCode::SUCCESS);
            }
            let roster = Roster::load(&settings.roster_file).map_err(|e| e.to_string())?;
            if refresh {
                for (account, error) in capacity::refresh(&settings, &roster, runs::now()) {
                    eprintln!("brigade: couldn't read the {account} account's usage: {error}");
                }
            }
            let accounts = capacity::capacity(&settings, &roster, runs::now(), &env_var);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&accounts).expect("capacity serializes")
                );
            } else {
                print!("{}", report::capacity(&accounts, runs::now()));
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Runs { state, json } => {
            let runs: Vec<_> = runs::list(&settings)
                .into_iter()
                .filter(|run| state.as_deref().is_none_or(|s| report::state_name(&run.state) == s))
                .collect();
            if json {
                let rows: Vec<_> = runs
                    .iter()
                    .map(|run| {
                        serde_json::json!({
                            "id": run.record.id,
                            "agent": run.record.agent.name,
                            "account": run.record.agent.account.name,
                            "state": report::state_name(&run.state),
                            "started_at": run.record.started_at,
                            "cwd": run.record.cwd,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&rows).expect("runs serialize"));
            } else {
                print!("{}", report::runs(&runs, runs::now()));
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::InstallAgents => {
            let roster = Roster::load(&settings.roster_file).map_err(|e| e.to_string())?;
            let changes = install::install(&roster, &settings, &env_var)?;
            if changes.is_empty() {
                println!("the subagents for your roster's agents are up to date");
            }
            for change in changes {
                match change {
                    Change::Wrote(path) => println!("wrote {}", path.display()),
                    Change::Removed(path) => println!("removed {}", path.display()),
                    Change::NotOurs(path) => {
                        eprintln!("left {} alone: brigade didn't write it", path.display())
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Exec { run } => {
            let (record, dir) = RunDir::open(&settings, &run)?;
            if !matches!(dir.state(), State::Finished(_)) {
                runs::execute(&settings, record, &dir, settings.lease, &env_var);
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn run(settings: &Settings, args: RunArgs) -> Result<ExitCode, String> {
    if settings.depth >= MAX_DEPTH {
        return Err(format!(
            "this is already a depth-{} brigade worker, and runs nest at most {MAX_DEPTH} deep",
            settings.depth
        ));
    }
    let roster = Roster::load(&settings.roster_file).map_err(|e| e.to_string())?;
    let routes = roster.routes(&args.agent);
    let first = routes.first().ok_or_else(|| {
        format!(
            "no agent `{}` in the roster; try one of: {}",
            args.agent,
            roster.names().join(", ")
        )
    })?;
    let fallback_name = first.fallback.clone();
    let mut routes: Vec<Agent> = routes.iter().map(|e| e.agent.clone()).collect();
    if let Some(account) = &args.account {
        routes.retain(|agent| &agent.account.name == account);
        if routes.is_empty() {
            return Err(format!("the {account} account doesn't offer {}", args.agent));
        }
    }
    if args.resume.is_some() && args.account.is_none() && routes.len() > 1 {
        let accounts: Vec<&str> = routes.iter().map(|a| a.account.name.as_str()).collect();
        return Err(format!(
            "{} is offered by the {} accounts; say which one the session is on with --account",
            args.agent,
            accounts.join(" and ")
        ));
    }
    // A chosen model, or a session to continue, belongs to one route's harness.
    if args.model.is_some() || args.resume.is_some() {
        routes.truncate(1);
    }
    if let Some(effort) = args.effort {
        let takers: Vec<Agent> = routes.iter().filter(|a| a.supports(effort)).cloned().collect();
        if takers.is_empty() {
            let agent = &routes[0];
            return Err(format!(
                "{} runs in {}, which takes {}",
                agent.name,
                agent.harness,
                agent.effort_levels()
            ));
        }
        routes = takers;
    }
    let mut routes = routes.into_iter().map(expanded).collect::<Result<Vec<_>, _>>()?;
    let agent = routes.remove(0);
    let effort = args.effort.or(agent.effort);
    if args.resume.is_some() && !agent.harness.can_resume() {
        return Err(format!(
            "{} runs in {}, which can't continue a session: its harness has no resume_args",
            agent.name, agent.harness
        ));
    }
    // A resumed session lives in its own harness, where no other agent can pick it up.
    let fallback = match (&args.fallback, args.no_fallback || args.resume.is_some()) {
        (Some(name), _) => Some(name.clone()),
        (None, false) => fallback_name,
        (None, true) => None,
    };
    if let Some(name) = &fallback {
        if name == &agent.name {
            return Err(format!("{name} can't fall back to itself"));
        }
        let fallback_routes = roster.routes(name);
        if fallback_routes.is_empty() {
            return Err(format!(
                "no agent `{name}` to fall back to; try one of: {}",
                roster.names().join(", ")
            ));
        }
        for entry in fallback_routes {
            routes.push(expanded(entry.agent.clone())?);
        }
    }
    let task = read_task(&args.task)?;
    let cwd = args
        .cwd
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
        .map_err(|e| format!("no cwd: {e}"))?;
    let cwd = cwd
        .canonicalize()
        .map_err(|e| format!("can't use {} as the cwd: {e}", cwd.display()))?;

    let (id, dir) = RunDir::allocate(settings, &agent.name)?;
    let (worktree, cwd) = if args.worktree {
        match worktree::create(settings, &cwd, &id) {
            Ok((worktree, run_cwd)) => (Some(worktree), run_cwd),
            Err(error) => {
                dir.discard();
                return Err(error);
            }
        }
    } else {
        (worktree::existing(settings, &cwd), cwd)
    };
    let record = RunRecord {
        id,
        cwd,
        model: args.model.unwrap_or_else(|| agent.model.clone()),
        agent,
        effort,
        access: if args.write { Access::Write } else { Access::ReadOnly },
        worktree,
        resume: args.resume,
        depth: settings.depth + 1,
        started_at: runs::now(),
        next: routes,
        asked_effort: args.effort,
        fell_back_from: Vec::new(),
        waited: None,
        wait: args.wait,
    };
    dir.write_request(&record, &task)?;

    if args.detach {
        runs::spawn_worker(&record, &dir)?;
        if args.json {
            print!("{}", report::state_json(&record, &State::Running, dir.path(), None));
        } else {
            print!("{}", report::started(&record));
        }
        return Ok(ExitCode::SUCCESS);
    }

    trap_interrupt();
    let mut worker = runs::spawn_worker(&record, &dir)?;
    // Reaped here so a worker that dies shows up as gone rather than as a zombie.
    std::thread::spawn(move || worker.wait());
    let orphaned = orphan_check();
    let state = match dir.wait(None, &|| INTERRUPTED.load(Ordering::SeqCst) || orphaned()) {
        State::Running if INTERRUPTED.load(Ordering::SeqCst) => dir.cancel("interrupted")?,
        state => state,
    };
    let code = print_state(settings, &dir, &state, args.json, 0)?;
    Ok(if INTERRUPTED.load(Ordering::SeqCst) {
        ExitCode::from(INTERRUPTED_EXIT)
    } else {
        code
    })
}

/// The route with its model's variables expanded, so a missing variable is
/// reported before anything starts and the record names the model the harness
/// really gets.
fn expanded(mut agent: Agent) -> Result<Agent, String> {
    agent.model = expand(&agent.model, &env_var)
        .map_err(|UnsetVar(name)| format!("`{}` needs ${name}, which isn't set", agent.name))?;
    Ok(agent)
}

/// What a waiting run waits for, as `brigade wait` explains it.
fn waiting_for(settings: &Settings, record: &RunRecord) -> String {
    let account = &record.agent.account;
    match accounts::load(settings, &account.name).limit_on(&record.model, runs::now()) {
        Some((limit, _)) => match limit.until {
            Some(until) => format!("which is limited until {}", accounts::local_time(until)),
            None => format!(
                "which is limited, and brigade tries it again after {}",
                accounts::local_time(limit.retry_at)
            ),
        },
        _ => match account.max_runs {
            Some(max) => format!("which is already running as many as it takes ({max})"),
            None => "for a slot".to_string(),
        },
    }
}

/// True once whoever started this process is gone. A waiter whose forwarder
/// was killed outright lives on as an orphan, and must stop renewing the lease
/// so the run is cancelled. It doesn't cancel by itself: a harness's shell
/// timeout kills the waiter's callers too, and the forwarder then waits again.
fn orphan_check() -> impl Fn() -> bool {
    let lineage = Lineage::of_self();
    let checked = Cell::new(Instant::now());
    let broken = Cell::new(false);
    move || {
        // ps is a process per call; every couple of seconds is plenty.
        if !broken.get() && checked.get().elapsed() >= ORPHAN_CHECK_EVERY {
            checked.set(Instant::now());
            broken.set(lineage.is_broken());
        }
        broken.get()
    }
}

extern "C" fn on_interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Ctrl-C on a foreground run cancels it. The worker sits in a session of its
/// own, out of reach of the terminal's signal, so brigade passes it on.
fn trap_interrupt() {
    let handler: extern "C" fn(libc::c_int) = on_interrupt;
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGINT, handler as libc::sighandler_t);
    }
}

/// Reads the run afresh, since a run that fell back now names its fallback agent.
fn print_state(settings: &Settings, dir: &RunDir, state: &State, json: bool, waited: u64) -> Result<ExitCode, String> {
    let record = &dir.record()?;
    if json {
        print!(
            "{}",
            report::state_json(record, state, dir.path(), dir.worktree_report().as_ref())
        );
    }
    Ok(match state {
        State::Finished(Outcome::Done { session, message }) => {
            if !json {
                print!(
                    "{}",
                    report::done(record, session.as_deref(), message, dir.worktree_report().as_ref())
                );
            }
            ExitCode::SUCCESS
        }
        State::Finished(Outcome::Failed { error }) => {
            if !json {
                eprint!(
                    "{}",
                    report::failed(record, error, dir.path(), dir.worktree_report().as_ref())
                );
            }
            ExitCode::FAILURE
        }
        State::Finished(outcome @ Outcome::Limited { .. }) => {
            if !json {
                eprint!(
                    "{}",
                    report::limited(record, outcome, dir.path(), dir.worktree_report().as_ref())
                );
            }
            ExitCode::from(LIMITED)
        }
        State::Finished(Outcome::Cancelled { reason }) => {
            if !json {
                eprint!(
                    "{}",
                    report::cancelled(record, reason, dir.path(), dir.worktree_report().as_ref())
                );
            }
            ExitCode::FAILURE
        }
        State::Running => {
            if !json {
                print!("{}", report::still_running(record, waited));
            }
            ExitCode::from(STILL_RUNNING)
        }
        State::Waiting => {
            if !json {
                print!(
                    "{}",
                    report::still_waiting(record, &waiting_for(settings, record), waited)
                );
            }
            ExitCode::from(STILL_RUNNING)
        }
        State::Lost => {
            if !json {
                eprint!("{}", report::lost(record, dir.path()));
            }
            ExitCode::FAILURE
        }
    })
}

fn read_task(words: &[String]) -> Result<String, String> {
    let task = if words.is_empty() || words == ["-"] {
        let mut stdin = std::io::stdin();
        if stdin.is_terminal() {
            return Err("give the task as arguments or on stdin".into());
        }
        let mut task = String::new();
        stdin
            .read_to_string(&mut task)
            .map_err(|e| format!("can't read the task from stdin: {e}"))?;
        task
    } else {
        words.join(" ")
    };
    let task = task.trim().to_string();
    if task.is_empty() {
        return Err("the task is empty".into());
    }
    Ok(task)
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

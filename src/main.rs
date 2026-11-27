use brigade::harness::{Access, Effort};
use brigade::invocation::{UnsetVar, expand};
use brigade::process::Lineage;
use brigade::report::{self, WAIT_SLICE_SECS};
use brigade::roster::Roster;
use brigade::runs::{self, Outcome, RunDir, RunRecord, State};
use brigade::settings::{MAX_DEPTH, Settings};
use brigade::worktree;
use clap::{Args, Parser, Subcommand};
use std::cell::Cell;
use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const STILL_RUNNING: u8 = 3;
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
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(roster.entries()).expect("entries serialize")
                );
            } else {
                print!("{}", report::roster(roster.entries(), &settings.roster_file));
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Run(args) => run(&settings, args),
        Command::Wait { run, timeout, json } => {
            let (record, dir) = RunDir::open(&settings, &run)?;
            let state = dir.wait(timeout.map(Duration::from_secs), &orphan_check());
            Ok(print_state(
                &record,
                &dir,
                &state,
                json,
                timeout.unwrap_or(WAIT_SLICE_SECS),
            ))
        }
        Command::Cancel { run, json } => {
            let (record, dir) = RunDir::open(&settings, &run)?;
            let state = dir.cancel("cancelled with brigade cancel")?;
            let _ = print_state(&record, &dir, &state, json, 0);
            Ok(ExitCode::SUCCESS)
        }
        Command::Exec { run } => {
            let (record, dir) = RunDir::open(&settings, &run)?;
            if !matches!(dir.state(), State::Finished(_)) {
                runs::execute(&record, &dir, settings.lease, &env_var);
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
    let entry = roster.get(&args.agent).ok_or_else(|| {
        format!(
            "no agent `{}` in the roster; try one of: {}",
            args.agent,
            roster.names().join(", ")
        )
    })?;
    let mut agent = entry.agent.clone();
    // Expanded here so a missing variable is reported before anything starts,
    // and the run's record names the model the harness really gets.
    agent.model = expand(&agent.model, &env_var)
        .map_err(|UnsetVar(name)| format!("`{}` needs ${name}, which isn't set", agent.name))?;
    let effort = args.effort.or(agent.effort);
    if effort.is_some_and(|e| !agent.harness.supports(e)) {
        let levels: Vec<&str> = agent.harness.efforts().iter().map(|e| e.as_str()).collect();
        return Err(format!(
            "{} runs in {}, which takes effort {}",
            agent.name,
            agent.harness,
            levels.join(", ")
        ));
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
    let code = print_state(&record, &dir, &state, args.json, 0);
    Ok(if INTERRUPTED.load(Ordering::SeqCst) {
        ExitCode::from(INTERRUPTED_EXIT)
    } else {
        code
    })
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

fn print_state(record: &RunRecord, dir: &RunDir, state: &State, json: bool, waited: u64) -> ExitCode {
    if json {
        print!(
            "{}",
            report::state_json(record, state, dir.path(), dir.worktree_state().as_ref())
        );
    }
    match state {
        State::Finished(Outcome::Done { session, message }) => {
            if !json {
                print!(
                    "{}",
                    report::done(record, session.as_deref(), message, dir.worktree_state().as_ref())
                );
            }
            ExitCode::SUCCESS
        }
        State::Finished(Outcome::Failed { error }) => {
            if !json {
                eprint!(
                    "{}",
                    report::failed(record, error, dir.path(), dir.worktree_state().as_ref())
                );
            }
            ExitCode::FAILURE
        }
        State::Finished(Outcome::Cancelled { reason }) => {
            if !json {
                eprint!(
                    "{}",
                    report::cancelled(record, reason, dir.path(), dir.worktree_state().as_ref())
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
        State::Lost => {
            if !json {
                eprint!("{}", report::lost(record, dir.path()));
            }
            ExitCode::FAILURE
        }
    }
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

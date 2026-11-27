use brigade::harness::{Access, Effort};
use brigade::report::{self, WAIT_SLICE_SECS};
use brigade::roster::Roster;
use brigade::runs::{self, Outcome, RunDir, RunRecord, State};
use brigade::settings::{MAX_DEPTH, Settings};
use brigade::worktree;
use clap::{Args, Parser, Subcommand};
use std::io::{IsTerminal, Read};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::time::Duration;

const STILL_RUNNING: u8 = 3;

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
    #[command(name = "__exec", hide = true)]
    Exec { run: String },
}

#[derive(Args)]
struct RunArgs {
    /// A roster agent, e.g. sol or neon (see `brigade list`)
    agent: String,
    /// Reasoning effort; defaults to the agent's roster effort
    #[arg(long, value_enum)]
    effort: Option<Effort>,
    /// Let the agent change files; runs are read-only otherwise
    #[arg(long)]
    write: bool,
    /// Directory to run in; defaults to the current one
    #[arg(long, value_name = "DIR")]
    cwd: Option<PathBuf>,
    /// Run in a new git worktree on its own branch, made from the HEAD of the cwd's repo
    #[arg(long, conflicts_with = "resume")]
    worktree: bool,
    /// Continue the session a previous run printed
    #[arg(long, value_name = "SESSION")]
    resume: Option<String>,
    /// Start the run in the background, print its id and return
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
            let state = dir.wait(timeout.map(Duration::from_secs));
            Ok(print_state(
                &record,
                &dir,
                &state,
                json,
                timeout.unwrap_or(WAIT_SLICE_SECS),
            ))
        }
        Command::Exec { run } => {
            let (record, dir) = RunDir::open(&settings, &run)?;
            if !matches!(dir.state(), State::Finished(_)) {
                runs::execute(&record, &dir, &env_var);
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
    let agent = entry.agent.clone();
    let effort = args.effort.unwrap_or(agent.effort);
    if !agent.harness.supports(effort) {
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
    let worktree = if args.worktree {
        Some(worktree::create(settings, &cwd, &id)?)
    } else {
        None
    };
    let record = RunRecord {
        id,
        cwd: worktree.as_ref().map(|w| w.path.clone()).unwrap_or(cwd),
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
        spawn_worker(&record, &dir)?;
        if args.json {
            print!("{}", report::state_json(&record, &State::Running, dir.path()));
        } else {
            print!("{}", report::started(&record));
        }
        return Ok(ExitCode::SUCCESS);
    }
    dir.write_pid(std::process::id())?;
    let outcome = runs::execute(&record, &dir, &env_var);
    Ok(print_state(&record, &dir, &State::Finished(outcome), args.json, 0))
}

fn spawn_worker(record: &RunRecord, dir: &RunDir) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("can't find the brigade binary: {e}"))?;
    let log = std::fs::File::create(dir.file("worker.log")).map_err(|e| format!("can't create the worker log: {e}"))?;
    let mut command = std::process::Command::new(exe);
    command
        .args(["__exec", &record.id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    // SAFETY: setsid is async-signal-safe. A new session keeps the worker alive
    // when the shell that started it exits, as a harness's shell call does.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = command.spawn().map_err(|e| format!("can't start the worker: {e}"))?;
    dir.write_pid(child.id())
}

fn print_state(record: &RunRecord, dir: &RunDir, state: &State, json: bool, waited: u64) -> ExitCode {
    if json {
        print!("{}", report::state_json(record, state, dir.path()));
    }
    match state {
        State::Finished(Outcome::Done { session, message }) => {
            if !json {
                print!("{}", report::done(record, session.as_deref(), message));
            }
            ExitCode::SUCCESS
        }
        State::Finished(Outcome::Failed { error }) => {
            if !json {
                eprint!("{}", report::failed(record, error, dir.path()));
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

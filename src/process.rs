//! Process trees and lineages, read from `ps` so the same code works on Linux
//! and macOS.
//!
//! Stopping a harness means stopping everything it started. Codex runs each
//! tool command in a session of its own, out of reach of a signal to the
//! harness's process group, and a command is reparented away from the harness
//! the moment the harness exits. So the process tree is listed before any
//! signal goes out, and every process and process group in it is signalled.

use std::collections::{BTreeSet, HashMap};
use std::process::Command;

#[derive(Debug, Default)]
pub struct Tree {
    pids: BTreeSet<i32>,
    groups: BTreeSet<i32>,
    /// Each process's start time as `ps` gives it, which tells a process apart
    /// from a later one that reused its pid.
    starts: HashMap<i32, String>,
}

impl Tree {
    /// `root` and its descendants as `ps` sees them now.
    pub fn of(root: i32) -> Tree {
        let table = process_table();
        let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
        let mut group_of = HashMap::new();
        let mut start_of = HashMap::new();
        for row in &table {
            children.entry(row.ppid).or_default().push(row.pid);
            group_of.insert(row.pid, row.pgid);
            start_of.insert(row.pid, row.start.clone());
        }
        let mut tree = Tree::default();
        let mut pending = vec![root];
        while let Some(pid) = pending.pop() {
            if !tree.pids.insert(pid) {
                continue;
            }
            if let Some(&pgid) = group_of.get(&pid) {
                tree.groups.insert(pgid);
            }
            if let Some(start) = start_of.get(&pid) {
                tree.starts.insert(pid, start.clone());
            }
            pending.extend(children.get(&pid).into_iter().flatten());
        }
        // The root's group always counts, even once the root has exited.
        tree.groups.insert(root);
        tree
    }

    pub fn merge(&mut self, other: Tree) {
        self.pids.extend(other.pids);
        self.groups.extend(other.groups);
        self.starts.extend(other.starts);
    }

    /// The tree's processes with their start times, to save and stop later.
    pub fn saved(&self) -> Vec<(i32, String)> {
        let mut saved: Vec<(i32, String)> = self.starts.iter().map(|(&pid, start)| (pid, start.clone())).collect();
        saved.sort();
        saved
    }

    /// SIGKILLs process group `group`, the harness's own, unless its pid now
    /// belongs to a process other than the saved one (a pid isn't reused while
    /// its group has members). Returns how many processes the group had.
    pub fn kill_saved_group(saved: &[(i32, String)], group: i32) -> usize {
        let table = process_table();
        let reused = table
            .iter()
            .find(|row| row.pid == group)
            .is_some_and(|row| !saved.iter().any(|(pid, start)| *pid == group && *start == row.start));
        let members = table.iter().filter(|row| row.pgid == group).count();
        if reused || members == 0 {
            return 0;
        }
        // SAFETY: killpg only sends a signal, to a group just checked to be the harness's.
        unsafe {
            libc::killpg(group, libc::SIGKILL);
        }
        members
    }

    /// SIGKILLs the processes in a saved tree that are still the same
    /// processes, and returns how many it found.
    pub fn kill_saved(saved: &[(i32, String)]) -> usize {
        let live: HashMap<i32, String> = process_table().into_iter().map(|row| (row.pid, row.start)).collect();
        let mut killed = 0;
        for (pid, start) in saved {
            if live.get(pid) == Some(start) {
                // SAFETY: kill only sends a signal, to a process just checked
                // to be the one saved.
                unsafe {
                    libc::kill(*pid, libc::SIGKILL);
                }
                killed += 1;
            }
        }
        killed
    }

    pub fn signal(&self, signal: libc::c_int) {
        // SAFETY: kill and killpg only send signals; a process that is already
        // gone just fails with ESRCH.
        unsafe {
            for &group in &self.groups {
                libc::killpg(group, signal);
            }
            for &pid in &self.pids {
                libc::kill(pid, signal);
            }
        }
    }
}

/// Whoever started this process, and whoever started them, as (pid, parent)
/// links. A waiter compares it against the live table: when a link breaks, the
/// caller is gone even if the shell in between survived. Claude Code runs Bash
/// through a process of its own, so a killed host can leave the shell alive and
/// reparented while the waiter's own parent never changes.
#[derive(Debug)]
pub struct Lineage {
    links: Vec<(i32, i32)>,
}

const LINEAGE_DEPTH: usize = 3;

impl Lineage {
    pub fn of_self() -> Lineage {
        let parents: HashMap<i32, i32> = process_table().into_iter().map(|row| (row.pid, row.ppid)).collect();
        let mut links = Vec::new();
        let mut pid = std::process::id() as i32;
        while links.len() < LINEAGE_DEPTH {
            match parents.get(&pid) {
                Some(&ppid) if ppid > 1 => {
                    links.push((pid, ppid));
                    pid = ppid;
                }
                _ => break,
            }
        }
        Lineage { links }
    }

    pub fn is_broken(&self) -> bool {
        let parents: HashMap<i32, i32> = process_table().into_iter().map(|row| (row.pid, row.ppid)).collect();
        self.links.iter().any(|(pid, ppid)| parents.get(pid) != Some(ppid))
    }
}

struct Row {
    pid: i32,
    ppid: i32,
    pgid: i32,
    start: String,
}

/// Whether this process is inside Codex's Linux sandbox, which runs each
/// command in a pid namespace of its own with `codex-linux-sandbox` as its
/// process 1. Everything in the namespace ends with that command. A command the
/// user approved to run unsandboxed isn't in one, though Codex still sets its
/// sandbox variables for it.
pub fn in_codex_linux_sandbox() -> bool {
    std::fs::read("/proc/1/cmdline").is_ok_and(|cmdline| {
        let program = cmdline.split(|&b| b == 0).next().unwrap_or_default();
        program.rsplit(|&b| b == b'/').next() == Some(b"codex-linux-sandbox".as_slice())
    })
}

fn process_table() -> Vec<Row> {
    let Ok(output) = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,pgid=,lstart="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let mut number = || fields.next()?.parse::<i32>().ok();
            let (pid, ppid, pgid) = (number()?, number()?, number()?);
            let start = fields.collect::<Vec<_>>().join(" ");
            Some(Row { pid, ppid, pgid, start })
        })
        .collect()
}

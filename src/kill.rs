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
}

impl Tree {
    /// `root` and its descendants as `ps` sees them now.
    pub fn of(root: i32) -> Tree {
        let table = process_table();
        let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
        let mut group_of = HashMap::new();
        for &(pid, ppid, pgid) in &table {
            children.entry(ppid).or_default().push(pid);
            group_of.insert(pid, pgid);
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
            pending.extend(children.get(&pid).into_iter().flatten());
        }
        // The root's group always counts, even once the root has exited.
        tree.groups.insert(root);
        tree
    }

    pub fn merge(&mut self, other: Tree) {
        self.pids.extend(other.pids);
        self.groups.extend(other.groups);
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

fn process_table() -> Vec<(i32, i32, i32)> {
    let Ok(output) = Command::new("ps").args(["-A", "-o", "pid=,ppid=,pgid="]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace().map(|f| f.parse::<i32>().ok());
            Some((fields.next()??, fields.next()??, fields.next()??))
        })
        .collect()
}

//! Installs forwarders for the user's own agents into each harness's user-level
//! agents directory, which the published plugin can't do: it doesn't know the
//! user's roster.

use crate::harness::Harness;
use crate::render::{self, HOSTS, INSTALLED_MARK};
use crate::roster::Roster;
use crate::settings::Settings;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub enum Change {
    Wrote(PathBuf),
    /// A forwarder for an agent that's no longer in the roster.
    Removed(PathBuf),
    /// A file with a forwarder's name that brigade didn't write, left alone.
    NotOurs(PathBuf),
}

/// Writes a forwarder for every user agent into each harness, rewrites the ones
/// that changed and removes the ones whose agent is gone.
pub fn install(
    roster: &Roster,
    settings: &Settings,
    var: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<Change>, String> {
    let forwarders = render::installed_forwarders(roster);
    let hosts = HOSTS.into_iter().chain(
        roster
            .harnesses()
            .iter()
            .filter(|h| h.host.is_some())
            .map(|h| Harness::Custom(Box::new(h.clone()))),
    );
    let mut changes = Vec::new();
    for host in hosts {
        let dir = settings.agents_dir(&host, var)?;
        let wanted: BTreeMap<&str, &str> = forwarders
            .iter()
            .filter(|f| f.host.name() == host.name())
            .map(|f| (f.file.as_str(), f.contents.as_str()))
            .collect();
        for path in installed(&dir) {
            let file = path.file_name().map(|f| f.to_string_lossy().into_owned());
            if !file.is_some_and(|f| wanted.contains_key(f.as_str())) {
                fs::remove_file(&path).map_err(|e| format!("can't remove {}: {e}", path.display()))?;
                changes.push(Change::Removed(path));
            }
        }
        for (file, contents) in wanted {
            let path = dir.join(file);
            match fs::read_to_string(&path) {
                Ok(existing) if existing == contents => {}
                Ok(existing) if !existing.contains(INSTALLED_MARK) => changes.push(Change::NotOurs(path)),
                _ => {
                    fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
                    fs::write(&path, contents).map_err(|e| format!("can't write {}: {e}", path.display()))?;
                    changes.push(Change::Wrote(path));
                }
            }
        }
    }
    Ok(changes)
}

/// The forwarders brigade wrote in `dir` earlier.
fn installed(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && fs::read_to_string(path).is_ok_and(|text| text.contains(INSTALLED_MARK)))
        .collect()
}

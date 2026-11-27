use brigade::render::{GENERATED_DIRS, SKILL_FILE, plugin_files};
use brigade::roster::Roster;
use std::path::{Path, PathBuf};

#[test]
fn committed_plugin_files_match_the_roster() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let skill = std::fs::read_to_string(root.join(SKILL_FILE)).unwrap();
    let files = plugin_files(&Roster::built_in(), env!("CARGO_PKG_VERSION"), &skill).unwrap();
    let mut stale: Vec<String> = files
        .iter()
        .filter(|(path, contents)| std::fs::read_to_string(root.join(path)).ok().as_deref() != Some(contents))
        .map(|(path, _)| path.display().to_string())
        .collect();
    for dir in GENERATED_DIRS {
        for entry in std::fs::read_dir(root.join(dir)).unwrap().flatten() {
            let rel: PathBuf = Path::new(dir).join(entry.file_name());
            if !files.iter().any(|(path, _)| *path == rel) {
                stale.push(format!("{} (no longer generated)", rel.display()));
            }
        }
    }
    assert!(
        stale.is_empty(),
        "out of date, run `cargo run --example generate`: {stale:?}"
    );
}

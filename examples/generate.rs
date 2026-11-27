//! Rewrites the plugin's generated files from roster.toml.

use brigade::render::{GENERATED_DIRS, SKILL_FILE, plugin_files};
use brigade::roster::Roster;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let skill = std::fs::read_to_string(root.join(SKILL_FILE)).expect("the skill template is readable");
    let files = plugin_files(&Roster::built_in(), env!("CARGO_PKG_VERSION"), &skill).unwrap_or_else(|e| panic!("{e}"));
    let written: HashSet<PathBuf> = files.iter().map(|(path, _)| path.clone()).collect();
    for dir in GENERATED_DIRS {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let rel = Path::new(dir).join(entry.file_name());
            if !written.contains(&rel) {
                std::fs::remove_file(entry.path()).expect("stale files are removable");
                println!("removed {}", rel.display());
            }
        }
    }
    for (path, contents) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().expect("files live in a directory")).expect("directories are creatable");
        std::fs::write(&path, contents).expect("files are writable");
        println!("wrote {}", path.strip_prefix(root).unwrap_or(&path).display());
    }
}

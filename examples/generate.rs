//! Rewrites the plugin's generated files from roster.toml.

use brigade::render::{SKILL_FILE, plugin_files};
use brigade::roster::Roster;
use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let skill = std::fs::read_to_string(root.join(SKILL_FILE)).expect("the skill template is readable");
    let files = plugin_files(&Roster::built_in(), env!("CARGO_PKG_VERSION"), &skill).unwrap_or_else(|e| panic!("{e}"));
    for (path, contents) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().expect("files live in a directory")).expect("directories are creatable");
        std::fs::write(&path, contents).expect("files are writable");
        println!("wrote {}", path.strip_prefix(root).unwrap_or(&path).display());
    }
}

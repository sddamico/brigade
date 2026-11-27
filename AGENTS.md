# brigade

A Rust CLI (`src/`) plus a plugin for Claude Code, Codex and agy (`plugin/`). The terms are in GLOSSARY.md; keep it current when they change.

## Layout

- `roster.toml`: the built-in roster, embedded into the binary
- `src/invocation.rs`: per-harness command lines and output parsing; the rules every harness quirk lives in
- `src/runs.rs`: run directories, the worker, the lease, waiting and cancelling
- `src/process.rs`: a harness's whole process tree, for cancelling (including commands it started in sessions of their own), and a waiter's lineage, for noticing its caller is gone
- `src/render.rs`: renders everything under `plugin/` (and `.claude-plugin/marketplace.json`) from the roster. A host only gets forwarders for agents from other harnesses; its own family stays on its ordinary subagents
- `plugin/skills/brigade/SKILL.md`: hand-written, except the roster table between the markers

## Workflow

- After changing `roster.toml`, `src/render.rs`, the skill or the version, run `cargo run --example generate` and commit the result. `cargo test` fails while `plugin/` is stale
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` must pass; CI runs them on Linux and macOS
- The CLI tests in `tests/cli.rs` run the binary against fake `claude`/`codex`/`agy` scripts. When a harness's flags or output change, update `src/invocation.rs` and those fakes together, and confirm against the real CLI before trusting either

## Releasing

1. Bump `version` in `Cargo.toml`, run `cargo run --example generate`, commit
2. Tag `vX.Y.Z` and push the tag. mise and `cargo install --tag` build the CLI from it; the marketplaces read the plugin from `main`

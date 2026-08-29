# magents

Rust MCP server + CLI. `cargo test --locked --all-targets`, `cargo fmt`, `cargo clippy -D warnings` before commit.
Coverage gate is `cargo llvm-cov --fail-under-lines 95` (ignores `src/main.rs`).
Handoff and inject coverage lives in `src/handoff.rs`, `src/handoff_tests.rs`, and `tests/cli.rs`.
Publishing a GitHub Release pushes GHCR (`ghcr.io/abnegate/magents`) and attaches musl/darwin binaries to that release. Pushing a tag alone does not.

Do not log mailbox message bodies or Claude UDS tokens.

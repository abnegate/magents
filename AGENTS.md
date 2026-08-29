# magents

Rust MCP server + CLI. `cargo test --locked --all-targets`, `cargo fmt`, `cargo clippy -D warnings` before commit.
Coverage gate is `cargo llvm-cov --fail-under-lines 95` (ignores `src/main.rs`).
Handoff and inject coverage lives in `src/handoff_tests.rs` and `tests/cli.rs`.
Git tags publish GHCR (`ghcr.io/abnegate/magents`) and attach musl/darwin binaries to the GitHub Release.

Do not log mailbox message bodies or Claude UDS tokens.

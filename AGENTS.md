# magents

Rust MCP server + CLI. `cargo test --locked --all-targets`, `cargo fmt`, `cargo clippy -D warnings` before commit.
Coverage gate is `cargo llvm-cov --fail-under-lines 98` (ignores `src/main.rs`).
Handoff and inject coverage lives in `src/handoff.rs`, `src/handoff_tests.rs`, and `tests/cli.rs`.
Publishing a GitHub Release pushes GHCR (`ghcr.io/abnegate/magents`), attaches musl/darwin binaries, and updates `abnegate/homebrew-tap` plus `abnegate/apt-repo`. Pushing a tag alone does not. After changing `package.version`, run `cargo update -p magents` so `Cargo.lock` matches — `--locked` CI fails if they drift.

Do not log mailbox message bodies or Claude UDS tokens.

# magents

Rust MCP server + CLI. `cargo test --locked --all-targets`, `cargo fmt`, `cargo clippy -D warnings` before commit.
Coverage gate is `cargo llvm-cov --fail-under-lines 98` (ignores `src/main.rs`).
Handoff and inject coverage lives in `src/handoff.rs`, `src/handoff_tests.rs`, and `tests/cli.rs`.
Publishing a GitHub Release sets `package.version` from the tag, regenerates `Cargo.lock`, then pushes GHCR (`ghcr.io/abnegate/magents`), attaches musl/darwin binaries, and updates `abnegate/homebrew-tap` plus `abnegate/apt-repo`. Pushing a tag alone does not. If the release commit is the default-branch tip, CI also commits that version bump with the owner token so required status checks do not block the push. Manual `package.version` edits still need `cargo update -p magents` so `--locked` CI does not drift.

Do not log mailbox message bodies or Claude UDS tokens.

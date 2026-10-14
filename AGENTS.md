# magents

Rust MCP server + CLI. `cargo test --locked --all-targets`, `cargo fmt`, `cargo clippy -D warnings` before commit.
Coverage gate is `cargo llvm-cov --fail-under-lines 98` (ignores `src/main.rs`).
Handoff and inject coverage lives in `src/handoff.rs`, `src/handoff_tests.rs`, and `tests/cli.rs`.
Publishing a GitHub Release sets every product version from the tag (`Cargo.toml`, `Cargo.lock`, and every tracked file that already carries the crate version, plus plugin / marketplace / Gemini / server manifests that declare a version — including the GHCR image tag in `server.json`), then pushes GHCR (`ghcr.io/abnegate/magents`), attaches musl/darwin binaries, and updates `abnegate/homebrew-tap` plus `abnegate/apt-repo`. `server.json` `description` (and `title`) must be ≤100 characters or the Official MCP Registry rejects the publish. Pushing a tag alone does not. If the release commit is the default-branch tip, CI also commits that version bump with the owner token so required status checks do not block the push. A manual bump is `.github/scripts/set-release-version.sh <version>` so `--locked` CI does not drift.

Do not log mailbox message bodies or Claude UDS tokens.

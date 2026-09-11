# Host plugins

Installable plugin packages for Codex, Claude Code, and Cursor. Each wires the
existing magents skill plus an MCP server entry that runs `magents mcp`.

Prerequisite: `magents` on PATH (`brew install abnegate/tap/magents`).

| Host | Path | Notes |
| --- | --- | --- |
| Codex | `plugins/codex` | `.codex-plugin/plugin.json` + HOL scanner CI |
| Claude Code | `plugins/claude` | `.claude-plugin/plugin.json` |
| Cursor | `plugins/cursor` | `.cursor-plugin/plugin.json`; repo marketplace at `.cursor-plugin/marketplace.json` |

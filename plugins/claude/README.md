# magents (Claude Code plugin)

Shared session bus for Claude Code: skill + MCP (`magents mcp`).

## Prerequisite

```bash
brew install abnegate/tap/magents
```

## Install

```bash
claude plugin marketplace add abnegate/magents
# or load this folder with --plugin-dir / path in a local marketplace
```

Manifest: `.claude-plugin/plugin.json`. MCP: `.mcp.json`.

## What it does

Same as the Codex plugin: hand off across Claude / Codex / Cursor / Grok without
pasting context.

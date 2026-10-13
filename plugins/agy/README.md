# magents (AGY plugin)

Shared session bus for AGY CLI and the Antigravity desktop app: skill + MCP
(`magents mcp`). Desktop sessions show up as `agy` with origin `desktop`.

## Prerequisite

```bash
brew install abnegate/tap/magents
```

`magents` must be on PATH.

## Install

From GitHub (uses the repo-root `plugin.json` and `mcp_config.json`):

```bash
agy plugin install https://github.com/abnegate/magents
```

Or load this folder:

```bash
agy plugin install /path/to/magents/plugins/agy
```

Manifest: `plugin.json`. MCP: `mcp_config.json`. Skills: `skills/{magents,learn}/SKILL.md`.

## What it does

List/read other live sessions, hand off, send into a specific chat, spawn
independent work that replies through magents.

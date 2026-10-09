# Installing magents (for AI agents such as Cline)

magents is a single Rust binary. The MCP server is the `mcp` subcommand over stdio.

1. Install the binary (pick one):
   - macOS / Linux with Homebrew: `brew install abnegate/tap/magents`
   - Debian / Ubuntu: follow the APT steps in README.md
   - Any platform: download the matching asset from https://github.com/abnegate/magents/releases/latest, `chmod +x magents`, and put it on `PATH`
2. Check it runs: `magents --version`
3. Add the server to the MCP settings file (for Cline, `cline_mcp_settings.json`):

```json
{
  "mcpServers": {
    "magents": {
      "command": "magents",
      "args": ["mcp"]
    }
  }
}
```

No API keys or environment variables are needed. magents reads the session
files that Claude Code, Codex, Copilot, Cursor, Gemini, Grok, and OpenCode
already write under the user's home directory.

4. Verify by calling the `whoami` or `list_sessions` tool.

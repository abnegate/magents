#!/usr/bin/env bash
set -euo pipefail

BIN="${1:-$(cd "$(dirname "$0")/../.." && pwd)/target/release/magents}"
OUT="$(cd "$(dirname "$0")" && pwd)/demo.json"
ROOT="$(mktemp -d)"
CAPTURES="$ROOT/captures"
mkdir -p "$CAPTURES"

CLAUDE_ID='9f3c1a2e-6b0d-4f5a-8c21-7e4d3b2a1c90'
GROK_ID='01j7q4d41e77b0k3m9x2p5r8s6'
CWD="$ROOT/src/edge"
mkdir -p "$CWD"

sleep 900 > /dev/null 2>&1 &
KEEP=$!
disown "$KEEP"
trap 'kill "$KEEP" 2>/dev/null || true; rm -rf "$ROOT"' EXIT

NOW_MS=$(( $(date +%s) * 1000 ))
STARTED_MS=$(( NOW_MS - 4 * 60 * 1000 ))

mkdir -p "$ROOT/claude/sessions" "$ROOT/claude/projects/edge"
cat > "$ROOT/claude/sessions/$KEEP.json" <<EOF
{"pid":$KEEP,"sessionId":"$CLAUDE_ID","cwd":"$CWD","entrypoint":"cli","name":"disaster recovery","startedAt":$STARTED_MS}
EOF
cat > "$ROOT/claude/projects/edge/$CLAUDE_ID.jsonl" <<'EOF'
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"write the failover runbook for the edge cluster"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Drafted docs/RUNBOOK.md with steps 1-6. Step 7 needs the DNS TTL from ops."},{"type":"tool_use","name":"Write","input":{"file_path":"docs/RUNBOOK.md"}}]}}
EOF

ENCODED_CWD=$(printf '%s' "$CWD" | sed 's#/#%2F#g')
mkdir -p "$ROOT/grok/sessions/$ENCODED_CWD/$GROK_ID"
cat > "$ROOT/grok/active_sessions.json" <<EOF
[{"session_id":"$GROK_ID","pid":$KEEP}]
EOF
cat > "$ROOT/grok/sessions/$ENCODED_CWD/$GROK_ID/summary.json" <<EOF
{"info":{"id":"$GROK_ID","cwd":"$CWD"},"generated_title":"Queue drain on edge","session_kind":"main","last_active_at":"$(date -u +%Y-%m-%dT%H:%M:%SZ)"}
EOF
cat > "$ROOT/grok/sessions/$ENCODED_CWD/$GROK_ID/updates.jsonl" <<'EOF'
{"params":{"update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"fix the queue drain on edge"}}}}
{"params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"closing the fd before requeue"}}}}
{"params":{"update":{"sessionUpdate":"turn_completed"}}}
EOF

mkdir -p "$ROOT/bin"
cat > "$ROOT/bin/grok" <<EOF
#!/bin/sh
cat > /dev/null
printf '{"method":"session/update","params":{"sessionId":"%s"}}\n' "$GROK_ID"
EOF
chmod +x "$ROOT/bin/grok"

capture() {
  local name="$1"; shift
  local file="$CAPTURES/$name.ansi"
  local -a pty
  if [[ "$(uname)" == "Darwin" ]]; then
    pty=(script -q "$file" "$BIN" "$@")
  else
    pty=(script -q -c "$(printf '%q ' "$BIN" "$@")" "$file")
  fi
  (
    cd "$CWD"
    env -i \
      PATH="/usr/bin:/bin" \
      TERM=xterm-256color \
      HOME="$ROOT" \
      CLAUDE_CONFIG_DIR="$ROOT/claude" \
      CLAUDE_SESSION_ID="$CLAUDE_ID" \
      CLAUDE_PROJECT_DIR="$CWD" \
      GROK_HOME="$ROOT/grok" \
      CODEX_HOME="$ROOT/codex" \
      CURSOR_CONFIG_DIR="$ROOT/cursor-config" \
      CURSOR_DATA_DIR="$ROOT/cursor" \
      CURSOR_APP_SUPPORT="$ROOT/cursor-app" \
      MAGENTS_HOME="$ROOT/magents" \
      XDG_CONFIG_HOME="$ROOT/opencode-config" \
      XDG_DATA_HOME="$ROOT" \
      GEMINI_CLI_HOME="$ROOT/gemini" \
      COPILOT_HOME="$ROOT/copilot" \
      MAGENTS_GROK_BIN="$ROOT/bin/grok" \
      "${pty[@]}" > /dev/null < /dev/null
  )
  perl -pi -e 's/^\^D\x08\x08//; s/\r\n/\n/g' "$file"
}

capture list list --live
capture digest digest claude:latest
capture handoff handoff grok:latest --reason 'continuing in grok'

jq -n \
  --rawfile list "$CAPTURES/list.ansi" \
  --rawfile digest "$CAPTURES/digest.ansi" \
  --rawfile handoff "$CAPTURES/handoff.ansi" \
  '{steps: [
    {command: "magents list --live", output: $list},
    {command: "magents digest claude:latest", output: $digest},
    {command: "magents handoff grok:latest --reason \"continuing in grok\"", output: $handoff}
  ]}' > "$OUT"

echo "wrote $OUT"

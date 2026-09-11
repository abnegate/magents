---
name: learn
description: >-
  Learn from every coding agent on this machine (Claude Code, Codex, Copilot,
  Cursor, Gemini, Grok, OpenCode) and tune the harness: add skills for things
  you keep typing, fix stale skill instructions, and retire skills, plugins, or
  MCP servers you never use. Use on "/learn", "learn from my traces", "learn
  from all agents", "what skills do I never use". Not for summarizing sessions
  into notes or memory.
user-invocable: true
argument-hint: "[--mode step|auto|report] [--days N | --since-last | --limit N] [--per-trace] [--include-headless] [--cwd PATH] [--agent NAME] [--focus TEXT] [--resume]"
---

# learn

Read the sessions this user sat at across every local agent, find what they
repeat, correct, and never use, and change the harness so the next session
needs less typing. Collection is deterministic magents work. Map-reduce
fans out from `plan.json` so **all history** is the default corpus, not a
14-day sample.

Prefer magents MCP tools (`search_tool` query `magents`, then `learn_collect`
and `learn_state`). If MCP is unavailable, run `magents --output json learn …`.
If magents is not installed, tell the user to run `magents install --all` and
stop. Do not walk session files by hand.

Files next to this SKILL.md: `signals.md` (what to extract), `report-format.md`
(shape of `report.md` and `actions.json`). Collect also copies those into
`<RUN>/skill/` for mapper sessions.

If the user asked to summarize learnings into memory or notes rather than
change skills, do not run this skill; use memory tools and say why.

## 0. Resume or set up

Call `learn_state` with `action=get` (or `magents --output json learn state`).
A pending run is `pending.status` other than `done`:

- `report_ready` or `curating` — the report exists but was never curated.
  Tell the user the run date, session count, and action count from
  `<run_dir>/actions.json`, and offer to resume at step 3. Honor
  `decisions.jsonl` lines already written for that run.
- `running` with no `report.md` — map-reduce was interrupted. Offer to read
  the newest file under `<run_dir>/reduce/` (else `map/`) and stop, or start
  fresh.
- `collected` — sessions were collected but map-reduce never ran. Offer to
  continue from step 2 with that run dir, or start fresh.
- `--resume` takes the first offer in each case without asking.

For a new run, price it first:

```
learn_collect  estimate=true  [batch=1 if --per-trace]
magents --output json learn estimate [--batch 1]
```

It writes `estimate.json` (`out` is the path). Each scope (`all`, `30d`,
`14d`, `quick` = 25 most recent, `since_last` when a completed run exists)
has session count, agent count, estimated tokens, and a minutes range, plus
`recommended` (`since_last` only after a completed run that collected
without `--days` or `--limit` and still has new sessions; otherwise
**`all`**). A previous `quick` / `14d` / `--limit` run does **not** count.
Tokens are roughly ±50%. Quote the minutes range, not a single number. If
every scope has 0 sessions, say there is nothing to learn from yet and stop.

Then read what the user typed. Depth words and go words combine ("just run
it, quick" = `auto` + `quick`). Explicit flags win over wording. One rule
overrides both: a first-ever run (no `last_completed_at` in the estimate) is
always `step`, because `auto` removes zero-use skills and the user has never
seen a report.

The point of this skill is the full corpus. `all` is the default scope.
`14d` / `quick` / `--limit` exist so the user can ask for a cheaper window;
they are not a substitute for a first run. Estimated tokens are a **price**,
not a stop. Quote the price, then collect the chosen scope. Never downscope
to 14 days or 25 sessions because the token estimate is large, because this
session cannot hold the traces, or because map-reduce will take a while.
Spawn mappers from `plan.json` instead.

- **They told you to go.** "just run it", "go ahead", "don't ask", "auto",
  "you decide" → mode `auto`, scope = recommended (`all` until a full
  unwindowed run has completed, then `since_last`). State the choice and
  its price in one line before launching.
- **They told you the depth.** "quick" / "cheap" / "light" → `quick`;
  "everything" / "all of it" / "deep" / "all history" → `all`; "per trace"
  → `batch=1`; "since last time" / "what's new" → `since_last`. Mode stays
  `step` unless they also said to go.
- **Neutral** (bare `/learn`, "learn from my traces") → ask once, two
  questions (structured question tool if the host has one, else one
  plain-text line like `step, all`):
  - **Mode** — `step` (recommended): present each group and apply what they
    pick. `auto`: apply every reversible action, then ask once about the
    rest. `report`: write the report, change nothing.
  - **Scope** — one option per estimate window, labeled with its price.
    Mark `all` as recommended on a first run (or `since_last` when it has
    sessions). Headless bot sessions stay out unless `--include-headless`.

`--limit N` keeps the N most recent. `--focus TEXT` is a lens for mappers
and reducers; it cannot remove a report section. `--cwd PATH` limits
collection to one working directory. `--agent NAME` limits to one harness.

## 1. Collect

```
learn_collect  [days|since_last|limit|include_headless|cwd|agent|batch]
magents --output json learn collect [--days N | --since-last | --limit N] [--include-headless] [--cwd PATH] [--agent NAME]
```

Pass no `days` / `limit` / `since_last` for `all`. Default `batch` is 25
(1 for `--per-trace`).

A session is kept when the user sat at it: not a subagent, not a magents
headless spawn unless included, not started under OS temp directories, at
least one real human prompt, and not a one-word smoke test with no tool use.
Session ids are `agent:id`. Pasted credentials are redacted before anything
is written. Host chrome (Claude `<local-command-stdout>`, "The app was
quit…", XML wrappers) is stripped from turns; slash commands inside XML
closing tags are ignored. Claude `~/.claude/commands/*.md` files are
inventoried as command-source skills.

Outputs in `<RUN>`: `manifest.json`, `sessions/NNNN-<agent>-<id>.json`
(human `turns`, `slash_commands`, `skills_loaded`, `mcp_servers_used`,
`tools`, `top_dirs_touched`, cwd, git root, title, model, **agent**),
`surfaces.json` (skills/plugins/MCP/hooks/workflows from every agent home,
names and paths only), `usage.json`, `phrases.json`, `plan.json` (mapper
shards and reduce groups), `map/prompts/*.md`, `skill/signals.md`,
`skill/report-format.md`, and `decisions.jsonl` when prior decisions exist.

Tell the user the coverage line (`seen`, `kept`, `dropped` by reason) and
the shard count from `plan.json` **before** map-reduce, and that the
prompts from those N sessions will be sent to mapper agents. If `kept` is
0, fix the scope. A skill counts as used when its SKILL.md was read, its
slash command appears in a prompt, or a matching `commands/*.md` file
exists.

If the batch should change after collect, `magents --output json learn plan
[--run-dir <RUN>] [--batch N]` rewrites `plan.json` and the prompt files.
`learn plan` with no `--run-dir` uses the pending run.

## 2. Map-reduce

Do not hold the corpus in this session. Do not invent a workflow engine.
Do not shrink the collect window to make the work fit here. Write only
inside `<RUN>`. Never print secrets.

`learn_state action=set run_dir=<RUN> status=running mode=<mode> scope=<scope>`
before starting.

Read `<RUN>/plan.json`. That file is the work list.

**Map.** One note per shard in `plan.json` `shards`, already named
(`note`, `prompt`, `files`). If there is **one** shard, you may map it
here: read every session file in that shard in full, then `surfaces.json`,
then `skill/signals.md` sections 1, 2, 3, 5. Follow the per-session record
shape in `signals.md`. End with `## Batch candidates`. Write the `note`
path.

If there is **more than one shard**, spawn one mapper session per shard.
Do not map a multi-shard corpus yourself. For each shard:

```
spawn_session  agent=<this host from whoami>  cwd=<RUN>  message=<contents of shard.prompt>
```

Launch in parallel waves of at most 10. A spawn `accepted: true`,
`status: starting` means launch was accepted, not that the note is
written. After each wave, `await_reply` / `inbox` until the mapper says
the `note` path is done. Then read that file; if it is missing, log the
shard as lost and continue. If `spawn_session` is unavailable, run
`magents spawn <agent> --prompt-file <RUN>/<shard.prompt> --cwd <RUN>`
for each shard the same way. Do not fall back to collecting 14 days or
25 sessions.

**Reduce.** Follow `plan.json` `reduce` in round order. Each group names
`inputs` (map or previous-round notes) and `note`. Read those files,
`usage.json`, `surfaces.json`, `phrases.json`, and `decisions.jsonl` if
present. Never re-propose an action the user rejected. Write the group
`note`. Sections: repeated phrases → skills; skills to update; unused →
delete or disable; gaps; Dropped (or a sibling `.dropped.md` if the file
would exceed 350 lines). Spawn reducers the same way as mappers when a
round has more than one group; the last round (one group) may run here.

**Verify.** Three skeptic passes, keep or drop, add nothing: phrases, stale
lines, deletes. Write `<RUN>/verify/{phrases,stale,deletes}.md` as `## Kept`
and `## Dropped`. Check at most 40 claims per section.

**Report.** Follow `skill/report-format.md` (same as `report-format.md`
next to this skill). Write `<RUN>/report.md` and `<RUN>/actions.json`. For
every edit, open the file and make `edit.anchor` an exact substring. For
every create, write the complete SKILL.md. A protected skill (`plugin`,
`bundled`, `git-tracked`) is `propose`, never an in-place edit or delete.
Then `learn_state action=set run_dir=<RUN> status=report_ready`.

A phrase seen on more than one agent is one habit. Skill creates, user-skill
edits, and user-skill overrides from `propose` are **one action** that apply
writes to every installed provider (every path in `surfaces.json`
`user_skills_dirs`). Plugin, MCP, workflow, hook, and config actions stay
per-harness.

## 3. Present

Read `report.md`. Give the Overview, the three tables trimmed to their top
rows, the Coverage line, and the path to the full report. Keep it under a
screen; do not paste `actions.json`. If a headline names a skill as missing,
check `slash_commands_with_no_loaded_skill` first.

## 4. Curate and apply

Consent rules, identical in every mode:

- Apply an action only when the user picked its id, or in `auto` when it is
  reversible and not `requires_confirmation`.
- A free-text answer is not consent. Re-ask with the literal reading as
  options before writing anything.
- Unpicked options are `deferred`, not `rejected`. Only an explicit refusal
  is `rejected`. Deferred items return next run; rejected ones do not.
- One group's answer never authorizes another group.
- A single-item pick on a multi-select that offered many is an accidental
  Enter: apply the pick, defer the rest, and say so.

Apply:

Skill `create`, `edit`, and `propose` (plugin/bundled override) fan out to
**every** installed provider. Read `user_skills_dirs` from
`<RUN>/surfaces.json` (claude, grok, cursor, gemini, copilot, opencode,
codex when present). For each directory, the file is
`<dir>/<target>/SKILL.md`. Consent on the action id authorizes every copy.
Plugin enable/disable, MCP, workflow, hook, and config actions do **not**
fan out — only `path`.

- `create` — write `edit.replacement` to `<dir>/<target>/SKILL.md` for every
  `user_skills_dirs` entry, creating each directory. `path` is the first
  copy; the rest are the same file under the other homes.
- `edit` — before the first edit to any copy, write an untouched original
  to `<magents_home>/learn/trash/<run name>/originals/<name>.md`. Then for
  each `user_skills_dirs` copy that exists, replace `edit.anchor` per
  `edit.mode` (`replace`, `insert_after`, `append`). If a home has no copy,
  write the full post-edit file there (same as create). If the anchor is
  missing on an existing copy, re-read and fix it; never overwrite the
  whole file except when creating a missing copy.
- `propose` — protected. For `plugin` or `bundled`: apply the edit to the
  full protected SKILL.md text and write the complete result to
  `<dir>/<target>/SKILL.md` for every `user_skills_dirs` entry (a user
  skill of the same name wins over plugin/bundled on that host). For
  `git-tracked`: write `<RUN>/patches/<name>.diff` and say it needs a pull
  request. Never edit plugin, bundled, or git-tracked files in place.
- `enable` / `disable` — edit only the named plugin enabled/disabled list in
  that harness config. Names only.
- `delete` — if `referenced_by` is non-empty, stop and ask. Otherwise
  `learn_state action=trash run_name=<run name> paths=[path]`. For an MCP
  server, cut its table/object out of config, write it to
  `<magents_home>/learn/trash/<run name>/mcp-<name>.toml` (or `.json`), then
  `learn_state action=restrict paths=[that file]` before removing the key.
- After each apply, re-read the changed file and confirm the change.

By mode: **step** — one question per group (phrases, updates, deletes,
gaps), at most eight options, apply picked then defer the rest.
**auto** — apply `requires_confirmation: false`, then one question for the
rest. **report** — stop after step 3 and set status `done`.

Record every decision with `learn_state action=decide` (`applied` /
`rejected` / `deferred`). Set status `curating` when group 1 opens and
`done` when the last group closes. Finish with what changed, what was
rejected or deferred, how to undo, and the full report path.

## Rules

- Zero hits is evidence, not proof. The only copy of a job is an `ask`, not
  a delete. A skill in slash MRU, or one that other loaded skills reference,
  is not a free delete.
- One repeated phrase is one action; it joins the skill that already owns
  the job when one exists.
- Never print secrets: tokens, headers, env values, API keys, auth files.
  Inventory is names and paths only.
- If the user disputes a count, re-run collect; do not guess.
- Do not log mailbox bodies or Claude UDS tokens.
- Never replace `all` with `14d` or `quick` unless the user asked for that
  window.

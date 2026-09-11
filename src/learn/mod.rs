mod scan;
mod state;
mod surfaces;
mod text;

use crate::discover::{ListFilter, list_sessions};
use crate::error::{Error, Result};
use crate::homes::Homes;
use crate::model::Agent;
use chrono::{Duration, Utc};
use scan::{CompactSession, ScanFilter, compact, filename};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use text::{normalize, phrases};

pub use state::{Decision, LearnState, StateUpdate, clear, decide, get, restrict, set, trash};

pub const DEFAULT_BATCH: usize = 25;
const FAN: usize = 10;
const FIXED_AGENTS: usize = 4;
const MAP_TOKENS_BATCH: usize = 110_000;
const MAP_TOKENS_PER_TRACE: usize = 250_000;
const REDUCER_TOKENS: usize = 1_300_000;
const VERIFIER_TOKENS: usize = 800_000;
const REPORT_TOKENS: usize = 2_500_000;
const DEFAULT_DROP: &str = r"^the user invoked `/feedback`";
const SIGNALS: &str = include_str!("../../skills/learn-signals.md");
const REPORT_FORMAT: &str = include_str!("../../skills/learn-report-format.md");

#[derive(Clone, Debug, Default)]
pub struct CollectParams {
    pub days: u32,
    pub since_last: bool,
    pub include_headless: bool,
    pub include_subagents: bool,
    pub cwd: Vec<String>,
    pub exclude_cwd: Option<Vec<String>>,
    pub drop_patterns: Vec<String>,
    pub min_turns: usize,
    pub session_ids: Vec<String>,
    pub limit: usize,
    pub agent: Option<Agent>,
    pub estimate: bool,
    pub batch: usize,
    pub out: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Estimate {
    pub sessions: usize,
    pub agents: usize,
    pub mappers: usize,
    pub reducers: usize,
    pub tokens: usize,
    pub tokens_m: f64,
    pub minutes: [u32; 2],
}

#[derive(Clone, Debug, Serialize)]
pub struct EstimateReport {
    pub magents_home: PathBuf,
    pub out: PathBuf,
    pub generated_at: String,
    pub sessions_seen: usize,
    pub dropped: BTreeMap<String, usize>,
    pub batch: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_completed_at: Option<String>,
    pub windows: BTreeMap<String, EstimateWindow>,
    pub recommended: String,
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct EstimateWindow {
    #[serde(flatten)]
    pub estimate: Estimate,
    pub flags: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CollectReport {
    pub run_dir: PathBuf,
    pub kept: usize,
    pub seen: usize,
    pub dropped: BTreeMap<String, usize>,
    pub batch: usize,
    pub shards: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlanReport {
    pub run_dir: PathBuf,
    pub batch: usize,
    pub fan: usize,
    pub sessions: usize,
    pub shards: Vec<MapShard>,
    pub reduce: Vec<ReduceRound>,
    pub estimate: Estimate,
}

#[derive(Clone, Debug, Serialize)]
pub struct MapShard {
    pub id: String,
    pub start: usize,
    pub end: usize,
    pub note: String,
    pub prompt: String,
    pub files: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReduceRound {
    pub round: usize,
    pub groups: Vec<ReduceGroup>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReduceGroup {
    pub id: String,
    pub inputs: Vec<String>,
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum CollectOutcome {
    Estimate(EstimateReport),
    Collect(CollectReport),
}

pub fn collect(homes: &Homes, params: &CollectParams) -> Result<CollectOutcome> {
    let batch = if params.batch == 0 {
        DEFAULT_BATCH
    } else {
        params.batch
    };
    let mut drop_patterns = vec![DEFAULT_DROP.to_string()];
    drop_patterns.extend(params.drop_patterns.iter().cloned());
    let drop_regex: Vec<regex::Regex> = drop_patterns
        .iter()
        .map(|pattern| {
            regex::Regex::new(&format!("(?i){pattern}"))
                .map_err(|error| Error::msg(error.to_string()))
        })
        .collect::<Result<Vec<_>>>()?;
    let exclude = params
        .exclude_cwd
        .clone()
        .unwrap_or_else(default_exclude_cwd);
    let state = state::get(homes)?;
    let cutoff = if params.estimate {
        None
    } else if params.days > 0 {
        Some(Utc::now() - Duration::days(params.days as i64))
    } else if params.since_last {
        state.last_completed_at.as_deref().and_then(parse_time)
    } else {
        None
    };
    let sessions = list_sessions(
        homes,
        &ListFilter {
            agent: params.agent,
            include_archived: true,
            limit: 0,
            ..ListFilter::default()
        },
    )?;
    let filter = ScanFilter {
        include_headless: params.include_headless,
        include_subagents: params.include_subagents,
        cwd: &params.cwd,
        exclude_cwd: &exclude,
        drop_patterns: &drop_regex,
        min_turns: params.min_turns.max(1),
        session_ids: &params.session_ids,
        cutoff,
    };
    let mut seen = 0usize;
    let mut dropped: BTreeMap<String, usize> = BTreeMap::new();
    let mut kept = Vec::new();
    for session in sessions {
        seen += 1;
        match compact(&session, &filter) {
            Ok(row) => kept.push(row),
            Err(skip) => *dropped.entry(skip.as_str().to_string()).or_default() += 1,
        }
    }
    kept.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then(left.id.cmp(&right.id))
    });

    let stamp = if params.estimate {
        "estimate".to_string()
    } else {
        Utc::now().format("%Y%m%d-%H%M%S").to_string()
    };
    let out = params
        .out
        .clone()
        .unwrap_or_else(|| homes.learn_dir().join("runs").join(stamp));
    mkdir_owner(&out)?;

    if params.estimate {
        let report = estimate_report(homes, &out, seen, &dropped, &kept, batch, &state)?;
        write_json(out.join("estimate.json"), &report)?;
        return Ok(CollectOutcome::Estimate(report));
    }

    if params.limit > 0 && kept.len() > params.limit {
        *dropped.entry("beyond_limit".into()).or_default() += kept.len() - params.limit;
        kept.truncate(params.limit);
    }
    mark_repeated_singles(&mut kept);

    let sessions_dir = out.join("sessions");
    mkdir_owner(&sessions_dir)?;
    for name in ["map", "reduce", "verify"] {
        mkdir_owner(&out.join(name))?;
    }
    for (index, row) in kept.iter_mut().enumerate() {
        row.index = index;
        let path = sessions_dir.join(filename(index, &row.agent, &row.session_id));
        write_json(path, row)?;
    }

    let surfaces = surfaces::collect(homes, &kept);
    write_json(out.join("surfaces.json"), &surfaces.value)?;
    let usage = surfaces::usage(&surfaces, &kept);
    write_json(out.join("usage.json"), &usage)?;
    let phrase_rows: Vec<(String, Vec<String>, bool)> = kept
        .iter()
        .map(|row| (row.id.clone(), row.turns.clone(), row.repeated_single_turn))
        .collect();
    let phrase_refs: Vec<(String, &[String], bool)> = phrase_rows
        .iter()
        .map(|(id, turns, repeated)| (id.clone(), turns.as_slice(), *repeated))
        .collect();
    write_json(out.join("phrases.json"), &phrases(&phrase_refs))?;
    state::copy_decisions(homes, &out)?;

    let generated_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let project_roots = surfaces
        .value
        .get("project_roots")
        .cloned()
        .unwrap_or(json!([]));
    let manifest = json!({
        "magents_home": homes.magents,
        "run_dir": out,
        "generated_at": generated_at,
        "params": {
            "days": params.days,
            "since_last": params.since_last,
            "cutoff": cutoff.map(|time| time.to_rfc3339()),
            "include_headless": params.include_headless,
            "include_subagents": params.include_subagents,
            "cwd": params.cwd,
            "exclude_cwd": exclude,
            "drop_patterns": params.drop_patterns,
            "min_turns": filter.min_turns,
            "session_ids": params.session_ids,
            "limit": params.limit,
            "agent": params.agent.map(|agent| agent.as_str()),
        },
        "estimate": estimate_cost(kept.len(), batch),
        "sessions_seen": seen,
        "sessions_kept": kept.len(),
        "dropped": dropped,
        "human_turns_total": kept.iter().map(|row| row.human_turns).sum::<usize>(),
        "repeated_single_turn_sessions": kept.iter().filter(|row| row.repeated_single_turn).count(),
        "project_roots": project_roots,
        "kept": kept.iter().map(|row| json!({
            "index": row.index,
            "id": row.id,
            "agent": row.agent,
            "cwd": row.cwd,
            "turns": row.human_turns,
            "updated_at": row.updated_at,
            "title": row.title,
        })).collect::<Vec<_>>(),
        "prior_decisions": homes.learn_dir().join("decisions.jsonl").is_file(),
    });
    write_json(out.join("manifest.json"), &manifest)?;
    let planned = plan(&out, batch)?;
    state::record_collection(homes, &out, &generated_at, kept.len())?;
    Ok(CollectOutcome::Collect(CollectReport {
        run_dir: out,
        kept: kept.len(),
        seen,
        dropped,
        batch,
        shards: planned.shards.len(),
    }))
}

pub fn plan(run_dir: &Path, batch: usize) -> Result<PlanReport> {
    let batch = if batch == 0 { DEFAULT_BATCH } else { batch };
    let sessions_dir = run_dir.join("sessions");
    let mut files: Vec<String> = match fs::read_dir(&sessions_dir) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                    Some(format!("sessions/{}", entry.file_name().to_string_lossy()))
                } else {
                    None
                }
            })
            .collect(),
        Err(source) => {
            return Err(Error::Io {
                path: sessions_dir,
                source,
            });
        }
    };
    files.sort();
    mkdir_owner(&run_dir.join("map"))?;
    mkdir_owner(&run_dir.join("map").join("prompts"))?;
    mkdir_owner(&run_dir.join("reduce"))?;
    mkdir_owner(&run_dir.join("verify"))?;
    mkdir_owner(&run_dir.join("skill"))?;
    fs::write(run_dir.join("skill").join("signals.md"), SIGNALS).map_err(|source| Error::Io {
        path: run_dir.join("skill").join("signals.md"),
        source,
    })?;
    fs::write(
        run_dir.join("skill").join("report-format.md"),
        REPORT_FORMAT,
    )
    .map_err(|source| Error::Io {
        path: run_dir.join("skill").join("report-format.md"),
        source,
    })?;
    let mut shards = Vec::new();
    for (index, chunk) in files.chunks(batch).enumerate() {
        let start = index * batch;
        let end = start + chunk.len() - 1;
        let id = format!("{:04}-{:04}", start, end);
        let note = format!("map/{id}.md");
        let prompt = format!("map/prompts/{id}.md");
        let body = mapper_prompt(&id, chunk, &note);
        fs::write(run_dir.join(&prompt), body).map_err(|source| Error::Io {
            path: run_dir.join(&prompt),
            source,
        })?;
        shards.push(MapShard {
            id,
            start,
            end,
            note,
            prompt,
            files: chunk.to_vec(),
        });
    }
    let map_notes: Vec<String> = shards.iter().map(|shard| shard.note.clone()).collect();
    let reduce = reduce_rounds(&map_notes, FAN);
    let report = PlanReport {
        run_dir: run_dir.to_path_buf(),
        batch,
        fan: FAN,
        sessions: files.len(),
        shards,
        reduce,
        estimate: estimate_cost(files.len(), batch),
    };
    write_json(run_dir.join("plan.json"), &report)?;
    Ok(report)
}

fn mapper_prompt(id: &str, files: &[String], note: &str) -> String {
    let mut lines = vec![
        format!("Map /learn shard {id}. Write only {note}. Do not print secrets."),
        String::new(),
        "Read every session file in this list in full:".into(),
    ];
    for file in files {
        lines.push(format!("- {file}"));
    }
    lines.push(String::new());
    lines.push("Then read surfaces.json and skill/signals.md sections 1, 2, 3, and 5.".into());
    lines.push(format!(
        "Write {note} using the per-session record shape from signals.md. End with `## Batch candidates`."
    ));
    lines.push(format!(
        "When the file is written, reply through magents that {note} is done, with the candidate count."
    ));
    lines.join("\n") + "\n"
}

fn reduce_rounds(notes: &[String], fan: usize) -> Vec<ReduceRound> {
    if notes.is_empty() {
        return Vec::new();
    }
    let fan = fan.max(1);
    let mut rounds = Vec::new();
    let mut inputs = notes.to_vec();
    let mut round = 1usize;
    loop {
        let groups: Vec<ReduceGroup> = inputs
            .chunks(fan)
            .enumerate()
            .map(|(index, chunk)| ReduceGroup {
                id: format!("r{round}-{index:04}"),
                inputs: chunk.to_vec(),
                note: format!("reduce/r{round}-{index:04}.md"),
            })
            .collect();
        let next: Vec<String> = groups.iter().map(|group| group.note.clone()).collect();
        let last = next.len() <= 1;
        rounds.push(ReduceRound { round, groups });
        if last {
            break;
        }
        inputs = next;
        round += 1;
    }
    rounds
}

pub fn estimate_cost(n: usize, batch: usize) -> Estimate {
    if n == 0 {
        return Estimate {
            sessions: 0,
            agents: 0,
            mappers: 0,
            reducers: 0,
            tokens: 0,
            tokens_m: 0.0,
            minutes: [0, 0],
        };
    }
    let batch = batch.max(1);
    let mappers = n.div_ceil(batch);
    let mut reducers = 0usize;
    let mut level = mappers;
    loop {
        let groups = level.div_ceil(FAN);
        reducers += groups;
        level = groups;
        if groups <= 1 {
            break;
        }
    }
    let per_session = if batch == 1 {
        MAP_TOKENS_PER_TRACE
    } else {
        MAP_TOKENS_BATCH * 10 / batch
    };
    let tokens = n * per_session + reducers * REDUCER_TOKENS + 3 * VERIFIER_TOKENS + REPORT_TOKENS;
    let mid = 30 + (n / 6) as u32;
    Estimate {
        sessions: n,
        agents: mappers + reducers + FIXED_AGENTS,
        mappers,
        reducers,
        tokens,
        tokens_m: (tokens as f64 / 1_000_000.0 * 10.0).round() / 10.0,
        minutes: [15.max(mid.saturating_sub(15)), mid + 20],
    }
}

fn estimate_report(
    homes: &Homes,
    out: &Path,
    seen: usize,
    dropped: &BTreeMap<String, usize>,
    kept: &[CompactSession],
    batch: usize,
    state: &LearnState,
) -> Result<EstimateReport> {
    let now = Utc::now();
    let last_completed_at = state.last_completed_at.clone();
    let since = last_completed_at.as_deref().and_then(parse_time);
    let in_window = |row: &CompactSession, cut: chrono::DateTime<chrono::Utc>| {
        row.updated_at
            .as_deref()
            .and_then(parse_time)
            .is_some_and(|time| time >= cut)
    };
    let mut windows = BTreeMap::new();
    let push = |windows: &mut BTreeMap<String, EstimateWindow>,
                name: &str,
                rows: &[CompactSession],
                flags: Vec<String>| {
        windows.insert(
            name.to_string(),
            EstimateWindow {
                estimate: estimate_cost(rows.len(), batch),
                flags,
            },
        );
    };
    push(&mut windows, "all", kept, vec![]);
    let last_30: Vec<_> = kept
        .iter()
        .filter(|row| in_window(row, now - Duration::days(30)))
        .cloned()
        .collect();
    push(
        &mut windows,
        "30d",
        &last_30,
        vec!["--days".into(), "30".into()],
    );
    let last_14: Vec<_> = kept
        .iter()
        .filter(|row| in_window(row, now - Duration::days(14)))
        .cloned()
        .collect();
    push(
        &mut windows,
        "14d",
        &last_14,
        vec!["--days".into(), "14".into()],
    );
    let quick: Vec<_> = kept.iter().take(25).cloned().collect();
    push(
        &mut windows,
        "quick",
        &quick,
        vec!["--limit".into(), "25".into()],
    );
    if let Some(since) = since {
        let rows: Vec<_> = kept
            .iter()
            .filter(|row| in_window(row, since))
            .cloned()
            .collect();
        push(
            &mut windows,
            "since_last",
            &rows,
            vec!["--since-last".into()],
        );
    }
    let recommended = if last_run_unwindowed(state.last_completed_dir.as_deref())
        && since.is_some()
        && windows
            .get("since_last")
            .is_some_and(|window| window.estimate.sessions > 0)
    {
        "since_last"
    } else {
        "all"
    };
    Ok(EstimateReport {
        magents_home: homes.magents.clone(),
        out: out.to_path_buf(),
        generated_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        sessions_seen: seen,
        dropped: dropped.clone(),
        batch,
        last_completed_at,
        windows,
        recommended: recommended.into(),
        note: "recommended is all history until a completed run collected without --days or --limit; after that, since_last. 14d and quick are cheaper windows, not a substitute for a first full run. tokens are a calibrated estimate (about +/-50%); minutes are a range, growing slowly with session count because mappers run in parallel".into(),
    })
}

fn last_run_unwindowed(run_dir: Option<&str>) -> bool {
    let Some(run_dir) = run_dir.filter(|path| !path.is_empty()) else {
        return false;
    };
    let Ok(raw) = fs::read_to_string(Path::new(run_dir).join("manifest.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let params = value.get("params");
    let days = params
        .and_then(|value| value.get("days"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let limit = params
        .and_then(|value| value.get("limit"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    days == 0 && limit == 0
}

fn mark_repeated_singles(kept: &mut [CompactSession]) {
    let mut single: BTreeMap<String, usize> = BTreeMap::new();
    for row in kept.iter() {
        if row.human_turns == 1
            && let Some(turn) = row.turns.first()
        {
            *single.entry(normalize(turn)).or_default() += 1;
        }
    }
    for row in kept.iter_mut() {
        row.repeated_single_turn = row.human_turns == 1
            && row
                .turns
                .first()
                .is_some_and(|turn| single.get(&normalize(turn)).copied().unwrap_or(0) >= 3);
    }
}

fn default_exclude_cwd() -> Vec<String> {
    let mut out = vec!["/tmp".into(), "/private/tmp".into(), "/var/folders".into()];
    let tmp = std::env::temp_dir();
    out.push(tmp.display().to_string());
    if let Ok(real) = tmp.canonicalize() {
        out.push(real.display().to_string());
    }
    out.sort();
    out.dedup();
    out
}

fn write_json(path: PathBuf, value: &impl serde::Serialize) -> Result<()> {
    fs::write(&path, serde_json::to_vec_pretty(value)?).map_err(|source| Error::Io { path, source })
}

fn mkdir_owner(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn parse_time(value: &str) -> Option<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

pub fn pending(state: &LearnState) -> bool {
    state
        .pending
        .as_ref()
        .is_some_and(|pending| pending.status != "done")
}

#[cfg(test)]
mod tests {
    use super::{CollectOutcome, CollectParams, collect, estimate_cost, pending, plan};
    use crate::homes::Homes;
    use crate::learn::state::{Decision, StateUpdate, decide, get, set};
    use chrono::Utc;
    use serde_json::json;
    use std::fs;
    use std::path::Path;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    fn grok_session(homes: &Homes, id: &str, cwd: &str, kind: &str, updated: &str, turns: &str) {
        let dir = homes
            .grok
            .join("sessions")
            .join("%2FUsers%2Ftest%2Fapp")
            .join(id);
        write(
            &dir.join("summary.json"),
            &json!({
                "info": {"id": id, "cwd": cwd},
                "generated_title": id,
                "session_kind": kind,
                "last_active_at": updated,
                "current_model_id": "grok-4.6"
            })
            .to_string(),
        );
        write(&dir.join("updates.jsonl"), turns);
    }

    fn user_turn(text: &str) -> String {
        let line1 = json!({"params":{"update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text": text}}}});
        let line2 = json!({"params":{"update":{"sessionUpdate":"tool_call","toolCall":{"name":"read_file"}}}});
        let line3 = json!({"params":{"update":{"sessionUpdate":"turn_completed"}}});
        format!("{line1}\n{line2}\n{line3}\n")
    }

    fn fixture() -> (tempfile::TempDir, Homes) {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let now = Utc::now().to_rfc3339();
        let phrase = "always run cargo test --locked --all-targets before commit";
        grok_session(
            &homes,
            "01learngrok00000000000001",
            "/Users/test/app",
            "main",
            &now,
            &(user_turn(phrase)
                + &json!({"params":{"update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"/review the crate"}}}}).to_string()
                + "\n"
                + &json!({"params":{"update":{"sessionUpdate":"tool_call","name":"use_tool","arguments":{"tool_name":"magents__list_sessions"}}}}).to_string()
                + "\n"
                + &json!({"params":{"update":{"sessionUpdate":"turn_completed"}}}).to_string()
                + "\n"),
        );
        grok_session(
            &homes,
            "01learngrok00000000000002",
            "/Users/test/app",
            "main",
            &now,
            &user_turn(phrase),
        );
        grok_session(
            &homes,
            "01headlessgrok0000000000",
            "/Users/test/app",
            "headless",
            &now,
            &user_turn("headless bot prompt that is long enough"),
        );
        grok_session(
            &homes,
            "01smokegrok0000000000000",
            "/Users/test/app",
            "main",
            &now,
            "{\"params\":{\"update\":{\"sessionUpdate\":\"user_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"hi\"}}}}\n{\"params\":{\"update\":{\"sessionUpdate\":\"turn_completed\"}}}\n",
        );
        grok_session(
            &homes,
            "01tempgrok00000000000000",
            "/tmp/edge",
            "main",
            &now,
            &user_turn("fix the dedicated databases leak for coverage"),
        );
        grok_session(
            &homes,
            "01e2egrok00000000000000",
            "/Users/test/grok-e2e/app",
            "main",
            &now,
            &user_turn("this is an e2e fixture session that should drop"),
        );
        grok_session(
            &homes,
            "01oldgrok00000000000000",
            "/Users/test/app",
            "main",
            "2020-01-01T00:00:00Z",
            &user_turn("ancient session that is outside the days window"),
        );
        let repeat = "same single automation prompt for three sessions now";
        for id in [
            "01rep1grok0000000000000",
            "01rep2grok0000000000000",
            "01rep3grok0000000000000",
        ] {
            grok_session(
                &homes,
                id,
                "/Users/test/app",
                "main",
                &now,
                &user_turn(repeat),
            );
        }
        write(
            &homes.grok.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: review the crate\n---\nRun /review.\n",
        );
        write(
            &homes.grok.join("skills/unused/SKILL.md"),
            "---\nname: unused\ndescription: never loaded\n---\nDo unused things.\n",
        );
        write(
            &homes.grok.join("skills/learn/SKILL.md"),
            "---\nname: learn\ndescription: learn from traces\n---\nSee /review.\n",
        );
        write(
            &homes.grok.join("config.toml"),
            "[mcp_servers.magents]\ncommand = \"magents\"\n[plugins]\nenabled = [\"demo\"]\ndisabled = [\"other\"]\n[skills]\ndisabled = [\"learn\"]\n",
        );
        write(
            &homes
                .grok
                .join("installed-plugins/demo-aaaaaaaa/skills/demo/SKILL.md"),
            "---\nname: demo\ndescription: plugin skill\n---\n",
        );
        write(
            &homes
                .grok
                .join("installed-plugins/other-bbbbbbbb/skills/other/SKILL.md"),
            "---\nname: other\ndescription: disabled plugin\n---\n",
        );
        write(
            &homes
                .grok
                .join("installed-plugins/ghost-cccccccc/skills/ghost/SKILL.md"),
            "---\nname: ghost\ndescription: unlisted plugin\n---\n",
        );
        write(
            &homes.grok.join("installed-plugins/registry.json"),
            &json!({"repos":{"x":{"path": homes.grok.join("installed-plugins/demo-aaaaaaaa"), "plugins":{"demo":{}}}}})
                .to_string(),
        );
        write(
            &homes.grok.join("bundled/skills/bundled-one/SKILL.md"),
            "---\nname: bundled-one\ndescription: bundled\n---\n",
        );
        write(
            &homes
                .claude
                .parent()
                .unwrap()
                .join(".agents/skills/shared/SKILL.md"),
            "---\nname: shared\ndescription: agents dir\n---\n",
        );
        write(
            &homes.grok.join("slash-mru.json"),
            "{\"by_command\":{\"review\":1}}\n",
        );
        write(
            &homes.claude.join("plugins/market/skills/plug/SKILL.md"),
            "---\nname: plug\ndescription: claude plugin\n---\n",
        );
        write(
            &homes.claude.join("settings.json"),
            "{\"mcpServers\":{\"claude-extra\":{\"command\":\"n\"}}}\n",
        );
        write(
            &homes.cursor_config.join("mcp.json"),
            "{\"mcpServers\":{\"cursor-extra\":{\"command\":\"n\"}}}\n",
        );
        write(
            &homes.gemini.join("settings.json"),
            "{\"mcpServers\":{\"gemini-extra\":{\"command\":\"n\"}}}\n",
        );
        write(
            &homes.copilot.join("mcp-config.json"),
            "{\"mcpServers\":{\"copilot-extra\":{\"command\":\"n\"}}}\n",
        );
        write(
            &homes.opencode_config.join("opencode.json"),
            "{\"mcp\":{\"opencode-extra\":{\"type\":\"local\"}}}\n",
        );
        write(
            &homes.codex.join("config.toml"),
            "[mcp_servers.codexextra]\ncommand = \"n\"\n",
        );
        write(&homes.grok.join("hooks/on-stop.sh"), "true\n");
        write(&homes.grok.join("workflows/demo.rhai"), "let meta = #{};\n");
        write(
            &homes.claude.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: review the crate\n---\n",
        );
        write(
            &homes.claude.join("commands/shepherd.md"),
            "---\nname: shepherd\ndescription: drive a pr to green\n---\nOpen the PR and wait for CI.\n",
        );
        write(&homes.claude.join("commands/README.md"), "# commands\n");
        write(
            &homes.claude.join("commands/nested/SKILL.md"),
            "---\nname: nested\ndescription: not a command file\n---\n",
        );
        write(
            &dir.path().join(".claude.json"),
            "{\"mcpServers\":{\"linear\":{\"command\":\"n\"}}}\n",
        );
        write(
            &homes.gemini.join("tmp/proj/chats/session-learn.json"),
            &json!({
                "sessionId": "55555555-5555-4555-8555-555555555555",
                "cwd": "/Users/test/app",
                "lastUpdated": now,
                "messages": [
                    {"type":"user","content": phrase},
                    {"type":"gemini","content":"ok","toolName":"Read"}
                ]
            })
            .to_string(),
        );
        (dir, homes)
    }

    #[test]
    fn estimate_math_matches_workflow_arithmetic() {
        let empty = estimate_cost(0, 10);
        assert_eq!(empty.agents, 0);
        let small = estimate_cost(25, 10);
        assert_eq!(small.mappers, 3);
        assert!(small.tokens > 0);
        let per_trace = estimate_cost(2, 1);
        assert!(per_trace.tokens > estimate_cost(2, 10).tokens);
    }

    #[test]
    fn estimate_recommends_all_above_one_hundred_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let now = Utc::now().to_rfc3339();
        for i in 0..101 {
            grok_session(
                &homes,
                &format!("01bulk{i:04}grok000000000"),
                "/Users/test/app",
                "main",
                &now,
                &user_turn("bulk session prompt that is long enough to keep"),
            );
        }
        let estimate = collect(
            &homes,
            &CollectParams {
                estimate: true,
                out: Some(dir.path().join("many")),
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Estimate(report) = estimate else {
            panic!("estimate");
        };
        assert_eq!(report.recommended, "all");
        assert!(report.windows["all"].estimate.sessions >= 101);
        assert!(report.windows["14d"].estimate.sessions >= 101);
    }

    #[test]
    fn estimate_recommends_all_after_a_limited_run() {
        let (dir, homes) = fixture();
        let limited = collect(
            &homes,
            &CollectParams {
                limit: 1,
                out: Some(dir.path().join("limited")),
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Collect(report) = limited else {
            panic!("collect");
        };
        set(
            &homes,
            &StateUpdate {
                run_dir: report.run_dir,
                status: "done".into(),
                mode: Some("report".into()),
                scope: Some("quick".into()),
                note: None,
            },
        )
        .unwrap();
        let estimate = collect(
            &homes,
            &CollectParams {
                estimate: true,
                out: Some(dir.path().join("after-limit")),
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Estimate(report) = estimate else {
            panic!("estimate");
        };
        assert_eq!(report.recommended, "all");
        let bogus = dir.path().join("bogus");
        fs::create_dir_all(&bogus).unwrap();
        fs::write(bogus.join("manifest.json"), "not-json").unwrap();
        let state_path = homes.learn_dir().join("state.json");
        let mut raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        raw["last_completed_dir"] = json!(bogus.display().to_string());
        fs::write(&state_path, raw.to_string()).unwrap();
        let again = collect(
            &homes,
            &CollectParams {
                estimate: true,
                out: Some(dir.path().join("after-bogus")),
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Estimate(report) = again else {
            panic!("estimate");
        };
        assert_eq!(report.recommended, "all");
        let windowed = dir.path().join("days-run");
        fs::create_dir_all(&windowed).unwrap();
        write(
            &windowed.join("manifest.json"),
            &json!({"params":{"days":14,"limit":0}}).to_string(),
        );
        raw["last_completed_dir"] = json!(windowed.display().to_string());
        fs::write(&state_path, raw.to_string()).unwrap();
        let days = collect(
            &homes,
            &CollectParams {
                estimate: true,
                out: Some(dir.path().join("after-days")),
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Estimate(report) = days else {
            panic!("estimate");
        };
        assert_eq!(report.recommended, "all");
    }

    #[test]
    fn collect_errors_when_run_dir_cannot_be_created() {
        let (dir, homes) = fixture();
        let blocked = dir.path().join("blocked");
        fs::write(&blocked, "file").unwrap();
        assert!(
            collect(
                &homes,
                &CollectParams {
                    out: Some(blocked),
                    ..CollectParams::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn collect_errors_when_output_files_are_directories() {
        let (dir, homes) = fixture();
        let out = dir.path().join("readonly");
        fs::create_dir_all(out.join("sessions")).unwrap();
        for name in [
            "estimate.json",
            "surfaces.json",
            "usage.json",
            "phrases.json",
            "manifest.json",
        ] {
            fs::create_dir_all(out.join(name)).unwrap();
        }
        fs::write(out.join("map"), "not-a-dir").unwrap();
        assert!(
            collect(
                &homes,
                &CollectParams {
                    estimate: true,
                    out: Some(out.clone()),
                    ..CollectParams::default()
                }
            )
            .is_err()
        );
        assert!(
            collect(
                &homes,
                &CollectParams {
                    out: Some(out),
                    ..CollectParams::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn collect_keeps_human_sessions_across_agents_and_drops_noise() {
        let (dir, homes) = fixture();
        let estimate = collect(
            &homes,
            &CollectParams {
                estimate: true,
                batch: 10,
                out: Some(dir.path().join("estimate")),
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Estimate(report) = estimate else {
            panic!("expected estimate");
        };
        assert_eq!(report.recommended, "all");
        assert!(report.windows["all"].estimate.sessions >= 3, "{report:?}");

        let collected = collect(
            &homes,
            &CollectParams {
                out: Some(dir.path().join("run")),
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Collect(report) = collected else {
            panic!("expected collect");
        };
        assert!(report.kept >= 3, "{report:?}");
        assert!(report.shards >= 1, "{report:?}");
        assert_eq!(report.batch, super::DEFAULT_BATCH);
        assert!(report.run_dir.join("plan.json").is_file());
        assert!(report.run_dir.join("skill/signals.md").is_file());
        assert!(report.dropped.get("headless").copied().unwrap_or(0) >= 1);
        assert!(report.dropped.get("smoke_test").copied().unwrap_or(0) >= 1);
        assert!(report.dropped.get("excluded_cwd").copied().unwrap_or(0) >= 1);
        let manifest: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(report.run_dir.join("manifest.json")).unwrap(),
        )
        .unwrap();
        assert!(manifest["sessions_kept"].as_u64().unwrap() >= 3);
        let phrases: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(report.run_dir.join("phrases.json")).unwrap())
                .unwrap();
        assert!(
            !phrases["exact_prompts"].as_array().unwrap().is_empty(),
            "{phrases}"
        );
        let usage: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(report.run_dir.join("usage.json")).unwrap())
                .unwrap();
        let unused = usage["unused_loaded"].as_array().unwrap();
        assert!(
            unused.iter().any(|item| item["name"] == "unused"),
            "{usage}"
        );
        let surfaces: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(report.run_dir.join("surfaces.json")).unwrap(),
        )
        .unwrap();
        assert!(
            surfaces["mcp_servers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["name"] == "magents"),
            "{surfaces}"
        );
        let state = get(&homes).unwrap();
        assert!(pending(&state));
        assert_eq!(state.pending.as_ref().unwrap().status, "collected");
        assert!(
            surfaces["mcp_servers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["name"] == "cursor-extra"),
            "{surfaces}"
        );
        assert!(
            surfaces["skills"]
                .as_array()
                .unwrap()
                .iter()
                .any(|skill| skill["name"] == "bundled-one" && skill["protected"] == "bundled"),
            "{surfaces}"
        );
        assert!(
            surfaces["skills"]
                .as_array()
                .unwrap()
                .iter()
                .any(|skill| skill["name"] == "shepherd" && skill["source"] == "command"),
            "{surfaces}"
        );
    }

    #[test]
    fn plan_shards_sessions_and_errors_without_a_run() {
        let (dir, homes) = fixture();
        let collected = collect(
            &homes,
            &CollectParams {
                out: Some(dir.path().join("run")),
                batch: 1,
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Collect(report) = collected else {
            panic!("collect");
        };
        assert_eq!(report.batch, 1);
        assert_eq!(report.shards, report.kept);
        let planned = plan(&report.run_dir, 2).unwrap();
        assert_eq!(planned.batch, 2);
        assert!(!planned.shards.is_empty());
        assert!(!planned.reduce.is_empty());
        assert!(report.run_dir.join(&planned.shards[0].prompt).is_file());
        let empty = dir.path().join("empty-run");
        fs::create_dir_all(empty.join("sessions")).unwrap();
        let zero = plan(&empty, 0).unwrap();
        assert!(zero.shards.is_empty());
        assert!(zero.reduce.is_empty());
        assert_eq!(zero.batch, super::DEFAULT_BATCH);
        assert!(plan(&dir.path().join("missing-run"), 10).is_err());
        let many = dir.path().join("many-shards");
        fs::create_dir_all(many.join("sessions")).unwrap();
        for index in 0..11 {
            fs::write(many.join("sessions").join(format!("{index:04}.json")), "{}").unwrap();
        }
        let deep = plan(&many, 1).unwrap();
        assert_eq!(deep.shards.len(), 11);
        assert!(deep.reduce.len() >= 2);
        let blocked = dir.path().join("blocked-plan");
        fs::create_dir_all(blocked.join("sessions")).unwrap();
        fs::create_dir_all(blocked.join("plan.json")).unwrap();
        assert!(plan(&blocked, 1).is_err());
        let skill_block = dir.path().join("skill-block");
        fs::create_dir_all(skill_block.join("sessions")).unwrap();
        fs::create_dir_all(skill_block.join("skill").join("signals.md")).unwrap();
        assert!(plan(&skill_block, 1).is_err());
        let report_block = dir.path().join("report-block");
        fs::create_dir_all(report_block.join("sessions")).unwrap();
        fs::create_dir_all(report_block.join("skill")).unwrap();
        fs::write(report_block.join("skill").join("signals.md"), "ok").unwrap();
        fs::create_dir_all(report_block.join("skill").join("report-format.md")).unwrap();
        assert!(plan(&report_block, 1).is_err());
        let prompt_block = dir.path().join("prompt-block");
        fs::create_dir_all(prompt_block.join("sessions")).unwrap();
        fs::write(prompt_block.join("sessions").join("0000.json"), "{}").unwrap();
        fs::create_dir_all(
            prompt_block
                .join("map")
                .join("prompts")
                .join("0000-0000.md"),
        )
        .unwrap();
        assert!(plan(&prompt_block, 1).is_err());
    }

    #[test]
    fn collect_respects_agent_limit_cwd_and_headless() {
        let (dir, homes) = fixture();
        let only_grok = collect(
            &homes,
            &CollectParams {
                agent: Some(crate::model::Agent::Grok),
                limit: 1,
                min_turns: 1,
                include_subagents: true,
                out: Some(dir.path().join("one")),
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Collect(report) = only_grok else {
            panic!("collect");
        };
        assert_eq!(report.kept, 1);
        let headless = collect(
            &homes,
            &CollectParams {
                include_headless: true,
                session_ids: vec!["01headlessgrok0000000000".into()],
                out: Some(dir.path().join("headless")),
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Collect(report) = headless else {
            panic!("collect");
        };
        assert_eq!(report.kept, 1);
        let outside = collect(
            &homes,
            &CollectParams {
                cwd: vec!["/Users/other".into()],
                out: Some(dir.path().join("outside")),
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Collect(report) = outside else {
            panic!("collect");
        };
        assert_eq!(report.kept, 0);
        assert!(report.dropped.get("outside_cwd").copied().unwrap_or(0) >= 1);
    }

    #[test]
    fn collect_since_last_and_days_and_state_roundtrip() {
        let (dir, homes) = fixture();
        let run = dir.path().join("old");
        fs::create_dir_all(&run).unwrap();
        write(
            &run.join("manifest.json"),
            &json!({"params":{"days":0,"limit":0,"since_last":false}}).to_string(),
        );
        set(
            &homes,
            &StateUpdate {
                run_dir: run,
                status: "done".into(),
                mode: None,
                scope: None,
                note: None,
            },
        )
        .unwrap();
        let state_path = homes.learn_dir().join("state.json");
        let mut raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        raw["last_completed_at"] = json!("2021-01-01T00:00:00Z");
        fs::write(&state_path, raw.to_string()).unwrap();
        let priced = collect(
            &homes,
            &CollectParams {
                estimate: true,
                out: Some(dir.path().join("est-since")),
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Estimate(report) = priced else {
            panic!("estimate");
        };
        assert_eq!(report.recommended, "since_last");
        let since = collect(
            &homes,
            &CollectParams {
                since_last: true,
                out: Some(dir.path().join("since")),
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Collect(report) = since else {
            panic!("collect");
        };
        assert!(report.kept >= 1);
        let old = collect(
            &homes,
            &CollectParams {
                days: 1,
                out: Some(dir.path().join("days")),
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Collect(_) = old else {
            panic!("collect");
        };
        decide(
            &homes,
            &Decision {
                run_dir: report.run_dir.display().to_string(),
                id: "A1".into(),
                kind: "skill".into(),
                action: "delete".into(),
                target: "unused".into(),
                path: homes.grok.join("skills/unused").display().to_string(),
                decision: "deferred".into(),
                undo: None,
            },
        )
        .unwrap();
        let again = collect(
            &homes,
            &CollectParams {
                out: Some(dir.path().join("again")),
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Collect(report) = again else {
            panic!("collect");
        };
        assert!(report.run_dir.join("decisions.jsonl").is_file());
        let bad = collect(
            &homes,
            &CollectParams {
                drop_patterns: vec!["[".into()],
                out: Some(dir.path().join("bad-re")),
                ..CollectParams::default()
            },
        );
        assert!(bad.is_err());
        let dropped_turns = collect(
            &homes,
            &CollectParams {
                drop_patterns: vec!["always run cargo test".into()],
                out: Some(dir.path().join("drop-turns")),
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Collect(_) = dropped_turns else {
            panic!("collect");
        };
    }

    #[test]
    fn project_skills_are_inventoried_when_cwd_exists() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let project = dir.path().join("repo");
        write(&project.join(".git").join("HEAD"), "ref: refs/heads/main\n");
        write(
            &project.join(".grok/skills/local/SKILL.md"),
            "---\nname: local\ndescription: project skill\n---\n",
        );
        write(
            &project.join(".claude/commands/ship.md"),
            "---\nname: ship\ndescription: ship the branch\n---\n",
        );
        write(
            &project.join("nested/.grok/skills/local/SKILL.md"),
            "---\nname: local\ndescription: duplicate project skill\n---\n",
        );
        grok_session(
            &homes,
            "01projectgrok00000000000",
            &project.join("nested").display().to_string(),
            "main",
            &Utc::now().to_rfc3339(),
            &user_turn("work the project skill path for inventory"),
        );
        let collected = collect(
            &homes,
            &CollectParams {
                exclude_cwd: Some(Vec::new()),
                out: Some(dir.path().join("proj")),
                ..CollectParams::default()
            },
        )
        .unwrap();
        let CollectOutcome::Collect(report) = collected else {
            panic!("collect");
        };
        let surfaces: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(report.run_dir.join("surfaces.json")).unwrap(),
        )
        .unwrap();
        assert!(
            surfaces["skills"]
                .as_array()
                .unwrap()
                .iter()
                .any(|skill| skill["name"] == "local" && skill["source"] == "project"),
            "{surfaces}"
        );
        assert!(
            surfaces["skills"]
                .as_array()
                .unwrap()
                .iter()
                .any(|skill| skill["name"] == "ship" && skill["source"] == "command"),
            "{surfaces}"
        );
    }
}

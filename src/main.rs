use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use magents::discover::{ListFilter, identify, list_sessions, resolve};
use magents::homes::Homes;
use magents::install::{HostInstall, HostStatus, InstallEvent};
use magents::mailbox::InboxQuery;
use magents::model::{
    AckReport, Agent, AwaitReport, Caller, Digest, FilesTouched, Identity, InboxReport,
    MemoryCreated, MemoryHit, MemoryRead, Note, SearchHit, SendReport, Session, StopReport,
    Transcript,
};
use magents::{handoff, mailbox, memory, notes, spawn, transcript};
use serde::Serialize;
use std::io::{IsTerminal, Read, Write};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "magents",
    version,
    about = "Mates + agents: shared session bus for Claude, Codex, Copilot, Cursor, Gemini, Grok, and OpenCode"
)]
struct Cli {
    /// Stable machine-readable output (`json` or `text`)
    #[arg(long, global = true, value_enum, value_name = "FORMAT")]
    output: Option<OutputFormat>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

fn wants_json(output: Option<OutputFormat>) -> bool {
    match output {
        Some(OutputFormat::Json) => true,
        Some(OutputFormat::Text) => false,
        None => !std::io::stdout().is_terminal(),
    }
}

#[derive(Subcommand)]
enum Command {
    /// Run the stdio MCP server
    Mcp,
    /// List sessions across Claude, Codex, Copilot, Cursor, Gemini, Grok, and OpenCode
    List {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        query: Option<String>,
        #[arg(long)]
        live: bool,
        #[arg(long)]
        archived: bool,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        branch: Option<String>,
    },
    /// Look up one session
    Get { reference: String },
    /// Read a compact transcript
    Read {
        reference: String,
        #[arg(short = 'n', long, default_value_t = 40)]
        limit: usize,
    },
    /// Search transcripts
    Search {
        query: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
    },
    /// Search Claude, Codex, and Grok memory markdown
    SearchMemories {
        query: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
    },
    /// Write a note into Claude, Codex, or Grok first-party memory
    CreateMemory {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        content: String,
    },
    /// Read one Claude, Codex, or Grok memory file
    ReadMemory {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        path: Option<String>,
        #[arg(short = 'n', long, default_value_t = 8000)]
        limit: usize,
    },
    /// Compact inert session summary
    Digest {
        reference: String,
        #[arg(short = 'n', long, default_value_t = 12)]
        limit: usize,
    },
    /// File paths a session touched
    Files { reference: String },
    /// Start a new headless persisted session for independent work
    Spawn {
        agent: String,
        /// Read the complete prompt from PATH; use '-' for stdin
        #[arg(long, value_name = "PATH", default_value = "-")]
        prompt_file: PathBuf,
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Queue a message for another session
    Send { to: String, message: String },
    /// Reply to the latest inbox mail, or a mail_id
    Reply {
        message: String,
        #[arg(long)]
        mail_id: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        agent: Option<String>,
    },
    /// Stop a magents-supervised session
    Stop { reference: String },
    /// Hand this work to another live agent with compact state
    Handoff {
        to: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Show the mailbox for this (or a given) session
    Inbox {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        unread: bool,
    },
    /// Mark inbox mail as read
    Ack {
        #[arg(long)]
        through: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        agent: Option<String>,
    },
    /// Wait briefly for new inbox mail
    AwaitReply {
        #[arg(long)]
        from: Option<String>,
        #[arg(long, default_value_t = 5)]
        timeout: u32,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        agent: Option<String>,
    },
    /// Read the magents-owned shared note for a cwd
    GetNote {
        #[arg(long)]
        cwd: Option<String>,
    },
    /// Write the magents-owned shared note for a cwd
    PutNote {
        content: String,
        #[arg(long)]
        cwd: Option<String>,
    },
    /// Who this process would run as
    Whoami,
    /// Register magents as an MCP server with local agents
    Install {
        #[arg(long)]
        claude: bool,
        #[arg(long)]
        grok: bool,
        #[arg(long)]
        codex: bool,
        #[arg(long)]
        cursor: bool,
        #[arg(long)]
        opencode: bool,
        #[arg(long)]
        gemini: bool,
        #[arg(long)]
        copilot: bool,
        #[arg(long)]
        all: bool,
    },
    #[command(name = "__supervise", hide = true)]
    Supervise {
        agent: String,
        #[arg(long)]
        cwd: PathBuf,
        #[arg(long)]
        session: Option<String>,
    },
}

fn parse_agent(value: &str) -> anyhow::Result<Agent> {
    Agent::parse(value).ok_or_else(|| anyhow::anyhow!("unknown agent: {value}"))
}

fn read_prompt(path: &Path) -> anyhow::Result<String> {
    let prompt = if path == Path::new("-") {
        let mut prompt = String::new();
        std::io::stdin().read_to_string(&mut prompt)?;
        prompt
    } else {
        std::fs::read_to_string(path).map_err(|error| {
            anyhow::anyhow!("failed to read prompt file {}: {error}", path.display())
        })?
    };

    if prompt.trim().is_empty() {
        anyhow::bail!("prompt must not be empty");
    }
    Ok(prompt)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("magents=info".parse()?),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let json = wants_json(cli.output);
    match try_run(cli.command, json).await {
        Ok(()) => Ok(()),
        Err(error) if Style::new().stderr_tty && !json => {
            let style = Style::new();
            eprintln!("  {} {:<8}  {error}", style.fail(), "error");
            std::process::exit(1);
        }
        Err(error) => Err(error),
    }
}

async fn try_run(command: Option<Command>, json: bool) -> anyhow::Result<()> {
    match command {
        Some(Command::Mcp) => {
            magents::mcp::serve().await?;
        }
        None if !std::io::stdin().is_terminal() => {
            magents::mcp::serve().await?;
        }
        None => {
            let _ = Cli::try_parse_from(["magents", "--help"]);
        }
        Some(Command::List {
            agent,
            query,
            live,
            archived,
            limit,
            cwd,
            branch,
        }) => {
            let sessions: Vec<Session> = with_spin(json, "list", || {
                list_sessions(
                    &Homes::from_env(),
                    &ListFilter {
                        agent: agent.as_deref().and_then(Agent::parse),
                        query,
                        live_only: live,
                        include_archived: archived,
                        limit,
                        cwd,
                        branch,
                    },
                )
            })?;
            print_value(json, &sessions, |style| format_sessions(style, &sessions))?;
        }
        Some(Command::Get { reference }) => {
            let session = resolve(&Homes::from_env(), &reference)?;
            print_value(json, &session, |style| format_session_card(style, &session))?;
        }
        Some(Command::Read { reference, limit }) => {
            let transcript = transcript::read_transcript(&Homes::from_env(), &reference, limit)?;
            print_value(json, &transcript, |style| {
                format_transcript(style, &transcript)
            })?;
        }
        Some(Command::Search {
            query,
            agent,
            limit,
        }) => {
            let hits: Vec<SearchHit> = with_spin(json, "search", || {
                transcript::search_transcripts(
                    &Homes::from_env(),
                    &query,
                    agent.as_deref().and_then(Agent::parse),
                    false,
                    limit,
                )
            })?;
            print_value(json, &hits, |style| format_search_hits(style, &hits))?;
        }
        Some(Command::SearchMemories {
            query,
            agent,
            limit,
        }) => {
            let hits: Vec<MemoryHit> = with_spin(json, "search", || {
                memory::search_memories(
                    &Homes::from_env(),
                    &query,
                    agent.as_deref().and_then(Agent::parse),
                    limit,
                )
            })?;
            print_value(json, &hits, |style| format_memory_hits(style, &hits))?;
        }
        Some(Command::CreateMemory {
            agent,
            file,
            project,
            cwd,
            content,
        }) => {
            let created = memory::create_memory(
                &Homes::from_env(),
                parse_agent(&agent)?,
                &content,
                file.as_deref(),
                project.as_deref(),
                cwd.as_deref(),
            )?;
            print_value(json, &created, |style| {
                format_memory_created(style, &created)
            })?;
        }
        Some(Command::ReadMemory {
            agent,
            file,
            project,
            cwd,
            path,
            limit,
        }) => {
            let read = memory::read_memory(
                &Homes::from_env(),
                parse_agent(&agent)?,
                file.as_deref(),
                project.as_deref(),
                cwd.as_deref(),
                path.as_deref(),
                limit,
            )?;
            print_value(json, &read, |style| format_memory_read(style, &read))?;
        }
        Some(Command::Digest { reference, limit }) => {
            let digest = transcript::session_digest(&Homes::from_env(), &reference, limit)?;
            print_value(json, &digest, |style| format_digest(style, &digest))?;
        }
        Some(Command::Files { reference }) => {
            let files = transcript::files_touched(&Homes::from_env(), &reference)?;
            print_value(json, &files, |style| format_files(style, &files))?;
        }
        Some(Command::Spawn {
            agent,
            prompt_file,
            cwd,
        }) => {
            let agent = parse_agent(&agent)?;
            let prompt = read_prompt(&prompt_file)?;
            let report = with_spin(json, "spawn", || {
                magents::spawn::run(&Homes::from_env(), agent, &prompt, cwd.as_deref())
            })?;
            print_value(json, &report, |style| format_spawn(style, &report))?;
        }
        Some(Command::Send { to, message }) => {
            let report = with_spin(json, "send", || {
                mailbox::send(&Homes::from_env(), &Caller::from_env(), &to, &message)
            })?;
            print_value(json, &report, |style| format_send(style, &report))?;
        }
        Some(Command::Reply {
            message,
            mail_id,
            session,
            agent,
        }) => {
            let report = with_spin(json, "reply", || {
                mailbox::reply(
                    &Homes::from_env(),
                    &Caller::from_env(),
                    &message,
                    mail_id.as_deref(),
                    session.as_deref(),
                    agent.as_deref().and_then(Agent::parse),
                )
            })?;
            print_value(json, &report, |style| format_send(style, &report))?;
        }
        Some(Command::Stop { reference }) => {
            let report = with_spin(json, "stop", || spawn::stop(&Homes::from_env(), &reference))?;
            print_value(json, &report, |style| format_stop(style, &report))?;
        }
        Some(Command::Handoff { to, reason }) => {
            let report = with_spin(json, "handoff", || {
                handoff::run(&Homes::from_env(), to.as_deref(), reason.as_deref())
            })?;
            print_value(json, &report, |style| format_handoff(style, &report))?;
        }
        Some(Command::Inbox {
            session,
            agent,
            since,
            unread,
        }) => {
            let mail = mailbox::inbox(
                &Homes::from_env(),
                &Caller::from_env(),
                InboxQuery {
                    session_id: session,
                    agent: agent.as_deref().and_then(Agent::parse),
                    since,
                    unread_only: unread,
                },
            )?;
            print_value(json, &mail, |style| format_inbox(style, &mail))?;
        }
        Some(Command::Ack {
            through,
            session,
            agent,
        }) => {
            let report = mailbox::ack(
                &Homes::from_env(),
                &Caller::from_env(),
                through.as_deref(),
                session.as_deref(),
                agent.as_deref().and_then(Agent::parse),
            )?;
            print_value(json, &report, |style| format_ack(style, &report))?;
        }
        Some(Command::AwaitReply {
            from,
            timeout,
            session,
            agent,
        }) => {
            let report = with_spin(json, "await", || {
                mailbox::await_reply(
                    &Homes::from_env(),
                    &Caller::from_env(),
                    from.as_deref(),
                    Some(timeout),
                    session.as_deref(),
                    agent.as_deref().and_then(Agent::parse),
                )
            })?;
            print_value(json, &report, |style| format_await(style, &report))?;
        }
        Some(Command::GetNote { cwd }) => {
            let note = notes::get_note(&Homes::from_env(), cwd.as_deref(), &Caller::from_env())?;
            print_value(json, &note, |style| format_note(style, &note, false))?;
        }
        Some(Command::PutNote { content, cwd }) => {
            let note = notes::put_note(
                &Homes::from_env(),
                &content,
                cwd.as_deref(),
                &Caller::from_env(),
            )?;
            print_value(json, &note, |style| format_note(style, &note, true))?;
        }
        Some(Command::Whoami) => {
            let identity = identify(&Homes::from_env());
            print_value(json, &identity, |style| format_whoami(style, &identity))?;
        }
        Some(Command::Install {
            claude,
            grok,
            codex,
            cursor,
            opencode,
            gemini,
            copilot,
            all,
        }) => {
            let spec = magents::install::InstallSpec {
                claude: claude || all,
                grok: grok || all,
                codex: codex || all,
                cursor: cursor || all,
                opencode: opencode || all,
                gemini: gemini || all,
                copilot: copilot || all,
                skip_missing: all,
            };
            if json {
                let notes = magents::install::install_spec(spec)?;
                print_value(true, &notes, |_| String::new())?;
            } else {
                let mut progress = InstallProgress::new();
                let result = magents::install::install_spec_with(spec, |event| match event {
                    InstallEvent::Started { host } => progress.start(host),
                    InstallEvent::Finished { result } => progress.finish(&result),
                });
                match result {
                    Ok(_) => {}
                    Err(error) => {
                        progress.fail(&error);
                        std::process::exit(1);
                    }
                }
            }
        }
        Some(Command::Supervise {
            agent,
            cwd,
            session,
        }) => {
            let agent = parse_agent(&agent)?;
            magents::runtime::supervise(&Homes::from_env(), agent, &cwd, session.as_deref())?;
        }
    }
    Ok(())
}

struct Style {
    stderr_tty: bool,
    color: bool,
}

impl Style {
    fn new() -> Self {
        let stdout_tty = std::io::stdout().is_terminal();
        let stderr_tty = std::io::stderr().is_terminal();
        Self {
            stderr_tty,
            color: (stdout_tty || stderr_tty) && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        paint(self.color, code, text)
    }

    fn dim(&self, text: &str) -> String {
        self.paint("2", text)
    }

    fn ok(&self) -> String {
        self.paint("32", "✓")
    }

    fn live(&self) -> String {
        self.paint("32", "●")
    }

    fn idle(&self) -> String {
        self.paint("2", "○")
    }

    fn skip(&self) -> String {
        self.paint("2", "–")
    }

    fn fail(&self) -> String {
        self.paint("31", "✗")
    }
}

fn paint(color: bool, code: &str, text: &str) -> String {
    if color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

fn print_value<T: Serialize>(
    json: bool,
    value: &T,
    human: impl FnOnce(&Style) -> String,
) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
        return Ok(());
    }
    let text = human(&Style::new());
    if text.ends_with('\n') {
        print!("{text}");
    } else {
        println!("{text}");
    }
    Ok(())
}

fn with_spin<T, E>(
    json: bool,
    label: &str,
    work: impl FnOnce() -> Result<T, E>,
) -> anyhow::Result<T>
where
    E: Into<anyhow::Error>,
{
    let mut spin = Spinner::new();
    if !json {
        spin.start(label);
    }
    match work() {
        Ok(value) => {
            spin.stop();
            Ok(value)
        }
        Err(error) => {
            spin.stop();
            Err(error.into())
        }
    }
}

struct Spinner {
    tty: bool,
    color: bool,
    hidden_cursor: bool,
    stop: Option<(Arc<AtomicBool>, thread::JoinHandle<()>)>,
}

impl Spinner {
    fn new() -> Self {
        let tty = std::io::stderr().is_terminal();
        Self {
            tty,
            color: tty && std::env::var_os("NO_COLOR").is_none(),
            hidden_cursor: false,
            stop: None,
        }
    }

    fn start(&mut self, label: &str) {
        self.stop_spin();
        if !self.tty {
            return;
        }
        if !self.hidden_cursor {
            eprint!("\x1b[?25l");
            self.hidden_cursor = true;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let color = self.color;
        let label = label.to_string();
        let handle = thread::spawn(move || {
            const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut i = 0;
            while !flag.load(Ordering::Relaxed) {
                let frame = paint(color, "36", FRAMES[i % FRAMES.len()]);
                eprint!("\r\x1b[K  {frame} {label}");
                let _ = std::io::stderr().flush();
                i += 1;
                thread::sleep(Duration::from_millis(80));
            }
        });
        self.stop = Some((stop, handle));
    }

    fn stop(&mut self) {
        self.stop_spin();
        if self.tty {
            eprint!("\r\x1b[K");
            let _ = std::io::stderr().flush();
        }
    }

    fn stop_spin(&mut self) {
        if let Some((stop, handle)) = self.stop.take() {
            stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop_spin();
        if self.hidden_cursor {
            eprint!("\x1b[?25h");
            let _ = std::io::stderr().flush();
        }
    }
}

fn row(mark: &str, label: &str, primary: &str) -> String {
    format!("  {mark} {label:<8}  {primary}")
}

fn sub(text: &str) -> String {
    format!("              {text}")
}

fn clip(text: &str, max: usize) -> String {
    let trimmed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.chars().count() <= max {
        return trimmed;
    }
    let mut clipped: String = trimmed.chars().take(max.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

fn short_path(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home = home.to_string_lossy();
        if let Some(rest) = path.strip_prefix(home.as_ref()) {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

fn relative_time(time: DateTime<Utc>) -> String {
    let secs = (Utc::now() - time).num_seconds().max(0);
    if secs < 10 {
        "just now".into()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

fn short_id(id: &str) -> &str {
    match id.char_indices().nth(8) {
        Some((index, _)) => &id[..index],
        None => id,
    }
}

fn session_mark(style: &Style, session: &Session) -> String {
    if session.live {
        style.live()
    } else if session.archived {
        style.skip()
    } else {
        style.idle()
    }
}

fn session_meta(session: &Session) -> String {
    let mut parts = vec![short_id(&session.session_id).to_string()];
    if let Some(cwd) = session.cwd.as_deref() {
        parts.push(short_path(cwd));
    }
    if let Some(branch) = session.branch.as_deref() {
        parts.push(branch.to_string());
    }
    if let Some(time) = session.last_activity_at {
        parts.push(relative_time(time));
    }
    if session.archived {
        parts.push("archived".into());
    }
    parts.join("  ")
}

fn format_sessions(style: &Style, sessions: &[Session]) -> String {
    if sessions.is_empty() {
        return format!("{}\n", row(&style.skip(), "list", "no sessions"));
    }
    let mut lines = Vec::new();
    for session in sessions {
        lines.push(row(
            &session_mark(style, session),
            session.agent.as_str(),
            &session.label(),
        ));
        lines.push(style.dim(&sub(&session_meta(session))));
        lines.push(String::new());
    }
    lines.join("\n")
}

fn format_session_card(style: &Style, session: &Session) -> String {
    let mut lines = vec![row(
        &session_mark(style, session),
        session.agent.as_str(),
        &session.label(),
    )];
    lines.push(style.dim(&sub(&format!("id       {}", session.session_id))));
    if let Some(cwd) = session.cwd.as_deref() {
        lines.push(style.dim(&sub(&format!("cwd      {}", short_path(cwd)))));
    }
    if let Some(branch) = session.branch.as_deref() {
        lines.push(style.dim(&sub(&format!("branch   {branch}"))));
    }
    if let Some(origin) = session.origin.as_deref() {
        lines.push(style.dim(&sub(&format!("origin   {origin}"))));
    }
    if let Some(model) = session.model.as_deref() {
        lines.push(style.dim(&sub(&format!("model    {model}"))));
    }
    if let Some(time) = session.last_activity_at {
        lines.push(style.dim(&sub(&format!("active   {}", relative_time(time)))));
    }
    if session.live {
        lines.push(style.dim(&sub("live     yes")));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn format_turns(style: &Style, turns: &[magents::model::Turn]) -> Vec<String> {
    let mut lines = Vec::new();
    for turn in turns {
        let role = match turn.role.as_str() {
            "assistant" => "assist",
            other => other,
        };
        lines.push(format!(
            "  {}  {}",
            style.dim(&format!("{role:<8}")),
            clip(&turn.text, 100)
        ));
        if !turn.tools.is_empty() {
            lines.push(style.dim(&sub(&format!("tools    {}", turn.tools.join(", ")))));
        }
    }
    lines
}

fn format_transcript(style: &Style, transcript: &Transcript) -> String {
    let mut lines = vec![row(
        &session_mark(style, &transcript.session),
        transcript.session.agent.as_str(),
        &transcript.session.label(),
    )];
    lines.push(style.dim(&sub(&format!(
        "{} turns  showing {}",
        transcript.turn_count, transcript.returned_turns
    ))));
    if let Some(request) = transcript.last_user_request.as_deref() {
        lines.push(style.dim(&sub(&format!("asked    {}", clip(request, 80)))));
    }
    lines.push(String::new());
    lines.extend(format_turns(style, &transcript.turns));
    lines.push(String::new());
    lines.join("\n")
}

fn format_digest(style: &Style, digest: &Digest) -> String {
    let mut lines = vec![row(
        &session_mark(style, &digest.session),
        digest.session.agent.as_str(),
        &digest.session.label(),
    )];
    if let Some(request) = digest.last_user_request.as_deref() {
        lines.push(style.dim(&sub(&format!("asked    {}", clip(request, 80)))));
    }
    if let Some(action) = digest.last_assistant_action.as_deref() {
        lines.push(style.dim(&sub(&format!("did      {}", clip(action, 80)))));
    }
    lines.push(String::new());
    lines.extend(format_turns(style, &digest.turns));
    lines.push(String::new());
    lines.join("\n")
}

fn format_files(style: &Style, files: &FilesTouched) -> String {
    let mut lines = vec![row(
        &session_mark(style, &files.session),
        files.session.agent.as_str(),
        &files.session.label(),
    )];
    if files.files.is_empty() {
        lines.push(style.dim(&sub("no files")));
    } else {
        for file in &files.files {
            lines.push(sub(&short_path(file)));
        }
    }
    lines.push(String::new());
    lines.join("\n")
}

fn format_search_hits(style: &Style, hits: &[SearchHit]) -> String {
    if hits.is_empty() {
        return format!("{}\n", row(&style.skip(), "search", "no matches"));
    }
    let mut lines = Vec::new();
    for hit in hits {
        lines.push(row(
            &session_mark(style, &hit.session),
            hit.session.agent.as_str(),
            &hit.session.label(),
        ));
        lines.push(style.dim(&sub(&format!(
            "{} matches  {}",
            hit.matches,
            clip(&hit.snippet, 80)
        ))));
        lines.push(String::new());
    }
    lines.join("\n")
}

fn format_memory_hits(style: &Style, hits: &[MemoryHit]) -> String {
    if hits.is_empty() {
        return format!("{}\n", row(&style.skip(), "memory", "no matches"));
    }
    let mut lines = Vec::new();
    for hit in hits {
        lines.push(row(&style.idle(), hit.agent.as_str(), &hit.file));
        let mut meta = Vec::new();
        if let Some(project) = hit.project.as_deref() {
            meta.push(project.to_string());
        }
        meta.push(format!("{} matches", hit.matches));
        meta.push(clip(&hit.snippet, 70));
        lines.push(style.dim(&sub(&meta.join("  "))));
        lines.push(String::new());
    }
    lines.join("\n")
}

fn format_memory_created(style: &Style, created: &MemoryCreated) -> String {
    let mut lines = vec![row(
        &style.ok(),
        "memory",
        &format!("{}  {}", created.agent, created.file),
    )];
    if let Some(project) = created.project.as_deref() {
        lines.push(style.dim(&sub(&format!(
            "{project}  {}",
            short_path(&created.path.display().to_string())
        ))));
    } else {
        lines.push(style.dim(&sub(&short_path(&created.path.display().to_string()))));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn format_memory_read(style: &Style, read: &MemoryRead) -> String {
    let mut lines = vec![row(&style.idle(), read.agent.as_str(), &read.file)];
    if let Some(project) = read.project.as_deref() {
        lines.push(style.dim(&sub(project)));
    }
    lines.push(String::new());
    lines.push(read.content.trim_end().to_string());
    if read.truncated {
        lines.push(style.dim(&sub("truncated")));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn format_spawn(style: &Style, report: &spawn::Report) -> String {
    let mut lines = vec![row(&style.ok(), "spawn", "accepted  starting")];
    lines.push(style.dim(&sub(&format!(
        "{}  {}  {}",
        report.session.agent,
        report.session.session_id,
        report
            .session
            .cwd
            .as_deref()
            .map(short_path)
            .unwrap_or_default()
    ))));
    lines.push(String::new());
    lines.join("\n")
}

fn format_send(style: &Style, report: &SendReport) -> String {
    let mut lines = vec![row(
        &style.ok(),
        "send",
        &format!(
            "queued  {}:{}",
            report.to.agent,
            short_id(&report.to.session_id)
        ),
    )];
    if !report.delivered.is_empty() {
        lines.push(style.dim(&sub(&format!("delivered  {}", report.delivered.join(", ")))));
    }
    lines.push(style.dim(&sub(&format!("mail      {}", report.mail_id))));
    lines.push(String::new());
    lines.join("\n")
}

fn format_stop(style: &Style, report: &StopReport) -> String {
    let status = if report.already_exited {
        "already exited"
    } else if report.stopped {
        "stopped"
    } else {
        "not running"
    };
    let mut lines = vec![row(
        &style.ok(),
        "stop",
        &format!(
            "{}  {}:{}",
            status,
            report.session.agent,
            short_id(&report.session.session_id)
        ),
    )];
    if !report.signaled.is_empty() {
        lines.push(style.dim(&sub(&format!("signaled  {}", report.signaled.join(", ")))));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn format_handoff(style: &Style, report: &handoff::Report) -> String {
    let mut lines = vec![row(
        &style.ok(),
        "handoff",
        &format!("{} → {}", report.from.agent, report.to.agent),
    )];
    lines.push(style.dim(&sub(&format!("reason    {}", clip(&report.reason, 80)))));
    if !report.delivered.is_empty() {
        lines.push(style.dim(&sub(&format!("delivered  {}", report.delivered.join(", ")))));
    }
    lines.push(style.dim(&sub(&format!("mail      {}", report.mail_id))));
    lines.push(String::new());
    lines.join("\n")
}

fn format_inbox(style: &Style, report: &InboxReport) -> String {
    if report.items.is_empty() {
        return format!(
            "{}\n",
            row(
                &style.skip(),
                "inbox",
                &format!("empty  {} unread", report.unread)
            )
        );
    }
    let mut lines = vec![row(
        &style.live(),
        "inbox",
        &format!("{} unread  {} total", report.unread, report.items.len()),
    )];
    lines.push(String::new());
    for item in &report.items {
        let from = item
            .from_agent
            .map(|agent| agent.as_str().to_string())
            .unwrap_or_else(|| "unknown".into());
        lines.push(row(&style.idle(), &from, &relative_time(item.ts)));
        lines.push(style.dim(&sub(&format!(
            "to       {}:{}",
            item.to_agent,
            short_id(&item.to_session)
        ))));
        lines.push(style.dim(&sub(&format!("mail     {}", item.id))));
        lines.push(sub(&clip(&item.message, 100)));
        lines.push(String::new());
    }
    lines.join("\n")
}

fn format_ack(style: &Style, report: &AckReport) -> String {
    let mut lines = vec![row(&style.ok(), "ack", "marked read")];
    lines.push(style.dim(&sub(&format!("through   {}", report.acked_through))));
    lines.push(style.dim(&sub(&format!("unread    {}", report.unread))));
    lines.push(String::new());
    lines.join("\n")
}

fn format_await(style: &Style, report: &AwaitReport) -> String {
    if report.items.is_empty() {
        return format!(
            "{}\n",
            row(
                &style.skip(),
                "await",
                &format!("{}  waited {}ms", report.status, report.waited_ms)
            )
        );
    }
    let mut lines = vec![row(
        &style.ok(),
        "await",
        &format!("{}  {} item(s)", report.status, report.items.len()),
    )];
    lines.push(String::new());
    let inbox = InboxReport {
        items: report.items.clone(),
        unread: report.items.len(),
        acked_through: None,
    };
    let body = format_inbox(style, &inbox);
    for line in body.lines().skip(2) {
        lines.push(line.to_string());
    }
    if !lines.last().is_some_and(|line| line.is_empty()) {
        lines.push(String::new());
    }
    lines.join("\n")
}

fn format_note(style: &Style, note: &Note, written: bool) -> String {
    if written {
        return format!(
            "{}\n{}\n",
            row(&style.ok(), "note", "saved"),
            style.dim(&sub(&short_path(&note.cwd)))
        );
    }
    if !note.exists || note.content.trim().is_empty() {
        return format!(
            "{}\n",
            row(
                &style.skip(),
                "note",
                &format!("none  {}", short_path(&note.cwd))
            )
        );
    }
    let mut lines = vec![row(&style.idle(), "note", &short_path(&note.cwd))];
    if let Some(time) = note.updated_at {
        lines.push(style.dim(&sub(&relative_time(time))));
    }
    lines.push(String::new());
    lines.push(note.content.trim_end().to_string());
    lines.push(String::new());
    lines.join("\n")
}

fn format_whoami(style: &Style, identity: &Identity) -> String {
    match (identity.agent, identity.session_id.as_deref()) {
        (Some(agent), Some(session)) => {
            let mut lines = vec![row(&style.live(), "whoami", &format!("{agent}  {session}"))];
            if let Some(cwd) = identity.cwd.as_deref() {
                lines.push(style.dim(&sub(&format!("cwd      {}", short_path(cwd)))));
            }
            if let Some(branch) = identity.branch.as_deref() {
                lines.push(style.dim(&sub(&format!("branch   {branch}"))));
            }
            lines.push(String::new());
            lines.join("\n")
        }
        _ => format!("{}\n", row(&style.skip(), "whoami", "unknown")),
    }
}

struct InstallProgress {
    tty: bool,
    color: bool,
    hidden_cursor: bool,
    host: Option<&'static str>,
    stop: Option<(Arc<AtomicBool>, thread::JoinHandle<()>)>,
}

impl InstallProgress {
    fn new() -> Self {
        let tty = std::io::stderr().is_terminal();
        Self {
            tty,
            color: tty && std::env::var_os("NO_COLOR").is_none(),
            hidden_cursor: false,
            host: None,
            stop: None,
        }
    }

    fn start(&mut self, host: &'static str) {
        self.stop_spin();
        self.host = Some(host);
        if !self.tty {
            return;
        }
        if !self.hidden_cursor {
            eprint!("\x1b[?25l");
            self.hidden_cursor = true;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let color = self.color;
        let handle = thread::spawn(move || {
            const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut i = 0;
            while !flag.load(Ordering::Relaxed) {
                let frame = paint(color, "36", FRAMES[i % FRAMES.len()]);
                eprint!("\r\x1b[K  {frame} {host}");
                let _ = std::io::stderr().flush();
                i += 1;
                thread::sleep(Duration::from_millis(80));
            }
        });
        self.stop = Some((stop, handle));
    }

    fn finish(&mut self, result: &HostInstall) {
        self.stop_spin();
        self.host = None;
        let mark = match result.status {
            HostStatus::Added | HostStatus::Replaced => paint(self.color, "32", "✓"),
            HostStatus::Skipped => paint(self.color, "2", "–"),
        };
        let message = match result.status {
            HostStatus::Added => "added MCP server".to_string(),
            HostStatus::Replaced => "replaced existing MCP server".to_string(),
            HostStatus::Skipped => format!("skipped ({})", result.detail),
        };
        if self.tty {
            eprint!("\r\x1b[K");
        }
        eprintln!("  {mark} {:<8}  {message}", result.host);
    }

    fn fail(&mut self, error: &dyn std::fmt::Display) {
        self.stop_spin();
        if let Some(host) = self.host.take() {
            let mark = paint(self.color, "31", "✗");
            if self.tty {
                eprint!("\r\x1b[K");
            }
            eprintln!("  {mark} {host:<8}  {error}");
        }
    }

    fn stop_spin(&mut self) {
        if let Some((stop, handle)) = self.stop.take() {
            stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
    }
}

impl Drop for InstallProgress {
    fn drop(&mut self) {
        self.stop_spin();
        if self.hidden_cursor {
            eprint!("\x1b[?25h");
            let _ = std::io::stderr().flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, parse_agent, read_prompt};
    use clap::Parser;
    use magents::model::Agent;
    use std::path::PathBuf;

    #[test]
    fn parses_spawn_arguments() {
        let cli = Cli::try_parse_from([
            "magents",
            "spawn",
            "codex",
            "--prompt-file",
            "task.md",
            "--cwd",
            "/tmp/worktree",
        ])
        .unwrap();

        match cli.command {
            Some(Command::Spawn {
                agent,
                prompt_file,
                cwd,
            }) => {
                assert_eq!(agent, "codex");
                assert_eq!(prompt_file, PathBuf::from("task.md"));
                assert_eq!(cwd, Some(PathBuf::from("/tmp/worktree")));
            }
            _ => panic!("expected spawn command"),
        }
    }

    #[test]
    fn defaults_spawn_prompt_to_stdin() {
        let cli = Cli::try_parse_from(["magents", "spawn", "codex"]).unwrap();

        match cli.command {
            Some(Command::Spawn { prompt_file, .. }) => {
                assert_eq!(prompt_file, PathBuf::from("-"));
            }
            _ => panic!("expected spawn command"),
        }
    }

    #[test]
    fn rejects_positional_or_conflicting_spawn_prompts() {
        assert!(Cli::try_parse_from(["magents", "spawn", "codex", "private prompt"]).is_err());
        assert!(
            Cli::try_parse_from([
                "magents",
                "spawn",
                "codex",
                "--prompt-file",
                "one.md",
                "--prompt-file",
                "two.md",
            ])
            .is_err()
        );
    }

    #[test]
    fn reads_prompt_files_and_rejects_empty_files() {
        let directory = tempfile::tempdir().unwrap();
        let prompt = directory.path().join("task.md");
        std::fs::write(&prompt, "complete task\nwith verification\n").unwrap();
        assert_eq!(
            read_prompt(&prompt).unwrap(),
            "complete task\nwith verification\n"
        );

        let empty = directory.path().join("empty.md");
        std::fs::write(&empty, " \n\t").unwrap();
        assert_eq!(
            read_prompt(&empty).unwrap_err().to_string(),
            "prompt must not be empty"
        );
    }

    #[test]
    fn rejects_unknown_spawn_agent_explicitly() {
        assert_eq!(parse_agent("codex").unwrap(), Agent::Codex);
        assert_eq!(
            parse_agent("unknown").unwrap_err().to_string(),
            "unknown agent: unknown"
        );
    }

    #[test]
    fn parses_hidden_supervisor_arguments() {
        let cli = Cli::try_parse_from([
            "magents",
            "__supervise",
            "grok",
            "--cwd",
            "/tmp/worktree",
            "--session",
            "session-id",
        ])
        .unwrap();

        match cli.command {
            Some(Command::Supervise {
                agent,
                cwd,
                session,
            }) => {
                assert_eq!(agent, "grok");
                assert_eq!(cwd, PathBuf::from("/tmp/worktree"));
                assert_eq!(session.as_deref(), Some("session-id"));
            }
            _ => panic!("expected supervisor command"),
        }
    }

    #[test]
    fn hides_supervisor_from_public_help() {
        let help = Cli::try_parse_from(["magents", "--help"])
            .err()
            .expect("help should stop parsing")
            .to_string();
        assert!(help.contains("spawn"));
        assert!(help.contains("create-memory"));
        assert!(help.contains("digest"));
        assert!(help.contains("await-reply"));
        assert!(help.contains("--output"));
        assert!(!help.contains("__supervise"));
    }

    #[test]
    fn parses_output_json_globally() {
        let before = Cli::try_parse_from(["magents", "--output", "json", "whoami"]).unwrap();
        assert_eq!(before.output, Some(super::OutputFormat::Json));
        let after = Cli::try_parse_from(["magents", "list", "--output", "json"]).unwrap();
        assert_eq!(after.output, Some(super::OutputFormat::Json));
        let text = Cli::try_parse_from(["magents", "--output", "text", "whoami"]).unwrap();
        assert_eq!(text.output, Some(super::OutputFormat::Text));
        assert!(super::wants_json(Some(super::OutputFormat::Json)));
        assert!(!super::wants_json(Some(super::OutputFormat::Text)));
    }

    #[test]
    fn parses_create_memory_arguments() {
        let cli = Cli::try_parse_from([
            "magents",
            "create-memory",
            "--agent",
            "claude",
            "--project",
            "tmp-dr",
            "--file",
            "dedicated-db-gaps.md",
            "--cwd",
            "/Users/foo/bar",
            "the note body",
        ])
        .unwrap();

        match cli.command {
            Some(Command::CreateMemory {
                agent,
                file,
                project,
                cwd,
                content,
            }) => {
                assert_eq!(agent, "claude");
                assert_eq!(file.as_deref(), Some("dedicated-db-gaps.md"));
                assert_eq!(project.as_deref(), Some("tmp-dr"));
                assert_eq!(cwd.as_deref(), Some("/Users/foo/bar"));
                assert_eq!(content, "the note body");
            }
            _ => panic!("expected create-memory command"),
        }
    }
}

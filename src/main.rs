use clap::{Parser, Subcommand};
use magents::discover::{ListFilter, list_sessions, resolve};
use magents::homes::Homes;
use magents::model::{Agent, Caller};
use magents::{deliver, mailbox, memory, transcript};
use std::io::{IsTerminal, Read};
use std::path::Path;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "magents",
    version,
    about = "Mates + agents: shared session bus for Claude, Codex, Cursor, Grok, and OpenCode"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the stdio MCP server
    Mcp,
    /// List sessions across Claude, Codex, Cursor, Grok, and OpenCode
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
    },
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
    match cli.command {
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
        }) => {
            let sessions = list_sessions(
                &Homes::from_env(),
                &ListFilter {
                    agent: agent.as_deref().and_then(Agent::parse),
                    query,
                    live_only: live,
                    include_archived: archived,
                    limit,
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&sessions)?);
        }
        Some(Command::Get { reference }) => {
            let session = resolve(&Homes::from_env(), &reference)?;
            println!("{}", serde_json::to_string_pretty(&session)?);
        }
        Some(Command::Read { reference, limit }) => {
            let transcript = transcript::read_transcript(&Homes::from_env(), &reference, limit)?;
            println!("{}", serde_json::to_string_pretty(&transcript)?);
        }
        Some(Command::Search {
            query,
            agent,
            limit,
        }) => {
            let hits = transcript::search_transcripts(
                &Homes::from_env(),
                &query,
                agent.as_deref().and_then(Agent::parse),
                false,
                limit,
            )?;
            println!("{}", serde_json::to_string_pretty(&hits)?);
        }
        Some(Command::SearchMemories {
            query,
            agent,
            limit,
        }) => {
            let hits = memory::search_memories(
                &Homes::from_env(),
                &query,
                agent.as_deref().and_then(Agent::parse),
                limit,
            )?;
            println!("{}", serde_json::to_string_pretty(&hits)?);
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
            println!("{}", serde_json::to_string_pretty(&created)?);
        }
        Some(Command::Spawn {
            agent,
            prompt_file,
            cwd,
        }) => {
            let agent = parse_agent(&agent)?;
            let prompt = read_prompt(&prompt_file)?;
            let report = magents::spawn::run(&Homes::from_env(), agent, &prompt, cwd.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Some(Command::Send { to, message }) => {
            let homes = Homes::from_env();
            let session = resolve(&homes, &to)?;
            let caller = Caller::from_env();
            let delivered = deliver::deliver_live(&homes, &session, &message)?;
            let mail = mailbox::compose(
                &caller,
                session.agent,
                session.session_id.clone(),
                message,
                delivered.clone(),
            );
            mailbox::post(&homes, &mail)?;
            println!(
                "{}",
                serde_json::json!({
                    "queued": true,
                    "to": session,
                    "delivered": delivered,
                    "mail_id": mail.id
                })
            );
        }
        Some(Command::Handoff { to, reason }) => {
            let report =
                magents::handoff::run(&Homes::from_env(), to.as_deref(), reason.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Some(Command::Inbox { session, agent }) => {
            let mail = mailbox::inbox(
                &Homes::from_env(),
                &Caller::from_env(),
                session.as_deref(),
                agent.as_deref().and_then(Agent::parse),
            )?;
            println!("{}", serde_json::to_string_pretty(&mail)?);
        }
        Some(Command::Install {
            claude,
            grok,
            codex,
            cursor,
            opencode,
            all,
        }) => {
            let notes = magents::install::install(
                claude || all,
                grok || all,
                codex || all,
                cursor || all,
                opencode || all,
            )?;
            for note in notes {
                println!("{note}");
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
        assert!(!help.contains("__supervise"));
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

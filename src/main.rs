use clap::{Parser, Subcommand};
use magents::discover::{ListFilter, list_sessions, resolve};
use magents::homes::Homes;
use magents::model::{Agent, Caller};
use magents::{deliver, mailbox, memory, transcript};
use std::io::IsTerminal;

#[derive(Parser)]
#[command(
    name = "magents",
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
    /// List sessions across Claude, Codex, and Grok
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
    }
    Ok(())
}

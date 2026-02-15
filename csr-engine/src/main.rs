use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

use csr_engine::engine;

#[derive(Parser, Debug)]
#[command(name = "csr-engine", about = "Claude Self-Reflect Rust Engine")]
struct Args {
    /// SQLite database path
    #[arg(long, default_value_os_t = default_db_path())]
    db_path: PathBuf,

    /// Claude projects directory
    #[arg(long, default_value_os_t = default_projects_dir())]
    projects_dir: PathBuf,

    /// Import conversations then exit (or combine with --serve)
    #[arg(long)]
    import: bool,

    /// Start MCP stdio server (default if no other flags)
    #[arg(long)]
    serve: bool,

    /// Max conversations to import
    #[arg(long)]
    limit: Option<usize>,

    /// Watch for new JSONL files and auto-import
    #[arg(long)]
    watch: bool,

    /// Run benchmarks instead of MCP server
    #[arg(long)]
    bench: bool,
}

fn default_db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude-self-reflect")
        .join("csr-engine.db")
}

fn default_projects_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("projects")
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    // Ensure DB directory exists
    if let Some(parent) = args.db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;

    if args.import {
        let count = eng.import_conversations(args.limit).await?;
        tracing::info!(count, "conversations imported");
    }

    if args.bench {
        tracing::info!("benchmark mode not yet implemented in main — use `cargo bench`");
        return Ok(());
    }

    // Start file watcher if requested (runs as background task)
    let _watcher_handle = if args.watch {
        Some(eng.start_watcher())
    } else {
        None
    };

    // Start MCP stdio server if --serve or no explicit action
    let should_serve = args.serve || (!args.import && !args.bench && !args.watch);
    if should_serve {
        eng.serve_mcp().await?;
    } else if args.watch {
        // If watching (with or without import), keep the process alive
        tracing::info!("watching for new conversations. Press Ctrl+C to stop.");
        tokio::signal::ctrl_c().await?;
    }

    Ok(())
}

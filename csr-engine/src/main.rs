use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

use csr_engine::engine;

#[derive(Parser, Debug)]
#[command(name = "csr-engine", about = "Claude Self-Reflect Rust Engine")]
struct Args {
    /// SQLite database path
    #[arg(long, default_value_os_t = default_db_path(), global = true)]
    db_path: PathBuf,

    /// Claude projects directory
    #[arg(long, default_value_os_t = default_projects_dir(), global = true)]
    projects_dir: PathBuf,

    #[command(subcommand)]
    command: Option<Commands>,

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

    /// Backfill import_state and run heuristic enrichment for all conversations
    #[arg(long)]
    enrich: bool,

    /// Backfill seq + is_sidechain on existing chunks from on-disk JSONLs (Saga Phase 1 WS1)
    #[arg(long)]
    backfill_saga: bool,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// One-shot setup: import, register MCP, install hooks
    Setup {
        /// Anthropic API key for AI narrative enrichment (optional)
        #[arg(long)]
        anthropic_key: Option<String>,
    },
    /// Show system status (JSON by default, --compact for statusline, --swiftbar for menu bar)
    Status {
        /// One-line output for statusline integration
        #[arg(long)]
        compact: bool,
        /// SwiftBar-compatible output for macOS menu bar plugin
        #[arg(long)]
        swiftbar: bool,
        /// Force a fresh full integrity check (slow on large DBs; otherwise cached)
        #[arg(long)]
        deep: bool,
    },
    /// Handle Claude Code hook events
    Hook {
        /// Hook name: session-start, session-end, precompact, stop, post-tool-use, prompt-submit, install
        name: String,

        /// For install: auto-apply to settings.json
        #[arg(long)]
        apply: bool,
    },
    /// Run the enrichment daemon (file watcher + Layer 2 + Layer 3)
    Daemon {
        /// Max conversations per AI narrative batch
        #[arg(long, default_value_t = 10)]
        batch_size: usize,

        /// Minutes before forcing a batch submission
        #[arg(long, default_value_t = 30)]
        batch_time: u64,

        /// Skip AI narrative generation (Layer 1+2 only, no API key needed)
        #[arg(long)]
        no_ai: bool,
    },
    /// Analyze code quality using AST patterns
    Quality {
        /// File path to analyze
        path: PathBuf,
    },
    /// Structured facts from a session transcript JSONL — stats, prompts,
    /// tool calls, touched files, errors, a turn-range slice, or a text
    /// grep — without hand-rolling a jq/Python parser over raw JSONL.
    Transcript {
        /// Session id (substring match against files under --projects-dir)
        /// or an explicit path to a transcript JSONL file.
        session: String,

        /// View to render: stats|prompts|tools|files|errors|slice|grep
        view: String,

        /// Narrow the session-id search to a project directory name
        /// containing this substring (ignored when `session` is a path).
        #[arg(long)]
        project: Option<String>,

        /// Role filter shared by every view: user|assistant|system|all
        #[arg(long, default_value = "all")]
        role: String,

        /// Turn range, inclusive: "40..120" or open-ended "340.."
        #[arg(long)]
        turns: Option<String>,

        /// `slice` view only: show the last N turns (ignored if --turns given)
        #[arg(long)]
        last: Option<usize>,

        /// `grep` view: regex to match against each turn's text
        #[arg(long)]
        grep: Option<String>,

        /// `tools`/`files` view: filter to this tool name only
        #[arg(long)]
        tool: Option<String>,

        /// Emit structured JSON instead of compact text
        #[arg(long)]
        json: bool,

        /// Character budget before an honest truncation marker is emitted
        #[arg(long, default_value_t = csr_engine::transcript::DEFAULT_BUDGET_CHARS)]
        budget_chars: usize,

        /// Reserved for v2 (subagent sidechain inclusion) — rejected with a
        /// clear message in v1, never silently ignored.
        #[arg(long)]
        sidechains: bool,
    },
    /// Run ratification extraction for specific conversation IDs (one-off,
    /// bypasses the daemon queue ordering)
    Ratify {
        /// Conversation IDs to score
        #[arg(required = true)]
        conversation_ids: Vec<String>,
    },
    /// Headless dream generation over this week's evidence: zero-or-one
    /// dream per project, printed to stdout. No journal HTML/routes, no
    /// browser surface.
    Dreams {
        /// Limit to one project (matches the stored project name exactly)
        #[arg(long)]
        project: Option<String>,
        /// Emit structured JSON instead of plain text
        #[arg(long)]
        json: bool,
        /// Skip the strategy (LLM-authored) category entirely — currently
        /// always effectively on, since that category isn't built yet
        #[arg(long = "no-llm")]
        no_llm: bool,
    },
    /// Run evaluation tests
    Eval {
        /// Run full evaluation (20 tests) instead of quick (5 tests)
        #[arg(long)]
        full: bool,
        /// Run the continuity gate (North Star: recall + provenance beats grep)
        #[arg(long)]
        continuity: bool,
        /// Run the LIVE north-star probe against the real index (no fixture)
        #[arg(long = "continuity-live")]
        continuity_live: bool,
        /// Provenance regression benchmark: reinstatement walk vs one-shot kNN.
        /// Also runs as part of `--full` (no-op-killer gate: `csr_why` machinery
        /// must genuinely engage — exit 1 on regression).
        #[arg(long)]
        provenance: bool,
        /// Run the deterministic code-graph release gate
        #[arg(long)]
        codegraph: bool,
        /// Measure the code-graph gate against the live database
        #[arg(long)]
        live: bool,
    },
    /// Backfill session stories from V3/heuristic data (zero cost)
    BackfillStories {
        /// Preview without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Backfill the code_evolution co-edit ledger from JSONL conversation
    /// history (feeds the B3 corpus-witness bind tier)
    BackfillCoedit {
        /// Preview would-insert counts per project; write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Code-graph operations (v9.4 conversation-provenance graph)
    Codegraph {
        #[command(subcommand)]
        action: CodegraphAction,
    },
    /// Dream journal server (v10.1 journal v4) — a read-only, loopback-only
    /// live surface over the witness ledger. The background daemon already
    /// hosts this on the same port; this subcommand is the standalone host
    /// for a machine that isn't running `csr-engine daemon`.
    Journal {
        #[command(subcommand)]
        action: JournalAction,
    },
    /// Show aggregated telemetry: hook latencies, startup stats, enrichment health
    Telemetry {
        /// Window (e.g. "24h", "7d", "30m", "all"). Default: 24h.
        #[arg(long)]
        since: Option<String>,
        /// Emit JSON instead of the text report
        #[arg(long)]
        json: bool,
        /// Open the live multi-pane TUI dashboard (q to quit)
        #[arg(long)]
        tui: bool,
    },
    /// Run one v10 "dreaming" cycle: HEAD stamp-spans, then the
    /// deterministic successor join over the witness ledger, emitting
    /// `witness_verdicts` events (anchor_obsolete / superseded_by /
    /// anchor_reinstated). Idempotent: re-running at an unchanged HEAD with
    /// unchanged conclusions writes nothing new.
    Dream {
        /// Compute and print the summary without writing verdict events.
        /// The prerequisite HEAD stamp-spans pass still runs FOR REAL
        /// (append-only witness_ledger evidence, harmless and idempotent) —
        /// only witness_verdicts insertion is suppressed, so a dry run and
        /// a subsequent real run compute identical verdicts.
        #[arg(long)]
        dry_run: bool,

        /// Restrict the join to anchors whose file resolves to this one
        /// repo root instead of every repo root the code graph knows about.
        #[arg(long)]
        repo: Option<String>,

        /// Render a self-contained static HTML dream journal from the
        /// witness ledger instead of running a dream cycle. Read-only —
        /// narrates whatever verdicts are already on record; run `dream`
        /// (without `--report`) first to produce new ones.
        #[arg(long)]
        report: bool,

        /// Output path for `--report`. Default:
        /// `~/.claude-self-reflect/reports/dream-<YYYY-MM-DD>.html`.
        #[arg(long)]
        out: Option<PathBuf>,

        /// With `--report`, don't launch the system viewer (`open`, macOS
        /// only) after writing the file.
        #[arg(long)]
        no_open: bool,
    },
    /// Generate a Haiku-curated session story (fire-and-forget from SessionEnd)
    GenerateStory {
        /// Path to the session transcript JSONL
        #[arg(long)]
        transcript: PathBuf,

        /// Current working directory (for project resolution)
        #[arg(long)]
        cwd: String,
    },
}

#[derive(Subcommand, Debug)]
enum JournalAction {
    /// Serve the dream journal on 127.0.0.1 until Ctrl-C.
    ///
    /// The bind interface is not configurable — the corpus behind this
    /// surface is private conversation data, so the listener is loopback by
    /// construction (`journal::loopback_addr` is the only bind-address
    /// constructor and takes a port, nothing else). `CSR_NO_JOURNAL_SERVER=1`
    /// disables it here and in the daemon.
    Serve {
        /// Loopback port. Defaults to `CSR_JOURNAL_PORT`, then the fixed
        /// bookmarkable default. `0` asks for an ephemeral port. If the
        /// chosen port is busy, an ephemeral one is used and printed.
        #[arg(long)]
        port: Option<u16>,

        /// Open the served URL in the system browser (macOS `open`).
        #[arg(long)]
        open: bool,
    },
}

#[derive(Subcommand, Debug)]
enum CodegraphAction {
    /// Reconstruct the code graph from all existing conversation JSONL history.
    Backfill {
        /// Parse + count what would be written, but make no changes.
        #[arg(long)]
        dry_run: bool,
    },
    /// Backfill `repo_root` (git toplevel) on existing code_nodes /
    /// code_evolution rows that predate the column (WP2 Stage 1, H8 finding).
    BackfillRepoRoot {
        /// Count what would be resolved, but make no changes.
        #[arg(long)]
        dry_run: bool,
    },
    /// Backfill two-channel symbol attribution (transcript + git) on every
    /// existing code_nodes row (WP2 Stage 2, H4 remediation).
    BackfillAttribution {
        /// Count what would be attributed, but make no changes.
        #[arg(long)]
        dry_run: bool,
    },
    /// Mint `codewitness` committed-tier witnesses for every function/type/
    /// const span (and whole-file fallback) known to the code graph — the
    /// append-only evidence substrate for v10 "dreaming" (evidence-grounded
    /// forgetting). Idempotent: safe to re-run.
    ///
    /// With `--at <rev>`, stamps spans AS THEY EXISTED at that historical
    /// commit instead of at each repo's live HEAD — the substrate for the
    /// S3 time-travel precision gate. Extraction runs directly against the
    /// commit's own tree (not the code graph's node list), so it can see
    /// files the graph never observed.
    StampSpans {
        /// Count what would be stamped, but make no changes.
        #[arg(long)]
        dry_run: bool,

        /// Stamp spans as they existed at this historical revision (commit
        /// SHA, tag, branch, `HEAD~N`, ...) instead of each repo's live
        /// HEAD. Resolved independently per repo; a repo where `rev` doesn't
        /// resolve is skipped, never guessed. One rev per invocation.
        #[arg(long)]
        at: Option<String>,

        /// Restrict to this one repo root instead of every repo root the
        /// code graph already knows about. Requires `--at` — HEAD-mode
        /// stamping (`backfill_stamp_spans`) has no repo filter, and
        /// silently ignoring the flag would stamp every known repo.
        #[arg(long, requires = "at")]
        repo: Option<String>,
    },
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
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    // Handle subcommands that don't need Engine::new()
    if let Some(Commands::Setup { anthropic_key }) = args.command {
        return csr_engine::setup::handle(&args.db_path, &args.projects_dir, anthropic_key).await;
    }

    if let Some(Commands::Status {
        compact,
        swiftbar,
        deep,
    }) = args.command
    {
        return csr_engine::status::handle(
            &args.db_path,
            &args.projects_dir,
            compact,
            swiftbar,
            deep,
        );
    }

    if let Some(Commands::Telemetry { since, json, tui }) = args.command {
        return csr_engine::telemetry::handle(&args.db_path, &args.projects_dir, since, json, tui);
    }

    if let Some(Commands::Daemon {
        batch_size,
        batch_time,
        no_ai,
    }) = args.command
    {
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
        let config = csr_engine::daemon::DaemonConfig {
            extraction_interval_secs: 30,
            batch_size_trigger: batch_size,
            batch_time_trigger_secs: batch_time * 60,
            batch_poll_interval_secs: 60,
        };
        let daemon = csr_engine::daemon::Daemon::new(
            eng.storage().clone(),
            eng.embeddings().clone(),
            eng.search().clone(),
            eng.projects_dir().to_path_buf(),
            eng.index_dir().to_path_buf(),
            config,
            !no_ai,
        );
        return daemon.run().await;
    }

    if let Some(Commands::Ratify {
        ref conversation_ids,
    }) = args.command
    {
        let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
        let storage = eng.storage().clone();
        for cid in conversation_ids {
            match csr_engine::daemon::ratification::process_ratification(&storage, cid).await {
                Ok(()) => println!("ratified {cid}"),
                Err(e) => {
                    let _ = storage.mark_enrichment_failed(cid, "ratification", &e.to_string());
                    eprintln!("FAILED {cid}: {e}");
                }
            }
        }
        return Ok(());
    }

    if let Some(Commands::Dreams {
        ref project,
        json,
        no_llm,
    }) = args.command
    {
        return csr_engine::dream::cli::handle(&args.db_path, project.as_deref(), json, no_llm);
    }

    if let Some(Commands::Quality { ref path }) = args.command {
        let report = csr_engine::extraction::quality::analyze_file(path)?;
        print!("{}", report.format_text());
        return Ok(());
    }

    if let Some(Commands::Transcript {
        ref session,
        ref view,
        ref project,
        ref role,
        ref turns,
        last,
        ref grep,
        ref tool,
        json,
        budget_chars,
        sidechains,
    }) = args.command
    {
        use csr_engine::transcript::{parse_turns_spec, RoleFilter, TranscriptRequest, ViewKind};

        let Some(view_kind) = ViewKind::parse(view) else {
            eprintln!(
                "error: unknown view '{view}' — expected one of: stats, prompts, tools, files, errors, slice, grep"
            );
            std::process::exit(2);
        };
        let Some(role_filter) = RoleFilter::parse(role) else {
            eprintln!(
                "error: unknown --role '{role}' — expected one of: user, assistant, system, all"
            );
            std::process::exit(2);
        };
        let parsed_turns = match turns {
            Some(spec) => match parse_turns_spec(spec) {
                Ok(range) => Some(range),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(2);
                }
            },
            None => None,
        };

        let req = TranscriptRequest {
            session: session.clone(),
            view: view_kind,
            project: project.clone(),
            role: role_filter,
            turns: parsed_turns,
            last,
            grep: grep.clone(),
            tool: tool.clone(),
            json,
            budget_chars,
            sidechains,
        };
        let out = csr_engine::transcript::run(&args.projects_dir, &req);
        print!("{out}");
        if !out.ends_with('\n') {
            println!();
        }
        return Ok(());
    }

    if let Some(Commands::Eval {
        full,
        continuity,
        continuity_live,
        provenance,
        codegraph,
        live,
    }) = args.command
    {
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
        if continuity_live {
            let out = csr_engine::eval::continuity::run_continuity_live(
                eng.storage(),
                eng.embeddings(),
                eng.search(),
            )
            .await;
            print!("{out}");
            return Ok(());
        }
        if continuity {
            let report = csr_engine::eval::continuity::run_continuity(eng.embeddings()).await;
            print!("{}", report.format_text());
            return Ok(());
        }
        if provenance {
            let report = csr_engine::eval::provenance::run_provenance(
                eng.storage(),
                eng.embeddings(),
                eng.search(),
            )
            .await?;
            print!("{}", report.text);
            if report.regression {
                std::process::exit(1);
            }
            return Ok(());
        }
        if codegraph {
            let report = if live {
                csr_engine::eval::codegraph::run_codegraph_live(eng.storage())?
            } else {
                csr_engine::eval::codegraph::run_codegraph(eng.storage())?
            };
            print!("{}", report.format_text());
            if report.results.iter().any(|result| !result.passed) {
                std::process::exit(1);
            }
            return Ok(());
        }
        let report = if full {
            csr_engine::eval::run_full(
                eng.storage(),
                eng.embeddings(),
                eng.search(),
                eng.index_dir(),
            )
            .await
        } else {
            csr_engine::eval::run_quick(
                eng.storage(),
                eng.embeddings(),
                eng.search(),
                eng.index_dir(),
            )
            .await
        };
        print!("{}", report.format_text());
        if full {
            // No-op-killer gate: the reinstatement machinery must genuinely
            // engage. Part of --full so it cannot silently rot as a local
            // opt-in nobody runs.
            let prov = csr_engine::eval::provenance::run_provenance(
                eng.storage(),
                eng.embeddings(),
                eng.search(),
            )
            .await?;
            print!("{}", prov.text);
            if prov.regression {
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    if let Some(Commands::BackfillStories { dry_run }) = args.command {
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
        return csr_engine::summarizer::backfill_stories_cli(&eng, dry_run).await;
    }

    if let Some(Commands::BackfillCoedit { dry_run }) = args.command {
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
        let stats = csr_engine::import::coedit_backfill::backfill_coedit(
            eng.storage(),
            &args.projects_dir,
            dry_run,
        )?;
        print!("{}", stats.format_text(dry_run));
        return Ok(());
    }

    if let Some(Commands::Codegraph {
        action: CodegraphAction::Backfill { dry_run },
    }) = args.command
    {
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
        let stats =
            csr_engine::import::backfill::backfill_code_graph(&eng, &args.projects_dir, dry_run)?;
        print!("{}", stats.format_text(dry_run));
        return Ok(());
    }

    if let Some(Commands::Codegraph {
        action: CodegraphAction::BackfillRepoRoot { dry_run },
    }) = args.command
    {
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
        let stats = csr_engine::import::backfill::backfill_repo_root(&eng, dry_run)?;
        print!("{}", stats.format_text(dry_run));
        return Ok(());
    }

    if let Some(Commands::Codegraph {
        action: CodegraphAction::BackfillAttribution { dry_run },
    }) = args.command
    {
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
        let stats = csr_engine::import::backfill::backfill_attribution(&eng, dry_run)?;
        print!("{}", stats.format_text(dry_run));
        return Ok(());
    }

    if let Some(Commands::Codegraph {
        action: CodegraphAction::StampSpans { dry_run, at, repo },
    }) = args.command
    {
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
        let stats = match at {
            Some(rev) => csr_engine::import::backfill::backfill_stamp_spans_at(
                &eng,
                &rev,
                repo.as_deref(),
                dry_run,
            )?,
            None => csr_engine::import::backfill::backfill_stamp_spans(&eng, dry_run)?,
        };
        print!("{}", stats.format_text(dry_run));
        return Ok(());
    }

    if let Some(Commands::Dream {
        dry_run,
        ref repo,
        report,
        ref out,
        no_open,
    }) = args.command
    {
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
        if report {
            let path = csr_engine::dream::report::run_report(
                eng.storage(),
                eng.projects_dir(),
                out.clone(),
                no_open,
            )?;
            println!("CSR dream journal written to {}", path.display());
            return Ok(());
        }
        let stats = csr_engine::dream::run_dream(&eng, repo.as_deref(), dry_run)?;
        // v10.1: CSR_DREAM_CONSUMPTION default OFF. The cycle still runs and
        // the witness ledger still updates for real either way — this
        // switch is about consumption/exposure, not about whether dreaming
        // happens — but the verdict-derived summary text (obsolete/
        // superseded/reinstated counts, events written) must not print
        // unless explicitly opted in, same shared switch as
        // mcp::tools/status/dream::report.
        match csr_engine::storage::recap_feeds::dream_consumption_mode() {
            csr_engine::storage::recap_feeds::ConsumptionMode::Off => {
                let mode = if dry_run { " (dry-run)" } else { "" };
                println!(
                    "CSR dream{mode}: cycle complete — verdict summary suppressed; witness ledger \
                     updated regardless."
                );
            }
            csr_engine::storage::recap_feeds::ConsumptionMode::AnnotateOnly
            | csr_engine::storage::recap_feeds::ConsumptionMode::Full => {
                print!("{}", stats.format_text(dry_run));
            }
        }
        return Ok(());
    }

    if let Some(Commands::Journal {
        action: JournalAction::Serve { port, open },
    }) = args.command
    {
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
        return csr_engine::journal::run_cli(eng.storage().clone(), port, open).await;
    }

    if let Some(Commands::GenerateStory {
        ref transcript,
        ref cwd,
    }) = args.command
    {
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
        return csr_engine::summarizer::generate_story_cli(&eng, transcript, cwd).await;
    }

    if let Some(Commands::Hook { ref name, apply }) = args.command {
        if name == "install" {
            return csr_engine::hooks::install::handle(apply);
        }

        // Recursion guard, checked before the engine is built: nested `claude -p`
        // sessions (briefing, narratives, ratification) inherit hook config —
        // exit before paying model/index startup for a no-op.
        if std::env::var("CSR_DISABLE_RECURSIVE_HOOKS").as_deref() == Ok("1") {
            return Ok(());
        }

        // SessionEnd races app exit: Claude Code cancels hooks still running at
        // quit, so engine startup + HNSW flush (~0.5s) almost always dies as
        // "Hook cancelled". Re-spawn ourselves disowned and return immediately;
        // the child does the real work and survives the parent's cancellation.
        // SessionEnd output is never injected, so nothing is lost by detaching.
        if name == "session-end" && std::env::var("CSR_HOOK_DETACHED").as_deref() != Ok("1") {
            use std::io::{Read, Write};
            let mut stdin_buf = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut stdin_buf);
            let exe = std::env::current_exe()?;
            let mut cmd = std::process::Command::new(exe);
            cmd.args(["hook", "session-end"])
                .env("CSR_HOOK_DETACHED", "1")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                cmd.process_group(0);
            }
            if let Ok(mut child) = cmd.spawn() {
                if let Some(mut pipe) = child.stdin.take() {
                    let _ = pipe.write_all(&stdin_buf);
                }
            }
            return Ok(());
        }

        // Other hooks need the engine
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
        csr_engine::hooks::dispatch_hook(name, &eng).await?;
        return Ok(());
    }

    // Ensure DB directory exists
    if let Some(parent) = args.db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;

    if args.import {
        let count = eng.import_conversations(args.limit).await?;
        tracing::info!(count, "conversations imported");
    }

    if args.enrich {
        let (backfilled, enriched) = eng.backfill_and_enrich().await?;
        eprintln!(
            "CSR: backfilled {} import_state rows, enriched {} conversations",
            backfilled, enriched
        );
    }

    if args.backfill_saga {
        let stats = csr_engine::import::backfill::backfill_saga_columns(&eng)?;
        print!("{}", stats.format_text());
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
    let should_serve = args.serve
        || (!args.import && !args.bench && !args.watch && !args.enrich && !args.backfill_saga);
    if should_serve {
        eng.serve_mcp().await?;
    } else if args.watch {
        // If watching (with or without import), keep the process alive
        tracing::info!("watching for new conversations. Press Ctrl+C to stop.");
        tokio::signal::ctrl_c().await?;
    }

    Ok(())
}

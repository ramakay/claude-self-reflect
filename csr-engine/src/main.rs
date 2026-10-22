use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

use csr_engine::engine;

#[derive(Parser, Debug)]
// `version` is load-bearing for the installers: they decide whether an already
// present binary is this release by running `csr-engine --version` and nothing
// else. Every other subcommand opens the user's live database.
#[command(
    name = "csr-engine",
    version,
    about = "Claude Self-Reflect Rust Engine"
)]
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
    /// Companion pane and private file writer
    Companion(csr_engine::companion::cli::CompanionArgs),
    /// One-shot setup: import, register MCP, install hooks, install the companion plugin
    Setup {
        /// Anthropic API key for AI narrative enrichment (optional)
        #[arg(long)]
        anthropic_key: Option<String>,
        /// Do not install the csr-companion plugin this binary writes
        #[arg(long)]
        no_companion: bool,
    },
    /// Show system status (JSON by default, --compact for statusline, --swiftbar for menu bar)
    Status {
        /// Explicit JSON output (the default); identical to plain `status`
        #[arg(long, conflicts_with_all = ["compact", "swiftbar", "stale"])]
        json: bool,
        /// One-line output for statusline integration
        #[arg(long)]
        compact: bool,
        /// SwiftBar-compatible output for macOS menu bar plugin
        #[arg(long)]
        swiftbar: bool,
        /// Force a fresh full integrity check (slow on large DBs; otherwise cached)
        #[arg(long)]
        deep: bool,
        /// DB-free fast path: only the stale-process report (run registry +
        /// liveness check, milliseconds), no SQLite gather at all. For
        /// installers, which need this right after writing a fresh binary
        /// and cannot afford the full status gather's multi-second DB
        /// pass (gap-G2 review finding 2).
        #[arg(long)]
        stale: bool,
    },
    /// Handle Claude Code hook events
    Hook {
        /// Hook name: session-start, session-end, precompact, stop, stop-work (internal: the detached worker stop starts), post-tool-use, prompt-submit, install, subagent-stop (deprecated, no-op)
        name: String,

        /// For install: auto-apply to settings.json
        #[arg(long)]
        apply: bool,

        /// For stop-work: the spool key (the session) whose Stop input to process
        #[arg(long)]
        session: Option<String>,
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
    /// Force a full HNSW index rebuild from SQLite now, in the foreground.
    /// Explicit maintenance command counterpart to the daemon's
    /// `needs_compaction`-marker compaction loop — use when no daemon is
    /// running, or to compact immediately rather than wait for the next
    /// daemon cycle.
    RebuildIndex,
    /// Analyze code quality using AST patterns
    Quality {
        /// File path to analyze
        path: PathBuf,
    },
    /// Live code map: a per-worktree map of every definition in your repos, so
    /// CSR can answer for code no conversation touched. `add`/`remove` register
    /// repos, `scan` builds the map, `status` and `query` read it. Off with
    /// `CSR_NO_CODEMAP=1`.
    Codemap {
        #[command(subcommand)]
        action: CodemapAction,
    },
    /// Facts about a symbol's history, read straight from git (never opens
    /// the database): `symbol change` says whether a commit authored a
    /// change to a symbol and, for a merge that did not, which commit did.
    Symbol {
        #[command(subcommand)]
        action: SymbolAction,
    },
    /// Reaction-judge shadow mode: `status`, `run` (one scoring pass now), or
    /// `parity` (Rust packing + probabilities vs the Python fixture)
    ReactionShadow {
        /// status | run | parity
        action: String,
        /// parity: fixture JSONL (default <models dir>/parity_dev40.jsonl)
        #[arg(long)]
        fixture: Option<PathBuf>,
        /// run: max model scorings in this pass
        #[arg(long, default_value_t = csr_engine::reaction_shadow::DEFAULT_MAX_SCORES_PER_CYCLE)]
        max: usize,
    },
    /// Structured facts from a session transcript JSONL — stats, prompts,
    /// tool calls, touched files, errors, a turn-range slice, or a text
    /// grep — without hand-rolling a jq/Python parser over raw JSONL.
    Transcript {
        /// Session id (substring match against files under --projects-dir)
        /// or an explicit path to a transcript JSONL file. When the JSONL and
        /// the mirror copy are both gone, a session CSR holds indexed chunks
        /// for (exact id, or a unique prefix of 8+ characters) is rebuilt from
        /// them in --db-path, read-only, under a header that says it is lossy.
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

        /// prompts view: include non-human user turns (task notifications,
        /// skill bodies, cross-session messages, local-command echoes,
        /// compaction summaries) and queued duplicates; default false.
        #[arg(long)]
        include_system: bool,
    },
    /// The transcript mirror: scrubbed, zstd-compressed copies of every
    /// Claude Code transcript, kept in the data folder after the JSONL goes.
    Mirror {
        #[command(subcommand)]
        action: MirrorAction,
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
        /// Skip the strategy (LLM-authored) category entirely; only the
        /// deterministic unfinished category runs
        #[arg(long = "no-llm")]
        no_llm: bool,
        /// Anchor "now" at a past instant (RFC3339 or YYYY-MM-DD, UTC) so the
        /// rolling 7-day evidence window covers an earlier week — replay/eval
        #[arg(long = "as-of")]
        as_of: Option<String>,
    },
    /// Run evaluation tests
    Eval {
        /// Run full evaluation (24 results) instead of quick (5 tests plus the reranker gate)
        #[arg(long)]
        full: bool,
        /// With --full: start a new known-item recall sample instead of reusing
        /// the frozen one. Prior history is kept; the selection ignores scores.
        #[arg(long = "refresh-recall-sample", requires = "full")]
        refresh_recall_sample: bool,
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
    /// Run a reproducible retrieval benchmark in a scratch in-memory store.
    Bench {
        /// Input shape: agentmemory or longmemeval.
        #[arg(long)]
        format: String,
        /// agentmemory directory (sessions.json + queries.json) or LongMemEval JSON file.
        #[arg(long)]
        data: PathBuf,
        /// Override the agentmemory queries.json path.
        #[arg(long)]
        queries: Option<PathBuf>,
        /// Retrieval cutoff.
        #[arg(long, default_value_t = 5)]
        k: usize,
        /// Search ablation: vector, fts, or hybrid.
        #[arg(long, default_value = "hybrid")]
        mode: String,
        /// Output directory for scores.ndjson, summary.json, and table.md.
        #[arg(long, default_value = "target/csr-bench")]
        out: PathBuf,
        /// Exclude LongMemEval's four *_abs question types (enabled by default).
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
        drop_abstention: bool,
        /// Maximum number of scored questions after filtering/stratification.
        #[arg(long)]
        limit: Option<usize>,
        /// Keep at most N questions of each question type.
        #[arg(long)]
        stratify: Option<usize>,
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
    /// One-time, idempotent re-stamp of `codex_rollout` chunk timestamps that
    /// were collapsed onto the session-start time by the pre-fix chunker. No
    /// re-embedding, no HNSW writes — content-guarded `UPDATE`s only. Safe to
    /// run repeatedly; a second run is a no-op once the meta marker is set.
    BackfillCodexTimestamps,
    /// One-time, idempotent invalidation of already-imported `codex_rollout`
    /// conversations whose stored summary is Codex harness boilerplate
    /// (`<recommended_plugins>`, `# AGENTS.md instructions`, ...) rather than
    /// the real first user turn — see the WP-A adapter strip in
    /// `import::codex_rollout::canonical_message`. Clears their
    /// `import_state` mtime only; run `setup`/the daemon afterward to
    /// actually re-import them through the fixed adapter. Safe to run
    /// repeatedly; a second run is a no-op once the meta marker is set.
    BackfillCodexHarnessStrip,
    /// Historical deterministic capture operations.
    Backfill {
        #[command(subcommand)]
        action: BackfillAction,
    },
    /// Structural memory provenance operations.
    Provenance {
        #[command(subcommand)]
        action: ProvenanceAction,
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
    /// Dump a deterministic, stratified sample of persisted reaction labels.
    AuditRerankLabels {
        /// Maximum audited rows for each non-abstained reaction class.
        #[arg(long, default_value_t = 20)]
        per_class: usize,
        /// Maximum abstain-adjacent near misses to include.
        #[arg(long, default_value_t = 20)]
        near_misses: usize,
        /// Emit the audit rows and counts as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Distill repeated user corrections into CLAUDE.md candidate lines.
    Lessons {
        /// Restrict candidates to this normalized project name.
        #[arg(long)]
        project: String,
        /// Ignore events older than this UTC date (YYYY-MM-DD).
        #[arg(long)]
        since: Option<String>,
        /// Minimum number of distinct sessions represented by a group.
        #[arg(long, default_value_t = 2)]
        min_sessions: usize,
        /// Emit structured groups and all byte receipts as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run one v10 "dreaming" cycle: HEAD stamp-spans, then the
    /// deterministic successor join over the witness ledger, emitting
    /// `witness_verdicts` events (anchor_obsolete / superseded_by /
    /// anchor_reinstated). Idempotent: re-running at an unchanged HEAD with
    /// unchanged conclusions writes nothing new.
    Dream {
        /// Dream BACKFILL pipeline (`.plans/dream-backfill-design.md`):
        /// `backfill` mines historical episodes for forgotten-but-consequential
        /// supersessions; `drain` composes the ranked queue into `dreams_v1`.
        /// When omitted, the flags below run the ORIGINAL v10 witness-ledger
        /// dream cycle unchanged.
        #[command(subcommand)]
        action: Option<DreamAction>,

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

        /// Advertise the live "dreaming" statusline marker for this cycle,
        /// labelled with this trigger name (e.g. `compact`), and self-skip if
        /// a dream is already in flight. Set by the PreCompact hook so a
        /// compaction lights the statusline; harmless to pass by hand.
        #[arg(long)]
        mark: Option<String>,
    },
    /// Generate a Haiku-curated session story (fire-and-forget from SessionEnd)
    GenerateStory {
        /// Path to the session transcript JSONL
        #[arg(long)]
        transcript: PathBuf,

        /// Current working directory (for project resolution)
        #[arg(long)]
        cwd: String,

        /// Write the story from Claude Code's newest compaction summary (no model call)
        #[arg(long, conflicts_with = "rewrite")]
        from_compaction: bool,

        /// Let Haiku rewrite a story that has fallen behind (opt-in: CSR_STORY_REWRITES=1)
        #[arg(long)]
        rewrite: bool,
    },
}

#[derive(Subcommand, Debug)]
enum BackfillAction {
    /// Capture corrections, redirects, and explicit abandoned approaches from transcripts.
    Intent {
        /// Ignore events older than this UTC date (YYYY-MM-DD).
        #[arg(long)]
        since: Option<String>,
        /// Restrict to one normalized Claude project name.
        #[arg(long)]
        project: Option<String>,
        /// Print counts without inserting intent_events rows.
        #[arg(long)]
        dry_run: bool,
        /// Compare deterministic events with verified narrator-ledger quotes.
        #[arg(long)]
        ledger: Option<PathBuf>,
    },
    /// Plan what a re-parse would change. Deletes nothing: a real run refuses,
    /// CSR text is labeled in search instead.
    Scrub {
        /// Print per-conversation plans (the only mode that runs).
        #[arg(long)]
        dry_run: bool,
        /// Restrict the plan to one conversation holding labeled CSR text.
        #[arg(long)]
        conversation: Option<String>,
        /// Only list conversations whose transcript is a CSR agent prompt
        /// (narrator, ratification, briefing children).
        #[arg(long)]
        agent_transcripts: bool,
        /// Also scrub conversations active in the last 15 minutes (source file
        /// or newest chunk). Off by default: a live session's hooks may be
        /// writing them.
        #[arg(long)]
        include_active: bool,
        /// Run even though other processes hold the database open (the
        /// preflight lists them). Their writes can race the scrub.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
enum ProvenanceAction {
    /// Re-derive deterministic artifact floors; model-produced legacy rows remain unknown.
    BackfillArtifacts {
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Populate structural provenance for chunks that do not have spans yet.
    Backfill {
        /// Override the SQLite database path for this operation.
        #[arg(long)]
        db: Option<PathBuf>,
        /// Inspect and reconstruct without writing.
        #[arg(long)]
        dry_run: bool,
        /// Maximum chunks per write transaction.
        #[arg(long, default_value_t = csr_engine::import::provenance_backfill::DEFAULT_BATCH_SIZE)]
        batch: usize,
        /// Re-examine chunks with only non-JSONL receipts. Real JSONL spans are never replaced.
        #[arg(long)]
        retry_unknown: bool,
    },
}

/// `csr-engine dream backfill` / `csr-engine dream drain`
/// (`.plans/dream-backfill-design.md` §4). A nested subcommand of `Dream`
/// rather than new flags directly on it: `backfill`'s own `--dry-run`/
/// `--report` mean something entirely different from the witness-ledger
/// `dream` command's flags of the same name, and clap subcommand namespaces
/// keep the two from colliding.
/// `mirror` subcommands. None of them loads the embedding model or the index.
/// `codemap` subcommands. They use the global `--db-path`; none loads the
/// embedding model or the search index.
#[derive(Subcommand, Debug)]
enum CodemapAction {
    /// Register a repo (any path inside it) and list its worktrees
    Add { path: PathBuf },
    /// Stop mapping a repo: drops its views and keeps discovery from re-adding it
    Remove { path: PathBuf },
    /// Map a repo's worktrees (`--repo`), or every discovered and registered repo (`--all`)
    Scan {
        /// Any path inside the repo to scan
        #[arg(long, conflicts_with = "all")]
        repo: Option<PathBuf>,
        /// Scan every registered repo plus the ones discovery finds
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
        /// Session opt-in: let this scan enter the macOS-protected folder (Desktop,
        /// Documents, Downloads, iCloud Drive, a volume) that `--repo` is in. The
        /// SessionStart hook passes it; the daemon never does
        #[arg(long, requires = "repo", conflicts_with = "all")]
        session: bool,
    },
    /// Rescan the queued edits of one repo in a protected folder, then exit (the
    /// PostToolUse hook starts this detached; there is no daemon for these repos)
    Drain {
        /// Any path inside the repo
        #[arg(long)]
        repo: PathBuf,
        /// Seconds to wait for more edits before rescanning
        #[arg(long, value_name = "SECS", default_value_t = 5)]
        debounce: u64,
    },
    /// Per repo and worktree: branch, head, files, dirty, untracked, age; totals
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Run the daemon's freshness loop (poll, file watcher, hook queue) in the foreground and log each action
    Watch {
        /// Stop after this many seconds (default: run until interrupted)
        #[arg(long = "for", value_name = "SECS")]
        for_secs: Option<u64>,
    },
    /// Find a definition by name, signature or path; shows which worktrees hold the same blob
    Query {
        text: String,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

/// `symbol` subcommands. None of them opens the database or loads the
/// embedding model or the index.
#[derive(Subcommand, Debug)]
enum SymbolAction {
    /// Did `--oid` author a change to the symbol? Prints one JSON object:
    /// `{"oid","merge","authored","author","author_committed_at","summary"}`
    /// (`summary`: one line saying what the author commit changed inside the
    /// symbol, or null).
    /// With `--batch`, reads one JSON question per stdin line
    /// (`{"repo","file","symbol","oid","from"}`, optionally `"summary":false`) and prints one answer per
    /// line, in order; a bad line answers `{"error":...}` and the batch goes on.
    Change {
        /// Repository root
        #[arg(long, required_unless_present = "batch", conflicts_with = "batch")]
        repo: Option<PathBuf>,
        /// File, relative to the repository root
        #[arg(long, required_unless_present = "batch", conflicts_with = "batch")]
        file: Option<String>,
        /// Ledger symbol; omit for the whole file
        #[arg(long, conflicts_with = "batch")]
        symbol: Option<String>,
        /// The commit asked about
        #[arg(long, required_unless_present = "batch", conflicts_with = "batch")]
        oid: Option<String>,
        /// The witness's at_oid; needed to resolve the author of a merge
        #[arg(long, conflicts_with = "batch")]
        from: Option<String>,
        /// Read the questions from stdin, one JSON object per line
        #[arg(long)]
        batch: bool,
    },
    /// Where is each symbol of a file now? Prints one JSON object
    /// `{"file","lines","symbols":{"<ledger symbol>":[start,end]}}` (1-based
    /// inclusive lines, named as the witness ledger names them) from the
    /// working-tree file, or from the blob at `--at`. An unsupported language or
    /// a file that does not parse prints `{"error":...}` and exits 1.
    Span {
        /// Repository root
        #[arg(long)]
        repo: PathBuf,
        /// File, relative to the repository root
        #[arg(long)]
        file: String,
        /// Read the file as committed at this commit, not from the working tree
        #[arg(long, conflicts_with = "stdin")]
        at: Option<String>,
        /// Take the file's bytes from stdin (a snapshot the caller holds);
        /// `--file` still names the path and so the language
        #[arg(long)]
        stdin: bool,
    },
}

#[derive(Subcommand, Debug)]
enum MirrorAction {
    /// One full pass: mirror new transcript bytes, seal quiet transcripts, flag
    /// mirrors whose source is gone. Prints files, bytes and errors.
    Sync,
    /// Totals from the state table (no directory walk).
    Status,
    /// Delete the compressed transcripts and their state rows. Prints what it
    /// would delete unless --yes is given.
    Purge {
        /// Actually delete.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
enum DreamAction {
    /// Deterministic supersession mining over historical episodes, with a
    /// single budgeted LLM adjudication stage. See
    /// `dream::backfill::cli::handle_backfill`'s doc for `--stage`'s exact
    /// semantics.
    Backfill {
        /// Restrict to one project (matches the stored project name exactly).
        #[arg(long)]
        project: Option<String>,

        /// Hard cap on adjudication (`claude -p`) calls this run may spend.
        #[arg(long, default_value_t = csr_engine::dream::backfill::adjudicate::DEFAULT_BUDGET_CALLS)]
        budget_calls: usize,

        /// Stages 0-3 only (materialize, unfinished scan, pair generation +
        /// rank/gate): prints candidate counts and the top queue heads.
        /// Zero LLM spend; this is the eyeball checkpoint the acceptance
        /// protocol runs before `--budget-calls` ever spends anything.
        #[arg(long)]
        dry_run: bool,

        /// Bypass the nightly drain and render the FULL post-verification
        /// ranked queue (verified relations + queue-U), unrestricted by the
        /// per-night cap.
        #[arg(long)]
        report: bool,

        /// Run through this pipeline stage only and stop (0-3; omit for a
        /// full run through adjudication). A resumability/debug aid.
        #[arg(long)]
        stage: Option<u8>,

        /// Print the SQL-stage funnel (episodes -> funeral symbols ->
        /// anchor overlap -> negative verdicts -> reinstated) per family
        /// and exit. Read-only corpus counts — no git, no LLM, ignores
        /// `--dry-run`/`--report`/`--stage`.
        #[arg(long)]
        funnel: bool,
    },

    /// Drain the ranked backfill queue (verified relations + queue-U) into
    /// `dreams_v1`, capped at `--n` and enforcing one open dream per
    /// (project, topic) within 30 days.
    Drain {
        /// How many dreams to compose this run (design default: 3/night).
        #[arg(long, default_value_t = csr_engine::dream::backfill::compose::DEFAULT_DRAIN_N)]
        n: usize,
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
    /// Run the project-wide edge resolve + rank for every file an edit hook
    /// extracted since the last one, and settle those files. What the daemon's
    /// graph drain loop does on a timer; prints the result as JSON.
    Drain,
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

fn main() -> Result<()> {
    // Captured as the literal first thing this process does, before ANY
    // other work — `Engine::new()` alone (storage open, HNSW load/rebuild,
    // embeddings warm) can take seconds on a large corpus, and macOS
    // codesign/Gatekeeper verification on a freshly installed binary's
    // first launch adds more before even that. Capturing identity any
    // later widens the window in which a binary replacement landing during
    // this process's own startup goes permanently invisible to it: it
    // would record the NEW file's identity as its own baseline and could
    // never see itself as replaced (gap-G2; caught by a scratch-HOME smoke
    // test where the replace happened to land in exactly that window).
    // Captured together (path + identity, ONE `current_exe()` call) rather
    // than separately: a caller that re-derives the path later via its own
    // `current_exe()` call can race the same startup-window replacement and
    // get back a `(deleted)` path on Linux, which fails every downstream
    // replacement check closed forever (gap-G2 review finding 4) — see
    // `binary_identity::current_exe_path_and_identity`'s doc.
    let startup_exe = csr_engine::binary_identity::current_exe_path_and_identity();
    let started = std::time::Instant::now();
    // A retired hook must stay silent even when probed, and the probe has to
    // print before argument parsing, so the name is spotted straight from argv.
    // `args_os`: a non-UTF-8 path argument is legal and must not panic here.
    let retired_hook = csr_engine::hooks::argv_names_retired_hook(std::env::args_os().skip(1));
    if !retired_hook && std::env::var_os("CSR_HOOK_TIMING_PROBE").is_some() {
        eprintln!(
            "CSR timing-probe main_unix_ns={}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
    }
    let args = Args::parse();
    if let Some(Commands::Companion(action)) = &args.command {
        std::process::exit(csr_engine::companion::cli::run(action));
    }
    // Retired hooks still named by older settings.json files: exit before the
    // maintenance lock, stdin, storage or runtime so they cost only the spawn.
    if matches!(&args.command, Some(Commands::Hook { name, .. }) if csr_engine::hooks::is_retired_hook(name))
    {
        return Ok(());
    }
    let _hook_operation = match &args.command {
        // `stop-work` waits for the lock itself, below: a maintenance run that
        // starts between the parent's spool and this process must not strand the
        // Stop it was started for.
        Some(Commands::Hook { name, .. }) if name != "install" && name != "stop-work" => {
            let Some(operation) = csr_engine::hooks::try_start(&args.db_path) else {
                return Ok(());
            };
            Some(operation)
        }
        _ => None,
    };
    if matches!(&args.command, Some(Commands::Hook { name, .. }) if name == "prompt-submit") {
        let _ = std::panic::catch_unwind(|| {
            csr_engine::hooks::prompt_transport::run_client(
                &args.db_path,
                &args.projects_dir,
                started,
            );
        });
        return Ok(());
    }
    // Raise the open-file ceiling before any index load so a low soft RLIMIT_NOFILE
    // cannot turn a transient EMFILE inside hnsw_rs's mmap load into a process exit.
    csr_engine::runtime::raise_fd_limit();
    let workers = csr_engine::runtime::resolve_tokio_workers();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()?;
    // `run()` can return while a `spawn_blocking` task the daemon's
    // compaction loop started (inside `Engine::rebuild_search_index`) is
    // still streaming an HNSW rebuild on the blocking pool — shutdown
    // abandons *awaiting* that task (see `daemon::compact_if_marked`'s
    // `tokio::select!`), but the closure itself keeps running. A bare
    // `runtime.block_on(run())` returning here would drop `runtime`
    // implicitly, and dropping a `Runtime` blocks the dropping thread until
    // every spawned blocking task returns (tokio 1.53.1
    // `src/runtime/runtime.rs`, `src/task/blocking.rs`) — minutes, on the
    // real corpus, well past whatever asked this process to stop. That
    // includes SIGTERM (`launchctl bootout`, the installer's stop step) —
    // `Daemon::run` waits on `daemon::wait_for_termination_signal`, which
    // races SIGINT against an installed SIGTERM handler, specifically so
    // SIGTERM reaches `run()`'s return here instead of killing the process
    // at the OS's default disposition before any Rust code (including this
    // bound) ever runs. `csr_engine::runtime::run_to_completion` bounds
    // that wait with `shutdown_timeout` instead of blocking indefinitely:
    // the abandoned closure only builds an in-memory `SearchEngine` nothing
    // will read once this process exits, so leaving its thread running past
    // the timeout — until the OS reclaims it on process exit — is safe.
    // Extracted into `runtime::run_to_completion` (not inlined here) so the
    // shutdown-bounding behaviour has a test that calls the exact function
    // `main` calls — see `runtime::tests::run_to_completion_bounds_the_wait_for_an_abandoned_blocking_task`.
    csr_engine::runtime::run_to_completion(
        runtime,
        run(args, startup_exe),
        std::time::Duration::from_secs(5),
    )
}

async fn run(
    args: Args,
    startup_exe: Option<(PathBuf, csr_engine::binary_identity::FileIdentity)>,
) -> Result<()> {
    let startup_identity = startup_exe.as_ref().map(|(_, identity)| *identity);
    let startup_exe_path = startup_exe.map(|(path, _)| path);
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    // Handle subcommands that don't need Engine::new()
    if let Some(Commands::Setup {
        anthropic_key,
        no_companion,
    }) = args.command
    {
        return csr_engine::setup::handle(
            &args.db_path,
            &args.projects_dir,
            anthropic_key,
            !no_companion,
        )
        .await;
    }

    if let Some(Commands::Status {
        json: _,
        compact,
        swiftbar,
        deep,
        stale,
    }) = args.command
    {
        if stale {
            return csr_engine::status::handle_stale();
        }
        return csr_engine::status::handle(
            &args.db_path,
            &args.projects_dir,
            compact,
            swiftbar,
            deep,
        );
    }

    if let Some(Commands::Provenance {
        action: ProvenanceAction::BackfillArtifacts { db },
    }) = &args.command
    {
        let storage = csr_engine::storage::Storage::open(db.as_ref().unwrap_or(&args.db_path))?;
        let written = csr_engine::storage::artifact_backfill::backfill(&storage)?;
        let lowered = storage.relink_sidechains()?;
        println!(
            "{}",
            serde_json::json!({"artifacts_written":written,"lowered":lowered})
        );
        return Ok(());
    }

    if let Some(Commands::Provenance {
        action:
            ProvenanceAction::Backfill {
                db,
                dry_run,
                batch,
                retry_unknown,
            },
    }) = &args.command
    {
        let db_path = db.as_ref().unwrap_or(&args.db_path);
        let storage = csr_engine::storage::Storage::open(db_path)?;
        let stats = csr_engine::import::provenance_backfill::backfill_with_retry(
            &storage,
            &args.projects_dir,
            *batch,
            *dry_run,
            *retry_unknown,
        )?;
        let (chunks_with_spans, chunks_unknown, chunks_total, tool_result_share_mean) =
            storage.provenance_coverage()?;
        let [source_missing, source_unparsed, source_unmatched] =
            storage.provenance_failure_counts()?;
        let tier_histogram = storage
            .provenance_tier_histogram()?
            .into_iter()
            .map(|(tier, count)| (tier.to_string(), count))
            .collect::<std::collections::BTreeMap<_, _>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "backfill": stats,
                "dry_run": dry_run,
                "provenance_coverage": {
                    "chunks_with_spans": chunks_with_spans,
                    "chunks_unknown": chunks_unknown,
                    "chunks_total": chunks_total,
                    "tool_result_share_mean": tool_result_share_mean,
                    "source_missing": source_missing,
                    "source_unparsed": source_unparsed,
                    "source_unmatched": source_unmatched,
                },
                "tier_histogram": tier_histogram,
            }))?
        );
        return Ok(());
    }

    if let Some(Commands::Telemetry { since, json, tui }) = args.command {
        return csr_engine::telemetry::handle(&args.db_path, &args.projects_dir, since, json, tui);
    }

    if let Some(Commands::Lessons {
        project,
        since,
        min_sessions,
        json,
    }) = args.command
    {
        let storage = csr_engine::storage::Storage::open_read_only(&args.db_path)?;
        print!(
            "{}",
            csr_engine::eval::lessons::handle(
                &storage,
                &project,
                since.as_deref(),
                min_sessions,
                json,
            )?
        );
        if json {
            println!();
        }
        return Ok(());
    }

    if let Some(Commands::AuditRerankLabels {
        per_class,
        near_misses,
        json,
    }) = args.command
    {
        let storage = csr_engine::storage::Storage::open(&args.db_path)?;
        let Some(classifier_hash) = storage.latest_reaction_classifier_hash()? else {
            if json {
                println!("{{\"classifier_hash\":null,\"counts\":null,\"labels\":[]}}");
            } else {
                println!("No harvested reaction labels are available.");
            }
            return Ok(());
        };
        let counts = storage.rerank_reaction_label_counts(&classifier_hash)?;
        let labels =
            storage.audit_rerank_reaction_labels(&classifier_hash, per_class, near_misses)?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "classifier_hash": classifier_hash,
                    "counts": counts,
                    "labels": labels,
                }))?
            );
        } else {
            println!("classifier_hash: {classifier_hash}");
            println!(
                "counts: acceptance={} correction={} reask={} redirect={} abstain={} near_miss={}",
                counts.acceptance,
                counts.correction,
                counts.reask,
                counts.redirect,
                counts.abstain,
                counts.near_miss
            );
            for label in labels {
                println!(
                    "\n[{}] session={} assistant_turn={} next_user_turn={} proposed={} confidence={:.4} runner_up={:.4} margin={:.4} pickup_similarity={} near_miss={}\n{}",
                    label.reaction,
                    label.session_id,
                    label.assistant_turn,
                    label.next_user_turn,
                    label.proposed_reaction.as_deref().unwrap_or("none"),
                    label.confidence,
                    label.runner_up_score,
                    label.margin,
                    label
                        .pickup_similarity
                        .map_or_else(|| "none".to_string(), |value| format!("{value:.4}")),
                    label.near_miss,
                    label.next_user_text
                );
            }
        }
        return Ok(());
    }

    if let Some(Commands::Daemon {
        batch_size,
        batch_time,
        no_ai,
    }) = args.command
    {
        // The lock first: a second daemon (another launchd label, a terminal)
        // must lose here, cheaply, not after loading the engine and the model.
        let daemon_lock = csr_engine::daemon::Daemon::acquire_lock()?;
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let eng = engine::Engine::new_for_daemon(&args.db_path, &args.projects_dir).await?;
        // The daemon embeds on its very first extraction tick (no initial
        // sleep) — warm the model now, synchronously, instead of paying
        // the load on that first tick with no log line to explain the
        // stall.
        eng.embeddings().warm()?;
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
        let exit_code = daemon
            .run_with_lock(daemon_lock, startup_identity, startup_exe_path)
            .await?;
        if exit_code != 0 {
            // Skips `runtime::run_to_completion`'s bounded shutdown wait
            // (see that function's doc comment) — deliberately: the
            // daemon's own `run()` already completed its full graceful
            // teardown (shutdown flag, joined loops, bounded flush) before
            // returning this code, so nothing meaningful is left running
            // except the same kind of abandoned-blocking-task case that
            // wait already tolerates. Exiting immediately here, rather
            // than threading the code back through `Result<()>`, is what
            // makes it observable to launchd: this process's `Result` flows
            // through `run_to_completion` -> `fn main() -> Result<()>`,
            // which anyhow always maps to exit code 1 on `Err` and 0 on
            // `Ok(())` — there is no path for a specific non-zero success
            // code other than exiting directly.
            std::process::exit(exit_code);
        }
        return Ok(());
    }

    if let Some(Commands::RebuildIndex) = args.command {
        let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
        if eng.index_freshly_rebuilt() {
            // Engine::new's own cache-miss path already streamed a fresh,
            // compact index from SQLite and dumped it — the exact case an
            // operator running this command most needs to be fast in (first
            // run, or a corrupt manifest). Calling rebuild_search_index()
            // again here would be a second, fully redundant ~11-min pass
            // over the same corpus.
            //
            // `Engine::new` only logs that dump's failure as a non-fatal
            // warning (foreground processes must never block startup on
            // persistence), so without this call a failed dump would print
            // "rebuilt" and exit 0 while the on-disk cache stayed stale or
            // absent — every later startup would then rebuild inline for
            // ~11 min instead of loading the cache this command claimed to
            // have written. `flush_index_checked` is a no-op when the dump
            // already succeeded (nothing dirty) and otherwise re-attempts
            // and surfaces the error via `?`.
            eng.flush_index_checked().await?;
            let (chunks, reflections) = (
                eng.storage().count_chunk_embeddings().unwrap_or(0),
                eng.storage().count_reflection_embeddings().unwrap_or(0),
            );
            println!("rebuilt index: {chunks} chunks, {reflections} reflections");
            return Ok(());
        }
        let (chunks, reflections) = eng.rebuild_search_index().await?;
        println!("rebuilt index: {chunks} chunks, {reflections} reflections");
        return Ok(());
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
        ref as_of,
    }) = args.command
    {
        return csr_engine::dream::cli::handle(
            &args.db_path,
            project.as_deref(),
            json,
            no_llm,
            as_of.as_deref(),
        );
    }

    if let Some(Commands::ReactionShadow {
        ref action,
        ref fixture,
        max,
    }) = args.command
    {
        use csr_engine::reaction_shadow as rs;
        let root = rs::models_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
        let out = match action.as_str() {
            "parity" => rs::parity(
                &root,
                &fixture
                    .clone()
                    .unwrap_or_else(|| root.join("parity_dev40.jsonl")),
            )?,
            "status" => rs::status(&csr_engine::storage::Storage::open(&args.db_path)?, &root)?,
            "run" => serde_json::to_value(rs::run_cycle(
                &csr_engine::storage::Storage::open(&args.db_path)?,
                &args.projects_dir,
                &root,
                max,
                rs::DEFAULT_WINDOW_DAYS,
                None,
            )?)?,
            other => anyhow::bail!("unknown action {other}: use status, run or parity"),
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if let Some(Commands::Symbol {
        action:
            SymbolAction::Change {
                ref repo,
                ref file,
                ref symbol,
                ref oid,
                ref from,
                batch,
            },
    }) = args.command
    {
        if batch {
            csr_engine::dream::symbol_cli::run_batch(
                std::io::stdin().lock(),
                std::io::stdout().lock(),
            )?;
            return Ok(());
        }
        let (Some(repo), Some(file), Some(oid)) = (repo, file, oid) else {
            eprintln!("error: --repo, --file and --oid are required without --batch");
            std::process::exit(2);
        };
        let repo = repo.to_string_lossy();
        let request = csr_engine::dream::symbol_cli::ChangeRequest {
            repo: &repo,
            file,
            symbol: symbol.as_deref(),
            oid,
            from: from.as_deref(),
            summary: true,
        };
        return match csr_engine::dream::symbol_cli::symbol_change(&request) {
            Ok(answer) => {
                println!("{answer}");
                Ok(())
            }
            Err(message) => {
                println!("{}", serde_json::json!({ "error": message }));
                std::process::exit(1);
            }
        };
    }

    if let Some(Commands::Symbol {
        action:
            SymbolAction::Span {
                ref repo,
                ref file,
                ref at,
                stdin,
            },
    }) = args.command
    {
        let repo = repo.to_string_lossy();
        let mut held = Vec::new();
        if stdin {
            use std::io::Read;
            if let Err(e) = std::io::stdin().lock().read_to_end(&mut held) {
                println!("{}", serde_json::json!({ "error": format!("stdin: {e}") }));
                std::process::exit(1);
            }
        }
        return match csr_engine::dream::symbol_cli::symbol_span(
            &repo,
            file,
            at.as_deref(),
            stdin.then_some(held),
        ) {
            Ok(answer) => {
                println!("{answer}");
                Ok(())
            }
            Err(message) => {
                println!("{}", serde_json::json!({ "error": message }));
                std::process::exit(1);
            }
        };
    }

    if let Some(Commands::Codemap { ref action }) = args.command {
        use csr_engine::codemap::cli;
        return match action {
            CodemapAction::Add { path } => cli::add(&args.db_path, path),
            CodemapAction::Remove { path } => cli::remove(&args.db_path, path),
            CodemapAction::Scan {
                repo,
                all,
                json,
                session,
            } => cli::scan(&args.db_path, repo.as_deref(), *all, *json, *session),
            CodemapAction::Drain { repo, debounce } => cli::drain(
                &args.db_path,
                repo,
                std::time::Duration::from_secs(*debounce),
            ),
            CodemapAction::Status { json } => cli::status(&args.db_path, *json),
            CodemapAction::Watch { for_secs } => cli::watch(&args.db_path, *for_secs),
            CodemapAction::Query { text, json, limit } => {
                cli::query(&args.db_path, text, *json, *limit)
            }
        };
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
        include_system,
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
            include_system,
        };
        let data_dir = args.db_path.parent().map(std::path::Path::to_path_buf);
        // Read-only: no migrations, no write, and no database is created just
        // to answer. A missing or unreadable database leaves the JSONL and
        // mirror answers as they were.
        let db = if args.db_path.is_file() {
            csr_engine::storage::Storage::open_read_only(&args.db_path).ok()
        } else {
            None
        };
        let out = csr_engine::transcript::run_with_sources(
            &args.projects_dir,
            data_dir.as_deref(),
            db.as_ref(),
            &req,
        );
        print!("{out}");
        if !out.ends_with('\n') {
            println!();
        }
        return Ok(());
    }

    if let Some(Commands::Mirror { ref action }) = args.command {
        let data_dir = args
            .db_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        if let MirrorAction::Status = action {
            // Read-only: never create a database just to report on it.
            let status = match csr_engine::storage::Storage::open_read_only(&args.db_path) {
                Ok(storage) => csr_engine::status::MirrorStatus::for_cli(&storage),
                Err(_) => csr_engine::status::MirrorStatus::default(),
            };
            println!("{}", serde_json::to_string_pretty(&status)?);
            return Ok(());
        }
        std::fs::create_dir_all(&data_dir)?;
        let storage = csr_engine::storage::Storage::open(&args.db_path)?;
        let mirror = csr_engine::mirror::Mirror::new(&storage, &data_dir, &args.projects_dir);
        match action {
            MirrorAction::Sync => {
                if mirror.disabled {
                    println!("transcript mirror is off (CSR_NO_MIRROR is set); nothing to do");
                    return Ok(());
                }
                let stats = mirror.sync_pass(None);
                println!(
                    "files {}  synced {}  source_bytes {}  sealed {}  sources_gone {}  errors {}",
                    stats.files,
                    stats.synced,
                    stats.source_bytes,
                    stats.sealed,
                    stats.sources_gone,
                    stats.errors
                );
                if stats.errors > 0 {
                    std::process::exit(1);
                }
            }
            MirrorAction::Purge { yes } => {
                let report = mirror.purge(*yes)?;
                if report.deleted {
                    println!(
                        "deleted {} mirrored transcripts ({} compressed bytes) and their state rows",
                        report.files, report.zst_bytes
                    );
                } else {
                    println!(
                        "dry run: would delete {} mirrored transcripts ({} compressed bytes) under {} and clear their state rows; pass --yes to do it",
                        report.files,
                        report.zst_bytes,
                        csr_engine::mirror::transcripts_dir(&data_dir).display()
                    );
                }
            }
            MirrorAction::Status => unreachable!("handled above"),
        }
        return Ok(());
    }

    if let Some(Commands::Bench {
        format,
        data,
        queries,
        k,
        mode,
        out,
        drop_abstention,
        limit,
        stratify,
    }) = &args.command
    {
        let format = csr_engine::eval::bench::BenchFormat::parse(format)?;
        if !matches!(mode.as_str(), "hybrid" | "vector" | "fts") {
            anyhow::bail!("unknown --mode {mode:?}; expected hybrid, vector, or fts");
        }
        let mode = csr_engine::mcp::tools::search_mode_from(Some(mode));
        let table = csr_engine::eval::bench::run(csr_engine::eval::bench::BenchConfig {
            format,
            data: data.clone(),
            queries: queries.clone(),
            k: *k,
            mode,
            out: out.clone(),
            drop_abstention: *drop_abstention,
            limit: *limit,
            stratify: *stratify,
        })
        .await?;
        print!("{table}");
        return Ok(());
    }

    if let Some(Commands::Eval {
        full,
        refresh_recall_sample,
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
        // quick/full/continuity/provenance embed on their first step and fold
        // embedding errors into failed report rows, so load the model up front
        // and let a broken model cache fail the command loudly. The codegraph
        // modes only read storage; when codegraph is the selected branch
        // (nothing above it in this if-chain is set) the model stays unloaded.
        if continuity_live || continuity || provenance || !codegraph {
            eng.embeddings().warm()?;
        }
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
            csr_engine::eval::run_full_with(
                eng.storage(),
                eng.embeddings(),
                eng.search(),
                eng.index_dir(),
                refresh_recall_sample,
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
            // The CSR-text census is information, not one of the 24 results.
            print!(
                "{}",
                csr_engine::eval::contamination::census_result(eng.storage()).format_line()
            );
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
            // The whole report is printed first; then any gating failure,
            // like the provenance regression, is a nonzero exit.
            if report.has_gating_failure() || prov.regression {
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

    if let Some(Commands::BackfillCodexTimestamps) = args.command {
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Storage only — this backfill never touches embeddings or the HNSW
        // index, so it does not need a full `Engine` (1.2 GB load, possible
        // index rebuild/dump on a cache miss).
        let storage = csr_engine::storage::Storage::open(&args.db_path)?;
        let stats = csr_engine::import::codex_rollout::restamp_codex_chunk_timestamps(&storage)?;
        println!("{}", serde_json::to_string_pretty(&stats)?);
        return Ok(());
    }

    if let Some(Commands::BackfillCodexHarnessStrip) = args.command {
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
        let stats = csr_engine::import::codex_rollout::invalidate_polluted_codex_rollouts(&eng)?;
        println!("{}", serde_json::to_string_pretty(&stats)?);
        return Ok(());
    }

    if let Some(Commands::Backfill {
        action:
            BackfillAction::Intent {
                since,
                project,
                dry_run,
                ledger,
            },
    }) = &args.command
    {
        if std::env::var("CSR_NO_INTENT_CAPTURE").as_deref() == Ok("1") {
            println!("intent backfill: disabled (CSR_NO_INTENT_CAPTURE)");
            return Ok(());
        }
        let stats = if *dry_run {
            let embeddings = std::sync::Arc::new(
                csr_engine::embeddings::EmbeddingEngine::new()?
                    .with_probe_dir(args.db_path.parent().unwrap_or(std::path::Path::new("."))),
            );
            csr_engine::transcript::intent_events::backfill_intent_events(
                None,
                &embeddings,
                &args.projects_dir,
                since.as_deref(),
                project.as_deref(),
                true,
            )
            .await?
        } else {
            if let Some(parent) = args.db_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
            csr_engine::transcript::intent_events::backfill_intent_events(
                Some(eng.storage()),
                eng.embeddings(),
                &args.projects_dir,
                since.as_deref(),
                project.as_deref(),
                false,
            )
            .await?
        };
        print!("{}", stats.format_text(*dry_run));
        if let Some(ledger) = ledger {
            let agreement =
                csr_engine::transcript::intent_events::agreement_report(ledger, stats.events())?;
            print!("{}", agreement.format_text());
        }
        return Ok(());
    }

    if let Some(Commands::Backfill {
        action:
            BackfillAction::Scrub {
                dry_run,
                conversation,
                agent_transcripts,
                include_active,
                force,
            },
    }) = &args.command
    {
        // Scrub deletes nothing in this release: refuse before taking any lock
        // or opening anything for writing. `--dry-run` prints the plan.
        if !*dry_run {
            anyhow::bail!(csr_engine::import::scrub::SCRUB_REFUSAL);
        }
        let data_dir = args.db_path.parent().unwrap_or(std::path::Path::new("."));
        let lock = csr_engine::maintenance::MaintenanceLock::acquire(data_dir)?;
        // Preflight: the plan reads with no other process on the database, as
        // a writer would have needed. A hook from an older binary writes a
        // chunk's text and its vector in two statements no transaction can fence.
        let checked = csr_engine::maintenance::other_holders(&args.db_path);
        let holders = checked.as_ref().cloned().unwrap_or_default();
        let preflight = if let Err(error) = &checked {
            vec![format!(
                "preflight: could not check other processes: {error:#}"
            )]
        } else if holders.is_empty() {
            vec!["preflight: no other process holds the database".to_string()]
        } else {
            let mut lines = vec![format!(
                "preflight: {} other process(es) hold the database open:",
                holders.len()
            )];
            lines.extend(
                holders
                    .iter()
                    .map(|holder| format!("  pid {} {}", holder.pid, holder.executable)),
            );
            lines
        };
        if let (Err(error), false) = (&checked, *force) {
            anyhow::bail!(
                "refusing to scrub: cannot verify that no other process holds the database ({error:#}); rerun with --force"
            );
        }
        if !holders.is_empty() && !*force {
            for line in &preflight {
                eprintln!("{line}");
            }
            anyhow::bail!(
                "refusing to scrub while other processes hold the database: stop them (Claude Code sessions, the daemon, MCP servers) or rerun with --force"
            );
        }
        if !holders.is_empty() {
            eprintln!(
                "warning: --force: scrubbing while {} other process(es) hold the database; their writes can race it",
                holders.len()
            );
        }
        let mut report = {
            let storage = csr_engine::storage::Storage::open_read_only(&args.db_path)?;
            let report = csr_engine::import::scrub::dry_run_scrub(
                &storage,
                conversation.as_deref(),
                *agent_transcripts,
                *include_active,
            )?;
            drop(lock);
            report
        };
        report.preflight = preflight;
        print!("{}", report.format_text(true));
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
        action: CodegraphAction::Drain,
    }) = args.command
    {
        let storage = csr_engine::storage::Storage::open(&args.db_path)?;
        use csr_engine::extraction::graph_drain::{drain_outcome, Outcome};
        let mut failed = false;
        let report = match drain_outcome(&storage, &args.db_path) {
            Err(e) => {
                failed = true;
                serde_json::json!({ "drained": false, "error": format!("{e:#}") })
            }
            Ok(Outcome::Idle) => {
                serde_json::json!({ "drained": false, "reason": "nothing pending" })
            }
            Ok(Outcome::Contended) => serde_json::json!({
                "drained": false,
                "reason": "another process is draining (the drain lock is held)",
            }),
            Ok(Outcome::Drained(drain)) => {
                failed = !drain.errors.is_empty();
                serde_json::json!({
                "drained": true,
                "projects": drain.projects,
                "extracted": drain.extracted,
                "files": drain.files,
                "edge_writes": drain.edge_writes,
                "rank_writes": drain.rank_writes,
                "replans": drain.replans,
                "locked_passes": drain.locked_passes,
                "failed_projects": drain.errors.iter().map(|(p, e)| serde_json::json!({"project": p, "error": e})).collect::<Vec<_>>(),
                "ms": drain.elapsed.as_millis() as u64,
                "longest_lock_hold_ms": drain.longest_lock_hold.as_millis() as u64,
                })
            }
        };
        println!("{report}");
        if failed {
            std::process::exit(1);
        }
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
        ref action,
        dry_run,
        ref repo,
        report,
        ref out,
        no_open,
        ref mark,
    }) = args.command
    {
        if let Some(action) = action {
            return match action {
                DreamAction::Backfill {
                    project,
                    budget_calls,
                    dry_run: backfill_dry_run,
                    report: backfill_report,
                    stage,
                    funnel,
                } => csr_engine::dream::backfill::cli::handle_backfill(
                    &args.db_path,
                    project.as_deref(),
                    *budget_calls,
                    *backfill_dry_run,
                    *backfill_report,
                    *stage,
                    *funnel,
                ),
                DreamAction::Drain { n } => {
                    csr_engine::dream::backfill::cli::handle_drain(&args.db_path, *n)
                }
            };
        }

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
        let stats = if let Some(label) = mark {
            match csr_engine::dream::run_dream_marked(&eng, repo.as_deref(), dry_run, label)? {
                Some(stats) => stats,
                None => {
                    println!(
                        "CSR dream: another dream is in flight (marker active); \
                         skipping {label}-triggered cycle"
                    );
                    return Ok(());
                }
            }
        } else {
            csr_engine::dream::run_dream(&eng, repo.as_deref(), dry_run)?
        };
        // CSR_DREAM_CONSUMPTION (default `annotate`: search notes + status
        // counters; `full` adds the rank sink and the recap clause; `off`
        // hides the notes, clause and counters). The cycle runs and the
        // witness ledger updates for real either way — the switch is about
        // consumption/exposure, not about whether dreaming happens — so under
        // `off` the verdict-derived summary text (obsolete/superseded/
        // reinstated counts, events written) must not print.
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
        from_compaction,
        rewrite,
    }) = args.command
    {
        use csr_engine::summarizer::StoryMode;
        let mode = match (from_compaction, rewrite) {
            (true, _) => StoryMode::FromCompaction,
            (_, true) => StoryMode::Rewrite,
            _ => StoryMode::First,
        };
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
        return csr_engine::summarizer::generate_story_cli(&eng, transcript, cwd, mode).await;
    }

    if let Some(Commands::Hook {
        ref name,
        apply,
        ref session,
    }) = args.command
    {
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
                // Failures of the detached child land in a log (rotated at open past 1 MiB) under
                // the data dir instead of vanishing.
                .stderr(csr_engine::telemetry::detached_session_end_stderr());
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

        // The detached worker `hook stop` starts: it reads its input from the
        // spool, not from stdin, and builds its own engine.
        if name == "stop-work" {
            if let Some(parent) = args.db_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let Some(key) = session.as_deref() else {
                return Ok(());
            };
            // Nobody waits on this process, so it waits for maintenance to end. On
            // timeout the spool stays for the stale sweep.
            let data_dir = args
                .db_path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new("."));
            let Ok(_operation) =
                csr_engine::maintenance::wait_shared(data_dir, std::time::Duration::from_secs(600))
                    .await
            else {
                return Ok(());
            };
            return csr_engine::hooks::stop_work::worker_main(
                &args.db_path,
                &args.projects_dir,
                key,
            )
            .await;
        }

        // Other hooks need the engine
        if let Some(parent) = args.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Read the input before building the engine: whether this invocation
        // will query the index depends on it (a harness turn or a "continue"
        // never searches), and loading the whole HNSW graph is the dominant
        // cost of a hook process on a large corpus. Hooks that only import or
        // write run on the import-only engine, which skips that load and the
        // re-dump. Everyone else needs the loaded index.
        let stdin_started = std::time::Instant::now();
        let input = csr_engine::hooks::read_stdin_json();
        let stdin_elapsed = stdin_started.elapsed();
        // Stop returns at once: the transcript work moves to a detached worker.
        // Before the engine is built, so the fast path pays for none of it.
        // A worker that did not start leaves the spool and this process drains
        // that key under the worker's lock. Only the kill switch, a missing
        // session id or a failed spool write fall through to the inline handler
        // below, so no Stop is lost.
        if name == "stop" {
            match csr_engine::hooks::stop_work::detach_stop(
                &args.db_path,
                &args.projects_dir,
                &input,
                stdin_elapsed,
            ) {
                csr_engine::hooks::stop_work::Handoff::Detached => return Ok(()),
                csr_engine::hooks::stop_work::Handoff::DrainHere(key) => {
                    return csr_engine::hooks::stop_work::drain_here(
                        &args.db_path,
                        &args.projects_dir,
                        &key,
                    )
                    .await;
                }
                csr_engine::hooks::stop_work::Handoff::Inline => {}
            }
        }
        // PostToolUse now fires for every Bash call. One that wrote no file has
        // nothing to record: leave before the engine, the database and the
        // transcript import are touched.
        if csr_engine::hooks::post_tool_use::exits_before_engine(name, &input) {
            return Ok(());
        }
        let eng = {
            // Hook stdout is the injection channel. A stale or missing cache
            // makes `Engine::new` rebuild through hnsw_rs, which prints to
            // stdout; keep that off the channel until the handler owns it.
            let _quiet = csr_engine::hooks::StdoutQuarantine::begin();
            if let Some(proxied) = engine::Engine::new_proxied(&args.db_path, &args.projects_dir)? {
                proxied
            } else if csr_engine::hooks::hook_needs_index(name, &input) {
                engine::Engine::new_for_hook(&args.db_path, &args.projects_dir)?
            } else {
                engine::Engine::new_import_only(&args.db_path, &args.projects_dir)?
            }
        };
        csr_engine::hooks::dispatch_hook(name, &eng, input, stdin_elapsed).await?;
        return Ok(());
    }

    // Ensure DB directory exists
    if let Some(parent) = args.db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // `--bench` never reaches `serve_mcp`, so it must not claim stdout: a
    // claim that is dropped without `into_async_writer` ever being called
    // leaks the saved descriptor and leaves fd 1 pointed at stderr for
    // whatever's left of the process. Settling this ahead of the
    // `should_serve`/claim below (rather than after `Engine::new`, where it
    // used to sit) also means `--bench` no longer pays for building a whole
    // `Engine` just to print this line.
    if args.bench {
        tracing::info!("benchmark mode not yet implemented in main — use `cargo bench`");
        return Ok(());
    }

    // Whether this run serves MCP only depends on `args`, so it can be
    // computed before `Engine::new` — which it now has to be: stdout has to
    // be claimed for the JSON-RPC channel *before* `Engine::new` runs, since
    // that call is what triggers the index rebuild/backfill that can print
    // through hnsw_rs.
    let should_serve = args.serve
        || (!args.import && !args.bench && !args.watch && !args.enrich && !args.backfill_saga);

    // MCP stdout is the JSON-RPC transport for the lifetime of this process,
    // not just for the duration of `Engine::new`: the server also runs
    // `spawn_enrichment_loops` and, with `--watch`, the file importer,
    // in-process, and both insert into the same HNSW index that can print
    // through hnsw_rs at any point in the session, not only at startup. A
    // scoped guard around `Engine::new` alone (as the hook path uses) would
    // not cover that. Claim fd 1 for the rest of the process up front; `None`
    // for non-serving commands. A failed claim returns an error before startup.
    // Once the claim succeeds, `serve_mcp` is
    // reached unless the process instead exits on an error from `Engine::new`
    // or one of `--import`/`--enrich`/`--backfill-saga` via `?` — and in that
    // case the leaked descriptor dies with the process, since `--bench` (the
    // one branch that would otherwise return early past this point) has
    // already been handled above.
    let jsonrpc_stdout = if should_serve {
        Some(csr_engine::hooks::ClaimedStdout::require(
            csr_engine::hooks::ClaimedStdout::claim(),
        )?)
    } else {
        None
    };

    // A plain MCP server leaves the index, the model, imports and enrichment
    // to a live daemon. Explicit maintenance flags keep the full engine.
    let maintenance = args.import || args.enrich || args.backfill_saga;
    let proxied = if should_serve && !maintenance {
        engine::Engine::new_proxied(&args.db_path, &args.projects_dir)?
    } else {
        None
    };
    let eng = match proxied {
        Some(eng) => eng,
        None => engine::Engine::new(&args.db_path, &args.projects_dir)?,
    };

    if args.import || args.enrich {
        // Both paths embed heavily and immediately — warm eagerly instead
        // of paying the load cost inside the first batch with no log line
        // to explain it. `--serve`/hooks never hit this branch.
        eng.embeddings().warm()?;
    }

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
        // `--backfill-saga --serve` would now send this text to stderr (fd 1
        // was already claimed for JSON-RPC above). That combination is not a
        // meaningful invocation, so the redirected output is acceptable.
        print!("{}", stats.format_text());
    }

    // Start file watcher if requested (runs as background task)
    let _watcher_handle = if args.watch && eng.is_proxied() {
        tracing::info!("daemon live: it imports transcripts, --watch not started");
        None
    } else if args.watch {
        Some(eng.start_watcher())
    } else {
        None
    };

    if should_serve {
        eng.serve_mcp(jsonrpc_stdout, startup_identity, startup_exe_path)
            .await?;
    } else if args.watch {
        // If watching (with or without import), keep the process alive
        tracing::info!("watching for new conversations. Press Ctrl+C to stop.");
        tokio::signal::ctrl_c().await?;
    }

    Ok(())
}

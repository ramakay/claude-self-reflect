//! CSR text detector: which byte ranges of a stored text match a registered
//! CSR formatter's grammar?
//!
//! A grammar hit is a **label**, never an edit. A pattern cannot prove who typed
//! a line: two rounds of grammar fixes kept removing the person's own words, and
//! the design that could prove authorship (a receipt for every send) is not in
//! this release. So nothing here removes, rewrites or trims stored user or
//! assistant text. What the release does with a hit:
//!
//! - **search** annotates the chunk `[CSR text]` and, where dream verdicts
//!   already demote, sinks it the same way (never drops it);
//! - **hook injection** skips the chunk when choosing context (selection);
//! - **status** and `eval --full` count the chunks and conversations that hit.
//!
//! What stays structural (not grammar, not here): CSR's own MCP tool calls and
//! results are suppressed by tool name, CSR's own narrator and subprocess
//! transcripts are skipped by their first prompt, and a complete hook-labelled
//! `<system-reminder>` wrapper is cut by the importer exactly as before.
//!
//! The detector is linear in the text: one pass over its lines, each block
//! grammar bounded by the lines it consumes, the inline forms found by regexes
//! that the regex engine runs in linear time. It reports what it finds and
//! protects what is plainly a mention: fenced blocks, inline code, blockquote
//! lines, indented code, strings, and anything not at column zero.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::extraction::provenance::{DREAM_SENTINEL, LESSONS_SENTINEL, RECAP_SENTINEL};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContaminationReason {
    SystemReminder,
    Recap,
    DreamInjection,
    Lessons,
    MemoryManifest,
    Briefing,
    Continuity,
    Continuum,
    EpisodeIndex,
    RecentSessions,
    EmptyStatus,
    DegradedStatus,
    Pickup,
    InjectionSection,
    CodeMap,
    AnchorNotice,
    MemoryFeedback,
    AgentSubprocess,
    PluginDreamForward,
    PluginCompactionCard,
    PluginSessionStory,
    PluginSideNote,
    PluginKeepList,
    PluginCompactNotice,
    PluginCarry,
    PluginPaneRecall,
    PluginPaneReading,
    PluginStandingRule,
    PluginRecapForward,
    PluginLastSession,
    PluginLeftover,
    PluginCodeShift,
    PluginLostAndFound,
    PluginAgentsEnvelope,
    HarnessRow,
    CsrToolUse,
    CsrToolResult,
}

impl ContaminationReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SystemReminder => "system_reminder",
            Self::Recap => "recap",
            Self::DreamInjection => "dream_injection",
            Self::Lessons => "lessons",
            Self::MemoryManifest => "memory_manifest",
            Self::Briefing => "briefing",
            Self::Continuity => "continuity",
            Self::Continuum => "continuum",
            Self::EpisodeIndex => "episode_index",
            Self::RecentSessions => "recent_sessions",
            Self::EmptyStatus => "empty_status",
            Self::DegradedStatus => "degraded_status",
            Self::Pickup => "pickup",
            Self::InjectionSection => "injection_section",
            Self::CodeMap => "code_map",
            Self::AnchorNotice => "anchor_notice",
            Self::MemoryFeedback => "memory_feedback",
            Self::AgentSubprocess => "agent_subprocess",
            Self::PluginDreamForward => "plugin_dream_forward",
            Self::PluginCompactionCard => "plugin_compaction_card",
            Self::PluginSessionStory => "plugin_session_story",
            Self::PluginSideNote => "plugin_side_note",
            Self::PluginKeepList => "plugin_keep_list",
            Self::PluginCompactNotice => "plugin_compact_notice",
            Self::PluginCarry => "plugin_carry",
            Self::PluginPaneRecall => "plugin_pane_recall",
            Self::PluginPaneReading => "plugin_pane_reading",
            Self::PluginStandingRule => "plugin_standing_rule",
            Self::PluginRecapForward => "plugin_recap_forward",
            Self::PluginLastSession => "plugin_last_session",
            Self::PluginLeftover => "plugin_leftover",
            Self::PluginAgentsEnvelope => "plugin_agents_envelope",
            Self::PluginCodeShift => "plugin_code_shift",
            Self::PluginLostAndFound => "plugin_lost_and_found",
            Self::HarnessRow => "harness_row",
            Self::CsrToolUse => "csr_tool_use",
            Self::CsrToolResult => "csr_tool_result",
        }
    }
}

/// `text` without its complete hook-labelled `<system-reminder>` wrappers (the
/// ones whose payload opens with one of CSR's headers), counting them. This is
/// the importer's structural cut, kept exactly: it never reads a grammar of
/// the person's or Claude's own prose.
pub fn remove_hook_wrappers(text: &str, count: &mut usize) -> String {
    *count += WRAPPER_RE.find_iter(text).count();
    WRAPPER_RE.replace_all(text, "").to_string()
}

/// `text` with every detected range dropped. For derived artifacts only (a
/// summary must not learn CSR's own words); the stored conversation is never
/// passed through this.
pub fn strip_hits(text: &str) -> String {
    let hits = detect(text);
    if hits.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for hit in hits {
        out.push_str(&text[cursor..hit.start_byte]);
        cursor = hit.end_byte;
    }
    out.push_str(&text[cursor..]);
    out
}

/// One place a text matches a registered CSR formatter's grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The registry formatter id.
    pub formatter: &'static str,
    pub reason: ContaminationReason,
    pub start_byte: usize,
    pub end_byte: usize,
}

/// Every place `text` matches a registered CSR grammar, in order, without
/// overlap. Reports only: the text is not touched.
pub fn detect(text: &str) -> Vec<Hit> {
    if !TRIGGER_RE.is_match(text) {
        return Vec::new();
    }
    scan(text)
}

/// The reason of the first hit, if there is one.
pub fn first_reason(text: &str) -> Option<ContaminationReason> {
    detect(text).first().map(|hit| hit.reason)
}

/// Whether `text` holds any CSR text.
pub fn has_csr_text(text: &str) -> bool {
    !detect(text).is_empty()
}

/// Cheap prefilter: a text with none of the formatters' openings cannot hit.
pub fn could_contain_emission(text: &str) -> bool {
    TRIGGER_RE.is_match(text)
}

/// Whether a stored chunk is the kind of text CSR could have put in a
/// conversation: a Claude transcript (not a plan, not a Codex rollout) and not
/// pure tool output. Tool results that quote CSR text are mentions.
pub fn labelable(source: Option<&str>, author: Option<crate::provenance::Speaker>) -> bool {
    matches!(source, Some("conversation" | "sidechain"))
        && matches!(
            author,
            None | Some(crate::provenance::Speaker::User | crate::provenance::Speaker::Assistant)
        )
}

/// [`labelable`] for a chunk read without its source column: the conversation
/// id says plan and Codex chunks apart.
pub fn labelable_chunk(chunk: &crate::import::ConversationChunk) -> bool {
    !chunk.conversation_id.starts_with("codex:")
        && !chunk.conversation_id.starts_with("plan:")
        && chunk.author != crate::provenance::Speaker::ToolResult
}

/// The first hit in a chunk's text, when the chunk is one that can carry CSR
/// text. This is the one question search, hooks and status ask.
pub fn chunk_hit(chunk: &crate::import::ConversationChunk) -> Option<Hit> {
    text_hit(&chunk.conversation_id, Some(chunk.author), &chunk.content)
}

/// [`chunk_hit`] for a chunk whose author is known apart from the chunk (a
/// chunk read from the chunks table carries the non-authoritative default).
/// An author of `None` is unknown, as for a legacy chunk with no provenance
/// row: it can carry CSR text like any conversation chunk, and is labeled.
pub fn text_hit(
    conversation_id: &str,
    author: Option<crate::provenance::Speaker>,
    content: &str,
) -> Option<Hit> {
    if conversation_id.starts_with("codex:")
        || conversation_id.starts_with("plan:")
        || author == Some(crate::provenance::Speaker::ToolResult)
    {
        return None;
    }
    detect(content).into_iter().next()
}

// ---------------------------------------------------------------------------
// Formatter registry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emitter {
    Engine,
    Plugin,
}

/// How a registered formatter is recognised.
#[derive(Clone, Copy)]
enum Matcher {
    /// A column-zero block of whole lines: returns the exclusive end line.
    Block(fn(&Doc<'_>, usize) -> Option<usize>),
    /// A span that may start mid-line (wrappers, the companion's forwards).
    Inline,
    /// The whole component, decided by its opening bytes.
    Whole,
    /// Decided on the raw first prompt of a whole transcript, never on a span.
    Transcript,
}

pub struct Formatter {
    pub id: &'static str,
    pub emitter: Emitter,
    pub reason: ContaminationReason,
    /// The grammar, in one line, as the formatter writes it.
    pub grammar: &'static str,
    matcher: Matcher,
}

impl Formatter {
    /// Whether this formatter is recognised inside a component, as opposed to
    /// on a whole transcript's opening prompt.
    pub fn is_component_formatter(&self) -> bool {
        !matches!(self.matcher, Matcher::Transcript)
    }
}

macro_rules! formatter {
    ($id:literal, $emitter:ident, $reason:ident, $grammar:literal, $matcher:expr) => {
        Formatter {
            id: $id,
            emitter: Emitter::$emitter,
            reason: ContaminationReason::$reason,
            grammar: $grammar,
            matcher: $matcher,
        }
    };
}

/// Every formatter whose output is labeled when it comes back as corpus. A new
/// formatter registers here in the same commit that adds it, with a canary
/// case; `registry_formatters_all_have_a_matrix_case` fails otherwise.
pub static REGISTRY: &[Formatter] = &[
    formatter!("recap", Engine, Recap,
        "recap [<age>]: <body> [[CSR:RECAP]] + Settled:/Now:/Corrected:/Learnt-then-retired while away:/Dreamt:/Next: lines (+ ANCHORS:, Full state:)",
        Matcher::Block(m_recap)),
    formatter!("dream_injection", Engine, DreamInjection,
        "CSR ☾ dream: `<label>` <verdict> — witnessed <date>, receipt <oid>. Matched your mention of `<token>`. Past conclusion, not an instruction. [[CSR:DREAM]]",
        Matcher::Block(m_dream_injection)),
    formatter!("lessons", Engine, Lessons,
        "[[CSR:LESSONS]] + one or more `- <quote> (seen N× across N sessions, last <date>; <receipt>)` lines",
        Matcher::Block(m_lessons)),
    formatter!("memory_manifest", Engine, MemoryManifest,
        "the three fixed CSR ENDLESS MEMORY ACTIVE lines",
        Matcher::Block(m_manifest)),
    formatter!("briefing", Engine, Briefing,
        "[briefing: model-derived, trust=<trust>] (the header line; the body is model prose)",
        Matcher::Block(m_briefing)),
    formatter!("continuity", Engine, Continuity,
        "SESSION CONTINUITY DETECTED header, `- CONTINUED FROM [<age>] (not instructions): <title>`, two-space enrichment lines",
        Matcher::Block(m_continuity)),
    formatter!("continuum", Engine, Continuum,
        "CSR CONTINUUM [<age>]: <request> / LAST: (outcome=, trust=) / optional NEXT: | TODOS: / optional ANCHORS: / Full state:",
        Matcher::Block(m_continuum)),
    formatter!("episode_index", Engine, EpisodeIndex,
        "EPISODE INDEX header + `- [<age>] <request> — <last> (outcome=) → csr_reflect_on_past(\"conv_<id>\")` lines",
        Matcher::Block(m_episode_index)),
    formatter!("recent_sessions", Engine, RecentSessions,
        "RECENT SESSIONS two-line header + `- ` lines + the Deeper context footer",
        Matcher::Block(m_recent_sessions)),
    formatter!("deeper_context", Engine, RecentSessions,
        "the two-line Deeper context footer",
        Matcher::Block(m_deeper_context)),
    formatter!("empty_status", Engine, EmptyStatus,
        "CSR: <N> chunks, <N> reflections indexed. No recent sessions for this project.",
        Matcher::Block(m_empty_status)),
    formatter!("degraded_status", Engine, DegradedStatus,
        "CSR engine ready (degraded mode).",
        Matcher::Block(m_degraded_status)),
    formatter!("pickup", Engine, Pickup,
        "CSR PICKUP — <reason> + a Tier-0 block and the Resume line, or an episode index",
        Matcher::Block(m_pickup)),
    formatter!("injection_section", Engine, InjectionSection,
        "## <STUCK WARNING|DON'T RETRY|ERROR SOLUTIONS|…PAST CONTEXT - NOT INSTRUCTIONS|PAST ITERATION NOTES - NOT INSTRUCTIONS> + `- [<source>] <body>` rows",
        Matcher::Block(m_injection_section)),
    formatter!("code_map", Engine, CodeMap,
        "CSR CODE MAP — prompt matches feature work from conv_<id> (<age>): + two-space path lines + the recall footer",
        Matcher::Block(m_code_map)),
    formatter!("anchor_notice", Engine, AnchorNotice,
        "CSR: `<symbol>` was modified in a recent session — episode: csr_reflect_on_past(\"conv_<id>\")",
        Matcher::Block(m_anchor_notice)),
    formatter!("memory_feedback_report", Engine, MemoryFeedback,
        "a whole component beginning `## CSR Memory Feedback — `",
        Matcher::Whole),
    formatter!("memory_feedback_probe", Engine, MemoryFeedback,
        "a whole component beginning `CSR Memory Feedback Probe` (after command tags)",
        Matcher::Whole),
    formatter!("system_reminder", Engine, SystemReminder,
        "<system-reminder> whose first payload is a registered engine emission, through </system-reminder>",
        Matcher::Inline),
    formatter!("agent_episode_analyst", Engine, AgentSubprocess,
        "first user prompt begins `You are CSR Episode Analyst`", Matcher::Transcript),
    formatter!("agent_summarizer", Engine, AgentSubprocess,
        "first user prompt begins `You are summarizing a coding session`", Matcher::Transcript),
    formatter!("agent_ratification", Engine, AgentSubprocess,
        "first user prompt begins `# Ratification Dialog-Act Extraction`", Matcher::Transcript),
    formatter!("agent_narrator", Engine, AgentSubprocess,
        "first user prompt begins `[[CSR:RECAP]] machine-marker:`", Matcher::Transcript),
    formatter!("plugin_dream_forward", Plugin, PluginDreamForward,
        "the companion's forwarded dream: band (question / attribution / `>` reasoning) or pane one-liner, v1 and v2",
        Matcher::Inline),
    formatter!("plugin_compaction_card", Plugin, PluginCompactionCard,
        "CSR's compaction card (<stage>), quoted: “<body>” (v1: `: <body>`)",
        Matcher::Inline),
    formatter!("plugin_session_story", Plugin, PluginSessionStory,
        "CSR's story of this session, rev <N>, quoted: “<body>” (v1: `: <body>`)",
        Matcher::Inline),
    formatter!("plugin_side_note", Plugin, PluginSideNote,
        "Here is a note offered by a side agent:|CSR (claude-self-reflect): + a contiguous blockquote",
        Matcher::Block(m_side_note)),
    formatter!("plugin_keep_list", Plugin, PluginKeepList,
        "CSR COMPACT KEEP — … + numbered lines, or the nothing-runs sentence",
        Matcher::Block(m_keep_list)),
    formatter!("plugin_compact_notice", Plugin, PluginCompactNotice,
        "CSR compacted at <before>[ → <after>] · <clause> …",
        Matcher::Block(m_compact_notice)),
    formatter!("plugin_carry", Plugin, PluginCarry,
        "After the summary, add a section headed 'Reminders from CSR (not tasks)'… + one to eight `- ` lines",
        Matcher::Block(m_carry)),
    formatter!("plugin_recall", Plugin, PluginPaneRecall,
        "CSR remembered: “<text>”[ (<where>)]. | CSR remembered a past session: “<text>” (<where>). The rest of it: csr_reflect_on_past(\"conv_<id>\")",
        Matcher::Block(m_recall)),
    formatter!("plugin_reading", Plugin, PluginPaneReading,
        "Earlier you read my “<words>” as “<reading>”|unclear. Check… | Earlier you read me this way: “<line>”. Check…",
        Matcher::Block(m_reading)),
    formatter!("plugin_rule", Plugin, PluginStandingRule,
        "A standing rule from <date>: “<quote>”",
        Matcher::Block(m_rule)),
    formatter!("plugin_recap_forward", Plugin, PluginRecapForward,
        "CSR's recap at the start of this session: <recap>[ The rest: <full-state>]",
        Matcher::Block(m_recap_forward)),
    formatter!("plugin_last_session", Plugin, PluginLastSession,
        "Last session <id8> (“<title>”): <active> active, <prompts> prompts, <compacts> compactions, last row <time>. The rest of it: csr_reflect_on_past(\"conv_<id>\")",
        Matcher::Block(m_last_session)),
    formatter!("plugin_leftover", Plugin, PluginLeftover,
        "Left over from session <id8>: “<subject>” (<status>).",
        Matcher::Block(m_leftover)),
    formatter!("plugin_agents_v1", Plugin, PluginAgentsEnvelope,
        "[[CSR:AGENTS:v1]] / <one JSON line> / [[/CSR:AGENTS:v1]] (each marker line)",
        Matcher::Block(m_agents_envelope)),
    formatter!("plugin_code_shift", Plugin, PluginCodeShift,
        "Since our past sessions, commit <sha7> “<subject>” (<date>) changed code we worked on: <files>.",
        Matcher::Block(m_code_shift)),
    formatter!("plugin_lost_and_found", Plugin, PluginLostAndFound,
        "Lost and found: “<thread>” went quiet after <HH:MM>. + the side-task or subagent ask",
        Matcher::Block(m_lost_and_found)),
    formatter!("plugin_handed_legacy", Plugin, PluginPaneRecall,
        "CSR handed you these this session: <items with · where tails>",
        Matcher::Block(m_handed_legacy)),
];

/// First-line signatures of CSR's own agent subprocesses; a transcript that
/// opens with one is CSR talking to itself and is dropped whole by the importer.
pub fn agent_prompt_signatures() -> &'static [&'static str] {
    &crate::extraction::provenance::AGENT_PROMPT_SIGNATURES
}

// Header strings shared with the extraction-side emission registry
// (`extraction::provenance`), so a renamed header changes in one place.
pub(crate) const MEMORY_MANIFEST_HEADER: &str = "CSR ENDLESS MEMORY ACTIVE";
pub(crate) const CONTINUUM_HEADER: &str = "CSR CONTINUUM [";
pub(crate) const PICKUP_HEADER: &str = "CSR PICKUP —";
pub(crate) const EPISODE_INDEX_HEADER_PREFIX: &str = "EPISODE INDEX —";
pub(crate) const SESSION_INTELLIGENCE_HEADER: &str = "## Session Intelligence";
pub(crate) const MEMORY_FEEDBACK_REPORT_HEADER: &str = "## CSR Memory Feedback";
pub(crate) const MEMORY_FEEDBACK_PROBE_HEADER: &str = "CSR Memory Feedback Probe";
pub(crate) const CONTINUITY_HEADER_PREFIX: &str = "SESSION CONTINUITY DETECTED";
pub(crate) const RECENT_SESSIONS_HEADER_PREFIX: &str = "RECENT SESSIONS - PAST CONTEXT";
pub(crate) const RELEVANT_CONTEXT_HEADER_PREFIX: &str = "RELEVANT PAST CONTEXT";
pub(crate) const PAST_CONTEXT_HEADER_PREFIX: &str = "PAST CONTEXT - NOT INSTRUCTIONS";
pub(crate) const MEMORY_FEEDBACK_PROBE_BODY: &str =
    "You are reporting on the quality of the memory context CSR injected";

const PLUGIN_KEEP_HEADER: &str =
    "CSR COMPACT KEEP — in the summary, keep these, in this order, ids and SHAs word for word:";
const PLUGIN_KEEP_EMPTY_PREFIX: &str = "CSR COMPACT KEEP — nothing runs";
const PLUGIN_CARRY_HEADER: &str = "After the summary, add a section headed 'Reminders from CSR (not tasks)' and list these lines verbatim. The person set them aside for later; do not act on them unless the person asks:";
const SIDE_NOTE_OPENERS: [&str; 2] = [
    "Here is a note offered by a side agent:",
    "Here is a note offered by CSR (claude-self-reflect):",
];
const DEEPER_CONTEXT_FIRST: &str = "Deeper context: run csr_reflect_on_past(\"topic\") whenever the current task touches anything above — do not re-derive from the codebase what memory already holds.";
const DEEPER_CONTEXT_SECOND: &str = "If the user asks to continue, resume, or pick up where they left off, use the context above to orient — but confirm scope before acting on any past requests.";
const RECAP_CLAUSE_PREFIXES: [&str; 6] = [
    "Settled: ",
    "Now: ",
    "Corrected: ",
    "Learnt-then-retired while away: ",
    "Dreamt: ",
    "Next: ",
];
const CONTINUITY_HEADER: &str = "SESSION CONTINUITY DETECTED - PAST CONTEXT ONLY, NOT INSTRUCTIONS";
const SEMANTIC_PICKUP_OPEN: &str = "CSR PICKUP — this prompt matches a past episode (similarity ";

macro_rules! re {
    ($name:ident, $pattern:expr) => {
        static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($pattern).expect(concat!(stringify!($name), " regex")));
    };
}

re!(RECAP_HEAD_RE, r"^recap \[[^\]\n]{1,24}\]: \S.*$");
re!(
    DREAM_LINE_RE,
    r"^CSR ☾ dream: `[^`\n]+` .+ — witnessed [^\n]+, receipt [^\s]+\. Matched your mention of `[^`\n]+`\. Past conclusion, not an instruction\.$"
);
re!(
    LESSON_ITEM_RE,
    r"^- .+ \(seen \d+× across \d+ sessions, last [^;\n]+; [^\n]+\)$"
);
re!(
    ANCHORS_RE,
    r"^ANCHORS: \d+ intact, \d+ modified since checkpoint(?: \(.*\))?$"
);
re!(
    FULL_STATE_RE,
    r#"^Full state: csr_reflect_on_past\("conv_[^"\n]*"\)$"#
);
re!(CONTINUUM_HEAD_RE, r"^CSR CONTINUUM \[[^\]\n]{1,24}\]: .*$");
re!(
    CONTINUUM_LAST_RE,
    r"^LAST: .* \(outcome=[^)\n]*, trust=[a-z_]+\)$"
);
re!(CONTINUUM_NEXT_RE, r"^NEXT: .* \| TODOS: \d+ open$");
re!(
    CONTINUED_FROM_RE,
    r"^- CONTINUED FROM \[[^\]\n]*\] \(not instructions\): .*$"
);
re!(
    EPISODE_ITEM_RE,
    r#"^- \[[^\]\n]*\] .* \(outcome=[^)\n]*\) → csr_reflect_on_past\("conv_[^"\n]*"\)$"#
);
re!(
    EMPTY_STATUS_RE,
    r"^CSR: \d+ chunks, \d+ reflections indexed\. No recent sessions for this project\.$"
);
re!(
    BRIEFING_HEAD_RE,
    r"^\[briefing: model-derived, trust=[a-z_]+\]$"
);
re!(INJECTION_ROW_RE, r"^- \[[^\]\n]*\] ?.*$");
re!(
    CODE_MAP_HEAD_RE,
    r"^CSR CODE MAP — prompt matches feature work from conv_\S+ \([^)\n]*\):$"
);
re!(
    CODE_MAP_FOOTER_RE,
    r#"^(?:Read these before mapping; full thread|Full state): csr_reflect_on_past\("conv_[^"\n]*"\)$"#
);
re!(
    ANCHOR_NOTICE_RE,
    r#"^CSR: `[^`\n]+` was modified in a recent session — episode: csr_reflect_on_past\("conv_[^"\n]*"\)$"#
);
re!(
    COMPACT_NOTICE_RE,
    r"^CSR compacted at \S+(?: → \S+)?(?: · .+)*$"
);
re!(KEEP_ITEM_RE, r"^\d+\. .+$");
re!(RECALL_RE, r"^CSR remembered: “.+”(?: \(.*\))?\.$");
re!(
    RECALL_SESSION_RE,
    r#"^CSR remembered a past session: “.+” \(.*\)\. The rest of it: csr_reflect_on_past\("conv_[^"\n]*"\)$"#
);
re!(
    READING_RE,
    r"^Earlier you read (?:my “.+” as (?:“.+”|unclear)|me this way: “.+”)\. Check .+$"
);
// short_date() writes MM-DD, and the date is empty when the rule has no time.
// (A rule whose time did not parse is sent with the first ten characters of
// whatever it was given; that cannot be told from a sentence, so it is not
// labeled.)
re!(
    RULE_RE,
    r"^A standing rule from (?:\d{2}-\d{2}|\d{4}-\d{2}-\d{2})?: “.+”$"
);
// The pane strips the recap's sentinel before it sends (told_lines). A recap
// forward that opens with a recap line is the pane's; its plain-line fallback
// reads like a sentence and is not labeled.
re!(
    RECAP_FORWARD_RE,
    r"^CSR's recap at the start of this session: recap \[[^\]\n]{1,24}\]: \S.*$"
);
re!(
    LAST_SESSION_RE,
    r#"^Last session \S* \(“.*”\): .+ active, \d+ prompts, \d+ compactions, last row .*\. The rest of it: csr_reflect_on_past\("conv_[^"\n]*"\)$"#
);
re!(
    LEFTOVER_RE,
    r"^Left over from session [^\s:]*: “.+” \(.+\)\.$"
);
re!(AGENTS_ENVELOPE_RE, r"^\[\[/?CSR:AGENTS:v1\]\]$");
re!(
    CODE_SHIFT_RE,
    r"^Since our past sessions, commit \S{1,40} “.+” \([^)\n]+\) changed code we worked on: .*\.$"
);
re!(
    LOST_RE,
    r"^Lost and found: “.+” went quiet after \d{1,2}:\d{2}\. (?:Revisit it as a side task: finish or close it in a few steps without derailing the current work, then tell me in one line\.|Hand it to a background subagent with a five-part spec \(objective, files, interfaces, constraints, verification\), keep working on the current task, and report its result when it lands\.)$"
);
// A send `one_line` cut at its 700-character limit ends in `…` with no
// closing quote or period. Only the openings of the sends it can cut count, and
// each send keeps its own opening.
re!(CUT_RECALL_RE, r"^CSR remembered(?: a past session)?: “");
re!(CUT_READING_RE, r"^Earlier you read (?:my “|me this way: “)");
re!(
    CUT_RULE_RE,
    r"^A standing rule from (?:\d{2}-\d{2}|\d{4}-\d{2}-\d{2})?: “"
);
re!(
    CUT_CODE_SHIFT_RE,
    r"^Since our past sessions, commit [0-9a-f]{7,40} “"
);
re!(CUT_LAST_SESSION_RE, r"^Last session \S* \(“");
re!(CUT_LEFTOVER_RE, r"^Left over from session [^\s:]*: “");
const ONE_LINE_LIMIT: usize = 700;
re!(HANDED_RE, r"^CSR handed you these this session: .+ · .+$");
// The hook wrapper the importer has always cut, exactly as it did before the
// detector: a complete `<system-reminder>` whose payload opens with one of CSR's
// own headers.
re!(
    WRAPPER_RE,
    r"(?s)<system-reminder>[ \t\r\n]*(?:CSR ENDLESS MEMORY ACTIVE|CSR PICKUP —|EPISODE INDEX —|(?:RELEVANT )?PAST CONTEXT - NOT INSTRUCTIONS).*?</system-reminder>"
);
re!(
    SEMANTIC_PICKUP_RE,
    r"^CSR PICKUP — this prompt matches a past episode \(similarity [0-9.]+\); .+$"
);

// Every opening the matcher can act on. A text with none of these cannot
// yield a span, so the common case costs one scan of the component.
re!(
    TRIGGER_RE,
    concat!(
        r"recap \[|CSR|EPISODE INDEX|SESSION CONTINUITY|RECENT SESSIONS|## |\[briefing|<system-reminder>",
        r"|Here is a note offered by|After the summary|Earlier you read|A standing rule from",
        r"|Last session |Left over from|Since our past|Lost and found|Deeper context|PAST CONTEXT",
        r"|\[[\x{200B}-\x{200D}\x{2060}\x{FEFF}\x{00AD}]*\[|[\x{FF3B}\x{27E6}\x{2E28}]"
    )
);

// ---------------------------------------------------------------------------
// Document model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Line {
    start: usize,
    /// End of the line's content: before `\n` or `\r\n`.
    end: usize,
    /// Start of the next line (the text length for the last line).
    next: usize,
}

struct Doc<'a> {
    text: &'a str,
    lines: Vec<Line>,
    /// Bytes of a UTF-8 BOM at byte zero (0 or 3).
    bom: usize,
}

impl<'a> Doc<'a> {
    fn new(text: &'a str) -> Self {
        let bom = if text.starts_with('\u{FEFF}') { 3 } else { 0 };
        let bytes = text.as_bytes();
        let mut lines = Vec::new();
        let mut start = 0;
        while start < text.len() {
            let newline = bytes[start..].iter().position(|&b| b == b'\n');
            let (end, next) = match newline {
                Some(offset) => {
                    let newline_at = start + offset;
                    let end = if newline_at > start && bytes[newline_at - 1] == b'\r' {
                        newline_at - 1
                    } else {
                        newline_at
                    };
                    (end, newline_at + 1)
                }
                None => (text.len(), text.len()),
            };
            lines.push(Line { start, end, next });
            start = next;
        }
        Self { text, lines, bom }
    }

    fn len(&self) -> usize {
        self.lines.len()
    }

    /// The line's content, with a byte-zero BOM ignored on line 0.
    /// Past the last line there is nothing: an empty line, never a panic. A
    /// header cut off at the end of a document must not index beyond it.
    fn line(&self, index: usize) -> &'a str {
        let Some(line) = self.lines.get(index) else {
            return "";
        };
        let skip = if index == 0 { self.bom } else { 0 };
        &self.text[(line.start + skip).min(line.end)..line.end]
    }

    fn is_blank(&self, index: usize) -> bool {
        self.line(index).trim().is_empty()
    }

    fn line_of(&self, byte: usize) -> usize {
        self.lines
            .partition_point(|line| line.next <= byte)
            .min(self.lines.len().saturating_sub(1))
    }

    /// Byte offset where line `index` starts (the text length past the end).
    fn byte_of(&self, index: usize) -> usize {
        self.lines
            .get(index)
            .map_or(self.text.len(), |line| line.start)
    }
}

fn is_zero_width(c: char) -> bool {
    matches!(
        c,
        '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}' | '\u{00AD}'
    )
}

fn fold_bracket(c: char) -> char {
    match c {
        '\u{FF3B}' | '\u{27E6}' | '\u{2E28}' => '[',
        '\u{FF3D}' | '\u{27E7}' | '\u{2E29}' => ']',
        other => other,
    }
}

/// The text before `sentinel` when `line` ends with it, matching the sentinel
/// through zero-width noise and confusable brackets. The normalisation is
/// confined to the sentinel token: the returned prefix keeps its original
/// bytes, and the caller removes the whole line.
fn strip_sentinel_suffix<'a>(line: &'a str, sentinel: &str) -> Option<&'a str> {
    let end = line
        .trim_end_matches(|c: char| c.is_whitespace() || is_zero_width(c))
        .len();
    let mut wanted = sentinel.chars().rev();
    let mut next = wanted.next();
    let mut start = end;
    for (index, c) in line[..end].char_indices().rev() {
        let Some(expected) = next else { break };
        if is_zero_width(c) {
            continue;
        }
        if fold_bracket(c) != expected {
            return None;
        }
        start = index;
        next = wanted.next();
    }
    next.is_none().then(|| &line[..start])
}

// ---------------------------------------------------------------------------
// Block matchers: (document, first line) -> exclusive end line
// ---------------------------------------------------------------------------

fn lines_equal(doc: &Doc<'_>, at: usize, expected: &[&str]) -> Option<usize> {
    if at + expected.len() > doc.len() {
        return None;
    }
    expected
        .iter()
        .enumerate()
        .all(|(offset, line)| doc.line(at + offset) == *line)
        .then_some(at + expected.len())
}

fn m_recap(doc: &Doc<'_>, at: usize) -> Option<usize> {
    let prefix = strip_sentinel_suffix(doc.line(at), RECAP_SENTINEL)?;
    let head = prefix.trim_end_matches(|c: char| c.is_whitespace() || is_zero_width(c));
    if !RECAP_HEAD_RE.is_match(head) {
        return None;
    }
    let mut end = at + 1;
    while end < doc.len()
        && RECAP_CLAUSE_PREFIXES
            .iter()
            .any(|prefix| doc.line(end).starts_with(prefix))
    {
        end += 1;
    }
    // Tier-0 trailer: the same block carries ANCHORS and the Full state pointer.
    if end < doc.len() && ANCHORS_RE.is_match(doc.line(end)) {
        end += 1;
    }
    if end < doc.len() && FULL_STATE_RE.is_match(doc.line(end)) {
        end += 1;
    }
    Some(end)
}

fn m_dream_injection(doc: &Doc<'_>, at: usize) -> Option<usize> {
    let prefix = strip_sentinel_suffix(doc.line(at), DREAM_SENTINEL)?;
    let head = prefix.trim_end_matches(|c: char| c.is_whitespace() || is_zero_width(c));
    DREAM_LINE_RE.is_match(head).then_some(at + 1)
}

fn m_lessons(doc: &Doc<'_>, at: usize) -> Option<usize> {
    let prefix = strip_sentinel_suffix(doc.line(at), LESSONS_SENTINEL)?;
    if !prefix.trim().is_empty() {
        return None;
    }
    let mut end = at + 1;
    while end < doc.len() && LESSON_ITEM_RE.is_match(doc.line(end)) {
        end += 1;
    }
    // A bare sentinel line with no lessons under it is indistinguishable from
    // someone naming the token: it stays.
    (end > at + 1).then_some(end)
}

fn m_manifest(doc: &Doc<'_>, at: usize) -> Option<usize> {
    if !doc.line(at).starts_with(MEMORY_MANIFEST_HEADER) {
        return None;
    }
    let expected: Vec<&str> = crate::hooks::session_start::MEMORY_MANIFEST
        .lines()
        .take_while(|line| !line.is_empty())
        .collect();
    lines_equal(doc, at, &expected)
}

/// The briefing's header line. Its body is model prose with no end of its own
/// (paragraphs, lists, even fences), so the header is the label.
fn m_briefing(doc: &Doc<'_>, at: usize) -> Option<usize> {
    BRIEFING_HEAD_RE.is_match(doc.line(at)).then_some(at + 1)
}

fn m_continuity(doc: &Doc<'_>, at: usize) -> Option<usize> {
    if doc.line(at) != CONTINUITY_HEADER || !CONTINUED_FROM_RE.is_match(doc.line(at + 1)) {
        return None;
    }
    let mut end = at + 2;
    // Enrichment, `Last working on:` and `Pitfall (N%):` lines: exactly two
    // spaces, then text. Four spaces is code.
    while is_two_space_line(doc.line(end)) {
        end += 1;
    }
    Some(end)
}

fn is_two_space_line(line: &str) -> bool {
    line.strip_prefix("  ")
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| !c.is_whitespace())
}

fn m_continuum(doc: &Doc<'_>, at: usize) -> Option<usize> {
    if !CONTINUUM_HEAD_RE.is_match(doc.line(at)) || at + 1 >= doc.len() {
        return None;
    }
    if !CONTINUUM_LAST_RE.is_match(doc.line(at + 1)) {
        return None;
    }
    let mut end = at + 2;
    if end < doc.len() && CONTINUUM_NEXT_RE.is_match(doc.line(end)) {
        end += 1;
    }
    if end < doc.len() && ANCHORS_RE.is_match(doc.line(end)) {
        end += 1;
    }
    (end < doc.len() && FULL_STATE_RE.is_match(doc.line(end))).then_some(end + 1)
}

fn m_episode_index(doc: &Doc<'_>, at: usize) -> Option<usize> {
    let header = crate::hooks::session_start::EPISODE_INDEX_HEADER.trim_end_matches('\n');
    if doc.line(at) != header {
        return None;
    }
    let mut end = at + 1;
    while end < doc.len() && EPISODE_ITEM_RE.is_match(doc.line(end)) {
        end += 1;
    }
    (end > at + 1).then_some(end)
}

fn m_deeper_context(doc: &Doc<'_>, at: usize) -> Option<usize> {
    lines_equal(doc, at, &[DEEPER_CONTEXT_FIRST, DEEPER_CONTEXT_SECOND])
}

fn m_recent_sessions(doc: &Doc<'_>, at: usize) -> Option<usize> {
    if !doc.line(at).starts_with(RECENT_SESSIONS_HEADER_PREFIX) {
        return None;
    }
    let header: Vec<&str> = crate::hooks::session_start::RECENT_SESSIONS_HEADER
        .lines()
        .collect();
    let mut end = lines_equal(doc, at, &header)?;
    while end < doc.len() {
        let line = doc.line(end);
        // Session lines, their two-space continuations, and the blank line
        // that sets the footer apart.
        let before_footer =
            doc.is_blank(end) && end + 1 < doc.len() && doc.line(end + 1) == DEEPER_CONTEXT_FIRST;
        if line.starts_with("- ") || is_two_space_line(line) || before_footer {
            end += 1;
        } else {
            break;
        }
    }
    if let Some(footer_end) = m_deeper_context(doc, end) {
        end = footer_end;
    }
    Some(end)
}

fn m_empty_status(doc: &Doc<'_>, at: usize) -> Option<usize> {
    EMPTY_STATUS_RE.is_match(doc.line(at)).then_some(at + 1)
}

fn m_degraded_status(doc: &Doc<'_>, at: usize) -> Option<usize> {
    (doc.line(at) == crate::hooks::session_start::DEGRADED_STATUS).then_some(at + 1)
}

/// A complete Tier-0 block: the CONTINUUM fragments, or the recap paragraph
/// with its trailer.
fn m_tier0(doc: &Doc<'_>, at: usize) -> Option<usize> {
    m_continuum(doc, at).or_else(|| m_recap(doc, at))
}

fn m_pickup(doc: &Doc<'_>, at: usize) -> Option<usize> {
    let line = doc.line(at);
    if line
        .strip_prefix(PICKUP_HEADER)
        .and_then(|rest| rest.strip_prefix(' '))
        .is_none_or(str::is_empty)
    {
        return None;
    }
    if line.starts_with(SEMANTIC_PICKUP_OPEN) {
        return SEMANTIC_PICKUP_RE
            .is_match(line)
            .then(|| m_episode_index(doc, at + 1))
            .flatten();
    }
    let tier0_end = m_tier0(doc, at + 1)?;
    let resume = crate::hooks::prompt_submit::PICKUP_RESUME_LINE;
    Some(if tier0_end < doc.len() && doc.line(tier0_end) == resume {
        tier0_end + 1
    } else {
        tier0_end
    })
}

fn m_injection_section(doc: &Doc<'_>, at: usize) -> Option<usize> {
    let title = doc.line(at).strip_prefix("## ")?;
    if !crate::injection::formatter::INJECTION_SECTION_TITLES.contains(&title) {
        return None;
    }
    let free_form = title == crate::injection::formatter::STUCK_WARNING_TITLE;
    let mut end = at + 1;
    while end < doc.len() && !doc.is_blank(end) {
        let line = doc.line(end);
        // A warning's words are free, but a quote, code or a fence under the
        // heading is somebody's markdown, not the warning.
        if free_form && (line_kind(line) != LineKind::Normal || fence_open(line).is_some()) {
            break;
        }
        if !free_form && !INJECTION_ROW_RE.is_match(line) {
            break;
        }
        end += 1;
    }
    (end > at + 1).then_some(end)
}

fn m_code_map(doc: &Doc<'_>, at: usize) -> Option<usize> {
    if !CODE_MAP_HEAD_RE.is_match(doc.line(at)) {
        return None;
    }
    let mut end = at + 1;
    while end < doc.len() && is_two_space_line(doc.line(end)) {
        end += 1;
    }
    (end > at + 1 && end < doc.len() && CODE_MAP_FOOTER_RE.is_match(doc.line(end)))
        .then_some(end + 1)
}

fn m_anchor_notice(doc: &Doc<'_>, at: usize) -> Option<usize> {
    ANCHOR_NOTICE_RE.is_match(doc.line(at)).then_some(at + 1)
}

// --- companion-plugin block grammars ---

fn m_side_note(doc: &Doc<'_>, at: usize) -> Option<usize> {
    if !SIDE_NOTE_OPENERS.contains(&doc.line(at).trim_end()) {
        return None;
    }
    let mut end = at + 1;
    let mut last_quote = None;
    while end < doc.len() {
        let line = doc.line(end);
        if line.trim_start().starts_with('>') {
            last_quote = Some(end + 1);
        } else if !line.trim().is_empty() {
            break;
        }
        end += 1;
    }
    last_quote
}

fn m_keep_list(doc: &Doc<'_>, at: usize) -> Option<usize> {
    let line = doc.line(at);
    if line.starts_with(PLUGIN_KEEP_EMPTY_PREFIX) {
        return Some(at + 1);
    }
    if line != PLUGIN_KEEP_HEADER {
        return None;
    }
    let mut end = at + 1;
    while end < doc.len() && KEEP_ITEM_RE.is_match(doc.line(end)) {
        end += 1;
    }
    (end > at + 1).then_some(end)
}

fn one_line(doc: &Doc<'_>, at: usize, re: &Regex) -> Option<usize> {
    re.is_match(doc.line(at)).then_some(at + 1)
}

/// A whole send, or the send `one_line` cut at its limit (its end is gone, its
/// opening and length are not).
fn one_send(doc: &Doc<'_>, at: usize, re: &Regex, opening: &Regex) -> Option<usize> {
    let line = doc.line(at);
    let cut =
        line.ends_with('…') && line.chars().count() == ONE_LINE_LIMIT && opening.is_match(line);
    (re.is_match(line) || cut).then_some(at + 1)
}

fn m_compact_notice(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_line(doc, at, &COMPACT_NOTICE_RE)
}

fn m_carry(doc: &Doc<'_>, at: usize) -> Option<usize> {
    if doc.line(at) != PLUGIN_CARRY_HEADER {
        return None;
    }
    let mut end = at + 1;
    while end < doc.len() && end - (at + 1) < 8 && doc.line(end).starts_with("- ") {
        end += 1;
    }
    (end > at + 1).then_some(end)
}

fn m_recall(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_send(doc, at, &RECALL_RE, &CUT_RECALL_RE)
        .or_else(|| one_send(doc, at, &RECALL_SESSION_RE, &CUT_RECALL_RE))
}

fn m_reading(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_send(doc, at, &READING_RE, &CUT_READING_RE)
}

fn m_rule(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_send(doc, at, &RULE_RE, &CUT_RULE_RE)
}

fn m_recap_forward(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_line(doc, at, &RECAP_FORWARD_RE)
}

fn m_last_session(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_send(doc, at, &LAST_SESSION_RE, &CUT_LAST_SESSION_RE)
}

/// An agents-envelope marker line (bin/agents.py writes three lines; the JSON
/// between the markers carries no marker of its own, so each marker line is the
/// hit).
fn m_agents_envelope(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_line(doc, at, &AGENTS_ENVELOPE_RE)
}

fn m_leftover(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_send(doc, at, &LEFTOVER_RE, &CUT_LEFTOVER_RE)
}

fn m_code_shift(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_send(doc, at, &CODE_SHIFT_RE, &CUT_CODE_SHIFT_RE)
}

fn m_lost_and_found(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_line(doc, at, &LOST_RE)
}

fn m_handed_legacy(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_line(doc, at, &HANDED_RE)
}

// ---------------------------------------------------------------------------
// The scan
// ---------------------------------------------------------------------------

/// A component that is, from its first bytes, a memory-feedback report or the
/// command prompt that asks for one.
fn whole_component_reason(text: &str) -> Option<(ContaminationReason, bool)> {
    let body = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    if body.starts_with(&format!("{MEMORY_FEEDBACK_REPORT_HEADER} — ")) {
        return Some((ContaminationReason::MemoryFeedback, false));
    }
    // The probe arrives behind its command tags: `<command-message>…` lines.
    let mut rest = body;
    while rest.starts_with("<command-") {
        let newline = rest.find('\n')?;
        rest = &rest[newline + 1..];
    }
    let probe = format!("{MEMORY_FEEDBACK_PROBE_HEADER}\n{MEMORY_FEEDBACK_PROBE_BODY}");
    rest.replace("\r\n", "\n")
        .starts_with(&probe)
        .then_some((ContaminationReason::MemoryFeedback, true))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Normal,
    Quote,
    Indented,
}

fn line_kind(line: &str) -> LineKind {
    if line.starts_with('\t') || line.starts_with("    ") {
        LineKind::Indented
    } else if line.trim_start().starts_with('>') {
        LineKind::Quote
    } else {
        LineKind::Normal
    }
}

#[derive(Clone, Copy)]
struct Fence {
    marker: u8,
    length: usize,
}

fn fence_open(line: &str) -> Option<Fence> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let marker = *trimmed.as_bytes().first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let length = trimmed.bytes().take_while(|&b| b == marker).count();
    if length < 3 {
        return None;
    }
    // A backtick fence's info string cannot hold a backtick: that is inline code.
    (marker != b'`' || !trimmed[length..].contains('`')).then_some(Fence { marker, length })
}

fn fence_closes(line: &str, fence: Fence) -> bool {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return false;
    }
    let run = trimmed.bytes().take_while(|&b| b == fence.marker).count();
    run >= fence.length && trimmed[run..].trim().is_empty()
}

/// The spans of `line` that are inline code, in one pass: an opening run of
/// backticks closes at the next run of exactly its length; one with no closer
/// is literal text.
fn code_spans(line: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'`' {
            let len = bytes[at..].iter().take_while(|&&b| b == b'`').count();
            runs.push((at, len));
            at += len;
        } else {
            at += 1;
        }
    }
    let mut closer: Vec<Option<usize>> = vec![None; runs.len()];
    let mut nearest: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for k in (0..runs.len()).rev() {
        closer[k] = nearest.get(&runs[k].1).copied();
        nearest.insert(runs[k].1, k);
    }
    let mut spans = Vec::new();
    let mut k = 0;
    while k < runs.len() {
        match closer[k] {
            Some(m) => {
                spans.push((runs[k].0, runs[m].0 + runs[m].1));
                k = m + 1;
            }
            None => k += 1,
        }
    }
    spans
}

re!(TRIPLE_RE, r#""""|'''"#);

/// Which triple-quoted string (if any) is open after the tokens of `line` that
/// start before byte `until`, given the state `open` at the line's start.
fn triple_state(line: &str, until: usize, mut open: Option<&'static str>) -> Option<&'static str> {
    for token in TRIPLE_RE.find_iter(line) {
        if token.start() >= until {
            break;
        }
        let kind = if token.as_str() == "\"\"\"" {
            "\"\"\""
        } else {
            "'''"
        };
        open = match open {
            None => Some(kind),
            Some(current) if current == kind => None,
            other => other,
        };
    }
    open
}

/// What marks a candidate on one line as a mention: inside inline code, inside
/// a quoted string, inside a triple-quoted string. Computed once per line.
struct LineMarks<'a> {
    line: &'a str,
    code: Vec<(usize, usize)>,
    quotes: Vec<usize>,
    open_at_start: Option<&'static str>,
    /// Where each triple-quote token starts, and the state it leaves behind.
    triples: Vec<(usize, Option<&'static str>)>,
}

impl<'a> LineMarks<'a> {
    fn new(line: &'a str, open_at_start: Option<&'static str>) -> Self {
        let mut quotes = Vec::new();
        let mut escaped = false;
        for (at, c) in line.char_indices() {
            match c {
                '\\' if !escaped => {
                    escaped = true;
                    continue;
                }
                '"' if !escaped => quotes.push(at),
                _ => {}
            }
            escaped = false;
        }
        let mut triples = Vec::new();
        let mut open = open_at_start;
        for token in TRIPLE_RE.find_iter(line) {
            let kind = if token.as_str() == "\"\"\"" {
                "\"\"\""
            } else {
                "'''"
            };
            open = match open {
                None => Some(kind),
                Some(current) if current == kind => None,
                other => other,
            };
            triples.push((token.start(), open));
        }
        let mut code = code_spans(line);
        code.sort_unstable();
        Self {
            line,
            code,
            quotes,
            open_at_start,
            triples,
        }
    }

    fn in_code(&self, at: usize) -> bool {
        let before = self.code.partition_point(|&(start, _)| start <= at);
        self.code[..before]
            .iter()
            .rev()
            .take(1)
            .any(|&(_, end)| at < end)
    }

    /// Inside a double-quoted string: an odd number of unescaped quotes before it.
    fn in_string(&self, at: usize) -> bool {
        self.quotes.partition_point(|&quote| quote < at) % 2 == 1
    }

    fn in_triple(&self, at: usize) -> bool {
        let _ = self.line;
        let before = self.triples.partition_point(|&(start, _)| start < at);
        match before.checked_sub(1) {
            Some(last) => self.triples[last].1,
            None => self.open_at_start,
        }
        .is_some()
    }
}

/// What a mention is glued to when it is quoted, escaped or path-prefixed.
const GLUING_CHARS: [char; 16] = [
    '"', '\'', '`', '\\', '(', '[', '{', '<', '=', ':', '/', '|', '\u{201c}', '\u{2018}', '\u{ab}',
    '\u{300c}',
];

/// A candidate that starts mid-line must stand on its own: not glued to a
/// quote, an escape or a path prefix (`file.rs:68:`). Whitespace, the digit
/// the companion's hotkey leaves in the prompt, and anything else a person
/// types before it (CJK text has no spaces) all leave it standing alone.
fn standalone_start(line: &str, rel: usize) -> bool {
    rel == 0
        || line[..rel]
            .chars()
            .next_back()
            .is_some_and(|c| !GLUING_CHARS.contains(&c))
}

re!(
    QUOTED_HOTKEY_RE,
    r"^(?:>[ \t]*)+\d(?:CSR's dreamer|CSR's compaction card|CSR's story)"
);
re!(QUOTED_HOTKEY_PREFIX_RE, r"^(?:>[ \t]*)+\d");

/// Where, in a quote line, the companion's hotkey digit leaves a CSR send: the
/// byte (relative to the line) just past `> ... <digit>`, when one of the three
/// sends follows it. Decided once per line, however many candidates it holds.
fn quoted_hotkey_start(line: &str) -> Option<usize> {
    QUOTED_HOTKEY_RE.is_match(line).then_some(())?;
    QUOTED_HOTKEY_PREFIX_RE
        .find(line)
        .map(|prefix| prefix.end())
}

struct Candidate {
    start: usize,
    end: usize,
    formatter: &'static str,
    reason: ContaminationReason,
}

re!(
    DREAM_PANE_RE,
    r"CSR's dreamer asked this while you worked, quoted: “[^”\n]*” Its reasoning: “[^”\n]*”"
);
re!(
    DREAM_PANE_V1_RE,
    r"CSR's dreamer asked this while you worked: “[^”\n]*” Its reasoning: [^\n]*"
);
re!(
    CARD_RE,
    r"CSR's compaction card \((?:compacting soon|compacting|compaction skipped|compacted by csr)\)(?:, quoted: “[^”\n]*”|: [^\n]*)"
);
re!(
    STORY_RE,
    r"CSR's story of this session, rev \d+(?:, quoted: “[^”\n]*”|: [^\n]*)"
);
const BAND_QUOTED: &str = "CSR's dreamer asked this while you worked, quoted. Its reasoning:";
const BAND_LEGACY: [&str; 2] = [
    "CSR's dreamer asked this while you worked. Its reasoning:",
    "CSR's dreamer asked this while you worked. It is a question, not a task.",
];

/// The forms that may start mid-line or span lines, found by the regex engine
/// (linear in the text) and by one look at each band attribution line.
fn inline_candidates(text: &str, doc: &Doc<'_>) -> Vec<Candidate> {
    let mut found: Vec<Candidate> = Vec::new();
    let mut push = |re: &Regex, formatter: &'static str, reason: ContaminationReason| {
        for hit in re.find_iter(text) {
            found.push(Candidate {
                start: hit.start(),
                end: hit.end(),
                formatter,
                reason,
            });
        }
    };
    push(
        &WRAPPER_RE,
        "system_reminder",
        ContaminationReason::SystemReminder,
    );
    push(
        &DREAM_PANE_RE,
        "plugin_dream_forward",
        ContaminationReason::PluginDreamForward,
    );
    push(
        &DREAM_PANE_V1_RE,
        "plugin_dream_forward",
        ContaminationReason::PluginDreamForward,
    );
    push(
        &CARD_RE,
        "plugin_compaction_card",
        ContaminationReason::PluginCompactionCard,
    );
    push(
        &STORY_RE,
        "plugin_session_story",
        ContaminationReason::PluginSessionStory,
    );
    // The band's dream: the attribution line, with the question above it
    // and/or the quoted reasoning under it, each past a blank line.
    for at in 0..doc.len() {
        let line = doc.line(at).trim();
        let v2 = line == BAND_QUOTED;
        if !v2 && !BAND_LEGACY.contains(&line) {
            continue;
        }
        let mut below = at + 1;
        while below < doc.len() && doc.is_blank(below) {
            below += 1;
        }
        let mut end = below;
        if below > at + 1 {
            while end < doc.len() && doc.line(end).trim_start().starts_with('>') {
                end += 1;
            }
        }
        let reasoning = end > below;
        let mut above = at;
        while above > 0 && doc.is_blank(above - 1) {
            above -= 1;
        }
        let question = (above < at && above > 0).then(|| above - 1).filter(|&q| {
            let text = doc.line(q).trim_end();
            if v2 {
                text.ends_with('\u{201d}') && text.contains('\u{201c}')
            } else {
                text.ends_with(['?', '\u{ff1f}', '\u{61f}'])
            }
        });
        if !reasoning && question.is_none() {
            continue;
        }
        let start = question.map_or(doc.byte_of(at), |q| doc.byte_of(q));
        let stop = if reasoning {
            doc.byte_of(end)
        } else {
            doc.byte_of(at + 1)
        };
        found.push(Candidate {
            start,
            end: stop,
            formatter: "plugin_dream_forward",
            reason: ContaminationReason::PluginDreamForward,
        });
    }
    found.sort_by_key(|candidate| (candidate.start, candidate.end));
    found
}

/// Whether the lines under a block's first line hold something its formatter
/// never writes: a fence opener, a quote line, or code.
fn body_is_protected(doc: &Doc<'_>, start: usize, end: usize) -> bool {
    (start + 1..end.min(doc.len())).any(|line| {
        let text = doc.line(line);
        fence_open(text).is_some() || line_kind(text) != LineKind::Normal
    })
}

fn scan(text: &str) -> Vec<Hit> {
    if let Some(reason) = whole_component_reason(text) {
        return vec![Hit {
            formatter: if reason.1 {
                "memory_feedback_probe"
            } else {
                "memory_feedback_report"
            },
            reason: reason.0,
            start_byte: 0,
            end_byte: text.len(),
        }];
    }
    let doc = Doc::new(text);
    let candidates = inline_candidates(text, &doc);
    let mut next_candidate = 0;
    let mut hits: Vec<Hit> = Vec::new();
    let mut fence: Option<Fence> = None;
    let mut triple: Option<&'static str> = None;
    let mut pos = 0usize;
    // The marks of the line being scanned, computed once however many
    // candidates the line holds.
    let mut line_marks: Option<(usize, LineMarks<'_>)> = None;
    let mut hotkey_cache: Option<(usize, Option<usize>)> = None;

    'scan: while pos < text.len() {
        let index = doc.line_of(pos);
        let line = doc.lines[index];
        let content = doc.line(index);
        let content_start = line.end - content.len();
        let at_line_start = pos <= line.start;

        if at_line_start {
            if let Some(open) = fence {
                if fence_closes(content, open) {
                    fence = None;
                }
                pos = line.next;
                continue;
            }
            if let Some(open) = fence_open(content) {
                fence = Some(open);
                pos = line.next;
                continue;
            }
            // Inside a triple-quoted string every line is a string's.
            if triple.is_some() {
                triple = triple_state(content, usize::MAX, triple);
                pos = line.next;
                continue;
            }
        }
        let kind = line_kind(content);

        if at_line_start && kind == LineKind::Normal {
            for formatter in REGISTRY {
                let Matcher::Block(block) = formatter.matcher else {
                    continue;
                };
                let Some(end_line) = block(&doc, index) else {
                    continue;
                };
                // A block whose lines hold a fence, a quote or code its
                // formatter never writes has run into somebody's markdown.
                if formatter.reason != ContaminationReason::PluginSideNote
                    && body_is_protected(&doc, index, end_line)
                {
                    continue;
                }
                let end = doc.byte_of(end_line);
                hits.push(Hit {
                    formatter: formatter.id,
                    reason: formatter.reason,
                    start_byte: line.start,
                    end_byte: end,
                });
                pos = end;
                continue 'scan;
            }
        }

        // Inline candidates that start in this line, in order. One that is
        // protected is passed over; each is looked at once.
        while next_candidate < candidates.len() && candidates[next_candidate].start < pos {
            next_candidate += 1;
        }
        while next_candidate < candidates.len() && candidates[next_candidate].start < line.next {
            let candidate = &candidates[next_candidate];
            next_candidate += 1;
            let rel = candidate
                .start
                .saturating_sub(content_start)
                .min(content.len());
            if line_marks.as_ref().map(|(at, _)| *at) != Some(index) {
                line_marks = Some((index, LineMarks::new(content, triple)));
            }
            let marks = &line_marks.as_ref().expect("set above").1;
            let structural = candidate.reason == ContaminationReason::SystemReminder;
            if hotkey_cache.as_ref().map(|(at, _)| *at) != Some(index) {
                hotkey_cache = Some((index, quoted_hotkey_start(content)));
            }
            let quote_ok = kind != LineKind::Quote
                || hotkey_cache
                    .as_ref()
                    .and_then(|(_, start)| *start)
                    .is_some_and(|start| rel == start);
            let allowed = kind != LineKind::Indented
                && quote_ok
                && !marks.in_code(rel)
                && !marks.in_string(rel)
                && !marks.in_triple(rel)
                && standalone_start(content, rel)
                && (structural || !spans_a_fence(&text[candidate.start..candidate.end]));
            if allowed {
                hits.push(Hit {
                    formatter: candidate.formatter,
                    reason: candidate.reason,
                    start_byte: candidate.start,
                    end_byte: candidate.end,
                });
                pos = candidate.end;
                continue 'scan;
            }
        }
        triple = triple_state(content, usize::MAX, triple);
        pos = line.next;
    }
    hits
}

fn spans_a_fence(span: &str) -> bool {
    span.lines().any(|line| fence_open(line).is_some())
}

/// Formatter ids, for tests that check every registration has a matrix case.
pub fn registered_formatter_ids() -> impl Iterator<Item = &'static str> {
    REGISTRY.iter().map(|formatter| formatter.id)
}

#[cfg(test)]
mod tests;

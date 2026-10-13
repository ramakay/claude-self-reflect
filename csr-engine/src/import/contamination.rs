//! Ownership-aware CSR contamination: which byte ranges of a piece of text did
//! CSR's own formatters produce, and where could CSR have put them?
//!
//! The old predicate asked whether text *mentioned* a CSR marker and dropped
//! the paragraph or chunk that did. Quoting a marker (code, grep output, an
//! analysis, a plan) is not emitting one; a measured clone run dropped 552
//! chunks, at least 322 of them legitimate. This module removes only the spans
//! a registered formatter grammar accounts for, and only from channels CSR can
//! write into:
//!
//! - **Eligible**: Claude user text (every grammar) and assistant text (engine
//!   grammars, complete blocks only). Structural channels (CSR tool blocks,
//!   harness rows) are removed whole by the importer's own identity checks.
//! - **Never touched**: non-CSR tool output, Codex prose, plans, unknown
//!   sources. Those are mentions by construction.
//! - **Protected**: fenced blocks, inline code, blockquote lines, indented
//!   code, and inline candidates glued to a quote or a path prefix. A header
//!   must also sit at column zero; the exact two-space continuation lines the
//!   formatters emit belong to the block they continue.
//!
//! Retention is byte-exact: no trim, no paragraph splitting, no whole-text
//! sentinel rejection. A bare `[[CSR:RECAP]]` in prose is prose; it counts only
//! as the terminal token of a valid recap line.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::extraction::provenance::{DREAM_SENTINEL, LESSONS_SENTINEL, RECAP_SENTINEL};
use crate::provenance::{ChunkSpan, ProvenanceEvent, Speaker};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Conversation,
    Sidechain,
    CodexRollout,
    Plan,
    Unknown,
}

impl SourceKind {
    /// The `chunks.source` column. A missing or unrecognised value is
    /// `Unknown`, never `Conversation`.
    pub fn from_chunk_source(source: Option<&str>) -> Self {
        match source {
            Some("conversation") => Self::Conversation,
            Some("sidechain") => Self::Sidechain,
            Some("codex_rollout") => Self::CodexRollout,
            Some("plan") => Self::Plan,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Sidechain => "sidechain",
            Self::CodexRollout => "codex_rollout",
            Self::Plan => "plan",
            Self::Unknown => "unknown",
        }
    }

    fn is_claude_transcript(self) -> bool {
        matches!(self, Self::Conversation | Self::Sidechain)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentChannel {
    UserText,
    AssistantText,
    CsrToolUse,
    CsrToolResult,
    NonCsrToolResult,
    HarnessRow,
    CodexUser,
    CodexAssistant,
    CodexTool,
    PlanFile,
    Unknown,
}

impl ContentChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserText => "user_text",
            Self::AssistantText => "assistant_text",
            Self::CsrToolUse => "csr_tool_use",
            Self::CsrToolResult => "csr_tool_result",
            Self::NonCsrToolResult => "non_csr_tool_result",
            Self::HarnessRow => "harness_row",
            Self::CodexUser => "codex_user",
            Self::CodexAssistant => "codex_assistant",
            Self::CodexTool => "codex_tool",
            Self::PlanFile => "plan_file",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::UserText,
            Self::AssistantText,
            Self::CsrToolUse,
            Self::CsrToolResult,
            Self::NonCsrToolResult,
            Self::HarnessRow,
            Self::CodexUser,
            Self::CodexAssistant,
            Self::CodexTool,
            Self::PlanFile,
            Self::Unknown,
        ]
        .into_iter()
        .find(|channel| channel.as_str() == name)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OwnedText<'a> {
    pub source: SourceKind,
    pub channel: ContentChannel,
    pub text: &'a str,
}

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
            Self::PluginCodeShift => "plugin_code_shift",
            Self::PluginLostAndFound => "plugin_lost_and_found",
            Self::HarnessRow => "harness_row",
            Self::CsrToolUse => "csr_tool_use",
            Self::CsrToolResult => "csr_tool_result",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub reason: ContaminationReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    pub kept: String,
    pub removed: Vec<RemovedSpan>,
}

/// The reason of the first removed span; `None` when nothing is removable.
pub fn classify_csr_contamination(input: OwnedText<'_>) -> Option<ContaminationReason> {
    remove_csr_emissions(input)
        .removed
        .first()
        .map(|span| span.reason)
}

/// Remove the byte ranges CSR's formatters produced. Everything else is kept
/// exactly, byte for byte.
pub fn remove_csr_emissions(input: OwnedText<'_>) -> Removal {
    let spans = removal_spans(input);
    let mut kept = String::with_capacity(input.text.len());
    let mut cursor = 0;
    for span in &spans {
        kept.push_str(&input.text[cursor..span.start_byte]);
        cursor = span.end_byte;
    }
    kept.push_str(&input.text[cursor..]);
    Removal {
        kept,
        removed: spans,
    }
}

/// Whether `text` could contain anything the matcher would remove: the cheap
/// prefilter every classification starts with.
pub fn could_contain_emission(text: &str) -> bool {
    TRIGGER_RE.is_match(text)
}

/// The reason the matcher would remove something from `text` if it were Claude
/// user text. Used to count stored chunks that look like CSR output but whose
/// author cannot be proven: they are kept, never mutated.
pub fn classify_as_user_text(text: &str) -> Option<ContaminationReason> {
    classify_csr_contamination(OwnedText {
        source: SourceKind::Conversation,
        channel: ContentChannel::UserText,
        text,
    })
}

/// `text` with only CSR's hook wrappers removed (every other emission stays).
/// For reading a transcript's opening prompt, where a prompt behind a wrapper
/// must still lead but a sentinel-led narrator prompt must stay recognisable.
pub fn without_hook_wrappers(text: &str) -> String {
    let removal = remove_csr_emissions(OwnedText {
        source: SourceKind::Conversation,
        channel: ContentChannel::UserText,
        text,
    });
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for span in removal
        .removed
        .iter()
        .filter(|span| span.reason == ContaminationReason::SystemReminder)
    {
        out.push_str(&text[cursor..span.start_byte]);
        cursor = span.end_byte;
    }
    out.push_str(&text[cursor..]);
    out
}

// ---------------------------------------------------------------------------
// Stored-chunk ownership proof
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct StoredChunkEvidence<'a> {
    pub chunk_id: &'a str,
    pub conversation_id: &'a str,
    pub source: Option<&'a str>,
    pub content: &'a str,
    pub aggregate_author: Option<Speaker>,
    pub spans: &'a [ChunkSpan],
    pub events: &'a [ProvenanceEvent],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredOwnership {
    Proven(ContentChannel),
    Unproven(&'static str),
}

/// A source-gone stored chunk is attributable only when one event owns every
/// byte of it. `chunk_provenance.author` alone is never proof: it is the
/// highest-authority aggregate over the chunk, not byte ownership.
pub fn prove_stored_ownership(evidence: StoredChunkEvidence<'_>) -> StoredOwnership {
    if !matches!(evidence.source, Some("conversation" | "sidechain")) {
        return StoredOwnership::Unproven("source is not an explicit Claude transcript");
    }
    let [span] = evidence.spans else {
        return StoredOwnership::Unproven("chunk does not carry exactly one span");
    };
    let [event] = evidence.events else {
        return StoredOwnership::Unproven("chunk does not carry exactly one event");
    };
    if span.chunk_id != evidence.chunk_id || span.event_id != event.event_id {
        return StoredOwnership::Unproven("span does not name the event");
    }
    if event.conversation_id != evidence.conversation_id {
        return StoredOwnership::Unproven("event belongs to another conversation");
    }
    let channel = match event.channel.as_str() {
        "user_message" => ContentChannel::UserText,
        "assistant_message" => ContentChannel::AssistantText,
        _ => return StoredOwnership::Unproven("event channel is not user or assistant text"),
    };
    if span.end_char < span.start_char
        || span.end_char - span.start_char != evidence.content.chars().count()
    {
        return StoredOwnership::Unproven("span does not cover the whole chunk");
    }
    if span.content_hash != crate::provenance::content_hash(evidence.content) {
        return StoredOwnership::Unproven("span hash does not match the stored content");
    }
    let compatible = matches!(
        (evidence.aggregate_author, channel),
        (None, _)
            | (Some(Speaker::User), ContentChannel::UserText)
            | (Some(Speaker::Assistant), ContentChannel::AssistantText)
    );
    if !compatible {
        return StoredOwnership::Unproven("chunk author contradicts the event channel");
    }
    StoredOwnership::Proven(channel)
}

/// The shared stored-chunk classifier behind both the status count and scrub:
/// a reason only when ownership is proven and the matcher finds a span.
pub fn classify_stored_chunk(evidence: StoredChunkEvidence<'_>) -> Option<ContaminationReason> {
    let source = SourceKind::from_chunk_source(evidence.source);
    let content = evidence.content;
    match prove_stored_ownership(evidence) {
        StoredOwnership::Proven(channel) => classify_csr_contamination(OwnedText {
            source,
            channel,
            text: content,
        }),
        StoredOwnership::Unproven(_) => None,
    }
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

/// Every formatter whose output must never come back as corpus. A new
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
        "[briefing: model-derived, trust=<trust>] + its first paragraph",
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
pub(crate) const CODE_MAP_HEADER_PREFIX: &str = "CSR CODE MAP —";
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
re!(RULE_RE, r"^A standing rule from \d{4}-\d{2}-\d{2}: “.+”$");
re!(
    RECAP_FORWARD_RE,
    r"^CSR's recap at the start of this session: recap \[[^\]\n]{1,24}\]: .+ \[\[CSR:RECAP\]\](?: The rest: .+)?$"
);
re!(
    LAST_SESSION_RE,
    r#"^Last session \S{8} \(“.*”\): .+ active, \d+ prompts, \d+ compactions, last row .+\. The rest of it: csr_reflect_on_past\("conv_[^"\n]*"\)$"#
);
re!(
    LEFTOVER_RE,
    r"^Left over from session \S{8}: “.+” \([^)\n]+\)\.$"
);
re!(
    CODE_SHIFT_RE,
    r"^Since our past sessions, commit [0-9a-f]{7,40} “.+” \(\d{4}-\d{2}-\d{2}\) changed code we worked on: .+\.$"
);
re!(
    LOST_RE,
    r"^Lost and found: “.+” went quiet after \d{1,2}:\d{2}\. (?:Revisit it as a side task: finish or close it in a few steps without derailing the current work, then tell me in one line\.|Hand it to a background subagent with a five-part spec \(objective, files, interfaces, constraints, verification\), keep working on the current task, and report its result when it lands\.)$"
);
re!(
    HOOK_LABEL_RE,
    r"^[A-Za-z]+(?::[A-Za-z_-]+)? hook (?:success|additional context): "
);
re!(HANDED_RE, r"^CSR handed you these this session: .+ · .+$");
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
    fn line(&self, index: usize) -> &'a str {
        let line = self.lines[index];
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

    fn skip_blank(&self, mut index: usize) -> usize {
        while index < self.len() && self.is_blank(index) {
            index += 1;
        }
        index
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

fn m_briefing(doc: &Doc<'_>, at: usize) -> Option<usize> {
    if !BRIEFING_HEAD_RE.is_match(doc.line(at)) {
        return None;
    }
    // The body is the briefing text: its first paragraph. A raw copy carries no
    // envelope to say where a longer body ends, so what follows the first
    // blank line is left to its own grammar.
    let mut end = at + 1;
    while end < doc.len() && !doc.is_blank(end) {
        end += 1;
    }
    (end > at + 1).then_some(end)
}

fn m_continuity(doc: &Doc<'_>, at: usize) -> Option<usize> {
    if doc.line(at) != CONTINUITY_HEADER || at + 1 >= doc.len() {
        return None;
    }
    if !CONTINUED_FROM_RE.is_match(doc.line(at + 1)) {
        return None;
    }
    let mut end = at + 2;
    // Enrichment, `Last working on:` and `Pitfall (N%):` lines: exactly two
    // spaces, then text. Four spaces is code.
    while end < doc.len() && is_two_space_line(doc.line(end)) {
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
        if !free_form && !INJECTION_ROW_RE.is_match(doc.line(end)) {
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
    let mut quoted = false;
    while end < doc.len() {
        let line = doc.line(end);
        if line.trim_start().starts_with('>') {
            quoted = true;
        } else if !line.trim().is_empty() {
            break;
        }
        end += 1;
    }
    quoted.then_some(end)
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
    let line = doc.line(at);
    (RECALL_RE.is_match(line) || RECALL_SESSION_RE.is_match(line)).then_some(at + 1)
}

fn m_reading(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_line(doc, at, &READING_RE)
}

fn m_rule(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_line(doc, at, &RULE_RE)
}

fn m_recap_forward(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_line(doc, at, &RECAP_FORWARD_RE)
}

fn m_last_session(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_line(doc, at, &LAST_SESSION_RE)
}

fn m_leftover(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_line(doc, at, &LEFTOVER_RE)
}

fn m_code_shift(doc: &Doc<'_>, at: usize) -> Option<usize> {
    one_line(doc, at, &CODE_SHIFT_RE)
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

fn removal_spans(input: OwnedText<'_>) -> Vec<RemovedSpan> {
    use ContentChannel as C;
    let whole = |reason| {
        if input.text.is_empty() {
            Vec::new()
        } else {
            vec![RemovedSpan {
                start_byte: 0,
                end_byte: input.text.len(),
                reason,
            }]
        }
    };
    // Structural channels are identified by the importer from tool identity
    // and row metadata, never from their text: the whole component goes.
    if matches!(
        input.source,
        SourceKind::Conversation | SourceKind::Sidechain | SourceKind::CodexRollout
    ) {
        match input.channel {
            C::CsrToolUse => return whole(ContaminationReason::CsrToolUse),
            C::CsrToolResult => return whole(ContaminationReason::CsrToolResult),
            C::HarnessRow => return whole(ContaminationReason::HarnessRow),
            _ => {}
        }
    }
    if !input.source.is_claude_transcript()
        || !matches!(input.channel, C::UserText | C::AssistantText)
    {
        return Vec::new();
    }
    if !TRIGGER_RE.is_match(input.text) {
        return Vec::new();
    }
    let user = input.channel == C::UserText;
    if let Some(reason) = whole_component_reason(input.text) {
        return whole(reason);
    }
    if !user && assistant_briefing_echo(input.text) {
        return whole(ContaminationReason::Briefing);
    }
    scan(input.text, user)
}

/// A component that is, from its first bytes, a memory-feedback report or the
/// command prompt that asks for one.
fn whole_component_reason(text: &str) -> Option<ContaminationReason> {
    let body = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    if body.starts_with(&format!("{MEMORY_FEEDBACK_REPORT_HEADER} — ")) {
        return Some(ContaminationReason::MemoryFeedback);
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
        .then_some(ContaminationReason::MemoryFeedback)
}

/// An assistant echo of the briefing is removable only when the entire
/// component is that briefing.
fn assistant_briefing_echo(text: &str) -> bool {
    let body = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    body.starts_with(&format!("{SESSION_INTELLIGENCE_HEADER} (CSR v9.2)")) && !body.contains("\n\n")
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

/// Whether byte `at` of `line` lies inside an inline code span.
fn in_inline_code(line: &str, at: usize) -> bool {
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let run = bytes[index..].iter().take_while(|&&b| b == b'`').count();
        let after = index + run;
        let mut search = after;
        let mut close = None;
        while search < bytes.len() {
            if bytes[search] == b'`' {
                let candidate = bytes[search..].iter().take_while(|&&b| b == b'`').count();
                if candidate == run {
                    close = Some(search + candidate);
                    break;
                }
                search += candidate;
            } else {
                search += 1;
            }
        }
        match close {
            Some(end) => {
                if at >= index && at < end {
                    return true;
                }
                index = end;
            }
            None => index = after,
        }
    }
    false
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

/// Inline candidates need a line the person could have typed them into: not
/// inside a fence (handled by the caller), not code, not a blockquote, not
/// inside backticks. The companion's own hotkey marker (`>2` before a send) is
/// the one blockquote shape that is its bytes, not a quote of them.
fn inline_candidate_allowed(line: &str, kind: LineKind, rel: usize, span: &str) -> bool {
    match kind {
        LineKind::Indented => return false,
        LineKind::Quote if !QUOTED_HOTKEY_RE.is_match(span) => return false,
        _ => {}
    }
    !in_inline_code(line, rel) && standalone_start(line, rel)
}

struct Candidate {
    start: usize,
    end: usize,
    reason: ContaminationReason,
}

fn scan(text: &str, user: bool) -> Vec<RemovedSpan> {
    let doc = Doc::new(text);
    let forwards = if user {
        forward_candidates(text)
    } else {
        Vec::new()
    };
    let mut spans: Vec<RemovedSpan> = Vec::new();
    let mut fence: Option<Fence> = None;
    let mut pos = 0usize;

    'scan: while pos < text.len() {
        let index = doc.line_of(pos);
        let line = doc.lines[index];
        let content = doc.line(index);
        let content_start = line.end - content.len();
        let at_line_start = pos <= line.start;
        let pos_in_line = pos.max(line.start);

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
        }
        let kind = line_kind(content);

        if at_line_start && kind == LineKind::Normal {
            for formatter in REGISTRY {
                if !user && formatter.emitter == Emitter::Plugin {
                    continue;
                }
                let Matcher::Block(block) = formatter.matcher else {
                    continue;
                };
                let Some(end_line) = block(&doc, index) else {
                    continue;
                };
                let follows_removal = spans.last().is_some_and(|span| span.end_byte == line.start);
                if !user && !assistant_boundaries_ok(&doc, index, end_line, follows_removal) {
                    continue;
                }
                let end_line = doc.skip_blank(end_line);
                let end = doc.byte_of(end_line);
                spans.push(RemovedSpan {
                    start_byte: line.start,
                    end_byte: end,
                    reason: formatter.reason,
                });
                pos = end;
                continue 'scan;
            }
        }

        if user {
            // Inline candidates that start in this line, earliest first. One
            // that is protected, or that would swallow a fence line, is passed
            // over and the rest of the line is searched.
            let mut from = pos_in_line.max(content_start);
            while from < line.next {
                let wrapper = find_wrapper(text, from, line.end);
                let forward = forwards
                    .get(forwards.partition_point(|forward| forward.start < from))
                    .filter(|forward| forward.start < line.next)
                    .map(Candidate::from);
                let found = match (wrapper, forward) {
                    (Some(w), Some(f)) => Some(if f.start < w.start { f } else { w }),
                    (w, f) => w.or(f),
                };
                let Some(found) = found else { break };
                // Judge where the formatter's own bytes begin: a forward's span
                // can open on the separator that followed the person's words.
                let raw = &text[found.start..found.end];
                let begins = found.start + raw.len() - raw.trim_start().len();
                let rel = begins.saturating_sub(content_start).min(content.len());
                if inline_candidate_allowed(content, kind, rel, &text[begins..found.end])
                    && (found.reason == ContaminationReason::SystemReminder
                        || !spans_a_fence(&text[found.start..found.end]))
                {
                    let (start, end) = settle_span(&doc, &found);
                    spans.push(RemovedSpan {
                        start_byte: start,
                        end_byte: end,
                        reason: found.reason,
                    });
                    pos = end;
                    continue 'scan;
                }
                from = found.start + text[found.start..].chars().next().map_or(1, char::len_utf8);
            }
        }
        pos = line.next;
    }
    merge_overlaps(spans)
}

/// An assistant relay is a complete block standing alone: set off from the
/// text around it by blank lines or the component's edges. Blocks the engine
/// writes back to back (a whole SessionStart emission) stand alone together,
/// so a neighbouring engine block counts as a boundary too.
fn assistant_boundaries_ok(doc: &Doc<'_>, start: usize, end: usize, follows_removal: bool) -> bool {
    let before = follows_removal || start == 0 || doc.is_blank(start - 1);
    let after = end >= doc.len() || doc.is_blank(end) || engine_block_at(doc, end).is_some();
    before && after
}

/// The end of the engine block that starts at line `at`, if one does.
fn engine_block_at(doc: &Doc<'_>, at: usize) -> Option<usize> {
    REGISTRY
        .iter()
        .filter(|formatter| formatter.emitter == Emitter::Engine)
        .find_map(|formatter| match formatter.matcher {
            Matcher::Block(block) => block(doc, at),
            _ => None,
        })
}

fn spans_a_fence(span: &str) -> bool {
    span.lines().any(|line| fence_open(line).is_some())
}

/// Trim a candidate to the formatter's own bytes. A span that begins its line
/// also takes the line break and the blank lines under it, so the person's
/// next paragraph starts the retained text; mid-line spans leave their
/// neighbours exactly as typed.
fn settle_span(doc: &Doc<'_>, candidate: &Candidate) -> (usize, usize) {
    let text = doc.text;
    let raw = &text[candidate.start..candidate.end];
    let lead = raw.len() - raw.trim_start().len();
    let mut start = candidate.start + lead;
    let mut end = start + raw.trim().len();
    if end <= start {
        return (candidate.start, candidate.end);
    }
    let first = doc.line_of(start);
    let prefix_blank = text[doc.lines[first].start..start]
        .chars()
        .all(|c| c == ' ' || c == '\t' || c == '\u{FEFF}');
    if prefix_blank {
        start = doc.lines[first].start;
    }
    let last = doc.line_of(end.saturating_sub(1));
    let suffix_blank = text[end..doc.lines[last].end].trim().is_empty();
    if prefix_blank && suffix_blank {
        end = doc.byte_of(doc.skip_blank(last + 1));
    }
    (start, end)
}

fn merge_overlaps(mut spans: Vec<RemovedSpan>) -> Vec<RemovedSpan> {
    spans.sort_by_key(|span| (span.start_byte, span.end_byte));
    let mut merged: Vec<RemovedSpan> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            Some(last) if span.start_byte < last.end_byte => {
                last.end_byte = last.end_byte.max(span.end_byte);
            }
            _ => merged.push(span),
        }
    }
    merged
}

const WRAPPER_OPEN: &str = "<system-reminder>";
const WRAPPER_CLOSE: &str = "</system-reminder>";

/// The first `<system-reminder>` at or after `from` (within the line) whose
/// first nonblank payload is a registered engine emission, through its close.
fn find_wrapper(text: &str, from: usize, line_end: usize) -> Option<Candidate> {
    let mut search = from;
    while search < line_end {
        let open = search + text[search..line_end].find(WRAPPER_OPEN)?;
        let payload_start = open + WRAPPER_OPEN.len();
        let close = text[payload_start..].find(WRAPPER_CLOSE);
        if let Some(close) = close {
            let payload = text[payload_start..payload_start + close].trim_start();
            // The harness labels hook output it wraps: `SessionStart:startup
            // hook success: <the hook's stdout>`. The label is not CSR's; what
            // follows it decides.
            let payload = HOOK_LABEL_RE
                .find(payload)
                .map_or(payload, |label| &payload[label.end()..]);
            if starts_registered_emission(payload) {
                return Some(Candidate {
                    start: open,
                    end: payload_start + close + WRAPPER_CLOSE.len(),
                    reason: ContaminationReason::SystemReminder,
                });
            }
        }
        search = payload_start;
    }
    None
}

/// Whether `payload` opens with a header some engine formatter writes.
fn starts_registered_emission(payload: &str) -> bool {
    const HEADERS: [&str; 14] = [
        MEMORY_MANIFEST_HEADER,
        PICKUP_HEADER,
        EPISODE_INDEX_HEADER_PREFIX,
        CONTINUUM_HEADER,
        CONTINUITY_HEADER_PREFIX,
        RECENT_SESSIONS_HEADER_PREFIX,
        PAST_CONTEXT_HEADER_PREFIX,
        RELEVANT_CONTEXT_HEADER_PREFIX,
        CODE_MAP_HEADER_PREFIX,
        "[briefing: model-derived",
        "recap [",
        "CSR ☾ dream:",
        "CSR engine ready",
        "CSR: `",
    ];
    let first_line = payload.lines().next().unwrap_or("");
    if let Some(title) = first_line.strip_prefix("## ") {
        let title = title.trim_end();
        return crate::injection::formatter::INJECTION_SECTION_TITLES.contains(&title)
            || title.starts_with("CSR Memory Feedback");
    }
    HEADERS.iter().any(|header| payload.starts_with(header))
        || EMPTY_STATUS_RE.is_match(first_line)
        || strip_sentinel_suffix(first_line, LESSONS_SENTINEL)
            .is_some_and(|prefix| prefix.trim().is_empty())
}

struct Forward {
    start: usize,
    end: usize,
    reason: ContaminationReason,
}

impl From<&Forward> for Candidate {
    fn from(forward: &Forward) -> Self {
        Candidate {
            start: forward.start,
            end: forward.end,
            reason: forward.reason,
        }
    }
}

/// The companion's forwarded dream, compaction card and story, as byte spans
/// of `text`, sorted by start. One grammar, shared with extraction.
fn forward_candidates(text: &str) -> Vec<Forward> {
    let mut found: Vec<Forward> = crate::extraction::provenance::forward_spans(text)
        .into_iter()
        .filter(|span| span.start < span.end && span.end <= text.len())
        .map(|span| {
            let body = &text[span.start..span.end];
            let reason = if body.contains("CSR's compaction card (") {
                ContaminationReason::PluginCompactionCard
            } else if body.contains("CSR's story of this session, rev ") {
                ContaminationReason::PluginSessionStory
            } else {
                ContaminationReason::PluginDreamForward
            };
            Forward {
                start: span.start,
                end: span.end,
                reason,
            }
        })
        .collect();
    found.sort_by_key(|forward| (forward.start, forward.end));
    found
}

/// Formatter ids, for tests that check every registration has a matrix case.
pub fn registered_formatter_ids() -> impl Iterator<Item = &'static str> {
    REGISTRY.iter().map(|formatter| formatter.id)
}

#[cfg(test)]
mod tests;

//! Deterministic recap composition from a stored episode and evidence feeds.

use std::sync::LazyLock;

use regex::Regex;

static XML_TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]{1,50}>").unwrap());
static MD_HEADING_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"#{1,4}\s+").unwrap());
/// Bullet markers at the head of a flattened line. Lines are joined with
/// spaces before the recap is composed, so a markdown list would otherwise
/// read as `a: - one - two` in the middle of a sentence.
static MD_BULLET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(?:[-*+]|\d{1,3}[.)])\s+").unwrap());

/// Maximum open facts accepted by the composer and requested by storage callers.
pub const STILL_OPEN_LIMIT: usize = 3;

/// A resolution associated with the current project.
pub struct SettledFact {
    pub claim: String,
    pub receipt: String,
    pub status: String,
    pub source: String,
}

/// The one dream clause the recap may carry (Journal v4 P5, delivery channel
/// (b)). Constructed only from a stored, receipt-bearing conclusion — there
/// is no constructor that can produce one without a receipt, which is why
/// the clause cannot be emitted unreceipted.
pub struct DreamClause {
    /// Symbol or file basename the conclusion is about.
    pub label: String,
    /// Plain-English phrasing of the stored verdict.
    pub verdict: String,
    /// Commit receipt proving it. Mandatory.
    pub receipt: String,
    /// `YYYY-MM-DD` the verdict was witnessed.
    pub date: String,
}

/// A previously held line invalidated while the user was away.
pub struct RetiredLine {
    pub label: String,
    pub receipt_oid: String,
    pub date: String,
}

/// A recent user correction with its transcript receipt.
pub struct CorrectionLine {
    pub quote: String,
    pub session8: String,
    pub turn: u32,
    pub date: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrectionIdentity {
    pub session8: String,
    pub turn: u32,
    pub date: String,
}

impl From<&CorrectionLine> for CorrectionIdentity {
    fn from(line: &CorrectionLine) -> Self {
        Self {
            session8: line.session8.clone(),
            turn: line.turn,
            date: line.date.clone(),
        }
    }
}

pub struct ComposedRecap {
    pub text: String,
    pub corrections: Vec<CorrectionIdentity>,
}

/// Evidence feeds supplied by storage.
///
/// Feed text is trusted, already-curated database content: its semantic content
/// passes through without provenance filtering. Structural whitespace is
/// collapsed so a database value cannot create additional recap lines.
pub struct RecapFeeds {
    pub settled: Vec<SettledFact>,
    pub still_open: Vec<SettledFact>,
    pub retired_while_away: Vec<RetiredLine>,
    pub corrections: Vec<CorrectionLine>,
    pub open_proposals: usize,
    /// The top undelivered dream for this project, or `None`. `None` drops
    /// the `Dreamt:` clause entirely — the clause is never emitted with a
    /// placeholder, a hedge, or a receiptless label.
    pub top_dream: Option<DreamClause>,
    /// The one line saying Claude Code's own auto-compact window beat CSR this week and how to
    /// let CSR lead (`companion::native_warn`). `None` drops the clause. A line that is not in
    /// the emitted grammar (`provenance::is_native_warn_line`) is dropped too, so what the recap
    /// carries is always what re-import suppresses.
    pub native_warn: Option<String>,
}

/// The clause prefix the dream delivery uses. Exposed so the caller can
/// verify the clause actually survived the recap's character budget before
/// recording a delivery — recording one for a clause that got dropped would
/// claim the user was shown something they were not.
pub const DREAM_CLAUSE_PREFIX: &str = "Dreamt: ";
pub const CORRECTION_CLAUSE_PREFIX: &str = "Corrected: ";
/// The recap's character budget, sentinel included.
const MAX_CHARS: usize = 700;
/// The clause naming the session's literal last typed message.
pub const THEN_CLAUSE_PREFIX: &str = "Then: ";

/// Compose an evidence-backed recap without reading external state.
pub fn compose_recap(
    ep: &crate::hooks::stop::Episode,
    feeds: &RecapFeeds,
    age: &str,
) -> Option<String> {
    compose_recap_with_deliveries(ep, feeds, age).map(|composed| composed.text)
}

pub fn compose_recap_with_deliveries(
    ep: &crate::hooks::stop::Episode,
    feeds: &RecapFeeds,
    age: &str,
) -> Option<ComposedRecap> {
    let base = compose_recap_inner(ep, feeds, age, None)?;
    // The `Then:` clause is a pure addition: it is kept only when every other
    // line of the recap comes out exactly as it would without it.
    if let Some(then) = then_clause(ep) {
        if let Some(with) = compose_recap_inner(ep, feeds, age, Some(&then)) {
            if with.text.chars().count() <= MAX_CHARS
                && with
                    .text
                    .split('\n')
                    .skip(1)
                    .eq(base.text.split('\n').skip(1))
                && with.corrections == base.corrections
            {
                return Some(with);
            }
        }
    }
    Some(base)
}

fn compose_recap_inner(
    ep: &crate::hooks::stop::Episode,
    feeds: &RecapFeeds,
    age: &str,
    then: Option<&str>,
) -> Option<ComposedRecap> {
    use crate::extraction::provenance::{extractable, RECAP_SENTINEL};
    use crate::import::contamination::has_csr_text;

    let intent = recap_intent(ep);
    let completed = recap_completed(ep);
    if intent.is_none() && completed.is_none() {
        return None;
    }

    let mut recap = match (intent.as_deref(), completed.as_deref()) {
        // The receipts shown were all made under the anchor's own task (see
        // `anchor_completions`). A commit or a merge made during the anchor's
        // turn is not thereby its answer (a question about a workflow does not
        // become "answered" by a merge the session made meanwhile), so it gets
        // its own clause; only a task the session marked finished reads as the
        // anchor's result.
        (Some(intent), Some(completed)) if !completed_is_a_finished_task(ep) => {
            format!(
                "recap [{age}]: {} Latest: {}",
                terminate_clause(intent),
                terminate_clause(completed)
            )
        }
        (Some(intent), Some(completed)) => format!(
            "recap [{age}]: {}: {}",
            intent.trim_end().trim_end_matches(TRAILING_JUNK),
            terminate_clause(completed)
        ),
        (Some(intent), None) => format!("recap [{age}]: {}", terminate_clause(intent)),
        (None, Some(completed)) => format!("recap [{age}]: {}", terminate_clause(completed)),
        (None, None) => unreachable!("empty episodes abstain above"),
    };
    if let Some(then) = then {
        recap.push_str(&format!(" {THEN_CLAUSE_PREFIX}{}", terminate_clause(then)));
    }
    // Machine-owned sentinel, accounted for INSIDE the MAX_CHARS budget below
    // (not appended after trimming) — every downstream clause-drop decision
    // already sees its true cost via `recap.chars().count()`.
    recap.push(' ');
    recap.push_str(RECAP_SENTINEL);

    // User-confirmed verdicts first, then what the session itself landed
    // (commits and merges, by OID) that the head clause does not already name.
    let settled_entries: Vec<String> = feeds
        .settled
        .iter()
        .filter(|fact| fact.status == "resolved")
        .filter(|fact| fact.source == "user_confirmed")
        .filter(|fact| !normalize_feed_text(&fact.receipt).is_empty())
        .filter(|fact| !has_csr_text(&normalize_feed_text(&fact.claim)))
        .map(format_fact)
        .chain(session_settled(ep))
        .take(3)
        .collect();

    let mut now_parts = Vec::new();
    if let Some(blocker) = ep
        .blockers
        .as_deref()
        .and_then(extractable)
        .map(|text| compact_preview(&text, 120))
        .filter(|text| has_substance(text) && !has_csr_text(text))
    {
        now_parts.push(blocker);
    }
    now_parts.extend(gate_failure_entry(ep));
    now_parts.extend(background_entry(ep));
    now_parts.extend(
        feeds
            .still_open
            .iter()
            .filter(|fact| fact.status != "resolved")
            .filter(|fact| fact.source == "user_confirmed")
            .filter(|fact| !normalize_feed_text(&fact.receipt).is_empty())
            .filter(|fact| !has_csr_text(&normalize_feed_text(&fact.claim)))
            .take(STILL_OPEN_LIMIT)
            .map(format_fact),
    );
    if feeds.open_proposals > 0 {
        now_parts.push(format!(
            "{} proposals awaiting csr_resolve",
            feeds.open_proposals
        ));
    }
    let open_todos = ep
        .todos
        .iter()
        .filter(|todo| todo.status != "completed")
        .count();
    let todo_entry = (open_todos > 0).then(|| format!("{open_todos} todos open"));
    let priority_now_entries = usize::from(!now_parts.is_empty());
    now_parts.extend(todo_entry);
    let next = ep
        .next_steps
        .as_deref()
        .and_then(next_preview)
        .filter(|next| !has_csr_text(next))
        .or_else(|| {
            ep.todos
                .iter()
                .filter(|todo| todo.status != "completed")
                .find_map(|todo| next_preview(&todo.content))
        });

    let retired_entries: Vec<String> = feeds
        .retired_while_away
        .iter()
        .filter(|line| !normalize_feed_text(&line.receipt_oid).is_empty())
        .filter(|line| !has_csr_text(&normalize_feed_text(&line.label)))
        .take(2)
        .map(|line| {
            format!(
                "{} (superseded {}, {})",
                normalize_feed_text(&line.label),
                normalize_feed_text(&line.date),
                normalize_feed_text(&line.receipt_oid)
            )
        })
        .collect();
    let next_line = next.map(|next| format!("Next: {next}."));
    // The native-window warning: one line of its own, reserved like `Next:` when it fits at all.
    let warn_line = feeds
        .native_warn
        .as_deref()
        .map(normalize_feed_text)
        .filter(|line| crate::extraction::provenance::is_native_warn_line(line))
        .filter(|line| {
            recap.chars().count()
                + next_line
                    .as_ref()
                    .map_or(0, |next| next.chars().count() + 1)
                + line.chars().count()
                < MAX_CHARS
        });

    // One list serves what is shown and what is recorded as delivered.
    let shown_corrections: Vec<&CorrectionLine> = feeds
        .corrections
        .iter()
        .filter(|line| !has_csr_text(&line.quote))
        .take(2)
        .collect();
    let correction_entries: Vec<String> = shown_corrections
        .iter()
        .map(|line| {
            format!(
                "{} ({}:{}, {})",
                compact_preview(&normalize_feed_text(&line.quote), 90),
                normalize_feed_text(&line.session8),
                line.turn,
                normalize_feed_text(&line.date)
            )
        })
        .collect();

    // Journal v4 P5 delivery channel (b): one clause naming the top dream and
    // its receipt. Same evidence gating as every other clause — no feed, no
    // clause; and the feed itself cannot carry a receiptless conclusion.
    let dream_entries: Vec<String> = feeds
        .top_dream
        .iter()
        .filter(|dream| !normalize_feed_text(&dream.receipt).is_empty())
        .filter(|dream| has_substance(&normalize_feed_text(&dream.label)))
        .filter(|dream| !has_csr_text(&normalize_feed_text(&dream.verdict)))
        .map(|dream| {
            format!(
                "{} {} ({}, {})",
                normalize_feed_text(&dream.label),
                normalize_feed_text(&dream.verdict),
                normalize_feed_text(&dream.date),
                normalize_feed_text(&dream.receipt)
            )
        })
        .collect();

    let mut clauses = [
        ListClause::new("Settled: ", "; ", settled_entries),
        ListClause::new("Now: ", " | ", now_parts),
        ListClause::new(CORRECTION_CLAUSE_PREFIX, "; ", correction_entries),
        ListClause::new("Learnt-then-retired while away: ", "; ", retired_entries),
        ListClause::new(DREAM_CLAUSE_PREFIX, "; ", dream_entries),
    ];
    const SETTLED: usize = 0;
    const NOW: usize = 1;
    const CORRECTED: usize = 2;
    const RETIRED: usize = 3;
    const DREAMT: usize = 4;

    // Explicit evidence priority: Next is reserved above every list clause;
    // substantive Now entries precede the first Settled fact, then Corrected.
    // Lower-value extras consume only the residual budget.
    let used = recap.chars().count()
        + next_line
            .as_ref()
            .map_or(0, |line| line.chars().count() + 1)
        + warn_line
            .as_ref()
            .map_or(0, |line| line.chars().count() + 1);
    let mut remaining = MAX_CHARS.saturating_sub(used);
    for _ in 0..priority_now_entries {
        if !clauses[NOW].add_one_within(&mut remaining) {
            break;
        }
    }
    clauses[SETTLED].add_one_within(&mut remaining);
    while clauses[CORRECTED].add_one_within(&mut remaining) {}
    while clauses[SETTLED].add_one_within(&mut remaining) {}
    while clauses[RETIRED].add_one_within(&mut remaining) {}
    while clauses[DREAMT].add_one_within(&mut remaining) {}
    while clauses[NOW].add_one_within(&mut remaining) {}

    let rendered_corrections = shown_corrections
        .iter()
        .take(clauses[CORRECTED].selected)
        .map(|line| CorrectionIdentity::from(*line))
        .collect();

    for line in clauses
        .iter()
        .filter_map(ListClause::render)
        .chain(warn_line)
        .chain(next_line)
    {
        recap.push('\n');
        recap.push_str(&line);
    }

    Some(ComposedRecap {
        text: recap,
        corrections: rendered_corrections,
    })
}

struct ListClause {
    prefix: &'static str,
    separator: &'static str,
    entries: Vec<String>,
    selected: usize,
}

impl ListClause {
    fn new(prefix: &'static str, separator: &'static str, entries: Vec<String>) -> Self {
        Self {
            prefix,
            separator,
            entries,
            selected: 0,
        }
    }

    fn add_one_within(&mut self, remaining: &mut usize) -> bool {
        let Some(entry) = self.entries.get(self.selected) else {
            return false;
        };
        let needed = if self.selected == 0 {
            self.prefix.chars().count() + entry.chars().count() + 2
        } else {
            self.separator.chars().count() + entry.chars().count()
        };
        if needed > *remaining {
            return false;
        }
        *remaining -= needed;
        self.selected += 1;
        true
    }

    fn render(&self) -> Option<String> {
        (self.selected > 0).then(|| {
            format!(
                "{}{}.",
                self.prefix,
                self.entries[..self.selected].join(self.separator)
            )
        })
    }
}

fn format_fact(fact: &SettledFact) -> String {
    let claim = normalize_feed_text(&fact.claim);
    let claim = if claim.is_empty() {
        "evidence".to_string()
    } else {
        claim
    };
    format!("{claim} ({})", normalize_feed_text(&fact.receipt))
}

fn normalize_feed_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_next_prefix(text: &str) -> &str {
    let trimmed = text.trim_start();
    let lower = trimmed.to_lowercase();
    for prefix in ["next steps:", "next step:", "next:"] {
        if lower.starts_with(prefix) {
            return trimmed[prefix.len()..].trim_start();
        }
    }
    trimmed
}

fn next_preview(text: &str) -> Option<String> {
    let extracted = crate::extraction::provenance::extractable(text)?;
    let preview = compact_preview(strip_next_prefix(&extracted), 80);
    (!preview.is_empty() && !is_no_evidence_next(&preview)).then_some(preview)
}

/// Evidence requires at least one alphanumeric character: values like "…",
/// "/", or "-*-" are punctuation-only sentinels, not content, and every
/// clause gate must treat them as absent (abstention-first).
pub(crate) fn has_substance(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

pub(crate) fn is_no_evidence_next(text: &str) -> bool {
    if !has_substance(text) {
        return true;
    }
    let normalized = text
        .trim()
        .trim_matches(|character: char| character.is_ascii_punctuation() && character != '/')
        .trim()
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "" | "none" | "nothing" | "n/a" | "no next step" | "no next steps"
    )
}

fn compact_preview(content: &str, max_chars: usize) -> String {
    let clean = sanitize_preview(content);
    if clean.chars().count() <= max_chars {
        return clean;
    }
    truncate_at_word_boundary(&clean, max_chars)
}

/// Longest intent the recap renders. A latest task whose leading sentence is
/// longer abstains: the clause is never cut mid-sentence.
pub const INTENT_MAX_CHARS: usize = 160;

/// Legacy episodes (written before `recap_evidence_version`) only carry the
/// session's FIRST prompt. It stands for the current intent only in a session
/// this short; in a longer one it is a stale topic and the clause abstains.
const LEGACY_SINGLE_TOPIC_MAX_MESSAGES: usize = 40;

/// The old extractor cut `request` at this many bytes, mid-word.
const LEGACY_REQUEST_CAP_BYTES: usize = 200;

/// The intent clause: the latest substantive task, as whole sentences.
/// `None` when the episode has no clean, current intent. Shared with the
/// `CSR CONTINUUM` fallback so neither can name a stale first prompt.
pub(crate) fn recap_intent(ep: &crate::hooks::stop::Episode) -> Option<String> {
    use crate::extraction::provenance::extractable;
    let source = if ep.recap_evidence_version.is_some() {
        ep.anchor_request.clone()?
    } else {
        if ep.message_count > LEGACY_SINGLE_TOPIC_MAX_MESSAGES
            || ep.request.len() >= LEGACY_REQUEST_CAP_BYTES
        {
            return None;
        }
        ep.request.clone()
    };
    extractable(&source).and_then(|text| clean_intent(&text))
}

/// The short handoff `/clear` receives in place of a recap: what the cleared session was doing,
/// what it left open, and where the rest lives. Two lines, no history dump: the person asked for a
/// clean context. Carries [`RECAP_SENTINEL`](crate::extraction::provenance::RECAP_SENTINEL) and
/// the `recap [<age>]: ` opening, so the self-contamination guard drops it on re-import. Abstains
/// (None) when the session has no usable intent.
pub(crate) fn compose_clear_handoff(ep: &crate::hooks::stop::Episode, age: &str) -> Option<String> {
    use crate::extraction::provenance::RECAP_SENTINEL;
    use crate::import::contamination::has_csr_text;

    const OPEN_MAX_CHARS: usize = 120;
    let intent = recap_intent(ep)?;
    let open = ep
        .todos
        .iter()
        .find(|todo| todo.status != "completed" && !todo.content.trim().is_empty())
        .map(|todo| todo.content.as_str())
        .or(ep.next_steps.as_deref())
        .or(ep.blockers.as_deref())
        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|text| !text.is_empty() && !has_csr_text(text))
        .map(|text| {
            if text.chars().count() > OPEN_MAX_CHARS {
                let cut: String = text.chars().take(OPEN_MAX_CHARS).collect();
                format!("{}…", cut.trim_end())
            } else {
                text
            }
        });
    let mut out = format!("recap [cleared, {age}]: {}", terminate_clause(&intent));
    if let Some(then) = then_clause(ep) {
        out.push_str(&format!(" {THEN_CLAUSE_PREFIX}{}", terminate_clause(&then)));
    }
    if let Some(open) = open {
        out.push_str(&format!(" Still open: {}", terminate_clause(&open)));
    }
    out.push_str(&format!(
        "\nMore: csr_reflect_on_past(\"conv_{}\") {RECAP_SENTINEL}",
        ep.session_id
    ));
    Some(out)
}

/// The literal last typed message, as a `Then:` clause: whole leading
/// sentences, only when it reads as an ask and says something the displayed
/// intent does not. `None` means the recap names no later ask. It never looks
/// at an older message: with no evidence of which one the session was on, the
/// recap stays as it was.
pub(crate) fn then_clause(ep: &crate::hooks::stop::Episode) -> Option<String> {
    use crate::extraction::provenance::extractable;
    let intent = recap_intent(ep)?;
    let ask = ep
        .latest_ask
        .as_deref()
        .and_then(extractable)
        .and_then(|text| clean_intent(&text))?;
    if crate::import::contamination::has_csr_text(&ask) || !is_request(&ask) {
        return None;
    }
    let ask_key = word_key(&ask);
    let covered = |other: &str| {
        let other = word_key(other);
        !other.is_empty() && (other.contains(&ask_key) || ask_key.contains(&other))
    };
    let anchor = ep.anchor_request.as_deref().unwrap_or("");
    (!ask_key.is_empty() && !covered(&intent) && !covered(anchor)).then_some(ask)
}

/// Lowercase alphanumeric words joined by single spaces, for comparing two
/// texts regardless of case, punctuation and spacing.
fn word_key(text: &str) -> String {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Words that open a question.
const QUESTION_OPENERS: &[&str] = &[
    "what", "why", "how", "where", "when", "who", "which", "is", "are", "was", "were", "does",
    "do", "did", "can", "could", "should", "would", "will", "have", "has", "shall",
];

/// Verbs that open an instruction. Words that are just as often a noun
/// ("work", "test", "plan") are left out on purpose.
const COMMAND_OPENERS: &[&str] = &[
    "fix",
    "add",
    "make",
    "build",
    "run",
    "show",
    "tell",
    "explain",
    "review",
    "update",
    "remove",
    "delete",
    "create",
    "write",
    "implement",
    "verify",
    "find",
    "search",
    "try",
    "continue",
    "proceed",
    "commit",
    "push",
    "merge",
    "install",
    "open",
    "read",
    "compare",
    "summarize",
    "save",
    "rename",
    "enable",
    "disable",
    "investigate",
    "debug",
    "deploy",
    "ship",
    "revert",
    "rebase",
    "refactor",
    "send",
    "share",
    "ask",
    "check",
    "pull",
    "restart",
    "retry",
    "rerun",
    "see",
    "look",
    "list",
    "give",
    "get",
    "let",
    "set",
    "change",
    "move",
    "use",
    "stop",
    "start",
    "kill",
    "clean",
    "double-check",
    "look",
];

/// Words a command may be followed by: a determiner, pronoun or wh-word.
/// Prepositions and particles are left out: "Push to main failed" and "Fix
/// for the login bug landed" are remarks that open with a command word.
const COMMAND_FOLLOWERS: &[&str] = &[
    "the",
    "a",
    "an",
    "this",
    "that",
    "these",
    "those",
    "it",
    "them",
    "me",
    "us",
    "my",
    "our",
    "your",
    "its",
    "all",
    "every",
    "each",
    "any",
    "some",
    "everything",
    "anything",
    "if",
    "whether",
    "what",
    "how",
    "why",
    "where",
    "when",
    "which",
    "who",
];

/// Words that turn "<command> this ..." into a noun phrase about a time:
/// "Run this morning took two minutes".
const TIME_WORDS: &[&str] = &[
    "morning",
    "afternoon",
    "evening",
    "night",
    "week",
    "weekend",
    "month",
    "year",
    "time",
    "session",
    "round",
    "run",
    "one",
];

/// Leading words that carry no ask of their own: "ok so now fix the build".
const REQUEST_FILLERS: &[&str] = &[
    "ok", "okay", "so", "now", "then", "also", "and", "please", "pls", "hey", "well", "next",
    "first", "finally", "actually", "alright", "yes", "yeah",
];

/// True when `text` is clearly an ask. It fails closed: a miss costs a clause
/// that was not shown, a false hit names a remark as the session's next step.
///
/// Three shapes count, each read from the opening words after any filler:
/// a question whose FIRST sentence ends in `?` and opens with a question word;
/// `can/could/would/will you|we ...` and `I want|need you to ...`; and an
/// instruction that opens with a command verb followed by a determiner,
/// pronoun or wh-word (or a path). A question word buried inside a statement
/// ("I already know how many workers finished") never counts, and neither
/// does a noun phrase that opens with a command word.
pub(crate) fn is_request(text: &str) -> bool {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '/' && c != '`')
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect();
    let start = words
        .iter()
        .position(|word| !REQUEST_FILLERS.contains(&word.as_str()))
        .unwrap_or(words.len());
    let words = &words[start..];
    let Some(opener) = words.first().map(String::as_str) else {
        return false;
    };
    let second = words.get(1).map(String::as_str).unwrap_or("");

    // A question: the first sentence ends with '?'.
    let first_sentence_is_question = text
        .split_once('?')
        .is_some_and(|(before, _)| !before.contains(['.', '!']));
    if first_sentence_is_question && words.len() >= 3 && QUESTION_OPENERS.contains(&opener) {
        return true;
    }
    if words.len() >= 4
        && ((matches!(opener, "can" | "could" | "would" | "will")
            && matches!(second, "you" | "we"))
            || (matches!(opener, "i" | "we")
                && matches!(second, "want" | "need")
                && words.get(2).map(String::as_str) == Some("you")
                && words.get(3).map(String::as_str) == Some("to")))
    {
        return true;
    }
    if (opener == "let's" || opener == "lets") && words.len() >= 3 {
        return true;
    }
    let third = words.get(2).map(String::as_str).unwrap_or("");
    COMMAND_OPENERS.contains(&opener)
        && words.len() >= 2
        && (COMMAND_FOLLOWERS.contains(&second) || second.starts_with(['/', '`']))
        && !(matches!(second, "this" | "that") && TIME_WORDS.contains(&third))
}

/// What the session itself landed: its strongest, latest receipts (commits and
/// merged PRs by OID, else completed tasks) that the head clause does not
/// already carry, as `<claim> (<receipt>)`. Needs no ledger verdict: a receipt
/// is a commit the transcript saw succeed.
fn session_settled(ep: &crate::hooks::stop::Episode) -> Vec<String> {
    let shown: Vec<String> = {
        let own = anchor_completions(ep);
        displayed_receipts(&own)
            .into_iter()
            .filter_map(|(index, _)| own.get(index).map(|done| done.receipt.clone()))
            .collect()
    };
    displayed_receipts(&ep.completions)
        .into_iter()
        .filter(|(index, _)| {
            ep.completions
                .get(*index)
                .is_some_and(|done| !shown.contains(&done.receipt))
        })
        .map(|(_, entry)| entry)
        .collect()
}

/// A verification command the latest task left failing: the command and the
/// exit code its result printed are the receipt.
fn gate_failure_entry(ep: &crate::hooks::stop::Episode) -> Option<String> {
    let failure = ep.recap_extras.gate_failure.as_ref()?;
    let command = normalize_feed_text(&failure.command);
    (has_substance(&command) && !crate::import::contamination::has_csr_text(&command)).then(|| {
        format!(
            "last gate `{command}` failed (exit {}), not rerun green",
            failure.exit_code
        )
    })
}

/// Background tasks the transcript never shows finishing.
fn background_entry(ep: &crate::hooks::stop::Episode) -> Option<String> {
    let extras = &ep.recap_extras;
    let newest = extras.background_open.first()?;
    let label = normalize_feed_text(&newest.label);
    let id = normalize_feed_text(&newest.id);
    if id.is_empty() || !has_substance(&label) || crate::import::contamination::has_csr_text(&label)
    {
        return None;
    }
    let total = extras
        .background_open_total
        .max(extras.background_open.len());
    let more = ep.recap_extras.background_open_more;
    Some(if total > 1 || more {
        let plus = if more { "+" } else { "" };
        format!("{total}{plus} background tasks not reported finished, newest: {label} ({id})")
    } else {
        format!("background task not reported finished: {label} ({id})")
    })
}

/// The receipts the recap may show: those produced under the task the intent
/// quotes. A commit made under a later or an earlier ask says nothing about
/// this one, so it never stands beside it. An episode written before the tie
/// was recorded shows all its receipts when they were the anchor's own, and
/// none when they were not.
fn anchor_completions(
    ep: &crate::hooks::stop::Episode,
) -> Vec<crate::hooks::stop::CompletionReceipt> {
    match &ep.anchor_receipts {
        Some(indices) => indices
            .iter()
            .filter_map(|&index| ep.completions.get(index).cloned())
            .collect(),
        None if ep.completions_untied => Vec::new(),
        None => ep.completions.clone(),
    }
}

/// The completed clause: the anchor's own `displayed_receipts`, never
/// assistant prose. Legacy episodes carry none and abstain.
fn recap_completed(ep: &crate::hooks::stop::Episode) -> Option<String> {
    let entries: Vec<String> = displayed_receipts(&anchor_completions(ep))
        .into_iter()
        .map(|(_, entry)| entry)
        .collect();
    (!entries.is_empty()).then(|| entries.join("; "))
}

/// Whether the completed clause is a task the session marked finished (the
/// receipts shown are all of one kind, the strongest the session has).
fn completed_is_a_finished_task(ep: &crate::hooks::stop::Episode) -> bool {
    let own = anchor_completions(ep);
    displayed_receipts(&own)
        .first()
        .and_then(|(index, _)| own.get(*index))
        .is_some_and(|done| receipt_kind(&done.receipt) == ReceiptKind::Task)
}

/// A commit receipt: a short or full hex SHA (task and todo receipts read
/// "task #N completed" / "todo completed").
pub(crate) fn is_commit_receipt(receipt: &str) -> bool {
    (7..=40).contains(&receipt.len()) && receipt.chars().all(|c| c.is_ascii_hexdigit())
}

/// What a receipt proves, strongest first. The recap shows the strongest
/// kind the session has and nothing from the weaker ones.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReceiptKind {
    /// A commit or merge, proved by its SHA.
    Commit,
    /// A task or todo marked completed.
    Task,
}

pub(crate) fn receipt_kind(receipt: &str) -> ReceiptKind {
    let receipt = receipt.trim();
    if is_commit_receipt(receipt) {
        return ReceiptKind::Commit;
    }
    ReceiptKind::Task
}

/// The receipts that show a session finished something, oldest first, each
/// with its index in `completions`: the latest two commit receipts, or the
/// latest two task or todo receipts when the session made no commit. Entries
/// are rendered whole as `<claim> (<receipt>)`; a claim over 100 chars is dropped, never
/// cut.
pub(crate) fn displayed_receipts(
    completions: &[crate::hooks::stop::CompletionReceipt],
) -> Vec<(usize, String)> {
    const MAX_ENTRIES: usize = 2;
    const MAX_CLAIM_CHARS: usize = 100;
    use crate::extraction::provenance::extractable;
    let render = |done: &crate::hooks::stop::CompletionReceipt| {
        let receipt = normalize_feed_text(&done.receipt);
        // The detector reads the claim as stored, before extraction trims it:
        // an indented claim is code the detector leaves alone.
        if crate::import::contamination::has_csr_text(&done.claim) {
            return None;
        }
        let claim = extractable(&done.claim).map(|claim| sanitize_preview(&claim))?;
        (!receipt.is_empty() && has_substance(&claim) && claim.chars().count() <= MAX_CLAIM_CHARS)
            .then(|| format!("{} ({receipt})", claim.trim_end_matches(TRAILING_JUNK)))
    };
    let pick = |kind: ReceiptKind| -> Vec<(usize, String)> {
        completions
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, done)| receipt_kind(&done.receipt) == kind)
            .filter_map(|(index, done)| render(done).map(|entry| (index, entry)))
            .take(MAX_ENTRIES)
            .collect()
    };
    let mut shown = pick(ReceiptKind::Commit);
    if shown.is_empty() {
        shown = pick(ReceiptKind::Task);
    }
    shown.reverse();
    shown
}

/// Render a typed prompt as a recap intent: its leading whole sentences, up
/// to [`INTENT_MAX_CHARS`]. Returns `None` (abstain) when the prompt has no
/// clean rendering: the first sentence alone is too long, or the opening
/// paragraph only introduces material that follows it (`Here is a note:`
/// before a quote or list). Soft line wraps inside a paragraph are joined;
/// a blank line or a list item ends the paragraph.
pub fn clean_intent(text: &str) -> Option<String> {
    let clean = first_paragraph(text)?;
    let mut taken = 0;
    for (_, end) in split_preview_sentences(&clean) {
        if clean[..end].chars().count() > INTENT_MAX_CHARS {
            break;
        }
        taken = end;
    }
    (taken > 0).then(|| clean[..taken].to_string())
}

/// The prompt's opening paragraph, sanitized, or `None` when it only
/// introduces material that follows (ends with ':') or has no substance.
fn first_paragraph(text: &str) -> Option<String> {
    let mut paragraph: Vec<&str> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if paragraph.is_empty() {
                continue;
            }
            break;
        }
        let starts_block = MD_BULLET_RE.is_match(line)
            || trimmed.starts_with('#')
            || trimmed.starts_with('>')
            || trimmed.starts_with('|');
        if starts_block && !paragraph.is_empty() {
            break;
        }
        paragraph.push(trimmed);
    }
    let clean = sanitize_preview(&paragraph.join(" "));
    (!clean.ends_with(':') && has_substance(&clean)).then_some(clean)
}

/// Clause boundaries an over-long first sentence may be cut at.
const CLAUSE_BOUNDARIES: [&str; 5] = [", ", " - ", " \u{2013} ", "; ", ": "];

/// Fewest words a clause cut may keep.
const MIN_CLAUSE_WORDS: usize = 4;

/// A file path or URL token: the cut never keeps one, so a clause never runs
/// into `/Users/...` or ends on a link.
fn is_path_or_url(token: &str) -> bool {
    let token = token.trim_matches(|c: char| matches!(c, '(' | ')' | '"' | '\'' | ',' | ';'));
    token.starts_with('/')
        || token.starts_with("~/")
        || token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with("www.")
        || token.contains("://")
        || is_relative_path(token)
}

/// A repo-relative path such as `src/parser.rs` or `docs/plans/a/b`: a '/'
/// plus a file extension on the last component, or two or more '/'. A lone
/// slash in prose (`and/or`, `w/o`) is not a path.
fn is_relative_path(token: &str) -> bool {
    match token.matches('/').count() {
        0 => false,
        1 => token.split_once('/').is_some_and(|(dir, file)| {
            !dir.is_empty()
                && file.rsplit_once('.').is_some_and(|(stem, ext)| {
                    !stem.is_empty()
                        && (1..=8).contains(&ext.len())
                        && ext.chars().all(|c| c.is_ascii_alphanumeric())
                })
        }),
        _ => true,
    }
}

/// The anchor's intent: `clean_intent`, or, when no whole sentence fits in
/// [`INTENT_MAX_CHARS`], the opening clause cut at the LAST clause boundary at
/// or before that limit. The clause keeps at least [`MIN_CLAUSE_WORDS`] words
/// and no file path or URL (so the cut lands before the first one), loses its
/// trailing punctuation, and gets no ellipsis. `None` when no boundary
/// qualifies.
pub fn anchor_intent(text: &str) -> Option<String> {
    if let Some(intent) = clean_intent(text) {
        return Some(intent);
    }
    let clean = first_paragraph(text)?;
    let mut best: Option<String> = None;
    for (at, _) in clean.char_indices() {
        if clean[..at].chars().count() > INTENT_MAX_CHARS {
            break;
        }
        if !CLAUSE_BOUNDARIES.iter().any(|b| clean[at..].starts_with(b)) {
            continue;
        }
        let clause = clean[..at].trim_end_matches(|c: char| {
            c.is_whitespace() || c.is_ascii_punctuation() || c == '\u{2013}' || c == '\u{2014}'
        });
        let words: Vec<&str> = clause.split_whitespace().collect();
        if words.iter().any(|w| is_path_or_url(w)) {
            // Every later clause contains this path too.
            break;
        }
        if words.len() >= MIN_CLAUSE_WORDS && has_substance(clause) {
            best = Some(clause.to_string());
        }
    }
    best
}

/// Words that only acknowledge or green-light the previous turn.
const ACK_WORDS: [&str; 47] = [
    "ok", "okay", "k", "kk", "yes", "yeah", "yep", "yup", "y", "sure", "go", "ahead", "on",
    "continue", "proceed", "please", "pls", "thanks", "thank", "you", "ty", "do", "it", "that",
    "this", "sounds", "good", "great", "perfect", "nice", "cool", "lgtm", "approved", "approve",
    "fine", "carry", "keep", "going", "right", "agreed", "agree", "alright", "all", "and", "then",
    "now", "so",
];

/// True when a prompt only acknowledges the previous turn ("go", "ok do
/// it", "yes please, carry on"): it continues that task, it is not a new one.
pub fn is_acknowledgement(text: &str) -> bool {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect();
    !words.is_empty() && words.len() <= 6 && words.iter().all(|w| ACK_WORDS.contains(&w.as_str()))
}

/// Words whose trailing `.` abbreviates rather than ends a sentence.
const ABBREVIATIONS: [&str; 9] = ["e.g", "i.e", "etc", "vs", "cf", "approx", "mr", "ms", "dr"];

/// Whether `before` (text up to a single `.`) ends in an abbreviation
/// or a lone letter (an initial, `Plan A.`), so the `.` is not a full stop.
fn ends_abbreviation(before: &str) -> bool {
    let word = before
        .rsplit(char::is_whitespace)
        .next()
        .unwrap_or("")
        .trim_start_matches(|c: char| !c.is_alphanumeric());
    let mut letters = word.chars();
    let single_letter =
        matches!((letters.next(), letters.next()), (Some(c), None) if c.is_alphabetic());
    single_letter || ABBREVIATIONS.contains(&word.to_lowercase().as_str())
}

/// The first sentence of `text` (abbreviation-aware), or `None` when it
/// has no substance.
pub(crate) fn first_sentence(text: &str) -> Option<String> {
    let clean = sanitize_preview(text);
    let (start, end) = split_preview_sentences(&clean).into_iter().next()?;
    let sentence = clean[start..end].trim();
    has_substance(sentence).then(|| sentence.to_string())
}

fn split_preview_sentences(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut sentences = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        if matches!(bytes[index], b'.' | b'!' | b'?') {
            let punctuation = bytes[index];
            let mut end = index + 1;
            while end < bytes.len() && bytes[end] == punctuation {
                end += 1;
            }
            if bytes.get(end) == Some(&b'*') && bytes.get(end + 1) == Some(&b'*') {
                end += 2;
            }
            let abbreviation =
                punctuation == b'.' && end == index + 1 && ends_abbreviation(&text[start..index]);
            if !abbreviation && (end >= bytes.len() || bytes[end] == b' ') {
                let slice = &text[start..end];
                let trim_start = slice.len() - slice.trim_start().len();
                let trim_end = slice.trim_end().len();
                if trim_end > trim_start {
                    sentences.push((start + trim_start, start + trim_end));
                }
                while end < bytes.len() && bytes[end] == b' ' {
                    end += 1;
                }
                start = end;
                index = end;
                continue;
            }
        }
        index += 1;
    }
    if start < text.len() {
        let slice = &text[start..];
        let trim_start = slice.len() - slice.trim_start().len();
        let trim_end = slice.trim_end().len();
        if trim_end > trim_start {
            sentences.push((start + trim_start, start + trim_end));
        }
    }
    sentences
}

fn truncate_at_word_boundary(text: &str, max_chars: usize) -> String {
    const ELLIPSIS: &str = "...";
    let budget = max_chars.saturating_sub(ELLIPSIS.chars().count());
    let byte_limit = text
        .char_indices()
        .nth(budget)
        .map_or(text.len(), |(index, _)| index);
    let window = &text[..byte_limit];
    let cut = window.rfind(char::is_whitespace).unwrap_or(byte_limit);
    // Trailing punctuation before an ellipsis reads as a typo once the
    // clause terminator is appended (`witness ledger,....`), so drop it.
    let kept = window[..cut].trim_end().trim_end_matches(TRAILING_JUNK);
    format!("{kept}{ELLIPSIS}")
}

/// Punctuation that must not survive immediately before an appended
/// ellipsis or clause terminator.
const TRAILING_JUNK: [char; 7] = [',', ';', ':', '-', '—', '–', '.'];

/// Terminate a recap clause with a single `.`, unless it already ends in
/// terminal punctuation or an ellipsis. Without this, a truncated clause
/// renders as `… witness ledger....`.
fn terminate_clause(clause: &str) -> String {
    let trimmed = clause.trim_end();
    if trimmed.ends_with("...")
        || trimmed.ends_with('…')
        || trimmed.ends_with('.')
        || trimmed.ends_with('!')
        || trimmed.ends_with('?')
    {
        trimmed.to_string()
    } else {
        format!("{trimmed}.")
    }
}

fn sanitize_preview(text: &str) -> String {
    let no_literal_newline = text.replace("\\n", " ");
    let no_xml = XML_TAG_RE.replace_all(&no_literal_newline, "");
    let no_headings = MD_HEADING_RE.replace_all(&no_xml, "");
    let no_markdown = MD_BULLET_RE.replace_all(&no_headings, "");
    let mut result = no_markdown
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    result.retain(|character| !character.is_control() || character == ' ');
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::provenance::{extractable, RECAP_SENTINEL};
    use crate::hooks::stop::{CompletionReceipt, Episode, TodoItem};

    fn episode() -> Episode {
        Episode {
            schema: "session_episode/v2".into(),
            session_id: "session-1".into(),
            project: "csr".into(),
            timestamp: "2026-08-07T10:00:00Z".into(),
            request: "Fix\nthe recap composer".into(),
            investigated: vec![],
            completed: "Implemented deterministic recap output".into(),
            next_steps: Some("Run the integration suite".into()),
            blockers: Some("waiting for storage migration".into()),
            outcome: "partial".into(),
            error_signatures: vec![],
            tools_used: vec![],
            files_modified: vec![],
            message_count: 4,
            duration_minutes: 12,
            todos: vec![
                TodoItem {
                    content: "wire session start".into(),
                    status: "pending".into(),
                },
                TodoItem {
                    content: "write composer".into(),
                    status: "completed".into(),
                },
            ],
            approved_plan: None,
            prev_episode_id: None,
            error_count: None,
            top_errors: vec![],
            steer_count: None,
            steers: vec![],
            instrumentation_version: None,
            correction_count: None,
            recap_evidence_version: Some(crate::hooks::stop::RECAP_EVIDENCE_VERSION),
            anchor_request: Some("Fix the recap composer".into()),
            completions: vec![CompletionReceipt {
                claim: "Implemented deterministic recap output".into(),
                receipt: "abc1234".into(),
            }],
            completions_untied: false,
            anchor_receipts: None,
            compact_anchor: None,
            late_errors: false,
            closing_success: false,
            closing_ended: false,
            final_failed: false,
            closing_receipt: false,
            outcome_version: None,
            latest_ask: None,
            watermark: None,
            recap_extras: Default::default(),
            anchors: vec![],
        }
    }

    // ---- session evidence clauses (hooks::recap_evidence) ----

    use crate::hooks::recap_evidence::{BackgroundTask, GateFailure};

    fn bare_feeds() -> RecapFeeds {
        RecapFeeds {
            settled: vec![],
            still_open: vec![],
            retired_while_away: vec![],
            corrections: vec![],
            open_proposals: 0,
            native_warn: None,
            top_dream: None,
        }
    }

    /// An episode with an intent and nothing else: no completions, todos,
    /// blockers or extras. Every clause below must come from what a test adds.
    fn bare_episode() -> Episode {
        let mut ep = episode();
        ep.completions.clear();
        ep.todos.clear();
        ep.next_steps = None;
        ep.blockers = None;
        ep.anchor_receipts = Some(vec![]);
        ep.completions_untied = false;
        ep
    }

    fn receipt(claim: &str, sha: &str) -> CompletionReceipt {
        CompletionReceipt {
            claim: claim.into(),
            receipt: sha.into(),
        }
    }

    fn lines_of(text: &str) -> Vec<&str> {
        text.lines().collect()
    }

    #[test]
    fn no_evidence_means_no_settled_now_or_next_clause() {
        let text = compose_recap(&bare_episode(), &bare_feeds(), "1h ago").unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        for clause in ["Settled:", "Now:", "Next:"] {
            assert!(!text.contains(clause), "{clause}: {text}");
        }
    }

    #[test]
    fn settled_names_session_commits_the_head_clause_does_not_carry() {
        let mut ep = bare_episode();
        ep.completions = vec![
            receipt("fix(auth): keep form errors", "abc1234"),
            receipt("merged PR #78", "def5678"),
        ];
        // Neither commit was made under the anchor's own task.
        ep.completions_untied = true;
        let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
        let settled = lines_of(&text)
            .into_iter()
            .find(|line| line.starts_with("Settled: "))
            .unwrap_or_else(|| panic!("no Settled clause: {text}"));
        assert_eq!(
            settled,
            "Settled: fix(auth): keep form errors (abc1234); merged PR #78 (def5678)."
        );
    }

    #[test]
    fn settled_skips_a_commit_the_head_clause_already_names() {
        let mut ep = bare_episode();
        ep.completions = vec![
            receipt("fix(auth): keep form errors", "abc1234"),
            receipt("merged PR #78", "def5678"),
        ];
        ep.anchor_receipts = Some(vec![0]);
        let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
        assert!(
            text.contains("Latest: fix(auth): keep form errors (abc1234)"),
            "{text}"
        );
        assert!(text.contains("Settled: merged PR #78 (def5678)."), "{text}");
        assert_eq!(text.matches("abc1234").count(), 1, "{text}");
        // When the head clause names every receipt, there is nothing to settle.
        ep.anchor_receipts = Some(vec![0, 1]);
        let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
        assert!(!text.contains("Settled:"), "{text}");
    }

    #[test]
    fn a_receiptless_session_commit_is_never_settled() {
        let mut ep = bare_episode();
        ep.completions = vec![receipt("fix(auth): keep form errors", "")];
        let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
        assert!(!text.contains("Settled:"), "{text}");
    }

    #[test]
    fn now_names_a_failed_gate_with_its_command_and_exit_code() {
        let mut ep = bare_episode();
        ep.recap_extras.gate_failure = Some(GateFailure {
            command: "cargo test --release --lib".into(),
            exit_code: 101,
        });
        let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
        assert!(
            text.contains(
                "Now: last gate `cargo test --release --lib` failed (exit 101), not rerun green."
            ),
            "{text}"
        );
    }

    #[test]
    fn now_names_background_tasks_never_reported_finished() {
        let mut ep = bare_episode();
        ep.recap_extras.background_open = vec![BackgroundTask {
            id: "bq2".into(),
            label: "upload build".into(),
        }];
        ep.recap_extras.background_open_total = 1;
        let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
        assert!(
            text.contains("Now: background task not reported finished: upload build (bq2)."),
            "{text}"
        );
        ep.recap_extras.background_open_total = 4;
        let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
        assert!(
            text.contains("4 background tasks not reported finished, newest: upload build (bq2)"),
            "{text}"
        );
    }

    #[test]
    fn stored_evidence_without_a_receipt_or_with_csr_text_is_dropped() {
        let mut ep = bare_episode();
        ep.recap_extras.background_open = vec![BackgroundTask {
            id: String::new(),
            label: "upload build".into(),
        }];
        ep.recap_extras.gate_failure = Some(GateFailure {
            command: "recap [2h ago]: x [[CSR:RECAP]]".into(),
            exit_code: 1,
        });
        let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
        for clause in ["Settled:", "Now:", "Next:"] {
            assert!(!text.contains(clause), "{clause}: {text}");
        }
    }

    #[test]
    fn a_recap_with_every_evidence_clause_stays_in_budget_and_trips_the_guard() {
        let mut ep = bare_episode();
        ep.completions = vec![
            receipt(
                "fix(auth): keep form errors until the next submitted request",
                "abc1234",
            ),
            receipt("merged PR #78", "def5678"),
        ];
        ep.completions_untied = true;
        ep.recap_extras.gate_failure = Some(GateFailure {
            command: "cargo clippy --release --lib --tests".into(),
            exit_code: 101,
        });
        ep.recap_extras.background_open = vec![BackgroundTask {
            id: "bq2".into(),
            label: "upload build".into(),
        }];
        ep.recap_extras.background_open_total = 1;
        ep.next_steps = Some("listen to the first 3 seconds on a phone.".into());
        let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
        for clause in ["Settled:", "Now:", "Next:"] {
            assert!(text.contains(clause), "{clause}: {text}");
        }
        assert!(
            text.chars().count() <= MAX_CHARS,
            "{}: {text}",
            text.chars().count()
        );
        assert!(text.starts_with("recap [1h ago]: "), "{text}");
        // The self-contamination guard: the new grammar is still CSR's own.
        assert!(crate::extraction::provenance::is_csr_emission(&text));
        assert!(crate::extraction::provenance::contains_machine_sentinel(
            &text
        ));
        assert!(extractable(&text).is_none());
        let no_sentinel = text.replace(RECAP_SENTINEL, "");
        assert!(crate::extraction::provenance::is_csr_emission(&no_sentinel));
    }

    #[test]
    fn clear_handoff_is_two_short_lines_with_a_pointer_and_the_guard_grammar() {
        let ep = episode();
        let handoff = compose_clear_handoff(&ep, "3m ago").expect("a usable intent");
        let lines: Vec<&str> = handoff.lines().collect();
        assert_eq!(lines.len(), 2, "{handoff}");
        assert_eq!(
            lines[0],
            "recap [cleared, 3m ago]: Fix the recap composer. Still open: wire session start."
        );
        assert_eq!(
            lines[1],
            format!("More: csr_reflect_on_past(\"conv_session-1\") {RECAP_SENTINEL}")
        );
        assert!(handoff.chars().count() <= 400, "{handoff}");
        // No dump of the old session: none of its completions, blockers or next steps.
        for leaked in [
            "Implemented",
            "abc1234",
            "storage migration",
            "integration suite",
        ] {
            assert!(!handoff.contains(leaked), "{leaked}: {handoff}");
        }
        // Re-import drops it, by sentinel and by the `recap [` grammar alone.
        assert!(crate::extraction::provenance::is_csr_emission(&handoff));
        let no_sentinel = handoff.replace(RECAP_SENTINEL, "");
        assert!(crate::extraction::provenance::is_csr_emission(&no_sentinel));
    }

    #[test]
    fn is_request_accepts_clear_asks_and_fails_closed_on_everything_else() {
        for ask in [
            "Why did the second worker finish so early?",
            "ok so what should happen with the scratch files?",
            "Can you check the usage numbers",
            "could we ship the build tonight",
            "Change the subagent setting please",
            "see if anything happened since the last push",
            "Share the final build with me on mobile",
            "Share it on mobile",
            "Tell me which crate pulls in the old parser",
            "Please fix the build before the release",
            "I want you to rewrite the intro section",
            "let's drop the second experiment",
            "Check /tmp/csr-out for the new log",
        ] {
            assert!(is_request(ask), "should be an ask: {ask}");
        }
        for remark in [
            // A question word inside a statement.
            "I already know how many workers finished today",
            "I already know how many workers finished today?",
            "That is why the build was slow.",
            // A command word opening a noun phrase.
            "Work on the pane is finally finished",
            "Build is green again.",
            "Fix for the login bug landed",
            "Push to main failed",
            "Update on the release: it shipped",
            "Build for the release is green",
            "Run on the studio finished in two minutes",
            "Set up of the pane is done",
            "Run this morning took two minutes",
            "Share on mobile",
            "Update available for the CLI",
            "Push failed with a lock error",
            "Test results look fine to me",
            // Reactions and acknowledgements.
            "wow that worker kill took forever",
            "ok go",
            "keeping them open for now",
            "Thanks, that works.",
            // A question word that opens, but no question.
            "How the build got slow is a long story.",
            "Why did it fail. I already know the reason.",
            "",
        ] {
            assert!(!is_request(remark), "should not be an ask: {remark}");
        }
    }

    #[test]
    fn then_clause_is_a_pure_addition_before_the_sentinel() {
        let mut ep = episode();
        let feeds = full_feeds();
        let old = compose_recap(&ep, &feeds, "3m ago").expect("recap");
        assert!(!old.contains("Then:"));
        ep.latest_ask = Some("Why did the second worker finish so early?".into());
        let new = compose_recap(&ep, &feeds, "3m ago").expect("recap");
        let at = old.find(&format!(" {RECAP_SENTINEL}")).expect("sentinel");
        let expected = format!(
            "{} Then: Why did the second worker finish so early?{}",
            &old[..at],
            &old[at..]
        );
        assert_eq!(new, expected);
        assert!(crate::extraction::provenance::is_csr_emission(&new));
        let no_sentinel = new.replace(RECAP_SENTINEL, "");
        assert!(crate::extraction::provenance::is_csr_emission(&no_sentinel));
    }

    #[test]
    fn then_clause_is_dropped_when_it_would_displace_any_other_line() {
        let mut ep = episode();
        ep.anchor_request = Some("r".repeat(INTENT_MAX_CHARS));
        let feeds = full_feeds();
        let old = compose_recap(&ep, &feeds, "3m ago").expect("recap");
        ep.latest_ask = Some("Why did the second worker finish so early, given that the queue was idle all morning and nothing else was running?".into());
        let new = compose_recap(&ep, &feeds, "3m ago").expect("recap");
        // Either the recap is unchanged, or it is the old one plus the clause.
        if new != old {
            assert!(new.contains(" Then: "), "{new}");
            assert_eq!(
                new.split('\n').skip(1).collect::<Vec<_>>(),
                old.split('\n').skip(1).collect::<Vec<_>>()
            );
        }
        assert!(new.chars().count() <= 700, "{new}");
    }

    #[test]
    fn then_clause_never_pushes_the_paragraph_past_the_budget() {
        // `n` characters of plain words.
        let words = |n: usize| {
            let mut text = String::new();
            for word in ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]
                .iter()
                .cycle()
            {
                if text.len() + word.len() + 1 > n {
                    break;
                }
                text.push_str(word);
                text.push(' ');
            }
            text.trim_end().to_string()
        };
        let mut ep = episode();
        ep.anchor_request = Some(format!("Rework {}.", words(148)));
        ep.completions = vec![
            CompletionReceipt {
                claim: format!("Rewrote {}", words(88)),
                receipt: "a".repeat(40),
            },
            CompletionReceipt {
                claim: format!("Added {}", words(90)),
                receipt: "b".repeat(40),
            },
        ];
        ep.todos.clear();
        ep.blockers = None;
        ep.next_steps = Some(format!("Run {}", words(120)));
        let feeds = RecapFeeds::empty();
        let base = compose_recap(&ep, &feeds, "2 weeks ago").expect("recap");
        let ask = format!("Why did {}?", words(147));
        // The preconditions that make this test mean something.
        assert!(base.chars().count() <= 700, "{}", base.chars().count());
        assert!(
            base.chars().count() + ask.chars().count() > 700,
            "{}: {base}",
            base.chars().count()
        );
        ep.latest_ask = Some(ask);
        let with = compose_recap(&ep, &feeds, "2 weeks ago").expect("recap");
        assert!(
            with.chars().count() <= 700,
            "{}: {with}",
            with.chars().count()
        );
        assert_eq!(
            with, base,
            "an over-budget clause is dropped, not squeezed in"
        );
    }

    #[test]
    fn then_clause_needs_a_distinct_ask_and_never_walks_back() {
        let feeds = full_feeds();
        let mut ep = episode();
        // The same words as the anchor, or part of it: nothing new to say.
        for same in [
            "Fix the recap composer",
            "fix the recap composer?",
            "Fix the recap composer please",
        ] {
            ep.latest_ask = Some(same.into());
            let out = compose_recap(&ep, &feeds, "3m ago").expect("recap");
            assert!(!out.contains("Then:"), "{same}: {out}");
        }
        // An acknowledgement, a remark and no ask at all: the recap is unchanged.
        let old = compose_recap(&episode(), &feeds, "3m ago").expect("recap");
        for quiet in [
            Some("ok go"),
            Some("Work on the pane is finally finished"),
            None,
        ] {
            ep.latest_ask = quiet.map(str::to_string);
            assert_eq!(
                compose_recap(&ep, &feeds, "3m ago").as_deref(),
                Some(old.as_str())
            );
        }
        // CSR's own text pasted back is not an ask.
        ep.latest_ask = Some(format!("Why did this repeat? {RECAP_SENTINEL}"));
        let out = compose_recap(&ep, &feeds, "3m ago").expect("recap");
        assert!(!out.contains("Then:"), "{out}");
    }

    #[test]
    fn the_clear_handoff_names_the_last_ask_between_intent_and_still_open() {
        let mut ep = episode();
        ep.latest_ask = Some("Why did the second worker finish so early?".into());
        let handoff = compose_clear_handoff(&ep, "3m ago").expect("handoff");
        assert_eq!(
            handoff.lines().next(),
            Some("recap [cleared, 3m ago]: Fix the recap composer. Then: Why did the second worker finish so early? Still open: wire session start.")
        );
        assert!(crate::extraction::provenance::is_csr_emission(&handoff));
        ep.latest_ask = Some("ok go".into());
        assert_eq!(
            compose_clear_handoff(&ep, "3m ago"),
            compose_clear_handoff(&episode(), "3m ago")
        );
    }

    #[test]
    fn clear_handoff_falls_back_to_next_steps_cuts_long_text_and_abstains_without_intent() {
        let mut ep = episode();
        ep.todos.clear();
        let handoff = compose_clear_handoff(&ep, "1h ago").unwrap();
        assert!(
            handoff.contains("Still open: Run the integration suite."),
            "{handoff}"
        );
        ep.next_steps = Some("word ".repeat(80));
        let handoff = compose_clear_handoff(&ep, "1h ago").unwrap();
        assert!(handoff.chars().count() <= 400, "{handoff}");
        assert!(handoff.lines().next().unwrap().ends_with('…') || handoff.contains("…."));
        ep.next_steps = None;
        ep.blockers = None;
        let handoff = compose_clear_handoff(&ep, "1h ago").unwrap();
        assert!(!handoff.contains("Still open"), "{handoff}");
        ep.anchor_request = None;
        assert!(compose_clear_handoff(&ep, "1h ago").is_none());
    }

    #[test]
    fn recap_claims_that_are_csr_text_are_skipped() {
        let rule = "A standing rule from 10-12: “Keep receipts”";
        let mut feeds = full_feeds();
        feeds.settled.insert(
            0,
            SettledFact {
                claim: rule.into(),
                receipt: "abc1234".into(),
                status: "resolved".into(),
                source: "user_confirmed".into(),
            },
        );
        feeds.still_open.insert(
            0,
            SettledFact {
                claim: rule.into(),
                receipt: "abc1234".into(),
                status: "still_open".into(),
                source: "user_confirmed".into(),
            },
        );
        let recap = compose_recap(&episode(), &feeds, "2h ago").unwrap();
        assert!(!recap.contains("Keep receipts"), "{recap}");
        // the other claims still stand
        assert!(recap.contains("schema is stable"), "{recap}");
    }

    fn full_feeds() -> RecapFeeds {
        RecapFeeds {
            settled: vec![
                SettledFact {
                    claim: "schema is stable".into(),
                    receipt: "810283b".into(),
                    status: "resolved".into(),
                    source: "user_confirmed".into(),
                },
                SettledFact {
                    claim: "queries are indexed".into(),
                    receipt: "T2 2026-07-27".into(),
                    status: "resolved".into(),
                    source: "user_confirmed".into(),
                },
            ],
            still_open: vec![SettledFact {
                claim: "Windows path behavior".into(),
                receipt: "91abcde".into(),
                status: "still_open".into(),
                source: "user_confirmed".into(),
            }],
            retired_while_away: vec![RetiredLine {
                label: "old schema assumption".into(),
                receipt_oid: "77fedca".into(),
                date: "2026-08-06".into(),
            }],
            corrections: vec![],
            open_proposals: 2,
            native_warn: None,
            top_dream: None,
        }
    }

    #[test]
    fn composes_full_recap_in_causal_clause_order() {
        let got = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
        assert_eq!(
            got,
            "recap [28m ago]: Fix the recap composer. Latest: Implemented deterministic recap output (abc1234). [[CSR:RECAP]]\n\
             Settled: schema is stable (810283b); queries are indexed (T2 2026-07-27).\n\
             Now: waiting for storage migration | Windows path behavior (91abcde) | 2 proposals awaiting csr_resolve | 1 todos open.\n\
             Learnt-then-retired while away: old schema assumption (superseded 2026-08-06, 77fedca).\n\
             Next: Run the integration suite."
        );
    }

    #[test]
    fn abstains_when_request_and_completed_are_provenance_filtered() {
        let mut ep = episode();
        ep.anchor_request = Some("CSR CONTINUUM [2m ago]: prior state".into());
        ep.completions = vec![CompletionReceipt {
            claim: "## Session Intelligence (CSR v9.2)\nLAST: x\nNEXT: y".into(),
            receipt: "abc1234".into(),
        }];
        assert_eq!(compose_recap(&ep, &full_feeds(), "2m ago"), None);
    }

    #[test]
    fn omits_next_without_next_steps_or_open_todos() {
        let mut ep = episode();
        ep.next_steps = None;
        ep.todos.clear();
        let got = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
        assert!(!got.contains("\nNext:"));
    }

    #[test]
    fn rejects_none_next_sentinels_and_uses_later_substantive_todo() {
        let mut ep = episode();
        ep.next_steps = Some("Next: none".into());
        ep.todos = vec![
            TodoItem {
                content: "none".into(),
                status: "pending".into(),
            },
            TodoItem {
                content: "wire session start".into(),
                status: "pending".into(),
            },
        ];

        let got = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
        assert!(got.ends_with("Next: wire session start."));
        assert!(!got.contains("Next: none"));
    }

    #[test]
    fn rejects_punctuation_only_next_values_and_uses_later_substantive_todo() {
        let mut ep = episode();
        ep.next_steps = Some(".".into());
        ep.todos = vec![
            TodoItem {
                content: "...".into(),
                status: "pending".into(),
            },
            TodoItem {
                content: "wire session start".into(),
                status: "pending".into(),
            },
        ];

        let got = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
        assert!(got.ends_with("Next: wire session start."));
    }

    #[test]
    fn rejects_unicode_punctuation_only_next_values_and_uses_later_substantive_todo() {
        let mut ep = episode();
        ep.next_steps = Some("\u{2026}".into()); // "…" — non-ASCII punctuation
        ep.todos = vec![
            TodoItem {
                content: "/".into(), // preserved by the ASCII trim, still no evidence
                status: "pending".into(),
            },
            TodoItem {
                content: "wire session start".into(),
                status: "pending".into(),
            },
        ];

        let got = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
        assert!(got.ends_with("Next: wire session start."));
        assert!(!got.contains("Next: \u{2026}"));
        assert!(!got.contains("Next: /"));
    }

    #[test]
    fn abstains_when_request_and_completed_are_punctuation_only() {
        let mut ep = episode();
        ep.anchor_request = Some("\u{2026}".into());
        ep.completions = vec![CompletionReceipt {
            claim: "/".into(),
            receipt: "abc1234".into(),
        }];
        assert!(compose_recap(&ep, &RecapFeeds::empty(), "1h ago").is_none());
    }

    #[test]
    fn omits_punctuation_only_blocker_from_now_clause() {
        let mut ep = episode();
        ep.blockers = Some("\u{2026}".into());
        let got = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
        assert!(!got.contains('\u{2026}'));
    }

    #[test]
    fn skips_settled_fact_without_mandatory_receipt() {
        let feeds = RecapFeeds {
            settled: vec![
                SettledFact {
                    claim: "unreceipted".into(),
                    receipt: "".into(),
                    status: "resolved".into(),
                    source: "user_confirmed".into(),
                },
                SettledFact {
                    claim: "receipted".into(),
                    receipt: "abc1234".into(),
                    status: "resolved".into(),
                    source: "user_confirmed".into(),
                },
            ],
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(got.contains("Settled: receipted (abc1234)."));
        assert!(!got.contains("unreceipted"));
    }

    #[test]
    fn enforces_fact_status_partition_at_emission() {
        let feeds = RecapFeeds {
            settled: vec![
                SettledFact {
                    claim: "actually resolved".into(),
                    receipt: "abc1234".into(),
                    status: "resolved".into(),
                    source: "user_confirmed".into(),
                },
                SettledFact {
                    claim: "still open in settled feed".into(),
                    receipt: "def5678".into(),
                    status: "still_open".into(),
                    source: "user_confirmed".into(),
                },
            ],
            still_open: vec![
                SettledFact {
                    claim: "actually open".into(),
                    receipt: "fed4321".into(),
                    status: "regressed".into(),
                    source: "user_confirmed".into(),
                },
                SettledFact {
                    claim: "resolved in open feed".into(),
                    receipt: "cba8765".into(),
                    status: "resolved".into(),
                    source: "user_confirmed".into(),
                },
            ],
            ..RecapFeeds::empty()
        };

        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(got.contains("Settled: actually resolved (abc1234)."));
        assert!(got.contains("Now: waiting for storage migration | actually open (fed4321)"));
        assert!(!got.contains("still open in settled feed"));
        assert!(!got.contains("resolved in open feed"));
    }

    #[test]
    fn agent_resolution_never_renders_as_settled_or_user_open_state() {
        let feeds = RecapFeeds {
            settled: vec![SettledFact {
                claim: "agent claimed completion".into(),
                receipt: "agent123".into(),
                status: "resolved".into(),
                source: "agent".into(),
            }],
            still_open: vec![
                SettledFact {
                    claim: "agent claimed blocker".into(),
                    receipt: "agent456".into(),
                    status: "still_open".into(),
                    source: "agent".into(),
                },
                SettledFact {
                    claim: "confirmed blocker".into(),
                    receipt: "user789".into(),
                    status: "still_open".into(),
                    source: "user_confirmed".into(),
                },
            ],
            ..RecapFeeds::empty()
        };

        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(!got.contains("Settled:"));
        assert!(!got.contains("agent claimed completion"));
        assert!(!got.contains("agent claimed blocker"));
        assert!(got.contains("confirmed blocker"));
    }

    #[test]
    fn bounds_still_open_facts_to_shared_query_limit() {
        let feeds = RecapFeeds {
            still_open: (0..=STILL_OPEN_LIMIT)
                .map(|index| SettledFact {
                    claim: format!("open fact {index}"),
                    receipt: format!("oid{index:04}"),
                    status: "still_open".into(),
                    source: "user_confirmed".into(),
                })
                .collect(),
            ..RecapFeeds::empty()
        };

        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(got.contains(&format!("open fact {}", STILL_OPEN_LIMIT - 1)));
        assert!(!got.contains(&format!("open fact {STILL_OPEN_LIMIT}")));
    }

    #[test]
    fn formats_at_most_two_retired_lines_with_receipts() {
        let feeds = RecapFeeds {
            retired_while_away: (1..=3)
                .map(|n| RetiredLine {
                    label: format!("line {n}"),
                    receipt_oid: format!("oid000{n}"),
                    date: format!("2026-08-0{n}"),
                })
                .collect(),
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(got.contains(
            "Learnt-then-retired while away: line 1 (superseded 2026-08-01, oid0001); line 2 (superseded 2026-08-02, oid0002)."
        ));
        assert!(!got.contains("line 3"));
    }

    #[test]
    fn settled_clause_survives_when_other_optional_clauses_are_empty() {
        let mut ep = episode();
        ep.blockers = None;
        ep.next_steps = None;
        ep.todos.clear();
        let feeds = RecapFeeds {
            settled: vec![SettledFact {
                claim: "done".into(),
                receipt: "abc1234".into(),
                status: "resolved".into(),
                source: "user_confirmed".into(),
            }],
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&ep, &feeds, "now").unwrap();
        assert_eq!(got.lines().count(), 2);
        assert!(got.ends_with("Settled: done (abc1234)."));
    }

    #[test]
    fn now_clause_survives_when_other_optional_clauses_are_empty() {
        let mut ep = episode();
        ep.next_steps = None;
        ep.todos.clear();
        ep.blockers = Some("blocked on fixture".into());
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert_eq!(got.lines().count(), 2);
        assert!(got.ends_with("Now: blocked on fixture."));
    }

    #[test]
    fn caps_output_and_drops_whole_receipted_entries() {
        let mut ep = episode();
        ep.anchor_request = Some("r".repeat(INTENT_MAX_CHARS));
        ep.completions = vec![CompletionReceipt {
            claim: "c".repeat(100),
            receipt: "abc1234".into(),
        }];
        let feeds = RecapFeeds {
            settled: (0..4)
                .map(|n| SettledFact {
                    claim: format!("{}-{n}", "settled".repeat(30)),
                    receipt: format!("receipt{n}"),
                    status: "resolved".into(),
                    source: "user_confirmed".into(),
                })
                .collect(),
            still_open: (0..4)
                .map(|n| SettledFact {
                    claim: format!("{}-{n}", "open".repeat(30)),
                    receipt: format!("openoid{n}"),
                    status: "still_open".into(),
                    source: "user_confirmed".into(),
                })
                .collect(),
            retired_while_away: vec![RetiredLine {
                label: "retired".repeat(50),
                receipt_oid: "retired-receipt".into(),
                date: "2026-08-07".into(),
            }],
            corrections: vec![],
            open_proposals: 8,
            native_warn: None,
            top_dream: None,
        };
        let got = compose_recap(&ep, &feeds, "now").unwrap();
        assert!(got.chars().count() <= 700);
        assert!(got.lines().count() <= 5);
        for fragment in got.split([';', '\n']) {
            if fragment.contains("settledsettled") || fragment.contains("openopen") {
                assert!(fragment.contains(')'));
            }
        }
    }

    #[test]
    fn trusted_feed_text_is_passed_through_without_provenance_filtering() {
        let feeds = RecapFeeds {
            settled: vec![SettledFact {
                claim: "CSR CONTINUUM retained evidence".into(),
                receipt: "abc1234".into(),
                status: "resolved".into(),
                source: "user_confirmed".into(),
            }],
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(got.contains("CSR CONTINUUM retained evidence (abc1234)"));
    }

    #[test]
    fn next_falls_back_to_first_open_todo_and_filters_episode_meta() {
        let mut ep = episode();
        ep.next_steps = Some("CSR CONTINUUM [2m ago]: noise".into());
        ep.todos = vec![
            TodoItem {
                content: "first completed".into(),
                status: "completed".into(),
            },
            TodoItem {
                content: "ship\nthe recap".into(),
                status: "in_progress".into(),
            },
        ];
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert!(got.ends_with("Next: ship the recap."));
    }

    #[test]
    fn uses_the_surviving_episode_field_without_inventing_a_placeholder() {
        let mut ep = episode();
        ep.anchor_request = Some("CSR CONTINUUM [2m ago]: noise".into());
        ep.next_steps = None;
        ep.todos.clear();
        ep.blockers = None;
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert_eq!(
            got,
            "recap [now]: Implemented deterministic recap output (abc1234). [[CSR:RECAP]]"
        );
    }

    #[test]
    fn assistant_prose_is_never_rendered_as_completed() {
        let mut ep = episode();
        ep.completed = "Done. Final verdict: 52754 chunks live.".into();
        ep.completions.clear();
        ep.next_steps = None;
        ep.todos.clear();
        ep.blockers = None;
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert_eq!(got, "recap [now]: Fix the recap composer. [[CSR:RECAP]]");
    }

    #[test]
    fn provenance_filters_csr_meta_blockers_from_now() {
        let mut ep = episode();
        ep.blockers = Some("CSR CONTINUUM [2m ago]: injected state".into());
        ep.next_steps = None;
        ep.todos.clear();
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert!(!got.contains("\nNow:"));
        assert!(!got.contains("injected state"));
    }

    #[test]
    fn long_early_lists_preserve_retired_and_next_clauses() {
        let feeds = RecapFeeds {
            settled: (0..3)
                .map(|n| SettledFact {
                    claim: format!("{}-{n}", "settled evidence ".repeat(10)),
                    receipt: format!("settled{n}"),
                    status: "resolved".into(),
                    source: "user_confirmed".into(),
                })
                .collect(),
            still_open: (0..4)
                .map(|n| SettledFact {
                    claim: format!("{}-{n}", "open evidence ".repeat(10)),
                    receipt: format!("openoid{n}"),
                    status: "still_open".into(),
                    source: "user_confirmed".into(),
                })
                .collect(),
            retired_while_away: vec![RetiredLine {
                label: "obsolete parser assumption".into(),
                receipt_oid: "77fedca".into(),
                date: "2026-08-06".into(),
            }],
            corrections: vec![],
            open_proposals: 3,
            native_warn: None,
            top_dream: None,
        };
        let got = compose_recap(&episode(), &feeds, "28m ago").unwrap();
        assert!(got.contains("\nSettled:"));
        assert!(got.contains("\nNow:"));
        assert!(got.contains("\nLearnt-then-retired while away:"));
        assert!(got.contains("\nNext: Run the integration suite."));
        assert!(got.chars().count() <= 700);
        assert!(got.lines().count() <= 5);
    }

    #[test]
    fn multiline_trusted_feeds_are_structurally_flat_but_not_filtered() {
        let feeds = RecapFeeds {
            settled: vec![SettledFact {
                claim: "CSR CONTINUUM\nretained\tevidence".into(),
                receipt: "abc\n1234".into(),
                status: "resolved".into(),
                source: "user_confirmed".into(),
            }],
            retired_while_away: vec![RetiredLine {
                label: "old\nline".into(),
                receipt_oid: "77f\nedca".into(),
                date: "2026-08-\n06".into(),
            }],
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(got.contains("CSR CONTINUUM retained evidence (abc 1234)"));
        assert!(got.contains("old line (superseded 2026-08- 06, 77f edca)"));
        assert!(got.lines().count() <= 5);
        assert!(got.chars().count() <= 700);
    }

    impl RecapFeeds {
        fn empty() -> Self {
            Self {
                settled: vec![],
                still_open: vec![],
                retired_while_away: vec![],
                corrections: vec![],
                open_proposals: 0,
                native_warn: None,
                top_dream: None,
            }
        }
    }

    // ---- readability regressions from the 10.1.0 live smoke ------------
    //
    // The first real recap read `...never thought about it v10 of CSR b...:
    // Current state: Merged to main: - #281 ... witness ledger,....` — a
    // mid-word cut, flattened bullets, and a comma stacked against the
    // ellipsis and clause terminator.

    #[test]
    fn compact_preview_never_cuts_mid_word() {
        let got = compact_preview(
            "dreaming research on v10 of CSR before anything shipped",
            30,
        );
        assert!(got.ends_with("..."), "expected an ellipsis, got {got:?}");
        let body = got.trim_end_matches('.');
        assert!(
            body.split_whitespace().last().is_some_and(|word| {
                "dreaming research on v10 of CSR before anything shipped"
                    .split_whitespace()
                    .any(|whole| whole == word)
            }),
            "last word was cut mid-token: {got:?}"
        );
    }

    #[test]
    fn truncation_drops_trailing_punctuation_before_the_ellipsis() {
        let got = truncate_at_word_boundary("codewitness, witness ledger, deterministic", 26);
        assert!(!got.contains(",..."), "comma survived the cut: {got:?}");
        assert!(got.ends_with("..."), "expected an ellipsis, got {got:?}");
    }

    #[test]
    fn clause_terminator_is_never_doubled() {
        assert_eq!(terminate_clause("witness ledger..."), "witness ledger...");
        assert_eq!(terminate_clause("done already."), "done already.");
        assert_eq!(terminate_clause("shipped it"), "shipped it.");
        assert_eq!(terminate_clause("why not?"), "why not?");
    }

    #[test]
    fn markdown_bullets_do_not_leak_into_the_paragraph() {
        let cleaned =
            sanitize_preview("Merged to main:\n- #281 landed\n- #282 landed\n2. then tag");
        assert!(!cleaned.contains("- #281"), "bullet survived: {cleaned:?}");
        assert!(
            !cleaned.contains("2. then"),
            "ordered marker survived: {cleaned:?}"
        );
        assert!(cleaned.contains("#281 landed"));
    }

    #[test]
    fn live_shaped_episode_reads_as_prose() {
        let mut ep = episode();
        ep.anchor_request = Some(
            "Dreaming, we researched it chatted around it never thought about it v10 of CSR being \
             the release that makes it real"
                .into(),
        );
        ep.completed = "Current state:\n- Merged to main: #281 v10 dreaming (codewitness, witness \
                        ledger, deterministic supersession)\n- #282 recap layer"
            .into();
        ep.next_steps = None;
        ep.blockers = None;
        ep.todos = vec![];

        let got = compose_recap(&ep, &RecapFeeds::empty(), "2m ago").unwrap();
        assert!(!got.contains("...."), "stacked punctuation: {got:?}");
        assert!(!got.contains(",..."), "comma against ellipsis: {got:?}");
        assert!(!got.contains(": - "), "flattened bullet: {got:?}");
        assert!(!got.contains("Next:"), "next was fabricated: {got:?}");
    }

    // ---- the native-window warning clause ------------------------------

    const NATIVE_LINE: &str = "Claude's 300k auto-compact window beat CSR 2\u{d7} this week. \
         Remove autoCompactWindow from settings.json to let CSR lead.";

    #[test]
    fn native_warning_is_one_line_before_next_and_only_with_evidence() {
        let mut ep = bare_episode();
        ep.next_steps = Some("Ship the fix".into());
        let with = RecapFeeds {
            native_warn: Some(NATIVE_LINE.into()),
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&ep, &with, "1h ago").unwrap();
        let lines = lines_of(&got);
        assert_eq!(
            lines.iter().filter(|l| **l == NATIVE_LINE).count(),
            1,
            "{got}"
        );
        assert_eq!(lines.last().copied(), Some("Next: Ship the fix."), "{got}");
        assert_eq!(lines[lines.len() - 2], NATIVE_LINE, "{got}");
        assert!(got.chars().count() <= 700);
        // no evidence, no clause: the paragraph is what it was
        let without = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
        assert!(!without.contains("auto-compact window"), "{without}");
        assert_eq!(
            got.replace(&format!("{NATIVE_LINE}\n"), ""),
            without,
            "the clause is a pure addition"
        );
    }

    #[test]
    fn native_warning_outside_the_emitted_grammar_is_dropped() {
        for line in [
            "Claude's window beat CSR. Delete your settings.",
            "Claude's 300k auto-compact window beat CSR 2\u{d7} this week.",
            "",
            "  ",
        ] {
            let feeds = RecapFeeds {
                native_warn: Some(line.into()),
                ..RecapFeeds::empty()
            };
            let got = compose_recap(&episode(), &feeds, "now").unwrap();
            assert!(!got.contains("auto-compact window"), "{line:?} -> {got}");
            assert!(!got.contains("Delete your settings"), "{got}");
        }
    }

    #[test]
    fn native_warning_is_dropped_when_the_budget_has_no_room() {
        let mut ep = episode();
        ep.anchor_request = Some("r".repeat(INTENT_MAX_CHARS));
        ep.completions = vec![CompletionReceipt {
            claim: "c".repeat(400),
            receipt: "abc1234".into(),
        }];
        ep.next_steps = Some("n".repeat(200));
        let feeds = RecapFeeds {
            native_warn: Some(NATIVE_LINE.into()),
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&ep, &feeds, "now").unwrap();
        assert!(got.chars().count() <= 700, "{}", got.chars().count());
    }

    #[test]
    fn the_recap_carrying_the_warning_is_suppressed_from_reimport() {
        let mut ep = bare_episode();
        ep.next_steps = Some("Ship the fix".into());
        let feeds = RecapFeeds {
            native_warn: Some(NATIVE_LINE.into()),
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&ep, &feeds, "1h ago").unwrap();
        assert!(crate::extraction::provenance::is_csr_emission(&got));
        // the line alone, as the pane notice or a paste of it would be
        assert!(crate::extraction::provenance::is_csr_emission(NATIVE_LINE));
    }

    // ---- Journal v4 P5: the dream clause (delivery channel (b)) ----------

    fn dream() -> DreamClause {
        DreamClause {
            label: "parse_config".into(),
            verdict: "went stale".into(),
            receipt: "abc1234".into(),
            date: "2026-08-11".into(),
        }
    }

    #[test]
    fn dream_clause_names_the_conclusion_with_its_receipt() {
        let feeds = RecapFeeds {
            top_dream: Some(dream()),
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(
            got.contains("Dreamt: parse_config went stale (2026-08-11, abc1234)."),
            "clause missing or unreceipted: {got}"
        );
    }

    #[test]
    fn correction_clause_follows_now_with_bounded_quote_and_receipt() {
        let feeds = RecapFeeds {
            corrections: vec![CorrectionLine {
                quote: "Never bypass verification, even when the quote is deliberately made much longer than ninety characters for this regression".into(),
                session8: "abcdef12".into(),
                turn: 9,
                date: "2026-09-01".into(),
            }],
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        let now = got.find("Now: ").unwrap();
        let corrected = got.find("Corrected: ").unwrap();
        assert!(now < corrected, "{got}");
        assert!(got.contains("(abcdef12:9, 2026-09-01)."), "{got}");
        let entry = got
            .split("Corrected: ")
            .nth(1)
            .unwrap()
            .split(" (")
            .next()
            .unwrap();
        assert!(entry.chars().count() <= 90, "{entry:?}");
        assert!(got.chars().count() <= 700);
    }

    #[test]
    fn correction_cannot_evict_first_settled_entry_at_701_chars() {
        let feeds = RecapFeeds {
            settled: vec![SettledFact {
                claim: "s".repeat(383),
                receipt: "abc1234".into(),
                status: "resolved".into(),
                source: "user_confirmed".into(),
            }],
            corrections: vec![CorrectionLine {
                quote: "q".repeat(90),
                session8: "abcdef12".into(),
                turn: 9,
                date: "2026-09-01".into(),
            }],
            ..RecapFeeds::empty()
        };

        let got = compose_recap(&episode(), &feeds, "now").unwrap();

        assert!(got.chars().count() <= 700, "budget exceeded: {got}");
        assert!(got.contains("Settled: "), "first settled entry was evicted");
        assert!(!got.contains(CORRECTION_CLAUSE_PREFIX), "{got}");
        assert!(got.contains("Next: Run the integration suite."), "{got}");
    }

    #[test]
    fn structured_deliveries_exclude_a_dropped_correction_even_when_body_echoes_marker() {
        let mut ep = episode();
        ep.anchor_request = Some("Corrected: note (abcdef12:9, 2026-09-01)".into());
        let feeds = RecapFeeds {
            settled: vec![SettledFact {
                claim: "s".repeat(383),
                receipt: "abc1234".into(),
                status: "resolved".into(),
                source: "user_confirmed".into(),
            }],
            corrections: vec![CorrectionLine {
                quote: "q".repeat(90),
                session8: "abcdef12".into(),
                turn: 9,
                date: "2026-09-01".into(),
            }],
            ..RecapFeeds::empty()
        };

        let composed = compose_recap_with_deliveries(&ep, &feeds, "now").unwrap();

        assert!(composed
            .text
            .contains("Corrected: note (abcdef12:9, 2026-09-01)"));
        assert!(!composed.text.contains("\nCorrected: "));
        assert!(composed.corrections.is_empty());
    }

    /// What is recorded as delivered is what was shown: a correction that is CSR
    /// text is skipped in both.
    #[test]
    fn the_delivered_correction_is_the_one_displayed_when_another_is_csr_text() {
        let correction = |quote: &str, turn: u32| CorrectionLine {
            quote: quote.into(),
            session8: "abcdef12".into(),
            turn,
            date: "2026-09-01".into(),
        };
        let feeds = RecapFeeds {
            corrections: vec![
                correction("A standing rule from 10-12: “Keep receipts”", 1),
                correction("Use the staging endpoint", 2),
            ],
            ..RecapFeeds::empty()
        };
        let composed = compose_recap_with_deliveries(&episode(), &feeds, "now").unwrap();
        assert!(
            composed.text.contains("Use the staging endpoint"),
            "{}",
            composed.text
        );
        assert!(
            !composed.text.contains("Keep receipts"),
            "{}",
            composed.text
        );
        let recorded: Vec<u32> = composed.corrections.iter().map(|c| c.turn).collect();
        assert_eq!(recorded, [2], "the identity recorded is the one shown");
    }

    /// A completion claim that is CSR text is not rendered; a clean one beside
    /// it is.
    #[test]
    fn a_completion_claim_that_is_csr_text_is_not_injected() {
        let mut ep = episode();
        ep.completions = vec![
            CompletionReceipt {
                claim: "Implemented deterministic recap output".into(),
                receipt: "abc1234".into(),
            },
            CompletionReceipt {
                claim: "A standing rule from 10-12: “Keep receipts”".into(),
                receipt: "def5678".into(),
            },
        ];
        let recap = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert!(!recap.contains("Keep receipts"), "{recap}");
        assert!(
            recap.contains("Implemented deterministic recap output"),
            "{recap}"
        );
    }

    /// The detector reads the claim as it was stored, before extraction trims
    /// it: an indented claim is code the detector leaves alone and still renders;
    /// the same claim unindented is CSR text and does not.
    #[test]
    fn an_indented_claim_still_renders_and_the_unindented_one_does_not() {
        let rule = "A standing rule from 10-12: “Keep receipts”";
        let recap_with = |claim: String| {
            let mut ep = episode();
            ep.completions = vec![CompletionReceipt {
                claim,
                receipt: "abc1234".into(),
            }];
            compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap()
        };
        let indented = recap_with(format!("    {rule}"));
        assert!(indented.contains("Keep receipts"), "{indented}");
        let plain = recap_with(rule.to_string());
        assert!(!plain.contains("Keep receipts"), "{plain}");
    }

    #[test]
    fn dream_clause_drops_entirely_without_a_feed() {
        let got = compose_recap(&episode(), &RecapFeeds::empty(), "now").unwrap();
        assert!(
            !got.contains(DREAM_CLAUSE_PREFIX),
            "no evidence must mean no clause: {got}"
        );
    }

    #[test]
    fn dream_clause_drops_when_the_receipt_is_missing() {
        let feeds = RecapFeeds {
            top_dream: Some(DreamClause {
                receipt: "   ".into(),
                ..dream()
            }),
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(
            !got.contains(DREAM_CLAUSE_PREFIX),
            "a receiptless conclusion must never be named: {got}"
        );
    }

    #[test]
    fn dream_clause_drops_when_the_label_carries_no_substance() {
        let feeds = RecapFeeds {
            top_dream: Some(DreamClause {
                label: "\u{2026}".into(),
                ..dream()
            }),
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(!got.contains(DREAM_CLAUSE_PREFIX), "{got}");
    }

    #[test]
    fn dream_clause_obeys_the_same_character_budget_as_every_other_clause() {
        let mut ep = episode();
        ep.anchor_request = Some("r".repeat(INTENT_MAX_CHARS));
        ep.completions = vec![CompletionReceipt {
            claim: "c".repeat(100),
            receipt: "abc1234".into(),
        }];
        let feeds = RecapFeeds {
            settled: (0..4)
                .map(|n| SettledFact {
                    claim: format!("{}-{n}", "settled".repeat(30)),
                    receipt: format!("receipt{n}"),
                    status: "resolved".into(),
                    source: "user_confirmed".into(),
                })
                .collect(),
            top_dream: Some(dream()),
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&ep, &feeds, "now").unwrap();
        assert!(got.chars().count() <= 700);
    }

    // ---- T3: machine-owned recap sentinel — metamorphic wrapper tests ----
    //
    // These compose a REAL recap via the canonical producer (not a
    // hand-built string) so coverage tracks what actually ships, then apply
    // each adversarial wrapper transformation and assert the full
    // extractable() pipeline still rejects the result. Non-tautological:
    // this exercises extractable()'s prose-extraction grammar end to end,
    // not is_csr_emission alone on the composer's raw output.

    fn wrap_bullet(text: &str) -> String {
        text.lines()
            .map(|l| format!("- {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn wrap_blockquote(text: &str) -> String {
        text.lines()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn wrap_emphasis(text: &str) -> String {
        format!("**{text}**")
    }

    fn wrap_html(text: &str) -> String {
        format!("<div class=\"recap\"><p>{text}</p></div>")
    }

    fn wrap_zero_width(text: &str) -> String {
        text.chars().flat_map(|c| [c, '\u{200B}']).collect()
    }

    fn wrap_confusable_brackets(text: &str) -> String {
        text.replace('[', "\u{FF3B}").replace(']', "\u{FF3D}")
    }

    fn wrap_long_preamble(text: &str) -> String {
        format!(
            "{}\n{text}",
            "filler text that reads like real prose ".repeat(40)
        )
    }

    #[test]
    fn composed_recap_survives_bullet_wrapping() {
        let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
        assert!(extractable(&wrap_bullet(&recap)).is_none());
    }

    #[test]
    fn composed_recap_survives_blockquote_wrapping() {
        let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
        assert!(extractable(&wrap_blockquote(&recap)).is_none());
    }

    #[test]
    fn composed_recap_survives_emphasis_wrapping() {
        let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
        assert!(extractable(&wrap_emphasis(&recap)).is_none());
    }

    #[test]
    fn composed_recap_survives_html_wrapping() {
        let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
        assert!(extractable(&wrap_html(&recap)).is_none());
    }

    #[test]
    fn composed_recap_survives_zero_width_noise() {
        let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
        assert!(extractable(&wrap_zero_width(&recap)).is_none());
    }

    #[test]
    fn composed_recap_survives_confusable_bracket_substitution() {
        let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
        assert!(extractable(&wrap_confusable_brackets(&recap)).is_none());
    }

    #[test]
    fn composed_recap_survives_preamble_past_header_window() {
        let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
        let wrapped = wrap_long_preamble(&recap);
        assert!(wrapped.len() > 400, "preamble must exceed HEADER_WINDOW");
        assert!(extractable(&wrapped).is_none());
    }

    #[test]
    fn composed_recap_survives_stacked_wrapping() {
        // Bullet + blockquote + zero-width noise + a long preamble, combined
        // — the sentinel must survive all of it at once, not just one
        // transformation at a time.
        let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
        let stacked = wrap_long_preamble(&wrap_zero_width(&wrap_blockquote(&wrap_bullet(&recap))));
        assert!(extractable(&stacked).is_none());
    }

    #[test]
    fn composed_recap_always_carries_the_sentinel_within_budget() {
        let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
        assert!(recap.contains(RECAP_SENTINEL));
        assert!(recap.chars().count() <= 700);
    }

    // ---- intent: whole sentences of the latest task, or abstain ----------

    #[test]
    fn clean_intent_joins_soft_wraps_and_never_truncates() {
        // Live garble: this prompt was cut at 77 chars ("…can") and the
        // truncation ellipsis then trimmed away as trailing junk.
        let got = clean_intent(
            "anthropics codemod, when github links are bought up or disccussed, can\n  \
             present a side pane (enhanced for herdr users) - thoughts?",
        );
        assert_eq!(
            got.as_deref(),
            Some(
                "anthropics codemod, when github links are bought up or disccussed, can \
                 present a side pane (enhanced for herdr users) - thoughts?"
            )
        );
    }

    #[test]
    fn clean_intent_keeps_leading_whole_sentences_within_budget() {
        let first = "Fix the recap composer.";
        let long = format!("{} end.", "word ".repeat(40));
        let got = clean_intent(&format!("{first} {long}")).unwrap();
        assert_eq!(got, first);
    }

    #[test]
    fn clean_intent_abstains_when_the_first_sentence_is_too_long() {
        let text = format!("{} end", "word ".repeat(40));
        assert!(text.chars().count() > INTENT_MAX_CHARS);
        assert_eq!(clean_intent(&text), None);
    }

    #[test]
    fn clean_intent_abstains_on_an_introducer_and_stops_at_lists() {
        assert_eq!(
            clean_intent("Here is a note offered by a side agent:"),
            None
        );
        assert_eq!(clean_intent("Fix these:\n- recap\n- pane"), None);
        assert_eq!(
            clean_intent("Fix the recap\n- it garbles\n\nlater text").as_deref(),
            Some("Fix the recap")
        );
    }

    #[test]
    fn acknowledgements_are_not_tasks() {
        for ack in [
            "go",
            "ok, do it please",
            "Yes go ahead",
            "lgtm!",
            "carry on",
        ] {
            assert!(is_acknowledgement(ack), "{ack}");
        }
        for task in ["fix the tests", "go fix the recap", "ok now ship 10.1"] {
            assert!(!is_acknowledgement(task), "{task}");
        }
    }

    #[test]
    fn legacy_long_session_abstains_from_its_first_prompt() {
        // Episodes written before the evidence fields only carry the first
        // prompt; in a long session it is a stale topic.
        let mut ep = episode();
        ep.recap_evidence_version = None;
        ep.anchor_request = None;
        ep.completions.clear();
        ep.request = "anthropics codemod, when github links are bought up".into();
        ep.completed = String::new();
        ep.message_count = 30_393;
        ep.next_steps = None;
        ep.todos.clear();
        ep.blockers = None;
        assert_eq!(compose_recap(&ep, &RecapFeeds::empty(), "3h ago"), None);

        ep.message_count = 6;
        assert_eq!(
            compose_recap(&ep, &RecapFeeds::empty(), "3h ago").as_deref(),
            Some("recap [3h ago]: anthropics codemod, when github links are bought up. [[CSR:RECAP]]")
        );
    }

    #[test]
    fn legacy_completed_prose_is_never_rendered() {
        let mut ep = episode();
        ep.recap_evidence_version = None;
        ep.anchor_request = None;
        ep.completions.clear();
        ep.request = "Fix the recap composer".into();
        ep.completed = "I'll recreate it headless: a real claude session runs in tmux.".into();
        ep.next_steps = None;
        ep.todos.clear();
        ep.blockers = None;
        let got = compose_recap(&ep, &RecapFeeds::empty(), "2m ago").unwrap();
        assert_eq!(got, "recap [2m ago]: Fix the recap composer. [[CSR:RECAP]]");
    }

    #[test]
    fn completed_clause_prefers_commits_and_renders_entries_whole() {
        let mut ep = episode();
        ep.completions = vec![
            CompletionReceipt {
                claim: "oldest".into(),
                receipt: "0000000".into(),
            },
            CompletionReceipt {
                claim: "x".repeat(101),
                receipt: "1111111".into(),
            },
            CompletionReceipt {
                claim: "unreceipted".into(),
                receipt: " ".into(),
            },
            CompletionReceipt {
                claim: "Write regression tests".into(),
                receipt: "task #3 completed".into(),
            },
        ];
        ep.next_steps = None;
        ep.todos.clear();
        ep.blockers = None;
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        // Commits come first: the only renderable commit (the 101-char claim
        // is dropped, never cut) outranks the newer task receipt.
        assert_eq!(
            got,
            "recap [now]: Fix the recap composer. Latest: oldest (0000000). [[CSR:RECAP]]"
        );
        // With no commit at all, the latest task receipts show.
        ep.completions
            .retain(|done| !is_commit_receipt(&done.receipt));
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert_eq!(
            got,
            "recap [now]: Fix the recap composer: Write regression tests (task #3 completed). [[CSR:RECAP]]"
        );
    }

    #[test]
    fn only_receipts_made_under_the_anchor_are_shown() {
        let done = |claim: &str, receipt: &str| CompletionReceipt {
            claim: claim.into(),
            receipt: receipt.into(),
        };
        // The head line is the intent and what was made under it; what the
        // session made under other asks is `Settled:`, never beside the intent.
        let head = |text: &str| text.lines().next().unwrap().to_string();
        let mut ep = episode();
        ep.todos.clear();
        ep.next_steps = None;
        ep.blockers = None;
        ep.completions = vec![
            done("fix(a): under an earlier ask", "aaaaaaa"),
            done("fix(b): under the anchor", "bbbbbbb"),
            done("fix(c): under a later ask", "ccccccc"),
        ];
        // Only the anchor's own commit stands beside its intent.
        ep.anchor_receipts = Some(vec![1]);
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert_eq!(
            head(&got),
            "recap [now]: Fix the recap composer. Latest: fix(b): under the anchor (bbbbbbb). [[CSR:RECAP]]"
        );
        assert_eq!(
            got.lines().nth(1),
            Some("Settled: fix(c): under a later ask (ccccccc).")
        );
        // The anchor made none: the head says what was asked and nothing more.
        ep.anchor_receipts = Some(vec![]);
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert_eq!(
            head(&got),
            "recap [now]: Fix the recap composer. [[CSR:RECAP]]"
        );
        assert_eq!(
            got.lines().nth(1),
            Some(
                "Settled: fix(b): under the anchor (bbbbbbb); fix(c): under a later ask (ccccccc)."
            )
        );
        // An episode stored before the tie was recorded: its receipts are the
        // anchor's own, or they are not shown beside the intent at all.
        ep.anchor_receipts = None;
        ep.completions_untied = true;
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert_eq!(
            head(&got),
            "recap [now]: Fix the recap composer. [[CSR:RECAP]]"
        );
        ep.completions_untied = false;
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert!(got.contains("(bbbbbbb); fix(c): under a later ask (ccccccc)"));
        assert!(!got.contains("Settled:"), "{got}");
        // A task the session marked completed reads as the anchor's result.
        ep.completions = vec![done("Write regression tests", "task #3 completed")];
        ep.anchor_receipts = Some(vec![0]);
        assert_eq!(
            compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap(),
            "recap [now]: Fix the recap composer: Write regression tests (task #3 completed). [[CSR:RECAP]]"
        );
        assert_eq!(receipt_kind("todo completed"), ReceiptKind::Task);
        assert_eq!(receipt_kind("task #4 completed"), ReceiptKind::Task);
        assert_eq!(receipt_kind("deadbee"), ReceiptKind::Commit);
    }

    #[test]
    fn abbreviations_do_not_end_a_sentence() {
        let long = "the recap composer so that it never cuts a prompt in the middle of a \
                    sentence, keeps soft wraps joined, reads only receipted completions, and \
                    abstains when nothing clean is left to say.";
        let text = format!("Please update e.g. {long}");
        assert!(text.chars().count() > INTENT_MAX_CHARS);
        assert_eq!(clean_intent(&text), None);

        let sentences = |text: &str| -> Vec<String> {
            split_preview_sentences(text)
                .into_iter()
                .map(|(start, end)| text[start..end].to_string())
                .collect()
        };
        assert_eq!(
            sentences("Use a tool, e.g. grep, i.e. fast, etc. vs. sed. Done."),
            ["Use a tool, e.g. grep, i.e. fast, etc. vs. sed.", "Done."]
        );
        assert_eq!(
            sentences("Ask Dr. Who and Mr. X, cf. approx. N. Smith. Done."),
            ["Ask Dr. Who and Mr. X, cf. approx. N. Smith.", "Done."]
        );
        assert_eq!(
            sentences("Ship 10.1 and fix stop.rs today. Then more."),
            ["Ship 10.1 and fix stop.rs today.", "Then more."]
        );
    }

    #[test]
    fn legacy_untied_receipts_are_not_shown_beside_the_intent() {
        // An episode stored before the tie was recorded, whose receipts were
        // not the anchor's own: they stand beside the intent never, and are
        // only the commits the session made, under `Settled:`.
        let mut ep = episode();
        ep.anchor_request = Some("Fix the recap composer".into());
        ep.completions_untied = true;
        ep.anchor_receipts = None;
        ep.next_steps = None;
        ep.todos.clear();
        ep.blockers = None;
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert_eq!(
            got,
            "recap [now]: Fix the recap composer. [[CSR:RECAP]]\nSettled: Implemented deterministic recap output (abc1234)."
        );
        assert!(crate::extraction::provenance::is_csr_emission(&got));
        // Without an intent there is nothing left to say.
        ep.anchor_request = None;
        assert_eq!(compose_recap(&ep, &RecapFeeds::empty(), "now"), None);
    }

    // ---- round 5: clause cut for an over-long first task ----

    const REYCITAL_1: &str = "This ia new Reycital challenge - there are two videos here /Users/ramakrishnanannaswamy/Downloads/IMG_8550.mov /Users/ramakrishnanannaswamy/Downloads/IMG_8546.mov - 8550 is the melodica, which is the primary melody and 8546 is the piano - your mission if you chose to take it on is to import both into logic get the bounce out - then for the video propose your thoughts. do initia probes and tell me any questions on concept";
    const SELF_BRANDING_1: &str = "I am visualizing a atlantic style essay about how ai usage trends have changed and why even though things look different they are thes same, this is a research and extrapolation task. the eassay looks at what has truly materially changed";

    #[test]
    fn anchor_intent_cuts_an_over_long_first_sentence_at_a_clause() {
        assert_eq!(clean_intent(REYCITAL_1), None);
        assert_eq!(
            anchor_intent(REYCITAL_1).as_deref(),
            Some("This ia new Reycital challenge")
        );
        assert_eq!(clean_intent(SELF_BRANDING_1), None);
        assert_eq!(
            anchor_intent(SELF_BRANDING_1).as_deref(),
            Some(
                "I am visualizing a atlantic style essay about how ai usage trends have changed \
                 and why even though things look different they are thes same"
            )
        );
        // A whole sentence that fits still wins over any cut.
        assert_eq!(
            anchor_intent("Fix the recap composer, please. Then more.").as_deref(),
            Some("Fix the recap composer, please. Then more.")
        );
    }

    #[test]
    fn clause_cut_needs_four_words_and_never_ends_in_a_path_or_url() {
        let long_tail = "word ".repeat(40);
        // Only boundary gives three words: abstain.
        assert_eq!(anchor_intent(&format!("Fix the pane, {long_tail}")), None);
        // A clause ending in a URL or path is never the cut.
        assert_eq!(
            anchor_intent(&format!(
                "review this repo https://github.com/x/y, {long_tail}"
            )),
            None
        );
        assert_eq!(
            anchor_intent(&format!(
                "compare the two takes in ~/Movies/take.mov; {long_tail}"
            )),
            None
        );
        // The last valid boundary at or before 160 chars wins; no ellipsis.
        let got = anchor_intent(&format!(
            "Build the reel from both clips, sync the melodica first; then {long_tail}"
        ))
        .unwrap();
        assert_eq!(
            got,
            "Build the reel from both clips, sync the melodica first"
        );
    }

    #[test]
    fn a_clipped_anchor_is_still_recognised_as_a_csr_emission() {
        let mut ep = episode();
        ep.anchor_request = anchor_intent(REYCITAL_1);
        ep.completions.clear();
        ep.next_steps = None;
        ep.todos.clear();
        ep.blockers = None;
        let got = compose_recap(&ep, &RecapFeeds::empty(), "11h ago").unwrap();
        assert_eq!(
            got,
            "recap [11h ago]: This ia new Reycital challenge. [[CSR:RECAP]]"
        );
        assert!(crate::extraction::provenance::is_csr_emission(&got));
        assert!(crate::extraction::provenance::extractable(&got).is_none());
        assert_eq!(crate::extraction::provenance::strip_csr_emissions(&got), "");
    }

    #[test]
    fn astra_r5_6_relative_paths_never_survive_a_clause_cut() {
        let text = format!(
            "Please fix the parser in src/parser.rs, {}",
            "word ".repeat(40)
        );
        assert_eq!(anchor_intent(&text), None);
        // A slash in prose is not a path.
        assert!(!is_path_or_url("and/or"));
        assert!(is_path_or_url("csr-engine/src/hooks/stop.rs"));
    }

    // ---- read-only probe against a real transcript ----------------------
    //
    // `cargo test --release --lib probe_real_transcript_recap -- --ignored --nocapture`
    // with CSR_RECAP_PROBE_TRANSCRIPT=<jsonl>, optional CSR_RECAP_PROBE_UNTIL
    // (ISO timestamp; lines from the first one at or after it are dropped, so
    // the episode is rebuilt as the Stop hook saw it then), CSR_RECAP_PROBE_AGE,
    // CSR_RECAP_PROBE_PROJECT, and CSR_RECAP_PROBE_DB (opened with
    // SQLITE_OPEN_READ_ONLY, no migrations; feeds are only read).
    #[test]
    #[ignore = "reads a real transcript named by CSR_RECAP_PROBE_TRANSCRIPT"]
    fn probe_real_transcript_recap() {
        let Ok(path) = std::env::var("CSR_RECAP_PROBE_TRANSCRIPT") else {
            eprintln!("CSR_RECAP_PROBE_TRANSCRIPT unset; nothing to probe");
            return;
        };
        let until = std::env::var("CSR_RECAP_PROBE_UNTIL").ok();
        let age = std::env::var("CSR_RECAP_PROBE_AGE").unwrap_or_else(|_| "now".into());
        let project = std::env::var("CSR_RECAP_PROBE_PROJECT").unwrap_or_else(|_| "probe".into());
        let raw = std::fs::read_to_string(&path).expect("transcript readable");
        let lines: Vec<&str> = raw
            .lines()
            .take_while(|line| {
                let Some(until) = until.as_deref() else {
                    return true;
                };
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .and_then(|v| v.get("timestamp")?.as_str().map(str::to_owned))
                    .is_none_or(|ts| ts.as_str() < until)
            })
            .collect();
        let session_id = std::path::Path::new(&path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ep = crate::hooks::stop::extract_episode(&lines, &session_id, &project);
        let feeds = match std::env::var("CSR_RECAP_PROBE_DB") {
            Ok(db) => {
                let storage = crate::storage::Storage::open_read_only(std::path::Path::new(&db))
                    .expect("read-only open");
                let (settled, still_open) = storage
                    .recap_ledger_feeds(&project, &ep.session_id)
                    .unwrap_or_default();
                let since = (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339();
                RecapFeeds {
                    settled,
                    still_open,
                    retired_while_away: storage
                        .recap_retired_since(&project, &ep.timestamp)
                        .unwrap_or_default(),
                    corrections: storage
                        .recap_corrections(&project, &since, 2)
                        .unwrap_or_default(),
                    open_proposals: storage.recap_open_proposals(&project).unwrap_or(0),
                    native_warn: None,
                    top_dream: storage.recap_top_dream(&project).unwrap_or(None),
                }
            }
            Err(_) => RecapFeeds::empty(),
        };
        eprintln!("lines kept: {}", lines.len());
        eprintln!(
            "RECAP>>>\n{}\n<<<RECAP",
            compose_recap(&ep, &feeds, &age).unwrap_or_else(|| "(abstained)".into())
        );
    }

    // ---- negative corpus: genuine prose about recaps must stay extractable ----

    #[test]
    fn genuine_prose_mentioning_recaps_is_not_rejected() {
        let samples = [
            "Let's recap what we discussed: the login bug is fixed and deployed.",
            "The recap feature works great, though it's verbose in the Now: clause sometimes.",
            "I want to recap [the auth changes], then prioritize them.",
            "Can you write a quick recap of yesterday's session for the standup?",
        ];
        for sample in samples {
            assert!(
                extractable(sample).is_some(),
                "genuine prose wrongly rejected: {sample:?}"
            );
        }
    }
}

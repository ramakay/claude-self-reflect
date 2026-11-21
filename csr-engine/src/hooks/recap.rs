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
    /// The anchor file the conclusion is about, as the dreamer stored it.
    /// SessionStart shows the clause only when the previous session touched
    /// this file, or the request names it or its symbol.
    pub file: String,
    /// The anchor symbol, when the conclusion is about one.
    pub symbol: Option<String>,
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

/// Longest branch name an off-branch note quotes; longer ones end in `…`.
const BRANCH_LABEL_MAX_CHARS: usize = 32;
/// The clause naming the session's literal last typed message.
pub const THEN_CLAUSE_PREFIX: &str = "Then: ";

/// Facts about the working tree the composer is handed. The composer never
/// reads git itself (`hooks::recap_git` does, with a timeout, and fails open
/// to [`RecapContext::default`]).
#[derive(Debug, Clone, Default)]
pub struct RecapContext {
    /// Receipt commits proved NOT to be ancestors of the working tree's HEAD.
    /// A receipt absent from this set is shown unmarked, whatever the reason.
    pub off_head: std::collections::HashSet<String>,
    /// What HEAD is called in `not on <label>`: the branch name, or `HEAD`
    /// when detached.
    pub head_label: String,
    /// The episode's session is the one now running (a compaction of it): its
    /// last turn is in flight by definition, so "ended mid-turn" is not said.
    pub session_is_live: bool,
}

impl RecapContext {
    /// `<label>` for a receipt proved off HEAD, else `None`.
    fn off_head_note(&self, receipt: &str) -> Option<String> {
        let receipt = receipt.trim();
        (!receipt.is_empty() && self.off_head.contains(receipt)).then(|| {
            let label = self.head_label.trim();
            let label = if label.is_empty() { "HEAD" } else { label };
            // The head carries up to two marked receipts and is not trimmed
            // after marking, so the branch name is bounded: with it, the head
            // stays inside `MAX_CHARS` whatever the branch is called.
            if label.chars().count() > BRANCH_LABEL_MAX_CHARS {
                let cut: String = label.chars().take(BRANCH_LABEL_MAX_CHARS - 1).collect();
                format!("not on {cut}\u{2026}")
            } else {
                format!("not on {label}")
            }
        })
    }

    /// `receipt` followed by its off-branch note, when it has one.
    fn mark_receipt(&self, receipt: &str) -> String {
        match self.off_head_note(receipt) {
            Some(note) => format!("{receipt}, {note}"),
            None => receipt.to_string(),
        }
    }

    /// An entry rendered as `<claim> (<receipt>)` with the note added inside
    /// its closing parenthesis. Any other shape is returned as it is.
    fn mark_entry(&self, entry: String) -> String {
        if self.off_head.is_empty() || !entry.ends_with(')') {
            return entry;
        }
        let Some(open) = entry.rfind(" (") else {
            return entry;
        };
        let receipt = &entry[open + 2..entry.len() - 1];
        match self.off_head_note(receipt) {
            Some(note) => format!("{} ({receipt}, {note})", &entry[..open]),
            None => entry,
        }
    }
}

/// Compose an evidence-backed recap without reading external state.
///
/// This form may return a paragraph whose only content is the session's intent:
/// its callers label that themselves (the repo recap reads it as "you asked").
/// SessionStart uses [`compose_recap_with_deliveries`], which abstains instead.
pub fn compose_recap(
    ep: &crate::hooks::stop::Episode,
    feeds: &RecapFeeds,
    age: &str,
) -> Option<String> {
    compose_with_then(ep, feeds, age, false, &RecapContext::default()).map(|composed| composed.text)
}

/// The SessionStart recap. A paragraph that would carry no evidenced clause (it
/// would only echo the user's last prompt) is not composed: `None`, so the
/// fragment fallback runs.
pub fn compose_recap_with_deliveries(
    ep: &crate::hooks::stop::Episode,
    feeds: &RecapFeeds,
    age: &str,
) -> Option<ComposedRecap> {
    compose_with_then(ep, feeds, age, true, &RecapContext::default())
}

/// [`compose_recap_with_deliveries`] with the working-tree facts SessionStart read.
pub fn compose_recap_in_context(
    ep: &crate::hooks::stop::Episode,
    feeds: &RecapFeeds,
    age: &str,
    ctx: &RecapContext,
) -> Option<ComposedRecap> {
    compose_with_then(ep, feeds, age, true, ctx)
}

/// Commit receipts the recap could render for this episode and feeds: the
/// candidates for the off-branch check. Bounded: at most the receipts the
/// composer itself caps (two per list, three settled, one dream).
pub fn candidate_receipts(ep: &crate::hooks::stop::Episode, feeds: &RecapFeeds) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |receipt: &str| {
        let receipt = receipt.trim();
        if is_commit_receipt(receipt) && !out.iter().any(|seen| seen == receipt) {
            out.push(receipt.to_string());
        }
    };
    for done in anchor_completions(ep) {
        push(&done.receipt);
    }
    for (index, _) in displayed_receipts(&ep.completions) {
        if let Some(done) = ep.completions.get(index) {
            push(&done.receipt);
        }
    }
    for fact in feeds.settled.iter().take(3) {
        push(&fact.receipt);
    }
    for fact in feeds.still_open.iter().take(STILL_OPEN_LIMIT) {
        push(&fact.receipt);
    }
    if let Some(dream) = &feeds.top_dream {
        push(&dream.receipt);
    }
    out
}

fn compose_with_then(
    ep: &crate::hooks::stop::Episode,
    feeds: &RecapFeeds,
    age: &str,
    abstain_intent_only: bool,
    ctx: &RecapContext,
) -> Option<ComposedRecap> {
    let base = compose_recap_inner(ep, feeds, age, None, abstain_intent_only, ctx)?;
    // The `Then:` clause is a pure addition: it is kept only when every other
    // line of the recap comes out exactly as it would without it. It follows
    // the same length rule as the intent (`cap_words`).
    if let Some(then) = then_clause(ep) {
        if let Some(with) =
            compose_recap_inner(ep, feeds, age, Some(&then), abstain_intent_only, ctx)
        {
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
    abstain_intent_only: bool,
    ctx: &RecapContext,
) -> Option<ComposedRecap> {
    use crate::extraction::provenance::{extractable, RECAP_SENTINEL};
    use crate::import::contamination::has_csr_text;

    // SessionStart (the abstaining form) composes its head from the stored
    // intent (`composed_intent`); the repo recap labels its intent "you asked"
    // itself.
    let intent = if abstain_intent_only {
        composed_intent(ep)
    } else {
        recap_intent(ep)
    };
    let completed = recap_completed(ep, ctx);
    // SessionStart can still compose with no head words at all (no clean
    // intent and nothing landed): the evidence lines below carry it, and the
    // check after them falls back to the fragment block unless a `Settled:` or
    // a named `Now:` entry stands.
    let headless = intent.is_none() && completed.is_none();
    if headless && !abstain_intent_only {
        return None;
    }
    // What the head says, for the relevance check on project-wide evidence:
    // the intent, and the `Then:` clause only when this paragraph renders it.
    // `compose_with_then` keeps a `Then:` only when every other line is the same
    // as without it, so a verdict only `Then:` names never reaches the output.
    let request_text = format!(
        "{} {}",
        intent.as_deref().unwrap_or(""),
        then.filter(|_| !headless).unwrap_or_default()
    );

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
        // SessionStart found no intent short enough to quote: lead with what the
        // session landed. (Elsewhere a missing intent is just missing.)
        (None, Some(completed)) if abstain_intent_only => {
            format!("recap [{age}]: Latest: {}", terminate_clause(completed))
        }
        (None, Some(completed)) => format!("recap [{age}]: {}", terminate_clause(completed)),
        // Nothing to quote and nothing landed: the sentinel stands alone and
        // the evidence lines carry the paragraph. Words are never invented.
        (None, None) => format!("recap [{age}]:"),
    };
    // The session's own last finding (SessionStart only): the fragment block's
    // LAST line, kept so the paragraph never says less than the block it
    // replaces. Read before `Then:` is added, so both forms carry the same line.
    let last_line = abstain_intent_only
        .then(|| last_clause(ep, &recap))
        .flatten();
    if let (Some(then), false) = (then, headless) {
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
        .map(|fact| format_fact(fact, ctx))
        .chain(session_settled(ep, ctx, abstain_intent_only))
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
            .map(|fact| format_fact(fact, ctx)),
    );
    if feeds.open_proposals > 0 {
        now_parts.push(format!(
            "{} proposals awaiting csr_resolve",
            feeds.open_proposals
        ));
    }
    // `Next:` is the stored next step, else the first open todo.
    let next_from_steps = ep
        .next_steps
        .as_deref()
        .and_then(next_preview)
        .filter(|next| !has_csr_text(next));
    let todo_titles: Vec<String> = ep
        .todos
        .iter()
        .filter(|todo| todo.status != "completed")
        .filter_map(|todo| next_preview(&todo.content))
        .collect();
    let next = next_from_steps.or_else(|| todo_titles.first().cloned());
    // Open todos by name, with the session they came from as the receipt. A
    // count with no names is not said, and the todo `Next:` already names is
    // not said twice.
    let session8: String = ep.session_id.chars().take(8).collect();
    let next_key = next.as_deref().map(word_key);
    let named_todos: Vec<String> = todo_titles
        .iter()
        .filter(|title| next_key.as_deref() != Some(word_key(title).as_str()))
        .filter(|title| !has_csr_text(title))
        .map(|title| compact_preview(title, TODO_TITLE_MAX_CHARS))
        .collect();
    let todo_entry = (!named_todos.is_empty() && !session8.is_empty()).then(|| {
        let shown = named_todos.iter().take(2).cloned().collect::<Vec<_>>();
        let more = named_todos.len() - shown.len();
        format!(
            "{}: {}{} (conv {session8})",
            if named_todos.len() == 1 {
                "open todo"
            } else {
                "open todos"
            },
            shown.join("; "),
            if more > 0 {
                format!("; +{more} more")
            } else {
                String::new()
            }
        )
    });
    let priority_now_entries = usize::from(!now_parts.is_empty());
    now_parts.extend(todo_entry);
    // The stored episode says the session's last turn never finished (no
    // closing assistant reply: cut off or interrupted). Said only for an
    // episode whose outcome rule ran, and never for the session now running.
    let evidenced_now_entries = now_parts.len();
    if ep.outcome_version.is_some()
        && !ep.closing_ended
        && !ctx.session_is_live
        && !session8.is_empty()
    {
        now_parts.push(format!("last turn did not finish (conv {session8})"));
    }

    // SessionStart shows a project-wide verdict (a dream, a retired note) only
    // when the recap's own text names its file or symbol: the ask, `Latest:`,
    // `Last:`, `Settled:`, `Now:` or `Next:`. Checked here against every
    // candidate entry, and again below against the entries that fit.
    let candidate_text = [
        request_text.clone(),
        completed.clone().unwrap_or_default(),
        last_line.clone().unwrap_or_default(),
        next.clone().unwrap_or_default(),
        settled_entries.join(" "),
        now_parts.join(" "),
    ]
    .join("\n");
    let shown_ok =
        |subject: &WitnessSubject, text: &str| !abstain_intent_only || subject.is_named_in(text);
    let (retired_subjects, retired_entries): (Vec<WitnessSubject>, Vec<String>) = feeds
        .retired_while_away
        .iter()
        .filter(|line| !normalize_feed_text(&line.receipt_oid).is_empty())
        .filter(|line| !has_csr_text(&normalize_feed_text(&line.label)))
        .map(|line| {
            (
                WitnessSubject::from_label(&normalize_feed_text(&line.label)),
                line,
            )
        })
        .filter(|(subject, _)| shown_ok(subject, &candidate_text))
        .take(2)
        .map(|(subject, line)| {
            (
                subject,
                format!(
                    "{} (superseded {}, {})",
                    normalize_feed_text(&line.label),
                    normalize_feed_text(&line.date),
                    normalize_feed_text(&line.receipt_oid)
                ),
            )
        })
        .unzip();
    let next_line = next.map(|next| format!("Next: {next}."));
    let line_cost =
        |line: &Option<String>| line.as_ref().map_or(0, |line| line.chars().count() + 1);
    // `Last:` is reserved after `Next:`, when it fits at all.
    let last_line = last_line.filter(|line| {
        recap.chars().count() + line_cost(&next_line) + line.chars().count() < MAX_CHARS
    });
    // The native-window warning: one line of its own, reserved like `Next:` when it fits at all.
    let warn_line = feeds
        .native_warn
        .as_deref()
        .map(normalize_feed_text)
        .filter(|line| crate::extraction::provenance::is_native_warn_line(line))
        .filter(|line| {
            recap.chars().count()
                + line_cost(&next_line)
                + line_cost(&last_line)
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
    let (dream_subjects, dream_entries): (Vec<WitnessSubject>, Vec<String>) = feeds
        .top_dream
        .iter()
        .filter(|dream| shown_ok(&WitnessSubject::from_dream(dream), &candidate_text))
        .filter(|dream| !normalize_feed_text(&dream.receipt).is_empty())
        .filter(|dream| has_substance(&normalize_feed_text(&dream.label)))
        .filter(|dream| is_qualified_dream_label(&normalize_feed_text(&dream.label)))
        .filter(|dream| !has_csr_text(&normalize_feed_text(&dream.verdict)))
        .map(|dream| {
            (
                WitnessSubject::from_dream(dream),
                format!(
                    "{} {} ({}, {})",
                    normalize_feed_text(&dream.label),
                    normalize_feed_text(&dream.verdict),
                    normalize_feed_text(&dream.date),
                    ctx.mark_receipt(&normalize_feed_text(&dream.receipt))
                ),
            )
        })
        .unzip();

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
        + line_cost(&next_line)
        + line_cost(&last_line)
        + line_cost(&warn_line);
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
    // The candidate check above may have leaned on an entry the budget then
    // dropped: keep a verdict only while the lines that are shown name it.
    let shown_text = [
        request_text.clone(),
        completed.clone().unwrap_or_default(),
        last_line.clone().unwrap_or_default(),
        next_line.clone().unwrap_or_default(),
        clauses[SETTLED].render().unwrap_or_default(),
        clauses[NOW].render().unwrap_or_default(),
    ]
    .join("\n");
    for (index, subjects) in [(RETIRED, &retired_subjects), (DREAMT, &dream_subjects)] {
        let named = subjects
            .iter()
            .take(clauses[index].selected)
            .take_while(|subject| shown_ok(subject, &shown_text))
            .count();
        clauses[index].selected = named;
    }

    // The user's own prompt is not evidence of anything. With no completed
    // clause, no `Next:` and no list clause that fit, the paragraph would only
    // echo the last thing they typed (typos included): abstain, so the fragment
    // fallback runs. `warn_line` is a notice about Claude Code, not about this
    // session, so it does not count either.
    // The "last turn did not finish" flag is a note about the session, not a
    // finding: alone it does not make a paragraph worth composing.
    let substantive = |index: usize| {
        let selected = clauses[index].selected;
        if index == NOW {
            selected.min(evidenced_now_entries)
        } else {
            selected
        }
    };
    if abstain_intent_only
        && completed.is_none()
        && next_line.is_none()
        && (0..clauses.len()).all(|index| substantive(index) == 0)
    {
        return None;
    }
    // With no head words, only a `Settled:` fact or a named `Now:` entry makes
    // the paragraph worth more than the fragment block and its LAST line: a
    // correction, a dream, a retired note or a bare `Next:` alone falls back.
    if headless && [SETTLED, NOW].iter().all(|&index| substantive(index) == 0) {
        return None;
    }

    let rendered_corrections = shown_corrections
        .iter()
        .take(clauses[CORRECTED].selected)
        .map(|line| CorrectionIdentity::from(*line))
        .collect();

    for line in last_line
        .into_iter()
        .chain(clauses.iter().filter_map(ListClause::render))
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

fn format_fact(fact: &SettledFact, ctx: &RecapContext) -> String {
    let claim = normalize_feed_text(&fact.claim);
    let claim = if claim.is_empty() {
        "evidence".to_string()
    } else {
        claim
    };
    format!(
        "{claim} ({})",
        ctx.mark_receipt(&normalize_feed_text(&fact.receipt))
    )
}

/// A dream label names what the conclusion is about well enough to find it: a
/// path, or a symbol said with its file (`send in pane/kitty.rs`). A bare
/// one-word symbol is not printed.
fn is_qualified_dream_label(label: &str) -> bool {
    label.split_whitespace().count() >= 2 || label.contains('/') || label.contains('.')
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

/// Most words of a typed ask the recap quotes. Longer, it is cut at a word
/// boundary and ends in an ellipsis.
pub const INTENT_MAX_WORDS: usize = 40;

/// Fewest words, after fillers, a later ask needs to be named as `Then:`.
const THEN_MIN_WORDS: usize = 4;

/// Longest todo title the `Now:` clause names.
const TODO_TITLE_MAX_CHARS: usize = 60;

/// Longest `Last:` text: the fragment block's `LAST:` budget.
const LAST_MAX_CHARS: usize = 120;

/// Fewest words a `Last:` finding needs ("Done." is not one).
const LAST_MIN_WORDS: usize = 2;

/// The session's whole ask, as typed; `Then:` compares a later ask against it
/// (the head itself is [`composed_intent`]). The stored
/// anchor is whole sentences up to [`INTENT_MAX_CHARS`]; when it is the start
/// of the session's first prompt, the rest of that prompt's opening paragraph
/// is quoted too. Either way [`cap_words`] applies. No word is added or
/// respelled, and which prompt is the intent is unchanged.
pub(crate) fn full_intent(ep: &crate::hooks::stop::Episode) -> Option<String> {
    let intent = recap_intent(ep)?;
    let whole = first_prompt_continuation(ep, &intent).unwrap_or(intent);
    Some(cap_words(&whole))
}

/// The first prompt's opening paragraph, when the stored anchor is its start
/// and it says more. The stored prompt was cut at
/// [`LEGACY_REQUEST_CAP_BYTES`]; a word that cut may have split is dropped and
/// an ellipsis marks the cut.
fn first_prompt_continuation(ep: &crate::hooks::stop::Episode, intent: &str) -> Option<String> {
    use crate::extraction::provenance::extractable;
    ep.recap_evidence_version?;
    let text = extractable(&ep.request)?;
    if crate::import::contamination::has_csr_text(&text) {
        return None;
    }
    let mut paragraph = first_paragraph(&text)?;
    let cut_inside =
        ep.request.len() + 4 > LEGACY_REQUEST_CAP_BYTES && paragraph == sanitize_preview(&text);
    if cut_inside {
        let at = paragraph.rfind(char::is_whitespace)?;
        paragraph = format!(
            "{}\u{2026}",
            paragraph[..at].trim_end().trim_end_matches(TRAILING_JUNK)
        );
    }
    let (whole, anchor) = (word_key(&paragraph), word_key(intent));
    (!anchor.is_empty() && whole.len() > anchor.len() && whole.starts_with(&format!("{anchor} ")))
        .then_some(paragraph)
}

/// `text` up to [`INTENT_MAX_WORDS`] words. Longer, it is cut after the last
/// whole word that fits and ends in `…`.
pub(crate) fn cap_words(text: &str) -> String {
    cap_words_to(text, INTENT_MAX_WORDS)
}

/// Most words of the SessionStart head clause.
pub const HEAD_INTENT_MAX_WORDS: usize = 12;

/// The SessionStart head clause: the episode's stored intent
/// ([`recap_intent`]: the anchor task Stop stored, as whole sentences), cut to
/// its first sentence and then to [`HEAD_INTENT_MAX_WORDS`] words at a word
/// boundary, ending in `…` when cut. No model call, no word added or
/// respelled. The head is never the raw prefix of a longer prompt: the rest of
/// the ask stays in the transcript, one `csr_transcript` away.
pub(crate) fn composed_intent(ep: &crate::hooks::stop::Episode) -> Option<String> {
    // The assistant's own first sentence under the anchor task, when Stop kept one: the task
    // in the session's words rather than the person's raw prompt.
    if let Some(reply) = ep
        .recap_extras
        .anchor_reply
        .as_deref()
        .filter(|reply| !crate::import::contamination::has_csr_text(reply))
    {
        return Some(cap_words_to(reply, HEAD_INTENT_MAX_WORDS));
    }
    let sentence = match recap_intent(ep) {
        Some(stored) => first_sentence(&stored).unwrap_or(stored),
        None => first_prompt_sentence(ep)?,
    };
    Some(cap_words_to(&sentence, HEAD_INTENT_MAX_WORDS))
}

/// The head's fallback when [`recap_intent`] abstains: the first sentence of
/// the ask, however long (the 12-word cut follows). The source is the stored
/// anchor task when there is one (a first sentence over [`INTENT_MAX_CHARS`]
/// made `recap_intent` abstain), else, for an episode stored before the
/// evidence fields, the session's first prompt, which stands for the intent
/// only in a session short enough that it is not a stale topic. A current
/// episode with no stored anchor stays without a head: Stop found no clean
/// task, and its first prompt is not a stand-in.
fn first_prompt_sentence(ep: &crate::hooks::stop::Episode) -> Option<String> {
    use crate::extraction::provenance::extractable;
    let source = match (&ep.anchor_request, ep.recap_evidence_version) {
        (Some(anchor), Some(_)) => anchor,
        (None, Some(_)) => return None,
        _ if ep.message_count > LEGACY_SINGLE_TOPIC_MAX_MESSAGES => return None,
        _ => &ep.request,
    };
    let text = extractable(source)?;
    if crate::import::contamination::has_csr_text(&text) {
        return None;
    }
    first_sentence(&first_paragraph(&text)?)
}

/// `text` up to `max_words` words. Longer, it is cut after the last whole word
/// that fits and ends in `…`.
pub(crate) fn cap_words_to(text: &str, max_words: usize) -> String {
    let text = text.trim();
    let mut words = 0;
    let mut in_word = false;
    for (at, character) in text.char_indices() {
        if character.is_whitespace() {
            if in_word && words == max_words {
                return format!("{}\u{2026}", text[..at].trim_end_matches(TRAILING_JUNK));
            }
            in_word = false;
        } else if !in_word {
            in_word = true;
            words += 1;
        }
    }
    text.to_string()
}

/// A command whose object only points back into the conversation ("save
/// this", "do it"): it names nothing a reader of the recap could act on.
const DEICTIC_OBJECTS: &[&str] = &["this", "that", "it", "them", "these", "those"];

/// True when `text` is a later ask worth naming: a request ([`is_request`]),
/// not an acknowledgement, at least [`THEN_MIN_WORDS`] words after fillers,
/// and not a command whose object is only "this" or "it".
pub(crate) fn is_substantive_ask(text: &str) -> bool {
    if !is_request(text) || is_acknowledgement(text) {
        return false;
    }
    let words: Vec<String> = text
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .skip_while(|word| REQUEST_FILLERS.contains(&word.as_str()))
        .collect();
    if words.len() < THEN_MIN_WORDS {
        return false;
    }
    !(COMMAND_OPENERS.contains(&words[0].as_str()) && DEICTIC_OBJECTS.contains(&words[1].as_str()))
}

/// `Last: <finding> (conv <sid8>)`: the closing summary's finding as the
/// fragment block's `LAST:` line reads it (`conclusion_preview`: its verdict
/// sentence, else its first sentence, with a distinct closing sentence when
/// both fit), so a recap never says less than the block it replaces. `None`
/// when there is none, it is CSR text, or `head` already says it.
fn last_clause(ep: &crate::hooks::stop::Episode, head: &str) -> Option<String> {
    use crate::extraction::provenance::extractable;
    let session8: String = ep.session_id.chars().take(8).collect();
    if session8.is_empty() {
        return None;
    }
    let text = extractable(&ep.completed)?;
    if crate::import::contamination::has_csr_text(&text) {
        return None;
    }
    let finding = crate::hooks::session_start::conclusion_preview(&text, LAST_MAX_CHARS);
    let body = finding
        .trim_end()
        .trim_end_matches(['.', ':', ';', ',', '-'])
        .trim_end();
    if !has_substance(body) || body.split_whitespace().count() < LAST_MIN_WORDS {
        return None;
    }
    let (said, last) = (word_key(head), word_key(body));
    if said.contains(&last) {
        return None;
    }
    Some(format!("Last: {body} (conv {session8})."))
}

/// What a project-wide verdict is about: a file, a symbol, or both. The
/// SessionStart recap shows the verdict only when its own text names one of
/// them ([`WitnessSubject::is_named_in`]).
struct WitnessSubject {
    file: String,
    symbol: Option<String>,
}

impl WitnessSubject {
    fn from_dream(dream: &DreamClause) -> Self {
        Self {
            file: dream.file.trim().to_string(),
            symbol: dream
                .symbol
                .as_deref()
                .map(str::trim)
                .filter(|symbol| !symbol.is_empty())
                .map(str::to_string),
        }
    }

    /// A retired line carries one label: the witness symbol, else its file.
    fn from_label(label: &str) -> Self {
        let label = label.trim();
        if label.contains(['/', '.']) {
            Self {
                file: label.to_string(),
                symbol: None,
            }
        } else {
            Self {
                file: String::new(),
                symbol: Some(label.to_string()),
            }
        }
    }

    /// A narrow whitelist, nothing fuzzy: the file's basename (case-insensitive)
    /// or the symbol (case-sensitive) appears in `text` as a whole word of at
    /// least 4 characters. What the session merely touched does not count.
    fn is_named_in(&self, text: &str) -> bool {
        fn named(word: &str, text: &str) -> bool {
            word.chars().count() >= 4
                && text.match_indices(word).any(|(at, _)| {
                    let before = text[..at].chars().next_back();
                    let after = text[at + word.len()..].chars().next();
                    let is_word =
                        |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                    !is_word(before) && !is_word(after)
                })
        }
        let basename = self
            .file
            .rsplit('/')
            .find(|part| !part.is_empty())
            .unwrap_or("")
            .to_lowercase();
        named(&basename, &text.to_lowercase())
            || self
                .symbol
                .as_deref()
                .is_some_and(|symbol| named(symbol, text))
    }
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

/// A later ask, as a `Then:` clause: the session's literal last typed message
/// when it is a substantive ask ([`is_substantive_ask`]), else the latest task
/// prompt Stop recorded after the anchor (`compact_anchor`) when that one is.
/// Its opening paragraph, cut by [`cap_words`]; never one the intent already
/// covers. `None` means the recap names no later ask: a remark, "save this",
/// "ok" or "continue" is never named.
pub(crate) fn then_clause(ep: &crate::hooks::stop::Episode) -> Option<String> {
    use crate::extraction::provenance::extractable;
    let intent = recap_intent(ep)?;
    let whole = full_intent(ep).unwrap_or_default();
    let anchor = ep.anchor_request.as_deref().unwrap_or("");
    let later = ep
        .compact_anchor
        .as_ref()
        .map(|later| later.request.as_str());
    [ep.latest_ask.as_deref(), later]
        .into_iter()
        .flatten()
        .find_map(|typed| {
            let ask = extractable(typed).and_then(|text| first_paragraph(&text))?;
            if crate::import::contamination::has_csr_text(&ask) || !is_substantive_ask(&ask) {
                return None;
            }
            let ask_key = word_key(&ask);
            let covered = |other: &str| {
                let other = word_key(other);
                !other.is_empty() && (other.contains(&ask_key) || ask_key.contains(&other))
            };
            (!ask_key.is_empty() && !covered(&intent) && !covered(anchor) && !covered(&whole))
                .then(|| cap_words(&ask))
        })
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
fn session_settled(
    ep: &crate::hooks::stop::Episode,
    ctx: &RecapContext,
    drop_predating: bool,
) -> Vec<String> {
    // Receipts sit in transcript order and the anchor's own form one run: one
    // listed before the anchor's first receipt was made under an earlier
    // prompt, before the anchor request was typed, and answers something else.
    let first_own = ep
        .anchor_receipts
        .as_ref()
        .and_then(|own| own.iter().min().copied())
        .filter(|_| drop_predating);
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
        .filter(|(index, _)| first_own.is_none_or(|first| *index >= first))
        .map(|(_, entry)| ctx.mark_entry(entry))
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
fn recap_completed(ep: &crate::hooks::stop::Episode, ctx: &RecapContext) -> Option<String> {
    let entries: Vec<String> = displayed_receipts(&anchor_completions(ep))
        .into_iter()
        .map(|(_, entry)| ctx.mark_entry(entry))
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
#[path = "recap_tests.rs"]
mod tests;

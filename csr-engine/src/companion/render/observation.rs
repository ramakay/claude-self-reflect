//! The detail popup of an observation card ("What confused the agent", "Should be a skill / memory / tool").
//! A card row is one truncated line; the popup shows one item in full, what happened, and a next
//! step. The explanation and next step come from the dreamer when it wrote them (`explain`, `next`)
//! and are otherwise composed here from the item's kind, line and evidence. No model runs at
//! render time.
use super::instrument::{evidence_receipt, should_be_kind, CW, M, P};
use super::instrument_gfx::*;
use super::theme::{self, Tokens, S2, S3};
use crate::companion::string;
use serde_json::Value;
use std::collections::BTreeSet;

/// Which observation card an item belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObsCard {
    Confusing,
    ShouldBe,
    /// Any other row the pane drew with a trailing "…": the popup shows its words in full. Its
    /// items are `{title, line}` and come from the frame that drew them.
    More,
    /// A thread of Lost and found: the popup is a handoff for a fresh Claude Code session.
    Lost,
    /// What CSR told Claude: the tall popup behind the "CSR handed over" row, with the Resume key.
    Handed,
}
impl ObsCard {
    pub fn other(self) -> Self {
        match self {
            Self::Confusing => Self::ShouldBe,
            Self::ShouldBe => Self::Confusing,
            Self::More => Self::More,
            Self::Lost => Self::Lost,
            Self::Handed => Self::Handed,
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::Confusing => super::instrument::CONFUSED_TITLE,
            Self::ShouldBe => super::instrument::SHOULD_BE_TITLE,
            Self::More => "In full",
            Self::Lost => "Lost and found",
            Self::Handed => crate::companion::handed::TITLE,
        }
    }
}

/// What the last `s` or `x` said: one plain line, and whether it worked.
pub type Note = (&'static str, bool);

/// The item the popup shows: its card and place on it, whether its text was just copied, and what
/// the last send said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PopupAt {
    pub card: ObsCard,
    pub index: usize,
    pub copied: bool,
    pub note: Option<Note>,
    /// The first row a scrolling popup shows (the "What CSR told Claude" popup).
    pub scroll: usize,
}

/// The key a dismissed item is kept under in the likes log: the session, the card and the line.
pub fn dismiss_key(sid: &str, card: &str, line: &str) -> String {
    // FNV-1a: the line is long and free text, the key must stay short and stable across runs.
    let hash = line.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("observation:{sid}:{card}:{hash:016x}")
}

/// Two rows are one event when the words of their `line` and `evidence` overlap by at least this
/// share of the shorter row. Measured on the 11 real rows of one session: rows of different events
/// overlap by at most 0.14, and the loosest link inside one event is 0.50. 0.35 sits in that gap.
pub(super) const SAME_EVENT_OVERLAP: f64 = 0.35;
/// A row with fewer distinct words than this is too thin to call a repeat: it abstains.
const SAME_EVENT_MIN_WORDS: usize = 5;
/// Rows that both quote must share this many content words in the quote. A shared sentence shape
/// ("ran X without waiting for approval") is no proof of one event; the record they quote is. On
/// the real rows, every link that holds an event together shares at least 2.
const SAME_EVENT_QUOTE_WORDS: usize = 2;

/// What one observation row says, reduced to compare with another: the content words of its line and
/// evidence, and the turns its evidence cites.
#[derive(Clone, Debug, Default)]
pub(super) struct EventKey {
    words: BTreeSet<String>,
    quote: BTreeSet<String>,
    turns: BTreeSet<u32>,
}

/// Words too common to say which event a row is about.
const FILLER: &[&str] = &[
    "a", "an", "the", "and", "or", "of", "to", "in", "on", "at", "by", "for", "with", "from", "as",
    "is", "it", "its", "was", "were", "be", "been", "that", "this", "these", "those", "then",
    "than", "into", "after", "before", "first", "later", "once", "not", "no", "up", "out", "over",
    "so", "but", "also", "has", "have", "had", "did", "do", "does",
];

fn event_words(text: &str) -> BTreeSet<String> {
    let lower = text.to_lowercase();
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() >= 2 && !FILLER.contains(w))
        .map(|w| {
            // One ending cut, so "retracted" and "retract", "tests" and "test" meet.
            for end in ["ing", "ed", "es", "s"] {
                if w.len() >= 4 + end.len() && w.ends_with(end) {
                    return w[..w.len() - end.len()].to_owned();
                }
            }
            w.to_owned()
        })
        .collect()
}

/// The turn numbers an evidence string cites in its lead-in ("Turns 4 and 12: ..."). Digits inside
/// a quote, or in a lead-in that does not say "turn", are not turn numbers.
fn cited_turns(evidence: &str) -> BTreeSet<u32> {
    let lead = evidence.split(':').next().unwrap_or(evidence);
    if !lead.to_lowercase().contains("turn") {
        return BTreeSet::new();
    }
    lead.split(|c: char| !c.is_ascii_digit())
        .filter_map(|n| n.parse().ok())
        .collect()
}

pub(super) fn event_key(item: &Value) -> EventKey {
    let (line, evidence) = (string(&item["line"]), string(&item["evidence"]));
    let turns = cited_turns(evidence);
    // A citation ("Turns 4 and 13: ...") is how the dreamer writes it, not what the row is about:
    // two rows on the same turns would share its words. Only the quote after it is compared.
    let said = match evidence.split_once(':') {
        Some((_, quote)) if !turns.is_empty() => quote,
        None if !turns.is_empty() => "",
        _ => evidence,
    };
    EventKey {
        words: event_words(&format!("{line} {said}")),
        quote: event_words(said),
        turns,
    }
}

/// Whether two rows tell one event. A false merge hides a real finding, a repeat only costs a row,
/// so every doubt answers no: rows that cite different turns are two events, and a row too thin to
/// compare is its own event.
pub(super) fn same_event(a: &EventKey, b: &EventKey) -> bool {
    if !a.turns.is_empty() && !b.turns.is_empty() && a.turns != b.turns {
        return false;
    }
    if a.words.len() < SAME_EVENT_MIN_WORDS || b.words.len() < SAME_EVENT_MIN_WORDS {
        return false;
    }
    let backed = if a.quote.is_empty() || b.quote.is_empty() {
        !a.turns.is_empty() && a.turns == b.turns
    } else {
        a.quote.intersection(&b.quote).count() >= SAME_EVENT_QUOTE_WORDS
    };
    backed && overlap(a, b) >= SAME_EVENT_OVERLAP
}

/// Whether two groups of rows cite different turns. A row with no citation can match both, and must
/// not make them one.
pub(super) fn turns_conflict(a: &[EventKey], b: &[EventKey]) -> bool {
    a.iter().any(|x| {
        b.iter()
            .any(|y| !x.turns.is_empty() && !y.turns.is_empty() && x.turns != y.turns)
    })
}

/// The shared words as a share of the shorter row's words.
pub(super) fn overlap(a: &EventKey, b: &EventKey) -> f64 {
    let shorter = a.words.len().min(b.words.len());
    if shorter == 0 {
        return 0.;
    }
    a.words.intersection(&b.words).count() as f64 / shorter as f64
}

/// One item, in full.
#[derive(Clone, Debug, PartialEq)]
pub struct Detail {
    /// The "Same page?" card: its message is the primary action.
    pub same_page: bool,
    pub card: ObsCard,
    /// The popup's heading: the card's title, or what a shortened row is called.
    pub heading: String,
    /// 1-based place on the card and the card's size.
    pub place: (usize, usize),
    /// A should-be item's label: skill, memory, hook, CLAUDE.md rule or tool.
    pub kind: Option<&'static str>,
    pub line: String,
    pub evidence: String,
    /// "What happened": one plain sentence.
    pub explain: String,
    /// "Next step for you": one concrete action.
    pub next: String,
    /// What `c` puts on the clipboard: the item and its explanation, or for a should-be item a
    /// draft ready to paste.
    pub copy: String,
    pub copied: bool,
    pub note: Option<Note>,
    /// What `s` types into Claude's prompt, unsent. Empty when the popup has no such key.
    pub send: String,
    /// A Lost and found thread's editorial view: summary, session numbers, where CSR keeps it.
    pub lost: Option<crate::companion::handoff::View>,
    /// What CSR told Claude, sectioned, and the row it is scrolled to.
    pub handed: Option<(crate::companion::handed::View, usize)>,
}

/// The turn numbers an evidence string cites, or `None` when it only quotes.
fn cited(item: &Value) -> Option<String> {
    let receipt = evidence_receipt(item);
    (receipt != "evidence").then_some(receipt)
}

fn stated(item: &Value, field: &str) -> Option<String> {
    let text = string(&item[field]).trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// A file-name-safe slug of the first words of `line`.
fn slug(line: &str) -> String {
    let words: Vec<String> = line
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(5)
        .map(str::to_lowercase)
        .collect();
    if words.is_empty() {
        "untitled".into()
    } else {
        words.join("-")
    }
}

fn explanation(card: ObsCard, item: &Value) -> String {
    if matches!(card, ObsCard::More | ObsCard::Lost | ObsCard::Handed) {
        return String::new();
    }
    if let Some(said) = stated(item, "explain") {
        return said;
    }
    let cite = cited(item).unwrap_or_else(|| "its quoted evidence".into());
    match card {
        ObsCard::More | ObsCard::Lost | ObsCard::Handed => String::new(),
        ObsCard::Confusing => format!(
            "The dreamer flagged this as a place where Claude misread you, contradicted itself or repeated a mistake, citing {cite}."
        ),
        ObsCard::ShouldBe => format!(
            "You asked for this more than once, citing {cite}, so the dreamer thinks it belongs in a {} instead.",
            should_be_kind(item)
        ),
    }
}

fn next_step(card: ObsCard, item: &Value) -> String {
    if matches!(card, ObsCard::More | ObsCard::Lost | ObsCard::Handed) {
        return String::new();
    }
    if let Some(said) = stated(item, "next") {
        return said;
    }
    match card {
        ObsCard::More | ObsCard::Lost | ObsCard::Handed => String::new(),
        ObsCard::Confusing => format!(
            "Re-read {} in the transcript, and if Claude is still off, tell it the right reading in your next message.",
            cited(item).unwrap_or_else(|| "the quoted evidence".into())
        ),
        ObsCard::ShouldBe => match should_be_kind(item) {
            "skill" => "Press c to copy a skill draft, then save it as .claude/skills/<name>/SKILL.md.",
            "memory" => "Press c to copy a memory draft, then ask the agent to remember it.",
            "hook" => "Press c to copy a hook sketch, then add the hook to your settings.",
            "CLAUDE.md rule" => "Press c to copy the rule, then paste it into CLAUDE.md.",
            _ => "Press c to copy a tool sketch, then turn it into a small script Claude can call.",
        }
        .into(),
    }
}

/// A draft of what the should-be item would become, ready to paste. Nothing is written for you.
fn draft(item: &Value, explain: &str) -> String {
    let (line, evidence) = (string(&item["line"]), string(&item["evidence"]));
    match should_be_kind(item) {
        "skill" => format!(
            "---\nname: {}\ndescription: {line}\n---\n\n# {line}\n\nSeen: {evidence}\nWhy: {explain}\n\n## When to use\n- \n\n## Steps\n1. \n",
            slug(line)
        ),
        "memory" => format!(
            "---\nname: {}\ndescription: {line}\ntype: feedback\n---\n\n{line}\n\n**Why:** {explain}\n**Seen:** {evidence}\n",
            slug(line)
        ),
        "hook" => format!(
            "# Hook idea: {line}\n# Seen: {evidence}\n# Why: {explain}\n# Event: PreToolUse, PostToolUse, UserPromptSubmit or Stop (pick one)\n# Command: <the script that does it>\n"
        ),
        "CLAUDE.md rule" => format!("- {line}\n  <!-- seen: {evidence} -->\n"),
        _ => format!(
            "Tool idea: {line}\nSeen: {evidence}\nWhy: {explain}\nInput: <what it takes>\nOutput: <what it returns>\n"
        ),
    }
}

/// What `s` types into Claude's prompt, unsent. It opens with the line CSR's own notes open with, so
/// the quoted block is CSR's words and what follows is yours; a `More` item has none.
fn send_text(card: ObsCard, item: &Value, explain: &str) -> String {
    // The "Same page?" card brings its own message.
    if let Some(send) = item["send"].as_str() {
        return send.to_owned();
    }
    let (line, evidence) = (string(&item["line"]), string(&item["evidence"]));
    let quoted = |text: &str| {
        text.lines()
            .map(|l| format!("> {l}").trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let opener = "Here is a note offered by CSR (claude-self-reflect):";
    match card {
        ObsCard::More | ObsCard::Lost | ObsCard::Handed => String::new(),
        ObsCard::Confusing => format!(
            "{opener}\n\n{}\n\nYou may have gone off course here. Please say what you got wrong and correct course.",
            quoted(&format!("What confused the agent: {line}\nEvidence: {evidence}"))
        ),
        ObsCard::ShouldBe => {
            let kind = should_be_kind(item);
            format!(
                "{opener}\n\n{}\n\nPlease make this {kind} from the draft above, and show me what you wrote.",
                quoted(&format!(
                    "{kind}: {line}\nEvidence: {evidence}\nDraft:\n{}",
                    draft(item, explain).trim_end()
                ))
            )
        }
    }
}

/// The popup's content for `items[at.index]` of `at.card`, or `None` when there is no such item.
pub fn detail(items: &[Value], at: PopupAt) -> Option<Detail> {
    let item = items.get(at.index)?;
    let explain = explanation(at.card, item);
    let next = next_step(at.card, item);
    let (line, evidence) = (
        string(&item["line"]).to_owned(),
        string(&item["evidence"]).to_owned(),
    );
    // A Lost and found thread's one handoff prompt: what `c` copies and `s` types. It is empty
    // while the facts are still being read, so a key then does nothing.
    let handoff = (at.card == ObsCard::Lost)
        .then(|| crate::companion::handoff::draft(item).unwrap_or_default());
    // What Resume types and `c` copies for the "What CSR told Claude" popup: one text.
    let resume = (at.card == ObsCard::Handed)
        .then(|| crate::companion::handed::draft(item).unwrap_or_default());
    let copy = match at.card {
        ObsCard::Lost => handoff.clone().unwrap_or_default(),
        ObsCard::Handed => resume.clone().unwrap_or_default(),
        ObsCard::Confusing => match item["send"].as_str() {
            Some(send) => send.to_owned(),
            None => format!(
                "What confused the agent: {line}\nEvidence: {evidence}\nWhat happened: {explain}\nNext step: {next}\n"
            ),
        },
        ObsCard::ShouldBe => draft(item, &explain),
        ObsCard::More => format!("{}\n{line}\n", string(&item["title"])),
    };
    let heading = match at.card {
        ObsCard::Confusing if item["same_page"] == true => {
            crate::companion::same_page::TITLE.to_owned()
        }
        ObsCard::More => string(&item["title"]).to_owned(),
        ObsCard::Lost => ObsCard::Lost.title().to_owned(),
        card => card.title().to_owned(),
    };
    let send = handoff
        .or(resume)
        .unwrap_or_else(|| send_text(at.card, item, &explain));
    let handed =
        (at.card == ObsCard::Handed).then(|| (crate::companion::handed::view_of(item), at.scroll));
    let lost_view = (at.card == ObsCard::Lost).then(|| crate::companion::handoff::view(item));
    let line = if at.card == ObsCard::Lost {
        string(&item["text"]).to_owned()
    } else {
        line
    };
    Some(Detail {
        same_page: item["same_page"] == true,
        card: at.card,
        heading,
        place: (at.index + 1, items.len()),
        kind: (at.card == ObsCard::ShouldBe).then(|| should_be_kind(item)),
        line,
        evidence,
        explain,
        next,
        copy,
        copied: at.copied,
        note: at.note,
        send,
        lost: lost_view,
        handed,
    })
}

/// The popup as plain rows: the whole pane in text mode, and the picture's text beneath it.
pub fn rows(d: &Detail, cols: usize) -> Vec<String> {
    use super::text;
    // A word wider than the pane breaks between letters; `text::wrap` alone would cut it short.
    let wrap = |s: &str| {
        let width = cols.max(1);
        let spread = s
            .split_whitespace()
            .flat_map(|word| {
                let letters: Vec<char> = word.chars().collect();
                if text::len(word) <= width {
                    return vec![word.to_string()];
                }
                letters
                    .chunks(width)
                    .map(|c| c.iter().collect::<String>())
                    .collect()
            })
            .collect::<Vec<_>>();
        // Pieces of one word each fill a row, so the wrap puts them on rows of their own.
        text::wrap(&spread.join(" "), width, 1000)
    };
    if d.card == ObsCard::More {
        let mut out = wrap(&d.heading);
        out.extend(wrap(&d.line));
        if d.copied {
            out.extend(wrap("Copied."));
        }
        return out;
    }
    if let Some((v, _)) = &d.handed {
        let mut out = vec![];
        for line in v.plain().lines() {
            if line.is_empty() {
                continue;
            }
            out.extend(wrap(line));
        }
        if d.copied {
            out.extend(wrap("Copied."));
        }
        if let Some((note, _)) = d.note {
            out.extend(wrap(note));
        }
        return out;
    }
    if let Some(v) = &d.lost {
        let mut out = wrap(&format!("{} · {} of {}", d.heading, d.place.0, d.place.1));
        out.extend(wrap(&v.kicker));
        for line in crate::companion::handoff::thread_lines(&v.title) {
            out.extend(wrap(&line));
        }
        out.extend(wrap(crate::companion::handoff::HANDS_OVER));
        if d.copied {
            out.extend(wrap("Copied."));
        }
        if let Some((note, _)) = d.note {
            out.extend(wrap(note));
        }
        return out;
    }
    let mut out = wrap(&format!(
        "{} · {} of {}{}",
        d.heading,
        d.place.0,
        d.place.1,
        d.kind.map(|k| format!(" · {k}")).unwrap_or_default()
    ));
    out.extend(wrap(&d.line));
    out.extend(wrap(&format!("Evidence: {}", d.evidence)));
    out.extend(wrap(&format!("What happened: {}", d.explain)));
    out.extend(wrap(&format!("Next step for you: {}", d.next)));
    if d.copied {
        out.extend(wrap("Copied."));
    }
    if let Some((note, _)) = d.note {
        out.extend(wrap(note));
    }
    out
}

/// The keys the popup answers to, as the footer lists them: an observation item can also be sent
/// to Claude or dismissed, a shortened row in full only copied.
pub fn footer_keys(d: Option<&Detail>) -> Vec<(&'static str, &'static str)> {
    if d.is_some_and(|d| d.card == ObsCard::Handed) {
        return vec![
            ("r", "resume"),
            ("c", "copy"),
            ("j/k", "scroll"),
            ("esc", "close"),
        ];
    }
    if d.is_some_and(|d| d.same_page) {
        return vec![
            ("s", "intervene"),
            ("c", "copy"),
            ("x", "hide"),
            ("esc", "close"),
        ];
    }
    if d.is_some_and(|d| d.card == ObsCard::Lost) {
        return vec![
            ("c", "copy draft"),
            ("s", "enter in Claude Code"),
            ("tab", "next"),
            ("esc", "close"),
        ];
    }
    let copy = match d.map(|d| d.card) {
        Some(ObsCard::ShouldBe) => "copy draft",
        _ => "copy",
    };
    if d.is_some_and(|d| d.card == ObsCard::More) {
        return vec![("c", copy), ("esc", "close")];
    }
    vec![
        ("c", copy),
        ("s", "send"),
        ("x", "dismiss"),
        ("tab", "next"),
        ("esc", "close"),
    ]
}

/// Wrap `s` into at most `n` lines, none of them cut: with `n` big enough the words are whole.
fn lines_of(f: &Fonts, s: &str, st: St, w: f32, n: usize) -> Vec<String> {
    // A word wider than the panel (a path, a JSON blob) breaks between letters; the greedy wrap
    // alone would leave it running off the edge.
    let mut lines = vec![];
    for line in f.wrap(s, st, w) {
        let mut piece = String::new();
        for ch in line.chars() {
            piece.push(ch);
            if f.width(&piece, st) > w && piece.chars().count() > 1 {
                piece.pop();
                lines.push(std::mem::take(&mut piece));
                piece.push(ch);
            }
        }
        lines.push(piece);
    }
    if lines.len() > n {
        let mut out: Vec<String> = lines[..n - 1].to_vec();
        out.push(f.fit(&lines[n - 1..].join(" "), st, w));
        return out;
    }
    lines
}

// ---------------------------------------------------------------- the popup's type

/// The popup's type scale in layout pt: four levels, each a plain step from the next. The title
/// names the popup, the action keys are the buttons along its foot, the body carries the item and
/// its sentences, and the caption carries headings, evidence and the numbers.
pub const POP_TITLE_SIZE: f32 = 26.;
pub const POP_CTA_SIZE: f32 = 18.5;
pub const POP_BODY_SIZE: f32 = 16.25;
pub const POP_CAPTION_SIZE: f32 = 13.65;
pub const POP_TITLE_LINE: f32 = 36.;
pub const POP_BODY_LINE: f32 = 23.;
pub const POP_CAPTION_LINE: f32 = 19.;
/// What each level was before the scale. The title was the item's own line in 19 pt serif; the
/// action keys were drawn as caption text; the body and the caption are the pane's.
pub const WAS_TITLE_SIZE: f32 = 19.;
pub const WAS_CTA_SIZE: f32 = theme::CAPTION_SIZE;
pub const WAS_BODY_SIZE: f32 = theme::BODY_SIZE;
pub const WAS_CAPTION_SIZE: f32 = theme::CAPTION_SIZE;

/// An action key is a pill this tall, `CTA_PAD` of room each side of its words, `CTA_GAP` apart.
const CTA_H: f32 = 34.;
const CTA_PAD: f32 = 9.;
const CTA_INNER: f32 = 6.;
const CTA_GAP: f32 = 8.;
/// The popup panel's left edge, and its text column, in layout pt.
const PANEL_X: f32 = M + 8.;
const TEXT_X: f32 = PANEL_X + P;
pub const TEXT_W: f32 = CW - 16. - 2. * P;

/// One line of `s` in `st`: whole when it fits. A path too wide for the line is cut in its middle
/// so its file name stays; any other line ends in "…". Never a second line.
pub(super) fn one_line(f: &Fonts, s: &str, st: St, w: f32) -> String {
    let s = s.trim();
    if f.width(s, st) <= w {
        return s.to_owned();
    }
    if let Some(tok) = s
        .split(' ')
        .find(|t| t.chars().count() > 24 && (t.starts_with('/') || t.starts_with("~/")))
    {
        let chars: Vec<char> = tok.chars().collect();
        for keep in (12..chars.len()).rev() {
            let head = (keep * 2 / 5).max(6);
            let tail = keep - head;
            let cut = format!(
                "{}…{}",
                chars[..head].iter().collect::<String>(),
                chars[chars.len() - tail..].iter().collect::<String>()
            );
            let line = s.replacen(tok, &cut, 1);
            if f.width(&line, st) <= w {
                return line;
            }
        }
    }
    f.fit(s, st, w)
}

/// The sentences of `text`, each cut after its full stop, semicolon, question or exclamation mark.
fn sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let (mut out, mut cur) = (vec![], String::new());
    for (i, &c) in chars.iter().enumerate() {
        cur.push(c);
        if matches!(c, '.' | ';' | '?' | '!') && chars.get(i + 1).is_some_and(|n| n.is_whitespace())
        {
            out.push(std::mem::take(&mut cur));
        }
    }
    out.push(cur);
    out.into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The clauses of a sentence, each cut after its comma or colon.
fn clauses(sentence: &str) -> Vec<String> {
    let chars: Vec<char> = sentence.chars().collect();
    let (mut out, mut cur) = (vec![], String::new());
    for (i, &c) in chars.iter().enumerate() {
        cur.push(c);
        if matches!(c, ',' | ':') && chars.get(i + 1).is_some_and(|n| n.is_whitespace()) {
            out.push(std::mem::take(&mut cur));
        }
    }
    out.push(cur);
    out.into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// `text` as one-line items, never wrapped: each sentence is its own item; a sentence too wide
/// for a line is cut at its commas and colons into items that each fit, so no line ends in the
/// middle of a clause. A clause still wider than the line ends in "…". At most `max` items; when
/// more were left out the last one ends in "…".
fn items(f: &Fonts, text: &str, st: St, w: f32, max: usize) -> Vec<String> {
    if max == 0 {
        return vec![];
    }
    let mut lines: Vec<String> = vec![];
    for sentence in sentences(text) {
        if f.width(&sentence, st) <= w {
            lines.push(sentence);
            continue;
        }
        let mut cur = String::new();
        for clause in clauses(&sentence) {
            let joined = if cur.is_empty() {
                clause.clone()
            } else {
                format!("{cur} {clause}")
            };
            if f.width(&joined, st) <= w {
                cur = joined;
            } else {
                if !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                }
                cur = clause;
            }
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
    }
    let cut = lines.len() > max;
    lines.truncate(max);
    if cut {
        if let Some(last) = lines.last_mut() {
            last.push('…');
        }
    }
    lines.iter().map(|l| one_line(f, l, st, w)).collect()
}

/// The keys the picture's popup shows as buttons, in the short words that fit the popup's width
/// at the action size. The text popup's footer keeps `footer_keys`.
pub fn cta_keys(d: Option<&Detail>) -> Vec<(&'static str, &'static str)> {
    match d.map(|d| d.card) {
        Some(ObsCard::Handed) => vec![("r", "Resume"), ("c", "copy"), ("esc", "close")],
        Some(ObsCard::Lost) => vec![
            ("c", "copy draft"),
            ("s", "enter"),
            ("tab", "next"),
            ("esc", "close"),
        ],
        Some(ObsCard::More) => vec![("c", "copy"), ("esc", "close")],
        Some(ObsCard::Confusing) if d.is_some_and(|d| d.same_page) => vec![
            ("s", "Intervene"),
            ("c", "copy"),
            ("x", "hide"),
            ("esc", "close"),
        ],
        _ => vec![
            ("c", "copy"),
            ("s", "send"),
            ("x", "hide"),
            ("tab", "next"),
            ("esc", "close"),
        ],
    }
}

/// Where each action key sits along the foot, from `x0`: key, label, left, width, key width.
fn cta_layout(
    f: &Fonts,
    d: &Detail,
    x0: f32,
    t: &Tokens,
) -> Vec<(&'static str, &'static str, f32, f32, f32)> {
    let (key_st, label_st) = (sans(POP_CTA_SIZE, t.bold), sans(POP_CTA_SIZE, t.medium));
    let mut x = x0;
    cta_keys(Some(d))
        .into_iter()
        .map(|(key, label)| {
            let kw = f.width(key, key_st);
            let w = 2. * CTA_PAD + kw + CTA_INNER + f.width(label, label_st);
            let at = (key, label, x, w, kw);
            x += w + CTA_GAP;
            at
        })
        .collect()
}

/// How wide the action keys are laid out together, in layout pt.
pub fn cta_row_width(f: &Fonts, d: &Detail, t: &Tokens) -> f32 {
    cta_layout(f, d, 0., t)
        .last()
        .map_or(0., |(_, _, x, w, _)| x + w)
}

/// Room the one-line note under the action keys takes, in layout pt.
fn note_h(d: &Detail) -> f32 {
    if d.note.is_some() {
        S2 + POP_CAPTION_LINE
    } else {
        0.
    }
}

/// The footer: a hairline, the action keys as pills, and the last note under them.
fn foot_h(d: &Detail) -> f32 {
    1. + S3 + CTA_H + note_h(d)
}

/// Dim the pane and float a panel `h` points tall over the first `shown` points of the picture.
/// Returns the panel's top.
fn float_panel(canvas: &mut Canvas, h: f32, height: f32, shown: f32, t: &Tokens) -> f32 {
    let panel_w = CW - 16.;
    let y = ((shown - h) / 2.).clamp(M, (height - h - M).max(M));
    canvas.rect(0., 0., W, height, al(t.ground, 0.66));
    canvas.shadow(PANEL_X, y, panel_w, h, 24., t.ambient_shadow);
    canvas.rrect(PANEL_X, y, panel_w, h, 24., t.ground);
    canvas.glass(
        PANEL_X,
        y,
        panel_w,
        h,
        24.,
        lin(
            0.,
            y,
            0.,
            y + h,
            &[(t.linear_at[0], t.glass[0]), (t.linear_at[1], t.glass[1])],
        ),
    );
    canvas.rrect_line(PANEL_X, y, panel_w, h, 24., t.hair, t.hair_width);
    y
}

/// A footer key's hit box in layout points: the key, then left, right, top and bottom.
pub type KeyBox = (&'static str, f32, f32, f32, f32);

/// The popup's footer at `cursor`: its hairline, the action keys as pills and the last note.
/// Returns where each key sits, so a click on it can answer as the key.
fn popup_footer(
    canvas: &mut Canvas,
    f: &Fonts,
    d: &Detail,
    cursor: f32,
    t: &Tokens,
    hint: Option<&str>,
) -> Vec<KeyBox> {
    let (key_st, label_st) = (sans(POP_CTA_SIZE, t.bold), sans(POP_CTA_SIZE, t.medium));
    canvas.hair(TEXT_X, cursor, TEXT_W, t.hair);
    let top = cursor + 1. + S3;
    let base = top + (CTA_H + f.cap(key_st)) / 2.;
    let mut boxes = vec![];
    // The Resume key is the primary action: a filled pill in the gold accent.
    let primary = d.card == ObsCard::Handed || d.same_page;
    let mut pills_end = TEXT_X;
    for (i, (key, label, x, w, kw)) in cta_layout(f, d, TEXT_X, t).into_iter().enumerate() {
        let filled = primary && i == 0;
        if filled {
            canvas.rrect(x, top, w, CTA_H, CTA_H / 2., t.dream);
        } else {
            canvas.rrect(x, top, w, CTA_H, CTA_H / 2., t.chip);
            canvas.rrect_line(x, top, w, CTA_H, CTA_H / 2., t.hair, t.hair_width);
        }
        let ink = if filled { t.ground } else { t.ink };
        canvas.text(f, key, x + CTA_PAD, base, key_st, ink, A::L);
        canvas.text(
            f,
            label,
            x + CTA_PAD + kw + CTA_INNER,
            base,
            label_st,
            ink,
            A::L,
        );
        boxes.push((key, x, x + w, top, top + CTA_H));
        pills_end = x + w;
    }
    if let Some(hint) = hint {
        let st = sans(POP_CAPTION_SIZE, t.regular);
        // Only where it does not run into the keys.
        if TEXT_X + TEXT_W - f.width(hint, st) > pills_end + CTA_GAP {
            canvas.text(
                f,
                hint,
                TEXT_X + TEXT_W,
                top + (CTA_H + f.cap(st)) / 2.,
                st,
                t.quiet,
                A::R,
            );
        }
    }
    if let Some((note, ok)) = d.note {
        let st = sans(POP_CAPTION_SIZE, t.medium);
        let line = one_line(f, note, st, TEXT_W);
        canvas.text(
            f,
            &line,
            TEXT_X,
            top + CTA_H + S2 + (POP_CAPTION_LINE + f.cap(st)) / 2.,
            st,
            if ok { t.mint } else { t.warn },
            A::L,
        );
    }
    boxes
}

/// A line of the popup's page.
enum Row {
    /// The popup's name, in the title face.
    Title(String),
    /// A caption line; `right` is a mark drawn at its right edge.
    Caption {
        text: String,
        ink: Rgba,
        medium: bool,
        right: Option<&'static str>,
    },
    /// The item's own words, in the body size and a heavier weight.
    Lead(String),
    Body(String),
    /// A section's heading, in the caption size.
    Heading(String, Rgba),
    /// A hairline between sections.
    Rule,
    Gap(f32),
}

fn row_h(row: &Row) -> f32 {
    match row {
        Row::Title(_) => POP_TITLE_LINE,
        Row::Caption { .. } => POP_CAPTION_LINE,
        Row::Lead(_) | Row::Body(_) => POP_BODY_LINE,
        Row::Heading(..) => POP_CAPTION_LINE + 2.,
        Row::Rule => S3 + 1.,
        Row::Gap(h) => *h,
    }
}

/// A caption's baseline in a band `line` tall that starts at `top`: the cap height centred.
fn centred(f: &Fonts, st: St, top: f32, line: f32) -> f32 {
    top + (line + f.cap(st)) / 2.
}

fn draw_rows(canvas: &mut Canvas, f: &Fonts, rows: &[Row], mut cursor: f32, t: &Tokens) {
    let title = serif(POP_TITLE_SIZE, t.bold);
    let lead = sans(POP_BODY_SIZE, t.medium);
    let body = sans(POP_BODY_SIZE, t.regular);
    for row in rows {
        match row {
            Row::Title(s) => {
                canvas.text(
                    f,
                    s,
                    TEXT_X,
                    centred(f, title, cursor, POP_TITLE_LINE),
                    title,
                    t.ink,
                    A::L,
                );
            }
            Row::Caption {
                text,
                ink,
                medium,
                right,
            } => {
                let st = sans(POP_CAPTION_SIZE, if *medium { t.medium } else { t.regular });
                let base = centred(f, st, cursor, POP_CAPTION_LINE);
                canvas.text(f, text, TEXT_X, base, st, *ink, A::L);
                if let Some(mark) = right {
                    let st = sans(POP_CAPTION_SIZE, t.medium);
                    canvas.text(f, mark, TEXT_X + TEXT_W, base, st, t.mint, A::R);
                }
            }
            Row::Lead(s) => {
                canvas.text(
                    f,
                    s,
                    TEXT_X,
                    centred(f, lead, cursor, POP_BODY_LINE),
                    lead,
                    t.ink,
                    A::L,
                );
            }
            Row::Body(s) => {
                canvas.text(
                    f,
                    s,
                    TEXT_X,
                    centred(f, body, cursor, POP_BODY_LINE),
                    body,
                    t.ink,
                    A::L,
                );
            }
            Row::Heading(s, ink) => {
                let st = sans(POP_CAPTION_SIZE, t.medium);
                canvas.text(
                    f,
                    s,
                    TEXT_X,
                    centred(f, st, cursor, POP_CAPTION_LINE),
                    st,
                    *ink,
                    A::L,
                );
            }
            Row::Rule => canvas.hair(TEXT_X, cursor + S3 / 2., TEXT_W, t.hair),
            Row::Gap(_) => {}
        }
        cursor += row_h(row);
    }
}

/// Lay `build(step)` out for the popup, dropping the lowest-priority rows (a later `step` has
/// fewer) until the panel fits `room`; draw it with the footer at its foot. `centre_on` is how far
/// down the picture the panel is centred.
#[allow(clippy::too_many_arguments)]
fn popup(
    canvas: &mut Canvas,
    f: &Fonts,
    d: &Detail,
    height: f32,
    centre_on: f32,
    steps: usize,
    t: &Tokens,
    build: &dyn Fn(usize) -> Vec<Row>,
) -> Vec<KeyBox> {
    let room = height - 2. * M;
    let foot = foot_h(d);
    let h_of = |rows: &[Row]| P + rows.iter().map(row_h).sum::<f32>() + S3 + foot + P;
    let mut rows = build(0);
    for step in 1..steps {
        if h_of(&rows) <= room {
            break;
        }
        rows = build(step);
    }
    let h = h_of(&rows).min(room);
    let y = float_panel(canvas, h, height, centre_on, t);
    draw_rows(canvas, f, &rows, y + P, t);
    popup_footer(canvas, f, d, y + h - P - foot, t, None)
}

/// A shortened row, in full: its name, then every word of it. This popup exists to show the whole
/// row, so its words wrap to the panel's width instead of ending in "…"; everything else on it is
/// one line.
fn overlay_more(
    canvas: &mut Canvas,
    f: &Fonts,
    d: &Detail,
    height: f32,
    shown: f32,
    t: &Tokens,
) -> Vec<KeyBox> {
    let body = sans(POP_BODY_SIZE, t.regular);
    let fixed = P + POP_TITLE_LINE + S2 + S3 + foot_h(d) + P;
    let cap = (((height - 2. * M - fixed) / POP_BODY_LINE) as usize).max(3);
    let wrapped = lines_of(f, &d.line, body, TEXT_W, usize::MAX);
    let build = |step: usize| {
        let take = cap.saturating_sub(step);
        let mut lines = wrapped.clone();
        if lines.len() > take.max(1) {
            lines = lines_of(f, &d.line, body, TEXT_W, take.max(1));
        }
        let mut rows = vec![Row::Title(one_line(
            f,
            &d.heading,
            serif(POP_TITLE_SIZE, t.bold),
            TEXT_W,
        ))];
        rows.push(Row::Gap(S2));
        rows.extend(lines.into_iter().map(Row::Body));
        rows.push(Row::Gap(S3));
        rows
    };
    popup(canvas, f, d, height, shown, 4, t, &build)
}

/// A Lost and found thread: what was left open, in sentences, when it went quiet, and what the
/// send key hands over. The session's numbers and where CSR keeps it belong to the draft, not here.
fn overlay_lost(
    canvas: &mut Canvas,
    f: &Fonts,
    d: &Detail,
    height: f32,
    t: &Tokens,
) -> Vec<KeyBox> {
    let Some(view) = d.lost.as_ref() else {
        return vec![];
    };
    let (body, caption) = (
        sans(POP_BODY_SIZE, t.medium),
        sans(POP_CAPTION_SIZE, t.regular),
    );
    let title = serif(POP_TITLE_SIZE, t.bold);
    // Rows kept per thread line, halved at each step when the popup would not fit.
    const STEPS: usize = 3;
    let build = |step: usize| {
        let lines_each = 4usize >> step.min(STEPS - 1);
        let mut rows = vec![
            Row::Title(one_line(f, d.card.title(), title, TEXT_W)),
            Row::Caption {
                text: format!("{} of {}", d.place.0, d.place.1),
                ink: t.dream,
                medium: true,
                right: d.copied.then_some("Copied"),
            },
            Row::Caption {
                text: one_line(f, &view.kicker, caption, TEXT_W),
                ink: t.quiet,
                medium: false,
                right: None,
            },
            Row::Gap(S2),
        ];
        for line in crate::companion::handoff::thread_lines(&view.title) {
            rows.extend(
                items(f, &line, body, TEXT_W, lines_each)
                    .into_iter()
                    .map(Row::Lead),
            );
            rows.push(Row::Gap(4.));
        }
        rows.push(Row::Gap(S2));
        rows.push(Row::Rule);
        rows.push(Row::Caption {
            text: one_line(f, crate::companion::handoff::HANDS_OVER, caption, TEXT_W),
            ink: t.quiet,
            medium: false,
            right: None,
        });
        rows
    };
    popup(canvas, f, d, height, height, STEPS, t, &build)
}

/// One line of the "What CSR told Claude" popup.
enum HRow {
    Rule,
    /// A section's heading, and what follows it, quieter.
    Heading(String, String),
    /// One drawn line: its words, how many characters from the start are bold (a label), the dot
    /// in front of a bullet's first line, the indent of a bullet's later lines, and whether it is
    /// the caption size.
    Line {
        text: String,
        bold: usize,
        dot: Option<Rgba>,
        indent: f32,
        caption: bool,
        first: bool,
    },
}
fn hrow_h(r: &HRow) -> f32 {
    match r {
        HRow::Rule => S3 + 1.,
        HRow::Heading(..) => POP_CAPTION_LINE + 4.,
        HRow::Line { caption: true, .. } => POP_CAPTION_LINE,
        HRow::Line { .. } => POP_BODY_LINE,
    }
}
/// Room a bullet's dot takes before its words, in layout pt, at the body and caption sizes.
const DOT_BODY: f32 = 20.;
const DOT_CAPTION: f32 = 16.;

/// The popup's scrolling rows, wrapped to the panel's text width.
fn handed_rows(f: &Fonts, v: &crate::companion::handed::View, t: &Tokens) -> Vec<HRow> {
    use crate::companion::handed::Look;
    let (body, caption) = (
        sans(POP_BODY_SIZE, t.regular),
        sans(POP_CAPTION_SIZE, t.regular),
    );
    let mut rows = vec![];
    for section in &v.sections {
        rows.push(HRow::Rule);
        rows.push(HRow::Heading(section.heading.clone(), section.meta.clone()));
        for r in &section.rows {
            let (st, small, indent, dot) = match section.look {
                Look::Lead | Look::Plain => (body, false, 0., None),
                Look::Gold => (body, false, DOT_BODY, Some(t.dream)),
                Look::Quiet => (caption, true, DOT_CAPTION, Some(t.faint)),
            };
            let (text, bold) = match &r.label {
                Some(l) => (format!("{l}: {}", r.text), l.chars().count() + 1),
                None => (r.text.clone(), 0),
            };
            // A bold label is a little wider than the face it is measured in.
            let width = TEXT_W - indent - if bold > 0 { 8. } else { 0. };
            for (i, line) in lines_of(f, &text, st, width, usize::MAX)
                .into_iter()
                .enumerate()
            {
                rows.push(HRow::Line {
                    text: line,
                    bold: if i == 0 { bold } else { 0 },
                    dot,
                    indent,
                    caption: small,
                    first: i == 0,
                });
            }
        }
    }
    rows
}

/// Where the popup's rows sit: the panel's height, the room for the scrolling rows, the first
/// row shown, how many rows fit from it, and the most the first row may be.
struct HandedPlan {
    h: f32,
    first: usize,
    shown: usize,
    max_scroll: usize,
}
const HANDED_HEAD_H: f32 = POP_TITLE_LINE + 2. * POP_CAPTION_LINE + S2;
fn handed_plan(rows: &[HRow], d: &Detail, height: f32, scroll: usize) -> HandedPlan {
    let room = height - 2. * M;
    let foot = foot_h(d);
    let chrome = P + HANDED_HEAD_H + S3 + foot + P;
    let total: f32 = rows.iter().map(hrow_h).sum();
    let h = (chrome + total).min(room);
    let view = h - chrome;
    // The most the first row may be: the last rows that fill the view end the list.
    let mut used = 0.;
    let mut max_scroll = rows.len();
    while max_scroll > 0 && used + hrow_h(&rows[max_scroll - 1]) <= view {
        used += hrow_h(&rows[max_scroll - 1]);
        max_scroll -= 1;
    }
    let first = scroll.min(max_scroll);
    let (mut used, mut shown) = (0., 0);
    for r in &rows[first..] {
        if used + hrow_h(r) > view {
            break;
        }
        used += hrow_h(r);
        shown += 1;
    }
    HandedPlan {
        h,
        first,
        shown,
        max_scroll,
    }
}

/// The most the "What CSR told Claude" popup can be scrolled at this pane height: the pane keeps
/// its scroll inside it.
pub fn handed_scroll_max(f: &Fonts, d: &Detail, height: f32, t: &Tokens) -> usize {
    d.handed.as_ref().map_or(0, |(v, _)| {
        handed_plan(&handed_rows(f, v, t), d, height, 0).max_scroll
    })
}

/// What CSR told Claude: a tall popup that fills the pane's height and scrolls. Its rows wrap (the
/// second popup after "In full" that does), because its job is to say every word CSR said. The
/// foot says how many lines are below when there are some. Returns where the keys sit and the
/// most the popup can scroll.
fn overlay_handed(
    canvas: &mut Canvas,
    f: &Fonts,
    d: &Detail,
    height: f32,
    t: &Tokens,
) -> (Vec<KeyBox>, usize) {
    let Some((view, scroll)) = d.handed.as_ref() else {
        return (vec![], 0);
    };
    let rows = handed_rows(f, view, t);
    let plan = handed_plan(&rows, d, height, *scroll);
    let y = float_panel(canvas, plan.h, height, height, t);
    let title = serif(POP_TITLE_SIZE, t.bold);
    let (cap_med, cap) = (
        sans(POP_CAPTION_SIZE, t.medium),
        sans(POP_CAPTION_SIZE, t.regular),
    );
    let (body, lead) = (sans(POP_BODY_SIZE, t.regular), sans(POP_BODY_SIZE, t.bold));
    let mut cursor = y + P;
    canvas.text(
        f,
        &one_line(f, crate::companion::handed::TITLE, title, TEXT_W),
        TEXT_X,
        centred(f, title, cursor, POP_TITLE_LINE),
        title,
        t.ink,
        A::L,
    );
    cursor += POP_TITLE_LINE;
    let kicker = one_line(f, &view.kicker, cap_med, TEXT_W);
    canvas.text(
        f,
        &kicker,
        TEXT_X,
        centred(f, cap_med, cursor, POP_CAPTION_LINE),
        cap_med,
        t.dream,
        A::L,
    );
    cursor += POP_CAPTION_LINE;
    let caption = one_line(f, view.caption, cap, TEXT_W);
    canvas.text(
        f,
        &caption,
        TEXT_X,
        centred(f, cap, cursor, POP_CAPTION_LINE),
        cap,
        t.quiet,
        A::L,
    );
    cursor += POP_CAPTION_LINE + S2;
    for r in &rows[plan.first..plan.first + plan.shown] {
        match r {
            HRow::Rule => canvas.hair(TEXT_X, cursor + S3 / 2., TEXT_W, t.hair),
            HRow::Heading(name, meta) => {
                let base = centred(f, cap_med, cursor + 2., POP_CAPTION_LINE);
                let w = canvas.text(f, name, TEXT_X, base, cap_med, t.dream, A::L);
                if !meta.is_empty() {
                    let meta = format!(" \u{b7} {meta}");
                    canvas.text(f, &meta, TEXT_X + w, base, cap, t.quiet, A::L);
                }
            }
            HRow::Line {
                text,
                bold,
                dot,
                indent,
                caption,
                first,
            } => {
                let st = if *caption { cap } else { body };
                let line_h = if *caption {
                    POP_CAPTION_LINE
                } else {
                    POP_BODY_LINE
                };
                let base = centred(f, st, cursor, line_h);
                if let (Some(ink), true) = (dot, *first) {
                    let r = if *caption { 2.5 } else { 3.25 };
                    canvas.circle(TEXT_X + r + 1., base - f.cap(st) / 2., r, *ink);
                }
                let x = TEXT_X + indent;
                if *bold > 0 {
                    let head: String = text.chars().take(*bold).collect();
                    let tail: String = text.chars().skip(*bold).collect();
                    let w = canvas.text(f, &head, x, base, lead, t.ink, A::L);
                    canvas.text(f, &tail, x + w, base, body, t.ink, A::L);
                } else {
                    canvas.text(f, text, x, base, st, t.ink, A::L);
                }
            }
        }
        cursor += hrow_h(r);
    }
    let below = rows[plan.first + plan.shown..]
        .iter()
        .filter(|r| matches!(r, HRow::Line { .. } | HRow::Heading(..)))
        .count();
    let hint = (below > 0).then(|| format!("j/k for {below} more"));
    let foot = foot_h(d);
    let keys = popup_footer(canvas, f, d, y + plan.h - P - foot, t, hint.as_deref());
    (keys, plan.max_scroll)
}

/// Dim the pane and float `d` over it, centred on the first `shown` points of the picture. Returns
/// where the footer keys sit.
pub fn overlay(
    canvas: &mut Canvas,
    f: &Fonts,
    d: &Detail,
    height: f32,
    shown: f32,
    t: &Tokens,
) -> Vec<KeyBox> {
    if d.card == ObsCard::More {
        return overlay_more(canvas, f, d, height, shown, t);
    }
    if d.handed.is_some() {
        return overlay_handed(canvas, f, d, height, t).0;
    }
    if d.lost.is_some() {
        return overlay_lost(canvas, f, d, height, t);
    }
    let (body, caption) = (
        sans(POP_BODY_SIZE, t.regular),
        sans(POP_CAPTION_SIZE, t.regular),
    );
    let lead = sans(POP_BODY_SIZE, t.medium);
    let title = serif(POP_TITLE_SIZE, t.bold);
    let kind_col = match d.card {
        ObsCard::Confusing => t.warn,
        ObsCard::ShouldBe | ObsCard::More | ObsCard::Lost | ObsCard::Handed => t.dream,
    };
    let place = if d.same_page {
        "The dreamer's reading, a model's opinion".to_owned()
    } else {
        format!(
            "{} of {}{}",
            d.place.0,
            d.place.1,
            d.kind.map(|k| format!(" · {k}")).unwrap_or_default()
        )
    };
    // (lead, evidence, what happened, next step) items kept at each step
    const STEPS: [(usize, usize, usize, usize); 5] = [
        (3, 2, 3, 2),
        (2, 1, 2, 2),
        (2, 1, 1, 1),
        (1, 0, 1, 1),
        (1, 0, 0, 1),
    ];
    // The "Same page?" popup is a page of misses, not a sentence.
    const SAME_PAGE_STEPS: [(usize, usize, usize, usize); 5] = [
        (5, 1, 12, 3),
        (5, 1, 8, 2),
        (3, 1, 5, 2),
        (3, 0, 3, 2),
        (2, 0, 2, 1),
    ];
    let (what_heading, next_heading) = if d.same_page {
        ("Where Claude missed", "How to go on")
    } else {
        ("What happened", "Next step for you")
    };
    let build = |step: usize| {
        let (lead_n, evidence_n, what_n, next_n) = if d.same_page {
            SAME_PAGE_STEPS[step.min(SAME_PAGE_STEPS.len() - 1)]
        } else {
            STEPS[step.min(STEPS.len() - 1)]
        };
        let heading = if d.same_page {
            d.heading.as_str()
        } else {
            d.card.title()
        };
        let mut rows = vec![
            Row::Title(one_line(f, heading, title, TEXT_W)),
            Row::Caption {
                text: place.clone(),
                ink: kind_col,
                medium: true,
                right: d.copied.then_some("Copied"),
            },
            Row::Gap(S2),
        ];
        rows.extend(
            items(f, &d.line, lead, TEXT_W, lead_n)
                .into_iter()
                .map(Row::Lead),
        );
        rows.push(Row::Gap(S2));
        rows.extend(
            items(
                f,
                &format!("Evidence · {}", d.evidence),
                caption,
                TEXT_W,
                evidence_n,
            )
            .into_iter()
            .map(|text| Row::Caption {
                text,
                ink: t.quiet,
                medium: false,
                right: None,
            }),
        );
        for (heading, text, n) in [
            (what_heading, &d.explain, what_n),
            (next_heading, &d.next, next_n),
        ] {
            if n == 0 {
                continue;
            }
            rows.push(Row::Gap(S3));
            rows.push(Row::Heading(heading.to_owned(), kind_col));
            if d.same_page {
                // A miss is told whole: its turn, the ask, and what Claude did.
                let mut wrapped: Vec<String> = text
                    .lines()
                    .flat_map(|line| f.wrap(line, body, TEXT_W))
                    .collect();
                wrapped.truncate(n);
                rows.extend(wrapped.into_iter().map(Row::Body));
            } else {
                rows.extend(items(f, text, body, TEXT_W, n).into_iter().map(Row::Body));
            }
        }
        rows
    };
    popup(canvas, f, d, height, shown, STEPS.len(), t, &build)
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;

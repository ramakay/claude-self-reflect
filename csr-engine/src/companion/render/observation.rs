//! The detail popup of an observation card ("Confused Claude", "Should be a skill / memory / tool").
//! A card row is one truncated line; the popup shows one item in full, what happened, and a next
//! step. The explanation and next step come from the dreamer when it wrote them (`explain`, `next`)
//! and are otherwise composed here from the item's kind, line and evidence. No model runs at
//! render time.
use super::instrument::{evidence_receipt, should_be_kind, CW, M, P};
use super::instrument_gfx::*;
use super::theme::{self, Tokens, S2, S3};
use crate::companion::string;
use serde_json::Value;

/// Which observation card an item belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObsCard {
    Confusing,
    ShouldBe,
    /// Any other row the pane drew with a trailing "…": the popup shows its words in full. Its
    /// items are `{title, line}` and come from the frame that drew them.
    More,
}
impl ObsCard {
    pub fn other(self) -> Self {
        match self {
            Self::Confusing => Self::ShouldBe,
            Self::ShouldBe => Self::Confusing,
            Self::More => Self::More,
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::Confusing => super::instrument::CONFUSED_TITLE,
            Self::ShouldBe => super::instrument::SHOULD_BE_TITLE,
            Self::More => "In full",
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
}

/// The key a dismissed item is kept under in the likes log: the session, the card and the line.
pub fn dismiss_key(sid: &str, card: &str, line: &str) -> String {
    // FNV-1a: the line is long and free text, the key must stay short and stable across runs.
    let hash = line.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("observation:{sid}:{card}:{hash:016x}")
}

/// One item, in full.
#[derive(Clone, Debug, PartialEq)]
pub struct Detail {
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
    if card == ObsCard::More {
        return String::new();
    }
    if let Some(said) = stated(item, "explain") {
        return said;
    }
    let cite = cited(item).unwrap_or_else(|| "its quoted evidence".into());
    match card {
        ObsCard::More => String::new(),
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
    if card == ObsCard::More {
        return String::new();
    }
    if let Some(said) = stated(item, "next") {
        return said;
    }
    match card {
        ObsCard::More => String::new(),
        ObsCard::Confusing => format!(
            "Re-read {} in the transcript, and if Claude is still off, tell it the right reading in your next message.",
            cited(item).unwrap_or_else(|| "the quoted evidence".into())
        ),
        ObsCard::ShouldBe => match should_be_kind(item) {
            "skill" => "Press c to copy a skill draft, then save it as .claude/skills/<name>/SKILL.md.",
            "memory" => "Press c to copy a memory draft, then ask Claude to remember it.",
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
    let (line, evidence) = (string(&item["line"]), string(&item["evidence"]));
    let quoted = |text: &str| {
        text.lines()
            .map(|l| format!("> {l}").trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let opener = "Here is a note offered by CSR (claude-self-reflect):";
    match card {
        ObsCard::More => String::new(),
        ObsCard::Confusing => format!(
            "{opener}\n\n{}\n\nYou may have gone off course here. Please say what you got wrong and correct course.",
            quoted(&format!("Confused Claude: {line}\nEvidence: {evidence}"))
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
    let copy = match at.card {
        ObsCard::Confusing => format!(
            "Confused Claude: {line}\nEvidence: {evidence}\nWhat happened: {explain}\nNext step: {next}\n"
        ),
        ObsCard::ShouldBe => draft(item, &explain),
        ObsCard::More => format!("{}\n{line}\n", string(&item["title"])),
    };
    let heading = match at.card {
        ObsCard::More => string(&item["title"]).to_owned(),
        card => card.title().to_owned(),
    };
    let send = send_text(at.card, item, &explain);
    Some(Detail {
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

/// Dim the pane and float a panel `h` points tall over the first `shown` points of the picture.
/// Returns the panel's top.
fn float_panel(canvas: &mut Canvas, h: f32, height: f32, shown: f32, t: &Tokens) -> f32 {
    let panel_x = M + 8.;
    let panel_w = CW - 16.;
    let y = ((shown - h) / 2.).clamp(M, (height - h - M).max(M));
    canvas.rect(0., 0., W, height, al(t.ground, 0.66));
    canvas.shadow(panel_x, y, panel_w, h, 24., t.ambient_shadow);
    canvas.rrect(panel_x, y, panel_w, h, 24., t.ground);
    canvas.glass(
        panel_x,
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
    canvas.rrect_line(panel_x, y, panel_w, h, 24., t.hair, t.hair_width);
    y
}

/// A footer key's hit box in layout points: the key, then left, right, top and bottom.
pub type KeyBox = (&'static str, f32, f32, f32, f32);

/// Room the one-line note under the footer keys takes, in layout points.
fn note_h(d: &Detail) -> f32 {
    if d.note.is_some() {
        S2 + theme::CAPTION_LINE
    } else {
        0.
    }
}

/// The popup's footer keys, the "Copied" mark and the last note, drawn at `cursor` under a
/// hairline. Returns where each key sits, so a click on it can answer as the key.
fn popup_footer(
    canvas: &mut Canvas,
    f: &Fonts,
    d: &Detail,
    text_x: f32,
    text_w: f32,
    cursor: f32,
    t: &Tokens,
) -> Vec<KeyBox> {
    let caption = sans(theme::CAPTION_SIZE, t.regular);
    canvas.hair(text_x, cursor, text_w, t.hair);
    let cursor = cursor + 1. + S3;
    let mut x = text_x;
    let mut boxes = vec![];
    for (key, label) in footer_keys(Some(d)) {
        let st = sans(theme::CAPTION_SIZE, t.medium);
        let w = canvas.text(f, key, x, cursor + f.cap(caption), st, t.ink, A::L);
        let w2 = canvas.text(
            f,
            label,
            x + w + 4.,
            cursor + f.cap(caption),
            caption,
            t.quiet,
            A::L,
        );
        boxes.push((
            key,
            x - S2,
            x + w + 4. + w2 + S2,
            cursor - S2,
            cursor + theme::CAPTION_LINE + S2,
        ));
        x += w + 4. + w2 + S3 * 2.;
    }
    if let Some((note, ok)) = d.note {
        canvas.text(
            f,
            note,
            text_x,
            cursor + theme::CAPTION_LINE + S2 + f.cap(caption),
            sans(theme::CAPTION_SIZE, t.medium),
            if ok { t.mint } else { t.warn },
            A::L,
        );
    }
    if d.copied {
        canvas.text(
            f,
            "Copied",
            text_x + text_w,
            cursor + f.cap(caption),
            sans(theme::CAPTION_SIZE, t.medium),
            t.mint,
            A::R,
        );
    }
    boxes
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

/// A shortened row, in full: its name, then every word of it.
fn overlay_more(
    canvas: &mut Canvas,
    f: &Fonts,
    d: &Detail,
    height: f32,
    shown: f32,
    t: &Tokens,
) -> Vec<KeyBox> {
    let panel_x = M + 8.;
    let text_x = panel_x + P;
    let text_w = CW - 16. - 2. * P;
    let body = sans(theme::BODY_SIZE, t.regular);
    let caption = sans(theme::CAPTION_SIZE, t.regular);
    let (body_line, cap_line) = (theme::BODY_LINE, theme::CAPTION_LINE);
    let cap = ((height - 2. * M - 3. * P - 2. * cap_line - 4. * S3) / body_line).max(3.) as usize;
    let mut lines = lines_of(f, &d.line, body, text_w, usize::MAX);
    if lines.len() > cap {
        lines = lines_of(f, &d.line, body, text_w, cap);
    }
    let h = P
        + cap_line
        + S2
        + lines.len() as f32 * body_line
        + S3
        + 1.
        + S3
        + cap_line
        + note_h(d)
        + P;
    let y = float_panel(canvas, h, height, shown, t);
    let mut cursor = y + P;
    canvas.text(
        f,
        &d.heading,
        text_x,
        cursor + f.cap(caption),
        sans(theme::CAPTION_SIZE, t.medium),
        t.dream,
        A::L,
    );
    cursor += cap_line + S2;
    for l in &lines {
        canvas.text(f, l, text_x, cursor + f.cap(body), body, t.ink, A::L);
        cursor += body_line;
    }
    cursor += S3;
    popup_footer(canvas, f, d, text_x, text_w, cursor, t)
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
    let panel_x = M + 8.;
    let panel_w = CW - 16.;
    let text_x = panel_x + P;
    let text_w = panel_w - 2. * P;
    let head = serif(19., t.bold);
    let body = sans(theme::BODY_SIZE, t.regular);
    let caption = sans(theme::CAPTION_SIZE, t.regular);
    let kind_col = match d.card {
        ObsCard::Confusing => t.warn,
        ObsCard::ShouldBe | ObsCard::More => t.dream,
    };
    let (head_line, body_line, cap_line) = (25., theme::BODY_LINE, theme::CAPTION_LINE);
    // Every word is shown; a cap only applies when the panel would not fit the picture at all.
    let build = |n: usize| {
        (
            lines_of(f, &d.line, head, text_w, n),
            lines_of(f, &format!("Evidence · {}", d.evidence), caption, text_w, n),
            lines_of(f, &d.explain, body, text_w, n),
            lines_of(f, &d.next, body, text_w, n),
        )
    };
    let section = |lines: &[String]| cap_line + 2. + lines.len() as f32 * body_line;
    let height_of =
        |(line, evidence, what, next): &(Vec<String>, Vec<String>, Vec<String>, Vec<String>)| {
            P + cap_line
                + S2
                + line.len() as f32 * head_line
                + S2
                + evidence.len() as f32 * cap_line
                + S3
                + section(what)
                + S3
                + section(next)
                + S3
                + 1.
                + S3
                + cap_line
                + note_h(d)
                + P
        };
    let mut shaped = build(usize::MAX);
    for n in [8_usize, 5, 3] {
        if height_of(&shaped) <= height - 2. * M {
            break;
        }
        shaped = build(n);
    }
    let h = height_of(&shaped);
    let (line, evidence, what, next) = shaped;
    let y = float_panel(canvas, h, height, shown, t);
    let mut cursor = y + P;
    let place = format!(
        "{} · {} of {}{}",
        d.heading,
        d.place.0,
        d.place.1,
        d.kind.map(|k| format!(" · {k}")).unwrap_or_default()
    );
    canvas.text(
        f,
        &place,
        text_x,
        cursor + f.cap(caption),
        sans(theme::CAPTION_SIZE, t.medium),
        kind_col,
        A::L,
    );
    cursor += cap_line + S2;
    for l in &line {
        canvas.text(f, l, text_x, cursor + f.cap(head) + 2., head, t.ink, A::L);
        cursor += head_line;
    }
    cursor += S2;
    for l in &evidence {
        canvas.text(
            f,
            l,
            text_x,
            cursor + f.cap(caption),
            caption,
            t.quiet,
            A::L,
        );
        cursor += cap_line;
    }
    cursor += S3;
    for (title, lines) in [("What happened", &what), ("Next step for you", &next)] {
        canvas.text(
            f,
            title,
            text_x,
            cursor + f.cap(caption),
            sans(theme::CAPTION_SIZE, t.medium),
            kind_col,
            A::L,
        );
        cursor += cap_line + 2.;
        for l in lines {
            canvas.text(f, l, text_x, cursor + f.cap(body), body, t.ink, A::L);
            cursor += body_line;
        }
        cursor += S3;
    }
    popup_footer(canvas, f, d, text_x, text_w, cursor, t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(card: ObsCard, index: usize) -> PopupAt {
        PopupAt {
            card,
            index,
            copied: false,
            note: None,
        }
    }

    #[test]
    fn a_word_wider_than_the_popup_breaks_between_letters_and_loses_nothing() {
        let f = Fonts::bundled();
        let st = sans(theme::BODY_SIZE, theme::REGULAR);
        let blob = format!(
            "Signature: {{\"completion_status\":\"{}\"}} done",
            "partial".repeat(30)
        );
        let lines = lines_of(&f, &blob, st, 300., usize::MAX);
        assert!(lines.len() > 3, "{lines:?}");
        assert!(lines.iter().all(|l| f.width(l, st) <= 300.), "{lines:?}");
        let joined: String = lines.concat();
        assert!(
            joined.replace(' ', "").contains(&"partial".repeat(30)),
            "{joined}"
        );
    }

    #[test]
    fn an_old_confusing_item_gets_a_composed_explanation_and_next_step() {
        let items = vec![
            json!({"line":"Repeated the rejected approach.","evidence":"Turns 12 and 15: 'keep it local'"}),
        ];
        let d = detail(&items, at(ObsCard::Confusing, 0)).unwrap();
        assert_eq!(d.line, "Repeated the rejected approach.");
        assert_eq!(d.place, (1, 1));
        assert_eq!(d.kind, None);
        assert!(d.explain.contains("misread you"), "{}", d.explain);
        assert!(d.explain.contains("turns 12, 15"), "{}", d.explain);
        assert!(d.next.starts_with("Re-read turns 12, 15"), "{}", d.next);
        // The copy carries the item and its explanation.
        for want in [
            "Confused Claude: Repeated the rejected approach.",
            "Evidence: Turns 12 and 15: 'keep it local'",
            "What happened: The dreamer flagged",
            "Next step: Re-read turns 12, 15",
        ] {
            assert!(d.copy.contains(want), "{}", d.copy);
        }
    }

    #[test]
    fn a_quote_without_turn_numbers_is_cited_as_its_quote() {
        let items =
            vec![json!({"line":"Misread the target.","evidence":"'the pane, not the page'"})];
        let d = detail(&items, at(ObsCard::Confusing, 0)).unwrap();
        assert!(d.explain.contains("its quoted evidence"), "{}", d.explain);
        assert!(
            d.next.starts_with("Re-read the quoted evidence"),
            "{}",
            d.next
        );
    }

    #[test]
    fn the_dreamers_own_explanation_and_next_step_win_over_the_composed_ones() {
        let items = vec![
            json!({"line":"L","evidence":"Turn 3","explain":"It misread you.","next":"Say it twice."}),
        ];
        let d = detail(&items, at(ObsCard::Confusing, 0)).unwrap();
        assert_eq!(
            (d.explain.as_str(), d.next.as_str()),
            ("It misread you.", "Say it twice.")
        );
        assert!(d.copy.contains("What happened: It misread you."));
        // A blank field is the same as a missing one.
        let items = vec![json!({"line":"L","evidence":"Turn 3","explain":"  ","next":""})];
        let d = detail(&items, at(ObsCard::Confusing, 0)).unwrap();
        assert!(d.explain.starts_with("The dreamer flagged"));
    }

    #[test]
    fn each_should_be_kind_has_its_own_next_step_and_a_draft_to_paste() {
        let item = |kind: &str| json!({"kind":kind,"line":"Run fmt before each commit.","evidence":"Turns 4 and 12: 'run fmt'"});
        for (kind, next, draft_start) in [
            (
                "skill",
                "SKILL.md",
                "---\nname: run-fmt-before-each-commit\n",
            ),
            (
                "memory",
                "remember it",
                "---\nname: run-fmt-before-each-commit\n",
            ),
            ("hook", "hook to your settings", "# Hook idea: Run fmt"),
            ("CLAUDE.md", "CLAUDE.md", "- Run fmt before each commit."),
            ("tool", "small script", "Tool idea: Run fmt"),
        ] {
            let items = vec![item(kind)];
            let d = detail(&items, at(ObsCard::ShouldBe, 0)).unwrap();
            assert!(d.next.contains(next), "{kind}: {}", d.next);
            assert!(d.copy.starts_with(draft_start), "{kind}: {}", d.copy);
            assert!(
                d.copy.contains("turns 4, 12") || d.copy.contains("Turns 4 and 12"),
                "{kind}: {}",
                d.copy
            );
            assert!(
                d.explain.contains("more than once"),
                "{kind}: {}",
                d.explain
            );
        }
        let items = vec![item("skill")];
        let d = detail(&items, at(ObsCard::ShouldBe, 0)).unwrap();
        assert_eq!(d.kind, Some("skill"));
        assert!(d.explain.contains("belongs in a skill"), "{}", d.explain);
        assert!(d.copy.contains("description: Run fmt before each commit."));
        assert!(d.copy.contains("Why: You asked for this more than once"));
    }

    #[test]
    fn a_missing_item_has_no_detail() {
        assert!(detail(&[], at(ObsCard::Confusing, 0)).is_none());
        let items = vec![json!({"line":"L","evidence":"Turn 1"})];
        assert!(detail(&items, at(ObsCard::Confusing, 1)).is_none());
    }

    #[test]
    fn the_text_rows_carry_every_part_and_the_copied_mark() {
        let items =
            vec![json!({"kind":"hook","line":"Check formatting.","evidence":"Turn 4: 'fmt'"})];
        let mut d = detail(&items, at(ObsCard::ShouldBe, 0)).unwrap();
        let rows = rows(&d, 60);
        assert_eq!(rows[0], "Should be a skill / memory / tool · 1 of 1 · hook");
        assert_eq!(rows[1], "Check formatting.");
        assert!(rows.iter().any(|r| r.starts_with("Evidence: Turn 4")));
        assert!(rows.iter().any(|r| r.starts_with("What happened: ")));
        assert!(rows.iter().any(|r| r.starts_with("Next step for you: ")));
        assert!(!rows.iter().any(|r| r == "Copied."));
        d.copied = true;
        assert_eq!(
            super::rows(&d, 60).last().map(String::as_str),
            Some("Copied.")
        );
        assert_eq!(footer_keys(Some(&d))[0], ("c", "copy draft"));
        assert_eq!(footer_keys(None)[0], ("c", "copy"));
    }

    #[test]
    fn the_text_popup_breaks_a_word_wider_than_the_pane_and_loses_nothing() {
        let blob = format!("{{\"status\":\"{}\"}}", "partial".repeat(12));
        let items = vec![json!({"line": format!("Story {blob} end"), "evidence": ""})];
        let d = detail(&items, at(ObsCard::Confusing, 0)).unwrap();
        let rows = rows(&d, 30);
        assert!(
            rows.iter()
                .all(|r| crate::companion::render::text::len(r) <= 30),
            "{rows:?}"
        );
        assert!(rows.concat().contains(&"partial".repeat(12)), "{rows:?}");
    }

    #[test]
    fn a_dismissed_item_is_kept_under_its_session_card_and_line() {
        let key = dismiss_key("sess-1", "confusing_claude", "Misread the pane.");
        assert!(
            key.starts_with("observation:sess-1:confusing_claude:"),
            "{key}"
        );
        assert_eq!(key.len(), "observation:sess-1:confusing_claude:".len() + 16);
        assert_eq!(
            key,
            dismiss_key("sess-1", "confusing_claude", "Misread the pane.")
        );
        assert_ne!(
            key,
            dismiss_key("sess-2", "confusing_claude", "Misread the pane.")
        );
        assert_ne!(key, dismiss_key("sess-1", "should_be", "Misread the pane."));
        assert_ne!(
            key,
            dismiss_key("sess-1", "confusing_claude", "Misread the page.")
        );
    }

    #[test]
    fn what_s_sends_quotes_csrs_words_and_ends_with_yours() {
        let items = vec![json!({"line":"Said it passed.","evidence":"Turn 3: 'it should pass'"})];
        let d = detail(&items, at(ObsCard::Confusing, 0)).unwrap();
        assert_eq!(
            d.send,
            "Here is a note offered by CSR (claude-self-reflect):\n\n> Confused Claude: Said it passed.\n> Evidence: Turn 3: 'it should pass'\n\nYou may have gone off course here. Please say what you got wrong and correct course."
        );
        let items = vec![json!({"kind":"CLAUDE.md","line":"Run fmt.","evidence":"Turn 4: 'fmt'"})];
        let d = detail(&items, at(ObsCard::ShouldBe, 0)).unwrap();
        assert!(d.send.starts_with("Here is a note offered by CSR (claude-self-reflect):\n\n> CLAUDE.md rule: Run fmt.\n"), "{}", d.send);
        assert!(
            d.send
                .contains("\n> Draft:\n> - Run fmt.\n>   <!-- seen: Turn 4: 'fmt' -->\n"),
            "{}",
            d.send
        );
        assert!(d.send.ends_with("\n\nPlease make this CLAUDE.md rule from the draft above, and show me what you wrote."), "{}", d.send);
        // The import reads the quote as CSR's and keeps only the ask as the person's words.
        assert_eq!(
            crate::extraction::provenance::strip_forwarded_note(&d.send).as_deref(),
            Some(
                "Please make this CLAUDE.md rule from the draft above, and show me what you wrote."
            )
        );
        // No quoted line is empty: a blank draft line is a bare `>`, which a quote keeps whole.
        assert!(d.send.lines().all(|l| l != "> "), "{}", d.send);
        // A shortened row is only read: nothing to send.
        let more = vec![json!({"title":"Where the work stands","line":"All of it."})];
        assert_eq!(detail(&more, at(ObsCard::More, 0)).unwrap().send, "");
    }

    #[test]
    fn the_footer_offers_send_and_dismiss_only_where_there_is_something_to_act_on() {
        let items = vec![json!({"line":"L","evidence":"Turn 1"})];
        let keys = |card| footer_keys(detail(&items, at(card, 0)).as_ref());
        assert_eq!(
            keys(ObsCard::Confusing),
            [
                ("c", "copy"),
                ("s", "send"),
                ("x", "dismiss"),
                ("tab", "next"),
                ("esc", "close")
            ]
        );
        assert_eq!(keys(ObsCard::ShouldBe)[0], ("c", "copy draft"));
        assert_eq!(keys(ObsCard::ShouldBe)[1], ("s", "send"));
        assert_eq!(keys(ObsCard::More), [("c", "copy"), ("esc", "close")]);
    }

    #[test]
    fn a_note_is_a_row_of_the_text_popup() {
        let items = vec![json!({"line":"L","evidence":"Turn 1"})];
        let mut a = at(ObsCard::Confusing, 0);
        a.note = Some(("No Claude pane to type into.", false));
        let d = detail(&items, a).unwrap();
        assert_eq!(
            rows(&d, 60).last().map(String::as_str),
            Some("No Claude pane to type into.")
        );
    }

    #[test]
    fn slugs_are_short_and_file_safe() {
        assert_eq!(
            slug("Run fmt — before EACH commit, please! ok"),
            "run-fmt-before-each-commit"
        );
        assert_eq!(slug("???"), "untitled");
    }
}

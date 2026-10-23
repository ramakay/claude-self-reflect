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

/// The item the popup shows: its card and place on it, and whether its text was just copied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PopupAt {
    pub card: ObsCard,
    pub index: usize,
    pub copied: bool,
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
    out
}

/// The keys the popup answers to, as the footer lists them.
pub fn footer_keys(d: Option<&Detail>) -> Vec<(&'static str, &'static str)> {
    let copy = match d.map(|d| d.card) {
        Some(ObsCard::ShouldBe) => "copy draft",
        _ => "copy",
    };
    if d.is_some_and(|d| d.card == ObsCard::More) {
        return vec![("c", copy), ("esc", "close")];
    }
    vec![("c", copy), ("tab", "next"), ("esc", "close")]
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

/// The popup's footer keys and the "Copied" mark, drawn at `cursor` under a hairline.
fn popup_footer(
    canvas: &mut Canvas,
    f: &Fonts,
    d: &Detail,
    text_x: f32,
    text_w: f32,
    cursor: f32,
    t: &Tokens,
) {
    let caption = sans(theme::CAPTION_SIZE, t.regular);
    canvas.hair(text_x, cursor, text_w, t.hair);
    let cursor = cursor + 1. + S3;
    let mut x = text_x;
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
        x += w + 4. + w2 + S3 * 2.;
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
fn overlay_more(canvas: &mut Canvas, f: &Fonts, d: &Detail, height: f32, shown: f32, t: &Tokens) {
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
    let h = P + cap_line + S2 + lines.len() as f32 * body_line + S3 + 1. + S3 + cap_line + P;
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
    popup_footer(canvas, f, d, text_x, text_w, cursor, t);
}

/// Dim the pane and float `d` over it, centred on the first `shown` points of the picture.
pub fn overlay(canvas: &mut Canvas, f: &Fonts, d: &Detail, height: f32, shown: f32, t: &Tokens) {
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
    popup_footer(canvas, f, d, text_x, text_w, cursor, t);
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
    fn slugs_are_short_and_file_safe() {
        assert_eq!(
            slug("Run fmt — before EACH commit, please! ok"),
            "run-fmt-before-each-commit"
        );
        assert_eq!(slug("???"), "untitled");
    }
}

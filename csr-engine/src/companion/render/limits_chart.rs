//! The Context card's limits as one column chart: every quota window the account has is a column
//! (Claude's 5 Hour and Weekly, each model's own weekly share, the Codex week, each agy group's
//! 5 Hour and Weekly), grouped by whose limit it is. A column fills from the bottom with the
//! percent used; its percent sits on top and its window under it, the group's name under that. A
//! model's part of the week is stacked into Claude's Weekly column in the model's own colour, the
//! colour of the model's column. Under the chart, one line of words per window that has something
//! to say (its pace, its reset, the models a group's limit covers), each led by whose it is.
//!
//! `height` and `draw` read the same rows, so the card's plan and its picture never disagree.
use super::*;
use crate::companion::contract::Limit;

/// The columns' height, full and when the card is lean: the chart of Claude's two windows and
/// the Codex week is no taller than the three bars it replaced.
const COL_H: f32 = 40.;
const COL_H_LEAN: f32 = 28.;
/// The widest a column gets, and the gaps between two columns of a group and two groups.
const COL_MAX_W: f32 = 46.;
const COL_GAP: f32 = 12.;
const GROUP_GAP_MIN: f32 = 14.;
const GROUP_GAP_MAX: f32 = 56.;

/// The row of group names is drawn only when a group other than Claude's has more than one
/// column (agy's): Claude's windows are the pane's own, and a one-column group is named under
/// its column.
fn group_row(ls: &[Limit]) -> bool {
    ls.iter()
        .any(|l| l.group != "Claude" && ls.iter().filter(|o| o.group == l.group).count() > 1)
}

/// The words under a column: its window, or a one-column group's name.
fn col_label<'a>(l: &'a Limit, ls: &[Limit]) -> &'a str {
    if ls.iter().filter(|o| o.group == l.group).count() == 1 && !l.group.is_empty() {
        &l.group
    } else {
        &l.label
    }
}

fn col_h(lean: bool) -> f32 {
    if lean {
        COL_H_LEAN
    } else {
        COL_H
    }
}

/// A window's line is drawn when it has words, and on a lean card only when it warns.
fn shows_line(l: &Limit, lean: bool) -> bool {
    !l.line.is_empty() && !(lean && l.sev == "ok")
}

/// A model's own weekly window: a Claude column that is neither of the account's two windows.
fn is_model(l: &Limit) -> bool {
    l.group == "Claude" && !matches!(l.code.as_str(), "5H" | "7D")
}

/// The words under the chart for one window: led by whose it is, then for a week with a model's
/// part stacked in it each part named (the legend of its colours, kept before a cut), then the
/// window's own line.
fn words(l: &Limit) -> String {
    let led = [&l.group, &l.name, &l.label]
        .iter()
        .any(|who| !who.is_empty() && l.line.starts_with(who.as_str()));
    let parts: String = l
        .parts
        .iter()
        .map(|(name, n)| format!("{name} {} · ", n.round() as i64))
        .collect();
    if led || l.label.is_empty() {
        format!("{parts}{}", l.line)
    } else {
        format!("{} · {parts}{}", l.label, l.line)
    }
}

/// The chart's height: the percents, the columns, their labels, the group names and the lines.
pub(super) fn height(ls: &[Limit], lean: bool) -> f32 {
    if ls.is_empty() {
        return 0.;
    }
    let lines = ls.iter().filter(|l| shows_line(l, lean)).count() as f32;
    drow()
        + S1
        + col_h(lean)
        + S1
        + drow()
        + if group_row(ls) { drow() } else { 0. }
        + if lines > 0. { S1 + lines * drow() } else { 0. }
}

/// Each column's left edge, the columns' width, and each group's (name, left, right).
fn layout(ls: &[Limit]) -> (Vec<f32>, f32, Vec<(String, f32, f32)>) {
    let n = ls.len() as f32;
    let groups = 1 + ls.windows(2).filter(|w| w[0].group != w[1].group).count();
    let inner = (ls.len() - groups) as f32;
    let spaced = (groups - 1) as f32;
    let w = ((IW - inner * COL_GAP - spaced * GROUP_GAP_MIN) / n).min(COL_MAX_W);
    let group_gap = if groups > 1 {
        ((IW - n * w - inner * COL_GAP) / spaced).clamp(GROUP_GAP_MIN, GROUP_GAP_MAX)
    } else {
        0.
    };
    let mut xs = Vec::with_capacity(ls.len());
    let mut spans: Vec<(String, f32, f32)> = vec![];
    let mut x = IX;
    for (i, l) in ls.iter().enumerate() {
        if i > 0 {
            x += w + if ls[i - 1].group == l.group {
                COL_GAP
            } else {
                group_gap
            };
        }
        xs.push(x);
        match spans.last_mut() {
            Some(span) if span.0 == l.group => span.2 = x + w,
            _ => spans.push((l.group.clone(), x, x + w)),
        }
    }
    (xs, w, spans)
}

/// Draws the chart from `at` and records each column for the metrics; returns the y under it.
pub(super) fn draw(
    c: &mut Canvas,
    f: &Fonts,
    t: &Tokens,
    ls: &[Limit],
    lean: bool,
    at: f32,
    out: &mut Vec<DashLimit>,
) -> f32 {
    if ls.is_empty() {
        return at;
    }
    let pct_st = sans(theme::row_caption_size(), t.bold);
    let label_st = sans(theme::row_caption_size(), t.regular);
    let group_st = sans(theme::row_caption_size(), t.medium);
    let h = col_h(lean);
    let top = at + drow() + S1;
    let (xs, w, spans) = layout(ls);
    for (l, &x) in ls.iter().zip(&xs) {
        let ink = dash_sev_ink(&l.sev, t);
        let mid = x + w / 2.;
        c.text(
            f,
            &format!("{}%", l.used.round() as i64),
            mid,
            at + f.cap(pct_st),
            pct_st,
            ink,
            A::M,
        );
        c.rrect(x, top, w, h, 3., t.track);
        let fill = (l.used / 100.).clamp(0., 1.) as f32;
        if l.parts.is_empty() {
            if l.used > 0. {
                let fh = (h * fill).max(3.);
                let col = if is_model(l) { t.asked } else { ink };
                c.rrect(x, top + h - fh, w, fh, 3., col);
            }
        } else {
            // A model's part at the bottom in its colour, the rest of the week above it.
            let mut y = top + h;
            for (i, (_, n)) in l.parts.iter().enumerate() {
                let ph = h * (n / 100.).clamp(0., 1.) as f32;
                if ph > 0. {
                    let col = if i + 1 < l.parts.len() { t.asked } else { ink };
                    c.rrect(x, y - ph, w, ph, 2., col);
                    y -= ph + 1.;
                }
            }
        }
        let tick = l.tick.map(|n| (n / 100.).clamp(0., 1.) as f32);
        if let Some(frac) = tick {
            let ty = (top + h * (1. - frac)).max(top + 1.);
            c.rect(x - 3., ty - 1., w + 6., 2., t.quiet);
        }
        let label = f.fit(col_label(l, ls), label_st, w + 2. * COL_GAP);
        c.text(
            f,
            &label,
            mid,
            top + h + S1 + f.cap(label_st),
            label_st,
            t.quiet,
            A::M,
        );
        out.push(DashLimit {
            code: l.code.clone(),
            name: l.name.clone(),
            bar: (x, top, w, h),
            fill,
            tick,
            line: l.line.clone(),
        });
    }
    let mut y = top + h + S1 + drow();
    if group_row(ls) {
        for (i, (name, left, right)) in spans.iter().enumerate() {
            // A one-column group is already named under its column.
            if ls.iter().filter(|l| &l.group == name).count() == 1 {
                continue;
            }
            // Centred under its columns, as wide as the halfway points to its neighbours allow.
            let mid = (left + right) / 2.;
            let lo = if i > 0 {
                (spans[i - 1].2 + left) / 2. + S1
            } else {
                IX
            };
            let hi = spans
                .get(i + 1)
                .map_or(IX + IW, |next| (right + next.1) / 2. - S1);
            let room = 2. * (mid - lo).min(hi - mid);
            let shown = f.fit(name, group_st, room);
            c.text(f, &shown, mid, y + f.cap(group_st), group_st, t.ink, A::M);
        }
        y += drow();
    }
    let shown: Vec<&Limit> = ls.iter().filter(|l| shows_line(l, lean)).collect();
    if !shown.is_empty() {
        y += S1;
    }
    for l in shown {
        let ink = if l.sev == "red" { t.risk } else { t.quiet };
        let line = words(l);
        single(
            c,
            f,
            &line,
            IX,
            y + f.cap(label_st),
            IW,
            theme::row_caption_size(),
            ink,
            t,
        );
        y += drow();
    }
    y
}

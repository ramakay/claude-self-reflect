//! What the pane draws around a page (pane.py's `Pane.frame`): the value row above the legend,
//! the gauge strip beside a column, column mode's page and the reflow of the rest,
//! and the countdown to the next read.
//!
//! pane.py -> here: `value_codes`, `value_rows`; `strip_tapes`, `strip_width`, `fill_levels` (in
//! `render::text`), `level_row` and `bug_cell` (in `info`), `ctx_tape`, `quota_tape` and
//! `cache_tape` (the last two in `info`), `gauge_strip`, `strip_rows`, `cell_text` (in `info`);
//! `stack_rows`, `column_line`, `draw_column_main`, `TAP_FORMS`,
//! `column_reflow`; `reload_in`.
use super::{
    array,
    info::{self, cache_tape, cell_text, quota_tape, Cell, Paint, NIGHT},
    masthead, num,
    pages::Lines,
    pyfmt::short,
    render::text::*,
    string, value,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// The masthead's first rows: a click anywhere on them opens the i page.
const MASTHEAD_CLICK: usize = 2;
/// The strip's tapes, left to right; a narrow pane drops them from the right.
const STRIP: [&str; 4] = ["ctx", "7d", "5h", "cache"];
/// Each tape's one-character header, on the top row.
const STRIP_HEAD: [&str; 4] = ["C", "7", "5", "◷"];
/// The fewest rows that hold a strip.
const STRIP_ROWS: usize = 6;
/// Column mode keeps at least this many columns of text left of the strip.
const COLUMN_TEXT: usize = 5;
/// Seconds between two reads of csr-engine and the logs; the legend counts down to the next.
pub const RELOAD: f64 = 60.;
/// What a phone's column says under its counts, longest that fits.
const TAP_FORMS: [&str; 3] = ["tap to open", "tap: open", "tap"];
/// The name a section goes by in column mode ('Lost 18' in 12 columns).
const COLUMN_LABEL: [(&str, &str); 12] = [
    ("story", "Story"),
    ("dreams", "If…"),
    ("lost", "Lost"),
    ("remembered", "Hand"),
    ("coach", "Read"),
    ("rules", "Rule"),
    ("told", "Told"),
    ("last", "Last"),
    ("todos", "Todo"),
    ("touching", "File"),
    ("changed", "Diff"),
    ("compacted", "Cmp"),
];

/// Whole seconds until the next reload, as the legend says them: never under 0.
pub fn reload_in(loaded: f64, now: f64) -> u64 {
    (RELOAD - (now - loaded)).ceil().max(0.) as u64
}

/// The value row's words and their figures, in order, each left out at zero (Fuel when it cannot
/// be said): Fuel what CSR cost, Haiku its Haiku calls, Dreams dreamer calls that ran a model,
/// Took dreams and lost threads you took, ♥ rows you liked.
pub fn value_codes(v: &Value) -> Vec<(&'static str, String)> {
    let count = |key: &str| num(&v[key]) as i64;
    let took = count("dreams") + count("threads");
    [
        ("Fuel", value::fuel_text(v)),
        ("Haiku", count("calls").to_string()),
        ("Dreams", count("looks").to_string()),
        ("Took", took.to_string()),
        ("♥", count("likes").to_string()),
    ]
    .into_iter()
    .filter(|(code, figure)| !figure.is_empty() && (*code == "Fuel" || figure != "0"))
    .collect()
}

/// The value row above the legend, one row of `width`: 'Fuel ≤0.1% · Haiku 16 · Dreams 8 · Took
/// 1 · ♥ 7', each word in the legend's pencil, its figure in the page's ink. Too narrow, they go
/// from the right (♥, Took, Dreams, then Haiku) and Fuel last, cut if it must. Empty when it says
/// nothing; the i page says it in words.
pub fn value_rows(v: &Value, width: usize) -> Vec<String> {
    let codes = value_codes(v);
    if codes.is_empty() {
        return vec![];
    }
    let said = |n: usize| {
        codes[..n]
            .iter()
            .map(|(code, figure)| format!("{code} {figure}"))
            .collect::<Vec<_>>()
            .join(" · ")
    };
    let room = width.saturating_sub(1);
    let Some(n) = (1..=codes.len()).rev().find(|n| len(&said(*n)) <= room) else {
        return vec![format!(" {}{}{OFF}", ink("faint"), fit(&said(1), room))];
    };
    let joined = codes[..n]
        .iter()
        .map(|(code, figure)| format!("{}{code} {}{figure}", ink("faint"), ink("you")))
        .collect::<Vec<_>>()
        .join(&format!("{} · ", ink("faint")));
    vec![format!(" {joined}{OFF}")]
}

/// How many of the strip's tapes the pane has room for: none beside a page, where the text takes
/// the width; in column mode, where the strip is the page, all four while COLUMN_TEXT columns of
/// text are left.
pub fn strip_tapes(cols: usize, rows: usize, column: bool) -> usize {
    if !column || rows < STRIP_ROWS {
        return 0;
    }
    STRIP.len().min(cols.saturating_sub(1 + COLUMN_TEXT))
}

/// The columns the strip takes at the right edge: a margin, then a column a tape.
pub fn strip_width(tapes: usize) -> usize {
    if tapes > 0 {
        1 + tapes
    } else {
        0
    }
}

/// The context's tape, top first. It fills bottom-up in eighths over 0..CTX_RED, each cell in the
/// heat of its own height, so the colour climbs with the context. The floor's bug wins its cell;
/// above the fill, CTX_AMBER and CTX_HOT tick the rail in their inks. Tokens unknown: no fill,
/// never a stale one.
pub(super) fn ctx_tape(tokens: Option<f64>, floor: Option<f64>, height: usize) -> Vec<Cell> {
    let red = masthead::ctx_red();
    let known = tokens.filter(|t| t.is_finite() && *t > 0.);
    let levels = fill_levels(known.map_or(0., |t| t / red), height);
    let mut ticks = BTreeMap::new();
    ticks.insert(
        info::level_row(Some(masthead::ctx_amber() / red), height),
        sev_rgb("amber"),
    );
    ticks.insert(
        info::level_row(Some(masthead::ctx_hot() / red), height),
        sev_rgb("red"),
    );
    let bug = floor
        .filter(|f| f.is_finite())
        .map(|f| info::level_row(Some(f / red), height));
    (0..height)
        .map(|row| {
            let paint = heat(((height - 1 - row) as f64 + 0.5) / height as f64 * red);
            let fill = levels[row];
            if Some(row) == bug {
                info::bug_cell(fill, paint)
            } else if fill > 0 {
                (EIGHTHS[fill], Paint::Rgb(paint), None)
            } else if let Some(tick) = ticks.get(&row) {
                ('─', Paint::Rgb(*tick), None)
            } else {
                (info::RAIL, Paint::Rail, None)
            }
        })
        .collect()
}

/// The strip's first `tapes` tapes as (kind, cells), each `height` cells, top first.
pub(super) fn gauge_strip(
    g: &Value,
    tapes: usize,
    height: usize,
) -> Vec<(&'static str, Vec<Cell>)> {
    if height == 0 {
        return vec![];
    }
    let quota = |code: &str| array(&g["quotas"]).iter().find(|q| q["code"] == code);
    STRIP[..tapes.min(STRIP.len())]
        .iter()
        .map(|kind| {
            let cells = match *kind {
                "ctx" => ctx_tape(
                    g["ctx"]["tokens"].as_f64(),
                    g["floor"]["tokens"].as_f64(),
                    height,
                ),
                "7d" => quota_tape(quota("7D"), height),
                "5h" => quota_tape(quota("5H"), height),
                _ => cache_tape(&g["cache"], height),
            };
            (*kind, cells)
        })
        .collect()
}

/// The strip as escapes per screen row, written at the pane's last columns by absolute position,
/// so no tint or wide glyph in a row can shift it: the margin, then each tape's header on the top
/// row and its cells below. A row beside the masthead takes that row's ground; below, the
/// masthead's last step runs on down the edge.
pub(super) fn strip_rows(
    columns: &[(&'static str, Vec<Cell>)],
    cols: usize,
    grounds: &[Rgb],
) -> Vec<String> {
    let Some((_, first)) = columns.first() else {
        return vec![];
    };
    let start = cols.saturating_sub(columns.len());
    (0..=first.len())
        .map(|row| {
            let ground = grounds.get(row).copied().unwrap_or(NIGHT);
            let body = if row == 0 {
                let heads: String = columns
                    .iter()
                    .map(|(kind, _)| STRIP_HEAD[STRIP.iter().position(|k| k == kind).unwrap()])
                    .collect();
                format!("{}{heads}", fg(sev_rgb("normal")))
            } else {
                columns
                    .iter()
                    .map(|(_, cells)| cell_text(&cells[row - 1], ground))
                    .collect()
            };
            format!("\x1b[{};{start}H{} {body}{OFF}", row + 1, bg(ground))
        })
        .collect()
}

/// Column mode's masthead, one value a row, each [(text, sev)] in the ink of its severity,
/// longest form first: CTX, the trend, the payback, each plan window, the cache, STS.
pub fn stack_rows(g: &Value) -> Vec<Vec<(String, String)>> {
    let ctx = &g["ctx"];
    let tokens = ctx["tokens"].as_f64();
    let value = tokens.map_or("--".to_owned(), short);
    let heat_sev = if ctx["sev"] == "normal" {
        "data".to_owned()
    } else {
        string(&ctx["sev"]).to_owned()
    };
    let mut rows = vec![vec![
        (
            format!("CTX {value}"),
            if tokens.is_none() {
                "normal".to_owned()
            } else {
                heat_sev.clone()
            },
        ),
        (value, heat_sev),
    ]];
    if let Some(trend) = g["trend"].as_f64().filter(|t| t.abs() >= 1000.) {
        rows.push(vec![(
            format!(
                "{}{}",
                if trend > 0. { "▲" } else { "▼" },
                short(trend.abs())
            ),
            "normal".into(),
        )]);
    }
    let compacting = g["compacting"].is_object();
    let (text, sev) = if compacting {
        let (text, sev) = masthead::compacting_slot(&g["compacting"], false, true);
        (text, sev.to_owned())
    } else {
        (
            string(&g["payback"]["text"]).to_owned(),
            string(&g["payback"]["sev"]).to_owned(),
        )
    };
    if !text.is_empty() {
        let forms: Vec<String> = if compacting {
            vec![text.clone(), text.replace(' ', "")]
        } else {
            let forms: Vec<String> = array(&g["payback"]["forms"])
                .iter()
                .map(|f| string(f).to_owned())
                .collect();
            if forms.is_empty() {
                vec![text.clone()]
            } else {
                forms
            }
        };
        rows.push(forms.into_iter().map(|f| (f, sev.clone())).collect());
    }
    for q in array(&g["quotas"]) {
        let text = string(&q["text"]);
        let pct = text.split(" ↻").next().unwrap_or("");
        let sev = string(&q["sev"]);
        rows.push(vec![
            (pct.to_owned(), sev.to_owned()),
            (pct.replace(' ', ""), sev.to_owned()),
            (
                pct.rsplit(' ').next().unwrap_or("").to_owned(),
                sev.to_owned(),
            ),
        ]);
    }
    let cache = &g["cache"];
    let said = string(&cache["text"]);
    let minutes: String = said.chars().skip(len("CACHE ")).collect();
    rows.push(vec![
        (format!("◷{minutes}"), string(&cache["sev"]).to_owned()),
        (
            format!("◷{}", minutes.split(' ').next().unwrap_or("")),
            string(&cache["sev"]).to_owned(),
        ),
    ]);
    if let Some(sev) = g["sts"]["sev"].as_str().filter(|s| !s.is_empty()) {
        rows.push(vec![("STS".into(), sev.into())]);
    }
    rows
}

/// The first of `forms` [(text, sev)] that fits `width`, else the last one cut. Column mode
/// spares no column for a margin: the strip's own keeps the text off the tapes.
pub fn column_line(forms: &[(String, String)], width: usize) -> String {
    let (text, sev) = forms
        .iter()
        .find(|(text, _)| len(text) <= width)
        .cloned()
        .unwrap_or_else(|| {
            let (text, sev) = forms.last().cloned().unwrap_or_default();
            (fit(&text, width.max(1)), sev)
        });
    slot(&text, &sev)
}

fn column_label(section: &Value) -> String {
    let name = string(&section["name"]);
    COLUMN_LABEL
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, label)| (*label).to_owned())
        .unwrap_or_else(|| {
            string(&section["title"])
                .split(' ')
                .next()
                .unwrap_or("")
                .to_owned()
        })
}

/// Column mode's main page, `width` columns left of the strip: the masthead's values one a row
/// on the dark ramp, each in the ink of its severity (`stack_rows`), then each section with
/// something in it as one count on its tint. The pages stay one key away; no sparkline, no icons.
/// Returns the lines and the masthead's grounds, for the strip beside it.
pub fn draw_column_main(page: &Value, width: usize, phone: bool) -> (Lines, Vec<Rgb>) {
    let stack = stack_rows(&page["head"]["gauges"]);
    let grounds = dark_steps(stack.len());
    let mut out = Lines::default();
    for (forms, ground) in stack.iter().zip(&grounds) {
        out.rows.push(on_dark(&column_line(forms, width), *ground));
    }
    for row in 0..MASTHEAD_CLICK.min(stack.len()) {
        out.acts.insert(row, json!({"key": "instrument:figure"}));
    }
    for section in array(&page["sections"]) {
        let items = array(&section["items"]);
        if items.is_empty() {
            continue;
        }
        let name = string(&section["name"]);
        let count = section["count"]
            .as_i64()
            .unwrap_or(items.len() as i64)
            .to_string();
        let label = column_label(section);
        let first: String = label.chars().take(1).collect();
        let forms = [
            format!("{label} {count}"),
            format!("{first} {count}"),
            format!("{first}{count}"),
        ];
        let text = forms
            .iter()
            .find(|t| len(t) <= width)
            .cloned()
            .unwrap_or_else(|| fit(&forms[2], width.max(1)));
        if out.rows.len() > stack.len() {
            out.rows.push(String::new()); // a strip of paper between counts
        }
        let bold = if truthy_star(section) { BOLD } else { "" };
        out.rows.push(tinted(
            &format!("{bold}{}{text}{OFF}", ink(section_ink(name))),
            &bg(tint(name, 0.10)),
        ));
    }
    if phone {
        // herdr keeps a phone's split, too narrow to read in: a tap fills the screen with the page
        let said = TAP_FORMS
            .iter()
            .find(|t| len(t) <= width)
            .map_or_else(|| fit(TAP_FORMS[2], width.max(1)), |t| (*t).to_owned());
        out.rows.push(String::new());
        out.rows.push(format!("{}{said}{OFF}", ink("faint")));
    }
    (out, grounds)
}
fn truthy_star(section: &Value) -> bool {
    match &section["star"] {
        Value::Bool(b) => *b,
        Value::Null => false,
        other => other.as_f64().is_some_and(|n| n != 0.),
    }
}
fn section_ink(name: &str) -> &'static str {
    section(name)
}

/// A page drawn at its narrowest, its rows' words rewrapped to `width` for column mode, each row
/// in its first ink; a row's click goes with every row it became. Pictures and tints drop.
pub fn column_reflow(lines: &Lines, width: usize) -> Lines {
    static FG: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\x1b\[38;[0-9;]*m").unwrap());
    static SGR: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\x1b\[[0-9;:]*m|\x1b\[K").unwrap());
    let mut out = Lines::default();
    for (i, line) in lines.rows.iter().enumerate() {
        let stripped = SGR.replace_all(line, "");
        let text: String = stripped.chars().filter(|c| !control(*c)).collect();
        let text = text.trim();
        let colour = FG.find(line).map_or("", |m| m.as_str());
        let pieces = if text.is_empty() {
            vec![String::new()]
        } else {
            wrap(text, width.max(1), 99)
        };
        for piece in pieces {
            if let Some(act) = lines.acts.get(&i) {
                out.acts.insert(out.rows.len(), act.clone());
            }
            out.rows.push(if piece.is_empty() {
                String::new()
            } else {
                format!("{colour}{piece}{OFF}")
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Text with every escape gone, cursor moves and erases too.
    fn bare(text: &str) -> String {
        regex::Regex::new(r"\x1b\[[0-9;]*[A-Za-z]")
            .unwrap()
            .replace_all(text, "")
            .into_owned()
    }

    fn gauge(tokens: Option<f64>) -> Value {
        json!({
            "ctx": {"tokens": tokens, "size": 1000000., "sev": "normal"},
            "floor": {"tokens": 60000., "measured": true},
            "trend": null,
            "payback": {"text": "", "sev": "normal", "forms": []},
            "cache": {"text": "CACHE 4m", "sev": "normal", "left": 240., "ttl": 300., "cold": false},
            "quotas": [
                {"code": "7D", "text": "7D 62% ↻Sun 07:00", "sev": "normal", "used": 62., "pace": 0.5},
                {"code": "5H", "text": "5H 41% ↻1h", "sev": "advisory", "used": 41., "pace": null},
            ],
            "sts": {"sev": "amber", "inop": []},
            "compacting": null,
        })
    }

    #[test]
    fn test_strip_tapes_by_width_and_rows() {
        assert_eq!(strip_tapes(23, 30, true), 4);
        assert_eq!(strip_tapes(10, 30, true), 4);
        assert_eq!(strip_tapes(7, 30, true), 1);
        assert_eq!(strip_tapes(6, 30, true), 0);
        assert_eq!(strip_tapes(23, 5, true), 0);
        assert_eq!(strip_tapes(100, 40, false), 0);
        assert_eq!(strip_width(0), 0);
        assert_eq!(strip_width(4), 5);
    }

    #[test]
    fn test_reload_in_counts_whole_seconds_and_never_goes_under_zero() {
        assert_eq!(reload_in(100., 100.), 60);
        assert_eq!(reload_in(100., 100.2), 60);
        assert_eq!(reload_in(100., 101.), 59);
        assert_eq!(reload_in(100., 159.5), 1);
        assert_eq!(reload_in(100., 160.), 0);
        assert_eq!(reload_in(100., 900.), 0);
    }

    #[test]
    fn test_value_row_drops_from_the_right_and_fuel_goes_last() {
        let v = json!({
            "mode": "plan", "calls": 16, "looks": 8, "dreams": 1, "threads": 0, "likes": 7,
            "figure": {"share": 0.1, "bound": true},
        });
        let all = plain(&value_rows(&v, 79)[0]);
        assert_eq!(all, " Fuel ≤0.1% · Haiku 16 · Dreams 8 · Took 1 · ♥ 7");
        assert_eq!(
            plain(&value_rows(&v, 40)[0]),
            " Fuel ≤0.1% · Haiku 16 · Dreams 8"
        );
        assert_eq!(plain(&value_rows(&v, 12)[0]), " Fuel ≤0.1%");
        assert_eq!(plain(&value_rows(&v, 8)[0]), " Fuel ≤…");
        assert!(value_rows(&json!({"mode": "plan"}), 79).is_empty());
        assert!(value_rows(&Value::Null, 79).is_empty());
    }

    #[test]
    fn test_ctx_tape_fills_from_the_bottom_with_the_floor_bug_and_ticks() {
        let cells = ctx_tape(Some(200_000.), Some(60_000.), 8);
        assert_eq!(cells.len(), 8);
        assert_eq!(cells[7].0, '█');
        assert_eq!(cells[0].0, info::RAIL);
        let empty = ctx_tape(None, None, 8);
        assert!(empty.iter().all(|c| c.0 == info::RAIL || c.0 == '─'));
        assert!(empty.iter().filter(|c| c.0 == '─').count() >= 1);
    }

    #[test]
    fn test_stack_rows_longest_form_first_and_sts_last() {
        let rows = stack_rows(&gauge(Some(78_000.)));
        assert_eq!(rows[0][0].0, "CTX 78k");
        assert_eq!(rows[0][1].0, "78k");
        assert_eq!(rows[1][0].0, "7D 62%");
        assert_eq!(rows[1][1].0, "7D62%");
        assert_eq!(rows[1][2].0, "62%");
        assert_eq!(rows.last().unwrap()[0].0, "STS");
        let unknown = stack_rows(&gauge(None));
        assert_eq!(unknown[0][0].0, "CTX --");
        assert_eq!(unknown[0][0].1, "normal");
    }

    #[test]
    fn test_column_line_takes_the_first_form_that_fits_or_cuts_the_last() {
        let forms = vec![
            ("CTX 78k".to_owned(), "data".to_owned()),
            ("78k".to_owned(), "data".to_owned()),
        ];
        assert_eq!(plain(&column_line(&forms, 18)), "CTX 78k");
        assert_eq!(plain(&column_line(&forms, 5)), "78k");
        assert_eq!(plain(&column_line(&forms, 2)), "7…");
    }

    #[test]
    fn test_strip_rows_sit_at_the_edge_by_absolute_position() {
        let columns = gauge_strip(&gauge(Some(78_000.)), 4, 6);
        let rows = strip_rows(&columns, 23, &dark_steps(2));
        assert_eq!(rows.len(), 7);
        assert!(rows[0].starts_with("\x1b[1;19H"));
        assert!(rows[6].starts_with("\x1b[7;19H"));
        assert_eq!(bare(&rows[0]).trim(), "C75◷");
        assert!(rows[1].contains(&bg(dark_steps(2)[1])));
        assert!(rows[2].contains(&bg(NIGHT)));
        assert!(strip_rows(&[], 23, &[]).is_empty());
    }

    #[test]
    fn test_column_reflow_wraps_words_and_carries_the_click_to_every_row() {
        let mut lines = Lines::default();
        lines
            .rows
            .push(format!("{}  one two three four  {OFF}", ink("you")));
        lines.rows.push(String::new());
        lines.acts.insert(0, json!({"key": "k"}));
        let out = column_reflow(&lines, 8);
        assert_eq!(out.rows.len(), 4);
        assert_eq!(plain(&out.rows[0]), "one two");
        assert_eq!(plain(&out.rows[1]), "three");
        assert_eq!(plain(&out.rows[2]), "four");
        assert_eq!(out.rows[3], "");
        assert!(out.acts.contains_key(&0) && out.acts.contains_key(&2));
        assert!(!out.acts.contains_key(&3));
    }

    #[test]
    fn test_column_main_counts_each_section_with_items_and_a_phone_says_tap() {
        let page = json!({
            "head": {"gauges": gauge(Some(78_000.))},
            "sections": [
                {"name": "story", "title": "Story", "items": [1, 2, 3]},
                {"name": "rules", "title": "Your rules", "items": []},
                {"name": "dreams", "title": "What if…", "items": [1], "count": 7, "star": true},
            ],
        });
        let (lines, grounds) = draw_column_main(&page, 12, true);
        let seen: Vec<String> = lines.rows.iter().map(|r| bare(r)).collect();
        assert_eq!(grounds.len(), 5);
        assert_eq!(seen[5], "Story 3");
        assert_eq!(seen[6], "");
        assert_eq!(seen[7], "If… 7");
        assert_eq!(seen[8], "");
        assert_eq!(seen[9], "tap to open");
        assert_eq!(lines.acts.len(), 2);
        let (narrow, _) = draw_column_main(&page, 5, false);
        assert_eq!(bare(&narrow.rows[5]), "S 3");
        assert_eq!(bare(&narrow.rows[7]), "If… 7");
    }
}

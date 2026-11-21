//! Shared tape primitives used by the companion figure.
use super::{num, render::text::*, string};
use serde_json::Value;
pub(super) const CACHE_LOW: f64 = 600.;
pub(super) const RAIL: char = '│';
pub(super) const WARM: Rgb = [36, 163, 114];
pub(super) const OCHRE: Rgb = [242, 182, 50];
pub(super) const INDIGO: Rgb = [58, 127, 224];
pub(super) const VERMILION: Rgb = [232, 88, 44];
pub(super) const CHALK: Rgb = [238, 245, 250];
pub(super) const NIGHT: Rgb = [14, 15, 17];

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Paint {
    Rail,
    Rgb(Rgb),
}
pub(super) type Cell = (char, Paint, Option<Rgb>);

pub(super) fn level_row(share: Option<f64>, height: usize) -> usize {
    let share = share.filter(|n| n.is_finite()).unwrap_or(0.);
    height - 1 - (height - 1).min((share.clamp(0., 1.) * height as f64) as usize)
}
pub(super) fn bug_cell(fill: usize, paint: Rgb) -> Cell {
    ('━', Paint::Rgb(CHALK), (fill >= 4).then_some(paint))
}
pub(super) fn quota_tape(q: Option<&Value>, height: usize) -> Vec<Cell> {
    let Some(used) = q.and_then(|q| q["used"].as_f64()) else {
        return vec![(RAIL, Paint::Rail, None); height];
    };
    let q = q.unwrap();
    let paint = match string(&q["sev"]) {
        "amber" => OCHRE,
        "red" => VERMILION,
        _ => INDIGO,
    };
    let levels = fill_levels(used / 100., height);
    let bug = q["pace"].as_f64().map(|p| level_row(Some(p), height));
    levels
        .iter()
        .enumerate()
        .map(|(row, fill)| {
            if Some(row) == bug {
                bug_cell(*fill, paint)
            } else if *fill > 0 {
                (EIGHTHS[*fill], Paint::Rgb(paint), None)
            } else {
                (RAIL, Paint::Rail, None)
            }
        })
        .collect()
}
pub(super) fn cache_tape(cache: &Value, height: usize) -> Vec<Cell> {
    let (left, ttl) = (num(&cache["left"]), num(&cache["ttl"]));
    if cache["cold"] == true || left == 0. || ttl == 0. {
        return vec![(RAIL, Paint::Rail, None); height];
    }
    let paint = if left >= CACHE_LOW { WARM } else { OCHRE };
    fill_levels(left / ttl, height)
        .iter()
        .map(|fill| {
            if *fill > 0 {
                (EIGHTHS[*fill], Paint::Rgb(paint), None)
            } else {
                (RAIL, Paint::Rail, None)
            }
        })
        .collect()
}
pub(super) fn cell_text(cell: &Cell, ground: Rgb) -> String {
    let (glyph, paint, back) = cell;
    let ink = match paint {
        Paint::Rail => Some(blend(ground, CHALK, 0.24)),
        Paint::Rgb(c) => Some(*c),
    };
    format!(
        "{}{}{glyph}",
        bg(back.unwrap_or(ground)),
        ink.map_or(String::new(), fg)
    )
}

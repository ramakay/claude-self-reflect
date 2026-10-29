use super::{
    cards,
    raster::{self, Cell, ContextFacts, LastFacts, PaybackFacts, SessionFacts},
    text::Rgb,
};
use crate::companion::{array, num, paths::epoch};
use serde_json::Value;
use std::{
    collections::VecDeque,
    hash::{Hash, Hasher},
    sync::Arc,
};
use tiny_skia::Pixmap;

#[derive(Clone, Debug, PartialEq)]
pub enum Picture {
    Session(SessionFacts),
    Last(LastFacts),
    Payback(PaybackFacts),
    Context(ContextFacts),
    Iso(String),
    Card(cards::Card),
    Pill(cards::Pill),
    Emblem,
    Logo,
}
fn numbers(v: &Value) -> Vec<f64> {
    array(v)
        .iter()
        .filter_map(Value::as_f64)
        .filter(|v| v.is_finite())
        .collect()
}
fn pairs(v: &Value) -> Vec<(f64, f64)> {
    array(v)
        .iter()
        .filter_map(|r| Some((r[0].as_f64()?, r[1].as_f64()?)))
        .filter(|(a, b)| a.is_finite() && b.is_finite())
        .collect()
}
impl SessionFacts {
    pub fn from_value(s: &Value, now: f64) -> Self {
        Self {
            prompts: numbers(&s["prompts"]),
            replies: pairs(&s["replies"]),
            compacts: numbers(&s["compacts"]),
            chunks: numbers(&s["learned"]["chunks"]),
            recalls: numbers(&s["gave"]["recalls"]),
            carried: numbers(&s["gave"]["carried"]),
            dreams: numbers(&s["gave"]["dreams"]),
            asked: numbers(&s["asked"]),
            turns: array(&s["learned"]["turns"])
                .iter()
                .filter_map(|r| epoch(r[0].as_str()?))
                .collect(),
            now,
        }
    }
}
impl LastFacts {
    pub fn from_value(f: &Value) -> Self {
        Self {
            series: pairs(&f["series"]),
            prompts: numbers(&f["prompt_times"]),
            compactions: pairs(&f["compactions"]),
            floor: num(&f["floor"]),
        }
    }
}
impl PaybackFacts {
    pub fn from_value(f: &Value) -> Self {
        Self {
            cost: num(&f["cost"]),
            saving: num(&f["saving"]),
            need: num(&f["need"]),
            done: num(&f["done"]),
            net: num(&f["net"]),
            per: num(&f["per"]),
        }
    }
}
impl ContextFacts {
    pub fn from_value(f: &Value) -> Self {
        Self {
            series: pairs(&f["series"]),
            prompts: numbers(&f["prompts"]),
            compacts: numbers(&f["compacts"]),
            csr_at: numbers(&f["csr_at"]),
            floor: num(&f["floor"]),
            now: num(&f["now"]),
            band: (num(&f["band"][0]), num(&f["band"][1])),
        }
    }
}
impl Picture {
    pub fn from_item(p: &Value) -> Self {
        if p["kind"] == "payback" {
            Self::Payback(PaybackFacts::from_value(&p["facts"]))
        } else {
            Self::Last(LastFacts::from_value(&p["facts"]))
        }
    }
    pub fn draw(
        &self,
        cols: usize,
        rows: usize,
        cell: Cell,
        backs: &[Rgb],
        fonts: &raster::Fonts,
    ) -> Option<Pixmap> {
        let ground = backs
            .first()
            .copied()
            .unwrap_or(crate::companion::render::theme::SWATCH_4);
        let two = [ground, backs.get(1).copied().unwrap_or(ground)];
        match self {
            Self::Session(f) => raster::session(f, cols, cell, backs),
            Self::Last(f) => raster::last(f, cols, rows, cell, ground),
            Self::Payback(f) => raster::payback(f, cols, rows, cell, ground, fonts),
            Self::Context(f) => raster::context(f, cols, rows, cell, backs, fonts),
            Self::Iso(name) => raster::iso(name, cell, two),
            Self::Card(card) => cards::draw_card(card, cols, rows, cell, backs, cards::fonts()?),
            Self::Pill(pill) => cards::draw_pill(pill, cols, rows, cell, backs, cards::fonts()?),
            Self::Emblem => raster::emblem(cell, two),
            Self::Logo => raster::logo(cell, ground),
        }
    }
}
fn hash_numbers(h: &mut impl Hasher, values: &[f64]) {
    values.len().hash(h);
    for v in values {
        v.to_bits().hash(h);
    }
}
fn hash_pairs(h: &mut impl Hasher, values: &[(f64, f64)]) {
    values.len().hash(h);
    for (a, b) in values {
        a.to_bits().hash(h);
        b.to_bits().hash(h);
    }
}
impl Hash for Picture {
    fn hash<H: Hasher>(&self, h: &mut H) {
        std::mem::discriminant(self).hash(h);
        match self {
            Self::Session(f) => {
                for v in [
                    &f.prompts,
                    &f.compacts,
                    &f.chunks,
                    &f.recalls,
                    &f.carried,
                    &f.dreams,
                    &f.asked,
                    &f.turns,
                ] {
                    hash_numbers(h, v);
                }
                hash_pairs(h, &f.replies);
                f.now.to_bits().hash(h);
            }
            Self::Last(f) => {
                hash_pairs(h, &f.series);
                hash_pairs(h, &f.compactions);
                hash_numbers(h, &f.prompts);
                f.floor.to_bits().hash(h);
            }
            Self::Payback(f) => hash_numbers(h, &[f.cost, f.saving, f.need, f.done, f.net, f.per]),
            Self::Context(f) => {
                hash_pairs(h, &f.series);
                for v in [&f.prompts, &f.compacts, &f.csr_at] {
                    hash_numbers(h, v);
                }
                hash_numbers(h, &[f.floor, f.now, f.band.0, f.band.1]);
            }
            Self::Iso(name) => name.hash(h),
            Self::Card(card) => format!("{card:?}").hash(h),
            Self::Pill(pill) => format!("{pill:?}").hash(h),
            _ => (),
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub picture: Picture,
    pub cols: usize,
    pub rows: usize,
    pub cell: Cell,
    pub backs: Vec<Rgb>,
}
impl Hash for Request {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.picture.hash(h);
        self.cols.hash(h);
        self.rows.hash(h);
        self.cell.hash(h);
        self.backs.hash(h);
    }
}
type CacheEntry = (u64, Request, Option<Arc<super::kitty::Png>>);
pub struct Cache {
    fonts: &'static raster::Fonts,
    entries: VecDeque<CacheEntry>,
    bytes: usize,
}
impl Default for Cache {
    fn default() -> Self {
        static EMPTY: std::sync::LazyLock<raster::Fonts> =
            std::sync::LazyLock::new(raster::Fonts::default);
        Self::with_fonts(&EMPTY)
    }
}
impl Cache {
    pub fn with_fonts(fonts: &'static raster::Fonts) -> Self {
        Self {
            fonts,
            entries: VecDeque::new(),
            bytes: 0,
        }
    }
    pub fn get(&mut self, request: Request) -> Option<Arc<super::kitty::Png>> {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        request.hash(&mut hash);
        let hash = hash.finish();
        if let Some(i) = self
            .entries
            .iter()
            .position(|(h, r, _)| *h == hash && r == &request)
        {
            let entry = self.entries.remove(i)?;
            let png = entry.2.clone();
            self.entries.push_back(entry);
            return png;
        }
        let png: Option<Arc<super::kitty::Png>> = request
            .picture
            .draw(
                request.cols,
                request.rows,
                request.cell,
                &request.backs,
                self.fonts,
            )
            .and_then(|p| raster::png(&p).ok())
            .map(super::kitty::Png::from_bytes);
        self.bytes += png.as_ref().map_or(0, |p| p.len());
        self.entries.push_back((hash, request, png.clone()));
        while self.entries.len() > 64 || self.bytes > 8 * 1024 * 1024 {
            if let Some((_, _, p)) = self.entries.pop_front() {
                self.bytes -= p.as_ref().map_or(0, |p| p.len());
            }
        }
        png
    }
}
#[derive(Clone)]
pub struct Wanted {
    pub row: usize,
    pub col: usize,
    pub cols: usize,
    pub rows: usize,
    pub picture: Picture,
    pub backs: Vec<Rgb>,
}
impl Wanted {
    pub fn place(&self, cache: &mut Cache, cell: Cell) -> Option<super::kitty::Placement> {
        let png = cache.get(Request {
            picture: self.picture.clone(),
            cols: self.cols,
            rows: self.rows,
            cell,
            backs: self.backs.clone(),
        })?;
        Some(super::kitty::Placement {
            row: self.row,
            col: self.col,
            cols: self.cols,
            rows: self.rows,
            png,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pictures_are_cached_by_all_inputs_and_cell_size() {
        let mut cache = Cache::default();
        let req = Request {
            picture: Picture::Logo,
            cols: 4,
            rows: 1,
            cell: (16, 34),
            backs: vec![crate::companion::render::theme::SWATCH_1],
        };
        let first = cache.get(req.clone()).unwrap();
        assert!(Arc::ptr_eq(&first, &cache.get(req.clone()).unwrap()));
        let mut changed = req.clone();
        changed.cell = (20, 40);
        assert!(!Arc::ptr_eq(&first, &cache.get(changed).unwrap()));
        let mut changed = req;
        changed.backs[0] = crate::companion::render::theme::SWATCH_6;
        assert!(!Arc::ptr_eq(&first, &cache.get(changed).unwrap()));
    }
}

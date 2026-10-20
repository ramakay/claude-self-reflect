//! The cards: the pane's pixel-mode info view (a stack of rounded cards) and masthead pill.
//!
//! Every figure a card draws is measured: the builders at the bottom read the same gauges, session and
//! dream data the text rows read, and a card with nothing real to say is not built at all.
#![allow(clippy::too_many_arguments)]
use super::{text::Rgb, theme};
use crate::companion::{agents, array, dreams, masthead, num, pyfmt::short, string};
use ab_glyph::{point, Font, FontVec, GlyphId, ScaleFont, VariableFont};
use serde_json::Value;
use std::sync::LazyLock;
use tiny_skia::{FillRule, Paint, Path, PathBuilder, Pixmap, Rect, Transform};

/// One point is two device pixels: the pane's cell size is already measured at twice the screen's.
const S: f32 = 2.0;
/// The narrowest picture, in points, the cards are laid out for; narrower panes keep their text.
pub const MIN_WIDTH_PT: f32 = 260.;
const PAD: f32 = 12.;
const GAP: f32 = 8.;
const MARGIN: f32 = 4.;

type Rgba = [u8; 4];
fn px(c: Rgb) -> Rgba {
    [c[0], c[1], c[2], 255]
}
const MONO: f32 = 0.;
/// A card's section: the pane icon at its left, the section's ink and the wash it is faced with.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Look {
    icon: &'static str,
    section: &'static str,
    title: &'static str,
}
impl Look {
    fn ink(self) -> Rgb {
        super::text::ink_rgb(super::text::section(self.section))
    }
    fn face(self) -> Rgb {
        super::text::tint(self.section, 0.07)
    }
}

// ---------------------------------------------------------------- fonts

struct Face {
    font: FontVec,
    wght: f32,
    opsz: f32,
}
/// The system's UI font at the weights the cards use, and the symbol fonts behind it.
pub struct CardFonts {
    mono: Option<FontVec>,
    primary: Vec<Face>,
    fallback: Vec<FontVec>,
}
struct Placed<'a> {
    font: &'a FontVec,
    scale: f32,
    id: GlyphId,
    x: f32,
}
fn px_scale(f: &FontVec, size_px: f32) -> f32 {
    size_px * f.height_unscaled() / f.units_per_em().unwrap_or(1000.)
}
fn read_font(path: &str) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}
impl CardFonts {
    fn load() -> Option<Self> {
        let home = std::env::var("HOME").unwrap_or_default();
        let found = pick_family(|p| std::path::Path::new(p).is_file(), &home)?;
        let mut primary = vec![];
        if found.variable {
            let bytes = read_font(&found.regular)?;
            for (w, o) in [(400., 17.), (500., 17.), (600., 17.), (600., 28.)] {
                let mut font = FontVec::try_from_vec(bytes.clone()).ok()?;
                font.set_variation(b"wght", w);
                font.set_variation(b"opsz", o);
                primary.push(Face {
                    font,
                    wght: w,
                    opsz: o,
                });
            }
        } else {
            // fixed weights: the regular stands in for 400 and 500, the bold for 600
            let bold = found.bold.as_ref().unwrap_or(&found.regular);
            for (path, w) in [(&found.regular, 400.), (bold, 600.)] {
                let font = FontVec::try_from_vec(read_font(path)?).ok()?;
                primary.push(Face {
                    font,
                    wght: w,
                    opsz: 17.,
                });
            }
        }
        let fallback = [
            "/System/Library/Fonts/Apple Symbols.ttf",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ]
        .into_iter()
        .filter_map(|p| FontVec::try_from_vec(read_font(p)?).ok())
        .collect();
        let mono = [
            ("/System/Library/Fonts/Menlo.ttc", 0),
            ("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 0),
            ("/usr/share/fonts/dejavu/DejaVuSansMono.ttf", 0),
            (
                "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
                0,
            ),
        ]
        .into_iter()
        .find_map(|(p, i)| FontVec::try_from_vec_and_index(read_font(p)?, i).ok());
        (!primary.is_empty()).then_some(Self {
            mono,
            primary,
            fallback,
        })
    }
    fn face(&self, size: f32, wght: f32) -> &FontVec {
        if wght == MONO {
            if let Some(m) = &self.mono {
                return m;
            }
        }
        let wght = if wght == MONO { 400. } else { wght };
        let want_o = if size >= 20. { 28. } else { 17. };
        self.primary
            .iter()
            .min_by(|a, b| {
                let key = |f: &Face| ((f.wght - wght).abs(), (f.opsz - want_o).abs());
                key(a)
                    .partial_cmp(&key(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|f| &f.font)
            .unwrap_or(&self.primary[0].font)
    }
    fn place(
        &self,
        s: &str,
        size: f32,
        wght: f32,
        track: f32,
        mult: f32,
    ) -> (Vec<Placed<'_>>, f32) {
        let main = self.face(size, wght);
        let size_px = size * S * mult;
        let mut out = vec![];
        let mut x = 0.;
        let mut prev: Option<(*const FontVec, GlyphId)> = None;
        for ch in s.chars() {
            let mut font = main;
            let mut id = font.glyph_id(ch);
            if id.0 == 0 {
                if let Some((f, g)) = self
                    .fallback
                    .iter()
                    .map(|f| (f, f.glyph_id(ch)))
                    .find(|(_, g)| g.0 != 0)
                {
                    font = f;
                    id = g;
                }
            }
            let scale = px_scale(font, size_px);
            let sf = font.as_scaled(scale);
            if let Some((pf, pid)) = prev {
                if std::ptr::eq(pf, font) {
                    x += sf.kern(pid, id);
                }
            }
            out.push(Placed { font, scale, id, x });
            x += sf.h_advance(id) + track * S * mult;
            prev = Some((font as *const FontVec, id));
        }
        (out, x)
    }
    /// Advance width in points, trailing tracking included.
    pub fn width(&self, s: &str, size: f32, wght: f32, track: f32) -> f32 {
        self.place(s, size, wght, track, 1.).1 / S
    }
    /// Ink box (left, right, top, bottom) in points around the pen origin and baseline.
    fn ink(&self, s: &str, size: f32, wght: f32, track: f32) -> (f32, f32, f32, f32) {
        let (g, _) = self.place(s, size, wght, track, 16.);
        let (mut l, mut r, mut t, mut b) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for p in &g {
            let glyph = p.id.with_scale_and_position(p.scale, point(p.x, 0.));
            if let Some(o) = p.font.outline_glyph(glyph) {
                let bb = o.px_bounds();
                l = l.min(bb.min.x);
                r = r.max(bb.max.x);
                t = t.min(bb.min.y);
                b = b.max(bb.max.y);
            }
        }
        if l == f32::MAX {
            return (0., 0., 0., 0.);
        }
        (l / (S * 16.), r / (S * 16.), t / (S * 16.), b / (S * 16.))
    }
    fn cap(&self, size: f32, wght: f32) -> f32 {
        -self.ink("H", size, wght, 0.).2
    }
}
/// Where one family keeps its regular and bold files.
struct Family {
    variable: bool,
    regular: Vec<&'static str>,
    bold: Vec<&'static str>,
}
/// The family the cards draw in, and the files it was found at.
#[derive(Debug, PartialEq)]
struct Found {
    variable: bool,
    regular: String,
    bold: Option<String>,
}
/// The families in the order they are tried: the Mac system font, then the open fonts Linux
/// commonly has (Inter, Noto Sans, DejaVu Sans, Liberation Sans). Nothing is bundled.
fn families() -> Vec<Family> {
    vec![
        Family {
            variable: true,
            regular: vec!["/System/Library/Fonts/SFNS.ttf"],
            bold: vec![],
        },
        Family {
            variable: false,
            regular: vec![
                "/usr/share/fonts/truetype/inter/Inter-Regular.ttf",
                "/usr/share/fonts/opentype/inter/Inter-Regular.otf",
                "/usr/share/fonts/inter/Inter-Regular.ttf",
                "~/.local/share/fonts/Inter-Regular.ttf",
            ],
            bold: vec![
                "/usr/share/fonts/truetype/inter/Inter-Bold.ttf",
                "/usr/share/fonts/opentype/inter/Inter-Bold.otf",
                "/usr/share/fonts/inter/Inter-Bold.ttf",
                "~/.local/share/fonts/Inter-Bold.ttf",
            ],
        },
        Family {
            variable: false,
            regular: vec![
                "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/opentype/noto/NotoSans-Regular.ttf",
                "~/.local/share/fonts/NotoSans-Regular.ttf",
            ],
            bold: vec![
                "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
                "/usr/share/fonts/noto/NotoSans-Bold.ttf",
                "/usr/share/fonts/opentype/noto/NotoSans-Bold.ttf",
                "~/.local/share/fonts/NotoSans-Bold.ttf",
            ],
        },
        Family {
            variable: false,
            regular: vec![
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                "/usr/share/fonts/dejavu/DejaVuSans.ttf",
                "~/.local/share/fonts/DejaVuSans.ttf",
            ],
            bold: vec![
                "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
                "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
                "~/.local/share/fonts/DejaVuSans-Bold.ttf",
            ],
        },
        Family {
            variable: false,
            regular: vec![
                "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
                "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
                "~/.local/share/fonts/LiberationSans-Regular.ttf",
            ],
            bold: vec![
                "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
                "/usr/share/fonts/liberation/LiberationSans-Bold.ttf",
                "~/.local/share/fonts/LiberationSans-Bold.ttf",
            ],
        },
    ]
}
/// The first family, in order, with a regular file `exists` accepts; `~` is `home`.
fn pick_family(exists: impl Fn(&str) -> bool, home: &str) -> Option<Found> {
    let at = |paths: &[&str]| {
        paths
            .iter()
            .map(|p| p.replacen('~', home, 1))
            .find(|p| exists(p))
    };
    families().into_iter().find_map(|f| {
        Some(Found {
            variable: f.variable,
            regular: at(&f.regular)?,
            bold: at(&f.bold),
        })
    })
}
/// The fonts the cards need, read once; `None` when the system has none, and the pane keeps its text.
pub fn fonts() -> Option<&'static CardFonts> {
    static FONTS: LazyLock<Option<CardFonts>> = LazyLock::new(CardFonts::load);
    FONTS.as_ref()
}

// ---------------------------------------------------------------- canvas

struct Canvas {
    pm: Pixmap,
    dry: bool,
}
#[derive(Clone, Copy, PartialEq)]
enum Anchor {
    L,
    M,
    R,
}
fn paint(c: Rgba) -> Paint<'static> {
    let mut p = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    p.set_color_rgba8(c[0], c[1], c[2], c[3]);
    p
}
fn sc() -> Transform {
    Transform::from_scale(S, S)
}
fn rrect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let r = r.min(w / 2.).min(h / 2.);
    let k = 0.552_284_7 * r;
    let mut p = PathBuilder::new();
    p.move_to(x + r, y);
    p.line_to(x + w - r, y);
    p.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    p.line_to(x + w, y + h - r);
    p.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    p.line_to(x + r, y + h);
    p.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    p.line_to(x, y + r);
    p.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    p.close();
    p.finish()
}
impl Canvas {
    fn new(w_px: u32, h_px: u32) -> Option<Self> {
        // A corrupt terminal report must not allocate an unbounded image.
        if u64::from(w_px) * u64::from(h_px) > 16_777_216 {
            return None;
        }
        Some(Self {
            pm: Pixmap::new(w_px, h_px)?,
            dry: false,
        })
    }
    fn dry() -> Self {
        Self {
            pm: Pixmap::new(1, 1).expect("one pixel"),
            dry: true,
        }
    }
    fn rrect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, col: Rgba) {
        if self.dry {
            return;
        }
        if let Some(p) = rrect_path(x, y, w, h, r) {
            self.pm
                .fill_path(&p, &paint(col), FillRule::Winding, sc(), None);
        }
    }
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, col: Rgba) {
        if self.dry {
            return;
        }
        if let Some(r) = Rect::from_xywh(x * S, y * S, w * S, h * S) {
            self.pm
                .fill_rect(r, &paint(col), Transform::identity(), None);
        }
    }
    fn circle(&mut self, cx: f32, cy: f32, r: f32, col: Rgba) {
        if self.dry {
            return;
        }
        if let Some(p) = PathBuilder::from_circle(cx, cy, r) {
            self.pm
                .fill_path(&p, &paint(col), FillRule::Winding, sc(), None);
        }
    }
    /// Draw text with its baseline at `base`; returns the advance in points.
    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        f: &CardFonts,
        s: &str,
        x: f32,
        base: f32,
        size: f32,
        wght: f32,
        col: Rgba,
        track: f32,
        anchor: Anchor,
    ) -> f32 {
        let (glyphs, adv) = f.place(s, size, wght, track, 1.);
        if self.dry {
            return adv / S;
        }
        let ox = match anchor {
            Anchor::L => x * S,
            Anchor::R => x * S - (adv - track * S),
            Anchor::M => {
                let (l, r, _, _) = f.ink(s, size, wght, track);
                x * S - (l + r) / 2. * S
            }
        };
        let by = (base * S).round();
        let (w, h) = (self.pm.width() as i32, self.pm.height() as i32);
        let data = self.pm.data_mut();
        for p in &glyphs {
            let glyph = p.id.with_scale_and_position(p.scale, point(ox + p.x, by));
            if let Some(o) = p.font.outline_glyph(glyph) {
                let b = o.px_bounds();
                let (bx, byy) = (b.min.x.floor() as i32, b.min.y.floor() as i32);
                o.draw(|gx, gy, cov| {
                    let (px, py) = (bx + gx as i32, byy + gy as i32);
                    if px < 0 || py < 0 || px >= w || py >= h {
                        return;
                    }
                    let sa = cov.clamp(0., 1.) * (col[3] as f32 / 255.);
                    if sa <= 0. {
                        return;
                    }
                    let i = ((py * w + px) * 4) as usize;
                    // the pixmap holds premultiplied colour
                    let inv = 1. - sa;
                    for k in 0..3 {
                        data[i + k] = (col[k] as f32 * sa + data[i + k] as f32 * inv).round() as u8;
                    }
                    data[i + 3] = (255. * sa + data[i + 3] as f32 * inv).round() as u8;
                });
            }
        }
        (adv - track * S) / S
    }
}

// ---------------------------------------------------------------- what a card says

/// A legend dot's colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dot {
    Blue,
    Pink,
    Amber,
    Green,
    Track,
    Red,
}
impl Dot {
    fn rgb(self) -> Rgb {
        match self {
            Dot::Blue => theme::CTX,
            Dot::Pink | Dot::Red => theme::TURN,
            Dot::Amber => theme::DREAM,
            Dot::Green => theme::RECALL,
            Dot::Track => theme::FAINT,
        }
    }
}
/// One legend row: a dot, its label, its value.
pub type Legend = (Dot, String, String);
/// The moon: how much of the window the context fills, and what it says under it.
#[derive(Clone, Debug, PartialEq)]
pub struct MoonFact {
    pub share: f64,
    pub tokens: f64,
    pub caption: String,
}
/// A thin bar for a quota window: its share, its words, and where the pace stands.
#[derive(Clone, Debug, PartialEq)]
pub struct BarFact {
    pub name: String,
    pub share: f64,
    pub caption: String,
    pub pace: Option<f64>,
    pub tone: Dot,
}
/// How an agent's pip is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Slot {
    Running,
    Stalled,
    Done,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Card {
    /// The context as a moon, and the 5H and 7D windows as bars beside it.
    Moon {
        moon: Option<MoonFact>,
        bars: Vec<BarFact>,
    },
    /// Context per request as bars, cached under fresh when the transcript splits them.
    History {
        bars: Vec<(f64, f64)>,
        head: String,
        legend: Vec<Legend>,
    },
    Agents {
        slots: Vec<Slot>,
        legend: Vec<Legend>,
    },
    Cache {
        share: f64,
        cold: bool,
        legend: Vec<Legend>,
    },
    /// The open dream, how many asks are left, and what this session has.
    WhatIf {
        line: String,
        tally: String,
        count: usize,
        unread: usize,
    },
    /// The session's story as a note: its rights, the sentence where work stands, and the Shift
    /// Map's first shift with its receipt (text, loud).
    Story {
        rights: String,
        now: String,
        shifts: Vec<(String, bool)>,
    },
    /// What CSR handed Claude, up, and what it wrote down, down, over the session.
    Mirror {
        recalled: usize,
        noted: usize,
        up: Vec<f64>,
        down: Vec<f64>,
    },
}
impl Card {
    fn look(&self) -> Look {
        let (icon, section, title) = match self {
            Card::Moon { .. } => ("last", "last", "CONTEXT"),
            Card::History { .. } => ("touching", "touching", "TOKENS PER REQUEST"),
            Card::Agents { .. } => ("flight", "flight", "AGENTS"),
            Card::Cache { .. } => ("told", "told", "CACHE"),
            Card::WhatIf { .. } => ("dreams", "dreams", "WHAT IF\u{2026}"),
            Card::Story { .. } => ("rules", "rules", "STORY"),
            Card::Mirror { .. } => ("memory", "memory", "RECALL IN \u{b7} NOTES OUT"),
        };
        Look {
            icon,
            section,
            title,
        }
    }
}
/// A run of the pill's text: the words, their ink, whether they are bold.
pub type Run = (String, Ink, bool);
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ink {
    Dim,
    White,
    Green,
    Lavender,
    Amber,
    Red,
}
impl Ink {
    fn rgb(self) -> Rgb {
        match self {
            Ink::Dim => theme::DATA_QUIET,
            Ink::White => theme::DATA,
            Ink::Green => theme::STRIP_GREEN,
            Ink::Lavender => theme::STRIP_PURPLE,
            Ink::Amber => theme::RAMP_AMBER,
            Ink::Red => theme::RAMP_RED,
        }
    }
}
/// The collapsed masthead: two lines of groups of runs on the masthead's slate, at one text size.
#[derive(Clone, Debug, PartialEq)]
pub struct Pill {
    pub size: f32,
    pub lines: Vec<Vec<Vec<Run>>>,
}

// ---------------------------------------------------------------- widgets

struct Env<'a> {
    f: &'a CardFonts,
    cell: (u32, u32),
}
impl Canvas {
    /// A face with a hairline border, corners `r` points.
    fn panel(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, face: Rgb) {
        self.rrect(x, y, w, h, r, px(theme::HAIRLINE));
        self.rrect(
            x + 0.5,
            y + 0.5,
            w - 1.,
            h - 1.,
            (r - 0.5).max(0.),
            px(face),
        );
    }
    fn lit(&mut self, points: &[(f32, f32)], col: Rgba) {
        if self.dry || points.len() < 3 {
            return;
        }
        let mut pb = PathBuilder::new();
        pb.move_to(points[0].0, points[0].1);
        for p in &points[1..] {
            pb.line_to(p.0, p.1);
        }
        pb.close();
        if let Some(path) = pb.finish() {
            self.pm
                .fill_path(&path, &paint(col), FillRule::Winding, sc(), None);
        }
    }
}
/// The pane's own icon for a section, drawn at the left of a heading on the card's face.
fn icon(c: &mut Canvas, e: &Env, look: Look, x: f32, y: f32) {
    if c.dry {
        return;
    }
    type Key = (&'static str, (u32, u32), Rgb);
    static ICONS: LazyLock<std::sync::Mutex<std::collections::HashMap<Key, Option<Pixmap>>>> =
        LazyLock::new(Default::default);
    let key = (look.icon, e.cell, look.face());
    let Ok(mut cache) = ICONS.lock() else {
        return;
    };
    let made = cache
        .entry(key)
        .or_insert_with(|| super::raster::iso(look.icon, e.cell, [look.face(), look.face()]));
    if let Some(pm) = made {
        let scale = 0.62;
        c.pm.draw_pixmap(
            0,
            0,
            pm.as_ref(),
            &tiny_skia::PixmapPaint {
                quality: tiny_skia::FilterQuality::Bilinear,
                ..Default::default()
            },
            Transform::from_row(scale, 0., 0., scale, x * S, y * S),
            None,
        );
    }
}
const HEAD_H: f32 = 26.;
/// The card's heading: its icon, its title in the section's ink, a word at the right.
fn heading(c: &mut Canvas, e: &Env, look: Look, x: f32, y: f32, w: f32, right: &str) {
    icon(c, e, look, x + PAD - 4., y + PAD - 6.);
    let tx = x + PAD + 30.;
    let base = y + PAD + 7. + e.f.cap(10.5, 600.) / 2.;
    c.text(
        e.f,
        look.title,
        tx,
        base,
        10.5,
        600.,
        px(look.ink()),
        0.8,
        Anchor::L,
    );
    if !right.is_empty() {
        let room = w - 2. * PAD - 30. - e.f.width(look.title, 10.5, 600., 0.8) - 8.;
        let right = fit_width(e.f, right, 11., MONO, room);
        c.text(
            e.f,
            &right,
            x + w - PAD,
            base,
            11.,
            MONO,
            px(theme::QUIET),
            0.,
            Anchor::R,
        );
    }
}
/// `text` cut with an ellipsis to fit `room` points.
fn fit_width(f: &CardFonts, text: &str, size: f32, wght: f32, room: f32) -> String {
    if f.width(text, size, wght, 0.) <= room {
        return text.to_owned();
    }
    let mut out: String = text.to_owned();
    while !out.is_empty() && f.width(&format!("{out}\u{2026}"), size, wght, 0.) > room {
        out.pop();
    }
    format!("{}\u{2026}", out.trim_end())
}
/// `text` broken into lines of at most `room` points, the last cut with an ellipsis.
fn wrap(f: &CardFonts, text: &str, size: f32, wght: f32, room: f32, most: usize) -> Vec<String> {
    let mut lines: Vec<String> = vec![];
    let mut line = String::new();
    for word in text.split_whitespace() {
        let tried = if line.is_empty() {
            word.to_owned()
        } else {
            format!("{line} {word}")
        };
        if f.width(&tried, size, wght, 0.) > room && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            line = word.to_owned();
        } else {
            line = tried;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.len() > most {
        lines.truncate(most);
        let last = lines[most - 1].clone();
        lines[most - 1] = fit_width(f, &format!("{last} \u{2026}"), size, wght, room - 1.);
    }
    lines
}
fn legend_row(
    c: &mut Canvas,
    f: &CardFonts,
    x: f32,
    y: f32,
    w: f32,
    dot: Rgb,
    label: &str,
    value: &str,
) {
    let mid = y + 9.;
    let base = mid + f.cap(12., 400.) / 2.;
    c.circle(x + 4., mid, 3.5, px(dot));
    c.text(
        f,
        label,
        x + 15.,
        base,
        12.,
        400.,
        px(theme::QUIET),
        0.,
        Anchor::L,
    );
    if !value.is_empty() {
        c.text(
            f,
            value,
            x + w,
            base,
            12.,
            MONO,
            px(theme::INK),
            0.,
            Anchor::R,
        );
    }
}
fn legend(c: &mut Canvas, f: &CardFonts, x: f32, y: f32, w: f32, rows: &[Legend]) -> f32 {
    for (i, (dot, label, value)) in rows.iter().enumerate() {
        legend_row(c, f, x, y + 18. * i as f32, w, dot.rgb(), label, value);
    }
    18. * rows.len() as f32
}
fn bars(c: &mut Canvas, x: f32, bottom: f32, w: f32, h: f32, data: &[(f64, f64)]) {
    if data.is_empty() {
        return;
    }
    let n = (((w + 1.) / 3.).floor() as usize).min(data.len()).max(1);
    let x0 = x + (w - (3. * n as f32 - 1.));
    let peak = data.iter().map(|(a, b)| a + b).fold(0., f64::max).max(1.);
    for i in 0..n {
        // the newest bar sits on the right edge
        let (a, b) = data[data.len() - n + i];
        let ha = (((a / peak) as f32 * h * 2.).round() / 2.).max(if a > 0. { 0.5 } else { 0. });
        let hb = (((b / peak) as f32 * h * 2.).round() / 2.).max(if b > 0. { 0.5 } else { 0. });
        let bx = x0 + 3. * i as f32;
        c.rect(bx, bottom - ha, 2., ha, px(theme::CTX));
        c.rect(bx, bottom - ha - hb, 2., hb, px(theme::TURN));
    }
}
/// The context moon: a night disc, lit from the right by the share of the window in use.
fn moon(c: &mut Canvas, f: &CardFonts, cx: f32, cy: f32, r: f32, m: &MoonFact) {
    if !c.dry {
        c.circle(cx, cy, r + 0.75, px(theme::HAIRLINE));
        c.circle(cx, cy, r, px(theme::SLATE_TOP));
        let share = m.share.clamp(0., 1.) as f32;
        if share >= 0.995 {
            c.circle(cx, cy, r, px(super::text::heat(m.tokens)));
        } else if share > 0.004 {
            let k = 1. - 2. * share;
            let steps = 48;
            let mut pts = vec![];
            for i in 0..=steps {
                let t =
                    -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / steps as f32;
                pts.push((cx + r * t.cos(), cy + r * t.sin()));
            }
            for i in 0..=steps {
                let t =
                    std::f32::consts::FRAC_PI_2 - std::f32::consts::PI * i as f32 / steps as f32;
                pts.push((cx + k * r * t.cos(), cy + r * t.sin()));
            }
            c.lit(&pts, px(super::text::heat(m.tokens)));
        }
    }
    let n = ((m.share * 100.).round() as i64).to_string();
    let (l, nr, _, _) = f.ink(&n, 20., 600., 0.);
    let adv = f.width(&n, 20., 600., 0.);
    let pr = f.ink("%", 12., 600., 0.).1;
    let x0 = cx - (l + adv + 0.5 + pr) / 2.;
    let base = cy + f.cap(20., 600.) / 2.;
    let _ = nr;
    c.text(f, &n, x0, base, 20., 600., px(theme::DATA), 0., Anchor::L);
    c.text(
        f,
        "%",
        x0 + adv + 0.5,
        base,
        12.,
        600.,
        px(theme::DATA),
        0.,
        Anchor::L,
    );
}
fn moon_card(
    c: &mut Canvas,
    e: &Env,
    look: Look,
    x: f32,
    y: f32,
    w: f32,
    moon_fact: &Option<MoonFact>,
    bars: &[BarFact],
) -> f32 {
    heading(c, e, look, x, y, w, "");
    let f = e.f;
    let top = y + PAD + HEAD_H + 6.;
    let r = 36.;
    let mut left = x + PAD;
    let mut height = 0.;
    if let Some(m) = moon_fact {
        let cx = x + PAD + r;
        moon(c, f, cx, top + r, r, m);
        c.text(
            f,
            &m.caption,
            cx,
            top + 2. * r + 6. + f.cap(11., MONO),
            11.,
            MONO,
            px(theme::QUIET),
            0.,
            Anchor::M,
        );
        left = x + PAD + 2. * r + 22.;
        height = 2. * r + 6. + f.cap(11., MONO) + 4.;
    }
    let bw = x + w - PAD - left;
    let row = 40.;
    let used = row * bars.len() as f32;
    let mut by = top + ((height - used) / 2.).max(0.);
    for b in bars {
        let ink = match b.tone {
            Dot::Amber => theme::DREAM,
            Dot::Red => theme::TURN,
            _ => theme::CTX,
        };
        let base = by + 9. + f.cap(11., 600.) / 2.;
        c.text(f, &b.name, left, base, 11., 600., px(ink), 0.6, Anchor::L);
        c.text(
            f,
            &format!("{}%", (b.share * 100.).round() as i64),
            left + bw,
            base,
            13.,
            MONO,
            px(theme::INK),
            0.,
            Anchor::R,
        );
        let ty = by + 17.;
        c.rrect(left, ty, bw, 5., 2.5, px(theme::TRACK));
        let fw = (bw * b.share.clamp(0., 1.) as f32).max(if b.share > 0. { 3. } else { 0. });
        if fw > 0. {
            c.rrect(left, ty, fw, 5., 2.5, px(ink));
        }
        if let Some(p) = b.pace {
            // where an even burn would stand now
            c.rect(left + bw * p as f32 - 0.5, ty - 2., 1., 9., px(theme::INK));
        }
        c.text(
            f,
            &b.caption,
            left,
            by + 33. + f.cap(10., 400.) / 2.,
            10.,
            400.,
            px(theme::FAINT),
            0.,
            Anchor::L,
        );
        by += row;
    }
    let body = height.max(used);
    top + body + PAD - y
}
fn history(
    c: &mut Canvas,
    e: &Env,
    look: Look,
    x: f32,
    y: f32,
    w: f32,
    data: &[(f64, f64)],
    head: &str,
    rows: &[Legend],
) -> f32 {
    heading(c, e, look, x, y, w, head);
    let (ix, iw) = (x + PAD, w - 2. * PAD);
    let mut cy = y + PAD + HEAD_H + 6.;
    let bh = 44.;
    bars(c, ix, cy + bh, iw, bh, data);
    cy += bh + 8.;
    cy += legend(c, e.f, ix, cy, iw, rows);
    cy + PAD - y
}
fn agents_card(
    c: &mut Canvas,
    e: &Env,
    look: Look,
    x: f32,
    y: f32,
    w: f32,
    slots: &[Slot],
    rows: &[Legend],
) -> f32 {
    heading(c, e, look, x, y, w, "");
    let (ix, iw) = (x + PAD, w - 2. * PAD);
    let mut cy = y + PAD + HEAD_H + 6.;
    let d = 12.;
    for (i, slot) in slots.iter().enumerate() {
        let px0 = ix + i as f32 * (d + 6.);
        if px0 + d > ix + iw {
            break;
        }
        match slot {
            Slot::Running => c.rrect(px0, cy, d, d, 3., px(theme::CTX)),
            Slot::Stalled => c.rrect(px0, cy, d, d, 3., px(theme::DREAM)),
            Slot::Done => {
                c.rrect(px0, cy, d, d, 3., px(theme::RECALL));
                c.rrect(px0 + 2., cy + 2., d - 4., d - 4., 1.5, px(look.face()));
            }
        }
    }
    cy += d + 10.;
    cy += legend(c, e.f, ix, cy, iw, rows);
    cy + PAD - y
}
fn cache_card(
    c: &mut Canvas,
    e: &Env,
    look: Look,
    x: f32,
    y: f32,
    w: f32,
    share: f64,
    cold: bool,
    rows: &[Legend],
) -> f32 {
    heading(c, e, look, x, y, w, "");
    let (ix, iw) = (x + PAD, w - 2. * PAD);
    let mut cy = y + PAD + HEAD_H + 6.;
    let ink = if cold { theme::DREAM } else { theme::RECALL };
    c.rrect(ix, cy, iw, 6., 3., px(theme::TRACK));
    let fw = (iw * share.clamp(0., 1.) as f32).max(if share > 0. { 3. } else { 0. });
    if fw > 0. {
        c.rrect(ix, cy, fw, 6., 3., px(ink));
    }
    cy += 6. + 10.;
    cy += legend(c, e.f, ix, cy, iw, rows);
    cy + PAD - y
}
fn what_if_card(
    c: &mut Canvas,
    e: &Env,
    look: Look,
    x: f32,
    y: f32,
    w: f32,
    line: &str,
    tally: &str,
    count: usize,
    unread: usize,
) -> f32 {
    heading(c, e, look, x, y, w, tally);
    let (ix, iw) = (x + PAD, w - 2. * PAD);
    let mut cy = y + PAD + HEAD_H + 4.;
    let f = e.f;
    if !line.is_empty() {
        for l in wrap(f, line, 12., 400., iw, 3) {
            c.text(
                f,
                &l,
                ix,
                cy + 8. + f.cap(12., 400.) / 2.,
                12.,
                400.,
                px(theme::INK),
                0.,
                Anchor::L,
            );
            cy += 17.;
        }
        cy += 3.;
    }
    let said = format!(
        "{count} {} this session \u{b7} {unread} unread",
        if count == 1 { "dream" } else { "dreams" }
    );
    c.text(
        f,
        &said,
        ix,
        cy + 6. + f.cap(10., 400.) / 2.,
        10.,
        400.,
        px(theme::FAINT),
        0.,
        Anchor::L,
    );
    cy + 14. + PAD - y
}
fn story_card(
    c: &mut Canvas,
    e: &Env,
    look: Look,
    x: f32,
    y: f32,
    w: f32,
    rights: &str,
    now: &str,
    shifts: &[(String, bool)],
) -> f32 {
    heading(c, e, look, x, y, w, "");
    let (ix, iw) = (x + PAD, w - 2. * PAD);
    let f = e.f;
    let mut cy = y + PAD + HEAD_H + 2.;
    if !rights.is_empty() {
        let said = fit_width(f, rights, 10., MONO, iw);
        c.text(
            f,
            &said,
            ix,
            cy + 6. + f.cap(10., MONO) / 2.,
            10.,
            MONO,
            px(theme::FAINT),
            0.,
            Anchor::L,
        );
        cy += 16.;
    }
    for l in wrap(f, now, 12.5, 400., iw, 4) {
        c.text(
            f,
            &l,
            ix,
            cy + 8. + f.cap(12.5, 400.) / 2.,
            12.5,
            400.,
            px(theme::INK),
            0.,
            Anchor::L,
        );
        cy += 17.;
    }
    if !shifts.is_empty() {
        cy += 4.;
        c.rect(ix, cy, iw, 0.5, px(theme::HAIRLINE));
        cy += 6.;
        for (text, loud) in shifts.iter().take(9) {
            if *loud {
                let l = fit_width(f, text, 11.5, 400., iw);
                c.text(
                    f,
                    &l,
                    ix,
                    cy + 7. + f.cap(11.5, 400.) / 2.,
                    11.5,
                    400.,
                    px(theme::INK),
                    0.,
                    Anchor::L,
                );
                cy += 15.;
            } else {
                let l = fit_width(f, text.trim(), 10., MONO, iw);
                c.text(
                    f,
                    &l,
                    ix,
                    cy + 6. + f.cap(10., MONO) / 2.,
                    10.,
                    MONO,
                    px(theme::QUIET),
                    0.,
                    Anchor::L,
                );
                cy += 13.;
            }
        }
    }
    cy + PAD - y
}
fn mirror_card(
    c: &mut Canvas,
    e: &Env,
    look: Look,
    x: f32,
    y: f32,
    w: f32,
    recalled: usize,
    noted: usize,
    up: &[f64],
    down: &[f64],
) -> f32 {
    heading(c, e, look, x, y, w, "");
    let f = e.f;
    let (ix, iw) = (x + PAD, w - 2. * PAD);
    let mut cy = y + PAD + HEAD_H + 4.;
    let cap = f.cap(20., MONO);
    let half = iw / 2.;
    let figs = [
        (count_text(recalled), "RECALL IN", theme::RECALL),
        (count_text(noted), "NOTES OUT", theme::CTX),
    ];
    for (i, (v, label, col)) in figs.iter().enumerate() {
        let fx = ix + half * i as f32;
        c.text(f, v, fx, cy + cap, 20., MONO, px(theme::INK), 0., Anchor::L);
        let ly = cy + cap + 6. + 6.;
        c.circle(fx + 3.5, ly, 3.5, px(*col));
        c.text(
            f,
            label,
            fx + 12.,
            ly + f.cap(10., 600.) / 2.,
            10.,
            600.,
            px(*col),
            0.8,
            Anchor::L,
        );
    }
    cy += cap + 6. + 12. + 10.;
    let n = (((iw + 1.) / 3.).floor() as usize).max(1);
    let x0 = ix + (iw - (3. * n as f32 - 1.));
    let arm = 24.;
    let mid = cy + arm;
    let (top, bottom) = (
        up.iter().copied().fold(0., f64::max).max(1.),
        down.iter().copied().fold(0., f64::max).max(1.),
    );
    for i in 0..n {
        let bx = x0 + 3. * i as f32;
        let at = |v: &[f64]| v.get(i * v.len().max(1) / n).copied().unwrap_or(0.);
        let (u, d) = (at(up), at(down));
        if u > 0. {
            let hu = (((u / top) as f32 * arm * 2.).round() / 2.).max(0.5);
            c.rect(bx, mid - hu, 2., hu, px(theme::RECALL));
        }
        if d > 0. {
            let hd = (((d / bottom) as f32 * arm * 2.).round() / 2.).max(0.5);
            c.rect(bx, mid + 1., 2., hd, px(theme::CTX));
        }
    }
    c.rect(ix, mid, iw, 1., px(theme::HAIRLINE));
    cy += 2. * arm + 1.;
    cy + PAD - y
}
fn count_text(n: usize) -> String {
    if n >= 1000 {
        short(n as f64)
    } else {
        n.to_string()
    }
}

// ---------------------------------------------------------------- drawing

fn draw_body(c: &mut Canvas, e: &Env, card: &Card, x: f32, y: f32, w: f32) -> f32 {
    let look = card.look();
    match card {
        Card::Moon { moon, bars } => moon_card(c, e, look, x, y, w, moon, bars),
        Card::History { bars, head, legend } => history(c, e, look, x, y, w, bars, head, legend),
        Card::Agents { slots, legend } => agents_card(c, e, look, x, y, w, slots, legend),
        Card::Cache {
            share,
            cold,
            legend,
        } => cache_card(c, e, look, x, y, w, *share, *cold, legend),
        Card::WhatIf {
            line,
            tally,
            count,
            unread,
        } => what_if_card(c, e, look, x, y, w, line, tally, *count, *unread),
        Card::Story {
            rights,
            now,
            shifts,
        } => story_card(c, e, look, x, y, w, rights, now, shifts),
        Card::Mirror {
            recalled,
            noted,
            up,
            down,
        } => mirror_card(c, e, look, x, y, w, *recalled, *noted, up, down),
    }
}
fn width_pt(cols: usize, cell: (u32, u32)) -> f32 {
    cols as f32 * cell.0 as f32 / S
}
/// The card's height in points at a picture `cols` wide.
pub fn height_pt(card: &Card, cols: usize, cell: (u32, u32), f: &CardFonts) -> f32 {
    let mut dry = Canvas::dry();
    draw_body(
        &mut dry,
        &Env { f, cell },
        card,
        MARGIN,
        0.,
        width_pt(cols, cell) - 2. * MARGIN,
    )
}
/// The rows a card takes, a gap under it included.
pub fn rows_of(card: &Card, cols: usize, cell: (u32, u32), f: &CardFonts) -> usize {
    let row_pt = cell.1 as f32 / S;
    ((height_pt(card, cols, cell, f) + GAP) / row_pt).ceil() as usize
}
fn ground(c: &mut Canvas, cell: (u32, u32), backs: &[Rgb]) {
    if c.dry {
        return;
    }
    let rows = c.pm.height() / cell.1.max(1);
    for r in 0..rows {
        let Some(b) = backs.get(r as usize).or(backs.last()) else {
            return;
        };
        c.rect(
            0.,
            (r * cell.1) as f32 / S,
            c.pm.width() as f32 / S,
            cell.1 as f32 / S,
            [b[0], b[1], b[2], 255],
        );
    }
}
/// One card as a picture `cols` by `rows` cells, on the pane's grounds.
pub fn draw_card(
    card: &Card,
    cols: usize,
    rows: usize,
    cell: (u32, u32),
    backs: &[Rgb],
    f: &CardFonts,
) -> Option<Pixmap> {
    let mut c = Canvas::new(
        u32::try_from(cols).ok()?.checked_mul(cell.0)?,
        u32::try_from(rows).ok()?.checked_mul(cell.1)?,
    )?;
    ground(&mut c, cell, backs);
    let e = Env { f, cell };
    let w = width_pt(cols, cell) - 2. * MARGIN;
    let mut dry = Canvas::dry();
    let h = draw_body(&mut dry, &e, card, MARGIN, 0., w);
    c.panel(MARGIN, 0., w, h, 5., card.look().face());
    draw_body(&mut c, &e, card, MARGIN, 0., w);
    Some(c.pm)
}
const PILL_GAP: f32 = 10.;
fn pill_width(p: &Pill, f: &CardFonts) -> f32 {
    let line = |groups: &Vec<Vec<Run>>| {
        let mut w = 0.;
        for (gi, g) in groups.iter().enumerate() {
            for (s, _, bold) in g {
                w += f.width(s, p.size, if *bold { 600. } else { 500. }, 0.);
            }
            if gi + 1 < groups.len() {
                w += PILL_GAP;
            }
        }
        w
    };
    p.lines.iter().map(line).fold(0., f32::max) + 2. * 12.
}
/// The pill's width in cells, one cell of air at its right.
pub fn pill_cols(p: &Pill, cell: (u32, u32), f: &CardFonts) -> usize {
    let cell_pt = cell.0 as f32 / S;
    ((pill_width(p, f) + 2. * MARGIN) / cell_pt).ceil() as usize + 1
}
/// The pill in a picture `cols` by `rows` cells: the masthead's slate, its data ink.
pub fn draw_pill(
    p: &Pill,
    cols: usize,
    rows: usize,
    cell: (u32, u32),
    backs: &[Rgb],
    f: &CardFonts,
) -> Option<Pixmap> {
    let mut c = Canvas::new(
        u32::try_from(cols).ok()?.checked_mul(cell.0)?,
        u32::try_from(rows).ok()?.checked_mul(cell.1)?,
    )?;
    ground(&mut c, cell, backs);
    let height_pt = rows as f32 * cell.1 as f32 / S;
    let (w, h) = (pill_width(p, f), (height_pt - 2.).max(10.));
    if let Some(path) = rrect_path(MARGIN, 1., w, h, 5.) {
        let [t, b] = [theme::SLATE_TOP, theme::SLATE_BOTTOM];
        let stops = vec![
            tiny_skia::GradientStop::new(0., tiny_skia::Color::from_rgba8(t[0], t[1], t[2], 255)),
            tiny_skia::GradientStop::new(1., tiny_skia::Color::from_rgba8(b[0], b[1], b[2], 255)),
        ];
        if let Some(shader) = tiny_skia::LinearGradient::new(
            tiny_skia::Point::from_xy(0., 1. * S),
            tiny_skia::Point::from_xy(0., (1. + h) * S),
            stops,
            tiny_skia::SpreadMode::Pad,
            Transform::identity(),
        ) {
            let mut paint = Paint {
                anti_alias: true,
                ..Paint::default()
            };
            paint.shader = shader;
            c.pm.fill_path(&path, &paint, FillRule::Winding, sc(), None);
        }
    }
    let n = p.lines.len().max(1) as f32;
    for (li, groups) in p.lines.iter().enumerate() {
        let mid = 1. + h * (li as f32 + 0.5) / n;
        let base = mid + f.cap(p.size, 600.) / 2.;
        let mut px0 = MARGIN + 12.;
        for (gi, g) in groups.iter().enumerate() {
            for (s, ink, bold) in g {
                px0 += c.text(
                    f,
                    s,
                    px0,
                    base,
                    p.size,
                    if *bold { 600. } else { 500. },
                    px(ink.rgb()),
                    0.,
                    Anchor::L,
                );
            }
            if gi + 1 < groups.len() {
                px0 += PILL_GAP;
            }
        }
    }
    Some(c.pm)
}

// ---------------------------------------------------------------- what the pane knows

/// The bar of a quota window, only while its reading is current.
fn quota_bar(q: &Value, now: f64) -> Option<BarFact> {
    let used = q["used"].as_f64()?;
    let code = string(&q["code"]);
    let resets = q["resets"].as_f64();
    let caption = resets
        .map(|at| {
            if code == "5H" {
                format!("resets {}", masthead::span((at - now).max(0.)))
            } else {
                format!("resets {}", masthead::reset_label(at, now))
            }
        })
        .unwrap_or_default();
    Some(BarFact {
        name: format!(
            "{code}{}",
            if super::text::ahead_of_pace(q) {
                "\u{bb}"
            } else {
                ""
            }
        ),
        share: (used / 100.).clamp(0., 1.),
        caption,
        pace: q["pace"].as_f64().filter(|p| (0. ..=1.).contains(p)),
        tone: match string(&q["sev"]) {
            "red" => Dot::Red,
            "amber" => Dot::Amber,
            _ => Dot::Blue,
        },
    })
}
fn ctx_moon(g: &Value) -> Option<MoonFact> {
    let tokens = g["ctx"]["tokens"].as_f64().filter(|n| *n > 0.)?;
    let size = g["ctx"]["size"].as_f64().filter(|n| *n > 0.)?;
    Some(MoonFact {
        share: (tokens / size).clamp(0., 1.),
        tokens,
        caption: format!("{}/{}", short(tokens), short(size)),
    })
}
/// Requests as (cached, fresh) token pairs, oldest first, from the transcript's own usage figures.
fn request_bars(s: &Value) -> (Vec<(f64, f64)>, bool) {
    let mut replies: Vec<(f64, f64, f64)> = array(&s["replies"])
        .iter()
        .map(|r| (num(&r[0]), num(&r[1]), num(&r[3])))
        .filter(|(t, c, _)| *t != 0. && *c > 0.)
        .collect();
    if replies.len() >= 2 {
        replies.sort_by(|a, b| a.0.total_cmp(&b.0));
        return (
            replies
                .iter()
                .map(|(_, c, fresh)| ((c - fresh).max(0.), fresh.min(*c)))
                .collect(),
            true,
        );
    }
    let mut main: Vec<(f64, f64)> = array(&s["main"])
        .iter()
        .map(|r| (num(&r[0]), num(&r[1])))
        .filter(|(t, c)| *t != 0. && *c > 0.)
        .collect();
    main.sort_by(|a, b| a.0.total_cmp(&b.0));
    (main.iter().map(|(_, c)| (*c, 0.)).collect(), false)
}
fn history_card(g: &Value, s: &Value, csr: usize) -> Option<Card> {
    let (data, split) = request_bars(s);
    if data.len() < 2 {
        return None;
    }
    let mut legend = vec![];
    if split {
        let shown = &data[data.len().saturating_sub(100)..];
        let total: f64 = shown.iter().map(|(a, b)| a + b).sum();
        let fresh: f64 = shown.iter().map(|(_, b)| b).sum();
        if total > 0. {
            let f = (fresh / total * 100.).round() as i64;
            legend.push((Dot::Blue, "Cached".into(), format!("{}%", 100 - f)));
            legend.push((Dot::Pink, "Fresh".into(), format!("{f}%")));
        }
    }
    let trend = g["trend"].as_f64().filter(|t| *t != 0.);
    let tokens = g["ctx"]["tokens"].as_f64().filter(|n| *n > 0.);
    if !split {
        if let Some(t) = tokens {
            legend.push((Dot::Blue, "Context".into(), short(t)));
        }
    }
    if let Some(t) = trend {
        legend.push((
            Dot::Amber,
            "Growth".into(),
            format!("{}{}/turn", if t > 0. { "+" } else { "-" }, short(t.abs())),
        ));
    }
    if csr > 0 {
        legend.push((Dot::Green, "Compacted by CSR".into(), csr.to_string()));
    }
    let head = match (trend, tokens) {
        (Some(t), _) => format!("{}{}/turn", if t > 0. { "+" } else { "-" }, short(t.abs())),
        (None, Some(n)) => format!("{} context", short(n)),
        _ => String::new(),
    };
    Some(Card::History {
        bars: data,
        head,
        legend,
    })
}
fn cache_card_of(cache: &Value) -> Option<Card> {
    let cold = cache["cold"] == true;
    let (left, ttl) = (cache["left"].as_f64(), cache["ttl"].as_f64());
    if cold {
        let said = string(&cache["text"]);
        let re = said.strip_prefix("CACHE COLD").unwrap_or("").trim();
        let mut legend = vec![(Dot::Amber, "Cold".to_owned(), String::new())];
        if !re.is_empty() {
            legend.push((Dot::Track, "Re-reads".into(), re.to_owned()));
        }
        return Some(Card::Cache {
            share: 0.,
            cold,
            legend,
        });
    }
    let (left, ttl) = (left?, ttl.filter(|t| *t > 0.)?);
    Some(Card::Cache {
        share: (left / ttl).clamp(0., 1.),
        cold,
        legend: vec![
            (Dot::Green, "Warm".into(), String::new()),
            (
                Dot::Track,
                "Left".into(),
                if left < 60. {
                    "under 1m".into()
                } else {
                    masthead::span(left)
                },
            ),
        ],
    })
}
/// The dreams this session has: the open dream's words (else the latest), how many asks are
/// left, the count, and how many wait unread.
pub fn dream_facts(data: &Value, sid: &str) -> Option<Card> {
    if data["dreams_on"] == false {
        return None;
    }
    let mine: Vec<&Value> = array(&data["dreams"])
        .iter()
        .filter(|d| d["session"] == sid)
        .collect();
    let latest = mine.first()?;
    let fed = dreams::feed(array(&data["dreams"]), &data["chapter"], sid);
    let open = &fed["star"];
    let unread = usize::from(!open.is_null());
    let line = string(if open.is_null() {
        &latest["line"]
    } else {
        &open["line"]
    });
    let tally = format!(
        "{} of {} left",
        fed["left"].as_u64().unwrap_or(0),
        fed["of"].as_u64().unwrap_or(0)
    );
    Some(Card::WhatIf {
        line: line.to_owned(),
        tally,
        count: mine.len(),
        unread,
    })
}
/// The session's story as the pane tells it: where work stands and the Shift Map's lines, whose
/// source line carries the commit receipt. `None` without a story or a map.
fn story_of(data: &Value, s: &Value, now: f64) -> Option<Card> {
    let story = &data["story"];
    let said = string(&story["now"]).trim().to_owned();
    let rights = if story.is_null() {
        String::new()
    } else {
        super::super::pages::story_rights(story, now)
            .into_iter()
            .next()
            .unwrap_or_default()
    };
    let map = &data["story_map"];
    let mut shifts: Vec<(String, bool)> = vec![];
    if map.is_object() {
        let from = data["story_map_from"].as_str();
        let messages = if from.is_some() {
            data["story_map_messages"].as_u64()
        } else {
            s["messages"].as_u64()
        };
        let (_, rows) =
            super::super::shiftmap::shift_map_marked(map, messages, 56, false, Some(8), from);
        shifts = rows;
    }
    if said.is_empty() && shifts.is_empty() {
        return None;
    }
    Some(Card::Story {
        rights,
        now: said,
        shifts,
    })
}
fn buckets(times: &[f64], from: f64, to: f64, n: usize) -> Vec<f64> {
    let mut out = vec![0.; n];
    let span = (to - from).max(1.);
    for t in times {
        let i = (((t - from) / span) * n as f64) as usize;
        out[i.min(n - 1)] += 1.;
    }
    out
}
/// The session's recall in and notes out: counts, and each over time.
fn mirror_card_of(s: &Value, handed: usize, now: f64) -> Option<Card> {
    let times = |v: &Value| -> Vec<f64> {
        array(v)
            .iter()
            .filter_map(Value::as_f64)
            .filter(|t| *t > 0.)
            .collect()
    };
    let chunks = times(&s["learned"]["chunks"]);
    let mut got = times(&s["gave"]["recalls"]);
    got.extend(times(&s["gave"]["carried"]));
    if handed == 0 && chunks.is_empty() {
        return None;
    }
    let from = got
        .iter()
        .chain(&chunks)
        .copied()
        .fold(f64::MAX, f64::min)
        .min(now);
    let n = 120;
    Some(Card::Mirror {
        recalled: handed,
        noted: chunks.len(),
        up: buckets(&got, from, now, n),
        down: buckets(&chunks, from, now, n),
    })
}
/// The cards the info view draws for this session, top to bottom; a card without data is not here.
pub fn build(
    data: &Value,
    s: &Value,
    g: &Value,
    now: f64,
    handed: usize,
    agents: &agents::Projection,
) -> Vec<Card> {
    let mut out = vec![];
    out.extend(story_of(data, s, now));
    let quotas = array(&g["quotas"]);
    let bars: Vec<BarFact> = ["5H", "7D"]
        .iter()
        .filter_map(|code| {
            quotas
                .iter()
                .find(|q| q["code"] == *code)
                .and_then(|q| quota_bar(q, now))
        })
        .collect();
    let moon = ctx_moon(g);
    if moon.is_some() || !bars.is_empty() {
        out.push(Card::Moon { moon, bars });
    }
    let csr = array(&data["infinite"]["csr_at"]).len();
    out.extend(history_card(g, s, csr));
    if !agents.agents.is_empty() {
        let shown = &agents.agents[..agents.agents.len().min(10)];
        let running = agents
            .agents
            .iter()
            .filter(|a| agents::is_active(a))
            .count();
        let done = agents.agents.len() - running;
        out.push(Card::Agents {
            slots: shown
                .iter()
                .map(|a| {
                    if !agents::is_active(a) {
                        Slot::Done
                    } else if agents::stalled(a, now) {
                        Slot::Stalled
                    } else {
                        Slot::Running
                    }
                })
                .collect(),
            legend: vec![
                (Dot::Blue, "Running".into(), running.to_string()),
                (Dot::Green, "Finished".into(), done.to_string()),
            ],
        });
    }
    out.extend(cache_card_of(&g["cache"]));
    out.extend(dream_facts(data, string(&s["id"])));
    out.extend(mirror_card_of(s, handed, now));
    out
}
/// What a pill line says, level `short` trading words for room: every fact stays, once.
fn pill_lines(
    g: &Value,
    csr: usize,
    dreams: Option<usize>,
    short_form: bool,
) -> Vec<Vec<Vec<Run>>> {
    let sev_ink = |sev: &str| match sev {
        "red" => Ink::Red,
        "amber" => Ink::Amber,
        _ => Ink::White,
    };
    let word = |label: &str, value: String, ink: Ink| -> Vec<Run> {
        vec![(format!("{label} "), Ink::Dim, false), (value, ink, true)]
    };
    let mut first: Vec<Vec<Run>> = vec![];
    let tokens = g["ctx"]["tokens"].as_f64().filter(|n| *n > 0.);
    if let Some(t) = tokens {
        let value = match g["ctx"]["size"].as_f64().filter(|n| *n > 0.) {
            Some(size) => format!("{}/{}", short(t), short(size)),
            None => short(t),
        };
        first.push(word("CTX", value, sev_ink(string(&g["ctx"]["sev"]))));
    }
    if tokens.is_some() {
        if let Some(t) = g["trend"].as_f64().filter(|t| *t != 0.) {
            first.push(vec![(
                format!(
                    "{}{}/{}",
                    if t > 0. { "+" } else { "-" },
                    short(t.abs()),
                    if short_form { "t" } else { "turn" }
                ),
                Ink::Dim,
                false,
            )]);
        }
    }
    let pay = if g["compacting"].is_object() {
        masthead::compacting_slot(&g["compacting"], false, true).0
    } else {
        string(&g["payback"]["text"]).to_owned()
    };
    if !pay.is_empty() {
        let pay = if short_form {
            pay.replace(" calls", "c")
                .replace(" call", "c")
                .replace(" prompts", "p")
                .replace(" prompt", "p")
        } else {
            pay
        };
        let ink = match string(&g["payback"]["sev"]) {
            "amber" => Ink::Amber,
            "advisory" => Ink::Lavender,
            _ => Ink::Green,
        };
        first.push(vec![(pay, ink, true)]);
    }
    if csr > 0 {
        first.push(vec![(
            format!("{csr} {}", if short_form { "CSR" } else { "by CSR" }),
            Ink::Dim,
            false,
        )]);
    }
    let mut second: Vec<Vec<Run>> = vec![];
    for q in array(&g["quotas"]).iter().filter(|q| q["used"].is_number()) {
        let said = super::text::paced(q);
        let said = if short_form {
            said.split(" \u{21bb}").next().unwrap_or(&said).to_owned()
        } else {
            said
        };
        let (code, rest) = said.split_once(' ').unwrap_or(("", &said));
        second.push(word(code, rest.to_owned(), sev_ink(string(&q["sev"]))));
    }
    let cache = string(&g["cache"]["text"]);
    if cache != "CACHE --" && !cache.is_empty() {
        let left = cache.strip_prefix("CACHE ").unwrap_or(cache);
        second.push(vec![(
            format!("\u{25f7} {left}"),
            sev_ink(string(&g["cache"]["sev"])),
            true,
        )]);
    }
    let sts = string(&g["sts"]["sev"]);
    if !sts.is_empty() {
        second.push(vec![("STS".into(), sev_ink(sts), true)]);
    }
    if let Some(n) = dreams.filter(|n| *n > 0) {
        second.push(vec![(format!("\u{263e} {n}"), Ink::Lavender, true)]);
    }
    [first, second]
        .into_iter()
        .filter(|l| !l.is_empty())
        .collect()
}
/// The collapsed masthead: the facts the text masthead shows, on two lines, at the largest size
/// that fits `avail` points. `None` when it has nothing to say or cannot hold it all (the text
/// rows stay, and they hold everything).
pub fn pill_of(
    g: &Value,
    csr: usize,
    dreams: Option<usize>,
    avail: f32,
    f: &CardFonts,
) -> Option<Pill> {
    // the dreams go first when room is short: they are on the info view's dreams card too
    for (size, short_form, with_dreams) in [
        (12., false, true),
        (12., false, false),
        (12., true, true),
        (12., true, false),
        (11., true, true),
        (11., true, false),
    ] {
        let lines = pill_lines(g, csr, dreams.filter(|_| with_dreams), short_form);
        if lines.is_empty() {
            return None;
        }
        let pill = Pill { size, lines };
        if pill_width(&pill, f) + 2. * MARGIN <= avail {
            return Some(pill);
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn the_font_lookup_tries_the_system_font_then_inter_noto_dejavu_liberation() {
        let only = |wanted: &[&str]| {
            let wanted: Vec<String> = wanted.iter().map(|s| s.to_string()).collect();
            move |p: &str| wanted.iter().any(|w| w == p)
        };
        let sys = "/System/Library/Fonts/SFNS.ttf";
        let noto = "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf";
        let dejavu = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf";
        let inter = "/home/me/.local/share/fonts/Inter-Regular.ttf";
        let liberation = "/usr/share/fonts/liberation/LiberationSans-Regular.ttf";
        let all: &[&str] = &[sys, noto, dejavu, inter, liberation];
        assert_eq!(pick_family(only(all), "/home/me").unwrap().regular, sys);
        assert!(pick_family(only(all), "/home/me").unwrap().variable);
        let linux: &[&str] = &[noto, dejavu, inter, liberation];
        assert_eq!(pick_family(only(linux), "/home/me").unwrap().regular, inter);
        assert_eq!(
            pick_family(only(&linux[..2]), "/home/me").unwrap().regular,
            noto
        );
        assert_eq!(
            pick_family(only(&linux[1..2]), "/home/me").unwrap().regular,
            dejavu
        );
        assert_eq!(
            pick_family(only(&linux[3..]), "/home/me").unwrap().regular,
            liberation
        );
        let found = pick_family(only(&linux[3..]), "/home/me").unwrap();
        assert!(!found.variable && found.bold.is_none());
        assert_eq!(pick_family(only(&[]), "/home/me"), None);
    }
    fn full_gauges() -> Value {
        json!({
            "ctx": {"tokens": 170000., "size": 1000000., "sev": "normal"},
            "trend": 22000.,
            "payback": {"text": "\u{267b} ~3 calls", "sev": "normal"},
            "quotas": [
                {"code": "7D", "used": 62., "sev": "normal", "pace": 0.3, "text": "7D 62% \u{21bb}Sun 09:00"},
                {"code": "5H", "used": 41., "sev": "normal", "pace": null, "text": "5H 41%"},
            ],
            "cache": {"text": "CACHE 57m", "sev": "normal"},
            "sts": {"sev": "amber"},
        })
    }
    fn said(p: &Pill) -> String {
        p.lines
            .iter()
            .flatten()
            .map(|g| g.iter().map(|r| r.0.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join(" | ")
    }
    #[test]
    fn the_pill_keeps_every_fact_of_the_text_masthead_at_46_and_33_columns() {
        let Some(f) = fonts() else {
            eprintln!("no usable font: skipped");
            return;
        };
        let g = full_gauges();
        // the pane's picture width at that many columns, as draw_page works it out
        for cols in [46usize, 33] {
            let width = cols - 1;
            let avail = (width - 1) as f32 * 16. / 2. - 8.;
            let pill = pill_of(&g, 2, None, avail, f).unwrap_or_else(|| panic!("{cols} cols"));
            let text = said(&pill);
            eprintln!("{cols}: {text}");
            for fact in [
                "CTX 170k/1M",
                "22k/",
                "\u{267b} ~3",
                "2 ",
                "5H 41%",
                "7D 62%",
                "\u{25f7} 57m",
                "STS",
            ] {
                assert!(text.contains(fact), "{cols} cols lost {fact:?}: {text}");
            }
            assert!(pill_cols(&pill, (16, 34), f) < width, "{cols}");
        }
        // the pace arrow rides the 7D reading
        let mut ahead = g.clone();
        ahead["quotas"][0]["used"] = json!(80.);
        ahead["quotas"][0]["text"] = json!("7D 80% \u{21bb}Sun 09:00");
        let pill = pill_of(&ahead, 0, None, 360., f).unwrap();
        assert!(said(&pill).contains("7D 80%\u{bb}"), "{}", said(&pill));
    }
    #[test]
    fn a_pill_fact_with_no_data_drops_and_no_data_means_no_pill() {
        let Some(f) = fonts() else {
            return;
        };
        assert!(pill_of(&json!({}), 0, None, 360., f).is_none());
        let g = json!({"ctx": {"tokens": 88000., "size": 1000000., "sev": "normal"}});
        let text = said(&pill_of(&g, 0, None, 360., f).unwrap());
        assert_eq!(text, "CTX 88k/1M");
    }
    #[test]
    fn a_card_without_data_is_not_built() {
        let g =
            json!({"ctx": {"tokens": null}, "quotas": [], "cache": {"cold": false, "left": null}});
        let cards = build(
            &json!({}),
            &json!({"id": "x"}),
            &g,
            1.0,
            0,
            &agents::Projection::default(),
        );
        assert!(cards.is_empty());
    }
    #[test]
    fn every_kind_of_card_draws_in_under_five_milliseconds() {
        let Some(f) = fonts() else {
            eprintln!("no usable font: skipped");
            return;
        };
        let legend = vec![
            (Dot::Blue, "Cached".to_owned(), "91%".to_owned()),
            (Dot::Pink, "Fresh".to_owned(), "9%".to_owned()),
        ];
        let bar = |name: &str| BarFact {
            name: name.into(),
            share: 0.5,
            caption: "resets in 2h".into(),
            pace: Some(0.4),
            tone: Dot::Blue,
        };
        let cards = [
            Card::Moon {
                moon: Some(MoonFact {
                    share: 0.17,
                    tokens: 170000.,
                    caption: "170k/1M".into(),
                }),
                bars: vec![bar("5H"), bar("7D\u{bb}")],
            },
            Card::Story {
                rights: "rev 3 \u{b7} Haiku \u{b7} just told \u{b7} next at compact".into(),
                now: "Work stands at the cards: the moon gauge draws and the pill keeps its facts."
                    .into(),
                shifts: vec![
                    ("1  moon gauge replaced the rings".into(), true),
                    (
                        "   Haiku's words \u{b7} names match commit 1c4183bc".into(),
                        false,
                    ),
                ],
            },
            Card::History {
                bars: (0..300).map(|i| (i as f64, 20.)).collect(),
                head: "+34k/turn".into(),
                legend: legend.clone(),
            },
            Card::Agents {
                slots: vec![Slot::Running; 10],
                legend: legend.clone(),
            },
            Card::Cache {
                share: 0.78,
                cold: false,
                legend,
            },
            Card::WhatIf {
                line: "What if the cards drew from the same gauges as the text rows?".into(),
                tally: "3 of 5 left".into(),
                count: 2,
                unread: 1,
            },
            Card::Mirror {
                recalled: 3100,
                noted: 800,
                up: (0..120).map(f64::from).collect(),
                down: (0..120).map(|i| f64::from(i % 7)).collect(),
            },
        ];
        let cell = (16, 34);
        for card in &cards {
            let rows = rows_of(card, 44, cell, f);
            let start = std::time::Instant::now();
            let pm = draw_card(card, 44, rows, cell, &[[22, 27, 38]], f).unwrap();
            let took = start.elapsed();
            eprintln!(
                "{}: {took:?}",
                format!("{card:?}")
                    .split([' ', '(', '{'])
                    .next()
                    .unwrap_or("")
            );
            assert_eq!(pm.width(), 44 * 16);
            // debug builds are an order slower; the bound is the release one
            if !cfg!(debug_assertions) {
                assert!(took.as_millis() < 5, "{took:?}");
            }
        }
        let pill = pill_of(
            &serde_json::json!({"ctx": {"tokens": 1000., "size": 2000.}}),
            0,
            Some(2),
            400.,
            f,
        )
        .unwrap();
        let start = std::time::Instant::now();
        draw_pill(
            &pill,
            pill_cols(&pill, cell, f),
            2,
            cell,
            &[[22, 27, 38]],
            f,
        )
        .unwrap();
        eprintln!("pill: {:?}", start.elapsed());
    }
    #[test]
    fn cached_and_fresh_come_from_the_transcripts_own_usage() {
        let s = json!({"replies": [[1., 1000., 10., 100.], [2., 2000., 10., 200.]]});
        let (data, split) = request_bars(&s);
        assert!(split);
        assert_eq!(data, vec![(900., 100.), (1800., 200.)]);
    }
}

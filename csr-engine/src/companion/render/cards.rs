//! The cards: the pane's pixel-mode info view (a stack of rounded cards) and masthead pill.
//!
//! Every figure a card draws is measured: the builders at the bottom read the same gauges, session and
//! dream data the text rows read, and a card with nothing real to say is not built at all.
use super::text::Rgb;
use crate::companion::{agents, array, dreams, masthead, num, pyfmt::short, string, truthy};
use ab_glyph::{point, Font, FontVec, GlyphId, ScaleFont, VariableFont};
use serde_json::Value;
use std::sync::LazyLock;
use tiny_skia::{FillRule, LineCap, Paint, Path, PathBuilder, Pixmap, Rect, Stroke, Transform};

/// One point is two device pixels: the pane's cell size is already measured at twice the screen's.
const S: f32 = 2.0;
/// The narrowest picture, in points, the cards are laid out for; narrower panes keep their text.
pub const MIN_WIDTH_PT: f32 = 260.;
const PAD: f32 = 12.;
const GAP: f32 = 8.;
const MARGIN: f32 = 4.;

type Rgba = [u8; 4];
const fn hex(v: u32) -> Rgba {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8, 255]
}
const CARD: Rgba = hex(0x273246);
const TRACK: Rgba = hex(0x4a5263);
const BLUE: Rgba = hex(0x4f7dff);
const PINK: Rgba = hex(0xe0559a);
const AMBER: Rgba = hex(0xe8c547);
const RED: Rgba = hex(0xe8582c);
const LABEL: Rgba = hex(0xc9d1de);
const HEAD: Rgba = hex(0x5f8cff);
const INK: Rgba = hex(0xe6ebf2);
const GREEN: Rgba = hex(0x5fd38d);
const LAV: Rgba = hex(0xa99bff);
const PILL: Rgba = hex(0x2a3447);
const WHITE: Rgba = hex(0xffffff);
const DIM: Rgba = hex(0x9aa6b8);
const RULE: Rgba = hex(0x3a4558);

// ---------------------------------------------------------------- fonts

struct Face {
    font: FontVec,
    wght: f32,
    opsz: f32,
}
/// The system's UI font at the weights the cards use, and the symbol fonts behind it.
pub struct CardFonts {
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
        (!primary.is_empty()).then_some(Self { primary, fallback })
    }
    fn face(&self, size: f32, wght: f32) -> &FontVec {
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
/// An arc clockwise from 12 o'clock; angles in degrees clockwise from the top.
fn arc_path(cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Option<Path> {
    let (t0, t1) = ((a0 as f64).to_radians(), (a1 as f64).to_radians());
    let n = (((t1 - t0).abs() / std::f64::consts::FRAC_PI_2).ceil() as usize).max(1);
    let d = (t1 - t0) / n as f64;
    let k = 4. / 3. * (d / 4.).tan();
    let (cx, cy, r) = (cx as f64, cy as f64, r as f64);
    let pt = |t: f64| (cx + r * t.sin(), cy - r * t.cos());
    let mut pb = PathBuilder::new();
    let (x0, y0) = pt(t0);
    pb.move_to(x0 as f32, y0 as f32);
    for i in 0..n {
        let (ta, tb) = (t0 + d * i as f64, t0 + d * (i + 1) as f64);
        let (pa, pbp) = (pt(ta), pt(tb));
        let c1 = (pa.0 + k * r * ta.cos(), pa.1 + k * r * ta.sin());
        let c2 = (pbp.0 - k * r * tb.cos(), pbp.1 - k * r * tb.sin());
        pb.cubic_to(
            c1.0 as f32,
            c1.1 as f32,
            c2.0 as f32,
            c2.1 as f32,
            pbp.0 as f32,
            pbp.1 as f32,
        );
    }
    pb.finish()
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
    fn stroke(&mut self, p: &Path, w: f32, col: Rgba) {
        let st = Stroke {
            width: w,
            line_cap: LineCap::Butt,
            ..Stroke::default()
        };
        self.pm.stroke_path(p, &paint(col), &st, sc(), None);
    }
    /// A ring gauge. `segs` are consecutive (fraction, colour) arcs from 12 o'clock, clockwise.
    fn ring(&mut self, cx: f32, cy: f32, d: f32, stroke: f32, segs: &[(f32, Rgba)]) {
        if self.dry {
            return;
        }
        let r = d / 2. - stroke / 2.;
        if let Some(p) = PathBuilder::from_circle(cx, cy, r) {
            self.stroke(&p, stroke, TRACK);
        }
        let total: f32 = segs.iter().map(|s| s.0).sum();
        if total <= 0.0005 {
            return;
        }
        if total >= 0.9995 && segs.len() == 1 {
            if let Some(p) = PathBuilder::from_circle(cx, cy, r) {
                self.stroke(&p, stroke, segs[0].1);
            }
            return;
        }
        let mut acc = 0.;
        for (i, (f, col)) in segs.iter().enumerate() {
            let a0 = acc * 360.;
            let mut a1 = (acc + f) * 360.;
            if i + 1 < segs.len() {
                a1 += 0.5;
            }
            if let Some(p) = arc_path(cx, cy, r, a0, a1) {
                self.stroke(&p, stroke, *col);
            }
            acc += f;
        }
        self.circle(cx, cy - r, stroke / 2., segs[0].1);
        let t = (acc * 360.0_f32).to_radians();
        self.circle(
            cx + r * t.sin(),
            cy - r * t.cos(),
            stroke / 2.,
            segs[segs.len() - 1].1,
        );
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
    fn rgba(self) -> Rgba {
        match self {
            Dot::Blue => BLUE,
            Dot::Pink => PINK,
            Dot::Amber => AMBER,
            Dot::Green => GREEN,
            Dot::Track => TRACK,
            Dot::Red => RED,
        }
    }
}
/// One legend row: a dot, its label, its value.
pub type Legend = (Dot, String, String);
/// One ring of the top card: its share, the name in it, the line under it.
#[derive(Clone, Debug, PartialEq)]
pub struct RingFact {
    pub share: f64,
    pub name: String,
    pub caption: String,
    pub dot: Dot,
}
/// How an agent's slot is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Slot {
    Running,
    Stalled,
    Done,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Card {
    /// CTX, 5H and 7D rings, each with the line that says it in words.
    Rings(Vec<RingFact>),
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
    Dreams {
        count: usize,
        share: f64,
        line: String,
    },
    /// What CSR handed Claude, up, and what it wrote down, down, over the session.
    Mirror {
        recalled: usize,
        noted: usize,
        up: Vec<f64>,
        down: Vec<f64>,
    },
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
    fn rgba(self) -> Rgba {
        match self {
            Ink::Dim => DIM,
            Ink::White => WHITE,
            Ink::Green => GREEN,
            Ink::Lavender => LAV,
            Ink::Amber => AMBER,
            Ink::Red => RED,
        }
    }
}
/// The collapsed masthead: groups of runs in one rounded pill.
#[derive(Clone, Debug, PartialEq)]
pub struct Pill {
    pub groups: Vec<Vec<Run>>,
}

// ---------------------------------------------------------------- widgets

#[allow(clippy::too_many_arguments)]
fn legend_row(
    c: &mut Canvas,
    f: &CardFonts,
    x: f32,
    y: f32,
    w: f32,
    dot: Rgba,
    label: &str,
    value: &str,
) {
    let mid = y + 9.;
    let base = mid + f.cap(12., 400.) / 2.;
    c.circle(x + 4., mid, 4., dot);
    c.text(f, label, x + 16., base, 12., 400., INK, 0., Anchor::L);
    if !value.is_empty() {
        c.text(f, value, x + w, base, 12., 400., INK, 0., Anchor::R);
    }
}
fn legend(c: &mut Canvas, f: &CardFonts, x: f32, y: f32, w: f32, rows: &[Legend]) -> f32 {
    for (i, (dot, label, value)) in rows.iter().enumerate() {
        legend_row(c, f, x, y + 18. * i as f32, w, dot.rgba(), label, value);
    }
    18. * rows.len() as f32
}
fn header(c: &mut Canvas, f: &CardFonts, x: f32, y: f32, w: f32, name: &str, value: &str) {
    let base = y + 11.;
    c.text(f, name, x, base, 11., 600., HEAD, 0.6, Anchor::L);
    if !value.is_empty() {
        let room = w - f.width(name, 11., 600., 0.6) - 12.;
        let value = if f.width(value, 12., 400., 0.) > room {
            value.split(' ').next().unwrap_or(value)
        } else {
            value
        };
        c.text(f, value, x + w, base, 12., 400., INK, 0., Anchor::R);
    }
}
/// A number, its smaller `%` and an all-caps name, centred in a ring.
#[allow(clippy::too_many_arguments)]
fn ring_text(
    c: &mut Canvas,
    f: &CardFonts,
    cx: f32,
    cy: f32,
    num: &str,
    pct: bool,
    nsize: f32,
    label: &str,
) {
    let psize = nsize * 0.6;
    let cap_n = f.cap(nsize, 600.);
    let cap_l = f.cap(10., 500.);
    let gap = if nsize > 20. { 8. } else { 6. };
    let top = cy - (cap_n + gap + cap_l) / 2.;
    let base_n = top + cap_n;
    let base_l = base_n + gap + cap_l;
    let (l, nr, _, _) = f.ink(num, nsize, 600., 0.);
    let adv = f.width(num, nsize, 600., 0.);
    let ink_right = if pct {
        adv + 0.5 + f.ink("%", psize, 600., 0.).1
    } else {
        nr
    };
    let x0 = cx - (l + ink_right) / 2.;
    c.text(f, num, x0, base_n, nsize, 600., WHITE, 0., Anchor::L);
    if pct {
        c.text(
            f,
            "%",
            x0 + adv + 0.5,
            base_n,
            psize,
            600.,
            WHITE,
            0.,
            Anchor::L,
        );
    }
    c.text(f, label, cx, base_l, 10., 500., LABEL, 0.9, Anchor::M);
}
fn bars(c: &mut Canvas, x: f32, bottom: f32, w: f32, h: f32, data: &[(f64, f64)]) {
    if data.is_empty() {
        return;
    }
    let n = (((w + 1.) / 3.).floor() as usize).min(data.len()).max(1);
    let total_w = 3. * n as f32 - 1.;
    let x0 = x + (w - total_w);
    let peak = data.iter().map(|(a, b)| a + b).fold(0., f64::max).max(1.);
    for i in 0..n {
        // the newest bar sits on the right edge
        let (a, b) = data[data.len() - n + i];
        let ha = (((a / peak) as f32 * h * 2.).round() / 2.).max(if a > 0. { 0.5 } else { 0. });
        let hb = (((b / peak) as f32 * h * 2.).round() / 2.).max(if b > 0. { 0.5 } else { 0. });
        let bx = x0 + 3. * i as f32;
        c.rect(bx, bottom - ha, 2., ha, BLUE);
        c.rect(bx, bottom - ha - hb, 2., hb, PINK);
    }
}

fn rings(c: &mut Canvas, f: &CardFonts, x: f32, y: f32, w: f32, items: &[RingFact]) -> f32 {
    let col = (w - 2. * PAD) / items.len().max(1) as f32;
    let d = 80.;
    for (i, r) in items.iter().enumerate() {
        let cx = x + PAD + col * (i as f32 + 0.5);
        let cy = y + PAD + d / 2.;
        c.ring(cx, cy, d, 5., &[(r.share as f32, r.dot.rgba())]);
        ring_text(
            c,
            f,
            cx,
            cy,
            &format!("{}", (r.share * 100.).round() as i64),
            true,
            18.,
            &r.name,
        );
        let base = y + PAD + d + 6. + f.cap(11., 400.) + 3.;
        c.text(f, &r.caption, cx, base, 11., 400., LABEL, 0., Anchor::M);
    }
    PAD + d + 6. + 14. + PAD
}
#[allow(clippy::too_many_arguments)]
fn history(
    c: &mut Canvas,
    f: &CardFonts,
    x: f32,
    y: f32,
    w: f32,
    data: &[(f64, f64)],
    head: &str,
    rows: &[Legend],
) -> f32 {
    let (ix, iw) = (x + PAD, w - 2. * PAD);
    let mut cy = y + PAD;
    header(c, f, ix, cy, iw, "TOKENS PER REQUEST", head);
    cy += 24.;
    let bh = 48.;
    bars(c, ix, cy + bh, iw, bh, data);
    cy += bh + 8.;
    cy += legend(c, f, ix, cy, iw, rows);
    cy + PAD - y
}
#[allow(clippy::too_many_arguments)]
fn agents_card(
    c: &mut Canvas,
    f: &CardFonts,
    x: f32,
    y: f32,
    w: f32,
    slots: &[Slot],
    rows: &[Legend],
) -> f32 {
    let (ix, iw) = (x + PAD, w - 2. * PAD);
    let mut cy = y + PAD;
    header(c, f, ix, cy, iw, "AGENTS", "");
    cy += 24.;
    let per = if iw < 10. * 22. + 9. * 3. { 5 } else { 10 };
    let d = ((iw - (per as f32 - 1.) * 4.) / per as f32).clamp(22., 30.);
    let pitch = if per > 1 {
        (iw - d) / (per as f32 - 1.)
    } else {
        0.
    };
    let lines = slots.len().div_ceil(per).max(1);
    for (i, slot) in slots.iter().enumerate() {
        let (r, col) = (i / per, i % per);
        let cx = ix + d / 2. + pitch * col as f32;
        let cy2 = cy + r as f32 * (d + 6.) + d / 2.;
        match slot {
            Slot::Running => c.ring(cx, cy2, d, 2.5, &[(1., BLUE)]),
            Slot::Stalled => c.ring(cx, cy2, d, 2.5, &[(1., AMBER)]),
            Slot::Done => {
                c.ring(cx, cy2, d, 2.5, &[]);
                c.circle(cx, cy2, 3., GREEN);
            }
        }
    }
    cy += d * lines as f32 + 6. * (lines as f32 - 1.) + 8.;
    cy += legend(c, f, ix, cy, iw, rows);
    cy + PAD - y
}
#[allow(clippy::too_many_arguments)]
fn cache_card(
    c: &mut Canvas,
    f: &CardFonts,
    x: f32,
    y: f32,
    w: f32,
    share: f64,
    cold: bool,
    rows: &[Legend],
) -> f32 {
    let d: f32 = 72.;
    let (ix, iw) = (x + PAD, w - 2. * PAD);
    let h = PAD + d.max(18. * rows.len() as f32) + PAD;
    let (cx, cy) = (ix + d / 2., y + h / 2.);
    c.ring(
        cx,
        cy,
        d,
        5.,
        &[(share as f32, if cold { AMBER } else { BLUE })],
    );
    ring_text(
        c,
        f,
        cx,
        cy,
        &format!("{}", (share * 100.).round() as i64),
        true,
        18.,
        "CACHE",
    );
    let lx = ix + d + 16.;
    let top = cy - 9. * rows.len() as f32;
    legend(c, f, lx, top, iw - d - 16., rows);
    h
}
#[allow(clippy::too_many_arguments)]
fn dreams_card(
    c: &mut Canvas,
    f: &CardFonts,
    x: f32,
    y: f32,
    _w: f32,
    count: usize,
    share: f64,
    line: &str,
) -> f32 {
    let d = 30.;
    let h = PAD + d + PAD;
    let (cx, cy) = (x + PAD + d / 2., y + h / 2.);
    c.ring(cx, cy, d, 3., &[(share as f32, BLUE)]);
    c.text(
        f,
        &count.to_string(),
        cx,
        cy + f.cap(13., 600.) / 2.,
        13.,
        600.,
        WHITE,
        0.,
        Anchor::M,
    );
    let tx = x + PAD + d + 10.;
    c.text(
        f,
        "Dreams",
        tx,
        cy - 8. + f.cap(13., 500.) / 2.,
        13.,
        500.,
        INK,
        0.,
        Anchor::L,
    );
    c.text(
        f,
        line,
        tx,
        cy + 8. + f.cap(12., 400.) / 2.,
        12.,
        400.,
        LABEL,
        0.,
        Anchor::L,
    );
    h
}
#[allow(clippy::too_many_arguments)]
fn mirror_card(
    c: &mut Canvas,
    f: &CardFonts,
    x: f32,
    y: f32,
    w: f32,
    recalled: usize,
    noted: usize,
    up: &[f64],
    down: &[f64],
) -> f32 {
    let (ix, iw) = (x + PAD, w - 2. * PAD);
    let mut cy = y + PAD;
    let cap = f.cap(22., 600.);
    let half = iw / 2.;
    let figs = [
        (count_text(recalled), "RECALL IN", PINK),
        (count_text(noted), "NOTES OUT", BLUE),
    ];
    for (i, (v, label, col)) in figs.iter().enumerate() {
        let fx = ix + half * i as f32;
        c.text(f, v, fx, cy + cap, 22., 600., WHITE, 0., Anchor::L);
        let ly = cy + cap + 6. + 7.;
        c.circle(fx + 3.5, ly, 3.5, *col);
        c.text(
            f,
            label,
            fx + 12.,
            ly + f.cap(10., 500.) / 2.,
            10.,
            500.,
            LABEL,
            0.8,
            Anchor::L,
        );
    }
    cy += cap + 6. + 14. + 10.;
    let n = (((iw + 1.) / 3.).floor() as usize).max(1);
    let x0 = ix + (iw - (3. * n as f32 - 1.));
    let arm = 26.;
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
            c.rect(bx, mid - hu, 2., hu, PINK);
        }
        if d > 0. {
            let hd = (((d / bottom) as f32 * arm * 2.).round() / 2.).max(0.5);
            c.rect(bx, mid + 1., 2., hd, BLUE);
        }
    }
    c.rect(ix, mid, iw, 1., RULE);
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

fn draw_body(c: &mut Canvas, f: &CardFonts, card: &Card, x: f32, y: f32, w: f32) -> f32 {
    match card {
        Card::Rings(items) => rings(c, f, x, y, w, items),
        Card::History { bars, head, legend } => history(c, f, x, y, w, bars, head, legend),
        Card::Agents { slots, legend } => agents_card(c, f, x, y, w, slots, legend),
        Card::Cache {
            share,
            cold,
            legend,
        } => cache_card(c, f, x, y, w, *share, *cold, legend),
        Card::Dreams { count, share, line } => dreams_card(c, f, x, y, w, *count, *share, line),
        Card::Mirror {
            recalled,
            noted,
            up,
            down,
        } => mirror_card(c, f, x, y, w, *recalled, *noted, up, down),
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
        f,
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
    let w = width_pt(cols, cell) - 2. * MARGIN;
    let mut dry = Canvas::dry();
    let h = draw_body(&mut dry, f, card, MARGIN, 0., w);
    c.rrect(MARGIN, 0., w, h, 10., CARD);
    draw_body(&mut c, f, card, MARGIN, 0., w);
    Some(c.pm)
}
fn pill_width(p: &Pill, f: &CardFonts) -> f32 {
    let mut w = 0.;
    for (gi, g) in p.groups.iter().enumerate() {
        for (s, _, bold) in g {
            w += f.width(s, 13., if *bold { 600. } else { 500. }, 0.);
        }
        if gi + 1 < p.groups.len() {
            w += 11.;
        }
    }
    w + 2. * 12.
}
/// The pill's width in cells, one cell of air at its right.
pub fn pill_cols(p: &Pill, cell: (u32, u32), f: &CardFonts) -> usize {
    let cell_pt = cell.0 as f32 / S;
    ((pill_width(p, f) + 2. * MARGIN) / cell_pt).ceil() as usize + 1
}
/// The pill in a picture `cols` by `rows` cells.
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
    let (w, h) = (pill_width(p, f), 26.);
    let y = ((height_pt - h) / 2.).max(0.);
    c.rrect(MARGIN, y, w, h, 13., PILL);
    let base = y + h / 2. + f.cap(13., 600.) / 2.;
    let mut px = MARGIN + 12.;
    for (gi, g) in p.groups.iter().enumerate() {
        for (s, ink, bold) in g {
            px += c.text(
                f,
                s,
                px,
                base,
                13.,
                if *bold { 600. } else { 500. },
                ink.rgba(),
                0.,
                Anchor::L,
            );
        }
        if gi + 1 < p.groups.len() {
            px += 11.;
        }
    }
    Some(c.pm)
}

// ---------------------------------------------------------------- what the pane knows

/// The ring of a quota window, only while its reading is current.
fn quota_ring(q: &Value, now: f64) -> Option<RingFact> {
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
    Some(RingFact {
        share: (used / 100.).clamp(0., 1.),
        name: format!(
            "{code}{}",
            if super::text::ahead_of_pace(q) {
                "\u{bb}"
            } else {
                ""
            }
        ),
        caption,
        dot: match string(&q["sev"]) {
            "red" => Dot::Red,
            "amber" => Dot::Amber,
            _ => Dot::Blue,
        },
    })
}
fn ctx_ring(g: &Value) -> Option<RingFact> {
    let tokens = g["ctx"]["tokens"].as_f64().filter(|n| *n > 0.)?;
    let size = g["ctx"]["size"].as_f64().filter(|n| *n > 0.)?;
    Some(RingFact {
        share: (tokens / size).clamp(0., 1.),
        name: "CTX".into(),
        caption: format!("{} / {}", short(tokens), short(size)),
        dot: match string(&g["ctx"]["sev"]) {
            "red" => Dot::Red,
            "amber" => Dot::Amber,
            _ => Dot::Blue,
        },
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
/// The dreams this session has: its count, the share of the chapter's asks used, the words.
pub fn dream_facts(data: &Value, sid: &str) -> Option<(usize, f64, String)> {
    if data["dreams_on"] == false {
        return None;
    }
    let mine = array(&data["dreams"])
        .iter()
        .filter(|d| d["session"] == sid)
        .count();
    if mine == 0 {
        return None;
    }
    let fed = dreams::feed(array(&data["dreams"]), &data["chapter"], sid);
    let unread = usize::from(!fed["star"].is_null());
    let of = fed["of"].as_f64().unwrap_or(0.);
    let left = fed["left"].as_f64().unwrap_or(0.);
    let share = if of > 0. {
        ((of - left) / of).clamp(0., 1.)
    } else {
        0.
    };
    Some((
        mine,
        share,
        format!("{unread} unread \u{b7} {} left", left as i64),
    ))
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
    let mut rings: Vec<RingFact> = vec![];
    rings.extend(ctx_ring(g));
    let quotas = array(&g["quotas"]);
    for code in ["5H", "7D"] {
        rings.extend(
            quotas
                .iter()
                .find(|q| q["code"] == code)
                .and_then(|q| quota_ring(q, now)),
        );
    }
    if !rings.is_empty() {
        out.push(Card::Rings(rings));
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
    if let Some((count, share, line)) = dream_facts(data, string(&s["id"])) {
        out.push(Card::Dreams { count, share, line });
    }
    out.extend(mirror_card_of(s, handed, now));
    out
}
/// The collapsed masthead's groups; `None` when it has nothing to say (the text rows stay).
pub fn pill_of(g: &Value, dreams: Option<usize>) -> Option<Pill> {
    let sev_ink = |sev: &str| match sev {
        "red" => Ink::Red,
        "amber" => Ink::Amber,
        _ => Ink::White,
    };
    let mut groups: Vec<Vec<Run>> = vec![];
    if let Some(r) = ctx_ring(g) {
        groups.push(vec![
            ("CTX ".into(), Ink::Dim, false),
            (
                format!("{}%", (r.share * 100.).round() as i64),
                sev_ink(string(&g["ctx"]["sev"])),
                true,
            ),
        ]);
    }
    if let Some(q) = array(&g["quotas"]).iter().find(|q| q["code"] == "5H") {
        if let Some(used) = q["used"].as_f64() {
            groups.push(vec![
                ("5h ".into(), Ink::Dim, false),
                (
                    format!(
                        "{}%{}",
                        used.round() as i64,
                        if super::text::ahead_of_pace(q) {
                            "\u{bb}"
                        } else {
                            ""
                        }
                    ),
                    sev_ink(string(&q["sev"])),
                    true,
                ),
            ]);
        }
    }
    let pay = string(&g["payback"]["text"]);
    if truthy(&g["payback"]["text"]) && !truthy(&g["compacting"]) {
        let words: Vec<&str> = pay
            .trim_start_matches('\u{267b}')
            .split_whitespace()
            .collect();
        if let Some(n) = words.first() {
            let unit = match words.get(1).copied().unwrap_or("") {
                u if u.starts_with("prompt") => "p",
                u if u.starts_with("call") => "r",
                _ => "",
            };
            groups.push(vec![(
                format!("\u{267b} {}{unit}", n.trim_start_matches('~')),
                Ink::Green,
                true,
            )]);
        }
    }
    if let Some(n) = dreams.filter(|n| *n > 0) {
        groups.push(vec![(format!("\u{263e} {n}"), Ink::Lavender, true)]);
    }
    (!groups.is_empty()).then_some(Pill { groups })
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
    #[test]
    fn a_pill_says_only_what_it_measured() {
        assert!(pill_of(&json!({}), None).is_none());
        let g = json!({
            "ctx": {"tokens": 104000., "size": 200000., "sev": "normal"},
            "quotas": [{"code": "5H", "used": 31., "sev": "normal", "pace": null}],
            "payback": {"text": "\u{267b} ~3 calls"},
        });
        let p = pill_of(&g, Some(2)).unwrap();
        let said: Vec<String> = p
            .groups
            .iter()
            .map(|g| g.iter().map(|r| r.0.as_str()).collect::<String>())
            .collect();
        assert_eq!(said, ["CTX 52%", "5h 31%", "\u{267b} 3r", "\u{263e} 2"]);
        assert_eq!(pill_of(&g, None).unwrap().groups.len(), 3);
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
        let ring = |name: &str| RingFact {
            share: 0.5,
            name: name.into(),
            caption: "104k / 200k".into(),
            dot: Dot::Blue,
        };
        let cards = [
            Card::Rings(vec![ring("CTX"), ring("5H"), ring("7D\u{bb}")]),
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
            Card::Dreams {
                count: 2,
                share: 0.4,
                line: "1 unread \u{b7} 3 left".into(),
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
            &serde_json::json!({"ctx": {"tokens": 1., "size": 2.}}),
            Some(2),
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

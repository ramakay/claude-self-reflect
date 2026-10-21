// Ported from the approved Instrument prototype drawing code.
// Shared drawing machinery: fonts (sans / serif / mono, variable axes), canvas with
// rounded rects, gradients, glass, thin rings with round caps, dotted rings,
// antialiased text with baseline snapping, ink measurement, wrapping, contact sheet.
#![allow(dead_code, clippy::too_many_arguments)]
use super::{instrument_icons, theme};
use ab_glyph::{point, Font, FontVec, GlyphId, ScaleFont, VariableFont};
use image::{codecs::png::PngEncoder, ImageEncoder};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use tiny_skia::{
    Color, FillRule, GradientStop, IntSize, LineCap, LinearGradient, Paint, Path, PathBuilder,
    Pixmap, PixmapPaint, Point, RadialGradient, Rect, Shader, SpreadMode, Stroke, Transform,
};

pub const S: f32 = 2.0; // retina: 1 pt = 2 px
pub const W: f32 = 605.; // pane width in pt (1210 px)
pub const HMAX: f32 = 1135.; // 2270 px

pub type Rgba = [u8; 4];
pub const fn hex(v: u32) -> Rgba {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8, 255]
}
pub fn al(c: Rgba, a: f32) -> Rgba {
    [c[0], c[1], c[2], (a.clamp(0., 1.) * 255.).round() as u8]
}
pub fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2]), m(a[3], b[3])]
}
fn color(c: Rgba) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], c[3])
}

// ---------------------------------------------------------------- fonts

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Fam {
    Sans,
    Serif,
    Mono,
    Icon,
}

#[derive(Clone, Copy)]
pub struct St {
    pub fam: Fam,
    pub size: f32,
    pub wght: f32,
    pub track: f32,
}
pub fn sans(size: f32, wght: f32) -> St {
    St {
        fam: Fam::Sans,
        size,
        wght,
        track: 0.,
    }
}
pub fn serif(size: f32, wght: f32) -> St {
    St {
        fam: Fam::Serif,
        size,
        wght,
        track: 0.,
    }
}
pub fn mono(size: f32, wght: f32) -> St {
    St {
        fam: Fam::Mono,
        size,
        wght,
        track: 0.,
    }
}
pub fn icon(size: f32) -> St {
    St {
        fam: Fam::Icon,
        size,
        wght: 400.,
        track: 0.,
    }
}

impl St {
    pub fn tr(mut self, t: f32) -> St {
        self.track = t;
        self
    }
}

type InkKey = (String, u8, u32, u32, u32);
type InkBox = (f32, f32, f32, f32);
struct Face {
    fam: Fam,
    wght: f32,
    opsz: f32,
    font: FontVec,
    digits: [GlyphId; 10],
}
pub struct Fonts {
    faces: Vec<Face>,
    fallback: Vec<(String, FontVec)>,
    pub used: RefCell<BTreeMap<char, String>>,
    pub notes: Vec<String>,
    pub names: BTreeMap<&'static str, String>,
    ink_cache: RefCell<HashMap<InkKey, InkBox>>,
}
struct Placed<'a> {
    font: &'a FontVec,
    scale: f32,
    id: GlyphId,
    x: f32,
    y: f32,
}
// ttf-parser returns integer bounds truncated toward zero, but variable outlines
// retain fractional extrema. A clipped edge makes the rasterizer leak coverage
// across the glyph box. Enclose the actual curves before allocating that box.
fn outlined(font: &FontVec, glyph: ab_glyph::Glyph) -> Option<ab_glyph::OutlinedGlyph> {
    let mut outline = font.outline(glyph.id)?;
    let mut include = |p: ab_glyph::Point| {
        outline.bounds.min.x = outline.bounds.min.x.min(p.x.floor());
        outline.bounds.max.x = outline.bounds.max.x.max(p.x.ceil());
        outline.bounds.min.y = outline.bounds.min.y.max(p.y.ceil());
        outline.bounds.max.y = outline.bounds.max.y.min(p.y.floor());
    };
    for curve in &outline.curves {
        use ab_glyph::OutlineCurve::*;
        match *curve {
            Line(a, b) => {
                include(a);
                include(b);
            }
            Quad(a, b, c) => {
                include(a);
                include(b);
                include(c);
            }
            Cubic(a, b, c, d) => {
                include(a);
                include(b);
                include(c);
                include(d);
            }
        }
    }
    let scale = font.as_scaled(glyph.scale).scale_factor();
    Some(ab_glyph::OutlinedGlyph::new(glyph, outline, scale))
}

fn px_scale(f: &FontVec, size_px: f32) -> f32 {
    size_px * f.height_unscaled() / f.units_per_em().unwrap_or(1000.)
}
fn axes(f: &FontVec) -> String {
    f.variations()
        .iter()
        .map(|a| {
            format!(
                "{} {}..{}",
                String::from_utf8_lossy(&a.tag),
                a.min_value,
                a.max_value
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

const BUNDLED_FACES: [(Fam, &str, &[u8]); 4] = [
    (
        Fam::Sans,
        "Inter Variable",
        include_bytes!("../../../assets/fonts/InterVariable-subset.ttf"),
    ),
    (
        Fam::Serif,
        "Source Serif 4 Variable",
        include_bytes!("../../../assets/fonts/SourceSerif4Variable-subset.ttf"),
    ),
    (
        Fam::Mono,
        "JetBrains Mono",
        include_bytes!("../../../assets/fonts/JetBrainsMono-Regular-subset.ttf"),
    ),
    (
        Fam::Icon,
        "Lucide",
        include_bytes!("../../../assets/fonts/lucide-subset.ttf"),
    ),
];

/// ab_glyph rasterizes outlines but does not shape GSUB features. Resolve the bundled fonts'
/// single substitutions for lining/tabular figures explicitly, preserving their real advances.
fn tabular_digits(bytes: &[u8]) -> [GlyphId; 10] {
    use ttf_parser::{
        gsub::{SingleSubstitution, SubstitutionSubtable},
        Tag,
    };
    let face = ttf_parser::Face::parse(bytes, 0).expect("bundled font tables");
    let mut digits = std::array::from_fn(|i| {
        face.glyph_index(char::from(b'0' + i as u8))
            .unwrap_or(ttf_parser::GlyphId(0))
    });
    if let Some(gsub) = face.tables().gsub {
        for tag in [b"lnum", b"tnum"] {
            if let Some(feature) = gsub.features.find(Tag::from_bytes(tag)) {
                for index in feature.lookup_indices {
                    if let Some(lookup) = gsub.lookups.get(index) {
                        for table in lookup.subtables.into_iter::<SubstitutionSubtable<'_>>() {
                            if let SubstitutionSubtable::Single(single) = table {
                                for glyph in &mut digits {
                                    if let Some(index) = single.coverage().get(*glyph) {
                                        *glyph = match single {
                                            SingleSubstitution::Format1 { delta, .. } => {
                                                ttf_parser::GlyphId(
                                                    glyph.0.wrapping_add(delta as u16),
                                                )
                                            }
                                            SingleSubstitution::Format2 { substitutes, .. } => {
                                                substitutes.get(index).unwrap_or(*glyph)
                                            }
                                        };
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    digits.map(|g| GlyphId(g.0))
}

impl Fonts {
    /// The complete primary font set. This path never reads the filesystem.
    pub fn bundled() -> Fonts {
        let mut notes = vec![];
        let mut names = BTreeMap::new();
        let mut faces = vec![];
        for (fam, name, bytes) in BUNDLED_FACES {
            let probe = FontVec::try_from_vec(bytes.to_vec()).expect("bundled font");
            notes.push(format!("{name} axes: {}", axes(&probe)));
            names.insert(
                match fam {
                    Fam::Sans => "sans",
                    Fam::Serif => "serif",
                    Fam::Mono => "mono",
                    Fam::Icon => "icon",
                },
                name.to_string(),
            );
            let digits = tabular_digits(bytes);
            let weights: &[f32] = if matches!(fam, Fam::Mono | Fam::Icon) {
                &[400.]
            } else {
                &[400., 500., 600.]
            };
            let sizes: &[f32] = if matches!(fam, Fam::Mono | Fam::Icon) {
                &[14.]
            } else {
                &[
                    8., 9., 10., 10.5, 11., 11.5, 12., 12.5, 13., 14., 16., 18., 20., 24., 28.,
                    30., 32., 36., 40., 44., 48., 60.,
                ]
            };
            for &wght in weights {
                for &opsz in sizes {
                    let mut font = FontVec::try_from_vec(bytes.to_vec()).expect("bundled font");
                    if !matches!(fam, Fam::Mono | Fam::Icon) {
                        assert!(font.set_variation(b"wght", wght));
                        assert!(font.set_variation(b"opsz", opsz));
                    }
                    faces.push(Face {
                        fam,
                        wght,
                        opsz,
                        font,
                        digits,
                    });
                }
            }
        }
        Fonts {
            faces,
            fallback: vec![],
            used: RefCell::new(BTreeMap::new()),
            notes,
            names,
            ink_cache: RefCell::new(HashMap::new()),
        }
    }

    pub fn load() -> Fonts {
        let mut fonts = Self::bundled();
        // System faces are used only if none of the bundled faces covers a character.
        for p in [
            "/System/Library/Fonts/SFNS.ttf",
            "/System/Library/Fonts/NewYork.ttf",
            "/System/Library/Fonts/SFNSMono.ttf",
            "/System/Library/Fonts/Apple Symbols.ttf",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        ] {
            if let Ok(b) = std::fs::read(p) {
                if let Ok(f) = FontVec::try_from_vec(b) {
                    fonts.fallback.push((p.to_string(), f));
                }
            }
        }
        fonts
    }

    fn face(&self, st: St) -> &Face {
        let want_o = if st.fam == Fam::Serif && st.wght >= 600. {
            st.size.clamp(32., 48.)
        } else {
            st.size
        };
        self.faces
            .iter()
            .filter(|f| f.fam == st.fam)
            .min_by(|a, b| {
                let score = |f: &Face| (f.wght - st.wght).abs() * 10. + (f.opsz - want_o).abs();
                score(a).total_cmp(&score(b))
            })
            .expect("font family")
    }

    pub fn bundled_has(&self, ch: char) -> bool {
        ch.is_whitespace() || self.faces.iter().any(|f| f.font.glyph_id(ch).0 != 0)
    }

    fn place(&self, s: &str, st: St, mult: f32) -> (Vec<Placed<'_>>, f32) {
        let primary = self.face(st);
        let main = &primary.font;
        let size_px = st.size * S * mult;
        let mut out = vec![];
        let mut x = 0.;
        let mut prev: Option<(*const FontVec, GlyphId)> = None;
        let mut previous_icon = false;
        for original in s.chars() {
            let ch = instrument_icons::legacy(original);
            let is_icon = instrument_icons::is_icon(ch);
            let mut font = if is_icon {
                &self.face(icon(st.size)).font
            } else {
                main
            };
            let mut id = if ch.is_ascii_digit() && primary.digits[ch as usize - '0' as usize].0 != 0
            {
                primary.digits[ch as usize - '0' as usize]
            } else {
                font.glyph_id(ch)
            };
            if id.0 == 0 && !ch.is_whitespace() {
                if let Some(face) = self.faces.iter().find(|face| face.font.glyph_id(ch).0 != 0) {
                    font = &face.font;
                    id = font.glyph_id(ch);
                }
            }
            if id.0 == 0 && !ch.is_whitespace() {
                let mut found = false;
                for (name, f) in &self.fallback {
                    let g = f.glyph_id(ch);
                    if g.0 != 0 {
                        font = f;
                        id = g;
                        self.used.borrow_mut().insert(ch, name.clone());
                        found = true;
                        break;
                    }
                }
                if !found {
                    self.used.borrow_mut().insert(ch, "MISSING".into());
                }
            }
            let inline_icon = is_icon && st.fam != Fam::Icon;
            let glyph_size = if inline_icon { 12. * S * mult } else { size_px };
            let scale = px_scale(font, glyph_size);
            let sf = font.as_scaled(scale);
            let y = if inline_icon {
                let cap = outlined(main, main.glyph_id('H').with_scale(px_scale(main, size_px)))
                    .map(|o| -o.px_bounds().min.y)
                    .unwrap_or(size_px * 0.7);
                let middle = outlined(font, id.with_scale(scale))
                    .map(|o| (o.px_bounds().min.y + o.px_bounds().max.y) / 2.)
                    .unwrap_or(0.);
                -cap / 2. - middle
            } else {
                0.
            };
            if let Some((pf, pid)) = prev {
                if std::ptr::eq(pf, font) && !ch.is_ascii_digit() {
                    x += sf.kern(pid, id);
                }
            }
            out.push(Placed {
                font,
                scale,
                id,
                x,
                y,
            });
            x += if previous_icon && ch == ' ' {
                theme::ICON_GAP * S * mult
            } else {
                sf.h_advance(id) + st.track * S * mult
            };
            previous_icon = is_icon;
            prev = (!ch.is_ascii_digit()).then_some((font as *const FontVec, id));
        }
        (out, x)
    }

    /// advance width in pt (without trailing tracking)
    pub fn width(&self, s: &str, st: St) -> f32 {
        let (_, a) = self.place(s, st, 1.);
        if s.is_empty() {
            0.
        } else {
            a / S - st.track
        }
    }

    /// ink box (left, right, top, bottom) in pt relative to pen origin / baseline (y down).
    pub fn ink(&self, s: &str, st: St) -> (f32, f32, f32, f32) {
        let key = (
            s.to_string(),
            st.fam as u8,
            st.size.to_bits(),
            st.wght.to_bits(),
            st.track.to_bits(),
        );
        if let Some(v) = self.ink_cache.borrow().get(&key) {
            return *v;
        }
        let (g, _) = self.place(s, st, 16.);
        let (mut l, mut r, mut t, mut b) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for p in &g {
            let glyph = p.id.with_scale_and_position(p.scale, point(p.x, p.y));
            if let Some(o) = outlined(p.font, glyph) {
                let bb = o.px_bounds();
                l = l.min(bb.min.x);
                r = r.max(bb.max.x);
                t = t.min(bb.min.y);
                b = b.max(bb.max.y);
            }
        }
        let v = if l == f32::MAX {
            (0., 0., 0., 0.)
        } else {
            (l / (S * 16.), r / (S * 16.), t / (S * 16.), b / (S * 16.))
        };
        self.ink_cache.borrow_mut().insert(key, v);
        v
    }

    pub fn cap(&self, st: St) -> f32 {
        -self.ink("H", st).2
    }
    pub fn xh(&self, st: St) -> f32 {
        -self.ink("x", st).2
    }

    /// shorten with an ellipsis so the advance fits `maxw` pt
    pub fn fit(&self, s: &str, st: St, maxw: f32) -> String {
        if self.width(s, st) <= maxw {
            return s.to_string();
        }
        // cut at a word boundary first
        let words: Vec<&str> = s.split(' ').collect();
        for k in (1..words.len()).rev() {
            let mut t = words[..k].join(" ");
            t = t
                .trim_end_matches([' ', ',', '.', '(', ':', ';', '\u{b7}'])
                .to_string();
            t.push('\u{2026}');
            if self.width(&t, st) <= maxw {
                return t;
            }
        }
        let chars: Vec<char> = s.chars().collect();
        let mut n = chars.len();
        while n > 0 {
            let mut t: String = chars[..n].iter().collect();
            t = t
                .trim_end_matches([' ', ',', '.', '(', '\u{b7}'])
                .to_string();
            t.push('\u{2026}');
            if self.width(&t, st) <= maxw {
                return t;
            }
            n -= 1;
        }
        "\u{2026}".into()
    }

    /// wrap to at most `n` lines; the last line is cut at a word boundary with an ellipsis
    pub fn wrap_max(&self, s: &str, st: St, maxw: f32, n: usize) -> Vec<String> {
        let lines = self.wrap(s, st, maxw);
        if lines.len() <= n {
            return lines;
        }
        let mut out: Vec<String> = lines[..n - 1].to_vec();
        let rest = lines[n - 1..].join(" ");
        out.push(self.fit(&rest, st, maxw));
        out
    }

    /// Balance headline lines without changing or truncating the words.
    pub fn wrap_balanced(&self, text: &str, st: St, maxw: f32) -> Vec<String> {
        let words: Vec<_> = text.split_whitespace().collect();
        if words.is_empty() {
            return vec![];
        }
        let n = words.len();
        let mut costs = vec![f32::INFINITY; n + 1];
        let mut next = vec![n; n];
        costs[n] = 0.;
        for start in (0..n).rev() {
            for end in start + 1..=n {
                let line = words[start..end].join(" ");
                let width = self.width(&line, st);
                if width > maxw && end > start + 1 {
                    break;
                }
                let orphan = if end == n && end - start == 1 && start > 0 {
                    maxw * maxw * 10.
                } else {
                    0.
                };
                let cost = (maxw - width).powi(2) + orphan + costs[end];
                if cost < costs[start] {
                    costs[start] = cost;
                    next[start] = end;
                }
            }
        }
        let mut lines = vec![];
        let mut start = 0;
        while start < n {
            let end = next[start];
            lines.push(words[start..end].join(" "));
            start = end;
        }
        lines
    }

    /// greedy word wrap
    pub fn wrap(&self, s: &str, st: St, maxw: f32) -> Vec<String> {
        let mut lines = vec![];
        let mut cur = String::new();
        for w in s.split(' ') {
            let cand = if cur.is_empty() {
                w.to_string()
            } else {
                format!("{cur} {w}")
            };
            if self.width(&cand, st) <= maxw || cur.is_empty() {
                cur = cand;
            } else {
                lines.push(cur);
                cur = w.to_string();
            }
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
        lines
    }
}

// ---------------------------------------------------------------- canvas

#[derive(Clone, Debug, serde::Serialize)]
pub struct QuotaGauge {
    pub code: String,
    pub center: (f32, f32),
    pub radius: f32,
    /// Fractions: end of other usage, start of CSR, end of CSR.
    pub csr_segment: Option<(f32, f32, f32)>,
}

pub struct Canvas {
    pub pm: Pixmap,
    pub dry: bool,
    /// Final output pixels per logical point (before supersampling/downsampling).
    pub output_scale: f32,
    pub quota_gauges: Vec<QuotaGauge>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum A {
    L,
    M,
    R,
}

pub fn paint(c: Rgba) -> Paint<'static> {
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
pub fn rrect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
    let r = r.min(w / 2.).min(h / 2.).max(0.);
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
    p.finish().unwrap()
}
/// arc clockwise; angles in degrees clockwise from 12 o'clock.
pub fn arc_path(cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Option<Path> {
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
pub fn on_ring(cx: f32, cy: f32, r: f32, deg: f32) -> (f32, f32) {
    let t = deg.to_radians();
    (cx + r * t.sin(), cy - r * t.cos())
}
pub fn stops(s: &[(f32, Rgba)]) -> Vec<GradientStop> {
    s.iter()
        .map(|(p, c)| GradientStop::new(*p, color(*c)))
        .collect()
}
pub fn lin(x0: f32, y0: f32, x1: f32, y1: f32, s: &[(f32, Rgba)]) -> Shader<'static> {
    LinearGradient::new(
        Point::from_xy(x0, y0),
        Point::from_xy(x1, y1),
        stops(s),
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap()
}
/// radial gradient; (sx, sy) stretches the circle into an ellipse around the centre.
pub fn radial(cx: f32, cy: f32, r: f32, sx: f32, sy: f32, s: &[(f32, Rgba)]) -> Shader<'static> {
    let t = Transform::from_row(sx, 0., 0., sy, cx * (1. - sx), cy * (1. - sy));
    RadialGradient::new(
        Point::from_xy(cx, cy),
        0.,
        Point::from_xy(cx, cy),
        r,
        stops(s),
        SpreadMode::Pad,
        t,
    )
    .unwrap()
}

type ShadowKey = (u32, u32, u32, u32);
struct ShadowMask {
    width: usize,
    height: usize,
    pad: i32,
    alpha: Vec<u8>,
}
type ShadowCache = Vec<(ShadowKey, std::sync::Arc<ShadowMask>)>;
static SHADOW_MASKS: std::sync::LazyLock<std::sync::Mutex<ShadowCache>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

/// Three separable box passes approximate a Gaussian. Sliding sums make each pass O(pixels).
fn blur_alpha(alpha: &mut [u8], width: usize, height: usize, radius: usize) {
    if radius == 0 {
        return;
    }
    let mut temp = vec![0; alpha.len()];
    let divisor = (2 * radius + 1) as u32;
    for _ in 0..3 {
        for y in 0..height {
            let row = y * width;
            let mut sum: u32 = (0..=radius.min(width - 1))
                .map(|x| u32::from(alpha[row + x]))
                .sum();
            for x in 0..width {
                temp[row + x] = ((sum + divisor / 2) / divisor) as u8;
                if x >= radius {
                    sum -= u32::from(alpha[row + x - radius]);
                }
                if x + radius + 1 < width {
                    sum += u32::from(alpha[row + x + radius + 1]);
                }
            }
        }
        for x in 0..width {
            let mut sum: u32 = (0..=radius.min(height - 1))
                .map(|y| u32::from(temp[y * width + x]))
                .sum();
            for y in 0..height {
                alpha[y * width + x] = ((sum + divisor / 2) / divisor) as u8;
                if y >= radius {
                    sum -= u32::from(temp[(y - radius) * width + x]);
                }
                if y + radius + 1 < height {
                    sum += u32::from(temp[(y + radius + 1) * width + x]);
                }
            }
        }
    }
}

fn shadow_mask(w: f32, h: f32, corner: f32, blur: f32) -> std::sync::Arc<ShadowMask> {
    let key = (
        (w * S).round() as u32,
        (h * S).round() as u32,
        (corner * S).round() as u32,
        (blur * S / 2.).round() as u32,
    );
    let mut cache = SHADOW_MASKS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(i) = cache.iter().position(|(k, _)| *k == key) {
        let entry = cache.remove(i);
        let result = entry.1.clone();
        cache.push(entry);
        return result;
    }
    let pad = key.3 * 3 + 1;
    let (width, height) = (key.0 + pad * 2, key.1 + pad * 2);
    let mut mask = Pixmap::new(width, height).expect("card shadow mask");
    let path = rrect_path(
        pad as f32,
        pad as f32,
        key.0 as f32,
        key.1 as f32,
        key.2 as f32,
    );
    mask.fill_path(
        &path,
        &paint(theme::OPAQUE_WHITE),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    let mut alpha = mask
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p[3])
        .collect::<Vec<_>>();
    blur_alpha(&mut alpha, width as usize, height as usize, key.3 as usize);
    let result = std::sync::Arc::new(ShadowMask {
        width: width as usize,
        height: height as usize,
        pad: pad as i32,
        alpha,
    });
    if cache.len() >= 32 {
        cache.remove(0);
    }
    cache.push((key, result.clone()));
    result
}

impl Canvas {
    pub fn new(w_pt: f32, h_pt: f32) -> Canvas {
        Canvas {
            pm: Pixmap::new((w_pt * S).round() as u32, (h_pt * S).round() as u32).unwrap(),
            dry: false,
            output_scale: S,
            quota_gauges: Vec::new(),
        }
    }
    pub fn dry() -> Canvas {
        Canvas {
            pm: Pixmap::new(1, 1).unwrap(),
            dry: true,
            output_scale: S,
            quota_gauges: Vec::new(),
        }
    }
    pub fn fill(&mut self, p: &Path, col: Rgba) {
        if !self.dry {
            self.pm
                .fill_path(p, &paint(col), FillRule::Winding, sc(), None);
        }
    }
    pub fn fill_shader(&mut self, p: &Path, sh: Shader<'static>) {
        if self.dry {
            return;
        }
        let pt = Paint {
            shader: sh,
            anti_alias: true,
            ..Paint::default()
        };
        self.pm.fill_path(p, &pt, FillRule::Winding, sc(), None);
    }
    pub fn rrect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, col: Rgba) {
        self.fill(&rrect_path(x, y, w, h, r), col);
    }
    pub fn rrect_sh(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, sh: Shader<'static>) {
        self.fill_shader(&rrect_path(x, y, w, h, r), sh);
    }
    /// hairline rounded outline (1 device px)
    pub fn rrect_line(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, col: Rgba, width: f32) {
        let o = width / 2.;
        self.stroke(
            &rrect_path(x + o, y + o, w - 2. * o, h - 2. * o, r),
            width,
            col,
            false,
        );
    }
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, col: Rgba) {
        if self.dry {
            return;
        }
        if let Some(r) = Rect::from_xywh(x * S, y * S, w * S, h * S) {
            self.pm
                .fill_rect(r, &paint(col), Transform::identity(), None);
        }
    }
    /// 1 device px line, snapped
    pub fn hair(&mut self, x: f32, y: f32, w: f32, col: Rgba) {
        if self.dry {
            return;
        }
        let py = (y * S).round();
        if let Some(r) = Rect::from_xywh(x * S, py, w * S, 1.) {
            self.pm
                .fill_rect(r, &paint(col), Transform::identity(), None);
        }
    }
    pub fn vhair(&mut self, x: f32, y: f32, h: f32, col: Rgba) {
        if self.dry {
            return;
        }
        let px = (x * S).round();
        if let Some(r) = Rect::from_xywh(px, y * S, 1., h * S) {
            self.pm
                .fill_rect(r, &paint(col), Transform::identity(), None);
        }
    }
    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, col: Rgba) {
        if let Some(p) = PathBuilder::from_circle(cx, cy, r) {
            self.fill(&p, col);
        }
    }
    pub fn stroke(&mut self, p: &Path, w: f32, col: Rgba, round: bool) {
        if self.dry {
            return;
        }
        let st = Stroke {
            width: w,
            line_cap: if round { LineCap::Round } else { LineCap::Butt },
            ..Stroke::default()
        };
        self.pm.stroke_path(p, &paint(col), &st, sc(), None);
    }
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, w: f32, col: Rgba) {
        let mut pb = PathBuilder::new();
        pb.move_to(x0, y0);
        pb.line_to(x1, y1);
        if let Some(p) = pb.finish() {
            self.stroke(&p, w, col, true);
        }
    }
    /// arc from fraction f0 to f1 of a full turn, round caps
    pub fn arc(
        &mut self,
        cx: f32,
        cy: f32,
        r: f32,
        f0: f32,
        f1: f32,
        w: f32,
        col: Rgba,
        round: bool,
    ) {
        if f1 - f0 <= 0.0005 {
            return;
        }
        if f1 - f0 >= 0.9995 {
            if let Some(p) = PathBuilder::from_circle(cx, cy, r) {
                self.stroke(&p, w, col, false);
            }
            return;
        }
        if let Some(p) = arc_path(cx, cy, r, f0 * 360., f1 * 360.) {
            self.stroke(&p, w, col, round);
        }
    }
    /// thin ring: muted track, then (from, to, colour) arcs
    pub fn ring(
        &mut self,
        cx: f32,
        cy: f32,
        r: f32,
        w: f32,
        track: Rgba,
        segs: &[(f32, f32, Rgba)],
    ) {
        self.arc(cx, cy, r, 0., 1., w, track, false);
        for (a, b, col) in segs {
            self.arc(cx, cy, r, *a, *b, w, *col, true);
        }
    }
    /// dots along a ring; `lit(i)` returns the colour of dot i
    pub fn dots(
        &mut self,
        cx: f32,
        cy: f32,
        r: f32,
        n: usize,
        dot_r: f32,
        lit: &dyn Fn(usize) -> Rgba,
    ) {
        for i in 0..n {
            let (x, y) = on_ring(cx, cy, r, 360. * i as f32 / n as f32);
            self.circle(x, y, dot_r, lit(i));
        }
    }
    /// radial tick on a ring at fraction f
    pub fn tick(&mut self, cx: f32, cy: f32, r0: f32, r1: f32, f: f32, w: f32, col: Rgba) {
        let (x0, y0) = on_ring(cx, cy, r0, f * 360.);
        let (x1, y1) = on_ring(cx, cy, r1, f * 360.);
        self.line(x0, y0, x1, y1, w, col);
    }

    /// The mesh already varies over hundreds of points, so no per-card backdrop blur is needed.
    pub fn glass(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, tint: Shader<'static>) {
        self.rrect_sh(x, y, w, h, r, tint);
    }

    pub fn background(&mut self, t: &theme::Tokens) {
        let (w, h) = (self.pm.width() as f32 / S, self.pm.height() as f32 / S);
        self.pm.fill(color(t.mesh_ground));
        let full = rrect_path(0., 0., w, h, 0.);
        let diagonal = w.hypot(h);
        for ((x, y, radius), stops) in [
            (-0.15 * w, -0.10 * h, 0.9 * diagonal),
            (1.15 * w, 0.45 * h, diagonal),
            (0.45 * w, 1.10 * h, 1.1 * diagonal),
        ]
        .into_iter()
        .zip(t.aurora)
        {
            self.fill_shader(&full, radial(x, y, radius, 1., 1., stops));
        }
    }

    /// Outer shadows are clipped to the pixmap, whose height excludes the terminal key row.
    pub fn shadow(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, shadow: theme::Shadow) {
        if self.dry {
            return;
        }
        let mask = shadow_mask(w, h, r, shadow.blur);
        let ox = (x * S).round() as i32 - mask.pad;
        let oy = ((y + shadow.y) * S).round() as i32 - mask.pad;
        let (pw, ph) = (self.pm.width() as i32, self.pm.height() as i32);
        let data = self.pm.data_mut();
        for my in 0..mask.height {
            let py = oy + my as i32;
            if py < 0 || py >= ph {
                continue;
            }
            for mx in 0..mask.width {
                let px = ox + mx as i32;
                if px < 0 || px >= pw {
                    continue;
                }
                // CSS-style outer shadow: don't darken the glass's interior backdrop.
                let lx = (px as f32 + 0.5) / S - x;
                let ly = (py as f32 + 0.5) / S - y;
                if lx >= 0. && ly >= 0. && lx <= w && ly <= h {
                    let dx = (r - lx).max(lx - (w - r)).max(0.);
                    let dy = (r - ly).max(ly - (h - r)).max(0.);
                    if dx * dx + dy * dy <= r * r {
                        continue;
                    }
                }
                let a = f32::from(mask.alpha[my * mask.width + mx]) / 255.
                    * f32::from(shadow.color[3])
                    / 255.;
                if a <= 0. {
                    continue;
                }
                let i = ((py * pw + px) * 4) as usize;
                for k in 0..3 {
                    data[i + k] = (f32::from(shadow.color[k]) * a
                        + f32::from(data[i + k]) * (1. - a))
                        .round() as u8;
                }
            }
        }
    }

    /// Draw text; `base` is the baseline in pt. Returns advance in pt.
    pub fn text(
        &mut self,
        f: &Fonts,
        s: &str,
        x: f32,
        base: f32,
        st: St,
        col: Rgba,
        anchor: A,
    ) -> f32 {
        let (glyphs, adv) = f.place(s, st, 1.);
        let adv_pt = if s.is_empty() { 0. } else { adv / S - st.track };
        if self.dry {
            return adv_pt;
        }
        let ox = match anchor {
            A::L => (x * S).round(),
            A::R => (x * S - adv_pt * S).round(),
            A::M => {
                let (l, r, _, _) = f.ink(s, st);
                (x * S - (l + r) / 2. * S).round()
            }
        };
        let by = (base * S).round();
        let (w, h) = (self.pm.width() as i32, self.pm.height() as i32);
        let data = self.pm.data_mut();
        for p in &glyphs {
            let glyph =
                p.id.with_scale_and_position(p.scale, point(ox + p.x, by + p.y));
            if let Some(o) = outlined(p.font, glyph) {
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
                    let inv = 1. - sa;
                    for k in 0..3 {
                        data[i + k] = (col[k] as f32 * sa + data[i + k] as f32 * inv).round() as u8;
                    }
                    data[i + 3] = (255. * sa + data[i + 3] as f32 * inv).round() as u8;
                });
            }
        }
        adv_pt
    }

    /// Align an icon's optical centre to the cap-height centre of its neighbouring text.
    pub fn icon(
        &mut self,
        f: &Fonts,
        glyph: instrument_icons::Icon,
        x: f32,
        baseline: f32,
        size: f32,
        text_size: f32,
        col: Rgba,
    ) -> f32 {
        let value = glyph.glyph().to_string();
        let st = icon(size);
        let (_, _, top, bottom) = f.ink(&value, st);
        let base = baseline - f.cap(sans(text_size, 500.)) / 2. - (top + bottom) / 2.;
        self.text(f, &value, x, base, st, col, A::L);
        size
    }

    /// wrapped paragraph; first baseline at `base`; returns the last baseline.
    #[allow(clippy::too_many_arguments)]
    pub fn para(
        &mut self,
        f: &Fonts,
        s: &str,
        x: f32,
        base: f32,
        w: f32,
        st: St,
        col: Rgba,
        lh: f32,
    ) -> f32 {
        let lines = f.wrap(s, st, w);
        let mut b = base;
        for (i, l) in lines.iter().enumerate() {
            b = base + lh * i as f32;
            self.text(f, l, x, b, st, col, A::L);
        }
        b
    }
}

pub fn encode(pm: &Pixmap) -> Vec<u8> {
    let bytes: Vec<u8> = pm
        .pixels()
        .iter()
        .flat_map(|p| {
            let p = p.demultiply();
            [p.red(), p.green(), p.blue(), p.alpha()]
        })
        .collect();
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(
            &bytes,
            pm.width(),
            pm.height(),
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    out
}

pub fn save(path: &str, pm: &Pixmap) {
    std::fs::write(path, encode(pm)).unwrap();
    println!("{path}  {}x{} px", pm.width(), pm.height());
}

/// Contact sheet: tiles at 50% in a 3-column grid with the file name under each.
pub fn contact(f: &Fonts, tiles: &[(String, Pixmap, String)], path: &str, bg: Rgba, ink: Rgba) {
    let cols = 3usize;
    let gap = 24u32;
    let label_h = 64u32;
    let tw = tiles.iter().map(|t| t.1.width() / 2).max().unwrap();
    let rows = tiles.len().div_ceil(cols);
    let mut row_h = vec![0u32; rows];
    for (i, t) in tiles.iter().enumerate() {
        row_h[i / cols] = row_h[i / cols].max(t.1.height() / 2);
    }
    let width = gap + cols as u32 * (tw + gap);
    let height = gap + row_h.iter().map(|h| h + label_h + gap).sum::<u32>();
    let mut c = Canvas {
        pm: Pixmap::new(width, height).unwrap(),
        dry: false,
        output_scale: S,
        quota_gauges: Vec::new(),
    };
    c.pm.fill(color(bg));
    let mut y = gap;
    for (r, row_height) in row_h.iter().enumerate() {
        for k in 0..cols {
            let i = r * cols + k;
            if i >= tiles.len() {
                break;
            }
            let (name, pm, cap) = &tiles[i];
            let img =
                image::RgbaImage::from_raw(pm.width(), pm.height(), pm.data().to_vec()).unwrap();
            let half = image::imageops::resize(
                &img,
                pm.width() / 2,
                pm.height() / 2,
                image::imageops::FilterType::Lanczos3,
            );
            let (hw, hh) = half.dimensions();
            let small =
                Pixmap::from_vec(half.into_raw(), IntSize::from_wh(hw, hh).unwrap()).unwrap();
            let x = gap + k as u32 * (tw + gap);
            c.pm.draw_pixmap(
                x as i32,
                y as i32,
                small.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
            c.text(
                f,
                name,
                (x as f32) / S,
                (y + hh + 28) as f32 / S,
                sans(11., 600.),
                ink,
                A::L,
            );
            c.text(
                f,
                cap,
                (x as f32) / S,
                (y + hh + 54) as f32 / S,
                sans(10., 400.),
                al(ink, 0.7),
                A::L,
            );
        }
        y += row_height + label_h + gap;
    }
    save(path, &c.pm);
}

pub fn report_glyphs(f: &Fonts) {
    for n in &f.notes {
        println!("  {n}");
    }
    for (k, v) in &f.names {
        println!("  {k}: {v}");
    }
    println!("glyphs served by a fallback font:");
    for (ch, font) in f.used.borrow().iter() {
        println!("  {ch} U+{:04X} -> {font}", *ch as u32);
    }
}

/// height split of one page: dream, instrument, story (story + changes), other
#[derive(Default, Clone, Copy, Debug)]
pub struct Split {
    pub dream: f32,
    pub inst: f32,
    pub story: f32,
}
impl Split {
    pub fn caption(&self, h: f32) -> String {
        let p = |v: f32| (100. * v / h).round() as i32;
        let other = 100 - p(self.dream) - p(self.inst) - p(self.story);
        format!(
            "dream {}% \u{b7} instrument {}% \u{b7} story {}% \u{b7} other {}%",
            p(self.dream),
            p(self.inst),
            p(self.story),
            other
        )
    }
}

#[cfg(test)]
mod bundled_font_tests {
    use super::*;

    #[test]
    fn four_primary_faces_load_without_filesystem_access() {
        let fonts = Fonts::bundled();
        assert_eq!(fonts.names.len(), 4);
        assert!(fonts.fallback.is_empty());
        for fam in [Fam::Sans, Fam::Serif, Fam::Mono] {
            assert!(
                fonts
                    .face(St {
                        fam,
                        size: 14.,
                        wght: 500.,
                        track: 0.
                    })
                    .font
                    .glyph_id('A')
                    .0
                    > 0
            );
        }
        assert!(fonts.face(icon(18.)).font.glyph_id('\u{e410}').0 > 0);
        for name in fonts.names.values() {
            assert!(!name.starts_with('/'));
        }
    }

    #[test]
    fn variable_serif_fractional_extrema_do_not_fill_the_glyph_box() {
        let fonts = Fonts::bundled();
        let font = &fonts.face(serif(30., 600.)).font;
        for x in [0., 0.25, 0.5, 0.75, 300.25] {
            let glyph = font
                .glyph_id('w')
                .with_scale_and_position(px_scale(font, 60.), point(x, 60.));
            let o = outlined(font, glyph).unwrap();
            let bottom = o.px_bounds().height() as u32;
            let mut outside_coverage = 0_f32;
            o.draw(|gx, gy, coverage| {
                // The lower-left corner is outside the w's two diagonal bowls.
                if gx < 2 && gy >= bottom - 2 {
                    outside_coverage = outside_coverage.max(coverage);
                }
            });
            assert!(
                outside_coverage < 0.01,
                "x={x}: stray coverage {outside_coverage}"
            );
        }
    }

    #[test]
    fn changing_digits_use_tabular_figures_at_every_weight_and_size() {
        let fonts = Fonts::bundled();
        for fam in [Fam::Sans, Fam::Serif, Fam::Mono] {
            for size in [10., 12.5, 24., 30., 48.] {
                for wght in [400., 500., 600.] {
                    let st = St {
                        fam,
                        size,
                        wght,
                        track: 0.,
                    };
                    let width = fonts.width("0000", st);
                    for digit in '1'..='9' {
                        let value = digit.to_string().repeat(4);
                        assert!(
                            (fonts.width(&value, st) - width).abs() < 0.01,
                            "{fam:?} {size} {wght}: {value}"
                        );
                    }
                }
            }
        }
        assert!(fonts.used.borrow().is_empty());
    }
}

#[cfg(test)]
mod shadow_tests {
    use super::*;
    #[test]
    fn box_blur_spreads_symmetrically_and_reuses_size_masks() {
        let mut alpha = vec![0; 31 * 31];
        alpha[15 * 31 + 15] = 255;
        blur_alpha(&mut alpha, 31, 31, 2);
        assert!(alpha[15 * 31 + 14] > 0);
        for y in 0..31 {
            for x in 0..31 {
                assert_eq!(alpha[y * 31 + x], alpha[y * 31 + 30 - x]);
                assert_eq!(alpha[y * 31 + x], alpha[(30 - y) * 31 + x]);
            }
        }
        let a = shadow_mask(200., 120., 24., 18.);
        let b = shadow_mask(200., 120., 24., 18.);
        assert!(std::sync::Arc::ptr_eq(&a, &b));
    }
    #[test]
    fn shadows_clip_at_pane_edges_and_leave_glass_interior_untouched() {
        let mut canvas = Canvas::new(120., 100.);
        canvas.background(&theme::DARK);
        let before = canvas.pm.clone();
        canvas.shadow(4., 4., 112., 92., 16., theme::DARK.ambient_shadow);
        let centre = ((50 * 2) * canvas.pm.width() as usize + 60 * 2) * 4;
        assert_eq!(
            &before.data()[centre..centre + 4],
            &canvas.pm.data()[centre..centre + 4]
        );
        assert_ne!(before.data(), canvas.pm.data());
        assert_eq!(canvas.pm.width(), 240);
        assert_eq!(canvas.pm.height(), 200);
    }
}

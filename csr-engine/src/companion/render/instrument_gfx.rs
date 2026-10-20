// Ported from the approved Instrument prototype drawing code.
// Shared drawing machinery: fonts (sans / serif / mono, variable axes), canvas with
// rounded rects, gradients, glass, thin rings with round caps, dotted rings,
// antialiased text with baseline snapping, ink measurement, wrapping, contact sheet.
#![allow(dead_code, clippy::too_many_arguments)]
use super::theme;
use ab_glyph::{point, Font, FontVec, GlyphId, ScaleFont, VariableFont};
use image::{codecs::png::PngEncoder, ImageEncoder};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, IntSize, LineCap, LinearGradient, Paint, Path,
    PathBuilder, Pattern, Pixmap, PixmapPaint, Point, RadialGradient, Rect, Shader, SpreadMode,
    Stroke, Transform,
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

impl Fonts {
    pub fn load() -> Fonts {
        let mut notes = vec![];
        let mut names = BTreeMap::new();
        let mut faces = vec![];
        let mut add = |fam: Fam,
                       paths: &[&str],
                       weights: &[f32],
                       opszs: &[f32],
                       notes: &mut Vec<String>|
         -> String {
            for p in paths {
                let Ok(bytes) = std::fs::read(p) else {
                    continue;
                };
                let Ok(probe) = FontVec::try_from_vec(bytes.clone()) else {
                    continue;
                };
                notes.push(format!("{p} axes: {}", axes(&probe)));
                for &w in weights {
                    for &o in opszs {
                        let mut font = FontVec::try_from_vec(bytes.clone()).unwrap();
                        let ok_w = font.set_variation(b"wght", w);
                        let ok_o = font.set_variation(b"opsz", o);
                        if !ok_w || !ok_o {
                            notes.push(format!("{p}: wght {w} -> {ok_w}, opsz {o} -> {ok_o}"));
                        }
                        faces.push(Face {
                            fam,
                            wght: w,
                            opsz: o,
                            font,
                        });
                    }
                }
                return p.to_string();
            }
            panic!("no font for {fam:?}");
        };
        let mut weights = theme::SETS
            .iter()
            .flat_map(|(_, t)| [t.regular, t.medium, t.bold])
            .collect::<Vec<_>>();
        weights.sort_by(f32::total_cmp);
        weights.dedup();
        let sn = add(
            Fam::Sans,
            &[
                "/System/Library/Fonts/SFNS.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            ],
            &weights,
            &[17., 28.],
            &mut notes,
        );
        let se = add(
            Fam::Serif,
            &[
                "/System/Library/Fonts/NewYork.ttf",
                "/System/Library/Fonts/Supplemental/Georgia.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf",
            ],
            &weights,
            &[17., 28.],
            &mut notes,
        );
        let mo = add(
            Fam::Mono,
            &[
                "/System/Library/Fonts/SFNSMono.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            ],
            &weights,
            &[17.],
            &mut notes,
        );
        names.insert("sans", sn);
        names.insert("serif", se);
        names.insert("mono", mo);
        let mut fallback = vec![];
        for p in [
            "/System/Library/Fonts/Apple Symbols.ttf",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
        ] {
            if let Ok(b) = std::fs::read(p) {
                if let Ok(f) = FontVec::try_from_vec(b) {
                    fallback.push((p.to_string(), f));
                }
            }
        }
        Fonts {
            faces,
            fallback,
            used: RefCell::new(BTreeMap::new()),
            notes,
            names,
            ink_cache: RefCell::new(HashMap::new()),
        }
    }

    fn face(&self, st: St) -> &FontVec {
        let want_o = if st.size >= 20. { 28. } else { 17. };
        let mut best: Option<&Face> = None;
        for f in self.faces.iter().filter(|f| f.fam == st.fam) {
            let score = (f.wght - st.wght).abs() * 10. + (f.opsz - want_o).abs();
            if best
                .map(|b| score < (b.wght - st.wght).abs() * 10. + (b.opsz - want_o).abs())
                .unwrap_or(true)
            {
                best = Some(f);
            }
        }
        &best.unwrap().font
    }

    fn place(&self, s: &str, st: St, mult: f32) -> (Vec<Placed<'_>>, f32) {
        let main = self.face(st);
        let size_px = st.size * S * mult;
        let mut out = vec![];
        let mut x = 0.;
        let mut prev: Option<(*const FontVec, GlyphId)> = None;
        for ch in s.chars() {
            let mut font = main;
            let mut id = font.glyph_id(ch);
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
            let scale = px_scale(font, size_px);
            let sf = font.as_scaled(scale);
            if let Some((pf, pid)) = prev {
                if std::ptr::eq(pf, font) {
                    x += sf.kern(pid, id);
                }
            }
            out.push(Placed { font, scale, id, x });
            x += sf.h_advance(id) + st.track * S * mult;
            prev = Some((font as *const FontVec, id));
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
            let glyph = p.id.with_scale_and_position(p.scale, point(p.x, 0.));
            if let Some(o) = p.font.outline_glyph(glyph) {
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

pub struct Canvas {
    pub pm: Pixmap,
    pub dry: bool,
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

impl Canvas {
    pub fn new(w_pt: f32, h_pt: f32) -> Canvas {
        Canvas {
            pm: Pixmap::new((w_pt * S).round() as u32, (h_pt * S).round() as u32).unwrap(),
            dry: false,
        }
    }
    pub fn dry() -> Canvas {
        Canvas {
            pm: Pixmap::new(1, 1).unwrap(),
            dry: true,
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

    /// Frosted glass: blur what is under the rounded rect, then lay a tint over it.
    pub fn glass(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, tint: Shader<'static>) {
        if self.dry {
            return;
        }
        let pad = 24i32;
        let (pw, ph) = (self.pm.width() as i32, self.pm.height() as i32);
        let x0 = ((x * S) as i32 - pad).max(0);
        let y0 = ((y * S) as i32 - pad).max(0);
        let x1 = (((x + w) * S) as i32 + pad).min(pw);
        let y1 = (((y + h) * S) as i32 + pad).min(ph);
        let (bw, bh) = ((x1 - x0) as u32, (y1 - y0) as u32);
        if let Some(part) = self
            .pm
            .clone_rect(tiny_skia::IntRect::from_xywh(x0, y0, bw, bh).unwrap())
        {
            let img = image::RgbaImage::from_raw(bw, bh, part.data().to_vec()).unwrap();
            let small = image::imageops::resize(
                &img,
                (bw / 8).max(1),
                (bh / 8).max(1),
                image::imageops::FilterType::Triangle,
            );
            let big =
                image::imageops::resize(&small, bw, bh, image::imageops::FilterType::Triangle);
            let blurred =
                Pixmap::from_vec(big.into_raw(), IntSize::from_wh(bw, bh).unwrap()).unwrap();
            let pat = Pattern::new(
                blurred.as_ref(),
                SpreadMode::Pad,
                FilterQuality::Bilinear,
                1.0,
                Transform::from_translate(x0 as f32, y0 as f32),
            );
            let p = rrect_path(x * S, y * S, w * S, h * S, r * S);
            let pt = Paint {
                shader: pat,
                anti_alias: true,
                ..Paint::default()
            };
            self.pm
                .fill_path(&p, &pt, FillRule::Winding, Transform::identity(), None);
        }
        self.rrect_sh(x, y, w, h, r, tint);
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

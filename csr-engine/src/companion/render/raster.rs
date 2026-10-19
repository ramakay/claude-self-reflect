use super::text::{self, Rgb};
use ab_glyph::{point, Font, FontArc, ScaleFont};
use image::{codecs::png::PngEncoder, ImageEncoder};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

pub use super::text::blend;
pub type Cell = (u32, u32);
pub type Point = (f32, f32);
pub type Rgba = [u8; 4];

pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let lum = |rgb: Rgb| {
        let [r, g, b] = rgb.map(|c| text::linear(c as f64));
        0.2126 * r + 0.7152 * g + 0.0722 * b
    };
    let (a, b) = (lum(a), lum(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
pub fn readable(ink: Rgb, ground: Rgb, ratio: f64) -> Rgb {
    let [mut light, a, b] = text::lab(ink);
    let step = if text::lab(ground)[0] > 50. { -1. } else { 1. };
    while contrast(text::from_lab([light, a, b]), ground) < ratio && light > 0. && light < 100. {
        light += step;
    }
    text::from_lab([light, a, b])
}
pub fn rgba(c: Rgb, alpha: u8) -> Rgba {
    [c[0], c[1], c[2], alpha]
}
pub fn png(pixmap: &Pixmap) -> image::ImageResult<Vec<u8>> {
    let bytes: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let p = p.demultiply();
            [p.red(), p.green(), p.blue(), p.alpha()]
        })
        .collect();
    let mut out = Vec::new();
    PngEncoder::new(&mut out).write_image(
        &bytes,
        pixmap.width(),
        pixmap.height(),
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(out)
}
#[derive(Default)]
pub struct Fonts {
    pub regular: Option<FontArc>,
    pub bold: Option<FontArc>,
}
impl Fonts {
    fn font(&self, bold: bool) -> Option<&FontArc> {
        if bold {
            self.bold.as_ref()
        } else {
            self.regular.as_ref()
        }
    }
    fn scale(&self, size: f32, bold: bool) -> Option<ab_glyph::PxScaleFont<&FontArc>> {
        let font = self.font(bold)?;
        // Pillow's size is pixels per em; ab_glyph scales by ascent minus descent.
        let scale = size * font.height_unscaled() / font.units_per_em()?;
        Some(font.as_scaled(scale))
    }
    pub fn width(&self, said: &str, size: f32, bold: bool) -> f32 {
        self.scale(size, bold)
            .map(|font| said.chars().map(|c| font.h_advance(font.glyph_id(c))).sum())
            .unwrap_or(0.)
    }
}
pub struct Canvas {
    pub pixmap: Pixmap,
    pub aa: bool,
}
impl Canvas {
    pub fn new(cols: usize, rows: usize, cell: Cell) -> Option<Self> {
        let width = u32::try_from(cols).ok()?.checked_mul(cell.0)?;
        let height = u32::try_from(rows).ok()?.checked_mul(cell.1)?;
        // A corrupt terminal report must not allocate an unbounded image.
        if u64::from(width) * u64::from(height) > 16_777_216 {
            return None;
        }
        Some(Self {
            pixmap: Pixmap::new(width, height)?,
            aa: true,
        })
    }
    pub fn width(&self) -> f32 {
        self.pixmap.width() as f32
    }
    pub fn height(&self) -> f32 {
        self.pixmap.height() as f32
    }
    fn paint(&self, color: Rgba) -> Paint<'static> {
        let mut paint = Paint {
            anti_alias: self.aa,
            ..Paint::default()
        };
        paint.set_color_rgba8(color[0], color[1], color[2], color[3]);
        paint
    }
    pub fn rect(&mut self, bounds: [f32; 4], color: Rgba) {
        let [x0, y0, x1, y1] = bounds;
        if let Some(rect) = Rect::from_ltrb(x0, y0, x1, y1) {
            self.pixmap
                .fill_rect(rect, &self.paint(color), Transform::identity(), None);
        }
    }
    pub fn polygon(&mut self, points: &[Point], color: Rgba) {
        let Some(&(x, y)) = points.first() else {
            return;
        };
        let mut path = PathBuilder::new();
        path.move_to(x, y);
        for &(x, y) in &points[1..] {
            path.line_to(x, y);
        }
        path.close();
        if let Some(path) = path.finish() {
            self.pixmap.fill_path(
                &path,
                &self.paint(color),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
    pub fn line(&mut self, points: &[Point], color: Rgba, width: f32) {
        let Some(&(x, y)) = points.first() else {
            return;
        };
        let mut path = PathBuilder::new();
        path.move_to(x, y);
        for &(x, y) in &points[1..] {
            path.line_to(x, y);
        }
        if let Some(path) = path.finish() {
            let stroke = Stroke {
                width,
                line_join: tiny_skia::LineJoin::Round,
                ..Stroke::default()
            };
            self.pixmap.stroke_path(
                &path,
                &self.paint(color),
                &stroke,
                Transform::identity(),
                None,
            );
        }
    }
    pub fn dashed(&mut self, points: &[Point], color: Rgba, width: f32, on: f32, off: f32) {
        if on <= 0. || off <= 0. {
            return;
        }
        let (mut left, mut ink) = (on, true);
        for pair in points.windows(2) {
            let [(x0, y0), (x1, y1)] = [pair[0], pair[1]];
            let length = (x1 - x0).hypot(y1 - y0);
            let mut done = 0.;
            while done < length {
                let step = left.min(length - done);
                let (a, b) = (done / length, (done + step) / length);
                if ink {
                    self.line(
                        &[
                            (x0 + (x1 - x0) * a, y0 + (y1 - y0) * a),
                            (x0 + (x1 - x0) * b, y0 + (y1 - y0) * b),
                        ],
                        color,
                        width,
                    );
                }
                done += step;
                left -= step;
                if left <= 0. {
                    ink = !ink;
                    left = if ink { on } else { off };
                }
            }
        }
    }
    pub fn ellipse(&mut self, bounds: [f32; 4], color: Rgba) {
        let [x0, y0, x1, y1] = bounds;
        if let Some(rect) = Rect::from_ltrb(x0, y0, x1, y1) {
            let Some(path) = PathBuilder::from_oval(rect) else {
                return;
            };
            self.pixmap.fill_path(
                &path,
                &self.paint(color),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
    pub fn text(
        &mut self,
        fonts: &Fonts,
        said: &str,
        pos: Point,
        size: f32,
        style: (bool, &str, Rgb),
    ) {
        let (bold, anchor, color) = style;
        let Some(font) = fonts.scale(size, bold) else {
            return;
        };
        let width = fonts.width(said, size, bold);
        let mut x = pos.0
            - match anchor.as_bytes()[0] {
                b'r' => width,
                b'm' => width / 2.,
                _ => 0.,
            };
        let y = pos.1
            + match anchor.as_bytes()[1] {
                b'a' => font.ascent(),
                b'm' => (font.ascent() + font.descent()) / 2.,
                b'd' => font.descent(),
                _ => 0.,
            };
        for c in said.chars() {
            let id = font.glyph_id(c);
            let glyph = id.with_scale_and_position(font.scale(), point(x, y));
            if let Some(outline) = font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, coverage| {
                    let px = bounds.min.x as i32 + gx as i32;
                    let py = bounds.min.y as i32 + gy as i32;
                    if px < 0 || py < 0 {
                        return;
                    }
                    self.rect(
                        [px as f32, py as f32, px as f32 + 1., py as f32 + 1.],
                        rgba(color, (coverage * 255.).round() as u8),
                    );
                });
            }
            x += font.h_advance(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lab_blend_and_readability_match_the_pane() {
        assert_eq!(blend([47, 58, 72], [14, 15, 17], 0.), [47, 58, 72]);
        assert_eq!(
            readable([46, 139, 106], [251, 251, 249], 4.5),
            [36, 131, 99]
        );
        assert!((contrast([0, 0, 0], [255, 255, 255]) - 21.).abs() < 0.001);
    }
    #[test]
    fn canvas_encodes_straight_alpha_and_missing_fonts_draw_nothing() {
        let mut c = Canvas::new(4, 2, (16, 34)).unwrap();
        c.rect([0., 0., 5., 5.], [200, 100, 50, 128]);
        let before = c.pixmap.clone();
        c.text(
            &Fonts::default(),
            "test",
            (0., 0.),
            17.,
            (false, "la", [0; 3]),
        );
        assert_eq!(c.pixmap.data(), before.data());
        let decoded = image::load_from_memory(&png(&c.pixmap).unwrap())
            .unwrap()
            .to_rgba8();
        assert_eq!(decoded.dimensions(), (64, 68));
        assert_eq!(decoded.get_pixel(1, 1).0, [199, 100, 50, 128]);
        assert!(Canvas::new(usize::MAX, 2, (16, 34)).is_none());
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionFacts {
    pub prompts: Vec<f64>,
    pub replies: Vec<(f64, f64)>,
    pub compacts: Vec<f64>,
    pub chunks: Vec<f64>,
    pub recalls: Vec<f64>,
    pub carried: Vec<f64>,
    pub dreams: Vec<f64>,
    pub asked: Vec<f64>,
    pub turns: Vec<f64>,
    pub now: f64,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LastFacts {
    pub series: Vec<(f64, f64)>,
    pub prompts: Vec<f64>,
    pub compactions: Vec<(f64, f64)>,
    pub floor: f64,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaybackFacts {
    pub cost: f64,
    pub saving: f64,
    pub need: f64,
    pub done: f64,
    pub net: f64,
    pub per: f64,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContextFacts {
    pub series: Vec<(f64, f64)>,
    pub prompts: Vec<f64>,
    pub compacts: Vec<f64>,
    pub csr_at: Vec<f64>,
    pub floor: f64,
    pub now: f64,
    pub band: (f64, f64),
}
fn dark(name: &str) -> Rgb {
    match name {
        "you" | "text" => [238, 245, 250],
        "ctx" => [58, 127, 224],
        "learn" | "recall" => [36, 163, 114],
        "turn" => [232, 88, 44],
        "dream" => [242, 182, 50],
        "asked" => [154, 123, 232],
        _ => blend([238, 245, 250], [47, 58, 72], 0.45),
    }
}
fn grounds(c: &mut Canvas, cell: Cell, backs: &[Rgb]) {
    for (k, back) in backs.iter().enumerate() {
        let y = k as f32 * cell.1 as f32;
        c.rect([0., y, c.width(), y + cell.1 as f32], rgba(*back, 255));
    }
}
pub fn session(f: &SessionFacts, cols: usize, cell: Cell, backs: &[Rgb]) -> Option<Pixmap> {
    use super::super::session::Clock;
    let mut c = Canvas::new(cols, 3, cell)?;
    grounds(&mut c, cell, backs);
    let ink = |name| {
        if backs.is_empty() {
            text::ink_rgb(name)
        } else {
            dark(name)
        }
    };
    let stamps: Vec<_> = f
        .prompts
        .iter()
        .copied()
        .chain(f.replies.iter().map(|r| r.0))
        .collect();
    let clock = Clock::new(&stamps, f.now);
    let (w, lane) = (c.width(), cell.1 as f32);
    let x = |t| 2. + clock.at(t) as f32 * (w - 4.);
    for i in [1., 2.] {
        c.line(
            &[(0., i * lane), (w, i * lane)],
            if backs.is_empty() {
                [0, 0, 0, 34]
            } else {
                [255, 255, 255, 40]
            },
            1.,
        );
    }
    for b in &clock.breaks {
        let bx = 2. + (*b / clock.total) as f32 * (w - 4.);
        for y in (0..(3. * lane) as usize).step_by(6) {
            c.line(
                &[(bx, y as f32), (bx, y as f32 + 2.)],
                if backs.is_empty() {
                    [0, 0, 0, 60]
                } else {
                    [255, 255, 255, 60]
                },
                1.,
            );
        }
    }
    for t in &f.compacts {
        c.line(
            &[(x(*t), lane), (x(*t), 2. * lane)],
            if backs.is_empty() {
                [0, 0, 0, 90]
            } else {
                [255, 255, 255, 90]
            },
            1.,
        );
    }
    for t in &f.prompts {
        c.line(
            &[(x(*t), lane * 0.2), (x(*t), lane * 0.8)],
            rgba(ink("you"), 255),
            2.,
        );
    }
    let peak = f.replies.iter().map(|r| r.1).fold(1., f64::max);
    let points: Vec<_> = f
        .replies
        .iter()
        .map(|&(t, ctx)| (x(t), 2. * lane - 2. - (ctx / peak) as f32 * (lane - 4.)))
        .collect();
    if points.len() > 1 {
        let mut wash = vec![(points[0].0, lane * 2.)];
        wash.extend_from_slice(&points);
        wash.push((points.last()?.0, lane * 2.));
        c.polygon(&wash, rgba(ink("ctx"), 70));
        c.line(&points, rgba(ink("ctx"), 255), 1.);
    }
    let (top, floor) = (2. * lane, 3. * lane - 2.);
    let mut bins = std::collections::BTreeMap::<i32, usize>::new();
    for t in f.chunks.iter().filter(|t| **t != 0.) {
        *bins.entry((x(*t) / 4.).floor() as i32).or_default() += 1;
    }
    let most = bins.values().copied().max().unwrap_or(1) as f32;
    for (b, n) in bins {
        let h = (n as f32 / most * lane * 0.45).max(1.);
        c.rect(
            [b as f32 * 4., floor - h, b as f32 * 4. + 3., floor],
            rgba(ink("learn"), 130),
        );
    }
    for t in f.recalls.iter().chain(&f.carried) {
        c.line(
            &[(x(*t), top + lane * 0.1), (x(*t), top + lane * 0.5)],
            rgba(ink("recall"), 255),
            1.,
        );
    }
    for t in &f.dreams {
        let (cx, cy) = (x(*t), top + lane * 0.3);
        c.polygon(
            &[(cx, cy - 4.), (cx + 4., cy + 3.2), (cx - 4., cy + 3.2)],
            rgba(ink("dream"), 255),
        );
    }
    for t in &f.asked {
        let (cx, cy) = (x(*t), top + lane * 0.3);
        c.ellipse(
            [cx - 2.6, cy - 2.6, cx + 2.6, cy + 2.6],
            rgba(ink("asked"), 255),
        );
    }
    for t in &f.turns {
        let (cx, cy) = (x(*t), top + lane * 0.5);
        c.polygon(
            &[
                (cx, cy - 4.5),
                (cx + 4.5, cy),
                (cx, cy + 4.5),
                (cx - 4.5, cy),
            ],
            rgba(ink("turn"), 255),
        );
    }
    Some(c.pixmap)
}
pub fn last(f: &LastFacts, cols: usize, rows: usize, cell: Cell, ground: Rgb) -> Option<Pixmap> {
    use super::super::session::Clock;
    if f.series.len() < 2 {
        return None;
    }
    let mut c = Canvas::new(cols, rows, cell)?;
    c.pixmap.fill(tiny_skia::Color::from_rgba8(
        ground[0], ground[1], ground[2], 255,
    ));
    let stamps: Vec<_> = f
        .series
        .iter()
        .map(|r| r.0)
        .chain(f.prompts.iter().copied())
        .collect();
    let now = stamps.iter().copied().fold(0., f64::max);
    let clock = Clock::new(&stamps, now);
    let (w, h) = (c.width(), c.height());
    let (pad, foot, top) = (3., h - 5., 3.);
    let &(when, peak) = f.series.iter().max_by(|a, b| a.1.total_cmp(&b.1))?;
    let floor = if f.floor > 0. { f.floor } else { 65000. };
    let scale = peak.max(floor) * 1.08;
    let x = |t| pad + clock.at(t) as f32 * (w - 2. * pad);
    let y = |v: f64| foot - (v / scale) as f32 * (foot - top);
    c.line(
        &[(0., foot), (w, foot)],
        rgba(text::ink_rgb("hair"), 255),
        1.,
    );
    for &(t, pre) in &f.compactions {
        c.line(
            &[(x(t), y(if pre > 0. { pre } else { peak })), (x(t), foot)],
            rgba(text::ink_rgb("faint"), 255),
            1.,
        );
    }
    let mut cuts: Vec<_> = f.compactions.iter().map(|r| r.0).collect();
    cuts.sort_by(f64::total_cmp);
    let mut runs: Vec<Vec<Point>> = vec![vec![]];
    let mut k = 0;
    for &(t, v) in &f.series {
        while k < cuts.len() && t >= cuts[k] {
            runs.push(vec![]);
            k += 1;
        }
        runs.last_mut()?.push((x(t), y(v)));
    }
    for run in runs.iter().filter(|r| r.len() > 1) {
        let mut wash = vec![(run[0].0, foot)];
        wash.extend_from_slice(run);
        wash.push((run.last()?.0, foot));
        c.polygon(&wash, rgba(text::ink_rgb("ctx"), 28));
    }
    for run in runs.iter().filter(|r| r.len() > 1) {
        c.line(run, rgba(text::ink_rgb("ctx"), 255), 1.5);
    }
    c.dashed(
        &[(0., y(floor)), (w, y(floor))],
        rgba(text::ink_rgb("quiet"), 255),
        1.,
        4.,
        4.,
    );
    for t in &f.prompts {
        c.line(
            &[(x(*t), foot + 1.5), (x(*t), h - 1.)],
            rgba(text::ink_rgb("you"), 255),
            1.,
        );
    }
    c.ellipse(
        [x(when) - 2.5, y(peak) - 2.5, x(when) + 2.5, y(peak) + 2.5],
        rgba(text::ink_rgb("turn"), 255),
    );
    Some(c.pixmap)
}
pub fn payback(
    f: &PaybackFacts,
    cols: usize,
    rows: usize,
    cell: Cell,
    ground: Rgb,
    fonts: &Fonts,
) -> Option<Pixmap> {
    use super::super::pyfmt::{round, short};
    let PaybackFacts {
        cost,
        saving,
        need,
        done,
        net,
        per,
    } = *f;
    if need <= 0. || saving <= 0. || cost <= 0. {
        return None;
    }
    let mut c = Canvas::new(cols, rows, cell)?;
    c.pixmap.fill(tiny_skia::Color::from_rgba8(
        ground[0], ground[1], ground[2], 255,
    ));
    let (w, h) = (c.width(), c.height());
    let size = cell.1 as f32 * 0.5;
    let paid = done >= need;
    let (left, right, top, foot) = (3., w - 3., 2., h - round(cell.1 as f64 * 0.85) as f32);
    let xmax = if paid {
        done + (done / 25.).floor().max(1.)
    } else {
        need + (need / 4.).floor().max(2.)
    };
    let ymax = if paid {
        (net * 1.3).max(cost * 0.42)
    } else {
        cost * 0.42
    };
    let ymin = if paid {
        -(cost * 1.08).max(ymax * 0.3)
    } else {
        -cost * 1.08
    };
    let x = |n: f64| left + (n / xmax) as f32 * (right - left);
    let y = |v: f64| top + ((ymax - v) / (ymax - ymin)) as f32 * (foot - top);
    let zero = y(0.);
    let ink = text::ink_rgb;
    c.rect([left, top, right, zero], rgba(ink("learn"), 40));
    c.rect([left, zero, right, foot], rgba(ink("quiet"), 30));
    c.line(&[(left, zero), (right, zero)], rgba(ink("you"), 255), 1.);
    c.line(
        &[(x(0.), zero), (x(0.), y(-cost))],
        rgba(ink("turn"), 255),
        3.,
    );
    if done > 0. {
        c.line(
            &[(x(0.), y(-cost)), (x(done), y(net))],
            rgba(ink("ctx"), 255),
            3.,
        );
    }
    if !paid {
        c.dashed(
            &[(x(done), y(net)), (x(need), zero)],
            rgba(ink("quiet"), 200),
            2.,
            6.,
            5.,
        );
    }
    for (cx, cy, name) in [
        (x(need), zero, if paid { "learn" } else { "quiet" }),
        (x(done), y(net), "ctx"),
    ] {
        c.ellipse([cx - 6.3, cy - 6.3, cx + 6.3, cy + 6.3], rgba(ground, 255));
        c.ellipse(
            [cx - 4.5, cy - 4.5, cx + 4.5, cy + 4.5],
            rgba(ink(name), 255),
        );
    }
    let gap = 4.;
    let line_y = |px| y(-cost) + (px - x(0.)) / (x(done) - x(0.)).max(1e-9) * (y(net) - y(-cost));
    c.text(
        fonts,
        "saving",
        (left + 3. * gap, top + gap),
        size,
        (false, "la", ink("learn")),
    );
    let wide = |s: &str, bold| fonts.width(s, size, bold);
    if paid {
        c.text(
            fonts,
            "paid back",
            (x(need) - gap, zero - 2. * gap),
            size,
            (true, "rd", ink("learn")),
        );
        let said = format!(
            "+{}",
            if net >= 1e6 {
                format!("{:.1}M", net / 1e6)
            } else {
                short(net)
            }
        );
        let end = x(done) - 2. * gap;
        let (pos, anchor) = if line_y(end) - gap - size >= top {
            ((end, line_y(end) - gap), "rd")
        } else {
            ((end, line_y(end - wide(&said, true)) + gap), "ra")
        };
        c.text(fonts, &said, pos, size, (true, anchor, ink("ctx")));
        let said = format!("cost {}, one time", short(cost));
        if foot - zero >= size + 2. && x(need) + 6. * gap + wide(&said, true) <= right {
            c.text(
                fonts,
                &said,
                (x(need) + 6. * gap, (zero + foot) / 2.),
                size,
                (true, "lm", ink("turn")),
            );
        }
    } else {
        c.text(
            fonts,
            "not yet paid back",
            (right - gap, foot - gap),
            size,
            (false, "rd", ink("quiet")),
        );
        c.text(
            fonts,
            "paid back",
            (x(need), zero - 2. * gap),
            size,
            (false, "md", ink("quiet")),
        );
        c.text(
            fonts,
            &format!("cost {}", short(cost)),
            (x(0.) + 2. * gap, y(-cost * 0.55)),
            size,
            (true, "lm", ink("turn")),
        );
        let clear = x(need) - wide("paid back", false) / 2. - gap;
        let (pos, anchor) = if done > 0. && x(done) + 2.5 * gap + wide("now", true) > clear {
            (
                (
                    x(done) - 2. * gap,
                    line_y(x(done) - 2. * gap - wide("now", true)) + gap,
                ),
                "ra",
            )
        } else {
            ((x(done) + 2.5 * gap, y(net) - gap), "ld")
        };
        c.text(fonts, "now", pos, size, (true, anchor, ink("ctx")));
    }
    let base = h - 1.;
    c.text(fonts, "0", (x(0.), base), size, (false, "ld", ink("quiet")));
    let need_s = format!("{need:.0}");
    c.text(
        fonts,
        &need_s,
        (x(need), base),
        size,
        (false, "md", ink("quiet")),
    );
    let tick = wide(&need_s, false);
    let mut rooms = vec![(
        x(0.) + wide("0", false) + 3. * gap,
        x(need) - tick / 2. - gap,
    )];
    if paid {
        let said = format!("{done:.0}");
        let end = wide(&said, false);
        if x(done) - x(need) > tick + end {
            c.text(
                fonts,
                &said,
                (x(done), base),
                size,
                (false, "md", ink("quiet")),
            );
        }
        rooms.push((x(need) + tick / 2. + gap, x(done) - end - gap));
        rooms.sort_by(|a, b| (a.0 - a.1).total_cmp(&(b.0 - b.1)));
    }
    let mut words = vec![];
    if per > 0. {
        words.push(format!(
            "API calls after compacting (~{} a prompt)",
            (per + 0.5) as i64
        ));
    }
    words.extend(["API calls after compacting".into(), "API calls".into()]);
    for (start, stop) in rooms {
        if let Some(said) = words.iter().find(|s| wide(s, false) <= stop - start) {
            c.text(
                fonts,
                said,
                (start, base),
                size,
                (false, "ld", ink("quiet")),
            );
            break;
        }
    }
    Some(c.pixmap)
}
pub fn context(
    f: &ContextFacts,
    cols: usize,
    rows: usize,
    cell: Cell,
    backs: &[Rgb],
    fonts: &Fonts,
) -> Option<Pixmap> {
    use super::super::{pyfmt::short, session::Clock};
    let mut series: Vec<_> = f
        .series
        .iter()
        .copied()
        .filter(|r| r.0 > 0. && r.1 > 0.)
        .collect();
    series.sort_by(|a, b| a.0.total_cmp(&b.0));
    if series.len() < 2 || backs.is_empty() {
        return None;
    }
    let mut c = Canvas::new(cols, rows, cell)?;
    grounds(&mut c, cell, backs);
    let stamps: Vec<_> = series
        .iter()
        .map(|r| r.0)
        .chain(f.prompts.iter().copied())
        .collect();
    let clock = Clock::new(&stamps, f.now);
    let &(peak_t, peak) = series.iter().max_by(|a, b| a.1.total_cmp(&b.1))?;
    let (w, h) = (c.width(), c.height());
    let (pad, top, foot) = (2., 3., h - 2.);
    let scale = peak.max(f.band.1) * 1.08;
    let x = |t| pad + clock.at(t) as f32 * (w - 2. * pad);
    let y = |v: f64| foot - (v / scale) as f32 * (foot - top);
    let label = dark("label");
    c.rect(
        [pad, y(f.band.1), w - pad, y(f.band.0)],
        rgba(dark("dream"), 34),
    );
    if f.floor > 0. {
        c.dashed(
            &[(pad, y(f.floor)), (w - pad, y(f.floor))],
            rgba(label, 150),
            1.,
            4.,
            4.,
        );
    }
    let mut bounds = f.compacts.clone();
    bounds.sort_by(f64::total_cmp);
    let mut runs: Vec<Vec<(f64, f64)>> = vec![vec![]];
    let mut drops = vec![];
    let mut k = 0;
    for &(t, v) in &series {
        while k < bounds.len() && t > bounds[k] {
            if let Some(prev) = runs.last().and_then(|r| r.last()) {
                drops.push((bounds[k], prev.1));
            }
            runs.push(vec![]);
            k += 1;
        }
        runs.last_mut()?.push((t, v));
    }
    for run in runs.iter().filter(|r| r.len() > 1) {
        let points: Vec<_> = run.iter().map(|&(t, v)| (x(t), y(v))).collect();
        let mut wash = vec![(points[0].0, foot)];
        wash.extend_from_slice(&points);
        wash.push((points.last()?.0, foot));
        c.polygon(&wash, rgba(dark("text"), 26));
        c.line(&points, rgba(dark("text"), 225), 1.5);
    }
    for (b, before) in drops {
        if let Some(after) = series.iter().find(|r| r.0 > b) {
            let ink = if f.csr_at.contains(&b) {
                dark("turn")
            } else {
                label
            };
            c.line(
                &[(x(b), y(before)), (x(b), y(after.1))],
                rgba(ink, 235),
                1.5,
            );
            c.ellipse(
                [x(b) - 2.2, y(before) - 2.2, x(b) + 2.2, y(before) + 2.2],
                rgba(ink, 255),
            );
        }
    }
    let size = cell.1 as f32 * 0.5;
    let said = format!("{}–{}", short(f.band.0), short(f.band.1));
    let mid = (y(f.band.0) + y(f.band.1)) / 2.;
    let wide = fonts.width(&said, size, false);
    if wide > 0. {
        c.rect(
            [
                w - pad - 1. - wide - 2.,
                mid - size / 2. - 1.,
                w - pad,
                mid + size / 2. + 1.,
            ],
            rgba(*backs.last()?, 200),
        );
    }
    c.text(
        fonts,
        &said,
        (w - pad - 1., mid),
        size,
        (false, "rm", dark("dream")),
    );
    let (px, py) = (x(peak_t), y(peak));
    let (dx, anchor) = if px < w * 0.6 {
        (3., "lb")
    } else {
        (-3., "rb")
    };
    c.text(
        fonts,
        &format!("peak {}", short(peak)),
        (px + dx, py + 2.),
        size,
        (false, anchor, dark("text")),
    );
    Some(c.pixmap)
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgba {
    rgba(
        std::array::from_fn(|i| {
            super::super::pyfmt::round(a[i] as f64 + (b[i] as f64 - a[i] as f64) * t) as u8
        }),
        255,
    )
}
fn pixel_poly(c: &mut Canvas, points: &[Point], color: Rgba) {
    // Integer scanlines keep the isometric faces' two-across, one-down stairs.
    let low = points.iter().map(|p| p.1 as i32).min().unwrap_or(0);
    let high = points.iter().map(|p| p.1 as i32).max().unwrap_or(0);
    for y in low.max(0)..=high.min(c.pixmap.height() as i32 - 1) {
        let mut left = f32::INFINITY;
        let mut right = f32::NEG_INFINITY;
        for i in 0..points.len() {
            let (a, b) = (points[i], points[(i + 1) % points.len()]);
            if (y as f32) < a.1.min(b.1) || (y as f32) > a.1.max(b.1) {
                continue;
            }
            if a.1 == b.1 {
                left = left.min(a.0.min(b.0));
                right = right.max(a.0.max(b.0));
            } else {
                let x = a.0 + (y as f32 - a.1) * (b.0 - a.0) / (b.1 - a.1);
                left = left.min(x);
                right = right.max(x);
            }
        }
        if left.is_finite() && right.is_finite() {
            c.rect(
                [left.ceil(), y as f32, right.floor() + 1., y as f32 + 1.],
                color,
            );
        }
    }
}
fn pixel_line(c: &mut Canvas, a: Point, b: Point, color: Rgba) {
    let (mut x, mut y) = (a.0 as i32, a.1 as i32);
    let (x1, y1) = (b.0 as i32, b.1 as i32);
    let (dx, dy) = ((x1 - x).abs(), (y1 - y).abs());
    let (sx, sy) = ((x1 - x).signum(), (y1 - y).signum());
    if dx > dy {
        let mut error = 2 * dy - dx;
        for _ in 0..dx {
            c.rect([x as f32, y as f32, x as f32 + 1., y as f32 + 1.], color);
            if error >= 0 {
                y += sy;
                error -= 2 * dx;
            }
            error += 2 * dy;
            x += sx;
        }
    } else {
        let mut error = 2 * dx - dy;
        for _ in 0..dy {
            c.rect([x as f32, y as f32, x as f32 + 1., y as f32 + 1.], color);
            if error >= 0 {
                x += sx;
                error -= 2 * dy;
            }
            error += 2 * dx;
            y += sy;
        }
    }
    c.rect(
        [x1 as f32, y1 as f32, x1 as f32 + 1., y1 as f32 + 1.],
        color,
    );
}
struct Iso {
    c: Canvas,
    origin: Point,
    step: f32,
}
impl Iso {
    fn new(w: usize, h: usize, origin: Point, step: f32) -> Option<Self> {
        let mut c = Canvas::new(w, h, (1, 1))?;
        c.aa = false;
        Some(Self { c, origin, step })
    }
    fn at(&self, [x, y, z]: [f32; 3]) -> Point {
        if self.step == 1. {
            (
                self.origin.0 + x - y,
                self.origin.1 + ((x + y) / 2.).floor() - z,
            )
        } else {
            use super::super::pyfmt::round;
            (
                round((self.origin.0 + (x - y) * self.step) as f64) as f32,
                round((self.origin.1 + (x + y) * self.step / 2. - z * self.step) as f64) as f32,
            )
        }
    }
    fn poly(&mut self, points: &[[f32; 3]], ink: Rgba) {
        let points: Vec<_> = points.iter().map(|p| self.at(*p)).collect();
        pixel_poly(&mut self.c, &points, ink);
    }
    fn line(&mut self, a: [f32; 3], b: [f32; 3], ink: Rgba) {
        let (a, b) = (self.at(a), self.at(b));
        pixel_line(&mut self.c, a, b, ink);
    }
    fn cube(&mut self, pos: [f32; 3], size: [f32; 3], colors: [Rgba; 3]) {
        let [x, y, z] = pos;
        let [dx, dy, dz] = size;
        self.poly(
            &[
                [x, y + dy, z],
                [x + dx, y + dy, z],
                [x + dx, y + dy, z + dz],
                [x, y + dy, z + dz],
            ],
            colors[0],
        );
        self.poly(
            &[
                [x + dx, y, z],
                [x + dx, y + dy, z],
                [x + dx, y + dy, z + dz],
                [x + dx, y, z + dz],
            ],
            colors[1],
        );
        self.poly(
            &[
                [x, y, z + dz],
                [x + dx, y, z + dz],
                [x + dx, y + dy, z + dz],
                [x, y + dy, z + dz],
            ],
            colors[2],
        );
    }
    fn part(&mut self, ink: Rgb, paint: impl FnOnce(&mut Self)) {
        let Some(mut layer) = Self::new(
            self.c.pixmap.width() as usize,
            self.c.pixmap.height() as usize,
            self.origin,
            self.step,
        ) else {
            return;
        };
        paint(&mut layer);
        let w = layer.c.pixmap.width() as i32;
        let h = layer.c.pixmap.height() as i32;
        for y in 0..h {
            for x in 0..w {
                let alpha = |x: i32, y: i32| {
                    if x < 0 || y < 0 || x >= w || y >= h {
                        0
                    } else {
                        layer.c.pixmap.pixel(x as u32, y as u32).unwrap().alpha()
                    }
                };
                if alpha(x, y) == 0
                    && [(1, 0), (-1, 0), (0, 1), (0, -1)]
                        .iter()
                        .any(|&(dx, dy)| alpha(x + dx, y + dy) > 0)
                {
                    self.c.rect(
                        [x as f32, y as f32, x as f32 + 1., y as f32 + 1.],
                        mix(ink, [0; 3], 0.55),
                    );
                }
            }
        }
        self.c.pixmap.draw_pixmap(
            0,
            0,
            layer.c.pixmap.as_ref(),
            &tiny_skia::PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    }
    fn block(&mut self, pos: [f32; 3], size: [f32; 3], ink: Rgb) {
        self.part(ink, |p| {
            p.cube(
                pos,
                size,
                [
                    mix(ink, [255; 3], 0.1),
                    mix(ink, [0; 3], 0.25),
                    mix(ink, [255; 3], 0.5),
                ],
            );
            let [x, y, z] = pos;
            let [dx, dy, dz] = size;
            p.line(
                [x, y + dy, z + dz],
                [x + dx, y + dy, z + dz],
                mix(ink, [255; 3], 0.78),
            );
            p.line(
                [x + dx, y + dy, z + dz],
                [x + dx, y, z + dz],
                mix(ink, [255; 3], 0.78),
            );
        });
    }
}
fn crop(p: Pixmap) -> Option<Pixmap> {
    let (mut x0, mut y0, mut x1, mut y1) = (p.width(), p.height(), 0, 0);
    for y in 0..p.height() {
        for x in 0..p.width() {
            if p.pixel(x, y)?.alpha() > 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    p.clone_rect(tiny_skia::IntRect::from_ltrb(
        x0 as i32, y0 as i32, x1 as i32, y1 as i32,
    )?)
}
fn iso_icon(name: &str) -> Option<Pixmap> {
    let mut p = Iso::new(44, 44, (22., 26.), 1.)?;
    let c = text::ink_rgb(text::section(name));
    let quiet = text::ink_rgb("quiet");
    let paper = [250, 249, 244];
    let pages = [226, 224, 216];
    let white = [255; 3];
    let black = [0; 3];
    match name {
        "touching" | "story" | "last" | "compacted" => {
            p.block([-2., -2., 0.], [14., 12., 2.], c);
            p.part(c, |p| {
                p.cube(
                    [0., 0., 2.],
                    [10., 8., 2.],
                    [rgba(pages, 255), mix(pages, black, 0.12), rgba(paper, 255)],
                );
                p.line([2., 2., 4.], [8., 2., 4.], mix(c, white, 0.2));
                p.line([4., 4., 4.], [8., 4., 4.], mix(c, white, 0.2));
                p.line([2., 6., 4.], [6., 6., 4.], rgba(text::ink_rgb("turn"), 255));
            });
        }
        "risk" => {
            p.block([0., 0., 0.], [8., 8., 6.], quiet);
            p.block([4., 0., 6.], [6., 6., 6.], c);
        }
        "changed" => {
            p.block([0., 0., 0.], [10., 10., 2.], quiet);
            p.block([2., 0., 2.], [10., 10., 2.], text::ink_rgb("turn"));
            p.block([0., 0., 4.], [10., 10., 2.], quiet);
        }
        "rules" | "kept" | "todos" => p.part(c, |p| {
            p.cube(
                [0., 0., 0.],
                [14., 10., 4.],
                [mix(c, black, 0.1), rgba(pages, 255), mix(c, white, 0.5)],
            );
            p.line([14., 0., 2.], [14., 10., 2.], mix(pages, black, 0.15));
            p.line([2., 10., 0.], [2., 10., 4.], mix(c, white, 0.45));
            p.line([12., 10., 0.], [12., 10., 4.], mix(c, white, 0.45));
            p.line(
                [14., 6., 4.],
                [14., 6., -1.],
                rgba(text::ink_rgb("turn"), 255),
            );
            p.line([0., 10., 4.], [14., 10., 4.], mix(c, white, 0.78));
            p.line([14., 10., 4.], [14., 0., 4.], mix(c, white, 0.78));
        }),
        "dreams" => {
            p.block([0., 0., 0.], [10., 10., 4.], c);
            let (cx, cy) = p.at([5., 5., 4.]);
            let moon = mix(c, white, 0.3);
            p.part(c, |p| {
                p.c.ellipse([cx - 6., cy - 14., cx + 4., cy - 4.], moon);
                // The cut-out replaces alpha rather than blending transparent paint.
                let mut cut = Canvas::new(44, 44, (1, 1)).unwrap();
                cut.aa = false;
                cut.ellipse([cx - 3., cy - 16., cx + 7., cy - 6.], [255; 4]);
                for (pixel, mask) in p.c.pixmap.pixels_mut().iter_mut().zip(cut.pixmap.pixels()) {
                    if mask.alpha() > 0 {
                        *pixel = tiny_skia::PremultipliedColorU8::TRANSPARENT;
                    }
                }
            });
            p.part(c, |p| {
                for (x, y) in [
                    (cx + 6., cy - 11.),
                    (cx + 5., cy - 12.),
                    (cx + 7., cy - 12.),
                    (cx + 6., cy - 13.),
                ] {
                    p.c.rect([x, y, x + 1., y + 1.], moon);
                }
            });
        }
        _ => {
            p.part(c, |p| {
                p.cube(
                    [0., 0., 0.],
                    [12., 10., 5.],
                    [mix(c, white, 0.1), mix(c, black, 0.25), mix(c, white, 0.5)],
                );
                p.line([0., 10., 5.], [12., 10., 5.], mix(c, white, 0.78));
                p.line([12., 10., 5.], [12., 0., 5.], mix(c, white, 0.78));
                p.poly(
                    &[[1., 1., 5.], [11., 1., 5.], [11., 9., 5.], [1., 9., 5.]],
                    mix(c, black, 0.45),
                );
            });
            p.part(c, |p| {
                p.poly(
                    &[[2., 4., 5.], [10., 4., 5.], [10., 4., 13.], [2., 4., 13.]],
                    rgba(paper, 255),
                );
                p.line([4., 4., 11.], [8., 4., 11.], mix(c, white, 0.1));
                p.line([4., 4., 9.], [6., 4., 9.], mix(c, white, 0.1));
            });
        }
    }
    crop(p.c.pixmap)
}
const LETTERS: [[&str; 5]; 3] = [
    [".##", "#..", "#..", "#..", ".##"],
    [".##", "#..", ".#.", "..#", "##."],
    ["##.", "#.#", "##.", "#.#", "#.#"],
];
const LETTER_INKS: [Rgb; 3] = [[58, 127, 224], [242, 182, 50], [36, 163, 114]];
fn csr_emblem() -> Option<Pixmap> {
    let mut p = Iso::new(110, 96, (40., 30.), 3.)?;
    let slab = [47, 58, 72];
    p.part(slab, |p| {
        p.cube(
            [-2., -2., -2.],
            [17., 8., 2.],
            [
                mix(slab, [255; 3], 0.1),
                mix(slab, [0; 3], 0.25),
                mix(slab, [255; 3], 0.5),
            ],
        )
    });
    let mut mirror = Iso::new(110, 96, (40., 30.), 3.)?;
    mirror.poly(
        &[[-2., -2., 0.], [15., -2., 0.], [15., 6., 0.], [-2., 6., 0.]],
        [255; 4],
    );
    let mut faint = Iso::new(110, 96, (40., 30.), 3.)?;
    for (k, letter) in LETTERS.iter().enumerate() {
        for (zi, row) in letter.iter().rev().enumerate() {
            for (xi, ch) in row.chars().enumerate().filter(|(_, c)| *c == '#') {
                let _ = ch;
                let (x, z) = ((5 * k + xi) as f32, -(zi as f32) - 1.);
                faint.poly(
                    &[
                        [x, 1., z],
                        [x + 1., 1., z],
                        [x + 1., 1., z + 1.],
                        [x, 1., z + 1.],
                    ],
                    mix(LETTER_INKS[k], slab, 0.62),
                );
            }
        }
    }
    for ((dst, src), mask) in
        p.c.pixmap
            .pixels_mut()
            .iter_mut()
            .zip(faint.c.pixmap.pixels())
            .zip(mirror.c.pixmap.pixels())
    {
        if src.alpha() > 0 && mask.alpha() > 0 {
            *dst = *src;
        }
    }
    for (k, letter) in LETTERS.iter().enumerate() {
        let ink = LETTER_INKS[k];
        p.part(ink, |p| {
            for xi in 0..3 {
                for (zi, row) in letter.iter().rev().enumerate() {
                    if row.as_bytes()[xi] == b'#' {
                        p.cube(
                            [(5 * k + xi) as f32, 0., zi as f32],
                            [1., 1., 1.],
                            [
                                rgba(ink, 255),
                                mix(ink, [0; 3], 0.45),
                                mix(ink, [255; 3], 0.22),
                            ],
                        );
                    }
                }
            }
        });
    }
    crop(p.c.pixmap)
}
fn slot(mark: Pixmap, cell: Cell, backs: [Rgb; 2], emblem: bool) -> Option<Pixmap> {
    let mut c = Canvas::new(5, 2, cell)?;
    grounds(&mut c, cell, &backs);
    let (w, h) = (c.pixmap.width(), c.pixmap.height());
    let k = if emblem {
        (w / (mark.width() + 2)).min(h / (mark.height() + 1))
    } else {
        ((w.saturating_sub(4)) / mark.width())
            .min(h.saturating_sub(4) / mark.height())
            .max(1)
    };
    let (mw, mh) = if k > 0 {
        (mark.width() * k, mark.height() * k)
    } else {
        let scale =
            (w as f32 / (mark.width() + 2) as f32).min(h as f32 / (mark.height() + 1) as f32);
        (
            (mark.width() as f32 * scale) as u32,
            (mark.height() as f32 * scale) as u32,
        )
    };
    let data: Vec<_> = mark
        .pixels()
        .iter()
        .flat_map(|p| {
            let p = p.demultiply();
            [p.red(), p.green(), p.blue(), p.alpha()]
        })
        .collect();
    let image = image::RgbaImage::from_raw(mark.width(), mark.height(), data)?;
    let image = image::imageops::resize(
        &image,
        mw.max(1),
        mh.max(1),
        if k > 0 {
            image::imageops::FilterType::Nearest
        } else {
            image::imageops::FilterType::Lanczos3
        },
    );
    let k = k.max(1) as i32;
    let x = (w as i32 - mw as i32 - if emblem { 2 * k } else { 0 }).div_euclid(2);
    let y = (h as i32 - mh as i32 - if emblem { k } else { 0 }).div_euclid(2);
    for (px, py, pixel) in image.enumerate_pixels() {
        if pixel[3] > 0 {
            let (px, py) = (x + px as i32 + 2 * k, y + py as i32 + k);
            c.rect(
                [px as f32, py as f32, px as f32 + 1., py as f32 + 1.],
                [0, 0, 0, 56],
            );
        }
    }
    for (px, py, pixel) in image.enumerate_pixels() {
        let (px, py) = (x + px as i32, y + py as i32);
        c.rect(
            [px as f32, py as f32, px as f32 + 1., py as f32 + 1.],
            pixel.0,
        );
    }
    Some(c.pixmap)
}
pub fn iso(name: &str, cell: Cell, backs: [Rgb; 2]) -> Option<Pixmap> {
    slot(iso_icon(name)?, cell, backs, false)
}
pub fn emblem(cell: Cell, backs: [Rgb; 2]) -> Option<Pixmap> {
    slot(csr_emblem()?, cell, backs, true)
}
pub fn logo(cell: Cell, ground: Rgb) -> Option<Pixmap> {
    let mut c = Canvas::new(4, 1, cell)?;
    c.aa = false;
    c.rect([0., 0., c.width(), c.height()], rgba(ground, 255));
    let (w, h) = (c.width() as i32, c.height() as i32);
    let v = (1..=h.max(1))
        .rev()
        .find(|&v| 11 * v + 2 * (v / 4).max(1) <= w - 2 && 5 * v + (v / 4).max(1) <= h - 2)
        .unwrap_or(1);
    let t = (v / 4).max(1);
    let (x0, y0) = (
        (w - 11 * v - 2 * t).div_euclid(2),
        (h - 5 * v - t).div_euclid(2) + t,
    );
    let mut cells = vec![];
    for (k, letter) in LETTERS.iter().enumerate() {
        for (ry, row) in letter.iter().enumerate() {
            for (cx, _) in row.chars().enumerate().filter(|(_, c)| *c == '#') {
                cells.push((
                    x0 + (4 * k + cx) as i32 * v,
                    y0 + ry as i32 * v,
                    LETTER_INKS[k],
                ));
            }
        }
    }
    let (v, t) = (v as f32, t as f32);
    for &(x, y, ink) in &cells {
        let (x, y) = (x as f32, y as f32);
        pixel_poly(
            &mut c,
            &[
                (x, y),
                (x + 2. * t, y - t),
                (x + v + 2. * t, y - t),
                (x + v, y),
            ],
            mix(ink, [255; 3], 0.35),
        );
        pixel_poly(
            &mut c,
            &[
                (x + v, y),
                (x + v + 2. * t, y - t),
                (x + v + 2. * t, y + v - t),
                (x + v, y + v),
            ],
            mix(ink, [0; 3], 0.4),
        );
    }
    for (x, y, ink) in cells {
        c.rect(
            [x as f32, y as f32, x as f32 + v, y as f32 + v],
            rgba(ink, 255),
        );
    }
    Some(c.pixmap)
}

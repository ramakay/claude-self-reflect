use super::{
    cli::PaneArgs,
    pane::Pane,
    paths::Paths,
    render::{kitty, raster, text, theme::Theme},
};

pub fn export(paths: Paths, args: &PaneArgs) -> anyhow::Result<()> {
    let dir = args
        .png
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing PNG directory"))?;
    std::fs::create_dir_all(dir)?;
    if args.sid.is_some() {
        let now = args
            .now
            .or_else(|| std::env::var("CSR_COMPANION_NOW").ok()?.parse().ok())
            .unwrap_or_else(super::paths::now);
        let paths_again = paths.clone();
        let mut pane = Pane::new(paths, args);
        let lines = pane.frame(args.cols.max(1), args.rows.max(1), now);
        let cell = (16, 34);
        if let Some(facts) = &pane.session_picture {
            if let Some(pixmap) = raster::session(facts, 40, cell, &[]) {
                std::fs::write(dir.join("session.png"), raster::png(&pixmap)?)?;
            }
        }
        std::fs::write(dir.join("screen.txt"), lines.join("\n") + "\n")?;
        cards_png(&paths_again, args, dir, now, cell)?;
        instrument_png(&paths_again, args, dir, now)?;
        if let Some(facts) = &pane.last_picture {
            if let Some(pixmap) = raster::last(facts, 43, 3, cell, text::tint("last", 0.07)) {
                std::fs::write(dir.join("last.png"), raster::png(&pixmap)?)?;
            }
        }
    }
    println!("{}", dir.display());
    Ok(())
}

/// The cards and the pill as the pane would send them: one PNG per placement, and the info view's
/// stack joined into `cards.png`. Nothing is written when the system has no font for them.
fn cards_png(
    paths: &Paths,
    args: &PaneArgs,
    dir: &std::path::Path,
    now: f64,
    cell: (u32, u32),
) -> anyhow::Result<()> {
    for (page, name, rows) in [("info", "cards", 120), ("main", "pill", 30)] {
        let mut shot = args.clone();
        shot.page = page.into();
        shot.png = None;
        shot.frame = true;
        let mut pane = Pane::new(paths.clone(), &shot);
        pane.pixels = true;
        pane.enable_cards();
        if !pane.cards {
            return Ok(());
        }
        pane.frame(46, rows, now);
        let mut stack: Vec<(u32, image::RgbaImage)> = vec![];
        for (i, p) in pane.placed.iter().enumerate() {
            let Ok(img) = image::load_from_memory(&p.png) else {
                continue;
            };
            std::fs::write(dir.join(format!("{name}-{i}.png")), &p.png)?;
            stack.push((p.row as u32 * cell.1, img.to_rgba8()));
        }
        let width = stack.iter().map(|(_, i)| i.width()).max().unwrap_or(0);
        let height = stack.iter().map(|(y, i)| y + i.height()).max().unwrap_or(0);
        if width > 0 {
            let mut sheet =
                image::RgbaImage::from_pixel(width, height, image::Rgba([22, 27, 38, 255]));
            for (y, img) in &stack {
                image::imageops::overlay(&mut sheet, img, 0, i64::from(*y));
            }
            sheet.save(dir.join(format!("{name}.png")))?;
        }
    }
    Ok(())
}

/// Render the same rows and Kitty placements as the live pane, at terminal cell resolution.
fn instrument_png(
    paths: &Paths,
    args: &PaneArgs,
    dir: &std::path::Path,
    now: f64,
) -> anyhow::Result<()> {
    let cell = kitty::cell_size();
    let (cols, rows) = (args.cols.max(1), args.rows.max(1));
    for (name, theme) in [("dark", Theme::Dark), ("light", Theme::Light)] {
        let mut pane = Pane::new(paths.clone(), args);
        pane.theme = theme;
        pane.pixels = true;
        pane.enable_cards();
        // The first frame gives the keys what they act on, as a running pane has.
        pane.frame(cols, rows, now);
        // CSR_PANE_KEYS="t" or "1,2": keys pressed before the frame, so a drill-down or a popup
        // can be rendered from the real data. "@observe:2:0" clicks the row of that act, found on
        // the frame the pane drew, as a mouse click would.
        for key in std::env::var("CSR_PANE_KEYS")
            .unwrap_or_default()
            .split([',', ' '])
            .filter(|key| !key.is_empty())
        {
            if let Some(act) = key.strip_prefix('@') {
                pane.frame(cols, rows, now);
                let row = pane.act_row(act);
                match row {
                    Some(row) => {
                        pane.click(row, 2, now);
                    }
                    None => eprintln!("no row answers {act}"),
                }
            } else {
                // A key meets the frame the watch loop drew after the one before it.
                pane.frame(cols, rows, now);
                pane.key(key);
            }
        }
        let lines = pane.frame(cols, rows, now);
        let image = compose(&lines, &pane.placed, cols, rows, cell, theme)?;
        image.save(dir.join(format!("instrument-{name}.png")))?;
        if let Some(metrics) = &pane.instrument_metrics {
            std::fs::write(
                dir.join(format!("instrument-{name}.metrics.json")),
                serde_json::to_vec_pretty(&serde_json::json!({
                    "key_gap_px": metrics.last_card_gap_px,
                    "figure_region": metrics.figure_region,
                    "agents_region": metrics.agents_region,
                    "help_region": metrics.help_region,
                    "history_rows": metrics.history_rows,
                    "history_bottom": metrics.history_bottom,
                    "light_card_stops": theme.tokens().glass,
                    "layout_height": metrics.height,
                    "content_left": metrics.content_left,
                    "content_width": metrics.content_width,
                    "quota_gauges": metrics.quota_gauges,
                    "fresh_plots": metrics.fresh_plots,
                    "csr_share": theme.tokens().csr_share,
                    "sky": theme.tokens().sky,
                    "cols": cols, "rows": rows,
                    "rows_open": pane.hit_rows(),
                }))?,
            )?;
        }
    }
    Ok(())
}

/// Compose text rows and Kitty placements into one pane-sized RGBA image.
pub fn compose(
    lines: &[String],
    placed: &[kitty::Placement],
    cols: usize,
    rows: usize,
    cell: raster::Cell,
    theme: Theme,
) -> anyhow::Result<image::RgbaImage> {
    let token = theme.tokens();
    let rgb = |color: [u8; 4]| [color[0], color[1], color[2]];
    let ground = rgb(token.ground);
    let ink = rgb(token.ink);
    let mut canvas = raster::Canvas::new(cols, rows, cell)
        .ok_or_else(|| anyhow::anyhow!("PNG canvas exceeds image limits"))?;
    canvas.rect([0., 0., canvas.width(), canvas.height()], token.ground);
    let loaded = super::sources::picture_fonts();
    anyhow::ensure!(
        loaded.regular.is_some(),
        "PNG text rendering requires a system monospace font"
    );
    let fonts = raster::Fonts {
        regular: loaded.regular.clone(),
        bold: loaded.bold.clone().or_else(|| loaded.regular.clone()),
    };
    // Position each character in a terminal cell, even when fallback glyph advances differ.
    let size =
        (cell.1 as f32 * 0.72).min(cell.0 as f32 * 22. / fonts.width("M", 22., false).max(1.));
    for (row, line) in lines.iter().take(rows).enumerate() {
        let (mut fg, mut bg, mut bold, mut dim) = (ink, ground, false, false);
        let mut chars = line.chars().peekable();
        let mut col = 0;
        while let Some(ch) = chars.next() {
            if ch == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                let mut params = String::new();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        if c == 'm' {
                            let codes: Vec<u16> =
                                params.split(';').map(|s| s.parse().unwrap_or(0)).collect();
                            let mut i = 0;
                            while i < codes.len() {
                                match codes[i] {
                                    0 => {
                                        fg = ink;
                                        bg = ground;
                                        bold = false;
                                        dim = false;
                                    }
                                    1 => bold = true,
                                    2 => dim = true,
                                    22 => {
                                        bold = false;
                                        dim = false;
                                    }
                                    39 => fg = ink,
                                    49 => bg = ground,
                                    38 | 48
                                        if codes.get(i + 1) == Some(&2) && i + 4 < codes.len() =>
                                    {
                                        let color = [
                                            codes[i + 2] as u8,
                                            codes[i + 3] as u8,
                                            codes[i + 4] as u8,
                                        ];
                                        if codes[i] == 38 {
                                            fg = color;
                                        } else {
                                            bg = color;
                                        }
                                        i += 4;
                                    }
                                    _ => {}
                                }
                                i += 1;
                            }
                        }
                        break;
                    }
                    params.push(c);
                }
                continue;
            }
            if ch.is_control() {
                continue;
            }
            if col >= cols {
                break;
            }
            let (x, y) = (col as f32 * cell.0 as f32, row as f32 * cell.1 as f32);
            canvas.rect(
                [x, y, x + cell.0 as f32, y + cell.1 as f32],
                raster::rgba(bg, 255),
            );
            let color = if dim { text::blend(bg, fg, 0.5) } else { fg };
            canvas.text(
                &fonts,
                &ch.to_string(),
                (x + cell.0 as f32 / 2., y + cell.1 as f32 / 2.),
                size,
                (bold, "mm", color),
            );
            col += 1;
        }
    }
    let mut image = image::load_from_memory(&raster::png(&canvas.pixmap)?)?.to_rgba8();
    // Kitty's c/r fields define display size, independently of the source PNG dimensions.
    for p in placed {
        anyhow::ensure!(
            p.col <= cols && p.row <= rows && p.cols <= cols - p.col && p.rows <= rows - p.row,
            "PNG placement exceeds pane dimensions"
        );
        if p.cols == 0 || p.rows == 0 {
            continue;
        }
        let source = image::load_from_memory(&p.png)?.to_rgba8();
        let display = image::imageops::resize(
            &source,
            p.cols as u32 * cell.0,
            p.rows as u32 * cell.1,
            image::imageops::FilterType::Lanczos3,
        );
        image::imageops::overlay(
            &mut image,
            &display,
            (p.col as u32 * cell.0).into(),
            (p.row as u32 * cell.1).into(),
        );
    }
    Ok(image)
}

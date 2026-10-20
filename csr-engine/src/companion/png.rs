use super::{
    cli::PaneArgs,
    pane::Pane,
    paths::Paths,
    render::{raster, text},
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
        let lines = pane.frame(46, 30, now);
        let cell = (16, 34);
        if let Some(facts) = &pane.session_picture {
            if let Some(pixmap) = raster::session(facts, 40, cell, &[]) {
                std::fs::write(dir.join("session.png"), raster::png(&pixmap)?)?;
            }
        }
        std::fs::write(dir.join("screen.txt"), lines.join("\n") + "\n")?;
        cards_png(&paths_again, args, dir, now, cell)?;
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

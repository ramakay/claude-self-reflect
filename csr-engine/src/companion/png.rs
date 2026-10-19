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
        let mut pane = Pane::new(paths, args);
        let lines = pane.frame(46, 30, now);
        let cell = (16, 34);
        if let Some(facts) = &pane.session_picture {
            if let Some(pixmap) = raster::session(facts, 40, cell, &[]) {
                std::fs::write(dir.join("session.png"), raster::png(&pixmap)?)?;
            }
        }
        std::fs::write(dir.join("screen.txt"), lines.join("\n") + "\n")?;
        if let Some(facts) = &pane.last_picture {
            if let Some(pixmap) = raster::last(facts, 43, 3, cell, text::tint("last", 0.07)) {
                std::fs::write(dir.join("last.png"), raster::png(&pixmap)?)?;
            }
        }
    }
    println!("{}", dir.display());
    Ok(())
}

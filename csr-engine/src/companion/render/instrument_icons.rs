//! Lucide 1.60.0 glyphs from the bundled ISC-licensed subset. Pixel rendering only.
#![allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub enum Icon {
    MoonStar,
    Moon,
    Check,
    X,
    ArrowRight,
    Lightbulb,
    Timer,
    Recycle,
    ScrollText,
    Gauge,
    Hourglass,
    Bot,
    CircleHelp,
    Sparkles,
    GitCommit,
    FilePen,
    FilePlus,
    FileWarning,
    Coffee,
    Archive,
    CircleAlert,
    Terminal,
    TriangleAlert,
    Activity,
    CircleCheck,
    CircleX,
    Refresh,
    Brain,
    Equal,
}
impl Icon {
    pub const fn glyph(self) -> char {
        match self {
            Self::MoonStar => '\u{e410}',
            Self::Moon => '\u{e11e}',
            Self::Check => '\u{e06c}',
            Self::X => '\u{e1b2}',
            Self::ArrowRight => '\u{e049}',
            Self::Lightbulb => '\u{e1c2}',
            Self::Timer => '\u{e1e0}',
            Self::Recycle => '\u{e2e9}',
            Self::ScrollText => '\u{e45f}',
            Self::Gauge => '\u{e1bf}',
            Self::Hourglass => '\u{e296}',
            Self::Bot => '\u{e1bb}',
            Self::CircleHelp => '\u{e082}',
            Self::Sparkles => '\u{e412}',
            Self::GitCommit => '\u{e0e3}',
            Self::FilePen => '\u{e31f}',
            Self::FilePlus => '\u{e0c9}',
            Self::FileWarning => '\u{e319}',
            Self::Coffee => '\u{e096}',
            Self::Archive => '\u{e041}',
            Self::CircleAlert => '\u{e077}',
            Self::Terminal => '\u{e181}',
            Self::TriangleAlert => '\u{e193}',
            Self::Activity => '\u{e038}',
            Self::CircleCheck => '\u{e226}',
            Self::CircleX => '\u{e084}',
            Self::Refresh => '\u{e144}',
            Self::Brain => '\u{e3c6}',
            Self::Equal => '\u{e1bd}',
        }
    }
}

/// Preserve the semantic terminal strings; only the font renderer translates their markers.
pub fn legacy(ch: char) -> char {
    match ch {
        '☾' => Icon::MoonStar.glyph(),
        '✓' => Icon::Check.glyph(),
        '×' | '✗' => Icon::X.glyph(),
        '→' | '↳' => Icon::ArrowRight.glyph(),
        '◷' => Icon::Timer.glyph(),
        '♻' => Icon::Recycle.glyph(),
        '⌗' => Icon::GitCommit.glyph(),
        '»' => Icon::Activity.glyph(),
        '●' => Icon::CircleCheck.glyph(),
        '○' => Icon::Refresh.glyph(),
        _ => ch,
    }
}

pub fn is_icon(ch: char) -> bool {
    ('\u{e000}'..='\u{f8ff}').contains(&ch)
}

pub fn line(s: &str) -> String {
    let prefix = if let Some(rest) = s.strip_prefix("~ ") {
        Some((Icon::FilePen, rest))
    } else if let Some(rest) = s.strip_prefix("+ ") {
        Some((Icon::FilePlus, rest))
    } else if let Some(rest) = s.strip_prefix("! ") {
        Some((Icon::FileWarning, rest))
    } else if s.starts_with("Away ") {
        Some((Icon::Coffee, s))
    } else if s.starts_with("Compacted.") {
        Some((Icon::Archive, s))
    } else if s.starts_with("refreshed ") {
        Some((Icon::Refresh, s))
    } else if s.contains(" stalled ·") {
        Some((Icon::TriangleAlert, s))
    } else if s.contains("/turn") && (s.starts_with('+') || s.starts_with('−')) {
        Some((Icon::Activity, s))
    } else {
        None
    };
    prefix
        .map(|(i, text)| format!("{} {text}", i.glyph()))
        .unwrap_or_else(|| s.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_named_icon_resolves_to_the_bundled_face() {
        let f = super::super::instrument_gfx::Fonts::bundled();
        for icon in [
            Icon::MoonStar,
            Icon::Moon,
            Icon::Check,
            Icon::X,
            Icon::ArrowRight,
            Icon::Lightbulb,
            Icon::Timer,
            Icon::Recycle,
            Icon::ScrollText,
            Icon::Gauge,
            Icon::Hourglass,
            Icon::Bot,
            Icon::CircleHelp,
            Icon::Sparkles,
            Icon::GitCommit,
            Icon::FilePen,
            Icon::FilePlus,
            Icon::FileWarning,
            Icon::Coffee,
            Icon::Archive,
            Icon::CircleAlert,
            Icon::Terminal,
            Icon::TriangleAlert,
            Icon::Activity,
            Icon::CircleCheck,
            Icon::CircleX,
            Icon::Refresh,
            Icon::Brain,
            Icon::Equal,
        ] {
            assert!(f.bundled_has(icon.glyph()), "{icon:?}");
        }
    }
}

//! The pane's own colours, named once: the paper the cards sit on, the section inks, and the dark
//! strip the masthead and its pill are drawn on. Every value is the pane's (text.rs, raster.rs).
use super::text::Rgb;

/// The pane's paper, and the paper of a note.
pub const PAPER: Rgb = [251, 251, 249];
pub const NOTE: Rgb = [250, 249, 244];
pub const HAIRLINE: Rgb = [226, 225, 219];
/// Text: ink, quiet and faint.
pub const INK: Rgb = [23, 25, 28];
pub const QUIET: Rgb = [94, 94, 89];
pub const FAINT: Rgb = [136, 135, 129];
/// Section inks on paper.
pub const CTX: Rgb = [44, 90, 160];
pub const RECALL: Rgb = [36, 131, 99];
pub const TURN: Rgb = [200, 68, 42];
pub const DREAM: Rgb = [152, 108, 0];
pub const ASKED: Rgb = [123, 95, 201];
pub const SEPIA: Rgb = [111, 86, 66];
/// The masthead strip: its slate runs from the top colour to the bottom one.
pub const SLATE_TOP: Rgb = [47, 58, 72];
pub const SLATE_BOTTOM: Rgb = [14, 15, 17];
/// Text on the strip and the gauge ramp's ends on it.
pub const DATA: Rgb = [238, 245, 250];
pub const DATA_QUIET: Rgb = [147, 155, 165];
pub const RAMP_BLUE: Rgb = [58, 127, 224];
pub const RAMP_AMBER: Rgb = [242, 182, 50];
pub const RAMP_RED: Rgb = [232, 88, 44];
pub const STRIP_GREEN: Rgb = [36, 163, 114];
pub const STRIP_PURPLE: Rgb = [154, 123, 232];
/// A track under a bar: a little darker than the hairline.
pub const TRACK: Rgb = [214, 213, 206];

/// The paper washed with `ink`, `strength` of the way: a card's face.
pub fn wash(ink: Rgb, strength: f64) -> Rgb {
    super::text::blend(PAPER, ink, strength)
}

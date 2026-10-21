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

/// The Instrument's single spacing scale, in layout points.
pub const S1: f32 = 4.;
pub const S2: f32 = 8.;
pub const S3: f32 = 12.;
pub const S4: f32 = 16.;
pub const S5: f32 = 24.;
pub const S6: f32 = 32.;
pub const ICON_GAP: f32 = 6.;
pub const TITLE_SIZE: f32 = 13.;
pub const TITLE_LINE: f32 = 18.;
pub const BODY_SIZE: f32 = 12.5;
pub const BODY_LINE: f32 = 18.;
pub const CAPTION_SIZE: f32 = 10.5;
pub const CAPTION_LINE: f32 = 14.;
pub const CHIP_SIZE: f32 = 10.;
pub const CHIP_LINE: f32 = 14.;

#[derive(Clone, Copy)]
pub struct Shadow {
    pub y: f32,
    pub blur: f32,
    pub color: [u8; 4],
}

/// All appearance choices for the instrument composition. Adding a theme adds one set.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Theme(usize);
impl std::fmt::Debug for Theme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(SETS[self.0].0)
    }
}
#[derive(Clone, Copy)]
pub struct Tokens {
    pub fresh: [u8; 4],
    pub ground: [u8; 4],
    pub ink: [u8; 4],
    pub quiet: [u8; 4],
    pub faint: [u8; 4],
    pub aurora: [&'static [(f32, [u8; 4])]; 3],
    pub ambient_shadow: Shadow,
    pub contact_shadow: Shadow,
    pub top_highlight: [u8; 4],
    pub linear_at: [f32; 2],
    pub glass: [[u8; 4]; 2],
    pub hair: [u8; 4],
    pub mint: [u8; 4],
    pub sky: [u8; 4],
    pub dream: [u8; 4],
    pub amber: [u8; 4],
    pub risk: [u8; 4],
    pub asked: [u8; 4],
    pub track: [u8; 4],
    pub band: [u8; 4],
    pub chip: [u8; 4],
    pub ring: f32,
    pub thin_ring: f32,
    pub hair_width: f32,
    pub regular: f32,
    pub medium: f32,
    pub bold: f32,
}
/// Registering a named set here is sufficient for environment selection and live cycling.
pub const SETS: &[(&str, Tokens)] = &[
    (
        "dark",
        Tokens {
            fresh: [244, 145, 199, 255],
            ground: [17, 20, 24, 255],
            ink: [242, 244, 246, 255],
            quiet: [184, 193, 203, 255],
            faint: [174, 184, 196, 255],
            aurora: [
                &[(0., [14, 58, 58, 217]), (1., [14, 58, 58, 0])],
                &[(0., [29, 42, 92, 191]), (1., [29, 42, 92, 0])],
                &[(0., [44, 90, 46, 140]), (1., [44, 90, 46, 0])],
            ],
            ambient_shadow: Shadow {
                y: 6.,
                blur: 18.,
                color: [0, 0, 0, 89],
            },
            contact_shadow: Shadow {
                y: 1.,
                blur: 2.,
                color: [0, 0, 0, 77],
            },
            top_highlight: [255, 255, 255, 15],
            linear_at: [0., 1.],
            glass: [[31, 36, 43, 184], [26, 31, 38, 184]],
            hair: [255, 255, 255, 23],
            mint: [155, 232, 194, 255],
            sky: [124, 196, 255, 255],
            dream: [232, 197, 71, 255],
            amber: [242, 182, 50, 255],
            risk: [255, 163, 140, 255],
            asked: [184, 173, 255, 255],
            track: [58, 66, 77, 255],
            band: [130, 150, 173, 255],
            chip: [255, 255, 255, 18],
            ring: 1.5,
            thin_ring: 1.,
            hair_width: 0.5,
            regular: 400.,
            medium: 500.,
            bold: 600.,
        },
    ),
    (
        "light",
        Tokens {
            fresh: [133, 35, 96, 255],
            ground: [247, 247, 245, 255],
            ink: [23, 25, 28, 255],
            quiet: [69, 77, 88, 255],
            faint: [78, 87, 100, 255],
            aurora: [
                &[(0., [221, 243, 234, 230]), (1., [221, 243, 234, 0])],
                &[(0., [233, 229, 251, 204]), (1., [233, 229, 251, 0])],
                &[(0., [252, 235, 221, 179]), (1., [252, 235, 221, 0])],
            ],
            ambient_shadow: Shadow {
                y: 8.,
                blur: 24.,
                color: [16, 24, 40, 20],
            },
            contact_shadow: Shadow {
                y: 1.,
                blur: 2.,
                color: [16, 24, 40, 15],
            },
            top_highlight: [255, 255, 255, 191],
            linear_at: [0., 1.],
            glass: [[255, 255, 255, 173], [249, 249, 249, 173]],
            hair: [23, 25, 28, 35],
            mint: [26, 109, 78, 255],
            sky: [32, 91, 147, 255],
            dream: [121, 85, 0, 255],
            amber: [132, 86, 0, 255],
            risk: [169, 53, 27, 255],
            asked: [104, 68, 164, 255],
            track: [212, 219, 223, 255],
            band: [128, 145, 163, 255],
            chip: [23, 25, 28, 12],
            ring: 1.5,
            thin_ring: 1.,
            hair_width: 0.5,
            regular: 400.,
            medium: 500.,
            bold: 600.,
        },
    ),
];
pub const DARK: Tokens = SETS[0].1;
pub const LIGHT: Tokens = SETS[1].1;
pub const REGULAR: f32 = DARK.regular;
pub const MEDIUM: f32 = DARK.medium;
pub const BOLD: f32 = DARK.bold;
impl Theme {
    #[allow(non_upper_case_globals)]
    pub const Dark: Self = Self(0);
    #[allow(non_upper_case_globals)]
    pub const Light: Self = Self(1);
    pub fn from_env() -> Self {
        // There is no background query in the current terminal integration, so auto is dark.
        let name = std::env::var("CSR_COMPANION_THEME").unwrap_or_else(|_| "auto".into());
        Self(SETS.iter().position(|(key, _)| *key == name).unwrap_or(0))
    }
    pub fn tokens(self) -> &'static Tokens {
        &SETS[self.0].1
    }
    pub fn toggle(&mut self) {
        self.0 = (self.0 + 1) % SETS.len();
    }
}

pub const OPAQUE_WHITE: [u8; 4] = [255, 255, 255, 255];
pub const SWATCH_0: [u8; 3] = [22, 27, 38];
pub const SWATCH_1: [u8; 3] = [47, 58, 72];
pub const SWATCH_2: [u8; 3] = [14, 15, 17];
pub const SWATCH_3: [u8; 3] = [46, 139, 106];
pub const SWATCH_4: [u8; 3] = [251, 251, 249];
pub const SWATCH_5: [u8; 3] = [36, 131, 99];
pub const SWATCH_6: [u8; 3] = [0, 0, 0];
pub const SWATCH_7: [u8; 3] = [255, 255, 255];
pub const SWATCH_8: [u8; 4] = [200, 100, 50, 128];
pub const SWATCH_9: [u8; 4] = [199, 100, 50, 128];
pub const SWATCH_10: [u8; 3] = [238, 245, 250];
pub const SWATCH_11: [u8; 3] = [58, 127, 224];
pub const SWATCH_12: [u8; 3] = [36, 163, 114];
pub const SWATCH_13: [u8; 3] = [232, 88, 44];
pub const SWATCH_14: [u8; 3] = [242, 182, 50];
pub const SWATCH_15: [u8; 3] = [154, 123, 232];
pub const SWATCH_16: [u8; 4] = [0, 0, 0, 34];
pub const SWATCH_17: [u8; 4] = [255, 255, 255, 40];
pub const SWATCH_18: [u8; 4] = [0, 0, 0, 60];
pub const SWATCH_19: [u8; 4] = [255, 255, 255, 60];
pub const SWATCH_20: [u8; 4] = [0, 0, 0, 90];
pub const SWATCH_21: [u8; 4] = [255, 255, 255, 90];
pub const SWATCH_22: [u8; 3] = [250, 249, 244];
pub const SWATCH_23: [u8; 3] = [226, 224, 216];
pub const SWATCH_24: [u8; 4] = [0, 0, 0, 56];
pub const SWATCH_25: [u8; 3] = [23, 25, 28];
pub const SWATCH_26: [u8; 3] = [44, 90, 160];
pub const SWATCH_27: [u8; 3] = [200, 68, 42];
pub const SWATCH_28: [u8; 3] = [152, 108, 0];
pub const SWATCH_29: [u8; 3] = [123, 95, 201];
pub const SWATCH_30: [u8; 3] = [111, 86, 66];
pub const SWATCH_31: [u8; 3] = [94, 94, 89];
pub const SWATCH_32: [u8; 3] = [136, 135, 129];
pub const SWATCH_33: [u8; 3] = [226, 225, 219];
pub const SWATCH_34: [u8; 3] = [147, 155, 165];
pub const SWATCH_35: [u8; 3] = [64, 190, 176];

#[cfg(test)]
mod instrument_tests {
    use super::*;
    fn luminance(c: [u8; 4]) -> f64 {
        let channel = |v: u8| {
            let v = f64::from(v) / 255.;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(c[0]) + 0.7152 * channel(c[1]) + 0.0722 * channel(c[2])
    }
    #[test]
    fn text_meets_aa_on_every_glass_stop_over_every_aurora_stop() {
        for (_, t) in SETS {
            let composite = |a: [u8; 4], b: [u8; 4]| {
                let mut out = b;
                for i in 0..3 {
                    out[i] = ((f64::from(a[i]) * f64::from(a[3])
                        + f64::from(b[i]) * f64::from(255 - a[3]))
                        / 255.)
                        .round() as u8;
                }
                out[3] = 255;
                out
            };
            let mut grounds = vec![t.ground];
            for layer in t.aurora {
                grounds = grounds
                    .iter()
                    .flat_map(|base| layer.iter().map(|(_, c)| composite(*c, *base)))
                    .collect();
            }
            for under in &grounds {
                for glass in t.glass {
                    let mut card = glass;
                    for i in 0..3 {
                        card[i] = ((f64::from(glass[i]) * f64::from(glass[3])
                            + f64::from(under[i]) * f64::from(255 - glass[3]))
                            / 255.)
                            .round() as u8;
                    }
                    for ink in [
                        // Dream-section bodies use ink; their short labels use dream amber.
                        t.ink, t.dream, t.quiet, t.faint, t.mint, t.sky, t.amber, t.risk, t.asked,
                    ] {
                        let a = luminance(card);
                        let b = luminance(ink);
                        assert!(
                            (a.max(b) + 0.05) / (a.min(b) + 0.05) >= 4.5,
                            "{ink:?} on {card:?}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn render_files_have_no_literal_colours() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/companion/render");
        let pattern=regex::Regex::new(r"\[\s*(?:0|255)\s*;\s*[34]\s*\]|\[\s*\d{1,3}\s*,\s*\d{1,3}\s*,\s*\d{1,3}(?:\s*,\s*\d{1,3})?\s*\]|0x[0-9a-fA-F]{6,8}\b|#[0-9a-fA-F]{6}\b").unwrap();
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|x| x == "rs")
                && path.file_name().unwrap() != "theme.rs"
            {
                let source = std::fs::read_to_string(&path).unwrap();
                assert!(
                    !pattern.find_iter(&source).any(|m| m.as_str().contains(';')
                        || !m.as_str().starts_with('[')
                        || m.as_str()
                            .trim_matches(['[', ']'])
                            .split(',')
                            .all(|v| v.trim().parse::<u16>().is_ok_and(|v| v <= 255))),
                    "literal colour in {}",
                    path.display()
                );
            }
        }
    }
}

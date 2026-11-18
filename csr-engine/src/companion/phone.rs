//! herdr's phone layout, and the zoom that gives a phone's screen to the pane (pane.py
//! `Pane.phone`, `is_mobile`, `mobile_threshold`, `zoom_key`, `zoom`).
//!
//! herdr 0.9.3 keeps a tab's split in its phone layout, so on a 50-column phone the pane has
//! about 17 columns. A tap there zooms the pane through `herdr pane zoom --on` and the phone page
//! draws at the tab's width; `z`, or a tap on its legend's `z split`, gives the split back and
//! the focus returns to Claude. A desktop never zooms, and a desktop client taking a zoomed tab
//! unzooms it.
use super::send;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// herdr's `ui.mobile_width_threshold` when its config does not set one.
pub const MOBILE_WIDTH: u64 = 64;
/// Seconds between two reads of the layout at one pane size.
pub const MOBILE_EVERY: f64 = 5.;
/// A pane narrower than this is in column mode: the strip is its page.
pub const COLUMN_UNDER: usize = 24;

/// herdr's `ui.mobile_width_threshold`: at or under this many columns it lays a tab out for a
/// phone. Its default on any doubt (no file, bad TOML, not a positive integer).
pub fn mobile_threshold(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|table| table.get("ui")?.get("mobile_width_threshold")?.as_integer())
        .filter(|n| *n > 0)
        .map_or(MOBILE_WIDTH, |n| n as u64)
}
/// True when `herdr pane layout` (its JSON text) puts this pane's tab in herdr's phone layout:
/// the tab's area no wider than `threshold`. Anything unreadable is a desktop.
pub fn is_mobile(layout: &str, threshold: u64) -> bool {
    serde_json::from_str::<Value>(layout)
        .ok()
        .and_then(|v| v["result"]["layout"]["area"]["width"].as_u64())
        .is_some_and(|width| width > 0 && width <= threshold)
}

type Call = Box<dyn Fn(&[&str]) -> Option<String> + Send>;
/// This pane in herdr: its id, herdr's config file, and the call that runs herdr's CLI.
pub struct Herdr {
    me: String,
    config: PathBuf,
    call: Call,
}
impl Herdr {
    /// The pane herdr started this process in (`HERDR_PANE_ID`); None outside herdr. Every call
    /// goes through `send::herdr`.
    pub fn from_env(home: &Path) -> Option<Self> {
        let me = std::env::var("HERDR_PANE_ID")
            .ok()
            .filter(|s| !s.is_empty())?;
        Some(Self {
            me,
            config: home.join(".config/herdr/config.toml"),
            call: Box::new(send::herdr),
        })
    }
    /// A herdr that answers through `call`, for tests.
    pub fn with(me: &str, config: &Path, call: Call) -> Self {
        Self {
            me: me.into(),
            config: config.into(),
            call,
        }
    }
    fn zoom(&self, on: bool) -> bool {
        (self.call)(&[
            "pane",
            "zoom",
            "--pane",
            &self.me,
            if on { "--on" } else { "--off" },
        ])
        .is_some()
    }
    fn focus_back(&self) {
        let _ = (self.call)(&["pane", "focus", "--pane", &self.me, "--direction", "left"]);
    }
    fn is_phone(&self) -> bool {
        (self.call)(&["pane", "layout", "--pane", &self.me])
            .is_some_and(|layout| is_mobile(&layout, mobile_threshold(&self.config)))
    }
}

/// What the pane knows of the phone: the last read of herdr's layout, whether this pane zoomed
/// itself, and what the legend row was last drawn for.
#[derive(Default)]
pub struct Phone {
    seen: Option<((usize, usize), f64, bool)>,
    /// This pane zoomed itself on a phone: z undoes it, and so does a desktop taking the tab.
    pub zoomed: bool,
    /// (on a phone, in column mode) as the legend row was last drawn.
    pub foot: Option<(bool, bool)>,
}
impl Phone {
    /// Whether herdr shows this pane on a phone now: read from its layout on a new size, else at
    /// most every `MOBILE_EVERY` seconds. Outside herdr, or on any doubt, a desktop.
    pub fn read(&mut self, herdr: Option<&Herdr>, cols: usize, rows: usize, now: f64) -> bool {
        if let Some((size, at, found)) = self.seen {
            if size == (cols, rows) && (0. ..MOBILE_EVERY).contains(&(now - at)) {
                return found;
            }
        }
        let found = herdr.is_some_and(Herdr::is_phone);
        self.seen = Some(((cols, rows), now, found));
        found
    }
    /// At the start of a frame: herdr's own key split the pane again, or a desktop has the tab
    /// now. A desktop's client taking the tab is undone through herdr; a split needs nothing.
    pub fn settle(&mut self, herdr: Option<&Herdr>, column: bool, phone: bool) {
        if self.zoomed && (column || !phone) {
            if !column {
                if let Some(herdr) = herdr {
                    herdr.zoom(false);
                }
            }
            self.zoomed = false;
        }
    }
    /// z, or a tap on a phone's narrow column: the pane fills the phone's screen, laid out for
    /// the phone; again, the split is back and the keys go to Claude. A desktop keeps its split.
    /// True when the layout changed.
    pub fn zoom_key(&mut self, herdr: Option<&Herdr>) -> bool {
        if self.zoomed {
            self.zoomed = false;
            if let Some(herdr) = herdr {
                herdr.zoom(false);
                herdr.focus_back();
            }
            return true;
        }
        if self.seen.is_some_and(|(_, _, phone)| phone) && herdr.is_some_and(|h| h.zoom(true)) {
            self.zoomed = true;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests;

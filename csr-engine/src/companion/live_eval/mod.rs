//! A live eval of a running trial: a soak, a canary, a long test. A trial is a JSON spec in
//! `~/.claude-self-reflect/trials/` naming a file of samples (one JSON object per line) and the
//! checks to hold the samples to. The core (`spec`, `samples`, `eval`, `words`) knows nothing of
//! the pane or of drawing, only of times, numbers and strings, so it can move to a crate of its
//! own. `load` is the one place that touches the disk.
//!
//! Each check is pass, fail or pending. The verdict is "N of M checks passing" and a headline in
//! words: "On track", "Failing: <label>" or "Waiting for data"; after the planned end it is the
//! final one, "Soak finished clean" or "Soak finished: <n> failed".
//!
//! A spec may say `"track": "road"`: a maintainer's long road (to a star rating, say) beside the
//! running trial. It is read only when the road is switched on; it shows as one line under the
//! run card, or as the card itself when no run trial is on.
mod eval;
mod load;
mod samples;
mod spec;
#[cfg(test)]
mod tests;
mod words;

pub use eval::{evaluate, CheckResult, Headline, Report, Status, Trend, Val};
pub use load::{card_at, load, Loaded, Shown, SHOWN_AFTER_END};
pub use samples::{parse as parse_samples, Sample};
pub use spec::{Candidate, CheckSpec, Kind, Op, Track, Trial};
pub use words::{card, road_card, road_line, unreadable, Card, Row};

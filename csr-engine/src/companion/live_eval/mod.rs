//! A live eval of a running trial: a soak, a canary, a long test. A trial is a JSON spec in
//! `~/.claude-self-reflect/trials/` naming a file of samples (one JSON object per line) and the
//! checks to hold the samples to. The core (`spec`, `samples`, `eval`, `words`) knows nothing of
//! the pane or of drawing, only of times, numbers and strings, so it can move to a crate of its
//! own. `load` is the one place that touches the disk.
//!
//! Each check is pass, fail or pending. The verdict is "N of M checks passing" and a headline in
//! words: "On track", "Failing: <label>" or "Waiting for data"; after the planned end it is the
//! final one, "Soak finished clean" or "Soak finished: <n> failed".
mod eval;
mod load;
mod samples;
mod spec;
#[cfg(test)]
mod tests;
mod words;

pub use eval::{evaluate, CheckResult, Headline, Report, Status, Trend, Val};
pub use load::{card_at, load, Loaded, SHOWN_AFTER_END};
pub use samples::{parse as parse_samples, Sample};
pub use spec::{Candidate, CheckSpec, Kind, Op, Trial};
pub use words::{card, unreadable, Card, Row};

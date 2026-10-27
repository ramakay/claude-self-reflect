pub mod agents;
pub mod app;
pub mod brief;
pub mod card;
pub mod carry;
pub mod cli;
pub mod coach;
pub mod codex_runs;
pub mod compact;
mod compaction_scan;
pub mod db;
pub mod dreams;
mod envelopes;
pub mod feed;
pub mod flight;
pub mod forecast;
pub mod gate;
pub mod herdr_events;
pub mod hook_json;
pub mod infinite;
pub mod info;
pub mod layout;
pub mod lease;
pub mod lost;
pub mod lostmain;
pub mod masthead;
pub mod note;
pub mod pages;
pub mod pane;
pub mod paths;
pub mod phone;
pub mod png;
pub mod private;
pub mod pyfmt;
pub mod readings;
pub mod recall;
pub mod relay;
pub mod render;
pub mod repo_recap;
pub mod send;
pub mod session;
pub mod shiftmap;
pub mod sources;
pub mod store;
pub mod story;
mod tail;
pub mod threads;
pub mod usage;
pub mod value;
pub mod wake;
pub mod watch;
/// Borrow an array, treating missing or differently typed values as empty.
fn array(v: &serde_json::Value) -> &[serde_json::Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
/// Read a number, treating missing or differently typed values as zero.
fn num(v: &serde_json::Value) -> f64 {
    v.as_f64().unwrap_or(0.)
}
/// Borrow a string, treating missing or differently typed values as empty.
fn string(v: &serde_json::Value) -> &str {
    v.as_str().unwrap_or("")
}

fn truthy(v: &serde_json::Value) -> bool {
    use serde_json::Value;
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(_) => num(v) != 0.,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

pub mod cli;
pub mod db;
pub mod dreams;
pub mod feed;
pub mod masthead;
pub mod pages;
pub mod pane;
pub mod paths;
pub mod private;
pub mod pyfmt;
pub mod render;
pub mod send;
pub mod session;
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

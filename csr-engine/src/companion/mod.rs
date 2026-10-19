pub mod cli;
pub mod db;
pub mod paths;
pub mod private;
pub mod session;
pub mod dreams;
pub mod feed;
pub mod masthead;
pub mod pyfmt;
pub mod render;
fn array(v: &serde_json::Value) -> &[serde_json::Value] { v.as_array().map(Vec::as_slice).unwrap_or(&[]) }
fn num(v: &serde_json::Value) -> f64 { v.as_f64().unwrap_or(0.) }
fn string(v: &serde_json::Value) -> &str { v.as_str().unwrap_or("") }

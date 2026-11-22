//! The one word a timeline row says before its line. Fixed tables, no model.
use super::instrument::RiverKind;
use crate::companion::{string, truthy};
use serde_json::Value;

/// A thread row's mood. "asked" only when the thread waits on the user; open or lost threads are
/// "left open"; everything else (faded, carried, answered, handed) is "noted".
pub fn thread_mood(thread: &Value) -> &'static str {
    if truthy(&thread["waiting"]) {
        "asked"
    } else if matches!(string(&thread["state"]), "open" | "lost") {
        "left open"
    } else {
        "noted"
    }
}
/// The one word a timeline row says before its line. Fixed table, no model.
pub fn mood(kind: RiverKind, lead: &str) -> &'static str {
    match (kind, lead) {
        (RiverKind::Write | RiverKind::Commit, "! ") => "worried",
        (RiverKind::Prompt, _) => "asked",
        (RiverKind::Dream, _) => "wondered",
        (RiverKind::Compact, _) => "made room",
        (RiverKind::Write, _) => "tinkered",
        (RiverKind::Commit, _) => "landed",
        (RiverKind::Agent, "✓ ") => "heard back",
        (RiverKind::Agent, _) => "delegated",
        (RiverKind::Other, _) => "",
    }
}

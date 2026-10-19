use super::{array, render::text::control, string};
use serde_json::Value;

fn keys(value: &Value, expected: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|v| v.len() == expected.len() && expected.iter().all(|k| v.contains_key(*k)))
}
fn id(value: &Value, extra: &str) -> bool {
    value.as_str().is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 128
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
    })
}
fn text(value: &Value, limit: usize) -> bool {
    value
        .as_str()
        .is_some_and(|s| s.chars().count() <= limit && !s.chars().any(control))
}
pub(super) fn valid(value: &Value) -> bool {
    if !id(&value["session"], "_-") {
        return false;
    }
    if value["kind"] == "handoff" {
        return keys(
            value,
            &["kind", "session", "request", "thread", "instruction"],
        ) && id(&value["request"], "_.:-")
            && value["thread"].as_str().is_some_and(|s| {
                s.len() == 12
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            && value["instruction"]
                == "Copy this envelope unchanged into the background agent's prompt.";
    }
    keys(value, &["kind", "session", "at", "agents"])
        && value["kind"] == "context"
        && value["at"]
            .as_f64()
            .is_some_and(|v| v.is_finite() && v.abs() < 1e18)
        && value["agents"].is_array()
        && array(&value["agents"]).len() <= 12
        && array(&value["agents"]).iter().all(|a| {
            keys(
                a,
                &[
                    "id",
                    "parent",
                    "type",
                    "model",
                    "status",
                    "edits",
                    "overlaps",
                    "completion",
                ],
            ) && id(&a["id"], "_.:@-")
                && (a["parent"].is_null() || id(&a["parent"], "_.:@-"))
                && [
                    "pending",
                    "running",
                    "waiting",
                    "idle",
                    "completed",
                    "failed",
                    "killed",
                    "unknown",
                ]
                .contains(&string(&a["status"]))
                && ["type", "model"]
                    .iter()
                    .all(|k| a[k].is_null() || text(&a[k], 300))
                && ["edits", "overlaps"].iter().all(|k| {
                    a[k].is_array()
                        && array(&a[k]).len() <= 8
                        && array(&a[k]).iter().all(|p| text(p, 300))
                })
                && (a["completion"].is_null() || {
                    let c = &a["completion"];
                    keys(c, &["turn_id", "reason", "answer"])
                        && id(&c["turn_id"], "_.:@-")
                        && ["answer", "aborted", "refusal", "error"].contains(&string(&c["reason"]))
                        && text(&c["answer"], 700)
                })
        })
}

pub(super) struct Envelope {
    pub start: usize,
    pub end: usize,
    pub payload: Value,
}
pub(super) fn parse(text: &str) -> Vec<Envelope> {
    if !text.contains("[[CSR:AGENTS:v1]]") {
        return vec![];
    }
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    let line = |i: usize| {
        let line = lines[i].strip_suffix('\n').unwrap_or(lines[i]);
        line.strip_suffix('\r').unwrap_or(line)
    };
    let mut found = vec![];
    let (mut i, mut start) = (0, 0);
    while i < lines.len() {
        let head = if i == 0 {
            line(i).strip_prefix('\u{feff}').unwrap_or(line(i)) // one BOM, as agents.py
        } else {
            line(i)
        };
        if i + 2 < lines.len()
            && head == "[[CSR:AGENTS:v1]]"
            && line(i + 2) == "[[/CSR:AGENTS:v1]]"
            && !line(i + 1).contains('\r')
        {
            let bytes = lines[i..i + 3].iter().map(|s| s.len()).sum::<usize>();
            let size = bytes + usize::from(!lines[i + 2].ends_with('\n'));
            if size <= 16384 {
                if let Ok(payload) = serde_json::from_str::<Value>(line(i + 1)) {
                    if valid(&payload) {
                        found.push(Envelope {
                            start,
                            end: start + bytes,
                            payload,
                        });
                        start += bytes;
                        i += 3;
                        continue;
                    }
                }
            }
        }
        start += lines[i].len();
        i += 1;
    }
    found
}
pub fn strip(text: &str) -> String {
    let mut out = String::new();
    let mut at = 0;
    for e in parse(text) {
        out.push_str(&text[at..e.start]);
        at = e.end;
    }
    out.push_str(&text[at..]);
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn valid_agent_envelopes_are_not_typed_words_and_surrounding_text_stays_exact() {
        let envelope = concat!(
            "[[CSR:AGENTS:v1]]\r\n",
            "{\"kind\":\"context\",\"session\":\"s\",\"at\":1,\"agents\":[]}\r\n",
            "[[/CSR:AGENTS:v1]]\r\n"
        );
        assert_eq!(super::super::send::strip_forwards(envelope), "");
        let surrounding = format!("before  \r\n{envelope}  after\n");
        assert_eq!(
            super::super::send::strip_forwards(&surrounding),
            "before  \r\n  after\n"
        );
        let invalid = envelope.replace("\"session\":\"s\",", "");
        assert_eq!(strip(&invalid), invalid);
    }
    #[test]
    fn one_leading_bom_is_allowed_before_an_envelope_and_two_are_not() {
        let envelope = concat!(
            "[[CSR:AGENTS:v1]]\n",
            "{\"kind\":\"context\",\"session\":\"s\",\"at\":1,\"agents\":[]}\n",
            "[[/CSR:AGENTS:v1]]\n"
        );
        assert_eq!(strip(&format!("\u{feff}{envelope}")), "");
        let two = format!("\u{feff}\u{feff}{envelope}");
        assert_eq!(strip(&two), two);
    }
}

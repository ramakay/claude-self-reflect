use super::{
    array,
    paths::{read_json, Paths},
    render::text::{control, fit, one_line},
    string,
};
use crate::process::CommandDeadline;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
    process::Command,
};

pub fn straight(text: &str) -> String {
    one_line(&text.replace(['“', '”'], "\""), usize::MAX)
}
pub fn closed_send(head: &str, body: &str, most: usize) -> String {
    let head = one_line(head, usize::MAX) + " ";
    let room = most.saturating_sub(head.chars().count() + 2).max(1);
    format!("{head}“{}”", fit(&straight(body), room))
}
pub fn strip_forwards(text: &str) -> String {
    let text = super::envelopes::strip(text);
    crate::extraction::provenance::strip_companion_forwards(&text).unwrap_or(text)
}
pub fn own_words(text: &str) -> String {
    static OPEN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"<(pasted_content|system-reminder|command-[\w-]+|local-command-[\w-]+)\b[^>]*>",
        )
        .unwrap()
    });
    let mut typed = String::new();
    let mut at = 0;
    for cap in OPEN.captures_iter(text) {
        let open = cap.get(0).unwrap();
        if open.start() < at {
            continue;
        }
        let close = format!("</{}>", &cap[1]);
        let end = text[open.end()..]
            .find(&close)
            .map(|i| open.end() + i + close.len());
        if let Some(end) = end {
            typed.push_str(&text[at..open.start()]);
            typed.push(' ');
            at = end;
        } else if matches!(&cap[1], "pasted_content" | "system-reminder") {
            typed.push_str(&text[at..open.start()]);
            at = text.len();
            break;
        }
    }
    typed.push_str(&text[at..]);
    strip_forwards(&typed)
        .lines()
        .filter(|line| !line.trim_start().starts_with('>'))
        .collect::<Vec<_>>()
        .join("\n")
}
pub fn typed_by_you(path: &Path, at: u64, quote: Option<&str>) -> bool {
    if path.to_string_lossy().contains("/subagents/") {
        return false;
    }
    let read = || -> Option<Value> {
        let mut f = std::fs::File::open(path).ok()?;
        let mut start = at;
        while start > 0 {
            let step = start.min(65536);
            f.seek(SeekFrom::Start(start - step)).ok()?;
            let mut bytes = vec![0; step as usize];
            f.read_exact(&mut bytes).ok()?;
            if let Some(cut) = bytes.iter().rposition(|b| *b == b'\n') {
                start = start - step + cut as u64 + 1;
                break;
            }
            start -= step;
        }
        f.seek(SeekFrom::Start(start)).ok()?;
        let mut line = String::new();
        BufReader::new(f).read_line(&mut line).ok()?;
        sonic_rs::from_str(&line).ok()
    };
    let Some(e) = read() else {
        return false;
    };
    let text = super::session::message_text(&e["message"]);
    let own = own_words(&text);
    e["type"] == "user"
        && e["isSidechain"] != true
        && e["isMeta"] != true
        && e["entrypoint"].as_str().unwrap_or("cli") == "cli"
        && e["userType"].as_str().unwrap_or("external") == "external"
        && !text.trim_start().starts_with('<')
        && !own.trim().is_empty()
        && quote.is_none_or(|q| {
            own.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains(
                    &q.split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .chars()
                        .take(80)
                        .collect::<String>(),
                )
        })
}
pub fn herdr(args: &[&str]) -> Option<String> {
    let binary = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let out = Command::new(binary)
        .args(args)
        .output_with_deadline(std::time::Duration::from_secs(5))
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}
pub fn send_text(agent: Option<&str>, text: &str) -> bool {
    agent.is_some_and(|a| herdr(&["pane", "send-text", a, &one_line(text, 4000)]).is_some())
}
pub fn claude_in_front(agent: &str) -> bool {
    let Some(info) = herdr(&["pane", "process-info", "--pane", agent])
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    else {
        return false;
    };
    let p = &info["result"]["process_info"];
    array(&p["foreground_processes"])
        .iter()
        .find(|r| r["pid"] == p["foreground_process_group_id"])
        .is_some_and(|r| {
            [&r["argv0"], &r["argv"][0]].iter().any(|v| {
                Path::new(string(v))
                    .file_name()
                    .is_some_and(|s| s == "claude")
            })
        })
}
pub fn send_draft(agent: Option<&str>, text: &str, named: Option<&str>) -> bool {
    let Some(agent) = agent.filter(|a| Some(*a) == named) else {
        return false;
    };
    let lines: Vec<String> = text
        .split('\n')
        .map(|s| s.chars().filter(|c| !control(*c)).collect())
        .collect();
    if lines.iter().any(|s| s.chars().count() > 4000) || !claude_in_front(agent) {
        return false;
    }
    for (i, line) in lines.iter().enumerate() {
        if i > 0 && herdr(&["pane", "send-keys", agent, "ctrl+j"]).is_none() {
            return false;
        }
        if !line.is_empty() && herdr(&["pane", "send-text", agent, line]).is_none() {
            return false;
        }
    }
    true
}
pub fn attached_session(paths: &Paths, agent: &str) -> Option<String> {
    let info: Value =
        serde_json::from_str(&herdr(&["pane", "process-info", "--pane", agent])?).ok()?;
    let p = &info["result"]["process_info"];
    let leader = array(&p["foreground_processes"])
        .iter()
        .find(|r| r["pid"] == p["foreground_process_group_id"])?;
    let args = array(&leader["argv"]);
    if args.len() < 2 || Path::new(string(&args[0])).file_name()? != "claude" {
        return None;
    }
    let job = if args[1] == "attach" {
        args[2..]
            .iter()
            .map(string)
            .find(|s| !s.starts_with('-'))?
            .to_owned()
    } else if args[1] == "agents" {
        let v: Value = serde_json::from_str(&herdr(&["pane", "get", agent])?).ok()?;
        let shown = &v["result"]["pane"]["agent_session"];
        if shown["kind"] != "id" {
            return None;
        }
        string(&shown["value"]).to_owned()
    } else {
        return None;
    };
    if job.is_empty() || !job.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return None;
    }
    let state = read_json(
        &paths
            .home
            .join(".claude/jobs")
            .join(job.chars().take(8).collect::<String>())
            .join("state.json"),
    );
    let sid = state["sessionId"].as_str()?;
    (sid.starts_with(&job) && sid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
        .then(|| sid.into())
}
pub fn keep_like(paths: &Paths, act: &Value, liked: bool, now: f64) -> std::io::Result<()> {
    let key = act["like"].as_str().unwrap_or(string(&act["key"]));
    super::private::append(
        &paths.keep.join("likes.jsonl"),
        &(super::private::dumps(&json!({
            "key": key,
            "source": act["source"],
            "title": act["title"],
            "liked": liked,
            "at": now,
        })) + "\n"),
        &paths.keep,
    )
}
pub fn read_likes(paths: &Paths) -> Value {
    let mut likes = json!({});
    for line in std::fs::read_to_string(paths.keep.join("likes.jsonl"))
        .unwrap_or_default()
        .lines()
    {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if let Some(key) = v["key"].as_str() {
                likes[key] = json!(v["liked"] == true);
            }
        }
    }
    likes
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_a_send_stays_closed_when_cut() {
        let s = closed_send(
            "CSR's story of this session, rev 1, quoted:",
            &"word ".repeat(100),
            100,
        );
        assert_eq!(s.chars().count(), 100);
        assert!(s.ends_with("…”"));
    }
    #[test]
    fn test_only_the_main_users_words_are_rules() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.jsonl");
        for (kind, meta, side, want) in [
            ("user", false, false, true),
            ("assistant", false, false, false),
            ("user", true, false, false),
            ("user", false, true, false),
        ] {
            std::fs::write(
                &p,
                json!({
                    "type": kind,
                    "isMeta": meta,
                    "isSidechain": side,
                    "message": {
                        "content": "keep the tests",
                    },
                })
                .to_string()
                    + "\n",
            )
            .unwrap();
            assert_eq!(typed_by_you(&p, 10, Some("keep the tests")), want);
        }
    }
    #[test]
    fn test_a_draft_fails_closed_without_the_named_agent() {
        assert!(!send_draft(Some("other"), "one\ntwo", Some("named")));
    }
}

#[cfg(test)]
mod forward_tests {
    use super::*;
    #[test]
    fn test_forwards_are_spans_and_your_words_stay() {
        let sent = closed_send(
            "CSR's story of this session, rev 3, quoted:",
            "The pane stands ready.",
            700,
        );
        assert_eq!(
            strip_forwards(&format!("before {sent} after")),
            "before after"
        );
    }
    #[test]
    fn test_pasted_or_injected_words_are_never_yours() {
        assert_eq!(
            own_words("mine <pasted_content>other agent</pasted_content> still mine"),
            "mine   still mine"
        );
        assert_eq!(
            own_words("mine <system-reminder>injected and cut off"),
            "mine "
        );
        assert_eq!(own_words("mine\n> another quote"), "mine");
    }
}

pub fn agent_pane(given: Option<&str>) -> Option<String> {
    if let Some(given) = given.filter(|s| !s.is_empty()) {
        return Some(given.into());
    }
    let me = std::env::var("HERDR_PANE_ID").ok()?;
    let said = herdr(&["pane", "neighbor", "--pane", &me, "--direction", "left"])?;
    let value: Value = serde_json::from_str(&said).ok()?;
    value["result"]["pane"]["pane_id"]
        .as_str()
        .or_else(|| value["result"]["pane_id"].as_str())
        .map(str::to_owned)
}
pub fn focus_back() {
    if let Ok(me) = std::env::var("HERDR_PANE_ID") {
        let _ = herdr(&["pane", "focus", "--pane", &me, "--direction", "left"]);
    }
}
#[cfg(test)]
mod python_cases {
    use super::*;
    const CARD: &str = "CSR's compaction card (compacted by csr): Never skip validation.";
    #[test]
    fn test_the_cards_draft_and_other_forwards_are_never_typed_by_you() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.jsonl");
        std::fs::write(
            &p,
            json!({
                "type": "user",
                "message": {
                    "content": CARD,
                },
            })
            .to_string()
                + "\n",
        )
        .unwrap();
        assert!(!typed_by_you(&p, 0, None));
        assert!(!typed_by_you(&p, 0, Some("Never skip validation.")));
        std::fs::write(
            &p,
            json!({
                "type": "user",
                "message": {
                    "content": format!("{CARD}\nNever compact on a Friday."),
                },
            })
            .to_string()
                + "\n",
        )
        .unwrap();
        assert!(typed_by_you(&p, 0, Some("Never compact on a Friday.")));
        assert!(!typed_by_you(&p, 0, Some("Never skip validation.")));
        assert_eq!(own_words(&format!("{CARD}\nmine")).trim(), "mine");
    }
    #[test]
    fn test_talk_about_a_forward_is_yours_and_every_forward_goes() {
        for text in [
            "CSR's compaction card (compacted by csr) is wrong; never drop \
                my pending task.",
            "Please change CSR's compaction card (the one in the sidebar) to \
                show savings in tokens.",
            "CSR's dreamer asked this while you worked yesterday, but I \
                wrote today's question myself.",
        ] {
            assert_eq!(own_words(text), text);
        }
        let band = "What if the pane took its colours from the deck's \
            inks?\n\nCSR's dreamer asked this while you worked. Its \
            reasoning:\n\n> **Share the palette**\n> * Two surfaces read \
            the same inks.";
        assert_eq!(
            own_words(&format!("{band}\n\nNo.\n\n{band}\n\nAnd stop asking.")),
            "No.\n\nAnd stop asking."
        );
        assert_eq!(own_words(&format!("2{CARD}")), "");
        assert_eq!(
            own_words("CSR's story of this session, rev 3: We moved the queue.\nyes"),
            "yes"
        );
        assert_eq!(
            own_words(
                "Here is a note offered by a side agent:\n\n> Heads up · x\n>\n> **T**\n\nwhy?"
            ),
            "why?"
        );
    }
    #[test]
    fn test_a_send_is_a_span_its_closing_quote_ends_and_a_band_forward_goes_line_by_line() {
        let head = "CSR's dreamer asked this while you worked: “What if?” Its reasoning: ";
        let cut = format!("{head}{}…", "x".repeat(699 - head.chars().count()));
        assert_eq!(cut.chars().count(), 700);
        assert_eq!(
            own_words(&format!("{cut} Keep the deployment frozen.")),
            "Keep the deployment frozen."
        );
        assert_eq!(
            own_words("Please check this: CSR's compaction card (compacting): Summarizing 278k."),
            "Please check this:"
        );
        let band = "What if we reuse the palette?\n\nCSR's dreamer asked this while \
            you worked. Its reasoning:\n\n> **Reuse**\n> Shared colours.";
        assert_eq!(
            own_words(&format!("Keep the deployment frozen.\n{band}")),
            "Keep the deployment frozen."
        );
        assert_eq!(
            strip_forwards(&format!("{band}\nNo, keep the current palette.")),
            "No, keep the current palette."
        );
    }
    #[test]
    fn test_what_the_pane_sends_comes_back_as_exactly_the_reply_typed_beside_it() {
        let story = closed_send(
            "CSR's story of this session, rev 2, quoted:",
            "We moved the “queue”.",
            700,
        );
        assert_eq!(
            story,
            "CSR's story of this session, rev 2, quoted: “We moved the \"queue\".”"
        );
        let sends = [
            story,
            closed_send(
                "CSR's compaction card (compacted by csr), quoted:",
                "The queue stayed intact.",
                1200,
            ),
            closed_send(
                "CSR's dreamer asked this while you worked, quoted: “What if?” Its reasoning:",
                "Say fresh. Read the queue.",
                700,
            ),
        ];
        for sent in sends {
            assert_eq!(
                own_words(&format!("Before: {sent} after, keep it frozen.")),
                "Before: after, keep it frozen."
            );
            assert_eq!(own_words(&format!("2{sent}")), "");
            assert_eq!(own_words(&format!("{sent}\nmine")), "mine");
        }
        let long = closed_send(
            "CSR's compaction card (compacting), quoted:",
            &"y".repeat(2000),
            1200,
        );
        assert_eq!(long.chars().count(), 1200);
        assert!(long.ends_with("…”"));
        assert_eq!(own_words(&format!("{long} mine")), "mine");
        let cjk = closed_send(
            "CSR's compaction card (compacting), quoted:",
            "カードの本文。「キュー」",
            1200,
        );
        assert_eq!(own_words(&format!("{cjk}　次は？")), "次は？");
    }
    #[test]
    fn test_v2_question_ends_its_line_and_v1_never_searches_for_a_closing_quote() {
        for question in ["What if we reuse the palette?", "What if \"Queue\" moved?"] {
            let band = format!(
                "“{question}”\n\nCSR's dreamer asked this while you worked, \
                quoted. Its reasoning:\n\n> **Reuse**\n> Shared."
            );
            assert_eq!(
                strip_forwards(&format!(
                    "Keep the deployment frozen. {band}\nNo, keep the current palette."
                )),
                "Keep the deployment frozen.\n\nNo, keep the current palette."
            );
            assert_eq!(own_words(&format!("2{band}\n\nNo.")), "No.");
        }
        for legacy in [
            "CSR's compaction card (compacting): “Queue” was moved. Never skip validation.",
            "CSR's story of this session, rev 3: “Queue” moved. Never wait.",
            "CSR's compaction card (compacting): 「キュー」が移動。",
        ] {
            assert_eq!(own_words(legacy), "");
        }
    }
    #[test]
    fn test_every_stage_of_the_card_is_one_form_for_the_pane_the_mod_and_the_engine() {
        for stage in [
            "compacting soon",
            "compacting",
            "compaction skipped",
            "compacted by csr",
        ] {
            let send = closed_send(
                &format!("CSR's compaction card ({stage}), quoted:"),
                "A small quoted card.",
                1200,
            );
            assert_eq!(own_words(&send), "");
            assert_eq!(own_words(&format!("{send}\nmine")), "mine");
        }
    }
}

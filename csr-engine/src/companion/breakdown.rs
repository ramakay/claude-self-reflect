//! What fills the context, and what the last turn added, from the transcript alone.
//!
//! No request is made and nothing is measured by Claude Code: the total is the newest reply's own
//! usage, and the parts are the text the transcript holds, sized by characters. Tool results, text
//! (the prompts, the replies, the tool calls) and what CSR's hooks injected are counted as the
//! rows go by. What the characters cannot explain is the floor under them (the system prompt,
//! the tool list, the memory files), taken from the first reply after the start or a compaction.
//! The parts are scaled to the reply's real total, so every figure but the total is an estimate,
//! and the card says so.

use super::pyfmt::short;
use serde_json::{json, Value};

/// The rough size of a token in characters, for the parts' shares and the floor.
pub const CHARS_PER_TOKEN: f64 = 3.6;

/// Characters by what they were.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Chars {
    /// Tool results.
    pub tool: u64,
    /// What CSR's hooks put in.
    pub csr: u64,
    /// Everything else the conversation holds: prompts, replies, tool calls, other hooks.
    pub text: u64,
}

impl Chars {
    fn since(self, base: Chars) -> Chars {
        Chars {
            tool: self.tool.saturating_sub(base.tool),
            csr: self.csr.saturating_sub(base.csr),
            text: self.text.saturating_sub(base.text),
        }
    }
    fn sum(self) -> u64 {
        self.tool + self.csr + self.text
    }
    fn json(self) -> Value {
        json!({"tool": self.tool, "csr": self.csr, "text": self.text})
    }
}

/// The running count for one session, kept beside the transcript reader.
#[derive(Clone, Debug, Default)]
pub struct Tally {
    /// Characters since the start or the last compaction.
    seen: Chars,
    /// The newest main-thread reply: its total and the characters before it.
    reply: Option<(f64, Chars)>,
    /// The newest reply before the latest prompt: what "this turn" is measured from.
    turn_from: Option<(f64, Chars)>,
    /// The tokens no characters explain, fixed by the first reply after a start or a compaction.
    floor: Option<f64>,
}

impl Tally {
    pub fn tool_result(&mut self, chars: usize) {
        self.seen.tool += chars as u64;
    }
    pub fn csr(&mut self, chars: usize) {
        self.seen.csr += chars as u64;
    }
    pub fn text(&mut self, chars: usize) {
        self.seen.text += chars as u64;
    }
    /// A prompt came in: the turn it starts is measured from the newest reply.
    pub fn prompt(&mut self) {
        self.turn_from = self.reply;
    }
    /// A compaction ended the old context: everything counted so far is gone.
    pub fn compacted(&mut self) {
        *self = Tally::default();
    }
    /// A main-thread reply reported `total` tokens of context. Returns the kept figures.
    pub fn reply(&mut self, total: f64) -> Value {
        if self.floor.is_none() {
            let explained = self.seen.sum() as f64 / CHARS_PER_TOKEN;
            self.floor = Some((total - explained).clamp(0., total));
        }
        self.reply = Some((total, self.seen));
        json!({
            "total": total,
            "floor": self.floor,
            "chars": self.seen.json(),
            "turn": self.turn_from.map(|(from, base)| json!({
                "from": from,
                "chars": self.seen.since(base).json(),
            })),
        })
    }
}

/// Split `tokens` over `chars` by share. All zero when there are no characters.
fn shared(tokens: f64, chars: Chars) -> [f64; 3] {
    let sum = chars.sum() as f64;
    if sum <= 0. || tokens <= 0. {
        return [0.; 3];
    }
    [chars.tool, chars.text, chars.csr].map(|n| tokens * n as f64 / sum)
}

fn chars_of(v: &Value) -> Chars {
    let n = |k: &str| v[k].as_u64().unwrap_or(0);
    Chars {
        tool: n("tool"),
        csr: n("csr"),
        text: n("text"),
    }
}

/// The figure's breakdown of a context of `tokens` in a window of `size`, from the session's kept
/// figures. `Null` when there is nothing to base it on: no reply has been counted, or the total
/// no longer matches (a compaction since). Every part is in tokens and the parts add up to
/// `size`; `turn` is the last turn's change and its parts, when it grew.
pub fn figure(kept: &Value, tokens: f64, size: f64) -> Value {
    // The kept figures belong to the newest reply: a total far from it is another context.
    let current = kept["total"]
        .as_f64()
        .is_some_and(|t| t > 0. && (tokens - t).abs() <= (t * 0.1).max(2_000.));
    if !(tokens > 0. && size >= tokens && current) {
        return Value::Null;
    }
    let floor = kept["floor"].as_f64().unwrap_or(0.).min(tokens);
    let [tool, text, csr] = shared(tokens - floor, chars_of(&kept["chars"]));
    // Characters that explain nothing leave their share in the floor.
    let floor = tokens - tool - text - csr;
    let turn = kept["turn"]
        .as_object()
        .and_then(|t| {
            let from = t.get("from")?.as_f64()?;
            let grew = tokens - from;
            (grew > 0.).then(|| {
                let [tool, text, csr] =
                    shared(grew, chars_of(t.get("chars").unwrap_or(&Value::Null)));
                json!({"grew": grew, "tool": tool, "text": text, "csr": csr})
            })
        })
        .unwrap_or(Value::Null);
    let caption = caption(&turn);
    json!({
        "caption": caption,
        "floor": floor,
        "text": text,
        "tool": tool,
        "csr": csr,
        "free": size - tokens,
        "total": tokens,
        "size": size,
        "turn": turn,
        "estimated": true,
    })
}

/// The last turn's change in words: what grew, and what it was made of (CSR's share always, the
/// biggest other part with it). The total is the replies' own; the parts are estimates.
fn caption(turn: &Value) -> Option<String> {
    let grew = turn["grew"].as_f64()?;
    let least = (grew * 0.05).max(100.);
    let part = |name: &'static str, key: &str| (name, turn[key].as_f64().unwrap_or(0.));
    let csr = part("CSR", "csr");
    let mut others = [part("tool results", "tool"), part("you + Claude", "text")];
    others.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut shown: Vec<_> = [Some(others[0]), (csr.1 >= least).then_some(csr)]
        .into_iter()
        .flatten()
        .filter(|p| p.1 >= least)
        .collect();
    shown.sort_by(|a, b| b.1.total_cmp(&a.1));
    let parts: Vec<_> = shown
        .iter()
        .map(|(name, n)| format!("{name} {}", short(*n)))
        .collect();
    Some(if parts.is_empty() {
        format!("+{} this turn", short(grew))
    } else {
        format!("+{} this turn: {} est.", short(grew), parts.join(", "))
    })
}

/// The characters one transcript row adds, as (tool results, CSR hooks, other text).
pub fn row_chars(row: &Value) -> Chars {
    let mut out = Chars::default();
    let len = |v: &Value| v.as_str().map_or(0, |s| s.chars().count());
    let message = &row["message"];
    match row["type"].as_str().unwrap_or("") {
        "assistant" | "user" => {
            let content = &message["content"];
            if let Some(text) = content.as_str() {
                out.text += text.chars().count() as u64;
            }
            for part in content.as_array().into_iter().flatten() {
                match part["type"].as_str().unwrap_or("") {
                    "text" => out.text += len(&part["text"]) as u64,
                    "tool_use" => out.text += part["input"].to_string().len() as u64,
                    "tool_result" => {
                        let body = &part["content"];
                        out.tool += match body {
                            Value::String(s) => s.chars().count() as u64,
                            Value::Array(items) => {
                                items.iter().map(|i| len(&i["text"]) as u64).sum()
                            }
                            _ => 0,
                        };
                    }
                    _ => {}
                }
            }
        }
        "attachment" => {
            let a = &row["attachment"];
            if a["type"] == "hook_success" {
                let n = len(&a["content"]) as u64;
                if is_csr_hook(a) {
                    out.csr += n;
                } else {
                    out.text += n;
                }
            }
        }
        _ => {}
    }
    out
}

/// Whether a hook attachment is CSR's: its command or its name says so.
fn is_csr_hook(a: &Value) -> bool {
    let has = |k: &str| {
        a[k].as_str().is_some_and(|s| {
            let s = s.to_ascii_lowercase();
            s.contains("csr-engine") || s.contains("claude-self-reflect")
        })
    };
    has("command") || has("hookName")
}

impl Tally {
    /// Count one transcript row of the main thread.
    pub fn row(&mut self, row: &Value) {
        let c = row_chars(row);
        self.seen.tool += c.tool;
        self.seen.csr += c.csr;
        self.seen.text += c.text;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(total: f64, tool: u64, text: u64, csr: u64) -> Value {
        let mut t = Tally::default();
        t.tool_result(tool as usize);
        t.text(text as usize);
        t.csr(csr as usize);
        t.reply(total)
    }

    #[test]
    fn test_the_parts_add_up_to_the_window_and_the_total_is_the_replys() {
        // 40k tokens, of which characters explain 36k (130k chars / 3.6) after a floor of 4k...
        // the first reply fixes the floor: 40k minus what its own characters explain.
        let mut t = Tally::default();
        t.text(3600);
        t.csr(3600);
        let first = t.reply(30_000.);
        assert_eq!(first["floor"], json!(28_000.));
        // Later: 100k chars of tool results and 20k of text arrive; the reply says 80k.
        t.tool_result(100_000);
        t.text(20_000);
        let now = t.reply(80_000.);
        let f = figure(&now, 80_000., 1_000_000.);
        let sum = ["floor", "text", "tool", "csr", "free"]
            .iter()
            .map(|k| f[k].as_f64().unwrap())
            .sum::<f64>();
        assert!((sum - 1_000_000.).abs() < 1e-6, "{sum}");
        assert_eq!(f["total"], json!(80_000.));
        // The floor fixed at 28k holds; the other 52k are split by characters.
        let chars = (100_000 + 23_600 + 3_600) as f64;
        let tool = 52_000. * 100_000. / chars;
        assert!((f["tool"].as_f64().unwrap() - tool).abs() < 1., "{f}");
        assert!(f["tool"].as_f64() > f["text"].as_f64());
        assert_eq!(f["estimated"], json!(true));
    }

    #[test]
    fn test_the_turn_is_what_grew_since_the_reply_before_the_prompt() {
        let mut t = Tally::default();
        t.text(1000);
        t.reply(40_000.);
        // A prompt, a tool result and CSR's hook, then a reply 9k larger.
        t.prompt();
        t.text(2000);
        t.csr(4000);
        t.tool_result(24_000);
        let k = t.reply(49_000.);
        let f = figure(&k, 49_000., 1_000_000.);
        assert_eq!(f["turn"]["grew"], json!(9_000.));
        let (tool, csr, text) = (
            f["turn"]["tool"].as_f64().unwrap(),
            f["turn"]["csr"].as_f64().unwrap(),
            f["turn"]["text"].as_f64().unwrap(),
        );
        assert!((tool + csr + text - 9_000.).abs() < 1e-6);
        assert!((tool - 9_000. * 24. / 30.).abs() < 1., "{tool}");
        assert!((csr - 9_000. * 4. / 30.).abs() < 1., "{csr}");
        // The first turn has nothing before it to be measured from.
        let mut first = Tally::default();
        first.text(100);
        let k = first.reply(30_000.);
        assert_eq!(figure(&k, 30_000., 1_000_000.)["turn"], Value::Null);
    }

    #[test]
    fn test_a_shrinking_context_or_a_compaction_has_no_turn_and_no_stale_parts() {
        let mut t = Tally::default();
        t.text(500);
        t.reply(60_000.);
        t.prompt();
        let k = t.reply(50_000.);
        assert_eq!(figure(&k, 50_000., 1_000_000.)["turn"], Value::Null);
        t.compacted();
        t.text(900);
        let k = t.reply(20_000.);
        let f = figure(&k, 20_000., 1_000_000.);
        assert_eq!(f["turn"], Value::Null);
        assert_eq!(
            f["floor"].as_f64().unwrap() + f["text"].as_f64().unwrap(),
            20_000.
        );
    }

    #[test]
    fn test_without_a_reply_or_room_there_is_no_breakdown() {
        assert_eq!(figure(&Value::Null, 10_000., 1_000_000.), Value::Null);
        let k = kept(10_000., 10, 10, 10);
        assert_eq!(figure(&k, 0., 1_000_000.), Value::Null);
        assert_eq!(figure(&k, 2_000_000., 1_000_000.), Value::Null);
    }

    #[test]
    fn test_rows_are_sized_by_what_they_carry() {
        let tool = json!({"type":"user","message":{"content":[
            {"type":"tool_result","tool_use_id":"a","content":"x".repeat(500)},
            {"type":"tool_result","tool_use_id":"b","content":[{"type":"text","text":"y".repeat(250)}]}]}});
        assert_eq!(
            row_chars(&tool),
            Chars {
                tool: 750,
                csr: 0,
                text: 0
            }
        );
        let prompt = json!({"type":"user","message":{"content":"hello there"}});
        assert_eq!(row_chars(&prompt).text, 11);
        let reply = json!({"type":"assistant","message":{"content":[
            {"type":"text","text":"done"},
            {"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}});
        let c = row_chars(&reply);
        assert!(c.text > 4 && c.tool == 0 && c.csr == 0, "{c:?}");
        let csr = json!({"type":"attachment","attachment":{"type":"hook_success",
            "hookName":"UserPromptSubmit","command":"/usr/local/bin/csr-engine hook prompt-submit",
            "content":"z".repeat(1100)}});
        assert_eq!(
            row_chars(&csr),
            Chars {
                tool: 0,
                csr: 1100,
                text: 0
            }
        );
        let other = json!({"type":"attachment","attachment":{"type":"hook_success",
            "hookName":"UserPromptSubmit","command":"/Users/me/.claude/hooks/nudge.sh",
            "content":"nudge"}});
        assert_eq!(
            row_chars(&other),
            Chars {
                tool: 0,
                csr: 0,
                text: 5
            }
        );
        let silent = json!({"type":"attachment","attachment":{"type":"hook_success",
            "command":"/usr/local/bin/csr-engine hook stop","content":""}});
        assert_eq!(row_chars(&silent), Chars::default());
    }

    #[test]
    fn test_the_caption_names_the_turns_biggest_part_and_csr_and_says_est() {
        let mut t = Tally::default();
        t.text(1000);
        t.reply(40_000.);
        t.prompt();
        t.text(6_840);
        t.csr(3_960);
        t.tool_result(21_600);
        let f = figure(&t.reply(49_000.), 49_000., 1_000_000.);
        assert_eq!(
            f["caption"],
            json!("+9k this turn: tool results 6k, CSR 1.1k est.")
        );
        // CSR below a hundredth of the turn is not worth a name; a quiet turn names only its size.
        let mut t = Tally::default();
        t.reply(40_000.);
        t.prompt();
        t.tool_result(36_000);
        t.csr(36);
        let f = figure(&t.reply(50_000.), 50_000., 1_000_000.);
        assert_eq!(f["caption"], json!("+10k this turn: tool results 10k est."));
        let mut t = Tally::default();
        t.reply(40_000.);
        t.prompt();
        let f = figure(&t.reply(40_500.), 40_500., 1_000_000.);
        assert_eq!(f["caption"], json!("+500 this turn"));
    }

    #[test]
    fn test_a_total_far_from_the_kept_reply_belongs_to_another_context() {
        let mut t = Tally::default();
        t.text(100);
        let k = t.reply(80_000.);
        assert!(figure(&k, 80_000., 1_000_000.).is_object());
        assert!(figure(&k, 81_000., 1_000_000.).is_object());
        // A compaction left the figure at 20k while the kept reply is still the old one.
        assert_eq!(figure(&k, 20_000., 1_000_000.), Value::Null);
    }
}

use super::*;

fn text_block(text: &str) -> serde_json::Value {
    serde_json::json!({"type": "text", "text": text})
}

const EMISSION: &str = "recap [2m ago]: Fixed it. [[CSR:RECAP]]";
/// The text `askClaude` (the removed `dreamer.ts`) sends: the
/// question, a blank line, the attribution, a blank line, the quoted reasoning.
const DREAMER_MESSAGE: &str = "What if the pane took its colours from pigment's nyt inks?\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\n> **Share the palette**\n>\n> Two surfaces already read the same inks.";

const YSK_FORWARD: &str = "Here is a note offered by a side agent:\n\n> **Retry storm fixed**\n>\n> Heads up \u{b7} the importer retry loop was capped [commit f154ead].\n> It no longer re-queues failed chunks.";

use crate::provenance::Speaker;

fn write_transcript(dir: &Path, name: &str, first_user: &str) -> PathBuf {
    let path = dir.join(name);
    let lines = [
        serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": first_user}),
        serde_json::json!({
            "type": "user",
            "timestamp": "2026-09-15T00:00:00Z",
            "message": {"content": first_user}
        }),
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-09-15T00:00:05Z",
            "message": {"content": [{"type": "text", "text": "{\"intent\": \"reply the importer must never index\"}"}]}
        }),
    ];
    std::fs::write(
        &path,
        lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    path
}

fn write_rows(dir: &Path, name: &str, rows: &[serde_json::Value]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        rows.iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    path
}

/// A `Read` of CSR's own emitter and tool sources holds the format strings
/// and test fixtures verbatim. It is the code, not CSR's output: it must
/// survive import whole (the 552-legit-chunks over-match, again).
#[test]
fn a_read_of_the_emitter_source_survives_import() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("read-source.jsonl");
    let sources = [
        ("read-format", include_str!("../format/mod.rs")),
        ("read-tools", include_str!("../mcp/tools.rs")),
        (
            "read-prose",
            "The parser fails on <summary count=\"5\" relevance=\"partial\">.\nFix: escape attributes before decoding.",
        ),
    ];
    let mut lines = Vec::new();
    for (id, text) in &sources {
        lines.push(serde_json::json!({
            "type": "assistant", "timestamp": "2026-10-08T12:00:00Z",
            "message": {"content": [{"type": "tool_use", "id": id, "name": "Read", "input": {"file_path": "/x/y.rs"}}]}
        }));
        lines.push(serde_json::json!({
            "type": "user", "timestamp": "2026-10-08T12:00:01Z",
            "message": {"content": [{"type": "tool_result", "tool_use_id": id, "content": text}]}
        }));
    }
    std::fs::write(
        &path,
        lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let parsed = parse_jsonl_file_with_stats(&path, "test").unwrap();
    assert_eq!(parsed.suppression.csr_tool_blocks_suppressed, 0);
    let content = parsed
        .chunks
        .into_iter()
        .map(|chunk| chunk.content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(content.contains("The parser fails on"), "{content}");
}

// Fixture secrets are assembled at runtime; no credential-shaped literal
// sits in the source tree.
struct FakeSecrets {
    github: String,
    anthropic: String,
    aws: String,
    stripe: String,
}

impl FakeSecrets {
    fn new() -> Self {
        Self {
            github: format!("{}{}", "ghp_", "A".repeat(36)),
            anthropic: format!("{}{}", "sk-ant-", "aB3dE5gH7jK9mN1pQ3rS5tU7".repeat(2)),
            aws: format!("{}{}", "AKIA", "Q".repeat(16)),
            stripe: format!("{}{}", "sk_live_", "Z".repeat(24)),
        }
    }

    fn all(&self) -> [&str; 4] {
        [&self.github, &self.anthropic, &self.aws, &self.stripe]
    }
}

fn write_secret_transcript(path: &Path, secrets: &FakeSecrets) {
    let lines = [
        serde_json::json!({
            "type": "user",
            "uuid": "u-1",
            "timestamp": "2026-10-14T10:00:00Z",
            "message": {"content": format!("please use my token {} for the deploy", secrets.github)}
        }),
        serde_json::json!({
            "type": "assistant",
            "uuid": "a-1",
            "timestamp": "2026-10-14T10:00:05Z",
            "message": {"content": [
                {"type":"text","text": format!("Stored the key {} as asked.", secrets.anthropic)},
                {"type":"tool_use","id":"t-1","name":"Bash","input":{
                    "command": format!("AWS_ACCESS_KEY_ID={} aws s3 ls", secrets.aws)}}
            ]}
        }),
        serde_json::json!({
            "type": "user",
            "uuid": "u-2",
            "timestamp": "2026-10-14T10:00:09Z",
            "message": {"content": [
                {"type":"tool_result","tool_use_id":"t-1",
                 "content": format!("billing key is {} in this listing", secrets.stripe)}
            ]}
        }),
    ];
    std::fs::write(
        path,
        lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
}

fn joined_input_text(inputs: &crate::storage::artifact_provenance::InputEnvelope) -> String {
    inputs
        .inputs()
        .iter()
        .map(|input| input.text())
        .collect::<Vec<_>>()
        .join("|")
}

#[path = "tests/mixed_tests.rs"]
mod mixed_tests;

#[path = "tests/tool_csr_tests.rs"]
mod tool_csr_tests;

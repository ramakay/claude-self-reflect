//! Authored-work citations from a parent Claude session to successful edits in
//! its subagent transcripts. Receipts point at the symbol bytes inside the
//! edit payload, never at a raw mention elsewhere in the transcript.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::value::RawValue;

use super::subagent_iface::{audit_bar_clause, DreamProvenance};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SubagentCitation {
    pub session_id: String,
    pub transcript_path: PathBuf,
    pub byte_offset: usize,
    pub needle: String,
    pub tool_name: String,
}

#[derive(Debug, Deserialize)]
struct TranscriptLine<'a> {
    #[serde(rename = "type")]
    record_type: Option<&'a str>,
    cwd: Option<&'a str>,
    #[serde(borrow)]
    message: Option<TranscriptMessage<'a>>,
}

#[derive(Debug, Deserialize)]
struct TranscriptMessage<'a> {
    role: Option<&'a str>,
    #[serde(default, borrow)]
    content: Vec<&'a RawValue>,
}

#[derive(Debug, Deserialize)]
struct TranscriptBlock<'a> {
    #[serde(rename = "type")]
    block_type: Option<&'a str>,
    id: Option<&'a str>,
    name: Option<&'a str>,
    tool_use_id: Option<&'a str>,
    is_error: Option<bool>,
    #[serde(borrow)]
    input: Option<&'a RawValue>,
}

#[derive(Debug, Deserialize)]
struct EditInput<'a> {
    file_path: Option<&'a str>,
    notebook_path: Option<&'a str>,
    #[serde(borrow)]
    old_string: Option<&'a RawValue>,
    #[serde(borrow)]
    new_string: Option<&'a RawValue>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
    #[serde(borrow)]
    new_source: Option<&'a RawValue>,
    #[serde(default, borrow)]
    edits: Vec<MultiEditInput<'a>>,
}

#[derive(Debug, Deserialize)]
struct MultiEditInput<'a> {
    #[serde(borrow)]
    old_string: Option<&'a RawValue>,
    #[serde(borrow)]
    new_string: Option<&'a RawValue>,
}

#[derive(Debug)]
struct EditTarget {
    file: String,
    cwd: Option<String>,
}

#[derive(Debug)]
struct AuthoredCandidate {
    tool_use_id: Option<String>,
    tool_name: String,
    target: EditTarget,
    byte_offset: usize,
}

#[derive(Debug)]
struct TranscriptEvidence {
    session_id: String,
    transcript_path: PathBuf,
    targets: Vec<EditTarget>,
    candidates: Vec<AuthoredCandidate>,
    errored_tool_uses: BTreeSet<String>,
}

/// JSON persisted on a dream row. The provenance fields remain top-level so
/// existing `DreamProvenance` consumers can read them directly, while the
/// citations retain the skeptical reader's byte-slice receipts.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DreamCitationEvidence {
    #[serde(flatten)]
    pub provenance: DreamProvenance,
    pub citations: Vec<SubagentCitation>,
    pub bar_clause_met: bool,
}

impl DreamCitationEvidence {
    pub fn audited(parent_sessions: Vec<String>, citations: Vec<SubagentCitation>) -> Self {
        let parent_sessions = parent_sessions
            .into_iter()
            .filter(|session| !session.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let subagent_sessions = citations
            .iter()
            .map(|citation| citation.session_id.clone())
            .filter(|session| !session.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let provenance = DreamProvenance {
            parent_sessions,
            subagent_sessions,
            chunk_ids: Vec::new(),
            attribution: "transcript_path",
        };
        Self {
            bar_clause_met: audit_bar_clause(&Some(provenance.clone())).is_ok(),
            provenance,
            citations,
        }
    }
}

pub fn subagent_transcripts_for_session(
    projects_root: &Path,
    project_dir: &str,
    parent_session_id: &str,
) -> Vec<PathBuf> {
    let directory = projects_root
        .join(project_dir)
        .join(parent_session_id)
        .join("subagents");
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut transcripts: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("agent-") && name.ends_with(".jsonl"))
        })
        .collect();
    transcripts.sort();
    transcripts
}

pub fn cite_subagents_for_symbol(
    transcripts: &[PathBuf],
    symbol: &str,
    dream_file: &str,
) -> Vec<SubagentCitation> {
    let mut transcripts: Vec<&PathBuf> = transcripts.iter().collect();
    transcripts.sort();
    let evidence: Vec<TranscriptEvidence> = transcripts
        .into_iter()
        .filter_map(|path| parse_transcript(path, symbol).ok().flatten())
        .collect();
    let all_targets: Vec<&EditTarget> = evidence
        .iter()
        .flat_map(|transcript| transcript.targets.iter())
        .collect();
    let mut citations = Vec::new();
    let mut cited_sessions = BTreeSet::new();
    for transcript in &evidence {
        if cited_sessions.contains(&transcript.session_id) {
            continue;
        }
        let authored = transcript.candidates.iter().find(|candidate| {
            candidate
                .tool_use_id
                .as_ref()
                .is_none_or(|id| !transcript.errored_tool_uses.contains(id))
                && target_matches_dream(&candidate.target, dream_file, &all_targets)
        });
        let Some(authored) = authored else {
            continue;
        };
        cited_sessions.insert(transcript.session_id.clone());
        citations.push(SubagentCitation {
            session_id: transcript.session_id.clone(),
            transcript_path: transcript.transcript_path.clone(),
            byte_offset: authored.byte_offset,
            needle: symbol.to_string(),
            tool_name: authored.tool_name.clone(),
        });
    }
    citations
}

fn parse_transcript(path: &Path, symbol: &str) -> io::Result<Option<TranscriptEvidence>> {
    let Some(session_id) = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.strip_prefix("agent-"))
        .filter(|session_id| !session_id.is_empty())
        .map(str::to_string)
    else {
        return Ok(None);
    };
    let mut reader = BufReader::new(File::open(path)?);
    let mut line = Vec::new();
    let mut line_offset = 0_usize;
    let mut targets = Vec::new();
    let mut candidates = Vec::new();
    let mut errored_tool_uses = BTreeSet::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        if let Ok(text) = std::str::from_utf8(&line) {
            collect_line_evidence(
                text.trim_end_matches(['\r', '\n']),
                line_offset,
                symbol,
                &mut targets,
                &mut candidates,
                &mut errored_tool_uses,
            );
        }
        let Some(next_offset) = line_offset.checked_add(read) else {
            return Ok(None);
        };
        line_offset = next_offset;
    }
    candidates.sort_by_key(|candidate| candidate.byte_offset);
    Ok(Some(TranscriptEvidence {
        session_id,
        transcript_path: path.to_path_buf(),
        targets,
        candidates,
        errored_tool_uses,
    }))
}

fn collect_line_evidence(
    line: &str,
    line_offset: usize,
    symbol: &str,
    targets: &mut Vec<EditTarget>,
    candidates: &mut Vec<AuthoredCandidate>,
    errored_tool_uses: &mut BTreeSet<String>,
) {
    let Ok(record) = serde_json::from_str::<TranscriptLine<'_>>(line) else {
        return;
    };
    let Some(message) = record.message else {
        return;
    };
    for raw_block in message.content {
        let Ok(block) = serde_json::from_str::<TranscriptBlock<'_>>(raw_block.get()) else {
            continue;
        };
        if block.block_type == Some("tool_result") && block.is_error == Some(true) {
            if let Some(tool_use_id) = block.tool_use_id.filter(|id| !id.is_empty()) {
                errored_tool_uses.insert(tool_use_id.to_string());
            }
            continue;
        }
        if record.record_type != Some("assistant")
            || message.role != Some("assistant")
            || block.block_type != Some("tool_use")
        {
            continue;
        }
        let Some(tool_name) = block.name.filter(|name| is_edit_tool(name)) else {
            continue;
        };
        let Some(raw_input) = block.input else {
            continue;
        };
        let Ok(input) = serde_json::from_str::<EditInput<'_>>(raw_input.get()) else {
            continue;
        };
        let target_file = match tool_name {
            "NotebookEdit" => input.notebook_path,
            _ => input.file_path,
        }
        .filter(|file| !file.is_empty());
        let Some(target_file) = target_file else {
            continue;
        };
        let target = EditTarget {
            file: target_file.to_string(),
            cwd: record.cwd.filter(|cwd| !cwd.is_empty()).map(str::to_string),
        };
        targets.push(EditTarget {
            file: target.file.clone(),
            cwd: target.cwd.clone(),
        });
        let payloads: Vec<&RawValue> = match tool_name {
            "Edit" => [input.old_string, input.new_string]
                .into_iter()
                .flatten()
                .collect(),
            "Write" => input.content.into_iter().collect(),
            "MultiEdit" => input
                .edits
                .iter()
                .flat_map(|edit| [edit.old_string, edit.new_string])
                .flatten()
                .collect(),
            "NotebookEdit" => input.new_source.into_iter().collect(),
            _ => Vec::new(),
        };
        let payload_offset = payloads
            .into_iter()
            .filter_map(|payload| {
                let payload_start = borrowed_subslice_offset(line, payload.get())?;
                let symbol_start = whole_identifier_offset_in_json_string(payload.get(), symbol)?;
                line_offset
                    .checked_add(payload_start)?
                    .checked_add(symbol_start)
            })
            .min();
        let Some(byte_offset) = payload_offset else {
            continue;
        };
        candidates.push(AuthoredCandidate {
            tool_use_id: block.id.filter(|id| !id.is_empty()).map(str::to_string),
            tool_name: tool_name.to_string(),
            target,
            byte_offset,
        });
    }
}

fn is_edit_tool(name: &str) -> bool {
    matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit")
}

fn borrowed_subslice_offset(haystack: &str, needle: &str) -> Option<usize> {
    let offset = (needle.as_ptr() as usize).checked_sub(haystack.as_ptr() as usize)?;
    let end = offset.checked_add(needle.len())?;
    (haystack.get(offset..end) == Some(needle)).then_some(offset)
}

fn whole_identifier_offset_in_json_string(raw: &str, symbol: &str) -> Option<usize> {
    if symbol.is_empty() {
        return None;
    }
    let decoded: String = serde_json::from_str(raw).ok()?;
    let mut search_from = 0_usize;
    while search_from <= raw.len().saturating_sub(symbol.len()) {
        let relative = find_bytes(&raw.as_bytes()[search_from..], symbol.as_bytes())?;
        let raw_offset = search_from.checked_add(relative)?;
        let mut prefix = raw.get(..raw_offset)?.to_string();
        prefix.push('"');
        if let Ok(decoded_prefix) = serde_json::from_str::<String>(&prefix) {
            let decoded_offset = decoded_prefix.len();
            if decoded
                .get(decoded_offset..)
                .is_some_and(|suffix| suffix.starts_with(symbol))
                && is_whole_identifier_at(&decoded, decoded_offset, symbol)
            {
                return Some(raw_offset);
            }
        }
        search_from = raw_offset.checked_add(symbol.len())?;
    }
    None
}

fn is_whole_identifier_at(text: &str, offset: usize, symbol: &str) -> bool {
    let Some(end) = offset.checked_add(symbol.len()) else {
        return false;
    };
    if text.get(offset..end) != Some(symbol) {
        return false;
    }
    let before_is_identifier = text
        .get(..offset)
        .and_then(|prefix| prefix.chars().next_back())
        .is_some_and(is_identifier_char);
    let after_is_identifier = text
        .get(end..)
        .and_then(|suffix| suffix.chars().next())
        .is_some_and(is_identifier_char);
    !before_is_identifier && !after_is_identifier
}

fn is_identifier_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn target_matches_dream(
    target: &EditTarget,
    dream_file: &str,
    all_targets: &[&EditTarget],
) -> bool {
    let Some(target_basename) = canonical_basename(&target.file) else {
        return false;
    };
    let Some(dream_basename) = canonical_basename(dream_file) else {
        return false;
    };
    if target_basename != dream_basename {
        return false;
    }
    let Some(target_path) = resolved_path(&target.file, target.cwd.as_deref()) else {
        return false;
    };
    let Some(dream_path) = canonical_compare_path(Path::new(dream_file)) else {
        return false;
    };
    if target_path.is_absolute() && dream_path.is_absolute() {
        return target_path == dream_path;
    }
    if target_path == dream_path {
        return true;
    }
    if !is_bare_path(&target.file) && !is_bare_path(dream_file) {
        return false;
    }
    basename_is_unambiguous(dream_file, &dream_basename, all_targets)
}

fn canonical_basename(path: &str) -> Option<String> {
    let canonical = super::family::canon_file(path);
    Path::new(&canonical)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn resolved_path(path: &str, cwd: Option<&str>) -> Option<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute() {
        return canonical_compare_path(path);
    }
    if let Some(cwd) = cwd.map(Path::new).filter(|cwd| cwd.is_absolute()) {
        return canonical_compare_path(&cwd.join(path));
    }
    canonical_compare_path(path)
}

fn canonical_compare_path(path: &Path) -> Option<PathBuf> {
    let collapsed = super::family::canon_file(path.to_str()?);
    let normalized = lexically_normalize(Path::new(&collapsed))?;
    Some(fs::canonicalize(&normalized).unwrap_or(normalized))
}

fn lexically_normalize(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
        }
    }
    (!normalized.as_os_str().is_empty()).then_some(normalized)
}

fn is_bare_path(path: &str) -> bool {
    let mut components = Path::new(path).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn basename_is_unambiguous(dream_file: &str, basename: &str, all_targets: &[&EditTarget]) -> bool {
    let mut identities = BTreeSet::new();
    if !is_bare_path(dream_file) {
        let Some(dream_path) = canonical_compare_path(Path::new(dream_file)) else {
            return false;
        };
        identities.insert(dream_path);
    }
    for target in all_targets {
        if canonical_basename(&target.file).as_deref() != Some(basename) {
            continue;
        }
        let Some(path) = resolved_path(&target.file, target.cwd.as_deref()) else {
            return false;
        };
        if !is_bare_path(&target.file) || path.is_absolute() {
            identities.insert(path);
        }
    }
    identities.len() <= 1
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

pub fn build_dream_provenance(
    parent_sessions: &[String],
    project_dir: &str,
    symbol: &str,
    file: &str,
    projects_root: &Path,
) -> DreamProvenance {
    build_dream_citation_evidence(parent_sessions, project_dir, symbol, file, projects_root)
        .provenance
}

pub fn build_dream_citation_evidence(
    parent_sessions: &[String],
    project_dir: &str,
    symbol: &str,
    file: &str,
    projects_root: &Path,
) -> DreamCitationEvidence {
    let parent_sessions: Vec<String> = parent_sessions
        .iter()
        .filter(|session| !session.is_empty())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut transcripts = Vec::new();
    for parent_session in &parent_sessions {
        transcripts.extend(subagent_transcripts_for_session(
            projects_root,
            project_dir,
            parent_session,
        ));
    }
    let citations = cite_subagents_for_symbol(&transcripts, symbol, file);
    DreamCitationEvidence::audited(parent_sessions, citations)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn jsonl(lines: &[serde_json::Value]) -> Vec<u8> {
        let mut bytes = lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes();
        bytes.push(b'\n');
        bytes
    }

    fn fixture_transcript(
        root: &std::path::Path,
        project: &str,
        parent: &str,
        agent: &str,
        content: &[u8],
    ) -> std::path::PathBuf {
        let subagents = root.join(project).join(parent).join("subagents");
        fs::create_dir_all(&subagents).unwrap();
        let path = subagents.join(format!("agent-{agent}.jsonl"));
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn edit_of_dream_file_with_whole_identifier_is_authored() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let bytes = jsonl(&[
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "tool-edit",
                        "name": "Edit",
                        "input": {
                            "file_path": "/repo/HomeScreen.tsx",
                            "old_string": "before",
                            "new_string": "call ensureRadioAudioMode now"
                        }
                    }]
                }
            }),
            serde_json::json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "tool-edit",
                        "is_error": false,
                        "content": "updated"
                    }]
                }
            }),
        ]);
        let transcript = fixture_transcript(&root, "-repo", "parent-1", "author", &bytes);

        let citations = cite_subagents_for_symbol(
            std::slice::from_ref(&transcript),
            "ensureRadioAudioMode",
            "/repo/HomeScreen.tsx",
        );

        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0].session_id, "author");
        assert_eq!(citations[0].tool_name, "Edit");
        let stored = fs::read(&citations[0].transcript_path).unwrap();
        assert_eq!(
            &stored
                [citations[0].byte_offset..citations[0].byte_offset + "ensureRadioAudioMode".len()],
            b"ensureRadioAudioMode"
        );
    }

    #[test]
    fn text_and_read_references_are_not_authored_or_bar_eligible() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let bytes = jsonl(&[
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "text",
                        "text": "I found ensureRadioAudioMode"
                    }]
                }
            }),
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [
                        {
                            "type": "tool_use",
                            "id": "tool-read",
                            "name": "Read",
                            "input": {
                                "file_path": "/repo/HomeScreen.tsx",
                                "description": "ensureRadioAudioMode"
                            }
                        },
                        {
                            "type": "tool_use",
                            "id": "tool-grep",
                            "name": "Grep",
                            "input": {"pattern": "ensureRadioAudioMode"}
                        },
                        {
                            "type": "tool_use",
                            "id": "tool-bash",
                            "name": "Bash",
                            "input": {"command": "rg ensureRadioAudioMode"}
                        }
                    ]
                }
            }),
        ]);
        fixture_transcript(&root, "-repo", "parent-1", "reader", &bytes);

        let evidence = build_dream_citation_evidence(
            &["parent-1".to_string()],
            "-repo",
            "ensureRadioAudioMode",
            "/repo/HomeScreen.tsx",
            &root,
        );

        assert!(evidence.citations.is_empty());
        assert!(evidence.provenance.subagent_sessions.is_empty());
        assert!(!evidence.bar_clause_met);
    }

    #[test]
    fn identifier_substrings_in_edit_payload_are_not_authored() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let bytes = jsonl(&[serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "tool-edit",
                    "name": "Edit",
                    "input": {
                        "file_path": "/repo/HomeScreen.tsx",
                        "old_string": "ensureRadioAudioModeHelper",
                        "new_string": "xensureRadioAudioMode"
                    }
                }]
            }
        })]);
        let transcript = fixture_transcript(&root, "-repo", "parent-1", "substring", &bytes);

        let citations = cite_subagents_for_symbol(
            &[transcript],
            "ensureRadioAudioMode",
            "/repo/HomeScreen.tsx",
        );

        assert!(citations.is_empty());
    }

    #[test]
    fn edit_with_paired_error_result_is_not_authored() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let bytes = jsonl(&[
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "tool-edit",
                        "name": "Edit",
                        "input": {
                            "file_path": "/repo/HomeScreen.tsx",
                            "old_string": "before",
                            "new_string": "ensureRadioAudioMode"
                        }
                    }]
                }
            }),
            serde_json::json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "tool-edit",
                        "is_error": true,
                        "content": "edit failed"
                    }]
                }
            }),
        ]);
        let transcript = fixture_transcript(&root, "-repo", "parent-1", "failed", &bytes);

        let citations = cite_subagents_for_symbol(
            &[transcript],
            "ensureRadioAudioMode",
            "/repo/HomeScreen.tsx",
        );

        assert!(citations.is_empty());
    }

    #[test]
    fn all_supported_edit_tools_can_produce_authored_receipts() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let cases = [
            (
                "edit",
                "Edit",
                serde_json::json!({
                    "file_path": "/repo/file.rs",
                    "old_string": "before",
                    "new_string": "symbol"
                }),
            ),
            (
                "write",
                "Write",
                serde_json::json!({"file_path": "/repo/file.rs", "content": "symbol"}),
            ),
            (
                "multi",
                "MultiEdit",
                serde_json::json!({
                    "file_path": "/repo/file.rs",
                    "edits": [{"old_string": "before", "new_string": "symbol"}]
                }),
            ),
            (
                "notebook",
                "NotebookEdit",
                serde_json::json!({
                    "notebook_path": "/repo/file.rs",
                    "new_source": "symbol"
                }),
            ),
        ];
        let mut transcripts = Vec::new();
        for (agent, tool_name, input) in cases {
            let bytes = jsonl(&[serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": format!("tool-{agent}"),
                        "name": tool_name,
                        "input": input
                    }]
                }
            })]);
            transcripts.push(fixture_transcript(
                &root, "-repo", "parent-1", agent, &bytes,
            ));
        }

        let citations = cite_subagents_for_symbol(&transcripts, "symbol", "/repo/file.rs");
        let tools: BTreeSet<&str> = citations
            .iter()
            .map(|citation| citation.tool_name.as_str())
            .collect();

        assert_eq!(
            tools,
            BTreeSet::from(["Edit", "MultiEdit", "NotebookEdit", "Write"])
        );
    }

    #[test]
    fn edit_of_different_absolute_file_with_same_basename_is_not_authored() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let bytes = jsonl(&[serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "tool-edit",
                    "name": "Edit",
                    "input": {
                        "file_path": "/repo-a/file.rs",
                        "old_string": "before",
                        "new_string": "symbol"
                    }
                }]
            }
        })]);
        let transcript = fixture_transcript(&root, "-repo", "parent-1", "wrong-file", &bytes);

        let citations = cite_subagents_for_symbol(&[transcript], "symbol", "/repo-b/file.rs");

        assert!(citations.is_empty());
    }

    #[test]
    fn bare_basename_match_fails_closed_when_multiple_paths_are_possible() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let bare = jsonl(&[serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "tool-bare",
                    "name": "Edit",
                    "input": {
                        "file_path": "file.rs",
                        "old_string": "before",
                        "new_string": "symbol"
                    }
                }]
            }
        })]);
        let other = jsonl(&[serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "tool-other",
                    "name": "Edit",
                    "input": {
                        "file_path": "/other/file.rs",
                        "old_string": "before",
                        "new_string": "unrelated"
                    }
                }]
            }
        })]);
        let bare = fixture_transcript(&root, "-repo", "parent-1", "bare", &bare);
        let other = fixture_transcript(&root, "-repo", "parent-1", "other", &other);

        let citations = cite_subagents_for_symbol(&[bare, other], "symbol", "/repo/file.rs");

        assert!(citations.is_empty());
    }

    #[test]
    fn bare_basename_match_is_accepted_when_unambiguous() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let bytes = jsonl(&[serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "tool-bare",
                    "name": "Edit",
                    "input": {
                        "file_path": "file.rs",
                        "old_string": "before",
                        "new_string": "symbol"
                    }
                }]
            }
        })]);
        let transcript = fixture_transcript(&root, "-repo", "parent-1", "bare", &bytes);

        let citations = cite_subagents_for_symbol(&[transcript], "symbol", "/repo/file.rs");

        assert_eq!(citations.len(), 1);
    }

    #[test]
    fn malformed_jsonl_lines_are_skipped_without_corrupting_receipt_offset() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let mut bytes = b"{malformed ensureRadioAudioMode}\n".to_vec();
        bytes.extend(jsonl(&[serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "tool-edit",
                    "name": "Edit",
                    "input": {
                        "file_path": "/repo/file.rs",
                        "old_string": "before",
                        "new_string": "ensureRadioAudioMode"
                    }
                }]
            }
        })]));
        let expected_offset = bytes
            .windows(b"ensureRadioAudioMode".len())
            .rposition(|window| window == b"ensureRadioAudioMode")
            .unwrap();
        let transcript = fixture_transcript(&root, "-repo", "parent-1", "malformed", &bytes);

        let citations =
            cite_subagents_for_symbol(&[transcript], "ensureRadioAudioMode", "/repo/file.rs");

        assert_eq!(citations[0].byte_offset, expected_offset);
    }

    #[test]
    fn transcripts_are_filtered_and_sorted() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let later = fixture_transcript(&root, "-repo", "parent-1", "z", b"later");
        let earlier = fixture_transcript(&root, "-repo", "parent-1", "a", b"earlier");
        let ignored = later.parent().unwrap().join("notes.jsonl");
        fs::write(ignored, b"not an agent transcript").unwrap();

        let found = subagent_transcripts_for_session(&root, "-repo", "parent-1");

        assert_eq!(found, vec![earlier, later]);
    }

    #[test]
    fn a_payload_hit_beyond_the_reader_buffer_keeps_its_absolute_offset() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let payload = format!("{} HomeScreen", "x".repeat(8 * 1024));
        let bytes = jsonl(&[serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "tool-write",
                    "name": "Write",
                    "input": {"file_path": "/repo/file.rs", "content": payload}
                }]
            }
        })]);
        let expected_offset = find_bytes(&bytes, b"HomeScreen").unwrap();
        let transcript = fixture_transcript(&root, "-repo", "parent-1", "boundary", &bytes);

        let citations = cite_subagents_for_symbol(&[transcript], "HomeScreen", "/repo/file.rs");

        assert!(expected_offset > 8 * 1024);
        assert_eq!(citations[0].byte_offset, expected_offset);
    }

    #[test]
    fn multiple_payload_mentions_return_the_first_payload_offset() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let bytes = jsonl(&[serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "tool-write",
                    "name": "Write",
                    "input": {
                        "file_path": "/repo/file.rs",
                        "content": "xx HomeScreen yy HomeScreen"
                    }
                }]
            }
        })]);
        let expected_offset = find_bytes(&bytes, b"HomeScreen").unwrap();
        let transcript = fixture_transcript(&root, "-repo", "parent-1", "first", &bytes);

        let citations = cite_subagents_for_symbol(&[transcript], "HomeScreen", "/repo/file.rs");

        assert_eq!(citations[0].byte_offset, expected_offset);
    }

    #[test]
    fn unreadable_transcript_is_skipped_without_losing_other_citations() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let missing = root.join("-repo/parent-1/subagents/agent-missing.jsonl");
        let bytes = jsonl(&[serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "tool-write",
                    "name": "Write",
                    "input": {"file_path": "/repo/file.rs", "content": "HomeScreen"}
                }]
            }
        })]);
        let readable = fixture_transcript(&root, "-repo", "parent-1", "readable", &bytes);

        let citations =
            cite_subagents_for_symbol(&[missing, readable], "HomeScreen", "/repo/file.rs");

        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0].session_id, "readable");
    }

    #[test]
    fn missing_symbol_and_basename_return_no_citation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let bytes = jsonl(&[serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "tool-edit",
                    "name": "Edit",
                    "input": {
                        "file_path": "/repo/HomeScreen.tsx",
                        "old_string": "unrelated",
                        "new_string": "work"
                    }
                }]
            }
        })]);
        let transcript = fixture_transcript(&root, "-repo", "parent-1", "abc", &bytes);

        let citations = cite_subagents_for_symbol(
            &[transcript],
            "ensureRadioAudioMode",
            "/repo/HomeScreen.tsx",
        );

        assert!(citations.is_empty());
    }

    #[test]
    fn citation_order_is_deterministic_for_unsorted_inputs() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let bytes = jsonl(&[serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "tool-write",
                    "name": "Write",
                    "input": {"file_path": "/repo/file.rs", "content": "symbol"}
                }]
            }
        })]);
        let later = fixture_transcript(&root, "-repo", "parent-1", "z", &bytes);
        let earlier = fixture_transcript(&root, "-repo", "parent-1", "a", &bytes);

        let citations = cite_subagents_for_symbol(&[later, earlier], "symbol", "/repo/file.rs");

        let sessions: Vec<&str> = citations
            .iter()
            .map(|citation| citation.session_id.as_str())
            .collect();
        assert_eq!(sessions, vec!["a", "z"]);
    }

    #[test]
    fn basename_reference_without_symbol_payload_is_not_authored() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let bytes = jsonl(&[serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "prefix HomeScreen.tsx suffix"}]
            }
        })]);
        let transcript = fixture_transcript(&root, "-repo", "parent-1", "xyz", &bytes);

        let citations = cite_subagents_for_symbol(
            std::slice::from_ref(&transcript),
            "ensureRadioAudioMode",
            "/repo/HomeScreen.tsx",
        );

        assert!(citations.is_empty());
    }

    #[test]
    fn provenance_passes_the_bar_iff_a_subagent_is_cited() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let authored = jsonl(&[serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "tool-edit",
                    "name": "Edit",
                    "input": {
                        "file_path": "/repo/HomeScreen.tsx",
                        "old_string": "before",
                        "new_string": "HomeScreen"
                    }
                }]
            }
        })]);
        fixture_transcript(&root, "-repo", "parent-hit", "cited", &authored);
        fixture_transcript(
            &root,
            "-repo",
            "parent-miss",
            "uncited",
            b"worked on something else",
        );

        let passing = build_dream_provenance(
            &["parent-hit".to_string()],
            "-repo",
            "HomeScreen",
            "/repo/HomeScreen.tsx",
            &root,
        );
        let failing = build_dream_provenance(
            &["parent-miss".to_string()],
            "-repo",
            "HomeScreen",
            "/repo/HomeScreen.tsx",
            &root,
        );

        assert!(super::super::subagent_iface::audit_bar_clause(&Some(passing)).is_ok());
        assert!(super::super::subagent_iface::audit_bar_clause(&Some(failing)).is_err());
    }

    #[test]
    fn stored_evidence_keeps_the_concrete_receipt_and_audited_bar_result() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let bytes = jsonl(&[serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "tool-edit",
                    "name": "Edit",
                    "input": {
                        "file_path": "/repo/HomeScreen.tsx",
                        "old_string": "before",
                        "new_string": "prefix HomeScreen suffix"
                    }
                }]
            }
        })]);
        let expected_offset = bytes
            .windows(b"HomeScreen".len())
            .rposition(|window| window == b"HomeScreen")
            .unwrap();
        let transcript = fixture_transcript(&root, "-repo", "parent-hit", "cited", &bytes);

        let evidence = build_dream_citation_evidence(
            &["parent-hit".to_string()],
            "-repo",
            "HomeScreen",
            "/repo/HomeScreen.tsx",
            &root,
        );

        assert!(evidence.bar_clause_met);
        assert_eq!(evidence.provenance.subagent_sessions, vec!["cited"]);
        assert_eq!(evidence.citations.len(), 1);
        assert_eq!(evidence.citations[0].transcript_path, transcript);
        assert_eq!(evidence.citations[0].byte_offset, expected_offset);
        assert_eq!(evidence.citations[0].tool_name, "Edit");
        let stored = fs::read(&evidence.citations[0].transcript_path).unwrap();
        assert!(stored[evidence.citations[0].byte_offset..].starts_with(b"HomeScreen"));
    }
}

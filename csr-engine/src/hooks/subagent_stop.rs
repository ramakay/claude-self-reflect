use std::path::{Path, PathBuf};

use anyhow::Result;

use super::HookInput;
use crate::engine::Engine;
use crate::mcp::tools::ConversationLookup;
use crate::search::cross_project::resolve_project_from_cwd;

fn locate_transcript(input: &HookInput, projects_dir: &Path) -> Option<PathBuf> {
    if let Some(path) = input.transcript_path.as_deref().map(PathBuf::from) {
        if path.is_file() && path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            return Some(path);
        }
    }
    let session_id = input.session_id.as_deref()?;
    let agent_id = input.agent_id.as_deref();
    let mut project_dirs =
        match crate::mcp::tools::find_conversation_file(projects_dir, session_id, None) {
            ConversationLookup::Found(_, project_dir) => vec![project_dir],
            _ => std::fs::read_dir(projects_dir)
                .ok()?
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().is_dir())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect(),
        };
    project_dirs.sort();
    for project_dir in project_dirs {
        let transcripts =
            crate::dream::backfill::subagent_citation::subagent_transcripts_for_session(
                projects_dir,
                &project_dir,
                session_id,
            );
        if let Some(agent_id) = agent_id {
            let expected = format!("agent-{agent_id}.jsonl");
            if let Some(path) = transcripts.into_iter().find(|path| {
                path.file_name().and_then(|name| name.to_str()) == Some(expected.as_str())
            }) {
                return Some(path);
            }
        } else if transcripts.len() == 1 {
            return transcripts.into_iter().next();
        }
    }
    None
}

/// Capture deterministic intent events from the completed child transcript.
/// Events retain the parent `session_id` supplied by Claude Code.
pub async fn handle(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    if std::env::var("CSR_NO_INTENT_CAPTURE").as_deref() == Ok("1") {
        return Ok(());
    }
    let Some(session_id) = input.session_id.as_deref() else {
        return Ok(());
    };
    let Some(transcript_path) = locate_transcript(input, engine.projects_dir()) else {
        return Ok(());
    };
    let project =
        resolve_project_from_cwd(&cwd.to_string_lossy()).unwrap_or_else(|| "unknown".to_string());
    match crate::transcript::intent_events::extract_intent_events(
        &transcript_path,
        session_id,
        &project,
        engine.embeddings(),
    )
    .await
    {
        Ok(events) => {
            if let Err(error) = engine.storage().insert_intent_events(&events) {
                eprintln!("CSR: subagent intent persist error (non-fatal): {error}");
            }
        }
        Err(error) => eprintln!("CSR: subagent intent extraction error (non-fatal): {error}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locates_agent_transcript_from_parent_session_and_agent_id() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        let project = projects.join("-repo");
        std::fs::create_dir_all(project.join("parent-1/subagents")).unwrap();
        std::fs::write(project.join("parent-1.jsonl"), "{}\n").unwrap();
        let expected = project.join("parent-1/subagents/agent-child-7.jsonl");
        std::fs::write(&expected, "{}\n").unwrap();
        std::fs::write(project.join("parent-1/subagents/agent-other.jsonl"), "{}\n").unwrap();
        let input = HookInput {
            session_id: Some("parent-1".into()),
            agent_id: Some("child-7".into()),
            ..Default::default()
        };

        assert_eq!(locate_transcript(&input, &projects), Some(expected));
    }

    #[test]
    fn explicit_transcript_path_wins_when_present() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("agent-explicit.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        let input = HookInput {
            transcript_path: Some(transcript.to_string_lossy().into_owned()),
            session_id: Some("parent".into()),
            ..Default::default()
        };

        assert_eq!(
            locate_transcript(&input, &temp.path().join("missing")),
            Some(transcript)
        );
    }
}

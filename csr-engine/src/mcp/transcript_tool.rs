//! The `csr_transcript` tool body. Lives here so `tools.rs` stays under the file-size cap;
//! `tools.rs` re-exports both entry points.

use std::path::Path;

use anyhow::Result;

use crate::storage::Storage;

/// Server-side cap on `csr_transcript`'s response, applied regardless of the
/// caller's requested `budget_chars` — bounds a misbehaving/huge request
/// against the MCP response size, matching the design doc's "token budget
/// enforced server-side" (Option C rationale).
const MCP_TRANSCRIPT_MAX_BUDGET_CHARS: usize = crate::transcript::MAX_BUDGET_CHARS;

/// Thin wrapper over [`crate::transcript::run`] for the `csr_transcript` MCP
/// tool: validate the view/role/turns strings (never panics on a bad one —
/// same fail-honest posture as the CLI, but returns the message as `Ok`
/// content instead of exiting the process), clamp the budget, and dispatch
/// through the exact same core the CLI uses.
#[allow(clippy::too_many_arguments)]
pub fn transcript(
    projects_dir: &Path,
    data_dir: Option<&Path>,
    session: &str,
    view: &str,
    project: Option<&str>,
    role: Option<&str>,
    turns: Option<&str>,
    last: Option<usize>,
    grep: Option<&str>,
    tool: Option<&str>,
    json: bool,
    budget_chars: Option<usize>,
    sidechains: bool,
    include_system: bool,
    result_chars: Option<usize>,
) -> Result<String> {
    transcript_with_db(
        projects_dir,
        data_dir,
        None,
        session,
        view,
        project,
        role,
        turns,
        last,
        grep,
        tool,
        json,
        budget_chars,
        sidechains,
        include_system,
        result_chars,
        None,
    )
}

/// [`transcript`], answering from the indexed chunks in `db` when the JSONL and
/// the mirror copy are both gone.
#[allow(clippy::too_many_arguments)]
pub fn transcript_with_db(
    projects_dir: &Path,
    data_dir: Option<&Path>,
    db: Option<&Storage>,
    session: &str,
    view: &str,
    project: Option<&str>,
    role: Option<&str>,
    turns: Option<&str>,
    last: Option<usize>,
    grep: Option<&str>,
    tool: Option<&str>,
    json: bool,
    budget_chars: Option<usize>,
    sidechains: bool,
    include_system: bool,
    result_chars: Option<usize>,
    offset: Option<usize>,
) -> Result<String> {
    use crate::transcript::{parse_turns_spec, RoleFilter, TranscriptRequest, ViewKind};

    let Some(view_kind) = ViewKind::parse(view) else {
        return Ok(format!(
            "<transcript_error>\n<error>unknown view '{view}' — expected one of: receipt, stats, prompts, yourasks, tools, files, errors, slice, grep</error>\n</transcript_error>"
        ));
    };
    let role_filter = match role {
        Some(r) => match RoleFilter::parse(r) {
            Some(rf) => rf,
            None => {
                return Ok(format!(
                    "<transcript_error>\n<error>unknown role '{r}' — expected one of: user, assistant, system, all</error>\n</transcript_error>"
                ));
            }
        },
        None => RoleFilter::All,
    };
    let parsed_turns = match turns {
        Some(spec) => match parse_turns_spec(spec) {
            Ok(range) => Some(range),
            Err(e) => {
                return Ok(format!(
                    "<transcript_error>\n<error>{e}</error>\n</transcript_error>"
                ));
            }
        },
        None => None,
    };

    let req = TranscriptRequest {
        session: session.to_string(),
        view: view_kind,
        project: project.map(str::to_string),
        role: role_filter,
        turns: parsed_turns,
        last,
        grep: grep.map(str::to_string),
        tool: tool.map(str::to_string),
        json,
        budget_chars: budget_chars
            .unwrap_or(crate::transcript::DEFAULT_BUDGET_CHARS)
            .min(MCP_TRANSCRIPT_MAX_BUDGET_CHARS),
        sidechains,
        include_system,
        result_chars,
        offset: offset.unwrap_or(0),
    };

    Ok(crate::transcript::run_with_sources(
        projects_dir,
        data_dir,
        db,
        &req,
    ))
}

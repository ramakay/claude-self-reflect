//! Canonical provenance model — WHO said a thing, WHERE it came from, and WHAT
//! it overrides.
//!
//! The failure benchmark (2026-06-11) was that CSR stored zero provenance: no
//! speaker, no supersession, no source span. Semantic recall therefore could not
//! distinguish a user's founding decision from a `tool_result` build-log line,
//! and a 20-line `grep` beat the whole stack on CSR's own vision.
//!
//! This is the shared type used by storage, extraction, and the continuity eval.
//! Poisoning defense (design §Q6.2): only [`Speaker::User`] text may be treated
//! as a decision or correction — never `assistant` narration or `tool_result`
//! / file content masquerading as one.

use std::str::FromStr;

/// Who authored a chunk of conversation content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    /// A real user message — the only authoritative source for decisions.
    User,
    /// Claude's own narration.
    Assistant,
    /// Tool output (`tool_result`) or pasted file content — never authoritative.
    ToolResult,
}

impl Speaker {
    /// Stable lowercase token for DB storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Speaker::User => "user",
            Speaker::Assistant => "assistant",
            Speaker::ToolResult => "tool_result",
        }
    }

    /// True only for user-authored content — the poisoning-defense gate.
    pub fn is_authoritative(&self) -> bool {
        matches!(self, Speaker::User)
    }
}

impl FromStr for Speaker {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Speaker::User),
            "assistant" => Ok(Speaker::Assistant),
            "tool_result" => Ok(Speaker::ToolResult),
            _ => Err(()),
        }
    }
}

/// Provenance attached to an indexed chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkProvenance {
    pub author: Speaker,
    /// The conversation id this content was sourced from.
    pub source_conv_id: String,
    /// The prior claim this content overrides, if any (e.g. "behavioral
    /// continuity"). Drives supersession-aware recall.
    pub supersedes: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaker_roundtrips_through_string() {
        for sp in [Speaker::User, Speaker::Assistant, Speaker::ToolResult] {
            assert_eq!(Speaker::from_str(sp.as_str()), Ok(sp));
        }
    }

    #[test]
    fn only_user_is_authoritative() {
        assert!(Speaker::User.is_authoritative());
        assert!(!Speaker::Assistant.is_authoritative());
        assert!(!Speaker::ToolResult.is_authoritative());
    }

    #[test]
    fn unknown_speaker_token_is_error() {
        assert_eq!(Speaker::from_str("system"), Err(()));
    }
}

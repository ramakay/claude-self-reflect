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

use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

/// Exact canonical ledger payload; field order is part of the digest contract.
#[derive(Debug, Clone)]
pub struct ResolutionConfirmationPayload {
    canonical_json: String,
    digest: String,
}

impl ResolutionConfirmationPayload {
    pub fn new(chunk_ids: &[String], status: &str, claim: Option<&str>, evidence: &str) -> Self {
        use sha2::{Digest, Sha256};
        #[derive(serde::Serialize)]
        struct Payload<'a> {
            chunk_ids: &'a [String],
            status: &'a str,
            claim: Option<&'a str>,
            evidence: &'a str,
        }
        let canonical_json = serde_json::to_string(&Payload {
            chunk_ids,
            status,
            claim,
            evidence,
        })
        .expect("resolution payload serialization is infallible");
        let digest = Sha256::digest(canonical_json.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Self {
            canonical_json,
            digest,
        }
    }
    pub fn canonical_json(&self) -> &str {
        &self.canonical_json
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// Created only at platform confirmation boundaries, never deserialized from
/// tool arguments, memory tags, or a source string. A local authority event for
/// this ledger payload only; no principal/target/risk/scope binding is implied.
#[derive(Debug)]
pub struct ResolutionConfirmation {
    payload: ResolutionConfirmationPayload,
    receipt_kind: &'static str,
}

impl ResolutionConfirmation {
    pub(crate) fn elicited(payload: ResolutionConfirmationPayload) -> Self {
        Self {
            payload,
            receipt_kind: "elicitation_digest",
        }
    }
    pub(crate) fn journal(payload: ResolutionConfirmationPayload) -> Self {
        Self {
            payload,
            receipt_kind: "journal_ui",
        }
    }
    pub fn matches(&self, payload: &ResolutionConfirmationPayload) -> bool {
        self.payload.canonical_json == payload.canonical_json
            && self.payload.digest == payload.digest
    }
    pub fn payload(&self) -> &ResolutionConfirmationPayload {
        &self.payload
    }
    pub fn receipt_kind(&self) -> &'static str {
        self.receipt_kind
    }
}

/// Cached authority floor for persisted memory.
///
/// The integer representation is part of the SQLite schema contract. New
/// variants must not renumber existing values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i64)]
pub enum TrustTier {
    Unknown = 0,
    External = 1,
    TrustedTool = 2,
    UserHistory = 3,
    UserConfirmed = 4,
    System = 5,
}

impl TrustTier {
    pub const fn as_i64(self) -> i64 {
        self as i64
    }

    /// Decode a nullable SQLite value conservatively.
    pub const fn from_db(value: Option<i64>) -> Self {
        match value {
            Some(1) => Self::External,
            Some(2) => Self::TrustedTool,
            Some(3) => Self::UserHistory,
            Some(4) => Self::UserConfirmed,
            Some(5) => Self::System,
            _ => Self::Unknown,
        }
    }

    pub const fn min(self, other: Self) -> Self {
        if self.as_i64() <= other.as_i64() {
            self
        } else {
            other
        }
    }
}

impl fmt::Display for TrustTier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unknown => "unknown",
            Self::External => "external",
            Self::TrustedTool => "trusted_tool",
            Self::UserHistory => "user_history",
            Self::UserConfirmed => "user_confirmed",
            Self::System => "system",
        })
    }
}

impl FromStr for TrustTier {
    type Err = Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "unknown" => Self::Unknown,
            "external" => Self::External,
            "trusted_tool" => Self::TrustedTool,
            "user_history" => Self::UserHistory,
            "user_confirmed" => Self::UserConfirmed,
            "system" => Self::System,
            _ => Self::Unknown,
        })
    }
}

/// Tools whose results are observations of local state only.
pub const LOCAL_ONLY_TOOLS: &[&str] = &[
    "Read",
    "Grep",
    "Glob",
    "Edit",
    "MultiEdit",
    "Write",
    "NotebookEdit",
    "LS",
    "TodoWrite",
    "TaskCreate",
    "TaskUpdate",
    "TaskList",
];

/// Fixed platform-observed channel rules. Tool and assistant channels are
/// handled beside this table because their tier depends on a tool name or the
/// running context floor.
pub const CHANNEL_TIER_RULES: &[(&str, TrustTier)] = &[
    ("user_message", TrustTier::UserHistory),
    ("codex_user", TrustTier::UserHistory),
    ("plan_file", TrustTier::TrustedTool),
    ("reflection", TrustTier::Unknown),
    ("memory_metadata", TrustTier::Unknown),
];

/// Map a platform-observed channel to a tier without consulting its text.
pub fn trust_for_channel(channel: &str, context_floor: TrustTier) -> TrustTier {
    if matches!(channel, "assistant_message" | "codex_assistant") {
        return context_floor;
    }
    if let Some(tool) = channel
        .strip_prefix("tool_result:")
        .or_else(|| channel.strip_prefix("codex_tool:"))
    {
        return if LOCAL_ONLY_TOOLS.contains(&tool) {
            TrustTier::TrustedTool
        } else {
            TrustTier::External
        };
    }
    if matches!(channel, "tool_result" | "codex_tool") {
        return TrustTier::External;
    }
    CHANNEL_TIER_RULES
        .iter()
        .find_map(|(known, tier)| (*known == channel).then_some(*tier))
        .unwrap_or(TrustTier::Unknown)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceEvent {
    pub event_id: String,
    pub conversation_id: String,
    pub message_key: String,
    pub seq: usize,
    pub channel: String,
    pub trust_tier: TrustTier,
    pub parent_event_id: Option<String>,
    pub receipt_kind: String,
    pub receipt_ref: Option<String>,
    pub observed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkSpan {
    pub chunk_id: String,
    pub event_id: String,
    pub start_char: usize,
    pub end_char: usize,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChunkEvidence {
    pub chunk_id: String,
    pub events: Vec<ProvenanceEvent>,
    pub spans: Vec<ChunkSpan>,
    pub min_trust: TrustTier,
    pub tool_result_share: Option<f64>,
}

pub fn content_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

/// Revalidate a span against the extracted event text. This is a cold-path
/// integrity check; retrieval reads the cached chunk floor instead.
pub fn validated_span_tier(
    event: &ProvenanceEvent,
    span: &ChunkSpan,
    extracted_message_text: &str,
) -> TrustTier {
    if span.event_id != event.event_id || span.end_char < span.start_char {
        return TrustTier::Unknown;
    }
    let text = extracted_message_text
        .chars()
        .skip(span.start_char)
        .take(span.end_char - span.start_char)
        .collect::<String>();
    if text.chars().count() != span.end_char - span.start_char
        || content_hash(&text) != span.content_hash
    {
        TrustTier::Unknown
    } else {
        event.trust_tier
    }
}

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
    fn trust_tier_has_stable_ordered_sqlite_encoding() {
        let cases = [
            (TrustTier::Unknown, 0_i64, "unknown"),
            (TrustTier::External, 1, "external"),
            (TrustTier::TrustedTool, 2, "trusted_tool"),
            (TrustTier::UserHistory, 3, "user_history"),
            (TrustTier::UserConfirmed, 4, "user_confirmed"),
            (TrustTier::System, 5, "system"),
        ];

        for (tier, encoded, rendered) in cases {
            assert_eq!(tier.as_i64(), encoded);
            assert_eq!(TrustTier::from_db(Some(encoded)), tier);
            assert_eq!(tier.to_string(), rendered);
            assert_eq!(rendered.parse::<TrustTier>().unwrap(), tier);
        }
        assert!(TrustTier::Unknown < TrustTier::External);
        assert!(TrustTier::External < TrustTier::TrustedTool);
        assert!(TrustTier::TrustedTool < TrustTier::UserHistory);
        assert!(TrustTier::UserHistory < TrustTier::UserConfirmed);
        assert!(TrustTier::UserConfirmed < TrustTier::System);
        assert_eq!(
            TrustTier::System.min(TrustTier::External),
            TrustTier::External
        );
    }

    #[test]
    fn missing_or_unparseable_trust_is_unknown() {
        assert_eq!(TrustTier::from_db(None), TrustTier::Unknown);
        assert_eq!(TrustTier::from_db(Some(-1)), TrustTier::Unknown);
        assert_eq!(TrustTier::from_db(Some(99)), TrustTier::Unknown);
        assert_eq!("user".parse::<TrustTier>().unwrap(), TrustTier::Unknown);
        assert_eq!("999".parse::<TrustTier>().unwrap(), TrustTier::Unknown);
    }

    #[test]
    fn fixed_channel_rules_map_each_declared_row() {
        for (channel, expected) in CHANNEL_TIER_RULES {
            assert_eq!(
                trust_for_channel(channel, TrustTier::External),
                *expected,
                "channel {channel}"
            );
        }
    }

    #[test]
    fn local_tool_allowlist_is_exhaustively_mapped() {
        for tool in LOCAL_ONLY_TOOLS {
            assert_eq!(
                trust_for_channel(&format!("tool_result:{tool}"), TrustTier::UserHistory),
                TrustTier::TrustedTool,
                "tool {tool}"
            );
            assert_eq!(
                trust_for_channel(&format!("codex_tool:{tool}"), TrustTier::UserHistory),
                TrustTier::TrustedTool,
                "codex tool {tool}"
            );
        }
    }

    #[test]
    fn external_and_unknown_channels_never_inherit_textual_authority() {
        for channel in [
            "tool_result:Bash",
            "tool_result:WebFetch",
            "tool_result:mcp__memory__store",
            "tool_result:MadeUp",
            "codex_tool:Bash",
        ] {
            assert_eq!(
                trust_for_channel(channel, TrustTier::System),
                TrustTier::External,
                "channel {channel}"
            );
        }
        assert_eq!(
            trust_for_channel("unclassified", TrustTier::System),
            TrustTier::Unknown
        );
    }

    #[test]
    fn assistant_channel_uses_context_floor_and_missing_context_is_unknown() {
        assert_eq!(
            trust_for_channel("assistant_message", TrustTier::External),
            TrustTier::External
        );
        assert_eq!(
            trust_for_channel("codex_assistant", TrustTier::Unknown),
            TrustTier::Unknown
        );
    }

    #[test]
    fn changed_span_content_decodes_to_unknown() {
        let event = ProvenanceEvent {
            event_id: "event-1".into(),
            conversation_id: "conv".into(),
            message_key: "message".into(),
            seq: 0,
            channel: "user_message".into(),
            trust_tier: TrustTier::UserHistory,
            parent_event_id: None,
            receipt_kind: "jsonl".into(),
            receipt_ref: Some("/tmp/transcript.jsonl#byte=0".into()),
            observed_at: "2026-09-04T00:00:00Z".into(),
        };
        let span = ChunkSpan {
            chunk_id: "chunk-1".into(),
            event_id: event.event_id.clone(),
            start_char: 0,
            end_char: 5,
            content_hash: content_hash("hello"),
        };

        assert_eq!(
            validated_span_tier(&event, &span, "hello world"),
            TrustTier::UserHistory
        );
        assert_eq!(
            validated_span_tier(&event, &span, "hullo world"),
            TrustTier::Unknown
        );
        assert_eq!(
            validated_span_tier(&event, &span, "short"),
            TrustTier::Unknown
        );
    }

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

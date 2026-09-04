//! Cached artifact floors and the shared, role-aware producer input envelope.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;

use super::{queries, Storage};
use crate::provenance::{content_hash, ProvenanceEvent, TrustTier};

/// Closed table/key mapping. These are internal producer identities, never tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Reflection,
    WitnessVerdict,
    DreamThread,
    DreamPlan,
    Dream,
    Intent,
    Resolution,
    ResolutionProposal,
    EpisodeIndex,
    DreamRelation,
    Ledger,
    JournalHeadline,
    SessionInstrumentation,
    Ratification,
}

pub const ARTIFACT_KINDS: &[ArtifactKind] = &[
    ArtifactKind::Reflection,
    ArtifactKind::WitnessVerdict,
    ArtifactKind::DreamThread,
    ArtifactKind::DreamPlan,
    ArtifactKind::Dream,
    ArtifactKind::Intent,
    ArtifactKind::Resolution,
    ArtifactKind::ResolutionProposal,
    ArtifactKind::EpisodeIndex,
    ArtifactKind::DreamRelation,
    ArtifactKind::Ledger,
    ArtifactKind::JournalHeadline,
    ArtifactKind::SessionInstrumentation,
    ArtifactKind::Ratification,
];

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reflection => "reflection",
            Self::WitnessVerdict => "witness_verdict",
            Self::DreamThread => "dream_thread",
            Self::DreamPlan => "dream_plan",
            Self::Dream => "dream",
            Self::Intent => "intent",
            Self::Resolution => "resolution",
            Self::ResolutionProposal => "resolution_proposal",
            Self::EpisodeIndex => "episode_index",
            Self::DreamRelation => "dream_relation",
            Self::Ledger => "ledger",
            Self::JournalHeadline => "journal_headline",
            Self::SessionInstrumentation => "session_instrumentation",
            Self::Ratification => "ratification",
        }
    }

    pub fn table(self) -> &'static str {
        match self {
            Self::Reflection => "reflections",
            Self::WitnessVerdict => "witness_verdicts",
            Self::DreamThread => "dream_threads",
            Self::DreamPlan => "dream_plans",
            Self::Dream => "dreams_v1",
            Self::Intent => "intent_events",
            Self::Resolution => "resolution_ledger",
            Self::ResolutionProposal => "resolution_proposals",
            Self::EpisodeIndex => "episode_index",
            Self::DreamRelation => "dream_relations",
            Self::Ledger => "derivation_ledger",
            Self::JournalHeadline => "journal_headlines",
            Self::SessionInstrumentation => "session_instrumentation",
            Self::Ratification => "ratification_scores",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::EpisodeIndex => "episode_id",
            Self::JournalHeadline | Self::SessionInstrumentation => "session_id",
            Self::Ratification => "conversation_id",
            Self::Ledger => "json_array(id,repo,branch,user)",
            _ => "id",
        }
    }

    /// Include all semantic fields, excluding mutable cache/timestamp metadata.
    /// A source version change invalidates its snapshot rather than certifying
    /// the replacement under an old floor.
    fn body(self) -> &'static str {
        match self {
            Self::Reflection | Self::Ledger => "content",
            Self::WitnessVerdict => "json_array(witness_id,verdict,successor_witness_id,receipt_oid,observed_head_oid)",
            Self::DreamThread => "json_array(thread,evidence_quote,files_json,receipt_tier,receipts_json)",
            Self::DreamPlan => "json_array(context,steps_json,files_json,acceptance)",
            Self::Dream => "prose",
            Self::Intent => "quote",
            Self::Resolution | Self::ResolutionProposal => "json_array(claim,evidence)",
            Self::EpisodeIndex => "json_array(request,completed,outcome,next_steps,blockers,files_json,anchors_json)",
            Self::DreamRelation => "json_array(ep_a,ep_b,relation,topic_key,quote_a,quote_b,load_bearing_oid,aux_oid,now_hook)",
            Self::JournalHeadline => "json_array(headline,description)",
            Self::SessionInstrumentation => "json_array(errors_json,steers_json,error_count,steer_count,turn_count)",
            Self::Ratification => "json_array(acts_json,ledger_refs,score)",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        ARTIFACT_KINDS
            .iter()
            .find(|kind| kind.as_str() == value)
            .copied()
    }
}

#[derive(Debug, Clone)]
enum InputSource {
    Event(ProvenanceEvent),
    Chunk {
        id: String,
        hash: String,
    },
    Artifact {
        kind: ArtifactKind,
        id: String,
        hash: String,
    },
    Unknown,
}

/// Not deserializable: model output and caller-controlled tags cannot construct
/// observations. Producers obtain these from structural reads or cached rows.
#[derive(Debug, Clone)]
pub struct ArtifactInput {
    channel: String,
    tier: TrustTier,
    text: String,
    source: InputSource,
    start: usize,
    end: usize,
}

impl ArtifactInput {
    pub fn unknown(text: &str) -> Self {
        Self {
            channel: "unobserved_input".into(),
            tier: TrustTier::Unknown,
            text: text.into(),
            source: InputSource::Unknown,
            start: 0,
            end: text.chars().count(),
        }
    }

    /// Structural producer seam. Validate message coordinates and hash before
    /// keeping the observed tier; this never interprets the span's prose.
    pub fn observed(
        event: ProvenanceEvent,
        message_text: &str,
        start: usize,
        end: usize,
        hash: String,
    ) -> Self {
        let text: String = message_text
            .chars()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect();
        if end < start || end > message_text.chars().count() || content_hash(&text) != hash {
            return Self::unknown(&text);
        }
        Self {
            channel: event.channel.clone(),
            tier: event.trust_tier,
            text,
            source: InputSource::Event(event),
            start,
            end,
        }
    }

    pub fn trust(&self) -> TrustTier {
        self.tier
    }
    pub fn channel(&self) -> &str {
        &self.channel
    }
    pub fn text(&self) -> &str {
        &self.text
    }

    /// A quoted substring of an observed message, disambiguated by byte receipt.
    pub(crate) fn quoted_span(&self, quote: &str, receipt: Option<&str>) -> Option<Self> {
        let InputSource::Event(event) = &self.source else {
            return None;
        };
        if event.receipt_kind != "jsonl"
            || receipt.is_some_and(|r| event.receipt_ref.as_deref() != Some(r))
            || quote.is_empty()
        {
            return None;
        }
        let start = self.text.find(quote)?;
        if self.text[start + quote.len()..].contains(quote) {
            return None;
        }
        let mut input = self.clone();
        input.start += self.text[..start].chars().count();
        input.end = input.start + quote.chars().count();
        input.text = quote.into();
        Some(input)
    }

    pub(crate) fn validated_stored_support(&self, conn: &Connection) -> Result<InputEnvelope> {
        let unknown = || InputEnvelope::new(vec![Self::unknown(&self.text)]);
        let InputSource::Event(event) = &self.source else {
            return Ok(unknown());
        };
        let chunks:Vec<String>=conn.prepare("SELECT DISTINCT s.chunk_id FROM provenance_events e JOIN chunk_spans s USING(event_id) WHERE e.conversation_id=?1 AND e.message_key=?2 AND e.channel=?3 AND e.receipt_kind='jsonl' AND s.start_char<=?4 AND s.end_char>=?5")?.query_map(params![event.conversation_id,event.message_key,event.channel,self.start as i64,self.end as i64],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let mut validated = InputEnvelope::default();
        for id in chunks {
            for input in chunk_inputs(conn, &id)?.inputs() {
                let InputSource::Event(native) = &input.source else {
                    continue;
                };
                if native.message_key != event.message_key
                    || native.channel != event.channel
                    || native.receipt_ref != event.receipt_ref
                {
                    continue;
                }
                if let Some(mut quote) = input
                    .quoted_span(&self.text, None)
                    .filter(|q| q.start == self.start && q.end == self.end)
                {
                    quote.tier = quote.tier.min(self.tier);
                    if let InputSource::Event(event) = &mut quote.source {
                        event.trust_tier = event.trust_tier.min(self.tier);
                    }
                    validated.push(quote);
                }
            }
        }
        Ok(if validated.inputs().is_empty() {
            unknown()
        } else {
            validated
        })
    }

    /// Prompt truncation retains the source identity and conservative floor.
    pub fn truncate(mut self, chars: usize) -> Self {
        self.text = self.text.chars().take(chars).collect();
        self.end = self.start + self.text.chars().count();
        self
    }
}

/// The same object renders prompt data and records its complete support set.
/// Empty or incompletely observed context is Unknown, never an empty-set maximum.
#[derive(Debug, Clone, Default)]
pub struct InputEnvelope {
    inputs: Vec<ArtifactInput>,
}

impl InputEnvelope {
    pub fn new(inputs: Vec<ArtifactInput>) -> Self {
        Self { inputs }
    }
    pub fn inputs(&self) -> &[ArtifactInput] {
        &self.inputs
    }
    pub fn push(&mut self, input: ArtifactInput) {
        self.inputs.push(input);
    }
    pub fn extend(&mut self, other: Self) {
        self.inputs.extend(other.inputs);
    }
    pub fn floor(&self) -> TrustTier {
        self.inputs
            .iter()
            .map(|i| i.tier)
            .reduce(TrustTier::min)
            .unwrap_or(TrustTier::Unknown)
    }
    pub fn render(&self) -> String {
        self.inputs
            .iter()
            .map(|input| {
                json!({"channel":input.channel,"trust":input.tier.to_string(),"text":input.text})
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn cached_floor(conn: &Connection, kind: ArtifactKind, id: &str) -> Result<TrustTier> {
    // Hot path: indexed row lookup only, never an event/derivation join.
    let encoded = conn
        .query_row(
            &format!(
                "SELECT min_trust FROM {} WHERE {}=?1",
                kind.table(),
                kind.key()
            ),
            [id],
            |r| Ok(r.get::<_, i64>(0).ok()),
        )
        .optional()?
        .flatten();
    Ok(TrustTier::from_db(encoded))
}

fn artifact_row(
    conn: &Connection,
    kind: ArtifactKind,
    id: &str,
) -> Result<Option<(String, TrustTier)>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {},min_trust FROM {} WHERE {}=?1",
                kind.body(),
                kind.table(),
                kind.key()
            ),
            [id],
            |r| Ok((r.get(0)?, TrustTier::from_db(r.get::<_, i64>(1).ok()))),
        )
        .optional()?)
}

/// Caller owns the transaction containing the output body write.
pub fn record_stored_inputs(
    conn: &Connection,
    kind: ArtifactKind,
    id: &str,
    inputs: &InputEnvelope,
) -> Result<TrustTier> {
    let (body, _) = artifact_row(conn, kind, id)?
        .ok_or_else(|| anyhow::anyhow!("missing {} output {id}", kind.as_str()))?;
    record_inputs(conn, kind, id, &body, inputs)
}

/// Compose with an existing producer transaction without publishing partial
/// body/support/cache state. The savepoint is also safe for standalone writers.
pub fn atomic_write<T>(
    conn: &Connection,
    write: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    conn.execute_batch("SAVEPOINT artifact_publication")?;
    match write(conn) {
        Ok(value) => {
            conn.execute_batch("RELEASE artifact_publication")?;
            Ok(value)
        }
        Err(error) => {
            conn.execute_batch("ROLLBACK TO artifact_publication; RELEASE artifact_publication")?;
            Err(error)
        }
    }
}

pub fn artifact_input(conn: &Connection, kind: ArtifactKind, id: &str) -> Result<ArtifactInput> {
    let Some((text, tier)) = artifact_row(conn, kind, id)? else {
        return Ok(ArtifactInput::unknown("missing artifact input"));
    };
    let hash = content_hash(&text);
    Ok(ArtifactInput {
        channel: format!("derived_artifact:{}", kind.as_str()),
        tier,
        start: 0,
        end: text.chars().count(),
        text,
        source: InputSource::Artifact {
            kind,
            id: id.into(),
            hash,
        },
    })
}

pub fn chunk_input(conn: &Connection, id: &str) -> Result<ArtifactInput> {
    let row = conn
        .query_row(
            "SELECT content,min_trust FROM chunks WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    TrustTier::from_db(r.get::<_, i64>(1).ok()),
                ))
            },
        )
        .optional()?;
    let Some((text, tier)) = row else {
        return Ok(ArtifactInput::unknown("missing chunk input"));
    };
    let hash = content_hash(&text);
    Ok(ArtifactInput {
        channel: "mixed_chunk".into(),
        tier,
        start: 0,
        end: text.chars().count(),
        text,
        source: InputSource::Chunk {
            id: id.into(),
            hash,
        },
    })
}

/// Cold producer read. Recover native channels using the recorded piece hashes,
/// even after transcript cleanup; no speaker aggregate or prose classifier.
pub fn chunk_inputs(conn: &Connection, id: &str) -> Result<InputEnvelope> {
    let whole = chunk_input(conn, id)?;
    let chars: Vec<char> = whole.text.chars().collect();
    let mut stmt=conn.prepare("SELECT e.event_id,e.conversation_id,e.message_key,e.seq,e.channel,e.trust_tier,e.parent_event_id,e.receipt_kind,e.receipt_ref,e.observed_at,s.start_char,s.end_char,s.content_hash FROM chunk_spans s JOIN provenance_events e USING(event_id) WHERE s.chunk_id=?1 ORDER BY e.seq,s.start_char,e.event_id")?;
    let pieces = stmt
        .query_map([id], |r| {
            Ok((
                ProvenanceEvent {
                    event_id: r.get(0)?,
                    conversation_id: r.get(1)?,
                    message_key: r.get(2)?,
                    seq: r.get::<_, i64>(3)? as usize,
                    channel: r.get(4)?,
                    trust_tier: TrustTier::from_db(r.get::<_, i64>(5).ok()),
                    parent_event_id: r.get(6)?,
                    receipt_kind: r.get(7)?,
                    receipt_ref: r.get(8)?,
                    observed_at: r.get(9)?,
                },
                r.get::<_, i64>(10)? as usize,
                r.get::<_, i64>(11)? as usize,
                r.get::<_, String>(12)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let fallback = || {
        let mut input = whole.clone();
        input.tier = TrustTier::Unknown;
        input.channel = "unreconstructed_chunk".into();
        InputEnvelope::new(vec![input])
    };
    if pieces.is_empty() {
        return Ok(fallback());
    }
    let mut used = vec![false; pieces.len()];
    let mut cursor = 0;
    let mut inputs = InputEnvelope::default();
    for _ in 0..pieces.len() {
        let mut found = None;
        for skip in 0..=2 {
            if cursor + skip > chars.len()
                || chars[cursor..cursor + skip].iter().any(|c| *c != '\n')
            {
                continue;
            }
            for (i, (event, start, end, hash)) in pieces.iter().enumerate() {
                let len = end.saturating_sub(*start);
                if used[i] || len == 0 || cursor + skip + len > chars.len() {
                    continue;
                }
                let text: String = chars[cursor + skip..cursor + skip + len].iter().collect();
                if content_hash(&text) == *hash
                    && found
                        .as_ref()
                        .is_none_or(|(_, _, _, tier)| event.trust_tier < *tier)
                {
                    found = Some((i, skip, text, event.trust_tier));
                }
            }
        }
        let Some((i, skip, text, tier)) = found else {
            return Ok(fallback());
        };
        let (event, start, end, _) = &pieces[i];
        used[i] = true;
        cursor += skip + text.chars().count();
        inputs.push(ArtifactInput {
            channel: event.channel.clone(),
            tier,
            text,
            source: InputSource::Event(event.clone()),
            start: *start,
            end: *end,
        });
    }
    if cursor != chars.len() {
        return Ok(fallback());
    }
    // Always retain the version-bound parent, with no duplicate body. Native
    // message events alone would miss later chunk replacement/cache lowering.
    inputs.push(whole.truncate(0));
    Ok(inputs)
}

/// Newest `session_episode` reflection for `session_id` as a row-level input.
/// Dream threads and plans are model output over exactly that episode, so it
/// is their support set; no episode on record is an Unknown input.
pub fn episode_reflection_input(conn: &Connection, session_id: &str) -> Result<ArtifactInput> {
    let id: Option<String> = conn
        .query_row(
            "SELECT id FROM reflections
              WHERE tags LIKE '%\"session_episode\"%'
                AND tags LIKE '%\"conv_' || ?1 || '\"%'
              ORDER BY timestamp DESC, id DESC LIMIT 1",
            [session_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match id {
        Some(id) => artifact_input(conn, ArtifactKind::Reflection, &id)?,
        None => ArtifactInput::unknown(&format!("no episode on record for session {session_id}")),
    })
}

/// Recheck source version and floor inside the publishing transaction. Captured
/// inputs can only decrease; a concurrent parent replacement cannot raise them.
fn input_event(
    conn: &Connection,
    input: &ArtifactInput,
) -> Result<(ProvenanceEvent, Option<String>)> {
    let mut tier = input.tier;
    let (receipt, chunk_id) = match &input.source {
        InputSource::Event(event) => {
            let mut event = event.clone();
            let current = conn
                .query_row(
                    "SELECT trust_tier FROM provenance_events WHERE event_id=?1",
                    [&event.event_id],
                    |r| Ok(r.get::<_, i64>(0).ok()),
                )
                .optional()?;
            if let Some(current) = current {
                event.trust_tier = event.trust_tier.min(TrustTier::from_db(current));
            }
            if event.receipt_kind == "artifact_input" {
                event.trust_tier = event.trust_tier.min(snapshot_floor(
                    conn,
                    event.receipt_ref.as_deref().unwrap_or(""),
                )?);
            }
            return Ok((event, None));
        }
        InputSource::Chunk { id, hash } => {
            let current = chunk_input(conn, id)?;
            tier = tier.min(if content_hash(&current.text) == *hash {
                current.tier
            } else {
                TrustTier::Unknown
            });
            (
                json!({"kind":"chunk","id":id,"content_hash":hash}).to_string(),
                Some(id.clone()),
            )
        }
        InputSource::Artifact { kind, id, hash } => {
            tier = tier.min(match artifact_row(conn, *kind, id)? {
                Some((body, tier)) if content_hash(&body) == *hash => tier,
                _ => TrustTier::Unknown,
            });
            (
                json!({"kind":kind.as_str(),"id":id,"content_hash":hash}).to_string(),
                None,
            )
        }
        InputSource::Unknown => {
            tier = TrustTier::Unknown;
            (
                json!({"kind":"unknown","content_hash":content_hash(&input.text)}).to_string(),
                None,
            )
        }
    };
    let key = content_hash(&format!("{receipt}\0{}", input.tier.as_i64()));
    Ok((
        ProvenanceEvent {
            event_id: format!("artifact-input:{key}"),
            conversation_id: "artifact-input".into(),
            message_key: key,
            seq: 0,
            channel: input.channel.clone(),
            trust_tier: tier,
            parent_event_id: None,
            receipt_kind: "artifact_input".into(),
            receipt_ref: Some(receipt),
            observed_at: chrono::Utc::now().to_rfc3339(),
        },
        chunk_id,
    ))
}

fn insert_event(conn: &Connection, event: &ProvenanceEvent) -> Result<(TrustTier, bool)> {
    // No-op duplicates do not invoke the immutable confirmation UPDATE trigger.
    conn.execute("INSERT OR IGNORE INTO provenance_events(event_id,conversation_id,message_key,seq,channel,trust_tier,parent_event_id,receipt_kind,receipt_ref,observed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![event.event_id,event.conversation_id,event.message_key,event.seq as i64,event.channel,event.trust_tier.as_i64(),event.parent_event_id,event.receipt_kind,event.receipt_ref,event.observed_at])?;
    let lowered = if event.channel != "user_confirmation" {
        conn.execute(
            "UPDATE provenance_events SET trust_tier=?2 WHERE event_id=?1 AND trust_tier>?2",
            params![event.event_id, event.trust_tier.as_i64()],
        )? > 0
    } else {
        false
    };
    let tier = conn.query_row(
        "SELECT trust_tier FROM provenance_events WHERE event_id=?1",
        [&event.event_id],
        |r| Ok(TrustTier::from_db(r.get::<_, i64>(0).ok())),
    )?;
    Ok((tier, lowered))
}

/// Caller owns the transaction containing both the artifact body and these
/// edges. Deterministic callers may supply exact output ranges; LLM callers
/// always use the complete artifact range for every input.
pub fn record_inputs(
    conn: &Connection,
    kind: ArtifactKind,
    id: &str,
    artifact_text: &str,
    envelope: &InputEnvelope,
) -> Result<TrustTier> {
    let end = artifact_text.chars().count();
    let ranges = vec![(0, end); envelope.inputs.len()];
    record_ranges(conn, kind, id, &ranges, envelope)
}

pub fn record_ranges(
    conn: &Connection,
    kind: ArtifactKind,
    id: &str,
    ranges: &[(usize, usize)],
    envelope: &InputEnvelope,
) -> Result<TrustTier> {
    let (floor, invalidate) = record_edges(conn, kind.as_str(), id, ranges, envelope)?;
    conn.execute(
        &format!(
            "UPDATE {} SET min_trust=?2 WHERE {}=?1 AND min_trust<>?2",
            kind.table(),
            kind.key()
        ),
        params![id, floor.as_i64()],
    )?;
    if invalidate {
        lower_descendants(conn)?;
    }
    cached_floor(conn, kind, id)
}

fn record_edges(
    conn: &Connection,
    kind: &str,
    id: &str,
    ranges: &[(usize, usize)],
    envelope: &InputEnvelope,
) -> Result<(TrustTier, bool)> {
    anyhow::ensure!(
        ranges.len() == envelope.inputs.len(),
        "every input needs an artifact range"
    );
    let mut invalidate = conn.execute(
        "DELETE FROM artifact_derivations WHERE artifact_kind=?1 AND artifact_id=?2",
        params![kind, id],
    )? > 0;
    let mut floor = None;
    for (input, (start, end)) in envelope.inputs.iter().zip(ranges) {
        let (event, chunk) = input_event(conn, input)?;
        let (effective, lowered) = insert_event(conn, &event)?;
        invalidate |= lowered;
        floor = Some(floor.map_or(effective, |f: TrustTier| f.min(effective)));
        conn.execute(
            "INSERT OR IGNORE INTO artifact_derivations VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                kind,
                id,
                *start as i64,
                *end as i64,
                event.event_id,
                chunk,
                input.start as i64,
                input.end as i64
            ],
        )?;
    }
    let floor = floor.unwrap_or(TrustTier::Unknown);
    Ok((floor, invalidate))
}

/// Durable support captured from the submitted prompt, before polling. This is
/// a non-retrievable request manifest, not another memory-artifact family.
pub fn record_request_inputs(conn: &Connection, id: &str, inputs: &InputEnvelope) -> Result<()> {
    // SAVEPOINT composes with the caller's batch-state transaction and also
    // protects standalone callers from publishing a trusted partial prefix.
    conn.execute_batch("SAVEPOINT csr_request_inputs")?;
    let result: Result<()> = (|| {
        let (_, invalidate) = record_edges(
            conn,
            "narrative_request",
            id,
            &vec![(0, 0); inputs.inputs.len()],
            inputs,
        )?;
        if invalidate {
            lower_descendants(conn)?;
        }
        Ok(())
    })();
    match result {
        Ok(_) => conn.execute_batch("RELEASE csr_request_inputs")?,
        Err(error) => {
            conn.execute_batch("ROLLBACK TO csr_request_inputs; RELEASE csr_request_inputs")?;
            return Err(error);
        }
    }
    Ok(())
}

/// Recover only the frozen support set; never rebuild a completed request from
/// today's transcript. Text is intentionally empty because it is not re-prompted.
pub fn load_request_inputs(conn: &Connection, id: &str) -> Result<InputEnvelope> {
    let mut stmt=conn.prepare("SELECT e.event_id,e.conversation_id,e.message_key,e.seq,e.channel,e.trust_tier,e.parent_event_id,e.receipt_kind,e.receipt_ref,e.observed_at,d.support_start_char,d.support_end_char FROM artifact_derivations d JOIN provenance_events e ON e.event_id=d.support_event_id WHERE d.artifact_kind='narrative_request' AND d.artifact_id=?1 ORDER BY d.rowid")?;
    let inputs = stmt
        .query_map([id], |r| {
            let event = ProvenanceEvent {
                event_id: r.get(0)?,
                conversation_id: r.get(1)?,
                message_key: r.get(2)?,
                seq: r.get::<_, i64>(3)? as usize,
                channel: r.get(4)?,
                trust_tier: TrustTier::from_db(r.get::<_, i64>(5).ok()),
                parent_event_id: r.get(6)?,
                receipt_kind: r.get(7)?,
                receipt_ref: r.get(8)?,
                observed_at: r.get(9)?,
            };
            Ok(ArtifactInput {
                channel: event.channel.clone(),
                tier: event.trust_tier,
                text: String::new(),
                source: InputSource::Event(event),
                start: r.get::<_, i64>(10)? as usize,
                end: r.get::<_, i64>(11)? as usize,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let expected:i64=conn.query_row("SELECT COUNT(*) FROM artifact_derivations WHERE artifact_kind='narrative_request' AND artifact_id=?1",[id],|r|r.get(0))?;
    if inputs.len() as i64 != expected {
        return Ok(InputEnvelope::new(vec![ArtifactInput::unknown(
            "incomplete request manifest",
        )]));
    }
    Ok(InputEnvelope::new(inputs))
}

fn snapshot_floor(conn: &Connection, receipt: &str) -> Result<TrustTier> {
    let value: serde_json::Value = serde_json::from_str(receipt).unwrap_or_default();
    let kind = value["kind"].as_str().unwrap_or("");
    let parent = value["id"].as_str().unwrap_or("");
    let hash = value["content_hash"].as_str().unwrap_or("");
    Ok(if kind == "chunk" {
        let input = chunk_input(conn, parent)?;
        if content_hash(input.text()) == hash {
            input.tier
        } else {
            TrustTier::Unknown
        }
    } else if let Some(kind) = ArtifactKind::parse(kind) {
        match artifact_row(conn, kind, parent)? {
            Some((body, tier)) if content_hash(&body) == hash => tier,
            _ => TrustTier::Unknown,
        }
    } else {
        TrustTier::Unknown
    })
}

/// Fixed-point invalidation follows version-bound artifact snapshots. A later
/// source upgrade never changes a prior observation or raises descendants.
pub fn lower_descendants(conn: &Connection) -> Result<usize> {
    let mut total = 0;
    loop {
        let mut changed = 0;
        let events:Vec<(String,String,i64)>=conn.prepare("SELECT event_id,receipt_ref,trust_tier FROM provenance_events WHERE conversation_id='artifact-input' AND receipt_kind='artifact_input' AND trust_tier>0")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<std::result::Result<_,_>>()?;
        for (id, receipt, previous) in events {
            let current = snapshot_floor(conn, &receipt)?;
            if current.as_i64() < previous {
                changed += conn.execute(
                    "UPDATE provenance_events SET trust_tier=?2 WHERE event_id=?1",
                    params![id, current.as_i64()],
                )?;
            }
        }
        for kind in ARTIFACT_KINDS {
            let key = if *kind == ArtifactKind::Ledger {
                "json_array(derivation_ledger.id,derivation_ledger.repo,derivation_ledger.branch,derivation_ledger.user)".into()
            } else {
                format!("{}.{}", kind.table(), kind.key())
            };
            changed+=conn.execute(&format!("UPDATE {table} SET min_trust=COALESCE((SELECT MIN(COALESCE(e.trust_tier,0)) FROM artifact_derivations d LEFT JOIN provenance_events e ON e.event_id=d.support_event_id WHERE d.artifact_kind=?1 AND d.artifact_id={key}),0) WHERE min_trust>0 AND min_trust>COALESCE((SELECT MIN(COALESCE(e.trust_tier,0)) FROM artifact_derivations d LEFT JOIN provenance_events e ON e.event_id=d.support_event_id WHERE d.artifact_kind=?1 AND d.artifact_id={key}),0)",table=kind.table()),[kind.as_str()])?;
        }
        total += changed;
        if changed == 0 {
            return Ok(total);
        }
    }
}

impl Storage {
    pub fn get_artifact_min_trust(&self, kind: ArtifactKind, id: &str) -> Result<TrustTier> {
        self.with_connection(|c| cached_floor(c, kind, id))
    }
    pub fn artifact_input(&self, kind: ArtifactKind, id: &str) -> Result<ArtifactInput> {
        self.with_connection(|c| artifact_input(c, kind, id))
    }
    pub fn chunk_input(&self, id: &str) -> Result<ArtifactInput> {
        self.with_connection(|c| chunk_input(c, id))
    }
    pub fn chunk_inputs(&self, id: &str) -> Result<InputEnvelope> {
        self.with_connection(|c| chunk_inputs(c, id))
    }
    /// Freeze the support set of an asynchronous producer request (the AI
    /// narrative batch) before its result exists; `load_narrative_request_inputs`
    /// recovers exactly that set when the result is stored.
    pub fn record_narrative_request_inputs(&self, id: &str, inputs: &InputEnvelope) -> Result<()> {
        self.with_connection(|c| record_request_inputs(c, id, inputs))
    }
    pub fn load_narrative_request_inputs(&self, id: &str) -> Result<InputEnvelope> {
        self.with_connection(|c| load_request_inputs(c, id))
    }
    /// Cold read that fails closed: a missing or unreadable artifact is an
    /// Unknown input, never an error the producer might swallow into "no input".
    pub fn artifact_input_or_unknown(&self, kind: ArtifactKind, id: &str) -> ArtifactInput {
        self.artifact_input(kind, id)
            .unwrap_or_else(|_| ArtifactInput::unknown(&format!("{}:{id}", kind.as_str())))
    }
    /// Row-level envelope over a conversation's chunks from cached floors only.
    pub fn conversation_chunk_inputs(&self, chunk_ids: &[String]) -> InputEnvelope {
        let mut inputs = Vec::with_capacity(chunk_ids.len());
        for id in chunk_ids {
            inputs.push(
                self.chunk_input(id)
                    .unwrap_or_else(|_| ArtifactInput::unknown(&format!("chunk:{id}"))),
            );
        }
        if inputs.is_empty() {
            inputs.push(ArtifactInput::unknown("conversation without chunks"));
        }
        InputEnvelope::new(inputs)
    }
    /// Upsert a ratification score and its support edges in one transaction.
    pub fn upsert_ratification_score_with_inputs(
        &self,
        row: &super::queries::RatificationScoreRow,
        inputs: &InputEnvelope,
    ) -> Result<TrustTier> {
        self.with_connection(|c| {
            let tx = c.unchecked_transaction()?;
            queries::upsert_ratification_score(&tx, row)?;
            let floor = record_stored_inputs(
                &tx,
                ArtifactKind::Ratification,
                &row.conversation_id,
                inputs,
            )?;
            tx.commit()?;
            Ok(floor)
        })
    }
    pub fn lower_artifact_descendants(&self) -> Result<usize> {
        self.with_connection(|c| {
            let tx = c.unchecked_transaction()?;
            let n = lower_descendants(&tx)?;
            tx.commit()?;
            Ok(n)
        })
    }
    pub fn insert_derived_reflection(
        &self,
        id: &str,
        content: &str,
        tags: &[String],
        embedding: &[f32],
        inputs: &InputEnvelope,
    ) -> Result<()> {
        let ranges = vec![(0, content.chars().count()); inputs.inputs().len()];
        self.insert_derived_reflection_ranges(id, content, tags, embedding, inputs, &ranges)
    }
    pub(crate) fn insert_derived_reflection_ranges(
        &self,
        id: &str,
        content: &str,
        tags: &[String],
        embedding: &[f32],
        inputs: &InputEnvelope,
        ranges: &[(usize, usize)],
    ) -> Result<()> {
        self.with_connection(|c| {
            let tx = c.unchecked_transaction()?;
            queries::insert_reflection(&tx, id, content, tags, embedding)?;
            record_ranges(&tx, ArtifactKind::Reflection, id, ranges, inputs)?;
            tx.commit()?;
            Ok(())
        })
    }
}

/// Test-only: an observed input at a chosen tier, so other modules' tests can
/// build properly derived fixture rows instead of writing floors directly
/// (a positive floor without derivation rows is exactly what
/// `lower_descendants` treats as stale and zeroes).
#[cfg(test)]
pub fn test_observed_input(channel: &str, tier: TrustTier, text: &str) -> ArtifactInput {
    let event = ProvenanceEvent {
        event_id: format!("test-observed:{}:{}", channel, content_hash(text)),
        conversation_id: "test-observed".into(),
        message_key: content_hash(text),
        seq: 0,
        channel: channel.into(),
        trust_tier: tier,
        parent_event_id: None,
        receipt_kind: "jsonl".into(),
        receipt_ref: Some("/fixture#byte=0".into()),
        observed_at: "2026-09-04T00:00:00Z".into(),
    };
    ArtifactInput::observed(event, text, 0, text.chars().count(), content_hash(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::{ProvenanceEvent, TrustTier};
    use crate::storage::Storage;

    fn observed(channel: &str, tier: TrustTier, text: &str) -> ArtifactInput {
        let event = ProvenanceEvent {
            event_id: format!("e:{channel}"),
            conversation_id: "s".into(),
            message_key: "m".into(),
            seq: 0,
            channel: channel.into(),
            trust_tier: tier,
            parent_event_id: None,
            receipt_kind: "jsonl".into(),
            receipt_ref: Some("/fixture#byte=0".into()),
            observed_at: "now".into(),
        };
        ArtifactInput::observed(
            event,
            text,
            0,
            text.chars().count(),
            crate::provenance::content_hash(text),
        )
    }

    #[test]
    fn replayed_quote_inherits_native_span_version_and_lowered_floor() {
        let storage = Storage::open_memory().unwrap();
        let native = observed("user_message", TrustTier::External, "quoted");
        let InputSource::Event(event) = native.source else {
            panic!("native event")
        };
        storage.with_connection(|conn| {
            conn.execute("INSERT INTO chunks(id,conversation_id,project_name,timestamp,content,message_count,min_trust) VALUES('c','s','p','now','quoted',1,1)",[])?;
            queries::insert_provenance_event(conn,&event)?;
            conn.execute("INSERT INTO chunk_spans(chunk_id,event_id,start_char,end_char,content_hash) VALUES('c',?1,0,6,?2)",params![event.event_id,content_hash("quoted")])?;
            let mut replay=event.clone(); replay.event_id="replayed".into(); replay.trust_tier=TrustTier::UserHistory;
            let input=ArtifactInput::observed(replay,"quoted",0,6,content_hash("quoted"));
            let support=input.validated_stored_support(conn)?;
            assert_eq!(support.floor(),TrustTier::External);
            conn.execute("UPDATE chunks SET content='forged' WHERE id='c'",[])?;
            assert_eq!(input.validated_stored_support(conn)?.floor(),TrustTier::Unknown);
            Ok(())
        }).unwrap();
    }

    #[test]
    fn regression_publication_lowers_existing_children_of_a_shared_snapshot() {
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_derived_reflection(
                "a",
                "original",
                &[],
                &[0.1],
                &InputEnvelope::new(vec![observed(
                    "user_message",
                    TrustTier::UserHistory,
                    "source",
                )]),
            )
            .unwrap();
        let captured = InputEnvelope::new(vec![storage
            .artifact_input(ArtifactKind::Reflection, "a")
            .unwrap()]);
        storage
            .insert_derived_reflection("b", "old child", &[], &[0.1], &captured)
            .unwrap();
        storage
            .insert_reflection("a", "replacement", &[], &[0.1])
            .unwrap();
        storage
            .insert_derived_reflection("c", "new child", &[], &[0.1], &captured)
            .unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Reflection, "b")
                .unwrap(),
            TrustTier::Unknown
        );
    }

    #[test]
    fn regression_publication_uses_the_persisted_lower_snapshot_floor() {
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_derived_reflection(
                "a",
                "same",
                &[],
                &[0.1],
                &InputEnvelope::new(vec![observed(
                    "user_message",
                    TrustTier::UserHistory,
                    "source",
                )]),
            )
            .unwrap();
        let first = storage
            .artifact_input(ArtifactKind::Reflection, "a")
            .unwrap();
        storage
            .insert_derived_reflection(
                "b",
                "first child",
                &[],
                &[0.1],
                &InputEnvelope::new(vec![first]),
            )
            .unwrap();
        storage
            .with_connection(|c| {
                c.execute("UPDATE reflections SET min_trust=1 WHERE id='a'", [])?;
                Ok(())
            })
            .unwrap();
        storage.lower_artifact_descendants().unwrap();
        storage
            .with_connection(|c| {
                c.execute("UPDATE reflections SET min_trust=3 WHERE id='a'", [])?;
                Ok(())
            })
            .unwrap();
        let next = storage
            .artifact_input(ArtifactKind::Reflection, "a")
            .unwrap();
        storage
            .insert_derived_reflection(
                "c",
                "second child",
                &[],
                &[0.1],
                &InputEnvelope::new(vec![next]),
            )
            .unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Reflection, "c")
                .unwrap(),
            TrustTier::External
        );
    }

    #[test]
    fn regression_request_manifest_failure_never_commits_a_trusted_subset() {
        let storage = Storage::open_memory().unwrap();
        storage.with_connection(|c| {
            c.execute_batch("CREATE TRIGGER fail_second_input BEFORE INSERT ON artifact_derivations WHEN NEW.support_event_id='e:tool_result:WebFetch' BEGIN SELECT RAISE(ABORT,'injected write failure'); END;")?;
            let inputs=InputEnvelope::new(vec![observed("user_message",TrustTier::UserHistory,"ask"),observed("tool_result:WebFetch",TrustTier::External,"answer")]);
            assert!(record_request_inputs(c,"batch:fail",&inputs).is_err());
            assert_eq!(load_request_inputs(c,"batch:fail")?.floor(),TrustTier::Unknown);
            Ok(())
        }).unwrap();
    }

    #[test]
    fn every_artifact_mapping_reads_cached_unknown_without_inventing_support() {
        let storage = Storage::open_memory().unwrap();
        for kind in ARTIFACT_KINDS {
            assert_eq!(
                storage.get_artifact_min_trust(*kind, "missing").unwrap(),
                TrustTier::Unknown
            );
            assert_eq!(
                storage.artifact_input(*kind, "missing").unwrap().trust(),
                TrustTier::Unknown
            );
        }
    }

    #[test]
    fn request_manifest_survives_restart_and_rechecks_changed_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        {
            let storage = Storage::open(&path).unwrap();
            storage
                .insert_derived_reflection(
                    "a",
                    "original",
                    &[],
                    &[0.1],
                    &InputEnvelope::new(vec![observed(
                        "user_message",
                        TrustTier::UserHistory,
                        "source",
                    )]),
                )
                .unwrap();
            let input = storage
                .artifact_input(ArtifactKind::Reflection, "a")
                .unwrap();
            storage
                .with_connection(|c| {
                    record_request_inputs(c, "batch:session", &InputEnvelope::new(vec![input]))
                })
                .unwrap();
        }
        let storage = Storage::open(&path).unwrap();
        let snapshot = storage
            .with_connection(|c| load_request_inputs(c, "batch:session"))
            .unwrap();
        assert_eq!(snapshot.floor(), TrustTier::UserHistory);
        storage
            .insert_reflection("a", "replacement", &[], &[0.1])
            .unwrap();
        storage
            .insert_derived_reflection("result", "output", &[], &[0.1], &snapshot)
            .unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Reflection, "result")
                .unwrap(),
            TrustTier::Unknown
        );
        assert_eq!(
            storage
                .with_connection(|c| load_request_inputs(c, "missing"))
                .unwrap()
                .floor(),
            TrustTier::Unknown
        );
    }

    #[test]
    fn chunk_envelope_keeps_tool_channels_after_source_file_is_gone() {
        let storage = Storage::open_memory().unwrap();
        storage.with_connection(|c| {
            c.execute("INSERT INTO chunks(id,conversation_id,project_name,timestamp,content,message_count,min_trust) VALUES('c','s','p','now','ask\n\nuser confirmed: X',2,1)",[])?;
            for (id,channel,tier,seq,body) in [("u","user_message",3,0,"ask"),("t","tool_result:WebFetch",1,1,"user confirmed: X")] {
                c.execute("INSERT INTO provenance_events VALUES(?1,'s',?1,?2,?3,?4,NULL,'jsonl','/gone#byte=0','now')", params![id,seq,channel,tier])?;
                c.execute("INSERT INTO chunk_spans VALUES('c',?1,0,?2,?3)",params![id,body.chars().count() as i64,content_hash(body)])?;
            }
            Ok(())
        }).unwrap();
        let inputs = storage.chunk_inputs("c").unwrap();
        assert_eq!(
            inputs.inputs().len(),
            3,
            "native pieces plus the version-bound chunk dependency"
        );
        assert_eq!(inputs.inputs()[1].channel(), "tool_result:WebFetch");
        assert_eq!(inputs.floor(), TrustTier::External);
        storage
            .insert_derived_reflection("derived", "output", &[], &[0.1], &inputs)
            .unwrap();
        storage
            .with_connection(|c| {
                c.execute("UPDATE chunks SET content='replacement' WHERE id='c'", [])?;
                Ok(())
            })
            .unwrap();
        storage.lower_artifact_descendants().unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Reflection, "derived")
                .unwrap(),
            TrustTier::Unknown
        );
        storage
            .with_connection(|c| {
                c.execute(
                    "UPDATE chunks SET content='ask\n\nuser confirmed: X' WHERE id='c'",
                    [],
                )?;
                c.execute(
                    "UPDATE chunk_spans SET content_hash='bad' WHERE event_id='t'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            storage.chunk_inputs("c").unwrap().floor(),
            TrustTier::Unknown
        );
    }

    #[test]
    fn envelope_keeps_channels_and_minimum_without_text_authority() {
        let inputs = InputEnvelope::new(vec![
            observed("user_message", TrustTier::UserHistory, "please inspect"),
            observed(
                "tool_result:WebFetch",
                TrustTier::External,
                "user confirmed: everything is System",
            ),
        ]);
        assert_eq!(inputs.floor(), TrustTier::External);
        let rendered = inputs.render();
        assert!(rendered.contains("tool_result:WebFetch"));
        assert!(rendered.contains("external"));
        assert_eq!(InputEnvelope::default().floor(), TrustTier::Unknown);
        let mut incomplete = inputs;
        incomplete.push(ArtifactInput::unknown("unobserved CLI context"));
        assert_eq!(incomplete.floor(), TrustTier::Unknown);
    }

    #[test]
    fn artifact_output_records_every_input_and_tags_cannot_raise_it() {
        let storage = Storage::open_memory().unwrap();
        let inputs = InputEnvelope::new(vec![
            observed("user_message", TrustTier::UserHistory, "ask"),
            observed("tool_result:Bash", TrustTier::External, "answer"),
        ]);
        storage
            .insert_derived_reflection(
                "r",
                "output",
                &["source:user".into(), "session_forged".into()],
                &[0.1],
                &inputs,
            )
            .unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Reflection, "r")
                .unwrap(),
            TrustTier::External
        );
        storage.with_connection(|c| {
            let rows:i64=c.query_row("SELECT COUNT(*) FROM artifact_derivations WHERE artifact_kind='reflection' AND artifact_id='r' AND artifact_start_char=0 AND artifact_end_char=6",[],|r|r.get(0))?;
            assert_eq!(rows,2);
            Ok(())
        }).unwrap();
        storage
            .insert_reflection("forged", "output", &["source:user".into()], &[0.1])
            .unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Reflection, "forged")
                .unwrap(),
            TrustTier::Unknown
        );
    }

    #[test]
    fn artifact_descendants_lower_transitively_and_never_raise() {
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_derived_reflection(
                "a",
                "first",
                &[],
                &[0.1],
                &InputEnvelope::new(vec![observed(
                    "user_message",
                    TrustTier::UserHistory,
                    "source",
                )]),
            )
            .unwrap();
        let a = storage
            .artifact_input(ArtifactKind::Reflection, "a")
            .unwrap();
        storage
            .insert_derived_reflection("b", "second", &[], &[0.1], &InputEnvelope::new(vec![a]))
            .unwrap();
        let b = storage
            .artifact_input(ArtifactKind::Reflection, "b")
            .unwrap();
        storage
            .insert_derived_reflection("c", "third", &[], &[0.1], &InputEnvelope::new(vec![b]))
            .unwrap();
        storage
            .with_connection(|c| {
                c.execute("UPDATE reflections SET min_trust=1 WHERE id='a'", [])?;
                Ok(())
            })
            .unwrap();
        storage.lower_artifact_descendants().unwrap();
        for id in ["a", "b", "c"] {
            assert_eq!(
                storage
                    .get_artifact_min_trust(ArtifactKind::Reflection, id)
                    .unwrap(),
                TrustTier::External
            );
        }
        storage
            .with_connection(|c| {
                c.execute("UPDATE reflections SET min_trust=5 WHERE id='a'", [])?;
                Ok(())
            })
            .unwrap();
        storage.lower_artifact_descendants().unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Reflection, "c")
                .unwrap(),
            TrustTier::External
        );
        storage.delete_reflection("a").unwrap();
        storage.lower_artifact_descendants().unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Reflection, "c")
                .unwrap(),
            TrustTier::Unknown
        );
    }

    #[test]
    fn changed_parent_and_bad_span_fail_closed_and_cached_reads_need_no_events() {
        let storage = Storage::open_memory().unwrap();
        let input = observed("user_message", TrustTier::UserHistory, "original");
        storage
            .insert_derived_reflection("a", "first", &[], &[0.1], &InputEnvelope::new(vec![input]))
            .unwrap();
        let old = storage
            .artifact_input(ArtifactKind::Reflection, "a")
            .unwrap();
        storage
            .insert_reflection("a", "replaced", &[], &[0.1])
            .unwrap();
        storage
            .insert_derived_reflection("b", "second", &[], &[0.1], &InputEnvelope::new(vec![old]))
            .unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Reflection, "b")
                .unwrap(),
            TrustTier::Unknown
        );
        let event = ProvenanceEvent {
            event_id: "bad".into(),
            conversation_id: "s".into(),
            message_key: "m".into(),
            seq: 0,
            channel: "user_message".into(),
            trust_tier: TrustTier::UserHistory,
            parent_event_id: None,
            receipt_kind: "jsonl".into(),
            receipt_ref: None,
            observed_at: "now".into(),
        };
        assert_eq!(
            ArtifactInput::observed(event, "body", 0, 4, "wrong hash".into()).trust(),
            TrustTier::Unknown
        );
        storage.with_connection(|c| {c.execute_batch("PRAGMA foreign_keys=OFF; DROP TABLE artifact_derivations; DROP TABLE chunk_spans; DROP TABLE provenance_events;")?; Ok(())}).unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Reflection, "a")
                .unwrap(),
            TrustTier::Unknown
        );
    }
}

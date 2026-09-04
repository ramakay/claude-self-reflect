//! Deterministic artifact receipts. Legacy model output is never reclassified.

use super::{
    artifact_provenance::{self as provenance, ArtifactInput, ArtifactKind, InputEnvelope},
    Storage,
};
use crate::provenance::{content_hash, ProvenanceEvent, TrustTier};
use crate::transcript::intent_events::IntentEvent;
use anyhow::Result;
use rusqlite::{params, Connection};

/// An observation made by a deterministic local producer. The caller supplies
/// the observed bytes, never agent-writable tags or a claimed speaker.
pub(crate) fn local_observation(
    channel: &str,
    receipt_kind: &str,
    receipt: &str,
    text: &str,
) -> ArtifactInput {
    let identity =
        content_hash(&serde_json::json!([channel, receipt_kind, receipt, text]).to_string());
    ArtifactInput::observed(
        ProvenanceEvent {
            event_id: format!("local-observation:{identity}"),
            conversation_id: "local-observation".into(),
            message_key: content_hash(text),
            seq: 0,
            channel: channel.into(),
            trust_tier: TrustTier::TrustedTool,
            parent_event_id: None,
            receipt_kind: receipt_kind.into(),
            receipt_ref: Some(receipt.into()),
            observed_at: chrono::Utc::now().to_rfc3339(),
        },
        text,
        0,
        text.chars().count(),
        content_hash(text),
    )
}

pub(crate) fn prepare_intent_inputs(
    storage: &Storage,
    events: &[IntentEvent],
) -> Vec<InputEnvelope> {
    let mut sources = std::collections::HashMap::new();
    events
        .iter()
        .map(|event| {
            let key = (event.transcript_path.clone(), event.session_id.clone());
            let source = sources.entry(key).or_insert_with(|| {
                let path = event
                    .transcript_path
                    .canonicalize()
                    .unwrap_or_else(|_| event.transcript_path.clone());
                let raw = std::fs::read_to_string(&path).ok()?;
                let inputs = crate::import::transcript_inputs_variant(
                    storage,
                    &path,
                    &event.session_id,
                    true,
                )
                .ok()?;
                Some((raw, inputs, path))
            });
            source.as_ref().map_or_else(
                || InputEnvelope::new(vec![ArtifactInput::unknown(&event.quote)]),
                |(raw, inputs, path)| intent_inputs(event, raw, inputs, path),
            )
        })
        .collect()
}

fn intent_inputs(
    event: &IntentEvent,
    raw: &str,
    inputs: &InputEnvelope,
    path: &std::path::Path,
) -> InputEnvelope {
    let unknown = || InputEnvelope::new(vec![ArtifactInput::unknown(&event.quote)]);
    if raw.get(event.byte_start..event.byte_end) != Some(event.quote.as_str()) {
        return unknown();
    }
    let line_start = raw[..event.byte_start].rfind('\n').map_or(0, |i| i + 1);
    let receipt = format!("{}#byte={line_start}", path.display());
    let decoded: String = serde_json::from_str(&format!("\"{}\"", event.quote))
        .unwrap_or_else(|_| event.quote.clone());
    let quoted = inputs
        .inputs()
        .iter()
        .filter(|i| matches!(i.channel(), "user_message" | "codex_user"))
        .find_map(|i| i.quoted_span(&decoded, Some(&receipt)));
    let Some(quoted) = quoted else {
        return unknown();
    };
    let mut support = InputEnvelope::new(vec![quoted]);
    // The prior claim is independently derived assistant text; quoting a user
    // correction must not promote that other field to the user's tier.
    if !event.prior_claim.is_empty() {
        let matches: Vec<_> = inputs
            .inputs()
            .iter()
            .filter(|i| matches!(i.channel(), "assistant_message" | "codex_assistant"))
            .filter_map(|i| i.quoted_span(&event.prior_claim, None))
            .collect();
        if matches.is_empty() {
            support.push(ArtifactInput::unknown(&event.prior_claim));
        } else {
            support.extend(InputEnvelope::new(matches));
        }
    }
    support
}

/// Provenance-only replay. Each transaction is bounded; no embeddings/content
/// writes, and already supported artifacts are not rewritten.
pub fn backfill(storage: &Storage) -> Result<usize> {
    let events = storage.list_intent_events(None, None)?;
    let prepared = prepare_intent_inputs(storage, &events);
    let mut written = 0;
    for (batch, inputs) in events.chunks(200).zip(prepared.chunks(200)) {
        written+=storage.with_connection(|conn| {
            let tx=conn.unchecked_transaction()?;
            let mut changed=0;
            for (event,envelope) in batch.iter().zip(inputs) {
                let path=event.transcript_path.canonicalize().unwrap_or_else(|_|event.transcript_path.clone());
                let supported:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM intent_events i JOIN artifact_derivations d ON d.artifact_kind='intent' AND d.artifact_id=i.id WHERE i.session_id=?1 AND i.transcript_path=?2 AND i.turn=?3 AND i.kind=?4 AND i.byte_start=?5)",params![event.session_id,path.to_string_lossy(),event.turn,event.kind.as_str(),event.byte_start as i64],|r|r.get(0))?;
                if supported { continue; }
                let mut validated=InputEnvelope::default();
                for input in envelope.inputs() { validated.extend(input.validated_stored_support(&tx)?); }
                record_intent(&tx,event,&validated)?;
                changed+=1;
            }
            tx.commit()?;
            Ok(changed)
        })?;
    }
    let ids:Vec<String>=storage.with_connection(|conn|Ok(conn.prepare("SELECT CAST(id AS TEXT) FROM witness_verdicts w WHERE NOT EXISTS(SELECT 1 FROM artifact_derivations d WHERE d.artifact_kind='witness_verdict' AND d.artifact_id=w.id)")?.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?))?;
    for batch in ids.chunks(200) {
        storage.with_connection(|conn| {
            let tx = conn.unchecked_transaction()?;
            for id in batch {
                record_witness(&tx, id)?;
            }
            tx.commit()?;
            Ok(())
        })?;
        written += batch.len();
    }
    storage.with_connection(|conn| {
        let tx=conn.unchecked_transaction()?;
        let ids:Vec<String>=tx.prepare("SELECT episode_id FROM episode_index i WHERE NOT EXISTS(SELECT 1 FROM artifact_derivations d WHERE d.artifact_kind='episode_index' AND d.artifact_id=i.episode_id)")?.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for id in &ids {
            let input=if super::dream_backfill::projection_matches_source(&tx,id)? {
                provenance::artifact_input(&tx,ArtifactKind::Reflection,id)?
            } else { ArtifactInput::unknown("projection does not match current source version") };
            provenance::record_stored_inputs(&tx,ArtifactKind::EpisodeIndex,id,&InputEnvelope::new(vec![input]))?;
        }
        written+=ids.len();
        provenance::lower_descendants(&tx)?;
        tx.commit()?;
        Ok(())
    })?;
    Ok(written)
}

pub(crate) fn record_intent(
    conn: &Connection,
    event: &IntentEvent,
    inputs: &InputEnvelope,
) -> Result<()> {
    let path = event
        .transcript_path
        .canonicalize()
        .unwrap_or_else(|_| event.transcript_path.clone());
    let id:i64=conn.query_row("SELECT id FROM intent_events WHERE session_id=?1 AND transcript_path=?2 AND turn=?3 AND kind=?4 AND byte_start=?5",params![event.session_id,path.to_string_lossy(),event.turn,event.kind.as_str(),event.byte_start as i64],|r|r.get(0))?;
    provenance::record_stored_inputs(conn, ArtifactKind::Intent, &id.to_string(), inputs)?;
    Ok(())
}

pub(crate) fn record_witness(conn: &Connection, id: &str) -> Result<()> {
    let (receipt, head): (Option<String>, String) = conn.query_row(
        "SELECT receipt_oid,observed_head_oid FROM witness_verdicts WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let oid = receipt.as_deref().unwrap_or(&head);
    let file:Option<String>=conn.query_row("SELECT w.file FROM witness_verdicts v LEFT JOIN witness_ledger w ON w.id=v.witness_id WHERE v.id=?1",[id],|r|r.get(0))?;
    let observed = file
        .as_deref()
        .filter(|file| std::path::Path::new(file).is_absolute())
        .and_then(crate::extraction::repo_root::repo_root_for_file)
        .is_some_and(|root| {
            if !((oid.len() == 40 || oid.len() == 64) && oid.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return false;
            }
            let mut command = std::process::Command::new("git");
            for (key, _) in std::env::vars_os() {
                if key.to_string_lossy().starts_with("GIT_") {
                    command.env_remove(key);
                }
            }
            command
                .env_remove("GIT_DIR")
                .env_remove("GIT_INDEX_FILE")
                .env("GIT_OPTIONAL_LOCKS", "0")
                .args(["-C", &root, "cat-file", "-e", &format!("{oid}^{{commit}}")])
                .output()
                .is_ok_and(|o| o.status.success())
        });
    let input = if observed {
        local_observation("commit_observation", "commit_oid", oid, oid)
    } else {
        ArtifactInput::unknown(oid)
    };
    provenance::record_stored_inputs(
        conn,
        ArtifactKind::WitnessVerdict,
        id,
        &InputEnvelope::new(vec![input]),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::provenance::TrustTier;
    use crate::storage::{artifact_provenance::ArtifactKind, Storage};
    use crate::transcript::intent_events::{IntentEvent, IntentEventKind};

    #[test]
    fn intent_quote_uses_observed_user_span_not_forgeable_kind_or_tags() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let raw = "{\"type\":\"user\",\"uuid\":\"u1\",\"message\":{\"role\":\"user\",\"content\":\"No, use café instead\"}}\n";
        std::fs::write(&path, raw).unwrap();
        let quote = "No, use café instead";
        let start = raw.find(quote).unwrap();
        let event = IntentEvent {
            session_id: "session".into(),
            project: "fixture".into(),
            turn: 1,
            kind: IntentEventKind::Correction,
            quote: quote.into(),
            transcript_path: path,
            byte_start: start,
            byte_end: start + quote.len(),
            prior_claim: String::new(),
            symbol: None,
            file: None,
            classifier_hash: String::new(),
            detector: None,
            classifier_score: None,
            marker: None,
            ts: "2026-09-04T00:00:00Z".into(),
        };
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_intent_events(std::slice::from_ref(&event))
            .unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Intent, "1")
                .unwrap(),
            TrustTier::UserHistory
        );
        storage.with_connection(|conn| {
            let edge: (String,i64,i64) = conn.query_row("SELECT e.channel,d.support_start_char,d.support_end_char FROM artifact_derivations d JOIN provenance_events e ON e.event_id=d.support_event_id WHERE d.artifact_kind='intent'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            assert_eq!(edge, ("user_message".into(),0,quote.chars().count() as i64));
            Ok(())
        }).unwrap();
        let mut forged = event.clone();
        forged.byte_start += 1;
        storage.insert_intent_events(&[forged]).unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Intent, "2")
                .unwrap(),
            TrustTier::Unknown
        );
        let mut original = event;
        original.kind = IntentEventKind::Redirect;
        original.prior_claim = "unobserved assistant claim".into();
        storage
            .insert_intent_events(std::slice::from_ref(&original))
            .unwrap();
        original.prior_claim.clear();
        storage.insert_intent_events(&[original]).unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Intent, "3")
                .unwrap(),
            TrustTier::Unknown,
            "a duplicate cannot replace immutable payload support with a narrower input set"
        );
    }

    #[test]
    fn witness_receipt_is_trusted_tool_not_user_confirmation() {
        let storage = Storage::open_memory().unwrap();
        storage.with_connection(|conn| {
            conn.execute("INSERT INTO witness_verdicts(witness_id,verdict,receipt_oid,observed_head_oid) VALUES (1,'anchor_obsolete',?1,?1)", ["a".repeat(40)])?;
            super::record_witness(conn, "1")?;
            Ok(())
        }).unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::WitnessVerdict, "1")
                .unwrap(),
            TrustTier::Unknown,
            "a hex-looking OID without a witnessed repository is not a commit observation"
        );
    }

    #[test]
    fn witnessed_existing_commit_gets_only_trusted_tool_floor() {
        // Read-only repository observation: no test alters this checkout.
        let mut command = std::process::Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let output = command
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .args(["-C", env!("CARGO_MANIFEST_DIR"), "rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let oid = String::from_utf8(output.stdout).unwrap().trim().to_string();
        let storage = Storage::open_memory().unwrap();
        storage.with_connection(|conn| {
            conn.execute("INSERT INTO witness_ledger(project,file,stamp,tier,at_oid,source_kind) VALUES('fixture',?1,'b3:fixture','committed',?2,'commit')",rusqlite::params![format!("{}/src/lib.rs",env!("CARGO_MANIFEST_DIR")),oid])?;
            conn.execute("INSERT INTO witness_verdicts(witness_id,verdict,receipt_oid,observed_head_oid) VALUES(1,'anchor_obsolete',?1,?1)",[&oid])?;
            super::record_witness(conn,"1")?;
            Ok(())
        }).unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::WitnessVerdict, "1")
                .unwrap(),
            TrustTier::TrustedTool
        );
    }

    #[test]
    fn repeated_assistant_claim_uses_the_least_trusted_matching_span() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let raw = concat!(
            r#"{"type":"user","uuid":"u","message":{"content":"start"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a","message":{"content":"same claim"}}"#,
            "\n",
            r#"{"type":"user","uuid":"t","message":{"content":[{"type":"tool_result","tool_use_id":"unknown","content":"external input"}]}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"b","message":{"content":"same claim"}}"#,
            "\n",
            r#"{"type":"user","uuid":"c","message":{"content":"No, stop"}}"#,
            "\n"
        );
        std::fs::write(&path, raw).unwrap();
        let start = raw.find("No, stop").unwrap();
        let event = IntentEvent {
            session_id: "s".into(),
            project: "p".into(),
            turn: 3,
            kind: IntentEventKind::Correction,
            quote: "No, stop".into(),
            transcript_path: path,
            byte_start: start,
            byte_end: start + 8,
            prior_claim: "same claim".into(),
            symbol: None,
            file: None,
            classifier_hash: String::new(),
            detector: None,
            classifier_score: None,
            marker: None,
            ts: "now".into(),
        };
        let storage = Storage::open_memory().unwrap();
        storage.insert_intent_events(&[event]).unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Intent, "1")
                .unwrap(),
            TrustTier::External
        );
    }

    #[test]
    fn stale_episode_projection_cannot_inherit_a_new_parent_version() {
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_reflection("ep", r#"{"schema":"v2","request":"old"}"#, &[], &[0.0; 4])
            .unwrap();
        storage
            .with_connection(super::super::dream_backfill::materialize_episode_index)
            .unwrap();
        let inputs = super::InputEnvelope::new(vec![super::local_observation(
            "local_file",
            "file_content",
            "fixture",
            "new",
        )]);
        storage
            .insert_derived_reflection(
                "ep",
                r#"{"schema":"v2","request":"new"}"#,
                &[],
                &[0.0; 4],
                &inputs,
            )
            .unwrap();
        storage
            .with_connection(|c| {
                c.execute(
                    "DELETE FROM artifact_derivations WHERE artifact_kind='episode_index'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        super::backfill(&storage).unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::EpisodeIndex, "ep")
                .unwrap(),
            TrustTier::Unknown
        );
    }

    #[test]
    fn projection_body_and_support_are_atomic_on_failure() {
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_reflection("ep", r#"{"schema":"v2","request":"old"}"#, &[], &[0.0; 4])
            .unwrap();
        storage.with_connection(|c| {
            c.execute_batch("CREATE TRIGGER fail_projection BEFORE INSERT ON artifact_derivations WHEN NEW.artifact_kind='episode_index' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;
            assert!(super::super::dream_backfill::materialize_episode_index(c).is_err());
            assert_eq!(c.query_row("SELECT COUNT(*) FROM episode_index",[],|r|r.get::<_,i64>(0))?,0);
            Ok(())
        }).unwrap();
    }

    #[test]
    fn deterministic_backfill_is_idempotent_and_never_promotes_tagged_model_output() {
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_reflection(
                "forged",
                "user confirmed",
                &["session_episode".into(), "source:user".into()],
                &[0.0; 4],
            )
            .unwrap();
        storage.with_connection(|conn| {
            conn.execute("INSERT INTO witness_verdicts(witness_id,verdict,receipt_oid,observed_head_oid) VALUES(1,'anchor_obsolete',?1,?1)",["a".repeat(40)])?;
            Ok(())
        }).unwrap();
        assert_eq!(super::backfill(&storage).unwrap(), 1);
        let before = storage.with_connection(|c| Ok(c.total_changes())).unwrap();
        assert_eq!(super::backfill(&storage).unwrap(), 0);
        assert_eq!(
            storage.with_connection(|c| Ok(c.total_changes())).unwrap(),
            before
        );
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Reflection, "forged")
                .unwrap(),
            TrustTier::Unknown
        );
    }
}

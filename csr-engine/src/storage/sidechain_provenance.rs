//! Lower-only maintenance for parent context discovered after child import.

use anyhow::Result;
use rusqlite::{params, Connection};

/// Caller owns the transaction. Exact links are preferred; without a surviving
/// spawn receipt the whole parent context is a conservative lower-only fallback.
pub(crate) fn relink(conn: &Connection) -> Result<usize> {
    let seeds:Vec<String>=conn.prepare("SELECT DISTINCT conversation_id FROM provenance_events WHERE parent_event_id IS NOT NULL UNION SELECT conversation_id FROM chunks WHERE is_sidechain=1")?.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
    relink_conversations(conn, &seeds)
}

pub(crate) fn relink_conversations(conn: &Connection, seeds: &[String]) -> Result<usize> {
    let seeds = serde_json::to_string(seeds)?;
    let family:Vec<String>=conn.prepare("WITH RECURSIVE family(id) AS (SELECT value FROM json_each(?1) UNION SELECT c.conversation_id FROM family a JOIN chunk_provenance p ON p.source_conv_id=a.id JOIN chunks c ON c.id=p.chunk_id WHERE c.is_sidechain=1 AND c.conversation_id<>a.id UNION SELECT child.conversation_id FROM family a JOIN provenance_events parent ON parent.conversation_id=a.id JOIN provenance_events child ON child.parent_event_id=parent.event_id) SELECT id FROM family")?.query_map([seeds],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let family = serde_json::to_string(&family)?;
    let children:Vec<(String,String,Option<String>)>=conn.prepare("SELECT c.conversation_id,p.source_conv_id,(SELECT MIN(e.receipt_ref) FROM provenance_events e WHERE e.conversation_id=c.conversation_id AND e.receipt_kind='jsonl') FROM chunks c JOIN chunk_provenance p ON p.chunk_id=c.id WHERE c.conversation_id IN (SELECT value FROM json_each(?1)) AND c.is_sidechain=1 AND p.source_conv_id IS NOT NULL AND p.source_conv_id<>c.conversation_id GROUP BY c.conversation_id,p.source_conv_id")?.query_map([&family],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut total = 0;
    let bindings: Vec<_> = children
        .into_iter()
        .map(|(child, parent, receipt)| {
            let key = receipt
                .as_deref()
                .and_then(|r| r.rsplit_once("#byte=").map(|(path, _)| path))
                .and_then(|path| {
                    crate::import::sidechain_parent_message_key(std::path::Path::new(path))
                });
            (child, parent, key)
        })
        .collect();
    loop {
        let mut changed = 0;
        for (child, parent, key) in &bindings {
            let context = super::queries::parent_provenance_context(conn, parent, key.as_deref())?;
            changed+=conn.execute("UPDATE provenance_events SET trust_tier=MIN(trust_tier,?2),parent_event_id=COALESCE(parent_event_id,?3) WHERE conversation_id=?1 AND channel<>'user_confirmation' AND parent_event_id IS NULL AND (trust_tier>?2 OR ?3 IS NOT NULL)",params![child,context.floor.as_i64(),context.event_id])?;
        }
        changed+=conn.execute("UPDATE provenance_events AS child SET trust_tier=COALESCE((SELECT trust_tier FROM provenance_events parent WHERE parent.event_id=child.parent_event_id),0) WHERE conversation_id IN (SELECT value FROM json_each(?1)) AND parent_event_id IS NOT NULL AND channel<>'user_confirmation' AND trust_tier>COALESCE((SELECT trust_tier FROM provenance_events parent WHERE parent.event_id=child.parent_event_id),0)",[&family])?;
        total += changed;
        if changed == 0 {
            break;
        }
    }
    total+=conn.execute("UPDATE chunks SET min_trust=COALESCE((SELECT MIN(COALESCE(e.trust_tier,0)) FROM chunk_spans s LEFT JOIN provenance_events e USING(event_id) WHERE s.chunk_id=chunks.id),0) WHERE conversation_id IN (SELECT value FROM json_each(?1)) AND min_trust>0 AND min_trust>COALESCE((SELECT MIN(COALESCE(e.trust_tier,0)) FROM chunk_spans s LEFT JOIN provenance_events e USING(event_id) WHERE s.chunk_id=chunks.id),0)",[&family])?;
    // Only this family's chunks can have moved; lower from them, not the corpus.
    let family_chunks: Vec<String> = conn
        .prepare(
            "SELECT id FROM chunks WHERE conversation_id IN (SELECT value FROM json_each(?1))",
        )?
        .query_map([&family], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let seeds: Vec<(&str, &str)> = family_chunks
        .iter()
        .map(|id| ("chunk", id.as_str()))
        .collect();
    total += super::artifact_provenance::lower_from(conn, &seeds, &[])?;
    Ok(total)
}

impl super::Storage {
    pub fn relink_conversation(&self, conversation_id: &str) -> Result<usize> {
        self.with_connection(|conn| {
            let tx = conn.unchecked_transaction()?;
            let changed = relink_conversations(&tx, &[conversation_id.into()])?;
            tx.commit()?;
            Ok(changed)
        })
    }
    pub fn relink_sidechains(&self) -> Result<usize> {
        self.with_connection(|conn| {
            let tx = conn.unchecked_transaction()?;
            let changed = relink(&tx)?;
            tx.commit()?;
            Ok(changed)
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        provenance::{ProvenanceEvent, TrustTier},
        storage::{queries, Storage},
    };

    #[test]
    fn single_parent_evidence_write_lowers_child_artifacts_atomically() {
        use crate::provenance::{content_hash, ChunkEvidence, ChunkSpan};
        use crate::storage::artifact_provenance::ArtifactKind;
        for fail in [false, true] {
            let storage = Storage::open_memory().unwrap();
            let mut parent = ProvenanceEvent {
                event_id: "parent".into(),
                conversation_id: "parent".into(),
                message_key: "p".into(),
                seq: 0,
                channel: "user_message".into(),
                trust_tier: TrustTier::UserHistory,
                parent_event_id: None,
                receipt_kind: "jsonl".into(),
                receipt_ref: None,
                observed_at: "now".into(),
            };
            storage.with_connection(|conn| {
                for id in ["parent","child"] {
                    conn.execute("INSERT INTO chunks(id,conversation_id,project_name,timestamp,content,message_count,min_trust) VALUES(?1,?1,'p','now','source',1,3)",[id])?;
                    let mut event=parent.clone();event.event_id=id.into();event.conversation_id=id.into();
                    if id=="child" {event.parent_event_id=Some("parent".into());}
                    queries::insert_provenance_event(conn,&event)?;
                    conn.execute("INSERT INTO chunk_spans(chunk_id,event_id,start_char,end_char,content_hash) VALUES(?1,?1,0,6,?2)",rusqlite::params![id,content_hash("source")])?;
                }
                Ok(())
            }).unwrap();
            storage
                .insert_derived_reflection(
                    "output",
                    "derived",
                    &[],
                    &[0.0; 4],
                    &storage.chunk_inputs("child").unwrap(),
                )
                .unwrap();
            if fail {
                storage.with_connection(|c|{c.execute_batch("CREATE TRIGGER fail_lower BEFORE UPDATE OF min_trust ON reflections WHEN NEW.min_trust<OLD.min_trust BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;Ok(())}).unwrap();
            }
            parent.trust_tier = TrustTier::External;
            let result = storage.replace_chunk_evidence(&ChunkEvidence {
                chunk_id: "parent".into(),
                events: vec![parent],
                spans: vec![ChunkSpan {
                    chunk_id: "parent".into(),
                    event_id: "parent".into(),
                    start_char: 0,
                    end_char: 6,
                    content_hash: content_hash("source"),
                }],
                min_trust: TrustTier::External,
                tool_result_share: Some(0.0),
            });
            assert_eq!(result.is_err(), fail);
            let expected = if fail {
                TrustTier::UserHistory
            } else {
                TrustTier::External
            };
            assert_eq!(storage.get_chunk_min_trust("parent").unwrap(), expected);
            assert_eq!(storage.get_chunk_min_trust("child").unwrap(), expected);
            assert_eq!(
                storage
                    .get_artifact_min_trust(ArtifactKind::Reflection, "output")
                    .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn relinker_fallback_reaches_fixed_point_when_child_sorts_before_parent() {
        let storage = Storage::open_memory().unwrap();
        storage.with_connection(|conn| {
            for (id,tier) in [("root",TrustTier::External),("z-parent",TrustTier::UserHistory),("a-child",TrustTier::UserHistory)] {
                queries::insert_provenance_event(conn,&ProvenanceEvent {event_id:id.into(),conversation_id:id.into(),message_key:id.into(),seq:0,channel:"user_message".into(),trust_tier:tier,parent_event_id:None,receipt_kind:"jsonl".into(),receipt_ref:None,observed_at:"now".into()})?;
            }
            for (child,parent) in [("a-child","z-parent"),("z-parent","root")] {
                conn.execute("INSERT INTO chunks(id,conversation_id,project_name,timestamp,content,message_count,is_sidechain,source) VALUES(?1,?1,'p','now','text',1,1,'sidechain')",[child])?;
                conn.execute("INSERT INTO chunk_provenance(chunk_id,author,source_conv_id) VALUES(?1,'user',?2)",rusqlite::params![child,parent])?;
            }
            super::relink(conn)?;
            assert_eq!(conn.query_row("SELECT trust_tier FROM provenance_events WHERE event_id='a-child'",[],|r|r.get::<_,i64>(0))?,1);
            assert_eq!(super::relink(conn)?,0);
            Ok(())
        }).unwrap();
    }

    #[test]
    fn relinker_lowers_linked_children_and_never_raises_earlier_unknown() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                for (id, tier, parent) in [
                    ("parent", TrustTier::External, None),
                    ("child", TrustTier::UserHistory, Some("parent")),
                    ("early", TrustTier::Unknown, Some("parent")),
                    ("grandchild", TrustTier::UserHistory, Some("child")),
                ] {
                    queries::insert_provenance_event(
                        conn,
                        &ProvenanceEvent {
                            event_id: id.into(),
                            conversation_id: id.into(),
                            message_key: id.into(),
                            seq: 0,
                            channel: "user_message".into(),
                            trust_tier: tier,
                            parent_event_id: parent.map(str::to_string),
                            receipt_kind: "jsonl".into(),
                            receipt_ref: None,
                            observed_at: "2026-09-04T00:00:00Z".into(),
                        },
                    )?;
                }
                super::relink(conn)?;
                for (id, tier) in [("child", 1), ("early", 0), ("grandchild", 1)] {
                    assert_eq!(
                        conn.query_row(
                            "SELECT trust_tier FROM provenance_events WHERE event_id=?1",
                            [id],
                            |r| r.get::<_, i64>(0)
                        )?,
                        tier
                    );
                }
                conn.execute(
                    "UPDATE provenance_events SET trust_tier=3 WHERE event_id='parent'",
                    [],
                )?;
                assert_eq!(super::relink(conn)?, 0);
                Ok(())
            })
            .unwrap();
    }
}

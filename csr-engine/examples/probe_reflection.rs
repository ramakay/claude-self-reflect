//! Probe why `store_reflection` rows do not surface in `csr_reflect_on_past`.
//! Loads the live index exactly as the MCP server does, then for each target
//! reflection id: HNSW membership, raw `search_reflections` rank for the row's
//! own text, brute-force cosine rank over every stored reflection embedding,
//! and the full `reflect_on_past` pipeline output.
//!   cargo run --release --example probe_reflection -- <reflection_id>...
use anyhow::Result;
use csr_engine::engine::Engine;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[tokio::main]
async fn main() -> Result<()> {
    let db = Path::new("/Users/ramakrishnanannaswamy/.claude-self-reflect/csr-engine.db");
    let projects = Path::new("/Users/ramakrishnanannaswamy/.claude/projects");
    let mut ids: Vec<String> = Vec::new();
    let mut query_override: Option<String> = None;
    for a in std::env::args().skip(1) {
        if let Some(q) = a.strip_prefix("q=") {
            query_override = Some(q.to_string());
        } else {
            ids.push(a);
        }
    }
    let engine = Engine::new(db, projects)?;
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;

    // all stored reflection embeddings for brute force
    let mut stmt = conn.prepare("SELECT reflection_id, embedding FROM reflection_embeddings")?;
    let all: Vec<(String, Vec<f32>)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                blob_to_vec(&r.get::<_, Vec<u8>>(1)?),
            ))
        })?
        .collect::<std::result::Result<_, _>>()?;
    println!("stored reflection embeddings: {}", all.len());

    for id in &ids {
        let content: String =
            conn.query_row("SELECT content FROM reflections WHERE id=?1", [id], |r| {
                r.get(0)
            })?;
        let stored = all
            .iter()
            .find(|(i, _)| i == id)
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let content = query_override.clone().unwrap_or(content);
        let qv = engine.embeddings().embed_single(&content)?;
        println!("\n=== {} ({} chars) ===", id, content.len());
        println!(
            "stored blob dims={} norm={:.4}; cos(fresh embed of own text, stored)={:.4}",
            stored.len(),
            stored.iter().map(|x| x * x).sum::<f32>().sqrt(),
            cosine(&qv, &stored)
        );
        {
            let idx = engine.search().read().await;
            println!("has_reflection in HNSW map: {}", idx.has_reflection(id));
            let res = idx.search_reflections(&qv, 10, 0.0);
            let pos = res.iter().position(|r| &r.id == id);
            println!(
                "raw search_reflections (own text): rank={:?}; top5={:?}",
                pos.map(|p| p + 1),
                res.iter()
                    .take(5)
                    .map(|r| format!("{:.3}:{}", r.score, &r.id[..8]))
                    .collect::<Vec<_>>()
            );
        }
        // brute force
        let mut bf: Vec<(f32, &str)> = all
            .iter()
            .map(|(i, v)| (cosine(&qv, v), i.as_str()))
            .collect();
        bf.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        let bpos = bf.iter().position(|(_, i)| *i == id.as_str());
        println!(
            "brute-force cosine rank={:?}; top3={:?}",
            bpos.map(|p| p + 1),
            bf.iter()
                .take(3)
                .map(|(s, i)| format!("{:.3}:{}", s, &i[..8]))
                .collect::<Vec<_>>()
        );
        // rerank breakdown: replicate the pipeline's candidate construction
        {
            use csr_engine::provenance::TrustTier;
            use csr_engine::search::rerank::{
                adjusted_score_with, is_scaffold_text, RankCandidate, RankPolicy,
            };
            let idx = engine.search().read().await;
            let fetch = 24usize;
            let chunk_hits = idx.search_chunks(&qv, fetch, 0.1);
            let refl_hits = idx.search_reflections(&qv, fetch, 0.1);
            drop(idx);
            let storage = engine.storage();
            let mut rows: Vec<(String, f32, String, f32, f32)> = Vec::new();
            for r in &refl_hits {
                if let Ok(Some((c, _tags, ts))) = storage.get_reflection_by_id(&r.id) {
                    let content = format!("[reflection] {}", c);
                    let mk = |mt: TrustTier| RankCandidate {
                        id: r.id.clone(),
                        cosine: r.score,
                        content: content.clone(),
                        provenance: None,
                        min_trust: mt,
                        timestamp: Some(ts.clone()),
                    };
                    rows.push((
                        format!("R:{}", &r.id[..8]),
                        r.score,
                        format!("scaffold={}", is_scaffold_text(&content)),
                        adjusted_score_with(&mk(TrustTier::Unknown), RankPolicy::Recall),
                        adjusted_score_with(&mk(TrustTier::UserHistory), RankPolicy::Recall),
                    ));
                }
            }
            let ids: Vec<String> = chunk_hits.iter().map(|r| r.id.clone()).collect();
            let chunks = storage.get_chunks_by_ids(&ids)?;
            for r in &chunk_hits {
                if let Some(c) = chunks.iter().find(|c| c.id == r.id) {
                    let mt = storage
                        .get_chunk_min_trust(&c.id)
                        .unwrap_or(TrustTier::Unknown);
                    let cand = RankCandidate {
                        id: c.id.clone(),
                        cosine: r.score,
                        content: c.content.clone(),
                        provenance: storage.get_chunk_provenance(&c.id).ok().flatten(),
                        min_trust: mt,
                        timestamp: Some(c.timestamp.clone()),
                    };
                    rows.push((
                        format!("C:{}", &c.id[..8]),
                        r.score,
                        format!("trust={:?} conv={}", mt, &c.conversation_id[..8]),
                        adjusted_score_with(&cand, RankPolicy::Recall),
                        f32::NAN,
                    ));
                }
            }
            rows.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap());
            println!("rerank breakdown (sorted by adjusted, Recall policy); reflection alt = if UserHistory:");
            for (id, cos, info, adj, alt) in rows.iter().take(10) {
                println!(
                    "   {:<12} cos={:.3} adj={:.3} alt={:.3} {}",
                    id, cos, adj, alt, info
                );
            }
            let fts = storage.fts5_search(&content, fetch, None)?;
            let fts_ids: std::collections::HashSet<String> =
                fts.iter().map(|(c, _, _)| c.id.clone()).collect();
            let overlap = chunk_hits
                .iter()
                .filter(|r| fts_ids.contains(&r.id))
                .count();
            println!(
                "fts hits={} overlap with semantic chunk hits={} (of {})",
                fts.len(),
                overlap,
                chunk_hits.len()
            );
        }
        // full pipeline, project=all
        let out = csr_engine::mcp::tools::reflect_on_past(
            engine.storage(),
            engine.embeddings(),
            engine.search(),
            &content,
            6,
            0.1,
            Some("all"),
        )
        .await?;
        let hit = out.contains(id.as_str());
        let head: Vec<&str> = out.lines().take(1).collect();
        println!(
            "full reflect_on_past(own text, project=all): id present={} | {}",
            hit,
            head.join(" ")
        );
        for line in out
            .lines()
            .filter(|l| l.contains("<cid>") || l.contains("<s"))
            .take(12)
        {
            println!("   {}", line.trim());
        }
    }
    Ok(())
}

//! Daemon module — background processing for progressive enrichment.
//!
//! Runs four background tasks:
//! 1. File watcher (existing) — auto-import new JSONL files
//! 2. Extraction loop (Layer 2) — V3 extraction on imported conversations
//! 3. Narrator loop (Layer 3) — AI batch narrative generation (if API key set)
//! 4. Consolidation loop (Layer 4) — Dreamer v1 typed fact extraction from narratives

pub mod consolidation;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::api::types::BatchRequest;
use crate::api::{AnthropicClient, BatchClient};
use crate::embeddings::EmbeddingEngine;
use crate::extraction;
use crate::import;
use crate::search::SearchEngine;
use crate::storage::Storage;

/// Configuration for the daemon loops.
pub struct DaemonConfig {
    pub extraction_interval_secs: u64,
    pub batch_size_trigger: usize,
    pub batch_time_trigger_secs: u64,
    pub batch_poll_interval_secs: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            extraction_interval_secs: 30,
            batch_size_trigger: 10,
            batch_time_trigger_secs: 1800,
            batch_poll_interval_secs: 60,
        }
    }
}

/// The daemon orchestrates file watching + enrichment loops.
pub struct Daemon {
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    projects_dir: PathBuf,
    index_dir: PathBuf,
    config: DaemonConfig,
    api_client: Option<AnthropicClient>,
}

impl Daemon {
    pub fn new(
        storage: Arc<Storage>,
        embeddings: Arc<EmbeddingEngine>,
        search: Arc<RwLock<SearchEngine>>,
        projects_dir: PathBuf,
        index_dir: PathBuf,
        config: DaemonConfig,
        enable_ai: bool,
    ) -> Self {
        let api_client = if enable_ai {
            AnthropicClient::from_env()
        } else {
            None
        };
        Self {
            storage,
            embeddings,
            search,
            projects_dir,
            index_dir,
            config,
            api_client,
        }
    }

    /// Acquire a lockfile to prevent multiple daemon instances.
    /// Uses OS-level advisory locking (flock) via the fs2 crate.
    pub fn acquire_lock() -> Result<std::fs::File> {
        use fs2::FileExt;

        let lock_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".claude-self-reflect");
        std::fs::create_dir_all(&lock_dir)?;
        let lock_path = lock_dir.join("daemon.lock");

        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&lock_path)?;

        // Try non-blocking exclusive lock — fails if another daemon holds it
        file.try_lock_exclusive().map_err(|_| {
            anyhow::anyhow!(
                "another daemon instance is already running (lockfile: {})",
                lock_path.display()
            )
        })?;

        // Write PID for diagnostics (after acquiring lock)
        use std::io::Write;
        let mut locked = &file;
        write!(locked, "{}", std::process::id())?;

        Ok(file)
    }

    /// Run the daemon: file watcher + extraction loop + narrator loop.
    pub async fn run(self) -> Result<()> {
        let _lock = Self::acquire_lock()?;
        tracing::info!("daemon started, acquired lockfile");

        // Shared shutdown flag for graceful termination (D-8)
        let shutdown = Arc::new(AtomicBool::new(false));

        // Start file watcher
        let watcher = crate::import::watcher::FileWatcher::new(
            self.projects_dir.clone(),
            self.storage.clone(),
            self.embeddings.clone(),
            self.search.clone(),
            self.index_dir.clone(),
        );
        let watcher_handle = watcher.spawn();
        tracing::info!("file watcher started");

        // Start extraction loop (Layer 2 — always runs, free)
        let extraction_handle = {
            let storage = self.storage.clone();
            let embeddings = self.embeddings.clone();
            let search = self.search.clone();
            let interval = self.config.extraction_interval_secs;
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                extraction_loop(storage, embeddings, search, interval, shutdown).await;
            })
        };
        tracing::info!("extraction loop started (Layer 2)");

        // Start narrator loop (Layer 3 — only if API key set)
        let narrator_handle = if let Some(client) = self.api_client {
            let storage = self.storage.clone();
            let embeddings = self.embeddings.clone();
            let search = self.search.clone();
            let config_batch_size = self.config.batch_size_trigger;
            let config_batch_time = self.config.batch_time_trigger_secs;
            let config_poll = self.config.batch_poll_interval_secs;
            let shutdown = shutdown.clone();
            let handle = tokio::spawn(async move {
                narrator_loop(
                    storage,
                    embeddings,
                    search,
                    Arc::new(client),
                    config_batch_size,
                    config_batch_time,
                    config_poll,
                    shutdown,
                )
                .await;
            });
            tracing::info!("narrator loop started (Layer 3 — AI narrative)");
            Some(handle)
        } else {
            tracing::info!("narrator loop skipped (no ANTHROPIC_API_KEY)");
            None
        };

        // Start consolidation loop (Layer 4 — Dreamer v1, always runs, free)
        let consolidation_handle = {
            let storage = self.storage.clone();
            let embeddings = self.embeddings.clone();
            let search = self.search.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                consolidation_loop(storage, embeddings, search, shutdown).await;
            })
        };
        tracing::info!("consolidation loop started (Layer 4 — Dreamer v1)");

        // Wait for Ctrl+C
        tokio::signal::ctrl_c().await?;
        tracing::info!("shutting down daemon gracefully");

        // Signal all loops to stop
        shutdown.store(true, Ordering::SeqCst);

        // Give loops time to finish current work (5s timeout)
        let timeout = tokio::time::Duration::from_secs(5);
        let _ = tokio::time::timeout(timeout, extraction_handle).await;
        if let Some(h) = narrator_handle {
            let _ = tokio::time::timeout(timeout, h).await;
        }
        let _ = tokio::time::timeout(timeout, consolidation_handle).await;
        watcher_handle.abort(); // Watcher uses notify which doesn't check shutdown flag

        // Flush HNSW index to disk before exit
        let mut idx = self.search.write().await;
        if idx.is_dirty() {
            let chunk_count = self.storage.count_chunk_embeddings().unwrap_or(0);
            let refl_count = self.storage.count_reflection_embeddings().unwrap_or(0);
            if let Err(e) = idx.dump_to_disk(&self.index_dir, chunk_count, refl_count) {
                tracing::warn!(error = %e, "failed to flush HNSW index on shutdown");
            } else {
                tracing::info!("HNSW index flushed to disk on shutdown");
            }
        }

        tracing::info!("daemon stopped");
        Ok(())
    }
}

/// Layer 2 extraction loop: finds conversations needing V3 extraction and processes them.
async fn extraction_loop(
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    interval_secs: u64,
    shutdown: Arc<AtomicBool>,
) {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            tracing::info!("extraction loop: shutdown signal received");
            break;
        }
        if let Err(e) = extraction_loop_inner(&storage, &embeddings, &search).await {
            tracing::warn!(error = %e, "extraction loop iteration failed (non-fatal)");
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(interval_secs)).await;
    }
}

async fn extraction_loop_inner(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
) -> Result<()> {
    let unenriched = storage.get_unenriched_conversations("extracted_v3", 5)?;
    if unenriched.is_empty() {
        return Ok(());
    }

    tracing::info!(count = unenriched.len(), "processing V3 extraction queue");

    for (conv_id, file_path) in &unenriched {
        if let Err(e) =
            process_v3_extraction(storage, embeddings, search, conv_id, Path::new(file_path)).await
        {
            tracing::warn!(conv = %conv_id, error = %e, "V3 extraction failed");
            let _ = storage.mark_enrichment_failed(conv_id, "extracted_v3", &e.to_string());
        }
    }

    Ok(())
}

/// Process V3 extraction for a single conversation.
async fn process_v3_extraction(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    conv_id: &str,
    file_path: &Path,
) -> Result<()> {
    let messages = import::parse_jsonl_messages(file_path)?;
    if messages.is_empty() {
        return Ok(());
    }

    let result = extraction::extract_v3(&messages);

    // Embed the search_index
    let search_index = result.search_index.clone();
    let emb = embeddings.clone();
    let embeddings_vec =
        tokio::task::spawn_blocking(move || emb.embed(&[search_index.as_str()])).await??;

    if let Some(embedding) = embeddings_vec.into_iter().next() {
        let reflection_id = format!("extracted_v3_{conv_id}");

        // Build rich content: search_index + signature metadata
        let sig_json = serde_json::to_string(&result.signature).unwrap_or_default();
        let content = format!(
            "{}\n\n---\nSignature: {}\nContext:\n{}",
            result.search_index, sig_json, result.context_cache
        );

        let tags = vec![
            "narrative_extracted_v3".to_string(),
            format!("conv_{conv_id}"),
            format!("status_{}", result.signature.completion_status),
        ];

        // Store the V3 reflection
        storage.insert_reflection(&reflection_id, &content, &tags, &embedding)?;
        let mut idx = search.write().await;
        idx.insert_reflection(reflection_id.clone(), embedding);

        // Supersede Layer 1: delete heuristic reflection if it exists
        if let Ok(Some(old_id)) = storage.get_enrichment_reflection_id(conv_id, "heuristic") {
            let _ = storage.delete_reflection(&old_id);
            idx.remove_reflection(&old_id);
        }

        storage.mark_enrichment_completed(conv_id, "extracted_v3", &reflection_id)?;
        tracing::debug!(conv = %conv_id, "Layer 2 V3 extraction complete (supersedes Layer 1)");
    }

    Ok(())
}

/// Minimum JSONL file size (50KB) for AI narrative generation.
/// Files smaller than this are trivial/subagent sessions not worth the ~$0.012/conversation cost.
const MIN_FILE_SIZE: u64 = 50_000;

/// Layer 3 narrator loop: accumulates conversations and submits batch API requests.
#[allow(clippy::too_many_arguments)]
async fn narrator_loop(
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    client: Arc<dyn BatchClient>,
    batch_size: usize,
    batch_time_secs: u64,
    poll_interval_secs: u64,
    shutdown: Arc<AtomicBool>,
) {
    let mut last_batch_time = tokio::time::Instant::now();

    loop {
        if shutdown.load(Ordering::SeqCst) {
            tracing::info!("narrator loop: shutdown signal received");
            break;
        }
        if let Err(e) = narrator_loop_inner(
            &storage,
            &embeddings,
            &search,
            &client,
            batch_size,
            batch_time_secs,
            poll_interval_secs,
            &mut last_batch_time,
            &shutdown,
        )
        .await
        {
            tracing::warn!(error = %e, "narrator loop iteration failed (non-fatal)");
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn narrator_loop_inner(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    client: &Arc<dyn BatchClient>,
    batch_size: usize,
    batch_time_secs: u64,
    poll_interval_secs: u64,
    last_batch_time: &mut tokio::time::Instant,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let unenriched = storage.get_unenriched_conversations("ai_narrative", batch_size)?;
    let time_elapsed = last_batch_time.elapsed().as_secs() >= batch_time_secs;

    // Submit batch when enough conversations accumulate or timeout
    if unenriched.len() >= batch_size || (time_elapsed && !unenriched.is_empty()) {
        tracing::info!(count = unenriched.len(), "submitting AI narrative batch");

        // Build batch requests
        let skill_prompt = load_skill_prompt();
        let mut requests = Vec::new();

        for (conv_id, file_path) in &unenriched {
            // Skip tiny files (subagent/initialization sessions) — not worth API cost
            let path = Path::new(file_path);
            if let Ok(meta) = std::fs::metadata(path) {
                if meta.len() < MIN_FILE_SIZE {
                    let _ = storage.mark_enrichment_completed(
                        conv_id,
                        "ai_narrative",
                        "skipped_too_small",
                    );
                    tracing::debug!(
                        conv = %conv_id,
                        size = meta.len(),
                        min = MIN_FILE_SIZE,
                        "skipping tiny conversation for AI narrative"
                    );
                    continue;
                }
            }

            let messages = import::parse_jsonl_messages(path)?;
            if messages.is_empty() {
                continue;
            }

            // Build a conversation summary for the AI with size cap (D-16)
            // Sample first 50 + last 50 messages to capture both context and resolution
            let mut summary = String::new();
            let max_prompt_chars: usize = 100_000; // ~25K tokens budget
            let total = messages.len();
            let head = 50.min(total);
            let tail_start = if total > 100 { total - 50 } else { head };
            let sampled: Vec<_> = messages[..head]
                .iter()
                .chain(messages[tail_start..].iter())
                .collect();
            for m in &sampled {
                let line = serde_json::to_string(m).unwrap_or_default();
                if summary.len() + line.len() > max_prompt_chars {
                    break;
                }
                if !summary.is_empty() {
                    summary.push('\n');
                }
                summary.push_str(&line);
            }

            // XML boundary tags separate system prompt from user data (D-4)
            // Escape XML boundary tags in transcript to prevent prompt injection (Codex H-1)
            let sanitized_summary = summary
                .replace("</conversation_data>", "&lt;/conversation_data&gt;")
                .replace("<conversation_data>", "&lt;conversation_data&gt;");
            let prompt = format!(
                "{}\n\n---\n<conversation_data>\n{}\n</conversation_data>",
                skill_prompt, sanitized_summary
            );
            requests.push(BatchRequest {
                custom_id: conv_id.clone(),
                prompt,
            });
        }

        if requests.is_empty() {
            return Ok(());
        }

        // Collect submitted IDs before requests is moved (Codex M-1)
        let submitted_ids: Vec<String> = requests.iter().map(|r| r.custom_id.clone()).collect();

        // Submit batch
        let batch_id = client.create_batch(requests).await?;
        tracing::info!(batch_id = %batch_id, "batch submitted");

        // Store prompt hash for re-enrichment detection (D-11)
        let hash = prompt_hash(&skill_prompt);
        for (conv_id, _) in &unenriched {
            if submitted_ids.contains(conv_id) {
                let _ = storage.set_batch_id(conv_id, &batch_id, &hash);
            }
        }

        // Spawn batch polling as a separate task (D-7: non-blocking)
        let poll_storage = storage.clone();
        let poll_embeddings = embeddings.clone();
        let poll_search = search.clone();
        let poll_client = client.clone();
        let poll_unenriched = unenriched.clone();
        let poll_shutdown = shutdown.clone();
        tokio::spawn(async move {
            if let Err(e) = poll_batch_results(
                &poll_storage,
                &poll_embeddings,
                &poll_search,
                &poll_client,
                &batch_id,
                &poll_unenriched,
                poll_interval_secs,
                &poll_shutdown,
            )
            .await
            {
                tracing::warn!(error = %e, "batch polling failed");
            }
        });

        *last_batch_time = tokio::time::Instant::now();
    }

    Ok(())
}

/// Poll a submitted batch until completion, then store results. Runs as a separate task (D-7).
#[allow(clippy::too_many_arguments)]
async fn poll_batch_results(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    client: &Arc<dyn BatchClient>,
    batch_id: &str,
    unenriched: &[(String, String)],
    poll_interval_secs: u64,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            tracing::info!("batch polling: shutdown signal received");
            return Ok(());
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(poll_interval_secs)).await;
        let status = client.get_batch_status(batch_id).await?;

        if status.processing_status == "ended" {
            tracing::info!(batch_id = %batch_id, "batch completed");
            let results = client.get_batch_results(batch_id).await?;

            for item in &results {
                if let Err(e) = store_narrative(
                    storage,
                    embeddings,
                    search,
                    &item.custom_id,
                    &item.narrative,
                )
                .await
                {
                    tracing::warn!(
                        conv = %item.custom_id,
                        error = %e,
                        "failed to store narrative"
                    );
                }
            }
            return Ok(());
        } else if status.processing_status == "errored"
            || status.processing_status == "expired"
            || status.processing_status == "canceled"
        {
            tracing::warn!(
                batch_id = %batch_id,
                status = %status.processing_status,
                "batch failed"
            );
            for (conv_id, _) in unenriched {
                let _ = storage.mark_enrichment_failed(
                    conv_id,
                    "ai_narrative",
                    &format!("batch {}", status.processing_status),
                );
            }
            return Ok(());
        }
    }
}

/// Store an AI narrative as a reflection, superseding Layer 2.
async fn store_narrative(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    conv_id: &str,
    narrative: &str,
) -> Result<()> {
    let narrative_owned = narrative.to_string();
    let emb = embeddings.clone();
    let embeddings_vec =
        tokio::task::spawn_blocking(move || emb.embed(&[narrative_owned.as_str()])).await??;

    if let Some(embedding) = embeddings_vec.into_iter().next() {
        let reflection_id = format!("ai_narrative_{conv_id}");
        let tags = vec!["narrative_ai".to_string(), format!("conv_{conv_id}")];

        storage.insert_reflection(&reflection_id, narrative, &tags, &embedding)?;
        let mut idx = search.write().await;
        idx.insert_reflection(reflection_id.clone(), embedding);

        // Supersede Layer 2: delete V3 extraction reflection if it exists
        if let Ok(Some(old_id)) = storage.get_enrichment_reflection_id(conv_id, "extracted_v3") {
            let _ = storage.delete_reflection(&old_id);
            idx.remove_reflection(&old_id);
        }

        storage.mark_enrichment_completed(conv_id, "ai_narrative", &reflection_id)?;
        tracing::debug!(conv = %conv_id, "Layer 3 AI narrative stored (supersedes Layer 2)");
    }

    Ok(())
}

/// Maximum size for a skill prompt override file (10KB).
const MAX_SKILL_PROMPT_SIZE: u64 = 10 * 1024;

/// Load the SKILL_V2 prompt with layered loading (per Codex F-5).
/// Checks for runtime override (capped at 10KB), falls back to compiled-in default.
fn load_skill_prompt() -> String {
    let override_path = dirs::home_dir()
        .unwrap_or_default()
        .join(".claude-self-reflect")
        .join("skill_v2.md");

    if override_path.exists() {
        // Check file size before reading (D-10: prevent oversized prompts)
        if let Ok(meta) = std::fs::metadata(&override_path) {
            if meta.len() > MAX_SKILL_PROMPT_SIZE {
                tracing::warn!(
                    path = %override_path.display(),
                    size = meta.len(),
                    max = MAX_SKILL_PROMPT_SIZE,
                    "custom SKILL prompt too large, using default"
                );
            } else if let Ok(content) = std::fs::read_to_string(&override_path) {
                tracing::info!(
                    path = %override_path.display(),
                    "using custom SKILL prompt"
                );
                return content;
            }
        }
    }

    include_str!("../../data/SKILL_V2.md").to_string()
}

/// Layer 4 consolidation loop: extracts typed facts from V3/AI narratives (Dreamer v1).
async fn consolidation_loop(
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    shutdown: Arc<AtomicBool>,
) {
    // Wait 120 seconds before first run — let extraction/narrator populate data first
    let interval = tokio::time::Duration::from_secs(120);
    loop {
        if shutdown.load(Ordering::SeqCst) {
            tracing::info!("consolidation loop: shutdown signal received");
            break;
        }
        if let Err(e) = run_consolidation(&storage, &embeddings, &search).await {
            tracing::warn!(error = %e, "consolidation loop iteration failed (non-fatal)");
        }
        tokio::time::sleep(interval).await;
    }
}

/// Inner consolidation step: process unconsolidated conversations and store extracted facts.
async fn run_consolidation(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
) -> Result<()> {
    let unconsolidated = storage.get_unconsolidated_conversations(10)?;
    if unconsolidated.is_empty() {
        return Ok(());
    }

    tracing::info!(
        count = unconsolidated.len(),
        "consolidating conversations (Dreamer v1)"
    );

    for (conv_id, narrative_content) in &unconsolidated {
        let facts = consolidation::extract_facts(narrative_content);
        if facts.is_empty() {
            // Mark as skipped (avoid reprocessing)
            storage.mark_consolidated_skipped(conv_id)?;
            continue;
        }

        // Look up project for cross-project scoping (Codex M-4)
        let project_name = storage
            .get_project_for_conversation(conv_id)
            .ok()
            .flatten()
            .unwrap_or_default();

        // Store each fact as a tagged reflection with embedding (for HNSW search)
        let mut stored_ids = Vec::new();
        for (i, fact) in facts.iter().enumerate() {
            let reflection_id = format!("fact_{}_{conv_id}_{i}", fact.fact_type);
            let content = format!("[{}] {}", fact.fact_type, fact.content);
            let mut tags = vec![
                "consolidated_fact".to_string(),
                format!("fact_type_{}", fact.fact_type),
                format!("conv_{conv_id}"),
            ];
            if !project_name.is_empty() {
                tags.push(format!("project_{project_name}"));
            }

            // Embed the fact content for searchability
            let content_for_embed = content.clone();
            let emb = embeddings.clone();
            let embeddings_vec =
                tokio::task::spawn_blocking(move || emb.embed(&[content_for_embed.as_str()]))
                    .await??;

            if let Some(embedding) = embeddings_vec.into_iter().next() {
                storage.insert_reflection(&reflection_id, &content, &tags, &embedding)?;
                let mut idx = search.write().await;
                idx.insert_reflection(reflection_id.clone(), embedding);
                stored_ids.push(reflection_id);
            }
        }

        // Mark conversation as consolidated, linking to the first fact reflection
        let link_id = stored_ids.first().cloned().unwrap_or_default();
        storage.mark_consolidated(conv_id, &link_id)?;
        tracing::debug!(
            conv = %&conv_id[..8.min(conv_id.len())],
            facts = facts.len(),
            "Layer 4 consolidation complete"
        );
    }

    Ok(())
}

/// Compute SHA-256 hash of prompt content (for re-enrichment detection).
pub fn prompt_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_hash_deterministic() {
        let h1 = prompt_hash("hello world");
        let h2 = prompt_hash("hello world");
        let h3 = prompt_hash("different");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_load_skill_prompt_fallback() {
        // Should always succeed with compiled-in default
        let prompt = load_skill_prompt();
        assert!(!prompt.is_empty());
    }

    #[test]
    fn test_daemon_config_defaults() {
        let config = DaemonConfig::default();
        assert_eq!(config.extraction_interval_secs, 30);
        assert_eq!(config.batch_size_trigger, 10);
        assert_eq!(config.batch_time_trigger_secs, 1800);
        assert_eq!(config.batch_poll_interval_secs, 60);
    }

    #[test]
    fn test_conversation_sampling_short() {
        // For <= 100 messages, all should be included (no gap)
        let messages: Vec<i32> = (0..80).collect();
        let total = messages.len();
        let head = 50.min(total);
        let tail_start = if total > 100 { total - 50 } else { head };
        let sampled: Vec<_> = messages[..head]
            .iter()
            .chain(messages[tail_start..].iter())
            .collect();
        assert_eq!(sampled.len(), 80); // all messages included
    }

    #[test]
    fn test_conversation_sampling_long() {
        // For > 100 messages, should get first 50 + last 50
        let messages: Vec<i32> = (0..200).collect();
        let total = messages.len();
        let head = 50.min(total);
        let tail_start = if total > 100 { total - 50 } else { head };
        let sampled: Vec<_> = messages[..head]
            .iter()
            .chain(messages[tail_start..].iter())
            .collect();
        assert_eq!(sampled.len(), 100);
        assert_eq!(*sampled[0], 0);
        assert_eq!(*sampled[49], 49);
        assert_eq!(*sampled[50], 150); // first of the tail
        assert_eq!(*sampled[99], 199);
    }
}

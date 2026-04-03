# CSR Algorithms: Rust Implementation Patterns (for Codex Review)

## 1. OBRL: Storage Layer (Database Schema)

### Current Schema (Phase 3)
```sql
-- reflections table (existing)
CREATE TABLE reflections (
    id TEXT PRIMARY KEY,
    project TEXT NOT NULL,
    content TEXT NOT NULL,
    embedding BLOB,
    created_at TEXT NOT NULL,
    source_type TEXT NOT NULL
);

-- What's MISSING for OBRL:
-- Track which memories were injected and whether Claude used them
```

### New Tables Needed
```sql
-- Record each hook injection event
CREATE TABLE memory_injections (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    memory_id TEXT NOT NULL,
    hook_type TEXT NOT NULL,           -- SessionStart, PromptSubmit, Stop
    rank_in_injection INTEGER NOT NULL,  -- Position 1, 2, 3...
    injected_at TEXT NOT NULL,
    FOREIGN KEY (memory_id) REFERENCES reflections(id)
);

-- Track outcomes (was memory cited?)
CREATE TABLE memory_outcomes (
    id TEXT PRIMARY KEY,
    injection_id TEXT NOT NULL,
    was_cited BOOLEAN DEFAULT FALSE,   -- Did Claude use this?
    led_to_success BOOLEAN DEFAULT NULL,  -- Did session complete?
    citation_location TEXT,             -- e.g., "turn 5, tool_result"
    updated_at TEXT NOT NULL,
    FOREIGN KEY (injection_id) REFERENCES memory_injections(id)
);

-- Cache hook reward profiles (computed quarterly)
CREATE TABLE hook_reward_profiles (
    id TEXT PRIMARY KEY,
    hook_type TEXT NOT NULL,
    project TEXT NOT NULL,
    avg_citation_rate REAL NOT NULL,   -- 0.0-1.0
    avg_success_rate REAL NOT NULL,    -- 0.0-1.0
    success_delta REAL NOT NULL,       -- +X% vs baseline
    computed_at TEXT NOT NULL,
    UNIQUE (hook_type, project)
);
```

### Rust Storage Implementation
```rust
// In src/storage/mod.rs or src/storage/obrl.rs

impl Storage {
    /// Record an injection event and its outcome
    pub fn record_injection(
        &self,
        session_id: &str,
        memory_id: &str,
        hook_type: &str,
        rank: usize,
    ) -> Result<String> {
        let injection_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO memory_injections (id, session_id, memory_id, hook_type, rank_in_injection, injected_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            [&injection_id, session_id, memory_id, hook_type, &rank.to_string(), &now],
        )?;
        
        Ok(injection_id)
    }
    
    /// Update outcome after Claude processes the hook
    pub fn record_outcome(
        &self,
        injection_id: &str,
        was_cited: bool,
        led_to_success: Option<bool>,
    ) -> Result<()> {
        let outcome_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO memory_outcomes (id, injection_id, was_cited, led_to_success, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                &outcome_id,
                injection_id,
                was_cited,
                led_to_success,
                &now
            ],
        )?;
        
        Ok(())
    }
    
    /// Compute hook reward profile (call weekly or monthly)
    pub fn compute_hook_reward_profile(
        &self,
        hook_type: &str,
        project: &str,
    ) -> Result<HookRewardProfile> {
        let conn = self.conn.lock().unwrap();
        
        // Citation rate: % of injected memories that were cited
        let (cited_count, total_count): (i64, i64) = conn.query_row(
            "SELECT COUNT(*) FILTER (WHERE was_cited), COUNT(*)
             FROM memory_outcomes mo
             JOIN memory_injections mi ON mo.injection_id = mi.id
             WHERE mi.hook_type = ?1 AND mi.project = ?2",
            [hook_type, project],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        
        let citation_rate = if total_count > 0 {
            cited_count as f32 / total_count as f32
        } else {
            0.5  // Default: assume 50% if no data
        };
        
        // Success rate: % of sessions where outcome was success
        let (success_count, total_sessions): (i64, i64) = conn.query_row(
            "SELECT COUNT(*) FILTER (WHERE led_to_success = 1), COUNT(DISTINCT session_id)
             FROM memory_outcomes mo
             JOIN memory_injections mi ON mo.injection_id = mi.id
             WHERE mi.hook_type = ?1",
            [hook_type],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        
        let success_rate = if total_sessions > 0 {
            success_count as f32 / total_sessions as f32
        } else {
            0.65  // Baseline: ~65% session completion
        };
        
        let baseline = 0.65;  // Empirical baseline for all hooks
        let success_delta = success_rate - baseline;
        
        let profile = HookRewardProfile {
            hook_type: hook_type.to_string(),
            project: project.to_string(),
            avg_citation_rate: citation_rate,
            avg_success_rate: success_rate,
            success_delta,
            computed_at: chrono::Utc::now(),
        };
        
        Ok(profile)
    }
}
```

---

## 2. CFP: Flow Prediction (Storage + Logic)

### Database Schema
```sql
CREATE TABLE flow_signatures (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    project TEXT NOT NULL,
    question_types TEXT NOT NULL,  -- JSON: ["Debugging", "Solution", "Verification"]
    confidence REAL NOT NULL,       -- 0.0-1.0 (avg pairwise similarity)
    created_at TEXT NOT NULL
);

CREATE TABLE question_classifications (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_number INTEGER NOT NULL,
    question_type TEXT NOT NULL,
    errors_detected TEXT,           -- JSON array of error names
    tools_used TEXT,                -- JSON array of tool names
    classified_at TEXT NOT NULL
);
```

### Rust Implementation
```rust
// In src/temporal/flow.rs (new module)

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum QuestionType {
    Diagnosis,       // "What's the error?"
    Hypothesis,      // "Could it be X?"
    Solution,        // "How do I fix it?"
    Implementation,  // "Does my code work?"
    Verification,    // "Is it production-ready?"
    Prevention,      // "How to prevent next time?"
}

impl QuestionType {
    /// Heuristic: classify based on errors, tools, and keywords
    pub fn classify(prompt: &str, errors: &[String], tools: &[String]) -> Self {
        let prompt_lower = prompt.to_lowercase();
        
        // Diagnostic signals: error messages, stack traces
        if !errors.is_empty() && prompt_lower.contains("error") {
            return QuestionType::Diagnosis;
        }
        
        // Solution signals: "how", "fix", "solve"
        if prompt_lower.contains("how") || prompt_lower.contains("fix") {
            return QuestionType::Solution;
        }
        
        // Implementation signals: "check", "review", "looks good"
        if prompt_lower.contains("review") || prompt_lower.contains("check") {
            return QuestionType::Implementation;
        }
        
        // Prevention signals: "avoid", "prevent", "best practice"
        if prompt_lower.contains("prevent") || prompt_lower.contains("best practice") {
            return QuestionType::Prevention;
        }
        
        // Default
        QuestionType::Hypothesis
    }
}

pub async fn predict_next_questions(
    storage: &Arc<Storage>,
    current_type: &QuestionType,
    project: &str,
    limit: usize,
) -> Result<Vec<(QuestionType, f32)>> {
    let conn = storage.conn.lock().unwrap();
    
    // Find past flow signatures in this project starting with similar type
    let mut stmt = conn.prepare(
        "SELECT question_types FROM flow_signatures
         WHERE project = ?1 AND question_types LIKE ?2
         ORDER BY created_at DESC
         LIMIT 100"
    )?;
    
    let current_str = format!("{:?}", current_type);
    let pattern = format!("%{}%", current_str);
    
    let mut next_types: HashMap<QuestionType, usize> = HashMap::new();
    let mut flows = stmt.query_map([project, &pattern], |row| {
        let json_str: String = row.get(0)?;
        Ok(json_str)
    })?;
    
    while let Some(Ok(json_str)) = flows.next() {
        if let Ok(types) = serde_json::from_str::<Vec<String>>(&json_str) {
            // Find current type, return next type
            if let Some(idx) = types.iter().position(|t| t == &current_str) {
                if idx + 1 < types.len() {
                    if let Ok(next_type) = serde_json::from_str::<QuestionType>(&format!("\"{}\"", types[idx + 1])) {
                        *next_types.entry(next_type).or_insert(0) += 1;
                    }
                }
            }
        }
    }
    
    // Convert to probabilities
    let total: usize = next_types.values().sum();
    if total == 0 {
        return Ok(vec![]);  // No prediction possible
    }
    
    let mut predictions: Vec<_> = next_types
        .into_iter()
        .map(|(qtype, count)| (qtype, count as f32 / total as f32))
        .collect();
    
    predictions.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(predictions.into_iter().take(limit).collect())
}

/// Store flow signature at session end
pub async fn record_flow_signature(
    storage: &Arc<Storage>,
    session_id: &str,
    project: &str,
    types: Vec<QuestionType>,
) -> Result<()> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    
    // Compute confidence: average pairwise distance between consecutive types
    // (simplified: just use length as proxy)
    let confidence = if types.len() > 2 {
        0.8  // Multi-turn → high confidence
    } else {
        0.5  // Single turn → low confidence
    };
    
    let json_types = serde_json::to_string(&types)?;
    
    let conn = storage.conn.lock().unwrap();
    conn.execute(
        "INSERT INTO flow_signatures (id, session_id, project, question_types, confidence, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![&id, session_id, project, &json_types, confidence, &now],
    )?;
    
    Ok(())
}
```

---

## 3. HMC: Memory Consolidation (Clustering)

### Database Schema
```sql
CREATE TABLE memory_clusters (
    id TEXT PRIMARY KEY,
    primary_memory_id TEXT NOT NULL,
    child_memory_ids TEXT NOT NULL,  -- JSON: ["mem_2", "mem_3", "mem_5"]
    parent_cluster_id TEXT,           -- NULL unless hierarchical
    consolidation_score REAL NOT NULL, -- Avg similarity of children
    represents_consensus BOOLEAN,      -- All children agree on solution?
    represents_contradiction BOOLEAN,  -- Children disagree?
    created_at TEXT NOT NULL,
    FOREIGN KEY (primary_memory_id) REFERENCES reflections(id)
);

CREATE INDEX idx_memory_clusters_primary ON memory_clusters(primary_memory_id);
CREATE INDEX idx_memory_clusters_contradiction ON memory_clusters(represents_contradiction);
```

### Rust Implementation
```rust
// In src/search/consolidation.rs (new module)

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCluster {
    pub id: String,
    pub primary_memory_id: String,
    pub child_memory_ids: Vec<String>,
    pub consolidation_score: f32,  // 0.0-1.0
    pub represents_consensus: bool,
    pub represents_contradiction: bool,
}

pub async fn consolidate_memories(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    similarity_threshold: f32,  // e.g., 0.85
) -> Result<Vec<MemoryCluster>> {
    // Step 1: Load all reflections
    let all_reflections = storage.get_all_reflections().await?;
    
    // Step 2: Compute pairwise similarity (cosine)
    let mut similarity_matrix: HashMap<(usize, usize), f32> = HashMap::new();
    for i in 0..all_reflections.len() {
        for j in (i + 1)..all_reflections.len() {
            let sim = cosine_similarity(
                &all_reflections[i].embedding,
                &all_reflections[j].embedding,
            );
            if sim > similarity_threshold {
                similarity_matrix.insert((i, j), sim);
            }
        }
    }
    
    // Step 3: Agglomerative clustering (bottom-up)
    let mut cluster_members: Vec<Vec<usize>> = all_reflections
        .iter()
        .enumerate()
        .map(|(i, _)| vec![i])
        .collect();
    
    loop {
        // Find closest pair of clusters
        let mut best_pair = None;
        let mut best_sim = similarity_threshold;
        
        for i in 0..cluster_members.len() {
            for j in (i + 1)..cluster_members.len() {
                // Average linkage: avg similarity between all pairs
                let mut sum_sim = 0.0;
                let mut count = 0;
                
                for &mi in &cluster_members[i] {
                    for &mj in &cluster_members[j] {
                        let key = if mi < mj { (mi, mj) } else { (mj, mi) };
                        if let Some(&sim) = similarity_matrix.get(&key) {
                            sum_sim += sim;
                            count += 1;
                        }
                    }
                }
                
                if count > 0 {
                    let avg_sim = sum_sim / count as f32;
                    if avg_sim > best_sim {
                        best_sim = avg_sim;
                        best_pair = Some((i, j));
                    }
                }
            }
        }
        
        // If no more pairs above threshold, stop
        if let Some((i, j)) = best_pair {
            // Merge cluster j into i
            cluster_members[i].extend_from_slice(&cluster_members[j]);
            cluster_members.remove(j);
        } else {
            break;
        }
    }
    
    // Step 4: Convert clusters to MemoryCluster structs
    let mut clusters = Vec::new();
    for members in cluster_members {
        if members.is_empty() {
            continue;
        }
        
        let primary_idx = members[0];  // Strongest (first)
        let primary = &all_reflections[primary_idx];
        
        let child_ids: Vec<String> = members[1..]
            .iter()
            .map(|&idx| all_reflections[idx].id.clone())
            .collect();
        
        // Check for contradictions
        let (consensus, contradiction) = check_contradiction(
            storage,
            &primary.id,
            &child_ids,
        ).await?;
        
        let avg_sim = {
            let mut sum = 0.0;
            for &mi in &members {
                for &mj in &members {
                    if mi != mj {
                        let key = if mi < mj { (mi, mj) } else { (mj, mi) };
                        if let Some(&sim) = similarity_matrix.get(&key) {
                            sum += sim;
                        }
                    }
                }
            }
            if members.len() > 1 {
                sum / (members.len() as f32 * (members.len() - 1) as f32 / 2.0)
            } else {
                1.0
            }
        };
        
        clusters.push(MemoryCluster {
            id: uuid::Uuid::new_v4().to_string(),
            primary_memory_id: primary.id.clone(),
            child_memory_ids: child_ids,
            consolidation_score: avg_sim,
            represents_consensus: consensus,
            represents_contradiction: contradiction,
        });
    }
    
    // Step 5: Store clusters in DB
    for cluster in &clusters {
        let child_json = serde_json::to_string(&cluster.child_memory_ids)?;
        let conn = storage.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO memory_clusters (id, primary_memory_id, child_memory_ids, 
             consolidation_score, represents_consensus, represents_contradiction, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                &cluster.id,
                &cluster.primary_memory_id,
                &child_json,
                cluster.consolidation_score,
                cluster.represents_consensus,
                cluster.represents_contradiction,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
    }
    
    Ok(clusters)
}

/// Check if cluster memories agree on solution
async fn check_contradiction(
    storage: &Arc<Storage>,
    primary_id: &str,
    child_ids: &[String],
) -> Result<(bool, bool)> {
    let primary = storage.get_reflection(primary_id).await?;
    let primary_lower = primary.content.to_lowercase();
    
    let conflict_keywords = ["don't", "avoid", "never", "wrong", "buggy"];
    let mut contradiction_count = 0;
    
    for child_id in child_ids {
        let child = storage.get_reflection(child_id).await?;
        let child_lower = child.content.to_lowercase();
        
        // Simple heuristic: if primary says "don't X" but child doesn't mention "don't",
        // they might disagree
        for kw in &conflict_keywords {
            let primary_has = primary_lower.contains(kw);
            let child_has = child_lower.contains(kw);
            
            if primary_has != child_has {
                contradiction_count += 1;
            }
        }
    }
    
    let contradiction = contradiction_count > child_ids.len() / 2;
    let consensus = !contradiction;
    
    Ok((consensus, contradiction))
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}
```

---

## 4. Type-Aware Decay (search/decay.rs Enhancement)

```rust
// In src/search/decay.rs

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum MemoryType {
    Fact(FactType),
    Solution(SolutionStatus),
    Strategy,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum FactType {
    Permanent,   // Security, protocol
    Temporary,   // "v7 dropped Python 2.7"
    Bugfix,      // CVE fixes
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SolutionStatus {
    Current,      // Latest best practice
    Superseded,   // Older workaround
    Deprecated,   // Don't use anymore
}

pub fn adaptive_decay(
    base_score: f32,
    memory_type: MemoryType,
    age_days: f64,
    last_validated_days: Option<f64>,
) -> f32 {
    match memory_type {
        MemoryType::Fact(FactType::Permanent) => {
            // Security vulnerabilities don't decay
            if let Some(validated_age) = last_validated_days {
                if validated_age < 180.0 {
                    base_score
                } else {
                    base_score * 0.95
                }
            } else {
                base_score * 0.99
            }
        }
        MemoryType::Solution(SolutionStatus::Current) => {
            // Standard: moderate decay, reset on reuse
            let decay_scale = if let Some(reuse_age) = last_validated_days {
                if reuse_age < 30.0 { 180.0 } else { 90.0 }
            } else {
                90.0
            };
            base_score * 2.0_f64.powf(-age_days / decay_scale) as f32
        }
        MemoryType::Solution(SolutionStatus::Deprecated) => {
            // Rapid decay
            base_score * 2.0_f64.powf(-age_days / 30.0) as f32
        }
        MemoryType::Solution(SolutionStatus::Superseded) => {
            // Very rapid (new solution exists)
            base_score * 2.0_f64.powf(-age_days / 14.0) as f32
        }
        MemoryType::Strategy => {
            // Moderate decay, boost on reuse
            if let Some(reuse_age) = last_validated_days {
                if reuse_age < 60.0 {
                    base_score
                } else {
                    base_score * 2.0_f64.powf(-reuse_age / 120.0) as f32
                }
            } else {
                base_score * 2.0_f64.powf(-age_days / 120.0) as f32
            }
        }
        MemoryType::Error => {
            // Boost if recurring, otherwise decay
            // (requires error tracking logic)
            base_score * 2.0_f64.powf(-age_days / 60.0) as f32
        }
        _ => {
            // Default exponential
            base_score * 2.0_f64.powf(-age_days / 90.0) as f32
        }
    }
}
```

---

## 5. Integration Points (Where These Connect)

### SessionStart Hook (src/hooks/session_start.rs)
```rust
// Add CFP + OBRL integration
pub async fn handle_inner(...) -> Result<()> {
    // Existing code...
    
    // NEW: Add CFP prediction
    let current_type = classify_question_type(ralph)?;
    let predicted = predict_next_questions(engine.storage(), &current_type, project, 2).await?;
    
    // NEW: Use OBRL weights instead of fixed w_phase
    let hook_reward = engine.storage()
        .get_hook_reward_profile("SessionStart", project)
        .await
        .ok();
    
    let mut search_results = engine.search_semantic(query, 5).await?;
    
    // Re-rank by OBRL weight
    if let Some(reward) = hook_reward {
        for result in &mut search_results {
            result.score *= (1.0 + reward.success_delta);
        }
    }
    
    // NEW: Consolidate results with HMC
    let clusters = engine.get_memory_clusters().await?;
    let consolidated = consolidate_results(&search_results, &clusters);
    
    // Format output...
    Ok(())
}
```

### Daemon / Background Task (src/daemon/mod.rs)
```rust
// Periodically recompute profiles and clusters
pub async fn background_consolidation() {
    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await; // 1 hour
        
        // Recompute OBRL profiles
        for hook_type in &["SessionStart", "PromptSubmit", "Stop", "PreCompact"] {
            let _ = storage.compute_hook_reward_profile(hook_type, "all").await;
        }
        
        // Recompute HMC clusters
        let _ = consolidate_memories(storage, embeddings, 0.85).await;
    }
}
```

---

## 6. Testing Patterns (Codex will evaluate)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_obrl_citation_rate() {
        let storage = create_test_storage();
        
        // Inject 10 memories
        for i in 0..10 {
            storage.record_injection(&format!("mem_{}", i), "SessionStart", i).await.unwrap();
        }
        
        // Mark 7 as cited, 3 as not
        for i in 0..7 {
            storage.record_outcome(&format!("injection_{}", i), true, Some(true)).await.unwrap();
        }
        for i in 7..10 {
            storage.record_outcome(&format!("injection_{}", i), false, Some(false)).await.unwrap();
        }
        
        let profile = storage.compute_hook_reward_profile("SessionStart", "test").await.unwrap();
        assert_eq!(profile.avg_citation_rate, 0.7);  // 7/10
    }
    
    #[test]
    fn test_cfp_question_classification() {
        let q1 = "What's this error: `connection timeout`";
        let errors = vec!["connection timeout".to_string()];
        
        let qtype = QuestionType::classify(q1, &errors, &[]);
        assert!(matches!(qtype, QuestionType::Diagnosis));
        
        let q2 = "How do I fix this?";
        let qtype = QuestionType::classify(q2, &[], &[]);
        assert!(matches!(qtype, QuestionType::Solution));
    }
    
    #[test]
    fn test_hmc_consolidation() {
        // Test agglomerative clustering with 10 memories
        // 5 about "Docker networking", 3 about "Docker mounting", 2 about "Docker builds"
        // Should consolidate to 3 clusters
        let clusters = test_consolidate_memories(0.85);
        assert_eq!(clusters.len(), 3);
    }
    
    #[test]
    fn test_adaptive_decay() {
        // CVE (permanent fact) should NOT decay
        let score = adaptive_decay(1.0, MemoryType::Fact(FactType::Permanent), 180.0, Some(180.0));
        assert!(score > 0.95);  // Barely decayed
        
        // Deprecated solution should decay rapidly
        let score = adaptive_decay(1.0, MemoryType::Solution(SolutionStatus::Deprecated), 45.0, None);
        assert!(score < 0.5);   // Significantly decayed
    }
}
```

---

## Summary: What Codex Should Evaluate

1. **Race Conditions**: OBRL's concurrent writes to `memory_outcomes`
2. **Performance**: HMC's O(n²) clustering on 10k memories
3. **Data Migration**: How to add these tables without breaking existing DBs
4. **Validation**: Do the heuristics (CFP classification, contradiction detection) actually work?
5. **Integration**: Do OBRL + CFP + HMC work smoothly together?
6. **Testing**: Is 273 test coverage sufficient for these new modules?


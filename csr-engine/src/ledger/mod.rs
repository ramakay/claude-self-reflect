//! Pillar 1 — the Derivation Ledger.
//!
//! Memories are priced by *estimated future re-derivation cost*, not historical
//! sunk cost (a fact that cost 18K tokens to discover may cost ~0 to re-derive if
//! the next prompt states it or it's one `git log` away). Injection is a knapsack:
//! under a tight token budget, pick the facts that maximize
//!
//! ```text
//! value = P(needed) × bucket_weight × (1 − inferability)
//! ```
//!
//! per token. Coarse, auditable cost brackets only — no false per-token
//! attribution (per-message `usage` charges whole-context, not marginal cost).
//! This layers on the existing multi-signal predictor (which supplies P(needed)).

/// Coarse, auditable re-derivation cost bracket. Derived from observable counts
/// (turns, tool-result volume, retries), never from fabricated per-fact tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostBucket {
    Cheap,
    Moderate,
    Expensive,
}

impl CostBucket {
    /// Ranking weight — expensive-to-re-derive facts are worth more budget.
    pub fn weight(&self) -> f32 {
        match self {
            CostBucket::Cheap => 0.3,
            CostBucket::Moderate => 0.6,
            CostBucket::Expensive => 1.0,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            CostBucket::Cheap => "cheap",
            CostBucket::Moderate => "moderate",
            CostBucket::Expensive => "expensive",
        }
    }

    pub fn from_str_lossy(s: &str) -> CostBucket {
        match s {
            "expensive" => CostBucket::Expensive,
            "moderate" => CostBucket::Moderate,
            _ => CostBucket::Cheap,
        }
    }
}

/// Classify re-derivation cost from observable session signals. Coarse brackets:
/// a fact discovered across many turns, lots of tool output, or several failed
/// retries is expensive to re-derive; a one-shot answer is cheap.
pub fn classify_cost_bucket(turns: u32, tool_results: u32, retries: u32) -> CostBucket {
    // Weighted effort score; retries dominate (a failed branch is the costliest
    // signal that understanding was hard-won).
    let score = turns + tool_results + retries * 3;
    if retries >= 2 || score >= 12 {
        CostBucket::Expensive
    } else if score >= 4 {
        CostBucket::Moderate
    } else {
        CostBucket::Cheap
    }
}

/// The scope a fact is valid in. Facts never cross scope silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub repo: String,
    pub branch: String,
    pub user: String,
}

/// One ledger entry: a fact priced for future re-derivation.
#[derive(Debug, Clone)]
pub struct LedgerEntry {
    pub id: String,
    pub content: String,
    /// Optional AST/structural anchor name (Pillar 5), for staleness probing.
    pub anchor: Option<String>,
    pub cost_bucket: CostBucket,
    /// 0.0 = cannot be re-derived cheaply; 1.0 = trivially re-derivable from the
    /// working tree or the likely next prompt (so worth ~nothing to inject).
    pub inferability: f32,
    pub confidence: f32,
    pub times_reused: u32,
    pub scope: Scope,
}

/// Estimate token cost of injecting an entry's content (~4 chars/token).
pub fn estimate_tokens(content: &str) -> usize {
    content.len().div_ceil(4)
}

/// Forward-looking injection value per the ledger formula. `p_needed` is the
/// predictor's relevance estimate for this turn, in [0, 1].
pub fn injection_value(entry: &LedgerEntry, p_needed: f32) -> f32 {
    p_needed * entry.cost_bucket.weight() * (1.0 - entry.inferability.clamp(0.0, 1.0))
}

/// A candidate for injection: a ledger entry plus its predictor relevance.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub entry: LedgerEntry,
    pub p_needed: f32,
}

/// Knapsack selection under a token budget. Greedy by value-density
/// (value per token) — the standard, auditable heuristic; deliberately not a
/// false-precision DP. Zero-value entries (fully inferable, or p_needed=0) are
/// never selected. Returns selected entries in descending density order.
pub fn select_for_injection(candidates: Vec<Candidate>, budget_tokens: usize) -> Vec<LedgerEntry> {
    let mut scored: Vec<(f32, usize, LedgerEntry)> = candidates
        .into_iter()
        .filter_map(|c| {
            let value = injection_value(&c.entry, c.p_needed);
            if value <= 0.0 {
                return None;
            }
            let tokens = estimate_tokens(&c.entry.content).max(1);
            let density = value / tokens as f32;
            Some((density, tokens, c.entry))
        })
        .collect();

    // Highest density first; ties broken by lower token cost (fit more).
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });

    let mut spent = 0usize;
    let mut selected = Vec::new();
    for (_density, tokens, entry) in scored {
        if spent + tokens <= budget_tokens {
            spent += tokens;
            selected.push(entry);
        }
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope {
            repo: "csr".into(),
            branch: "main".into(),
            user: "rama".into(),
        }
    }

    fn entry(id: &str, content: &str, bucket: CostBucket, inferability: f32) -> LedgerEntry {
        LedgerEntry {
            id: id.into(),
            content: content.into(),
            anchor: None,
            cost_bucket: bucket,
            inferability,
            confidence: 0.9,
            times_reused: 0,
            scope: scope(),
        }
    }

    #[test]
    fn cost_bucket_brackets() {
        assert_eq!(classify_cost_bucket(1, 0, 0), CostBucket::Cheap);
        assert_eq!(classify_cost_bucket(2, 2, 0), CostBucket::Moderate);
        assert_eq!(classify_cost_bucket(8, 6, 0), CostBucket::Expensive);
        // Retries dominate: even a short session with 2 failed branches is expensive.
        assert_eq!(classify_cost_bucket(1, 0, 2), CostBucket::Expensive);
    }

    #[test]
    fn value_zero_when_fully_inferable() {
        let e = entry("a", "x", CostBucket::Expensive, 1.0);
        assert_eq!(injection_value(&e, 1.0), 0.0);
    }

    #[test]
    fn value_zero_when_not_needed() {
        let e = entry("a", "x", CostBucket::Expensive, 0.0);
        assert_eq!(injection_value(&e, 0.0), 0.0);
    }

    #[test]
    fn expensive_low_inferability_outranks_cheap() {
        let exp = entry("exp", "decision worth keeping", CostBucket::Expensive, 0.1);
        let cheap = entry("cheap", "trivial restatable note", CostBucket::Cheap, 0.8);
        assert!(injection_value(&exp, 0.8) > injection_value(&cheap, 0.8));
    }

    #[test]
    fn knapsack_respects_budget() {
        // Each content ~5 tokens; budget 8 tokens → at most 1 fits.
        let cands = vec![
            Candidate {
                entry: entry("a", "twenty char content!", CostBucket::Expensive, 0.0),
                p_needed: 1.0,
            },
            Candidate {
                entry: entry("b", "twenty char content!", CostBucket::Expensive, 0.0),
                p_needed: 1.0,
            },
        ];
        let sel = select_for_injection(cands, 8);
        assert_eq!(sel.len(), 1);
    }

    #[test]
    fn knapsack_skips_zero_value_even_with_budget() {
        let cands = vec![Candidate {
            entry: entry("inf", "short", CostBucket::Expensive, 1.0), // fully inferable
            p_needed: 1.0,
        }];
        let sel = select_for_injection(cands, 1000);
        assert!(sel.is_empty());
    }

    #[test]
    fn knapsack_prefers_higher_density() {
        // Same tokens; "exp" has higher value → selected first when only one fits.
        let cands = vec![
            Candidate {
                entry: entry("cheap", "same length text!!!!", CostBucket::Cheap, 0.5),
                p_needed: 0.5,
            },
            Candidate {
                entry: entry("exp", "same length text!!!!", CostBucket::Expensive, 0.0),
                p_needed: 1.0,
            },
        ];
        let tokens = estimate_tokens("same length text!!!!");
        let sel = select_for_injection(cands, tokens); // only one fits
        assert_eq!(sel.len(), 1);
        assert_eq!(sel[0].id, "exp");
    }
}

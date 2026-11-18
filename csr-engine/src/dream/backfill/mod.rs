//! Dream backfill pipeline (`.plans/dream-backfill-design.md`).
//!
//! [`pairs`], [`rank`], and [`unfinished`] are the deterministic, zero-LLM
//! half of the pipeline (design §3 Stages 0-3) — everything reachable from
//! them is safe to run under `CSR_NO_DREAMING`
//! (`daemon::dream_cadence::dreaming_disabled`) and never trusts an LLM's
//! output, because there is no LLM in any of them at all.
//!
//! [`adjudicate`] is the ONE stage that spends an LLM call (design §3 Stage
//! 4, bare JSON-constrained, no narrative persona — D11), and [`verify`] is
//! what keeps that call's output honest (design §3 Stage 5): every
//! adjudicated claim is re-checked deterministically — quotes re-matched,
//! OIDs re-resolved, direction re-verified in SQL — before anything is
//! promoted, and a failure discards the candidate rather than trusting it.
//! [`compose`] is Stage 6 (design §3 "compose + drain", amended by §8's
//! D1/D7/D11): it turns verified relations + Queue U into `dreams_v1`
//! `supersession`/`unfinished` rows, capped at N/night and deduplicated by
//! `(project, topic_key)`, and renders the full post-verification ranked
//! queue for `--report`. [`cli`] wires all of the above (plus Stages 0-3)
//! into `csr-engine dream backfill` / `csr-engine dream drain`.

pub mod adjudicate;
pub mod claim_resolution;
pub mod cli;
pub mod compose;
pub mod death_time;
pub mod family;
pub mod funnel;
#[cfg(test)]
mod golden;
pub mod intent_channel;
pub mod pairs;
pub mod rank;
pub mod subagent_citation;
pub mod subagent_iface;
pub mod unfinished;
pub mod verify;

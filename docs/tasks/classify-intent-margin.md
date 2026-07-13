# Watch: classifier has no inter-intent margin

**Status:** watching
**Files:** `csr-engine/src/hooks/intent.rs` (`classify`, `threshold()`)

`classify` is argmax-over-exemplars with per-intent thresholds (Continue 0.60, StateRecall 0.55, Explore 0.55) but **no margin requirement between the top two intents**. Explore and StateRecall share the 0.55 floor, so a prompt scoring 0.56/0.55 flips routes on embedding noise — StateRecall emits the recency pickup while Explore emits the CODE MAP, so adjacent prompts can get different injection shapes.

## What to do

1. Observe real flips first: run with `CSR_DEBUG_CORRELATE=1` and watch for Explore-vs-StateRecall boundary flips on real prompts.
2. Only if flips are observed and harmful: add a margin requirement (e.g. top intent must beat runner-up by 0.03–0.05) with abstain-on-tie falling through to Route B correlation.
3. Calibration is synthetic-only so far — collect real-prompt scores before picking the margin value.

Do NOT add the margin preemptively; both routes inject useful context, so a flip is a quality wobble, not a correctness bug.

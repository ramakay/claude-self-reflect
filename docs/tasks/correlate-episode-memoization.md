# Memoize correlate_episode on Explore-miss path

**Status:** open
**Files:** `csr-engine/src/hooks/prompt_submit.rs` (`correlate_episode`, intent match arm)

When the classifier returns `Explore` but the correlated episode yields no usable CODE MAP (no anchors / no file pointers), control falls through to Route B topic correlation — which calls `correlate_episode` **a second time** with the same inputs. Each call embeds the prompt and scans episodes, so the miss path pays double.

## What to do

Compute the correlation once (`Option<(Episode, String, f32)>`), reuse the result in both the Explore arm and the Route B fallback. Straightforward refactor; add a test asserting single correlation per prompt (e.g. count via a test hook or instrument the query layer).

Low urgency: hook budget is milliseconds and prompt-submit stays under it, but it is wasted work on every Explore-miss.

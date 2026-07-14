# Punch List — 2026-07-13 (post-v9.3.0 side items)

Grok-advisor reviewed. Advisor verdict: items 1–3 today, defer 4–5 to a fresh session.
Overridden for 4–5 by user directive (attempt via grok-implementer with session-model review);
mitigation = pure-refactor scope, full `cargo test`, line-by-line diff review.

| # | Item | Route | Status |
|---|------|-------|--------|
| 1 | Merge dependabot PR #232 (uuid 1.23.5) | direct, CI green | done (merged) |
| 2 | Merge dependabot PR #233 (chrono 0.4.45) | direct, CI green | done (merged) |
| 3 | Security PR: `cargo update -p openssl` (0.10.80, lockfile-only, Linux-target transitive) + docs-site vite 6.4.3 + @babel/core 7.29.7 (devDeps, dev-server/build-time only) | direct | done (PR #235, all 4 alerts cleared) |
| 4 | classify_attempt extraction | grok-implementer + review | done (PR #236, 7 new unit tests, task file removed) |
| 5 | correlate_episode memoization | grok-implementer + review | done (PR #237, task file removed) |

Deferred (unchanged): intent-margin watch, file-anchor denylist (both gated on observed noise), Reddit post (user-gated).

Advisor gaps to note: (a) no cargo-audit merge-blocking policy on main — security items are hygiene, not mandatory; (b) Explore-miss p95 not measured — memoization is efficiency hygiene, not a latency fix.

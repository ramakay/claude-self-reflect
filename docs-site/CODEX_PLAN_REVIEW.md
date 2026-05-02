# Review: GitHub Pages Documentation Site Plan

## 1. Missing Critical Pages/Content That Would Hurt Adoption

The plan covers the obvious launch pages, but it still misses several adoption-critical pages identified in the earlier audit. The biggest omission is a complete CLI reference. The README lists `setup`, `status`, `daemon`, `hook install`, `eval`, `quality`, `--import`, `--enrich`, and `--watch`, but the plan only names the MCP tools reference. Users installing a single binary will immediately need command-level help, flags, expected output, and failure modes.

The plan also omits a configuration reference, uninstall page, FAQ, changelog, and Ralph Loop Memory guide. That last one is not optional: the README gives Ralph Loop Memory its own section and frames CSR as persistent memory for long tasks, but the plan does not mention it. The earlier audit also flags old Python/Docker docs as a high-risk confusion source; the plan says nothing about deprecation banners, redirects, or archiving old docs. Leaving stale install docs alive will directly hurt v8 adoption.

## 2. Information Architecture - Optimal For New Users?

The plan is organized as a production task list, not a user navigation model. Its "10 Theme Areas" are useful for project management, but they do not define the final Starlight sidebar or first-run path. New users need a tight sequence: Why CSR, Install, Verify, First Search, Active Hooks, Troubleshooting. The earlier audit's proposed IA is stronger because it separates Getting Started, Guides, Reference, Architecture, Contributing, About, and Changelog.

The current plan also mixes public docs with release chores: "Fix the 1 failing test" and "Commit SwiftBar hook changes" do not belong in a documentation site plan. That muddles scope on a ship-today effort. Contributor docs should not appear before core user reference content; a new user with a broken install cares more about PATH, MCP registration, hook install state, and `csr-engine status` than PR process.

## 3. Technical Risks With Starlight + GitHub Pages

The plan correctly chooses Starlight and requires local preview plus `npm run build`, but the GitHub Pages risk analysis is too shallow. It does not call out Astro's `site` and `base` configuration, which matter for project pages under `https://ramakay.github.io/claude-self-reflect/`. It also does not specify workflow permissions, build output upload, artifact deployment, or whether Pages deploys from Actions or a branch.

Asset handling is another weak point. The plan says images are "Codex generating in background" and screenshots will be embedded, but it does not define fallbacks if those assets are late, large, missing, or incorrectly pathed under the GitHub Pages base path. For a same-day ship, every image and social card should be treated as optional unless the build verifies it exists. Search also deserves an explicit check because Starlight's local search output can break when base paths are wrong.

## 4. Is The Active Memory Injection Story Told Compellingly Enough?

This is the strongest part of the plan, but it is still not sharp enough. The plan names "6 Hooks deep-dive," "Predictive injection," and "passive search vs active injection," which aligns with the README's hook table and the stated differentiator. However, it needs to show the exact memory loop, not just describe it.

The README gives concrete hook names: SessionStart, SessionEnd, PreCompact, Stop, PostToolUse, and UserPromptSubmit. The docs should make these impossible to miss with a table: trigger, what CSR searches, what gets injected, what user pain it prevents. The plan should also include one full example of an injected context block. "Active intelligence" is only compelling if the reader can see Claude receiving context before it asks for a search.

## 5. SEO, Open Graph, Social Sharing Strategy

The plan's SEO strategy is underdeveloped. "Favicon and Open Graph meta" is not enough. The README has strong search phrases that should become page titles and descriptions: "cross-session memory for Claude Code," "single 44MB binary," "no Docker," "no API keys," "12 MCP tools," and "6 Claude Code hooks." The plan should require unique meta titles/descriptions per core page, canonical URLs, sitemap, robots.txt, and a real OG image at the exact path used in metadata.

Recommendation based on general docs best practice: create shareable landing sections for "Claude Code memory," "MCP memory server," and "Claude Code hooks" because those map to likely discovery queries. The plan should also ensure GitHub, npm, and README links point to the same canonical docs URL.

## 6. Should There Be A Blog Section For Announcements?

Yes, but it should not block today's launch. The plan omits a blog or news section entirely. For v8, a small "News" or "Changelog" section is more valuable than a traditional blog: one v8 launch post, one migration note from Python/Docker/Qdrant to Rust, and future release announcements.

This is grounded in the audit's finding of 57 scattered release notes and no unified changelog. Starlight is docs-first, so a lightweight `/news/` collection or a single changelog page is enough for day one. Full blog infrastructure can wait.

## 7. Risks With The Install Flow Or npm Package The Docs Should Address

The install plan is directionally right: one-command install, platform matrix, setup walkthrough, and troubleshooting decision tree. The missing detail is around sharp install edges already visible in the README. The README says the npm path is alternative and still requires installing the binary separately. The docs must make that explicit or users will assume `npm install -g claude-self-reflect` is sufficient.

The docs also need dedicated troubleshooting for `spawn ENOENT`, missing MCP tools after setup, first-start index rebuild, unsupported Intel Mac, WSL path behavior, optional Anthropic API key for AI narratives, and how to verify hooks were installed. The plan says "from 160 issues," but it does not name the actual decision tree nodes. Same-day docs should prioritize the top failures over broad narrative polish.

## 8. Overall Rating 1-10 And Top 5 Improvements

Rating: 7/10. The plan has the right strategic center: Starlight, GitHub Pages, single-binary install, performance proof, and the 6-hook active memory story. It also covers many of the audit's critical gaps: installation, MCP tools, hooks, architecture, migration, privacy, and contributing. The weaknesses are scope discipline and missing reference depth. A 40-task same-day plan that includes unrelated code/release tasks is too broad, and it leaves out CLI/config/Ralph/changelog/archive work that the README and audit show users will need.

1. **P0 - Replace the task themes with a final sidebar IA.** Define exact pages and order: Why CSR, Installation, Verify, Quick Start, Hooks, Search, MCP Tools, CLI, Configuration, Troubleshooting, Migration.
2. **P0 - Add CLI and configuration references.** Document every README-listed command, key flags, expected output, env vars, hook config, and common failure states.
3. **P0 - Make active injection concrete.** Add a six-hook table plus one real UserPromptSubmit injection example and one Stop/stuck-detection example.
4. **P1 - Fix install-flow ambiguity.** Explain curl install vs npm alternative, binary availability, PATH, Intel Mac unsupported status, WSL, restart requirements, and hook verification.
5. **P1 - Add launch hygiene.** Add old-doc deprecation/redirect plan, changelog or news page, per-page SEO metadata, verified OG image, and GitHub Pages base-path checks.

---
active: true
iteration: 3
max_iterations: 5
completion_promise: "Release completed"
started_at: "2026-01-04T20:20:34Z"
---

Prepare for release, to do this check all sub-agents and their prompts do they satisfy all ralph related checks including runaway loops, run all subagents that test the system, then ensure that coderabbit and other claude ci/cd checks are run sleep and fix them, then prepare for a gh release - review the github ci.yml it should publish to npm automatically since i just now setup trusted publishing but make sure subagent understands this and can prove a npm release occurred.

import json, uuid, datetime, os

OUT_DIR = os.path.expandvars("$SCRATCH/e3/c5_transcripts")

QUERIES = [
    ("Q5", "why does import skip conversations that start with CSR agent prompts"),
    ("Q9", "why do hooks use catch-all wrappers so they never block claude code"),
    ("Q12", "why was fts5 keyword fallback added when semantic scores are low"),
    ("A3", "why does the command center cache campaign data in a snapshot instead of calling the APIs live on page load"),
    ("A5", "why was score save instrumented with observability across multiple app runtime versions"),
    ("A6", "why does the whats running section exist on the command center and what does it monitor"),
    ("A7", "why was the radio reel video built as a remotion composition with a root of multiple scenes"),
    ("A8", "why does the lessons page pull lesson analytics from posthog instead of supabase"),
]

# 5 answer templates (varied wording per cycle), 2-3 sentences each, clearly
# marked synthetic (E3 contamination probe re-ask cycles).
ANSWER_TEMPLATES = {
    "Q5": [
        "[synthetic re-ask cycle {n}] Import skips those conversations because they're CSR's own agent-subprocess transcripts (session-briefing analyst, compaction summarizer) — their first user message IS the agent prompt itself. Without the skip, CSR would import its own reports and feed them back into the next briefing, poisoning the corpus with self-talk. This is a synthetic answer generated for the E3 contamination experiment.",
        "[synthetic re-ask cycle {n}] The skip exists so CSR never indexes its own generated prompts as if a real user wrote them. If the first user turn matches a known agent-prompt pattern, the whole file is dropped from import. Synthetic content for E3 corpus-contamination testing.",
        "[synthetic re-ask cycle {n}] Because those transcripts are CSR talking to itself (briefing/compaction subagents), and importing them would create a feedback loop where synthetic reports get treated as organic user history. This answer is synthetic, generated for the E3 experiment.",
        "[synthetic re-ask cycle {n}] Agent-subprocess sessions open with CSR's own prompt text, not real user prose — importing them would contaminate search with the tool's own scaffolding. Flagged here as synthetic E3 test content.",
        "[synthetic re-ask cycle {n}] The importer checks the first user message against known CSR agent-prompt strings and skips the whole file if it matches, preventing self-referential contamination. Synthetic answer for E3 re-ask cycle testing.",
    ],
    "Q9": [
        "[synthetic re-ask cycle {n}] Hooks use catch-all wrappers so any internal error in CSR's hook logic is swallowed rather than surfaced as a failure to Claude Code — a broken hook must never block the user's actual session. This is synthetic content generated for the E3 contamination probe.",
        "[synthetic re-ask cycle {n}] Because hooks are a side channel: if CSR's own code throws, Claude Code should keep working normally rather than freeze or error out. Synthetic E3 test answer.",
        "[synthetic re-ask cycle {n}] The wrappers exist purely for fault isolation — CSR is a memory add-on, not core functionality, so any hook exception gets caught and logged instead of propagating. Synthetic content, E3 experiment.",
        "[synthetic re-ask cycle {n}] All 6 hooks wrap their bodies in catch-all error handling so a bug in CSR never becomes a bug in the user's coding session. Marked synthetic for E3 re-ask testing.",
        "[synthetic re-ask cycle {n}] Catch-all wrappers guarantee hook failures degrade silently rather than blocking Claude Code's normal flow, since memory features are best-effort. Synthetic answer, E3 contamination cycle.",
    ],
    "Q12": [
        "[synthetic re-ask cycle {n}] FTS5 keyword fallback was added because semantic-only search sometimes misses exact-term matches when embedding similarity is weak (rare identifiers, error codes) — a keyword hit at low semantic score still surfaces the right chunk. Synthetic content for the E3 contamination experiment.",
        "[synthetic re-ask cycle {n}] When semantic scores drop below the confidence threshold, FTS5 keyword search kicks in as a safety net so literal-term queries still find their target. Synthetic E3 test answer.",
        "[synthetic re-ask cycle {n}] Pure vector search can miss exact identifier or error-string matches; fts5 fallback catches those cases when semantic scores are too low to trust alone. This is synthetic content generated for E3.",
        "[synthetic re-ask cycle {n}] The fallback exists to cover low-similarity edge cases where keyword overlap is a stronger signal than the embedding distance. Synthetic answer for the E3 re-ask cycle test.",
        "[synthetic re-ask cycle {n}] Semantic search alone under-performs on rare exact terms, so fts5 keyword matching backstops it whenever the top semantic score falls below the min threshold. Synthetic E3 experiment content.",
    ],
    "A3": [
        "[synthetic re-ask cycle {n}] The command center caches campaign data in a snapshot instead of live API calls to avoid hammering rate-limited ad-platform APIs on every page load and to keep the dashboard fast. This is synthetic content generated for the E3 contamination experiment.",
        "[synthetic re-ask cycle {n}] Snapshotting avoids live API latency and rate limits on page load, trading a small staleness window for a responsive dashboard. Synthetic E3 test answer.",
        "[synthetic re-ask cycle {n}] Live calls on every load would be slow and quota-expensive, so campaign data is pre-fetched into a snapshot the command center reads from instead. Synthetic content for E3.",
        "[synthetic re-ask cycle {n}] The snapshot approach decouples dashboard responsiveness from third-party API rate limits and latency spikes. Marked synthetic for E3 re-ask testing.",
        "[synthetic re-ask cycle {n}] Caching a snapshot means the command center loads instantly regardless of upstream API health, refreshing on a schedule rather than per-request. Synthetic answer, E3 experiment.",
    ],
    "A5": [
        "[synthetic re-ask cycle {n}] Score save was instrumented with observability across app runtime versions because silent save failures were hard to diagnose without per-version telemetry when users were on mixed app builds. This is synthetic content for the E3 contamination experiment.",
        "[synthetic re-ask cycle {n}] Cross-version instrumentation lets the team see which app runtime a failed save came from, since bugs sometimes only hit older builds. Synthetic E3 test answer.",
        "[synthetic re-ask cycle {n}] Without per-version observability, score-save regressions in one app runtime were invisible until users complained; instrumentation fixed that blind spot. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] The instrumentation tracks save success/failure tagged by runtime version so version-specific bugs surface in dashboards immediately. Synthetic answer for E3 re-ask cycle.",
        "[synthetic re-ask cycle {n}] Observability was added because score saves failed differently across runtime versions and there was no way to tell which cohort was affected. Synthetic E3 experiment content.",
    ],
    "A6": [
        "[synthetic re-ask cycle {n}] The whats running section exists on the command center to give an at-a-glance view of active campaigns and jobs so operators know what's currently live without digging through logs. This is synthetic content for the E3 contamination experiment.",
        "[synthetic re-ask cycle {n}] It monitors currently active campaigns/processes in real time, surfacing them in one place for operator visibility. Synthetic E3 test answer.",
        "[synthetic re-ask cycle {n}] The section exists so operators can see live campaign/job status without needing to check individual platform dashboards. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] It's a live status panel tracking what's currently executing, added so nothing runs silently in the background unnoticed. Synthetic answer for E3 re-ask testing.",
        "[synthetic re-ask cycle {n}] Whats running gives a consolidated real-time view of active work so the team can catch stuck or runaway jobs early. Synthetic E3 experiment content.",
    ],
    "A7": [
        "[synthetic re-ask cycle {n}] The radio reel video was built as a Remotion composition with a multi-scene root so individual scenes could be edited, reordered, and previewed independently while still rendering as one video. This is synthetic content for the E3 contamination experiment.",
        "[synthetic re-ask cycle {n}] A root composition of multiple scenes let the team compose, preview, and swap segments without re-rendering the whole reel each time. Synthetic E3 test answer.",
        "[synthetic re-ask cycle {n}] Remotion's scene-based root made the video modular — each scene is its own component, composed together at render time. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Splitting into scenes under one root composition kept the video maintainable and let designers iterate on one scene without touching others. Synthetic answer for E3 re-ask cycle.",
        "[synthetic re-ask cycle {n}] The multi-scene root architecture was chosen so the radio reel could be assembled from reusable, independently testable Remotion scene components. Synthetic E3 experiment content.",
    ],
    "A8": [
        "[synthetic re-ask cycle {n}] The lessons page pulls analytics from PostHog instead of Supabase because PostHog already captures the event-level interaction data needed for analytics, while Supabase just holds the lesson records themselves. This is synthetic content for the E3 contamination experiment.",
        "[synthetic re-ask cycle {n}] PostHog is the system of record for behavioral events, so lesson analytics reads from there rather than duplicating event tracking into Supabase. Synthetic E3 test answer.",
        "[synthetic re-ask cycle {n}] Supabase stores lesson content/state; PostHog captures usage events — analytics naturally comes from the latter. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Splitting concerns this way avoided building custom event aggregation in Supabase when PostHog already does it well. Synthetic answer for E3 re-ask testing.",
        "[synthetic re-ask cycle {n}] The lessons page reads analytics from PostHog because it's purpose-built for event analytics, whereas Supabase is the app's relational data store. Synthetic E3 experiment content.",
    ],
}

os.makedirs(OUT_DIR, exist_ok=True)

for n in range(1, 6):
    session_id = f"c5cycle{n}-e3000000-0000-4000-8000-00000000000{n}"
    filename = f"{session_id}.jsonl"
    path = os.path.join(OUT_DIR, filename)
    base_day = 11 + n  # cycle N -> 2026-06-1{1+N}
    base_ts = datetime.datetime(2026, 6, base_day, 10, 0, 0, tzinfo=datetime.timezone.utc)
    lines = []
    parent_uuid = None
    t_offset = 0
    for qid, qtext in QUERIES:
        # user turn
        user_uuid = str(uuid.uuid4())
        ts = (base_ts + datetime.timedelta(seconds=t_offset)).strftime("%Y-%m-%dT%H:%M:%SZ")
        t_offset += 30
        user_line = {
            "parentUuid": parent_uuid,
            "isSidechain": False,
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": qtext}],
            },
            "uuid": user_uuid,
            "timestamp": ts,
            "sessionId": session_id,
        }
        lines.append(user_line)
        parent_uuid = user_uuid

        # assistant turn
        asst_uuid = str(uuid.uuid4())
        ts2 = (base_ts + datetime.timedelta(seconds=t_offset)).strftime("%Y-%m-%dT%H:%M:%SZ")
        t_offset += 30
        answer = ANSWER_TEMPLATES[qid][n - 1].format(n=n)
        asst_line = {
            "parentUuid": parent_uuid,
            "isSidechain": False,
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": answer}],
            },
            "uuid": asst_uuid,
            "timestamp": ts2,
            "sessionId": session_id,
        }
        lines.append(asst_line)
        parent_uuid = asst_uuid

    with open(path, "w") as f:
        for line in lines:
            f.write(json.dumps(line) + "\n")
    print(f"wrote {path} ({len(lines)} lines)")

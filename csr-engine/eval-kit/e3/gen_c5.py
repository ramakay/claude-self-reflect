import json, uuid, datetime, os

OUT_DIR = os.path.expandvars("$SCRATCH/e3/c5_transcripts")

QUERIES = [
    ("Q5", "why does import skip conversations that start with CSR agent prompts"),
    ("Q9", "why do hooks use catch-all wrappers so they never block claude code"),
    ("Q12", "why was fts5 keyword fallback added when semantic scores are low"),
    ("A3", "why does the example dashboard cache report data in a snapshot instead of calling the APIs live on page load"),
    ("A5", "why was the save action instrumented with observability across multiple client versions"),
    ("A6", "why does the status section exist on the example dashboard and what does it monitor"),
    ("A7", "why was the promo video built as a composition with a root of multiple scenes"),
    ("A8", "why does the progress page pull its analytics from one backend instead of another"),
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
        "[synthetic re-ask cycle {n}] Placeholder answer 1 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 2 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 3 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 4 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 5 for a withheld example query. Synthetic content, E3.",
    ],
    "A5": [
        "[synthetic re-ask cycle {n}] Placeholder answer 1 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 2 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 3 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 4 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 5 for a withheld example query. Synthetic content, E3.",
    ],
    "A6": [
        "[synthetic re-ask cycle {n}] Placeholder answer 1 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 2 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 3 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 4 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 5 for a withheld example query. Synthetic content, E3.",
    ],
    "A7": [
        "[synthetic re-ask cycle {n}] Placeholder answer 1 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 2 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 3 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 4 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 5 for a withheld example query. Synthetic content, E3.",
    ],
    "A8": [
        "[synthetic re-ask cycle {n}] Placeholder answer 1 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 2 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 3 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 4 for a withheld example query. Synthetic content, E3.",
        "[synthetic re-ask cycle {n}] Placeholder answer 5 for a withheld example query. Synthetic content, E3.",
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

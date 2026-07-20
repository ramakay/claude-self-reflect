# E2 Dialog-Act Extraction Protocol (frozen 2026-07-17, before any grading)

You are extracting OBSERVABLE dialog-acts from conversation digests. You are NOT grading,
NOT judging relevance, NOT ranking. Extractive only: every claim must carry a verbatim quote
from the digest. If no quote exists, the act is absent. Do not infer beyond the text.

Input: digest file `digests/<QID>.md` — header gives the provenance question ("query") and
TARGET file. Each `## CONV <id>` section has: source marker (MAIN session / SIDECHAIN /
db-chunks), timestamp span, EDITS TOUCHING TARGET (or TARGET-FILE LINK line), and OPERATOR
TURNS (for db-chunks sources: mixed-role text — attribute to operator only what clearly
reads as a human instruction/reaction, not assistant prose).

Per conversation, report:

- directs: an operator turn that DIRECTS the work the query asks about — an instruction,
  requirement, or problem report that initiates/steers it (e.g., "fix X", "why is Y slow,
  handle it", "use Z instead"). Quote it. Generic prompts ("continue", "what next") never
  count. present=false if none.
- accepts: an operator turn AFTER work that ratifies it ("ship it", "commit", "works now",
  "looks good", approval to release). Quote it. present=false if none. (Implicit acceptance
  is NOT yours to infer — grading handles that from external ledgers.)
- rejects: operator turn rejecting/reverting the work ("no", "that's wrong", "revert").
  Quote it.
- discusses: conversation talks ABOUT the query's subject (explains, documents, retrospects)
  without directing it. true/false + one short quote if true.
- reasks: an operator turn that essentially RESTATES the query itself as a question
  (especially dated 2026-07-15 or later = evaluation echo). Quote it.
- edits_target: true iff digest shows "EDITS TOUCHING TARGET" entries or a "TARGET-FILE
  LINK: code-graph" line.
- off_topic: true if the conversation shows no connection to the query subject at all.

Output STRICT JSON (no markdown fences, no commentary):
{"qid": "<QID>", "items": [
  {"conv_id": "...", "directs": {"present": true, "quote": "..."},
   "accepts": {"present": false}, "rejects": {"present": false},
   "discusses": true, "discusses_quote": "...",
   "reasks": {"present": false}, "edits_target": true, "off_topic": false,
   "notes": "<=120 chars"}]}

Rules: quotes must be verbatim substrings of the digest (truncation allowed, ≥8 words when
available). One entry per CONV section, all sections covered. Timestamps in quotes not
required. Work alone — do not consult other extraction outputs, retrieval tools, or CSR MCP
tools. Do not use any semantic/embedding search anywhere.

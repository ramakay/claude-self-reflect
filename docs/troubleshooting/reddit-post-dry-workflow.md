# How I stay DRY with Claude Code (and why I index conversations, not code)

Been following the RAG/indexing debates here. Cline's post about not indexing codebases? Spot on. Code indexing mangles context, goes stale instantly, security nightmare. I'm doing something different though - indexing conversations about code.

Last Tuesday, spent 45 minutes with Claude on a Qdrant dimension mismatch. Tried everything, finally realized I'd mixed embedding models. Monday, different project, same error. Started explaining it all over again when it hit me - we solved this literally last week. I'm violating DRY in my conversations.

Code changes constantly. Yesterday's solution is tomorrow's tech debt. The knowledge of how you fixed something though? That stays valid. Adding network_mode: bridge to fix Docker networking remains the fix even after your repo evolves. Code indexing asks "where is this?" Conversation search asks "how did we fix this?" One rots, the other accumulates.

After using this daily for months, the workflow is simple: hit familiar error, search past chats, find solution ("oh right, 384 dimensions"), apply, move on. No re-explaining, no re-debugging.

Not another RAG tool. It only searches what you've discussed - no hallucinating similar code from random repos. Default is local with FastEmbed (your conversations stay on your machine), but you can use Voyage AI if you want better accuracy and don't mind the cloud. Built-in time decay (90-day half-life) keeps recent solutions on top.

My setup now: Claude reads files on demand for current code (Cline-style), search past conversations for solved problems, /enhance for complex planning. Different tools, different jobs.

Real numbers: 11k conversation chunks (wasn't clearing conversations earlier, oops), ~80ms searches on M3, finds the right solution 12/15 times, saves ~10 minutes per repeated issue. Whole index is 47MB locally.

Best for environment configs, build problems, API quirks, regex patterns. Useless for current code structure or new problems.

We obsess over DRY in code then happily re-explain the same bug to Claude. That's a workflow bug. Maybe you don't need a tool - bookmark chats, keep notes, whatever. Just stop re-solving solved problems.

Setup's npm install now (or Docker if you hit issues - some folks had install problems). Also added a streaming importer for large conversation histories. I update this thing constantly based on feedback, so hit me with your edge cases.

What are you sick of re-explaining to Claude?

---

Projects mentioned:
- github.com/ramakay/claude-self-reflect (conversation search)
- github.com/ramakay/claude-organize (/enhance command)
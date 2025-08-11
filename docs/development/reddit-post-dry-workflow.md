# How I stay DRY with Claude Code (and why I index conversations, not code)

**TL;DR**
- Conversation search and metadata extraction probably a better bet than code indexing. Concepts, file search but with metadata included is more powerful in practice.
- https://www.github.com/ramakay/claude-self-reflect (conversation search)
- https://www.github.com/ramakay/claude-organize (/enhance command)

Been following the RAG/indexing debates here including one probably on the same page about Claude code context (Great project, probably on the same page). Cline's post about not indexing codebases was very interesting.

I agree, Code indexing mangles context, goes stale instantly, security nightmare. I'm doing something different though - indexing conversations about code because I could not get claude.md, memory-banks, start-here.md, shouting matches etc. to make the claude lifecycle respect conventions or get previous solutions started.

It started when I was working on a different project with Claude on a Qdrant dimension mismatch. Tried everything, finally realized I'd mixed embedding models. Monday, different project, same error. Started explaining it all over again when it hit me - we solved this literally last week. I'm violating DRY in my conversations.

Code changes constantly. Yesterday's solution is tomorrow's tech debt. The knowledge of how you fixed something though? That stays valid.

**My workflow**

New Convo: nothing special the streaming importer is working on it.

Similar concept that I have worked on before, spline 3d or websockets or docker imports:
- shift+p
- /enhance I want to <my focus task>, check claude self reflect for previous learnings and example files and get a consensus from GPT-5 (optional: project: all if its not from current project)
- Result: takes a few seconds but pieces together a self researched, triangulated approach ahead with todos.

Not another RAG tool. It only searches what you've discussed - no hallucinating similar code from random repos. Default is local with FastEmbed (your conversations stay on your machine), but you can use Voyage AI if you want better accuracy and don't mind the cloud. Built-in time decay (90-day half-life, not complex - I could not make Qdrant functions work) keeps recent solutions on top.

Looking at the other thread, people asked about benchmarks, large codebases, and comparisons to Serena/Cursor. I am not indexing large codebases like they do - we index conversation input, tool input, output, files. While tools like Claude Context handle "where is this code?", we handle "how did we fix this before?" For large projects, we search 11k conversation chunks in ~80ms, finding past solutions 12/15 times. It's complementary - they do code discovery, I do solution recall. The DRY principle isn't just for code, it's for not re-explaining the same bug to Claude every week.

Best for environment configs, build problems, API quirks, regex patterns. Useless for current code structure or new problems. We obsess over DRY in code then happily re-explain the same bug to Claude. That's a workflow bug. Maybe you don't need a tool - bookmark chats, keep notes, whatever. Just stop re-solving solved problems. I update this thing constantly based on feedback, so hit me with your edge cases.

I appreciate what the community is building and the real feedback you all provide.
# Stuck Detection Prompt (for Claude Code Stop Hook)

Check if the Ralph session is stuck by examining `.ralph_state.md`:

1. **Read** `.ralph_state.md` if it exists
2. **Check** the `blocking_errors` section
3. **If** the same error appears 3+ times:
   - Use `reflect_on_past("error: {the blocking error}")` to search CSR
   - If solutions found, write them to `.ralph_memories.md`
   - Continue with the suggested solution
4. **If** iteration count > 10 without progress:
   - Use `reflect_on_past("{current task} solutions")` for broader search
   - Consider alternative approaches from past sessions
5. **Update** `.ralph_state.md` with any new insights

Remember: The goal is to break out of stuck loops by leveraging past experience.

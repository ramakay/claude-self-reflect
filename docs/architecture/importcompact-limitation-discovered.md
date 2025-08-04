# ImportCompact Auto Mode Limitation

## Critical Discovery

The sub-agent approach for automating importcompact **does not work** due to a fundamental misunderstanding of how sub-agents operate in Claude Code.

## The Problem

When a sub-agent executes `/compact`, it compacts **its own conversation context**, not the parent/main conversation. This means:

1. Main conversation runs `/importcompact --auto`
2. Import completes successfully 
3. Sub-agent is triggered
4. Sub-agent runs `/compact` - but this compacts the sub-agent's context, not the main conversation
5. Main conversation remains uncompacted

## Why This Happens

- Sub-agents have their own isolated conversation context
- Slash commands executed by sub-agents affect only their context
- There is no mechanism for sub-agents to execute commands in the parent context
- This is by design for isolation and security

## Implications

1. **Auto mode cannot work**: The fundamental premise of using sub-agents to automate the compact step is flawed
2. **Manual mode remains**: The two-step process is the only viable approach
3. **No workaround exists**: This is an architectural limitation, not a implementation bug

## What This Means for Users

Users must continue to use the two-step process:
1. Run `/importcompact [focus]`
2. Manually run `/compact [focus]`

The convenience of one-step automation is not possible with current Claude Code architecture.

## Lessons Learned

- Sub-agents are isolated execution environments
- Commands in sub-agents don't affect parent context
- Always verify architectural assumptions before implementation
- The 4-second overhead benchmark was meaningless since the approach doesn't work

## Recommendation

Remove the --auto flag and import-orchestrator agent as they provide no value and will confuse users.
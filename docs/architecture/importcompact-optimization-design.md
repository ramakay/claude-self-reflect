# ImportCompact Optimization Design

## Executive Summary

The current importcompact workflow requires two manual steps due to Claude Code's limitation that slash commands cannot chain other slash commands. This document proposes a hybrid solution that balances automation convenience with performance.

## Current State Analysis

### Performance Metrics
- **Direct import time**: 3-45 seconds (depending on conversation count)
- **Sub-agent overhead**: ~4 seconds (2.5s startup + 1.5s communication)
- **Total overhead**: 126% increase in execution time
- **User friction**: Manual second step required

### Architecture Constraints
1. Slash commands cannot invoke other slash commands
2. Sub-agents can orchestrate but add latency
3. MCP tools have minimal overhead but cannot trigger slash commands
4. Background jobs would require additional infrastructure

## Proposed Solution: Hybrid Approach

### Design Principles
1. **Default to speed**: Keep current two-step as default
2. **Opt-in automation**: Add --auto flag for convenience
3. **Clear feedback**: Show progress and next steps
4. **Graceful degradation**: Fall back to manual if automation fails

### Implementation Strategy

#### Option 1: Enhanced ImportCompact Command (Recommended)
```bash
#!/bin/bash
# /importcompact command with --auto flag

FOCUS_INSTRUCTIONS="$@"
AUTO_MODE=false

# Parse arguments
if [[ "$1" == "--auto" ]]; then
    AUTO_MODE=true
    shift
    FOCUS_INSTRUCTIONS="$@"
fi

# Run import
python3 ~/projects/claude-self-reflect/scripts/import-immediate.py

if [ $? -eq 0 ]; then
    if [ "$AUTO_MODE" = true ]; then
        echo "🤖 AUTO MODE: Triggering import-orchestrator sub-agent..."
        echo "TRIGGER_SUBAGENT: import-orchestrator --compact \"$FOCUS_INSTRUCTIONS\""
    else
        echo "📦 READY TO COMPACT"
        echo "Run: /compact $FOCUS_INSTRUCTIONS"
    fi
fi
```

#### Option 2: New Orchestrator Sub-Agent
Create a specialized `import-orchestrator` sub-agent that:
1. Monitors import completion
2. Automatically triggers compact workflow
3. Provides real-time progress updates
4. Handles errors gracefully

### Performance Trade-offs

| Approach | Time | User Actions | Pros | Cons |
|----------|------|--------------|------|------|
| Current Two-Step | 3-45s | 2 commands | Fastest | Manual intervention |
| Sub-Agent Auto | 7-49s | 1 command | Fully automated | 4s overhead |
| Hybrid Default | 3-45s | 2 commands | Fast by default | Still manual |
| Hybrid --auto | 7-49s | 1 command | User choice | Slightly complex |

## Implementation Plan

### Phase 1: Enhance ImportCompact Script
1. Add argument parsing for --auto flag
2. Implement sub-agent trigger mechanism
3. Improve progress feedback
4. Add help documentation

### Phase 2: Create Import Orchestrator Agent
```yaml
name: import-orchestrator
description: Orchestrates import and compact workflow seamlessly
tools: [Read, Bash, Task]
capabilities:
  - Monitor import progress
  - Trigger compact with preserved context
  - Handle errors and retries
  - Provide detailed feedback
```

### Phase 3: User Experience Improvements
1. Clear documentation on when to use --auto
2. Progress indicators during automation
3. Keyboard interrupt handling
4. Success/failure notifications

## Risk Mitigation

### Performance Risks
- **Mitigation**: Make automation opt-in, not default
- **Monitoring**: Log timing metrics for analysis

### Reliability Risks
- **Mitigation**: Implement timeout handling
- **Fallback**: Always show manual command as backup

### User Experience Risks
- **Mitigation**: Clear documentation and examples
- **Feedback**: Real-time progress updates

## Recommendations

1. **Implement Hybrid Approach**: Provides best of both worlds
2. **Start with Script Enhancement**: Lower complexity, immediate value
3. **Add Sub-Agent Later**: Based on user feedback and usage patterns
4. **Monitor Usage**: Track --auto flag adoption to guide future decisions

## Success Metrics

- **Adoption Rate**: % of users using --auto flag
- **Time Saved**: Average reduction in workflow completion
- **Error Rate**: % of failed automated workflows
- **User Satisfaction**: Feedback on convenience vs speed

## Conclusion

The hybrid approach offers the best balance:
- **Power users** get maximum speed with manual control
- **Convenience seekers** get one-command automation
- **Everyone** gets clear feedback and fallback options

The 4-second overhead is acceptable for the convenience gained, especially for longer import operations where the relative overhead becomes negligible.
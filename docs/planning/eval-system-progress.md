# Claude Self-Reflect Evaluation System Progress

**Date**: 2025-09-08  
**Status**: Architecture Design Phase  
**Timeline**: 1-week implementation target

## Executive Summary

We're integrating a simple, self-learning evaluation system into claude-self-reflect that discovers code patterns from conversations and learns which patterns correlate with successful vs problematic sessions. This avoids the complexity of external evaluation frameworks while leveraging our existing semantic memory infrastructure.

## Evolution of Design

### Initial Proposal (Over-engineered)
- Separate MCP tool for evaluation
- Microservices architecture with event buses
- Complex scoring with multiple evaluation engines
- **Verdict**: 10-100x more complex than needed

### Current Design (Consensus-driven)
After review by GPT-5 and Opus 4.1, we've converged on:
- **Integration**: Built into claude-self-reflect, not separate
- **Scoring**: Simple frequency counting, no Bayesian statistics initially
- **Signals**: Conversation-based labeling only (no external metrics)
- **Scope**: Project-specific patterns first, global catalog later

## Technical Architecture

### Pattern Discovery System

```python
# Core concept - simplified from complex Bayesian approach
class PatternLearner:
    def __init__(self):
        self.patterns = {}  # pattern_key -> {good: 0, bad: 0, neutral: 0}
    
    def canonicalize_pattern(self, ast_pattern):
        """Replace variables with placeholders: $VAR, $NUM, $STR"""
        # Example: useState(loading) -> useState($VAR)
        return canonicalized
    
    def label_session(self, conversation):
        """Use conversation signals to label quality"""
        last_messages = conversation[-5:]  # Last 5 messages
        
        if any(signal in msg for signal in ["error", "failed", "broken"]):
            return "bad"
        elif any(signal in msg for signal in ["thanks", "perfect", "works"]):
            return "good"
        else:
            return "neutral"
    
    def update_pattern_stats(self, patterns, label):
        for pattern in patterns:
            key = self.canonicalize_pattern(pattern)
            if key not in self.patterns:
                self.patterns[key] = {"good": 0, "bad": 0, "neutral": 0}
            self.patterns[key][label] += 1
```

### Integration Points

1. **During Import** (`scripts/importer/processors/ast_extractor.py`)
   - Already extracting AST patterns
   - Add: Pattern canonicalization
   - Add: Store patterns in Qdrant metadata

2. **Post-Import Analysis** (new background process)
   - Label conversations based on signals
   - Update pattern statistics
   - Generate pattern quality scores

3. **MCP Tool Interface**
   ```python
   @server.tool()
   async def analyze_code_patterns(code: str, project: str = None):
       """Analyze code for known good/bad patterns"""
       patterns = extract_patterns(code)
       scores = lookup_pattern_scores(patterns, project)
       return format_evaluation_report(scores)
   ```

## Implementation Phases

### Week 1: MVP Implementation

**Day 1-2: Pattern Extraction Enhancement**
- Extend `ast_extractor.py` to canonicalize patterns
- Store canonicalized patterns in Qdrant metadata
- Test with existing imported conversations

**Day 3-4: Session Labeling**
- Implement conversation signal detection
- Create labeling heuristics (error/success signals)
- Backfill labels for existing conversations

**Day 5-6: Pattern Scoring**
- Count pattern frequencies by label
- Implement Laplace smoothing for small samples
- Create pattern quality scores (good/total ratio)

**Day 7: MCP Tool Integration**
- Add `analyze_code_patterns` tool to MCP server
- Create evaluation report formatting
- Test end-to-end with Claude

### Future Enhancements (Post-MVP)

1. **Enhanced Signals** (Month 2)
   - Git commit success/revert signals
   - Test pass/fail correlation
   - Performance metrics integration

2. **Global Catalog** (Month 3)
   - Cross-project pattern aggregation
   - Community pattern sharing
   - Pattern evolution tracking

3. **Advanced Scoring** (Month 6)
   - Bayesian confidence intervals
   - Context-aware pattern scoring
   - Pattern interaction effects

## Key Decisions & Rationale

### Why Simple Frequency Counting?
- **Complexity**: Bayesian statistics add unnecessary complexity initially
- **Data Volume**: Need significant data before advanced stats help
- **Interpretability**: Simple ratios are easier to understand/debug

### Why Conversation Signals Only?
- **Availability**: Every conversation has these signals
- **Immediacy**: No external dependencies or delays
- **Correlation**: Strong correlation with actual session quality

### Why Project-Specific First?
- **Relevance**: Patterns vary significantly by project type
- **Privacy**: No cross-project data sharing initially
- **Simplicity**: Easier to implement and validate

## Success Metrics

1. **Pattern Discovery**: Find 100+ unique canonicalized patterns
2. **Signal Quality**: 80%+ accuracy in session labeling (manual validation)
3. **Performance**: <100ms pattern analysis for typical code file
4. **Adoption**: Active use in 50+ projects within first month

## Risk Mitigation

| Risk | Mitigation |
|------|------------|
| False positive patterns | Minimum support threshold (5+ occurrences) |
| Noisy conversation signals | Multiple signal types, weighted scoring |
| Pattern explosion | Canonicalization, pattern clustering |
| Performance degradation | Async processing, caching layer |

## Implementation Notes

### Current State
- AST extraction exists but doesn't evaluate
- Patterns extracted: function/class names only
- No pattern canonicalization yet
- No quality scoring system

### Required Changes
1. Enhance AST extraction for full patterns
2. Add canonicalization logic
3. Implement session labeling
4. Create scoring system
5. Build MCP tool interface

### Dependencies
- Existing: ast_extractor.py, Qdrant storage
- New: Pattern canonicalization library
- Optional: AST-grep for advanced patterns

## Consensus Points (GPT-5 & Opus 4.1)

Both models agreed on:
- ✅ Start with simple frequency counting
- ✅ Use conversation signals only initially
- ✅ Skip Bayesian statistics in v1
- ✅ Focus on project-specific patterns first
- ✅ 1-week implementation is realistic
- ✅ Pattern canonicalization is essential

## Next Steps

1. [ ] Implement pattern canonicalization function
2. [ ] Extend ast_extractor.py for pattern storage
3. [ ] Create session labeling logic
4. [ ] Build pattern scoring system
5. [ ] Add MCP tool for pattern analysis
6. [ ] Test with real conversation data
7. [ ] Document usage and examples

## References

- AST-grep catalog: https://ast-grep.github.io/catalog/
- Current AST extractor: `scripts/importer/processors/ast_extractor.py`
- MCP server location: `mcp-server/src/`
- Qdrant collections: Project-specific with metadata storage
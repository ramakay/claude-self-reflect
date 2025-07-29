# v2.4.6 - Documentation & Context Clarity

## 📚 Documentation Improvements

This patch release focuses on improving documentation clarity and user understanding.

### Key Changes

#### README Cleanup
- **Condensed version details**: Moved lengthy version-specific information to dedicated documentation
- **Cleaner structure**: Focused on what users need to know immediately
- **Better organization**: Version history now in `docs/release-history.md`

#### Context Preservation Benefits Explained
The README now clearly explains why using the reflection-specialist agent is recommended:
- **Preserves main conversation context** - Search results don't clutter your working memory
- **Clean markdown output** - No XML dumps in your chat history
- **Isolated processing** - Multiple searches won't exhaust your context window

#### Performance Clarification
Added clarity about the 24-30s "overhead" when using sub-agents:
- This time includes context preservation
- Actual search performance remains 200-350ms
- The overhead provides value by keeping your main conversation clean

### What's Included
- Improved README.md with cleaner structure
- New `docs/release-history.md` with full version details
- Release notes for v2.4.5 preserved in documentation
- Updated performance guide explaining context benefits

### No Breaking Changes
This is a documentation-only release. All functionality remains the same.

---

**Full Changelog**: https://github.com/ramakay/claude-self-reflect/compare/v2.4.5...v2.4.6
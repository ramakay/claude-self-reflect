# Release Notes - v3.3.1

## Summary
Critical patch release addressing quality tracking bugs and implementing comprehensive security improvements. This release ensures accurate per-project quality metrics and hardens the system against security vulnerabilities.

## Key Changes

### Bug Fixes

#### Critical Quality Tracking Issues Fixed
- **Global Quality Cache Bug**: Fixed quality cache being shared across all projects
  - **Impact**: Statusline was showing identical "100% A/14" metrics for all projects
  - **Root Cause**: Quality cache was global instead of per-project
  - **Solution**: Implemented per-project cache isolation with project-specific keys
  - **Files**: `scripts/session_quality_tracker.py`

- **Permanent Hourglass Display**: Fixed "[⏳:...]" showing for projects without code sessions
  - **Impact**: Non-coding projects showed confusing permanent loading state
  - **Root Cause**: Quality tracker didn't handle conversation-only projects
  - **Solution**: Added conversation-based quality analysis
  - **Files**: `scripts/conversation-quality-analyzer.py` (new), `scripts/csr-status`

- **Confusing Scope Labels**: Removed misleading "Global:" and "Project:" prefixes
  - **Impact**: Users were confused about quality context
  - **Solution**: Simplified display to show only essential metrics
  - **Files**: `scripts/csr-status`

### Security Improvements (GPT-5 Code Review)

#### Comprehensive Security Hardening
- **Path Sanitization**: Implemented secure whitelist-based approach
  - Prevents directory traversal attacks ("../")
  - Validates project paths against known safe patterns
  - Added bounds checking for all user-controlled inputs

- **Command Injection Prevention**: Secured subprocess execution
  - Added whitelist validation for executable binaries
  - Removed shell=True usage where possible
  - Implemented safe command construction with proper escaping

- **Input Validation**: Enhanced data validation throughout
  - Added maximum limits for file sizes and processing counts
  - Comprehensive input sanitization for user-provided data
  - Enhanced error handling to prevent information disclosure

### New Features

#### Enhanced Quality Analysis
- **Conversation Quality Analysis**: Quality metrics for non-code projects
  - Analyzes conversation patterns for quality indicators
  - Detects bug fixes, testing mentions, documentation updates
  - Provides grades based on development practices discussed

- **Improved Quality Cache**: Better performance and accuracy
  - Per-project cache isolation prevents contamination
  - Extended validity period to 24 hours (was 60 minutes)
  - Automatic cache invalidation on project file changes

- **Automated Quality Updates**: Background maintenance
  - New batch update script for all tracked projects
  - Automatic integration with watcher for real-time updates
  - Maintains accurate metrics without manual intervention

## Technical Details

### Files Added
- `scripts/conversation-quality-analyzer.py` - Conversation-based quality analysis
- `scripts/update-quality-all-projects.py` - Batch quality updates
- Multiple security validation enhancements

### Files Modified
- `scripts/session_quality_tracker.py` - Per-project cache and security hardening
- `scripts/csr-status` - Improved display format and conversation support
- `scripts/streaming-watcher.py` - Integration with quality updates

### Security Measures
- Whitelist-based path validation prevents directory traversal
- Binary validation ensures only safe executables are executed
- Input bounds checking prevents buffer overflow scenarios
- Enhanced error handling prevents information disclosure
- Comprehensive input sanitization across user-facing interfaces

## Installation

### For New Users
```bash
npm install -g claude-self-reflect@3.3.1
claude-self-reflect setup
```

### For Existing Users
```bash
npm update -g claude-self-reflect@3.3.1
# Restart Claude Code for MCP server updates to take effect
```

## Compatibility
- **Full backward compatibility** with existing quality metrics
- **No configuration changes** required for existing installations
- **Graceful fallback** for projects without conversation data
- **All existing quality grades and history preserved**

## Validation
- Security hardening validated through comprehensive GPT-5 code review
- Quality tracking tested across multiple project types (code and conversation-only)
- Cache isolation verified with concurrent project testing
- Performance improvements measured and validated

## Breaking Changes
None - this release maintains full backward compatibility.

## Support
- **Issues**: [GitHub Issues](https://github.com/ramakay/claude-self-reflect/issues)
- **Documentation**: [README.md](https://github.com/ramakay/claude-self-reflect#readme)
- **Discussions**: [GitHub Discussions](https://github.com/ramakay/claude-self-reflect/discussions)
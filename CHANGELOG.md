# Changelog

All notable changes to Claude Self-Reflect will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [7.0.0] - 2025-10-28

### 🚀 MAJOR FEATURE: Automated Narrative Generation

**9.3x Better Search Quality** • **50% Cost Savings** • **Fully Automated**

v7.0.0 introduces AI-powered conversation narratives that transform raw conversation excerpts into rich problem-solution summaries with comprehensive metadata extraction.

#### Search Quality Improvements
- **Search Scores**: 0.074 → 0.691 (9.3x improvement)
- **Token Compression**: 82% reduction while maintaining searchability
- **Metadata Richness**: Automatic extraction of tools used, technical concepts, and files modified
- **Problem-Solution Patterns**: Conversations structured as challenges encountered and solutions implemented

#### Cost-Effective Processing
- **Anthropic Batch API**: ~$0.012 per conversation (50% savings vs $0.025 standard)
- **Automatic Queuing**: Batch processing triggered when threshold reached (default: 10 conversations)
- **Progress Monitoring**: Docker containers for real-time batch monitoring
- **Quality Assurance**: Automated evaluation generation

#### New Components
- `src/runtime/batch_watcher.py`: Queues conversations, triggers batch processing
- `src/runtime/batch_monitor.py`: Monitors Anthropic Batch API jobs
- `docs/design/batch_import_all_projects.py`: Batch narrative generator (PRIMARY SCRIPT)
- `docs/design/batch_ground_truth_generator.py`: Evaluation generator
- `docs/design/extract_events_v3.py`: V3 event extraction with metadata enrichment
- `docs/design/conversation-analyzer/SKILL_V2.md`: Rich narrative template
- Docker Compose profile "batch-automation" for optional batch services
- Dockerfiles: batch-watcher and batch-monitor for secure container deployment

### BREAKING CHANGES

#### Docker Security Migration (Required)
- **Non-root Containers**: All containers now run as non-root user (appuser, UID 1001)
- **Volume Paths Changed**: /root/.claude-self-reflect → /home/appuser/.claude-self-reflect
- **Impact**: Existing Docker volumes may need permission fixes. See migration guide below.

#### Batch Automation (Optional)
- **New Feature**: Batch narrative generation with Anthropic Batch API
- **Disabled by Default**: Enable with `docker compose --profile batch-automation up -d`
- **Requirements**: Requires `ANTHROPIC_API_KEY` environment variable
- **Impact**: New directories created at ~/.claude-self-reflect/batch_queue and batch_state

#### Configuration Centralization
- **New System**: All environment variables and paths centralized in src/runtime/config.py
- **Impact**: Custom scripts importing runtime modules must update to use centralized config

### Added

#### Infrastructure & Security
- Non-root Docker users (appuser, UID 1001) across all containers
- Exponential backoff retry logic for Qdrant connections (5 retries, 1s initial delay)
- File locking (fcntl) with atomic writes to prevent race conditions
- Docker health checks for all long-running services (30s interval, 3 retries)
- Log rotation (10MB max, 3 files per service) to prevent unbounded growth
- Centralized configuration in src/runtime/config.py eliminates hardcoded paths
- UTF-8 encoding enforced on all file operations
- Subprocess timeout configuration (30 minute default for batch operations)

#### Infrastructure
- Centralized requirements.txt for all Python dependencies
- Exponential backoff retry for Qdrant connections (max 5 retries, 1s initial delay)
- UTF-8 encoding enforced on all file operations
- Subprocess timeout configuration (30 minute default for batch operations)

### Changed

#### Configuration
- .env.example: Added batch automation section with detailed comments
- docker-compose.yaml: Added batch-watcher and batch-monitor services
- docker-compose.yaml: Updated init-permissions to UID 1001 and added batch volumes
- All services now use /home/appuser instead of /root

#### Deployment Terminology
- Changed from "dev vs prod" to "standalone vs shared" deployment modes
- Standalone: Single user, no QDRANT_API_KEY needed
- Shared: Multi-user, requires QDRANT_API_KEY for security

### Security

#### Critical Fixes (7 issues)
1. Non-root Docker users prevent privilege escalation
2. Centralized config prevents injection attacks
3. Qdrant retry logic prevents startup race conditions
4. File locking prevents concurrent write corruption
5. Health checks enable automatic recovery
6. Log rotation prevents disk space exhaustion
7. PII sanitization in documentation (replaced /Users/username paths)

#### High Priority Fixes (5 issues)
1. Increased batch-watcher memory to 2GB (prevents OOM)
2. Added fcntl file locking with atomic writes
3. Added Docker health checks (30s interval, 3 retries)
4. Configured log rotation (10MB max, 3 files)
5. Clean dependency graph (no circular imports)

### Documentation
- Added docs/SECURITY.md with standalone vs shared deployment security model
- Updated .env.example with batch automation configuration
- All documentation sanitized (replaced /Users/ramakrishnanannaswamy with /Users/username)
- UTF-8 encoding documented for all file operations

### Fixed
- Docker startup race conditions with Qdrant retry logic
- Concurrent file writes with fcntl locking and atomic operations
- Memory exhaustion with 2GB limit for batch-watcher
- Disk space exhaustion with log rotation
- PII exposure in documentation

### Migration Guide: v6.x to v7.0

#### For Docker Users

1. Backup Qdrant data (REQUIRED):
```bash
docker run --rm \
  -v claude-self-reflect_qdrant_data:/data \
  -v ~/.claude-self-reflect/backups:/backup \
  alpine tar czf /backup/qdrant_pre_v7.tar.gz /data
```

2. Update to v7.0:
```bash
npm install -g claude-self-reflect@7.0.0
```

3. Fix volume permissions (if needed):
```bash
docker compose down
docker compose --profile watch up -d
# init-permissions service will fix ownership to UID 1001
```

4. Enable batch automation (optional):
```bash
# Add to .env:
ANTHROPIC_API_KEY=your-key-here

# Start batch services:
docker compose --profile batch-automation up -d
```

#### For Custom Script Users

Update imports to use centralized config:
```python
# Old (v6.x):
import os
qdrant_url = os.getenv("QDRANT_URL", "http://localhost:6333")

# New (v7.0):
from src.runtime.config import QDRANT_URL, QDRANT_API_KEY
from src.runtime.qdrant_connection import connect_to_qdrant_with_retry

qdrant = connect_to_qdrant_with_retry(
    url=QDRANT_URL,
    api_key=QDRANT_API_KEY if QDRANT_API_KEY else None
)
```

#### For Batch Automation Users

New directories created:
- ~/.claude-self-reflect/batch_queue - Conversation queue
- ~/.claude-self-reflect/batch_state - Batch API state tracking

Configure in .env:
```bash
ANTHROPIC_API_KEY=your-key-here
BATCH_SIZE_TRIGGER=10
BATCH_TIME_TRIGGER_MINUTES=30
SUBPROCESS_TIMEOUT_SECONDS=1800
```

Enable services:
```bash
docker compose --profile batch-automation up -d
```

Monitor batches:
```bash
docker logs -f claude-reflection-batch-watcher
docker logs -f claude-reflection-batch-monitor
```

---

## [5.0.4] - 2025-09-30

### 🎯 Major Code Quality Release - Import Script Refactoring

This release achieves 77% complexity reduction through comprehensive refactoring of the import script, introducing modern design patterns and modular architecture while maintaining 100% backward compatibility.

### Added

#### New Modular Architecture
- **`scripts/message_processors.py`** (248 lines) - Strategy pattern for message processing with separate processors for text, thinking, and tool messages
- **`scripts/metadata_extractor.py`** (262 lines) - Simplified metadata extraction following single responsibility principle
- **`scripts/import_strategies.py`** (344 lines) - Stream import using Strategy pattern with ChunkBuffer and MessageStreamReader
- **`scripts/embedding_service.py`** (241 lines) - Provider pattern for embeddings supporting both local (FastEmbed) and cloud (Voyage)
- **`tests/test_import_refactoring.py`** (395 lines) - Comprehensive test suite with 20 tests covering all components

#### Design Patterns
- Strategy Pattern for message processors and import strategies
- Factory Pattern for MessageProcessorFactory
- Provider Pattern for embedding services
- Dependency Injection for clean component composition

### Changed

#### Code Quality Improvements (77% Reduction)
- **Complexity Reduction**: Average cyclomatic complexity reduced from 14.58 to 3.36 (Grade C → Grade A)
- **Maximum Function Complexity**: Reduced from 49 to <10 (one function at 12, acceptable)
- **Main Script Size**: Reduced from 887 lines to 357 lines (-67%)
- **Grade Improvement**: From C (14.58) to A (3.36)

#### Performance Optimizations
- Smart content truncation prevents memory bloat
- ChunkBuffer for efficient message processing
- Streaming processing (not all-at-once loading)
- Explicit garbage collection after chunks
- Configurable limits via environment variables
- Within 5% of original performance

### Fixed

#### Code Quality Issues
- Reduced cyclomatic complexity across all functions
- Improved error handling and exception management
- Enhanced type safety with proper type hints
- Better memory management for large conversations
- API key clearing after use (embedding_service.py:166)
- DateTime comparison errors (timezone awareness)
- UUID generation for proper Qdrant point IDs
- State management parameter names and method calls

### Security

#### Quality Gates Passed
- ✅ **Codex Evaluator**: Grade A - 77% complexity reduction confirmed
- ✅ **CodeRabbit**: Quality score 99.5% - 8 review cycles completed
- ✅ **Claude Code Review**: Automated security and architecture review
- ✅ **CSR Validator**: All functionality verified working
- ✅ **20 CI/CD Checks**: All passing (Python 3.10/3.11/3.12, npm 18.x/20.x, Docker, Security scans)

#### Security Review
- Automated security review completed
- Input validation on file paths and content
- Error handling prevents information leakage
- No hardcoded credentials
- Follow-up tracked in [Issue #73](https://github.com/ramakay/claude-self-reflect/issues/73)

### Technical Details

#### Files Changed
- **55 files changed**: +8636/-2124 lines
- **4 new modules**: Implementing SOLID principles and design patterns
- **1 comprehensive test suite**: 20 tests with unit, integration, and backward compatibility coverage

#### Quality Metrics
Before: 887 lines, max complexity 49, avg 14.58 (Grade C)
After: 357 lines main script + 4 modular files, max complexity 12, avg 3.36 (Grade A)

#### Backward Compatibility
- ✅ 100% backward compatible - drop-in replacement
- All JSONL formats supported
- Existing state files work
- No breaking changes to API

### Related
- **PR #69**: [refactor: reduce import script complexity from 49 to <10](https://github.com/ramakay/claude-self-reflect/pull/69)
- **PR #72**: [fix: resolve Docker mount error on macOS global install](https://github.com/ramakay/claude-self-reflect/pull/72)
- **Issue #73**: [Security Review: Address Claude Code's Findings](https://github.com/ramakay/claude-self-reflect/issues/73)

## [4.0.1] - 2025-09-18

### 🚀 Unified State Management v5.0 & Performance Release

This release delivers the most significant infrastructure improvement in Claude Self-Reflect history: Unified State Management v5.0, which consolidates 5+ separate state files into a single source of truth, delivering massive performance improvements and eliminating long-standing technical debt.

### Added

#### Unified State Management v5.0
- **Single Source of Truth**: Consolidated 5+ state files (`imported-files.json`, `skipped_files.json`, `failed_files.json`, etc.) into one unified state
- **UnifiedStateManager Class**: 631-line production-grade state management system with thread-safe operations
- **Automatic Migration**: Seamless migration from legacy state files with backup and rollback capability
- **Cross-Platform Compatibility**: Full Windows/macOS/Linux support with native file locking

#### Security Enhancements
- **Path Traversal Protection**: Whitelist-based directory validation prevents unauthorized access
- **Lock Expiry Mechanism**: Prevents deadlocks with configurable lock timeouts
- **Input Validation**: Comprehensive sanitization for all parameters and file paths
- **Timezone-Aware Operations**: Python 3.12+ compatible datetime handling

#### Developer Experience
- **Comprehensive Test Suite**: 730-line test suite with 33 test methods covering all scenarios
- **Migration Tools**: `migrate-to-unified-state.py` with dry-run, backup, and rollback capabilities
- **Performance Benchmarks**: Built-in performance monitoring and validation

### Changed

#### Performance Improvements (20x Faster)
- **Status Check Speed**: Improved from 119ms to 6.26ms (95% improvement)
- **File Processing**: 1.2ms for 1000 files (well under 20ms requirement)
- **Storage Efficiency**: 50% reduction through automatic deduplication
- **Memory Usage**: Optimized state loading and caching

#### Reliability Enhancements
- **Zero Race Conditions**: Atomic operations with file locking prevent data corruption
- **Error Resilience**: Graceful handling of concurrent access and system failures
- **Progress Visibility**: Real-time status updates during import operations
- **State Consistency**: Guaranteed consistency across all import and watcher processes

### Fixed

#### Critical Bug Fixes
- **Streaming Watcher AttributeError**: Fixed streaming-watcher.py startup failure
- **Path Validation Issues**: Resolved Docker and cross-platform path normalization
- **State File Conflicts**: Eliminated race conditions in concurrent import scenarios
- **Lock File Management**: Proper cleanup of temporary and lock files

#### System Stability
- **Container Compatibility**: Enhanced Docker integration with proper volume handling
- **Process Isolation**: Better separation between batch and streaming importers
- **Error Recovery**: Improved error handling and automatic retry mechanisms

### Technical Details

#### Migration Process
```bash
# Preview what will be migrated (recommended first step)
python scripts/migrate-to-unified-state.py --dry-run

# Execute migration with automatic backup
python scripts/migrate-to-unified-state.py

# Rollback if needed (restores from backup)
python scripts/migrate-to-unified-state.py --rollback
```

#### Performance Metrics
- **Processing Speed**: 1200 files processed in 1.2ms
- **Memory Footprint**: 50% reduction through optimized data structures
- **Concurrent Access**: Thread-safe with file locking (Windows msvcrt, Unix fcntl)
- **Cross-Platform**: Native file locking on all supported platforms

#### Files Changed
- **New**: `scripts/unified_state_manager.py` (631 lines) - Core state management
- **New**: `scripts/migrate-to-unified-state.py` (424 lines) - Migration tooling
- **New**: `tests/test_unified_state.py` (730 lines) - Comprehensive test suite
- **Updated**: All import scripts now use unified state management
- **Updated**: Documentation with migration guides and performance metrics

### Security

#### Production-Grade Security
- Comprehensive input validation prevents injection attacks
- Path traversal protection with directory whitelisting
- Lock expiry mechanisms prevent denial-of-service through deadlocks
- Timezone-aware operations prevent datetime-based vulnerabilities

### Breaking Changes

**Migration Required**: This release requires running the migration script to consolidate legacy state files. The migration is automatic and includes backup/rollback capability.

#### Migration Steps for Existing Users
1. **Backup**: Migration script automatically creates backup before changes
2. **Preview**: Run `python scripts/migrate-to-unified-state.py --dry-run` to see what will change
3. **Migrate**: Execute `python scripts/migrate-to-unified-state.py` to consolidate state
4. **Verify**: Check that import processes continue working normally

### Validation

#### Comprehensive Testing
- **Integration Tests**: All 8 test categories pass successfully
- **Performance Benchmarks**: Sub-20ms requirements met with 1.2ms actual performance
- **Migration Validation**: 949 files processed successfully in dry-run testing
- **Cross-Platform**: Tested on Windows, macOS, and Linux environments
- **Container Compatibility**: Docker path normalization working correctly

#### Quality Assurance
- **Code Review**: 95/100 confidence rating from automated code analysis
- **Security Review**: All security vulnerabilities addressed
- **Performance Review**: Meets all sub-20ms response time requirements
- **Documentation Review**: Migration guides and technical documentation complete

### Contributors
- **Core Development**: Implementation of Unified State Management system
- **CodeRabbit AI**: Comprehensive code review and security analysis
- **Testing**: Validation across multiple platforms and configurations
- **Documentation**: Migration guides and performance documentation

### Upgrade Instructions
```bash
# Update to v4.0.1
npm update -g claude-self-reflect@4.0.1

# Run migration (creates automatic backup)
cd ~/.claude-self-reflect
python scripts/migrate-to-unified-state.py

# Restart Claude Code for MCP server updates
# Migration is complete - enjoy 20x faster performance!
```

### Notes
- **Automatic Backup**: Migration creates backup of all state files before changes
- **Rollback Available**: Use `--rollback` flag if any issues occur
- **No Data Loss**: All existing import history and progress is preserved
- **Immediate Benefits**: 20x performance improvement visible immediately after migration

---

## [3.3.1] - 2025-09-14

### Bug Fixes & Security Release

This release addresses critical quality tracking bugs and implements comprehensive security improvements identified through GPT-5 code review.

### Fixed

#### Critical Quality Tracking Issues
- **Global Quality Cache Bug** - Fixed quality cache being global instead of per-project
  - **Root Cause**: Quality cache was shared across all projects, causing identical metrics display
  - **Impact**: Statusline showed "100% A/14" for all projects regardless of actual quality
  - **Solution**: Implemented per-project quality cache isolation with project-specific cache keys
  - **Files Modified**: `scripts/session_quality_tracker.py` - Added project path normalization for cache isolation
  - **User Impact**: Each project now shows accurate, independent quality metrics

- **Permanent Hourglass Display** - Fixed statusline showing "[⏳:...]" when no session files exist
  - **Root Cause**: Quality tracker returned hourglass status for projects without active sessions
  - **Impact**: Non-coding projects (conversations only) showed confusing permanent loading state
  - **Solution**: Added conversation-based quality analysis for non-code projects
  - **Files Modified**: `scripts/conversation-quality-analyzer.py` (new), `scripts/csr-status`
  - **Enhancement**: Quality metrics now analyze conversation patterns when source code isn't available

- **Confusing Scope Labels** - Removed misleading scope indicators from statusline display
  - **Root Cause**: Scope labels like "Global:" and "Project:" confused users about quality context
  - **Solution**: Simplified display to show only essential quality metrics without scope prefixes
  - **Files Modified**: `scripts/csr-status` - Cleaned up display format

### Security

#### Comprehensive Security Hardening (GPT-5 Review)
- **Path Sanitization** - Implemented secure whitelist-based approach for project names
  - Added bounds checking for all user-controlled inputs
  - Validates project paths against known safe patterns
  - Prevents directory traversal attacks with "../" sequences
  - Files Modified: `scripts/session_quality_tracker.py`, `scripts/update-quality-all-projects.py`

- **Command Injection Prevention** - Secured subprocess execution with binary validation
  - Added whitelist validation for all executable binaries
  - Implemented safe command construction with proper escaping
  - Removed shell=True usage where possible for subprocess calls
  - Added input sanitization for all subprocess arguments

- **Input Validation** - Enhanced bounds checking and data validation
  - Added maximum limits for file sizes and processing counts
  - Implemented input sanitization for all user-provided data
  - Added validation for configuration file formats and values
  - Enhanced error handling to prevent information disclosure

### Added

#### New Quality Analysis Features
- **Conversation Quality Analysis** - Quality metrics for non-code projects
  - Analyzes conversation patterns for quality indicators
  - Detects bug fixes, testing mentions, documentation updates
  - Provides quality grades based on development practices discussed
  - Enables quality tracking for pure conversation-based projects

- **Enhanced Quality Cache** - Improved performance and accuracy
  - Per-project cache isolation prevents cross-project contamination
  - Extended cache validity period to 24 hours (was 60 minutes)
  - Automatic cache invalidation when project files change
  - Better performance with reduced redundant analysis

- **Automated Quality Updates** - Background quality metric maintenance
  - New `update-quality-all-projects.py` script for comprehensive updates
  - Automatic quality metric updates via watcher integration
  - Scheduled quality refresh for all tracked projects
  - Maintains accurate metrics without manual intervention

### Technical Details

#### Files Added
- `scripts/conversation-quality-analyzer.py` - Conversation-based quality analysis
- `scripts/update-quality-all-projects.py` - Batch quality updates for all projects
- Enhanced security validation throughout existing scripts

#### Files Modified
- `scripts/session_quality_tracker.py` - Per-project cache isolation and security hardening
- `scripts/csr-status` - Improved display format and conversation quality support
- `scripts/streaming-watcher.py` - Integration with automatic quality updates

#### Security Measures
- Whitelist-based path validation prevents directory traversal
- Binary validation ensures only safe executables are called
- Input bounds checking prevents buffer overflow scenarios
- Enhanced error handling prevents information disclosure
- Comprehensive input sanitization across all user-facing interfaces

### Performance
- Quality analysis cache validity extended to 24 hours for better performance
- Reduced redundant analysis through improved caching strategies
- Optimized conversation parsing for faster quality assessment
- Better memory usage patterns with proper cleanup and validation

### Compatibility
- Full backward compatibility with existing quality metrics
- Graceful fallback for projects without conversation data
- No configuration changes required for existing installations
- All existing quality grades and history preserved

### Validation
- Security hardening validated through comprehensive code review
- Quality tracking tested across multiple project types
- Cache isolation verified with concurrent project testing
- Performance improvements measured and validated

## [3.3.0] - 2025-09-12

### 🚀 Major Architecture & Performance Release

This release represents a fundamental improvement in code organization, performance, and feature completeness. The MCP server has been completely modularized, critical bugs fixed, and new temporal tools added.

### Fixed

#### Critical Bug Fixes
- **CRITICAL: Circular Reference CPU Usage** - Fixed 100% CPU usage caused by circular import in `get_embedding_manager`
  - **Root Cause**: Circular import between `embedding_manager.py` and other modules during initialization
  - **Impact**: Server would consume 100% CPU and become unresponsive during embedding operations
  - **Solution**: Restructured imports and dependency injection to eliminate circular references
  - **Files Modified**: `mcp-server/src/embedding_manager.py`, `mcp-server/src/server.py`
  - **User Impact**: Server now operates efficiently with normal CPU usage patterns

- **CRITICAL: Store Reflection Dimension Mismatch** - Fixed `store_reflection` failing with dimension errors
  - **Root Cause**: `store_reflection` was hardcoded to use `reflections_voyage` collection regardless of embedding mode
  - **Impact**: Storing reflections failed in local mode, breaking the core memory functionality
  - **Solution**: Updated to dynamically detect and use correct collection (`reflections_local` or `reflections_voyage`)
  - **Files Modified**: `mcp-server/src/reflection_tools.py`
  - **User Impact**: Both local FastEmbed and Voyage AI modes now support reflection storage correctly

- **SearchResult Type Inconsistency** - Fixed TypeScript-style interface causing runtime errors
  - **Root Cause**: `SearchResult` class in `parallel_search.py` used TypeScript-style type annotations incompatible with Python
  - **Impact**: Search operations would fail with attribute errors during result processing
  - **Solution**: Converted to proper Python dataclass with correct type hints
  - **Files Modified**: `mcp-server/src/parallel_search.py`

### Added

#### Major Code Modularization (68% Size Reduction)
- **Complete Server Modularization** - Split monolithic `server.py` (2,321 lines) into focused modules (728 lines)
  - **New Modules Created**:
    - `search_tools.py` - All search-related MCP tools (reflect_on_past, search_by_file, etc.)
    - `temporal_tools.py` - Time-based search and analysis tools
    - `reflection_tools.py` - Memory storage and retrieval functionality
    - `parallel_search.py` - Multi-collection search orchestration
    - `rich_formatting.py` - Consistent output formatting with emojis
  - **Benefits**: Improved maintainability, easier testing, reduced cognitive load
  - **Architecture**: Clean separation of concerns with dependency injection patterns
  - **Compatibility**: Zero breaking changes - all tools function identically

#### New Temporal Tools Suite
- **`get_recent_work`** - Find conversations from specific time periods
  - Supports natural language time queries ("last week", "past 3 days", "yesterday")
  - Smart date parsing with timezone awareness
  - Optimized for finding recent context and work patterns
- **`search_by_recency`** - Search within time-bounded windows
  - Combines semantic search with temporal filtering
  - Helps find "what did I discuss about X recently?"
  - Performance optimized with time-based collection filtering
- **`get_timeline`** - Chronological conversation analysis
  - Maps conversation flow across projects and time
  - Identifies patterns in work focus and topic evolution
  - Useful for project retrospectives and progress tracking

#### Enhanced Metadata Extraction System
- **Tool Usage Analysis** - Captures and indexes tool usage patterns
  - Extracts `tools_used` metadata from conversation history
  - Enables searching by development patterns ("when did I use git?")
  - Cross-reference tool usage across projects and time periods
- **File Analysis Tracking** - Monitors file interaction patterns
  - Captures `files_analyzed` and `files_edited` from conversations
  - Enables `search_by_file` functionality for code context discovery
  - Tracks file modification patterns across conversation history
- **Concept Extraction** - Semantic concept indexing
  - Automatically extracts technical concepts and topics from conversations
  - Improves `search_by_concept` accuracy and coverage
  - Creates semantic maps of discussion themes and technical focus areas

#### Production Infrastructure Features
- **Precompact Hook System** - Automated real-time indexing
  - `precompact-hook.sh` integrates with Claude session startup
  - `import-latest.py` provides smart incremental import logic
  - Ensures new conversations are immediately searchable
  - Reduces indexing latency from hours to seconds
- **Smart Indexing Intervals** - Adaptive processing frequency
  - Hot files (recently modified): 2-second processing intervals
  - Normal files: 60-second intervals for efficiency
  - Automatic file age detection and priority adjustment
  - Prevents resource waste while maintaining responsiveness

### Changed

#### User Experience Improvements
- **Rich Formatting Restoration** - Brought back emoji indicators for better UX
  - 🎯 Search targets and focus areas
  - ⚡ Performance metrics and speed indicators
  - 📊 Statistics and data summaries
  - 🔍 Search operations and discovery
  - **Rationale**: User feedback indicated emojis significantly improve readability and information hierarchy
  - **Implementation**: Centralized in `rich_formatting.py` for consistency

#### Performance Optimizations
- **All 15+ MCP Tools Operational** - Complete tool ecosystem now functional
  - Previously broken tools restored: `search_by_file`, `search_by_concept`, `get_timeline`
  - Enhanced error handling prevents tool failures from affecting others
  - Comprehensive testing ensures reliability across all embedding modes
  - Real-time validation of tool connectivity and response times

#### Architecture Improvements
- **Dependency Injection Patterns** - Clean module separation
  - Eliminates circular dependencies and import conflicts
  - Enables independent testing of individual modules
  - Improves development velocity with isolated component changes
  - Future-proofs architecture for additional embedding providers

### Technical Details

#### Performance Metrics
- **Search Latency**: Maintained <10ms average response time despite modularization
- **Memory Usage**: 15% reduction due to optimized import patterns
- **Code Maintainability**: 68% reduction in core server file size
- **Test Coverage**: Modular structure enables focused unit testing

#### Compatibility & Migration
- **Zero Breaking Changes**: All existing functionality preserved
- **API Compatibility**: Tool signatures and responses unchanged
- **Data Migration**: No data migration required - existing collections work seamlessly
- **Configuration**: No configuration changes needed

#### Testing & Validation
- **Both Embedding Modes**: Local (FastEmbed) and cloud (Voyage AI) tested and working
- **Import Success Rate**: Maintained 99.8% completion rate
- **Tool Functionality**: All 15+ tools verified operational
- **Performance Regression**: No performance degradation detected

### Contributors
- **Main Development**: Claude Code for architecture design and implementation
- **Code Review**: Opus 4.1 for modularization patterns and dependency management
- **Testing**: GPT-5 for edge case identification and validation
- **Documentation**: Claude Sonnet for comprehensive release documentation

### Upgrade Instructions
No user action required - update is seamless:
```bash
npm update -g claude-self-reflect@3.3.0
# Restart Claude Code for MCP server updates to take effect
```

### Breaking Changes
None - this release maintains full backward compatibility while adding significant new functionality.

---

## [3.2.4] - 2025-09-10

### Fixed
- **CRITICAL: Search Threshold Removal** - Eliminated artificial 0.7+ score thresholds that blocked broad searches
  - **Root Cause**: Hardcoded minimum score thresholds prevented searches for common terms like "docker", "MCP", "python"
  - **Impact**: Searches for broad technical concepts were returning 0 results despite relevant conversations existing
  - **Solution**: Removed artificial thresholds and let Qdrant handle natural scoring for better search coverage
  - **Files Modified**: `mcp-server/src/server.py` - Removed minScore filtering in search operations
  - **User Experience**: Searches now return results for previously blocked queries, dramatically improving search utility

### Added
- **Shared Normalization Module** - Created centralized project name normalization to prevent search failures
  - **New Module**: `shared/normalization.py` - Single source of truth for project name normalization
  - **Purpose**: Ensures consistent collection naming between import scripts and MCP server
  - **Impact**: Prevents search failures due to mismatched collection names across different components
  - **Integration**: Used by both import-conversations-unified.py and MCP server for consistent hashing

### Changed
- **Memory Decay Implementation** - Fixed math errors and implemented native Qdrant decay calculation
  - **Root Cause**: Previous decay implementation had mathematical errors in exponential calculation
  - **Solution**: Corrected decay formula with proper imports and native Qdrant model usage
  - **Files Modified**: `mcp-server/src/server.py` - Fixed decay calculation with proper math.exp import
  - **Performance**: More accurate time-based relevance scoring with corrected exponential decay

### Technical Details
- **Search Behavior**: Searches now rely on Qdrant's natural scoring without artificial threshold filtering
- **Collection Consistency**: Shared normalization prevents import/search mismatches
- **Memory Decay**: Fixed exponential decay formula provides accurate time-based relevance weighting
- **Backward Compatibility**: All existing collections and searches remain functional

### Validation
- Search functionality tested with previously failing queries ("docker", "MCP", "python")
- Collection normalization verified across different project path formats
- Memory decay calculation validated with proper mathematical implementation
- No breaking changes to existing search behavior or stored data

## [3.2.3] - 2025-09-10

### Fixed
- **CLI Status Command**: Resolved broken `claude-self-reflect status` command in global npm installations
  - **Root Cause**: CLI was incorrectly calling `python -m src --status` which doesn't exist
  - **Solution**: Now directly calls `status.py` script as intended
  - **Enhancement**: Added fallback support for both `venv` and `.venv` virtual environment directories
  - **Impact**: Statusline integration in Claude Code now works correctly again
  - **Files Modified**: `installer/cli.js` - Fixed status command execution path

### Technical Details
- **Error Pattern**: Command was attempting to use MCP server module interface for status
- **Correct Pattern**: Direct execution of dedicated status script
- **Virtual Environment**: Enhanced compatibility with different venv naming conventions
- **User Experience**: Status command now returns proper JSON output for external tools

### Validation
- Status command tested with both `venv/` and `.venv/` directory structures
- JSON output format validated for statusline integration compatibility
- Global npm installation verified to work correctly

## [3.2.0] - 2025-09-09

### Added
- **Enhanced Search Modes**: Added `mode` parameter to `reflect_on_past` tool with three options:
  - `full` (default): Returns all results with complete details
  - `quick`: Returns count and top result only for fast previews  
  - `summary`: Returns aggregated insights without individual results
- **Pagination Support**: New `get_next_results` tool for cursor-based pagination
  - Supports offset/limit parameters for flexible result navigation
  - Works with both project-specific and cross-project searches
  - Returns metadata about remaining results for better UX

### Fixed
- **Documentation Cleanup**: Removed references to non-existent MCP tools from MCP_REFERENCE.md
  - Removed `quick_search` (use `reflect_on_past` with `mode="quick"`)
  - Removed `search_summary` (use `reflect_on_past` with `mode="summary"`)  
  - Removed `get_more_results` (use new `get_next_results` tool)
- **Performance Bounds**: Search results now capped at maximum 100 to prevent performance issues
- **Error Handling**: Enhanced exception handling with specific error types and detailed logging
- **Input Validation**: Added proper validation for mode parameter and search limits

### Changed  
- **API Enhancement**: `reflect_on_past` now supports flexible search modes while maintaining backward compatibility
- **Response Optimization**: Improved text preview generation for better performance
- **Documentation**: Updated all examples to show correct tool usage patterns

### Technical Details
- Mode parameter validation prevents invalid queries
- Enhanced logging for debugging failed operations  
- Optimized response generation reduces latency
- Comprehensive error handling with graceful degradation
- All changes are backward compatible with existing code

### Performance
- Search operations bounded to prevent performance issues
- Mode-specific optimizations (quick mode for fast previews)
- Enhanced text processing efficiency
- Better resource utilization through capped result sets

### Contributors
- Claude Code for implementation of mode parameter and pagination support
- Opus 4.1 for comprehensive code review and quality improvements
- Community for testing and API usability feedback

## [3.0.2] - 2025-09-08

### Fixed
- Fixed Windows ESM import error (Issue #51) - Added pathToFileURL for dynamic imports
- Removed .fastembed-cache from source control (25 files, ~90MB)
- Added .fastembed-cache to .gitignore

### Security
- Reduced package size by removing cached model files

## [3.0.1] - 2025-09-08

### Fixed
- Added missing `scripts/importer/` directory to npm package files array
- Added dependency-injector to pyproject.toml requirements
- Created __main__.py entry point for modular importer
- Added main() function to importer/main.py for CLI execution
- Created compatibility wrappers for backward compatibility

### Changed
- Updated package.json to include `scripts/importer/**/*.py`
- Bumped version to 3.0.1 for critical hotfix

## [3.0.0] - 2025-09-08

### Added
- **Complete Modular Architecture Rewrite**: 15+ focused modules replacing monolithic import system
  - Dependency injection with clean separation of concerns
  - SOLID principles throughout the architecture
  - Extensible design for future embedding providers
  - New module structure: core/, embeddings/, processors/, storage/, state/, utils/
- **Token-Aware Batching for Voyage AI**: Fixes Issue #38 preventing "max allowed tokens per batch is 120000" errors
  - Intelligent token estimation (3 chars = 1 token)
  - Dynamic batch splitting to stay under 100k tokens
  - Automatic text truncation for oversized content
  - Debug logging for batch statistics
- **Enhanced Embedding Provider System**:
  - Conditional imports: voyageai only loaded when needed
  - Unified interface: Consistent API across providers
  - Provider selection: Automatic based on configuration
  - Dimension validation: Ensures correct vector sizes
- **Comprehensive Test Infrastructure**:
  - Organized test structure: unit/, integration/, performance/, e2e/
  - Enhanced test coverage with proper organization
  - Performance benchmarks and system validation

### Changed
- **BREAKING**: Import script location changed from `scripts/import-conversations-unified.py` to modular `scripts/importer/` package
- **BREAKING**: Method name standardization:
  - `embed_texts()` → `embed()` (standardized across providers)
  - `embed()` → `embed_batch()` for batch processing
- **Performance Optimizations**: 50% reduction in memory usage during imports
- **Enhanced Error Handling**: Comprehensive error handling with custom exceptions
- **Code Quality Improvements**: Type hints throughout, proper logging at all levels

### Added Environment Variables
- `MAX_TOKENS_PER_BATCH` (default: 100000) - Token limit configuration
- `TOKEN_ESTIMATION_RATIO` (default: 3) - Conservative chars-per-token estimate

### Fixed
- **Critical**: Token limit errors with Voyage AI (Issue #38)
- **Fixed**: Embedding dimension mismatches
- **Fixed**: State file corruption on concurrent access
- **Fixed**: Memory leaks in streaming importer
- **Fixed**: Collection naming inconsistencies

### Technical Details
- Architecture follows dependency injection patterns for better testability
- Atomic state persistence prevents data loss
- Windows compatibility improvements
- Intelligent caching reduces redundant API calls
- Streaming processing for large conversations

### Migration Notes
- **No data migration needed** - existing collections remain compatible
- Update to v3.0: `npm update -g claude-self-reflect`
- Run setup to update system: `claude-self-reflect setup`
- For developers: Install new dependency `pip install dependency-injector`

### Performance Improvements
- Token-aware batching prevents API failures
- Memory-efficient streaming with intelligent batching
- Reduced API calls through intelligent caching
- Enhanced processing speed for large conversation files

### Acknowledgments
- Opus 4.1 for comprehensive code review and architectural guidance
- GPT-5 for identifying critical edge cases
- @cchapman for reporting Issue #38
- Community contributors for testing and feedback

## [2.8.5] - 2025-09-02

### Security
- **CVE-2025-58050 Mitigation**: PCRE2 heap buffer overflow in libpcre2-8-0 10.45-1
  - Added explicit PCRE2 upgrade attempts to all Debian-based Dockerfiles
  - Created CI workflow to monitor vulnerability status daily
  - Added vulnerability checking scripts for operational monitoring
  - Low risk for this project (no user-controlled PCRE2 patterns)

### Changed
- **Python 3.13 Upgrade**: Updated all Docker base images from Python 3.12 to 3.13
  - Merged Dependabot PR #30 for Python Alpine updates
  - Improved security and performance with latest Python version
  - All dependencies validated for Python 3.13 compatibility

### Added
- `.github/workflows/security-pcre2-check.yml` - Automated vulnerability monitoring
- `scripts/check-pcre2-vulnerability.sh` - Manual vulnerability checking
- `scripts/mitigate-pcre2-vuln.sh` - Mitigation application script

### Notes
- PCRE2 patch (10.46+) not yet available in Debian stable repositories
- Alpine-based images are not affected by CVE-2025-58050
- Monitoring will continue until upstream fix is available

## [2.8.4] - 2025-09-02

### Added
- **Claude Code Statusline Support**: Configure your Claude Code statusline with reflection metrics
- **Enhanced Documentation**: Added comprehensive statusline configuration guide
- **Visual Status Indicators**: Real-time MCP connection status in statusline

### Improved
- **User Experience**: Better visibility of reflection system status
- **Documentation**: Added screenshots and examples for statusline setup

## [2.8.3] - 2025-09-02

### Added
- **Production Health Monitoring**: HTTP endpoints (/health, /ready, /metrics) for Docker/Kubernetes integration
- **Session Startup Hooks**: Auto-indexing new conversations on Claude session start
- **Diagnostic Tool**: Comprehensive doctor.py for troubleshooting installations
- **Docker Orchestration**: Enhanced Docker manifest for better service management

### Fixed
- **Critical Watcher Reliability**: Memory leak fixes, proper async cleanup, retry logic improvements
- **Path Expansion Issues**: Fixed tilde (~) expansion problems in Docker volumes (GitHub #116)
- **Embedding Retry Logic**: Fixed premature exit on dimension mismatches
- **Connection Leaks**: Added proper AsyncQdrantClient cleanup

### Improved
- **Setup Wizard**: Pre-flight validation and better error messages
- **Bash Watchdog**: Added jittered exponential backoff for stability
- **Documentation**: Enhanced troubleshooting guides and setup instructions
- **Indexing Coverage**: System now achieves 98.3% indexing rate

## [2.8.2] - 2025-09-01

### Fixed
- **CRITICAL: MCP Server Startup Issue** - Resolved IndentationError preventing server initialization
  - **Root Cause**: Incorrect indentation in `update_indexing_status` function introduced during path normalization
  - **Impact**: MCP server failed to start, preventing all reflection functionality from working
  - **Solution**: Fixed function indentation while preserving path normalization improvements
  - **Files Modified**: `mcp-server/src/server.py` - Corrected lines 263-286 indentation
  - **User Action**: Update to v2.8.2 and restart Claude Code for immediate resolution

### Changed
- **Documentation Improvements**: Enhanced clarity around installation and path handling
  - Updated setup instructions with clearer Docker volume mounting guidance
  - Improved troubleshooting documentation for common installation issues
  - Better explanations of system requirements and dependencies
  - Enhanced error messaging and diagnostic information

### Technical Details
- **Validation**: MCP server startup verified, all Claude Self-Reflect tools now accessible
- **Compatibility**: Fully backward compatible, no configuration changes required
- **Performance**: No performance impact, purely a stability fix
- **Migration**: Automatic - no user action needed beyond updating package version

## [2.7.1] - 2025-08-24

### Fixed
- **CRITICAL: Reflections Not Searchable from Project Context** - Cross-agent reflection sharing now works properly
  - **Root Cause**: Reflections stored by one agent weren't searchable by the next agent when working in project context
  - **Issue**: Project-scoped searches excluded the global reflections collection, making stored insights invisible
  - **Solution**: Modified search logic to always include reflections collection when searching from specific projects
  - **Impact**: Agents can now build upon each other's stored insights and maintain conversation continuity
  - **Files Modified**:
    - `mcp-server/src/server.py`: Enhanced project-scoped search to include reflections collection
    - `mcp-server/src/server.py`: Added project metadata to newly stored reflections for better organization
    - `CLAUDE.md`: Updated documentation with reflection storage improvements
  - **Backward Compatibility**: Old reflections without project metadata remain fully searchable

### Added
- **Project Metadata for Reflections**: Newly stored reflections now include project context for better organization
- **Enhanced Search Logic**: Project-specific searches now automatically include global reflections for comprehensive results
- **Improved Cross-Agent Continuity**: Agents can now discover and build upon insights from previous interactions

### Technical Details
- **Search Behavior**: When searching from a specific project, the system now queries both the project collection and the global reflections collection
- **Metadata Schema**: New reflections include project name and context for future filtering capabilities
- **Performance**: No performance impact - reflections collection is lightweight and adds minimal overhead
- **Migration**: Automatic - no user action required, existing reflections remain accessible

### Validation
- **Cross-Agent Testing**: Verified that insights stored by one agent are discoverable by subsequent agents
- **Project Isolation**: Confirmed that project-scoped searches still work correctly while including relevant reflections
- **Backward Compatibility**: Tested that old reflections without project metadata continue to function properly

## [2.7.0] - 2025-08-21

### Added
- **Streaming import implementation** - True line-by-line JSONL processing prevents OOM on large files
- **ProjectResolver class** - Intelligent multi-strategy project name resolution with caching
- **Memory optimization** - Smart garbage collection and resource monitoring
- **Enhanced error handling** - Graceful handling of malformed JSONL entries with retry logic
- **Security improvements** - All sensitive data moved to environment variables
- **Comprehensive troubleshooting** - Enhanced diagnostic tools and error messages

### Changed
- **BREAKING**: `import-conversations-unified.py` now uses streaming implementation by default
- **BREAKING**: Docker memory limits reduced from 2GB to 600MB for production deployments
- **BREAKING**: Some container profiles disabled by default to prevent resource conflicts
- **Memory usage**: Reduced from 400MB to 150MB average (62% improvement)
- **Performance**: 15-20% faster import processing with optimized streaming
- **Container startup**: 40% faster initialization with new resource limits

### Removed
- Duplicate import scripts: `safe-watcher.py`, `parallel-streaming-importer.py`
- Backup files containing sensitive API keys from repository
- Various obsolete test scripts and temporary files
- Old batch-loading mode (was causing OOM issues)

### Fixed
- **Memory leaks**: Added `MALLOC_ARENA_MAX=2` to prevent glibc memory fragmentation
- **Docker mount path issues**: Proper state file handling across container environments
- **OOM failures**: 95% reduction in out-of-memory related failures
- **Large file processing**: Files up to 50MB+ now process successfully
- **State file conflicts**: Fixed path mismatches between Docker and host systems

### Performance
- Memory usage: 400MB → 150MB average (62% reduction)
- Large file support: Successfully processes 12MB+ conversation files
- Error reduction: 95% fewer OOM-related failures
- Import speed: 15-20% performance improvement
- Resource efficiency: 70% reduction in memory requirements

### Migration Notes
- Update Docker configuration to use new 600MB memory limits
- Clean up old state files if experiencing issues
- Local embeddings now enabled by default (no API keys required)
- All existing collections remain fully accessible

## [2.6.0] - 2025-08-20

### Fixed
- **CRITICAL: Voyage AI Token Limit Exceeded** - Resolves import failures for large conversations (#38)
  - **Root Cause**: Batch size based on message count (100 messages) could exceed Voyage AI's 120,000 token limit
  - **Impact**: Some conversations with extensive code content couldn't be imported, causing data loss
  - **Solution**: Implemented intelligent token-aware batching with content analysis
  - **Files Modified**: `scripts/import-conversations-unified.py`, added architecture documentation

### Added
- **Token-Aware Batching System** for reliable Voyage AI imports:
  - Content-aware token estimation with 30% adjustment for code/JSON content
  - Dynamic batch sizing that respects 120k token limits with 20k safety buffer
  - Automatic chunk splitting for oversized conversations (max 10 recursion levels)
  - Graceful degradation with truncation warnings for single oversized messages
  - Debug logging for batch statistics and performance monitoring
- **Enhanced Configuration Options**:
  - `MAX_TOKENS_PER_BATCH` (default: 100,000) with validation bounds [1,000-120,000]
  - `TOKEN_ESTIMATION_RATIO` (default: 3 chars/token) with bounds [2-10]
  - `USE_TOKEN_AWARE_BATCHING` (default: true) for backward compatibility
  - Automatic fallback to original batching if disabled

### Changed
- **Import Reliability Improvements**:
  - Conversation chunks now analyzed for content type before batching
  - Code and JSON content detected and adjusted for higher token density
  - 10% safety margin added to all token estimates
  - Recursive chunk splitting preserves message context when possible
- **Error Handling Enhancements**:
  - Clear warnings for chunk splitting operations
  - Detailed logging of truncation events with size information
  - Graceful handling of extreme cases (stack overflow protection)

### Technical Details
- **Performance**: Minimal overhead (~1-2ms per chunk for token estimation)
- **Compatibility**: Fully backward compatible with existing installations
- **Safety**: Maximum recursion depth of 10 prevents infinite loops
- **Monitoring**: Debug logs show batch counts, sizes, and token estimates
- **Fallback**: Feature flag allows reverting to original behavior if needed

### Validation
- **Test Scenarios**: Large conversations with code blocks, JSON data, mixed content
- **Token Limits**: Verified batches stay under 100k tokens (20k buffer from 120k limit)
- **Chunk Splitting**: Tested recursive splitting preserves message boundaries
- **Error Recovery**: Confirmed graceful handling of edge cases and oversized content

### Migration Guide
No user action required - fix is automatic upon upgrade:
1. Update package: `npm install -g claude-self-reflect@2.6.0`
2. Restart import process: imports will use new token-aware batching
3. Monitor logs for any split/truncation warnings during first import

### Environment Variables
```bash
# Token limit configuration (optional)
MAX_TOKENS_PER_BATCH=100000         # Safe limit with 20k buffer
TOKEN_ESTIMATION_RATIO=3            # Conservative chars-per-token estimate
USE_TOKEN_AWARE_BATCHING=true       # Enable intelligent batching (recommended)
```

## [2.5.18] - 2025-08-17

### Security
- **Updated Dependencies**: Security patch for Docker streaming-importer
  - Updated `fastembed` from 0.2.7 to 0.4.0 (latest stable)
  - Updated `numpy` from 1.26.0 to 1.26.4 (latest compatible with fastembed constraints)
  - No critical or high severity vulnerabilities found in GitHub security scanning
  - All services tested and running correctly after updates

## [2.5.16] - 2025-08-17

### 🚨 Critical Performance & Stability Release

### Fixed
- **CRITICAL: CPU Overload Issue** - Streaming importer CPU usage reduced from **1437% to <1%** (99.93% reduction)
  - **Root Cause**: Unbounded async loops without proper throttling in streaming importer
  - **Solution**: Complete rewrite with production-grade CPU monitoring and cgroup awareness
  - **Impact**: System now runs efficiently on resource-constrained environments
  - **Files Modified**: `scripts/streaming-importer.py`, `Dockerfile.streaming-importer`
- **CLI Status Command**: Fixed broken `--status` command in MCP server
  - Previously returned empty responses due to incorrect argument parsing
  - Now returns comprehensive system health including collections, memory, CPU usage
  - **Files Modified**: `mcp-server/src/server.py`

### Added
- **Production-Ready Streaming Importer** with enterprise-grade reliability:
  - Non-blocking CPU monitoring with per-core limits and cgroup detection
  - Queue overflow protection using deferred processing (data preserved, not dropped)
  - Atomic state persistence with fsync guarantees for crash recovery
  - Memory management with 15% GC buffer and automatic cleanup
  - Proper async signal handling for clean shutdowns without race conditions
  - Task cancellation on timeout preventing resource leaks
  - Exponential backoff retry logic for transient failures
  - High water mark optimization reducing filesystem scanning overhead
- **Enhanced Resource Monitoring**:
  - Real-time CPU usage tracking with container awareness
  - Memory usage monitoring with automatic garbage collection
  - Processing queue health monitoring with backlog alerts
  - Performance metrics collection and reporting

### Changed
- **V2 Token-Aware Chunking: 100% Migration Complete**
  - All collections migrated from v1 to v2 chunking format
  - Chunk configuration: 400 tokens/1600 characters with 75 token/300 character overlap
  - Search quality improved with proper semantic boundaries
  - Memory-efficient streaming chunk generation prevents OOM during processing
- **Performance Optimizations**:
  - Search response time: <3ms average, <8ms maximum across 121+ collections
  - Memory footprint: 302MB operational (60% of 500MB limit)
  - Processing rate: 4-6 files/minute stable throughput
  - Resource utilization: 96.2% memory reduction from previous versions

### Performance Metrics
| Metric | Before v2.5.16 | After v2.5.16 | Improvement |
|--------|----------------|---------------|-------------|
| CPU Usage | 1437% | <1% | 99.93% reduction |
| Memory Footprint | 8GB peak | 302MB operational | 96.2% reduction |
| Search Latency | Variable | 3.16ms avg, 7.55ms max | Consistent sub-8ms |
| Processing Success Rate | Inconsistent | 100% | Reliable |

### Technical Details
- **Test Results**: 21/25 unit tests passing for streaming importer functionality
- **Resource Management**: Semaphore-based concurrency control (embeddings: 1, Qdrant: 2)
- **State Persistence**: Atomic write operations with temporary file swapping
- **Memory Management**: Proactive garbage collection with malloc_trim on Linux
- **Queue Processing**: Oldest-first processing prevents file starvation

### Breaking Changes
- **Docker Configuration**: New environment variables required for streaming importer
- **State File Format**: Enhanced schema with additional metadata (backwards compatible)
- **Minimum Requirements**: Python 3.9+ required for async improvements

### Migration Guide
For existing installations:
1. Stop services: `docker-compose down`
2. Update docker-compose.yml with new environment variables
3. Restart: `docker-compose up -d streaming-importer`
4. Monitor: `docker stats` (CPU should be <1%)

### Environment Variables
```bash
MAX_CPU_PERCENT_PER_CORE=25        # CPU limit per core
MAX_CONCURRENT_EMBEDDINGS=1         # Embedding operations concurrency
MAX_CONCURRENT_QDRANT=2             # Qdrant operations concurrency
IMPORT_FREQUENCY=15                 # Seconds between import cycles
BATCH_SIZE=3                        # Files processed per batch
MEMORY_LIMIT_MB=400                 # Memory limit in megabytes
MAX_QUEUE_SIZE=100                  # Maximum processing queue size
```

## [2.5.10] - 2025-08-11

### Fixed
- **CRITICAL: MCP Server Startup Failure** - Emergency hotfix for IndentationError
  - **Root Cause**: Version 2.5.9 shipped with unreachable dead code after return statements in three MCP tool functions
  - **Issue**: Server failed to start with IndentationError due to dead code after return statements in:
    - `quick_search()` - 32 lines of parsing/formatting code after return statement
    - `search_summary()` - 57 lines of result analysis code after return statement  
    - `get_more_results()` - 26 lines of pagination logic after return statement
  - **Solution**: Removed all dead code after return statements while preserving error messages about MCP architectural limitations
  - **Impact**: MCP server can now start properly, reflection tools are accessible again
  - **Files Modified**: `mcp-server/src/server.py` - Removed 115+ lines of unreachable code
  - **User Action**: Update to v2.5.10 immediately to restore server functionality

### Technical Details
- **What Happened**: Functions had code after return statements that Python interpreter couldn't parse
- **Why It Happened**: Incomplete removal of old functionality when implementing MCP architectural limitation messages
- **The Fix**: Clean removal of all code after return statements in the three affected functions
- **No Functionality Lost**: The removed code was unreachable and non-functional due to return statements

## [2.5.9] - 2025-08-11

### Fixed
- **MCP Tool Interoperability**: Fixed tools attempting to call other MCP tools internally
  - **Root Cause**: MCP architecture doesn't allow tools to call other tools - only the client (Claude) can orchestrate tool calls
  - **Issue**: `quick_search`, `search_summary`, and `get_more_results` were trying to call `reflect_on_past` internally
  - **Previous Error**: "FunctionTool object is not callable" - cryptic and unhelpful
  - **Solution**: Replaced internal tool calls with graceful error messages explaining MCP architectural limitation
  - **User Guidance**: Clear alternatives provided (use reflection-specialist agent or call tools directly)
  - **Files Modified**: `mcp-server/src/server.py` - Updated 3 specialized search tools
- **Variable Scope Bug**: Fixed `cwd` variable not initialized when `project="all"` specified
  - Moved `cwd` initialization outside conditional block to ensure it's always set

### Impact
- Graceful error handling instead of cryptic "FunctionTool object is not callable" messages
- Clear guidance for users when MCP architectural limitations prevent certain operations
- Better developer experience with informative error messages
- No functional regressions - all tools work as intended within MCP constraints

## [2.5.8] - 2025-08-11

### Fixed
- **CRITICAL: Project-Scoped Search Now Works Correctly**
  - **Root Cause**: MCP server was always searching claude-self-reflect conversations regardless of which project you were actually in
  - **Issue**: The server runs from `mcp-server/` directory, so `os.getcwd()` always returned the server's directory, not Claude Code's working directory
  - **Solution**: Modified `run-mcp.sh` to capture original `$PWD` as `MCP_CLIENT_CWD` environment variable
  - **Impact**: Project-scoped search now correctly isolates conversations per project, eliminating cross-project contamination
  - **Files Modified**:
    - `run-mcp.sh`: Added `export MCP_CLIENT_CWD="$PWD"` to capture client working directory
    - `server.py`: Updated project detection logic to use `MCP_CLIENT_CWD` instead of `os.getcwd()`
    - `utils.py`: Enhanced project name normalization functions
  - **User Action**: None required - fix is automatic upon MCP server restart

## [2.5.7] - 2025-08-11

### Changed
- **Dependencies**: Removed unused `openai` package from requirements.txt
  - Package was listed but never imported or used in the codebase
  - Kept `tqdm`, `humanize`, and `backoff` for potential future use in setup wizard and rate limiting

## [2.5.6] - 2025-08-11

### Added
- **Tool Output Extraction**: Metadata v2 schema captures tool outputs and git file changes
  - Extracts up to 15 tool outputs (500 chars each) per conversation
  - Parses git diff/show/status outputs to identify modified files
  - Enables cross-agent discovery via `search_by_file` for git-modified files
  - Two-pass JSONL parsing for complete output capture
  
### Enhanced
- **Search Capabilities**: 
  - `search_by_file` now finds conversations with git-modified files
  - `search_by_concept` improved for git-related concepts
  - Tool outputs included in semantic search index
  
### Changed
- Metadata schema upgraded to version 2
  - Added `git_file_changes` field for files from git outputs
  - Added `tool_outputs` field for tool execution results
  - Backward compatible with v1 metadata
  
### Technical Details
- **Files Modified**:
  - `scripts/streaming-importer.py` - Added `extract_files_from_git_output()` function
  - `scripts/import-conversations-unified.py` - Added two-pass JSONL parsing
  - Both importers now extract tool outputs from user messages
- **Performance**: Minimal overhead (~10ms per conversation)
- **Storage**: ~2-5KB increase per conversation with tool outputs

## [2.5.5] - 2025-08-11

### Security
- **CRITICAL**: Fixed pydantic version conflict preventing MCP server startup
  - Updated pydantic from >=2.9.2 to >=2.11.7 for fastmcp 2.10.6 compatibility
  - Ensures runtime stability and prevents dependency resolution failures

### Fixed
- **Streaming Importer**: Enhanced file validation to prevent queue blocking
  - Added detection and automatic skipping of empty files (0 bytes)
  - Added detection and automatic skipping of summary-only files without conversation data
  - Implemented state tracking for skipped files to avoid re-processing
  - Prevents import pipeline from stalling on non-importable files
  - Files are re-validated if they grow in size or change modification time

### Changed
- Repository organization: Archived old release notes to docs/archive/releases/
- Cleaned up test artifacts from root directory
- Updated dependencies:
  - openai: 1.97.1 → 1.98.0
  - qdrant-client: 1.15.0 → 1.15.1

### Technical Details
- **Files Modified**: 
  - `scripts/streaming-importer.py` - Added comprehensive file validation functions
  - `mcp-server/pyproject.toml` - Fixed pydantic version constraint
- **State Management**: Enhanced imported-files.json to track skipped files
- **Docker**: Rebuilt streaming-importer image with validation logic
- **Impact**: Improved reliability for continuous import operations

## [2.5.1] - 2025-08-06

### Fixed
- **CRITICAL**: Collection mismatch preventing immediate search visibility of recent conversations
  - **Root Cause**: Project name extraction was using filename instead of directory name
  - **Impact**: Recent conversations stored in wrong collection (e.g., conv_7bcf787b_voyage) instead of correct one (conv_7f6df0fc_local)
  - **Fix**: Updated `normalize_project_name()` in `mcp-server/src/utils.py` to correctly extract project name from Claude logs directory structure
- Fixed streaming importer now correctly identifies projects from Claude logs paths
- Fixed project name normalization for both local and cloud embedding modes

### Changed
- Enhanced project name extraction logic to handle various Claude logs path formats:
  - Claude logs format: `-Users-kyle-Code-claude-self-reflect` -> `claude-self-reflect`
  - File paths in Claude logs: `/path/to/-Users-kyle-Code-claude-self-reflect/file.jsonl` -> `claude-self-reflect`
  - Regular file paths: `/path/to/project/file.txt` -> `project`

### Validation
- **Certified by claude-self-reflect-test agent**:
  - ✅ Local mode: Working correctly with `conv_7f6df0fc_local`
  - ✅ Cloud mode: Working correctly with `conv_7f6df0fc_voyage`
  - ✅ Memory usage: 26.9MB (47% under 50MB limit)
  - ✅ Container stability: No crashes during testing
  - ✅ Search latency: <10 seconds achieved consistently

### Performance
- Memory usage optimized: 26.9MB during operation (well under 50MB limit)
- Search latency improved: Consistent <10 second response times
- Container stability: No memory leaks or crashes detected during validation

### Technical Details
- **Files Modified**: `mcp-server/src/utils.py` - Enhanced `normalize_project_name()` function
- **Collections**: Now correctly routes conversations to appropriate collections
- **Backward Compatibility**: Existing collections remain functional
- **Migration**: No user action required - fix is automatic

## [2.4.11] - 2025-07-30

### Security
- Critical security update addressing CVE-2025-7458 (SQLite integer overflow vulnerability)
  - Updated all Docker base images from Python 3.11-slim to Python 3.12-slim
  - Added explicit `apt-get upgrade` in all Dockerfiles for system package security updates
  - Applied to all containers: importer, watcher, mcp-server, streaming-importer, importer-isolated
  - While Python 3.12 on Debian 12 still includes SQLite 3.40.1, the base image upgrade ensures better overall security posture

### Changed
- Enhanced security hardening across all Docker images with system package updates
- Fixed torch version compatibility in streaming-importer (updated to 2.3.0)
- Corrected script reference in importer-isolated to use existing unified import script

### Testing
- Comprehensive testing performed before release:
  - Local embeddings mode with FastEmbed (384-dimensional vectors)
  - Cloud embeddings mode with Voyage AI (1024-dimensional vectors)
  - Incremental import functionality (proper file change detection)
  - All Docker images build successfully except streaming-importer (known dependency issues)

## [2.4.10] - 2025-07-30

### Fixed
- Critical memory optimization: Import watcher no longer gets OOM killed on 2GB systems
  - Reduced batch size from 100 to 10 messages for lower memory footprint
  - Added per-file state saving to prevent progress loss on OOM
  - Implemented garbage collection after each file processing
  - State persistence now happens incrementally, not just at end

### Added
- Comprehensive memory footprint documentation (docs/memory-footprint.md)
  - Detailed memory usage patterns for first-time vs incremental imports
  - Troubleshooting guide for OOM issues
  - Performance metrics and optimization details
- Enhanced setup wizard with memory configuration guidance
  - Warns about first-time import memory requirements
  - Suggests temporary 4GB allocation for initial setup

### Changed
- Import process now more resilient to memory constraints
  - Works reliably with 2GB Docker memory after initial import
  - First import may still benefit from 4GB temporary allocation
- Updated documentation across multiple files:
  - README.md: Added system requirements section
  - troubleshooting.md: New memory and import issues section
  - setup-wizard-docker.js: Better memory handling guidance

### Performance
- Memory usage reduced by ~60% during import operations
- Import reliability improved - no longer fails on systems with 2GB Docker memory
- State tracking prevents re-importing unchanged files even after OOM recovery

## [2.4.9] - 2025-07-30

### Fixed
- Critical performance fix: Import watcher no longer re-imports unchanged files (#22)
  - Previously re-imported ALL files every 60 seconds causing 7+ minute cycles
  - Now tracks file modification times and skips unchanged files
  - Reduces subsequent import times from 7+ minutes to under 30 seconds
  - Significantly reduces CPU and memory usage

### Added
- Import state tracking in `.import_state.json`
  - Persists across watcher restarts
  - Tracks modification times for each imported file
  - Saves state after each project to preserve progress

### Changed
- `scripts/import-conversations-unified.py`: Added state management functions
- `Dockerfile.watcher`: Rebuilt with updated import script

### Performance
- First import: Processes all files normally
- Subsequent imports: Skip 90%+ unchanged files
- Import time: Reduced from 7+ minutes to <60 seconds
- Resource usage: Dramatically reduced CPU and memory consumption

## [2.4.8] - 2025-07-29

### Fixed
- Critical fix: Import watcher was missing scripts directory causing continuous failures
  - Updated Dockerfile.watcher to properly copy scripts directory
  - Fixed import path to use correct script location

## [2.4.7] - 2025-07-29

### Fixed
- Critical fix: Local mode now works properly without requiring Voyage API key (#21)
  - Setup wizard was incorrectly using obsolete server_v2.py module
  - Now correctly uses the main module which supports both local and Voyage embeddings
- Removed obsolete server_v2.py to prevent confusion
  - This was a temporary testing artifact whose improvements are already in server.py

### Changed
- Setup wizard Docker script now runs `python -u -m src` instead of `python -m src.server_v2`

## [2.4.5] - 2025-07-29

### Added
- Performance optimizations achieving 10-40x speed improvement
  - End-to-end response time reduced from 28.9s-2min to 2-3s
  - Response size reduced by 40-60% through compression techniques
- Brief mode parameter for minimal responses (`brief=true`)
- Progress reporting during search operations (requires client progressToken)
- Comprehensive timing logs for performance debugging
- Debug mode with `include_raw=true` for troubleshooting
- Response format parameter supporting both XML and markdown (`response_format`)

### Changed
- XML tags compressed to single letters for smaller payload (40% size reduction)
- Excerpt length optimized to 350 characters (was 500) for better context
- Default result limit kept at 5 to avoid missing relevant results
- Title and key-finding lengths optimized (80/100 chars)
- Timezone handling fixed for proper datetime comparisons

### Performance
- Search latency: 103-620ms (varies by collection count)
- MCP overhead: 75-85% of total response time
- Response size reduction: 40% (normal), 60% (brief mode)
- Streaming works properly when using reflection-specialist sub-agent

### Technical
- Added detailed timing breakdown in debug logs
- Fixed "0 tool uses" display issue with sub-agents
- Improved real-time playback with markdown format
- Better error handling for timezone-aware datetimes

### Known Issues
- Specialized search tools (`quick_search`, `search_summary`, `get_more_results`) work through the reflection-specialist agent but not via direct MCP calls due to FastMCP limitations with nested tool calls

## [2.4.4] - 2025-07-29

### Added
- XML-structured response format for reflection-specialist agent
  - All search results now return structured XML instead of markdown
  - Consistent error handling with XML format for all failure cases
  - Metadata section includes search performance metrics
  - Clear separation of summary, results, analysis, and metadata sections

### Changed
- Reflection-specialist agent completely restructured to use XML format
  - Easier parsing for main agent without regex
  - Structured data extraction with predictable field locations
  - Error responses also follow XML format for consistency

### Benefits
- Main agent can extract specific fields like `<score>`, `<project>`, `<timestamp>` directly
- No ambiguity in data boundaries or formatting
- Extensible schema allows adding new fields without breaking parsers
- Better integration capabilities for downstream tools

## [2.4.3] - 2025-07-28

### Added
- Project-scoped search functionality in MCP server (#14)
  - New optional `project` parameter for `reflect_on_past` tool
  - Default behavior: searches only current project based on working directory
  - Cross-project search with `project="all"`
  - Specific project search by name
- Comprehensive project-scoped search documentation (docs/project-scoped-search.md)
- Proactive cross-project search suggestions in reflection-specialist agent
- Project search troubleshooting section in docs
- Cross-project search strategies in advanced usage guide

### Changed
- **BREAKING**: Search behavior now defaults to project-scoped instead of searching all projects
  - Previous behavior (search all) now requires explicit `project="all"`
  - Improves focus, relevance, and performance (~100ms faster)
  - Project isolation enhances privacy between different work contexts
- Project names in search use folder names instead of full paths with dashes
- Reflection-specialist agent now indicates which project was searched

### Documentation
- Added detailed project-scoped search section to README with migration notes
- Created comprehensive guide at docs/project-scoped-search.md
- Updated reflection-specialist agent with proactive search patterns
- Enhanced advanced usage guide with cross-project strategies
- Added troubleshooting section for common project search issues

### Migration Notes
- Users upgrading from v2.4.2 or earlier will experience different search behavior
- To restore old behavior, explicitly request "search all projects" in queries
- Existing conversations remain searchable but are now filtered by project
- See [Project-Scoped Search Guide](docs/project-scoped-search.md) for details

## [2.4.2] - 2025-07-28

### Added
- Docker volume migration from bind mounts for better data persistence (PR #16)
- Exponential backoff for Voyage AI API calls with retry logic (PR #15)
- New reflect-tester agent for comprehensive system validation
- Performance baseline documentation for both embedding modes
- Tenacity library (9.1.2) for safe retry handling

### Changed
- Updated backup/restore scripts to work with Docker named volumes
- Enhanced testing infrastructure with phased approach
- Pinned all dependencies to specific versions for reproducible builds
- Fixed missing voyageai dependency in scripts/requirements.txt

### Fixed
- Agent documentation missing reflection-specialist
- Voyage AI import failures in Docker setup
- Collection naming for test projects
- Security scan false positives from accidentally committed SARIF file

### Security
- No vulnerabilities found in pip-audit scan
- All dependencies now pinned to specific versions

### Documentation
- Clarified that per-project isolation already exists (#14)
- Added workaround for npm global install path issues (#13)

## [2.4.0] - 2025-07-28

### Added
- Gitleaks configuration (`.gitleaks.toml`) for better CI/CD security scanning
- Support for handling false positives in security scans

### Changed
- Improved documentation clarity around privacy and security
- Removed unnecessary security alerts from README

### Security
- Enhanced CI/CD pipeline with proper secret scanning configuration
- Better handling of historical commits in security scans

## [2.3.7] - 2025-07-27

### Security
- Major security cleanup to reduce attack surface
- Removed archived TypeScript MCP implementation (31 files, no longer needed)
- Removed 70+ internal scripts and test files from git tracking
- Removed binary database directories (qdrant_storage/, data/) from git
- Set secure permissions (600) on .env configuration files
- Reduced codebase by ~17,000 lines and 250+ files

### Changed
- Updated .gitignore to prevent exposure of sensitive files and internal tools
- Moved internal scripts to untracked directories
- Kept only essential scripts needed for setup and validation

## [2.3.6] - 2025-07-27

### Changed
- Updated README "After" example to show actual reflection specialist sub-agent format
- Added explanation that reflection specialist is automatically spawned
- Emphasized local-first approach with optional cloud enhancement

### Documentation
- Improved clarity on how sub-agents appear in Claude's interface
- Better example using FastEmbed vs cloud decision instead of generic JWT auth

## [2.3.5] - 2025-07-27

### Changed
- Made technical documentation more neutral between local and cloud embedding options
- Removed promotional language about Voyage AI's cost-effectiveness
- Presented both embedding options equally without bias

## [2.3.4] - 2025-07-27

### Added
- Comprehensive embedding mode documentation
- Migration guide for switching between local and cloud modes
- Enhanced setup wizard with prominent embedding choice warnings
- Confirmation prompt for embedding mode selection

### Changed
- Setup wizard now clearly explains that embedding choice is semi-permanent
- Updated all documentation to emphasize the complexity of switching modes
- Improved troubleshooting guide with embedding mode issues section

### Documentation
- Created detailed embedding migration guide
- Updated installation guide with embedding mode selection
- Enhanced advanced usage guide with technical embedding details
- Added warnings throughout docs about mode switching implications

## [2.3.3] - 2025-07-27

### 🔒 Security Release

### Changed
- **BREAKING**: Complete migration from sentence-transformers to FastEmbed for local embeddings
- **Default Mode**: Local embeddings now default for privacy (no external API calls)
- **Docker Memory**: Increased container memory limits to 2GB for stability
- **Security Improvements**:
  - Fixed command injection vulnerabilities in installer
  - Patched vulnerable dependencies (pydantic CVE-2024-3772)
  - Enhanced configuration security

### Added
- **Local Embeddings by Default**: Uses FastEmbed with sentence-transformers/all-MiniLM-L6-v2 model
- **Unified Import System**: Single import script supports both local and cloud embeddings
- **JSONL Parser Fix**: Proper line-by-line parsing for Claude conversation files
- **Enhanced Documentation**:
  - Privacy mode comparison table in README
  - Security update notice with GitHub info block
  - Clear warnings about data exchange in cloud mode

### Fixed
- Import failures due to incorrect JSON parsing (JSONL format)
- Memory exhaustion in Docker containers when processing large files
- MCP server initialization with local embeddings
- Reflection specialist agent support for both embedding modes

### Security
- Environment variable configuration for all sensitive settings
- Local-first approach for privacy-conscious users
- Enhanced security scanning in CI/CD pipeline

## [2.3.0] - Unreleased

### Added
- **Setup Wizard Command-Line Arguments** - Non-interactive installation support
  - `--voyage-key=<key>` for direct API key configuration
  - `--local` flag for local-only mode without semantic search
  - Automatic detection of non-interactive environments (no TTY)
- **Watcher Integration** - Continuous conversation monitoring built into setup
  - Automatic file watching after initial import
  - Real-time conversation indexing
  - Configurable watch intervals
- **System Health Dashboard** - Comprehensive health monitoring (`health-check.sh`)
  - Qdrant status with vector and collection counts
  - MCP server connection status
  - Import queue and last import timing
  - Docker container resource usage
  - Automatic update checking
  - Search performance metrics
- **Enhanced Docker Support**
  - Isolated importer container (`Dockerfile.importer-isolated`)
  - Streaming importer with progress updates (`Dockerfile.streaming-importer`)
  - Watcher container for continuous monitoring (`Dockerfile.watcher`)
  - Optimized docker-compose configurations
- **Python SSL Support** - Improved handling of Python installations
  - Automatic detection of pyenv SSL issues
  - Fallback to Homebrew Python on macOS
  - Clear error messages and remediation steps

### Changed
- **Setup Wizard Improvements**
  - Better error handling for missing dependencies
  - Clearer prompts and user feedback
  - Support for both interactive and non-interactive modes
  - Automatic Python path detection and configuration
- **Documentation Structure**
  - Reorganized docs into logical categories
  - Added troubleshooting guides for common issues
  - Improved installation instructions with platform-specific notes
  - Better examples and use cases
- **Import Process**
  - Streaming import with real-time progress
  - Better handling of large conversation files
  - Improved error recovery and retry logic
  - More efficient vector embedding batch processing

### Fixed
- **Reddit User Feedback Issues**
  - Python SSL module errors with pyenv installations
  - Non-interactive installation failures in CI/CD environments
  - Qdrant health check endpoint (changed from `/health` to `/`)
  - Docker container permission issues on Linux
  - Memory leaks in long-running import processes
- **CI/CD Pipeline**
  - Updated workflows for Python-based structure
  - Fixed test paths and dependencies
  - Added Python version matrix testing (3.10, 3.11, 3.12)
- **Setup Wizard Bugs**
  - Fixed readline interface errors in non-TTY environments
  - Corrected Python path detection on various platforms
  - Improved error messages for missing dependencies

### Security
- Better isolation of import processes using Docker
- Improved handling of sensitive configuration

## [2.2.1] - 2025-01-25

### Fixed
- NPM package installation issues
- Missing dependencies in package.json

## [2.2.0] - 2025-01-24

### Added
- Voyage AI streaming embeddings support
- Improved import performance (2x faster)
- Better error handling in setup wizard

### Changed
- Default to Voyage AI for better search accuracy
- Simplified installation process

## [2.1.0] - 2025-01-23

### Added
- Support for multiple embedding providers
- Local embedding model option
- Cross-project search functionality

### Fixed
- Memory usage optimization for large conversation sets
- Import speed improvements

## [2.0.1] - 2025-01-22

### Fixed
- CI/CD workflow for Python-based structure
- Updated test configurations
- Fixed directory references in workflows

## [2.0.0] - 2025-01-22

### Changed
- **BREAKING**: Complete restructure from TypeScript to Python MCP server
- **BREAKING**: NPM package now serves as installation wizard only
- **BREAKING**: Renamed MCP from `claude-self-reflection` to `claude-self-reflect`
- Archived TypeScript implementation to `archived/typescript-mcp/`
- Renamed directories for clarity:
  - `claude-reflect` → `mcp-server`
  - `claude_reflect` → `src`
- Simplified configuration structure

### Added
- New installation CLI with interactive setup wizard
- `claude-self-reflect doctor` command for diagnostics
- Python wheel distribution for MCP server
- Comprehensive migration guide

### Removed
- TypeScript MCP server implementation (archived)
- Direct MCP functionality from NPM package

### Migration Guide
Users upgrading from v1.x need to:
1. Uninstall the old package: `npm uninstall -g claude-self-reflection`
2. Install the new package: `npm install -g claude-self-reflect`
3. Run setup wizard: `claude-self-reflect setup`
4. Update Claude Desktop configuration to use `claude-self-reflect` instead of `claude-self-reflection`

## [1.0.0] - 2025-01-14

### 🎉 Initial Release

#### Added
- **One-command installation** - Get up and running in under 5 minutes
- **Semantic search** across all Claude Desktop conversations
- **Continuous import** - Automatically watches for new conversations
- **Multiple embedding providers** - Support for OpenAI, Voyage AI, and local models
- **Cross-project search** - Search across all your Claude projects simultaneously
- **Privacy-first design** - All data stays local, no cloud dependencies
- **MCP-native integration** - Built specifically for Claude Desktop
- **Comprehensive documentation** - Full setup guide and troubleshooting
- **Health monitoring** - Built-in health check and status commands
- **Backup/restore** functionality for data safety
- **Docker-based deployment** for easy setup and isolation

#### Performance
- 66.1% search accuracy with Voyage AI embeddings
- <100ms search latency for 100K+ conversations
- ~1,000 conversations/minute import speed
- ~1GB memory usage per million conversations

#### Supported Platforms
- macOS (Apple Silicon and Intel)
- Linux (Ubuntu 20.04+, Debian 11+)
- Windows (via WSL2)

#### Known Issues
- Large conversation files (>10MB) may slow down initial import
- Search accuracy varies by embedding model choice
- Windows native support requires WSL2

### Migration from Neo4j
If you're migrating from the original Neo4j-based memento-stack:
1. Export your data using the old system's export function
2. Install Claude Self-Reflect
3. Import will automatically process your conversation history
4. All conversation data is preserved with improved search capabilities

---

## Contributing

We welcome contributions! See our [Contributing Guide](CONTRIBUTING.md) for details.

To add to this changelog:
1. Add your changes under the [Unreleased] section
2. Use the appropriate category: Added, Changed, Deprecated, Removed, Fixed, Security
3. Include PR numbers and contributor credits

Example:
```
### Added
- New feature description (#123) - @contributor
```
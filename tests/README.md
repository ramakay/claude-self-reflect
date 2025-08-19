# Claude Self-Reflect Test Suite

Comprehensive test coverage for all Claude Self-Reflect features and scenarios.

## Quick Start

```bash
# Run all tests
python tests/run_all_tests.py

# Run specific test categories
python tests/run_all_tests.py -c mcp_tools memory_decay

# Run with verbose output
python tests/run_all_tests.py -v

# List available test categories
python tests/run_all_tests.py --list
```

## Test Categories

### Core Feature Tests

#### 1. MCP Tool Integration (`test_mcp_tools_comprehensive.py`)
Tests all Model Context Protocol tools with various parameters and edge cases.

**Coverage:**
- `reflect_on_past` with all parameters (limit, brief, min_score, use_decay, etc.)
- `store_reflection` with tags and content validation
- `quick_search` for rapid lookups
- `search_summary` for aggregated insights
- `get_more_results` for pagination
- `search_by_file` and `search_by_concept`
- Edge cases: empty queries, invalid parameters, cross-project search

**Run:** `python tests/test_mcp_tools_comprehensive.py`

#### 2. Memory Decay (`test_memory_decay.py`)
Tests time-based decay calculations for prioritizing recent content.

**Coverage:**
- Decay calculations with various time periods (0-730 days)
- Half-life parameter variations (7-365 days)
- Score adjustments with decay enabled/disabled
- Ranking changes due to decay
- Performance impact measurements
- Configuration persistence

**Run:** `python tests/test_memory_decay.py`

#### 3. Multi-Project Support (`test_multi_project.py`)
Tests project isolation and cross-project search capabilities.

**Coverage:**
- Project name normalization (handles special characters, paths)
- Collection isolation verification
- Cross-project search with `project="all"`
- Collection naming consistency
- Project discovery from existing collections
- Project metadata storage and retrieval
- Collection limits and edge cases

**Run:** `python tests/test_multi_project.py`

### Advanced Feature Tests

#### 4. Embedding Models (`test_embedding_models.py`)
Tests switching between local and cloud embedding models.

**Coverage:**
- FastEmbed (local, 384 dimensions) vs Voyage AI (cloud, 1024 dimensions)
- Model switching logic
- Dimension compatibility checks
- Embedding quality comparisons
- Performance differences
- Fallback mechanisms

#### 5. Delta Metadata Updates (`test_delta_metadata.py`)
Tests incremental metadata updates without re-embedding.

**Coverage:**
- Tool usage extraction from conversations
- File reference tracking
- Concept identification
- Incremental update performance
- State tracking and recovery

#### 6. Performance & Load (`test_performance_load.py`)
Tests system performance under various load conditions.

**Coverage:**
- Large conversation imports (>1000 chunks)
- Concurrent search requests
- Memory usage patterns
- CPU utilization monitoring
- Queue overflow handling
- Response time benchmarks

### Reliability Tests

#### 7. Data Integrity (`test_data_integrity.py`)
Tests data consistency and correctness.

**Coverage:**
- Duplicate detection and handling
- Conversation ID consistency
- Chunk ordering preservation
- Unicode and special character handling
- Collection consistency checks
- Point count verification

#### 8. Recovery Scenarios (`test_recovery_scenarios.py`)
Tests system resilience and recovery capabilities.

**Coverage:**
- Partial import recovery
- Corrupted state file handling
- Network interruption resilience
- Docker container restarts
- Collection restoration
- Graceful degradation

#### 9. Security (`test_security.py`)
Tests security measures and input validation.

**Coverage:**
- API key validation and storage
- Input sanitization
- Path traversal prevention
- Resource limits enforcement
- Sensitive data handling
- Access control verification

## Existing Integration Tests

The following tests from the `scripts/` directory are also included:

- **E2E Import** (`test-e2e-import.py`): End-to-end conversation import
- **MCP Search** (`test-mcp-search.py`): Basic MCP search functionality
- **Search Functionality** (`test-search-functionality.py`): Semantic search accuracy
- **Streaming Importer** (`test-streaming-importer-e2e.py`): Streaming import E2E

## Test Infrastructure

### Prerequisites
- Docker and Docker Compose running
- Qdrant container active
- Python 3.10+ with virtual environment
- Required packages: `qdrant-client`, `fastembed`, `fastmcp`

### Test Runner Features
- Parallel test execution where possible
- Detailed result reporting
- JSON output for CI/CD integration
- Performance metrics tracking
- Automatic cleanup of test data

### Environment Variables
```bash
# Control test behavior
VERBOSE=1                    # Detailed output
USE_DECAY=0                 # Disable memory decay
PREFER_LOCAL_EMBEDDINGS=true # Use local embeddings
QDRANT_URL=http://localhost:6333
```

## CI/CD Integration

Tests are automatically run on:
- Push to main branch
- Pull requests
- Release creation

GitHub Actions workflow: `.github/workflows/ci.yml`

## Test Results

Results are saved to `tests/test_results.json` with:
- Timestamp
- Duration for each test
- Pass/fail status
- Error messages if any
- Performance metrics

## Contributing Tests

When adding new features, include corresponding tests:

1. Create test file in `tests/` directory
2. Follow naming convention: `test_<feature>.py`
3. Include in `TEST_SUITES` dict in `run_all_tests.py`
4. Update relevant agent documentation
5. Add to this README

## Troubleshooting

### Common Issues

**Qdrant not running:**
```bash
docker compose up -d qdrant
```

**Import errors:**
```bash
cd ~/projects/claude-self-reflect
source venv/bin/activate
pip install -r requirements.txt
```

**MCP tools not available:**
```bash
claude mcp remove claude-self-reflect
claude mcp add claude-self-reflect "/path/to/mcp-server/run-mcp.sh" -e QDRANT_URL="http://localhost:6333" -s user
# Restart Claude Code
```

## Performance Benchmarks

Expected performance on standard hardware:

- MCP tool response: <100ms
- Semantic search (10 results): <200ms
- Cross-project search: <500ms
- Import 100 conversations: <30s
- Memory decay calculation (1000 items): <10ms

## Test Coverage Goals

- Unit test coverage: >80%
- Integration test coverage: >70%
- Critical path coverage: 100%
- Edge case coverage: >90%

---

For questions or issues, see the main [README](../README.md) or open an issue on GitHub.
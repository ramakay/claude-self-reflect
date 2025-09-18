# Unified State Management Test Suite

This comprehensive test suite validates the unified state management system (`scripts/unified_state_manager.py`) across all critical scenarios including migration, concurrency, performance, security, and cross-platform compatibility.

## Overview

The test suite includes **7 major test categories** with **40+ individual test methods** covering:

### 1. Migration Tests (`TestMigration`)
- **Dry-run migration detection** - Validates migration needs without file changes
- **Actual migration from v3.x** - Tests real migration from older state formats
- **Rollback functionality** - Ensures failed migrations can be reverted
- **Data preservation** - Verifies all data survives migration intact

### 2. Concurrency Tests (`TestConcurrency`)
- **Multiple readers** - Tests concurrent read operations don't interfere
- **Multiple writers with locking** - Validates file locking prevents corruption
- **Race condition handling** - Tests mixed reader/writer scenarios
- **Lock timeout and retry** - Validates timeout mechanism works correctly

### 3. Performance Tests (`TestPerformance`)
- **Status check speed** - Ensures status checks complete under 20ms
- **Large state files** - Tests performance with 1000+ file entries
- **Memory usage validation** - Monitors memory consumption stays reasonable
- **Benchmark vs old approach** - Compares performance to legacy multi-file system

### 4. Security Tests (`TestSecurity`)
- **Path traversal protection** - Validates defense against `../../../etc/passwd` attacks
- **Lock expiry mechanism** - Tests automatic cleanup of expired locks
- **Input validation** - Comprehensive validation of all input parameters
- **Safe JSON serialization** - Ensures datetime/Path objects serialize safely

### 5. Cross-Platform Tests (`TestCrossPlatform`)
- **Unix file locking** - Tests fcntl-based locking on Unix systems
- **Windows file locking** - Tests msvcrt-based locking on Windows
- **Docker path normalization** - Validates Docker-to-local path mapping
- **Platform-specific atomic writes** - Tests atomic operations across platforms

### 6. Integration Tests (`TestIntegration`)
- **Batch importer integration** - Tests workflow with batch import system
- **Streaming watcher integration** - Tests workflow with streaming file watcher
- **Status checking integration** - Validates status API works with real data
- **Error recovery scenarios** - Tests retry and failure handling

### 7. Edge Cases (`TestEdgeCases`)
- **Empty/corrupted/missing files** - Handles malformed state files gracefully
- **Network/disk failures** - Tests resilience to infrastructure issues
- **Cleanup operations** - Tests old entry cleanup with concurrent access
- **Unicode paths** - Handles international characters in file paths
- **Large chunk counts** - Tests with very large numbers

## Quick Start

### Install Dependencies
```bash
# Install test requirements
pip install -r tests/requirements-test.txt

# Or install minimal requirements
pip install pytest psutil mock
```

### Run All Tests
```bash
python tests/run_unified_state_tests.py
```

### Run Specific Test Categories
```bash
# Migration tests only
python tests/run_unified_state_tests.py migration

# Performance tests only
python tests/run_unified_state_tests.py performance

# Quick tests only (skip slow ones)
python tests/run_unified_state_tests.py --quick

# Generate detailed report
python tests/run_unified_state_tests.py --report
```

### Run All Scenarios with Summary
```bash
python tests/run_unified_state_tests.py --scenarios
```

## Test Commands Reference

| Command | Description |
|---------|-------------|
| `python tests/run_unified_state_tests.py` | Run all tests |
| `python tests/run_unified_state_tests.py migration` | Migration tests only |
| `python tests/run_unified_state_tests.py concurrency` | Concurrency tests only |
| `python tests/run_unified_state_tests.py performance` | Performance tests only |
| `python tests/run_unified_state_tests.py security` | Security tests only |
| `python tests/run_unified_state_tests.py cross-platform` | Cross-platform tests only |
| `python tests/run_unified_state_tests.py integration` | Integration tests only |
| `python tests/run_unified_state_tests.py edge-cases` | Edge case tests only |
| `python tests/run_unified_state_tests.py --quick` | Quick tests (no slow tests) |
| `python tests/run_unified_state_tests.py --report` | Generate HTML report |
| `python tests/run_unified_state_tests.py --scenarios` | All scenarios with summary |

## Direct pytest Usage

You can also run tests directly with pytest:

```bash
# Run all unified state tests
pytest tests/test_unified_state.py -v

# Run specific test class
pytest tests/test_unified_state.py::TestMigration -v

# Run specific test method
pytest tests/test_unified_state.py::TestConcurrency::test_multiple_writers_with_locking -v

# Run with coverage
pytest tests/test_unified_state.py --cov=scripts.unified_state_manager --cov-report=html
```

## Test Data and Fixtures

The test suite uses pytest fixtures for:
- **Temporary directories** - Isolated test environments
- **State managers** - Pre-configured UnifiedStateManager instances
- **Sample state data** - v3.x and v5.0 state examples for migration testing
- **Mock objects** - Simulated file systems and network conditions

## Performance Benchmarks

The performance tests validate:
- **Status checks** complete in under 20ms
- **Large state files** (1000+ entries) read in under 1 second
- **Memory usage** increases by less than 50MB for 500 files
- **Write operations** are competitive with legacy multi-file approach

## Security Validations

The security tests protect against:
- **Path traversal attacks** - `../../../etc/passwd`, `..\\..\\windows\\system32`
- **Lock hijacking** - Expired locks are automatically cleared
- **Input injection** - All parameters validated and sanitized
- **Serialization attacks** - Safe JSON handling for all data types

## Concurrency Safety

The concurrency tests verify:
- **Multiple readers** can access state simultaneously without corruption
- **File locking** prevents write conflicts between processes
- **Race conditions** are handled gracefully in mixed read/write scenarios
- **Lock timeouts** prevent deadlocks and ensure progress

## Cross-Platform Compatibility

Tests validate compatibility across:
- **Unix systems** - fcntl-based file locking
- **Windows systems** - msvcrt-based file locking
- **Docker environments** - Path normalization between container and host
- **Different filesystems** - NTFS, ext4, APFS, etc.

## Error Recovery

The test suite validates robust error handling for:
- **Corrupted state files** - Graceful degradation and recovery
- **Network failures** - Appropriate error handling and retries
- **Disk full conditions** - Safe failure without data loss
- **Process interruption** - Lock cleanup and state consistency

## Integration Points

Tests validate integration with:
- **Batch import system** - Proper state tracking for bulk operations
- **Streaming file watcher** - Real-time state updates
- **Status API** - Accurate reporting across all importers
- **Collection management** - Qdrant collection tracking

## Test Coverage Goals

The test suite aims for:
- **95%+ line coverage** of unified_state_manager.py
- **100% branch coverage** of critical paths (locking, migration, validation)
- **Platform coverage** across Windows, macOS, and Linux
- **Python version coverage** for 3.8+ (project requirement)

## Troubleshooting Tests

### Common Issues

1. **Permission Errors**
   ```bash
   # Ensure test directory is writable
   chmod 755 tests/
   ```

2. **Missing Dependencies**
   ```bash
   # Install all test requirements
   pip install -r tests/requirements-test.txt
   ```

3. **Platform-Specific Failures**
   ```bash
   # Skip platform-specific tests
   pytest tests/test_unified_state.py -k "not windows and not unix"
   ```

4. **Slow Test Timeouts**
   ```bash
   # Run only quick tests
   python tests/run_unified_state_tests.py --quick
   ```

### Debug Mode

For detailed debugging:
```bash
# Run with maximum verbosity
pytest tests/test_unified_state.py -vv --tb=long --capture=no

# Run single test with debugging
pytest tests/test_unified_state.py::TestMigration::test_actual_migration_from_v3 -vv -s
```

## Continuous Integration

For CI/CD pipelines:
```bash
# Quick validation
python tests/run_unified_state_tests.py --quick

# Full test suite with reporting
python tests/run_unified_state_tests.py --report --scenarios
```

The test suite is designed to be robust, comprehensive, and suitable for both development validation and production deployment verification.
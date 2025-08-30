# HOT/WARM/COLD Prioritization System

## Overview

The Claude Self-Reflect streaming watcher implements an intelligent file prioritization system based on "temperature" - how recently a file was modified. This ensures that your most recent conversations are imported almost immediately while preventing older files from being completely starved of processing.

## Temperature Categories

### 🔥 HOT Files (< 5 minutes old)
- **Priority**: Highest (priority score 0-9999)
- **Processing Interval**: 2 seconds
- **Behavior**: System switches to rapid 2-second check intervals when HOT files are detected
- **Use Case**: Active conversations that need near real-time indexing

### 🌡️ WARM Files (5 minutes - 24 hours old)
- **Priority**: Medium (priority score 20000-39999)
- **Processing Interval**: 60 seconds (configurable)
- **Special Cases**:
  - **URGENT_WARM**: Files waiting > 30 minutes get promoted to prevent starvation (priority score 10000-19999)
  - **Current Project Boost**: WARM files from your current working directory get slight priority boost
- **Use Case**: Recent conversations that should be indexed soon

### ❄️ COLD Files (> 24 hours old)
- **Priority**: Lowest (priority score 40000+)
- **Processing Limit**: Maximum 5 COLD files per cycle
- **Behavior**: Batch processed to prevent blocking newer content
- **Use Case**: Historical conversations for completeness

## Configuration

Environment variables control the temperature thresholds:

```bash
# Temperature windows
HOT_WINDOW_MINUTES=5        # Files younger than this are HOT
WARM_WINDOW_HOURS=24        # Files younger than this (but older than HOT) are WARM
MAX_WARM_WAIT_MINUTES=30    # WARM files waiting longer become URGENT_WARM

# Processing controls
HOT_CHECK_INTERVAL_S=2       # Interval when HOT files detected
IMPORT_FREQUENCY=60          # Normal check interval (seconds)
MAX_COLD_FILES=5             # Max COLD files per processing cycle
```

## How It Works

### 1. File Discovery
The watcher scans `~/.claude/projects/` for new or modified JSONL files every cycle.

### 2. Categorization
Each file is categorized based on its modification time:
```python
age_minutes = (now - file_mtime) / 60

if age_minutes < HOT_WINDOW_MINUTES:
    category = HOT
elif age_minutes < WARM_WINDOW_HOURS * 60:
    category = WARM
    if wait_time > MAX_WARM_WAIT_MINUTES:
        category = URGENT_WARM  # Starvation prevention
else:
    category = COLD
```

### 3. Priority Scoring
Files get a priority score (lower = higher priority):
- Base priority: HOT=0, URGENT_WARM=1, WARM=2-3, COLD=4
- Fine-tuning: Add age in minutes (capped at 9999) for tie-breaking
- Result: `priority = base * 10000 + min(age_minutes, 9999)`

### 4. Queue Management
The priority queue ensures:
- **HOT and URGENT_WARM files jump to front** using `deque.appendleft()`
- **Duplicate prevention** via tracking queued files
- **COLD file limiting** to prevent overwhelming the system
- **Memory efficiency** through bounded queue size

### 5. Dynamic Interval Switching
When HOT files are detected (either new or in queue):
- System switches to 2-second check intervals
- Returns to normal 60-second intervals when no HOT files remain
- Logging only occurs on mode changes to reduce noise

## Starvation Prevention

The system prevents WARM files from waiting indefinitely:
1. Tracks when each file was first seen
2. Calculates wait time for WARM files
3. Promotes to URGENT_WARM after 30 minutes
4. URGENT_WARM files get second-highest priority (after HOT)

## Memory Management

The implementation includes several memory-conscious features:
- **Cleanup of tracking data** after successful processing
- **Bounded queue** with overflow handling
- **Deduplication** to prevent multiple entries
- **Streaming processing** to avoid loading entire files

## Architecture Benefits

This design balances several competing concerns:
- **Responsiveness**: HOT files processed within 2-3 seconds
- **Fairness**: No file waits forever due to starvation prevention
- **Efficiency**: COLD files batch-processed to reduce overhead
- **Scalability**: Bounded resources prevent memory/CPU exhaustion
- **Observability**: Clear logging of temperature distribution

## Comparison with Alternative Approaches

### Why Not Use watchdog/inotify?
- Our use case involves periodic scanning of accumulated conversation files
- Temperature-based prioritization is domain-specific
- The combination of scanning + custom prioritization is appropriate

### Why Not Pure Priority Queue?
- Would starve WARM/COLD files indefinitely
- No consideration for wait times
- Our approach adds aging and quotas for fairness

### Why Not FIFO?
- Would process old files before recent conversations
- Poor user experience for active sessions
- Our approach ensures recent content is searchable quickly

## Testing

Run the comprehensive test suite:
```bash
python tests/test_hot_warm_cold.py
```

This tests:
- Correct categorization by age
- Priority ordering
- Starvation prevention
- Queue deduplication
- COLD file limiting
- Interval switching

## Monitoring

The system provides detailed metrics:
- File counts by temperature category
- Queue depth and processing rates
- Memory usage and CPU utilization
- Oldest file age (backlog monitoring)
- Progress toward 100% indexing

## Future Enhancements

Potential improvements identified but not yet implemented:
- Adaptive intervals based on system load
- Per-project temperature thresholds
- Machine learning for predicting file importance
- Integration with watchdog for true event-driven processing
- Distributed processing for large installations
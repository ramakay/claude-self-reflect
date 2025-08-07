# Docker Subprocess Memory Management Fix

## Problem Statement

The import watcher container was experiencing consistent OOM (Out of Memory) kills with exit code -9, despite using only 137MB of the allocated 2GB memory limit. This was causing import failures and preventing the system from processing new conversations.

## Root Cause Analysis

### The Subprocess Memory Accounting Issue

Docker's memory cgroup accounting treats subprocess memory differently than expected:

1. **Parent Process**: import-watcher.py uses ~7MB idle, ~15MB during execution
2. **Child Process**: When spawning import script via subprocess, Docker's memory accounting can trigger OOM killer
3. **Key Factor**: Using `capture_output=True` in subprocess.run() exacerbates the issue

### Why This Happens

```python
# Problematic code
result = subprocess.run(cmd, capture_output=True, text=True)
```

When `capture_output=True` is used:
- Python creates pipes for stdout/stderr
- Docker's cgroup memory accounting includes these buffers
- Memory spike during subprocess creation triggers OOM killer
- Actual memory usage remains low (137MB) but accounting thinks it's exceeded

## Solution Architecture

### Short-term Fix (Implemented in v2.4.15)

```python
# Remove output capture
result = subprocess.run(cmd)  # No capture_output
```

This reduces memory accounting overhead but doesn't solve the fundamental issue.

### Long-term Solution (Planned for v2.5.0)

#### Option 1: Direct Import (Recommended)

Eliminate subprocess entirely by importing directly in the watcher:

```python
# Instead of subprocess
import import_conversations_unified

class ImportWatcher:
    def process_file(self, file_path):
        # Direct function call instead of subprocess
        import_conversations_unified.import_file(file_path)
```

**Advantages**:
- No subprocess overhead
- Better error handling
- Shared memory space
- More efficient

#### Option 2: Threading Instead of Subprocess

Use threading for parallel processing:

```python
import threading
from concurrent.futures import ThreadPoolExecutor

class ImportWatcher:
    def __init__(self):
        self.executor = ThreadPoolExecutor(max_workers=2)
    
    def process_files(self, files):
        futures = []
        for file in files:
            future = self.executor.submit(self.import_file, file)
            futures.append(future)
```

**Advantages**:
- Shared memory space
- No subprocess overhead
- Better resource utilization

#### Option 3: Message Queue Architecture

Decouple watching from importing:

```yaml
# docker-compose.yaml
services:
  watcher:
    # Only watches for changes, publishes to queue
    
  importer:
    # Separate service that consumes from queue
    
  redis:
    # Message broker
```

**Advantages**:
- Complete isolation
- Scalable
- Can restart services independently

## Implementation Guidelines

### For Immediate Relief

1. **Increase Memory Limit**
   ```yaml
   watcher:
     mem_limit: 2g  # Increased from 1g
     memswap_limit: 2g
   ```

2. **Remove Output Capture**
   ```python
   # Avoid capture_output=True
   result = subprocess.run(cmd)
   ```

3. **Add Monitoring**
   ```bash
   # Monitor real memory usage
   docker stats watcher --no-stream
   ```

### For Permanent Fix

1. **Refactor Import Architecture**
   - Move import logic into watcher process
   - Use function calls instead of subprocess
   - Implement proper error boundaries

2. **Add Health Checks**
   ```yaml
   healthcheck:
     test: ["CMD", "python", "-c", "import psutil; exit(0 if psutil.virtual_memory().percent < 80 else 1)"]
     interval: 30s
     timeout: 10s
     retries: 3
   ```

3. **Implement Graceful Degradation**
   - Batch size reduction under memory pressure
   - Automatic garbage collection triggers
   - Memory usage monitoring

## Docker-Specific Considerations

### Cgroup v1 vs v2

Different Docker versions handle memory differently:
- **Cgroup v1**: More aggressive memory accounting
- **Cgroup v2**: Better subprocess handling

Check your version:
```bash
docker info | grep "Cgroup"
```

### Memory Overcommit

Enable memory overcommit for development:
```bash
# Not recommended for production
echo 1 > /proc/sys/vm/overcommit_memory
```

### Swap Configuration

Ensure swap is properly configured:
```yaml
watcher:
  mem_limit: 2g
  memswap_limit: 2g  # Same as mem_limit = no swap
  # Or
  memswap_limit: 4g  # 2g memory + 2g swap
```

## Testing Strategy

### Memory Stress Testing

```python
# test_memory_stress.py
import psutil
import subprocess
import time

def test_subprocess_memory():
    """Test subprocess memory usage patterns"""
    process = psutil.Process()
    
    # Baseline memory
    baseline = process.memory_info().rss / 1024 / 1024
    print(f"Baseline: {baseline:.2f} MB")
    
    # Test with capture_output
    result = subprocess.run(
        ["python", "-c", "print('x' * 1000000)"],
        capture_output=True
    )
    peak_with_capture = process.memory_info().rss / 1024 / 1024
    print(f"With capture: {peak_with_capture:.2f} MB")
    
    # Test without capture_output
    result = subprocess.run(["python", "-c", "print('x' * 1000000)"])
    peak_without_capture = process.memory_info().rss / 1024 / 1024
    print(f"Without capture: {peak_without_capture:.2f} MB")
```

### Container Testing

```bash
# Test container memory limits
docker run --rm -it --memory=256m --memory-swap=256m \
  python:3.12-slim python -c "
import subprocess
# This should fail with OOM
subprocess.run(['python', '-c', 'print(1)'], capture_output=True)
"
```

## Monitoring and Alerts

### Real-time Monitoring Script

```bash
#!/bin/bash
# monitor-import-memory.sh

while true; do
    echo "=== $(date) ==="
    docker stats watcher --no-stream --format "table {{.Container}}\t{{.MemUsage}}\t{{.MemPerc}}"
    
    # Check for OOM kills
    docker inspect watcher | grep -A5 "OOMKilled"
    
    # Check dmesg for OOM messages
    dmesg | grep -i "killed process" | tail -5
    
    sleep 5
done
```

### Prometheus Metrics

```python
# Add to import-watcher.py
from prometheus_client import Gauge, Counter

memory_usage = Gauge('import_watcher_memory_bytes', 'Memory usage in bytes')
import_success = Counter('import_success_total', 'Successful imports')
import_failure = Counter('import_failure_total', 'Failed imports')

# Update metrics
memory_usage.set(psutil.Process().memory_info().rss)
```

## Best Practices

1. **Always Monitor Memory Usage**
   - Use `docker stats` during development
   - Set up alerts for production
   - Track memory trends over time

2. **Avoid Subprocess When Possible**
   - Direct function calls are more efficient
   - Use threading for parallelism
   - Consider async/await for I/O operations

3. **Handle Memory Pressure Gracefully**
   - Implement backpressure mechanisms
   - Reduce batch sizes under pressure
   - Add circuit breakers

4. **Test with Realistic Data**
   - Use production-sized files
   - Test with memory constraints
   - Simulate long-running scenarios

## References

- [Docker Memory Management](https://docs.docker.com/config/containers/resource_constraints/)
- [Python Subprocess Memory Usage](https://docs.python.org/3/library/subprocess.html#security-considerations)
- [Cgroup Memory Accounting](https://www.kernel.org/doc/Documentation/cgroup-v1/memory.txt)
- [OOM Killer Behavior](https://www.kernel.org/doc/gorman/html/understand/understand016.html)
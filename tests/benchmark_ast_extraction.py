#!/usr/bin/env python3
"""
Performance benchmark for AST pattern extraction
Tests speed and resource usage with various conversation sizes
"""

import sys
import os
import time
import psutil
import asyncio
from pathlib import Path
from typing import List, Dict, Any
import statistics

# Add scripts directory to path
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "scripts"))

from ast_pattern_extractor import extract_code_patterns, AST_GREP_AVAILABLE

# Test data samples
SMALL_CODE = """
```javascript
const x = 5;
console.log(x);
```
"""

MEDIUM_CODE = """
```javascript
import React, { useState, useEffect } from 'react';

function TodoList() {
    const [todos, setTodos] = useState([]);
    const [loading, setLoading] = useState(true);
    
    useEffect(() => {
        async function fetchTodos() {
            try {
                const response = await fetch('/api/todos');
                const data = await response.json();
                setTodos(data);
            } catch (error) {
                console.error('Failed to fetch todos:', error);
            } finally {
                setLoading(false);
            }
        }
        
        fetchTodos();
    }, []);
    
    return (
        <div>
            {loading ? 'Loading...' : todos.map(todo => <li>{todo}</li>)}
        </div>
    );
}
```

```python
import asyncio

async def process_batch(items):
    results = []
    async with aiohttp.ClientSession() as session:
        for item in items:
            try:
                result = await process_item(session, item)
                results.append(result)
            except Exception as e:
                logger.error(f"Failed: {e}")
    return results
```
"""

LARGE_CODE = MEDIUM_CODE * 5  # Repeat to simulate larger conversation


def benchmark_extraction(text: str, label: str, iterations: int = 10) -> Dict[str, Any]:
    """Benchmark AST extraction performance"""
    
    times = []
    memory_usage = []
    process = psutil.Process()
    
    for i in range(iterations):
        # Memory before
        mem_before = process.memory_info().rss / 1024 / 1024  # MB
        
        # Time extraction
        start = time.perf_counter()
        result = extract_code_patterns(text, max_blocks=10)
        elapsed = time.perf_counter() - start
        
        # Memory after
        mem_after = process.memory_info().rss / 1024 / 1024  # MB
        
        times.append(elapsed)
        memory_usage.append(mem_after - mem_before)
        
        # Cool down between iterations
        time.sleep(0.1)
    
    return {
        "label": label,
        "iterations": iterations,
        "text_size_kb": len(text) / 1024,
        "extraction_method": result.get("extraction_method", "unknown"),
        "blocks_found": result.get("blocks_processed", 0),
        "patterns_found": len(result.get("code_patterns", {})),
        "avg_time_ms": statistics.mean(times) * 1000,
        "min_time_ms": min(times) * 1000,
        "max_time_ms": max(times) * 1000,
        "stddev_time_ms": statistics.stdev(times) * 1000 if len(times) > 1 else 0,
        "avg_memory_mb": statistics.mean(memory_usage),
        "max_memory_mb": max(memory_usage),
    }


async def benchmark_async_extraction(text: str, label: str, iterations: int = 10) -> Dict[str, Any]:
    """Benchmark async extraction with executor (as used in watcher)"""
    
    times = []
    memory_usage = []
    process = psutil.Process()
    loop = asyncio.get_running_loop()
    
    for i in range(iterations):
        # Memory before
        mem_before = process.memory_info().rss / 1024 / 1024  # MB
        
        # Time async extraction
        start = time.perf_counter()
        try:
            result = await asyncio.wait_for(
                loop.run_in_executor(
                    None, 
                    lambda: extract_code_patterns(text[:2_000_000], max_blocks=10)
                ),
                timeout=5.0
            )
        except asyncio.TimeoutError:
            result = {"extraction_method": "timeout"}
        elapsed = time.perf_counter() - start
        
        # Memory after
        mem_after = process.memory_info().rss / 1024 / 1024  # MB
        
        times.append(elapsed)
        memory_usage.append(mem_after - mem_before)
        
        # Cool down
        await asyncio.sleep(0.1)
    
    return {
        "label": f"{label} (async)",
        "iterations": iterations,
        "text_size_kb": len(text) / 1024,
        "extraction_method": result.get("extraction_method", "unknown"),
        "blocks_found": result.get("blocks_processed", 0),
        "patterns_found": len(result.get("code_patterns", {})),
        "avg_time_ms": statistics.mean(times) * 1000,
        "min_time_ms": min(times) * 1000,
        "max_time_ms": max(times) * 1000,
        "stddev_time_ms": statistics.stdev(times) * 1000 if len(times) > 1 else 0,
        "avg_memory_mb": statistics.mean(memory_usage),
        "max_memory_mb": max(memory_usage),
    }


def print_benchmark_results(results: List[Dict[str, Any]]):
    """Pretty print benchmark results"""
    
    print("\n" + "=" * 80)
    print("AST PATTERN EXTRACTION PERFORMANCE BENCHMARKS")
    print("=" * 80)
    
    if AST_GREP_AVAILABLE:
        print("✅ AST-grep is available and being used")
    else:
        print("⚠️  AST-grep not available - using regex fallback")
    
    print("\n📊 Results:\n")
    
    # Header
    print(f"{'Test':<20} {'Size':<10} {'Method':<12} {'Avg Time':<12} {'Std Dev':<10} {'Memory':<10}")
    print("-" * 80)
    
    for r in results:
        print(f"{r['label']:<20} "
              f"{r['text_size_kb']:.1f}KB".ljust(10) + " "
              f"{r['extraction_method']:<12} "
              f"{r['avg_time_ms']:.2f}ms".ljust(12) + " "
              f"±{r['stddev_time_ms']:.2f}ms".ljust(10) + " "
              f"{r['avg_memory_mb']:.2f}MB")
    
    print("\n📈 Pattern Detection:\n")
    for r in results:
        print(f"{r['label']:<20} Found {r['blocks_found']} blocks, {r['patterns_found']} pattern types")
    
    print("\n⚡ Performance Summary:")
    sync_times = [r['avg_time_ms'] for r in results if 'async' not in r['label']]
    async_times = [r['avg_time_ms'] for r in results if 'async' in r['label']]
    
    if sync_times:
        print(f"  Sync average: {statistics.mean(sync_times):.2f}ms")
    if async_times:
        print(f"  Async average: {statistics.mean(async_times):.2f}ms")
    
    # Check if performance is acceptable
    print("\n✅ Performance Validation:")
    all_under_100ms = all(r['avg_time_ms'] < 100 for r in results if r['text_size_kb'] < 10)
    all_under_500ms = all(r['avg_time_ms'] < 500 for r in results if r['text_size_kb'] < 50)
    
    if all_under_100ms:
        print("  ✅ Small conversations (<10KB) process in <100ms")
    else:
        print("  ⚠️  Some small conversations take >100ms")
    
    if all_under_500ms:
        print("  ✅ Medium conversations (<50KB) process in <500ms")
    else:
        print("  ⚠️  Some medium conversations take >500ms")
    
    print("=" * 80)


async def main():
    """Run all benchmarks"""
    
    results = []
    
    # Synchronous benchmarks
    print("Running synchronous benchmarks...")
    results.append(benchmark_extraction(SMALL_CODE, "Small (sync)", iterations=20))
    results.append(benchmark_extraction(MEDIUM_CODE, "Medium (sync)", iterations=10))
    results.append(benchmark_extraction(LARGE_CODE, "Large (sync)", iterations=5))
    
    # Async benchmarks (as used in watcher)
    print("Running async benchmarks...")
    results.append(await benchmark_async_extraction(SMALL_CODE, "Small", iterations=20))
    results.append(await benchmark_async_extraction(MEDIUM_CODE, "Medium", iterations=10))
    results.append(await benchmark_async_extraction(LARGE_CODE, "Large", iterations=5))
    
    # Very large text (stress test)
    print("Running stress test...")
    very_large = MEDIUM_CODE * 20  # ~30KB
    results.append(await benchmark_async_extraction(very_large, "Very Large", iterations=3))
    
    print_benchmark_results(results)


if __name__ == "__main__":
    asyncio.run(main())
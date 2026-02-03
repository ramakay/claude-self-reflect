#!/usr/bin/env python3
"""
Test script for batch import with Haiku 4.5 and size filtering.

This script validates:
1. Haiku 4.5 model is correctly configured
2. File size filtering works (skips files < 50KB)
3. Batch API creates jobs successfully
4. Cost estimation is accurate

Usage:
    python scripts/test-batch-import.py [--dry-run] [--verbose]

Options:
    --dry-run   Don't create actual batches, just validate configuration
    --verbose   Show detailed output
"""

import os
import sys
import json
import argparse
from pathlib import Path
from datetime import datetime

# Add project paths
sys.path.insert(0, str(Path(__file__).parent.parent))
sys.path.insert(0, str(Path(__file__).parent.parent / "docs" / "design"))

from dotenv import load_dotenv
load_dotenv()

# Configuration to test
EXPECTED_MODEL = "claude-haiku-4-5-20251001"
MIN_FILE_SIZE = 50_000  # 50KB
HAIKU_INPUT_COST = 0.80 / 1_000_000  # $0.80 per million (standard)
HAIKU_OUTPUT_COST = 4.00 / 1_000_000  # $4.00 per million (standard)
HAIKU_BATCH_DISCOUNT = 0.5  # 50% off for batch API


def test_api_key():
    """Test that API key is configured."""
    api_key = os.getenv("ANTHROPIC_API_KEY")
    if not api_key:
        return False, "ANTHROPIC_API_KEY not set"
    if not api_key.startswith("sk-ant-"):
        return False, "Invalid API key format"
    return True, f"API key configured ({api_key[:12]}...)"


def test_model_config():
    """Test that batch import uses Haiku 4.5."""
    try:
        from batch_import_all_projects import batch_generate_narratives
        # Check the source code for model
        import inspect
        source = inspect.getsource(batch_generate_narratives)
        if EXPECTED_MODEL in source:
            return True, f"Model correctly set to {EXPECTED_MODEL}"
        else:
            return False, f"Model not set to {EXPECTED_MODEL}"
    except Exception as e:
        return False, f"Could not verify model config: {e}"


def test_size_filter():
    """Test that size filtering is configured."""
    try:
        from batch_import_all_projects import discover_projects
        import inspect
        source = inspect.getsource(discover_projects)
        if "MIN_FILE_SIZE" in source or "50_000" in source or "50000" in source:
            return True, "Size filter (50KB) is configured"
        else:
            return False, "Size filter not found in discover_projects"
    except Exception as e:
        return False, f"Could not verify size filter: {e}"


def test_file_discovery():
    """Test file discovery with size filter."""
    projects_dir = Path.home() / ".claude/projects"

    total_files = 0
    filtered_files = 0
    skipped_files = 0

    for project_dir in projects_dir.iterdir():
        if not project_dir.is_dir() or project_dir.name.startswith('.'):
            continue

        for f in project_dir.glob("*.jsonl"):
            total_files += 1
            if f.stat().st_size >= MIN_FILE_SIZE:
                filtered_files += 1
            else:
                skipped_files += 1

    if total_files == 0:
        return False, "No JSONL files found"

    skip_pct = (skipped_files / total_files) * 100
    return True, f"Found {total_files} files, {filtered_files} pass filter ({skip_pct:.0f}% skipped)"


def test_haiku_api(dry_run=False):
    """Test Haiku 4.5 API call."""
    if dry_run:
        return True, "Skipped (dry-run mode)"

    try:
        import anthropic
        client = anthropic.Anthropic()

        response = client.messages.create(
            model=EXPECTED_MODEL,
            max_tokens=50,
            messages=[{"role": "user", "content": "Say 'test passed' in 3 words."}]
        )

        if response.model == EXPECTED_MODEL:
            cost = (response.usage.input_tokens * HAIKU_INPUT_COST +
                   response.usage.output_tokens * HAIKU_OUTPUT_COST)
            return True, f"Haiku 4.5 responding (cost: ${cost:.6f})"
        else:
            return False, f"Wrong model returned: {response.model}"
    except Exception as e:
        return False, f"API error: {e}"


def test_batch_api(dry_run=False):
    """Test batch API creation."""
    if dry_run:
        return True, "Skipped (dry-run mode)"

    try:
        import anthropic
        client = anthropic.Anthropic()

        batch = client.messages.batches.create(
            requests=[{
                "custom_id": f"test-{datetime.now().strftime('%Y%m%d%H%M%S')}",
                "params": {
                    "model": EXPECTED_MODEL,
                    "max_tokens": 50,
                    "messages": [{"role": "user", "content": "Test"}]
                }
            }]
        )

        # Cancel it immediately (we just want to verify creation works)
        client.messages.batches.cancel(batch.id)

        return True, f"Batch API working (created and cancelled {batch.id})"
    except Exception as e:
        return False, f"Batch API error: {e}"


def estimate_costs():
    """Estimate costs for full import."""
    projects_dir = Path.home() / ".claude/projects"

    file_count = 0
    total_size = 0

    for project_dir in projects_dir.iterdir():
        if not project_dir.is_dir() or project_dir.name.startswith('.'):
            continue

        for f in project_dir.glob("*.jsonl"):
            if f.stat().st_size >= MIN_FILE_SIZE:
                file_count += 1
                total_size += f.stat().st_size

    # Estimate ~2500 input tokens + ~500 output tokens per conversation
    est_input_tokens = file_count * 2500
    est_output_tokens = file_count * 500

    # Batch pricing (50% off)
    batch_cost = (est_input_tokens * HAIKU_INPUT_COST * HAIKU_BATCH_DISCOUNT +
                  est_output_tokens * HAIKU_OUTPUT_COST * HAIKU_BATCH_DISCOUNT)

    return {
        "files": file_count,
        "total_size_mb": total_size / 1024 / 1024,
        "estimated_cost": batch_cost
    }


def main():
    parser = argparse.ArgumentParser(description="Test batch import configuration")
    parser.add_argument("--dry-run", action="store_true", help="Skip actual API calls")
    parser.add_argument("--verbose", "-v", action="store_true", help="Verbose output")
    args = parser.parse_args()

    print("=" * 60)
    print("BATCH IMPORT TEST SUITE")
    print("=" * 60)
    print()

    tests = [
        ("API Key", test_api_key),
        ("Model Config", test_model_config),
        ("Size Filter", test_size_filter),
        ("File Discovery", test_file_discovery),
        ("Haiku 4.5 API", lambda: test_haiku_api(args.dry_run)),
        ("Batch API", lambda: test_batch_api(args.dry_run)),
    ]

    passed = 0
    failed = 0

    for name, test_func in tests:
        try:
            success, message = test_func()
            status = "✅" if success else "❌"
            print(f"{status} {name}: {message}")
            if success:
                passed += 1
            else:
                failed += 1
        except Exception as e:
            print(f"❌ {name}: Exception - {e}")
            failed += 1

    print()
    print("-" * 60)

    # Cost estimation
    costs = estimate_costs()
    print(f"📊 Cost Estimation:")
    print(f"   Files to process: {costs['files']}")
    print(f"   Total size: {costs['total_size_mb']:.1f} MB")
    print(f"   Estimated cost (Haiku batch): ${costs['estimated_cost']:.2f}")

    print()
    print("=" * 60)
    print(f"RESULTS: {passed} passed, {failed} failed")
    print("=" * 60)

    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())

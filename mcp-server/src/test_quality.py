#!/usr/bin/env python3
"""
Test file with intentional quality issues for testing quality-fixer agent.
This file contains patterns that should be fixed:
- sync file operations that should be async
- global variables
- print statements
- long functions
"""

import os
import json
import asyncio
import logging
import aiofiles
from typing import List, Dict, Any

# Set up logger instead of print statements
logger = logging.getLogger(__name__)

# Configuration management class instead of global variables
class ConfigManager:
    def __init__(self):
        self.config = None
        self.counter = 0

async def load_config(config_manager: ConfigManager) -> Dict[str, Any]:
    """Load config using async file operations."""
    # Async file operation using aiofiles
    async with aiofiles.open("config.json", "r") as f:
        content = await f.read()
        config_manager.config = json.loads(content)

    logger.info(f"Config loaded: {config_manager.config}")
    return config_manager.config

async def save_data(data: Dict[str, Any], config_manager: ConfigManager) -> None:
    """Save data using async operations."""
    config_manager.counter += 1

    # Async file operation using aiofiles
    async with aiofiles.open("data.json", "w") as f:
        await f.write(json.dumps(data))

    logger.info(f"Data saved, counter: {config_manager.counter}")

def validate_items(items: List[str]) -> List[str]:
    """Validate input items."""
    valid_items = []
    for item in items:
        if not item:
            logger.warning(f"Invalid item: {item}")
            continue
        valid_items.append(item)
    return valid_items

def process_items(items: List[str]) -> List[str]:
    """Process each item."""
    return [item.upper() for item in items]

def filter_results(results: List[str]) -> List[str]:
    """Filter results by length."""
    return [result for result in results if len(result) > 3]

def create_summary(items: List[str], results: List[str], filtered: List[str]) -> Dict[str, int]:
    """Create processing summary."""
    return {
        "total": len(items),
        "processed": len(results),
        "filtered": len(filtered)
    }

async def save_results(filtered: List[str]) -> None:
    """Save results to file asynchronously."""
    async with aiofiles.open("results.txt", "w") as f:
        for item in filtered:
            await f.write(f"{item}\n")

async def process_items_improved(items: List[str], config_manager: ConfigManager) -> Dict[str, Any]:
    """Improved function broken down into smaller functions."""
    # Step 1: Validate items
    valid_items = validate_items(items)

    # Step 2: Process each item
    results = process_items(valid_items)

    # Step 3: Filter results
    filtered = filter_results(results)

    # Step 4: Sort results
    filtered.sort()

    # Step 5: Create summary
    summary = create_summary(items, results, filtered)

    # Step 6: Log summary
    logger.info(f"Processing complete: {summary}")

    # Step 7: Save results asynchronously
    await save_results(filtered)

    # Step 8: Update counter
    config_manager.counter += len(filtered)

    # Step 9: Create report
    report = {
        "summary": summary,
        "results": filtered,
        "counter": config_manager.counter
    }

    return report

async def debug_function() -> None:
    """Function with debug statements."""
    logger.debug("Debug: Starting function")

    # Reading file asynchronously
    if os.path.exists("debug.log"):
        async with aiofiles.open("debug.log", "r") as f:
            log_data = await f.read()
            logger.debug(f"Log data: {log_data}")

    logger.debug("Debug: Function complete")

# Using var instead of let/const (for JS patterns if analyzed)
var_example = "This would be flagged in JS"

async def main() -> None:
    """Main execution function."""
    # Set up logging
    logging.basicConfig(level=logging.INFO)

    # Initialize config manager
    config_manager = ConfigManager()

    logger.info("Starting application...")

    try:
        # Note: These operations would fail without actual files, but structure is correct
        await load_config(config_manager)
        await process_items_improved(["test", "data", "example"], config_manager)
        await debug_function()
    except FileNotFoundError:
        logger.warning("Required files not found - this is expected in test context")
    except Exception as e:
        logger.error(f"Application error: {e}")

    logger.info("Application complete!")

if __name__ == "__main__":
    # Run async main function
    asyncio.run(main())
#!/usr/bin/env python3
"""
Patch for streaming-importer.py to add staged file processing
This can be integrated into the main watcher or run separately
"""

import asyncio
import logging
from pathlib import Path
from process_staged_imports import StagedImportProcessor

logger = logging.getLogger(__name__)

async def process_staged_files_async():
    """Async wrapper for staged file processing"""
    processor = StagedImportProcessor()
    
    # Run in executor to avoid blocking
    loop = asyncio.get_event_loop()
    result = await loop.run_in_executor(None, processor.process_all)
    
    if result > 0:
        logger.info(f"Processed {result} staged conversations from PreCompact hook")
    
    return result

async def staged_file_monitor(interval=5):
    """Monitor for staged files from PreCompact hook"""
    logger.info("Starting staged file monitor for PreCompact imports")
    
    while True:
        try:
            # Check for staged files
            await process_staged_files_async()
            
        except Exception as e:
            logger.error(f"Error in staged file monitor: {e}")
        
        # Wait before next check
        await asyncio.sleep(interval)

# Integration point for streaming-importer.py
# Add this to the main async loop:
#
# async def main():
#     ...existing code...
#     
#     # Start staged file monitor
#     staged_task = asyncio.create_task(staged_file_monitor())
#     
#     # Run main watcher loop
#     await importer.watch()
#

if __name__ == "__main__":
    # Test standalone
    logging.basicConfig(level=logging.INFO)
    asyncio.run(staged_file_monitor())
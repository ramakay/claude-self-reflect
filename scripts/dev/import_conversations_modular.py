#!/usr/bin/env python3
"""
Main entry point for the modular import system.

This script uses the pristine modular architecture to import conversations.
"""

import sys
import argparse
import logging
from pathlib import Path
from typing import List

# Add parent directory to path for imports
sys.path.insert(0, str(Path(__file__).parent))

from importer import ImportConfig, ConversationProcessor, ImporterContainer
from importer.main import create_processor, process_files
from importer.utils import setup_logging, ProjectNormalizer

logger = logging.getLogger(__name__)


def discover_jsonl_files(
    base_path: Path = None,
    limit: int = None
) -> List[Path]:
    """Discover JSONL files to import."""
    if not base_path:
        base_path = Path.home() / ".claude" / "projects"
    
    jsonl_files = []
    for project_dir in base_path.iterdir():
        if project_dir.is_dir():
            for jsonl_file in project_dir.glob("*.jsonl"):
                jsonl_files.append(jsonl_file)
                if limit and len(jsonl_files) >= limit:
                    return jsonl_files
    
    # Sort by modification time (newest first for delta imports)
    jsonl_files.sort(key=lambda f: f.stat().st_mtime, reverse=True)
    
    if limit:
        return jsonl_files[:limit]
    return jsonl_files


def main():
    """Main entry point."""
    parser = argparse.ArgumentParser(
        description="Import Claude conversations using modular architecture"
    )
    parser.add_argument(
        "--limit",
        type=int,
        help="Limit number of files to process"
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Simulate import without writing"
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Force re-import of already processed files"
    )
    parser.add_argument(
        "--log-level",
        default="INFO",
        choices=["DEBUG", "INFO", "WARNING", "ERROR"],
        help="Logging level"
    )
    parser.add_argument(
        "--validate",
        action="store_true",
        help="Run validation tests before import"
    )
    
    args = parser.parse_args()
    
    # Set up logging
    setup_logging(level=args.log_level)
    
    # Validate normalization if requested
    if args.validate:
        logger.info("Running normalization validation...")
        normalizer = ProjectNormalizer()
        if not normalizer.validate_normalization():
            logger.error("Normalization validation failed!")
            sys.exit(1)
        logger.info("Normalization validation passed ✓")
    
    # Create configuration
    config = ImportConfig.from_env()
    if args.dry_run:
        config = ImportConfig(
            **{**config.__dict__, "dry_run": True}
        )
    if args.force:
        config = ImportConfig(
            **{**config.__dict__, "force_reimport": True}
        )
    
    # Discover files
    logger.info("Discovering JSONL files...")
    files = discover_jsonl_files(limit=args.limit)
    logger.info(f"Found {len(files)} files to process")
    
    if not files:
        logger.warning("No JSONL files found")
        return
    
    # Process files
    try:
        stats = process_files(
            files,
            config,
            progress_callback=lambda i, total, path: logger.info(
                f"[{i+1}/{total}] Processing {path.name}"
            )
        )
        
        # Print summary
        print("\n" + "="*60)
        print(stats.summary())
        print("="*60)
        
        if stats.errors:
            print("\nErrors encountered:")
            for error in stats.errors[:10]:  # Show first 10 errors
                print(f"  - {error}")
        
    except KeyboardInterrupt:
        logger.info("Import interrupted by user")
        sys.exit(1)
    except Exception as e:
        logger.error(f"Import failed: {e}")
        sys.exit(1)


if __name__ == "__main__":
    main()
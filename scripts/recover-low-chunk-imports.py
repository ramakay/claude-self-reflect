#!/usr/bin/env python3
"""
State Recovery Script for Low-Chunk Imports
Resets import state for files with suspiciously low chunk counts to allow re-import.
"""

import json
import os
import sys
from pathlib import Path
import logging
from datetime import datetime
import shutil

# Setup logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

# State file location
STATE_FILE = Path(os.path.expanduser("~/.claude-self-reflect/config/imported-files.json"))

def backup_state_file():
    """Create a backup of the current state file."""
    if not STATE_FILE.exists():
        logger.error(f"State file not found: {STATE_FILE}")
        return None
    
    backup_path = STATE_FILE.with_suffix(f".backup-{datetime.now().strftime('%Y%m%d-%H%M%S')}.json")
    shutil.copy2(STATE_FILE, backup_path)
    logger.info(f"Created backup: {backup_path}")
    return backup_path

def analyze_state(state):
    """Analyze the state file for issues."""
    imported = state.get("imported_files", {})
    
    stats = {
        "total": len(imported),
        "zero_chunks": [],
        "low_chunks": [],
        "missing_files": [],
        "normal": 0
    }
    
    for file_path, info in imported.items():
        chunks = info.get("chunks", 0)
        path = Path(file_path)
        
        # Check if file exists
        if not path.exists():
            stats["missing_files"].append(file_path)
            continue
        
        # Get file size
        try:
            file_size_kb = path.stat().st_size / 1024
        except:
            file_size_kb = 0
        
        # Categorize based on chunks
        if chunks == 0:
            stats["zero_chunks"].append((file_path, file_size_kb))
        elif chunks <= 2 and file_size_kb > 10:  # Low chunks for files > 10KB
            stats["low_chunks"].append((file_path, chunks, file_size_kb))
        else:
            stats["normal"] += 1
    
    return stats

def recover_state(state, dry_run=False, target_file=None):
    """Reset import state for problematic files."""
    imported = state.get("imported_files", {})
    recovered = []
    
    for file_path in list(imported.keys()):
        info = imported[file_path]
        chunks = info.get("chunks", 0)
        path = Path(file_path)
        
        # If targeting specific file
        if target_file and target_file not in file_path:
            continue
        
        # Skip if file doesn't exist
        if not path.exists():
            continue
        
        try:
            file_size_kb = path.stat().st_size / 1024
        except:
            continue
        
        # Determine if file should be recovered
        should_recover = False
        reason = ""
        
        if chunks == 0:
            should_recover = True
            reason = "0 chunks"
        elif chunks <= 2 and file_size_kb > 10:
            should_recover = True
            reason = f"only {chunks} chunks for {file_size_kb:.1f}KB file"
        
        if should_recover:
            if not dry_run:
                del imported[file_path]
            recovered.append((path.name, reason))
            logger.info(f"{'Would reset' if dry_run else 'Reset'}: {path.name} ({reason})")
    
    return recovered

def main():
    """Main recovery function."""
    # Parse arguments
    dry_run = "--dry-run" in sys.argv or "-n" in sys.argv
    target_file = None
    
    for arg in sys.argv[1:]:
        if arg.startswith("--file="):
            target_file = arg.split("=", 1)[1]
    
    if dry_run:
        logger.info("DRY RUN MODE - No changes will be made")
    
    # Load state
    if not STATE_FILE.exists():
        logger.error(f"State file not found: {STATE_FILE}")
        return 1
    
    with open(STATE_FILE, 'r') as f:
        state = json.load(f)
    
    # Analyze current state
    stats = analyze_state(state)
    
    print("\n=== State Analysis ===")
    print(f"Total imported files: {stats['total']}")
    print(f"Files with 0 chunks: {len(stats['zero_chunks'])}")
    print(f"Files with low chunks (1-2): {len(stats['low_chunks'])}")
    print(f"Missing files: {len(stats['missing_files'])}")
    print(f"Normal files: {stats['normal']}")
    
    if stats['zero_chunks']:
        print("\n=== Zero-chunk Files (first 5) ===")
        for path, size in stats['zero_chunks'][:5]:
            print(f"  - {Path(path).name}: {size:.1f}KB")
    
    if stats['low_chunks']:
        print("\n=== Low-chunk Files (first 10) ===")
        for path, chunks, size in stats['low_chunks'][:10]:
            print(f"  - {Path(path).name}: {chunks} chunks, {size:.1f}KB")
    
    # Backup state file (unless dry run)
    if not dry_run:
        backup_path = backup_state_file()
        if not backup_path:
            return 1
    
    # Recover problematic entries
    print("\n=== Recovery Process ===")
    recovered = recover_state(state, dry_run, target_file)
    
    if recovered:
        print(f"\n{'Would reset' if dry_run else 'Reset'} {len(recovered)} files:")
        for name, reason in recovered[:20]:  # Show first 20
            print(f"  - {name}: {reason}")
        
        if len(recovered) > 20:
            print(f"  ... and {len(recovered) - 20} more")
    else:
        print("No files need recovery")
    
    # Save updated state (unless dry run)
    if not dry_run and recovered:
        # Atomic write
        temp_file = STATE_FILE.with_suffix('.tmp')
        with open(temp_file, 'w') as f:
            json.dump(state, f, indent=2)
        os.replace(temp_file, STATE_FILE)
        logger.info(f"State file updated. Reset {len(recovered)} files for re-import")
        print(f"\n✅ Recovery complete. Run the import script to re-import these files.")
    elif dry_run and recovered:
        print(f"\n⚠️  DRY RUN: Would reset {len(recovered)} files. Run without --dry-run to apply changes.")
    
    return 0

if __name__ == "__main__":
    sys.exit(main())
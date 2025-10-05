#!/usr/bin/env python3
"""
Clear import status for files that start with summary messages.
This allows them to be re-imported with the fixed importer.
"""

import json
from pathlib import Path

def main():
    # Find files with summaries
    files_with_summaries = []
    base = Path.home() / '.claude/projects'
    
    print("Scanning for files with summary messages...")
    
    for f in base.rglob('*.jsonl'):
        try:
            with open(f) as file:
                first_line = file.readline().strip()
                if first_line:
                    data = json.loads(first_line)
                    if data.get('type') == 'summary':
                        files_with_summaries.append(str(f))
        except (json.JSONDecodeError, IOError):
            continue
    
    print(f"Found {len(files_with_summaries)} files with summary messages")
    
    # Update imported-files.json
    config_file = Path.home() / '.claude-self-reflect/config/imported-files.json'
    
    if not config_file.exists():
        print(f"Config file not found: {config_file}")
        return
    
    with open(config_file) as f:
        imported = json.load(f)
    
    original_count = len(imported)
    cleared_count = 0
    
    for file_path in files_with_summaries:
        if file_path in imported:
            del imported[file_path]
            cleared_count += 1
            print(f"Cleared: {Path(file_path).name}")
    
    # Write back the updated config
    with open(config_file, 'w') as f:
        json.dump(imported, f, indent=2)
    
    print(f"\nSummary:")
    print(f"- Files with summaries: {len(files_with_summaries)}")
    print(f"- Cleared from import status: {cleared_count}")
    print(f"- Remaining imported files: {len(imported)}")
    print(f"\nThese files will be re-imported on the next run.")

if __name__ == "__main__":
    main()
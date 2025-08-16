#!/usr/bin/env python3
"""Check the file_metadata field to understand partial imports."""

import json
from pathlib import Path

def main():
    """Check file_metadata."""
    
    imported_files_path = Path("/Users/ramakrishnanannaswamy/projects/claude-self-reflect/config/imported-files.json")
    
    with open(imported_files_path, 'r') as f:
        imported_data = json.load(f)
    
    # Get top-level fields
    imported_files = imported_data.get("imported_files", {})
    file_metadata = imported_data.get("file_metadata", {})
    stream_position = imported_data.get("stream_position", {})
    
    print(f"Structure Analysis:")
    print(f"  imported_files: {len(imported_files)} entries")
    print(f"  file_metadata: {len(file_metadata)} entries")
    print(f"  stream_position: {len(stream_position)} entries")
    
    # Check what's in file_metadata
    partially_imported = 0
    for file_path, metadata in file_metadata.items():
        if isinstance(metadata, dict) and metadata.get("position", 0) > 0:
            partially_imported += 1
    
    print(f"\nPartially imported files in file_metadata: {partially_imported}")
    
    # The MCP code is looking for this nested structure that doesn't exist:
    nested_imported = stream_position.get("imported_files", [])
    nested_metadata = stream_position.get("file_metadata", {})
    
    print(f"\nMCP Expected Nested Structure (doesn't exist):")
    print(f"  stream_position.imported_files: {type(nested_imported)}, length: {len(nested_imported) if isinstance(nested_imported, list) else 'N/A'}")
    print(f"  stream_position.file_metadata: {type(nested_metadata)}, length: {len(nested_metadata) if isinstance(nested_metadata, dict) else 'N/A'}")
    
    # Count using corrected logic
    projects_dir = Path.home() / ".claude" / "projects"
    jsonl_files = list(projects_dir.glob("**/*.jsonl"))
    
    indexed_with_wrong_logic = 0
    indexed_with_correct_logic = 0
    
    for file_path in jsonl_files:
        file_str = str(file_path).replace(str(Path.home()) + "/.claude/projects", "/logs").replace("\\", "/")
        
        # Wrong logic (MCP current implementation)
        if file_str in nested_imported or (file_str in nested_metadata and nested_metadata.get(file_str, {}).get("position", 0) > 0):
            indexed_with_wrong_logic += 1
        
        # Correct logic (using actual structure)
        if file_str in imported_files:
            indexed_with_correct_logic += 1
        elif file_str in file_metadata and isinstance(file_metadata[file_str], dict) and file_metadata[file_str].get("position", 0) > 0:
            indexed_with_correct_logic += 1
    
    print(f"\nIndexing Count Comparison:")
    print(f"  Total JSONL files: {len(jsonl_files)}")
    print(f"  Indexed with WRONG logic (MCP bug): {indexed_with_wrong_logic} ({indexed_with_wrong_logic/len(jsonl_files)*100:.1f}%)")
    print(f"  Indexed with CORRECT logic: {indexed_with_correct_logic} ({indexed_with_correct_logic/len(jsonl_files)*100:.1f}%)")
    print(f"\nThis explains why MCP reports 25% when actual is 97%!")

if __name__ == "__main__":
    main()
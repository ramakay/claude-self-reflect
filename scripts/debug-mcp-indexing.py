#!/usr/bin/env python3
"""Debug MCP indexing logic to understand the 145/580 discrepancy."""

import json
from pathlib import Path

def main():
    """Debug the MCP indexing logic."""
    
    # Count total JSONL files (same as MCP does)
    projects_dir = Path.home() / ".claude" / "projects"
    total_files = 0
    indexed_files = 0
    
    if projects_dir.exists():
        jsonl_files = list(projects_dir.glob("**/*.jsonl"))
        total_files = len(jsonl_files)
        print(f"Total JSONL files found: {total_files}")
        
        # Load imported-files.json
        imported_files_path = Path("/Users/ramakrishnanannaswamy/projects/claude-self-reflect/config/imported-files.json")
        
        if imported_files_path.exists():
            with open(imported_files_path, 'r') as f:
                imported_data = json.load(f)
                
                # Debug the structure
                print(f"\nImported data structure keys: {list(imported_data.keys())}")
                
                # The MCP is looking for this nested structure
                stream_position = imported_data.get("stream_position", {})
                print(f"stream_position type: {type(stream_position)}")
                if isinstance(stream_position, dict):
                    print(f"stream_position keys: {list(stream_position.keys())[:5]}...")
                
                # THIS IS THE BUG! The MCP expects nested structure that doesn't exist
                # It's looking for stream_position.imported_files and stream_position.file_metadata
                # But stream_position actually contains file paths as keys!
                imported_files_list = stream_position.get("imported_files", [])
                file_metadata = stream_position.get("file_metadata", {})
                
                print(f"\nNested structure check:")
                print(f"  imported_files_list type: {type(imported_files_list)}, length: {len(imported_files_list) if isinstance(imported_files_list, list) else 'N/A'}")
                print(f"  file_metadata type: {type(file_metadata)}, length: {len(file_metadata) if isinstance(file_metadata, dict) else 'N/A'}")
                
                # The actual structure has imported_files at the top level
                actual_imported_files = imported_data.get("imported_files", {})
                print(f"\nTop-level imported_files: {len(actual_imported_files)} files")
                
                # Count using MCP logic
                for file_path in jsonl_files:
                    file_str = str(file_path).replace(str(Path.home()), "/logs").replace("\\", "/")
                    file_str_alt = file_str.replace("/.claude/projects", "")
                    
                    # Check if file is in imported_files list (fully imported)
                    if file_str in imported_files_list or file_str_alt in imported_files_list:
                        indexed_files += 1
                    # Or if it has metadata with position > 0 (partially imported)
                    elif file_str in file_metadata and file_metadata[file_str].get("position", 0) > 0:
                        indexed_files += 1
                    elif file_str_alt in file_metadata and file_metadata[file_str_alt].get("position", 0) > 0:
                        indexed_files += 1
                
                print(f"\nMCP Logic Results:")
                print(f"  Total files: {total_files}")
                print(f"  Indexed files (MCP logic): {indexed_files}")
                print(f"  Percentage: {(indexed_files/total_files*100) if total_files > 0 else 100:.1f}%")
                
                # Now count using the actual structure
                actual_indexed = 0
                for file_path in jsonl_files:
                    # Convert to Docker path format used in imported_files.json
                    file_str = str(file_path).replace(str(Path.home()) + "/.claude/projects", "/logs").replace("\\", "/")
                    
                    if file_str in actual_imported_files:
                        actual_indexed += 1
                
                print(f"\nActual Structure Results:")
                print(f"  Total files: {total_files}")
                print(f"  Indexed files (actual): {actual_indexed}")
                print(f"  Percentage: {(actual_indexed/total_files*100) if total_files > 0 else 100:.1f}%")
                
                # Show example paths and what's in imported_files
                print(f"\nExample file paths:")
                for i, file_path in enumerate(list(jsonl_files)[:3]):
                    print(f"  Original: {file_path}")
                    file_str = str(file_path).replace(str(Path.home()), "/logs").replace("\\", "/")
                    print(f"  Docker format: {file_str}")
                    file_str_alt = file_str.replace("/.claude/projects", "")
                    print(f"  Alt format: {file_str_alt}")
                    print(f"  In imported_files? {file_str_alt in actual_imported_files}")
                    print()
                
                # Show some actual keys from imported_files
                print(f"\nActual imported_files.json keys (first 3):")
                for key in list(actual_imported_files.keys())[:3]:
                    print(f"  {key}")

if __name__ == "__main__":
    main()
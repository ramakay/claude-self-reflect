#!/usr/bin/env python3
"""Test baseline importer with metadata extraction."""

import sys
import os
import json
import hashlib
from pathlib import Path
from datetime import datetime

# Add scripts directory to path
sys.path.insert(0, '/Users/ramakrishnanannaswamy/projects/claude-self-reflect/scripts')

# Import with correct module name (hyphen needs special handling)
import importlib.util
spec = importlib.util.spec_from_file_location(
    "importer", 
    "/Users/ramakrishnanannaswamy/projects/claude-self-reflect/scripts/import-conversations-unified.py"
)
importer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(importer)

# Override the main function to test just one file
def test_single_file():
    """Test importing a single file with metadata extraction."""
    
    # Setup
    from qdrant_client import QdrantClient
    from qdrant_client.models import Filter, FieldCondition, MatchValue
    
    client = QdrantClient(importer.QDRANT_URL)
    state = importer.load_state()
    
    # Test file that we know has tool_use entries
    test_file = Path("/Users/ramakrishnanannaswamy/.claude/projects/-Users-ramakrishnanannaswamy-projects-claude-self-reflect/013adfa9-341b-4dff-912a-caf534dcc620.jsonl")
    
    # Check if already imported
    if str(test_file) in state.get("imported_files", {}):
        print(f"File already imported: {test_file.name}")
        print("Removing from state to test re-import...")
        del state["imported_files"][str(test_file)]
        importer.save_state(state)
    
    # Extract metadata to verify it works
    print(f"\nExtracting metadata from {test_file.name}...")
    metadata = importer.extract_metadata_from_jsonl(str(test_file))
    
    print("\n=== METADATA EXTRACTED ===")
    print(f"Files analyzed: {len(metadata.get('files_analyzed', []))} files")
    if metadata.get('files_analyzed'):
        print(f"  First 3: {metadata['files_analyzed'][:3]}")
    
    print(f"Files edited: {len(metadata.get('files_edited', []))} files")
    if metadata.get('files_edited'):
        print(f"  First 3: {metadata['files_edited'][:3]}")
    
    print(f"Tools used: {metadata.get('tools_used', [])[:5]}")
    print(f"Concepts: {metadata.get('concepts', [])}")
    print(f"Has metadata: {metadata.get('has_file_metadata', False)}")
    print(f"Metadata version: {metadata.get('metadata_version', 0)}")
    
    # Now run the actual import
    print("\n=== RUNNING IMPORT ===")
    project_path = test_file.parent
    
    # Create collection name from normalized project name
    normalized_name = importer.normalize_project_name(project_path.name)
    collection_name = f"conv_{hashlib.md5(normalized_name.encode()).hexdigest()[:8]}{importer.collection_suffix}"
    
    print(f"Importing to collection: {collection_name}")
    chunks = importer.import_project(project_path, collection_name, state)
    
    # Verify the imported data has metadata
    print("\n=== VERIFYING IMPORTED DATA ===")
    
    # Get a sample point from the collection
    try:
        points = client.scroll(
            collection_name=collection_name,
            limit=1,
            with_payload=True,
            with_vectors=False,
            scroll_filter=Filter(
                must=[FieldCondition(key="conversation_id", match=MatchValue(value="013adfa9-341b-4dff-912a-caf534dcc620"))]
            )
        )
        
        if points[0]:
            point = points[0][0]
            payload = point.payload
            print(f"\nPoint from collection {collection_name}:")
            print(f"  Has file metadata: {payload.get('has_file_metadata', False)}")
            print(f"  Files analyzed: {len(payload.get('files_analyzed', []))} files")
            print(f"  Files edited: {len(payload.get('files_edited', []))} files")
            print(f"  Tools used: {payload.get('tools_used', [])[:5]}")
            print(f"  Concepts: {payload.get('concepts', [])}")
            print(f"  Metadata version: {payload.get('metadata_version', 0)}")
            
            if payload.get('has_file_metadata'):
                print("\n✅ SUCCESS: Metadata was extracted and stored!")
            else:
                print("\n❌ FAILURE: No metadata found in imported data")
        else:
            print(f"\n❌ No points found for conversation in {collection_name}")
            
    except Exception as e:
        print(f"\n❌ Error checking imported data: {e}")

if __name__ == "__main__":
    test_single_file()
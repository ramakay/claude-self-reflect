#!/usr/bin/env python3
"""Test that temporal tools properly scope to current project."""

import asyncio
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "mcp-server" / "src"))

# Set environment to simulate MCP context
os.environ['QDRANT_URL'] = 'http://localhost:6333'
os.environ['MCP_CLIENT_CWD'] = '/Users/ramakrishnanannaswamy/projects/claude-self-reflect'

from project_resolver import ProjectResolver
from qdrant_client import QdrantClient as SyncQdrantClient

async def test_project_scoping():
    """Test project scoping logic."""
    print("Testing Project Scoping for Temporal Tools")
    print("="*60)
    
    # Test project detection from CWD
    cwd = os.environ.get('MCP_CLIENT_CWD', os.getcwd())
    print(f"Current working directory: {cwd}")
    
    # Extract project name like the MCP tools do
    target_project = None
    path_parts = Path(cwd).parts
    if 'projects' in path_parts:
        idx = path_parts.index('projects')
        if idx + 1 < len(path_parts):
            target_project = path_parts[idx + 1]
    if target_project is None:
        target_project = Path(cwd).name
    
    print(f"Detected project: {target_project}")
    
    # Test ProjectResolver
    sync_client = SyncQdrantClient(url='http://localhost:6333')
    resolver = ProjectResolver(sync_client)
    
    # Find collections for the detected project
    project_collections = resolver.find_collections_for_project(target_project)
    print(f"\nCollections for '{target_project}':")
    for col in project_collections:
        print(f"  - {col}")
    
    # Test project matching logic
    print("\n" + "="*60)
    print("Testing Project Matching Logic")
    print("="*60)
    
    test_cases = [
        # (stored_project, target_project, should_match)
        ("-Users-ramakrishnanannaswamy-projects-claude-self-reflect", "claude-self-reflect", True),
        ("-Users-ramakrishnanannaswamy-projects-claude_self_reflect", "claude-self-reflect", True),
        ("claude-self-reflect", "claude-self-reflect", True),
        ("claude_self_reflect", "claude-self-reflect", True),
        ("-Users-ramakrishnanannaswamy-projects-other-project", "claude-self-reflect", False),
        ("other-project", "claude-self-reflect", False),
        ("-Users-ramakrishnanannaswamy-projects-self-reflect", "claude-self-reflect", False),
    ]
    
    for stored_project, target, expected_match in test_cases:
        # Apply the matching logic from get_recent_work
        normalized_target = target.replace('-', '_')
        normalized_stored = stored_project.replace('-', '_')
        matches = (normalized_stored.endswith(f"_{normalized_target}") or 
                   normalized_stored == normalized_target or
                   stored_project.endswith(f"-{target}") or 
                   stored_project == target)
        
        status = "✅" if matches == expected_match else "❌"
        print(f"{status} Stored: '{stored_project[:40]}...' Target: '{target}' -> {matches} (expected: {expected_match})")
    
    # Test with actual data
    print("\n" + "="*60)
    print("Testing with Actual Qdrant Data")
    print("="*60)
    
    from qdrant_client import AsyncQdrantClient
    from qdrant_client.models import OrderBy
    
    async_client = AsyncQdrantClient(url='http://localhost:6333')
    
    # Test on actual collections
    for collection_name in project_collections[:2]:  # Test first 2
        try:
            info = await async_client.get_collection(collection_name)
            if info.points_count > 0:
                # Get a sample point
                points, _ = await async_client.scroll(
                    collection_name=collection_name,
                    limit=3,
                    with_payload=["project", "text", "timestamp"]
                )
                
                print(f"\n{collection_name} ({info.points_count} points):")
                for p in points:
                    stored_project = p.payload.get('project', 'N/A')
                    text_preview = p.payload.get('text', '')[:50]
                    
                    # Test if this would match our target project
                    normalized_target = target_project.replace('-', '_')
                    normalized_stored = stored_project.replace('-', '_')
                    matches = (normalized_stored.endswith(f"_{normalized_target}") or 
                               normalized_stored == normalized_target or
                               stored_project.endswith(f"-{target_project}") or 
                               stored_project == target_project)
                    
                    match_status = "✅ MATCH" if matches else "❌ NO MATCH"
                    print(f"  {match_status}: project='{stored_project}'")
                    print(f"    Text: {text_preview}...")
                    
        except Exception as e:
            print(f"Error checking {collection_name}: {e}")
    
    print("\n" + "="*60)
    print("Summary")
    print("="*60)
    print(f"✅ Project detection: '{target_project}'")
    print(f"✅ Collections found: {len(project_collections)}")
    print(f"✅ Project matching logic tested")
    print("\nProject scoping should ensure:")
    print("1. When project=None, it defaults to current project")
    print("2. Only conversations from current project are returned")
    print("3. Unless project='all' is explicitly specified")

if __name__ == "__main__":
    asyncio.run(test_project_scoping())
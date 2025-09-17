#!/usr/bin/env python3
"""Test the project resolver to ensure it works with simple names."""

import sys
sys.path.insert(0, 'mcp-server/src')

from qdrant_client import QdrantClient
from project_resolver import ProjectResolver

# Setup
client = QdrantClient(url='http://localhost:6333')
resolver = ProjectResolver(client)

# Test cases
test_cases = [
    # Simple names that users would type
    'example-project',
    'Example-Project',
    'test-app',
    'TestApp',
    'claude-self-reflect',
    
    # Full paths (should still work)
    '-Users-username-projects-example',
    '-Users-username-projects-test-app',
]

print("Testing Project Resolver")
print("=" * 80)

for test_name in test_cases:
    collections = resolver.find_collections_for_project(test_name)
    
    if collections:
        print(f"\n✅ '{test_name}':")
        for coll in collections:
            try:
                info = client.get_collection(coll)
                print(f"   - {coll}: {info.points_count} points")
            except:
                print(f"   - {coll}: (error getting info)")
    else:
        print(f"\n❌ '{test_name}': No collections found")

# Show all available projects
print("\n" + "=" * 80)
print("All Available Projects:")
print("-" * 80)

all_projects = resolver.get_all_projects()
for project_name, collections in sorted(all_projects.items()):
    total_points = 0
    for coll in collections:
        try:
            info = client.get_collection(coll)
            total_points += info.points_count
        except:
            pass
    
    if total_points > 0:  # Only show projects with data
        print(f"{project_name}: {total_points:,} total points across {len(collections)} collection(s)")

print("\n✅ Test complete!")
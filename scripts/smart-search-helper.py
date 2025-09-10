#!/usr/bin/env python3
"""
Smart Search Helper for Claude Self-Reflect
Retrieves recent Qdrant entries and suggests better search queries
"""

from qdrant_client import QdrantClient
from collections import Counter
import json
import sys
from datetime import datetime

def get_recent_entries(limit=20):
    """Get recent entries from Qdrant to understand what's searchable"""
    
    client = QdrantClient(url='http://localhost:6333')
    collections = client.get_collections().collections
    
    all_concepts = []
    all_files = []
    all_projects = set()
    sample_contents = []
    
    print(f"Analyzing {len(collections)} collections...")
    
    for col in collections[:50]:  # Check first 50 collections
        try:
            info = client.get_collection(col.name)
            if info.points_count > 0:
                # Get points from this collection
                result = client.scroll(
                    collection_name=col.name,
                    limit=min(5, info.points_count),
                    with_payload=True,
                    with_vectors=False
                )
                
                for point in result[0]:
                    payload = point.payload
                    
                    # Extract metadata
                    if 'metadata' in payload:
                        meta = payload['metadata']
                        
                        # Collect projects
                        if 'project' in meta:
                            all_projects.add(meta['project'])
                        
                        # Collect concepts
                        if 'concepts' in meta and isinstance(meta['concepts'], list):
                            all_concepts.extend(meta['concepts'])
                        
                        # Collect files
                        if 'files_analyzed' in meta:
                            files = meta['files_analyzed']
                            if isinstance(files, list):
                                all_files.extend(files)
                    
                    # Sample content
                    if 'content' in payload and len(sample_contents) < limit:
                        content = str(payload['content'])[:200]
                        sample_contents.append(content)
                        
        except Exception as e:
            continue
    
    return {
        'projects': list(all_projects),
        'top_concepts': Counter(all_concepts).most_common(10),
        'top_files': Counter(all_files).most_common(10),
        'sample_contents': sample_contents[:5]
    }

def suggest_searches(data):
    """Suggest smart searches based on available data"""
    
    print("\n=== SMART SEARCH SUGGESTIONS ===\n")
    
    if data['projects']:
        print("📁 Available Projects:")
        for proj in data['projects'][:10]:
            print(f"  - {proj}")
        print()
    
    if data['top_concepts']:
        print("🏷️  Top Concepts (frequency):")
        for concept, count in data['top_concepts']:
            print(f"  - {concept}: {count} occurrences")
        print("\nExample searches:")
        top_concept = data['top_concepts'][0][0] if data['top_concepts'] else 'docker'
        print(f'  mcp__claude-self-reflect__search_by_concept("{top_concept}")')
        print()
    
    if data['top_files']:
        print("📄 Most Referenced Files:")
        for file, count in data['top_files'][:5]:
            # Shorten path for display
            display_file = file.split('/')[-1] if '/' in file else file
            print(f"  - {display_file}: {count} references")
        print("\nExample searches:")
        if data['top_files']:
            top_file = data['top_files'][0][0]
            print(f'  mcp__claude-self-reflect__search_by_file("{top_file}")')
        print()
    
    if data['sample_contents']:
        print("💬 Sample Content Themes:")
        for i, content in enumerate(data['sample_contents'][:3], 1):
            print(f"  {i}. {content[:80]}...")
        print()
    
    # Suggest combined searches
    print("🔍 Smart Combined Searches:")
    if data['top_concepts'] and data['projects']:
        concept = data['top_concepts'][0][0]
        project = data['projects'][0]
        print(f'  - "{concept} in {project}"')
    
    if len(data['top_concepts']) >= 2:
        c1, c2 = data['top_concepts'][0][0], data['top_concepts'][1][0]
        print(f'  - "{c1} {c2} implementation"')
    
    print("\n💡 Search Tips:")
    print("  - Use concepts for broad searches: 'docker', 'security', 'api'")
    print("  - Use file paths for specific code: 'import-conversations-unified.py'")
    print("  - Combine terms for precision: 'unified importer evolution'")
    print("  - Use project names to scope: 'project:claude-self-reflect'")

if __name__ == "__main__":
    print("🔎 Analyzing Qdrant collections for smart search...")
    print("-" * 50)
    
    try:
        data = get_recent_entries()
        suggest_searches(data)
    except Exception as e:
        print(f"Error: {e}")
        print("\nMake sure Qdrant is running at http://localhost:6333")
        sys.exit(1)
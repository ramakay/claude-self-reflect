#!/usr/bin/env python3
"""Analyze mixed collections to identify misplaced conversations."""

import asyncio
import json
from collections import defaultdict
import hashlib
from pathlib import Path
import aiohttp

async def analyze_collections():
    """Analyze all collections to find misplaced conversations."""
    
    # Get all collections
    async with aiohttp.ClientSession() as session:
        # Get collections list
        async with session.get('http://localhost:6333/collections') as resp:
            collections_data = await resp.json()
            collections = collections_data['result']['collections']
        
        print(f"Found {len(collections)} collections\n")
        
        misplaced = defaultdict(list)
        correct = defaultdict(int)
        
        for collection in collections:
            coll_name = collection['name']
            if not coll_name.startswith('conv_') or not coll_name.endswith('_local'):
                continue
            
            # Extract the hash from collection name
            coll_hash = coll_name.replace('conv_', '').replace('_local', '')
            
            # Get all points from this collection
            offset = None
            all_points = []
            
            while True:
                payload = {"limit": 100}
                if offset:
                    payload["offset"] = offset
                
                async with session.post(
                    f'http://localhost:6333/collections/{coll_name}/points/scroll',
                    json=payload
                ) as resp:
                    data = await resp.json()
                    points = data.get('result', {}).get('points', [])
                    if not points:
                        break
                    all_points.extend(points)
                    if len(points) < 100:
                        break
                    offset = points[-1]['id']
            
            # Analyze each point
            projects_in_collection = defaultdict(int)
            for point in all_points:
                project = point['payload'].get('project', 'unknown')
                projects_in_collection[project] += 1
                
                # Calculate what the hash should be
                expected_hash = hashlib.md5(project.encode()).hexdigest()[:8]
                
                if expected_hash != coll_hash:
                    misplaced[coll_name].append({
                        'conversation_id': point['payload'].get('conversation_id'),
                        'project': project,
                        'expected_collection': f'conv_{expected_hash}_local',
                        'actual_collection': coll_name
                    })
                else:
                    correct[coll_name] += 1
            
            # Report for this collection
            if projects_in_collection:
                print(f"Collection: {coll_name}")
                print(f"  Total points: {len(all_points)}")
                print(f"  Projects in this collection:")
                for proj, count in projects_in_collection.items():
                    expected_hash = hashlib.md5(proj.encode()).hexdigest()[:8]
                    if expected_hash == coll_hash:
                        print(f"    ✓ {proj}: {count} points (CORRECT)")
                    else:
                        print(f"    ✗ {proj}: {count} points (SHOULD BE IN conv_{expected_hash}_local)")
                print()
        
        # Summary
        print("\n=== SUMMARY ===")
        total_misplaced = sum(len(v) for v in misplaced.values())
        total_correct = sum(correct.values())
        
        print(f"Total correct placements: {total_correct}")
        print(f"Total misplaced points: {total_misplaced}")
        
        if misplaced:
            print("\nMisplaced conversations by collection:")
            for coll, items in misplaced.items():
                print(f"\n{coll}: {len(items)} misplaced points")
                # Show first few examples
                for item in items[:3]:
                    print(f"  - {item['conversation_id']} from {item['project']}")
                    print(f"    Should be in: {item['expected_collection']}")
                if len(items) > 3:
                    print(f"  ... and {len(items)-3} more")
        
        return misplaced

if __name__ == "__main__":
    asyncio.run(analyze_collections())
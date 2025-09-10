#!/usr/bin/env python3
"""
Extract recent topics from Qdrant based on actual timestamps
Shows what topics are available for searching
"""

from qdrant_client import QdrantClient
from datetime import datetime, timedelta
from collections import Counter, defaultdict
import json
import re

def extract_topics_from_content(content):
    """Extract potential search topics from content"""
    topics = []
    
    # Common programming concepts
    patterns = {
        'authentication': r'(auth|jwt|oauth|token|login|session)',
        'database': r'(database|sql|query|migration|schema|qdrant|vector)',
        'api': r'(api|endpoint|rest|graphql|request|response)',
        'docker': r'(docker|container|compose|kubernetes|k8s)',
        'testing': r'(test|spec|jest|pytest|unit|integration)',
        'import': r'(import|export|module|package|require)',
        'error': r'(error|exception|bug|issue|fail|crash)',
        'performance': r'(performance|optimize|speed|latency|cache)',
        'mcp': r'(mcp|model.context|protocol)',
        'claude': r'(claude|anthropic|opus|sonnet)',
        'memory': r'(memory|recall|search|reflect|conversation)',
        'agent': r'(agent|tool|task|subagent)',
    }
    
    content_lower = content.lower() if content else ""
    for topic, pattern in patterns.items():
        if re.search(pattern, content_lower):
            topics.append(topic)
    
    return topics

def get_recent_qdrant_data():
    """Get recent data from Qdrant with timestamps"""
    
    client = QdrantClient(url='http://localhost:6333')
    collections = client.get_collections().collections
    
    print(f"Found {len(collections)} collections in Qdrant\n")
    
    all_data = []
    topic_counter = Counter()
    file_counter = Counter()
    concept_counter = Counter()
    recent_entries = []
    
    # Process each collection
    for col in collections:
        try:
            info = client.get_collection(col.name)
            if info.points_count == 0:
                continue
                
            # Get all points from collection
            result = client.scroll(
                collection_name=col.name,
                limit=100,  # Get up to 100 points per collection
                with_payload=True,
                with_vectors=False
            )
            
            for point in result[0]:
                payload = point.payload
                entry_data = {
                    'collection': col.name,
                    'point_id': str(point.id),
                    'timestamp': None,
                    'content_preview': '',
                    'topics': [],
                    'metadata': {}
                }
                
                # Extract timestamp if available
                if 'timestamp' in payload:
                    entry_data['timestamp'] = payload['timestamp']
                elif 'metadata' in payload and 'timestamp' in payload['metadata']:
                    entry_data['timestamp'] = payload['metadata']['timestamp']
                
                # Extract content
                if 'content' in payload:
                    content = str(payload['content'])
                    entry_data['content_preview'] = content[:200]
                    entry_data['topics'] = extract_topics_from_content(content)
                    topic_counter.update(entry_data['topics'])
                
                # Extract metadata
                if 'metadata' in payload:
                    meta = payload['metadata']
                    entry_data['metadata'] = meta
                    
                    # Count concepts
                    if 'concepts' in meta and isinstance(meta['concepts'], list):
                        concept_counter.update(meta['concepts'])
                    
                    # Count files
                    if 'files_analyzed' in meta and isinstance(meta['files_analyzed'], list):
                        # Clean file paths to just filename
                        clean_files = [f.split('/')[-1] for f in meta['files_analyzed'] if f]
                        file_counter.update(clean_files)
                    
                    # Extract conversation ID as timestamp proxy
                    if 'conversation_id' in meta:
                        entry_data['conv_id'] = meta['conversation_id']
                
                all_data.append(entry_data)
                
        except Exception as e:
            continue
    
    # Sort by timestamp if available (newest first)
    # Collections with 'local' or recent voyage are likely newer
    all_data.sort(key=lambda x: (
        '_local' in x['collection'],  # Prioritize local collections (newer)
        x['collection']  # Then by collection name
    ), reverse=True)
    
    return {
        'entries': all_data[:20],  # Top 20 most recent
        'topics': topic_counter.most_common(15),
        'concepts': concept_counter.most_common(15),
        'files': file_counter.most_common(10),
        'total_points': len(all_data)
    }

def display_results(data):
    """Display the results in a useful format"""
    
    print("=" * 60)
    print("QDRANT RECENT TOPICS & SEARCH SUGGESTIONS")
    print("=" * 60)
    
    print(f"\n📊 Total indexed points: {data['total_points']}")
    
    # Show top topics
    if data['topics']:
        print("\n🔥 HOT TOPICS (from content analysis):")
        print("-" * 40)
        for topic, count in data['topics'][:10]:
            print(f"  {topic:15} : {count:3} occurrences")
        
        print("\n💡 Try these searches:")
        for topic, _ in data['topics'][:5]:
            print(f'  mcp__claude-self-reflect__reflect_on_past("{topic}")')
    
    # Show concepts from metadata
    if data['concepts']:
        print("\n🏷️  INDEXED CONCEPTS (from metadata):")
        print("-" * 40)
        for concept, count in data['concepts'][:10]:
            print(f"  {concept:15} : {count:3} occurrences")
        
        print("\n💡 Try these concept searches:")
        for concept, _ in data['concepts'][:3]:
            print(f'  mcp__claude-self-reflect__search_by_concept("{concept}")')
    
    # Show files
    if data['files']:
        print("\n📁 MOST DISCUSSED FILES:")
        print("-" * 40)
        for file, count in data['files'][:5]:
            print(f"  {file[:40]:40} : {count:3} refs")
    
    # Show recent entry samples
    if data['entries']:
        print("\n📝 RECENT ENTRY SAMPLES:")
        print("-" * 40)
        
        for i, entry in enumerate(data['entries'][:5], 1):
            print(f"\n{i}. Collection: {entry['collection']}")
            
            if entry['topics']:
                print(f"   Topics: {', '.join(entry['topics'][:3])}")
            
            if 'project' in entry['metadata']:
                print(f"   Project: {entry['metadata']['project']}")
            
            if entry['content_preview']:
                preview = entry['content_preview'][:80].replace('\n', ' ')
                print(f"   Content: {preview}...")
    
    # Suggest combined searches
    print("\n🎯 SMART SEARCH COMBINATIONS:")
    print("-" * 40)
    
    if len(data['topics']) >= 2:
        t1, t2 = data['topics'][0][0], data['topics'][1][0]
        print(f'  "{t1} {t2}"')
    
    if data['topics'] and data['concepts']:
        topic = data['topics'][0][0]
        concept = data['concepts'][0][0] if data['concepts'] else 'debugging'
        print(f'  "{topic} {concept}"')
    
    print("\n✨ SEARCH TIPS:")
    print("-" * 40)
    print("  1. Use hot topics for broad searches")
    print("  2. Use concepts for technical searches") 
    print("  3. Combine terms for precision")
    print("  4. Add 'evolution' or 'learning' for historical context")
    print("  5. Use file names for specific code discussions")

if __name__ == "__main__":
    print("🔍 Analyzing Qdrant for recent topics and search opportunities...")
    print("=" * 60)
    
    try:
        data = get_recent_qdrant_data()
        display_results(data)
    except Exception as e:
        print(f"Error: {e}")
        import traceback
        traceback.print_exc()
#!/usr/bin/env python3
"""Comprehensive test suite for claude-self-reflect after fixes"""

import sys
import os
import hashlib
import json
from pathlib import Path

# Add scripts to path
sys.path.insert(0, 'scripts')
sys.path.insert(0, 'mcp-server/src')

from qdrant_client import QdrantClient
from utils import normalize_project_name

# Initialize Qdrant client
client = QdrantClient(url="http://localhost:6333")

def test_collection_naming():
    """Test that collections use correct normalized names"""
    print("\n=== Testing Collection Naming ===")
    
    # Test cases
    test_projects = {
        '-Users-ramakrishnanannaswamy-projects-claude-self-reflect': ('claude-self-reflect', '7f6df0fc'),
        '-Users-ramakrishnanannaswamy-projects-metafora-Lion': ('metafora-Lion', '75645341'),
        '-Users-ramakrishnanannaswamy-projects-procsolve-website': ('procsolve-website', '9f2f312b'),
    }
    
    results = []
    for project_path, (expected_name, expected_hash) in test_projects.items():
        normalized = normalize_project_name(project_path)
        actual_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
        collection_name = f"conv_{actual_hash}_local"
        
        # Check if collection exists
        collections = client.get_collections().collections
        exists = any(c.name == collection_name for c in collections)
        
        result = {
            'project': project_path.split('-')[-1],
            'normalized': normalized,
            'expected': expected_name,
            'hash': actual_hash,
            'expected_hash': expected_hash,
            'collection': collection_name,
            'exists': exists,
            'pass': normalized == expected_name and actual_hash == expected_hash and exists
        }
        results.append(result)
        
        print(f"  {result['project']}:")
        print(f"    Normalized: {result['normalized']} {'✅' if result['normalized'] == expected_name else '❌'}")
        print(f"    Hash: {result['hash']} {'✅' if result['hash'] == expected_hash else '❌'}")
        print(f"    Collection exists: {result['exists']} {'✅' if result['exists'] else '❌'}")
    
    return all(r['pass'] for r in results)

def test_metadata_extraction():
    """Test that metadata is properly extracted"""
    print("\n=== Testing Metadata Extraction ===")
    
    # Check a collection for metadata
    collection_name = "conv_7f6df0fc_local"  # claude-self-reflect
    
    try:
        # Get a sample point
        result = client.scroll(
            collection_name=collection_name,
            limit=5,
            with_payload=True
        )
        
        if result[0]:
            metadata_checks = {
                'has_ast_elements': False,
                'has_message_index': False,
                'has_total_messages': False,
                'has_concepts': False,
                'has_files_analyzed': False,
                'has_tools_used': False,
                'has_code_blocks_flag': False
            }
            
            for point in result[0]:
                payload = point.payload
                
                # Check for AST elements
                if 'ast_elements' in payload and payload['ast_elements']:
                    metadata_checks['has_ast_elements'] = True
                    print(f"  AST elements found: {len(payload['ast_elements'])} elements")
                    
                # Check message indexing
                if 'message_index' in payload:
                    metadata_checks['has_message_index'] = True
                    
                if 'total_messages' in payload:
                    metadata_checks['has_total_messages'] = True
                    
                # Check concepts
                if 'concepts' in payload and payload['concepts']:
                    metadata_checks['has_concepts'] = True
                    print(f"  Concepts: {', '.join(payload['concepts'][:5])}")
                    
                # Check files
                if 'files_analyzed' in payload and payload['files_analyzed']:
                    metadata_checks['has_files_analyzed'] = True
                    
                # Check tools
                if 'tools_used' in payload and payload['tools_used']:
                    metadata_checks['has_tools_used'] = True
                    
                # Check code blocks flag
                if 'has_code_blocks' in payload:
                    metadata_checks['has_code_blocks_flag'] = True
            
            print("\n  Metadata Status:")
            for check, status in metadata_checks.items():
                print(f"    {check.replace('_', ' ').title()}: {'✅' if status else '❌'}")
            
            return sum(metadata_checks.values()) >= 5  # At least 5 metadata types present
        
    except Exception as e:
        print(f"  Error checking metadata: {e}")
        return False

def test_search_functionality():
    """Test that search works with new metadata"""
    print("\n=== Testing Search Functionality ===")
    
    try:
        # Directly test search using Qdrant client
        collection_name = "conv_7f6df0fc_local"  # claude-self-reflect
        
        # Test 1: Verify points have searchable metadata
        result = client.scroll(
            collection_name=collection_name,
            limit=10,
            with_payload=True
        )
        
        if not result[0]:
            print("  ❌ No points found in collection")
            return False
        
        # Check metadata completeness
        metadata_checks = {
            'has_message_index': False,
            'has_total_messages': False,
            'has_concepts': False,
            'has_ast_elements': False,
            'has_files_analyzed': False,
            'has_tools_used': False
        }
        
        for point in result[0]:
            payload = point.payload
            if 'message_index' in payload:
                metadata_checks['has_message_index'] = True
            if 'total_messages' in payload:
                metadata_checks['has_total_messages'] = True
            if 'concepts' in payload and payload['concepts']:
                metadata_checks['has_concepts'] = True
            if 'ast_elements' in payload and payload['ast_elements']:
                metadata_checks['has_ast_elements'] = True
            if 'files_analyzed' in payload and payload['files_analyzed']:
                metadata_checks['has_files_analyzed'] = True
            if 'tools_used' in payload and payload['tools_used']:
                metadata_checks['has_tools_used'] = True
        
        # Test 2: Perform actual semantic search
        print("  Testing semantic search capability:")
        
        # Get a sample embedding dimension from existing point
        sample_point = result[0][0]
        vector_dim = len(sample_point.vector) if sample_point.vector else 384
        
        # Create a dummy query vector for testing search
        import random
        query_vector = [random.random() for _ in range(vector_dim)]
        
        search_result = client.search(
            collection_name=collection_name,
            query_vector=query_vector,
            limit=3
        )
        
        if search_result:
            print(f"    ✅ Search returned {len(search_result)} results")
            
            # Check if results have expected metadata
            for i, res in enumerate(search_result[:2], 1):
                if res.payload:
                    print(f"    Result {i} score: {res.score:.3f}")
                    if 'message_index' in res.payload:
                        print(f"      - Has message indexing")
                    if 'concepts' in res.payload and res.payload['concepts']:
                        print(f"      - Has concepts: {', '.join(res.payload['concepts'][:3])}")
        else:
            print("    ❌ Search returned no results")
            
        # Evaluate overall search functionality
        all_metadata_present = all(metadata_checks.values())
        search_works = len(search_result) > 0 if search_result else False
        
        if all_metadata_present and search_works:
            print("\n  ✅ Search functionality FULLY OPERATIONAL")
            print(f"    - All metadata types present: {all_metadata_present}")
            print(f"    - Search returns results: {search_works}")
            return True
        else:
            print("\n  ⚠️ Search functionality PARTIAL")
            for check, status in metadata_checks.items():
                if not status:
                    print(f"    - Missing: {check}")
            return False
            
    except Exception as e:
        print(f"  Error testing search: {e}")
        return False

def test_import_status():
    """Check overall import status"""
    print("\n=== Import Status ===")
    
    # Check collections
    collections = client.get_collections().collections
    print(f"  Total collections: {len(collections)}")
    
    # Count points
    total_points = 0
    for collection in collections:
        if collection.name.startswith('conv_'):
            info = client.get_collection(collection.name)
            total_points += info.points_count
    
    print(f"  Total points: {total_points}")
    
    # Check state file
    state_file = Path.home() / ".claude-self-reflect" / "config" / "imported-files.json"
    if state_file.exists():
        with open(state_file) as f:
            state = json.load(f)
            print(f"  Files imported: {len(state.get('imported_files', {}))}")
    
    return total_points > 0

def main():
    """Run all tests"""
    print("=" * 60)
    print("COMPREHENSIVE TEST SUITE - Claude Self-Reflect")
    print("=" * 60)
    
    results = {
        'Collection Naming': test_collection_naming(),
        'Metadata Extraction': test_metadata_extraction(),
        'Search Functionality': test_search_functionality(),
        'Import Status': test_import_status()
    }
    
    print("\n" + "=" * 60)
    print("TEST RESULTS SUMMARY")
    print("=" * 60)
    
    for test_name, passed in results.items():
        status = "✅ PASS" if passed else "❌ FAIL"
        print(f"  {test_name}: {status}")
    
    all_passed = all(results.values())
    
    print("\n" + "=" * 60)
    if all_passed:
        print("🎉 ALL TESTS PASSED!")
    else:
        print("⚠️ SOME TESTS FAILED - Review results above")
    print("=" * 60)
    
    return 0 if all_passed else 1

if __name__ == "__main__":
    sys.exit(main())
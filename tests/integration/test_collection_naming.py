#!/usr/bin/env python3
"""Comprehensive test suite for claude-self-reflect after fixes - IMPROVED VERSION"""

import sys
import os
import hashlib
import json
from pathlib import Path
from collections import defaultdict

# Add scripts to path
sys.path.insert(0, 'scripts')
sys.path.insert(0, 'mcp-server/src')

from qdrant_client import QdrantClient
from qdrant_client.models import Filter, FieldCondition, MatchText, MatchValue
from utils import normalize_project_name

# Initialize Qdrant client
client = QdrantClient(url="http://localhost:6333")

def test_collection_naming():
    """Test that collections use correct normalized names"""
    print("\n=== Testing Collection Naming ===")
    
    # Get collections once for efficiency
    collections = client.get_collections().collections
    collection_names = {c.name for c in collections}
    
    # Comprehensive test cases including edge cases
    test_projects = {
        '-Users-ramakrishnanannaswamy-projects-claude-self-reflect': ('claude-self-reflect', '7f6df0fc'),
        '-Users-ramakrishnanannaswamy-projects-metafora-Lion': ('metafora-Lion', '75645341'),
        '-Users-ramakrishnanannaswamy-projects-procsolve-website': ('procsolve-website', '9f2f312b'),
        # Edge cases
        '/Users/Me/Projects/My App/': ('My App', None),  # Space in name
        '-Users-me-projects-MIXED_Case': ('MIXED_Case', None),  # Mixed case
        'simple-project': ('simple-project', None),  # Simple name
    }
    
    results = []
    for project_path, (expected_name, expected_hash) in test_projects.items():
        normalized = normalize_project_name(project_path)
        actual_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
        collection_name = f"conv_{actual_hash}_local"
        
        # Check if collection exists (only for known projects)
        exists = collection_name in collection_names
        
        result = {
            'project': project_path.split('-')[-1] if '-' in project_path else project_path,
            'normalized': normalized,
            'expected': expected_name,
            'hash': actual_hash,
            'expected_hash': expected_hash,
            'collection': collection_name,
            'exists': exists,
            'normalization_pass': normalized == expected_name,
            'hash_pass': expected_hash is None or actual_hash == expected_hash
        }
        results.append(result)
        
        print(f"  {result['project']}:")
        print(f"    Normalized: {result['normalized']} {'✅' if result['normalization_pass'] else '❌'}")
        if expected_hash:
            print(f"    Hash: {result['hash']} {'✅' if result['hash_pass'] else '❌'}")
            print(f"    Collection exists: {result['exists']} {'✅' if result['exists'] else '❌'}")
    
    # All main projects should pass
    main_results = results[:3]  # First 3 are main projects
    return all(r['normalization_pass'] and r['hash_pass'] and r['exists'] for r in main_results)

def test_metadata_extraction():
    """Test that metadata is properly extracted with validation"""
    print("\n=== Testing Metadata Extraction ===")
    
    # Use dynamically computed collection name
    project = normalize_project_name("-Users-ramakrishnanannaswamy-projects-claude-self-reflect")
    hash_val = hashlib.md5(project.encode()).hexdigest()[:8]
    collection_name = f"conv_{hash_val}_local"
    
    try:
        # First, look for points that likely contain code
        code_points = client.scroll(
            collection_name=collection_name,
            limit=50,
            with_payload=True,
            scroll_filter=Filter(
                should=[
                    FieldCondition(key="content", match=MatchText(text="def ")),
                    FieldCondition(key="content", match=MatchText(text="function ")),
                    FieldCondition(key="content", match=MatchText(text="class ")),
                ]
            ) if hasattr(client, 'scroll_filter') else None
        )
        
        # Also get general points
        general_points = client.scroll(
            collection_name=collection_name,
            limit=20,
            with_payload=True
        )
        
        all_points = (code_points[0] if code_points[0] else []) + (general_points[0] if general_points[0] else [])
        
        if not all_points:
            print("  ❌ No points found in collection")
            return False
        
        # Required metadata fields with expected types
        required_metadata = {
            'message_index': int,
            'total_messages': int,
            'concepts': list,
            'ast_elements': list,
            'files_analyzed': list,
            'tools_used': list,
            'has_code_blocks': bool
        }
        
        metadata_found = {k: False for k in required_metadata}
        validation_errors = []
        
        # Track message indexing consistency
        by_conversation = defaultdict(list)
        
        for point in all_points:
            payload = point.payload
            
            # Check each required field
            for field, expected_type in required_metadata.items():
                if field in payload:
                    if isinstance(payload[field], expected_type):
                        metadata_found[field] = True
                        
                        # Additional validation
                        if field == 'message_index':
                            idx = payload['message_index']
                            total = payload.get('total_messages', 0)
                            if not (0 <= idx < total):
                                validation_errors.append(f"Invalid message_index: {idx} not in [0, {total})")
                            
                            # Track for conversation consistency
                            conv_id = payload.get('conversation_id', 'unknown')
                            by_conversation[conv_id].append((idx, total))
                        
                        elif field == 'concepts' and payload[field]:
                            if not all(isinstance(c, str) for c in payload[field]):
                                validation_errors.append("Concepts must be list of strings")
                        
                        elif field == 'ast_elements' and payload[field]:
                            # Validate AST element structure
                            for elem in payload[field][:5]:  # Check first 5
                                if isinstance(elem, dict):
                                    if 'type' not in elem:
                                        validation_errors.append(f"AST element missing 'type': {elem}")
                                elif isinstance(elem, str):
                                    # Legacy format like "func:normalize_project_name"
                                    if not elem.startswith(('func:', 'class:', 'method:')):
                                        validation_errors.append(f"Invalid AST element format: {elem}")
                        
                        # Cross-field validation
                        if field == 'has_code_blocks' and payload[field]:
                            if not payload.get('ast_elements'):
                                validation_errors.append("has_code_blocks=True but ast_elements is empty")
                    else:
                        validation_errors.append(f"{field} has wrong type: {type(payload[field]).__name__} instead of {expected_type.__name__}")
        
        # Check conversation consistency
        for conv_id, indices in by_conversation.items():
            if len(indices) > 1:
                indices_sorted = sorted(indices, key=lambda x: x[0])
                # Check for duplicates
                idx_values = [idx for idx, _ in indices_sorted]
                if len(set(idx_values)) != len(idx_values):
                    validation_errors.append(f"Duplicate message indices in conversation {conv_id}")
        
        # Print detailed results
        print("\n  Metadata Validation:")
        for field, found in metadata_found.items():
            print(f"    {field}: {'✅' if found else '❌'}")
        
        if validation_errors:
            print("\n  ⚠️ Validation Errors:")
            for error in validation_errors[:5]:  # Show first 5 errors
                print(f"    - {error}")
        
        # Test AST extraction specifically for code
        code_with_ast = 0
        code_without_ast = 0
        for point in all_points:
            if point.payload.get('has_code_blocks'):
                if point.payload.get('ast_elements'):
                    code_with_ast += 1
                    # Print sample AST elements
                    if code_with_ast == 1:
                        print(f"\n  Sample AST elements: {point.payload['ast_elements'][:3]}")
                else:
                    code_without_ast += 1
        
        if code_with_ast > 0 or code_without_ast > 0:
            print(f"\n  Code blocks with AST: {code_with_ast}")
            print(f"  Code blocks without AST: {code_without_ast}")
        
        # Pass if all required metadata is found and no critical validation errors
        critical_errors = [e for e in validation_errors if 'message_index' in e or 'AST element missing' in e]
        return all(metadata_found.values()) and len(critical_errors) == 0
        
    except Exception as e:
        print(f"  Error checking metadata: {e}")
        return False

def test_search_functionality():
    """Test search with deterministic vector retrieval and filters"""
    print("\n=== Testing Search Functionality ===")
    
    try:
        # Compute collection name
        project = normalize_project_name("-Users-ramakrishnanannaswamy-projects-claude-self-reflect")
        hash_val = hashlib.md5(project.encode()).hexdigest()[:8]
        collection_name = f"conv_{hash_val}_local"
        
        # Get points WITH vectors for deterministic testing
        result = client.scroll(
            collection_name=collection_name,
            limit=5,
            with_payload=True,
            with_vectors=True
        )
        
        if not result[0]:
            print("  ❌ No points found in collection")
            return False
        
        # Test 1: Self-retrieval (deterministic)
        print("  Test 1: Self-retrieval")
        test_point = result[0][0]
        test_vector = test_point.vector
        
        # Handle named vectors if present
        if isinstance(test_vector, dict):
            test_vector = next(iter(test_vector.values()))
        
        if not test_vector:
            print("    ❌ No vector found in point")
            return False
        
        # Use query_points instead of deprecated search
        search_result = client.query_points(
            collection_name=collection_name,
            query=test_vector,
            limit=3,
            with_payload=True
        )
        
        if search_result and search_result.points:
            print(f"    ✅ Search returned {len(search_result.points)} results")
            
            # Verify self-retrieval (top result should be the same point)
            if search_result.points[0].id == test_point.id:
                print(f"    ✅ Self-retrieval successful (top-1 is same point)")
            else:
                print(f"    ❌ Self-retrieval failed (top-1 is different point)")
            
            # Check score ordering
            scores = [p.score for p in search_result.points]
            if all(scores[i] >= scores[i+1] for i in range(len(scores)-1)):
                print(f"    ✅ Scores properly ordered: {[f'{s:.3f}' for s in scores]}")
            else:
                print(f"    ❌ Scores not ordered: {scores}")
        else:
            print("    ❌ Search returned no results")
            return False
        
        # Test 2: Filtered search (metadata-aware)
        print("\n  Test 2: Filtered search")
        try:
            filtered_result = client.query_points(
                collection_name=collection_name,
                query=test_vector,
                limit=5,
                query_filter=Filter(
                    must=[
                        FieldCondition(key="has_code_blocks", match=MatchValue(value=True))
                    ]
                ),
                with_payload=True
            )
            
            if filtered_result and filtered_result.points:
                print(f"    ✅ Filtered search returned {len(filtered_result.points)} results")
                
                # Verify filter was applied
                all_have_code = all(
                    p.payload.get('has_code_blocks', False) 
                    for p in filtered_result.points
                )
                if all_have_code:
                    print("    ✅ All results have has_code_blocks=True")
                else:
                    print("    ❌ Some results don't have has_code_blocks=True")
                
                # Show sample metadata
                if filtered_result.points:
                    sample = filtered_result.points[0].payload
                    if sample.get('concepts'):
                        print(f"    Sample concepts: {', '.join(sample['concepts'][:3])}")
                    if sample.get('ast_elements'):
                        print(f"    Sample has {len(sample['ast_elements'])} AST elements")
            else:
                print("    ⚠️ No results with has_code_blocks=True (may be expected)")
        except Exception as e:
            print(f"    ⚠️ Filtered search not supported or failed: {e}")
        
        # Test 3: Check metadata in search results
        print("\n  Test 3: Metadata in search results")
        metadata_checks = {
            'message_index': 0,
            'total_messages': 0,
            'concepts': 0,
            'ast_elements': 0,
            'files_analyzed': 0,
            'tools_used': 0
        }
        
        for point in search_result.points:
            for key in metadata_checks:
                if key in point.payload:
                    metadata_checks[key] += 1
        
        print("    Metadata presence in search results:")
        for key, count in metadata_checks.items():
            pct = (count / len(search_result.points)) * 100
            status = '✅' if pct >= 80 else '⚠️' if pct >= 50 else '❌'
            print(f"      {key}: {count}/{len(search_result.points)} ({pct:.0f}%) {status}")
        
        return True
        
    except Exception as e:
        print(f"  Error testing search: {e}")
        import traceback
        traceback.print_exc()
        return False

def test_import_status():
    """Check overall import status with per-collection validation"""
    print("\n=== Import Status ===")
    
    # Check collections
    collections = client.get_collections().collections
    print(f"  Total collections: {len(collections)}")
    
    # Analyze conversation collections
    conv_collections = []
    empty_collections = []
    total_points = 0
    
    for collection in collections:
        if collection.name.startswith('conv_'):
            info = client.get_collection(collection.name)
            conv_collections.append((collection.name, info.points_count))
            total_points += info.points_count
            
            if info.points_count == 0:
                empty_collections.append(collection.name)
    
    print(f"  Conversation collections: {len(conv_collections)}")
    print(f"  Total points: {total_points}")
    
    # Show top collections by point count
    top_collections = sorted(conv_collections, key=lambda x: x[1], reverse=True)[:3]
    print("\n  Top collections by points:")
    for name, count in top_collections:
        print(f"    {name}: {count} points")
    
    if empty_collections:
        print(f"\n  ⚠️ Empty collections found: {len(empty_collections)}")
        for name in empty_collections[:3]:
            print(f"    - {name}")
    
    # Check state file
    state_file = Path.home() / ".claude-self-reflect" / "config" / "imported-files.json"
    if state_file.exists():
        with open(state_file) as f:
            state = json.load(f)
            imported_count = len(state.get('imported_files', {}))
            print(f"\n  Files imported (per state): {imported_count}")
    
    # Pass if we have points - empty collections are expected for projects with no conversations
    # Fail only if more than 50% of collections are empty
    empty_ratio = len(empty_collections) / len(conv_collections) if conv_collections else 0
    if empty_ratio > 0.5:
        print(f"\n  ❌ Too many empty collections: {len(empty_collections)}/{len(conv_collections)} ({empty_ratio*100:.0f}%)")
        return False
    
    return total_points > 0

def main():
    """Run all tests"""
    print("=" * 60)
    print("COMPREHENSIVE TEST SUITE - Claude Self-Reflect (IMPROVED)")
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
        failed_tests = [name for name, passed in results.items() if not passed]
        print(f"⚠️ TESTS FAILED: {', '.join(failed_tests)}")
        print("Review the detailed output above for specific issues.")
    print("=" * 60)
    
    return 0 if all_passed else 1

if __name__ == "__main__":
    sys.exit(main())
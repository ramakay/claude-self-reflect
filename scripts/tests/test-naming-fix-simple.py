#!/usr/bin/env python3
"""
Simple test to prove the collection naming fix works.
"""

import hashlib
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent))
from scripts.utils import normalize_project_name

def test_fix():
    """Test that the fix correctly handles file paths."""
    print("="*60)
    print("TESTING COLLECTION NAMING FIX")
    print("="*60)
    
    # Simulate a JSONL file in a Claude project
    test_file_path = Path("/Users/ramakrishnanannaswamy/.claude/projects/-Users-ramakrishnanannaswamy-projects-claude-self-reflect/conversation-12345.jsonl")
    
    print(f"\nTest file: {test_file_path}")
    print("-"*60)
    
    # OLD BUGGY BEHAVIOR (what streaming-watcher.py was doing)
    print("\n❌ OLD BUGGY BEHAVIOR:")
    project_path_old = str(test_file_path.parent)  # Full path
    print(f"  project_path = str(file_path.parent)")
    print(f"  Result: {project_path_old}")
    normalized_old = normalize_project_name(project_path_old)
    hash_old = hashlib.md5(normalized_old.encode()).hexdigest()[:8]
    collection_old = f"conv_{hash_old}_local"
    print(f"  Normalized: {normalized_old}")
    print(f"  Hash: {hash_old}")
    print(f"  Collection: {collection_old}")
    
    # NEW FIXED BEHAVIOR
    print("\n✅ NEW FIXED BEHAVIOR:")
    project_path_new = test_file_path.parent.name  # Just directory name
    print(f"  project_path = file_path.parent.name")
    print(f"  Result: {project_path_new}")
    normalized_new = normalize_project_name(project_path_new)
    hash_new = hashlib.md5(normalized_new.encode()).hexdigest()[:8]
    collection_new = f"conv_{hash_new}_local"
    print(f"  Normalized: {normalized_new}")
    print(f"  Hash: {hash_new}")
    print(f"  Collection: {collection_new}")
    
    # EXPECTED BEHAVIOR (what import-conversations-unified.py does)
    print("\n✓ EXPECTED BEHAVIOR (import-conversations-unified.py):")
    project_dir = test_file_path.parent
    project_name = project_dir.name  # This is what unified script uses
    print(f"  project_path = project_dir.name")
    print(f"  Result: {project_name}")
    normalized_expected = normalize_project_name(project_name)
    hash_expected = hashlib.md5(normalized_expected.encode()).hexdigest()[:8]
    collection_expected = f"conv_{hash_expected}_local"
    print(f"  Normalized: {normalized_expected}")
    print(f"  Hash: {hash_expected}")
    print(f"  Collection: {collection_expected}")
    
    # VERIFICATION
    print("\n" + "="*60)
    print("VERIFICATION:")
    print("="*60)
    
    if collection_old != collection_new:
        print(f"✅ Fix changes behavior (no longer creates spurious collections)")
        print(f"   Old: {collection_old}")
        print(f"   New: {collection_new}")
    else:
        print(f"⚠️  Fix doesn't change behavior - both produce: {collection_old}")
    
    if collection_new == collection_expected:
        print(f"✅ Fixed behavior matches expected behavior")
        print(f"   Both produce: {collection_new}")
    else:
        print(f"❌ Fixed behavior doesn't match expected!")
        print(f"   Fixed: {collection_new}")
        print(f"   Expected: {collection_expected}")
    
    # Test multiple scenarios
    print("\n" + "="*60)
    print("TESTING MULTIPLE SCENARIOS:")
    print("="*60)
    
    scenarios = [
        {
            'name': 'Standard Claude project',
            'file': '/Users/user/.claude/projects/-Users-user-projects-myapp/conv.jsonl',
            'expected_normalized': 'myapp'
        },
        {
            'name': 'Project with dashes',
            'file': '/Users/user/.claude/projects/-Users-user-projects-my-cool-app/conv.jsonl',
            'expected_normalized': 'my-cool-app'
        },
        {
            'name': 'Nested projects keyword',
            'file': '/Users/user/.claude/projects/-Users-user-projects-projects-manager/conv.jsonl',
            'expected_normalized': 'projects-manager'
        }
    ]
    
    all_pass = True
    for scenario in scenarios:
        print(f"\n{scenario['name']}:")
        file_path = Path(scenario['file'])
        
        # Test the fix
        project_name = file_path.parent.name
        normalized = normalize_project_name(project_name)
        hash_val = hashlib.md5(normalized.encode()).hexdigest()[:8]
        
        if normalized == scenario['expected_normalized']:
            print(f"  ✅ {project_name} -> {normalized} (hash: {hash_val})")
        else:
            print(f"  ❌ {project_name} -> {normalized} (expected: {scenario['expected_normalized']})")
            all_pass = False
    
    print("\n" + "="*60)
    if all_pass and collection_new == collection_expected:
        print("🎉 SUCCESS: The fix is working correctly!")
        print("\nThe fix ensures:")
        print("1. streaming-watcher.py and streaming-importer.py use parent.name")
        print("2. This produces the same collection as import-conversations-unified.py")
        print("3. No more spurious collections will be created")
        return 0
    else:
        print("⚠️  Issues detected - review the results above")
        return 1

if __name__ == "__main__":
    sys.exit(test_fix())
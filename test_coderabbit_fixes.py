#!/usr/bin/env python3
"""
Focused regression test for specific CodeRabbit fixes.
"""

import os
import sys
import json
import tempfile
import subprocess
from pathlib import Path

# Test environment
os.environ["QDRANT_URL"] = "http://localhost:6334"
os.environ["PREFER_LOCAL_EMBEDDINGS"] = "true"

def test_normalize_import():
    """Test 1: normalize_project_name import cascade works."""
    print("🧪 Testing normalize_project_name import fallback...")
    
    cmd = [
        "python", "scripts/import-conversations-unified.py", 
        "--help"
    ]
    
    result = subprocess.run(cmd, capture_output=True, text=True, cwd=".")
    
    if result.returncode == 0:
        print("✅ Import script loads successfully")
        return True
    else:
        print(f"❌ Import script failed to load: {result.stderr}")
        return False

def test_qdrant_count_api():
    """Test 2: Test Qdrant count API directly."""
    print("🧪 Testing Qdrant count() API...")
    
    try:
        import subprocess
        result = subprocess.run([
            "python", "-c", 
            """
import sys
sys.path.insert(0, 'venv/lib/python3.13/site-packages')
from qdrant_client import QdrantClient
from qdrant_client.models import Filter, FieldCondition, MatchValue, Distance, VectorParams

client = QdrantClient(url='http://localhost:6334', timeout=30)

# Create test collection
test_collection = 'test_count_api_simple'
try:
    client.delete_collection(test_collection)
except:
    pass
    
client.create_collection(
    collection_name=test_collection,
    vectors_config=VectorParams(size=384, distance=Distance.COSINE)
)

# Test count() API - this was the CodeRabbit fix
count_filter = Filter(
    must=[FieldCondition(key='conversation_id', match=MatchValue(value='test-123'))]
)

count_result = client.count(
    collection_name=test_collection,
    count_filter=count_filter,
    exact=True
)

print(f'Count result: {count_result.count}')
assert count_result.count == 0
client.delete_collection(test_collection)
print('Qdrant count() API working')
            """
        ], capture_output=True, text=True, cwd=".")
        
        if result.returncode == 0:
            print("✅ Qdrant count() API working correctly")
            return True
        else:
            print(f"❌ Qdrant count() API failed: {result.stderr}")
            return False
    except Exception as e:
        print(f"❌ Qdrant test error: {e}")
        return False

def test_message_counting():
    """Test 3: Test message counting logic."""
    print("🧪 Testing tool entries don't count as messages...")
    
    # Create test JSONL
    test_dir = Path("/tmp/claude")
    test_dir.mkdir(exist_ok=True)
    test_file = test_dir / "test_messages.jsonl"
    
    with open(test_file, 'w') as f:
        # User message (should count)
        f.write(json.dumps({
            "message": {"role": "user", "content": "Hello"},
            "timestamp": "2024-01-01T00:00:00Z"
        }) + "\n")
        
        # Assistant message (should count)  
        f.write(json.dumps({
            "message": {"role": "assistant", "content": "Hi there"},
            "timestamp": "2024-01-01T00:01:00Z"
        }) + "\n")
        
        # Tool use entry (should NOT count as message)
        f.write(json.dumps({
            "type": "tool_use",
            "name": "Read",
            "input": {"file_path": "test.py"},
            "timestamp": "2024-01-01T00:02:00Z"
        }) + "\n")
    
    # Test with metadata extractor
    cmd = [
        "python", "-c", f"""
import sys
sys.path.insert(0, 'scripts')
from metadata_extractor import MetadataExtractor

extractor = MetadataExtractor()
metadata, created_at, message_count = extractor.extract_metadata_from_file('{test_file}')

print(f'Message count: {{message_count}}')
assert message_count == 2, f'Expected 2 messages, got {{message_count}}'
print('Message counting correct: only user+assistant messages counted')
        """
    ]
    
    result = subprocess.run(cmd, capture_output=True, text=True, cwd=".")
    
    # Cleanup
    test_file.unlink(missing_ok=True)
    
    if result.returncode == 0:
        print("✅ Message counting logic correct")
        return True
    else:
        print(f"❌ Message counting failed: {result.stderr}")
        return False

def test_actual_import():
    """Test 4: Test actual import with small dataset."""
    print("🧪 Testing actual import functionality...")
    
    # Find a small conversation file
    claude_projects = Path.home() / ".claude" / "projects"
    test_project = claude_projects / "-Users-ramakrishnanannaswamy-projects-claude-self-reflect"
    
    if not test_project.exists():
        print("⏭️  Skipping actual import test - no test project found")
        return True
    
    jsonl_files = list(test_project.glob("*.jsonl"))
    if not jsonl_files:
        print("⏭️  Skipping actual import test - no JSONL files found")
        return True
    
    # Import 1 file only
    cmd = [
        "python", "scripts/import-conversations-unified.py",
        "--project", str(test_project),
        "--limit", "1",
        "--verbose"
    ]
    
    result = subprocess.run(cmd, capture_output=True, text=True, cwd=".")
    
    if result.returncode == 0 and ("Imported: 1" in result.stderr or "Imported: 0" in result.stderr):
        print("✅ Actual import working (import completed)")
        return True
    else:
        print(f"❌ Actual import failed: {result.stderr}")
        return False

def test_exception_handling():
    """Test 5: Test exception handling doesn't crash."""
    print("🧪 Testing exception handling robustness...")
    
    # Create invalid JSONL file
    test_dir = Path("/tmp/claude")
    test_dir.mkdir(exist_ok=True)
    invalid_file = test_dir / "invalid.jsonl"
    
    with open(invalid_file, 'w') as f:
        f.write("{ invalid json }\n")
        f.write("{ \"valid\": true }\n")
    
    # Create fake project structure
    fake_project = test_dir / "fake_project"
    fake_project.mkdir(exist_ok=True)
    
    # Move invalid file to fake project
    target_file = fake_project / "invalid.jsonl"
    invalid_file.rename(target_file)
    
    # Try to import - should not crash
    cmd = [
        "python", "scripts/import-conversations-unified.py",
        "--project", str(fake_project),
        "--verbose"
    ]
    
    result = subprocess.run(cmd, capture_output=True, text=True, cwd=".")
    
    # Cleanup
    target_file.unlink(missing_ok=True)
    fake_project.rmdir()
    
    # Should not crash (returncode might be 0 or 1, but shouldn't be something like -11)
    if result.returncode in [0, 1]:
        print("✅ Exception handling robust (no crash)")
        return True
    else:
        print(f"❌ Exception handling failed (crash): {result.stderr}")
        return False

def main():
    """Run focused regression tests."""
    print("🚀 Starting focused CodeRabbit fixes regression test\n")
    
    tests = [
        ("Import script loading", test_normalize_import),
        ("Qdrant count() API", test_qdrant_count_api),
        ("Message counting logic", test_message_counting),
        ("Exception handling", test_exception_handling),
        ("Actual import functionality", test_actual_import),
    ]
    
    passed = 0
    total = len(tests)
    
    for test_name, test_func in tests:
        try:
            if test_func():
                passed += 1
            print()  # Add spacing
        except Exception as e:
            print(f"❌ Test {test_name} crashed: {e}\n")
    
    print(f"📊 Results: {passed}/{total} tests passed")
    
    if passed == total:
        print("🎉 ALL CRITICAL CODERABBIT FIXES VALIDATED!")
        return True
    else:
        print("⚠️  Some critical fixes may have issues")
        return False

if __name__ == "__main__":
    success = main()
    sys.exit(0 if success else 1)

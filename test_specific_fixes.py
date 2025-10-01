#!/usr/bin/env python3
"""
Test specific CodeRabbit fixes in isolation.
"""

import os
import sys
import json
import tempfile
from pathlib import Path

# Add scripts to path  
sys.path.insert(0, "scripts")

def test_normalize_import_cascade():
    """Test normalize_project_name import cascade works."""
    print("🧪 Testing normalize_project_name import cascade...")
    
    try:
        # This tests the import fallback fix
        from import_conversations_unified import ConversationImporter
        
        # Test that normalization works 
        importer = ConversationImporter()
        test_path = Path("-Users-test-projects-myproject")
        collection_name = importer.get_collection_name(test_path)
        
        print(f"   Input: {test_path}")
        print(f"   Collection: {collection_name}")
        
        # Should contain normalized project name
        assert "myproject" in collection_name, f"Normalization failed: {collection_name}"
        
        print("✅ normalize_project_name import cascade working")
        return True
    except Exception as e:
        print(f"❌ normalize_project_name import cascade failed: {e}")
        return False

def test_message_counting_tool_entries():
    """Test that tool entries don't count as messages."""
    print("🧪 Testing tool entries don't count as messages...")
    
    try:
        from metadata_extractor import MetadataExtractor
        
        # Create test JSONL with mixed entries
        test_dir = Path("/tmp/claude")
        test_dir.mkdir(exist_ok=True)
        test_file = test_dir / "test_count.jsonl"
        
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
            
            # Tool use (should NOT count as message)
            f.write(json.dumps({
                "type": "tool_use",
                "name": "Read",
                "input": {"file_path": "test.py"},
                "timestamp": "2024-01-01T00:02:00Z"
            }) + "\n")
            
            # Tool result (should NOT count as message)
            f.write(json.dumps({
                "type": "tool_result",
                "content": [{"type": "text", "text": "File content"}],
                "timestamp": "2024-01-01T00:03:00Z"
            }) + "\n")
        
        # Extract metadata
        extractor = MetadataExtractor()
        metadata, created_at, message_count = extractor.extract_metadata_from_file(str(test_file))
        
        print(f"   Total entries: 4 (2 messages, 2 tools)")
        print(f"   Counted messages: {message_count}")
        print(f"   Tools tracked: {metadata.get('tools_used', [])}")
        
        # Should only count user/assistant messages
        assert message_count == 2, f"Expected 2 messages, got {message_count}"
        assert "Read" in metadata.get('tools_used', []), "Tool not tracked in metadata"
        
        # Cleanup
        test_file.unlink()
        
        print("✅ Tool entries correctly excluded from message count")
        return True
    except Exception as e:
        print(f"❌ Message counting test failed: {e}")
        return False

def test_qdrant_count_api_pattern():
    """Test Qdrant count API usage pattern (without actual connection)."""
    print("🧪 Testing Qdrant count() API pattern...")
    
    try:
        from import_strategies import StreamImportStrategy
        
        # Test that the cleanup method exists and uses count() pattern
        import inspect
        source = inspect.getsource(StreamImportStrategy._cleanup_old_points)
        
        print(f"   Checking cleanup method source...")
        
        # Should use count() not scroll()
        assert "count(" in source, "count() API not found in cleanup method"
        assert "scroll(" not in source, "deprecated scroll() API still present"
        assert "count_filter" in source, "count_filter parameter not found"
        
        print("✅ Qdrant count() API pattern correctly implemented")
        return True
    except Exception as e:
        print(f"❌ Qdrant count() API pattern test failed: {e}")
        return False

def test_unified_state_manager_integration():
    """Test UnifiedStateManager integration."""
    print("🧪 Testing UnifiedStateManager integration...")
    
    try:
        from unified_state_manager import UnifiedStateManager
        
        # Test basic functionality without file operations
        test_state_file = Path("/tmp/claude/test_state.json")
        test_state_file.parent.mkdir(exist_ok=True)
        
        manager = UnifiedStateManager(test_state_file)
        
        # Test normalized path generation
        test_path = "/some/test/path.jsonl"
        normalized = manager.normalize_path(test_path)
        
        print(f"   Path normalization: {test_path} -> {normalized}")
        assert normalized is not None, "Path normalization failed"
        
        # Test get_imported_files doesn't crash
        imported = manager.get_imported_files()
        assert isinstance(imported, dict), "get_imported_files should return dict"
        
        # Cleanup
        test_state_file.unlink(missing_ok=True)
        
        print("✅ UnifiedStateManager integration working")
        return True
    except Exception as e:
        print(f"❌ UnifiedStateManager integration failed: {e}")
        return False

def test_embedding_service_instantiation():
    """Test embedding service can be created."""
    print("🧪 Testing embedding service instantiation...")
    
    try:
        from embedding_service import create_embedding_service
        
        # Test local embedding service creation
        os.environ["PREFER_LOCAL_EMBEDDINGS"] = "true"
        service = create_embedding_service()
        
        print(f"   Provider: {service.get_provider_name()}")
        print(f"   Dimensions: {service.get_dimension()}")
        print(f"   Collection suffix: {service.get_collection_suffix()}")
        
        assert service.get_dimension() > 0, "Invalid embedding dimension"
        assert "local" in service.get_collection_suffix().lower(), "Expected local suffix"
        
        print("✅ Embedding service instantiation working")
        return True
    except Exception as e:
        print(f"❌ Embedding service instantiation failed: {e}")
        return False

def test_datetime_comparison_fix():
    """Test datetime comparison fix."""
    print("🧪 Testing datetime comparison fix...")
    
    try:
        from import_conversations_unified import ConversationImporter
        from datetime import datetime
        
        # Test file time comparison logic
        importer = ConversationImporter()
        
        # Create test file
        test_dir = Path("/tmp/claude")
        test_dir.mkdir(exist_ok=True)
        test_file = test_dir / "test_datetime.jsonl"
        test_file.write_text('{"test": true}\n')
        
        # Test should_import_file doesn't crash on datetime comparison
        should_import = importer.should_import_file(test_file)
        
        print(f"   File check result: {should_import}")
        assert isinstance(should_import, bool), "should_import_file should return boolean"
        
        # Cleanup
        test_file.unlink()
        
        print("✅ Datetime comparison fix working")
        return True
    except Exception as e:
        print(f"❌ Datetime comparison test failed: {e}")
        return False

def main():
    """Run all isolated tests."""
    print("🚀 Testing specific CodeRabbit fixes in isolation\n")
    
    tests = [
        ("normalize_project_name import cascade", test_normalize_import_cascade),
        ("Tool entries message counting", test_message_counting_tool_entries),
        ("Qdrant count() API pattern", test_qdrant_count_api_pattern),
        ("UnifiedStateManager integration", test_unified_state_manager_integration),
        ("Embedding service instantiation", test_embedding_service_instantiation),
        ("Datetime comparison fix", test_datetime_comparison_fix),
    ]
    
    passed = 0
    total = len(tests)
    
    for test_name, test_func in tests:
        try:
            if test_func():
                passed += 1
            print()  # spacing
        except Exception as e:
            print(f"❌ Test '{test_name}' crashed: {e}\n")
    
    print(f"📊 Isolated Tests Results: {passed}/{total} passed")
    
    if passed == total:
        print("🎉 ALL CODERABBIT FIXES VALIDATED IN ISOLATION!")
        print("\nValidated fixes:")
        print("✅ normalize_project_name import fallback cascade")
        print("✅ Tool entries excluded from message counting")
        print("✅ Qdrant count() API usage (not scroll())")
        print("✅ UnifiedStateManager integration")
        print("✅ Embedding service creation")
        print("✅ Datetime comparison fix")
        return True
    else:
        print(f"⚠️  {total - passed} fixes may have issues")
        return False

if __name__ == "__main__":
    success = main()
    sys.exit(0 if success else 1)

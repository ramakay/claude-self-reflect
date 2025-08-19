#!/usr/bin/env python3
"""
Multi-Project Support Tests
Tests project isolation, cross-project search, and collection management.
"""

import os
import sys
import json
import hashlib
import asyncio
import tempfile
from pathlib import Path
from typing import Dict, List, Any
from qdrant_client import QdrantClient
from qdrant_client.http.models import Distance, VectorParams, PointStruct

# Add parent directory to path
sys.path.insert(0, str(Path(__file__).parent.parent))

class TestMultiProject:
    """Test suite for multi-project functionality."""
    
    def __init__(self):
        self.test_results = []
        self.client = QdrantClient(host="localhost", port=6333)
        self.test_projects = [
            "project-alpha",
            "project-beta",
            "project-gamma",
            "test.project.with.dots",
            "project_with_underscores",
            "project-with-dashes",
            "CamelCaseProject",
            "project/with/slashes"  # Should be normalized
        ]
        self.created_collections = []
    
    def normalize_project_name(self, project: str) -> str:
        """Normalize project name for collection naming."""
        # Replace problematic characters
        normalized = project.lower()
        normalized = normalized.replace("/", "_")
        normalized = normalized.replace("\\", "_")
        normalized = normalized.replace(".", "_")
        normalized = normalized.replace("-", "_")
        normalized = normalized.replace(" ", "_")
        
        # Remove consecutive underscores
        while "__" in normalized:
            normalized = normalized.replace("__", "_")
        
        return normalized.strip("_")
    
    def get_collection_name(self, project: str, embedding_type: str = "local") -> str:
        """Get collection name for a project."""
        normalized = self.normalize_project_name(project)
        project_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
        return f"conv_{project_hash}_{embedding_type}"
    
    async def create_test_collection(self, project: str, embedding_type: str = "local"):
        """Create a test collection for a project."""
        collection_name = self.get_collection_name(project, embedding_type)
        vector_size = 384 if embedding_type == "local" else 1024
        
        try:
            # Check if collection exists
            collections = self.client.get_collections().collections
            if any(c.name == collection_name for c in collections):
                print(f"  Collection {collection_name} already exists")
                return collection_name
            
            # Create collection
            self.client.create_collection(
                collection_name=collection_name,
                vectors_config=VectorParams(size=vector_size, distance=Distance.COSINE)
            )
            self.created_collections.append(collection_name)
            
            # Add test data
            test_points = [
                PointStruct(
                    id=i,
                    vector=[0.1 * i] * vector_size,
                    payload={
                        "project": project,
                        "content": f"Test content for {project} - item {i}",
                        "timestamp": f"2025-01-{10+i:02d}T10:00:00Z"
                    }
                )
                for i in range(1, 4)
            ]
            
            self.client.upsert(
                collection_name=collection_name,
                points=test_points
            )
            
            return collection_name
            
        except Exception as e:
            print(f"  ⚠️ Error creating collection for {project}: {e}")
            return None
    
    async def test_project_normalization(self):
        """Test project name normalization."""
        test_name = "project_normalization"
        print(f"\n📝 Testing {test_name}...")
        
        test_cases = [
            ("simple-project", "simple_project"),
            ("Project.With.Dots", "project_with_dots"),
            ("path/to/project", "path_to_project"),
            ("UPPERCASE", "uppercase"),
            ("with  spaces", "with_spaces"),
            ("multiple___underscores", "multiple_underscores"),
            ("../../etc/passwd", "etc_passwd"),  # Security test
        ]
        
        for original, expected in test_cases:
            try:
                normalized = self.normalize_project_name(original)
                assert normalized == expected, \
                    f"Expected '{expected}', got '{normalized}'"
                
                self.test_results.append({
                    "test": f"{test_name}_{original}",
                    "status": "✅ PASS"
                })
                print(f"  ✅ '{original}' → '{normalized}'")
                
            except AssertionError as e:
                self.test_results.append({
                    "test": f"{test_name}_{original}",
                    "status": "❌ FAIL",
                    "error": str(e)
                })
    
    async def test_collection_isolation(self):
        """Test that projects are properly isolated."""
        test_name = "collection_isolation"
        print(f"\n📝 Testing {test_name}...")
        
        # Create collections for different projects
        project1 = "isolation-test-1"
        project2 = "isolation-test-2"
        
        collection1 = await self.create_test_collection(project1)
        collection2 = await self.create_test_collection(project2)
        
        if not collection1 or not collection2:
            self.test_results.append({
                "test": test_name,
                "status": "⚠️ SKIP",
                "error": "Could not create test collections"
            })
            return
        
        try:
            # Get points from each collection
            points1 = self.client.scroll(collection_name=collection1, limit=10)[0]
            points2 = self.client.scroll(collection_name=collection2, limit=10)[0]
            
            # Verify isolation
            for point in points1:
                assert point.payload["project"] == project1, \
                    f"Collection 1 should only contain project1 data"
            
            for point in points2:
                assert point.payload["project"] == project2, \
                    f"Collection 2 should only contain project2 data"
            
            # Verify collection names are different
            assert collection1 != collection2, \
                "Different projects should have different collections"
            
            self.test_results.append({
                "test": test_name,
                "status": "✅ PASS"
            })
            print(f"  ✅ Projects are properly isolated")
            
        except Exception as e:
            self.test_results.append({
                "test": test_name,
                "status": "❌ FAIL",
                "error": str(e)
            })
    
    async def test_cross_project_search(self):
        """Test searching across multiple projects."""
        test_name = "cross_project_search"
        print(f"\n📝 Testing {test_name}...")
        
        # Create multiple project collections
        created_projects = []
        for project in self.test_projects[:3]:  # Test with first 3 projects
            collection = await self.create_test_collection(project)
            if collection:
                created_projects.append((project, collection))
        
        if len(created_projects) < 2:
            self.test_results.append({
                "test": test_name,
                "status": "⚠️ SKIP",
                "error": "Not enough test collections created"
            })
            return
        
        try:
            # Simulate cross-project search
            all_results = []
            test_vector = [0.1] * 384  # Test query vector
            
            for project, collection in created_projects:
                try:
                    results = self.client.search(
                        collection_name=collection,
                        query_vector=test_vector,
                        limit=5
                    )
                    for r in results:
                        r.payload["_collection"] = collection
                        r.payload["_project"] = project
                    all_results.extend(results)
                except Exception as e:
                    print(f"  ⚠️ Could not search {collection}: {e}")
            
            # Verify we got results from multiple projects
            projects_found = set(r.payload["_project"] for r in all_results)
            assert len(projects_found) >= 2, \
                "Cross-project search should return results from multiple projects"
            
            self.test_results.append({
                "test": test_name,
                "status": "✅ PASS",
                "details": f"Found results from {len(projects_found)} projects"
            })
            print(f"  ✅ Cross-project search found {len(projects_found)} projects")
            
        except Exception as e:
            self.test_results.append({
                "test": test_name,
                "status": "❌ FAIL",
                "error": str(e)
            })
    
    async def test_collection_naming_consistency(self):
        """Test collection naming consistency."""
        test_name = "collection_naming"
        print(f"\n📝 Testing {test_name}...")
        
        test_cases = [
            # Same project should always produce same collection name
            ("consistent-project", "local", "consistent-project", "local", True),
            ("consistent-project", "voyage", "consistent-project", "voyage", True),
            # Different embedding types should produce different names
            ("same-project", "local", "same-project", "voyage", False),
            # Different projects should produce different names
            ("project-a", "local", "project-b", "local", False),
        ]
        
        for proj1, emb1, proj2, emb2, should_match in test_cases:
            try:
                name1 = self.get_collection_name(proj1, emb1)
                name2 = self.get_collection_name(proj2, emb2)
                
                if should_match:
                    assert name1 == name2, f"Names should match: {name1} != {name2}"
                else:
                    assert name1 != name2, f"Names should differ: {name1} == {name2}"
                
                self.test_results.append({
                    "test": f"{test_name}_{proj1}_{emb1}",
                    "status": "✅ PASS"
                })
                
            except AssertionError as e:
                self.test_results.append({
                    "test": f"{test_name}_{proj1}_{emb1}",
                    "status": "❌ FAIL",
                    "error": str(e)
                })
    
    async def test_project_discovery(self):
        """Test automatic project discovery from collections."""
        test_name = "project_discovery"
        print(f"\n📝 Testing {test_name}...")
        
        try:
            # Get all collections
            collections = self.client.get_collections().collections
            
            # Filter for conversation collections
            conv_collections = [
                c.name for c in collections 
                if c.name.startswith("conv_")
            ]
            
            # Extract project info
            projects = {}
            for coll_name in conv_collections:
                parts = coll_name.split("_")
                if len(parts) >= 3:
                    project_hash = parts[1]
                    embedding_type = parts[2] if len(parts) > 2 else "unknown"
                    projects[project_hash] = {
                        "collection": coll_name,
                        "embedding_type": embedding_type
                    }
            
            print(f"  Found {len(projects)} unique projects")
            print(f"  Total collections: {len(conv_collections)}")
            
            self.test_results.append({
                "test": test_name,
                "status": "✅ PASS",
                "details": f"Found {len(projects)} projects"
            })
            
        except Exception as e:
            self.test_results.append({
                "test": test_name,
                "status": "❌ FAIL",
                "error": str(e)
            })
    
    async def test_project_switching(self):
        """Test switching between projects during search."""
        test_name = "project_switching"
        print(f"\n📝 Testing {test_name}...")
        
        # Create test collections
        projects = ["switch-test-1", "switch-test-2", "switch-test-3"]
        collections = []
        
        for project in projects:
            coll = await self.create_test_collection(project)
            if coll:
                collections.append((project, coll))
        
        if len(collections) < 2:
            self.test_results.append({
                "test": test_name,
                "status": "⚠️ SKIP",
                "error": "Not enough collections created"
            })
            return
        
        try:
            test_vector = [0.2] * 384
            
            # Test switching between projects
            for project, collection in collections:
                results = self.client.search(
                    collection_name=collection,
                    query_vector=test_vector,
                    limit=1
                )
                
                assert len(results) > 0, f"Should get results from {project}"
                assert results[0].payload["project"] == project, \
                    f"Results should be from correct project"
            
            self.test_results.append({
                "test": test_name,
                "status": "✅ PASS"
            })
            print(f"  ✅ Successfully switched between {len(collections)} projects")
            
        except Exception as e:
            self.test_results.append({
                "test": test_name,
                "status": "❌ FAIL",
                "error": str(e)
            })
    
    async def test_project_metadata(self):
        """Test project metadata storage and retrieval."""
        test_name = "project_metadata"
        print(f"\n📝 Testing {test_name}...")
        
        project = "metadata-test"
        collection = await self.create_test_collection(project)
        
        if not collection:
            self.test_results.append({
                "test": test_name,
                "status": "⚠️ SKIP",
                "error": "Could not create test collection"
            })
            return
        
        try:
            # Add point with metadata
            metadata_point = PointStruct(
                id=100,
                vector=[0.5] * 384,
                payload={
                    "project": project,
                    "content": "Test with metadata",
                    "files_analyzed": ["main.py", "utils.py"],
                    "tools_used": ["Read", "Edit", "Bash"],
                    "concepts": ["testing", "metadata", "storage"],
                    "conversation_id": "test-conv-123",
                    "chunk_index": 0,
                    "total_chunks": 1
                }
            )
            
            self.client.upsert(
                collection_name=collection,
                points=[metadata_point]
            )
            
            # Retrieve and verify metadata
            retrieved = self.client.retrieve(
                collection_name=collection,
                ids=[100]
            )
            
            assert len(retrieved) == 1, "Should retrieve the point"
            payload = retrieved[0].payload
            
            assert payload["project"] == project
            assert "files_analyzed" in payload
            assert "tools_used" in payload
            assert "concepts" in payload
            assert len(payload["files_analyzed"]) == 2
            assert len(payload["tools_used"]) == 3
            
            self.test_results.append({
                "test": test_name,
                "status": "✅ PASS"
            })
            print(f"  ✅ Metadata stored and retrieved correctly")
            
        except Exception as e:
            self.test_results.append({
                "test": test_name,
                "status": "❌ FAIL",
                "error": str(e)
            })
    
    async def test_collection_limits(self):
        """Test handling of collection limits."""
        test_name = "collection_limits"
        print(f"\n📝 Testing {test_name}...")
        
        try:
            # Test very long project name
            long_project = "a" * 500
            normalized = self.normalize_project_name(long_project)
            collection_name = self.get_collection_name(long_project)
            
            # Collection name should be reasonable length
            assert len(collection_name) < 100, \
                f"Collection name too long: {len(collection_name)}"
            
            # Test special characters in project name
            special_project = "test!@#$%^&*()[]{}|\\<>?,./;':\""
            normalized_special = self.normalize_project_name(special_project)
            collection_special = self.get_collection_name(special_project)
            
            # Should handle special characters gracefully
            assert collection_special.startswith("conv_"), \
                "Collection name should have proper prefix"
            
            self.test_results.append({
                "test": test_name,
                "status": "✅ PASS"
            })
            print(f"  ✅ Collection limits handled properly")
            
        except Exception as e:
            self.test_results.append({
                "test": test_name,
                "status": "❌ FAIL",
                "error": str(e)
            })
    
    async def cleanup(self):
        """Clean up test collections."""
        print("\n🧹 Cleaning up test collections...")
        for collection in self.created_collections:
            try:
                self.client.delete_collection(collection_name=collection)
                print(f"  Deleted {collection}")
            except Exception as e:
                print(f"  Could not delete {collection}: {e}")
    
    def print_results(self):
        """Print test results summary."""
        print("\n" + "="*60)
        print("📊 MULTI-PROJECT TEST RESULTS")
        print("="*60)
        
        passed = sum(1 for r in self.test_results if "PASS" in r["status"])
        failed = sum(1 for r in self.test_results if "FAIL" in r["status"])
        skipped = sum(1 for r in self.test_results if "SKIP" in r["status"])
        
        print(f"\n✅ Passed: {passed}")
        print(f"❌ Failed: {failed}")
        print(f"⚠️  Skipped: {skipped}")
        print(f"📝 Total: {len(self.test_results)}")
        
        if failed > 0:
            print("\n❌ FAILED TESTS:")
            for result in self.test_results:
                if "FAIL" in result["status"]:
                    print(f"  - {result['test']}: {result.get('error', 'Unknown error')}")
        
        print("\n" + "="*60)
        return failed == 0
    
    async def run_all_tests(self):
        """Run all multi-project tests."""
        print("🚀 Starting Multi-Project Support Tests")
        print("="*60)
        
        await self.test_project_normalization()
        await self.test_collection_isolation()
        await self.test_cross_project_search()
        await self.test_collection_naming_consistency()
        await self.test_project_discovery()
        await self.test_project_switching()
        await self.test_project_metadata()
        await self.test_collection_limits()
        
        success = self.print_results()
        
        # Cleanup
        await self.cleanup()
        
        return success

async def main():
    """Main test runner."""
    tester = TestMultiProject()
    success = await tester.run_all_tests()
    sys.exit(0 if success else 1)

if __name__ == "__main__":
    asyncio.run(main())
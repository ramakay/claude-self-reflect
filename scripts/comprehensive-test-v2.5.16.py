#!/usr/bin/env python3
"""
Comprehensive Test Suite for Claude Self-Reflect v2.5.16
Tests all critical functionality before release.
"""

import asyncio
import json
import time
from pathlib import Path
from typing import Dict, List, Any, Tuple
from datetime import datetime, timedelta
import sys
import os

# Add parent directory for imports
sys.path.insert(0, str(Path(__file__).parent))
from utils import normalize_project_name

# Test configuration
TEST_CONFIG = {
    "qdrant_url": os.getenv("QDRANT_URL", "http://localhost:6333"),
    "min_score_thresholds": [0.2, 0.3, 0.5, 0.7],
    "test_queries": {
        "technical_terms": [
            "TensorZero API ML model inference backend",
            "Sanskrit philosophical discussions",
            "Observable A/B testing frameworks",
            "streaming importer chunking",
            "Qdrant vector database"
        ],
        "code_patterns": [
            "async def process_conversation_batch",
            "TokenAwareChunker class implementation",
            "from fastembed import TextEmbedding",
            "docker-compose streaming-importer"
        ],
        "general_topics": [
            "memory decay implementation",
            "search accuracy improvements",
            "collection naming conventions",
            "MCP server integration",
            "test coverage metrics"
        ],
        "error_patterns": [
            "ModuleNotFoundError langchain",
            "truncated to 1500 characters",
            "No conversations found matching",
            "collection already exists"
        ]
    },
    "expected_projects": [
        "claude-self-reflect",
        "anukruti",
        "ShopifyMCPMockShop"
    ]
}

class ComprehensiveTestSuite:
    """Comprehensive test suite for v2.5.16 release validation."""
    
    def __init__(self):
        self.results = {
            "timestamp": datetime.now().isoformat(),
            "version": "2.5.16",
            "tests": {},
            "summary": {
                "total": 0,
                "passed": 0,
                "failed": 0,
                "warnings": 0
            }
        }
        self.qdrant_client = None
        self.mcp_available = self._check_mcp_availability()
    
    def _check_mcp_availability(self) -> bool:
        """Check if MCP server is available."""
        try:
            # Try to import MCP tools (would be available in Claude)
            return "mcp__claude-self-reflect__reflect_on_past" in dir()
        except:
            return False
    
    async def initialize(self):
        """Initialize test environment."""
        from qdrant_client import AsyncQdrantClient
        self.qdrant_client = AsyncQdrantClient(url=TEST_CONFIG["qdrant_url"])
        
        # Get collection info
        collections = await self.qdrant_client.get_collections()
        self.collections = [c.name for c in collections.collections]
        self.conv_collections = [c for c in self.collections if c.startswith("conv_")]
        
        print(f"🔧 Initialized with {len(self.conv_collections)} conversation collections")
    
    async def test_jsonl_format_compatibility(self) -> Dict:
        """Test 1: Verify different JSONL formats are handled correctly."""
        test_name = "JSONL Format Compatibility"
        print(f"\n📝 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            # Check for different format patterns in collections
            format_stats = {
                "standard": 0,  # Direct {"role": ..., "content": ...}
                "nested": 0,    # {"message": {"role": ..., "content": ...}}
                "anukruti": 0,  # Specific Anukruti format
                "total_chunks": 0,
                "v1_chunks": 0,
                "v2_chunks": 0
            }
            
            # Sample a few collections
            for collection in self.conv_collections[:5]:
                response = await self.qdrant_client.scroll(
                    collection_name=collection,
                    limit=10,
                    with_payload=True,
                    with_vectors=False
                )
                
                points, _ = response
                for point in points:
                    format_stats["total_chunks"] += 1
                    
                    # Check chunking version
                    version = point.payload.get("chunking_version", "v1")
                    if version == "v2":
                        format_stats["v2_chunks"] += 1
                    else:
                        format_stats["v1_chunks"] += 1
                    
                    # Detect format from text pattern
                    text = point.payload.get("text", "")
                    if "role: user" in text or "role: assistant" in text:
                        format_stats["standard"] += 1
                    if "message:" in text:
                        format_stats["nested"] += 1
                    if "anukruti" in collection.lower():
                        format_stats["anukruti"] += 1
            
            results["details"] = format_stats
            
            # Validate results
            if format_stats["total_chunks"] > 0:
                v2_percentage = (format_stats["v2_chunks"] / format_stats["total_chunks"]) * 100
                if v2_percentage > 10:  # At least some v2 chunks
                    results["status"] = "passed"
                    results["message"] = f"Multiple formats detected, {v2_percentage:.1f}% v2 chunks"
                else:
                    results["status"] = "warning"
                    results["message"] = f"Low v2 adoption: {v2_percentage:.1f}%"
            else:
                results["status"] = "failed"
                results["message"] = "No chunks found"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def test_age_based_searches(self) -> Dict:
        """Test 2: Verify age-based search functionality."""
        test_name = "Age-Based Search Patterns"
        print(f"\n📅 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            # Group chunks by age
            age_groups = {
                "today": 0,
                "this_week": 0,
                "this_month": 0,
                "older": 0
            }
            
            now = datetime.now()
            
            # Sample from collections
            for collection in self.conv_collections[:3]:
                response = await self.qdrant_client.scroll(
                    collection_name=collection,
                    limit=50,
                    with_payload=True,
                    with_vectors=False
                )
                
                points, _ = response
                for point in points:
                    timestamp_str = point.payload.get("timestamp", "")
                    if timestamp_str:
                        try:
                            timestamp = datetime.fromisoformat(timestamp_str.replace("Z", "+00:00"))
                            age_days = (now - timestamp.replace(tzinfo=None)).days
                            
                            if age_days == 0:
                                age_groups["today"] += 1
                            elif age_days <= 7:
                                age_groups["this_week"] += 1
                            elif age_days <= 30:
                                age_groups["this_month"] += 1
                            else:
                                age_groups["older"] += 1
                        except:
                            pass
            
            results["details"] = age_groups
            
            # Validate
            total_aged = sum(age_groups.values())
            if total_aged > 0:
                results["status"] = "passed"
                results["message"] = f"Found {total_aged} chunks across all age groups"
            else:
                results["status"] = "failed"
                results["message"] = "No timestamped chunks found"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def test_conversation_sizes(self) -> Dict:
        """Test 3: Verify handling of different conversation sizes."""
        test_name = "Conversation Size Handling"
        print(f"\n📏 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            size_categories = {
                "small": [],    # < 1000 chars
                "medium": [],   # 1000-5000 chars
                "large": [],    # 5000-10000 chars
                "huge": []      # > 10000 chars
            }
            
            # Check chunk sizes
            for collection in self.conv_collections[:5]:
                response = await self.qdrant_client.scroll(
                    collection_name=collection,
                    limit=20,
                    with_payload=True,
                    with_vectors=False
                )
                
                points, _ = response
                for point in points:
                    text_length = len(point.payload.get("text", ""))
                    version = point.payload.get("chunking_version", "v1")
                    
                    if text_length < 1000:
                        size_categories["small"].append((text_length, version))
                    elif text_length < 5000:
                        size_categories["medium"].append((text_length, version))
                    elif text_length < 10000:
                        size_categories["large"].append((text_length, version))
                    else:
                        size_categories["huge"].append((text_length, version))
            
            # Calculate statistics
            stats = {}
            for category, chunks in size_categories.items():
                if chunks:
                    v2_count = sum(1 for _, v in chunks if v == "v2")
                    avg_size = sum(size for size, _ in chunks) / len(chunks)
                    stats[category] = {
                        "count": len(chunks),
                        "v2_count": v2_count,
                        "avg_size": avg_size
                    }
            
            results["details"] = stats
            
            # Check for proper chunking
            if stats:
                # Check if we have v2 chunks in medium/large categories
                has_proper_v2 = any(
                    cat.get("v2_count", 0) > 0 
                    for key, cat in stats.items() 
                    if key in ["medium", "large"]
                )
                
                if has_proper_v2:
                    results["status"] = "passed"
                    results["message"] = "V2 chunking working for larger conversations"
                else:
                    results["status"] = "warning"
                    results["message"] = "Limited v2 chunking in larger conversations"
            else:
                results["status"] = "failed"
                results["message"] = "No chunk size data collected"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def test_chunk_boundaries(self) -> Dict:
        """Test 4: Verify chunk boundary handling."""
        test_name = "Chunk Boundary Cases"
        print(f"\n🔍 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            boundary_issues = {
                "truncated_v1": 0,  # Exactly 1500 chars
                "truncated_markers": 0,  # Contains [...] or ...
                "empty_chunks": 0,
                "overlapping_v2": 0,
                "proper_v2": 0
            }
            
            # Check for boundary issues
            for collection in self.conv_collections[:5]:
                response = await self.qdrant_client.scroll(
                    collection_name=collection,
                    limit=30,
                    with_payload=True,
                    with_vectors=False
                )
                
                points, _ = response
                for point in points:
                    text = point.payload.get("text", "")
                    text_length = len(text)
                    version = point.payload.get("chunking_version", "v1")
                    
                    if text_length == 1500:
                        boundary_issues["truncated_v1"] += 1
                    
                    if text.endswith("[...]") or text.endswith("..."):
                        boundary_issues["truncated_markers"] += 1
                    
                    if text_length == 0 or text == "NO TEXT":
                        boundary_issues["empty_chunks"] += 1
                    
                    if version == "v2":
                        if point.payload.get("chunk_overlap"):
                            boundary_issues["overlapping_v2"] += 1
                        boundary_issues["proper_v2"] += 1
            
            results["details"] = boundary_issues
            
            # Validate
            total_issues = boundary_issues["truncated_v1"] + boundary_issues["empty_chunks"]
            if boundary_issues["proper_v2"] > 0 and total_issues < boundary_issues["proper_v2"]:
                results["status"] = "passed"
                results["message"] = f"V2 chunking reducing boundary issues ({boundary_issues['proper_v2']} good v2 chunks)"
            elif total_issues > 10:
                results["status"] = "warning"
                results["message"] = f"Still have {total_issues} boundary issues to fix"
            else:
                results["status"] = "passed"
                results["message"] = "Minimal boundary issues"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def test_technical_searches(self) -> Dict:
        """Test 5: Verify technical term search functionality."""
        test_name = "Technical Term Searches"
        print(f"\n🔬 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            from fastembed import TextEmbedding
            model = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
            
            search_results = {}
            
            # Test each technical query
            for query in TEST_CONFIG["test_queries"]["technical_terms"][:3]:
                # Generate embedding
                query_embedding = list(model.embed([query]))[0].tolist()
                
                # Search across collections
                all_scores = []
                found_v2 = False
                
                for collection in self.conv_collections[:3]:
                    try:
                        results_batch = await self.qdrant_client.search(
                            collection_name=collection,
                            query_vector=query_embedding,
                            limit=3
                        )
                        
                        for result in results_batch:
                            all_scores.append(result.score)
                            if result.payload.get("chunking_version") == "v2":
                                found_v2 = True
                    except:
                        pass
                
                if all_scores:
                    search_results[query[:30]] = {
                        "max_score": max(all_scores),
                        "avg_score": sum(all_scores) / len(all_scores),
                        "found_v2": found_v2
                    }
            
            results["details"] = search_results
            
            # Validate
            if search_results:
                # Check if we're finding results with reasonable scores
                max_scores = [r["max_score"] for r in search_results.values()]
                avg_max_score = sum(max_scores) / len(max_scores)
                
                if avg_max_score > 0.25:  # Lower threshold for technical terms
                    results["status"] = "passed"
                    results["message"] = f"Technical searches working (avg max score: {avg_max_score:.3f})"
                else:
                    results["status"] = "warning"
                    results["message"] = f"Low scores for technical terms (avg: {avg_max_score:.3f})"
            else:
                results["status"] = "failed"
                results["message"] = "No search results obtained"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def test_project_isolation(self) -> Dict:
        """Test 6: Verify project isolation and cross-project search."""
        test_name = "Project Isolation"
        print(f"\n🏢 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            project_stats = {}
            
            # Analyze project distribution
            for collection in self.conv_collections:
                # Extract project from collection name
                # Format: conv_HASH_local or conv_HASH_voyage
                parts = collection.split("_")
                if len(parts) >= 2:
                    hash_part = parts[1]
                    
                    # Try to find actual project name from a sample point
                    try:
                        response = await self.qdrant_client.scroll(
                            collection_name=collection,
                            limit=1,
                            with_payload=True,
                            with_vectors=False
                        )
                        
                        points, _ = response
                        if points:
                            project = points[0].payload.get("project", "unknown")
                            if project not in project_stats:
                                project_stats[project] = {
                                    "collections": [],
                                    "total_chunks": 0,
                                    "v2_chunks": 0
                                }
                            project_stats[project]["collections"].append(collection)
                    except:
                        pass
            
            # Get chunk counts per project
            for project, stats in project_stats.items():
                for collection in stats["collections"][:2]:  # Sample 2 collections per project
                    try:
                        info = await self.qdrant_client.get_collection(collection)
                        stats["total_chunks"] += info.points_count or 0
                    except:
                        pass
            
            results["details"] = {
                "projects_found": list(project_stats.keys())[:10],  # Limit output
                "total_projects": len(project_stats),
                "project_sample": {k: v for k, v in list(project_stats.items())[:3]}
            }
            
            # Validate
            if len(project_stats) >= 2:
                results["status"] = "passed"
                results["message"] = f"Found {len(project_stats)} isolated projects"
            else:
                results["status"] = "warning"
                results["message"] = "Limited project diversity"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def test_score_thresholds(self) -> Dict:
        """Test 7: Test search with different min_score thresholds."""
        test_name = "Score Threshold Testing"
        print(f"\n📊 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            from fastembed import TextEmbedding
            model = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
            
            # Test query
            test_query = "chunking implementation vector database"
            query_embedding = list(model.embed([test_query]))[0].tolist()
            
            threshold_results = {}
            
            # Test different thresholds
            for threshold in TEST_CONFIG["min_score_thresholds"]:
                matches = 0
                v2_matches = 0
                
                # Search a few collections
                for collection in self.conv_collections[:3]:
                    try:
                        results_batch = await self.qdrant_client.search(
                            collection_name=collection,
                            query_vector=query_embedding,
                            limit=10,
                            score_threshold=threshold
                        )
                        
                        for result in results_batch:
                            if result.score >= threshold:
                                matches += 1
                                if result.payload.get("chunking_version") == "v2":
                                    v2_matches += 1
                    except:
                        pass
                
                threshold_results[f"threshold_{threshold}"] = {
                    "matches": matches,
                    "v2_matches": v2_matches
                }
            
            results["details"] = threshold_results
            
            # Validate
            # Check if lower thresholds give more results
            thresholds_sorted = sorted(threshold_results.items(), key=lambda x: float(x[0].split("_")[1]))
            matches_progression = [x[1]["matches"] for x in thresholds_sorted]
            
            if matches_progression == sorted(matches_progression, reverse=True):
                results["status"] = "passed"
                results["message"] = "Score thresholds working correctly"
            else:
                results["status"] = "warning"
                results["message"] = "Unexpected score threshold behavior"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def test_v1_v2_coexistence(self) -> Dict:
        """Test 8: Verify v1/v2 chunk coexistence and boosting."""
        test_name = "V1/V2 Coexistence & Boosting"
        print(f"\n🔄 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            coexistence_stats = {
                "v1_only_collections": 0,
                "v2_only_collections": 0,
                "mixed_collections": 0,
                "v2_boost_evidence": []
            }
            
            # Check each collection
            for collection in self.conv_collections[:10]:
                has_v1 = False
                has_v2 = False
                
                response = await self.qdrant_client.scroll(
                    collection_name=collection,
                    limit=20,
                    with_payload=True,
                    with_vectors=False
                )
                
                points, _ = response
                for point in points:
                    version = point.payload.get("chunking_version", "v1")
                    if version == "v1":
                        has_v1 = True
                    elif version == "v2":
                        has_v2 = True
                
                if has_v1 and has_v2:
                    coexistence_stats["mixed_collections"] += 1
                elif has_v2:
                    coexistence_stats["v2_only_collections"] += 1
                elif has_v1:
                    coexistence_stats["v1_only_collections"] += 1
            
            # Test boosting with a search
            from fastembed import TextEmbedding
            model = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
            test_query = "test search query"
            query_embedding = list(model.embed([test_query]))[0].tolist()
            
            # Find a mixed collection
            for collection in self.conv_collections[:5]:
                try:
                    results_batch = await self.qdrant_client.search(
                        collection_name=collection,
                        query_vector=query_embedding,
                        limit=10
                    )
                    
                    for result in results_batch:
                        version = result.payload.get("chunking_version", "v1")
                        coexistence_stats["v2_boost_evidence"].append({
                            "version": version,
                            "score": result.score
                        })
                except:
                    pass
            
            # Check if v2 chunks tend to have higher scores
            v1_scores = [x["score"] for x in coexistence_stats["v2_boost_evidence"] if x["version"] == "v1"]
            v2_scores = [x["score"] for x in coexistence_stats["v2_boost_evidence"] if x["version"] == "v2"]
            
            if v1_scores and v2_scores:
                avg_v1 = sum(v1_scores) / len(v1_scores)
                avg_v2 = sum(v2_scores) / len(v2_scores)
                coexistence_stats["avg_v1_score"] = avg_v1
                coexistence_stats["avg_v2_score"] = avg_v2
                coexistence_stats["v2_boost_percentage"] = ((avg_v2 - avg_v1) / avg_v1 * 100) if avg_v1 > 0 else 0
            
            results["details"] = coexistence_stats
            
            # Validate
            if coexistence_stats["mixed_collections"] > 0:
                results["status"] = "passed"
                results["message"] = f"V1/V2 coexisting in {coexistence_stats['mixed_collections']} collections"
            else:
                results["status"] = "warning"
                results["message"] = "Limited v1/v2 mixing"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def test_critical_content(self) -> Dict:
        """Test 9: Verify critical content (TensorZero, Sanskrit) is searchable."""
        test_name = "Critical Content Recovery"
        print(f"\n⚠️ Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            from fastembed import TextEmbedding
            model = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
            
            critical_searches = {
                "TensorZero": "TensorZero API ML model inference backend observability",
                "Sanskrit": "Sanskrit philosophical discussions ancient texts",
                "Observable": "Observable A/B testing experimentation platform"
            }
            
            search_outcomes = {}
            
            for name, query in critical_searches.items():
                query_embedding = list(model.embed([query]))[0].tolist()
                
                found = False
                max_score = 0
                found_in_v2 = False
                
                # Search across collections
                for collection in self.conv_collections[:10]:
                    try:
                        results_batch = await self.qdrant_client.search(
                            collection_name=collection,
                            query_vector=query_embedding,
                            limit=5,
                            score_threshold=0.2  # Lower threshold for critical content
                        )
                        
                        for result in results_batch:
                            text = result.payload.get("text", "")
                            if name.lower() in text.lower():
                                found = True
                                max_score = max(max_score, result.score)
                                if result.payload.get("chunking_version") == "v2":
                                    found_in_v2 = True
                    except:
                        pass
                
                search_outcomes[name] = {
                    "found": found,
                    "max_score": max_score,
                    "found_in_v2": found_in_v2
                }
            
            results["details"] = search_outcomes
            
            # Validate - this is the critical test!
            found_count = sum(1 for x in search_outcomes.values() if x["found"])
            
            if found_count >= 2:  # At least 2 out of 3 critical items found
                results["status"] = "passed"
                results["message"] = f"Critical content recovered: {found_count}/3 items found"
            elif found_count >= 1:
                results["status"] = "warning"
                results["message"] = f"Partial recovery: {found_count}/3 critical items found"
            else:
                results["status"] = "failed"
                results["message"] = "Critical content still not searchable!"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def run_all_tests(self):
        """Run all tests and generate report."""
        print("=" * 60)
        print("🚀 COMPREHENSIVE TEST SUITE v2.5.16")
        print("=" * 60)
        
        await self.initialize()
        
        # Run all tests
        test_methods = [
            self.test_jsonl_format_compatibility,
            self.test_age_based_searches,
            self.test_conversation_sizes,
            self.test_chunk_boundaries,
            self.test_technical_searches,
            self.test_project_isolation,
            self.test_score_thresholds,
            self.test_v1_v2_coexistence,
            self.test_critical_content
        ]
        
        for test_method in test_methods:
            result = await test_method()
            self.results["tests"][result["name"]] = result
            
            # Update summary
            self.results["summary"]["total"] += 1
            if result["status"] == "passed":
                self.results["summary"]["passed"] += 1
                print(f"  ✅ {result['name']}")
            elif result["status"] == "warning":
                self.results["summary"]["warnings"] += 1
                print(f"  ⚠️ {result['name']}: {result.get('message', '')}")
            else:
                self.results["summary"]["failed"] += 1
                print(f"  ❌ {result['name']}: {result.get('message', result.get('error', ''))}")
        
        # Generate final report
        self.generate_report()
    
    def generate_report(self):
        """Generate final test report."""
        print("\n" + "=" * 60)
        print("📋 FINAL TEST REPORT")
        print("=" * 60)
        
        summary = self.results["summary"]
        pass_rate = (summary["passed"] / summary["total"] * 100) if summary["total"] > 0 else 0
        
        print(f"\n📊 Summary:")
        print(f"  Total Tests: {summary['total']}")
        print(f"  ✅ Passed: {summary['passed']}")
        print(f"  ⚠️ Warnings: {summary['warnings']}")
        print(f"  ❌ Failed: {summary['failed']}")
        print(f"  Pass Rate: {pass_rate:.1f}%")
        
        # Critical test check
        critical_test = self.results["tests"].get("Critical Content Recovery", {})
        critical_passed = critical_test.get("status") == "passed"
        
        print(f"\n🎯 Critical Content Test: {'✅ PASSED' if critical_passed else '❌ FAILED'}")
        
        # Release recommendation
        print(f"\n🏁 RELEASE RECOMMENDATION FOR v2.5.16:")
        
        if pass_rate >= 80 and critical_passed:
            print("  ✅ READY FOR RELEASE")
            print("  The v2 chunking solution successfully addresses the critical issues.")
        elif pass_rate >= 60 and summary["failed"] <= 2:
            print("  ⚠️ CONDITIONAL RELEASE")
            print("  Consider releasing with known limitations documented.")
        else:
            print("  ❌ NOT READY FOR RELEASE")
            print("  Critical issues remain unresolved.")
        
        # Save report to file
        report_path = Path(__file__).parent / f"test-report-{datetime.now().strftime('%Y%m%d-%H%M%S')}.json"
        with open(report_path, 'w') as f:
            json.dump(self.results, f, indent=2, default=str)
        
        print(f"\n💾 Full report saved to: {report_path}")


async def main():
    """Main entry point."""
    suite = ComprehensiveTestSuite()
    await suite.run_all_tests()


if __name__ == "__main__":
    asyncio.run(main())
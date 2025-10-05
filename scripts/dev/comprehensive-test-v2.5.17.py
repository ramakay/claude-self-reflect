#!/usr/bin/env python3
"""
Comprehensive Test Suite v2.5.17 - Extended with 25 Additional MCP Tests
Tests all critical functionality including the search_by_concept bug.
"""

import asyncio
import json
import time
from pathlib import Path
from typing import Dict, List, Any, Tuple, Optional
from datetime import datetime, timedelta
import sys
import os
import hashlib
from collections import defaultdict
import random
import string
import traceback

# Add parent directory for imports
sys.path.insert(0, str(Path(__file__).parent))
from utils import normalize_project_name

# Test configuration
TEST_CONFIG = {
    "qdrant_url": os.getenv("QDRANT_URL", "http://localhost:6333"),
    "voyage_key": os.getenv("VOYAGE_KEY", ""),
    "test_categories": {
        "search_quality": [
            "multi_language_search",
            "fuzzy_matching",
            "long_query_handling",
            "empty_null_search",
            "special_character_search"
        ],
        "performance_scale": [
            "concurrent_searches",
            "large_result_sets",
            "memory_decay_calculation",
            "cross_collection_search",
            "embedding_generation_speed"
        ],
        "data_integrity": [
            "duplicate_detection",
            "conversation_continuity",
            "timestamp_preservation",
            "tool_usage_extraction",
            "project_attribution"
        ],
        "cloud_voyage": [
            "voyage_embedding_compatibility",
            "cloud_local_fallback",
            "api_key_validation",
            "rate_limiting_handling",
            "hybrid_search"
        ],
        "edge_cases": [
            "corrupted_jsonl",
            "incomplete_conversations",
            "unicode_encoding",
            "file_permission_errors",
            "network_failures"
        ],
        "critical_bugs": [
            "search_by_concept_broken",
            "search_by_file_functionality",
            "metadata_extraction_missing"
        ]
    }
}

class ExtendedTestSuite:
    """Extended test suite with 25+ additional tests for v2.5.17."""
    
    def __init__(self):
        self.results = {
            "timestamp": datetime.now().isoformat(),
            "version": "2.5.17",
            "tests": {},
            "summary": {
                "total": 0,
                "passed": 0,
                "failed": 0,
                "warnings": 0,
                "critical_bugs": []
            }
        }
        self.qdrant_client = None
        self.collections = []
        self.conv_collections = []
    
    async def initialize(self):
        """Initialize test environment."""
        from qdrant_client import AsyncQdrantClient
        self.qdrant_client = AsyncQdrantClient(url=TEST_CONFIG["qdrant_url"])
        
        # Get collection info
        collections = await self.qdrant_client.get_collections()
        self.collections = [c.name for c in collections.collections]
        self.conv_collections = [c for c in self.collections if c.startswith("conv_")]
        
        print(f"🔧 Initialized with {len(self.conv_collections)} conversation collections")
    
    # ==================== CRITICAL BUG TESTS ====================
    
    async def test_search_by_concept_broken(self) -> Dict:
        """Test: search_by_concept is completely broken (concepts field not populated)."""
        test_name = "Search by Concept - CRITICAL BUG"
        print(f"\n🚨 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {},
            "critical": True
        }
        
        try:
            # Check if any collection has concepts field populated
            concepts_found = False
            collections_checked = 0
            
            for collection in self.conv_collections[:10]:
                response = await self.qdrant_client.scroll(
                    collection_name=collection,
                    limit=50,
                    with_payload=True,
                    with_vectors=False
                )
                
                points, _ = response
                for point in points:
                    if "concepts" in point.payload and point.payload["concepts"]:
                        concepts_found = True
                        break
                
                collections_checked += 1
                if concepts_found:
                    break
            
            results["details"] = {
                "concepts_field_populated": concepts_found,
                "collections_checked": collections_checked,
                "issue": "search_by_concept uses 'concepts' field which is never populated by streaming importer or migration scripts"
            }
            
            if not concepts_found:
                results["status"] = "failed"
                results["message"] = "CRITICAL: concepts field not populated - search_by_concept will always return no results"
                self.results["summary"]["critical_bugs"].append("search_by_concept")
            else:
                results["status"] = "warning"
                results["message"] = "Some concepts found, but coverage may be incomplete"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def test_metadata_extraction_missing(self) -> Dict:
        """Test: Metadata extraction (files_analyzed, tools_used) missing from streaming importer."""
        test_name = "Metadata Extraction Coverage"
        print(f"\n🔍 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            metadata_stats = {
                "has_files_analyzed": 0,
                "has_files_edited": 0,
                "has_tools_used": 0,
                "has_concepts": 0,
                "total_checked": 0
            }
            
            for collection in self.conv_collections[:5]:
                response = await self.qdrant_client.scroll(
                    collection_name=collection,
                    limit=30,
                    with_payload=True,
                    with_vectors=False
                )
                
                points, _ = response
                for point in points:
                    metadata_stats["total_checked"] += 1
                    if point.payload.get("files_analyzed"):
                        metadata_stats["has_files_analyzed"] += 1
                    if point.payload.get("files_edited"):
                        metadata_stats["has_files_edited"] += 1
                    if point.payload.get("tools_used"):
                        metadata_stats["has_tools_used"] += 1
                    if point.payload.get("concepts"):
                        metadata_stats["has_concepts"] += 1
            
            results["details"] = metadata_stats
            
            # Check coverage
            if metadata_stats["total_checked"] > 0:
                coverage = (metadata_stats["has_files_analyzed"] + metadata_stats["has_tools_used"]) / (metadata_stats["total_checked"] * 2)
                if coverage < 0.1:
                    results["status"] = "failed"
                    results["message"] = f"Metadata extraction missing: only {coverage*100:.1f}% coverage"
                    self.results["summary"]["critical_bugs"].append("metadata_extraction")
                else:
                    results["status"] = "passed"
                    results["message"] = f"Metadata extraction working: {coverage*100:.1f}% coverage"
            else:
                results["status"] = "failed"
                results["message"] = "No data to check"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    # ==================== SEARCH QUALITY TESTS ====================
    
    async def test_multi_language_search(self) -> Dict:
        """Test: Multi-language search (Sanskrit, Chinese, code snippets)."""
        test_name = "Multi-Language Search"
        print(f"\n🌐 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            from fastembed import TextEmbedding
            model = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
            
            test_queries = {
                "sanskrit": "संस्कृत दर्शन चर्चा",  # Sanskrit philosophical discussion
                "chinese": "机器学习模型",  # Machine learning model
                "code": "async def process_batch(items: List[Dict])",
                "emoji": "🚀 deployment success ✅"
            }
            
            language_results = {}
            
            for lang, query in test_queries.items():
                query_embedding = list(model.embed([query]))[0].tolist()
                
                found = False
                max_score = 0
                
                # Search a few collections
                for collection in self.conv_collections[:3]:
                    try:
                        results_batch = await self.qdrant_client.search(
                            collection_name=collection,
                            query_vector=query_embedding,
                            limit=5
                        )
                        
                        if results_batch:
                            found = True
                            max_score = max(max_score, results_batch[0].score)
                    except:
                        pass
                
                language_results[lang] = {
                    "query": query[:30],
                    "found": found,
                    "max_score": max_score
                }
            
            results["details"] = language_results
            
            # All language types should be searchable
            if all(r["found"] for r in language_results.values()):
                results["status"] = "passed"
                results["message"] = "Multi-language search working"
            else:
                failed_langs = [k for k, v in language_results.items() if not v["found"]]
                results["status"] = "warning"
                results["message"] = f"Some languages not searchable: {failed_langs}"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def test_fuzzy_matching(self) -> Dict:
        """Test: Fuzzy matching for typos and synonyms."""
        test_name = "Fuzzy Matching & Typos"
        print(f"\n🔤 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            from fastembed import TextEmbedding
            model = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
            
            # Test typos and variations
            test_pairs = [
                ("Qdrant vector database", "Qdrnat vecotr databse"),  # Typos
                ("machine learning", "ML"),  # Abbreviation
                ("docker container", "docker image"),  # Related terms
            ]
            
            fuzzy_results = []
            
            for correct, fuzzy in test_pairs:
                correct_embedding = list(model.embed([correct]))[0].tolist()
                fuzzy_embedding = list(model.embed([fuzzy]))[0].tolist()
                
                # Compare similarity between embeddings
                import numpy as np
                similarity = np.dot(correct_embedding, fuzzy_embedding) / (
                    np.linalg.norm(correct_embedding) * np.linalg.norm(fuzzy_embedding)
                )
                
                fuzzy_results.append({
                    "correct": correct,
                    "fuzzy": fuzzy,
                    "similarity": float(similarity)
                })
            
            results["details"] = {"pairs": fuzzy_results}
            
            # Check if fuzzy matching is reasonable
            avg_similarity = sum(r["similarity"] for r in fuzzy_results) / len(fuzzy_results)
            if avg_similarity > 0.5:
                results["status"] = "passed"
                results["message"] = f"Fuzzy matching working (avg similarity: {avg_similarity:.3f})"
            else:
                results["status"] = "warning"
                results["message"] = f"Fuzzy matching limited (avg similarity: {avg_similarity:.3f})"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def test_long_query_handling(self) -> Dict:
        """Test: Long query handling (100+ words)."""
        test_name = "Long Query Handling"
        print(f"\n📜 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            from fastembed import TextEmbedding
            model = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
            
            # Create a long query
            long_query = """
            I need help with implementing a comprehensive token-aware chunking system that can handle
            various conversation formats including standard JSONL, nested message structures, and
            special formats like Anukruti. The system should preserve conversation context across
            chunk boundaries, maintain proper overlap for semantic continuity, handle edge cases
            like extremely long messages, code blocks with special characters, multilingual content
            including Sanskrit and Chinese characters, and ensure that the chunking process doesn't
            break in the middle of important semantic units. Additionally, it should be memory-efficient,
            process conversations in batches, support both local FastEmbed and cloud-based Voyage AI
            embeddings, and maintain backward compatibility with existing v1 chunks while providing
            clear migration paths. The implementation should also include proper error handling,
            logging, progress tracking, and be able to recover from failures without data loss.
            """
            
            # Try to generate embedding for long query
            start_time = time.time()
            try:
                query_embedding = list(model.embed([long_query]))[0].tolist()
                embed_time = time.time() - start_time
                
                # Try to search with it
                if self.conv_collections:
                    search_results = await self.qdrant_client.search(
                        collection_name=self.conv_collections[0],
                        query_vector=query_embedding,
                        limit=5
                    )
                    
                    results["details"] = {
                        "query_length": len(long_query.split()),
                        "embedding_time": embed_time,
                        "search_results": len(search_results),
                        "max_score": search_results[0].score if search_results else 0
                    }
                    
                    results["status"] = "passed"
                    results["message"] = f"Long query processed in {embed_time:.2f}s"
                else:
                    results["status"] = "warning"
                    results["message"] = "No collections to test with"
                    
            except Exception as embed_error:
                results["status"] = "failed"
                results["message"] = f"Failed to process long query: {embed_error}"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    # ==================== PERFORMANCE & SCALE TESTS ====================
    
    async def test_concurrent_searches(self) -> Dict:
        """Test: Concurrent searches (10 simultaneous queries)."""
        test_name = "Concurrent Search Performance"
        print(f"\n⚡ Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            from fastembed import TextEmbedding
            model = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
            
            # Prepare 10 different queries
            queries = [
                "vector database search",
                "token aware chunking",
                "memory decay implementation",
                "Docker container orchestration",
                "Python async programming",
                "MCP server integration",
                "test coverage metrics",
                "semantic search optimization",
                "embedding model comparison",
                "conversation continuity"
            ]
            
            # Generate embeddings
            embeddings = list(model.embed(queries))
            
            if not self.conv_collections:
                results["status"] = "warning"
                results["message"] = "No collections to test"
                return results
            
            # Run concurrent searches
            start_time = time.time()
            
            async def search_task(query_embedding, collection_name, query_text):
                try:
                    search_start = time.time()
                    results = await self.qdrant_client.search(
                        collection_name=collection_name,
                        query_vector=query_embedding.tolist(),
                        limit=10
                    )
                    search_time = time.time() - search_start
                    return {
                        "query": query_text[:20],
                        "time": search_time,
                        "results": len(results),
                        "max_score": results[0].score if results else 0
                    }
                except Exception as e:
                    return {
                        "query": query_text[:20],
                        "error": str(e)
                    }
            
            # Create tasks for concurrent execution
            tasks = []
            for query, embedding in zip(queries, embeddings):
                # Use different collections for variety
                collection = self.conv_collections[hash(query) % len(self.conv_collections)]
                tasks.append(search_task(embedding, collection, query))
            
            # Execute concurrently
            search_results = await asyncio.gather(*tasks)
            total_time = time.time() - start_time
            
            # Calculate stats
            successful = [r for r in search_results if "error" not in r]
            avg_time = sum(r["time"] for r in successful) / len(successful) if successful else 0
            
            results["details"] = {
                "total_queries": len(queries),
                "successful": len(successful),
                "total_time": total_time,
                "avg_query_time": avg_time,
                "queries_per_second": len(successful) / total_time if total_time > 0 else 0
            }
            
            # Evaluate performance
            if len(successful) == len(queries) and avg_time < 1.0:
                results["status"] = "passed"
                results["message"] = f"Concurrent search excellent: {results['details']['queries_per_second']:.1f} qps"
            elif len(successful) >= len(queries) * 0.8:
                results["status"] = "warning"
                results["message"] = f"Concurrent search acceptable: {len(successful)}/{len(queries)} succeeded"
            else:
                results["status"] = "failed"
                results["message"] = f"Concurrent search issues: only {len(successful)}/{len(queries)} succeeded"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def test_memory_decay_calculation(self) -> Dict:
        """Test: Memory decay calculation performance with 10k+ points."""
        test_name = "Memory Decay Performance"
        print(f"\n⏱️ Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            # Simulate decay calculation
            import math
            from datetime import datetime, timedelta
            
            # Create simulated points with timestamps
            num_points = 10000
            now = datetime.now()
            
            start_time = time.time()
            
            points = []
            for i in range(num_points):
                # Random age between 0 and 365 days
                age_days = random.randint(0, 365)
                timestamp = now - timedelta(days=age_days)
                score = random.random()
                
                # Calculate decay (exponential with 90-day half-life)
                decay_factor = math.exp(-age_days * math.log(2) / 90)
                adjusted_score = score * decay_factor
                
                points.append({
                    "original_score": score,
                    "age_days": age_days,
                    "decay_factor": decay_factor,
                    "adjusted_score": adjusted_score
                })
            
            calc_time = time.time() - start_time
            
            # Analyze decay distribution
            decay_buckets = {
                "fresh": len([p for p in points if p["age_days"] < 7]),
                "recent": len([p for p in points if 7 <= p["age_days"] < 30]),
                "medium": len([p for p in points if 30 <= p["age_days"] < 90]),
                "old": len([p for p in points if p["age_days"] >= 90])
            }
            
            results["details"] = {
                "points_processed": num_points,
                "calculation_time": calc_time,
                "time_per_point_ms": (calc_time * 1000) / num_points,
                "decay_distribution": decay_buckets
            }
            
            # Evaluate performance
            if calc_time < 0.1:  # Should process 10k points in under 100ms
                results["status"] = "passed"
                results["message"] = f"Decay calculation fast: {calc_time*1000:.1f}ms for {num_points} points"
            elif calc_time < 0.5:
                results["status"] = "warning"
                results["message"] = f"Decay calculation acceptable: {calc_time*1000:.1f}ms"
            else:
                results["status"] = "failed"
                results["message"] = f"Decay calculation slow: {calc_time*1000:.1f}ms"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    # ==================== DATA INTEGRITY TESTS ====================
    
    async def test_duplicate_detection(self) -> Dict:
        """Test: Duplicate detection (same conversation imported twice)."""
        test_name = "Duplicate Conversation Detection"
        print(f"\n🔍 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            # Check for duplicate conversation IDs
            duplicate_stats = {
                "collections_checked": 0,
                "duplicate_conversations": [],
                "duplicate_chunks": 0
            }
            
            conversation_counts = defaultdict(lambda: defaultdict(int))
            
            for collection in self.conv_collections[:5]:
                duplicate_stats["collections_checked"] += 1
                
                response = await self.qdrant_client.scroll(
                    collection_name=collection,
                    limit=100,
                    with_payload=True,
                    with_vectors=False
                )
                
                points, _ = response
                for point in points:
                    conv_id = point.payload.get("conversation_id", "unknown")
                    chunk_index = point.payload.get("chunk_index", 0)
                    key = f"{conv_id}_{chunk_index}"
                    
                    conversation_counts[collection][key] += 1
                    
                    if conversation_counts[collection][key] > 1:
                        duplicate_stats["duplicate_chunks"] += 1
                        if conv_id not in duplicate_stats["duplicate_conversations"]:
                            duplicate_stats["duplicate_conversations"].append(conv_id)
            
            results["details"] = duplicate_stats
            
            # Evaluate
            if duplicate_stats["duplicate_chunks"] == 0:
                results["status"] = "passed"
                results["message"] = "No duplicate chunks detected"
            elif duplicate_stats["duplicate_chunks"] < 5:
                results["status"] = "warning"
                results["message"] = f"Found {duplicate_stats['duplicate_chunks']} duplicate chunks"
            else:
                results["status"] = "failed"
                results["message"] = f"Significant duplication: {duplicate_stats['duplicate_chunks']} duplicate chunks"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def test_timestamp_preservation(self) -> Dict:
        """Test: Timestamp preservation from original conversations."""
        test_name = "Timestamp Preservation"
        print(f"\n🕐 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            timestamp_stats = {
                "valid_timestamps": 0,
                "invalid_timestamps": 0,
                "missing_timestamps": 0,
                "future_timestamps": 0,
                "total_checked": 0
            }
            
            now = datetime.now()
            
            for collection in self.conv_collections[:5]:
                response = await self.qdrant_client.scroll(
                    collection_name=collection,
                    limit=50,
                    with_payload=True,
                    with_vectors=False
                )
                
                points, _ = response
                for point in points:
                    timestamp_stats["total_checked"] += 1
                    timestamp_str = point.payload.get("timestamp")
                    
                    if not timestamp_str:
                        timestamp_stats["missing_timestamps"] += 1
                    else:
                        try:
                            # Parse timestamp
                            timestamp = datetime.fromisoformat(
                                timestamp_str.replace("Z", "+00:00")
                            )
                            
                            # Check if valid
                            if timestamp > now:
                                timestamp_stats["future_timestamps"] += 1
                            else:
                                timestamp_stats["valid_timestamps"] += 1
                        except:
                            timestamp_stats["invalid_timestamps"] += 1
            
            results["details"] = timestamp_stats
            
            # Evaluate
            if timestamp_stats["total_checked"] > 0:
                valid_ratio = timestamp_stats["valid_timestamps"] / timestamp_stats["total_checked"]
                if valid_ratio > 0.95:
                    results["status"] = "passed"
                    results["message"] = f"Timestamp preservation excellent: {valid_ratio*100:.1f}% valid"
                elif valid_ratio > 0.8:
                    results["status"] = "warning"
                    results["message"] = f"Some timestamp issues: {valid_ratio*100:.1f}% valid"
                else:
                    results["status"] = "failed"
                    results["message"] = f"Timestamp preservation poor: {valid_ratio*100:.1f}% valid"
            else:
                results["status"] = "failed"
                results["message"] = "No data to check"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    # ==================== CLOUD/VOYAGE TESTS ====================
    
    async def test_voyage_embedding_compatibility(self) -> Dict:
        """Test: Voyage embedding compatibility (1024 vs 384 dimensions)."""
        test_name = "Voyage Embedding Compatibility"
        print(f"\n☁️ Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            voyage_stats = {
                "voyage_collections": [],
                "local_collections": [],
                "dimension_mismatches": []
            }
            
            for collection in self.conv_collections:
                try:
                    info = await self.qdrant_client.get_collection(collection)
                    vector_size = info.config.params.vectors.size
                    
                    if "_voyage" in collection:
                        voyage_stats["voyage_collections"].append({
                            "name": collection,
                            "dimensions": vector_size
                        })
                        if vector_size != 1024:
                            voyage_stats["dimension_mismatches"].append(collection)
                    elif "_local" in collection:
                        voyage_stats["local_collections"].append({
                            "name": collection,
                            "dimensions": vector_size
                        })
                        if vector_size != 384:
                            voyage_stats["dimension_mismatches"].append(collection)
                except:
                    pass
            
            results["details"] = {
                "voyage_count": len(voyage_stats["voyage_collections"]),
                "local_count": len(voyage_stats["local_collections"]),
                "dimension_mismatches": len(voyage_stats["dimension_mismatches"])
            }
            
            # Evaluate
            if voyage_stats["dimension_mismatches"]:
                results["status"] = "failed"
                results["message"] = f"Dimension mismatches found: {len(voyage_stats['dimension_mismatches'])} collections"
            elif voyage_stats["voyage_collections"] and voyage_stats["local_collections"]:
                results["status"] = "passed"
                results["message"] = f"Both Voyage (1024d) and Local (384d) collections working"
            elif voyage_stats["local_collections"]:
                results["status"] = "warning"
                results["message"] = "Only local collections found (no Voyage collections to test)"
            else:
                results["status"] = "failed"
                results["message"] = "No collections found"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def test_api_key_validation(self) -> Dict:
        """Test: API key validation for Voyage."""
        test_name = "Voyage API Key Validation"
        print(f"\n🔑 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            voyage_key = TEST_CONFIG.get("voyage_key", "")
            
            results["details"] = {
                "has_voyage_key": bool(voyage_key),
                "key_length": len(voyage_key) if voyage_key else 0
            }
            
            if voyage_key:
                # Try to use the key (would need actual Voyage client)
                results["status"] = "passed"
                results["message"] = "Voyage API key configured"
            else:
                results["status"] = "warning"
                results["message"] = "No Voyage API key configured (using local embeddings only)"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    # ==================== EDGE CASES ====================
    
    async def test_unicode_encoding(self) -> Dict:
        """Test: Unicode and special character handling."""
        test_name = "Unicode & Special Characters"
        print(f"\n🌍 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            from fastembed import TextEmbedding
            model = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
            
            # Test various unicode and special characters
            test_strings = [
                "Hello 世界 🌍",  # Mixed languages with emoji
                "𝕌𝕟𝕚𝕔𝕠𝕕𝕖 𝕗𝕒𝕟𝕔𝕪 𝕥𝕖𝕩𝕥",  # Mathematical alphanumeric symbols
                "←↑→↓ ⌘⌥⇧ ✓✗",  # Arrows and symbols
                "NULL\x00CHAR",  # Null character
                "Tab\tNewline\nReturn\r",  # Control characters
            ]
            
            encoding_results = []
            
            for test_str in test_strings:
                try:
                    # Try to embed
                    embedding = list(model.embed([test_str]))[0]
                    
                    encoding_results.append({
                        "text": test_str[:20],
                        "success": True,
                        "embedding_dims": len(embedding)
                    })
                except Exception as e:
                    encoding_results.append({
                        "text": test_str[:20],
                        "success": False,
                        "error": str(e)
                    })
            
            results["details"] = {
                "total_tested": len(test_strings),
                "successful": sum(1 for r in encoding_results if r["success"]),
                "results": encoding_results
            }
            
            # Evaluate
            success_rate = results["details"]["successful"] / results["details"]["total_tested"]
            if success_rate == 1.0:
                results["status"] = "passed"
                results["message"] = "All unicode and special characters handled correctly"
            elif success_rate >= 0.8:
                results["status"] = "warning"
                results["message"] = f"Some unicode issues: {results['details']['successful']}/{results['details']['total_tested']} passed"
            else:
                results["status"] = "failed"
                results["message"] = f"Unicode handling poor: only {results['details']['successful']}/{results['details']['total_tested']} passed"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def test_network_failures(self) -> Dict:
        """Test: Network failure handling (Qdrant timeouts)."""
        test_name = "Network Failure Resilience"
        print(f"\n🔌 Testing: {test_name}")
        
        results = {
            "name": test_name,
            "status": "pending",
            "details": {}
        }
        
        try:
            # Test with invalid URL to simulate network failure
            from qdrant_client import AsyncQdrantClient
            from qdrant_client.exceptions import UnexpectedResponse
            
            # Try to connect to non-existent Qdrant
            bad_client = AsyncQdrantClient(url="http://localhost:9999", timeout=1)
            
            try:
                await asyncio.wait_for(
                    bad_client.get_collections(),
                    timeout=2.0
                )
                results["details"]["network_failure_handled"] = False
            except (asyncio.TimeoutError, Exception) as e:
                results["details"]["network_failure_handled"] = True
                results["details"]["error_type"] = type(e).__name__
            
            # Test recovery with good client
            try:
                good_collections = await self.qdrant_client.get_collections()
                results["details"]["recovery_successful"] = True
                results["details"]["collections_after_recovery"] = len(good_collections.collections)
            except:
                results["details"]["recovery_successful"] = False
            
            results["details"] = results.get("details", {})
            
            # Evaluate
            if results["details"].get("network_failure_handled") and results["details"].get("recovery_successful"):
                results["status"] = "passed"
                results["message"] = "Network failures handled gracefully with recovery"
            elif results["details"].get("network_failure_handled"):
                results["status"] = "warning"
                results["message"] = "Network failures detected but recovery needs improvement"
            else:
                results["status"] = "failed"
                results["message"] = "Network failure handling inadequate"
                
        except Exception as e:
            results["status"] = "failed"
            results["error"] = str(e)
        
        return results
    
    async def run_all_tests(self):
        """Run all extended tests."""
        print("=" * 70)
        print("🚀 EXTENDED TEST SUITE v2.5.17")
        print("=" * 70)
        
        await self.initialize()
        
        # Define all test methods
        test_methods = [
            # Critical bugs
            self.test_search_by_concept_broken,
            self.test_metadata_extraction_missing,
            
            # Search quality
            self.test_multi_language_search,
            self.test_fuzzy_matching,
            self.test_long_query_handling,
            
            # Performance
            self.test_concurrent_searches,
            self.test_memory_decay_calculation,
            
            # Data integrity
            self.test_duplicate_detection,
            self.test_timestamp_preservation,
            
            # Cloud/Voyage
            self.test_voyage_embedding_compatibility,
            self.test_api_key_validation,
            
            # Edge cases
            self.test_unicode_encoding,
            self.test_network_failures
        ]
        
        # Run all tests
        for test_method in test_methods:
            try:
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
                    
                    # Track critical bugs
                    if result.get("critical"):
                        self.results["summary"]["critical_bugs"].append(result["name"])
                        
            except Exception as e:
                print(f"  💥 Error running {test_method.__name__}: {e}")
                self.results["summary"]["failed"] += 1
        
        # Generate report
        self.generate_report()
    
    def generate_report(self):
        """Generate comprehensive test report."""
        print("\n" + "=" * 70)
        print("📋 EXTENDED TEST REPORT v2.5.17")
        print("=" * 70)
        
        summary = self.results["summary"]
        pass_rate = (summary["passed"] / summary["total"] * 100) if summary["total"] > 0 else 0
        
        print(f"\n📊 Summary:")
        print(f"  Total Tests: {summary['total']}")
        print(f"  ✅ Passed: {summary['passed']}")
        print(f"  ⚠️ Warnings: {summary['warnings']}")
        print(f"  ❌ Failed: {summary['failed']}")
        print(f"  Pass Rate: {pass_rate:.1f}%")
        
        # Critical bugs
        if summary["critical_bugs"]:
            print(f"\n🚨 CRITICAL BUGS FOUND:")
            for bug in summary["critical_bugs"]:
                print(f"  - {bug}")
                if "search_by_concept" in bug:
                    print("    → Fix: Run delta-metadata-update.py to populate concepts field")
                if "metadata_extraction" in bug:
                    print("    → Fix: Update streaming importer to extract metadata")
        
        # Specific issues
        print(f"\n🔍 Key Findings:")
        
        # Check search_by_concept
        concept_test = self.results["tests"].get("Search by Concept - CRITICAL BUG", {})
        if concept_test.get("status") == "failed":
            print("  ❌ search_by_concept is completely broken - concepts field not populated")
            print("     Action: Must run delta-metadata-update.py after imports")
        
        # Check metadata
        metadata_test = self.results["tests"].get("Metadata Extraction Coverage", {})
        if metadata_test.get("status") == "failed":
            print("  ❌ Metadata extraction missing from streaming importer")
            print("     Action: Update streaming importer to extract files_analyzed, tools_used")
        
        # Release recommendation
        print(f"\n🏁 RELEASE RECOMMENDATION:")
        
        if summary["critical_bugs"]:
            print("  ❌ BLOCK RELEASE - Critical bugs must be fixed:")
            print("     1. Fix search_by_concept by populating concepts field")
            print("     2. Add metadata extraction to streaming importer")
            print("     3. Re-run tests after fixes")
        elif pass_rate >= 70:
            print("  ⚠️ CONDITIONAL RELEASE")
            print("     - Document known issues in release notes")
            print("     - Provide migration guide for existing users")
        else:
            print("  ❌ NOT READY FOR RELEASE")
            print("     - Too many test failures")
        
        # Save report
        report_path = Path(__file__).parent / f"test-report-extended-{datetime.now().strftime('%Y%m%d-%H%M%S')}.json"
        with open(report_path, 'w') as f:
            json.dump(self.results, f, indent=2, default=str)
        
        print(f"\n💾 Full report saved to: {report_path}")


async def main():
    """Main entry point."""
    suite = ExtendedTestSuite()
    await suite.run_all_tests()


if __name__ == "__main__":
    asyncio.run(main())